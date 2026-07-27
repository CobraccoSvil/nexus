//! Adapter del trait [`nexus_agent_graph::runtime::ports::CriteriaRunner`].
//!
//! Implementa `CriteriaRunner::run` eseguendo i criteri generali del final gate
//! (parita' con `brain/agents/criteria_runner.py`). I criteri che orchestrano
//! tool delegano al PUNTO UNICO dell'esecuzione tool ([`ToolExecutor`], regola L):
//! NON ricostruiscono il dispatch.
//!
//! COPERTURA DEI CRITERI (segnalata esplicitamente):
//!
//! | criterio              | strategia | note |
//! |-----------------------|-----------|------|
//! | `run_command` (build) | NATIVO    | tool `run_command` -> confronto `exit_code` vs `expected.exit_code`; evidence con `output_excerpt`/`output_total_chars`/`output_truncated` (chiavi lette da `final_gate::render_failed_block`, ramo build) |
//! | `service_logs_clean`  | NATIVO    | tool `run_command` -> match dei `patterns` sulle righe di log; inconclusive su comando fallito |
//! | `http`                | NATIVO    | chiamata REALE via `reqwest` (parita' httpx Python); status singolo o lista + `body_contains` opzionale |
//! | `file_exists`         | NATIVO    | tool `list_files` sulla parent + match basename (parita' fallback Python) |
//! | `outputs_exist`       | NATIVO    | lettura `agent_steps` (tool mutativi file del run) + verifica esistenza via `file_exists` |
//! | `no_orphan_imported`  | TODO (F3) | grafo degli import BFS (~150 righe Python non portabili rapidamente): ritorna `inconclusive` (passed=true) finche' non portato — un criterio non valutabile NON deve far fallire il gate |
//!
//! RISCHIO AL CUTOVER (lezione "final_gate deve verificare il build"): il criterio
//! BUILD (`run_command` con `expected.exit_code=0`) e' NATIVO e funzionante: al
//! cutover il final_gate Rust verifica davvero che il codice COMPILI. L'unico
//! criterio non coperto e' `no_orphan_imported` (anti-placeholder Figma/v0): resta
//! inconclusive finche' F3 non porta il grafo import — il gate non lo conteggia,
//! quindi non degrada la qualita' degli altri criteri, ma l'anti-placeholder e'
//! temporaneamente cieco (TODO F3 tracciato).
//!
//! PARITA' ERRORE (parita' col try/except `final_gate.py:381-385`): un criterio
//! fallito NON propaga un errore di porta — diventa un [`CriterionResult`] con
//! `passed=false` + `evidence.error`/`evidence.verdict`. Il [`PortError`] resta
//! per un guasto infrastrutturale del runner stesso (es. lettura `agent_steps`
//! fallita in `outputs_exist`).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{
    CriteriaRunner, CriterionResult, CriterionSpec, PortError, ToolCall, ToolExecutor,
};

/// Timeout di default per i criteri (parita' col Python: `30.0s`). I criteri con
/// `timeout_s` valorizzato (solo il build) lo sovrascrivono.
const DEFAULT_TIMEOUT_S: f64 = 30.0;

/// Default storico dell'excerpt di output dei criteri (`final_gate.py`: 600).
const DEFAULT_MAX_OUTPUT_CHARS: usize = 600;

/// Tool mutativi file -> chiave dell'input col path di OUTPUT (parita'
/// `criteria_runner._OUTPUT_PATH_KEYS`). Usato da `outputs_exist`.
const OUTPUT_PATH_KEYS: &[(&str, &str)] = &[
    ("write_file", "path"),
    ("edit_file", "path"),
    ("create_file", "path"),
    ("apply_patch", "path"),
    ("rename_file", "to"),
    ("fs_move", "to"),
];

/// Adapter [`CriteriaRunner`] -> criteri NATIVI su [`ToolExecutor`] + HTTP/DB.
pub struct FinalGateCriteriaRunnerAdapter {
    /// Esecutore tool (PUNTO UNICO, regola L): i criteri che usano `run_command`/
    /// `list_files` delegano qui invece di ricostruire il dispatch.
    tool_executor: Arc<dyn ToolExecutor>,
    /// Pool Postgres: lettura `agent_steps` per il criterio `outputs_exist`.
    db: PgPool,
    /// Client HTTP per il criterio `http` (chiamata reale all'endpoint).
    http_client: reqwest::Client,
}

impl FinalGateCriteriaRunnerAdapter {
    /// Costruisce il runner sull'esecutore tool condiviso + pool Postgres.
    pub fn new(tool_executor: Arc<dyn ToolExecutor>, db: PgPool) -> Self {
        Self {
            tool_executor,
            db,
            http_client: reqwest::Client::new(),
        }
    }

    /// Esegue un tool via il PUNTO UNICO [`ToolExecutor`] e ritorna il testo del
    /// risultato (content). Errore di porta -> propagato al chiamante (che lo mappa
    /// su evidence o lo gestisce).
    async fn run_tool(
        &self,
        name: &str,
        input: Value,
    ) -> Result<String, PortError> {
        let call = ToolCall {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            input,
            thought_signature: None,
        };
        let outcome = self.tool_executor.execute(call).await?;
        // content e' tipicamente una stringa (output del tool); normalizziamo.
        Ok(match outcome.content {
            Value::String(s) => s,
            other => other.to_string(),
        })
    }

    /// Misura l'exit code di un comando sull'albero CORRENTE (baseline
    /// pre-lavoro degli step gate, delta-aware sui criteri). Riusa il PUNTO
    /// UNICO `run_tool` + `extract_exit_code` (regola L/M: exit code
    /// strutturato, mai parsing dell'output). `None` = comando non eseguibile
    /// o exit non estraibile -> il chiamante NON persiste baseline (fail-closed
    /// nel gate). Chiamata SOLO dal run primario, prima che l'executor tocchi
    /// file.
    pub async fn measure_command_exit(
        &self,
        command: &str,
        working_dir: Option<&str>,
    ) -> Option<i64> {
        let mut tool_input = json!({ "command": command });
        if let Some(wd) = working_dir {
            tool_input["working_dir"] = json!(wd);
        }
        let raw = self
            .run_tool("run_command", tool_input)
            .await
            .ok()?;
        crate::tool_runner_server::extract_exit_code(&raw).map(|e| e as i64)
    }

