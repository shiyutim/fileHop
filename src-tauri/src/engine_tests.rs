use super::*;
use tokio::time::{sleep, Instant};

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("filehop-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn pair(root: &TestDir) -> (Arc<Engine>, Arc<Engine>) {
    let left = Engine::new(root.0.join("left"), None);
    let right = Engine::new(root.0.join("right"), None);
    left.start(0, false).await.unwrap();
    right.start(0, false).await.unwrap();
    (left, right)
}
async fn wait(engine: &Engine, predicate: impl Fn(&Transfer) -> bool) -> Transfer {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(t) = engine.snapshot().transfers.into_iter().find(&predicate) {
            return t;
        }
        assert!(
            Instant::now() < deadline,
            "timed out: {:?}",
            engine.snapshot().transfers
        );
        sleep(Duration::from_millis(10)).await;
    }
}
async fn send_path(sender: &Arc<Engine>, receiver: &Engine, path: &Path) -> String {
    let files = sender
        .inspect(vec![path.to_string_lossy().into_owned()])
        .await
        .unwrap();
    sender
        .send_files(
            format!("127.0.0.1:{}", receiver.snapshot().device.port),
            files.into_iter().map(|f| f.path.unwrap()).collect(),
            None,
        )
        .await
        .unwrap()
}
async fn send_named_text(sender: &Arc<Engine>, receiver: &Engine, name: &str) -> String {
    sender
        .send_text(
            format!("127.0.0.1:{}", receiver.snapshot().device.port),
            format!("内容：{name}\n"),
            Some(name.into()),
            None,
        )
        .await
        .unwrap()
}
async fn finish_named_transfer(
    sender: &Engine,
    receiver: &Engine,
    id: &str,
    name: &str,
) -> (Transfer, Transfer) {
    let sent = wait(sender, |t| t.id == id && !active(&t.status)).await;
    let received = wait(receiver, |t| t.files[0].name == name && !active(&t.status)).await;
    assert_eq!(sent.status, "completed", "{:?}", sent.error);
    assert_eq!(received.status, "completed", "{:?}", received.error);
    (sent, received)
}
async fn establish_trust(sender: &Arc<Engine>, receiver: &Arc<Engine>, sender_trusts: bool) {
    let id = send_named_text(sender, receiver, "初次确认").await;
    let outgoing = wait(sender, |t| t.id == id && t.status == "waiting").await;
    let incoming = wait(receiver, |t| t.status == "waiting").await;
    assert!(!outgoing.peer_trusted);
    assert!(!incoming.peer_trusted);
    assert!(!outgoing.local_confirmed);
    assert!(!incoming.local_confirmed);
    assert_eq!(outgoing.verification_code, incoming.verification_code);
    sender.respond(&id, true, sender_trusts).unwrap();
    receiver.respond(&incoming.id, true, true).unwrap();
    finish_named_transfer(sender, receiver, &id, "初次确认.txt").await;
    assert_eq!(
        sender.snapshot().trusted_devices.len(),
        usize::from(sender_trusts)
    );
    assert_eq!(receiver.snapshot().trusted_devices.len(), 1);
}

#[tokio::test]
async fn encrypted_transfer_requires_both_confirmations_and_never_overwrites() {
    let root = TestDir::new();
    let (sender, receiver) = pair(&root).await;
    let path = root.0.join("example.bin");
    let payload: Vec<u8> = (0..2_000_000).map(|n| (n % 251) as u8).collect();
    fs::write(&path, &payload).await.unwrap();
    fs::create_dir_all(&receiver.snapshot().save_dir)
        .await
        .unwrap();
    fs::write(
        PathBuf::from(receiver.snapshot().save_dir).join("example.bin"),
        b"existing file",
    )
    .await
    .unwrap();
    let id = send_path(&sender, &receiver, &path).await;
    let outgoing = wait(&sender, |t| t.status == "waiting").await;
    let incoming = wait(&receiver, |t| t.status == "waiting").await;
    assert_eq!(outgoing.verification_code, incoming.verification_code);
    assert_eq!(outgoing.verification_code.unwrap().len(), 32);
    assert!(!outgoing.peer_trusted);
    assert!(!incoming.peer_trusted);
    receiver.respond(&incoming.id, true, false).unwrap();
    sleep(Duration::from_millis(100)).await;
    assert_eq!(
        sender.snapshot().transfers[0].transferred_bytes,
        0,
        "remote approval alone must not release file bytes"
    );
    assert_eq!(sender.snapshot().transfers[0].status, "waiting");
    sender.respond(&id, true, false).unwrap();
    let sent = wait(&sender, |t| !active(&t.status)).await;
    let received = wait(&receiver, |t| !active(&t.status)).await;
    assert_eq!(sent.status, "completed", "{:?}", sent.error);
    assert_eq!(received.status, "completed", "{:?}", received.error);
    assert_eq!(sent.transferred_bytes, payload.len() as u64);
    assert_eq!(received.transferred_bytes, payload.len() as u64);
    let save = PathBuf::from(receiver.snapshot().save_dir);
    assert_eq!(
        fs::read(save.join("example.bin")).await.unwrap(),
        b"existing file"
    );
    assert_eq!(
        fs::read(save.join("example (1).bin")).await.unwrap(),
        payload
    );
    assert_eq!(
        std::fs::read_dir(save).unwrap().count(),
        2,
        "temporary files should be removed"
    );
    assert!(sender.snapshot().trusted_devices.is_empty());
    assert!(receiver.snapshot().trusted_devices.is_empty());
}

