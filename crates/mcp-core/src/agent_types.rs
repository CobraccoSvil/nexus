//! Tipi pubblici condivisi del loop agente e helper DB.
//!
//! Estratto da `agent_loop.rs` durante la Fase 4 del refactor Nexus: il loop
//! vero e proprio e' ora nel brain LangGraph (Python), ma questi tipi (step,
//! run result, eventi broadcast, helper DB, ecc.) sono ancora consumati dal
//! ponte `agent_turn_setup` e dall'SSE del frontend.

use serde::{Deserialize, Serialize};
use std::str::FromStr;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// SupervisorMode — modalità di supervisione AI del worker
// ---------------------------------------------------------------------------

/// Modalità di supervisione del processo agente.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorMode {
    /// Nessuna supervisione (default)
    #[default]
    None,
    /// Il supervisor viene chiamato solo quando rileva anomalie:
    /// loop, errori ripetuti, step count > 20 (Modalità C — più economica)
    Anomaly,
    /// Il supervisor controlla ogni N iterazioni (Modalità A)
    #[serde(rename = "interleaved")]
    Interleaved,
    /// Il supervisor controlla dopo ogni iterazione (Modalità B — più precisa)
    Continuous,
}


impl FromStr for SupervisorMode {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "anomaly" => Self::Anomaly,
            "interleaved" => Self::Interleaved,
            "continuous" => Self::Continuous,
            _ => Self::None,
        })
    }
}

