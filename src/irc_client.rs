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
use tracing::{error, info};
use uuid::Uuid;

use crate::config::Role;
use crate::state::AppState;
use crate::tls;

pub enum IrcEvent {
    UserJoined { nick: String, role: Role },
    UserLeft(String),
    WebRtcSignal { from: String, payload: String },
    ChatMessage { channel: String, from: String, text: String },
    ModAction { from: String, action: String, target: String },
}

struct FragmentBuffer {
    parts: HashMap<usize, String>,
    total: usize,
    last_update: std::time::Instant,
}

// Custom stream that handles both Plain and TLS connections
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
    // We use a channel to send raw IRC command strings to the writer task
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

        // 1. Establish TCP Connection
        let addr = format!("{}:{}", server, port.unwrap_or(6667));
        info!("Connecting to {}...", addr);
        let tcp_stream = TcpStream::connect(addr).await?;

        // 2. Upgrade to TLS if fingerprint provided (Manual Pinning)
        let stream = if let Some(fp) = cert_fingerprint {
            info!("Upgrading to TLS (Pinned Fingerprint: {}...)", &fp[..8]);
            let config = tls::client_config_pinned(&fp);
            let connector = TlsConnector::from(config);
            // Verify against "voirc.local" or "localhost" as a placeholder since we use pinning
            let domain = rustls::pki_types::ServerName::try_from("voirc.local")
                .unwrap_or_else(|_| rustls::pki_types::ServerName::try_from("localhost").unwrap());
            let tls_stream = connector.connect(domain, tcp_stream).await?;
            MaybeTlsStream::Tls(tls_stream)
        } else {
            info!("Using Plaintext connection");
            MaybeTlsStream::Plain(tcp_stream)
        };

        // 3. Split stream into Read/Write
        let (reader, mut writer) = tokio::io::split(stream);
        let mut buf_reader = BufReader::new(reader);

        // 4. Setup channels
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        // This dummy receiver signals when the loop exits
        let (done_tx, done_rx) = mpsc::unbounded_channel();

        // 5. Spawn Writer Task
        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if writer.write_all(msg.as_bytes()).await.is_err() {
                    break;
                }
                if writer.write_all(b"\r\n").await.is_err() {
                    break;
                }
            }
        });

        // 6. Construct Self
        let client = Self {
            tx: out_tx.clone(),
            state,
            event_tx,
            fragments: Mutex::new(HashMap::new()),
            nickname: nickname.clone(),
        };

        // 7. Perform Login
        client.send_raw(format!("NICK {}", nickname))?;
        client.send_raw(format!("USER {} 0 * :Voirc User", nickname))?;

        // 8. Spawn Reader Loop (Manual IRC Protocol Handler)
        let client_clone = Arc::new(client);
        // We return a lightweight wrapper, but the heavy logic runs in the background task
        // Actually, we can't return `client_clone` easily because `connect` returns `Self`.
        // We will return a fresh `Self` struct and spawn the reader logic separately.
        
        // We need a way for the background task to call methods on "Client".
        // Since `IrcClient` is just a handle (channels + state), it's cheap to clone if we derive Clone or wrap.
        // But `Mutex` isn't Clone. Let's make `IrcClient` mostly Arc internals or just move the logic.
        // Simpler: We run the loop here in a spawn and use a local "Handler" struct.

        let loop_tx = out_tx.clone();
        let loop_event_tx = client_clone.event_tx.clone();
        let loop_state = client_clone.state.clone();
        let loop_fragments = Arc::new(Mutex::new(HashMap::new()));
        let loop_nick = nickname.clone();
        
        let handler_ctx = HandlerContext {
            tx: loop_tx,
            event_tx: loop_event_tx,
            state: loop_state,
            fragments: loop_fragments,
            nickname: loop_nick,
        };

        tokio::spawn(async move {
            let mut line = String::new();
            while let Ok(n) = buf_reader.read_line(&mut line).await {
                if n == 0 { break; } // EOF
                
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    if let Ok(msg) = Message::from_str(trimmed) {
                        // Handle PING automatically
                        if let Command::PING(ref s1, ref s2) = msg.command {
                            let _ = handler_ctx.send_raw(format!("PONG {} {:?}", s1, s2));
                        } else {
                            // Handle other messages
                            let _ = handler_ctx.handle_message(msg).await;
                        }
                    }
                }
                line.clear();
            }
            // Signal exit
            drop(done_tx);
        });

        // Send initial join
        client_clone.send_raw(format!("JOIN {}", channel))?;

        // Return the client handle. 
        // Note: The `run` method in gui.rs expects a `stream` to await.
        // We return `done_rx` as a dummy stream that completes when the connection dies.
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

    pub fn send_webrtc_signal(&self, target: &str, payload: &str) -> Result<()> {
        let chunk_size = 400;
        let total_len = payload.len();
        let total_chunks = (total_len + chunk_size - 1) / chunk_size;
        let msg_id = Uuid::new_v4().to_string()[..8].to_string();

        for (i, chunk) in payload.as_bytes().chunks(chunk_size).enumerate() {
            let seq = i + 1;
            let chunk_str = String::from_utf8_lossy(chunk);
            self.send_raw(format!("PRIVMSG {} :WRTC:[{}/{}|{}]{}", target, seq, total_chunks, msg_id, chunk_str))?;
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

    // This method is kept for compatibility with gui.rs structure
    // It awaits the connection "stream" (rx channel) until it closes.
    pub async fn run(&self, mut stream: mpsc::UnboundedReceiver<()>) -> Result<()> {
        let _ = stream.recv().await;
        Ok(())
    }
}

// Context for the background task to handle logic
struct HandlerContext {
    tx: mpsc::UnboundedSender<String>,
    event_tx: mpsc::UnboundedSender<IrcEvent>,
    state: Arc<AppState>,
    fragments: Arc<Mutex<HashMap<String, FragmentBuffer>>>,
    nickname: String,
}

impl HandlerContext {
    fn send_raw(&self, msg: String) -> Result<()> {
        let _ = self.tx.send(msg);
        Ok(())
    }

    async fn handle_message(&self, message: Message) -> Result<()> {
        // Cleanup old fragments
        {
            let mut frags = self.fragments.lock().unwrap();
            frags.retain(|_, v| v.last_update.elapsed().as_secs() < 60);
        }

        match message.command {
            Command::JOIN(ref _channel, _, _) => {
                if let Some(Prefix::Nickname(ref nick, _, _)) = message.prefix {
                    if nick != &self.nickname {
                        let _ = self.event_tx.send(IrcEvent::UserJoined {
                            nick: nick.clone(),
                            role: Role::Peer,
                        });
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
                    if text.starts_with("WRTC:") {
                        self.handle_fragment(nick, text)?;
                    } else if let Some(role_str) = text.strip_prefix("VOIRC_ROLE:") {
                        let role = crate::config::Role::from_str(role_str.trim());
                        self.state.set_peer_role(nick, role).await;
                        info!("Peer {} announced role: {:?}", nick, role);
                    } else if let Some(rest) = text.strip_prefix("VOIRC_MOD:") {
                        if let Some((action, target_nick)) = rest.split_once(':') {
                            let _ = self.event_tx.send(IrcEvent::ModAction {
                                from: nick.clone(),
                                action: action.to_string(),
                                target: target_nick.to_string(),
                            });
                        }
                    } else {
                        let _ = self.event_tx.send(IrcEvent::ChatMessage {
                            channel: target.clone(),
                            from: nick.clone(),
                            text: text.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
        Ok(())
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
        let total: usize = counts[1].parse().unwrap_or(0);

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
            let mut full_payload = String::new();
            for i in 1..=entry.total {
                if let Some(p) = entry.parts.get(&i) {
                    full_payload.push_str(p);
                }
            }
            fragments.remove(&key);
            let _ = self.event_tx.send(IrcEvent::WebRtcSignal {
                from: sender.to_string(),
                payload: full_payload,
            });
        }
        Ok(())
    }
}