#[tokio::test]
async fn mutual_trust_automatically_transfers_files_and_text_after_name_change() {
    let root = TestDir::new();
    let (sender, receiver) = pair(&root).await;
    establish_trust(&sender, &receiver, true).await;
    sender.set_name("改名后的电脑".into()).unwrap();

    let path = root.0.join("trusted.bin");
    let payload: Vec<u8> = (0..200_000).map(|n| (n % 251) as u8).collect();
    fs::write(&path, &payload).await.unwrap();
    let id = send_path(&sender, &receiver, &path).await;
    let (sent, received) = finish_named_transfer(&sender, &receiver, &id, "trusted.bin").await;
    for transfer in [sent, received] {
        assert!(transfer.peer_trusted);
        assert!(transfer.local_confirmed);
        assert_eq!(transfer.transferred_bytes, payload.len() as u64);
    }
    assert_eq!(
        fs::read(PathBuf::from(receiver.snapshot().save_dir).join("trusted.bin"))
            .await
            .unwrap(),
        payload
    );

    let id = send_named_text(&receiver, &sender, "自动发送文案").await;
    let (sent, received) = finish_named_transfer(&receiver, &sender, &id, "自动发送文案.txt").await;
    assert!(sent.peer_trusted && received.peer_trusted);
    assert_eq!(
        fs::read(PathBuf::from(sender.snapshot().save_dir).join("自动发送文案.txt"))
            .await
            .unwrap(),
        "内容：自动发送文案\n".as_bytes()
    );
}

#[tokio::test]
async fn one_sided_trust_only_bypasses_the_consenting_devices_confirmation() {
    let root = TestDir::new();
    let (sender, receiver) = pair(&root).await;
    establish_trust(&sender, &receiver, false).await;

    let id = send_named_text(&sender, &receiver, "单方信任收件").await;
    let outgoing = wait(&sender, |t| t.id == id && t.status == "waiting").await;
    let incoming = wait(&receiver, |t| {
        t.files[0].name == "单方信任收件.txt" && t.local_confirmed
    })
    .await;
    assert!(!outgoing.peer_trusted);
    assert!(!outgoing.local_confirmed);
    assert!(incoming.peer_trusted);
    assert_eq!(incoming.transferred_bytes, 0);
    assert!(!PathBuf::from(receiver.snapshot().save_dir)
        .join("单方信任收件.txt")
        .exists());
    sender.respond(&id, true, false).unwrap();
    finish_named_transfer(&sender, &receiver, &id, "单方信任收件.txt").await;

    let id = send_named_text(&receiver, &sender, "单方信任发件").await;
    let outgoing = wait(&receiver, |t| t.id == id && t.local_confirmed).await;
    let incoming = wait(&sender, |t| {
        t.files[0].name == "单方信任发件.txt" && t.status == "waiting"
    })
    .await;
    assert!(outgoing.peer_trusted);
    assert_eq!(outgoing.transferred_bytes, 0);
    assert!(!incoming.peer_trusted);
    assert!(!incoming.local_confirmed);
    sender.respond(&incoming.id, true, false).unwrap();
    finish_named_transfer(&receiver, &sender, &id, "单方信任发件.txt").await;
    assert!(sender.snapshot().trusted_devices.is_empty());
}

#[tokio::test]
async fn identity_and_trust_survive_restart_and_revocation_is_persisted() {
    let root = TestDir::new();
    let sender_settings = root.0.join("sender-settings.json");
    let receiver_settings = root.0.join("receiver-settings.json");
    let sender = Engine::new(root.0.join("left"), Some(sender_settings.clone()));
    let receiver = Engine::new(root.0.join("right"), Some(receiver_settings.clone()));
    sender.start(0, false).await.unwrap();
    receiver.start(0, false).await.unwrap();
    establish_trust(&sender, &receiver, true).await;
    let trusted_sender_key = receiver.snapshot().trusted_devices[0].public_key.clone();
    let trusted_receiver_key = sender.snapshot().trusted_devices[0].public_key.clone();
    assert_ne!(trusted_sender_key, trusted_receiver_key);
    drop(sender);
    drop(receiver);

    let sender = Engine::new(root.0.join("left"), Some(sender_settings));
    let receiver = Engine::new(root.0.join("right"), Some(receiver_settings.clone()));
    sender.start(0, false).await.unwrap();
    receiver.start(0, false).await.unwrap();
    assert_eq!(
        sender.snapshot().trusted_devices[0].public_key,
        trusted_receiver_key
    );
    assert_eq!(
        receiver.snapshot().trusted_devices[0].public_key,
        trusted_sender_key
    );
    let id = send_named_text(&sender, &receiver, "重启后自动传输").await;
    let (sent, received) =
        finish_named_transfer(&sender, &receiver, &id, "重启后自动传输.txt").await;
    assert!(sent.peer_trusted && received.peer_trusted);

    receiver.revoke_trusted_device(&trusted_sender_key).unwrap();
    assert!(receiver.snapshot().trusted_devices.is_empty());
    drop(receiver);
    let receiver = Engine::new(root.0.join("right"), Some(receiver_settings));
    receiver.start(0, false).await.unwrap();
    assert!(receiver.snapshot().trusted_devices.is_empty());
    let id = send_named_text(&sender, &receiver, "取消信任后").await;
    let incoming = wait(&receiver, |t| t.status == "waiting").await;
    let outgoing = wait(&sender, |t| t.id == id && t.local_confirmed).await;
    assert!(outgoing.peer_trusted);
    assert!(!incoming.peer_trusted);
    assert!(!incoming.local_confirmed);
    assert_eq!(outgoing.transferred_bytes, 0);
    receiver.respond(&incoming.id, true, false).unwrap();
    finish_named_transfer(&sender, &receiver, &id, "取消信任后.txt").await;
    assert!(receiver.snapshot().trusted_devices.is_empty());
}

