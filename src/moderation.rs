use crate::config::Role;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum ModAction {
    Kick(String),
    Ban(String),
    Unban(String),
    Promote(String),
    Demote(String),
}

#[derive(Debug, Clone)]
pub enum Command {
    Mod(ModAction),
    Help,
    ListBanned,
    ShowRole,
    ListPeers,
    EditCommands,
    Invite,
    Reload,
    Diag,
    Custom { response: String, broadcast: bool },
    Unknown(String),
}

pub fn parse_command(text: &str, custom_commands: &CustomCommands, context: &CommandContext) -> Option<Command> {
    let text = text.trim();
    if !text.starts_with('/') {
        return None;
    }

    let parts: Vec<&str> = text.splitn(2, ' ').collect();
    let cmd = parts[0].to_lowercase();
    let arg = parts.get(1).map(|s| s.trim().to_string());

    match cmd.as_str() {
        "/?" | "/help" => Some(Command::Help),
        "/kick" => arg.map(|a| Command::Mod(ModAction::Kick(a))),
        "/ban" => arg.map(|a| Command::Mod(ModAction::Ban(a))),
        "/unban" => arg.map(|a| Command::Mod(ModAction::Unban(a))),
        "/listbanned" | "/bans" => Some(Command::ListBanned),
        "/mod" => arg.map(|a| Command::Mod(ModAction::Promote(a))),
        "/unmod" => arg.map(|a| Command::Mod(ModAction::Demote(a))),
        "/role" => Some(Command::ShowRole),
        "/peers" | "/who" => Some(Command::ListPeers),
        "/editcommands" | "/commands" => Some(Command::EditCommands),
        "/invite" | "/link" => Some(Command::Invite),
        "/reload" => Some(Command::Reload),
        "/diag" | "/diagnostics" => Some(Command::Diag),
        _ => {
            let cmd_name = &cmd[1..];
            if let Some(template) = custom_commands.get(cmd_name) {
                let response = expand_template(template, context);
                Some(Command::Custom { response, broadcast: custom_commands.is_broadcast(cmd_name) })
            } else {
                Some(Command::Unknown(cmd))
            }
        }
    }
}

pub fn check_permission(our_role: Role, action: &ModAction) -> Result<(), &'static str> {
    match action {
        ModAction::Kick(_) | ModAction::Ban(_) | ModAction::Unban(_) => {
            if our_role.can_moderate() {
                Ok(())
            } else {
                Err("Only host and mods can kick/ban")
            }
        }
        ModAction::Promote(_) | ModAction::Demote(_) => {
            if our_role.can_promote() {
                Ok(())
            } else {
                Err("Only the host can promote/demote mods")
            }
        }
    }
}

pub fn help_text(our_role: Role, custom_commands: &CustomCommands) -> Vec<String> {
    let mut lines = vec![
        "── Commands ──".to_string(),
        "/help, /?       Show this help".to_string(),
        "/role           Show your current role".to_string(),
        "/peers, /who    List connected peers".to_string(),
        "/invite, /link  Copy invite link to clipboard".to_string(),
        "/diag           Show connection diagnostics".to_string(),
        "/editcommands   Open commands.toml for custom commands".to_string(),
        "/reload         Reload custom commands from disk".to_string(),
    ];

    if our_role.can_moderate() {
        lines.push("/kick <nick>    Disconnect a user".to_string());
        lines.push("/ban <nick>     Ban a user".to_string());
        lines.push("/unban <nick>   Unban a user".to_string());
        lines.push("/listbanned     Show ban list".to_string());
    }

    if our_role.can_promote() {
        lines.push("/mod <nick>     Promote to mod (superpeer)".to_string());
        lines.push("/unmod <nick>   Demote from mod".to_string());
    }

    if !custom_commands.commands.is_empty() {
        lines.push("── Custom ──".to_string());
        for (name, entry) in &custom_commands.commands {
            let desc = entry.description.as_deref().unwrap_or(&entry.response);
            let truncated = if desc.len() > 40 { format!("{}...", &desc[..37]) } else { desc.to_string() };
            lines.push(format!("/{}  {}", name, truncated));
        }
    }

    lines
}

#[derive(Debug, Clone, Deserialize, Default)]
struct CommandEntry {
    response: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    broadcast: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct CommandsFile {
    #[serde(default)]
    commands: HashMap<String, CommandEntryOrString>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum CommandEntryOrString {
    Full(CommandEntry),
    Simple(String),
}

impl CommandEntryOrString {
    fn to_entry(self) -> CommandEntry {
        match self {
            CommandEntryOrString::Full(e) => e,
            CommandEntryOrString::Simple(s) => CommandEntry {
                response: s,
                description: None,
                broadcast: false,
            },
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CustomCommands {
    commands: HashMap<String, CommandEntry>,
}

impl CustomCommands {
    pub fn load() -> Self {
        let path = Self::commands_path();
        if !path.exists() {
            Self::write_example(&path);
            return Self::default();
        }

        match std::fs::read_to_string(&path) {
            Ok(content) => {
                match toml::from_str::<CommandsFile>(&content) {
                    Ok(file) => {
                        let commands = file.commands.into_iter()
                            .map(|(k, v)| (k, v.to_entry()))
                            .collect();
                        Self { commands }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse commands.toml: {}", e);
                        Self::default()
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to read commands.toml: {}", e);
                Self::default()
            }
        }
    }

    pub fn commands_path() -> PathBuf {
        let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        path.push("voirc");
        std::fs::create_dir_all(&path).ok();
        path.push("commands.toml");
        path
    }

    fn write_example(path: &PathBuf) {
        let example = r#"# Voirc Custom Commands
# Each command can be a simple string or a table with options.
#
# Simple: /commandname shows the response locally
#   commandname = "response text"
#
# Advanced: broadcast sends it as a chat message everyone sees
#   [commands.commandname]
#   response = "response text"
#   description = "Short help text"
#   broadcast = true
#
# Template variables: {nick} {channel} {role} {peers}
#
# Example:
# [commands]
# rules = "1. Be respectful  2. No spam  3. Use headphones"
# gg = "{nick} says GG!"
#
# [commands.roll]
# response = "{nick} rolled a random number!"
# description = "Roll dice"
# broadcast = true
"#;
        let _ = std::fs::write(path, example);
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.commands.get(name).map(|e| e.response.as_str())
    }

    pub fn is_broadcast(&self, name: &str) -> bool {
        self.commands.get(name).map(|e| e.broadcast).unwrap_or(false)
    }
}

pub struct CommandContext {
    pub nick: String,
    pub channel: String,
    pub role: Role,
    pub peers: Vec<String>,
}

fn expand_template(template: &str, ctx: &CommandContext) -> String {
    template
        .replace("{nick}", &ctx.nick)
        .replace("{channel}", &ctx.channel)
        .replace("{role}", ctx.role.as_str())
        .replace("{peers}", &ctx.peers.join(", "))
}
