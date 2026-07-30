//! Porte astratte delle dipendenze I/O dei nodi (inversione di dipendenza).
//!
//! VINCOLO ARCHITETTURALE: `nexus-agent-graph` NON deve dipendere da `mcp-core`
//! (mcp-core dipendera' da lui -> ciclo). Le dipendenze I/O (gateway LLM,
//! esecuzione tool, eventi SSE) sono qui espresse come TRAIT astratti; mcp-core
//! li implementera' in un PR futuro delegando alle sue infrastrutture concrete
//! (es. `nexus-gateway` per `LlmGateway`, il ToolRunner gRPC per `ToolExecutor`,
//! il canale SSE per `EventSink`). Questo e' il confine d'inversione: i nodi
//! dipendono dai trait, non dalle implementazioni.
//!
//! Le strutture dati (`LlmRequest`/`LlmResponse`/`ToolCall`/`ToolOutcome`/
//! `SseEvent`) sono MINIMALI e provider-agnostiche: trasportano solo cio' che
//! serve ai nodi. Nessun nome modello / URL provider e' hardcoded qui (regola
//! G): provider e model arrivano dal chiamante (risolti a monte dalla routing
//! matrix).

use async_trait::async_trait;
use nexus_types::error_presentation::RenderedError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::decisions::dag_scheduler::{Todo, TodoStatus};
use crate::decisions::escalation::{CrossProviderCandidate, EscalationCandidate};
use crate::decisions::governance::GovernancePolicy;
use crate::state::ToolUse;

/// Errore di una porta I/O. Opaco al runtime: messaggio + classe sintetica per
/// permettere ai nodi di distinguere un guasto infrastrutturale (gateway/tool
/// down) da un errore applicativo, senza accoppiarsi al dettaglio concreto.
#[derive(Debug, Error)]
pub enum PortError {
    /// Il gateway LLM ha risposto con un errore (provider down, billing, 4xx).
    ///
    /// Porta un [`RenderedError`] e non una `String` perche' questo e' il tipo
    /// che attraversa il confine fino al messaggio in chat: con una stringa
    /// opaca il dettaglio tecnico e la frase per l'utente erano la STESSA cosa,
    /// e cio' che arrivava all'occhio era la riga diagnostica nata per i log
    /// ("error sending request for url ... <- io(ConnectionRefused,
    /// os_error=10061)"). Ora `Display` da' la frase e `detail` resta per i log.
    #[error("gateway LLM: {0}")]
    Llm(RenderedError),
    /// Il gateway LLM ha risposto che il/i provider risolti per la richiesta NON
    /// sono disponibili (cooldown billing/transient, rifiuto 4xx del provider,
    /// esclusione di policy per tier, o `PROVIDER_ERROR` aggregato: "tutti i
    /// provider hanno fallito"). E' un SEGNALE STRUTTURATO distinto da
    /// [`PortError::Llm`] generico (regola L): il nodo executor lo matcha per
    /// tentare il FALLBACK cross-provider (escalation) invece di chiudere il run
    /// con `StopReason::Error`. Il discriminante e' il CODICE errore del gateway
    /// (`PROVIDER_ERROR`/`POLICY_TIER_EXCLUDED` nel body), NON il testo lessicale
    /// "in cooldown" (fragile): la mappatura vive nell'adapter (`llm_gateway.rs`).
    /// Il payload porta anche la CAUSA tipizzata cosi' il meta-step di
    /// escalation riporta il motivo onesto (regola M), non "cooldown" per tutto.
    #[error("provider non disponibile: {0}")]
    ProviderUnavailable(ProviderUnavailableInfo),
    /// L'esecuzione di un tool e' fallita (ToolRunner down o errore applicativo).
    ///
    /// Stesso motivo di [`PortError::Llm`]: da qui passava il `Display` di
    /// `tonic::Status`, che stampa `details: [], metadata: MetadataMap { headers:
    /// {...} }` dentro il tool_result mostrato nel nastro attivita'.
    #[error("esecuzione tool: {0}")]
    Tool(RenderedError),
}

/// Causa STRUTTURATA dell'indisponibilita' di un provider (vocabolario chiuso,
/// regola M): derivata dai CODICI del body d'errore del gateway
/// (`code` + `details.primary_cause`), mai dal testo umano. E' cio' che rende
/// onesto il meta-step "Cambio provider": un rifiuto 4xx del provider o
/// un'esclusione di policy NON devono essere raccontati come "cooldown"
/// (incidente run 48793fde del 2026-07-06).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFailureCause {
    /// Cooldown transitorio (rate-limit/5xx recenti) del provider.
    Cooldown,
    /// Credito/fatturazione esauriti (billing fresco o cooldown billing).
    Billing,
    /// Il provider ha RIFIUTATO la richiesta (4xx client): il provider e' sano,
    /// e' la richiesta corrente a non essere accettata.
    ClientError,
    /// Provider escluso dalla policy per il sensitivity tier del contenuto.
    PolicyTierExcluded,
    /// Il provider ha risposto 200 ma DEGENERE: nessun testo ne' tool-call, con
    /// `finish_reason` non terminale (tipicamente Gemini che consuma il budget
    /// nel thinking e ritorna `length`). Il provider e' raggiungibile e non ha
    /// esaurito credito: e' il TURNO a non aver prodotto output. Va trattato
    /// come indisponibilita' del provider PER QUESTA richiesta -> failover
    /// cross-provider (a differenza di `ClientError`, che non ripiega).
    EmptyCompletion,
    /// La richiesta e' troppo grande PER QUESTO provider (HTTP 413 request_too_large
    /// / context/request-size limit): ritentare lo STESSO provider e' inutile (come
    /// `ClientError`), ma un provider a finestra/limite piu' grande la accetterebbe.
    /// Quindi e' cross-provider-recuperabile -> failover (a differenza di
    /// `ClientError`). Incidente reale: groq/128k dopo google/1M -> 413 -> niente
    /// secondo failover -> figura consiglio n/d.
    ContextTooLong,
    /// Non determinabile dai segnali disponibili.
    Unknown,
}

impl ProviderFailureCause {
    /// Identificatore stabile per telemetria/payload meta-step.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cooldown => "cooldown",
            Self::Billing => "billing",
            Self::ClientError => "client_error",
            Self::PolicyTierExcluded => "policy_tier_excluded",
            Self::EmptyCompletion => "empty_completion",
            Self::ContextTooLong => "context_too_long",
            Self::Unknown => "unknown",
        }
    }

    /// `true` quando lo switch e' dovuto a indisponibilita' temporale del
    /// provider (cooldown/credito): il frontend colora la banda come cooldown.
    /// `EmptyCompletion` NON e' cooldown-like: il provider e' sano e non ha
    /// esaurito credito, e' il turno a non aver prodotto output.
    pub fn is_cooldown_like(self) -> bool {
        matches!(self, Self::Cooldown | Self::Billing)
    }
}

/// Payload di [`PortError::ProviderUnavailable`]: causa tipizzata + CODICE
/// strutturato del provider (regola M) + messaggio umano (solo display/log).
/// `From<String>` per i costruttori che non hanno segnali di causa (stub di test,
/// porte legacy): causa `Unknown`, `code` `None`.
#[derive(Debug, Clone)]
pub struct ProviderUnavailableInfo {
    pub cause: ProviderFailureCause,
    /// Codice d'errore STRUTTURATO del provider (`error.code`/`error.type`/
    /// `error.status` normalizzato lowercase dal gateway, es. `invalid_argument`
    /// per Google, `invalid_request_message_order` per Mistral). E' il DISCRIMINANTE
    /// (regola M: mai la prosa del messaggio) fra un 400 PROVIDER-SPECIFICO
    /// recuperabile su un altro provider e un 400 di formato/history CONDIVISA che
    /// fallirebbe ovunque. `None` quando il gateway non ha un codice strutturato
    /// (retro-compatibile) o quando la causa non e' un `ClientError` (irrilevante).
    pub code: Option<String>,
    pub message: String,
}

impl ProviderUnavailableInfo {
    pub fn new(cause: ProviderFailureCause, message: impl Into<String>) -> Self {
        Self {
            cause,
            code: None,
            message: message.into(),
        }
    }

    /// Builder additivo: allega il codice strutturato del provider (regola M). Il
    /// costruttore [`Self::new`] resta invariato per i call site senza segnale.
    pub fn with_code(mut self, code: Option<String>) -> Self {
        self.code = code;
        self
    }

    /// PUNTO UNICO (regola L) della decisione "questo provider caduto consente il
    /// FAILOVER cross-provider?", dato il vocabolario DB-driven dei codici di
    /// `client_error` recuperabili (regola G, mai hardcoded sparso).
    ///
    /// - causa != `ClientError` (billing/cooldown/policy/empty_completion): il
    ///   provider e' il problema, un altro provider aiuta -> failover SEMPRE
    ///   consentito (comportamento storico invariato).
    /// - causa == `ClientError`: il provider ha RIFIUTATO la richiesta (4xx). Il
    ///   failover e' consentito SOLO se il [`Self::code`] STRUTTURATO (regola M:
    ///   status+code, mai la prosa) e' nel vocabolario `recoverable_codes` — cioe'
    ///   un 400 PROVIDER-SPECIFICO (es. Google `invalid_argument`/`thought_signature`/
    ///   schema) che un altro provider (deepseek) accetterebbe. Un 400 di formato/
    ///   history CONDIVISA (Mistral `invalid_request_message_order`) NON e' in
    ///   whitelist: fallirebbe su qualunque provider (incidente f0ad0337), quindi
    ///   niente failover cieco -> chiusura onesta.
    pub fn allows_cross_provider_failover(&self, recoverable_codes: &[String]) -> bool {
        if self.cause != ProviderFailureCause::ClientError {
            return true;
        }
        client_error_code_is_recoverable(self.code.as_deref(), recoverable_codes)
    }
}

/// PUNTO UNICO (regola L) del match "codice `client_error` recuperabile cross-
/// provider": normalizza il codice a lowercase/trim e lo confronta col vocabolario
/// DB-driven (regola G). `None` o codice vuoto -> NON recuperabile (conservativo:
/// senza un segnale strutturato non si fa failover cieco, regola M). Usato dalla
/// decisione [`ProviderUnavailableInfo::allows_cross_provider_failover`] e
/// testabile in isolamento.
pub fn client_error_code_is_recoverable(code: Option<&str>, recoverable_codes: &[String]) -> bool {
    let Some(c) = code
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
    else {
        return false;
    };
    recoverable_codes
        .iter()
        .any(|w| w.trim().eq_ignore_ascii_case(&c))
}

impl std::fmt::Display for ProviderUnavailableInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl From<String> for ProviderUnavailableInfo {
    fn from(message: String) -> Self {
        Self {
            cause: ProviderFailureCause::Unknown,
            code: None,
            message,
        }
    }
}

/// Messaggio nel formato minimale richiesto dal gateway (ruolo + contenuto).
///
/// Provider-agnostico: il gateway concreto (mcp-core) traduce questa forma nel
/// payload specifico del provider scelto. `content` e' JSON arbitrario per
/// ammettere sia testo semplice sia blocchi strutturati (tool_use/tool_result).
///
/// CONTINUITA' TOOL MULTI-TURN (regola L, un solo formato messaggio wire): per
/// un turno `assistant` che ha chiamato tool i `tool_use` vanno in
/// [`LlmMessage::tool_calls`] (NON appiattiti in `content`); per un turno `tool`
/// (risultato) il `role` e' `"tool"` e [`LlmMessage::tool_call_id`] referenzia la
/// chiamata. Il server (`to_anthropic_messages`) riconosce la coppia tool_use /
/// tool_result SOLO da questi campi: senza di essi Anthropic risponde HTTP 400
/// (`tool_use ids without tool_result`). I due campi sono `Option` additivi:
/// `None` su tutti i messaggi testuali (retrocompatibile coi call site esistenti).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LlmMessage {
    /// Ruolo del messaggio (`system` | `user` | `assistant` | `tool`).
    pub role: String,
    /// Contenuto: stringa o struttura a blocchi (JSON opaco al runtime).
    pub content: Value,
    /// Tool-call emesse da un turno `assistant` (continuita' tool_use). `None` /
    /// vuoto per un turno testuale. Gli id qui DEVONO combaciare col
    /// [`LlmMessage::tool_call_id`] del messaggio `tool` che ne porta il risultato.
    pub tool_calls: Option<Vec<ToolUse>>,
    /// Id della tool-call a cui un messaggio `tool` (risultato) risponde
    /// (round-trip). `None` su tutti gli altri ruoli.
    pub tool_call_id: Option<String>,
    /// Reasoning (`reasoning_content`) di un turno `assistant` precedente generato
    /// in thinking mode (DeepSeek), da RI-PASSARE all'API: vincolo HTTP 400
    /// analogo al `thinking_signature` Anthropic. Il gateway concreto lo inoltra
    /// SOLO al dialetto DeepSeek (vedi `nexus_gateway::GwMessage::reasoning` ->
    /// wire `reasoning_content`). `None` su tutti gli altri ruoli/provider.
    /// Additivo (`Default`): retrocompatibile coi call site esistenti.
    pub reasoning: Option<String>,
    /// Firma opaca del blocco `thinking` (Anthropic) di un turno `assistant`
    /// precedente, da RI-PASSARE nei turni con tool: l'API la esige o risponde
    /// HTTP 400. A livello di MESSAGGIO (una firma per blocco thinking del turno),
    /// gemella del `reasoning` DeepSeek. Il gateway concreto la inoltra solo ad
    /// Anthropic (`GwMessage::thinking_signature` -> `types::LlmMessage`). `None`
    /// per gli altri ruoli/provider. Additivo (`Default`).
    pub thinking_signature: Option<String>,
}

