//! Adapter del trait [`nexus_agent_graph::runtime::ports::ToolExecutor`].
//!
//! Implementa `ToolExecutor::execute` delegando al dispatch tool IN-PROCESS di
//! mcp-core ([`crate::agent_tools::execute_agent_tool`]) costruendo l'`AgentToolContext`
//! col PUNTO UNICO [`ToolRunnerService::build_ctx`]. ATTENZIONE: mcp-core *e'* il
//! ToolRunner — NON ci si chiama via gRPC (sarebbe un loop di rete su se' stessi);
//! si esegue la stessa funzione di dispatch del server gRPC, in processo. I tool
//! sono REALI (side-effect possibili sul progetto).
//!
//! ESITO STRUTTURATO (regola L): `is_error` e `exit_code` sono derivati dal testo
//! del risultato col PUNTO UNICO di mcp-core ([`crate::tool_runner_server::
//! tool_result_is_error`] / [`crate::tool_runner_server::extract_exit_code`]),
//! gli stessi usati dal path gRPC. L'`exit_code` fluisce INVARIATO nel
//! [`ToolOutcome`] (alimenta `routing::signals::tool_result_outcome_after`).
//!
//! GUASTO INFRA vs ERRORE APPLICATIVO (caso "gRPC-down -> degrada a executor",
//! WAVE 2.2: mcp-core NON scala il provider su un guasto infra):
//! - un guasto della COSTRUZIONE del ctx (sessione non risolvibile, DB down =
//!   ToolRunner non operativo) e' propagato come [`PortError::Tool`]: il chiamante
//!   (nodo) lo mappa a degrado, senza scalare provider;
//! - quando invece il tool produce un risultato APPLICATIVO (anche un errore col
//!   marker `\u{274C}`), il [`ToolOutcome`] ha `is_infrastructure=false` (il
//!   ToolRunner ha risposto). Nel dispatch IN-PROCESS attuale non c'e' un piano
//!   "tool eseguito ma con esito infra", quindi `is_infrastructure` resta `false`
//!   sugli `Ok`; il segnale infra viaggia via `PortError`.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{PortError, ToolCall, ToolExecutor, ToolOutcome};
use nexus_types::error_presentation::RenderedError;

use crate::agent_tools::execute_agent_tool;
use crate::tool_runner_server::{
    extract_exit_code, tool_result_is_error, ToolRunnerDeps, ToolRunnerService,
};

/// Adapter [`ToolExecutor`] -> dispatch tool IN-PROCESS.
pub struct ToolRunnerExecutorAdapter {
    /// Dipendenze del ToolRunner concreto (db, neural, channels...): costruzione
    /// ctx + esecuzione tool.
    deps: ToolRunnerDeps,
    /// Sessione chat del run corrente: risolve project/root/permessi per il ctx.
    session_id: Uuid,
    /// Override della root di lavoro per un SUB-RUN ISOLATO (FASE 2 orchestrazione:
    /// git worktree effimero proprio del sub-run). Campo IMMUTABILE del run,
    /// passato a `execute` -> `build_ctx_with_root`. `None` (default) -> il ctx usa
    /// la root del progetto risolta dalla sessione: comportamento invariato.
    working_root: Option<PathBuf>,
    /// Aree file dichiarate dal pianificatore per il task di questo sub-run.
    /// Campo IMMUTABILE del run, passato a `execute` -> `build_ctx_with_root` ->
    /// ctx, dove l'hook delle mutazioni MISURA quante scritture cadono fuori.
    /// Vuoto (default per il run principale e per ogni dispatch fuori dal percorso
    /// a passi di piano) -> le mutazioni risultano `no_scope_declared`.
    write_scope: Vec<String>,
    /// Narrazione del run invocante (run_id + canale SSE del run del grafo che
    /// esegue i tool): iniettata nel ctx dei tool, cosi' i tool a lunga durata
    /// (dispatch_subagents) possono emettere meta-step sul run padre mentre
    /// lavorano. `None` fuori dal grafo.
    parent_narration: Option<crate::agent_tools::context::ParentNarration>,
}

