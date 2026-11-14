//! Discord Rich Presence em thread própria (reconecta se o Discord abrir depois).

use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone)]
enum Msg {
    Menu,
    Playing(String),
    Quit,
}

pub struct Rpc {
    tx: Sender<Msg>,
    pub app_id: String,
}

impl Rpc {
    pub fn start(app_id: String) -> Self {
        let (tx, rx) = channel::<Msg>();
        let id = app_id.clone();
        std::thread::spawn(move || {
            let mut client: Option<DiscordIpcClient> = None;
            let mut state = Msg::Menu;
            let mut since = now();
            loop {
                match rx.recv_timeout(Duration::from_secs(15)) {
                    Ok(Msg::Quit) | Err(RecvTimeoutError::Disconnected) => {
                        if let Some(c) = client.as_mut() {
                            let _ = c.clear_activity();
                            let _ = c.close();
                        }
                        return;
                    }
                    Ok(m) => {
                        state = m;
                        since = now();
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if client.is_some() {
                            continue; // já conectado, nada a fazer
                        }
                    }
                }
                if client.is_none() {
                    let mut c = DiscordIpcClient::new(&id);
                    if c.connect().is_ok() {
                        client = Some(c);
                    }
                }
                if let Some(c) = client.as_mut() {
                    let (details, st, ts) = match &state {
                        Msg::Playing(v) => (format!("Jogando Minecraft {v}"), "via kiln".to_string(), Some(since)),
                        _ => ("No launcher".to_string(), "kiln".to_string(), None),
                    };
                    let mut act = activity::Activity::new().details(&details).state(&st).assets(
                        activity::Assets::new()
                            .large_image("minecraft")
                            .large_text("kiln — launcher minimalista"),
                    );
                    if let Some(t) = ts {
                        act = act.timestamps(activity::Timestamps::new().start(t));
                    }
                    if c.set_activity(act).is_err() {
                        client = None; // Discord fechou; tenta reconectar depois
                    }
                }
            }
        });
        Self { tx, app_id }
    }

    pub fn menu(&self) {
        let _ = self.tx.send(Msg::Menu);
    }

    pub fn playing(&self, version: &str) {
        let _ = self.tx.send(Msg::Playing(version.to_string()));
    }
}

impl Drop for Rpc {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Quit);
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
