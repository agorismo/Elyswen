use crate::auth;
use crate::config::{self, data_dir, Account, Instance, Session, Settings};
use crate::events::{Emitter, Event};
use crate::install::{self, Manifest, VersionEntry};
use crate::launch;
use crate::modrinth::{self, Hit};
use crate::rpc::Rpc;
use eframe::egui::{self, Color32, RichText};
use reqwest::blocking::Client;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

const ACCENT: Color32 = Color32::from_rgb(110, 231, 143);
const BG: Color32 = Color32::from_rgb(13, 13, 15);
const PANEL: Color32 = Color32::from_rgb(22, 22, 26);
const MUTED: Color32 = Color32::from_rgb(140, 140, 150);
const ERR: Color32 = Color32::from_rgb(255, 120, 120);

#[derive(PartialEq, Clone, Copy)]
enum Busy {
    Idle,
    LoggingIn,
    Working,
    Running,
}

#[derive(PartialEq, Clone, Copy)]
enum Panel {
    Closed,
    Mods,
    Stats,
    Config,
}

pub struct App {
    settings: Settings,
    saved: Settings,
    account: Option<Account>,
    manifest: Option<Manifest>,
    instances: Vec<Instance>,
    /// id da instância selecionada
    current: Option<String>,
    new_open: bool,
    new_name: String,
    new_version: String,
    new_fabric: bool,
    status: String,
    error: bool,
    progress: f32,
    busy: Busy,
    code: Option<(String, String)>,
    crash: Vec<String>,
    crash_hint: Option<String>,
    show_settings: bool,
    panel: Panel,
    mod_query: String,
    mod_hits: Vec<Hit>,
    mods: Vec<String>,
    sessions: Vec<Session>,
    rx: Receiver<Event>,
    em: Emitter,
    rpc: Option<Rpc>,
    client: Client,
    update: Option<(String, String)>,
}

/// Repositório do launcher no GitHub, usado para avisar sobre versões novas.
const UPDATE_REPO: &str = "agorismo/Elyswen";

fn check_update(client: &Client) -> Option<(String, String)> {
    let v: serde_json::Value = client
        .get(format!("https://api.github.com/repos/{UPDATE_REPO}/releases/latest"))
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .ok()?;
    let tag = v["tag_name"].as_str()?.to_string();
    let url = v["html_url"].as_str()?.to_string();
    if version_parts(&tag) > version_parts(env!("CARGO_PKG_VERSION")) {
        Some((tag, url))
    } else {
        None
    }
}

