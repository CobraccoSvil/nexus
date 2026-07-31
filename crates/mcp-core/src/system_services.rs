//! Stato e controllo dei MICROSERVIZI INFRASTRUTTURA Nexus (mcp-core, gateway,
//! admin/doc/billing/plugin-service, ...), platform-aware.
//!
//! Punto unico (regola L) per:
//!   - il pannello "Servizi Nexus" del web-ide, che consuma
//!     `GET /api/system/services` e `POST /api/system/services/:name/:action`
//!     via proxy Next.js (prima questi due route re-implementavano il controllo
//!     con `systemctl` + `child_process.exec`, che su Windows nativo fallivano
//!     sempre con HTTP 500 e mascheravano lo stato "unknown" ad "active" con
//!     un'euristica port_alive — vedi ADR/CLAUDE.md regole G/H/L/M);
//!   - il `services_watchdog`, che filtra dal catalogo le voci
//!     `watchdog_managed=true`.
//!
//! Fonte di verita' UNICA: il setting `system.services_catalog` (migrazione
//! 0541). Niente nomi unit / porte hardcoded nel codice (regola G).
//!
//! STATO onesto (regola M): derivato da un TCP probe della porta risolta dal DB,
//! segnale strutturato identico su Windows e Unix. Non si interroga piu'
//! `systemctl` (assente su Windows) e non si maschera "unknown" ad "active".
//!
//! CONTROLLO platform-aware (regola L: una ricetta per OS):
//!   - Unix: `systemctl <action> <systemd_unit>` (prova --user poi system);
//!   - Windows: `deploy/dev-service.ps1 -Action <action> -Service <winsw_id>`,
//!     che gestisce sia il modello WinSW sia il modello a processi di
//!     `dev-start.ps1` (PID file + manifest), unica ricetta Windows riusata
//!     anche dal watchdog.
//!   - mcp-core controlla se stesso in modalita' detached: il comando lo
//!     terminerebbe a meta' risposta, quindi si lancia in background e si
//!     risponde subito.

use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::project_workspace::port_recovery::port_listening;
use crate::AppState;

// Forma delle voci e lettura del catalogo: punto unico in `nexus-service-catalog`
// (regola L). Qui restano solo probe, controllo e handler HTTP, cioe' cio' che
// e' proprio di mcp-core. La struttura era `pub(crate)`, quindi il generatore
// dei manifest avrebbe dovuto ricopiarsela per leggere lo stesso dato.
pub(crate) use nexus_service_catalog::{resolve_port, CatalogEntry};

/// Servizio che ospita questo processo: si controlla in modalita' detached.
const SELF_SERVICE_NAME: &str = "mcp-core";

/// Carica il catalogo delegando al punto unico, riportando l'esito come lista.
///
/// I chiamanti storici (pannello, watchdog, controllo) trattavano "catalogo non
/// leggibile" come "nessun servizio". Il punto unico ora distingue i tre casi;
/// qui si logga il MOTIVO e si degrada a lista vuota, cosi' il pannello resta
/// servito ma il log dice se il catalogo manca o se non si e' potuto leggere.
pub(crate) async fn load_catalog(db: &PgPool) -> Vec<CatalogEntry> {
    match nexus_service_catalog::load_catalog(db).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("system_services: catalogo non disponibile: {e}");
            Vec::new()
        }
    }
}

/// Deriva lo stato onesto dalla coppia (porta risolvibile?, porta in ascolto?).
/// Funzione PURA per essere testabile senza IO. "unknown" solo quando la porta
/// NON e' risolvibile dal catalogo (errore di config): non e' un mascheramento,
/// e' l'assenza reale del segnale.
fn derive_state(port: Option<u16>, alive: bool) -> (&'static str, &'static str) {
    match port {
        None => ("unknown", "porta non risolvibile dal catalogo"),
        Some(_) if alive => ("active", "listening"),
        Some(_) => ("inactive", "porta non in ascolto"),
    }
}

/// Stato di un servizio restituito dall'endpoint. Il contratto (name, label,
/// port, description, led, readonly, state, sub_state, port_alive) e' quello
/// atteso dal pannello `NexusServicesSection` del web-ide.
#[derive(Debug, Serialize)]
pub(crate) struct ServiceStatus {
    pub name: String,
    pub label: String,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub led: Option<String>,
    pub readonly: bool,
    pub controllable: bool,
    pub state: String,
    pub sub_state: String,
    /// true se il TCP probe ha risposto. Coincide con `state == "active"`: qui
    /// e' un segnale onesto, NON un override che maschera "unknown".
    pub port_alive: bool,
}

