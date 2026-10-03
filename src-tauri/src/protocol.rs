//! FileHop v2: Noise XX authenticates persistent device keys. New peers still
//! require users to compare the transcript-derived code before trusting a key;
//! device discovery remains untrusted. There is no fallback to v1's Noise NN.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{tcp::OwnedWriteHalf, TcpStream},
    sync::mpsc,
    task::JoinHandle,
    time::timeout,
};

pub const CHUNK: usize = 60 * 1024;
pub const VERSION: u8 = 2;
const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const PROLOGUE: &[u8] = b"FileHop LAN v2";
const MAX_FRAME: usize = 65_535;
pub const MAX_CONTROL: usize = 48 * 1024;
pub const CANCELLED: &str = "__cancelled__";
pub const REJECTED: &str = "__rejected__";
pub type Result<T> = std::result::Result<T, String>;

/// Generate once, then persist privately so trust survives app restarts.
pub fn generate_identity() -> Result<Vec<u8>> {
    snow::Builder::new(NOISE_PATTERN.parse().map_err(|e| format!("{e}"))?)
        .generate_keypair()
        .map(|keypair| keypair.private)
        .map_err(|e| format!("无法生成设备身份：{e}"))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfferedFile {
    pub name: String,
    pub size: u64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Control {
    Offer {
        version: u8,
        name: String,
        files: Vec<OfferedFile>,
    },
    Hello {
        name: String,
    },
    Ready,
    Reject,
    Cancel,
    FileEnd {
        sha256: String,
    },
    Complete,
    Ack,
    Error {
        message: String,
    },
}

async fn read_frame<R: AsyncReadExt + Unpin>(read: &mut R, limit: usize) -> Result<Vec<u8>> {
    let length = read
        .read_u32()
        .await
        .map_err(|e| format!("连接已中断：{e}"))? as usize;
    if length == 0 || length > limit {
        return Err("对方发送了无效长度的数据帧".into());
    }
    let mut data = vec![0; length];
    read.read_exact(&mut data)
        .await
        .map_err(|e| format!("读取数据失败：{e}"))?;
    Ok(data)
}
async fn write_frame<W: AsyncWriteExt + Unpin>(write: &mut W, data: &[u8]) -> Result<()> {
    write
        .write_u32(data.len() as u32)
        .await
        .map_err(|e| e.to_string())?;
    write
        .write_all(data)
        .await
        .map_err(|e| format!("发送数据失败：{e}"))
}

pub struct Secure {
    write: OwnedWriteHalf,
    noise: Arc<Mutex<snow::TransportState>>,
    incoming: mpsc::Receiver<Result<Vec<u8>>>,
    reader: JoinHandle<()>,
    pub code: String,
    /// Authenticated static public key, available only after the full handshake.
    /// Persist trust against this value, never the discovered device name or IP.
    pub peer_key: String,
}
impl Drop for Secure {
    fn drop(&mut self) {
        self.reader.abort();
    }
}
impl Secure {
    pub async fn handshake(
        mut stream: TcpStream,
        initiator: bool,
        private_key: &[u8],
    ) -> Result<Self> {
        if private_key.len() != 32 {
            return Err("本机设备身份无效，无法建立安全连接".into());
        }
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        timeout(Duration::from_secs(10), async {
            let builder = snow::Builder::new(NOISE_PATTERN.parse().map_err(|e| format!("{e}"))?)
                .prologue(PROLOGUE)
                .local_private_key(private_key);
            let mut handshake = if initiator {
                builder.build_initiator()
            } else {
                builder.build_responder()
            }
            .map_err(|e| e.to_string())?;
            let mut buffer = vec![0; 1024];
            // XX has three messages. In particular, a responder must authenticate
            // the initiator's static key in the third before consulting trust.
            while !handshake.is_handshake_finished() {
                if handshake.is_my_turn() {
                    let n = handshake
                        .write_message(&[], &mut buffer)
                        .map_err(|e| e.to_string())?;
                    write_frame(&mut stream, &buffer[..n]).await?;
                } else {
                    let message = read_frame(&mut stream, 1024).await?;
                    let payload_length = handshake
                        .read_message(&message, &mut buffer)
                        .map_err(|e| e.to_string())?;
                    if payload_length != 0 {
                        return Err("握手包含不支持的附加数据".into());
                    }
                }
            }
            let peer_key: String = handshake
                .get_remote_static()
                .ok_or("对方未提供设备身份")?
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            // Hash the complete transcript with a domain separator before truncation.
            let digest = Sha256::digest(
                [
                    b"FileHop verification v2".as_slice(),
                    handshake.get_handshake_hash(),
                ]
                .concat(),
            );
            // 128-bit fingerprint; a short decimal truncation is not an
            // authenticated SAS protocol and would permit practical grinding.
            let code: String = digest[..16]
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect();
            let noise = Arc::new(Mutex::new(
                handshake.into_transport_mode().map_err(|e| e.to_string())?,
            ));
            let (mut read, write) = stream.into_split();
            let (tx, incoming) = mpsc::channel(2);
            let reader_noise = noise.clone();
            let reader = tokio::spawn(async move {
                loop {
                    let result = async {
                        let frame = read_frame(&mut read, MAX_FRAME).await?;
                        let mut plain = vec![0; MAX_FRAME];
                        let n = reader_noise
                            .lock()
                            .unwrap()
                            .read_message(&frame, &mut plain)
                            .map_err(|e| format!("数据解密失败：{e}"))?;
                        plain.truncate(n);
                        if plain.is_empty() {
                            return Err("收到空的数据帧".into());
                        }
                        Ok(plain)
                    }
                    .await;
                    let failed = result.is_err();
                    if tx.send(result).await.is_err() || failed {
                        break;
                    }
                }
            });
            Ok(Self {
                write,
                noise,
                incoming,
                reader,
                code,
                peer_key,
            })
        })
        .await
        .map_err(|_| "安全握手超时，请检查网络并将两台设备都更新到最新版本".to_string())?
        .map_err(|error: String| {
            format!("安全握手失败，请确认两台设备都已更新到支持长期信任的版本：{error}")
        })
    }
    async fn send(&mut self, data: &[u8]) -> Result<()> {
        if data.len() > MAX_FRAME - 16 {
            return Err("数据帧过大".into());
        }
        let mut frame = vec![0; data.len() + 16];
        let n = self
            .noise
            .lock()
            .unwrap()
            .write_message(data, &mut frame)
            .map_err(|e| e.to_string())?;
        timeout(
            Duration::from_secs(45),
            write_frame(&mut self.write, &frame[..n]),
        )
        .await
        .map_err(|_| "发送超时，请检查网络".to_string())?
    }
    pub async fn control(&mut self, control: &Control) -> Result<()> {
        let mut bytes = vec![0];
        bytes.extend(serde_json::to_vec(control).map_err(|e| e.to_string())?);
        if bytes.len() > MAX_CONTROL {
            return Err("文件信息过多，请减少一次发送的文件数量".into());
        }
        self.send(&bytes).await
    }
    pub async fn data(&mut self, data: &[u8]) -> Result<()> {
        if data.len() > CHUNK {
            return Err("文件分块过大".into());
        }
        let mut bytes = Vec::with_capacity(data.len() + 1);
        bytes.push(1);
        bytes.extend(data);
        let mut frame = vec![0; bytes.len() + 16];
        let n = self
            .noise
            .lock()
            .unwrap()
            .write_message(&bytes, &mut frame)
            .map_err(|e| e.to_string())?;
        // Listen for rejection/cancellation while a large file is being written.
        // A stopped write is never resumed; the whole connection is then closed.
        tokio::select! {
            biased;
            packet = self.incoming.recv() => {
                checked_packet(packet.ok_or("连接已关闭")??)?;
                Err("传输期间收到意外的对方消息".into())
            }
            result = timeout(Duration::from_secs(45), write_frame(&mut self.write, &frame[..n])) => result.map_err(|_| "发送超时，请检查网络".to_string())?,
        }
    }
    pub async fn packet(&mut self, seconds: u64) -> Result<Vec<u8>> {
        let packet = timeout(Duration::from_secs(seconds), self.incoming.recv())
            .await
            .map_err(|_| "等待对方超时".to_string())?
            .ok_or_else(|| "连接已关闭".to_string())??;
        checked_packet(packet)
    }
    pub async fn receive_control(&mut self, seconds: u64) -> Result<Control> {
        decode(&self.packet(seconds).await?)
    }
}
pub fn decode(packet: &[u8]) -> Result<Control> {
    if packet.first() != Some(&0) || packet.len() > MAX_CONTROL {
        return Err("收到无效的控制消息".into());
    }
    serde_json::from_slice(&packet[1..]).map_err(|_| "收到无法识别的控制消息".into())
}

fn checked_packet(packet: Vec<u8>) -> Result<Vec<u8>> {
    if packet.first() == Some(&0) {
        match decode(&packet)? {
            Control::Cancel => return Err(CANCELLED.into()),
            Control::Reject => return Err(REJECTED.into()),
            Control::Error { message } => {
                return Err(format!(
                    "对方报告：{}",
                    message.chars().take(300).collect::<String>()
                ))
            }
            _ => {}
        }
    } else if packet.first() != Some(&1) || packet.len() > CHUNK + 1 {
        return Err("收到无效的数据帧".into());
    }
    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{io::duplex, net::TcpListener};

    fn identity() -> snow::Keypair {
        snow::Builder::new(NOISE_PATTERN.parse().unwrap())
            .generate_keypair()
            .unwrap()
    }

    async fn secure_pair(initiator_key: &[u8], responder_key: &[u8]) -> (Secure, Secure) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let connect = async {
            let stream = TcpStream::connect(address).await.unwrap();
            Secure::handshake(stream, true, initiator_key)
                .await
                .unwrap()
        };
        let accept = async {
            let (stream, _) = listener.accept().await.unwrap();
            Secure::handshake(stream, false, responder_key)
                .await
                .unwrap()
        };
        tokio::join!(connect, accept)
    }

    #[tokio::test]
    async fn authenticated_identities_persist_while_session_codes_change() {
        let alice = identity();
        let bob = identity();
        let (mut first_alice, mut first_bob) = secure_pair(&alice.private, &bob.private).await;
        let (second_alice, second_bob) = secure_pair(&alice.private, &bob.private).await;
        let hex = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        assert_eq!(first_alice.peer_key, hex(&bob.public));
        assert_eq!(first_bob.peer_key, hex(&alice.public));
        assert_eq!(first_alice.peer_key.len(), 64);
        assert_eq!(first_alice.peer_key, second_alice.peer_key);
        assert_eq!(first_bob.peer_key, second_bob.peer_key);
        assert_eq!(first_alice.code, first_bob.code);
        assert_eq!(second_alice.code, second_bob.code);
        assert_ne!(first_alice.code, second_alice.code);

        first_alice.control(&Control::Ready).await.unwrap();
        assert!(matches!(
            first_bob.receive_control(2).await.unwrap(),
            Control::Ready
        ));
        first_bob.control(&Control::Ack).await.unwrap();
        assert!(matches!(
            first_alice.receive_control(2).await.unwrap(),
            Control::Ack
        ));

        // Changing sender/receiver roles keeps the same device identity.
        let (reverse_bob, reverse_alice) = secure_pair(&bob.private, &alice.private).await;
        assert_eq!(reverse_bob.peer_key, first_bob.peer_key);
        assert_eq!(reverse_alice.peer_key, first_alice.peer_key);

        // A different private key cannot present the previously trusted identity.
        let (replacement_alice, _) =
            secure_pair(&alice.private, &generate_identity().unwrap()).await;
        assert_ne!(replacement_alice.peer_key, first_alice.peer_key);
    }

    #[tokio::test]
    async fn frame_length_is_bounded_before_allocation() {
        for length in [0, 65_536, u32::MAX] {
            let (mut read, mut write) = duplex(8);
            write.write_u32(length).await.unwrap();
            assert!(read_frame(&mut read, MAX_FRAME).await.is_err());
        }
    }

    #[tokio::test]
    async fn unsolicited_handshake_payload_is_rejected() {
        // Check all three messages, including both encrypted identity messages.
        for payload_step in 1..=3 {
            let attacker_initiates = payload_step != 2;
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                assert!(Secure::handshake(
                    stream,
                    !attacker_initiates,
                    &generate_identity().unwrap()
                )
                .await
                .err()
                .unwrap()
                .contains("附加数据"));
            });
            let mut stream = TcpStream::connect(address).await.unwrap();
            let private = generate_identity().unwrap();
            let builder = snow::Builder::new(NOISE_PATTERN.parse().unwrap())
                .prologue(PROLOGUE)
                .local_private_key(&private);
            let mut noise = if attacker_initiates {
                builder.build_initiator()
            } else {
                builder.build_responder()
            }
            .unwrap();
            let mut bytes = [0; 1024];
            for step in 1..=payload_step {
                if noise.is_my_turn() {
                    let payload: &[u8] = if step == payload_step {
                        b"unexpected"
                    } else {
                        &[]
                    };
                    let count = noise.write_message(payload, &mut bytes).unwrap();
                    write_frame(&mut stream, &bytes[..count]).await.unwrap();
                } else {
                    let message = read_frame(&mut stream, 1024).await.unwrap();
                    noise.read_message(&message, &mut bytes).unwrap();
                }
            }
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn legacy_handshake_is_rejected_with_upgrade_guidance_in_both_roles() {
        for legacy_initiates in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let error =
                    Secure::handshake(stream, !legacy_initiates, &generate_identity().unwrap())
                        .await
                        .err()
                        .unwrap();
                assert!(error.contains("两台设备都已更新"), "{error}");
            });
            let mut stream = TcpStream::connect(address).await.unwrap();
            let builder = snow::Builder::new("Noise_NN_25519_ChaChaPoly_BLAKE2s".parse().unwrap())
                .prologue(b"FileHop LAN v1");
            let mut legacy = if legacy_initiates {
                builder.build_initiator()
            } else {
                builder.build_responder()
            }
            .unwrap();
            let mut bytes = [0; 1024];
            while !legacy.is_handshake_finished() {
                if legacy.is_my_turn() {
                    let count = legacy.write_message(&[], &mut bytes).unwrap();
                    write_frame(&mut stream, &bytes[..count]).await.unwrap();
                } else {
                    let message = read_frame(&mut stream, 1024).await.unwrap();
                    if legacy.read_message(&message, &mut bytes).is_err() {
                        break;
                    }
                }
            }
            drop(stream);
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn ciphertext_tampering_is_detected() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut secure = Secure::handshake(stream, false, &generate_identity().unwrap())
                .await
                .unwrap();
            assert!(secure.packet(2).await.unwrap_err().contains("解密失败"));
        });
        let stream = TcpStream::connect(address).await.unwrap();
        let mut secure = Secure::handshake(stream, true, &generate_identity().unwrap())
            .await
            .unwrap();
        let mut frame = [0; 100];
        let count = secure
            .noise
            .lock()
            .unwrap()
            .write_message(b"\x01private file content", &mut frame)
            .unwrap();
        frame[count - 1] ^= 1;
        write_frame(&mut secure.write, &frame[..count])
            .await
            .unwrap();
        task.await.unwrap();
    }
}
