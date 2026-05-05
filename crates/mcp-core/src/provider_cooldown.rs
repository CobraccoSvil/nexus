//! Cooldown e circuit breaker per provider LLM.
//!
//! Estratto da `agent_loop.rs` durante la Fase 4 del refactor Nexus: i symbol
//! `is_provider_in_cooldown`, `put_provider_in_cooldown`,
//! `reset_provider_failures`, `all_providers_in_cooldown` sono usati anche
//! fuori dal loop agente (es. `orchestrator.rs`).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

static PROVIDER_COOLDOWN: OnceLock<Mutex<HashMap<String, std::time::Instant>>> = OnceLock::new();
const PROVIDER_COOLDOWN_SECS: u64 = 300; // 5 minuti (default se retry-after assente)
const PROVIDER_COOLDOWN_MAX_SECS: u64 = 3600; // cap superiore: 1 ora
const PROVIDER_COOLDOWN_MIN_SECS: u64 = 10; // cap inferiore per evitare hammering

// -- Circuit breaker state --
// Traccia gli istanti dei fallimenti recenti per provider. Se 3+ fallimenti in 60s
// entriamo in stato OPEN con cooldown esteso.
static PROVIDER_FAILURES: OnceLock<Mutex<HashMap<String, Vec<std::time::Instant>>>> = OnceLock::new();
const CIRCUIT_BREAKER_WINDOW_SECS: u64 = 60;
const CIRCUIT_BREAKER_THRESHOLD: usize = 3;
const CIRCUIT_BREAKER_EXTENDED_COOLDOWN_SECS: u64 = 600; // 10 minuti dopo threshold

pub fn is_provider_in_cooldown(provider: &str) -> bool {
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(map) = store.lock() {
        if let Some(&until) = map.get(&provider.to_lowercase()) {
            return std::time::Instant::now() < until;
        }
    }
    false
}

/// Registra un fallimento per il provider e restituisce true se la soglia
/// del circuit breaker e' stata superata (3+ fallimenti in 60s).
fn record_provider_failure(provider: &str) -> bool {
    let store = PROVIDER_FAILURES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        let now = std::time::Instant::now();
        let window = std::time::Duration::from_secs(CIRCUIT_BREAKER_WINDOW_SECS);
        let entry = map.entry(provider.to_lowercase()).or_insert_with(Vec::new);
        entry.retain(|&t| now.duration_since(t) < window);
        entry.push(now);
        entry.len() >= CIRCUIT_BREAKER_THRESHOLD
    } else {
        false
    }
}

/// Reset del contatore fallimenti (chiamare su successo = stato CLOSED).
#[allow(dead_code)]
pub(crate) fn reset_provider_failures(provider: &str) {
    let store = PROVIDER_FAILURES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        map.remove(&provider.to_lowercase());
    }
}

/// Mette un provider in cooldown. Se `retry_after_seconds` e' fornito dal
/// provider (header Retry-After), lo usa con un cap a [10s, 3600s]. Altrimenti
/// default 300s. Se il circuit breaker scatta, cooldown esteso a 600s.
pub(crate) fn put_provider_in_cooldown(provider: &str, retry_after_seconds: Option<u64>) {
    let breaker_tripped = record_provider_failure(provider);
    let base_secs = retry_after_seconds
        .map(|s| s.clamp(PROVIDER_COOLDOWN_MIN_SECS, PROVIDER_COOLDOWN_MAX_SECS))
        .unwrap_or(PROVIDER_COOLDOWN_SECS);
    let secs = if breaker_tripped {
        base_secs.max(CIRCUIT_BREAKER_EXTENDED_COOLDOWN_SECS)
    } else {
        base_secs
    };
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        if breaker_tripped {
            tracing::warn!(
                "Provider '{}' circuit breaker OPEN: cooldown esteso {}s (>= {} fallimenti in {}s)",
                provider, secs, CIRCUIT_BREAKER_THRESHOLD, CIRCUIT_BREAKER_WINDOW_SECS
            );
        } else {
            tracing::warn!(
                "Provider '{}' in cooldown per {}s (retry_after={:?})",
                provider, secs, retry_after_seconds
            );
        }
        map.insert(provider.to_lowercase(), until);
    }
}

/// Cooldown lungo (default 6h) per errori non recuperabili a breve termine
/// (credit balance too low, quota daily exceeded). Il provider non viene più
/// scelto né dal routing automatico né dal fallback finché il cooldown non
/// scade — costringe Nexus a usare un altro provider della gerarchia.
const PROVIDER_COOLDOWN_LONG_SECS: u64 = 6 * 3600; // 6 ore

/// Connessione Redis globale per persistere il cooldown lungo (sopravvive
/// al riavvio di mcp-core). Inizializzata da `main.rs::main` dopo init_redis.
static REDIS_CLIENT: OnceLock<redis::aio::MultiplexedConnection> = OnceLock::new();

/// Inizializza il client Redis globale per la persistenza dei cooldown
/// lunghi. Chiamato una volta da `main.rs` dopo `cache::init_redis`.
/// Il client e' clonabile (è un Arc internamente) — ne salviamo un clone.
pub fn init_redis_client(client: redis::aio::MultiplexedConnection) {
    let _ = REDIS_CLIENT.set(client);
}

