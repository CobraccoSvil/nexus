//! `ReflectionNode` — porta la parte PORTABILE/deterministica del
//! `reflection_node` (`brain/agents/nodes/__init__.py:4227-4423`) + la rubrica
//! statica (`brain/agents/reflection_rubric.py`).
//!
//! La self-reflection e' un nodo POST-esecuzione, CAMPIONATO (~30% di default):
//! valuta l'output dell'agente con una rubrica statica (4 dimensioni pesate),
//! ottiene un punteggio JSON dal modello valutatore, e FONDE quel punteggio con
//! un reward euristico per produrre il `final_reward` (segnale di qualita' per
//! il PromptOptimizerWorker / reasoning_bank). E' best-effort: ogni guasto
//! (LLM down, parsing fallito, sampling escluso) degrada al SOLO euristico o a
//! un pass-through `{None, None}`, MAI un errore propagato.
//!
//! ## Cosa porta QUESTO PR (deterministico, testato golden 1:1)
//!
//! - **Gli 8 gate nell'ordine Python** (`Self::run`): flag OFF -> {None,None};
//!   tag `<reflection>` assente in `system_text` -> {None,None}; sampling
//!   (`roll > sample_rate`) -> {None,None}; `result` vuoto -> {None,None};
//!   `ctx.llm` errore/timeout -> SOLO euristico (final_reward=None); parsing
//!   fallito -> SOLO euristico; persistenza se reflection_data+prompt_key;
//!   reasoning_bank se score>=soglia && suggestions (TODO, vedi sotto).
//! - **`should_sample`** (`__init__.py:4269`): funzione PURA `roll > rate ->
//!   skip`. Il `roll` (RNG) e' iniettato (test deterministici); nel nodo arriva
//!   da `rand`.
//! - **La rubrica statica** (`reflection_rubric.py`): dimensioni+pesi, troncamenti
//!   task@2000/output@3000, rendering `.format()` deterministico, parsing JSON a
//!   2 tentativi (json puro -> primo blocco `{...}` via regex), validazione range
//!   0.0-1.0, round a 3 decimali (half-to-even), cap weaknesses/suggestions a 3.
//! - **Il reward euristico + final_reward**: delegati al PUNTO UNICO
//!   `decisions::reward` (regola L: identico al learner, NON duplicato qui).
//!
//! ## Cosa NON porta (I/O delegato dietro i trait / TODO espliciti)
//!
//! - La **scelta provider** ("anthropic" se disponibile, altrimenti il
//!   `provider_used` del run, `__init__.py:4317`) e il **model** (`reflection_model`
//!   dal DB) sono RISOLTI A MONTE (regola G): arrivano nella [`ReflectionConfig`]
//!   gia' decisi. Il nodo NON sceglie il provider/model, NON legge il DB per la
//!   config.
//! - I **template** del prompt (`system.reflection_rubric` /
//!   `system.reflection_user_template`, mig 0448) sono I/O DB: arrivano risolti
//!   nella [`ReflectionConfig`] con fallback alle costanti `SYSTEM_RUBRIC` /
//!   `TEMPLATE_UTENTE` (safe-default, replica del fallback Python).
//! - La **persistenza** in `nexus_agent_reflections` (`_persist_reflection`,
//!   `__init__.py:4426`) e' delegata a `ctx.db` come task best-effort
//!   fire-and-forget, GATED su shadow (in shadow NON scrive: zero side-effect).
//! - Il **reasoning_bank** (`maybe_store_reflection_example`, embedding pesante):
//!   NON portato in questo PR -> TODO esplicito dietro porta dedicata.
//!
//! Il nodo NON instrada (l'edge reflection->learner e' fuori, in `edge.rs`).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::decisions::reward::{final_reward as fuse_final_reward, heuristic_reward, round_half_even};
use crate::runtime::AgentNodeCtx;
use crate::state::{AgentState, ContentBlock, Message, MessageContent, StateDelta};

/// Lunghezza massima del task originale nel prompt (`reflection_rubric.py:116`:
/// `task_input[:2000]`).
const TASK_MAX_CHARS: usize = 2000;

/// Lunghezza massima dell'output agente nel prompt (`reflection_rubric.py:117`:
/// `agent_output[:3000]`).
const OUTPUT_MAX_CHARS: usize = 3000;

/// Dimensioni della rubrica con (descrizione, peso). Replica `DIMENSIONI`
/// (`reflection_rubric.py:33-50`). I pesi sommano a 1.0. Ordine LOAD-BEARING
/// (governa l'ordine di rendering e di validazione, deterministico). Niente nome
/// modello/provider qui (regola G).
const DIMENSIONI: &[(&str, &str, f64)] = &[
    (
        "correctness",
        "L'output risolve correttamente e completamente il problema richiesto?",
        0.40,
    ),
    (
        "completeness",
        "L'output copre tutti gli aspetti del task senza lasciare parti irrisolte o incomplete?",
        0.30,
    ),
    (
        "efficiency",
        "L'agente ha usato il numero minimo necessario di iterazioni e tool, senza ridondanze?",
        0.15,
    ),
    (
        "safety",
        "L'agente ha evitato azioni distruttive o irreversibili non esplicitamente richieste?",
        0.15,
    ),
];

/// System prompt di FALLBACK del valutatore (`reflection_rubric.py:53-58`,
/// `_SYSTEM_RUBRIC`). Safe-default: usato solo se il DB non fornisce il template
/// `system.reflection_rubric` (regola G: il valore di produzione viene dal DB).
pub const SYSTEM_RUBRIC: &str = "Sei un valutatore critico e imparziale di output di agenti AI specializzati in sviluppo software.\nIl tuo unico compito e' analizzare l'output dell'agente e produrre una valutazione JSON strutturata.\nNon devi generare codice, correggere bug o svolgere il task originale: solo valutare.\nRispondi ESCLUSIVAMENTE con JSON valido, senza testo aggiuntivo, markdown o delimitatori.\n";

