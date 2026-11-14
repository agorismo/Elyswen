//! Perfis de mod loader (por enquanto: Fabric).

use anyhow::{anyhow, Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;

#[derive(Deserialize)]
struct LoaderEntry {
    loader: LoaderVer,
}

#[derive(Deserialize)]
struct LoaderVer {
    version: String,
    stable: bool,
}

/// Devolve (id do perfil, json do perfil) para a versão do Minecraft `mc`.
pub fn fabric_profile(client: &Client, mc: &str) -> Result<(String, String)> {
    let unsupported = || anyhow!("o Fabric não tem suporte para {mc}");
    let list: Vec<LoaderEntry> = client
        .get(format!("https://meta.fabricmc.net/v2/versions/loader/{mc}"))
        .send()?
        .error_for_status()
        .with_context(unsupported)?
        .json()?;
    let loader = list
        .iter()
        .find(|e| e.loader.stable)
        .or(list.first())
        .ok_or_else(unsupported)?
        .loader
        .version
        .clone();

    let text = client
        .get(format!("https://meta.fabricmc.net/v2/versions/loader/{mc}/{loader}/profile/json"))
        .send()?
        .error_for_status()?
        .text()?;
    let v: serde_json::Value = serde_json::from_str(&text)?;
    let id = v["id"]
        .as_str()
        .ok_or_else(|| anyhow!("perfil do Fabric sem id"))?
        .to_string();
    Ok((id, text))
}
