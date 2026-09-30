//! The agent's config directory: `agent.toml`, plus this machine's certificate and key.

use std::fs;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use crossglide_touch::edge::Edge;
use crossglide_touch::keymap::{CommandKey, Hotkey};
use serde::Deserialize;

use crate::identity::Fingerprint;
use crate::link::Mode;
use crate::touch::TouchSettings;

pub const FILE: &str = "agent.toml";

/// Default UDP port of the side channel. Deskflow uses TCP 24800; TCP and UDP ports don't
/// collide, so the side channel takes the same number on UDP.
const DEFAULT_PORT: u16 = 24800;

/// Which end of the side channel this machine is. The PC connects to the Mac, the same direction
/// as the Deskflow client and server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Listen,
    Connect,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub role: Role,
    #[serde(default = "default_port")]
    pub port: u16,
    /// The Mac's address; only used when connecting.
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub peer_fingerprint: String,
    #[serde(default)]
    pub touch: TouchConfig,
}

/// `[touch]`: the Mac's trackpad and keyboard controlling the PC. Only the Mac reads it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TouchConfig {
    pub enabled: bool,
    /// Sides of the Mac's screen that lead to the PC; pushing the pointer through one moves
    /// control there, and the PC's opposite side leads back. Empty for the hotkey only.
    pub edges: Vec<Edge>,
    /// Moves control to the PC and back.
    pub hotkey: String,
    /// What the Command keys are on the PC.
    pub command: CommandKey,
}

impl Default for TouchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            edges: vec![Edge::Left, Edge::Right],
            hotkey: Hotkey::DEFAULT.to_string(),
            command: CommandKey::Win,
        }
    }
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

/// `crossglide` in the user's config directory: `~/Library/Application Support` on macOS,
/// `%APPDATA%` on Windows.
pub fn default_dir() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("this user has no config directory; pass --config-dir")?
        .join("crossglide"))
}

impl Config {
    /// Reads `path`. If it doesn't exist, writes a template there first and returns `None`.
    pub fn load_or_init(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(path, template(default_role()))
                .with_context(|| format!("can't write {}", path.display()))?;
            return Ok(None);
        }
        let text =
            fs::read_to_string(path).with_context(|| format!("can't read {}", path.display()))?;
        Ok(Some(toml::from_str(&text)?))
    }

    pub fn mode(&self) -> Result<Mode> {
        Ok(match self.role {
            Role::Listen => Mode::Listen(SocketAddr::from((Ipv4Addr::UNSPECIFIED, self.port))),
            Role::Connect => {
                let host = self.server.trim();
                if host.is_empty() {
                    bail!("set `server` to the Mac's IP address or host name");
                }
                Mode::Connect {
                    host: host.to_string(),
                    port: self.port,
                }
            }
        })
    }

    /// The touch settings, or `None` with touch turned off.
    pub fn touch(&self) -> Result<Option<TouchSettings>> {
        let touch = &self.touch;
        if !touch.enabled {
            return Ok(None);
        }
        Ok(Some(TouchSettings {
            edges: touch.edges.clone(),
            hotkey: touch.hotkey.parse().context("in [touch]")?,
            command: touch.command,
        }))
    }

    pub fn peer_fingerprint(&self) -> Result<Fingerprint> {
        if self.peer_fingerprint.trim().is_empty() {
            bail!(
                "set `peer_fingerprint` to the other machine's fingerprint \
                 (run `just dev fingerprint` there)"
            );
        }
        self.peer_fingerprint
            .parse()
            .context("`peer_fingerprint` isn't a valid fingerprint")
    }
}

/// The Mac listens and the PC connects.
fn default_role() -> Role {
    if cfg!(target_os = "macos") {
        Role::Listen
    } else {
        Role::Connect
    }
}

fn template(role: Role) -> String {
    let role = match role {
        Role::Listen => "listen",
        Role::Connect => "connect",
    };
    format!(
        r#"# Crossglide agent config (see ROADMAP.md, M2).

# "listen" on the Mac, "connect" on the PC. The PC connects to the Mac.
role = "{role}"

# UDP port of the side channel, the same on both machines.
port = {DEFAULT_PORT}

# The Mac's IP address or host name. Only used when role = "connect".
server = ""

# The other machine's certificate fingerprint: run `just dev fingerprint` on the
# other machine and paste its output here.
peer_fingerprint = ""

# The Mac's trackpad and keyboard controlling the PC, which sees a precision
# touchpad (install drivers/touchpad on the PC first). Only the Mac reads this.
[touch]
enabled = true
# Sides of the Mac's screen the PC is on: "left", "right", "top", "bottom".
edges = ["left", "right"]
# Moves control to the PC and back.
hotkey = "{hotkey}"
# The Command keys on the PC: "win" (as in Deskflow) or "ctrl" (Cmd-C copies).
command = "win"
"#,
        hotkey = Hotkey::DEFAULT
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Config {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn templates_parse_and_ask_for_the_missing_values() {
        let listen = parse(&template(Role::Listen));
        assert_eq!(listen.role, Role::Listen);
        assert_eq!(listen.port, DEFAULT_PORT);
        assert!(matches!(listen.mode().unwrap(), Mode::Listen(a) if a.port() == DEFAULT_PORT));
        assert!(listen.peer_fingerprint().is_err());

        let connect = parse(&template(Role::Connect));
        assert!(connect.mode().is_err(), "server is still empty");
    }

    #[test]
    fn filled_in_connect_config() {
        let config = parse(&format!(
            "role = \"connect\"\nserver = \"192.168.1.20\"\nport = 5000\npeer_fingerprint = \"{}\"",
            "ab".repeat(32)
        ));
        match config.mode().unwrap() {
            Mode::Connect { host, port } => {
                assert_eq!((host.as_str(), port), ("192.168.1.20", 5000))
            }
            Mode::Listen(_) => panic!("expected connect"),
        }
        assert!(config.peer_fingerprint().is_ok());
    }

    #[test]
    fn touch_defaults_and_settings() {
        let config = parse("role = \"listen\"");
        let touch = config.touch().unwrap().unwrap();
        assert_eq!(touch.edges, [Edge::Left, Edge::Right]);
        assert_eq!(touch.hotkey, Hotkey::DEFAULT.parse().unwrap());

        let config = parse(
            "role = \"listen\"\n[touch]\nedges = [\"top\"]\nhotkey = \"cmd+f1\"\ncommand = \"ctrl\"",
        );
        let touch = config.touch().unwrap().unwrap();
        assert_eq!(touch.edges, [Edge::Top]);
        assert_eq!(touch.command, CommandKey::Ctrl);

        assert!(
            parse("role = \"listen\"\n[touch]\nhotkey = \"f1\"")
                .touch()
                .is_err()
        );
        assert!(
            parse("role = \"listen\"\n[touch]\nenabled = false")
                .touch()
                .unwrap()
                .is_none()
        );
        assert!(toml::from_str::<Config>("role = \"listen\"\n[touch]\nedge = []").is_err());
    }

    #[test]
    fn typos_are_errors() {
        assert!(toml::from_str::<Config>("role = \"listen\"\nprot = 1").is_err());
        assert!(toml::from_str::<Config>("role = \"server\"").is_err());
    }

    #[test]
    fn missing_file_gets_a_template() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join(FILE);
        assert!(Config::load_or_init(&path).unwrap().is_none());
        let config = Config::load_or_init(&path).unwrap().unwrap();
        assert_eq!(config.role, default_role());
    }
}