/// Template UTENTE di FALLBACK (`reflection_rubric.py:60-93`, `_TEMPLATE_UTENTE`).
/// Contiene i 3 placeholder `{task}` / `{output}` / `{rubrica_dettaglio}` e le
/// graffe letterali `{{` / `}}` dello scheletro JSON. Reso da
/// [`render_user_template`] (semantica `str.format`). Safe-default DB-driven.
pub const TEMPLATE_UTENTE: &str = "<task_originale>\n{task}\n</task_originale>\n\n<output_agente>\n{output}\n</output_agente>\n\n<rubrica>\nValuta ciascuna dimensione con un punteggio da 0.0 (pessimo) a 1.0 (eccellente):\n\n{rubrica_dettaglio}\n</rubrica>\n\nIstruzioni:\n1. Assegna un punteggio per ciascuna dimensione.\n2. Calcola il punteggio finale come media ponderata (pesi: correctness=0.40, completeness=0.30, efficiency=0.15, safety=0.15).\n3. Elenca al massimo 3 punti deboli specifici e concreti (non generici).\n4. Suggerisci al massimo 3 miglioramenti concreti e applicabili al prompt dell'agente.\n\nRispondi SOLO con questo JSON (nessun altro testo):\n{{\n  \"score\": <float 0.0-1.0>,\n  \"dimensions\": {{\n    \"correctness\": <float>,\n    \"completeness\": <float>,\n    \"efficiency\": <float>,\n    \"safety\": <float>\n  }},\n  \"weaknesses\": [\"<stringa>\", \"...\"],\n  \"suggestions\": [\"<stringa>\", \"...\"]\n}}\n";

/// Placeholder usato dal Python quando il task e' vuoto (`reflection_rubric.py:116`).
const TASK_PLACEHOLDER: &str = "(nessun input)";

/// Placeholder usato dal Python quando l'output e' vuoto (`reflection_rubric.py:117`).
const OUTPUT_PLACEHOLDER: &str = "(nessun output)";

/// Regex che estrae il primo blocco `{...}` da un testo (replica `_JSON_RE`,
/// `reflection_rubric.py:131`: `r"\{[\s\S]*\}"`, greedy multiline). Compilata una
/// volta sola (`LazyLock`).
static JSON_RE: LazyLock<Regex> = LazyLock::new(|| {
    // `(?s)` = dotall (`.` matcha newline); equivalente a `[\s\S]` del Python.
    Regex::new(r"(?s)\{.*\}").expect("regex JSON statica valida")
});

/// Config DB-driven del nodo reflection, PASSATA (regola G: nessuna lettura DB
/// nel nodo, nessun fallback hardcoded dentro la logica decisionale).
///
/// Mappa i settings letti dal brain via `reflection_config.get()`
/// (`reflection_config.py`, categoria `reflection`) PIU' i template risolti dal
/// registry (mig 0448) e il provider/model RISOLTI A MONTE (regola G): la scelta
/// "anthropic se disponibile, altrimenti provider_used del run" e' del chiamante.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReflectionConfig {
    /// Reflection attiva (`reflection_enabled`, default true; safe-default DB
    /// down = false). OFF -> {reflection_score:None, final_reward:None}.
    pub enabled: bool,
    /// Tasso di campionamento (`reflection_sample_rate`, default 0.3): un roll
    /// `> sample_rate` esclude il turno.
    pub sample_rate: f64,
    /// Timeout della chiamata LLM in secondi (`reflection_timeout_s`, def 10.0).
    pub timeout_s: f64,
    /// Provider RISOLTO A MONTE (regola G): "anthropic" se disponibile altrimenti
    /// il provider del run. Il nodo NON lo sceglie.
    pub provider: String,
    /// Modello valutatore RISOLTO A MONTE (`reflection_model`, regola G): mai
    /// hardcoded qui. Stringa vuota -> reflection disabilitata (safe-default).
    pub model: String,
    /// Peso del reflection_score nella fusione (`reflection_reward_weight`,
    /// def 0.3). `heuristic_weight = 1 - reward_weight`.
    pub reward_weight: f64,
    /// Soglia minima dello score per il bridge reasoning_bank
    /// (`reflection_reasoning_bank_min_score`, def 0.85).
    pub reasoning_bank_min_score: f64,
    /// System prompt del valutatore risolto dal DB (mig 0448) con fallback a
    /// [`SYSTEM_RUBRIC`]. Risolto a monte (regola G).
    pub system_template: String,
    /// Template utente risolto dal DB con fallback a [`TEMPLATE_UTENTE`].
    pub user_template: String,
}

impl Default for ReflectionConfig {
    fn default() -> Self {
        // Default IDENTICI ai default del brain (`reflection_config.py:11-16`),
        // con i template ai safe-default costanti. Valgono SOLO se il DB e'
        // irraggiungibile, mai come magic fallback nella logica.
        Self {
            enabled: true,
            sample_rate: 0.3,
            timeout_s: 10.0,
            provider: String::new(),
            model: String::new(),
            reward_weight: 0.3,
            reasoning_bank_min_score: 0.85,
            system_template: SYSTEM_RUBRIC.to_string(),
            user_template: TEMPLATE_UTENTE.to_string(),
        }
    }
}

/// Dati di reflection gia' parsati e validati (l'output di [`parse_reflection`]).
/// Replica il dict prodotto da `_validate_reflection` (`reflection_rubric.py:184-189`).
#[derive(Debug, Clone, PartialEq)]
pub struct ReflectionData {
    /// Punteggio aggregato 0.0-1.0, round a 3 decimali.
    pub score: f64,
    /// Punteggio per dimensione (chiavi = nomi rubrica), round a 3 decimali.
    /// `BTreeMap` per ordine stabile nella serializzazione (parita' golden).
    pub dimensions: BTreeMap<String, f64>,
    /// Fino a 3 punti deboli (stringificati).
    pub weaknesses: Vec<String>,
    /// Fino a 3 suggerimenti (stringificati).
    pub suggestions: Vec<String>,
}

impl ReflectionData {
    /// `dimensions` come `Value` JSON object (per il delta / la persistenza).
    fn dimensions_json(&self) -> Value {
        let map: serde_json::Map<String, Value> = self
            .dimensions
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        Value::Object(map)
    }
}

/// Nodo self-reflection. Stateless: legge lo stato + la config passata e fa I/O
/// tramite le porte del `AgentNodeCtx` (LLM via `ctx.llm`, persistenza via
/// `ctx.db`). La scelta provider/model e i template sono RISOLTI A MONTE
/// (regola G); la logica di gate, rubrica e fusione reward e' interamente qui.
pub struct ReflectionNode {
    /// Config DB-driven (regola G: passata, mai letta dal nodo).
    cfg: ReflectionConfig,
}

impl ReflectionNode {
    /// Costruisce il nodo con la config DB-driven gia' risolta dal chiamante.
    pub fn new(cfg: ReflectionConfig) -> Self {
        Self { cfg }
    }

    /// Funzione PURA: il turno va ESCLUSO dal campionamento? (`__init__.py:4269`:
    /// `random.random() > cfg_sample_rate -> skip`). `roll` e' il valore RNG in
    /// `[0,1)` (iniettato nei test, generato nel nodo). `true` = skip.
    pub fn should_sample_skip(roll: f64, sample_rate: f64) -> bool {
        roll > sample_rate
    }

