-- 0406: Bonifica settings morte (audit configurazioni 2026-06-11).
--
-- Ogni chiave qui sotto e' stata classificata MORTA da scripts/audit_settings.py
-- (mai citata come stringa in crates/, brain/, apps/, packages/, scripts/,
-- evals/, deploy/, config/) e CONFERMATA da una verifica adversariale
-- indipendente (workflow a 8 verificatori: grep su costruzioni dinamiche dei
-- segmenti, git log -S per la storia del lettore, scan di config/policy/prompt
-- template). I falsi positivi dello scanner (rate_limit_*_window_ms letti dal
-- gateway via DB_KEY_MAP non quotato, web_ide_port letta dinamicamente dal
-- watchdog via port_setting_key) sono stati ESCLUSI da questa lista.
--
-- Famiglie principali e causa radice:
--   impact.*            endpoint /api/internal/impact/* cancellato dal commit
--                       eb5e47a (ADR 0017 v2) senza cleanup settings
--   kb.* / knowledge.*  feature KB legacy sostituite dal wiki unificato
--   meta_docs.*         vault meta-docs legacy (ADR 0017 v2); incluse le
--                       chiavi del worker autofix ZOMBIE (pollava
--                       nexus_e2e_runs in cui nessun codice ha mai scritto;
--                       worker rimosso nello stesso change set)
--   wiki.retention/lock retention/versioning mai implementati
--   sandbox_* / schema.* / supervisor_* / optimizer_* seed storici mai letti
--
-- Idempotente: DELETE su chiavi esplicite.

DELETE FROM settings WHERE key IN (
    'agent.build_graph.refresh_on_watcher',
    'agent.build_graph.warn_on_unknown',
    'agent.continuation.auto_restart_enabled',
    'agent.continuation.follow_up_prompt',
    'agent.continuation.max_auto_restarts',
    'agent.continuation.min_promise_recency_chars',
    'agent.cooperative_cancel.check_interval_seconds',
    'agent.diagnostics.empty_response_retention_days',
    'agent.kb.cluster_method',
    'agent.observer.latency_degraded_ms',
    'agent.observer.restart_rate_window_s',
    'agent.observer.tail_only_with_subscribers',
    'agent.scanner.sql_injection_enabled',
    'agent.scanner.sql_injection_min_severity',
    'agent.todos.carry_over_enabled',
    'agent.tools.inline_core_count',
    'agent.tools.max_description_tokens',
    'agent.tools.regression_test_enabled',
    'agent.wiki.watcher_poll_interval_secs',
    'alignment_sync_enabled',
    'claude_agents.overwrite_unmanaged_default',
    'extra_project_roots',
    'impact.depth_cap',
    'impact.enabled',
    'impact.max_nodes',
    'impact.test_informed_enabled',
    'impact.test_informed_max_listed_tests',
    'impact.test_informed_max_seed_paths',
    'kb.autolink.enabled',
    'kb.autolink.semantic_threshold',
    'kb.autolink.semantic_top_k',
    'kb.autolink.wikilink_max_per_note',
    'kb.changelog_cross_enabled',
    'kb.code_doc.enabled',
    'kb.code_doc.max_file_bytes',
    'kb.code_doc.max_files',
    'kb.code_doc.max_source_chars',
    'kb.ingest.body_max_chars',
    'kb.ingest.cjk_max_ratio_pct',
    'kb.ingest.enabled',
    'kb.ingest.min_chars',
    'kb.ingest.title_max_chars',
    'kb.lifecycle.auto_deprecate_on_correction',
    'kb.lifecycle.context_stale_enabled',
    'knowledge.autolink_threshold',
    'knowledge.cleanup_draft_days',
    'knowledge.cleanup_inactive_days',
    'knowledge.cleanup_inactive_enabled',
    'knowledge.commit_vault_to_git',
    'knowledge.graph_import_autolink',
    'knowledge.link_worker_interval_secs',
    'knowledge.similarity_banner_threshold',
    'knowledge.vault_watcher_debounce_ms',
    'meta_docs.autofix_enabled',
    'meta_docs.autofix_target_branch',
    'meta_docs.changelog_min_significance',
    'meta_docs.e2e_smoke_cron',
    'meta_docs.e2e_smoke_url',
    'meta_docs.enabled',
    'meta_docs.obsidian_vault_name',
    'meta_docs.refresh_worker_interval_secs',
    'meta_docs.watcher_debounce_ms',
    'optimizer_rollback_threshold',
    'optimizer_success_rate_threshold',
    'prompt_optimizer_use_batch_api',
    'routing.degradation.cooldown_seconds',
    'routing.degradation.min_visits',
    'routing.degradation.threshold',
    'sandbox_cpus',
    'sandbox_enabled',
    'sandbox_memory_mb',
    'schema.descr_max',
    'schema.enum_max',
    'schema.tool_descr_max',
    'supervisor_model',
    'supervisor_provider',
    'terminal_default_shell',
    'wiki.lock_on_external_edit',
    'wiki.protect_manual_edits',
    'wiki.regen_section_merge',
    'wiki.retention_keep_all_manual',
    'wiki.retention_max_age_days',
    'wiki.retention_max_versions',
    'wiki.retention_worker_interval_secs',
    'wiki.versioning_enabled'
);

-- NOTE su chiavi MANTENUTE benche' senza lettore diretto nel codice:
--   agent.visual_compare.similarity_threshold  citata nel contratto testuale
--       dei system prompt attivi (mig 0215, blocco <visual_verification>)
--   gitlab_personal_access_token  contratto secret_bindings del plugin
--       gitlab-stdio (catalogo abilitato; risoluzione dinamica
--       plugin-service/src/plugins.rs resolve_secret_value)
--   nexus_profile  bridge DB->env aggiunto al gateway in questo change set
--   orchestrator.clarifying_questions_{enabled,max}  wiring riparato in
--       brain/agents/orchestrator_config.py (questo change set)
