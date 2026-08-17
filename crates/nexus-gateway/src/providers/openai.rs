//! Provider OpenAI.
//!
//! Porting di `packages/llm-gateway/src/providers/openai.ts` + parita' con
//! `brain/providers/openai_provider.py`. Delega il trasporto al client condiviso
//! [`OpenAiCompatClient`] (composizione, regola L); aggiunge la detection
//! o-series (per nome modello) che cambia il dialetto reasoning: i modelli
//! reasoning (o1/o3/o4, gpt-5*, gpt-4.5*) non accettano temperatura e ammettono
//! `reasoning_effort`. Il tetto di output in `max_completion_tokens` NON e' del
//! dialetto: OpenAI ha deprecato `max_tokens` per l'INTERO parco, chat compresi,
//! e lo dichiara il costruttore (vedi
//! [`OpenAiCompatClient::with_tetto_su_completion`]).

use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use nexus_cache::TtlCache;
use reqwest::Client;
use sqlx::PgPool;

use crate::provider::{ChunkStream, LlmProvider};
use crate::providers::openai_compat::{OpenAiCompatClient, ReasoningDialect, ResolvedReasoning};
use crate::tassonomia_errori::e_capacita_flex;
use crate::types::{
    ImageGenRequest, ImageGenResponse, LlmRequest, LlmResponse, PromptCacheKeying, SensitivityTier,
    TranscribeRequest, TranscribeResponse, TtsRequest, TtsResponse,
};

/// Il nome con cui questo fornitore si presenta ovunque: e' la chiave con cui
/// il catalogo, il registry, il ledger e i log lo nominano, quindi una sola
/// definizione (stessa disciplina di `KimiProvider`).
const PROVIDER_NAME: &str = "openai";

/// Tier ammessi: pubblico/interno/confidenziale (mai tier 3, riservato a onprem).
const TIERS: &[SensitivityTier] = &[0, 1, 2];

/// Endpoint OpenAI di default. La `base_url` resta un parametro del costruttore
/// (override per gateway compatibili); questo valore e' solo il default quando
/// il chiamante non ne passa una.
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Chiave settings (regola G) del livello di reasoning per i modelli o-series.
/// Valori ammessi dall'API: `low`/`medium`/`high`. Assente => non si invia
/// `reasoning_effort` (l'API usa il default del modello): nessun hardcoded.
const REASONING_EFFORT_SETTING: &str = "providers.openai.reasoning_effort";

/// TTL della cache settings (60s, come gli altri provider).
const SETTINGS_TTL: Duration = Duration::from_secs(60);

/// Famiglie reasoning (gpt-5*, gpt-4.5*) coperte per PREFISSO: ogni release
/// (gpt-5.1, gpt-5-mini, ...) e' gestita senza elencarla a mano (parita' col
/// Python `_is_o_series` ~104, regola G: famiglia strutturale, non nome esatto).
const O_SERIES_FAMILY_PREFIXES: &[&str] = &["gpt-5", "gpt-4.5"];

/// Basi o-series (o1/o3/o4) trattate per match esatto o `base-...` (parita' col
/// Python `_O_SERIES_MODELS`: `m == base || m.starts_with(base + "-")`). Non un
/// `starts_with` puro, che catturerebbe per errore nomi come `o1abc`.
const O_SERIES_BASES: &[&str] = &["o1", "o3", "o4"];

/// Chiave settings (regola G) dell'interruttore del tier differibile. Seed
/// `'false'` (mig **0729**): il meccanismo nasce SPENTO e si accende dal DB,
/// senza redeploy, con la TTL di 60s delle altre cache.
const FLEX_ENABLED_SETTING: &str = "providers.openai.flex_enabled";

/// Il tier che openai chiama la propria corsia differibile: meta' prezzo,
/// nessuna garanzia di latenza, e un 429 dedicato quando la capacita' non c'e'.
const SERVICE_TIER_FLEX: &str = "flex";

/// Il fornitore accetta la corsia differibile su QUESTO modello?
///
/// Sta nel catalogo e non in un elenco di nomi qui (regola G): la platea e' del
/// FORNITORE e cambia quando lui la cambia, mentre un elenco nel codice
/// resterebbe fermo e servirebbe un redeploy per seguirlo. Stessa forma con cui
/// kimi legge la disattivabilita' del pensiero, e per la stessa ragione.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlexAmmesso {
    /// Il catalogo lo dichiara ammesso.
    Si,
    /// Il catalogo lo dichiara NON ammesso.
    No,
    /// Nessuno ha dichiarato niente: riga assente, colonna `NULL`, DB muto o
    /// non agganciato. Non e' un permesso (regola Q: l'ignoto e' una variante,
    /// non un valore comodo).
    NonDichiarato,
}

impl FlexAmmesso {
    fn dal_catalogo(v: Option<bool>) -> Self {
        match v {
            Some(true) => Self::Si,
            Some(false) => Self::No,
            None => Self::NonDichiarato,
        }
    }

    /// L'unico punto in cui questo fatto diventa un permesso.
    fn consente_la_corsia(self) -> bool {
        matches!(self, Self::Si)
    }
}