    /// Esegue UN criterio. Parita' col `run_criterion` Python: il dispatch per
    /// `criterion_type` + il try/except che mappa un fallimento su
    /// `passed=false`/`evidence.error` (mai un panico). Il [`PortError`] risale solo
    /// per un guasto infrastrutturale del runner (es. DB).
    async fn run_one(
        &self,
        c: &CriterionSpec,
    ) -> Result<CriterionResult, PortError> {
        let timeout_s = c.timeout_s.unwrap_or(DEFAULT_TIMEOUT_S);
        let (passed, mut evidence) = match c.criterion_type.as_str() {
            "run_command" => {
                self.check_run_command(&c.spec, &c.expected, timeout_s)
                    .await
            }
            "design_verify" => Self::check_design_verify(&c.spec),
            // Criteri STRUTTURALI (ADR 0018 leva 3): PURI, i fatti sono gia'
            // nella spec (estratti dallo stato in FinalGateNode::build_criteria).
            "action_requested" => Self::check_action_requested(&c.spec),
            "tool_capability" => Self::check_tool_capability(&c.spec),
            "completion_confirmed" => Self::check_completion_confirmed(&c.spec),
            "service_logs_clean" => {
                self.check_service_logs_clean(&c.spec, timeout_s)
                    .await
            }
            "http" => self.check_http(&c.spec, &c.expected, timeout_s).await,
            "file_exists" => {
                self.check_file_exists(&c.spec, &c.expected, timeout_s)
                    .await
            }
            "outputs_exist" => self.check_outputs_exist(&c.spec, timeout_s).await?,
            // Anti-placeholder grafo import: non ancora portato (F3) -> inconclusive.
            "no_orphan_imported" | "imported_code_mounted" => (
                true,
                json!({
                    "inconclusive": true,
                    "skipped_reason": "no_orphan_imported non ancora portato in Rust (grafo import BFS): criterio inconcludente, escluso dal gate (TODO F3)",
                }),
            ),
            other => (
                false,
                json!({ "error": format!("tipo di criterion sconosciuto: '{other}'") }),
            ),
        };
        // Eco del tipo nell'evidence (parita' `run_criterion`: `ev["type"]`).
        if let Value::Object(map) = &mut evidence {
            map.insert("type".to_string(), json!(c.criterion_type));
        }
        Ok(CriterionResult {
            criterion_type: c.criterion_type.clone(),
            passed,
            evidence,
        })
    }

    // ── run_command (BUILD): exit_code vs expected ───────────────────────────

    // design_verify (P5): resa visiva conforme al figma. PURO - lo similarity_score
    // e' gia' nella spec (estratto dalla history da build_criteria), niente vision qui.
    fn check_design_verify(spec: &Value) -> (bool, Value) {
        let score = spec
            .get("similarity_score")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let min = spec.get("min_score").and_then(Value::as_i64).unwrap_or(0);
        let passed = score >= min;
        let verdict = if passed {
            "resa visiva conforme al design".to_string()
        } else {
            format!(
                "resa visiva sotto soglia ({score}/{min}): continua ad allineare il layout \
al figma ed esegui di nuovo nexus_visual_compare finche similarity_score >= {min}"
            )
        };
        (
            passed,
            json!({ "similarity_score": score, "min_score": min, "verdict": verdict }),
        )
    }

    // ── Criteri STRUTTURALI (ADR 0018 leva 3): puri, fatti nella spec ────────

    /// action_requested: una richiesta d'AZIONE chiusa senza alcuna azione
    /// produttiva in history non passa. Non-action-oriented -> passa sempre.
    fn check_action_requested(spec: &Value) -> (bool, Value) {
        let action_oriented = spec
            .get("action_oriented")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let acted = spec
            .get("has_productive_action")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let passed = !action_oriented || acted;
        let verdict = if passed {
            "nessuna azione richiesta o azione produttiva presente".to_string()
        } else {
            "richiesta d'azione chiusa SENZA alcuna azione produttiva: esegui \
il lavoro con i tool prima di chiudere"
                .to_string()
        };
        (
            passed,
            json!({ "action_oriented": action_oriented, "has_productive_action": acted, "verdict": verdict }),
        )
    }

    /// tool_capability: un task software con ZERO tool esposti e senza tool call
    /// gia' osservate e' una misconfigurazione del catalogo/whitelist. La history
    /// con tool_use e' un segnale strutturato che il catalogo era disponibile
    /// durante il lavoro, anche se `tools_json` non e' arrivato al gate/resume.
    fn check_tool_capability(spec: &Value) -> (bool, Value) {
        let tools_count = spec.get("tools_count").and_then(Value::as_i64).unwrap_or(0);
        let has_tool_calls = spec
            .get("has_tool_calls")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let passed = tools_count > 0 || has_tool_calls;
        let verdict = if tools_count > 0 {
            format!("{tools_count} tool esposti al modello")
        } else if has_tool_calls {
            "catalogo tool non presente nello stato finale, ma la history contiene \
tool_use gia' osservati: capacita' tool confermata dal segnale strutturato"
                .to_string()
        } else {
            "ZERO tool esposti su un task software: misconfigurazione modello/\
whitelist (verificare supports_tool_use e le whitelist agent.tools.*)"
                .to_string()
        };
        (
            passed,
            json!({ "tools_count": tools_count, "has_tool_calls": has_tool_calls, "verdict": verdict }),
        )
    }

    /// completion_confirmed: la chiusura va CONFERMATA da una dichiarazione
    /// strutturata task_complete (ADR 0034). Qualunque outcome onesto passa.
    ///
    /// Delega post-subagente: quando il PADRE coordinatore delega il lavoro a un
    /// sub-agente che arriva a chiusura (`subagent_completed`, segnale MACCHINA
    /// dalla history, regola M) e chiude senza ri-dichiarare, la dichiarazione
    /// onesta del run ESISTE gia' (quella del figlio) -> passa. Il criterio ne
    /// cerca UNA, non che sia del padre; la verifica tecnica (build/typecheck)
    /// resta a guardia della correttezza per un figlio che ha lasciato incompleto.
    fn check_completion_confirmed(spec: &Value) -> (bool, Value) {
        let declared = spec
            .get("declared_outcome")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty());
        let subagent_completed = spec
            .get("subagent_completed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let passed = declared.is_some() || subagent_completed;
        let verdict = match (declared, subagent_completed) {
            (Some(o), _) => format!("esito dichiarato: {o}"),
            (None, true) => "chiusura strutturata delegata a un sub-agente \
completato (task_complete del figlio)"
                .to_string(),
            (None, false) => "nessuna dichiarazione strutturata: chiudi con il tool \
task_complete (outcome + summary)"
                .to_string(),
        };
        (
            passed,
            json!({
                "declared_outcome": declared,
                "subagent_completed": subagent_completed,
                "verdict": verdict,
            }),
        )
    }

