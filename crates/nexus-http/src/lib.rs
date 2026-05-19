//! # nexus-http
//!
//! Client HTTP condiviso per tutti i backend Nexus.
//! Fornisce timeout, retry, connection pooling e proxy applicativo opzionale.
//!
//! Legge configurazione da variabili d'ambiente NEXUS_*:
//! - `NEXUS_HTTP_TIMEOUT_SECS` (default 30)
//! - `NEXUS_HTTP_POOL_MAX` (default 20 idle per host)
//! - `NEXUS_HTTP_POOL_IDLE_SECS` (default 90)
//! - `NEXUS_PROXY` — proxy applicativo (es. http://localhost:8002), non modifica il sistema

use std::time::Duration;
use reqwest::{Client, ClientBuilder, Proxy};
use tracing::{debug, warn};

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_POOL_MAX: usize = 20;
const DEFAULT_POOL_IDLE_SECS: u64 = 90;
const USER_AGENT: &str = concat!("nexus-backend/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone)]
pub struct NexusHttpConfig {
    pub timeout_secs: u64,
    pub pool_max: usize,
    pub pool_idle_timeout_secs: u64,
    /// Proxy applicativo opzionale. Non tocca /etc/resolv.conf ne altre configurazioni di sistema.
    pub proxy: Option<String>,
}

impl NexusHttpConfig {
    pub fn from_env() -> Self {
        Self {
            timeout_secs: std::env::var("NEXUS_HTTP_TIMEOUT_SECS")
                .ok().and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_TIMEOUT_SECS),
            pool_max: std::env::var("NEXUS_HTTP_POOL_MAX")
                .ok().and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_POOL_MAX),
            pool_idle_timeout_secs: std::env::var("NEXUS_HTTP_POOL_IDLE_SECS")
                .ok().and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_POOL_IDLE_SECS),
            proxy: std::env::var("NEXUS_PROXY").ok().filter(|v| !v.is_empty()),
        }
    }
}

impl Default for NexusHttpConfig {
    fn default() -> Self { Self::from_env() }
}

/// Configurazione globale inizializzata da `main.rs` dopo la lettura del DB.
/// Se non inizializzata, `build_client` ricade su `from_env()`.
/// L'env var (NEXUS_HTTP_TIMEOUT_SECS, NEXUS_HTTP_POOL_MAX) resta come override.
static GLOBAL_CONFIG: std::sync::OnceLock<NexusHttpConfig> = std::sync::OnceLock::new();

/// Inizializza la configurazione globale con i valori letti dal DB.
/// I parametri `None` mantengono il default da env/costante.
/// Idempotente: la seconda chiamata non ha effetto (OnceLock).
pub fn init_global_config(timeout_secs: Option<u64>, pool_max: Option<usize>) {
    let mut cfg = NexusHttpConfig::from_env();
    // L'env var ha priorita' sulla lettura DB: applica override DB solo se
    // l'env var non e' impostata.
    if std::env::var("NEXUS_HTTP_TIMEOUT_SECS").is_err() {
        if let Some(v) = timeout_secs {
            cfg.timeout_secs = v;
        }
    }
    if std::env::var("NEXUS_HTTP_POOL_MAX").is_err() {
        if let Some(v) = pool_max {
            cfg.pool_max = v;
        }
    }
    let _ = GLOBAL_CONFIG.set(cfg);
}

/// Costruisce un `reqwest::Client` ottimizzato con le impostazioni Nexus.
///
/// Usa la configurazione globale inizializzata da `init_global_config` se
/// disponibile, altrimenti cade su `from_env()`.
/// Se `NEXUS_PROXY` e' impostato, le richieste HTTPS vengono inoltrate al proxy.
pub fn build_client() -> Client {
    let config = GLOBAL_CONFIG.get_or_init(NexusHttpConfig::from_env);
    build_client_with_config(config)
}

