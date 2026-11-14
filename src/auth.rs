//! Login Microsoft -> Xbox Live -> XSTS -> Minecraft Services.
//! (Contas Mojang antigas foram migradas para Microsoft; este é o fluxo oficial.)

use crate::config::Account;
use crate::events::{Emitter, Event};
use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::{Client, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SCOPE: &str = "XboxLive.signin offline_access";
const DEVICE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn check(resp: Response, what: &str) -> Result<Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    let body: String = body.chars().take(300).collect();
    bail!("{what} falhou ({status}): {body}")
}

#[derive(Deserialize)]
struct DeviceResp {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    #[serde(default)]
    interval: u64,
}

#[derive(Deserialize)]
struct TokenResp {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
}

/// Login interativo por "device code": o usuário digita um código em microsoft.com/link.
pub fn login(client: &Client, client_id: &str, em: &Emitter) -> Result<Account> {
    if client_id.trim().is_empty() {
        bail!("Defina o Client ID do Azure nas configurações (veja o README).");
    }
    em.status("Pedindo código de login...");
    let d: DeviceResp = check(
        client
            .post(DEVICE_URL)
            .form(&[("client_id", client_id), ("scope", SCOPE)])
            .send()
            .context("sem conexão com a Microsoft")?,
        "pedido de código",
    )?
    .json()?;

    em.send(Event::DeviceCode {
        code: d.user_code.clone(),
        url: d.verification_uri.clone(),
    });

    let deadline = Instant::now() + Duration::from_secs(d.expires_in);
    let mut interval = d.interval.max(2);
    let tok: TokenResp = loop {
        if Instant::now() > deadline {
            bail!("O código expirou. Tente entrar de novo.");
        }
        std::thread::sleep(Duration::from_secs(interval));
        let resp = client
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", d.device_code.as_str()),
            ])
            .send()?;
        if resp.status().is_success() {
            break resp.json()?;
        }
        let v: Value = resp.json().unwrap_or(Value::Null);
        match v["error"].as_str().unwrap_or("") {
            "authorization_pending" => continue,
            "slow_down" => interval += 5,
            "authorization_declined" | "access_denied" => bail!("Login recusado."),
            "expired_token" => bail!("O código expirou. Tente entrar de novo."),
            other => bail!(
                "Erro de login: {other} {}",
                v["error_description"].as_str().unwrap_or("")
            ),
        }
    };

    em.status("Autenticando no Xbox e no Minecraft...");
    minecraft_from_ms(client, &tok.access_token, tok.refresh_token)
}

fn refresh(client: &Client, client_id: &str, acct: &Account) -> Result<Account> {
    let tok: TokenResp = check(
        client
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", client_id),
                ("refresh_token", acct.refresh.as_str()),
                ("scope", SCOPE),
            ])
            .send()?,
        "renovação do login",
    )
    .map_err(|e| anyhow!("{e}. Entre de novo."))?
    .json()?;
    let refresh = if tok.refresh_token.is_empty() {
        acct.refresh.clone()
    } else {
        tok.refresh_token
    };
    minecraft_from_ms(client, &tok.access_token, refresh)
}

/// Devolve a conta com token válido (renova se estiver perto de expirar).
pub fn ensure_fresh(client: &Client, client_id: &str, acct: &Account) -> Result<Account> {
    if acct.mc_expires > now() + 300 {
        return Ok(acct.clone());
    }
    refresh(client, client_id, acct)
}

fn minecraft_from_ms(client: &Client, ms_token: &str, refresh: String) -> Result<Account> {
    // 1) Xbox Live
    let xbl: Value = check(
        client
            .post("https://user.auth.xboxlive.com/user/authenticate")
            .header("Accept", "application/json")
            .json(&json!({
                "Properties": {
                    "AuthMethod": "RPS",
                    "SiteName": "user.auth.xboxlive.com",
                    "RpsTicket": format!("d={ms_token}")
                },
                "RelyingParty": "http://auth.xboxlive.com",
                "TokenType": "JWT"
            }))
            .send()?,
        "login no Xbox Live",
    )?
    .json()?;
    let xbl_token = xbl["Token"].as_str().ok_or_else(|| anyhow!("resposta Xbox inválida"))?;
    let uhs = xbl["DisplayClaims"]["xui"][0]["uhs"]
        .as_str()
        .ok_or_else(|| anyhow!("resposta Xbox inválida (uhs)"))?;

    // 2) XSTS
    let resp = client
        .post("https://xsts.auth.xboxlive.com/xsts/authorize")
        .header("Accept", "application/json")
        .json(&json!({
            "Properties": { "SandboxId": "RETAIL", "UserTokens": [xbl_token] },
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT"
        }))
        .send()?;
    if resp.status().as_u16() == 401 {
        let v: Value = resp.json().unwrap_or(Value::Null);
        bail!(
            "{}",
            match v["XErr"].as_u64() {
                Some(2148916233) => "Essa conta Microsoft não tem perfil Xbox. Crie um em xbox.com.",
                Some(2148916235) => "Xbox Live indisponível no seu país.",
                Some(2148916236) | Some(2148916237) => "A conta precisa de verificação de adulto.",
                Some(2148916238) => "Conta de menor de idade: precisa ser adicionada a uma família Microsoft.",
                _ => "O Xbox recusou o login (XSTS).",
            }
        );
    }
    let xsts: Value = check(resp, "XSTS")?.json()?;
    let xsts_token = xsts["Token"].as_str().ok_or_else(|| anyhow!("resposta XSTS inválida"))?;

    // 3) Minecraft
    let mc: Value = check(
        client
            .post("https://api.minecraftservices.com/authentication/login_with_xbox")
            .json(&json!({ "identityToken": format!("XBL3.0 x={uhs};{xsts_token}") }))
            .send()?,
        "login no Minecraft",
    )?
    .json()?;
    let mc_token = mc["access_token"]
        .as_str()
        .ok_or_else(|| anyhow!("resposta Minecraft inválida"))?
        .to_string();
    let expires_in = mc["expires_in"].as_u64().unwrap_or(86400);

    // 4) Perfil (falha com 404 se a conta não tem o jogo)
    let resp = client
        .get("https://api.minecraftservices.com/minecraft/profile")
        .bearer_auth(&mc_token)
        .send()?;
    if resp.status().as_u16() == 404 {
        bail!("Essa conta não possui o Minecraft: Java Edition.");
    }
    let profile: Value = check(resp, "perfil do Minecraft")?.json()?;

    Ok(Account {
        name: profile["name"].as_str().unwrap_or("Player").to_string(),
        uuid: profile["id"].as_str().unwrap_or_default().to_string(),
        mc_token,
        mc_expires: now() + expires_in,
        refresh,
    })
}
