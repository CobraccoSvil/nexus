//! `ClarifyOrExpandNode` — porta la parte PORTABILE/deterministica di
//! `clarify_or_expand_node` (`brain/agents/clarify_or_expand_node.py:587-898`).
//!
//! Il nodo e' condizionale e produce tre output mutuamente esclusivi:
//!   - `ask` CON interlocutore -> meta_step `clarify` con la domanda +
//!                 `pending_clarify=true` (il grafo va a END, il turno si ferma)
//!                 + `clarify_attempts+1`.
//!   - `ask` SENZA interlocutore -> meta_step `clarify_assumption` con
//!                 l'assunzione dichiarata in `applied_default_assumptions`, e
//!                 il run PROSEGUE. Vedi sotto.
//!   - `expand` -> popola `expanded_query` (arricchisce il retrieve RAG; il
//!                 messaggio utente passa intatto al modello principale).
//! Negli altri casi e' no-op (delta vuoto), il flusso prosegue invariato.
//!
//! ## La domanda posta a nessuno (18/08/2026)
//!
//! Il nodo non si attiva «solo sull'1% dei casi»: con
//! `clarify.confirm_irreversible_in_auto=true` (valore in produzione)
//! `force_classify` disarma sia il gate di autonomia sia quello di confidence, e
//! la chiamata LLM parte per OGNI run che arrivi qui senza `intent_hint` — cioe'
//! per ogni sub-run, che per costruzione non ne ha. Quando la decisione torna
//! `mode=ask` con `category=product`, il gate residuo non l'assorbe e il nodo
//! emette `pending_clarify`, che l'edge del grafo instrada a `End` come stato
//! TERMINALE: il sub-run chiude «completed» a zero iterazioni, per porre una
//! domanda a un interlocutore che non esiste.
//!
//! Misurato su `app-libri-18-08` (`ui_ux_designer`, 0 iterazioni, 0 token,
//! summary vuoto, 2,6 s) e il giorno prima su un sub-run `review`. Il criterio
//! che mancava e' [`Interlocutore`] — vedi quel modulo per la catena completa e
//! per il perche' NON e' la modalita' di automazione.
//!
//! ## Cosa porta QUESTO PR (deterministico, testato golden 1:1)
//!
//! - **La catena di gate di skip pre-LLM** (`clarify_or_expand_node.py:596-753`):
//!   flag `enabled` OFF, `intent_hint` presente (disambiguazione gia' risolta),
//!   intent conversazionale con `agentic_score <= smalltalk_max` (small-talk),
//!   short-circuit autonomia (`is_auto && !force_classify`), tetto
//!   `clarify_attempts >= max_attempts` (fail-open verso l'azione, mig 0386),
//!   soglia `confidence >= threshold && !force_classify`, `user_msg` troppo
//!   corto (`< 3`). Punto unico in `Self::pre_llm_gate` (regola L): ritorna
//!   `GateOutcome::Skip` o `GateOutcome::CallLlm`.
//! - **`is_auto`** (`:691-692`): normalizzazione dell'`automation_mode` ai 5
//!   alias che valgono "automatico".
//! - **`force_classify`** (`:693`): `is_auto && confirm_irreversible_in_auto`.
//! - **`last_user_message`** (`:186-191`): ultimo messaggio di ruolo human/user
//!   (in reverse), content stringa, NON trimmato in selezione (il trim arriva a
//!   valle, `:643`).
//! - **`truncate_question`** (`:854-856`): troncamento a `max_question_chars`
//!   con `rsplit(" ", 1)` + `"..."` (rimuove l'ultima parola spezzata).
//! - **Il ramo `ask`** (`:841-885`): gate `force_classify` (auto + decisione
//!   reversibile/technical -> procede autonomo, no domanda) + costruzione del
//!   meta_step `clarify` + `pending_clarify=true` + `clarify_attempts+1`.
//! - **Il ramo `expand`** (`:887-895`): validazione `expanded != "" &&
//!   expanded != user_msg` -> set di `expanded_query`.
//! - **Normalizzazione dell'output LLM** (`:830-833`): `mode`/`category`/
//!   `reversible` con i default del Python (in `LlmDecision::from_tool_input`).
//!
//! ## Cosa NON porta (I/O delegato dietro i trait, TODO espliciti)
//!
//! - La **chiamata LLM** che produce la decisione `clarify_or_expand`
//!   (`:809-834`): passa per `ctx.llm` (`LlmGateway`). Il provider/model sono
//!   RISOLTI A MONTE (regola G): finche' non c'e' la porta che fornisce il
//!   purpose `clarify_expand` risolto + il system prompt `agent.clarify.base`
//!   dal registry, il nodo li riceve dalla `ClarifyConfig` (TODO chiamante). I
//!   test usano lo stub `LlmGateway`.
//! - Il **project_context** (`_build_project_context`, `:535-584`): UNA
//!   `list_files` sulla root via `ctx.tools` (`ToolExecutor`) + rilevamento
//!   marcatori di dominio. Deterministico nel parsing (replicato qui), I/O dietro
//!   la porta. Best-effort: errore -> blocco vuoto.
//! - I **sotto-gate Comp.1 / Cluster 4** (`_intake_gate`, `_lookup_existing_decision`,
//!   `_note_implementation_status`, `_apply_intake_verdict`, `:194-532`): sono
//!   OFF di default (`intake_gate_enabled=false`, `decision_lookup_enabled=false`)
//!   e quasi-interamente I/O (ricerca KB via HTTP + classificazione LLM + query
//!   DB sullo stato di implementazione). Restano un TODO delegato: vanno portati
//!   quando esistera' la porta KB-search (oggi non c'e' un trait dedicato) +
//!   l'accesso DB strutturato. Con i default DB il flusso NON li attraversa,
//!   quindi nessun comportamento divergente.
//! - `meta_steps.persist_async` (`:106-139`): la persistenza best-effort del
//!   meta_step su `nexus_agent_meta_steps` e' un side-effect del brain; nel
//!   runtime Rust il meta_step viaggia nel delta (`meta_steps`, reducer append)
//!   e la persistenza sara' del runtime/emit, non del nodo.
//!
//! Il nodo NON instrada (l'edge `ask -> END` e' fuori, in `edge.rs`).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::decisions::interlocutore::Interlocutore;
use crate::nodes::decisione_chiarimento::{
    una_volta_per_convocazione, ChiaveDecisione, EsitoDecisione, MotivoNonPresa,
    ProvenienzaDecisione,
};
use crate::runtime::ports::MetaStepStore;
use crate::runtime::AgentNodeCtx;
use crate::state::{AgentState, Message, MetaStep, StateDelta, ToolUse};

/// Lunghezza minima dell'ultimo messaggio utente sotto la quale il nodo salta
/// (`clarify_or_expand_node.py:752`: `len(user_msg) < 3`).
const MIN_USER_MSG_LEN: usize = 3;

/// `kind` del meta_step della DOMANDA posta all'utente.
///
/// E' un vocabolario con DUE lati e per questo vive qui, dove il produttore lo
/// scrive: l'unico consumatore e' `mcp-core::agent_graph_adapter::clarify_history_store`,
/// che conta le righe `kind='clarify'` come «questo turno ha posto una domanda»
/// per il detector cross-run di loop. Finche' nessuno persisteva questi
/// meta_step quel detector leggeva sempre 0: nessuna riga da contare, quindi
/// nessuna ripetizione rilevabile (misurato il 18/08/2026: zero righe
/// `kind='clarify'` su 104+ meta_step in due progetti, il canale non esisteva
/// in DB).
pub const META_KIND_CLARIFY: &str = "clarify";

/// Nome del campo con cui il modello dichiara il proprio default.
///
/// Ha TRE lati che devono nominarlo allo stesso modo — lo schema che lo chiede,
/// il parser che lo legge, il payload dell'assunzione che lo riporta — e una
/// costante e' il solo modo di renderli un contratto invece di tre stringhe che
/// si somigliano.
const CAMPO_SUGGESTED_DEFAULT: &str = "suggested_default";

/// Nome del campo con cui il payload dichiara DA DOVE viene la decisione.
///
/// Vale `null` dove l'ha presa questo run: «non c'e' stata alcuna deviazione»
/// non e' la stessa cosa di «non lo so», e l'assenza del campo significherebbe
/// un produttore che non parla questa versione del contratto (regola Q).
const CAMPO_DECISIONE: &str = "decisione";

/// Descrizione del campo `suggested_default` dichiarata al modello: fuori dalla
/// `json!` per non produrre una riga oltre il limite di stile (stessa ragione di
/// `DESCRIZIONE_TIPI_CRITERIO` nel planner).
const DESCRIZIONE_SUGGESTED_DEFAULT: &str = "Con mode=ask: la risposta che \
    adotteresti se nessuno rispondesse alla domanda. Obbligatoria di fatto: \
    dove non c'e' nessuno a cui chiedere (sub-run di una figura convocata) e' \
    QUESTA che viene applicata come assunzione dichiarata, e il lavoro prosegue.";

/// `kind` del meta_step dell'ASSUNZIONE applicata al posto della domanda, dove
/// non c'e' nessuno a cui chiederla. Distinto da [`META_KIND_CLARIFY`]: sono due
/// esiti diversi e vogliono due varianti (regola Q), e confonderli farebbe
/// contare al detector di loop domande che nessuno ha mai posto.
pub const META_KIND_CLARIFY_ASSUNZIONE: &str = "clarify_assumption";

/// Marcatori di dominio/codice/design cercati nel listing top-level del progetto.
/// Replica `_DOMAIN_MARKERS` (`clarify_or_expand_node.py:52-67`): coppie
/// (marker-lowercase, label). Niente nome modello/provider qui (regola G).
const DOMAIN_MARKERS: &[(&str, &str)] = &[
    ("figma_export", "design importato (figma_export/)"),
    ("src", "codice sorgente (src/)"),
    ("app", "codice applicativo (app/)"),
    ("lib", "codice di libreria (lib/)"),
    ("package.json", "progetto Node/JS (package.json)"),
    ("requirements.txt", "progetto Python (requirements.txt)"),
    ("pyproject.toml", "progetto Python (pyproject.toml)"),
    ("go.mod", "progetto Go (go.mod)"),
    ("cargo.toml", "progetto Rust (Cargo.toml)"),
    ("pom.xml", "progetto Java/Maven (pom.xml)"),
    ("readme", "documentazione di progetto (README)"),
    (".csproj", "progetto .NET (*.csproj)"),
    ("composer.json", "progetto PHP (composer.json)"),
    ("gemfile", "progetto Ruby (Gemfile)"),
];

/// Config DB-driven del nodo clarify, PASSATA (regola G: nessuna lettura DB nel
/// nodo, nessun fallback hardcoded dentro la logica decisionale).
///
/// Mappa i settings letti dal brain via `_load_config()`
/// (`clarify_or_expand_node.py:82-183`, categoria `orchestrator`, prefisso
/// `clarify.`). Sono omessi i campi che governano SOLO i sotto-gate Comp.1 /
/// Cluster 4 (intake/decision-lookup), non ancora portati (vedi doc del modulo):
/// `prompt_key`, `decision_*`, `intake_*`, `confirm_if_implemented`. Il provider/
/// model del purpose `clarify_expand` arriva RISOLTO A MONTE (regola G).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClarifyConfig {
    /// Nodo clarify attivo (`clarify.enabled`, default true). OFF -> no-op.
    pub enabled: bool,
    /// Soglia di confidence sotto cui il nodo si attiva (`clarify.confidence_threshold`,
    /// default 0.6). `confidence >= threshold` (e non force_classify) -> skip.
    pub confidence_threshold: f64,
    /// Tetto ai tentativi di clarify per run (`clarify.max_attempts`, default 1,
    /// mig 0386). `clarify_attempts >= max_attempts` -> fail-open (skip).
    pub max_attempts: i64,
    /// Lunghezza massima della question prima del troncamento
    /// (`clarify.max_question_chars`, default 280).
    pub max_question_chars: i64,
    /// Sotto (`<=`) questo `agentic_score` un intent chat e' small-talk puro e
    /// bypassa il gate (`clarify.smalltalk_agentic_score_max`, default 0.3).
    pub smalltalk_agentic_score_max: f64,
    /// Cluster 4: in automatico, se ON, classifichiamo SEMPRE per intercettare
    /// le decisioni irreversibili (`clarify.confirm_irreversible_in_auto`,
    /// default false). Abilita `force_classify`.
    pub confirm_irreversible_in_auto: bool,
}

