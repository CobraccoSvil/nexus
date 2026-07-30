//! Stato tipizzato del grafo agentico Nexus (replica `brain/agents/state.py`).
//!
//! I ~90 campi del `TypedDict` Python diventano campi Rust type-safe. Scelte di
//! tipizzazione (vedi `/tmp/langgraph_plan.md` sez. 3):
//!
//! - Campi `X | None` Python -> `Option<T>`.
//! - `messages` e `meta_steps` -> `Vec<...>` con `#[reduce(append)]`: sono gli
//!   UNICI due canali con reducer `add` (verificato su `state.py:17,24`). Tutti
//!   gli altri campi usano il reducer di default (overwrite-by-last-write).
//! - Campi enumerabili (`stop_reason`, `automation_mode`, `task_complexity`)
//!   -> enum Rust dedicati con `serde(rename...)`, NON `String`.
//! - Schema APERTO: `#[serde(default)]` sullo struct + `#[serde(flatten)] extra`
//!   per i campi runtime non presenti nel `TypedDict` (`iteration_budget`,
//!   `complexity_score`, `project_id`, `auto_escalations`, ...). Garantisce la
//!   non-perdita nel round-trip e la tolleranza in avanti nella coesistenza.
//!
//! Il reducer (`merge_typed`) e' generato dal `#[derive(GraphState)]` (punto
//! unico, regola L): l'impl del trait `nexus_graph::GraphState` (che lavora su
//! un delta JSON opaco lato runtime) vi delega.

pub mod delta;
pub mod lc_serde;
pub mod message;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use nexus_graph_derive::GraphState as DeriveGraphState;

pub use delta::{put_extra, StateDelta};

/// Esito di UN verdetto del final gate: una variante per ogni ramo di uscita
/// del nodo (punto unico del vocabolario, regola L+M).
///
/// I consumatori DEVONO leggerlo con un `match` esaustivo: e' il meccanismo che
/// rende il fix duraturo. Aggiungere un ramo al gate senza aggiungere qui la
/// variante, o aggiungere una variante senza dichiararne la semantica nei
/// consumatori, NON COMPILA. E' esattamente cio' che mancava quando il ramo
/// "turno di grazia" fu introdotto: nessuno segnalo' al finalizzatore che il
/// suo `final_gate_cycle > 0` aveva smesso di significare "bocciato".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalGateVerdict {
    /// Tutti i criteri superati: il run chiude verificato. (Se il profilo di
    /// verifica mancava, `final_gate_unverified` lo dice a parte: qui il gate
    /// ha comunque emesso un verdetto positivo su cio' che ha potuto misurare.)
    Passed,
    /// Criteri OGGETTIVI tutti superati; manca SOLO la dichiarazione
    /// strutturata di chiusura (`task_complete`). Il gate ha concesso il turno
    /// di grazia per la firma. **Non e' un fallimento**: il lavoro e'
    /// verificato. Lascia `final_gate_cycle = max_cycles` per non ri-entrare,
    /// e quel residuo era la causa del falso `FailedDiagnosed`.
    ObjectivePassedSignatureMissing,
    /// Criteri oggettivi falliti al cap: il turno e' ceduto all'executor
    /// perche' promuova un modello piu' capace. Il run PROSEGUE: non e' ancora
    /// un esito.
    EscalationHandoff,
    /// Chiusura al cap / forced_close con criteri NON superati: bocciatura
    /// esplicita e DEFINITIVA (nessuna ri-verifica era prevista).
    FailedFinal,
    /// Bocciatura con correzione RIMANDATA all'executor: la ri-verifica era
    /// prevista. Se il run muore prima di rientrare nel gate, questo e' l'unico
    /// caso in cui l'esito "verifica fallita e non ripetuta" e' VERO.
    FailedPendingCorrection,
}

/// ESITO dell'ultimo passaggio dal ReviewGate (gemello di [`FinalGateVerdict`],
/// regola M: l'esito e' un campo proprio, mai dedotto dal contatore
/// `review_cycle`). Lo scrive OGNI ramo del nodo; i consumatori fanno `match`
/// esaustivo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewGateVerdict {
    /// Il panel ha approvato (Pass): il run chiude con review superata.
    Approved,
    /// Il gate non si applicava (disabilitato, nessun codice modificato,
    /// dichiarazione non-done, run gia' bocciato dal final_gate, panel
    /// azzerato dal dimensionamento): nessun giudizio emesso.
    NotApplicable,
    /// Panel convocato ma quorum non raggiunto: limite infrastrutturale, non un
    /// difetto del codice (mai trattato come rifiuto).
    Inconclusive,
    /// La convocazione e' fallita a monte (ctx non costruibile, porta in
    /// errore): il giudizio non e' stato possibile.
    Unavailable,
    /// Bocciatura con correzione RIMANDATA all'executor: la ri-review era
    /// prevista. Se il run muore prima di rientrare nel gate, la bocciatura
    /// resta VERA (nessuna ri-verifica avvenuta).
    PendingCorrection,
    /// Bocciatura DEFINITIVA: cap dei rimandi raggiunto, il run chiude bocciato.
    RejectedFinal,
    /// Bocciatura definitiva in cui NESSUN rimando ha prodotto una modifica:
    /// l'agente ha risposto ai rilievi senza toccare un file, per tutti i
    /// tentativi disponibili.
    ///
    /// Perche' e' una variante e non una nota: la causa e' diversa da
    /// [`ReviewGateVerdict::RejectedFinal`] e quindi lo e' anche l'azione
    /// dell'utente. "Ha provato e non ci e' riuscito" e' un problema di
    /// difficolta' del rilievo (si guarda il codice); "non ha provato" e' un
    /// problema del modello o del prompt (si cambia figura, si riformula). Il run
    /// osservato sul progetto `gestione-spese` (28/07/2026) chiuse col primo
    /// verdetto mostrando il secondo caso: tre bocciature, zero file toccati,
    /// 1.243.417 token.
    ///
    /// Chi deve solo sapere SE la review ha bocciato usa
    /// [`ReviewGateVerdict::e_bocciatura_definitiva`], non un match sulla
    /// variante: e' il punto unico di quella domanda.
    RejectedNoCorrection,
}

impl ReviewGateVerdict {
    /// La bocciatura e' DEFINITIVA (il run chiude bocciato, nessun altro
    /// rimando): vero per entrambe le nature del rifiuto finale.
    ///
    /// PUNTO UNICO (regola L): il guard anti-loop, gli edge e i consumatori
    /// chiedono qui invece di elencare le varianti. Con l'elenco a mano,
    /// l'aggiunta di [`ReviewGateVerdict::RejectedNoCorrection`] avrebbe fatto
    /// ri-convocare il panel a ogni rientro nel funnel di chiusura per l'unico
    /// esito in cui il panel e' certamente inutile.
    pub fn e_bocciatura_definitiva(&self) -> bool {
        matches!(self, Self::RejectedFinal | Self::RejectedNoCorrection)
    }
}

