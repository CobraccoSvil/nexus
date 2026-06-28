//! Provider Google (Generative Language REST nativo).
//!
//! Il TS (`providers/google.ts`, 34 righe) instrada Gemini attraverso l'endpoint
//! OpenAI-compatibile di Google delegando a `OpenAIProvider`. Qui implementiamo
//! invece il formato REST NATIVO `generateContent`/`streamGenerateContent`, piu'
//! fedele all'API e testabile in isolamento:
//!   - i messaggi diventano `contents[]` con `role` (`user`/`model`) e `parts[]`;
//!   - il `system` prompt e' un campo separato `systemInstruction`;
//!   - la API key viaggia come query param `?key=...` (convenzione Google);
//!   - lo streaming usa `?alt=sse` con eventi `data: {GenerateContentResponse}`.
//!
//! Regola G: nessun modello hardcoded (arriva da `req.model`, finisce nel path
//! URL). Regola F: mai loggare prompt/response in chiaro.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use nexus_cache::TtlCache;
use reqwest::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio_stream::wrappers::ReceiverStream;

use super::gcp_auth::{
    vertex_action_endpoint, vertex_endpoint, VertexAuth, SETTING_BACKEND,
    SETTING_VERTEX_CREDENTIALS_JSON, SETTING_VERTEX_DISCOVERY_LOCATIONS, SETTING_VERTEX_LOCATION,
    SETTING_VERTEX_PROJECT,
};
use crate::provider::{ChunkStream, LlmProvider};
use crate::types::{
    GeneratedImage, ImageGenRequest, ImageGenResponse, LlmRequest, LlmResponse, LlmStreamChunk,
    LlmToolCall, LlmUsage, MessageContent, SensitivityTier, ToolCallDelta, ToolCallDeltaFunction,
    ToolFunctionCall, VideoGenRequest, VideoGenResponse,
};

/// Tier ammessi: pubblico/interno/confidenziale (mai tier 3, riservato a onprem).
const TIERS: &[SensitivityTier] = &[0, 1, 2];

/// Endpoint REST nativo di Generative Language (override via costruttore).
const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Chiave settings (regola G) del budget thinking Gemini. La stessa letta dal
/// brain Python (`providers.google.thinking_budget`, mig 0407): unica fonte di
/// verita' condivisa tra i due porting.
const THINKING_BUDGET_SETTING: &str = "providers.google.thinking_budget";

/// Budget thinking usato SOLO se il DB e' irraggiungibile e la richiesta ha
/// thinking abilitato (fallback graceful documentato, regola G). Allineato al
/// default 8192 della mig 0407; non e' un "magic default" di routing.
const THINKING_BUDGET_DB_DOWN_FALLBACK: u32 = 8192;

/// Soglia minima di `max_tokens` sotto la quale il thinking resta disattivo
/// (parita' col Python ~489: `if max_tokens >= 256`). Sotto questo valore non
/// c'e' spazio nemmeno per la sola risposta.
const THINKING_MIN_MAX_TOKENS: u32 = 256;

/// Pavimento del budget thinking effettivo (parita' col Python ~490:
/// `max(128, min(_tb_base, max_tokens))`).
const THINKING_BUDGET_FLOOR: u32 = 128;

/// TTL della cache settings (60s, come gli altri provider).
const SETTINGS_TTL: Duration = Duration::from_secs(60);

/// Chiave settings (regola G) del timeout del poll-loop video-gen, in secondi.
/// Letta dal DB (mig 0482): oltre questo tempo il poll-loop si interrompe con
/// errore esplicito (regola H: niente attesa infinita).
const VIDEO_POLL_TIMEOUT_SETTING: &str = "media.video.poll_timeout_s";

/// Timeout del poll-loop video usato SOLO se il setting e' illeggibile dal DB
/// (fallback graceful documentato, regola G/H). Allineato al default '300' della
/// mig 0482; non e' un "magic default" di routing.
const VIDEO_POLL_TIMEOUT_DB_DOWN_FALLBACK: u64 = 300;

/// Intervallo tra due GET di poll dell'operation long-running Veo (~5s). Non e'
/// configurabile: e' un dettaglio di cortesia verso l'API, non una policy di
/// business. Il LIMITE complessivo (timeout) e' invece DB-driven.
const VIDEO_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Location Vertex usata se `google_vertex_location` e' assente in DB. Parita'
/// col brain (`google_provider.py` ~97: default "europe-west4"). NON e' un
/// "magic default" di routing (regola G): e' solo la region geografica di
/// fallback quando l'admin non ne specifica una; project e credenziali restano
/// obbligatori dal DB.
const VERTEX_DEFAULT_LOCATION: &str = "europe-west4";

/// TTL della cache model->region funzionante per l'inference Vertex (mig 0476).
/// Piu' lunga del TTL settings (60s): la mappa di disponibilita' di un modello in
/// una region e' stabile per minuti, evitiamo di ri-pagare il fallback 404 ad
/// ogni richiesta. Allo scadere si ricalcola provando di nuovo dalla prima region.
const VERTEX_REGION_CACHE_TTL: Duration = Duration::from_secs(300);

/// Backend Google risolto dai settings (regola G). `Gemini` usa l'API key in
/// query param (`?key=...`); `Vertex` usa OAuth2 Service Account + endpoint
/// regionale aiplatform.
#[derive(Clone)]
enum GoogleBackend {
    /// API key direct (generativelanguage.googleapis.com).
    Gemini,
    /// Vertex AI: project/location + auth Service Account condivisa.
    Vertex {
        project: String,
        /// Region di prima scelta per l'inference (data-residency UE). Resta il
        /// primo elemento di `discovery_locations`.
        location: String,
        /// Region candidate ORDINATE per preferenza (mig 0476): usate sia per il
        /// discovery (list_models itera su tutte e unisce) sia per il fallback di
        /// region in inference (la prima che risponde non-404 vince). Sempre non
        /// vuoto: se il setting manca, contiene la sola `location`.
        discovery_locations: Vec<String>,
        auth: Arc<VertexAuth>,
    },
}

pub struct GoogleProvider {
    http: Client,
    base_url: String,
    api_key: String,
    db: Option<PgPool>,
    thinking_budget: TtlCache<(), u32>,
    /// Backend risolto dai settings (cache TTL 60s). La cache memorizza un
    /// `Arc<GoogleBackend>` cosi' il `VertexAuth` (e la sua cache token) e'
    /// condiviso tra le richieste invece di ricreare l'auth ad ogni chiamata.
    backend: TtlCache<(), Arc<GoogleBackend>>,
    /// Mappa model -> region Vertex funzionante (mig 0476, TTL 300s). Memorizza la
    /// PRIMA region che ha risposto non-404 per quel modello, cosi' le richieste
    /// successive non ri-pagano il fallback 404. Vuota per il backend Gemini.
    vertex_model_region: TtlCache<String, String>,
}

impl GoogleProvider {
    /// Costruisce il provider senza accesso DB (test di mappatura). Il budget
    /// thinking non sara' leggibile dai settings: il thinking resta disattivo a
    /// meno che la request non porti un `budget_tokens` esplicito.
    pub fn new(http: Client, api_key: impl Into<String>, base_url: Option<String>) -> Self {
        Self::with_db(http, api_key, base_url, None)
    }

    /// Costruisce il provider con accesso DB per leggere il budget thinking dai
    /// settings (regola G). `base_url` opzionale (default Google ufficiale);
    /// `api_key` iniettata dal chiamante (regola F: niente segreti nel codice).
    pub fn with_db(
        http: Client,
        api_key: impl Into<String>,
        base_url: Option<String>,
        db: Option<PgPool>,
    ) -> Self {
        let base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            http,
            base_url,
            api_key: api_key.into(),
            db,
            thinking_budget: TtlCache::new(SETTINGS_TTL),
            backend: TtlCache::new(SETTINGS_TTL),
            vertex_model_region: TtlCache::new(VERTEX_REGION_CACHE_TTL),
        }
    }

    /// Budget thinking di base dai settings (cache TTL 60s). Se il DB e'
    /// irraggiungibile o la chiave assente, ricade sul fallback documentato.
    /// Il valore viene poi validato in [`resolve_thinking`] (clamp + guardia
    /// `max_tokens`).
    async fn configured_thinking_budget(&self) -> u32 {
        if let Some(b) = self.thinking_budget.get(&()) {
            return b;
        }
        let Some(db) = self.db.as_ref() else {
            return THINKING_BUDGET_DB_DOWN_FALLBACK;
        };
        let parsed = nexus_auth::get_setting(db, THINKING_BUDGET_SETTING)
            .await
            .and_then(|v| v.trim().parse::<u32>().ok());
        let budget = parsed.unwrap_or(THINKING_BUDGET_DB_DOWN_FALLBACK);
        self.thinking_budget.insert((), budget);
        budget
    }

    /// Timeout del poll-loop video-gen dai settings (regola G), in secondi. Se il
    /// DB e' irraggiungibile o la chiave assente, ricade sul fallback documentato
    /// (regola H: il LIMITE esiste sempre, mai attesa infinita). Non cachato: e'
    /// letto una sola volta per richiesta (le video-gen sono rare).
    async fn configured_video_poll_timeout_secs(&self) -> u64 {
        let Some(db) = self.db.as_ref() else {
            return VIDEO_POLL_TIMEOUT_DB_DOWN_FALLBACK;
        };
        nexus_auth::get_setting(db, VIDEO_POLL_TIMEOUT_SETTING)
            .await
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|s| *s > 0)
            .unwrap_or(VIDEO_POLL_TIMEOUT_DB_DOWN_FALLBACK)
    }

    /// Risolve il backend Google dai settings (regola G), con cache TTL 60s.
    ///
    /// `google_provider_backend`: "vertex" => Service Account OAuth2; qualunque
    /// altro valore (incluso assente) => "gemini" (API key direct), come il brain
    /// che fa fallback a "gemini" su valori invalidi.
    ///
    /// Per "vertex" legge project/location/credentials dal DB e costruisce un
    /// [`VertexAuth`] condiviso (cache token interna). Se project o credenziali
    /// mancano/sono invalidi, propaga errore (regola G: niente fallback nascosto
    /// ad ADC/env; il chiamante vedra' l'errore e il cooldown lo gestira').
    async fn resolved_backend(&self) -> anyhow::Result<Arc<GoogleBackend>> {
        if let Some(b) = self.backend.get(&()) {
            return Ok(b);
        }
        let Some(db) = self.db.as_ref() else {
            // Senza DB (test di mappatura) il backend e' sempre Gemini con la
            // api_key iniettata.
            let b = Arc::new(GoogleBackend::Gemini);
            self.backend.insert((), b.clone());
            return Ok(b);
        };

        let raw = nexus_auth::get_setting(db, SETTING_BACKEND)
            .await
            .unwrap_or_default();
        let backend = if raw.trim().eq_ignore_ascii_case("vertex") {
            let project = nexus_auth::get_setting(db, SETTING_VERTEX_PROJECT)
                .await
                .unwrap_or_default();
            if project.trim().is_empty() {
                anyhow::bail!(
                    "backend Vertex selezionato ma '{}' vuoto in DB",
                    SETTING_VERTEX_PROJECT
                );
            }
            let location = nexus_auth::get_setting(db, SETTING_VERTEX_LOCATION)
                .await
                .unwrap_or_default();
            let location = if location.trim().is_empty() {
                VERTEX_DEFAULT_LOCATION.to_string()
            } else {
                location.trim().to_string()
            };
            // Region candidate per discovery + fallback inference (mig 0476).
            // CSV ordinato per preferenza; sempre non vuoto (se il setting manca
            // si usa la sola `location`). La `location` di prima scelta e'
            // garantita come PRIMO elemento (anteposta se assente dal CSV) cosi'
            // l'inference parte sempre dalla region UE di data-residency.
            let discovery_raw = nexus_auth::get_setting(db, SETTING_VERTEX_DISCOVERY_LOCATIONS)
                .await
                .unwrap_or_default();
            let discovery_locations = build_discovery_locations(&discovery_raw, &location);
            let credentials = nexus_auth::get_setting(db, SETTING_VERTEX_CREDENTIALS_JSON)
                .await
                .unwrap_or_default();
            if credentials.trim().is_empty() {
                anyhow::bail!(
                    "backend Vertex selezionato ma '{}' vuoto in DB",
                    SETTING_VERTEX_CREDENTIALS_JSON
                );
            }
            let auth = VertexAuth::from_credentials_json(self.http.clone(), &credentials)?;
            tracing::info!(
                project = %project,
                location = %location,
                discovery_locations = %discovery_locations.join(","),
                "google provider: backend vertex"
            );
            GoogleBackend::Vertex {
                project: project.trim().to_string(),
                location,
                discovery_locations,
                auth: Arc::new(auth),
            }
        } else {
            GoogleBackend::Gemini
        };
        let backend = Arc::new(backend);
        self.backend.insert((), backend.clone());
        Ok(backend)
    }

    /// Costruisce la `RequestBuilder` POST verso l'endpoint corretto in base al
    /// backend, con l'auth appropriata (query param `?key=` per Gemini, header
    /// `Authorization: Bearer` per Vertex). Punto unico per `complete`/`stream`
    /// (regola L): l'unico bivio gemini/vertex e' qui.
    ///
    /// Per Vertex la `region` e' un PARAMETRO esplicito (mig 0476): cosi'
    /// l'helper di fallback puo' ricostruire la stessa richiesta su region diverse
    /// senza duplicare la logica di auth/endpoint. Per Gemini `region` e' ignorato
    /// (nessun concetto di region).
    async fn build_post_in_region(
        &self,
        backend: &GoogleBackend,
        region: &str,
        model: &str,
        stream: bool,
    ) -> anyhow::Result<RequestBuilder> {
        match backend {
            GoogleBackend::Gemini => Ok(self
                .http
                .post(self.endpoint(model, stream))
                .query(&[("key", &self.api_key)])),
            GoogleBackend::Vertex {
                project, auth, ..
            } => {
                let token = auth.access_token().await?;
                let url = vertex_endpoint(project, region, model, stream);
                Ok(self.http.post(url).bearer_auth(token))
            }
        }
    }

    /// Lista ORDINATA delle region da provare per un modello sul backend Vertex
    /// (mig 0476): se la cache conosce gia' una region funzionante per quel
    /// modello la usa da sola; altrimenti restituisce tutte le `discovery_locations`
    /// nell'ordine di preferenza. Per il backend Gemini ritorna `None` (nessuna
    /// region: si invia una volta sola sull'endpoint API key).
    fn vertex_regions_for_model(&self, backend: &GoogleBackend, model: &str) -> Option<Vec<String>> {
        let GoogleBackend::Vertex {
            discovery_locations,
            ..
        } = backend
        else {
            return None;
        };
        if let Some(cached) = self.vertex_model_region.get(model) {
            return Some(vec![cached]);
        }
        Some(discovery_locations.clone())
    }

    /// Invia la richiesta con FALLBACK di region per il backend Vertex (mig 0476).
    ///
    /// Prova le region nell'ordine di [`vertex_regions_for_model`]: costruisce la
    /// richiesta con [`build_post_in_region`], la invia e ispeziona lo STATUS
    /// PRIMA di consumare il body. Un 404 significa "modello non disponibile in
    /// quella region": si passa alla successiva. Al primo status non-404 (200 o
    /// errore reale) quella risposta vince; se 2xx la region viene cachata per il
    /// modello (cosi' le richieste successive non ri-pagano i 404). Se TUTTE le
    /// region danno 404 si ritorna l'errore dell'ultima.
    ///
    /// I modelli presenti in europe-west4 (prima region, UE) NON producono 404:
    /// vincono al primo tentativo, comportamento INVARIATO (zero regressione su
    /// gemini-2.5). Il fallback a 'global' scatta solo per i 3.x assenti in UE, e
    /// SOLO se 'global' e' fra le `discovery_locations` (un deploy UE-only la
    /// omette -> nessuna uscita dalla UE).
    ///
    /// Per il backend Gemini (nessuna region) invia una volta sola sull'endpoint
    /// con API key: comportamento identico al passato.
    async fn send_with_region_fallback(
        &self,
        backend: &GoogleBackend,
        model: &str,
        stream: bool,
        body: &GenerateContentRequest,
    ) -> anyhow::Result<reqwest::Response> {
        let Some(regions) = self.vertex_regions_for_model(backend, model) else {
            // Backend Gemini: nessuna region, un solo invio.
            return Ok(self
                .build_post_in_region(backend, "", model, stream)
                .await?
                .json(body)
                .send()
                .await?);
        };

        let mut last: Option<anyhow::Result<reqwest::Response>> = None;
        for region in &regions {
            let resp = self
                .build_post_in_region(backend, region, model, stream)
                .await?
                .json(body)
                .send()
                .await;
            match resp {
                Ok(r) => {
                    if r.status().as_u16() == 404 {
                        // Modello non disponibile in questa region: prova la
                        // successiva (regola G: niente fallback hardcoded, la
                        // lista arriva dal DB). Non logghiamo il body (regola F).
                        tracing::warn!(
                            model = %model,
                            region = %region,
                            "vertex 404: modello assente in region, provo la successiva"
                        );
                        last = Some(Ok(r));
                        continue;
                    }
                    // Primo status non-404: questa risposta vince. Se 2xx, cacha
                    // la region per il modello cosi' le richieste successive
                    // saltano direttamente qui.
                    if r.status().is_success() {
                        self.vertex_model_region
                            .insert(model.to_string(), region.clone());
                    }
                    return Ok(r);
                }
                Err(e) => {
                    // Errore di trasporto (non un 404 applicativo): lo trattiamo
                    // come tentativo fallito e proviamo la region successiva,
                    // conservandolo come ultimo esito.
                    tracing::warn!(
                        model = %model,
                        region = %region,
                        "vertex errore di trasporto, provo la region successiva"
                    );
                    last = Some(Err(anyhow::Error::new(e)));
                }
            }
        }
        // Tutte le region hanno dato 404 (o errore di trasporto): ritorna
        // l'ultimo esito raccolto. `regions` non e' mai vuoto (discovery_locations
        // garantito non vuoto), quindi `last` e' sempre `Some`.
        match last {
            Some(r) => r,
            None => anyhow::bail!("vertex: nessuna region candidata per il modello {model}"),
        }
    }

    /// URL dell'azione per il modello richiesto. `stream=true` usa
    /// `streamGenerateContent?alt=sse`, altrimenti `generateContent`.
    fn endpoint(&self, model: &str, stream: bool) -> String {
        let action = if stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };
        let mut url = format!("{}/models/{}:{}", self.base_url, model, action);
        if stream {
            url.push_str("?alt=sse");
        }
        url
    }
}