/// In quale corsia parte QUESTA richiesta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SceltaCorsia {
    /// Come e' sempre partita: questo driver non aggiunge alcun `service_tier`.
    ///
    /// E' anche il caso del tier PINNATO dal chiamante, che viaggia gia' nel
    /// corpo per passthrough e non e' una scelta nostra — percio' non e' nostro
    /// nemmeno il ripiego: chi pinna un tier ha deciso, e un rifiuto per
    /// capacita' deve arrivargli intero (lo raccoglie la guardia sulla causa
    /// `flex_capacity` in `complete_with_retry`).
    Storica,
    /// Con `service_tier: "flex"`, e un rifiuto per capacita' la fa ripiegare
    /// UNA volta sulla corsia storica.
    Differibile,
}

/// Le tre condizioni della corsia differibile, in un punto solo e senza IO.
///
/// Sono in CONGIUNZIONE e nessuna e' ridondante, perche' rispondono a domande
/// di tre proprietari diversi: il CHIAMANTE dice se l'esito puo' attendere
/// (`deferrable`), l'INSTALLAZIONE se la corsia e' accesa (setting, regola G),
/// il FORNITORE su quali modelli esista (catalogo). Togliendone una qualsiasi
/// il meccanismo decide al posto di chi ne ha titolo.
///
/// Il tier PINNATO le precede tutte e non e' una quarta condizione: e' la
/// dichiarazione che la scelta non spetta a noi.
fn scelta_corsia(
    tier_pinnato: bool,
    puo_attendere: bool,
    interruttore: bool,
    ammesso: FlexAmmesso,
) -> SceltaCorsia {
    if tier_pinnato || !puo_attendere || !interruttore || !ammesso.consente_la_corsia() {
        return SceltaCorsia::Storica;
    }
    SceltaCorsia::Differibile
}

/// La stessa richiesta, come parte nella corsia differibile.
fn con_corsia_differibile(req: &LlmRequest) -> LlmRequest {
    let mut r = req.clone();
    r.service_tier = Some(SERVICE_TIER_FLEX.to_string());
    r
}

/// True se il modello richiede il dialetto reasoning o-series. Case-insensitive.
/// Parita' fedele col Python: prefisso per le famiglie gpt-5/gpt-4.5, match
/// esatto o `base-` per o1/o3/o4.
fn is_o_series(model: &str) -> bool {
    let m = model.to_lowercase();
    if O_SERIES_FAMILY_PREFIXES.iter().any(|p| m.starts_with(p)) {
        return true;
    }
    O_SERIES_BASES
        .iter()
        .any(|b| m == *b || m.starts_with(&format!("{b}-")))
}

pub struct OpenAiProvider {
    client: OpenAiCompatClient,
    db: Option<PgPool>,
    reasoning_effort: TtlCache<(), Option<String>>,
    /// L'interruttore del tier differibile, dai settings (chiave `()`: e' una
    /// decisione dell'installazione, non del modello).
    flex_enabled: TtlCache<(), bool>,
    /// La platea del tier differibile, PER MODELLO: e' del fornitore, cambia
    /// per modello, e sta nel catalogo. Vi entra solo cio' che si e' letto — un
    /// errore non e' una misura, e scriverlo qui cristallizzerebbe per 60s
    /// un'ignoranza momentanea.
    flex_ammesso: TtlCache<String, FlexAmmesso>,
}

impl OpenAiProvider {
    /// Costruisce il provider senza accesso DB (test di mappatura). L'effort
    /// reasoning non sara' leggibile dai settings: si usa il default del modello.
    pub fn new(http: Client, api_key: impl Into<String>, base_url: Option<String>) -> Self {
        Self::with_db(http, api_key, base_url, None)
    }

    /// Costruisce il provider con accesso DB per leggere `reasoning_effort` dai
    /// settings (regola G). `base_url` opzionale (default OpenAI ufficiale); la
    /// `api_key` e' iniettata dal chiamante (regola F: niente segreti nel codice).
    pub fn with_db(
        http: Client,
        api_key: impl Into<String>,
        base_url: Option<String>,
        db: Option<PgPool>,
    ) -> Self {
        let base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Self {
            // CACHE: `prompt_cache_key` e' un parametro dell'API OpenAI, il
            // dialetto da cui Mistral lo eredita. Ma qui NON e' la differenza
            // fra cacheare e non cacheare, come invece e' su Mistral: misurato
            // il 29/07/2026, gpt-4o-mini riusa 11.392 token su 11.469 anche
            // SENZA la chiave, e con la chiave il risultato e' identico.
            // La si manda lo stesso perche' e' l'hint di affinita' che il
            // provider documenta: il riuso automatico dipende da quale nodo
            // serve la richiesta, e senza chiave quella scelta e' casuale. Che
            // il caso esista non e' un'ipotesi: nello stesso disegno di prova
            // mistral-small, che pure cachea da solo, ha riusato il prefisso
            // una volta su due. Rischio nullo (campo nativo del dialetto),
            // guadagno atteso sui carichi distribuiti.
            // TETTO: `max_tokens` e' deprecato dal PROVIDER per l'intera
            // famiglia (doc API reference: "deprecated in favor of
            // max_completion_tokens"), non dai soli modelli reasoning — anche
            // i chat (gpt-4o*) lo accettano. Percio' la dichiarazione sta sul
            // client e non sul dialetto: finche' viveva nel dialetto, un
            // modello non-reasoning partiva col campo deprecato.
            client: OpenAiCompatClient::new(http, base_url, api_key, PROVIDER_NAME)
                .with_prompt_cache_keying(PromptCacheKeying::RequiresKey)
                .with_tetto_su_completion(),
            db,
            reasoning_effort: TtlCache::new(SETTINGS_TTL),
            flex_enabled: TtlCache::new(SETTINGS_TTL),
            flex_ammesso: TtlCache::new(SETTINGS_TTL),
        }
    }