/// DECISIONE DI ROUTING dichiarata da un gate di chiusura (final_gate,
/// review_gate): l'UNICO segnale su cui i loro edge instradano.
///
/// Perche' esiste (regola M: lo stato tecnico si legge da un segnale
/// strutturato, mai dedotto). Prima gli edge dei due gate deducevano il rimando
/// da `stop_reason == ToolUse`, un campo CONDIVISO che il gate non possiede: lo
/// scrive anche l'executor a ogni turno con tool pendenti. Il final_gate
/// riscrive `stop_reason` su OGNI ramo (`EndTurn` quando chiude), quindi la
/// deduzione per lui tornava; il ReviewGate lo riscriveva solo sul rimando e sui
/// rami di CHIUSURA (approvazione, bocciatura definitiva, pass-through del
/// guard) lasciava in piedi il `ToolUse` del turno precedente. L'edge lo leggeva
/// come "rimanda" e rispediva all'executor un run che il gate aveva appena
/// dichiarato chiuso.
///
/// Misurato sul run 609000c1 (26/07/2026): il checkpoint
/// alterna `review_gate -> executor -> review_gate` per 107 superstep con
/// `stop_reason=tool_use` costante, verdetto `approved` ai cicli 4-7 e
/// `rejected_final` dal ciclo 8 in poi. Nessuno dei due esiti chiude. Il ciclo
/// si e' rotto solo quando l'utente ha premuto Stop e `stop_reason` e' diventato
/// `superseded` — cioe' quando il campo su cui l'edge instradava ha smesso, per
/// una causa ESTERNA al gate, di valere `ToolUse`.
///
/// Il default (`None`) e' il ramo SICURO: un gate che non dichiara nulla fa
/// chiudere il run, non ciclare. L'assenza di dichiarazione puo' al piu'
/// anticipare una chiusura; non puo' produrre un loop a spesa illimitata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateRouting {
    /// Il gate ha finito: il run prosegue verso la chiusura (reflection).
    Chiude,
    /// Il gate restituisce il turno all'executor per una correzione.
    RimandaInCorrezione,
}
pub use message::{ContentBlock, Message, MessageContent, ToolUse};

/// Modalita' supervisore worker (UI: off / su anomalia / ogni N step / continuo).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorMode {
    #[default]
    None,
    Anomaly,
    #[serde(rename = "interleaved")]
    Interleaved,
    Continuous,
}

impl std::str::FromStr for SupervisorMode {
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

/// Modalita' di automazione del turno chat propagata da mcp-core
/// (`state.py:274`). Enum dedicato invece di `String`: il `match` esaustivo
/// nei nodi non puo' dimenticare un caso.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationMode {
    /// Nessuna automazione (default conservativo).
    None,
    /// Conferma richiesta prima di azioni potenzialmente distruttive.
    Confirm,
    /// L'agente esplora/agisce senza fermarsi a chiedere.
    Automatic,
    /// Modalita' continua (catena di turni autonomi).
    Continuous,
}

impl AutomationMode {
    /// True se il run procede senza HITL strutturale (automatic o continuous).
    pub fn is_autonomous(self) -> bool {
        matches!(self, Self::Automatic | Self::Continuous)
    }

    /// Etichetta wire canonica per hint di autonomia (`automatic` / `continuous`).
    pub fn wire_label(self) -> Option<&'static str> {
        match self {
            Self::Automatic => Some("automatic"),
            Self::Continuous => Some("continuous"),
            Self::None | Self::Confirm => None,
        }
    }
}

/// Complessita' del task stimata dal classifier agentico (`state.py:31`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskComplexity {
    /// Task semplice.
    Low,
    /// Task di media difficolta'.
    Medium,
    /// Task complesso.
    High,
}

/// Motivo di arresto del turno executor (`state.py:107-108` + piano sez. 5).
///
/// Chiave di tutto il routing post-executor: enum esaustivo, non `String`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Il modello ha risposto con tool_use: loop verso tool_dispatch.
    ToolUse,
    /// Fine turno naturale: verso learner.
    EndTurn,
    /// Stop generico del modello.
    Stop,
    /// L'executor ha rilevato chiamate ripetute: forza chiusura.
    LoopDetected,
    /// Run soppiantato da un run piu' recente sulla stessa sessione.
    Superseded,
    /// Escalation G1 (risposta descrittiva su richiesta d'azione).
    G1Escalated,
    /// Abort coordinato del progress_controller.
    LoopAbort,
    /// Cap G1 raggiunto.
    G1CapReached,
    /// L'executor ha rilevato uno stallo che richiede META-RAGIONAMENTO e instrada
    /// al nodo dedicato `StallRecovery` (superstep isolato, ADR 0036-style). E' un
    /// segnale di ROUTING interno: `route_after_executor` lo mappa su
    /// `NodeTarget::StallRecovery`. Il `StallContext` serializzato viaggia in
    /// `extra` (chiave [`crate::nodes::stall_recovery::STALL_CONTEXT_KEY`]).
    ///
    /// Prodotto dai gate di emissione dell'executor — `maybe_stall_reason_delta`
    /// per gli assi di stallo, piu' il gemello per gli assi runaway pre-LLM — solo
    /// quando `agent.stall_recovery.enabled` e' truthy in `settings` (il valore
    /// vive nel DB e cambia a caldo: qui non c'e' un default di compile-time) e il
    /// budget per-sessione non e' esaurito. Senza il flag nulla lo produce e
    /// decide la sola gerarchia fissa di `progress_controller::decide`.
    StallReason,
    /// Il nodo `StallRecovery` ha risolto lo stallo (mossa scelta o fallback) e
    /// rientra nell'executor (self-loop, come `G1Escalated -> executor`).
    /// `route_after_executor` NON e' interessato (il nodo produttore e'
    /// `StallRecovery`, il cui edge instrada direttamente all'executor): la
    /// variante e' il segnale che il superstep di recovery e' concluso.
    StallResolved,
    /// L'executor ha rilevato che e' il momento di VALUTARE la scala-tier del
    /// modello (up/down PRE-CRISI) e instrada al nodo dedicato `ScaleControl`
    /// (superstep isolato, SCALE-CONTROLLER). Gemello di [`StopReason::StallReason`]:
    /// e' un segnale di ROUTING interno; il `ScaleContext` serializzato viaggia in
    /// `extra`.
    ///
    /// Prodotto dal detector di scala dell'executor (`maybe_scale_reason_delta`,
    /// pre-LLM) solo quando `agent.scale.enabled` e' truthy in `settings`, il tetto
    /// dei cambi-tier non e' raggiunto e il budget `max_evals_per_run` non e'
    /// esaurito. `route_after_executor` lo instrada su `NodeTarget::ScaleControl`;
    /// senza il flag nulla lo produce.
    ScaleReason,
    /// Il nodo `ScaleControl` ha risolto la scala (mossa scelta o `KeepTier`) e
    /// rientra nell'executor (self-loop, come `StallResolved`). Gemello di
    /// [`StopReason::StallResolved`].
    ///
    /// La produce il nodo `ScaleControl` al termine del suo superstep: compare
    /// quindi solo nei run in cui e' stato emesso uno
    /// [`StopReason::ScaleReason`], che ha per gate `agent.scale.enabled`.
    ScaleResolved,
    /// Il nodo `Supervisor` ha completato il check (continue/redirect) e rientra
    /// nell'executor.
    SupervisorResolved,
    /// Il supervisore ha deciso di abbandonare il task: instrada verso chiusura.
    SupervisorAbandon,
    /// Errore provider durante l'executor (`__init__.py:3104-3107`): l'executor
    /// scrive `result="[Errore provider ...]"` (NON vuoto) e `stop_reason="error"`.
    /// Serializza in `"error"` (snake_case): e' il SOLO valore che fa entrare il
    /// punto unico `heuristic_reward` nel ramo 0.0. Senza questa variante quel
    /// ramo era irraggiungibile dallo stato Rust (un run fallito sarebbe stato
    /// premiato 1.0 invece di 0.0). Sul routing cade sul default come in Python
    /// (route_after_executor -> learner; route_after_todo_runner -> executor).
    Error,
}

