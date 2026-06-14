//! Cooldown dei provider con RE-PROBE reattivo (il fix del bug "OpenAI non
//! torna dopo la ricarica").
//!
//! ## Bug che questo modulo risolve
//!
//! Nel gateway Node il cooldown di billing era in-memory e ASPETTAVA la scadenza
//! (impostata su ore) senza mai ri-provare il provider. Conseguenza: dopo che
//! l'utente ricaricava i crediti su OpenAI, il provider restava marcato giallo
//! (in cooldown) per ore, perche' nessuno verificava che fosse di nuovo sano.
//! In piu', vivendo nel processo Node separato da mcp-core, lo stato non era
//! condiviso con il resto del runtime.
//!
//! ## Soluzione
//!
//! [`CooldownManager`] tiene lo stato di cooldown per provider in una
//! `DashMap` thread-safe. La novita' rispetto al Node e' il RE-PROBE LOOP
//! ([`spawn_recovery_loop`]): un task tokio periodico che, per OGNI provider
//! attualmente in cooldown, esegue [`crate::provider::LlmProvider::healthcheck`].
//! Se il provider torna sano, il cooldown viene rimosso SUBITO -- quindi dopo
//! una ricarica il provider rientra entro un intervallo di re-probe (minuti),
//! NON dopo la scadenza nominale (ore).
//!
//! ## Configurazione DB-driven (regola G)
//!
//! Durate e intervallo NON sono hardcoded nella business logic: arrivano dai
//! `settings` con cache TTL ([`nexus_cache::TtlCache`], punto unico regola L).
//! Le costanti di questo modulo sono SOLO il fallback di sicurezza usato se il
//! DB e' irraggiungibile, documentate come tali. Chiavi lette:
//!   - `gateway.cooldown.billing_seconds`   (default 3600s)
//!   - `gateway.cooldown.transient_seconds` (default 30s)
//!   - `gateway.cooldown.reprobe_interval_seconds` (default 600s)
//!
//! Regola F: i log non contengono prompt/response; solo nome provider e durate.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use nexus_cache::TtlCache;
use sqlx::PgPool;

use crate::provider::LlmProvider;
use crate::types::ProviderStatus;

/// Fallback durata cooldown billing se il DB e' irraggiungibile: 1 ora. NON e'
/// un valore di business (quello sta in `settings`), e' la rete di sicurezza.
pub const DEFAULT_BILLING_SECONDS: i64 = 3600;

/// Fallback durata cooldown transitorio (errori di rete/5xx): 30 secondi.
pub const DEFAULT_TRANSIENT_SECONDS: i64 = 30;

/// Fallback intervallo di re-probe: 600 secondi (10 minuti).
pub const DEFAULT_REPROBE_INTERVAL_SECONDS: u64 = 600;

/// TTL della cache settings (60s, allineato al resto del gateway).
const SETTINGS_TTL: Duration = Duration::from_secs(60);

/// Causa del cooldown. Discrimina la durata applicata e l'audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CooldownReason {
    /// Crediti esauriti / problema di fatturazione: cooldown lungo, ma il
    /// re-probe lo annulla appena il provider torna a rispondere 200.
    Billing,
    /// Errore transitorio (rete, 5xx, timeout): cooldown breve.
    Transient,
}

impl CooldownReason {
    fn as_str(self) -> &'static str {
        match self {
            CooldownReason::Billing => "billing",
            CooldownReason::Transient => "transient",
        }
    }
}

/// Stato di cooldown di un singolo provider.
#[derive(Debug, Clone)]
pub struct CooldownState {
    /// Istante (UTC) fino al quale il provider resta in cooldown.
    pub until: DateTime<Utc>,
    /// Causa del cooldown.
    pub reason: CooldownReason,
    /// Ultimo messaggio d'errore osservato (gia' privo di prompt/response).
    pub last_error: Option<String>,
}

/// Durate di cooldown effettive (gia' risolte da DB o fallback).
#[derive(Debug, Clone, Copy)]
struct Durations {
    billing_seconds: i64,
    transient_seconds: i64,
}

