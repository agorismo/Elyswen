//! Manifest de versões, download de client/libraries/assets/natives e Java oficial da Mojang.

use crate::events::Emitter;
use anyhow::{anyhow, bail, Context, Result};
use rayon::prelude::*;
use reqwest::blocking::Client;
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const JAVA_ALL_URL: &str =
    "https://launchermeta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";

// ---------------------------------------------------------------- manifest

#[derive(Deserialize, Clone)]
pub struct Manifest {
    pub latest: Latest,
    pub versions: Vec<VersionEntry>,
}

#[derive(Deserialize, Clone)]
pub struct Latest {
    pub release: String,
    pub snapshot: String,
}

#[derive(Deserialize, Clone)]
pub struct VersionEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub sha1: String,
}

pub fn fetch_manifest(client: &Client, root: &Path) -> Result<Manifest> {
    let cache = root.join("version_manifest.json");
    match client.get(MANIFEST_URL).send().and_then(|r| r.error_for_status()) {
        Ok(r) => {
            let text = r.text()?;
            let m: Manifest = serde_json::from_str(&text)?;
            let _ = fs::create_dir_all(root);
            let _ = fs::write(&cache, text);
            Ok(m)
        }
        Err(e) => {
            // offline: usa o cache da última vez
            let text = fs::read_to_string(&cache)
                .map_err(|_| anyhow!("não consegui baixar a lista de versões: {e}"))?;
            Ok(serde_json::from_str(&text)?)
        }
    }
}

// ---------------------------------------------------------------- version json

#[derive(Deserialize, Clone, Default)]
pub struct Artifact {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub url: String,
}

#[derive(Deserialize, Clone)]
pub struct VersionJson {
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(rename = "mainClass")]
    pub main_class: String,
    #[serde(rename = "minecraftArguments", default)]
    pub legacy_args: Option<String>,
    #[serde(default)]
    pub arguments: Option<Arguments>,
    #[serde(rename = "assetIndex", default)]
    pub asset_index: Option<AssetIndexRef>,
    #[serde(default)]
    pub downloads: Downloads,
    #[serde(default)]
    pub libraries: Vec<Library>,
    #[serde(rename = "javaVersion", default)]
    pub java_version: Option<JavaVersion>,
    #[serde(default)]
    pub logging: Option<Logging>,
}

#[derive(Deserialize, Clone, Default)]
pub struct Downloads {
    #[serde(default)]
    pub client: Artifact,
}

#[derive(Deserialize, Clone)]
pub struct AssetIndexRef {
    pub id: String,
    #[serde(default)]
    pub sha1: String,
    pub url: String,
}

#[derive(Deserialize, Clone)]
pub struct JavaVersion {
    pub component: String,
}

#[derive(Deserialize, Clone)]
pub struct Logging {
    pub client: Option<LogClient>,
}

#[derive(Deserialize, Clone)]
pub struct LogClient {
    pub argument: String,
    pub file: LogFile,
}

#[derive(Deserialize, Clone)]
pub struct LogFile {
    pub id: String,
    #[serde(default)]
    pub sha1: String,
    pub url: String,
}

#[derive(Deserialize, Clone, Default)]
pub struct Arguments {
    #[serde(default)]
    pub game: Vec<Arg>,
    #[serde(default)]
    pub jvm: Vec<Arg>,
}

#[derive(Deserialize, Clone)]
#[serde(untagged)]
pub enum Arg {
    Plain(String),
    Ruled {
        #[serde(default)]
        rules: Vec<Rule>,
        value: StringOrVec,
    },
}

#[derive(Deserialize, Clone)]
#[serde(untagged)]
pub enum StringOrVec {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize, Clone)]
pub struct Rule {
    pub action: String,
    #[serde(default)]
    pub os: Option<OsRule>,
    #[serde(default)]
    pub features: Option<HashMap<String, bool>>,
}

#[derive(Deserialize, Clone)]
pub struct OsRule {
    pub name: Option<String>,
    pub arch: Option<String>,
}