/// Meta-step semantico pubblicato al frontend chat (`state.py:24`).
///
/// Ogni nodo che vuole emettere uno step semantico (plan/routing/clarify/
/// fallback/reflection) aggiunge un `MetaStep` a `AgentState.meta_steps`; il
/// generator SSE li converte in eventi `{"type":"meta_step",...}`. E' uno dei
/// due canali con reducer `add` (accumulo cross-nodo).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetaStep {
    /// Tipo dello step (plan, routing, clarify, fallback, reflection, ...).
    pub kind: String,
    /// Titolo leggibile mostrato in chat.
    pub title: String,
    /// Payload arbitrario (dipende dal kind).
    #[serde(default)]
    pub payload: Value,
    /// Id di correlazione opzionale (per raggruppare step collegati).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// Timestamp ISO8601 di creazione (opzionale).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

/// Stato condiviso fra tutti i nodi del grafo agentico Nexus.
///
/// Replica `AgentState` (TypedDict, `total=False`) di `brain/agents/state.py`.
/// Tutti i campi sono `#[serde(default)]` (lo stato iniziale puo' omettere
/// qualsiasi campo). Solo `messages` e `meta_steps` hanno reducer `append`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, DeriveGraphState)]
#[serde(default)]
#[graph_state(delta = "StateDelta")]
pub struct AgentState {
    // ── Canali con reducer `add` (gli UNICI due, state.py:17,24) ─────────────
    /// Cronologia messaggi. Reducer `add` (append cross-nodo).
    #[reduce(append)]
    pub messages: Vec<Message>,
    /// Meta-step semantici pubblicati al frontend. Reducer `add`.
    #[reduce(append)]
    pub meta_steps: Vec<MetaStep>,

    // ── Classificazione / intent ─────────────────────────────────────────────
    /// Intent dell'utente classificato dal router.
    pub user_intent: Option<String>,
    /// Confidence della classificazione intent (0..1).
    pub intent_confidence: Option<f64>,
    /// Complessita' del task (segnale del classifier agentico).
    pub task_complexity: Option<TaskComplexity>,
    /// Score agentico (0..1) del classifier.
    pub agentic_score: Option<f64>,
    /// `true` se la richiesta e' ambigua.
    pub is_ambiguous: Option<bool>,
    /// Query arricchita prodotta dal clarify_or_expand (mode=expand).
    pub expanded_query: Option<String>,
    /// `true` se e' stata emessa una richiesta di chiarimento: il run e' TERMINALE
    /// (l'edge condizionale di `clarify_or_expand` instrada a `End` -> `Completed`).
    /// Il prossimo messaggio utente avvia un NUOVO run dall'entry `router`. NON e'
    /// un interrupt-resume (a differenza di `awaiting_confirmation`).
    pub pending_clarify: Option<bool>,
    /// Numero di chiarimenti gia' emessi nel run.
    pub clarify_attempts: Option<i64>,
    /// Numero di domande-chiarimento IDENTICHE gia' poste all'utente nella
    /// SESSIONE (CROSS-RUN), calcolato UNA volta all'avvio del run dal detector
    /// `ClarifyHistoryPort` (mcp-core) e checkpointato qui cosi' sopravvive nel
    /// run corrente e il replay lo rilegge senza re-interrogare il DB
    /// (replay-safe). Alimenta `ProgressSignals::repeated_user_question_count`:
    /// oltre la soglia (`agent.loop.repeated_user_question_threshold`) scatta
    /// l'asse `Axis::RepeatedUserQuestion`. E' il segnale che il loop email
    /// (chat Beaty-Book) attraversava senza mai essere rilevato (la loop-detection
    /// copriva solo le firme di TOOL). `None`/0 su un run normale -> asse mai
    /// attivo -> comportamento invariato. Regola M: la DECISIONE deriva dal
    /// segnale strutturato (i meta_step `kind='clarify'` della sessione), la
    /// firma-testo della domanda e' solo euristica di conteggio.
    pub repeated_clarify_count: Option<i64>,
    /// Intent gia' risolto a monte da mcp-core (salta la ri-classificazione).
    pub intent_hint: Option<String>,
    /// Punto unico: il turno corrente richiede azione con tool?
    pub action_oriented: Option<bool>,
    /// Punto unico: il turno e' di sola lettura/verifica (nessuna modifica
    /// autorizzata)? Derivato FEDELE dal classifier (`derive_report_only`,
    /// `authorizes_changes=false`). Distinto da `action_oriented`: un listing
    /// richiede tool (action=true) ma NON autorizza modifiche (report=true).
    /// I guard che spingono verso tool "produttivi" (strip dei read-only)
    /// devono restare inerti quando e' `Some(true)`.
    pub report_only: Option<bool>,

    // ── Esito dichiarato / governance chiusura ───────────────────────────────
    /// Esito dichiarato dal modello via tool task_complete.
    pub declared_outcome: Option<Value>,
    /// Verdetto del closure_judge (LLM) quando l'esito non e' dichiarato.
    pub closure_verdict: Option<Value>,
    /// Verdetto strutturato del REVISORE via tool review_verdict (Fase B
    /// ultracode): dict normalizzato {verdict, summary, findings[]}. Propagato
    /// oltre il confine sub-run in `structured_verdict` (regola M).
    pub review_verdict: Option<Value>,
    /// Parere strutturato di una FIGURA del consiglio di analisi a monte via tool
    /// advisory_verdict: dict normalizzato {verdict, summary, requirements[],
    /// risks[], recommendations[]}. Propagato oltre il confine sub-run in
    /// `structured_verdict` (campo `advisory`, regola M).
    pub advisory_verdict: Option<Value>,
    /// Posizione strutturata di un AVVOCATO del dibattito a tesi contrapposte via
    /// tool debate_position: dict normalizzato {assigned_position, stance,
    /// summary, key_arguments[], risks[]}. Propagato oltre il confine sub-run in
    /// `structured_verdict` (campo `debate`, regola M).
    pub debate_position: Option<Value>,
    /// `true` se un tool e' fallito per ToolRunner gRPC down (infrastruttura).
    pub tool_infra_error: Option<bool>,
    /// Passi strutturati del playbook matchato.
    pub playbook_steps: Option<Vec<String>>,
    /// Chiave del playbook matchato.
    pub playbook_key: Option<String>,
    /// Conteggio dichiarazioni task_complete outcome=done.
    pub declared_done_count: Option<i64>,
    /// `true` se una dichiarazione blocked e' gia' stata rifiutata (guard).
    pub blocked_cap_rejected: Option<bool>,