impl Default for ClarifyConfig {
    fn default() -> Self {
        // Default IDENTICI ai `defaults` di `_load_config`
        // (`clarify_or_expand_node.py:85-113`). Valgono SOLO se il DB e'
        // irraggiungibile, mai come magic fallback nella logica.
        Self {
            enabled: true,
            confidence_threshold: 0.6,
            max_attempts: 1,
            max_question_chars: 280,
            smalltalk_agentic_score_max: 0.3,
            confirm_irreversible_in_auto: false,
        }
    }
}

/// Esito della catena di gate pre-LLM (`Self::pre_llm_gate`): o il nodo SALTA
/// (no-op, delta vuoto) o procede a chiamare l'LLM. Distingue i due rami senza
/// duplicare i gate nei call site (punto unico, regola L).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GateOutcome {
    /// I gate impongono no-op: il flusso prosegue invariato (delta vuoto).
    Skip,
    /// I gate sono superati: si procede alla decisione LLM (ask/expand/skip).
    /// `force_classify` indica il ramo Cluster 4 (auto + conferma irreversibili).
    CallLlm {
        /// `true` se siamo nel ramo `force_classify` (auto con conferma
        /// irreversibili attiva): cambia il trattamento del mode=ask a valle.
        force_classify: bool,
        /// Confidence corrente (propagata nel meta_step ask).
        confidence: f64,
    },
}

/// Mode deciso dall'LLM clarify/expand (`clarify_or_expand_node.py:788`, enum
/// del tool input). Enum dedicato invece di stringa: il `match` esaustivo a valle
/// non puo' dimenticare un caso.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClarifyMode {
    /// Chiedi un chiarimento all'utente (ferma il turno).
    Ask,
    /// Arricchisci la query per il retrieve.
    Expand,
    /// Nessuna azione (default conservativo + valore sconosciuto).
    Skip,
}

/// Categoria della decisione (`clarify_or_expand_node.py:792-796`, enum del tool
/// input). Governa il gate `force_classify` sul mode=ask in automatico.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionCategory {
    /// Scelta implementativa (default).
    Technical,
    /// Scelta di prodotto/UX/business.
    Product,
    /// Azione difficile da annullare.
    Irreversible,
}

/// Decisione gia' parsata del tool LLM `clarify_or_expand`
/// (`clarify_or_expand_node.py:829-833`). La logica deterministica del nodo
/// agisce su QUESTA struct (l'I/O LLM che la produce e' dietro `ctx.llm`).
#[derive(Debug, Clone, PartialEq)]
pub struct LlmDecision {
    /// Mode scelto (ask/expand/skip).
    pub mode: ClarifyMode,
    /// Domanda all'utente (ramo ask).
    pub question: String,
    /// Query arricchita (ramo expand).
    pub expanded_query: String,
    /// Razionale (incluso nel meta_step ask).
    pub rationale: String,
    /// Categoria della decisione.
    pub category: DecisionCategory,
    /// `true` se la decisione e' facilmente reversibile (default true).
    pub reversible: bool,
    /// Risposta che il modello adotterebbe se nessuno rispondesse alla domanda.
    ///
    /// Campo NUOVO rispetto al Python (che non ne aveva alcuno): senza, un run
    /// privo di interlocutore non ha nulla su cui ripiegare e il livello 1 del
    /// fix degraderebbe a «ignora la domanda». E' la stessa forma che il planner
    /// usa gia' per la propria clarifying pre-flight (`suggested_default` dentro
    /// `{id, question, suggested_default}`), riusata invece di reinventata
    /// (regola L). Stringa vuota = il modello non ne ha proposto uno: l'ignoto
    /// resta dichiarato, non diventa un default inventato da noi (regola Q).
    pub suggested_default: String,
}

impl LlmDecision {
    /// Costruisce la decisione dall'`input` del tool LLM, con i default ESATTI del
    /// Python (`clarify_or_expand_node.py:830-833`):
    ///   - `mode`       default "skip", lower-case, valore ignoto -> Skip.
    ///   - `category`   default "technical", lower-case, ignoto -> Technical.
    ///   - `reversible` default true (`None -> True`), altrimenti `bool(...)`.
    ///   - `question`/`expanded_query`/`rationale`: stringa o "".
    pub fn from_tool_input(input: &Value) -> Self {
        let mode = match input
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("skip")
            .to_lowercase()
            .as_str()
        {
            "ask" => ClarifyMode::Ask,
            "expand" => ClarifyMode::Expand,
            // "skip" o qualunque valore fuori contratto -> Skip (default Python).
            _ => ClarifyMode::Skip,
        };
        let category = match input
            .get("category")
            .and_then(Value::as_str)
            .unwrap_or("technical")
            .to_lowercase()
            .as_str()
        {
            "product" => DecisionCategory::Product,
            "irreversible" => DecisionCategory::Irreversible,
            // "technical" o ignoto -> Technical (default Python).
            _ => DecisionCategory::Technical,
        };
        // `reversible = True if reversible is None else bool(reversible)`:
        // chiave assente/null -> true; altrimenti truthiness del valore JSON.
        let reversible = match input.get("reversible") {
            None | Some(Value::Null) => true,
            Some(v) => Self::json_truthy(v),
        };
        Self {
            mode,
            question: Self::input_str(input, "question"),
            expanded_query: Self::input_str(input, "expanded_query"),
            rationale: Self::input_str(input, "rationale"),
            category,
            reversible,
            suggested_default: Self::input_str(input, CAMPO_SUGGESTED_DEFAULT),
        }
    }

    /// Estrae una stringa dal tool input (`str(inp.get(k) or "")`): valore
    /// stringa cosi' com'e', altrimenti "".
    fn input_str(input: &Value, key: &str) -> String {
        input
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    /// Truthiness di un valore JSON alla `bool(...)` Python: false/null/0/""/
    /// []/{} sono falsy, il resto truthy. Replica `bool(reversible)` quando il
    /// campo e' presente e non null.
    fn json_truthy(v: &Value) -> bool {
        match v {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
        }
    }
}

/// Nodo clarify-or-expand. Stateless: legge lo stato + la config passata e fa
/// I/O tramite le porte del `AgentNodeCtx` (LLM via `ctx.llm`, project_context
/// via `ctx.tools`). La decisione LLM e' un TODO delegato (vedi doc del modulo);
/// la logica di gate e di costruzione dei due rami e' interamente qui.
pub struct ClarifyOrExpandNode {
    /// Config DB-driven (regola G: passata, mai letta dal nodo).
    cfg: ClarifyConfig,
    /// Persistenza dei meta_step (come planner / executor / final_gate).
    ///
    /// Il nodo era l'UNICO produttore di meta_step senza questa porta: il
    /// MetaStep viaggiava nel `StateDelta`, `AgentState.meta_steps` lo
    /// accumulava in memoria e mcp-core non lo leggeva mai
    /// (`grep 'state.meta_steps' crates/mcp-core/` -> zero righe). La domanda
    /// che il modello aveva formulato veniva costruita, troncata, impacchettata
    /// e buttata: nemmeno un umano avrebbe potuto rispondere.
    meta_steps: std::sync::Arc<dyn MetaStepStore>,
}

impl ClarifyOrExpandNode {
    /// Costruisce il nodo con la config DB-driven gia' risolta dal chiamante e
    /// la porta di persistenza dei meta_step.
    pub fn new(cfg: ClarifyConfig, meta_steps: std::sync::Arc<dyn MetaStepStore>) -> Self {
        Self { cfg, meta_steps }
    }

    /// Ultimo messaggio di ruolo human/user (in reverse), content come stringa,
    /// NON trimmato. Replica `_last_user_message`
    /// (`clarify_or_expand_node.py:186-191`): itera dal fondo, filtra per ruolo
    /// `human`/`user` (qui SOLO `Message::Human`), ritorna `m.content` se stringa
    /// (per i blocchi: la forma `str(content)` del Python non e' riproducibile 1:1
    /// e fuori contratto per l'utente, quindi concateniamo i blocchi Text). Il
    /// `.strip()` e' applicato a valle dal chiamante (`:643`), non qui.
    pub fn last_user_message(messages: &[Message]) -> String {
        for m in messages.iter().rev() {
            if let Message::Human { content } = m {
                return content.flatten_text();
            }
        }
        String::new()
    }

    /// `true` se l'`automation_mode` e' uno dei 5 alias "automatico"
    /// (`clarify_or_expand_node.py:417` / `:692`).
    ///
    /// DELEGA a [`AgentState::is_autonomous_run`] (regola L): la stessa domanda
    /// aveva cinque risposte scritte in cinque posti — qui, in
    /// [`crate::decisions::hitl::automation_requires_hitl`], in
    /// `planner::clarifying_requires_hitl`, in `AutomationMode::is_autonomous` e
    /// in `AgentState::is_autonomous_run` — ed e' la ragione strutturale per cui
    /// questo nodo e il planner si comportavano in modo OPPOSTO davanti allo
    /// stesso problema nello stesso grafo (il planner applica i default e
    /// prosegue, questo fermava il run).
    pub fn is_auto(state: &AgentState) -> bool {
        state.is_autonomous_run()
    }

    /// Tronca la question a `max_chars` caratteri rimuovendo l'ultima parola
    /// spezzata e appendendo "..." (`clarify_or_expand_node.py:854-856`):
    /// `question[:max].rsplit(" ", 1)[0] + "..."`. Se non c'e' spazio nel prefisso
    /// il `rsplit` ritorna l'intero prefisso (nessun taglio di parola). Sotto
    /// soglia ritorna la question invariata.
    pub fn truncate_question(question: &str, max_chars: usize) -> String {
        if question.chars().count() <= max_chars {
            return question.to_string();
        }
        let prefix: String = question.chars().take(max_chars).collect();
        // `rsplit(" ", 1)[0]`: tutto cio' che precede l'ultimo spazio; se non c'e'
        // spazio, l'intero prefisso.
        let head = match prefix.rsplit_once(' ') {
            Some((before, _)) => before,
            None => prefix.as_str(),
        };
        format!("{head}...")
    }