pub fn build_client_with_config(config: &NexusHttpConfig) -> Client {
    let mut builder = ClientBuilder::new()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(config.timeout_secs))
        .connect_timeout(Duration::from_secs(10))
        .pool_max_idle_per_host(config.pool_max)
        .pool_idle_timeout(Duration::from_secs(config.pool_idle_timeout_secs))
        .tcp_keepalive(Duration::from_secs(60))
        .tcp_nodelay(true)
        .use_rustls_tls()
        .redirect(reqwest::redirect::Policy::limited(5));

    if let Some(proxy_url) = &config.proxy {
        match Proxy::all(proxy_url) {
            Ok(proxy) => {
                debug!("nexus-http: proxy applicativo -> {}", proxy_url);
                builder = builder.proxy(proxy);
            }
            Err(e) => {
                warn!("nexus-http: NEXUS_PROXY='{}' non valido: {} — ignorato", proxy_url, e);
            }
        }
    }

    // safety: reqwest::ClientBuilder::build() puo' fallire solo per
    // init TLS o allocazione. E' bootstrap del client HTTP globale —
    // se fallisce qui, l'intera applicazione non puo' fare HTTP comunque.
    // Ammesso da CLAUDE.md §F come "bootstrap critico".
    builder.build().expect("nexus-http: impossibile costruire il client HTTP")
}

#[derive(Clone, Debug)]
pub struct NexusClient {
    inner: Client,
}

impl NexusClient {
    pub fn build() -> Self { Self { inner: build_client() } }
    pub fn with_config(config: &NexusHttpConfig) -> Self { Self { inner: build_client_with_config(config) } }
    pub fn with_timeout(secs: u64) -> Self {
        let mut c = NexusHttpConfig::from_env();
        c.timeout_secs = secs;
        Self::with_config(&c)
    }
    pub fn inner(&self) -> &Client { &self.inner }
    pub fn get(&self, url: &str) -> reqwest::RequestBuilder { self.inner.get(url) }
    pub fn post(&self, url: &str) -> reqwest::RequestBuilder { self.inner.post(url) }
    pub fn put(&self, url: &str) -> reqwest::RequestBuilder { self.inner.put(url) }
    pub fn delete(&self, url: &str) -> reqwest::RequestBuilder { self.inner.delete(url) }
    pub fn patch(&self, url: &str) -> reqwest::RequestBuilder { self.inner.patch(url) }
}

impl std::ops::Deref for NexusClient {
    type Target = Client;
    fn deref(&self) -> &Self::Target { &self.inner }
}

impl From<NexusClient> for Client {
    fn from(c: NexusClient) -> Self { c.inner }
}

/// Esegue una richiesta con retry automatico e backoff esponenziale.
/// Riprova su errori di connessione/timeout e risposte 5xx (max `max_retries` tentativi extra).
pub async fn with_retry<F, Fut>(
    max_retries: u32,
    mut request_fn: F,
) -> Result<reqwest::Response, reqwest::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<reqwest::Response, reqwest::Error>>,
{
    let mut attempt = 0u32;
    loop {
        match request_fn().await {
            Ok(resp) if resp.status().is_success() || resp.status().is_client_error() => {
                return Ok(resp);
            }
            Ok(resp) if attempt >= max_retries => return Ok(resp),
            Ok(_) => {
                attempt += 1;
                let delay = Duration::from_millis(500 * (1u64 << attempt.min(4)));
                debug!("nexus-http: retry {}/{} (5xx) dopo {:?}", attempt, max_retries, delay);
                tokio::time::sleep(delay).await;
            }
            Err(e) if attempt >= max_retries => return Err(e),
            Err(e) if e.is_connect() || e.is_timeout() => {
                attempt += 1;
                let delay = Duration::from_millis(500 * (1u64 << attempt.min(4)));
                debug!("nexus-http: retry {}/{} ({}) dopo {:?}", attempt, max_retries, e, delay);
                tokio::time::sleep(delay).await;
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // I test in questo modulo mutano variabili d'ambiente di processo. Se eseguiti
    // in parallelo (default di cargo test) causano poisoning reciproco (es. uno
    // imposta NEXUS_PROXY, un altro lo legge prima che il primo faccia remove_var,
    // un terzo confonde l'asserzione). Serializziamo con un mutex statico.
    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_config_defaults() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::remove_var("NEXUS_HTTP_TIMEOUT_SECS");
        std::env::remove_var("NEXUS_PROXY");
        let c = NexusHttpConfig::from_env();
        assert_eq!(c.timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert!(c.proxy.is_none());
    }

    #[test]
    fn test_build_no_proxy() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::remove_var("NEXUS_PROXY");
        let _ = build_client();
    }

    #[test]
    fn test_build_invalid_proxy() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("NEXUS_PROXY", "not-a-valid-url!!!");
        let _ = build_client(); // non deve crashare, solo warn
        std::env::remove_var("NEXUS_PROXY");
    }
}