    /// Livello reasoning dai settings (cache TTL 60s). `None` => chiave assente o
    /// DB irraggiungibile: non si invia `reasoning_effort` (default del modello).
    async fn configured_effort(&self) -> Option<String> {
        if let Some(e) = self.reasoning_effort.get(&()) {
            return e;
        }
        let db = self.db.as_ref()?;
        let value = nexus_auth::get_setting(db, REASONING_EFFORT_SETTING)
            .await
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        self.reasoning_effort.insert((), value.clone());
        value
    }

    /// L'interruttore della corsia differibile, dai settings (cache TTL 60s).
    ///
    /// Ogni via di fuga porta a `false`: chiave assente, valore non
    /// riconoscibile, nessun DB agganciato. Il meccanismo nasce SPENTO (seed
    /// `'false'`, mig 0729) e l'errore cade dalla parte del comportamento di
    /// ieri — un'installazione che non ha deciso non manda le proprie richieste
    /// in una corsia senza garanzie di latenza.
    async fn flex_abilitato(&self) -> bool {
        if let Some(v) = self.flex_enabled.get(&()) {
            return v;
        }
        let Some(db) = self.db.as_ref() else {
            return false;
        };
        let acceso = nexus_auth::get_setting(db, FLEX_ENABLED_SETTING)
            .await
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
        self.flex_enabled.insert((), acceso);
        acceso
    }

    /// Se il fornitore accetti la corsia differibile su QUESTO modello, dal
    /// catalogo (mig 0729).
    ///
    /// Stessa disciplina di `KimiProvider::pensiero_spegnibile`, e per la stessa
    /// ragione: ogni via di fuga porta a [`FlexAmmesso::NonDichiarato`], che non
    /// e' un permesso. Il costo dell'errore non e' simmetrico — non mandare in
    /// flex una richiesta che poteva andarci costa il pieno prezzo di UNA
    /// chiamata, mandarcene una che non puo' andarci costa un 400 su ogni
    /// chiamata a quel modello finche' la TTL non scade.
    ///
    /// L'esito di un errore NON entra in cache: un guasto momentaneo del DB
    /// cristallizzerebbe per 60s un'ignoranza, invece di essere ri-chiesto al
    /// giro dopo.
    async fn flex_ammesso(&self, model: &str) -> FlexAmmesso {
        if let Some(v) = self.flex_ammesso.get(model) {
            return v;
        }
        let Some(db) = self.db.as_ref() else {
            return FlexAmmesso::NonDichiarato;
        };
        // `Option<Option<bool>>`: il primo livello e' la riga, il secondo la
        // colonna nullable. Entrambi gli assenti dicono la stessa cosa.
        let letto: Result<Option<(Option<bool>,)>, sqlx::Error> = sqlx::query_as(
            "SELECT supports_flex FROM ai_price_catalog WHERE provider = $1 AND model = $2",
        )
        .bind(self.name())
        .bind(model)
        .fetch_optional(db)
        .await;
        match letto {
            Ok(row) => {
                let esito = FlexAmmesso::dal_catalogo(row.and_then(|(v,)| v));
                self.flex_ammesso.insert(model.to_string(), esito);
                esito
            }
            Err(e) => {
                // Regola F: nei campi solo identificatori di configurazione e la
                // causa strutturata dell'errore, mai il payload.
                tracing::warn!(
                    provider = PROVIDER_NAME,
                    model = %model,
                    error = %e,
                    "ammissibilita' della corsia differibile non leggibile dal \
                     catalogo: la richiesta parte al prezzo pieno"
                );
                FlexAmmesso::NonDichiarato
            }
        }
    }

    /// In quale corsia parte questa richiesta: raccoglie i tre fatti e delega la
    /// decisione a [`scelta_corsia`], che e' pura.
    ///
    /// I due fatti che costano IO si leggono solo se possono ancora cambiare
    /// l'esito: la congiunzione si chiude al primo `false`, e i due che si
    /// leggono dalla richiesta non costano niente. Non e' una seconda copia del
    /// criterio — e' il rifiuto di pagare, su OGNI chiamata a openai, letture
    /// il cui risultato e' gia' ininfluente.
    async fn corsia(&self, req: &LlmRequest) -> SceltaCorsia {
        if req.service_tier.is_some() || !req.deferrable {
            return SceltaCorsia::Storica;
        }
        let interruttore = self.flex_abilitato().await;
        let ammesso = if interruttore {
            self.flex_ammesso(&req.model).await
        } else {
            FlexAmmesso::NonDichiarato
        };
        scelta_corsia(false, true, interruttore, ammesso)
    }