impl SupervisorMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Anomaly => "anomaly",
            Self::Interleaved => "interleaved",
            Self::Continuous => "continuous",
        }
    }

    /// Parsa identificatore canonico supervisor (none/anomaly/interleaved/continuous).
    pub fn try_parse(value: &str) -> Result<Self, &'static str> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "none" => Ok(Self::None),
            "anomaly" => Ok(Self::Anomaly),
            "interleaved" => Ok(Self::Interleaved),
            "continuous" => Ok(Self::Continuous),
            _ => Err("invalid supervisor_mode"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tipi pubblici dell'agent run
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStep {
    pub run_id: String,
    pub step_index: u32,
    pub tool_name: String,
    pub tool_input: Value,
    pub tool_result: Option<String>,
    pub status: AgentStepStatus,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStepStatus {
    Running,
    Completed,
    Failed,
    AwaitingConfirmation,
    /// Il run e' SOSPESO in attesa del fan-in dei sub-run background (fan-in
    /// deterministico, Fase D): gemello di `AwaitingConfirmation` ma il resume e'
    /// innescato dal completamento dell'ultimo figlio, non da un'azione utente.
    AwaitingSubagents,
    Skipped,
    /// Tutti i provider configurati sono in cooldown / non disponibili.
    /// Stato emesso da `chat_messages.rs::spawn_agent_run` quando il
    /// routing ritorna `no_capable_provider=true`. La UI deve mostrare un
    /// banner di alert (vedi `chat-panel.tsx` gestione `provider_unavailable`)
    /// e NON avviare il run agente.
    ProviderUnavailable,
}

impl AgentStepStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::AwaitingSubagents => "awaiting_subagents",
            Self::Skipped => "skipped",
            Self::ProviderUnavailable => "provider_unavailable",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPendingAction {
    pub index: usize,
    pub tool_name: String,
    pub tool_input: Value,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunStatus {
    Running,
    Completed,
    AwaitingConfirmation,
    /// Il run e' SOSPESO in attesa del fan-in dei sub-run background (fan-in
    /// deterministico, Fase D). Gemello di `AwaitingConfirmation` (run SOSPESO,
    /// non-terminale, resumibile), ma il resume e' innescato dal completamento
    /// dell'ULTIMO figlio background (via `subagent_fanin_resume_queue`, mig
    /// 0542), non da un'azione utente. Distinto da `AwaitingConfirmation` cosi'
    /// il worker fan-in seleziona SOLO i propri run (CAS su questo status).
    AwaitingSubagents,
    Failed,
    TimedOut,
    Cancelled,
    /// Il brain ha rilevato un loop (stessa tool call ripetuta >= LOOP_THRESHOLD volte)
    /// e tutti i tentativi di escalation intra-provider e cross-provider sono esauriti.
    LoopAborted,
    /// Nessun provider disponibile: tutti in cooldown (billing_error / rate_limit)
    /// o non configurati. Il turno non ha potuto essere elaborato.
    ProviderUnavailable,
    // ── Esiti terminali canonici (macchina a stati deterministica, ADR terminazione) ──
    // Emessi dal punto unico `outcome_node` del brain via `nexus_run_outcome` e mappati
    // qui da `derive_status` (agent_turn_setup.rs). Mutuamente esclusivi: un run d'azione
    // termina SEMPRE in uno di questi tre, mai in una domanda di disambiguazione.
    /// Il task e' completato E verificato: final_gate passato (o non applicabile),
    /// almeno un'azione produttiva applicata, risposta finale non vuota.
    CompletedVerified,
    /// Il task e' stato SVOLTO ma la verifica tecnica NON e' stata eseguita: il
    /// final gate e' entrato senza un profilo di verifica dell'ambiente (ADR 0036),
    /// quindi nessun comando di build/test reale ha confermato l'esito. E' un
    /// successo ONESTO ma non verificato, distinto da `CompletedVerified` (verifica
    /// eseguita e passata) e da `Completed` (verifica non applicabile). Segnale
    /// strutturato dal gate (regola M), mai dedotto dal testo.
    CompletedUnverified,
    /// Il task NON e' completato: il run e' stato chiuso da abort anti-loop, cap o budget,
    /// MA l'agente ha prodotto una diagnosi (perche' e' fallito, cosa lo blocca, prossimo
    /// passo). E' un esito definitivo e onesto, non un errore infrastrutturale.
    FailedDiagnosed,
    /// Il run e' bloccato e richiede input esterno reale (segreto/credenziale mancante,
    /// permesso non disponibile, servizio offline) OPPURE la conferma di un'azione
    /// irreversibile (governata da automation_mode) OPPURE la risposta a una
    /// domanda di chiarimento che il turno ha effettivamente POSTO
    /// (`pending_clarify`, dal 18/08/2026).
    ///
    /// Il MODELLO non lo dichiara mai per ambiguita' di intent: `task_complete`
    /// con `outcome=needs_input` non e' il modo di scaricare un compito ambiguo
    /// che va risolto lavorando. Il terzo caso ha un produttore diverso — un
    /// segnale strutturato del grafo, dopo che il run si e' DAVVERO fermato in
    /// attesa — e li' l'alternativa era dichiarare «completato con successo» un
    /// turno a zero iterazioni.
    BlockedNeedsInput,
}

/// Stati di un run "attivo su una sessione": esegue (`running`) o e' sospeso ma
/// VIVO e resumibile via checkpoint (`awaiting_confirmation` HITL,
/// `awaiting_subagents` fan-in Fase D). PUNTO UNICO (regola L): ogni logica che
/// decide "c'e' un run che occupa la sessione", "il checkpoint va conservato",
/// "il run va superato/cancellato o atteso" deriva da qui, mai re-inlinando la
/// lista. Un nuovo stato sospeso-vivo si aggiunge QUI, una volta sola.
pub const ACTIVE_RUN_STATUSES: [&str; 3] =
    ["running", "awaiting_confirmation", "awaiting_subagents"];

/// Frammento SQL della lista [`ACTIVE_RUN_STATUSES`] per clausole `IN (...)` /
/// `NOT IN (...)`. Const senza input utente: interpolazione sicura. La coerenza
/// con l'array e' verificata dal test `active_run_status_sql_coerente_con_array`.
pub const ACTIVE_RUN_STATUS_SQL: &str = "'running', 'awaiting_confirmation', 'awaiting_subagents'";

/// `true` se lo stato (stringa DB) e' un run attivo/sospeso-vivo su una sessione.
/// Per i match-arm Rust che non usano SQL (es. loop di attesa della remediation).
pub fn is_active_run_status(status: &str) -> bool {
    ACTIVE_RUN_STATUSES.contains(&status)
}

impl AgentRunStatus {
    /// Stringa snake_case persistita in `agent_runs.status`. Punto unico (regola L):
    /// usato sia da `finalize_agent_run` sia dall'update inline in `agent_run.rs`,
    /// che prima duplicavano lo stesso match.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::AwaitingSubagents => "awaiting_subagents",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
            Self::LoopAborted => "loop_aborted",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::CompletedVerified => "completed_verified",
            Self::CompletedUnverified => "completed_unverified",
            Self::FailedDiagnosed => "failed_diagnosed",
            Self::BlockedNeedsInput => "blocked_needs_input",
        }
    }

    /// Gli stati NON terminali, nella forma in cui vivono in `agent_runs.status`.
    ///
    /// Gemello di [`Self::is_terminal`] per i call site SQL (regola L): una query
    /// che deve selezionare "run ormai conclusi" filtra su `status <> ALL(...)` di
    /// QUESTA lista, invece di enumerare i terminali. La lista dei terminali e'
    /// APERTA — cresce a ogni esito canonico nuovo (`failed_diagnosed`,
    /// `completed_verified`, ...) — e ricopiarla in SQL significa dimenticarne uno
    /// al prossimo che si aggiunge; quella dei non-terminali e' chiusa e piccola.
    /// `interrupted` (scritto dal cleanup di startup) NON e' qui: e' terminale,
    /// coerentemente con `from_db_str` che lo mappa su `Cancelled`.
    ///
    /// Le stringhe NON sono ricopiate: derivano da [`Self::as_str`], che resta
    /// l'unico posto dove vive la forma persistita di uno stato. La coerenza con
    /// `is_terminal` non e' affidata alla buona volonta': la verifica
    /// `stati_non_terminali_db_coerente_con_is_terminal`.
    pub fn stati_non_terminali_db() -> [&'static str; 3] {
        [
            Self::Running.as_str(),
            Self::AwaitingConfirmation.as_str(),
            Self::AwaitingSubagents.as_str(),
        ]
    }

    /// `true` se il run e' terminato con successo (con o senza verifica E2E).
    /// Punto unico della semantica "run riuscito": i call site usano questo invece
    /// di `matches!(status, Completed)`, cosi' l'esito verificato non viene perso.
    /// `CompletedUnverified` E' un successo (il lavoro c'e'): la mancata verifica
    /// e' un'annotazione onesta, non un fallimento -> non deve rompere i flussi
    /// che dipendono da `is_success` (billing, learning, KB).
    pub fn is_success(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::CompletedVerified | Self::CompletedUnverified
        )
    }

    /// Parsing inverso di `as_str` (punto unico, regola L): da stringa
    /// persistita in `agent_runs.status` all'enum. `interrupted` (scritto dal
    /// cleanup di startup sui run rimasti `running` dopo un crash) e' terminale
    /// e viene mappato a `Cancelled` (semanticamente: interrotto, non riprende).
    /// Stringa ignota -> `Running` conservativo: chi interroga `is_terminal`
    /// continua ad attendere invece di chiudere un run di stato non riconosciuto.
    pub fn from_db_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "running" => Self::Running,
            "completed" => Self::Completed,
            "awaiting_confirmation" => Self::AwaitingConfirmation,
            "awaiting_subagents" => Self::AwaitingSubagents,
            "failed" => Self::Failed,
            "timed_out" => Self::TimedOut,
            "cancelled" | "interrupted" => Self::Cancelled,
            "loop_aborted" => Self::LoopAborted,
            "provider_unavailable" => Self::ProviderUnavailable,
            "completed_verified" => Self::CompletedVerified,
            "completed_unverified" => Self::CompletedUnverified,
            "failed_diagnosed" => Self::FailedDiagnosed,
            "blocked_needs_input" => Self::BlockedNeedsInput,
            _ => Self::Running,
        }
    }

    /// `true` se il run e' in uno stato TERMINALE (concluso, non riprendera' da
    /// solo). Punto unico (regola L): sostituisce i `matches!` inline sparsi che
    /// dimenticavano gli esiti canonici nuovi (`failed_diagnosed`,
    /// `completed_verified`) -> un run chiuso con la "determinazione certa" non
    /// veniva riconosciuto come terminato nel replay/recovery SSE.
    ///
    /// NON terminali: `Running` (in corso) e i due stati "in pausa che attendono
    /// input" — `AwaitingConfirmation` e `BlockedNeedsInput`: il run non e'
    /// concluso, attende un'azione dell'utente, quindi il client deve restare in
    /// ascolto / mostrare la richiesta, non considerarlo finito.
    pub fn is_terminal(&self) -> bool {
        // BlockedNeedsInput e' TERMINALE (ADR 0034): il run e' CONCLUSO con la
        // dichiarazione onesta "serve input umano"; il prossimo messaggio utente
        // crea un NUOVO run (nessun meccanismo di resume esiste per questo stato,
        // a differenza di AwaitingConfirmation che e' un run SOSPESO con resume
        // HITL). Da non-terminale produceva run appesi per sempre: mai purgati
        // dalla retention, replay senza agent_final, frontend che forzava
        // "failed" dopo timeout con warning fuorviante.
        // AwaitingSubagents (fan-in, Fase D) e' NON-terminale come
        // AwaitingConfirmation: il run e' SOSPESO, riprende al completamento
        // dell'ultimo figlio background (worker fan-in), quindi il client deve
        // restare in ascolto, non considerarlo finito.
        !matches!(
            self,
            Self::Running | Self::AwaitingConfirmation | Self::AwaitingSubagents
        )
    }
}