    /// Catena di gate PRE-LLM (punto unico, regola L). Replica
    /// `clarify_or_expand_node.py:596-753` nell'ORDINE esatto del Python. Ritorna
    /// `Skip` (no-op) o `CallLlm { force_classify, confidence }`.
    ///
    /// `user_msg` e' l'ultimo messaggio utente GIA' trimmato (come
    /// `user_msg_preview`, `:643`). Tutta la decisione e' pura: stato + config +
    /// user_msg in input, esito in output.
    pub fn pre_llm_gate(cfg: &ClarifyConfig, state: &AgentState, user_msg: &str) -> GateOutcome {
        // Gate 1: flag disabilitato (:597-599).
        if !cfg.enabled {
            return GateOutcome::Skip;
        }
        // Gate 2: intent_hint gia' risolto (disambiguazione mcp-core, :606-611).
        // `state.get("intent_hint")` truthy: stringa non vuota.
        if state
            .intent_hint
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
        {
            return GateOutcome::Skip;
        }
        // Gate 3: intent conversazionale small-talk (:621-641). Per chat/
        // general_chat con agentic_score <= smalltalk_max -> skip; sopra soglia
        // NON e' uno skip (prosegue all'intake gate/KB, qui ai gate successivi).
        let intent = state
            .user_intent
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if intent == "chat" || intent == "general_chat" {
            let agentic_score = state.agentic_score.unwrap_or(0.0);
            if agentic_score <= cfg.smalltalk_agentic_score_max {
                return GateOutcome::Skip;
            }
            // Sopra soglia: richiesta sostanziale, prosegue ai gate sotto.
        }

        // force_classify (:691-693): is_auto && confirm_irreversible_in_auto.
        let is_auto = Self::is_auto(state);
        let force_classify = is_auto && cfg.confirm_irreversible_in_auto;

        // Gate 4: short-circuit autonomia (:694-699). In auto SENZA force_classify
        // l'agente procede autonomo.
        if is_auto && !force_classify {
            return GateOutcome::Skip;
        }

        // Gate 5: tetto tentativi clarify (:704-712, mig 0386, fail-open).
        let clarify_attempts = state.clarify_attempts.unwrap_or(0);
        let max_clarify = if cfg.max_attempts != 0 {
            cfg.max_attempts
        } else {
            1
        };
        if clarify_attempts >= max_clarify {
            return GateOutcome::Skip;
        }

        // Gate 6: soglia confidence (:713-733). confidence default 1.0 (assente).
        // confidence >= threshold && !force_classify -> skip.
        let confidence = state.intent_confidence.unwrap_or(1.0);
        if confidence >= cfg.confidence_threshold && !force_classify {
            return GateOutcome::Skip;
        }

        // Gate 7: user_msg troppo corto (:751-753). `len(user_msg) < 3`.
        if user_msg.chars().count() < MIN_USER_MSG_LEN {
            return GateOutcome::Skip;
        }

        GateOutcome::CallLlm {
            force_classify,
            confidence,
        }
    }

    /// Costruisce il blocco `CONTESTO PROGETTO` dal listing top-level del progetto.
    /// Replica la parte DETERMINISTICA di `_build_project_context`
    /// (`clarify_or_expand_node.py:565-584`): scarta listing in errore, cerca i
    /// `DOMAIN_MARKERS` (case-insensitive, dedup per label preservando l'ordine),
    /// e — se ne trova — wrappa nel blocco testuale. Stringa vuota se nessun
    /// marcatore o listing in errore (comportamento storico). L'I/O (la
    /// `list_files`) e' del chiamante (dietro `ctx.tools`).
    ///
    /// `elenco_fallito` e' la DICHIARAZIONE d'esito di chi ha eseguito la
    /// `list_files` (`ToolOutcome::is_error`), non una deduzione dal testo
    /// (regola Q). Finche' la firma ammetteva il solo listing, questa funzione
    /// DOVEVA ricavarsi l'esito da sola mentre il chiamante aveva in mano
    /// l'esito strutturato e lo buttava via: un fallimento il cui marker non sta
    /// in testa (una premessa anteposta basta a spostarlo) diventava qui un
    /// progetto ESISTENTE, e il blocco vietava al nodo di chiedere all'utente la
    /// natura dell'applicazione sulla base di un messaggio d'errore.
    pub fn build_project_context(listing: &str, elenco_fallito: bool) -> String {
        // Un elenco che il suo esecutore dichiara fallito non descrive un
        // progetto: il criterio e' il campo, mai il vocabolario del messaggio.
        if listing.is_empty() || elenco_fallito {
            return String::new();
        }
        let lower = listing.to_lowercase();
        let mut found: Vec<&str> = Vec::new();
        for (marker, label) in DOMAIN_MARKERS {
            if lower.contains(marker) && !found.contains(label) {
                found.push(label);
            }
        }
        if found.is_empty() {
            return String::new();
        }
        format!(
            "CONTESTO PROGETTO: il workspace contiene gia' {}. Si tratta di un \
             progetto ESISTENTE: dominio, entita' e stack sono DEDUCIBILI \
             esplorando questi file. NON chiedere all'utente la natura \
             dell'applicazione ne' le entita'.",
            found.join(", ")
        )
    }

    /// Applica la decisione LLM (mode=ask) producendo il delta del ramo `ask`, o
    /// `None` se il gate `force_classify` impone di procedere autonomo
    /// (`clarify_or_expand_node.py:841-885`). Deterministico: decisione + stato +
    /// config + force_classify + confidence + interlocutore -> delta.
    ///
    /// ## I due esiti, e perche' sono due (regola Q)
    ///
    /// - [`Interlocutore::Umano`]: `pending_clarify=true` + meta_step
    ///   [`META_KIND_CLARIFY`] + `clarify_attempts+1`. Il turno si ferma, la
    ///   domanda compare in chat, il messaggio successivo apre un run nuovo.
    ///   Comportamento invariato.
    /// - [`Interlocutore::Nessuno`]: MAI `pending_clarify`, qualunque sia la
    ///   categoria. La domanda diventa un'ASSUNZIONE DICHIARATA nel canale che
    ///   il planner usa gia' per lo stesso problema
    ///   (`applied_default_assumptions`, [`crate::nodes::ClarifyingBranch::ApplyDefaults`])
    ///   e il run PROSEGUE all'understanding. E' la regola D: chi non ha nessuno
    ///   a cui chiedere procede dichiarando le proprie assunzioni, non tace.
    ///
    /// Il gate `force_classify` resta e viene PRIMA, perche' risponde a un'altra
    /// domanda («vale la pena disturbare un umano che esiste?»): con una
    /// decisione technical+reversibile in automatico non c'e' ne' domanda ne'
    /// assunzione da dichiarare, e il ramo esce `None` come sempre.
    ///
    /// `None` anche quando la question e' vuota (no-op, `:852-853`): senza
    /// domanda non c'e' nulla da chiedere ne' da assumere.
    ///
    /// `provenienza` dichiara nei campi se la decisione l'ha presa questo run o
    /// se e' quella della convocazione (regola Q): una figura che agisce su una
    /// decisione presa altrove deve dirlo, non lasciarlo dedurre.
    #[allow(clippy::too_many_arguments)]
    pub fn build_ask_delta(
        cfg: &ClarifyConfig,
        state: &AgentState,
        decision: &LlmDecision,
        force_classify: bool,
        confidence: f64,
        interlocutore: Interlocutore,
        provenienza: &ProvenienzaDecisione,
    ) -> Option<StateDelta> {
        // Gate Cluster 4 (:841-848): in auto (force_classify) NON interrompiamo
        // per decisioni tecniche/reversibili; chiediamo conferma SOLO per
        // product/irreversible o non-reversibile.
        if force_classify {
            let is_product_or_irreversible = matches!(
                decision.category,
                DecisionCategory::Product | DecisionCategory::Irreversible
            ) || !decision.reversible;
            if !is_product_or_irreversible {
                return None;
            }
        }

        // question vuota (dopo trim) -> no-op (:851-853).
        let question = decision.question.trim();
        if question.is_empty() {
            return None;
        }
        let max_chars = if cfg.max_question_chars > 0 {
            cfg.max_question_chars as usize
        } else {
            0
        };
        let question = Self::truncate_question(question, max_chars);
        let clarify_attempts = state.clarify_attempts.unwrap_or(0);

        // IL BIVIO, e l'unico posto in cui il criterio ha una conseguenza.
        Some(if interlocutore.puo_porre_una_domanda() {
            Self::delta_domanda(
                state,
                decision,
                &question,
                clarify_attempts,
                confidence,
                provenienza,
            )
        } else {
            Self::delta_assunzione(
                state,
                decision,
                &question,
                clarify_attempts,
                interlocutore,
                provenienza,
            )
        })
    }

    /// Il delta del ramo CON interlocutore: la domanda posta, il turno che si
    /// ferma. Comportamento storico, invariato.
    fn delta_domanda(
        state: &AgentState,
        decision: &LlmDecision,
        question: &str,
        clarify_attempts: i64,
        confidence: f64,
        provenienza: &ProvenienzaDecisione,
    ) -> StateDelta {
        let meta = MetaStep {
            kind: META_KIND_CLARIFY.to_string(),
            title: "Serve un chiarimento".to_string(),
            payload: json!({
                "question": question,
                "rationale": decision.rationale,
                "category": Self::category_label(decision.category),
                "reversible": decision.reversible,
                "intent": state.user_intent,
                "confidence": confidence,
                CAMPO_DECISIONE: provenienza.dichiarazione(),
            }),
            correlation_id: None,
            created_at: None,
        };
        StateDelta {
            pending_clarify: Some(Some(true)),
            clarify_attempts: Some(Some(clarify_attempts + 1)),
            meta_steps: Some(vec![meta]),
            ..Default::default()
        }
    }

    /// Il delta del ramo SENZA interlocutore: l'assunzione dichiarata al posto
    /// della domanda, e il run prosegue.
    ///
    /// L'entry ha la SHAPE del planner (`{id, question, suggested_default}`)
    /// perche' e' lo stesso canale e chi lo legge non deve conoscere due forme;
    /// i campi in piu' (`rationale`, `category`, `source`, `reason`) dicono da
    /// dove viene e perche' e' stata applicata invece di chiesta.
    ///
    /// L'append e' esplicito (`state` + push) e non un reducer: il campo ha
    /// semantica di sovrascrittura, e assegnarlo secco cancellerebbe quanto gia'
    /// presente.
    ///
    /// `suggested_default` VUOTO resta `null`: un default che il modello non ha
    /// proposto non lo inventiamo noi (regola G/Q). L'assunzione vale comunque —
    /// dice «qui c'era un'ambiguita' e ho proceduto lo stesso», che e'
    /// esattamente l'informazione che oggi va perduta.
    ///
    /// PORTATA di cio' che si vede: il canale VISIBILE e' il meta_step, che
    /// `run` emette live e persiste. Il campo di stato ha oggi i soli lettori
    /// che ha gia' per il planner (nessuno fuori dallo stato): vi si scrive
    /// perche' e' lo stesso canale per la stessa cosa (regola L), non perche'
    /// da solo basti a mostrarla.
    fn delta_assunzione(
        state: &AgentState,
        decision: &LlmDecision,
        question: &str,
        clarify_attempts: i64,
        interlocutore: Interlocutore,
        provenienza: &ProvenienzaDecisione,
    ) -> StateDelta {
        let default = decision.suggested_default.trim();
        let payload = json!({
            "id": format!("clarify_{}", clarify_attempts + 1),
            "question": question,
            CAMPO_SUGGESTED_DEFAULT: if default.is_empty() { Value::Null } else { json!(default) },
            "rationale": decision.rationale,
            "category": Self::category_label(decision.category),
            "source": META_KIND_CLARIFY_ASSUNZIONE,
            "reason": interlocutore.motivo_assenza(),
            CAMPO_DECISIONE: provenienza.dichiarazione(),
        });
        let mut assunzioni = state.applied_default_assumptions.clone().unwrap_or_default();
        assunzioni.push(payload.clone());
        let meta = MetaStep {
            // Kind DIVERSO da `clarify` e non e' un dettaglio: l'ESISTENZA di un
            // meta_step `clarify` e' la dichiarazione strutturata "questo turno
            // ha posto una domanda all'utente", ed e' cio' che il detector
            // cross-run di loop conta (`ClarifyHistoryPort`). Un'assunzione
            // applicata non e' una domanda posta: contarla li' significherebbe
            // rilevare un loop di domande che nessuno ha mai fatto.
            kind: META_KIND_CLARIFY_ASSUNZIONE.to_string(),
            title: "Assunzione dichiarata (nessuno a cui chiedere)".to_string(),
            payload,
            correlation_id: None,
            created_at: None,
        };
        StateDelta {
            // NIENTE `pending_clarify`: e' l'intero fix. Qui il campo non e'
            // "false", e' ASSENTE — il ramo non ha nulla da dire su un flag che
            // governa la terminazione del run.
            clarify_attempts: Some(Some(clarify_attempts + 1)),
            applied_default_assumptions: Some(Some(assunzioni)),
            meta_steps: Some(vec![meta]),
            ..Default::default()
        }
    }

