---
id: aa2d3f48-059b-4a94-bf43-c901a4eba9b6
kind: api
title: Endpoint REST
slug: rest-endpoints
tags:
  - api
  - rest
source_commit: 4a9397442aee48941b64520b98a5c4cb5d97848c
source_files:
  - crates/mcp-core/src/main.rs
auto_generated: true
created_at: 2026-05-23T07:20:00Z
updated_at: 2026-05-23T07:20:03Z
nexus_meta_version: 1
---

Endpoint REST esposti da mcp-core (axum). Generato parsando `crates/mcp-core/src/main.rs`.

**Totale endpoint**: 238


## `/api/admin`

| Metodo | Path | Handler |
|---|---|---|
| `DELETE` | `/api/admin/prompt-corrections/:id` | `chat_learning::admin_delete_prompt_correction` |
| `GET` | `/api/admin/available-mcp-tools` | `prompt_templates::available_mcp_tools_handler` |
| `GET` | `/api/admin/billing/prices` | `billing::list_prices` |
| `GET` | `/api/admin/billing/quotas` | `billing::list_quotas` |
| `GET` | `/api/admin/billing/usage` | `billing::admin_usage_report` |
| `GET` | `/api/admin/environment/status` | `environment::get_environment_status` |
| `GET` | `/api/admin/feedback/errors` | `chat_learning::admin_list_feedback_errors` |
| `GET` | `/api/admin/fs/directories` | `settings::browse_directories` |
| `GET` | `/api/admin/gateway/providers` | `environment::gateway_providers_handler` |
| `GET` | `/api/admin/learning/projects/:id/config` | `chat_learning::admin_get_project_learning_config` |
| `GET` | `/api/admin/long-running` | `long_running::list_patterns` |
| `GET` | `/api/admin/profiles` | `profiles::admin_list_profiles` |
| `GET` | `/api/admin/projects` | `admin::projects::list_all_projects` |
| `GET` | `/api/admin/projects/:project_id/members` | `admin::projects::list_project_members` |
| `GET` | `/api/admin/prompt-corrections` | `chat_learning::admin_list_prompt_corrections` |
| `GET` | `/api/admin/prompt-templates/:key/tools` | `prompt_templates::get_prompt_tools_handler` |
| `GET` | `/api/admin/providers/budget` | `environment::admin_providers_budget_list` |
| `GET` | `/api/admin/providers/cooldown` | `environment::admin_cooldown_list` |
| `GET` | `/api/admin/qdrant-health` | `environment::qdrant_health_handler` |
| `GET` | `/api/admin/routing/purpose-models` | `admin::routing::list_purpose_models` |
| `GET` | `/api/admin/settings` | `settings::list_settings` |
| `GET` | `/api/admin/settings-by-category/:category` | `settings::list_by_category` |
| `GET` | `/api/admin/user-profiles` | `profiles::admin_list_user_profiles` |
| `GET` | `/api/admin/users` | `admin::users::list_users` |
| `GET` | `/api/admin/users/:user_id` | `admin::users::get_user` |
| `GET` | `/api/admin/users/search` | `admin::users::search_users` |
| `GET` | `/api/admin/vector/compact/runs` | `chat_learning::admin_list_vector_compaction_runs` |
| `GET` | `/api/admin/watchdog-status` | `task_watchdog::watchdog_status_handler` |
| `POST` | `/api/admin/embeddings/apply` | `environment::embeddings_apply_handler` |
| `POST` | `/api/admin/embeddings/validate` | `environment::embeddings_validate_handler` |
| `POST` | `/api/admin/environment/fix` | `environment::fix_environment` |
| `POST` | `/api/admin/feedback/:id/review` | `chat_learning::admin_review_feedback` |
| `POST` | `/api/admin/fs/directories/create` | `settings::create_directory` |
| `POST` | `/api/admin/gateway/reload` | `environment::gateway_reload_handler` |
| `POST` | `/api/admin/learning/projects/:id/retrain-routing` | `chat_learning::admin_retrain_project_routing` |
| `POST` | `/api/admin/plugins/integrate/draft` | `plugins::draft_plugin_integration` |
| `POST` | `/api/admin/plugins/integrate/publish` | `plugins::publish_plugin_integration` |
| `POST` | `/api/admin/probe-models` | `models::probe_models_now` |
| `POST` | `/api/admin/projects/port` | `admin::projects::port_projects` |
| `POST` | `/api/admin/prompt-templates/batch-assign-tools` | `prompt_templates::batch_assign_tools_handler` |
| `POST` | `/api/admin/providers/:name/recharge-budget` | `environment::admin_recharge_provider_budget` |
| `POST` | `/api/admin/providers/:name/reset-cooldown` | `environment::admin_reset_provider_cooldown` |
| `POST` | `/api/admin/providers/:name/set-budget` | `environment::admin_set_provider_budget` |
| `POST` | `/api/admin/routing-matrix/auto-promote-now` | `environment::admin_routing_matrix_auto_promote_now` |
| `POST` | `/api/admin/sync-model-catalog` | `models::sync_model_catalog` |
| `POST` | `/api/admin/vector/compact` | `chat_learning::admin_run_vector_compaction` |
| `PUT` | `/api/admin/billing/prices/:id` | `billing::update_price` |
| `PUT` | `/api/admin/billing/quotas/:id` | `billing::update_quota` |
| `PUT` | `/api/admin/long-running/:id` | `long_running::update_pattern` |
| `PUT` | `/api/admin/profiles/:id` | `profiles::admin_update_profile` |
| `PUT` | `/api/admin/projects/:project_id/members/:user_id` | `admin::projects::update_project_member` |
| `PUT` | `/api/admin/routing/purpose-model/:purpose` | `admin::routing::update_purpose_model` |
| `PUT` | `/api/admin/setting/:key` | `settings::update_setting` |
| `PUT` | `/api/admin/users/:user_id/role` | `admin::users::update_user_role` |