    // ── Tool discovery / compressione prefix ─────────────────────────────────
    /// Tool M16 scoperti PERSISTENTI per il run (accumulo dedup per nome).
    pub discovered_tools_run: Option<Vec<Value>>,
    /// Indice di taglio della compressione "a generazioni".
    pub compress_cutoff_index: Option<i64>,
    /// Fase del taglio di compressione.
    pub compress_cutoff_phase: Option<i64>,
    /// Taccuino del run gestito dall'agente (tool nexus_run_notes).
    pub run_notes: Option<String>,

    // ── Routing / esecuzione base ────────────────────────────────────────────
    /// Tipo di task.
    pub task_type: Option<String>,
    /// Modalita' di comportamento (behavior_mode).
    pub behavior_mode: Option<String>,
    /// Budget token del turno.
    pub token_budget: Option<i64>,
    /// Risultato testuale finale del run.
    pub result: Option<String>,
    /// Ragionamento (thinking) accumulato del run: concatenazione dei
    /// `ThinkingDelta` emessi dall'executor a ogni interrogazione. LIVE viaggia
    /// come evento SSE volatile; questo accumulatore lo rende persistibile nel
    /// `metadata.reasoning` del messaggio assistant (FIX divergenza chat
    /// post-refresh). `None`/vuoto se il modello non ha prodotto thinking.
    pub reasoning_acc: Option<String>,
    /// Provider effettivamente usato.
    pub provider_used: Option<String>,
    /// Modello effettivamente usato.
    pub model_used: Option<String>,
    /// Punteggio di feedback (telemetria).
    pub feedback_score: Option<f64>,
    /// Latenza in millisecondi.
    pub latency_ms: Option<f64>,
    /// Token usati (aggregato legacy).
    pub token_usage: Option<i64>,
    /// Numero di iterazioni eseguite.
    pub iterations: Option<i64>,
    /// Id del thread LangGraph (= run_id Nexus).
    pub thread_id: Option<String>,

    // ── Agent tool loop ──────────────────────────────────────────────────────
    /// Tool_use pendenti emessi dall'executor.
    pub pending_tool_uses: Option<Vec<Value>>,
    /// Motivo di arresto del turno executor.
    pub stop_reason: Option<StopReason>,
    /// Firme delle ultime tool calls (loop detection).
    pub recent_tool_signatures: Option<Vec<String>>,
    /// Tool dichiarati al modello (schema Anthropic-compatible).
    pub tools_json: Option<Vec<Value>>,
    /// Tool scoperti da iniettare come native nel SOLO turno successivo.
    /// Overwrite: `[]` azzera (durata esatta 1 turno) -> distinzione
    /// `None` (no-op) vs `Some(vec![])` (azzera) LOAD-BEARING.
    pub discovered_tools_next_turn: Option<Vec<Value>>,
    /// System prompt del profilo agente (vuoto = default).
    pub system_text: Option<String>,
    /// Id della sessione (iniettato dal chiamante).
    pub session_id: Option<String>,
    /// `true` dopo /agent/approve (HITL): salta interrupt in loop.
    pub approved: Option<bool>,
    /// Provider forzato dall'esterno (override routing).
    pub provider_override: Option<String>,
    /// Modello forzato dall'esterno (override routing).
    pub model_override: Option<String>,
    /// Profilo agente selezionato (core/github/specialized/general).
    pub profile_name: Option<String>,

    // ── Metriche AI estese ────────────────────────────────────────────────────
    /// Token di prompt LORDI dell'ultimo turno: quanto contesto e' stato
    /// inviato, cache compresa (convenzione unica del sistema, vedi
    /// `nexus_gateway::LlmUsage`).
    ///
    /// E' il lordo perche' e' l'unica quantita' monotona rispetto alla crescita
    /// della history: la quota servita da cache la decide il provider turno per
    /// turno. Alimenta il delta anti-runaway (`executor`) e il riempimento della
    /// finestra di contesto (`last_prompt_tokens` in UI).
    pub prompt_tokens: Option<i64>,
    /// Token di completion.
    pub completion_tokens: Option<i64>,
    /// Token di creazione cache: sottoinsieme di `prompt_tokens`, non un addendo.
    pub cache_creation_tokens: Option<i64>,
    /// Token letti da cache: sottoinsieme di `prompt_tokens`, non un addendo.
    pub cache_read_tokens: Option<i64>,
    /// Token totali.
    pub total_tokens: Option<i64>,
    /// Costo totale in USD.
    pub total_cost_usd: Option<f64>,
    /// Tasso di cache hit.
    pub cache_hit_rate: Option<f64>,
    /// Temperature usata.
    pub temperature: Option<f64>,
    /// Top-p usato.
    pub top_p: Option<f64>,
    /// Timestamp ISO8601 di creazione.
    pub created_at: Option<String>,
    /// Timestamp ISO8601 di completamento.
    pub completed_at: Option<String>,

    // ── Self-reflection ────────────────────────────────────────────────────────
    /// Punteggio della reflection (0..1).
    pub reflection_score: Option<f64>,
    /// Dettaglio per dimensione della reflection.
    pub reflection_dimensions: Option<Value>,
    /// Punti deboli rilevati.
    pub reflection_weaknesses: Option<Vec<Value>>,
    /// Suggerimenti di miglioramento.
    pub reflection_suggestions: Option<Vec<Value>>,
    /// Reward finale fuso.
    pub final_reward: Option<f64>,

