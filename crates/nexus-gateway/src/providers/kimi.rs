//! Provider Kimi (Moonshot AI), endpoint OpenAI-compatibile con tre quirk.
//!
//! Gemello di [`super::mistral::MistralProvider`] per la forma — thin wrapper su
//! [`OpenAiCompatClient`], composizione e non ereditarieta' (regola L) — ma NON
//! costruibile dal provider generico del registry, e la ragione non e' di stile.
//!
//! La documentazione ufficiale (`platform.kimi.ai/docs/api/models-overview`)
//! dichiara che su `kimi-k3`, `kimi-k2.7-code` e `kimi-k2.6` la `temperature` e'
//! un valore FISSO del modello, e verbatim: "'Fixed' means the parameter cannot
//! be modified: passing any other value returns an error, so do not pass it
//! explicitly". Il provider generico inoltra `req.temperature` cosi' com'e', e
//! nel sistema esistono tre call site che mandano `Some(0.0)` a ogni chiamata
//! (`agent_graph_adapter/summary_store.rs`, `agent_graph_adapter/next_actions_deriver.rs`,
//! `project_workspace/wizard.rs`): con l'adapter generico quei tre percorsi
//! sarebbero un HTTP 400 sistematico. Il quirk sta nel CONTRATTO del fornitore,
//! non nel testo di un errore: la sede giusta e' un dialetto (regola M/H), non
//! una toppa a valle.
//!
//! Gli altri tre quirk stanno in [`ReasoningDialect::Kimi`], che li documenta:
//! `max_completion_tokens` al posto del deprecato `max_tokens`, il round-trip
//! del `reasoning_content` preteso dal Preserved Thinking, e lo spegnimento del
//! pensiero — che questo modulo decide, perche' e' l'unico a sapere se il
//! modello lo consenta (vedi [`FattiDelModello`]).
//!
//! CACHE: il fornitore cachea il prefisso da solo ("Context Caching is
//! automatically enabled for all model requests"), ma documenta anche il campo
//! `prompt_cache_key` come l'etichetta con cui raggruppare le richieste della
//! stessa sessione. E' la posizione di Mistral, e la si dichiara col vocabolario
//! gia' esistente: [`PromptCacheKeying::RequiresKey`].

use std::time::Duration;

use async_trait::async_trait;
use nexus_cache::TtlCache;
use reqwest::Client;
use sqlx::PgPool;

use crate::provider::{ChunkStream, LlmProvider};
use crate::providers::openai_compat::{OpenAiCompatClient, ReasoningDialect, ResolvedReasoning};
use crate::types::{LlmRequest, LlmResponse, PromptCacheKeying, SensitivityTier};

/// Tier ammessi: pubblico/interno/confidenziale. Mai il tier 3, riservato a chi
/// gira on-premise: Moonshot e' un fornitore cloud extra-UE come gli altri.
const TIERS: &[SensitivityTier] = &[0, 1, 2];

/// Nome del provider: identita' verso il registry, chiave di `ai_price_catalog` e
/// `provider_used` della risposta. Scritto una volta perche' le tre non possano
/// divergere — una query che cercasse un nome diverso da quello con cui il
/// provider e' costruito non troverebbe mai la riga, e in silenzio.
const PROVIDER_NAME: &str = "kimi";

/// TTL della disattivabilita' letta dal catalogo: 60s, come le altre cache di
/// configurazione del gateway (`policy_engine`, `cooldown`, affinita' upstream).
const CATALOG_TTL: Duration = Duration::from_secs(60);

/// Chiave settings (regola G) dello sforzo di ragionamento da chiedere a questo
/// fornitore. Gemella di `providers.openai.reasoning_effort`: stessa forma, altro
/// vocabolario, e per questo due chiavi e non una.
const EFFORT_SETTING: &str = "providers.kimi.reasoning_effort";

/// Vocabolario CHIUSO documentato da Moonshot per `reasoning_effort`.
///
/// E' anche il solo posto da cui puo' nascere il valore che finisce sul wire:
/// [`effort_ammesso`] ritorna un `&'static str` preso da qui (regola Q).
///
/// SERVE PIU' DI QUANTO SEMBRI, perche' l'API non fa da rete: MISURATO il
/// 17/08/2026, `reasoning_effort: "assurdo"` su kimi-k3 risponde 200 con 117 char
/// di pensiero. Un valore inventato non torna indietro come errore, torna come un
/// comportamento che nessuno ha dichiarato.
///
/// `minimal` e' stato OSSERVATO funzionare su k3 (200, 10 char di pensiero, meno
/// ancora di `low`) e NON e' qui: la doc dichiara low|high|max, e su un valore non
/// documentato non si spedisce.
const EFFORT_VOCABOLARIO: [&str; 3] = ["low", "high", "max"];

/// Lo sforzo dichiarato dal DB, ridotto al vocabolario chiuso. Stringa vuota,
/// chiave assente o valore fuori vocabolario -> `None`, cioe' non si emette.
fn effort_ammesso(valore: &str) -> Option<&'static str> {
    let valore = valore.trim();
    EFFORT_VOCABOLARIO.iter().copied().find(|v| *v == valore)
}