    async fn check_run_command(
        &self,
        spec: &Value,
        expected: &Value,
        _timeout_s: f64,
    ) -> (bool, Value) {
        let cmd = spec.get("command").and_then(Value::as_str).unwrap_or("");
        if cmd.is_empty() {
            return (false, json!({ "error": "spec.command obbligatorio" }));
        }
        let mut tool_input = json!({ "command": cmd });
        if let Some(wd) = spec.get("working_dir").and_then(Value::as_str) {
            tool_input["working_dir"] = json!(wd);
        }
        // Delega al ToolExecutor. Un guasto di porta -> evidence.error (parita'
        // col try/except Python), non propaga.
        let raw = match self.run_tool("run_command", tool_input).await {
            Ok(s) => s,
            Err(e) => {
                return (
                    false,
                    json!({ "error": format!("execute_tool: {e}"), "command": cmd }),
                )
            }
        };
        // exit_code STRUTTURATO dal punto unico (stesso parser del path gRPC).
        let actual_exit = crate::tool_runner_server::extract_exit_code(&raw);
        let expected_exit = expected
            .get("exit_code")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let exit_ok = actual_exit.map(|a| a as i64) == Some(expected_exit);

        // RETE DI SICUREZZA (regola H): alcuni build ESCONO 0 anche quando il
        // bundling FALLISCE (es. `vite build` con "Could not resolve" / "error
        // during build" che in certe config esce 0). Affidarsi al solo exit_code
        // -> falso verde -> il final_gate chiude "completed" un'app rotta
        // (incidente Beauty-Book). Se l'output contiene pattern di errore di
        // build (punto unico count_build_errors, regola L) il criterio FALLISCE
        // comunque, anche con exit 0.
        let build_errors = nexus_agent_graph::count_build_errors(&raw);

        // ── Gate DELTA-aware (regola H) ──────────────────────────────────────
        // Un errore di build conta come REGRESSIONE (che fallisce il gate) solo se
        // colpisce un file che QUESTO run ha TOCCATO. Gli errori in file non
        // toccati sono debito preesistente del progetto: non devono impedire la
        // chiusura di un task che non li ha introdotti (es. un fix di login
        // bocciato da errori di tipo preesistenti in BookingPage.tsx). Segnali
        // STRUTTURATI (regola M): exit_code, set dei file-con-errori (localizzazione
        // tsc/rustc), lista dei file toccati (dai tool_use mutator, non dalla
        // prosa). Fail-CLOSED di default: senza localizzazione degli errori
        // (formato non coperto -> set vuoto) o senza file toccati dichiarati si
        // ricade sul criterio ASSOLUTO (exit 0 + zero errori), identico a prima.
        let touched: Vec<&str> = spec
            .get("touched_files")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let error_files = nexus_agent_graph::build_error_files(&raw);
        let regressed_files: Vec<String> = error_files
            .iter()
            .filter(|ef| {
                touched
                    .iter()
                    .any(|tf| nexus_agent_graph::error_file_matches_touched(ef, tf))
            })
            .cloned()
            .collect();
        let delta_applicable = !error_files.is_empty() && !touched.is_empty();
        let regression = !regressed_files.is_empty();
        let preexisting_files = error_files.len().saturating_sub(regressed_files.len());
        // ── Delta-aware sui CRITERI (baseline pre-lavoro, regola H/M) ────────
        // Un criterio che fallisce ORA con lo STESSO exit code non-zero
        // misurato sull'albero PRE-lavoro (baseline all'innesto del profilo) e
        // SENZA file d'errore localizzati e' un fallimento di BOOTSTRAP
        // pre-esistente dell'ambiente (es. `npx eslint` exit 2 per config
        // assente: incidente run 695794af, pnpm build verde ma gate bocciato
        // per un criterio impossibile in QUALUNQUE stato del lavoro). Non e'
        // una regressione del run: non boccia, viene dichiarato nel verdict.
        // Condizioni STRETTE (solo segnali strutturati): exit non-zero
        // IDENTICO alla baseline + zero error_files (se il comando localizza
        // errori in file, decide il ramo delta sui touched_files). Se il run
        // avesse ROTTO lui il bootstrap (es. config cancellata), la baseline
        // sarebbe 0 e il confronto NON scatterebbe. `None` = fail-closed.
        let baseline_exit = spec.get("baseline_exit_code").and_then(Value::as_i64);
        let preexisting_bootstrap = !exit_ok
            && error_files.is_empty()
            && baseline_exit.is_some_and(|b| b != 0 && actual_exit.map(|a| a as i64) == Some(b));
        let passed = if delta_applicable {
            // Chiude se il task non ha lasciato errori nei file che ha toccato,
            // anche se il progetto ha debito preesistente altrove.
            !regression
        } else if preexisting_bootstrap {
            true
        } else {
            // Fallback fail-closed: criterio assoluto (identico a prima).
            exit_ok && build_errors == 0
        };

        // max_output_chars (parita' Python: default 600, floor 200). Il criterio
        // BUILD del nodo Rust lo valorizza (build_output_max_chars).
        let max_chars = spec
            .get("max_output_chars")
            .and_then(Value::as_i64)
            .map(|v| v.max(200) as usize)
            .unwrap_or(DEFAULT_MAX_OUTPUT_CHARS);
        let excerpt = truncate_chars(&raw, max_chars);
        let total_chars = raw.chars().count();
        let mut ev = json!({
            "command": cmd,
            "exit_code": actual_exit,
            "expected_exit": expected_exit,
            "output_excerpt": excerpt,
            "output_truncated": total_chars > max_chars,
            "output_total_chars": total_chars,
            "build_errors": build_errors,
            "error_files": error_files.len(),
            "delta_applied": delta_applicable,
            "preexisting_error_files": preexisting_files,
            "regressed_files": regressed_files.clone(),
            "baseline_exit_code": baseline_exit,
            "preexisting_bootstrap": preexisting_bootstrap,
        });
        if preexisting_bootstrap {
            ev["verdict"] = json!(format!(
                "Criterio fallito con esito IDENTICO alla baseline pre-lavoro (exit {} gia' misurato all'innesto del profilo): fallimento PRE-ESISTENTE dell'ambiente (es. config del tool assente), non una regressione di questo run. Debito del progetto, non blocca la chiusura.",
                baseline_exit.unwrap_or_default()
            ));
        } else if regression {
            // Il task ha lasciato errori nei file che ha modificato: blocca e dillo.
            ev["verdict"] = json!(format!(
                "Verifica delta: errori nei file modificati da questo task ({}). Correggi TUTTI gli errori in questi file prima di chiudere; il debito preesistente in altri file non e' richiesto.",
                regressed_files.join(", ")
            ));
        } else if delta_applicable && (build_errors > 0 || !exit_ok) {
            // Passa nonostante gli errori: sono debito preesistente in file non toccati.
            ev["verdict"] = json!(format!(
                "Verifica delta superata: {preexisting_files} file con errori PREESISTENTI non modificati da questo task (debito del progetto, non una regressione introdotta qui). Nessun errore nei file toccati dal task."
            ));
        } else if exit_ok && build_errors > 0 {
            // Fallback (delta non applicabile): exit-code bugiardo, come prima.
            ev["verdict"] = json!(format!(
                "Build uscito con exit ok ma l'output contiene {build_errors} errore/i di build (es. import non risolti): il bundle NON e' valido. Correggi gli errori sopra e riverifica."
            ));
        }
        (passed, ev)
    }