    /// Estrae il testo "piatto" dal contenuto di un messaggio (concatena i blocchi
    /// `Text` se strutturato). Per il content stringa restituisce la stringa.
    fn flatten_text(content: &MessageContent) -> String {
        match content {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    /// Testo del task originale = content del PRIMO HumanMessage dei messages
    /// (`__init__.py:4296-4301`: itera in avanti, primo `HumanMessage`, `break`).
    /// Stringa vuota se non c'e' alcun messaggio umano.
    pub fn task_input(messages: &[Message]) -> String {
        for m in messages {
            if let Message::Human { content } = m {
                return Self::flatten_text(content);
            }
        }
        String::new()
    }

    /// Dettaglio testuale della rubrica (`reflection_rubric.py:96-101`,
    /// `_rubrica_dettaglio`): una riga per dimensione `- {nome} (peso {p:.0%}): {desc}`,
    /// join con `\n`. `{p:.0%}` = percentuale intera (0.40 -> "40%").
    pub fn rubrica_dettaglio() -> String {
        DIMENSIONI
            .iter()
            .map(|(nome, descrizione, peso)| {
                // `{:.0%}` Python: moltiplica per 100, arrotonda a 0 decimali, '%'.
                let pct = (peso * 100.0).round() as i64;
                format!("- {nome} (peso {pct}%): {descrizione}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Rende il template utente sostituendo i 3 placeholder, con la semantica di
    /// `str.format(task=..., output=..., rubrica_dettaglio=...)` Python
    /// (`reflection_rubric.py:122-125`): `{{`/`}}` -> graffe letterali, i
    /// placeholder noti -> i valori. Implementazione manuale a stati per
    /// replicare ESATTAMENTE `str.format` su questo set di chiavi (niente
    /// dipendenza da un mini-template engine).
    ///
    /// I troncamenti del task/output sono applicati dal chiamante prima di
    /// passarli qui (vedi [`build_reflection_prompt`]).
    pub fn render_user_template(
        template: &str,
        task: &str,
        output: &str,
        rubrica_dettaglio: &str,
    ) -> String {
        let mut out = String::with_capacity(template.len() + task.len() + output.len());
        let mut chars = template.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' => {
                    if chars.peek() == Some(&'{') {
                        // `{{` -> `{` letterale.
                        chars.next();
                        out.push('{');
                        continue;
                    }
                    // Legge il nome del campo fino a `}`.
                    let mut field = String::new();
                    for fc in chars.by_ref() {
                        if fc == '}' {
                            break;
                        }
                        field.push(fc);
                    }
                    match field.as_str() {
                        "task" => out.push_str(task),
                        "output" => out.push_str(output),
                        "rubrica_dettaglio" => out.push_str(rubrica_dettaglio),
                        // Campo sconosciuto: in Python `.format` solleverebbe
                        // KeyError -> fallback alla costante; qui i template del
                        // DB sono validati a monte. Per robustezza lasciamo il
                        // placeholder grezzo (non capita coi template noti).
                        other => {
                            out.push('{');
                            out.push_str(other);
                            out.push('}');
                        }
                    }
                }
                '}' => {
                    if chars.peek() == Some(&'}') {
                        // `}}` -> `}` letterale.
                        chars.next();
                    }
                    out.push('}');
                }
                _ => out.push(c),
            }
        }
        out
    }

    /// Costruisce `(system_prompt, user_prompt)` per la reflection
    /// (`reflection_rubric.py:104-127`, `build_reflection_prompt`). Applica i
    /// troncamenti (task@2000, output@3000) e i placeholder per gli input vuoti,
    /// poi rende il template utente. I template arrivano dalla config (DB-driven
    /// con fallback alle costanti, regola G).
    pub fn build_reflection_prompt(
        cfg: &ReflectionConfig,
        task_input: &str,
        agent_output: &str,
    ) -> (String, String) {
        // task vuoto -> placeholder; altrimenti tronca a 2000 char.
        let task: String = if task_input.is_empty() {
            TASK_PLACEHOLDER.to_string()
        } else {
            task_input.chars().take(TASK_MAX_CHARS).collect()
        };
        let output: String = if agent_output.is_empty() {
            OUTPUT_PLACEHOLDER.to_string()
        } else {
            agent_output.chars().take(OUTPUT_MAX_CHARS).collect()
        };
        let dettaglio = Self::rubrica_dettaglio();
        let user = Self::render_user_template(&cfg.user_template, &task, &output, &dettaglio);
        (cfg.system_template.clone(), user)
    }

    /// Parsa la risposta grezza del modello (`reflection_rubric.py:134-165`,
    /// `parse_reflection_response`). 2 tentativi: (1) il testo e' JSON puro
    /// (trimmato); (2) il PRIMO blocco `{...}` estratto via regex. Ogni tentativo
    /// valida via [`Self::validate`]; al primo successo ritorna; altrimenti `None`.
    pub fn parse_reflection(raw: &str) -> Option<ReflectionData> {
        if raw.is_empty() {
            return None;
        }
        // Tentativo 1: JSON puro (trim come `raw.strip()`).
        if let Ok(data) = serde_json::from_str::<Value>(raw.trim()) {
            if let Some(rd) = Self::validate(&data) {
                return Some(rd);
            }
        }
        // Tentativo 2: primo blocco `{...}` (greedy, come il Python).
        if let Some(m) = JSON_RE.find(raw) {
            if let Ok(data) = serde_json::from_str::<Value>(m.as_str()) {
                if let Some(rd) = Self::validate(&data) {
                    return Some(rd);
                }
            }
        }
        None
    }

    /// Valida e normalizza il dict di reflection (`reflection_rubric.py:168-189`,
    /// `_validate_reflection`). In Python solleva `ValueError` fuori range; qui
    /// ritorna `None` (il chiamante `parse_reflection` lo tratta come tentativo
    /// fallito, identico al `except ValueError` Python).
    ///
    /// Regole 1:1:
    /// - `score = float(data.get("score", -1))`; fuori `[0,1]` -> None.
    /// - per ogni dimensione: `v = float(dims.get(dim, -1))`; fuori `[0,1]` -> None.
    /// - round score/dims a 3 decimali (half-to-even).
    /// - weaknesses/suggestions: stringifica i PRIMI 3 elementi (`[:3]`).
    pub fn validate(data: &Value) -> Option<ReflectionData> {
        let obj = data.as_object()?;
        // `float(data.get("score", -1))`: assente -> -1.0 (fuori range -> None).
        let score = Self::as_float(obj.get("score"), -1.0)?;
        if !(0.0..=1.0).contains(&score) {
            return None;
        }
        // dims = data.get("dimensions", {}) — assente => {} => ogni dim manca => -1.
        let dims = obj.get("dimensions").and_then(Value::as_object);
        let mut dimensions = BTreeMap::new();
        for (nome, _desc, _peso) in DIMENSIONI {
            let raw_v = dims.and_then(|d| d.get(*nome));
            let v = Self::as_float(raw_v, -1.0)?;
            if !(0.0..=1.0).contains(&v) {
                return None;
            }
            dimensions.insert((*nome).to_string(), round_half_even(v, 3));
        }
        Some(ReflectionData {
            score: round_half_even(score, 3),
            dimensions,
            weaknesses: Self::stringify_cap3(obj.get("weaknesses")),
            suggestions: Self::stringify_cap3(obj.get("suggestions")),
        })
    }

    /// `float(value)` Python con default se assente/None. Ritorna `None` se il
    /// valore e' presente ma NON convertibile a float (replica il `ValueError`
    /// di `float("abc")` che fa fallire la validazione -> None).
    fn as_float(value: Option<&Value>, default: f64) -> Option<f64> {
        match value {
            None | Some(Value::Null) => Some(default),
            Some(Value::Number(n)) => n.as_f64(),
            Some(Value::Bool(b)) => Some(if *b { 1.0 } else { 0.0 }), // `float(True)`==1.0
            Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
            // list/dict -> `float(...)` solleva TypeError -> fallimento (None).
            Some(_) => None,
        }
    }

    /// `[str(x) for x in (value or [])[:3]]` (`reflection_rubric.py:187-188`):
    /// prende i PRIMI 3 elementi dell'iterabile e li stringifica.
    ///
    /// CONTRATTO: la rubrica si aspetta `weaknesses`/`suggestions` come LISTE di
    /// stringhe. Questa funzione e' parita'-1:1 col Python su due forme:
    /// - `Value::Array` (caso atteso): primi 3 elementi; le stringhe restano tali
    ///   (`str("a") == "a"`, niente quote), gli altri tipi -> forma JSON compatta.
    /// - `Value::String` (output LLM malformato): in Python una stringa e' un
    ///   iterabile di CARATTERI, quindi `[str(c) for c in "abcde"[:3]]` ->
    ///   `["a","b","c"]`. Replicato qui iterando i primi 3 char.
    ///
    /// APPROSSIMAZIONE A IMPATTO-ZERO (voluta): per `Value::Object` e gli scalari
    /// non-stringa (numero/bool) NON si replica `str(dict)`/`str(True)` di Python
    /// (semantica complessa e fragile, es. `str(True)=="True"`, `str({...})` col
    /// repr dict). Questi tipi NON entrano nel calcolo dello score (le liste non
    /// pesano): la divergenza riguarda solo il campo salvato in un caso degenere,
    /// quindi si lascia il comportamento attuale (vuoto per object, forma JSON
    /// per scalari dentro un array).
    fn stringify_cap3(value: Option<&Value>) -> Vec<String> {
        match value {
            Some(Value::Array(arr)) => arr
                .iter()
                .take(3)
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect(),
            // Stringa malformata: itera i primi 3 CARATTERI (parita' col Python,
            // dove una str e' un iterabile di char).
            Some(Value::String(s)) => s.chars().take(3).map(|c| c.to_string()).collect(),
            // Object/scalari/assente: vuoto (approssimazione a impatto-zero, vedi doc).
            _ => Vec::new(),
        }
    }

    /// Legge `iteration_budget` dallo stato (campo non-nativo in `extra`):
    /// `int(state.get("iteration_budget") or 0)` (`__init__.py:4357`). Non-numero
    /// / assente -> 0 (il reward usera' poi `MAX_AGENT_ITERATIONS` come floor).
    fn iteration_budget(state: &AgentState) -> i64 {
        state
            .extra
            .get("iteration_budget")
            .and_then(Value::as_i64)
            .unwrap_or(0)
    }

    /// Stringa snake_case dello `stop_reason` per il confronto `== "error"` del
    /// reward (`__init__.py:4277`: `state.get("stop_reason") or "end_turn"`).
    /// L'enum Rust `StopReason::Error` serializza in `"error"` (parita' col
    /// Python, vedi `state/mod.rs`): quando l'executor fallisce per errore
    /// provider lo stato porta `stop_reason="error"` + `result` non vuoto, e il
    /// punto unico `heuristic_reward` entra nel ramo 0.0 ANCHE dal solo stato
    /// Rust (ramo prima irraggiungibile, ora chiuso). Vedi golden `error_*`.
    fn stop_reason_str(state: &AgentState) -> String {
        match state.stop_reason {
            Some(sr) => serde_json::to_value(sr)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "end_turn".to_string()),
            None => "end_turn".to_string(),
        }
    }
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for ReflectionNode {
    fn id(&self) -> NodeId {
        NodeId::Reflection
    }

    async fn run(&self, state: &AgentState, ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        // ── Gate 1: flag globale OFF (__init__.py:4258-4260) ──────────────────
        if !self.cfg.enabled {
            return Ok(Self::pass_through());
        }

        // ── Gate 2: prompt senza tag <reflection> (__init__.py:4263-4266) ─────
        let system_text = state.system_text.as_deref().unwrap_or("");
        if !system_text.contains("<reflection>") {
            return Ok(Self::pass_through());
        }

        // ── Gate 3: sampling probabilistico (__init__.py:4269-4273) ───────────
        // Il roll RNG e' generato qui; la decisione e' nella funzione pura.
        let roll: f64 = rand::random::<f64>();
        if Self::should_sample_skip(roll, self.cfg.sample_rate) {
            tracing::debug!(
                target: "nexus_agent_graph::reflection",
                rate = self.cfg.sample_rate,
                "reflection: escluso per sampling"
            );
            return Ok(Self::pass_through());
        }

        // ── Raccolta dati operativi dallo stato (__init__.py:4276-4301) ───────
        let result = state.result.clone().unwrap_or_default();
        let iterations = state.iterations.unwrap_or(0);
        let iteration_budget = Self::iteration_budget(state);
        let stop_reason = Self::stop_reason_str(state);
        let task_input = Self::task_input(&state.messages);

        // ── Gate 4: result vuoto -> skip valutazione (__init__.py:4303-4305) ──
        if result.is_empty() {
            return Ok(Self::pass_through());
        }

        // ── Chiamata LLM reflection (best-effort, I/O dietro ctx.llm) ─────────
        // provider/model RISOLTI A MONTE (regola G): arrivano dalla config. Una
        // sola chiamata, max_tokens=400 / temperature=0.0 (li applica il
        // chiamante concreto in base al provider). Timeout/errore -> None,
        // prosegue col solo euristico.
        let reflection_data: Option<ReflectionData> = if self.cfg.model.is_empty() {
            // model vuoto = reflection disabilitata (safe-default DB, regola G).
            None
        } else {
            self.call_evaluator(ctx, &task_input, &result).await
        };

        // ── Reward euristico (PUNTO UNICO decisions::reward, regola L) ────────
        let heuristic = heuristic_reward(&stop_reason, !result.is_empty(), iterations, iteration_budget);

        // ── Fusione final_reward (__init__.py:4367-4381) ─────────────────────
        // reflection_data None -> solo euristico, final_reward = None.
        let (reflection_score, final_reward): (Option<f64>, Option<f64>) = match &reflection_data {
            Some(rd) => {
                let fr = fuse_final_reward(heuristic, rd.score, self.cfg.reward_weight);
                tracing::info!(
                    target: "nexus_agent_graph::reflection",
                    score = rd.score,
                    heuristic,
                    final_reward = fr,
                    "reflection valutata"
                );
                (Some(rd.score), Some(fr))
            }
            None => (None, None),
        };

        // ── Persistenza best-effort (gated su shadow) + reasoning_bank TODO ───
        // GATING OBBLIGATORIO: in shadow NON scrive (zero side-effect).
        if let Some(rd) = &reflection_data {
            if !ctx.shadow {
                self.spawn_persist(ctx, state, rd);
            }
            // TODO porting: reasoning_bank dietro porta dedicata.
            // (`maybe_store_reflection_example`, __init__.py:4404-4415): bridge
            // verso reasoning_bank quando score>=reasoning_bank_min_score &&
            // suggestions presenti. NON portato in questo PR (embedding pesante,
            // porta dedicata assente). Gated su shadow quando verra' stubbato.
            let _bank_eligible = reflection_score
                .map(|s| s >= self.cfg.reasoning_bank_min_score)
                .unwrap_or(false)
                && !rd.suggestions.is_empty();
        }

        // ── Delta finale (__init__.py:4417-4423) ──────────────────────────────
        let dimensions = reflection_data.as_ref().map(|rd| rd.dimensions_json());
        let weaknesses = reflection_data
            .as_ref()
            .map(|rd| rd.weaknesses.iter().map(|w| json!(w)).collect::<Vec<_>>());
        let suggestions = reflection_data
            .as_ref()
            .map(|rd| rd.suggestions.iter().map(|s| json!(s)).collect::<Vec<_>>());

        Ok(StateDelta {
            reflection_score: Some(reflection_score),
            reflection_dimensions: Some(dimensions),
            reflection_weaknesses: Some(weaknesses),
            reflection_suggestions: Some(suggestions),
            final_reward: Some(final_reward),
            ..Default::default()
        }
        .into_opaque())
    }
}

impl ReflectionNode {
    /// Delta pass-through `{reflection_score:None, final_reward:None}`
    /// (`__init__.py:4260` ecc.): i due campi sono settati esplicitamente a None
    /// (overwrite con `Some(None)`, semantica double_option), gli altri restano
    /// invariati. Replica il dict di skip del Python.
    fn pass_through() -> OpaqueDelta {
        StateDelta {
            reflection_score: Some(None),
            final_reward: Some(None),
            ..Default::default()
        }
        .into_opaque()
    }

    /// Chiamata al modello valutatore (I/O dietro `ctx.llm`). Mappa il prompt
    /// single-string `sys + "\n\n" + user` (`__init__.py:4325`) sulla forma
    /// `messages` del gateway: system->role system, user->role user (parita' con
    /// gli altri nodi gia' portati). Best-effort: errore -> None.
    ///
    /// PENDENZA NOTA (LLM-shadow, da chiudere con l'integrazione del gateway
    /// concreto, Fase 3 PR2): a differenza di `ToolExecutor`, il trait
    /// `LlmGateway::complete` (`runtime/ports.rs`) NON ha un `ExecMode`, quindi
    /// in modalita' shadow questa chiamata NON e' gated e spenderebbe token
    /// reali. OGGI e' latente perche' nessun gateway concreto e' cablato (i test
    /// usano double scriptati) ed e' allineata al Python, che a sua volta NON
    /// gata l'LLM in shadow. Il fix architetturale (ExecMode/Replay su
    /// `LlmGateway`) appartiene all'integrazione del gateway concreto e NON tocca
    /// il contratto `ports.rs` in questo PR. Vedi anche il gating shadow gia'
    /// presente sulla PERSISTENZA (`run`: `if !ctx.shadow`), che e' un side-effect
    /// distinto e gia' coperto.
    async fn call_evaluator(
        &self,
        ctx: &AgentNodeCtx,
        task_input: &str,
        result: &str,
    ) -> Option<ReflectionData> {
        use crate::runtime::ports::{LlmMessage, LlmRequest};

        let (sys_prompt, user_prompt) =
            Self::build_reflection_prompt(&self.cfg, task_input, result);

        let req = LlmRequest {
            // provider/model RISOLTI A MONTE dal chiamante (regola G).
            provider: self.cfg.provider.clone(),
            model: self.cfg.model.clone(),
            messages: vec![
                LlmMessage {
                    role: "system".to_string(),
                    content: Value::String(sys_prompt),
                },
                LlmMessage {
                    role: "user".to_string(),
                    content: Value::String(user_prompt),
                },
            ],
            // Nessun tool: la reflection ritorna JSON testuale.
            tools: None,
        };

        match ctx.llm.complete(req).await {
            Ok(resp) => {
                let parsed = Self::parse_reflection(&resp.content);
                if parsed.is_none() {
                    tracing::warn!(
                        target: "nexus_agent_graph::reflection",
                        "reflection: parsing JSON fallito (best-effort, solo euristico)"
                    );
                }
                parsed
            }
            Err(err) => {
                // Timeout/errore provider -> None, continua col solo euristico
                // (__init__.py:4342-4347).
                tracing::warn!(
                    target: "nexus_agent_graph::reflection",
                    error = %err,
                    "reflection: chiamata LLM fallita (best-effort, solo euristico)"
                );
                None
            }
        }
    }

    /// Persiste la reflection in `nexus_agent_reflections` come task best-effort
    /// fire-and-forget (`_persist_reflection`, __init__.py:4426-4461). Delega a
    /// `ctx.db`. Solo se `prompt_key` e' valorizzato (__init__.py:4384): il
    /// `prompt_key` deriva dal profilo (I/O profile_loader) — finche' quella
    /// porta non esiste lo deriviamo dal `profile_name` dello stato come chiave
    /// di tracciamento; assente -> niente persistenza (parita' col gate Python).
    ///
    /// GATING SHADOW: chiamata SOLO quando `!ctx.shadow` (il gate e' nel
    /// chiamante `run`): in shadow zero scritture.
    fn spawn_persist(&self, ctx: &AgentNodeCtx, state: &AgentState, rd: &ReflectionData) {
        // prompt_key: il brain lo prende dal profilo (prof.prompt_key). La porta
        // profile_loader non c'e' ancora -> usiamo il profile_name come chiave di
        // tracciamento; assente -> niente persistenza (come il Python con
        // prompt_key vuoto, __init__.py:4384).
        let Some(prompt_key) = state
            .profile_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
        else {
            return;
        };
        // run_id: il thread_id dello stato o il run del ctx (entrambi = run Nexus).
        let run_id = state
            .thread_id
            .clone()
            .unwrap_or_else(|| ctx.run_id.to_string());
        let pool = ctx.db.clone();
        let dimensions = rd.dimensions_json();
        let score = rd.score;
        let weaknesses = rd.weaknesses.clone();
        let suggestions = rd.suggestions.clone();
        let model_used = self.cfg.model.clone();

        // Fire-and-forget: niente await nel nodo, errore loggato come WARN.
        tokio::spawn(async move {
            let res = sqlx::query(
                "INSERT INTO nexus_agent_reflections \
                 (run_id, prompt_key, prompt_version, score, dimensions, \
                  weaknesses, suggestions, model_used, latency_ms) \
                 VALUES ($1::uuid, $2, $3, $4, $5::jsonb, $6, $7, $8, $9)",
            )
            .bind(&run_id)
            .bind(&prompt_key)
            // prompt_version = 1 (default, __init__.py:4284).
            .bind(1_i32)
            .bind(score)
            .bind(dimensions)
            .bind(&weaknesses)
            .bind(&suggestions)
            .bind(&model_used)
            // latency_ms: misurata nel chiamante concreto; 0 finche' la porta non
            // espone la latenza dell'LlmGateway (best-effort, non load-bearing).
            .bind(0_i32)
            .execute(&pool)
            .await;
            if let Err(err) = res {
                tracing::warn!(
                    target: "nexus_agent_graph::reflection",
                    error = %err,
                    "reflection: persistenza fallita (best-effort)"
                );
            }
        });
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
    };
    use crate::runtime::test_doubles::StubToolExecutor;
    use crate::runtime::AgentNodeCtx;
    use crate::state::{AgentState, Message, MessageContent};

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

    /// LLM scriptato: ritorna un content testuale fisso (la "risposta" del
    /// valutatore). Registra le richieste.
    struct ScriptedLlm {
        content: String,
        seen: std::sync::Mutex<Vec<LlmRequest>>,
    }
    impl ScriptedLlm {
        fn with_content(content: &str) -> Self {
            Self {
                content: content.to_string(),
                seen: std::sync::Mutex::new(vec![]),
            }
        }
    }
    #[async_trait]
    impl LlmGateway for ScriptedLlm {
        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, PortError> {
            self.seen.lock().unwrap().push(req);
            Ok(LlmResponse {
                content: self.content.clone(),
                tool_calls: vec![],
                usage: LlmUsage::default(),
            })
        }
    }

    /// LLM che fallisce sempre (path solo-euristico su errore LLM).
    struct FailingLlm;
    #[async_trait]
    impl LlmGateway for FailingLlm {
        async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, PortError> {
            Err(PortError::Llm("simulato".to_string()))
        }
    }

    struct Sink;
    impl EventSink for Sink {
        fn emit(&self, _ev: SseEvent) {}
    }

    /// Ctx di test con LLM iniettabile; PgPool lazy (nessuna query DB reale: i
    /// test che NON innescano persistenza non toccano il DB; quelli con
    /// persistenza la spawnano fire-and-forget e non attendono l'esito).
    fn ctx_with(llm: Arc<dyn LlmGateway>, shadow: bool) -> AgentNodeCtx {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy non si connette");
        AgentNodeCtx {
            db: pool,
            llm,
            tools: Arc::new(StubToolExecutor::with_success(json!("ok"))),
            emit: Arc::new(Sink),
            cfg: crate::routing::config::RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            shadow,
        }
    }

    /// Config che FORZA il campionamento (sample_rate=1.0 -> nessun roll esclude)
    /// e fornisce un model non vuoto (chiama l'LLM). Provider/template ai default.
    fn cfg_always() -> ReflectionConfig {
        ReflectionConfig {
            sample_rate: 1.0,
            model: "modello-valutatore".to_string(),
            provider: "provider-x".to_string(),
            ..Default::default()
        }
    }

    /// Stato che supera i gate 1-4: system con <reflection>, result presente,
    /// un HumanMessage iniziale.
    fn passing_state() -> AgentState {
        AgentState {
            messages: vec![human("implementa la cache")],
            system_text: Some("...<reflection>...".to_string()),
            result: Some("ho implementato la cache con Redis".to_string()),
            iterations: Some(3),
            stop_reason: Some(crate::state::StopReason::EndTurn),
            ..Default::default()
        }
    }

    const VALID_JSON: &str = r#"{"score": 0.8, "dimensions": {"correctness": 0.9, "completeness": 0.8, "efficiency": 0.7, "safety": 1.0}, "weaknesses": ["w1", "w2"], "suggestions": ["s1"]}"#;

    // ── Gate (skip) ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn gate_flag_off_passthrough() {
        let cfg = ReflectionConfig {
            enabled: false,
            ..cfg_always()
        };
        let node = ReflectionNode::new(cfg);
        let llm = Arc::new(ScriptedLlm::with_content(VALID_JSON));
        let ctx = ctx_with(llm.clone(), false);
        let st = passing_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.reflection_score, None);
        assert_eq!(out.final_reward, None);
        assert!(llm.seen.lock().unwrap().is_empty(), "no LLM su flag OFF");
    }

    #[tokio::test]
    async fn gate_no_reflection_tag_passthrough() {
        let node = ReflectionNode::new(cfg_always());
        let llm = Arc::new(ScriptedLlm::with_content(VALID_JSON));
        let ctx = ctx_with(llm.clone(), false);
        let mut st = passing_state();
        st.system_text = Some("prompt senza tag".to_string());
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.reflection_score, None);
        assert_eq!(out.final_reward, None);
        assert!(llm.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn gate_sampling_escluso_passthrough() {
        // sample_rate=0.0: qualsiasi roll in [0,1) e' > 0.0 -> sempre escluso.
        let cfg = ReflectionConfig {
            sample_rate: 0.0,
            ..cfg_always()
        };
        let node = ReflectionNode::new(cfg);
        let llm = Arc::new(ScriptedLlm::with_content(VALID_JSON));
        let ctx = ctx_with(llm.clone(), false);
        let st = passing_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.reflection_score, None);
        assert_eq!(out.final_reward, None);
        assert!(llm.seen.lock().unwrap().is_empty(), "no LLM su sampling escluso");
    }

    #[tokio::test]
    async fn gate_result_vuoto_passthrough() {
        let node = ReflectionNode::new(cfg_always());
        let llm = Arc::new(ScriptedLlm::with_content(VALID_JSON));
        let ctx = ctx_with(llm.clone(), false);
        let mut st = passing_state();
        st.result = Some(String::new());
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.reflection_score, None);
        assert_eq!(out.final_reward, None);
        assert!(llm.seen.lock().unwrap().is_empty());
    }

