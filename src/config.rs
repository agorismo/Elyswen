use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn data_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "kiln")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".kiln"))
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------- configurações

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

// ---------------------------------------------------------------- instâncias

/// Uma instalação de jogo: versão + loader + pasta própria (saves, mods, config).
#[derive(Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Instance {
    pub id: String,
    pub name: String,
    pub version: String,
    /// "vanilla" ou "fabric".
    pub loader: String,
    pub playtime_secs: u64,
    pub last_played: u64,
}

pub fn instance_dir(id: &str) -> PathBuf {
    data_dir().join("instances").join(id)
}

pub fn mods_dir(id: &str) -> PathBuf {
    instance_dir(id).join("mods")
}

fn instances_path() -> PathBuf {
    data_dir().join("instances.json")
}

pub fn load_instances() -> Vec<Instance> {
    std::fs::read_to_string(instances_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_instances(list: &[Instance]) -> Result<()> {
    std::fs::create_dir_all(data_dir())?;
    std::fs::write(instances_path(), serde_json::to_string_pretty(list)?)?;
    Ok(())
}

pub fn add_playtime(id: &str, secs: u64) -> Result<()> {
    let mut list = load_instances();
    if let Some(i) = list.iter_mut().find(|i| i.id == id) {
        i.playtime_secs += secs;
        i.last_played = now();
        save_instances(&list)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- histórico de sessões

#[derive(Serialize, Deserialize, Clone)]
pub struct Session {
    pub instance: String,
    pub version: String,
    pub start: u64,
    pub secs: u64,
}

fn sessions_path() -> PathBuf {
    data_dir().join("sessions.json")
}

pub fn load_sessions() -> Vec<Session> {
    std::fs::read_to_string(sessions_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn record_session(s: Session) -> Result<()> {
    let mut list = load_sessions();
    list.push(s);
    if list.len() > 500 {
        let excess = list.len() - 500;
        list.drain(..excess);
    }
    std::fs::create_dir_all(data_dir())?;
    std::fs::write(sessions_path(), serde_json::to_string(&list)?)?;
    Ok(())
}

// ---------------------------------------------------------------- conta

#[derive(Serialize, Deserialize, Clone)]
pub struct Account {
    pub name: String,
    pub uuid: String,
    pub mc_token: String,
    /// Unix timestamp (s) em que o token do Minecraft expira.
    pub mc_expires: u64,
    /// Fica vazio quando o refresh token está no cofre do sistema.
    pub refresh: String,
}

impl Account {
    fn path() -> PathBuf {
        data_dir().join("account.json")
    }

    pub fn load() -> Option<Self> {
        let mut a: Self = std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())?;
        if a.refresh.is_empty() {
            a.refresh = keyring_entry(&a.uuid)
                .and_then(|e| e.get_password().ok())
                .unwrap_or_default();
        }
        Some(a)
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(data_dir())?;
        let mut stored = self.clone();
        // o refresh token vai para o cofre do sistema; sem cofre, fica no arquivo
        if keyring_store(&self.uuid, &self.refresh) {
            stored.refresh.clear();
        }
        let p = Self::path();
        std::fs::write(&p, serde_json::to_string_pretty(&stored)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    pub fn delete() {
        if let Some(a) = Self::load() {
            if let Some(e) = keyring_entry(&a.uuid) {
                let _ = e.delete_credential();
            }
        }
        let _ = std::fs::remove_file(Self::path());
    }
}

fn keyring_entry(uuid: &str) -> Option<keyring::Entry> {
    if uuid.is_empty() {
        return None;
    }
    keyring::Entry::new("kiln", &format!("ms-refresh-{uuid}")).ok()
}

fn keyring_store(uuid: &str, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let Some(e) = keyring_entry(uuid) else { return false };
    if e.set_password(token).is_err() {
        return false;
    }
    // alguns backends aceitam e não guardam; confere antes de confiar
    e.get_password().map(|t| t == token).unwrap_or(false)
}
