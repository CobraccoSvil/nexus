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

/// Rimuove completamente il cooldown e il contatore failures per un provider.
/// Usato dall'endpoint admin per forzare il rientro in servizio di un provider.
pub fn remove_cooldown(provider: &str) {
    let key = provider.to_lowercase();
    // Rimuovi cooldown timer
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        map.remove(&key);
    }
    // Rimuovi contatore failures (circuit breaker)
    reset_provider_failures(provider);
    // Rimuovi reason
    let reasons = PROVIDER_COOLDOWN_REASONS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = reasons.lock() {
        map.remove(&key);
    }
    // Rimuovi da Redis (se persistito)
    if let Some(conn) = REDIS_CLIENT.get() {
        let mut conn = conn.clone();
        let redis_key = format!("nexus:billing_cooldown:{}", key);
        tokio::spawn(async move {
            let _: Result<(), _> = redis::cmd("DEL")
                .arg(&redis_key)
                .query_async(&mut conn)
                .await;
        });
    }
    tracing::info!("Provider '{}' cooldown rimosso manualmente (admin)", provider);
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
        tracing::info!(
            "put_provider_in_long_cooldown: avvio persistenza Redis per '{}'",
            provider,
        );
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
            match &res {
                Ok(()) => tracing::info!(
                    "put_provider_in_long_cooldown: Redis SET ok per '{}' (chiave={})",
                    provider, key,
                ),
                Err(e) => tracing::warn!(
                    "put_provider_in_long_cooldown: persistenza Redis fallita per '{}': {}",
                    provider, e,
                ),
            }
        });
    } else {
        tracing::warn!(
            "put_provider_in_long_cooldown: REDIS_CLIENT non inizializzato, cooldown solo in-memory per '{}'",
            provider,
        );
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

// =====================================================================
// TEST SCALABILITA' COOLDOWN PROVIDER
// =====================================================================
// NOTA: provider_cooldown usa stato globale (OnceLock<Mutex<HashMap>>).
// I test usano nomi provider univoci con prefisso `__test_<funzione>_`
// per evitare interferenze quando eseguiti in parallelo.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_sconosciuto_non_e_in_cooldown() {
        // Stato iniziale pulito: un provider mai messo in cooldown
        // ritorna false. Garanzia base del sistema.
        assert!(!is_provider_in_cooldown("__test_unknown_xyzzy"));
    }

    #[test]
    fn short_cooldown_marca_provider_indisponibile() {
        let p = "__test_short_cooldown_provider";
        assert!(!is_provider_in_cooldown(p));
        put_provider_in_short_cooldown(p, "test rate limit", 60);
        assert!(
            is_provider_in_cooldown(p),
            "dopo short cooldown il provider deve essere in cooldown"
        );
    }

    #[test]
    fn long_cooldown_marca_provider_indisponibile_per_ore() {
        // Caso billing_error: cooldown deve durare almeno 6h (test sull'effetto,
        // non sulla durata esatta — verificarla richiederebbe sleep).
        let p = "__test_long_cooldown_billing";
        put_provider_in_long_cooldown(p, "credit balance too low");
        assert!(is_provider_in_cooldown(p));
        // Snapshot deve contenere il provider con remaining > 0
        let snap = cooldown_snapshot();
        let found = snap.iter().find(|(name, _, _)| name == p);
        assert!(
            found.is_some(),
            "long cooldown deve apparire in cooldown_snapshot"
        );
        let (_, secs, reason) = found.unwrap();
        assert!(
            *secs > 5 * 3600,
            "long cooldown deve durare >= 5h, trovato {}s",
            secs
        );
        assert_eq!(reason.as_deref(), Some("credit balance too low"));
    }

    #[test]
    fn cooldown_e_case_insensitive_sul_nome_provider() {
        // Defense in depth: put('OpenAI') deve coincidere con is_in_cooldown('openai').
        // Tutti i call site del codice usano lowercase, ma vogliamo essere robusti.
        let p_upper = "__TEST_CASE_INSENSITIVE";
        let p_lower = "__test_case_insensitive";
        put_provider_in_short_cooldown(p_upper, "test", 60);
        assert!(is_provider_in_cooldown(p_lower));
    }

    #[test]
    fn provider_diversi_hanno_cooldown_indipendenti() {
        // Caso pratico: openai in cooldown billing non deve impattare anthropic.
        let p_a = "__test_indep_alpha";
        let p_b = "__test_indep_beta";
        put_provider_in_short_cooldown(p_a, "rate limit", 60);
        assert!(is_provider_in_cooldown(p_a));
        assert!(!is_provider_in_cooldown(p_b));
    }

    #[test]
    fn restore_cooldown_da_redis_ripristina_stato() {
        // Simula riavvio mcp-core: il cooldown viene letto da Redis e
        // restore_cooldown lo ricarica in memoria.
        let p = "__test_restore_provider";
        assert!(!is_provider_in_cooldown(p));
        restore_cooldown(p, 3600, "billing_error from redis");
        assert!(is_provider_in_cooldown(p));
        let snap = cooldown_snapshot();
        let entry = snap.iter().find(|(name, _, _)| name == p);
        assert!(entry.is_some());
        assert_eq!(
            entry.unwrap().2.as_deref(),
            Some("billing_error from redis")
        );
    }

    #[test]
    fn all_providers_in_cooldown_rileva_correttamente() {
        // Scenario chiave per lo scaling: se TUTTI i provider sono down,
        // il caller deve sapere per quanto attendere prima di ritentare.
        let providers = vec![
            "__test_all_cd_p1".to_string(),
            "__test_all_cd_p2".to_string(),
            "__test_all_cd_p3".to_string(),
        ];
        for p in &providers {
            put_provider_in_short_cooldown(p, "rate limit", 120);
        }
        let remaining = all_providers_in_cooldown(&providers);
        assert!(remaining.is_some());
        let secs = remaining.unwrap();
        assert!(
            secs > 0 && secs <= 120,
            "remaining {}s fuori range [1, 120]",
            secs
        );
    }

    #[test]
    fn all_providers_in_cooldown_ritorna_none_se_almeno_uno_disponibile() {
        // Caso frequente: scaling intra-provider deve poter procedere se almeno
        // un provider della gerarchia e' fuori cooldown.
        let providers = vec![
            "__test_partial_cd_p1".to_string(),
            "__test_partial_cd_p2_AVAILABLE".to_string(),
        ];
        put_provider_in_short_cooldown(&providers[0], "rate limit", 60);
        // providers[1] NON viene messo in cooldown
        let result = all_providers_in_cooldown(&providers);
        assert_eq!(
            result, None,
            "se almeno un provider e' libero, all_providers_in_cooldown deve essere None"
        );
    }

    #[test]
    fn all_providers_in_cooldown_lista_vuota_ritorna_none() {
        let result = all_providers_in_cooldown(&[]);
        assert_eq!(result, None);
    }

    #[test]
    fn cooldown_snapshot_esclude_provider_non_in_cooldown() {
        // Nessun provider mai messo in cooldown → snapshot vuoto (al netto di
        // altri test paralleli). Verifichiamo solo che __test_snap_excluded
        // NON appaia perche' non e' mai stato messo in cooldown.
        let p_never = "__test_snap_never_in_cooldown";
        let snap = cooldown_snapshot();
        let found = snap.iter().find(|(name, _, _)| name == p_never);
        assert!(
            found.is_none(),
            "provider mai messo in cooldown non deve apparire in snapshot"
        );
    }

    #[test]
    fn cap_retry_after_clampato_dentro_intervallo_valido() {
        // put_provider_in_cooldown clampa retry_after in [10, 3600].
        // Test sul comportamento (cooldown attivo) — non sulla durata esatta.
        let p_lo = "__test_clamp_low";
        let p_hi = "__test_clamp_high";
        // 5s sotto il minimo (10s): deve essere clampato a 10s, cooldown attivo
        put_provider_in_cooldown(p_lo, Some(5));
        assert!(is_provider_in_cooldown(p_lo));
        // 99999s sopra il massimo (3600s): clampato a 3600s, cooldown attivo
        put_provider_in_cooldown(p_hi, Some(99999));
        assert!(is_provider_in_cooldown(p_hi));
        // Verifica via snapshot che il cap superiore sia rispettato
        let snap = cooldown_snapshot();
        let entry_hi = snap.iter().find(|(name, _, _)| name == p_hi);
        if let Some((_, secs, _)) = entry_hi {
            assert!(
                *secs <= 3600,
                "cap superiore violato: {}s > 3600s",
                secs
            );
        }
    }

    #[test]
    fn reset_provider_failures_pulisce_contatore_circuit_breaker() {
        let p = "__test_reset_failures_cb";
        // Triggera il circuit breaker con 3 fallimenti
        for _ in 0..3 {
            put_provider_in_cooldown(p, Some(60));
        }
        assert!(is_provider_in_cooldown(p));
        // Reset del contatore: il prossimo put non scatena cooldown esteso
        reset_provider_failures(p);
        // Il cooldown attivo rimane finche' non scade naturalmente, ma il
        // contatore failures e' stato resettato (verifica indiretta: assenza panic)
    }
}