    /// Risolve il dialetto reasoning per la richiesta: o-series se il nome del
    /// modello lo richiede, altrimenti dialetto base. L'`effort` arriva dai
    /// settings solo per o-series.
    async fn resolve(&self, req: &LlmRequest) -> ResolvedReasoning {
        if !is_o_series(&req.model) {
            return ResolvedReasoning::none();
        }
        ResolvedReasoning {
            dialect: ReasoningDialect::OpenAiReasoning,
            // o-series e' sempre in reasoning mode (non disattivabile via param):
            // `enabled` informativo, il comportamento e' guidato dal dialetto.
            enabled: true,
            effort: self.configured_effort().await,
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn max_context_tokens(&self) -> u32 {
        128_000
    }

    fn tier_compatibility(&self) -> &[SensitivityTier] {
        TIERS
    }

    /// Il RIPIEGO in-driver e' qui e non nella chain, e la ragione e' che il
    /// rifiuto per capacita' della corsia differibile non e' un fatto del
    /// FORNITORE: openai e' sano e servirebbe subito al tier standard. Farlo
    /// salire significherebbe consumare un anello di chain (o un cooldown) per
    /// una condizione che si risolve rimandando la STESSA richiesta allo STESSO
    /// modello un istante dopo, senza il campo. Nel caso normale la chain, il
    /// registro dei cooldown e la tassonomia non lo vedono mai.
    ///
    /// UNA sola volta: se anche il tier standard rifiuta, l'errore e' suo e
    /// deve arrivare intero a chi sa fare failover.
    async fn complete(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
        let reasoning = self.resolve(req).await;
        if self.corsia(req).await == SceltaCorsia::Storica {
            return self.client.complete_with_reasoning(req, &reasoning).await;
        }
        let differibile = con_corsia_differibile(req);
        match self
            .client
            .complete_with_reasoning(&differibile, &reasoning)
            .await
        {
            Err(e) if e_capacita_flex(&e) => {
                registra_ripiego(&req.model, "complete");
                self.client.complete_with_reasoning(req, &reasoning).await
            }
            esito => esito,
        }
    }

    /// Stesso ripiego del non-streaming (vedi [`Self::complete`]): la corsia e'
    /// una proprieta' della RICHIESTA, non del trasporto, e un ripiego su un
    /// percorso solo sarebbe una promessa che sparisce appena la chiamata va in
    /// streaming — lo stesso difetto che `corpo_della_richiesta` esiste per
    /// evitare sui campi del corpo.
    ///
    /// Il rifiuto arriva comunque prima del primo chunk (e' lo status della
    /// risposta iniziale), quindi qui non c'e' nulla di gia' emesso da disfare.
    async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        let reasoning = self.resolve(req).await;
        if self.corsia(req).await == SceltaCorsia::Storica {
            return self.client.stream_with_reasoning(req, &reasoning).await;
        }
        let differibile = con_corsia_differibile(req);
        match self
            .client
            .stream_with_reasoning(&differibile, &reasoning)
            .await
        {
            Err(e) if e_capacita_flex(&e) => {
                registra_ripiego(&req.model, "stream");
                self.client.stream_with_reasoning(req, &reasoning).await
            }
            esito => esito,
        }
    }

    async fn healthcheck(&self) -> bool {
        self.client.healthcheck().await
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        self.client.list_models().await
    }

    fn supports_image_gen(&self) -> bool {
        true
    }

    /// Delega al trasporto condiviso (`POST /images/generations`): stesso client
    /// HTTP/auth della chat (regola L). Il modello (es. `gpt-image-1`) arriva dal
    /// chiamante (regola G).
    async fn generate_image(&self, req: &ImageGenRequest) -> anyhow::Result<ImageGenResponse> {
        self.client
            .images_generations(&req.model, &req.prompt, req.n, req.size.as_deref())
            .await
    }

    fn supports_audio_in(&self) -> bool {
        true
    }

    /// Decodifica l'audio base64 e delega al trasporto condiviso
    /// (`POST /audio/transcriptions`, multipart): stesso client HTTP/auth della
    /// chat (regola L). Il modello (es. `whisper-1`, `gpt-4o-transcribe`) arriva
    /// dal chiamante (regola G). Il filename multipart deriva dal mime dichiarato
    /// (estensione) cosi' OpenAI inferisce il formato dell'audio.
    async fn transcribe_audio(
        &self,
        req: &TranscribeRequest,
    ) -> anyhow::Result<TranscribeResponse> {
        let audio_bytes = B64
            .decode(req.audio_base64.trim())
            .map_err(|e| anyhow::anyhow!("audio base64 non valido: {e}"))?;
        let filename = audio_filename(req.mime.as_deref());
        self.client
            .transcribe(&req.model, audio_bytes, &filename, req.language.as_deref())
            .await
    }

    fn supports_audio_out(&self) -> bool {
        true
    }

    /// Delega al trasporto condiviso (`POST /audio/speech`, JSON in -> bytes out):
    /// stesso client HTTP/auth della chat (regola L). Il modello (es.
    /// `gpt-4o-mini-tts`, `tts-1`) arriva dal chiamante (regola G). I bytes audio
    /// vengono codificati base64 per il contratto JSON del gateway.
    async fn text_to_speech(&self, req: &TtsRequest) -> anyhow::Result<TtsResponse> {
        let start = std::time::Instant::now();
        let (bytes, mime) = self
            .client
            .speech(
                &req.model,
                &req.input,
                req.voice.as_deref(),
                req.response_format.as_deref(),
            )
            .await?;
        let latency_ms = start.elapsed().as_millis() as u64;
        Ok(TtsResponse {
            audio_base64: B64.encode(&bytes),
            mime,
            model_used: req.model.clone(),
            provider_used: self.name().to_string(),
            latency_ms,
        })
    }
}

/// Il ripiego alla corsia storica lascia una riga, e la lascia a `info`.
///
/// E' l'unico segnale che quella richiesta e' costata il prezzo pieno: senza,
/// «il flex e' acceso e non risparmia niente» sarebbe indistinguibile da «il
/// flex non e' mai stato scelto», e i due hanno rimedi opposti (chiedere
/// capacita' al fornitore contro guardare le tre condizioni). Il conteggio vero
/// resta il ledger, che quella chiamata la registra comunque.
fn registra_ripiego(model: &str, percorso: &str) {
    tracing::info!(
        provider = PROVIDER_NAME,
        model = %model,
        percorso,
        service_tier = SERVICE_TIER_FLEX,
        "corsia differibile senza capacita' -> ripiego al tier standard, \
         stessa richiesta e stesso modello a prezzo pieno"
    );
}

/// Nome file multipart per l'audio, derivato dal MIME dichiarato. OpenAI usa
/// l'estensione del `file_name` per inferire il formato; senza mime usiamo `.mp3`
/// (formato piu' comune). Funzione PURA (testabile). Niente nome hardcoded di
/// business: e' solo l'estensione tecnica del file multipart.
fn audio_filename(mime: Option<&str>) -> String {
    let ext = match mime.map(|m| m.trim().to_lowercase()).as_deref() {
        Some("audio/mpeg" | "audio/mp3") => "mp3",
        Some("audio/wav" | "audio/x-wav") => "wav",
        Some("audio/mp4" | "audio/x-m4a" | "audio/m4a") => "m4a",
        Some("audio/ogg" | "audio/opus") => "ogg",
        Some("audio/flac" | "audio/x-flac") => "flac",
        Some("audio/webm") => "webm",
        _ => "mp3",
    };
    format!("audio.{ext}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LlmMessage, MessageContent, RequestMetadata};

    fn provider() -> OpenAiProvider {
        OpenAiProvider::new(Client::new(), "sk-test", None)
    }

    fn richiesta(model: &str) -> LlmRequest {
        LlmRequest {
            model: model.to_string(),
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
            temperature: Some(0.5),
            max_tokens: Some(64),
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

    /// La deprecazione di `max_tokens` e' del PROVIDER, non del dialetto: anche
    /// un modello chat non-reasoning parte con `max_completion_tokens`. Prima
    /// il tetto viveva nel predicato sul dialetto (`OpenAiReasoning | Kimi`) e
    /// un `gpt-4o-mini` — dialetto base — usciva col campo che la doc OpenAI
    /// dichiara deprecato.
    ///
    /// Attraversa `resolve` e `corpo_della_richiesta` REALI (regola O): e' la
    /// coppia dialetto+client che parte in produzione, non una composta a mano.
    /// Il verso opposto (mistral resta su `max_tokens`) e' il test gemello in
    /// mistral.rs: insieme fissano che la dichiarazione e' per-fornitore.
    ///
    /// MUTAZIONE: togliere `.with_tetto_su_completion()` dal costruttore, o
    /// riportare il tetto sul dialetto in `build_request_body` -> gpt-4o-mini
    /// risolve dialetto base e il body torna a `max_tokens`: rosso.
    #[tokio::test]
    async fn anche_un_modello_chat_porta_max_completion_tokens() {
        let p = provider();
        let req = richiesta("gpt-4o-mini");
        let reasoning = p.resolve(&req).await;
        assert_eq!(
            reasoning.dialect,
            ReasoningDialect::None,
            "premessa: gpt-4o-mini non e' o-series"
        );
        let corpo =
            serde_json::to_value(p.client.corpo_della_richiesta(&req, false, &reasoning).await)
                .expect("serializza");
        assert_eq!(corpo["max_completion_tokens"], 64);
        assert!(corpo.get("max_tokens").is_none());
        // La temperatura resta materia del DIALETTO: un chat la manda ancora.
        assert_eq!(corpo["temperature"], 0.5);
    }

    /// OpenAI cachea anche senza la chiave (misurato: 11.392 token su 11.469),
    /// quindi qui non e' un rimedio a un difetto ma l'hint di affinita' che il
    /// provider documenta. Resta dichiarato perche' il nodo che serve la
    /// richiesta altrimenti lo si sceglie a caso: vedi il costruttore.
    #[test]
    fn dichiara_la_chiave_di_cache_del_proprio_dialetto() {
        assert_eq!(
            provider().client.cache_keying(),
            PromptCacheKeying::RequiresKey
        );
    }

    #[test]
    fn capacita_dichiarate() {
        let p = provider();
        assert_eq!(p.name(), "openai");
        assert!(p.supports_tools());
        assert!(p.supports_streaming());
        assert_eq!(p.max_context_tokens(), 128_000);
        assert_eq!(p.tier_compatibility(), &[0, 1, 2]);
        // Capability media: OpenAI genera immagini, trascrive e sintetizza audio.
        assert!(p.supports_image_gen());
        assert!(p.supports_audio_in());
        assert!(p.supports_audio_out());
    }

    #[test]
    fn audio_filename_dal_mime() {
        assert_eq!(audio_filename(Some("audio/mpeg")), "audio.mp3");
        assert_eq!(audio_filename(Some("audio/wav")), "audio.wav");
        assert_eq!(audio_filename(Some("audio/mp4")), "audio.m4a");
        assert_eq!(audio_filename(Some("audio/ogg")), "audio.ogg");
        assert_eq!(audio_filename(Some("audio/flac")), "audio.flac");
        // Mime assente o sconosciuto -> default mp3.
        assert_eq!(audio_filename(None), "audio.mp3");
        assert_eq!(audio_filename(Some("application/octet-stream")), "audio.mp3");
    }

    // ── Corsia differibile (mig 0729) ────────────────────────────────────────

    /// Le tre condizioni sono in CONGIUNZIONE, e il tier pinnato le precede.
    /// Il criterio e' puro: qui si prova che nessuna delle tre e' ridondante —
    /// per ognuna esiste un caso in cui e' l'unica a dire di no.
    ///
    /// MUTAZIONE: togliere una qualunque delle quattro condizioni da
    /// `scelta_corsia` -> il suo caso qui sotto diventa `Differibile`, rosso.
    #[test]
    fn la_corsia_differibile_pretende_tutte_e_tre_le_condizioni() {
        use FlexAmmesso::*;
        use SceltaCorsia::*;
        // Il caso completo: chiamante, installazione e fornitore concordi.
        assert_eq!(scelta_corsia(false, true, true, Si), Differibile);
        // Ognuna da sola basta a farla decadere.
        assert_eq!(scelta_corsia(false, false, true, Si), Storica, "chiamante");
        assert_eq!(
            scelta_corsia(false, true, false, Si),
            Storica,
            "installazione"
        );
        assert_eq!(scelta_corsia(false, true, true, No), Storica, "fornitore");
        // L'ignoto del catalogo non e' un permesso (regola Q): un modello mai
        // misurato costa il prezzo pieno, non un 400 su ogni chiamata.
        assert_eq!(
            scelta_corsia(false, true, true, NonDichiarato),
            Storica,
            "l'ignoto non autorizza"
        );
        // Il tier PINNATO vince su tutto: chi lo dichiara ha gia' deciso, e la
        // sua decisione non e' nostra da correggere ne' da ripiegare.
        assert_eq!(scelta_corsia(true, true, true, Si), Storica, "pin");
    }

    /// Il corto circuito di [`OpenAiProvider::corsia`] non e' una seconda copia
    /// del criterio: su un provider SENZA db (interruttore spento e catalogo
    /// muto) deve dare la stessa risposta della funzione pura con quei fatti.
    #[tokio::test]
    async fn senza_db_nessuna_richiesta_prende_la_corsia_differibile() {
        let p = provider();
        let mut req = richiesta("o4-mini");
        req.deferrable = true;
        assert_eq!(p.corsia(&req).await, SceltaCorsia::Storica);
        // E il corpo che parte davvero non porta il campo.
        let reasoning = p.resolve(&req).await;
        let corpo =
            serde_json::to_value(p.client.corpo_della_richiesta(&req, false, &reasoning).await)
                .expect("serializza");
        assert!(corpo.get("service_tier").is_none());
    }

    /// L'interruttore acceso, come lo accenderebbe un operatore: la mig 0729 lo
    /// semina `'false'` e questo e' l'unico modo di provare il ramo acceso senza
    /// fissare qui il valore che la migrazione dichiara (regola O).
    async fn accendi_la_corsia(pool: &PgPool) {
        sqlx::query("UPDATE settings SET value = 'true' WHERE key = $1")
            .bind(FLEX_ENABLED_SETTING)
            .execute(pool)
            .await
            .expect("l'interruttore esiste: lo semina la mig 0729");
    }

    fn provider_con_catalogo(pool: PgPool) -> OpenAiProvider {
        OpenAiProvider::with_db(Client::new(), "sk-test", None, Some(pool))
    }

    async fn corpo_di(p: &OpenAiProvider, req: &LlmRequest) -> serde_json::Value {
        let reasoning = p.resolve(req).await;
        let differibile = if p.corsia(req).await == SceltaCorsia::Differibile {
            con_corsia_differibile(req)
        } else {
            req.clone()
        };
        serde_json::to_value(
            p.client
                .corpo_della_richiesta(&differibile, false, &reasoning)
                .await,
        )
        .expect("serializza")
    }

    /// IL test del lotto: la platea arriva dal CATALOGO, per modello, e la
    /// conseguenza si guarda sul corpo che parte davvero.
    ///
    /// I tre verdetti vengono dalla mig 0729, non da un inserimento di comodo:
    /// sono cio' che l'API ha risposto il 17/08/2026 (`o4-mini` accetta il
    /// parametro, `gpt-4o-mini` risponde 400 «Invalid service_tier argument»),
    /// e se qualcuno cambia quella migrazione questo test se ne accorge.
    ///
    /// MUTAZIONE: far ritornare `Si` a `FlexAmmesso::dal_catalogo(None)`, o
    /// togliere il gate del catalogo da `corsia` -> il modello ignoto parte in
    /// flex, rosso.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_corsia_differibile_la_dichiara_il_catalogo(pool: PgPool) {
        let p = provider_con_catalogo(pool.clone());
        accendi_la_corsia(&pool).await;

        // MISURATO: 429 sul credito, cioe' il parametro ha superato la
        // validazione del modello.
        let mut req = richiesta("o4-mini");
        req.deferrable = true;
        assert_eq!(p.flex_ammesso("o4-mini").await, FlexAmmesso::Si);
        assert_eq!(p.corsia(&req).await, SceltaCorsia::Differibile);
        assert_eq!(
            corpo_di(&p, &req).await["service_tier"],
            "flex",
            "senza questo campo la richiesta costa il doppio"
        );

        // MISURATO: 400 «Invalid service_tier argument». Mandarlo qui sarebbe un
        // errore su OGNI chiamata a quel modello.
        let mut req = richiesta("gpt-4o-mini");
        req.deferrable = true;
        assert_eq!(p.flex_ammesso("gpt-4o-mini").await, FlexAmmesso::No);
        assert_eq!(p.corsia(&req).await, SceltaCorsia::Storica);
        assert!(corpo_di(&p, &req).await.get("service_tier").is_none());

        // Un modello che il catalogo non conosce non autorizza niente.
        let mut req = richiesta("gpt-9-mai-vista");
        req.deferrable = true;
        assert_eq!(
            p.flex_ammesso("gpt-9-mai-vista").await,
            FlexAmmesso::NonDichiarato
        );
        assert!(corpo_di(&p, &req).await.get("service_tier").is_none());
    }

    /// Il permesso del catalogo non basta: senza la dichiarazione del CHIAMANTE
    /// e senza l'interruttore dell'installazione non parte niente. Prova che le
    /// tre condizioni sono davvero in congiunzione anche attraverso il driver
    /// reale, non solo nella funzione pura.
    ///
    /// MUTAZIONE: togliere `self.flex_abilitato().await` da `corsia` -> il primo
    /// blocco (interruttore al valore di seed) diventa `Differibile`, rosso.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_permesso_del_catalogo_da_solo_non_accende_niente(pool: PgPool) {
        let p = provider_con_catalogo(pool.clone());

        // Interruttore al valore di SEED ('false'): il meccanismo e' inerte al
        // deploy, ed e' il punto del lotto.
        let mut req = richiesta("o4-mini");
        req.deferrable = true;
        assert!(!p.flex_abilitato().await, "la mig 0729 lo semina spento");
        assert_eq!(p.corsia(&req).await, SceltaCorsia::Storica);
        assert!(corpo_di(&p, &req).await.get("service_tier").is_none());

        // Acceso, ma il chiamante non ha dichiarato niente: la corsia non e'
        // una proprieta' del fornitore, e' una dichiarazione di chi chiede.
        let p = provider_con_catalogo(pool.clone());
        accendi_la_corsia(&pool).await;
        let req = richiesta("o4-mini");
        assert!(!req.deferrable, "il default del contratto");
        assert_eq!(p.corsia(&req).await, SceltaCorsia::Storica);
        assert!(corpo_di(&p, &req).await.get("service_tier").is_none());
    }

    /// Il tier PINNATO dal chiamante vince e non viene toccato, nemmeno quando
    /// tutte e tre le condizioni direbbero di si'. Chi pinna ha deciso: il
    /// driver non lo scavalca e non ripiega per lui (lo raccoglie la guardia
    /// sulla causa in `complete_with_retry`).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_tier_pinnato_dal_chiamante_non_si_tocca(pool: PgPool) {
        let p = provider_con_catalogo(pool.clone());
        accendi_la_corsia(&pool).await;
        let mut req = richiesta("o4-mini");
        req.deferrable = true;
        req.service_tier = Some("priority".to_string());
        assert_eq!(p.corsia(&req).await, SceltaCorsia::Storica);
        assert_eq!(corpo_di(&p, &req).await["service_tier"], "priority");
    }

    /// Server finto che risponde come openai il 17/08/2026: 429 con
    /// `flex_unavailable` alla richiesta CON `service_tier`, 200 a quella senza.
    /// Registra i corpi ricevuti, che sono la sola prova di cosa sia partito —
    /// asserire sull'esito proverebbe che il provider ritorna Ok, non che abbia
    /// ripiegato.
    async fn finge_corsia_piena() -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("porta effimera");
        let porta = listener.local_addr().expect("indirizzo").port();
        let corpi = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let registro = corpi.clone();

        tokio::spawn(async move {
            // Terminatore di riga del PROTOCOLLO: costante, cosi' un
            // normalizzatore di fine-riga sull'albero non lo puo' toccare.
            const CRLF: &str = "\r\n";
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut grezzo = Vec::new();
                let mut buf = [0u8; 4096];
                // Header + corpo: la lunghezza la dichiara `Content-Length`.
                let atteso = loop {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break 0usize,
                        Ok(n) => grezzo.extend_from_slice(&buf[..n]),
                    }
                    let testo = String::from_utf8_lossy(&grezzo);
                    let Some(fine_testa) = testo.find("\r\n\r\n") else {
                        continue;
                    };
                    let len: usize = testo
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    if grezzo.len() >= fine_testa + 4 + len {
                        break fine_testa + 4;
                    }
                };
                let intero = String::from_utf8_lossy(&grezzo).to_string();
                let corpo_ricevuto = intero.get(atteso..).unwrap_or_default().to_string();
                registro
                    .lock()
                    .expect("registro")
                    .push(corpo_ricevuto.clone());

                let (stato, corpo) = if corpo_ricevuto.contains("\"service_tier\"") {
                    (
                        "429 Too Many Requests",
                        r#"{"error":{"message":"Flex tier does not have sufficient resources available to fulfill your request.","type":"resource_unavailable","code":"flex_unavailable"}}"#,
                    )
                } else {
                    (
                        "200 OK",
                        r#"{"id":"x","model":"o4-mini","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#,
                    )
                };
                let testa = [
                    &format!("HTTP/1.1 {stato}"),
                    "Content-Type: application/json",
                    // Come il fornitore vero: e' proprio questo che il ramo
                    // Transient onorerebbe, mettendo la coppia in cooldown.
                    "Retry-After: 300",
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

        (porta, corpi)
    }

    /// Il RIPIEGO in-driver: la corsia piena non deve costare un anello di
    /// chain ne' un cooldown, perche' il fornitore e' sano e serve subito al
    /// tier standard. Due richieste allo stesso modello, la seconda senza il
    /// campo, e l'esito e' un Ok.
    ///
    /// MUTAZIONE: togliere il ramo `Err(e) if e_capacita_flex(&e)` da
    /// `complete` -> l'errore si propaga e il server vede UNA sola richiesta,
    /// rosso su entrambe le asserzioni.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_corsia_piena_ripiega_al_tier_standard(pool: PgPool) {
        let (porta, corpi) = finge_corsia_piena().await;
        let p = OpenAiProvider::with_db(
            Client::new(),
            "sk-test",
            Some(format!("http://127.0.0.1:{porta}")),
            Some(pool.clone()),
        );
        accendi_la_corsia(&pool).await;

        let mut req = richiesta("o4-mini");
        req.deferrable = true;
        let esito = p.complete(&req).await;
        assert!(
            esito.is_ok(),
            "il ripiego deve produrre una risposta: {:?}",
            esito.err()
        );

        let visti = corpi.lock().expect("registro").clone();
        assert_eq!(visti.len(), 2, "una in flex, una al tier standard");
        assert!(
            visti[0].contains("\"service_tier\":\"flex\""),
            "la prima chiede la corsia scontata: {}",
            visti[0]
        );
        assert!(
            !visti[1].contains("\"service_tier\""),
            "la seconda NON deve richiederla, o riprenderebbe lo stesso rifiuto: {}",
            visti[1]
        );
    }

    /// UNA volta sola: se anche il tier standard rifiuta, l'errore e' suo e deve
    /// arrivare intero a chi sa fare failover. Qui il server risponde 429 flex a
    /// TUTTO, quindi il ripiego incontra lo stesso rifiuto.
    ///
    /// MUTAZIONE: trasformare il ripiego in un ciclo -> il test non termina o il
    /// conteggio delle richieste esplode.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_ripiego_non_si_ripete(pool: PgPool) {
        let (porta, corpi) = finge_sempre_429().await;
        let p = OpenAiProvider::with_db(
            Client::new(),
            "sk-test",
            Some(format!("http://127.0.0.1:{porta}")),
            Some(pool.clone()),
        );
        accendi_la_corsia(&pool).await;

        let mut req = richiesta("o4-mini");
        req.deferrable = true;
        assert!(p.complete(&req).await.is_err(), "l'errore deve salire");
        assert_eq!(corpi.lock().expect("registro").len(), 2, "due, non tre");
    }

