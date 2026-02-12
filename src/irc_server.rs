use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracing::{error, info};

type Tx = mpsc::UnboundedSender<String>;

struct Client {
    nick: Option<String>,
    tx: Tx,
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
}

pub struct EmbeddedServer;

impl EmbeddedServer {
    pub async fn run(port: u16) -> std::io::Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
        info!("Embedded IRC Server listening on 0.0.0.0:{}", port);

        let state = Arc::new(Mutex::new(ServerState::new()));

        loop {
            let (socket, addr) = listener.accept().await?;
            let state = Arc::clone(&state);

            tokio::spawn(async move {
                if let Err(e) = handle_connection(socket, addr, state).await {
                    error!("Connection error for {}: {}", addr, e);
                }
            });
        }
    }
}

async fn handle_connection(
    conn: TcpStream,
    addr: SocketAddr,
    state: Arc<Mutex<ServerState>>,
) -> std::io::Result<()> {
    let (reader, mut writer) = conn.into_split();
    let mut reader = BufReader::new(reader);
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    {
        let mut s = state.lock().unwrap();
        s.clients.insert(addr, Client { nick: None, tx });
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
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            process_command(trimmed, addr, &state).await;
        }
        line.clear();
    }

    // Cleanup
    {
        let mut s = state.lock().unwrap();
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

                // FIX: Standard QUIT message
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

async fn process_command(cmd: &str, addr: SocketAddr, state: &Arc<Mutex<ServerState>>) {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() { return; }

    match parts[0] {
        "CAP" => {
            if let Some(c) = state.lock().unwrap().clients.get(&addr) {
                let _ = c.tx.send(":voirc CAP * LS :\r\n".to_string());
            }
        }
        "NICK" => {
            if parts.len() > 1 {
                let mut s = state.lock().unwrap();
                if let Some(c) = s.clients.get_mut(&addr) {
                    c.nick = Some(parts[1].to_string());
                }
            }
        }
        "USER" => {
            let nick = {
                let s = state.lock().unwrap();
                s.clients.get(&addr).and_then(|c| c.nick.clone())
            };

            if let Some(n) = nick {
                let s = state.lock().unwrap();
                if let Some(c) = s.clients.get(&addr) {
                    // 1. Send Welcome
                    let _ = c.tx.send(format!(":voirc 001 {} :Welcome to Voirc\r\n", n));
                    // 2. FIX: Send "ERR_NOMOTD" (422). This signals the client to start auto-joining.
                    let _ = c.tx.send(format!(":voirc 422 {} :MOTD File is missing\r\n", n));
                }
            }
        }
        "JOIN" => {
            if parts.len() > 1 {
                let channel = parts[1];
                let mut names_list = Vec::new();
                
                let nick_opt = {
                    let s = state.lock().unwrap();
                    s.clients.get(&addr).and_then(|c| c.nick.clone())
                };

                if let Some(n) = nick_opt {
                    let mut s = state.lock().unwrap();
                    s.channels.entry(channel.to_string()).or_default().insert(addr);
                    
                    let full_mask = format!("{}!voirc@127.0.0.1", n);
                    let join_msg = format!(":{} JOIN {}\r\n", full_mask, channel);

                    if let Some(members) = s.channels.get(channel) {
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
                
                let s = state.lock().unwrap();
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
        "PING" => {
             if parts.len() > 1 {
                 let s = state.lock().unwrap();
                 if let Some(c) = s.clients.get(&addr) {
                     let _ = c.tx.send(format!("PONG {}\r\n", parts[1]));
                 }
             }
        }
        _ => {}
    }
}
