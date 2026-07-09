//! Cooldown e circuit breaker per provider LLM.
//!
//! Estratto da `agent_loop.rs` durante la Fase 4 del refactor Nexus: i symbol
//! `is_provider_in_cooldown`, `put_provider_in_cooldown` e
//! `reset_provider_failures` sono usati anche fuori dal loop agente
//! (es. `orchestrator.rs`).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};

/// Tempi di health/cooldown provider, DB-driven (regola G). Inizializzati da
/// `main.rs` all'avvio leggendo la tabella `settings` (chiavi `provider.*`,
/// migrazioni 0253/0255). Se `init_provider_health_timings` non viene chiamato (es.
/// nei test unitari), si usano i default storici qui sotto — cosi' il modulo
/// resta utilizzabile senza dipendere dal DB.
#[derive(Debug, Clone, Copy)]
pub struct ProviderHealthTimings {
    pub cooldown_default_s: u64,
    pub cooldown_min_s: u64,
    pub cooldown_max_s: u64,
    pub cooldown_long_s: u64,
    pub circuit_breaker_window_s: u64,
    pub circuit_breaker_threshold: usize,
    pub circuit_breaker_extended_cooldown_s: u64,
    pub health_probe_timeout_s: u64,
    pub slow_cooldown_s: u64,
    pub outage_threshold: usize,
    pub billing_recovery_interval_s: u64,
    pub recovery_probe_timeout_s: u64,
    /// GOVERNANCE: TTL adattivo del cooldown LUNGO (billing) per TIPO d'errore
    /// (`agent.governance.cooldown_adaptive_ttl`, regola G, default OFF). Con OFF il
    /// cooldown lungo resta `cooldown_long_s` (bit-identico). NON tocca il
    /// circuit-breaker ne' la fallback-chain (reti di sicurezza FISSE).
    pub adaptive_billing_cooldown_enabled: bool,
    /// TTL ridotto (s) usato dal cooldown adattivo per gli errori billing a recupero
    /// periodico PREVEDIBILE (quota/rate), quando il flag e' ON
    /// (`agent.governance.cooldown_adaptive_ttl_min_s`). Clampato in
    /// `[cooldown_min_s, cooldown_long_s]`. Gli errori hard (credit/balance/payment,
    /// ricarica manuale imprevedibile) restano a `cooldown_long_s`.
    pub adaptive_billing_cooldown_min_s: u64,
}

impl Default for ProviderHealthTimings {
    fn default() -> Self {
        Self {
            cooldown_default_s: 300,
            cooldown_min_s: 10,
            cooldown_max_s: 3600,
            cooldown_long_s: 6 * 3600,
            circuit_breaker_window_s: 60,
            circuit_breaker_threshold: 3,
            circuit_breaker_extended_cooldown_s: 600,
            health_probe_timeout_s: 30,
            slow_cooldown_s: 60,
            outage_threshold: 3,
            billing_recovery_interval_s: 60,
            recovery_probe_timeout_s: 30,
            // Governance TTL adattivo OFF di default (opt-in): con OFF il cooldown
            // lungo resta 6h (bit-identico). Min ridotto storico-neutro: 2h.
            adaptive_billing_cooldown_enabled: false,
            adaptive_billing_cooldown_min_s: 2 * 3600,
        }
    }
}

