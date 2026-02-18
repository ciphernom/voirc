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
    SetPow(u8),
    MineNick { bits: u8 },
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
           "/powset" => arg
               .and_then(|a| a.parse::<u8>().ok())
               .map(Command::SetPow)
               .or(Some(Command::Unknown("/powset <bits>".to_string()))),
           "/mine" => Some(Command::MineNick {
               bits: arg.and_then(|a| a.parse().ok()).unwrap_or(16),
           }),
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
   lines.push(format!(
       "/mine [bits]    Mine a new nick at N bits (default 16, ~{})",
       crate::pow::time_estimate(16)
   ));
   
    if our_role.can_moderate() {
        lines.push("/kick <nick>    Disconnect a user".to_string());
        lines.push("/ban <nick>     Ban a user".to_string());
        lines.push("/unban <nick>   Unban a user".to_string());
        lines.push("/listbanned     Show ban list".to_string());
        lines.push("/powset <bits>  Change server PoW requirement (0=off)".to_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_command_not_command() {
        let custom = CustomCommands::default();
        let ctx = CommandContext {
            nick: "test".to_string(),
            channel: "#general".to_string(),
            role: Role::Peer,
            peers: vec![],
        };
        assert!(parse_command("hello world", &custom, &ctx).is_none());
    }

    #[test]
    fn test_parse_command_help() {
        let custom = CustomCommands::default();
        let ctx = CommandContext {
            nick: "test".to_string(),
            channel: "#general".to_string(),
            role: Role::Peer,
            peers: vec![],
        };
        assert!(matches!(parse_command("/help", &custom, &ctx), Some(Command::Help)));
        assert!(matches!(parse_command("/?", &custom, &ctx), Some(Command::Help)));
    }

    #[test]
    fn test_parse_command_kick() {
        let custom = CustomCommands::default();
        let ctx = CommandContext {
            nick: "test".to_string(),
            channel: "#general".to_string(),
            role: Role::Peer,
            peers: vec![],
        };
        let result = parse_command("/kick baduser", &custom, &ctx);
        assert!(matches!(result, Some(Command::Mod(ModAction::Kick(nick))) if nick == "baduser"));
    }

    #[test]
    fn test_parse_command_ban() {
        let custom = CustomCommands::default();
        let ctx = CommandContext {
            nick: "test".to_string(),
            channel: "#general".to_string(),
            role: Role::Peer,
            peers: vec![],
        };
        let result = parse_command("/ban spammer", &custom, &ctx);
        assert!(matches!(result, Some(Command::Mod(ModAction::Ban(nick))) if nick == "spammer"));
    }

    #[test]
    fn test_parse_command_unban() {
        let custom = CustomCommands::default();
        let ctx = CommandContext {
            nick: "test".to_string(),
            channel: "#general".to_string(),
            role: Role::Peer,
            peers: vec![],
        };
        let result = parse_command("/unban reformed_user", &custom, &ctx);
        assert!(matches!(result, Some(Command::Mod(ModAction::Unban(nick))) if nick == "reformed_user"));
    }

    #[test]
    fn test_parse_command_promote() {
        let custom = CustomCommands::default();
        let ctx = CommandContext {
            nick: "test".to_string(),
            channel: "#general".to_string(),
            role: Role::Peer,
            peers: vec![],
        };
        let result = parse_command("/mod newmod", &custom, &ctx);
        assert!(matches!(result, Some(Command::Mod(ModAction::Promote(nick))) if nick == "newmod"));
    }

    #[test]
    fn test_parse_command_demote() {
        let custom = CustomCommands::default();
        let ctx = CommandContext {
            nick: "test".to_string(),
            channel: "#general".to_string(),
            role: Role::Peer,
            peers: vec![],
        };
        let result = parse_command("/unmod exmod", &custom, &ctx);
        assert!(matches!(result, Some(Command::Mod(ModAction::Demote(nick))) if nick == "exmod"));
    }

    #[test]
    fn test_parse_command_listbanned() {
        let custom = CustomCommands::default();
        let ctx = CommandContext {
            nick: "test".to_string(),
            channel: "#general".to_string(),
            role: Role::Peer,
            peers: vec![],
        };
        assert!(matches!(parse_command("/listbanned", &custom, &ctx), Some(Command::ListBanned)));
        assert!(matches!(parse_command("/bans", &custom, &ctx), Some(Command::ListBanned)));
    }

    #[test]
    fn test_parse_command_role() {
        let custom = CustomCommands::default();
        let ctx = CommandContext {
            nick: "test".to_string(),
            channel: "#general".to_string(),
            role: Role::Peer,
            peers: vec![],
        };
        assert!(matches!(parse_command("/role", &custom, &ctx), Some(Command::ShowRole)));
    }

    #[test]
    fn test_parse_command_peers() {
        let custom = CustomCommands::default();
        let ctx = CommandContext {
            nick: "test".to_string(),
            channel: "#general".to_string(),
            role: Role::Peer,
            peers: vec![],
        };
        assert!(matches!(parse_command("/peers", &custom, &ctx), Some(Command::ListPeers)));
        assert!(matches!(parse_command("/who", &custom, &ctx), Some(Command::ListPeers)));
    }

    #[test]
    fn test_parse_command_unknown() {
        let custom = CustomCommands::default();
        let ctx = CommandContext {
            nick: "test".to_string(),
            channel: "#general".to_string(),
            role: Role::Peer,
            peers: vec![],
        };
        // Fix: Expect "/unknowncommand" since the parser keeps the slash
        assert!(matches!(parse_command("/unknowncommand", &custom, &ctx), Some(Command::Unknown(cmd)) if cmd == "/unknowncommand"));
    }

    #[test]
    fn test_check_permission_kick_ban_host() {
        assert!(check_permission(Role::Host, &ModAction::Kick("user".to_string())).is_ok());
        assert!(check_permission(Role::Host, &ModAction::Ban("user".to_string())).is_ok());
        assert!(check_permission(Role::Host, &ModAction::Unban("user".to_string())).is_ok());
    }

    #[test]
    fn test_check_permission_kick_ban_mod() {
        assert!(check_permission(Role::Mod, &ModAction::Kick("user".to_string())).is_ok());
        assert!(check_permission(Role::Mod, &ModAction::Ban("user".to_string())).is_ok());
        assert!(check_permission(Role::Mod, &ModAction::Unban("user".to_string())).is_ok());
    }

    #[test]
    fn test_check_permission_kick_ban_peer_fails() {
        assert!(check_permission(Role::Peer, &ModAction::Kick("user".to_string())).is_err());
        assert!(check_permission(Role::Peer, &ModAction::Ban("user".to_string())).is_err());
        assert!(check_permission(Role::Peer, &ModAction::Unban("user".to_string())).is_err());
    }

    #[test]
    fn test_check_permission_promote_host_only() {
        assert!(check_permission(Role::Host, &ModAction::Promote("user".to_string())).is_ok());
        assert!(check_permission(Role::Host, &ModAction::Demote("user".to_string())).is_ok());

        assert!(check_permission(Role::Mod, &ModAction::Promote("user".to_string())).is_err());
        assert!(check_permission(Role::Mod, &ModAction::Demote("user".to_string())).is_err());

        assert!(check_permission(Role::Peer, &ModAction::Promote("user".to_string())).is_err());
        assert!(check_permission(Role::Peer, &ModAction::Demote("user".to_string())).is_err());
    }

    #[test]
    fn test_help_text_peer() {
        let custom = CustomCommands::default();
        let help = help_text(Role::Peer, &custom);

        // Fix: Logic inverted. Should check that it does NOT contain kick.
        assert!(!help.iter().any(|l| l.contains("/kick")));
        assert!(!help.iter().any(|l| l.contains("/mod")));
        assert!(help.iter().any(|l| l.contains("/help")));
    }

    #[test]
    fn test_help_text_mod() {
        let custom = CustomCommands::default();
        let help = help_text(Role::Mod, &custom);

        assert!(help.iter().any(|l| l.contains("/kick")));
        assert!(help.iter().any(|l| l.contains("/ban")));
        assert!(!help.iter().any(|l| l.contains("/mod")));
    }

    #[test]
    fn test_help_text_host() {
        let custom = CustomCommands::default();
        let help = help_text(Role::Host, &custom);

        assert!(help.iter().any(|l| l.contains("/kick")));
        assert!(help.iter().any(|l| l.contains("/mod")));
    }

    #[test]
    fn test_expand_template() {
        let ctx = CommandContext {
            nick: "alice".to_string(),
            channel: "#general".to_string(),
            role: Role::Host,
            peers: vec!["bob".to_string(), "charlie".to_string()],
        };

        assert_eq!(expand_template("Hello {nick}", &ctx), "Hello alice");
        assert_eq!(expand_template("In {channel}", &ctx), "In #general");
        assert_eq!(expand_template("Your role is {role}", &ctx), "Your role is host");
        assert_eq!(expand_template("Peers: {peers}", &ctx), "Peers: bob, charlie");
    }
}