#[tokio::test]
async fn an_untrusted_device_cannot_gain_trust_by_copying_a_trusted_devices_name() {
    let root = TestDir::new();
    let (sender, receiver) = pair(&root).await;
    sender.set_name("常用电脑".into()).unwrap();
    establish_trust(&sender, &receiver, true).await;
    let trusted_key = receiver.snapshot().trusted_devices[0].public_key.clone();
    let impostor = Engine::new(root.0.join("impostor"), None);
    impostor.set_name(sender.snapshot().device.name).unwrap();
    impostor.start(0, false).await.unwrap();

    let id = send_named_text(&impostor, &receiver, "同名新设备").await;
    let incoming = wait(&receiver, |t| {
        t.files[0].name == "同名新设备.txt" && t.status == "waiting"
    })
    .await;
    assert_eq!(incoming.peer_name, "常用电脑");
    assert!(!incoming.peer_trusted);
    assert!(!incoming.local_confirmed);
    assert_eq!(incoming.transferred_bytes, 0);
    assert_eq!(receiver.snapshot().trusted_devices.len(), 1);
    assert_eq!(
        receiver.snapshot().trusted_devices[0].public_key,
        trusted_key
    );
    receiver.respond(&incoming.id, false, true).unwrap();
    assert_eq!(
        wait(&impostor, |t| t.id == id && !active(&t.status))
            .await
            .status,
        "rejected"
    );
    assert_eq!(receiver.snapshot().trusted_devices.len(), 1);
}

#[tokio::test]
async fn failed_trust_save_leaves_confirmation_pending_and_allows_accepting_once() {
    let root = TestDir::new();
    let settings_path = root.0.join("receiver-settings.json");
    let sender = Engine::new(root.0.join("left"), None);
    let receiver = Engine::new(root.0.join("right"), Some(settings_path.clone()));
    sender.start(0, false).await.unwrap();
    receiver.start(0, false).await.unwrap();
    let id = send_named_text(&sender, &receiver, "保存失败").await;
    wait(&sender, |t| t.id == id && t.status == "waiting").await;
    let incoming = wait(&receiver, |t| t.status == "waiting").await;
    fs::remove_file(&settings_path).await.unwrap();
    fs::create_dir(&settings_path).await.unwrap();
    sender.respond(&id, true, false).unwrap();

    assert!(receiver.respond(&incoming.id, true, true).is_err());
    sleep(Duration::from_millis(100)).await;
    let state = receiver.snapshot();
    assert!(state.trusted_devices.is_empty());
    let pending = state
        .transfers
        .iter()
        .find(|t| t.id == incoming.id)
        .unwrap();
    assert_eq!(pending.status, "waiting");
    assert!(!pending.local_confirmed);
    assert!(!pending.peer_trusted);
    assert_eq!(pending.transferred_bytes, 0);
    assert!(!Path::new(&state.save_dir).exists());

    receiver.respond(&incoming.id, true, false).unwrap();
    finish_named_transfer(&sender, &receiver, &id, "保存失败.txt").await;
    assert!(receiver.snapshot().trusted_devices.is_empty());
}

#[test]
fn device_name_defaults_to_neutral_and_preserves_saved_names() {
    let root = TestDir::new();
    let settings_path = root.0.join("settings.json");
    let new_engine = || Engine::new(root.0.join("downloads"), Some(settings_path.clone()));
    assert_eq!(new_engine().snapshot().device.name, "我的电脑");

    for name in [None, Some(""), Some("\n"), Some(" ")] {
        std::fs::write(
            &settings_path,
            serde_json::to_vec(&serde_json::json!({ "name": name })).unwrap(),
        )
        .unwrap();
        assert_eq!(new_engine().snapshot().device.name, "我的电脑");
    }

    let engine = new_engine();
    engine.set_name("工作电脑".into()).unwrap();
    drop(engine);
    assert_eq!(new_engine().snapshot().device.name, "工作电脑");
}

#[tokio::test]
async fn legacy_settings_keep_preferences_and_persist_a_stable_identity() {
    let root = TestDir::new();
    let settings_path = root.0.join("settings.json");
    let save_dir = root.0.join("已有下载目录");
    fs::write(
        &settings_path,
        serde_json::to_vec(&serde_json::json!({
            "name": "原来的电脑名称",
            "saveDir": save_dir,
        }))
        .unwrap(),
    )
    .await
    .unwrap();

    let engine = Engine::new(root.0.join("fallback"), Some(settings_path.clone()));
    assert!(engine
        .send_text("127.0.0.1:53318".into(), "启动前文案".into(), None, None)
        .await
        .is_err());
    assert!(engine.snapshot().transfers.is_empty());
    engine.start(0, false).await.unwrap();
    assert_eq!(engine.snapshot().device.name, "原来的电脑名称");
    assert_eq!(PathBuf::from(engine.snapshot().save_dir), save_dir);
    assert!(engine.snapshot().trusted_devices.is_empty());
    let migrated: serde_json::Value =
        serde_json::from_slice(&fs::read(&settings_path).await.unwrap()).unwrap();
    let identity = migrated["privateKey"].as_array().unwrap();
    assert_eq!(identity.len(), 32);
    assert!(identity
        .iter()
        .all(|byte| byte.as_u64().is_some_and(|n| n <= 255)));
    assert_eq!(migrated["trustedDevices"], serde_json::json!([]));
    assert_eq!(migrated["name"], "原来的电脑名称");
    assert_eq!(migrated["saveDir"], serde_json::json!(save_dir));
    drop(engine);

    let reloaded = Engine::new(root.0.join("other-fallback"), Some(settings_path.clone()));
    reloaded.start(0, false).await.unwrap();
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(settings_path).await.unwrap()).unwrap();
    assert!(persisted["privateKey"] == migrated["privateKey"]);
    assert_eq!(reloaded.snapshot().device.name, "原来的电脑名称");
    assert_eq!(PathBuf::from(reloaded.snapshot().save_dir), save_dir);
    assert!(reloaded.snapshot().trusted_devices.is_empty());
}

