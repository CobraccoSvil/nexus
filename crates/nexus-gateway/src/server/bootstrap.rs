//! Bootstrap del gateway: config, chiavi provider dal DB, costruzione provider.
//!
//! Porting della fase di avvio di `server.ts` (`loadApiKeysFromDb`, `loadConfig`,
//! costruzione `LLMGateway`). Differenze dovute alle regole Nexus:
//!   - regola G: profilo e chiavi provengono SOLO dal DB/env documentato, niente
//!     nomi modello hardcoded;
//!   - regola F: nessuna chiave finisce nei log (solo nomi provider abilitati);
//!   - regola L: i provider riusano i costruttori del crate (`providers::*`); le
//!     chiavi sono lette con il punto unico `nexus_auth::get_setting`.

use std::sync::Arc;

use anyhow::{Context, Result};
use reqwest::Client;
use sqlx::PgPool;

use crate::cooldown::CooldownManager;
use nexus_auth::llm_timeouts::LlmTimeouts;
use crate::model_alias_resolver::ModelAliasResolver;
use crate::policy_engine::PolicyEngine;
use crate::provider::LlmProvider;
use crate::providers::{
    AnthropicProvider, DeepSeekProvider, GenericOpenAiProvider, GoogleProvider, MistralProvider,
    OpenAiProvider, VllmProvider,
};
use crate::redaction::presidio_client::PresidioClient;
use crate::types::SensitivityTier;

use super::{AppState, RuntimeState, DEV_SERVICE_TOKEN};

/// Default dei path dei file di configurazione (relativi alla repo root). Sono
/// STRUTTURA, non valori di business: i nomi dei file sono fissi nel repo. I
/// percorsi effettivi sono override-abili via env per i test/CI.
const DEFAULT_ALIASES_FILE: &str = "config/model-aliases.yaml";
const DEFAULT_POLICY_DIR: &str = "config/policies";

/// Mappa profilo -> file policy. Il profilo `cloud` usa `default.yaml` (parita'
/// con il gateway Node che carica `config/policies/default.yaml` di default).
fn policy_file_for_profile(profile: &str, policy_dir: &str) -> String {
    let file = match profile {
        "onprem" => "onprem.yaml",
        "hybrid" => "hybrid.yaml",
        // cloud (default) e qualunque profilo sconosciuto -> default.yaml.
        _ => "default.yaml",
    };
    format!("{policy_dir}/{file}")
}

/// Config risolta del gateway. Immutabile dopo il caricamento; il reload ne
/// costruisce una nuova e sostituisce il `RuntimeState`.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub profile: String,
    pub aliases_file: String,
    pub policy_file: String,
}

impl GatewayConfig {
    /// Carica la config: profilo dal DB (`settings.nexus_profile`, regola G), con
    /// fallback all'env `NEXUS_PROFILE` e infine `cloud` (il default del setting).
    /// I path dei file sono override-abili via env (utile in CI/test).
    pub async fn load(db: &PgPool) -> Self {
        let profile = nexus_auth::get_setting(db, "nexus_profile")
            .await
            .or_else(|| std::env::var("NEXUS_PROFILE").ok())
            .unwrap_or_else(|| "cloud".to_string());

        let aliases_file = std::env::var("NEXUS_MODEL_ALIASES_FILE")
            .unwrap_or_else(|_| DEFAULT_ALIASES_FILE.to_string());

        let policy_file = std::env::var("NEXUS_LLM_POLICY_FILE").unwrap_or_else(|_| {
            let dir =
                std::env::var("NEXUS_LLM_POLICY_DIR").unwrap_or_else(|_| DEFAULT_POLICY_DIR.to_string());
            policy_file_for_profile(&profile, &dir)
        });

        Self {
            profile,
            aliases_file,
            policy_file,
        }
    }
}

/// Descrittore di un provider dal registry DB (`nexus_provider_registry`, mig 0565).
/// Regola F: la chiave risolta NON e' in questa struct (viene risolta e passata al
/// costruttore senza essere loggata).
#[derive(sqlx::FromRow)]
struct ProviderDescriptor {
    name: String,
    api_format: String,
    key_setting: Option<String>,
    enabled_setting: Option<String>,
    base_url_setting: Option<String>,
    base_url_default: Option<String>,
    activation: String,
    tiers: Vec<i32>,
    max_context_tokens: i32,
    supports_tools: bool,
}