## `/api/ai`

| Metodo | Path | Handler |
|---|---|---|
| `POST` | `/api/ai/generate-prompt` | `projects::generate_system_prompt` |

## `/api/billing`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/api/billing/session-usage` | `billing::get_session_usage` |
| `GET` | `/api/billing/usage/me` | `billing::my_usage_report` |

## `/api/chat`

| Metodo | Path | Handler |
|---|---|---|
| `DELETE` | `/api/chat/messages/:id` | `chat_messages::delete_chat_message` |
| `GET` | `/api/chat/agent-runs/:run_id` | `chat_agent::get_agent_run` |
| `GET` | `/api/chat/sessions` | `chat_sessions::list_chat_sessions` |
| `GET` | `/api/chat/sessions/:id/agent-stream` | `chat_agent::agent_stream` |
| `GET` | `/api/chat/sessions/:id/messages` | `chat_messages::list_chat_messages` |
| `GET` | `/api/chat/sessions/:session_id/active-run` | `chat_agent::get_active_run_for_session` |
| `POST` | `/api/chat` | `chat_messages::legacy_chat` |
| `POST` | `/api/chat/agent-runs/:run_id/cancel` | `chat_agent::cancel_agent_run` |
| `POST` | `/api/chat/agent-runs/:run_id/confirm` | `chat_agent::confirm_agent_run` |
| `POST` | `/api/chat/feedback-assist` | `chat_messages::feedback_assist_handler` |
| `POST` | `/api/chat/messages/:id/feedback-error` | `chat_messages::feedback_error` |
| `POST` | `/api/chat/messages/:id/feedback-positive` | `chat_messages::feedback_positive` |
| `POST` | `/api/chat/messages/:id/resend` | `chat_messages::resend_chat_message` |
| `POST` | `/api/chat/precheck` | `chat_messages::precheck_chat_message` |

## `/api/dashboard`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/api/dashboard` | `dashboard` |

## `/api/fs`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/api/fs/directories` | `projects::browse_server_directories` |
| `POST` | `/api/fs/directories/create` | `projects::create_server_directory` |