    // ── service_logs_clean: run_command + match pattern sui log ───────────────

    async fn check_service_logs_clean(
        &self,
        spec: &Value,
        _timeout_s: f64,
    ) -> (bool, Value) {
        let cmd = spec.get("command").and_then(Value::as_str).unwrap_or("");
        let patterns: Vec<String> = spec
            .get("patterns")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if cmd.is_empty() || patterns.is_empty() {
            return (
                true,
                json!({ "skipped": "service_logs_clean: command/patterns mancanti (N/A)" }),
            );
        }
        let raw = match self
            .run_tool("run_command", json!({ "command": cmd }))
            .await
        {
            Ok(s) => s,
            // Inconclusivo (non un fallimento): non blocchiamo la chiusura su un
            // errore di lettura log (parita' Python: ritorna passed=true).
            Err(e) => {
                return (
                    true,
                    json!({ "inconclusive": true, "skipped_reason": format!("run_command log fallito: {e}") }),
                )
            }
        };
        let lows: Vec<String> = patterns.iter().map(|p| p.to_lowercase()).collect();
        let mut hits: Vec<String> = Vec::new();
        for line in raw.lines() {
            let ll = line.to_lowercase();
            if lows.iter().any(|p| ll.contains(p)) {
                hits.push(truncate_chars(line.trim(), 200));
                if hits.len() >= 8 {
                    break;
                }
            }
        }
        let passed = hits.is_empty();
        let mut evidence = json!({
            "command": truncate_chars(cmd, 120),
            "error_lines": hits.len(),
            "matched": hits.clone(),
        });
        if !passed {
            evidence["verdict"] = json!("errori runtime nei log dei servizi");
            evidence["output_excerpt"] = json!(format!(
                "ERRORI RUNTIME nei log dei servizi: il codice e' stato scritto ma il flusso reale FALLISCE.\n{}\nAGISCI: trova e correggi la causa (es. applica le migrazioni / crea le tabelle mancanti, sistema la rotta), poi RIESERCITA il flusso reale e verifica che gli errori siano spariti.",
                hits.join("\n")
            ));
        }
        (passed, evidence)
    }

    // ── http: chiamata REALE via reqwest (parita' httpx Python) ───────────────

    async fn check_http(
        &self,
        spec: &Value,
        expected: &Value,
        timeout_s: f64,
    ) -> (bool, Value) {
        let url = spec.get("url").and_then(Value::as_str).unwrap_or("");
        if url.is_empty() {
            return (false, json!({ "error": "spec.url obbligatorio" }));
        }
        let method = spec
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_uppercase();
        // Status atteso: int singolo o lista (parita' Python).
        let expected_statuses: Vec<u16> = match expected.get("status") {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(Value::as_i64)
                .map(|v| v as u16)
                .collect(),
            Some(v) => vec![v.as_i64().unwrap_or(200) as u16],
            None => vec![200],
        };
        let m = match reqwest::Method::from_bytes(method.as_bytes()) {
            Ok(m) => m,
            Err(_) => {
                return (
                    false,
                    json!({ "error": format!("metodo HTTP invalido: {method}"), "url": url }),
                )
            }
        };
        let mut rb = self
            .http_client
            .request(m, url)
            .timeout(Duration::from_secs_f64(timeout_s));
        if let Some(body) = spec.get("body") {
            match body {
                Value::Object(_) | Value::Array(_) => rb = rb.json(body),
                Value::String(s) if !s.is_empty() => rb = rb.body(s.clone()),
                _ => {}
            }
        }
        if let Some(Value::Object(headers)) = spec.get("headers") {
            for (k, v) in headers {
                if let Some(vs) = v.as_str() {
                    rb = rb.header(k.as_str(), vs);
                }
            }
        }
        let resp = match rb.send().await {
            Ok(r) => r,
            Err(e) => {
                return (
                    false,
                    json!({ "error": format!("http call: {e}"), "url": url }),
                )
            }
        };
        let actual = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let body_excerpt = truncate_chars(&text, 400);
        let mut passed = expected_statuses.contains(&actual);
        if let Some(needle) = expected.get("body_contains").and_then(Value::as_str) {
            passed = passed && text.contains(needle);
        }
        (
            passed,
            json!({
                "url": url,
                "method": method,
                "status": actual,
                "expected_status": expected_statuses,
                "body_excerpt": body_excerpt,
            }),
        )
    }

    // ── file_exists: list_files sulla parent + match basename ─────────────────

    async fn check_file_exists(
        &self,
        spec: &Value,
        expected: &Value,
        _timeout_s: f64,
    ) -> (bool, Value) {
        let path = spec.get("path").and_then(Value::as_str).unwrap_or("");
        if path.is_empty() {
            // file_exists senza path: N/A (pass), parita' Python.
            return (
                true,
                json!({ "skipped": "file_exists senza path: criterio non applicabile (N/A)" }),
            );
        }
        let (parent_dir, basename) = match path.rsplit_once('/') {
            Some((p, b)) => (if p.is_empty() { "." } else { p }, b),
            None => (".", path),
        };
        let raw = match self
            .run_tool("list_files", json!({ "directory": parent_dir }))
            .await
        {
            Ok(s) => s,
            Err(e) => {
                return (
                    false,
                    json!({ "error": format!("execute_tool: {e}"), "path": path }),
                )
            }
        };
        let head = raw.chars().take(80).collect::<String>().to_lowercase();
        let list_error = raw.starts_with('\u{274C}')
            || head.contains("[errore")
            || head.contains("[error")
            || head.contains("non trovato")
            || head.contains("not found");
        if list_error {
            // list_files in errore: trattiamo come "non esiste" (parita' fallback).
            let expected_exists = expected
                .get("exists")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let passed = !expected_exists; // exists=false == expected_exists?
            return (
                passed,
                json!({
                    "path": path,
                    "exists": false,
                    "expected_exists": expected_exists,
                    "method": "list_files_error",
                    "output_excerpt": truncate_chars(&raw, 300),
                }),
            );
        }
        // Match del basename come token isolato (boundary spazio/slash/quote).
        let exists = raw
            .lines()
            .any(|line| line_contains_basename(line, basename));
        let expected_exists = expected
            .get("exists")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        (
            exists == expected_exists,
            json!({
                "path": path,
                "exists": exists,
                "expected_exists": expected_exists,
                "method": "list_files",
                "parent_dir": parent_dir,
                "basename": basename,
                "output_excerpt": truncate_chars(&raw, 300),
            }),
        )
    }