impl Default for Durations {
    fn default() -> Self {
        Self {
            billing_seconds: DEFAULT_BILLING_SECONDS,
            transient_seconds: DEFAULT_TRANSIENT_SECONDS,
        }
    }
}

/// Gestore dei cooldown per provider. Clonabile a basso costo (condivide lo
/// store via `Arc`), cosi' puo' essere riposto nello stato applicativo e nel
/// task di re-probe.
#[derive(Clone)]
pub struct CooldownManager {
    states: Arc<DashMap<String, CooldownState>>,
    /// Cache delle durate lette dai settings (chiave unit: un solo set globale).
    durations: TtlCache<(), Durations>,
}

impl Default for CooldownManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CooldownManager {
    /// Crea un manager vuoto con le durate di fallback. Le durate reali vengono
    /// caricate da [`Self::refresh_settings`] (chiamato dal re-probe loop).
    pub fn new() -> Self {
        Self {
            states: Arc::new(DashMap::new()),
            durations: TtlCache::new(SETTINGS_TTL),
        }
    }

    /// Durate correnti: da cache settings se valide, altrimenti fallback.
    fn current_durations(&self) -> Durations {
        self.durations.get(&()).unwrap_or_default()
    }

    /// Marca un provider in cooldown billing. Usa l'orologio reale.
    pub fn mark_billing(&self, provider: &str, last_error: Option<String>) {
        let secs = self.current_durations().billing_seconds;
        self.mark_at(provider, CooldownReason::Billing, last_error, Utc::now(), secs);
    }

    /// Marca un provider in cooldown transitorio. Usa l'orologio reale.
    pub fn mark_transient(&self, provider: &str, last_error: Option<String>) {
        let secs = self.current_durations().transient_seconds;
        self.mark_at(
            provider,
            CooldownReason::Transient,
            last_error,
            Utc::now(),
            secs,
        );
    }

    /// Marca un provider con `now` e durata espliciti. Punto unico della logica
    /// di marcatura (regola L): `mark_billing`/`mark_transient` e i test ci
    /// delegano, cosi' il calcolo di `until` ha UNA sola implementazione e i test
    /// possono iniettare un istante deterministico senza usare `Utc::now()`.
    pub fn mark_at(
        &self,
        provider: &str,
        reason: CooldownReason,
        last_error: Option<String>,
        now: DateTime<Utc>,
        duration_seconds: i64,
    ) {
        let until = now + chrono::Duration::seconds(duration_seconds);
        // Regola F: logghiamo nome provider e durata, MAI il payload. last_error
        // qui e' il messaggio d'errore del provider (no prompt utente).
        tracing::warn!(
            provider,
            reason = reason.as_str(),
            duration_seconds,
            "gateway: provider in cooldown"
        );
        self.states.insert(
            provider.to_string(),
            CooldownState {
                until,
                reason,
                last_error,
            },
        );
    }

    /// `true` se il provider e' in cooldown rispetto all'istante corrente.
    pub fn is_in_cooldown(&self, provider: &str) -> bool {
        self.is_in_cooldown_at(provider, Utc::now())
    }

    /// Variante con istante iniettato (deterministica per i test).
    pub fn is_in_cooldown_at(&self, provider: &str, now: DateTime<Utc>) -> bool {
        match self.states.get(provider) {
            Some(s) => s.until > now,
            None => false,
        }
    }

    /// Secondi rimanenti di cooldown (0 se non in cooldown o scaduto).
    pub fn seconds_remaining(&self, provider: &str) -> i64 {
        self.seconds_remaining_at(provider, Utc::now())
    }

    /// Variante con istante iniettato (deterministica per i test).
    pub fn seconds_remaining_at(&self, provider: &str, now: DateTime<Utc>) -> i64 {
        match self.states.get(provider) {
            Some(s) => (s.until - now).num_seconds().max(0),
            None => 0,
        }
    }