/// Variante "lunga" di `put_provider_in_cooldown` per errori semantici tipo
/// "credit balance too low" / "quota exceeded" che non si risolvono in pochi
/// minuti (servono soldi/giorni). Bypassa il circuit breaker.
///
/// Persistenza: oltre allo store in-memory, scrive anche su Redis con TTL
/// pari al cooldown — cosi' al riavvio mcp-core ricarica il cooldown via
/// `restore_cooldown` invece di partire pulito (quel restart era il bug
/// "LED openai verde" segnalato dall'utente).
pub fn put_provider_in_long_cooldown(provider: &str, reason: &str) {
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(PROVIDER_COOLDOWN_LONG_SECS);
        map.insert(provider.to_lowercase(), until);
        tracing::warn!(
            "Provider '{}' in COOLDOWN LUNGO ({}s, {} ore) per: {}",
            provider, PROVIDER_COOLDOWN_LONG_SECS, PROVIDER_COOLDOWN_LONG_SECS / 3600, reason,
        );
    }
    // Salva anche il motivo nel registro motivazioni
    let reasons = PROVIDER_COOLDOWN_REASONS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = reasons.lock() {
        map.insert(provider.to_lowercase(), reason.to_string());
    }
    // Persistenza Redis fire-and-forget. Stesso schema usato da
    // `gateway_providers_handler` (chiave `nexus:billing_cooldown:<provider>`)
    // cosi' il restore al riavvio funziona uniformemente.
    if let Some(conn) = REDIS_CLIENT.get() {
        let provider = provider.to_lowercase();
        let reason = reason.to_string();
        let mut conn = conn.clone();
        tokio::spawn(async move {
            let now_ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let until_ts = now_ts.saturating_add(PROVIDER_COOLDOWN_LONG_SECS);
            let key = format!("nexus:billing_cooldown:{}", provider);
            let value = format!("{}|{}", until_ts, reason);
            let res: Result<(), _> = redis::cmd("SET")
                .arg(&key)
                .arg(&value)
                .arg("EX")
                .arg(PROVIDER_COOLDOWN_LONG_SECS + 60)
                .query_async(&mut conn)
                .await;
            if let Err(e) = res {
                tracing::warn!(
                    "put_provider_in_long_cooldown: persistenza Redis fallita per '{}': {}",
                    provider, e
                );
            }
        });
    }
}

/// Cooldown breve (default 60s) per errori transient (5xx, rate limit short window).
/// Diverso dal long cooldown (6h) usato per billing/quota esaurita: qui il provider
/// si presume tornera' funzionante in pochi secondi/minuti, quindi non vale la pena
/// escluderlo per ore. Solo in-memory: non persistito su Redis perche' il valore di
/// 60s e' minore del tempo medio di restart del processo.
pub fn put_provider_in_short_cooldown(provider: &str, reason: &str, duration_secs: u64) {
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(duration_secs);
        map.insert(provider.to_lowercase(), until);
        tracing::warn!(
            "Provider '{}' in COOLDOWN BREVE ({}s) per: {}",
            provider, duration_secs, reason
        );
    }
    let reasons = PROVIDER_COOLDOWN_REASONS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = reasons.lock() {
        map.insert(provider.to_lowercase(), reason.to_string());
    }
}

/// Registro dei motivi di cooldown ("credit balance too low", "rate limit", …).
/// Esposto al frontend via `cooldown_snapshot()` → reason mostrato nel LED tooltip.
static PROVIDER_COOLDOWN_REASONS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

/// Snapshot di tutti i provider attualmente in cooldown.
/// Returns: vec di (provider_name, remaining_seconds, reason)
pub fn cooldown_snapshot() -> Vec<(String, u64, Option<String>)> {
    let store = match PROVIDER_COOLDOWN.get() {
        Some(s) => s,
        None => return Vec::new(),
    };
    let map = match store.lock() { Ok(m) => m, Err(_) => return Vec::new() };
    let reasons = PROVIDER_COOLDOWN_REASONS.get().and_then(|s| s.lock().ok());
    let now = std::time::Instant::now();
    let mut out = Vec::new();
    for (name, &until) in map.iter() {
        if until > now {
            let secs = (until - now).as_secs().max(1);
            let reason = reasons.as_ref().and_then(|r| r.get(name).cloned());
            out.push((name.clone(), secs, reason));
        }
    }
    out
}

/// Ripristina un cooldown (billing) da un timestamp letto da Redis dopo riavvio.
/// `remaining_secs`: secondi rimasti al momento della lettura.
pub fn restore_cooldown(provider: &str, remaining_secs: u64, reason: &str) {
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(remaining_secs);
        map.insert(provider.to_lowercase(), until);
        tracing::info!(
            "Provider '{}' cooldown ripristinato da Redis: {}s rimanenti, motivo: {}",
            provider, remaining_secs, reason
        );
    }
    let reasons = PROVIDER_COOLDOWN_REASONS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = reasons.lock() {
        map.insert(provider.to_lowercase(), reason.to_string());
    }
}

/// Controlla se tutti i provider nell'ordine di fallback sono in cooldown.
/// Restituisce `Some(secondi_rimanenti)` se tutti sono in cooldown, `None` se almeno uno è disponibile.
#[allow(dead_code)]
pub(crate) fn all_providers_in_cooldown(provider_order: &[String]) -> Option<u64> {
    if provider_order.is_empty() { return None; }
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    let map = match store.lock() { Ok(m) => m, Err(_) => return None };
    let now = std::time::Instant::now();
    let mut min_remaining: Option<u64> = None;
    for p in provider_order {
        match map.get(&p.to_lowercase()) {
            Some(&until) if until > now => {
                let secs = (until - now).as_secs().max(1);
                min_remaining = Some(min_remaining.map_or(secs, |prev| prev.min(secs)));
            }
            _ => return None, // almeno un provider disponibile
        }
    }
    min_remaining
}