/// TTL adattivo del cooldown LUNGO (billing) per TIPO d'errore, derivato dalla
/// `reason` STRUTTURATA (regola M: la reason e' la categoria d'errore, non prosa da
/// classificare). Funzione PURA (testabile senza stato globale). Opt-in
/// (`adaptive_billing_cooldown_enabled`): con OFF ritorna sempre `cooldown_long_s`
/// -> comportamento bit-identico. NON tocca il circuit-breaker ne' la fallback-chain
/// (reti di sicurezza FISSE, per vincolo del task).
///
/// Il re-probe periodico (`BILLING_REPROBE_INTERVAL_S`) recupera comunque in
/// anticipo se il credito torna: il TTL e' solo il LIMITE SUPERIORE. La riduzione e'
/// quindi a basso rischio.
///   - hard billing (`credit`/`balance`/`payment`, ricarica MANUALE imprevedibile)
///     -> `cooldown_long_s` pieno (nessuna riduzione).
///   - quota/rate (recupero periodico PREVEDIBILE) -> `adaptive_billing_cooldown_min_s`
///     clampato in `[cooldown_min_s, cooldown_long_s]`.
///   - ogni altra reason -> `cooldown_long_s` (conservativo).
pub fn adaptive_billing_cooldown_secs(reason: &str, timings: &ProviderHealthTimings) -> u64 {
    if !timings.adaptive_billing_cooldown_enabled {
        return timings.cooldown_long_s;
    }
    let r = reason.to_ascii_lowercase();
    // Hard billing: ricarica manuale -> nessuna riduzione.
    if r.contains("credit") || r.contains("balance") || r.contains("payment") {
        return timings.cooldown_long_s;
    }
    // Quota/rate: recupero periodico prevedibile -> TTL ridotto (clampato).
    if r.contains("quota") || r.contains("rate") {
        // Clamp robusto: `cooldown_min_s`/`cooldown_long_s` vengono da settings DB
        // INDIPENDENTI, senza validazione reciproca. Se un operatore li invertisse
        // (min > long) `u64::clamp` panicherebbe (`assert!(min <= max)`) proprio sul
        // path di gestione errori billing (quando il sistema e' gia' in difficolta').
        // `min(min_s, long_s)` garantisce lower <= upper: config invertita -> degrada
        // al cooldown lungo pieno (conservativo), mai panic.
        return timings.adaptive_billing_cooldown_min_s.clamp(
            timings.cooldown_min_s.min(timings.cooldown_long_s),
            timings.cooldown_long_s,
        );
    }
    // Conservativo: qualunque altra causa resta al cooldown lungo pieno.
    timings.cooldown_long_s
}

static HEALTH_TIMINGS: OnceLock<ProviderHealthTimings> = OnceLock::new();

/// Inizializza i tempi health/cooldown dai settings DB. Idempotente: la prima
/// chiamata vince (OnceLock). Va invocata una sola volta all'avvio da main.rs.
pub fn init_provider_health_timings(timings: ProviderHealthTimings) {
    let _ = HEALTH_TIMINGS.set(timings);
}

/// Ritorna i tempi correnti (copia). Default storici se non ancora inizializzati.
pub fn provider_health_timings() -> ProviderHealthTimings {
    HEALTH_TIMINGS.get().copied().unwrap_or_default()
}

static PROVIDER_COOLDOWN: OnceLock<Mutex<HashMap<String, std::time::Instant>>> = OnceLock::new();

// -- Circuit breaker state --
// Traccia gli istanti dei fallimenti recenti per provider. Se la soglia di
// fallimenti viene superata entro la finestra, entriamo in stato OPEN con
// cooldown esteso (durate da `provider_health_timings()`).
static PROVIDER_FAILURES: OnceLock<Mutex<HashMap<String, Vec<std::time::Instant>>>> =
    OnceLock::new();

/// Intervallo di ri-probe per i provider ancora in cooldown BILLING: il credito
/// puo' tornare con una ricarica imprevedibile, quindi non si aspetta la scadenza
/// del cooldown lungo (6h) ma si riprova ogni 10 minuti.
const BILLING_REPROBE_INTERVAL_S: u64 = 600;

/// Ultimo istante in cui il recovery loop ha ri-probato un provider ancora in
/// cooldown. Limita la frequenza dei re-probe (vedi BILLING_REPROBE_INTERVAL_S),
/// cosi' il probe periodico non martella il provider ad ogni giro del loop.
static LAST_RECOVERY_PROBE: OnceLock<Mutex<HashMap<String, std::time::Instant>>> = OnceLock::new();

/// True se il provider in cooldown va ri-probato adesso (mai probato, o ultimo
/// probe piu' vecchio di `interval`); in tal caso aggiorna il timestamp.
fn should_reprobe_cooldown(provider: &str, interval: std::time::Duration) -> bool {
    let store = LAST_RECOVERY_PROBE.get_or_init(|| Mutex::new(HashMap::new()));
    match store.lock() {
        Ok(mut map) => {
            let now = std::time::Instant::now();
            let due = map
                .get(provider)
                .map(|last| now.duration_since(*last) >= interval)
                .unwrap_or(true);
            if due {
                map.insert(provider.to_string(), now);
            }
            due
        }
        Err(_) => false,
    }
}