    // ── Plan / Act / Verify ────────────────────────────────────────────────────
    /// `true` quando il planner ha prodotto un piano.
    pub plan_phase_active: Option<bool>,
    /// Motivo dello skip del planner.
    pub plan_phase_skip_reason: Option<String>,
    /// UUID del plan corrente.
    pub current_plan_id: Option<String>,
    /// Snapshot dei todos letti dal DB.
    pub current_todos: Option<Vec<Value>>,
    /// Acceptance criteria globali del plan.
    pub acceptance_criteria: Option<Vec<Value>>,
    /// Id del todo attivo.
    pub active_todo_id: Option<String>,
    /// Razionale decisionale del planner forte.
    pub plan_rationale: Option<String>,
    /// Vincoli del plan.
    pub plan_constraints: Option<Vec<String>>,
    /// Alternative considerate dal planner.
    pub plan_alternatives: Option<Vec<Value>>,
    /// Contesto RAG recuperato prima di pianificare.
    pub plan_rationale_context: Option<String>,
    /// Brief di comprensione pre-planning.
    pub context_brief: Option<String>,
    /// `true` se il nodo understanding e' attivo.
    pub understanding_active: Option<bool>,
    /// Motivo dello skip di understanding.
    pub understanding_skip_reason: Option<String>,
    /// Contatore per il reminder injection dei todo.
    pub since_last_todo_reminder: Option<i64>,
    /// Ciclo verifier corrente per active_todo.
    pub verify_cycle: Option<i64>,
    /// Ciclo della verifica esplorativa LLM.
    pub exploratory_verify_cycle: Option<i64>,
    /// Cap globale per run della verifica esplorativa.
    pub exploratory_verify_total: Option<i64>,
    /// Ciclo del final gate generale. E' un CONTATORE ("quanti giri ha fatto il
    /// gate"): non risponde e non deve rispondere a "com'e' andata la verifica"
    /// — per quello c'e' [`AgentState::final_gate_verdict`] (regola M).
    pub final_gate_cycle: Option<i64>,
    /// ESITO dell'ultimo verdetto del final gate: il segnale STRUTTURATO che
    /// dice "com'e' andata" (regola M). Lo scrive OGNI ramo del gate, e i
    /// consumatori lo leggono con un `match` esaustivo invece di dedurre
    /// l'esito da `final_gate_cycle`.
    ///
    /// Perche' esiste: l'esito era INFERITO dal contatore
    /// (`final_gate_cycle > 0 && !plan_phase_active`) sulla base di
    /// un'enumerazione dei rami del gate scritta a mano nella doc del
    /// consumatore — enumerazione FALSA. Il turno di GRAZIA
    /// (`final_gate.rs`, ramo `completion_grace`) lascia di proposito
    /// `cycle = max_cycles` pur avendo TUTTI i criteri oggettivi PASSATI
    /// ("nessun nuovo campo di stato", diceva il commento): quel residuo veniva
    /// letto come "verifica fallita e non ripetuta" e un lavoro riuscito
    /// chiudeva `FailedDiagnosed`. Lo stesso falso positivo era gia' emerso per
    /// la plan-phase ed era stato tappato con un'eccezione ad-hoc su
    /// `!plan_phase_active`, invece di dare all'esito un campo proprio.
    ///
    /// `None` = il gate non e' mai entrato in questo run.
    pub final_gate_verdict: Option<FinalGateVerdict>,
    /// Ciclo del ReviewGate (CONTATORE dei rimandi in correzione della review
    /// adversariale; gemello di `final_gate_cycle`; `review_gate_*` e non
    /// `review_*`: `review_verdict` e' gia' il CANALE DI RUOLO del revisore, mai riusato quello: il
    /// residuo di un contatore altrui e' gia' stato causa di un falso
    /// `FailedDiagnosed`, vedi doc di `final_gate_verdict`).
    #[serde(default)]
    pub review_gate_cycle: Option<i64>,
    /// ESITO dell'ultimo passaggio dal ReviewGate (regola M): il segnale che il
    /// finalizzatore legge per `review_panel_rejected`. `None` = nodo mai
    /// raggiunto (motore vecchio o run chiuso per altra via).
    #[serde(default)]
    pub review_gate_verdict: Option<ReviewGateVerdict>,
    /// WATERMARK della misura di progresso: `id` dell'ultima scrittura registrata
    /// quando e' stato emesso l'ultimo rimando in correzione. Al rientro nel gate
    /// dice DA DOVE guardare per rispondere a "questo rimando ha prodotto
    /// qualcosa?" (punto unico del criterio:
    /// [`crate::decisions::correction_progress`]).
    ///
    /// E' un `id` e non un istante: l'orologio dell'applicazione e quello del DB
    /// non delimitano la stessa finestra. `None` = nessun rimando ancora emesso,
    /// oppure misura non disponibile (porta assente o in errore) — in entrambi i
    /// casi il gate ricade sul comportamento storico e convoca.
    #[serde(default)]
    pub review_correction_watermark: Option<i64>,
    /// Quanti rimandi in correzione NON hanno prodotto alcuna modifica ai file.
    /// Confrontato con `review_gate_cycle` (che conta i rimandi TOTALI) distingue
    /// "non ha mai provato" da "ha provato e non ci e' riuscito": e' il segnale
    /// che porta a [`ReviewGateVerdict::RejectedNoCorrection`] e la premessa
    /// strutturata (regola M) per chi in futuro vorra' cambiare figura al secondo
    /// giro a vuoto invece di ripetere la stessa richiesta allo stesso modello.
    #[serde(default)]
    pub review_correction_no_progress: Option<i64>,
    /// DECISIONE DI ROUTING dell'ultimo gate di chiusura eseguito (regola M):
    /// il segnale, di PROPRIETA' del gate, su cui instradano gli edge di
    /// final_gate e review_gate. Lo scrive OGNI ramo di uscita dei due nodi.
    /// `None` = nessun gate ancora eseguito -> si chiude (ramo sicuro).
    /// Vedi [`GateRouting`] per il difetto che ha reso necessario il campo.
    #[serde(default)]
    pub gate_routing: Option<GateRouting>,

    /// `true` quando il final gate ha PASSATO la verifica E2E (esito canonico
    /// CompletedVerified lato mcp-core). Settato solo sul ramo PASSED del
    /// `final_gate_node`; il ramo forced_close/cap NON lo imposta (resta
    /// FailedDiagnosed). Vedi `final_gate.py:521`.
    pub final_gate_passed: Option<bool>,
    /// `true` quando il final gate e' ENTRATO (task software) ma NON ha potuto
    /// eseguire la verifica tecnica dell'ambiente perche' il profilo di verifica
    /// (ADR 0036) non e' disponibile: il lavoro c'e' ma non e' stato verificato.
    /// Segnale STRUTTURATO (regola M) letto dal finalizzatore per l'esito ONESTO
    /// `CompletedUnverified` (distinto da `Completed`/`CompletedVerified`): mai un
    /// "completato" muto quando la verifica non e' stata proprio eseguita. `None`
    /// = gate non entrato o verifica eseguita.
    pub final_gate_unverified: Option<bool>,
    /// Ultimo risultato del verifier.
    pub verifier_last_result: Option<Value>,
    /// Contatore revisioni strutturali del plan.
    pub plan_revisions: Option<i64>,
    /// Domande di chiarimento pendenti emesse dal planner (HITL Confirm): se
    /// valorizzato il turno si ferma in attesa della risposta utente. `None`
    /// (assente) abilita la pre-flight clarifying del planner; una lista (anche
    /// vuota) la salta. Forma opaca (lista di `{id, question, suggested_default}`).
    pub pending_clarifications: Option<Vec<Value>>,
    /// Assunzioni di default applicate dal planner in modalita' autonoma
    /// (Automatico/Continuo) al posto di fermarsi per chiarimenti: trasparenza
    /// (le stesse domande con il loro `suggested_default`). Forma opaca.
    pub applied_default_assumptions: Option<Vec<Value>>,

