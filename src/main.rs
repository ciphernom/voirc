// src/main.rs

mod irc_client;
mod state;
mod voice_mixer;
mod webrtc_peer;
mod irc_server;
mod config;
mod magic_link;
mod upnp;
mod ui;

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
use tracing::{error, info};
use webrtc_peer::{WebRtcPeer, WebRtcSignal, InternalSignal};

use crate::config::UserConfig;
use crate::magic_link::ConnectionInfo;
use crate::state::AppState;
use crate::ui::{AppScreen, DashboardAction, DashboardUI, JoinPromptUI, RecentServersUI, SettingsUI};
use crate::upnp::PortForwarder;
use crate::voice_mixer::VoiceMixer;

#[derive(Parser, Debug)]
#[clap(name = "voirc")]
struct Args {
    /// Skip dashboard and connect directly (for testing)
    #[clap(long)]
    direct: bool,

    /// Server address when using --direct
    #[clap(long, default_value = "127.0.0.1:6667")]
    server: String,

    /// Channel when using --direct
    #[clap(long, default_value = "#voice-room")]
    channel: String,

    /// Override nickname via CLI
    #[clap(long, short)]
    name: Option<String>,
}

struct VoiceApp {
    state: Arc<AppState>,
    channel: String,
    nickname: String,
    input_buffer: String,
    peers: Arc<RwLock<HashMap<String, Arc<WebRtcPeer>>>>,
}

impl VoiceApp {
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

            let chat_items: Vec<ListItem> = messages
                .iter()
                .rev()
                .take(chunks[0].height as usize - 2)
                .rev()
                .map(|msg| ListItem::new(Line::from(Span::raw(msg.clone()))))
                .collect();
            f.render_widget(
                List::new(chat_items)
                    .block(Block::default().borders(Borders::ALL).title("Chat")),
                chunks[0],
            );

            f.render_widget(
                Paragraph::new(self.input_buffer.as_str())
                    .block(Block::default().borders(Borders::ALL).title("Input")),
                chunks[1],
            );

