//! `FinalGateNode` — porta la parte DETERMINISTICA del `final_gate_node`
//! (`brain/agents/final_gate.py:496-551`).
//!
//! Il final_gate e' il gate generale fail-closed per i task SOFTWARE che
//! chiudono SENZA plan_phase (executor diretto, end_turn): senza di esso
//! un'app placeholder montata sopra un design importato passerebbe in silenzio
//! (fail-open, incidente Beauty-Book 2026-06-11). E' un nodo DETERMINISTICO:
//! NON chiama l'LLM (a differenza di router/understanding/clarify). L'unico I/O
//! e' l'esecuzione dei criteri di verifica, delegata al sotto-sistema
//! [`CriteriaRunner`] (vedi sotto); la config DB-driven e' risolta a MONTE.
//!
//! ## Cosa porta QUESTO PR (deterministico, testato golden 1:1)
//!
//! - **La decision machine** (`final_gate.py:496-546`, [`FinalGateNode::run`]):
//!   gate `enabled`/`is_software_task` -> pass-through `{}`; ramo PASSED ->
//!   `{final_gate_cycle:0, stop_reason:end_turn, final_gate_passed:true}`; ramo
//!   FORCED (`forced_close_unverified` OR `cycle>=max_cycles`) ->
//!   `{final_gate_cycle:0, stop_reason:end_turn}` (NO `final_gate_passed`: resta
//!   FailedDiagnosed); ramo FAIL -> inietta `HumanMessage(_render_failed_block)`
//!   + `{messages:append, final_gate_cycle:cycle, stop_reason:tool_use,
//!   pending_tool_uses:[]}`.
//! - **`is_software_task`**: DELEGATO al PUNTO UNICO `signals::is_software_task`
//!   (regola L: e' identico a `_is_software_task` del Python e gia' usato dalle
//!   `route_after_*`; NON re-implementato qui). Esso valuta in OR il segnale
//!   STRUTTURALE `has_filesystem_mutation_in_history` (lista mutator-tools
//!   DB-driven dalla config) e la whitelist intent.
//! - **La costruzione delle spec criteri** (`final_gate.py:400-470`,
//!   [`FinalGateNode::build_criteria`]): `no_orphan_imported` (sempre),
//!   `outputs_exist` (sempre), `service_logs_clean` (se `runtime_check_enabled` +
//!   `log_command`), `run_command`-build (se `build_command` presente) e
//!   `http`-endpoint (se `endpoint_criterion` risolto a monte,
//!   `_resolve_endpoint_check`). Costruzione PURA.
//! - **`_count_build_errors` + `_BUILD_ERROR_PATTERNS`** (`final_gate.py:276-294`):
//!   regex TS/rustc/SyntaxError/TypeError/generico, conteggio indicativo. 1:1.
//! - **`_render_failed_block`** (`final_gate.py:396-493`,
//!   [`FinalGateNode::render_failed_block`]): testo `<final_gate_failed>` con
//!   excerpt per criterio, ramo speciale build (max_output_chars,
//!   output_truncated, header), direttive fail-closed, prefisso `<autonomy_hint>`
//!   se behavior_mode autonomo. Stringhe deterministiche 1:1.
//! - **`all_passed`** (`final_gate.py:392`): `all(r.passed)` sui risultati.
//!
//! ## Cosa NON porta (sotto-sistema delegato dietro porta + TODO espliciti)
//!
//! - **Esecuzione dei criteri** (`criteria_runner._check_*`,
//!   `brain/agents/criteria_runner.py`): SOTTO-SISTEMA separato (come
//!   `closure_judge` per il learner). Il nodo costruisce le [`CriterionSpec`] e
//!   ottiene i [`CriterionResult`] tramite il trait [`CriteriaRunner`]; la LOGICA
//!   dei singoli criteri (grafo import, esistenza file su disco, parsing log,
//!   esecuzione build) NON e' portata in questo PR -> TODO esplicito, porta
//!   dedicata da implementare con l'integrazione del ToolRunner gRPC. Per il
//!   GOLDEN i risultati sono INPUT stubati.
//! - **La risoluzione della config** (`_resolve_build_command`/
//!   `_resolve_log_command`/`_build_timeout_s`/`_build_output_max_chars` +
//!   `_project_slug`): tutta lettura DB (regola G) -> risolta A MONTE dal
//!   chiamante, passata nella [`FinalGateConfig`]. In particolare `_project_slug`
//!   NON e' portato qui (vive in mcp-core `project_workspace/logs.rs` e serve solo
//!   al risolutore della config; `nexus-agent-graph` non puo' dipendere da
//!   mcp-core, regola L).
//!
//! GATING SHADOW: in `ctx.shadow == true` l'esecuzione criteri usa
//! `ExecMode::Replay` (i criteri rileggono i tool_result del primario = zero
//! side-effect); il nodo NON emette eventi e NON scrive. Verificato nei test.
//!
//! Il nodo NON instrada: l'edge post-final_gate vive in `routing::route_after_final_gate`.

use std::sync::LazyLock;

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::routing::config::RoutingConfig;
use crate::routing::signals;
use crate::runtime::ports::{CriterionResult, CriterionSpec};
use crate::runtime::AgentNodeCtx;
use crate::state::{AgentState, Message, MessageContent, StateDelta, StopReason};

/// Pattern di errore di compilazione comuni (TypeScript, Rust, generici).
/// Replica 1:1 `_BUILD_ERROR_PATTERNS` (`final_gate.py:276-282`). Il conteggio
/// e' indicativo (best-effort): comunica all'agente la SCALA del problema, non
/// e' un parser esatto. Compilati una volta sola (`LazyLock`).
static BUILD_ERROR_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // tsc: `error TS\d+:` (IGNORECASE).
        Regex::new(r"(?i)error TS\d+:").expect("regex tsc valida"),
        // rustc: `\berror\[E\d+\]` (IGNORECASE).
        Regex::new(r"(?i)\berror\[E\d+\]").expect("regex rustc valida"),
        Regex::new(r"\bSyntaxError\b").expect("regex SyntaxError valida"),
        Regex::new(r"\bTypeError\b").expect("regex TypeError valida"),
        // generico cargo/cc: `^\s*error:\s` (MULTILINE).
        Regex::new(r"(?m)^\s*error:\s").expect("regex error generico valida"),
        // vite/rollup/esbuild: il bundler JS puo' USCIRE 0 anche quando il build
        // FALLISCE (config con exit-code bugiardo). Questi pattern rendono
        // count_build_errors > 0 -> il criterio build fallisce comunque (rete di
        // sicurezza in criteria_runner::check_run_command).
        Regex::new(r"(?i)could not resolve\b").expect("regex rollup resolve valida"),
        Regex::new(r"(?i)\berror during build\b").expect("regex vite build valida"),
        Regex::new(r"(?i)\bbuild failed\b").expect("regex vite failed valida"),
        Regex::new(r"\[plugin:").expect("regex vite plugin valida"),
    ]
});