/// Lo sforzo da emettere dato cio' che il setting contiene, con l'avviso quando
/// quel contenuto non e' spedibile.
///
/// Funzione PURA e separata dalla lettura: e' la parte che DECIDE, e tenerla
/// fuori dal metodo che fa I/O la rende provabile senza DB — oltre a togliere
/// due livelli di annidamento a chi legge.
///
/// Il ripiego NON esiste di proposito. Un valore fuori vocabolario non ricade su
/// uno "ragionevole": qui il ripiego non sarebbe conservativo, sarebbe
/// l'attivazione di un meccanismo che nessuno ha chiesto.
fn effort_dal_setting(letto: Option<&str>) -> Option<&'static str> {
    let valore = letto?;
    let ammesso = effort_ammesso(valore);
    if ammesso.is_none() {
        // Regola F: solo identificatori di configurazione, mai il valore.
        tracing::warn!(
            provider = PROVIDER_NAME,
            setting = EFFORT_SETTING,
            "sforzo di ragionamento fuori vocabolario (low|high|max): non lo \
             dichiaro. L'API lo accetterebbe in silenzio"
        );
    }
    ammesso
}

/// Endpoint internazionale di default (override via costruttore/registry).
///
/// L'host API resta `api.moonshot.ai`: sono i domini di DOCUMENTAZIONE e console
/// ad essere migrati a `platform.kimi.ai` (301). Le chiavi dei due lati non sono
/// interscambiabili — una chiave del lato cinese su questo host non degrada, da'
/// 401 — quindi chi usa `api.moonshot.cn` cambia anche la chiave, e per farlo
/// gli basta il setting `kimi_base_url` senza toccare codice (regola G).
const DEFAULT_BASE_URL: &str = "https://api.moonshot.ai/v1";

/// Finestra del modello piu' capiente offerto (`kimi-k3`, 1M token). E' il tetto
/// del PROVIDER: la finestra per-modello vive in `ai_price_catalog.context_window`
/// e non si duplica qui.
const MAX_CONTEXT_TOKENS: u32 = 1_048_576;

/// Cio' che il catalogo DICHIARA su un fatto booleano di un modello.
///
/// Tre varianti e non un `bool` (regola Q): l'ignoto ha una conseguenza propria e
/// non deve degradare ne' a «si'» ne' a «no» per comodita' di chi legge. Solo
/// [`Self::Si`] autorizza, e [`Self::NonDichiarato`] si comporta come il codice si
/// comportava prima che il dato esistesse.
///
/// UN vocabolario per DUE fatti (mig 0705 e 0732), e non due copie: la domanda e'
/// la stessa — «cosa dice il catalogo di questa colonna nullable?» — e prima
/// erano due enum con lo stesso corpo, cioe' due posti in cui rispondere. QUALE
/// fatto sia lo dice il campo che lo porta ([`FattiDelModello`]), che e' anche
/// dove sta scritto cosa si e' misurato: le due asimmetrie non si somigliano
/// affatto, e vanno lette li' prima di cambiare una direzione.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FattoDichiarato {
    /// Il catalogo lo afferma.
    Si,
    /// Il catalogo lo nega.
    No,
    /// Il catalogo non lo dice — riga assente, colonna `NULL`, o DB che non ha
    /// parlato. Non e' una misura.
    NonDichiarato,
}

impl FattoDichiarato {
    /// Da una colonna booleana nullable, dove il `NULL` e la riga assente
    /// collassano legittimamente nella stessa risposta: in entrambi i casi
    /// nessuno ha dichiarato nulla.
    fn dal_catalogo(letto: Option<bool>) -> Self {
        match letto {
            Some(true) => Self::Si,
            Some(false) => Self::No,
            None => Self::NonDichiarato,
        }
    }

    /// L'unico punto in cui un fatto dichiarato diventa un permesso.
    fn autorizza(self) -> bool {
        matches!(self, Self::Si)
    }
}

/// Questa richiesta vuole una RISPOSTA o un RAGIONAMENTO?
///
/// Funzione pura: e' la VOLONTA' del chiamante, distinta dal fatto del fornitore
/// (`FattiDelModello::spegnibile`) con cui viene incrociata. Due segnali, in ordine:
///
/// 1. la preferenza ESPLICITA, quando c'e': chi si e' preso la briga di dirlo
///    comanda, in entrambi i versi (e' il canale che usa la batteria di
///    qualificazione per chiedere il pensiero acceso);
/// 2. in sua assenza, la presenza di TOOL. E' un run agentico: si vuole l'azione,
///    e il tetto di output e' li' per contenere una risposta, non un ragionamento.
///    E' la stessa traduzione al confine che [`super::deepseek`] fa gia' — li' per
///    evitare un 400, qui per evitare che il tetto se lo prenda il pensiero.
///
/// La volonta' sta QUI e non in `agentic_thinking_policy`, che pure ha il valore
/// giusto nel proprio vocabolario (`disable_for_tools`) e per kimi vale `'none'`:
/// quella colonna la RISCRIVE il catalog sync da un'euristica sul nome, che per
/// «kimi-k2.6» non trova alcun marcatore di reasoning. Un valore scritto li'
/// verrebbe cancellato dal primo giro di sync (motivazione estesa nella mig 0705).
///
/// Una lista di tool VUOTA non e' «ci sono tool»: senza tool veri il turno e'
/// testuale e vale il default del fornitore, come per DeepSeek.
fn vuole_risposta_e_non_ragionamento(req: &LlmRequest) -> bool {
    if let Some(t) = req.thinking.as_ref() {
        return !t.enabled;
    }
    req.tools.as_ref().is_some_and(|t| !t.is_empty())
}

