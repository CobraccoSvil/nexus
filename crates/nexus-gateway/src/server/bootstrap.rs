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
use crate::model_alias_resolver::ModelAliasResolver;
use crate::policy_engine::PolicyEngine;
use crate::provider::LlmProvider;
use crate::providers::{
    AnthropicProvider, DeepSeekProvider, GoogleProvider, MistralProvider, OpenAiProvider,
    VllmProvider,
};
use crate::redaction::presidio_client::PresidioClient;

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

/// Chiavi provider lette dal DB. `None` = provider disabilitato o senza chiave.
/// Regola F: questa struct NON viene mai loggata (contiene segreti).
struct ProviderKeys {
    openai: Option<String>,
    anthropic: Option<String>,
    mistral: Option<String>,
    deepseek: Option<String>,
    google: Option<String>,
    /// True se il backend Google e' "vertex" con project+credenziali presenti in
    /// DB: in tal caso il provider Google va istanziato anche SENZA api_key
    /// Gemini (l'auth e' via Service Account, non query param).
    google_vertex_configured: bool,
    vllm_base_url: Option<String>,
}

impl ProviderKeys {
    /// Carica le chiavi e i flag `*_enabled` dal DB. Un provider e' incluso solo
    /// se la sua chiave e' non vuota E il flag `*_enabled` non e' `false`.
    /// Le chiavi sono lette col punto unico `nexus_auth::get_setting` (regola L):
    /// nessuna query duplicata sulla tabella `settings`.
    async fn load(db: &PgPool) -> Self {
        async fn keyed(db: &PgPool, key_setting: &str, enabled_setting: &str) -> Option<String> {
            // `*_enabled` assente -> abilitato (default storico true, mig 0045).
            let enabled = nexus_auth::get_bool_setting(db, enabled_setting)
                .await
                .ok()
                .flatten()
                .unwrap_or(true);
            if !enabled {
                return None;
            }
            nexus_auth::get_setting(db, key_setting).await
        }

        // Backend Vertex configurato? (regola G): backend=="vertex" e
        // project+credenziali presenti. Permette di abilitare Google senza
        // api_key Gemini quando si usa il Service Account.
        let google_vertex_configured = {
            let backend = nexus_auth::get_setting(db, "google_provider_backend")
                .await
                .unwrap_or_default();
            if backend.trim().eq_ignore_ascii_case("vertex") {
                let project = nexus_auth::get_setting(db, "google_vertex_project")
                    .await
                    .unwrap_or_default();
                let creds = nexus_auth::get_setting(db, "google_vertex_credentials_json")
                    .await
                    .unwrap_or_default();
                !project.trim().is_empty() && !creds.trim().is_empty()
            } else {
                false
            }
        };

        Self {
            openai: keyed(db, "openai_api_key", "openai_enabled").await,
            anthropic: keyed(db, "anthropic_api_key", "anthropic_enabled").await,
            mistral: keyed(db, "mistral_api_key", "mistral_enabled").await,
            deepseek: keyed(db, "deepseek_api_key", "deepseek_enabled").await,
            google: keyed(db, "google_api_key", "google_enabled").await,
            google_vertex_configured,
            // vLLM (onprem): la chiave non e' obbligatoria; serve la base_url.
            vllm_base_url: nexus_auth::get_setting(db, "vllm_base_url").await,
        }
    }
}

/// Costruisce la lista dei provider abilitati a partire dalle chiavi. Ritorna
/// SOLO i provider effettivamente configurati. Niente segreti nel log (regola F):
/// si logga la lista dei nomi abilitati, mai le chiavi.
fn build_providers(db: &PgPool, http: &Client, keys: &ProviderKeys) -> Vec<Arc<dyn LlmProvider>> {
    let mut providers: Vec<Arc<dyn LlmProvider>> = Vec::new();

    if let Some(k) = &keys.openai {
        // DB passato per leggere il reasoning_effort o-series dai settings (regola G).
        providers.push(Arc::new(OpenAiProvider::with_db(
            http.clone(),
            k.clone(),
            None,
            Some(db.clone()),
        )));
    }
    if let Some(k) = &keys.anthropic {
        // DB passato per leggere il budget thinking dai settings (regola G).
        providers.push(Arc::new(AnthropicProvider::with_db(
            http.clone(),
            k.clone(),
            None,
            Some(db.clone()),
        )));
    }
    if let Some(k) = &keys.mistral {
        providers.push(Arc::new(MistralProvider::new(http.clone(), k.clone(), None)));
    }
    if let Some(k) = &keys.deepseek {
        providers.push(Arc::new(DeepSeekProvider::new(http.clone(), k.clone(), None)));
    }
    // Google: istanziato se c'e' la api_key Gemini OPPURE se il backend Vertex e'
    // configurato (Service Account, nessuna api_key richiesta). Il backend
    // effettivo (gemini/vertex) e' risolto a runtime dal provider via settings.
    if keys.google.is_some() || keys.google_vertex_configured {
        // DB passato per: budget thinking, backend gemini/vertex, credenziali
        // Service Account (regola G). api_key vuota se si usa solo Vertex.
        let api_key = keys.google.clone().unwrap_or_default();
        providers.push(Arc::new(GoogleProvider::with_db(
            http.clone(),
            api_key,
            None,
            Some(db.clone()),
        )));
    }
    if let Some(base_url) = &keys.vllm_base_url {
        providers.push(Arc::new(VllmProvider::new(
            http.clone(),
            base_url.clone(),
            None,
            None,
        )));
    }

    providers
}

/// Costruisce il `RuntimeState` da config + DB: provider, policy, alias, Presidio.
/// Estratto come funzione cosi' `/admin/reload` lo riusa per la sostituzione a
/// caldo.
pub async fn build_runtime(
    db: &PgPool,
    http: &Client,
    config: GatewayConfig,
) -> Result<RuntimeState> {
    let keys = ProviderKeys::load(db).await;
    let providers = build_providers(db, http, &keys);

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
    })
}

/// Costruisce lo stato applicativo completo all'avvio: pool, token, JWT, runtime
/// e cooldown manager. NON avvia il re-probe loop ne' il server (lo fa il
/// binario). Il client HTTP e' condiviso tra tutti i provider (pool riuso).
pub async fn build_state(db: PgPool) -> Result<AppState> {
    let http = Client::builder()
        .build()
        .context("costruzione client HTTP gateway")?;

    let config = GatewayConfig::load(&db).await;
    let runtime = build_runtime(&db, &http, config).await?;

    let service_token = std::env::var("NEXUS_GATEWAY_SERVICE_TOKEN")
        .unwrap_or_else(|_| DEV_SERVICE_TOKEN.to_string());

    // JWT secret dal DB (punto unico settings): valido solo se >= 32 char, come
    // il `JWT_SECRET.length >= 32` del server.ts. Altrimenti auth permissiva (dev).
    let jwt_secret = nexus_auth::get_setting(&db, "jwt_secret")
        .await
        .filter(|s| s.len() >= 32);
    if jwt_secret.is_none() {
        tracing::warn!(
            "gateway: jwt_secret assente o < 32 char nel DB -> autenticazione JWT permissiva (dev only)"
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