#[derive(Deserialize, Clone)]
#[allow(dead_code)]
pub struct Library {
    pub name: String,
    #[serde(default)]
    pub downloads: Option<LibDownloads>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub natives: Option<HashMap<String, String>>,
    #[serde(default)]
    pub extract: Option<Extract>,
}

#[derive(Deserialize, Clone, Default)]
pub struct LibDownloads {
    #[serde(default)]
    pub artifact: Option<Artifact>,
    #[serde(default)]
    pub classifiers: Option<HashMap<String, Artifact>>,
}

#[derive(Deserialize, Clone, Default)]
pub struct Extract {
    #[serde(default)]
    pub exclude: Vec<String>,
}

pub fn os_name() -> &'static str {
    match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "osx",
        _ => "linux",
    }
}

fn arch_matches(a: &str) -> bool {
    match a {
        "x86" => cfg!(target_arch = "x86"),
        "arm64" | "aarch64" => cfg!(target_arch = "aarch64"),
        "x86_64" | "amd64" => cfg!(target_arch = "x86_64"),
        _ => true,
    }
}

impl Rule {
    fn matches(&self) -> bool {
        if let Some(os) = &self.os {
            if let Some(n) = &os.name {
                if n != os_name() {
                    return false;
                }
            }
            if let Some(a) = &os.arch {
                if !arch_matches(a) {
                    return false;
                }
            }
        }
        if let Some(f) = &self.features {
            // nenhuma feature opcional (demo, resolução custom, quick play) fica ligada
            if f.values().any(|v| *v) {
                return false;
            }
        }
        true
    }
}

pub fn rules_ok(rules: &[Rule]) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut ok = false;
    for r in rules {
        if r.matches() {
            ok = r.action == "allow";
        }
    }
    ok
}

impl Library {
    pub fn allowed(&self) -> bool {
        rules_ok(&self.rules)
    }

    fn native_classifier(&self) -> Option<String> {
        let key = self.natives.as_ref()?.get(os_name())?;
        let bits = if cfg!(target_pointer_width = "64") { "64" } else { "32" };
        Some(key.replace("${arch}", bits))
    }
}

// ---------------------------------------------------------------- paths

pub fn version_dir(root: &Path, id: &str) -> PathBuf {
    root.join("versions").join(id)
}
pub fn client_jar(root: &Path, id: &str) -> PathBuf {
    version_dir(root, id).join(format!("{id}.jar"))
}
pub fn natives_dir(root: &Path, id: &str) -> PathBuf {
    root.join("natives").join(id)
}
pub fn libraries_dir(root: &Path) -> PathBuf {
    root.join("libraries")
}
pub fn assets_dir(root: &Path) -> PathBuf {
    root.join("assets")
}

// ---------------------------------------------------------------- downloads

struct Task {
    url: String,
    dest: PathBuf,
    sha1: String,
    size: u64,
}

fn sha1_hex(data: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(data);
    hex::encode(h.finalize())
}

fn sha1_file(path: &Path) -> Option<String> {
    let mut f = fs::File::open(path).ok()?;
    let mut h = Sha1::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Some(hex::encode(h.finalize()))
}

fn already_ok(t: &Task) -> bool {
    match fs::metadata(&t.dest) {
        Ok(m) if m.is_file() => {
            if t.size > 0 {
                m.len() == t.size
            } else if !t.sha1.is_empty() {
                sha1_file(&t.dest).as_deref() == Some(t.sha1.as_str())
            } else {
                m.len() > 0
            }
        }
        _ => false,
    }
}

