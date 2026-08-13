//! Client OpenAI-compatibile CONDIVISO.
//!
//! Punto unico (regola L) per tutti i provider che parlano il dialetto OpenAI
//! Chat Completions: OpenAI, Mistral, DeepSeek, vLLM. I provider concreti non
//! ereditano nulla, ma COMPONGONO un'istanza di [`OpenAiCompatClient`]
//! parametrizzata con `base_url`, `api_key` e capacita' proprie.
//!
//! Porting di `packages/llm-gateway/src/providers/openai.ts`:
//! - costruzione richiesta `POST {base_url}/chat/completions`
//! - mapping `ChatCompletion` JSON -> [`LlmResponse`]
//! - streaming SSE (`response.bytes_stream()` + parser righe `data: {json}`)
//!
//! Regola G: nessun modello hardcoded, arriva sempre da `req.model`.
//! Regola F: mai loggare prompt/response in chiaro.

use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use dashmap::DashMap;
use futures::StreamExt;
use nexus_cache::TtlCache;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio_stream::wrappers::ReceiverStream;

use crate::provider::ChunkStream;
use crate::tassonomia_errori::CandidatiErrore;
use crate::types::{
    GeneratedImage, ImageGenResponse, LlmRequest, LlmResponse, LlmStreamChunk, LlmToolCall,
    LlmUsage, MessageContent, PromptCacheKeying, PromptCacheReporting, ReasoningTokens,
    ToolCallDelta,
    ToolCallDeltaFunction, ToolFunctionCall, TranscribeResponse,
};

/// Dialetto di reasoning di un endpoint OpenAI-compatibile. Centralizza (regola
/// L) le differenze tra i provider che parlano il dialetto OpenAI ma gestiscono
/// il reasoning in modi diversi. La detection per-modello (es. o-series OpenAI)
/// resta a carico del provider, che sceglie il dialetto a runtime via
/// [`OpenAiCompatClient::with_reasoning`] / [`resolve_reasoning`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningDialect {
    /// Nessuna gestione speciale: parametri base, niente reasoning (Mistral, e
    /// OpenAI per i modelli chat non-reasoning). I `reasoning_content` even-
    /// tualmente presenti nella response sono comunque letti (best-effort).
    None,
    /// DeepSeek: thinking governato da `extra_body.thinking.type`
    /// (enabled/disabled); il reasoning torna nel campo `reasoning_content`
    /// (response e stream delta).
    DeepSeek,
    /// OpenAI o-series / gpt-5 / gpt-4.5: usa `max_completion_tokens` al posto di
    /// `max_tokens` e accetta `reasoning_effort`; non espone il reasoning come
    /// testo, solo i `reasoning_tokens` in `completion_tokens_details`.
    OpenAiReasoning,
    /// Moonshot/Kimi: il pensiero e' SEMPRE acceso e non si spegne, quindi il
    /// dialetto non e' una preferenza da esprimere ma un contratto da rispettare.
    /// Tre differenze dal dialetto base, tutte documentate:
    ///
    /// - `temperature` e' FISSA sui modelli moderni e "passing any other value
    ///   returns an error" (doc `/docs/api/models-overview`): non si invia. Non e'
    ///   una precauzione — tre call site interni mandano `Some(0.0)`
    ///   (`summary_store`, `next_actions_deriver`, `wizard`) e prenderebbero 400
    ///   su ogni chiamata;
    /// - `max_tokens` e' deprecato in favore di `max_completion_tokens`
    ///   (doc `/docs/api/chat`);
    /// - Preserved Thinking: per `kimi-k3` e `kimi-k2.7-code` la doc prescrive
    ///   di rimandare indietro l'assistant "completo e inalterato,
    ///   `reasoning_content` compreso" (`/docs/guide/use-thinking-models`).
    ///   MISURATO il 09/08/2026 su `kimi-k2.6` e `kimi-k2.7-code`: l'API NON
    ///   rifiuta il turno che lo omette — il vincolo e' meno duro di quello
    ///   DeepSeek, che risponde 400. Il round-trip resta perche' e' il punto del
    ///   meccanismo: quel campo E' il ragionamento del turno precedente, e
    ///   toglierlo lo fa ricominciare da capo su un modello che pensa sempre.
    ///   Non e' una difesa da un errore, e' la continuita' del pensiero.
    ///
    /// Non emette `thinking` ne' `reasoning_effort`: i default del fornitore sono
    /// dichiarati (k3 `max`, k2.x `enabled`) e nessun chiamante esprime oggi una
    /// preferenza. Un ramo senza produttore sarebbe codice morto (regola O), e
    /// per giunta rischioso: su `kimi-k2.7-code` un `thinking` esplicito diverso
    /// da `{"type":"enabled","keep":"all"}` e' un errore.
    Kimi,
}

/// Configurazione di reasoning risolta per una richiesta. `dialect` indica come
/// parlare col provider; `enabled` se il thinking va attivato; `effort` il
/// livello per i modelli o-series (low/medium/high).
#[derive(Debug, Clone)]
pub struct ResolvedReasoning {
    pub dialect: ReasoningDialect,
    pub enabled: bool,
    pub effort: Option<String>,
}

impl ResolvedReasoning {
    /// Nessun reasoning, dialetto base: il default per i provider che non lo
    /// gestiscono (Mistral) e per le richieste senza `thinking`.
    pub fn none() -> Self {
        Self {
            dialect: ReasoningDialect::None,
            enabled: false,
            effort: None,
        }
    }
}

/// Client HTTP riusabile verso un endpoint OpenAI-compatibile.
///
/// Composto (non ereditato) dai provider concreti. Il `provider_name` viene
/// scritto in `LlmResponse.provider_used` cosi' ogni wrapper riporta la propria
/// identita' senza dover rimappare la risposta.
#[derive(Clone)]
pub struct OpenAiCompatClient {
    http: Client,
    base_url: String,
    api_key: String,
    provider_name: String,
    cache_keying: PromptCacheKeying,
    /// Serve solo agli endpoint [`PromptCacheKeying::requires_upstream_pinning`],
    /// per leggere quale fornitore a valle preferire. Opzionale: i test che
    /// esercitano la sola mappatura request/response non hanno DB, e senza si
    /// perde un riuso di prefisso, non la correttezza.
    db: Option<PgPool>,
    /// Preferenza per modello, con la TTL delle altre cache del gateway (60s,
    /// punto unico `TtlCache`). `None` in valore = misurato assente, e va
    /// ricordato: senza, ogni chiamata su un modello senza riga interrogherebbe
    /// il DB da capo. Vi entra SOLO cio' che si e' letto: un errore non e' una
    /// misura, e scriverlo qui cristallizzerebbe per 60s un'ignoranza
    /// momentanea.
    upstream_order: TtlCache<String, Option<Vec<String>>>,
    /// Ultimo esito LETTO con successo, senza scadenza: e' cio' che si serve
    /// mentre il DB non risponde, invece di degradare a "nessuna preferenza"
    /// (stesso pattern di `RoutingMatrixCache`, regola G — la cache tiene
    /// l'ultimo valore valido e il refresh fallito resta un WARN).
    ///
    /// Non maschera una riconfigurazione: viene sovrascritto a ogni lettura
    /// riuscita, quindi una riga rimossa o disattivata vi finisce come `None`.
    /// Copre il solo caso in cui il DB non ha parlato.
    ultimo_ordine_letto: Arc<DashMap<String, Option<Vec<String>>>>,
}

/// TTL della cache delle preferenze di fornitore (come `policy_engine`/`cooldown`).
const UPSTREAM_AFFINITY_TTL: Duration = Duration::from_secs(60);

/// SQLSTATE `undefined_table`: la tabella dell'affinita' non esiste su questo DB,
/// cioe' la mig 0657 non e' applicata. E' il caso peggiore (funzionalita' inerte
/// al 100%) e va detto per nome, non confuso con "nessuna preferenza".
const SQLSTATE_TABELLA_ASSENTE: &str = "42P01";

/// Perche' la preferenza non si e' potuta leggere, dal segnale STRUTTURATO
/// dell'errore e non dal suo messaggio (regola M): lo SQLSTATE del database, o
/// l'assenza di codice quando l'errore e' del trasporto (pool esaurito,
/// connessione caduta) e non del server.
fn causa_preferenza_illeggibile(sqlstate: Option<&str>) -> &'static str {
    match sqlstate {
        Some(SQLSTATE_TABELLA_ASSENTE) => {
            "tabella assente: la migrazione 0657 non e' applicata su questo DB"
        }
        Some(_) => "il database ha rifiutato la query",
        None => "il database non e' raggiungibile",
    }
}

/// SQLSTATE dell'errore, se l'errore viene dal server e non dal trasporto.
/// Estratto qui perche' il campo sia leggibile una volta e prestato al log senza
/// tenere in vita un temporaneo.
fn sqlstate_di(e: &sqlx::Error) -> Option<String> {
    e.as_database_error()
        .and_then(|d| d.code())
        .map(|c| c.into_owned())
}

/// Legge il CSV di `nexus_router_upstream_affinity.upstream_order`.
///
/// Funzione PURA, separata dalla lettura DB: il CSV lo scrive un umano in una
/// migrazione, quindi lo spazio dopo la virgola e' la norma e non deve entrare
/// nel nome del fornitore — un ordine con `" DeepInfra"` non lo soddisfa
/// nessuno, e l'instradatore lo tratterebbe come una preferenza impossibile.
/// Un CSV vuoto o di soli separatori vale "nessuna preferenza": e' diverso da
/// una preferenza vuota, che sul wire sarebbe un vincolo insoddisfacibile.
fn parse_upstream_order(csv: &str) -> Option<Vec<String>> {
    let v: Vec<String> = csv
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    (!v.is_empty()).then_some(v)
}

