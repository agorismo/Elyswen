use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "kiln")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".kiln"))
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Application (client) ID do app registrado no Azure (ver README).
    pub client_id: String,
    /// Application ID do Discord Developer Portal (Rich Presence).
    pub discord_app_id: String,
    pub rpc: bool,
    pub ram_mb: u32,
    /// Vazio = usa o Java oficial baixado automaticamente.
    pub java_path: String,
    pub jvm_args: String,
    /// Uma pasta de jogo separada por versão.
    pub isolate: bool,
    pub snapshots: bool,
    pub old: bool,
    pub last_version: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            client_id: std::env::var("KILN_CLIENT_ID").unwrap_or_default(),
            discord_app_id: std::env::var("KILN_DISCORD_APP_ID").unwrap_or_default(),
            rpc: true,
            ram_mb: 4096,
            java_path: String::new(),
            jvm_args: String::new(),
            isolate: false,
            snapshots: false,
            old: false,
            last_version: String::new(),
        }
    }
}

impl Settings {
    fn path() -> PathBuf {
        data_dir().join("settings.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(data_dir());
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(Self::path(), s);
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Account {
    pub name: String,
    pub uuid: String,
    pub mc_token: String,
    /// Unix timestamp (s) em que o token do Minecraft expira.
    pub mc_expires: u64,
    pub refresh: String,
}

impl Account {
    fn path() -> PathBuf {
        data_dir().join("account.json")
    }

    pub fn load() -> Option<Self> {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(data_dir())?;
        let p = Self::path();
        std::fs::write(&p, serde_json::to_string_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    pub fn delete() {
        let _ = std::fs::remove_file(Self::path());
    }
}
