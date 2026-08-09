//! Tool agente per pilotare il dispatcher centrale di eventi.
//!
//! Il modello AI puo' chiamare questi tool per aggiornare esplicitamente i
//! pannelli del frontend (oltre agli aggiornamenti automatici emessi dai tool
//! che mutano DB).
//!
//! Tool esposti:
//! - `dispatcher_emit_event`        — evento custom (kind/resource/payload)
//! - `dispatcher_post_notification` — toast all'utente
//! - `dispatcher_set_flag`          — flag globale del progetto (persistito)
//! - `dispatcher_update_monitor`    — widget monitor custom (in-memory)
//! - `dispatcher_highlight_panel`   — flash animation su un pannello
//!
//! MIGRATI al contratto d'ingresso e a `RispostaTool` (regola Q).
//!
//! # Cosa la migrazione ha chiuso, oltre alla forma
//!
//! Nessuno dei cinque handler aveva rami nudi — ogni fallimento portava il
//! marker — ma tre parametri arrivavano ai pannelli in silenzio, e il silenzio
//! e' la stessa famiglia di difetto letta dal lato dell'INGRESSO:
//!
//! - `severity` era dichiarato OBBLIGATORIO dal catalogo e l'handler ci
//!   ripiegava su `"info"` quando mancava: un errore annunciato all'utente col
//!   colore di un'informazione;
//! - `payload` era promesso come oggetto e la lettura accettava qualunque JSON,
//!   quindi un payload scalare raggiungeva i pannelli in una forma che nessuno
//!   di loro sa leggere;
//! - `ttl_ms` e `duration_ms` passavano da `as_u64`, che su un valore negativo
//!   o frazionario non fallisce: restituisce `None`, cioe' fa sparire il
//!   parametro e manda l'evento col default.
//!
//! Tutti e tre li chiude il contratto d'ingresso, che pretende il TIPO oltre
//! alla presenza. Il negativo resta l'unico caso che il contratto non puo'
//! fermare (`i64` lo ammette, `ProjectEvent` porta `u64`) ed e' l'unico che
//! questo modulo controlla a mano.

use serde_json::Value;

use super::ToolContextCore;
use crate::input_contract::InputTool;
use crate::tool_inputs::{
    DispatcherEmitEventInput, DispatcherHighlightPanelInput, DispatcherPostNotificationInput,
    DispatcherSetFlagInput, DispatcherUpdateMonitorInput,
};
use nexus_events::{dispatcher, event::ProjectEvent};
use nexus_types::tool_outcome::RispostaTool;

/// Allowlist di chiavi per `dispatcher_set_flag`. Le chiavi che non matchano
/// nessun prefisso vengono rifiutate per evitare abuso (es. settare chiavi
/// che potrebbero collidere con stato interno).
const FLAG_KEY_PREFIXES: &[&str] = &["build_", "test_", "deploy_", "custom_", "feature_"];

/// La categoria attribuita a un evento custom che non la dichiara.
const RESOURCE_DEFAULT: &str = "custom";

/// Durata del flash quando l'agente non la chiede, e tetto oltre il quale la
/// richiesta viene troncata. Entrambi DICHIARATI nella descrizione del campo:
/// il troncamento e' comportamento promesso, non una correzione silenziosa.
const HIGHLIGHT_DEFAULT_MS: u64 = 800;
const HIGHLIGHT_MAX_MS: u64 = 5000;

fn is_allowed_flag(key: &str) -> bool {
    FLAG_KEY_PREFIXES.iter().any(|p| key.starts_with(p))
}

