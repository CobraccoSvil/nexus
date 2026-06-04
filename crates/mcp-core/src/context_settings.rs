//! Cache 60s per i parametri dell'auto-compact delle sessioni chat.
//!
//! Niente hardcoded (regola G): i parametri vivono in `settings` (key
//! `agent.context.auto_compact_*`) con default safe applicati dalla migrazione
//! 0277. La cache evita di interrogare il DB ad ogni turno agente.
//!
//! Parametri:
//! - `auto_compact_enabled`: flag master (default true). Se false, l'auto-compact
//!   non scatta mai e il compact resta solo manuale (pulsante "Compatta chat").
//! - `auto_compact_ratio`: soglia ratio = session_tokens / context_window oltre
//!   la quale (>=) la sessione viene compattata prima del turno. Default 0.80,
//!   clampato nel range valido [0.5, 0.95].

use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use sqlx::PgPool;
use tokio::sync::RwLock;

/// Range valido per la soglia di auto-compact. Valori fuori range vengono
/// clampati per evitare configurazioni patologiche (es. 0.0 = compact ad ogni
/// turno, 1.0+ = compact mai).
const RATIO_MIN: f64 = 0.5;
const RATIO_MAX: f64 = 0.95;

/// Parametri dell'auto-compact a soglia.
#[derive(Debug, Clone, Copy)]
pub struct AutoCompactSettings {
    /// Flag master: se false, l'auto-compact non scatta mai.
    pub enabled: bool,
    /// Soglia ratio = session_tokens / context_window (clampata in [0.5, 0.95]).
    pub ratio: f64,
}

impl AutoCompactSettings {
    /// Default safe (identici ai valori della migrazione 0277). Usati solo come
    /// ultima rete se DB down e cache vuota — comportamento documentato.
    pub(crate) const fn safe_defaults() -> Self {
        Self {
            enabled: true,
            ratio: 0.80,
        }
    }
}

const CACHE_TTL: Duration = Duration::from_secs(60);

static CACHE: Lazy<RwLock<Option<(AutoCompactSettings, Instant)>>> =
    Lazy::new(|| RwLock::new(None));

/// Carica i parametri auto-compact dalla tabella `settings`, con cache 60s.
///
/// Se il DB e' down o le chiavi mancano, ritorna i `safe_defaults()` con WARN.
pub async fn current(db: &PgPool) -> AutoCompactSettings {
    {
        let guard = CACHE.read().await;
        if let Some((value, expires)) = *guard {
            if Instant::now() < expires {
                return value;
            }
        }
    }

    let settings = match load_from_db(db).await {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "context_settings: lettura settings agent.context.auto_compact_* fallita, uso safe_defaults"
            );
            AutoCompactSettings::safe_defaults()
        }
    };

    let mut guard = CACHE.write().await;
    *guard = Some((settings, Instant::now() + CACHE_TTL));
    settings
}

async fn load_from_db(db: &PgPool) -> Result<AutoCompactSettings, sqlx::Error> {
    use sqlx::Row;

    let rows = sqlx::query(
        "SELECT key, value FROM settings \
         WHERE key IN ( \
            'agent.context.auto_compact_enabled', \
            'agent.context.auto_compact_ratio' \
         )",
    )
    .fetch_all(db)
    .await?;

    let mut settings = AutoCompactSettings::safe_defaults();
    for row in rows {
        let key: String = row.try_get("key").unwrap_or_default();
        let raw: String = row.try_get("value").unwrap_or_default();
        let raw = raw.trim();
        match key.as_str() {
            "agent.context.auto_compact_enabled" => {
                if let Ok(v) = raw.parse::<bool>() {
                    settings.enabled = v;
                }
            }
            "agent.context.auto_compact_ratio" => {
                if let Ok(v) = raw.parse::<f64>() {
                    settings.ratio = v.clamp(RATIO_MIN, RATIO_MAX);
                }
            }
            _ => {}
        }
    }
    Ok(settings)
}

#[cfg(test)]
pub async fn _reset_for_tests() {
    let mut guard = CACHE.write().await;
    *guard = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_default_in_range() {
        let d = AutoCompactSettings::safe_defaults();
        assert!(d.enabled);
        assert!((RATIO_MIN..=RATIO_MAX).contains(&d.ratio));
    }

    #[test]
    fn ratio_clamp_bounds() {
        assert_eq!((0.1_f64).clamp(RATIO_MIN, RATIO_MAX), RATIO_MIN);
        assert_eq!((0.99_f64).clamp(RATIO_MIN, RATIO_MAX), RATIO_MAX);
        assert_eq!((0.80_f64).clamp(RATIO_MIN, RATIO_MAX), 0.80);
    }
}