pub fn is_provider_in_cooldown(provider: &str) -> bool {
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(map) = store.lock() {
        if let Some(&until) = map.get(&provider.to_lowercase()) {
            return std::time::Instant::now() < until;
        }
    }
    false
}

/// True se la `reason` di un cooldown indica credito/quota esaurito (billing),
/// non un errore transiente (rate-limit/timeout/rete). Punto unico (regola L)
/// della classificazione billing di una reason di cooldown: stessa semantica
/// della nota di esaurimento mostrata all'utente.
pub fn reason_is_billing(reason: &str) -> bool {
    let r = reason.to_lowercase();
    r.contains("credit") || r.contains("quota") || r.contains("billing") || r.contains("balance")
}

/// True se il provider e' ATTUALMENTE in cooldown e la causa e' billing
/// (credito/quota esaurito). Il re-probe dei provider in billing cooldown e'
/// gestito ESCLUSIVAMENTE dal loop dedicato `billing_cooldown_recovery_loop`
/// (re-probe a `BILLING_REPROBE_INTERVAL_S`): il probe periodico generico
/// (`run_one_round`) li salta, cosi' non rinnova il cooldown lungo ne' martella
/// il gateway con 500 a cascata (incidente Beauty-Book). I cooldown transient
/// restano invece pingati dal probe periodico per il recovery rapido.
pub fn is_provider_in_billing_cooldown(provider: &str) -> bool {
    if !is_provider_in_cooldown(provider) {
        return false;
    }
    let key = provider.to_lowercase();
    PROVIDER_COOLDOWN_REASONS
        .get()
        .and_then(|s| s.lock().ok())
        .and_then(|m| m.get(&key).cloned())
        .map(|r| reason_is_billing(&r))
        .unwrap_or(false)
}

/// Registra un fallimento per il provider e restituisce true se la soglia
/// del circuit breaker e' stata superata (3+ fallimenti in 60s).
fn record_provider_failure(provider: &str) -> bool {
    let store = PROVIDER_FAILURES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        let t = provider_health_timings();
        let now = std::time::Instant::now();
        let window = std::time::Duration::from_secs(t.circuit_breaker_window_s);
        let entry = map.entry(provider.to_lowercase()).or_insert_with(Vec::new);
        entry.retain(|&ts| now.duration_since(ts) < window);
        entry.push(now);
        entry.len() >= t.circuit_breaker_threshold
    } else {
        false
    }
}

/// Reset del contatore fallimenti (chiamare su successo = stato CLOSED).

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
    // Azzera il TTL persistente su nexus_provider_health (fonte GIUSTA del
    // billing cooldown: ha scadenza). Cosi' un restart non lo ripristina.
    if let Some(pool) = DB_POOL.get() {
        let pool = pool.clone();
        let key = key.clone();
        tokio::spawn(async move {
            if let Err(e) = sqlx::query(
                "UPDATE nexus_provider_health \
                 SET billing_cooldown_until = NULL, updated_at = NOW() \
                 WHERE LOWER(provider) = $1 AND billing_cooldown_until IS NOT NULL",
            )
            .bind(&key)
            .execute(&pool)
            .await
            {
                tracing::warn!(
                    "remove_cooldown: clear TTL nexus_provider_health fallito per '{}': {}",
                    key,
                    e
                );
            }
        });
    }
    tracing::info!(
        "Provider '{}' cooldown rimosso manualmente (admin)",
        provider
    );
}

/// Mette un provider in cooldown. Se `retry_after_seconds` e' fornito dal
/// provider (header Retry-After), lo usa con un cap a [10s, 3600s]. Altrimenti
/// default 300s. Se il circuit breaker scatta, cooldown esteso a 600s.
pub(crate) fn put_provider_in_cooldown(provider: &str, retry_after_seconds: Option<u64>) {
    let t = provider_health_timings();
    let breaker_tripped = record_provider_failure(provider);
    let base_secs = retry_after_seconds
        .map(|s| s.clamp(t.cooldown_min_s, t.cooldown_max_s))
        .unwrap_or(t.cooldown_default_s);
    let secs = if breaker_tripped {
        base_secs.max(t.circuit_breaker_extended_cooldown_s)
    } else {
        base_secs
    };
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        if breaker_tripped {
            tracing::warn!(
                "Provider '{}' circuit breaker OPEN: cooldown esteso {}s (>= {} fallimenti in {}s)",
                provider,
                secs,
                t.circuit_breaker_threshold,
                t.circuit_breaker_window_s
            );
        } else {
            tracing::warn!(
                "Provider '{}' in cooldown per {}s (retry_after={:?})",
                provider,
                secs,
                retry_after_seconds
            );
        }
        map.insert(provider.to_lowercase(), until);
    }
}