    // ── outputs_exist: agent_steps (tool mutativi) + file_exists ──────────────

    async fn check_outputs_exist(
        &self,
        spec: &Value,
        timeout_s: f64,
    ) -> Result<(bool, Value), PortError> {
        let run_id = spec
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if run_id.is_empty() {
            return Ok((
                true,
                json!({ "skipped": "outputs_exist senza run_id: N/A" }),
            ));
        }
        let run_uuid = match Uuid::parse_str(run_id) {
            Ok(u) => u,
            Err(_) => {
                return Ok((
                    true,
                    json!({ "skipped": "outputs_exist run_id non-UUID: N/A" }),
                ))
            }
        };
        // Tool mutativi file del run da agent_steps (status completed), ordinati.
        let mutator_names: Vec<&str> = OUTPUT_PATH_KEYS.iter().map(|(t, _)| *t).collect();
        let rows: Vec<(String, Value)> = sqlx::query_as(
            "SELECT tool_name, tool_input FROM agent_steps \
             WHERE run_id = $1 AND status = 'completed' AND tool_name = ANY($2) \
             ORDER BY step_index ASC",
        )
        .bind(run_uuid)
        .bind(&mutator_names)
        .fetch_all(&self.db)
        .await
        .map_err(|e| PortError::Tool(format!("outputs_exist lettura agent_steps: {e}").into()))?;

        let mut paths: Vec<String> = Vec::new();
        for (tool_name, tool_input) in &rows {
            let Some((_, key)) = OUTPUT_PATH_KEYS.iter().find(|(t, _)| *t == tool_name) else {
                continue;
            };
            // tool_input puo' essere JSONB object o stringa JSON.
            let obj = match tool_input {
                Value::Object(_) => tool_input.clone(),
                Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
                _ => Value::Null,
            };
            if let Some(p) = obj.get(*key).and_then(Value::as_str) {
                let p = p.trim().to_string();
                if !p.is_empty() && !paths.contains(&p) {
                    paths.push(p);
                }
            }
        }
        if paths.is_empty() {
            return Ok((
                true,
                json!({ "skipped": "nessuno step mutativo file nel run: N/A" }),
            ));
        }
        let mut missing: Vec<String> = Vec::new();
        let mut checked: Vec<String> = Vec::new();
        // Cap difensivo (parita' Python: 20 output).
        for p in paths.iter().take(20) {
            let (ok, _ev) = self
                .check_file_exists(&json!({ "path": p }), &json!({}), timeout_s)
                .await;
            checked.push(p.clone());
            if !ok {
                missing.push(p.clone());
            }
        }
        if !missing.is_empty() {
            return Ok((
                false,
                json!({
                    "missing": missing,
                    "checked": checked,
                    "verdict": "output dichiarati dagli step assenti sul filesystem a fine run",
                }),
            ));
        }
        Ok((true, json!({ "checked": checked })))
    }
}

#[async_trait]
impl CriteriaRunner for FinalGateCriteriaRunnerAdapter {
    async fn run(
        &self,
        criteria: Vec<CriterionSpec>,
    ) -> Result<Vec<CriterionResult>, PortError> {
        let mut out = Vec::with_capacity(criteria.len());
        for c in &criteria {
            out.push(self.run_one(c).await?);
        }
        Ok(out)
    }
}

