use anyhow::Result;
use irc::proto::{Command, Message, Prefix};
use std::collections::HashMap;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::TlsConnector;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::config::Role;
use crate::persistence::Identity;
use crate::state::AppState;
use crate::tls;

pub enum IrcEvent {
    UserJoined { nick: String, role: Role },
    UserLeft(String),
    WebRtcSignal { from: String, payload: String },
    ChatMessage { channel: String, from: String, text: String },
    ModAction { from: String, action: String, target: String },
    /// Server requires at least `bits` leading zero bits in nick hash.
    /// Fired on initial connect and whenever a mod changes the difficulty.
    PowRequirementChanged { bits: u8 },
    /// Our VOIRC_HELLO was rejected because our nick's PoW is too weak.
    /// The UI should prompt the user to re-mine their nick.
    PowTooWeak { required_bits: u8 },
}

struct FragmentBuffer {
    parts: HashMap<usize, String>,
    total: usize,
    last_update: std::time::Instant,
}

pub enum MaybeTlsStream {
    Plain(TcpStream),
    Tls(tokio_rustls::client::TlsStream<TcpStream>),
}

impl AsyncRead for MaybeTlsStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            MaybeTlsStream::Tls(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MaybeTlsStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            MaybeTlsStream::Tls(s) => Pin::new(s).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => Pin::new(s).poll_flush(cx),
            MaybeTlsStream::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTlsStream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            MaybeTlsStream::Tls(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

pub struct IrcClient {
    tx: mpsc::UnboundedSender<String>,
    state: Arc<AppState>,
    event_tx: mpsc::UnboundedSender<IrcEvent>,
    fragments: Mutex<HashMap<String, FragmentBuffer>>,
    nickname: String,
}

impl IrcClient {
    pub async fn connect(
        connection_string: String,
        nickname: String,
        channel: String,
        state: Arc<AppState>,
        cert_fingerprint: Option<String>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<()>, mpsc::UnboundedReceiver<IrcEvent>)> {
        let (server, port) = if let Some((h, p)) = connection_string.split_once(':') {
            (h.to_string(), p.parse::<u16>().ok())
        } else {
            (connection_string, None)
        };

        let addr = format!("{}:{}", server, port.unwrap_or(6667));
        info!("Connecting to {}...", addr);
        let tcp_stream = TcpStream::connect(addr).await?;

        let stream = if let Some(fp) = cert_fingerprint {
            info!("Upgrading to TLS (Pinned Fingerprint: {}...)", &fp[..8.min(fp.len())]);
            let config = tls::client_config_pinned(&fp);
            let connector = TlsConnector::from(config);
            let domain = rustls::pki_types::ServerName::try_from("voirc.local")
                .unwrap_or_else(|_| rustls::pki_types::ServerName::try_from("localhost").unwrap());
            let tls_stream = connector.connect(domain, tcp_stream).await?;
            MaybeTlsStream::Tls(tls_stream)
        } else {
            MaybeTlsStream::Plain(tcp_stream)
        };

        let (reader, mut writer) = tokio::io::split(stream);
        let mut buf_reader = BufReader::new(reader);

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (done_tx, done_rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if writer.write_all(msg.as_bytes()).await.is_err() { break; }
                if writer.write_all(b"\r\n").await.is_err() { break; }
            }
        });

        let client = Self {
            tx: out_tx.clone(),
            state,
            event_tx,
            fragments: Mutex::new(HashMap::new()),
            nickname: nickname.clone(),
        };

        client.send_raw(format!("NICK {}", nickname))?;
        client.send_raw(format!("USER {} 0 * :Voirc User", nickname))?;

        let client_clone = Arc::new(client);
        let identity = client_clone.state.identity.clone();

        let handler_ctx = HandlerContext {
            tx: out_tx.clone(),
            event_tx: client_clone.event_tx.clone(),
            state: client_clone.state.clone(),
            fragments: Arc::new(Mutex::new(HashMap::new())),
            nickname: nickname.clone(),
            identity: identity.clone(),
        };

        // Send VOIRC_HELLO after a short delay so the server has processed
        // NICK/USER.  The server will have already sent VOIRC_POW_REQUIRED in
        // the welcome NOTICE; the handler below checks it before accepting.
        if let Some(ref id) = identity {
            if let Ok(hello) = build_hello_message(&nickname, id) {
                let tx_hello = out_tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    let _ = tx_hello.send(hello);
                });
            }
        }

        tokio::spawn(async move {
            let mut line = String::new();
            while let Ok(n) = buf_reader.read_line(&mut line).await {
                if n == 0 { break; }
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    if let Ok(msg) = Message::from_str(trimmed) {
                        if let Command::PING(ref s1, ref s2) = msg.command {
                            let _ = handler_ctx.send_raw(format!("PONG {} {:?}", s1, s2));
                        } else {
                            let _ = handler_ctx.handle_message(msg).await;
                        }
                    }
                }
                line.clear();
            }
            drop(done_tx);
        });

        // Join channel after hello is processed
        let tx_join = out_tx.clone();
        let ch = channel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let _ = tx_join.send(format!("JOIN {}", ch));
        });

        Ok((
            Arc::try_unwrap(client_clone).unwrap_or_else(|c| IrcClient {
                tx: c.tx.clone(),
                state: c.state.clone(),
                event_tx: c.event_tx.clone(),
                fragments: Mutex::new(HashMap::new()),
                nickname: c.nickname.clone(),
            }),
            done_rx,
            event_rx
        ))
    }

    pub fn send_raw(&self, msg: String) -> Result<()> {
        self.tx.send(msg)?;
        Ok(())
    }

    pub fn announce_role(&self, channel: &str, role: Role) -> Result<()> {
        self.send_raw(format!("PRIVMSG {} :VOIRC_ROLE:{}", channel, role.as_str()))
    }

    pub fn send_mod_action(&self, channel: &str, action: &str, target: &str) -> Result<()> {
        self.send_raw(format!("PRIVMSG {} :VOIRC_MOD:{}:{}", channel, action, target))
    }

    /// Set the server-wide PoW difficulty.  Only works if our role permits it;
    /// the server does its own authentication check too.
    pub fn send_pow_set(&self, bits: u8) -> Result<()> {
        self.send_raw(format!("PRIVMSG voirc :VOIRC_POW_SET:{}", bits))
    }

    pub fn send_webrtc_signal(&self, target: &str, payload: &str) -> Result<()> {
        let chunk_size = 400;
        let total_len = payload.len();
        let total_chunks = (total_len + chunk_size - 1) / chunk_size;
        let msg_id = Uuid::new_v4().to_string()[..8].to_string();

        for (i, chunk) in payload.as_bytes().chunks(chunk_size).enumerate() {
            let seq = i + 1;
            let chunk_str = String::from_utf8_lossy(chunk);
            self.send_raw(format!(
                "PRIVMSG {} :WRTC:[{}/{}|{}]{}",
                target, seq, total_chunks, msg_id, chunk_str
            ))?;
        }
        Ok(())
    }

    pub fn send_message(&self, channel: &str, text: &str) -> Result<()> {
        self.send_raw(format!("PRIVMSG {} :{}", channel, text))
    }

    pub fn join_channel(&self, channel: &str) -> Result<()> {
        self.send_raw(format!("JOIN {}", channel))
    }

    pub fn part_channel(&self, channel: &str) -> Result<()> {
        self.send_raw(format!("PART {}", channel))
    }

    pub async fn run(&self, mut stream: mpsc::UnboundedReceiver<()>) -> Result<()> {
        let _ = stream.recv().await;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// VOIRC_HELLO construction
// ─────────────────────────────────────────────────────────────────────────────

fn build_hello_message(nick: &str, identity: &Identity) -> Result<String> {
    use ed25519_dalek::Signer;
    let signature = identity.signing_key.sign(nick.as_bytes());
    let sig_hex = hex::encode(signature.to_bytes());
    Ok(format!(
        "PRIVMSG voirc :VOIRC_HELLO:{}:{}:{}",
        nick, identity.pubkey_hex, sig_hex
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// Message handler
// ─────────────────────────────────────────────────────────────────────────────

struct HandlerContext {
    tx: mpsc::UnboundedSender<String>,
    event_tx: mpsc::UnboundedSender<IrcEvent>,
    state: Arc<AppState>,
    fragments: Arc<Mutex<HashMap<String, FragmentBuffer>>>,
    nickname: String,
    identity: Option<Identity>,
}

impl HandlerContext {
    fn send_raw(&self, msg: String) -> Result<()> {
        let _ = self.tx.send(msg);
        Ok(())
    }

    async fn handle_message(&self, message: Message) -> Result<()> {
        {
            let mut frags = self.fragments.lock().unwrap();
            frags.retain(|_, v| v.last_update.elapsed().as_secs() < 60);
        }

        match message.command {
            Command::NOTICE(ref _target, ref text) => {
                // Parse server NOTICEs that carry structured data.
                self.handle_notice(text).await?;
            }
            Command::JOIN(ref _channel, _, _) => {
                if let Some(Prefix::Nickname(ref nick, _, _)) = message.prefix {
                    if nick != &self.nickname {
                        let _ = self.event_tx.send(IrcEvent::UserJoined {
                            nick: nick.clone(),
                            role: Role::Peer,
                        });
                        // Re-announce our hello so the new peer learns our pubkey
                        if let Some(ref id) = self.identity {
                            if let Ok(hello) = build_hello_message(&self.nickname, id) {
                                let _ = self.tx.send(hello);
                            }
                        }
                    }
                }
            }
            Command::Response(irc::proto::Response::RPL_NAMREPLY, ref args) => {
                if let Some(names) = args.last() {
                    for name in names.split_whitespace() {
                        let clean_name = name.trim_start_matches(|c| c == '@' || c == '+');
                        if clean_name != self.nickname && !clean_name.is_empty() {
                            let _ = self.event_tx.send(IrcEvent::UserJoined {
                                nick: clean_name.to_string(),
                                role: Role::Peer,
                            });
                        }
                    }
                }
            }
            Command::PART(_, _) | Command::QUIT(_) => {
                if let Some(Prefix::Nickname(ref nick, _, _)) = message.prefix {
                    let _ = self.event_tx.send(IrcEvent::UserLeft(nick.clone()));
                    self.state.remove_peer(nick).await;
                }
            }
            Command::PRIVMSG(ref target, ref text) => {
                if let Some(Prefix::Nickname(ref nick, _, _)) = message.prefix {
                    self.handle_privmsg(nick, target, text).await?;
                }
                // Server-broadcast PRIVMSG (from ":voirc") carry VOIRC_POW_REQUIRED
                // and VOIRC_PUBKEY after a mod changes difficulty.
                if let Some(Prefix::ServerName(_)) | None = message.prefix {
                    if text.starts_with("VOIRC_POW_REQUIRED:") {
                        if let Some(bits_str) = text.strip_prefix("VOIRC_POW_REQUIRED:") {
                            if let Ok(bits) = bits_str.trim().parse::<u8>() {
                                let _ = self.event_tx.send(IrcEvent::PowRequirementChanged { bits });
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_notice(&self, text: &str) -> Result<()> {
        // VOIRC_POW_REQUIRED:<bits>  — server tells us the current requirement on welcome
        if let Some(rest) = text.strip_prefix("VOIRC_POW_REQUIRED:") {
            if let Ok(bits) = rest.trim().parse::<u8>() {
                info!("Server requires {} PoW bits for nick registration", bits);
                let _ = self.event_tx.send(IrcEvent::PowRequirementChanged { bits });
            }
            return Ok(());
        }

        // HELLO_OK pow_bits:<N>  — our HELLO was accepted
        if text.starts_with("HELLO_OK") {
            info!("VOIRC_HELLO accepted: {}", text);
            return Ok(());
        }

        // HELLO_FAILED <reason>  — our HELLO was rejected
        if let Some(reason) = text.strip_prefix("HELLO_FAILED ") {
            if let Some(bits_str) = reason.strip_prefix("pow_too_weak:") {
                if let Ok(required_bits) = bits_str.trim().parse::<u8>() {
                    warn!(
                        "Nick '{}' PoW too weak — server requires {} bits",
                        self.nickname, required_bits
                    );
                    let _ = self.event_tx.send(IrcEvent::PowTooWeak { required_bits });
                }
            } else {
                warn!("VOIRC_HELLO rejected: {}", reason);
            }
            return Ok(());
        }

        Ok(())
    }

    async fn handle_privmsg(&self, nick: &str, target: &str, text: &str) -> Result<()> {
        // VOIRC_PUBKEY broadcast from server
        if let Some(rest) = text.strip_prefix("VOIRC_PUBKEY:") {
            let parts: Vec<&str> = rest.splitn(2, ':').collect();
            if parts.len() == 2 {
                let peer_nick = parts[0];
                let pubkey_hex = parts[1];
                // FIX: Added .to_string() to both arguments
                if self.state.register_peer_pubkey(peer_nick.to_string(), pubkey_hex.to_string()).await {
                    info!("Registered pubkey for {}: {}...", peer_nick, &pubkey_hex[..8.min(pubkey_hex.len())]);
                } else {
                    warn!("Pubkey conflict for {}", peer_nick);
                }
            }
            return Ok(());
        }

        // VOIRC_POW_REQUIRED broadcast (relayed through channel)
        if let Some(rest) = text.strip_prefix("VOIRC_POW_REQUIRED:") {
            if let Ok(bits) = rest.trim().parse::<u8>() {
                let _ = self.event_tx.send(IrcEvent::PowRequirementChanged { bits });
            }
            return Ok(());
        }

        // Peer-to-peer VOIRC_HELLO relay
        if let Some(rest) = text.strip_prefix("VOIRC_HELLO:") {
            let parts: Vec<&str> = rest.splitn(3, ':').collect();
            if parts.len() == 3 && verify_hello(parts[0], parts[1], parts[2]) {
                // FIX: Added .to_string() to both arguments
                let _ = self.state.register_peer_pubkey(parts[0].to_string(), parts[1].to_string()).await;
            }
            return Ok(());
        }

        // All privileged messages require a verified pubkey
        if text.starts_with("WRTC:") {
            if !self.is_verified(nick).await {
                warn!("Dropping WRTC from unverified peer {}", nick);
                return Ok(());
            }
            self.handle_fragment(nick, text)?;
            return Ok(());
        }

        if let Some(role_str) = text.strip_prefix("VOIRC_ROLE:") {
            if !self.is_verified(nick).await {
                warn!("Dropping VOIRC_ROLE from unverified peer {}", nick);
                return Ok(());
            }
            let role = Role::from_str(role_str.trim());
            self.state.set_peer_role(nick, role).await;
            return Ok(());
        }

        if let Some(rest) = text.strip_prefix("VOIRC_MOD:") {
            if !self.is_verified(nick).await {
                warn!("Dropping VOIRC_MOD from unverified peer {}", nick);
                return Ok(());
            }
            if let Some((action, target_nick)) = rest.split_once(':') {
                let _ = self.event_tx.send(IrcEvent::ModAction {
                    from: nick.to_string(),
                    action: action.to_string(),
                    target: target_nick.to_string(),
                });
            }
            return Ok(());
        }

        if let Some(rest) = text.strip_prefix("VOIRC_SYNC_REQ:") {
            if !self.is_verified(nick).await {
                warn!("Dropping VOIRC_SYNC_REQ from unverified peer {}", nick);
                return Ok(());
            }
            if let Ok(req) = serde_json::from_str::<crate::persistence::SyncRequest>(rest) {
                let messages = self.state.message_log.messages_since(&req.channel, req.since).await;
                let resp = crate::persistence::SyncResponse { channel: req.channel, messages };
                if let Ok(json) = serde_json::to_string(&resp) {
                    let _ = self.tx.send(format!("PRIVMSG {} :VOIRC_SYNC_RESP:{}\r\n", target, json));
                }
            }
            return Ok(());
        }

        if let Some(rest) = text.strip_prefix("VOIRC_SYNC_RESP:") {
            if !self.is_verified(nick).await {
                warn!("Dropping VOIRC_SYNC_RESP from unverified peer {}", nick);
                return Ok(());
            }
            if let Ok(resp) = serde_json::from_str::<crate::persistence::SyncResponse>(rest) {
                let trusted = self.state.trusted_keys_snapshot().await;
                let (accepted, rejected) = crate::persistence::process_sync_response(
                    &self.state.message_log, resp, &trusted,
                ).await;
                info!("Sync: {} accepted, {} rejected", accepted, rejected);
            }
            return Ok(());
        }

        if let Some(rest) = text.strip_prefix("SIGNED:") {
            if let Ok(msg) = serde_json::from_str::<crate::persistence::SignedMessage>(rest) {
                if let Some(kp) = self.state.pubkey_for_nick(nick).await {
                    if kp != msg.pubkey {
                        warn!("Dropping SIGNED from {}: pubkey mismatch", nick);
                        return Ok(());
                    }
                }
                let result = msg.verify(None);
                if result == crate::persistence::VerifyResult::InvalidSignature {
                    warn!("Dropping SIGNED with invalid sig from {}", nick);
                    return Ok(());
                }
                let display = if result.is_suspicious() {
                    format!("[?] {}", msg.content)
                } else {
                    msg.content.clone()
                };
                self.state.message_log.append(msg).await.ok();
                let _ = self.event_tx.send(IrcEvent::ChatMessage {
                    channel: target.to_string(),
                    from: nick.to_string(),
                    text: display,
                });
            }
            return Ok(());
        }

        let _ = self.event_tx.send(IrcEvent::ChatMessage {
            channel: target.to_string(),
            from: nick.to_string(),
            text: text.to_string(),
        });
        Ok(())
    }

    async fn is_verified(&self, nick: &str) -> bool {
        self.state.pubkey_for_nick(nick).await.is_some()
    }

    fn handle_fragment(&self, sender: &str, text: &str) -> Result<()> {
        let content = text.strip_prefix("WRTC:").unwrap_or("");
        let end_bracket = content.find(']').unwrap_or(0);
        if end_bracket == 0 || !content.starts_with('[') { return Ok(()); }

        let header = &content[1..end_bracket];
        let payload = &content[end_bracket + 1..];
        let parts: Vec<&str> = header.split('|').collect();
        if parts.len() != 2 { return Ok(()); }

        let counts: Vec<&str> = parts[0].split('/').collect();
        let msg_id = parts[1];
        let seq: usize = counts[0].parse().unwrap_or(0);
        let total: usize = counts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

        let key = format!("{}:{}", sender, msg_id);
        let mut fragments = self.fragments.lock().unwrap();
        let entry = fragments.entry(key.clone()).or_insert(FragmentBuffer {
            parts: HashMap::new(),
            total,
            last_update: std::time::Instant::now(),
        });
        entry.parts.insert(seq, payload.to_string());
        entry.last_update = std::time::Instant::now();

        if entry.parts.len() == entry.total {
            let mut full = String::new();
            for i in 1..=entry.total {
                if let Some(p) = entry.parts.get(&i) { full.push_str(p); }
            }
            fragments.remove(&key);
            let _ = self.event_tx.send(IrcEvent::WebRtcSignal {
                from: sender.to_string(),
                payload: full,
            });
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Standalone HELLO signature verifier
// ─────────────────────────────────────────────────────────────────────────────

fn verify_hello(nick: &str, pubkey_hex: &str, sig_hex: &str) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let pubkey_bytes = match hex::decode(pubkey_hex) {
        Ok(b) if b.len() == 32 => b, _ => return false,
    };
    let sig_bytes = match hex::decode(sig_hex) {
        Ok(b) if b.len() == 64 => b, _ => return false,
    };
    let pk_arr: [u8; 32] = match pubkey_bytes.try_into() { Ok(a) => a, Err(_) => return false };
    let verifying_key = match VerifyingKey::from_bytes(&pk_arr) { Ok(k) => k, Err(_) => return false };
    let sig_arr: [u8; 64] = match sig_bytes.try_into() { Ok(a) => a, Err(_) => return false };
    verifying_key.verify(nick.as_bytes(), &Signature::from_bytes(&sig_arr)).is_ok()
}