#[test]
fn corrupt_settings_fail_startup_without_overwriting_or_deadlocking() {
    let root = TestDir::new();
    let cases = [
        b"{invalid json".to_vec(),
        serde_json::to_vec(&serde_json::json!({"privateKey": [1, 2]})).unwrap(),
        serde_json::to_vec(&serde_json::json!({
            "trustedDevices": [{
                "publicKey": "ab".repeat(32),
                "name": "身份丢失的电脑",
                "trustedAt": 1,
            }]
        }))
        .unwrap(),
    ];
    for (index, bytes) in cases.into_iter().enumerate() {
        let settings_path = root.0.join(format!("corrupt-{index}.json"));
        std::fs::write(&settings_path, &bytes).unwrap();
        let engine = Engine::new(root.0.join("downloads"), Some(settings_path.clone()));
        let worker_engine = engine.clone();
        let (result_sender, result_receiver) = std::sync::mpsc::channel();
        // An async timeout cannot interrupt a synchronous mutex deadlock.
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let result = runtime.block_on(async {
                let startup = worker_engine.start(0, false).await;
                let outbound = worker_engine
                    .send_text("127.0.0.1:53318".into(), "有效文案".into(), None, None)
                    .await;
                (startup, outbound)
            });
            let _ = result_sender.send(result);
        });
        let (startup, outbound) = result_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("startup with corrupt settings must return without deadlocking");
        let error = startup.unwrap_err();
        assert!(outbound.is_err());
        assert!(engine.snapshot().transfers.is_empty());
        assert!(!error.is_empty());
        assert_eq!(
            engine.snapshot().server_error.as_deref(),
            Some(error.as_str())
        );
        assert_eq!(engine.snapshot().device.port, 0);
        assert_eq!(std::fs::read(&settings_path).unwrap(), bytes);
    }
}

#[tokio::test]
async fn failed_revocation_preserves_trust_in_memory_and_on_disk() {
    let root = TestDir::new();
    let settings_path = root.0.join("receiver-settings.json");
    let original_path = root.0.join("receiver-settings-backup.json");
    let sender = Engine::new(root.0.join("left"), None);
    let receiver = Engine::new(root.0.join("right"), Some(settings_path.clone()));
    sender.start(0, false).await.unwrap();
    receiver.start(0, false).await.unwrap();
    establish_trust(&sender, &receiver, true).await;
    let trusted_key = receiver.snapshot().trusted_devices[0].public_key.clone();
    let original_bytes = fs::read(&settings_path).await.unwrap();

    // Keep the real settings intact while forcing replacement to fail on every OS.
    fs::rename(&settings_path, &original_path).await.unwrap();
    fs::create_dir(&settings_path).await.unwrap();
    assert!(receiver.revoke_trusted_device(&trusted_key).is_err());
    assert_eq!(receiver.snapshot().trusted_devices.len(), 1);
    assert_eq!(
        receiver.snapshot().trusted_devices[0].public_key,
        trusted_key
    );
    assert_eq!(fs::read(&original_path).await.unwrap(), original_bytes);
    fs::remove_dir(&settings_path).await.unwrap();
    fs::rename(&original_path, &settings_path).await.unwrap();
    drop(receiver);

    let receiver = Engine::new(root.0.join("right"), Some(settings_path));
    receiver.start(0, false).await.unwrap();
    assert_eq!(receiver.snapshot().trusted_devices.len(), 1);
    assert_eq!(
        receiver.snapshot().trusted_devices[0].public_key,
        trusted_key
    );
    let id = send_named_text(&sender, &receiver, "取消信任失败后").await;
    let (sent, received) =
        finish_named_transfer(&sender, &receiver, &id, "取消信任失败后.txt").await;
    assert!(sent.peer_trusted && received.peer_trusted);
}

#[tokio::test]
async fn rejection_stops_both_sides_without_saving_files() {
    let root = TestDir::new();
    let (sender, receiver) = pair(&root).await;
    let path = root.0.join("private.txt");
    fs::write(&path, b"private").await.unwrap();
    send_path(&sender, &receiver, &path).await;
    let incoming = wait(&receiver, |t| t.status == "waiting").await;
    receiver.respond(&incoming.id, false, true).unwrap();
    assert_eq!(
        wait(&receiver, |t| !active(&t.status)).await.status,
        "rejected"
    );
    assert_eq!(
        wait(&sender, |t| !active(&t.status)).await.status,
        "rejected"
    );
    assert!(!Path::new(&receiver.snapshot().save_dir).exists());
    assert!(receiver.snapshot().trusted_devices.is_empty());
    assert!(sender.snapshot().trusted_devices.is_empty());
}

#[tokio::test]
async fn cancellation_while_waiting_propagates() {
    let root = TestDir::new();
    let (sender, receiver) = pair(&root).await;
    let path = root.0.join("cancel.txt");
    fs::write(&path, b"cancel").await.unwrap();
    let id = send_path(&sender, &receiver, &path).await;
    wait(&receiver, |t| t.status == "waiting").await;
    sender.cancel(&id).unwrap();
    assert_eq!(
        wait(&sender, |t| !active(&t.status)).await.status,
        "cancelled"
    );
    assert_eq!(
        wait(&receiver, |t| !active(&t.status)).await.status,
        "cancelled"
    );
}

