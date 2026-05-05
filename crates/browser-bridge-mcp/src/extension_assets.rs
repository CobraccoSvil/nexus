//! Asset di installazione dell'estensione browser.
//!
//! Endpoint:
//!   GET /extension/info             -> JSON { id, version, urls } per UI admin
//!   GET /extension/extension.crx    -> serve il file .crx
//!   GET /extension/update.xml       -> Chrome update manifest (gupdate v2.0)
//!   GET /extension/install.ps1      -> script PowerShell per Windows (registry policy)
//!   GET /extension/install.sh       -> script bash per Linux (managed policies json)
//!
//! Tutti gli artefatti vivono nella directory `BROWSER_BRIDGE_DIST_DIR`
//! (default: <repo>/apps/browser-bridge-extension/dist).
//! L'extension ID e` derivato dalla SubjectPublicKeyInfo della chiave RSA in
//! `dist/key.pem` (PKCS#1 o PKCS#8 PEM): SHA-256 dei primi 16 byte, mappati nibble->[a-p].

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::{DecodePrivateKey, EncodePublicKey};
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub struct ExtensionAssets {
    pub dist_dir: PathBuf,
    pub bind_host: String,
    pub bind_port: u16,
}

impl ExtensionAssets {
    pub fn from_env(bind_host: &str, bind_port: u16) -> Self {
        let dist_dir = std::env::var("BROWSER_BRIDGE_DIST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_dist_dir());
        Self { dist_dir, bind_host: bind_host.to_string(), bind_port }
    }

    fn key_path(&self) -> PathBuf {
        self.dist_dir.join("key.pem")
    }

    fn crx_path(&self) -> Option<PathBuf> {
        find_first_with_ext(&self.dist_dir, "crx")
    }

    fn manifest_version(&self) -> Option<String> {
        // Legge la version dal manifest.json dell'estensione (se presente accanto a dist/).
        let candidates = [
            self.dist_dir.parent().map(|p| p.join("manifest.json")),
            Some(self.dist_dir.join("manifest.json")),
        ];
        for c in candidates.into_iter().flatten() {
            if let Ok(text) = std::fs::read_to_string(&c) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(ver) = v.get("version").and_then(|s| s.as_str()) {
                        return Some(ver.to_string());
                    }
                }
            }
        }
        None
    }

    fn update_url(&self) -> String {
        format!("http://{}:{}/extension/update.xml", self.bind_host, self.bind_port)
    }
    fn crx_url(&self) -> String {
        format!("http://{}:{}/extension/extension.crx", self.bind_host, self.bind_port)
    }
}

fn default_dist_dir() -> PathBuf {
    // Cerca a partire dal CWD; risale fino a trovare apps/browser-bridge-extension/dist.
    if let Ok(cwd) = std::env::current_dir() {
        let mut cur = cwd.as_path();
        loop {
            let candidate = cur.join("apps/browser-bridge-extension/dist");
            if candidate.exists() {
                return candidate;
            }
            match cur.parent() {
                Some(p) => cur = p,
                None => break,
            }
        }
    }
    PathBuf::from("apps/browser-bridge-extension/dist")
}

fn find_first_with_ext(dir: &Path, ext: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut matches: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(ext))
        .collect();
    matches.sort();
    matches.into_iter().next_back()
}

// ---------- Calcolo extension ID ----------

#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error("file mancante: {0}")]
    Missing(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("chiave RSA invalida: {0}")]
    Key(String),
}