/// Causa di un completamento "hollow" (l'agente dichiara di aver finito senza
/// aver prodotto nulla di verificabile). Sono cause CUMULABILI: un turno puo'
/// averle entrambe, e l'ordine di `BTreeSet` segue la dichiarazione qui sotto.
///
/// Identificatori canonici inglesi sul wire (regola N); il vocabolario STORICO
/// della colonna `nexus_provider_empty_responses.kind` (mig 0291) resta invece
/// maiuscolo ed e' composto da [`hollow_kind_for_db`]: la colonna ha righe
/// scritte da anni, e cambiarne il vocabolario renderebbe incomparabili le vecchie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HollowCause {
    /// Nessun corpo di risposta: il turno ha chiuso muto.
    EmptyAnswer,
    /// Nessun tool invocato, pur avendone di disponibili.
    NoTools,
}

impl HollowCause {
    /// Vocabolario della colonna `nexus_provider_empty_responses.kind`.
    pub fn as_db_kind(self) -> &'static str {
        match self {
            Self::EmptyAnswer => "EMPTY_ANSWER",
            Self::NoTools => "NO_TOOLS",
        }
    }
}

/// Testo del `kind` per la colonna storica `nexus_provider_empty_responses.kind`
/// (mig 0291), COMPOSTO dalle cause tipizzate: `"EMPTY_ANSWER+NO_TOOLS"` per due
/// cause, `"UNKNOWN"` per l'insieme vuoto. E' l'unico punto in cui quelle cause
/// tornano a essere testo, e solo perche' la colonna e' un registro diagnostico
/// gia' popolato: nessun consumatore decide su questa stringa.
pub fn hollow_kind_for_db(causes: &std::collections::BTreeSet<HollowCause>) -> String {
    if causes.is_empty() {
        return "UNKNOWN".to_string();
    }
    causes
        .iter()
        .map(|c| c.as_db_kind())
        .collect::<Vec<_>>()
        .join("+")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunResult {
    pub run_id: String,
    pub status: AgentRunStatus,
    pub steps: Vec<AgentStep>,
    pub pending_actions: Vec<AgentPendingAction>,
    pub final_answer: Option<String>,
    pub provider: String,
    pub model: String,
    pub iteration_count: u32,
    /// `true` se il Nexus Q-Learning router ha sovrascritto provider/model
    /// e iniettato il system prompt per questo run.
    pub nexus_override_applied: bool,
    /// AgentType suggerito dal Q-Learning router (es. `"Coder"`, `"Architect"`).
    /// `None` se il bridge non era disponibile o non ha prodotto una decisione.
    pub nexus_agent_type: Option<String>,
    /// Q-value del router per questa decisione. `None` se assente.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nexus_q_value: Option<f32>,
    /// Avviso privacy per provider non-EU/non-locali. Mostrato prima della risposta.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_privacy_notice: Option<String>,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub total_cost: f64,
    /// Token di prompt dell'ULTIMA iterazione dell'agent loop.
    /// Usato dal frontend per calcolare il context ratio (% di occupazione
    /// della context_window del modello). `prompt_tokens` resta il valore
    /// cumulativo di tutte le iterazioni, corretto per il billing.
    ///
    /// E' il prompt LORDO come tutti i conteggi di prompt del sistema: la
    /// finestra di contesto la occupa tutto il prompt inviato, compresi i token
    /// che il provider ha servito dalla propria cache (che risparmiano DENARO,
    /// non SPAZIO).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_prompt_tokens: Option<u32>,
    /// Classe errore propagata dal brain (es. "billing_error", "rate_limit",
    /// "overloaded", "provider_error"). Permette al chiamante in chat_messages.rs
    /// di decidere se ritentare con altro provider e di applicare il cooldown
    /// corretto (lungo per billing, breve per transient 5xx/429).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<String>,
    /// Segnale STRUTTURATO gemello di `forced_close_unverified` (regola M): `true`
    /// quando l'ULTIMO turno del grafo ha chiuso perche' il gateway LLM e' fallito
    /// (`NativeRunOutcome::provider_error_close`, che a sua volta viene da
    /// `AgentState::provider_error_close`). Sopravvive alla riscrittura di
    /// `stop_reason`, cosi' il path RESUME (`canonical_run_status`, che rilegge
    /// questo `AgentRunResult` gia' persistito) non deve piu' rileggere il
    /// PREFISSO testuale della `final_answer` per saperlo. Default `false` per i
    /// run storici (persistiti prima di questo campo): `is_provider_error_answer`
    /// resta il fallback per loro.
    #[serde(default)]
    pub provider_error_close: bool,
    /// Stop reason finale: end_turn | tool_use | error | loop_detected | timeout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// CHI ha prodotto la sospensione, quando lo status e' `AwaitingConfirmation`
    /// (rilievo A4). `None` = il run non e' sospeso.
    ///
    /// Trasporta fino alla riga DB la dichiarazione del nodo che ha sospeso
    /// (`NativeRunOutcome::suspension_origin`): da qui nascono
    /// `agent_runs.suspension_kind` e, se la sospensione scade senza che
    /// nessuno la sciolga, il `blocker` ADR 0034 con cui il run viene chiuso.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspension_origin: Option<nexus_agent_graph::decisions::SuspensionOrigin>,
    /// Task type rilevato dal router (es. "fix", "code_read", "architecture").
    /// Propagato dal brain Python nell'evento SSE end_turn per popolare
    /// la colonna `nexus_task_type` in agent_runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nexus_task_type: Option<String>,
    /// `true` se l'agente ha dichiarato di aver completato senza invocare
    /// alcun tool nonostante avesse tool disponibili (0 step, iteration <= 1).
    /// Tipico di modelli piccoli che "allucinano il completamento".
    #[serde(default)]
    pub hollow_completion: bool,
    /// `true` quando l'hollow e' specificamente del tipo "aveva tool esposti ma
    /// non ne ha invocato nessuno al primo turno" (steps vuoti, iteration <= 1).
    /// E' il segnale runtime di un MALFORMED_FUNCTION_CALL / output-vuoto sul
    /// tool-forcing (es. gemini-2.5-pro sui task agentici). Permette al caller
    /// di marcare il modello come NON tool-capable (supports_tool_use=false)
    /// senza disabilitarlo globalmente, lasciandolo disponibile per i task chat.
    #[serde(default)]
    pub hollow_no_tools: bool,
    /// Cause STRUTTURATE del completamento hollow (vedi [`HollowCause`]). Vuoto
    /// se `hollow_completion=false`, o se l'hollow non ha una causa dichiarata —
    /// il non-saputo e' l'insieme vuoto, non una causa arbitraria.
    ///
    /// Era una `String` che impacchettava piu' cause concatenate con un `+`
    /// ("EMPTY_ANSWER+NO_TOOLS"), e a valle il declassamento dello status del run
    /// si decideva con un `contains("EMPTY_ANSWER")` su quella stringa: un
    /// insieme travestito da testo, che costringeva ogni consumatore a ri-parsare
    /// cio' che il produttore sapeva gia' in forma discreta. Il testo si compone
    /// ora DAI campi, e solo dove serve davvero: la colonna storica
    /// `nexus_provider_empty_responses.kind` (mig 0291) la scrive
    /// [`hollow_kind_for_db`].
    #[serde(default)]
    pub hollow_causes: std::collections::BTreeSet<HollowCause>,
    /// Ragionamento (thinking) accumulato del run: concatenazione di tutti i
    /// `thinking_delta` emessi durante l'esecuzione. LIVE viaggia come evento SSE
    /// `agent_thinking` (volatile, perso al refresh); qui lo accumuliamo per
    /// PERSISTERLO nel `metadata.reasoning` del messaggio assistant, cosi' dopo un
    /// F5 il blocco di ragionamento resta visibile (FIX divergenza chat post-refresh).
    /// `None` se il modello non ha prodotto thinking. Mai loggato in chiaro (regola F).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Conversazione finale del run nativo serializzata `[{role, content, ...}]`
    /// (campo `messages` dello stato del grafo). PERSISTITA in
    /// `agent_runs.messages_json` dal finalizzatore: serve al resume
    /// (`status='interrupted'` con `messages_json IS NOT NULL`) e al trace panel.
    /// `None` sul path Python (che persiste la history per altra via) o quando la
    /// conversazione e' vuota. Non serializzata se assente.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages_json: Option<String>,
}

/// Evento di trace LLM: mostra i messaggi inviati al provider e la risposta ricevuta.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AITraceEvent {
    pub run_id: String,
    pub iteration: u32,
    pub provider: String,
    pub model: String,
    pub messages_sent: u32,     // quanti messaggi nella conversazione
    pub tools_count: u32,       // quanti tool disponibili
    pub response_text: String,  // testo della risposta (troncato)
    pub tool_calls: Vec<Value>, // tool call names + inputs
    pub stop_reason: String,
    pub timestamp: String,
    /// Token di prompt LORDI del turno (convenzione di
    /// `nexus_gateway::LlmUsage`): comprendono i due conteggi di cache qui sotto.
    /// Chi prezza la traccia scorpora — tariffa piena su
    /// `input_tokens - cache_read - cache_creation`, poi ciascuna quantita' di
    /// cache alla sua tariffa.
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Token del turno serviti da cache (tariffa ridotta): sottoinsieme di
    /// `input_tokens`.
    pub cache_read_tokens: u32,
    /// Token del turno scritti in cache (tariffa maggiorata). Assente nelle
    /// tracce persistite prima dell'introduzione del campo: il frontend lo legge
    /// come opzionale e in quel caso non ha nulla da tariffare.
    pub cache_creation_tokens: u32,
}

