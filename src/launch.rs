//! Monta a linha de comando, executa o jogo, grava o tempo jogado e tenta explicar crashes.

use crate::config::{self, Account, Instance, Session, Settings};
use crate::events::{Emitter, Event};
use crate::install::*;
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

pub const LAUNCHER_NAME: &str = "elyswen";
pub const LAUNCHER_VERSION: &str = env!("CARGO_PKG_VERSION");

fn subst(s: &str, vars: &HashMap<&str, String>) -> String {
    let mut out = s.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("${{{k}}}"), v);
    }
    out
}

fn expand(args: &[Arg], vars: &HashMap<&str, String>) -> Vec<String> {
    let mut out = Vec::new();
    for a in args {
        match a {
            Arg::Plain(s) => out.push(subst(s, vars)),
            Arg::Ruled { rules, value } => {
                if !rules_ok(rules) {
                    continue;
                }
                match value {
                    StringOrVec::One(s) => out.push(subst(s, vars)),
                    StringOrVec::Many(v) => out.extend(v.iter().map(|s| subst(s, vars))),
                }
            }
        }
    }
    out
}

/// Opções de execução já resolvidas (instância sobrepõe as globais).
#[derive(Default)]
pub struct Opts {
    pub ram_mb: u32,
    pub preset: String,
    pub extra_jvm: String,
    pub server: Option<String>,
    pub game_version: String,
}

impl Opts {
    pub fn new(settings: &Settings, inst: &Instance) -> Self {
        Self {
            ram_mb: if inst.ram_mb > 0 { inst.ram_mb } else { settings.ram_mb },
            preset: inst.jvm_preset.clone(),
            extra_jvm: format!("{} {}", inst.jvm_args, settings.jvm_args),
            server: Some(inst.join_server.trim().to_string()).filter(|s| !s.is_empty()),
            game_version: inst.version.clone(),
        }
    }
}

/// Flags de GC prontas. Aikar é pensado para heaps de 4 GB ou mais.
pub fn preset_flags(name: &str) -> Vec<&'static str> {
    match name {
        "aikar" => vec![
            "-XX:+UseG1GC",
            "-XX:+ParallelRefProcEnabled",
            "-XX:MaxGCPauseMillis=200",
            "-XX:+UnlockExperimentalVMOptions",
            "-XX:+DisableExplicitGC",
            "-XX:+AlwaysPreTouch",
            "-XX:G1NewSizePercent=30",
            "-XX:G1MaxNewSizePercent=40",
            "-XX:G1HeapRegionSize=8M",
            "-XX:G1ReservePercent=20",
            "-XX:G1HeapWastePercent=5",
            "-XX:G1MixedGCCountTarget=4",
            "-XX:InitiatingHeapOccupancyPercent=15",
            "-XX:G1MixedGCLiveThresholdPercent=90",
            "-XX:G1RSetUpdatingPauseTimePercent=5",
            "-XX:SurvivorRatio=32",
            "-XX:+PerfDisableSharedMem",
            "-XX:MaxTenuringThreshold=1",
        ],
        "zgc" => vec!["-XX:+UseZGC"],
        _ => vec![],
    }
}

/// Desde 1.20 o Minecraft usa --quickPlayMultiplayer; antes, --server/--port.
pub fn uses_quickplay(v: &str) -> bool {
    let mut p = v.split('.');
    if let (Some("1"), Some(minor)) = (p.next(), p.next()) {
        return minor.parse::<u32>().map_or(false, |m| m >= 20);
    }
    // snapshots no formato 24w14a
    v.get(0..2).and_then(|y| y.parse::<u32>().ok()).map_or(false, |y| y >= 23)
}

pub fn split_addr(addr: &str) -> (&str, &str) {
    match addr.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => (h, p),
        _ => (addr, "25565"),
    }
}