/// Richiesta minimale al gateway LLM.
///
/// `provider`/`model` sono RISOLTI A MONTE dalla routing matrix (regola G: il
/// nodo non li sceglie e non li hardcoda, li riceve gia' decisi). `tools` e'
/// opzionale: assente per un turno puramente testuale.
///
/// I campi estesi sono tutti `Option`/`Default` per non rompere i call site
/// esistenti (i nodi gia' portati costruiscono `LlmRequest` con i soli campi
/// base + `..Default::default()`): un turno minimale resta valido senza
/// valorizzarli.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlmRequest {
    /// Provider risolto dalla routing matrix (es. valore opaco "anthropic").
    pub provider: String,
    /// Modello risolto dalla routing matrix (mai hardcoded qui, regola G).
    pub model: String,
    /// Messaggi della conversazione nel formato minimale.
    pub messages: Vec<LlmMessage>,
    /// Tool dichiarati al modello (schema JSON). `None` = turno senza tool.
    pub tools: Option<Vec<Value>>,
    /// Forza il modello a chiamare un tool (`tool_choice` non-`auto`).
    ///
    /// RISCHIO NOTO (memoria progetto, "Gateway droppava tool_choice"): in una
    /// migrazione passata il gateway PERDEVA il `tool_choice`, neutralizzando il
    /// force-action anti-loop (l'agente descriveva il fix ma non chiamava
    /// `edit_file` -> abort). Per questo `force_tool_choice` e' un campo
    /// ESPLICITO del contratto: l'impl gateway concreta (Fase 6) DEVE onorarlo
    /// end-to-end (`tool_choice` propagato al provider), mai droppato. `None` =
    /// lascia la scelta al modello (`auto`); `Some(true)` = forza un tool;
    /// `Some(false)` = vieta i tool (turno puramente testuale).
    pub force_tool_choice: Option<bool>,
    /// System prompt del turno (forma testuale provider-agnostica). `None` se il
    /// system viaggia gia' come primo `LlmMessage` con `role="system"` (forma
    /// usata da planner/clarify): le due forme sono alternative, l'impl concreta
    /// le normalizza nel payload del provider.
    pub system_text: Option<String>,
    /// Tetto di token di output per il turno (`max_tokens`). `None` = default del
    /// provider risolto a monte.
    pub max_tokens: Option<i64>,
    /// Formato di risposta richiesto al provider nel contratto gateway
    /// provider-agnostico (es. `{"type":"json_object"}`). `None` = nessun
    /// vincolo: il provider usa il proprio default.
    pub response_format: Option<Value>,
    /// Configurazione thinking/reasoning esplicita del turno. `None` lascia al
    /// gateway il comportamento storico DB-driven/provider-specifico.
    pub thinking: Option<ThinkingConfig>,
    /// Metadati per la registrazione usage lato gateway concreto (telemetria
    /// token/costo per run): `run_id` del run corrente. `None` per le chiamate
    /// fuori-run (es. test). Opaco al nodo.
    pub run_id: Option<String>,
    /// Iterazione del run a cui la chiamata appartiene (telemetria usage). `None`
    /// fuori-run.
    pub iteration: Option<i64>,
    /// Intent classificato del turno (telemetria usage / routing osservabile).
    /// `None` se non disponibile. Opaco al nodo.
    pub intent: Option<String>,
    /// Nodo CHIAMANTE della completion (`"executor"` / `"planner"` /
    /// `"reflection"` / `"clarify_expand"`). `None` (Default) per i call site che
    /// non lo valorizzano (test, turni minimali).
    ///
    /// SCOPO (regola L): e' un discriminante OPACO al gateway concreto trasportato
    /// nel payload. L'impl di produzione [`LlmGateway`] (`GatewayLlmAdapter` di
    /// mcp-core) lo usa per risolvere un purpose model quando il modello richiesto
    /// e' vuoto (`purpose_for_empty_model`); il trait [`LlmGateway`] resta invariato
    /// (firma `complete()` non cambia): l'impl decide se guardarlo.
    pub purpose: Option<String>,
}

/// Configurazione thinking provider-agnostica trasportata fino al gateway LLM.
/// Il gateway concreto la traduce nel dialetto del provider (Anthropic thinking,
/// Gemini thinkingConfig, DeepSeek thinking.type, ecc.).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThinkingConfig {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
}

/// Uso/consumo token riportato dal gateway (forma normalizzata cross-provider).
///
/// I campi cache/costo sono `Option`: `None` = il provider/gateway non li ha
/// riportati (compute_turn_cost resta lato gateway concreto, il nodo legge
/// l'usage gia' normalizzato; vedi memoria progetto "Token usage stream + live"
/// per la normalizzazione cross-provider `extract_usage_tokens`). `Eq` rimosso
/// perche' `total_cost_usd` e' un `f64` (non `Eq`); `PartialEq` basta per i test.
///
/// `prompt_tokens` e' il prompt LORDO — il contesto inviato al modello, cache
/// compresa — perche' e' la convenzione unica del sistema
/// (`nexus_gateway::LlmUsage`) ed e' cio' che serve a chi misura: crescita della
/// history, riempimento della finestra, freno anti-runaway. `cache_read_tokens`
/// e `cache_creation_tokens` ne sono il DETTAGLIO, mai addendi: servono al
/// listino, che scorpora al momento di tariffare.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LlmUsage {
    /// Token di prompt LORDI: il contesto inviato, cache compresa.
    pub prompt_tokens: i64,
    /// Token di completion (output).
    pub completion_tokens: i64,
    /// Token totali.
    pub total_tokens: i64,
    /// Token usati per CREARE la cache di prompt (Anthropic
    /// `cache_creation_input_tokens`). `None` se il provider non la espone.
    pub cache_creation_tokens: Option<i64>,
    /// Token LETTI dalla cache di prompt (Anthropic `cache_read_input_tokens`):
    /// risparmio della KV-cache. `None` se il provider non la espone.
    pub cache_read_tokens: Option<i64>,
    /// Token di ragionamento che il provider NON conta in `completion_tokens`
    /// (oggi il solo Google). Non e' un dettaglio come i due campi di cache: e'
    /// un ADDENDO che si paga alla tariffa di output, e chi calcola un costo lo
    /// somma dal punto unico
    /// `nexus_types::token_usage::completion_tokens_billable`.
    ///
    /// Resta fuori da `completion_tokens` — che misura il testo PRODOTTO e ha
    /// consumatori che lo leggono come tale — e fuori da `total_tokens`, che
    /// misura il CONTESTO: il ragionamento consuma il tetto di output del turno
    /// ma non torna nella finestra del turno seguente.
    pub reasoning_tokens: Option<i64>,
    /// Costo del turno in USD calcolato dal gateway (`compute_turn_cost`). `None`
    /// se non calcolato. Il nodo lo legge gia' pronto, non lo calcola.
    pub total_cost_usd: Option<f64>,
}

/// Risposta minimale del gateway LLM.
///
/// I campi estesi sono `Option`/`Vec` con `Default` per non rompere i call site
/// esistenti (i nodi gia' portati costruiscono `LlmResponse` con i soli campi
/// base + `..Default::default()`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlmResponse {
    /// Contenuto testuale prodotto dal modello (vuoto se solo tool_calls).
    pub content: String,
    /// Richieste di tool emesse dal modello (vuoto se turno testuale).
    pub tool_calls: Vec<ToolUse>,
    /// Consumo token normalizzato.
    pub usage: LlmUsage,
    /// Provider EFFETTIVAMENTE usato dal gateway (puo' differire dal `provider`
    /// richiesto: il gateway fa cascade/sticky internamente). Il nodo legge
    /// l'effettivo per la telemetria e per `RunControlStore::set_effective_model`.
    /// `None` = il gateway non l'ha riportato (es. stub di test).
    pub provider_used: Option<String>,
    /// Modello EFFETTIVAMENTE usato dal gateway (vedi `provider_used`). `None` se
    /// non riportato.
    pub model_used: Option<String>,
    /// Blocchi grezzi del contenuto dell'assistente nel formato "anthropic_content"
    /// (testo + `tool_use`), per ricostruire fedelmente il `Message::Ai` con i
    /// blocchi `tool_use` originali. Serve la continuita' `tool_use`/`tool_result`
    /// gia' gestita nel planner (un `tool_result` deve referenziare il `tool_use`
    /// originale). `Vec` vuoto = il gateway ha riportato solo `content`/`tool_calls`
    /// (i nodi ricostruiscono dai campi base).
    pub assistant_content: Vec<Value>,
    /// Motivo di fine turno riportato dal provider (`stop_reason`/`finish_reason`
    /// normalizzato: `end_turn`/`tool_use`/`max_tokens`/...). `None` se non
    /// riportato. Segnale per la chiusura turno (vedi `routing::signals`).
    pub stop_reason: Option<String>,
    /// Ragionamento/pensiero intermedio aggregato dal gateway (reasoning_content
    /// OpenAI-compat, thoughts Gemini, thinking Anthropic). `None` se il provider
    /// non lo riporta. L'executor lo emette come `SseEvent::ThinkingDelta` per
    /// mostrare il pensiero del modello in chat (visibilita' pre-porting).
    pub reasoning: Option<String>,
    /// Firma opaca del blocco `thinking` (Anthropic) del turno, riportata dal
    /// gateway per essere RI-PASSATA nel turno successivo con tool (HTTP 400
    /// senza). Gemella per-messaggio del `reasoning`; `None` per gli altri
    /// provider. Additivo (`Default`).
    pub thinking_signature: Option<String>,
}

/// Astrazione del gateway LLM. mcp-core la implementera' delegando a
/// `nexus-gateway` (catena Fallback DB-driven). I nodi dipendono solo da questo
/// trait, mai dal client concreto.
#[async_trait]
pub trait LlmGateway: Send + Sync {
    /// Esegue una completion. L'I/O (HTTP, retry, cooldown) e' del concreto.
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, PortError>;
}

/// Una chiamata a tool da eseguire (riusa la forma `ToolUse` del canale interno
/// per non duplicare la struttura nome/args/id, regola L).
pub type ToolCall = ToolUse;

/// Esito dell'esecuzione di un tool nel formato minimale richiesto dai nodi.
///
/// COERENZA `exit_code` (regola L, un solo segnale d'esito): un `ToolOutcome`
/// diventa un [`crate::state::ContentBlock::ToolResult`] quando il nodo lo
/// appende ai messaggi (vedi `tool_call_id` -> `tool_use_id`, `content`,
/// `is_error`, `exit_code`). Il campo `exit_code` qui DEVE fluire INVARIATO in
/// quel `ContentBlock::ToolResult` (stesso tipo `Option<i64>`, stessa semantica:
/// `Some(0)` successo, `Some(!=0)` errore di comando, `None` tool non-comando):
/// e' il segnale PRIMARIO letto da
/// [`crate::routing::signals::tool_result_outcome_after`]. Non re-derivare
/// l'esito altrove.
///
/// I campi estesi sono `Option`/`bool` con `Default` per non rompere i call site
/// esistenti.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolOutcome {
    /// Id della `ToolCall` a cui questo esito risponde (round-trip). Diventa
    /// `tool_use_id` nel `ContentBlock::ToolResult`.
    pub tool_call_id: String,
    /// Contenuto del risultato (JSON: stringa o struttura).
    pub content: Value,
    /// `true` se il tool ha fallito (errore applicativo, non infrastrutturale).
    pub is_error: bool,
    /// Exit code STRUTTURATO del tool-comando: `Some(0)` successo, `Some(!=0)`
    /// errore, `None` tool non-comando. DEVE confluire INVARIATO nel campo
    /// `exit_code` di [`crate::state::ContentBlock::ToolResult`] (alimenta
    /// [`crate::routing::signals::tool_result_outcome_after`]).
    pub exit_code: Option<i64>,
    /// `true` se il fallimento e' INFRASTRUTTURALE (ToolRunner/gRPC down, non un
    /// errore applicativo del tool). Mappa il caso "gRPC-down -> degrada a
    /// executor" senza scalare provider (WAVE 2.2: mcp-core NON scala il provider
    /// su un guasto infra). Default `false` (errore applicativo / successo).
    pub is_infrastructure: bool,
    /// Classe sintetica dell'errore (`timeout`/`grpc_unavailable`/`tool_error`/...)
    /// per il mapping diagnostico fine. `None` su successo o quando non
    /// classificato. Opaco al nodo (eco diagnostica).
    pub error_class: Option<String>,
}

/// Astrazione dell'esecutore di tool. mcp-core la implementera' delegando al
/// ToolRunner gRPC.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Esegue una chiamata a tool.
    async fn execute(&self, call: ToolCall) -> Result<ToolOutcome, PortError>;
}

/// Specifica di UN criterio di verifica costruita dal `FinalGateNode`
/// (`no_orphan_imported` / `outputs_exist` / `service_logs_clean` /
/// `run_command`-build). Replica i dict `{type, spec, expected, timeout_s}`
/// costruiti in `final_gate.py:316-377`.
///
/// E' la forma trasportata al sotto-sistema [`CriteriaRunner`]: il nodo NON
/// esegue i criteri (richiederebbero il ToolRunner gRPC + la logica di
/// `criteria_runner._check_*`, sotto-sistema separato come `closure_judge` per
/// il learner). Il nodo costruisce SOLO queste spec (deterministico,
/// golden-abile) e delega l'esecuzione.
///
/// `Serialize`/`Deserialize`: serve a [`crate::nodes::final_gate::FinalGateConfig`]
/// (che incapsula un `Option<CriterionSpec>` come criterio endpoint risolto a
/// monte) per restare serializzabile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CriterionSpec {
    /// Tipo del criterio (`no_orphan_imported`, `outputs_exist`,
    /// `service_logs_clean`, `run_command`).
    pub criterion_type: String,
    /// Parametri del criterio (`spec` Python: staging_dir, command, ...).
    pub spec: Value,
    /// Atteso (`expected` Python: `{mounted:true}`, `{exit_code:0}`, ...).
    pub expected: Value,
    /// Timeout dedicato in secondi (solo il criterio build lo valorizza).
    pub timeout_s: Option<f64>,
}

/// Esito dell'esecuzione di UN criterio. `evidence` e' JSON arbitrario: per il
/// criterio build contiene `output_excerpt`/`exit_code`/`output_total_chars`/
/// `output_truncated`, per gli altri `verdict`/`error`.
#[derive(Debug, Clone, PartialEq)]
pub struct CriterionResult {
    /// Tipo del criterio (eco dello spec).
    pub criterion_type: String,
    /// `true` se il criterio e' passato.
    pub passed: bool,
    /// Evidenza diagnostica (JSON opaco al nodo).
    pub evidence: Value,
}

/// Astrazione del motore di verifica dei criteri generali del final gate.
/// L'implementazione concreta e' in mcp-core (`agent_graph_adapter`), che esegue
/// i singoli `_check_*` sui tool del run.
///
/// Il confine e' pulito: il `FinalGateNode` costruisce le [`CriterionSpec`] e
/// ottiene i [`CriterionResult`].
#[async_trait]
pub trait CriteriaRunner: Send + Sync {
    /// Esegue i criteri nell'ordine dato; un fallimento di un
    /// criterio NON deve propagare un errore: il concreto lo mappa su un
    /// [`CriterionResult`] con `passed=false` + `evidence.error` (parita' col
    /// try/except del Python, `final_gate.py:381-385`). L'errore di porta resta
    /// per un guasto infrastrutturale del runner stesso.
    async fn run(
        &self,
        criteria: Vec<CriterionSpec>,
    ) -> Result<Vec<CriterionResult>, PortError>;
}