impl OpenAiCompatClient {
    /// Costruisce il client. `base_url` senza slash finale (es.
    /// `https://api.mistral.ai/v1`); l'endpoint `/chat/completions` viene
    /// aggiunto internamente.
    pub fn new(
        http: Client,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        provider_name: impl Into<String>,
    ) -> Self {
        let base_url = base_url.into();
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            http,
            base_url,
            api_key: api_key.into(),
            provider_name: provider_name.into(),
            // Il default e' il provider che si arrangia: dichiarare una chiave a
            // un endpoint che non la conosce e' il solo verso che puo' fare
            // danno (campo sconosciuto -> HTTP 400), mentre ometterla costa al
            // massimo un riuso mancato.
            cache_keying: PromptCacheKeying::ProviderManaged,
            db: None,
            upstream_order: TtlCache::new(UPSTREAM_AFFINITY_TTL),
            ultimo_ordine_letto: Arc::new(DashMap::new()),
        }
    }

    /// Dichiara che questo endpoint riusa il prefisso solo con
    /// `prompt_cache_key` in richiesta (vedi [`PromptCacheKeying`]).
    pub fn with_prompt_cache_keying(mut self, keying: PromptCacheKeying) -> Self {
        self.cache_keying = keying;
        self
    }

    /// Aggancia il DB da cui leggere la preferenza di fornitore a valle
    /// (`nexus_router_upstream_affinity`, mig 0657). Serve ai soli instradatori:
    /// altrove la preferenza non viene nemmeno interrogata.
    pub fn with_db(mut self, db: Option<PgPool>) -> Self {
        self.db = db;
        self
    }

    /// Fornitori a valle preferiti per questo modello, in ordine.
    ///
    /// Interroga solo se l'endpoint e' un instradatore: su un provider diretto la
    /// domanda non ha senso e la riga non esisterebbe. Nessun DB agganciato vale
    /// "nessuna preferenza": si perde il riuso del prefisso, non la chiamata —
    /// l'opposto di quello che farebbe rifiutare la richiesta.
    ///
    /// Una query FALLITA e' un'altra cosa, e prima finiva nella stessa casella:
    /// `unwrap_or(None)` appiattiva l'errore in "riga assente" e lo scriveva in
    /// cache, cioe' cristallizzava per 60s una risposta che nessuno aveva dato
    /// (regola M). Nessun log lo diceva, e il costo non era simmetrico: su
    /// `minimax/minimax-m2` la riga esiste per ESCLUDERE un fornitore che sullo
    /// stesso prefisso fattura il prompt il doppio (20.011 token contro 10.162
    /// misurati il 29/07/2026), quindi perderla non costa un riuso mancato ma una
    /// sovrafatturazione. Qui l'errore: (1) si dichiara, con la sua causa
    /// strutturata; (2) NON entra in cache, cosi' la chiamata dopo ritenta;
    /// (3) non cancella cio' che si era gia' letto.
    async fn upstream_order_for(&self, model: &str) -> Option<Vec<String>> {
        if !self.cache_keying.requires_upstream_pinning() {
            return None;
        }
        if let Some(v) = self.upstream_order.get(model) {
            return v;
        }
        let db = self.db.as_ref()?;
        let letto: Result<Option<(String,)>, sqlx::Error> = sqlx::query_as(
            "SELECT upstream_order FROM nexus_router_upstream_affinity \
             WHERE provider = $1 AND model_id = $2 AND is_active",
        )
        .bind(&self.provider_name)
        .bind(model)
        .fetch_optional(db)
        .await;
        match letto {
            Ok(row) => {
                let ordine = row.and_then(|(csv,)| parse_upstream_order(&csv));
                self.upstream_order.insert(model.to_string(), ordine.clone());
                self.ultimo_ordine_letto
                    .insert(model.to_string(), ordine.clone());
                ordine
            }
            Err(e) => {
                let noto = self
                    .ultimo_ordine_letto
                    .get(model)
                    .and_then(|v| v.value().clone());
                let sqlstate = sqlstate_di(&e);
                // Regola F: niente prompt/payload nei campi; qui viaggiano solo
                // identificatori di configurazione e la causa strutturata.
                tracing::warn!(
                    provider = %self.provider_name,
                    model = %model,
                    causa = causa_preferenza_illeggibile(sqlstate.as_deref()),
                    sqlstate = sqlstate.as_deref(),
                    servito_ultimo_noto = noto.is_some(),
                    "affinita' di fornitore a valle non leggibile: il prefisso puo' \
                     atterrare su un fornitore diverso"
                );
                noto
            }
        }
    }

    /// Come questo client dichiara il riuso del prefisso. Esposto perche' il
    /// provider che lo compone possa provare di averlo configurato: il difetto
    /// da cui nasce era proprio una richiesta che partiva senza chiave.
    pub fn cache_keying(&self) -> PromptCacheKeying {
        self.cache_keying
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    /// PUNTO UNICO (regola L) del corpo che parte da QUESTO client: risolve la
    /// preferenza di fornitore a valle, costruisce il body col dialetto di cache
    /// che il client DICHIARA, e applica i quirk di forma dell'endpoint.
    ///
    /// Perche' esiste: la sequenza era ricopiata in `complete_with_reasoning` e
    /// `stream_with_reasoning`, e la duplicazione era la ragione per cui nessun
    /// test la attraversava — i test del body chiamavano
    /// [`build_request_body`] a mano, passando `cache_keying` e ordine come
    /// argomenti, cioe' fissando l'assunto che volevano verificare (regola O).
    /// MISURATO il 29/07/2026: sostituendo i due call site con
    /// `PromptCacheKeying::ProviderManaged, None` — cioe' revocando in blocco i
    /// tre livelli di affinita' del prefisso — `cargo test -p nexus-gateway`
    /// dava 407 passati e 0 falliti, identico alla baseline.
    ///
    /// Con un punto solo, i test che lo attraversano coprono ENTRAMBI i
    /// percorsi, e la stessa mutazione ora fa rosseggiare la suite.
    pub(crate) async fn corpo_della_richiesta(
        &self,
        req: &LlmRequest,
        stream: bool,
        reasoning: &ResolvedReasoning,
    ) -> ChatCompletionRequest {
        let ordine = self.upstream_order_for(&req.model).await;
        let mut body =
            build_request_body(req, stream, reasoning, self.cache_keying, ordine.as_deref());
        if provider_requires_user_or_tool_last(&self.provider_name) {
            strip_trailing_assistant(&mut body.messages);
        }
        body
    }

    /// Esegue una completion non-streaming e mappa il risultato in
    /// [`LlmResponse`]. Dialetto base, nessun reasoning (Mistral, vLLM, OpenAI
    /// chat non-reasoning): delega a [`Self::complete_with_reasoning`].
    pub async fn complete(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
        self.complete_with_reasoning(req, &ResolvedReasoning::none())
            .await
    }

    /// Variante con reasoning esplicito: i provider che lo gestiscono
    /// (DeepSeek, OpenAI o-series) passano il [`ResolvedReasoning`] risolto.
    pub async fn complete_with_reasoning(
        &self,
        req: &LlmRequest,
        reasoning: &ResolvedReasoning,
    ) -> anyhow::Result<LlmResponse> {
        let body = self.corpo_della_richiesta(req, false, reasoning).await;
        let start = Instant::now();

        let resp = self
            .http
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            // Regola F: il body d'errore puo' contenere dettagli del provider
            // ma non prompt/response utente; lo propaghiamo al caller (la Fase 3
            // distingue il billing error), senza loggarlo qui in chiaro.
            return Err(provider_http_error(&self.provider_name, resp).await.into());
        }

        // Body come testo + parse esplicito: il generico `resp.json()` di
        // reqwest appiattiva OGNI mismatch in "error decoding response body"
        // (incidente mistral 2026-07-06: 18 errori in history senza causa
        // diagnosticabile). Il parse separato distingue "body troncato dalla
        // rete" (fallisce `text()`, transitorio vero) da "schema inatteso"
        // (fallisce serde con campo/posizione precisi, senza payload nel
        // messaggio: regola F).
        let body = resp.text().await?;
        let parsed = parse_chat_completion(&self.provider_name, &body)?;
        let latency_ms = start.elapsed().as_millis() as u64;
        from_chat_completion(parsed, req.model.clone(), &self.provider_name, latency_ms)
    }

    /// Esegue una completion in streaming. Legge `bytes_stream()`, accumula i
    /// byte e parsa le righe SSE `data: {json}` fino a `[DONE]`, emettendo un
    /// [`LlmStreamChunk`] per ogni delta.
    ///
    /// Implementazione: un task `tokio::spawn` consuma il `bytes_stream()` (dove
    /// il tipo concreto e' inferito, cosi' non serve nominare `bytes::Bytes` nei
    /// campi) e spinge i chunk parsati in un canale; lo stream restituito legge
    /// dal canale. Cosi' lo `ChunkStream` e' `'static + Send` come da contratto.
    pub async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        self.stream_with_reasoning(req, &ResolvedReasoning::none())
            .await
    }

    /// Variante streaming con reasoning esplicito (vedi
    /// [`Self::complete_with_reasoning`]).
    pub async fn stream_with_reasoning(
        &self,
        req: &LlmRequest,
        reasoning: &ResolvedReasoning,
    ) -> anyhow::Result<ChunkStream> {
        let body = self.corpo_della_richiesta(req, true, reasoning).await;

        let resp = self
            .http
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            return Err(provider_http_error(&self.provider_name, resp).await.into());
        }

        let provider_name = self.provider_name.clone();
        let model_used = req.model.clone();

        let (tx, rx) = tokio::sync::mpsc::channel::<anyhow::Result<LlmStreamChunk>>(32);

        tokio::spawn(async move {
            let mut bytes = resp.bytes_stream();
            let mut parser = SseParser {
                line_buf: String::new(),
                pending: std::collections::VecDeque::new(),
                done: false,
                provider_name,
                model_used,
            };

            loop {
                match bytes.next().await {
                    Some(Ok(buf)) => {
                        parser.line_buf.push_str(&String::from_utf8_lossy(&buf));
                        parser.drain_lines();
                    }
                    Some(Err(e)) => {
                        let _ = tx.send(Err(anyhow::Error::new(e))).await;
                        return;
                    }
                    None => {
                        // Fine stream: processa l'eventuale residuo nel buffer.
                        let leftover = std::mem::take(&mut parser.line_buf);
                        for line in leftover.lines() {
                            parser.parse_line(line);
                        }
                        while let Some(chunk) = parser.pending.pop_front() {
                            if tx.send(Ok(chunk)).await.is_err() {
                                return;
                            }
                        }
                        return;
                    }
                }

                // Inoltra i chunk pronti; se il consumer ha chiuso, termina.
                while let Some(chunk) = parser.pending.pop_front() {
                    if tx.send(Ok(chunk)).await.is_err() {
                        return;
                    }
                }
                if parser.done {
                    return;
                }
            }
        });

        let out = ReceiverStream::new(rx);
        Ok(out.boxed())
    }

    /// Probe di salute: una HEAD/GET su `{base_url}/models`. Ritorna `false` su
    /// qualunque errore (rete, auth, status non 2xx).
    pub async fn healthcheck(&self) -> bool {
        let url = format!("{}/models", self.base_url);
        match self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }

    /// Autodiscovery live: `GET {base_url}/models` (Bearer) ed estrae `data[].id`.
    /// Dialetto OpenAI condiviso da OpenAI/Mistral/DeepSeek/vLLM (punto unico,
    /// regola L). Il parsing della risposta e' delegato a [`parse_models_response`]
    /// (puro, testabile senza rete).
    pub async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        Ok(self
            .list_models_meta()
            .await?
            .into_iter()
            .map(|m| m.id)
            .collect())
    }

    /// Autodiscovery live CON METADATI: id + finestra di contesto dichiarata
    /// dal provider quando il dialetto la espone (Mistral: `max_context_length`
    /// in `data[]`; OpenAI/DeepSeek non la espongono -> `None`). Un solo fetch
    /// (regola L): [`Self::list_models`] delega qui e proietta i soli id.
    pub async fn list_models_meta(&self) -> anyhow::Result<Vec<crate::provider::ModelMeta>> {
        let url = format!("{}/models", self.base_url);
        let resp = self.http.get(url).bearer_auth(&self.api_key).send().await?;
        let status = resp.status();
        if !status.is_success() {
            // Errore strutturato anche sulla lista modelli (regola M): status +
            // codice, mai testo da classificare. Il caller aggrega best-effort.
            return Err(provider_http_error(&self.provider_name, resp).await.into());
        }
        let body: serde_json::Value = resp.json().await?;
        Ok(parse_models_meta_response(&body))
    }

    /// Genera immagini via `POST {base_url}/images/generations` (dialetto OpenAI
    /// Images). Punto unico del trasporto image-gen OpenAI-compatibile (regola L):
    /// stesso `http` client e `bearer_auth(api_key)` di [`Self::complete`], stesso
    /// status-check propagato al caller (che ne chiede il verdetto al catalogo
    /// dei codici, `tassonomia_errori`, e applica il cooldown).
    ///
    /// Richiesta: `{model, prompt, n?, size?, response_format:"b64_json"}`.
    /// Risposta: `{data:[{b64_json|url}], ...}` -> [`GeneratedImage`]. Regola G:
    /// `model` arriva dal chiamante. Regola F: il body d'errore (che non contiene
    /// prompt utente) e' propagato al caller, non loggato qui in chiaro.
    pub async fn images_generations(
        &self,
        model: &str,
        prompt: &str,
        n: Option<u32>,
        size: Option<&str>,
    ) -> anyhow::Result<ImageGenResponse> {
        let body = ImageGenWireRequest {
            model: model.to_string(),
            prompt: prompt.to_string(),
            n,
            size: size.map(|s| s.to_string()),
            // base64 inline: il gateway non dipende da URL temporanee del provider.
            response_format: "b64_json".to_string(),
        };
        let start = Instant::now();

        let resp = self
            .http
            .post(format!("{}/images/generations", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            return Err(provider_http_error(&self.provider_name, resp).await.into());
        }

        let parsed: ImagesResponse = resp.json().await?;
        let latency_ms = start.elapsed().as_millis() as u64;
        Ok(from_images_response(
            parsed,
            model.to_string(),
            &self.provider_name,
            latency_ms,
        ))
    }

    /// Trascrive audio via `POST {base_url}/audio/transcriptions` (dialetto OpenAI
    /// Audio, MULTIPART/form-data). Punto unico del trasporto audio-in OpenAI-
    /// compatibile (regola L): stesso `http` client e `bearer_auth(api_key)` di
    /// [`Self::complete`], stesso status-check propagato al caller (che ne chiede
    /// il verdetto al catalogo dei codici, `tassonomia_errori`, e applica il cooldown).
    ///
    /// Form: `file=<bytes>` (con `file_name` + mime), `model`, `response_format=json`,
    /// `language` se presente. Risposta: `{"text":"..."}` -> [`TranscribeResponse`].
    /// Regola G: `model` arriva dal chiamante. Regola F: il body d'errore (che non
    /// contiene il payload audio) e' propagato al caller, non loggato qui.
    pub async fn transcribe(
        &self,
        model: &str,
        audio_bytes: Vec<u8>,
        filename: &str,
        language: Option<&str>,
    ) -> anyhow::Result<TranscribeResponse> {
        let mut part = reqwest::multipart::Part::bytes(audio_bytes).file_name(filename.to_string());
        // MIME inferito dall'estensione del filename (gia' risolta dal chiamante in
        // base al mime dichiarato). Se non riconosciuto, lasciamo che reqwest usi
        // application/octet-stream: OpenAI inferisce comunque dal file_name.
        if let Some(mime) = mime_from_filename(filename) {
            part = part.mime_str(mime)?;
        }
        let mut form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", model.to_string())
            .text("response_format", "json");
        if let Some(lang) = language.filter(|l| !l.trim().is_empty()) {
            form = form.text("language", lang.trim().to_string());
        }

        let start = Instant::now();
        let resp = self
            .http
            .post(format!("{}/audio/transcriptions", self.base_url))
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            return Err(provider_http_error(&self.provider_name, resp).await.into());
        }

        let parsed: TranscriptionResponse = resp.json().await?;
        let latency_ms = start.elapsed().as_millis() as u64;
        Ok(TranscribeResponse {
            text: parsed.text,
            model_used: model.to_string(),
            provider_used: self.provider_name.clone(),
            latency_ms,
        })
    }

    /// Sintetizza audio via `POST {base_url}/audio/speech` (dialetto OpenAI Audio,
    /// JSON in -> BYTES binari out). Punto unico del trasporto audio-out OpenAI-
    /// compatibile (regola L): stesso `http` client e `bearer_auth(api_key)` di
    /// [`Self::complete`], stesso status-check propagato al caller (che ne chiede
    /// il verdetto al catalogo dei codici, `tassonomia_errori`, e applica il cooldown).
    ///
    /// Body JSON: `model`, `input`, `voice` (se presente), `response_format`.
    /// Risposta: BYTES audio (NON JSON) + il Content-Type per il MIME reale.
    /// Regola G: `model` arriva dal chiamante. Regola F: il body d'errore (che non
    /// contiene il testo sintetizzato) e' propagato al caller, non loggato qui.
    pub async fn speech(
        &self,
        model: &str,
        input: &str,
        voice: Option<&str>,
        response_format: Option<&str>,
    ) -> anyhow::Result<(Vec<u8>, String)> {
        let mut body = serde_json::json!({
            "model": model,
            "input": input,
        });
        if let Some(v) = voice.filter(|v| !v.trim().is_empty()) {
            body["voice"] = serde_json::Value::String(v.trim().to_string());
        }
        if let Some(fmt) = response_format.filter(|f| !f.trim().is_empty()) {
            body["response_format"] = serde_json::Value::String(fmt.trim().to_string());
        }

        let resp = self
            .http
            .post(format!("{}/audio/speech", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            return Err(provider_http_error(&self.provider_name, resp).await.into());
        }

        // Content-Type per il MIME reale; se assente lo deriviamo dal formato
        // richiesto (default mp3 -> audio/mpeg).
        let mime = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| mime_from_audio_format(response_format).to_string());

        // La risposta e' BINARIA: NON json(). Leggiamo i bytes.
        let bytes = resp.bytes().await?.to_vec();
        Ok((bytes, mime))
    }
}

/// MIME audio dal `response_format` richiesto al TTS. Copre i formati emessi
/// dall'API OpenAI Audio Speech. Default `audio/mpeg` (formato `mp3`, il default
/// del provider). Funzione PURA (testabile).
fn mime_from_audio_format(format: Option<&str>) -> &'static str {
    match format.map(|f| f.trim().to_lowercase()).as_deref() {
        Some("wav") => "audio/wav",
        Some("opus") => "audio/opus",
        Some("aac") => "audio/aac",
        Some("flac") => "audio/flac",
        Some("pcm") => "audio/pcm",
        // mp3 o assente -> default mp3.
        _ => "audio/mpeg",
    }
}

/// MIME audio dall'estensione del filename multipart. Copre i formati accettati
/// dall'API OpenAI Audio. `None` per estensioni non riconosciute (reqwest usa il
/// default; OpenAI inferisce dal file_name). Funzione PURA (testabile).
fn mime_from_filename(filename: &str) -> Option<&'static str> {
    let ext = filename.rsplit_once('.').map(|(_, e)| e.to_lowercase())?;
    let mime = match ext.as_str() {
        "mp3" | "mpga" | "mpeg" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "mp4" => "audio/mp4",
        "ogg" | "oga" => "audio/ogg",
        "flac" => "audio/flac",
        "webm" => "audio/webm",
        _ => return None,
    };
    Some(mime)
}

/// Mappa una [`ImagesResponse`] del dialetto OpenAI Images nel contratto
/// [`ImageGenResponse`]. Funzione PURA (testabile senza rete).
fn from_images_response(
    resp: ImagesResponse,
    model_used: String,
    provider_name: &str,
    latency_ms: u64,
) -> ImageGenResponse {
    let images = resp
        .data
        .into_iter()
        .map(|d| GeneratedImage {
            b64_json: d.b64_json.filter(|s| !s.is_empty()),
            url: d.url.filter(|s| !s.is_empty()),
            // OpenAI Images non dichiara il mime: e' sempre PNG inline; lasciamo
            // None per non inventare un valore (regola G/H).
            mime: None,
        })
        .collect();
    ImageGenResponse {
        images,
        model_used,
        provider_used: provider_name.to_string(),
        latency_ms,
    }
}

/// Estrae i nomi modello dalla risposta `GET /models` del dialetto OpenAI:
/// `{ "data": [{ "id": "..." }, ...] }`. Funzione PURA (regola L, testabile
/// senza rete): salta gli elementi senza `id` non-vuoto, deduplica e ordina per
/// output deterministico (parita' col brain `list_models_live`).
pub fn parse_models_response(body: &serde_json::Value) -> Vec<String> {
    parse_models_meta_response(body)
        .into_iter()
        .map(|m| m.id)
        .collect()
}

/// Variante CON METADATI di [`parse_models_response`] (punto unico del parsing,
/// regola L: la versione nomi-soli vi delega). Oltre all'`id`, estrae la
/// finestra di contesto DICHIARATA dal provider quando il dialetto la espone:
/// Mistral usa `max_context_length` in `data[]` (OpenAI/DeepSeek non hanno il
/// campo -> `None`). Valori non positivi sono trattati come non dichiarati:
/// meglio "ignota" di una finestra inventata (regola H, incidente 2026-07-06).
/// Ordinamento/dedup per id come la versione nomi-soli (output deterministico).
pub fn parse_models_meta_response(body: &serde_json::Value) -> Vec<crate::provider::ModelMeta> {
    let items = body.get("data").and_then(|d| d.as_array());
    let mut metas: Vec<crate::provider::ModelMeta> = items
        .map(|arr| arr.iter().filter_map(openai_model_meta_of).collect())
        .unwrap_or_default();
    metas.sort_by(|a, b| a.id.cmp(&b.id));
    metas.dedup_by(|a, b| a.id == b.id);
    metas
}

/// Mappa UN elemento di `data[]` (dialetto OpenAI) in [`ModelMeta`]: `id`
/// trimmato non-vuoto obbligatorio; `max_context_length` (Mistral) come
/// finestra dichiarata solo se positiva.
fn openai_model_meta_of(m: &serde_json::Value) -> Option<crate::provider::ModelMeta> {
    let id = m
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    let context_window = m
        .get("max_context_length")
        .and_then(serde_json::Value::as_i64)
        .filter(|w| *w > 0);
    Some(crate::provider::ModelMeta { id, context_window })
}

/// Parser SSE riusabile: accumula righe, le decodifica in [`LlmStreamChunk`] e
/// le accoda in `pending`. Stateful ma autonomo dal trasporto (testabile senza
/// rete: vedi i test sotto).
struct SseParser {
    line_buf: String,
    pending: std::collections::VecDeque<LlmStreamChunk>,
    done: bool,
    provider_name: String,
    model_used: String,
}

impl SseParser {
    /// Estrae dal buffer tutte le righe complete (terminate da `\n`) e le parsa,
    /// lasciando nel buffer l'eventuale riga parziale finale.
    fn drain_lines(&mut self) {
        while let Some(idx) = self.line_buf.find('\n') {
            let line = self.line_buf[..idx].to_string();
            // Rimuove la riga consumata (incluso il '\n').
            self.line_buf.drain(..=idx);
            self.parse_line(&line);
        }
    }

    /// Parsa una singola riga SSE. Le righe utili iniziano con `data:`; `[DONE]`
    /// chiude lo stream. Le altre (commenti, righe vuote) sono ignorate.
    fn parse_line(&mut self, line: &str) {
        let line = line.trim_end_matches('\r');
        let payload = match line.strip_prefix("data:") {
            Some(p) => p.trim(),
            None => return,
        };
        if payload.is_empty() {
            return;
        }
        if payload == "[DONE]" {
            self.done = true;
            return;
        }
        let parsed: ChatCompletionChunk = match serde_json::from_str(payload) {
            Ok(p) => p,
            // Frammento JSON non valido: lo ignoriamo (puo' arrivare spezzato in
            // un blocco di byte successivo gia' gestito dal buffer riga).
            Err(_) => return,
        };
        if let Some(chunk) = chunk_from_sse(parsed, &self.provider_name, &self.model_used) {
            self.pending.push_back(chunk);
        }
    }
}

