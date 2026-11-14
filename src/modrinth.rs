//! Busca e instalação de mods pela API pública do Modrinth (sem chave).

use crate::events::Emitter;
use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

const API: &str = "https://api.modrinth.com/v2";

#[derive(Deserialize, Clone)]
pub struct Hit {
    pub project_id: String,
    pub title: String,
    pub description: String,
    pub downloads: u64,
}

#[derive(Deserialize)]
struct SearchResp {
    hits: Vec<Hit>,
}

#[derive(Deserialize)]
struct Version {
    dependencies: Vec<Dep>,
    files: Vec<VFile>,
}

#[derive(Deserialize)]
struct Dep {
    project_id: Option<String>,
    dependency_type: String,
}

#[derive(Deserialize)]
struct VFile {
    url: String,
    filename: String,
    primary: bool,
    hashes: Hashes,
}

#[derive(Deserialize)]
struct Hashes {
    sha1: String,
}

/// Mods Fabric para a versão `mc`, ordenados pela relevância do Modrinth.
pub fn search(client: &Client, query: &str, mc: &str) -> Result<Vec<Hit>> {
    let facets = format!("[[\"project_type:mod\"],[\"versions:{mc}\"],[\"categories:fabric\"]]");
    let r: SearchResp = client
        .get(format!("{API}/search"))
        .query(&[("query", query), ("facets", facets.as_str()), ("limit", "20")])
        .send()?
        .error_for_status()
        .context("a busca do Modrinth falhou")?
        .json()?;
    Ok(r.hits)
}

/// Instala o mod e as dependências obrigatórias. Devolve os nomes dos arquivos.
pub fn install(client: &Client, mods_dir: &Path, project: &str, mc: &str, em: &Emitter) -> Result<Vec<String>> {
    let mut seen = HashSet::new();
    let mut names = Vec::new();
    install_rec(client, mods_dir, project, mc, em, &mut seen, &mut names, 0)?;
    if names.is_empty() {
        bail!("esse mod já estava instalado");
    }
    Ok(names)
}

#[allow(clippy::too_many_arguments)]
fn install_rec(
    client: &Client,
    mods_dir: &Path,
    project: &str,
    mc: &str,
    em: &Emitter,
    seen: &mut HashSet<String>,
    names: &mut Vec<String>,
    depth: u32,
) -> Result<()> {
    if depth > 4 || !seen.insert(project.to_string()) {
        return Ok(());
    }
    let loaders = "[\"fabric\"]";
    let game_versions = format!("[\"{mc}\"]");
    let versions: Vec<Version> = client
        .get(format!("{API}/project/{project}/version"))
        .query(&[("loaders", loaders), ("game_versions", game_versions.as_str())])
        .send()?
        .error_for_status()
        .context("o Modrinth recusou o pedido de versões")?
        .json()?;
    let v = versions
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("não há versão de {project} para {mc} com Fabric"))?;

    for d in v.dependencies.iter().filter(|d| d.dependency_type == "required") {
        if let Some(p) = &d.project_id {
            install_rec(client, mods_dir, p, mc, em, seen, names, depth + 1)?;
        }
    }

    let file = v
        .files
        .iter()
        .find(|f| f.primary)
        .or(v.files.first())
        .ok_or_else(|| anyhow!("a versão de {project} não tem arquivo"))?;
    let name = Path::new(&file.filename)
        .file_name()
        .ok_or_else(|| anyhow!("nome de arquivo inválido"))?
        .to_os_string();

    em.status(format!("Baixando {}", file.filename));
    let bytes = client.get(&file.url).send()?.error_for_status()?.bytes()?;
    let mut h = Sha1::new();
    h.update(&bytes);
    if !hex::encode(h.finalize()).eq_ignore_ascii_case(&file.hashes.sha1) {
        bail!("hash inválido em {}", file.filename);
    }
    fs::create_dir_all(mods_dir)?;
    fs::write(mods_dir.join(&name), &bytes)?;
    names.push(name.to_string_lossy().to_string());
    Ok(())
}

/// Nomes dos .jar da pasta de mods.
pub fn list_installed(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.ends_with(".jar"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

pub fn remove(dir: &Path, file: &str) -> Result<()> {
    let name = Path::new(file).file_name().ok_or_else(|| anyhow!("nome inválido"))?;
    fs::remove_file(dir.join(name))?;
    Ok(())
}
