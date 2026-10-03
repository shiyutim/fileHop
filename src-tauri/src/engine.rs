use crate::{
    archive::{compress_directory, PreparedArchive},
    protocol::{self, Control, OfferedFile, Result, Secure, CANCELLED, CHUNK, REJECTED},
};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    fs::{self, File},
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch, Semaphore},
    time::timeout,
};
use uuid::Uuid;

const SERVICE: &str = "_filehop._tcp.local.";
const MAX_FILES: usize = 100;
const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_TOTAL: u64 = 10 * 1024 * 1024 * 1024 * 1024; // 10 TiB; safely below JS integer limit.
const MAX_ACTIVE: usize = 8;
const HISTORY_LIMIT: usize = 200;
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn active(status: &str) -> bool {
    matches!(
        status,
        "preparing" | "connecting" | "waiting" | "transferring"
    )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileItem {
    pub name: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub is_directory: bool,
}
enum SendSource {
    Path(String),
    Directory(String),
    Archive(PreparedArchive),
    Bytes(Vec<u8>),
}
struct SendFile {
    item: FileItem,
    source: SendSource,
}
impl SendFile {
    async fn reader(&self) -> Result<Box<dyn AsyncRead + Unpin + Send + '_>> {
        let path = match &self.source {
            SendSource::Path(path) => Path::new(path),
            SendSource::Archive(archive) => archive.path(),
            SendSource::Bytes(bytes) => return Ok(Box::new(bytes.as_slice())),
            SendSource::Directory(_) => return Err("文件夹尚未完成压缩".into()),
        };
        let file = File::open(path)
            .await
            .map_err(|e| format!("无法打开文件：{e}"))?;
        if file.metadata().await.map_err(|e| e.to_string())?.len() != self.item.size {
            return Err("文件大小在发送前发生了变化，请重新选择".into());
        }
        Ok(Box::new(file))
    }
    async fn prepare(&mut self, cancelled: &watch::Receiver<bool>) -> Result<()> {
        if *cancelled.borrow() {
            return Err(CANCELLED.into());
        }
        if let SendSource::Directory(path) = &self.source {
            let path = PathBuf::from(path);
            let cancellation = cancelled.clone();
            // Await the worker even after cancellation so partial archives are
            // removed before the transfer becomes terminal.
            let archive =
                tokio::task::spawn_blocking(move || compress_directory(&path, &cancellation))
                    .await
                    .map_err(|error| format!("压缩任务失败：{error}"))??;
            self.item.name = format!("{}.zip", self.item.name);
            self.item.size = archive.size();
            self.item.path = None;
            self.item.is_directory = false;
            self.source = SendSource::Archive(archive);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub addresses: Vec<String>,
    pub port: u16,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub address: String,
    pub port: u16,
    pub last_seen: u64,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transfer {
    pub id: String,
    pub direction: String,
    pub peer_name: String,
    pub files: Vec<FileItem>,
    pub total_bytes: u64,
    pub transferred_bytes: u64,
    pub status: String,
    pub verification_code: Option<String>,
    pub local_confirmed: bool,
    pub peer_trusted: bool,
    pub error: Option<String>,
    pub created_at: u64,
    pub completed_at: Option<u64>,
    pub save_dir: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustedDevice {
    pub public_key: String,
    pub name: String,
    pub trusted_at: u64,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub device: Device,
    pub peers: Vec<Peer>,
    pub transfers: Vec<Transfer>,
    pub save_dir: String,
    pub trusted_devices: Vec<TrustedDevice>,
    pub discovery_error: Option<String>,
    pub server_error: Option<String>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    name: Option<String>,
    save_dir: Option<String>,
    private_key: Option<Vec<u8>>,
    #[serde(default)]
    trusted_devices: Vec<TrustedDevice>,
}
struct State {
    snapshot: Snapshot,
    approvals: HashMap<String, oneshot::Sender<bool>>,
    cancellations: HashMap<String, watch::Sender<bool>>,
    selected: HashMap<String, FileItem>,
    // Keys come exclusively from the authenticated Noise handshake, never discovery.
    transfer_peer_keys: HashMap<String, String>,
}
pub struct Engine {
    state: Mutex<State>,
    daemon: Mutex<Option<ServiceDaemon>>,
    settings_path: Option<PathBuf>,
    identity: Result<Vec<u8>>,
}

impl Engine {
    pub fn new(save_dir: PathBuf, settings_path: Option<PathBuf>) -> Arc<Self> {
        let loaded = read_settings(settings_path.as_deref());
        let load_error = loaded.as_ref().err().cloned();
        let settings = loaded.unwrap_or_default();
        let identity = match (load_error, settings.private_key) {
            (Some(error), _) => Err(error),
            (None, Some(key)) if key.len() == 32 => Ok(key),
            (None, Some(_)) => Err("保存的设备身份无效，无法启动安全连接".into()),
            (None, None) if !settings.trusted_devices.is_empty() => {
                Err("设备身份缺失，无法使用已保存的信任记录".into())
            }
            (None, None) => protocol::generate_identity(),
        };
        let name = settings
            .name
            .filter(|n| valid_device_name(n))
            .unwrap_or_else(|| "我的电脑".into());
        Arc::new(Self {
            state: Mutex::new(State {
                snapshot: Snapshot {
                    device: Device {
                        id: Uuid::new_v4().to_string(),
                        name,
                        platform: std::env::consts::OS.into(),
                        addresses: local_addresses(),
                        port: 0,
                    },
                    peers: vec![],
                    transfers: vec![],
                    save_dir: settings
                        .save_dir
                        .unwrap_or_else(|| save_dir.to_string_lossy().into_owned()),
                    trusted_devices: settings.trusted_devices,
                    discovery_error: None,
                    server_error: Some("正在启动局域网服务…".into()),
                },
                approvals: HashMap::new(),
                cancellations: HashMap::new(),
                selected: HashMap::new(),
                transfer_peer_keys: HashMap::new(),
            }),
            daemon: Mutex::new(None),
            settings_path,
            identity,
        })
    }
    pub async fn start(self: &Arc<Self>, preferred_port: u16, discover: bool) -> Result<()> {
        // Save the identity before accepting any connections so a restart cannot
        // silently turn a trusted device into a different one.
        let initialized = {
            let state = self.state.lock().unwrap();
            self.persist_locked(&state.snapshot)
        };
        initialized.map_err(|error| self.server_failure(error))?;
        let listener = match TcpListener::bind((Ipv4Addr::UNSPECIFIED, preferred_port)).await {
            Ok(listener) => listener,
            Err(_) if preferred_port != 0 => TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))
                .await
                .map_err(|e| self.server_failure(e.to_string()))?,
            Err(e) => return Err(self.server_failure(e.to_string())),
        };
        {
            let mut state = self.state.lock().unwrap();
            state.snapshot.device.port = listener.local_addr().map_err(|e| e.to_string())?.port();
            state.snapshot.server_error = None;
        }
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let slots = Arc::new(Semaphore::new(MAX_ACTIVE));
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(value) => value,
                    Err(error) => {
                        if let Some(engine) = weak.upgrade() {
                            engine.server_failure(error.to_string());
                        }
                        break;
                    }
                };
                let Some(engine) = weak.upgrade() else {
                    break;
                };
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    continue;
                };
                tokio::spawn(async move {
                    let _permit = permit;
                    engine.incoming(stream).await;
                });
            }
        });
        if discover {
            if let Err(error) = self.start_discovery() {
                self.state.lock().unwrap().snapshot.discovery_error = Some(error);
            }
        }
        Ok(())
    }
    fn server_failure(&self, detail: String) -> String {
        let message = format!("无法启动接收服务：{detail}");
        self.state.lock().unwrap().snapshot.server_error = Some(message.clone());
        message
    }
    pub fn snapshot(&self) -> Snapshot {
        self.state.lock().unwrap().snapshot.clone()
    }
    fn update(&self, id: &str, f: impl FnOnce(&mut Transfer)) {
        if let Some(t) = self
            .state
            .lock()
            .unwrap()
            .snapshot
            .transfers
            .iter_mut()
            .find(|t| t.id == id)
        {
            f(t);
        }
    }
    fn insert(
        &self,
        direction: &str,
        peer_name: String,
        files: Vec<FileItem>,
    ) -> Result<(String, oneshot::Receiver<bool>, watch::Receiver<bool>)> {
        let mut state = self.state.lock().unwrap();
        if state
            .snapshot
            .transfers
            .iter()
            .filter(|t| active(&t.status))
            .count()
            >= MAX_ACTIVE
        {
            return Err("同时最多进行 8 个传输，请稍后再试".into());
        }
        if state.snapshot.transfers.len() >= HISTORY_LIMIT {
            if let Some(index) = state
                .snapshot
                .transfers
                .iter()
                .position(|t| !active(&t.status))
            {
                state.snapshot.transfers.remove(index);
            }
        }
        let total_bytes = validate_files(
            &files
                .iter()
                .map(|f| OfferedFile {
                    name: f.name.clone(),
                    size: f.size,
                })
                .collect::<Vec<_>>(),
        )?;
        let id = Uuid::new_v4().to_string();
        let (approval, response) = oneshot::channel();
        let (cancellation, cancelled) = watch::channel(false);
        state.approvals.insert(id.clone(), approval);
        state.cancellations.insert(id.clone(), cancellation);
        let status = if files.iter().any(|file| file.is_directory) {
            "preparing"
        } else {
            "connecting"
        };
        state.snapshot.transfers.push(Transfer {
            id: id.clone(),
            direction: direction.into(),
            peer_name,
            files,
            total_bytes,
            transferred_bytes: 0,
            status: status.into(),
            verification_code: None,
            local_confirmed: false,
            peer_trusted: false,
            error: None,
            created_at: now(),
            completed_at: None,
            save_dir: None,
        });
        Ok((id, response, cancelled))
    }
    fn finish(&self, id: &str, result: Result<()>) {
        self.update(id, |t| {
            let (status, error) = match result {
                Ok(()) => ("completed", None),
                Err(e) if e == CANCELLED => ("cancelled", None),
                Err(e) if e == REJECTED => ("rejected", None),
                Err(e) => ("failed", Some(e)),
            };
            t.status = status.into();
            t.error = error;
            t.completed_at = Some(now());
        });
        let mut state = self.state.lock().unwrap();
        state.approvals.remove(id);
        state.cancellations.remove(id);
        state.transfer_peer_keys.remove(id);
    }
    pub fn respond(&self, id: &str, accept: bool, trust: bool) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let t = state
            .snapshot
            .transfers
            .iter()
            .find(|t| t.id == id)
            .ok_or("找不到这个传输")?;
        if t.status != "waiting" || t.local_confirmed {
            return Err("这个传输已不在等待确认".into());
        }
        if state
            .approvals
            .get(id)
            .is_none_or(oneshot::Sender::is_closed)
        {
            return Err("连接已经结束".into());
        }
        if accept && trust {
            let key = state
                .transfer_peer_keys
                .get(id)
                .ok_or("尚未验证对方的设备身份")?
                .clone();
            let mut next = state.snapshot.clone();
            if !next
                .trusted_devices
                .iter()
                .any(|device| device.public_key == key)
            {
                next.trusted_devices.push(TrustedDevice {
                    public_key: key,
                    name: t.peer_name.clone(),
                    trusted_at: now(),
                });
            }
            // Persist before approving; a failed save leaves the request pending
            // and does not accidentally enable trust for this process only.
            self.persist_locked(&next)?;
            state.snapshot = next;
        }
        if accept {
            let t = state
                .snapshot
                .transfers
                .iter_mut()
                .find(|t| t.id == id)
                .unwrap();
            t.local_confirmed = true;
        }
        state
            .approvals
            .remove(id)
            .ok_or("已经确认过这个传输")?
            .send(accept)
            .map_err(|_| "连接已经结束".into())
    }
    pub fn revoke_trusted_device(&self, public_key: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let mut next = state.snapshot.clone();
        next.trusted_devices
            .retain(|device| device.public_key != public_key);
        self.persist_locked(&next)?;
        state.snapshot = next;
        Ok(())
    }
    pub fn cancel(&self, id: &str) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .cancellations
            .get(id)
            .ok_or("这个传输已经结束")?
            .send(true)
            .map_err(|_| "这个传输已经结束".into())
    }
    pub fn clear_history(&self) {
        self.state
            .lock()
            .unwrap()
            .snapshot
            .transfers
            .retain(|t| active(&t.status));
    }
    pub fn set_name(&self, name: String) -> Result<()> {
        let name = name.trim().to_string();
        if !valid_device_name(&name) {
            return Err("设备名称应为 1–40 个字符，不能包含控制字符".into());
        }
        {
            let mut state = self.state.lock().unwrap();
            let mut next = state.snapshot.clone();
            next.device.name = name;
            self.persist_locked(&next)?;
            state.snapshot = next;
        }
        if let Some(daemon) = self.daemon.lock().unwrap().as_ref() {
            let device = self.snapshot().device;
            daemon
                .register(service_info(&device)?)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub async fn set_save_dir(&self, path: PathBuf) -> Result<String> {
        fs::create_dir_all(&path)
            .await
            .map_err(|e| format!("无法使用此文件夹：{e}"))?;
        let path = fs::canonicalize(path)
            .await
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned();
        let mut state = self.state.lock().unwrap();
        let mut next = state.snapshot.clone();
        next.save_dir = path.clone();
        self.persist_locked(&next)?;
        state.snapshot = next;
        Ok(path)
    }
    fn private_key(&self) -> Result<&[u8]> {
        self.identity.as_deref().map_err(Clone::clone)
    }
    // The caller holds state for the entire update/write, serializing concurrent
    // trust, revocation and preference changes and keeping disk/memory in sync.
    fn persist_locked(&self, snapshot: &Snapshot) -> Result<()> {
        let private_key = self.private_key()?;
        if let Some(path) = &self.settings_path {
            let bytes = serde_json::to_vec(&Settings {
                name: Some(snapshot.device.name.clone()),
                save_dir: Some(snapshot.save_dir.clone()),
                private_key: Some(private_key.to_vec()),
                trusted_devices: snapshot.trusted_devices.clone(),
            })
            .map_err(|e| e.to_string())?;
            write_settings(path, &bytes)?;
        }
        Ok(())
    }
    pub async fn inspect(&self, paths: Vec<String>) -> Result<Vec<FileItem>> {
        if paths.is_empty() || paths.len() > MAX_FILES {
            return Err("一次请选择 1–100 个文件或文件夹".into());
        }
        let mut files = Vec::new();
        for path in paths {
            let path = fs::canonicalize(path)
                .await
                .map_err(|e| format!("文件无法访问：{e}"))?;
            let meta = fs::metadata(&path).await.map_err(|e| e.to_string())?;
            if !meta.is_file() && !meta.is_dir() {
                return Err("只支持普通文件或文件夹".into());
            }
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or("文件名不是有效的 Unicode")?
                .to_string();
            validate_name(&name)?;
            if meta.is_dir() {
                validate_name(&format!("{name}.zip"))?;
            }
            let canonical = path
                .to_str()
                .ok_or("文件路径不是有效的 Unicode")?
                .to_string();
            if files
                .iter()
                .any(|f: &FileItem| f.path.as_ref() == Some(&canonical))
            {
                continue;
            }
            files.push(FileItem {
                name,
                size: if meta.is_dir() { 0 } else { meta.len() },
                path: Some(canonical),
                is_directory: meta.is_dir(),
            });
        }
        validate_files(
            &files
                .iter()
                .map(|f| OfferedFile {
                    name: f.name.clone(),
                    size: f.size,
                })
                .collect::<Vec<_>>(),
        )?;
        let mut state = self.state.lock().unwrap();
        if state.selected.len() > 1000 {
            state.selected.clear();
        }
        for file in &files {
            state
                .selected
                .insert(file.path.clone().unwrap(), file.clone());
        }
        Ok(files)
    }
    pub async fn send_files(
        self: &Arc<Self>,
        address: String,
        paths: Vec<String>,
        peer_name: Option<String>,
    ) -> Result<String> {
        if paths.is_empty() || paths.len() > MAX_FILES {
            return Err("一次请选择 1–100 个文件或文件夹".into());
        }
        {
            let state = self.state.lock().unwrap();
            if paths.iter().any(|p| !state.selected.contains_key(p)) {
                return Err("请通过选择或拖放重新添加文件或文件夹".into());
            }
        }
        let address = normalize_address(&address)?;
        let files = self
            .inspect(paths)
            .await?
            .into_iter()
            .map(|item| SendFile {
                source: if item.is_directory {
                    SendSource::Directory(item.path.clone().unwrap())
                } else {
                    SendSource::Path(item.path.clone().unwrap())
                },
                item,
            })
            .collect();
        self.start_send(address, files, peer_name)
    }
    pub async fn send_text(
        self: &Arc<Self>,
        address: String,
        text: String,
        name: Option<String>,
        peer_name: Option<String>,
    ) -> Result<String> {
        if text.trim().is_empty() {
            return Err("请输入要发送的文案".into());
        }
        if text.len() > MAX_TEXT_BYTES {
            return Err("文案不能超过 1 MB，请改为选择文件发送".into());
        }
        let name = text_file_name(name.as_deref())?;
        let address = normalize_address(&address)?;
        let file = SendFile {
            item: FileItem {
                name,
                size: text.len() as u64,
                path: None,
                is_directory: false,
            },
            source: SendSource::Bytes(text.into_bytes()),
        };
        self.start_send(address, vec![file], peer_name)
    }
    fn start_send(
        self: &Arc<Self>,
        address: String,
        mut files: Vec<SendFile>,
        peer_name: Option<String>,
    ) -> Result<String> {
        // start() must successfully persist the identity before either direction
        // can expose it to a peer that may choose to trust it.
        if let Some(error) = &self.state.lock().unwrap().snapshot.server_error {
            return Err(error.clone());
        }
        let (id, response, mut cancelled) = self.insert(
            "send",
            peer_name
                .filter(|s| valid_device_name(s))
                .unwrap_or_else(|| address.clone()),
            files.iter().map(|file| file.item.clone()).collect(),
        )?;
        let engine = self.clone();
        let task_id = id.clone();
        tokio::spawn(async move {
            let result = async {
                for file in &mut files {
                    file.prepare(&cancelled).await?;
                }
                if *cancelled.borrow() {
                    return Err(CANCELLED.into());
                }
                let total_bytes = validate_files(
                    &files
                        .iter()
                        .map(|file| OfferedFile {
                            name: file.item.name.clone(),
                            size: file.item.size,
                        })
                        .collect::<Vec<_>>(),
                )?;
                engine.update(&task_id, |transfer| {
                    transfer.files = files.iter().map(|file| file.item.clone()).collect();
                    transfer.total_bytes = total_bytes;
                    transfer.status = "connecting".into();
                });
                let connect = async {
                    let stream = timeout(Duration::from_secs(10), TcpStream::connect(&address))
                        .await
                        .map_err(|_| "连接超时，请检查地址、Wi-Fi 和防火墙".to_string())?
                        .map_err(|e| format!("连接失败：{e}"))?;
                    Secure::handshake(stream, true, engine.private_key()?).await
                };
                let secure = tokio::select! {
                    biased;
                    _ = cancelled.changed() => Err(CANCELLED.into()),
                    result = connect => result,
                };
                match secure {
                    Ok(mut secure) => {
                        let result = tokio::select! {
                            biased;
                            _ = cancelled.changed() => Err(CANCELLED.into()),
                            result = engine.send_session(&task_id, &files, response, &mut secure) => result,
                        };
                        report_error(&mut secure, &result).await;
                        result
                    }
                    Err(e) => Err(e),
                }
            }
            .await;
            // File readers have closed by here, including on cancel or failure.
            // Drop the owned temporary ZIPs before reporting the final status.
            drop(files);
            engine.finish(&task_id, result);
        });
        Ok(id)
    }
    async fn incoming(self: Arc<Self>, stream: TcpStream) {
        let Ok(private_key) = self.private_key() else {
            return;
        };
        let Ok(mut secure) = Secure::handshake(stream, false, private_key).await else {
            return;
        };
        let offer = secure.receive_control(15).await;
        let (name, files) = match offer {
            Ok(Control::Offer {
                version: protocol::VERSION,
                name,
                files,
            }) if valid_device_name(&name) => (name, files),
            _ => {
                let _ = secure
                    .control(&Control::Error {
                        message: "不支持的协议或设备名称".into(),
                    })
                    .await;
                return;
            }
        };
        if let Err(error) = validate_files(&files) {
            let _ = secure.control(&Control::Error { message: error }).await;
            return;
        }
        let items = files
            .iter()
            .map(|f| FileItem {
                name: f.name.clone(),
                size: f.size,
                path: None,
                is_directory: false,
            })
            .collect();
        let (id, approval, mut cancelled) = match self.insert("receive", name, items) {
            Ok(value) => value,
            Err(e) => {
                let _ = secure.control(&Control::Error { message: e }).await;
                return;
            }
        };
        let result = tokio::select! { result = self.receive_session(&id, &files, approval, &mut secure) => result, _ = cancelled.changed() => Err(CANCELLED.into()) };
        report_error(&mut secure, &result).await;
        self.finish(&id, result);
    }
    async fn approve(
        &self,
        id: &str,
        mut approval: oneshot::Receiver<bool>,
        secure: &mut Secure,
    ) -> Result<()> {
        let trusted = {
            let mut state = self.state.lock().unwrap();
            let trusted = state
                .snapshot
                .trusted_devices
                .iter()
                .any(|device| device.public_key == secure.peer_key);
            let save_dir = state.snapshot.save_dir.clone();
            let transfer = state
                .snapshot
                .transfers
                .iter_mut()
                .find(|transfer| transfer.id == id)
                .ok_or("找不到这个传输")?;
            transfer.status = "waiting".into();
            transfer.verification_code = Some(secure.code.clone());
            transfer.peer_trusted = trusted;
            transfer.local_confirmed = trusted;
            if transfer.direction == "receive" {
                transfer.save_dir = Some(save_dir);
            }
            state
                .transfer_peer_keys
                .insert(id.into(), secure.peer_key.clone());
            if trusted {
                state.approvals.remove(id);
            }
            trusted
        };
        timeout(Duration::from_secs(300), async {
            let mut local_ready = trusted; let mut peer_ready = false;
            if trusted {
                secure.control(&Control::Ready).await?;
            }
            while !local_ready || !peer_ready {
                tokio::select! {
                    answer = &mut approval, if !local_ready => {
                        if !answer.unwrap_or(false) { return Err::<(), String>(REJECTED.into()); }
                        secure.control(&Control::Ready).await?; local_ready = true;
                    }
                    answer = secure.receive_control(300) => {
                        if peer_ready || !matches!(answer?, Control::Ready) { return Err("等待确认时收到了意外消息".into()); }
                        peer_ready = true;
                    }
                }
            } Ok(())
        }).await.map_err(|_| "确认已超时，请重新发送".to_string())??;
        self.update(id, |t| t.status = "transferring".into());
        Ok(())
    }
    async fn send_session(
        &self,
        id: &str,
        files: &[SendFile],
        approval: oneshot::Receiver<bool>,
        secure: &mut Secure,
    ) -> Result<()> {
        secure
            .control(&Control::Offer {
                version: protocol::VERSION,
                name: self.snapshot().device.name,
                files: files
                    .iter()
                    .map(|f| OfferedFile {
                        name: f.item.name.clone(),
                        size: f.item.size,
                    })
                    .collect(),
            })
            .await?;
        match secure.receive_control(15).await? {
            Control::Hello { name } if valid_device_name(&name) => {
                self.update(id, |t| t.peer_name = name)
            }
            _ => return Err("对方未返回有效设备信息".into()),
        }
        self.approve(id, approval, secure).await?;
        let mut transferred = 0;
        let mut buffer = vec![0; CHUNK];
        for source in files {
            let item = &source.item;
            let mut file = source.reader().await?;
            let mut hash = Sha256::new();
            let mut size = 0u64;
            loop {
                let count = file
                    .read(&mut buffer)
                    .await
                    .map_err(|e| format!("读取文件失败：{e}"))?;
                if count == 0 {
                    break;
                }
                size = size.checked_add(count as u64).ok_or("文件大小溢出")?;
                if size > item.size {
                    return Err("文件在发送期间发生了变化".into());
                }
                hash.update(&buffer[..count]);
                secure.data(&buffer[..count]).await?;
                transferred += count as u64;
                self.update(id, |t| t.transferred_bytes = transferred);
            }
            if size != item.size {
                return Err("文件在发送期间发生了变化".into());
            }
            secure
                .control(&Control::FileEnd {
                    sha256: format!("{:x}", hash.finalize()),
                })
                .await?;
        }
        // Receiver only completes after every checksum is verified and files are saved.
        if !matches!(secure.receive_control(300).await?, Control::Complete) {
            return Err("对方未确认文件保存成功".into());
        }
        secure.control(&Control::Ack).await?;
        Ok(())
    }
    async fn receive_session(
        &self,
        id: &str,
        files: &[OfferedFile],
        approval: oneshot::Receiver<bool>,
        secure: &mut Secure,
    ) -> Result<()> {
        secure
            .control(&Control::Hello {
                name: self.snapshot().device.name,
            })
            .await?;
        self.approve(id, approval, secure).await?;
        let save_dir = PathBuf::from(self.snapshot().save_dir);
        fs::create_dir_all(&save_dir)
            .await
            .map_err(|e| format!("无法创建接收文件夹：{e}"))?;
        self.update(id, |t| {
            t.save_dir = Some(save_dir.to_string_lossy().into_owned())
        });
        let mut temporary = TemporaryFiles::default();
        let mut verified = Vec::new();
        let mut total = 0u64;
        for offered in files {
            let path = save_dir.join(format!(".filehop-{}.part", Uuid::new_v4()));
            // Creation and cleanup ownership are synchronous: cancellation of an
            // async filesystem open must not leave a late-created orphan file.
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| format!("无法创建接收文件：{e}"))?;
            temporary.paths.push(path.clone());
            let mut file = File::from_std(file);
            let mut size = 0u64;
            let mut hash = Sha256::new();
            loop {
                let packet = secure.packet(45).await?;
                if packet[0] == 1 {
                    let data = &packet[1..];
                    if data.is_empty() {
                        return Err("收到了空的文件分块".into());
                    }
                    size = size.checked_add(data.len() as u64).ok_or("文件大小溢出")?;
                    if size > offered.size {
                        return Err("接收到的文件超出了声明大小".into());
                    }
                    file.write_all(data)
                        .await
                        .map_err(|e| format!("写入文件失败，请检查磁盘空间：{e}"))?;
                    hash.update(data);
                    total += data.len() as u64;
                    self.update(id, |t| t.transferred_bytes = total);
                } else {
                    match protocol::decode(&packet)? {
                        Control::FileEnd { sha256 } => {
                            if size != offered.size || sha256 != format!("{:x}", hash.finalize()) {
                                return Err("文件完整性校验失败，请重新发送".into());
                            }
                            file.flush().await.map_err(|e| e.to_string())?;
                            file.sync_all().await.map_err(|e| e.to_string())?;
                            break;
                        }
                        _ => return Err("收到了顺序错误的文件消息".into()),
                    }
                }
            }
            drop(file);
            verified.push((path, offered.name.clone()));
        }
        for (temp, name) in &verified {
            publish_file(temp, &save_dir, name).await?;
        }
        secure.control(&Control::Complete).await?;
        // Files are committed even if the final acknowledgement is lost. Surface
        // that ambiguity as an error instead of claiming two-sided completion.
        if !matches!(secure.receive_control(45).await?, Control::Ack) {
            return Err("文件已保存，但未收到发送方的最终确认".into());
        }
        Ok(())
    }
    fn start_discovery(self: &Arc<Self>) -> Result<()> {
        let daemon =
            ServiceDaemon::new().map_err(|e| format!("设备发现不可用，可手动输入地址：{e}"))?;
        daemon
            .register(service_info(&self.snapshot().device)?)
            .map_err(|e| e.to_string())?;
        let receiver = daemon.browse(SERVICE).map_err(|e| e.to_string())?;
        *self.daemon.lock().unwrap() = Some(daemon);
        let weak = Arc::downgrade(self);
        std::thread::spawn(move || {
            while let Ok(event) = receiver.recv() {
                let Some(engine) = weak.upgrade() else {
                    break;
                };
                let mut state = engine.state.lock().unwrap();
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        let Some(id) = info.get_property_val_str("id") else {
                            continue;
                        };
                        if id == state.snapshot.device.id || Uuid::parse_str(id).is_err() {
                            continue;
                        }
                        let name = info.get_property_val_str("name").unwrap_or("另一台电脑");
                        if !valid_device_name(name) {
                            continue;
                        }
                        let Some(ip) = info
                            .get_addresses()
                            .iter()
                            .find(|ip| ip.is_ipv4() && !ip.is_loopback())
                        else {
                            continue;
                        };
                        let peer = Peer {
                            id: id.into(),
                            name: name.into(),
                            platform: info
                                .get_property_val_str("platform")
                                .unwrap_or("unknown")
                                .chars()
                                .take(30)
                                .collect(),
                            address: ip.to_string(),
                            port: info.get_port(),
                            last_seen: now(),
                        };
                        state.snapshot.peers.retain(|p| p.id != id);
                        if state.snapshot.peers.len() < 128 {
                            state.snapshot.peers.push(peer);
                        }
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        state
                            .snapshot
                            .peers
                            .retain(|p| !fullname.starts_with(&format!("{}.", p.id)));
                    }
                    _ => {}
                }
            }
        });
        Ok(())
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        if let Ok(daemon) = self.daemon.lock() {
            if let Some(daemon) = daemon.as_ref() {
                let _ = daemon.shutdown();
            }
        }
    }
}
fn read_settings(path: Option<&Path>) -> Result<Settings> {
    let Some(path) = path else {
        return Ok(Settings::default());
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Settings::default());
        }
        Err(error) => return Err(format!("无法读取设备设置：{error}")),
    };
    let settings: Settings = serde_json::from_slice(&bytes)
        .map_err(|_| "设备设置文件损坏，无法读取设备身份和信任记录".to_string())?;
    if settings.trusted_devices.iter().any(|device| {
        device.public_key.len() != 64
            || !device
                .public_key
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || !valid_device_name(&device.name)
    }) {
        return Err("保存的信任记录无效，无法启动安全连接".into());
    }
    Ok(settings)
}
fn write_settings(path: &Path, bytes: &[u8]) -> Result<()> {
    let result = (|| -> std::io::Result<()> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".filehop-settings-{}.tmp", Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        // The settings now contain the device's private identity key.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        let _cleanup = TemporaryFiles {
            paths: vec![temporary.clone()],
        };
        std::io::Write::write_all(&mut file, bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    result.map_err(|error| format!("无法保存设备身份和信任设置：{error}"))
}
fn service_info(device: &Device) -> Result<ServiceInfo> {
    let properties = [
        ("id", device.id.as_str()),
        ("name", device.name.as_str()),
        ("platform", device.platform.as_str()),
        ("version", "2"),
    ];
    ServiceInfo::new(
        SERVICE,
        &device.id,
        &format!("filehop-{}.local.", device.id),
        "",
        device.port,
        &properties[..],
    )
    .map(|info| info.enable_addr_auto())
    .map_err(|e| format!("无法广播设备：{e}"))
}
fn local_addresses() -> Vec<String> {
    let mut addresses: Vec<_> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|interface| !interface.is_loopback() && interface.ip().is_ipv4())
        .map(|interface| interface.ip().to_string())
        .collect();
    addresses.sort();
    addresses.dedup();
    addresses
}
fn valid_device_name(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= 40 && !name.chars().any(char::is_control)
}
fn normalize_address(address: &str) -> Result<String> {
    let value = address.trim();
    if value.is_empty()
        || value.len() > 255
        || value.contains(['/', '\\', '@'])
        || value.chars().any(char::is_whitespace)
    {
        return Err("请输入对方的 IP 地址，例如 192.168.1.12:53318".into());
    }
    if let Ok(ip) = value.parse::<IpAddr>() {
        return Ok(if ip.is_ipv6() {
            format!("[{ip}]:53318")
        } else {
            format!("{ip}:53318")
        });
    }
    if let Some((host, port)) = value.rsplit_once(':') {
        if host.is_empty() || port.parse::<u16>().ok().filter(|p| *p > 0).is_none() {
            return Err("端口必须为 1–65535".into());
        }
        Ok(value.into())
    } else {
        Ok(format!("{value}:53318"))
    }
}
pub fn validate_name(name: &str) -> Result<()> {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if name.is_empty()
        || name.len() > 200
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
        || reserved
    {
        return Err(format!(
            "文件名无法安全地跨平台保存：{}",
            name.chars().take(80).collect::<String>()
        ));
    }
    Ok(())
}
fn text_file_name(name: Option<&str>) -> Result<String> {
    let name = name.unwrap_or_default().trim();
    if name.is_empty() {
        return Ok(format!("文案-{}.txt", now()));
    }
    validate_name(name)?;
    let name = if name.to_ascii_lowercase().ends_with(".txt") {
        name.to_string()
    } else {
        format!("{name}.txt")
    };
    validate_name(&name)?;
    Ok(name)
}
fn validate_files(files: &[OfferedFile]) -> Result<u64> {
    if files.is_empty() || files.len() > MAX_FILES {
        return Err("一次只支持 1–100 个文件".into());
    }
    let mut total = 0u64;
    for file in files {
        validate_name(&file.name)?;
        total = total
            .checked_add(file.size)
            .filter(|s| *s <= MAX_TOTAL)
            .ok_or("文件总大小超出上限（10 TiB）")?;
    }
    Ok(total)
}
#[derive(Default)]
struct TemporaryFiles {
    paths: Vec<PathBuf>,
}
impl Drop for TemporaryFiles {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}
async fn publish_file(temp: &Path, directory: &Path, name: &str) -> Result<PathBuf> {
    for index in 0..10_000 {
        let candidate = if index == 0 {
            name.into()
        } else {
            let p = Path::new(name);
            let stem = p.file_stem().unwrap_or_default().to_string_lossy();
            match p.extension() {
                Some(ext) => format!("{stem} ({index}).{}", ext.to_string_lossy()),
                None => format!("{stem} ({index})"),
            }
        };
        let target = directory.join(candidate);
        match std::fs::hard_link(temp, &target) {
            Ok(()) => return Ok(target),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                // Filesystems such as FAT lack hard links. Exclusive creation still
                // prevents overwrites; remove the new file if its copy fails.
                let file = match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                {
                    Ok(file) => file,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(format!("无法保存文件：{e}")),
                };
                let mut cleanup = TemporaryFiles {
                    paths: vec![target.clone()],
                };
                let mut file = File::from_std(file);
                let mut source = File::open(temp).await.map_err(|e| e.to_string())?;
                tokio::io::copy(&mut source, &mut file)
                    .await
                    .map_err(|e| format!("保存文件失败：{e}"))?;
                file.flush().await.map_err(|e| e.to_string())?;
                file.sync_all().await.map_err(|e| e.to_string())?;
                cleanup.paths.clear();
                return Ok(target);
            }
        }
    }
    Err("同名文件过多，请更换接收文件夹".into())
}
async fn report_error(secure: &mut Secure, result: &Result<()>) {
    if let Err(error) = result {
        let message = match error.as_str() {
            CANCELLED => Control::Cancel,
            REJECTED => Control::Reject,
            _ => Control::Error {
                message: error.chars().take(300).collect(),
            },
        };
        let _ = timeout(Duration::from_secs(2), secure.control(&message)).await;
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