            let peer_items: Vec<ListItem> = peer_states
                .values()
                .map(|p| {
                    let s = if p.connected {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::Red)
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(&p.nickname, s),
                        Span::raw(if p.connected { " 🟢" } else { " 🔴" }),
                    ]))
                })
                .collect();
            f.render_widget(
                List::new(peer_items)
                    .block(Block::default().borders(Borders::ALL).title("Peers")),
                chunks[2],
            );
        })?;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Setup logging
    let _ = std::fs::create_dir_all("logs");
    let file_appender = tracing_appender::rolling::daily("logs", "voirc.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .init();

    let args = Args::parse();

    // Load user config
    let mut config = UserConfig::load()?;
    
    // Override config if CLI arg provided
    if let Some(name) = args.name {
        config.display_name = name;
    }

    info!("Loaded config for user: {}", config.display_name);

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = if args.direct {
        // Direct connection mode (legacy)
        run_direct_mode(&mut terminal, &mut config, args.server, args.channel).await
    } else {
        // Dashboard mode
        run_dashboard_mode(&mut terminal, &mut config).await
    };

    // Cleanup
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

async fn run_dashboard_mode(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: &mut UserConfig,
) -> Result<()> {
    let mut screen = AppScreen::Dashboard;
    
    // UI States
    let mut dashboard_ui = DashboardUI::new(config.clone());
    let mut settings_ui = SettingsUI::new(config.display_name.clone());

    loop {
        match screen {
            AppScreen::Dashboard => {
                terminal.draw(|f| dashboard_ui.render(f))?;

                if let Event::Key(key) = event::read()? {
                    match dashboard_ui.handle_key(key) {
                        DashboardAction::HostRoom => {
                            screen = AppScreen::HostSetup;
                        }
                        DashboardAction::JoinRoom => {
                            screen = AppScreen::ConnectPrompt;
                        }
                        DashboardAction::SelectRecent(_) => {
                            if !config.recent_servers.is_empty() {
                                let mut recent_ui =
                                    RecentServersUI::new(config.recent_servers.clone());
                                
                                loop {
                                    terminal.draw(|f| recent_ui.render(f))?;
                                    
                                    if let Event::Key(key) = event::read()? {
                                        if key.code == KeyCode::Esc {
                                            break;
                                        }
                                        
                                        if let Some(conn_str) = recent_ui.handle_key(key) {
                                            if let Ok(info) = ConnectionInfo::from_magic_link(&conn_str) {
                                                return start_voice_session(
                                                    terminal,
                                                    config,
                                                    info,
                                                    false,
                                                )
                                                .await;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        DashboardAction::EditProfile => {
                            settings_ui = SettingsUI::new(config.display_name.clone());
                            screen = AppScreen::Settings;
                        }
                        DashboardAction::Quit => return Ok(()),
                        _ => {}
                    }
                }
            }
            
            AppScreen::Settings => {
                terminal.draw(|f| settings_ui.render(f))?;

                if let Event::Key(key) = event::read()? {
                    if key.code == KeyCode::Esc {
                        screen = AppScreen::Dashboard;
                    }
                    else if let Some(new_name) = settings_ui.handle_key(key) {
                        config.display_name = new_name;
                        let _ = config.save();
                        // Update dashboard with new name
                        dashboard_ui = DashboardUI::new(config.clone());
                        screen = AppScreen::Dashboard;
                    }
                }
            }

            AppScreen::ConnectPrompt => {
                let mut join_ui = JoinPromptUI::new();

                loop {
                    terminal.draw(|f| join_ui.render(f))?;

                    if let Event::Key(key) = event::read()? {
                        if key.code == KeyCode::Esc {
                            screen = AppScreen::Dashboard;
                            break;
                        }

                        if let Some(result) = join_ui.handle_key(key) {
                            match result {
                                Ok(info) => {
                                    return start_voice_session(terminal, config, info, false).await;
                                }
                                Err(_) => {
                                    // Error is displayed in UI
                                }
                            }
                        }
                    }
                }
            }

            AppScreen::HostSetup => {
                // Host a room
                return host_room(terminal, config).await;
            }

            _ => {}
        }
    }
}

async fn host_room(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: &mut UserConfig,
) -> Result<()> {
    const PORT: u16 = 6667;
    const CHANNEL: &str = "#voice-room";

    // Show hosting status
    terminal.draw(|f| {
        let area = f.area();
        let para = Paragraph::new("🚀 Setting up your room...\n\nStarting server and configuring network...")
            .style(Style::default().fg(Color::Cyan))
            .block(Block::default().borders(Borders::ALL).title("Hosting Room"));
        f.render_widget(para, area);
    })?;

    // Start embedded server
    tokio::spawn(async move {
        if let Err(e) = crate::irc_server::EmbeddedServer::run(PORT).await {
            error!("Embedded server failed: {}", e);
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Try UPnP port forwarding
    let upnp_success = PortForwarder::forward_port(PORT).await.is_ok();

    // Get external IP
    let ip = PortForwarder::get_external_ip().await.unwrap_or_else(|_| "127.0.0.1".to_string());

    let conn_info = ConnectionInfo::new(ip.clone(), PORT, CHANNEL.to_string());
    let magic_link = conn_info.to_magic_link()?;

    // Copy to clipboard (Initial attempt)
    #[cfg(feature = "clipboard")]
    {
        use arboard::Clipboard;
        if let Ok(mut clipboard) = Clipboard::new() {
            let _ = clipboard.set_text(&magic_link);
        }
    }

    let mut copied_notification = false;

    // Wait for user to start or copy
    loop {
        terminal.draw(|f| {
            let area = f.area();
            let mut text = format!(
                "✅ Room is ready!\n\n\
                Share this link with friends:\n\
                {}\n\n\
                Your IP: {}\n\
                Port: {}\n\
                UPnP: {}\n\n\
                ⚠️  If friends can't connect, forward port 6667 in your router.\n\n\
                [ENTER] Start Session   [C] Copy Link   [ESC] Back",
                magic_link,
                ip,
                PORT,
                if upnp_success { "✅ Enabled" } else { "⚠️  Manual forwarding may be needed" },
            );

            if copied_notification {
                text.push_str("\n\n📋 LINK COPIED TO CLIPBOARD!");
            }

            let para = Paragraph::new(text)
                .style(Style::default().fg(Color::Green))
                .block(Block::default().borders(Borders::ALL).title("Room Ready"));
            f.render_widget(para, area);
        })?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Enter => break,
                KeyCode::Esc => return Ok(()), // Cancel hosting
                KeyCode::Char('c') | KeyCode::Char('C') => {
                    // Manual copy trigger
                    #[cfg(feature = "clipboard")]
                    {
                        use arboard::Clipboard;
                        if let Ok(mut clipboard) = Clipboard::new() {
                            if clipboard.set_text(&magic_link).is_ok() {
                                copied_notification = true;
                            }
                        }
                    }
                    // If no clipboard feature, we can't do much, but we won't crash
                },
                _ => {}
            }
        }
    }

    // Save to recent servers
    config.add_recent_server("My Hosted Room".to_string(), magic_link);

    // Start the voice session as host
    start_voice_session(terminal, config, conn_info, true).await
}

async fn start_voice_session(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: &mut UserConfig,
    conn_info: ConnectionInfo,
    is_host: bool,
) -> Result<()> {
    let state = AppState::new();
    let nickname = config.display_name.clone();

    // Audio system
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

    // IRC connection
    let (irc_client, irc_stream, mut irc_events) = IrcClient::connect(
        conn_info.server_address(),
        nickname.clone(),
        conn_info.channel.clone(),
        Arc::clone(&state),
    )
    .await?;

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

    let mut app = VoiceApp::new(Arc::clone(&state), conn_info.channel.clone(), nickname.clone());

    let peers_access = Arc::clone(&app.peers);
    tokio::spawn(async move {
        while let Some(packet) = mic_rx.recv().await {
            let peers = peers_access.read().await;
            for peer in peers.values() {
                let _ = peer.send_audio(&packet).await;
            }
        }
    });

    state
        .add_message(format!("Connected as {}", nickname))
        .await;
    if is_host {
        state.add_message("🏠 You are hosting this room".to_string()).await;
    }

    loop {
        app.render_ui(terminal).await?;

        // Handle reconnections
        while let Ok(nick) = reconnect_rx.try_recv() {
            info!("Connection failed with {}, attempting re-dial", nick);
            app.peers.write().await.remove(&nick);

            if nickname.as_str() < nick.as_str() {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                let states = state.peer_states.read().await;
                if states.contains_key(&nick) {
                    info!("Re-initiating call to {}", nick);
                    match WebRtcPeer::new(
                        nick.clone(),
                        Arc::clone(&state),
                        mix_tx.clone(),
                        ice_out_tx.clone(),
                    )
                    .await
                    {
                        Ok(peer) => match peer.create_offer().await {
                            Ok(offer_json) => {
                                app.peers.write().await.insert(nick.clone(), Arc::new(peer));
                                let _ = irc.send_webrtc_signal(&nick, &offer_json);
                                state
                                    .add_message(format!("-> Reconnecting to {}", nick))
                                    .await;
                            }
                            Err(e) => error!("Failed to create re-dial offer: {}", e),
                        },
                        Err(e) => error!("Failed to create re-dial peer: {}", e),
                    }
                }
            }
        }

        // Handle IRC events
        while let Ok(event) = irc_events.try_recv() {
            match event {
                IrcEvent::UserJoined(nick) => {
                    info!("Joined: {}", nick);
                    state.update_peer_state(nick.clone(), false, false).await;

                    if nickname < nick {
                        info!("I am initiating call to {}", nick);
                        let peer = WebRtcPeer::new(
                            nick.clone(),
                            Arc::clone(&state),
                            mix_tx.clone(),
                            ice_out_tx.clone(),
                        )
                        .await?;
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

                            let peer = WebRtcPeer::new(
                                from.clone(),
                                Arc::clone(&state),
                                mix_tx.clone(),
                                ice_out_tx.clone(),
                            )
                            .await?;
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
                        Ok(WebRtcSignal::IceCandidate {
                            candidate,
                            sdp_mid,
                            sdp_mline_index,
                        }) => {
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

        // Handle keyboard input
        if event::poll(std::time::Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char(c) => app.input_buffer.push(c),
                    KeyCode::Backspace => {
                        app.input_buffer.pop();
                    }
                    KeyCode::Esc => break,
                    KeyCode::Enter => {
                        if !app.input_buffer.is_empty() {
                            irc.send_message(&app.channel, &app.input_buffer)?;
                            state
                                .add_message(format!("<{}> {}", app.nickname, app.input_buffer))
                                .await;
                            app.input_buffer.clear();
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(())
}

async fn run_direct_mode(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: &mut UserConfig,
    server: String,
    channel: String,
) -> Result<()> {
    let (host, port_str) = server.split_once(':').unwrap_or((&server, "6667"));
    let port = port_str.parse::<u16>().unwrap_or(6667);

    let conn_info = ConnectionInfo::new(host.to_string(), port, channel);

    start_voice_session(terminal, config, conn_info, false).await
}