    /// Applica la decisione LLM (mode=expand) producendo il delta del ramo
    /// `expand`, o `None` se l'espansione e' vuota o uguale al messaggio
    /// originale (`clarify_or_expand_node.py:887-895`). `user_msg` e' l'ultimo
    /// messaggio utente trimmato.
    pub fn build_expand_delta(decision: &LlmDecision, user_msg: &str) -> Option<StateDelta> {
        let expanded = decision.expanded_query.trim();
        if expanded.is_empty() || expanded == user_msg.trim() {
            return None;
        }
        Some(StateDelta {
            expanded_query: Some(Some(expanded.to_string())),
            ..Default::default()
        })
    }

    /// Etichetta stabile della categoria (per il payload del meta_step), allineata
    /// agli enum del tool input Python.
    fn category_label(category: DecisionCategory) -> &'static str {
        match category {
            DecisionCategory::Technical => "technical",
            DecisionCategory::Product => "product",
            DecisionCategory::Irreversible => "irreversible",
        }
    }

    /// Schema del tool `clarify_or_expand` dichiarato all'LLM
    /// (`clarify_or_expand_node.py:782-801`). Costruito qui per la chiamata via
    /// `ctx.llm`; il provider/model sono risolti a monte (regola G).
    fn tool_schema() -> Value {
        json!({
            "name": "clarify_or_expand",
            "description": "Decidi se serve un chiarimento all'utente o un'espansione della query per il retrieve.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "mode": {"type": "string", "enum": ["ask", "expand", "skip"]},
                    "question": {"type": "string"},
                    "expanded_query": {"type": "string"},
                    "rationale": {"type": "string"},
                    "category": {
                        "type": "string",
                        "enum": ["technical", "product", "irreversible"],
                        "description": "Tipo di decisione: technical (scelta implementativa), product (scelta di prodotto/UX/business), irreversible (azione difficile da annullare)."
                    },
                    "reversible": {"type": "boolean", "description": "True se la decisione e' facilmente reversibile."},
                    CAMPO_SUGGESTED_DEFAULT: {"type": "string", "description": DESCRIZIONE_SUGGESTED_DEFAULT}
                },
                "required": ["mode"]
            }
        })
    }
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for ClarifyOrExpandNode {
    fn id(&self) -> NodeId {
        NodeId::ClarifyOrExpand
    }

    async fn run(&self, state: &AgentState, ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        // L'ultimo messaggio utente, trimmato (= user_msg_preview, :643).
        let user_msg = Self::last_user_message(&state.messages);
        let user_msg = user_msg.trim().to_string();

        // ── Catena di gate pre-LLM (punto unico) ──────────────────────────────
        let (force_classify, confidence) = match Self::pre_llm_gate(&self.cfg, state, &user_msg) {
            GateOutcome::Skip => {
                // No-op: il flusso prosegue invariato (delta vuoto).
                return Ok(StateDelta::default().into_opaque());
            }
            GateOutcome::CallLlm {
                force_classify,
                confidence,
            } => (force_classify, confidence),
        };

        // NB i sotto-gate Comp.1 (intake_gate) / Cluster 4 (decision_lookup) NON
        // sono portati (OFF di default, I/O KB+LLM+DB): vedi doc del modulo. Con i
        // default DB il flusso non li attraversa, quindi nessuna divergenza.

        // ── La decisione: una volta per CONVOCAZIONE, non una per figlio ──────
        // Punto unico dell'identita' in `decisione_chiarimento`: se questo run e'
        // una figura convocata, la decisione appartiene alla convocazione e al
        // TESTO del mandato, e il primo che arriva la prende per tutte. Dentro
        // `prendi_decisione` stanno ENTRAMBI gli I/O (la `list_files` del contesto
        // progetto e la chiamata al modello): condividerne solo uno lascerebbe
        // l'altro moltiplicato per il numero di figure.
        let chiave = ChiaveDecisione::della_convocazione(state, &user_msg);
        let (esito, provenienza) = match chiave.as_ref() {
            Some(k) => {
                una_volta_per_convocazione(k, || Self::prendi_decisione(ctx, &user_msg)).await
            }
            // Run di chat: nessuna convocazione, la domanda se la pone lui solo.
            None => (
                Self::prendi_decisione(ctx, &user_msg).await,
                ProvenienzaDecisione::FuoriConvocazione,
            ),
        };
        let decision = match esito {
            EsitoDecisione::Presa(d) => d,
            EsitoDecisione::NonPresa(motivo) => {
                // Best-effort come il Python (:816-818, :822-828): nessuna
                // decisione -> no-op, con la CAUSA nel campo (regola Q).
                tracing::info!(
                    target: "nexus_agent_graph::clarify_or_expand",
                    motivo = motivo.identificatore(),
                    provenienza = provenienza.identificatore(),
                    "nessuna decisione clarify, no-op"
                );
                return Ok(StateDelta::default().into_opaque());
            }
        };
        // Chi puo' rispondere a una domanda posta da QUESTO run: punto unico
        // (regola L), letto dallo stato. Non e' la modalita' — vedi la doc di
        // `Interlocutore` per il perche' le due domande restano separate.
        let interlocutore = Interlocutore::dello_stato(state);
        tracing::info!(
            target: "nexus_agent_graph::clarify_or_expand",
            mode = ?decision.mode,
            category = ?decision.category,
            reversible = decision.reversible,
            interlocutore = ?interlocutore,
            provenienza = provenienza.identificatore(),
            "decisione clarify"
        );

        // ── Applica la decisione (logica deterministica) ──────────────────────
        let delta = match decision.mode {
            ClarifyMode::Ask => Self::build_ask_delta(
                &self.cfg,
                state,
                &decision,
                force_classify,
                confidence,
                interlocutore,
                &provenienza,
            )
            .unwrap_or_default(),
            ClarifyMode::Expand => {
                Self::build_expand_delta(&decision, &user_msg).unwrap_or_default()
            }
            // mode=skip o sconosciuto -> no-op (:897-898).
            ClarifyMode::Skip => StateDelta::default(),
        };

        // I meta_step costruiti qui sopra sono l'unico canale su cui la domanda
        // (o l'assunzione che la sostituisce) sopravvive al run: si emettono live
        // e si persistono dal PUNTO UNICO di narrazione (regola L), lo stesso di
        // planner/executor/final_gate. Best-effort: non fallisce mai il turno.
        for meta in delta.meta_steps.iter().flatten() {
            super::emit_phase_meta(
                ctx.emit.as_ref(),
                self.meta_steps.as_ref(),
                &meta.kind,
                meta.title.clone(),
                meta.payload.clone(),
            )
            .await;
        }

        Ok(delta.into_opaque())
    }
}

impl ClarifyOrExpandNode {
    /// I DUE I/O della decisione, in un punto solo: il contesto di progetto
    /// (`list_files` sulla radice) e la chiamata al modello che decide.
    ///
    /// Estratta da `run` perche' e' cio' che una convocazione paga UNA volta
    /// (vedi [`super::decisione_chiarimento`]): finche' viveva inline, ogni
    /// figura ne pagava una copia e non c'era niente da condividere. Entrambi gli
    /// I/O stanno DENTRO — il blocco `CONTESTO PROGETTO` fa parte della richiesta
    /// su cui il modello decide, quindi condividerne solo la meta' darebbe alle
    /// figure una decisione presa su un prompt diverso dal loro.
    ///
    /// Best-effort su entrambi i fronti, come il Python: `list_files` fallita ->
    /// nessun contesto (`:561-563`); chiamata fallita o verdetto non emesso ->
    /// [`EsitoDecisione::NonPresa`] con la causa nel campo (`:816-818`,
    /// `:822-828`), mai un `None` che confonde le due.
    async fn prendi_decisione(ctx: &AgentNodeCtx, user_msg: &str) -> EsitoDecisione {
        let project_context = Self::contesto_di_progetto(ctx).await;

        let llm_response = match ctx
            .llm
            .complete(Self::richiesta_di_decisione(user_msg, project_context))
            .await
        {
            Ok(resp) => resp,
            Err(err) => {
                tracing::warn!(
                    target: "nexus_agent_graph::clarify_or_expand",
                    error = %err,
                    "chiamata LLM clarify fallita (best-effort, no-op)"
                );
                return EsitoDecisione::NonPresa(MotivoNonPresa::ChiamataFallita);
            }
        };

        // Estrae il tool_use `clarify_or_expand`; assente -> skip (:822-828).
        let Some(tool_call) = llm_response
            .tool_calls
            .iter()
            .find(|t| t.name == "clarify_or_expand")
        else {
            tracing::info!(
                target: "nexus_agent_graph::clarify_or_expand",
                blocks = llm_response.tool_calls.len(),
                "tool_use 'clarify_or_expand' non emesso, no-op"
            );
            return EsitoDecisione::NonPresa(MotivoNonPresa::VerdettoNonEmesso);
        };

        EsitoDecisione::Presa(LlmDecision::from_tool_input(&tool_call.input))
    }

    /// La richiesta al modello: PURA (nessun I/O), cosi' «cosa chiediamo» resta
    /// leggibile accanto a «chi lo chiede una volta sola».
    ///
    /// TODO (I/O delegato, invariato dal porting e non chiuso qui): il
    /// provider/model del purpose `clarify_expand` + il system prompt
    /// `agent.clarify.base` sono RISOLTI A MONTE (regola G). Finche' non c'e' la
    /// porta che li fornisce, il chiamante li passa via `LlmRequest`
    /// (provider/model gia' decisi). Il `project_context` (se presente) e'
    /// appeso al system prompt dal chiamante: qui lo passiamo come messaggio di
    /// sistema implicito tramite il primo blocco. La forma resta minimale e
    /// provider-agnostica (porte `ports.rs`).
    fn richiesta_di_decisione(
        user_msg: &str,
        project_context: String,
    ) -> crate::runtime::ports::LlmRequest {
        use crate::runtime::ports::{LlmMessage, LlmRequest};
        let mut messages: Vec<LlmMessage> = Vec::new();
        if !project_context.is_empty() {
            messages.push(LlmMessage {
                role: "system".to_string(),
                content: Value::String(project_context),
                ..Default::default()
            });
        }
        messages.push(LlmMessage {
            role: "user".to_string(),
            content: Value::String(user_msg.to_string()),
            ..Default::default()
        });
        LlmRequest {
            // provider/model RISOLTI A MONTE dal chiamante (regola G): qui
            // restano vuoti finche' la porta purpose-resolver non li fornisce.
            // mcp-core li popolera' con `resolve_purpose_model("clarify_expand")`.
            provider: String::new(),
            model: String::new(),
            messages,
            tools: Some(vec![Self::tool_schema()]),
            // Nodo chiamante = clarify/understanding. Il gateway concreto lo
            // IGNORA quando il modello e' gia' risolto (regola L).
            purpose: Some("clarify_expand".into()),
            ..Default::default()
        }
    }

    /// Il blocco `CONTESTO PROGETTO`: UNA `list_files` sulla radice, dietro la
    /// porta. Best-effort — errore -> blocco vuoto (comportamento storico,
    /// `:561-563`) — e chiamata SOLO da [`Self::prendi_decisione`], perche' fa
    /// parte della richiesta su cui il modello decide e quindi si paga con lei.
    async fn contesto_di_progetto(ctx: &AgentNodeCtx) -> String {
        let call = ToolUse {
            id: Uuid::new_v4().to_string(),
            name: "list_files".to_string(),
            input: json!({ "directory": "." }),
            thought_signature: None,
        };
        match ctx.tools.execute(call).await {
            Ok(outcome) => {
                // L'esito viaggia nel campo: il listing e' testo per i
                // marcatori, `is_error` e' cio' su cui si decide.
                let raw = Self::outcome_result_json(&outcome.content);
                Self::build_project_context(&raw, outcome.is_error)
            }
            Err(err) => {
                tracing::debug!(
                    target: "nexus_agent_graph::clarify_or_expand",
                    error = %err,
                    "list_files root fallita (best-effort, nessun project_context)"
                );
                String::new()
            }
        }
    }

