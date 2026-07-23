//! Tool agente per la gestione dei profili utente.
//!
//! La delega a sotto-agenti NON sta qui: vive in `dispatch_subagent` /
//! `dispatch_subagents`.

use serde_json::Value;
use uuid::Uuid;

use super::ToolContextCore;

// ── Profili utente ──────────────────────────────────────────────────────────

pub async fn tool_create_profile(ctx: &ToolContextCore, input: &Value) -> String {
    let name = match input.get("name").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return "[Errore: parametro 'name' obbligatorio]".to_string(),
    };
    let system_prompt = match input.get("system_prompt").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return "[Errore: parametro 'system_prompt' obbligatorio]".to_string(),
    };
    let emoji = input
        .get("emoji")
        .and_then(Value::as_str)
        .unwrap_or("🤖")
        .trim()
        .to_string();
    let description: Option<String> = input
        .get("description")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let default_provider: Option<String> = input
        .get("default_provider")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let default_model: Option<String> = input
        .get("default_model")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let default_automation: Option<String> = input
        .get("default_automation")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let set_as_default = input
        .get("set_as_default")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Controlla se esiste già un profilo con lo stesso nome per l'utente
    let existing: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM user_profiles WHERE user_id = $1 AND name = $2")
            .bind(ctx.user_id)
            .bind(&name)
            .fetch_optional(&*ctx.db)
            .await
            .unwrap_or(None);

    if existing.is_some() {
        return format!(
            "[Profilo '{}' già esistente. Usa update_profile per modificarlo.]",
            name
        );
    }

    let profile_id = Uuid::new_v4();

    // Se set_as_default, azzera is_default sugli altri
    if set_as_default {
        let _ = sqlx::query("UPDATE user_profiles SET is_default = FALSE WHERE user_id = $1")
            .bind(ctx.user_id)
            .execute(&*ctx.db)
            .await;
    }

    let res = sqlx::query(
        "INSERT INTO user_profiles (id, user_id, name, avatar_emoji, description, system_prompt, \
         default_provider, default_model, default_automation, is_default, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW(), NOW())",
    )
    .bind(profile_id)
    .bind(ctx.user_id)
    .bind(&name)
    .bind(&emoji)
    .bind(&description)
    .bind(&system_prompt)
    .bind(&default_provider)
    .bind(&default_model)
    .bind(&default_automation)
    .bind(set_as_default)
    .execute(&*ctx.db)
    .await;

    match res {
        Ok(_) => format!(
            "Profilo '{}' {} creato con successo (ID: {}). L'utente lo troverà nel selettore profili accanto alla chat.",
            name, emoji,
            profile_id
        ),
        Err(e) => format!("[Errore creazione profilo: {}]", e),
    }
}

pub async fn tool_update_profile(ctx: &ToolContextCore, input: &Value) -> String {
    let profile_name = match input.get("profile_name").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return "[Errore: parametro 'profile_name' obbligatorio]".to_string(),
    };

    // Trova il profilo per nome e user_id
    let row: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, system_prompt, avatar_emoji FROM user_profiles WHERE user_id = $1 AND name = $2"
    )
    .bind(ctx.user_id)
    .bind(&profile_name)
    .fetch_optional(&*ctx.db)
    .await
    .unwrap_or(None);

    let (profile_id, current_prompt, current_emoji) = match row {
        Some(r) => r,
        None => {
            return format!(
                "[Profilo '{}' non trovato. Usa create_profile per crearlo.]",
                profile_name
            )
        }
    };

    let system_prompt = input
        .get("system_prompt")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or(current_prompt);
    let emoji = input
        .get("emoji")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or(current_emoji);
    let description: Option<String> = input
        .get("description")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let res = sqlx::query(
        "UPDATE user_profiles SET system_prompt = $1, avatar_emoji = $2, description = COALESCE($3, description), updated_at = NOW() \
         WHERE id = $4"
    )
    .bind(&system_prompt)
    .bind(&emoji)
    .bind(&description)
    .bind(profile_id)
    .execute(&*ctx.db)
    .await;

    match res {
        Ok(_) => format!("Profilo '{}' aggiornato con successo.", profile_name),
        Err(e) => format!("[Errore aggiornamento profilo: {}]", e),
    }
}