pub fn build_command(
    root: &Path,
    opts: &Opts,
    prep: &Prepared,
    acct: &Account,
    gdir: &Path,
) -> Result<Command> {
    let settings = opts;
    let vj = &prep.vj;
    let sep = if cfg!(windows) { ";" } else { ":" };

    // classpath: se uma biblioteca aparece duas vezes (vanilla + perfil), a primeira vence
    let libs = libraries_dir(root);
    let mut seen = HashSet::new();
    let mut cp: Vec<String> = Vec::new();
    for lib in vj.libraries.iter().filter(|l| l.allowed()) {
        if let Some(rel) = lib.rel_path() {
            if seen.insert(lib.key()) {
                cp.push(libs.join(rel).to_string_lossy().to_string());
            }
        }
    }
    cp.push(client_jar(root, &prep.jar_id).to_string_lossy().to_string());

    let assets_root = assets_dir(root);
    let index_id = vj.asset_index.as_ref().map(|a| a.id.clone()).unwrap_or_else(|| "legacy".into());
    let game_assets = prep.assets_virtual.clone().unwrap_or_else(|| assets_root.clone());

    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("auth_player_name", acct.name.clone());
    vars.insert("version_name", vj.id.clone());
    vars.insert("game_directory", gdir.to_string_lossy().to_string());
    vars.insert("assets_root", assets_root.to_string_lossy().to_string());
    vars.insert("assets_index_name", index_id);
    vars.insert("game_assets", game_assets.to_string_lossy().to_string());
    vars.insert("auth_uuid", acct.uuid.clone());
    vars.insert("auth_access_token", acct.mc_token.clone());
    vars.insert("auth_session", format!("token:{}:{}", acct.mc_token, acct.uuid));
    vars.insert("clientid", String::new());
    vars.insert("auth_xuid", String::new());
    vars.insert("user_type", "msa".into());
    vars.insert("user_properties", "{}".into());
    vars.insert("version_type", vj.kind.clone());
    vars.insert("launcher_name", LAUNCHER_NAME.into());
    vars.insert("launcher_version", LAUNCHER_VERSION.into());
    vars.insert("natives_directory", natives_dir(root, &prep.jar_id).to_string_lossy().to_string());
    vars.insert("classpath", cp.join(sep));
    vars.insert("classpath_separator", sep.into());
    vars.insert("library_directory", libs.to_string_lossy().to_string());

    // JVM
    let mut jvm: Vec<String> = vec![format!("-Xmx{}M", settings.ram_mb), "-Xms512M".into()];
    jvm.extend(preset_flags(&settings.preset).into_iter().map(String::from));
    match vj.arguments.as_ref().map(|a| &a.jvm).filter(|v| !v.is_empty()) {
        Some(j) => jvm.extend(expand(j, &vars)),
        None => {
            jvm.push(format!("-Djava.library.path={}", vars["natives_directory"]));
            jvm.push(format!("-Dminecraft.launcher.brand={LAUNCHER_NAME}"));
            jvm.push(format!("-Dminecraft.launcher.version={LAUNCHER_VERSION}"));
            jvm.push("-cp".into());
            jvm.push(vars["classpath"].clone());
        }
    }
    if let Some(lc) = vj.logging.as_ref().and_then(|l| l.client.as_ref()) {
        let f = assets_root.join("log_configs").join(&lc.file.id);
        jvm.push(lc.argument.replace("${path}", &f.to_string_lossy()));
    }
    jvm.extend(settings.extra_jvm.split_whitespace().map(String::from));

    // jogo
    let mut game: Vec<String> = match (&vj.arguments, &vj.legacy_args) {
        (Some(a), _) if !a.game.is_empty() => expand(&a.game, &vars),
        (_, Some(s)) => s.split_whitespace().map(|p| subst(p, &vars)).collect(),
        _ => Vec::new(),
    };
    if let Some(addr) = &settings.server {
        if uses_quickplay(&settings.game_version) {
            game.push("--quickPlayMultiplayer".into());
            game.push(addr.clone());
        } else {
            let (host, port) = split_addr(addr);
            game.extend(["--server", host, "--port", port].map(String::from));
        }
    }

    let mut cmd = Command::new(&prep.java);
    cmd.args(&jvm).arg(&vj.main_class).args(&game).current_dir(gdir);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    Ok(cmd)
}

