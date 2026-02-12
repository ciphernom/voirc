// src/ui.rs

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

use crate::config::{RecentServer, UserConfig};
use crate::magic_link::ConnectionInfo;

#[derive(Debug, Clone, PartialEq)]
pub enum AppScreen {
    Dashboard,
    ConnectPrompt,
    HostSetup,
    Settings, // Added this
    InCall,
}

#[derive(Debug, Clone)]
pub enum DashboardAction {
    None,
    HostRoom,
    JoinRoom,
    SelectRecent(usize),
    EditProfile,
    Quit,
}

pub struct DashboardUI {
    pub selected: usize,
    pub items: Vec<String>,
    config: UserConfig,
}

impl DashboardUI {
    pub fn new(config: UserConfig) -> Self {
        Self {
            selected: 0,
            items: vec![
                "🏠 Host a Room".to_string(),
                "🔗 Join a Room".to_string(),
                "📋 Recent Servers".to_string(),
                "⚙️  Settings".to_string(),
                "❌ Quit".to_string(),
            ],
            config,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> DashboardAction {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                DashboardAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected < self.items.len() - 1 {
                    self.selected += 1;
                }
                DashboardAction::None
            }
            KeyCode::Enter => match self.selected {
                0 => DashboardAction::HostRoom,
                1 => DashboardAction::JoinRoom,
                2 => DashboardAction::SelectRecent(0),
                3 => DashboardAction::EditProfile,
                4 => DashboardAction::Quit,
                _ => DashboardAction::None,
            },
            KeyCode::Esc | KeyCode::Char('q') => DashboardAction::Quit,
            _ => DashboardAction::None,
        }
    }

    pub fn render(&mut self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(10),
                Constraint::Length(3),
            ])
            .split(f.area());

        // Header
        let header = Paragraph::new(format!(
            "🎤 Voirc - Welcome, {}",
            self.config.display_name
        ))
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
        f.render_widget(header, chunks[0]);

        // Main menu
        let items: Vec<ListItem> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let style = if i == self.selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                ListItem::new(Line::from(Span::styled(item, style)))
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Main Menu"))
            .style(Style::default().fg(Color::White));

        f.render_widget(list, chunks[1]);

        // Footer
        let footer = Paragraph::new("↑↓ Navigate | Enter Select | Esc/Q Quit")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL));
        f.render_widget(footer, chunks[2]);
    }
}

// --- NEW SETTINGS UI ---
pub struct SettingsUI {
    pub input: String,
}

impl SettingsUI {
    pub fn new(current_name: String) -> Self {
        Self {
            input: current_name,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<String> {
        match key.code {
            KeyCode::Char(c) => {
                self.input.push(c);
                None
            }
            KeyCode::Backspace => {
                self.input.pop();
                None
            }
            KeyCode::Enter => {
                if !self.input.trim().is_empty() {
                    Some(self.input.clone())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn render(&mut self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
            ])
            .split(center_rect(f.area(), 60, 20));

        let title = Paragraph::new("User Configuration")
            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL));
        f.render_widget(title, chunks[0]);

        let input = Paragraph::new(self.input.as_str())
            .style(Style::default().fg(Color::Yellow))
            .block(Block::default().borders(Borders::ALL).title("Display Name"));
        f.render_widget(input, chunks[1]);

        let footer = Paragraph::new("Enter to Save | Esc to Cancel")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL));
        f.render_widget(footer, chunks[2]);
    }
}

pub struct RecentServersUI {
    pub selected: usize,
    servers: Vec<RecentServer>,
}

impl RecentServersUI {
    pub fn new(servers: Vec<RecentServer>) -> Self {
        Self {
            selected: 0,
            servers,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<String> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected < self.servers.len().saturating_sub(1) {
                    self.selected += 1;
                }
                None
            }
            KeyCode::Enter => {
                if self.selected < self.servers.len() {
                    Some(self.servers[self.selected].connection_string.clone())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn render(&mut self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(5)])
            .split(f.area());

        let title = Paragraph::new("Recent Servers (Enter to connect, Esc to go back)")
            .style(Style::default().fg(Color::Cyan))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL));
        f.render_widget(title, chunks[0]);

        if self.servers.is_empty() {
            let empty = Paragraph::new("No recent servers")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(empty, chunks[1]);
        } else {
            let items: Vec<ListItem> = self
                .servers
                .iter()
                .enumerate()
                .map(|(i, server)| {
                    let style = if i == self.selected {
                        Style::default().fg(Color::Black).bg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(&server.name, style),
                        Span::raw(" - "),
                        Span::styled(&server.last_connected, Style::default().fg(Color::DarkGray)),
                    ]))
                })
                .collect();

            let list = List::new(items).block(Block::default().borders(Borders::ALL));
            f.render_widget(list, chunks[1]);
        }
    }
}

pub struct JoinPromptUI {
    pub input: String,
    pub error: Option<String>,
}

impl JoinPromptUI {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            error: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Result<ConnectionInfo>> {
        match key.code {
            KeyCode::Char(c) => {
                self.input.push(c);
                self.error = None;
                None
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.error = None;
                None
            }
            KeyCode::Enter => {
                if self.input.is_empty() {
                    self.error = Some("Please paste a connection link".to_string());
                    None
                } else {
                    match ConnectionInfo::from_magic_link(&self.input) {
                        Ok(info) => Some(Ok(info)),
                        Err(e) => {
                            self.error = Some(format!("Invalid link: {}", e));
                            None
                        }
                    }
                }
            }
            _ => None,
        }
    }

    pub fn render(&mut self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),
                Constraint::Length(3),
                Constraint::Length(3),
            ])
            .split(center_rect(f.area(), 60, 15));

        let title = Paragraph::new("Paste the voirc:// link from your friend:")
            .style(Style::default().fg(Color::Cyan))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title("Join Room"));
        f.render_widget(title, chunks[0]);

        let input = Paragraph::new(self.input.as_str())
            .style(Style::default().fg(Color::Yellow))
            .block(Block::default().borders(Borders::ALL).title("Connection Link"));
        f.render_widget(input, chunks[1]);

        if let Some(err) = &self.error {
            let error = Paragraph::new(err.as_str())
                .style(Style::default().fg(Color::Red))
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(error, chunks[2]);
        } else {
            let hint = Paragraph::new("Press Enter to connect | Esc to cancel")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(hint, chunks[2]);
        }
    }
}

fn center_rect(r: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
