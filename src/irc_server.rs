use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, RwLock};
use tokio_rustls::TlsAcceptor;
use tracing::{error, info, warn};

use crate::pow;
use crate::tls::CertInfo;

const MAX_MSG_LEN: usize = 512;
const MAX_CLIENTS_PER_IP: usize = 5;
const MAX_TOTAL_CLIENTS: usize = 100;

type Tx = mpsc::UnboundedSender<String>;

struct Client {
    nick: Option<String>,
    pubkey: Option<String>,
    authenticated: bool,
    tx: Tx,
    ip: std::net::IpAddr,
}

struct ServerState {
    clients: HashMap<SocketAddr, Client>,
    channels: HashMap<String, HashSet<SocketAddr>>,
    /// Nick → pubkey binding: once set, only that key can claim the nick.
    nick_pubkeys: HashMap<String, String>,
    /// Current PoW difficulty requirement (leading zero bits in nick hash).
    /// 0 = disabled.  Mods/host can change at runtime via VOIRC_POW_SET.
    pow_required_bits: u8,
}

impl ServerState {
    fn new(pow_required_bits: u8) -> Self {
        Self {
            clients: HashMap::new(),
            channels: HashMap::new(),
            nick_pubkeys: HashMap::new(),
            pow_required_bits,
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

    fn try_bind_nick_pubkey(&mut self, nick: &str, pubkey: &str) -> bool {
        match self.nick_pubkeys.get(nick) {
            Some(existing) if existing != pubkey => false,
            _ => {
                self.nick_pubkeys.insert(nick.to_string(), pubkey.to_string());
                true
            }
        }
    }
}

pub struct EmbeddedServer;

impl EmbeddedServer {
    pub async fn run(port: u16, pow_bits: u8) -> std::io::Result<()> {
        Self::run_inner(port, None, pow_bits).await
    }

    pub async fn run_tls(port: u16, cert_info: &CertInfo, pow_bits: u8) -> std::io::Result<()> {
        let tls_config = crate::tls::server_config(cert_info)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let acceptor = TlsAcceptor::from(tls_config);
        Self::run_inner(port, Some(acceptor), pow_bits).await
    }

    async fn run_inner(port: u16, acceptor: Option<TlsAcceptor>, pow_bits: u8) -> std::io::Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
        let mode = if acceptor.is_some() { "TLS" } else { "plaintext" };
        info!(
            "Embedded IRC Server listening on 0.0.0.0:{} ({}) pow_required_bits={}",
            port, mode, pow_bits
        );

        let state = Arc::new(RwLock::new(ServerState::new(pow_bits)));

        loop {
            let (socket, addr) = listener.accept().await?;
            let state = Arc::clone(&state);

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
        s.clients.insert(addr, Client {
            nick: None,
            pubkey: None,
            authenticated: false,
            tx,
            ip: addr.ip(),
        });
    }

    let writer_handle = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if writer.write_all(msg.as_bytes()).await.is_err() { break; }
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

    {
        let mut s = state.write().await;
        if let Some(client) = s.clients.remove(&addr) {
            if let Some(nick) = client.nick {
                info!("Client disconnected: {}", nick);
                s.nick_pubkeys.remove(&nick);

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
                let new_nick = parts[1].to_string();
                let mut s = state.write().await;
                let client_pubkey = s.clients.get(&addr).and_then(|c| c.pubkey.clone());
                if let Some(ref bound_key) = s.nick_pubkeys.get(&new_nick).cloned() {
                    if client_pubkey.as_deref() != Some(bound_key.as_str()) {
                        if let Some(c) = s.clients.get(&addr) {
                            let _ = c.tx.send(format!(
                                ":voirc 433 * {} :Nickname is already in use\r\n", new_nick
                            ));
                        }
                        return;
                    }
                }
                if let Some(c) = s.clients.get_mut(&addr) {
                    c.nick = Some(new_nick);
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
                    // Advertise current PoW requirement immediately on welcome
                    let _ = c.tx.send(format!(
                        ":voirc NOTICE {} :VOIRC_POW_REQUIRED:{}\r\n",
                        n, s.pow_required_bits
                    ));
                }
            }
        }
        "PRIVMSG" => {
            if parts.len() > 2 {
                let target = parts[1];
                let msg_start = cmd.find(':').map(|i| i + 1).unwrap_or(0);
                let text = if msg_start > 0 { &cmd[msg_start..] } else { "" };

                if target == "voirc" {
                    if let Some(rest) = text.strip_prefix("VOIRC_HELLO:") {
                        handle_hello(rest, addr, state).await;
                        return;
                    }
                    if let Some(rest) = text.strip_prefix("VOIRC_POW_SET:") {
                        handle_pow_set(rest, addr, state).await;
                        return;
                    }
                }

                let s = state.read().await;
                let sender = s.clients.get(&addr);
                let sender_nick = sender.and_then(|c| c.nick.clone()).unwrap_or_default();
                let sender_authed = sender.map(|c| c.authenticated).unwrap_or(false);

                if !sender_nick.is_empty()
                    && s.nick_pubkeys.contains_key(&sender_nick)
                    && !sender_authed
                {
                    warn!("Dropping PRIVMSG from unauthenticated client claiming nick {}", sender_nick);
                    return;
                }

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
                                if let Some(mn) = &c.nick { names_list.push(mn.clone()); }
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
        "KICK" => {
            if parts.len() >= 3 {
                let channel = parts[1].to_string();
                let target_nick = parts[2];
                let reason = cmd.find(':').map(|i| &cmd[i+1..]).unwrap_or("Kicked");
                let mut s = state.write().await;
                let kicker_authed = s.clients.get(&addr).map(|c| c.authenticated).unwrap_or(false);
                if !kicker_authed {
                    warn!("Unauthenticated client at {} tried to kick", addr);
                    return;
                }
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

// ─────────────────────────────────────────────────────────────────────────────
// VOIRC_HELLO
// ─────────────────────────────────────────────────────────────────────────────
//
// Format: VOIRC_HELLO:<nick>:<pubkey_hex>:<sig_hex>
//
// The sig is ed25519 over nick bytes.  The PoW check is simply:
//   leading_zero_bits(SHA256(nick || pubkey_hex)) >= pow_required_bits
//
// A nick mined at difficulty ≥ current requirement passes instantly and forever
// (as long as the room doesn't raise the bar above what the nick was mined to).
// A nick mined below the current requirement gets HELLO_FAILED pow_too_weak:<N>
// and must re-mine.

async fn handle_hello(rest: &str, addr: SocketAddr, state: &Arc<RwLock<ServerState>>) {
    let parts: Vec<&str> = rest.splitn(3, ':').collect();
    if parts.len() != 3 {
        warn!("Malformed VOIRC_HELLO from {}", addr);
        return;
    }
    let (hello_nick, pubkey_hex, sig_hex) = (parts[0], parts[1], parts[2]);

    // Decode + verify ed25519 sig
    let pubkey_bytes = match hex::decode(pubkey_hex) {
        Ok(b) if b.len() == 32 => b,
        _ => { send_notice(state, addr, hello_nick, "HELLO_FAILED invalid_pubkey").await; return; }
    };
    let sig_bytes = match hex::decode(sig_hex) {
        Ok(b) if b.len() == 64 => b,
        _ => { send_notice(state, addr, hello_nick, "HELLO_FAILED invalid_signature").await; return; }
    };

    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let pk_arr: [u8; 32] = pubkey_bytes.try_into().unwrap();
    let verifying_key = match VerifyingKey::from_bytes(&pk_arr) {
        Ok(k) => k,
        Err(_) => { send_notice(state, addr, hello_nick, "HELLO_FAILED invalid_pubkey").await; return; }
    };
    let sig_arr: [u8; 64] = sig_bytes.try_into().unwrap();
    let signature = Signature::from_bytes(&sig_arr);

    if verifying_key.verify(hello_nick.as_bytes(), &signature).is_err() {
        warn!("VOIRC_HELLO sig failed for {} from {}", hello_nick, addr);
        send_notice(state, addr, hello_nick, "HELLO_FAILED invalid_signature").await;
        return;
    }

    // Nick must match what client sent via NICK
    {
        let s = state.read().await;
        if s.clients.get(&addr).and_then(|c| c.nick.as_deref()) != Some(hello_nick) {
            send_notice(state, addr, hello_nick, "HELLO_FAILED nick_mismatch").await;
            return;
        }
    }

    // PoW check: verify SHA256(nick || pubkey_hex) has enough leading zero bits.
    // This is a pure hash check — no grinding at join time.
    {
        let s = state.read().await;
        let required = s.pow_required_bits;
        if !pow::check_difficulty(hello_nick, pubkey_hex, required) {
            let actual = pow::leading_zero_bits(&pow::nick_hash(hello_nick, pubkey_hex));
            warn!(
                "VOIRC_HELLO PoW too weak for {}: {} bits < {} required",
                hello_nick, actual, required
            );
            let msg = format!("HELLO_FAILED pow_too_weak:{}", required);
            send_notice(state, addr, hello_nick, &msg).await;
            return;
        }
    }

    // Bind nick → pubkey, mark authenticated, broadcast
    {
        let mut s = state.write().await;

        if !s.try_bind_nick_pubkey(hello_nick, pubkey_hex) {
            warn!("Nick {} claimed by different key, rejecting {}", hello_nick, addr);
            if let Some(c) = s.clients.get(&addr) {
                let _ = c.tx.send(format!(
                    ":voirc 433 * {} :Nickname is already in use (key mismatch)\r\n", hello_nick
                ));
            }
            return;
        }

        if let Some(c) = s.clients.get_mut(&addr) {
            c.pubkey = Some(pubkey_hex.to_string());
            c.authenticated = true;
        }

        let actual_bits = pow::leading_zero_bits(&pow::nick_hash(hello_nick, pubkey_hex));
        info!("Auth OK: {} pow={} bits pubkey={}...", hello_nick, actual_bits, &pubkey_hex[..8]);

        if let Some(c) = s.clients.get(&addr) {
            let _ = c.tx.send(format!(
                ":voirc NOTICE {} :HELLO_OK pow_bits:{}\r\n", hello_nick, actual_bits
            ));
        }

        let broadcast = format!(":voirc PRIVMSG * :VOIRC_PUBKEY:{}:{}\r\n", hello_nick, pubkey_hex);
        let mut notified = HashSet::new();
        for members in s.channels.values() {
            if members.contains(&addr) {
                for &member_addr in members {
                    if member_addr != addr && notified.insert(member_addr) {
                        if let Some(c) = s.clients.get(&member_addr) {
                            let _ = c.tx.send(broadcast.clone());
                        }
                    }
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// VOIRC_POW_SET
// ─────────────────────────────────────────────────────────────────────────────
//
// Format: PRIVMSG voirc :VOIRC_POW_SET:<bits>
//
// Only authenticated clients may call this.  The client-side permission check
// (host/mod role) is the first gate; this is the second.
//
// After changing difficulty, broadcasts VOIRC_POW_REQUIRED:<new_bits> to all
// clients.  Any client whose current nick no longer meets the new bar will see
// this broadcast and can warn its user.  They are NOT immediately kicked —
// that would be disruptive — but they will fail to re-authenticate if they
// reconnect.

async fn handle_pow_set(rest: &str, addr: SocketAddr, state: &Arc<RwLock<ServerState>>) {
    let new_bits: u8 = match rest.trim().parse::<u8>() {
        Ok(b) if b <= 28 => b,
        _ => {
            let s = state.read().await;
            if let Some(c) = s.clients.get(&addr) {
                let _ = c.tx.send(
                    ":voirc NOTICE * :VOIRC_POW_SET_FAILED invalid_bits (0-28)\r\n".to_string()
                );
            }
            return;
        }
    };

    let nick = {
        let s = state.read().await;
        if !s.clients.get(&addr).map(|c| c.authenticated).unwrap_or(false) {
            warn!("Unauthenticated client {} tried VOIRC_POW_SET", addr);
            return;
        }
        s.clients.get(&addr).and_then(|c| c.nick.clone()).unwrap_or_default()
    };

    let mut s = state.write().await;
    let old = s.pow_required_bits;
    s.pow_required_bits = new_bits;
    info!("PoW difficulty changed by {}: {} → {} bits", nick, old, new_bits);

    let broadcast = format!(":voirc PRIVMSG * :VOIRC_POW_REQUIRED:{}\r\n", new_bits);
    for client in s.clients.values() {
        let _ = client.tx.send(broadcast.clone());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper
// ─────────────────────────────────────────────────────────────────────────────

async fn send_notice(state: &Arc<RwLock<ServerState>>, addr: SocketAddr, nick: &str, msg: &str) {
    let s = state.read().await;
    if let Some(c) = s.clients.get(&addr) {
        let _ = c.tx.send(format!(":voirc NOTICE {} :{}\r\n", nick, msg));
    }
}