/// Costruisce il corpo JSON della richiesta `/chat/completions`.
///
/// `stream=true` aggiunge anche `stream_options.include_usage` per ottenere il
/// conteggio token nell'ultimo chunk (parita' col TS).
///
/// `reasoning` governa le differenze di dialetto (regola L, punto unico):
///   - [`ReasoningDialect::None`] (Mistral, vLLM, OpenAI chat): `max_tokens`
///     standard, nessun parametro reasoning;
///   - [`ReasoningDialect::OpenAiReasoning`] (o-series/gpt-5): `max_tokens`
///     diventa `max_completion_tokens`, temperatura omessa (non accettata) e si
///     invia `reasoning_effort` se presente;
///   - [`ReasoningDialect::DeepSeek`]: `extra_body.thinking.type` enabled/disabled.
/// Identificatore stabile del gruppo di chiamate che condividono il prefisso,
/// per i soli endpoint [`PromptCacheKeying::RequiresKey`].
///
/// ## Cosa entra, e perche' quello
///
/// Solo la parte di prompt che NON cresce da un turno all'altro: system prompt e
/// nomi dei tool. La conversazione e' esclusa per costruzione — e' cio' che
/// cambia a ogni chiamata, e una chiave che cambia a ogni chiamata non raggruppa
/// niente: varrebbe quanto non mandarla, che e' il difetto che questa funzione
/// chiude. Sul run misurato il prompt cresceva 14.198 -> 17.586 token con la
/// testa ferma: e' esattamente la forma su cui il riuso del prefisso rende.
///
/// `tenant_id` e `user_id` entrano perche' sono stabili quanto il system prompt
/// e tengono separati gli spazi di chi chiama, senza costare un riuso: dentro un
/// run non cambiano mai.
///
/// ## Perche' derivarla qui e non farsela passare
///
/// L'alternativa era propagare un id di sessione dai chiamanti. Sarebbe stata la
/// stessa informazione presa per la strada lunga: ogni percorso che apre una
/// conversazione (chat, run agentico, sub-agente, worker) avrebbe dovuto
/// ricordarsi di popolarla, e quello che se ne fosse dimenticato avrebbe smesso
/// di cacheare in silenzio — un difetto invisibile, uguale a quello di partenza.
/// Derivandola dal prefisso la chiave e' corretta per costruzione: identifica
/// cio' che deve identificare perche' E' quello.
///
/// L'hash e' opaco anche per contratto del provider, che vieta di mettere dati
/// sensibili nella chiave.
fn prompt_cache_key(req: &LlmRequest) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(req.metadata.tenant_id.as_bytes());
    hasher.update([0]);
    hasher.update(req.metadata.user_id.as_bytes());
    hasher.update([0]);
    for msg in req.messages.iter().filter(|m| m.role == "system") {
        // Impronta del contenuto serializzato invece del solo testo: copre allo
        // stesso modo il system a blocchi e quello a stringa, e resta valida se
        // la forma dei blocchi cambia. Un contenuto non serializzabile non
        // esiste per questi tipi; se mai lo diventasse, saltarlo degrada il
        // raggruppamento, non la correttezza (la chiave e' un hint).
        //
        // Del system entra la sola PARTE STABILE (punto unico
        // `nexus_types::system_prompt`, regola L): le direttive di turno che il
        // motore vi appende dietro il confine sono ricalcolate a ogni chiamata, e
        // includerle rimetterebbe la chiave in movimento — che e' esattamente il
        // difetto che questa funzione esiste per chiudere. Un system senza
        // confine resta integralmente stabile, quindi le chiamate che non
        // appendono nulla hanno la chiave di prima (invariato).
        let stabile = match &msg.content {
            MessageContent::Text(s) => {
                MessageContent::Text(nexus_types::system_prompt::parte_stabile(s).to_string())
            }
            altro => altro.clone(),
        };
        if let Ok(bytes) = serde_json::to_vec(&stabile) {
            hasher.update(&bytes);
        }
        hasher.update([0]);
    }
    if let Some(tools) = req.tools.as_ref() {
        for t in tools {
            hasher.update(t.function.name.as_bytes());
            hasher.update([0]);
        }
    }
    // 128 bit in esadecimale: la chiave e' un'etichetta di raggruppamento, non
    // un segreto, e il provider la vuole corta.
    format!("{:x}", hasher.finalize())[..32].to_string()
}

fn build_request_body(
    req: &LlmRequest,
    stream: bool,
    reasoning: &ResolvedReasoning,
    cache_keying: PromptCacheKeying,
    upstream_order: Option<&[String]>,
) -> ChatCompletionRequest {
    let mut messages: Vec<WireMessage> = req.messages.iter().map(to_wire_message).collect();

    // ROUND-TRIP reasoning_content (DeepSeek, Kimi): per gli assistant message
    // generati in thinking mode l'API IMPONE che il `reasoning_content` venga
    // ri-passato nelle richieste successive, altrimenti HTTP 400. Lo facciamo
    // viaggiare SOLO verso quei dialetti: per ogni coppia (wire, sorgente) con
    // role=="assistant" e `reasoning` non vuoto, copiamo il reasoning della
    // history nel campo wire. Gli altri dialetti non vedono mai il campo (resta
    // None -> omesso). Speculare al round-trip `thinking_signature` di Anthropic.
    //
    // La differenza fra i due sta nella FORZA del vincolo, e va detta con
    // precisione: DeepSeek risponde 400 se il campo manca, e la sua via di fuga
    // e' spegnere il pensiero (cio' che fa `deepseek::resolve_reasoning` coi
    // tool). Kimi non rifiuta — misurato su k2.6 e k2.7-code — ma il pensiero
    // non si spegne, quindi ogni turno ne produce uno nuovo: senza il
    // round-trip il modello riparte ogni volta senza il proprio ragionamento
    // precedente. Li' e' continuita', non protezione da un errore.
    if matches!(
        reasoning.dialect,
        ReasoningDialect::DeepSeek | ReasoningDialect::Kimi
    ) {
        for (wire, src) in messages.iter_mut().zip(req.messages.iter()) {
            if wire.role == "assistant" {
                if let Some(r) = src.reasoning.as_ref().filter(|r| !r.is_empty()) {
                    wire.reasoning_content = Some(r.clone());
                }
            }
        }
    }

    let tools = req.tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|t| WireTool {
                kind: "function".to_string(),
                function: WireToolFn {
                    name: t.function.name.clone(),
                    description: t.function.description.clone(),
                    parameters: t.function.parameters.clone(),
                    strict: t.function.strict,
                },
            })
            .collect()
    });

    // Tetto di output via `max_completion_tokens` invece di `max_tokens`, e
    // temperatura NON inviata: due dialetti lo pretendono per ragioni diverse e
    // il codice le tiene distinte, perche' il giorno in cui una delle due
    // cambia si tocca un predicato solo. o-series: l'API rifiuta la temperatura
    // sui modelli reasoning. Kimi: la temperatura e' un valore FISSO del modello
    // e "passing any other value returns an error", mentre `max_tokens` e'
    // deprecato (doc Moonshot, vedi [`ReasoningDialect::Kimi`]).
    let tetto_su_completion = matches!(
        reasoning.dialect,
        ReasoningDialect::OpenAiReasoning | ReasoningDialect::Kimi
    );
    let temperatura_rifiutata = tetto_su_completion;
    let (max_tokens, max_completion_tokens) = if tetto_su_completion {
        (None, req.max_tokens)
    } else {
        (req.max_tokens, None)
    };
    let temperature = if temperatura_rifiutata {
        None
    } else {
        req.temperature
    };
    // `reasoning_effort` resta del solo dialetto o-series: su Kimi e' accettato
    // dal solo `kimi-k3` e nessun chiamante lo esprime oggi (vedi il commento
    // della variante), quindi qui non avrebbe un produttore.
    let reasoning_effort = if reasoning.dialect == ReasoningDialect::OpenAiReasoning {
        reasoning.effort.clone()
    } else {
        None
    };

    // DeepSeek: thinking ufficiale via extra_body. Lo inviamo SOLO quando vogliamo
    // forzare uno stato esplicito (disabled per task interni/tool; enabled su
    // richiesta thinking). Senza extra_body DeepSeek usa il suo default.
    let extra_body = if reasoning.dialect == ReasoningDialect::DeepSeek {
        let kind = if reasoning.enabled { "enabled" } else { "disabled" };
        Some(serde_json::json!({ "thinking": { "type": kind } }))
    } else {
        None
    };

    // tool_choice: dialetto OpenAI nativo, inoltrato tale e quale (canonicalizzato)
    // via il punto unico di mapping (regola L). Inviato solo quando c'e' un
    // vincolo riconosciuto E ci sono tool da scegliere (senza tools sarebbe
    // ignorato/rifiutato dall'API).
    let tool_choice = req
        .tool_choice
        .as_ref()
        .filter(|_| tools.is_some())
        .and_then(super::tool_choice::to_openai);

    ChatCompletionRequest {
        model: req.model.clone(),
        messages,
        // Solo verso gli endpoint che la richiedono: altrove sarebbe un campo
        // sconosciuto in un dialetto che non lo documenta. Stessa chiave in ogni
        // caso, perche' la domanda a cui risponde e' la stessa; cambia il campo
        // perche' cambia chi la legge.
        //
        // Su un INSTRADATORE (RequiresSessionId) i campi servono ENTRAMBI, perche'
        // i lettori sono DUE: `session_id` lo legge l'instradatore per fissare il
        // fornitore, `prompt_cache_key` viene inoltrato a quel fornitore, che ci
        // fissa il proprio server interno. MISURATO su OpenRouter->xAI il
        // 29/07/2026, 4 chiamate consecutive a prefisso identico: col solo
        // `session_id` la cache non arriva mai (128 token fissi, il blocco
        // minimo); col solo `prompt_cache_key` 8704/8797 stabile dal secondo
        // colpo; con entrambi idem. Il fornitore la cache la gestiva benissimo:
        // eravamo noi a non dirgli quale conversazione fosse.
        prompt_cache_key: match cache_keying {
            PromptCacheKeying::RequiresKey | PromptCacheKeying::RequiresSessionId => {
                Some(prompt_cache_key(req))
            }
            PromptCacheKeying::ProviderManaged => None,
        },
        session_id: match cache_keying {
            PromptCacheKeying::RequiresSessionId => Some(prompt_cache_key(req)),
            PromptCacheKeying::ProviderManaged | PromptCacheKeying::RequiresKey => None,
        },
        // Terzo livello di affinita': QUALE fornitore a valle. `session_id`
        // doveva fissarlo e non lo fissa (misurato: 8 chiamate consecutive
        // ripartite su tre fornitori), e i fornitori dello stesso modello non si
        // equivalgono — su qwen3-235b solo Google serve il prefisso. Il criterio
        // e' nell'enum, il valore nel DB (mig 0657): qui si applica soltanto.
        provider: upstream_order
            .filter(|_| cache_keying.requires_upstream_pinning())
            .filter(|o| !o.is_empty())
            .map(|o| WireProviderRouting {
                order: o.to_vec(),
                allow_fallbacks: true,
            }),
        temperature,
        max_tokens,
        max_completion_tokens,
        reasoning_effort,
        extra_body,
        tools,
        tool_choice,
        response_format: req.response_format.clone(),
        stream: if stream { Some(true) } else { None },
        stream_options: if stream {
            Some(StreamOptions { include_usage: true })
        } else {
            None
        },
    }
}

/// Alcuni provider OpenAI-compat stretti (es. Mistral) RIFIUTANO con HTTP 400
/// ("Expected last role User or Tool (or Assistant with prefix True) ... but got
/// assistant") una richiesta il cui ULTIMO messaggio ha role "assistant" senza
/// tool-call pendenti. Nei run agentici la cronologia puo' terminare con un
/// assistant interlocutorio o, in cascade/fallback, con la risposta di un altro
/// provider. Rimuoviamo i trailing assistant SENZA tool_calls cosi' l'ultimo role
/// e' user/tool; gli assistant CON tool_calls pendenti restano (parte valida del
/// flusso tool). Porting del fix Python `_strip_trailing_assistant` perso nel
/// cutover a Rust. Mantiene sempre almeno un messaggio.
fn strip_trailing_assistant(messages: &mut Vec<WireMessage>) {
    while messages.len() > 1 {
        let drop_last = matches!(
            messages.last(),
            Some(m) if m.role == "assistant" && m.tool_calls.is_none()
        );
        if drop_last {
            messages.pop();
        } else {
            break;
        }
    }
}

/// True per i provider che esigono come ULTIMO messaggio role `user` o `tool`
/// (assistant trailing rifiutato dall'API). Delega al sanitizer autoritativo
/// (regola L): un solo punto di controllo cross-provider.
fn provider_requires_user_or_tool_last(provider: &str) -> bool {
    crate::history_sanitizer::provider_requires_user_or_tool_last(provider)
}

/// Converte un [`crate::types::LlmMessage`] nel formato wire OpenAI.
///
/// Il content e' una stringa nel caso semplice. Quando e' una lista di blocchi
/// (`MessageContent::Blocks`):
///   - se contiene blocchi immagine (`image_url`) si emette un content ARRAY
///     nativo OpenAI (`[{type:"text",...}, {type:"image_url", image_url:{url}}]`)
///     cosi' la capability vision e' preservata (regola: il gateway non deve
///     perdere le immagini quando elimineremo `brain/providers`);
///   - altrimenti (solo testo / tool_result) si ricade sulla serializzazione a
///     stringa (parita' col TS che fa `JSON.stringify`).
/// Per i messaggi `assistant` con tool-call il content puo' essere `null`.
fn to_wire_message(msg: &crate::types::LlmMessage) -> WireMessage {
    use crate::types::MessageContent;

    let content_value = match &msg.content {
        MessageContent::Text(s) => Some(WireContent::Text(s.clone())),
        MessageContent::Blocks(blocks) => {
            if blocks.iter().any(|b| b.kind == "image_url") {
                Some(WireContent::Parts(blocks_to_openai_parts(blocks)))
            } else {
                // Nessuna immagine: parita' col TS (JSON.stringify dei blocchi).
                serde_json::to_string(blocks)
                    .ok()
                    .map(WireContent::Text)
                    .or(Some(WireContent::Text(String::new())))
            }
        }
    };

    let tool_calls = msg.tool_calls.as_ref().map(|calls| {
        calls
            .iter()
            .map(|tc| WireToolCall {
                id: tc.id.clone(),
                kind: "function".to_string(),
                function: WireToolCallFn {
                    name: tc.function.name.clone(),
                    arguments: tc.function.arguments.clone(),
                },
            })
            .collect::<Vec<_>>()
    });

    // assistant con tool_calls: content puo' essere null (parita' TS).
    let content = if msg.role == "assistant" && tool_calls.is_some() {
        match &msg.content {
            MessageContent::Text(s) if !s.is_empty() => Some(WireContent::Text(s.clone())),
            _ => None,
        }
    } else {
        content_value
    };

    // ESITO DEL TOOL (regola Q): questo dialetto non ha un campo per dirlo — il
    // tool message e' `{role, tool_call_id, content}` e basta. Il degrado e'
    // dichiarato in un punto solo (`tool_error_channel`), che compone il testo
    // DAL campo `is_error`; senza, un tool fallito arriverebbe al modello
    // indistinguibile da uno riuscito da quando il marker non e' piu' nel testo.
    let content = match (msg.role.as_str(), content) {
        ("tool", Some(WireContent::Text(testo))) => Some(WireContent::Text(
            crate::providers::tool_error_channel::testo_con_esito_dichiarato(testo, msg.is_error),
        )),
        (_, altro) => altro,
    };

    WireMessage {
        role: msg.role.clone(),
        content,
        tool_call_id: msg.tool_call_id.clone(),
        tool_calls,
        name: msg.name.clone(),
        // Popolato a valle in `build_request_body` SOLO per il dialetto DeepSeek
        // (round-trip del reasoning_content): qui resta None, neutro per gli altri.
        reasoning_content: None,
    }
}

/// Mappa i blocchi del contratto nel content array nativo OpenAI. I blocchi
/// `image_url` mantengono il formato OpenAI nativo (`{type:"image_url",
/// image_url:{url, detail?}}`, dove `url` puo' essere `http(s)` o
/// `data:<mime>;base64,<...>`). I blocchi testuali diventano
/// `{type:"text", text}`. I blocchi `tool_result` (qui inattesi nel content
/// array) sono serializzati come testo per non perderne il payload.
fn blocks_to_openai_parts(blocks: &[crate::types::LlmContentBlock]) -> Vec<serde_json::Value> {
    blocks
        .iter()
        .filter_map(|b| match b.kind.as_str() {
            "image_url" => b
                .image_url
                .as_ref()
                .map(|iu| serde_json::json!({"type": "image_url", "image_url": iu})),
            "text" => Some(serde_json::json!({
                "type": "text",
                "text": b.text.clone().unwrap_or_default(),
            })),
            _ => b.content.as_ref().map(|c| {
                serde_json::json!({"type": "text", "text": c})
            }),
        })
        .collect()
}

/// Parsa il body 200 di `/chat/completions` in [`ChatCompletion`] con errore
/// CONTESTUALIZZATO: provider + causa serde (campo mancante/tipo inatteso, con
/// riga e colonna), mai il contenuto del body (regola F). Funzione PURA
/// (testabile senza rete); punto unico del parse non-streaming (regola L).
fn parse_chat_completion(provider: &str, body: &str) -> anyhow::Result<ChatCompletion> {
    serde_json::from_str(body).map_err(|e| {
        // Causa serde troncata: per gli invalid-type su stringhe serde include
        // il valore nel messaggio; il taglio evita di trascinare contenuto di
        // risposta nel canale d'errore (regola F) mantenendo campo/riga/colonna.
        let cause: String = e.to_string().chars().take(200).collect();
        anyhow::anyhow!("{provider}: risposta 200 non decodificabile come ChatCompletion ({cause})")
    })
}