## `/api/gateway`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/api/gateway/providers` | `environment::gateway_providers_handler` |
| `POST` | `/api/gateway/reload` | `environment::gateway_reload_handler` |

## `/api/github`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/api/github/account` | `github::github_account` |
| `GET` | `/api/github/repositories` | `github::github_list_user_repositories` |
| `POST` | `/api/github/connect` | `github::github_connect` |

## `/api/health`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/api/health` | `health` |

## `/api/internal`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/api/internal/providers/status` | `environment::providers_status_internal` |
| `GET` | `/api/internal/routing/catalog` | `internal_routing::list_catalog` |
| `GET` | `/api/internal/routing/purpose` | `internal_routing::resolve_purpose` |
| `POST` | `/api/internal/learning/feedback` | `internal_learning::submit_feedback` |
| `POST` | `/api/internal/prompt-templates/batch-assign-tools` | `prompt_templates::internal_batch_assign_tools_handler` |
| `POST` | `/api/internal/provider-error` | `internal_routing::provider_error_handler` |
| `POST` | `/api/internal/routing/decide` | `internal_routing::decide_routing` |

## `/api/mcp-servers`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/api/mcp-servers` | `mcp_connectors::list_mcp_servers` |
| `POST` | `/api/mcp-servers/:id/test` | `mcp_connectors::test_mcp_server` |
| `PUT` | `/api/mcp-servers/:id` | `mcp_connectors::update_mcp_server` |
| `PUT` | `/api/mcp-servers/:id/toggle` | `mcp_connectors::toggle_mcp_server` |

## `/api/me`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/api/me` | `auth::me` |

## `/api/meta-docs`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/api/meta-docs/:id` | `meta_docs::routes::get_meta_doc` |
| `GET` | `/api/meta-docs/list` | `meta_docs::routes::list_meta_docs` |
| `POST` | `/api/meta-docs/ingest-commit` | `meta_docs::routes::ingest_commit_stub` |
| `POST` | `/api/meta-docs/refresh-all` | `meta_docs::routes::refresh_all_stub` |

## `/api/models`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/api/models` | `models::list_models` |
| `GET` | `/api/models/routing-preview` | `models::routing_preview` |

## `/api/orchestrator`

| Metodo | Path | Handler |
|---|---|---|
| `POST` | `/api/orchestrator/chat` | `chat_messages::legacy_chat` |

## `/api/plugins`

| Metodo | Path | Handler |
|---|---|---|
| `DELETE` | `/api/plugins/:id` | `plugins::uninstall_plugin` |
| `GET` | `/api/plugins/:id/health` | `plugins::get_plugin_health` |
| `GET` | `/api/plugins/catalog` | `plugins::list_plugin_catalog` |
| `GET` | `/api/plugins/figma/oauth/status` | `plugins::get_figma_oauth_status` |
| `GET` | `/api/plugins/installed` | `plugins::list_installed_plugins` |
| `POST` | `/api/plugins/:id/test` | `plugins::test_plugin` |
| `POST` | `/api/plugins/:id/update` | `plugins::update_plugin` |
| `POST` | `/api/plugins/figma/oauth/connect` | `plugins::start_figma_oauth` |
| `POST` | `/api/plugins/install` | `plugins::install_plugin` |
| `POST` | `/api/plugins/migrate-legacy/:id` | `plugins::migrate_legacy_mcp_server` |
| `PUT` | `/api/plugins/:id/toggle` | `plugins::toggle_plugin` |
| `PUT` | `/api/plugins/:id/tool-policy` | `plugins::update_plugin_tool_policy` |

## `/api/profiles`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/api/profiles` | `profiles::list_profiles` |
| `POST` | `/api/profiles/:id/default` | `profiles::set_default_profile` |
| `POST` | `/api/profiles/:id/fork` | `profiles::fork_profile` |
| `PUT` | `/api/profiles/:id` | `profiles::update_profile` |

