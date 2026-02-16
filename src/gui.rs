use egui::{CentralPanel, Context, RichText, ScrollArea, TextEdit, TopBottomPanel};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info, warn};

use crate::config::{ConnState, Role, TurnServer, UserConfig};
use crate::irc_client::{IrcClient, IrcEvent};
use crate::irc_server::EmbeddedServer;
use crate::magic_link::ConnectionInfo;
use crate::moderation;
use crate::relay::{AudioRelay, RelayConnection};
use crate::state::AppState;
use crate::tls;
use crate::topology;
use crate::upnp::PortForwarder;
use crate::voice_mixer::{PeerDecoders, VoiceMixer};
use crate::webrtc_peer::{InternalSignal, ReceivedFile, WebRtcPeer, WebRtcSignal};

fn open_path(path: &Path) {
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

#[derive(Debug, Clone, PartialEq)]
enum Screen {
    Dashboard,
    HostSetup,
    JoinPrompt,
    Settings,
    RecentServers,
    InCall,
}

enum CallCommand {
    SendMessage(String),
    SwitchChannel(String),
    CreateChannel(String),
    SendFile { name: String, data: Vec<u8> },
    Shutdown,
}

struct CallState {
    state: Arc<AppState>,
    command_tx: mpsc::UnboundedSender<CallCommand>,
    channels: Arc<RwLock<Vec<String>>>,
    current_channel: Arc<RwLock<String>>,
    nickname: String,
    invite_link: Arc<RwLock<Option<String>>>,
    _mixer: Arc<VoiceMixer>,
    _input_stream: Option<cpal::Stream>,
    _output_stream: cpal::Stream,
}

pub struct VoircApp {
    config: UserConfig,
    screen: Screen,

    host_port: String,
    host_channels: String,
    host_error: Option<String>,
    host_link: Option<String>,
    host_link_external: Arc<RwLock<Option<String>>>,
    link_copied: bool,

    join_input: String,
    join_error: Option<String>,

    settings_name: String,
    settings_turn_url: String,
    settings_turn_user: String,
    settings_turn_cred: String,
    selected_recent: usize,

    chat_input: String,
    new_channel_input: String,
    call_state: Option<Arc<CallState>>,
    file_status: Option<String>,

    upnp_warning: Option<String>,
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
            settings_turn_url: String::new(),
            settings_turn_user: String::new(),
            settings_turn_cred: String::new(),
            selected_recent: 0,
            chat_input: String::new(),
            new_channel_input: String::new(),
            call_state: None,
            file_status: None,
            upnp_warning: None,
        }
    }

    fn render_dashboard(&mut self, ctx: &Context) {
        CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.heading(RichText::new("Voirc").size(32.0).strong());
                ui.add_space(10.0);
                ui.label(RichText::new(format!("Welcome, {}", self.config.display_name)).size(18.0));
                ui.add_space(40.0);
            });

            ui.vertical_centered(|ui| {
                let sz = egui::vec2(300.0, 50.0);
                if ui.add_sized(sz, egui::Button::new(RichText::new("Host a Room").size(16.0))).clicked() {
                    self.screen = Screen::HostSetup;
                }
                ui.add_space(10.0);
                if ui.add_sized(sz, egui::Button::new(RichText::new("Join a Room").size(16.0))).clicked() {
                    self.screen = Screen::JoinPrompt;
                }
                ui.add_space(10.0);
                if !self.config.recent_servers.is_empty() {
                    if ui.add_sized(sz, egui::Button::new(RichText::new("Recent Servers").size(16.0))).clicked() {
                        self.screen = Screen::RecentServers;
                    }
                    ui.add_space(10.0);
                }
                if ui.add_sized(sz, egui::Button::new(RichText::new("Settings").size(16.0))).clicked() {
                    self.settings_name = self.config.display_name.clone();
                    if let Some(ts) = self.config.turn_servers.first() {
                        self.settings_turn_url = ts.url.clone();
                        self.settings_turn_user = ts.username.clone();
                        self.settings_turn_cred = ts.credential.clone();
                    }
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

                        if ui.add_sized([400.0, 40.0], egui::Button::new(RichText::new("Start Server").size(16.0))).clicked() {
                            self.start_hosting();
                        }
                    });
                } else {
                    let external_link = self.host_link_external.try_read()
                        .ok()
                        .and_then(|guard| guard.clone());

                    ui.vertical(|ui| {
                        ui.set_max_width(500.0);
                        ui.label(RichText::new("Server is running!").size(18.0).color(egui::Color32::GREEN));
                        ui.add_space(20.0);

                        if let Some(warn) = &self.upnp_warning {
                            ui.colored_label(egui::Color32::YELLOW, warn);
                            ui.add_space(10.0);
                        }

                        if let Some(link) = &external_link {
                            ui.label(RichText::new("Share this link with friends:").size(14.0));
                            ui.add_space(10.0);
                            egui::Frame::none()
                                .fill(egui::Color32::from_rgb(40, 40, 40))
                                .inner_margin(10.0)
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.monospace(link);
                                        if ui.button("Copy").clicked() {
                                            ui.output_mut(|o| o.copied_text = link.clone());
                                            self.link_copied = true;
                                        }
                                    });
                                });
                            if self.link_copied {
                                ui.colored_label(egui::Color32::GREEN, "Copied!");
                            }
                        } else {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(RichText::new("  Resolving external IP...").size(14.0).color(egui::Color32::GRAY));
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
                if ui.button("<- Back").clicked() {
                    self.screen = Screen::Dashboard;
                    self.host_error = None;
                    self.host_link = None;
                    self.link_copied = false;
                    self.upnp_warning = None;
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
                if ui.button("<- Back").clicked() {
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
            ui.separator();
            ui.add_space(10.0);
            ui.label(RichText::new("TURN Server (for NAT traversal)").size(14.0).strong());
            ui.label(RichText::new("Required if you or friends are on corporate/university WiFi").size(12.0).color(egui::Color32::GRAY));
            ui.add_space(10.0);

            ui.horizontal(|ui| {
                ui.label("URL:     ");
                ui.add(TextEdit::singleline(&mut self.settings_turn_url).hint_text("turn:your-server.com:3478"));
            });
            ui.horizontal(|ui| {
                ui.label("Username:");
                ui.text_edit_singleline(&mut self.settings_turn_user);
            });
            ui.horizontal(|ui| {
                ui.label("Password:");
                ui.add(TextEdit::singleline(&mut self.settings_turn_cred).password(true));
            });

            ui.add_space(20.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    self.config.display_name = self.settings_name.clone();
                    self.config.turn_servers.clear();
                    if !self.settings_turn_url.is_empty() {
                        self.config.turn_servers.push(TurnServer {
                            url: self.settings_turn_url.clone(),
                            username: self.settings_turn_user.clone(),
                            credential: self.settings_turn_cred.clone(),
                        });
                    }
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

            {
                let s = Arc::clone(&state);
                tokio::spawn(async move { s.refresh_speaking().await });
            }

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

            let cmd_tx_top = cmd_tx.clone();
            let our_role = state.our_role.try_read().map(|r| *r).unwrap_or(Role::Peer);
            let invite_link_ref = call_state.invite_link.try_read()
                .ok()
                .and_then(|g| g.clone());
            TopBottomPanel::top("call_header").show(ctx, |ui| {
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(30, 30, 35))
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("V").size(20.0));
                            ui.label(RichText::new(&current_channel).size(16.0).strong());

                            let badge = match our_role {
                                Role::Host => RichText::new(" [HOST]").size(12.0).color(egui::Color32::GOLD),
                                Role::Mod => RichText::new(" [MOD]").size(12.0).color(egui::Color32::LIGHT_BLUE),
                                Role::Peer => RichText::new("").size(12.0),
                            };
                            ui.label(badge);

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add_sized([100.0, 30.0], egui::Button::new("Disconnect")).clicked() {
                                    let _ = cmd_tx_top.send(CallCommand::Shutdown);
                                    disconnect = true;
                                }

                                if let Some(link) = &invite_link_ref {
                                    if ui.button("Invite").clicked() {
                                        ui.output_mut(|o| o.copied_text = link.clone());
                                    }
                                }

                                if let Some(status) = &self.file_status {
                                    ui.label(RichText::new(status).size(11.0).color(egui::Color32::LIGHT_GREEN));
                                }
                            });
                        });
                    });
            });

            let chat_id = egui::Id::new("chat_input_field");
            TopBottomPanel::bottom("call_input").show(ctx, |ui| {
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(30, 30, 35))
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let resp = ui.add(
                                TextEdit::singleline(&mut self.chat_input)
                                    .id(chat_id)
                                    .hint_text("Type a message... (/? for commands)")
                                    .desired_width(ui.available_width() - 10.0),
                            );
                            if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.chat_input.is_empty() {
                                send_msg = true;
                                resp.request_focus();
                            }
                        });
                    });
            });

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
                                        RichText::new(format!("> {}", ch)).size(14.0).strong().color(egui::Color32::WHITE)
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

            // Right sidebar — voice panel with connection state
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

                                ui.label(RichText::new(format!("VOICE - {} online", connected)).size(12.0).color(egui::Color32::GRAY));
                                ui.add_space(10.0);
                                ui.separator();
                                ui.add_space(10.0);

                                // You
                                ui.horizontal(|ui| {
                                    ui.add_space(10.0);
                                    let role_tag = match our_role {
                                        Role::Host => " *",
                                        Role::Mod => " +",
                                        Role::Peer => "",
                                    };
                                    ui.label(RichText::new(format!("{} (you){}", nickname, role_tag)).size(14.0));
                                });
                                ui.add_space(5.0);

                                // Peers with connection state
                                if let Ok(peers) = state_sb.peer_states.try_read() {
                                    for peer in peers.values() {
                                        ui.horizontal(|ui| {
                                            ui.add_space(10.0);

                                            let (icon, icon_color) = match peer.conn_state {
                                                ConnState::Connected => {
                                                    if peer.speaking {
                                                        (">>", egui::Color32::GREEN)
                                                    } else {
                                                        ("--", egui::Color32::GRAY)
                                                    }
                                                }
                                                ConnState::Connecting => ("...", egui::Color32::YELLOW),
                                                ConnState::NatIssue => ("!!", egui::Color32::from_rgb(255, 165, 0)),
                                                ConnState::Relayed => ("~>", egui::Color32::LIGHT_BLUE),
                                                ConnState::Failed => ("XX", egui::Color32::RED),
                                            };
                                            ui.label(RichText::new(icon).size(14.0).color(icon_color).monospace());

                                            let role_tag = match peer.role {
                                                Role::Host => " *",
                                                Role::Mod => " +",
                                                Role::Peer => "",
                                            };
                                            let color = if peer.connected { egui::Color32::WHITE } else { egui::Color32::GRAY };
                                            ui.label(RichText::new(format!("{}{}", peer.nickname, role_tag)).size(14.0).color(color));
                                        });

                                        // Show status text for non-connected peers
                                        if peer.conn_state != ConnState::Connected {
                                            ui.horizontal(|ui| {
                                                ui.add_space(35.0);
                                                let status_color = match peer.conn_state {
                                                    ConnState::NatIssue | ConnState::Failed => egui::Color32::from_rgb(255, 165, 0),
                                                    _ => egui::Color32::DARK_GRAY,
                                                };
                                                ui.label(RichText::new(peer.conn_state.to_string()).size(10.0).color(status_color));
                                            });
                                        }
                                        ui.add_space(5.0);
                                    }
                                }

                                // Files section
                                if let Ok(files) = state_sb.received_files.try_read() {
                                    if !files.is_empty() {
                                        ui.add_space(15.0);
                                        ui.separator();
                                        ui.add_space(10.0);
                                        ui.label(RichText::new("FILES").size(12.0).color(egui::Color32::GRAY));
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
                                                            ui.label(RichText::new(format!("from {} - {} KB", file.from, file.size / 1024)).size(10.0).color(egui::Color32::GRAY));
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
                            ui.label(RichText::new(format!("TEXT CHAT - {}", current_channel)).size(12.0).color(egui::Color32::GRAY));
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

        // Generate TLS cert
        let cert_dir = UserConfig::tls_cert_dir();
        let cert_info = match tls::load_or_generate(&cert_dir) {
            Ok(ci) => ci,
            Err(e) => {
                warn!("TLS cert generation failed, falling back to plaintext: {}", e);
                // Fall back to plaintext
                tokio::spawn(async move {
                    if let Err(e) = EmbeddedServer::run(port).await {
                        error!("Server error: {}", e);
                    }
                });

                let conn_info = ConnectionInfo::new("127.0.0.1".to_string(), port, channels.clone());
                self.host_link = conn_info.to_magic_link().ok();
                self.link_copied = false;

                let external = Arc::clone(&self.host_link_external);
                let ch = channels;
                tokio::spawn(async move {
                    if let Ok(ip) = PortForwarder::get_external_ip(None).await {
                        let info = ConnectionInfo::new(ip, port, ch);
                        if let Ok(link) = info.to_magic_link() {
                            *external.write().await = Some(link);
                        }
                    }
                });

                self.host_error = None;
                return;
            }
        };

        let fingerprint = cert_info.fingerprint.clone();
        let relay_port = port + 1;

        // Start TLS IRC server
        let cert_for_server = crate::tls::CertInfo {
            cert_der: cert_info.cert_der.clone(),
            key_der: cert_info.key_der.clone(),
            fingerprint: cert_info.fingerprint.clone(),
        };
        tokio::spawn(async move {
            if let Err(e) = EmbeddedServer::run_tls(port, &cert_for_server).await {
                error!("Server error: {}", e);
            }
        });

        // Start relay
        tokio::spawn(async move {
            if let Err(e) = AudioRelay::run(relay_port).await {
                error!("Relay error: {}", e);
            }
        });

        // UPnP with warning feedback
        let upnp_warning: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));
        let uw = Arc::clone(&upnp_warning);
        let port_c = port;
        tokio::spawn(async move {
            if let Err(e) = PortForwarder::forward_port(port_c, None).await {
                info!("UPnP failed (not critical): {}", e);
                *uw.write().await = Some(
                    "Could not auto-forward port. Friends outside your network may not connect.".to_string()
                );
            }
        });
        // Check the warning after a brief delay
        let uw2 = Arc::clone(&upnp_warning);
        let upnp_warning_out = Arc::new(std::sync::Mutex::new(None::<String>));
        let uwo = Arc::clone(&upnp_warning_out);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            if let Some(w) = uw2.read().await.clone() {
                if let Ok(mut guard) = uwo.lock() {
                    *guard = Some(w);
                }
            }
        });

        let conn_info = ConnectionInfo::new("127.0.0.1".to_string(), port, channels.clone())
            .with_tls(fingerprint.clone())
            .with_relay(relay_port);
        self.host_link = conn_info.to_magic_link().ok();
        self.link_copied = false;

        let external = Arc::clone(&self.host_link_external);
        let ch = channels;
        let fp = fingerprint;
        tokio::spawn(async move {
            if let Ok(ip) = PortForwarder::get_external_ip(None).await {
                let info = ConnectionInfo::new(ip, port, ch)
                    .with_tls(fp)
                    .with_relay(relay_port);
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
        let turn_servers = self.config.turn_servers.clone();
        let banned_users = self.config.banned_users.clone();
        
        // --- CHANGE: Capture the fingerprint explicitly ---
        let cert_fingerprint = conn_info.cert_fingerprint.clone();
        let use_tls = cert_fingerprint.is_some();
        // --------------------------------------------------

        let relay_port = conn_info.relay_port;
        let relay_addr = if let Some(rp) = relay_port {
            Some(format!("{}:{}", conn_info.host, rp))
        } else {
            None
        };

        // Update diagnostics
        {
            let s = Arc::clone(&state);
            let turn_count = turn_servers.len();
            let has_relay = relay_port.is_some();
            tokio::spawn(async move {
                let mut d = s.diagnostics.write().await;
                d.turn_configured = turn_count;
                d.relay_enabled = has_relay;
                d.relay_port = relay_port;
            });
        }

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

        let mixer = match VoiceMixer::new() {
            Ok(m) => Arc::new(m),
            Err(e) => { error!("Mixer: {}", e); return; }
        };

        let (mic_tx, mic_rx) = mpsc::unbounded_channel();
        let (mix_tx, mut mix_rx) = mpsc::unbounded_channel::<(String, Vec<u8>)>();

        let input_stream = match mixer.start_input(mic_tx) {
            Ok(s) => s,
            Err(e) => { error!("Input stream: {}", e); None }
        };
        let output_stream = match mixer.start_output() {
            Ok(o) => o,
            Err(e) => { error!("Output stream: {}", e); return; }
        };

        if input_stream.is_none() {
            info!("Running in listen-only mode (no microphone)");
        }
        let has_mic = input_stream.is_some();

        let mixer_c = Arc::clone(&mixer);
        let state_mix = Arc::clone(&state);
        let state_fwd = Arc::clone(&state);
        let (forward_tx, forward_rx) = mpsc::unbounded_channel::<(String, Vec<u8>)>();
        tokio::spawn(async move {
            let mut decoders = PeerDecoders::new();
            while let Some((nick, packet)) = mix_rx.recv().await {
                if let Some(pcm) = decoders.decode(&nick, &packet) {
                    mixer_c.queue_audio(pcm);
                    state_mix.mark_speaking(&nick).await;
                }
                let our_role = state_fwd.our_role().await;
                if our_role.is_superpeer() {
                    let _ = forward_tx.send((nick, packet));
                }
            }
        });

        let (command_tx, command_rx) = mpsc::unbounded_channel::<CallCommand>();
        let (file_tx, file_rx) = mpsc::unbounded_channel::<ReceivedFile>();

        let current_channel = Arc::new(RwLock::new(default_channel.clone()));
        let channels = Arc::new(RwLock::new(channels_vec));

        let state_c = Arc::clone(&state);
        let nick_c = nickname.clone();
        let conn_c = conn_info.clone();
        let cur_ch = Arc::clone(&current_channel);
        let channels_for_loop = Arc::clone(&channels);

        let our_role = if is_host { Role::Host } else { Role::Peer };
        let state_role = Arc::clone(&state);
        tokio::spawn(async move {
            state_role.set_our_role(our_role).await;
        });

        let invite_link_c = Arc::clone(&self.host_link_external);

        tokio::spawn(async move {
            let timeout = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                IrcClient::connect(
                    conn_c.server_address(),
                    nick_c.clone(),
                    default_channel.clone(),
                    state_c.clone(),
                    // --- CHANGE: Pass the fingerprint instead of bool ---
                    cert_fingerprint,
                    // ----------------------------------------------------
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

                    let _ = irc.announce_role(&default_channel, our_role);

                    let ch = cur_ch.read().await.clone();
                    let tls_note = if use_tls { " (TLS)" } else { "" };
                    state_c.add_message(&ch, format!("Connected as {}{}", nick_c, tls_note)).await;
                    if is_host {
                        state_c.add_message(&ch, "You are hosting this room".to_string()).await;
                        if relay_addr.is_some() {
                            state_c.add_message(&ch, "Audio relay is active for NAT-challenged peers".to_string()).await;
                        }
                    }
                    if !has_mic {
                        state_c.add_message(&ch, "No microphone detected - listen-only mode. Chat still works!".to_string()).await;
                    }

                    state_c.load_history(&ch, 100).await;

                    let custom_commands = moderation::CustomCommands::load();

                    Self::event_loop(
                        irc, irc_events, state_c, nick_c, our_role,
                        mix_tx, mic_rx, forward_rx,
                        command_rx, file_tx, file_rx,
                        cur_ch, channels_for_loop,
                        turn_servers, banned_users,
                        custom_commands, invite_link_c,
                        relay_addr,
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

        let invite_link = Arc::clone(&self.host_link_external);
        if !is_host {
            if let Ok(link) = conn_info.to_magic_link() {
                let il = Arc::clone(&invite_link);
                tokio::spawn(async move {
                    let mut guard = il.write().await;
                    if guard.is_none() {
                        *guard = Some(link);
                    }
                });
            }
        }

        self.call_state = Some(Arc::new(CallState {
            state,
            command_tx,
            channels,
            current_channel,
            nickname,
            invite_link,
            _mixer: mixer,
            _input_stream: input_stream,
            _output_stream: output_stream,
        }));
        self.screen = Screen::InCall;
        self.file_status = None;
    }

    #[allow(clippy::too_many_arguments)]
    async fn event_loop(
        irc: Arc<IrcClient>,
        mut irc_events: mpsc::UnboundedReceiver<IrcEvent>,
        state: Arc<AppState>,
        nickname: String,
        our_role: Role,
        mix_tx: mpsc::UnboundedSender<(String, Vec<u8>)>,
        mut mic_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        mut forward_rx: mpsc::UnboundedReceiver<(String, Vec<u8>)>,
        mut command_rx: mpsc::UnboundedReceiver<CallCommand>,
        file_tx: mpsc::UnboundedSender<ReceivedFile>,
        mut file_rx: mpsc::UnboundedReceiver<ReceivedFile>,
        current_channel: Arc<RwLock<String>>,
        channels: Arc<RwLock<Vec<String>>>,
        turn_servers: Vec<TurnServer>,
        banned_users: std::collections::HashSet<String>,
        mut custom_commands: moderation::CustomCommands,
        invite_link: Arc<RwLock<Option<String>>>,
        relay_addr: Option<String>,
    ) {
        let (ice_out_tx, mut ice_out_rx) = mpsc::unbounded_channel::<InternalSignal>();
        let peers: Arc<RwLock<HashMap<String, Arc<WebRtcPeer>>>> =
            Arc::new(RwLock::new(HashMap::new()));

        let reconnect_attempts: Arc<RwLock<HashMap<String, u32>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // Optional relay connection for fallback
        let relay_conn: Arc<RwLock<Option<RelayConnection>>> = Arc::new(RwLock::new(None));
        if let Some(ref addr) = relay_addr {
            let mix_tx_relay = mix_tx.clone();
            let addr_c = addr.clone();
            let nick_c = nickname.clone();
            let rc = Arc::clone(&relay_conn);
            tokio::spawn(async move {
                match RelayConnection::connect(&addr_c, &nick_c, mix_tx_relay).await {
                    Ok(conn) => {
                        info!("Relay connection established");
                        *rc.write().await = Some(conn);
                    }
                    Err(e) => {
                        warn!("Could not connect to relay: {}", e);
                    }
                }
            });
        }

        let irc_c = Arc::clone(&irc);
        let (reconnect_tx, mut reconnect_rx) = mpsc::unbounded_channel::<String>();
        let (conn_failed_tx, mut conn_failed_rx) = mpsc::unbounded_channel::<String>();
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
                    InternalSignal::ConnFailed(nick) => {
                        let _ = conn_failed_tx.send(nick);
                    }
                }
            }
        });

        let peers_mic = Arc::clone(&peers);
        let relay_mic = Arc::clone(&relay_conn);
        tokio::spawn(async move {
            while let Some(pkt) = mic_rx.recv().await {
                let r = peers_mic.read().await;
                for p in r.values() {
                    let _ = p.send_audio(&pkt).await;
                }
                // Also send via relay if connected
                if let Some(rc) = relay_mic.read().await.as_ref() {
                    let _ = rc.send_audio(&pkt).await;
                }
            }
        });

        let peers_fwd = Arc::clone(&peers);
        tokio::spawn(async move {
            while let Some((sender_nick, raw_packet)) = forward_rx.recv().await {
                let r = peers_fwd.read().await;
                for (nick, peer) in r.iter() {
                    if *nick != sender_nick {
                        let _ = peer.send_audio(&raw_packet).await;
                    }
                }
            }
        });

        async fn maybe_create_peer(
            nick: &str,
            nickname: &str,
            our_role: Role,
            state: &Arc<AppState>,
            peers: &Arc<RwLock<HashMap<String, Arc<WebRtcPeer>>>>,
            irc: &Arc<IrcClient>,
            mix_tx: &mpsc::UnboundedSender<(String, Vec<u8>)>,
            ice_out_tx: &mpsc::UnboundedSender<InternalSignal>,
            file_tx: &mpsc::UnboundedSender<ReceivedFile>,
            turn_servers: &[TurnServer],
            current_channel: &Arc<RwLock<String>>,
        ) {
            let target_role = state.get_peer_role(nick).await;

            if !topology::should_connect_to(state, nickname, our_role, nick, target_role).await {
                info!("Topology: skipping connection to {} (not our superpeer)", nick);
                return;
            }

            if nickname >= nick {
                return;
            }

            state.set_peer_connecting(nick).await;

            match WebRtcPeer::new(
                nick.to_string(), Arc::clone(state), mix_tx.clone(),
                ice_out_tx.clone(), file_tx.clone(), turn_servers.to_vec(),
            ).await {
                Ok(peer) => {
                    if let Ok(offer) = peer.create_offer().await {
                        peers.write().await.insert(nick.to_string(), Arc::new(peer));
                        let _ = irc.send_webrtc_signal(nick, &offer);
                        let ch = current_channel.read().await.clone();
                        state.add_message(&ch, format!("-> Calling {}", nick)).await;
                    }
                }
                Err(e) => error!("Peer create: {}", e),
            }
        }

        loop {
            tokio::select! {
                Some(cmd) = command_rx.recv() => {
                    match cmd {
                        CallCommand::SendMessage(text) => {
                            let ch = current_channel.read().await.clone();

                            let peer_nicks: Vec<String> = state.peer_states.read().await
                                .values()
                                .filter(|ps| ps.connected)
                                .map(|ps| ps.nickname.clone())
                                .collect();
                            let ctx = moderation::CommandContext {
                                nick: nickname.clone(),
                                channel: ch.clone(),
                                role: our_role,
                                peers: peer_nicks,
                            };

                            if let Some(command) = moderation::parse_command(&text, &custom_commands, &ctx) {
                                match command {
                                    moderation::Command::Help => {
                                        for line in moderation::help_text(our_role, &custom_commands) {
                                            state.add_message(&ch, line).await;
                                        }
                                    }
                                    moderation::Command::ShowRole => {
                                        state.add_message(&ch, format!("Your role: {}", our_role.as_str())).await;
                                    }
                                    moderation::Command::ListPeers => {
                                        let peers_list = state.peer_states.read().await;
                                        if peers_list.is_empty() {
                                            state.add_message(&ch, "No peers connected".to_string()).await;
                                        } else {
                                            state.add_message(&ch, "-- Connected Peers --".to_string()).await;
                                            for ps in peers_list.values() {
                                                state.add_message(&ch, format!(
                                                    "  {} [{}] ({})", ps.nickname, ps.role.as_str(), ps.conn_state
                                                )).await;
                                            }
                                        }
                                    }
                                    moderation::Command::ListBanned => {
                                        if !our_role.can_moderate() {
                                            state.add_message(&ch, "Only host and mods can view ban list".to_string()).await;
                                        } else if banned_users.is_empty() {
                                            state.add_message(&ch, "No banned users".to_string()).await;
                                        } else {
                                            state.add_message(&ch, "-- Banned Users --".to_string()).await;
                                            for nick in &banned_users {
                                                state.add_message(&ch, format!("  {}", nick)).await;
                                            }
                                        }
                                    }
                                    moderation::Command::Diag => {
                                        let d = state.diagnostics.read().await;
                                        state.add_message(&ch, "-- Connection Diagnostics --".to_string()).await;
                                        state.add_message(&ch, format!("  Local IP: {}", d.local_ip.as_deref().unwrap_or("unknown"))).await;
                                        state.add_message(&ch, format!("  External IP: {}", d.external_ip.as_deref().unwrap_or("unknown"))).await;
                                        state.add_message(&ch, format!("  UPnP: {}", d.upnp_status.as_deref().unwrap_or("not checked"))).await;
                                        state.add_message(&ch, format!("  TURN servers: {}", d.turn_configured)).await;
                                        state.add_message(&ch, format!("  Relay: {}", if d.relay_enabled { "enabled" } else { "disabled" })).await;
                                        if let Some(rp) = d.relay_port {
                                            state.add_message(&ch, format!("  Relay port: {}", rp)).await;
                                        }
                                        state.add_message(&ch, format!("  Port open: {}", match d.port_open {
                                            Some(true) => "yes",
                                            Some(false) => "no",
                                            None => "unknown",
                                        })).await;

                                        // Show peer connection states
                                        let peers_list = state.peer_states.read().await;
                                        if !peers_list.is_empty() {
                                            state.add_message(&ch, "  Peer connections:".to_string()).await;
                                            for ps in peers_list.values() {
                                                state.add_message(&ch, format!("    {} - {}", ps.nickname, ps.conn_state)).await;
                                            }
                                        }
                                    }
                                    moderation::Command::EditCommands => {
                                        let path = moderation::CustomCommands::commands_path();
                                        state.add_message(&ch, format!("Commands file: {}", path.display())).await;
                                        let opened = std::process::Command::new("xdg-open")
                                            .arg(&path)
                                            .spawn()
                                            .or_else(|_| std::process::Command::new("open").arg(&path).spawn())
                                            .or_else(|_| std::process::Command::new("notepad").arg(&path).spawn());
                                        match opened {
                                            Ok(_) => {
                                                state.add_message(&ch, "Opened in editor. Use /reload when done.".to_string()).await;
                                            }
                                            Err(_) => {
                                                state.add_message(&ch, "Could not open editor - edit the file manually, then /reload".to_string()).await;
                                            }
                                        }
                                    }
                                    moderation::Command::Invite => {
                                        let link = invite_link.read().await;
                                        if let Some(l) = link.as_ref() {
                                            state.add_message(&ch, format!("Invite link: {}", l)).await;
                                        } else {
                                            state.add_message(&ch, "No invite link available yet (external IP still resolving)".to_string()).await;
                                        }
                                    }
                                    moderation::Command::Reload => {
                                        custom_commands = moderation::CustomCommands::load();
                                        state.add_message(&ch, "Custom commands reloaded".to_string()).await;
                                    }
                                    moderation::Command::Mod(action) => {
                                        match moderation::check_permission(our_role, &action) {
                                            Ok(()) => {
                                                match &action {
                                                    moderation::ModAction::Kick(target) => {
                                                        let _ = irc.send_mod_action(&ch, "kick", target);
                                                        state.add_message(&ch, format!("Kicked {}", target)).await;
                                                    }
                                                    moderation::ModAction::Ban(target) => {
                                                        let _ = irc.send_mod_action(&ch, "ban", target);
                                                        state.add_message(&ch, format!("Banned {}", target)).await;
                                                    }
                                                    moderation::ModAction::Unban(target) => {
                                                        state.add_message(&ch, format!("Unbanned {}", target)).await;
                                                    }
                                                    moderation::ModAction::Promote(target) => {
                                                        let _ = irc.send_mod_action(&ch, "promote", target);
                                                        state.set_peer_role(target, Role::Mod).await;
                                                        state.add_message(&ch, format!("{} is now a mod (superpeer relay)", target)).await;
                                                    }
                                                    moderation::ModAction::Demote(target) => {
                                                        let _ = irc.send_mod_action(&ch, "demote", target);
                                                        state.set_peer_role(target, Role::Peer).await;
                                                        state.add_message(&ch, format!("{} is no longer a mod", target)).await;
                                                    }
                                                }
                                            }
                                            Err(msg) => {
                                                state.add_message(&ch, format!("Permission denied: {}", msg)).await;
                                            }
                                        }
                                    }
                                    moderation::Command::Custom { response, broadcast } => {
                                        if broadcast {
                                            if let Err(e) = irc.send_message(&ch, &response) {
                                                error!("Send: {}", e);
                                            } else {
                                                state.add_message(&ch, format!("<{}> {}", nickname, response)).await;
                                            }
                                        } else {
                                            state.add_message(&ch, response).await;
                                        }
                                    }
                                    moderation::Command::Unknown(cmd) => {
                                        state.add_message(&ch, format!("Unknown command: {}. Type /help for commands.", cmd)).await;
                                    }
                                }
                            } else {
                                if let Err(e) = irc.send_message(&ch, &text) {
                                    error!("Send: {}", e);
                                } else {
                                    state.add_message(&ch, format!("<{}> {}", nickname, text)).await;
                                }
                            }
                        }
                        CallCommand::SwitchChannel(new_ch) => {
                            let old_ch = current_channel.read().await.clone();
                            info!("Switching {} -> {}", old_ch, new_ch);

                            let mut pw = peers.write().await;
                            for (_, p) in pw.drain() {
                                p.close().await;
                            }
                            drop(pw);
                            state.clear_peers().await;

                            let _ = irc.part_channel(&old_ch);
                            let _ = irc.join_channel(&new_ch);
                            *current_channel.write().await = new_ch.clone();

                            state.load_history(&new_ch, 100).await;
                            let _ = irc.announce_role(&new_ch, our_role);

                            state.add_message(&new_ch, format!("Joined {}", new_ch)).await;
                        }
                        CallCommand::CreateChannel(new_ch) => {
                            let mut ch_list = channels.write().await;
                            if !ch_list.contains(&new_ch) {
                                ch_list.push(new_ch.clone());
                                drop(ch_list);

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

                                let _ = irc.announce_role(&new_ch, our_role);

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
                                "You shared {} ({} KB) -> {} peers", name, kb, ok
                            )).await;
                        }
                        CallCommand::Shutdown => {
                            info!("Shutdown");
                            break;
                        }
                    }
                }

                Some(file) = file_rx.recv() => {
                    let ch = current_channel.read().await.clone();
                    let size = file.data.len();
                    let kb = size / 1024;

                    let save_dir = dirs::download_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
                    let save_path = save_dir.join(&file.name);
                    match std::fs::write(&save_path, &file.data) {
                        Ok(_) => {
                            state.add_received_file(
                                file.from.clone(), file.name.clone(), size, save_path.clone(),
                            ).await;
                            state.add_message(&ch, format!(
                                "{} shared {} ({} KB)",
                                file.from, file.name, kb,
                            )).await;
                        }
                        Err(e) => {
                            state.add_message(&ch, format!(
                                "{} shared {} ({} KB) - save failed: {}",
                                file.from, file.name, kb, e
                            )).await;
                        }
                    }
                }

                // Connection failure - suggest relay
                Some(nick) = conn_failed_rx.recv() => {
                    let ch = current_channel.read().await.clone();
                    if relay_addr.is_some() {
                        state.add_message(&ch, format!(
                            "WebRTC to {} failed - audio will route through relay", nick
                        )).await;
                        state.set_peer_conn_state(&nick, ConnState::Relayed).await;
                    } else {
                        state.add_message(&ch, format!(
                            "Connection to {} failed. Ask the host to enable relay mode.", nick
                        )).await;
                    }
                }

                Some(nick) = reconnect_rx.recv() => {
                    info!("Reconnect: {}", nick);
                    peers.write().await.remove(&nick);

                    if !state.peer_states.read().await.contains_key(&nick) { continue; }

                    let mut attempts = reconnect_attempts.write().await;
                    let attempt = attempts.entry(nick.clone()).or_insert(0);
                    *attempt += 1;
                    let delay = std::cmp::min(2u64.pow(*attempt), 30);
                    drop(attempts);

                    info!("Reconnect backoff for {}: {}s", nick, delay);
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;

                    if nickname < nick {
                        let target_role = state.get_peer_role(&nick).await;
                        if !topology::should_connect_to(&state, &nickname, our_role, &nick, target_role).await {
                            continue;
                        }

                        state.set_peer_connecting(&nick).await;

                        match WebRtcPeer::new(
                            nick.clone(), Arc::clone(&state), mix_tx.clone(),
                            ice_out_tx.clone(), file_tx.clone(), turn_servers.clone(),
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

                Some(event) = irc_events.recv() => {
                    match event {
                        IrcEvent::UserJoined { nick, role } => {
                            info!("Joined: {} (role: {:?})", nick, role);

                            if banned_users.contains(&nick) {
                                warn!("Banned user {} tried to join, ignoring", nick);
                                let ch = current_channel.read().await.clone();
                                state.add_message(&ch, format!("Banned user {} tried to join", nick)).await;
                                continue;
                            }

                            state.update_peer_state(nick.clone(), false, false).await;
                            state.set_peer_role(&nick, role).await;

                            reconnect_attempts.write().await.remove(&nick);

                            maybe_create_peer(
                                &nick, &nickname, our_role, &state, &peers,
                                &irc, &mix_tx, &ice_out_tx, &file_tx,
                                &turn_servers, &current_channel,
                            ).await;
                        }

                        IrcEvent::UserLeft(nick) => {
                            if let Some(p) = peers.write().await.remove(&nick) {
                                p.close().await;
                            }
                            state.remove_peer(&nick).await;
                            reconnect_attempts.write().await.remove(&nick);
                        }

                        IrcEvent::WebRtcSignal { from, payload } => {
                            match serde_json::from_str::<WebRtcSignal>(&payload) {
                                Ok(WebRtcSignal::Offer { sdp }) => {
                                    let from_role = state.get_peer_role(&from).await;
                                    if !topology::should_connect_to(&state, &nickname, our_role, &from, from_role).await {
                                        info!("Topology: rejecting offer from {}", from);
                                        continue;
                                    }

                                    state.set_peer_connecting(&from).await;

                                    match WebRtcPeer::new(
                                        from.clone(), Arc::clone(&state), mix_tx.clone(),
                                        ice_out_tx.clone(), file_tx.clone(), turn_servers.clone(),
                                    ).await {
                                        Ok(peer) => {
                                            if let Ok(answer) = peer.handle_offer(sdp).await {
                                                peers.write().await.insert(from.clone(), Arc::new(peer));
                                                let _ = irc.send_webrtc_signal(&from, &answer);
                                                reconnect_attempts.write().await.remove(&from);
                                            }
                                        }
                                        Err(e) => error!("Peer for offer: {}", e),
                                    }
                                }
                                Ok(WebRtcSignal::Answer { sdp }) => {
                                    if let Some(p) = peers.read().await.get(&from) {
                                        let _ = p.handle_answer(sdp).await;
                                        reconnect_attempts.write().await.remove(&from);
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

                        IrcEvent::ModAction { from, action, target } => {
                            let from_role = state.get_peer_role(&from).await;
                            if !from_role.can_moderate() {
                                warn!("Non-mod {} tried to use mod action", from);
                                continue;
                            }

                            let ch = current_channel.read().await.clone();
                            match action.as_str() {
                                "kick" => {
                                    if target == nickname {
                                        state.add_message(&ch, format!("You were kicked by {}", from)).await;
                                    } else {
                                        state.add_message(&ch, format!("{} was kicked by {}", target, from)).await;
                                    }
                                }
                                "ban" => {
                                    if target == nickname {
                                        state.add_message(&ch, format!("You were banned by {}", from)).await;
                                    } else {
                                        state.add_message(&ch, format!("{} was banned by {}", target, from)).await;
                                    }
                                }
                                "promote" => {
                                    state.set_peer_role(&target, Role::Mod).await;
                                    if target == nickname {
                                        state.set_our_role(Role::Mod).await;
                                        state.add_message(&ch, format!("You were promoted to mod by {}", from)).await;
                                    } else {
                                        state.add_message(&ch, format!("{} was promoted to mod by {}", target, from)).await;
                                    }
                                }
                                "demote" => {
                                    state.set_peer_role(&target, Role::Peer).await;
                                    if target == nickname {
                                        state.set_our_role(Role::Peer).await;
                                        state.add_message(&ch, "You were demoted from mod".to_string()).await;
                                    } else {
                                        state.add_message(&ch, format!("{} was demoted from mod", target)).await;
                                    }
                                }
                                _ => {}
                            }
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