/// Probe di stato di una singola voce (porta risolta dal DB + TCP probe).
async fn probe_entry(db: &PgPool, entry: &CatalogEntry) -> ServiceStatus {
    let port = resolve_port(db, entry).await;
    let alive = match port {
        Some(p) => port_listening(p).await,
        None => false,
    };
    let (state, sub_state) = derive_state(port, alive);
    ServiceStatus {
        name: entry.name.clone(),
        label: if entry.label.is_empty() {
            entry.name.clone()
        } else {
            entry.label.clone()
        },
        port: port.unwrap_or(0),
        description: entry.description.clone(),
        led: entry.led.clone(),
        readonly: entry.readonly,
        controllable: entry.controllable,
        state: state.to_string(),
        sub_state: sub_state.to_string(),
        port_alive: alive,
    }
}

/// Azione di controllo. Segnale strutturato: si valida alla fonte, mai dal testo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Start,
    Stop,
    Restart,
}

impl Action {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            "restart" => Some(Self::Restart),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

/// Esito dell'esecuzione di un comando di controllo.
pub(crate) struct ControlOutcome {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Errore strutturato del controllo (regola M): il chiamante decide sullo
/// stato macchina, non sul testo.
#[derive(Debug)]
pub(crate) enum ControlError {
    /// Nome non presente nel catalogo.
    NotFound,
    /// Voce presente ma non controllabile (readonly / controllable=false).
    NotControllable,
    /// Errore di esecuzione del comando platform-specifico.
    Exec(String),
}

/// Controlla un servizio infrastruttura in modo platform-aware. Punto unico
/// riusato dall'endpoint e dal watchdog (per il restart su Windows).
pub(crate) async fn control_service(
    db: &PgPool,
    name: &str,
    action: Action,
) -> Result<ControlOutcome, ControlError> {
    let entry = load_catalog(db)
        .await
        .into_iter()
        .find(|e| e.name == name)
        .ok_or(ControlError::NotFound)?;
    if !entry.controllable {
        return Err(ControlError::NotControllable);
    }
    // mcp-core controlla se stesso: il comando lo termina a meta' risposta,
    // quindi si lancia detached e si risponde subito.
    let detached = entry.name == SELF_SERVICE_NAME;
    run_control(&entry, action, detached)
        .await
        .map_err(ControlError::Exec)
}

/// Controllo su Unix via `systemctl` (prova --user poi system, come i due route
/// originali). Il target e' `systemd_unit` dal catalogo.
#[cfg(unix)]
async fn run_control(
    entry: &CatalogEntry,
    action: Action,
    detached: bool,
) -> Result<ControlOutcome, String> {
    let unit = entry
        .systemd_unit
        .as_deref()
        .ok_or_else(|| format!("systemd_unit non configurato per {}", entry.name))?;
    let act = action.as_str();

    if detached {
        let shell = format!(
            "( systemctl --user {act} '{unit}' || systemctl {act} '{unit}' ) >/dev/null 2>&1 &"
        );
        tokio::process::Command::new("bash")
            .args(["-lc", &shell])
            .spawn()
            .map_err(|e| format!("spawn systemctl detached fallito: {e}"))?;
        return Ok(ControlOutcome {
            ok: true,
            stdout: format!("{act} {unit} avviato in background"),
            stderr: String::new(),
        });
    }

    let user = tokio::process::Command::new("systemctl")
        .args(["--user", act, unit])
        .output()
        .await
        .map_err(|e| format!("exec systemctl --user fallito: {e}"))?;
    if user.status.success() {
        return Ok(ControlOutcome {
            ok: true,
            stdout: String::from_utf8_lossy(&user.stdout).trim().to_string(),
            stderr: String::new(),
        });
    }

    let sys = tokio::process::Command::new("systemctl")
        .args([act, unit])
        .output()
        .await
        .map_err(|e| format!("exec systemctl fallito: {e}"))?;
    let stderr = String::from_utf8_lossy(&sys.stderr).trim().to_string();
    Ok(ControlOutcome {
        ok: sys.status.success(),
        stdout: String::from_utf8_lossy(&sys.stdout).trim().to_string(),
        stderr: if sys.status.success() {
            String::new()
        } else if !stderr.is_empty() {
            stderr
        } else {
            String::from_utf8_lossy(&user.stderr).trim().to_string()
        },
    })
}

/// Controllo su Windows via `deploy/dev-service.ps1`, punto unico che copre sia
/// il modello WinSW sia il modello a processi di `dev-start.ps1`. Il target e'
/// `winsw_id` dal catalogo.
#[cfg(windows)]
async fn run_control(
    entry: &CatalogEntry,
    action: Action,
    detached: bool,
) -> Result<ControlOutcome, String> {
    let winsw = entry
        .winsw_id
        .as_deref()
        .ok_or_else(|| format!("winsw_id non configurato per {}", entry.name))?;
    let act = action.as_str();
    let script = find_repo_script("deploy/dev-service.ps1").ok_or_else(|| {
        "deploy/dev-service.ps1 non trovato (impostare NEXUS_REPO_ROOT o avviare dalla repo root)"
            .to_string()
    })?;
    let script_str = script.to_string_lossy().to_string();
    let args: [&str; 10] = [
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        script_str.as_str(),
        "-Action",
        act,
        "-Service",
        winsw,
    ];

    if detached {
        // Self-restart di mcp-core: lo script deve sopravvivere alla morte di
        // mcp-core. Lo lanciamo DETACHED (nuovo gruppo processi, niente console
        // ereditata) con std::process::Command fire-and-forget. dev-service.ps1
        // termina mcp-core senza /T (vedi lo script) per non abbattere anche
        // questo figlio detached.
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        std::process::Command::new("powershell")
            .args(args)
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .map_err(|e| format!("spawn dev-service.ps1 detached fallito: {e}"))?;
        return Ok(ControlOutcome {
            ok: true,
            stdout: format!("{act} {winsw} avviato in background"),
            stderr: String::new(),
        });
    }

    let out = tokio::process::Command::new("powershell")
        .args(args)
        .output()
        .await
        .map_err(|e| format!("exec dev-service.ps1 fallito: {e}"))?;
    Ok(ControlOutcome {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

/// Individua uno script del repo su Windows: prima `NEXUS_REPO_ROOT`, poi
/// risalendo da `current_dir` (mcp-core gira dalla repo root dello stack dev).
#[cfg(windows)]
fn find_repo_script(rel: &str) -> Option<std::path::PathBuf> {
    if let Ok(root) = std::env::var("NEXUS_REPO_ROOT") {
        let p = std::path::Path::new(&root).join(rel);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(mut dir) = std::env::current_dir() {
        for _ in 0..6 {
            let p = dir.join(rel);
            if p.exists() {
                return Some(p);
            }
            if !dir.pop() {
                break;
            }
        }
    }
    None
}

// ── Handler HTTP ─────────────────────────────────────────────────────────────

/// `GET /api/system/services` — stato dei microservizi mostrati nel pannello.
pub(crate) async fn get_system_services(State(state): State<AppState>) -> Json<Value> {
    let shown: Vec<CatalogEntry> = load_catalog(&state.db)
        .await
        .into_iter()
        .filter(|e| e.panel_shown)
        .collect();
    let services = futures::future::join_all(shown.iter().map(|e| probe_entry(&state.db, e))).await;
    Json(json!({ "services": services }))
}

/// `POST /api/system/services/:service/:action` — controlla un servizio.
/// Ritorna 200 con `{ok:false, stderr}` quando il comando gira ma fallisce
/// (cosi' il pannello mostra l'errore reale); non-2xx solo per richiesta
/// invalida o errore interno.
pub(crate) async fn post_system_service_action(
    State(state): State<AppState>,
    Path((service, action)): Path<(String, String)>,
) -> (axum::http::StatusCode, Json<Value>) {
    use axum::http::StatusCode;

    let Some(act) = Action::parse(&action) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false, "service": service, "action": action,
                "stderr": format!("azione non valida: {action}"),
            })),
        );
    };

