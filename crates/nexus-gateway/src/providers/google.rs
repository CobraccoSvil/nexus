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
    vertex_action_endpoint, vertex_endpoint, vertex_host, VertexAuth, SETTING_BACKEND,
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

/// Budget usato per DISABILITARE esplicitamente il thinking Gemini quando la
/// richiesta porta tool (gate [`GoogleThinking::DisabledForTools`]). Su
/// gemini-2.5-flash (i modelli dei run agentici) `thinkingBudget=0` spegne il
/// thinking. QUIRK: gemini-2.5-pro rifiuta 0 (minimo documentato 128); i run
/// agentici Nexus non usano pro, quindi 0 e' corretto e deterministico. Punto
/// unico del valore di disabilitazione: se servisse un trattamento per-modello
/// (es. pro -> 128) si interviene qui, non sparso nei call site (regola L).
const THINKING_DISABLE_BUDGET: u32 = 0;

/// Marker (case-insensitive) presente nel body d'errore HTTP 400 INVALID_ARGUMENT
/// quando Gemini RIFIUTA `thinkingConfig.thinkingBudget=0`. Quirk per-modello: i
/// modelli con thinking OBBLIGATORIO (es. gemini-2.5-pro, minimo documentato > 0)
/// rispondono "The model does not support setting thinking_budget to 0."; i flash
/// invece accettano 0 (disabilita). Anziche' mantenere una lista di modelli
/// (fragile e in violazione della regola G), il provider RI-ESEGUE la richiesta
/// una volta OMETTENDO il `thinkingConfig` (vedi
/// [`GoogleProvider::send_with_thinking_retry`]). La sottostringa "thinking_budget"
/// e' un campo dell'API (snake_case), non un nome modello hardcoded.
const THINKING_BUDGET_ERROR_MARKER: &str = "thinking_budget";

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
            self.build_vertex_backend(db).await?
        } else {
            GoogleBackend::Gemini
        };
        let backend = Arc::new(backend);
        self.backend.insert((), backend.clone());
        Ok(backend)
    }

    /// Costruisce il ramo [`GoogleBackend::Vertex`] leggendo project/location/
    /// discovery/credentials dai settings (regola G). Estratto da
    /// [`resolved_backend`] per contenerne la lunghezza (regola A); il bivio
    /// gemini/vertex resta nel chiamante. Propaga errore se project o credenziali
    /// mancano (niente fallback nascosto ad ADC/env).
    async fn build_vertex_backend(&self, db: &PgPool) -> anyhow::Result<GoogleBackend> {
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
        Ok(GoogleBackend::Vertex {
            project: project.trim().to_string(),
            location,
            discovery_locations,
            auth: Arc::new(auth),
        })
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
    /// (mig 0476 + 0545). Punto unico che decide l'ordine region (regola L):
    ///   1. cache in-memory HIT -> usa quella region da sola (fast path, no DB);
    ///   2. MISS -> legge la region persistita nel catalog (mig 0545, regola G:
    ///      cache DB-driven) e, se ancora valida, la mette PER PRIMA con le altre
    ///      `discovery_locations` come fallback (vedi [`order_regions_with_probed`]);
    ///      popola anche la cache in-memory per non ri-leggere il DB entro il TTL;
    ///   3. nessuna region persistita -> tutte le `discovery_locations` in ordine.
    /// Per il backend Gemini ritorna `None` (nessuna region: si invia una volta
    /// sola sull'endpoint API key).
    async fn vertex_regions_for_model(
        &self,
        backend: &GoogleBackend,
        model: &str,
    ) -> Option<Vec<String>> {
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
        // MISS in-memory: recupera la region persistita (sopravvive ai restart) e
        // mettila per prima. La cache in-memory viene popolata cosi' le richieste
        // successive entro il TTL non ri-leggono il DB; resta comunque validata dal
        // fallback del loop se nel frattempo desse 404 (send_across_regions la
        // sovrascrive con la region che funziona davvero, e ne persiste il cambio).
        let probed = self.read_probed_region(model).await;
        if let Some(region) = probed.as_deref() {
            self.vertex_model_region
                .insert(model.to_string(), region.to_string());
        }
        Some(order_regions_with_probed(
            probed.as_deref(),
            discovery_locations,
        ))
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
        let Some(regions) = self.vertex_regions_for_model(backend, model).await else {
            // Backend Gemini: nessuna region, un solo invio.
            return self.post_body_in_region(backend, "", model, stream, body).await;
        };
        self.send_across_regions(backend, model, stream, body, &regions)
            .await
    }

    /// Costruisce la richiesta per la `region` indicata e la invia (build + json +
    /// send). Punto unico dell'invio di un singolo tentativo (regola L): condiviso
    /// dal ramo Gemini (region vuota) e dal loop di fallback Vertex.
    async fn post_body_in_region(
        &self,
        backend: &GoogleBackend,
        region: &str,
        model: &str,
        stream: bool,
        body: &GenerateContentRequest,
    ) -> anyhow::Result<reqwest::Response> {
        Ok(self
            .build_post_in_region(backend, region, model, stream)
            .await?
            .json(body)
            .send()
            .await?)
    }

    /// Itera le `regions` nell'ordine di preferenza inviando la richiesta finche'
    /// una risponde non-404 (mig 0476). Estratto da [`send_with_region_fallback`]
    /// per contenerne la lunghezza (regola A): la scelta gemini/vertex resta nel
    /// chiamante, qui vive solo il loop Vertex. Un 404 significa "modello assente
    /// in quella region": si prova la successiva. Al primo 2xx la region viene
    /// cachata per il modello. Se TUTTE danno 404/errore si ritorna l'ultimo esito.
    async fn send_across_regions(
        &self,
        backend: &GoogleBackend,
        model: &str,
        stream: bool,
        body: &GenerateContentRequest,
        regions: &[String],
    ) -> anyhow::Result<reqwest::Response> {
        let mut last: Option<anyhow::Result<reqwest::Response>> = None;
        // `true` finche' OGNI region provata risponde 404 (modello assente). Un
        // errore di trasporto (transiente) o un non-404 lo azzera: l'auto-disable
        // scatta SOLO su 404-in-tutte-le-region (segnale strutturale = modello non
        // servibile in Vertex per il progetto), mai su un glitch temporaneo.
        let mut all_404 = true;
        for region in regions {
            match self.post_body_in_region(backend, region, model, stream, body).await {
                Ok(r) if r.status().as_u16() == 404 => {
                    // Modello non disponibile in questa region: prova la successiva
                    // (regola G: la lista arriva dal DB). Non logghiamo il body (F).
                    tracing::warn!(
                        model = %model,
                        region = %region,
                        "vertex 404: modello assente in region, provo la successiva"
                    );
                    last = Some(Ok(r));
                }
                Ok(r) => {
                    // Primo status non-404: questa risposta vince. Se 2xx, cacha la
                    // region (in-memory + DB) cosi' le richieste successive, ANCHE
                    // dopo un restart del gateway, saltano direttamente qui.
                    if r.status().is_success() {
                        let region_changed =
                            self.vertex_model_region.get(model).as_deref() != Some(region.as_str());
                        self.vertex_model_region
                            .insert(model.to_string(), region.clone());
                        // Persisti SOLO al cambio region: evita un round-trip DB a
                        // ogni richiesta (la cache in-memory gia' converge). La
                        // scrittura e' best-effort FAIL-OPEN (regola H): un errore
                        // di persistenza non tocca l'inference gia' riuscita.
                        if region_changed {
                            self.persist_probed_region(model, region).await;
                        }
                    }
                    return Ok(r);
                }
                Err(e) => {
                    // Errore di trasporto (non un 404 applicativo): tentativo
                    // fallito, provo la region successiva conservando l'esito.
                    tracing::warn!(
                        model = %model,
                        region = %region,
                        "vertex errore di trasporto, provo la region successiva"
                    );
                    all_404 = false;
                    last = Some(Err(e));
                }
            }
        }
        // Tutte le region hanno dato 404 (o errore di trasporto): ritorna l'ultimo
        // esito. `regions` non e' mai vuoto (discovery_locations garantito non
        // vuoto), quindi `last` e' sempre `Some`.
        //
        // AUTO-DISABLE 404-ovunque (regola H + M): se OGNI region ha risposto 404,
        // il modello e' pubblicato nel listing Vertex ma NON servibile in inference
        // per questo progetto/region (es. gemini-2.0-flash-001, gemma4). Il router
        // continuerebbe a sceglierlo e a pagare il cambio-provider ad ogni run. Lo
        // disabilitiamo nel catalog (best-effort, fail-open): il segnale e' il 404
        // strutturato di TUTTE le region, mai un errore transiente (all_404 azzerato
        // da qualunque Err di trasporto o non-404). Ripristino: ri-abilitazione
        // manuale o via re-probe (follow-up), i modelli 404-ovunque raramente
        // tornano servibili.
        if all_404 {
            self.mark_model_unavailable_all_regions(model).await;
        }
        match last {
            Some(r) => r,
            None => anyhow::bail!("vertex: nessuna region candidata per il modello {model}"),
        }
    }

    /// Disabilita nel catalog un modello Vertex risultato 404 in TUTTE le region
    /// (auto-disable su segnale strutturato, regola H/M). Best-effort FAIL-OPEN: un
    /// errore di persistenza NON tocca l'inference (che comunque fallira' con l'esito
    /// 404 restituito al chiamante). `is_enabled = true` nella WHERE rende l'UPDATE un
    /// no-op idempotente sui modelli gia' disabilitati. `auto_disabled_reason`
    /// distingue questo motivo (ripristinabile via re-probe) dai disable di policy;
    /// `reconcile_policy` NON ri-abilita i disable per fallimento (motivo != policy).
    async fn mark_model_unavailable_all_regions(&self, model: &str) {
        let Some(db) = self.db.as_ref() else {
            return;
        };
        if let Err(e) = sqlx::query(
            "UPDATE ai_price_catalog \
             SET is_enabled = false, auto_disabled_reason = 'vertex_404_all_regions', \
                 auto_disabled_at = NOW(), updated_at = NOW() \
             WHERE provider = 'google' AND model = $1 AND is_enabled = true",
        )
        .bind(model)
        .execute(db)
        .await
        {
            tracing::warn!(
                model = %model,
                error = %e,
                "vertex: auto-disable 404-ovunque fallito (best-effort)"
            );
        } else {
            tracing::warn!(
                model = %model,
                "vertex: modello 404 in TUTTE le region -> auto-disabilitato nel catalog"
            );
        }
    }

    /// Legge dal catalog la region Vertex confermata per il modello (mig 0545,
    /// regola G: cache DB-driven). Best-effort FAIL-OPEN (regola H): assenza di
    /// pool o qualunque errore DB ritorna `None` e il chiamante ricade sul
    /// fallback multi-region — l'inference non si rompe mai per la persistenza.
    /// Bind parametrico (regola M/G): niente interpolazione della stringa modello.
    async fn read_probed_region(&self, model: &str) -> Option<String> {
        let db = self.db.as_ref()?;
        match sqlx::query_scalar::<_, Option<String>>(
            "SELECT vertex_probed_region FROM ai_price_catalog \
             WHERE provider = 'google' AND model = $1",
        )
        .bind(model)
        .fetch_optional(db)
        .await
        {
            Ok(row) => row.flatten(),
            Err(e) => {
                tracing::warn!(
                    model = %model,
                    error = %e,
                    "vertex: lettura vertex_probed_region fallita, uso fallback multi-region"
                );
                None
            }
        }
    }

    /// Persiste nel catalog la region Vertex confermata per il modello (mig 0545,
    /// regola G). Best-effort FAIL-OPEN (regola H): un errore di persistenza NON
    /// deve rompere l'inference gia' riuscita — si logga WARN e si prosegue. La
    /// clausola `IS DISTINCT FROM` rende la UPDATE un no-op se la region non e'
    /// cambiata (guardia contro scritture concorrenti ridondanti). Bind
    /// parametrico (regola M/G): niente interpolazione. Chiamata SOLO per il
    /// backend Vertex (dal loop di [`send_across_regions`]), mai per Gemini.
    async fn persist_probed_region(&self, model: &str, region: &str) {
        let Some(db) = self.db.as_ref() else {
            return;
        };
        if let Err(e) = sqlx::query(
            "UPDATE ai_price_catalog SET vertex_probed_region = $1, updated_at = NOW() \
             WHERE provider = 'google' AND model = $2 AND vertex_probed_region IS DISTINCT FROM $1",
        )
        .bind(region)
        .bind(model)
        .execute(db)
        .await
        {
            tracing::warn!(
                model = %model,
                region = %region,
                error = %e,
                "vertex: persistenza vertex_probed_region fallita (best-effort, inference ok)"
            );
        }
    }

    /// Invia la richiesta a Gemini con RETRY-SU-400 per il quirk `thinking_budget`
    /// (fix definitivo, regola H + L). Punto unico dell'invio condiviso da
    /// [`complete`](LlmProvider::complete) e [`stream`](LlmProvider::stream): la
    /// logica del retry vive QUI, i due call site delegano e non re-implementano.
    ///
    /// Flusso:
    ///   1. costruisce il body con il `thinking` gia' risolto da
    ///      [`resolve_thinking`] e lo invia via [`send_with_region_fallback`];
    ///   2. se l'esito e' HTTP 400 e il body d'errore contiene
    ///      [`THINKING_BUDGET_ERROR_MARKER`] (quirk: il modello rifiuta
    ///      `thinkingBudget=0`, tipico di gemini-2.5-pro), RI-COSTRUISCE il body
    ///      con [`GoogleThinking::Absent`] — cioe' SENZA alcun `thinkingConfig` —
    ///      e RI-INVIA UNA sola volta. Omettendo il blocco, Gemini applica il suo
    ///      default (thinking ON sui modelli con thinking obbligatorio, invariato
    ///      altrove): evita il 400 senza sapere a priori quali modelli accettano 0;
    ///   3. ogni altro status (incluso un 400 di natura diversa) NON innesca il
    ///      retry: la risposta originale risale al chiamante invariata.
    ///
    /// Ritorna la `Response` FINALE (successo o errore non-retriabile). Il caller
    /// e' responsabile di controllare `status().is_success()` come prima: questo
    /// metodo non altera la gestione d'errore esistente, aggiunge solo il singolo
    /// retry mirato. Il retry e' saltato del tutto se il body iniziale non aveva
    /// `thinkingConfig` (niente da omettere -> il 400 e' di altra natura).
    async fn send_with_thinking_retry(
        &self,
        backend: &GoogleBackend,
        req: &LlmRequest,
        thinking: GoogleThinking,
        stream: bool,
    ) -> anyhow::Result<reqwest::Response> {
        let body = build_request_body(req, thinking);
        let had_thinking_config = !matches!(thinking, GoogleThinking::Absent);
        let resp = self
            .send_with_region_fallback(backend, &req.model, stream, &body)
            .await?;

        // Retry mirato solo se: status 400 + avevamo davvero un thinkingConfig da
        // omettere + il body d'errore parla di thinking_budget. Negli altri casi la
        // risposta passa intatta (incluso 400 di natura diversa).
        if resp.status().as_u16() != 400 || !had_thinking_config {
            return Ok(resp);
        }

        // Consuma il body d'errore per ispezionarlo. Se NON e' il quirk
        // thinking_budget dobbiamo restituire comunque l'errore al chiamante: ma la
        // `Response` e' gia' consumata, quindi rifabbrichiamo un errore equivalente
        // a quello che il call site avrebbe prodotto (`google HTTP 400: <body>`).
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !is_thinking_budget_error(&text) {
            return Err(
                super::ProviderHttpError::from_response("google", status.as_u16(), text).into(),
            );
        }

        // Quirk confermato: ri-eseguo OMETTENDO il thinkingConfig (GoogleThinking::
        // Absent -> nessun blocco nel body). Non logghiamo il body (regola F).
        tracing::warn!(
            model = %req.model,
            "gemini ha rifiutato thinkingBudget=0; ri-invio senza thinkingConfig (quirk thinking obbligatorio)"
        );
        let retry_body = build_request_body(req, GoogleThinking::Absent);
        self.send_with_region_fallback(backend, &req.model, stream, &retry_body)
            .await
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

    /// Autodiscovery Gemini direct: `GET {base_url}/models?key=...` ->
    /// `{ "models": [{ "name": "models/gemini-...", "inputTokenLimit": N }],
    ///    "nextPageToken": "..." }`, seguendo la paginazione (senza `pageSize`
    /// il default e' 50: sopra i 50 modelli il discovery troncava in silenzio).
    /// Estratto da [`list_models_meta`](LlmProvider::list_models_meta)
    /// (regola A). La Gemini API dichiara `inputTokenLimit` per ogni modello
    /// nel listing; propagarlo come finestra di contesto permette al catalog
    /// sync di scrivere il valore REALE invece di 0 = ignota (regola G/H: i
    /// preview/gemma senza finestra nota vengono re-instradati dal pre-check
    /// agentico).
    async fn list_gemini_models_meta(&self) -> anyhow::Result<Vec<crate::provider::ModelMeta>> {
        self.fetch_google_models_pages(
            || {
                self.http
                    .get(format!("{}/models", self.base_url))
                    .query(&[("key", self.api_key.as_str())])
            },
            "gemini",
            GEMINI_MODELS_PAGE_SIZE,
        )
        .await
    }

    /// Mappa basename -> finestra dichiarata dall'endpoint Gemini direct, usata
    /// per ARRICCHIRE il discovery Vertex (che non espone `inputTokenLimit`). Il
    /// `base_url` resta l'endpoint Gemini ufficiale anche in backend Vertex (che
    /// costruisce i suoi URL a parte), quindi la fonte e' interrogabile finche'
    /// c'e' una API key Gemini. Vuota se la key manca o la chiamata fallisce
    /// (degrado grazioso: nessun arricchimento, mai un errore che abbatte il
    /// discovery Vertex). Fonte REALE (regola G/H): niente finestra inventata.
    async fn gemini_declared_windows(&self) -> std::collections::HashMap<String, i64> {
        if self.api_key.trim().is_empty() {
            return std::collections::HashMap::new();
        }
        match self.list_gemini_models_meta().await {
            Ok(metas) => metas
                .into_iter()
                .filter_map(|m| m.context_window.map(|w| (m.id, w)))
                .collect(),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "google discovery: arricchimento finestre da Gemini direct fallito; i modelli Vertex senza finestra restano ignoti (0)"
                );
                std::collections::HashMap::new()
            }
        }
    }

    /// Loop di paginazione condiviso dai listing modelli Google (punto unico,
    /// regola L: Gemini direct e Vertex differiscono solo per URL/auth della
    /// singola pagina, costruita da `build_page_req` SENZA i parametri di
    /// paginazione, aggiunti qui). La `pageSize` e' un PARAMETRO esplicito perche'
    /// il tetto e' per-API (Gemini `models.list` max 1000, Vertex
    /// `publishers.models.list` max 300): un valore condiviso >300 fa fallire
    /// Vertex con 400 INVALID_ARGUMENT (regola L: punto unico parametrico, non
    /// una costante valida "per un solo ramo"). Segue `nextPageToken` accumulando
    /// le pagine parsate dal parser puro; su pagina non-2xx propaga l'errore
    /// STRUTTURATO (regola M) senza ritornare liste parziali: sarebbe il
    /// troncamento silenzioso che la paginazione elimina. Guard anti-loop: al
    /// tetto `GOOGLE_MODELS_MAX_PAGES` logga WARN (mai troncamento muto).
    async fn fetch_google_models_pages(
        &self,
        build_page_req: impl Fn() -> reqwest::RequestBuilder,
        scope: &str,
        page_size: &str,
    ) -> anyhow::Result<Vec<crate::provider::ModelMeta>> {
        let mut metas: Vec<crate::provider::ModelMeta> = Vec::new();
        let mut page_token: Option<String> = None;
        for page in 0..GOOGLE_MODELS_MAX_PAGES {
            let mut req = build_page_req().query(&[("pageSize", page_size)]);
            if let Some(tok) = page_token.as_deref() {
                req = req.query(&[("pageToken", tok)]);
            }
            let resp = req.send().await?;
            let status = resp.status();
            if !status.is_success() {
                // Errore strutturato anche sulla lista modelli (regola M): status +
                // codice, mai testo da classificare.
                let text = resp.text().await.unwrap_or_default();
                return Err(
                    super::ProviderHttpError::from_response("google", status.as_u16(), text)
                        .into(),
                );
            }
            let body: serde_json::Value = resp.json().await?;
            metas.extend(parse_google_models_meta_response(&body));
            page_token = next_google_page_token(&body);
            if page_token.is_none() {
                break;
            }
            if page + 1 == GOOGLE_MODELS_MAX_PAGES {
                tracing::warn!(
                    scope = %scope,
                    pages = GOOGLE_MODELS_MAX_PAGES,
                    "google discovery: tetto pagine raggiunto con nextPageToken ancora presente, listing potenzialmente incompleto"
                );
            }
        }
        // Il parser ordina+deduplica la singola pagina; l'unione cross-pagina
        // va ripulita di nuovo per output deterministico.
        metas.sort_by(|a, b| a.id.cmp(&b.id));
        metas.dedup_by(|a, b| a.id == b.id);
        Ok(metas)
    }

    /// Autodiscovery Vertex multi-region (mig 0476): interroga OGNI region
    /// candidata (con paginazione via `fetch_google_models_pages`) e unisce i
    /// risultati. europe-west4 NON espone i gemini-3.x, 'global' si': iterando
    /// le scopriamo entrambe. Una region che fallisce (non-2xx / rete, su
    /// QUALUNQUE pagina) e' loggata WARN e saltata, senza far fallire l'intero
    /// discovery (degrado parziale). Estratto da
    /// [`list_models`](LlmProvider::list_models) (regola A).
    async fn list_vertex_models(
        &self,
        discovery_locations: &[String],
        auth: &Arc<VertexAuth>,
    ) -> anyhow::Result<Vec<String>> {
        let token = auth.access_token().await?;
        let mut all: Vec<String> = Vec::new();
        let mut ok_regions = 0usize;
        for region in discovery_locations {
            // Host via punto unico vertex_host: la region `global` usa
            // aiplatform.googleapis.com (senza prefisso), le altre {region}-aiplatform.
            // Senza questo, `global` dava 404 e i modelli esposti solo li' (preview
            // Gemini 3) restavano invisibili al discovery.
            let url = format!(
                "https://{host}/v1beta1/publishers/google/models",
                host = vertex_host(region)
            );
            // Una pagina fallita fa fallire la region INTERA (warn+skip sotto):
            // una region parziale sarebbe un troncamento mascherato da successo.
            match self
                .fetch_google_models_pages(
                    || self.http.get(&url).bearer_auth(&token),
                    "vertex",
                    VERTEX_MODELS_PAGE_SIZE,
                )
                .await
            {
                Ok(metas) => {
                    // Il listing Vertex non espone la finestra di contesto:
                    // qui servono i soli id.
                    all.extend(metas.into_iter().map(|m| m.id));
                    ok_regions += 1;
                }
                // Log dei segnali strutturati: status + codice via downcast e il
                // messaggio diagnostico strutturato (`error.message`, regola M)
                // che dice QUALE argomento e' invalido; mai il body grezzo /
                // payload utente (regola F).
                Err(err) => match err.downcast_ref::<super::ProviderHttpError>() {
                    Some(he) => tracing::warn!(
                        region = %region,
                        status = he.status,
                        code = he.code.as_deref().unwrap_or(""),
                        message = he.structured_message().as_deref().unwrap_or(""),
                        "vertex discovery: GET models non-2xx, salto la region"
                    ),
                    None => tracing::warn!(
                        region = %region,
                        "vertex discovery: errore di rete/parse su GET models, salto la region"
                    ),
                },
            }
        }
        if ok_regions == 0 {
            anyhow::bail!(
                "vertex discovery: nessuna delle {} region ha risposto",
                discovery_locations.len()
            );
        }
        // Dedup mantenendo output deterministico (parita' col parser puro, che
        // ordina+deduplica per region; qui ri-uniamo le liste cross-region).
        all.sort();
        all.dedup();
        Ok(all)
    }

    /// Fase START della video-gen Veo: POST `:predictLongRunning` e ritorna
    /// l'`operation name` (non vuoto). Estratto da
    /// [`generate_video`](LlmProvider::generate_video) (regola A). Regola F: il
    /// body d'errore e' propagato al caller, non loggato in chiaro.
    async fn start_veo_operation(
        &self,
        project: &str,
        location: &str,
        auth: &Arc<VertexAuth>,
        req: &VideoGenRequest,
    ) -> anyhow::Result<String> {
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
            let text = resp.text().await.unwrap_or_default();
            return Err(
                super::ProviderHttpError::from_response("google", status.as_u16(), text).into(),
            );
        }
        let start_parsed: LongRunningStartResponse = resp.json().await?;
        start_parsed.name.filter(|s| !s.is_empty()).ok_or_else(|| {
            anyhow::anyhow!(":predictLongRunning non ha restituito un operation name")
        })
    }

    /// Poll-loop dell'operation long-running Veo: GET `{poll_url}` ogni
    /// [`VIDEO_POLL_INTERVAL`] finche' l'operation e' `done` o scade il
    /// `poll_timeout` (regola H: niente attesa infinita). Estratto da
    /// [`generate_video`](LlmProvider::generate_video) (regola A). Rinfresca il
    /// token ad ogni giro (i poll lunghi superano la scadenza). Ritorna il
    /// [`ParsedVideo`] estratto dall'operation conclusa.
    async fn poll_veo_operation(
        &self,
        poll_url: &str,
        auth: &Arc<VertexAuth>,
        start: Instant,
        poll_timeout: Duration,
    ) -> anyhow::Result<ParsedVideo> {
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
            let poll_resp = self.http.get(poll_url).bearer_auth(&poll_token).send().await?;
            let poll_status = poll_resp.status();
            if !poll_status.is_success() {
                // Errore strutturato anche sul poll (regola M): status + codice.
                let text = poll_resp.text().await.unwrap_or_default();
                return Err(
                    super::ProviderHttpError::from_response("google", poll_status.as_u16(), text)
                        .into(),
                );
            }
            let op: LongRunningOperation = poll_resp.json().await?;
            match parse_operation_response(op)? {
                OperationOutcome::Pending => continue,
                OperationOutcome::Done(video) => return Ok(video),
            }
        }
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
        let start = Instant::now();

        // Invio con fallback di region per Vertex (mig 0476) e retry-su-400 per il
        // quirk thinking_budget (punto unico send_with_thinking_retry): la prima
        // region non-404 vince; per Gemini e' un singolo invio invariato.
        let resp = self
            .send_with_thinking_retry(&backend, req, thinking, false)
            .await?;

        let status = resp.status();
        if !status.is_success() {
            // Regola F: body d'errore propagato al caller (cooldown Fase 3 lo
            // classifica via is_billing_error), non loggato qui in chiaro.
            let text = resp.text().await.unwrap_or_default();
            return Err(
                super::ProviderHttpError::from_response("google", status.as_u16(), text).into(),
            );
        }

        let parsed: GenerateContentResponse = resp.json().await?;
        let latency_ms = start.elapsed().as_millis() as u64;
        Ok(from_generate_response(parsed, req.model.clone(), latency_ms))
    }

    async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        let backend = self.resolved_backend().await?;
        let configured = self.configured_thinking_budget().await;
        let thinking = resolve_thinking(req, configured);

        // Fallback di region risolto sullo STATUS HTTP iniziale (mig 0476): il 404
        // arriva prima di qualunque byte di stream, quindi scegliamo la region
        // PRIMA di iniziare a consumare lo stream. Il retry-su-400 thinking_budget
        // (punto unico send_with_thinking_retry) avviene anch'esso prima del primo
        // byte, perche' Gemini risponde 400 sullo status iniziale. Solo a risposta
        // non-404 (e poi 2xx) si avvia il consumo dei bytes piu' sotto.
        let resp = self
            .send_with_thinking_retry(&backend, req, thinking, true)
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(
                super::ProviderHttpError::from_response("google", status.as_u16(), text).into(),
            );
        }

        let model_used = req.model.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<anyhow::Result<LlmStreamChunk>>(32);
        tokio::spawn(pump_google_sse(resp, model_used, tx));
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
                    "https://{host}/v1/projects/{project}/locations/{location}/publishers/google/models",
                    host = vertex_host(location)
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
        // Un solo bivio gemini/vertex (regola L): delega alla variante con
        // metadati e proietta i soli id, come il dialetto OpenAI-compat.
        Ok(self
            .list_models_meta()
            .await?
            .into_iter()
            .map(|m| m.id)
            .collect())
    }

    async fn list_models_meta(&self) -> anyhow::Result<Vec<crate::provider::ModelMeta>> {
        // Autodiscovery live per entrambi i backend:
        //   - Gemini: `GET {base_url}/models?key=...`
        //     -> `{ "models": [{ "name": "models/gemini-...", "inputTokenLimit": N }] }`,
        //     la finestra dichiarata fluisce nel `ModelMeta`;
        //   - Vertex: token Bearer su
        //     `GET https://{location}-aiplatform.googleapis.com/v1beta1/publishers/google/models`
        //     -> `{ "publisherModels": [{ "name": "publishers/google/models/gemini-..." }] }`,
        //     il listing NON espone la finestra -> `context_window=None`
        //     (il catalog sync scrive 0 = ignota, regola G/H, mai placeholder).
        // Il bivio gemini/vertex e' qui (l'auth Vertex riusa `gcp_auth`, regola L);
        // la normalizzazione a basename e' delegata al parser puro
        // [`parse_google_models_meta_response`] (parita' col brain `list_models_live`).
        let backend = self.resolved_backend().await?;
        match backend.as_ref() {
            GoogleBackend::Gemini => self.list_gemini_models_meta().await,
            GoogleBackend::Vertex {
                discovery_locations,
                auth,
                ..
            } => {
                // L'elenco Vertex e' AUTOREVOLE per la disponibilita' dei modelli
                // nel progetto, ma il listing `publisherModels` NON dichiara
                // `inputTokenLimit` -> finestra ignota. L'endpoint Gemini direct
                // (`{base}/models?key=`) la dichiara: se una API key Gemini e'
                // configurata, ARRICCHIAMO le finestre per basename da quella
                // fonte REALE (regola G/H: mai un placeholder inventato). I
                // modelli che nessuna fonte Google dichiara (alias Vertex-only
                // deprecati) restano `context_window=None` = 0 ignota, e il
                // pre-check agentico li re-instrada (fail-safe). Un errore Gemini
                // direct NON fa fallire il discovery Vertex (degrado grazioso).
                let ids = self.list_vertex_models(discovery_locations, auth).await?;
                let declared_windows = self.gemini_declared_windows().await;
                Ok(merge_ids_with_declared_windows(ids, &declared_windows))
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
            return Err(
                super::ProviderHttpError::from_response("google", status.as_u16(), text).into(),
            );
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
        let operation_name = self
            .start_veo_operation(project, location, auth, req)
            .await?;

        // 2. POLL: GET {operation_name} finche' done o timeout (regola H).
        let poll_url = format!(
            "https://{host}/v1/{operation_name}",
            host = vertex_host(location)
        );
        let video = self
            .poll_veo_operation(&poll_url, auth, start, poll_timeout)
            .await?;

        // 3. ESTRAZIONE: mappa il video estratto al contratto.
        let latency_ms = start.elapsed().as_millis() as u64;
        Ok(VideoGenResponse {
            video_base64: video.video_base64,
            url: video.url,
            mime: video.mime,
            model_used: req.model.clone(),
            provider_used: "google".to_string(),
            latency_ms,
        })
    }
}

/// Task di forwarding dello stream SSE Google: consuma i bytes della `resp`,
/// li da' in pasto al [`GoogleSseParser`] e inoltra i chunk pronti sul canale
/// `tx`. Estratto dal corpo di [`GoogleProvider::stream`] (regola A) per
/// contenerne la lunghezza; comportamento identico (un errore di trasporto
/// chiude il canale, il flush finale svuota il leftover del parser). Interrompe
/// non appena il ricevitore e' droppato (`tx.send` fallisce).
async fn pump_google_sse(
    resp: reqwest::Response,
    model_used: String,
    tx: tokio::sync::mpsc::Sender<anyhow::Result<LlmStreamChunk>>,
) {
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

/// Compone l'ordine delle region Vertex da provare per un modello, data la
/// region eventualmente persistita nel catalog (mig 0545). Funzione PURA (regola
/// L: UN solo punto che decide l'ordine region, testabile senza DB): se `probed`
/// e' presente ED e' ancora fra le `discovery_locations`, va provata PER PRIMA,
/// seguita dalle altre come fallback (cosi' se nel frattempo desse 404 il loop
/// prova comunque le altre). Se `probed` e' `None` o non piu' fra le candidate
/// (robustezza: region rimossa dal deploy) viene ignorata e si torna alle
/// `discovery_locations` nell'ordine di preferenza.
fn order_regions_with_probed(probed: Option<&str>, discovery_locations: &[String]) -> Vec<String> {
    match probed {
        Some(p) if discovery_locations.iter().any(|r| r == p) => {
            let mut ordered = Vec::with_capacity(discovery_locations.len());
            ordered.push(p.to_string());
            ordered.extend(
                discovery_locations
                    .iter()
                    .filter(|r| r.as_str() != p)
                    .cloned(),
            );
            ordered
        }
        _ => discovery_locations.to_vec(),
    }
}

/// Dimensione pagina per la Gemini API `models.list` (`pageSize`): massimo
/// documentato 1000; oltre il massimo il server la riduce da se' (AIP-158).
/// Senza il parametro il default e' 50 (troncamento silenzioso sopra i 50
/// modelli).
const GEMINI_MODELS_PAGE_SIZE: &str = "1000";

/// Dimensione pagina per Vertex `publishers.models.list` (`pageSize`): il tetto
/// e' 300 e Vertex e' STRETTO — un valore superiore viene RIFIUTATO con 400
/// INVALID_ARGUMENT ("Field: page_size; Message: Page size should be
/// non-negative and the maximum size is 300", verificato live 2026-07-07),
/// diversamente dalla Gemini API (max 1000) che invece lo clampa. Costante
/// distinta perche' il tetto e' per-API: condividere `1000` faceva fallire OGNI
/// region del discovery Vertex (regola H: il tetto vero, non un valore valido
/// "per un solo ramo").
const VERTEX_MODELS_PAGE_SIZE: &str = "300";

/// Tetto di sicurezza sulle pagine seguite per singolo listing: previene il
/// loop infinito se il server ripetesse lo stesso `nextPageToken`. 20 pagine
/// x 1000 modelli sono ordini di grandezza sopra il catalogo reale; se scatta
/// viene loggato WARN (mai troncamento silenzioso).
const GOOGLE_MODELS_MAX_PAGES: usize = 20;

/// Estrae il `nextPageToken` da una pagina del listing modelli Google (stesso
/// campo per Gemini `models[]` e Vertex `publisherModels[]`). Funzione PURA
/// (regola L, testabile senza rete): token assente o vuoto/spazi -> `None`
/// (ultima pagina).
pub fn next_google_page_token(body: &serde_json::Value) -> Option<String> {
    body.get("nextPageToken")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Estrae i nomi modello dalla risposta `models.list` di Google e li normalizza
/// a basename. Funzione PURA (regola L, testabile senza rete): gestisce entrambe
/// le forme di risposta, leggendo il campo `name` da:
///   - `models[]` (Gemini direct, es. `"models/gemini-2.5-flash"`);
///   - `publisherModels[]` (Vertex AI, es. `"publishers/google/models/gemini-2.5-flash"`).
/// Punto unico del parsing (regola L): delega alla variante con metadati e
/// proietta i soli id.
pub fn parse_google_models_response(body: &serde_json::Value) -> Vec<String> {
    parse_google_models_meta_response(body)
        .into_iter()
        .map(|m| m.id)
        .collect()
}

/// Variante CON METADATI di [`parse_google_models_response`] (punto unico del
/// parsing, regola L: la versione nomi-soli vi delega). Oltre al basename,
/// estrae la finestra di contesto DICHIARATA dal provider quando la forma la
/// espone: Gemini direct dichiara `inputTokenLimit` per ogni modello in
/// `models[]`; `publisherModels[]` (Vertex) non ha il campo -> `None`. Valori
/// non positivi sono trattati come non dichiarati: meglio "ignota" di una
/// finestra inventata (regola H, incidente sub-agente 2026-07-06).
/// Normalizza ogni nome al basename (`rsplit('/').next()`), come il brain
/// `list_models_live`; deduplica e ordina per id (output deterministico).
pub fn parse_google_models_meta_response(
    body: &serde_json::Value,
) -> Vec<crate::provider::ModelMeta> {
    let items = body
        .get("models")
        .and_then(|m| m.as_array())
        .or_else(|| body.get("publisherModels").and_then(|m| m.as_array()));
    let mut metas: Vec<crate::provider::ModelMeta> = items
        .map(|arr| arr.iter().filter_map(google_model_meta_of).collect())
        .unwrap_or_default();
    metas.sort_by(|a, b| a.id.cmp(&b.id));
    metas.dedup_by(|a, b| a.id == b.id);
    metas
}

/// Mappa UN elemento del listing Google in [`ModelMeta`]: `name` normalizzato
/// a basename ("publishers/google/models/X" -> "X", "models/X" -> "X",
/// "X" -> "X"); `inputTokenLimit` (Gemini direct) come finestra dichiarata
/// solo se positiva (Vertex non ha il campo -> `None`).
fn google_model_meta_of(m: &serde_json::Value) -> Option<crate::provider::ModelMeta> {
    let id = m
        .get("name")
        .and_then(|v| v.as_str())
        .and_then(|name| name.rsplit('/').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    let context_window = m
        .get("inputTokenLimit")
        .and_then(serde_json::Value::as_i64)
        .filter(|w| *w > 0);
    Some(crate::provider::ModelMeta { id, context_window })
}

/// Fonde l'elenco (autorevole) dei modelli Vertex con le finestre di contesto
/// DICHIARATE dall'endpoint Gemini direct (mappa basename -> finestra). Ogni id
/// Vertex prende la finestra dalla mappa se presente, altrimenti `None`
/// (0 = ignota): mai un valore inventato (regola G/H). Funzione PURA (regola L,
/// testabile senza rete), usata da [`list_models_meta`](LlmProvider::list_models_meta)
/// nel ramo Vertex.
fn merge_ids_with_declared_windows(
    ids: Vec<String>,
    declared_windows: &std::collections::HashMap<String, i64>,
) -> Vec<crate::provider::ModelMeta> {
    ids.into_iter()
        .map(|id| {
            let context_window = declared_windows.get(&id).copied();
            crate::provider::ModelMeta { id, context_window }
        })
        .collect()
}

/// Esito della risoluzione del thinking Gemini per una richiesta.
///
/// Tre stati distinti perche' Gemini distingue "thinkingConfig assente" da
/// "thinkingConfig con budget 0":
///   - [`GoogleThinking::Absent`]: NESSUN `thinkingConfig` nel body. Gemini usa il
///     suo default (thinking ON sui modelli 2.5/3.x). Storicamente l'unico ramo
///     per `req.thinking=None`. Usato solo quando NON ci sono tool e il chiamante
///     non ha chiesto thinking esplicito.
///   - [`GoogleThinking::DisabledForTools`]: `thinkingConfig { thinkingBudget: 0,
///     includeThoughts: false }` ESPLICITO. Spegne il thinking di Gemini. E' il
///     gate tool (vedi sotto): obbligatorio quando la richiesta porta tool, perche'
///     il thinking ON + function-calling forzato produce MALFORMED_FUNCTION_CALL /
///     turni vuoti -> cooldown -> probe disabilita tutti i gemini.
///   - [`GoogleThinking::Enabled`]: `thinkingConfig { thinkingBudget: budget,
///     includeThoughts: true }` con tetto di output alzato (fix hollow completion).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoogleThinking {
    /// Nessun `thinkingConfig` nel body (default provider).
    Absent,
    /// Thinking spento esplicitamente per via dei tool (`thinkingBudget=0`).
    DisabledForTools,
    /// Thinking attivo con `budget` token riservati al reasoning.
    Enabled(u32),
}

/// Riconosce, dal body d'errore HTTP 400, il quirk Gemini "il modello non
/// supporta thinkingBudget=0" (punto unico, funzione PURA testabile senza IO).
/// Discrimina questo 400 specifico da tutti gli altri 400 (schema invalido,
/// function-call mismatch, ecc.): solo per questo si fa il retry-senza-thinking.
/// Match case-insensitive sul campo d'API [`THINKING_BUDGET_ERROR_MARKER`].
fn is_thinking_budget_error(body: &str) -> bool {
    body.to_ascii_lowercase()
        .contains(THINKING_BUDGET_ERROR_MARKER)
}

/// Budget thinking effettivo per la richiesta (parita' col Python ~470-503).
///
/// GATE THINKING+TOOL (fix definitivo, regola H — incidente "google 0 enabled nel
/// catalog"): i modelli Gemini 2.5/3.x girano in thinking ON di DEFAULT. Nei run
/// agentici mcp-core passa `req.thinking=None` ma allega `tools` (+ spesso
/// `tool_choice` per il force-action). Con `thinking=None` il ramo storico
/// restituiva "thinkingConfig assente" -> Gemini applicava il suo default ON ->
/// thinking + function-calling forzato e' incompatibile -> l'API risponde
/// MALFORMED_FUNCTION_CALL (o turno vuoto) -> il provider va in cooldown -> il
/// model_health_probe a soglia disabilita TUTTI i gemini. Soluzione: quando la
/// richiesta porta tool (o un `tool_choice` riconosciuto) FORZIAMO il thinking OFF
/// emettendo un `thinkingConfig` ESPLICITO con `thinkingBudget=0`
/// (`DisabledForTools`). NON e' sufficiente lasciare il config assente: il default
/// sarebbe comunque ON. E' la traduzione, al confine col provider, della stessa
/// policy applicata a DeepSeek (`resolve_reasoning`, `disable_for_tools`). Regola
/// G: il gate scatta sulla PRESENZA di tool/tool_choice, non su una stringa
/// modello. Il riconoscimento del vincolo delega al PUNTO UNICO di mapping
/// ([`super::tool_choice::ToolChoice::from_openai`], regola L).
///
/// QUIRK gemini-2.5-pro: alcune versioni del modello pro NON accettano
/// `thinkingBudget=0` (rifiutano "thinking budget out of range", il minimo
/// documentato e' 128). Sui run agentici Nexus usa i modelli `flash`
/// (`thinkingBudget=0` disabilita correttamente). Se in futuro un modello pro
/// entra nei run agentici e rifiuta il budget 0, il fix definitivo e' usare il
/// floor minimo (128) per quel modello; vedi [`build_request_body`] dove il
/// budget di disabilitazione e' centralizzato in [`THINKING_DISABLE_BUDGET`].
///
/// Senza tool: comportamento storico (parita' col brain):
///   - thinking attivo solo se `req.thinking.enabled`;
///   - budget esplicito nella request ha priorita' su quello configurato;
///   - se `max_tokens` < soglia minima (256), thinking disattivato (troppo poco
///     spazio anche solo per la risposta);
///   - clamp del budget a `max(128, min(budget, max_tokens))`.
fn resolve_thinking(req: &LlmRequest, configured_budget: u32) -> GoogleThinking {
    // GATE TOOL: prima di tutto. Stessa rilevazione di deepseek.rs::resolve_reasoning.
    let has_tools = req.tools.as_ref().is_some_and(|t| !t.is_empty());
    let has_tool_choice_constraint = req
        .tool_choice
        .as_ref()
        .and_then(super::tool_choice::ToolChoice::from_openai)
        .is_some();
    if has_tools || has_tool_choice_constraint {
        return GoogleThinking::DisabledForTools;
    }

    let enabled = req.thinking.as_ref().is_some_and(|t| t.enabled);
    if !enabled {
        return GoogleThinking::Absent;
    }
    // Senza un tetto di output esplicito non sappiamo dimensionare il budget
    // (il Python alza max_output_tokens partendo da max_tokens richiesto): in
    // assenza, evitiamo di attivare il thinking per non rischiare hollow.
    let Some(max_tokens) = req.max_tokens else {
        return GoogleThinking::Absent;
    };
    if max_tokens < THINKING_MIN_MAX_TOKENS {
        return GoogleThinking::Absent;
    }
    let base = req
        .thinking
        .as_ref()
        .and_then(|t| t.budget_tokens)
        .unwrap_or(configured_budget);
    if base == 0 {
        return GoogleThinking::Absent;
    }
    let budget = base.min(max_tokens).max(THINKING_BUDGET_FLOOR);
    GoogleThinking::Enabled(budget)
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
                // Gemini `functionDeclarations` vuole il Type enum in UPPERCASE
                // (STRING/NUMBER/INTEGER/BOOLEAN/ARRAY/OBJECT): gli schemi tool
                // Nexus sono JSON-Schema OpenAI-style con `type` lowercase
                // ("string"), che Gemini rifiuta con HTTP 400 invalid_argument su
                // OGNI richiesta con tool. Normalizza SOLO il VALORE stringa del
                // campo `type`; una property NOMINATA "type" ha valore Object (non
                // stringa) e prosegue nella ricorsione, i valori di `enum` restano
                // array di stringhe (Gemini li accetta cosi').
                if k == "type" {
                    if let serde_json::Value::String(s) = v {
                        out.insert(k.clone(), serde_json::Value::String(s.to_uppercase()));
                        continue;
                    }
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
    let (system_instruction, contents) = build_google_contents(req);
    GenerateContentRequest {
        contents,
        system_instruction,
        generation_config: build_generation_config(req, thinking),
        tools: build_google_tools(req),
        tool_config: build_google_tool_config(req),
    }
}

/// Costruisce la mappa `tool_call_id -> tool_name` di TUTTE le tool-call in
/// history (parita' Python ~769-775). Estratto da [`build_google_contents`]
/// (regola A). Funzione PURA.
fn build_tool_id_to_name(req: &LlmRequest) -> std::collections::HashMap<String, String> {
    let mut id_to_name: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for msg in &req.messages {
        if let Some(calls) = &msg.tool_calls {
            for tc in calls {
                id_to_name.insert(tc.id.clone(), tc.function.name.clone());
            }
        }
    }
    id_to_name
}

/// Svuota il buffer `pending` (functionResponse accumulate) in un unico turno
/// `user`, se non vuoto. Punto unico (regola L) del raggruppamento richiesto da
/// Gemini, condiviso dal flush inline e da quello finale di
/// [`build_google_contents`]. No-op se `pending` e' vuoto.
fn flush_pending_responses(contents: &mut Vec<GoogleContent>, pending: &mut Vec<GooglePart>) {
    if pending.is_empty() {
        return;
    }
    contents.push(GoogleContent {
        role: Some("user".to_string()),
        parts: std::mem::take(pending),
    });
}

/// Converte i messaggi del contratto nei `contents[]` Google, separando il
/// `system` come `systemInstruction`. Estratto da [`build_request_body`]
/// (regola A/L, punto unico della conversione history). Raggruppa le
/// functionResponse in un unico turno user (invariante Gemini) e chiude
/// richiamando [`reconcile_function_call_response_pairs`].
fn build_google_contents(req: &LlmRequest) -> (Option<GoogleContent>, Vec<GoogleContent>) {
    let mut system_instruction: Option<GoogleContent> = None;
    let mut contents: Vec<GoogleContent> = Vec::new();

    // Mappa id->name di TUTTE le tool-call in history: Gemini vuole il NOME del
    // tool nel functionResponse, non l'id (costruita prima del loop cosi' un
    // tool-result risolve il nome anche se la call e' in un turno precedente).
    let id_to_name = build_tool_id_to_name(req);

    // Buffer functionResponse: Gemini vuole TUTTE le functionResponse di un turno
    // model multi-functionCall raggruppate in UN unico turno user (altrimenti HTTP
    // 400 "number of function response parts is equal to function call parts").
    let mut pending_fn_responses: Vec<GooglePart> = Vec::new();
    for msg in &req.messages {
        if msg.role.as_str() != "tool" {
            flush_pending_responses(&mut contents, &mut pending_fn_responses);
        }
        match msg.role.as_str() {
            "system" => {
                system_instruction = Some(GoogleContent {
                    role: None,
                    parts: vec![GooglePart::text(content_to_string(&msg.content))],
                });
            }
            "tool" => pending_fn_responses.push(tool_result_to_part(msg, &id_to_name)),
            "assistant" if msg.tool_calls.as_ref().is_some_and(|c| !c.is_empty()) => {
                contents.push(assistant_tool_call_content(msg));
            }
            role => contents.push(plain_message_content(role, msg)),
        }
    }
    // Flush finale delle functionResponse accumulate (history che termina con
    // uno o piu' tool-result): stesso raggruppamento in un unico turno user.
    flush_pending_responses(&mut contents, &mut pending_fn_responses);

    // Riconciliazione invariante Gemini (punto unico, regola L): per ogni content
    // `model` con N functionCall, il content `user` successivo deve avere
    // ESATTAMENTE N functionResponse (stesso name/ordine). History interrotte o
    // troncate violano l'invariante e Gemini risponde HTTP 400 INVALID_ARGUMENT
    // ("number of function response parts is equal to function call parts"). La
    // riconciliazione sintetizza i response mancanti e scarta quelli orfani.
    reconcile_function_call_response_pairs(&mut contents);
    (system_instruction, contents)
}

/// Mappa un messaggio `role="tool"` nella sua part `functionResponse` (turno
/// `user`, response `{"result": ...}`). Il NOME del tool si risolve dalla mappa
/// id->name; se l'id e' sconosciuto si ripiega sull'id grezzo (Gemini
/// rifiuterebbe il mismatch, ma e' il fallback meno dannoso). Parita' Python
/// ~818-842. Estratto da [`build_google_contents`].
fn tool_result_to_part(
    msg: &crate::types::LlmMessage,
    id_to_name: &std::collections::HashMap<String, String>,
) -> GooglePart {
    let name = msg
        .tool_call_id
        .as_ref()
        .and_then(|id| id_to_name.get(id).cloned())
        .or_else(|| msg.tool_call_id.clone())
        .unwrap_or_default();
    let response = serde_json::json!({ "result": content_to_string(&msg.content) });
    GooglePart::function_response(name, response)
}

/// Costruisce il content `role="model"` per un assistant con tool-call: una part
/// `functionCall` per ciascuna call (parita' Python ~807-817), preceduta
/// dall'eventuale testo. Su Gemini 3 la `thoughtSignature` e' PER-CALL: va
/// riattaccata alla SUA part `functionCall` (regola H/M), altrimenti HTTP 400
/// INVALID_ARGUMENT "Function call is missing a thought_signature". La firma a
/// livello di turno (`msg.thinking_signature`, pattern Anthropic / thought-text)
/// resta il fallback sulla PRIMA part quando questa non e' gia' firmata per-call.
/// Estratto da [`build_google_contents`].
fn assistant_tool_call_content(msg: &crate::types::LlmMessage) -> GoogleContent {
    let mut parts: Vec<GooglePart> = Vec::new();
    // Eventuale testo dell'assistant prima delle call (raro).
    if let MessageContent::Text(t) = &msg.content {
        if !t.is_empty() {
            parts.push(GooglePart::text(t.clone()));
        }
    }
    for tc in msg.tool_calls.as_ref().into_iter().flatten() {
        // arguments e' una stringa JSON nel contratto; Gemini vuole un oggetto
        // in `args` (parita' col mapping Anthropic ~606-608).
        let args: serde_json::Value =
            serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| serde_json::json!({}));
        let mut part = GooglePart::function_call(tc.function.name.clone(), args);
        // Firma PER-CALL: ogni functionCall ri-passa la propria thoughtSignature.
        part.thought_signature = tc.thought_signature.clone();
        parts.push(part);
    }
    if parts.is_empty() {
        parts.push(GooglePart::text(String::new()));
    }
    // Fallback a livello di turno: solo se la PRIMA part non porta gia' una
    // firma per-call (es. testo-thought che precede le call, o parita' Anthropic).
    if let Some(first) = parts.first_mut() {
        if first.thought_signature.is_none() {
            first.thought_signature = msg.thinking_signature.clone();
        }
    }
    GoogleContent {
        role: Some("model".to_string()),
        parts,
    }
}

/// Costruisce il content per un messaggio ordinario (user/assistant-testo).
/// RI-PASSAGGIO thought_signature: se il turno mappa a `model`, va riattaccata
/// alla PRIMA part (parita' Python ~776-798; obbligatoria su Gemini 3). Estratto
/// da [`build_google_contents`].
fn plain_message_content(role: &str, msg: &crate::types::LlmMessage) -> GoogleContent {
    let signature = if map_role(role) == "model" {
        msg.thinking_signature.clone()
    } else {
        None
    };
    let mut parts = content_to_parts(&msg.content);
    // La signature si attacca alla PRIMA part del turno (vuota se assente).
    // `content_to_parts` garantisce almeno una part.
    if let Some(first) = parts.first_mut() {
        first.thought_signature = signature;
    }
    GoogleContent {
        role: Some(map_role(role).to_string()),
        parts,
    }
}

/// Costruisce il `generationConfig` (temperature + maxOutputTokens + thinking).
/// Estratto da [`build_request_body`] (regola A). Fix hollow completion: quando
/// il thinking e' ATTIVO alza il tetto di output del budget cosi' i max_tokens
/// richiesti restano interi per la risposta. Ritorna `None` se nessun campo e'
/// valorizzato.
fn build_generation_config(req: &LlmRequest, thinking: GoogleThinking) -> Option<GenerationConfig> {
    let max_output_tokens = match (req.max_tokens, thinking) {
        (Some(mt), GoogleThinking::Enabled(budget)) => Some(mt.saturating_add(budget)),
        (mt, _) => mt,
    };

    // Mapping enum -> wire `thinkingConfig` (punto unico, regola L):
    //   - Absent          -> nessun thinkingConfig (Gemini usa il suo default ON).
    //   - DisabledForTools -> thinkingConfig ESPLICITO budget 0 / includeThoughts
    //     false: spegne il thinking ON di default, necessario col function-calling
    //     (lasciarlo assente NON basterebbe, vedi resolve_thinking).
    //   - Enabled(budget) -> thinkingConfig con budget e thoughts visibili.
    let thinking_config = match thinking {
        GoogleThinking::Absent => None,
        GoogleThinking::DisabledForTools => Some(ThinkingConfigWire {
            include_thoughts: false,
            thinking_budget: THINKING_DISABLE_BUDGET,
        }),
        GoogleThinking::Enabled(budget) => Some(ThinkingConfigWire {
            include_thoughts: true,
            thinking_budget: budget,
        }),
    };
    let response_format = google_response_format(req.response_format.as_ref());

    if req.temperature.is_some()
        || max_output_tokens.is_some()
        || thinking_config.is_some()
        || response_format.is_some()
    {
        Some(GenerationConfig {
            temperature: req.temperature,
            max_output_tokens,
            thinking_config,
            response_mime_type: response_format
                .as_ref()
                .map(|rf| rf.response_mime_type.clone()),
            response_schema: response_format.and_then(|rf| rf.response_schema),
        })
    } else {
        None
    }
}

/// Mappa `response_format` OpenAI-style nel dialetto Gemini/Vertex.
///
/// Supporta il vincolo JSON senza inventare schema: `{"type":"json_object"}`
/// diventa `responseMimeType:"application/json"`. Se il chiamante usa
/// `json_schema` e passa uno schema strutturato, lo inoltriamo come
/// `responseSchema`.
fn google_response_format(format: Option<&serde_json::Value>) -> Option<GoogleResponseFormat> {
    let format = format?;
    let kind = format.get("type").and_then(|v| v.as_str())?;
    match kind {
        "json_object" => Some(GoogleResponseFormat {
            response_mime_type: "application/json".to_string(),
            response_schema: None,
        }),
        "json_schema" => {
            let schema = format
                .get("json_schema")
                .and_then(|v| v.get("schema"))
                .cloned();
            Some(GoogleResponseFormat {
                response_mime_type: "application/json".to_string(),
                response_schema: schema,
            })
        }
        _ => None,
    }
}

struct GoogleResponseFormat {
    response_mime_type: String,
    response_schema: Option<serde_json::Value>,
}

/// Costruisce le `functionDeclarations` native Gemini: ogni tool del contratto
/// OpenAI diventa una FunctionDeclaration con lo schema normalizzato al subset
/// Google (clean_schema_for_google). Senza questo blocco, `tool_config`(mode=ANY)
/// e' inerte e il modello emette i control-token nel testo invece di una
/// functionCall. Parita' col brain (un solo elemento Tool con tutte le
/// declarations). Estratto da [`build_request_body`] (regola A). NOTA
/// thinking+tools: il gate disable_for_tools e' applicato da [`resolve_thinking`].
fn build_google_tools(req: &LlmRequest) -> Option<Vec<GoogleToolDecl>> {
    req.tools.as_ref().map(|defs| {
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
    })
}

/// Costruisce il `tool_config.function_calling_config` via il punto unico di
/// mapping (regola L). Presente SOLO quando ci sono `tools`: se il chiamante
/// fornisce un vincolo riconosciuto (auto/required/none/function) si usa quello,
/// altrimenti il DEFAULT ESPLICITO `mode=AUTO`. Estratto da
/// [`build_request_body`] (regola A).
///
/// QUIRK GEMINI (fix definitivo, regola H): i modelli "thinking" (gemini-2.5/
/// 3.x) con `tools` presenti ma SENZA `toolConfig` rispondono in modo NON
/// deterministico con un turno vuoto (zero output token, finishReason STOP,
/// nessuna functionCall) invece di chiamare il tool. Diagnosticato sul tool-probe
/// di mcp-core (`generate_agent_turn` non invia `tool_choice`): ~1 richiesta su 3
/// tornava vuota -> a soglia tutti i gemini finivano auto-disabilitati con
/// `tool_probe_failed`. Inviando esplicitamente `functionCallingConfig.mode=AUTO`
/// il function calling torna deterministico (3/3 OK in verifica). Senza tool
/// (`req.tools` None) nessun tool_config: un `toolConfig` orfano sarebbe rifiutato.
fn build_google_tool_config(req: &LlmRequest) -> Option<GoogleToolConfig> {
    req.tools.as_ref().map(|_| {
        let function_calling_config = req
            .tool_choice
            .as_ref()
            .and_then(super::tool_choice::to_google_function_calling_config)
            .unwrap_or_else(super::tool_choice::default_google_function_calling_config);
        GoogleToolConfig {
            function_calling_config,
        }
    })
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
        reconcile_model_turn(contents, i, &call_names);
        // Salta sia il turno model sia il turno user di response (quest'ultimo
        // non contiene functionCall e non va ri-processato come turno model).
        i += 2;
    }
    drop_orphan_leading_responses(contents);
}

/// Riconcilia il turno `user` che segue il turno model in posizione `i` (con
/// `call_names` functionCall) affinche' porti ESATTAMENTE una functionResponse
/// per call, nello stesso ordine (name-correlato, FIFO sui duplicati). Le
/// response mancanti sono sintetizzate, le orfane scartate; se non c'e' turno
/// user successivo ne inserisce uno. Estratto da
/// [`reconcile_function_call_response_pairs`] (regola A).
fn reconcile_model_turn(contents: &mut Vec<GoogleContent>, i: usize, call_names: &[String]) {
    // Recupera (consumando) le functionResponse del content successivo, se e' il
    // turno user che le porta, indicizzate per name in code FIFO.
    let has_next_user_responses = contents
        .get(i + 1)
        .is_some_and(|c| c.parts.iter().any(|p| p.function_response.is_some()));
    let mut by_name = if has_next_user_responses {
        collect_responses_by_name(std::mem::take(&mut contents[i + 1].parts))
    } else {
        std::collections::HashMap::new()
    };

    // Ricostruisci le response NELLO STESSO ORDINE delle call: per ogni call
    // consuma una response con lo stesso name; se non c'e', sintetizzala. Le
    // response rimaste in `by_name` sono orfane e vengono scartate.
    let mut reconciled: Vec<GooglePart> = Vec::with_capacity(call_names.len());
    for name in call_names {
        match by_name.get_mut(name).and_then(|q| q.pop_front()) {
            Some(part) => reconciled.push(part),
            None => reconciled.push(GooglePart::function_response(
                name.clone(),
                serde_json::json!({ "error": SYNTHETIC_TOOL_RESULT_MESSAGE }),
            )),
        }
    }

    if has_next_user_responses {
        // Sovrascrive il turno user successivo con i response riconciliati.
        contents[i + 1].parts = reconciled;
    } else {
        // Nessun turno user con response dopo il model (history troncata subito
        // dopo le call): inserisce un nuovo turno user di soli response sintetici.
        contents.insert(
            i + 1,
            GoogleContent {
                role: Some("user".to_string()),
                parts: reconciled,
            },
        );
    }
}

/// Indicizza le functionResponse di un turno per name in code FIFO, cosi' piu'
/// call con lo stesso nome consumano response distinte nell'ordine. Le part
/// non-functionResponse vengono scartate. Estratto da [`reconcile_model_turn`].
fn collect_responses_by_name(
    parts: Vec<GooglePart>,
) -> std::collections::HashMap<String, VecDeque<GooglePart>> {
    let mut by_name: std::collections::HashMap<String, VecDeque<GooglePart>> =
        std::collections::HashMap::new();
    for part in parts {
        if let Some(name) = part.function_response.as_ref().map(|fr| fr.name.clone()) {
            by_name.entry(name).or_default().push_back(part);
        }
    }
    by_name
}

/// Passata finale: scarta le functionResponse che NON sono precedute da un turno
/// model con functionCall (response orfane "di testa", caso degenere: un
/// tool-result come primo messaggio). Inviarle a Gemini produrrebbe un 400. Le
/// parts non-functionResponse dello stesso turno restano; un turno che resta
/// senza parts viene rimosso. Estratto da
/// [`reconcile_function_call_response_pairs`] (regola A).
fn drop_orphan_leading_responses(contents: &mut Vec<GoogleContent>) {
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
/// Testo/reasoning/tool-call/signature estratti dalle parts di un candidate
/// Gemini (risposta non-stream). Aggregatore intermedio di
/// [`collect_candidate_parts`], consumato da [`from_generate_response`].
#[derive(Default)]
struct CandidateParts {
    content: String,
    reasoning: String,
    thinking_signature: Option<String>,
    tool_calls: Vec<LlmToolCall>,
}

/// Scandisce le `parts` del candidate separando testo utente, reasoning
/// (`thought=true`), tool-call e thoughtSignature (catturata una sola volta).
/// Estratto da [`from_generate_response`] (regola A). Funzione PURA.
fn collect_candidate_parts(candidate: Option<&GoogleCandidate>) -> CandidateParts {
    let mut acc = CandidateParts::default();
    let Some(c) = candidate else {
        return acc;
    };
    for part in &c.content.parts {
        if acc.thinking_signature.is_none() {
            if let Some(sig) = &part.thought_signature {
                if !sig.is_empty() {
                    acc.thinking_signature = Some(sig.clone());
                }
            }
        }
        // functionCall: il modello chiede di eseguire un tool. Gemini non emette
        // un id di tool-call, ne sintetizziamo uno stabile cosi' il brain puo'
        // correlare il functionResponse nel round-trip (parita' Python ~589).
        // La part-functionCall non porta testo: passa oltre.
        if let Some(fc) = &part.function_call {
            acc.tool_calls.push(LlmToolCall {
                id: format!("call_{}", uuid::Uuid::new_v4().simple()),
                kind: "function".to_string(),
                function: ToolFunctionCall {
                    name: fc.name.clone(),
                    // Il contratto vuole `arguments` come STRINGA JSON (parita'
                    // Anthropic ~761): serializziamo l'oggetto args.
                    arguments: function_args_to_string(&fc.args),
                },
                // Gemini 3 lega la thoughtSignature alla SINGOLA functionCall:
                // va catturata sulla SUA part (non a livello di turno) e
                // ri-passata identica nel round-trip, altrimenti HTTP 400.
                thought_signature: part
                    .thought_signature
                    .as_ref()
                    .filter(|s| !s.is_empty())
                    .cloned(),
            });
            continue;
        }
        if let Some(text) = &part.text {
            if part.thought.unwrap_or(false) {
                acc.reasoning.push_str(text);
            } else {
                acc.content.push_str(text);
            }
        }
    }
    acc
}

/// Mappa l'`usageMetadata` Gemini (non-stream) nel contratto [`LlmUsage`],
/// azzerando i conteggi quando il metadata e' assente. Estratto da
/// [`from_generate_response`] (regola A). Funzione PURA.
fn usage_from_metadata(meta: Option<GoogleUsageMetadata>) -> LlmUsage {
    meta.map(|u| LlmUsage {
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
    })
}

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
    let parts = collect_candidate_parts(candidate.as_ref());

    // Quando ci sono tool-call, Gemini segnala finishReason=STOP: per parita' col
    // contratto (Anthropic/OpenAI usano "tool_calls"), forziamo il segnale che il
    // brain usa per capire che deve eseguire i tool.
    let finish_reason = if parts.tool_calls.is_empty() {
        map_finish_reason(candidate.as_ref().and_then(|c| c.finish_reason.as_deref()))
    } else {
        "tool_calls".to_string()
    };

    let usage = usage_from_metadata(resp.usage_metadata);

    LlmResponse {
        content: parts.content,
        // functionCall native Gemini -> tool_calls del contratto (parita' coi
        // peer anthropic/openai_compat). `None` quando il turno non chiede tool.
        tool_calls: if parts.tool_calls.is_empty() {
            None
        } else {
            Some(parts.tool_calls)
        },
        usage,
        model_used,
        provider_used: "google".to_string(),
        latency_ms,
        finish_reason,
        privacy_rerouted: None,
        reasoning: if parts.reasoning.is_empty() {
            None
        } else {
            Some(parts.reasoning)
        },
        thinking_signature: parts.thinking_signature,
        citations: None,
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

/// Delta di testo/reasoning/tool-call estratti dalle parts di un chunk SSE.
/// Aggregatore intermedio di [`collect_stream_parts`], consumato da
/// [`GoogleSseParser::chunk_from_response`].
#[derive(Default)]
struct StreamParts {
    delta: String,
    reasoning_delta: String,
    tool_call_delta: Option<ToolCallDelta>,
}

/// Scandisce le parts di un chunk SSE separando testo utente (`delta`),
/// reasoning (`thought=true`, `reasoning_delta`) e la PRIMA functionCall come
/// [`ToolCallDelta`] gia' completo (Gemini non frammenta gli args nello stream).
/// Estratto da [`GoogleSseParser::chunk_from_response`] (regola A). Funzione PURA.
fn collect_stream_parts(candidate: Option<&GoogleCandidate>) -> StreamParts {
    let mut acc = StreamParts::default();
    let Some(c) = candidate else {
        return acc;
    };
    for part in &c.content.parts {
        if acc.tool_call_delta.is_none() {
            if let Some(fc) = &part.function_call {
                acc.tool_call_delta = Some(ToolCallDelta {
                    index: 0,
                    id: Some(format!("call_{}", uuid::Uuid::new_v4().simple())),
                    function: Some(ToolCallDeltaFunction {
                        name: Some(fc.name.clone()),
                        // arguments come stringa JSON completa in un colpo.
                        arguments: Some(function_args_to_string(&fc.args)),
                    }),
                });
                continue; // la part-functionCall non porta text
            }
        }
        if let Some(text) = &part.text {
            if part.thought.unwrap_or(false) {
                acc.reasoning_delta.push_str(text);
            } else {
                acc.delta.push_str(text);
            }
        }
    }
    acc
}

/// Determina il `finish_reason` di un chunk SSE. Quando il chunk porta una
/// tool-call segnaliamo "tool_calls" al consumer (parita' col mapping
/// non-stream): Gemini puo' mandare functionCall con finishReason=STOP (o senza
/// finish nel chunk della call). Estratto da
/// [`GoogleSseParser::chunk_from_response`] (regola A).
fn stream_finish_reason(candidate: Option<&GoogleCandidate>, has_tool_call: bool) -> Option<String> {
    let mapped = candidate
        .and_then(|c| c.finish_reason.as_deref())
        .map(|r| map_finish_reason(Some(r)));
    if has_tool_call {
        match mapped.as_deref() {
            None | Some("stop") => Some("tool_calls".to_string()),
            _ => mapped,
        }
    } else {
        mapped
    }
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

        // Separa testo utente da reasoning (part `thought=true`) ed estrae
        // l'eventuale tool-call (parita' col Python streaming).
        let stream_parts = collect_stream_parts(candidate.as_ref());
        let StreamParts {
            delta,
            reasoning_delta,
            tool_call_delta,
        } = stream_parts;

        let finish_reason = stream_finish_reason(candidate.as_ref(), tool_call_delta.is_some());

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
    /// JSON mode / schema strutturato nel dialetto Gemini/Vertex.
    #[serde(rename = "responseMimeType", skip_serializing_if = "Option::is_none")]
    response_mime_type: Option<String>,
    #[serde(rename = "responseSchema", skip_serializing_if = "Option::is_none")]
    response_schema: Option<serde_json::Value>,
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
            reasoning: None,
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
    fn parse_models_meta_gemini_estrae_input_token_limit() {
        // Payload REALE (campi e valori) della Gemini API `GET /models`:
        // ogni modello dichiara `inputTokenLimit` accanto a nome/versione.
        let body = serde_json::json!({
            "models": [
                {
                    "name": "models/gemini-2.5-flash",
                    "version": "001",
                    "displayName": "Gemini 2.5 Flash",
                    "description": "Stable version of Gemini 2.5 Flash",
                    "inputTokenLimit": 1048576,
                    "outputTokenLimit": 65536,
                    "supportedGenerationMethods": ["generateContent", "countTokens"],
                    "temperature": 1.0,
                    "topP": 0.95,
                    "topK": 64
                },
                {
                    "name": "models/gemma-3-27b-it",
                    "version": "001",
                    "displayName": "Gemma 3 27B",
                    "inputTokenLimit": 131072,
                    "outputTokenLimit": 8192,
                    "supportedGenerationMethods": ["generateContent", "countTokens"]
                }
            ]
        });
        let metas = parse_google_models_meta_response(&body);
        assert_eq!(
            metas,
            vec![
                crate::provider::ModelMeta {
                    id: "gemini-2.5-flash".to_string(),
                    context_window: Some(1_048_576),
                },
                crate::provider::ModelMeta {
                    id: "gemma-3-27b-it".to_string(),
                    context_window: Some(131_072),
                },
            ]
        );
    }

    #[test]
    fn parse_models_meta_finestra_assente_o_non_positiva_resta_ignota() {
        // Campo assente o non positivo: `None`, mai una finestra inventata
        // (regola H, incidente sub-agente 2026-07-06).
        let body = serde_json::json!({
            "models": [
                { "name": "models/gemini-preview-x" },
                { "name": "models/gemini-zero", "inputTokenLimit": 0 },
                { "name": "models/gemini-neg", "inputTokenLimit": -1 },
            ]
        });
        let metas = parse_google_models_meta_response(&body);
        assert_eq!(metas.len(), 3);
        assert!(metas.iter().all(|m| m.context_window.is_none()));
    }

    #[test]
    fn parse_models_meta_vertex_senza_finestra() {
        // Il listing Vertex `publisherModels[]` non espone la finestra:
        // basename normalizzato e `context_window=None`.
        let body = serde_json::json!({
            "publisherModels": [
                { "name": "publishers/google/models/gemini-2.5-pro" },
            ]
        });
        let metas = parse_google_models_meta_response(&body);
        assert_eq!(
            metas,
            vec![crate::provider::ModelMeta {
                id: "gemini-2.5-pro".to_string(),
                context_window: None,
            }]
        );
    }

    #[test]
    fn merge_vertex_ids_arricchisce_solo_i_dichiarati() {
        // L'elenco Vertex e' autorevole; le finestre arrivano dalla mappa Gemini
        // direct (fonte reale). Un modello presente nella mappa prende la finestra
        // REALE; uno assente (nessuna fonte lo dichiara) resta ignoto (None), mai
        // un valore inventato (regola G/H).
        let ids = vec![
            "gemini-omni-flash-preview".to_string(), // dichiarato da Gemini direct
            "gemini-2.5-flash".to_string(),          // dichiarato
            "gemma3".to_string(),                    // alias Vertex-only: nessuna fonte
        ];
        let mut windows = std::collections::HashMap::new();
        windows.insert("gemini-omni-flash-preview".to_string(), 131_072_i64);
        windows.insert("gemini-2.5-flash".to_string(), 1_048_576_i64);
        let metas = merge_ids_with_declared_windows(ids, &windows);
        let win = |id: &str| {
            metas
                .iter()
                .find(|m| m.id == id)
                .and_then(|m| m.context_window)
        };
        assert_eq!(win("gemini-omni-flash-preview"), Some(131_072));
        assert_eq!(win("gemini-2.5-flash"), Some(1_048_576));
        assert_eq!(win("gemma3"), None, "nessuna fonte dichiara: resta ignota");
        assert_eq!(metas.len(), 3, "l'elenco Vertex non viene ne' filtrato ne' esteso");
    }

    #[test]
    fn merge_vertex_ids_mappa_vuota_tutti_ignoti() {
        // Nessuna API key Gemini / chiamata fallita -> mappa vuota -> comportamento
        // storico: tutti i modelli Vertex a finestra ignota (degrado grazioso).
        let ids = vec!["gemini-2.5-pro".to_string(), "gemma".to_string()];
        let metas = merge_ids_with_declared_windows(ids, &std::collections::HashMap::new());
        assert!(metas.iter().all(|m| m.context_window.is_none()));
    }

    #[test]
    fn next_page_token_presente_assente_vuoto() {
        // Presente: il listing segue la pagina successiva.
        let body = serde_json::json!({
            "models": [{ "name": "models/gemini-2.5-flash", "inputTokenLimit": 1048576 }],
            "nextPageToken": "Ch4KHG1vZGVscy9nZW1pbmktMi41LWZsYXNo"
        });
        assert_eq!(
            next_google_page_token(&body).as_deref(),
            Some("Ch4KHG1vZGVscy9nZW1pbmktMi41LWZsYXNo")
        );
        // Assente o vuoto/spazi: ultima pagina, il loop si ferma.
        assert!(next_google_page_token(&serde_json::json!({ "models": [] })).is_none());
        assert!(next_google_page_token(&serde_json::json!({ "nextPageToken": "" })).is_none());
        assert!(next_google_page_token(&serde_json::json!({ "nextPageToken": "  " })).is_none());
    }

    #[test]
    fn parse_models_meta_deduplica_e_ordina_per_id() {
        let body = serde_json::json!({
            "models": [
                { "name": "models/gemini-b", "inputTokenLimit": 200 },
                { "name": "models/gemini-a", "inputTokenLimit": 100 },
                { "name": "models/gemini-b", "inputTokenLimit": 300 }, // duplicato
            ]
        });
        let metas = parse_google_models_meta_response(&body);
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].id, "gemini-a");
        assert_eq!(metas[1].id, "gemini-b");
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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();

        assert_eq!(json["systemInstruction"]["parts"][0]["text"], "istruzione");
        // Solo lo user finisce in contents.
        assert_eq!(json["contents"].as_array().unwrap().len(), 1);
        assert_eq!(json["contents"][0]["role"], "user");
        assert_eq!(json["contents"][0]["parts"][0]["text"], "domanda");
        assert_eq!(json["generationConfig"]["temperature"], 0.5);
        assert_eq!(json["generationConfig"]["maxOutputTokens"], 500);
    }

    #[test]
    fn response_format_json_object_diventa_response_mime_type() {
        let req = LlmRequest {
            model: "gemini-x".to_string(),
            messages: vec![msg("user", "restituisci json")],
            temperature: None,
            max_tokens: None,
            tools: None,
            response_format: Some(serde_json::json!({"type": "json_object"})),
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
        };
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
        assert_eq!(
            json["generationConfig"]["responseMimeType"],
            "application/json"
        );
        assert!(json["generationConfig"].get("responseSchema").is_none());
    }

    #[test]
    fn response_format_json_schema_inoltra_response_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}},
            "required": ["answer"]
        });
        let req = LlmRequest {
            model: "gemini-x".to_string(),
            messages: vec![msg("user", "restituisci json")],
            temperature: None,
            max_tokens: None,
            tools: None,
            response_format: Some(serde_json::json!({
                "type": "json_schema",
                "json_schema": {"schema": schema}
            })),
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: metadata(),
        };
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
        assert_eq!(
            json["generationConfig"]["responseMimeType"],
            "application/json"
        );
        assert_eq!(
            json["generationConfig"]["responseSchema"]["properties"]["answer"]["type"],
            "string"
        );
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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
        assert_eq!(
            json["toolConfig"]["functionCallingConfig"]["mode"],
            "ANY"
        );

        // Oggetto funzione -> mode ANY + allowedFunctionNames.
        let req2 = req_tool_choice(
            serde_json::json!({"type": "function", "function": {"name": "search"}}),
            true,
        );
        let json2 = serde_json::to_value(build_request_body(&req2, GoogleThinking::Absent)).unwrap();
        assert_eq!(json2["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
        assert_eq!(
            json2["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"][0],
            "search"
        );

        // "none" -> mode NONE.
        let req3 = req_tool_choice(serde_json::json!("none"), true);
        let json3 = serde_json::to_value(build_request_body(&req3, GoogleThinking::Absent)).unwrap();
        assert_eq!(json3["toolConfig"]["functionCallingConfig"]["mode"], "NONE");
    }

    #[test]
    fn tool_choice_senza_tools_non_aggiunge_tool_config() {
        let req = req_tool_choice(serde_json::json!("required"), false);
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
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
        assert_eq!(thinking, GoogleThinking::Enabled(2048));
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
        assert_eq!(thinking, GoogleThinking::Absent);
        let json = serde_json::to_value(build_request_body(&req, thinking)).unwrap();
        assert!(json["generationConfig"].get("thinkingConfig").is_none());
        // Output non alzato: resta il max_tokens richiesto.
        assert_eq!(json["generationConfig"]["maxOutputTokens"], 8000);
    }

    #[test]
    fn thinking_budget_usa_configurato_e_clampa() {
        // Budget configurato 50000 > max_tokens 4000 -> clamp a max_tokens.
        let req = req_thinking(true, None, Some(4000), vec![msg("user", "x")]);
        assert_eq!(resolve_thinking(&req, 50_000), GoogleThinking::Enabled(4000));
        // max_tokens sotto soglia minima -> thinking disattivato.
        let req2 = req_thinking(true, Some(1024), Some(100), vec![msg("user", "x")]);
        assert_eq!(resolve_thinking(&req2, 8192), GoogleThinking::Absent);
        // Nessun max_tokens -> non dimensionabile -> disattivato.
        let req3 = req_thinking(true, Some(1024), None, vec![msg("user", "x")]);
        assert_eq!(resolve_thinking(&req3, 8192), GoogleThinking::Absent);
    }

    #[test]
    fn gate_tool_forza_thinking_off_esplicito() {
        // Fix incidente "google 0 enabled": una richiesta con `tools` (run
        // agentico tipico: mcp-core passa thinking=None ma allega i tool) DEVE
        // produrre un thinkingConfig ESPLICITO con budget 0 / includeThoughts
        // false, a prescindere da req.thinking. Lasciarlo assente (ramo storico)
        // farebbe usare a Gemini il thinking ON di default -> incompatibile col
        // function-calling -> MALFORMED_FUNCTION_CALL -> cooldown.
        let req = req_with_tools(vec![msg("user", "leggi x")], true);
        // Sanity: nessuna preferenza esplicita di thinking nel contratto.
        assert!(req.thinking.is_none());
        assert!(req.tool_choice.is_none());
        let thinking = resolve_thinking(&req, 8192);
        assert_eq!(thinking, GoogleThinking::DisabledForTools);

        let json = serde_json::to_value(build_request_body(&req, thinking)).unwrap();
        let tc = &json["generationConfig"]["thinkingConfig"];
        // thinkingConfig PRESENTE (non assente) e thinking spento.
        assert!(!tc.is_null(), "thinkingConfig deve essere esplicito con i tool");
        assert_eq!(tc["thinkingBudget"], 0);
        assert_eq!(tc["includeThoughts"], false);
        // max_output_tokens NON alzato dal budget (thinking spento).
        assert_eq!(json["generationConfig"]["maxOutputTokens"], 1024);
    }

    #[test]
    fn gate_tool_vince_su_thinking_richiesto_esplicito() {
        // Anche se il chiamante chiede ESPLICITAMENTE thinking ON, la presenza dei
        // tool ha la precedenza: il thinking resta spento (parita' col gate
        // deepseek). Senza questa precedenza un chiamante "thinking on" sui tool
        // ritroverebbe il 400/MALFORMED.
        let mut req = req_with_tools(vec![msg("user", "leggi x")], true);
        req.thinking = Some(crate::types::ThinkingConfig {
            enabled: true,
            budget_tokens: Some(4096),
        });
        req.max_tokens = Some(8000);
        let thinking = resolve_thinking(&req, 8192);
        assert_eq!(thinking, GoogleThinking::DisabledForTools);
        let json = serde_json::to_value(build_request_body(&req, thinking)).unwrap();
        assert_eq!(json["generationConfig"]["thinkingConfig"]["thinkingBudget"], 0);
        assert_eq!(json["generationConfig"]["thinkingConfig"]["includeThoughts"], false);
        // Il tetto NON e' alzato del budget richiesto: thinking spento.
        assert_eq!(json["generationConfig"]["maxOutputTokens"], 8000);
    }

    #[test]
    fn gate_tool_scatta_anche_solo_con_tool_choice() {
        // Rilevazione simmetrica a deepseek: anche senza `tools` ma con un
        // `tool_choice` riconosciuto dal punto unico (ToolChoice::from_openai) il
        // gate scatta. Copre il force-action ("required") senza ridichiarare i tool.
        let mut req = req_with_tools(vec![msg("user", "x")], false);
        assert!(req.tools.is_none());
        req.tool_choice = Some(serde_json::json!("required"));
        assert_eq!(resolve_thinking(&req, 8192), GoogleThinking::DisabledForTools);
    }

    #[test]
    fn senza_tool_il_gate_non_scatta() {
        // Controprova: senza tool e senza tool_choice il ramo storico resta
        // intatto (thinking assente -> nessun thinkingConfig, default Gemini).
        let req = req_with_tools(vec![msg("user", "x")], false);
        assert!(req.tools.is_none());
        assert!(req.tool_choice.is_none());
        assert_eq!(resolve_thinking(&req, 8192), GoogleThinking::Absent);
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
        // Nessun thinkingConfig: nel ramo storico Gemini usa il suo default.
        let gc = &json["generationConfig"];
        assert!(gc.is_null() || gc.get("thinkingConfig").is_none());
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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
        assert!(json["contents"][0]["parts"][0]
            .get("thoughtSignature")
            .is_none());
    }

    #[test]
    fn thought_signature_per_call_su_functioncall_rispettiva() {
        // REGRESSIONE bug Gemini 3 (HTTP 400 "Function call is missing a
        // thought_signature in functionCall parts"): con PIU' tool-call ogni
        // functionCall deve ri-passare la SUA thoughtSignature sulla SUA part,
        // non tutte sulla prima. Prima del fix la firma finiva solo sulla prima
        // part (pattern Anthropic per-messaggio) e le altre functionCall
        // restavano senza -> 400.
        let assistant = LlmMessage {
            role: "assistant".to_string(),
            content: MessageContent::Text(String::new()),
            tool_call_id: None,
            tool_calls: Some(vec![
                LlmToolCall {
                    id: "c1".to_string(),
                    kind: "function".to_string(),
                    function: ToolFunctionCall {
                        name: "read_file".to_string(),
                        arguments: "{}".to_string(),
                    },
                    thought_signature: Some("sigA".to_string()),
                },
                LlmToolCall {
                    id: "c2".to_string(),
                    kind: "function".to_string(),
                    function: ToolFunctionCall {
                        name: "dispatch_subagent".to_string(),
                        arguments: "{}".to_string(),
                    },
                    thought_signature: Some("sigB".to_string()),
                },
            ]),
            name: None,
            thinking_signature: None,
            reasoning: None,
        };
        let json =
            serde_json::to_value(build_request_body(&req_with(assistant), GoogleThinking::Absent))
                .unwrap();
        let parts = &json["contents"][0]["parts"];
        // Ogni functionCall porta la propria signature sulla propria part.
        assert_eq!(parts[0]["functionCall"]["name"], "read_file");
        assert_eq!(parts[0]["thoughtSignature"], "sigA");
        assert_eq!(parts[1]["functionCall"]["name"], "dispatch_subagent");
        assert_eq!(parts[1]["thoughtSignature"], "sigB");
    }

    #[test]
    fn parsing_cattura_thought_signature_per_functioncall() {
        // La thoughtSignature di una part `functionCall` in risposta deve finire
        // sulla tool-call corrispondente (per-call), non solo a livello messaggio.
        let raw = r#"{
            "candidates": [{
                "content": {"parts": [
                    {"functionCall": {"name": "dispatch_subagent", "args": {}}, "thoughtSignature": "sig-fc"}
                ]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
        }"#;
        let parsed: GenerateContentResponse = serde_json::from_str(raw).unwrap();
        let resp = from_generate_response(parsed, "gemini-x".to_string(), 0);
        let tcs = resp.tool_calls.expect("tool_calls presenti");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].function.name, "dispatch_subagent");
        assert_eq!(tcs[0].thought_signature.as_deref(), Some("sig-fc"));
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
            reasoning: None,
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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
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

    #[test]
    fn clean_schema_normalizza_type_uppercase_e_rimuove_skip() {
        // Gemini functionDeclarations rifiuta con 400 invalid_argument sia le chiavi
        // JSON-Schema extra sia il `type` lowercase: clean_schema_for_google deve
        // rimuovere le prime E normalizzare il secondo a UPPERCASE, ricorsivamente.
        let raw = serde_json::json!({
            "type": "object",
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "X",
            "additionalProperties": false,
            "properties": {
                "path": { "type": "string", "default": "." },
                "items": { "type": "array", "items": { "type": "integer" } },
                // property NOMINATA "type": il suo VALORE e' uno schema (Object),
                // NON il campo `type` -> non va trattata come tipo da uppercasare.
                "type": { "type": "string" }
            }
        });
        let cleaned = clean_schema_for_google(&raw);
        assert_eq!(cleaned["type"], "OBJECT", "type di primo livello -> UPPERCASE");
        assert!(cleaned.get("$schema").is_none(), "$schema rimosso");
        assert!(cleaned.get("title").is_none(), "title rimosso");
        assert!(
            cleaned.get("additionalProperties").is_none(),
            "additionalProperties rimosso"
        );
        assert_eq!(cleaned["properties"]["path"]["type"], "STRING");
        assert!(
            cleaned["properties"]["path"].get("default").is_none(),
            "default rimosso"
        );
        assert_eq!(cleaned["properties"]["items"]["type"], "ARRAY");
        assert_eq!(cleaned["properties"]["items"]["items"]["type"], "INTEGER");
        // property nominata "type": la chiave-nome resta, il suo schema interno e'
        // normalizzato (type:string -> STRING).
        assert_eq!(cleaned["properties"]["type"]["type"], "STRING");
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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
        let decls = &json["tools"][0]["functionDeclarations"];
        assert_eq!(decls[0]["name"], "read_file");
        assert_eq!(decls[0]["description"], "legge un file");
        // `type` normalizzato a UPPERCASE per Gemini functionDeclarations (fix 2774928f).
        assert_eq!(decls[0]["parameters"]["type"], "OBJECT");
        assert_eq!(decls[0]["parameters"]["properties"]["path"]["type"], "STRING");
    }

    #[test]
    fn schema_pulito_rimuove_chiavi_non_supportate() {
        // Le chiavi JSON-Schema non supportate da Gemini vengono rimosse a TUTTI
        // i livelli di annidamento (properties + items), non solo alla radice.
        let req = req_with_tools(vec![msg("user", "leggi x")], true);
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
        let params = &json["tools"][0]["functionDeclarations"][0]["parameters"];
        // Radice ripulita.
        assert!(params.get("$schema").is_none());
        assert!(params.get("title").is_none());
        assert!(params.get("additionalProperties").is_none());
        // Property annidata ripulita (title/default rimossi, type UPPERCASE 2774928f).
        let path = &params["properties"]["path"];
        assert_eq!(path["type"], "STRING");
        assert!(path.get("title").is_none());
        assert!(path.get("default").is_none());
        // items annidato in array ripulito.
        let nested = &params["properties"]["lines"]["items"];
        assert!(nested.get("additionalProperties").is_none());
        assert_eq!(nested["properties"]["n"]["type"], "INTEGER");
        // required (vocabolario supportato) preservato.
        assert_eq!(params["required"][0], "path");
    }

    #[test]
    fn clean_schema_funzione_pura() {
        // Test diretto della funzione pura: chiavi note rimosse, struttura
        // preservata, `type` normalizzato a UPPERCASE per Gemini (STRING/OBJECT/...).
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
        assert_eq!(cleaned["type"], "OBJECT");
        assert!(cleaned.get("additionalProperties").is_none());
        assert!(cleaned.get("$defs").is_none());
        assert!(cleaned.get("definitions").is_none());
        assert!(cleaned.get("examples").is_none());
        assert_eq!(cleaned["properties"]["a"]["type"], "STRING");
        assert!(cleaned["properties"]["a"].get("default").is_none());
    }

    #[test]
    fn tool_choice_required_con_tools_emette_sia_tool_config_sia_tools() {
        // Rafforza il test storico: con tool_choice="required" + tools, ora il
        // body porta SIA toolConfig(mode=ANY) SIA le functionDeclarations (prima
        // le declarations mancavano e mode=ANY era inerte).
        let mut req = req_with_tools(vec![msg("user", "trova")], true);
        req.tool_choice = Some(serde_json::json!("required"));
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
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
            thought_signature: None,
        }]);
        let req = req_with_tools(vec![a], true);
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
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
            thought_signature: None,
        }]);
        let mut tool = msg("tool", "contenuto del file");
        tool.tool_call_id = Some("call_x".to_string());
        let req = req_with_tools(vec![a, tool], true);
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
        let contents = json["contents"].as_array().unwrap();
        assert!(
            contents.is_empty(),
            "il functionResponse orfano di testa va scartato, contents = {contents:?}"
        );
    }

    // --- Discovery + fallback di region Vertex (mig 0476) ------------------

    #[test]
    fn page_size_vertex_entro_il_tetto_di_300() {
        // Vertex publishers.models.list RIFIUTA con 400 INVALID_ARGUMENT una
        // pageSize > 300 (verificato live 2026-07-07: "Page size should be
        // non-negative and the maximum size is 300"). Sentinella di regressione:
        // se qualcuno ri-condivide la costante Gemini (1000) o alza il tetto, il
        // discovery Vertex torna a fallire su ogni region -> questo test lo blocca.
        let vertex: u32 = VERTEX_MODELS_PAGE_SIZE
            .parse()
            .expect("pageSize Vertex numerica");
        assert!(
            (1..=300).contains(&vertex),
            "pageSize Vertex fuori dal tetto [1,300]: {vertex}"
        );
        // La Gemini API tollera fino a 1000 (oltre, clampa): la costante non deve
        // superare quel massimo documentato.
        let gemini: u32 = GEMINI_MODELS_PAGE_SIZE
            .parse()
            .expect("pageSize Gemini numerica");
        assert!(
            (1..=1000).contains(&gemini),
            "pageSize Gemini fuori da [1,1000]: {gemini}"
        );
    }

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
    fn order_regions_probed_valida_messa_per_prima() {
        // Region persistita valida (fra le candidate) -> per prima, il resto come
        // fallback con l'ordine relativo preservato.
        let discovery = vec![
            "europe-west4".to_string(),
            "global".to_string(),
            "us-central1".to_string(),
        ];
        let ordered = order_regions_with_probed(Some("global"), &discovery);
        assert_eq!(ordered, vec!["global", "europe-west4", "us-central1"]);
    }

    #[test]
    fn order_regions_probed_gia_in_testa_e_idempotente() {
        // Region persistita gia' prima candidata: nessun cambiamento d'ordine.
        let discovery = vec!["europe-west4".to_string(), "global".to_string()];
        let ordered = order_regions_with_probed(Some("europe-west4"), &discovery);
        assert_eq!(ordered, vec!["europe-west4", "global"]);
    }

    #[test]
    fn order_regions_probed_non_candidata_ignorata() {
        // Region persistita non piu' fra le discovery_locations (es. rimossa dal
        // deploy): ignorata, si torna all'ordine di preferenza.
        let discovery = vec!["europe-west4".to_string(), "global".to_string()];
        let ordered = order_regions_with_probed(Some("asia-east1"), &discovery);
        assert_eq!(ordered, vec!["europe-west4", "global"]);
    }

    #[test]
    fn order_regions_probed_assente_usa_discovery() {
        // Nessuna region persistita -> discovery_locations intatte, in ordine.
        let discovery = vec!["europe-west4".to_string(), "global".to_string()];
        let ordered = order_regions_with_probed(None, &discovery);
        assert_eq!(ordered, discovery);
    }

    #[tokio::test]
    async fn vertex_regions_gemini_ritorna_none() {
        // Backend Gemini: nessuna region -> None (invio singolo su API key).
        let p = GoogleProvider::new(Client::new(), "k", None);
        assert!(p
            .vertex_regions_for_model(&GoogleBackend::Gemini, "gemini-x")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn vertex_regions_usa_cache_se_presente() {
        // Se la cache conosce una region per il modello, e' l'unica provata
        // (le richieste successive saltano il fallback 404). Senza `db` la
        // lettura persistita e' un no-op (FAIL-OPEN), quindi resta l'ordine di
        // discovery: il comportamento del path senza DB e' invariato.
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
            .await
            .unwrap();
        assert_eq!(before, vec!["europe-west4", "global"]);
        // Dopo aver cachato 'global' per quel modello: solo 'global'.
        p.vertex_model_region
            .insert("gemini-3.5-flash".to_string(), "global".to_string());
        let after = p
            .vertex_regions_for_model(&backend, "gemini-3.5-flash")
            .await
            .unwrap();
        assert_eq!(after, vec!["global"]);
        // Un modello diverso resta sull'ordine di discovery completo.
        let other = p
            .vertex_regions_for_model(&backend, "gemini-2.5-pro")
            .await
            .unwrap();
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
                    thought_signature: None,
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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();

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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();

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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();

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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();

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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();

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
        let json = serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();

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

    // --- Quirk thinking_budget=0 (retry-su-400) ----------------------------

    #[test]
    fn riconosce_errore_thinking_budget_e_ignora_gli_altri_400() {
        // Il messaggio REALE di produzione (gemini-2.5-pro col gate tool) deve
        // innescare il riconoscimento, anche con maiuscole/punteggiatura diverse.
        assert!(is_thinking_budget_error(
            "The model does not support setting thinking_budget to 0."
        ));
        assert!(is_thinking_budget_error(
            r#"{"error":{"code":400,"message":"thinking_budget out of range","status":"INVALID_ARGUMENT"}}"#
        ));
        // Un 400 di NATURA DIVERSA (schema invalido, mismatch function-call) NON
        // deve essere scambiato per il quirk: niente retry-senza-thinking li'.
        assert!(!is_thinking_budget_error(
            "Please ensure that the number of function response parts is equal to the number of function call parts"
        ));
        assert!(!is_thinking_budget_error(
            r#"{"error":{"message":"Invalid JSON payload received."}}"#
        ));
        assert!(!is_thinking_budget_error(""));
    }

    #[test]
    fn retry_ricostruisce_il_body_senza_thinking_config() {
        // Cuore del retry-su-400: il body INIZIALE (gate tool -> DisabledForTools)
        // porta un thinkingConfig esplicito con thinkingBudget=0 (cio' che
        // gemini-2.5-pro rifiuta); il body del RETRY (GoogleThinking::Absent, cioe'
        // thinkingConfig OMESSO) NON deve contenere alcun thinkingConfig, cosi'
        // Gemini applica il suo default e il 400 sparisce. Questa e' la funzione
        // pura di costruzione del body che send_with_thinking_retry riusa: testarla
        // qui copre l'invariante senza bisogno di un mock HTTP.
        let req = req_with_tools(vec![msg("user", "leggi x")], true);

        // Replica la risoluzione del gate: con tool presenti -> DisabledForTools.
        let initial = resolve_thinking(&req, 8192);
        assert_eq!(initial, GoogleThinking::DisabledForTools);
        let body_iniziale = serde_json::to_value(build_request_body(&req, initial)).unwrap();
        assert_eq!(
            body_iniziale["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            0,
            "il body iniziale deve portare il thinkingBudget=0 che pro rifiuta"
        );

        // Body del retry: ricostruito con Absent come fa send_with_thinking_retry.
        let body_retry =
            serde_json::to_value(build_request_body(&req, GoogleThinking::Absent)).unwrap();
        assert!(
            body_retry["generationConfig"]
                .get("thinkingConfig")
                .is_none(),
            "il body del retry NON deve contenere thinkingConfig"
        );
        // I tool restano dichiarati nel retry: omettiamo SOLO il thinkingConfig,
        // non i function-declarations (la richiesta resta una richiesta agentica).
        assert_eq!(
            body_retry["tools"][0]["functionDeclarations"][0]["name"],
            "read_file"
        );
    }
}
