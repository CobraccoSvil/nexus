//! Prompt Registry — global agent prompt store populated from DB at startup.
//!
//! `nexus-orchestrator` has no DB access by design (no sqlx dependency in this module).
//! At startup, `mcp-core` queries `nexus_prompt_templates` for all `agent.*` keys
//! and calls `initialize()` to populate this registry.
//!
//! Il registry viene letto dal brain LangGraph (via gRPC) e dai consumer
//! che compongono prompt lato Rust. Se il registry non e' inizializzato
//! (es. unit test), `get_prompt()` ritorna stringa vuota e logga errore.
//!
//! # Esempio minimo (doc test)
//!
//! ```
//! use nexus_orchestrator::prompt_registry::get_prompt;
//! // Registry non inizializzato -> chiavi ignote restituiscono stringa vuota.
//! assert_eq!(get_prompt("agent.inesistente"), "");
//! ```

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

/// Global registry: key → prompt content
static REGISTRY: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

fn registry() -> &'static RwLock<HashMap<String, String>> {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Populate the registry with all `agent.*` prompts fetched from the DB.
///
/// Called once at startup by `NexusBridge::init_global_with_pool`.
/// Subsequent calls merge new entries (updates overwrite, no deletes).
pub fn initialize(prompts: HashMap<String, String>) {
    let n = prompts.len();
    let mut w = registry().write().expect("prompt_registry write lock poisoned");
    for (k, v) in prompts {
        w.insert(k, v);
    }
    tracing::info!("prompt_registry: {} agent prompts loaded from DB (total: {})", n, w.len());
}

/// Retrieve a prompt by key.
///
/// Returns the content string, or an empty string if the key is missing.
/// Missing keys are logged at ERROR level so they're easy to spot.
///
/// # Esempi
///
/// ```
/// use nexus_orchestrator::prompt_registry;
///
/// // Senza inizializzazione, le chiavi mancanti restituiscono stringa vuota
/// let result = prompt_registry::get_prompt("chiave_inesistente");
/// assert_eq!(result, "");
/// ```
pub fn get_prompt(key: &str) -> String {
    let r = registry().read().expect("prompt_registry read lock poisoned");
    if let Some(content) = r.get(key) {
        content.clone()
    } else {
        tracing::error!(
            "AGENT PROMPT MISSING: key='{}' not found in registry. \
             DB not initialized or migration missing. \
             Add via /admin/prompts or run migration 0059.",
            key
        );
        String::new()
    }
}

/// Returns true if the registry has been populated (at least one entry).
pub fn is_initialized() -> bool {
    registry().read().map(|r| !r.is_empty()).unwrap_or(false)
}

// ── Test helpers ─────────────────────────────────────────────────────────────

#[cfg(test)]
pub fn init_test_registry(keys: &[&str]) {
    let mut w = registry().write().expect("lock");
    for key in keys {
        w.entry(key.to_string())
            .or_insert_with(|| format!("(test prompt for {key})"));
    }
}