impl ToolRunnerExecutorAdapter {
    /// Costruisce l'adapter per il run corrente.
    ///
    /// - `session_id`: sessione chat (risolve il ctx);
    /// - `working_root`: override root del sub-run ISOLATO (FASE 2). `None`
    ///   (default per ogni run non isolato) -> ctx sulla root del progetto,
    ///   comportamento invariato.
    /// - `write_scope`: aree file dichiarate dal pianificatore per il task del
    ///   sub-run (misura, non vincolo). Vuoto per il run principale.
    pub fn new(
        deps: ToolRunnerDeps,
        session_id: Uuid,
        working_root: Option<PathBuf>,
        write_scope: Vec<String>,
        parent_narration: Option<crate::agent_tools::context::ParentNarration>,
    ) -> Self {
        Self {
            deps,
            session_id,
            working_root,
            write_scope,
            parent_narration,
        }
    }
}

#[async_trait]
impl ToolExecutor for ToolRunnerExecutorAdapter {
    /// Esegue REALMENTE il tool in processo (side-effect possibili) e mappa il
    /// risultato testuale nel [`ToolOutcome`] strutturato.
    async fn execute(&self, call: ToolCall) -> Result<ToolOutcome, PortError> {
        // Ctx col PUNTO UNICO del server gRPC (stesso root/permessi/reindex). Un
        // fallimento qui (sessione non risolvibile, DB down) e' INFRASTRUTTURALE:
        // il ToolRunner non e' operativo -> is_infrastructure (degrada a executor,
        // niente scalata provider).
        let svc = ToolRunnerService::new(self.deps.clone());
        // PUNTO UNICO di costruzione ctx (regola L): con `working_root=None`
        // (default) e' identico a `build_ctx(session_id)` — stessa root del
        // progetto, `isolated_subrun=false`. Con un override il ctx punta al
        // worktree effimero del sub-run e sopprime autocommit/reindex.
        let mut ctx = svc
            .build_ctx_with_root(
                self.session_id,
                self.working_root.as_deref(),
                &self.write_scope,
            )
            .await
            // Il `Display` di `tonic::Status` stampa la struttura INTERA —
            // `status: Unavailable, message: "...", details: [], metadata:
            // MetadataMap { headers: {...} }` — ed e' il testo che l'utente
            // trovava nel tool_result del nastro attivita'. I segnali che
            // servono sono due e sono strutturati: `code()` e `message()`
            // (regola M). Il resto resta nel detail.
            .map_err(|status| PortError::Tool(rendered_from_status(&status)))?;
        // Inietta la narrazione del run invocante (run_id + canale SSE del run
        // del grafo): i tool a lunga durata (dispatch_subagents) la usano per
        // emettere meta-step sul run padre mentre lavorano.
        ctx.parent_narration = self.parent_narration.clone();
        // Run CORRENTE del grafo (quello che il motore SOSPENDE su
        // `awaiting_subagents` e che il fan-in deve RIPRENDERE): lo porta la
        // narrazione del run invocante (unica fonte del run_id).
        // `dispatch_subagents` background lo usa per accodare il PARENT giusto
        // nella coda fan-in (non session_id/parent_anchor).
        ctx.core.run_id = self.parent_narration.as_ref().map(|n| n.run_id);

        // Esecuzione IN-PROCESS: la STESSA funzione del dispatch gRPC, non una
        // chiamata di rete a se' stessi (regola: mcp-core E' il ToolRunner).
        let result = execute_agent_tool(&ctx, &call.name, &call.input).await;

        Ok(map_result_to_outcome(&call.id, result))
    }
}