    // ── Sub-agents ──────────────────────────────────────────────────────────────
    /// Id del run genitore (se sub-run).
    pub parent_run_id: Option<String>,
    /// Profondita' del sub-agente.
    pub subagent_depth: Option<i64>,
    /// Risultati dei sub-agenti.
    pub subagent_results: Option<Vec<Value>>,
    /// Sub-run attivi.
    pub active_subagent_runs: Option<Vec<String>>,
    /// Costo cumulativo dei sub-agenti in USD.
    pub subagent_cost_cumulative_usd: Option<f64>,
    /// Numero di retry gia' consumati dal todo_runner per il todo corrente
    /// (`todo_runner_node.py:308`, `int(state.get("todo_isolation_retries") or 0)`).
    /// Letto prima del retry, valorizzato a `extra_retries` da `_advance_patch`.
    pub todo_isolation_retries: Option<i64>,

    // ── Allegati / budget ─────────────────────────────────────────────────────
    /// Byte cumulativi letti via read_attachment/read_archive_entry.
    pub attachment_read_bytes: Option<i64>,

    // ── G1 / loop-detection ────────────────────────────────────────────────────
    /// Contatore nudge G1 iniettati.
    pub action_nudge_count: Option<i64>,
    /// Contatore re-routing G1 verso executor.
    pub g1_reroute_count: Option<i64>,
    /// Chiamate consecutive di sola esplorazione.
    pub consecutive_exploration_calls: Option<i64>,
    /// `true` dopo aver iniettato il nudge anti-esplorazione.
    pub exploration_nudge_sent: Option<bool>,
    /// `true` dopo aver iniettato il nudge anti-loop-comando.
    pub repeated_cmd_nudge_sent: Option<bool>,
    /// Token di LAVORO INCREMENTALE cumulati sul run (regola M: segnale
    /// strutturato dal gateway, mai stima dal testo): per ogni turno si somma il
    /// DELTA del prompt rispetto al turno precedente (solo contesto nuovo, non la
    /// history ri-inviata) + completion. I token di cache NON sono addendi: sono
    /// un SOTTOINSIEME del prompt lordo, quindi il delta li comprende gia' e
    /// sommarli li conterebbe due volte (vedi `executor.rs`, ramo del delta).
    /// La vecchia semantica (somma dei `total_tokens` lordi per-turno)
    /// condannava i run con contesto
    /// grande: history ~50k -> ~8 turni sani esaurivano il budget -> cascata di
    /// escalation fino al cap. Safety net anti-runaway per-run: quando
    /// `>= ExecutorConfig::run_token_budget` (se il budget e' `> 0`) l'executor
    /// chiude deterministicamente PRIMA della prossima chiamata LLM. Reducer
    /// overwrite (last-write): l'executor legge il valore portato dallo stato,
    /// somma il turno e riscrive il totale (come `iterations`). `None`/0 su un
    /// run appena avviato -> nessun runaway rilevato. `i64` per coerenza serde
    /// col resto dei contatori (letto/scritto come non-negativo).
    pub tokens_used_total: Option<i64>,
    /// Costo CUMULATIVO in USD del run (somma di `LlmUsage.total_cost_usd` di ogni
    /// turno, ognuno col prezzo del modello di QUEL turno -> esatto anche dopo
    /// un'escalation cross-tier). Freno di spesa per-RUN: quando `>=
    /// ExecutorConfig::run_cost_budget_usd` (se `> 0`) l'executor chiude
    /// deterministicamente. A DIFFERENZA di `tokens_used_total` NON si azzera
    /// all'escalation: e' il tetto dell'intero run, non del turno-modello (che resta
    /// governato dal trigger token). Reducer overwrite (last-write): l'executor legge
    /// il valore portato dallo stato, somma il costo del turno e riscrive il totale
    /// (come `tokens_used_total`). `None` = nessun costo noto ancora (turno senza
    /// prezzo in catalog o run appena avviato).
    pub run_cost_cumulative_usd: Option<f64>,
    /// Epoch UNIX (secondi) di AVVIO del run primario, valorizzato UNA volta da
    /// `build_initial_state` e CHECKPOINTATO: la deadline di run
    /// (`ExecutorConfig::run_time_budget_s`) misura il tempo di parete del run
    /// INTERO anche dopo un resume/recovery, non dell'ultimo spezzone. `None` =
    /// run avviato prima della fase 3: nessun enforcement (mai un default
    /// inventato). Reducer overwrite come gli altri contatori.
    pub run_started_at_epoch_s: Option<i64>,
    /// Turni solo-testo CONSECUTIVI del run (la risposta LLM non conteneva tool_use
    /// mentre il loop si aspettava azioni; segnale strutturato `LlmResponse.tool_calls`,
    /// regola M). Azzerato appena il modello emette un tool_use, incrementato quando
    /// non lo fa. Quando `>= ExecutorConfig::max_consecutive_text_only_turns` (se
    /// `> 0`) l'executor chiude deterministicamente: fast-fail sul modello che descrive
    /// senza agire (pattern gemini che ignora `force_tool_choice`). Reducer overwrite.
    pub consecutive_text_only_turns: Option<i64>,

    // ── progress_controller ────────────────────────────────────────────────────
    /// Assi di stallo gia' guidati (GUIDE applicata) in questo run.
    pub progress_guided_axes: Option<Vec<String>>,
    /// Assi gia' passati per la diagnosi forzata.
    pub progress_diagnosed_axes: Option<Vec<String>>,
    /// Assi gia' passati per il CAMBIO DI STRATEGIA forzato (livello 1.9 del
    /// progress_controller: prima si cambia strada, poi il modello).
    pub progress_strategy_axes: Option<Vec<String>>,
    /// `true` quando un abort coordinato ha chiuso senza verifica.
    pub forced_close_unverified: Option<bool>,
    /// `true` quando QUESTO turno ha chiuso perche' il gateway LLM ha fallito
    /// (provider down/billing/rate-limit/richiesta troppo grande) e l'executor
    /// ha sintetizzato il testo `[Errore provider ...]` (regola M). Segnale
    /// STRUTTURATO gemello di `forced_close_unverified`, per lo stesso identico
    /// motivo: senza, mcp-core doveva rileggere il PREFISSO di quel testo per
    /// sapere se un run "completed" fosse in realta' un fallimento
    /// infrastrutturale — un contratto tenuto per copia fra due crate, in
    /// italiano, dentro un campo di DISPLAY. Scritto ESPLICITAMENTE ad ogni
    /// turno (`Some(true)`/`Some(false)`, mai lasciato ereditato): il turno
    /// riuscito lo azzera, cosi' un run che si e' ripreso dopo un errore
    /// gateway non resta etichettato per sempre.
    pub provider_error_close: Option<bool>,

    // ── Sticky cascade ──────────────────────────────────────────────────────────
    /// Provider sticky dopo un cascade riuscito.
    pub sticky_provider: Option<String>,
    /// Modello sticky dopo un cascade riuscito.
    pub sticky_model: Option<String>,
    /// Provider sticky specifico del planner.
    pub planner_sticky_provider: Option<String>,
    /// Modello sticky specifico del planner.
    pub planner_sticky_model: Option<String>,