pub struct KimiProvider {
    client: OpenAiCompatClient,
    /// Serve a leggere la disattivabilita' del pensiero dal catalogo. Opzionale:
    /// i test che esercitano la sola mappatura request/response non hanno DB, e
    /// senza si perde uno spegnimento, non la correttezza della chiamata.
    db: Option<PgPool>,
    /// Per MODELLO, perche' la risposta cambia per modello dentro lo stesso
    /// fornitore: e' l'intero punto di questo meccanismo. Vi entra solo cio' che
    /// si e' letto — un errore non e' una misura, e scriverlo qui
    /// cristallizzerebbe per 60s un'ignoranza momentanea.
    fatti: TtlCache<String, FattiDelModello>,
    /// Lo sforzo configurato: uno per FORNITORE, non per modello, e ridotto al
    /// vocabolario chiuso gia' qui — cio' che si memorizza e' gia' spedibile.
    effort_configurato: TtlCache<(), Option<&'static str>>,
}

/// Cio' che il catalogo dichiara su un modello kimi.
///
/// Stanno insieme perche' sono la stessa riga di `ai_price_catalog` e si leggono
/// in una volta sola. Il vocabolario e' lo stesso ([`FattoDichiarato`]) perche' la
/// domanda posta al catalogo e' la stessa; cio' che NON e' lo stesso e' la
/// conseguenza di sbagliare, ed e' scritto qui perche' e' qui che si distingue
/// quale fatto si sta leggendo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FattiDelModello {
    /// Il fornitore consente di SPEGNERE il ragionamento (mig 0705)?
    ///
    /// MISURATO il 13/08/2026: `thinking: {"type":"disabled"}` accettato su
    /// `kimi-k2.6` e `kimi-k3`, HTTP 400 «only type=enabled is allowed» su
    /// `kimi-k2.7-code` e `-highspeed`. L'errore e' RUMOROSO in un verso solo:
    /// spegnere dove non si puo' e' un 400 su OGNI chiamata a quel modello,
    /// mentre non spegnere dove si potrebbe costa soltanto il tetto di output.
    spegnibile: FattoDichiarato,
    /// Il modello interpreta `reasoning_effort` (mig 0732)?
    ///
    /// MISURATO il 17/08/2026: accettato da tutti e quattro i modelli a catalogo
    /// (l'idea che fosse del solo k3 era una supposizione), e su k3 `low` porta
    /// il completion da 80 a 24 token. L'asimmetria e' OPPOSTA a quella qui
    /// sopra e per questo piu' insidiosa: l'API risponde 200 anche a un valore
    /// insensato, quindi sbagliare non produce un errore visibile ma un effetto
    /// che nessuno ha dichiarato.
    effort: FattoDichiarato,
}

impl FattiDelModello {
    /// Cio' che si sa di un modello di cui non si e' letto nulla: niente. Non e'
    /// un default comodo, e' l'unico stato onesto quando il DB non ha parlato.
    fn ignoti() -> Self {
        Self {
            spegnibile: FattoDichiarato::NonDichiarato,
            effort: FattoDichiarato::NonDichiarato,
        }
    }
}

impl KimiProvider {
    /// Costruisce il provider senza accesso DB: la disattivabilita' del pensiero
    /// non sara' leggibile e nessuna richiesta lo spegnera' (comportamento
    /// anteriore alla mig 0705).
    pub fn new(http: Client, api_key: impl Into<String>, base_url: Option<String>) -> Self {
        Self::with_db(http, api_key, base_url, None)
    }

    /// Costruisce il provider con accesso DB per leggere dal catalogo se il
    /// pensiero sia disattivabile su quel modello (regola G: il fatto sta nel
    /// dato, non in un riconoscimento sul nome scritto qui).
    pub fn with_db(
        http: Client,
        api_key: impl Into<String>,
        base_url: Option<String>,
        db: Option<PgPool>,
    ) -> Self {
        let base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Self {
            // TETTO: Moonshot dichiara `max_tokens` deprecato in favore di
            // `max_completion_tokens` (doc `/docs/api/chat`). E' un fatto del
            // FORNITORE e sta sul client, non sul dialetto reasoning — che
            // continua a governare temperatura, round-trip e spegnimento.
            client: OpenAiCompatClient::new(http, base_url, api_key, PROVIDER_NAME)
                .with_prompt_cache_keying(PromptCacheKeying::RequiresKey)
                .with_tetto_su_completion(),
            db,
            fatti: TtlCache::new(CATALOG_TTL),
            effort_configurato: TtlCache::new(CATALOG_TTL),
        }
    }

