//! Lettori best-effort della tabella `settings` con default tipizzato.
//!
//! Punto unico (regola L / ADR 0026) dei piccoli helper `read_bool` / `read_u64`
//! / `read_text` prima duplicati pari-pari in `agent_tool_result_cache`,
//! `project_workspace::user_manager` e `sudo_manager`.
//!
//! Best-effort: in caso di errore DB o valore assente/vuoto ritornano il
//! `default`. La semantica e' preservata rispetto alle versioni locali e NON
//! coincide con `nexus_auth::get_bool_setting` (che accetta anche "1"/"yes"/"on"):
//! qui `read_bool` riconosce solo "true" case-insensitive.

use sqlx::PgPool;

pub(crate) async fn read_bool(db: &PgPool, key: &str, default: bool) -> bool {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(default)
}

pub(crate) async fn read_u64(db: &PgPool, key: &str, default: u64) -> u64 {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

pub(crate) async fn read_text(db: &PgPool, key: &str, default: &str) -> String {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}