/// Config DB-driven del nodo final_gate, PASSATA (regola G: nessuna lettura DB
/// nel nodo, nessun fallback hardcoded dentro la logica decisionale).
///
/// Mappa i settings risolti dal brain (`orchestrator_config.get()` +
/// `_resolve_build_command`/`_resolve_log_command`/`_build_timeout_s`/
/// `_build_output_max_chars`, `final_gate.py:78-271`). I comandi build/log sono
/// gia' RISOLTI per-progetto a monte (regola G): il nodo li riceve pronti, non
/// legge il DB ne' calcola lo slug.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FinalGateConfig {
    /// Final gate abilitato (`final_gate_enabled`, default true). OFF ->
    /// pass-through `{}` (`final_gate.py:505`).
    pub enabled: bool,
    /// Cap di cicli del final gate (`final_gate_max_cycles`, default 2). Al cap
    /// si chiude comunque (no loop infinito, `final_gate.py:532`).
    pub max_cycles: i64,
    /// Verifica runtime E2E dei log servizi abilitata
    /// (`final_gate_runtime_check_enabled`, default ON). Aggiunge il criterio
    /// `service_logs_clean` (`final_gate.py:342`).
    pub runtime_check_enabled: bool,
    /// Comando build risolto per-progetto (`_resolve_build_command`). `None` =
    /// build-check disabilitato o nessun comando risolvibile (N/A: niente
    /// criterio build, non blocca i progetti senza build).
    pub build_command: Option<String>,
    /// Working dir del comando build (`build_cwd`, `final_gate.py:361`). `None`
    /// = cwd di default del runner.
    pub build_working_dir: Option<String>,
    /// Timeout (s) del criterio build (`_build_timeout_s`, default 180: i build
    /// sono lenti). Risolto a monte.
    pub build_timeout_s: f64,
    /// Limite caratteri dell'output_excerpt del criterio build esposto
    /// all'agente quando fallisce (`_build_output_max_chars`, default 4000,
    /// clamp 1000-32000). Risolto a monte (gia' clampato dal chiamante).
    pub build_output_max_chars: i64,
    /// Comando log dei servizi risolto per-progetto (`_resolve_log_command`).
    /// Vuoto = nessun criterio `service_logs_clean` anche se runtime_check ON
    /// (`final_gate.py:344`).
    pub log_command: String,
    /// Pattern di errore runtime per `service_logs_clean`
    /// (`final_gate_runtime_error_patterns`). Vuoto ammesso.
    pub runtime_error_patterns: Vec<String>,
    /// Ratio minimo di moduli raggiunti per `no_orphan_imported`
    /// (`no_orphan_min_ratio`, default 0.4).
    pub no_orphan_min_ratio: f64,
    /// Directory di staging per `no_orphan_imported` (`import_staging_dirs`,
    /// default `["figma_export"]`).
    pub import_staging_dirs: Vec<String>,
    /// Timeout (s) generale dei criteri non-build (`verifier_timeout_s`, def 30):
    /// passato nel ctx dei criteri (`final_gate.py:313`).
    pub criteria_timeout_s: f64,
    /// Criterio ENDPOINT HTTP risolto per-progetto (`_resolve_endpoint_check`,
    /// `final_gate.py:234-302`). `None` = nessun endpoint configurato O check
    /// disabilitato (N/A: niente criterio, non blocca i progetti senza endpoint).
    /// Risolto A MONTE (regola G): la lettura DB di `run_configurations`
    /// (role='endpoint' + `http_spec`) e del setting
    /// `agent.final_gate.endpoint_check_enabled` resta fuori dal nodo, esattamente
    /// come `build_command`/`log_command`. La spec arriva pronta nella forma
    /// `{type:"http", spec:{url, method, body?, headers?}, expected:{status?,
    /// body_contains?}, timeout_s}` (vedi `_resolve_endpoint_check`).
    pub endpoint_criterion: Option<CriterionSpec>,
    /// P5: gate design_verify abilitato (agent.final_gate.design_verify_enabled,
    /// default true). Si applica SOLO se nella history c'e' un nexus_visual_compare
    /// (task figma): None = non-figma -> non blocca.
    pub design_verify_enabled: bool,
    /// P5: soglia minima di similarity_score (0-100) per chiudere un task figma
    /// (agent.final_gate.design_verify_min_score, default 70).
    pub design_verify_min_score: i64,
}

impl Default for FinalGateConfig {
    fn default() -> Self {
        // Default IDENTICI ai safe-default del brain (orchestrator_config.py +
        // i default delle funzioni `_resolve_*`). Valgono SOLO se il DB e'
        // irraggiungibile, mai come magic fallback nella logica.
        Self {
            enabled: true,
            max_cycles: 2,
            runtime_check_enabled: true,
            build_command: None,
            build_working_dir: None,
            build_timeout_s: 180.0,
            build_output_max_chars: 4000,
            log_command: String::new(),
            runtime_error_patterns: Vec::new(),
            no_orphan_min_ratio: 0.4,
            import_staging_dirs: vec!["figma_export".to_string()],
            criteria_timeout_s: 30.0,
            endpoint_criterion: None,
            design_verify_enabled: true,
            design_verify_min_score: 70,
        }
    }
}