    /// Cio' che il catalogo dichiara su questo modello (mig 0705 + 0732), in una
    /// lettura sola.
    ///
    /// UNA query e UNA cache per DUE fatti, e non e' un'ottimizzazione: sono la
    /// STESSA RIGA. Due letture separate erano due copie dello stesso giro
    /// (cache, guardia sul DB, `Option<Option<bool>>`, WARN) e avrebbero potuto
    /// divergere su cio' che conta davvero — la DIREZIONE in cui si sbaglia
    /// quando il DB non risponde. Cosi' la direzione e' scritta una volta.
    ///
    /// Ogni via di fuga porta ai `NonDichiarato`, e sempre dalla parte del non
    /// fare: nessun DB agganciato, riga assente, colonne `NULL`, query fallita —
    /// in nessuno di questi casi qualcuno ha misurato alcunche'. Un guasto del DB
    /// piu' lungo della TTL riporta al comportamento di ieri, che e' sicuro; la
    /// direzione opposta manderebbe un `disabled` a un modello che risponde 400,
    /// o un `reasoning_effort` a un modello che nessuno ha provato.
    async fn fatti_del_modello(&self, model: &str) -> FattiDelModello {
        if let Some(v) = self.fatti.get(model) {
            return v;
        }
        let Some(db) = self.db.as_ref() else {
            return FattiDelModello::ignoti();
        };
        // `Option<(..)>` e' la riga; gli `Option<bool>` dentro sono le colonne
        // nullable. Riga assente e colonna nulla dicono la stessa cosa.
        let letto: Result<Option<(Option<bool>, Option<bool>)>, sqlx::Error> = sqlx::query_as(
            "SELECT thinking_can_be_disabled, accepts_reasoning_effort \
             FROM ai_price_catalog WHERE provider = $1 AND model = $2",
        )
        .bind(PROVIDER_NAME)
        .bind(model)
        .fetch_optional(db)
        .await;
        match letto {
            Ok(row) => {
                let (spegnibile, effort) = row.unwrap_or((None, None));
                let esito = FattiDelModello {
                    spegnibile: FattoDichiarato::dal_catalogo(spegnibile),
                    effort: FattoDichiarato::dal_catalogo(effort),
                };
                self.fatti.insert(model.to_string(), esito);
                esito
            }
            Err(e) => {
                // Regola F: nei campi solo identificatori di configurazione e la
                // causa strutturata dell'errore, mai il payload.
                tracing::warn!(
                    provider = PROVIDER_NAME,
                    model = %model,
                    error = %e,
                    "fatti del modello non leggibili dal catalogo: il ragionamento \
                     resta acceso col proprio default e non si dichiara nulla"
                );
                FattiDelModello::ignoti()
            }
        }
    }

    /// Se il fornitore consenta di spegnere il pensiero su questo modello
    /// (mig 0705).
    async fn pensiero_spegnibile(&self, model: &str) -> FattoDichiarato {
        self.fatti_del_modello(model).await.spegnibile
    }

    /// Se il modello interpreti `reasoning_effort` (mig 0732).
    async fn effort_ammesso(&self, model: &str) -> FattoDichiarato {
        self.fatti_del_modello(model).await.effort
    }

    /// Lo sforzo configurato per questo fornitore (settings, TTL 60s), gia'
    /// ridotto al vocabolario chiuso.
    ///
    /// `None` non e' un ripiego ed e' il SEED: la mig 0732 nasce con la chiave
    /// vuota, cioe' il meccanismo spento. Un valore fuori vocabolario finisce
    /// nello stesso `None` con un WARN, e non ricade su un valore "ragionevole":
    /// qui il ripiego non sarebbe conservativo, sarebbe l'attivazione di un
    /// meccanismo che nessuno ha chiesto.
    async fn effort_configurato(&self) -> Option<&'static str> {
        if let Some(v) = self.effort_configurato.get(&()) {
            return v;
        }
        let db = self.db.as_ref()?;
        // `get_setting` scarta gia' trim e valori vuoti: al criterio arriva solo
        // qualcosa che qualcuno ha scritto davvero.
        let letto = nexus_auth::get_setting(db, EFFORT_SETTING).await;
        let effort = effort_dal_setting(letto.as_deref());
        self.effort_configurato.insert((), effort);
        effort
    }

    /// Il dialetto e' SEMPRE `Kimi`, senza condizioni sul modello o sulla
    /// richiesta: i quirk di forma che porta con se' (niente temperatura,
    /// `max_completion_tokens`, round-trip del pensiero) valgono per l'intero
    /// parco moderno del fornitore.
    ///
    /// `enabled` e' l'unico campo che varia, ed e' la CONGIUNZIONE di due cose
    /// che restano distinte fino a qui: cosa vuole il chiamante
    /// ([`vuole_risposta_e_non_ragionamento`], pura) e cosa il fornitore consente
    /// ([`Self::pensiero_spegnibile`], dal catalogo). Il pensiero si spegne solo
    /// se entrambe lo dicono; in ogni altro caso resta il default del fornitore,
    /// che e' acceso.
    ///
    /// `effort` (mig 0732) e' la CONGIUNZIONE di tre cose, e nessuna delle tre e'
    /// sostituibile dalle altre:
    ///
    ///   1. il MODELLO lo interpreta ([`Self::effort_ammesso`], dal catalogo);
    ///   2. qualcuno lo ha CONFIGURATO ([`Self::effort_configurato`]) — al seed
    ///      la chiave e' vuota, quindi il meccanismo nasce spento;
    ///   3. il pensiero NON viene spento su questa stessa richiesta.
    ///
    /// La terza non e' una cautela di stile: chiedere uno sforzo di ragionamento
    /// e insieme dichiarare `thinking: {"type":"disabled"}` sono due istruzioni
    /// contraddittorie nello stesso corpo, e quale delle due vinca lo deciderebbe
    /// il fornitore al posto nostro. Fra le due comanda lo spegnimento, che e'
    /// l'istruzione MISURATA (mig 0705).
    async fn resolve_reasoning(&self, req: &LlmRequest) -> ResolvedReasoning {
        let spegnere = vuole_risposta_e_non_ragionamento(req)
            && self
                .pensiero_spegnibile(&req.model)
                .await
                .autorizza();
        let effort = if spegnere {
            None
        } else if self.effort_ammesso(&req.model).await.autorizza() {
            self.effort_configurato().await
        } else {
            None
        };
        ResolvedReasoning {
            dialect: ReasoningDialect::Kimi,
            enabled: !spegnere,
            effort: effort.map(str::to_string),
        }
    }
}