#[async_trait]
impl LlmProvider for GoogleProvider {
    fn name(&self) -> &str {
        "google"
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn max_context_tokens(&self) -> u32 {
        1_000_000
    }

    fn tier_compatibility(&self) -> &[SensitivityTier] {
        TIERS
    }

    async fn complete(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
        let backend = self.resolved_backend().await?;
        let configured = self.configured_thinking_budget().await;
        let thinking = resolve_thinking(req, configured);
        let body = build_request_body(req, thinking);
        let start = Instant::now();

        // Invio con fallback di region per Vertex (mig 0476): la prima region
        // non-404 vince; per Gemini e' un singolo invio invariato.
        let resp = self
            .send_with_region_fallback(&backend, &req.model, false, &body)
            .await?;

        let status = resp.status();
        if !status.is_success() {
            // Regola F: body d'errore propagato al caller (cooldown Fase 3 lo
            // classifica via is_billing_error), non loggato qui in chiaro.
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("google HTTP {}: {}", status.as_u16(), text);
        }

        let parsed: GenerateContentResponse = resp.json().await?;
        let latency_ms = start.elapsed().as_millis() as u64;
        Ok(from_generate_response(parsed, req.model.clone(), latency_ms))
    }

    async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        let backend = self.resolved_backend().await?;
        let configured = self.configured_thinking_budget().await;
        let thinking = resolve_thinking(req, configured);
        let body = build_request_body(req, thinking);

        // Fallback di region risolto sullo STATUS HTTP iniziale (mig 0476): il 404
        // arriva prima di qualunque byte di stream, quindi scegliamo la region
        // PRIMA di iniziare a consumare lo stream. Solo a risposta non-404 (e poi
        // 2xx) si avvia il consumo dei bytes piu' sotto.
        let resp = self
            .send_with_region_fallback(&backend, &req.model, true, &body)
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("google HTTP {}: {}", status.as_u16(), text);
        }

        let model_used = req.model.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<anyhow::Result<LlmStreamChunk>>(32);

        tokio::spawn(async move {
            let mut bytes = resp.bytes_stream();
            let mut parser = GoogleSseParser::new(model_used);

            loop {
                match bytes.next().await {
                    Some(Ok(buf)) => parser.push_bytes(&String::from_utf8_lossy(&buf)),
                    Some(Err(e)) => {
                        let _ = tx.send(Err(anyhow::Error::new(e))).await;
                        return;
                    }
                    None => {
                        parser.flush_leftover();
                        while let Some(chunk) = parser.pending.pop_front() {
                            if tx.send(Ok(chunk)).await.is_err() {
                                return;
                            }
                        }
                        return;
                    }
                }

                while let Some(chunk) = parser.pending.pop_front() {
                    if tx.send(Ok(chunk)).await.is_err() {
                        return;
                    }
                }
            }
        });

        Ok(ReceiverStream::new(rx).boxed())
    }

    async fn healthcheck(&self) -> bool {
        // GET /models: 2xx => raggiungibile. Usato anche dal re-probe del cooldown.
        // Per Vertex serve l'auth Bearer e l'endpoint regionale; per Gemini la
        // API key in query param. Se il backend non si risolve (config Vertex
        // mancante/invalida) il provider e' di fatto non operativo => unhealthy.
        let backend = match self.resolved_backend().await {
            Ok(b) => b,
            Err(_) => return false,
        };
        let req = match backend.as_ref() {
            GoogleBackend::Gemini => self
                .http
                .get(format!("{}/models", self.base_url))
                .query(&[("key", &self.api_key)]),
            GoogleBackend::Vertex {
                project,
                location,
                auth,
                ..
            } => {
                let token = match auth.access_token().await {
                    Ok(t) => t,
                    Err(_) => return false,
                };
                let url = format!(
                    "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}/publishers/google/models"
                );
                self.http.get(url).bearer_auth(token)
            }
        };
        match req.send().await {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        // Autodiscovery live per entrambi i backend:
        //   - Gemini: `GET {base_url}/models?key=...`
        //     -> `{ "models": [{ "name": "models/gemini-..." }] }`;
        //   - Vertex: token Bearer su
        //     `GET https://{location}-aiplatform.googleapis.com/v1beta1/publishers/google/models`
        //     -> `{ "publisherModels": [{ "name": "publishers/google/models/gemini-..." }] }`.
        // Il bivio gemini/vertex e' qui (l'auth Vertex riusa `gcp_auth`, regola L);
        // la normalizzazione a basename e' delegata al parser puro
        // [`parse_google_models_response`] (parita' col brain `list_models_live`).
        let backend = self.resolved_backend().await?;
        match backend.as_ref() {
            GoogleBackend::Gemini => {
                let resp = self
                    .http
                    .get(format!("{}/models", self.base_url))
                    .query(&[("key", &self.api_key)])
                    .send()
                    .await?;
                let status = resp.status();
                if !status.is_success() {
                    // Regola F: il body d'errore non contiene prompt/response utente.
                    let text = resp.text().await.unwrap_or_default();
                    anyhow::bail!("google GET models HTTP {}: {}", status.as_u16(), text);
                }
                let body: serde_json::Value = resp.json().await?;
                Ok(parse_google_models_response(&body))
            }
            GoogleBackend::Vertex {
                discovery_locations,
                auth,
                ..
            } => {
                // Discovery multi-region (mig 0476): interroghiamo OGNI region
                // candidata e uniamo i risultati. europe-west4 NON espone i
                // gemini-3.x, 'global' si': iterando le scopriamo entrambe. Una
                // region che fallisce (non-2xx / rete) e' loggata WARN e saltata,
                // senza far fallire l'intero discovery (degrado parziale).
                let token = auth.access_token().await?;
                let mut all: Vec<String> = Vec::new();
                let mut ok_regions = 0usize;
                for region in discovery_locations {
                    let url = format!(
                        "https://{region}-aiplatform.googleapis.com/v1beta1/publishers/google/models"
                    );
                    let resp = self.http.get(url).bearer_auth(&token).send().await;
                    match resp {
                        Ok(r) if r.status().is_success() => {
                            match r.json::<serde_json::Value>().await {
                                Ok(body) => {
                                    all.extend(parse_google_models_response(&body));
                                    ok_regions += 1;
                                }
                                Err(_) => tracing::warn!(
                                    region = %region,
                                    "vertex discovery: risposta models non parsabile, salto"
                                ),
                            }
                        }
                        Ok(r) => tracing::warn!(
                            region = %region,
                            status = r.status().as_u16(),
                            "vertex discovery: GET models non-2xx, salto la region"
                        ),
                        Err(_) => tracing::warn!(
                            region = %region,
                            "vertex discovery: errore di rete su GET models, salto la region"
                        ),
                    }
                }
                if ok_regions == 0 {
                    anyhow::bail!(
                        "vertex discovery: nessuna delle {} region ha risposto",
                        discovery_locations.len()
                    );
                }
                // Dedup mantenendo output deterministico (parita' col parser puro,
                // che ordina+deduplica per ogni singola region; qui ri-uniamo le
                // liste cross-region).
                all.sort();
                all.dedup();
                Ok(all)
            }
        }
    }

    fn supports_image_gen(&self) -> bool {
        true
    }

    /// Genera immagini con Imagen via `:predict`. Riusa `resolved_backend()`
    /// (regola L): per Vertex costruisce l'URL `:predict` con
    /// [`vertex_action_endpoint`] e l'auth Bearer (`VertexAuth::access_token`); per
    /// Gemini API-key usa `{base}/models/{model}:predict?key=`. NON usa il
    /// fallback-region (l'image-gen non e' critico in questo PR): si invia sulla
    /// `location` di prima scelta. Mappa `predictions[].bytesBase64Encoded` ->
    /// [`GeneratedImage`]. Regola G: il `model` arriva dal chiamante.
    async fn generate_image(&self, req: &ImageGenRequest) -> anyhow::Result<ImageGenResponse> {
        let backend = self.resolved_backend().await?;
        let body = build_predict_request(&req.prompt, req.n);
        let start = Instant::now();

        let builder = match backend.as_ref() {
            GoogleBackend::Gemini => self
                .http
                .post(format!("{}/models/{}:predict", self.base_url, req.model))
                .query(&[("key", &self.api_key)]),
            GoogleBackend::Vertex {
                project,
                location,
                auth,
                ..
            } => {
                let token = auth.access_token().await?;
                let url = vertex_action_endpoint(project, location, &req.model, "predict");
                self.http.post(url).bearer_auth(token)
            }
        };

        let resp = builder.json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            // Regola F: body d'errore propagato al caller (cooldown lo classifica
            // via is_billing_error), non loggato qui in chiaro.
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("google HTTP {}: {}", status.as_u16(), text);
        }

        let parsed: PredictResponse = resp.json().await?;
        let latency_ms = start.elapsed().as_millis() as u64;
        Ok(from_predict_response(parsed, req.model.clone(), latency_ms))
    }

    fn supports_video_gen(&self) -> bool {
        true
    }

    /// Genera un video con Veo via `:predictLongRunning` (ASYNC). A differenza
    /// dell'image-gen (`:predict` sincrono) il flusso e' a tre fasi:
    ///   1. START: POST `:predictLongRunning` -> ritorna un `operation name`;
    ///   2. POLL: GET `{operation_name}` in loop ogni ~5s finche' `done:true`
    ///      (timeout DB-driven, regola H: niente attesa infinita);
    ///   3. ESTRAZIONE: dalla `response` dell'operation prende il primo video
    ///      (`bytesBase64Encoded` inline, altrimenti `gcsUri` come URL).
    ///
    /// Backend: SOLO Vertex (`:predictLongRunning` e' un'azione aiplatform). Il
    /// backend Gemini API-key NON espone Veo via questo dialetto -> bail esplicito
    /// (regola H: errore onesto, niente fallback al dialetto sbagliato). Riusa
    /// `resolved_backend()` + `vertex_action_endpoint` + `VertexAuth` (regola L).
    /// Regola G: il `model` arriva dal chiamante.
    async fn generate_video(&self, req: &VideoGenRequest) -> anyhow::Result<VideoGenResponse> {
        let backend = self.resolved_backend().await?;
        let (project, location, auth) = match backend.as_ref() {
            GoogleBackend::Vertex {
                project,
                location,
                auth,
                ..
            } => (project, location, auth),
            GoogleBackend::Gemini => anyhow::bail!(
                "video-generation (Veo) supportata solo dal backend Vertex; \
                 imposta google_provider_backend='vertex' nei settings"
            ),
        };

        let start = Instant::now();
        let poll_timeout = Duration::from_secs(self.configured_video_poll_timeout_secs().await);

        // 1. START: :predictLongRunning -> operation name.
        let token = auth.access_token().await?;
        let body = build_predict_long_running_request(&req.prompt, req.duration_seconds);
        let start_url = vertex_action_endpoint(project, location, &req.model, "predictLongRunning");
        let resp = self
            .http
            .post(start_url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            // Regola F: body d'errore propagato al caller (cooldown lo classifica),
            // non loggato qui in chiaro.
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("google HTTP {}: {}", status.as_u16(), text);
        }
        let start_parsed: LongRunningStartResponse = resp.json().await?;
        let operation_name = start_parsed.name.filter(|s| !s.is_empty()).ok_or_else(|| {
            anyhow::anyhow!(":predictLongRunning non ha restituito un operation name")
        })?;

        // 2. POLL: GET {operation_name} finche' done o timeout (regola H).
        let poll_url = format!(
            "https://{location}-aiplatform.googleapis.com/v1/{operation_name}"
        );
        loop {
            if start.elapsed() >= poll_timeout {
                anyhow::bail!(
                    "video-generation: timeout dopo {}s in attesa dell'operation Veo (setting {})",
                    poll_timeout.as_secs(),
                    VIDEO_POLL_TIMEOUT_SETTING
                );
            }
            tokio::time::sleep(VIDEO_POLL_INTERVAL).await;

            // Token rinfrescato ad ogni giro (la cache interna evita refresh inutili):
            // i poll lunghi possono superare la scadenza del token.
            let poll_token = auth.access_token().await?;
            let poll_resp = self.http.get(&poll_url).bearer_auth(&poll_token).send().await?;
            let poll_status = poll_resp.status();
            if !poll_status.is_success() {
                let text = poll_resp.text().await.unwrap_or_default();
                anyhow::bail!("google HTTP {} (poll): {}", poll_status.as_u16(), text);
            }
            let op: LongRunningOperation = poll_resp.json().await?;
            match parse_operation_response(op)? {
                OperationOutcome::Pending => continue,
                OperationOutcome::Done(video) => {
                    let latency_ms = start.elapsed().as_millis() as u64;
                    return Ok(VideoGenResponse {
                        video_base64: video.video_base64,
                        url: video.url,
                        mime: video.mime,
                        model_used: req.model.clone(),
                        provider_used: "google".to_string(),
                        latency_ms,
                    });
                }
            }
        }
    }
}

/// Costruisce il corpo `:predict` per Imagen: `{instances:[{prompt}], parameters:{sampleCount}}`.
/// `sampleCount` omesso quando `n` e' assente (default lato API).
fn build_predict_request(prompt: &str, n: Option<u32>) -> PredictRequest {
    PredictRequest {
        instances: vec![PredictInstance {
            prompt: prompt.to_string(),
        }],
        parameters: n.map(|sample_count| PredictParameters { sample_count }),
    }
}

