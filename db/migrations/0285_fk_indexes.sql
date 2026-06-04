-- 0285: indici sulle foreign key prive di indice di supporto.
--
-- Audit revisione codice (best practice DB). Postgres NON crea automaticamente
-- un indice sulla colonna REFERENTE di una foreign key (indicizza solo la
-- colonna referenziata via PK/unique). Senza indice, ogni JOIN/lookup sulla FK
-- e ogni verifica ON DELETE/UPDATE CASCADE esegue un sequential scan -> O(n)
-- sulla tabella figlia. 48 FK risultavano prive di indice.
--
-- Statement generati da pg_constract (FK senza indice corrispondente).
-- Idempotente: CREATE INDEX IF NOT EXISTS.

CREATE INDEX IF NOT EXISTS idx_agent_processes_session_id ON agent_processes (session_id);
CREATE INDEX IF NOT EXISTS idx_agent_runs_project_id ON agent_runs (project_id);
CREATE INDEX IF NOT EXISTS idx_agent_runs_run_message_id ON agent_runs (run_message_id);
CREATE INDEX IF NOT EXISTS idx_ai_price_catalog_created_by ON ai_price_catalog (created_by);
CREATE INDEX IF NOT EXISTS idx_ai_quota_policies_created_by ON ai_quota_policies (created_by);
CREATE INDEX IF NOT EXISTS idx_ai_response_feedback_message_id ON ai_response_feedback (message_id);
CREATE INDEX IF NOT EXISTS idx_ai_response_feedback_orchestrator_run_id ON ai_response_feedback (orchestrator_run_id);
CREATE INDEX IF NOT EXISTS idx_ai_response_feedback_reviewed_by ON ai_response_feedback (reviewed_by);
CREATE INDEX IF NOT EXISTS idx_ai_response_feedback_session_id ON ai_response_feedback (session_id);
CREATE INDEX IF NOT EXISTS idx_ai_response_feedback_user_id ON ai_response_feedback (user_id);
CREATE INDEX IF NOT EXISTS idx_chat_messages_deleted_by_user_id ON chat_messages (deleted_by_user_id);
CREATE INDEX IF NOT EXISTS idx_git_operations_user_id ON git_operations (user_id);
CREATE INDEX IF NOT EXISTS idx_git_operations_workspace_id ON git_operations (workspace_id);
CREATE INDEX IF NOT EXISTS idx_git_status_snapshots_project_id ON git_status_snapshots (project_id);
CREATE INDEX IF NOT EXISTS idx_git_status_snapshots_workspace_id ON git_status_snapshots (workspace_id);
CREATE INDEX IF NOT EXISTS idx_learning_decisions_log_snapshot_id ON learning_decisions_log (snapshot_id);
CREATE INDEX IF NOT EXISTS idx_learning_policy_snapshots_created_by_user_id ON learning_policy_snapshots (created_by_user_id);
CREATE INDEX IF NOT EXISTS idx_nexus_events_audit_actor_user_id ON nexus_events_audit (actor_user_id);
CREATE INDEX IF NOT EXISTS idx_nexus_meta_doc_changes_generated_doc_id ON nexus_meta_doc_changes (generated_doc_id);
CREATE INDEX IF NOT EXISTS idx_nexus_port_allocations_run_config_id ON nexus_port_allocations (run_config_id);
CREATE INDEX IF NOT EXISTS idx_nexus_resource_audit_actor_user_id ON nexus_resource_audit (actor_user_id);
CREATE INDEX IF NOT EXISTS idx_orchestrator_audit_events_run_id ON orchestrator_audit_events (run_id);
CREATE INDEX IF NOT EXISTS idx_orchestrator_runs_session_id ON orchestrator_runs (session_id);
CREATE INDEX IF NOT EXISTS idx_orchestrator_runs_user_id ON orchestrator_runs (user_id);
CREATE INDEX IF NOT EXISTS idx_plugin_audit_events_project_id ON plugin_audit_events (project_id);
CREATE INDEX IF NOT EXISTS idx_plugin_instance_health_runs_tested_by_user_id ON plugin_instance_health_runs (tested_by_user_id);
CREATE INDEX IF NOT EXISTS idx_plugin_instance_tool_policies_updated_by_user_id ON plugin_instance_tool_policies (updated_by_user_id);
CREATE INDEX IF NOT EXISTS idx_plugin_instances_installed_by_user_id ON plugin_instances (installed_by_user_id);
CREATE INDEX IF NOT EXISTS idx_plugin_instances_release_id ON plugin_instances (release_id);
CREATE INDEX IF NOT EXISTS idx_project_documents_created_by ON project_documents (created_by);
CREATE INDEX IF NOT EXISTS idx_project_knowledge_notes_source_run_id ON project_knowledge_notes (source_run_id);
CREATE INDEX IF NOT EXISTS idx_project_migration_history_applied_by_user ON project_migration_history (applied_by_user);
CREATE INDEX IF NOT EXISTS idx_project_migration_history_created_by_user ON project_migration_history (created_by_user);
CREATE INDEX IF NOT EXISTS idx_project_open_sessions_workspace_id ON project_open_sessions (workspace_id);
CREATE INDEX IF NOT EXISTS idx_project_runtime_issues_step_id ON project_runtime_issues (step_id);
CREATE INDEX IF NOT EXISTS idx_projects_default_profile_id ON projects (default_profile_id);
CREATE INDEX IF NOT EXISTS idx_projects_last_opened_by_user_id ON projects (last_opened_by_user_id);
CREATE INDEX IF NOT EXISTS idx_prompt_corrections_feedback_id ON prompt_corrections (feedback_id);
CREATE INDEX IF NOT EXISTS idx_prompt_corrections_message_id ON prompt_corrections (message_id);
CREATE INDEX IF NOT EXISTS idx_prompt_corrections_orchestrator_run_id ON prompt_corrections (orchestrator_run_id);
CREATE INDEX IF NOT EXISTS idx_prompt_corrections_session_id ON prompt_corrections (session_id);
CREATE INDEX IF NOT EXISTS idx_repositories_project_id ON repositories (project_id);
CREATE INDEX IF NOT EXISTS idx_security_findings_project_id ON security_findings (project_id);
CREATE INDEX IF NOT EXISTS idx_sessions_user_id ON sessions (user_id);
CREATE INDEX IF NOT EXISTS idx_terminal_commands_session_id ON terminal_commands (session_id);
CREATE INDEX IF NOT EXISTS idx_vector_compaction_runs_project_id ON vector_compaction_runs (project_id);
CREATE INDEX IF NOT EXISTS idx_vector_compaction_runs_requested_by ON vector_compaction_runs (requested_by);
CREATE INDEX IF NOT EXISTS idx_workspaces_project_id ON workspaces (project_id);