/// Richiesta di convocazione del panel di review adversariale (ReviewGate).
/// Il grafo passa solo cio' che sa; il concreto (mcp-core) risolve settings,
/// sizing e sub-run.
#[derive(Debug, Clone)]
pub struct ReviewPanelRequest {
    /// Run per cui la review giudica (= thread del grafo).
    pub run_id: String,
    /// Costo cumulativo gia' speso dal run (stringe il sizing del panel).
    pub cost_spent_usd: f64,
    /// Ciclo di review corrente (1-based): al secondo giro il concreto sa che
    /// e' una RI-review dopo correzione.
    pub cycle: i64,
}

/// Perche' il panel NON e' stato convocato (segnale strutturato, regola M:
/// il nodo decide sul motivo, mai su una stringa).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewSkipReason {
    /// `orchestrator.review_panel_autoconvene_enabled` = false.
    AutoconveneDisabled,
    /// Il run non ha modificato codice: niente da rivedere.
    NoCodeChanges,
    /// Una review e' gia' avvenuta in questo run (direttiva LLM onorata).
    AlreadyReviewed,
    /// Il dimensionamento ha azzerato il panel (budget residuo insufficiente).
    SizedToZero,
    /// Panel convocato ma nessun verdetto valido raccolto.
    NoValidVerdict,
}

/// Esito della porta: panel saltato (con motivo) o convocato (col verdetto
/// composto dal punto unico `decisions::compose_panel_verdict`).
#[derive(Debug, Clone)]
pub enum ReviewPanelReport {
    Skipped(ReviewSkipReason),
    Convened(crate::decisions::PanelOutcome),
}

/// Porta del panel di review adversariale (gemello di [`CriteriaRunner`] per il
/// ReviewGate). Il concreto in mcp-core convoca i sub-run revisori
/// (`convene_review_panel`) e compone il verdetto; il nodo decide SOLO sul
/// [`ReviewPanelReport`]. In shadow/replay il concreto non viene chiamato.
#[async_trait]
pub trait ReviewPanelPort: Send + Sync {
    async fn review(&self, req: ReviewPanelRequest) -> Result<ReviewPanelReport, PortError>;
}

/// Scritture registrate oltre un watermark, con il watermark aggiornato.
///
/// Il watermark e' l'`id` (BIGSERIAL, monotono) dell'ultima scrittura vista, non
/// un istante: due orologi — quello dell'applicazione e quello del DB — non
/// definiscono la stessa finestra, e la domanda "dopo il rimando" deve avere una
/// risposta sola.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteScan {
    /// Massimo `id` scansionato: si riporta al giro successivo come `after`.
    /// Quando non c'e' nulla di nuovo resta uguale all'`after` ricevuto.
    pub watermark: i64,
    /// I fatti della finestra, nell'ordine in cui sono stati registrati. VUOTO
    /// quando `after` era `None` (si voleva solo prendere il watermark).
    pub facts: Vec<crate::decisions::WriteFact>,
}

/// Porta della MISURA del progresso: "cosa e' stato scritto da questo istante in
/// poi?". Controparte fattuale di [`ReviewPanelPort`] — quella porta la
/// convocazione dei giudici, questa il fatto su cui i giudici verrebbero
/// riconvocati.
///
/// CONFINE (regola L): qui SOLO l'I/O (leggere le scritture registrate). Il
/// GIUDIZIO ("questo conta come correzione?") e' del modulo puro
/// [`crate::decisions::correction_progress`], che il nodo interroga sui
/// [`crate::decisions::WriteFact`] ritornati. L'impl NON deve filtrare per hash
/// diversi: se filtrasse, il criterio vivrebbe in due posti — la query SQL e il
/// modulo puro — e una riscrittura identica sparirebbe prima di essere contata
/// come tale, cioe' proprio il caso che la misura esiste per vedere.
///
/// NIENTE FAIL-OPEN INVENTATO (regola G/M). Un guasto di lettura propaga
/// `PortError`: il chiamante lo tratta come "misura non disponibile" e ricade sul
/// comportamento storico (convoca il panel). Un `Ok(vuoto)` di ripiego direbbe
/// "nessuna scrittura", cioe' "nessun progresso", e SOPPRIMEREBBE una review
/// dovuta ogni volta che il DB ha un singhiozzo — il difetto opposto, e piu'
/// grave, di quello che la misura chiude.
#[async_trait]
pub trait MutationProgressPort: Send + Sync {
    /// `after = Some(id)`: i fatti registrati DOPO `id`, piu' il nuovo watermark.
    /// `after = None`: nessuna finestra da esaminare, si vuole solo il watermark
    /// corrente (primo rimando: non c'e' ancora un "prima" con cui confrontare).
    async fn scan_writes(&self, after: Option<i64>) -> Result<WriteScan, PortError>;
}

/// Esito di UN run del verifier da persistere su `nexus_agent_verifier_runs`
/// (`verifier_node._persist_verifier_run`, `verifier_node.py:584-601`). Forma
/// minimale: i campi della INSERT (`run_id`/`todo_id`/`cycle`/`criteria_results`/
/// `passed`/`duration_ms`). `criteria_results` e' JSON arbitrario (la lista dei
/// [`CriterionResult`] serializzata) opaco allo store.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifierRunRecord {
    /// Id del run (= thread_id).
    pub run_id: String,
    /// Id del todo verificato.
    pub todo_id: String,
    /// Ciclo di verifica (1-based).
    pub cycle: i64,
    /// Risultati dei criteri serializzati (JSON: lista `{type, passed, evidence}`).
    pub criteria_results: Value,
    /// `true` se la verifica e' passata.
    pub passed: bool,
    /// Durata dell'esecuzione criteri in millisecondi.
    pub duration_ms: i64,
}

/// Astrazione della persistenza degli esiti del verifier su
/// `nexus_agent_verifier_runs` (`verifier_node._persist_verifier_run`). mcp-core
/// la implementera' con `sqlx` (INSERT best-effort).
///
/// Parita' col Python: la INSERT e' BEST-EFFORT (su errore DB il verifier
/// prosegue, `verifier_node.py:600`). L'impl concreta NON deve propagare un
/// `PortError` per un fallimento dell'INSERT (lo logga e ritorna `Ok(())`); il
/// `PortError` resta per un contratto rotto (mai usato nel flusso normale).
#[async_trait]
pub trait VerifierRunStore: Send + Sync {
    /// Persiste (best-effort) un esito del verifier.
    async fn record(&self, run: VerifierRunRecord) -> Result<(), PortError>;
}

/// Conteggio token del contesto (ADR 0016 D1). Porta SINCRONA e CPU-only
/// (nessun I/O: una BPE in-process, es. tiktoken cl100k in `mcp-token`).
/// Iniettata nell'executor dal wiring in base al setting
/// `agent.context.tokenizer`; assente -> stima char-based storica (fallback
/// deterministico, mai un panico).
pub trait TokenCounter: Send + Sync {
    /// Numero di token del testo.
    fn count(&self, text: &str) -> i64;
}

/// Evento pubblicato verso il frontend chat (sottoinsieme del contratto SSE).
///
/// Solo le varianti che servono ai nodi di questo PR + quelle del contratto
/// gia' note. Allineato al canale SSE prodotto dal brain (`type` + payload). Lo
/// shadow NON emette eventi (l'`EventSink` no-op viene iniettato nel ctx shadow):
/// l'unica fonte di verita' verso l'utente resta il run primario.
#[derive(Debug, Clone, PartialEq)]
pub enum SseEvent {
    /// Delta di ragionamento/streaming testuale.
    ThinkingDelta {
        /// Frammento di testo.
        delta: String,
    },
    /// Meta-step semantico (plan/routing/clarify/fallback/reflection).
    MetaStep {
        /// Tipo dello step.
        kind: String,
        /// Titolo leggibile.
        title: String,
        /// Payload arbitrario.
        payload: Value,
        /// Id di correlazione opzionale: collega lo step a un'entita' esterna
        /// (es. la narrazione sub-agente porta il `subagent_run_id`, cosi' il
        /// frontend raggruppa gli step dello stesso sub-run). `None` per gli
        /// step non correlati (default storico).
        correlation_id: Option<String>,
    },
    /// Consumo token aggiornato (barra contesto + traccia del turno).
    ///
    /// Porta il prompt LORDO piu' il DETTAGLIO di quanta parte l'ha servita la
    /// cache. Chi misura il riempimento della finestra usa `prompt_tokens` e
    /// basta; chi prezza scorpora dal lordo le due quantita' di cache e applica
    /// le tre tariffe. Prima l'evento portava il solo prompt e la traccia
    /// scriveva i token di cache a 0 fisso: la UI non aveva modo di applicare la
    /// tariffa ridotta, e pagava tutto a prezzo pieno di input.
    Usage {
        /// Token di prompt LORDI: il contesto inviato, cioe' il numeratore del
        /// rapporto ctx%.
        prompt_tokens: i64,
        /// Token di completion.
        completion_tokens: i64,
        /// Token del prompt serviti da cache in questo turno, sottoinsieme di
        /// `prompt_tokens` (0 se il provider non ne ha riportati).
        cache_read_tokens: i64,
        /// Token del prompt scritti in cache in questo turno, stessa convenzione.
        cache_creation_tokens: i64,
        /// Token totali del turno (prompt lordo + completion).
        total_tokens: i64,
    },
    /// Il modello ha richiesto un tool.
    ToolUse {
        /// Id della richiesta.
        id: String,
        /// Nome del tool.
        name: String,
        /// Argomenti.
        input: Value,
    },
    /// Risultato di un tool.
    ToolResult {
        /// Id della richiesta a cui risponde.
        tool_call_id: String,
        /// Contenuto del risultato.
        content: Value,
        /// `true` se il tool ha fallito.
        is_error: bool,
    },
    /// Fine del turno corrente (il modello ha terminato la generazione).
    EndTurn,
    /// Fine del run (terminatore dello stream).
    Done,
}

/// Astrazione del canale eventi verso il frontend. `emit` e' SINCRONO e
/// infallibile dal punto di vista del nodo (best-effort: il concreto bufferizza
/// / scarta se non ci sono subscriber). mcp-core la implementera' col canale SSE.
pub trait EventSink: Send + Sync {
    /// Pubblica un evento (best-effort, non blocca il nodo).
    fn emit(&self, ev: SseEvent);
}

/// Riga di piano (`nexus_agent_plans`) nella forma MINIMALE che serve al riuso
/// piano intent/mode-aware del planner (`planner_node.py:84-113`,
/// `todo_store.fetch_plan`). Trasporta SOLO i due campi su cui il planner decide
/// l'invalidazione (`user_intent` / `behavior_mode`): un piano esistente si
/// RIUSA se questi non sono cambiati (campo non-None e divergente -> rigenera).
/// I piani legacy (pre-mig 0328) hanno questi campi a `None` (intent non
/// tracciato): in quel caso il riuso storico e' mantenuto (vedi nota Python).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlanRow {
    /// Intent con cui il piano e' stato creato (`None` per i piani legacy).
    pub user_intent: Option<String>,
    /// Behavior_mode con cui il piano e' stato creato (`None` per i legacy).
    pub behavior_mode: Option<String>,
}

/// Astrazione dell'I/O sui todo del DAG. Confine d'inversione: la LOGICA DAG
/// (selezione, ready layer, discendenti) e' pura e vive in
/// [`crate::decisions::dag_scheduler`] (punto unico, regola L); questo trait
/// isola SOLO l'accesso DB. I nodi (`verifier`/`todo_runner`/`planner`) leggono
/// i todo da qui e delegano la decisione al modulo puro.
///
/// L'implementazione concreta e' `sqlx` su `nexus_agent_todos`, in
/// `mcp-core::agent_graph_adapter::todo_store`.
///
/// INVARIANTE (regola H, bug 2026-06-10): [`list_todos`](TodoStore::list_todos)
/// DEVE restituire `Todo::depends_on` come `Vec`, MAI una stringa `"{...}"`.
/// Lato impl significa il cast `depends_on::text[]` (come `todo_store.list_todos`
/// Python); il tipo Rust `Vec<String>` rende l'invariante non eludibile. Deve
/// inoltre restituire gli elementi GIA' ordinati per `seq` ascendente (le
/// funzioni pure si basano sull'ordine dello slice come tie-break).
#[async_trait]
pub trait TodoStore: Send + Sync {
    /// Tutti i todo del run, ordinati per `seq` ascendente, con `depends_on`
    /// come `Vec` (cast `::text[]`). 1:1 con `todo_store.list_todos`.
    async fn list_todos(&self, run_id: &str) -> Result<Vec<Todo>, PortError>;

    /// Il piano esistente per il run, se presente (1:1 con
    /// `todo_store.fetch_plan`). Usato dal `PlannerNode` per il riuso piano
    /// intent/mode-aware (`planner_node.py:84-113`): `None` = nessun piano
    /// (prima pianificazione del run). Default `Ok(None)` cosi' le impl che non
    /// servono il planner (es. il `TodoRunnerNode`, gia' esistente) non devono
    /// fornirlo; l'impl concreta in mcp-core e lo stub del planner lo
    /// sovrascrivono.
    async fn fetch_plan(&self, _run_id: &str) -> Result<Option<PlanRow>, PortError> {
        Ok(None)
    }

    /// Il todo "attivo": il primo `in_progress`, altrimenti il primo `pending`,
    /// altrimenti `None`. 1:1 con `todo_store.active_todo`. Default fornito sopra
    /// [`list_todos`](TodoStore::list_todos) per non duplicare la selezione.
    async fn active_todo(&self, run_id: &str) -> Result<Option<Todo>, PortError> {
        let todos = self.list_todos(run_id).await?;
        let by_status = |s: TodoStatus| todos.iter().find(|t| t.status == s).cloned();
        Ok(by_status(TodoStatus::InProgress).or_else(|| by_status(TodoStatus::Pending)))
    }

    /// Aggiorna lo status di un todo (UPDATE best-effort). 1:1 con i `_mark` /
    /// `_mark_todo_status` di `dag_scheduler.py` e `verifier_node.py`.
    async fn mark_status(
        &self,
        todo_id: &str,
        status: TodoStatus,
    ) -> Result<(), PortError>;

    /// Reminder testuale dei todo da iniettare nel prompt dell'executor quando la
    /// fase di piano e' attiva (`plan_phase_active`). L'impl concreta legge i
    /// todos del run e rende il testo (lista compatta stato/seq), `None` se non
    /// c'e' un piano attivo o nessun todo. Default `Ok(None)` per non obbligare
    /// gli store che non servono l'executor (es. il `TodoRunnerNode` esistente).
    /// SOLA LETTURA (non scrive).
    async fn build_reminder_text(&self, _run_id: &str) -> Result<Option<String>, PortError> {
        Ok(None)
    }