/// Mappa una [`PredictResponse`] Imagen nel contratto [`ImageGenResponse`].
/// Funzione PURA (testabile senza rete): `bytesBase64Encoded` -> `b64_json`,
/// `mimeType` -> `mime`. Scarta le prediction senza base64.
fn from_predict_response(
    resp: PredictResponse,
    model_used: String,
    latency_ms: u64,
) -> ImageGenResponse {
    let images = resp
        .predictions
        .into_iter()
        .filter_map(|p| {
            let b64 = p.bytes_base64_encoded.filter(|s| !s.is_empty())?;
            Some(GeneratedImage {
                b64_json: Some(b64),
                url: None,
                mime: p.mime_type.filter(|s| !s.is_empty()),
            })
        })
        .collect();
    ImageGenResponse {
        images,
        model_used,
        provider_used: "google".to_string(),
        latency_ms,
    }
}

/// Costruisce il corpo `:predictLongRunning` per Veo: `{instances:[{prompt}],
/// parameters:{sampleCount:1, durationSeconds?}}`. Funzione PURA (testabile senza
/// rete). `durationSeconds` omesso quando `duration_seconds` e' assente (default
/// lato API). `sampleCount` sempre 1: il tool salva un file per chiamata.
fn build_predict_long_running_request(
    prompt: &str,
    duration_seconds: Option<u32>,
) -> PredictLongRunningRequest {
    PredictLongRunningRequest {
        instances: vec![PredictInstance {
            prompt: prompt.to_string(),
        }],
        parameters: PredictVideoParameters {
            sample_count: 1,
            duration_seconds,
        },
    }
}

/// Video estratto dalla `response` di una operation Veo conclusa.
#[derive(Debug)]
struct ParsedVideo {
    video_base64: Option<String>,
    url: Option<String>,
    mime: String,
}

/// Esito del parsing di un giro di poll dell'operation long-running.
#[derive(Debug)]
enum OperationOutcome {
    /// L'operation non e' ancora conclusa (`done` assente/false): continua il poll.
    Pending,
    /// L'operation e' conclusa con successo: il video estratto.
    Done(ParsedVideo),
}

/// Interpreta una [`LongRunningOperation`] (un giro di poll). Funzione PURA
/// (testabile senza rete):
///   - `done != true` -> [`OperationOutcome::Pending`] (continua il poll);
///   - `error` valorizzato -> `Err` esplicito (regola H: l'errore dell'operation
///     risale onestamente, niente video vuoto);
///   - `done == true` senza error -> estrae il primo video dalla `response`
///     (`bytesBase64Encoded` inline, altrimenti `gcsUri`), `Err` se nessuno dei due.
fn parse_operation_response(op: LongRunningOperation) -> anyhow::Result<OperationOutcome> {
    if let Some(err) = op.error {
        let code = err.code.unwrap_or_default();
        let message = err.message.unwrap_or_default();
        anyhow::bail!("operation Veo fallita (code {code}): {message}");
    }
    if op.done != Some(true) {
        return Ok(OperationOutcome::Pending);
    }
    let response = op
        .response
        .ok_or_else(|| anyhow::anyhow!("operation Veo conclusa ma senza campo 'response'"))?;
    let first = response
        .videos
        .into_iter()
        .find(|v| {
            v.bytes_base64_encoded
                .as_deref()
                .is_some_and(|s| !s.is_empty())
                || v.gcs_uri.as_deref().is_some_and(|s| !s.is_empty())
        })
        .ok_or_else(|| {
            anyhow::anyhow!("operation Veo conclusa ma senza video (ne' bytes ne' gcsUri)")
        })?;
    let mime = first
        .mime_type
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "video/mp4".to_string());
    Ok(OperationOutcome::Done(ParsedVideo {
        video_base64: first.bytes_base64_encoded.filter(|s| !s.is_empty()),
        url: first.gcs_uri.filter(|s| !s.is_empty()),
        mime,
    }))
}

/// Costruisce la lista ORDINATA di region candidate per Vertex (mig 0476) dal
/// CSV del setting `google_vertex_discovery_locations` e dalla `location` di
/// prima scelta. Funzione PURA (regola L, testabile senza rete):
///   - split su ',', trim, scarta i vuoti;
///   - dedup mantenendo l'ordine di prima apparizione;
///   - garantisce `location` come PRIMO elemento (anteposta se assente dal CSV),
///     cosi' l'inference parte sempre dalla region UE di data-residency;
///   - se il CSV non produce alcuna region, ricade su `[location]`.
/// Risultato sempre NON VUOTO.
fn build_discovery_locations(csv: &str, location: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // La region di prima scelta e' sempre la prima candidata.
    let location = location.trim();
    if !location.is_empty() {
        out.push(location.to_string());
    }
    for part in csv.split(',') {
        let region = part.trim();
        if region.is_empty() {
            continue;
        }
        if !out.iter().any(|r| r == region) {
            out.push(region.to_string());
        }
    }
    if out.is_empty() {
        // Ne' location ne' CSV utili: fallback prudente alla default UE cosi' la
        // lista resta sempre non vuota (l'inference ha almeno una region).
        out.push(VERTEX_DEFAULT_LOCATION.to_string());
    }
    out
}

/// Estrae i nomi modello dalla risposta `models.list` di Google e li normalizza
/// a basename. Funzione PURA (regola L, testabile senza rete): gestisce entrambe
/// le forme di risposta, leggendo il campo `name` da:
///   - `models[]` (Gemini direct, es. `"models/gemini-2.5-flash"`);
///   - `publisherModels[]` (Vertex AI, es. `"publishers/google/models/gemini-2.5-flash"`).
/// Normalizza ogni nome al basename (`rsplit('/').next()`), come il brain
/// `list_models_live`; deduplica e ordina per output deterministico.
pub fn parse_google_models_response(body: &serde_json::Value) -> Vec<String> {
    let items = body
        .get("models")
        .and_then(|m| m.as_array())
        .or_else(|| body.get("publisherModels").and_then(|m| m.as_array()));
    let mut names: Vec<String> = items
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("name").and_then(|v| v.as_str()))
                // Normalizza a basename: "publishers/google/models/X" -> "X",
                // "models/X" -> "X", "X" -> "X".
                .filter_map(|name| name.rsplit('/').next())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.dedup();
    names
}

/// Esito della risoluzione del thinking Gemini per una richiesta. `None` =
/// thinking disattivo; `Some(budget)` = `thinkingConfig` con quel budget e tetto
/// di output alzato di conseguenza (fix hollow completion).
type GoogleThinking = Option<u32>;

/// Budget thinking effettivo per la richiesta (parita' col Python ~470-503).
///
/// Replica le guardie del brain:
///   - thinking attivo solo se `req.thinking.enabled`;
///   - budget esplicito nella request ha priorita' su quello configurato;
///   - se `max_tokens` < soglia minima (256), thinking disattivato (troppo poco
///     spazio anche solo per la risposta);
///   - clamp del budget a `max(128, min(budget, max_tokens))`.
fn resolve_thinking(req: &LlmRequest, configured_budget: u32) -> GoogleThinking {
    let enabled = req.thinking.as_ref().is_some_and(|t| t.enabled);
    if !enabled {
        return None;
    }
    // Senza un tetto di output esplicito non sappiamo dimensionare il budget
    // (il Python alza max_output_tokens partendo da max_tokens richiesto): in
    // assenza, evitiamo di attivare il thinking per non rischiare hollow.
    let max_tokens = req.max_tokens?;
    if max_tokens < THINKING_MIN_MAX_TOKENS {
        return None;
    }
    let base = req
        .thinking
        .as_ref()
        .and_then(|t| t.budget_tokens)
        .unwrap_or(configured_budget);
    if base == 0 {
        return None;
    }
    let budget = base.min(max_tokens).max(THINKING_BUDGET_FLOOR);
    Some(budget)
}

/// Rimuove ricorsivamente le chiavi JSON-Schema non supportate da Gemini
/// (parita' col brain `_clean_schema_for_google` -> `compress_schema`,
/// `_SKIP_KEYS` in `brain/providers/_schema_utils.py`). Gemini rifiuta con 400
/// INVALID_ARGUMENT le chiavi di vocabolario JSON-Schema che non implementa
/// (`additionalProperties`, `$schema`, `$defs`, `definitions`, `title`,
/// `default`, `examples`). La ricorsione e' uniforme su `Object`/`Array`, cosi'
/// copre `properties`/`items`/`anyOf`/`oneOf`/`allOf` senza casistica esplicita.
/// Funzione PURA (regola L: nessun equivalente esiste nel gateway), testabile
/// senza rete.
fn clean_schema_for_google(schema: &serde_json::Value) -> serde_json::Value {
    const SKIP: &[&str] = &[
        "additionalProperties",
        "$schema",
        "$defs",
        "definitions",
        "title",
        "default",
        "examples",
    ];
    match schema {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if SKIP.contains(&k.as_str()) {
                    continue;
                }
                out.insert(k.clone(), clean_schema_for_google(v));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(clean_schema_for_google).collect())
        }
        other => other.clone(),
    }
}

/// Costruisce il corpo `GenerateContentRequest`: separa il system come
/// `systemInstruction`, mappa i ruoli (`assistant`->`model`) e impacchetta i
/// parametri di generazione in `generationConfig`.
///
/// `thinking` (gia' risolto da [`resolve_thinking`]): `Some(budget)` attiva il
/// `thinkingConfig` con `includeThoughts=true` e ALZA `maxOutputTokens` di
/// `budget` (fix hollow completion: i token di reasoning sono conteggiati dentro
/// il tetto di output, parita' col Python ~494 `_effective_output_tokens`).
fn build_request_body(req: &LlmRequest, thinking: GoogleThinking) -> GenerateContentRequest {
    let mut system_instruction: Option<GoogleContent> = None;
    let mut contents: Vec<GoogleContent> = Vec::new();

    // Mappa id->name di TUTTE le tool-call in history (parita' col Python
    // `_convert_messages_to_google` ~769-775): Gemini vuole il NOME del tool nel
    // functionResponse, non l'id. Costruita prima del loop cosi' un tool-result
    // puo' risolvere il nome anche se la call e' in un turno precedente.
    let mut id_to_name: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for msg in &req.messages {
        if let Some(calls) = &msg.tool_calls {
            for tc in calls {
                id_to_name.insert(tc.id.clone(), tc.function.name.clone());
            }
        }
    }

    // Buffer functionResponse: Gemini vuole TUTTE le functionResponse di un turno
    // model multi-functionCall raggruppate in UN unico turno user (altrimenti HTTP
    // 400 "number of function response parts is equal to function call parts").
    let mut pending_fn_responses: Vec<GooglePart> = Vec::new();
    for msg in &req.messages {
        if msg.role.as_str() != "tool" && !pending_fn_responses.is_empty() {
            contents.push(GoogleContent {
                role: Some("user".to_string()),
                parts: std::mem::take(&mut pending_fn_responses),
            });
        }
        match msg.role.as_str() {
            "system" => {
                system_instruction = Some(GoogleContent {
                    role: None,
                    parts: vec![GooglePart::text(content_to_string(&msg.content))],
                });
            }
            // Tool-result: Gemini lo vuole come una part `functionResponse` su un
            // messaggio role=`user` (parita' col Python ~818-842, response
            // `{"result": ...}`). Il NOME del tool si risolve dalla mappa
            // id->name; se l'id e' sconosciuto si ripiega sull'id grezzo (Gemini
            // rifiuterebbe il mismatch, ma e' il fallback meno dannoso).
            "tool" => {
                let name = msg
                    .tool_call_id
                    .as_ref()
                    .and_then(|id| id_to_name.get(id).cloned())
                    .or_else(|| msg.tool_call_id.clone())
                    .unwrap_or_default();
                let response = serde_json::json!({ "result": content_to_string(&msg.content) });
                // Accumula: raggruppate sotto in UN turno user (Gemini parity).
                pending_fn_responses.push(GooglePart::function_response(name, response));
            }
            // Assistant con tool-call: emette role=`model` con una part
            // `functionCall` per ciascuna call (parita' col Python ~807-817). La
            // thought_signature va sulla PRIMA part del turno.
            "assistant" if msg.tool_calls.as_ref().is_some_and(|c| !c.is_empty()) => {
                let mut parts: Vec<GooglePart> = Vec::new();
                // Eventuale testo dell'assistant prima delle call (raro).
                if let MessageContent::Text(t) = &msg.content {
                    if !t.is_empty() {
                        parts.push(GooglePart::text(t.clone()));
                    }
                }
                for tc in msg.tool_calls.as_ref().unwrap() {
                    // arguments e' una stringa JSON nel contratto; Gemini vuole un
                    // oggetto in `args` (parita' col mapping Anthropic ~606-608).
                    let args: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    parts.push(GooglePart::function_call(tc.function.name.clone(), args));
                }
                if parts.is_empty() {
                    parts.push(GooglePart::text(String::new()));
                }
                if let Some(first) = parts.first_mut() {
                    first.thought_signature = msg.thinking_signature.clone();
                }
                contents.push(GoogleContent {
                    role: Some("model".to_string()),
                    parts,
                });
            }
            role => {
                // RI-PASSAGGIO thought_signature: se il turno assistant la porta,
                // va riattaccata alla PRIMA part del turno (parita' col Python
                // `_convert_messages_to_google` ~776-798). Obbligatoria su
                // Gemini 3, raccomandata su 2.5. Solo sui turni `model`.
                let signature = if map_role(role) == "model" {
                    msg.thinking_signature.clone()
                } else {
                    None
                };
                let mut parts = content_to_parts(&msg.content);
                // La signature si attacca alla PRIMA part del turno (vuota se
                // assente). `content_to_parts` garantisce almeno una part.
                if let Some(first) = parts.first_mut() {
                    first.thought_signature = signature;
                }
                contents.push(GoogleContent {
                    role: Some(map_role(role).to_string()),
                    parts,
                });
            }
        }
    }
    // Flush finale delle functionResponse accumulate (history che termina con
    // uno o piu' tool-result): stesso raggruppamento in un unico turno user.
    if !pending_fn_responses.is_empty() {
        contents.push(GoogleContent {
            role: Some("user".to_string()),
            parts: std::mem::take(&mut pending_fn_responses),
        });
    }

    // Riconciliazione invariante Gemini (punto unico, regola L): per ogni content
    // `model` con N functionCall, il content `user` successivo deve avere
    // ESATTAMENTE N functionResponse (stesso name/ordine). History interrotte o
    // troncate violano l'invariante e Gemini risponde HTTP 400 INVALID_ARGUMENT
    // ("number of function response parts is equal to function call parts"). La
    // riconciliazione sintetizza i response mancanti e scarta quelli orfani.
    reconcile_function_call_response_pairs(&mut contents);

    // Fix hollow completion: alza il tetto di output del budget thinking cosi'
    // i max_tokens richiesti restano interi per la risposta utente.
    let max_output_tokens = match (req.max_tokens, thinking) {
        (Some(mt), Some(budget)) => Some(mt.saturating_add(budget)),
        (mt, _) => mt,
    };

    let thinking_config = thinking.map(|budget| ThinkingConfigWire {
        include_thoughts: true,
        thinking_budget: budget,
    });

    let generation_config =
        if req.temperature.is_some() || max_output_tokens.is_some() || thinking_config.is_some() {
            Some(GenerationConfig {
                temperature: req.temperature,
                max_output_tokens,
                thinking_config,
            })
        } else {
            None
        };

    // functionDeclarations native Gemini: ogni tool del contratto OpenAI diventa
    // una FunctionDeclaration con lo schema normalizzato al subset Google
    // (clean_schema_for_google). Senza questo blocco, `tool_config` (mode=ANY) e'
    // inerte e il modello emette i control-token nel testo invece di una
    // functionCall. Parita' col brain `google_provider.py` (un solo elemento
    // Tool contenente tutte le declarations) e con openai_compat/anthropic, che
    // dichiarano i tool nel body. NOTA thinking+tools: la policy
    // disable_for_tools vive nel brain (punto unico, regola L); il gateway
    // riceve gia' `req.thinking` risolto e non aggiunge gate qui.
    let tools = req.tools.as_ref().map(|defs| {
        let decls: Vec<GoogleFunctionDeclaration> = defs
            .iter()
            .map(|t| GoogleFunctionDeclaration {
                name: t.function.name.clone(),
                description: t.function.description.clone().unwrap_or_default(),
                // Schema assente/vuoto -> oggetto minimale (parita' Python:427).
                parameters: clean_schema_for_google(&t.function.parameters),
            })
            .collect();
        vec![GoogleToolDecl {
            function_declarations: decls,
        }]
    });

    // tool_choice mappato a `tool_config.function_calling_config.mode` via il
    // punto unico (regola L). Lo iniettiamo SEMPRE quando ci sono `tools`: se il
    // chiamante fornisce un vincolo riconosciuto (auto/required/none/function) si
    // usa quello, altrimenti si applica il DEFAULT ESPLICITO `mode=AUTO`.
    //
    // QUIRK GEMINI (fix definitivo, regola H): i modelli "thinking" (gemini-2.5/
    // 3.x) con `tools` presenti ma SENZA `toolConfig` rispondono in modo NON
    // deterministico con un turno vuoto (zero output token, finishReason STOP,
    // nessuna functionCall) invece di chiamare il tool. Diagnosticato sul
    // tool-probe di mcp-core (`generate_agent_turn` non invia `tool_choice`):
    // ~1 richiesta su 3 tornava vuota -> a soglia tutti i gemini finivano
    // auto-disabilitati con `tool_probe_failed`. Inviando esplicitamente
    // `functionCallingConfig.mode=AUTO` il function calling torna deterministico
    // (3/3 OK in verifica). Senza tool (`req.tools` None) nessun tool_config: un
    // `toolConfig` orfano sarebbe rifiutato da Gemini.
    let tool_config = req.tools.as_ref().map(|_| {
        let function_calling_config = req
            .tool_choice
            .as_ref()
            .and_then(super::tool_choice::to_google_function_calling_config)
            .unwrap_or_else(super::tool_choice::default_google_function_calling_config);
        GoogleToolConfig {
            function_calling_config,
        }
    });

    GenerateContentRequest {
        contents,
        system_instruction,
        generation_config,
        tools,
        tool_config,
    }
}

