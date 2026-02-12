// src/gui.rs

use egui::{CentralPanel, Context, RichText, ScrollArea, TextEdit, TopBottomPanel};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info};

use crate::config::UserConfig;
use crate::irc_client::{IrcClient, IrcEvent};
use crate::irc_server::EmbeddedServer;
use crate::magic_link::ConnectionInfo;
use crate::state::AppState;
use crate::upnp::PortForwarder;
use crate::voice_mixer::VoiceMixer;
use crate::webrtc_peer::{InternalSignal, WebRtcPeer, WebRtcSignal};

#[derive(Debug, Clone, PartialEq)]
enum Screen {
    Dashboard,
    HostSetup,
    JoinPrompt,
    Settings,
    RecentServers,
    InCall,
}

pub struct VoircApp {
    config: UserConfig,
    screen: Screen,
    
    // Dashboard
    selected_dashboard_item: usize,
    
    // Host Setup
    host_port: String,
    host_channel: String,
    host_error: Option<String>,
    host_link: Option<String>,
    
    // Join Prompt
    join_input: String,
    join_error: Option<String>,
    
    // Settings
    settings_name: String,
    
    // Recent Servers
    selected_recent: usize,
    
    // In Call
    chat_input: String,
    call_state: Option<Arc<CallState>>,
}

struct CallState {
    state: Arc<AppState>,
    send_message_tx: mpsc::UnboundedSender<String>,
    shutdown_tx: mpsc::UnboundedSender<()>,
    channel: String,
    nickname: String,
    _mixer: Arc<VoiceMixer>,
    _input_stream: cpal::Stream,
    _output_stream: cpal::Stream,
}