pub fn compute_extension_id(key_pem_path: &Path) -> Result<String, AssetError> {
    let pem = std::fs::read_to_string(key_pem_path)
        .map_err(|_| AssetError::Missing(key_pem_path.display().to_string()))?;
    // Chrome puo` generare PKCS#1 ("RSA PRIVATE KEY") o PKCS#8 ("PRIVATE KEY").
    // Prova entrambi.
    let priv_key = RsaPrivateKey::from_pkcs1_pem(&pem)
        .or_else(|_| RsaPrivateKey::from_pkcs8_pem(&pem))
        .map_err(|e| AssetError::Key(format!("PKCS#1/PKCS#8: {e}")))?;
    let pub_key = RsaPublicKey::from(&priv_key);
    // SubjectPublicKeyInfo DER (X.509 SPKI), che e` quello che Chrome firma.
    let spki_der = pub_key
        .to_public_key_der()
        .map_err(|e| AssetError::Key(e.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(spki_der.as_bytes());
    let digest = hasher.finalize();
    let mut id = String::with_capacity(32);
    for byte in &digest[..16] {
        id.push((b'a' + (byte >> 4)) as char);
        id.push((b'a' + (byte & 0x0f)) as char);
    }
    let _ = pub_key.n(); // silenzia warning import non usato in alcune feature combo
    Ok(id)
}

// ---------- Router ----------

pub fn router() -> Router<ExtensionAssets> {
    Router::new()
        .route("/info", get(info))
        .route("/extension.crx", get(serve_crx))
        .route("/update.xml", get(serve_update_xml))
        .route("/install.ps1", get(serve_install_ps1))
        .route("/install.sh", get(serve_install_sh))
}

#[derive(Serialize)]
struct ExtensionInfo {
    extension_id: Option<String>,
    version: Option<String>,
    crx_available: bool,
    crx_url: String,
    update_url: String,
    install_windows_url: String,
    install_linux_url: String,
    error: Option<String>,
}

async fn info(State(a): State<ExtensionAssets>) -> impl IntoResponse {
    let (id, err) = match compute_extension_id(&a.key_path()) {
        Ok(s) => (Some(s), None),
        Err(e) => (None, Some(e.to_string())),
    };
    Json(ExtensionInfo {
        extension_id: id,
        version: a.manifest_version(),
        crx_available: a.crx_path().is_some(),
        crx_url: a.crx_url(),
        update_url: a.update_url(),
        install_windows_url: format!("http://{}:{}/extension/install.ps1", a.bind_host, a.bind_port),
        install_linux_url: format!("http://{}:{}/extension/install.sh", a.bind_host, a.bind_port),
        error: err,
    })
}

async fn serve_crx(State(a): State<ExtensionAssets>) -> Response {
    let Some(path) = a.crx_path() else {
        return (StatusCode::NOT_FOUND, "crx non disponibile (esegui pack.ps1)").into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mut resp = Response::new(Body::from(bytes));
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/x-chrome-extension"),
            );
            let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("extension.crx");
            if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
                resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
            }
            resp
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn serve_update_xml(State(a): State<ExtensionAssets>) -> Response {
    let id = match compute_extension_id(&a.key_path()) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let version = a.manifest_version().unwrap_or_else(|| "0.0.0".to_string());
    let body = format!(
        r#"<?xml version='1.0' encoding='UTF-8'?>
<gupdate xmlns='http://www.google.com/update2/response' protocol='2.0'>
  <app appid='{id}'>
    <updatecheck codebase='{crx}' version='{version}' />
  </app>
</gupdate>
"#,
        crx = a.crx_url(),
    );
    text_response(body, "application/xml; charset=utf-8")
}

async fn serve_install_ps1(State(a): State<ExtensionAssets>) -> Response {
    let id = match compute_extension_id(&a.key_path()) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let manifest_url = a.update_url();
    let body = render_powershell(&id, &manifest_url);
    text_response(body, "text/plain; charset=utf-8")
}

async fn serve_install_sh(State(a): State<ExtensionAssets>) -> Response {
    let id = match compute_extension_id(&a.key_path()) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let manifest_url = a.update_url();
    let body = render_bash(&id, &manifest_url);
    text_response(body, "text/x-shellscript; charset=utf-8")
}

fn text_response(body: String, content_type: &'static str) -> Response {
    let mut resp = Response::new(Body::from(body));
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    resp
}

fn render_powershell(extension_id: &str, manifest_url: &str) -> String {
    // ATTENZIONE: questo testo finisce in uno script eseguito come admin.
    // I valori `extension_id` e `manifest_url` sono auto-generati lato server,
    // non arrivano da input utente: niente rischio di iniezione.
    // Se in futuro accettassimo override esterni, escapare con cura le doppie virgolette.
    let policy_value = format!("{extension_id};{manifest_url}");
    let json_pkg = json!({
        "policy": policy_value,
        "extension_id": extension_id,
    })
    .to_string();
    format!(
        r#"# IDEAI Browser Bridge - installazione automatica via Chrome Enterprise Policy
# Generato dal daemon browser-bridge-mcp. Esegui come Amministratore.
#Requires -RunAsAdministrator
$ErrorActionPreference = "Stop"
$cfg = '{json_pkg}' | ConvertFrom-Json
$key = "HKLM:\Software\Policies\Google\Chrome\ExtensionInstallForcelist"
if (-not (Test-Path $key)) {{ New-Item -Path $key -Force | Out-Null }}

# Rimuovi eventuali entry duplicate per la stessa estensione
$existing = Get-ItemProperty $key -ErrorAction SilentlyContinue
if ($existing) {{
    $existing.PSObject.Properties |
        Where-Object {{ $_.Name -match '^\d+$' -and $_.Value -like ($cfg.extension_id + ';*') }} |
        ForEach-Object {{ Remove-ItemProperty -Path $key -Name $_.Name -ErrorAction SilentlyContinue }}
}}

# Trova primo slot libero
$existing = Get-ItemProperty $key -ErrorAction SilentlyContinue
$used = @()
if ($existing) {{
    $used = $existing.PSObject.Properties |
        Where-Object {{ $_.Name -match '^\d+$' }} |
        ForEach-Object {{ [int]$_.Name }}
}}
$slot = 1
if ($used.Count -gt 0) {{ $slot = (($used | Measure-Object -Maximum).Maximum) + 1 }}

Set-ItemProperty -Path $key -Name "$slot" -Value $cfg.policy -Type String
Write-Host "Policy installata (extension ID: $($cfg.extension_id))."
Write-Host "Riavvia Chrome: l'estensione verra` installata silenziosamente."
"#
    )
}

fn render_bash(extension_id: &str, manifest_url: &str) -> String {
    format!(
        r#"#!/bin/bash
# IDEAI Browser Bridge - installazione via Chrome managed policy (Linux).
# Esegui con sudo. Riavvia Chrome dopo.
set -euo pipefail
DIR="/etc/opt/chrome/policies/managed"
sudo mkdir -p "$DIR"
sudo tee "$DIR/ideai-browser-bridge.json" >/dev/null <<JSON
{{
  "ExtensionInstallForcelist": [ "{extension_id};{manifest_url}" ]
}}
JSON
echo "Policy installata in $DIR/ideai-browser-bridge.json (id: {extension_id})."
echo "Riavvia Chrome per completare l'installazione."
"#
    )
}

// ---------- Test ----------

#[cfg(test)]
mod tests {
    use super::*;

    // Vector di test ufficiale Chromium: una chiave RSA-1024 nota produce un
    // extension ID deterministico. Useremo una chiave generata al volo per il
    // test di stabilita`: due chiamate sulla stessa chiave devono restituire
    // lo stesso ID, e l'ID deve essere 32 caratteri in [a-p].
    #[test]
    fn extension_id_is_deterministic_and_in_alphabet() {
        use rand::rngs::OsRng;
        use rsa::pkcs1::EncodeRsaPrivateKey;
        let key = RsaPrivateKey::new(&mut OsRng, 2048).expect("gen key");
        let pem = key.to_pkcs1_pem(rsa::pkcs8::LineEnding::LF).expect("pem");
        let tmp = std::env::temp_dir().join(format!("bb-test-{}.pem", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, pem.as_bytes()).unwrap();
        let id1 = compute_extension_id(&tmp).expect("id1");
        let id2 = compute_extension_id(&tmp).expect("id2");
        std::fs::remove_file(&tmp).ok();
        assert_eq!(id1, id2);
        assert_eq!(id1.len(), 32);
        assert!(id1.chars().all(|c| ('a'..='p').contains(&c)));
    }

    #[test]
    fn rendered_powershell_contains_id_and_url() {
        let s = render_powershell("abcdefghijklmnopabcdefghijklmnop", "http://127.0.0.1:4055/extension/update.xml");
        assert!(s.contains("abcdefghijklmnopabcdefghijklmnop"));
        assert!(s.contains("update.xml"));
        assert!(s.contains("ExtensionInstallForcelist"));
    }
}
