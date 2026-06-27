//! Adapter del trait [`nexus_agent_graph::runtime::ports::CriteriaRunner`].
//!
//! Implementa `CriteriaRunner::run` eseguendo i criteri generali del final gate
//! (parita' con `brain/agents/criteria_runner.py`). I criteri che orchestrano
//! tool delegano al PUNTO UNICO dell'esecuzione tool ([`ToolExecutor`], regola L):
//! NON ricostruiscono il dispatch. In modalita' shadow i criteri girano in
//! [`ExecMode::Replay`] (rileggono i tool_result del primario = zero side-effect):
//! il `mode` e' propagato INVARIATO a ogni `ToolExecutor::execute`.
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
    CriteriaRunner, CriterionResult, CriterionSpec, ExecMode, PortError, ToolCall, ToolExecutor,
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
    /// `list_files` delegano qui invece di ricostruire il dispatch. Il `mode` e'
    /// propagato a ogni `execute` (Real esegue, Replay rilegge il primario).
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
        mode: ExecMode,
    ) -> Result<String, PortError> {
        let call = ToolCall {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            input,
        };
        let outcome = self.tool_executor.execute(call, mode).await?;
        // content e' tipicamente una stringa (output del tool); normalizziamo.
        Ok(match outcome.content {
            Value::String(s) => s,
            other => other.to_string(),
        })
    }

    /// Esegue UN criterio. Parita' col `run_criterion` Python: il dispatch per
    /// `criterion_type` + il try/except che mappa un fallimento su
    /// `passed=false`/`evidence.error` (mai un panico). Il [`PortError`] risale solo
    /// per un guasto infrastrutturale del runner (es. DB).
    async fn run_one(
        &self,
        c: &CriterionSpec,
        mode: ExecMode,
    ) -> Result<CriterionResult, PortError> {
        let timeout_s = c.timeout_s.unwrap_or(DEFAULT_TIMEOUT_S);
        let (passed, mut evidence) = match c.criterion_type.as_str() {
            "run_command" => self.check_run_command(&c.spec, &c.expected, mode, timeout_s).await,
            "service_logs_clean" => {
                self.check_service_logs_clean(&c.spec, mode, timeout_s).await
            }
            "http" => self.check_http(&c.spec, &c.expected, mode, timeout_s).await,
            "file_exists" => self.check_file_exists(&c.spec, &c.expected, mode, timeout_s).await,
            "outputs_exist" => self.check_outputs_exist(&c.spec, mode, timeout_s).await?,
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

    async fn check_run_command(
        &self,
        spec: &Value,
        expected: &Value,
        mode: ExecMode,
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
        // Delega al ToolExecutor (Real esegue, Replay rilegge). Un guasto di porta
        // -> evidence.error (parita' col try/except Python), non propaga.
        let raw = match self.run_tool("run_command", tool_input, mode).await {
            Ok(s) => s,
            Err(e) => return (false, json!({ "error": format!("execute_tool: {e}"), "command": cmd })),
        };
        // exit_code STRUTTURATO dal punto unico (stesso parser del path gRPC).
        let actual_exit = crate::tool_runner_server::extract_exit_code(&raw);
        let expected_exit = expected.get("exit_code").and_then(Value::as_i64).unwrap_or(0);
        let exit_ok = actual_exit.map(|a| a as i64) == Some(expected_exit);

        // RETE DI SICUREZZA (regola H): alcuni build ESCONO 0 anche quando il
        // bundling FALLISCE (es. `vite build` con "Could not resolve" / "error
        // during build" che in certe config esce 0). Affidarsi al solo exit_code
        // -> falso verde -> il final_gate chiude "completed" un'app rotta
        // (incidente Beauty-Book). Se l'output contiene pattern di errore di
        // build (punto unico count_build_errors, regola L) il criterio FALLISCE
        // comunque, anche con exit 0.
        let build_errors = nexus_agent_graph::count_build_errors(&raw);
        let passed = exit_ok && build_errors == 0;

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
        });
        // Exit-code bugiardo: exit ok ma errori nell'output -> esplicita il motivo
        // per l'agente (altrimenti non sarebbe ovvio perche' il gate ha fallito).
        if exit_ok && build_errors > 0 {
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
        mode: ExecMode,
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
        let raw = match self.run_tool("run_command", json!({ "command": cmd }), mode).await {
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
        mode: ExecMode,
        timeout_s: f64,
    ) -> (bool, Value) {
        // SHADOW-SAFETY (FIX FINDING F2c-2): il criterio `http` esegue reqwest
        // verso un endpoint REALE — un side-effect che NON passa per il
        // ToolExecutor (che in Replay rilegge senza eseguire). In modalita'
        // Replay (run shadow read-only) NON deve partire alcuna chiamata: si
        // ritorna inconclusive (passed=true, non conteggiato dal gate, parita'
        // col trattamento di un criterio non valutabile), con evidence che
        // dichiara lo skip. Cosi' lo shadow resta a ZERO side-effect anche sui
        // criteri side-effect-ful estranei al ToolExecutor.
        if matches!(mode, ExecMode::Replay) {
            return (
                true,
                json!({
                    "inconclusive": true,
                    "skipped_reason": "criterio http saltato in modalita' Replay (shadow): nessuna chiamata di rete reale",
                }),
            );
        }
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
            Err(_) => return (false, json!({ "error": format!("metodo HTTP invalido: {method}"), "url": url })),
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
            Err(e) => return (false, json!({ "error": format!("http call: {e}"), "url": url })),
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
        mode: ExecMode,
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
            .run_tool("list_files", json!({ "directory": parent_dir }), mode)
            .await
        {
            Ok(s) => s,
            Err(e) => return (false, json!({ "error": format!("execute_tool: {e}"), "path": path })),
        };
        let head = raw.chars().take(80).collect::<String>().to_lowercase();
        let list_error = raw.starts_with('\u{274C}')
            || head.contains("[errore")
            || head.contains("[error")
            || head.contains("non trovato")
            || head.contains("not found");
        if list_error {
            // list_files in errore: trattiamo come "non esiste" (parita' fallback).
            let expected_exists = expected.get("exists").and_then(Value::as_bool).unwrap_or(true);
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
        let exists = raw.lines().any(|line| line_contains_basename(line, basename));
        let expected_exists = expected.get("exists").and_then(Value::as_bool).unwrap_or(true);
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
        mode: ExecMode,
        timeout_s: f64,
    ) -> Result<(bool, Value), PortError> {
        let run_id = spec.get("run_id").and_then(Value::as_str).unwrap_or("").trim();
        if run_id.is_empty() {
            return Ok((true, json!({ "skipped": "outputs_exist senza run_id: N/A" })));
        }
        let run_uuid = match Uuid::parse_str(run_id) {
            Ok(u) => u,
            Err(_) => return Ok((true, json!({ "skipped": "outputs_exist run_id non-UUID: N/A" }))),
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
        .map_err(|e| PortError::Tool(format!("outputs_exist lettura agent_steps: {e}")))?;

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
            return Ok((true, json!({ "skipped": "nessuno step mutativo file nel run: N/A" })));
        }
        let mut missing: Vec<String> = Vec::new();
        let mut checked: Vec<String> = Vec::new();
        // Cap difensivo (parita' Python: 20 output).
        for p in paths.iter().take(20) {
            let (ok, _ev) = self
                .check_file_exists(&json!({ "path": p }), &json!({}), mode, timeout_s)
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
        mode: ExecMode,
    ) -> Result<Vec<CriterionResult>, PortError> {
        let mut out = Vec::with_capacity(criteria.len());
        for c in &criteria {
            out.push(self.run_one(c, mode).await?);
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
    /// registra le chiamate ricevute (per asserire il `mode` propagato e gli args).
    struct FakeToolExecutor {
        /// risultati per tool_name (in coda: la N-esima chiamata pop dell'indice N).
        results: std::collections::HashMap<String, Vec<String>>,
        /// log delle chiamate (name, mode) per le asserzioni.
        calls: StdMutex<Vec<(String, ExecMode)>>,
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
            mode: ExecMode,
        ) -> Result<nexus_agent_graph::runtime::ports::ToolOutcome, PortError> {
            self.calls.lock().unwrap().push((call.name.clone(), mode));
            let idx = self
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(n, _)| n == &call.name)
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
                ExecMode::Real,
            )
            .await
            .expect("ok");
        assert_eq!(res.len(), 1);
        assert!(res[0].passed, "exit 0 == expected 0 -> passed");
        assert_eq!(res[0].evidence["exit_code"], json!(0));
        assert!(res[0].evidence["output_total_chars"].is_number());
        assert_eq!(res[0].evidence["type"], json!("run_command"));
    }

    #[sqlx::test]
    async fn run_command_build_fallisce_su_exit_non_zero(pool: PgPool) {
        let exec = FakeToolExecutor::with(&[(
            "run_command",
            &["error[E0432]\nEXIT CODE: 101\nboom"],
        )]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "cargo check" }),
                    json!({ "exit_code": 0 }),
                )],
                ExecMode::Real,
            )
            .await
            .expect("ok");
        assert!(!res[0].passed, "exit 101 != 0 -> build fallito");
        assert_eq!(res[0].evidence["exit_code"], json!(101));
        // output_excerpt presente (render_failed_block ramo build lo legge).
        assert!(res[0].evidence["output_excerpt"].is_string());
    }

    #[sqlx::test]
    async fn run_command_propaga_mode_replay(pool: PgPool) {
        let exec = FakeToolExecutor::with(&[("run_command", &["EXIT CODE: 0"])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec.clone(), pool);
        runner
            .run(
                vec![spec("run_command", json!({ "command": "x" }), json!({ "exit_code": 0 }))],
                ExecMode::Replay,
            )
            .await
            .expect("ok");
        // Il mode Replay e' arrivato INVARIATO al ToolExecutor (zero side-effect).
        let calls = exec.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], ("run_command".to_string(), ExecMode::Replay));
    }

    #[sqlx::test]
    async fn service_logs_clean_passa_senza_pattern_hit(pool: PgPool) {
        let exec = FakeToolExecutor::with(&[(
            "run_command",
            &["INFO server avviato\nINFO richiesta ok"],
        )]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "service_logs_clean",
                    json!({ "command": "docker logs app", "patterns": ["does not exist", "ERROR"] }),
                    json!({}),
                )],
                ExecMode::Real,
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
                ExecMode::Real,
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
                ExecMode::Real,
            )
            .await
            .expect("ok");
        // inconclusive -> passed=true (il gate non lo conteggia come fallimento).
        assert!(res[0].passed);
        assert_eq!(res[0].evidence["inconclusive"], json!(true));
    }

    #[sqlx::test]
    async fn http_in_replay_e_inconclusive_senza_chiamata(pool: PgPool) {
        // FIX FINDING F2c-2: il criterio http in modalita' Replay (shadow) NON
        // deve eseguire reqwest. Si esercita con un URL irraggiungibile: se la
        // chiamata partisse il criterio fallirebbe (passed=false, evidence.error);
        // invece deve ritornare inconclusive (passed=true) senza alcuna rete.
        let exec = FakeToolExecutor::with(&[]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec(
                    "http",
                    json!({ "url": "http://127.0.0.1:1/never-reached" }),
                    json!({ "status": 200 }),
                )],
                ExecMode::Replay,
            )
            .await
            .expect("ok");
        assert!(res[0].passed, "Replay -> inconclusive (passed=true, non conteggiato)");
        assert_eq!(res[0].evidence["inconclusive"], json!(true));
        // Nessuna evidence di una chiamata avvenuta (no status, no error di rete).
        assert!(res[0].evidence.get("status").is_none());
        assert!(res[0].evidence.get("error").is_none());
    }

    #[sqlx::test]
    async fn file_exists_trova_basename_nel_listing(pool: PgPool) {
        let exec = FakeToolExecutor::with(&[(
            "list_files",
            &["- README.md\n- src/\n- variables.txt"],
        )]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec("file_exists", json!({ "path": "variables.txt" }), json!({}))],
                ExecMode::Real,
            )
            .await
            .expect("ok");
        assert!(res[0].passed, "basename presente nel listing -> esiste");
        assert_eq!(res[0].evidence["exists"], json!(true));
    }

    #[sqlx::test]
    async fn outputs_exist_na_senza_step(pool: PgPool) {
        // agent_steps tabella minimale per la query.
        create_steps_tables(&pool).await;
        let run = Uuid::new_v4();
        let exec = FakeToolExecutor::with(&[]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool);
        let res = runner
            .run(
                vec![spec("outputs_exist", json!({ "run_id": run.to_string() }), json!({}))],
                ExecMode::Real,
            )
            .await
            .expect("ok");
        // Nessuno step mutativo -> N/A (pass).
        assert!(res[0].passed);
        assert!(res[0].evidence["skipped"].is_string());
    }

    #[sqlx::test]
    async fn outputs_exist_fallisce_se_output_assente(pool: PgPool) {
        create_steps_tables(&pool).await;
        let run = Uuid::new_v4();
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
                vec![spec("outputs_exist", json!({ "run_id": run.to_string() }), json!({}))],
                ExecMode::Real,
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
            .run(vec![spec("inventato", json!({}), json!({}))], ExecMode::Real)
            .await
            .expect("ok");
        assert!(!res[0].passed);
        assert!(res[0].evidence["error"].is_string());
    }

    async fn create_steps_tables(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE agent_steps ( \
                 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                 run_id UUID NOT NULL, \
                 step_index INT NOT NULL, \
                 tool_name TEXT NOT NULL, \
                 tool_input JSONB NOT NULL, \
                 tool_result TEXT, \
                 status TEXT NOT NULL DEFAULT 'running', \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW() \
             )",
        )
        .execute(pool)
        .await
        .expect("create agent_steps");
    }

    // ── helper puri ───────────────────────────────────────────────────────────

    #[test]
    fn line_contains_basename_token_isolato() {
        assert!(line_contains_basename("- variables.txt", "variables.txt"));
        assert!(line_contains_basename("\"variables.txt\"", "variables.txt"));
        assert!(line_contains_basename("dir/variables.txt", "variables.txt"));
        // NON deve matchare un substring di un nome piu' lungo.
        assert!(!line_contains_basename("- variables.txt.bak", "variables.txt"));
        assert!(!line_contains_basename("- myvariables.txt", "variables.txt"));
    }

    #[test]
    fn truncate_chars_non_spezza_utf8() {
        assert_eq!(truncate_chars("abc", 10), "abc");
        assert_eq!(truncate_chars("abcdef", 3), "abc");
        // caratteri multibyte: taglio per char, non per byte.
        assert_eq!(truncate_chars("àèìòù", 2), "àè");
    }
}