/// Il rifiuto di una stringa obbligatoria arrivata VUOTA.
///
/// RIMEDIABILE: il contratto d'ingresso pretende che il campo ci SIA, non che
/// abbia contenuto, e una stringa vuota qui produce un evento senza nome, un
/// flag senza chiave o un monitor senza identita' — cioe' un aggiornamento che
/// il pannello riceve e non sa a chi attribuire. Il messaggio nomina il campo,
/// che e' tutto cio' che serve per correggere.
fn non_vuoto(campo: &str, valore: &str) -> Option<RispostaTool> {
    if !valore.trim().is_empty() {
        return None;
    }
    Some(RispostaTool::fallito_rimediabile(format!(
        "[Errore: '{campo}' e' vuoto: indica un valore non vuoto]"
    )))
}

/// Una durata in millisecondi dichiarata dall'agente, portata al tipo che
/// `ProjectEvent` usa davvero.
///
/// RIMEDIABILE su un negativo: i millisecondi viaggiano come `u64` e un valore
/// negativo non ha traduzione, ma il rimedio e' evidente e il messaggio lo
/// nomina. Prima la lettura era `as_u64`, che su un negativo restituisce `None`
/// senza distinguerlo da un campo assente: il parametro spariva e l'evento
/// partiva col default, cioe' l'agente otteneva un esito diverso da quello
/// chiesto e nessuno glielo diceva.
fn durata_ms(campo: &str, valore: Option<i64>) -> Result<Option<u64>, RispostaTool> {
    match valore {
        None => Ok(None),
        Some(v) => u64::try_from(v).map(Some).map_err(|_| {
            RispostaTool::fallito_rimediabile(format!(
                "[Errore: '{campo}' deve essere >= 0, ricevuto {v}]"
            ))
        }),
    }
}

pub async fn tool_dispatcher_emit_event(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    let params = match DispatcherEmitEventInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    if let Some(vuoto) = non_vuoto("kind", &params.kind) {
        return vuoto;
    }
    let resource = params
        .resource
        .unwrap_or_else(|| RESOURCE_DEFAULT.to_string());
    // Il contratto pretende un OGGETTO, che e' cio' che il catalogo promette:
    // la lettura a mano prendeva qualunque JSON e lo inoltrava, quindi un
    // payload scalare arrivava ai pannelli in una forma che nessuno sa leggere.
    let payload = params.payload.map_or(Value::Null, Value::Object);

    let env = dispatcher::emit(
        &ctx.project_channels,
        ctx.project_id,
        ProjectEvent::Custom {
            event_name: params.kind.clone(),
            resource: resource.clone(),
            payload,
        },
    );
    RispostaTool::riuscito(format!(
        "Evento custom emesso: kind={} resource={} seq={}",
        params.kind, resource, env.seq
    ))
}

pub async fn tool_dispatcher_post_notification(
    ctx: &ToolContextCore,
    input: &Value,
) -> RispostaTool {
    // `severity` e' obbligatoria per contratto e il suo vocabolario e' un enum:
    // un valore fuori elenco non arriva piu' qui, e il messaggio che ferma la
    // chiamata elenca gia' i valori ammessi senza che nessuno li riscriva.
    let params = match DispatcherPostNotificationInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    if let Some(vuoto) = non_vuoto("message", &params.message) {
        return vuoto;
    }
    let ttl_ms = match durata_ms("ttl_ms", params.ttl_ms) {
        Ok(v) => v,
        Err(risposta) => return risposta,
    };
    let severity = params.severity.come_stringa();

    let env = dispatcher::emit(
        &ctx.project_channels,
        ctx.project_id,
        ProjectEvent::Notification {
            severity: severity.to_string(),
            message: params.message.clone(),
            panel: params.panel,
            ttl_ms,
            run_id: ctx.parent_run_id.map(|u| u.to_string()),
        },
    );
    RispostaTool::riuscito(format!(
        "Notifica inviata ({}): {} (seq={})",
        severity, params.message, env.seq
    ))
}