    /// Incrementa il contatore di iterazioni "viste" per il run (telemetria di
    /// avanzamento del piano). UPDATE best-effort. Default no-op: gli store che
    /// non lo servono non lo sovrascrivono.
    async fn increment_iteration_seen(
        &self,
        _run_id: &str,
    ) -> Result<(), PortError> {
        Ok(())
    }
}

/// Controllo di run condiviso da `executor` e `tool_dispatch` (PUNTO UNICO,
/// regola L): la stessa domanda "il run e' stato superato?" e gli stessi
/// side-effect best-effort (heartbeat, modello effettivo) vivono in UN solo
/// trait, i due nodi delegano qui invece di re-implementare query/UPDATE.
///
/// Relazione con `ctx.cancel` (memoria progetto, "Single run per session" /
/// supersede last-wins): in Rust il segnale di run superato e' GIA' parzialmente
/// coperto dal `CancellationToken` del ctx (l'orchestratore lo cancella quando
/// un nuovo run supera il corrente). `is_superseded` e' il segnale ESPLICITO e
/// POLLABILE complementare: il nodo lo interroga ai checkpoint (inizio
/// iterazione) anche quando il token non e' ancora propagato (es. supersede
/// scritto su DB da un altro processo). Le due fonti convergono; l'impl concreta
/// puo' leggere il flag `superseded`/`supersede_active_runs` su `agent_runs`.
#[async_trait]
pub trait RunControlStore: Send + Sync {
    /// `true` se il run e' stato superato (last-wins) e deve fermarsi. FAIL-OPEN
    /// (regola di sicurezza): un errore di lettura DB ritorna `Ok(false)` (il run
    /// PROSEGUE), mai un `PortError` che lo bloccherebbe per un guasto
    /// infrastrutturale. SOLA LETTURA.
    async fn is_superseded(&self, run_id: &str) -> Result<bool, PortError>;

    /// Heartbeat del run (UPDATE `updated_at` best-effort): segnala che il run e'
    /// vivo (anti-recovery prematuro). Best-effort: l'impl logga e ritorna
    /// `Ok(())` su errore DB.
    async fn heartbeat(&self, _run_id: &str) -> Result<(), PortError>;

    /// Registra il modello EFFETTIVAMENTE usato dal gateway (da
    /// [`LlmResponse::provider_used`]/[`LlmResponse::model_used`]) sul run, per la
    /// telemetria/osservabilita' (la chat mostra il modello reale, non quello
    /// richiesto). Best-effort.
    async fn set_effective_model(
        &self,
        _run_id: &str,
        _provider: &str,
        _model: &str,
    ) -> Result<(), PortError>;
}

/// Persistenza dei singoli step dell'agente su `agent_steps`
/// (modello: [`VerifierRunStore`]). Ogni blocco prodotto in un'iterazione
/// (tool_use, tool_result, testo) e' uno step indicizzato. Confine d'inversione:
/// SOLO l'INSERT, nessuna logica.
///
/// `step_index` deterministico = `iteration * 1000 + idx` (idx = posizione del
/// blocco nell'iterazione): garantisce ordinamento globale stabile senza
/// contatore condiviso. L'impl concreta DEVE usare `ON CONFLICT DO NOTHING`
/// (idempotente sui retry) + guard `untracked_run` (non inserire step per run non
/// tracciati, evita FK orfane).
#[async_trait]
pub trait AgentStepStore: Send + Sync {
    /// Persiste UN blocco di una iterazione. `block` = il blocco emesso
    /// (tool_use/testo, JSON opaco), `result` = l'eventuale tool_result associato
    /// (`None` per i blocchi di testo o quando non ancora disponibile).
    ///
    /// Best-effort come [`VerifierRunStore::record`]: errore DB loggato,
    /// `Ok(())` ritornato (il `PortError` resta per un contratto rotto).
    async fn persist_step(
        &self,
        run_id: &str,
        iteration: i64,
        idx: i64,
        block: Value,
        result: Option<Value>,
    ) -> Result<(), PortError>;
}

/// Persistenza dei meta-step dell'agente su `agent_meta_steps` (plan/routing/
/// clarify/fallback/reflection persistiti per la cronologia, distinti dal canale
/// live SSE).
///
/// SCELTA `MetaStepStore` vs estendere [`EventSink`] (documentata, regola L):
/// resta un TRAIT SEPARATO. `EventSink::emit` e' il canale LIVE verso il
/// frontend — SINCRONO, infallibile, best-effort. La PERSISTENZA DB e' invece
/// una scrittura ASYNC e FALLIBILE: semantica diversa. Fonderle in `EventSink`
/// costringerebbe a un `emit` async fallibile sul canale live, rompendo i call
/// site `emit` esistenti e mescolando due concern (live vs storico). I due trait
/// sono complementari: un meta-step tipicamente si EMETTE (live) e si PERSISTE
/// (storico) nello stesso punto.
#[async_trait]
pub trait MetaStepStore: Send + Sync {
    /// Persiste un meta-step (`meta_step` = JSON `{kind,title,payload}` opaco allo
    /// store). Best-effort: errore DB loggato, `Ok(())`.
    async fn persist_meta_step(&self, meta_step: Value) -> Result<(), PortError>;
}

/// Offload del contesto verso RAG (Qdrant + embeddings) quando un payload e'
/// troppo grande per restare inline nel contesto del modello.
///
/// CONFINE (regola L): la logica head/tail/pointer-size (DECIDERE cosa offloadare
/// e come troncare) e' PURA e NON vive qui — andra' nel modulo
/// `context_reduction` quando servira'. Questo trait espone SOLO l'I/O di
/// offload (scrittura su Qdrant + ritorno del pointer), che e' infrastrutturale.
/// Best-effort con DEGRADO A TRONCAMENTO: se l'offload fallisce (embed/Qdrant
/// down), l'impl ritorna un `PortError` e il chiamante degrada troncando inline
/// (non blocca il run).
#[async_trait]
pub trait ContextOffload: Send + Sync {
    /// Scrive `payload` su RAG e ritorna un POINTER opaco (chiave per il recupero
    /// successivo). Su guasto infrastrutturale ritorna `PortError` (il chiamante
    /// degrada a troncamento).
    ///
    /// `kind` sceglie la collection RAG di destinazione (regola L: la porta resta
    /// agnostica dall'enum `SourceKind` concreto di mcp-core, mappato dall'impl):
    /// [`OffloadKind::ToolResult`] per i tool_result compressi, [`OffloadKind::ChatHistory`]
    /// per gli originali del rolling-summary (recuperabili filtrando per `session_id`).
    /// `session_id`/`project_id` (UUID come stringa, agnostici) abilitano il filtro
    /// di retrieval; `None` = nessun filtro (offload globale). Prima di questa firma
    /// l'offload era vincolato a `ToolResult` senza filtri, quindi il rolling-summary
    /// non poteva essere reso recuperabile per sessione.
    async fn offload_to_rag(
        &self,
        payload: Value,
        kind: OffloadKind,
        session_id: Option<String>,
        project_id: Option<String>,
    ) -> Result<String, PortError>;
}

/// Collection RAG di destinazione dell'offload, agnostica dall'enum `SourceKind`
/// concreto di mcp-core (l'impl la mappa): tiene `nexus-agent-graph` disaccoppiato
/// dall'infrastruttura RAG (regola L, confine d'inversione).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffloadKind {
    /// Contenuto di tool_result compresso, cache del contesto offloadato.
    ToolResult,
    /// Originale della history conversazionale riassunta (rolling-summary),
    /// recuperabile via `search_semantic(source_kinds=[chat_history], session_id)`.
    ChatHistory,
}

/// Rolling-summary della history conversazionale: RIASSUME (non tronca) i
/// messaggi vecchi chiamando un modello economico, per ridurre i token sui run
/// lunghi.
///
/// CONFINE (regola L): la logica PURA (DECIDERE il cutoff, serializzare il
/// prefisso, applicare il riassunto sostituendo il prefisso con un solo
/// messaggio) NON vive qui — sta nel modulo `context_reduction`
/// ([`crate::decisions::context_reduction::select_rolling_summary_cutoff`],
/// `serialize_prefix_for_summary`, `apply_rolling_summary`). Questo trait espone
/// SOLO l'I/O: la chiamata LLM al summarizer (modello da
/// `agent.context.rolling_summary_model`, regola G).
///
/// Best-effort con DEGRADO A HISTORY INVARIATA: se la chiamata fallisce (gateway
/// down, provider in cooldown, modello non risolto) l'impl ritorna `PortError` e
/// il nodo executor degrada lasciando la history invariata (a valle compress e
/// token_brake fanno comunque il loro lavoro). Il guasto del summarizer NON deve
/// MAI rompere il run.
#[async_trait]
pub trait SummaryStore: Send + Sync {
    /// Riassume `text` (il prefisso della history gia' serializzato dal punto
    /// unico puro) chiamando il modello economico, e ritorna il testo del
    /// riassunto. Su guasto LLM ritorna `PortError` (il nodo degrada a history
    /// invariata). Best-effort.
    async fn summarize(&self, text: String) -> Result<String, PortError>;
}

/// Embedding di testo per la compressione SEMANTICA del contesto (continuity-trim):
/// scarta dal prefisso vecchio i messaggi semanticamente IRRILEVANTI al focus del
/// turno, invece del troncamento posizionale.
///
/// CONFINE (regola L): la DECISIONE (coseno vs focus, chi scartare, cap, pairing)
/// e' PURA e vive in [`crate::decisions::context_reduction`]
/// ([`crate::decisions::context_reduction::cosine_similarity`],
/// `select_continuity_trim_candidates`, `decide_continuity_drops`,
/// `apply_continuity_trim`). Questo trait espone SOLO l'I/O: il calcolo del vettore
/// (embedder ONNX in-process, punto unico `NeuralCoreClient::embed_text_with_model`).
///
/// Best-effort: su guasto infra (embedder down) ritorna `PortError` e il nodo
/// degrada = niente continuity-trim, la history resta invariata.
#[async_trait]
pub trait EmbeddingStore: Send + Sync {
    /// Calcola l'embedding di ciascun testo (batch), preservando l'ordine di input.
    /// Su qualunque guasto ritorna `PortError` (il nodo degrada al troncamento
    /// posizionale). Best-effort. `texts` vuoto -> `Ok(vec![])`.
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, PortError>;
}

/// Forma del turno OSSERVATA: quanti token di prompt e di completamento ha
/// mosso l'ultima chiamata del run.
///
/// Serve all'impl per stimare quanto costerebbe la PROSSIMA chiamata su ciascun
/// candidato. Senza, il costo di un modello puo' essere solo quello di listino —
/// e il listino e' cieco alla prompt-cache, che in un loop agentico e' il fattore
/// dominante: il prefisso (system prompt, tool schemas, primi messaggi) e'
/// identico a ogni iterazione, quindi una quota grande e sistematica del prompt
/// costa una frazione. Misurato il 29/07/2026: deepseek 67,0% di hit contro
/// mistral 5,2%, e sullo stesso task mistral e' costato 18 volte tanto.
///
/// E' la forma OSSERVATA e non una stima: `AgentState` la porta gia' dal turno
/// appena concluso (regola M, dai segnali strutturati). Un tipo e non due `i64`
/// sciolti perche' i due numeri si scambiano senza che il compilatore se ne
/// accorga, e scambiati invertono il confronto.
///
/// `Default` = zeri, che l'impl tratta come "forma ignota" e che riporta
/// all'ordine di listino: chi non sa dichiararla non riceve una scelta peggiore
/// di quella di prima.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TurnShape {
    /// Prompt LORDO dell'ultimo turno: comprende i token serviti da cache.
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

impl TurnShape {
    /// `true` se la forma non dice nulla di utile (nessun prompt osservato).
    pub fn e_ignota(&self) -> bool {
        self.prompt_tokens <= 0
    }
}

/// Dalla misura del turno appena concluso. E' la fonte piu' diretta — l'usage
/// che il gateway ha riportato per QUELLA chiamata — e la si preferisce allo
/// stato quando si e' dentro il turno che ha appena fallito.
impl From<&LlmUsage> for TurnShape {
    fn from(u: &LlmUsage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
        }
    }
}

/// Dati di INPUT dell'auto-escalation gia' risolti dall'impl (catena DB + gate
/// cooldown + router cross-provider): tutto cio' che serve a
/// [`crate::decisions::escalation::pick_escalation_model`] per DECIDERE in modo
/// PURO. Il confine d'inversione (regola L): l'I/O (lettura
/// `nexus_model_escalation_chain`, gate ADR 0020, purpose `loop_fallback_default`)
/// vive nell'impl della porta; la SELEZIONE resta nel modulo puro `escalation`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EscalationInputs {
    /// Insieme UNIFICATO dei candidati di escalation ammissibili (catena
    /// intra-provider + cross-provider fusi), ognuno con tier + telemetria, gia'
    /// filtrati per capability/cooldown dall'impl della porta. La SELEZIONE agentica
    /// (salute -> tier -> likelihood, niente indice posizionale ne' split fisso) vive
    /// nel modulo puro [`crate::decisions::escalation::pick_escalation_model`].
    pub candidates: Vec<EscalationCandidate>,
    /// Soglie governance DB-driven (regola G) per salute/likelihood del ranking.
    pub policy: GovernancePolicy,
}

/// Una scelta di proseguimento derivata dal testo dell'assistente (meta_step
/// `next_actions`): `{label, prompt}` (`next_actions.py:11`, contratto frontend).
/// Forma minimale: lo store/sink concreto la serializza nel payload del meta_step
/// (`{"choices": [...]}`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NextActionChoice {
    /// Testo breve del pulsante (max 60 char lato Python).
    pub label: String,
    /// Prompt completo da inviare come messaggio utente proseguendo con la scelta.
    pub prompt: String,
}

/// Astrazione della DERIVAZIONE delle scelte di proseguimento dal testo
/// dell'assistente (`next_actions.derive`, `next_actions.py:451-484`). Confine
/// d'inversione (regola L): la RIMOZIONE deterministica del blocco
/// `<suggested_actions>` dal testo visibile e' PURA e vive in
/// [`crate::decisions::end_turn::strip_suggested_actions`]; QUESTA porta isola SOLO
/// l'I/O della derivazione delle scelte (parse blocco machine-readable -> fallback
/// deterministico "Prossimi passi" -> fallback LLM purpose `choices_extractor`).
///
/// BEST-EFFORT (parita' col try/except py:3401-3402): la derivazione NON deve mai
/// rompere il turno. Su qualunque errore (router giu', provider in cooldown, JSON
/// malformato) l'impl ritorna `Ok(vec![])` (nessuna scelta -> nessun meta_step),
/// MAI un `PortError`. Il blocco `<suggested_actions>` viene rimosso dal nodo a
/// prescindere dall'esito (punto unico deterministico), quindi il testo visibile
/// e' sempre pulito anche quando la derivazione fallisce.
///
/// SOLA LETTURA / nessun side-effect persistente: nessun gate `mode` (il
/// meta_step si persiste/emette a valle via [`MetaStepStore`]/[`EventSink`]).
#[async_trait]
pub trait NextActionsDeriver: Send + Sync {
    /// Deriva le scelte di proseguimento da `cleaned_text` (testo GIA' privo del
    /// blocco `<suggested_actions>`, rimosso a monte dal punto unico
    /// deterministico). Ritorna le scelte (vuoto = nessuna). Best-effort: errore
    /// -> `Ok(vec![])`, mai `PortError` nel flusso normale.
    async fn derive(&self, cleaned_text: &str) -> Result<Vec<NextActionChoice>, PortError>;
}