/// Taglia una stringa a `max` CARATTERI (non byte: evita di spezzare UTF-8).
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// `true` se `basename` appare come token isolato in `line` (boundary inizio/fine
/// riga o spazio/slash/quote), parita' col regex Python di `_check_file_exists`.
fn line_contains_basename(line: &str, basename: &str) -> bool {
    if basename.is_empty() {
        return false;
    }
    let mut start = 0;
    while let Some(pos) = line[start..].find(basename) {
        let abs = start + pos;
        let before_ok = abs == 0
            || line[..abs]
                .chars()
                .next_back()
                .map(|c| c.is_whitespace() || matches!(c, '/' | '"' | '\'' | '`'))
                .unwrap_or(true);
        let after_idx = abs + basename.len();
        let after_ok = after_idx >= line.len()
            || line[after_idx..]
                .chars()
                .next()
                .map(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '`'))
                .unwrap_or(true);
        if before_ok && after_ok {
            return true;
        }
        start = abs + basename.len();
        if start >= line.len() {
            break;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// ToolExecutor fittizio: ritorna risultati pre-programmati per nome, e
    /// registra le chiamate ricevute (per asserire gli args).
    struct FakeToolExecutor {
        /// risultati per tool_name (in coda: la N-esima chiamata pop dell'indice N).
        results: std::collections::HashMap<String, Vec<String>>,
        /// log dei nomi tool chiamati per le asserzioni.
        calls: StdMutex<Vec<String>>,
    }

    impl FakeToolExecutor {
        fn with(results: &[(&str, &[&str])]) -> Arc<Self> {
            let mut map = std::collections::HashMap::new();
            for (name, outs) in results {
                map.insert(
                    name.to_string(),
                    outs.iter().map(|s| s.to_string()).collect(),
                );
            }
            Arc::new(Self {
                results: map,
                calls: StdMutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl ToolExecutor for FakeToolExecutor {
        async fn execute(
            &self,
            call: ToolCall,
        ) -> Result<nexus_agent_graph::runtime::ports::ToolOutcome, PortError> {
            self.calls.lock().unwrap().push(call.name.clone());
            let idx = self
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|n| *n == &call.name)
                .count()
                - 1;
            let content = self
                .results
                .get(&call.name)
                .and_then(|v| v.get(idx))
                .cloned()
                .unwrap_or_default();
            Ok(nexus_agent_graph::runtime::ports::ToolOutcome {
                tool_call_id: call.id,
                content: Value::String(content),
                ..Default::default()
            })
        }
    }

    fn spec(criterion_type: &str, spec: Value, expected: Value) -> CriterionSpec {
        CriterionSpec {
            criterion_type: criterion_type.to_string(),
            spec,
            expected,
            timeout_s: None,
        }
    }

    // sqlx::test fornisce un pool; per i criteri che non toccano il DB usiamo
    // comunque il pool (db richiesto dal costruttore) ma non lo interroghiamo.

    #[sqlx::test]
    async fn run_command_build_passa_su_exit_zero(pool: PgPool) {
        let exec = FakeToolExecutor::with(&[("run_command", &["compilato\nEXIT CODE: 0\nok"])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec.clone(), pool);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "cargo check" }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert_eq!(res.len(), 1);
        assert!(res[0].passed, "exit 0 == expected 0 -> passed");
        assert_eq!(res[0].evidence["exit_code"], json!(0));
        assert!(res[0].evidence["output_total_chars"].is_number());
        assert_eq!(res[0].evidence["type"], json!("run_command"));
    }

    /// REGRESSIONE run 48793fde (Beaty-Book): `pnpm build` (vite) esce 0 e RIESCE,
    /// ma stampa il warning del reporter `[plugin:vite:reporter]` (import misto
    /// dinamico/statico + chunk > 500 kB). Prima il pattern `[plugin:` di
    /// count_build_errors lo contava come errore -> il criterio build bocciava un
    /// build OGGETTIVAMENTE verde (falso negativo del final_gate, gate 2/2). Ora
    /// passa: exit 0 + zero errori reali.
    #[sqlx::test]
    async fn run_command_build_passa_con_warning_reporter_vite(pool: PgPool) {
        let raw = "EXIT CODE: 0\nSTDOUT:\n\
            vite v5.4.21 building for production...\n\
            2334 modules transformed.\n\
            [plugin:vite:reporter] [plugin vite:reporter]\n\
            (!) src/app/services/bookingService.ts is dynamically imported by \
            src/app/components/admin/AppointmentsTab.tsx but also statically imported by \
            src/app/components/admin/AdminLogin.tsx, dynamic import will not move module \
            into another chunk.\n\
            (!) Some chunks are larger than 500 kB after minification.\n\
            built in 7.67s\nSTDERR:\n";
        let exec = FakeToolExecutor::with(&[("run_command", &[raw])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "pnpm build" }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert!(
            res[0].passed,
            "build vite exit 0 con soli warning deve PASSARE, evidence: {}",
            res[0].evidence
        );
        assert_eq!(res[0].evidence["exit_code"], json!(0));
        assert_eq!(
            res[0].evidence["build_errors"],
            json!(0),
            "il warning [plugin:vite:reporter] non e' un errore"
        );
    }

    #[sqlx::test]
    async fn run_command_build_fallisce_su_exit_non_zero(pool: PgPool) {
        let exec =
            FakeToolExecutor::with(&[("run_command", &["error[E0432]\nEXIT CODE: 101\nboom"])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "cargo check" }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert!(!res[0].passed, "exit 101 != 0 -> build fallito");
        assert_eq!(res[0].evidence["exit_code"], json!(101));
        // output_excerpt presente (render_failed_block ramo build lo legge).
        assert!(res[0].evidence["output_excerpt"].is_string());
    }

    #[sqlx::test]
    async fn run_command_bootstrap_identico_alla_baseline_non_boccia(pool: PgPool) {
        // REGRESSIONE run 695794af: `npx eslint src` exit 2 per config assente
        // (fallimento di bootstrap PRE-ESISTENTE, zero file localizzati) con
        // baseline pre-lavoro identica NON deve bocciare il gate.
        let exec = FakeToolExecutor::with(&[(
            "run_command",
            &["Oops! Something went wrong!\nESLint couldn't find a config file.\nEXIT CODE: 2"],
        )]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "npx eslint src", "baseline_exit_code": 2, "touched_files": ["src/app/x.ts"] }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert!(
            res[0].passed,
            "fallimento identico alla baseline pre-lavoro = debito ambiente, non regressione"
        );
        assert_eq!(res[0].evidence["preexisting_bootstrap"], json!(true));
        let verdict = res[0].evidence["verdict"].as_str().unwrap_or_default();
        assert!(
            verdict.contains("PRE-ESISTENTE"),
            "verdict dichiara il debito: {verdict}"
        );
    }

    #[sqlx::test]
    async fn run_command_fallimento_diverso_dalla_baseline_boccia(pool: PgPool) {
        // Exit diverso dalla baseline = comportamento NUOVO (possibile rottura
        // introdotta dal run, es. config cancellata): resta bloccante.
        let exec = FakeToolExecutor::with(&[("run_command", &["boom di bootstrap\nEXIT CODE: 2"])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "npx eslint src", "baseline_exit_code": 1 }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert!(
            !res[0].passed,
            "exit 2 con baseline 1: non identico -> boccia"
        );
        assert_eq!(res[0].evidence["preexisting_bootstrap"], json!(false));
    }

    #[sqlx::test]
    async fn run_command_baseline_con_errori_localizzati_resta_delta_su_file(pool: PgPool) {
        // Se l'output LOCALIZZA errori in file, decide il ramo delta sui
        // touched_files (qui: errore in file TOCCATO -> regressione, boccia),
        // MAI il ramo bootstrap anche con baseline identica.
        let exec = FakeToolExecutor::with(&[(
            "run_command",
            &["src/app/x.ts(3,5): error TS2304: Cannot find name 'y'.\nEXIT CODE: 2"],
        )]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "npx tsc --noEmit", "baseline_exit_code": 2, "touched_files": ["src/app/x.ts"] }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert!(
            !res[0].passed,
            "errore localizzato in file toccato = regressione: la baseline non salva"
        );
        assert_eq!(res[0].evidence["preexisting_bootstrap"], json!(false));
    }

    // ── Gate DELTA-aware (regola H): regressione vs debito preesistente ──────────

    /// NO fail-open: un errore in un file che il task ha TOCCATO e' una
    /// regressione e fallisce il gate, anche se il resto e' preesistente.
    #[sqlx::test]
    async fn run_command_delta_regressione_in_file_toccato_fallisce(pool: PgPool) {
        let out = "src/app/pages/BookingPage.tsx(156,7): error TS2554: Expected 2 args\n\
                   src/app/pages/LoginPage.tsx(5,10): error TS2305: no member\n\
                   Found 2 errors.\nEXIT CODE: 2";
        let exec = FakeToolExecutor::with(&[("run_command", &[out])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "npx tsc --noEmit", "touched_files": ["src/app/pages/LoginPage.tsx"] }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert!(
            !res[0].passed,
            "errore in un file toccato = regressione -> fallisce (no fail-open)"
        );
        assert_eq!(res[0].evidence["delta_applied"], json!(true));
        let regressed = res[0].evidence["regressed_files"].as_array().unwrap();
        assert!(regressed
            .iter()
            .any(|v| v.as_str() == Some("src/app/pages/LoginPage.tsx")));
    }

    /// Debito preesistente: errori SOLO in file non toccati -> non bloccano la
    /// chiusura, anche con exit != 0 (il task non li ha introdotti).
    #[sqlx::test]
    async fn run_command_delta_debito_preesistente_non_blocca(pool: PgPool) {
        let out = "src/app/pages/BookingPage.tsx(156,7): error TS2554: Expected 2 args\n\
                   src/app/pages/ConfirmationPage.tsx(17,42): error TS2304: Cannot find name\n\
                   Found 2 errors.\nEXIT CODE: 2";
        let exec = FakeToolExecutor::with(&[("run_command", &[out])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "npx tsc --noEmit", "touched_files": ["src/app/services/authService.ts"] }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert!(
            res[0].passed,
            "errori solo in file NON toccati (debito preesistente) -> non blocca"
        );
        assert_eq!(res[0].evidence["delta_applied"], json!(true));
        assert_eq!(res[0].evidence["preexisting_error_files"], json!(2));
        assert!(res[0].evidence["regressed_files"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    /// Fail-CLOSED di default: errori localizzati ma il run non dichiara file
    /// toccati -> criterio ASSOLUTO come prima (exit != 0 -> fallisce).
    #[sqlx::test]
    async fn run_command_delta_senza_file_toccati_e_failclosed(pool: PgPool) {
        let out =
            "src/app/pages/BookingPage.tsx(156,7): error TS2554: Expected 2 args\nEXIT CODE: 2";
        let exec = FakeToolExecutor::with(&[("run_command", &[out])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "npx tsc --noEmit" }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert!(
            !res[0].passed,
            "senza file toccati -> fail-closed (criterio assoluto)"
        );
        assert_eq!(res[0].evidence["delta_applied"], json!(false));
    }

    #[sqlx::test]
    async fn run_command_delega_al_tool_executor(pool: PgPool) {
        let exec = FakeToolExecutor::with(&[("run_command", &["EXIT CODE: 0"])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec.clone(), pool);
        runner
            .run(vec![spec(
                "run_command",
                json!({ "command": "x" }),
                json!({ "exit_code": 0 }),
            )])
            .await
            .expect("ok");
        // La chiamata e' arrivata al ToolExecutor (punto unico dell'esecuzione).
        let calls = exec.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], "run_command".to_string());
    }

    #[sqlx::test]
    async fn service_logs_clean_passa_senza_pattern_hit(pool: PgPool) {
        let exec =
            FakeToolExecutor::with(&[("run_command", &["INFO server avviato\nINFO richiesta ok"])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "service_logs_clean",
                    json!({ "command": "docker logs app", "patterns": ["does not exist", "ERROR"] }),
                    json!({}),
                )],
            )
            .await
            .expect("ok");
        assert!(res[0].passed, "nessun pattern -> log puliti");
    }

    #[sqlx::test]
    async fn service_logs_clean_fallisce_su_pattern_hit(pool: PgPool) {
        let exec = FakeToolExecutor::with(&[(
            "run_command",
            &["relation \"users\" does not exist\nINFO altro"],
        )]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "service_logs_clean",
                    json!({ "command": "docker logs app", "patterns": ["does not exist"] }),
                    json!({}),
                )],
            )
            .await
            .expect("ok");
        assert!(!res[0].passed, "pattern presente -> errori runtime");
        assert_eq!(res[0].evidence["error_lines"], json!(1));
        assert!(res[0].evidence["verdict"].is_string());
    }

    #[sqlx::test]
    async fn no_orphan_imported_e_inconclusive_e_non_blocca(pool: PgPool) {
        let exec = FakeToolExecutor::with(&[]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "no_orphan_imported",
                    json!({ "staging_dir": "figma_export" }),
                    json!({ "mounted": true }),
                )],
            )
            .await
            .expect("ok");
        // inconclusive -> passed=true (il gate non lo conteggia come fallimento).
        assert!(res[0].passed);
        assert_eq!(res[0].evidence["inconclusive"], json!(true));
    }

    #[sqlx::test]
    async fn tool_capability_passa_se_history_ha_tool_call(pool: PgPool) {
        // Regressione: il final_gate puo' vedere tools_json vuoto/assente su
        // resume/fan-in, ma la history porta tool_use gia' osservati. Quel segnale
        // strutturato dimostra che il catalogo era disponibile nel run.
        let exec = FakeToolExecutor::with(&[]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "tool_capability",
                    json!({ "tools_count": 0, "has_tool_calls": true }),
                    json!({ "capable": true }),
                )],
            )
            .await
            .expect("ok");
        assert!(res[0].passed, "history tool_use conferma capacita' tool");
        assert_eq!(res[0].evidence["tools_count"], json!(0));
        assert_eq!(res[0].evidence["has_tool_calls"], json!(true));
    }

    #[sqlx::test]
    async fn tool_capability_fallisce_senza_catalogo_ne_history(pool: PgPool) {
        let exec = FakeToolExecutor::with(&[]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "tool_capability",
                    json!({ "tools_count": 0, "has_tool_calls": false }),
                    json!({ "capable": true }),
                )],
            )
            .await
            .expect("ok");
        assert!(
            !res[0].passed,
            "zero catalogo e zero tool_use resta misconfigurazione"
        );
    }


    #[sqlx::test]
    async fn file_exists_trova_basename_nel_listing(pool: PgPool) {
        let exec =
            FakeToolExecutor::with(&[("list_files", &["- README.md\n- src/\n- variables.txt"])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "file_exists",
                    json!({ "path": "variables.txt" }),
                    json!({}),
                )],
            )
            .await
            .expect("ok");
        assert!(res[0].passed, "basename presente nel listing -> esiste");
        assert_eq!(res[0].evidence["exists"], json!(true));
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn outputs_exist_na_senza_step(pool: PgPool) {
        let run = crate::test_support::seed_agent_run(&pool).await;
        let exec = FakeToolExecutor::with(&[]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "outputs_exist",
                    json!({ "run_id": run.to_string() }),
                    json!({}),
                )],
            )
            .await
            .expect("ok");
        // Nessuno step mutativo -> N/A (pass).
        assert!(res[0].passed);
        assert!(res[0].evidence["skipped"].is_string());
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn outputs_exist_fallisce_se_output_assente(pool: PgPool) {
        let run = crate::test_support::seed_agent_run(&pool).await;
        // Step write_file su "nuovo.rs": il file NON e' nel listing -> missing.
        sqlx::query(
            "INSERT INTO agent_steps (id, run_id, step_index, tool_name, tool_input, status) \
             VALUES (gen_random_uuid(), $1, 1000, 'write_file', $2, 'completed')",
        )
        .bind(run)
        .bind(json!({ "path": "nuovo.rs" }))
        .execute(&pool)
        .await
        .expect("insert step");

        // list_files della parent (".") NON contiene nuovo.rs.
        let exec = FakeToolExecutor::with(&[("list_files", &["- vecchio.rs\n- README.md"])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "outputs_exist",
                    json!({ "run_id": run.to_string() }),
                    json!({}),
                )],
            )
            .await
            .expect("ok");
        assert!(!res[0].passed, "output dichiarato assente -> fallisce");
        assert_eq!(res[0].evidence["missing"], json!(["nuovo.rs"]));
    }

    #[sqlx::test]
    async fn tipo_sconosciuto_fallisce_con_error(pool: PgPool) {
        let exec = FakeToolExecutor::with(&[]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec("inventato", json!({}), json!({}))],
            )
            .await
            .expect("ok");
        assert!(!res[0].passed);
        assert!(res[0].evidence["error"].is_string());
    }


    // ── helper puri ───────────────────────────────────────────────────────────

    #[test]
    fn line_contains_basename_token_isolato() {
        assert!(line_contains_basename("- variables.txt", "variables.txt"));
        assert!(line_contains_basename("\"variables.txt\"", "variables.txt"));
        assert!(line_contains_basename("dir/variables.txt", "variables.txt"));
        // NON deve matchare un substring di un nome piu' lungo.
        assert!(!line_contains_basename(
            "- variables.txt.bak",
            "variables.txt"
        ));
        assert!(!line_contains_basename(
            "- myvariables.txt",
            "variables.txt"
        ));
    }

    #[test]
    fn truncate_chars_non_spezza_utf8() {
        assert_eq!(truncate_chars("abc", 10), "abc");
        assert_eq!(truncate_chars("abcdef", 3), "abc");
        // caratteri multibyte: taglio per char, non per byte.
        assert_eq!(truncate_chars("àèìòù", 2), "àè");
    }

    // ── criteri strutturali (ADR 0018 leva 3): tabella pass/fail ─────────────

    #[test]
    fn check_action_requested_tabella() {
        use serde_json::json;
        // (action_oriented, has_productive_action) -> passed
        let cases = [
            (false, false, true), // nessuna azione richiesta -> passa
            (false, true, true),
            (true, true, true),   // azione richiesta ED eseguita -> passa
            (true, false, false), // azione richiesta MAI eseguita -> fallisce
        ];
        for (ao, acted, want) in cases {
            let (passed, ev) = FinalGateCriteriaRunnerAdapter::check_action_requested(&json!({
                "action_oriented": ao, "has_productive_action": acted,
            }));
            assert_eq!(passed, want, "ao={ao} acted={acted}");
            assert!(ev.get("verdict").is_some());
        }
        // Fatti assenti -> default conservativo (non action_oriented): passa.
        let (passed, _) = FinalGateCriteriaRunnerAdapter::check_action_requested(&json!({}));
        assert!(passed);
    }

    #[test]
    fn check_tool_capability_tabella() {
        use serde_json::json;
        let (ok, _) =
            FinalGateCriteriaRunnerAdapter::check_tool_capability(&json!({"tools_count": 12}));
        assert!(ok);
        let (ko, ev) =
            FinalGateCriteriaRunnerAdapter::check_tool_capability(&json!({"tools_count": 0}));
        assert!(!ko);
        assert!(ev["verdict"]
            .as_str()
            .unwrap()
            .contains("misconfigurazione"));
        // Fatto assente -> 0 -> fallisce (fail-closed: e' il gate a decidere).
        let (ko2, _) = FinalGateCriteriaRunnerAdapter::check_tool_capability(&json!({}));
        assert!(!ko2);
    }

    #[test]
    fn check_completion_confirmed_tabella() {
        use serde_json::json;
        // Qualunque outcome DICHIARATO passa (anche blocked: e' onesto).
        for o in ["done", "blocked", "partial", "needs_input"] {
            let (ok, _) = FinalGateCriteriaRunnerAdapter::check_completion_confirmed(
                &json!({"declared_outcome": o}),
            );
            assert!(ok, "outcome {o} dichiarato deve passare");
        }
        // Assente o vuoto -> fallisce con invito a task_complete.
        for spec in [
            json!({}),
            json!({"declared_outcome": null}),
            json!({"declared_outcome": "  "}),
        ] {
            let (ko, ev) = FinalGateCriteriaRunnerAdapter::check_completion_confirmed(&spec);
            assert!(!ko, "spec {spec} deve fallire");
            assert!(ev["verdict"].as_str().unwrap().contains("task_complete"));
        }
        // Delega: il PADRE non dichiara ma un sub-agente e' arrivato a chiusura
        // -> passa (la dichiarazione onesta del run e' quella del figlio).
        let (ok, ev) = FinalGateCriteriaRunnerAdapter::check_completion_confirmed(
            &json!({"declared_outcome": null, "subagent_completed": true}),
        );
        assert!(ok, "completamento subagente deve confermare la chiusura");
        assert!(ev["verdict"].as_str().unwrap().contains("sub-agente"));
        // subagent_completed=false senza dichiarazione -> continua a fallire.
        let (ko, _) = FinalGateCriteriaRunnerAdapter::check_completion_confirmed(
            &json!({"declared_outcome": null, "subagent_completed": false}),
        );
        assert!(
            !ko,
            "nessuna dichiarazione e nessun subagente completato -> fallisce"
        );
        // La dichiarazione del padre prevale sul verdict del subagente.
        let (ok, ev) = FinalGateCriteriaRunnerAdapter::check_completion_confirmed(
            &json!({"declared_outcome": "done", "subagent_completed": true}),
        );
        assert!(ok);
        assert!(ev["verdict"]
            .as_str()
            .unwrap()
            .contains("esito dichiarato: done"));
    }
}