/// Estrae una `&str` da un campo evidence con la semantica `or` FALSY di Python
/// (`final_gate.py:421`): ritorna `Some(s)` SOLO se il campo e' una stringa NON
/// vuota; una stringa vuota `""` (falsy in Python) o un tipo diverso/assente
/// ritornano `None`, cosi' il chiamante cade sul campo successivo della catena.
/// Senza questo, `Some("")` interromperebbe il fallback (divergenza dal Python).
fn str_truthy(v: Option<&Value>) -> Option<&str> {
    match v.and_then(Value::as_str) {
        Some(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

/// Estrae un `i64` da un campo evidence con la semantica `or` FALSY di Python
/// (`final_gate.py:435`: `output_total_chars or len(text)`): ritorna `Some(n)`
/// SOLO se il campo e' un intero NON-zero; lo `0` (falsy in Python), un tipo
/// diverso o l'assenza ritornano `None`, cosi' il chiamante cade sul default
/// (`len(text)`). Senza questo, `Some(0)` verrebbe mantenuto (divergenza).
fn i64_truthy(v: Option<&Value>) -> Option<i64> {
    match v.and_then(Value::as_i64) {
        Some(n) if n != 0 => Some(n),
        _ => None,
    }
}

/// Conta occorrenze grezze di errori in un output di build (TS/Rust/...).
/// Replica 1:1 `_count_build_errors` (`final_gate.py:285-294`): somma i match
/// di tutti i [`BUILD_ERROR_PATTERNS`]. Indicativo: comunica all'agente quanti
/// errori deve risolvere (non solo il primo). 0 se l'output e' vuoto o nessun
/// pattern matcha.
pub fn count_build_errors(output: &str) -> usize {
    if output.is_empty() {
        return 0;
    }
    BUILD_ERROR_PATTERNS
        .iter()
        .map(|pat| pat.find_iter(output).count())
        .sum()
}

/// Nodo final_gate. Stateless: legge lo stato + la config passata + la
/// `RoutingConfig` (per `is_software_task`/lista mutator). I criteri sono
/// eseguiti dietro il trait [`crate::runtime::ports::CriteriaRunner`], iniettato
/// nel costruttore (dipendenza propria del nodo, NON nel ctx: minimizza
/// l'impatto sugli altri nodi). La config DB-driven e' risolta A MONTE (regola
/// G); la decision machine + rendering e' interamente qui.
pub struct FinalGateNode {
    /// Config DB-driven del gate (regola G: passata, mai letta dal nodo).
    cfg: FinalGateConfig,
    /// Config di routing: serve a `signals::is_software_task` (lista
    /// mutator-tools DB-driven + whitelist intent), punto unico (regola L).
    routing_cfg: RoutingConfig,
    /// Motore criteri (sotto-sistema delegato). mcp-core lo implementera' col
    /// ToolRunner gRPC; nei test e' stubato.
    criteria: std::sync::Arc<dyn crate::runtime::ports::CriteriaRunner>,
    /// Persistenza dei meta-step di fase `final_gate` (narrazione live in chat:
    /// "Verifico il risultato" / esito). Pattern emit+persist, punto unico
    /// [`crate::nodes::emit_phase_meta`] (regola L).
    meta_steps: std::sync::Arc<dyn crate::runtime::ports::MetaStepStore>,
}

impl FinalGateNode {
    /// Costruisce il nodo con la config DB-driven gia' risolta dal chiamante e
    /// il motore criteri concreto (o stub nei test).
    pub fn new(
        cfg: FinalGateConfig,
        routing_cfg: RoutingConfig,
        criteria: std::sync::Arc<dyn crate::runtime::ports::CriteriaRunner>,
        meta_steps: std::sync::Arc<dyn crate::runtime::ports::MetaStepStore>,
    ) -> Self {
        Self {
            cfg,
            routing_cfg,
            criteria,
            meta_steps,
        }
    }

    /// Costruisce le spec dei criteri da eseguire (`final_gate.py:400-470`).
    /// PURA: nessun I/O. L'ordine e' load-bearing (riprodotto 1:1):
    /// `no_orphan_imported`, `outputs_exist`, poi opzionali `service_logs_clean`
    /// (se runtime_check_enabled + log_command non vuoto), `run_command`-build
    /// (se build_command presente) e infine `http`-endpoint (se
    /// `endpoint_criterion` risolto a monte, `final_gate.py:468-470`).
    ///
    /// COPERTURA: questo metodo e' verificato dagli unit test Rust
    /// (`build_criteria_ordine_e_opzionali`), NON dal golden cross-language. Il
    /// golden e' deliberatamente DB-free, ma il LATO Python di questa costruzione
    /// e' inline nella coroutine async `_run_final_gate_criteria` e i suoi pezzi
    /// opzionali derivano da `_resolve_log_command`/`_resolve_build_command`/
    /// `_build_timeout_s`/`_build_output_max_chars` — tutte funzioni che aprono
    /// connessioni DB (`brain/agents/final_gate.py:78,186,234,249`). Estrarre
    /// l'assemblaggio in una funzione pura lato Python (per chiamarlo dallo
    /// script golden senza I/O) significherebbe o modificare il brain, o replicare
    /// la risoluzione config nello script — un secondo punto di verita' che
    /// diverge dal Python reale (anti-pattern regola L/H). Qui la config arriva
    /// gia' RISOLTA in [`FinalGateConfig`] (regola G), quindi `build_criteria` e'
    /// puro assemblaggio della lista: l'unit test Rust ne copre ordine, opzionali
    /// e contenuto spec senza bisogno della parita' cross-language.
    pub fn build_criteria(&self, state: &AgentState) -> Vec<CriterionSpec> {
        let mut criteria: Vec<CriterionSpec> = Vec::new();

        // (1) no_orphan_imported (anti-placeholder), sempre.
        criteria.push(CriterionSpec {
            criterion_type: "no_orphan_imported".to_string(),
            spec: json!({
                "staging_dir": self.cfg.import_staging_dirs,
                "min_reached_ratio": self.cfg.no_orphan_min_ratio,
            }),
            expected: json!({ "mounted": true }),
            timeout_s: None,
        });

        // (2) outputs_exist (claim-vs-fatti), sempre. run_id = thread_id.
        let run_id = state.thread_id.clone().unwrap_or_default();
        criteria.push(CriterionSpec {
            criterion_type: "outputs_exist".to_string(),
            spec: json!({ "run_id": run_id }),
            expected: json!({}),
            timeout_s: None,
        });

        // (3) service_logs_clean (verifica runtime E2E), se abilitato e c'e' un
        //     comando log risolto a monte.
        if self.cfg.runtime_check_enabled && !self.cfg.log_command.is_empty() {
            criteria.push(CriterionSpec {
                criterion_type: "service_logs_clean".to_string(),
                spec: json!({
                    "command": self.cfg.log_command,
                    "patterns": self.cfg.runtime_error_patterns,
                }),
                expected: json!({}),
                timeout_s: None,
            });
        }

        // (4) run_command-build (il codice deve COMPILARE), se c'e' un build
        //     command risolto. max_output_chars dedicato (mig 0426) + timeout
        //     build dedicato.
        if let Some(build_cmd) = &self.cfg.build_command {
            let mut spec = serde_json::Map::new();
            spec.insert("command".to_string(), json!(build_cmd));
            spec.insert(
                "max_output_chars".to_string(),
                json!(self.cfg.build_output_max_chars),
            );
            if let Some(cwd) = &self.cfg.build_working_dir {
                spec.insert("working_dir".to_string(), json!(cwd));
            }
            criteria.push(CriterionSpec {
                criterion_type: "run_command".to_string(),
                spec: Value::Object(spec),
                expected: json!({ "exit_code": 0 }),
                timeout_s: Some(self.cfg.build_timeout_s),
            });
        }

        // (5) http-endpoint (chiamata REALE all'endpoint che il task doveva far
        //     funzionare), se risolto a monte (regola G: `_resolve_endpoint_check`
        //     resta fuori dal nodo). Ultimo nell'ordine (`final_gate.py:468-470`).
        //     Risolve "build verde ma login ancora 500" (incidente Beauty-Book).
        if let Some(endpoint_crit) = &self.cfg.endpoint_criterion {
            criteria.push(endpoint_crit.clone());
        }

        // (6) design_verify (P5): per i task figma l'agente non puo' chiudere con
        //     una resa visiva sotto soglia che HA GIA' misurato con nexus_visual_compare.
        //     Deterministico: prende l'ultimo similarity_score dalla history (niente
        //     vision nel gate). None (nessun confronto) = task non-figma -> non blocca.
        if self.cfg.design_verify_enabled {
            let mut last_score: Option<i64> = None;
            for m in &state.messages {
                if let Message::Tool { content, .. } = m {
                    if let Ok(v) =
                        serde_json::from_str::<Value>(content.flatten_text().trim())
                    {
                        if let Some(sc) =
                            v.get("similarity_score").and_then(Value::as_i64)
                        {
                            last_score = Some(sc);
                        }
                    }
                }
            }
            if let Some(score) = last_score {
                criteria.push(CriterionSpec {
                    criterion_type: "design_verify".to_string(),
                    spec: json!({
                        "similarity_score": score,
                        "min_score": self.cfg.design_verify_min_score,
                    }),
                    expected: json!({ "min_score": self.cfg.design_verify_min_score }),
                    timeout_s: None,
                });
            }
        }

        criteria
    }

    /// `all_passed` reduce (`final_gate.py:392`): tutti i criteri passati?
    pub fn all_passed(results: &[CriterionResult]) -> bool {
        results.iter().all(|r| r.passed)
    }

    /// Costruisce il testo del `HumanMessage` da iniettare quando il gate
    /// fallisce (`_render_failed_block`, `final_gate.py:396-493`). PURA: i
    /// risultati arrivano gia' calcolati; il `behavior_mode` viene dallo stato.
    /// Riproduce 1:1 il corpo `<final_gate_failed>`, il ramo speciale build
    /// (header + count + nota troncamento), le direttive fail-closed e il
    /// prefisso `<autonomy_hint>` per le modalita' autonome.
    pub fn render_failed_block(
        state: &AgentState,
        cycle: i64,
        max_cycles: i64,
        results: &[CriterionResult],
    ) -> String {
        // Corpo specifico per criterio fallito (aggregato, non testo fisso).
        let failed: Vec<&CriterionResult> = results.iter().filter(|r| !r.passed).collect();
        let mut body_parts: Vec<String> = Vec::new();
        // build_errors_count e' load-bearing OLTRE il loop (entra nelle direttive
        // se >0, final_gate.py:464). build_truncated invece e' usato SOLO nel
        // ramo build per l'header, quindi resta locale al loop.
        let mut build_errors_count: usize = 0;

        for r in &failed {
            let ev = &r.evidence;
            // excerpt = output_excerpt or verdict or error or "" (Python:421).
            // Semantica `or` FALSY: una STRINGA VUOTA "" e' falsy come None, quindi
            // cade sul campo successivo (non solo su None/tipo-sbagliato). Usiamo
            // `str_truthy` (Some solo se la stringa NON e' vuota) per replicarla
            // 1:1: con output_excerpt:"" + verdict valorizzato si rende il verdict.
            let excerpt = str_truthy(ev.get("output_excerpt"))
                .or_else(|| str_truthy(ev.get("verdict")))
                .or_else(|| str_truthy(ev.get("error")))
                .unwrap_or("");
            if excerpt.is_empty() {
                continue;
            }
            // is_build_run_cmd: run_command con exit_code e output_total_chars
            // presenti nell'evidence (`final_gate.py:424-428`).
            let is_build_run_cmd = r.criterion_type == "run_command"
                && !ev.get("exit_code").map(Value::is_null).unwrap_or(true)
                && !ev
                    .get("output_total_chars")
                    .map(Value::is_null)
                    .unwrap_or(true);

            if is_build_run_cmd {
                // L'excerpt e' gia' tagliato dal runner: lo passiamo intero.
                let text = excerpt.to_string();
                build_errors_count = count_build_errors(&text);
                let build_truncated = ev
                    .get("output_truncated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                // total_chars = int(output_total_chars or len(text)) (Python:435).
                // Semantica `or` FALSY: 0 e' falsy, quindi cade su len(text) come
                // l'assenza o un tipo non-intero. `i64_truthy` ritorna Some solo se
                // il valore e' un intero NON-zero; altrimenti len(text) (codepoint).
                let total_chars = i64_truthy(ev.get("output_total_chars"))
                    .unwrap_or_else(|| text.chars().count() as i64);
                let mut header_bits = vec![format!("[{}]", r.criterion_type)];
                if build_errors_count > 0 {
                    header_bits.push(format!("errori rilevati: {build_errors_count}"));
                }
                if build_truncated {
                    // len(text) Python = numero di char (str), non byte.
                    let text_len = text.chars().count();
                    header_bits.push(format!(
                        "output troncato ({text_len}/{total_chars} char): \
                         rilancia il build per leggere il resto"
                    ));
                }
                body_parts.push(format!("{}\n{}", header_bits.join(" "), text));
            } else {
                // Altri criteri: excerpt tagliato a 900 char (codepoint, come
                // `str(excerpt)[:900]` Python).
                let truncated: String = excerpt.chars().take(900).collect();
                body_parts.push(format!("[{}]\n{}", r.criterion_type, truncated));
            }
        }

        let detail = if body_parts.is_empty() {
            "Una verifica del gate e' fallita.".to_string()
        } else {
            body_parts.join("\n\n")
        };

        // Direttive fail-closed (`final_gate.py:454-470`). Se ci sono errori
        // build, la riga col conteggio si inserisce in posizione 1 (subito sotto
        // l'intestazione "DIRETTIVE").
        let mut directives_lines: Vec<String> = vec![
            "DIRETTIVE (fail-closed):".to_string(),
            "- Leggi TUTTO l'output qui sopra: ogni errore va corretto, non solo il primo."
                .to_string(),
            "- Correggi TUTTI gli errori in un solo giro quando possibile: edita ogni file"
                .to_string(),
            "  impattato (anche errori 'banali' tipo unused/type mismatch contano)."
                .to_string(),
            "- Se l'output e' troncato (vedi nota 'output troncato'), rilancia il comando di"
                .to_string(),
            "  build con run_command (o rileggi i file impattati) per vedere il resto."
                .to_string(),
            "- Lavora per CONVERGENZA: niente 'task completato' finche' il build non passa"
                .to_string(),
            "  al 100% (exit 0, zero errori). Riverifica sempre dopo le correzioni."
                .to_string(),
        ];
        if build_errors_count > 0 {
            directives_lines.insert(
                1,
                format!(
                    "- Numero di errori rilevati nel build: {build_errors_count}. \
                     Risolvili TUTTI prima del prossimo final_gate."
                ),
            );
        }
        let directives = directives_lines.join("\n");

        let mut body = format!(
            "<final_gate_failed cycle=\"{cycle}/{max_cycles}\">\n\
             Verifica pre-chiusura FALLITA. NON dichiarare il task completato finche'\n\
             non e' risolto e RIVERIFICATO esercitando il flusso reale.\n\n\
             {detail}\n\n\
             {directives}\n\
             </final_gate_failed>"
        );

        // Prefisso autonomy_hint per le modalita' autonome
        // (`final_gate.py:481-492`): behavior_mode trimmed+lowercased in
        // {automatic, automatico, continuous, continuo}.
        let behavior_mode = state
            .behavior_mode
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let is_autonomous = matches!(
            behavior_mode.as_str(),
            "automatic" | "automatico" | "continuous" | "continuo"
        );
        if is_autonomous {
            let autonomy_prefix = format!(
                "<autonomy_hint mode=\"{behavior_mode}\">\n\
                 L'utente ha selezionato la modalita' '{behavior_mode}': procedi\n\
                 AUTONOMAMENTE con l'integrazione. NON chiedere conferma, NON scrivere\n\
                 domande tipo 'Vuoi che lo faccia?' o 'Confermi?'. Esegui direttamente\n\
                 le modifiche necessarie usando i tool disponibili.\n\
                 </autonomy_hint>\n\n"
            );
            body = format!("{autonomy_prefix}{body}");
        }
        body
    }
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for FinalGateNode {
    fn id(&self) -> NodeId {
        NodeId::FinalGate
    }

    async fn run(&self, state: &AgentState, ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        // ── Gate enabled / is_software_task (final_gate.py:505-506) ───────────
        // enabled OFF o task non-software -> pass-through {} (flusso prosegue).
        // is_software_task: PUNTO UNICO signals::is_software_task (regola L).
        if !self.cfg.enabled || !signals::is_software_task(state, &self.routing_cfg) {
            return Ok(Self::pass_through());
        }

        // ── Cycle (final_gate.py:508-509) ─────────────────────────────────────
        let cycle = state.final_gate_cycle.unwrap_or(0) + 1;
        let max_cycles = self.cfg.max_cycles;

        // Narrazione live: la chat racconta che parte la verifica oggettiva.
        crate::nodes::emit_phase_meta(
            ctx.emit.as_ref(),
            self.meta_steps.as_ref(),
            ctx.exec_mode(),
            "final_gate",
            format!("Verifico il risultato (build/test, tentativo {cycle}/{max_cycles})"),
            serde_json::json!({"cycle": cycle, "max_cycles": max_cycles, "phase": "start"}),
        )
        .await;

        // ── Esecuzione criteri (sotto-sistema delegato) ───────────────────────
        // GATING SHADOW: ExecMode::Replay in shadow (zero side-effect), punto
        // unico ctx.exec_mode() (regola L). Un guasto infrastrutturale del runner
        // propaga NodeError; un fallimento di un singolo criterio e' mappato dal
        // concreto su CriterionResult{passed:false} (parita' col try/except
        // Python, final_gate.py:381-385) e NON propaga errore.
        let criteria = self.build_criteria(state);
        let results = self
            .criteria
            .run(criteria, ctx.exec_mode())
            .await
            .map_err(|e| NodeError::Failed {
                node: "final_gate",
                message: format!("esecuzione criteri fallita: {e}"),
            })?;

        let passed = Self::all_passed(&results);

        // ── Ramo PASSED (final_gate.py:513-522) ───────────────────────────────
        // Chiude con esito canonico CompletedVerified lato mcp-core.
        if passed {
            tracing::info!(
                target: "nexus_agent_graph::final_gate",
                cycle,
                "final_gate: passato -> chiusura"
            );
            crate::nodes::emit_phase_meta(
                ctx.emit.as_ref(),
                self.meta_steps.as_ref(),
                ctx.exec_mode(),
                "final_gate",
                "Verifica superata".to_string(),
                serde_json::json!({"cycle": cycle, "phase": "passed"}),
            )
            .await;
            return Ok(StateDelta {
                final_gate_cycle: Some(Some(0)),
                stop_reason: Some(Some(StopReason::EndTurn)),
                final_gate_passed: Some(Some(true)),
                ..Default::default()
            }
            .into_opaque());
        }

        // ── Ramo FORCED / CAP (final_gate.py:524-537) ─────────────────────────
        // Chiusura SENZA re-executor su forced_close_unverified (abort anti-loop:
        // re-eseguire duplicherebbe il messaggio finale) o cap raggiunto. NON
        // imposta final_gate_passed -> resta FailedDiagnosed.
        let forced_close = state.forced_close_unverified.unwrap_or(false);
        if forced_close || cycle >= max_cycles {
            tracing::warn!(
                target: "nexus_agent_graph::final_gate",
                forced_close,
                cycle,
                max_cycles,
                "final_gate: chiusura senza re-executor"
            );
            crate::nodes::emit_phase_meta(
                ctx.emit.as_ref(),
                self.meta_steps.as_ref(),
                ctx.exec_mode(),
                "final_gate",
                "Verifica non superata: chiudo (limite tentativi)".to_string(),
                serde_json::json!({"cycle": cycle, "phase": "forced_close", "forced": forced_close}),
            )
            .await;
            return Ok(StateDelta {
                final_gate_cycle: Some(Some(0)),
                stop_reason: Some(Some(StopReason::EndTurn)),
                ..Default::default()
            }
            .into_opaque());
        }

        // ── Ramo FAIL (final_gate.py:539-546) ─────────────────────────────────
        // Inietta il verdetto come HumanMessage e rimanda all'executor.
        tracing::info!(
            target: "nexus_agent_graph::final_gate",
            cycle,
            max_cycles,
            "final_gate: fallito -> re-executor"
        );
        crate::nodes::emit_phase_meta(
            ctx.emit.as_ref(),
            self.meta_steps.as_ref(),
            ctx.exec_mode(),
            "final_gate",
            format!("Verifica fallita: rimando in correzione ({cycle}/{max_cycles})"),
            serde_json::json!({"cycle": cycle, "max_cycles": max_cycles, "phase": "failed"}),
        )
        .await;
        let block = Self::render_failed_block(state, cycle, max_cycles, &results);
        let hm = Message::Human {
            content: MessageContent::text(block),
        };
        Ok(StateDelta {
            messages: Some(vec![hm]),
            final_gate_cycle: Some(Some(cycle)),
            stop_reason: Some(Some(StopReason::ToolUse)),
            // pending_tool_uses azzerato a lista vuota (durata 1 turno):
            // Some(Some(vec![])) e' AZZERA, distinto da None (no-op).
            pending_tool_uses: Some(Some(vec![])),
            ..Default::default()
        }
        .into_opaque())
    }
}

impl FinalGateNode {
    /// Delta pass-through `{}` (`final_gate.py:506`): nessun campo modificato
    /// (delta vuoto), il flusso prosegue. Distinto dai pass-through di
    /// reflection (che azzerano due campi a `Some(None)`): qui il Python ritorna
    /// `{}` letterale, quindi NESSUNA chiave nel delta.
    fn pass_through() -> OpaqueDelta {
        StateDelta::default().into_opaque()
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
    use crate::runtime::ports::ExecMode;
    use crate::runtime::test_doubles::{NullEventSink, StubCriteriaRunner, StubLlmGateway, StubMetaStepStore, StubToolExecutor};
    use crate::runtime::AgentNodeCtx;
    use crate::state::{ContentBlock, Message, MessageContent};

    fn apply(base: AgentState, delta: nexus_graph::StateDelta) -> AgentState {
        let mut s = base;
        s.merge(delta);
        s
    }

    fn ok_result(t: &str) -> CriterionResult {
        CriterionResult {
            criterion_type: t.to_string(),
            passed: true,
            evidence: json!({}),
        }
    }

    fn fail_result(t: &str, evidence: Value) -> CriterionResult {
        CriterionResult {
            criterion_type: t.to_string(),
            passed: false,
            evidence,
        }
    }

    /// Ctx di test con flag shadow. Il motore criteri NON e' nel ctx: vive nel
    /// nodo (`FinalGateNode::new`), quindi qui basta lo shadow per derivare la
    /// `ExecMode`. PgPool lazy (il final_gate non scrive DB), LLM stub mai
    /// chiamato (nodo deterministico).
    fn ctx_with(shadow: bool) -> AgentNodeCtx {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy non si connette");
        AgentNodeCtx {
            db: pool,
            llm: Arc::new(StubLlmGateway::with_text("non usato")),
            tools: Arc::new(StubToolExecutor::with_success(json!("ok"))),
            emit: Arc::new(NullEventSink),
            cfg: RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            shadow,
        }
    }

    fn node_with(
        cfg: FinalGateConfig,
        criteria: Arc<dyn crate::runtime::ports::CriteriaRunner>,
    ) -> FinalGateNode {
        FinalGateNode::new(cfg, RoutingConfig::default(), criteria, std::sync::Arc::new(StubMetaStepStore::default()))
    }

    /// Messaggio AI con un tool_use mutativo: rende il task "software"
    /// strutturalmente (write_file e' in fs_mutator_tools), cosi' il gate non
    /// pass-through-a per non-software.
    fn ai_mutation() -> Message {
        Message::Ai {
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: "c1".into(),
                name: "write_file".into(),
                input: json!({"path": "src/main.tsx"}),
            }]),
            tool_calls: vec![],
            reasoning: None,
        }
    }

    /// Stato software (mutazione fs in history) che entra nel gate.
    fn software_state() -> AgentState {
        AgentState {
            messages: vec![ai_mutation()],
            thread_id: Some("11111111-1111-1111-1111-111111111111".to_string()),
            ..Default::default()
        }
    }

    // ── Gate pass-through ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn gate_enabled_off_passthrough() {
        let cfg = FinalGateConfig {
            enabled: false,
            ..Default::default()
        };
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![ok_result(
            "no_orphan_imported",
        )]));
        let node = node_with(cfg, runner.clone());
        let ctx = ctx_with(false);
        let st = software_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        // Pass-through: nessun cambio di stop_reason / final_gate_passed.
        assert_eq!(out.stop_reason, None);
        assert_eq!(out.final_gate_passed, None);
        // Criteri NON eseguiti (gate prima dei criteri).
        assert!(runner.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn gate_non_software_passthrough() {
        // Nessuna mutazione fs + intent non-software -> non software -> {}.
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![ok_result(
            "no_orphan_imported",
        )]));
        let node = node_with(FinalGateConfig::default(), runner.clone());
        let ctx = ctx_with(false);
        let st = AgentState {
            user_intent: Some("chat".into()),
            ..Default::default()
        };
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, None);
        assert!(runner.seen.lock().unwrap().is_empty());
    }

    // ── Ramo PASSED ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn passed_chiude_con_final_gate_passed() {
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![
            ok_result("no_orphan_imported"),
            ok_result("outputs_exist"),
        ]));
        let node = node_with(FinalGateConfig::default(), runner.clone());
        let ctx = ctx_with(false);
        let st = software_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.final_gate_passed, Some(true));
        assert_eq!(out.final_gate_cycle, Some(0));
        // Criteri eseguiti in Real (non shadow).
        let seen = runner.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1, ExecMode::Real);
    }

    // ── Ramo FORCED / CAP ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn forced_close_chiude_senza_final_gate_passed() {
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![fail_result(
            "no_orphan_imported",
            json!({"verdict": "placeholder rilevato"}),
        )]));
        let node = node_with(FinalGateConfig::default(), runner.clone());
        let ctx = ctx_with(false);
        let mut st = software_state();
        st.forced_close_unverified = Some(true);
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        // Chiude (end_turn) ma NON imposta final_gate_passed (resta FailedDiagnosed).
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.final_gate_passed, None);
        assert_eq!(out.final_gate_cycle, Some(0));
        // Nessun messaggio iniettato (chiusura, non re-executor).
        assert_eq!(out.messages.len(), 1, "solo il messaggio AI preesistente");
    }

    #[tokio::test]
    async fn cap_raggiunto_chiude_senza_re_executor() {
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![fail_result(
            "no_orphan_imported",
            json!({"verdict": "fail"}),
        )]));
        // max_cycles=2, final_gate_cycle gia' a 1 -> cycle diventa 2 >= 2 -> cap.
        let node = node_with(FinalGateConfig::default(), runner.clone());
        let ctx = ctx_with(false);
        let mut st = software_state();
        st.final_gate_cycle = Some(1);
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.final_gate_passed, None);
        assert_eq!(out.final_gate_cycle, Some(0));
    }

    // ── Ramo FAIL (re-executor) ─────────────────────────────────────────────────

    #[tokio::test]
    async fn fail_re_executor_inietta_human_message() {
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![fail_result(
            "no_orphan_imported",
            json!({"verdict": "hello-world non raggiunge il design importato"}),
        )]));
        let node = node_with(FinalGateConfig::default(), runner.clone());
        let ctx = ctx_with(false);
        // cycle parte da 0 -> 1, max_cycles 2, non forced -> FAIL.
        let st = software_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(out.final_gate_cycle, Some(1));
        assert_eq!(out.final_gate_passed, None);
        // pending_tool_uses azzerato a lista vuota.
        assert_eq!(out.pending_tool_uses, Some(vec![]));
        // HumanMessage del gate accodato (AI preesistente + il nuovo Human).
        assert_eq!(out.messages.len(), 2);
        let last = out.messages.last().expect("ultimo messaggio");
        match last {
            Message::Human { content } => {
                let text = content.flatten_text();
                assert!(text.contains("<final_gate_failed cycle=\"1/2\">"));
                assert!(text.contains("hello-world non raggiunge"));
                assert!(text.contains("DIRETTIVE (fail-closed):"));
            }
            other => panic!("atteso HumanMessage, trovato {other:?}"),
        }
    }

    // ── Shadow: ExecMode::Replay, zero side-effect ───────────────────────────────

    #[tokio::test]
    async fn shadow_usa_replay() {
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![ok_result(
            "no_orphan_imported",
        )]));
        let node = node_with(FinalGateConfig::default(), runner.clone());
        let ctx = ctx_with(true); // shadow
        let st = software_state();
        let _ = node.run(&st, &ctx).await.expect("run ok");
        let seen = runner.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1, ExecMode::Replay, "shadow deve usare Replay");
    }

    // ── build_criteria deterministico ────────────────────────────────────────────

    #[test]
    fn build_criteria_ordine_e_opzionali() {
        // Senza build_command e con runtime_check ON ma log_command vuoto:
        // solo no_orphan + outputs_exist.
        let node = node_with(
            FinalGateConfig::default(),
            Arc::new(StubCriteriaRunner::with_results(vec![])),
        );
        let st = software_state();
        let crits = node.build_criteria(&st);
        let types: Vec<&str> = crits.iter().map(|c| c.criterion_type.as_str()).collect();
        assert_eq!(types, vec!["no_orphan_imported", "outputs_exist"]);

        // Con log_command + build_command: 4 criteri nell'ordine canonico.
        let cfg = FinalGateConfig {
            log_command: "docker compose logs".to_string(),
            build_command: Some("pnpm build".to_string()),
            build_working_dir: Some("app".to_string()),
            ..Default::default()
        };
        let node2 = node_with(cfg, Arc::new(StubCriteriaRunner::with_results(vec![])));
        let crits2 = node2.build_criteria(&st);
        let types2: Vec<&str> = crits2.iter().map(|c| c.criterion_type.as_str()).collect();
        assert_eq!(
            types2,
            vec![
                "no_orphan_imported",
                "outputs_exist",
                "service_logs_clean",
                "run_command"
            ]
        );
        // Il criterio build porta max_output_chars + working_dir + timeout.
        let build = crits2.last().expect("build criterion");
        assert_eq!(build.spec["command"], json!("pnpm build"));
        assert_eq!(build.spec["max_output_chars"], json!(4000));
        assert_eq!(build.spec["working_dir"], json!("app"));
        assert_eq!(build.timeout_s, Some(180.0));
        assert_eq!(build.expected, json!({ "exit_code": 0 }));

        // Con anche l'endpoint_criterion risolto a monte: 5 criteri, http ULTIMO
        // nell'ordine (`final_gate.py:468-470`). La spec arriva pronta dal
        // risolutore DB (`_resolve_endpoint_check`): il nodo la accoda 1:1.
        let endpoint = CriterionSpec {
            criterion_type: "http".to_string(),
            spec: json!({ "url": "http://localhost:3000/api/login", "method": "POST" }),
            expected: json!({ "status": 200 }),
            timeout_s: Some(15.0),
        };
        let cfg3 = FinalGateConfig {
            log_command: "docker compose logs".to_string(),
            build_command: Some("pnpm build".to_string()),
            build_working_dir: Some("app".to_string()),
            endpoint_criterion: Some(endpoint.clone()),
            ..Default::default()
        };
        let node3 = node_with(cfg3, Arc::new(StubCriteriaRunner::with_results(vec![])));
        let crits3 = node3.build_criteria(&st);
        let types3: Vec<&str> = crits3.iter().map(|c| c.criterion_type.as_str()).collect();
        assert_eq!(
            types3,
            vec![
                "no_orphan_imported",
                "outputs_exist",
                "service_logs_clean",
                "run_command",
                "http"
            ]
        );
        // Il criterio endpoint e' accodato 1:1 (spec/expected/timeout invariati).
        assert_eq!(crits3.last().expect("endpoint criterion"), &endpoint);

        // Endpoint risolto SENZA build/log: comunque accodato dopo i 2 sempre-on.
        let cfg4 = FinalGateConfig {
            endpoint_criterion: Some(endpoint.clone()),
            ..Default::default()
        };
        let node4 = node_with(cfg4, Arc::new(StubCriteriaRunner::with_results(vec![])));
        let crits4 = node4.build_criteria(&st);
        let types4: Vec<&str> = crits4.iter().map(|c| c.criterion_type.as_str()).collect();
        assert_eq!(types4, vec!["no_orphan_imported", "outputs_exist", "http"]);
    }

    // ── count_build_errors ────────────────────────────────────────────────────────

    #[test]
    fn count_build_errors_pattern() {
        assert_eq!(count_build_errors(""), 0);
        assert_eq!(count_build_errors("tutto ok, nessun errore qui"), 0);
        // tsc.
        assert_eq!(
            count_build_errors("src/a.ts(1,1): error TS2304: not found\nerror TS1005: ;"),
            2
        );
        // rustc.
        assert_eq!(count_build_errors("error[E0432]: unresolved import"), 1);
        // SyntaxError/TypeError.
        assert_eq!(count_build_errors("SyntaxError: bad\nTypeError: x"), 2);
        // generico cargo (a inizio riga).
        assert_eq!(count_build_errors("error: cannot find crate"), 1);
        // vite/rollup: build che ESCE 0 ma FALLISCE (rete di sicurezza mig 0465).
        assert!(
            count_build_errors(
                "error during build:\nCould not resolve \"./components/ui/sonner\" from \"src/app/App.tsx\""
            ) >= 2,
            "pattern vite (could not resolve + error during build) devono contare"
        );
        assert!(count_build_errors("[plugin:vite:import-analysis] x") >= 1);
        assert!(count_build_errors("x Build failed in 312ms") >= 1);
    }

    // ── render_failed_block ────────────────────────────────────────────────────────

    #[test]
    fn render_failed_block_build_con_troncamento() {
        let st = software_state();
        let results = vec![fail_result(
            "run_command",
            json!({
                "output_excerpt": "error TS2304: a\nerror TS2305: b",
                "exit_code": 1,
                "output_total_chars": 9999,
                "output_truncated": true,
            }),
        )];
        let block = FinalGateNode::render_failed_block(&st, 1, 2, &results);
        assert!(block.contains("<final_gate_failed cycle=\"1/2\">"));
        // Header build: tipo + count errori + nota troncamento.
        assert!(block.contains("[run_command]"));
        assert!(block.contains("errori rilevati: 2"));
        assert!(block.contains("output troncato"));
        assert!(block.contains("/9999 char)"));
        // La riga col conteggio errori e' inserita nelle direttive.
        assert!(block.contains("Numero di errori rilevati nel build: 2"));
        // Niente autonomy_hint (behavior_mode non autonomo).
        assert!(!block.contains("<autonomy_hint"));
    }

    #[test]
    fn render_failed_block_criterio_non_build_e_autonomy_hint() {
        let mut st = software_state();
        st.behavior_mode = Some("automatico".to_string());
        let results = vec![fail_result(
            "service_logs_clean",
            json!({"verdict": "errore 500 nei log del servizio"}),
        )];
        let block = FinalGateNode::render_failed_block(&st, 1, 2, &results);
        // autonomy_hint prefisso per modalita' autonoma.
        assert!(block.contains("<autonomy_hint mode=\"automatico\">"));
        assert!(block.contains("[service_logs_clean]"));
        assert!(block.contains("errore 500 nei log"));
        // Nessun header build (niente "errori rilevati").
        assert!(!block.contains("errori rilevati:"));
    }

    #[test]
    fn render_failed_block_excerpt_vuoto_cade_su_verdict() {
        // FIX A (parita' falsy): output_excerpt = "" deve cadere su verdict
        // (Python `or`). Senza il fix, "" interromperebbe la catena e il criterio
        // verrebbe saltato (excerpt vuoto -> continue), perdendo il verdict.
        let st = software_state();
        let results = vec![fail_result(
            "service_logs_clean",
            json!({"output_excerpt": "", "verdict": "errore 500 dal verdict"}),
        )];
        let block = FinalGateNode::render_failed_block(&st, 1, 2, &results);
        // Il verdict e' renderizzato (non saltato).
        assert!(block.contains("[service_logs_clean]"));
        assert!(block.contains("errore 500 dal verdict"));
        // Il corpo NON e' il placeholder di "nessun criterio renderizzato".
        assert!(!block.contains("Una verifica del gate e' fallita."));
    }

    #[test]
    fn render_failed_block_total_chars_zero_cade_su_len_text() {
        // FIX B (parita' falsy): output_total_chars = 0 deve cadere su len(text)
        // (Python `or`). text = "error TS2304: x" -> 15 codepoint. Con lo 0 tenuto
        // (vecchio Rust) l'header sarebbe "(15/0 char)", col fallback "(15/15 char)".
        let st = software_state();
        let text = "error TS2304: x";
        let results = vec![fail_result(
            "run_command",
            json!({
                "output_excerpt": text,
                "exit_code": 1,
                "output_total_chars": 0,
                "output_truncated": true,
            }),
        )];
        let block = FinalGateNode::render_failed_block(&st, 1, 2, &results);
        let expected_len = text.chars().count();
        assert!(block.contains("output troncato"));
        // total_chars = len(text), NON 0.
        assert!(
            block.contains(&format!("({expected_len}/{expected_len} char)")),
            "atteso fallback a len(text)={expected_len}, blocco:\n{block}"
        );
        assert!(!block.contains("/0 char)"), "lo 0 non deve essere tenuto");
    }

    #[test]
    fn render_failed_block_nessun_excerpt_default() {
        let st = software_state();
        // Criterio fallito senza output_excerpt/verdict/error.
        let results = vec![fail_result("outputs_exist", json!({}))];
        let block = FinalGateNode::render_failed_block(&st, 2, 2, &results);
        assert!(block.contains("Una verifica del gate e' fallita."));
    }

    #[test]
    fn all_passed_reduce() {
        assert!(FinalGateNode::all_passed(&[
            ok_result("a"),
            ok_result("b")
        ]));
        assert!(!FinalGateNode::all_passed(&[
            ok_result("a"),
            fail_result("b", json!({}))
        ]));
        // Lista vuota -> all() su iterabile vuoto = true (parita' Python).
        assert!(FinalGateNode::all_passed(&[]));
    }
}