    // ── Scale-controller (FIX-A: tier autoritativo checkpointato) ────────────────
    /// Tier di scala CORRENTE del run (`light`/`medium`/`heavy`), stato
    /// AUTORITATIVO checkpointato dello SCALE-CONTROLLER (FIX-A, regola H): scritto
    /// dal ramo che risolve il modello (routing iniziale / smart-upscale /
    /// escalation) col `performance_tier` gia' noto al pick, MAI ricalcolato via DB
    /// a volo (romperebbe il determinismo di replay). Il modulo puro
    /// [`crate::decisions::scale_reason::build_scale_context`] lo legge come segnale
    /// (fallback deterministico `medium` se assente, default catalog mig 0032).
    ///
    /// SCRITTO in PR-B1 (FIX-A): il routing iniziale (`native_engine`) lo popola col
    /// tier del primo modello dal catalog; i call-site che cambiano modello
    /// (escalation/failover in `executor`, upscale via `UpscalePick::tier`) lo
    /// aggiornano col `performance_tier` gia' noto al pick — le porte
    /// `EscalationPort`/`ModelUpscalePort` trasportano il tier (`ChainEntry::tier`,
    /// `CrossProviderCandidate::tier`, `UpscalePick::tier`). NESSUN decisore lo legge
    /// ancora (detector/nodo scale = PR-B2/B3): il routing usa `sticky_provider/model`,
    /// non `current_tier` -> comportamento invariato -> bit-identico. `None` (es.
    /// cascade fallback, dove il tier non e' disponibile senza I/O nel path del turno)
    /// -> ogni consumatore (PR-B) ricade sul default `medium`.
    pub current_tier: Option<String>,

    /// Finestra di contesto EFFETTIVA (token) dell'ultimo turno LLM eseguito:
    /// quella del modello richiesto dalla config, oppure quella del modello
    /// PROMOSSO quando lo smart-upscale e' scattato (context_overflow). Scritta
    /// dall'executor a ogni turno; il ToolDispatchNode la usa per il predictive
    /// context cap al posto della finestra statica di config (regola H,
    /// incidente 2026-07-06: gate fermo alla finestra del modello di partenza
    /// mentre le chiamate LLM giravano gia' sul modello promosso -> tutti i
    /// tool bloccati per sempre). `None` (primo turno / checkpoint storici) o
    /// `<=0` -> il gate ricade sulla finestra di config (comportamento storico).
    pub effective_context_window: Option<i64>,

    // ── Automazione ─────────────────────────────────────────────────────────────
    /// Modalita' automazione del turno chat propagata da mcp-core.
    pub automation_mode: Option<AutomationMode>,
    /// Modalita' supervisore worker scelta in UI (none/anomaly/interleaved/continuous).
    pub supervisor_mode: Option<SupervisorMode>,

    // ── HITL (predicato di interrupt-resume) ─────────────────────────────────────
    /// `true` quando lo stato attende una conferma umana (HITL). Non presente
    /// nel TypedDict come campo top-level: lo aggiungiamo qui esplicitamente
    /// perche' e' l'UNICO predicato di interrupt-resume del runtime (il motore
    /// SOSPENDE lo stesso run con `Interrupted`). Distinto da `pending_clarify`,
    /// che e' uno stato TERMINALE gestito dalla topologia (edge a `End`).
    pub awaiting_confirmation: Option<bool>,
    /// `true` quando lo stato attende il completamento dei sub-run BACKGROUND
    /// dispatchati (fan-in deterministico, Fase D). Secondo motivo di
    /// interrupt-resume gemello di `awaiting_confirmation`: il motore sospende lo
    /// STESSO run; il resume (innescato dal completamento dell'ultimo figlio, non
    /// da un'azione utente) azzera questo flag e inietta i `subagent_results`.
    pub awaiting_subagents: Option<bool>,

    // ── Schema aperto ───────────────────────────────────────────────────────────
    /// Campi runtime non promossi a campi nativi (`iteration_budget`,
    /// `complexity_score`, `project_id`, `auto_escalations`, ...). `flatten`
    /// preserva qualsiasi chiave sconosciuta nel round-trip e tollera l'avanti
    /// durante la coesistenza Python<->Rust (regola: non-perdita).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl AgentState {
    /// Forma OSSERVATA dell'ultimo turno, per chi deve stimare quanto costerebbe
    /// la prossima chiamata (l'escalation, che sceglie fra modelli).
    ///
    /// Punto unico della conversione (regola L): i quattro call site
    /// dell'escalation la chiedono qui invece di ricostruirla ognuno dai campi,
    /// dove sbagliare `Option` o scambiare prompt e completion non sarebbe un
    /// errore visibile — solo un confronto di costi che si inverte in silenzio.
    ///
    /// `prompt_tokens` e' il LORDO, cache compresa: e' la stessa convenzione di
    /// [`nexus_pricing::TokenUsage`], che scorpora la cache al proprio interno.
    /// Un turno non ancora misurato (`None`) da' una forma ignota, che riporta
    /// l'escalation all'ordine di listino.
    pub fn turn_shape(&self) -> crate::runtime::ports::TurnShape {
        crate::runtime::ports::TurnShape {
            prompt_tokens: self.prompt_tokens.unwrap_or(0),
            completion_tokens: self.completion_tokens.unwrap_or(0),
        }
    }

    /// `true` se lo stato richiede una conferma umana prima di proseguire.
    pub fn is_awaiting_confirmation(&self) -> bool {
        self.awaiting_confirmation.unwrap_or(false)
    }

    /// `true` se lo stato attende il completamento dei sub-run background
    /// (fan-in deterministico, Fase D).
    pub fn is_awaiting_subagents(&self) -> bool {
        self.awaiting_subagents.unwrap_or(false)
    }

    /// `true` se lo stato attende un chiarimento dall'utente (disambiguazione).
    /// Usato dalla TOPOLOGIA (edge condizionale di `clarify_or_expand` -> `End`),
    /// NON come predicato di interrupt del motore: il run si CHIUDE (terminale).
    pub fn is_pending_clarify(&self) -> bool {
        self.pending_clarify.unwrap_or(false)
    }

    /// True se `automation_mode` e' autonomo (`automatic` / `continuous`).
    pub fn is_autonomous_run(&self) -> bool {
        self.automation_mode
            .map(AutomationMode::is_autonomous)
            .unwrap_or(false)
    }
}

