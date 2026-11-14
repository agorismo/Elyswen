use crate::config::Account;
use crate::install::Manifest;
use crate::modrinth::Hit;
use std::sync::mpsc::Sender;

/// Mensagens enviadas pelas threads de trabalho para a interface.
pub enum Event {
    Status(String),
    /// Erro de uma ação que não derruba o jogo (ex.: mod não instalado).
    Warn(String),
    Progress(f32),
    DeviceCode { code: String, url: String },
    Account(Account),
    Manifest(Manifest),
    Started(String),
    Exited(Option<i32>),
    CrashLog { lines: Vec<String>, hint: Option<String> },
    Error(String),
    ModResults(Vec<Hit>),
    ModsChanged,
    UpdateAvailable { version: String, url: String },
    Idle,
}

#[derive(Clone)]
pub struct Emitter {
    tx: Sender<Event>,
    ctx: Option<eframe::egui::Context>,
}

impl Emitter {
    pub fn new(tx: Sender<Event>, ctx: Option<eframe::egui::Context>) -> Self {
        Self { tx, ctx }
    }

    pub fn send(&self, e: Event) {
        let _ = self.tx.send(e);
        if let Some(c) = &self.ctx {
            c.request_repaint();
        }
    }

    pub fn status(&self, s: impl Into<String>) {
        self.send(Event::Status(s.into()));
    }

    pub fn progress(&self, p: f32) {
        self.send(Event::Progress(p));
    }
}