#[cfg(test)]
mod golden {
    //! Golden-test di PARITA' 1:1 vs Python sulla logica DETERMINISTICA del nodo
    //! final_gate. Lo script `scripts/gen_golden_final_gate.py` importa le
    //! funzioni reali da `brain/agents/final_gate.py` (`_is_software_task`,
    //! `_count_build_errors`, `_render_failed_block`, `route_after_final_gate`) +
    //! replica la decision machine di `final_gate_node` (deterministica, dati i
    //! risultati criteri) e salva `{case_id, function, input, output}` in
    //! `/tmp/golden_final_gate.json`. Qui ricostruiamo l'input, chiamiamo la
    //! funzione Rust corrispondente e verifichiamo `output == golden Python`.
    //!
    //! `#[ignore]` perche' dipende dal file generato. Comando:
    //!   python3 crates/nexus-agent-graph/scripts/gen_golden_final_gate.py
    //!   cargo test -p nexus-agent-graph --lib golden_final_gate_parita -- --ignored

    use serde::Deserialize;
    use serde_json::{json, Value};

    use super::{count_build_errors, FinalGateNode};
    use crate::routing::config::RoutingConfig;
    use crate::routing::{route_after_final_gate, signals, NodeTarget};
    use crate::runtime::ports::CriterionResult;
    use crate::state::{AgentState, ContentBlock, Message, MessageContent};

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        case_id: String,
        function: String,
        input: Value,
        output: Value,
    }

    /// Ricostruisce un `AgentState` minimale dai campi dell'input golden usati
    /// dalle funzioni testate (messages con tool_use, user_intent, behavior_mode,
    /// thread_id, forced_close_unverified, final_gate_cycle, stop_reason). Le
    /// assegnazioni sono CONDIZIONALI (un campo si popola solo se la chiave e'
    /// presente nell'input golden): `st` parte dal costruttore con `messages` +
    /// `..Default::default()`, poi i campi opzionali sono settati se presenti.
    fn state_from(input: &Value) -> AgentState {
        // messages: lista di {role, tool_uses:[name,...]} -> Message::Ai con blocchi.
        // Costruite PRIMA dell'inizializzazione di `st` (evita
        // field_reassign_with_default: il costruttore usa direttamente i campi).
        let mut msgs: Vec<Message> = Vec::new();
        if let Some(arr) = input.get("messages").and_then(Value::as_array) {
            for m in arr {
                let role = m.get("role").and_then(Value::as_str).unwrap_or("");
                if role == "assistant" || role == "ai" {
                    let mut blocks: Vec<ContentBlock> = Vec::new();
                    if let Some(tools) = m.get("tool_uses").and_then(Value::as_array) {
                        for (i, t) in tools.iter().enumerate() {
                            if let Some(name) = t.as_str() {
                                blocks.push(ContentBlock::ToolUse {
                                    id: format!("c{i}"),
                                    name: name.to_string(),
                                    input: json!({}),
                                });
                            }
                        }
                    }
                    msgs.push(Message::Ai {
                        content: MessageContent::Blocks(blocks),
                        tool_calls: vec![],
                        reasoning: None,
                    });
                } else if role == "user" || role == "human" {
                    let c = m.get("content").and_then(Value::as_str).unwrap_or("");
                    msgs.push(Message::Human {
                        content: MessageContent::text(c),
                    });
                }
            }
        }
        let mut st = AgentState {
            messages: msgs,
            ..Default::default()
        };
        if let Some(ui) = input.get("user_intent").and_then(Value::as_str) {
            st.user_intent = Some(ui.to_string());
        }
        if let Some(intent) = input.get("intent").and_then(Value::as_str) {
            st.extra.insert("intent".to_string(), json!(intent));
        }
        if let Some(bm) = input.get("behavior_mode").and_then(Value::as_str) {
            st.behavior_mode = Some(bm.to_string());
        }
        if let Some(tid) = input.get("thread_id").and_then(Value::as_str) {
            st.thread_id = Some(tid.to_string());
        }
        if let Some(f) = input.get("forced_close_unverified").and_then(Value::as_bool) {
            st.forced_close_unverified = Some(f);
        }
        if let Some(c) = input.get("final_gate_cycle").and_then(Value::as_i64) {
            st.final_gate_cycle = Some(c);
        }
        if let Some(sr) = input.get("stop_reason").and_then(Value::as_str) {
            st.stop_reason = serde_json::from_value(json!(sr)).ok();
        }
        st
    }

    /// Risultati criteri dall'input golden (lista {type, passed, evidence}).
    fn results_from(input: &Value) -> Vec<CriterionResult> {
        input
            .get("results")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .map(|r| CriterionResult {
                        criterion_type: r
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        passed: r.get("passed").and_then(Value::as_bool).unwrap_or(false),
                        evidence: r.get("evidence").cloned().unwrap_or(json!({})),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Replica in Rust la DECISION MACHINE di `final_gate_node` data la
    /// pre-condizione che il gate sia entrato (enabled + software gia' decisi a
    /// monte dal golden) e i risultati criteri. Ritorna il delta nella forma
    /// confrontabile col Python (`{}` o dict). NON esegue i criteri (input).
    fn decision_delta(st: &AgentState, results: &[CriterionResult], max_cycles: i64) -> Value {
        let cycle = st.final_gate_cycle.unwrap_or(0) + 1;
        let passed = FinalGateNode::all_passed(results);
        if passed {
            return json!({
                "final_gate_cycle": 0,
                "stop_reason": "end_turn",
                "final_gate_passed": true,
            });
        }
        let forced = st.forced_close_unverified.unwrap_or(false);
        if forced || cycle >= max_cycles {
            return json!({"final_gate_cycle": 0, "stop_reason": "end_turn"});
        }
        let block = FinalGateNode::render_failed_block(st, cycle, max_cycles, results);
        json!({
            "messages": [block],
            "final_gate_cycle": cycle,
            "stop_reason": "tool_use",
            "pending_tool_uses": [],
        })
    }

    #[test]
    #[ignore = "richiede /tmp/golden_final_gate.json generato da gen_golden_final_gate.py"]
    fn golden_final_gate_parita() {
        let Some(raw) =
            crate::golden_util::load_golden("golden_final_gate.json", "gen_golden_final_gate.py")
        else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(!cases.is_empty(), "golden vuoto");

        // is_software_task usa la RoutingConfig di default (whitelist + mutator
        // identici ai _SAFE_DEFAULTS Python).
        let routing_cfg = RoutingConfig::default();

        let mut checked = 0usize;
        for c in &cases {
            let got: Value = match c.function.as_str() {
                "is_software_task" => {
                    let st = state_from(&c.input);
                    json!(signals::is_software_task(&st, &routing_cfg))
                }
                "count_build_errors" => {
                    let out = c.input.get("output").and_then(Value::as_str).unwrap_or("");
                    json!(count_build_errors(out))
                }
                "render_failed_block" => {
                    let st = state_from(&c.input);
                    let cycle = c.input.get("cycle").and_then(Value::as_i64).unwrap_or(1);
                    let max_cycles =
                        c.input.get("max_cycles").and_then(Value::as_i64).unwrap_or(2);
                    let results = results_from(&c.input);
                    json!(FinalGateNode::render_failed_block(&st, cycle, max_cycles, &results))
                }
                "route_after_final_gate" => {
                    let st = state_from(&c.input);
                    let target = match route_after_final_gate(&st) {
                        NodeTarget::Executor => "executor",
                        NodeTarget::Learner => "learner",
                        other => panic!("target inatteso da route_after_final_gate: {other:?}"),
                    };
                    json!(target)
                }
                "decision_machine" => {
                    let st = state_from(&c.input);
                    let results = results_from(&c.input);
                    let max_cycles =
                        c.input.get("max_cycles").and_then(Value::as_i64).unwrap_or(2);
                    decision_delta(&st, &results, max_cycles)
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
        println!("golden final_gate: {checked} casi verificati, tutti verdi");
    }
}
