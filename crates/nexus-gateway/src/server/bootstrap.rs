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
    AnthropicProvider, DeepSeekProvider, GenericOpenAiProvider, GoogleProvider, KimiProvider,
    MistralProvider, OpenAiProvider, VllmProvider,
};
use crate::redaction::presidio_client::PresidioClient;
use crate::tassonomia_errori::VocabolarioErrori;
use crate::types::SensitivityTier;

use super::{AppState, RuntimeState};

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
    /// Percorso della lista modelli relativo a `base_url` (mig 0705). Perplexity
    /// espone le completion sulla radice e i modelli sotto `/v1`: senza questo
    /// campo la discovery E lo healthcheck di quel fornitore erano 404 fissi.
    models_path: Option<String>,
    /// Header extra dichiarati dal registry (mig 0714), come testo del JSONB
    /// (oggetto piatto nome->valore). Consumato dal SOLO provider generico:
    /// gli adapter dedicati compongono le proprie richieste e non lo leggono.
    /// Oggi lo dichiara openrouter per l'attribuzione (HTTP-Referer/X-Title).
    extra_headers: Option<String>,
    /// Opt-in di usage accounting (mig 0717), come testo del boolean
    /// (`to_jsonb` rende 'true'/'false'; NULL = migrazione non applicata =
    /// false). Consumato dal SOLO provider generico: chiede al fornitore di
    /// dichiarare `usage.cost` sulla risposta. Oggi solo openrouter.
    usage_accounting: Option<String>,
    /// Tier di servizio imposto dal registry su OGNI richiesta dell'endpoint
    /// (mig 0728). NULL = non emettere, il default di tutti: un tier non
    /// richiesto e' un HTTP 400 sui piani che non lo includono (misurato su
    /// groq 'flex' il 17/08/2026). Consumato dal SOLO provider generico.
    service_tier: Option<String>,
}