## `/api/projects`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/api/projects/:id` | `projects::get_project` |
| `GET` | `/api/projects/:id/agent-processes/:process_id/stream` | `project_workspace::stream_agent_process_logs` |
| `GET` | `/api/projects/:id/billing/usage` | `billing::project_usage_report` |
| `GET` | `/api/projects/:id/changes` | `project_workspace::get_project_changes` |
| `GET` | `/api/projects/:id/custom-instructions` | `projects::get_custom_instructions` |
| `GET` | `/api/projects/:id/db` | `project_db_routes::get_project_db_config` |
| `GET` | `/api/projects/:id/db/connections` | `project_db_routes::list_project_db_connections` |
| `GET` | `/api/projects/:id/db/migrations` | `project_db_routes::list_project_migrations` |
| `GET` | `/api/projects/:id/deep-review/:job_id` | `projects::get_deep_review_status` |
| `GET` | `/api/projects/:id/documents` | `documents::list_documents` |
| `GET` | `/api/projects/:id/documents/:doc_id` | `documents::get_document` |
| `GET` | `/api/projects/:id/documents/:doc_id/download` | `documents::download_document` |
| `GET` | `/api/projects/:id/documents/:doc_id/versions` | `documents::list_versions` |
| `GET` | `/api/projects/:id/event-stream` | `dispatcher_routes::event_stream` |
| `GET` | `/api/projects/:id/file-lines` | `projects::get_file_lines` |
| `GET` | `/api/projects/:id/files` | `project_files::get_project_file` |
| `GET` | `/api/projects/:id/git/branches` | `project_git::git_branches` |
| `GET` | `/api/projects/:id/git/diff` | `project_git::git_diff` |
| `GET` | `/api/projects/:id/git/log` | `project_git::git_log` |
| `GET` | `/api/projects/:id/git/status` | `project_git::git_status` |
| `GET` | `/api/projects/:id/github/repositories` | `github::github_list_repositories` |
| `GET` | `/api/projects/:id/github/status` | `github::github_project_status` |
| `GET` | `/api/projects/:id/index-status` | `projects::get_index_status` |
| `GET` | `/api/projects/:id/insights` | `projects::get_project_insights` |
| `GET` | `/api/projects/:id/knowledge/notes` | `knowledge::routes::list_notes` |
| `GET` | `/api/projects/:id/knowledge/notes/:note_id` | `knowledge::routes::get_note` |
| `GET` | `/api/projects/:id/knowledge/tags` | `knowledge::routes::list_tags` |
| `GET` | `/api/projects/:id/output/channels` | `project_workspace::get_output_channels` |
| `GET` | `/api/projects/:id/output/events` | `project_workspace::get_output_events` |
| `GET` | `/api/projects/:id/playwright/artifact` | `project_workspace::serve_playwright_artifact` |
| `GET` | `/api/projects/:id/playwright/runs` | `project_workspace::get_playwright_runs` |
| `GET` | `/api/projects/:id/playwright/runs/:run_id` | `project_workspace::get_playwright_run_detail` |
| `GET` | `/api/projects/:id/playwright/runs/:run_id/stream` | `project_workspace::stream_playwright_run` |
| `GET` | `/api/projects/:id/port-allocations` | `project_workspace::get_port_allocations` |
| `GET` | `/api/projects/:id/ports` | `project_workspace::get_project_ports` |
| `GET` | `/api/projects/:id/preferences/git-ui` | `project_git::get_git_ui_preferences` |
| `GET` | `/api/projects/:id/problems` | `project_workspace::get_project_problems` |
| `GET` | `/api/projects/:id/quality-findings` | `projects::get_quality_findings` |
| `GET` | `/api/projects/:id/quality-scan/:scan_id` | `projects::get_quality_scan_status` |
| `GET` | `/api/projects/:id/run-configs` | `project_workspace::get_run_configs` |
| `GET` | `/api/projects/:id/run-configs/detect` | `project_workspace::detect_run_configs` |
| `GET` | `/api/projects/:id/search` | `project_files::search_project` |
| `GET` | `/api/projects/:id/security/audit` | `security::api::get_project_audit` |
| `GET` | `/api/projects/:id/security/quota` | `security::api::get_project_quota` |
| `GET` | `/api/projects/:id/services` | `project_workspace::get_project_services_status` |
| `GET` | `/api/projects/:id/services/wizard/detect` | `project_workspace::wizard_detect_services` |
| `GET` | `/api/projects/:id/snapshot` | `dispatcher_routes::project_snapshot` |
| `GET` | `/api/projects/:id/terminal-commands/stream` | `projects::terminal_commands_stream` |
| `GET` | `/api/projects/:id/tree` | `project_files::get_project_tree` |
| `GET` | `/api/projects/:id/workbench-state` | `project_workspace::get_workbench_state` |
| `GET` | `/api/projects/clone-target-exists` | `projects::clone_target_exists` |
| `GET` | `/api/projects/mine` | `projects::list_user_projects` |
| `PATCH` | `/api/projects/:id/default-profile` | `projects::patch_project_default_profile` |
| `POST` | `/api/projects/:id/agent-processes/:process_id/stop` | `project_workspace::stop_agent_process` |
| `POST` | `/api/projects/:id/agent-processes/clear-finished` | `project_workspace::clear_finished_processes` |
| `POST` | `/api/projects/:id/analyze` | `projects::analyze_project` |
| `POST` | `/api/projects/:id/db/config` | `project_db_routes::set_project_db_config` |
| `POST` | `/api/projects/:id/db/connections/:conn_id/set-primary` | `project_db_routes::set_primary_project_db_connection` |
| `POST` | `/api/projects/:id/db/detect` | `project_db_routes::detect_project_db` |
| `POST` | `/api/projects/:id/db/migrations/apply` | `project_db_routes::apply_project_migrations` |
| `POST` | `/api/projects/:id/db/migrations/rollback` | `project_db_routes::rollback_project_migration` |
| `POST` | `/api/projects/:id/db/override-request` | `project_db_routes::request_ddl_override` |
| `POST` | `/api/projects/:id/db/test-connection` | `project_db_routes::test_project_db_connection` |
| `POST` | `/api/projects/:id/deep-analyze` | `projects::deep_analyze_project` |
| `POST` | `/api/projects/:id/deep-review` | `projects::submit_deep_review` |
| `POST` | `/api/projects/:id/dispatcher/test` | `dispatcher_routes::dispatcher_test` |
| `POST` | `/api/projects/:id/execute-command` | `project_workspace::execute_command` |
| `POST` | `/api/projects/:id/files/create` | `project_files::create_project_entry` |
| `POST` | `/api/projects/:id/files/delete` | `project_files::delete_project_entry` |
| `POST` | `/api/projects/:id/files/rename` | `project_files::rename_project_entry` |
| `POST` | `/api/projects/:id/git/branch` | `project_git::git_create_branch` |
| `POST` | `/api/projects/:id/git/checkout` | `project_git::git_checkout` |
| `POST` | `/api/projects/:id/git/commit` | `project_git::git_commit` |
| `POST` | `/api/projects/:id/git/pull` | `project_git::git_pull` |
| `POST` | `/api/projects/:id/git/push` | `project_git::git_push` |
| `POST` | `/api/projects/:id/git/stage` | `project_git::git_stage` |
| `POST` | `/api/projects/:id/git/unstage` | `project_git::git_unstage` |
| `POST` | `/api/projects/:id/github/clone` | `github::github_clone_repository` |
| `POST` | `/api/projects/:id/github/create-repo` | `github::github_create_repo` |
| `POST` | `/api/projects/:id/github/publish` | `github::github_publish_project` |
| `POST` | `/api/projects/:id/github/publish-branch` | `github::github_publish_branch` |
| `POST` | `/api/projects/:id/github/pull-request` | `github::github_create_pull_request` |
| `POST` | `/api/projects/:id/knowledge/links` | `knowledge::routes::create_link` |
| `POST` | `/api/projects/:id/knowledge/similar` | `knowledge::routes::similar_handler` |
| `POST` | `/api/projects/:id/open` | `project_workspace::open_project` |
| `POST` | `/api/projects/:id/quality-findings/:finding_id/mark-fixed` | `projects::mark_finding_fixed` |
| `POST` | `/api/projects/:id/quality-scan` | `projects::run_quality_scan` |
| `POST` | `/api/projects/:id/quality-scan-file` | `projects::scan_single_file` |
| `POST` | `/api/projects/:id/reindex-stale` | `projects::reindex_stale_files` |
| `POST` | `/api/projects/:id/run-configs/:config_id/launch` | `project_workspace::launch_run_config` |
| `POST` | `/api/projects/:id/services/:service/:action` | `project_workspace::control_project_service` |
| `POST` | `/api/projects/:id/services/allocate-port` | `project_workspace::allocate_project_port` |
| `POST` | `/api/projects/:id/services/cleanup-ports` | `project_workspace::cleanup_project_ports` |
| `POST` | `/api/projects/:id/services/kill-orphan-processes` | `project_workspace::kill_project_orphan_processes` |
| `POST` | `/api/projects/:id/services/kill-port-process` | `project_workspace::kill_project_port_process` |
| `POST` | `/api/projects/:id/services/restart-all` | `project_workspace::restart_all_project_services` |
| `POST` | `/api/projects/:id/services/wizard/install` | `project_workspace::wizard_install_service` |
| `POST` | `/api/projects/:id/terminal-commands/:command_id/ack` | `projects::terminal_command_ack` |
| `POST` | `/api/projects/:id/terminal-commands/:command_id/finish` | `projects::terminal_command_finish` |
| `POST` | `/api/projects/:id/terminal-commands/presence` | `projects::terminal_presence` |
| `POST` | `/api/projects/:id/terminal/session` | `project_workspace::create_terminal_session` |
| `POST` | `/api/projects/clone` | `projects::clone_project` |
| `POST` | `/api/projects/register` | `projects::register_project` |
| `PUT` | `/api/projects/:id/run-configs/:config_id` | `project_workspace::update_run_config` |

