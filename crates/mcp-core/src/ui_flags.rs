//! Endpoint `GET /api/ui-flags`: espone una WHITELIST di flag UI non sensibili
//! dal DB `settings`, leggibili da QUALUNQUE utente autenticato (middleware
//! `require_auth`, NON `require_admin`).
//!
//! Motivazione (ADR 0037, regola H): i flag di rendering della chat (es.
//! `chat.activity_stream_enabled`) governano cosa vede l'utente. Se fossero
//! leggibili solo via `/api/admin/settings-by-category` (require_admin), per gli
//! utenti non admin il frontend leggerebbe sempre "assente" -> feature morta
//! silenziosa. Questo endpoint espone SOLO le chiavi di una whitelist esplicita,
//! cosi' i valori sensibili di `settings` non trapelano ai non admin.
//!
//! Regola G: nessun valore hardcoded. I valori arrivano dal DB `settings` tramite
//! l'accessor standard (`nexus_auth::get_setting_nonempty`, punto unico di lettura
//! settings — regola L). Le chiavi assenti vengono OMESSE dalla mappa: il frontend
//! applica il proprio default (OFF).

use std::collections::BTreeMap;

use axum::{extract::State, http::StatusCode, Json};
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::AppState;

/// Whitelist costante dei flag UI non sensibili esposti ai non admin. Estensibile:
/// aggiungere qui una chiave settings la rende leggibile da `/api/ui-flags`. NON
/// inserire chiavi sensibili (chiavi API, segreti, connection string).
pub(crate) const UI_FLAG_WHITELIST: &[&str] = &["chat.activity_stream_enabled"];

type ApiError = (StatusCode, Json<Value>);

/// Costruisce la mappa `{ chiave -> valore }` filtrata sulla whitelist leggendo i
/// valori dal DB `settings`. Punto unico della logica (testabile senza HTTP).
///
/// - Lettura via [`nexus_auth::get_setting_nonempty`] (accessor standard, regola
///   L): propaga l'errore DB (regola H, niente ingoio silenzioso) e scarta i
///   valori vuoti/whitespace.
/// - Le chiavi assenti (o vuote) sono OMESSE: il frontend fa default OFF.
///
/// `BTreeMap` per un ordine deterministico della risposta (test stabili).
pub(crate) async fn build_flags_map(
    db: &PgPool,
    whitelist: &[&str],
) -> anyhow::Result<BTreeMap<String, String>> {
    let mut flags = BTreeMap::new();
    for &key in whitelist {
        if let Some(value) = nexus_auth::get_setting_nonempty(db, key).await? {
            flags.insert(key.to_string(), value);
        }
    }
    Ok(flags)
}

/// GET /api/ui-flags
///
/// Risposta: `{ "flags": { "<key>": "<value_string>", ... } }` con SOLO le chiavi
/// della whitelist presenti (non vuote) nel DB. DB irraggiungibile -> HTTP 503
/// (regola G: fallimento visibile, mai un fallback nascosto).
pub async fn get_ui_flags(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let flags = build_flags_map(&state.db, UI_FLAG_WHITELIST)
        .await
        .map_err(|e| {
            tracing::warn!(target: "mcp_core::ui_flags", error = %e, "lettura ui-flags fallita");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "ui-flags non disponibili: DB irraggiungibile" })),
            )
        })?;
    Ok(Json(json!({ "flags": flags })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    /// Crea la tabella `settings` minimale per i test (stesse colonne della
    /// migrazione 0002): key/value/category/description/is_secret/updated_at.
    async fn create_settings_table(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS settings (\
                key TEXT PRIMARY KEY, \
                value TEXT NOT NULL DEFAULT '', \
                category TEXT NOT NULL DEFAULT 'general', \
                description TEXT NOT NULL DEFAULT '', \
                is_secret BOOLEAN NOT NULL DEFAULT FALSE, \
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW())",
        )
        .execute(pool)
        .await
        .expect("create settings");
    }

    async fn set_flag(pool: &PgPool, key: &str, value: &str) {
        sqlx::query("INSERT INTO settings (key, value) VALUES ($1, $2)")
            .bind(key)
            .bind(value)
            .execute(pool)
            .await
            .expect("insert flag");
    }

    /// Chiave presente e non vuota -> compare nella mappa col suo valore RAW.
    #[sqlx::test]
    async fn chiave_presente_nella_mappa(pool: PgPool) {
        create_settings_table(&pool).await;
        set_flag(&pool, "chat.activity_stream_enabled", "true").await;

        let flags = build_flags_map(&pool, &["chat.activity_stream_enabled"])
            .await
            .expect("build ok");
        assert_eq!(
            flags
                .get("chat.activity_stream_enabled")
                .map(String::as_str),
            Some("true"),
        );
    }

    /// Chiave assente -> OMESSA dalla mappa (il frontend fa default OFF).
    #[sqlx::test]
    async fn chiave_assente_e_omessa(pool: PgPool) {
        create_settings_table(&pool).await;
        // Nessuna riga inserita.
        let flags = build_flags_map(&pool, &["chat.activity_stream_enabled"])
            .await
            .expect("build ok");
        assert!(flags.is_empty(), "chiave assente non deve comparire");
    }

    /// Valore vuoto/whitespace -> scartato (get_setting_nonempty filtra i vuoti):
    /// non compare, cosi' il frontend non riceve un flag "attivo" ambiguo.
    #[sqlx::test]
    async fn valore_vuoto_e_scartato(pool: PgPool) {
        create_settings_table(&pool).await;
        set_flag(&pool, "chat.activity_stream_enabled", "   ").await;

        let flags = build_flags_map(&pool, &["chat.activity_stream_enabled"])
            .await
            .expect("build ok");
        assert!(flags.is_empty(), "valore vuoto non deve comparire");
    }

    /// SOLO le chiavi della whitelist finiscono nella mappa: una chiave settings
    /// fuori whitelist (potenzialmente sensibile) NON viene esposta.
    #[sqlx::test]
    async fn solo_whitelist_esposta(pool: PgPool) {
        create_settings_table(&pool).await;
        set_flag(&pool, "chat.activity_stream_enabled", "true").await;
        set_flag(&pool, "openai_api_key", "sk-segreto").await;

        let flags = build_flags_map(&pool, &["chat.activity_stream_enabled"])
            .await
            .expect("build ok");
        assert_eq!(flags.len(), 1, "solo la chiave in whitelist");
        assert!(
            !flags.contains_key("openai_api_key"),
            "una chiave fuori whitelist non deve mai trapelare"
        );
    }
}