/// Testo (in inglese, neutro lato API) della risposta sintetica usata per le
/// functionCall rimaste senza il loro functionResponse nella history (run
/// interrotto/troncato). Non e' un dato di business: serve solo a far combaciare
/// il conteggio cosi' Gemini non rifiuta l'intera richiesta con HTTP 400.
const SYNTHETIC_TOOL_RESULT_MESSAGE: &str =
    "tool result missing from history (truncated or interrupted run)";

/// Ripristina l'invariante function-call/function-response richiesta da Gemini
/// (`generateContent`): per OGNI content `role:"model"` che contiene M parts
/// `functionCall`, il content IMMEDIATAMENTE successivo deve essere un
/// `role:"user"` con ESATTAMENTE M parts `functionResponse`, una per functionCall
/// (correlate per `name`, nello stesso ORDINE). Se questa invariante e' violata
/// l'API risponde HTTP 400 INVALID_ARGUMENT ("Please ensure that the number of
/// function response parts is equal to the number of function call parts ...").
///
/// Punto unico (regola L): tutta la riconciliazione vive qui, dopo che
/// [`build_request_body`] ha costruito i `contents`. Funzione PURA (nessun IO,
/// testabile in isolamento) che opera in-place e che e' robusta a tutti i casi
/// scoperti che producevano il 400 in produzione:
///   - functionCall senza functionResponse (call orfana da run troncato): si
///     SINTETIZZA un functionResponse placeholder `{name, response:{error: ...}}`
///     cosi' il conteggio combacia (regola H: non si manda a Google una richiesta
///     destinata a fallire);
///   - functionResponse orfano (name che non corrisponde ad alcuna functionCall
///     del turno model precedente, es. tool_call_id sconosciuto): viene SCARTATO;
///   - functionResponse presenti ma in ordine diverso: riordinati per matchare
///     l'ordine delle functionCall (Gemini correla per name; piu' call con lo
///     stesso name sono consumate FIFO).
///
/// I content senza alcuna functionCall (testo/immagini puri) restano invariati.
///
/// Itera con un indice esplicito perche' puo' INSERIRE un turno user (quando il
/// turno model con functionCall non e' seguito da alcun turno di response): dopo
/// un insert l'indice avanza oltre il turno appena inserito (privo di
/// functionCall), evitando di ri-processarlo.
fn reconcile_function_call_response_pairs(contents: &mut Vec<GoogleContent>) {
    let mut i = 0;
    while i < contents.len() {
        // Estrai i nomi delle functionCall del turno model corrente, in ordine.
        let call_names: Vec<String> = contents[i]
            .parts
            .iter()
            .filter_map(|p| p.function_call.as_ref().map(|fc| fc.name.clone()))
            .collect();
        if call_names.is_empty() {
            i += 1;
            continue;
        }

        // Recupera (consumando) le functionResponse del content successivo, se e'
        // il turno user che le porta. Le indicizziamo per name in code FIFO cosi'
        // piu' call con lo stesso nome consumano response distinte nell'ordine.
        let mut by_name: std::collections::HashMap<String, VecDeque<GooglePart>> =
            std::collections::HashMap::new();
        let has_next_user_responses = contents
            .get(i + 1)
            .is_some_and(|c| c.parts.iter().any(|p| p.function_response.is_some()));
        if has_next_user_responses {
            let next_parts = std::mem::take(&mut contents[i + 1].parts);
            for part in next_parts {
                // Le part non-functionResponse in mezzo ai tool-result (raro)
                // vengono scartate: il turno user di response e' dedicato ad essi.
                if let Some(name) = part.function_response.as_ref().map(|fr| fr.name.clone()) {
                    by_name.entry(name).or_default().push_back(part);
                }
            }
        }

        // Ricostruisci le response NELLO STESSO ORDINE delle call: per ogni call
        // consuma una response con lo stesso name; se non c'e', sintetizzala.
        let mut reconciled: Vec<GooglePart> = Vec::with_capacity(call_names.len());
        for name in &call_names {
            let matched = by_name.get_mut(name).and_then(|q| q.pop_front());
            match matched {
                Some(part) => reconciled.push(part),
                None => reconciled.push(GooglePart::function_response(
                    name.clone(),
                    serde_json::json!({ "error": SYNTHETIC_TOOL_RESULT_MESSAGE }),
                )),
            }
        }
        // Tutte le response rimaste in `by_name` sono orfane (name senza call
        // corrispondente) e vengono SCARTATE: non finiscono nel turno.

        if has_next_user_responses {
            // Sovrascrive il turno user successivo con i response riconciliati.
            contents[i + 1].parts = reconciled;
        } else {
            // Nessun turno user con response dopo il model (history troncata
            // subito dopo le call): inserisce un nuovo turno user di soli
            // response sintetici per ripristinare l'invariante.
            contents.insert(
                i + 1,
                GoogleContent {
                    role: Some("user".to_string()),
                    parts: reconciled,
                },
            );
        }
        // Salta sia il turno model sia il turno user di response (quest'ultimo
        // non contiene functionCall e non va ri-processato come turno model).
        i += 2;
    }

    // Passata finale: scarta le functionResponse che NON sono precedute da un
    // turno model con functionCall (response orfane "di testa", caso degenere:
    // un tool-result come primo messaggio, senza alcuna call). Inviarle a Gemini
    // produrrebbe comunque un 400 (functionResponse senza functionCall). Le parts
    // non-functionResponse dello stesso turno restano; un turno che resta senza
    // parts viene rimosso del tutto per non spedire un content vuoto.
    let mut j = 0;
    while j < contents.len() {
        let prev_is_model_with_calls = j
            .checked_sub(1)
            .and_then(|p| contents.get(p))
            .is_some_and(|c| c.parts.iter().any(|p| p.function_call.is_some()));
        if !prev_is_model_with_calls {
            contents[j].parts.retain(|p| p.function_response.is_none());
            if contents[j].parts.is_empty() {
                contents.remove(j);
                continue;
            }
        }
        j += 1;
    }
}

/// Mappa il ruolo del contratto al ruolo Google: `assistant` -> `model`, tutto
/// il resto (`user`, `tool`) -> `user` (Google non distingue il tool come ruolo
/// separato nel formato base).
fn map_role(role: &str) -> &str {
    match role {
        "assistant" | "model" => "model",
        _ => "user",
    }
}

fn content_to_string(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(s) => s.clone(),
        MessageContent::Blocks(blocks) => serde_json::to_string(blocks).unwrap_or_default(),
    }
}

/// Mappa il content di un messaggio nelle `parts[]` Google. Caso semplice:
/// una sola part di testo. Con blocchi immagine (`image_url`) emette una part
/// `inlineData` (per i data URI base64) o `fileData` (per le URL http), cosi'
/// la capability vision e' preservata (parita' col formato nativo che il brain
/// usa via `Part.from_bytes`). I blocchi non-immagine restano testo.
///
/// Garantisce SEMPRE almeno una part (eventualmente testo vuoto): la signature
/// del thinking va riattaccata alla prima part del turno.
fn content_to_parts(content: &MessageContent) -> Vec<GooglePart> {
    match content {
        MessageContent::Text(s) => vec![GooglePart::text(s.clone())],
        MessageContent::Blocks(blocks) => {
            let has_image = blocks.iter().any(|b| b.kind == "image_url");
            if !has_image {
                // Nessuna immagine: testo serializzato (parita' col TS).
                return vec![GooglePart::text(content_to_string(content))];
            }
            let mut parts: Vec<GooglePart> = Vec::new();
            for b in blocks {
                match b.kind.as_str() {
                    "image_url" => {
                        if let Some(url) = b
                            .image_url
                            .as_ref()
                            .and_then(|iu| iu.get("url"))
                            .and_then(|u| u.as_str())
                        {
                            parts.push(image_url_to_part(url));
                        }
                    }
                    "text" => {
                        if let Some(t) = &b.text {
                            parts.push(GooglePart::text(t.clone()));
                        }
                    }
                    _ => {
                        if let Some(c) = &b.content {
                            parts.push(GooglePart::text(c.clone()));
                        }
                    }
                }
            }
            if parts.is_empty() {
                parts.push(GooglePart::text(String::new()));
            }
            parts
        }
    }
}

/// Converte una `url` di un blocco immagine in una part Google:
///   - `data:<mime>;base64,<dati>` -> `inlineData{mimeType, data}` (base64);
///   - qualunque altra URL (http/https/gs) -> `fileData{mimeType, fileUri}`.
/// Per i data URI malformati ricade su `fileData` con la URL grezza, senza
/// rompere la richiesta.
fn image_url_to_part(url: &str) -> GooglePart {
    if let Some((mime, data)) = parse_data_uri(url) {
        GooglePart::inline_data(mime, data)
    } else {
        // URL remota: Google la scarica via fileData. Il mimeType non e'
        // sempre deducibile dalla URL; quando ignoto si omette (l'API lo
        // inferisce dal contenuto scaricato).
        GooglePart::file_data(mime_from_url(url), url.to_string())
    }
}

/// Estrae `(mime, base64)` da un data URI `data:<mime>;base64,<dati>`. Ritorna
/// `None` se non e' un data URI base64 ben formato.
fn parse_data_uri(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let meta = meta.strip_suffix(";base64")?;
    if meta.is_empty() {
        return None;
    }
    Some((meta.to_string(), data.to_string()))
}

/// Best-effort del mime da estensione URL (solo per `fileData`). `None` se non
/// riconosciuto: l'API Google inferisce comunque dal contenuto.
fn mime_from_url(url: &str) -> Option<String> {
    let lower = url.split('?').next().unwrap_or(url).to_lowercase();
    let mime = if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else {
        return None;
    };
    Some(mime.to_string())
}

/// Mappa una `GenerateContentResponse` nel contratto [`LlmResponse`]: concatena
/// le `parts[].text` del primo candidate e normalizza il `finishReason`.
fn from_generate_response(
    resp: GenerateContentResponse,
    model_used: String,
    latency_ms: u64,
) -> LlmResponse {
    let candidate = resp.candidates.into_iter().next();

    // Separa il testo utente dai "thoughts" (part con `thought=true`): il
    // reasoning interno va in `reasoning`, non nel content (parita' col Python
    // ~575-583). La `thoughtSignature` (gia' base64 nell'API REST) si cattura
    // una sola volta, dovunque appaia (parita' col Python ~567-574).
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut thinking_signature: Option<String> = None;
    let mut tool_calls: Vec<LlmToolCall> = Vec::new();

    if let Some(c) = candidate.as_ref() {
        for part in &c.content.parts {
            if thinking_signature.is_none() {
                if let Some(sig) = &part.thought_signature {
                    if !sig.is_empty() {
                        thinking_signature = Some(sig.clone());
                    }
                }
            }
            // functionCall: il modello chiede di eseguire un tool. Gemini non
            // emette un id di tool-call, ne sintetizziamo uno stabile cosi' il
            // brain puo' correlare il functionResponse nel round-trip (parita'
            // col Python ~589 `toolu_{uuid}`). La part-functionCall non porta
            // testo: passa oltre.
            if let Some(fc) = &part.function_call {
                tool_calls.push(LlmToolCall {
                    id: format!("call_{}", uuid::Uuid::new_v4().simple()),
                    kind: "function".to_string(),
                    function: ToolFunctionCall {
                        name: fc.name.clone(),
                        // Il contratto vuole `arguments` come STRINGA JSON
                        // (parita' Anthropic ~761): serializziamo l'oggetto args.
                        arguments: function_args_to_string(&fc.args),
                    },
                });
                continue;
            }
            if let Some(text) = &part.text {
                if part.thought.unwrap_or(false) {
                    reasoning.push_str(text);
                } else {
                    content.push_str(text);
                }
            }
        }
    }

    // Quando ci sono tool-call, Gemini segnala finishReason=STOP: per parita' col
    // contratto (Anthropic/OpenAI usano "tool_calls"), forziamo il segnale che il
    // brain usa per capire che deve eseguire i tool.
    let finish_reason = if tool_calls.is_empty() {
        map_finish_reason(candidate.as_ref().and_then(|c| c.finish_reason.as_deref()))
    } else {
        "tool_calls".to_string()
    };

    let usage = resp
        .usage_metadata
        .map(|u| LlmUsage {
            input_tokens: u.prompt_token_count,
            output_tokens: u.candidates_token_count,
            cache_read_tokens: u.cached_content_token_count,
            cache_creation_tokens: None,
        })
        .unwrap_or(LlmUsage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: None,
            cache_creation_tokens: None,
        });

    LlmResponse {
        content,
        // functionCall native Gemini -> tool_calls del contratto (parita' coi
        // peer anthropic/openai_compat). `None` quando il turno non chiede tool.
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        usage,
        model_used,
        provider_used: "google".to_string(),
        latency_ms,
        finish_reason,
        privacy_rerouted: None,
        reasoning: if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        },
        thinking_signature,
    }
}

/// Serializza gli `args` di una functionCall Gemini nella STRINGA JSON attesa
/// dal contratto (`ToolFunctionCall.arguments`). Gli args nulli/assenti
/// (Gemini li omette per le funzioni senza parametri) diventano `{}`, mai la
/// stringa `"null"` che il brain non saprebbe deserializzare.
fn function_args_to_string(args: &serde_json::Value) -> String {
    if args.is_null() {
        return "{}".to_string();
    }
    serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string())
}

/// Mappa il `finishReason` Google ai valori canonici del contratto. `STOP` e i
/// valori non noti collassano a `stop`.
fn map_finish_reason(raw: Option<&str>) -> String {
    match raw.unwrap_or("STOP") {
        "MAX_TOKENS" => "length",
        "SAFETY" | "RECITATION" | "PROHIBITED_CONTENT" => "content_filter",
        _ => "stop",
    }
    .to_string()
}

