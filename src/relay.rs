use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info};

// Wire format: [1 byte nick_len][nick bytes][2 bytes payload_len BE][payload]
// Total overhead per packet: 3 + nick_len bytes

struct RelayClient {
    nick: String,
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

struct RelayState {
    clients: HashMap<SocketAddr, RelayClient>,
}

pub struct AudioRelay;

impl AudioRelay {
    pub async fn run(port: u16) -> std::io::Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
        info!("Audio relay listening on 0.0.0.0:{}", port);

        let state = Arc::new(RwLock::new(RelayState {
            clients: HashMap::new(),
        }));

        loop {
            let (socket, addr) = listener.accept().await?;
            let state = Arc::clone(&state);

            tokio::spawn(async move {
                if let Err(e) = handle_relay_client(socket, addr, state).await {
                    error!("Relay client error {}: {}", addr, e);
                }
            });
        }
    }
}

async fn handle_relay_client(
    mut socket: TcpStream,
    addr: SocketAddr,
    state: Arc<RwLock<RelayState>>,
) -> std::io::Result<()> {
    // Handshake: client sends [1 byte nick_len][nick bytes]
    let nick_len = socket.read_u8().await? as usize;
    if nick_len == 0 || nick_len > 64 {
        return Ok(());
    }
    let mut nick_buf = vec![0u8; nick_len];
    socket.read_exact(&mut nick_buf).await?;
    let nick = String::from_utf8_lossy(&nick_buf).to_string();

    info!("Relay client connected: {} ({})", nick, addr);

    let (reader, mut writer) = socket.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();

    {
        let mut s = state.write().await;
        s.clients.insert(addr, RelayClient { nick: nick.clone(), tx });
    }

    // Writer task: send relay frames to this client
    let write_handle = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if writer.write_all(&frame).await.is_err() {
                break;
            }
        }
    });

    // Reader: receive frames and forward to all other clients
    let mut reader = tokio::io::BufReader::new(reader);
    loop {
        // Read payload length (2 bytes BE)
        let payload_len = match reader.read_u16().await {
            Ok(len) => len as usize,
            Err(_) => break,
        };

        if payload_len == 0 || payload_len > 4096 {
            break;
        }

        let mut payload = vec![0u8; payload_len];
        if reader.read_exact(&mut payload).await.is_err() {
            break;
        }

        // Build relay frame: [nick_len][nick][payload_len][payload]
        let mut frame = Vec::with_capacity(1 + nick.len() + 2 + payload_len);
        frame.push(nick.len() as u8);
        frame.extend_from_slice(nick.as_bytes());
        frame.extend_from_slice(&(payload_len as u16).to_be_bytes());
        frame.extend_from_slice(&payload);

        // Forward to all other connected clients
        let s = state.read().await;
        for (client_addr, client) in s.clients.iter() {
            if *client_addr != addr {
                let _ = client.tx.send(frame.clone());
            }
        }
    }

    // Cleanup
    {
        let mut s = state.write().await;
        s.clients.remove(&addr);
    }
    write_handle.abort();
    info!("Relay client disconnected: {} ({})", nick, addr);
    Ok(())
}

// Client-side relay connection
pub struct RelayConnection {
    writer: Arc<tokio::sync::Mutex<tokio::net::tcp::OwnedWriteHalf>>,
}

impl RelayConnection {
    pub async fn connect(
        addr: &str,
        nick: &str,
        audio_rx_tx: mpsc::UnboundedSender<(String, Vec<u8>)>,
    ) -> anyhow::Result<Self> {
        let mut socket = TcpStream::connect(addr).await?;

        // Send handshake
        let nick_bytes = nick.as_bytes();
        socket.write_u8(nick_bytes.len() as u8).await?;
        socket.write_all(nick_bytes).await?;

        let (reader, writer) = socket.into_split();
        let writer = Arc::new(tokio::sync::Mutex::new(writer));

        // Reader task: parse incoming relay frames
        tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(reader);
            loop {
                // Read sender nick
                let nick_len = match reader.read_u8().await {
                    Ok(len) => len as usize,
                    Err(_) => break,
                };
                if nick_len == 0 || nick_len > 64 { break; }

                let mut nick_buf = vec![0u8; nick_len];
                if reader.read_exact(&mut nick_buf).await.is_err() { break; }
                let sender = String::from_utf8_lossy(&nick_buf).to_string();

                // Read payload
                let payload_len = match reader.read_u16().await {
                    Ok(len) => len as usize,
                    Err(_) => break,
                };
                if payload_len == 0 || payload_len > 4096 { break; }

                let mut payload = vec![0u8; payload_len];
                if reader.read_exact(&mut payload).await.is_err() { break; }

                let _ = audio_rx_tx.send((sender, payload));
            }
        });

        info!("Connected to relay as {}", nick);
        Ok(Self { writer })
    }

    pub async fn send_audio(&self, data: &[u8]) -> anyhow::Result<()> {
        let mut writer = self.writer.lock().await;
        writer.write_u16(data.len() as u16).await?;
        writer.write_all(data).await?;
        Ok(())
    }
}