/// Meta-step pubblicato in chat per dare visibilità a passaggi interni del
/// graph (plan del planner, decisione di routing, richiesta di chiarimento,
/// fallback provider, riflessione post-hoc). Discriminato da `kind`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMetaStep {
    /// `plan` | `routing` | `clarify` | `fallback` | `reflection`.
    pub kind: String,
    /// Titolo umano sintetico mostrato in UI (collassato).
    pub title: String,
    /// Payload strutturato dipendente da `kind`.
    pub payload: Value,
    /// Collega il meta_step a un evento precedente (es. fallback ↔ tool_use).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub created_at: String,
}

/// Evento trasmesso via broadcast per l'SSE del frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStepEvent {
    pub run_id: String,
    pub step: Option<AgentStep>,
    pub trace: Option<AITraceEvent>,
    pub is_final: bool,
    /// Token parziale durante la generazione (streaming). Se presente, è evento `agent_token`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_delta: Option<String>,
    /// Ragionamento intermedio del modello (testo che accompagna le tool calls
    /// durante le iterazioni dell'agent loop). Mostrato nella chat come blocco
    /// collassabile "Ragionamento" per dare feedback visivo durante il lavoro.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_delta: Option<String>,
    /// Meta-step (plan/routing/clarify/fallback/reflection). Mutuamente
    /// esclusivo con `step`/`trace`/`token_delta` ma non vincolato.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta_step: Option<AgentMetaStep>,
}

