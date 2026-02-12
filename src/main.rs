mod irc_client;
mod state;
mod voice_mixer;
mod webrtc_peer;
mod irc_server; // <--- FIX: Added this line

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use irc_client::{IrcClient, IrcEvent};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Terminal,
};
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info}; // FIX: Removed unused 'warn'
use webrtc_peer::{WebRtcPeer, WebRtcSignal, InternalSignal};

use crate::state::AppState;
use crate::voice_mixer::VoiceMixer;

#[derive(Parser, Debug)]
#[clap(name = "voice-irc")]
struct Args {
    #[clap(long)]
    host: bool,

    #[clap(long, default_value = "127.0.0.1:6667")]
    server: String,

    #[clap(long, default_value = "#voice-room")]
    channel: String,
    
    #[clap(long, default_value = "User1")]
    nickname: String,
}

struct App {
    state: Arc<AppState>,
    channel: String,
    nickname: String,
    input_buffer: String,
    peers: Arc<RwLock<HashMap<String, Arc<WebRtcPeer>>>>,
}

impl App {
    fn new(state: Arc<AppState>, channel: String, nickname: String) -> Self {
        Self {
            state,
            channel,
            nickname,
            input_buffer: String::new(),
            peers: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    async fn render_ui(&self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        let messages = self.state.messages.read().await;
        let peer_states = self.state.peer_states.read().await;

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(5), Constraint::Length(3), Constraint::Length(5)])
                .split(f.area());

            let chat_items: Vec<ListItem> = messages.iter().rev().take(chunks[0].height as usize - 2).rev()
                .map(|msg| ListItem::new(Line::from(Span::raw(msg.clone())))).collect();
            f.render_widget(List::new(chat_items).block(Block::default().borders(Borders::ALL).title("Chat")), chunks[0]);

            f.render_widget(Paragraph::new(self.input_buffer.as_str()).block(Block::default().borders(Borders::ALL).title("Input")), chunks[1]);

            let peer_items: Vec<ListItem> = peer_states.values().map(|p| {
                let s = if p.connected { Style::default().fg(Color::Green) } else { Style::default().fg(Color::Red) };
                ListItem::new(Line::from(vec![Span::styled(&p.nickname, s), Span::raw(if p.connected { " [CONN]" } else { " [....]" })]))
            }).collect();
            f.render_widget(List::new(peer_items).block(Block::default().borders(Borders::ALL).title("Peers")), chunks[2]);
        })?;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Create logs directory
    let _ = std::fs::create_dir_all("logs");
    
    // 2. Setup Logging
    let file_appender = tracing_appender::rolling::daily("logs", "voirc.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .init();

    let args = Args::parse();
    let state = AppState::new();

    // --- Embedded Server Logic ---
    let target_server = if args.host {
        let port = args.server.split(':').last().unwrap_or("6667").parse::<u16>().unwrap_or(6667);
        tokio::spawn(async move {
            if let Err(e) = crate::irc_server::EmbeddedServer::run(port).await {
                error!("Embedded server failed: {}", e);
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        format!("127.0.0.1:{}", port)
    } else {
        args.server.clone()
    };

    // --- Audio System ---
    let mixer = Arc::new(VoiceMixer::new()?);
    let (mic_tx, mut mic_rx) = mpsc::unbounded_channel();
    let (mix_tx, mut mix_rx) = mpsc::unbounded_channel();

    let _input_stream = mixer.start_input(mic_tx)?;
    let _output_stream = mixer.start_output()?;

    let mixer_clone = Arc::clone(&mixer);
    tokio::spawn(async move {
        while let Some(packet) = mix_rx.recv().await {
            mixer_clone.add_peer_audio(packet).await;
        }
    });

    let (ice_out_tx, mut ice_out_rx) = mpsc::unbounded_channel::<InternalSignal>();
    let (reconnect_tx, mut reconnect_rx) = mpsc::unbounded_channel::<String>();

    // --- IRC System ---
    let (irc_client, irc_stream, mut irc_events) = IrcClient::connect(
        target_server, args.nickname.clone(), args.channel.clone(), Arc::clone(&state)
    ).await?;
    
    let irc = Arc::new(irc_client);
    
    let irc_run = Arc::clone(&irc);
    tokio::spawn(async move { 
        let _ = irc_run.run(irc_stream).await; 
    });

    let irc_send = Arc::clone(&irc);
    let reconnect_fwd = reconnect_tx.clone();
    tokio::spawn(async move {
        while let Some(signal) = ice_out_rx.recv().await {
            match signal {
                InternalSignal::WebRtc(target, webrtc_signal) => {
                    if let Ok(json) = serde_json::to_string(&webrtc_signal) {
                        let _ = irc_send.send_webrtc_signal(&target, &json);
                    }
                }
                InternalSignal::Reconnect(nick) => {
                    let _ = reconnect_fwd.send(nick);
                }
            }
        }
    });

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let mut app = App::new(Arc::clone(&state), args.channel.clone(), args.nickname.clone());

    let peers_access = Arc::clone(&app.peers);
    tokio::spawn(async move {
        while let Some(packet) = mic_rx.recv().await {
            let peers = peers_access.read().await;
            for peer in peers.values() {
                let _ = peer.send_audio(&packet).await;
            }
        }
    });

    loop {
        app.render_ui(&mut terminal).await?;

        // Handle reconnect events
        while let Ok(nick) = reconnect_rx.try_recv() {
            info!("Connection failed with {}, attempting re-dial", nick);
            
            // Remove old peer
            app.peers.write().await.remove(&nick);
            
            // Only re-dial if we're the initiator (alphabetically first)
            if args.nickname.as_str() < nick.as_str() {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                
                // Check if still in channel
                let states = state.peer_states.read().await;
                if states.contains_key(&nick) {
                    info!("Re-initiating call to {}", nick);
                    match WebRtcPeer::new(nick.clone(), Arc::clone(&state), mix_tx.clone(), ice_out_tx.clone()).await {
                        Ok(peer) => {
                            match peer.create_offer().await {
                                Ok(offer_json) => {
                                    app.peers.write().await.insert(nick.clone(), Arc::new(peer));
                                    let _ = irc.send_webrtc_signal(&nick, &offer_json);
                                    state.add_message(format!("-> Reconnecting to {}", nick)).await;
                                }
                                Err(e) => error!("Failed to create re-dial offer: {}", e),
                            }
                        }
                        Err(e) => error!("Failed to create re-dial peer: {}", e),
                    }
                }
            }
        }

        while let Ok(event) = irc_events.try_recv() {
            match event {
                IrcEvent::UserJoined(nick) => {
                    info!("Joined: {}", nick);
                    
                    // Initialize peer state as disconnected
                    state.update_peer_state(nick.clone(), false, false).await;
                    
                    if args.nickname < nick {
                        info!("I am initiating call to {}", nick);
                        let peer = WebRtcPeer::new(nick.clone(), Arc::clone(&state), mix_tx.clone(), ice_out_tx.clone()).await?;
                        let offer_json = peer.create_offer().await?;
                        app.peers.write().await.insert(nick.clone(), Arc::new(peer));
                        irc.send_webrtc_signal(&nick, &offer_json)?;
                        state.add_message(format!("-> Calling {}", nick)).await;
                    } else {
                        info!("Waiting for call from {}", nick);
                    }
                }
                IrcEvent::UserLeft(nick) => {
                    app.peers.write().await.remove(&nick);
                    state.remove_peer(&nick).await;
                }
                IrcEvent::WebRtcSignal { from, payload } => {
                    match serde_json::from_str::<WebRtcSignal>(&payload) {
                        Ok(WebRtcSignal::Offer { sdp }) => {
                            state.add_message(format!("<- Offer {}", from)).await;
                            
                            let peer = WebRtcPeer::new(from.clone(), Arc::clone(&state), mix_tx.clone(), ice_out_tx.clone()).await?;
                            let answer_json = peer.handle_offer(sdp).await?;
                            app.peers.write().await.insert(from.clone(), Arc::new(peer));
                            irc.send_webrtc_signal(&from, &answer_json)?;
                        }
                        Ok(WebRtcSignal::Answer { sdp }) => {
                            state.add_message(format!("<- Answer {}", from)).await;
                            if let Some(peer) = app.peers.read().await.get(&from) {
                                if let Err(e) = peer.handle_answer(sdp).await {
                                    error!("Failed to handle answer from {}: {}", from, e);
                                }
                            }
                        }
                        Ok(WebRtcSignal::IceCandidate { candidate, sdp_mid, sdp_mline_index }) => {
                            if let Some(peer) = app.peers.read().await.get(&from) {
                                let _ = peer.add_ice_candidate(candidate, sdp_mid, sdp_mline_index).await;
                            }
                        }
                        Err(e) => error!("Signal Error: {}", e),
                    }
                }
                _ => {}
            }
        }

        if event::poll(std::time::Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char(c) => app.input_buffer.push(c),
                    KeyCode::Backspace => { app.input_buffer.pop(); },
                    KeyCode::Esc => break,
                    KeyCode::Enter => {
                        if !app.input_buffer.is_empty() {
                            irc.send_message(&app.channel, &app.input_buffer)?;
                            state.add_message(format!("<{}> {}", app.nickname, app.input_buffer)).await;
                            app.input_buffer.clear();
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}