    /// Come [`finge_corsia_piena`], ma rifiuta anche la richiesta senza tier:
    /// serve a provare che il ripiego e' UNO.
    async fn finge_sempre_429() -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("porta effimera");
        let porta = listener.local_addr().expect("indirizzo").port();
        let corpi = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let registro = corpi.clone();

        tokio::spawn(async move {
            const CRLF: &str = "\r\n";
            const CORPO: &str = r#"{"error":{"message":"Flex tier does not have sufficient resources available.","type":"resource_unavailable","code":"flex_unavailable"}}"#;
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut grezzo = Vec::new();
                let mut buf = [0u8; 4096];
                while !grezzo.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => grezzo.extend_from_slice(&buf[..n]),
                    }
                }
                registro
                    .lock()
                    .expect("registro")
                    .push(String::from_utf8_lossy(&grezzo).to_string());
                let testa = [
                    "HTTP/1.1 429 Too Many Requests",
                    "Content-Type: application/json",
                    &format!("Content-Length: {}", CORPO.len()),
                    "Connection: close",
                    "",
                    "",
                ]
                .join(CRLF);
                let _ = socket.write_all(testa.as_bytes()).await;
                let _ = socket.write_all(CORPO.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });

        (porta, corpi)
    }

    #[test]
    fn detection_o_series_per_famiglia() {
        // Reasoning family per prefisso (parita' col Python).
        assert!(is_o_series("o1"));
        assert!(is_o_series("o1-mini"));
        assert!(is_o_series("o3"));
        assert!(is_o_series("o4-mini"));
        assert!(is_o_series("gpt-5"));
        assert!(is_o_series("gpt-5.1"));
        assert!(is_o_series("gpt-5-nano"));
        assert!(is_o_series("gpt-4.5-preview"));
        assert!(is_o_series("GPT-5")); // case-insensitive
                                       // Chat non-reasoning: dialetto base.
        assert!(!is_o_series("gpt-4o"));
        assert!(!is_o_series("gpt-4o-mini"));
        assert!(!is_o_series("gpt-4.1"));
    }
}