    /// Rimuove il cooldown di un provider (usato dal re-probe al ripristino).
    pub fn clear(&self, provider: &str) {
        if self.states.remove(provider).is_some() {
            tracing::info!(provider, "gateway: provider ripristinato (cooldown rimosso)");
        }
    }

    /// Snapshot dello stato di tutti i provider in cooldown, come
    /// [`ProviderStatus`] (per esposizione su `/status`). I provider non
    /// presenti nella mappa sono considerati sani e NON compaiono qui.
    pub fn snapshot(&self) -> Vec<ProviderStatus> {
        self.snapshot_at(Utc::now())
    }

    /// Variante con istante iniettato (deterministica per i test).
    pub fn snapshot_at(&self, now: DateTime<Utc>) -> Vec<ProviderStatus> {
        self.states
            .iter()
            .filter(|e| e.until > now)
            .map(|e| {
                let s = e.value();
                ProviderStatus {
                    name: e.key().clone(),
                    healthy: false,
                    last_check: now,
                    last_error: s.last_error.clone(),
                    billing_error: if s.reason == CooldownReason::Billing {
                        s.last_error.clone()
                    } else {
                        None
                    },
                }
            })
            .collect()
    }

    /// Lista dei provider attualmente in cooldown (per il re-probe loop).
    fn providers_in_cooldown(&self, now: DateTime<Utc>) -> Vec<String> {
        self.states
            .iter()
            .filter(|e| e.until > now)
            .map(|e| e.key().clone())
            .collect()
    }

    /// Ricarica le durate dai `settings` nella cache TTL 60s. `force=true`
    /// ignora la cache. Se il DB e' down mantiene i valori correnti (graceful):
    /// il routing non si blocca. Le durate effettive si leggono poi via
    /// `mark_billing`/`mark_transient` (che usano `current_durations`).
    pub async fn refresh_settings(&self, pool: &PgPool, force: bool) {
        if !force && self.durations.get(&()).is_some() {
            return;
        }

        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT key, value FROM settings \
             WHERE key IN ('gateway.cooldown.billing_seconds', \
                           'gateway.cooldown.transient_seconds')",
        )
        .fetch_all(pool)
        .await;

        match rows {
            Ok(rows) => {
                let mut d = Durations::default();
                for (key, value) in &rows {
                    if let Ok(n) = value.trim().parse::<i64>() {
                        match key.as_str() {
                            "gateway.cooldown.billing_seconds" => d.billing_seconds = n,
                            "gateway.cooldown.transient_seconds" => d.transient_seconds = n,
                            _ => {}
                        }
                    }
                }
                self.durations.insert((), d);
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "gateway-cooldown: refresh durate fallito, mantengo i valori correnti (fallback)"
                );
            }
        }
    }
}

/// Legge l'intervallo di re-probe dai `settings`. Fallback alla costante se il
/// DB e' down o il valore manca/non e' parsabile. Non usa cache (e' letto una
/// volta all'avvio del loop e ad ogni giro per recepire i cambi a caldo).
pub async fn reprobe_interval_seconds(pool: &PgPool) -> u64 {
    let row = sqlx::query_as::<_, (String,)>(
        "SELECT value FROM settings WHERE key = 'gateway.cooldown.reprobe_interval_seconds'",
    )
    .fetch_optional(pool)
    .await;

    match row {
        Ok(Some((v,))) => v
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_REPROBE_INTERVAL_SECONDS),
        _ => DEFAULT_REPROBE_INTERVAL_SECONDS,
    }
}