/// Connessione Redis globale per persistere il cooldown lungo (sopravvive
/// al riavvio di mcp-core). Inizializzata da `main.rs::main` dopo init_redis.
static REDIS_CLIENT: OnceLock<redis::aio::MultiplexedConnection> = OnceLock::new();

/// Inizializza il client Redis globale per la persistenza dei cooldown
/// lunghi. Chiamato una volta da `main.rs` dopo `cache::init_redis`.
/// Il client e' clonabile (è un Arc internamente) — ne salviamo un clone.
pub fn init_redis_client(client: redis::aio::MultiplexedConnection) {
    let _ = REDIS_CLIENT.set(client);
}

/// Pool DB globale per la persistenza TTL del billing cooldown su
/// `nexus_provider_health` (la fonte persistente GIUSTA: ha scadenza, non
/// disabilita il catalog). Inizializzato da `main.rs` all'avvio.
static DB_POOL: OnceLock<sqlx::PgPool> = OnceLock::new();

/// Espone il pool DB a `provider_cooldown` per l'UPSERT/clear del TTL
/// billing su `nexus_provider_health`. Idempotente (OnceLock).
pub fn init_db_pool(pool: sqlx::PgPool) {
    let _ = DB_POOL.set(pool);
}

/// Worker periodico: ogni `interval_secs` controlla i provider che hanno righe
/// ancora disabilitate dal billing cooldown e, se il cooldown locale e' scaduto,
/// li riabilita nel DB **solo dopo un probe attivo andato a buon fine**
/// (probe-then-reenable).
///
/// Prima del fix, la riabilitazione avveniva alla cieca allo scadere del timer:
/// se il billing non era stato ricaricato, il provider tornava attivo, veniva
/// scelto per un run reale, falliva di nuovo e rientrava in cooldown (ciclo).
/// Ora, allo scadere del cooldown:
///   - probe Healthy   -> riabilita (catalog + matrix).
///   - probe Billing    -> il credito e' ancora KO: rinnova il cooldown lungo,
///                         niente riabilitazione.
///   - probe Transient  -> errore non conclusivo (rate-limit/timeout/rete):
///                         applica un cooldown breve e riprova al prossimo giro.
///
/// `interval_secs` e il timeout del probe sono DB-driven (settings `provider.*`,
/// migrazione 0252) — vedi `provider_health_timings()`.
pub async fn billing_cooldown_recovery_loop(
    orchestrator: Arc<crate::orchestrator::Orchestrator>,
    db: sqlx::PgPool,
    interval_secs: u64,
) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(5)));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await; // skip immediate

    loop {
        ticker.tick().await;

        let providers_disabled: Vec<String> = match sqlx::query_scalar::<_, String>(
            "SELECT LOWER(provider) FROM nexus_provider_health \
             WHERE billing_cooldown_until IS NOT NULL",
        )
        .fetch_all(&db)
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("billing_cooldown_recovery: query fallita: {}", e);
                continue;
            }
        };

        for provider in providers_disabled {
            // Cooldown ancora attivo: di norma si aspetta la scadenza. MA per i
            // billing-cooldown (6h) la ricarica del credito e' un evento esterno
            // imprevedibile: ri-proviamo a intervalli regolari (BILLING_REPROBE_
            // INTERVAL_S) invece di tenere il provider giu' per ore dopo che
            // l'utente ha gia' ricaricato.
            if is_provider_in_cooldown(&provider)
                && !should_reprobe_cooldown(
                    &provider,
                    std::time::Duration::from_secs(BILLING_REPROBE_INTERVAL_S),
                )
            {
                continue;
            }
            // Probe-then-reenable: il cooldown e' scaduto (o e' ora di ri-provare)
            // ma prima di riabilitare accertiamo che il provider sia DAVVERO
            // tornato operativo.
            let probe_timeout = provider_health_timings().recovery_probe_timeout_s;
            match crate::provider_health_probe::probe_provider_once(
                &orchestrator,
                &provider,
                probe_timeout,
            )
            .await
            {
                crate::provider_health_probe::ProbeOutcome::Healthy => {
                    tracing::info!(
                        target: "provider_cooldown",
                        provider = %provider,
                        "probe-then-reenable: provider sano, esco dal cooldown e riabilito nel DB"
                    );
                    // Esce SUBITO dal cooldown (anche se non ancora scaduto): il
                    // re-probe periodico ha rilevato che il credito e' tornato.
                    // remove_cooldown azzera anche il TTL su nexus_provider_health.
                    remove_cooldown(&provider);
                }
                crate::provider_health_probe::ProbeOutcome::Billing(kind) => {
                    tracing::warn!(
                        target: "provider_cooldown",
                        provider = %provider,
                        kind = %kind,
                        "probe-then-reenable: billing ancora KO, rinnovo cooldown lungo (niente riabilitazione)"
                    );
                    put_provider_in_long_cooldown(&provider, &kind);
                }
                crate::provider_health_probe::ProbeOutcome::Transient(kind) => {
                    let slow = provider_health_timings().slow_cooldown_s;
                    tracing::warn!(
                        target: "provider_cooldown",
                        provider = %provider,
                        kind = %kind,
                        "probe-then-reenable: esito non conclusivo, cooldown breve e nuovo tentativo al prossimo giro"
                    );
                    put_provider_in_short_cooldown(&provider, &kind, slow);
                }
            }
        }
    }
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
    // TTL adattivo per tipo d'errore (governance, opt-in): con flag OFF ritorna
    // `cooldown_long_s` (6h, bit-identico). Il re-probe periodico recupera comunque
    // in anticipo; il TTL e' solo il limite superiore.
    let long_secs = adaptive_billing_cooldown_secs(reason, &provider_health_timings());
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(long_secs);
        map.insert(provider.to_lowercase(), until);
        tracing::warn!(
            "Provider '{}' in COOLDOWN LUNGO ({}s, {} ore) per: {}",
            provider,
            long_secs,
            long_secs / 3600,
            reason,
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
            let until_ts = now_ts.saturating_add(long_secs);
            let key = format!("nexus:billing_cooldown:{}", provider);
            let value = format!("{}|{}", until_ts, reason);
            let res: Result<(), _> = redis::cmd("SET")
                .arg(&key)
                .arg(&value)
                .arg("EX")
                .arg(long_secs + 60)
                .query_async(&mut conn)
                .await;
            match &res {
                Ok(()) => tracing::info!(
                    "put_provider_in_long_cooldown: Redis SET ok per '{}' (chiave={})",
                    provider,
                    key,
                ),
                Err(e) => tracing::warn!(
                    "put_provider_in_long_cooldown: persistenza Redis fallita per '{}': {}",
                    provider,
                    e,
                ),
            }
        });
    } else {
        tracing::warn!(
            "put_provider_in_long_cooldown: REDIS_CLIENT non inizializzato, cooldown solo in-memory per '{}'",
            provider,
        );
    }
    // Persistenza TTL su nexus_provider_health (fonte PERSISTENTE GIUSTA del
    // billing cooldown: ha scadenza, non disabilita il catalog). E' la parte
    // BUONA spostata qui dall'ex propagate_billing_disable_to_db: writer unico
    // Rust (regola L/ADR 0020), letta al boot da restore_billing_cooldowns_from_db.
    // TTL = cooldown lungo DB-driven (cooldown_long_s, gia' in long_secs).
    if let Some(pool) = DB_POOL.get() {
        let pool = pool.clone();
        let provider = provider.to_lowercase();
        let reason = reason.to_string();
        let ttl_secs = (long_secs as i64).to_string();
        tokio::spawn(async move {
            if let Err(e) = sqlx::query(
                "INSERT INTO nexus_provider_health \
                   (provider, billing_cooldown_until, last_error, last_error_at, last_error_source, updated_at) \
                 VALUES ($1, NOW() + ($2 || ' seconds')::interval, $3, NOW(), 'mcp-core', NOW()) \
                 ON CONFLICT (provider) DO UPDATE SET \
                   billing_cooldown_until = EXCLUDED.billing_cooldown_until, \
                   last_error = EXCLUDED.last_error, last_error_at = NOW(), \
                   last_error_source = 'mcp-core', updated_at = NOW()",
            )
            .bind(&provider)
            .bind(&ttl_secs)
            .bind(&reason)
            .execute(&pool)
            .await
            {
                tracing::warn!(
                    "put_provider_in_long_cooldown: UPSERT TTL nexus_provider_health fallito per '{}': {}",
                    provider,
                    e
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
            provider,
            duration_secs,
            reason
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
    let map = match store.lock() {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
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
            provider,
            remaining_secs,
            reason
        );
    }
    let reasons = PROVIDER_COOLDOWN_REASONS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = reasons.lock() {
        map.insert(provider.to_lowercase(), reason.to_string());
    }
}

/// Bootstrap del cooldown billing dal DB persistente al riavvio (ADR 0020).
///
/// Principio: il polling di health e' l'UNICO che testa i provider; un run di
/// produzione deve CONSULTARE lo stato gia' accertato, non scoprirlo chiamando
/// il provider. Lo store del gate e' pero' in-memory e si azzera ad ogni restart
/// di mcp-core. Il `restore_cooldown` da Redis (main.rs) copre il caso comune ma
/// fallisce se Redis e' stato svuotato/riavviato. `nexus_provider_health`
/// (scritta dal brain via cooldown_bridge e dal gate) e' la fonte PERSISTENTE
/// piu' affidabile: la leggiamo al boot e rimettiamo i provider esausti in
/// cooldown lungo, cosi' il PRIMO run dopo un restart li salta senza ri-testarli
/// (era la causa del loop "anthropic 400 / openai 429 ad ogni turno").
///
/// I provider il cui credito e' stato nel frattempo ricaricato vengono riabilitati
/// dal `billing_cooldown_recovery_loop` (probe-then-reenable) al primo giro.
pub async fn restore_billing_cooldowns_from_db(db: &sqlx::PgPool) {
    let rows = match sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT LOWER(provider), last_error \
         FROM nexus_provider_health \
         WHERE billing_cooldown_until IS NOT NULL AND billing_cooldown_until > NOW()",
    )
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("restore_billing_cooldowns_from_db: query fallita: {e}");
            return;
        }
    };
    let n = rows.len();
    for (provider, reason) in rows {
        // Billing/quota e' persistente -> cooldown lungo (6h). Il recovery loop
        // (probe-then-reenable) lo rimuovera' appena il provider torna 200.
        put_provider_in_long_cooldown(
            &provider,
            reason
                .as_deref()
                .unwrap_or("billing_cooldown (ripristino DB al boot)"),
        );
    }
    if n > 0 {
        tracing::info!(
            "restore_billing_cooldowns_from_db: {n} provider in cooldown billing ripristinati dal DB al boot (gate allineato allo stato persistente)"
        );
    }
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
    fn reason_is_billing_classifica_credito_quota() {
        assert!(reason_is_billing("credit_balance_too_low"));
        assert!(reason_is_billing("quota_exceeded"));
        assert!(reason_is_billing("insufficient balance"));
        assert!(reason_is_billing("BILLING required"));
        assert!(!reason_is_billing("rate_limit"));
        assert!(!reason_is_billing("timeout"));
        assert!(!reason_is_billing("connection_error"));
    }

    #[test]
    fn billing_cooldown_distinto_da_transient() {
        // Il probe periodico salta i billing (gestiti dal recovery loop) ma
        // continua a pingare i transient. is_provider_in_billing_cooldown e'
        // il discriminante.
        let p_bill = "__test_billing_cd_probe";
        let p_trans = "__test_transient_cd_probe";
        assert!(!is_provider_in_billing_cooldown(p_bill));
        restore_cooldown(p_bill, 3600, "credit_balance_too_low");
        assert!(is_provider_in_billing_cooldown(p_bill));
        restore_cooldown(p_trans, 60, "rate_limit");
        assert!(is_provider_in_cooldown(p_trans));
        assert!(!is_provider_in_billing_cooldown(p_trans));
        remove_cooldown(p_bill);
        remove_cooldown(p_trans);
        assert!(!is_provider_in_billing_cooldown(p_bill));
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
            assert!(*secs <= 3600, "cap superiore violato: {}s > 3600s", secs);
        }
    }

    #[test]
    fn timings_default_riflettono_i_valori_storici() {
        // I default DB-driven devono coincidere con le vecchie costanti
        // hardcoded, cosi' un setting mancante non cambia il comportamento.
        let t = ProviderHealthTimings::default();
        assert_eq!(t.cooldown_default_s, 300);
        assert_eq!(t.cooldown_min_s, 10);
        assert_eq!(t.cooldown_max_s, 3600);
        assert_eq!(t.cooldown_long_s, 6 * 3600);
        assert_eq!(t.circuit_breaker_window_s, 60);
        assert_eq!(t.circuit_breaker_threshold, 3);
        assert_eq!(t.circuit_breaker_extended_cooldown_s, 600);
        assert_eq!(t.health_probe_timeout_s, 30);
        assert_eq!(t.slow_cooldown_s, 60);
        assert_eq!(t.outage_threshold, 3);
        assert_eq!(t.billing_recovery_interval_s, 60);
        assert_eq!(t.recovery_probe_timeout_s, 30);
    }

    #[test]
    fn adaptive_ttl_off_ritorna_sempre_cooldown_lungo() {
        // Flag OFF (default): qualunque reason -> cooldown lungo pieno (bit-identico).
        let t = ProviderHealthTimings::default();
        assert!(!t.adaptive_billing_cooldown_enabled);
        assert_eq!(
            adaptive_billing_cooldown_secs("credit balance too low", &t),
            t.cooldown_long_s
        );
        assert_eq!(
            adaptive_billing_cooldown_secs("quota_exceeded", &t),
            t.cooldown_long_s
        );
    }

    #[test]
    fn adaptive_ttl_on_riduce_solo_quota_e_rate() {
        let t = ProviderHealthTimings {
            adaptive_billing_cooldown_enabled: true,
            adaptive_billing_cooldown_min_s: 2 * 3600,
            ..Default::default()
        };
        // Hard billing (credit/balance/payment): ricarica manuale -> nessuna riduzione.
        assert_eq!(
            adaptive_billing_cooldown_secs("credit balance too low", &t),
            t.cooldown_long_s
        );
        assert_eq!(
            adaptive_billing_cooldown_secs("payment required", &t),
            t.cooldown_long_s
        );
        // Quota/rate: recupero prevedibile -> TTL ridotto (2h).
        assert_eq!(
            adaptive_billing_cooldown_secs("quota_exceeded", &t),
            2 * 3600
        );
        assert_eq!(adaptive_billing_cooldown_secs("rate_limit", &t), 2 * 3600);
        // Altra causa: conservativo -> cooldown lungo pieno.
        assert_eq!(adaptive_billing_cooldown_secs("boh", &t), t.cooldown_long_s);
    }

    #[test]
    fn adaptive_ttl_min_clampato_nel_range() {
        let base = ProviderHealthTimings {
            adaptive_billing_cooldown_enabled: true,
            ..Default::default()
        };
        // Min sotto cooldown_min_s (10) -> clampato a cooldown_min_s.
        let t = ProviderHealthTimings {
            adaptive_billing_cooldown_min_s: 1,
            ..base
        };
        assert_eq!(
            adaptive_billing_cooldown_secs("quota", &t),
            t.cooldown_min_s
        );
        // Min sopra cooldown_long_s -> clampato a cooldown_long_s.
        let t2 = ProviderHealthTimings {
            adaptive_billing_cooldown_min_s: 99 * 3600,
            ..base
        };
        assert_eq!(
            adaptive_billing_cooldown_secs("quota", &t2),
            t2.cooldown_long_s
        );
    }

    #[test]
    fn provider_health_timings_ritorna_default_se_non_inizializzato() {
        // Senza init (caso test/avvio precoce) si usano i default, mai panico.
        let t = provider_health_timings();
        assert_eq!(t.cooldown_long_s, 6 * 3600);
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
