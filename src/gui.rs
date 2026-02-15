// src/gui.rs

use egui::{CentralPanel, Context, RichText, ScrollArea, TextEdit, TopBottomPanel};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info};

use crate::config::UserConfig;
use crate::irc_client::{IrcClient, IrcEvent};
use crate::irc_server::EmbeddedServer;
use crate::magic_link::ConnectionInfo;
use crate::state::AppState;
use crate::upnp::PortForwarder;
use crate::voice_mixer::{PeerDecoders, VoiceMixer};
use crate::webrtc_peer::{InternalSignal, ReceivedFile, WebRtcPeer, WebRtcSignal};

/// Open a file or folder with the OS default handler
fn open_path(path: &Path) {
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

// ── Screens ──

#[derive(Debug, Clone, PartialEq)]
enum Screen {
    Dashboard,
    HostSetup,
    JoinPrompt,
    Settings,
    RecentServers,
    InCall,
}

// ── Commands from GUI → event loop ──

enum CallCommand {
    SendMessage(String),
    SwitchChannel(String),
    CreateChannel(String),
    SendFile { name: String, data: Vec<u8> },
    Shutdown,
}

// ── Call state shared between GUI and async tasks ──

struct CallState {
    state: Arc<AppState>,
    command_tx: mpsc::UnboundedSender<CallCommand>,
    channels: Arc<RwLock<Vec<String>>>,
    current_channel: Arc<RwLock<String>>,
    nickname: String,
    _mixer: Arc<VoiceMixer>,
    _input_stream: cpal::Stream,
    _output_stream: cpal::Stream,
}

// ── App ──

pub struct VoircApp {
    config: UserConfig,
    screen: Screen,

    // Host
    host_port: String,
    host_channels: String,
    host_error: Option<String>,
    host_link: Option<String>,
    host_link_external: Arc<RwLock<Option<String>>>,
    link_copied: bool,

    // Join
    join_input: String,
    join_error: Option<String>,

    // Settings
    settings_name: String,
    selected_recent: usize,

    // In Call
    chat_input: String,
    new_channel_input: String,
    call_state: Option<Arc<CallState>>,
    file_status: Option<String>,
}

impl VoircApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, config: UserConfig) -> Self {
        Self {
            config,
            screen: Screen::Dashboard,
            host_port: "6667".to_string(),
            host_channels: "#general, #gaming".to_string(),
            host_error: None,
            host_link: None,
            host_link_external: Arc::new(RwLock::new(None)),
            link_copied: false,
            join_input: String::new(),
            join_error: None,
            settings_name: String::new(),
            selected_recent: 0,
            chat_input: String::new(),
            new_channel_input: String::new(),
            call_state: None,
            file_status: None,
        }
    }

    // ── Dashboard ──

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
                let sz = egui::vec2(300.0, 50.0);
                if ui.add_sized(sz, egui::Button::new(RichText::new("🏠 Host a Room").size(16.0))).clicked() {
                    self.screen = Screen::HostSetup;
                }
                ui.add_space(10.0);
                if ui.add_sized(sz, egui::Button::new(RichText::new("🔗 Join a Room").size(16.0))).clicked() {
                    self.screen = Screen::JoinPrompt;
                }
                ui.add_space(10.0);
                if !self.config.recent_servers.is_empty() {
                    if ui.add_sized(sz, egui::Button::new(RichText::new("📋 Recent Servers").size(16.0))).clicked() {
                        self.screen = Screen::RecentServers;
                    }
                    ui.add_space(10.0);
                }
                if ui.add_sized(sz, egui::Button::new(RichText::new("⚙️ Settings").size(16.0))).clicked() {
                    self.settings_name = self.config.display_name.clone();
                    self.screen = Screen::Settings;
                }
            });
        });
    }

    // ── Host Setup ──

    fn render_host_setup(&mut self, ctx: &Context) {
        CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.heading(RichText::new("Host a Voice Room").size(24.0));
                ui.add_space(30.0);

                if self.host_link.is_none() {
                    ui.vertical(|ui| {
                        ui.set_max_width(400.0);
                        ui.label(RichText::new("Port").size(14.0));
                        ui.text_edit_singleline(&mut self.host_port);
                        ui.add_space(15.0);
                        ui.label(RichText::new("Channels (comma-separated)").size(14.0));
                        ui.text_edit_singleline(&mut self.host_channels);
                        ui.add_space(20.0);

                        if let Some(err) = &self.host_error {
                            ui.colored_label(egui::Color32::RED, err);
                            ui.add_space(10.0);
                        }

                        if ui.add_sized([400.0, 40.0], egui::Button::new(RichText::new("🚀 Start Server").size(16.0))).clicked() {
                            self.start_hosting();
                        }
                    });
                } else {
                    let display_link = if let Ok(guard) = self.host_link_external.try_read() {
                        guard.clone().or_else(|| self.host_link.clone())
                    } else {
                        self.host_link.clone()
                    };

                    ui.vertical(|ui| {
                        ui.set_max_width(500.0);
                        ui.label(RichText::new("✅ Server is running!").size(18.0).color(egui::Color32::GREEN));
                        ui.add_space(20.0);
                        ui.label(RichText::new("Share this link with friends:").size(14.0));
                        ui.add_space(10.0);

                        if let Some(link) = &display_link {
                            egui::Frame::none()
                                .fill(egui::Color32::from_rgb(40, 40, 40))
                                .inner_margin(10.0)
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.monospace(link);
                                        if ui.button("📋 Copy").clicked() {
                                            ui.output_mut(|o| o.copied_text = link.clone());
                                            self.link_copied = true;
                                        }
                                    });
                                });
                            if self.link_copied {
                                ui.colored_label(egui::Color32::GREEN, "Copied!");
                            }
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
                    self.link_copied = false;
                }
            });
        });
    }

    // ── Join ──

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
                    let resp = ui.add_sized([500.0, 30.0], TextEdit::singleline(&mut self.join_input).hint_text("voirc://..."));
                    ui.add_space(15.0);

                    if let Some(err) = &self.join_error {
                        ui.colored_label(egui::Color32::RED, err);
                        ui.add_space(10.0);
                    }

                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.add_sized([500.0, 40.0], egui::Button::new(RichText::new("Connect").size(16.0))).clicked() || enter {
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

    // ── Settings ──

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

    // ── Recent Servers ──

    fn render_recent_servers(&mut self, ctx: &Context) {
        CentralPanel::default().show(ctx, |ui| {
            ui.heading("Recent Servers");
            ui.add_space(20.0);

            if self.config.recent_servers.is_empty() {
                ui.label("No recent servers");
            } else {
                let mut selected = None;
                for (i, server) in self.config.recent_servers.iter().enumerate() {
                    let label = format!("{} - {}", server.name, server.last_connected);
                    let btn = if i == self.selected_recent {
                        ui.button(RichText::new(label).strong())
                    } else {
                        ui.button(label)
                    };
                    if btn.clicked() {
                        selected = Some(server.connection_string.clone());
                    }
                }
                if let Some(conn) = selected {
                    if let Ok(info) = ConnectionInfo::from_magic_link(&conn) {
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

    // ── In Call ──

    fn render_in_call(&mut self, ctx: &Context) {
        let mut disconnect = false;
        let mut send_msg = false;

        if let Some(call_state) = &self.call_state {
            let nickname = call_state.nickname.clone();
            let state = Arc::clone(&call_state.state);
            let cmd_tx = call_state.command_tx.clone();
            let channels = call_state.channels.try_read()
                .map(|g| g.clone())
                .unwrap_or_default();
            let current_channel_lock = Arc::clone(&call_state.current_channel);

            let current_channel = current_channel_lock
                .try_read()
                .map(|g| g.clone())
                .unwrap_or_default();

            // Refresh speaking flags
            {
                let s = Arc::clone(&state);
                tokio::spawn(async move { s.refresh_speaking().await });
            }

            // Handle file drag-and-drop
            let dropped: Vec<_> = ctx.input(|i| i.raw.dropped_files.clone());
            for file in dropped {
                if let Some(path) = &file.path {
                    match std::fs::read(path) {
                        Ok(data) => {
                            let name = path.file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| "file".to_string());
                            let _ = cmd_tx.send(CallCommand::SendFile { name, data });
                        }
                        Err(e) => error!("Failed to read dropped file: {}", e),
                    }
                }
            }

            // Top bar
            let cmd_tx_top = cmd_tx.clone();
            let file_status = self.file_status.clone();
            TopBottomPanel::top("call_header").show(ctx, |ui| {
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(30, 30, 35))
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("🎤").size(20.0));
                            ui.label(RichText::new(&current_channel).size(16.0).strong());

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add_sized([100.0, 30.0], egui::Button::new("Disconnect")).clicked() {
                                    let _ = cmd_tx_top.send(CallCommand::Shutdown);
                                    disconnect = true;
                                }

                                if let Some(status) = &file_status {
                                    ui.label(RichText::new(status).size(11.0).color(egui::Color32::LIGHT_GREEN));
                                }
                            });
                        });
                    });
            });

            // Bottom input bar
            TopBottomPanel::bottom("call_input").show(ctx, |ui| {
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(30, 30, 35))
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("💬");
                            let _resp = ui.add(
                                TextEdit::singleline(&mut self.chat_input)
                                    .hint_text("Send a message... (drag file to share)")
                                    .desired_width(ui.available_width() - 10.0),
                            );
                            if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.chat_input.is_empty() {
                                send_msg = true;
                            }
                        });
                    });
            });

            // Left sidebar — channel list
            let cmd_tx_ch = call_state.command_tx.clone();
            let cmd_tx_new = call_state.command_tx.clone();
            egui::SidePanel::left("channel_panel")
                .resizable(false)
                .exact_width(160.0)
                .show(ctx, |ui| {
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(30, 30, 35))
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.add_space(10.0);
                                ui.label(RichText::new("CHANNELS").size(12.0).color(egui::Color32::GRAY));
                                ui.add_space(8.0);
                                ui.separator();
                                ui.add_space(8.0);

                                for ch in &channels {
                                    let is_current = *ch == current_channel;
                                    let text = if is_current {
                                        RichText::new(format!("▶ {}", ch)).size(14.0).strong().color(egui::Color32::WHITE)
                                    } else {
                                        RichText::new(format!("  {}", ch)).size(14.0).color(egui::Color32::GRAY)
                                    };

                                    if ui.add(egui::Label::new(text).sense(egui::Sense::click())).clicked() && !is_current {
                                        let _ = cmd_tx_ch.send(CallCommand::SwitchChannel(ch.clone()));
                                    }
                                    ui.add_space(4.0);
                                }

                                ui.add_space(10.0);
                                ui.separator();
                                ui.add_space(6.0);

                                ui.horizontal(|ui| {
                                    let resp = ui.add(
                                        TextEdit::singleline(&mut self.new_channel_input)
                                            .hint_text("#new")
                                            .desired_width(100.0),
                                    );
                                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                    if (ui.small_button("+").clicked() || enter) && !self.new_channel_input.is_empty() {
                                        let mut name = self.new_channel_input.trim().to_string();
                                        if !name.starts_with('#') { name = format!("#{}", name); }
                                        let _ = cmd_tx_new.send(CallCommand::CreateChannel(name));
                                        self.new_channel_input.clear();
                                    }
                                });
                            });
                        });
                });

            // Right sidebar — voice panel + files
            let state_sb = Arc::clone(&state);
            egui::SidePanel::right("voice_panel")
                .resizable(false)
                .exact_width(220.0)
                .show(ctx, |ui| {
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(40, 40, 45))
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.add_space(10.0);
                                let connected = state_sb.peer_states.try_read()
                                    .map(|p| p.values().filter(|v| v.connected).count())
                                    .unwrap_or(0);

                                ui.label(RichText::new(format!("VOICE — {} online", connected)).size(12.0).color(egui::Color32::GRAY));
                                ui.add_space(10.0);
                                ui.separator();
                                ui.add_space(10.0);

                                // You
                                ui.horizontal(|ui| {
                                    ui.add_space(10.0);
                                    ui.label(RichText::new("🔊").size(18.0));
                                    ui.label(RichText::new(format!("{} (you)", nickname)).size(14.0));
                                });
                                ui.add_space(5.0);

                                // Peers
                                if let Ok(peers) = state_sb.peer_states.try_read() {
                                    for peer in peers.values() {
                                        ui.horizontal(|ui| {
                                            ui.add_space(10.0);
                                            if peer.speaking {
                                                ui.label(RichText::new("🔊").size(18.0).color(egui::Color32::GREEN));
                                            } else if peer.connected {
                                                ui.label(RichText::new("🔇").size(18.0).color(egui::Color32::GRAY));
                                            } else {
                                                ui.label(RichText::new("⚫").size(18.0).color(egui::Color32::DARK_GRAY));
                                            }
                                            let color = if peer.connected { egui::Color32::WHITE } else { egui::Color32::GRAY };
                                            ui.label(RichText::new(&peer.nickname).size(14.0).color(color));
                                        });
                                        ui.add_space(5.0);
                                    }
                                }

                                // Files section
                                if let Ok(files) = state_sb.received_files.try_read() {
                                    if !files.is_empty() {
                                        ui.add_space(15.0);
                                        ui.separator();
                                        ui.add_space(10.0);
                                        ui.label(RichText::new("📁 FILES").size(12.0).color(egui::Color32::GRAY));
                                        ui.add_space(8.0);

                                        ScrollArea::vertical()
                                            .id_salt("files_scroll")
                                            .max_height(200.0)
                                            .show(ui, |ui| {
                                                for file in files.iter().rev() {
                                                    ui.horizontal(|ui| {
                                                        ui.add_space(5.0);
                                                        ui.vertical(|ui| {
                                                            ui.label(RichText::new(&file.name).size(12.0).strong());
                                                            ui.label(RichText::new(format!("from {} · {} KB", file.from, file.size / 1024)).size(10.0).color(egui::Color32::GRAY));
                                                        });
                                                        if ui.small_button("Open").clicked() {
                                                            open_path(&file.path);
                                                        }
                                                    });
                                                    ui.add_space(4.0);
                                                }
                                            });
                                    }
                                }
                            });
                        });
                });

            // Center — chat
            CentralPanel::default().show(ctx, |ui| {
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(50, 50, 55))
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.add_space(10.0);
                            ui.label(RichText::new(format!("TEXT CHAT — {}", current_channel)).size(12.0).color(egui::Color32::GRAY));
                            ui.add_space(5.0);
                            ui.separator();
                            ui.add_space(10.0);

                            ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .stick_to_bottom(true)
                                .show(ui, |ui| {
                                    if let Ok(messages) = state.messages.try_read() {
                                        if let Some(msgs) = messages.get(&current_channel) {
                                            for msg in msgs.iter() {
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
                                    }
                                });
                        });
                    });
            });
        }

        if disconnect {
            self.call_state = None;
            self.screen = Screen::Dashboard;
        }
        if send_msg {
            self.send_message();
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

        let channels: Vec<String> = self.host_channels
            .split(',')
            .map(|s| {
                let s = s.trim().to_string();
                if s.starts_with('#') { s } else { format!("#{}", s) }
            })
            .filter(|s| s.len() > 1)
            .collect();

        if channels.is_empty() {
            self.host_error = Some("Need at least one channel".to_string());
            return;
        }

        tokio::spawn(async move {
            if let Err(e) = EmbeddedServer::run(port).await {
                error!("Server error: {}", e);
            }
        });

        let port_c = port;
        tokio::spawn(async move {
            if let Err(e) = PortForwarder::forward_port(port_c).await {
                info!("UPnP failed (not critical): {}", e);
            }
        });

        let conn_info = ConnectionInfo::new("127.0.0.1".to_string(), port, channels.clone());
        self.host_link = conn_info.to_magic_link().ok();
        self.link_copied = false;

        let external = Arc::clone(&self.host_link_external);
        let ch = channels;
        tokio::spawn(async move {
            if let Ok(ip) = PortForwarder::get_external_ip().await {
                let info = ConnectionInfo::new(ip, port, ch);
                if let Ok(link) = info.to_magic_link() {
                    info!("External magic link: {}", link);
                    *external.write().await = Some(link);
                }
            }
        });

        self.host_error = None;
    }

    fn join_room(&mut self) {
        match ConnectionInfo::from_magic_link(&self.join_input) {
            Ok(info) => self.connect_to_server(info, false),
            Err(e) => self.join_error = Some(format!("Invalid link: {}", e)),
        }
    }

    fn connect_to_server(&mut self, conn_info: ConnectionInfo, is_host: bool) {
        let state = AppState::new();
        let nickname = self.config.display_name.clone();
        let channels_vec = conn_info.channels.clone();
        let default_channel = conn_info.default_channel().to_string();

        // Save recent
        let mut config = self.config.clone();
        let name = if is_host {
            format!("My Server ({})", channels_vec.join(", "))
        } else {
            format!("{} ({})", conn_info.host, channels_vec.join(", "))
        };
        if let Ok(link) = conn_info.to_magic_link() {
            config.add_recent_server(name, link);
            self.config = config;
        }

        // Audio
        let mixer = match VoiceMixer::new() {
            Ok(m) => Arc::new(m),
            Err(e) => { error!("Mixer: {}", e); return; }
        };

        let (mic_tx, mic_rx) = mpsc::unbounded_channel();
        let (mix_tx, mut mix_rx) = mpsc::unbounded_channel::<(String, Vec<u8>)>();

        let (input_stream, output_stream) = match (mixer.start_input(mic_tx), mixer.start_output()) {
            (Ok(i), Ok(o)) => (i, o),
            _ => { error!("Audio streams failed"); return; }
        };

        // Mixer task
        let mixer_c = Arc::clone(&mixer);
        let state_mix = Arc::clone(&state);
        tokio::spawn(async move {
            let mut decoders = PeerDecoders::new();
            while let Some((nick, packet)) = mix_rx.recv().await {
                if let Some(pcm) = decoders.decode(&nick, &packet) {
                    mixer_c.queue_audio(pcm).await;
                    state_mix.mark_speaking(&nick).await;
                }
            }
        });

        // Command channel
        let (command_tx, command_rx) = mpsc::unbounded_channel::<CallCommand>();

        // File receive channel
        let (file_tx, file_rx) = mpsc::unbounded_channel::<ReceivedFile>();

        let current_channel = Arc::new(RwLock::new(default_channel.clone()));
        let channels = Arc::new(RwLock::new(channels_vec));

        let state_c = Arc::clone(&state);
        let nick_c = nickname.clone();
        let conn_c = conn_info.clone();
        let cur_ch = Arc::clone(&current_channel);
        let channels_for_loop = Arc::clone(&channels);

        // Main call task
        tokio::spawn(async move {
            let timeout = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                IrcClient::connect(
                    conn_c.server_address(),
                    nick_c.clone(),
                    default_channel.clone(),
                    state_c.clone(),
                ),
            );

            match timeout.await {
                Ok(Ok((irc_client, irc_stream, irc_events))) => {
                    let irc = Arc::new(irc_client);

                    let irc_c = Arc::clone(&irc);
                    let state_s = Arc::clone(&state_c);
                    let cur_ch_s = Arc::clone(&cur_ch);
                    tokio::spawn(async move {
                        if let Err(e) = irc_c.run(irc_stream).await {
                            error!("IRC stream: {}", e);
                            let ch = cur_ch_s.read().await.clone();
                            state_s.add_message(&ch, format!("IRC error: {}", e)).await;
                        }
                    });

                    let ch = cur_ch.read().await.clone();
                    state_c.add_message(&ch, format!("Connected as {}", nick_c)).await;
                    if is_host {
                        state_c.add_message(&ch, "🏠 You are hosting this room".to_string()).await;
                    }

                    Self::event_loop(
                        irc, irc_events, state_c, nick_c,
                        mix_tx, mic_rx, command_rx, file_tx, file_rx,
                        cur_ch, channels_for_loop,
                    ).await;
                }
                Ok(Err(e)) => {
                    let ch = cur_ch.read().await.clone();
                    state_c.add_message(&ch, format!("Connection failed: {}", e)).await;
                }
                Err(_) => {
                    let ch = cur_ch.read().await.clone();
                    state_c.add_message(&ch, "Connection timed out".to_string()).await;
                }
            }
        });

        self.call_state = Some(Arc::new(CallState {
            state,
            command_tx,
            channels,
            current_channel,
            nickname,
            _mixer: mixer,
            _input_stream: input_stream,
            _output_stream: output_stream,
        }));
        self.screen = Screen::InCall;
        self.file_status = None;
    }

    // ── Event Loop ──

    async fn event_loop(
        irc: Arc<IrcClient>,
        mut irc_events: mpsc::UnboundedReceiver<IrcEvent>,
        state: Arc<AppState>,
        nickname: String,
        mix_tx: mpsc::UnboundedSender<(String, Vec<u8>)>,
        mut mic_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        mut command_rx: mpsc::UnboundedReceiver<CallCommand>,
        file_tx: mpsc::UnboundedSender<ReceivedFile>,
        mut file_rx: mpsc::UnboundedReceiver<ReceivedFile>,
        current_channel: Arc<RwLock<String>>,
        channels: Arc<RwLock<Vec<String>>>,
    ) {
        let (ice_out_tx, mut ice_out_rx) = mpsc::unbounded_channel::<InternalSignal>();
        let (reconnect_tx, mut reconnect_rx) = mpsc::unbounded_channel::<String>();
        let peers: Arc<RwLock<HashMap<String, Arc<WebRtcPeer>>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // Forward WebRTC signals → IRC
        let irc_c = Arc::clone(&irc);
        tokio::spawn(async move {
            while let Some(sig) = ice_out_rx.recv().await {
                match sig {
                    InternalSignal::WebRtc(target, ws) => {
                        if let Ok(json) = serde_json::to_string(&ws) {
                            let _ = irc_c.send_webrtc_signal(&target, &json);
                        }
                    }
                    InternalSignal::Reconnect(nick) => {
                        let _ = reconnect_tx.send(nick);
                    }
                }
            }
        });

        // Fan-out mic → all peers
        let peers_mic = Arc::clone(&peers);
        tokio::spawn(async move {
            while let Some(pkt) = mic_rx.recv().await {
                let r = peers_mic.read().await;
                for p in r.values() {
                    let _ = p.send_audio(&pkt).await;
                }
            }
        });

        loop {
            tokio::select! {
                // Commands from GUI
                Some(cmd) = command_rx.recv() => {
                    match cmd {
                        CallCommand::SendMessage(text) => {
                            let ch = current_channel.read().await.clone();
                            if let Err(e) = irc.send_message(&ch, &text) {
                                error!("Send: {}", e);
                            } else {
                                state.add_message(&ch, format!("<{}> {}", nickname, text)).await;
                            }
                        }
                        CallCommand::SwitchChannel(new_ch) => {
                            let old_ch = current_channel.read().await.clone();
                            info!("Switching {} → {}", old_ch, new_ch);

                            // Tear down peers
                            let mut pw = peers.write().await;
                            for (_, p) in pw.drain() {
                                p.close().await;
                            }
                            drop(pw);
                            state.clear_peers().await;

                            // Switch IRC channels
                            let _ = irc.part_channel(&old_ch);
                            let _ = irc.join_channel(&new_ch);
                            *current_channel.write().await = new_ch.clone();

                            state.add_message(&new_ch, format!("Joined {}", new_ch)).await;
                        }
                        CallCommand::CreateChannel(new_ch) => {
                            let mut ch_list = channels.write().await;
                            if !ch_list.contains(&new_ch) {
                                ch_list.push(new_ch.clone());
                                drop(ch_list);

                                // Tear down peers and switch
                                let old_ch = current_channel.read().await.clone();
                                let mut pw = peers.write().await;
                                for (_, p) in pw.drain() {
                                    p.close().await;
                                }
                                drop(pw);
                                state.clear_peers().await;

                                let _ = irc.part_channel(&old_ch);
                                let _ = irc.join_channel(&new_ch);
                                *current_channel.write().await = new_ch.clone();

                                state.add_message(&new_ch, format!("Created and joined {}", new_ch)).await;
                            }
                        }
                        CallCommand::SendFile { name, data } => {
                            let ch = current_channel.read().await.clone();
                            let size = data.len();
                            let r = peers.read().await;
                            let mut ok = 0usize;
                            for p in r.values() {
                                if p.send_file(&name, &data).await.is_ok() {
                                    ok += 1;
                                }
                            }
                            let kb = size / 1024;
                            state.add_message(&ch, format!(
                                "📎 You shared {} ({} KB) → {} peers", name, kb, ok
                            )).await;
                        }
                        CallCommand::Shutdown => {
                            info!("Shutdown");
                            break;
                        }
                    }
                }

                // Received files from peers
                Some(file) = file_rx.recv() => {
                    let ch = current_channel.read().await.clone();
                    let size = file.data.len();
                    let kb = size / 1024;

                    // Save to downloads
                    let save_dir = dirs::download_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
                    let save_path = save_dir.join(&file.name);
                    match std::fs::write(&save_path, &file.data) {
                        Ok(_) => {
                            state.add_received_file(
                                file.from.clone(), file.name.clone(), size, save_path.clone(),
                            ).await;
                            state.add_message(&ch, format!(
                                "📎 {} shared {} ({} KB)",
                                file.from, file.name, kb,
                            )).await;
                        }
                        Err(e) => {
                            state.add_message(&ch, format!(
                                "📎 {} shared {} ({} KB) — save failed: {}",
                                file.from, file.name, kb, e
                            )).await;
                        }
                    }
                }

                // Reconnects
                Some(nick) = reconnect_rx.recv() => {
                    info!("Reconnect: {}", nick);
                    peers.write().await.remove(&nick);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                    if !state.peer_states.read().await.contains_key(&nick) { continue; }

                    if nickname < nick {
                        match WebRtcPeer::new(
                            nick.clone(), Arc::clone(&state), mix_tx.clone(),
                            ice_out_tx.clone(), file_tx.clone(),
                        ).await {
                            Ok(peer) => {
                                if let Ok(offer) = peer.create_offer().await {
                                    peers.write().await.insert(nick.clone(), Arc::new(peer));
                                    let _ = irc.send_webrtc_signal(&nick, &offer);
                                    let ch = current_channel.read().await.clone();
                                    state.add_message(&ch, format!("-> Reconnecting to {}", nick)).await;
                                }
                            }
                            Err(e) => error!("Reconnect peer: {}", e),
                        }
                    }
                }

                // IRC events
                Some(event) = irc_events.recv() => {
                    match event {
                        IrcEvent::UserJoined(nick) => {
                            info!("Joined: {}", nick);
                            state.update_peer_state(nick.clone(), false, false).await;

                            if nickname < nick {
                                match WebRtcPeer::new(
                                    nick.clone(), Arc::clone(&state), mix_tx.clone(),
                                    ice_out_tx.clone(), file_tx.clone(),
                                ).await {
                                    Ok(peer) => {
                                        if let Ok(offer) = peer.create_offer().await {
                                            peers.write().await.insert(nick.clone(), Arc::new(peer));
                                            let _ = irc.send_webrtc_signal(&nick, &offer);
                                            let ch = current_channel.read().await.clone();
                                            state.add_message(&ch, format!("-> Calling {}", nick)).await;
                                        }
                                    }
                                    Err(e) => error!("Peer create: {}", e),
                                }
                            }
                        }

                        IrcEvent::UserLeft(nick) => {
                            if let Some(p) = peers.write().await.remove(&nick) {
                                p.close().await;
                            }
                            state.remove_peer(&nick).await;
                        }

                        IrcEvent::WebRtcSignal { from, payload } => {
                            match serde_json::from_str::<WebRtcSignal>(&payload) {
                                Ok(WebRtcSignal::Offer { sdp }) => {
                                    match WebRtcPeer::new(
                                        from.clone(), Arc::clone(&state), mix_tx.clone(),
                                        ice_out_tx.clone(), file_tx.clone(),
                                    ).await {
                                        Ok(peer) => {
                                            if let Ok(answer) = peer.handle_offer(sdp).await {
                                                peers.write().await.insert(from.clone(), Arc::new(peer));
                                                let _ = irc.send_webrtc_signal(&from, &answer);
                                            }
                                        }
                                        Err(e) => error!("Peer for offer: {}", e),
                                    }
                                }
                                Ok(WebRtcSignal::Answer { sdp }) => {
                                    if let Some(p) = peers.read().await.get(&from) {
                                        let _ = p.handle_answer(sdp).await;
                                    }
                                }
                                Ok(WebRtcSignal::IceCandidate { candidate, sdp_mid, sdp_mline_index }) => {
                                    if let Some(p) = peers.read().await.get(&from) {
                                        let _ = p.add_ice_candidate(candidate, sdp_mid, sdp_mline_index).await;
                                    }
                                }
                                Err(e) => error!("Parse signal: {}", e),
                            }
                        }

                        IrcEvent::ChatMessage { channel, from, text } => {
                            state.add_message(&channel, format!("<{}> {}", from, text)).await;
                        }
                    }
                }

                else => break,
            }
        }

        info!("Event loop exiting");
    }

    fn send_message(&mut self) {
        if let Some(cs) = &self.call_state {
            if !self.chat_input.is_empty() {
                let _ = cs.command_tx.send(CallCommand::SendMessage(self.chat_input.clone()));
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