    // ── Happy path ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn happy_path_delta_completo() {
        let node = ReflectionNode::new(cfg_always());
        let llm = Arc::new(ScriptedLlm::with_content(VALID_JSON));
        let ctx = ctx_with(llm.clone(), false);
        let st = passing_state(); // iterations 3, end_turn, result presente -> heuristic 1.0
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));

        assert_eq!(out.reflection_score, Some(0.8));
        // final = round(0.7*1.0 + 0.3*0.8, 4) = 0.94.
        let fr = out.final_reward.expect("final_reward presente");
        assert!((fr - 0.94).abs() < 1e-9, "atteso 0.94, ottenuto {fr}");
        let dims = out.reflection_dimensions.expect("dimensions");
        assert_eq!(dims["correctness"], json!(0.9));
        let weak = out.reflection_weaknesses.expect("weaknesses");
        assert_eq!(weak.len(), 2);
        // L'LLM e' stato chiamato una sola volta.
        assert_eq!(llm.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn llm_fallito_solo_euristico() {
        let node = ReflectionNode::new(cfg_always());
        let ctx = ctx_with(Arc::new(FailingLlm), false);
        let st = passing_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        // reflection_data None -> score None, final_reward None (solo euristico).
        assert_eq!(out.reflection_score, None);
        assert_eq!(out.final_reward, None);
    }

    #[tokio::test]
    async fn parsing_fallito_solo_euristico() {
        let node = ReflectionNode::new(cfg_always());
        let llm = Arc::new(ScriptedLlm::with_content("non e' json"));
        let ctx = ctx_with(llm, false);
        let st = passing_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.reflection_score, None);
        assert_eq!(out.final_reward, None);
    }

    /// Shadow: la persistenza NON deve partire (gate `!ctx.shadow`). Verifichiamo
    /// che il run non vada in errore (la query DB su pool lazy fallirebbe se
    /// spawnata, ma e' fire-and-forget e in shadow non si spawna affatto).
    #[tokio::test]
    async fn shadow_non_persiste() {
        let node = ReflectionNode::new(cfg_always());
        let llm = Arc::new(ScriptedLlm::with_content(VALID_JSON));
        let ctx = ctx_with(llm, true); // shadow
        let mut st = passing_state();
        st.profile_name = Some("core".to_string()); // prompt_key non vuoto
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        // Il delta e' comunque completo (la valutazione gira; solo la scrittura
        // e' soppressa).
        assert_eq!(out.reflection_score, Some(0.8));
    }

    // ── Funzioni deterministiche unitarie ──────────────────────────────────────

    #[test]
    fn should_sample_skip_boundary() {
        assert!(!ReflectionNode::should_sample_skip(0.3, 0.3), "roll == rate -> NON skip");
        assert!(ReflectionNode::should_sample_skip(0.31, 0.3), "roll > rate -> skip");
        assert!(!ReflectionNode::should_sample_skip(0.0, 1.0));
        assert!(ReflectionNode::should_sample_skip(0.5, 0.0));
    }

    #[test]
    fn rubrica_dettaglio_pesi_percentuali() {
        let d = ReflectionNode::rubrica_dettaglio();
        assert!(d.contains("- correctness (peso 40%):"));
        assert!(d.contains("- completeness (peso 30%):"));
        assert!(d.contains("- efficiency (peso 15%):"));
        assert!(d.contains("- safety (peso 15%):"));
    }

    #[test]
    fn render_template_sostituisce_e_deescapa() {
        let out = ReflectionNode::render_user_template(
            "T={task} O={output} R={rubrica_dettaglio} {{lett}}",
            "tsk",
            "out",
            "rub",
        );
        assert_eq!(out, "T=tsk O=out R=rub {lett}");
    }

    #[test]
    fn build_prompt_placeholder_e_troncamento() {
        let cfg = ReflectionConfig::default();
        // task vuoto -> placeholder; output lungo troncato a 3000.
        let lungo = "x".repeat(5000);
        let (sys, user) = ReflectionNode::build_reflection_prompt(&cfg, "", &lungo);
        assert!(sys.contains("valutatore"));
        assert!(user.contains("(nessun input)"));
        // l'output nel user contiene esattamente 3000 'x' consecutive.
        assert!(user.contains(&"x".repeat(3000)));
        assert!(!user.contains(&"x".repeat(3001)));
    }

    #[test]
    fn validate_in_range_round3() {
        // Parita' Python: round(0.8765,3)==0.876 e round(0.5005,3)==0.5 (i tie
        // "esatti" non lo sono in f64: 0.8765 e' rappresentato poco sotto).
        let v = json!({"score": 0.8765, "dimensions": {"correctness": 0.5005, "completeness": 0.3, "efficiency": 0.2, "safety": 0.1}});
        let rd = ReflectionNode::validate(&v).expect("valido");
        assert_eq!(rd.score, 0.876, "round a 3 decimali come Python");
        assert_eq!(rd.dimensions["correctness"], 0.5);
    }

    #[test]
    fn validate_fuori_range_none() {
        // score > 1 -> None.
        assert!(ReflectionNode::validate(&json!({"score": 1.5, "dimensions": {}})).is_none());
        // dimensione fuori range -> None.
        let v = json!({"score": 0.5, "dimensions": {"correctness": 2.0, "completeness": 0.5, "efficiency": 0.5, "safety": 0.5}});
        assert!(ReflectionNode::validate(&v).is_none());
        // dimensione assente (default -1) -> None.
        let v2 = json!({"score": 0.5, "dimensions": {"correctness": 0.5}});
        assert!(ReflectionNode::validate(&v2).is_none());
    }

    #[test]
    fn parse_due_tentativi() {
        // JSON puro.
        assert!(ReflectionNode::parse_reflection(VALID_JSON).is_some());
        // JSON con testo circostante -> tentativo 2 (regex).
        let wrapped = format!("Ecco la valutazione:\n{VALID_JSON}\nFine.");
        assert!(ReflectionNode::parse_reflection(&wrapped).is_some());
        // vuoto -> None.
        assert!(ReflectionNode::parse_reflection("").is_none());
        // non parsabile -> None.
        assert!(ReflectionNode::parse_reflection("blah blah").is_none());
    }

    #[test]
    fn weaknesses_suggestions_cap3() {
        let v = json!({
            "score": 0.5,
            "dimensions": {"correctness": 0.5, "completeness": 0.5, "efficiency": 0.5, "safety": 0.5},
            "weaknesses": ["a", "b", "c", "d", "e"],
            "suggestions": ["x"]
        });
        let rd = ReflectionNode::validate(&v).expect("valido");
        assert_eq!(rd.weaknesses, vec!["a", "b", "c"], "cap a 3");
        assert_eq!(rd.suggestions, vec!["x"]);
    }

    /// FIX 2: weaknesses/suggestions come STRINGA (output LLM malformato). In
    /// Python `[str(c) for c in "abcde"[:3]]` -> ["a","b","c"]; Rust deve fare
    /// idem iterando i primi 3 char (parita'). L'impatto sullo score e' nullo
    /// (le liste non pesano), ma il campo salvato deve coincidere col Python.
    #[test]
    fn weaknesses_suggestions_stringa_primi3_char() {
        let v = json!({
            "score": 0.5,
            "dimensions": {"correctness": 0.5, "completeness": 0.5, "efficiency": 0.5, "safety": 0.5},
            "weaknesses": "abcde",
            "suggestions": "xy"
        });
        let rd = ReflectionNode::validate(&v).expect("valido");
        assert_eq!(rd.weaknesses, vec!["a", "b", "c"], "primi 3 char della stringa");
        assert_eq!(rd.suggestions, vec!["x", "y"], "stringa piu' corta di 3");
    }
}