/// Astrazione della LISTA dei provider AI in cooldown billing/quota (fonte unica:
/// `brain.providers.registry.get_billing_cooldown_snapshot`, `__init__.py:1611-1620`).
/// Confine d'inversione: la DECISIONE fail-fast (gate soglia + messaggio) e' PURA
/// e vive in [`crate::decisions::end_turn::billing_fail_fast_message`]; questa
/// porta isola SOLO la lettura dello snapshot cooldown.
///
/// FAIL-OPEN (sicurezza, parita' col best-effort py:1619): un guasto di lettura
/// NON deve bloccare il run — l'impl ritorna `Ok(vec![])` (nessun provider
/// esausto -> nessun fail-fast, il run prosegue), MAI un `PortError`. La lista
/// DEVE arrivare GIA' ORDINATA (`sorted(snap.keys())`, py:1618) per parita' del
/// messaggio. SOLA LETTURA: nessun gate `mode`.
#[async_trait]
pub trait BillingCooldownPort: Send + Sync {
    /// Provider in cooldown billing/quota, ordinati alfabeticamente. Vuoto se
    /// nessuno. Fail-open: errore -> `Ok(vec![])`, mai `PortError`.
    async fn billing_exhausted_providers(&self) -> Result<Vec<String>, PortError>;
}

/// Esito dello smart-upscale risolto dall'I/O: il modello target promosso (con il
/// suo provider risolto via catalog) + reason diagnostica, oppure `None` se non
/// c'e' un candidato adeguato. Forma minimale: il nodo riassegna `provider`/`model`
/// del turno e logga `reason` (`_smart_upscale_model` + `_provider_from_model`,
/// `helpers.py:2742-2800` / `2719-2739`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpscalePick {
    /// Provider del modello promosso (risolto da `ai_price_catalog`).
    pub provider: String,
    /// Modello promosso (window piu' grande nel tier configurato).
    pub model: String,
    /// Reason diagnostica (`context_overflow:est=...:from_window=...:tier=...`).
    pub reason: String,
    /// Performance tier del modello promosso (il `agent.upscale.target_tier`
    /// configurato, entro cui la selezione ha scelto). Campo STRUTTURATO (regola M):
    /// il chiamante lo scrive in `StateDelta::current_tier` (FIX-A scale-controller)
    /// senza parsare la stringa `reason`. Sempre valorizzato dall'impl (il tier e' il
    /// vincolo di selezione, quindi noto per costruzione).
    pub tier: String,
}

/// Astrazione dell'I/O dello smart-upscale del modello (`_smart_upscale_model`,
/// `helpers.py:2742-2800`). Confine d'inversione (regola L): la DECISIONE di SE
/// tentare l'upscale (`est_tokens >= window*0.9`) + il numero `required` di token
/// sono PURI ([`crate::decisions::end_turn::should_upscale`] /
/// [`crate::decisions::end_turn::upscale_required_tokens`]); questa porta isola
/// l'I/O: il lookup del context window del modello corrente E la SELEZIONE
/// dinamica dal catalog (tier + capability + window >= required) del modello
/// target con provider risolto.
///
/// Tier-based, DB-driven (regola G): nessun nome modello hardcoded; il tier
/// (`agent.upscale.target_tier`) e i flag (`agent.upscale.*`) sono settings letti
/// dall'impl. BEST-EFFORT (parita' col try/except py:2829-2830): un guasto NON
/// deve rompere il turno — l'impl ritorna `Ok(None)` (nessun upscale, prosegue
/// col modello corrente), MAI un `PortError`. SOLA LETTURA: nessun gate `mode`.
#[async_trait]
pub trait ModelUpscalePort: Send + Sync {
    /// Context window (token) del modello corrente (`_model_context_window`,
    /// `helpers.py:3309-3336`). `0` se ignoto: il chiamante salta l'upscale
    /// (`should_upscale` gata `current_window > 0`). Fail-open: errore -> `Ok(0)`.
    async fn context_window(&self, model: &str) -> Result<i64, PortError>;

    /// Seleziona dal catalog un modello con `context_window >= required_tokens`
    /// nel tier configurato (capable per tool use, escluso `agentic_thinking_policy
    /// = 'exclude'`), col provider risolto. `None` se nessun candidato o se il
    /// migliore coincide col modello corrente. Fail-open: errore -> `Ok(None)`.
    async fn select_upscale_model(
        &self,
        current_model: &str,
        required_tokens: i64,
    ) -> Result<Option<UpscalePick>, PortError>;

    /// SELEZIONE BIDIREZIONALE per lo SCALE-CONTROLLER (PR-B3, regola L: NON una
    /// nuova porta `ScaleApplyPort` — il design la vieta — ma un metodo su questa
    /// porta gia' dedicata alla selezione modello-per-tier). Risolve il MIGLIOR
    /// modello agentico SANO del `tier` TARGET (up O down) col `min_context_window`
    /// richiesto (FIX-B: nel downscale = `est_tokens * overhead`, cosi' un tier piu'
    /// basso con finestra insufficiente viene scartato invece di troncare), la
    /// `capability` propagata (mai persa in un downscale) e i `exclude_providers`.
    /// DELEGA al PUNTO UNICO `select_agentic_model` (regola L, che accetta gia'
    /// `min_context_window`): stesso selettore del routing iniziale
    /// (gate cooldown ADR 0020, tool-use, agentic_thinking_policy<>'exclude').
    ///
    /// Ritorna `Some((provider, model))` se un modello del tier soddisfa i vincoli,
    /// `None` se nessun candidato (-> il chiamante ANNULLA il cambio-tier, fail-safe
    /// che mantiene il modello corrente). Fail-open: errore di lettura -> `Ok(None)`,
    /// MAI un `PortError` nel flusso normale.
    async fn select_model_for_tier(
        &self,
        tier: &str,
        min_context_window: i64,
        capability: Option<&str>,
        exclude_providers: &[String],
    ) -> Result<Option<(String, String)>, PortError>;
}

/// Astrazione dell'I/O dell'auto-escalation (catena DB + cooldown + router
/// cross-provider). mcp-core la implementera' leggendo
/// `nexus_model_escalation_chain` (mig 0128), consultando il gate cooldown
/// (ADR 0020, fonte unica) e risolvendo il purpose `loop_fallback_default` dalla
/// routing matrix (regola G). I nodi dipendono solo da questo trait, mai dal DB.
///
/// CONFINE (regola L): qui c'e' SOLO l'I/O che fornisce i dati; la DECISIONE
/// (quale modello promuovere o `None`) e' del modulo puro
/// [`crate::decisions::escalation`], golden-abile in isolamento.
#[async_trait]
pub trait EscalationPort: Send + Sync {
    /// Risolve gli input dell'escalation per il turno corrente: catena
    /// intra-provider di `(provider, model)`, stato cooldown del provider e
    /// candidato cross-provider per `intent`. SOLA LETTURA.
    ///
    /// GOVERNANCE telemetria-aware (opt-in): l'impl PUO' RIORDINARE la `chain` per
    /// PROBABILITA' di successo derivata da telemetria strutturata (punto unico puro
    /// [`crate::decisions::governance::rank_candidates`]) quando il flag di
    /// governance e' ON. Con flag OFF (default) il riordino non avviene:
    /// comportamento bit-identico. La SELEZIONE resta il punto unico puro
    /// [`crate::decisions::escalation::pick_escalation_model`] (invariato).
    ///
    /// FAIL-OPEN (sicurezza): un guasto di lettura (DB/router down) NON deve
    /// bloccare il run — l'impl ritorna `EscalationInputs` "vuoto" (catena vuota,
    /// `provider_in_cooldown=false`, `cross_provider=None`), che fa risolvere la
    /// selezione a `None` (chiusura secca come oggi), MAI un `PortError`. Il
    /// `PortError` resta per un contratto rotto (mai nel flusso normale).
    ///
    /// `turn_shape` = forma OSSERVATA dell'ultimo turno, con cui l'impl stima il
    /// costo della prossima chiamata su ciascun candidato (la prompt-cache rende
    /// il listino un cattivo criterio proprio a contesto grande, che e' il regime
    /// in cui l'escalation scatta). [`TurnShape::default`] = ignota -> ordine di
    /// listino, cioe' il comportamento precedente.
    async fn escalation_inputs(
        &self,
        intent: Option<&str>,
        provider: Option<&str>,
        model: Option<&str>,
        turn_shape: TurnShape,
    ) -> Result<EscalationInputs, PortError>;

    /// FAILOVER cross-provider su provider CADUTO (gateway 500 `PROVIDER_ERROR` /
    /// cooldown runtime): selezione AGENTICA del SOSTITUTO. L'impl enumera TUTTI i
    /// candidati agentici ammissibili (OGNI tier, esclusi i provider gia' provati
    /// `exclude` e quelli in cooldown, ADR 0020), li arricchisce di telemetria
    /// strutturata (regola M) e DELEGA la scelta al modulo puro
    /// [`crate::decisions::escalation::pick_failover_model`] (regola L): salute ->
    /// `likelihood * affinita' di tier`. Il tier del modello CADUTO
    /// (`current_tier`, o risolto dal catalog via `current_provider`/
    /// `current_model`) e' una INDICAZIONE, mai un filtro: nessun pavimento, nessuna
    /// catena posizionale. Accumulando `exclude` ad ogni salto la cascata prova in
    /// sequenza tutti i provider sani disponibili invece di insistere su uno solo.
    ///
    /// `None` SOLO quando nessun provider sano resta (rete davvero esaurita ->
    /// chiusura `Error` onesta). FAIL-OPEN: un guasto di lettura -> `Ok(None)`
    /// (nessun failover, il chiamante chiude come oggi), MAI un `PortError` nel
    /// flusso normale. SOLA LETTURA.
    ///
    /// `cause` = causa TIPIZZATA del fallimento del provider corrente (regola M).
    /// L'impl la usa per il filtro finestra CAUSA-AWARE: solo per
    /// [`ProviderFailureCause::ContextTooLong`] il sostituto deve avere una finestra
    /// >= a quella del caduto (413 = richiesta troppo grande per QUESTA finestra); per
    /// ogni altra causa (EmptyCompletion/Cooldown/Billing/ClientError) la finestra e'
    /// IRRILEVANTE e non va filtrata (altrimenti un empty su un modello a finestra
    /// enorme escluderebbe i sostituti a finestra minore, pur validi).
    async fn failover_provider(
        &self,
        current_provider: Option<&str>,
        current_model: Option<&str>,
        current_tier: Option<&str>,
        cause: ProviderFailureCause,
        exclude: &[String],
    ) -> Result<Option<CrossProviderCandidate>, PortError>;

    /// Il fornitore a cui l'utente ha VINCOLATO questo run ("Forza" nel
    /// composer), se l'ha fatto. `None` = run non vincolato: e' il default, e
    /// tiene invariata ogni implementazione che non conosce i pin.
    ///
    /// NON e' una decisione, ed e' l'unico motivo per cui sta sul trait: la
    /// scelta di quali candidati siano leciti resta INTERA dentro l'adapter
    /// (regola L) — se ne uscisse anche solo un pezzo, i nove rami che chiedono
    /// un candidato a questa porta (sette `escalation_inputs`, due
    /// `failover_provider`) tornerebbero a decidere ognuno per conto suo. Questo
    /// metodo serve a DIRLO: quando il ripiego non c'e', l'esecutore deve poter
    /// scrivere "non l'ho cercato perche' me l'hai vietato" invece di "tutti i
    /// provider hanno fallito", che sarebbe falso e manderebbe l'utente a
    /// cercare un guasto che non esiste.
    fn pinned_provider(&self) -> Option<&str> {
        None
    }
}

/// Astrazione della LETTURA della storia delle domande-chiarimento poste
/// all'utente nella SESSIONE (CROSS-RUN). Chiude il loop email (chat Beaty-Book):
/// `clarify_or_expand` imposta `pending_clarify` -> il grafo va a END -> il turno
/// TERMINA (il run chiude `Completed`) -> il messaggio successivo dell'utente
/// avvia un RUN NUOVO con stato ricostruito da zero. Tenere il detector in
/// `AgentState` NON funziona (azzerato per-run): il conteggio va letto dal DB
/// della sessione, UNA volta all'avvio del run.
///
/// FONTE STRUTTURATA (regola M): i `nexus_agent_meta_steps` `kind='clarify'` dei
/// run della sessione (join `agent_runs.session_id`). L'ESISTENZA di un meta_step
/// `kind='clarify'` E' la dichiarazione strutturata "questo turno ha posto una
/// domanda-chiarimento all'utente" (il turno di clarify chiude `Completed` +
/// `pending_clarify`, NON `blocked_needs_input`: il segnale d'esito dichiarato di
/// QUESTO canale e' il meta_step stesso, non `agent_runs.status`). La DECISIONE di
/// contare deriva da quel segnale + dal payload strutturato; la firma-testo (sha1
/// della domanda normalizzata) e' SOLO l'euristica di loop-detection che decide se
/// e' la STESSA domanda ripetuta (analogo di `name|sha1` per i tool).
///
/// CONFINE (regola L): qui c'e' SOLO la lettura DB; la costruzione della firma e
/// la DECISIONE d'asse (soglia) restano fuori (il conteggio alimenta
/// `AgentState::repeated_clarify_count` -> `ProgressSignals` -> `progress_controller`).
#[async_trait]
pub trait ClarifyHistoryPort: Send + Sync {
    /// Conta i turni PRECEDENTI della sessione che hanno posto una
    /// domanda-chiarimento con la STESSA firma (`current_question_signature`).
    /// Interroga gli ultimi N meta_step `kind='clarify'` dei run della sessione
    /// e conta quelli la cui domanda normalizzata coincide con la firma corrente.
    ///
    /// FAIL-OPEN (sicurezza, come [`EscalationPort`]/[`BillingCooldownPort`]): un
    /// guasto di lettura DB NON deve bloccare l'avvio del run — l'impl ritorna
    /// `Ok(0)` (nessuna ripetizione nota -> asse mai attivo, comportamento
    /// invariato), MAI un `PortError`. Il `PortError` resta per un contratto rotto
    /// (mai nel flusso normale). SOLA LETTURA: nessun gate `mode`; consultata UNA
    /// volta all'avvio del run (fuori dal percorso caldo), il valore e'
    /// checkpointato in stato -> replay-safe.
    async fn repeated_clarify_count(
        &self,
        session_id: uuid::Uuid,
        current_question_signature: &str,
    ) -> Result<i64, PortError>;
}