/// Parser SSE Google (`?alt=sse`): ogni riga `data: {GenerateContentResponse}`
/// porta un delta incrementale; l'ultimo evento contiene `usageMetadata` e il
/// `finishReason`. Stateful, testabile senza rete.
struct GoogleSseParser {
    line_buf: String,
    pending: VecDeque<LlmStreamChunk>,
    model_used: String,
}

impl GoogleSseParser {
    fn new(model_used: String) -> Self {
        Self {
            line_buf: String::new(),
            pending: VecDeque::new(),
            model_used,
        }
    }

    fn push_bytes(&mut self, s: &str) {
        self.line_buf.push_str(s);
        while let Some(idx) = self.line_buf.find('\n') {
            let line = self.line_buf[..idx].to_string();
            self.line_buf.drain(..=idx);
            self.parse_line(&line);
        }
    }

    fn flush_leftover(&mut self) {
        let leftover = std::mem::take(&mut self.line_buf);
        for line in leftover.lines() {
            self.parse_line(line);
        }
    }

    fn parse_line(&mut self, line: &str) {
        let line = line.trim_end_matches('\r');
        let payload = match line.strip_prefix("data:") {
            Some(p) => p.trim(),
            None => return,
        };
        if payload.is_empty() {
            return;
        }
        let resp: GenerateContentResponse = match serde_json::from_str(payload) {
            Ok(r) => r,
            Err(_) => return,
        };
        if let Some(chunk) = self.chunk_from_response(resp) {
            self.pending.push_back(chunk);
        }
    }