/// Scrive il flag nel META-DB. Estratta perche' l'handler resti leggibile: la
/// query e' l'unica parte che parla col DB, e il chiamante ne traduce l'esito.
async fn upsert_flag(
    ctx: &ToolContextCore,
    key: &str,
    value: &Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO nexus_project_flags (project_id, key, value, updated_at)
           VALUES ($1, $2, $3, NOW())
           ON CONFLICT (project_id, key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()"#,
    )
    .bind(ctx.project_id)
    .bind(key)
    .bind(value)
    .execute(&*ctx.db)
    .await
    .map(|_| ())
}

pub async fn tool_dispatcher_set_flag(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    if !ctx.can_write {
        // DEL SISTEMA: e' una decisione del progetto sul run, non un parametro
        // della chiamata. Ripetere non la cambia.
        return RispostaTool::fallito_di_sistema("[Errore: permesso di scrittura non concesso]");
    }
    let params = match DispatcherSetFlagInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    if let Some(vuoto) = non_vuoto("key", &params.key) {
        return vuoto;
    }
    if !is_allowed_flag(&params.key) {
        // RIMEDIABILE, e il messaggio dice COME: l'elenco dei prefissi e'
        // esattamente cio' che serve per riscrivere la chiave.
        return RispostaTool::fallito_rimediabile(format!(
            "[Errore: chiave '{}' non ammessa. Rinominala con uno dei prefissi consentiti: {}]",
            params.key,
            FLAG_KEY_PREFIXES.join(", ")
        ));
    }
    let value = params.value.unwrap_or(Value::Null);
    if let Err(e) = upsert_flag(ctx, &params.key, &value).await {
        // DEL SISTEMA: la chiave e' gia' stata validata qui sopra, quindi cio'
        // che resta e' lo stato del DB (irraggiungibile, permessi, vincolo di
        // foreign key su un progetto cancellato). Nessuno di questi cambia
        // ripetendo la stessa chiamata. La natura NON viene dal messaggio di
        // sqlx (regola M): quello e' testo, e qui non abbiamo un codice di
        // errore strutturato da cui distinguere il transitorio.
        return RispostaTool::fallito_di_sistema(format!("[Errore DB: {e}]"));
    }

    let env = dispatcher::emit(
        &ctx.project_channels,
        ctx.project_id,
        ProjectEvent::FlagChanged {
            key: params.key.clone(),
            value: value.clone(),
        },
    );
    RispostaTool::riuscito(format!(
        "Flag '{}' impostata a {} (seq={})",
        params.key, value, env.seq
    ))
}

pub async fn tool_dispatcher_update_monitor(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    let params = match DispatcherUpdateMonitorInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    if let Some(vuoto) = non_vuoto("monitor_id", &params.monitor_id) {
        return vuoto;
    }

    // Riusa l'helper condiviso (regola L): aggiorna registry + emette MonitorUpdated.
    let seq = super::monitor::set_monitor(
        &ctx.monitor_registry,
        &ctx.project_channels,
        ctx.project_id,
        &params.monitor_id,
        params.value.clone(),
        params.label,
    );
    RispostaTool::riuscito(format!(
        "Monitor '{}' aggiornato a {} (seq={})",
        params.monitor_id, params.value, seq
    ))
}

pub async fn tool_dispatcher_highlight_panel(
    ctx: &ToolContextCore,
    input: &Value,
) -> RispostaTool {
    let params = match DispatcherHighlightPanelInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    if let Some(vuoto) = non_vuoto("panel", &params.panel) {
        return vuoto;
    }
    let duration_ms = match durata_ms("duration_ms", params.duration_ms) {
        Ok(v) => v.unwrap_or(HIGHLIGHT_DEFAULT_MS).min(HIGHLIGHT_MAX_MS),
        Err(risposta) => return risposta,
    };

    let env = dispatcher::emit(
        &ctx.project_channels,
        ctx.project_id,
        ProjectEvent::HighlightPanel {
            panel: params.panel.clone(),
            duration_ms,
        },
    );
    RispostaTool::riuscito(format!(
        "Highlight inviato a pannello '{}' per {}ms (seq={})",
        params.panel, duration_ms, env.seq
    ))
}