fn download(client: &Client, t: &Task) -> Result<()> {
    if already_ok(t) {
        return Ok(());
    }
    if let Some(p) = t.dest.parent() {
        fs::create_dir_all(p)?;
    }
    let mut last: Option<anyhow::Error> = None;
    for _ in 0..3 {
        match fetch_bytes(client, &t.url) {
            Ok(bytes) => {
                if !t.sha1.is_empty() && !sha1_hex(&bytes).eq_ignore_ascii_case(&t.sha1) {
                    last = Some(anyhow!("hash inválido em {}", t.url));
                    continue;
                }
                let tmp = t.dest.with_extension("part");
                let mut f = fs::File::create(&tmp)?;
                f.write_all(&bytes)?;
                drop(f);
                fs::rename(&tmp, &t.dest)?;
                return Ok(());
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("falha desconhecida"))).with_context(|| format!("baixando {}", t.url))
}

fn fetch_bytes(client: &Client, url: &str) -> Result<Vec<u8>> {
    let r = client.get(url).send()?.error_for_status()?;
    Ok(r.bytes()?.to_vec())
}

fn run_tasks(client: &Client, tasks: &[Task], em: &Emitter, label: &str) -> Result<()> {
    let total = tasks.len().max(1);
    let done = AtomicUsize::new(0);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(16).build()?;
    pool.install(|| {
        tasks.par_iter().try_for_each(|t| -> Result<()> {
            download(client, t)?;
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 8 == 0 || n == total {
                em.progress(n as f32 / total as f32);
                em.status(format!("{label} ({n}/{total})"));
            }
            Ok(())
        })
    })
}

// ---------------------------------------------------------------- install

pub struct Prepared {
    pub vj: VersionJson,
    pub java: PathBuf,
    pub assets_virtual: Option<PathBuf>,
}

#[derive(Deserialize)]
struct AssetIndex {
    objects: HashMap<String, AssetObj>,
    #[serde(rename = "virtual", default)]
    is_virtual: bool,
    #[serde(default)]
    map_to_resources: bool,
}

#[derive(Deserialize)]
struct AssetObj {
    hash: String,
    size: u64,
}

pub fn install(
    client: &Client,
    root: &Path,
    entry: &VersionEntry,
    java_override: &str,
    game_dir: &Path,
    em: &Emitter,
) -> Result<Prepared> {
    // 1) json da versão
    em.status(format!("Baixando metadados de {}", entry.id));
    let json_path = version_dir(root, &entry.id).join(format!("{}.json", entry.id));
    download(
        client,
        &Task { url: entry.url.clone(), dest: json_path.clone(), sha1: entry.sha1.clone(), size: 0 },
    )?;
    let vj: VersionJson = serde_json::from_str(&fs::read_to_string(&json_path)?)
        .with_context(|| format!("json da versão {} inválido", entry.id))?;

    let mut tasks: Vec<Task> = Vec::new();

    // 2) client.jar
    if vj.downloads.client.url.is_empty() {
        bail!("a versão {} não tem client.jar", entry.id);
    }
    tasks.push(Task {
        url: vj.downloads.client.url.clone(),
        dest: client_jar(root, &entry.id),
        sha1: vj.downloads.client.sha1.clone(),
        size: vj.downloads.client.size,
    });

    // 3) libraries + natives
    let libs = libraries_dir(root);
    let mut native_jars: Vec<(PathBuf, Vec<String>)> = Vec::new();
    for lib in vj.libraries.iter().filter(|l| l.allowed()) {
        let Some(d) = &lib.downloads else { continue };
        if let Some(a) = &d.artifact {
            if !a.url.is_empty() {
                tasks.push(Task {
                    url: a.url.clone(),
                    dest: libs.join(&a.path),
                    sha1: a.sha1.clone(),
                    size: a.size,
                });
            }
        }
        if let (Some(cls), Some(map)) = (lib.native_classifier(), &d.classifiers) {
            if let Some(a) = map.get(&cls) {
                let dest = libs.join(&a.path);
                tasks.push(Task { url: a.url.clone(), dest: dest.clone(), sha1: a.sha1.clone(), size: a.size });
                let excl = lib.extract.clone().unwrap_or_default().exclude;
                native_jars.push((dest, excl));
            }
        }
    }

    // 4) log4j config
    let mut log_cfg_dest = None;
    if let Some(Logging { client: Some(lc) }) = &vj.logging {
        let dest = assets_dir(root).join("log_configs").join(&lc.file.id);
        tasks.push(Task { url: lc.file.url.clone(), dest: dest.clone(), sha1: lc.file.sha1.clone(), size: 0 });
        log_cfg_dest = Some(dest);
    }
    let _ = log_cfg_dest;

    // 5) assets
    let mut assets_virtual = None;
    let mut index: Option<(AssetIndex, String)> = None;
    if let Some(ai) = &vj.asset_index {
        em.status("Baixando índice de assets");
        let idx_path = assets_dir(root).join("indexes").join(format!("{}.json", ai.id));
        download(
            client,
            &Task { url: ai.url.clone(), dest: idx_path.clone(), sha1: ai.sha1.clone(), size: 0 },
        )?;
        let idx: AssetIndex = serde_json::from_str(&fs::read_to_string(&idx_path)?)?;
        for o in idx.objects.values() {
            let pre = &o.hash[..2];
            tasks.push(Task {
                url: format!("https://resources.download.minecraft.net/{pre}/{}", o.hash),
                dest: assets_dir(root).join("objects").join(pre).join(&o.hash),
                sha1: o.hash.clone(),
                size: o.size.max(1),
            });
        }
        index = Some((idx, ai.id.clone()));
    }

    run_tasks(client, &tasks, em, "Baixando arquivos")?;

    // 6) extrai natives (versões antigas, LWJGL 2 / 3.x < 3.3)
    em.status("Extraindo bibliotecas nativas");
    let ndir = natives_dir(root, &entry.id);
    let _ = fs::remove_dir_all(&ndir);
    fs::create_dir_all(&ndir)?;
    for (jar, exclude) in &native_jars {
        extract_natives(jar, &ndir, exclude)?;
    }

    // 7) assets "virtual" / "map_to_resources" (versões antigas)
    if let Some((idx, id)) = &index {
        if idx.is_virtual || idx.map_to_resources {
            em.status("Preparando assets legados");
            let base = if idx.map_to_resources {
                game_dir.join("resources")
            } else {
                assets_dir(root).join("virtual").join(id)
            };
            for (name, o) in &idx.objects {
                let src = assets_dir(root).join("objects").join(&o.hash[..2]).join(&o.hash);
                let dst = base.join(name);
                if dst.exists() {
                    continue;
                }
                if let Some(p) = dst.parent() {
                    fs::create_dir_all(p)?;
                }
                fs::copy(&src, &dst)?;
            }
            if idx.is_virtual {
                assets_virtual = Some(base);
            }
        }
    }

    // 8) Java
    let java = resolve_java(client, root, &vj, java_override, em)?;

    em.progress(1.0);
    Ok(Prepared { vj, java, assets_virtual })
}

fn extract_natives(jar: &Path, dest: &Path, exclude: &[String]) -> Result<()> {
    let f = fs::File::open(jar)?;
    let mut zip = zip::ZipArchive::new(f)?;
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        if e.is_dir() {
            continue;
        }
        let name = e.name().map(|n| n.to_string()).unwrap_or_default();
        if exclude.iter().any(|x| name.starts_with(x.as_str())) {
            continue;
        }
        // evita path traversal
        let Some(rel) = e.enclosed_name() else { continue };
        let out = dest.join(rel);
        if let Some(p) = out.parent() {
            fs::create_dir_all(p)?;
        }
        let mut o = fs::File::create(&out)?;
        std::io::copy(&mut e, &mut o)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- java

fn platform_key() -> Option<&'static str> {
    use std::env::consts::{ARCH, OS};
    match (OS, ARCH) {
        ("linux", "x86_64") => Some("linux"),
        ("linux", "x86") => Some("linux-i386"),
        ("windows", "x86_64") => Some("windows-x64"),
        ("windows", "aarch64") => Some("windows-arm64"),
        ("windows", "x86") => Some("windows-x86"),
        ("macos", "x86_64") => Some("mac-os"),
        ("macos", "aarch64") => Some("mac-os-arm64"),
        _ => None,
    }
}

fn java_rel() -> &'static str {
    match std::env::consts::OS {
        "windows" => "bin/java.exe",
        "macos" => "jre.bundle/Contents/Home/bin/java",
        _ => "bin/java",
    }
}