/// Mappa una [`ChatCompletion`] non-streaming in [`LlmResponse`].
fn from_chat_completion(
    resp: ChatCompletion,
    model_used: String,
    provider_name: &str,
    latency_ms: u64,
) -> anyhow::Result<LlmResponse> {
    let choice = resp
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("{}: nessuna choice nella risposta", provider_name))?;

    let tool_calls: Option<Vec<LlmToolCall>> = choice.message.tool_calls.map(|calls| {
        calls
            .into_iter()
            .map(|tc| LlmToolCall {
                id: tc.id,
                kind: "function".to_string(),
                function: ToolFunctionCall {
                    name: tc.function.name,
                    arguments: tc.function.arguments,
                },
                // Firma per-call specifica di Gemini: assente sui provider
                // OpenAI-compatibili.
                thought_signature: None,
            })
            .collect()
    });

    // Prompt caching automatico (DeepSeek `prompt_cache_hit_tokens`, OpenAI
    // `prompt_tokens_details.cached_tokens`): il dialetto li conta DENTRO
    // `prompt_tokens`, che e' quindi gia' il LORDO voluto dal sistema. Il punto
    // unico `LlmUsage::normalized` (regola L) non tocca il conteggio; qui si
    // dichiara solo la convenzione del formato.
    let usage = LlmUsage::normalized(
        PromptCacheReporting::CachedIncludedInPrompt,
        resp.usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
        resp.usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0),
        resp.usage.as_ref().and_then(|u| u.cached_input_tokens()),
        // Nessun dialetto OpenAI-compatibile espone un costo di SCRITTURA della
        // cache: il caching e' automatico e il miss paga la tariffa input piena.
        None,
        // Nel dialetto OpenAI il ragionamento e' DENTRO `completion_tokens`
        // (`completion_tokens_details.reasoning_tokens` ne e' il dettaglio, non
        // un addendo): vale per o1/o3, per il `reasoning_content` DeepSeek e per
        // ogni compatibile. Sommarlo qui raddoppierebbe l'addebito.
        ReasoningTokens::IncludedInOutput,
    );

    // Citazioni top-level (Perplexity): estratte prima di costruire la risposta.
    let citations = resp.citations.filter(|c| !c.is_empty());

    let finish_reason = normalize_finish_reason(choice.finish_reason.as_deref());

    // Reasoning DeepSeek: arriva nel campo separato `reasoning_content`. OpenAI
    // o-series non espone il reasoning come testo (solo i token, gia' nel usage),
    // quindi qui resta `None` per quel dialetto.
    let reasoning = choice
        .message
        .reasoning_content
        .filter(|r| !r.is_empty());

    Ok(LlmResponse {
        content: choice.message.content.unwrap_or_default(),
        tool_calls,
        usage,
        model_used,
        provider_used: provider_name.to_string(),
        latency_ms,
        finish_reason,
        privacy_rerouted: None,
        reasoning,
        // Dialetto OpenAI-compat: nessuna signature opaca da ri-passare.
        thinking_signature: None,
        citations,
        // La riga di ledger la scrive la pipeline HTTP, non il provider.
        ledger: None,
    })
}

/// Mappa un chunk SSE in [`LlmStreamChunk`]. Ritorna `None` se il chunk non
/// porta delta utili (es. solo metadati di apertura).
fn chunk_from_sse(
    chunk: ChatCompletionChunk,
    provider_name: &str,
    model_used: &str,
) -> Option<LlmStreamChunk> {
    let usage = chunk.usage.map(|u| {
        LlmUsage::normalized(
            PromptCacheReporting::CachedIncludedInPrompt,
            u.prompt_tokens,
            u.completion_tokens,
            u.cached_input_tokens(),
            None,
            // Come nel non-streaming: gia' dentro `completion_tokens`.
            ReasoningTokens::IncludedInOutput,
        )
    });

    let choice = chunk.choices.into_iter().next();
    let finish_reason = choice
        .as_ref()
        .and_then(|c| c.finish_reason.clone())
        .map(|r| normalize_finish_reason(Some(&r)));

    let delta = choice.as_ref().and_then(|c| c.delta.as_ref());

    // Tool-call delta: emette il primo (parita' col TS che yield-a tc[0]).
    if let Some(d) = delta {
        if let Some(tc) = d.tool_calls.as_ref().and_then(|v| v.first()) {
            return Some(LlmStreamChunk {
                delta: String::new(),
                tool_call_delta: Some(ToolCallDelta {
                    index: tc.index,
                    id: tc.id.clone(),
                    function: tc.function.as_ref().map(|f| ToolCallDeltaFunction {
                        name: f.name.clone(),
                        arguments: f.arguments.clone(),
                    }),
                }),
                finish_reason: None,
                usage: None,
                provider_used: Some(provider_name.to_string()),
                model_used: Some(model_used.to_string()),
                reasoning_delta: None,
            });
        }
    }

    let content_delta = delta.and_then(|d| d.content.clone()).unwrap_or_default();
    // Reasoning DeepSeek in streaming: campo separato `reasoning_content` nel
    // delta. Va in `reasoning_delta`, non in `delta` (parita' col round-trip
    // reasoning del brain).
    let reasoning_delta = delta
        .and_then(|d| d.reasoning_content.clone())
        .filter(|r| !r.is_empty());

    // Niente delta di testo, niente reasoning, niente finish, niente usage:
    // chunk vuoto, salta.
    if content_delta.is_empty()
        && reasoning_delta.is_none()
        && finish_reason.is_none()
        && usage.is_none()
    {
        return None;
    }

    // L'usage va riportato solo all'ultimo chunk (quando c'e' finish_reason),
    // come nel TS.
    let usage = if finish_reason.is_some() { usage } else { None };

    Some(LlmStreamChunk {
        delta: content_delta,
        tool_call_delta: None,
        finish_reason,
        usage,
        provider_used: Some(provider_name.to_string()),
        model_used: Some(model_used.to_string()),
        reasoning_delta,
    })
}

/// Normalizza il `finish_reason` ai valori canonici del contratto. I valori non
/// noti collassano a `stop` (parita' col `finishReasonMap` del TS).
fn normalize_finish_reason(raw: Option<&str>) -> String {
    match raw.unwrap_or("stop") {
        "length" => "length",
        "tool_calls" => "tool_calls",
        "content_filter" => "content_filter",
        _ => "stop",
    }
    .to_string()
}

/// Classe di errore provider ai fini della strategia retry/cooldown.
///
/// E' un RE-EXPORT: la definizione vive in `nexus-types` accanto al vocabolario
/// di wire (`provider_failure::classe`), perche' la traduzione classe->stringa
/// e' il contratto che mcp-core legge e finche' l'enum stava qui quella
/// traduzione era scritta a mano in due punti (regola L). Il nome storico resta
/// per non toccare i quattro `match` dei call site.
pub use nexus_types::provider_failure::ClasseErrore as ProviderErrorKind;

/// Errore HTTP di un provider, con lo status NUMERICO (segnale CERTO) e il
/// codice d'errore STRUTTURATO estratto dal JSON (`error.code`/`error.type`/
/// `error.status`, identificatore macchina STABILE). Sostituisce la
/// classificazione fragile sul testo del messaggio (regola H): il testo puo'
/// cambiare per provider/versione/lingua, lo status e il codice no.
///
/// `Display` e' IDENTICO al vecchio `bail!("{provider} HTTP {status}: {body}")`
/// cosi' i chiamanti legacy che leggono `to_string()` non cambiano
/// comportamento, mentre il codice nuovo fa `downcast` per accedere ai campi
/// strutturati.
#[derive(Debug)]
pub struct ProviderHttpError {
    pub provider: String,
    pub status: u16,
    /// Codice d'errore strutturato dal body JSON (lowercase), se presente.
    ///
    /// E' il valore che viaggia sul WIRE (`failures[].code`, `ErrorFacts.code`)
    /// e che i consumatori a valle confrontano per uguaglianza: resta quello
    /// storico ([`CandidatiErrore::codice_esportato`]). Chi deve DECIDERE non
    /// legge questo campo ma [`Self::candidati`], perche' un solo valore non
    /// basta: e' l'aver collassato sei campi in uno che ha reso invisibile il
    /// credito esaurito di openai per 14 giorni.
    pub code: Option<String>,
    /// TUTTI i campi d'errore osservati nel body, in ordine di rango. Chi
    /// classifica decide sul primo RICONOSCIUTO, non sul primo presente.
    pub candidati: CandidatiErrore,
    /// Secondi indicati dall'header `Retry-After` (RFC 9457/7231), se il provider
    /// lo fornisce (es. Mistral/OpenAI su 429). Segnale AUTORITATIVO di quanto
    /// attendere prima di ritentare: ha precedenza sul backoff calcolato.
    pub retry_after_seconds: Option<u64>,
    /// Body grezzo, SOLO per logging/display: mai usato per classificare.
    pub message: String,
}

impl std::fmt::Display for ProviderHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} HTTP {}: {}", self.provider, self.status, self.message)
    }
}

impl std::error::Error for ProviderHttpError {}

impl ProviderHttpError {
    /// Costruisce dall'HTTP status + body grezzo, estraendo il codice d'errore
    /// STRUTTURATO dal JSON (non dalla prosa).
    pub fn from_response(provider: &str, status: u16, body: String) -> Self {
        let mut candidati = CandidatiErrore::dal_body(&body);
        if let Some(sintetico) = quirk_del_fornitore(provider, status, &candidati, &body) {
            candidati = candidati.con_quirk(sintetico);
        }
        Self {
            provider: provider.to_string(),
            status,
            code: candidati.codice_esportato().map(str::to_string),
            candidati,
            retry_after_seconds: None,
            message: body,
        }
    }

    /// Imposta i secondi di `Retry-After` (builder). `None` lascia il default.
    pub fn with_retry_after(mut self, secs: Option<u64>) -> Self {
        self.retry_after_seconds = secs;
        self
    }

    /// Messaggio diagnostico STRUTTURATO (`error.message`) estratto dal body,
    /// per il solo logging/display (regola M): dice COSA e' invalido senza
    /// dumpare il body grezzo (`message`, che puo' contenere JSON di contorno).
    /// `None` se il body non e' JSON o non espone il campo.
    pub fn structured_message(&self) -> Option<String> {
        extract_structured_error_message(&self.message)
    }
}

/// Parsa l'header `Retry-After` in secondi. RFC 7231: gestiamo il formato
/// "delta-seconds" (intero); una data HTTP ritorna `None` (ripiego sul backoff
/// calcolato). Punto unico (regola L): i provider lo leggono da qui.
pub fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    // Clamp difensivo: un Retry-After assurdo non deve bloccare la richiesta per
    // ore; il caller applica comunque il proprio tetto.
    raw.trim().parse::<u64>().ok().map(|s| s.min(3600))
}

/// Costruisce un [`ProviderHttpError`] da una `Response` non-2xx catturando
/// status, header `Retry-After` e body (async: consuma la response). Punto unico
/// della costruzione errore HTTP dei provider OpenAI-compat (regola L).
pub async fn provider_http_error(provider: &str, resp: reqwest::Response) -> ProviderHttpError {
    let status = resp.status().as_u16();
    let retry_after = parse_retry_after(resp.headers());
    let body = resp.text().await.unwrap_or_default();
    ProviderHttpError::from_response(provider, status, body).with_retry_after(retry_after)
}

