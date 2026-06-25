//! `ClarifyOrExpandNode` — porta la parte PORTABILE/deterministica di
//! `clarify_or_expand_node` (`brain/agents/clarify_or_expand_node.py:587-898`).
//!
//! Il nodo e' condizionale: si attiva SOLO su bassa confidence del classifier
//! intent (~1% dei casi) e produce due output mutuamente esclusivi:
//!   - `ask`    -> meta_step `clarify` con la domanda + `pending_clarify=true`
//!                 (il grafo va a END, il turno si ferma) + `clarify_attempts+1`.
//!   - `expand` -> popola `expanded_query` (arricchisce il retrieve RAG; il
//!                 messaggio utente passa intatto al modello principale).
//! Negli altri casi e' no-op (delta vuoto), il flusso prosegue invariato.
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

use crate::runtime::AgentNodeCtx;
use crate::state::{AgentState, Message, MetaStep, StateDelta, ToolUse};

/// Lunghezza minima dell'ultimo messaggio utente sotto la quale il nodo salta
/// (`clarify_or_expand_node.py:752`: `len(user_msg) < 3`).
const MIN_USER_MSG_LEN: usize = 3;

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
}

impl ClarifyOrExpandNode {
    /// Costruisce il nodo con la config DB-driven gia' risolta dal chiamante.
    pub fn new(cfg: ClarifyConfig) -> Self {
        Self { cfg }
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
    /// (`clarify_or_expand_node.py:417` / `:692`). Nel runtime Rust
    /// `automation_mode` e' gia' un enum: `Automatic`/`Continuous` sono auto.
    pub fn is_auto(state: &AgentState) -> bool {
        use crate::state::AutomationMode;
        matches!(
            state.automation_mode,
            Some(AutomationMode::Automatic) | Some(AutomationMode::Continuous)
        )
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
        let max_clarify = if cfg.max_attempts != 0 { cfg.max_attempts } else { 1 };
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
    pub fn build_project_context(listing: &str) -> String {
        // Guard errori identica al Python (:565): vuoto, prefisso di errore noto.
        if listing.is_empty()
            || listing.starts_with('\u{274c}') // simbolo "x rossa" usato dai tool
            || listing.get(..30).unwrap_or(listing).contains("[Errore")
            || listing.get(..30).unwrap_or(listing).contains("[Error")
        {
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
    /// config + force_classify + confidence -> delta.
    ///
    /// Ritorna `Some(StateDelta)` con il meta_step clarify + `pending_clarify` +
    /// `clarify_attempts+1`; `None` quando il gate force_classify procede senza
    /// domanda, oppure quando la question e' vuota (no-op, `:852-853`).
    pub fn build_ask_delta(
        cfg: &ClarifyConfig,
        state: &AgentState,
        decision: &LlmDecision,
        force_classify: bool,
        confidence: f64,
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

        let payload = json!({
            "question": question,
            "rationale": decision.rationale,
            "category": Self::category_label(decision.category),
            "reversible": decision.reversible,
            "intent": state.user_intent,
            "confidence": confidence,
        });
        let meta = MetaStep {
            kind: "clarify".to_string(),
            title: "Serve un chiarimento".to_string(),
            payload,
            correlation_id: None,
            created_at: None,
        };
        let clarify_attempts = state.clarify_attempts.unwrap_or(0);
        Some(StateDelta {
            pending_clarify: Some(Some(true)),
            clarify_attempts: Some(Some(clarify_attempts + 1)),
            meta_steps: Some(vec![meta]),
            ..Default::default()
        })
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
                    "reversible": {"type": "boolean", "description": "True se la decisione e' facilmente reversibile."}
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
        let (force_classify, confidence) =
            match Self::pre_llm_gate(&self.cfg, state, &user_msg) {
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

        let mode = ctx.exec_mode();

        // ── project_context: UNA list_files top-level (I/O dietro la porta) ───
        // Best-effort: errore -> blocco vuoto (comportamento storico, :561-563).
        let project_context = {
            let call = ToolUse {
                id: Uuid::new_v4().to_string(),
                name: "list_files".to_string(),
                input: json!({ "directory": "." }),
            };
            match ctx.tools.execute(call, mode).await {
                Ok(outcome) => {
                    let raw = Self::outcome_result_json(&outcome.content);
                    Self::build_project_context(&raw)
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
        };

        // ── Decisione LLM (TODO I/O delegato) ─────────────────────────────────
        // Il provider/model del purpose `clarify_expand` + il system prompt
        // `agent.clarify.base` sono RISOLTI A MONTE (regola G). Finche' non c'e'
        // la porta che li fornisce, il chiamante li passa via `LlmRequest`
        // (provider/model gia' decisi). Il project_context (se presente) e'
        // appeso al system prompt dal chiamante: qui lo passiamo come messaggio di
        // sistema implicito tramite il primo blocco. La forma della richiesta
        // resta minimale e provider-agnostica (porte ports.rs).
        let llm_response = {
            use crate::runtime::ports::{LlmMessage, LlmRequest};
            // Il system prompt arriva risolto a monte: qui costruiamo SOLO la
            // parte nota (user msg + eventuale contesto progetto come system).
            let mut messages: Vec<LlmMessage> = Vec::new();
            if !project_context.is_empty() {
                messages.push(LlmMessage {
                    role: "system".to_string(),
                    content: Value::String(project_context),
                });
            }
            messages.push(LlmMessage {
                role: "user".to_string(),
                content: Value::String(user_msg.clone()),
            });
            let req = LlmRequest {
                // provider/model RISOLTI A MONTE dal chiamante (regola G): qui
                // restano vuoti finche' la porta purpose-resolver non li fornisce.
                // mcp-core li popolera' con `resolve_purpose_model("clarify_expand")`.
                provider: String::new(),
                model: String::new(),
                messages,
                tools: Some(vec![Self::tool_schema()]),
            };
            match ctx.llm.complete(req).await {
                Ok(resp) => resp,
                Err(err) => {
                    // LLM call fallita -> skip (no-op), come il Python (:816-818).
                    tracing::warn!(
                        target: "nexus_agent_graph::clarify_or_expand",
                        error = %err,
                        "chiamata LLM clarify fallita (best-effort, no-op)"
                    );
                    return Ok(StateDelta::default().into_opaque());
                }
            }
        };

        // Estrae il tool_use `clarify_or_expand`; assente -> skip (:822-828).
        let tool_call = llm_response
            .tool_calls
            .iter()
            .find(|t| t.name == "clarify_or_expand");
        let Some(tool_call) = tool_call else {
            tracing::info!(
                target: "nexus_agent_graph::clarify_or_expand",
                blocks = llm_response.tool_calls.len(),
                "tool_use 'clarify_or_expand' non emesso, no-op"
            );
            return Ok(StateDelta::default().into_opaque());
        };

        let decision = LlmDecision::from_tool_input(&tool_call.input);
        tracing::info!(
            target: "nexus_agent_graph::clarify_or_expand",
            mode = ?decision.mode,
            category = ?decision.category,
            reversible = decision.reversible,
            "decisione clarify"
        );

        // ── Applica la decisione (logica deterministica) ──────────────────────
        let delta = match decision.mode {
            ClarifyMode::Ask => {
                Self::build_ask_delta(&self.cfg, state, &decision, force_classify, confidence)
                    .unwrap_or_default()
            }
            ClarifyMode::Expand => {
                Self::build_expand_delta(&decision, &user_msg).unwrap_or_default()
            }
            // mode=skip o sconosciuto -> no-op (:897-898).
            ClarifyMode::Skip => StateDelta::default(),
        };

        Ok(delta.into_opaque())
    }
}

impl ClarifyOrExpandNode {
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
        EventSink, ExecMode, LlmGateway, LlmRequest, LlmResponse, LlmUsage, PortError, SseEvent,
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
                }],
                None => vec![],
            };
            Ok(LlmResponse {
                content: String::new(),
                tool_calls,
                usage: LlmUsage::default(),
            })
        }
    }

    /// Gateway che fallisce sempre (path no-op su errore LLM).
    struct FailingLlm;
    #[async_trait]
    impl LlmGateway for FailingLlm {
        async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, PortError> {
            Err(PortError::Llm("simulato".to_string()))
        }
    }

    /// Tool executor che ritorna un listing fisso per `list_files` e registra le
    /// modalita' osservate.
    struct ListingTools {
        listing: String,
        modes: std::sync::Mutex<Vec<ExecMode>>,
    }
    impl ListingTools {
        fn new(listing: &str) -> Self {
            Self {
                listing: listing.to_string(),
                modes: std::sync::Mutex::new(vec![]),
            }
        }
    }
    #[async_trait]
    impl ToolExecutor for ListingTools {
        async fn execute(&self, call: ToolCall, mode: ExecMode) -> Result<ToolOutcome, PortError> {
            self.modes.lock().unwrap().push(mode);
            assert_eq!(call.name, "list_files");
            Ok(ToolOutcome {
                tool_call_id: call.id,
                content: Value::String(self.listing.clone()),
                is_error: false,
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
            _mode: ExecMode,
        ) -> Result<ToolOutcome, PortError> {
            Err(PortError::Tool("simulato".to_string()))
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
        shadow: bool,
    ) -> AgentNodeCtx {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy non si connette");
        AgentNodeCtx {
            db: pool,
            llm,
            tools,
            emit: Arc::new(Sink),
            cfg: crate::routing::config::RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            shadow,
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
        let ctx = ClarifyOrExpandNode::build_project_context(listing);
        assert!(ctx.contains("CONTESTO PROGETTO"));
        assert!(ctx.contains("package.json"));
        assert!(ctx.contains("src/"));
        // listing in errore -> vuoto.
        assert_eq!(
            ClarifyOrExpandNode::build_project_context("[Errore: dir non trovata]"),
            ""
        );
        // nessun marcatore -> vuoto.
        assert_eq!(ClarifyOrExpandNode::build_project_context("foo.txt\nbar.csv"), "");
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
        assert!(ClarifyOrExpandNode::build_ask_delta(&cfg, &st, &decision, true, 0.4).is_none());
    }

    #[test]
    fn build_ask_delta_force_classify_product_emette() {
        let cfg = ClarifyConfig::default();
        let st = trigger_state();
        let decision = LlmDecision::from_tool_input(&json!({
            "mode": "ask", "question": "scelta di prodotto?", "category": "product"
        }));
        let delta =
            ClarifyOrExpandNode::build_ask_delta(&cfg, &st, &decision, true, 0.4).expect("delta");
        assert_eq!(delta.pending_clarify, Some(Some(true)));
    }

    #[test]
    fn build_ask_delta_question_vuota_none() {
        let cfg = ClarifyConfig::default();
        let st = trigger_state();
        let decision = LlmDecision::from_tool_input(&json!({"mode": "ask", "question": "  "}));
        assert!(ClarifyOrExpandNode::build_ask_delta(&cfg, &st, &decision, false, 0.4).is_none());
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

    /// ask -> pending_clarify + meta_step + clarify_attempts incrementato.
    #[tokio::test]
    async fn nodo_ask_emette_pending_clarify_e_meta_step() {
        let node = ClarifyOrExpandNode::new(ClarifyConfig::default());
        let llm = Arc::new(ScriptedLlm::with_decision(json!({
            "mode": "ask",
            "question": "Vuoi una cache in-memory o Redis?",
            "rationale": "scelta architetturale ambigua",
            "category": "technical",
            "reversible": true
        })));
        let tools = Arc::new(FailingTools); // project_context vuoto, irrilevante
        let ctx = ctx_with(llm, tools, false);
        let st = trigger_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));

        assert_eq!(out.pending_clarify, Some(true));
        assert_eq!(out.clarify_attempts, Some(1), "clarify_attempts incrementato");
        assert_eq!(out.meta_steps.len(), 1);
        assert_eq!(out.meta_steps[0].kind, "clarify");
        assert_eq!(out.meta_steps[0].title, "Serve un chiarimento");
    }

    /// expand -> expanded_query, niente pending_clarify ne' meta_step.
    #[tokio::test]
    async fn nodo_expand_popola_expanded_query() {
        let node = ClarifyOrExpandNode::new(ClarifyConfig::default());
        let llm = Arc::new(ScriptedLlm::with_decision(json!({
            "mode": "expand",
            "expanded_query": "implementazione cache sessioni utente con TTL e invalidazione"
        })));
        let ctx = ctx_with(llm, Arc::new(FailingTools), false);
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
        let node = ClarifyOrExpandNode::new(ClarifyConfig::default());
        let llm = Arc::new(ScriptedLlm::with_decision(json!({"mode": "ask", "question": "x"})));
        let ctx = ctx_with(llm.clone(), Arc::new(FailingTools), false);
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
        let node = ClarifyOrExpandNode::new(ClarifyConfig::default());
        let ctx = ctx_with(Arc::new(ScriptedLlm::no_tool()), Arc::new(FailingTools), false);
        let st = trigger_state();
        let delta = node.run(&st, &ctx).await.expect("run ok");
        assert_eq!(delta.as_map().len(), 0);
    }

    /// LLM fallisce -> no-op (best-effort).
    #[tokio::test]
    async fn nodo_llm_fallito_noop() {
        let node = ClarifyOrExpandNode::new(ClarifyConfig::default());
        let ctx = ctx_with(Arc::new(FailingLlm), Arc::new(FailingTools), false);
        let st = trigger_state();
        let delta = node.run(&st, &ctx).await.expect("run ok");
        assert_eq!(delta.as_map().len(), 0);
    }

    /// Shadow: la list_files del project_context gira in Replay (zero side-effect).
    #[tokio::test]
    async fn nodo_shadow_usa_replay() {
        let node = ClarifyOrExpandNode::new(ClarifyConfig::default());
        let tools = Arc::new(ListingTools::new("package.json\nsrc/"));
        let llm = Arc::new(ScriptedLlm::with_decision(json!({"mode": "skip"})));
        let ctx = ctx_with(llm, tools.clone(), true);
        let st = trigger_state();
        let _ = node.run(&st, &ctx).await.expect("run ok");
        let modes = tools.modes.lock().unwrap();
        assert!(!modes.is_empty());
        assert!(modes.iter().all(|m| *m == ExecMode::Replay));
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

    use super::{ClarifyConfig, ClarifyMode, ClarifyOrExpandNode, DecisionCategory, GateOutcome, LlmDecision};
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
        if let Some(b) = v.get("confirm_irreversible_in_auto").and_then(Value::as_bool) {
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
        let path = "/tmp/golden_clarify.json";
        let raw = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!("impossibile leggere {path}: {e}; genera con python3 /tmp/gen_golden_clarify.py")
        });
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(!cases.is_empty(), "golden vuoto");

        let mut checked = 0usize;
        for c in &cases {
            let got: Value = match c.function.as_str() {
                "pre_llm_gate" => {
                    let cfg = cfg_from_json(c.input.get("cfg").expect("cfg"));
                    let st = state_from_json(c.input.get("state").expect("state"));
                    let user_msg = c.input.get("user_msg").and_then(Value::as_str).unwrap_or("");
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
                    let q = c.input.get("question").and_then(Value::as_str).unwrap_or("");
                    let m =
                        c.input.get("max_chars").and_then(Value::as_i64).unwrap_or(0) as usize;
                    Value::String(ClarifyOrExpandNode::truncate_question(q, m))
                }
                "build_project_context" => {
                    let listing =
                        c.input.get("listing").and_then(Value::as_str).unwrap_or("");
                    Value::String(ClarifyOrExpandNode::build_project_context(listing))
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
                    let user_msg =
                        c.input.get("user_msg").and_then(Value::as_str).unwrap_or("");
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