    /// Estrae la stringa `result_json` dal contenuto di un `ToolOutcome`.
    /// Allineato a `UnderstandingNode::outcome_result_json`: stringa -> tale e
    /// quale; oggetto con campo `result_json` -> quello; oggetto -> serializzato;
    /// null -> "" (qui il project_context tratta "" come "nessun listing").
    fn outcome_result_json(content: &Value) -> String {
        match content {
            Value::String(s) => s.clone(),
            Value::Object(map) => {
                if let Some(Value::String(rj)) = map.get("result_json") {
                    rj.clone()
                } else {
                    Value::Object(map.clone()).to_string()
                }
            }
            Value::Null => String::new(),
            other => other.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nexus_graph::node::GraphNode;
    use nexus_graph::GraphState as _;
    use sqlx::postgres::PgPoolOptions;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::*;
    use crate::runtime::ports::{
        EventSink, LlmGateway, LlmRequest, LlmResponse, LlmUsage, PortError, SseEvent,
        ToolCall, ToolExecutor, ToolOutcome,
    };
    use crate::runtime::AgentNodeCtx;
    use crate::state::{AgentState, AutomationMode, Message, MessageContent};

    /// Applica il delta opaco prodotto dal nodo a uno stato e lo ritorna.
    fn apply(base: AgentState, delta: nexus_graph::StateDelta) -> AgentState {
        let mut s = base;
        s.merge(delta);
        s
    }

    fn human(text: &str) -> Message {
        Message::Human {
            content: MessageContent::text(text),
        }
    }

    /// Gateway LLM scriptato: ritorna una decisione `clarify_or_expand` fissa
    /// (tool_call) o solo testo. Registra le richieste.
    struct ScriptedLlm {
        tool_input: Option<Value>,
        seen: std::sync::Mutex<Vec<LlmRequest>>,
    }

    impl ScriptedLlm {
        /// Emette un tool_call `clarify_or_expand` con l'input dato.
        fn with_decision(input: Value) -> Self {
            Self {
                tool_input: Some(input),
                seen: std::sync::Mutex::new(vec![]),
            }
        }
        /// Non emette tool_call (solo testo) -> il nodo deve fare no-op.
        fn no_tool() -> Self {
            Self {
                tool_input: None,
                seen: std::sync::Mutex::new(vec![]),
            }
        }
        /// Quante chiamate al modello sono nate: e' LA misura del difetto
        /// «una decisione per figlio invece che una per convocazione».
        fn chiamate(&self) -> usize {
            self.seen.lock().expect("lock").len()
        }
    }

    #[async_trait]
    impl LlmGateway for ScriptedLlm {
        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, PortError> {
            self.seen.lock().unwrap().push(req);
            let tool_calls = match &self.tool_input {
                Some(input) => vec![ToolUse {
                    id: "tc-1".to_string(),
                    name: "clarify_or_expand".to_string(),
                    input: input.clone(),
                    thought_signature: None,
                }],
                None => vec![],
            };
            Ok(LlmResponse {
                content: String::new(),
                tool_calls,
                usage: LlmUsage::default(),
                ..Default::default()
            })
        }
    }

    /// Gateway che fallisce sempre (path no-op su errore LLM).
    struct FailingLlm;
    #[async_trait]
    impl LlmGateway for FailingLlm {
        async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, PortError> {
            Err(PortError::Llm("simulato".to_string().into()))
        }
    }

    /// Tool executor che CONTA le esecuzioni: la `list_files` del contesto
    /// progetto e' la seconda meta' di cio' che una convocazione pagava per
    /// figura, e senza contarla il risparmio dichiarato sarebbe meta' misurato e
    /// meta' assunto.
    #[derive(Default)]
    struct ContaTools {
        chiamate: std::sync::atomic::AtomicUsize,
    }

    impl ContaTools {
        fn chiamate(&self) -> usize {
            self.chiamate.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ToolExecutor for ContaTools {
        async fn execute(&self, _call: ToolCall) -> Result<ToolOutcome, PortError> {
            self.chiamate
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Un listing reale del progetto misurato: contiene marcatori, quindi
            // il blocco CONTESTO PROGETTO viene davvero composto.
            Ok(ToolOutcome {
                content: Value::String("src\npackage.json\nREADME.md".to_string()),
                is_error: false,
                ..Default::default()
            })
        }
    }

    /// Tool executor che fallisce sempre (project_context vuoto, best-effort).
    struct FailingTools;
    #[async_trait]
    impl ToolExecutor for FailingTools {
        async fn execute(
            &self,
            _call: ToolCall,
        ) -> Result<ToolOutcome, PortError> {
            Err(PortError::Tool("simulato".to_string().into()))
        }
    }

    struct Sink;
    impl EventSink for Sink {
        fn emit(&self, _ev: SseEvent) {}
    }

    /// Ctx di test con LLM e tool iniettabili; PgPool lazy (nessuna query DB).
    fn ctx_with(
        llm: Arc<dyn LlmGateway>,
        tools: Arc<dyn ToolExecutor>,
    ) -> AgentNodeCtx {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy non si connette");
        AgentNodeCtx {
            isolation_available: false,
            db: pool,
            llm,
            tools,
            emit: Arc::new(Sink),
            cfg: crate::routing::config::RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            advisory_gate: None,
        step_gate: None,
        }
    }

    /// Stato "trigger" tipico: confidence bassa, intent code, msg lungo.
    fn trigger_state() -> AgentState {
        AgentState {
            messages: vec![human("come implemento la cache delle sessioni")],
            user_intent: Some("code_write".to_string()),
            intent_confidence: Some(0.4),
            ..Default::default()
        }
    }

    /// Lo stato di un SUB-RUN: gli stessi campi che il dispatcher scrive
    /// all'origine (`parent_run_id: Some(anchor)`, `subagent_depth >= 1`,
    /// `automation_mode='automatic'`), piu' il mandato al posto del messaggio
    /// utente. E' il caso che nessun test attraversava (regola O): finche' il
    /// nodo veniva esercitato solo con lo stato di un run di chat, la morte di
    /// `ui_ux_designer` non era rappresentabile in nessuna prova.
    ///
    /// La CONVOCAZIONE e' nuova a ogni chiamata, e non e' un dettaglio di stile:
    /// da quando la decisione di chiarimento e' memoizzata per (convocazione,
    /// testo) — [`super::decisione_chiarimento`] — due prove che condividessero
    /// il `dispatcher_run_id` del run misurato condividerebbero anche la
    /// decisione, e la seconda leggerebbe il verdetto scriptato per la prima
    /// (regola F).
    fn stato_sub_run() -> AgentState {
        stato_sub_run_di(&Uuid::new_v4().to_string())
    }

    /// Come sopra, con la convocazione DICHIARATA: la usano le prove che
    /// mettono piu' figure sotto lo stesso dispatcher.
    ///
    /// `parent_run_id` resta valorizzato perche' e' cio' che il dispatcher
    /// scrive davvero (l'ANCORA della famiglia), ma porta la SESSIONE — che nel
    /// caso ordinario e' condivisa fra tutte le convocazioni di una
    /// conversazione. Tenerlo distinto dal dispatcher e' cio' che rende le prove
    /// capaci di vedere la differenza fra i due campi.
    fn stato_sub_run_di(convocazione: &str) -> AgentState {
        AgentState {
            messages: vec![human(
                "Sei ui_ux_designer. Analizza il compito e dai un parere \
                 sull'interfaccia dell'app libri.",
            )],
            user_intent: Some("code_write".to_string()),
            intent_confidence: Some(0.4),
            automation_mode: Some(AutomationMode::Automatic),
            dispatcher_run_id: Some(convocazione.to_string()),
            parent_run_id: Some("sessione-della-conversazione".to_string()),
            subagent_depth: Some(1),
            ..Default::default()
        }
    }

    /// Nodo di test con uno store di meta_step usa-e-getta (quando la prova non
    /// guarda cosa e' stato persistito).
    fn nodo_di_test() -> ClarifyOrExpandNode {
        nodo_con_store(ClarifyConfig::default()).0
    }

    /// Nodo con una config dichiarata, store usa-e-getta.
    fn nodo_di_test_con(cfg: ClarifyConfig) -> ClarifyOrExpandNode {
        nodo_con_store(cfg).0
    }

    /// Nodo + il suo store, per le prove che guardano CHE COSA e' stato
    /// persistito: quel canale e' l'unico su cui la domanda sopravvive al run.
    fn nodo_con_store(
        cfg: ClarifyConfig,
    ) -> (
        ClarifyOrExpandNode,
        Arc<crate::runtime::test_doubles::StubMetaStepStore>,
    ) {
        let store = Arc::new(crate::runtime::test_doubles::StubMetaStepStore::default());
        (ClarifyOrExpandNode::new(cfg, store.clone()), store)
    }

    /// La config REALE di produzione (misurata sul DB meta il 18/08/2026:
    /// `clarify.confirm_irreversible_in_auto=true`), non il default del codice —
    /// che vale `false` e da solo non riproduce nulla.
    fn cfg_produzione() -> ClarifyConfig {
        ClarifyConfig {
            confirm_irreversible_in_auto: true,
            ..Default::default()
        }
    }

    /// La decisione che ha ucciso `ui_ux_designer`, nella forma in cui e' uscita
    /// dal modello: `mode=ask`, `category=product`, `reversible=true`.
    fn decisione_product() -> Value {
        json!({
            "mode": "ask",
            "question": "Quale palette e quale famiglia tipografica devo adottare?",
            "rationale": "scelta di prodotto non specificata dal mandato",
            "category": "product",
            "reversible": true,
            "suggested_default": "palette neutra su fondo chiaro, font di sistema sans-serif"
        })
    }

    // ── Gate pre-LLM (unitari, deterministici) ────────────────────────────────

    #[test]
    fn gate_flag_off_skip() {
        let cfg = ClarifyConfig {
            enabled: false,
            ..Default::default()
        };
        let st = trigger_state();
        assert_eq!(
            ClarifyOrExpandNode::pre_llm_gate(&cfg, &st, "qualcosa di lungo"),
            GateOutcome::Skip
        );
    }

    #[test]
    fn gate_intent_hint_skip() {
        let cfg = ClarifyConfig::default();
        let mut st = trigger_state();
        st.intent_hint = Some("code_write".to_string());
        assert_eq!(
            ClarifyOrExpandNode::pre_llm_gate(&cfg, &st, "qualcosa di lungo"),
            GateOutcome::Skip
        );
    }

    #[test]
    fn gate_smalltalk_skip_e_sostanziale_prosegue() {
        let cfg = ClarifyConfig::default();
        // chat con score basso -> small-talk -> skip.
        let mut st = trigger_state();
        st.user_intent = Some("chat".to_string());
        st.agentic_score = Some(0.2);
        assert_eq!(
            ClarifyOrExpandNode::pre_llm_gate(&cfg, &st, "ciao come stai oggi"),
            GateOutcome::Skip
        );
        // chat con score alto -> sostanziale -> prosegue ai gate (confidence bassa).
        st.agentic_score = Some(0.8);
        assert!(matches!(
            ClarifyOrExpandNode::pre_llm_gate(&cfg, &st, "ciao come stai oggi"),
            GateOutcome::CallLlm { .. }
        ));
    }

    #[test]
    fn gate_auto_short_circuit_skip() {
        let cfg = ClarifyConfig::default();
        let mut st = trigger_state();
        st.automation_mode = Some(AutomationMode::Automatic);
        assert_eq!(
            ClarifyOrExpandNode::pre_llm_gate(&cfg, &st, "qualcosa di lungo"),
            GateOutcome::Skip
        );
    }

    #[test]
    fn gate_auto_con_confirm_irreversible_classifica() {
        let cfg = ClarifyConfig {
            confirm_irreversible_in_auto: true,
            ..Default::default()
        };
        let mut st = trigger_state();
        st.automation_mode = Some(AutomationMode::Automatic);
        // confidence alta MA force_classify attivo -> classifica comunque.
        st.intent_confidence = Some(0.95);
        match ClarifyOrExpandNode::pre_llm_gate(&cfg, &st, "qualcosa di lungo") {
            GateOutcome::CallLlm { force_classify, .. } => assert!(force_classify),
            other => panic!("atteso CallLlm force, ottenuto {other:?}"),
        }
    }

    #[test]
    fn gate_tetto_tentativi_skip() {
        let cfg = ClarifyConfig::default(); // max_attempts = 1
        let mut st = trigger_state();
        st.clarify_attempts = Some(1);
        assert_eq!(
            ClarifyOrExpandNode::pre_llm_gate(&cfg, &st, "qualcosa di lungo"),
            GateOutcome::Skip
        );
    }

    #[test]
    fn gate_confidence_alta_skip() {
        let cfg = ClarifyConfig::default(); // threshold 0.6
        let mut st = trigger_state();
        st.intent_confidence = Some(0.9);
        assert_eq!(
            ClarifyOrExpandNode::pre_llm_gate(&cfg, &st, "qualcosa di lungo"),
            GateOutcome::Skip
        );
    }

    #[test]
    fn gate_msg_corto_skip() {
        let cfg = ClarifyConfig::default();
        let st = trigger_state();
        // 2 char < 3 -> skip.
        assert_eq!(
            ClarifyOrExpandNode::pre_llm_gate(&cfg, &st, "ab"),
            GateOutcome::Skip
        );
    }

    #[test]
    fn gate_trigger_callllm() {
        let cfg = ClarifyConfig::default();
        let st = trigger_state(); // confidence 0.4 < 0.6, intent code, no auto
        match ClarifyOrExpandNode::pre_llm_gate(&cfg, &st, "qualcosa di lungo") {
            GateOutcome::CallLlm {
                force_classify,
                confidence,
            } => {
                assert!(!force_classify);
                assert_eq!(confidence, 0.4);
            }
            other => panic!("atteso CallLlm, ottenuto {other:?}"),
        }
    }

    // ── Funzioni deterministiche unitarie ──────────────────────────────────────

    #[test]
    fn truncate_question_rimuove_parola_spezzata() {
        let q = "questa e una domanda molto lunga che supera la soglia massima";
        let out = ClarifyOrExpandNode::truncate_question(q, 20);
        assert!(out.ends_with("..."), "deve appendere ...: {out}");
        // 20 char -> "questa e una domanda", rsplit toglie l'ultima parola
        // ("domanda") perche' il char 20 e' dentro/fine parola.
        assert!(!out.contains("supera"), "tronca prima di supera");
    }

    #[test]
    fn truncate_question_sotto_soglia_invariata() {
        let q = "domanda breve";
        assert_eq!(ClarifyOrExpandNode::truncate_question(q, 280), q);
    }

    #[test]
    fn last_user_message_filtra_ruolo_e_reverse() {
        let msgs = vec![
            human("primo utente"),
            Message::Ai {
                content: MessageContent::text("risposta ai"),
                tool_calls: vec![],
                reasoning: None,
                thinking_signature: None,
            },
            human("ultimo utente"),
            Message::Tool {
                tool_call_id: "t".to_string(),
                content: MessageContent::text("tool out"),
            },
        ];
        assert_eq!(
            ClarifyOrExpandNode::last_user_message(&msgs),
            "ultimo utente"
        );
    }

    #[test]
    fn build_project_context_marcatori() {
        let listing = "package.json\nsrc/\nnode_modules/\nREADME.md";
        let ctx = ClarifyOrExpandNode::build_project_context(listing, false);
        assert!(ctx.contains("CONTESTO PROGETTO"));
        assert!(ctx.contains("package.json"));
        assert!(ctx.contains("src/"));
        // nessun marcatore -> vuoto.
        assert_eq!(
            ClarifyOrExpandNode::build_project_context("foo.txt\nbar.csv", false),
            ""
        );
    }

    /// L'esito dell'elenco e' quello DICHIARATO da chi l'ha eseguito, non quello
    /// indovinato dal testo. Il fixture e' un fallimento REALE il cui marker non
    /// sta in testa: e' la forma che `prepend_preserving_failure` documenta
    /// (una premessa anteposta sposta il marker) e che il criterio testuale
    /// leggeva come successo. Il testo contiene ANCHE parole di dominio, cosi'
    /// il vuoto atteso puo' venire SOLO dalla guardia, mai dall'assenza di
    /// marcatori.
    ///
    /// PROVA DI MUTAZIONE: rimettendo il criterio testuale
    /// (`nexus_types::tool_outcome::is_tool_failure(listing)`) al posto del
    /// campo, questo caso ritorna il blocco "CONTESTO PROGETTO" costruito su un
    /// messaggio d'errore e l'assert rosseggia.
    #[test]
    fn un_elenco_fallito_non_descrive_un_progetto() {
        let elenco_fallito =
            "Elenco parziale della radice.\n\u{274C} [Errore: 'src/' non leggibile, vedi README.md]";
        assert_eq!(
            ClarifyOrExpandNode::build_project_context(elenco_fallito, true),
            "",
            "l'esito e' nel campo: il posto del marker nel testo non c'entra"
        );
        // Contro-prova: lo STESSO testo dichiarato riuscito e' un elenco come un
        // altro — il campo decide in entrambe le direzioni.
        assert!(
            ClarifyOrExpandNode::build_project_context(elenco_fallito, false)
                .contains("CONTESTO PROGETTO")
        );
    }

    /// Il nodo passa a `build_project_context` l'esito che la porta DICHIARA:
    /// un `list_files` fallito non produce contesto progetto anche quando il suo
    /// testo elenca marcatori. Attraversa il produttore vero (il nodo + la porta
    /// `ToolExecutor`), non la sola funzione pura (regola O).
    #[tokio::test]
    async fn il_nodo_scarta_l_elenco_che_la_porta_dichiara_fallito() {
        /// Porta che RIESCE a rispondere ma dichiara il fallimento del tool.
        struct ElencoFallito;
        #[async_trait]
        impl ToolExecutor for ElencoFallito {
            async fn execute(&self, call: ToolCall) -> Result<ToolOutcome, PortError> {
                Ok(ToolOutcome {
                    tool_call_id: call.id,
                    content: Value::String(
                        "Elenco parziale.\npackage.json src/ README.md non leggibili".to_string(),
                    ),
                    is_error: true,
                    ..Default::default()
                })
            }
        }

        let node = nodo_di_test();
        let llm = Arc::new(ScriptedLlm::with_decision(
            json!({"mode": "skip"}),
        ));
        let ctx = ctx_with(llm.clone(), Arc::new(ElencoFallito));
        let st = trigger_state();
        node.run(&st, &ctx).await.expect("run ok");

        let visto = llm.seen.lock().expect("lock");
        let req = visto.first().expect("una richiesta LLM");
        assert!(
            !req.messages.iter().any(|m| m.role == "system"),
            "nessun blocco CONTESTO PROGETTO da un elenco fallito: {:?}",
            req.messages
        );
    }

    #[test]
    fn llm_decision_default_python() {
        // mode assente -> skip; category assente -> technical; reversible assente -> true.
        let d = LlmDecision::from_tool_input(&json!({}));
        assert_eq!(d.mode, ClarifyMode::Skip);
        assert_eq!(d.category, DecisionCategory::Technical);
        assert!(d.reversible);
        // reversible esplicito false.
        let d2 = LlmDecision::from_tool_input(&json!({"mode": "ASK", "reversible": false}));
        assert_eq!(d2.mode, ClarifyMode::Ask);
        assert!(!d2.reversible);
        // category product, mode expand.
        let d3 = LlmDecision::from_tool_input(&json!({"mode": "expand", "category": "PRODUCT"}));
        assert_eq!(d3.mode, ClarifyMode::Expand);
        assert_eq!(d3.category, DecisionCategory::Product);
    }

    // ── build_ask_delta / build_expand_delta (deterministici) ──────────────────

    #[test]
    fn build_ask_delta_force_classify_reversibile_none() {
        let cfg = ClarifyConfig::default();
        let st = trigger_state();
        let decision = LlmDecision::from_tool_input(&json!({
            "mode": "ask", "question": "quale db?", "category": "technical", "reversible": true
        }));
        // force_classify + technical + reversibile -> None (procede autonomo).
        assert!(ClarifyOrExpandNode::build_ask_delta(
            &cfg,
            &st,
            &decision,
            true,
            0.4,
            Interlocutore::Umano,
            &ProvenienzaDecisione::Presa,
        )
        .is_none());
    }

    /// Con un interlocutore la domanda si pone: comportamento INVARIATO.
    #[test]
    fn build_ask_delta_force_classify_product_emette() {
        let cfg = ClarifyConfig::default();
        let st = trigger_state();
        let decision = LlmDecision::from_tool_input(&json!({
            "mode": "ask", "question": "scelta di prodotto?", "category": "product"
        }));
        let delta = ClarifyOrExpandNode::build_ask_delta(
            &cfg,
            &st,
            &decision,
            true,
            0.4,
            Interlocutore::Umano,
            &ProvenienzaDecisione::Presa,
        )
        .expect("delta");
        assert_eq!(delta.pending_clarify, Some(Some(true)));
        let meta = delta.meta_steps.expect("meta");
        assert_eq!(meta[0].kind, META_KIND_CLARIFY);
    }

    /// SENZA interlocutore la STESSA decisione non ferma il run: diventa
    /// un'assunzione dichiarata, e il flusso prosegue all'understanding.
    ///
    /// MUTAZIONE: togliere il ramo `!interlocutore.puo_porre_una_domanda()` da
    /// `build_ask_delta` fa rosseggiare la prima asserzione con
    /// `Some(Some(true))` — cioe' il valore esatto che, passando per l'edge del
    /// grafo, ha chiuso `ui_ux_designer` a zero iterazioni.
    #[test]
    fn build_ask_delta_senza_interlocutore_dichiara_l_assunzione() {
        let cfg = cfg_produzione();
        let st = stato_sub_run();
        let decision = LlmDecision::from_tool_input(&decisione_product());
        let delta = ClarifyOrExpandNode::build_ask_delta(
            &cfg,
            &st,
            &decision,
            true,
            0.4,
            Interlocutore::Nessuno,
            &ProvenienzaDecisione::Presa,
        )
        .expect("delta");

        assert_eq!(
            delta.pending_clarify, None,
            "nessun pending_clarify: il campo non e' 'false', e' ASSENTE"
        );
        let assunzioni = delta
            .applied_default_assumptions
            .expect("il campo e' valorizzato")
            .expect("le assunzioni ci sono");
        assert_eq!(assunzioni.len(), 1);
        assert_eq!(
            assunzioni[0]["suggested_default"],
            json!("palette neutra su fondo chiaro, font di sistema sans-serif")
        );
        assert_eq!(
            assunzioni[0]["reason"],
            json!(crate::decisions::interlocutore::MOTIVO_NESSUN_INTERLOCUTORE)
        );
        let meta = delta.meta_steps.expect("meta");
        assert_eq!(
            meta[0].kind, META_KIND_CLARIFY_ASSUNZIONE,
            "kind DIVERSO da 'clarify': il detector di loop conta le domande poste"
        );
    }

    /// Il default assente resta `null`: non lo inventiamo noi (regola G/Q), ma
    /// l'ambiguita' resta dichiarata e il run prosegue lo stesso.
    #[test]
    fn senza_default_proposto_resta_l_ambiguita_dichiarata() {
        let cfg = cfg_produzione();
        let st = stato_sub_run();
        let decision = LlmDecision::from_tool_input(&json!({
            "mode": "ask", "question": "quale palette?", "category": "product"
        }));
        let delta = ClarifyOrExpandNode::build_ask_delta(
            &cfg,
            &st,
            &decision,
            true,
            0.4,
            Interlocutore::Nessuno,
            &ProvenienzaDecisione::Presa,
        )
        .expect("delta");
        assert_eq!(delta.pending_clarify, None);
        let assunzioni = delta
            .applied_default_assumptions
            .expect("campo")
            .expect("valore");
        assert_eq!(assunzioni[0]["suggested_default"], Value::Null);
        assert_eq!(assunzioni[0]["question"], json!("quale palette?"));
    }

    /// Le assunzioni si APPENDONO: il campo ha reducer di sovrascrittura, e
    /// assegnarlo secco cancellerebbe quanto un altro nodo ha gia' dichiarato.
    #[test]
    fn le_assunzioni_non_cancellano_le_precedenti() {
        let cfg = cfg_produzione();
        let mut st = stato_sub_run();
        st.applied_default_assumptions = Some(vec![json!({"id": "planner_1"})]);
        let decision = LlmDecision::from_tool_input(&decisione_product());
        let delta = ClarifyOrExpandNode::build_ask_delta(
            &cfg,
            &st,
            &decision,
            true,
            0.4,
            Interlocutore::Nessuno,
            &ProvenienzaDecisione::Presa,
        )
        .expect("delta");
        let assunzioni = delta
            .applied_default_assumptions
            .expect("campo")
            .expect("valore");
        assert_eq!(assunzioni.len(), 2);
        assert_eq!(assunzioni[0]["id"], json!("planner_1"));
    }

    #[test]
    fn build_ask_delta_question_vuota_none() {
        let cfg = ClarifyConfig::default();
        let st = trigger_state();
        let decision = LlmDecision::from_tool_input(&json!({"mode": "ask", "question": "  "}));
        assert!(ClarifyOrExpandNode::build_ask_delta(
            &cfg,
            &st,
            &decision,
            false,
            0.4,
            Interlocutore::Umano,
            &ProvenienzaDecisione::Presa,
        )
        .is_none());
    }

    #[test]
    fn build_expand_delta_valido_e_uguale() {
        let decision = LlmDecision::from_tool_input(&json!({
            "mode": "expand", "expanded_query": "cache sessioni redis ttl utenti"
        }));
        let delta = ClarifyOrExpandNode::build_expand_delta(&decision, "originale").expect("delta");
        assert_eq!(
            delta.expanded_query,
            Some(Some("cache sessioni redis ttl utenti".to_string()))
        );
        // expanded == user_msg -> None.
        let same = LlmDecision::from_tool_input(&json!({
            "mode": "expand", "expanded_query": "originale"
        }));
        assert!(ClarifyOrExpandNode::build_expand_delta(&same, "originale").is_none());
    }

    // ── Nodo end-to-end con i mock dei trait ────────────────────────────────────

    /// ask -> pending_clarify + meta_step + clarify_attempts incrementato, E il
    /// meta_step PERSISTITO: quel canale e' l'unico su cui la domanda sopravvive
    /// al run (misurato: zero righe `kind='clarify'` in DB su due progetti,
    /// quindi nemmeno un umano avrebbe potuto rispondere).
    #[tokio::test]
    async fn nodo_ask_emette_pending_clarify_e_meta_step() {
        let (node, store) = nodo_con_store(ClarifyConfig::default());
        let llm = Arc::new(ScriptedLlm::with_decision(json!({
            "mode": "ask",
            "question": "Vuoi una cache in-memory o Redis?",
            "rationale": "scelta architetturale ambigua",
            "category": "technical",
            "reversible": true
        })));
        let tools = Arc::new(FailingTools); // project_context vuoto, irrilevante
        let ctx = ctx_with(llm, tools);
        let st = trigger_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));

        assert_eq!(out.pending_clarify, Some(true));
        assert_eq!(
            out.clarify_attempts,
            Some(1),
            "clarify_attempts incrementato"
        );
        assert_eq!(out.meta_steps.len(), 1);
        assert_eq!(out.meta_steps[0].kind, META_KIND_CLARIFY);
        assert_eq!(out.meta_steps[0].title, "Serve un chiarimento");

        let persistiti = store.meta_steps.lock().expect("lock");
        assert_eq!(
            persistiti.len(),
            1,
            "la domanda deve arrivare in DB, o nessuno potra' rispondere"
        );
        assert_eq!(persistiti[0]["kind"], json!(META_KIND_CLARIFY));
        assert_eq!(
            persistiti[0]["payload"]["question"],
            json!("Vuoi una cache in-memory o Redis?")
        );
    }

    /// IL CASO MISURATO (18/08/2026, `ui_ux_designer` su `app-libri-18-08`).
    ///
    /// Stessa config di produzione, stesso stato di un sub-run, stessa decisione
    /// che il modello ha davvero emesso — e la CONSEGUENZA che si guarda e'
    /// quella che l'utente ha visto due volte: una figura che chiude senza dire
    /// niente.
    ///
    /// MUTAZIONE: rimettere `pending_clarify: Some(Some(true))` nel ramo senza
    /// interlocutore (o togliere il ramo) fa rosseggiare la prima asserzione, e
    /// con essa il resto: `is_pending_clarify()` torna `true`, l'edge del grafo
    /// instrada a `End`, e il sub-run chiude «completed» con 0 iterazioni.
    #[tokio::test]
    async fn una_figura_senza_interlocutore_non_muore_muta() {
        let (node, store) = nodo_con_store(cfg_produzione());
        let llm = Arc::new(ScriptedLlm::with_decision(decisione_product()));
        let ctx = ctx_with(llm.clone(), Arc::new(FailingTools));
        let st = stato_sub_run();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));

        assert!(
            !out.is_pending_clarify(),
            "e' la condizione che l'edge instrada a End: il run non deve morire qui"
        );
        let assunzioni = out
            .applied_default_assumptions
            .expect("l'assunzione e' dichiarata, non buttata");
        assert_eq!(assunzioni.len(), 1);
        assert_eq!(
            assunzioni[0]["question"],
            json!("Quale palette e quale famiglia tipografica devo adottare?")
        );

        let persistiti = store.meta_steps.lock().expect("lock");
        assert_eq!(persistiti.len(), 1);
        assert_eq!(
            persistiti[0]["kind"],
            json!(META_KIND_CLARIFY_ASSUNZIONE),
            "un'assunzione applicata non e' una domanda posta"
        );
    }

