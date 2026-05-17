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

use super::*;
use nexus_events::{dispatcher, event::ProjectEvent};

/// Allowlist di chiavi per `dispatcher_set_flag`. Le chiavi che non matchano
/// nessun prefisso vengono rifiutate per evitare abuso (es. settare chiavi
/// che potrebbero collidere con stato interno).
const FLAG_KEY_PREFIXES: &[&str] = &["build_", "test_", "deploy_", "custom_", "feature_"];

fn is_allowed_flag(key: &str) -> bool {
    FLAG_KEY_PREFIXES.iter().any(|p| key.starts_with(p))
}

pub(super) async fn tool_dispatcher_emit_event(
    ctx: &AgentToolContext,
    input: &Value,
) -> String {
    let event_name = match input.get("kind").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return "[Errore: parametro 'kind' obbligatorio]".to_string(),
    };
    let resource = input
        .get("resource")
        .and_then(Value::as_str)
        .unwrap_or("custom")
        .to_string();
    let payload = input
        .get("payload")
        .cloned()
        .unwrap_or(Value::Null);

    let env = dispatcher::emit(
        &ctx.project_channels,
        ctx.project_id,
        ProjectEvent::Custom {
            event_name: event_name.clone(),
            resource: resource.clone(),
            payload,
        },
    );
    format!(
        "Evento custom emesso: kind={} resource={} seq={}",
        event_name, resource, env.seq
    )
}

pub(super) async fn tool_dispatcher_post_notification(
    ctx: &AgentToolContext,
    input: &Value,
) -> String {
    let severity = input
        .get("severity")
        .and_then(Value::as_str)
        .unwrap_or("info")
        .to_string();
    if !["info", "success", "warning", "error"].contains(&severity.as_str()) {
        return format!(
            "[Errore: severity '{}' non valida. Valori ammessi: info, success, warning, error]",
            severity
        );
    }
    let message = match input.get("message").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return "[Errore: parametro 'message' obbligatorio]".to_string(),
    };
    let panel = input
        .get("panel")
        .and_then(Value::as_str)
        .map(str::to_string);
    let ttl_ms = input.get("ttl_ms").and_then(Value::as_u64);

    let env = dispatcher::emit(
        &ctx.project_channels,
        ctx.project_id,
        ProjectEvent::Notification {
            severity: severity.clone(),
            message: message.clone(),
            panel,
            ttl_ms,
            run_id: ctx.parent_run_id.map(|u| u.to_string()),
        },
    );
    format!(
        "Notifica inviata ({}): {} (seq={})",
        severity, message, env.seq
    )
}

pub(super) async fn tool_dispatcher_set_flag(
    ctx: &AgentToolContext,
    input: &Value,
) -> String {
    if !ctx.can_write {
        return "[Errore: permesso di scrittura non concesso]".to_string();
    }
    let key = match input.get("key").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return "[Errore: parametro 'key' obbligatorio]".to_string(),
    };
    if !is_allowed_flag(&key) {
        return format!(
            "[Errore: chiave '{}' non ammessa. Prefissi consentiti: {}]",
            key,
            FLAG_KEY_PREFIXES.join(", ")
        );
    }
    let value = input.get("value").cloned().unwrap_or(Value::Null);

    // Upsert in DB
    let res = sqlx::query(
        r#"INSERT INTO nexus_project_flags (project_id, key, value, updated_at)
           VALUES ($1, $2, $3, NOW())
           ON CONFLICT (project_id, key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()"#,
    )
    .bind(ctx.project_id)
    .bind(&key)
    .bind(&value)
    .execute(&*ctx.db)
    .await;

    if let Err(e) = res {
        return format!("[Errore DB: {}]", e);
    }

    let env = dispatcher::emit(
        &ctx.project_channels,
        ctx.project_id,
        ProjectEvent::FlagChanged {
            key: key.clone(),
            value: value.clone(),
        },
    );
    format!("Flag '{}' impostata a {} (seq={})", key, value, env.seq)
}

pub(super) async fn tool_dispatcher_update_monitor(
    ctx: &AgentToolContext,
    input: &Value,
) -> String {
    let monitor_id = match input.get("monitor_id").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return "[Errore: parametro 'monitor_id' obbligatorio]".to_string(),
    };
    let value = match input.get("value") {
        Some(v) => v.clone(),
        None => return "[Errore: parametro 'value' obbligatorio]".to_string(),
    };
    let label = input
        .get("label")
        .and_then(Value::as_str)
        .map(str::to_string);

    // Aggiorna registry in-memory
    {
        let mut reg = ctx.monitor_registry.write();
        let project_map = reg.entry(ctx.project_id).or_default();
        project_map.insert(
            monitor_id.clone(),
            serde_json::json!({
                "value": value,
                "label": label,
                "updated_at": chrono::Utc::now().to_rfc3339(),
            }),
        );
    }

    let env = dispatcher::emit(
        &ctx.project_channels,
        ctx.project_id,
        ProjectEvent::MonitorUpdated {
            monitor_id: monitor_id.clone(),
            value: value.clone(),
            label,
        },
    );
    format!(
        "Monitor '{}' aggiornato a {} (seq={})",
        monitor_id, value, env.seq
    )
}

pub(super) async fn tool_dispatcher_highlight_panel(
    ctx: &AgentToolContext,
    input: &Value,
) -> String {
    let panel = match input.get("panel").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return "[Errore: parametro 'panel' obbligatorio]".to_string(),
    };
    let duration_ms = input
        .get("duration_ms")
        .and_then(Value::as_u64)
        .unwrap_or(800)
        .min(5000);

    let env = dispatcher::emit(
        &ctx.project_channels,
        ctx.project_id,
        ProjectEvent::HighlightPanel {
            panel: panel.clone(),
            duration_ms,
        },
    );
    format!(
        "Highlight inviato a pannello '{}' per {}ms (seq={})",
        panel, duration_ms, env.seq
    )
}