async fn manual_sender(receiver: &Engine, name: &str, size: u64) -> Secure {
    let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, receiver.snapshot().device.port))
        .await
        .unwrap();
    let identity = protocol::generate_identity().unwrap();
    let mut secure = Secure::handshake(stream, true, &identity).await.unwrap();
    secure
        .control(&Control::Offer {
            version: 2,
            name: "Test sender".into(),
            files: vec![OfferedFile {
                name: name.into(),
                size,
            }],
        })
        .await
        .unwrap();
    secure
}
async fn approve_manual(receiver: &Engine, secure: &mut Secure) -> String {
    assert!(matches!(
        secure.receive_control(2).await.unwrap(),
        Control::Hello { .. }
    ));
    let incoming = wait(receiver, |t| t.status == "waiting").await;
    assert_eq!(
        incoming.verification_code.as_deref(),
        Some(secure.code.as_str())
    );
    receiver.respond(&incoming.id, true, false).unwrap();
    secure.control(&Control::Ready).await.unwrap();
    assert!(matches!(
        secure.receive_control(2).await.unwrap(),
        Control::Ready
    ));
    incoming.id
}
#[tokio::test]
async fn cancelled_partial_file_is_removed() {
    let root = TestDir::new();
    let (_, receiver) = pair(&root).await;
    let mut secure = manual_sender(&receiver, "large.bin", 1_000_000).await;
    let id = approve_manual(&receiver, &mut secure).await;
    secure.data(&vec![42; 20_000]).await.unwrap();
    wait(&receiver, |t| t.transferred_bytes > 0).await;
    receiver.cancel(&id).unwrap();
    assert_eq!(
        wait(&receiver, |t| !active(&t.status)).await.status,
        "cancelled"
    );
    assert_eq!(secure.receive_control(2).await.unwrap_err(), CANCELLED);
    assert_eq!(
        std::fs::read_dir(receiver.snapshot().save_dir)
            .unwrap()
            .count(),
        0
    );
}
#[tokio::test]
async fn corrupt_checksum_never_publishes_file() {
    let root = TestDir::new();
    let (_, receiver) = pair(&root).await;
    let mut secure = manual_sender(&receiver, "bad.bin", 5).await;
    approve_manual(&receiver, &mut secure).await;
    secure.data(b"hello").await.unwrap();
    secure
        .control(&Control::FileEnd {
            sha256: "0".repeat(64),
        })
        .await
        .unwrap();
    let failed = wait(&receiver, |t| !active(&t.status)).await;
    assert_eq!(failed.status, "failed");
    assert!(failed.error.unwrap().contains("校验失败"));
    assert_eq!(
        std::fs::read_dir(receiver.snapshot().save_dir)
            .unwrap()
            .count(),
        0
    );
}
#[tokio::test]
async fn excess_file_bytes_are_rejected_and_cleaned_up() {
    let root = TestDir::new();
    let (_, receiver) = pair(&root).await;
    let mut secure = manual_sender(&receiver, "overflow.bin", 1).await;
    approve_manual(&receiver, &mut secure).await;
    secure.data(b"too long").await.unwrap();
    assert_eq!(
        wait(&receiver, |t| !active(&t.status)).await.status,
        "failed"
    );
    assert_eq!(
        std::fs::read_dir(receiver.snapshot().save_dir)
            .unwrap()
            .count(),
        0
    );
}
#[tokio::test]
async fn traversal_offer_is_rejected_before_user_prompt() {
    let root = TestDir::new();
    let (_, receiver) = pair(&root).await;
    let mut secure = manual_sender(&receiver, "../escape.txt", 0).await;
    assert!(secure
        .receive_control(2)
        .await
        .unwrap_err()
        .contains("文件名"));
    assert!(receiver.snapshot().transfers.is_empty());
    assert!(!root.0.join("escape.txt").exists());
}
#[test]
fn cross_platform_filename_and_size_boundaries() {
    for name in [
        "../x", "a/b", "a\\b", "CON.txt", "nul", "LPT1", "a.", "a ", "x:y", ".", "..", "\0foo", "",
    ] {
        assert!(validate_name(name).is_err(), "{name:?}");
    }
    for name in ["你好.txt", "report (1).pdf", "README", "file.name.png"] {
        assert!(validate_name(name).is_ok(), "{name:?}");
    }
    assert!(validate_files(&[OfferedFile {
        name: "a".into(),
        size: u64::MAX
    }])
    .is_err());
    assert!(validate_files(&vec![
        OfferedFile {
            name: "a".into(),
            size: 0
        };
        101
    ])
    .is_err());
}