    /// LA MISURA CHE CONTA (18/08/2026, run `abdbc7c4` su `app-libri-18-08`).
    ///
    /// Otto figure convocate, UN solo `md5(task_description)` (verificato con
    /// query sul DB del progetto), otto chiamate al modello in tre secondi piu'
    /// otto `list_files`. Qui il numero che si guarda non e' un delta dello
    /// stato: e' QUANTE CHIAMATE nascono da una convocazione di otto figure.
    ///
    /// Le otto girano CONCORRENTI perche' e' cosi' che `spawn_fanout` le lancia:
    /// un «guarda se c'e' gia'» senza attesa qui non risparmierebbe nulla —
    /// nessuna ha ancora risposto quando le altre guardano — e il test lo
    /// dimostrerebbe restando rosso.
    ///
    /// MUTAZIONE: in `run`, sostituire il ramo `Some(k)` con
    /// `(Self::prendi_decisione(ctx, &user_msg).await, ProvenienzaDecisione::Presa)`
    /// — cioe' il difetto reale, la decisione presa da ogni figlio — fa
    /// rosseggiare le prime due asserzioni con 8 invece di 1.
    #[tokio::test]
    async fn una_convocazione_di_otto_figure_paga_una_sola_decisione() {
        let node = Arc::new(nodo_di_test_con(cfg_produzione()));
        let llm = Arc::new(ScriptedLlm::with_decision(decisione_product()));
        let tools = Arc::new(ContaTools::default());
        let ctx = Arc::new(ctx_with(llm.clone(), tools.clone()));
        // Una convocazione sola, otto figure, mandato BYTE-IDENTICO.
        let convocazione = Uuid::new_v4().to_string();

        let mut figure = Vec::new();
        for _ in 0..8 {
            let node = node.clone();
            let ctx = ctx.clone();
            let st = stato_sub_run_di(&convocazione);
            figure.push(async move {
                let delta = node.run(&st, &ctx).await.expect("run ok");
                apply(st, delta)
            });
        }
        let esiti = futures::future::join_all(figure).await;

        assert_eq!(
            llm.chiamate(),
            1,
            "otto figure sullo stesso mandato: UNA decisione, non otto"
        );
        assert_eq!(
            tools.chiamate(),
            1,
            "anche il contesto di progetto e' un fatto della convocazione"
        );

        // E nessuna delle otto perde il proprio prodotto: l'assunzione c'e' per
        // tutte, con lo stesso contenuto (avevano lo stesso identico mandato).
        for out in &esiti {
            let assunzioni = out
                .applied_default_assumptions
                .as_ref()
                .expect("ogni figura dichiara la propria assunzione");
            assert_eq!(assunzioni.len(), 1);
            assert_eq!(
                assunzioni[0]["question"],
                json!("Quale palette e quale famiglia tipografica devo adottare?")
            );
        }
        // Sette su otto la dichiarano EREDITATA (regola Q): chi agisce su una
        // decisione presa altrove lo scrive, non lo lascia dedurre.
        let ereditate = esiti
            .iter()
            .filter(|o| {
                o.applied_default_assumptions.as_ref().expect("assunzioni")[0][CAMPO_DECISIONE]
                    ["provenienza"]
                    == json!("inherited")
            })
            .count();
        assert_eq!(ereditate, 7, "una la prende, sette la ereditano");
    }