fn resolve_java(
    client: &Client,
    root: &Path,
    vj: &VersionJson,
    java_override: &str,
    em: &Emitter,
) -> Result<PathBuf> {
    if !java_override.trim().is_empty() {
        return Ok(PathBuf::from(java_override.trim()));
    }
    let component = vj
        .java_version
        .as_ref()
        .map(|j| j.component.clone())
        .unwrap_or_else(|| "jre-legacy".to_string());
    match ensure_runtime(client, root, &component, em) {
        Ok(Some(p)) => Ok(p),
        Ok(None) => {
            em.status("Java oficial indisponível nesta plataforma; usando o java do sistema");
            Ok(PathBuf::from("java"))
        }
        Err(e) => Err(e.context("falha ao preparar o Java")),
    }
}

#[derive(Deserialize)]
struct RtFiles {
    files: HashMap<String, RtFile>,
}

#[derive(Deserialize)]
struct RtFile {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    executable: bool,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    downloads: Option<RtDownloads>,
}

#[derive(Deserialize)]
struct RtDownloads {
    raw: Artifact,
}

fn ensure_runtime(client: &Client, root: &Path, component: &str, em: &Emitter) -> Result<Option<PathBuf>> {
    let Some(plat) = platform_key() else { return Ok(None) };
    let dir = root.join("runtime").join(component).join(plat);
    let java = dir.join(java_rel());

    em.status(format!("Verificando Java ({component})"));
    let all: serde_json::Value = client.get(JAVA_ALL_URL).send()?.error_for_status()?.json()?;
    let Some(m) = all[plat][component].get(0).and_then(|v| v.get("manifest")) else {
        return Ok(None);
    };
    let m_url = m["url"].as_str().ok_or_else(|| anyhow!("manifesto de Java inválido"))?;
    let m_sha = m["sha1"].as_str().unwrap_or_default().to_string();

    let marker = dir.join(".kiln-ok");
    if java.exists() && fs::read_to_string(&marker).map(|s| s.trim() == m_sha).unwrap_or(false) {
        return Ok(Some(java));
    }

    let files: RtFiles = client.get(m_url).send()?.error_for_status()?.json()?;
    fs::create_dir_all(&dir)?;

    let mut tasks = Vec::new();
    let mut exec = Vec::new();
    let mut links = Vec::new();
    for (rel, f) in &files.files {
        let dest = dir.join(rel);
        match f.kind.as_str() {
            "directory" => fs::create_dir_all(&dest)?,
            "file" => {
                if let Some(d) = &f.downloads {
                    tasks.push(Task {
                        url: d.raw.url.clone(),
                        dest: dest.clone(),
                        sha1: d.raw.sha1.clone(),
                        size: d.raw.size,
                    });
                    if f.executable {
                        exec.push(dest);
                    }
                }
            }
            "link" => {
                if let Some(t) = &f.target {
                    links.push((dest, t.clone()));
                }
            }
            _ => {}
        }
    }
    run_tasks(client, &tasks, em, "Baixando Java")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in &exec {
            fs::set_permissions(p, fs::Permissions::from_mode(0o755))?;
        }
        for (dest, target) in &links {
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p)?;
            }
            let _ = fs::remove_file(dest);
            std::os::unix::fs::symlink(target, dest)?;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (&exec, &links);
    }

    if !java.exists() {
        bail!("Java baixado, mas {} não foi encontrado", java.display());
    }
    fs::write(&marker, m_sha)?;
    Ok(Some(java))
}