#[tokio::test]
async fn multiple_files_including_empty_and_unicode_complete_in_one_session() {
    let root = TestDir::new();
    let (sender, receiver) = pair(&root).await;
    let expected: [(&str, &[u8]); 3] = [
        ("empty.txt", b""),
        ("你好，文件.txt", "来自另一台电脑的文件\n".as_bytes()),
        ("payload.bin", &[0, 1, 2, 255, 128, 64, 0]),
    ];
    let mut paths = Vec::new();
    for (name, bytes) in expected {
        let path = root.0.join(name);
        fs::write(&path, bytes).await.unwrap();
        paths.push(path.to_string_lossy().into_owned());
    }
    let files = sender.inspect(paths).await.unwrap();
    let id = sender
        .send_files(
            format!("127.0.0.1:{}", receiver.snapshot().device.port),
            files.into_iter().map(|f| f.path.unwrap()).collect(),
            None,
        )
        .await
        .unwrap();
    wait(&sender, |t| t.status == "waiting").await;
    let incoming = wait(&receiver, |t| t.status == "waiting").await;
    sender.respond(&id, true, false).unwrap();
    receiver.respond(&incoming.id, true, false).unwrap();
    let sent = wait(&sender, |t| !active(&t.status)).await;
    let received = wait(&receiver, |t| !active(&t.status)).await;
    assert_eq!(sent.status, "completed", "{:?}", sent.error);
    assert_eq!(received.status, "completed", "{:?}", received.error);
    let total: u64 = expected.iter().map(|(_, bytes)| bytes.len() as u64).sum();
    for transfer in [sent, received] {
        assert_eq!(transfer.files.len(), 3);
        assert_eq!(transfer.total_bytes, total);
        assert_eq!(transfer.transferred_bytes, total);
    }
    let save_dir = PathBuf::from(receiver.snapshot().save_dir);
    for (name, bytes) in expected {
        assert_eq!(fs::read(save_dir.join(name)).await.unwrap(), bytes);
    }
    assert_eq!(std::fs::read_dir(save_dir).unwrap().count(), 3);
}

#[tokio::test]
async fn folder_selection_keeps_original_paths_without_preparing_an_archive() {
    let root = TestDir::new();
    let engine = Engine::new(root.0.join("downloads"), None);
    let folder = root.0.join("待发送资料");
    fs::create_dir_all(folder.join("空目录")).await.unwrap();
    fs::write(folder.join("原文.txt"), "保持原样\n")
        .await
        .unwrap();
    let path = fs::canonicalize(&folder)
        .await
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let selected = engine
        .inspect(vec![path.clone(), path.clone()])
        .await
        .unwrap();
    assert_eq!(
        selected.len(),
        1,
        "duplicate selections should be deduplicated"
    );
    assert_eq!(selected[0].name, "待发送资料");
    assert_eq!(selected[0].path.as_deref(), Some(path.as_str()));
    assert!(selected[0].is_directory);
    assert_eq!(
        selected[0].size, 0,
        "folder size is determined when sending"
    );
    // Dropping the queue item must not require temporary-file cleanup.
    drop(selected);
    assert!(engine.snapshot().transfers.is_empty());
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 2);
    assert_eq!(
        fs::read(folder.join("原文.txt")).await.unwrap(),
        "保持原样\n".as_bytes()
    );
    assert!(folder.join("空目录").is_dir());
}

#[tokio::test]
async fn folder_zip_and_plain_file_transfer_preserves_contents_and_existing_zip() {
    use std::io::Read;

    let root = TestDir::new();
    let (sender, receiver) = pair(&root).await;
    let folder = root.0.join("旅行资料");
    fs::create_dir_all(folder.join("照片/空相册"))
        .await
        .unwrap();
    let nested_payload = "你好，世界 🌏\r\n第二行\n".repeat(4000);
    fs::write(folder.join("照片/说明.txt"), nested_payload.as_bytes())
        .await
        .unwrap();
    fs::write(folder.join("空文件.txt"), b"").await.unwrap();
    let plain = root.0.join("单独文件.bin");
    let plain_payload = [0, 1, 2, 255, 128, 64, 0];
    fs::write(&plain, plain_payload).await.unwrap();
    let save = PathBuf::from(receiver.snapshot().save_dir);
    fs::create_dir_all(&save).await.unwrap();
    fs::write(save.join("旅行资料.zip"), b"existing archive")
        .await
        .unwrap();

    let selected = sender
        .inspect(vec![
            folder.to_str().unwrap().into(),
            plain.to_str().unwrap().into(),
        ])
        .await
        .unwrap();
    let id = sender
        .send_files(
            format!("127.0.0.1:{}", receiver.snapshot().device.port),
            selected
                .into_iter()
                .map(|file| file.path.unwrap())
                .collect(),
            None,
        )
        .await
        .unwrap();
    // On the current-thread runtime, the spawned preparation task cannot run
    // until this test yields, so this checks the immediate cancellable state.
    let preparing = sender
        .snapshot()
        .transfers
        .into_iter()
        .find(|t| t.id == id)
        .unwrap();
    assert_eq!(preparing.status, "preparing");
    assert_eq!(preparing.files[0].name, "旅行资料");
    assert!(preparing.files[0].is_directory);
    assert!(receiver.snapshot().transfers.is_empty());

    let outgoing = wait(&sender, |t| t.id == id && t.status == "waiting").await;
    let incoming = wait(&receiver, |t| t.status == "waiting").await;
    for transfer in [&outgoing, &incoming] {
        assert_eq!(transfer.files.len(), 2);
        assert_eq!(transfer.files[0].name, "旅行资料.zip");
        assert!(!transfer.files[0].is_directory);
        assert!(transfer.files[0].size > 0);
        assert_eq!(transfer.files[1].name, "单独文件.bin");
        assert_eq!(transfer.transferred_bytes, 0);
    }
    assert_eq!(outgoing.verification_code, incoming.verification_code);
    sender.respond(&id, true, false).unwrap();
    receiver.respond(&incoming.id, true, false).unwrap();
    let sent = wait(&sender, |t| t.id == id && !active(&t.status)).await;
    let received = wait(&receiver, |t| t.id == incoming.id && !active(&t.status)).await;
    let archive_path = save.join("旅行资料 (1).zip");
    let archive_size = fs::metadata(&archive_path).await.unwrap().len();
    for transfer in [sent, received] {
        assert_eq!(transfer.status, "completed", "{:?}", transfer.error);
        assert_eq!(transfer.files[0].size, archive_size);
        assert_eq!(
            transfer.total_bytes,
            archive_size + plain_payload.len() as u64
        );
        assert_eq!(transfer.transferred_bytes, transfer.total_bytes);
    }
    assert_eq!(
        fs::read(save.join("旅行资料.zip")).await.unwrap(),
        b"existing archive"
    );
    assert_eq!(
        fs::read(save.join("单独文件.bin")).await.unwrap(),
        plain_payload
    );
    assert_eq!(std::fs::read_dir(&save).unwrap().count(), 3);

    let mut archive = zip::ZipArchive::new(std::fs::File::open(archive_path).unwrap()).unwrap();
    assert_eq!(archive.len(), 5);
    for name in ["旅行资料/", "旅行资料/照片/", "旅行资料/照片/空相册/"] {
        assert!(archive.by_name(name).unwrap().is_dir(), "{name}");
    }
    let mut bytes = Vec::new();
    archive
        .by_name("旅行资料/照片/说明.txt")
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, nested_payload.as_bytes());
    assert_eq!(archive.by_name("旅行资料/空文件.txt").unwrap().size(), 0);
    assert!(
        !save.join("旅行资料").exists(),
        "received ZIPs are not automatically extracted"
    );
    assert_eq!(
        fs::read(folder.join("照片/说明.txt")).await.unwrap(),
        nested_payload.as_bytes()
    );
    assert_eq!(fs::read(folder.join("空文件.txt")).await.unwrap(), b"");
    assert!(folder.join("照片/空相册").is_dir());
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 2);
    assert!(!root.0.join("旅行资料.zip").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn folder_preparation_failure_never_offers_files_to_receiver() {
    let root = TestDir::new();
    let (sender, receiver) = pair(&root).await;
    let folder = root.0.join("带链接的资料");
    fs::create_dir(&folder).await.unwrap();
    let source = folder.join("原文件.txt");
    fs::write(&source, b"source content").await.unwrap();
    std::os::unix::fs::symlink(&source, folder.join("链接.txt")).unwrap();

    // Selection only inspects the folder itself; traversal happens after send.
    let id = send_path(&sender, &receiver, &folder).await;
    let failed = wait(&sender, |t| t.id == id && !active(&t.status)).await;
    assert_eq!(failed.status, "failed");
    assert!(failed
        .error
        .as_deref()
        .is_some_and(|error| !error.is_empty()));
    assert_eq!(failed.transferred_bytes, 0);
    assert!(receiver.snapshot().transfers.is_empty());
    assert!(!Path::new(&receiver.snapshot().save_dir).exists());
    assert_eq!(fs::read(&source).await.unwrap(), b"source content");
    assert_eq!(std::fs::read_link(folder.join("链接.txt")).unwrap(), source);
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 2);
    assert!(!root.0.join("带链接的资料.zip").exists());
}