// ---------------------------------------------------------------------------
// Helper detection testo utente
// ---------------------------------------------------------------------------

/// Rileva se il messaggio utente richiede un'azione operativa (build, deploy, run, ecc.).

pub(crate) fn detect_action_request(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let lower = text.to_lowercase();
    let action_patterns: &[&str] = &[
        // Italiano — imperativo / infinito / futuro
        "avvia",
        "avviare",
        "lancia",
        "lanciare",
        "esegui",
        "eseguire",
        "builda",
        "buildare",
        "crea ",
        "creare",
        "crea il",
        "crea la",
        "installa",
        "installare",
        "configura",
        "configurare",
        "deploya",
        "deployare",
        "compila",
        "compilare",
        "fai partire",
        "metti in piedi",
        "porta in su",
        "metti online",
        "avvia i servizi",
        "avvia il servizio",
        "avvia il backend",
        "avvia il frontend",
        "avvia il server",
        "avvia i container",
        "testa il",
        "testa la",
        // Inglese — imperativo / common forms
        "start ",
        "start the",
        "launch ",
        "launch the",
        " run ",
        "run the",
        "run it",
        " build",
        "build the",
        "build it",
        " create ",
        "create the",
        "install ",
        "install the",
        "setup ",
        "set up ",
        "configure ",
        "deploy ",
        "deploy the",
        "compile ",
        "compile the",
        // Tool / tecnologie specifiche (alta probabilità d'azione)
        "docker",
        "docker-compose",
        "docker compose",
        "npm install",
        "npm run",
        "pnpm install",
        "pnpm run",
        "cargo build",
        "cargo run",
        "dotnet run",
        "dotnet build",
        "dotnet watch",
        "pip install",
        "pip3 install",
        "apt install",
        "apt-get install",
        "systemctl start",
        "service start",
        "make ",
        "make\t",
    ];
    action_patterns.iter().any(|p| lower.contains(p))
}

/// Azione da applicare al catalog dopo un turno-con-tool agentico, in base
/// all'esito (regola B: tool-failure model-specific via supports_tool_use).
/// Funzione PURA per testabilita' — il chiamante (chat_messages.rs) la
/// traduce nelle UPDATE SQL effettive.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum ToolCapabilityAction {
    /// Nessuna azione (es. intent chat, o run fallito non classificabile).
    None,
    /// Incrementa consecutive_tool_failures di 1.
    IncrementToolFailure,
    /// Soglia raggiunta: marca supports_tool_use=false (NON is_enabled).
    MarkNonToolCapable,
    /// Turno-con-tool riuscito: reset contatori + riabilita tool-capability.
    ResetOnSuccess,
}

/// Decide l'azione tool-capability dato lo stato del turno.
/// - `intent_uses_tools`: l'intent richiede tool (non "chat").
/// - `completed`: status == Completed.
/// - `hollow_no_tools`: segnale MALFORMED/output-vuoto sul tool-forcing.
/// - `success_with_tool`: completato con final_answer non vuoto e non hollow.
/// - `prior_tool_failures`: valore corrente di consecutive_tool_failures.
/// - `threshold`: soglia da settings.agent.model_tool_failure_threshold.
pub(crate) fn tool_failure_action(
    intent_uses_tools: bool,
    completed: bool,
    hollow_no_tools: bool,
    success_with_tool: bool,
    prior_tool_failures: i32,
    threshold: i32,
) -> ToolCapabilityAction {
    if !intent_uses_tools || !completed {
        return ToolCapabilityAction::None;
    }
    if hollow_no_tools {
        let new_count = prior_tool_failures + 1;
        if new_count >= threshold {
            ToolCapabilityAction::MarkNonToolCapable
        } else {
            ToolCapabilityAction::IncrementToolFailure
        }
    } else if success_with_tool {
        ToolCapabilityAction::ResetOnSuccess
    } else {
        ToolCapabilityAction::None
    }
}

