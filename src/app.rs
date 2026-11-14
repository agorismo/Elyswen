use crate::auth;
use crate::config::{data_dir, Account, Settings};
use crate::events::{Emitter, Event};
use crate::install::{self, Manifest, VersionEntry};
use crate::launch;
use crate::rpc::Rpc;
use eframe::egui::{self, Color32, RichText};
use reqwest::blocking::Client;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

const ACCENT: Color32 = Color32::from_rgb(110, 231, 143);
const BG: Color32 = Color32::from_rgb(13, 13, 15);
const PANEL: Color32 = Color32::from_rgb(22, 22, 26);
const MUTED: Color32 = Color32::from_rgb(140, 140, 150);

#[derive(PartialEq, Clone, Copy)]
enum Busy {
    Idle,
    LoggingIn,
    Working,
    Running,
}

pub struct App {
    settings: Settings,
    saved: Settings,
    account: Option<Account>,
    manifest: Option<Manifest>,
    selected: String,
    status: String,
    error: bool,
    progress: f32,
    busy: Busy,
    code: Option<(String, String)>,
    crash: Vec<String>,
    show_settings: bool,
    rx: Receiver<Event>,
    em: Emitter,
    rpc: Option<Rpc>,
    client: Client,
}

pub fn http_client() -> Client {
    Client::builder()
        .user_agent(format!("kiln/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .expect("http client")
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut v = egui::Visuals::dark();
        v.panel_fill = BG;
        v.window_fill = PANEL;
        v.extreme_bg_color = PANEL;
        v.widgets.inactive.bg_fill = PANEL;
        v.widgets.inactive.weak_bg_fill = PANEL;
        v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
        cc.egui_ctx.set_visuals(v);

        let (tx, rx) = channel();
        let em = Emitter::new(tx, Some(cc.egui_ctx.clone()));
        let settings = Settings::load();
        let client = http_client();

        // carrega a lista de versões em segundo plano
        {
            let (em, client) = (em.clone(), client.clone());
            std::thread::spawn(move || match install::fetch_manifest(&client, &data_dir()) {
                Ok(m) => em.send(Event::Manifest(m)),
                Err(e) => em.send(Event::Error(format!("{e:#}"))),
            });
        }

        let mut app = Self {
            selected: settings.last_version.clone(),
            saved: settings.clone(),
            settings,
            account: Account::load(),
            manifest: None,
            status: "Carregando versões...".into(),
            error: false,
            progress: 0.0,
            busy: Busy::Idle,
            code: None,
            crash: Vec::new(),
            show_settings: false,
            rx,
            em,
            rpc: None,
            client,
        };
        app.sync_rpc();
        app
    }

    fn sync_rpc(&mut self) {
        let want = self.settings.rpc && !self.settings.discord_app_id.trim().is_empty();
        let id = self.settings.discord_app_id.trim().to_string();
        match (&self.rpc, want) {
            (Some(r), true) if r.app_id == id => {}
            (_, true) => {
                let r = Rpc::start(id);
                r.menu();
                self.rpc = Some(r);
            }
            (_, false) => self.rpc = None,
        }
    }

    fn drain(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::Status(s) => {
                    self.status = s;
                    self.error = false;
                }
                Event::Progress(p) => self.progress = p,
                Event::DeviceCode { code, url } => {
                    self.status = "Digite o código no site da Microsoft".into();
                    let _ = open::that(&url);
                    self.code = Some((code, url));
                }
                Event::Account(a) => {
                    let _ = a.save();
                    if self.busy == Busy::LoggingIn {
                        self.busy = Busy::Idle;
                        self.status = format!("Conectado como {}", a.name);
                    }
                    self.code = None;
                    self.account = Some(a);
                }
                Event::Manifest(m) => {
                    if self.selected.is_empty() || !m.versions.iter().any(|v| v.id == self.selected) {
                        self.selected = m.latest.release.clone();
                    }
                    self.status = "Pronto".into();
                    self.manifest = Some(m);
                }
                Event::Started(v) => {
                    self.busy = Busy::Running;
                    self.status = format!("Minecraft {v} em execução");
                    if let Some(r) = &self.rpc {
                        r.playing(&v);
                    }
                }
                Event::Exited(code) => {
                    self.busy = Busy::Idle;
                    self.progress = 0.0;
                    self.status = match code {
                        Some(0) => "Jogo fechado".into(),
                        Some(c) => format!("O jogo fechou com erro (código {c})"),
                        None => "O jogo foi encerrado".into(),
                    };
                    self.error = code != Some(0);
                    if let Some(r) = &self.rpc {
                        r.menu();
                    }
                }
                Event::CrashLog(lines) => self.crash = lines,
                Event::Error(e) => {
                    self.status = e;
                    self.error = true;
                    self.busy = Busy::Idle;
                    self.code = None;
                    self.progress = 0.0;
                }
                Event::Idle => self.busy = Busy::Idle,
            }
        }
    }

    fn start_login(&mut self) {
        if self.settings.client_id.trim().is_empty() {
            self.show_settings = true;
            self.status = "Defina o Client ID do Azure (veja o README)".into();
            self.error = true;
            return;
        }
        self.busy = Busy::LoggingIn;
        self.error = false;
        let (em, client, id) = (self.em.clone(), self.client.clone(), self.settings.client_id.trim().to_string());
        std::thread::spawn(move || match auth::login(&client, &id, &em) {
            Ok(a) => em.send(Event::Account(a)),
            Err(e) => em.send(Event::Error(format!("{e:#}"))),
        });
    }

    fn play(&mut self) {
        let Some(manifest) = &self.manifest else { return };
        let Some(entry) = manifest.versions.iter().find(|v| v.id == self.selected).cloned() else { return };
        let Some(acct) = self.account.clone() else {
            self.start_login();
            return;
        };
        self.busy = Busy::Working;
        self.error = false;
        self.progress = 0.0;
        self.crash.clear();
        self.settings.last_version = entry.id.clone();

        let (em, client, settings) = (self.em.clone(), self.client.clone(), self.settings.clone());
        std::thread::spawn(move || {
            if let Err(e) = play_flow(&client, &settings, acct, &entry, &em) {
                em.send(Event::Error(format!("{e:#}")));
            }
        });
    }

    fn visible_versions(&self) -> Vec<&VersionEntry> {
        let Some(m) = &self.manifest else { return Vec::new() };
        m.versions
            .iter()
            .filter(|v| match v.kind.as_str() {
                "release" => true,
                "snapshot" => self.settings.snapshots,
                _ => self.settings.old,
            })
            .collect()
    }
}