#[tokio::test]
async fn immediately_cancelling_folder_preparation_never_starts_a_network_transfer() {
    let root = TestDir::new();
    let (sender, receiver) = pair(&root).await;
    let folder = root.0.join("取消发送");
    fs::create_dir(&folder).await.unwrap();
    fs::write(folder.join("保留.txt"), b"keep this file")
        .await
        .unwrap();

    let id = send_path(&sender, &receiver, &folder).await;
    assert_eq!(sender.snapshot().transfers[0].status, "preparing");
    sender.cancel(&id).unwrap();
    let cancelled = wait(&sender, |t| t.id == id && !active(&t.status)).await;
    assert_eq!(cancelled.status, "cancelled");
    assert_eq!(cancelled.transferred_bytes, 0);
    assert!(receiver.snapshot().transfers.is_empty());
    assert!(!Path::new(&receiver.snapshot().save_dir).exists());
    assert_eq!(
        fs::read(folder.join("保留.txt")).await.unwrap(),
        b"keep this file"
    );
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1);
    assert!(!root.0.join("取消发送.zip").exists());
}

#[tokio::test]
async fn rejecting_a_folder_transfer_removes_its_prepared_archive() {
    let root = TestDir::new();
    let (sender, receiver) = pair(&root).await;
    let folder = root.0.join("拒绝接收");
    fs::create_dir(&folder).await.unwrap();
    fs::write(folder.join("原文件.txt"), b"leave source intact")
        .await
        .unwrap();
    let item = sender
        .inspect(vec![folder.to_str().unwrap().into()])
        .await
        .unwrap()
        .remove(0);
    let mut file = SendFile {
        source: SendSource::Directory(item.path.clone().unwrap()),
        item,
    };
    let (_cancellation, cancelled) = watch::channel(false);
    file.prepare(&cancelled).await.unwrap();
    let archive_path = match &file.source {
        SendSource::Archive(archive) => archive.path().to_owned(),
        _ => panic!("folder preparation must produce an owned archive"),
    };
    assert!(archive_path.is_file());

    let id = sender
        .start_send(
            format!("127.0.0.1:{}", receiver.snapshot().device.port),
            vec![file],
            None,
        )
        .unwrap();
    let incoming = wait(&receiver, |t| t.status == "waiting").await;
    receiver.respond(&incoming.id, false, false).unwrap();
    assert_eq!(
        wait(&sender, |t| t.id == id && !active(&t.status))
            .await
            .status,
        "rejected"
    );
    assert!(
        !archive_path.exists(),
        "sender must release the ZIP before reporting completion"
    );
    assert!(!Path::new(&receiver.snapshot().save_dir).exists());
    assert_eq!(
        fs::read(folder.join("原文件.txt")).await.unwrap(),
        b"leave source intact"
    );
}