## `/api/prompt-templates`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/api/prompt-templates` | `prompt_templates::list_templates_handler` |
| `GET` | `/api/prompt-templates/:key` | `prompt_templates::get_template_handler` |
| `POST` | `/api/prompt-templates/:key/ai-suggest` | `prompt_templates::ai_suggest_handler` |
| `POST` | `/api/prompt-templates/:key/disable` | `prompt_templates::disable_template_handler` |
| `POST` | `/api/prompt-templates/:key/enable` | `prompt_templates::enable_template_handler` |

## `/api/quality`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/api/quality/false-positive-stats` | `prompt_templates::false_positive_stats_handler` |
| `POST` | `/api/quality/findings/:id/false-positive` | `prompt_templates::mark_false_positive_handler` |

## `/auth`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/auth/figma/mcp/callback` | `plugins::figma_oauth_callback` |
| `GET` | `/auth/github` | `auth::github_login` |
| `GET` | `/auth/github/callback` | `auth::github_callback` |
| `POST` | `/auth/logout` | `auth::logout` |

## `/health`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/health` | `health` |

## `/internal`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/internal/nexus-database-stats` | `nexus_database_stats::nexus_database_stats` |
| `GET` | `/internal/settings/:key` | `settings::get_raw_value` |

## `/nexus`

| Metodo | Path | Handler |
|---|---|---|
| `GET` | `/nexus/healthz` | `nexus_bridge::nexus_healthz` |
| `GET` | `/nexus/metrics` | `nexus_bridge::nexus_prometheus` |
| `GET` | `/nexus/stats` | `nexus_bridge::nexus_stats` |
| `GET` | `/nexus/tools` | `nexus_bridge::nexus_tools` |
| `POST` | `/nexus/test-routing` | `nexus_bridge::nexus_test_routing` |