/// Astrazione del budget CROSS-RUN delle consultazioni del meta-reasoner di
/// recovery-da-stallo, per SESSIONE. Chiude il gap del cap per-run:
/// `extra["stall_moves_used"]` e' checkpointato con lo stato e si AZZERA tra run
/// diversi della stessa sessione (il loop email e' cross-run: 9 run, 6 richieste
/// identiche). Questa porta persiste+conta le consultazioni della SESSIONE cosi'
/// che il cap `agent.stall_recovery.max_moves_per_session` (regola G) sia
/// veramente per-sessione, non per-run.
///
/// MECCANISMO (scelto: il MENO invasivo, documentato): NESSUNA DDL. Il conteggio
/// vive come righe `nexus_agent_meta_steps` con un `kind` dedicato
/// (`stall_budget`), append-and-count per sessione (join `agent_runs.session_id`),
/// stesso pattern gia' usato da [`ClarifyHistoryPort`]. La tabella e' gia' nel DB
/// per-progetto (separazione DB, regola L): il call site risolve il pool.
///
/// CONFINE (regola L): qui SOLO l'I/O (leggi/incrementa il contatore); la
/// DECISIONE (cap raggiunto?) resta nel gate di emissione dell'executor, che
/// somma il per-run (`extra`) al cross-run letto qui e confronta con la soglia DB.
///
/// FAIL-OPEN (sicurezza, come [`EscalationPort`]/[`ClarifyHistoryPort`]): un
/// guasto di lettura NON deve MAI bloccare — [`consultations_in_session`] ritorna
/// `Ok(0)` (budget non esaurito -> il meta-reasoner resta consultabile, degrado
/// verso il comportamento a solo cap per-run), MAI un `PortError`. La scrittura
/// [`record_consultation`] e' best-effort e gata `Real`.
#[async_trait]
pub trait StallBudgetPort: Send + Sync {
    /// Numero di consultazioni del meta-reasoner gia' registrate nella SESSIONE
    /// (cross-run). SOLA LETTURA; consultata all'avvio del gate di emissione.
    /// FAIL-OPEN: guasto DB -> `Ok(0)` (mai bloccare), MAI un `PortError` nel
    /// flusso normale.
    async fn consultations_in_session(&self, session_id: uuid::Uuid) -> Result<i64, PortError>;

    /// Registra UNA consultazione effettiva del meta-reasoner per la sessione
    /// (append). Best-effort: errore DB loggato, `Ok(())` ritornato (il `PortError`
    /// resta per un contratto rotto, mai nel flusso normale).
    async fn record_consultation(
        &self,
        session_id: uuid::Uuid,
    ) -> Result<(), PortError>;
}

/// Contesto STRUTTURATO passato al meta-reasoner di recovery-da-stallo (ADR 0036
/// applicato al recovery). Regola M: SOLO segnali strutturati (assi, contatori,
/// esiti tipizzati), MAI prosa. Costruito deterministicamente dal modulo puro
/// [`crate::decisions::meta_reason`] dai segnali gia' esistenti dell'executor,
/// serializzato in JSON e passato all'LLM (non l'intera history: budget/costo).
///
/// La `work_epoch` (avanza solo su cambi macroscopici: nuovo todo, escalation,
/// bump `repeat_scan_floor`) e' la chiave di idempotenza/replay: lo stesso stallo
/// non riconsulta l'LLM anche se i `tool_result` volatili variano.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StallContext {
    /// Asse di stallo attivo (`exploration`/`signature`/`g1_descriptive`/
    /// `repeated_action`/`resource_reallocation`/`repeated_user_question`).
    pub axis: String,
    /// Etichetta dell'azione ripetuta (comando/tool + argomenti sintetici).
    pub label: Option<String>,
    /// Conteggio delle ripetizioni/occorrenze dell'asse.
    pub count: i64,
    /// L'azione ripetuta e' un edit FALLITO (segnale strutturato exit_code/is_error).
    pub repeated_action_edit_failed: bool,
    /// L'azione ripetuta e' un servizio FALLITO (segnale strutturato).
    pub repeated_action_service_failed: bool,
    /// L'azione ripetuta e' di SOLA LETTURA (idempotente).
    pub repeated_action_read_only: bool,
    /// Il turno e' action-oriented (dal classifier/soglia, non da prosa).
    pub action_oriented: bool,
    /// Escalation gia' effettuate nel run.
    pub escalations: i64,
    /// Budget massimo di escalation (`agent.executor.max_escalations`, regola G).
    pub max_escalations: i64,
    /// L'asse ha gia' ricevuto GUIDE (evita nudge ripetuti).
    pub already_guided: bool,
    /// L'asse ha gia' ricevuto FORCE_DIAGNOSE.
    pub already_diagnosed: bool,
    /// L'asse ha gia' ricevuto un cambio-strategia forzato.
    pub already_strategy_shifted: bool,
    /// E' gia' stata posta una domanda-chiarimento per questo asse/sessione (cap
    /// strutturale anti ri-domanda, chiude il loop email).
    pub already_asked_user: bool,
    /// Intento utente del run (per orientare la strategia; testo utente, non prosa
    /// di sistema da cui dedurre stato).
    pub user_intent: Option<String>,
    /// Coda (tail) delle firme di tool recenti (`name|sha1`).
    pub recent_tool_signatures: Vec<String>,
    /// Esito STRUTTURATO dell'ultimo tool (`tool_result_outcome_after`):
    /// `ok`/`error`/`redaction_rejected`/... — mai il testo grezzo.
    pub last_tool_outcome: Option<String>,
    /// Un tool ha rifiutato l'input per placeholder di redazione (segnale
    /// strutturato, NON `contains("[REDACTED:")`): riconosce il blocco ambientale.
    pub redaction_rejected: bool,
    /// Numero di domande-chiarimento ripetute nella SESSIONE (cross-run, dal
    /// detector `ClarifyHistoryPort`): il segnale che ha condannato il loop email.
    pub repeated_clarify_count: i64,
    /// File modificati nel run (per capire se c'e' stato progresso reale).
    pub modified_files: Vec<String>,
    /// Epoca di lavoro stabile (chiave idempotenza/replay).
    pub work_epoch: i64,
}

/// Mossa strategica prodotta dal meta-reasoner. Enum CHIUSO: il modulo puro
/// [`crate::decisions::meta_reason`] la traduce in una
/// [`crate::decisions::progress_controller::ProgressDecision`] (stessa struct del
/// ramo fisso), NON un vocabolario di intenti parallelo (regola L). `blocker` e'
/// validato contro il vocabolario ADR 0034 dal modulo puro; un valore fuori
/// vocabolario o un enum sconosciuto degrada a [`RecoveryMove::Fallback`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "move", rename_all = "snake_case")]
pub enum RecoveryMove {
    /// Prosegui con una guida assertiva (nudge generato dall'LLM).
    ContinueGuided { nudge: String },
    /// Cambia STRATEGIA (non solo modello): nudge che riorienta l'approccio.
    ShiftStrategy { nudge: String },
    /// Forza la diagnosi della causa radice (leggi l'errore, dichiara la causa).
    ForceDiagnose { nudge: String },
    /// Promuovi il modello (ramo Escalate esistente + `pick_escalation_model`).
    EscalateModel,
    /// Poni all'utente UNA domanda mirata (cap strutturale per-sessione).
    AskUser { question: String },
    /// Dichiara il blocco in modo ONESTO e strutturato (ADR 0034 `task_complete`).
    DeclareBlocked { blocker: String },
    /// Nessuna mossa LLM valida: ricadi sulla gerarchia fissa `pc::decide`
    /// (rete di sicurezza, include ABORT).
    Fallback,
}

/// Pressione del contesto del run (finestra token quasi piena): segnale
/// STRUTTURATO (regola M) derivato dal rapporto `context_tokens_used /
/// context_window_limit` (soglie deterministiche nel modulo puro
/// [`crate::decisions::orchestration_reason`]). Orienta la decisione di
/// orchestrazione (es. decomporre invece di eseguire inline quando il contesto
/// e' sotto pressione). `Default` = [`ContextPressure::Low`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPressure {
    /// Contesto ampio: nessuna pressione.
    #[default]
    Low,
    /// Contesto in avvicinamento al limite: pressione moderata.
    Medium,
    /// Contesto vicino al limite: pressione alta (favorisce la decomposizione).
    High,
}

/// Fase di orchestrazione in cui la porta viene consultata. Enum CHIUSO: la
/// Fase 1 (rollout, piano design v2) usa SOLO [`OrchPhase::PlanEntry`] — la
/// decisione di SE/COME fare la plan-phase all'ingresso del run, in modalita'
/// SEQUENZIALE (nessuna parallelizzazione-che-scrive finche' manca l'isolamento
/// fisico dei sub-run). Le fasi successive (worktree isolati) potranno aggiungere
/// varianti senza toccare i chiamanti esistenti. `Default` = [`OrchPhase::PlanEntry`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchPhase {
    /// Ingresso del run: decidere SE fare la plan-phase e COME procedere
    /// (inline / decompose / delega sequenziale). Unica fase attiva in Fase 1.
    #[default]
    PlanEntry,
}

/// Contesto STRUTTURATO passato al meta-reasoner di ORCHESTRAZIONE (gemello di
/// [`StallContext`], tipi DISGIUNTI: nessun campo condiviso, nessun enum wrapper
/// — regola L). Regola M: SOLO segnali strutturati (fase, intent, contatori,
/// pressione-contesto, guard di delega deterministiche), MAI prosa da cui dedurre
/// stato. Costruito deterministicamente dal modulo puro
/// [`crate::decisions::orchestration_reason::build_orchestration_context`] dai
/// segnali gia' risolti a monte (routing/context_reduction/depth/cost),
/// serializzato in JSON e passato all'LLM (non l'intera history: budget/costo).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OrchestrationContext {
    /// Fase di orchestrazione (Fase 1: sempre [`OrchPhase::PlanEntry`]).
    pub phase: OrchPhase,
    /// Intento utente del run (testo utente per orientare la strategia; NON prosa
    /// di sistema da cui dedurre stato). `None` se non classificato.
    pub user_intent: Option<String>,
    /// Behavior_mode del run (`study`/`confirm`/`automatic`/...): valore opaco
    /// risolto a monte, orienta l'aggressivita' della decomposizione.
    pub behavior_mode: String,
    /// Budget di token disponibile per il run (`agent.*` risolto a monte, regola G).
    pub token_budget: i64,
    /// Complessita' stimata del task (segnale numerico strutturato dal classifier,
    /// NON dedotto da prosa): alto -> favorisce decompose/delega.
    pub task_complexity: i64,
    /// Punteggio agentico del turno (segnale numerico del classifier): quanto il
    /// task richiede piu' passi/tool. Strutturato (regola M).
    pub agentic_score: i64,
    /// Il task e' ambiguo (segnale strutturato dal classifier/clarify): puo'
    /// orientare verso una plan-phase piu' cauta.
    pub is_ambiguous: bool,
    /// Esiste gia' un piano per il run (`todo_store.fetch_plan`): evita di
    /// ripianificare (la decisione di riuso e' del planner; qui e' solo un segnale).
    pub plan_exists: bool,
    /// Token gia' consumati dal contesto del run (`context_reduction`).
    pub context_tokens_used: i64,
    /// Limite della finestra di contesto del modello risolto (regola G, a monte).
    pub context_window_limit: i64,
    /// Lunghezza della history conversazionale (numero messaggi): segnale di
    /// crescita del contesto.
    pub history_len: i64,
    /// Pressione del contesto derivata deterministicamente da used/limit (regola M):
    /// alta -> favorisce la decomposizione per non saturare la finestra.
    pub context_pressure: ContextPressure,
    /// Profondita' di annidamento dei sub-agenti nel run corrente (guard di delega
    /// DETERMINISTICA): oltre una soglia la delega e' vietata (evita ricorsione
    /// incontrollata). La soglia vive nel modulo puro.
    pub subagent_depth: i64,
    /// Costo gia' speso dal run in USD (guard di delega deterministica): oltre il
    /// cap la delega/decomposizione costosa e' vietata.
    pub cost_spent_usd: f64,
    /// Cap di costo del run in USD (`agent.*` risolto a monte, regola G). `0` =
    /// nessun cap configurato.
    pub cost_cap_usd: f64,
    /// La delega a sub-agenti e' VIETATA per questo run (guard deterministica
    /// aggregata: depth oltre soglia / cost oltre cap / policy). Se `true`,
    /// [`crate::decisions::orchestration_reason::validate_orch_move`] rifiuta
    /// [`OrchestrationMove::DelegateSubagents`] -> [`OrchestrationMove::Fallback`].
    pub delegation_forbidden: bool,
    /// L'isolamento fisico dei sub-run paralleli e' DISPONIBILE per questo run
    /// (worktree effimero per-sub-run: fase infra successiva). Guard fisica
    /// anti-race per [`Coordination::ParallelIsolated`]. `#[serde(default)]` per
    /// retrocompat (contesti/checkpoint pre-esistenti non hanno il campo -> `false`,
    /// comportamento invariato). Il valore e' calcolato al call site mcp-core (che
    /// conosce project_root/is_git_repo, non la porta), MAI dedotto qui. In Fase 1
    /// e' hardwired `false` -> [`Coordination::ParallelIsolated`] sempre degradata a
    /// [`Coordination::Sequential`] da `validate_orch_move`.
    #[serde(default)]
    pub isolation_available: bool,
}

/// Blocco di piano proposto dal meta-reasoner di orchestrazione (forma MINIMALE:
/// titolo + descrizione). Il planner concreto lo materializza in
/// `nexus_agent_todos` (blocco successivo del piano); qui trasporta SOLO cio' che
/// serve alla decisione strutturata (regola M). Ordine dei blocchi = ordine di
/// esecuzione sequenziale in Fase 1.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct PlanBlock {
    /// Titolo breve del blocco (etichetta dell'obiettivo).
    pub title: String,
    /// Descrizione dell'obiettivo del blocco (cosa deve produrre, NON come).
    pub description: String,
}