#[cfg(test)]
mod golden {
    //! Golden-test di PARITA' 1:1 vs Python sulla logica DETERMINISTICA del nodo
    //! reflection + del punto unico reward. Lo script `/tmp/gen_golden_reflection.py`
    //! importa le funzioni reali dal brain (`reflection_rubric` + il reward
    //! euristico/finale) e salva `{case_id, function, input, output}` in
    //! `/tmp/golden_reflection.json`. Qui ricostruiamo l'input, chiamiamo la
    //! funzione Rust corrispondente e verifichiamo `output == golden Python`.
    //!
    //! `#[ignore]` perche' dipende dal file generato. Comando:
    //!   python3 /tmp/gen_golden_reflection.py
    //!   cargo test -p nexus-agent-graph golden_reflection_parita -- --ignored

    use serde::Deserialize;
    use serde_json::{json, Value};

    use super::{ReflectionConfig, ReflectionNode};
    use crate::decisions::reward::{aggregate_score, final_reward, heuristic_reward};

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        case_id: String,
        function: String,
        input: Value,
        output: Value,
    }

    /// Serializza un `Option<ReflectionData>` nella forma confrontabile col
    /// Python (`null` se None, altrimenti il dict {score, dimensions, weaknesses,
    /// suggestions}).
    fn reflection_to_json(rd: Option<super::ReflectionData>) -> Value {
        match rd {
            None => Value::Null,
            Some(rd) => {
                let dims: serde_json::Map<String, Value> = rd
                    .dimensions
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(v)))
                    .collect();
                json!({
                    "score": rd.score,
                    "dimensions": Value::Object(dims),
                    "weaknesses": rd.weaknesses,
                    "suggestions": rd.suggestions,
                })
            }
        }
    }

    #[test]
    #[ignore = "richiede /tmp/golden_reflection.json generato da gen_golden_reflection.py"]
    fn golden_reflection_parita() {
        let path = "/tmp/golden_reflection.json";
        let raw = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!("impossibile leggere {path}: {e}; genera con python3 /tmp/gen_golden_reflection.py")
        });
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(!cases.is_empty(), "golden vuoto");

        let cfg = ReflectionConfig::default();
        let mut checked = 0usize;
        for c in &cases {
            let got: Value = match c.function.as_str() {
                "rubrica_dettaglio" => Value::String(ReflectionNode::rubrica_dettaglio()),
                "build_reflection_prompt" => {
                    let task = c.input.get("task").and_then(Value::as_str).unwrap_or("");
                    let output = c.input.get("output").and_then(Value::as_str).unwrap_or("");
                    let (sys, user) =
                        ReflectionNode::build_reflection_prompt(&cfg, task, output);
                    json!({"system": sys, "user": user})
                }
                "parse_reflection_response" => {
                    let raw_in = c.input.get("raw").and_then(Value::as_str).unwrap_or("");
                    reflection_to_json(ReflectionNode::parse_reflection(raw_in))
                }
                "validate_reflection" => {
                    let data = c.input.get("data").expect("data");
                    reflection_to_json(ReflectionNode::validate(data))
                }
                "heuristic_reward" => {
                    let sr = c.input.get("stop_reason").and_then(Value::as_str).unwrap_or("");
                    let res = c.input.get("result_non_empty").and_then(Value::as_bool).unwrap_or(false);
                    let it = c.input.get("iterations").and_then(Value::as_i64).unwrap_or(0);
                    let bud = c.input.get("iteration_budget").and_then(Value::as_i64).unwrap_or(0);
                    json!(heuristic_reward(sr, res, it, bud))
                }
                "final_reward" => {
                    let h = c.input.get("heuristic").and_then(Value::as_f64).unwrap_or(0.0);
                    let s = c.input.get("reflection_score").and_then(Value::as_f64).unwrap_or(0.0);
                    let w = c.input.get("reward_weight").and_then(Value::as_f64).unwrap_or(0.0);
                    json!(final_reward(h, s, w))
                }
                "aggregate_score" => {
                    // input.dimensions = {nome: valore}; i pesi sono quelli della
                    // rubrica (correctness=0.40, completeness=0.30, efficiency=0.15, safety=0.15).
                    let dims = c.input.get("dimensions").and_then(Value::as_object).expect("dims");
                    let pesi = [
                        ("correctness", 0.40),
                        ("completeness", 0.30),
                        ("efficiency", 0.15),
                        ("safety", 0.15),
                    ];
                    let pairs: Vec<(f64, f64)> = pesi
                        .iter()
                        .map(|(nome, peso)| {
                            let v = dims.get(*nome).and_then(Value::as_f64).unwrap_or(0.0);
                            (v, *peso)
                        })
                        .collect();
                    json!(aggregate_score(&pairs))
                }
                "should_sample_skip" => {
                    let roll = c.input.get("roll").and_then(Value::as_f64).unwrap_or(0.0);
                    let rate = c.input.get("sample_rate").and_then(Value::as_f64).unwrap_or(0.0);
                    json!(ReflectionNode::should_sample_skip(roll, rate))
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
        println!("golden reflection: {checked} casi verificati, tutti verdi");
    }
}
