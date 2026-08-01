//! Adapter del trait [`nexus_agent_graph::runtime::ports::ReviewPanelPort`].
//!
//! E' il corpo dell'ex `maybe_convene_review_panel` (post-processing del
//! finalizzatore, agent_run.rs) TRASLOCATO dietro la porta del ReviewGate: la
//! review adversariale ora gira DENTRO il grafo, prima della chiusura, cosi' su
//! bocciatura il nodo puo' rimandare in correzione (il post-processing girava a
//! run gia' morto e poteva solo annotare l'esito).
//!
//! Il concreto risolve qui tutto cio' che il grafo non conosce (regola G/L):
//! settings, segnali del run (`review_gate_signals`), dimensionamento del panel
//! (`resolve_orchestration_plan_for`, punto unico), e la convocazione dei
//! sub-run revisori (`convene_review_panel`, che gia' esclude il provider del
//! padre). Ogni motivo di salto e' un [`ReviewSkipReason`] STRUTTURATO
//! (regola M): il nodo decide sull'enum, mai sul testo.

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use nexus_agent_graph::decisions::orchestration_sizing::TaskComplexity;
use nexus_agent_graph::runtime::ports::{
    PortError, ReviewPanelPort, ReviewPanelReport, ReviewPanelRequest, ReviewSkipReason,
};

use crate::tool_runner_server::{ToolRunnerDeps, ToolRunnerService};

pub struct ReviewPanelAdapter {
    /// Pool META (settings, regola G).
    db: PgPool,
    /// Pool del PROGETTO (agent_steps, deadline del run).
    steps_pool: PgPool,
    /// Dipendenze per costruire l'`AgentToolContext` dei sub-run revisori.
    deps: ToolRunnerDeps,
    session_id: Uuid,
    /// Dimensionamento (dal classifier del turno): stringe il panel a valle.
    sizing_complexity: Option<TaskComplexity>,
    sizing_scope_system_wide: bool,
}

impl ReviewPanelAdapter {
    /// Costruisce l'adapter con le dipendenze gia' risolte dal call site.
    pub fn new(
        db: PgPool,
        steps_pool: PgPool,
        deps: ToolRunnerDeps,
        session_id: Uuid,
        sizing_complexity: Option<TaskComplexity>,
        sizing_scope_system_wide: bool,
    ) -> Self {
        Self {
            db,
            steps_pool,
            deps,
            session_id,
            sizing_complexity,
            sizing_scope_system_wide,
        }
    }
}

impl ReviewPanelAdapter {
    /// Guardie del gate: setting spento, nessun file di codice modificato, o
    /// review gia' avvenuta (marker della direttiva LLM -- SOLO al primo
    /// passaggio: al secondo, la ri-review dopo correzione non va soppressa).
    /// `Ok(files)` = si procede coi file modificati.
    async fn gate_skips(&self, run_id: Uuid, cycle: i64) -> Result<Vec<String>, ReviewSkipReason> {
        if !nexus_auth::get_bool_setting(&self.db, "orchestrator.review_panel_autoconvene_enabled")
            .await
            .ok()
            .flatten()
            .unwrap_or(true)
        {
            return Err(ReviewSkipReason::AutoconveneDisabled);
        }
        let (modified, already_reviewed) =
            crate::chat_messages::agent_run::review_gate_signals(&self.steps_pool, run_id).await;
        if modified.is_empty() {
            return Err(ReviewSkipReason::NoCodeChanges);
        }
        if already_reviewed && cycle <= 1 {
            return Err(ReviewSkipReason::AlreadyReviewed);
        }
        Ok(modified)
    }

    /// La lente di interfaccia va convocata per questa review?
    ///
    /// Decide dai FILE MODIFICATI dal run (regola M), non dal testo del task:
    /// un task che diceva "sistema le spese" puo' aver riscritto tre pagine.
    /// Vocabolario dal DB (regola G); la REGOLA di riconoscimento e' il punto
    /// unico `decisions::ui_surface`. Spenta se manca il suo interruttore: un
    /// revisore in piu' non si accende da solo.
    async fn lente_ui_attiva(&self, modified: &[String]) -> bool {
        if !nexus_auth::get_bool_setting(&self.db, "orchestrator.review_ui_lens_enabled")
            .await
            .ok()
            .flatten()
            .unwrap_or(false)
        {
            return false;
        }
        let suffissi =
            nexus_auth::get_csv_setting(&self.db, "orchestrator.review_ui_suffixes").await;
        let segmenti =
            nexus_auth::get_csv_setting(&self.db, "orchestrator.review_ui_path_segments").await;
        nexus_agent_graph::decisions::ui_surface::tocca_interfaccia(modified, &suffissi, &segmenti)
    }

    /// Policy del quorum dalle chiavi gia' esistenti (mig 0571/0572).
    async fn quorum_policy(&self) -> nexus_agent_graph::decisions::QuorumPolicy {
        nexus_agent_graph::decisions::QuorumPolicy {
            min_valid_verdicts: nexus_auth::get_setting(
                &self.db,
                "orchestrator.review_quorum_min_valid",
            )
            .await
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(1)
            .max(1),
            fail_on_high_severity: nexus_auth::get_bool_setting(
                &self.db,
                "orchestrator.review_fail_on_high_severity",
            )
            .await
            .ok()
            .flatten()
            .unwrap_or(true),
            min_severity_per_rimando: Self::soglia_rimando(&self.db).await,
        }
    }

