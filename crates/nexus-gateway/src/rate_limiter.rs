//! Rate limiter in-memory a sliding window per (tenant, provider).
//!
//! Porting fedele di `packages/llm-gateway/src/router/rate-limiter.ts`. In
//! produzione la stessa interfaccia e' sostituibile da Redis (contratto
//! identico, implementazione diversa). Qui lo store concorrente e' un
//! `DashMap` (la versione TS usa una `Map` non sincronizzata: in Rust serve
//! l'accesso concorrente tra task tokio).
//!
//! Finestra "fixed window" come nel TS: alla prima richiesta della finestra si
//! registra `window_start`; le richieste successive incrementano il contatore
//! finche' la finestra non scade (`now - window_start >= window`), dopodiche'
//! la finestra si azzera. Quando il contatore raggiunge il massimo si ritorna
//! `Err(RateLimitExceeded)` con il `retry_after` residuo.

use std::time::{Duration, Instant};

use dashmap::DashMap;

/// Configurazione di un singolo limite (numero richieste per finestra).
#[derive(Debug, Clone, Copy)]
pub struct LimitConfig {
    /// Numero massimo di richieste ammesse nella finestra.
    pub requests: u32,
    /// Ampiezza della finestra temporale.
    pub window: Duration,
}

/// Limiti applicati: uno per-tenant, uno per-provider.
#[derive(Debug, Clone, Copy)]
pub struct RateLimits {
    pub per_tenant: LimitConfig,
    pub per_provider: LimitConfig,
}

/// Errore di superamento del rate limit (`RateLimitError` del TS).
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct RateLimitExceeded {
    /// Messaggio leggibile per il caller.
    pub message: String,
    /// Tempo da attendere prima di ritentare (residuo della finestra).
    pub retry_after: Duration,
    /// Chiave del bucket che ha superato il limite (es. "tenant:acme").
    pub key: String,
    /// Conteggio raggiunto al momento del blocco.
    pub count: u32,
    /// Limite configurato per il bucket.
    pub limit: u32,
}

/// Stato di un bucket: conteggio corrente e inizio della finestra.
#[derive(Debug, Clone, Copy)]
struct Entry {
    count: u32,
    window_start: Instant,
}

/// Rate limiter concorrente. `Clone` a basso costo (lo store e' dietro `Arc`
/// interno a `DashMap`), quindi condivisibile tra handler axum.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    store: std::sync::Arc<DashMap<String, Entry>>,
    limits: RateLimits,
}

impl RateLimiter {
    /// Crea un rate limiter con i limiti indicati.
    pub fn new(limits: RateLimits) -> Self {
        Self {
            store: std::sync::Arc::new(DashMap::new()),
            limits,
        }
    }

    /// Verifica/consuma una unita' di quota per il tenant indicato.
    pub fn check_tenant(&self, tenant_id: &str) -> Result<(), RateLimitExceeded> {
        let cfg = self.limits.per_tenant;
        self.check(
            format!("tenant:{tenant_id}"),
            cfg,
            format!("Tenant \"{tenant_id}\" ha superato il rate limit"),
        )
    }

    /// Verifica/consuma una unita' di quota per il provider indicato.
    pub fn check_provider(&self, provider_name: &str) -> Result<(), RateLimitExceeded> {
        let cfg = self.limits.per_provider;
        self.check(
            format!("provider:{provider_name}"),
            cfg,
            format!("Provider \"{provider_name}\" ha raggiunto il rate limit"),
        )
    }