    match control_service(&state.db, &service, act).await {
        Ok(outcome) => (
            StatusCode::OK,
            Json(json!({
                "ok": outcome.ok,
                "service": service,
                "action": action,
                "stdout": outcome.stdout,
                "stderr": outcome.stderr,
            })),
        ),
        Err(ControlError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "ok": false, "service": service, "action": action,
                "stderr": format!("servizio non presente nel catalogo: {service}"),
            })),
        ),
        Err(ControlError::NotControllable) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false, "service": service, "action": action,
                "stderr": format!("servizio non controllabile: {service}"),
            })),
        ),
        Err(ControlError::Exec(msg)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "ok": false, "service": service, "action": action, "stderr": msg,
            })),
        ),
    }
}

// ── Test ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Voci di comodo per esercitare i FILTRI di questo modulo.
    ///
    /// PREMESSA DICHIARATA (regola O): questo JSON e' scritto qui, non letto
    /// dalla migrazione. Copre la logica di filtro — chi finisce nel pannello,
    /// chi e' sorvegliato — e NON dice nulla sul catalogo reale: non si e' mai
    /// accorto ne' dei sette `systemd_unit` che sul disco non esistono, ne' del
    /// manifest mancante di browser-bridge, e non e' costruito per accorgersene.
    /// La conformita' del catalogo VERO la verifica il test su DB del generatore
    /// di manifest, che lo legge dalle migrazioni.
    const FILTRI_INPUT: &str = r#"[
      {"name":"mcp-core","label":"Core","port_setting_key":"mcp_core_http_port","led":"Core","readonly":false,"controllable":true,"panel_shown":true,"watchdog_managed":false,"systemd_unit":"nexus-core-wsl","winsw_id":"nexus-mcp-core"},
      {"name":"nexus-gateway","label":"LLM Gateway","port_setting_key":"nexus_gateway_port","controllable":true,"panel_shown":true,"watchdog_managed":true,"systemd_unit":"nexus-gateway","winsw_id":"nexus-gateway"},
      {"name":"web-ide","label":"Web IDE","port_setting_key":"web_ide_port","controllable":false,"panel_shown":false,"watchdog_managed":true,"systemd_unit":"nexus-webide","winsw_id":"nexus-web-ide"},
      {"name":"postgres","label":"PostgreSQL","port":5433,"readonly":true,"controllable":false,"panel_shown":true,"watchdog_managed":false}
    ]"#;

    fn parse() -> Vec<CatalogEntry> {
        serde_json::from_str::<Vec<CatalogEntry>>(FILTRI_INPUT).expect("input dei filtri valido")
    }

    // La deserializzazione della voce (campi obbligatori, default, round-trip)
    // e' testata nel punto unico `nexus-service-catalog`, dove la struttura
    // vive: ripeterla qui sarebbe una seconda copia della stessa verifica.

    #[test]
    fn filtro_panel_shown() {
        let shown: Vec<_> = parse().into_iter().filter(|e| e.panel_shown).collect();
        // web-ide escluso dal pannello (self-lockout).
        assert_eq!(shown.len(), 3);
        assert!(shown.iter().all(|e| e.name != "web-ide"));
    }

    #[test]
    fn filtro_watchdog_managed_esclude_mcp_core() {
        let managed: Vec<_> = parse()
            .into_iter()
            .filter(|e| e.watchdog_managed)
            .map(|e| e.name)
            .collect();
        assert!(managed.contains(&"nexus-gateway".to_string()));
        assert!(managed.contains(&"web-ide".to_string()));
        // mcp-core NON e' auto-restartato: ospita il watchdog.
        assert!(!managed.contains(&"mcp-core".to_string()));
    }

    #[test]
    fn stato_derivato_onesto() {
        // Porta risolvibile e in ascolto -> active.
        assert_eq!(derive_state(Some(4000), true).0, "active");
        // Porta risolvibile ma non in ascolto -> inactive (niente maschera).
        assert_eq!(derive_state(Some(4000), false).0, "inactive");
        // Porta NON risolvibile dal catalogo -> unknown onesto.
        assert_eq!(derive_state(None, false).0, "unknown");
    }

    #[test]
    fn azione_parse_solo_valide() {
        assert_eq!(Action::parse("start"), Some(Action::Start));
        assert_eq!(Action::parse("stop"), Some(Action::Stop));
        assert_eq!(Action::parse("restart"), Some(Action::Restart));
        assert_eq!(Action::parse("delete"), None);
        assert_eq!(Action::parse(""), None);
    }
}