/// Impl del trait di runtime `nexus_graph::GraphState`.
///
/// Il runtime lavora su un `nexus_graph::StateDelta` OPACO (mappa JSON): qui lo
/// deserializziamo nello `StateDelta` tipizzato e deleghiamo a `merge_typed`
/// (generato dal derive). La semantica reducer resta in UN solo posto (regola L);
/// l'impl del trait e' solo l'adattatore mappa-opaca -> delta-tipizzato.
impl nexus_graph::GraphState for AgentState {
    fn merge(&mut self, delta: nexus_graph::StateDelta) {
        // Il delta opaco e' una mappa JSON {campo -> valore}. La sua
        // deserializzazione nello StateDelta tipizzato porta ogni chiave PRESENTE
        // a `Some(...)` e ogni chiave ASSENTE a `None` (no-op), preservando la
        // distinzione load-bearing chiave-assente vs chiave-presente.
        let map = serde_json::Value::Object(delta.as_map().clone());
        match serde_json::from_value::<StateDelta>(map) {
            Ok(typed) => self.merge_typed(typed),
            Err(err) => {
                // Niente panic (regola: errori espliciti): un delta malformato e'
                // un bug del nodo chiamante; lo logghiamo e non tocchiamo lo stato.
                tracing::error!(
                    target: "nexus_agent_graph::state",
                    error = %err,
                    "StateDelta opaco non deserializzabile: merge ignorato"
                );
            }
        }
    }

    fn is_awaiting_interrupt(&self) -> bool {
        // PUNTO UNICO (regola L): il motore sospende su QUALSIASI motivo di
        // interrupt-resume. Oggi due: conferma umana (HITL) e attesa dei sub-run
        // background (fan-in, Fase D). Il resume azzera il flag specifico.
        AgentState::is_awaiting_confirmation(self) || AgentState::is_awaiting_subagents(self)
    }

    fn interrupt_resume_node(
        &self,
        interrupted_after: nexus_graph::node::NodeId,
        routed_next: nexus_graph::node::NodeId,
    ) -> nexus_graph::node::NodeId {
        // HITL: tool_dispatch ha sospeso PRIMA di eseguire i mutators pendenti.
        // Al resume rientra in tool_dispatch (con approved=true nel delta), non
        // nell'executor gia' instradato (evita un turno LLM che consumerebbe il
        // prossimo script prima del dispatch dei pending approvati).
        if self.is_awaiting_confirmation()
            && interrupted_after == nexus_graph::node::NodeId::ToolDispatch
        {
            interrupted_after
        } else {
            routed_next
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Fase D: il predicato di interrupt del motore (`is_awaiting_interrupt`,
    /// PUNTO UNICO) compone conferma umana (HITL) E attesa dei sub-run background.
    /// `Some(false)` (azzeramento esplicito del resume) NON sospende.
    #[test]
    fn is_awaiting_interrupt_compone_conferma_e_subagents() {
        use nexus_graph::GraphState;
        assert!(
            !GraphState::is_awaiting_interrupt(&AgentState::default()),
            "nessun flag -> nessun interrupt"
        );
        assert!(GraphState::is_awaiting_interrupt(&AgentState {
            awaiting_confirmation: Some(true),
            ..Default::default()
        }));
        let fanin = AgentState {
            awaiting_subagents: Some(true),
            ..Default::default()
        };
        assert!(GraphState::is_awaiting_interrupt(&fanin), "fan-in sospende");
        assert!(fanin.is_awaiting_subagents());
        assert!(!fanin.is_awaiting_confirmation(), "flag indipendenti");
        assert!(
            !GraphState::is_awaiting_interrupt(&AgentState {
                awaiting_subagents: Some(false),
                ..Default::default()
            }),
            "Some(false) azzera l'interrupt (load-bearing per il resume)"
        );
    }

    #[test]
    fn interrupt_resume_node_hitl_riparte_da_tool_dispatch() {
        use nexus_graph::GraphState;
        use nexus_graph::node::NodeId;
        let hitl = AgentState {
            awaiting_confirmation: Some(true),
            ..Default::default()
        };
        assert_eq!(
            GraphState::interrupt_resume_node(&hitl, NodeId::ToolDispatch, NodeId::Executor),
            NodeId::ToolDispatch,
            "HITL sospeso in tool_dispatch -> resume li', non nell'executor instradato"
        );
        assert_eq!(
            GraphState::interrupt_resume_node(&hitl, NodeId::Executor, NodeId::FinalGate),
            NodeId::FinalGate,
            "altri nodi restano sul routed_next"
        );
    }

    /// Round-trip serde di `AgentState`: serialize -> deserialize identico, con
    /// un campo extra SCONOSCIUTO preservato (flatten dello schema aperto).
    #[test]
    fn round_trip_serde_preserva_extra_sconosciuto() {
        // JSON con campi noti + un campo runtime non promosso a campo nativo.
        let raw = json!({
            "user_intent": "code_write",
            "iterations": 7,
            "discovered_tools_next_turn": [],
            "automation_mode": "automatic",
            "stop_reason": "tool_use",
            // Campi NON presenti nello struct: devono finire in `extra` (flatten).
            "iteration_budget": 42,
            "project_id": "proj-123",
            "auto_escalations": ["a", "b"],
        });

        // Deserializza: i campi noti popolano i campi nativi, gli ignoti `extra`.
        let state: AgentState = serde_json::from_value(raw.clone()).expect("deserialize");
        assert_eq!(state.user_intent.as_deref(), Some("code_write"));
        assert_eq!(state.iterations, Some(7));
        assert_eq!(state.discovered_tools_next_turn, Some(vec![]));
        assert_eq!(state.automation_mode, Some(AutomationMode::Automatic));
        assert_eq!(state.stop_reason, Some(StopReason::ToolUse));
        // Le chiavi sconosciute sono catturate da `extra` (flatten).
        assert_eq!(state.extra.get("iteration_budget"), Some(&json!(42)));
        assert_eq!(state.extra.get("project_id"), Some(&json!("proj-123")));
        assert_eq!(
            state.extra.get("auto_escalations"),
            Some(&json!(["a", "b"]))
        );

        // Round-trip: re-serializza e ri-deserializza; deve essere identico.
        let serialized = serde_json::to_value(&state).expect("serialize");
        let back: AgentState = serde_json::from_value(serialized).expect("re-deserialize");
        // Confronto via JSON canonico (serde_json::Value ignora l'ordine chiavi).
        assert_eq!(
            serde_json::to_value(&state).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
        // L'extra sconosciuto sopravvive al giro completo.
        assert_eq!(back.extra.get("iteration_budget"), Some(&json!(42)));
        assert_eq!(back.extra.get("auto_escalations"), Some(&json!(["a", "b"])));
    }

    /// I tre enum dedicati serializzano/deserializzano col rename atteso
    /// (snake_case), coerente col contratto verso il brain Python.
    #[test]
    fn enum_rename_snake_case() {
        assert_eq!(
            serde_json::to_value(StopReason::G1Escalated).unwrap(),
            json!("g1_escalated")
        );
        assert_eq!(
            serde_json::to_value(AutomationMode::Continuous).unwrap(),
            json!("continuous")
        );
        assert_eq!(
            serde_json::to_value(TaskComplexity::High).unwrap(),
            json!("high")
        );
        let r: StopReason = serde_json::from_value(json!("loop_abort")).unwrap();
        assert_eq!(r, StopReason::LoopAbort);
    }
}