/// Sotto-task da delegare a un sub-agente (forma MINIMALE). `kind` e' il TIPO di
/// sub-agente (segnale strutturato opaco: `coder`/`general`/...), risolto a monte
/// dal wiring; `task_description` e' l'obiettivo del sotto-task (cosa, non come —
/// regola D per i prompt fuori-chat, applicata a valle dal chiamante).
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct SubTask {
    /// Obiettivo del sotto-task (descrizione strutturata dell'esito atteso).
    pub task_description: String,
    /// Tipo di sub-agente a cui delegare (valore opaco risolto a monte).
    pub kind: String,
    /// Aree file (path/prefissi relativi alla root) che il sotto-task DICHIARA di
    /// voler scrivere. Segnale strutturato per la verifica statica di DISGIUNZIONE
    /// (regola M): [`crate::decisions::orchestration_reason::subtasks_are_disjoint`]
    /// ammette [`Coordination::ParallelIsolated`] SOLO se ogni task dichiara almeno
    /// un path e gli scope sono a due a due disgiunti (nessuno che tocca lock/generati
    /// condivisi). Vuoto = scope non dichiarato -> non parallelizzabile. `#[serde(default)]`
    /// per retrocompat (mosse LLM/checkpoint pre-esistenti non hanno il campo).
    #[serde(default)]
    pub write_scope: Vec<String>,
}

/// Modalita' di coordinamento dei sub-task delegati. Enum CHIUSO: in Fase 1 SOLO
/// [`Coordination::Sequential`] e' ammessa da
/// [`crate::decisions::orchestration_reason::validate_orch_move`].
/// [`Coordination::ParallelIsolated`] richiede l'isolamento fisico (worktree
/// per-sub-run, fase infra successiva): finche' `isolation_available=false` la
/// validazione la rifiuta -> [`OrchestrationMove::Fallback`] (anti-race: due
/// sub-run paralleli sulla stessa root si pesterebbero, verificato su
/// `dag_scheduler`). `Default` = [`Coordination::Sequential`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coordination {
    /// Esecuzione SEQUENZIALE dei sub-task (nessun conflitto sulla root condivisa).
    #[default]
    Sequential,
    /// Esecuzione PARALLELA con isolamento fisico (worktree). Ammessa SOLO quando
    /// `isolation_available=true`, valore calcolato a runtime dal chiamante
    /// (`compute_run_isolation_available` in mcp-core: dipende dal progetto e
    /// dalla disponibilita' dei worktree git). Quando e' `false` la validazione
    /// degrada la mossa a `Sequential`.
    ParallelIsolated,
}

/// Mossa di ORCHESTRAZIONE prodotta dal meta-reasoner (gemello di
/// [`RecoveryMove`], tipi DISGIUNTI — regola L). Enum CHIUSO,
/// `#[serde(tag="move")]`: il modulo puro
/// [`crate::decisions::orchestration_reason::validate_orch_move`] deserializza e
/// valida; qualunque forma malformata / `Decompose` senza blocchi /
/// `DelegateSubagents` senza task o vietata / `ParallelIsolated` senza isolamento
/// degrada a [`OrchestrationMove::Fallback`] (rete di sicurezza: l'euristica
/// esistente `is_eligible`/`should_parallelize`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "move", rename_all = "snake_case")]
pub enum OrchestrationMove {
    /// Attiva la plan-phase (pianificazione esplicita). `decompose=true` -> il
    /// piano va scomposto in piu' blocchi; `false` -> plan-phase leggera.
    PlanPhase {
        /// Se il piano deve essere scomposto in blocchi (vs plan-phase leggera).
        decompose: bool,
    },
    /// Esegui il task INLINE (nessuna plan-phase, nessuna decomposizione): il
    /// task e' abbastanza semplice per un singolo flusso.
    RunInline,
    /// Decomponi il run in blocchi di piano SEQUENZIALI (Fase 1). Vuoto -> la
    /// validazione degrada a [`OrchestrationMove::Fallback`].
    Decompose {
        /// Blocchi di piano nell'ordine di esecuzione (sequenziale in Fase 1).
        blocks: Vec<PlanBlock>,
    },
    /// Delega a sub-agenti i sotto-task con la `coordination` indicata. Vuoto o
    /// `delegation_forbidden` o `ParallelIsolated` senza isolamento -> la
    /// validazione degrada a [`OrchestrationMove::Fallback`].
    DelegateSubagents {
        /// Sotto-task da delegare (uno per sub-agente).
        tasks: Vec<SubTask>,
        /// Coordinamento dei sub-task (Fase 1: solo [`Coordination::Sequential`]).
        coordination: Coordination,
    },
    /// Nessuna mossa LLM valida/applicabile: ricadi sull'euristica esistente
    /// (`is_eligible`/`should_parallelize`), rete di sicurezza.
    Fallback,
}

/// Tier di scala del modello: valore opaco che riflette la tassonomia
/// `ai_price_catalog.performance_tier`. Scala a 5 livelli
/// (`light`<`medium`<`high`<`heavy`<`frontier`, mig 0032 estesa): i modelli di
/// fascia alta, prima tutti `heavy`, si distribuiscono ora su high/heavy/frontier
/// (es. gemini-2.5-pro->high, gemini-3-pro->heavy, gpt-5.5/opus-4-8->frontier),
/// cosi' l'escalation e la selezione distinguono il "meglio disponibile". Qui e'
/// un dominio STRUTTURATO (regola M): lo scale-controller ragiona sul TIER astratto,
/// non su un nome modello (regola G). Il tier->modello sano e' risolto A VALLE via
/// `best_model_for_tier`/`select_agentic_model`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleTier {
    /// Modelli piccoli/economici (bassa potenza, bassa latenza/costo).
    Light,
    /// Tier intermedio (default catalog, mig 0032).
    #[default]
    Medium,
    /// Modelli forti ma non di frontiera (es. gemini-2.5-pro, deepseek-v4-pro).
    High,
    /// Modelli molto potenti (es. gemini-3-pro, gpt-5.1/5.2, opus-4-6).
    Heavy,
    /// Modelli di frontiera, il top assoluto (es. gpt-5.5, claude-opus-4-8, fable-5).
    Frontier,
}

impl ScaleTier {
    /// Etichetta canonica del tier, 1:1 con `ai_price_catalog.performance_tier`.
    /// Usata per serializzare/chiave-cache.
    pub fn as_str(&self) -> &'static str {
        match self {
            ScaleTier::Light => "light",
            ScaleTier::Medium => "medium",
            ScaleTier::High => "high",
            ScaleTier::Heavy => "heavy",
            ScaleTier::Frontier => "frontier",
        }
    }

    /// Parsa un tier dal catalog (case-insensitive, trimmed). Valore fuori
    /// vocabolario -> `None` (il chiamante decide il fallback DETERMINISTICO,
    /// tipicamente [`ScaleTier::Medium`], default catalog): niente magic-fallback
    /// nascosto qui (regola G), solo parsing puro.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "light" => Some(ScaleTier::Light),
            "medium" => Some(ScaleTier::Medium),
            "high" => Some(ScaleTier::High),
            "heavy" => Some(ScaleTier::Heavy),
            "frontier" => Some(ScaleTier::Frontier),
            _ => None,
        }
    }

    /// Ordinale del tier per il clamp "1 gradino per epoca"
    /// (light=0/medium=1/high=2/heavy=3/frontier=4).
    pub fn rank(&self) -> i64 {
        match self {
            ScaleTier::Light => 0,
            ScaleTier::Medium => 1,
            ScaleTier::High => 2,
            ScaleTier::Heavy => 3,
            ScaleTier::Frontier => 4,
        }
    }

    /// Inverso di [`rank`]: ricostruisce il tier dal suo ordinale (clampato al
    /// range valido 0..=4). Punto unico usato dal clamp "1 gradino per epoca"
    /// per avanzare/arretrare di un solo gradino su una scala a N livelli.
    pub fn from_rank(rank: i64) -> Self {
        match rank {
            r if r <= 0 => ScaleTier::Light,
            1 => ScaleTier::Medium,
            2 => ScaleTier::High,
            3 => ScaleTier::Heavy,
            _ => ScaleTier::Frontier,
        }
    }
}

/// Contesto STRUTTURATO passato allo SCALE-CONTROLLER (TERZO scope disgiunto della
/// [`MetaReasonerPort`], gemello di [`StallContext`]/[`OrchestrationContext`]: tipi
/// DISGIUNTI, nessun campo condiviso, nessun enum wrapper — regola L). Regola M:
/// SOLO segnali strutturati (tier, contatori, pressione-contesto, streak, cost,
/// capability, guard deterministiche), MAI prosa da cui dedurre stato. Costruito
/// deterministicamente dal modulo puro
/// [`crate::decisions::scale_reason::build_scale_context`] dai segnali gia' risolti
/// dall'executor (nessun I/O al build, FIX-A: `current_tier` letto dallo stato
/// checkpointato, non ricalcolato via DB), serializzato in JSON e passato all'LLM
/// (non l'intera history: budget/costo).
///
/// L'LLM sceglie SOLO il tier astratto + confidence; i 5 gate deterministici di
/// [`crate::decisions::scale_reason::apply_hysteresis`] (l'LLM NON li scavalca) e il
/// tier->modello a valle (PR-B) restano fuori dal suo controllo.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScaleContext {
    /// Tier corrente del run (da `AgentState.current_tier` checkpointato, FIX-A):
    /// il decisore ragiona da qui, non da un lookup a volo.
    pub current_tier: ScaleTier,
    /// Pavimento di tier per l'intent corrente (FIX-D, regola G): il downscale non
    /// scende MAI sotto questo tier. Risolto a monte (I/O nel call site mcp-core),
    /// qui e' un segnale gia' pronto.
    pub intent_tier_floor: ScaleTier,
    /// Intento utente del run (testo utente per orientare la scala; NON prosa di
    /// sistema da cui dedurre stato). `None` se non classificato.
    pub user_intent: Option<String>,
    /// Behavior_mode del run (`study`/`confirm`/`automatic`/...): valore opaco
    /// risolto a monte.
    pub behavior_mode: String,
    /// Iterazioni gia' consumate dal run.
    pub iterations: i64,
    /// Cap di iterazioni del run (`agent.*` risolto a monte, regola G).
    pub iteration_cap: i64,
    /// Coda residua (`iteration_cap - iterations`): gate BREAK-EVEN (FIX costo).
    /// Sotto `min_tail_iters` il controller non si attiva (costo netto zero su run
    /// corti).
    pub tail_headroom: i64,
    /// Complessita' stimata del task (segnale numerico del classifier, non da prosa).
    pub task_complexity_est: i64,
    /// Il task e' CRITICO (derivato da intent/behavior_mode a monte, FIX-D): rafforza
    /// il floor e disincentiva il downscale a prescindere dai segnali di superficie.
    pub task_critical: bool,
    /// Pressione del contesto (RIUSO [`ContextPressure`]): banda stretta `Low`
    /// richiesta per il downscale.
    pub context_pressure: ContextPressure,
    /// Token stimati del contesto corrente (FIX-B): per il vincolo finestra nel
    /// downscale (predicato puro qui, risoluzione reale con `min_context_window`
    /// a valle in PR-B).
    pub est_tokens: i64,
    /// Rapporto headroom finestra (`est_tokens / window_limit`): segnale di
    /// pressione finestra, guard aggiuntiva al downscale.
    pub token_headroom_ratio: f64,
    /// File modificati NEL run rispetto all'ultimo checkpoint (progresso reale
    /// osservato = segnale esplicito, FIX-D): condizione per il downscale pulito.
    pub files_modified_delta: i64,
    /// Todo chiusi nel run (progresso macroscopico, monotono): segnale-trigger e
    /// condizione di downscale.
    pub todos_closed: i64,
    /// Errori accumulati nel run (segnale strutturato exit_code/is_error, regola M).
    pub error_count: i64,
    /// Streak di iterazioni SENZA errori (condizione di downscale pulito, FIX-D).
    pub error_free_streak: i64,
    /// L'azione ripetuta e' un FALLIMENTO strutturato (regola M): vieta il downscale.
    pub repeated_action_failed: bool,
    /// Escalation gia' effettuate nel run: segnale-trigger e guard anti-downscale.
    pub escalations_done: i64,
    /// Un'escalation reattiva ha PINNATO il tier verso l'alto (FIX-E): finche' attivo
    /// il controller puo' solo mantenere o salire, MAI scendere (precedenza stallo).
    pub escalation_lock_active: bool,
    /// Costo gia' speso dal run in USD (guard di scala deterministica).
    pub cost_spent_usd: f64,
    /// Cap di costo del run in USD (`agent.*` risolto a monte, regola G). `0` =
    /// nessun cap configurato.
    pub cost_cap_usd: f64,
    /// Capability richiesta dal run (es. `vision`): propagata al selettore a valle
    /// (mai persa in un downscale). `None` = nessun requisito speciale.
    pub required_capability: Option<String>,
    /// Il run richiede tool-use (sempre `true` per un run agentico): propagato al
    /// selettore a valle.
    pub requires_tool_use: bool,
    /// Turni dall'ultimo cambio-tier (cooldown cambio-tier, gate 3): sotto
    /// `change_cooldown_turns` il controller non cambia tier.
    pub turns_since_change: i64,
    /// Numero di INVERSIONI di direzione sulla stessa coppia di tier (FIX-D
    /// reversal-pin): oltre `max_reversals` si pinna al tier PIU' ALTO e si smette di
    /// consultare l'LLM su quell'asse (anti-oscillazione, gemello di `already_guided`).
    pub reversal_count: i64,

    // ── Segnali di DIMENSIONAMENTO (sizing agentico, mig 0524) ────────────────
    // Tutti `Option` + `skip_serializing_if`: ASSENTI (sizing OFF, o detector che
    // non li popola) -> OMESSI dal JSON serializzato all'LLM -> il flusso TIER resta
    // BIT-IDENTICO (nessun campo extra nella richiesta cambia la decisione tier).
    // Popolati SOLO dal detector quando `agent.scale.sizing_enabled=true` (regola G).
    // Sono gli OCCHI del sizing (regola M): crescita/rumore/progresso strutturati, mai
    // prosa. La DECISIONE (posture Compact/Relax/Hold) e' dell'LLM; la traduzione in
    // soglie concrete e' deterministica (`scale_reason::resolve_sizing_overrides`).
    /// Numero di messaggi nella history conversazionale (segnale di crescita del
    /// contesto). `None` = sizing OFF -> omesso (bit-identico).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_size: Option<i64>,
    /// Tasso di crescita della history: messaggi per iterazione
    /// (`history_size / (iterations+1)`), segnale strutturato di quanto rapidamente
    /// il contesto si espande. `None` = sizing OFF.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_growth_rate: Option<f64>,
    /// Rumore della storia: dimensione (caratteri) del piu' grande tool_result
    /// recente (proxy della distribuzione `tool_result_size_distribution`: un singolo
    /// output enorme che inquina il contesto). `None` = sizing OFF.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result_noise: Option<i64>,
    /// Finestra di contesto EFFETTIVA del modello del turno (post smart-upscale, in
    /// token): denominatore reale della pressione. `None` = sizing OFF / ignota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_window: Option<i64>,
    /// Il run ha compiuto AZIONI PRODUTTIVE recenti (tool non-esplorazione): segnale
    /// che il modello STA PROGREDENDO (per non stringere/escalare un modello che
    /// avanza). `None` = sizing OFF.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_productive: Option<bool>,
    /// Turni dall'ultimo cambio di POSTURE di sizing (cooldown anti-thrash del
    /// sizing, DISTINTO da `turns_since_change` che e' il cooldown TIER). `None` =
    /// sizing OFF.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sizing_turns_since_change: Option<i64>,
}