    /// Logica comune: registra o aggiorna il bucket `key` secondo `cfg`.
    fn check(
        &self,
        key: String,
        cfg: LimitConfig,
        error_msg: String,
    ) -> Result<(), RateLimitExceeded> {
        let now = Instant::now();

        // `entry` tiene il lock sullo shard del DashMap per tutta la sezione
        // critica: l'incremento e' atomico rispetto agli altri task.
        let mut slot = self.store.entry(key.clone()).or_insert(Entry {
            count: 0,
            window_start: now,
        });

        // Finestra nuova (prima richiesta o finestra scaduta): azzera e ammetti.
        if slot.count == 0 || now.duration_since(slot.window_start) >= cfg.window {
            slot.count = 1;
            slot.window_start = now;
            return Ok(());
        }

        // Limite raggiunto: rifiuta con il residuo della finestra.
        if slot.count >= cfg.requests {
            let elapsed = now.duration_since(slot.window_start);
            let retry_after = cfg.window.saturating_sub(elapsed);
            return Err(RateLimitExceeded {
                message: error_msg,
                retry_after,
                key,
                count: slot.count,
                limit: cfg.requests,
            });
        }

        slot.count += 1;
        Ok(())
    }

    /// Rimuove i bucket con finestra scaduta (cleanup periodico, `cleanup` del TS).
    pub fn cleanup(&self) {
        let now = Instant::now();
        self.store.retain(|key, entry| {
            let window = if key.starts_with("tenant:") {
                self.limits.per_tenant.window
            } else {
                self.limits.per_provider.window
            };
            now.duration_since(entry.window_start) < window
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(tenant_reqs: u32, provider_reqs: u32) -> RateLimiter {
        RateLimiter::new(RateLimits {
            per_tenant: LimitConfig {
                requests: tenant_reqs,
                window: Duration::from_secs(60),
            },
            per_provider: LimitConfig {
                requests: provider_reqs,
                window: Duration::from_secs(60),
            },
        })
    }

    #[test]
    fn entro_il_limite_passa() {
        let rl = limiter(3, 3);
        assert!(rl.check_tenant("acme").is_ok());
        assert!(rl.check_tenant("acme").is_ok());
        assert!(rl.check_tenant("acme").is_ok());
    }

    #[test]
    fn oltre_il_limite_e_errore() {
        let rl = limiter(2, 10);
        assert!(rl.check_tenant("acme").is_ok()); // count=1
        assert!(rl.check_tenant("acme").is_ok()); // count=2
        let err = rl.check_tenant("acme").unwrap_err(); // count gia' 2 >= 2
        assert_eq!(err.limit, 2);
        assert_eq!(err.count, 2);
        assert!(err.key.starts_with("tenant:"));
        assert!(err.retry_after <= Duration::from_secs(60));
    }

    #[test]
    fn tenant_e_provider_sono_bucket_separati() {
        let rl = limiter(1, 1);
        // Stesso identificatore "x" ma namespace diverso: non si interferiscono.
        assert!(rl.check_tenant("x").is_ok());
        assert!(rl.check_provider("x").is_ok());
        // Secondo giro su ciascuno: entrambi oltre il limite di 1.
        assert!(rl.check_tenant("x").is_err());
        assert!(rl.check_provider("x").is_err());
    }

    #[test]
    fn finestra_scaduta_riammette() {
        let rl = RateLimiter::new(RateLimits {
            per_tenant: LimitConfig {
                requests: 1,
                window: Duration::from_millis(5),
            },
            per_provider: LimitConfig {
                requests: 1,
                window: Duration::from_secs(60),
            },
        });
        assert!(rl.check_tenant("acme").is_ok());
        assert!(rl.check_tenant("acme").is_err());
        std::thread::sleep(Duration::from_millis(10));
        // Finestra scaduta: il bucket si azzera e riammette.
        assert!(rl.check_tenant("acme").is_ok());
    }

    #[test]
    fn cleanup_rimuove_bucket_scaduti() {
        let rl = RateLimiter::new(RateLimits {
            per_tenant: LimitConfig {
                requests: 5,
                window: Duration::from_millis(5),
            },
            per_provider: LimitConfig {
                requests: 5,
                window: Duration::from_millis(5),
            },
        });
        let _ = rl.check_tenant("acme");
        assert_eq!(rl.store.len(), 1);
        std::thread::sleep(Duration::from_millis(10));
        rl.cleanup();
        assert_eq!(rl.store.len(), 0);
    }
}
