//! Cache + lookup per `nexus_command_hints` (migration 0230).
//!
//! Carica una sola volta tutti gli hint abilitati e li tiene in cache 60s.
//! Il lookup match-prima-substring case-insensitive. Pattern regex non ancora
//! supportato (sicurezza: ReDoS); il filtro DB CHECK lascia entrare 'regex'
//! ma il match qui ricade su substring.
//!
//! Usato da `tool_run_command` per prefissare il tool_result con hint
//! correttivi (es. shadcn-ui rebrand, create-react-app deprecato).

use sqlx::{PgPool, Row};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct CommandHint {
    pub pattern: String,
    pub hint_text: String,
    pub severity: String,
}

struct HintCache {
    entries: Vec<CommandHint>,
    fetched_at: Instant,
}

static CACHE: Mutex<Option<HintCache>> = Mutex::new(None);

async fn fetch_from_db(db: &PgPool) -> Vec<CommandHint> {
    let rows = match sqlx::query(
        "SELECT pattern, hint_text, severity FROM nexus_command_hints \
         WHERE enabled = true ORDER BY length(pattern) DESC",
    )
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("command_hints: fetch DB fallita (tabella mancante?): {e}");
            return Vec::new();
        }
    };
    rows.iter()
        .filter_map(|r| {
            let pattern: String = r.try_get("pattern").ok()?;
            let hint_text: String = r.try_get("hint_text").ok()?;
            let severity: String = r
                .try_get::<String, _>("severity")
                .unwrap_or_else(|_| "info".to_string());
            if pattern.trim().is_empty() || hint_text.trim().is_empty() {
                return None;
            }
            Some(CommandHint {
                pattern,
                hint_text,
                severity,
            })
        })
        .collect()
}

/// Restituisce tutti gli hint che matchano il command. Match substring
/// case-insensitive sul pattern. Pattern piu' lunghi hanno priorita' (gia'
/// ordinati per length DESC in SQL).
///
/// La cache si auto-refresha ogni 60s. Niente errore visibile se il DB e'
/// temporaneamente down: ritorna vuoto.
pub async fn match_hints(db: &PgPool, command: &str) -> Vec<CommandHint> {
    if command.trim().is_empty() {
        return Vec::new();
    }
    let needs_refresh = {
        let guard = CACHE.lock().unwrap();
        match guard.as_ref() {
            None => true,
            Some(c) => c.fetched_at.elapsed() > CACHE_TTL,
        }
    };
    if needs_refresh {
        let fresh = fetch_from_db(db).await;
        let mut guard = CACHE.lock().unwrap();
        *guard = Some(HintCache {
            entries: fresh,
            fetched_at: Instant::now(),
        });
    }
    let lower = command.to_ascii_lowercase();
    let guard = CACHE.lock().unwrap();
    let entries = match guard.as_ref() {
        Some(c) => c.entries.clone(),
        None => return Vec::new(),
    };
    drop(guard);
    entries
        .into_iter()
        .filter(|h| lower.contains(&h.pattern.to_ascii_lowercase()))
        .collect()
}

/// Formatta gli hint matchati come prefisso da prependere al tool_result.
/// Ritorna stringa vuota se nessun hint.
pub fn format_hints_prefix(hints: &[CommandHint]) -> String {
    if hints.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for h in hints {
        let tag = match h.severity.as_str() {
            "error" => "[HINT — ERROR]",
            "warning" => "[HINT — WARNING]",
            _ => "[HINT]",
        };
        out.push_str(&format!(
            "{} (pattern: `{}`)\n{}\n\n",
            tag, h.pattern, h.hint_text
        ));
    }
    out.push_str("---\n");
    out
}