/// Traduce un [`tonic::Status`] nei suoi due segnali strutturati.
///
/// `code()` e' un enum (`Unavailable`, `DeadlineExceeded`, ...) e `message()` la
/// riga scritta da chi ha costruito lo Status: sono le sole due cose che
/// servono. Il `Display` dell'intero Status — che stampa anche `details: []` e
/// `metadata: MetadataMap { headers: {...} }` — scende nel `detail`, dove serve
/// a chi diagnostica e non a chi legge la chat.
fn rendered_from_status(status: &tonic::Status) -> RenderedError {
    use nexus_types::error_presentation::{render_user_error, ErrorDomain, ErrorFacts};
    render_user_error(
        &ErrorFacts::opaque(ErrorDomain::Tool, format!("{status:?}"))
            .with_code(format!("{:?}", status.code()))
            // `message()` e' la frase scritta da chi ha costruito lo Status:
            // testo, non struttura. `{status:?}` — che porta `details: []` e
            // `MetadataMap { headers: {...} }` — resta nel detail.
            .with_upstream(status.message()),
    )
}

/// Mappa il risultato testuale di un tool (output di `execute_agent_tool`) nel
/// [`ToolOutcome`] strutturato.
///
/// `is_error`/`exit_code` derivano dal PUNTO UNICO di mcp-core (stesso codice del
/// path gRPC). `content` resta il testo grezzo (i nodi lo trattano come opaco).
/// `is_infrastructure=false`: un errore applicativo del tool (marker `\u{274C}`)
/// NON e' un guasto infra (il ToolRunner ha risposto). L'infra-error e' segnalato
/// a monte (build_ctx fallita -> `PortError::Tool`, mappato dal chiamante).
pub(crate) fn map_result_to_outcome(tool_call_id: &str, result: String) -> ToolOutcome {
    let is_error = tool_result_is_error(&result);
    let exit_code = extract_exit_code(&result).map(|c| c as i64);
    ToolOutcome {
        tool_call_id: tool_call_id.to_string(),
        content: Value::String(result),
        is_error,
        exit_code,
        // Errore applicativo, non infrastrutturale: il tool ha prodotto un esito.
        is_infrastructure: false,
        error_class: if is_error {
            Some("tool_error".to_string())
        } else {
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── map_result_to_outcome (punto unico esito) ─────────────────────────────

    #[test]
    fn esito_successo_con_exit_code_zero() {
        let out = map_result_to_outcome(
            "call_1",
            "hints\nEXIT CODE: 0\nSTDOUT:\nok\nSTDERR:\n".to_string(),
        );
        assert_eq!(out.tool_call_id, "call_1");
        assert!(!out.is_error);
        // exit_code STRUTTURATO estratto e propagato (alimenta tool_result_outcome_after).
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.is_infrastructure);
        assert_eq!(out.error_class, None);
    }

    #[test]
    fn esito_comando_fallito_exit_code_non_zero() {
        let out = map_result_to_outcome("c", "EXIT CODE: 1\nSTDERR:\nboom".to_string());
        // exit_code != 0: errore di comando, propagato strutturato.
        assert_eq!(out.exit_code, Some(1));
        // NB: un exit_code != 0 NON marca is_error (quello e' il marker U+274C):
        // l'esito di comando viaggia in exit_code, l'errore applicativo nel marker.
        assert!(!out.is_error);
    }

    #[test]
    fn esito_errore_applicativo_marker() {
        let out = map_result_to_outcome("c", "\u{274C} Tool 'pippo' non esiste".to_string());
        assert!(out.is_error, "marker U+274C -> is_error");
        // tool non-comando: nessun exit_code.
        assert_eq!(out.exit_code, None);
        assert_eq!(out.error_class.as_deref(), Some("tool_error"));
        // errore APPLICATIVO, non infrastrutturale.
        assert!(!out.is_infrastructure);
    }

    #[test]
    fn esito_tool_non_comando_nessun_exit_code() {
        let out = map_result_to_outcome("c", "contenuto del file letto".to_string());
        assert!(!out.is_error);
        assert_eq!(out.exit_code, None, "tool non-comando -> exit_code None");
    }
}