/// Estrae il MESSAGGIO d'errore leggibile dal body JSON di errore provider, per
/// la DIAGNOSI (regola M): il campo `message` del contratto d'errore dice COSA
/// e' invalido (es. Google: quale argomento e il limite atteso). Cerca (in
/// ordine) `error.message` e il `message` top-level. E' diagnostico, mai usato
/// per classificare (la classificazione resta su status + codice strutturato).
/// Ritorna `None` se il body non e' JSON o non ha un campo `message`.
fn extract_structured_error_message(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    for c in [v.pointer("/error/message"), v.get("message")]
        .into_iter()
        .flatten()
    {
        if let Some(s) = c.as_str() {
            let t = s.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Codice di fatturazione emesso quando un provider segnala credito esaurito
/// senza un identificatore proprio.
///
/// NON e' piu' una stringa che si fa riconoscere per la sottostringa `billing`:
/// e' un valore DICHIARATO nel catalogo (mig 0705, riga
/// `('anthropic','billing_error',400,'credit_exhausted')`), e un test contro la
/// migrazione vera fallisce se il quirk emette un valore che nessuna riga
/// dichiara. La dipendenza implicita — «lo riconoscono perche' contiene una
/// parola» — era esattamente il meccanismo che questo intervento elimina.
pub const CODICE_BILLING_NORMALIZZATO: &str = "billing_error";

/// Traduce nel vocabolario comune un codice che il provider riporta AMBIGUO.
///
/// Anthropic risponde al credito esaurito con status 400 e
/// `error.type = "invalid_request_error"`: lo STESSO identificatore che usa per
/// una richiesta davvero malformata. Chi legge solo status+codice non puo'
/// distinguere "non hai credito" da "hai sbagliato la richiesta", e il gateway
/// infatti trattava il credito esaurito come errore di formato: faceva partire un
/// retry di sanificazione della history (rimedio che non c'entra e non puo'
/// funzionare) e NON metteva il provider in cooldown, continuando a sceglierlo e a
/// spendere una chiamata per ciclo (misurato il 26/07 sui log del gateway).
///
/// La regola M prevede questo caso: quando il provider non offre un codice
/// distinto, il quirk si ISOLA qui - dove sappiamo di chi e' la risposta - e si
/// traduce in un codice strutturato. Il punto di decisione a valle resta
/// deterministico e non impara nulla di provider-specifico.
///
/// Il testo e' consultato SOLO qui, SOLO per il provider che ha l'ambiguita' e
/// SOLO sul `error.message` strutturato: e' il perimetro minimo, non una
/// classificazione dalla prosa.
///
/// Ritorna il candidato SINTETICO da AGGIUNGERE (rango massimo), non piu' un
/// codice che sostituisce gli altri: il resto dei campi resta osservabile, cosi'
/// il giorno in cui anthropic pubblichera' un identificatore distinto quel
/// codice comparira' fra i non dichiarati e il quirk si potra' togliere.
fn quirk_del_fornitore(
    provider: &str,
    status: u16,
    candidati: &CandidatiErrore,
    body: &str,
) -> Option<&'static str> {
    let e_anthropic = provider.trim().eq_ignore_ascii_case("anthropic");
    let dice_invalid_request = candidati
        .iter()
        .any(|c| c.valore == "invalid_request_error");
    if e_anthropic && status == 400 && dice_invalid_request && dichiara_credito_esaurito(body) {
        return Some(CODICE_BILLING_NORMALIZZATO);
    }
    None
}

/// Il body dichiara credito/saldo insufficiente? Guarda il `error.message`
/// STRUTTURATO (non il body grezzo), sulle formule con cui Anthropic segnala il
/// saldo esaurito: "credit balance ... too low" e il rimando a Plans & Billing.
fn dichiara_credito_esaurito(body: &str) -> bool {
    let Some(msg) = extract_structured_error_message(body) else {
        return false;
    };
    let m = msg.to_ascii_lowercase();
    m.contains("credit balance") || m.contains("plans & billing")
}

/// Il codice esportato sul wire e i CANDIDATI da cui si classifica nascono
/// insieme in [`ProviderHttpError::from_response`]. La classificazione vive nel
/// punto unico [`crate::tassonomia_errori`]: qui resta solo la costruzione
/// dell'errore, perche' decidere richiede il CATALOGO dei codici (mig 0705) e
/// il catalogo richiede il DB — che questo modulo non ha e non deve avere sul
/// percorso di una chiamata fallita.

// ---------------------------------------------------------------------------
// Tipi wire (formato OpenAI Chat Completions). Separati dai tipi di contratto
// per non accoppiare la serializzazione del dialetto provider al contratto del
// gateway.
// ---------------------------------------------------------------------------

/// Corpo `/chat/completions`. `pub(crate)` per il solo tipo (i campi restano
/// privati): e' cio' che [`OpenAiCompatClient::corpo_della_richiesta`] ritorna, e
/// i test degli adapter che lo compongono devono poterlo serializzare per
/// guardare i campi che partono davvero.
#[derive(Debug, Serialize)]
pub(crate) struct ChatCompletionRequest {
    model: String,
    messages: Vec<WireMessage>,
    /// Identificatore stabile del gruppo di chiamate che condividono il
    /// prefisso. Senza, gli endpoint [`PromptCacheKeying::RequiresKey`] non
    /// riusano nulla: misurato su Mistral, `cached_tokens` resta 0 anche
    /// ripetendo lo stesso prefisso di 11.918 token a pochi secondi di
    /// distanza. Omesso per gli altri dialetti.
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
    /// Chiave di instradamento adesivo per gli endpoint che smistano verso
    /// fornitori terzi: tiene i turni di una stessa conversazione sul fornitore
    /// che ha gia' il prefisso caldo. Senza, la richiesta funziona lo stesso ma
    /// si paga il prompt intero.
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    /// Fornitore a valle preferito, per gli instradatori. Necessario perche'
    /// `session_id` da solo NON lo tiene fermo: misurato il 29/07/2026 su
    /// OpenRouter, la stessa sequenza di 8 chiamate girava fra tre fornitori.
    /// Omesso ovunque non serva.
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<WireProviderRouting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    /// Tetto di output per i modelli o-series/gpt-5 (al posto di `max_tokens`).
    #[serde(rename = "max_completion_tokens", skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    /// Livello di reasoning (low/medium/high) per i modelli o-series.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<WireTool>>,
    /// Vincolo di scelta tool in formato OpenAI nativo (stringa o oggetto).
    /// Inoltrato tale e quale; omesso quando assente.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    /// Campi extra appiattiti nel body radice (DeepSeek `thinking`): il client
    /// OpenAI ufficiale fonde `extra_body` nel top-level, quindi facciamo lo
    /// stesso con `serde(flatten)`. `None` => nessun campo aggiunto.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    extra_body: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct StreamOptions {
    include_usage: bool,
}

/// Preferenza di fornitore a valle, dialetto degli instradatori (OpenRouter
/// `provider`).
///
/// `allow_fallbacks` resta TRUE per scelta misurata: il 29/07/2026, con i
/// ripieghi attivi, l'ordine da solo ha tenuto fermo il fornitore per 8 chiamate
/// su 8 sia su `z-ai/glm-4.7-flash` (DeepInfra) sia su
/// `qwen/qwen3-235b-a22b-2507` (Google). Non c'e' quindi ragione di pagare la
/// perdita del ripiego: con `false`, un fornitore giu' farebbe fallire la
/// richiesta invece di costare il solo riuso del prefisso.
#[derive(Debug, Serialize)]
struct WireProviderRouting {
    order: Vec<String>,
    allow_fallbacks: bool,
}

/// Corpo della richiesta `POST /images/generations` (dialetto OpenAI Images).
#[derive(Debug, Serialize)]
struct ImageGenWireRequest {
    model: String,
    prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<String>,
    response_format: String,
}

/// Risposta di `POST /images/generations`: `{ "data": [{ "b64_json"|"url" }] }`.
#[derive(Debug, Deserialize)]
struct ImagesResponse {
    #[serde(default)]
    data: Vec<ImageData>,
}

#[derive(Debug, Deserialize)]
struct ImageData {
    #[serde(default)]
    b64_json: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

/// Risposta di `POST /audio/transcriptions` con `response_format=json`:
/// `{ "text": "..." }`.
#[derive(Debug, Deserialize)]
struct TranscriptionResponse {
    #[serde(default)]
    text: String,
}

#[derive(Debug, Serialize)]
struct WireMessage {
    role: String,
    // Serializziamo sempre `content` (anche null) per i messaggi assistant con
    // tool-call, dove l'API richiede esplicitamente `content: null`.
    content: Option<WireContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<WireToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    // Reasoning del turno assistant prodotto in thinking mode da DeepSeek, da
    // RI-PASSARE all'API nelle richieste successive (vincolo HTTP 400: "The
    // reasoning_content in the thinking mode must be passed back to the API").
    // Valorizzato SOLO per il dialetto DeepSeek in `build_request_body`; per gli
    // altri provider resta `None` (omesso) cosi' il campo non viaggia mai verso
    // chi non lo conosce.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
}

/// Content di un messaggio nel wire OpenAI: stringa (caso semplice) o array di
/// parti tipizzate (testo + immagini, per le richieste vision). L'enum untagged
/// serializza direttamente al valore JSON (stringa o array) atteso dall'API.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum WireContent {
    Text(String),
    Parts(Vec<serde_json::Value>),
}

#[derive(Debug, Serialize)]
struct WireTool {
    #[serde(rename = "type")]
    kind: String,
    function: WireToolFn,
}

#[derive(Debug, Serialize)]
struct WireToolFn {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    parameters: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    strict: Option<bool>,
}

#[derive(Debug, Serialize)]
struct WireToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: WireToolCallFn,
}

#[derive(Debug, Serialize)]
struct WireToolCallFn {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletion {
    #[serde(default)]
    choices: Vec<RespChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
    /// Perplexity espone le fonti come array top-level `citations` (non standard
    /// OpenAI): mappato una sola volta qui, vale per ogni provider OpenAI-compat.
    #[serde(default)]
    citations: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct RespChoice {
    message: RespMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RespMessage {
    /// Content della risposta. Il dialetto OpenAI classico e' una STRINGA, ma
    /// Mistral (contratto ufficiale: `content: string | ContentChunk[]`) puo'
    /// rispondere con un ARRAY di chunk (`{type:"text", text}`, reference,
    /// thinking). Un `Option<String>` rigido faceva fallire l'intero parse
    /// ("error decoding response body", classificato transitorio e ritentato a
    /// vuoto): il deserializzatore tollerante estrae il testo dai chunk `text`
    /// e ignora gli altri.
    #[serde(default, deserialize_with = "deserialize_lenient_content")]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<RespToolCall>>,
    /// Reasoning DeepSeek (campo separato dal content). Assente sugli altri
    /// provider OpenAI-compat.
    #[serde(default)]
    reasoning_content: Option<String>,
}

/// Deserializza un content wire tollerante: stringa as-is, array di chunk
/// concatenando i soli `{type:"text"}` (il resto — reference, thinking — non e'
/// testo di risposta), `null`/assente -> `None`. Punto unico riusato da
/// response e delta streaming (regola L).
fn deserialize_lenient_content<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(v.and_then(|v| match v {
        serde_json::Value::String(s) => Some(s),
        serde_json::Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(""),
        ),
        // null gia' mappato a None da Option; altri tipi inattesi -> None
        // (il chiamante degrada a content vuoto, non a parse fallito).
        _ => None,
    }))
}

#[derive(Debug, Deserialize)]
struct RespToolCall {
    id: String,
    function: RespToolCallFn,
}

#[derive(Debug, Deserialize)]
struct RespToolCallFn {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    /// DeepSeek: token di input serviti dal context caching automatico.
    #[serde(default)]
    prompt_cache_hit_tokens: Option<u32>,
    /// OpenAI: dettaglio dei token di input, con `cached_tokens`.
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
    /// Moonshot/Kimi: lo STESSO nome di OpenAI, ma al PRIMO livello di `usage`.
    /// E' la sola forma che lo schema ufficiale dell'endpoint chat dichiara:
    /// `{prompt_tokens, completion_tokens, total_tokens, cached_tokens}`.
    ///
    /// MISURATO il 09/08/2026 su `kimi-k2.6` (tre chiamate a prefisso identico da
    /// 4267 token): l'API ne espone DUE, `usage.cached_tokens` e
    /// `usage.prompt_tokens_details.cached_tokens`, entrambi a 4096 — quindi oggi
    /// il ramo OpenAI qui sopra basterebbe da solo. Il campo resta perche' lo
    /// scarto e' fra la doc e l'implementazione, e a colmarsi puo' essere l'una o
    /// l'altra: se il fornitore allineasse la risposta al proprio schema, questo
    /// e' l'unico ramo che continuerebbe a leggere. Costa un `Option` e copre il
    /// verso in cui il difetto sarebbe MUTO — `cache_read_tokens` a zero per
    /// sempre, hit-rate 0%, sconto mai applicato, e nessun errore da nessuna
    /// parte (la firma del `thoughtsTokenCount` di Google).
    #[serde(default)]
    cached_tokens: Option<u32>,
}

impl WireUsage {
    /// Token di input serviti da cache, normalizzati cross-provider: DeepSeek li
    /// espone in `prompt_cache_hit_tokens`, OpenAI in
    /// `prompt_tokens_details.cached_tokens`, Moonshot/Kimi in `cached_tokens`
    /// al primo livello. Ritorna `None` se tutti assenti o a zero.
    ///
    /// I tre nomi si interrogano in cascata e non per provider: un dialetto
    /// che riusasse uno di questi nomi verrebbe letto senza toccare nulla, e un
    /// `match provider` qui sarebbe la logica dispersa che la regola L vieta.
    fn cached_input_tokens(&self) -> Option<u32> {
        let hit = self
            .prompt_cache_hit_tokens
            .or_else(|| self.prompt_tokens_details.as_ref().and_then(|d| d.cached_tokens))
            .or(self.cached_tokens);
        hit.filter(|&n| n > 0)
    }
}

#[derive(Debug, Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Debug, Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    delta: Option<ChunkDelta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChunkDelta {
    /// Stesso contratto tollerante del content non-streaming (stringa o array
    /// di chunk): un delta array altrimenti veniva scartato in silenzio dal
    /// parser SSE (risposta troncata senza errore).
    #[serde(default, deserialize_with = "deserialize_lenient_content")]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChunkToolCallDelta>>,
    /// Delta del reasoning DeepSeek in streaming (campo separato).
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChunkToolCallDelta {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ChunkToolCallDeltaFn>,
}

#[derive(Debug, Deserialize)]
struct ChunkToolCallDeltaFn {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LlmMessage, MessageContent, RequestMetadata};

    fn sample_request() -> LlmRequest {
        LlmRequest {
            model: "test-model".to_string(),
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
            max_tokens: Some(256),
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
        }
    }

    /// Il degrado DICHIARATO: questo dialetto non ha un campo per l'esito, e il
    /// fallimento arriva al modello nel testo composto DAL campo.
    ///
    /// Non e' un ritorno al marker: la direzione consentita dalla regola Q e'
    /// proprio questa (il testo si compone dai campi, mai il contrario), il
    /// consumatore qui e' il MODELLO, e nessun codice di Nexus rilegge questo
    /// prefisso per sapere com'e' andata.
    ///
    /// MUTAZIONE: togliere la chiamata a `tool_error_channel` da
    /// `to_wire_message` -> il messaggio tool torna nudo e il test rosseggia,
    /// che e' esattamente la cecita' che il modello avrebbe.
    #[test]
    fn un_tool_fallito_e_dichiarato_nel_testo_dove_manca_il_campo() {
        let corpo = |is_error: Option<bool>| {
            let mut req = sample_request();
            req.messages = vec![LlmMessage {
                role: "tool".to_string(),
                content: MessageContent::Text("nessun ascolto sulla porta 24806".to_string()),
                tool_call_id: Some("call_42".to_string()),
                tool_calls: None,
                name: None,
                thinking_signature: None,
                reasoning: None,
                is_error,
            }];
            let body = build_request_body(
                &req,
                false,
                &ResolvedReasoning::none(),
                PromptCacheKeying::ProviderManaged,
                None,
            );
            serde_json::to_value(body).expect("serializza")
        };

        let fallito = corpo(Some(true));
        assert_eq!(
            fallito["messages"][0]["content"],
            "[tool_error] nessun ascolto sulla porta 24806",
            "senza campo nel dialetto, la dichiarazione deve entrare nel testo: {fallito}"
        );

        // Un tool riuscito non riceve decorazioni, e un esito non dichiarato non
        // ne inventa uno: chi non sa, tace.
        assert_eq!(
            corpo(Some(false))["messages"][0]["content"],
            "nessun ascolto sulla porta 24806"
        );
        assert_eq!(
            corpo(None)["messages"][0]["content"],
            "nessun ascolto sulla porta 24806"
        );
    }

    fn msg(role: &str, testo: &str) -> LlmMessage {
        LlmMessage {
            role: role.to_string(),
            content: MessageContent::Text(testo.to_string()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: None,
            is_error: None,
        }
    }

    /// IL test del difetto: la chiave deve restare FERMA mentre la conversazione
    /// cresce, perche' quella e' l'unica forma in cui serve a qualcosa.
    ///
    /// Sul run misurato il prompt passava da 14.198 a 17.586 token con la testa
    /// immutata. Una chiave che si muovesse con la coda darebbe a ogni chiamata
    /// un gruppo tutto suo: varrebbe esattamente quanto non mandarla, che e' il
    /// difetto da cui si e' partiti.
    #[test]
    fn la_chiave_resta_ferma_mentre_la_conversazione_cresce() {
        let mut req = sample_request();
        req.messages.insert(0, msg("system", "istruzioni di progetto"));
        let prima = prompt_cache_key(&req);

        // Turni successivi: la coda cresce, la testa no.
        req.messages.push(msg("assistant", "ho letto il file"));
        req.messages.push(msg("user", "ora modificalo"));
        let dopo = prompt_cache_key(&req);

        assert_eq!(
            prima, dopo,
            "la chiave deve dipendere dalla sola parte stabile del prompt: \
             se si muove con la conversazione non raggruppa niente"
        );
    }

    /// Terzo caso del contratto, quello che mancava: il motore APPENDE al system
    /// direttive ricalcolate a ogni turno (focus del turno, razionale del piano)
    /// dietro il confine di [`nexus_types::system_prompt`]. La chiave non deve
    /// muoversi per quelle: se lo fa, ogni turno finisce in un gruppo diverso e
    /// il raggruppamento non serve a niente — che e' il difetto misurato il
    /// 29/07/2026 (mistral-medium: 171 chiamate, 3 con cache).
    #[test]
    fn la_chiave_resta_ferma_quando_cambia_solo_la_direttiva_di_turno() {
        use nexus_types::system_prompt::appendi_blocco_di_turno;

        let base = "istruzioni di progetto";
        let mut t1 = sample_request();
        t1.messages.insert(
            0,
            msg("system", &appendi_blocco_di_turno(base, "FOCUS: scrivi A")),
        );
        let mut t2 = sample_request();
        t2.messages.insert(
            0,
            msg("system", &appendi_blocco_di_turno(base, "FOCUS: correggi B")),
        );
        // Terzo turno: la direttiva sparisce del tutto (in un loop agentico
        // succede appena l'ultimo messaggio umano diventa un risultato di tool).
        let mut t3 = sample_request();
        t3.messages.insert(0, msg("system", base));

        let k1 = prompt_cache_key(&t1);
        assert_eq!(k1, prompt_cache_key(&t2), "direttive diverse, stesso gruppo");
        assert_eq!(
            k1,
            prompt_cache_key(&t3),
            "la direttiva che sparisce non deve cambiare gruppo"
        );
    }

    /// Il verso opposto: due prefissi diversi non devono finire nello stesso
    /// gruppo. Serve a tenere onesto il test qui sopra, che da solo passerebbe
    /// anche con una chiave costante.
    #[test]
    fn la_chiave_cambia_quando_cambia_la_parte_stabile() {
        let mut a = sample_request();
        a.messages.insert(0, msg("system", "istruzioni di progetto"));
        let mut b = sample_request();
        b.messages.insert(0, msg("system", "istruzioni DIVERSE"));
        assert_ne!(prompt_cache_key(&a), prompt_cache_key(&b));

        // Stesso prefisso, utenti diversi: gruppi separati.
        let mut c = a.clone();
        c.metadata.user_id = "altro-utente".to_string();
        assert_ne!(
            prompt_cache_key(&a),
            prompt_cache_key(&c),
            "utenti diversi non condividono il gruppo"
        );
    }

    /// La conseguenza sul wire, non il flag: verso un dialetto che la richiede
    /// la chiave deve comparire nel body serializzato.
    #[test]
    fn il_body_porta_la_chiave_solo_dove_serve() {
        let mut req = sample_request();
        req.messages.insert(0, msg("system", "istruzioni di progetto"));

        let con = serde_json::to_value(build_request_body(
            &req,
            false,
            &ResolvedReasoning::none(),
            PromptCacheKeying::RequiresKey,
            None,
        ))
        .expect("serializza");
        let chiave = con
            .get("prompt_cache_key")
            .and_then(|v| v.as_str())
            .expect("il dialetto che la richiede deve riceverla");
        assert_eq!(chiave.len(), 32, "etichetta corta e opaca");
        assert!(
            !chiave.contains("istruzioni"),
            "la chiave non deve trasportare il contenuto del prompt"
        );

        // Il default resta l'omissione: un campo sconosciuto verso un endpoint
        // che non lo documenta e' il solo verso che puo' fare danno.
        let senza = serde_json::to_value(build_request_body(
            &req,
            false,
            &ResolvedReasoning::none(),
            PromptCacheKeying::ProviderManaged,
            None,
        ))
        .expect("serializza");
        assert!(senza.get("prompt_cache_key").is_none());
        assert!(senza.get("session_id").is_none());

        // L'instradatore riceve la STESSA chiave in ENTRAMBI i campi, perche' i
        // lettori sono due: `session_id` fissa il fornitore dentro l'instradatore,
        // `prompt_cache_key` viene inoltrato a quel fornitore e ci fissa il server.
        // MISURATO (OpenRouter->xAI, 29/07/2026): col solo session_id la cache non
        // arrivava mai (128 token fissi su 4 chiamate a prefisso identico); con
        // prompt_cache_key 8704/8797 stabile dal secondo colpo. Ometterlo qui non
        // era prudenza: era il difetto.
        let sticky = serde_json::to_value(build_request_body(
            &req,
            false,
            &ResolvedReasoning::none(),
            PromptCacheKeying::RequiresSessionId,
            None,
        ))
        .expect("serializza");
        assert_eq!(sticky.get("session_id").and_then(|v| v.as_str()), Some(chiave));
        assert_eq!(
            sticky.get("prompt_cache_key").and_then(|v| v.as_str()),
            Some(chiave),
            "senza prompt_cache_key il fornitore a valle non riconosce la conversazione"
        );
        // Senza preferenza risolta il campo non compare: chi non ha una riga in
        // `nexus_router_upstream_affinity` deve partire come prima.
        assert!(
            sticky.get("provider").is_none(),
            "nessuna preferenza risolta: il campo va omesso, non inviato vuoto"
        );
    }

    /// Campo wire con cui un instradatore riceve la preferenza di fornitore.
    const CAMPO_PROVIDER: &str = "provider";
    /// L'instradatore su cui la preferenza e' stata misurata.
    const INSTRADATORE: &str = "openrouter";
    /// Il modello su cui l'intermittenza e' stata misurata (mig 0657).
    const MODELLO_MISURATO: &str = "qwen/qwen3-235b-a22b-2507";
    /// Il fornitore a valle che su quel modello serve il prefisso.
    const PREFERITO: &str = "Google";

    /// Terzo livello di affinita': la richiesta dichiara QUALE fornitore a valle.
    ///
    /// Il difetto che copre e' esattamente cio' che i due campi sopra NON
    /// bastavano a chiudere. MISURATO il 29/07/2026 contro l'API OpenRouter, 8
    /// chiamate consecutive a prefisso identico CON `session_id` e
    /// `prompt_cache_key` regolarmente inviati: `qwen/qwen3-235b-a22b-2507`
    /// rimbalzava fra DeepInfra, Alibaba e Novita con 0/8 di cache; fissata la
    /// preferenza su Google, 6/6 al 99%. `session_id` il fornitore non lo fissa.
    #[test]
    fn il_fornitore_a_valle_si_dichiara_solo_sugli_instradatori() {
        let req = sample_request();
        let ordine = vec![PREFERITO.to_string(), "DeepInfra".to_string()];

        let instradatore = serde_json::to_value(build_request_body(
            &req,
            false,
            &ResolvedReasoning::none(),
            PromptCacheKeying::RequiresSessionId,
            Some(&ordine),
        ))
        .expect("serializza");
        let p = instradatore
            .get(CAMPO_PROVIDER)
            .expect("l'instradatore deve ricevere la preferenza di fornitore");
        assert_eq!(
            p.get("order").and_then(|v| v.as_array()).map(|a| a.len()),
            Some(2),
            "l'ordine va inoltrato intero: il secondo nome e' il ripiego preferito"
        );
        assert_eq!(p["order"][0].as_str(), Some(PREFERITO));
        // PREFERENZA, non vincolo. Misurato che l'ordine tiene fermo il fornitore
        // anche coi ripieghi attivi (8/8 su entrambi i modelli provati), quindi
        // spegnerli costerebbe la resilienza senza comprare nulla: con `false`,
        // un fornitore giu' fa fallire la richiesta invece di costare il solo
        // riuso del prefisso.
        assert_eq!(
            p.get("allow_fallbacks").and_then(|v| v.as_bool()),
            Some(true),
            "il ripiego resta attivo: perdere il prefisso costa meno che perdere la chiamata"
        );

        // Su un provider diretto la domanda non esiste: non c'e' nessun fornitore
        // a valle da scegliere, e il campo sarebbe sconosciuto al dialetto.
        for keying in [
            PromptCacheKeying::ProviderManaged,
            PromptCacheKeying::RequiresKey,
        ] {
            let diretto = serde_json::to_value(build_request_body(
                &req,
                false,
                &ResolvedReasoning::none(),
                keying,
                Some(&ordine),
            ))
            .expect("serializza");
            assert!(
                diretto.get(CAMPO_PROVIDER).is_none(),
                "{keying:?} non instrada verso terzi: il campo non deve partire"
            );
        }

        // Un ordine vuoto e' "nessuna preferenza", non "preferisci niente": un
        // `order: []` inviato a OpenRouter e' un vincolo che nessun fornitore
        // soddisfa.
        let vuoto = serde_json::to_value(build_request_body(
            &req,
            false,
            &ResolvedReasoning::none(),
            PromptCacheKeying::RequiresSessionId,
            Some(&[]),
        ))
        .expect("serializza");
        assert!(
            vuoto.get(CAMPO_PROVIDER).is_none(),
            "ordine vuoto = campo omesso"
        );
    }

    /// La GIUNZIONE, non `build_request_body`: il corpo lo costruisce il client
    /// dal dialetto che ha in mano, e i due percorsi di produzione (`complete`,
    /// `stream`) passano solo da li'.
    ///
    /// Perche' serve, dato che il test sopra guarda gli stessi campi: quello
    /// passa `cache_keying` a mano, quindi prova che la funzione lo onora, non
    /// che qualcuno glielo dia. Revocando il fix nel client (`self.cache_keying`
    /// -> `PromptCacheKeying::ProviderManaged`) quel test resta verde e la
    /// richiesta parte senza chiave — che e' esattamente il difetto da cui il
    /// modulo nasce (regola O).
    #[tokio::test]
    async fn il_client_dichiara_il_proprio_dialetto_nel_corpo() {
        let apri = |nome: &str, keying| {
            OpenAiCompatClient::new(Client::new(), "https://esempio.invalid/v1", "chiave", nome)
                .with_prompt_cache_keying(keying)
        };
        let mut req = sample_request();
        req.messages.insert(0, msg("system", "istruzioni di progetto"));

        // Instradatore: entrambi i campi, perche' i lettori sono due.
        let instradatore = apri(INSTRADATORE, PromptCacheKeying::RequiresSessionId);
        let corpo = serde_json::to_value(
            instradatore
                .corpo_della_richiesta(&req, false, &ResolvedReasoning::none())
                .await,
        )
        .expect("serializza");
        let chiave = corpo
            .get("prompt_cache_key")
            .and_then(|v| v.as_str())
            .expect("l'instradatore deve ricevere la chiave di raggruppamento");
        assert_eq!(
            corpo.get("session_id").and_then(|v| v.as_str()),
            Some(chiave)
        );

        // Lo stream e' l'altro percorso di produzione, e passa dalla stessa
        // giunzione: se un giorno divergesse, il campo sparirebbe da meta' delle
        // chiamate senza che nulla lo dica.
        let in_stream = serde_json::to_value(
            instradatore
                .corpo_della_richiesta(&req, true, &ResolvedReasoning::none())
                .await,
        )
        .expect("serializza");
        assert_eq!(
            in_stream.get("prompt_cache_key").and_then(|v| v.as_str()),
            Some(chiave),
            "lo stream deve dichiarare lo stesso gruppo del non-streaming"
        );
        assert_eq!(in_stream.get("stream").and_then(|v| v.as_bool()), Some(true));

        // Provider a cache automatica: nessun campo, come oggi.
        let diretto = apri("mistral", PromptCacheKeying::ProviderManaged);
        let senza = serde_json::to_value(
            diretto
                .corpo_della_richiesta(&req, false, &ResolvedReasoning::none())
                .await,
        )
        .expect("serializza");
        assert!(senza.get("prompt_cache_key").is_none());
        assert!(senza.get("session_id").is_none());
    }

    /// Il criterio sta nell'enum, non sparso nei call site (regola L): questo
    /// test guarda il punto unico, cosi' un nuovo instradatore aggiunto a
    /// `cache_keying_per_endpoint` eredita il livello senza altre modifiche.
    #[test]
    fn solo_gli_instradatori_dichiarano_il_fornitore_a_valle() {
        assert!(PromptCacheKeying::RequiresSessionId.requires_upstream_pinning());
        assert!(!PromptCacheKeying::RequiresKey.requires_upstream_pinning());
        assert!(!PromptCacheKeying::ProviderManaged.requires_upstream_pinning());
    }

    /// La preferenza arriva dal DB REALE, sulla migrazione reale (regola O).
    ///
    /// I test qui sopra passano l'ordine a mano: provano che il body lo porta,
    /// non che qualcuno lo sappia leggere. Il difetto da cui nasce questo modulo
    /// era esattamente una richiesta che partiva senza il campo che credevamo di
    /// mandare, quindi la catena va attraversata dove sta davvero il valore —
    /// la riga di `nexus_router_upstream_affinity` (mig 0657).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_preferenza_arriva_dal_db(pool: sqlx::PgPool) {
        // Costruito qui e non a mano ogni volta: e' lo stesso endpoint, e tre
        // copie della stessa configurazione si sarebbero potute scostare senza
        // che il test se ne accorgesse.
        let apri = |nome: &str, keying| {
            OpenAiCompatClient::new(Client::new(), "https://esempio.invalid/v1", "chiave", nome)
                .with_prompt_cache_keying(keying)
                .with_db(Some(pool.clone()))
        };
        let client = apri(INSTRADATORE, PromptCacheKeying::RequiresSessionId);

        // Un modello senza riga non ha preferenza: deve partire come prima, non
        // con un ordine inventato. `x-ai/grok-4.5` e' il caso reale — un solo
        // fornitore a valle, niente da scegliere — e infatti la 0657 non lo
        // elenca.
        assert_eq!(client.upstream_order_for("x-ai/grok-4.5").await, None);

        // La preferenza MISURATA arriva dalla migrazione, non da un inserimento
        // di comodo: se qualcuno cambia la 0657, questo test se ne accorge.
        let ordine = client
            .upstream_order_for(MODELLO_MISURATO)
            .await
            .expect("la 0657 elenca questo modello: la preferenza va letta");
        assert_eq!(
            ordine,
            vec![PREFERITO.to_string()],
            "misurato: solo Google serve il prefisso su questo modello"
        );

        // CSV multi-valore, con lo spazio che un umano scrive dopo la virgola:
        // non deve entrare nel nome del fornitore, o l'ordine non lo soddisfa
        // nessuno.
        sqlx::query(
            "INSERT INTO nexus_router_upstream_affinity \
             (provider, model_id, upstream_order, nota) VALUES ($1, $2, $3, $4)",
        )
        .bind(INSTRADATORE)
        .bind("prova/modello-a-due-fornitori")
        .bind("Google, DeepInfra")
        .bind("riga di prova")
        .execute(&pool)
        .await
        .expect("inserisce la preferenza");
        assert_eq!(
            client
                .upstream_order_for("prova/modello-a-due-fornitori")
                .await,
            Some(vec![PREFERITO.to_string(), "DeepInfra".to_string()])
        );

        // E arriva fino al body, che e' cio' che il fornitore legge davvero.
        // Dalla GIUNZIONE, non da `build_request_body`: quest'ultima vuole
        // l'ordine come argomento, e passarglielo a mano proverebbe soltanto che
        // lo scrive nel campo — cioe' l'unica parte che non era in dubbio. Il
        // difetto da cui nasce il modulo era una richiesta che partiva SENZA il
        // campo che credevamo di mandare, quindi il body deve nascere dove nasce
        // in produzione: il client lo legge dal DB da se' (regola O).
        let mut req = sample_request();
        req.model = MODELLO_MISURATO.to_string();
        let body = serde_json::to_value(
            client
                .corpo_della_richiesta(&req, false, &ResolvedReasoning::none())
                .await,
        )
        .expect("serializza");
        assert_eq!(
            body[CAMPO_PROVIDER]["order"][0].as_str(),
            Some(PREFERITO),
            "la preferenza letta dal DB non e' arrivata nel corpo: {body}"
        );

        // Una riga disattivata non e' una preferenza: serve a togliere un
        // fornitore diventato cattivo senza cancellare la misura che lo diceva.
        sqlx::query("UPDATE nexus_router_upstream_affinity SET is_active = false")
            .execute(&pool)
            .await
            .expect("disattiva");
        // La cache TTL tiene ancora il valore vecchio: e' un client nuovo a dover
        // vedere lo stato nuovo, non questo (60s, come le altre cache).
        let fresco = apri(INSTRADATORE, PromptCacheKeying::RequiresSessionId);
        assert_eq!(
            fresco.upstream_order_for(MODELLO_MISURATO).await,
            None,
            "riga disattivata: nessuna preferenza"
        );

        // Su un provider diretto la domanda non si pone nemmeno: stessa riga,
        // ma il livello non esiste.
        let diretto = apri("mistral", PromptCacheKeying::RequiresKey);
        assert_eq!(diretto.upstream_order_for(MODELLO_MISURATO).await, None);
    }

    /// Il modello su cui perdere la preferenza NON costa un riuso mancato: la
    /// riga della 0657 esiste per ESCLUDERE un fornitore che sullo stesso
    /// prefisso fattura il prompt il doppio (20.011 token contro 10.162).
    const MODELLO_SOVRAFATTURATO: &str = "minimax/minimax-m2";

    /// Un DB che non risponde non e' "nessuna preferenza", e non lo diventa per
    /// i 60s successivi.
    ///
    /// L'errore lo produce il database VERO: la tabella viene resa invisibile con
    /// un rename, non ricreata da uno schema ricopiato nel test — quella copia
    /// divergerebbe dalla 0657 e il test misurerebbe se stesso (regola O). Il
    /// rename e' anche l'unico modo di riportare indietro la tabella con i suoi
    /// dati, che e' cio' che serve per provare il ritento.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn un_errore_del_db_non_diventa_nessuna_preferenza(pool: sqlx::PgPool) {
        let client =
            OpenAiCompatClient::new(Client::new(), "https://esempio.invalid/v1", "chiave", INSTRADATORE)
                .with_prompt_cache_keying(PromptCacheKeying::RequiresSessionId)
                .with_db(Some(pool.clone()));
        let sposta = |da: &'static str, a: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query(&format!("ALTER TABLE {da} RENAME TO {a}"))
                    .execute(&pool)
                    .await
                    .expect("rinomina la tabella");
            }
        };
        const NASCOSTA: &str = "affinita_nascosta_dal_test";
        const VERA: &str = "nexus_router_upstream_affinity";

        // (1) Cio' che si e' letto quando il DB parlava.
        assert_eq!(
            client.upstream_order_for(MODELLO_SOVRAFATTURATO).await,
            Some(vec![PREFERITO.to_string(), "Minimax".to_string()])
        );

        // (2) Col DB muto, l'ultimo valore letto continua a partire: e' il
        // pattern che la regola G prescrive (la cache tiene l'ultimo valore
        // valido, il refresh fallito e' un WARN). L'invalidazione della casella
        // TTL e' cio' che il tempo farebbe da se': `get` di una entry scaduta
        // ritorna `None` esattamente come dopo un `invalidate`.
        sposta(VERA, NASCOSTA).await;
        client.upstream_order.invalidate(MODELLO_SOVRAFATTURATO);
        assert_eq!(
            client.upstream_order_for(MODELLO_SOVRAFATTURATO).await,
            Some(vec![PREFERITO.to_string(), "Minimax".to_string()]),
            "il DB non ha risposto: la preferenza gia' letta non va buttata, o la \
             chiamata atterra sul fornitore che fattura il doppio"
        );

        // (3) Su un modello mai letto non c'e' nulla da conservare, e l'errore
        // non autorizza a inventare un ordine.
        assert_eq!(client.upstream_order_for(MODELLO_MISURATO).await, None);

        // (4) E non si e' cristallizzato: appena il DB torna, la chiamata dopo
        // ritenta. Col vecchio `unwrap_or(None)` l'errore finiva in cache come
        // "nessuna riga" e questo assert cadeva, perche' la preferenza restava
        // perduta per tutto il TTL senza che nulla lo dicesse.
        sposta(NASCOSTA, VERA).await;
        assert_eq!(
            client.upstream_order_for(MODELLO_MISURATO).await,
            Some(vec![PREFERITO.to_string()]),
            "l'errore e' stato scritto in cache: la preferenza resta perduta per 60s"
        );
    }

    /// La causa si legge dallo SQLSTATE, non dal messaggio dell'errore (regola
    /// M): il caso peggiore — la 0657 non applicata sul DB di destinazione, in
    /// cui la funzionalita' e' inerte al 100% — deve distinguersi da "il DB non
    /// risponde" e da "nessuna preferenza configurata".
    #[test]
    fn la_causa_arriva_dal_codice_strutturato() {
        assert!(causa_preferenza_illeggibile(Some(SQLSTATE_TABELLA_ASSENTE)).contains("0657"));
        assert!(!causa_preferenza_illeggibile(Some("53300")).contains("0657"));
        assert!(causa_preferenza_illeggibile(None).contains("raggiungibile"));
    }

    #[test]
    fn parse_models_estrae_id_ordina_e_deduplica() {
        // Forma canonica della risposta `GET /models` (OpenAI/Mistral/DeepSeek/vLLM).
        let body = serde_json::json!({
            "object": "list",
            "data": [
                { "id": "gpt-4o", "object": "model" },
                { "id": "gpt-4o-mini", "object": "model" },
                { "id": "gpt-4o", "object": "model" }, // duplicato
            ]
        });
        let models = parse_models_response(&body);
        // Ordinato e deduplicato.
        assert_eq!(models, vec!["gpt-4o", "gpt-4o-mini"]);
    }

    /// Body VERBATIM catturato dai log del gateway il 26/07 (13:53 UTC), quando
    /// il credito Anthropic si e' esaurito nel mezzo di un run.
    const BODY_CREDITO_ANTHROPIC: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing to upgrade or purchase credits."}}"#;

    /// Errore Anthropic di formato VERO: stesso status, stesso codice, messaggio
    /// che non parla di credito. Il discrimine e' solo questo.
    const BODY_FORMATO_ANTHROPIC: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages.1: Expected `thinking` or `redacted_thinking`, but found `text`"}}"#;

    /// Il candidato che il quirk emette, quando lo emette. E' cio' che QUESTO
    /// modulo produce: che poi valga `Billing` lo dice il catalogo dei codici, e
    /// ha il suo test contro la migrazione vera
    /// (`tassonomia_errori::il_quirk_emette_un_valore_che_il_catalogo_dichiara`).
    fn quirk_emesso(provider: &str, status: u16, body: &str) -> Option<String> {
        ProviderHttpError::from_response(provider, status, body.to_string())
            .candidati
            .iter()
            .find(|c| c.campo == crate::tassonomia_errori::CampoErrore::QuirkFornitore)
            .map(|c| c.valore.clone())
    }

    #[test]
    fn credito_anthropic_esaurito_emette_il_candidato_sintetico() {
        // Attraversa il PRODUTTORE (from_response), non un codice scritto a mano:
        // costruire l'errore a mano fisserebbe l'assunto da verificare (regola O).
        assert_eq!(
            quirk_emesso("anthropic", 400, BODY_CREDITO_ANTHROPIC).as_deref(),
            Some(CODICE_BILLING_NORMALIZZATO),
            "senza il candidato sintetico nessun catalogo puo' vedere quel credito: \
             anthropic lo dichiara con lo STESSO identificatore di una richiesta \
             malformata"
        );
        // Il codice sul WIRE resta quello storico: i consumatori a valle lo
        // confrontano per uguaglianza.
        let err = ProviderHttpError::from_response(
            "anthropic",
            400,
            BODY_CREDITO_ANTHROPIC.to_string(),
        );
        assert_eq!(err.code.as_deref(), Some(CODICE_BILLING_NORMALIZZATO));
    }

    #[test]
    fn un_400_di_formato_anthropic_non_emette_nessun_quirk() {
        // La traduzione non deve inghiottire i 400 legittimi: stesso provider,
        // stesso status, stesso codice, messaggio diverso -> nessun sintetico, e
        // il codice resta quello del fornitore.
        assert_eq!(quirk_emesso("anthropic", 400, BODY_FORMATO_ANTHROPIC), None);
        let err = ProviderHttpError::from_response(
            "anthropic",
            400,
            BODY_FORMATO_ANTHROPIC.to_string(),
        );
        assert_eq!(err.code.as_deref(), Some("invalid_request_error"));
    }

    #[test]
    fn lo_stesso_messaggio_da_un_altro_provider_non_emette_il_quirk() {
        // Il quirk resta isolato al provider che ha l'ambiguita': un altro
        // provider non viene reinterpretato qui.
        assert_eq!(quirk_emesso("deepseek", 400, BODY_CREDITO_ANTHROPIC), None);
    }

    #[test]
    fn parse_models_meta_estrae_finestra_dichiarata() {
        // Dialetto Mistral: `max_context_length` in data[]. OpenAI/DeepSeek non
        // hanno il campo -> None (finestra IGNOTA, mai inventata: regola H).
        let body = serde_json::json!({
            "object": "list",
            "data": [
                { "id": "mistral-medium-3", "max_context_length": 131072 },
                { "id": "mistral-ocr-latest" },                     // senza campo
                { "id": "mistral-rotto", "max_context_length": 0 }, // non positivo
            ]
        });
        let metas = parse_models_meta_response(&body);
        assert_eq!(metas.len(), 3);
        assert_eq!(metas[0].id, "mistral-medium-3");
        assert_eq!(metas[0].context_window, Some(131072));
        assert_eq!(metas[1].id, "mistral-ocr-latest");
        assert_eq!(metas[1].context_window, None);
        // Valore non positivo = non dichiarato (mai una finestra inventata).
        assert_eq!(metas[2].context_window, None);
    }

    #[test]
    fn parse_models_salta_id_assenti_o_vuoti_e_gestisce_data_mancante() {
        let body = serde_json::json!({
            "data": [
                { "id": "deepseek-chat" },
                { "object": "model" },          // niente id
                { "id": "" },                    // id vuoto
                { "id": "  mistral-small  " },   // trimmato
            ]
        });
        let models = parse_models_response(&body);
        assert_eq!(models, vec!["deepseek-chat", "mistral-small"]);

        // Risposta senza `data`: lista vuota, non panico.
        let vuoto = serde_json::json!({ "object": "list" });
        assert!(parse_models_response(&vuoto).is_empty());
    }

    #[test]
    fn request_body_serializza_campi_base() {
        let req = sample_request();
        let body = build_request_body(
            &req,
            false,
            &ResolvedReasoning::none(),
            PromptCacheKeying::ProviderManaged,
            None,
        );
        let json = serde_json::to_value(&body).unwrap();

        assert_eq!(json["model"], "test-model");
        assert_eq!(json["temperature"], 0.5);
        assert_eq!(json["max_tokens"], 256);
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "ciao");
        // stream non richiesto: campo assente.
        assert!(json.get("stream").is_none());
        assert!(json.get("stream_options").is_none());
        // Dialetto base: nessun campo reasoning.
        assert!(json.get("max_completion_tokens").is_none());
        assert!(json.get("reasoning_effort").is_none());
        assert!(json.get("thinking").is_none());
    }

    /// Round-trip reasoning_content (DeepSeek): un assistant message con
    /// `reasoning=Some(...)` DEVE comparire come `messages[i].reasoning_content`
    /// nel body SOLO per il dialetto DeepSeek (vincolo HTTP 400). Per i dialetti
    /// non-DeepSeek il campo NON deve viaggiare (assente).
    #[test]
    fn reasoning_content_round_trip_solo_deepseek() {
        // Richiesta con un assistant in thinking mode (porta reasoning) seguito da
        // un turno user: speculare a una history agentica multi-turno DeepSeek.
        let mut req = sample_request();
        req.messages = vec![
            LlmMessage {
                role: "assistant".to_string(),
                content: MessageContent::Text("rispondo".to_string()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
                reasoning: Some("ho ragionato cosi'".to_string()),
                is_error: None,
            },
            LlmMessage {
                role: "user".to_string(),
                content: MessageContent::Text("continua".to_string()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
                // L'utente non porta reasoning: non deve mai comparire reasoning_content.
                reasoning: Some("spurio-da-ignorare".to_string()),
                is_error: None,
            },
        ];

        // Dialetto DeepSeek: il reasoning dell'assistant e' ri-passato.
        let deepseek = ResolvedReasoning {
            dialect: ReasoningDialect::DeepSeek,
            enabled: true,
            effort: None,
        };
        let body = build_request_body(&req, false, &deepseek, PromptCacheKeying::ProviderManaged, None);
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(
            json["messages"][0]["reasoning_content"], "ho ragionato cosi'",
            "l'assistant DeepSeek deve ri-passare il reasoning_content"
        );
        // Lo user NON deve portare reasoning_content (solo i ruoli assistant).
        assert!(
            json["messages"][1].get("reasoning_content").is_none(),
            "lo user non deve mai esporre reasoning_content"
        );

        // Dialetto non-DeepSeek (base): il campo non viaggia mai.
        let body_base = build_request_body(
            &req,
            false,
            &ResolvedReasoning::none(),
            PromptCacheKeying::ProviderManaged,
            None,
        );
        let json_base = serde_json::to_value(&body_base).unwrap();
        assert!(
            json_base["messages"][0].get("reasoning_content").is_none(),
            "fuori dal dialetto DeepSeek il reasoning_content non deve essere inviato"
        );
    }

    fn edit_tool() -> crate::types::LlmToolDefinition {
        crate::types::LlmToolDefinition {
            kind: "function".to_string(),
            function: crate::types::ToolFunctionDef {
                name: "edit_file".to_string(),
                description: Some("modifica un file".to_string()),
                parameters: serde_json::json!({"type": "object"}),
                strict: None,
            },
        }
    }

    #[test]
    fn tool_choice_required_passthrough_nativo_openai() {
        // Con tools presenti il vincolo "required" e' inoltrato tale e quale
        // (dialetto OpenAI nativo): e' questo che FORZA il modello a chiamare il
        // tool invece di descrivere (fix del bug tool_choice droppato).
        let mut req = sample_request();
        req.tools = Some(vec![edit_tool()]);
        req.tool_choice = Some(serde_json::json!("required"));
        let json = serde_json::to_value(build_request_body(
            &req,
            false,
            &ResolvedReasoning::none(),
            PromptCacheKeying::ProviderManaged,
            None,
        ))
            .unwrap();
        assert_eq!(json["tool_choice"], "required");
        // Oggetto funzione: passthrough nella forma OpenAI canonica.
        req.tool_choice = Some(serde_json::json!({"type": "function", "function": {"name": "edit_file"}}));
        let json = serde_json::to_value(build_request_body(
            &req,
            false,
            &ResolvedReasoning::none(),
            PromptCacheKeying::ProviderManaged,
            None,
        ))
            .unwrap();
        assert_eq!(json["tool_choice"]["type"], "function");
        assert_eq!(json["tool_choice"]["function"]["name"], "edit_file");
    }

    #[test]
    fn tool_choice_omesso_senza_tools() {
        // tool_choice senza tools non ha senso: il campo non viene inviato.
        let mut req = sample_request();
        req.tools = None;
        req.tool_choice = Some(serde_json::json!("required"));
        let json = serde_json::to_value(build_request_body(
            &req,
            false,
            &ResolvedReasoning::none(),
            PromptCacheKeying::ProviderManaged,
            None,
        ))
            .unwrap();
        assert!(json.get("tool_choice").is_none());
        // Senza tool_choice (caso storico): campo assente.
        let mut req2 = sample_request();
        req2.tools = Some(vec![edit_tool()]);
        let json2 = serde_json::to_value(build_request_body(
            &req2,
            false,
            &ResolvedReasoning::none(),
            PromptCacheKeying::ProviderManaged,
            None,
        ))
            .unwrap();
        assert!(json2.get("tool_choice").is_none());
    }

    #[test]
    fn request_body_streaming_aggiunge_include_usage() {
        let req = sample_request();
        let body = build_request_body(
            &req,
            true,
            &ResolvedReasoning::none(),
            PromptCacheKeying::ProviderManaged,
            None,
        );
        let json = serde_json::to_value(&body).unwrap();

        assert_eq!(json["stream"], true);
        assert_eq!(json["stream_options"]["include_usage"], true);
    }

    // --- Dialetti reasoning (passo 2) --------------------------------------

    #[test]
    fn dialetto_openai_reasoning_usa_max_completion_tokens() {
        let req = sample_request();
        let reasoning = ResolvedReasoning {
            dialect: ReasoningDialect::OpenAiReasoning,
            enabled: true,
            effort: Some("high".to_string()),
        };
        let json =
            serde_json::to_value(build_request_body(&req, false, &reasoning, PromptCacheKeying::ProviderManaged, None)).unwrap();

        // max_tokens -> max_completion_tokens; temperatura omessa; effort inviato.
        assert!(json.get("max_tokens").is_none());
        assert_eq!(json["max_completion_tokens"], 256);
        assert!(json.get("temperature").is_none());
        assert_eq!(json["reasoning_effort"], "high");
    }

    #[test]
    fn dialetto_openai_reasoning_senza_effort_non_lo_invia() {
        let req = sample_request();
        let reasoning = ResolvedReasoning {
            dialect: ReasoningDialect::OpenAiReasoning,
            enabled: true,
            effort: None,
        };
        let json =
            serde_json::to_value(build_request_body(&req, false, &reasoning, PromptCacheKeying::ProviderManaged, None)).unwrap();
        assert_eq!(json["max_completion_tokens"], 256);
        // Nessun effort configurato: il campo non c'e' (default del modello).
        assert!(json.get("reasoning_effort").is_none());
    }

    #[test]
    fn dialetto_deepseek_enabled_aggiunge_thinking_appiattito() {
        let req = sample_request();
        let reasoning = ResolvedReasoning {
            dialect: ReasoningDialect::DeepSeek,
            enabled: true,
            effort: None,
        };
        let json =
            serde_json::to_value(build_request_body(&req, false, &reasoning, PromptCacheKeying::ProviderManaged, None)).unwrap();

        // extra_body appiattito nel body radice: thinking.type=enabled.
        assert_eq!(json["thinking"]["type"], "enabled");
        // max_tokens standard (DeepSeek non e' o-series).
        assert_eq!(json["max_tokens"], 256);
        assert!(json.get("max_completion_tokens").is_none());
    }

    #[test]
    fn dialetto_deepseek_disabled_aggiunge_thinking_disabled() {
        let req = sample_request();
        let reasoning = ResolvedReasoning {
            dialect: ReasoningDialect::DeepSeek,
            enabled: false,
            effort: None,
        };
        let json =
            serde_json::to_value(build_request_body(&req, false, &reasoning, PromptCacheKeying::ProviderManaged, None)).unwrap();
        assert_eq!(json["thinking"]["type"], "disabled");
    }

    #[test]
    fn deserializza_reasoning_content_deepseek() {
        let raw = r#"{
            "choices": [{
                "message": {"content": "risposta", "reasoning_content": "ho riflettuto"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "prompt_cache_hit_tokens": 4}
        }"#;
        let parsed: ChatCompletion = serde_json::from_str(raw).unwrap();
        let resp = from_chat_completion(parsed, "m".to_string(), "deepseek", 1).unwrap();

        assert_eq!(resp.content, "risposta");
        assert_eq!(resp.reasoning.as_deref(), Some("ho riflettuto"));
        // Cache hit DeepSeek normalizzato.
        assert_eq!(resp.usage.cache_read_tokens, Some(4));
    }

    #[test]
    fn deserializza_cache_openai_prompt_tokens_details() {
        let raw = r#"{
            "choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 20, "completion_tokens": 3, "prompt_tokens_details": {"cached_tokens": 12}}
        }"#;
        let parsed: ChatCompletion = serde_json::from_str(raw).unwrap();
        let resp = from_chat_completion(parsed, "m".to_string(), "openai", 1).unwrap();
        assert_eq!(resp.usage.cache_read_tokens, Some(12));
    }

    /// La cache di Moonshot/Kimi si legge in entrambe le forme che l'API usa, e
    /// i token letti restano DENTRO il prompt.
    ///
    /// La seconda meta' non e' un'assunzione: la documentazione non lo dice da
    /// nessuna parte, ed e' stata MISURATA il 09/08/2026 su `kimi-k2.6` con tre
    /// chiamate consecutive a prefisso identico. `prompt_tokens` e' rimasto 4267
    /// in tutti e tre i giri mentre `cached_tokens` passava da assente a 4096, e
    /// `prompt + completion == total` sempre: se i token serviti da cache fossero
    /// FUORI dal prompt, al secondo giro il prompt sarebbe sceso a 171. Da qui
    /// [`PromptCacheReporting::CachedIncludedInPrompt`]; con la dichiarazione
    /// opposta il sistema conterebbe il prefisso due volte.
    ///
    /// I due blocchi `usage` sono le due forme reali osservate: la prima e' lo
    /// schema che la doc dichiara, la seconda quella che l'API emette oggi (con
    /// entrambi i campi, allo stesso valore).
    ///
    /// MUTAZIONE DI CONTROLLO: togliendo il campo `cached_tokens` da
    /// [`WireUsage`] rosseggia il primo caso; togliendo il ramo
    /// `prompt_tokens_details` rosseggia il gemello gia' presente per OpenAI.
    #[test]
    fn deserializza_cache_kimi_in_entrambe_le_forme() {
        // Forma dichiarata dallo schema ufficiale: solo il primo livello.
        let raw = r#"{
            "choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 19, "completion_tokens": 21, "total_tokens": 40, "cached_tokens": 10}
        }"#;
        let parsed: ChatCompletion = serde_json::from_str(raw).unwrap();
        let resp = from_chat_completion(parsed, "kimi-k3".to_string(), "kimi", 1).unwrap();
        assert_eq!(resp.usage.cache_read_tokens, Some(10));
        // Il prompt resta il LORDO dichiarato dal wire: sommare qui i cached li
        // conterebbe due volte, come gia' fissato per gli altri dialetti.
        assert_eq!(resp.usage.input_tokens, 19);

        // Forma emessa oggi dall'API, verbatim dalla misura: entrambi i campi.
        let reale = r#"{
            "choices": [{"message": {"content": "ok", "reasoning_content": "..."}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 4267, "completion_tokens": 16, "total_tokens": 4283,
                      "cached_tokens": 4096,
                      "completion_tokens_details": {"reasoning_tokens": 15},
                      "prompt_tokens_details": {"cached_tokens": 4096}}
        }"#;
        let parsed: ChatCompletion = serde_json::from_str(reale).unwrap();
        let resp = from_chat_completion(parsed, "kimi-k2.6".to_string(), "kimi", 1).unwrap();
        assert_eq!(resp.usage.cache_read_tokens, Some(4096));
        assert_eq!(resp.usage.input_tokens, 4267, "il prompt lordo non cambia sui cache hit");
    }

    /// La RELAZIONE fra i conteggi, non solo la loro presenza.
    ///
    /// I dialetti OpenAI-compatibili contano i cache hit DENTRO `prompt_tokens`,
    /// che e' quindi gia' il LORDO: `LlmUsage.input_tokens` deve uscire IDENTICO
    /// al wire. Sommare qui i token di cache — la normalizzazione dell'altro
    /// verso, quella di Anthropic — li conterebbe due volte, gonfiando il
    /// contesto misurato e il monte da cui il listino scorpora. Nessun test
    /// fissava questa premessa: si poteva invertire il verso e restare verdi.
    #[test]
    fn input_tokens_resta_il_lordo_nei_dialetti_openai_compat() {
        // Forma reale DeepSeek.
        let raw = r#"{
            "choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5,
                      "prompt_cache_hit_tokens": 4, "prompt_cache_miss_tokens": 6}
        }"#;
        let parsed: ChatCompletion = serde_json::from_str(raw).unwrap();
        let u = from_chat_completion(parsed, "m".to_string(), "deepseek", 1)
            .unwrap()
            .usage;
        assert_eq!(u.cache_read_tokens, Some(4));
        assert_eq!(u.input_tokens, 10, "il wire e' gia' lordo: nessuna somma");
        assert_eq!(u.cache_creation_tokens, None);

        // Forma reale OpenAI.
        let raw = r#"{
            "choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 20, "completion_tokens": 3,
                      "prompt_tokens_details": {"cached_tokens": 12}}
        }"#;
        let parsed: ChatCompletion = serde_json::from_str(raw).unwrap();
        let u = from_chat_completion(parsed, "m".to_string(), "openai", 1)
            .unwrap()
            .usage;
        assert_eq!(u.input_tokens, 20, "il wire e' gia' lordo: nessuna somma");
        assert_eq!(u.cache_read_tokens, Some(12));

        // Senza cache nulla cambia (nessuna regressione).
        let raw = r#"{
            "choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 20, "completion_tokens": 3}
        }"#;
        let parsed: ChatCompletion = serde_json::from_str(raw).unwrap();
        let u = from_chat_completion(parsed, "m".to_string(), "openai", 1)
            .unwrap()
            .usage;
        assert_eq!(u.input_tokens, 20);
        assert_eq!(u.cache_read_tokens, None);
    }

    /// Lo streaming e' un secondo percorso di produzione dello stesso usage: se
    /// normalizza diversamente, la stessa chiamata dichiara un contesto diverso
    /// e costa cifre diverse a seconda che sia stata servita in streaming o no.
    #[test]
    fn anche_lo_streaming_lascia_il_prompt_lordo() {
        let raw = r#"{
            "choices": [{"delta": {"content": ""}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 7,
                      "prompt_tokens_details": {"cached_tokens": 90}}
        }"#;
        let parsed: ChatCompletionChunk = serde_json::from_str(raw).unwrap();
        let chunk = chunk_from_sse(parsed, "openai", "m").expect("chunk con usage");
        let u = chunk.usage.expect("usage presente nel chunk finale");
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.cache_read_tokens, Some(90));
    }

    // ── Content tollerante (contratto Mistral: string | ContentChunk[]) ─────

    #[test]
    fn content_array_di_chunk_estrae_il_testo() {
        // Mistral puo' rispondere content come array di chunk: i `text` vanno
        // concatenati, reference/thinking ignorati. Prima falliva l'INTERO
        // parse -> "error decoding response body" (18 occorrenze in history il
        // 2026-07-06) ritentato a vuoto come transitorio.
        let raw = r#"{
            "choices": [{
                "message": {"content": [
                    {"type": "text", "text": "Ciao "},
                    {"type": "reference", "reference_ids": [1]},
                    {"type": "text", "text": "mondo"}
                ]},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        }"#;
        let parsed = parse_chat_completion("mistral", raw).expect("parse tollerante");
        let resp = from_chat_completion(parsed, "m".to_string(), "mistral", 1).unwrap();
        assert_eq!(resp.content, "Ciao mondo");
    }

    #[test]
    fn content_stringa_resta_invariato() {
        let raw = r#"{
            "choices": [{"message": {"content": "semplice"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        }"#;
        let parsed = parse_chat_completion("mistral", raw).expect("parse stringa");
        let resp = from_chat_completion(parsed, "m".to_string(), "mistral", 1).unwrap();
        assert_eq!(resp.content, "semplice");
    }

    #[test]
    fn sse_delta_content_array_non_scartato() {
        // Delta streaming con content array: prima il parser SSE scartava la
        // riga in silenzio (risposta troncata); ora estrae il testo.
        let raw = r#"{
            "choices": [{"delta": {"content": [{"type": "text", "text": "pezzo"}]}, "finish_reason": null}]
        }"#;
        let chunk: ChatCompletionChunk = serde_json::from_str(raw).unwrap();
        let out = chunk_from_sse(chunk, "mistral", "m").expect("chunk emesso");
        assert_eq!(out.delta, "pezzo");
    }

    #[test]
    fn parse_fallito_ha_errore_contestualizzato() {
        // Il messaggio deve dire provider + causa serde (diagnostico), MAI il
        // generico "error decoding response body" ne' il contenuto del body.
        let err = parse_chat_completion("mistral", "<html>proxy error</html>").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("mistral"), "manca il provider: {msg}");
        assert!(
            msg.contains("non decodificabile come ChatCompletion"),
            "manca il contesto: {msg}"
        );
        assert!(!msg.contains("proxy error"), "il body non va nel messaggio: {msg}");
    }

    #[test]
    fn response_senza_reasoning_ha_reasoning_none() {
        let raw = r#"{
            "choices": [{"message": {"content": "ok", "reasoning_content": ""}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        }"#;
        let parsed: ChatCompletion = serde_json::from_str(raw).unwrap();
        let resp = from_chat_completion(parsed, "m".to_string(), "deepseek", 1).unwrap();
        // reasoning vuoto -> None; cache assente -> None.
        assert!(resp.reasoning.is_none());
        assert!(resp.usage.cache_read_tokens.is_none());
    }

    #[test]
    fn sse_reasoning_content_emette_reasoning_delta() {
        let raw = r#"{
            "choices": [{"delta": {"reasoning_content": "penso"}, "finish_reason": null}]
        }"#;
        let chunk: ChatCompletionChunk = serde_json::from_str(raw).unwrap();
        let out = chunk_from_sse(chunk, "deepseek", "m").expect("chunk reasoning");
        assert_eq!(out.reasoning_delta.as_deref(), Some("penso"));
        assert_eq!(out.delta, "");
    }

    #[test]
    fn deserializza_response_in_llm_response() {
        let raw = r#"{
            "choices": [{
                "message": {"content": "risposta", "tool_calls": null},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        }"#;
        let parsed: ChatCompletion = serde_json::from_str(raw).unwrap();
        let resp =
            from_chat_completion(parsed, "m".to_string(), "openai", 42).unwrap();

        assert_eq!(resp.content, "risposta");
        assert_eq!(resp.finish_reason, "stop");
        assert_eq!(resp.usage.input_tokens, 10);
        assert_eq!(resp.usage.output_tokens, 5);
        assert_eq!(resp.provider_used, "openai");
        assert_eq!(resp.latency_ms, 42);
        assert!(resp.tool_calls.is_none());
    }

    #[test]
    fn deserializza_response_con_tool_calls() {
        let raw = r#"{
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {"name": "do_thing", "arguments": "{\"a\":1}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 7}
        }"#;
        let parsed: ChatCompletion = serde_json::from_str(raw).unwrap();
        let resp = from_chat_completion(parsed, "m".to_string(), "openai", 1).unwrap();

        assert_eq!(resp.content, "");
        assert_eq!(resp.finish_reason, "tool_calls");
        let calls = resp.tool_calls.expect("tool_calls presenti");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function.name, "do_thing");
        assert_eq!(calls[0].function.arguments, "{\"a\":1}");
    }

    #[test]
    fn parsa_evento_sse_data_in_chunk() {
        let raw = r#"{
            "choices": [{"delta": {"content": "Hel"}, "finish_reason": null}]
        }"#;
        let chunk: ChatCompletionChunk = serde_json::from_str(raw).unwrap();
        let out = chunk_from_sse(chunk, "openai", "m").expect("chunk emesso");

        assert_eq!(out.delta, "Hel");
        assert!(out.finish_reason.is_none());
        assert!(out.usage.is_none());
        assert_eq!(out.provider_used.as_deref(), Some("openai"));
    }

    #[test]
    fn sse_chunk_finale_riporta_usage() {
        let raw = r#"{
            "choices": [{"delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 8, "completion_tokens": 2}
        }"#;
        let chunk: ChatCompletionChunk = serde_json::from_str(raw).unwrap();
        let out = chunk_from_sse(chunk, "openai", "m").expect("chunk finale");

        assert_eq!(out.delta, "");
        assert_eq!(out.finish_reason.as_deref(), Some("stop"));
        let usage = out.usage.expect("usage all'ultimo chunk");
        assert_eq!(usage.input_tokens, 8);
        assert_eq!(usage.output_tokens, 2);
    }

    #[test]
    fn sse_tool_call_delta() {
        let raw = r#"{
            "choices": [{"delta": {"tool_calls": [{
                "index": 0,
                "id": "call_x",
                "function": {"name": "f", "arguments": "{}"}
            }]}, "finish_reason": null}]
        }"#;
        let chunk: ChatCompletionChunk = serde_json::from_str(raw).unwrap();
        let out = chunk_from_sse(chunk, "deepseek", "m").expect("tool delta");

        let tcd = out.tool_call_delta.expect("tool_call_delta presente");
        assert_eq!(tcd.index, 0);
        assert_eq!(tcd.id.as_deref(), Some("call_x"));
        assert_eq!(tcd.function.unwrap().name.as_deref(), Some("f"));
    }

    fn empty_parser() -> SseParser {
        SseParser {
            line_buf: String::new(),
            pending: std::collections::VecDeque::new(),
            done: false,
            provider_name: "openai".to_string(),
            model_used: "m".to_string(),
        }
    }

    #[test]
    fn parse_sse_line_consuma_data_e_done() {
        let mut st = empty_parser();

        st.parse_line(
            r#"data: {"choices":[{"delta":{"content":"x"},"finish_reason":null}]}"#,
        );
        assert_eq!(st.pending.len(), 1);
        assert_eq!(st.pending[0].delta, "x");

        st.parse_line("data: [DONE]");
        assert!(st.done);
    }

    #[test]
    fn drain_lines_gestisce_riga_parziale() {
        let mut st = empty_parser();
        // Primo blocco: una riga completa + una parziale (senza '\n' finale).
        st.line_buf.push_str(
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\ndata: {\"choices\":[{\"del",
        );
        st.drain_lines();
        // Solo la prima riga e' completa: un chunk pronto.
        assert_eq!(st.pending.len(), 1);
        assert_eq!(st.pending[0].delta, "a");
        // Il resto del secondo evento arriva dopo: ora la riga si completa.
        st.line_buf
            .push_str("ta\":{\"content\":\"b\"}}]}\n");
        st.drain_lines();
        assert_eq!(st.pending.len(), 2);
        assert_eq!(st.pending[1].delta, "b");
    }

    #[test]
    fn finish_reason_sconosciuto_collassa_a_stop() {
        assert_eq!(normalize_finish_reason(Some("boh")), "stop");
        assert_eq!(normalize_finish_reason(None), "stop");
        assert_eq!(normalize_finish_reason(Some("length")), "length");
        assert_eq!(normalize_finish_reason(Some("tool_calls")), "tool_calls");
    }

    // --- Vision: blocchi immagine nel content array (passo 3) --------------

    fn image_block(url: &str) -> crate::types::LlmContentBlock {
        crate::types::LlmContentBlock {
            kind: "image_url".to_string(),
            text: None,
            image_url: Some(serde_json::json!({ "url": url })),
            tool_use_id: None,
            content: None,
        }
    }

    fn text_block(text: &str) -> crate::types::LlmContentBlock {
        crate::types::LlmContentBlock {
            kind: "text".to_string(),
            text: Some(text.to_string()),
            image_url: None,
            tool_use_id: None,
            content: None,
        }
    }

    #[test]
    fn vision_blocco_immagine_diventa_content_array_nativo() {
        let mut req = sample_request();
        req.messages[0].content = MessageContent::Blocks(vec![
            text_block("descrivi"),
            image_block("data:image/png;base64,AAAA"),
        ]);
        let json = serde_json::to_value(build_request_body(
            &req,
            false,
            &ResolvedReasoning::none(),
            PromptCacheKeying::ProviderManaged,
            None,
        ))
            .unwrap();

        let content = &json["messages"][0]["content"];
        // Il content e' un ARRAY (formato OpenAI vision), non una stringa.
        let arr = content.as_array().expect("content array per vision");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "descrivi");
        assert_eq!(arr[1]["type"], "image_url");
        assert_eq!(arr[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn vision_url_http_preservato() {
        let mut req = sample_request();
        req.messages[0].content =
            MessageContent::Blocks(vec![image_block("https://example.com/x.png")]);
        let json = serde_json::to_value(build_request_body(
            &req,
            false,
            &ResolvedReasoning::none(),
            PromptCacheKeying::ProviderManaged,
            None,
        ))
            .unwrap();
        let arr = json["messages"][0]["content"].as_array().unwrap();
        assert_eq!(arr[0]["type"], "image_url");
        assert_eq!(arr[0]["image_url"]["url"], "https://example.com/x.png");
    }

    #[test]
    fn blocchi_senza_immagine_restano_stringa() {
        // Nessuna immagine -> parita' col TS (content serializzato a stringa).
        let mut req = sample_request();
        req.messages[0].content = MessageContent::Blocks(vec![text_block("solo testo")]);
        let json = serde_json::to_value(build_request_body(
            &req,
            false,
            &ResolvedReasoning::none(),
            PromptCacheKeying::ProviderManaged,
            None,
        ))
            .unwrap();
        assert!(json["messages"][0]["content"].is_string());
    }

    // --- Image generation (dialetto OpenAI Images) ------------------------

    #[test]
    fn images_response_mappa_b64_e_filtra_vuoti() {
        let raw = r#"{
            "data": [
                {"b64_json": "AAAA"},
                {"b64_json": ""},
                {"url": "https://example.com/x.png"}
            ]
        }"#;
        let parsed: ImagesResponse = serde_json::from_str(raw).unwrap();
        let out = from_images_response(parsed, "gpt-image-1".to_string(), "openai", 7);
        assert_eq!(out.model_used, "gpt-image-1");
        assert_eq!(out.provider_used, "openai");
        assert_eq!(out.latency_ms, 7);
        assert_eq!(out.images.len(), 3);
        assert_eq!(out.images[0].b64_json.as_deref(), Some("AAAA"));
        // base64 vuoto -> None (non si propaga una stringa vuota).
        assert!(out.images[1].b64_json.is_none());
        assert!(out.images[1].url.is_none());
        assert_eq!(out.images[2].url.as_deref(), Some("https://example.com/x.png"));
        // OpenAI Images non dichiara il mime.
        assert!(out.images[0].mime.is_none());
    }

    #[test]
    fn images_request_body_imposta_response_format_b64() {
        let body = ImageGenWireRequest {
            model: "gpt-image-1".to_string(),
            prompt: "un gatto".to_string(),
            n: Some(2),
            size: Some("1024x1024".to_string()),
            response_format: "b64_json".to_string(),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["model"], "gpt-image-1");
        assert_eq!(json["prompt"], "un gatto");
        assert_eq!(json["n"], 2);
        assert_eq!(json["size"], "1024x1024");
        assert_eq!(json["response_format"], "b64_json");
    }

    // --- Audio transcription (dialetto OpenAI Audio) ----------------------

    #[test]
    fn transcription_response_estrae_text() {
        let raw = r#"{ "text": "ciao mondo", "language": "it" }"#;
        let parsed: TranscriptionResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.text, "ciao mondo");
        // Risposta senza text -> stringa vuota (tollerante, non panico).
        let vuoto: TranscriptionResponse = serde_json::from_str("{}").unwrap();
        assert!(vuoto.text.is_empty());
    }

    #[test]
    fn mime_from_filename_mappa_estensioni_audio() {
        assert_eq!(mime_from_filename("audio.mp3"), Some("audio/mpeg"));
        assert_eq!(mime_from_filename("a.WAV"), Some("audio/wav"));
        assert_eq!(mime_from_filename("nota.m4a"), Some("audio/mp4"));
        assert_eq!(mime_from_filename("voce.ogg"), Some("audio/ogg"));
        assert_eq!(mime_from_filename("x.flac"), Some("audio/flac"));
        // Estensione non audio o assente -> None (default reqwest).
        assert_eq!(mime_from_filename("file.bin"), None);
        assert_eq!(mime_from_filename("senza_estensione"), None);
    }

    #[test]
    fn mime_from_audio_format_mappa_formati_tts() {
        assert_eq!(mime_from_audio_format(Some("mp3")), "audio/mpeg");
        assert_eq!(mime_from_audio_format(Some("WAV")), "audio/wav");
        assert_eq!(mime_from_audio_format(Some("opus")), "audio/opus");
        assert_eq!(mime_from_audio_format(Some("aac")), "audio/aac");
        assert_eq!(mime_from_audio_format(Some("flac")), "audio/flac");
        assert_eq!(mime_from_audio_format(Some("pcm")), "audio/pcm");
        // Formato assente o sconosciuto -> default mp3.
        assert_eq!(mime_from_audio_format(None), "audio/mpeg");
        assert_eq!(mime_from_audio_format(Some("xyz")), "audio/mpeg");
    }

    /// I sei campi storici, nell'ordine storico: e' il valore che finisce in
    /// `failures[].code` e che i consumatori a valle confrontano per
    /// uguaglianza. NON e' cio' su cui si DECIDE (per quello ci sono i
    /// candidati): e' il contratto di wire, e cambiarlo sarebbe un cambiamento
    /// di contratto travestito da miglioramento della classificazione.
    #[test]
    fn il_codice_esportato_sul_wire_resta_quello_storico() {
        let code = |provider: &str, status: u16, body: &str| {
            ProviderHttpError::from_response(provider, status, body.to_string()).code
        };
        // OpenAI/DeepSeek/Mistral: error.code / error.type.
        assert_eq!(
            code(
                "openai",
                429,
                r#"{"error":{"code":"insufficient_quota","type":"insufficient_quota"}}"#
            )
            .as_deref(),
            Some("insufficient_quota")
        );
        assert_eq!(
            code(
                "deepseek",
                400,
                r#"{"error":{"type":"invalid_request_error","message":"bad"}}"#
            )
            .as_deref(),
            Some("invalid_request_error")
        );
        // Google: error.code e' NUMERICO -> si usa error.status (enum).
        assert_eq!(
            code(
                "google",
                400,
                r#"{"error":{"code":400,"status":"INVALID_ARGUMENT","message":"x"}}"#
            )
            .as_deref(),
            Some("invalid_argument")
        );
        // groq: `type` e' la CATEGORIA ("tokens"), `code` e' l'errore. Corpo
        // VERBATIM del 2026-07-16.
        assert_eq!(
            code(
                "groq",
                413,
                r#"{"error":{"message":"Request too large for model `openai/gpt-oss-120b` on tokens per minute (TPM): Limit 8000, Requested 20083","type":"tokens","code":"rate_limit_exceeded"}}"#
            )
            .as_deref(),
            Some("rate_limit_exceeded")
        );
        // openrouter 402: il campo che DECIDE sta in `metadata.limit_source`, ma
        // sul wire il code resta `null` come oggi (il /error/code e' numerico).
        // Includerlo nell'export cambierebbe 127 righe senza che nessuno
        // l'abbia chiesto.
        assert_eq!(
            code(
                "openrouter",
                402,
                r#"{"error":{"message":"more credits","code":402,"metadata":{"limit_source":"openrouter_credits"}}}"#
            ),
            None
        );
        // Body non-JSON o senza campi: None.
        assert_eq!(code("openai", 502, "502 Bad Gateway (html)"), None);
    }

    #[test]
    fn extract_structured_error_message_da_json() {
        // Google: error.message dice QUALE argomento e' invalido (regola M).
        assert_eq!(
            extract_structured_error_message(
                r#"{"error":{"code":400,"status":"INVALID_ARGUMENT","message":"List of found errors:\t1.Field: page_size; Message: Page size should be non-negative and the maximum size is 300.\t"}}"#
            )
            .as_deref(),
            Some("List of found errors:\t1.Field: page_size; Message: Page size should be non-negative and the maximum size is 300.")
        );
        // Fallback sul message top-level (alcuni provider non annidano in error).
        assert_eq!(
            extract_structured_error_message(r#"{"message":"bad request"}"#).as_deref(),
            Some("bad request")
        );
        // Body non-JSON o senza campo message: None.
        assert_eq!(
            extract_structured_error_message(r#"{"error":{"code":400}}"#),
            None
        );
        assert_eq!(
            extract_structured_error_message("502 Bad Gateway (html)"),
            None
        );
    }

    #[test]
    fn parse_retry_after_delta_seconds_e_clamp() {
        use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
        let mut h = HeaderMap::new();
        assert_eq!(parse_retry_after(&h), None); // header assente
        h.insert(RETRY_AFTER, HeaderValue::from_static("5"));
        assert_eq!(parse_retry_after(&h), Some(5));
        // Valore assurdo: clamp a 3600.
        h.insert(RETRY_AFTER, HeaderValue::from_static("999999"));
        assert_eq!(parse_retry_after(&h), Some(3600));
        // Formato data HTTP (non delta-seconds): non gestito -> None.
        h.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2026 07:28:00 GMT"),
        );
        assert_eq!(parse_retry_after(&h), None);
    }

    // I test "dal body alla CLASSE" (413 groq, 429 kimi, credito openai,
    // mistral 1500) vivono ora in `tassonomia_errori`, dove girano contro il
    // catalogo REALE caricato dalla migrazione 0705: qui la classe non e' piu'
    // derivabile, perche' deciderla richiede il catalogo. Questo modulo resta
    // responsabile di cio' che PRODUCE - i candidati e il codice di wire - e i
    // suoi test misurano quello.
}