/// Executa o jogo e bloqueia até ele fechar. A saída vai para um arquivo de log.
pub fn run(
    root: &Path,
    settings: &Settings,
    prep: &Prepared,
    acct: &Account,
    gdir: &Path,
    inst: &Instance,
    em: &Emitter,
) -> Result<()> {
    fs::create_dir_all(gdir)?;
    let opts = Opts::new(settings, inst);
    let mut cmd = build_command(root, &opts, prep, acct, gdir)?;

    let logs = root.join("logs");
    fs::create_dir_all(&logs)?;
    let log_path = logs.join("latest-launch.log");
    let out = fs::File::create(&log_path)?;
    let err = out.try_clone()?;
    cmd.stdin(Stdio::null()).stdout(out).stderr(err);

    em.status(format!("Iniciando {}", inst.name));
    let mut child = cmd
        .spawn()
        .with_context(|| format!("não consegui executar o Java ({})", prep.java.display()))?;
    let start = config::now();
    em.send(Event::Started(format!("{} · {}", inst.version, inst.name)));

    let code = child.wait()?.code();
    let secs = config::now().saturating_sub(start);
    let _ = config::record_session(Session {
        instance: inst.id.clone(),
        version: inst.version.clone(),
        start,
        secs,
    });
    let _ = config::add_playtime(&inst.id, secs);

    if code != Some(0) {
        if let Ok(text) = fs::read_to_string(&log_path) {
            let hint = match (diagnose(&text), key_line(&text)) {
                (Some(h), Some(k)) => Some(format!("{h}\n{k}")),
                (None, Some(k)) => Some(k),
                (h, None) => h.map(String::from),
            };
            let lines: Vec<String> = text.lines().rev().take(25).map(String::from).collect();
            let lines = lines.into_iter().rev().collect();
            em.send(Event::CrashLog { lines, hint });
        }
    }
    em.send(Event::Exited(code));
    Ok(())
}

/// Última linha de erro do log (ignora linhas de stack trace).
pub fn key_line(log: &str) -> Option<String> {
    log.lines()
        .filter(|l| !l.starts_with(char::is_whitespace) && (l.contains("Exception") || l.contains("Error")))
        .last()
        .map(|l| l.chars().take(220).collect())
}