fn play_flow(client: &Client, settings: &Settings, acct: Account, entry: &VersionEntry, em: &Emitter) -> anyhow::Result<()> {
    let root = data_dir();
    em.status("Verificando login...");
    let acct = auth::ensure_fresh(client, settings.client_id.trim(), &acct)?;
    em.send(Event::Account(acct.clone()));

    let gdir = launch::game_dir(&root, settings, &entry.id);
    let prep = install::install(client, &root, entry, &settings.java_path, &gdir, em)?;
    launch::run(&root, settings, &prep, &acct, &gdir, em)
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        self.drain();

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(24.0))
            .show(ui, |ui| {
                // topo: nome + conta
                ui.horizontal(|ui| {
                    ui.label(RichText::new("kiln").size(22.0).strong().color(ACCENT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("⚙").clicked() {
                            self.show_settings = !self.show_settings;
                        }
                        match self.account.clone() {
                            Some(a) => {
                                if self.busy == Busy::Idle && ui.small_button("sair").clicked() {
                                    Account::delete();
                                    self.account = None;
                                }
                                ui.label(RichText::new(&a.name).color(Color32::WHITE));
                            }
                            None => {
                                let can = self.busy == Busy::Idle;
                                if ui.add_enabled(can, egui::Button::new("Entrar com a Microsoft")).clicked() {
                                    self.start_login();
                                }
                            }
                        }
                    });
                });
                ui.add_space(18.0);

                // código do device login
                if let Some((code, url)) = self.code.clone() {
                    egui::Frame::new().fill(PANEL).inner_margin(14.0).show(ui, |ui| {
                        ui.label(RichText::new("Seu código de login").color(MUTED));
                        ui.label(RichText::new(&code).size(30.0).strong().monospace().color(ACCENT));
                        ui.horizontal(|ui| {
                            if ui.button("Copiar código").clicked() {
                                ctx.copy_text(code.clone());
                            }
                            if ui.button("Abrir página").clicked() {
                                let _ = open::that(&url);
                            }
                        });
                    });
                    ui.add_space(12.0);
                }

                // seleção de versão
                let idle = self.busy == Busy::Idle;
                ui.label(RichText::new("VERSÃO").size(11.0).color(MUTED));
                let versions: Vec<(String, String)> = self
                    .visible_versions()
                    .iter()
                    .map(|v| (v.id.clone(), v.kind.clone()))
                    .collect();
                let latest = self.manifest.as_ref().map(|m| m.latest.release.clone()).unwrap_or_default();
                ui.add_enabled_ui(idle, |ui| {
                    egui::ComboBox::from_id_salt("version")
                        .width(ui.available_width())
                        .selected_text(self.selected.clone())
                        .show_ui(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                                for (id, kind) in &versions {
                                    let mut label = id.clone();
                                    if *id == latest {
                                        label.push_str("  · latest");
                                    } else if kind != "release" {
                                        label.push_str(&format!("  · {kind}"));
                                    }
                                    ui.selectable_value(&mut self.selected, id.clone(), label);
                                }
                            });
                        });
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.settings.snapshots, "snapshots");
                        ui.checkbox(&mut self.settings.old, "beta / alpha");
                    });
                });

                ui.add_space(16.0);

                // botão principal
                let (label, enabled) = match self.busy {
                    Busy::Idle => ("JOGAR", self.manifest.is_some()),
                    Busy::LoggingIn => ("AGUARDANDO LOGIN...", false),
                    Busy::Working => ("PREPARANDO...", false),
                    Busy::Running => ("EM EXECUÇÃO", false),
                };
                let btn = egui::Button::new(RichText::new(label).size(20.0).strong().color(Color32::BLACK)).fill(ACCENT);
                if ui.add_enabled(enabled, btn.min_size(egui::vec2(ui.available_width(), 54.0))).clicked() {
                    self.play();
                }

                ui.add_space(14.0);
                if self.busy == Busy::Working {
                    ui.add(egui::ProgressBar::new(self.progress).fill(ACCENT).desired_height(6.0));
                    ui.add_space(6.0);
                }
                let color = if self.error { Color32::from_rgb(255, 120, 120) } else { MUTED };
                ui.add(egui::Label::new(RichText::new(&self.status).color(color)).wrap());

                if !self.crash.is_empty() {
                    ui.add_space(10.0);
                    egui::CollapsingHeader::new("últimas linhas do log").show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                            for l in &self.crash {
                                ui.label(RichText::new(l).monospace().size(11.0).color(MUTED));
                            }
                        });
                    });
                }

                if self.show_settings {
                    ui.add_space(16.0);
                    ui.separator();
                    ui.add_space(8.0);
                    egui::Grid::new("settings").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label("Client ID (Azure)");
                        ui.add(egui::TextEdit::singleline(&mut self.settings.client_id).desired_width(260.0));
                        ui.end_row();
                        ui.label("Memória");
                        ui.add(egui::Slider::new(&mut self.settings.ram_mb, 1024..=16384).step_by(512.0).suffix(" MB"));
                        ui.end_row();
                        ui.label("Java (vazio = automático)");
                        ui.add(egui::TextEdit::singleline(&mut self.settings.java_path).desired_width(260.0));
                        ui.end_row();
                        ui.label("Args da JVM");
                        ui.add(egui::TextEdit::singleline(&mut self.settings.jvm_args).desired_width(260.0));
                        ui.end_row();
                        ui.label("Discord App ID");
                        ui.add(egui::TextEdit::singleline(&mut self.settings.discord_app_id).desired_width(260.0));
                        ui.end_row();
                        ui.label("Discord Rich Presence");
                        ui.checkbox(&mut self.settings.rpc, "ativado");
                        ui.end_row();
                        ui.label("Pasta por versão");
                        ui.checkbox(&mut self.settings.isolate, "isolar saves/mods");
                        ui.end_row();
                    });
                    ui.add_space(6.0);
                    if ui.small_button("abrir pasta de dados").clicked() {
                        let _ = open::that(data_dir());
                    }
                }
            });

        if self.settings != self.saved {
            self.settings.save();
            self.saved = self.settings.clone();
            self.sync_rpc();
        }
        if self.busy == Busy::LoggingIn {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }
}