// ---------------------------------------------------------------------------
// DB helpers
// ---------------------------------------------------------------------------

/// Funzione pubblica per finalizzare un run (usata da chat_messages per la ripresa).

pub async fn finalize_agent_run(
    db: &PgPool,
    run_id: Uuid,
    status: AgentRunStatus,
    final_answer: Option<&str>,
    iteration_count: u32,
) {
    let status_str = status.as_str();
    let _ = sqlx::query("UPDATE agent_runs SET status = $2, final_answer = $3, iteration_count = $4, completed_at = NOW() WHERE id = $1")
    .bind(run_id)
    .bind(status_str)
    .bind(final_answer)
    .bind(iteration_count as i32)
    .execute(db)
    .await;

    // Fix M27 (auto-commit locale): a fine run completato con successo, fa
    // git add -A + commit nel project_root se ci sono cambiamenti.
    // Idempotente (skip se git status pulito), best-effort (errori solo loggati),
    // NO push automatico. L'utente decide se/quando pushare via UI.
    if status.is_success() {
        let db_clone = db.clone();
        tokio::spawn(async move {
            auto_commit_project_changes(&db_clone, run_id).await;
        });
    }
}

/// Esegue `git add -A && git commit -m ...` nel project_root del run.
/// Idempotente: se non ci sono modifiche staged, non commit niente.
/// Best-effort: errori loggati ma non propagati.
pub async fn auto_commit_project_changes(db: &PgPool, run_id: Uuid) {
    // 1. Recupera project_id e project_root
    let row = match sqlx::query_as::<_, (Uuid, Option<String>)>(
        r#"
        SELECT r.project_id, w.absolute_path
        FROM agent_runs r
        LEFT JOIN workspaces w ON w.project_id = r.project_id AND w.is_primary = true
        WHERE r.id = $1
        LIMIT 1
        "#,
    )
    .bind(run_id)
    .fetch_optional(db)
    .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!("auto_commit: lookup run fallito: {}", e);
            return;
        }
    };
    let (_project_id, root_opt) = row;
    let root = match root_opt {
        Some(p) if !p.is_empty() => p,
        _ => return,
    };
    let root_path = std::path::PathBuf::from(&root);
    if !root_path.is_dir() {
        return;
    }
    // 2. Verifica che sia un repo git
    let is_git = tokio::process::Command::new("git")
        .args(["-C", &root, "rev-parse", "--is-inside-work-tree"])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !is_git {
        return;
    }
    // Fix M34: se manca .gitignore, scrive un default minimo prima del git add
    // per evitare di committare node_modules / build artifacts / secrets.
    // Patterns scelti per coprire i default Node/Python/Rust/Next/Vite generici.
    let gitignore_path = root_path.join(".gitignore");
    if !gitignore_path.exists() {
        let default_gitignore = "\
# Auto-generato da Nexus al primo commit. Personalizza se serve.

# Dependencies
node_modules/
.pnpm-store/
__pycache__/
*.pyc
.venv/
venv/

# Build artifacts
dist/
build/
out/
.next/
.nuxt/
.turbo/
target/
*.tsbuildinfo

# Test artifacts
coverage/
.nyc_output/
test-results/
playwright-report/

# Logs
*.log
logs/
npm-debug.log*
yarn-debug.log*
yarn-error.log*

# Env / secrets
.env
.env.*
!.env.example
*.pem
*.key

# IDE
.vscode/
.idea/
*.swp
*.swo
.DS_Store
Thumbs.db
";
        if let Err(e) = tokio::fs::write(&gitignore_path, default_gitignore).await {
            tracing::warn!(
                "auto_commit: scrittura .gitignore default fallita: {} (continuo lo stesso)",
                e
            );
        } else {
            tracing::info!("auto_commit: scritto .gitignore default in {}", root);
        }
    }
    // 3. Stage all changes
    let _ = tokio::process::Command::new("git")
        .args(["-C", &root, "add", "-A"])
        .output()
        .await;
    // 4. Skip se niente staged (idempotente)
    let any_staged = tokio::process::Command::new("git")
        .args(["-C", &root, "diff", "--cached", "--quiet"])
        .output()
        .await
        .map(|o| !o.status.success()) // exit code != 0 => changes staged
        .unwrap_or(false);
    if !any_staged {
        return;
    }
    // 5. Conta i file staged per il messaggio
    let count = tokio::process::Command::new("git")
        .args(["-C", &root, "diff", "--cached", "--name-only"])
        .output()
        .await
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count()
        })
        .unwrap_or(0);
    let msg = format!(
        "feat: modifiche da Nexus run {} ({} file)\n\n\
         Auto-commit generato da Nexus al completamento del run agente.\n\
         Per pubblicare su GitHub usa il pannello Source Control (Crea repo / Push).\n",
        run_id, count
    );
    // 6. Commit con user.email/name fallback per evitare fail se git config vuota
    let output = tokio::process::Command::new("git")
        .args([
            "-C",
            &root,
            "-c",
            "user.email=nexus@local",
            "-c",
            "user.name=Nexus Agent",
            "commit",
            "-m",
            &msg,
        ])
        .output()
        .await;
    match output {
        Ok(o) if o.status.success() => {
            tracing::info!(
                "auto_commit: {} file committati nel run {} (root={})",
                count,
                run_id,
                root
            );
        }
        Ok(o) => {
            tracing::warn!(
                "auto_commit: git commit fallito (exit {}): stderr={}",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        Err(e) => tracing::warn!("auto_commit: spawn git commit fallito: {}", e),
    }
}

// ---------------------------------------------------------------------------
// G4 — Memorizzazione startup_command dopo avvio servizio riuscito
// ---------------------------------------------------------------------------

/// G4: dopo un run completato con successo cerca l'ultimo `shell_exec` che ha
/// eseguito `docker compose up` e salva il comando esatto in `memory_entries`
/// con ns_type='project'.
///
/// NOTA: con la rimozione del modulo morto `project_context.rs` non esiste
/// piu' alcun lettore di `memory_entries` nel repo: questa scrittura e'
/// funzionalmente orfana finche' non viene cablata un'iniezione nel prompt
/// (oppure rimossa anche questa funzione).
///
/// Fire-and-forget: gli errori non bloccano il return del run.
pub async fn save_startup_command_if_needed(db: &PgPool, project_id: Uuid, steps: &[AgentStep]) {
    // Cerca l'ultimo step shell_exec riuscito con `docker compose up`
    let docker_cmd = steps
        .iter()
        .rev()
        .filter(|s| s.tool_name == "shell_exec" && s.status == AgentStepStatus::Completed)
        .find_map(|s| {
            let cmd = s.tool_input.get("command").and_then(|v| v.as_str())?;
            if cmd.contains("docker") && cmd.contains("compose") && cmd.contains("up") {
                Some(cmd.to_string())
            } else {
                None
            }
        });

    let cmd = match docker_cmd {
        Some(c) => c,
        None => return, // nessun docker compose up → niente da salvare
    };

    // Cerca o crea il namespace di tipo 'project' per questo progetto
    let ns_key = format!("project:{project_id}");
    let ns_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM memory_namespaces WHERE ns_key = $1 AND active = TRUE LIMIT 1",
    )
    .bind(&ns_key)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    let ns_id = match ns_id {
        Some(id) => id,
        None => {
            let new_id = Uuid::new_v4();
            let _ = sqlx::query(
                "INSERT INTO memory_namespaces \
                 (id, ns_key, ns_type, project_id, merge_strategy, active) \
                 VALUES ($1, $2, 'project', $3, 'lww', TRUE) \
                 ON CONFLICT (ns_key) DO NOTHING",
            )
            .bind(new_id)
            .bind(&ns_key)
            .bind(project_id)
            .execute(db)
            .await;
            match sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM memory_namespaces WHERE ns_key = $1 AND active = TRUE LIMIT 1",
            )
            .bind(&ns_key)
            .fetch_optional(db)
            .await
            {
                Ok(Some(id)) => id,
                _ => {
                    tracing::debug!("G4: impossibile creare/trovare namespace per {ns_key}");
                    return;
                }
            }
        }
    };

    let value = serde_json::json!(cmd);
    let _ = sqlx::query(
        "INSERT INTO memory_entries \
         (id, namespace_id, entry_key, value, written_by, version, vector_clock) \
         VALUES (gen_random_uuid(), $1, 'startup_command', $2, 'agent_run', 1, '{}') \
         ON CONFLICT (namespace_id, entry_key) WHERE deleted = FALSE \
         DO UPDATE SET value = EXCLUDED.value, \
                       written_by = EXCLUDED.written_by, \
                       version = memory_entries.version + 1, \
                       updated_at = NOW()",
    )
    .bind(ns_id)
    .bind(&value)
    .execute(db)
    .await
    .map(|_| {
        tracing::info!("G4: startup_command salvato per progetto {project_id}: {cmd:?}");
    })
    .map_err(|e| tracing::warn!("G4: salvataggio startup_command fallito: {e}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_run_status_sql_coerente_con_array() {
        // Il frammento SQL e l'array sono due rappresentazioni della STESSA lista
        // (punto unico): ogni stato dell'array deve comparire quotato nel frammento e
        // il conteggio deve coincidere. Se qualcuno aggiunge uno stato sospeso-vivo
        // all'array ma dimentica il frammento SQL (o viceversa), questo test rompe.
        for s in ACTIVE_RUN_STATUSES {
            assert!(
                ACTIVE_RUN_STATUS_SQL.contains(&format!("'{s}'")),
                "lo stato {s} manca nel frammento SQL ACTIVE_RUN_STATUS_SQL"
            );
        }
        let sql_count = ACTIVE_RUN_STATUS_SQL.matches('\'').count() / 2;
        assert_eq!(
            sql_count,
            ACTIVE_RUN_STATUSES.len(),
            "frammento SQL e array hanno un numero di stati diverso"
        );
        assert!(is_active_run_status("awaiting_subagents"));
        assert!(!is_active_run_status("completed"));
    }

    #[test]
    fn tool_action_chat_intent_is_none() {
        // Intent chat: mai toccare la tool-capability anche se hollow_no_tools.
        let a = tool_failure_action(false, true, true, false, 5, 3);
        assert_eq!(a, ToolCapabilityAction::None);
    }

    #[test]
    fn tool_action_increment_below_threshold() {
        // hollow_no_tools, prior=1, threshold=3 -> new=2 < 3 -> solo incremento.
        let a = tool_failure_action(true, true, true, false, 1, 3);
        assert_eq!(a, ToolCapabilityAction::IncrementToolFailure);
    }

    #[test]
    fn tool_action_marks_non_tool_capable_at_threshold() {
        // hollow_no_tools, prior=2, threshold=3 -> new=3 >= 3 -> non tool-capable.
        let a = tool_failure_action(true, true, true, false, 2, 3);
        assert_eq!(a, ToolCapabilityAction::MarkNonToolCapable);
    }

    #[test]
    fn tool_action_reset_on_success_with_tool() {
        // Turno-con-tool riuscito -> reset (riabilita tool-capability).
        let a = tool_failure_action(true, true, false, true, 2, 3);
        assert_eq!(a, ToolCapabilityAction::ResetOnSuccess);
    }

    #[test]
    fn tool_action_completed_but_not_hollow_not_success_is_none() {
        // Completato ma ne' hollow_no_tools ne' success_with_tool (es. hollow
        // generico empty-answer): la regola B non agisce qui.
        let a = tool_failure_action(true, true, false, false, 0, 3);
        assert_eq!(a, ToolCapabilityAction::None);
    }

    #[test]
    fn tool_action_not_completed_is_none() {
        let a = tool_failure_action(true, false, true, false, 0, 3);
        assert_eq!(a, ToolCapabilityAction::None);
    }

    // ── is_terminal / from_db_str (punto unico stati run) ──────────────────
    #[test]
    fn from_db_str_roundtrip_su_ogni_variante() {
        // as_str -> from_db_str deve tornare alla stessa variante per tutte
        // le varianti dell'enum (eccetto i sinonimi gestiti a parte).
        for st in [
            AgentRunStatus::Running,
            AgentRunStatus::Completed,
            AgentRunStatus::AwaitingConfirmation,
            AgentRunStatus::AwaitingSubagents,
            AgentRunStatus::Failed,
            AgentRunStatus::TimedOut,
            AgentRunStatus::Cancelled,
            AgentRunStatus::LoopAborted,
            AgentRunStatus::ProviderUnavailable,
            AgentRunStatus::CompletedVerified,
            AgentRunStatus::CompletedUnverified,
            AgentRunStatus::FailedDiagnosed,
            AgentRunStatus::BlockedNeedsInput,
        ] {
            assert_eq!(AgentRunStatus::from_db_str(st.as_str()), st);
        }
    }

    #[test]
    fn from_db_str_interrupted_e_ignoto() {
        // 'interrupted' (cleanup di startup) -> Cancelled (terminale).
        assert_eq!(
            AgentRunStatus::from_db_str("interrupted"),
            AgentRunStatus::Cancelled
        );
        // Stringa ignota -> Running conservativo (non terminale).
        assert_eq!(
            AgentRunStatus::from_db_str("qualcosa_di_strano"),
            AgentRunStatus::Running
        );
    }

    #[test]
    fn is_terminal_include_esiti_canonici_nuovi() {
        // Gli esiti della "determinazione certa" DEVONO essere terminali:
        // era il bug del match inline in chat_agent.rs.
        assert!(AgentRunStatus::FailedDiagnosed.is_terminal());
        assert!(AgentRunStatus::CompletedVerified.is_terminal());
        assert!(AgentRunStatus::CompletedUnverified.is_terminal());
        // CompletedUnverified e' un successo (lavoro svolto, non verificato).
        assert!(AgentRunStatus::CompletedUnverified.is_success());
        // Terminali classici.
        for st in [
            AgentRunStatus::Completed,
            AgentRunStatus::Failed,
            AgentRunStatus::TimedOut,
            AgentRunStatus::Cancelled,
            AgentRunStatus::LoopAborted,
            AgentRunStatus::ProviderUnavailable,
        ] {
            assert!(st.is_terminal(), "{} deve essere terminale", st.as_str());
        }
    }

    #[test]
    fn is_terminal_esclude_in_corso_e_in_pausa() {
        // Running = in corso; Awaiting = run SOSPESO con resume HITL.
        assert!(!AgentRunStatus::Running.is_terminal());
        assert!(!AgentRunStatus::AwaitingConfirmation.is_terminal());
        // AwaitingSubagents = run SOSPESO con resume fan-in (Fase D): non-terminale.
        assert!(!AgentRunStatus::AwaitingSubagents.is_terminal());
        // BlockedNeedsInput e' TERMINALE (ADR 0034): run concluso con
        // dichiarazione "serve input"; il prossimo input crea un nuovo run.
        assert!(AgentRunStatus::BlockedNeedsInput.is_terminal());
    }

    /// `stati_non_terminali_db` e `is_terminal` non devono poter divergere: la
    /// costante serve ai call site SQL, che non possono chiamare il predicato.
    /// Enumerando OGNI variante si copre anche il caso pericoloso — qualcuno
    /// aggiunge uno stato non-terminale e non aggiorna la lista, e una query di
    /// backstop inizia a trattare come "concluso" un run che sta ancora girando.
    ///
    /// Test di mutazione: togliendo `awaiting_subagents` dalla costante (o
    /// rendendolo terminale in `is_terminal`) questo assert rosseggia nominando
    /// lo stato divergente.
    #[test]
    fn stati_non_terminali_db_coerente_con_is_terminal() {
        for st in [
            AgentRunStatus::Running,
            AgentRunStatus::Completed,
            AgentRunStatus::AwaitingConfirmation,
            AgentRunStatus::AwaitingSubagents,
            AgentRunStatus::Failed,
            AgentRunStatus::TimedOut,
            AgentRunStatus::Cancelled,
            AgentRunStatus::LoopAborted,
            AgentRunStatus::ProviderUnavailable,
            AgentRunStatus::CompletedVerified,
            AgentRunStatus::CompletedUnverified,
            AgentRunStatus::FailedDiagnosed,
            AgentRunStatus::BlockedNeedsInput,
        ] {
            let nella_lista = AgentRunStatus::stati_non_terminali_db().contains(&st.as_str());
            assert_eq!(
                !nella_lista,
                st.is_terminal(),
                "'{}': is_terminal={} ma nella lista dei non-terminali={}",
                st.as_str(),
                st.is_terminal(),
                nella_lista
            );
        }
    }
}