/// Carica i descrittori provider dal registry (mig 0565), ordinati. Fallback ai 6
/// provider noti se la tabella non esiste / e' vuota (fail-safe: se la migrazione
/// non e' ancora applicata all'avvio, nessuna regressione).
async fn load_provider_descriptors(db: &PgPool) -> Vec<ProviderDescriptor> {
    // `models_path`, `extra_headers` e `usage_accounting` si leggono via
    // `to_jsonb(r) ->> ...` e non come colonne: su un DB dove la loro migrazione
    // (0705, 0714, 0717) non e' ancora applicata la chiave semplicemente non c'e'
    // e il valore esce NULL, mentre nominarle direttamente sarebbe un errore SQL
    // — e l'errore qui non degrada al default del campo, degrada all'INTERO
    // registry (`unwrap_or_default` -> `fallback_descriptors`, cioe' sei provider
    // al posto di dieci). Il costo di una colonna nuova non puo' essere la
    // sparizione di quattro fornitori nella finestra fra il riavvio del gateway e
    // le migrazioni.
    let rows = sqlx::query_as::<_, ProviderDescriptor>(
        "SELECT name, api_format, key_setting, enabled_setting, base_url_setting, \
         base_url_default, activation, tiers, max_context_tokens, supports_tools, \
         to_jsonb(r) ->> 'models_path' AS models_path, \
         to_jsonb(r) ->> 'extra_headers' AS extra_headers, \
         to_jsonb(r) ->> 'usage_accounting' AS usage_accounting, \
         to_jsonb(r) ->> 'service_tier' AS service_tier \
         FROM nexus_provider_registry r WHERE is_active = true ORDER BY sort_order, name",
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
            // Nessuno dei sei di ripiego devia dal `/models` del dialetto OpenAI:
            // il caso che ha motivato il campo (perplexity) non e' fra loro.
            models_path: None,
            // Idem per gli header extra: li dichiara il solo openrouter (mig
            // 0714), che non e' fra i sei di ripiego.
            extra_headers: None,
            // E idem per l'usage accounting (mig 0717): solo openrouter.
            usage_accounting: None,
            // E idem per il tier di servizio (mig 0728): nessuno dei sei di
            // ripiego ne dichiara uno.
            service_tier: None,
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

/// Legge gli header extra del registry (JSONB oggetto piatto, mig 0714) nella
/// forma che il client applica alle richieste. Funzione PURA, testabile.
///
/// Entrano le sole coppie con valore STRINGA: un valore di altro tipo non ha
/// una resa HTTP ovvia e si scarta, e un JSON non-oggetto o non parsabile vale
/// "nessun header" — l'attribuzione e' un miglioramento, non una condizione di
/// avvio, e un registry malformato non deve spegnere il provider.
fn parse_extra_headers(raw: &str) -> Vec<(String, String)> {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Object(campi)) => campi
            .into_iter()
            .filter_map(|(nome, valore)| match valore {
                serde_json::Value::String(s) => Some((nome, s)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
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
        // Adapter dedicato come mistral: `api_format='openai_compat'` nel
        // registry, ma il ramo per nome precede il controllo sul formato. I
        // quirk (temperatura fissa, max_completion_tokens, Preserved Thinking)
        // sono nel dialetto, non qui.
        // DB passato per: disattivabilita' del pensiero per modello (mig 0705,
        // regola G). Senza, nessuna richiesta lo spegne e il tetto di output se
        // lo prende il ragionamento.
        "kimi" => Some(Arc::new(KimiProvider::with_db(
            http.clone(),
            k,
            base_url,
            Some(db.clone()),
        ))),
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
            Some(Arc::new(
                GenericOpenAiProvider::new(
                    http.clone(),
                    url,
                    k,
                    d.name.clone(),
                    tiers,
                    d.max_context_tokens as u32,
                    d.supports_tools,
                )
                // Dove il fornitore non tiene la lista modelli sotto la base delle
                // completion (perplexity: `/v1/models` contro `/chat/completions`).
                .with_models_path(d.models_path.as_deref())
                // Header di attribuzione dichiarati dal registry (mig 0714):
                // openrouter chiede HTTP-Referer/X-Title su ogni richiesta.
                .with_extra_headers(
                    d.extra_headers
                        .as_deref()
                        .map(parse_extra_headers)
                        .unwrap_or_default(),
                )
                // Opt-in di usage accounting dal registry (mig 0717): il
                // fornitore dichiara il costo esatto in `usage.cost`. Il
                // boolean arriva come testo dal `to_jsonb`; NULL (migrazione
                // non applicata) vale false, cioe' il comportamento di prima.
                .with_usage_accounting(d.usage_accounting.as_deref() == Some("true"))
                // Tier di servizio dal registry (mig 0728): groq 'flex' quando
                // il piano dell'org lo include. NULL = nessun campo sul wire.
                .with_service_tier(d.service_tier.clone())
                // Serve agli instradatori per leggere il fornitore a valle
                // preferito; gli altri endpoint non lo interrogano mai.
                .with_db(Some(db.clone())),
            ))
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

    /// Server finto che risponde 200 SOLO al percorso atteso e 404 a ogni altro,
    /// esattamente come fa Perplexity. Ritorna la porta e il canale su cui
    /// pubblica i percorsi che ha davvero ricevuto: e' la sola prova di QUALE
    /// indirizzo il client abbia interrogato — asserire sul valore del campo
    /// proverebbe che la struct lo contiene, non che qualcuno lo usi.
    async fn finge_perplexity(
        percorso_servito: &'static str,
    ) -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("porta effimera");
        let porta = listener.local_addr().expect("indirizzo").port();
        let visti = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let registro = visti.clone();

        tokio::spawn(async move {
            // Terminatore di riga del PROTOCOLLO: costante, cosi' un
            // normalizzatore di fine-riga sull'albero non lo puo' toccare.
            const CRLF: &str = "\r\n";
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut grezzo = Vec::new();
                let mut buf = [0u8; 1024];
                // Fino alla fine degli header: una GET non ha corpo.
                while !grezzo.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => grezzo.extend_from_slice(&buf[..n]),
                    }
                }
                let richiesta = String::from_utf8_lossy(&grezzo).to_string();
                let percorso = richiesta
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or_default()
                    .to_string();
                registro.lock().expect("registro").push(percorso.clone());

                let (stato, corpo) = if percorso == percorso_servito {
                    ("200 OK", r#"{"object":"list","data":[{"id":"sonar"}]}"#)
                } else {
                    ("404 Not Found", "")
                };
                let testa = [
                    &format!("HTTP/1.1 {stato}"),
                    "Content-Type: application/json",
                    &format!("Content-Length: {}", corpo.len()),
                    "Connection: close",
                    "",
                    "",
                ]
                .join(CRLF);
                let _ = socket.write_all(testa.as_bytes()).await;
                let _ = socket.write_all(corpo.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });

        (porta, visti)
    }

    /// IL CASO MISURATO il 13/08/2026 contro l'API reale: Perplexity serve le
    /// completion sulla radice e i modelli sotto `/v1`, e il registry porta UNA
    /// base sola. La discovery chiedeva `GET https://api.perplexity.ai/models` e
    /// prendeva 404 a ogni sync; lo healthcheck e' la STESSA richiesta, quindi per
    /// il re-probe quel fornitore non sarebbe mai tornato sano.
    ///
    /// Attraversa la catena intera (regola O): migrazione reale -> riga di
    /// `nexus_provider_registry` -> `load_provider_descriptors` -> factory ->
    /// `list_models()` -> richiesta HTTP vera. Un test che passasse il percorso a
    /// mano al client proverebbe la concatenazione di due stringhe; il difetto
    /// stava fra il registry e il client, cioe' proprio nel tratto che quel test
    /// non attraverserebbe.
    ///
    /// MUTAZIONE: togliere `.with_models_path(...)` dalla factory (oppure riportare
    /// `models_path` a `/models` nella mig 0705) -> il server finto registra
    /// `/models`, `list_models()` ritorna `perplexity HTTP 404` e questo test cade
    /// sul difetto reale.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_lista_modelli_segue_il_percorso_dichiarato_dal_registry(db: PgPool) {
        let (porta, visti) = finge_perplexity("/v1/models").await;

        // La chiave attiva il provider; la base lo punta al server finto. Il
        // PERCORSO non si tocca: e' quello che la migrazione ha scritto.
        for (chiave, valore) in [
            ("perplexity_api_key", "chiave-di-prova".to_string()),
            ("perplexity_enabled", "true".to_string()),
            ("perplexity_base_url", format!("http://127.0.0.1:{porta}")),
        ] {
            sqlx::query(
                "INSERT INTO settings (key, value, category) VALUES ($1, $2, 'providers') \
                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            )
            .bind(chiave)
            .bind(&valore)
            .execute(&db)
            .await
            .expect("settings di prova");
        }

        let descrittori = load_provider_descriptors(&db).await;
        let perplexity = descrittori
            .iter()
            .find(|d| d.name == "perplexity")
            .expect("il registry conosce perplexity dalla mig 0568");
        assert_eq!(
            perplexity.models_path.as_deref(),
            Some("/v1/models"),
            "e' la mig 0705 a dichiararlo: senza, il campo non arriva fin qui"
        );

        let providers = build_providers(&db, &Client::new(), &descrittori).await;
        let provider = providers
            .iter()
            .find(|p| p.name() == "perplexity")
            .expect("chiave presente ed enabled: il provider e' attivo");

        let modelli = provider
            .list_models()
            .await
            .expect("il fornitore risponde 200 sul percorso che ha dichiarato");
        assert_eq!(modelli, vec!["sonar".to_string()]);
        assert!(
            provider.healthcheck().await,
            "lo healthcheck e' la stessa GET: se sbaglia percorso, il fornitore \
             resta 'non sano' per sempre qualunque cosa faccia"
        );

        let interrogati = visti.lock().expect("registro").clone();
        assert!(
            !interrogati.is_empty() && interrogati.iter().all(|p| p == "/v1/models"),
            "il client deve interrogare SOLO il percorso dichiarato, percorsi visti: {interrogati:?}"
        );
    }

    /// L'altro verso: chi non dichiara nulla resta sul `/models` del dialetto
    /// OpenAI. Senza questo, il test sopra passerebbe anche mandando tutti a
    /// `/v1/models`, che romperebbe gli altri otto fornitori.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn chi_non_dichiara_un_percorso_resta_sul_dialetto_openai(db: PgPool) {
        let (porta, visti) = finge_perplexity("/models").await;

        for (chiave, valore) in [
            ("mistral_api_key", "chiave-di-prova".to_string()),
            ("mistral_enabled", "true".to_string()),
            ("mistral_base_url", format!("http://127.0.0.1:{porta}")),
        ] {
            sqlx::query(
                "INSERT INTO settings (key, value, category) VALUES ($1, $2, 'providers') \
                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            )
            .bind(chiave)
            .bind(&valore)
            .execute(&db)
            .await
            .expect("settings di prova");
        }

        let descrittori = load_provider_descriptors(&db).await;
        let providers = build_providers(&db, &Client::new(), &descrittori).await;
        let mistral = providers
            .iter()
            .find(|p| p.name() == "mistral")
            .expect("chiave presente ed enabled");

        assert_eq!(
            mistral.list_models().await.expect("200 su /models"),
            vec!["sonar".to_string()]
        );
        assert_eq!(visti.lock().expect("registro").as_slice(), ["/models"]);
    }

    /// Server finto che registra la TESTA integrale di ogni richiesta (riga di
    /// richiesta + header): e' la sola prova di QUALI header siano partiti
    /// davvero — asserire sul campo del descrittore proverebbe che la
    /// migrazione l'ha scritto, non che qualcuno lo mandi sul wire.
    /// Risponde 200 con una lista modelli al solo `GET /models`, 404 al resto.
    async fn finge_endpoint_che_registra_le_teste(
    ) -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("porta effimera");
        let porta = listener.local_addr().expect("indirizzo").port();
        let teste = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let registro = teste.clone();

        tokio::spawn(async move {
            const CRLF: &str = "\r\n";
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut grezzo = Vec::new();
                let mut buf = [0u8; 1024];
                while !grezzo.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => grezzo.extend_from_slice(&buf[..n]),
                    }
                }
                let testa = String::from_utf8_lossy(&grezzo).to_string();
                let prima_riga = testa.lines().next().unwrap_or_default().to_string();
                registro.lock().expect("registro").push(testa);

                let (stato, corpo) = if prima_riga.starts_with("GET /models") {
                    ("200 OK", r#"{"object":"list","data":[{"id":"z-ai/glm-5.2"}]}"#)
                } else {
                    ("404 Not Found", "")
                };
                let risposta = [
                    &format!("HTTP/1.1 {stato}"),
                    "Content-Type: application/json",
                    &format!("Content-Length: {}", corpo.len()),
                    "Connection: close",
                    "",
                    "",
                ]
                .join(CRLF);
                let _ = socket.write_all(risposta.as_bytes()).await;
                let _ = socket.write_all(corpo.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });

        (porta, teste)
    }

    /// Richiesta chat minima per provare che gli header partono anche sul POST
    /// delle completion, non solo sulle GET di discovery.
    fn richiesta_chat() -> crate::types::LlmRequest {
        use crate::types::{LlmMessage, LlmRequest, MessageContent, RequestMetadata};
        LlmRequest {
            model: "z-ai/glm-5.2".to_string(),
            messages: vec![LlmMessage {
                role: "user".to_string(),
                content: MessageContent::Text("ciao".to_string()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
                reasoning: None,
                is_error: None,
            }],
            temperature: None,
            max_tokens: Some(16),
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: RequestMetadata {
                tenant_id: "t".to_string(),
                user_id: "u".to_string(),
                request_id: "r".to_string(),
                sensitivity_tier: 0,
                feature: "f".to_string(),
            },
            run_timeout_secs: None,
            service_tier: None,
            seed: None,
            stop: None,
            user: None,
            parallel_tool_calls: None,
            deferrable: false,
        }
    }

    /// Gli header di attribuzione dichiarati dal registry partono su OGNI
    /// richiesta. Attraversa la catena intera (regola O): migrazione reale
    /// (0714) -> riga openrouter di `nexus_provider_registry` ->
    /// `load_provider_descriptors` -> factory -> richieste HTTP vere (GET
    /// lista modelli + POST completion). Un test che passasse gli header a
    /// mano al client proverebbe che il client sa applicarli, non che il
    /// registry glieli consegni: il tratto da coprire e' la giunzione.
    ///
    /// MUTAZIONE: togliere `.with_extra_headers(...)` dalla factory, oppure
    /// l'applicazione in `OpenAiCompatClient::con_extra_headers` -> le teste
    /// registrate non portano piu' i due header e il test rosseggia.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn gli_header_di_attribuzione_del_registry_partono_su_ogni_richiesta(db: PgPool) {
        let (porta, teste) = finge_endpoint_che_registra_le_teste().await;

        for (chiave, valore) in [
            ("openrouter_api_key", "chiave-di-prova".to_string()),
            ("openrouter_enabled", "true".to_string()),
            ("openrouter_base_url", format!("http://127.0.0.1:{porta}")),
        ] {
            sqlx::query(
                "INSERT INTO settings (key, value, category) VALUES ($1, $2, 'providers') \
                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            )
            .bind(chiave)
            .bind(&valore)
            .execute(&db)
            .await
            .expect("settings di prova");
        }

        let descrittori = load_provider_descriptors(&db).await;
        let openrouter = descrittori
            .iter()
            .find(|d| d.name == "openrouter")
            .expect("il registry conosce openrouter dalla mig 0567");
        let dichiarati = openrouter
            .extra_headers
            .as_deref()
            .map(parse_extra_headers)
            .unwrap_or_default();
        assert_eq!(
            dichiarati.len(),
            2,
            "e' la mig 0714 a dichiararli: senza, il campo non arriva fin qui"
        );

        let providers = build_providers(&db, &Client::new(), &descrittori).await;
        let provider = providers
            .iter()
            .find(|p| p.name() == "openrouter")
            .expect("chiave presente ed enabled: il provider e' attivo");

        assert_eq!(
            provider.list_models().await.expect("200 su /models"),
            vec!["z-ai/glm-5.2".to_string()]
        );
        // Il POST delle completion prende 404 dal server finto e l'esito qui
        // non conta: conta la testa della richiesta, che parte comunque.
        let _ = provider.complete(&richiesta_chat()).await;

        let registrate = teste.lock().expect("registro").clone();
        assert!(
            registrate.len() >= 2,
            "attese almeno la GET modelli e il POST completion, viste: {}",
            registrate.len()
        );
        for testa in &registrate {
            // hyper serializza i nomi header in minuscolo: si confronta la
            // testa minuscolata, i VALORI della mig 0714 sono gia' minuscoli
            // tranne "Nexus".
            let minuscola = testa.to_lowercase();
            assert!(
                minuscola.contains("http-referer: https://cobracco.it/nexus"),
                "manca HTTP-Referer nella testa: {testa}"
            );
            assert!(
                minuscola.contains("x-title: nexus"),
                "manca X-Title nella testa: {testa}"
            );
        }
    }

    /// L'altro verso: chi non dichiara header extra non ne manda. Senza
    /// questo, il test sopra passerebbe anche applicando i due header a TUTTI
    /// i fornitori, che e' un'attribuzione falsa verso chi non l'ha chiesta.
    /// Groq e non mistral perche' groq passa dallo STESSO ramo generico della
    /// factory: e' li' che una consegna indiscriminata nascerebbe.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn chi_non_dichiara_header_extra_non_ne_manda(db: PgPool) {
        let (porta, teste) = finge_endpoint_che_registra_le_teste().await;

        for (chiave, valore) in [
            ("groq_api_key", "chiave-di-prova".to_string()),
            ("groq_enabled", "true".to_string()),
            ("groq_base_url", format!("http://127.0.0.1:{porta}")),
        ] {
            sqlx::query(
                "INSERT INTO settings (key, value, category) VALUES ($1, $2, 'providers') \
                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            )
            .bind(chiave)
            .bind(&valore)
            .execute(&db)
            .await
            .expect("settings di prova");
        }

        let descrittori = load_provider_descriptors(&db).await;
        let providers = build_providers(&db, &Client::new(), &descrittori).await;
        let groq = providers
            .iter()
            .find(|p| p.name() == "groq")
            .expect("chiave presente ed enabled");
        assert_eq!(
            groq.list_models().await.expect("200 su /models"),
            vec!["z-ai/glm-5.2".to_string()]
        );

        let registrate = teste.lock().expect("registro").clone();
        assert!(!registrate.is_empty());
        for testa in &registrate {
            let minuscola = testa.to_lowercase();
            assert!(
                !minuscola.contains("http-referer:") && !minuscola.contains("x-title:"),
                "un fornitore senza dichiarazione non deve portare header di \
                 attribuzione: {testa}"
            );
        }
    }

    /// Server finto che accetta la POST delle completion, registra il CORPO
    /// ricevuto (testa + Content-Length, poi il body per intero) e risponde
    /// 404: come nel test degli header di attribuzione, l'esito della
    /// chiamata non conta — conta cio' che il client ha DAVVERO spedito.
    async fn finge_endpoint_che_registra_i_corpi(
    ) -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("porta effimera");
        let porta = listener.local_addr().expect("indirizzo").port();
        let corpi = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let registro = corpi.clone();

        tokio::spawn(async move {
            const CRLF: &str = "\r\n";
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut grezzo = Vec::new();
                let mut buf = [0u8; 4096];
                let corpo = loop {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break None,
                        Ok(n) => grezzo.extend_from_slice(&buf[..n]),
                    }
                    if let Some(pos) = grezzo.windows(4).position(|w| w == b"\r\n\r\n") {
                        let testa = String::from_utf8_lossy(&grezzo[..pos]).to_string();
                        let atteso: usize = testa
                            .lines()
                            .filter_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse().ok())
                            })
                            .next()
                            .unwrap_or(0);
                        if grezzo.len() >= pos + 4 + atteso {
                            break Some(
                                String::from_utf8_lossy(&grezzo[pos + 4..pos + 4 + atteso])
                                    .to_string(),
                            );
                        }
                    }
                };
                if let Some(c) = corpo {
                    if !c.is_empty() {
                        registro.lock().expect("registro").push(c);
                    }
                }
                let risposta = [
                    "HTTP/1.1 404 Not Found",
                    "Content-Length: 0",
                    "Connection: close",
                    "",
                    "",
                ]
                .join(CRLF);
                let _ = socket.write_all(risposta.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });

        (porta, corpi)
    }

    /// Registry -> factory -> client -> WIRE: il service_tier dichiarato dalla
    /// colonna (mig 0728) parte su ogni richiesta dell'endpoint. Attraversa la
    /// catena intera (regola O) perche' il tratto scoperto e' proprio la
    /// factory (`construct_provider` -> `with_service_tier`), che nessuno dei
    /// due test parziali — descrittore da un lato, builder->wire dall'altro —
    /// misura.
    ///
    /// Il valore lo semina il test e non la mig del flip: il flip di groq e'
    /// una migrazione DATI futura (oggi il piano dell'org non include flex,
    /// misurato: 400 su ogni tier esplicito), e questo test resta vero con o
    /// senza di essa.
    ///
    /// MUTAZIONE ESEGUITA: togliere `.with_service_tier(...)` dalla factory ->
    /// il descrittore porta 'flex', il corpo spedito non lo porta, rosso.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_service_tier_del_registry_parte_su_ogni_richiesta(db: PgPool) {
        let (porta, corpi) = finge_endpoint_che_registra_i_corpi().await;

        sqlx::query("UPDATE nexus_provider_registry SET service_tier = 'flex' WHERE name = 'groq'")
            .execute(&db)
            .await
            .expect("la colonna esiste dalla mig 0728");
        for (chiave, valore) in [
            ("groq_api_key", "chiave-di-prova".to_string()),
            ("groq_enabled", "true".to_string()),
            ("groq_base_url", format!("http://127.0.0.1:{porta}")),
        ] {
            sqlx::query(
                "INSERT INTO settings (key, value, category) VALUES ($1, $2, 'providers') \
                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            )
            .bind(chiave)
            .bind(&valore)
            .execute(&db)
            .await
            .expect("settings di prova");
        }

        let descrittori = load_provider_descriptors(&db).await;
        let groq = descrittori
            .iter()
            .find(|d| d.name == "groq")
            .expect("il registry conosce groq dalla mig 0566");
        assert_eq!(
            groq.service_tier.as_deref(),
            Some("flex"),
            "senza la colonna (mig 0728) il valore non arriva fin qui"
        );

        let providers = build_providers(&db, &Client::new(), &descrittori).await;
        let provider = providers
            .iter()
            .find(|p| p.name() == "groq")
            .expect("chiave presente ed enabled: il provider e' attivo");

        // Il POST prende 404 dal server finto e l'esito non conta: conta il
        // corpo della richiesta, che parte comunque.
        let _ = provider.complete(&richiesta_chat()).await;

        let registrati = corpi.lock().expect("registro").clone();
        assert!(!registrati.is_empty(), "il server deve aver visto il POST");
        let corpo: serde_json::Value =
            serde_json::from_str(&registrati[0]).expect("il body spedito e' JSON");
        assert_eq!(
            corpo["service_tier"], "flex",
            "il tier del registry deve partire sul wire: {corpo}"
        );
    }

    /// I casi di bordo del parse, che la catena sopra non esercita: valori non
    /// stringa scartati, JSON malformato o non-oggetto = nessun header.
    #[test]
    fn parse_extra_headers_ammette_solo_oggetti_di_stringhe() {
        assert_eq!(
            parse_extra_headers(r#"{"X-Title": "Nexus", "X-Num": 7, "X-Null": null}"#),
            vec![("X-Title".to_string(), "Nexus".to_string())]
        );
        assert!(parse_extra_headers("{").is_empty());
        assert!(parse_extra_headers(r#"["X-Title"]"#).is_empty());
        assert!(parse_extra_headers("{}").is_empty());
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

    // Catalogo dei codici errore fornitore (mig 0705). Se non e' caricabile il
    // gateway NON parte: senza catalogo ogni errore verrebbe classificato dal
    // solo status, cioe' col difetto identico a prima e in SILENZIO. Non e' un
    // modo di morire nuovo — qui non si parte gia' senza DB (`connect` eager in
    // bin/server.rs) ne' senza listino (`assert_configured`).
    let vocabolario_errori = VocabolarioErrori::carica_o_panica(&db).await;

    Ok(AppState {
        db,
        jwt_secret,
        mcp_core_url,
        cooldown,
        vocabolario_errori,
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