/// Procura causas conhecidas no log do jogo e devolve uma dica em português.
pub fn diagnose(log: &str) -> Option<&'static str> {
    let rules: &[(&str, &str)] = &[
        ("OutOfMemoryError", "Faltou memória. Aumente a RAM em ⚙ → Memória."),
        (
            "UnsupportedClassVersionError",
            "Java incompatível com esta versão. Deixe o Java em automático.",
        ),
        (
            "Could not find or load main class",
            "Instalação incompleta. Jogue de novo para baixar tudo.",
        ),
        (
            "ModResolutionException",
            "Mods incompatíveis entre si. Remova o mod mais recente.",
        ),
        (
            "Incompatible mods found",
            "Mods incompatíveis. Remova o que não é da sua versão.",
        ),
        (
            "NoClassDefFoundError",
            "Falta uma dependência de algum mod. Confira os requisitos dele.",
        ),
        ("Mixin apply failed", "Um mod falhou ao aplicar mixin: geralmente é versão errada dele."),
        ("Session check failed", "Login expirado. Saia da conta e entre de novo."),
        (
            "Could not reserve enough space",
            "A RAM configurada é grande demais para este PC. Reduza a memória.",
        ),
        ("Invalid maximum heap size", "Valor de RAM inválido. Ajuste a memória da instância."),
        (
            "Error occurred during initialization of VM",
            "O Java não iniciou com essas flags. Tire os args extras ou o preset.",
        ),
        (
            "NoSuchMethodError",
            "Um mod foi feito para outra versão do jogo ou do loader. Atualize ou remova-o.",
        ),
        (
            "ClassNotFoundException",
            "Falta uma classe de um mod, geralmente uma dependência. Confira os requisitos.",
        ),
        (
            "ZipException",
            "Um .jar está corrompido. Remova-o da pasta mods e baixe de novo.",
        ),
    ];
    rules.iter().find(|(needle, _)| log.contains(needle)).map(|(_, hint)| *hint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const MODERN: &str = r#"{"id":"1.21.4","type":"release","mainClass":"net.minecraft.client.main.Main",
    "arguments":{"game":["--username","${auth_player_name}","--version","${version_name}","--gameDir","${game_directory}","--assetsDir","${assets_root}","--assetIndex","${assets_index_name}","--uuid","${auth_uuid}","--accessToken","${auth_access_token}","--userType","${user_type}",
      {"rules":[{"action":"allow","features":{"is_demo_user":true}}],"value":"--demo"},
      {"rules":[{"action":"allow","features":{"has_custom_resolution":true}}],"value":["--width","${resolution_width}"]}],
     "jvm":[{"rules":[{"action":"allow","os":{"name":"osx"}}],"value":["-XstartOnFirstThread"]},
      {"rules":[{"action":"allow","os":{"name":"windows"}}],"value":"-XX:HeapDumpPath=x.bin"},
      "-Djava.library.path=${natives_directory}","-Dminecraft.launcher.brand=${launcher_name}","-cp","${classpath}"]},
    "assetIndex":{"id":"19","sha1":"x","url":"http://x"},
    "downloads":{"client":{"sha1":"a","size":1,"url":"http://c"}},
    "libraries":[{"name":"a:b:1","downloads":{"artifact":{"path":"a/b/1/b-1.jar","sha1":"s","size":1,"url":"http://u"}}},
     {"name":"x:y:1:natives-windows","rules":[{"action":"allow","os":{"name":"windows"}}],"downloads":{"artifact":{"path":"win.jar","sha1":"s","size":1,"url":"http://u"}}}],
    "javaVersion":{"component":"java-runtime-delta","majorVersion":21},
    "logging":{"client":{"argument":"-Dlog4j.configurationFile=${path}","file":{"id":"client-1.12.xml","sha1":"s","size":1,"url":"http://l"},"type":"log4j2-xml"}}}"#;

    const LEGACY: &str = r#"{"id":"1.8.9","type":"release","mainClass":"net.minecraft.client.main.Main",
    "minecraftArguments":"--username ${auth_player_name} --version ${version_name} --gameDir ${game_directory} --assetsDir ${assets_root} --assetIndex ${assets_index_name} --uuid ${auth_uuid} --accessToken ${auth_access_token} --userProperties ${user_properties} --userType ${user_type}",
    "assetIndex":{"id":"1.8","sha1":"x","url":"http://x"},
    "downloads":{"client":{"sha1":"a","size":1,"url":"http://c"}},
    "libraries":[{"name":"org.lwjgl.lwjgl:lwjgl-platform:2.9.4","natives":{"linux":"natives-linux","windows":"natives-windows","osx":"natives-osx"},"extract":{"exclude":["META-INF/"]},
      "downloads":{"classifiers":{"natives-linux":{"path":"l.jar","sha1":"s","size":1,"url":"u"},"natives-windows":{"path":"w.jar","sha1":"s","size":1,"url":"u"},"natives-osx":{"path":"o.jar","sha1":"s","size":1,"url":"u"}}}}]}"#;

    fn parse(json: &str) -> VersionJson {
        serde_json::from_str(json).expect("parse")
    }

    /// Monta o comando e devolve os argumentos (barras normalizadas para rodar em qualquer SO).
    fn args_for_vj(vj: VersionJson, jar_id: &str) -> Vec<String> {
        let prep = Prepared { vj, jar_id: jar_id.into(), java: PathBuf::from("java"), assets_virtual: None };
        let acct = Account {
            name: "Steve".into(),
            uuid: "uuid123".into(),
            mc_token: "tok".into(),
            mc_expires: 0,
            refresh: String::new(),
        };
        let opts = Opts { ram_mb: 4096, game_version: jar_id.into(), ..Default::default() };
        let cmd = build_command(Path::new("/r"), &opts, &prep, &acct, Path::new("/g")).unwrap();
        cmd.get_args().map(|a| a.to_string_lossy().replace('\\', "/")).collect()
    }

    #[test]
    fn modern_args() {
        let a = args_for_vj(parse(MODERN), "1.21.4");
        let has = |s: &str| a.iter().any(|x| x == s);
        assert!(has("--username") && has("Steve") && has("uuid123") && has("msa"));
        assert!(!has("--demo") && !has("--width"), "features opcionais devem ficar desligadas");
        assert!(has("net.minecraft.client.main.Main"));
        assert!(a.iter().any(|x| x == "-Djava.library.path=/r/natives/1.21.4"));
        assert!(a.iter().any(|x| x == "-Dlog4j.configurationFile=/r/assets/log_configs/client-1.12.xml"));
        let cp = a.iter().position(|x| x == "-cp").map(|i| a[i + 1].clone()).unwrap();
        assert!(cp.contains("b-1.jar") && cp.ends_with("1.21.4.jar"));
        if cfg!(target_os = "linux") {
            assert!(!cp.contains("win.jar") && !has("-XstartOnFirstThread"));
        }
        assert!(!a.iter().any(|x| x.contains("${")), "variável sem substituir: {a:?}");
    }

    #[test]
    fn legacy_args() {
        let a = args_for_vj(parse(LEGACY), "1.8.9");
        assert!(a.iter().any(|x| x == "{}"), "userProperties");
        assert!(a.iter().any(|x| x == "-cp"));
        assert!(!a.iter().any(|x| x.contains("${")), "variável sem substituir: {a:?}");
    }

    #[test]
    fn fabric_profile_merges_over_vanilla() {
        let child = parse(
            r#"{"id":"fabric-loader-0.16.9-1.21.4","type":"release","mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
            "libraries":[{"name":"net.fabricmc:fabric-loader:0.16.9","url":"https://maven.fabricmc.net/"}]}"#,
        );
        let m = merge_profile(child, parse(MODERN));
        assert_eq!(m.id, "fabric-loader-0.16.9-1.21.4");
        assert!(m.asset_index.is_some());
        let a = args_for_vj(m, "1.21.4");
        assert!(a.iter().any(|x| x == "net.fabricmc.loader.impl.launch.knot.KnotClient"));
        let cp = a.iter().position(|x| x == "-cp").map(|i| a[i + 1].clone()).unwrap();
        assert!(cp.contains("/libraries/net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar"), "{cp}");
        assert!(cp.ends_with("1.21.4.jar"), "{cp}");
    }

    #[test]
    fn maven_paths() {
        assert_eq!(
            maven_path("net.fabricmc:fabric-loader:0.16.9").unwrap(),
            "net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar"
        );
        assert_eq!(
            maven_path("org.lwjgl:lwjgl:3.3.3:natives-linux").unwrap(),
            "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-linux.jar"
        );
    }

    fn cmd_args(vj: VersionJson, jar: &str, opts: &Opts) -> Vec<String> {
        let prep = Prepared { vj, jar_id: jar.into(), java: PathBuf::from("java"), assets_virtual: None };
        let acct = Account {
            name: "Steve".into(),
            uuid: "u".into(),
            mc_token: "t".into(),
            mc_expires: 0,
            refresh: String::new(),
        };
        let cmd = build_command(Path::new("/r"), opts, &prep, &acct, Path::new("/g")).unwrap();
        cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect()
    }

    #[test]
    fn jvm_preset_ram_and_quickplay() {
        let opts = Opts {
            ram_mb: 2048,
            preset: "aikar".into(),
            extra_jvm: "-Dfoo=1".into(),
            server: Some("mc.x.com:25570".into()),
            game_version: "1.21.4".into(),
        };
        let a = cmd_args(parse(MODERN), "1.21.4", &opts);
        assert!(a.iter().any(|x| x == "-Xmx2048M"));
        assert!(a.iter().any(|x| x == "-XX:+UseG1GC"));
        assert!(a.iter().any(|x| x == "-Dfoo=1"));
        let q = a.iter().position(|x| x == "--quickPlayMultiplayer").unwrap();
        assert_eq!(a[q + 1], "mc.x.com:25570");
    }

    #[test]
    fn legacy_server_args_use_server_and_port() {
        let opts = Opts { ram_mb: 1024, server: Some("mc.x.com".into()), game_version: "1.8.9".into(), ..Default::default() };
        let a = cmd_args(parse(LEGACY), "1.8.9", &opts);
        let s = a.iter().position(|x| x == "--server").unwrap();
        assert_eq!(a[s + 1], "mc.x.com");
        assert_eq!(a[s + 2], "--port");
        assert_eq!(a[s + 3], "25565");
    }

    #[test]
    fn quickplay_by_version_and_addr_split() {
        assert!(uses_quickplay("1.21.4") && uses_quickplay("1.20") && uses_quickplay("24w14a"));
        assert!(!uses_quickplay("1.19.4") && !uses_quickplay("22w01a"));
        assert_eq!(split_addr("a.b:1234"), ("a.b", "1234"));
        assert_eq!(split_addr("a.b"), ("a.b", "25565"));
    }

    #[test]
    fn key_line_skips_stack_frames() {
        let log = "[main/INFO] iniciando\njava.lang.NoSuchMethodError: x\n\tat a.b(c.java:1)\n";
        assert_eq!(key_line(log).unwrap(), "java.lang.NoSuchMethodError: x");
        assert!(diagnose("java.util.zip.ZipException: zip END header").unwrap().contains("corrompido"));
    }

    #[test]
    fn diagnose_common_crashes() {
        assert!(diagnose("java.lang.OutOfMemoryError: Java heap space").unwrap().contains("RAM"));
        assert!(diagnose("tudo bem").is_none());
    }
}