    fn chunk_from_response(&self, resp: GenerateContentResponse) -> Option<LlmStreamChunk> {
        let candidate = resp.candidates.into_iter().next();

        // Separa testo utente da reasoning (part `thought=true`): il primo va in
        // `delta`, il secondo in `reasoning_delta` (parita' col Python streaming).
        let mut delta = String::new();
        let mut reasoning_delta = String::new();
        // Gemini emette la functionCall completa dentro una part (non frammentata
        // carattere-per-carattere come OpenAI): produciamo un singolo
        // ToolCallDelta gia' completo, prendendo la prima functionCall della part
        // list (parita' con openai_compat che yield-a tc[0]).
        let mut tool_call_delta: Option<ToolCallDelta> = None;
        if let Some(c) = candidate.as_ref() {
            for part in &c.content.parts {
                if tool_call_delta.is_none() {
                    if let Some(fc) = &part.function_call {
                        tool_call_delta = Some(ToolCallDelta {
                            index: 0,
                            id: Some(format!("call_{}", uuid::Uuid::new_v4().simple())),
                            function: Some(ToolCallDeltaFunction {
                                name: Some(fc.name.clone()),
                                // arguments come stringa JSON completa in un colpo
                                // (Gemini non frammenta gli args nello stream).
                                arguments: Some(function_args_to_string(&fc.args)),
                            }),
                        });
                        continue; // la part-functionCall non porta text
                    }
                }
                if let Some(text) = &part.text {
                    if part.thought.unwrap_or(false) {
                        reasoning_delta.push_str(text);
                    } else {
                        delta.push_str(text);
                    }
                }
            }
        }

        // Quando il chunk porta una tool-call, segnaliamo "tool_calls" al consumer
        // (parita' col mapping non-stream): Gemini puo' mandare functionCall con
        // finishReason=STOP (o senza finish nel chunk della call).
        let finish_reason = {
            let mapped = candidate
                .as_ref()
                .and_then(|c| c.finish_reason.as_deref())
                .map(|r| map_finish_reason(Some(r)));
            if tool_call_delta.is_some() {
                match mapped.as_deref() {
                    None | Some("stop") => Some("tool_calls".to_string()),
                    _ => mapped,
                }
            } else {
                mapped
            }
        };

        let usage = resp.usage_metadata.as_ref().map(|u| LlmUsage {
            input_tokens: u.prompt_token_count,
            output_tokens: u.candidates_token_count,
            cache_read_tokens: u.cached_content_token_count,
            cache_creation_tokens: None,
        });

        // Chunk vuoto (nessun delta, nessun reasoning, nessuna tool-call, nessun
        // finish, nessun usage): salta. La tool-call va inclusa nella guardia,
        // altrimenti un chunk con SOLA functionCall verrebbe scartato.
        if delta.is_empty()
            && reasoning_delta.is_empty()
            && tool_call_delta.is_none()
            && finish_reason.is_none()
            && usage.is_none()
        {
            return None;
        }

        // L'usage va riportato solo sul chunk finale (quando c'e' finish).
        let usage = if finish_reason.is_some() { usage } else { None };

        Some(LlmStreamChunk {
            delta,
            tool_call_delta,
            finish_reason,
            usage,
            provider_used: Some("google".to_string()),
            model_used: Some(self.model_used.clone()),
            reasoning_delta: if reasoning_delta.is_empty() {
                None
            } else {
                Some(reasoning_delta)
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Tipi wire (formato Generative Language). Separati dal contratto del gateway.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct GenerateContentRequest {
    contents: Vec<GoogleContent>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GoogleContent>,
    #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
    /// Dichiarazioni di funzione (function calling nativo Gemini). Wrapper
    /// `[{ functionDeclarations: [...] }]`. Presente solo quando `req.tools` e'
    /// valorizzato. Senza questo campo, `toolConfig`(mode=ANY) e' inerte e il
    /// modello emette i control-token nel testo invece di una functionCall.
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GoogleToolDecl>>,
    /// Vincolo di chiamata tool (`tool_config.function_calling_config`). Presente
    /// solo quando il chiamante imposta `tool_choice` e ci sono tool.
    #[serde(rename = "toolConfig", skip_serializing_if = "Option::is_none")]
    tool_config: Option<GoogleToolConfig>,
}

/// `tool_config` del body Gemini: incapsula `function_calling_config` che
/// governa la modalita' di scelta del tool (`mode` + opzionale
/// `allowedFunctionNames`). Wrapper attorno al `Value` prodotto dal punto unico
/// di mapping ([`super::tool_choice::to_google_function_calling_config`]).
#[derive(Debug, Serialize)]
struct GoogleToolConfig {
    #[serde(rename = "functionCallingConfig")]
    function_calling_config: serde_json::Value,
}

/// Elemento `tools[]` del body Gemini: contenitore di functionDeclarations. Il
/// wire vuole `{ "functionDeclarations": [...] }`. Un solo elemento Tool
/// raccoglie tutte le declarations (parita' col Python `google_provider.py`).
#[derive(Debug, Serialize)]
struct GoogleToolDecl {
    #[serde(rename = "functionDeclarations")]
    function_declarations: Vec<GoogleFunctionDeclaration>,
}

/// Singola dichiarazione di funzione (function calling nativo). `parameters` e'
/// lo schema JSON gia' normalizzato al subset Google ([`clean_schema_for_google`]).
#[derive(Debug, Serialize)]
struct GoogleFunctionDeclaration {
    name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct GoogleContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<GooglePart>,
}

/// Part di un messaggio Google. Esattamente uno tra `text`, `inline_data`,
/// `file_data`, `function_call`, `function_response` e' valorizzato (gli altri
/// sono omessi dal wire). La `thought_signature` si attacca alla prima part del
/// turno `model`.
#[derive(Debug, Serialize)]
struct GooglePart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    /// Immagine inline base64 (`{mimeType, data}`), per i data URI.
    #[serde(rename = "inlineData", skip_serializing_if = "Option::is_none")]
    inline_data: Option<GoogleInlineData>,
    /// Riferimento a file remoto (`{mimeType?, fileUri}`), per le URL http.
    #[serde(rename = "fileData", skip_serializing_if = "Option::is_none")]
    file_data: Option<GoogleFileData>,
    /// Chiamata a funzione emessa dal modello in un turno assistant precedente,
    /// ri-passata identica (`{name, args}`) nel round-trip. Mutuamente esclusiva
    /// con text/inlineData/fileData.
    #[serde(rename = "functionCall", skip_serializing_if = "Option::is_none")]
    function_call: Option<GoogleFunctionCallPart>,
    /// Risultato di un tool (`{name, response}`), su una part di un turno `user`.
    #[serde(rename = "functionResponse", skip_serializing_if = "Option::is_none")]
    function_response: Option<GoogleFunctionResponsePart>,
    /// Firma opaca del thinking (base64) ri-passata nei turni successivi. Sul
    /// wire e' `thoughtSignature`; assente quando il turno non la porta.
    #[serde(rename = "thoughtSignature", skip_serializing_if = "Option::is_none")]
    thought_signature: Option<String>,
}

impl GooglePart {
    fn text(text: String) -> Self {
        Self {
            text: Some(text),
            inline_data: None,
            file_data: None,
            function_call: None,
            function_response: None,
            thought_signature: None,
        }
    }

    fn inline_data(mime_type: String, data: String) -> Self {
        Self {
            text: None,
            inline_data: Some(GoogleInlineData { mime_type, data }),
            file_data: None,
            function_call: None,
            function_response: None,
            thought_signature: None,
        }
    }

    fn file_data(mime_type: Option<String>, file_uri: String) -> Self {
        Self {
            text: None,
            inline_data: None,
            file_data: Some(GoogleFileData {
                mime_type,
                file_uri,
            }),
            function_call: None,
            function_response: None,
            thought_signature: None,
        }
    }

    /// Part `functionCall`: ri-passa una tool-call assistant nel round-trip.
    fn function_call(name: String, args: serde_json::Value) -> Self {
        Self {
            text: None,
            inline_data: None,
            file_data: None,
            function_call: Some(GoogleFunctionCallPart { name, args }),
            function_response: None,
            thought_signature: None,
        }
    }

    /// Part `functionResponse`: porta il risultato di un tool nel round-trip.
    fn function_response(name: String, response: serde_json::Value) -> Self {
        Self {
            text: None,
            inline_data: None,
            file_data: None,
            function_call: None,
            function_response: Some(GoogleFunctionResponsePart { name, response }),
            thought_signature: None,
        }
    }
}

/// Part `functionCall` (invio): chiamata a funzione di un turno assistant
/// precedente, ri-passata identica. Gemini vuole `args` come oggetto.
#[derive(Debug, Serialize)]
struct GoogleFunctionCallPart {
    name: String,
    args: serde_json::Value,
}

/// Part `functionResponse` (invio): risultato di un tool. Gemini correla il
/// `name` alla functionCall precedente con lo stesso nome.
#[derive(Debug, Serialize)]
struct GoogleFunctionResponsePart {
    name: String,
    response: serde_json::Value,
}

/// Immagine inline base64 nel formato Gemini (`inlineData`).
#[derive(Debug, Serialize)]
struct GoogleInlineData {
    #[serde(rename = "mimeType")]
    mime_type: String,
    data: String,
}

/// Riferimento a file remoto nel formato Gemini (`fileData`). Il `mimeType` e'
/// opzionale: l'API lo inferisce dal contenuto quando assente.
#[derive(Debug, Serialize)]
struct GoogleFileData {
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    mime_type: Option<String>,
    #[serde(rename = "fileUri")]
    file_uri: String,
}

#[derive(Debug, Serialize)]
struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(rename = "maxOutputTokens", skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    /// Configurazione thinking (`includeThoughts` + `thinkingBudget`). Presente
    /// solo quando il thinking e' attivo per la richiesta.
    #[serde(rename = "thinkingConfig", skip_serializing_if = "Option::is_none")]
    thinking_config: Option<ThinkingConfigWire>,
}

/// `thinkingConfig` del body Gemini. `includeThoughts=true` espone i thoughts
/// nella risposta cosi' il reasoning e' visibile (parita' col Python ~491-493).
#[derive(Debug, Serialize)]
struct ThinkingConfigWire {
    #[serde(rename = "includeThoughts")]
    include_thoughts: bool,
    #[serde(rename = "thinkingBudget")]
    thinking_budget: u32,
}

#[derive(Debug, Deserialize)]
struct GenerateContentResponse {
    #[serde(default)]
    candidates: Vec<GoogleCandidate>,
    #[serde(rename = "usageMetadata", default)]
    usage_metadata: Option<GoogleUsageMetadata>,
}

#[derive(Debug, Deserialize)]
struct GoogleCandidate {
    #[serde(default)]
    content: GoogleRespContent,
    #[serde(rename = "finishReason", default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct GoogleRespContent {
    #[serde(default)]
    parts: Vec<GoogleRespPart>,
}

#[derive(Debug, Deserialize)]
struct GoogleRespPart {
    #[serde(default)]
    text: Option<String>,
    /// `true` se la part e' un "thought" (reasoning interno dei modelli 2.5/3),
    /// da separare dal testo utente.
    #[serde(default)]
    thought: Option<bool>,
    /// Chiamata a funzione emessa dal modello (function calling nativo). Quando
    /// presente, la part NON porta testo: va mappata a un [`LlmToolCall`].
    #[serde(rename = "functionCall", default)]
    function_call: Option<GoogleFunctionCall>,
    /// Firma opaca del thinking (base64) emessa da Gemini: va catturata e
    /// rispedita identica nei turni successivi.
    #[serde(rename = "thoughtSignature", default)]
    thought_signature: Option<String>,
}

/// `functionCall` emessa da Gemini nella risposta (`{name, args}`). Gli `args`
/// arrivano gia' come oggetto JSON strutturato (non stringa come OpenAI).
#[derive(Debug, Deserialize)]
struct GoogleFunctionCall {
    #[serde(default)]
    name: String,
    #[serde(default)]
    args: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct GoogleUsageMetadata {
    #[serde(rename = "promptTokenCount", default)]
    prompt_token_count: u32,
    #[serde(rename = "candidatesTokenCount", default)]
    candidates_token_count: u32,
    /// Token serviti dall'implicit caching Gemini 2.5+ (sottoinsieme di
    /// `promptTokenCount`). Presente solo a cache hit.
    #[serde(rename = "cachedContentTokenCount", default)]
    cached_content_token_count: Option<u32>,
}

// --- Image generation Imagen (`:predict`) ----------------------------------

/// Corpo della richiesta `:predict` Imagen.
#[derive(Debug, Serialize)]
struct PredictRequest {
    instances: Vec<PredictInstance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<PredictParameters>,
}

#[derive(Debug, Serialize)]
struct PredictInstance {
    prompt: String,
}

#[derive(Debug, Serialize)]
struct PredictParameters {
    #[serde(rename = "sampleCount")]
    sample_count: u32,
}

/// Risposta `:predict` Imagen: `{ "predictions": [{ "bytesBase64Encoded", "mimeType" }] }`.
#[derive(Debug, Deserialize)]
struct PredictResponse {
    #[serde(default)]
    predictions: Vec<PredictPrediction>,
}

#[derive(Debug, Deserialize)]
struct PredictPrediction {
    #[serde(rename = "bytesBase64Encoded", default)]
    bytes_base64_encoded: Option<String>,
    #[serde(rename = "mimeType", default)]
    mime_type: Option<String>,
}

// --- Video generation Veo (`:predictLongRunning` + poll) -------------------

/// Corpo della richiesta `:predictLongRunning` Veo. Riusa [`PredictInstance`]
/// (stesso `{prompt}` dell'image-gen). `parameters` sempre presente (almeno
/// `sampleCount`).
#[derive(Debug, Serialize)]
struct PredictLongRunningRequest {
    instances: Vec<PredictInstance>,
    parameters: PredictVideoParameters,
}

#[derive(Debug, Serialize)]
struct PredictVideoParameters {
    #[serde(rename = "sampleCount")]
    sample_count: u32,
    #[serde(rename = "durationSeconds", skip_serializing_if = "Option::is_none")]
    duration_seconds: Option<u32>,
}

/// Risposta di START `:predictLongRunning`: `{ "name": "projects/.../operations/<id>" }`.
#[derive(Debug, Deserialize)]
struct LongRunningStartResponse {
    #[serde(default)]
    name: Option<String>,
}

/// Risposta di POLL `GET {operation_name}`: `{ "done": bool, "response": {...},
/// "error": {...} }`. `done` assente => operation ancora in corso.
#[derive(Debug, Deserialize)]
struct LongRunningOperation {
    #[serde(default)]
    done: Option<bool>,
    #[serde(default)]
    response: Option<LongRunningVideoResponse>,
    #[serde(default)]
    error: Option<LongRunningError>,
}

/// `response` di una operation Veo conclusa. Veo espone i video sotto
/// `videos[]` (alcune versioni usano `generatedSamples[]`/`predictions[]`): per
/// l'MVP leggiamo `videos[]`, la forma documentata della Vertex Veo API.
#[derive(Debug, Deserialize)]
struct LongRunningVideoResponse {
    #[serde(default)]
    videos: Vec<LongRunningVideo>,
}

#[derive(Debug, Deserialize)]
struct LongRunningVideo {
    #[serde(rename = "bytesBase64Encoded", default)]
    bytes_base64_encoded: Option<String>,
    #[serde(rename = "gcsUri", default)]
    gcs_uri: Option<String>,
    #[serde(rename = "mimeType", default)]
    mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LongRunningError {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LlmMessage, RequestMetadata};

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            tenant_id: "t".to_string(),
            user_id: "u".to_string(),
            request_id: "r".to_string(),
            sensitivity_tier: 0,
            feature: "f".to_string(),
        }
    }

    fn msg(role: &str, text: &str) -> LlmMessage {
        LlmMessage {
            role: role.to_string(),
            content: MessageContent::Text(text.to_string()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
        }
    }

    #[test]
    fn capacita_dichiarate() {
        let p = GoogleProvider::new(Client::new(), "key", None);
        assert_eq!(p.name(), "google");
        assert!(p.supports_streaming());
        assert_eq!(p.max_context_tokens(), 1_000_000);
        assert_eq!(p.tier_compatibility(), &[0, 1, 2]);
    }

    #[test]
    fn parse_models_gemini_normalizza_basename() {
        // Forma Gemini direct: `models[]` con name "models/<id>".
        let body = serde_json::json!({
            "models": [
                { "name": "models/gemini-2.5-flash" },
                { "name": "models/gemini-2.5-pro" },
                { "name": "models/gemini-2.5-flash" }, // duplicato
            ]
        });
        let models = parse_google_models_response(&body);
        assert_eq!(models, vec!["gemini-2.5-flash", "gemini-2.5-pro"]);
    }

    #[test]
    fn parse_models_vertex_normalizza_basename() {
        // Forma Vertex: `publisherModels[]` con name "publishers/google/models/<id>".
        let body = serde_json::json!({
            "publisherModels": [
                { "name": "publishers/google/models/gemini-2.0-flash-exp" },
                { "name": "publishers/google/models/gemini-2.5-pro" },
            ]
        });
        let models = parse_google_models_response(&body);
        assert_eq!(models, vec!["gemini-2.0-flash-exp", "gemini-2.5-pro"]);
    }

    #[test]
    fn parse_models_google_gestisce_assenze() {
        // Niente `models` ne' `publisherModels`: lista vuota, non panico.
        let vuoto = serde_json::json!({ "nextPageToken": "abc" });
        assert!(parse_google_models_response(&vuoto).is_empty());

        // Name assente/vuoto: scartato.
        let body = serde_json::json!({
            "models": [
                { "name": "models/gemini-x" },
                { "object": "model" },
                { "name": "" },
            ]
        });
        assert_eq!(parse_google_models_response(&body), vec!["gemini-x"]);
    }

    #[test]
    fn endpoint_streaming_aggiunge_alt_sse() {
        let p = GoogleProvider::new(Client::new(), "key", None);
        let url = p.endpoint("gemini-x", true);
        assert!(url.ends_with("/models/gemini-x:streamGenerateContent?alt=sse"));
        let url2 = p.endpoint("gemini-x", false);
        assert!(url2.ends_with("/models/gemini-x:generateContent"));
    }

    #[tokio::test]
    async fn backend_senza_db_e_sempre_gemini() {
        // Senza DB (costruttore `new`) il backend non e' configurabile e ricade
        // su Gemini con la api_key iniettata: la build_post deve usare ?key=.
        let p = GoogleProvider::new(Client::new(), "k", None);
        let backend = p.resolved_backend().await.expect("backend gemini");
        assert!(matches!(*backend, GoogleBackend::Gemini));
    }

    #[test]
    fn predict_request_imagen_instances_e_sample_count() {
        let body = build_predict_request("un gatto", Some(2));
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["instances"][0]["prompt"], "un gatto");
        assert_eq!(json["parameters"]["sampleCount"], 2);
        // n assente -> parameters omesso (default lato API).
        let body2 = build_predict_request("un cane", None);
        let json2 = serde_json::to_value(&body2).unwrap();
        assert!(json2.get("parameters").is_none());
    }

    #[test]
    fn predict_response_mappa_bytes_base64_e_mime() {
        let raw = r#"{
            "predictions": [
                {"bytesBase64Encoded": "AAAA", "mimeType": "image/png"},
                {"bytesBase64Encoded": ""}
            ]
        }"#;
        let parsed: PredictResponse = serde_json::from_str(raw).unwrap();
        let out = from_predict_response(parsed, "imagen-3.0".to_string(), 5);
        assert_eq!(out.provider_used, "google");
        assert_eq!(out.model_used, "imagen-3.0");
        // La prediction con base64 vuoto e' scartata.
        assert_eq!(out.images.len(), 1);
        assert_eq!(out.images[0].b64_json.as_deref(), Some("AAAA"));
        assert_eq!(out.images[0].mime.as_deref(), Some("image/png"));
    }

    #[test]
    fn predict_long_running_request_veo_instances_e_parameters() {
        let body = build_predict_long_running_request("un drone sul mare", Some(8));
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["instances"][0]["prompt"], "un drone sul mare");
        assert_eq!(json["parameters"]["sampleCount"], 1);
        assert_eq!(json["parameters"]["durationSeconds"], 8);
        // duration assente -> durationSeconds omesso, sampleCount comunque presente.
        let body2 = build_predict_long_running_request("una citta'", None);
        let json2 = serde_json::to_value(&body2).unwrap();
        assert_eq!(json2["parameters"]["sampleCount"], 1);
        assert!(json2["parameters"].get("durationSeconds").is_none());
    }

    #[test]
    fn parse_operation_pending_quando_done_assente_o_false() {
        // done assente -> Pending.
        let op: LongRunningOperation = serde_json::from_str(r#"{}"#).unwrap();
        assert!(matches!(
            parse_operation_response(op).unwrap(),
            OperationOutcome::Pending
        ));
        // done:false -> Pending.
        let op2: LongRunningOperation = serde_json::from_str(r#"{"done": false}"#).unwrap();
        assert!(matches!(
            parse_operation_response(op2).unwrap(),
            OperationOutcome::Pending
        ));
    }

    #[test]
    fn parse_operation_done_estrae_bytes_e_mime() {
        let raw = r#"{
            "done": true,
            "response": {
                "videos": [
                    {"bytesBase64Encoded": "QUJD", "mimeType": "video/mp4"}
                ]
            }
        }"#;
        let op: LongRunningOperation = serde_json::from_str(raw).unwrap();
        let OperationOutcome::Done(video) = parse_operation_response(op).unwrap() else {
            panic!("attesa operation conclusa");
        };
        assert_eq!(video.video_base64.as_deref(), Some("QUJD"));
        assert_eq!(video.url, None);
        assert_eq!(video.mime, "video/mp4");
    }

    #[test]
    fn parse_operation_done_gcs_uri_senza_bytes() {
        // Solo gcsUri (niente bytes inline): url valorizzata, mime di default.
        let raw = r#"{
            "done": true,
            "response": {
                "videos": [
                    {"gcsUri": "gs://bucket/out.mp4"}
                ]
            }
        }"#;
        let op: LongRunningOperation = serde_json::from_str(raw).unwrap();
        let OperationOutcome::Done(video) = parse_operation_response(op).unwrap() else {
            panic!("attesa operation conclusa");
        };
        assert_eq!(video.video_base64, None);
        assert_eq!(video.url.as_deref(), Some("gs://bucket/out.mp4"));
        assert_eq!(video.mime, "video/mp4");
    }

    #[test]
    fn parse_operation_error_propaga_errore_esplicito() {
        let raw = r#"{"error": {"code": 7, "message": "permission denied"}}"#;
        let op: LongRunningOperation = serde_json::from_str(raw).unwrap();
        let err = parse_operation_response(op).unwrap_err().to_string();
        assert!(err.contains("permission denied"), "err = {err}");
        assert!(err.contains("code 7"), "err = {err}");
    }

    #[test]
    fn parse_operation_done_senza_video_e_errore() {
        let raw = r#"{"done": true, "response": {"videos": []}}"#;
        let op: LongRunningOperation = serde_json::from_str(raw).unwrap();
        assert!(parse_operation_response(op).is_err());
    }

    #[test]
    fn system_estratto_in_system_instruction() {
        let req = LlmRequest {
            model: "gemini-x".to_string(),
            messages: vec![msg("system", "istruzione"), msg("user", "domanda")],
            temperature: Some(0.5),
            max_tokens: Some(500),
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
        };
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();

        assert_eq!(json["systemInstruction"]["parts"][0]["text"], "istruzione");
        // Solo lo user finisce in contents.
        assert_eq!(json["contents"].as_array().unwrap().len(), 1);
        assert_eq!(json["contents"][0]["role"], "user");
        assert_eq!(json["contents"][0]["parts"][0]["text"], "domanda");
        assert_eq!(json["generationConfig"]["temperature"], 0.5);
        assert_eq!(json["generationConfig"]["maxOutputTokens"], 500);
    }

    fn search_tool() -> crate::types::LlmToolDefinition {
        crate::types::LlmToolDefinition {
            kind: "function".to_string(),
            function: crate::types::ToolFunctionDef {
                name: "search".to_string(),
                description: Some("cerca".to_string()),
                parameters: serde_json::json!({"type": "object"}),
                strict: None,
            },
        }
    }

    fn req_tool_choice(choice: serde_json::Value, with_tools: bool) -> LlmRequest {
        LlmRequest {
            model: "gemini-x".to_string(),
            messages: vec![msg("user", "trova")],
            temperature: None,
            max_tokens: Some(500),
            tools: if with_tools {
                Some(vec![search_tool()])
            } else {
                None
            },
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: Some(choice),
            pin_provider: None,
            metadata: metadata(),
        }
    }

    #[test]
    fn tool_choice_required_diventa_mode_any() {
        // "required" -> tool_config.functionCallingConfig.mode = ANY.
        let req = req_tool_choice(serde_json::json!("required"), true);
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        assert_eq!(
            json["toolConfig"]["functionCallingConfig"]["mode"],
            "ANY"
        );

        // Oggetto funzione -> mode ANY + allowedFunctionNames.
        let req2 = req_tool_choice(
            serde_json::json!({"type": "function", "function": {"name": "search"}}),
            true,
        );
        let json2 = serde_json::to_value(build_request_body(&req2, None)).unwrap();
        assert_eq!(json2["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
        assert_eq!(
            json2["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"][0],
            "search"
        );

        // "none" -> mode NONE.
        let req3 = req_tool_choice(serde_json::json!("none"), true);
        let json3 = serde_json::to_value(build_request_body(&req3, None)).unwrap();
        assert_eq!(json3["toolConfig"]["functionCallingConfig"]["mode"], "NONE");
    }

    #[test]
    fn tool_choice_senza_tools_non_aggiunge_tool_config() {
        let req = req_tool_choice(serde_json::json!("required"), false);
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        assert!(json.get("toolConfig").is_none());
    }

    #[test]
    fn assistant_mappato_su_model() {
        let req = LlmRequest {
            model: "gemini-x".to_string(),
            messages: vec![msg("assistant", "risposta precedente")],
            temperature: None,
            max_tokens: None,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
        };
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        assert_eq!(json["contents"][0]["role"], "model");
        // Nessun parametro di generazione -> generationConfig assente.
        assert!(json.get("generationConfig").is_none());
    }

    #[test]
    fn deserializza_response() {
        let raw = r#"{
            "candidates": [{
                "content": {"parts": [{"text": "Ciao "}, {"text": "mondo"}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 11, "candidatesTokenCount": 4}
        }"#;
        let parsed: GenerateContentResponse = serde_json::from_str(raw).unwrap();
        let resp = from_generate_response(parsed, "gemini-x".to_string(), 33);

        assert_eq!(resp.content, "Ciao mondo");
        assert_eq!(resp.finish_reason, "stop");
        assert_eq!(resp.usage.input_tokens, 11);
        assert_eq!(resp.usage.output_tokens, 4);
        assert_eq!(resp.provider_used, "google");
    }

    #[test]
    fn finish_reason_mappato() {
        assert_eq!(map_finish_reason(Some("STOP")), "stop");
        assert_eq!(map_finish_reason(Some("MAX_TOKENS")), "length");
        assert_eq!(map_finish_reason(Some("SAFETY")), "content_filter");
        assert_eq!(map_finish_reason(Some("boh")), "stop");
        assert_eq!(map_finish_reason(None), "stop");
    }

    #[test]
    fn sse_delta_emette_chunk() {
        let mut p = GoogleSseParser::new("gemini-x".to_string());
        p.parse_line(r#"data: {"candidates":[{"content":{"parts":[{"text":"Hel"}]}}]}"#);
        assert_eq!(p.pending.len(), 1);
        assert_eq!(p.pending[0].delta, "Hel");
        assert!(p.pending[0].finish_reason.is_none());
        assert_eq!(p.pending[0].provider_used.as_deref(), Some("google"));
    }

    #[test]
    fn sse_chunk_finale_riporta_usage() {
        let mut p = GoogleSseParser::new("gemini-x".to_string());
        p.parse_line(
            r#"data: {"candidates":[{"content":{"parts":[{"text":"."}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":2}}"#,
        );
        let chunk = p.pending.pop_back().expect("chunk finale");
        assert_eq!(chunk.delta, ".");
        assert_eq!(chunk.finish_reason.as_deref(), Some("stop"));
        let usage = chunk.usage.expect("usage finale");
        assert_eq!(usage.input_tokens, 9);
        assert_eq!(usage.output_tokens, 2);
    }

    #[test]
    fn sse_riga_parziale_gestita() {
        let mut p = GoogleSseParser::new("gemini-x".to_string());
        p.push_bytes(r#"data: {"candidates":[{"content":{"parts":[{"te"#);
        assert_eq!(p.pending.len(), 0);
        p.push_bytes("xt\":\"ok\"}]}}]}\n");
        assert_eq!(p.pending.len(), 1);
        assert_eq!(p.pending[0].delta, "ok");
    }

    // --- Extended thinking (passo 2) ---------------------------------------

    fn req_thinking(
        enabled: bool,
        budget: Option<u32>,
        max_tokens: Option<u32>,
        messages: Vec<LlmMessage>,
    ) -> LlmRequest {
        LlmRequest {
            model: "gemini-x".to_string(),
            messages,
            temperature: None,
            max_tokens,
            tools: None,
            response_format: None,
            stream: None,
            thinking: Some(crate::types::ThinkingConfig {
                enabled,
                budget_tokens: budget,
            }),
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
        }
    }

    #[test]
    fn thinking_attivo_aggiunge_config_e_alza_output() {
        // budget esplicito 2048, max_tokens 8000 -> thinking attivo, output alzato.
        let req = req_thinking(true, Some(2048), Some(8000), vec![msg("user", "ciao")]);
        let thinking = resolve_thinking(&req, 8192);
        assert_eq!(thinking, Some(2048));
        let json = serde_json::to_value(build_request_body(&req, thinking)).unwrap();
        assert_eq!(json["generationConfig"]["thinkingConfig"]["includeThoughts"], true);
        assert_eq!(json["generationConfig"]["thinkingConfig"]["thinkingBudget"], 2048);
        // Fix hollow: maxOutputTokens = max_tokens + budget.
        assert_eq!(json["generationConfig"]["maxOutputTokens"], 8000 + 2048);
    }

    #[test]
    fn thinking_disattivo_non_aggiunge_config() {
        let req = req_thinking(false, Some(2048), Some(8000), vec![msg("user", "ciao")]);
        let thinking = resolve_thinking(&req, 8192);
        assert_eq!(thinking, None);
        let json = serde_json::to_value(build_request_body(&req, thinking)).unwrap();
        assert!(json["generationConfig"].get("thinkingConfig").is_none());
        // Output non alzato: resta il max_tokens richiesto.
        assert_eq!(json["generationConfig"]["maxOutputTokens"], 8000);
    }

    #[test]
    fn thinking_budget_usa_configurato_e_clampa() {
        // Budget configurato 50000 > max_tokens 4000 -> clamp a max_tokens.
        let req = req_thinking(true, None, Some(4000), vec![msg("user", "x")]);
        assert_eq!(resolve_thinking(&req, 50_000), Some(4000));
        // max_tokens sotto soglia minima -> thinking disattivato.
        let req2 = req_thinking(true, Some(1024), Some(100), vec![msg("user", "x")]);
        assert_eq!(resolve_thinking(&req2, 8192), None);
        // Nessun max_tokens -> non dimensionabile -> disattivato.
        let req3 = req_thinking(true, Some(1024), None, vec![msg("user", "x")]);
        assert_eq!(resolve_thinking(&req3, 8192), None);
    }

    #[test]
    fn round_trip_thought_signature_su_part_model() {
        // Un turno assistant con thinking_signature deve produrre la
        // thoughtSignature sulla part del messaggio `model`.
        let mut a = msg("assistant", "ho ragionato");
        a.thinking_signature = Some("c2lnLWdlbWluaQ==".to_string());
        let req = LlmRequest {
            model: "gemini-x".to_string(),
            messages: vec![a],
            temperature: None,
            max_tokens: None,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
        };
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        assert_eq!(json["contents"][0]["role"], "model");
        assert_eq!(
            json["contents"][0]["parts"][0]["thoughtSignature"],
            "c2lnLWdlbWluaQ=="
        );
    }

    #[test]
    fn signature_su_user_non_viene_ripassata() {
        // La signature appartiene ai turni `model`: su uno user e' ignorata
        // (mai inviata su una part `user`, non avrebbe senso lato API).
        let mut u = msg("user", "domanda");
        u.thinking_signature = Some("spuria".to_string());
        let req = LlmRequest {
            model: "gemini-x".to_string(),
            messages: vec![u],
            temperature: None,
            max_tokens: None,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
        };
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        assert!(json["contents"][0]["parts"][0]
            .get("thoughtSignature")
            .is_none());
    }

    #[test]
    fn deserializza_response_con_thought_e_signature() {
        let raw = r#"{
            "candidates": [{
                "content": {"parts": [
                    {"text": "rifletto", "thought": true},
                    {"text": "risposta utente", "thoughtSignature": "c2lnLXJlc3A="}
                ]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 12, "candidatesTokenCount": 5, "cachedContentTokenCount": 8}
        }"#;
        let parsed: GenerateContentResponse = serde_json::from_str(raw).unwrap();
        let resp = from_generate_response(parsed, "gemini-x".to_string(), 9);

        // Il thought NON entra nel content utente.
        assert_eq!(resp.content, "risposta utente");
        assert_eq!(resp.reasoning.as_deref(), Some("rifletto"));
        assert_eq!(resp.thinking_signature.as_deref(), Some("c2lnLXJlc3A="));
        assert_eq!(resp.usage.cache_read_tokens, Some(8));
    }

    #[test]
    fn response_senza_thought_ha_reasoning_none() {
        let raw = r#"{
            "candidates": [{"content": {"parts": [{"text": "solo risposta"}]}, "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
        }"#;
        let parsed: GenerateContentResponse = serde_json::from_str(raw).unwrap();
        let resp = from_generate_response(parsed, "m".to_string(), 0);
        assert!(resp.reasoning.is_none());
        assert!(resp.thinking_signature.is_none());
        assert!(resp.usage.cache_read_tokens.is_none());
    }

    #[test]
    fn sse_thought_emette_reasoning_delta() {
        let mut p = GoogleSseParser::new("gemini-x".to_string());
        p.parse_line(
            r#"data: {"candidates":[{"content":{"parts":[{"text":"penso...","thought":true}]}}]}"#,
        );
        assert_eq!(p.pending.len(), 1);
        assert_eq!(p.pending[0].reasoning_delta.as_deref(), Some("penso..."));
        // Il delta testuale resta vuoto sul chunk di reasoning.
        assert_eq!(p.pending[0].delta, "");
    }

    // --- Vision: parts inlineData / fileData (passo 3) ---------------------

    fn image_block(url: &str) -> LlmMessage {
        LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Blocks(vec![
                crate::types::LlmContentBlock {
                    kind: "text".to_string(),
                    text: Some("descrivi".to_string()),
                    image_url: None,
                    tool_use_id: None,
                    content: None,
                },
                crate::types::LlmContentBlock {
                    kind: "image_url".to_string(),
                    text: None,
                    image_url: Some(serde_json::json!({ "url": url })),
                    tool_use_id: None,
                    content: None,
                },
            ]),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
        }
    }

    fn req_with(msg: LlmMessage) -> LlmRequest {
        LlmRequest {
            model: "gemini-x".to_string(),
            messages: vec![msg],
            temperature: None,
            max_tokens: None,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
        }
    }

    #[test]
    fn vision_data_uri_diventa_inline_data() {
        let req = req_with(image_block("data:image/png;base64,QUJD"));
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        let parts = json["contents"][0]["parts"].as_array().unwrap();
        // Prima part: testo; seconda: inlineData con mimeType+data.
        assert_eq!(parts[0]["text"], "descrivi");
        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
        assert_eq!(parts[1]["inlineData"]["data"], "QUJD");
        // Niente text spurio sulla part immagine.
        assert!(parts[1].get("text").is_none());
    }

    #[test]
    fn vision_url_http_diventa_file_data() {
        let req = req_with(image_block("https://example.com/foto.jpg"));
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        let parts = json["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts[1]["fileData"]["fileUri"], "https://example.com/foto.jpg");
        assert_eq!(parts[1]["fileData"]["mimeType"], "image/jpeg");
        assert!(parts[1].get("inlineData").is_none());
    }

    #[test]
    fn parse_data_uri_estrae_mime_e_dati() {
        assert_eq!(
            parse_data_uri("data:image/webp;base64,XYZ"),
            Some(("image/webp".to_string(), "XYZ".to_string()))
        );
        // Non base64 / non data URI -> None.
        assert!(parse_data_uri("https://x/y.png").is_none());
        assert!(parse_data_uri("data:image/png,raw").is_none());
    }

    #[test]
    fn vision_signature_su_prima_part_con_immagine() {
        // La signature del thinking si attacca alla PRIMA part anche quando il
        // turno e' multimodale (testo + immagine).
        let mut msg = image_block("data:image/png;base64,QUJD");
        msg.role = "assistant".to_string();
        msg.thinking_signature = Some("c2ln".to_string());
        let req = req_with(msg);
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        let parts = json["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(json["contents"][0]["role"], "model");
        assert_eq!(parts[0]["thoughtSignature"], "c2ln");
        assert!(parts[1].get("thoughtSignature").is_none());
    }

    // --- Function calling nativo (passo tool) ------------------------------

    fn read_file_tool() -> crate::types::LlmToolDefinition {
        crate::types::LlmToolDefinition {
            kind: "function".to_string(),
            function: crate::types::ToolFunctionDef {
                name: "read_file".to_string(),
                description: Some("legge un file".to_string()),
                parameters: serde_json::json!({
                    "type": "object",
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "title": "ReadFileArgs",
                    "additionalProperties": false,
                    "properties": {
                        "path": {
                            "type": "string",
                            "title": "Path",
                            "default": "."
                        },
                        "lines": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": { "n": { "type": "integer" } }
                            }
                        }
                    },
                    "required": ["path"]
                }),
                strict: None,
            },
        }
    }

    fn req_with_tools(messages: Vec<LlmMessage>, tools: bool) -> LlmRequest {
        LlmRequest {
            model: "gemini-2.5-pro".to_string(),
            messages,
            temperature: None,
            max_tokens: Some(1024),
            tools: if tools {
                Some(vec![read_file_tool()])
            } else {
                None
            },
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
        }
    }

    #[test]
    fn tools_diventano_function_declarations() {
        // I tool del contratto finiscono in tools[0].functionDeclarations con
        // nome/descrizione/parametri (parita' coi peer openai_compat/anthropic).
        let req = req_with_tools(vec![msg("user", "leggi x")], true);
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        let decls = &json["tools"][0]["functionDeclarations"];
        assert_eq!(decls[0]["name"], "read_file");
        assert_eq!(decls[0]["description"], "legge un file");
        assert_eq!(decls[0]["parameters"]["type"], "object");
        assert_eq!(decls[0]["parameters"]["properties"]["path"]["type"], "string");
    }

    #[test]
    fn schema_pulito_rimuove_chiavi_non_supportate() {
        // Le chiavi JSON-Schema non supportate da Gemini vengono rimosse a TUTTI
        // i livelli di annidamento (properties + items), non solo alla radice.
        let req = req_with_tools(vec![msg("user", "leggi x")], true);
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        let params = &json["tools"][0]["functionDeclarations"][0]["parameters"];
        // Radice ripulita.
        assert!(params.get("$schema").is_none());
        assert!(params.get("title").is_none());
        assert!(params.get("additionalProperties").is_none());
        // Property annidata ripulita (title/default rimossi, type preservato).
        let path = &params["properties"]["path"];
        assert_eq!(path["type"], "string");
        assert!(path.get("title").is_none());
        assert!(path.get("default").is_none());
        // items annidato in array ripulito.
        let nested = &params["properties"]["lines"]["items"];
        assert!(nested.get("additionalProperties").is_none());
        assert_eq!(nested["properties"]["n"]["type"], "integer");
        // required (vocabolario supportato) preservato.
        assert_eq!(params["required"][0], "path");
    }

    #[test]
    fn clean_schema_funzione_pura() {
        // Test diretto della funzione pura: chiavi note rimosse, struttura
        // preservata.
        let raw = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "$defs": { "X": {} },
            "definitions": { "Y": {} },
            "examples": [1, 2],
            "properties": {
                "a": { "type": "string", "default": "z" }
            }
        });
        let cleaned = clean_schema_for_google(&raw);
        assert_eq!(cleaned["type"], "object");
        assert!(cleaned.get("additionalProperties").is_none());
        assert!(cleaned.get("$defs").is_none());
        assert!(cleaned.get("definitions").is_none());
        assert!(cleaned.get("examples").is_none());
        assert_eq!(cleaned["properties"]["a"]["type"], "string");
        assert!(cleaned["properties"]["a"].get("default").is_none());
    }

    #[test]
    fn tool_choice_required_con_tools_emette_sia_tool_config_sia_tools() {
        // Rafforza il test storico: con tool_choice="required" + tools, ora il
        // body porta SIA toolConfig(mode=ANY) SIA le functionDeclarations (prima
        // le declarations mancavano e mode=ANY era inerte).
        let mut req = req_with_tools(vec![msg("user", "trova")], true);
        req.tool_choice = Some(serde_json::json!("required"));
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        assert_eq!(json["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
        assert_eq!(
            json["tools"][0]["functionDeclarations"][0]["name"],
            "read_file"
        );
    }

    #[test]
    fn tools_senza_tool_choice_emette_tool_config_auto() {
        // QUIRK GEMINI (regressione): con tools presenti ma SENZA tool_choice
        // (caso del tool-probe mcp-core via generate_agent_turn), il body DEVE
        // comunque portare toolConfig(mode=AUTO). Senza questo, i modelli
        // "thinking" Gemini rispondono in modo non deterministico con un turno
        // vuoto invece di chiamare il tool -> auto-disable dei gemini.
        let req = req_with_tools(vec![msg("user", "leggi x")], true);
        assert!(req.tool_choice.is_none(), "il setup deve omettere tool_choice");
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        assert_eq!(json["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
        // Le functionDeclarations restano presenti accanto al toolConfig.
        assert_eq!(
            json["tools"][0]["functionDeclarations"][0]["name"],
            "read_file"
        );
    }

    #[test]
    fn senza_tools_niente_campo_tools() {
        let req = req_with_tools(vec![msg("user", "ciao")], false);
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        assert!(json.get("tools").is_none());
        // Coerenza: senza tools nemmeno il toolConfig (un toolConfig orfano
        // sarebbe rifiutato da Gemini).
        assert!(json.get("toolConfig").is_none());
    }

    #[test]
    fn response_function_call_diventa_tool_calls() {
        // Una part functionCall nella risposta -> tool_calls valorizzato, args
        // serializzati a stringa JSON, finish_reason forzato a "tool_calls".
        let raw = r#"{
            "candidates": [{
                "content": {"parts": [
                    {"functionCall": {"name": "read_file", "args": {"path": "src/main.rs"}}}
                ]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 3}
        }"#;
        let parsed: GenerateContentResponse = serde_json::from_str(raw).unwrap();
        let resp = from_generate_response(parsed, "gemini-2.5-pro".to_string(), 12);

        assert_eq!(resp.finish_reason, "tool_calls");
        let calls = resp.tool_calls.expect("tool_calls valorizzato");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[0].kind, "function");
        assert!(calls[0].id.starts_with("call_"));
        // arguments e' una STRINGA JSON deserializzabile.
        let args: serde_json::Value =
            serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "src/main.rs");
        // Nessun testo: content vuoto.
        assert_eq!(resp.content, "");
    }

    #[test]
    fn response_function_call_senza_args_diventa_oggetto_vuoto() {
        // Funzione senza parametri: args assente -> arguments "{}".
        let raw = r#"{
            "candidates": [{
                "content": {"parts": [{"functionCall": {"name": "ping"}}]},
                "finishReason": "STOP"
            }]
        }"#;
        let parsed: GenerateContentResponse = serde_json::from_str(raw).unwrap();
        let resp = from_generate_response(parsed, "m".to_string(), 0);
        let calls = resp.tool_calls.expect("tool_calls");
        assert_eq!(calls[0].function.arguments, "{}");
    }

    #[test]
    fn sse_function_call_emette_tool_call_delta() {
        // Stream: una part functionCall in un evento SSE produce un
        // tool_call_delta completo + finish_reason "tool_calls".
        let mut p = GoogleSseParser::new("gemini-2.5-pro".to_string());
        p.parse_line(
            r#"data: {"candidates":[{"content":{"parts":[{"functionCall":{"name":"read_file","args":{"path":"a.txt"}}}]},"finishReason":"STOP"}]}"#,
        );
        assert_eq!(p.pending.len(), 1);
        let chunk = &p.pending[0];
        let tcd = chunk.tool_call_delta.as_ref().expect("tool_call_delta");
        assert_eq!(tcd.index, 0);
        assert!(tcd.id.as_deref().unwrap().starts_with("call_"));
        let f = tcd.function.as_ref().unwrap();
        assert_eq!(f.name.as_deref(), Some("read_file"));
        let args: serde_json::Value =
            serde_json::from_str(f.arguments.as_deref().unwrap()).unwrap();
        assert_eq!(args["path"], "a.txt");
        // finish forzato a tool_calls.
        assert_eq!(chunk.finish_reason.as_deref(), Some("tool_calls"));
        // Nessun delta testuale.
        assert_eq!(chunk.delta, "");
    }

    #[test]
    fn sse_solo_function_call_senza_finish_non_scartata() {
        // Un chunk con SOLA functionCall (senza finish/text/usage) NON deve
        // essere scartato dalla guardia chunk-vuoto.
        let mut p = GoogleSseParser::new("m".to_string());
        p.parse_line(
            r#"data: {"candidates":[{"content":{"parts":[{"functionCall":{"name":"f","args":{}}}]}}]}"#,
        );
        assert_eq!(p.pending.len(), 1);
        assert!(p.pending[0].tool_call_delta.is_some());
        // Senza finish nel chunk, lo forziamo comunque a tool_calls.
        assert_eq!(p.pending[0].finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn round_trip_assistant_tool_calls_diventa_function_call_part() {
        // Un turno assistant con tool_calls -> role=model con part functionCall
        // (args deserializzato da stringa a oggetto).
        let mut a = msg("assistant", "");
        a.tool_calls = Some(vec![LlmToolCall {
            id: "call_x".to_string(),
            kind: "function".to_string(),
            function: ToolFunctionCall {
                name: "read_file".to_string(),
                arguments: r#"{"path":"x.rs"}"#.to_string(),
            },
        }]);
        let req = req_with_tools(vec![a], true);
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        assert_eq!(json["contents"][0]["role"], "model");
        let part = &json["contents"][0]["parts"][0];
        assert_eq!(part["functionCall"]["name"], "read_file");
        assert_eq!(part["functionCall"]["args"]["path"], "x.rs");
        // Niente text spurio sulla part-functionCall.
        assert!(part.get("text").is_none());
    }

    #[test]
    fn round_trip_tool_result_diventa_function_response_con_nome_risolto() {
        // Un turno tool (tool_call_id) -> role=user con functionResponse il cui
        // `name` e' risolto dalla tool-call precedente (non l'id grezzo).
        let mut a = msg("assistant", "");
        a.tool_calls = Some(vec![LlmToolCall {
            id: "call_x".to_string(),
            kind: "function".to_string(),
            function: ToolFunctionCall {
                name: "read_file".to_string(),
                arguments: "{}".to_string(),
            },
        }]);
        let mut tool = msg("tool", "contenuto del file");
        tool.tool_call_id = Some("call_x".to_string());
        let req = req_with_tools(vec![a, tool], true);
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        // Secondo content: il tool-result.
        let tr = &json["contents"][1];
        assert_eq!(tr["role"], "user");
        let part = &tr["parts"][0];
        // name risolto a "read_file", NON "call_x".
        assert_eq!(part["functionResponse"]["name"], "read_file");
        assert_eq!(
            part["functionResponse"]["response"]["result"],
            "contenuto del file"
        );
    }

    #[test]
    fn round_trip_tool_result_orfano_senza_call_scartato() {
        // Un tool-result SENZA alcun turno model con functionCall che lo preceda
        // (id sconosciuto, history priva della call): la riconciliazione lo
        // SCARTA invece di inviarlo a Gemini come functionResponse orfano (che
        // produrrebbe comunque HTTP 400). Il turno user, rimasto senza parts,
        // viene rimosso del tutto: il body non ha contents.
        let mut tool = msg("tool", "out");
        tool.tool_call_id = Some("call_orfano".to_string());
        let req = req_with_tools(vec![tool], true);
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();
        let contents = json["contents"].as_array().unwrap();
        assert!(
            contents.is_empty(),
            "il functionResponse orfano di testa va scartato, contents = {contents:?}"
        );
    }

    // --- Discovery + fallback di region Vertex (mig 0476) ------------------

    #[test]
    fn discovery_locations_da_csv_ordinato_e_dedup() {
        // CSV ordinato per preferenza; la location (UE) resta prima, duplicati
        // rimossi mantenendo l'ordine di prima apparizione.
        let regions = build_discovery_locations("europe-west4,global,europe-west4", "europe-west4");
        assert_eq!(regions, vec!["europe-west4", "global"]);
    }

    #[test]
    fn discovery_locations_anteposta_location_se_assente_dal_csv() {
        // Se il CSV non include la location di prima scelta, va comunque ANTEPOSTA
        // (l'inference parte sempre dalla region UE di data-residency).
        let regions = build_discovery_locations("global,us-central1", "europe-west4");
        assert_eq!(regions, vec!["europe-west4", "global", "us-central1"]);
    }

    #[test]
    fn discovery_locations_csv_vuoto_usa_sola_location() {
        // Setting assente/vuoto -> si usa la sola location (nessun fallback fuori
        // UE in un deploy che non configura il CSV).
        let regions = build_discovery_locations("", "europe-west4");
        assert_eq!(regions, vec!["europe-west4"]);
        // Spazi e separatori spuri vengono ripuliti.
        let regions2 = build_discovery_locations(" , ,  ", "europe-west4");
        assert_eq!(regions2, vec!["europe-west4"]);
    }

    #[test]
    fn discovery_locations_ue_only_non_include_global() {
        // Deploy UE-only: CSV = sola europe-west4 -> nessuna region fuori UE,
        // quindi in inference non c'e' alcun fallback a 'global'.
        let regions = build_discovery_locations("europe-west4", "europe-west4");
        assert_eq!(regions, vec!["europe-west4"]);
        assert!(!regions.iter().any(|r| r == "global"));
    }

    #[test]
    fn discovery_locations_mai_vuoto() {
        // Caso limite: ne' location ne' CSV -> fallback alla default UE, lista
        // sempre non vuota (l'inference ha almeno una region da provare).
        let regions = build_discovery_locations("", "");
        assert_eq!(regions, vec!["europe-west4"]);
    }

    #[test]
    fn vertex_regions_gemini_ritorna_none() {
        // Backend Gemini: nessuna region -> None (invio singolo su API key).
        let p = GoogleProvider::new(Client::new(), "k", None);
        assert!(p
            .vertex_regions_for_model(&GoogleBackend::Gemini, "gemini-x")
            .is_none());
    }

    #[test]
    fn vertex_regions_usa_cache_se_presente() {
        // Se la cache conosce una region per il modello, e' l'unica provata
        // (le richieste successive saltano il fallback 404).
        let p = GoogleProvider::new(Client::new(), "k", None);
        let backend = GoogleBackend::Vertex {
            project: "proj".to_string(),
            location: "europe-west4".to_string(),
            discovery_locations: vec!["europe-west4".to_string(), "global".to_string()],
            auth: Arc::new(
                VertexAuth::from_credentials_json(Client::new(), &sample_sa_json()).unwrap(),
            ),
        };
        // Prima del cache hit: tutte le region candidate, in ordine.
        let before = p
            .vertex_regions_for_model(&backend, "gemini-3.5-flash")
            .unwrap();
        assert_eq!(before, vec!["europe-west4", "global"]);
        // Dopo aver cachato 'global' per quel modello: solo 'global'.
        p.vertex_model_region
            .insert("gemini-3.5-flash".to_string(), "global".to_string());
        let after = p
            .vertex_regions_for_model(&backend, "gemini-3.5-flash")
            .unwrap();
        assert_eq!(after, vec!["global"]);
        // Un modello diverso resta sull'ordine di discovery completo.
        let other = p.vertex_regions_for_model(&backend, "gemini-2.5-pro").unwrap();
        assert_eq!(other, vec!["europe-west4", "global"]);
    }

    fn sample_sa_json() -> String {
        serde_json::json!({
            "type": "service_account",
            "project_id": "nexus-test",
            "private_key": "-----BEGIN PRIVATE KEY-----\nFAKE\n-----END PRIVATE KEY-----\n",
            "client_email": "nexus-sa@nexus-test.iam.gserviceaccount.com"
        })
        .to_string()
    }

    // --- Riconciliazione invariante functionCall/functionResponse (Gemini) ---

    /// Helper: turno assistant con N tool-call (name, id) per i test di
    /// riconciliazione. Gli arguments sono un oggetto vuoto serializzato.
    fn assistant_with_calls(calls: &[(&str, &str)]) -> LlmMessage {
        let mut a = msg("assistant", "");
        a.tool_calls = Some(
            calls
                .iter()
                .map(|(name, id)| LlmToolCall {
                    id: (*id).to_string(),
                    kind: "function".to_string(),
                    function: ToolFunctionCall {
                        name: (*name).to_string(),
                        arguments: "{}".to_string(),
                    },
                })
                .collect(),
        );
        a
    }

    /// Helper: turno tool-result correlato a una tool-call via `tool_call_id`.
    fn tool_result(tool_call_id: &str, text: &str) -> LlmMessage {
        let mut t = msg("tool", text);
        t.tool_call_id = Some(tool_call_id.to_string());
        t
    }

    /// Helper: estrae i name delle functionResponse di un content (turno user).
    fn response_names(content: &serde_json::Value) -> Vec<String> {
        content["parts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p["functionResponse"]["name"].as_str().map(String::from))
            .collect()
    }

    #[test]
    fn reconcile_due_call_un_result_sintetizza_il_mancante() {
        // CASO PRIMARIO osservato in prod: il turno model emette 2 functionCall
        // ma la history (run troncato) porta un solo tool-result -> dopo la
        // riconciliazione il turno user ha 2 functionResponse (1 sintetico),
        // cosi' il conteggio combacia e Gemini non risponde 400.
        let a = assistant_with_calls(&[("read_file", "call_1"), ("edit_file", "call_2")]);
        let t = tool_result("call_1", "contenuto");
        let req = req_with_tools(vec![a, t], true);
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();

        // contents[0] = model con 2 functionCall.
        let model_parts = json["contents"][0]["parts"].as_array().unwrap();
        let call_count = model_parts
            .iter()
            .filter(|p| p.get("functionCall").is_some())
            .count();
        assert_eq!(call_count, 2, "il turno model deve avere 2 functionCall");
        assert_eq!(json["contents"][0]["role"], "model");

        // contents[1] = user con 2 functionResponse (invariante ripristinata).
        let user = &json["contents"][1];
        assert_eq!(user["role"], "user");
        let names = response_names(user);
        assert_eq!(names, vec!["read_file", "edit_file"]);
        // Il primo e' il result reale; il secondo e' il placeholder sintetico.
        assert_eq!(
            user["parts"][0]["functionResponse"]["response"]["result"],
            "contenuto"
        );
        assert_eq!(
            user["parts"][1]["functionResponse"]["response"]["error"],
            SYNTHETIC_TOOL_RESULT_MESSAGE
        );
    }

    #[test]
    fn reconcile_tool_result_orfano_scartato() {
        // Un tool-result il cui name NON corrisponde ad alcuna functionCall del
        // turno model precedente e' orfano: va SCARTATO (altrimenti gonfia il
        // conteggio e Gemini risponde 400). Qui la call e' "read_file" ma in
        // history arriva ANCHE un result per "call_xxx" sconosciuto.
        let a = assistant_with_calls(&[("read_file", "call_1")]);
        let t_ok = tool_result("call_1", "ok");
        // Result orfano: tool_call_id non presente fra le call -> il name si
        // risolve all'id grezzo "call_orfano", che non matcha "read_file".
        let t_orfano = tool_result("call_orfano", "spurio");
        let req = req_with_tools(vec![a, t_ok, t_orfano], true);
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();

        // Un solo turno user, con UNA sola functionResponse (quella valida): la
        // orfana e' stata scartata.
        let user = &json["contents"][1];
        assert_eq!(user["role"], "user");
        let names = response_names(user);
        assert_eq!(names, vec!["read_file"]);
        assert_eq!(user["parts"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn reconcile_caso_normale_invariato() {
        // N functionCall == N functionResponse, stesso ordine: nessuna
        // sintesi, nessuno scarto, l'output e' quello atteso (idempotente).
        let a = assistant_with_calls(&[("read_file", "call_1"), ("list_dir", "call_2")]);
        let t1 = tool_result("call_1", "file");
        let t2 = tool_result("call_2", "elenco");
        let req = req_with_tools(vec![a, t1, t2], true);
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();

        let user = &json["contents"][1];
        let names = response_names(user);
        assert_eq!(names, vec!["read_file", "list_dir"]);
        assert_eq!(
            user["parts"][0]["functionResponse"]["response"]["result"],
            "file"
        );
        assert_eq!(
            user["parts"][1]["functionResponse"]["response"]["result"],
            "elenco"
        );
        // Esattamente 2 content: nessun turno spurio inserito.
        assert_eq!(json["contents"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn reconcile_call_senza_alcun_result_inserisce_turno_user_sintetico() {
        // History che termina con il turno model di sole functionCall e NESSUN
        // turno di response (run interrotto subito dopo le call): la
        // riconciliazione deve INSERIRE un turno user di soli response sintetici.
        let a = assistant_with_calls(&[("read_file", "call_1"), ("edit_file", "call_2")]);
        let req = req_with_tools(vec![a], true);
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();

        assert_eq!(json["contents"].as_array().unwrap().len(), 2);
        assert_eq!(json["contents"][0]["role"], "model");
        let user = &json["contents"][1];
        assert_eq!(user["role"], "user");
        let names = response_names(user);
        assert_eq!(names, vec!["read_file", "edit_file"]);
        // Entrambe sintetiche (nessun result reale in history).
        for k in 0..2 {
            assert_eq!(
                user["parts"][k]["functionResponse"]["response"]["error"],
                SYNTHETIC_TOOL_RESULT_MESSAGE
            );
        }
    }

    #[test]
    fn reconcile_response_in_ordine_diverso_riallineate_alle_call() {
        // I tool-result arrivano in ordine inverso rispetto alle call: la
        // riconciliazione li riallinea all'ordine delle functionCall (Gemini
        // correla per name nello stesso ordine del turno model).
        let a = assistant_with_calls(&[("alpha", "call_a"), ("beta", "call_b")]);
        let t_beta = tool_result("call_b", "risultato-beta");
        let t_alpha = tool_result("call_a", "risultato-alpha");
        let req = req_with_tools(vec![a, t_beta, t_alpha], true);
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();

        let user = &json["contents"][1];
        let names = response_names(user);
        // Ordine allineato alle call (alpha, beta), non a quello dei result.
        assert_eq!(names, vec!["alpha", "beta"]);
        assert_eq!(
            user["parts"][0]["functionResponse"]["response"]["result"],
            "risultato-alpha"
        );
        assert_eq!(
            user["parts"][1]["functionResponse"]["response"]["result"],
            "risultato-beta"
        );
    }

    #[test]
    fn reconcile_assistant_testo_e_tool_calls_mappato_correttamente() {
        // Un turno assistant con SIA testo SIA tool_calls: il content model deve
        // avere la part testo + la part functionCall, e il turno user successivo
        // deve avere il functionResponse correlato (conteggio call=1, resp=1).
        let mut a = assistant_with_calls(&[("run_command", "call_1")]);
        a.content = MessageContent::Text("Eseguo il comando per te".to_string());
        let t = tool_result("call_1", "exit 0");
        let req = req_with_tools(vec![a, t], true);
        let json = serde_json::to_value(build_request_body(&req, None)).unwrap();

        let model = &json["contents"][0];
        assert_eq!(model["role"], "model");
        // Prima part: il testo dell'assistant; seconda: la functionCall.
        assert_eq!(model["parts"][0]["text"], "Eseguo il comando per te");
        assert_eq!(model["parts"][1]["functionCall"]["name"], "run_command");

        // Una functionCall -> una functionResponse correlata.
        let user = &json["contents"][1];
        assert_eq!(user["role"], "user");
        let names = response_names(user);
        assert_eq!(names, vec!["run_command"]);
        assert_eq!(
            user["parts"][0]["functionResponse"]["response"]["result"],
            "exit 0"
        );
    }

    #[test]
    fn reconcile_funzione_pura_idempotente_senza_tool() {
        // Una history senza alcun tool: la riconciliazione non tocca nulla.
        let mut contents = vec![
            GoogleContent {
                role: Some("user".to_string()),
                parts: vec![GooglePart::text("ciao".to_string())],
            },
            GoogleContent {
                role: Some("model".to_string()),
                parts: vec![GooglePart::text("salve".to_string())],
            },
        ];
        reconcile_function_call_response_pairs(&mut contents);
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0].parts[0].text.as_deref(), Some("ciao"));
        assert_eq!(contents[1].parts[0].text.as_deref(), Some("salve"));
    }
}