    /// DUE mandati diversi nella stessa convocazione sono DUE domande: e' il
    /// caso misurato delle due `provider_analyst`, che sotto lo stesso padre
    /// avevano `context_blob` distinti (md5 `00ddb047` e `21949e6b`).
    ///
    /// E' la ragione per cui l'identita' della decisione e' il TESTO e non la
    /// convocazione: una decisione sola per padre darebbe a una figura la
    /// risposta data sul contesto dell'altra.
    #[tokio::test]
    async fn due_mandati_diversi_restano_due_decisioni() {
        let node = Arc::new(nodo_di_test_con(cfg_produzione()));
        let llm = Arc::new(ScriptedLlm::with_decision(decisione_product()));
        let tools = Arc::new(ContaTools::default());
        let ctx = Arc::new(ctx_with(llm.clone(), tools.clone()));
        let convocazione = Uuid::new_v4().to_string();

        let mut figure = Vec::new();
        for contesto in ["openai", "mistral"] {
            let node = node.clone();
            let ctx = ctx.clone();
            let mut st = stato_sub_run_di(&convocazione);
            st.messages = vec![human(&format!(
                "Sei provider_analyst. Analizza il fornitore {contesto}."
            ))];
            figure.push(async move { node.run(&st, &ctx).await.expect("run ok") });
        }
        futures::future::join_all(figure).await;

        assert_eq!(
            llm.chiamate(),
            2,
            "contesti diversi -> prompt diversi -> due decisioni"
        );
    }

