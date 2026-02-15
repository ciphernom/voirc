use anyhow::Result;
use irc::client::prelude::*;
use futures_util::stream::StreamExt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{error, info};
use uuid::Uuid;

use crate::config::Role;
use crate::state::AppState;

pub enum IrcEvent {
    UserJoined { nick: String, role: Role },
    UserLeft(String),
    WebRtcSignal { from: String, payload: String },
    ChatMessage { channel: String, from: String, text: String },
    /// Moderation action broadcast from a mod/host
    ModAction { from: String, action: String, target: String },
}

struct FragmentBuffer {
    parts: HashMap<usize, String>,
    total: usize,
    last_update: std::time::Instant,
}

pub struct IrcClient {
    sender: Sender,
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
    ) -> Result<(Self, irc::client::ClientStream, mpsc::UnboundedReceiver<IrcEvent>)> {
        let (server, port) = if let Some((h, p)) = connection_string.split_once(':') {
            (h.to_string(), p.parse::<u16>().ok())
        } else {
            (connection_string, None)
        };

        let config = Config {
            nickname: Some(nickname.clone()),
            server: Some(server),
            port,
            channels: vec![channel.clone()],
            use_tls: Some(false),
            ..Default::default()
        };

        let mut client = Client::from_config(config).await?;
        client.identify()?;
        client.send_join(&channel)?;

        let stream = client.stream()?;
        let sender = client.sender();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        info!("Connected to IRC server, joining {}", channel);

        Ok((
            Self {
                sender,
                state,
                event_tx,
                fragments: Mutex::new(HashMap::new()),
                nickname,
            },
            stream,
            event_rx,
        ))
    }

    /// Announce our role to the channel so peers know our topology position.
    pub fn announce_role(&self, channel: &str, role: Role) -> Result<()> {
        self.sender.send_privmsg(channel, &format!("VOIRC_ROLE:{}", role.as_str()))?;
        Ok(())
    }

    /// Send a moderation action to the channel.
    pub fn send_mod_action(&self, channel: &str, action: &str, target: &str) -> Result<()> {
        self.sender.send_privmsg(channel, &format!("VOIRC_MOD:{}:{}", action, target))?;
        Ok(())
    }

    pub async fn run(&self, mut stream: irc::client::ClientStream) -> Result<()> {
        while let Some(message) = stream.next().await.transpose()? {
            info!("RX: {:?}", message);
            if let Err(e) = self.handle_message(message).await {
                error!("Error handling IRC message: {}", e);
            }
        }
        Ok(())
    }

    async fn handle_message(&self, message: Message) -> Result<()> {
        {
            let mut frags = self.fragments.lock().unwrap();
            frags.retain(|_, v| v.last_update.elapsed().as_secs() < 60);
        }

        match message.command {
            Command::JOIN(ref _channel, _, _) => {
                if let Some(Prefix::Nickname(ref nick, _, _)) = message.prefix {
                    info!("Processing JOIN for {}", nick);
                    if nick != &self.nickname {
                        // Default to Peer role; they'll announce their real role shortly
                        self.event_tx.send(IrcEvent::UserJoined {
                            nick: nick.clone(),
                            role: Role::Peer,
                        })?;
                    }
                }
            }
            Command::Response(Response::RPL_NAMREPLY, ref args) => {
                info!("Processing NAMES list: {:?}", args);
                if let Some(names) = args.last() {
                    for name in names.split_whitespace() {
                        let clean_name = name.trim_start_matches(|c| c == '@' || c == '+');
                        if clean_name != self.nickname && !clean_name.is_empty() {
                            info!("Found existing peer: {}", clean_name);
                            self.event_tx.send(IrcEvent::UserJoined {
                                nick: clean_name.to_string(),
                                role: Role::Peer,
                            })?;
                        }
                    }
                }
            }
            Command::PART(_, _) | Command::QUIT(_) => {
                if let Some(Prefix::Nickname(ref nick, _, _)) = message.prefix {
                    self.event_tx.send(IrcEvent::UserLeft(nick.clone()))?;
                    self.state.remove_peer(nick).await;
                }
            }
            Command::PRIVMSG(ref target, ref text) => {
                if let Some(Prefix::Nickname(ref nick, _, _)) = message.prefix {
                    if text.starts_with("WRTC:") {
                        self.handle_fragment(nick, text)?;
                    } else if let Some(role_str) = text.strip_prefix("VOIRC_ROLE:") {
                        // Peer announcing their role
                        let role = Role::from_str(role_str.trim());
                        self.state.set_peer_role(nick, role).await;
                        info!("Peer {} announced role: {:?}", nick, role);
                    } else if let Some(rest) = text.strip_prefix("VOIRC_MOD:") {
                        // Moderation action from a mod/host
                        if let Some((action, target_nick)) = rest.split_once(':') {
                            self.event_tx.send(IrcEvent::ModAction {
                                from: nick.clone(),
                                action: action.to_string(),
                                target: target_nick.to_string(),
                            })?;
                        }
                    } else {
                        self.event_tx.send(IrcEvent::ChatMessage {
                            channel: target.clone(),
                            from: nick.clone(),
                            text: text.clone(),
                        })?;
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

            self.event_tx.send(IrcEvent::WebRtcSignal {
                from: sender.to_string(),
                payload: full_payload,
            })?;
        }

        Ok(())
    }

    pub fn send_webrtc_signal(&self, target: &str, payload: &str) -> Result<()> {
        let chunk_size = 400;
        let total_len = payload.len();
        let total_chunks = (total_len + chunk_size - 1) / chunk_size;
        let msg_id = Uuid::new_v4().to_string()[..8].to_string();

        for (i, chunk) in payload.as_bytes().chunks(chunk_size).enumerate() {
            let seq = i + 1;
            let chunk_str = String::from_utf8_lossy(chunk);
            let msg = format!("WRTC:[{}/{}|{}]{}", seq, total_chunks, msg_id, chunk_str);
            self.sender.send_privmsg(target, &msg)?;
        }
        Ok(())
    }

    pub fn send_message(&self, channel: &str, text: &str) -> Result<()> {
        self.sender.send_privmsg(channel, text)?;
        Ok(())
    }

    pub fn join_channel(&self, channel: &str) -> Result<()> {
        self.sender.send_join(channel)?;
        Ok(())
    }

    pub fn part_channel(&self, channel: &str) -> Result<()> {
        self.sender.send(Command::PART(channel.to_string(), None))?;
        Ok(())
    }
}
