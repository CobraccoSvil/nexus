//! Dispatch centrale dei tool agente: mappa nome-tool -> handler.
//!
//! Estratto da mod.rs (refactor god-file). Nessun cambiamento di routing:
//! stessi nomi tool mappati agli stessi handler.

use serde_json::Value;

use super::profile_tools::{tool_create_profile, tool_update_profile};
use super::quality_tools::{tool_batch_analyze_code, tool_scan_code_quality};
use super::semantic_tools::{
    tool_recall_context, tool_search_codebase_semantic, tool_search_file_semantic,
};
use super::{
    archive_tools, attachment_inspector, attachments, audio_tools, command, dev_diagnostics,
    dispatcher, document_tools, figma_tools, files, git, image_tools, knowledge, ports,
    project_db_query, rag_search, sandbox, scaffold_verifier, service, shadcn_setup,
    subagent_native, testing, todos, tool_not_found, ui_patterns, ui_reference_search, verify,
    video_tools, vision_tools, visual_compare, AgentToolContext,
};

/// Esegue un tool per conto dell'agente.
/// Ritorna sempre una stringa: il risultato in caso di successo, o un messaggio d'errore.
pub async fn execute_agent_tool(ctx: &AgentToolContext, name: &str, input: &Value) -> String {
    match name {
        "read_file" => files::tool_read_file(ctx, input).await,
        "read_file_lines" => files::tool_read_file_lines(ctx, input).await,
        "write_file" => files::tool_write_file(ctx, input).await,
        "list_files" => files::tool_list_files(ctx, input).await,
        "search_in_files" => files::tool_search_in_files(ctx, input).await,
        "git_status" => git::tool_git_status(ctx).await,
        "git_stage" => git::tool_git_stage(ctx, input).await,
        "git_commit" => git::tool_git_commit(ctx, input).await,
        "git_push" => git::tool_git_push(ctx).await,
        "git_pull" => git::tool_git_pull(ctx).await,
        "git_remote_add" => git::tool_git_remote_add(ctx, input).await,
        // Fix M51: tool dedicato per allocazione porta (evita curl via run_command).
        "request_port" => ports::tool_request_port(ctx, input).await,
        // Tool read-only per verifica/audit dello stato porte (bucket + allocazioni).
        "nexus_list_ports" => ports::tool_nexus_list_ports(ctx, input).await,
        // PR-1 Plan/Act/Verify: emette/aggiorna la TODO list del planner.
        "nexus_todo_write" => todos::tool_nexus_todo_write(ctx, input).await,
        // Sub-agents NATIVI (zero-Python): il sub-run gira sul grafo Rust
        // (crate::native_engine::run_native) in-process, niente piu' chiamata al
        // brain /agent/subagent-run. L'orchestrazione vive in mcp-core perche'
        // richiede native_engine (regola gerarchia crate); le guard
        // enabled/whitelist/depth/cost sono replicate DB-driven (regola G).
        "dispatch_subagent" => subagent_native::tool_dispatch_subagent(ctx, input).await,
        // Batch parallelo di sub-agent nativi (base del DAG scheduler).
        "dispatch_subagents" => subagent_native::tool_dispatch_subagents(ctx, input).await,
        // Poll (DB-only) + resume (ri-esecuzione nativa) dei sub-agent.
        "nexus_subagent_poll" => subagent_native::tool_nexus_subagent_poll(ctx, input).await,
        "nexus_subagent_resume" => subagent_native::tool_nexus_subagent_resume(ctx, input).await,
        "run_in_terminal" => service::tool_run_service(ctx, input, "task").await,
        "run_service" => service::tool_run_service(ctx, input, "service").await,
        "read_terminal_output" => service::tool_read_service_output(ctx, input).await,
        "read_service_output" => service::tool_read_service_output(ctx, input).await,
        "stop_service" => service::tool_stop_service(ctx, input).await,
        "service_restart" => service::tool_service_restart(ctx, input).await,
        "tail_service_logs" => service::tool_tail_service_logs(ctx, input).await,
        "list_active_services" => service::tool_list_active_services(ctx, input).await,
        "fs_mkdir" => files::tool_fs_mkdir(ctx, input).await,
        "fs_copy" => files::tool_fs_copy(ctx, input).await,
        "fs_move" => files::tool_fs_move(ctx, input).await,
        "run_specific_test" => testing::tool_run_specific_test(ctx, input).await,
        "run_lint_fix" => testing::tool_run_lint_fix(ctx, input).await,
        "format_file" => testing::tool_format_file(ctx, input).await,
        "delete_file" => files::tool_delete_file(ctx, input).await,
        "rename_file" => files::tool_rename_file(ctx, input).await,
        "edit_file" => files::tool_edit_file(ctx, input).await,
        "run_command" => command::tool_run_command(ctx, input).await,
        // Catena di verifica post-modifica (ADR 0019 L3): typecheck -> build ->
        // lint -> test con fail-fast e VerifyReport strutturato.
        "nexus_verify_change" => verify::tool_nexus_verify_change(ctx, input).await,
        // Tool dedicato ai cicli test-fix-test: esecuzione sincrona con
        // timeout esteso (raccomandato dai prompt al posto di run_command).
        "run_tests" => command::tool_run_tests(ctx, input).await,
        "create_profile" => tool_create_profile(ctx, input).await,
        "update_profile" => tool_update_profile(ctx, input).await,
        "set_sandbox_config" => sandbox::tool_set_sandbox_config(ctx, input).await,
        "get_sandbox_config" => sandbox::tool_get_sandbox_config(ctx).await,
        "build_project_image" => service::tool_build_project_image(ctx).await,
        "scan_code_quality" => tool_scan_code_quality(ctx, input).await,
        "search_codebase_semantic" => {
            let query = input
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let limit = input
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(8)
                .min(20) as usize;
            tool_search_codebase_semantic(ctx, &query, limit).await
        }
        "search_file_semantic" => {
            let path = input
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let query = input
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let top_k = input
                .get("top_k")
                .and_then(Value::as_u64)
                .unwrap_or(5)
                .min(10) as usize;
            let chunk_lines = input
                .get("chunk_lines")
                .and_then(Value::as_u64)
                .unwrap_or(50)
                .clamp(10, 200) as usize;
            tool_search_file_semantic(ctx, &path, &query, top_k, chunk_lines).await
        }
        "recall_context" => {
            let query = input
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let source = input
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("all")
                .to_string();
            let limit = input
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(5)
                .min(10) as usize;
            tool_recall_context(ctx, &query, &source, limit).await
        }
        "run_playwright_tests" => testing::tool_run_playwright_tests(ctx, input).await,
        "batch_analyze_code" => tool_batch_analyze_code(ctx, input).await,
        // ── Dispatcher centrale (pilotaggio pannelli) ──────────────────────
        "dispatcher_emit_event" => dispatcher::tool_dispatcher_emit_event(ctx, input).await,
        "dispatcher_post_notification" => {
            dispatcher::tool_dispatcher_post_notification(ctx, input).await
        }
        "dispatcher_set_flag" => dispatcher::tool_dispatcher_set_flag(ctx, input).await,
        "dispatcher_update_monitor" => dispatcher::tool_dispatcher_update_monitor(ctx, input).await,
        "dispatcher_highlight_panel" => {
            dispatcher::tool_dispatcher_highlight_panel(ctx, input).await
        }
        // ── Catalogo pattern di layout (trasversale ai progetti) ───────────
        "ui_layout_patterns" => ui_patterns::tool_ui_layout_patterns(&ctx.core.db, input).await,
        // Unico tool che guarda FUORI dal progetto: cio' che torna e' DATO, e
        // arriva gia' dichiarato come non fidato (vedi il modulo).
        "ui_reference_search" => {
            ui_reference_search::tool_ui_reference_search(&ctx.core, input).await
        }
        // ── Knowledge Base per-progetto ────────────────────────────────────
        "knowledge_search" => knowledge::tool_knowledge_search(ctx, input).await,
        "code_doc" => knowledge::tool_code_doc(ctx, input).await,
        "knowledge_get_note" => knowledge::tool_knowledge_get_note(ctx, input).await,
        "knowledge_create_note" => knowledge::tool_knowledge_create_note(ctx, input).await,
        // Comp.0: navigazione/modifica del grafo KB (link, sottografo, pertinenza)
        "knowledge_get_links" => knowledge::tool_knowledge_get_links(ctx, input).await,
        "knowledge_get_subgraph" => knowledge::tool_knowledge_get_subgraph(ctx, input).await,
        "knowledge_create_link" => knowledge::tool_knowledge_create_link(ctx, input).await,
        "knowledge_set_relevance" => knowledge::tool_knowledge_set_relevance(ctx, input).await,
        // Comp.2: import di grafi esterni nella KB (JSON node-link / Mermaid / DOT)
        "knowledge_import_graph" => knowledge::tool_knowledge_import_graph(ctx, input).await,
        // ── Allegati chat (ADR 0010) ───────────────────────────────────────
        "nexus_list_attachments" => attachments::tool_nexus_list_attachments(ctx, input).await,
        "nexus_read_attachment" => attachments::tool_nexus_read_attachment(ctx, input).await,
        // ── Ingestion intelligente allegati (ADR 0011) ─────────────────────
        "nexus_inspect_attachment" => {
            attachment_inspector::tool_nexus_inspect_attachment(ctx, input).await
        }
        "nexus_list_archive_entries" => {
            archive_tools::tool_nexus_list_archive_entries(ctx, input).await
        }
        "nexus_read_archive_entry" => {
            archive_tools::tool_nexus_read_archive_entry(ctx, input).await
        }
        "nexus_extract_pdf_text" => document_tools::tool_nexus_extract_pdf_text(ctx, input).await,
        "nexus_extract_docx_text" => document_tools::tool_nexus_extract_docx_text(ctx, input).await,
        "nexus_extract_xlsx_data" => document_tools::tool_nexus_extract_xlsx_data(ctx, input).await,
        "nexus_extract_figma_structure" => {
            figma_tools::tool_nexus_extract_figma_structure(ctx, input).await
        }
        "nexus_extract_figma_code" => figma_tools::tool_nexus_extract_figma_code(ctx, input).await,
        "nexus_describe_image_attachment" => {
            vision_tools::tool_nexus_describe_image_attachment(ctx, input).await
        }
        // PR6b-2: genera un'immagine dal prompt e la salva path-safe nel progetto.
        "nexus_generate_image" => image_tools::tool_nexus_generate_image(ctx, input).await,
        // PR6c: trascrive un audio allegato (speech-to-text) via gateway.
        "nexus_transcribe_audio" => audio_tools::tool_nexus_transcribe_audio(ctx, input).await,
        // PR6d: sintetizza un testo in audio (text-to-speech) e lo salva nel progetto.
        "nexus_text_to_speech" => audio_tools::tool_nexus_text_to_speech(ctx, input).await,
        // PR6e: genera un video dal prompt (text-to-video, Veo async) e lo salva nel progetto.
        "nexus_generate_video" => video_tools::tool_nexus_generate_video(ctx, input).await,
        "nexus_install_shadcn_components" => {
            shadcn_setup::tool_nexus_install_shadcn_components(ctx, input).await
        }
        "nexus_dev_server_diagnose" => {
            dev_diagnostics::tool_nexus_dev_server_diagnose(ctx, input).await
        }
        "nexus_verify_scaffold" => scaffold_verifier::tool_nexus_verify_scaffold(ctx, input).await,
        "nexus_db_query" => project_db_query::tool_nexus_db_query(ctx, input).await,
        "nexus_db_tables" => project_db_query::tool_nexus_db_tables(ctx, input).await,
        "nexus_db_describe" => project_db_query::tool_nexus_db_describe(ctx, input).await,
        // FASE 2 "resa Figma Make": verifica visiva (screenshot vs design).
        "nexus_visual_compare" => visual_compare::tool_nexus_visual_compare(ctx, input).await,
        "nexus_search_semantic" => rag_search::tool_nexus_search_semantic(ctx, input).await,
        // Worklog di sessione (mig 0411): drill-down on-demand della storia di
        // lavoro — il digest compatto sta nel system, il dettaglio vive qui.
        // VINCOLO: deve restare in _ALWAYS_ON_TOOLS (profile_loader.py) cosi'
        // il modello puo' sempre approfondire oltre il digest (contratto D8).
        "nexus_get_worklog" => crate::session_worklog::tool_nexus_get_worklog(ctx, input).await,
        // ── Nexus Builtin tool (prefisso nexus_*) ──────────────────────────
        // Dispatch verso nexus_builtin::execute_with_neural per usare
        // la ricerca semantica quando neural è disponibile (Qdrant).
        // Caso speciale: nexus_mcp_tool_call con server_id="builtin" reindirizza
        // ricorsivamente a execute_agent_tool, consentendo al modello di
        // invocare via mcp_tool_call qualsiasi tool builtin (es. quelli
        // suggeriti da next_action_recommended di nexus_inspect_attachment)
        // senza doverli avere in toolspec. Sistema lazy discovery preservato.
        "nexus_mcp_tool_call" => {
            let server_id = input
                .get("server_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if server_id.eq_ignore_ascii_case("builtin")
                || server_id == "00000000-0000-0000-0000-000000000000"
            {
                let inner_tool = input
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if inner_tool.is_empty() {
                    return serde_json::json!({
                        "error": "tool_name richiesto per nexus_mcp_tool_call con server_id=builtin"
                    })
                    .to_string();
                }
                if inner_tool == "nexus_mcp_tool_call" {
                    return serde_json::json!({
                        "error": "ricorsione builtin -> builtin non permessa"
                    })
                    .to_string();
                }
                let inner_args = input
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                return Box::pin(execute_agent_tool(ctx, inner_tool, &inner_args)).await;
            }
            crate::nexus_builtin::execute_with_neural(
                &ctx.db,
                ctx.user_id,
                ctx.project_id,
                &ctx.user_role,
                &ctx.neural,
                "nexus_mcp_tool_call",
                input.clone(),
            )
            .await
        }
        other if other.starts_with("nexus_") => {
            crate::nexus_builtin::execute_with_neural(
                &ctx.db,
                ctx.user_id,
                ctx.project_id,
                &ctx.user_role,
                &ctx.neural,
                other,
                input.clone(),
            )
            .await
        }
        // Tool non cablato: delega al punto unico tool-not-found resolver
        // (regola L). Sostituisce la tabella alias hardcoded con un LOOKUP REALE
        // (builtin fuzzy + connettori installati + catalog non installato) e
        // garantisce il marker '\u{274C}' -> is_error=true (gap1). neural=Some:
        // abilita anche il match semantico Qdrant best-effort.
        other => {
            tool_not_found::resolve_tool_not_found(
                &ctx.db,
                Some(&ctx.neural),
                ctx.user_id,
                ctx.project_id,
                &ctx.user_role,
                other,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use uuid::Uuid;

    /// Contesto minimale senza infrastruttura: pool DB lazy mai contattato,
    /// brain non connesso. Sufficiente per i path di dispatch che non
    /// toccano rete ne' DB.
    fn ctx_for_dispatch_tests(root: std::path::PathBuf) -> AgentToolContext {
        let db =
            sqlx::PgPool::connect_lazy("postgres://test:test@127.0.0.1:1/test").expect("pool lazy");
        AgentToolContext {
            core: nexus_agent_tools::ToolContextCore {
                root_path: root,
                user_id: Uuid::nil(),
                is_git_repo: false,
                can_write: true,
                project_id: Uuid::nil(),
                session_id: None,
                db: Arc::new(db.clone()),
                run_db: Arc::new(db.clone()),
                parent_run_id: None,
                run_id: None,
                long_running_patterns: Vec::new(),
                user_role: "admin".to_string(),
                is_nexus_operator: true,
                project_channels: Arc::new(dashmap::DashMap::new()),
                monitor_registry: Arc::new(parking_lot::RwLock::new(
                    std::collections::HashMap::new(),
                )),
                hooks: Arc::new(nexus_agent_tools::context_core::NoopMutationHooks),
                embedder: Arc::new(nexus_agent_tools::context_core::NoopEmbedder),
                isolated_subrun: false,
                write_scope: Vec::new(),
            },
            playwright_channels: crate::playwright_live::new_channels(),
            neural: crate::orchestrator::NeuralCoreClient::disconnected_for_tests(),
            dependency_status: Arc::new(crate::task_watchdog::DependencyStatus::new()),
            port_registry: crate::port_registry::PortRegistryCache::empty_for_tests(db),
            parent_narration: None,
        }
    }

    /// Regressione: `run_tests` e' esposto al modello (tool_schema, prompt
    /// test-fix-test, whitelist migrazioni 0218/0286) ma il braccio nel
    /// dispatcher era assente — ogni invocazione cadeva nel fallback
    /// "Tool non esiste". Su una root vuota e senza comando esplicito
    /// l'implementazione risponde con l'errore di auto-detection, senza
    /// toccare DB ne' sandbox: basta a provare il ricablaggio.
    #[tokio::test]
    async fn run_tests_e_dispatchato_e_non_cade_nel_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_for_dispatch_tests(dir.path().to_path_buf());
        let out = execute_agent_tool(&ctx, "run_tests", &serde_json::json!({})).await;
        assert!(
            !out.contains("non esiste"),
            "run_tests caduto nel fallback del dispatcher: {out}"
        );
        assert!(
            out.contains("impossibile rilevare il comando test"),
            "output inatteso da tool_run_tests: {out}"
        );
    }

    /// Contro-prova: un nome sconosciuto cade ancora nel fallback.
    ///
    /// GAP1: l'output DEVE iniziare con il marker '\u{274C}' (con eventuale
    /// trim_start) cosi' `tool_runner_server` deriva is_error=true. Il pool e'
    /// lazy non connesso: le query DB del resolver degradano (no panic) e resta
    /// il messaggio base + nudge tool_search.
    #[tokio::test]
    async fn tool_sconosciuto_cade_nel_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_for_dispatch_tests(dir.path().to_path_buf());
        let out = execute_agent_tool(&ctx, "tool_che_non_esiste", &serde_json::json!({})).await;
        assert!(
            out.contains("non esiste"),
            "fallback atteso, ottenuto: {out}"
        );
        assert!(
            out.trim_start().starts_with('\u{274C}'),
            "GAP1: l'errore tool-not-found deve iniziare col marker U+274C: {out}"
        );
        assert!(
            out.contains("nexus_mcp_tool_search"),
            "GAP3: nudge a tool_search sempre presente: {out}"
        );
    }

    /// GAP1 (bug chiuso): un `nexus_*` INESISTENTE passa per
    /// nexus_builtin::execute_with_neural -> execute -> fallback `_`. Prima
    /// ritornava "[Nexus Builtin] Tool ... non riconosciuto." SENZA marker ->
    /// is_error=FALSE -> finto successo. Ora il resolver antepone U+274C.
    #[tokio::test]
    async fn nexus_tool_inesistente_ha_marker_errore() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_for_dispatch_tests(dir.path().to_path_buf());
        let out = execute_agent_tool(
            &ctx,
            "nexus_tool_inventato_dal_modello",
            &serde_json::json!({}),
        )
        .await;
        assert!(
            out.trim_start().starts_with('\u{274C}'),
            "GAP1: nexus_* inesistente deve produrre is_error path (marker U+274C): {out}"
        );
        assert!(
            !out.contains("non riconosciuto"),
            "il vecchio messaggio senza marker non deve piu' comparire: {out}"
        );
    }

    /// GAP2: il fuzzy reale (non piu' alias hardcoded) suggerisce il builtin
    /// corretto per un nome storpiato, end-to-end attraverso il dispatcher.
    #[tokio::test]
    async fn fuzzy_storpiato_suggerisce_builtin() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_for_dispatch_tests(dir.path().to_path_buf());
        let out = execute_agent_tool(&ctx, "read_fil", &serde_json::json!({})).await;
        assert!(
            out.trim_start().starts_with('\u{274C}'),
            "marker atteso: {out}"
        );
        assert!(
            out.contains("read_file"),
            "GAP2: 'read_fil' deve suggerire read_file: {out}"
        );
    }
}