#[async_trait]
impl LlmProvider for KimiProvider {
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
        MAX_CONTEXT_TOKENS
    }

    fn tier_compatibility(&self) -> &[SensitivityTier] {
        TIERS
    }

    async fn complete(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
        let reasoning = self.resolve_reasoning(req).await;
        self.client.complete_with_reasoning(req, &reasoning).await
    }

    async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        let reasoning = self.resolve_reasoning(req).await;
        self.client.stream_with_reasoning(req, &reasoning).await
    }

    async fn healthcheck(&self) -> bool {
        self.client.healthcheck().await
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        self.client.list_models().await
    }

    /// `GET /v1/models` di Moonshot dichiara `context_length` accanto all'id: la
    /// si propaga come fa Mistral, cosi' il catalog sync scrive la finestra REALE
    /// invece di lasciarla ignota (regola G/H).
    async fn list_models_meta(&self) -> anyhow::Result<Vec<crate::provider::ModelMeta>> {
        self.client.list_models_meta().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        LlmMessage, LlmToolDefinition, MessageContent, RequestMetadata, ThinkingConfig,
        ToolFunctionDef,
    };

    fn provider() -> KimiProvider {
        KimiProvider::new(Client::new(), "chiave", None)
    }

    fn provider_con_catalogo(pool: PgPool) -> KimiProvider {
        KimiProvider::with_db(Client::new(), "chiave", None, Some(pool))
    }

    /// Un tool qualunque: serve solo a rendere la richiesta un turno agentico.
    fn tool_qualunque() -> LlmToolDefinition {
        LlmToolDefinition {
            kind: "function".to_string(),
            function: ToolFunctionDef {
                name: "read_file".to_string(),
                description: None,
                parameters: serde_json::json!({"type": "object", "properties": {}}),
                strict: None,
            },
        }
    }

    /// Turno agentico su un modello preciso: e' la forma in cui il difetto e'
    /// stato misurato — tool presenti, tetto stretto, e il pensiero che se lo
    /// prende tutto.
    fn richiesta_agentica(model: &str) -> LlmRequest {
        let mut req = richiesta();
        req.model = model.to_string();
        req.tools = Some(vec![tool_qualunque()]);
        req
    }

    /// Richiesta con i due campi che il dialetto deve trattare in modo diverso
    /// dal default: una temperatura esplicita e un tetto di output. La history
    /// porta un assistant col pensiero, per il round-trip.
    fn richiesta() -> LlmRequest {
        LlmRequest {
            model: "kimi-k3".to_string(),
            messages: vec![
                LlmMessage {
                    role: "system".to_string(),
                    content: MessageContent::Text("istruzioni di progetto".to_string()),
                    tool_call_id: None,
                    tool_calls: None,
                    name: None,
                    thinking_signature: None,
                    reasoning: None,
                    is_error: None,
                },
                LlmMessage {
                    role: "assistant".to_string(),
                    content: MessageContent::Text("ecco la risposta".to_string()),
                    tool_call_id: None,
                    tool_calls: None,
                    name: None,
                    thinking_signature: None,
                    reasoning: Some("ho ragionato cosi'".to_string()),
                    is_error: None,
                },
            ],
            // E' il valore che i tre call site interni mandano davvero.
            temperature: Some(0.0),
            max_tokens: Some(2048),
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
            effort: None,
        }
    }

    /// Il corpo che parte davvero, costruito dalla stessa strada di `complete`:
    /// `resolve_reasoning` reale e `corpo_della_richiesta` reale, non un
    /// `ResolvedReasoning` scritto a mano nel test (regola O — e' l'errore che
    /// rese verdi per sempre i tre test di `error_class_from_gateway`).
    async fn corpo_di(p: &KimiProvider, req: &LlmRequest) -> serde_json::Value {
        serde_json::to_value(
            p.client
                .corpo_della_richiesta(req, false, &p.resolve_reasoning(req).await)
                .await,
        )
        .expect("il corpo serializza")
    }

    async fn corpo() -> serde_json::Value {
        corpo_di(&provider(), &richiesta()).await
    }

    /// IL quirk che costa 400 a ogni chiamata: la temperatura non deve partire.
    ///
    /// La doc dice "passing any other value returns an error, so do not pass it
    /// explicitly", e tre call site interni mandano `Some(0.0)` sempre. Il test
    /// parte da una richiesta che LA CONTIENE: se il dialetto smettesse di
    /// filtrarla, qui comparirebbe.
    #[tokio::test]
    async fn la_temperatura_non_parte_verso_kimi() {
        let corpo = corpo().await;
        assert!(
            corpo.get("temperature").is_none(),
            "temperature inviata a Kimi: e' un HTTP 400 su ogni chiamata"
        );
    }

    /// `max_tokens` e' deprecato: il tetto viaggia su `max_completion_tokens`.
    #[tokio::test]
    async fn il_tetto_di_output_usa_il_campo_non_deprecato() {
        let corpo = corpo().await;
        assert_eq!(
            corpo.get("max_completion_tokens").and_then(|v| v.as_u64()),
            Some(2048)
        );
        assert!(corpo.get("max_tokens").is_none());
    }

    /// Preserved Thinking: l'assistant torna indietro col proprio
    /// `reasoning_content`, come la doc prescrive.
    ///
    /// MISURATO il 09/08/2026 che l'API non rifiuta il turno che lo omette (a
    /// differenza di DeepSeek, che risponde 400): il campo si manda perche' su
    /// un modello che pensa sempre e' la continuita' del ragionamento fra un
    /// turno e il successivo, non una difesa da un errore.
    #[tokio::test]
    async fn il_pensiero_dell_assistant_torna_indietro() {
        let corpo = corpo().await;
        let messaggi = corpo["messages"].as_array().expect("array di messaggi");
        let assistant = messaggi
            .iter()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
            .expect("l'assistant e' nella history");
        assert_eq!(
            assistant.get("reasoning_content").and_then(|v| v.as_str()),
            Some("ho ragionato cosi'"),
            "il pensiero non e' tornato indietro: il modello riparte senza il proprio \
             ragionamento del turno precedente"
        );
    }

    /// Dal NOME del provider fino ai campi di affinita' sul wire: guarda la
    /// CONSEGUENZA, non il valore dell'enum. Un test su `cache_keying()`
    /// proverebbe solo che il costruttore ritorna cio' che c'e' scritto.
    ///
    /// `session_id` e `provider` NON devono partire: sono i campi che un
    /// instradatore verso terzi legge, e Moonshot serve i propri modelli — un
    /// campo sconosciuto e' il solo verso in cui questa scelta puo' fare danno.
    #[tokio::test]
    async fn la_chiave_di_cache_parte_e_i_campi_da_instradatore_no() {
        let corpo = corpo().await;
        assert!(
            corpo
                .get("prompt_cache_key")
                .and_then(|v| v.as_str())
                .is_some_and(|k| !k.is_empty()),
            "senza prompt_cache_key il fornitore raggruppa le richieste a caso"
        );
        assert!(corpo.get("session_id").is_none());
        assert!(corpo.get("provider").is_none());
    }

    // ── spegnimento del pensiero ──────────────────────────────────────────────

    /// La VOLONTA', da sola. Pura: non tocca il catalogo, quindi misura solo il
    /// segnale del chiamante e non lo confonde col permesso del fornitore.
    #[test]
    fn la_volonta_viene_dal_chiamante_e_poi_dai_tool() {
        // Turno testuale senza preferenza: default del fornitore (pensiero acceso).
        assert!(!vuole_risposta_e_non_ragionamento(&richiesta()));

        // Run agentico: si vuole l'azione, e il tetto deve contenere la risposta.
        assert!(vuole_risposta_e_non_ragionamento(&richiesta_agentica(
            "kimi-k2.6"
        )));

        // Una lista di tool VUOTA non e' «ci sono tool»: fissa il confine del ramo.
        let mut vuota = richiesta();
        vuota.tools = Some(vec![]);
        assert!(!vuole_risposta_e_non_ragionamento(&vuota));

        // La preferenza esplicita comanda, in ENTRAMBI i versi, anche contro i
        // tool: e' il canale con cui la batteria di qualificazione chiede il
        // pensiero acceso, e un run agentico non deve poterglielo negare.
        let mut chiede_pensiero = richiesta_agentica("kimi-k2.6");
        chiede_pensiero.thinking = Some(ThinkingConfig {
            enabled: true,
            budget_tokens: None,
            mandatory: false,
        });
        assert!(!vuole_risposta_e_non_ragionamento(&chiede_pensiero));

        let mut chiede_risposta = richiesta();
        chiede_risposta.thinking = Some(ThinkingConfig {
            enabled: false,
            budget_tokens: None,
            mandatory: false,
        });
        assert!(vuole_risposta_e_non_ragionamento(&chiede_risposta));
    }

    /// Senza catalogo non si spegne NIENTE, nemmeno dove il fornitore lo
    /// consentirebbe: il permesso e' un dato, e in sua assenza vale il
    /// comportamento anteriore alla mig 0705.
    #[tokio::test]
    async fn senza_catalogo_il_pensiero_resta_acceso() {
        let p = provider();
        assert_eq!(
            p.pensiero_spegnibile("kimi-k2.6").await,
            FattoDichiarato::NonDichiarato
        );
        let corpo = corpo_di(&p, &richiesta_agentica("kimi-k2.6")).await;
        assert!(
            corpo.get("thinking").is_none(),
            "senza il dato non si dichiara nulla sul pensiero"
        );
    }

    /// IL test del difetto: il permesso arriva dal CATALOGO, per modello, e la
    /// conseguenza si guarda sul corpo che parte davvero.
    ///
    /// I quattro verdetti vengono dalla mig 0705, non da un inserimento di
    /// comodo: sono cio' che l'API ha risposto il 13/08/2026, e se qualcuno
    /// cambia quella migrazione questo test se ne accorge (regola O). Il corpo
    /// passa da `resolve_reasoning` e `corpo_della_richiesta` reali: costruire un
    /// `ResolvedReasoning` a mano fisserebbe qui l'assunto da verificare.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_pensiero_si_spegne_solo_dove_il_fornitore_lo_consente(pool: PgPool) {
        let p = provider_con_catalogo(pool);

        // MISURATO: `thinking: {"type":"disabled"}` accettato (reasoning 575 -> 1).
        for consentito in ["kimi-k2.6", "kimi-k3"] {
            assert_eq!(
                p.pensiero_spegnibile(consentito).await,
                FattoDichiarato::Si,
                "{consentito}: la 0705 lo dichiara disattivabile"
            );
            let corpo = corpo_di(&p, &richiesta_agentica(consentito)).await;
            assert_eq!(
                corpo["thinking"]["type"], "disabled",
                "{consentito}: senza questo campo il tetto se lo prende il \
                 ragionamento (content vuoto a 1024, 214,8s a 8192)"
            );
        }

        // MISURATO: HTTP 400 «only type=enabled is allowed». Spegnere qui
        // sarebbe un errore su OGNI chiamata a quel modello.
        for vietato in ["kimi-k2.7-code", "kimi-k2.7-code-highspeed"] {
            assert_eq!(
                p.pensiero_spegnibile(vietato).await,
                FattoDichiarato::No,
                "{vietato}: la 0705 lo dichiara NON disattivabile"
            );
            let corpo = corpo_di(&p, &richiesta_agentica(vietato)).await;
            assert!(
                corpo.get("thinking").is_none(),
                "{vietato}: un thinking esplicito qui e' un 400 sistematico"
            );
        }

        // Un modello che il catalogo non conosce non autorizza niente: l'ignoto
        // non degrada a permesso.
        assert_eq!(
            p.pensiero_spegnibile("kimi-k9-mai-vista").await,
            FattoDichiarato::NonDichiarato
        );
        let corpo = corpo_di(&p, &richiesta_agentica("kimi-k9-mai-vista")).await;
        assert!(corpo.get("thinking").is_none());
    }

    /// Il permesso non basta: senza la volonta' il pensiero resta acceso. Prova
    /// che i due segnali sono davvero in congiunzione e che il turno testuale su
    /// un modello disattivabile non cambia comportamento.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_permesso_da_solo_non_spegne_niente(pool: PgPool) {
        let p = provider_con_catalogo(pool);
        let mut req = richiesta();
        req.model = "kimi-k2.6".to_string();
        assert!(req.tools.is_none(), "premessa: turno testuale");

        let corpo = corpo_di(&p, &req).await;
        assert!(
            corpo.get("thinking").is_none(),
            "nessuno ha chiesto una risposta secca: vale il default del fornitore"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // reasoning_effort (mig 0732). Il corpo passa sempre da `corpo_di`, cioe'
    // dalla `resolve_reasoning` reale: un `ResolvedReasoning` scritto a mano
    // fisserebbe qui l'assunto da verificare (regola O).
    // ─────────────────────────────────────────────────────────────────────

    /// Scrive lo sforzo configurato come farebbe un amministratore, e invalida la
    /// cache di processo di nexus-auth: la scrittura non passa da
    /// `update_setting_value`, quindi quella cache non se ne accorgerebbe: e' il
    /// contratto dichiarato di quella cache, e il test lo rispetta invece di
    /// aggirarlo.
    async fn configura_effort(pool: &PgPool, valore: &str) {
        sqlx::query("UPDATE settings SET value = $1 WHERE key = $2")
            .bind(valore)
            .bind(EFFORT_SETTING)
            .execute(pool)
            .await
            .expect("scrittura setting");
        nexus_auth::invalidate_setting_cache(pool, EFFORT_SETTING);
    }

    /// IL test del lotto: lo sforzo parte solo dove i TRE segnali concordano, e
    /// il permesso viene dal CATALOGO.
    ///
    /// I valori seminati vengono dalla mig 0732, che a sua volta li ha dai probe
    /// del 17/08/2026 sull'API reale: su kimi-k3 `low` porta il completion da 80
    /// a 24 token. Se qualcuno cambia quella migrazione, questo test se ne accorge
    /// (regola O).
    ///
    /// MUTAZIONE: emettere senza guardare il catalogo -> rosso sul modello ignoto;
    /// hardcodare un valore -> rosso col setting vuoto.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn lo_sforzo_parte_solo_se_catalogo_e_setting_concordano(pool: PgPool) {
        // SEED della 0732: il meccanismo nasce SPENTO. E' la prima cosa da
        // provare, perche' e' lo stato in cui il deploy mette il sistema.
        let p = provider_con_catalogo(pool.clone());
        assert_eq!(p.effort_ammesso("kimi-k3").await, FattoDichiarato::Si);
        assert!(
            corpo_di(&p, &richiesta())
                .await
                .get("reasoning_effort")
                .is_none(),
            "col setting al seed (vuoto) non parte nulla, nemmeno dove il \
             modello lo accetta"
        );

        // Configurato: parte, e parte su tutti i modelli che la 0732 dichiara.
        configura_effort(&pool, "low").await;
        for model in [
            "kimi-k3",
            "kimi-k2.6",
            "kimi-k2.7-code",
            "kimi-k2.7-code-highspeed",
        ] {
            let p = provider_con_catalogo(pool.clone());
            let mut req = richiesta();
            req.model = model.to_string();
            assert_eq!(
                corpo_di(&p, &req).await["reasoning_effort"],
                "low",
                "{model}: la 0732 lo dichiara, il setting lo configura"
            );
        }

        // Modello che il catalogo non conosce: l'ignoto non autorizza. Conta piu'
        // che altrove — l'API non risponde 400 a cio' che non capisce, quindi uno
        // sbaglio qui non si vedrebbe in nessun log.
        let p = provider_con_catalogo(pool.clone());
        let mut ignoto = richiesta();
        ignoto.model = "kimi-k9-mai-vista".to_string();
        assert_eq!(
            p.effort_ammesso("kimi-k9-mai-vista").await,
            FattoDichiarato::NonDichiarato
        );
        assert!(
            corpo_di(&p, &ignoto).await.get("reasoning_effort").is_none(),
            "nessuno ha misurato questo modello: non gli si dichiara nulla"
        );
    }

    /// Il vocabolario e' chiuso, e cio' che ne sta fuori NON parte.
    ///
    /// Non e' zelo: MISURATO il 17/08/2026, `reasoning_effort: "assurdo"` su
    /// kimi-k3 risponde 200 con 117 char di pensiero. L'API non fa da rete, quindi
    /// la rete e' questa.
    ///
    /// MUTAZIONE: inoltrare la stringa grezza del setting -> rosso sul caso
    /// 'assurdo'.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn fuori_vocabolario_non_parte(pool: PgPool) {
        for (scritto, atteso) in [
            ("high", Some("high")),
            ("max", Some("max")),
            // Osservato funzionare sull'API, ma non documentato: non si spedisce.
            ("minimal", None),
            ("assurdo", None),
            ("", None),
        ] {
            configura_effort(&pool, scritto).await;
            // Provider NUOVO a ogni giro: la cache dello sforzo ha TTL 60s.
            let p = provider_con_catalogo(pool.clone());
            let corpo = corpo_di(&p, &richiesta()).await;
            match atteso {
                Some(v) => assert_eq!(corpo["reasoning_effort"], v, "setting '{scritto}'"),
                None => assert!(
                    corpo.get("reasoning_effort").is_none(),
                    "setting '{scritto}': l'API lo accetterebbe in silenzio"
                ),
            }
        }
    }

    /// Spegnere il pensiero e chiedere uno sforzo di ragionamento sono due
    /// istruzioni contraddittorie nello stesso corpo: fra le due comanda lo
    /// spegnimento, che e' quella MISURATA (mig 0705).
    ///
    /// MUTAZIONE: calcolare lo sforzo prima dello spegnimento (o in parallelo) ->
    /// il corpo porta insieme `thinking: disabled` e `reasoning_effort`, rosso.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn lo_spegnimento_del_pensiero_esclude_lo_sforzo(pool: PgPool) {
        configura_effort(&pool, "low").await;
        let p = provider_con_catalogo(pool.clone());

        // Turno agentico su un modello che la 0705 dichiara spegnibile.
        let corpo = corpo_di(&p, &richiesta_agentica("kimi-k3")).await;
        assert_eq!(corpo["thinking"]["type"], "disabled");
        assert!(
            corpo.get("reasoning_effort").is_none(),
            "chiedere uno sforzo a un pensiero appena spento lascerebbe al \
             fornitore la scelta di quale delle due istruzioni onorare"
        );

        // Controprova sullo stesso setting: dove il pensiero NON si spegne (la
        // 0705 lo vieta su k2.7-code) lo sforzo parte.
        let corpo = corpo_di(&p, &richiesta_agentica("kimi-k2.7-code")).await;
        assert!(corpo.get("thinking").is_none());
        assert_eq!(corpo["reasoning_effort"], "low");
    }

    #[test]
    fn capacita_dichiarate() {
        let p = provider();
        assert_eq!(p.name(), "kimi");
        assert!(p.supports_tools());
        assert!(p.supports_streaming());
        assert_eq!(p.max_context_tokens(), 1_048_576);
        assert_eq!(p.tier_compatibility(), &[0, 1, 2]);
    }
}
