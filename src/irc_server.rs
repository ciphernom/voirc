use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener};
use tokio::sync::{mpsc, RwLock};
use tokio_rustls::TlsAcceptor;
use tracing::{error, info, warn};

use crate::tls::CertInfo;

const MAX_MSG_LEN: usize = 512;
const MAX_CLIENTS_PER_IP: usize = 5;
const MAX_TOTAL_CLIENTS: usize = 100;

type Tx = mpsc::UnboundedSender<String>;

struct Client {
    nick: Option<String>,
    tx: Tx,
    ip: std::net::IpAddr,
}

struct ServerState {
    clients: HashMap<SocketAddr, Client>,
    channels: HashMap<String, HashSet<SocketAddr>>,
}

impl ServerState {
    fn new() -> Self {
        Self {
            clients: HashMap::new(),
            channels: HashMap::new(),
        }
    }

    fn find_addr_by_nick(&self, nick: &str) -> Option<SocketAddr> {
        self.clients.iter()
            .find(|(_, c)| c.nick.as_deref() == Some(nick))
            .map(|(addr, _)| *addr)
    }

    fn count_ip(&self, ip: std::net::IpAddr) -> usize {
        self.clients.values().filter(|c| c.ip == ip).count()
    }
}

pub struct EmbeddedServer;

impl EmbeddedServer {
    pub async fn run(port: u16) -> std::io::Result<()> {
        Self::run_inner(port, None).await
    }

    pub async fn run_tls(port: u16, cert_info: &CertInfo) -> std::io::Result<()> {
        let tls_config = crate::tls::server_config(cert_info)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let acceptor = TlsAcceptor::from(tls_config);
        Self::run_inner(port, Some(acceptor)).await
    }

    async fn run_inner(port: u16, acceptor: Option<TlsAcceptor>) -> std::io::Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
        let mode = if acceptor.is_some() { "TLS" } else { "plaintext" };
        info!("Embedded IRC Server listening on 0.0.0.0:{} ({})", port, mode);

        let state = Arc::new(RwLock::new(ServerState::new()));

        loop {
            let (socket, addr) = listener.accept().await?;
            let state = Arc::clone(&state);

            // Connection limit checks
            {
                let s = state.read().await;
                if s.clients.len() >= MAX_TOTAL_CLIENTS {
                    warn!("Max clients reached, rejecting {}", addr);
                    continue;
                }
                if s.count_ip(addr.ip()) >= MAX_CLIENTS_PER_IP {
                    warn!("Max clients per IP reached for {}", addr.ip());
                    continue;
                }
            }

            let acc = acceptor.clone();
            tokio::spawn(async move {
                let result = if let Some(acceptor) = acc {
                    match acceptor.accept(socket).await {
                        Ok(tls_stream) => {
                            let (reader, writer) = tokio::io::split(tls_stream);
                            handle_connection_io(reader, writer, addr, state).await
                        }
                        Err(e) => {
                            warn!("TLS handshake failed for {}: {}", addr, e);
                            return;
                        }
                    }
                } else {
                    let (reader, writer) = socket.into_split();
                    handle_connection_io(reader, writer, addr, state).await
                };

                if let Err(e) = result {
                    error!("Connection error for {}: {}", addr, e);
                }
            });
        }
    }
}

async fn handle_connection_io<R, W>(
    reader: R,
    mut writer: W,
    addr: SocketAddr,
    state: Arc<RwLock<ServerState>>,
) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut reader = BufReader::new(reader);
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    {
        let mut s = state.write().await;
        s.clients.insert(addr, Client { nick: None, tx, ip: addr.ip() });
    }

    let writer_handle = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if writer.write_all(msg.as_bytes()).await.is_err() {
                break;
            }
        }
    });

    let mut line = String::new();
    while reader.read_line(&mut line).await? > 0 {
        if line.len() > MAX_MSG_LEN {
            line.clear();
            continue;
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            process_command(trimmed, addr, &state).await;
        }
        line.clear();
    }

    // Cleanup
    {
        let mut s = state.write().await;
        if let Some(client) = s.clients.remove(&addr) {
            if let Some(nick) = client.nick {
                info!("Client disconnected: {}", nick);

                let mut peers_to_notify = HashSet::new();
                for (_channel, members) in s.channels.iter_mut() {
                    if members.remove(&addr) {
                        for member_addr in members.iter() {
                            peers_to_notify.insert(*member_addr);
                        }
                    }
                }

                let full_mask = format!("{}!voirc@127.0.0.1", nick);
                let quit_msg = format!(":{} QUIT :Connection closed\r\n", full_mask);
                for peer_addr in peers_to_notify {
                    if let Some(c) = s.clients.get(&peer_addr) {
                        let _ = c.tx.send(quit_msg.clone());
                    }
                }
            }
        }
    }

    writer_handle.abort();
    Ok(())
}

