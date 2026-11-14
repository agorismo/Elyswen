//! Monta a linha de comando, executa o jogo, grava o tempo jogado e tenta explicar crashes.

use crate::config::{self, Account, Instance, Session, Settings};
use crate::events::{Emitter, Event};
use crate::install::*;
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

pub const LAUNCHER_NAME: &str = "kiln";
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

pub fn build_command(
    root: &Path,
    settings: &Settings,
    prep: &Prepared,
    acct: &Account,
    gdir: &Path,
) -> Result<Command> {
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
    jvm.extend(settings.jvm_args.split_whitespace().map(String::from));

    // jogo
    let game: Vec<String> = match (&vj.arguments, &vj.legacy_args) {
        (Some(a), _) if !a.game.is_empty() => expand(&a.game, &vars),
        (_, Some(s)) => s.split_whitespace().map(|p| subst(p, &vars)).collect(),
        _ => Vec::new(),
    };

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
    let mut cmd = build_command(root, settings, prep, acct, gdir)?;

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
            let hint = diagnose(&text).map(String::from);
            let lines: Vec<String> = text.lines().rev().take(25).map(String::from).collect();
            let lines = lines.into_iter().rev().collect();
            em.send(Event::CrashLog { lines, hint });
        }
    }
    em.send(Event::Exited(code));
    Ok(())
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
        let cmd = build_command(Path::new("/r"), &Settings::default(), &prep, &acct, Path::new("/g")).unwrap();
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

    #[test]
    fn diagnose_common_crashes() {
        assert!(diagnose("java.lang.OutOfMemoryError: Java heap space").unwrap().contains("RAM"));
        assert!(diagnose("tudo bem").is_none());
    }
}