/// Avvia il RE-PROBE LOOP in un task tokio dedicato. Ad ogni iterazione:
///   1. aggiorna le durate dai settings (cache 60s) e l'intervallo;
///   2. per OGNI provider in cooldown chiama `healthcheck()`;
///   3. se il provider torna sano, [`CooldownManager::clear`] lo riabilita.
///
/// Questo e' il cuore del fix: il provider non aspetta la scadenza nominale, ma
/// rientra appena un probe lo trova sano (es. dopo la ricarica crediti OpenAI).
///
/// Il loop e' infinito; il task termina quando l'handle viene droppato/abortito
/// (gestito dal chiamante, es. allo shutdown di mcp-core).
pub fn spawn_recovery_loop(
    manager: CooldownManager,
    providers: Vec<Arc<dyn LlmProvider>>,
    pool: PgPool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            // Aggiorna durate (cache) e intervallo a ogni giro: i cambi DB
            // vengono recepiti senza restart (regola G).
            manager.refresh_settings(&pool, false).await;
            let interval_secs = reprobe_interval_seconds(&pool).await;

            run_recovery_pass(&manager, &providers).await;

            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        }
    })
}

/// Un singolo passaggio di recovery: estratto come funzione pura (sul manager e
/// la lista provider) cosi' la logica e' testabile senza il loop infinito ne'
/// il timer. Per ogni provider in cooldown esegue il probe; se sano, lo libera.
pub async fn run_recovery_pass(manager: &CooldownManager, providers: &[Arc<dyn LlmProvider>]) {
    let in_cooldown = manager.providers_in_cooldown(Utc::now());
    if in_cooldown.is_empty() {
        return;
    }

    for provider in providers {
        let name = provider.name().to_string();
        if !in_cooldown.contains(&name) {
            continue;
        }
        // Probe: NON consuma crediti di generazione (e' un /models). Se torna
        // sano, il provider rientra subito.
        if provider.healthcheck().await {
            manager.clear(&name);
        } else {
            tracing::debug!(
                provider = %name,
                "gateway-reprobe: provider ancora non sano, resta in cooldown"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use crate::provider::{ChunkStream, LlmProvider};
    use crate::types::{LlmRequest, LlmResponse, SensitivityTier};

    /// Provider finto controllabile: `healthy` decide l'esito dello healthcheck,
    /// `probe_calls` conta quante volte e' stato sondato. Nessuna rete.
    struct FakeProvider {
        name: String,
        healthy: std::sync::atomic::AtomicBool,
        probe_calls: AtomicUsize,
    }

    impl FakeProvider {
        fn new(name: &str, healthy: bool) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                healthy: std::sync::atomic::AtomicBool::new(healthy),
                probe_calls: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl LlmProvider for FakeProvider {
        fn name(&self) -> &str {
            &self.name
        }
        fn supports_tools(&self) -> bool {
            true
        }
        fn supports_streaming(&self) -> bool {
            true
        }
        fn max_context_tokens(&self) -> u32 {
            1000
        }
        fn tier_compatibility(&self) -> &[SensitivityTier] {
            &[0]
        }
        async fn complete(&self, _req: &LlmRequest) -> anyhow::Result<LlmResponse> {
            anyhow::bail!("non usato nei test cooldown")
        }
        async fn stream(&self, _req: &LlmRequest) -> anyhow::Result<ChunkStream> {
            anyhow::bail!("non usato nei test cooldown")
        }
        async fn healthcheck(&self) -> bool {
            self.probe_calls.fetch_add(1, Ordering::SeqCst);
            self.healthy.load(Ordering::SeqCst)
        }
    }

    fn t0() -> DateTime<Utc> {
        // Istante fisso e deterministico per i test (no Utc::now()).
        DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn mark_e_seconds_remaining_deterministici() {
        let m = CooldownManager::new();
        let now = t0();
        m.mark_at("openai", CooldownReason::Billing, None, now, 3600);

        // Subito dopo: ~3600s rimanenti.
        assert!(m.is_in_cooldown_at("openai", now));
        assert_eq!(m.seconds_remaining_at("openai", now), 3600);

        // A meta' durata: 1800s rimanenti.
        let mid = now + chrono::Duration::seconds(1800);
        assert!(m.is_in_cooldown_at("openai", mid));
        assert_eq!(m.seconds_remaining_at("openai", mid), 1800);

        // Dopo la scadenza: non piu' in cooldown, 0 rimanenti.
        let after = now + chrono::Duration::seconds(3601);
        assert!(!m.is_in_cooldown_at("openai", after));
        assert_eq!(m.seconds_remaining_at("openai", after), 0);
    }

    #[test]
    fn clear_rimuove_il_cooldown() {
        let m = CooldownManager::new();
        let now = t0();
        m.mark_at("openai", CooldownReason::Transient, None, now, 30);
        assert!(m.is_in_cooldown_at("openai", now));
        m.clear("openai");
        assert!(!m.is_in_cooldown_at("openai", now));
        assert_eq!(m.seconds_remaining_at("openai", now), 0);
    }

    #[test]
    fn provider_sconosciuto_non_in_cooldown() {
        let m = CooldownManager::new();
        assert!(!m.is_in_cooldown_at("ignoto", t0()));
        assert_eq!(m.seconds_remaining_at("ignoto", t0()), 0);
    }

    #[test]
    fn snapshot_riporta_solo_in_cooldown_con_billing_flag() {
        let m = CooldownManager::new();
        let now = t0();
        m.mark_at(
            "openai",
            CooldownReason::Billing,
            Some("credit balance too low".to_string()),
            now,
            3600,
        );
        m.mark_at("mistral", CooldownReason::Transient, Some("timeout".to_string()), now, 30);
        // Provider scaduto: non deve comparire.
        m.mark_at("deepseek", CooldownReason::Transient, None, now, 10);

        let later = now + chrono::Duration::seconds(20);
        let snap = m.snapshot_at(later);

        // deepseek scaduto a +20s, openai e mistral ancora attivi.
        assert_eq!(snap.len(), 2);

        let openai = snap.iter().find(|s| s.name == "openai").unwrap();
        assert!(!openai.healthy);
        assert_eq!(openai.billing_error.as_deref(), Some("credit balance too low"));

        let mistral = snap.iter().find(|s| s.name == "mistral").unwrap();
        // Transient: nessun billing_error, ma last_error presente.
        assert!(mistral.billing_error.is_none());
        assert_eq!(mistral.last_error.as_deref(), Some("timeout"));
    }

    #[tokio::test]
    async fn recovery_pass_libera_provider_tornato_sano() {
        let m = CooldownManager::new();
        // openai in cooldown billing, healthcheck simulato SANO -> verra' liberato.
        m.mark_at("openai", CooldownReason::Billing, None, Utc::now(), 3600);
        let openai = FakeProvider::new("openai", true);
        let providers: Vec<Arc<dyn LlmProvider>> = vec![openai.clone()];

        assert!(m.is_in_cooldown("openai"));
        run_recovery_pass(&m, &providers).await;

        // Probe eseguito una volta e provider liberato (il fix: rientro reattivo).
        assert_eq!(openai.probe_calls.load(Ordering::SeqCst), 1);
        assert!(!m.is_in_cooldown("openai"));
    }

    #[tokio::test]
    async fn recovery_pass_lascia_in_cooldown_provider_ancora_rotto() {
        let m = CooldownManager::new();
        m.mark_at("openai", CooldownReason::Billing, None, Utc::now(), 3600);
        // Ancora non sano (crediti non ricaricati).
        let openai = FakeProvider::new("openai", false);
        let providers: Vec<Arc<dyn LlmProvider>> = vec![openai.clone()];

        run_recovery_pass(&m, &providers).await;

        assert_eq!(openai.probe_calls.load(Ordering::SeqCst), 1);
        assert!(m.is_in_cooldown("openai"));
    }

    #[tokio::test]
    async fn recovery_pass_non_sonda_provider_non_in_cooldown() {
        let m = CooldownManager::new();
        // Nessun provider in cooldown: il pass non deve sondare nulla.
        let openai = FakeProvider::new("openai", true);
        let providers: Vec<Arc<dyn LlmProvider>> = vec![openai.clone()];

        run_recovery_pass(&m, &providers).await;

        assert_eq!(openai.probe_calls.load(Ordering::SeqCst), 0);
    }
}