async fn process_command(cmd: &str, addr: SocketAddr, state: &Arc<RwLock<ServerState>>) {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() { return; }

    match parts[0] {
        "CAP" => {
            let s = state.read().await;
            if let Some(c) = s.clients.get(&addr) {
                let _ = c.tx.send(":voirc CAP * LS :\r\n".to_string());
            }
        }
        "NICK" => {
            if parts.len() > 1 {
                let mut s = state.write().await;
                if let Some(c) = s.clients.get_mut(&addr) {
                    c.nick = Some(parts[1].to_string());
                }
            }
        }
        "USER" => {
            let s = state.read().await;
            let nick = s.clients.get(&addr).and_then(|c| c.nick.clone());
            if let Some(n) = nick {
                if let Some(c) = s.clients.get(&addr) {
                    let _ = c.tx.send(format!(":voirc 001 {} :Welcome to Voirc\r\n", n));
                    let _ = c.tx.send(format!(":voirc 422 {} :MOTD File is missing\r\n", n));
                }
            }
        }
        "JOIN" => {
            if parts.len() > 1 {
                let channel = parts[1].to_string();
                let mut names_list = Vec::new();

                let nick_opt = {
                    let s = state.read().await;
                    s.clients.get(&addr).and_then(|c| c.nick.clone())
                };

                if let Some(n) = nick_opt {
                    let mut s = state.write().await;
                    s.channels.entry(channel.clone()).or_default().insert(addr);

                    let full_mask = format!("{}!voirc@127.0.0.1", n);
                    let join_msg = format!(":{} JOIN {}\r\n", full_mask, channel);

                    if let Some(members) = s.channels.get(&channel) {
                        for member in members {
                            if let Some(c) = s.clients.get(member) {
                                let _ = c.tx.send(join_msg.clone());
                                if let Some(mn) = &c.nick {
                                    names_list.push(mn.clone());
                                }
                            }
                        }
                    }

                    if let Some(c) = s.clients.get(&addr) {
                        let names_str = names_list.join(" ");
                        let _ = c.tx.send(format!(":voirc 353 {} = {} :{}\r\n", n, channel, names_str));
                        let _ = c.tx.send(format!(":voirc 366 {} {} :End of /NAMES list.\r\n", n, channel));
                    }
                }
            }
        }
        "PRIVMSG" => {
            if parts.len() > 2 {
                let target = parts[1];
                let msg_start = cmd.find(':').map(|i| i + 1).unwrap_or(0);
                let text = if msg_start > 0 { &cmd[msg_start..] } else { "" };

                let s = state.read().await;
                let sender_nick = s.clients.get(&addr).and_then(|c| c.nick.clone()).unwrap_or_default();

                let full_mask = format!("{}!voirc@127.0.0.1", sender_nick);
                let raw_msg = format!(":{} PRIVMSG {} :{}\r\n", full_mask, target, text);

                if let Some(members) = s.channels.get(target) {
                    for member in members {
                        if *member != addr {
                            if let Some(c) = s.clients.get(member) {
                                let _ = c.tx.send(raw_msg.clone());
                            }
                        }
                    }
                } else {
                    for client in s.clients.values() {
                        if client.nick.as_deref() == Some(target) {
                            let _ = client.tx.send(raw_msg.clone());
                            break;
                        }
                    }
                }
            }
        }
        "KICK" => {
            if parts.len() >= 3 {
                let channel = parts[1].to_string();
                let target_nick = parts[2];
                let reason = cmd.find(':').map(|i| &cmd[i+1..]).unwrap_or("Kicked");

                let mut s = state.write().await;
                let kicker_nick = s.clients.get(&addr).and_then(|c| c.nick.clone()).unwrap_or_default();

                if let Some(target_addr) = s.find_addr_by_nick(target_nick) {
                    let full_mask = format!("{}!voirc@127.0.0.1", kicker_nick);
                    let kick_msg = format!(":{} KICK {} {} :{}\r\n", full_mask, channel, target_nick, reason);

                    if let Some(members) = s.channels.get(&channel) {
                        for member in members.iter() {
                            if let Some(c) = s.clients.get(member) {
                                let _ = c.tx.send(kick_msg.clone());
                            }
                        }
                    }

                    // Actually remove the target from the channel server-side
                    if let Some(members) = s.channels.get_mut(&channel) {
                        members.remove(&target_addr);
                    }
                }
            }
        }
        "PART" => {
            if parts.len() > 1 {
                let channel = parts[1].to_string();
                let mut s = state.write().await;
                let nick_opt = s.clients.get(&addr).and_then(|c| c.nick.clone());

                if let Some(n) = nick_opt {
                    let full_mask = format!("{}!voirc@127.0.0.1", n);
                    let part_msg = format!(":{} PART {}\r\n", full_mask, channel);

                    if let Some(members) = s.channels.get(&channel) {
                        for member in members.iter() {
                            if *member != addr {
                                if let Some(c) = s.clients.get(member) {
                                    let _ = c.tx.send(part_msg.clone());
                                }
                            }
                        }
                    }
                    if let Some(members) = s.channels.get_mut(&channel) {
                        members.remove(&addr);
                    }
                }
            }
        }
        "PING" => {
            if parts.len() > 1 {
                let s = state.read().await;
                if let Some(c) = s.clients.get(&addr) {
                    let _ = c.tx.send(format!("PONG {}\r\n", parts[1]));
                }
            }
        }
        _ => {}
    }
}