/// Postura di DIMENSIONAMENTO scelta dall'LLM per lo scale-controller (regola M:
/// l'LLM sceglie SOLO una DIREZIONE bounded, non le soglie numeriche — quelle sono
/// derivate deterministicamente da [`crate::decisions::scale_reason::resolve_sizing_overrides`]
/// proporzionalmente ai segnali). Enum CHIUSO. `Default` = [`SizingPosture::Hold`]
/// (nessun cambio: rete di sicurezza).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizingPosture {
    /// Contesto sotto pressione / storia rumorosa / crescita rapida: comprimi PRIMA
    /// e piu' forte, attiva il rolling-summary, stringi il freno token, alza la
    /// soglia g1-loop se il run progredisce (piu' respiro a un modello che avanza).
    Compact,
    /// Run che fila con finestra ampia: RILASSA la compressione (piu' contesto vivo),
    /// rolling-summary off, allarga il freno token. Piu' fedelta', meno aggressivita'.
    Relax,
    /// Nessun cambio di dimensionamento (rete di sicurezza; degrado di ogni forma
    /// non chiara).
    #[default]
    Hold,
}

/// Aggiustamenti CONCRETI di dimensionamento risolti deterministicamente da una
/// [`SizingPosture`] + i segnali dello [`ScaleContext`] (punto unico
/// [`crate::decisions::scale_reason::resolve_sizing_overrides`], regola L). Ogni
/// campo e' `Option`: `None` = MANTIENI la soglia fissa DB-driven (il merge
/// `effective_*` lascia invariata la config base -> bit-identico). Serializzato e
/// persistito in `extra["scale_sizing"]` dal rientro dell'executor; letto dal blocco
/// di riduzione contesto e dal gate g1-loop del turno successivo (regola M: segnali
/// strutturati, mai prosa). Tutti i valori sono gia' CLAMPATI ai bound di sicurezza
/// dal risolutore: il consumatore li applica senza ulteriori controlli.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SizingOverrides {
    /// Override di `compress_start_iter` (anticipa/posticipa l'inizio compressione).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compress_start_iter: Option<i64>,
    /// Override di `compress_phase_keep_recent` (messaggi recenti preservati per
    /// fase): shrink (Compact) / grow (Relax), clampato al floor di sicurezza.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compress_phase_keep_recent: Option<Vec<i64>>,
    /// Override di `compress_phase_max_chars` (cap del singolo tool_result per fase):
    /// proporzionale al rumore storia.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compress_phase_max_chars: Option<Vec<i64>>,
    /// Override di `token_brake.max_context_ratio` (soglia del freno token),
    /// clampato in `[brake_ratio_min, brake_ratio_max]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_brake_max_context_ratio: Option<f64>,
    /// Forza ON/OFF il rolling-summary (attiva su pressione, disattiva su rilasso).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rolling_summary_enabled: Option<bool>,
    /// Override di `rolling_keep_recent` (prefisso preservato dal rolling-summary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rolling_keep_recent: Option<i64>,
    /// Moltiplicatore della soglia g1-loop (`>1.0` = piu' respiro prima dell'
    /// escalation-da-loop quando il run progredisce; `<1.0` = anticipa se la storia
    /// cresce a vuoto). `None` = 1.0 (bit-identico).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub g1_loop_threshold_mult: Option<f64>,
}

/// Mossa dello SCALE-CONTROLLER prodotta dal meta-reasoner (gemello di
/// [`RecoveryMove`]/[`OrchestrationMove`], tipi DISGIUNTI — regola L). Enum CHIUSO,
/// `#[serde(tag="move")]`: il modulo puro
/// [`crate::decisions::scale_reason::validate_scale_move`] deserializza e valida
/// (tier fuori vocabolario / confidence fuori `[0,1]` / enum sconosciuto ->
/// [`ScaleMove::KeepTier`]); i 5 gate di
/// [`crate::decisions::scale_reason::apply_hysteresis`] applicano l'anti-oscillazione
/// DOPO la validazione (l'LLM NON scavalca il gate). `KeepTier` = nessun cambio
/// (rete di sicurezza: il routing usa comunque sticky, il tier resta).
///
/// [`ScaleMove::AdjustSizing`] e' la terza direzione (mig 0524): NON tocca il tier
/// ma il DIMENSIONAMENTO del motore (soglie context_reduction / token_brake /
/// rolling_summary / g1-loop). Non passa dai 5 gate tier di `apply_hysteresis` ma
/// dal gate dedicato [`crate::decisions::scale_reason::apply_sizing_gate`]
/// (kill-switch `sizing_enabled` + confidenza + cooldown anti-thrash del sizing).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "move", rename_all = "snake_case")]
pub enum ScaleMove {
    /// Mantieni il tier corrente (nessun cambio-modello). Rete di sicurezza: ogni
    /// forma malformata o gate non superato degrada qui.
    KeepTier,
    /// Sali al tier indicato (upscale) con la `confidence` dichiarata dall'LLM.
    UpscaleTo {
        /// Tier target (deve essere > current per essere un upscale valido).
        tier: ScaleTier,
        /// Confidenza LLM `[0,1]`: sotto `min_confidence` degrada a `KeepTier`.
        confidence: f64,
    },
    /// Scendi al tier indicato (downscale) con la `confidence` dichiarata dall'LLM.
    /// Applicabile SOLO in banda-morta stretta e mai sotto `intent_tier_floor` /
    /// `escalation_lock_active` (gate 2, FIX-D/FIX-E).
    DownscaleTo {
        /// Tier target (deve essere < current e >= floor per essere valido).
        tier: ScaleTier,
        /// Confidenza LLM `[0,1]`: sotto `min_confidence` degrada a `KeepTier`.
        confidence: f64,
    },
    /// Aggiusta il DIMENSIONAMENTO del motore (soglie di context_reduction /
    /// token_brake / rolling_summary / compress-start / g1-loop) SENZA cambiare tier.
    /// Terza direzione dello scale-controller (mig 0524): la stessa consultazione
    /// `assess_scale` puo' decidere il TIER (up/down) O il SIZING (postura). L'LLM
    /// sceglie SOLO la [`SizingPosture`] + `confidence`; il traduttore deterministico
    /// [`crate::decisions::scale_reason::resolve_sizing_overrides`] la espande in
    /// [`SizingOverrides`] concreti proporzionali ai segnali (regola M). Gata da
    /// `agent.scale.sizing_enabled` (default OFF -> degrada a `KeepTier`): con
    /// scale ON ma sizing OFF il flusso tier resta BIT-IDENTICO.
    AdjustSizing {
        /// Direzione del dimensionamento scelta dall'LLM (Compact/Relax/Hold).
        posture: SizingPosture,
        /// Confidenza LLM `[0,1]`: sotto `min_confidence` degrada a `KeepTier`.
        confidence: f64,
    },
}

/// Porta UNICA del meta-reasoner LLM (opt-in DB, ADR 0036-style). Espone TRE
/// metodi a TIPI DISGIUNTI (regola L: un solo adapter mcp-core, nessun enum
/// wrapper `MetaMove` che reintrodurrebbe cross-scope leak tra gli scope):
///   - [`MetaReasonerPort::recover`] — recovery-da-stallo:
///     [`StallContext`] -> [`RecoveryMove`] (purpose `stall_recovery`,
///     template `system.stall_recovery.decide`);
///   - [`MetaReasonerPort::orchestrate`] — orchestrazione (plan-phase / decompose
///     / delega): [`OrchestrationContext`] -> [`OrchestrationMove`] (purpose
///     `orchestration_decide`, template `system.orchestration.decide`);
///   - [`MetaReasonerPort::assess_scale`] — scala-tier PRE-CRISI (up/down del tier
///     modello sull'andamento del run): [`ScaleContext`] -> [`ScaleMove`] (purpose
///     `scale_assess`, template `system.scale.assess`). Terzo dominio disgiunto:
///     non recovery-da-stallo, non orchestrazione-di-superstep, ma la POTENZA del
///     modello in modo continuo durante l'esecuzione (PR-A: fondamenta inerti; con
///     `agent.scale.enabled=false` default e nessun nodo/detector non e' mai
///     chiamato -> bit-identico).
///
/// CONFINE (regola L): qui c'e' SOLO l'I/O che consulta l'LLM; la DECISIONE
/// (validazione + traduzione) e' dei moduli puri [`crate::decisions::meta_reason`]
/// (recovery) e [`crate::decisions::orchestration_reason`] (orchestrazione),
/// golden-abili in isolamento.
///
/// A differenza delle porte SOLA-LETTURA, questi metodi consultano l'LLM:
/// kill-switch OFF / purpose `NotFound` -> `Ok(None)` (degrado legittimo alla
/// gerarchia/euristica fissa, opt-in); DB-down / provider indisponibile ->
/// `Err(PortError::ProviderUnavailable)` (MAI `Ok(None)` mascherante, regola G).
#[async_trait]
pub trait MetaReasonerPort: Send + Sync {
    /// Consulta il meta-reasoner per il contesto di stallo `ctx`.
    async fn recover(
        &self,
        ctx: StallContext,
    ) -> Result<Option<RecoveryMove>, PortError>;

    /// Consulta il meta-reasoner per la decisione di ORCHESTRAZIONE (plan-phase /
    /// decompose / delega) sul contesto `ctx`. Stesso contratto/degrado di
    /// [`MetaReasonerPort::recover`] ma su tipi DISGIUNTI:
    /// [`OrchestrationContext`] -> [`OrchestrationMove`]. `Ok(None)` = degrado
    /// legittimo all'euristica esistente (`is_eligible`/`should_parallelize`).
    async fn orchestrate(
        &self,
        ctx: OrchestrationContext,
    ) -> Result<Option<OrchestrationMove>, PortError>;

    /// Consulta lo SCALE-CONTROLLER per la scala-tier del modello (up/down
    /// PRE-CRISI) sul contesto `ctx`. TERZO scope disgiunto (regola L):
    /// [`ScaleContext`] -> [`ScaleMove`], STESSO contratto/degrado di
    /// [`MetaReasonerPort::recover`]/[`MetaReasonerPort::orchestrate`].
    ///
    /// Consulta l'LLM (kill-switch `agent.scale.enabled` non truthy / purpose
    /// `scale_assess` NotFound -> `Ok(None)` = degrado legittimo; DB-down /
    /// provider indisponibile -> `Err(PortError::ProviderUnavailable)`, MAI
    /// `Ok(None)` mascherante — regola G).
    ///
    /// Lo consuma il nodo `ScaleControl`, raggiunto quando l'executor emette
    /// `ScaleReason`; su `Ok(None)` il nodo non persiste alcuna mossa.
    async fn assess_scale(
        &self,
        ctx: ScaleContext,
    ) -> Result<Option<ScaleMove>, PortError>;

    /// Consulta il supervisore worker (`automation.supervisor_monitoring`) sul
    /// contesto `ctx`. QUARTO scope disgiunto (regola L):
    /// [`SupervisorContext`] -> [`crate::decisions::supervisor::SupervisorDecision`].
    ///
    /// Consulta l'LLM (purpose `supervisor_monitoring`, regola G). Guasto
    /// infrastrutturale -> `Err(PortError::ProviderUnavailable)`.
    async fn supervise(
        &self,
        ctx: SupervisorContext,
    ) -> Result<Option<crate::decisions::supervisor::SupervisorDecision>, PortError>;
}

/// Payload per il template `automation.supervisor_monitoring` (placeholder
/// `{{task}}`, `{{steps_summary}}`, `{{anomaly_block}}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorContext {
    pub task: String,
    pub steps_summary: String,
    pub anomaly_block: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_error_code_recuperabile_solo_in_whitelist() {
        let codes = vec![
            "invalid_argument".to_string(),
            "thought_signature".to_string(),
        ];
        // Code in whitelist (case-insensitive, trim) -> recuperabile.
        assert!(client_error_code_is_recoverable(Some("invalid_argument"), &codes));
        assert!(client_error_code_is_recoverable(Some("  INVALID_ARGUMENT "), &codes));
        // Code di history CONDIVISA (fuori whitelist) -> NON recuperabile (f0ad0337).
        assert!(!client_error_code_is_recoverable(
            Some("invalid_request_message_order"),
            &codes
        ));
        // Code assente/vuoto -> conservativo, NON recuperabile (nessun failover cieco).
        assert!(!client_error_code_is_recoverable(None, &codes));
        assert!(!client_error_code_is_recoverable(Some(""), &codes));
    }

    #[test]
    fn allows_failover_client_error_solo_se_code_recuperabile() {
        let codes = vec!["invalid_argument".to_string()];
        // Non-ClientError -> failover sempre consentito (comportamento storico).
        assert!(ProviderUnavailableInfo::new(ProviderFailureCause::EmptyCompletion, "vuoto")
            .allows_cross_provider_failover(&codes));
        assert!(ProviderUnavailableInfo::new(ProviderFailureCause::Billing, "credito")
            .allows_cross_provider_failover(&codes));
        // ContextTooLong (413 request_too_large): cross-provider recuperabile (un
        // provider a finestra/limite piu' grande accetta) -> failover consentito,
        // NON serve un code in whitelist (non e' ClientError).
        assert!(ProviderUnavailableInfo::new(ProviderFailureCause::ContextTooLong, "413")
            .allows_cross_provider_failover(&codes));
        // ClientError con code recuperabile (google) -> failover consentito.
        assert!(ProviderUnavailableInfo::new(ProviderFailureCause::ClientError, "400")
            .with_code(Some("invalid_argument".to_string()))
            .allows_cross_provider_failover(&codes));
        // ClientError con code di history condivisa (mistral) -> niente failover.
        assert!(!ProviderUnavailableInfo::new(ProviderFailureCause::ClientError, "400")
            .with_code(Some("invalid_request_message_order".to_string()))
            .allows_cross_provider_failover(&codes));
        // ClientError senza code -> niente failover (conservativo).
        assert!(!ProviderUnavailableInfo::new(ProviderFailureCause::ClientError, "400")
            .allows_cross_provider_failover(&codes));
    }
}