impl VoircApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, config: UserConfig) -> Self {
        Self {
            config,
            screen: Screen::Dashboard,
            selected_dashboard_item: 0,
            host_port: "6667".to_string(),
            host_channel: "#general".to_string(),
            host_error: None,
            host_link: None,
            join_input: String::new(),
            join_error: None,
            settings_name: String::new(),
            selected_recent: 0,
            chat_input: String::new(),
            call_state: None,
        }
    }

    fn render_dashboard(&mut self, ctx: &Context) {
        CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.heading(RichText::new("🎤 Voirc").size(32.0).strong());
                ui.add_space(10.0);
                ui.label(RichText::new(format!("Welcome, {}", self.config.display_name)).size(18.0));
                ui.add_space(40.0);
            });

            ui.vertical_centered(|ui| {
                let button_size = egui::vec2(300.0, 50.0);
                
                if ui.add_sized(button_size, egui::Button::new(RichText::new("🏠 Host a Room").size(16.0))).clicked() {
                    self.screen = Screen::HostSetup;
                }
                ui.add_space(10.0);
                
                if ui.add_sized(button_size, egui::Button::new(RichText::new("🔗 Join a Room").size(16.0))).clicked() {
                    self.screen = Screen::JoinPrompt;
                }
                ui.add_space(10.0);
                
                if !self.config.recent_servers.is_empty() {
                    if ui.add_sized(button_size, egui::Button::new(RichText::new("📋 Recent Servers").size(16.0))).clicked() {
                        self.screen = Screen::RecentServers;
                    }
                    ui.add_space(10.0);
                }
                
                if ui.add_sized(button_size, egui::Button::new(RichText::new("⚙️ Settings").size(16.0))).clicked() {
                    self.settings_name = self.config.display_name.clone();
                    self.screen = Screen::Settings;
                }
            });
        });
    }

    fn render_host_setup(&mut self, ctx: &Context) {
        CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.heading(RichText::new("Host a Voice Room").size(24.0));
                ui.add_space(30.0);

                if self.host_link.is_none() {
                    // Configuration phase
                    ui.vertical(|ui| {
                        ui.set_max_width(400.0);
                        
                        ui.label(RichText::new("Port").size(14.0));
                        ui.text_edit_singleline(&mut self.host_port);
                        ui.add_space(15.0);

                        ui.label(RichText::new("Channel Name").size(14.0));
                        ui.text_edit_singleline(&mut self.host_channel);
                        ui.add_space(20.0);

                        if let Some(error) = &self.host_error {
                            ui.colored_label(egui::Color32::RED, error);
                            ui.add_space(10.0);
                        }

                        if ui.add_sized([400.0, 40.0], egui::Button::new(RichText::new("🚀 Start Server").size(16.0))).clicked() {
                            self.start_hosting();
                        }
                    });
                } else {
                    // Server running phase
                    ui.vertical(|ui| {
                        ui.set_max_width(500.0);
                        
                        ui.label(RichText::new("✅ Server is running!").size(18.0).color(egui::Color32::GREEN));
                        ui.add_space(20.0);
                        
                        ui.label(RichText::new("Share this link with friends:").size(14.0));
                        ui.add_space(10.0);
                        
                        if let Some(link) = &self.host_link {
                            egui::Frame::none()
                                .fill(egui::Color32::from_rgb(40, 40, 40))
                                .inner_margin(10.0)
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.monospace(link);
                                        
                                        #[cfg(feature = "clipboard")]
                                        if ui.button("📋").clicked() {
                                            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                                let _ = clipboard.set_text(link.clone());
                                            }
                                        }
                                        
                                        #[cfg(not(feature = "clipboard"))]
                                        if ui.button("📋").clicked() {
                                            ui.output_mut(|o| o.copied_text = link.clone());
                                        }
                                    });
                                });
                        }
                        
                        ui.add_space(30.0);
                        
                        if ui.add_sized([500.0, 40.0], egui::Button::new(RichText::new("Join Your Room").size(16.0))).clicked() {
                            if let Some(link) = &self.host_link {
                                if let Ok(info) = ConnectionInfo::from_magic_link(link) {
                                    self.connect_to_server(info, true);
                                }
                            }
                        }
                    });
                }

                ui.add_space(20.0);
                if ui.button("← Back").clicked() {
                    self.screen = Screen::Dashboard;
                    self.host_error = None;
                    self.host_link = None;
                }
            });
        });
    }

    fn render_join_prompt(&mut self, ctx: &Context) {
        CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.heading(RichText::new("Join a Voice Room").size(24.0));
                ui.add_space(30.0);

                ui.vertical(|ui| {
                    ui.set_max_width(500.0);
                    
                    ui.label(RichText::new("Paste the invite link:").size(14.0));
                    ui.add_space(10.0);
                    
                    let response = ui.add_sized(
                        [500.0, 30.0],
                        TextEdit::singleline(&mut self.join_input).hint_text("voirc://...")
                    );
                    
                    ui.add_space(15.0);

                    if let Some(error) = &self.join_error {
                        ui.colored_label(egui::Color32::RED, error);
                        ui.add_space(10.0);
                    }

                    let enter_pressed = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    
                    if ui.add_sized([500.0, 40.0], egui::Button::new(RichText::new("Connect").size(16.0))).clicked() || enter_pressed {
                        self.join_room();
                    }
                });

                ui.add_space(20.0);
                if ui.button("← Back").clicked() {
                    self.screen = Screen::Dashboard;
                    self.join_error = None;
                }
            });
        });
    }

    fn render_settings(&mut self, ctx: &Context) {
        CentralPanel::default().show(ctx, |ui| {
            ui.heading("Settings");
            ui.add_space(20.0);

            ui.horizontal(|ui| {
                ui.label("Display Name:");
                ui.text_edit_singleline(&mut self.settings_name);
            });

            ui.add_space(20.0);

            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    self.config.display_name = self.settings_name.clone();
                    let _ = self.config.save();
                    self.screen = Screen::Dashboard;
                }
                if ui.button("Cancel").clicked() {
                    self.screen = Screen::Dashboard;
                }
            });
        });
    }

    fn render_recent_servers(&mut self, ctx: &Context) {
        CentralPanel::default().show(ctx, |ui| {
            ui.heading("Recent Servers");
            ui.add_space(20.0);

            if self.config.recent_servers.is_empty() {
                ui.label("No recent servers");
            } else {
                let mut selected_connection: Option<String> = None;
                
                for (i, server) in self.config.recent_servers.iter().enumerate() {
                    let response = if i == self.selected_recent {
                        ui.button(
                            RichText::new(format!("{} - {}", server.name, server.last_connected))
                                .strong(),
                        )
                    } else {
                        ui.button(format!("{} - {}", server.name, server.last_connected))
                    };

                    if response.clicked() {
                        selected_connection = Some(server.connection_string.clone());
                    }
                }
                
                if let Some(conn_str) = selected_connection {
                    if let Ok(info) = ConnectionInfo::from_magic_link(&conn_str) {
                        self.connect_to_server(info, false);
                    }
                }
            }

            ui.add_space(20.0);
            if ui.button("Back").clicked() {
                self.screen = Screen::Dashboard;
            }
        });
    }

    fn render_in_call(&mut self, ctx: &Context) {
        if let Some(call_state) = &self.call_state {
            let channel = call_state.channel.clone();
            let nickname = call_state.nickname.clone();
            let state = Arc::clone(&call_state.state);
            let shutdown_tx = call_state.shutdown_tx.clone();
            
            // Top bar
            TopBottomPanel::top("call_header").show(ctx, |ui| {
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(30, 30, 35))
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("🎤").size(20.0));
                            ui.label(RichText::new(&channel).size(16.0).strong());
                            
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add_sized([100.0, 30.0], egui::Button::new("Disconnect")).clicked() {
                                    let _ = shutdown_tx.send(());
                                    self.call_state = None;
                                    self.screen = Screen::Dashboard;
                                }
                            });
                        });
                    });
            });

            // Bottom input bar
            let state_for_input = Arc::clone(&state);
            TopBottomPanel::bottom("call_input").show(ctx, |ui| {
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(30, 30, 35))
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("💬");
                            let _response = ui.add(
                                TextEdit::singleline(&mut self.chat_input)
                                    .hint_text("Send a message...")
                                    .desired_width(ui.available_width() - 10.0),
                            );
                            
                            if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.chat_input.is_empty() {
                                self.send_message();
                            }
                        });
                    });
            });

            // Main content area with sidebar
            let state_for_sidebar = Arc::clone(&state);
            egui::SidePanel::right("voice_panel")
                .resizable(false)
                .exact_width(250.0)
                .show(ctx, |ui| {
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(40, 40, 45))
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.add_space(10.0);
                                
                                let (_total, connected_count) = if let Ok(peer_states) = state_for_sidebar.peer_states.try_read() {
                                    let total = peer_states.len();
                                    let connected = peer_states.values().filter(|p| p.connected).count();
                                    (total, connected)
                                } else {
                                    (0, 0)
                                };
                                
                                ui.label(RichText::new(format!("VOICE — {} online", connected_count)).size(12.0).color(egui::Color32::GRAY));
                                ui.add_space(10.0);
                                ui.separator();
                                ui.add_space(10.0);
                                
                                // Your own user first
                                ui.horizontal(|ui| {
                                    ui.add_space(10.0);
                                    ui.label(RichText::new("🔊").size(18.0));
                                    ui.label(RichText::new(format!("{} (you)", nickname)).size(14.0));
                                });
                                ui.add_space(5.0);

                                // Other peers
                                if let Ok(peer_states) = state_for_sidebar.peer_states.try_read() {
                                    for peer in peer_states.values() {
                                        ui.horizontal(|ui| {
                                            ui.add_space(10.0);
                                            
                                            // Speaking indicator
                                            if peer.speaking {
                                                ui.label(RichText::new("🔊").size(18.0).color(egui::Color32::GREEN));
                                            } else if peer.connected {
                                                ui.label(RichText::new("🔇").size(18.0).color(egui::Color32::GRAY));
                                            } else {
                                                ui.label(RichText::new("⚫").size(18.0).color(egui::Color32::DARK_GRAY));
                                            }
                                            
                                            let color = if peer.connected {
                                                egui::Color32::WHITE
                                            } else {
                                                egui::Color32::GRAY
                                            };
                                            
                                            ui.label(RichText::new(&peer.nickname).size(14.0).color(color));
                                        });
                                        ui.add_space(5.0);
                                    }
                                }
                            });
                        });
                });

            // Chat area
            CentralPanel::default().show(ctx, |ui| {
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(50, 50, 55))
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.add_space(10.0);
                            ui.label(RichText::new("TEXT CHAT").size(12.0).color(egui::Color32::GRAY));
                            ui.add_space(5.0);
                            ui.separator();
                            ui.add_space(10.0);
                            
                            ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .stick_to_bottom(true)
                                .show(ui, |ui| {
                                    if let Ok(messages) = state.messages.try_read() {
                                        for msg in messages.iter() {
                                            // Parse message format: <nickname> message
                                            if let Some(rest) = msg.strip_prefix('<') {
                                                if let Some((nick, text)) = rest.split_once('>') {
                                                    ui.horizontal(|ui| {
                                                        ui.label(RichText::new(nick).strong().color(egui::Color32::from_rgb(100, 150, 255)));
                                                        ui.label(RichText::new(text.trim()));
                                                    });
                                                } else {
                                                    ui.label(msg);
                                                }
                                            } else {
                                                ui.label(RichText::new(msg).color(egui::Color32::GRAY).italics());
                                            }
                                            ui.add_space(5.0);
                                        }
                                    }
                                });
                        });
                    });
            });
        }
    }

    fn start_hosting(&mut self) {
        let port: u16 = match self.host_port.parse() {
            Ok(p) => p,
            Err(_) => {
                self.host_error = Some("Invalid port number".to_string());
                return;
            }
        };

        let channel = self.host_channel.clone();

        // Spawn server
        tokio::spawn(async move {
            if let Err(e) = EmbeddedServer::run(port).await {
                error!("Server error: {}", e);
            }
        });

        // Try UPnP
        let port_clone = port;
        tokio::spawn(async move {
            if let Err(e) = PortForwarder::forward_port(port_clone).await {
                info!("UPnP failed (not critical): {}", e);
            }
        });

        // Get external IP and generate link
        let conn_info = ConnectionInfo::new("127.0.0.1".to_string(), port, channel.clone());
        self.host_link = conn_info.to_magic_link().ok();
        
        // Try to get external IP in background and update link
        let port_clone = port;
        let channel_clone = channel.clone();
        tokio::spawn(async move {
            match PortForwarder::get_external_ip().await {
                Ok(ip) => {
                    let conn_info = ConnectionInfo::new(ip, port_clone, channel_clone);
                    if let Ok(link) = conn_info.to_magic_link() {
                        info!("External magic link: {}", link);
                        // TODO: Could update UI with external IP link
                    }
                }
                Err(e) => error!("Failed to get external IP: {}", e),
            }
        });

        // Don't auto-connect - let user click "Join Room" button
        self.host_error = None;
    }

    fn join_room(&mut self) {
        match ConnectionInfo::from_magic_link(&self.join_input) {
            Ok(info) => {
                self.connect_to_server(info, false);
            }
            Err(e) => {
                self.join_error = Some(format!("Invalid link: {}", e));
            }
        }
    }

    fn connect_to_server(&mut self, conn_info: ConnectionInfo, is_host: bool) {
        let state = AppState::new();
        let nickname = self.config.display_name.clone();

        // Save to recent servers
        let mut config = self.config.clone();
        let server_name = if is_host {
            format!("My Server ({})", conn_info.channel)
        } else {
            format!("{} ({})", conn_info.host, conn_info.channel)
        };
        
        if let Ok(link) = conn_info.to_magic_link() {
            config.add_recent_server(server_name, link);
            self.config = config;
        }

        // Setup audio
        let mixer = match VoiceMixer::new() {
            Ok(m) => Arc::new(m),
            Err(e) => {
                error!("Failed to create mixer: {}", e);
                return;
            }
        };

        let (mic_tx, mic_rx) = mpsc::unbounded_channel();
        let (mix_tx, mut mix_rx) = mpsc::unbounded_channel();

        let (input_stream, output_stream) = match (
            mixer.start_input(mic_tx),
            mixer.start_output()
        ) {
            (Ok(input), Ok(output)) => (input, output),
            _ => {
                error!("Failed to start audio streams");
                return;
            }
        };

        // Mixer task
        let mixer_clone = Arc::clone(&mixer);
        tokio::spawn(async move {
            while let Some(packet) = mix_rx.recv().await {
                mixer_clone.add_peer_audio(packet).await;
            }
        });

        // Channel for sending chat messages from UI
        let (send_message_tx, mut send_message_rx) = mpsc::unbounded_channel::<String>();
        let (shutdown_tx, shutdown_rx) = mpsc::unbounded_channel::<()>();

        let state_clone = Arc::clone(&state);
        let nickname_clone = nickname.clone();
        let conn_info_clone = conn_info.clone();
        
        // Main call task
        tokio::spawn(async move {
            // Add timeout for initial connection
            let connect_timeout = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                IrcClient::connect(
                    conn_info_clone.server_address(),
                    nickname_clone.clone(),
                    conn_info_clone.channel.clone(),
                    state_clone.clone(),
                )
            );
            
            match connect_timeout.await {
                Ok(Ok((irc_client, irc_stream, irc_events))) => {
                    let irc = Arc::new(irc_client);
                    
                    // IRC stream handler with error recovery
                    let irc_clone = Arc::clone(&irc);
                    let state_for_stream = Arc::clone(&state_clone);
                    tokio::spawn(async move {
                        if let Err(e) = irc_clone.run(irc_stream).await {
                            error!("IRC stream error: {}", e);
                            state_for_stream.add_message(format!("IRC error: {}", e)).await;
                        }
                    });

                    // Message sender task
                    let irc_clone = Arc::clone(&irc);
                    let channel_clone = conn_info_clone.channel.clone();
                    let nickname_for_echo = nickname_clone.clone();
                    let state_for_echo = Arc::clone(&state_clone);
                    tokio::spawn(async move {
                        while let Some(msg) = send_message_rx.recv().await {
                            if let Err(e) = irc_clone.send_message(&channel_clone, &msg) {
                                error!("Failed to send message: {}", e);
                            } else {
                                state_for_echo
                                    .add_message(format!("<{}> {}", nickname_for_echo, msg))
                                    .await;
                            }
                        }
                    });

                    state_clone
                        .add_message(format!("Connected as {}", nickname_clone))
                        .await;
                    if is_host {
                        state_clone
                            .add_message("🏠 You are hosting this room".to_string())
                            .await;
                    }

                    // Handle IRC events
                    Self::handle_irc_events(
                        irc,
                        irc_events,
                        state_clone,
                        nickname_clone,
                        mix_tx,
                        mic_rx,
                        shutdown_rx,
                    )
                    .await;
                }
                Ok(Err(e)) => {
                    error!("IRC connection failed: {}", e);
                    state_clone.add_message(format!("Connection failed: {}", e)).await;
                }
                Err(_) => {
                    error!("IRC connection timed out after 10 seconds");
                    state_clone.add_message("Connection timed out".to_string()).await;
                }
            }
        });

        self.call_state = Some(Arc::new(CallState {
            state,
            send_message_tx,
            shutdown_tx,
            channel: conn_info.channel,
            nickname,
            _mixer: mixer,
            _input_stream: input_stream,
            _output_stream: output_stream,
        }));
        
        self.screen = Screen::InCall;
    }

    async fn handle_irc_events(
        irc: Arc<IrcClient>,
        mut irc_events: mpsc::UnboundedReceiver<IrcEvent>,
        state: Arc<AppState>,
        nickname: String,
        mix_tx: mpsc::UnboundedSender<Vec<u8>>,
        mut mic_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        mut shutdown_rx: mpsc::UnboundedReceiver<()>,
    ) {
        let (ice_out_tx, mut ice_out_rx) = mpsc::unbounded_channel::<InternalSignal>();
        let (reconnect_tx, mut reconnect_rx) = mpsc::unbounded_channel::<String>();
        let peers: Arc<RwLock<HashMap<String, Arc<WebRtcPeer>>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // Forward outgoing WebRTC signals to IRC
        let irc_clone = Arc::clone(&irc);
        tokio::spawn(async move {
            while let Some(signal) = ice_out_rx.recv().await {
                match signal {
                    InternalSignal::WebRtc(target, webrtc_signal) => {
                        if let Ok(json) = serde_json::to_string(&webrtc_signal) {
                            let _ = irc_clone.send_webrtc_signal(&target, &json);
                        }
                    }
                    InternalSignal::Reconnect(nick) => {
                        let _ = reconnect_tx.send(nick);
                    }
                }
            }
        });

        // Forward mic audio to all connected peers
        let peers_clone = Arc::clone(&peers);
        tokio::spawn(async move {
            while let Some(packet) = mic_rx.recv().await {
                let peers_read = peers_clone.read().await;
                for peer in peers_read.values() {
                    let _ = peer.send_audio(&packet).await;
                }
            }
        });

        // Main event loop
        loop {
            tokio::select! {
                // Shutdown signal
                Some(_) = shutdown_rx.recv() => {
                    info!("Shutdown requested, cleaning up");
                    break;
                }
                
                // Handle reconnection attempts
                Some(nick) = reconnect_rx.recv() => {
                    info!("Reconnection requested for {}", nick);
                    
                    // Remove dead peer
                    peers.write().await.remove(&nick);
                    
                    // Wait before redialing (backoff)
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    
                    // Only redial if peer is still in channel
                    let peer_exists = state.peer_states.read().await.contains_key(&nick);
                    if !peer_exists {
                        continue;
                    }
                    
                    // Alphabetical ordering determines who initiates
                    if nickname < nick {
                        info!("Re-initiating call to {}", nick);
                        
                        match WebRtcPeer::new(
                            nick.clone(),
                            Arc::clone(&state),
                            mix_tx.clone(),
                            ice_out_tx.clone(),
                        )
                        .await
                        {
                            Ok(peer) => {
                                match peer.create_offer().await {
                                    Ok(offer_json) => {
                                        peers.write().await.insert(nick.clone(), Arc::new(peer));
                                        let _ = irc.send_webrtc_signal(&nick, &offer_json);
                                        state.add_message(format!("-> Reconnecting to {}", nick)).await;
                                    }
                                    Err(e) => {
                                        error!("Failed to create offer during reconnect: {}", e);
                                    }
                                }
                            }
                            Err(e) => {
                                error!("Failed to create peer during reconnect: {}", e);
                            }
                        }
                    }
                }
                
                // Handle IRC events
                Some(event) = irc_events.recv() => {
                    match event {
                        IrcEvent::UserJoined(nick) => {
                            info!("User joined: {}", nick);
                            state.update_peer_state(nick.clone(), false, false).await;

                            // Alphabetical ordering to prevent dual offers
                            if nickname < nick {
                                info!("Initiating call to {}", nick);
                                
                                match WebRtcPeer::new(
                                    nick.clone(),
                                    Arc::clone(&state),
                                    mix_tx.clone(),
                                    ice_out_tx.clone(),
                                )
                                .await
                                {
                                    Ok(peer) => {
                                        match peer.create_offer().await {
                                            Ok(offer_json) => {
                                                peers.write().await.insert(nick.clone(), Arc::new(peer));
                                                let _ = irc.send_webrtc_signal(&nick, &offer_json);
                                                state.add_message(format!("-> Calling {}", nick)).await;
                                            }
                                            Err(e) => {
                                                error!("Failed to create offer: {}", e);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        error!("Failed to create peer: {}", e);
                                    }
                                }
                            } else {
                                info!("Waiting for call from {}", nick);
                            }
                        }
                        
                        IrcEvent::UserLeft(nick) => {
                            info!("User left: {}", nick);
                            peers.write().await.remove(&nick);
                            state.remove_peer(&nick).await;
                        }
                        
                        IrcEvent::WebRtcSignal { from, payload } => {
                            match serde_json::from_str::<WebRtcSignal>(&payload) {
                                Ok(WebRtcSignal::Offer { sdp }) => {
                                    state.add_message(format!("<- Offer from {}", from)).await;

                                    match WebRtcPeer::new(
                                        from.clone(),
                                        Arc::clone(&state),
                                        mix_tx.clone(),
                                        ice_out_tx.clone(),
                                    )
                                    .await
                                    {
                                        Ok(peer) => {
                                            match peer.handle_offer(sdp).await {
                                                Ok(answer_json) => {
                                                    peers.write().await.insert(from.clone(), Arc::new(peer));
                                                    let _ = irc.send_webrtc_signal(&from, &answer_json);
                                                }
                                                Err(e) => {
                                                    error!("Failed to handle offer from {}: {}", from, e);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            error!("Failed to create peer for {}: {}", from, e);
                                        }
                                    }
                                }
                                
                                Ok(WebRtcSignal::Answer { sdp }) => {
                                    state.add_message(format!("<- Answer from {}", from)).await;
                                    
                                    if let Some(peer) = peers.read().await.get(&from) {
                                        if let Err(e) = peer.handle_answer(sdp).await {
                                            error!("Failed to handle answer from {}: {}", from, e);
                                        }
                                    } else {
                                        error!("Received answer from unknown peer: {}", from);
                                    }
                                }
                                
                                Ok(WebRtcSignal::IceCandidate {
                                    candidate,
                                    sdp_mid,
                                    sdp_mline_index,
                                }) => {
                                    if let Some(peer) = peers.read().await.get(&from) {
                                        if let Err(e) = peer.add_ice_candidate(candidate, sdp_mid, sdp_mline_index).await {
                                            error!("Failed to add ICE candidate from {}: {}", from, e);
                                        }
                                    }
                                }
                                
                                Err(e) => {
                                    error!("Failed to parse WebRTC signal from {}: {}", from, e);
                                }
                            }
                        }
                        
                        IrcEvent::ChatMessage { .. } => {
                            // Already handled by IRC client adding to state
                        }
                        
                        IrcEvent::ConnectionFailed(reason) => {
                            error!("IRC connection failed: {}", reason);
                            state.add_message(format!("Connection failed: {}", reason)).await;
                            break;
                        }
                    }
                }
                
                else => {
                    // All channels closed
                    break;
                }
            }
        }
        
        info!("IRC event handler shutting down");
    }

    fn send_message(&mut self) {
        if let Some(call_state) = &self.call_state {
            if !self.chat_input.is_empty() {
                let msg = self.chat_input.clone();
                let _ = call_state.send_message_tx.send(msg);
                self.chat_input.clear();
            }
        }
    }
}

impl eframe::App for VoircApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));

        match self.screen {
            Screen::Dashboard => self.render_dashboard(ctx),
            Screen::HostSetup => self.render_host_setup(ctx),
            Screen::JoinPrompt => self.render_join_prompt(ctx),
            Screen::Settings => self.render_settings(ctx),
            Screen::RecentServers => self.render_recent_servers(ctx),
            Screen::InCall => self.render_in_call(ctx),
        }
    }
}