    /// Gravita' minima perche' un `needs_changes` valga come rimando in
    /// correzione (`orchestrator.review_min_severity_per_rimando`, mig 0668).
    ///
    /// Una gravita' non riconosciuta NON diventa il default in silenzio: il
    /// vocabolario e' chiuso (`alta|media|bassa`) e un valore fuori vocabolario
    /// e' un errore di configurazione da vedere nei log, non una soglia che
    /// qualcuno crede di aver impostato.
    async fn soglia_rimando(db: &sqlx::PgPool) -> nexus_agent_graph::decisions::severity::Severity {
        use nexus_agent_graph::decisions::severity::Severity;
        const CHIAVE: &str = "orchestrator.review_min_severity_per_rimando";
        let default = Severity::Medium;
        let Some(raw) = nexus_auth::get_setting(db, CHIAVE).await else {
            return default;
        };
        match Severity::try_parse(&raw) {
            Some(s) => s,
            None => {
                tracing::warn!(
                    chiave = CHIAVE,
                    valore = %raw,
                    "gravita' fuori vocabolario (alta|media|bassa): uso il default"
                );
                default
            }
        }
    }

    /// Numero di revisori: re-risoluzione del piano coi budget RESIDUI reali
    /// (punto unico del pre-run, regola L). `Err(SizedToZero)` se il
    /// dimensionamento azzera il panel; backstop DB se il piano non risolve.
    async fn panel_size(
        &self,
        run_id: Uuid,
        req: &ReviewPanelRequest,
    ) -> Result<usize, ReviewSkipReason> {
        let backstop = nexus_auth::get_setting(&self.db, "orchestrator.review_panel_size")
            .await
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(2)
            .max(1);
        let time_remaining = crate::agent_tools::subagent_native::run_time_remaining_s(
            &self.db,
            &self.steps_pool,
            run_id,
        )
        .await;
        match crate::chat_messages::agent_run::resolve_orchestration_plan_for(
            &self.db,
            self.sizing_complexity,
            self.sizing_scope_system_wide,
            false,
            req.cost_spent_usd,
            time_remaining,
        )
        .await
        {
            Some(plan) if plan.review_panel_size == 0 => {
                tracing::info!(
                    run_id = %run_id,
                    sized_by = plan.sized_by.as_str(),
                    "review gate: panel azzerato dal dimensionamento (budget residuo)"
                );
                Err(ReviewSkipReason::SizedToZero)
            }
            Some(plan) => Ok(plan.review_panel_size),
            None => Ok(backstop),
        }
    }
}

#[async_trait]
impl ReviewPanelPort for ReviewPanelAdapter {
    async fn review(&self, req: ReviewPanelRequest) -> Result<ReviewPanelReport, PortError> {
        let run_id = Uuid::parse_str(&req.run_id)
            .map_err(|e| PortError::Tool(format!("run_id non valido: {e}").into()))?;

        let modified = match self.gate_skips(run_id, req.cycle).await {
            Ok(files) => files,
            Err(skip) => return Ok(ReviewPanelReport::Skipped(skip)),
        };

        // Ctx ancorato al RUN che convoca la review, non alla sola sessione
        // (punto unico `build_ctx_for_primary_run`, regola L). Con `build_ctx` i
        // revisori nascevano orfani: `nexus_subagent_runs.dispatcher_run_id`
        // finiva sull'id della SESSIONE, che non e' un run, e da li' il lavoro
        // dei revisori non era piu' attribuibile al run che lo aveva ordinato —
        // la barra costo-per-provider ometteva il loro provider intero (misurato:
        // 4 cicli di review su openrouter, 21 iterazioni, $0.008453). Lo stesso
        // id governa il cost-cap e il residuo di deadline dei figli.
        let svc = ToolRunnerService::new(self.deps.clone());
        let ctx = svc
            .build_ctx_for_primary_run(self.session_id, run_id)
            .await
            .map_err(|e| PortError::Tool(format!("build_ctx fallita: {e}").into()))?;

        let policy = self.quorum_policy().await;
        let reviewers = match self.panel_size(run_id, &req).await {
            Ok(n) => n,
            Err(skip) => return Ok(ReviewPanelReport::Skipped(skip)),
        };

        let files_line = modified
            .iter()
            .map(|f| format!("- {f}"))
            .collect::<Vec<_>>()
            .join("\n");
        let task = format!(
            "Rivedi le modifiche al codice appena applicate dal run corrente. File modificati:\n\
             {files_line}\n\nLeggi questi file, verifica correttezza, sicurezza, edge case e \
             regressioni, e dichiara il verdetto con review_verdict."
        );
        let lente_ui = self.lente_ui_attiva(&modified).await;
        tracing::info!(
            run_id = %run_id,
            reviewers,
            files = modified.len(),
            cycle = req.cycle,
            lente_ui,
            "review gate: convocazione del panel (dentro il grafo)"
        );
        match crate::agent_tools::subagent_native::convene_review_panel(
            &ctx, &task, reviewers, &policy, lente_ui,
        )
        .await
        {
            Some(panel) => Ok(ReviewPanelReport::Convened(panel)),
            None => Ok(ReviewPanelReport::Skipped(ReviewSkipReason::NoValidVerdict)),
        }
    }
}
