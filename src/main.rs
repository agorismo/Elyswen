mod app;
mod auth;
mod config;
mod events;
mod install;
mod launch;
mod rpc;

use events::{Emitter, Event};
use std::sync::mpsc::channel;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("list") => {
            cli_list();
            Ok(())
        }
        Some("install") => {
            cli_install(args.get(1).cloned());
            Ok(())
        }
        _ => {
            let options = eframe::NativeOptions {
                viewport: eframe::egui::ViewportBuilder::default()
                    .with_inner_size([440.0, 460.0])
                    .with_min_inner_size([440.0, 380.0])
                    .with_title("kiln"),
                ..Default::default()
            };
            eframe::run_native(
                "kiln",
                options,
                Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
            )
        }
    }
}

/// `kiln list` — imprime todas as versões disponíveis.
fn cli_list() {
    let client = app::http_client();
    match install::fetch_manifest(&client, &config::data_dir()) {
        Ok(m) => {
            println!("latest release: {}  |  latest snapshot: {}", m.latest.release, m.latest.snapshot);
            println!("{} versões no total", m.versions.len());
            for v in &m.versions {
                println!("{:<14} {}", v.id, v.kind);
            }
        }
        Err(e) => eprintln!("erro: {e:#}"),
    }
}

/// `kiln install <versão>` — baixa tudo (jar, libs, assets, natives, Java) sem abrir o jogo.
fn cli_install(id: Option<String>) {
    let Some(id) = id else {
        eprintln!("uso: kiln install <versão>");
        return;
    };
    let client = app::http_client();
    let root = config::data_dir();
    let manifest = match install::fetch_manifest(&client, &root) {
        Ok(m) => m,
        Err(e) => return eprintln!("erro: {e:#}"),
    };
    let Some(entry) = manifest.versions.iter().find(|v| v.id == id).cloned() else {
        return eprintln!("versão '{id}' não existe (use `kiln list`)");
    };
    let settings = config::Settings::load();
    let gdir = launch::game_dir(&root, &settings, &id);
    let (tx, rx) = channel();
    let em = Emitter::new(tx, None);
    let worker = std::thread::spawn(move || {
        let r = install::install(&client, &root, &entry, &settings.java_path, &gdir, &em);
        match &r {
            Ok(p) => em.status(format!("OK — java: {}", p.java.display())),
            Err(e) => em.send(Event::Error(format!("{e:#}"))),
        }
        em.send(Event::Idle);
    });
    let mut last = String::new();
    while let Ok(ev) = rx.recv() {
        match ev {
            Event::Status(s) if s != last => {
                println!("{s}");
                last = s;
            }
            Event::Error(e) => eprintln!("erro: {e}"),
            Event::Idle => break,
            _ => {}
        }
    }
    let _ = worker.join();
}
