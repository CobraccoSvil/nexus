---
id: ee37312a-93c4-49c2-ad92-06699e747f60
kind: schema
title: Log migrazioni Postgres
slug: migrations-log
tags:
  - schema
  - migrations
source_files:
  - db/migrations/
auto_generated: true
created_at: 2026-05-23T07:20:00Z
updated_at: 2026-05-23T11:11:50Z
nexus_meta_version: 1
---

Cronologia migrazioni SQL in `db/migrations/`. Generato automaticamente.

Vedi anche: [[postgres-tables]], [[nexus-architetturale]].

| File | Descrizione |
|---|---|
| `0001_initial_schema.sql` | (senza descrizione) |
| `0002_settings.sql` | Settings table: chiave/valore con categorie e cifratura opzionale |
| `0003_github_auth.sql` | GitHub OAuth: add GitHub fields to users, create sessions table, seed auth settings |
| `0004_user_owned_projects.sql` | (senza descrizione) |
| `0005_routing_admin.sql` | (senza descrizione) |
| `0006_ai_billing.sql` | (senza descrizione) |
| `0007_projects_root_setting.sql` | (senza descrizione) |
| `0008_chat_learning_vector.sql` | (senza descrizione) |
| `0009_agent_tables.sql` | Agent runs: traccia ogni esecuzione del loop agente |
| `0010_project_analysis.sql` | Aggiunge campo analisi progetto |
| `0011_terminal_commands.sql` | Comandi da iniettare nei terminali IDE via agente |
| `0012_agent_parent_run.sql` | Migrazione: supporto agenti paralleli con gerarchia padre/figlio |
| `0013_terminal_commands_ack.sql` | Affidabilita' consegna comandi terminale: claim + ack esplicito |
| `0014_mcp_servers.sql` | MCP external server connectors |
| `0015_session_management.sql` | Discriminatore per riassunti di sessione vs correzioni prompt standard |
| `0016_github_source_control.sql` | GitHub Source Control+: per-user GitHub connection for IDE Git operations |
| `0017_plugin_manager.sql` | Plugin Manager v1 (Plugin = MCP) |
| `0018_project_vector_bootstrap.sql` | (senza descrizione) |
| `0019_profiles_extension.sql` | User-owned profiles (GPT/Gem style). |
| `0020_plugin_secret_settings.sql` | Plugin Manager: chiavi segrete base per plugin MCP curati |
| `0021_plugin_instances_dedup_unique.sql` | Deduplica plugin instances (stesso catalog_item_id) e impone unicita' globale. |
| `0022_mcp_plugin_adapter_dedup_unique.sql` | Hardening dedup plugin manager: |
| `0023_figma_plugin_headers_compat.sql` | Figma plugin compatibility: |
| `0024_long_running_patterns.sql` | Patterns per rilevare comandi long-running nell'agent loop. |
| `0025_figma_oauth_settings.sql` | Figma OAuth + fallback settings for Plugin Manager. |
| `0026_terminal_commands_finish.sql` | Terminal commands: aggiunge campi per completamento evento-driven |
| `0027_agent_processes.sql` | (senza descrizione) |
| `0028_project_quality.sql` | (senza descrizione) |
| `0029_run_configurations.sql` | (senza descrizione) |
| `0030_file_index_hashes.sql` | Traccia l'hash SHA256 dei file indicizzati nel vector store |
| `0031_google_batch_settings.sql` | Migration 0031: Google Gemini Batch API settings |
| `0032_model_catalog.sql` | Migration 0032: Extended model catalog with capabilities + 5 providers (21 models) |
| `0033_users_soft_delete.sql` | Add soft delete support to users table |
| `0034_agent_run_resume.sql` | Migration 0034: aggiunge colonna messages_json ad agent_runs per supportare ripresa dopo interruzione |
| `0035_prompt_templates.sql` | Seed: N+1 quality rule |
| `0036_finding_false_positive.sql` | (senza descrizione) |
| `0037_prompt_templates_expand_seeds.sql` | Expand nexus_prompt_templates with all hardcoded prompts from the Rust backend. |
| `0038_prompt_template_usage_context.sql` | Aggiunge colonna usage_context per fornire contesto all'assistente AI di editing prompt |
| `0039_agent_run_supervisor_mode.sql` | Migration 0039: aggiunge supervisor_mode ad agent_runs per preservarlo durante il resume |
| `0040_fix_mistral_model_names.sql` | Migration 0040: corregge i nomi modello Mistral nel catalogo |
| `0041_precheck_prompt_template.sql` | Migration 0041: aggiunge il prompt template per il precheck dei messaggi chat. |
| `0042_prompt_templates_add_chat_category.sql` | Migration 0042: aggiunge la categoria 'chat' ai prompt templates |
| `0043_feedback_assist_template.sql` | Migration 0043: feedback assist prompt template |
| `0044_nexus_builtin_mcp.sql` | Migration 0044: Nexus Builtin MCP Server |
| `0045_provider_enabled_settings.sql` | Aggiunge le impostazioni enable/disable per ogni provider LLM. |
| `0047_fix_automatic_mode_prompt.sql` | Fix: Renderere il prompt della modalità AUTOMATICA più imperativo |
| `0048_project_documents.sql` | Project documentation system: tracks generated documents and their versions |
| `0049_doc_prompt_templates.sql` | Prompt templates for document generation guidance |
| `0050_prompt_mcp_tools.sql` | Add MCP tools management to prompt templates |
| `0051_nexus_routing_observability.sql` | Migration 0051: Nexus routing observability on agent_runs. |
| `0052_ruvector_tables.sql` | Migration 0052: RuVector — database vettoriale nativo per Nexus. |
| `0053_agent_registry.sql` | Migration 0053: Agent Registry — registro degli agent types. |
| `0054_q_learning_state.sql` | Migration 0054: Q-Learning state — persistenza Q-values del router. |
| `0055_reasoning_bank.sql` | Migration 0055: Reasoning Bank — storage pattern cognitivi. |
| `0056_memory_namespace.sql` | Migration 0056: Memory Namespace — memoria condivisa tra agenti (CRDT-friendly). |
| `0057_nexus_replication_log.sql` | Tabella per persistenza dei batch di replicazione prodotti da ReplicationWorker. |
| `0058_nexus_agent_prompt_templates.sql` | Seed dei template di sistema per i tipi di agente Nexus. |
| `0059_agent_prompt_templates.sql` | Migration 0059: prompt templates per tutti gli agent type Nexus. |
| `0060_fix_supervisor_prompt_params.sql` | Fix: il supervisor prompt usava "offset" e "limit" come parametri di read_file_lines, |
| `0061_nexus_active_routing_pct_seed.sql` | Assicura che nexus_active_routing_pct esista con category='routing' |
| `0062_supervisor_prompt_edit_file_rule.sql` | Fix: il supervisor non aveva una regola specifica per i fallimenti di edit_file |
| `0063_supervisor_prompt_batch_edit_rule.sql` | Fix: il supervisor non rilevava il pattern di molte edit_file consecutive sullo stesso file. |
| `0064_system_prompt_verify_after_task.sql` | Aggiunge istruzione globale: l'agente deve eseguire `pnpm verify` (o equivalente) |
| `0065_project_custom_instructions.sql` | Aggiunge custom_instructions al progetto: istruzioni specifiche per-progetto |
| `0066_backfill_solarmatch_custom_instructions.sql` | Popolamento retroattivo custom_instructions per SolarMatch. |
| `0067_gateway_settings.sql` | Aggiunge le chiavi di configurazione del gateway nella tabella settings. |
| `0068_run_configurations_role_essential.sql` | Aggiunge metadati di classificazione alle configurazioni di run. |
| `0069_projects_detected_suggestions.sql` | Cache dei suggerimenti run-config rilevati automaticamente dal filesystem. |
| `0070_projects_repository_root_path.sql` | Aggiunge il percorso radice del repository al progetto. |
| `0071_projects_default_profile.sql` | Profilo AI di default per progetto. |
| `0072_user_profiles_system.sql` | Fase 2: unificazione profili utente + template profilo di sistema. |
| `0073_chat_sessions_preferred_model.sql` | Preferenza modello per sessione di chat. |
| `0074_system_prompt_no_model_comments.sql` | Aggiunge istruzione al system prompt di Nexus: |
| `0075_chat_sessions_privacy_state.sql` | Traccia quando il gateway ha re-instradato automaticamente su provider locale per privacy. |
| `0076_system_prompt_agent_behavior.sql` | 1. Divieto di narrare il processo interno |
| `0077_agent_processes_sandbox.sql` | Aggiunge la colonna sandboxed ad agent_processes |
| `0078_profile_mcp_servers.sql` | Associazione many-to-many tra profili di sistema e server MCP globali. |
| `0079_agent_processes_kind.sql` | Aggiunge la colonna kind ad agent_processes per distinguere |
| `0080_project_sandbox_config.sql` | Configurazione sandbox per-progetto. |
| `0081_project_database_config.sql` | Configurazione database per-progetto utente. |
| `0082_project_migration_history.sql` | Storico delle migrazioni applicate al DB di ogni progetto utente. |
| `0083_project_database_multi_connection.sql` | Estende project_database_config per supportare piu' connessioni DB |
| `0084_precheck_fix_tools_false_negative.sql` | Aggiorna il template di precheck per evitare un falso negativo: |
| `0085_fix_agent_autonomy_prompt.sql` | Migrazione 0085: Corregge i prompt degli agenti debugger e coder |
| `0086_prompt_v2_core.sql` | Migrazione 0086: Refactor catalogo prompt — Wave A (4 agenti core) |
| `0087_prompt_v2_admin_metadata.sql` | Migrazione 0087: Metadati per la pagina admin prompt v2 |
| `0090_agent_reflections.sql` | Migrazione 0090: tabella nexus_agent_reflections per self-reflection runtime (Fase 2) |
| `0091_reflection_settings.sql` | Migrazione 0091: impostazioni runtime per il sistema di self-reflection (Fase 2) |
| `0092_prompt_optimizer.sql` | Migrazione 0092: infrastruttura PromptOptimizerWorker (Fase 3) |
| `0093_prompt_eval_runs.sql` | Migrazione 0093: tabella prompt_eval_runs per l'eval harness (Fase 4) |
| `0094_project_analyzer_agent.sql` | Migrazione 0094: Agente dedicato per analisi profonda del progetto. |
| `0095_analyzer_run_mode.sql` | Migrazione 0095: estende il prompt agent.project.analyzer con la |
| `0096_project_isolation_rules.sql` | Migrazione 0096: regole di isolamento progetto e safety Docker. |
| `0097_provider_health_history.sql` | Migrazione 0097: storico health check provider LLM. |
| `0098_prompt_anti_hallucination_guard.sql` | Migrazione 0098: rinforzo anti-allucinazione e scope di modifica nei prompt agente. |
| `0099_prompt_no_empty_fields.sql` | Migrazione 0099: regola "no campi vuoti / no valori inventati" nei prompt agente. |
| `0100_update_obsolete_model_names.sql` | Migrazione 0100: aggiorna nomi modello AI obsoleti. |
| `0101_routing_model_registry.sql` | Migrazione 0101: registry DB-driven dei modelli AI per il routing. |
| `0102_purpose_model_registry.sql` | Migrazione 0102: registry DB-driven per "purpose-specific" models. |
| `0103_insights_status_running.sql` | Migrazione 0103: estende il check constraint di nexus_project_insights |
| `0104_quality_scans_async.sql` | 0104: quality scans async (stub - migrazione gia' applicata) |
| `0105_quality_vector_enhancements.sql` | 0105: quality vector enhancements (stub - migrazione gia' applicata) |
| `0106_coder_output_format_structured.sql` | 0106: coder output format structured (stub - migrazione gia' applicata) |
| `0107_fix_semplice_capable_model.sql` | 0107: fix semplice capable model (stub - migrazione gia' applicata) |
| `0108_agent_tier_purpose_models.sql` | Migrazione 0104: purpose models per i 3 tier agente. |
| `0109_fix_chat_breve_approfondita.sql` | Migrazione 0109: corregge incoerenza in nexus_routing_matrix per chat_breve × approfondita. |
| `0110_intent_capability.sql` | Migrazione 0110: tabella nexus_intent_capability |
| `0111_routing_thresholds.sql` | Migrazione 0111: settings.routing.* per parametri configurabili del routing |
| `0112_routing_decisions_audit.sql` | Migrazione 0112: nexus_routing_decisions (telemetria audit del routing) |
| `0113_agent_runs_token_usage.sql` | Migrazione: aggiunge colonne token/costo alla tabella agent_runs |
| `0114_port_allocations.sql` | 0114_port_allocations.sql |
| `0115_project_documents_unique_path.sql` | Migrazione 0115: vincolo UNIQUE su (project_id, file_path) per project_documents |
| `0116_dependency_health.sql` | Migrazione 0116: storico health check dipendenze infrastrutturali |
| `0117_loop_fallback_purpose_model.sql` | Purpose model usato per auto-escalation quando il brain rileva loop tool-use. |
| `0118_more_mcp_stdio_catalog.sql` | Estende il catalogo curato con MCP stdio standard @modelcontextprotocol/server-*. |
| `0119_conversation_summaries.sql` | 0119_conversation_summaries.sql |
| `0120_routing_token_threshold.sql` | 0120_routing_token_threshold.sql |
| `0121_anthropic_batches.sql` | 0121_anthropic_batches.sql |
| `0122_mcp_tools_embedding.sql` | 0122_mcp_tools_embedding.sql |
| `0123_extended_thinking_settings.sql` | Migrazione 0123: settings configurabili per Extended Thinking (Anthropic) |
| `0124_agent_router_settings.sql` | Migrazione 0124: flag agent_router_enabled nella tabella settings |
| `0125_env_to_settings.sql` | Migrazione 0125: flag operativi e parametri di tuning migrati da env var a tabella settings |
| `0126_drop_orphan_tables.sql` | Migrazione 0126: rimozione tabelle orfane (nessun riferimento nel codice runtime) |
| `0127_anti_narration_loop.sql` | Migrazione 0127: regola anti-narrazione per tutti gli agenti Nexus. |
| `0128_model_escalation_chain.sql` | Migrazione 0128: catena di escalation intra-provider |
| `0129_ledger_cache_columns.sql` | Migrazione 0129: aggiunge colonne cache token ad ai_usage_ledger |
| `0130_price_cache_columns.sql` | Migrazione 0130: aggiunge pricing cache ad ai_price_catalog |
| `0131_drop_dead_tables.sql` | Migrazione 0131: rimozione tabelle morte (nessun INSERT né SELECT nel codice attivo) |
| `0132_disambiguation_and_study_mode_settings.sql` | 0132: settings per disambiguation step (L2) e study mode tool gating (L3) |
| `0133_routing_slots_matrix.sql` | 0133: Slot-filling routing matrix (Livello 4 disambiguation framework) |
| `0134_classifier_provider_chain.sql` | 0134: Chain di provider per il classifier agentico |
| `0135_shared_directives.sql` | Migrazione 0135: direttive condivise per agenti. |
| `0136_purpose_models_optimizer_batch.sql` | 0136: Aggiunge purpose_model per prompt_optimizer e anthropic_batch. |
| `0137_nexus_operator_unrestricted.sql` | Migrazione 0137: Nexus operatore supremo — rimozione restrizioni su progetti gestiti. |
| `0138_project_runtime_issues.sql` | Fix M10: tabella per errori runtime dei tool agente (run_command, browser-check, ecc.) |
| `0139_automatic_mode_no_question.sql` | Fix M22 (iterazione 2 test maturita): rafforza il prompt AUTOMATIC mode |
| `0140_automatic_mode_git_suggestion.sql` | Fix M27 (parte C): aggiunge al prompt automatic mode il suggerimento di |
| `0141_automatic_mode_port_allocation.sql` | Fix M33-A: rimuove le porte hardcoded dal prompt automatic mode e aggiunge |
| `0142_automatic_mode_locale_docs.sql` | Fix M42: i documenti markdown generati (PRD, README, docs/*) devono |
| `0143_modern_models_enable_and_routing.sql` | Fix M53: abilita modelli moderni in ai_price_catalog + aggiorna default |
| `0144_automatic_mode_scaffolding_app.sql` | Fix M54: il prompt automatic mode deve far comportare l'agente come Claude |
| `0145_automatic_mode_dod_no_premature_close.sql` | Fix M56: aggiungi DoD (Definition of Done) esplicita al prompt automatic mode |
| `0146_port_allocation_mode_extend.sql` | Fix M58: estendi CHECK constraint allocation_mode in nexus_port_allocations. |
| `0147_automatic_mode_db_port_strict.sql` | Fix M59: rinforza la regola Postgres applicativi in mode_automatic_instruction. |
| `0148_agent_plans_todos.sql` | PR-1 Plan/Act/Verify orchestrator: schema per planner + TodoList + verifier runs. |
| `0149_orchestrator_prompts.sql` | PR-1 Plan/Act/Verify: prompt templates per planner + todo reminder + replan |
| `0150_orchestrator_settings.sql` | PR-1 Plan/Act/Verify: setting orchestrator + nexus_purpose_model entry. |
| `0151_subagent_definitions_runs.sql` | PR-3 sub-agents pattern: tabelle definitions + runs. |
| `0152_subagent_prompts.sql` | PR-3 sub-agents: prompt templates per i 5 kind base. |
| `0153_subagent_settings.sql` | PR-3 sub-agents: setting categoria orchestrator. |
| `0154_security_audit.sql` | M63 guardrail: audit log dei comandi shell bloccati dal sanitizer. |
| `0156_postgres_app_container_settings.sql` | M74 — Settings per il container Postgres separato dedicato ai DB applicativi |
| `0157_project_instructions.sql` | PR-3 (Codex pattern) — AGENTS.md / CLAUDE.md / .cursorrules analogo per Nexus. |
| `0158_agent_clarifications.sql` | PR-3 (Codex pattern) — Clarifying questions pre-flight del planner. |
| `0159_clarifying_prompts_and_extras.sql` | PR-3 — Prompt keys per clarifying questions (Codex) + auto-delegation (Cursor) |
| `0160_prompt_provider_neutral_tools.sql` | 0160_prompt_provider_neutral_tools.sql |
| `0161_jobs_live_progress.sql` | 0161: aggiunge campi per il monitoraggio live di run Playwright (e simili) |
| `0162_project_flags.sql` | Dispatcher centrale: flag globali per progetto. |
| `0163_dispatcher_purpose.sql` | Dispatcher: modello LLM per fallback classifier (eventi custom). |
| `0164_nexus_events_audit.sql` | Tabella di audit persistente per eventi dispatcher |
| `0165_nexus_resource_quotas.sql` | 0165_nexus_resource_quotas.sql |
| `0166_nexus_resource_audit.sql` | 0166_nexus_resource_audit.sql |
| `0167_agent_db_management_prompt.sql` | 0167: Aggiunge istruzioni database_management al prompt agent.coder.base. |
| `0168_agent_meta_steps.sql` | Meta-step pubblicati in chat (plan/routing/clarify/fallback/reflection). |
| `0169_clarify_or_expand.sql` | Clarify/Expand condizionale (Fase 2 normalizzazione prompt). |
| `0170_model_capabilities.sql` | M170: Capability per modello (es. extended thinking) come JSONB su ai_price_catalog. |
| `0171_provider_test_and_admin_purposes.sql` | M171: Purpose model per provider test_connection() e admin tool selection. |
| `0172_ai_model_health.sql` | Migrazione 0172: model-level health history e contatore fallimenti. |
| `0173_provider_budget_tracking.sql` | Migrazione 0173: tracking budget per provider AI. |
| `0174_routing_matrix_auto_promote.sql` | Migrazione 0174: routing matrix auto-promoter. |
| `0175_knowledge_base.sql` | ========================================================================== |
| `0176_brain_learning_to_postgres.sql` | Migrazione del learning storage dal SQLite locale del brain a PostgreSQL. |
| `0177_nexus_meta_docs.sql` | Migrazione meta-docs vault: documentazione del meta-progetto Nexus |
| `0178_obsidian_vault_name.sql` | Migrazione: registra il nome del vault Obsidian per il meta-vault Nexus e per |

**Totale**: 174 migrazioni.

Ultima migrazione: `0178_obsidian_vault_name.sql`.
