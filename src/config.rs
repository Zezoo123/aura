//! User configuration (`~/.config/aura/config.toml`).

use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Spotify Developer app Client ID (PKCE flow, no secret needed).
    pub client_id: Option<String>,
    /// Loopback port used for the OAuth redirect (must match the dashboard).
    pub redirect_port: Option<u16>,
    /// Two-letter market for search results (defaults to the account's market).
    pub market: Option<String>,
}

pub const DEFAULT_REDIRECT_PORT: u16 = 8888;

pub fn config_dir() -> Result<PathBuf> {
    // ~/.config/aura on every platform: predictable and easy to document.
    let home = dirs::home_dir().context("no home directory")?;
    Ok(home.join(".config").join("aura"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

impl Config {
    pub fn load() -> Result<Config> {
        let path = config_path()?;
        match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        fs::create_dir_all(path.parent().unwrap())?;
        let text = toml::to_string_pretty(self)?;
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
    }

    pub fn redirect_port(&self) -> u16 {
        self.redirect_port.unwrap_or(DEFAULT_REDIRECT_PORT)
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.redirect_port())
    }
}