    /// Il run di CHAT non entra in nessuna convocazione: la decisione se la
    /// prende lui, e due run di chat con lo stesso testo non se la scambiano.
    #[tokio::test]
    async fn due_run_di_chat_non_condividono_la_decisione() {
        let node = nodo_di_test_con(cfg_produzione());
        let llm = Arc::new(ScriptedLlm::with_decision(decisione_product()));
        let tools = Arc::new(ContaTools::default());
        let ctx = ctx_with(llm.clone(), tools.clone());
        let st = trigger_state();
        node.run(&st, &ctx).await.expect("run ok");
        node.run(&st, &ctx).await.expect("run ok");
        assert_eq!(
            llm.chiamate(),
            2,
            "senza convocazione non c'e' nessuno con cui condividere"
        );
    }

    /// Il run di CHAT in automatico non e' toccato: li' un umano c'e', la
    /// domanda compare in chat, e `confirm_irreversible_in_auto` continua a fare
    /// cio' per cui esiste (intercettare product/irreversibile).
    #[tokio::test]
    async fn il_run_di_chat_in_automatico_puo_ancora_chiedere() {
        let (node, _store) = nodo_con_store(cfg_produzione());
        let llm = Arc::new(ScriptedLlm::with_decision(decisione_product()));
        let ctx = ctx_with(llm, Arc::new(FailingTools));
        let mut st = trigger_state();
        st.automation_mode = Some(AutomationMode::Automatic);
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));

        assert!(out.is_pending_clarify());
        assert_eq!(out.applied_default_assumptions, None);
    }

    /// expand -> expanded_query, niente pending_clarify ne' meta_step.
    #[tokio::test]
    async fn nodo_expand_popola_expanded_query() {
        let node = nodo_di_test();
        let llm = Arc::new(ScriptedLlm::with_decision(json!({
            "mode": "expand",
            "expanded_query": "implementazione cache sessioni utente con TTL e invalidazione"
        })));
        let ctx = ctx_with(llm, Arc::new(FailingTools));
        let st = trigger_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));

        assert_eq!(
            out.expanded_query.as_deref(),
            Some("implementazione cache sessioni utente con TTL e invalidazione")
        );
        assert_eq!(out.pending_clarify, None);
        assert!(out.meta_steps.is_empty());
    }

    /// skip dal gate (confidence alta) -> delta vuoto, l'LLM NON viene chiamato.
    #[tokio::test]
    async fn nodo_gate_skip_passthrough() {
        let node = nodo_di_test();
        let llm = Arc::new(ScriptedLlm::with_decision(
            json!({"mode": "ask", "question": "x"}),
        ));
        let ctx = ctx_with(llm.clone(), Arc::new(FailingTools));
        let mut st = trigger_state();
        st.intent_confidence = Some(0.95); // sopra soglia -> skip
        let delta = node.run(&st, &ctx).await.expect("run ok");
        assert_eq!(delta.as_map().len(), 0, "skip -> delta vuoto");
        // L'LLM non e' stato interrogato.
        assert!(llm.seen.lock().unwrap().is_empty(), "no LLM call su skip");
    }

    /// LLM emette solo testo (no tool_use) -> no-op.
    #[tokio::test]
    async fn nodo_no_tool_use_noop() {
        let node = nodo_di_test();
        let ctx = ctx_with(
            Arc::new(ScriptedLlm::no_tool()),
            Arc::new(FailingTools),
        );
        let st = trigger_state();
        let delta = node.run(&st, &ctx).await.expect("run ok");
        assert_eq!(delta.as_map().len(), 0);
    }

    /// LLM fallisce -> no-op (best-effort).
    #[tokio::test]
    async fn nodo_llm_fallito_noop() {
        let node = nodo_di_test();
        let ctx = ctx_with(Arc::new(FailingLlm), Arc::new(FailingTools));
        let st = trigger_state();
        let delta = node.run(&st, &ctx).await.expect("run ok");
        assert_eq!(delta.as_map().len(), 0);
    }

}

#[cfg(test)]
mod golden {
    //! Golden-test di PARITA' 1:1 vs Python sulla logica DETERMINISTICA del nodo
    //! clarify. Lo script `/tmp/gen_golden_clarify.py` esercita le funzioni del
    //! brain con I/O fissato e salva `{case_id, function, input, output}` in
    //! `/tmp/golden_clarify.json`. Qui ricostruiamo l'input, chiamiamo la funzione
    //! Rust corrispondente e verifichiamo `output == golden Python`.
    //!
    //! `#[ignore]` perche' dipende dal file generato. Comando:
    //!   python3 /tmp/gen_golden_clarify.py
    //!   cargo test -p nexus-agent-graph golden_clarify_parita -- --ignored

    use serde::Deserialize;
    use serde_json::Value;

    use super::{
        ClarifyConfig, ClarifyMode, ClarifyOrExpandNode, DecisionCategory, GateOutcome, LlmDecision,
    };
    use crate::state::{AgentState, AutomationMode, Message, MessageContent};

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        case_id: String,
        function: String,
        input: Value,
        output: Value,
    }

    /// Ricostruisce la `ClarifyConfig` dal dict `cfg` del golden (default + override).
    fn cfg_from_json(v: &Value) -> ClarifyConfig {
        let mut c = ClarifyConfig::default();
        if let Some(b) = v.get("enabled").and_then(Value::as_bool) {
            c.enabled = b;
        }
        if let Some(f) = v.get("confidence_threshold").and_then(Value::as_f64) {
            c.confidence_threshold = f;
        }
        if let Some(i) = v.get("max_attempts").and_then(Value::as_i64) {
            c.max_attempts = i;
        }
        if let Some(i) = v.get("max_question_chars").and_then(Value::as_i64) {
            c.max_question_chars = i;
        }
        if let Some(f) = v.get("smalltalk_agentic_score_max").and_then(Value::as_f64) {
            c.smalltalk_agentic_score_max = f;
        }
        if let Some(b) = v
            .get("confirm_irreversible_in_auto")
            .and_then(Value::as_bool)
        {
            c.confirm_irreversible_in_auto = b;
        }
        c
    }

    /// Ricostruisce un `AgentState` dai campi rilevanti per i gate.
    fn state_from_json(v: &Value) -> AgentState {
        let mut s = AgentState::default();
        if let Some(h) = v.get("intent_hint").and_then(Value::as_str) {
            s.intent_hint = Some(h.to_string());
        }
        if let Some(i) = v.get("user_intent").and_then(Value::as_str) {
            s.user_intent = Some(i.to_string());
        }
        if let Some(f) = v.get("agentic_score").and_then(Value::as_f64) {
            s.agentic_score = Some(f);
        }
        if let Some(f) = v.get("intent_confidence").and_then(Value::as_f64) {
            s.intent_confidence = Some(f);
        }
        if let Some(i) = v.get("clarify_attempts").and_then(Value::as_i64) {
            s.clarify_attempts = Some(i);
        }
        if let Some(a) = v.get("automation_mode").and_then(Value::as_str) {
            s.automation_mode = match a.to_lowercase().as_str() {
                "automatic" => Some(AutomationMode::Automatic),
                "continuous" => Some(AutomationMode::Continuous),
                "confirm" => Some(AutomationMode::Confirm),
                "none" => Some(AutomationMode::None),
                _ => None,
            };
        }
        s
    }

    /// Serializza un `GateOutcome` nella forma confrontabile col Python (dict
    /// `{"outcome": "skip"}` oppure `{"outcome": "call_llm", "force_classify": b}`).
    fn gate_to_json(g: GateOutcome) -> Value {
        match g {
            GateOutcome::Skip => serde_json::json!({"outcome": "skip"}),
            GateOutcome::CallLlm { force_classify, .. } => {
                serde_json::json!({"outcome": "call_llm", "force_classify": force_classify})
            }
        }
    }

    /// Etichetta del mode (parita' con la stringa Python).
    fn mode_label(m: ClarifyMode) -> &'static str {
        match m {
            ClarifyMode::Ask => "ask",
            ClarifyMode::Expand => "expand",
            ClarifyMode::Skip => "skip",
        }
    }

    /// Etichetta della category (parita' con la stringa Python).
    fn category_label(c: DecisionCategory) -> &'static str {
        match c {
            DecisionCategory::Technical => "technical",
            DecisionCategory::Product => "product",
            DecisionCategory::Irreversible => "irreversible",
        }
    }

    #[test]
    #[ignore = "richiede /tmp/golden_clarify.json generato da gen_golden_clarify.py"]
    fn golden_clarify_parita() {
        let Some(raw) =
            crate::golden_util::load_golden("golden_clarify.json", "gen_golden_clarify.py")
        else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(!cases.is_empty(), "golden vuoto");

        let mut checked = 0usize;
        for c in &cases {
            let got: Value = match c.function.as_str() {
                "pre_llm_gate" => {
                    let cfg = cfg_from_json(c.input.get("cfg").expect("cfg"));
                    let st = state_from_json(c.input.get("state").expect("state"));
                    let user_msg = c
                        .input
                        .get("user_msg")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    gate_to_json(ClarifyOrExpandNode::pre_llm_gate(&cfg, &st, user_msg))
                }
                "last_user_message" => {
                    // messages: array di {role, content-string}.
                    let msgs: Vec<Message> = c
                        .input
                        .get("messages")
                        .and_then(Value::as_array)
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|m| {
                                    let role = m.get("role").and_then(Value::as_str)?;
                                    let content =
                                        m.get("content").and_then(Value::as_str).unwrap_or("");
                                    let mc = MessageContent::Text(content.to_string());
                                    Some(match role {
                                        "user" | "human" => Message::Human { content: mc },
                                        "assistant" | "ai" => Message::Ai {
                                            content: mc,
                                            tool_calls: vec![],
                                            reasoning: None,
                                            thinking_signature: None,
                                        },
                                        _ => Message::Tool {
                                            tool_call_id: "t".to_string(),
                                            content: mc,
                                        },
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Value::String(ClarifyOrExpandNode::last_user_message(&msgs))
                }
                "truncate_question" => {
                    let q = c
                        .input
                        .get("question")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let m = c
                        .input
                        .get("max_chars")
                        .and_then(Value::as_i64)
                        .unwrap_or(0) as usize;
                    Value::String(ClarifyOrExpandNode::truncate_question(q, m))
                }
                "build_project_context" => {
                    let listing = c.input.get("listing").and_then(Value::as_str).unwrap_or("");
                    // Il golden Python conosce il solo TESTO (li' l'esito non
                    // aveva un campo): l'esito si ricostruisce col ponte legacy,
                    // esplicitamente e in un punto solo, per non far rientrare
                    // il criterio testuale nella funzione.
                    let fallito = nexus_types::tool_outcome::is_tool_failure(listing);
                    Value::String(ClarifyOrExpandNode::build_project_context(listing, fallito))
                }
                "llm_decision" => {
                    let d = LlmDecision::from_tool_input(c.input.get("tool_input").expect("input"));
                    serde_json::json!({
                        "mode": mode_label(d.mode),
                        "category": category_label(d.category),
                        "reversible": d.reversible,
                        "question": d.question,
                        "expanded_query": d.expanded_query,
                        "rationale": d.rationale,
                    })
                }
                "build_expand_delta" => {
                    let d = LlmDecision::from_tool_input(c.input.get("tool_input").expect("input"));
                    let user_msg = c
                        .input
                        .get("user_msg")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    match ClarifyOrExpandNode::build_expand_delta(&d, user_msg) {
                        // None -> {"expanded_query": null} (no-op), Some -> il valore.
                        None => serde_json::json!(null),
                        Some(delta) => match delta.expanded_query {
                            Some(Some(v)) => Value::String(v),
                            _ => serde_json::json!(null),
                        },
                    }
                }
                other => panic!("funzione golden sconosciuta: {other} (caso {})", c.case_id),
            };

            assert!(
                got == c.output,
                "PARITA' FALLITA caso {} ({}):\n  rust   = {}\n  python = {}",
                c.case_id,
                c.function,
                got,
                c.output
            );
            checked += 1;
        }
        println!("golden clarify: {checked} casi verificati, tutti verdi");
    }
}