fn version_parts(s: &str) -> Vec<u64> {
    s.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

pub fn http_client() -> Client {
    Client::builder()
        .user_agent(format!("elyswen/{}", env!("CARGO_PKG_VERSION")))
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

        {
            let (em, client) = (em.clone(), client.clone());
            std::thread::spawn(move || match install::fetch_manifest(&client, &data_dir()) {
                Ok(m) => em.send(Event::Manifest(m)),
                Err(e) => em.send(Event::Error(format!("{e:#}"))),
            });
        }

        {
            let (em, client) = (em.clone(), client.clone());
            std::thread::spawn(move || {
                if let Some((version, url)) = check_update(&client) {
                    em.send(Event::UpdateAvailable { version, url });
                }
            });
        }

        let instances = config::load_instances();
        let current = instances.first().map(|i| i.id.clone());
        let mut app = Self {
            new_version: settings.last_version.clone(),
            saved: settings.clone(),
            settings,
            account: Account::load(),
            manifest: None,
            instances,
            current,
            new_open: false,
            new_name: String::new(),
            new_fabric: false,
            status: "Carregando versões...".into(),
            error: false,
            progress: 0.0,
            busy: Busy::Idle,
            code: None,
            crash: Vec::new(),
            crash_hint: None,
            show_settings: false,
            panel: Panel::Closed,
            mod_query: String::new(),
            mod_hits: Vec::new(),
            mods: Vec::new(),
            sessions: config::load_sessions(),
            rx,
            em,
            rpc: None,
            client,
            update: None,
        };
        app.sync_rpc();
        app.refresh_mods();
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

    fn current_instance(&self) -> Option<Instance> {
        let id = self.current.as_ref()?;
        self.instances.iter().find(|i| &i.id == id).cloned()
    }

    fn refresh_mods(&mut self) {
        self.mods = match self.current_instance() {
            Some(i) => modrinth::list_installed(&config::mods_dir(&i.id)),
            None => Vec::new(),
        };
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

    fn drain(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::Status(s) => {
                    self.status = s;
                    self.error = false;
                }
                Event::Warn(s) => {
                    self.status = s;
                    self.error = true;
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
                    if self.new_version.is_empty() || !m.versions.iter().any(|v| v.id == self.new_version) {
                        self.new_version = m.latest.release.clone();
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
                    self.instances = config::load_instances();
                    self.sessions = config::load_sessions();
                    if let Some(r) = &self.rpc {
                        r.menu();
                    }
                }
                Event::CrashLog { lines, hint } => {
                    self.crash = lines;
                    self.crash_hint = hint;
                }
                Event::Error(e) => {
                    self.status = e;
                    self.error = true;
                    self.busy = Busy::Idle;
                    self.code = None;
                    self.progress = 0.0;
                }
                Event::ModResults(hits) => {
                    self.status = format!("{} resultados", hits.len());
                    self.error = false;
                    self.mod_hits = hits;
                }
                Event::ModsChanged => self.refresh_mods(),
                Event::UpdateAvailable { version, url } => self.update = Some((version, url)),
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
        let Some(inst) = self.current_instance() else { return };
        let Some(manifest) = &self.manifest else { return };
        let Some(entry) = manifest.versions.iter().find(|v| v.id == inst.version).cloned() else { return };
        let Some(acct) = self.account.clone() else {
            self.start_login();
            return;
        };
        self.busy = Busy::Working;
        self.error = false;
        self.progress = 0.0;
        self.crash.clear();
        self.crash_hint = None;
        self.settings.last_version = inst.version.clone();

        let (em, client, settings) = (self.em.clone(), self.client.clone(), self.settings.clone());
        std::thread::spawn(move || {
            if let Err(e) = play_flow(&client, &settings, acct, &entry, &inst, &em) {
                em.send(Event::Error(format!("{e:#}")));
            }
        });
    }

    fn create_instance(&mut self) {
        let version = self.new_version.clone();
        if version.is_empty() {
            return;
        }
        let loader = if self.new_fabric { "fabric" } else { "vanilla" };
        let name = match self.new_name.trim() {
            "" => format!("{version} {loader}"),
            n => n.to_string(),
        };
        let id = format!("{}-{}", config::now(), self.instances.len());
        self.instances.push(Instance {
            id: id.clone(),
            name,
            version,
            loader: loader.into(),
            playtime_secs: 0,
            last_played: 0,
            ..Default::default()
        });
        if let Err(e) = config::save_instances(&self.instances) {
            self.status = format!("erro ao salvar instância: {e}");
            self.error = true;
        }
        self.current = Some(id);
        self.new_open = false;
        self.new_name.clear();
        self.refresh_mods();
    }

    fn remove_instance(&mut self) {
        if let Some(id) = self.current.take() {
            self.instances.retain(|i| i.id != id);
            let _ = config::save_instances(&self.instances);
            self.current = self.instances.first().map(|i| i.id.clone());
            self.refresh_mods();
            self.status = "Removida da lista (os arquivos ficaram na pasta de dados)".into();
            self.error = false;
        }
    }

    fn search_mods(&mut self) {
        let q = self.mod_query.trim().to_string();
        let Some(inst) = self.current_instance() else { return };
        if q.is_empty() {
            return;
        }
        let (em, client) = (self.em.clone(), self.client.clone());
        self.status = "Buscando mods...".into();
        self.error = false;
        std::thread::spawn(move || match modrinth::search(&client, &q, &inst.version) {
            Ok(h) => em.send(Event::ModResults(h)),
            Err(e) => em.send(Event::Warn(format!("{e:#}"))),
        });
    }

    fn install_mod(&mut self, project: String) {
        let Some(inst) = self.current_instance() else { return };
        let (em, client) = (self.em.clone(), self.client.clone());
        self.status = "Instalando mod...".into();
        self.error = false;
        std::thread::spawn(move || {
            let dir = config::mods_dir(&inst.id);
            match modrinth::install(&client, &dir, &project, &inst.version, &em) {
                Ok(names) => {
                    em.status(format!("Instalado: {}", names.join(", ")));
                    em.send(Event::ModsChanged);
                }
                Err(e) => em.send(Event::Warn(format!("{e:#}"))),
            }
        });
    }

    fn remove_mod(&mut self, file: &str) {
        let Some(inst) = self.current_instance() else { return };
        if let Err(e) = modrinth::remove(&config::mods_dir(&inst.id), file) {
            self.status = format!("erro: {e}");
            self.error = true;
        }
        self.refresh_mods();
    }
}

fn play_flow(
    client: &Client,
    settings: &Settings,
    acct: Account,
    entry: &VersionEntry,
    inst: &Instance,
    em: &Emitter,
) -> anyhow::Result<()> {
    let root = data_dir();
    em.status("Verificando login...");
    let acct = auth::ensure_fresh(client, settings.client_id.trim(), &acct)?;
    em.send(Event::Account(acct.clone()));

    // a pasta da instância guarda saves, config e mods
    let gdir = config::instance_dir(&inst.id);
    let java = if inst.java_path.trim().is_empty() { settings.java_path.clone() } else { inst.java_path.clone() };
    let prep = install::install(client, &root, entry, &inst.loader, &java, &gdir, em)?;
    launch::run(&root, settings, &prep, &acct, &gdir, inst, em)
}

fn preset_label(p: &str) -> &'static str {
    match p {
        "aikar" => "Aikar (G1)",
        "zgc" => "ZGC",
        _ => "nenhum",
    }
}

fn instance_label(i: &Instance) -> String {
    let loader = if i.loader == "fabric" { " · Fabric" } else { "" };
    format!("{}  · {}{}", i.name, i.version, loader)
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s.to_string()
    }
}

fn fmt_dur(secs: u64) -> String {
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if h > 0 {
        format!("{h}h {m:02}min")
    } else {
        format!("{m}min")
    }
}

fn fmt_ago(secs: u64) -> String {
    match secs {
        0..=59 => "agora".into(),
        60..=3599 => format!("há {}min", secs / 60),
        3600..=86399 => format!("há {}h", secs / 3600),
        _ => format!("há {}d", secs / 86400),
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain();
        let accent = Color32::from_rgb(self.settings.accent[0], self.settings.accent[1], self.settings.accent[2]);

        // ações pedidas pela interface; executadas depois de desenhar
        let mut play_req = false;
        let mut search_req = false;
        let mut install_req: Option<String> = None;
        let mut remove_mod_req: Option<String> = None;
        let mut create_req = false;
        let mut remove_inst_req = false;

        let inst = self.current_instance();
        let versions: Vec<(String, String)> = self
            .visible_versions()
            .iter()
            .map(|v| (v.id.clone(), v.kind.clone()))
            .collect();
        let latest = self.manifest.as_ref().map(|m| m.latest.release.clone()).unwrap_or_default();
        let idle = self.busy == Busy::Idle;
        let now = config::now();

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(24.0))
            .show(ui, |ui| {
                // topo: nome + conta
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Elyswen").size(22.0).strong().color(accent));
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
                                if ui.add_enabled(idle, egui::Button::new("Entrar com a Microsoft")).clicked() {
                                    self.start_login();
                                }
                            }
                        }
                    });
                });
                if let Some((v, url)) = self.update.clone() {
                    egui::Frame::new().fill(PANEL).inner_margin(10.0).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("Nova versão {v} disponível")).color(accent));
                            if ui.small_button("ver release").clicked() {
                                let _ = open::that(&url);
                            }
                            if ui.small_button("ocultar").clicked() {
                                self.update = None;
                            }
                        });
                    });
                    ui.add_space(8.0);
                }
                ui.add_space(14.0);

                // código do device login
                if let Some((code, url)) = self.code.clone() {
                    egui::Frame::new().fill(PANEL).inner_margin(14.0).show(ui, |ui| {
                        ui.label(RichText::new("Seu código de login").color(MUTED));
                        ui.label(RichText::new(&code).size(30.0).strong().monospace().color(accent));
                        ui.horizontal(|ui| {
                            if ui.button("Copiar código").clicked() {
                                ctx.copy_text(code.clone());
                            }
                            if ui.button("Abrir página").clicked() {
                                let _ = open::that(&url);
                            }
                        });
                    });
                    ui.add_space(10.0);
                }

                // instância
                ui.horizontal(|ui| {
                    ui.label(RichText::new("INSTÂNCIA").size(11.0).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let txt = if self.new_open { "cancelar" } else { "+ nova" };
                        if ui.small_button(txt).clicked() {
                            self.new_open = !self.new_open;
                        }
                    });
                });

                if self.new_open {
                    egui::Frame::new().fill(PANEL).inner_margin(12.0).show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.new_name)
                                .hint_text("nome (ex: Survival)")
                                .desired_width(f32::INFINITY),
                        );
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label("Loader");
                            ui.radio_value(&mut self.new_fabric, false, "Vanilla");
                            ui.radio_value(&mut self.new_fabric, true, "Fabric");
                        });
                        ui.horizontal(|ui| {
                            ui.label("Versão");
                            egui::ComboBox::from_id_salt("new_version")
                                .width(ui.available_width() - 4.0)
                                .selected_text(self.new_version.clone())
                                .show_ui(ui, |ui| {
                                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                                        for (id, kind) in &versions {
                                            let mut label = id.clone();
                                            if *id == latest {
                                                label.push_str("  · latest");
                                            } else if kind != "release" {
                                                label.push_str(&format!("  · {kind}"));
                                            }
                                            ui.selectable_value(&mut self.new_version, id.clone(), label);
                                        }
                                    });
                                });
                        });
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.settings.snapshots, "snapshots");
                            ui.checkbox(&mut self.settings.old, "beta / alpha");
                        });
                        if ui
                            .add_enabled(!self.new_version.is_empty(), egui::Button::new("Criar instância"))
                            .clicked()
                        {
                            create_req = true;
                        }
                    });
                } else {
                    let label = inst.as_ref().map(instance_label).unwrap_or_else(|| "nenhuma: crie uma".into());
                    ui.add_enabled_ui(idle, |ui| {
                        egui::ComboBox::from_id_salt("instance")
                            .width(ui.available_width())
                            .selected_text(label)
                            .show_ui(ui, |ui| {
                                for i in &self.instances {
                                    ui.selectable_value(&mut self.current, Some(i.id.clone()), instance_label(i));
                                }
                            });
                    });
                }

                if inst.is_some() && !self.new_open {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.selectable_label(self.panel == Panel::Mods, "Mods").clicked() {
                            self.panel = if self.panel == Panel::Mods { Panel::Closed } else { Panel::Mods };
                            self.refresh_mods();
                        }
                        if ui.selectable_label(self.panel == Panel::Stats, "Estatísticas").clicked() {
                            self.panel = if self.panel == Panel::Stats { Panel::Closed } else { Panel::Stats };
                        }
                        if ui.selectable_label(self.panel == Panel::Config, "Config").clicked() {
                            self.panel = if self.panel == Panel::Config { Panel::Closed } else { Panel::Config };
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button("remover da lista")
                                .on_hover_text("os arquivos da instância ficam no disco")
                                .clicked()
                            {
                                remove_inst_req = true;
                            }
                        });
                    });
                    ui.add_space(4.0);

                    match (self.panel, &inst) {
                        (Panel::Mods, Some(i)) => {
                            if i.loader != "fabric" {
                                ui.label(RichText::new("Mods precisam de uma instância Fabric. Crie uma nova com Fabric.").color(MUTED));
                            } else {
                                ui.horizontal(|ui| {
                                    let w = ui.available_width() - 70.0;
                                    let r = ui.add(
                                        egui::TextEdit::singleline(&mut self.mod_query)
                                            .hint_text("buscar no Modrinth")
                                            .desired_width(w),
                                    );
                                    let enter = r.lost_focus() && ui.input(|inp| inp.key_pressed(egui::Key::Enter));
                                    if enter || ui.button("buscar").clicked() {
                                        search_req = true;
                                    }
                                });
                                egui::ScrollArea::vertical().id_salt("hits").max_height(180.0).show(ui, |ui| {
                                    for h in &self.mod_hits {
                                        ui.horizontal(|ui| {
                                            ui.vertical(|ui| {
                                                ui.label(RichText::new(&h.title).strong());
                                                ui.label(
                                                    RichText::new(format!(
                                                        "{} · {} downloads",
                                                        short(&h.description, 70),
                                                        h.downloads
                                                    ))
                                                    .small()
                                                    .color(MUTED),
                                                );
                                            });
                                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                if ui.small_button("instalar").clicked() {
                                                    install_req = Some(h.project_id.clone());
                                                }
                                            });
                                        });
                                    }
                                });
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(format!("INSTALADOS ({})", self.mods.len()))
                                        .size(11.0)
                                        .color(MUTED),
                                );
                                for m in &self.mods {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(m).small());
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            if ui.small_button("remover").clicked() {
                                                remove_mod_req = Some(m.clone());
                                            }
                                        });
                                    });
                                }
                            }
                        }
                        (Panel::Config, Some(_)) => {
                            if let Some(idx) = self.current.as_ref().and_then(|id| self.instances.iter().position(|i| &i.id == id)) {
                                let c = &mut self.instances[idx];
                                let mut changed = false;
                                ui.label(RichText::new("Memória (0 = usar a global)").small().color(MUTED));
                                changed |= ui
                                    .add(egui::Slider::new(&mut c.ram_mb, 0..=16384).step_by(512.0).suffix(" MB"))
                                    .changed();
                                ui.label(RichText::new("Java (vazio = automático)").small().color(MUTED));
                                changed |= ui
                                    .add(egui::TextEdit::singleline(&mut c.java_path).hint_text("caminho do java.exe").desired_width(f32::INFINITY))
                                    .changed();
                                ui.label(RichText::new("Preset de flags da JVM").small().color(MUTED));
                                let preset_now = preset_label(&c.jvm_preset);
                                egui::ComboBox::from_id_salt("preset").selected_text(preset_now).show_ui(ui, |ui| {
                                    for (key, label) in [("", "nenhum"), ("aikar", "Aikar (G1, a partir de 4 GB)"), ("zgc", "ZGC (pausas curtas)")] {
                                        if ui.selectable_value(&mut c.jvm_preset, key.to_string(), label).clicked() {
                                            changed = true;
                                        }
                                    }
                                });
                                ui.label(RichText::new("Args extras da JVM").small().color(MUTED));
                                changed |= ui
                                    .add(egui::TextEdit::singleline(&mut c.jvm_args).desired_width(f32::INFINITY))
                                    .changed();
                                ui.label(RichText::new("Entrar em servidor ao abrir (vazio = menu)").small().color(MUTED));
                                changed |= ui
                                    .add(egui::TextEdit::singleline(&mut c.join_server).hint_text("ex: mc.exemplo.com:25565").desired_width(f32::INFINITY))
                                    .changed();
                                if changed {
                                    let _ = config::save_instances(&self.instances);
                                }
                            }
                        }
                        (Panel::Stats, Some(i)) => {
                            ui.label(format!("Tempo jogado: {}", fmt_dur(i.playtime_secs)));
                            if i.last_played > 0 {
                                ui.label(format!("Última vez: {}", fmt_ago(now.saturating_sub(i.last_played))));
                            }
                            ui.add_space(4.0);
                            ui.label(RichText::new("ÚLTIMAS SESSÕES").size(11.0).color(MUTED));
                            for s in self.sessions.iter().rev().filter(|s| s.instance == i.id).take(8) {
                                ui.label(
                                    RichText::new(format!(
                                        "{} · {} · {}",
                                        fmt_ago(now.saturating_sub(s.start)),
                                        s.version,
                                        fmt_dur(s.secs)
                                    ))
                                    .small(),
                                );
                            }
                        }
                        _ => {}
                    }
                }

                ui.add_space(12.0);

                // botão principal
                let (label, enabled) = match self.busy {
                    Busy::Idle => ("JOGAR", self.manifest.is_some() && inst.is_some()),
                    Busy::LoggingIn => ("AGUARDANDO LOGIN...", false),
                    Busy::Working => ("PREPARANDO...", false),
                    Busy::Running => ("EM EXECUÇÃO", false),
                };
                let btn = egui::Button::new(RichText::new(label).size(20.0).strong().color(Color32::BLACK)).fill(accent);
                if ui.add_enabled(enabled, btn.min_size(egui::vec2(ui.available_width(), 54.0))).clicked() {
                    play_req = true;
                }

                ui.add_space(12.0);
                if self.busy == Busy::Working {
                    ui.add(egui::ProgressBar::new(self.progress).fill(accent).desired_height(6.0));
                    ui.add_space(6.0);
                }
                let color = if self.error { ERR } else { MUTED };
                ui.add(egui::Label::new(RichText::new(&self.status).color(color)).wrap());

                if let Some(h) = &self.crash_hint {
                    ui.add_space(6.0);
                    ui.label(RichText::new(h).color(ERR));
                }
                if !self.crash.is_empty() {
                    ui.add_space(6.0);
                    egui::CollapsingHeader::new("últimas linhas do log").show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                            for l in &self.crash {
                                ui.label(RichText::new(l).monospace().size(11.0).color(MUTED));
                            }
                        });
                    });
                }

                if self.show_settings {
                    ui.add_space(14.0);
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
                        ui.label("Cor de destaque");
                        ui.color_edit_button_srgb(&mut self.settings.accent);
                        ui.end_row();
                        ui.label("Discord App ID");
                        ui.add(egui::TextEdit::singleline(&mut self.settings.discord_app_id).desired_width(260.0));
                        ui.end_row();
                        ui.label("Discord Rich Presence");
                        ui.checkbox(&mut self.settings.rpc, "ativado");
                        ui.end_row();
                    });
                    ui.add_space(6.0);
                    if ui.small_button("abrir pasta de dados").clicked() {
                        let _ = open::that(data_dir());
                    }
                }
            });

        // ações da interface
        if play_req {
            self.play();
        }
        if search_req {
            self.search_mods();
        }
        if let Some(p) = install_req {
            self.install_mod(p);
        }
        if let Some(f) = remove_mod_req {
            self.remove_mod(&f);
        }
        if create_req {
            self.create_instance();
        }
        if remove_inst_req {
            self.remove_instance();
        }

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