/// Carica i descrittori provider dal registry (mig 0565), ordinati. Fallback ai 6
/// provider noti se la tabella non esiste / e' vuota (fail-safe: se la migrazione
/// non e' ancora applicata all'avvio, nessuna regressione).
async fn load_provider_descriptors(db: &PgPool) -> Vec<ProviderDescriptor> {
    let rows = sqlx::query_as::<_, ProviderDescriptor>(
        "SELECT name, api_format, key_setting, enabled_setting, base_url_setting, \
         base_url_default, activation, tiers, max_context_tokens, supports_tools \
         FROM nexus_provider_registry WHERE is_active = true ORDER BY sort_order, name",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();
    if rows.is_empty() {
        fallback_descriptors()
    } else {
        rows
    }
}

/// I 6 provider noti come descrittori in codice: fallback identico al seed della
/// mig 0565 quando il registry non e' leggibile (tabella assente / vuota).
fn fallback_descriptors() -> Vec<ProviderDescriptor> {
    #[allow(clippy::too_many_arguments)]
    fn d(
        name: &str,
        api_format: &str,
        key: Option<&str>,
        en: Option<&str>,
        bset: Option<&str>,
        bdef: Option<&str>,
        act: &str,
        tiers: Vec<i32>,
        ctx: i32,
    ) -> ProviderDescriptor {
        ProviderDescriptor {
            name: name.to_string(),
            api_format: api_format.to_string(),
            key_setting: key.map(str::to_string),
            enabled_setting: en.map(str::to_string),
            base_url_setting: bset.map(str::to_string),
            base_url_default: bdef.map(str::to_string),
            activation: act.to_string(),
            tiers,
            max_context_tokens: ctx,
            supports_tools: true,
        }
    }
    vec![
        d("openai", "openai", Some("openai_api_key"), Some("openai_enabled"), Some("openai_base_url"), None, "api_key", vec![0, 1, 2], 400_000),
        d("anthropic", "anthropic", Some("anthropic_api_key"), Some("anthropic_enabled"), Some("anthropic_base_url"), None, "api_key", vec![0, 1, 2], 200_000),
        d("google", "google", Some("google_api_key"), Some("google_enabled"), None, None, "api_key_or_vertex", vec![0, 1, 2], 1_000_000),
        d("mistral", "openai_compat", Some("mistral_api_key"), Some("mistral_enabled"), Some("mistral_base_url"), Some("https://api.mistral.ai/v1"), "api_key", vec![0, 1, 2], 128_000),
        d("deepseek", "deepseek", Some("deepseek_api_key"), Some("deepseek_enabled"), Some("deepseek_base_url"), None, "api_key", vec![0, 1, 2], 128_000),
        d("vllm", "openai_compat", None, None, Some("vllm_base_url"), None, "base_url", vec![0, 1, 2, 3], 32_768),
    ]
}

/// Backend Vertex configurato (regola G): backend=="vertex" e project+credenziali
/// presenti. Permette di abilitare Google senza api_key Gemini (Service Account).
async fn google_vertex_configured(db: &PgPool) -> bool {
    let backend = nexus_auth::get_setting(db, "google_provider_backend")
        .await
        .unwrap_or_default();
    if !backend.trim().eq_ignore_ascii_case("vertex") {
        return false;
    }
    let project = nexus_auth::get_setting(db, "google_vertex_project")
        .await
        .unwrap_or_default();
    let creds = nexus_auth::get_setting(db, "google_vertex_credentials_json")
        .await
        .unwrap_or_default();
    !project.trim().is_empty() && !creds.trim().is_empty()
}

/// Risolve la base_url override per un descrittore: setting `<x>_base_url` (se
/// presente e non vuoto) -> `base_url_default` del registry -> `None` (il
/// costruttore dedicato usa la propria costante).
async fn resolve_base_url(db: &PgPool, d: &ProviderDescriptor) -> Option<String> {
    if let Some(setting) = &d.base_url_setting {
        if let Some(v) = nexus_auth::get_setting(db, setting).await {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    d.base_url_default.clone()
}

/// Criterio di attivazione di un provider (puro, testabile). Replica la logica
/// storica di `ProviderKeys`: `api_key` richiede chiave+enabled; `base_url`
/// richiede la url (vLLM); `api_key_or_vertex` (Google) e' attivo con
/// chiave+enabled OPPURE con Vertex configurato, che BYPASSA `enabled` (l'auth via
/// Service Account non usa la api_key ne' il flag Gemini).
fn provider_is_active(
    activation: &str,
    enabled: bool,
    has_key: bool,
    has_base_url: bool,
    vertex_ok: bool,
) -> bool {
    match activation {
        "api_key" => enabled && has_key,
        "base_url" => has_base_url,
        "api_key_or_vertex" => (enabled && has_key) || vertex_ok,
        _ => false,
    }
}

/// Costruisce la lista dei provider abilitati dai descrittori del registry.
/// Ritorna SOLO i provider effettivamente attivabili (chiave presente / base_url /
/// Vertex). Niente segreti nel log (regola F): si logga la lista dei nomi, mai le
/// chiavi. La factory (`construct_provider`) seleziona l'adapter per nome (quirk
/// nei costruttori dedicati) o il provider generico per gli OpenAI-compat nuovi.
async fn build_providers(
    db: &PgPool,
    http: &Client,
    descriptors: &[ProviderDescriptor],
) -> Vec<Arc<dyn LlmProvider>> {
    let vertex_ok = google_vertex_configured(db).await;
    let mut providers: Vec<Arc<dyn LlmProvider>> = Vec::new();

    for d in descriptors {
        // `*_enabled` assente -> abilitato (default storico true, mig 0045).
        let enabled = match &d.enabled_setting {
            Some(s) => nexus_auth::get_bool_setting(db, s)
                .await
                .ok()
                .flatten()
                .unwrap_or(true),
            None => true,
        };
        let key: Option<String> = match &d.key_setting {
            Some(s) if enabled => nexus_auth::get_setting(db, s)
                .await
                .filter(|k| !k.trim().is_empty()),
            _ => None,
        };
        let base_url = resolve_base_url(db, d).await;

        if !provider_is_active(&d.activation, enabled, key.is_some(), base_url.is_some(), vertex_ok)
        {
            continue;
        }

        if let Some(p) = construct_provider(db, http, d, key, base_url) {
            providers.push(p);
        }
    }

    providers
}

/// Factory: mappa un descrittore ATTIVO al costruttore concreto. Gli adapter con
/// quirk sono selezionati per nome (o-series OpenAI, XML/thinking DeepSeek, cache
/// Anthropic, Vertex Google, placeholder vLLM); un `api_format='openai_compat'`
/// non tra i noti -> provider generico costruito dai campi del registry (regola G:
/// un provider OpenAI-compat nuovo = riga registry, zero nuovo codice).
fn construct_provider(
    db: &PgPool,
    http: &Client,
    d: &ProviderDescriptor,
    key: Option<String>,
    base_url: Option<String>,
) -> Option<Arc<dyn LlmProvider>> {
    // Chiave per i costruttori che la vogliono per valore (vuota se solo Vertex).
    let k = key.clone().unwrap_or_default();
    match d.name.as_str() {
        // DB passato per: reasoning_effort o-series (regola G).
        "openai" => Some(Arc::new(OpenAiProvider::with_db(
            http.clone(),
            k,
            base_url,
            Some(db.clone()),
        ))),
        // DB passato per: budget thinking (regola G).
        "anthropic" => Some(Arc::new(AnthropicProvider::with_db(
            http.clone(),
            k,
            base_url,
            Some(db.clone()),
        ))),
        "deepseek" => Some(Arc::new(DeepSeekProvider::new(http.clone(), k, base_url))),
        // DB passato per: backend gemini/vertex, credenziali SA (regola G). api_key
        // vuota se si usa solo Vertex.
        "google" => Some(Arc::new(GoogleProvider::with_db(
            http.clone(),
            k,
            base_url,
            Some(db.clone()),
        ))),
        "mistral" => Some(Arc::new(MistralProvider::new(http.clone(), k, base_url))),
        "vllm" => {
            // vLLM: base_url obbligatoria (l'attivazione 'base_url' l'ha garantita).
            let url = base_url?;
            Some(Arc::new(VllmProvider::new(http.clone(), url, key, None)))
        }
        _ => {
            if d.api_format != "openai_compat" {
                tracing::warn!(
                    "provider '{}' con api_format '{}' senza adapter dedicato: skip",
                    d.name,
                    d.api_format
                );
                return None;
            }
            let url = base_url.or_else(|| d.base_url_default.clone())?;
            let tiers: Vec<SensitivityTier> =
                d.tiers.iter().map(|t| *t as SensitivityTier).collect();
            Some(Arc::new(GenericOpenAiProvider::new(
                http.clone(),
                url,
                k,
                d.name.clone(),
                tiers,
                d.max_context_tokens as u32,
                d.supports_tools,
            )))
        }
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    #[test]
    fn fallback_descriptors_replica_i_6_provider() {
        let d = fallback_descriptors();
        let names: Vec<&str> = d.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(
            names,
            ["openai", "anthropic", "google", "mistral", "deepseek", "vllm"]
        );
        // Capacita' usate dal provider generico (openai_compat): devono combaciare
        // con providers/mistral.rs e providers/vllm.rs (regression-zero).
        let mistral = d.iter().find(|x| x.name == "mistral").unwrap();
        assert_eq!(mistral.api_format, "openai_compat");
        assert_eq!(mistral.tiers, vec![0, 1, 2]);
        assert_eq!(mistral.max_context_tokens, 128_000);
        let vllm = d.iter().find(|x| x.name == "vllm").unwrap();
        assert_eq!(vllm.api_format, "openai_compat");
        assert_eq!(vllm.tiers, vec![0, 1, 2, 3]);
        assert_eq!(vllm.max_context_tokens, 32_768);
        assert_eq!(vllm.activation, "base_url");
        assert!(vllm.key_setting.is_none());
    }

    #[test]
    fn attivazione_api_key() {
        assert!(provider_is_active("api_key", true, true, false, false));
        assert!(!provider_is_active("api_key", false, true, false, false)); // enabled=false
        assert!(!provider_is_active("api_key", true, false, false, false)); // niente chiave
    }

    #[test]
    fn attivazione_base_url_vllm() {
        assert!(provider_is_active("base_url", true, false, true, false));
        assert!(!provider_is_active("base_url", true, false, false, false));
    }

    #[test]
    fn attivazione_google_vertex_bypassa_enabled() {
        // Regressione: Vertex configurato attiva Google anche con google_enabled=false.
        assert!(provider_is_active("api_key_or_vertex", false, false, false, true));
        // Chiave + enabled, senza vertex: attivo.
        assert!(provider_is_active("api_key_or_vertex", true, true, false, false));
        // enabled=false, niente vertex, con chiave: NON attivo.
        assert!(!provider_is_active("api_key_or_vertex", false, true, false, false));
    }
}

/// Costruisce il `RuntimeState` da config + DB: provider, policy, alias, Presidio.
/// Estratto come funzione cosi' `/admin/reload` lo riusa per la sostituzione a
/// caldo.
pub async fn build_runtime(
    db: &PgPool,
    http: &Client,
    config: GatewayConfig,
    timeouts: LlmTimeouts,
) -> Result<RuntimeState> {
    let descriptors = load_provider_descriptors(db).await;
    let providers = build_providers(db, http, &descriptors).await;

    let policy = PolicyEngine::from_yaml_file(&config.policy_file)
        .with_context(|| format!("policy file '{}'", config.policy_file))?;
    // Refresh iniziale forzato dei flag DLP dal DB (best-effort, regola G).
    policy.refresh_db_overrides(db, true).await;

    let aliases = ModelAliasResolver::from_yaml_file(&config.aliases_file)
        .with_context(|| format!("aliases file '{}'", config.aliases_file))?;

    let presidio = PresidioClient::new();
    presidio.refresh_config(db, true).await;

    let provider_names: Vec<&str> = providers.iter().map(|p| p.name()).collect();
    tracing::info!(
        profile = %config.profile,
        providers = ?provider_names,
        "gateway: runtime costruito"
    );

    Ok(RuntimeState {
        providers,
        policy: Arc::new(policy),
        aliases: Arc::new(aliases),
        presidio,
        profile: config.profile.clone(),
        config: Arc::new(config),
        timeouts,
    })
}

/// Punto unico di costruzione del client HTTP verso i provider (regola L).
///
/// Esiste perche' `/admin/reload` ne costruiva un GEMELLO con
/// `reqwest::Client::new()`: dopo un reload i provider perdevano timeout,
/// keepalive e `pool_max_idle_per_host(0)` — cioe' proprio le protezioni contro
/// le chiamate appese e le connessioni morte post-sleep. Un solo costruttore,
/// nessuna copia da tenere allineata.
///
/// Resilienza alle connessioni TCP morte (regola H, causa radice): il pool
/// keep-alive di default (idle illimitato, nessun keepalive) fa RIUSARE
/// connessioni che muoiono quando la macchina va in sleep; al risveglio la prima
/// richiesta su una connessione morta fallisce con "error sending request" e il
/// provider appare down pur essendo raggiungibile (verificato: reqwest da fresco
/// -> 200). `pool_max_idle_per_host(0)` non trattiene idle: ogni chiamata usa una
/// connessione fresca. L'handshake (~100-300ms) e' trascurabile sulle chiamate
/// LLM (secondi di inference); il pool illimitato costava un run KO per risveglio.
///
/// Il timeout e' quello del TRASPORTO condiviso (copre lo streaming, il caso piu'
/// lungo): il budget applicativo delle completion e' la deadline logica in
/// `routes::complete`, non questo tetto.
pub fn build_http_client(timeouts: &LlmTimeouts) -> Result<Client> {
    Client::builder()
        .timeout(timeouts.client_http_timeout())
        .tcp_keepalive(std::time::Duration::from_secs(30))
        .pool_max_idle_per_host(0)
        .build()
        .context("costruzione client HTTP gateway")
}

/// Costruisce lo stato applicativo completo all'avvio: pool, token, JWT, runtime
/// e cooldown manager. NON avvia il re-probe loop ne' il server (lo fa il
/// binario). Il client HTTP e' condiviso tra tutti i provider (pool riuso).
pub async fn build_state(db: PgPool) -> Result<AppState> {
    let timeouts = LlmTimeouts::resolve(&db).await;
    tracing::info!(
        transport_timeout_s = timeouts.client_http_timeout().as_secs(),
        request_budget_s = timeouts.request_budget.as_secs(),
        per_attempt_s = timeouts.per_attempt.as_secs(),
        run_timeout_s = timeouts.run_timeout.as_secs(),
        min_guaranteed_turns = timeouts.min_guaranteed_turns,
        "gateway: timeout LLM derivati dal punto unico (DB-driven)"
    );
    let http = build_http_client(&timeouts)?;

    let config = GatewayConfig::load(&db).await;
    let runtime = build_runtime(&db, &http, config, timeouts).await?;

    let service_token = std::env::var("NEXUS_GATEWAY_SERVICE_TOKEN")
        .unwrap_or_else(|_| DEV_SERVICE_TOKEN.to_string());

    // Chiave di firma della piattaforma. NON e' un `Option`: finche' lo era,
    // `token_is_valid` aveva un ramo che con `None` faceva passare QUALUNQUE
    // richiesta, anche senza header — e quello stato era raggiungibile per
    // costruzione, perche' `jwt_secret` e' seminata VUOTA dalla mig 0003 e
    // veniva generata solo al primo login: un gateway avviato prima di quel
    // login restava ad autenticazione disabilitata fino al riavvio successivo.
    //
    // Ora il segreto viene GENERATO qui se la riga e' vuota (punto unico
    // `get_or_create_platform_secret`, atomico), quindi lo stato "nessun
    // segreto" non e' piu' rappresentabile e il ramo permissivo non esiste.
    // Se il DB non risponde il gateway non parte: e' gia' cosi' per la
    // connessione (`bin/server.rs`, connect eager) e per `assert_configured`
    // del listino, quindi non introduce un modo nuovo di non partire.
    let jwt_secret = nexus_auth::get_or_create_platform_secret(&db, "jwt_secret")
        .await
        .context("jwt_secret non risolvibile: il gateway non puo' autenticare")?;
    if jwt_secret.len() < 32 {
        anyhow::bail!(
            "jwt_secret piu' corto di 32 caratteri ({} char): chiave di firma non accettabile",
            jwt_secret.len()
        );
    }

    let mcp_core_url =
        std::env::var("MCP_CORE_URL").unwrap_or_else(|_| "http://localhost:4000".to_string());

    // Il pool collegato abilita la persistenza dell'ultimo errore per provider
    // su nexus_provider_health / _history (migrazione 0536).
    let cooldown = CooldownManager::new();
    cooldown.attach_db(db.clone());

    Ok(AppState {
        db,
        service_token,
        jwt_secret,
        mcp_core_url,
        cooldown,
        runtime: Arc::new(tokio::sync::RwLock::new(runtime)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profilo_mappa_al_file_policy() {
        assert_eq!(
            policy_file_for_profile("cloud", "config/policies"),
            "config/policies/default.yaml"
        );
        assert_eq!(
            policy_file_for_profile("onprem", "config/policies"),
            "config/policies/onprem.yaml"
        );
        assert_eq!(
            policy_file_for_profile("hybrid", "config/policies"),
            "config/policies/hybrid.yaml"
        );
        // Profilo sconosciuto -> default (fail-safe documentato).
        assert_eq!(
            policy_file_for_profile("ignoto", "config/policies"),
            "config/policies/default.yaml"
        );
    }
}