#[tokio::test]
async fn text_transfer_preserves_unicode_whitespace_and_multiline_bytes() {
    let root = TestDir::new();
    let (sender, receiver) = pair(&root).await;
    let text = "  你好，世界 🌏\r\n\t第二行：keep spaces  \n\n".repeat(3000);
    assert!(text.len() > CHUNK);
    let id = sender
        .send_text(
            format!("127.0.0.1:{}", receiver.snapshot().device.port),
            text.clone(),
            Some("活动文案".into()),
            None,
        )
        .await
        .unwrap();
    let outgoing = wait(&sender, |t| t.status == "waiting").await;
    let incoming = wait(&receiver, |t| t.status == "waiting").await;
    assert_eq!(outgoing.verification_code, incoming.verification_code);
    receiver.respond(&incoming.id, true, false).unwrap();
    sleep(Duration::from_millis(100)).await;
    assert_eq!(sender.snapshot().transfers[0].transferred_bytes, 0);
    assert!(!Path::new(&receiver.snapshot().save_dir).exists());
    sender.respond(&id, true, false).unwrap();
    let sent = wait(&sender, |t| !active(&t.status)).await;
    let received = wait(&receiver, |t| !active(&t.status)).await;
    for transfer in [sent, received] {
        assert_eq!(transfer.status, "completed", "{:?}", transfer.error);
        assert_eq!(transfer.files.len(), 1);
        assert_eq!(transfer.files[0].name, "活动文案.txt");
        assert_eq!(transfer.files[0].size, text.len() as u64);
        assert!(transfer.files[0].path.is_none());
        assert_eq!(transfer.total_bytes, text.len() as u64);
        assert_eq!(transfer.transferred_bytes, text.len() as u64);
    }
    assert!(!Path::new(&sender.snapshot().save_dir).exists());
    let save_dir = PathBuf::from(receiver.snapshot().save_dir);
    assert_eq!(
        fs::read(save_dir.join("活动文案.txt")).await.unwrap(),
        text.as_bytes()
    );
    assert_eq!(std::fs::read_dir(save_dir).unwrap().count(), 1);
}

#[tokio::test]
async fn text_transfer_accepts_one_mib_and_generates_default_filename() {
    let root = TestDir::new();
    let (sender, receiver) = pair(&root).await;
    let text = "好".repeat(MAX_TEXT_BYTES / 3) + &"a".repeat(MAX_TEXT_BYTES % 3);
    assert_eq!(text.len(), MAX_TEXT_BYTES);
    let id = sender
        .send_text(
            format!("127.0.0.1:{}", receiver.snapshot().device.port),
            text.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    wait(&sender, |t| t.status == "waiting").await;
    let incoming = wait(&receiver, |t| t.status == "waiting").await;
    let name = &incoming.files[0].name;
    assert!(name
        .strip_prefix("文案-")
        .and_then(|s| s.strip_suffix(".txt"))
        .and_then(|s| s.parse::<u64>().ok())
        .is_some());
    validate_name(name).unwrap();
    sender.respond(&id, true, false).unwrap();
    receiver.respond(&incoming.id, true, false).unwrap();
    for transfer in [
        wait(&sender, |t| !active(&t.status)).await,
        wait(&receiver, |t| !active(&t.status)).await,
    ] {
        assert_eq!(transfer.status, "completed", "{:?}", transfer.error);
        assert_eq!(transfer.files[0].name, *name);
        assert_eq!(transfer.transferred_bytes, MAX_TEXT_BYTES as u64);
    }
    assert_eq!(
        fs::read(PathBuf::from(receiver.snapshot().save_dir).join(name))
            .await
            .unwrap(),
        text.as_bytes()
    );
}

#[tokio::test]
async fn invalid_text_never_starts_a_transfer() {
    let root = TestDir::new();
    let engine = Engine::new(root.0.join("sender"), None);
    for text in ["", " \r\n\t", "\u{3000}\u{2003}"] {
        assert!(engine
            .send_text("127.0.0.1:53318".into(), text.into(), None, None)
            .await
            .unwrap_err()
            .contains("请输入"));
    }
    // The limit counts UTF-8 bytes, not characters.
    let oversized = "好".repeat(MAX_TEXT_BYTES / 3 + 1);
    assert!(oversized.chars().count() < MAX_TEXT_BYTES);
    assert!(engine
        .send_text("127.0.0.1:53318".into(), oversized, None, None)
        .await
        .unwrap_err()
        .contains("1 MB"));
    for name in [
        "../文案",
        "folder/文案",
        "folder\\文案",
        "CON",
        "NUL.txt",
        "文案.",
    ] {
        assert!(engine
            .send_text(
                "127.0.0.1:53318".into(),
                "内容".into(),
                Some(name.into()),
                None,
            )
            .await
            .unwrap_err()
            .contains("文件名"));
    }
    assert!(engine
        .send_text("".into(), "内容".into(), None, None)
        .await
        .is_err());
    assert!(engine.snapshot().transfers.is_empty());
    assert!(!Path::new(&engine.snapshot().save_dir).exists());
}

#[test]
fn text_filenames_keep_existing_extension_and_validate_final_length() {
    for (input, expected) in [
        ("文案", "文案.txt"),
        ("文案.txt", "文案.txt"),
        ("文案.TxT", "文案.TxT"),
        (" 文案 ", "文案.txt"),
        ("文案.md", "文案.md.txt"),
    ] {
        assert_eq!(text_file_name(Some(input)).unwrap(), expected);
    }
    for input in [None, Some(""), Some("   ")] {
        let name = text_file_name(input).unwrap();
        assert!(name.starts_with("文案-"));
        assert!(name.ends_with(".txt"));
        validate_name(&name).unwrap();
    }
    assert_eq!(text_file_name(Some(&"a".repeat(196))).unwrap().len(), 200);
    assert!(text_file_name(Some(&"a".repeat(197))).is_err());
}
