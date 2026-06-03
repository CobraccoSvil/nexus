---
id: 04f1de6a-7fb0-41e0-abb9-56d58455c9fa
kind: schema
title: Schema Postgres
slug: postgres-tables
tags:
  - schema
  - postgres
source_commit: d6f2c3dcd0c0ff77d19a6b136ff7058325d9981a
source_files:
  - db/migrations/
auto_generated: true
created_at: 2026-05-23T07:20:00Z
updated_at: 2026-06-03T20:53:30Z
nexus_meta_version: 1
---

Tabelle attuali nello schema `public` di PostgreSQL. Generato automaticamente da `information_schema`.

Vedi anche: [[migrations-log]], [[qdrant-collections]], [[nexus-architetturale]], [[knowledge-base-funzionamento]], [[meta-vault-architettura]].

## `agent_processes`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `project_id` | uuid | NO | `—` |
| `session_id` | uuid | YES | `—` |
| `label` | text | NO | `''::text` |
| `command` | text | NO | `—` |
| `working_dir` | text | YES | `—` |
| `pid` | integer | YES | `—` |
| `status` | text | NO | `'starting'::text` |
| `exit_code` | integer | YES | `—` |
| `output` | text | NO | `''::text` |
| `error_output` | text | NO | `''::text` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `started_at` | timestamp with time zone | YES | `—` |
| `stopped_at` | timestamp with time zone | YES | `—` |
| `sandboxed` | boolean | NO | `false` |
| `kind` | text | NO | `'service'::text` |

## `agent_runs`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `session_id` | uuid | NO | `—` |
| `project_id` | uuid | NO | `—` |
| `user_id` | uuid | NO | `—` |
| `run_message_id` | uuid | YES | `—` |
| `status` | text | NO | `'running'::text` |
| `automation_mode` | text | NO | `'confirm'::text` |
| `provider` | text | YES | `—` |
| `model` | text | YES | `—` |
| `iteration_count` | integer | NO | `0` |
| `final_answer` | text | YES | `—` |
| `pending_actions_json` | jsonb | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `completed_at` | timestamp with time zone | YES | `—` |
| `parent_run_id` | uuid | YES | `—` |
| `messages_json` | text | YES | `—` |
| `supervisor_mode` | text | NO | `'none'::text` |
| `nexus_override_applied` | boolean | NO | `false` |
| `nexus_agent_type` | text | YES | `—` |
| `nexus_q_value` | real | YES | `—` |
| `nexus_task_type` | text | YES | `—` |
| `prompt_tokens` | integer | NO | `0` |
| `completion_tokens` | integer | NO | `0` |
| `total_tokens` | integer | NO | `0` |
| `total_cost` | double precision | NO | `0.0` |

## `agent_steps`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `run_id` | uuid | NO | `—` |
| `step_index` | integer | NO | `—` |
| `tool_name` | text | NO | `—` |
| `tool_input` | jsonb | NO | `—` |
| `tool_result` | text | YES | `—` |
| `status` | text | NO | `'running'::text` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `ai_model_health_history`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | bigint | NO | `nextval('ai_model_health_history_id_seq'::regclass)` |
| `provider` | text | NO | `—` |
| `model` | text | NO | `—` |
| `healthy` | boolean | NO | `—` |
| `latency_ms` | integer | YES | `—` |
| `error_kind` | text | YES | `—` |
| `error_message` | text | YES | `—` |
| `checked_at` | timestamp with time zone | NO | `now()` |

## `ai_price_catalog`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `provider` | text | NO | `—` |
| `model` | text | NO | `—` |
| `input_cost_per_million_tokens` | numeric | NO | `—` |
| `output_cost_per_million_tokens` | numeric | NO | `—` |
| `currency` | text | NO | `—` |
| `effective_from` | timestamp with time zone | NO | `now()` |
| `effective_to` | timestamp with time zone | YES | `—` |
| `is_enabled` | boolean | NO | `true` |
| `created_by` | uuid | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |
| `display_name` | text | NO | `''::text` |
| `context_window` | integer | NO | `8192` |
| `performance_tier` | text | NO | `'medium'::text` |
| `speed_tier` | text | NO | `'medium'::text` |
| `capabilities` | jsonb | NO | `'[]'::jsonb` |
| `supports_tool_use` | boolean | NO | `true` |
| `batch_discount_pct` | integer | NO | `0` |
| `is_featured` | boolean | NO | `false` |
| `cache_read_cost_per_million_tokens` | numeric | YES | `—` |
| `cache_creation_cost_per_million_tokens` | numeric | YES | `—` |
| `consecutive_failures` | integer | NO | `0` |
| `auto_disabled_at` | timestamp with time zone | YES | `—` |
| `auto_disabled_reason` | text | YES | `—` |
| `consecutive_tool_failures` | integer | NO | `0` |
| `is_thinking` | boolean | NO | `false` |

## `ai_price_catalog_audit`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `occurred_at` | timestamp with time zone | NO | `now()` |
| `provider` | text | NO | `—` |
| `model` | text | NO | `—` |
| `action` | text | NO | `—` |
| `details` | jsonb | NO | `'{}'::jsonb` |

## `ai_quota_policies`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `scope_type` | text | NO | `—` |
| `user_id` | uuid | YES | `—` |
| `project_id` | uuid | YES | `—` |
| `token_limit` | bigint | YES | `—` |
| `cost_limit` | numeric | YES | `—` |
| `currency` | text | YES | `—` |
| `valid_from` | timestamp with time zone | NO | `—` |
| `valid_to` | timestamp with time zone | NO | `—` |
| `is_enabled` | boolean | NO | `true` |
| `created_by` | uuid | YES | `—` |
| `note` | text | NO | `''::text` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `ai_response_feedback`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `project_id` | uuid | NO | `—` |
| `session_id` | uuid | NO | `—` |
| `message_id` | uuid | NO | `—` |
| `orchestrator_run_id` | uuid | YES | `—` |
| `user_id` | uuid | NO | `—` |
| `feedback_type` | text | NO | `'error'::text` |
| `intent` | text | YES | `—` |
| `provider` | text | YES | `—` |
| `model` | text | YES | `—` |
| `error_comment` | text | NO | `—` |
| `status` | text | NO | `'open'::text` |
| `review_note` | text | YES | `—` |
| `reviewed_by` | uuid | YES | `—` |
| `reviewed_at` | timestamp with time zone | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `ai_usage_ledger`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `run_id` | uuid | YES | `—` |
| `user_id` | uuid | NO | `—` |
| `project_id` | uuid | NO | `—` |
| `provider` | text | NO | `—` |
| `model` | text | NO | `—` |
| `prompt_tokens` | integer | NO | `0` |
| `completion_tokens` | integer | NO | `0` |
| `total_tokens` | integer | NO | `0` |
| `input_cost` | numeric | NO | `0` |
| `output_cost` | numeric | NO | `0` |
| `total_cost` | numeric | NO | `0` |
| `currency` | text | NO | `—` |
| `status` | text | NO | `—` |
| `rejection_reason` | text | YES | `—` |
| `details` | jsonb | NO | `'{}'::jsonb` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `finalized_at` | timestamp with time zone | YES | `—` |
| `cache_read_tokens` | bigint | NO | `0` |
| `cache_creation_tokens` | bigint | NO | `0` |
| `cache_read_cost` | numeric | NO | `0` |
| `cache_creation_cost` | numeric | NO | `0` |

## `brain_learning_interactions`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | bigint | NO | `nextval('brain_learning_interactions_id_seq'::regclass)` |
| `thread_id` | text | NO | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `task_type` | text | NO | `—` |
| `behavior_mode` | text | NO | `'bilanciata'::text` |
| `user_input` | text | NO | `—` |
| `agent_output` | text | NO | `''::text` |
| `provider` | text | YES | `—` |
| `model` | text | YES | `—` |
| `latency_ms` | real | YES | `—` |
| `token_usage` | integer | YES | `—` |
| `feedback_score` | real | YES | `—` |
| `qdrant_id` | text | YES | `—` |
| `metadata` | jsonb | YES | `—` |

## `brain_task_stats`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `task_type` | text | NO | `—` |
| `total_count` | integer | NO | `0` |
| `success_count` | integer | NO | `0` |
| `avg_latency_ms` | real | NO | `0.0` |
| `avg_feedback` | real | NO | `0.0` |
| `last_updated` | timestamp with time zone | NO | `now()` |

## `change_drafts`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `project_id` | uuid | YES | `—` |
| `requested_by_user` | uuid | YES | `—` |
| `trigger_kind` | text | NO | `—` |
| `summary` | text | NO | `''::text` |
| `draft_json` | jsonb | NO | `—` |
| `status` | text | NO | `'pending'::text` |
| `applied_at` | timestamp with time zone | YES | `—` |
| `related_commit_sha` | text | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `chat_message_attachments`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `message_id` | uuid | NO | `—` |
| `project_id` | uuid | NO | `—` |
| `file_name` | text | NO | `—` |
| `file_path` | text | NO | `—` |
| `mime_type` | text | NO | `—` |
| `size_bytes` | bigint | NO | `—` |
| `kind` | text | NO | `—` |
| `kb_note_id` | uuid | YES | `—` |
| `indexed_at` | timestamp with time zone | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `chunk_count` | integer | NO | `0` |
| `content_hash` | text | YES | `—` |
| `display_id` | text | YES | `—` |

## `chat_messages`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `uuid_generate_v4()` |
| `session_id` | uuid | NO | `—` |
| `role` | text | NO | `—` |
| `content` | text | NO | `—` |
| `metadata` | jsonb | NO | `'{}'::jsonb` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `project_id` | uuid | NO | `—` |
| `request_message_id` | uuid | YES | `—` |
| `deleted_at` | timestamp with time zone | YES | `—` |
| `deleted_by_user_id` | uuid | YES | `—` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `chat_sessions`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `uuid_generate_v4()` |
| `project_id` | uuid | NO | `—` |
| `user_id` | uuid | YES | `—` |
| `profile_id` | uuid | YES | `—` |
| `title` | text | NO | `'New Session'::text` |
| `status` | text | NO | `'active'::text` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |
| `preferred_provider` | text | YES | `—` |
| `preferred_model` | text | YES | `—` |
| `privacy_rerouted_at` | timestamp with time zone | YES | `—` |

## `file_index_hashes`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `project_id` | uuid | NO | `—` |
| `file_path` | text | NO | `—` |
| `content_hash` | text | NO | `—` |
| `indexed_at` | timestamp with time zone | NO | `now()` |

## `git_operations`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `user_id` | uuid | NO | `—` |
| `project_id` | uuid | NO | `—` |
| `workspace_id` | uuid | YES | `—` |
| `branch` | text | YES | `—` |
| `operation` | text | NO | `—` |
| `status` | text | NO | `—` |
| `stdout` | text | NO | `''::text` |
| `stderr` | text | NO | `''::text` |
| `metadata` | jsonb | NO | `'{}'::jsonb` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `git_remotes`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `repository_id` | uuid | NO | `—` |
| `name` | text | NO | `—` |
| `fetch_url` | text | NO | `—` |
| `push_url` | text | NO | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `git_status_snapshots`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `project_id` | uuid | NO | `—` |
| `workspace_id` | uuid | YES | `—` |
| `branch` | text | YES | `—` |
| `status_json` | jsonb | NO | `'{}'::jsonb` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `github_connections`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `user_id` | uuid | NO | `—` |
| `github_user_id` | bigint | YES | `—` |
| `github_username` | text | YES | `—` |
| `connection_status` | text | NO | `'connected'::text` |
| `access_token_encrypted` | bytea | YES | `—` |
| `refresh_token_encrypted` | bytea | YES | `—` |
| `token_scope` | text | NO | `''::text` |
| `access_token_expires_at` | timestamp with time zone | YES | `—` |
| `refresh_token_expires_at` | timestamp with time zone | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `jobs`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `uuid_generate_v4()` |
| `project_id` | uuid | NO | `—` |
| `kind` | text | NO | `—` |
| `status` | text | NO | `'queued'::text` |
| `input` | jsonb | NO | `'{}'::jsonb` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |
| `output_log` | text | YES | `—` |
| `progress` | jsonb | NO | `'{}'::jsonb` |

## `langgraph_checkpoints`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `thread_id` | text | NO | `—` |
| `checkpoint_id` | text | NO | `—` |
| `checkpoint_data` | jsonb | NO | `—` |
| `metadata` | jsonb | NO | `'{}'::jsonb` |
| `versions` | jsonb | NO | `'{}'::jsonb` |
| `created_at` | timestamp without time zone | YES | `CURRENT_TIMESTAMP` |

## `learning_decisions_log`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `project_id` | uuid | NO | `—` |
| `intent` | text | NO | `—` |
| `provider` | text | YES | `—` |
| `model` | text | YES | `—` |
| `confidence` | numeric | NO | `—` |
| `feedback_count` | integer | NO | `0` |
| `window_days` | integer | NO | `7` |
| `action` | text | NO | `—` |
| `status` | text | NO | `'applied'::text` |
| `snapshot_id` | uuid | YES | `—` |
| `details` | jsonb | NO | `'{}'::jsonb` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `applied_at` | timestamp with time zone | NO | `now()` |
| `rolled_back_at` | timestamp with time zone | YES | `—` |

## `learning_policy_snapshots`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `project_id` | uuid | NO | `—` |
| `intent` | text | NO | `—` |
| `previous_chain` | text | NO | `—` |
| `next_chain` | text | NO | `—` |
| `baseline_error_count` | bigint | NO | `0` |
| `snapshot_reason` | text | NO | `'auto_apply'::text` |
| `created_by_user_id` | uuid | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `long_running_patterns`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `pattern` | text | NO | `—` |
| `description` | text | NO | `''::text` |
| `enabled` | boolean | NO | `true` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `mcp_server_tools`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `server_id` | uuid | NO | `—` |
| `tool_name` | text | NO | `—` |
| `description` | text | YES | `—` |
| `input_schema` | jsonb | NO | `'{}'::jsonb` |
| `discovered_at` | timestamp with time zone | NO | `now()` |
| `embedding_hash` | text | YES | `—` |
| `embedded_at` | timestamp with time zone | YES | `—` |

## `mcp_servers`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `user_id` | uuid | YES | `—` |
| `project_id` | uuid | YES | `—` |
| `name` | text | NO | `—` |
| `description` | text | YES | `—` |
| `icon_url` | text | YES | `—` |
| `transport` | text | NO | `—` |
| `url` | text | YES | `—` |
| `command` | text | YES | `—` |
| `args` | jsonb | NO | `'[]'::jsonb` |
| `env_vars` | jsonb | NO | `'{}'::jsonb` |
| `headers` | jsonb | NO | `'{}'::jsonb` |
| `enabled` | boolean | NO | `true` |
| `scope` | text | NO | `'user'::text` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |
| `plugin_instance_id` | uuid | YES | `—` |

## `memory_entries`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `namespace_id` | uuid | NO | `—` |
| `entry_key` | text | NO | `—` |
| `value` | jsonb | NO | `—` |
| `embedding` | ARRAY | YES | `—` |
| `version` | bigint | NO | `1` |
| `vector_clock` | jsonb | NO | `'{}'::jsonb` |
| `written_by` | text | YES | `—` |
| `deleted` | boolean | NO | `false` |
| `expires_at` | timestamp with time zone | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `memory_namespaces`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `ns_key` | text | NO | `—` |
| `ns_type` | text | NO | `'swarm'::text` |
| `project_id` | uuid | YES | `—` |
| `ttl_seconds` | integer | YES | `—` |
| `merge_strategy` | text | NO | `'lww'::text` |
| `max_entries` | integer | NO | `10000` |
| `active` | boolean | NO | `true` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `expires_at` | timestamp with time zone | YES | `—` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_agent_clarifications`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `run_id` | uuid | NO | `—` |
| `project_id` | uuid | NO | `—` |
| `questions` | jsonb | NO | `—` |
| `user_answers` | jsonb | YES | `—` |
| `applied_defaults` | jsonb | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `answered_at` | timestamp with time zone | YES | `—` |

## `nexus_agent_meta_steps`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | bigint | NO | `nextval('nexus_agent_meta_steps_id_seq'::regclass)` |
| `run_id` | uuid | NO | `—` |
| `kind` | text | NO | `—` |
| `title` | text | NO | `''::text` |
| `payload` | jsonb | NO | `'{}'::jsonb` |
| `correlation_id` | text | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `nexus_agent_plans`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `run_id` | uuid | NO | `—` |
| `project_id` | uuid | NO | `—` |
| `thread_id` | text | NO | `—` |
| `acceptance_criteria` | jsonb | NO | `'[]'::jsonb` |
| `planner_model` | text | NO | `—` |
| `approved_at` | timestamp with time zone | YES | `—` |
| `approved_by` | uuid | YES | `—` |
| `score` | double precision | YES | `—` |
| `plan_revisions` | integer | NO | `0` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `rationale` | text | YES | `—` |
| `constraints` | jsonb | NO | `'[]'::jsonb` |
| `alternatives` | jsonb | NO | `'[]'::jsonb` |

## `nexus_agent_reflections`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | bigint | NO | `nextval('nexus_agent_reflections_id_seq'::regclass)` |
| `run_id` | uuid | YES | `—` |
| `prompt_key` | text | NO | `—` |
| `prompt_version` | integer | NO | `1` |
| `score` | numeric | YES | `—` |
| `dimensions` | jsonb | YES | `—` |
| `weaknesses` | ARRAY | NO | `'{}'::text[]` |
| `suggestions` | ARRAY | NO | `'{}'::text[]` |
| `model_used` | text | YES | `—` |
| `latency_ms` | integer | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `nexus_agent_stats`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `agent_key` | text | NO | `—` |
| `total_executions` | bigint | NO | `0` |
| `successful_executions` | bigint | NO | `0` |
| `failed_executions` | bigint | NO | `0` |
| `avg_quality_score` | real | NO | `0.0` |
| `avg_execution_ms` | real | NO | `0.0` |
| `last_executed_at` | timestamp with time zone | YES | `—` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_agent_todos`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `run_id` | uuid | NO | `—` |
| `project_id` | uuid | NO | `—` |
| `seq` | integer | NO | `—` |
| `content` | text | NO | `—` |
| `status` | text | NO | `—` |
| `priority` | text | NO | `'normal'::text` |
| `acceptance_criteria` | jsonb | NO | `'[]'::jsonb` |
| `verify_failures` | integer | NO | `0` |
| `iteration_seen` | integer | NO | `0` |
| `updated_at` | timestamp with time zone | NO | `now()` |
| `depends_on` | ARRAY | NO | `'{}'::uuid[]` |
| `dep_keys` | ARRAY | YES | `—` |
| `node_key` | text | YES | `—` |
| `dag_layer` | integer | YES | `—` |
| `edited_by` | text | YES | `—` |
| `carry_over` | boolean | NO | `false` |
| `origin_run_id` | uuid | YES | `—` |

## `nexus_agent_types`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `agent_key` | text | NO | `—` |
| `display_name` | text | NO | `—` |
| `category` | USER-DEFINED | NO | `'core'::agent_category` |
| `description` | text | NO | `''::text` |
| `profile_embedding` | ARRAY | YES | `—` |
| `default_config` | jsonb | NO | `'{}'::jsonb` |
| `enabled` | boolean | NO | `true` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_agent_verifier_runs`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `run_id` | uuid | NO | `—` |
| `todo_id` | uuid | YES | `—` |
| `cycle` | integer | NO | `—` |
| `criteria_results` | jsonb | NO | `—` |
| `passed` | boolean | NO | `—` |
| `duration_ms` | integer | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `nexus_classifier_provider_chain`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | bigint | NO | `nextval('nexus_classifier_provider_chain_id_seq'::regclass)` |
| `provider` | text | NO | `—` |
| `model_id` | text | NO | `—` |
| `priority` | integer | NO | `100` |
| `is_active` | boolean | NO | `true` |
| `rationale` | text | NO | `''::text` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_command_hints`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `pattern` | text | NO | `—` |
| `pattern_kind` | text | NO | `'substring'::text` |
| `hint_text` | text | NO | `—` |
| `severity` | text | NO | `'warning'::text` |
| `enabled` | boolean | NO | `true` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_conversation_summaries`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | bigint | NO | `nextval('nexus_conversation_summaries_id_seq'::regclass)` |
| `thread_id` | text | NO | `—` |
| `replaced_msg_count` | integer | NO | `—` |
| `summary_text` | text | NO | `—` |
| `model_used` | text | NO | `—` |
| `latency_ms` | integer | NO | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `nexus_dependency_health`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | bigint | NO | `nextval('nexus_dependency_health_id_seq'::regclass)` |
| `dependency` | text | NO | `—` |
| `healthy` | boolean | NO | `—` |
| `latency_ms` | integer | YES | `—` |
| `error_kind` | text | YES | `—` |
| `error_message` | text | YES | `—` |
| `checked_at` | timestamp with time zone | NO | `now()` |

## `nexus_dev_diagnostics`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `pattern_regex` | text | NO | `—` |
| `category` | text | NO | `—` |
| `fix_template` | text | NO | `—` |
| `severity` | text | NO | `'warning'::text` |
| `confidence` | integer | NO | `80` |
| `description` | text | NO | `''::text` |
| `enabled` | boolean | NO | `true` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_e2e_runs`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `scenario` | text | NO | `—` |
| `status` | text | NO | `—` |
| `duration_ms` | integer | NO | `0` |
| `artifact_path` | text | YES | `—` |
| `log_excerpt` | text | YES | `—` |
| `failed_assertion` | text | YES | `—` |
| `started_at` | timestamp with time zone | NO | `now()` |
| `completed_at` | timestamp with time zone | YES | `—` |

## `nexus_events_audit`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `event_id` | uuid | NO | `—` |
| `project_id` | uuid | NO | `—` |
| `seq` | bigint | NO | `—` |
| `ts` | timestamp with time zone | NO | `now()` |
| `topic` | text | NO | `—` |
| `kind` | text | NO | `—` |
| `payload` | jsonb | NO | `—` |
| `enrichment` | jsonb | YES | `—` |
| `actor_user_id` | uuid | YES | `—` |

## `nexus_intent_capability`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `intent` | text | NO | `—` |
| `base_tier` | text | NO | `—` |
| `base_capability` | text | NO | `—` |
| `preferred_provider` | text | YES | `—` |
| `medium_token_threshold` | integer | YES | `—` |
| `heavy_token_threshold` | integer | YES | `—` |
| `notes` | text | YES | `—` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_intent_routing_requirements`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `intent` | text | NO | `—` |
| `behavior_mode` | text | NO | `—` |
| `required_capabilities` | ARRAY | NO | `'{}'::text[]` |
| `requires_tool_use` | boolean | NO | `false` |
| `preferred_tier` | text | NO | `'medium'::text` |
| `weight_tier` | real | NO | `0.35` |
| `weight_cost` | real | NO | `0.25` |
| `weight_context` | real | NO | `0.20` |
| `weight_capabilities` | real | NO | `0.20` |
| `cost_direction` | text | NO | `'asc'::text` |

## `nexus_meta_doc_changes`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | bigint | NO | `nextval('nexus_meta_doc_changes_id_seq'::regclass)` |
| `commit_sha` | text | NO | `—` |
| `commit_msg` | text | NO | `''::text` |
| `author` | text | YES | `—` |
| `files_changed` | ARRAY | NO | `'{}'::text[]` |
| `significance` | real | NO | `0.5` |
| `generated_doc_id` | uuid | YES | `—` |
| `processed_at` | timestamp with time zone | NO | `now()` |

## `nexus_meta_doc_links`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `from_doc_id` | uuid | NO | `—` |
| `to_doc_id` | uuid | NO | `—` |
| `rel_type` | text | NO | `'relates'::text` |
| `created_by` | text | NO | `—` |
| `confidence` | real | NO | `1.0` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `nexus_meta_docs`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `kind` | text | NO | `—` |
| `title` | text | NO | `—` |
| `slug` | text | NO | `—` |
| `body_md` | text | NO | `—` |
| `vault_file_path` | text | NO | `—` |
| `vault_file_hash` | text | NO | `—` |
| `source_commit` | text | YES | `—` |
| `source_files` | ARRAY | NO | `'{}'::text[]` |
| `auto_generated` | boolean | NO | `true` |
| `tags` | ARRAY | NO | `'{}'::text[]` |
| `qdrant_point_id` | text | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_model_escalation_chain`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `provider` | text | NO | `—` |
| `base_model` | text | NO | `—` |
| `escalation_position` | integer | NO | `—` |
| `escalation_model` | text | NO | `—` |
| `capability_tier` | text | NO | `—` |
| `is_active` | boolean | NO | `true` |

## `nexus_port_allocations`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `project_id` | uuid | NO | `—` |
| `port` | integer | NO | `—` |
| `label` | text | NO | `''::text` |
| `allocation_mode` | text | NO | `'auto'::text` |
| `run_config_id` | uuid | YES | `—` |
| `service_unit` | text | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_project_flags`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `project_id` | uuid | NO | `—` |
| `key` | text | NO | `—` |
| `value` | jsonb | YES | `—` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_project_insights`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | bigint | NO | `nextval('nexus_project_insights_id_seq'::regclass)` |
| `project_id` | uuid | NO | `—` |
| `insight_version` | integer | NO | `1` |
| `insights` | jsonb | NO | `—` |
| `prompt_key` | text | NO | `'agent.project.analyzer'::text` |
| `prompt_version` | integer | NO | `—` |
| `model_used` | text | YES | `—` |
| `duration_ms` | integer | YES | `—` |
| `config_files_count` | integer | NO | `0` |
| `status` | text | NO | `'completed'::text` |
| `error_message` | text | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `nexus_project_instructions`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `project_id` | uuid | NO | `—` |
| `file_path` | text | NO | `'.nexus/project-instructions.md'::text` |
| `content_cache` | text | YES | `—` |
| `content_hash` | text | YES | `—` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_prompt_template_history`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | integer | NO | `nextval('nexus_prompt_template_history_id_seq'::regclass)` |
| `template_id` | integer | NO | `—` |
| `content` | text | NO | `—` |
| `version` | integer | NO | `—` |
| `changed_by` | text | NO | `'system'::text` |
| `changed_at` | timestamp with time zone | NO | `now()` |
| `change_note` | text | YES | `—` |

## `nexus_prompt_templates`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | integer | NO | `nextval('nexus_prompt_templates_id_seq'::regclass)` |
| `key` | text | NO | `—` |
| `category` | text | NO | `—` |
| `title` | text | NO | `—` |
| `content` | text | NO | `—` |
| `is_active` | boolean | NO | `true` |
| `version` | integer | NO | `1` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |
| `updated_by` | text | NO | `'system'::text` |
| `usage_context` | text | YES | `—` |
| `mcp_tools_json` | jsonb | YES | `'[]'::jsonb` |
| `suggested_tools_json` | jsonb | YES | `'[]'::jsonb` |
| `schema_type` | text | NO | `'plain'::text` |
| `placeholder_vars` | jsonb | NO | `'[]'::jsonb` |
| `experimental` | boolean | NO | `false` |

## `nexus_provider_capabilities`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `provider` | text | NO | `—` |
| `model` | text | NO | `—` |
| `tool_use` | boolean | NO | `false` |
| `vision` | boolean | NO | `false` |
| `thinking` | boolean | NO | `false` |
| `max_context_tokens` | integer | NO | `8192` |
| `default_max_output_tokens` | integer | NO | `4096` |
| `max_output_tokens_hard` | integer | NO | `16384` |
| `tool_choice_style` | text | NO | `'openai_auto'::text` |
| `tool_choice_first_turn_force` | boolean | NO | `false` |
| `schema_strict` | boolean | NO | `false` |
| `schema_dialect` | text | NO | `'openai_loose'::text` |
| `tool_call_format` | text | NO | `'openai_delta'::text` |
| `max_tools_in_request` | integer | YES | `—` |
| `supports_prompt_cache` | boolean | NO | `false` |
| `prompt_cache_dialect` | text | YES | `—` |
| `supports_parallel_tools` | boolean | NO | `true` |
| `stop_reason_dialect` | text | NO | `'openai_finish_reason'::text` |
| `soft_failure_iter_threshold` | integer | NO | `3` |
| `soft_failure_content_threshold` | integer | NO | `800` |
| `history_keep_recent_messages` | integer | NO | `12` |
| `history_max_old_tool_result_chars` | integer | NO | `2000` |
| `request_timeout_seconds` | integer | NO | `60` |
| `connect_timeout_seconds` | integer | NO | `10` |
| `tool_result_max_chars` | integer | NO | `6000` |
| `tool_result_max_bytes` | integer | NO | `512000` |
| `tool_result_max_lines` | integer | NO | `2000` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_provider_default_model`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `provider` | text | NO | `—` |
| `model_id` | text | NO | `—` |
| `notes` | text | YES | `—` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_provider_health`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `provider` | text | NO | `—` |
| `billing_cooldown_until` | timestamp with time zone | YES | `—` |
| `last_error` | text | YES | `—` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_provider_health_history`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | bigint | NO | `nextval('nexus_provider_health_history_id_seq'::regclass)` |
| `provider` | text | NO | `—` |
| `healthy` | boolean | NO | `—` |
| `latency_ms` | integer | YES | `—` |
| `error_kind` | text | YES | `—` |
| `error_message` | text | YES | `—` |
| `checked_at` | timestamp with time zone | NO | `now()` |

## `nexus_provider_intent_health`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `provider` | text | NO | `—` |
| `model` | text | NO | `—` |
| `intent_subkind` | text | NO | `—` |
| `success_count` | bigint | NO | `0` |
| `failure_count` | bigint | NO | `0` |
| `soft_failure_count` | bigint | NO | `0` |
| `last_seen_at` | timestamp with time zone | NO | `now()` |
| `last_success_at` | timestamp with time zone | YES | `—` |
| `last_failure_at` | timestamp with time zone | YES | `—` |
| `cooldown_until` | timestamp with time zone | YES | `—` |
| `cooldown_reason` | text | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_purpose_model`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `purpose` | text | NO | `—` |
| `provider` | text | NO | `—` |
| `model_id` | text | NO | `—` |
| `notes` | text | YES | `—` |
| `updated_at` | timestamp with time zone | NO | `now()` |
| `tier` | text | YES | `—` |
| `required_capability` | text | YES | `—` |
| `requires_tool_use` | boolean | NO | `false` |

## `nexus_q_values`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `task_type` | text | NO | `—` |
| `agent_key` | text | NO | `—` |
| `q_value` | real | NO | `0.5` |
| `visit_count` | bigint | NO | `0` |
| `success_count` | bigint | NO | `0` |
| `failure_count` | bigint | NO | `0` |
| `last_reward` | real | YES | `—` |
| `avg_reward` | real | NO | `0.0` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_quality_scans`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | bigint | NO | `nextval('nexus_quality_scans_id_seq'::regclass)` |
| `project_id` | uuid | NO | `—` |
| `status` | text | NO | `'running'::text` |
| `files_scanned` | integer | YES | `—` |
| `total_findings` | integer | YES | `—` |
| `by_severity` | jsonb | YES | `—` |
| `by_category` | jsonb | YES | `—` |
| `error_message` | text | YES | `—` |
| `duration_ms` | integer | YES | `—` |
| `started_at` | timestamp with time zone | NO | `now()` |
| `completed_at` | timestamp with time zone | YES | `—` |

## `nexus_replication_log`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | bigint | NO | `nextval('nexus_replication_log_id_seq'::regclass)` |
| `namespace_id` | text | NO | `—` |
| `key` | text | NO | `—` |
| `value` | jsonb | NO | `'{}'::jsonb` |
| `author` | text | NO | `''::text` |
| `replicated_at` | timestamp with time zone | NO | `now()` |

## `nexus_resource_audit`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | bigint | NO | `nextval('nexus_resource_audit_id_seq'::regclass)` |
| `ts` | timestamp with time zone | NO | `now()` |
| `project_id` | uuid | NO | `—` |
| `actor` | text | NO | `—` |
| `actor_user_id` | uuid | YES | `—` |
| `actor_session_id` | uuid | YES | `—` |
| `action` | text | NO | `—` |
| `resource_kind` | text | NO | `—` |
| `resource_id` | text | YES | `—` |
| `outcome` | text | NO | `—` |
| `details` | jsonb | NO | `'{}'::jsonb` |

## `nexus_resource_quotas`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `project_id` | uuid | NO | `—` |
| `max_ports` | integer | NO | `20` |
| `max_memory_mb` | integer | NO | `4096` |
| `max_disk_mb` | integer | NO | `10240` |
| `max_containers` | integer | NO | `5` |
| `max_db_pool_size` | integer | NO | `10` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_routing_decisions`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | bigint | NO | `nextval('nexus_routing_decisions_id_seq'::regclass)` |
| `decided_at` | timestamp with time zone | NO | `now()` |
| `prompt_hash` | text | NO | `—` |
| `estimated_tokens` | integer | YES | `—` |
| `behavior_mode` | text | YES | `—` |
| `intent` | text | YES | `—` |
| `classifier_source` | text | YES | `—` |
| `classifier_confidence` | real | YES | `—` |
| `classifier_cached` | boolean | YES | `—` |
| `selected_provider` | text | NO | `—` |
| `selected_model` | text | NO | `—` |
| `decision_source` | text | YES | `—` |
| `rationale` | text | YES | `—` |
| `no_capable_provider` | boolean | NO | `false` |
| `providers_in_cooldown` | ARRAY | YES | `—` |
| `fallback_triggered` | boolean | NO | `false` |
| `latency_ms` | integer | YES | `—` |
| `actual_quality_score` | real | YES | `—` |

## `nexus_routing_matrix`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `intent` | text | NO | `—` |
| `behavior_mode` | text | NO | `—` |
| `provider` | text | NO | `—` |
| `model_id` | text | NO | `—` |
| `priority` | integer | NO | `100` |
| `is_active` | boolean | NO | `true` |
| `notes` | text | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |
| `escalation_threshold_tokens` | integer | YES | `—` |
| `escalation_provider` | text | YES | `—` |
| `escalation_model_id` | text | YES | `—` |
| `manual_override` | boolean | NO | `false` |
| `last_auto_promote_at` | timestamp with time zone | YES | `—` |
| `auto_promote_score` | real | YES | `—` |

## `nexus_routing_slots_matrix`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | bigint | NO | `nextval('nexus_routing_slots_matrix_id_seq'::regclass)` |
| `action_verb` | text | NO | `—` |
| `target_type` | text | NO | `—` |
| `framework` | text | NO | `'*'::text` |
| `scope` | text | NO | `—` |
| `provider` | text | NO | `—` |
| `model_id` | text | NO | `—` |
| `priority` | integer | NO | `100` |
| `is_active` | boolean | NO | `true` |
| `rationale` | text | NO | `''::text` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_security_audit`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `project_id` | uuid | NO | `—` |
| `user_id` | uuid | YES | `—` |
| `session_id` | uuid | YES | `—` |
| `tool_name` | text | NO | `—` |
| `command_excerpt` | text | NO | `—` |
| `category` | text | NO | `—` |
| `message` | text | NO | `—` |
| `blocked` | boolean | NO | `true` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `nexus_shared_directives`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `key` | text | NO | `—` |
| `content` | text | NO | `—` |
| `scope` | text | NO | `'agent'::text` |
| `priority` | integer | NO | `100` |
| `is_active` | boolean | NO | `true` |
| `description` | text | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_subagent_definitions`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `kind` | text | NO | `—` |
| `description` | text | NO | `—` |
| `prompt_key` | text | NO | `—` |
| `tool_whitelist` | ARRAY | NO | `—` |
| `model_purpose` | text | NO | `—` |
| `max_iterations` | integer | NO | `25` |
| `timeout_s` | integer | NO | `300` |
| `is_background` | boolean | NO | `false` |
| `is_enabled` | boolean | NO | `true` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `nexus_subagent_runs`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `parent_run_id` | uuid | NO | `—` |
| `project_id` | uuid | NO | `—` |
| `kind` | text | NO | `—` |
| `task_description` | text | NO | `—` |
| `context_blob` | text | YES | `—` |
| `expected_format` | text | YES | `—` |
| `status` | text | NO | `—` |
| `is_background` | boolean | NO | `false` |
| `resumable_token` | text | YES | `—` |
| `final_summary` | text | YES | `—` |
| `artifacts` | ARRAY | YES | `'{}'::text[]` |
| `iterations` | integer | YES | `0` |
| `tokens_prompt` | integer | YES | `0` |
| `tokens_completion` | integer | YES | `0` |
| `cost_usd` | numeric | YES | `0` |
| `depth` | integer | NO | `1` |
| `source` | text | NO | `'db'::text` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `completed_at` | timestamp with time zone | YES | `—` |

## `orchestrator_audit_events`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `uuid_generate_v4()` |
| `run_id` | uuid | NO | `—` |
| `event_type` | text | NO | `—` |
| `payload` | jsonb | NO | `'{}'::jsonb` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `orchestrator_runs`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `uuid_generate_v4()` |
| `project_id` | uuid | NO | `—` |
| `session_id` | uuid | YES | `—` |
| `profile_id` | uuid | YES | `—` |
| `status` | text | NO | `'started'::text` |
| `audit` | jsonb | NO | `'{}'::jsonb` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `user_id` | uuid | YES | `—` |
| `audit_json` | jsonb | YES | `—` |

## `plugin_audit_events`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `plugin_instance_id` | uuid | YES | `—` |
| `user_id` | uuid | YES | `—` |
| `project_id` | uuid | YES | `—` |
| `action` | text | NO | `—` |
| `status` | text | NO | `'ok'::text` |
| `message` | text | YES | `—` |
| `payload` | jsonb | NO | `'{}'::jsonb` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `plugin_catalog_items`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `slug` | text | NO | `—` |
| `name` | text | NO | `—` |
| `description` | text | NO | `''::text` |
| `plugin_type` | text | NO | `'mcp'::text` |
| `transport` | text | NO | `—` |
| `http_url` | text | YES | `—` |
| `stdio_command` | text | YES | `—` |
| `stdio_args` | jsonb | NO | `'[]'::jsonb` |
| `required_secret_refs` | jsonb | NO | `'[]'::jsonb` |
| `optional_secret_refs` | jsonb | NO | `'[]'::jsonb` |
| `default_scope` | text | NO | `'global'::text` |
| `allowed_commands` | jsonb | NO | `'[]'::jsonb` |
| `default_tool_policy` | jsonb | NO | `'{"mode": "allowlist", "tools": [], "blockedTools": []}'::jsonb` |
| `metadata` | jsonb | NO | `'{}'::jsonb` |
| `is_allowlisted` | boolean | NO | `true` |
| `enabled` | boolean | NO | `true` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `plugin_instance_health_runs`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `plugin_instance_id` | uuid | NO | `—` |
| `tested_by_user_id` | uuid | YES | `—` |
| `success` | boolean | NO | `—` |
| `tool_count` | integer | NO | `0` |
| `error_message` | text | YES | `—` |
| `details` | jsonb | NO | `'{}'::jsonb` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `plugin_instance_tool_policies`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `plugin_instance_id` | uuid | NO | `—` |
| `mode` | text | NO | `'allowlist'::text` |
| `tools` | jsonb | NO | `'[]'::jsonb` |
| `blocked_tools` | jsonb | NO | `'[]'::jsonb` |
| `updated_by_user_id` | uuid | YES | `—` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `plugin_instances`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `catalog_item_id` | uuid | NO | `—` |
| `release_id` | uuid | YES | `—` |
| `installed_by_user_id` | uuid | YES | `—` |
| `project_id` | uuid | YES | `—` |
| `scope` | text | NO | `'global'::text` |
| `name` | text | NO | `—` |
| `enabled` | boolean | NO | `true` |
| `config` | jsonb | NO | `'{}'::jsonb` |
| `secret_bindings` | jsonb | NO | `'{}'::jsonb` |
| `health_status` | text | NO | `'unknown'::text` |
| `last_health_message` | text | YES | `—` |
| `last_tested_at` | timestamp with time zone | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `plugin_releases`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `catalog_item_id` | uuid | NO | `—` |
| `version` | text | NO | `—` |
| `changelog` | text | NO | `''::text` |
| `config_patch` | jsonb | NO | `'{}'::jsonb` |
| `is_stable` | boolean | NO | `true` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `profile_mcp_servers`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `profile_id` | uuid | NO | `—` |
| `mcp_server_id` | uuid | NO | `—` |

## `project_code_edges`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `project_id` | uuid | NO | `—` |
| `from_path` | text | NO | `—` |
| `to_path` | text | NO | `—` |
| `edge_kind` | text | NO | `—` |
| `weight` | real | NO | `1.0` |
| `source` | text | NO | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `project_code_nodes`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `project_id` | uuid | NO | `—` |
| `file_path` | text | NO | `—` |
| `lang` | text | YES | `—` |
| `content_hash` | text | YES | `—` |
| `last_seen_at` | timestamp with time zone | NO | `now()` |

## `project_code_tests`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `project_id` | uuid | NO | `—` |
| `test_path` | text | NO | `—` |
| `covers_path` | text | NO | `—` |
| `method` | text | NO | `—` |
| `confidence` | real | NO | `0.6` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `project_database_config`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `uuid_generate_v4()` |
| `project_id` | uuid | NO | `—` |
| `engine` | text | NO | `—` |
| `hosting_mode` | text | NO | `—` |
| `connection_secret` | bytea | YES | `—` |
| `migration_tool` | text | YES | `—` |
| `migration_path` | text | YES | `—` |
| `allow_ddl_override` | boolean | NO | `false` |
| `detection_metadata` | jsonb | NO | `'{}'::jsonb` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |
| `name` | text | NO | `'primary'::text` |
| `is_primary` | boolean | NO | `true` |

## `project_document_versions`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `document_id` | uuid | NO | `—` |
| `version` | text | NO | `—` |
| `file_path` | text | NO | `—` |
| `change_summary` | text | YES | `—` |
| `changed_sections` | ARRAY | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `project_documents`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `project_id` | uuid | NO | `—` |
| `doc_type` | text | NO | `—` |
| `title` | text | NO | `—` |
| `version` | text | NO | `'1.0.0'::text` |
| `file_path` | text | NO | `—` |
| `structure_json` | jsonb | NO | `'{}'::jsonb` |
| `status` | text | NO | `'draft'::text` |
| `metadata` | jsonb | NO | `'{}'::jsonb` |
| `qdrant_point_ids` | ARRAY | YES | `'{}'::text[]` |
| `created_by` | uuid | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `project_impact_runs`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `run_id` | uuid | YES | `—` |
| `change_request_note_id` | uuid | YES | `—` |
| `project_id` | uuid | YES | `—` |
| `seed_paths` | ARRAY | YES | `—` |
| `impact_paths` | jsonb | YES | `—` |
| `gate_status` | text | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `project_knowledge_links`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `from_note_id` | uuid | NO | `—` |
| `to_note_id` | uuid | NO | `—` |
| `rel_type` | text | NO | `—` |
| `created_by` | text | NO | `—` |
| `confidence` | real | NO | `1.0` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `project_knowledge_notes`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `project_id` | uuid | NO | `—` |
| `source_run_id` | uuid | YES | `—` |
| `source_message_id` | uuid | YES | `—` |
| `intent` | text | YES | `—` |
| `title` | text | NO | `—` |
| `body_md` | text | NO | `—` |
| `status` | text | NO | `'draft'::text` |
| `qdrant_point_id` | text | YES | `—` |
| `tags` | ARRAY | NO | `'{}'::text[]` |
| `file_paths` | ARRAY | NO | `'{}'::text[]` |
| `vault_file_path` | text | YES | `—` |
| `vault_file_hash` | text | YES | `—` |
| `access_count` | integer | NO | `0` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |
| `last_accessed_at` | timestamp with time zone | YES | `—` |
| `kind` | text | NO | `'chat'::text` |
| `off_topic` | boolean | NO | `false` |
| `relevance_score` | real | YES | `—` |
| `source_kind` | text | NO | `'native'::text` |
| `external_source_id` | text | YES | `—` |
| `context_stale_at` | timestamp with time zone | YES | `—` |
| `deprecated_at` | timestamp with time zone | YES | `—` |
| `superseded_by` | uuid | YES | `—` |
| `archived_at` | timestamp with time zone | YES | `—` |

## `project_knowledge_tags`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `project_id` | uuid | NO | `—` |
| `tag` | text | NO | `—` |
| `note_count` | integer | NO | `0` |
| `last_used_at` | timestamp with time zone | NO | `now()` |

## `project_learning_config`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `project_id` | uuid | NO | `—` |
| `enabled` | boolean | NO | `true` |
| `prompt_corrections_enabled` | boolean | NO | `true` |
| `auto_apply_max_changes_per_day` | integer | NO | `2` |
| `feedback_threshold` | integer | NO | `5` |
| `feedback_window_days` | integer | NO | `7` |
| `min_confidence` | numeric | NO | `0.6500` |
| `rollback_window_hours` | integer | NO | `24` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `project_members`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `uuid_generate_v4()` |
| `project_id` | uuid | NO | `—` |
| `user_id` | uuid | NO | `—` |
| `role` | text | NO | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `project_migration_history`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `uuid_generate_v4()` |
| `project_id` | uuid | NO | `—` |
| `filename` | text | NO | `—` |
| `checksum` | text | NO | `—` |
| `status` | text | NO | `'pending'::text` |
| `description` | text | YES | `—` |
| `sql_diff` | text | YES | `—` |
| `rollback_sql` | text | YES | `—` |
| `created_by_agent` | text | YES | `—` |
| `created_by_user` | uuid | YES | `—` |
| `applied_by_user` | uuid | YES | `—` |
| `applied_by_agent` | text | YES | `—` |
| `override_reason` | text | YES | `—` |
| `error_message` | text | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `applied_at` | timestamp with time zone | YES | `—` |
| `rolled_back_at` | timestamp with time zone | YES | `—` |

## `project_open_sessions`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `user_id` | uuid | NO | `—` |
| `project_id` | uuid | NO | `—` |
| `workspace_id` | uuid | YES | `—` |
| `active_file_paths` | jsonb | NO | `'[]'::jsonb` |
| `terminal_cwd` | text | YES | `—` |
| `last_opened_at` | timestamp with time zone | NO | `now()` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `project_quality_findings`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `project_id` | uuid | NO | `—` |
| `scanned_at` | timestamp with time zone | NO | `now()` |
| `file_path` | text | NO | `—` |
| `category` | text | NO | `—` |
| `severity` | text | NO | `—` |
| `title` | text | NO | `—` |
| `detail` | text | NO | `—` |
| `line_number` | integer | YES | `—` |
| `fixed_at` | timestamp with time zone | YES | `—` |
| `fixed_by_run_id` | uuid | YES | `—` |
| `is_false_positive` | boolean | NO | `false` |
| `false_positive_reason` | text | YES | `—` |
| `false_positive_at` | timestamp with time zone | YES | `—` |
| `false_positive_rule_key` | text | YES | `—` |
| `confidence` | text | YES | `—` |
| `context_snippet` | text | YES | `—` |
| `related_files` | ARRAY | YES | `—` |
| `is_auto_suppressed` | boolean | NO | `false` |

## `project_runtime_issues`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `project_id` | uuid | NO | `—` |
| `source` | text | NO | `—` |
| `severity` | text | NO | `'error'::text` |
| `message` | text | NO | `—` |
| `details` | text | YES | `—` |
| `run_id` | uuid | YES | `—` |
| `step_id` | uuid | YES | `—` |
| `tool_name` | text | YES | `—` |
| `command` | text | YES | `—` |
| `exit_code` | integer | YES | `—` |
| `status` | text | NO | `'open'::text` |
| `fingerprint` | text | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |
| `resolved_at` | timestamp with time zone | YES | `—` |

## `projects`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `uuid_generate_v4()` |
| `team_id` | uuid | NO | `—` |
| `name` | text | NO | `—` |
| `slug` | text | NO | `—` |
| `default_branch` | text | NO | `'main'::text` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `owner_user_id` | uuid | NO | `—` |
| `visibility` | text | NO | `'private'::text` |
| `last_opened_by_user_id` | uuid | YES | `—` |
| `analysis_json` | jsonb | YES | `—` |
| `analyzed_at` | timestamp with time zone | YES | `—` |
| `custom_instructions` | text | YES | `—` |
| `detected_suggestions` | jsonb | YES | `—` |
| `detected_suggestions_at` | timestamp with time zone | YES | `—` |
| `repository_root_path` | text | YES | `—` |
| `default_profile_id` | uuid | YES | `—` |
| `sandbox_config` | jsonb | YES | `—` |
| `obsidian_vault_name` | text | NO | `''::text` |

## `prompt_ab_experiments`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `prompt_key` | text | NO | `—` |
| `baseline_version` | integer | NO | `—` |
| `variant_version` | integer | NO | `—` |
| `traffic_pct` | integer | NO | `10` |
| `status` | text | NO | `'running'::text` |
| `started_at` | timestamp with time zone | NO | `now()` |
| `ended_at` | timestamp with time zone | YES | `—` |
| `baseline_success_rate` | numeric | YES | `—` |
| `variant_success_rate` | numeric | YES | `—` |
| `baseline_reflection_avg` | numeric | YES | `—` |
| `variant_reflection_avg` | numeric | YES | `—` |
| `p_value` | numeric | YES | `—` |
| `decision_reason` | text | YES | `—` |
| `min_runs_required` | integer | NO | `30` |
| `auto_promote_enabled` | boolean | NO | `false` |

## `prompt_corrections`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `project_id` | uuid | NO | `—` |
| `feedback_id` | uuid | YES | `—` |
| `session_id` | uuid | YES | `—` |
| `message_id` | uuid | YES | `—` |
| `orchestrator_run_id` | uuid | YES | `—` |
| `intent` | text | YES | `—` |
| `provider` | text | YES | `—` |
| `model` | text | YES | `—` |
| `correction_text` | text | NO | `—` |
| `normalized_hint_hash` | text | NO | `—` |
| `qdrant_point_id` | text | NO | `—` |
| `active` | boolean | NO | `true` |
| `status` | text | NO | `'open'::text` |
| `retrieved_count` | bigint | NO | `0` |
| `last_retrieved_at` | timestamp with time zone | YES | `—` |
| `resolved_at` | timestamp with time zone | YES | `—` |
| `deleted_at` | timestamp with time zone | YES | `—` |
| `metadata` | jsonb | NO | `'{}'::jsonb` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |
| `type` | text | NO | `'correction'::text` |

## `provider_budget_status`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `provider` | text | NO | `—` |
| `monthly_budget_usd` | numeric | NO | `0` |
| `spent_current_period_usd` | numeric | NO | `0` |
| `period_start` | timestamp with time zone | NO | `now()` |
| `min_threshold_usd` | numeric | NO | `1.0` |
| `notes` | text | YES | `—` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `quality_findings`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `uuid_generate_v4()` |
| `project_id` | uuid | NO | `—` |
| `file_path` | text | NO | `—` |
| `category` | text | NO | `—` |
| `severity` | text | NO | `—` |
| `finding` | jsonb | NO | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `reasoning_examples`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `pattern_id` | uuid | NO | `—` |
| `input_summary` | text | NO | `—` |
| `output_summary` | text | NO | `—` |
| `context` | jsonb | NO | `'{}'::jsonb` |
| `quality_score` | real | NO | `0.5` |
| `validated` | boolean | NO | `false` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `reasoning_patterns`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `pattern_type` | USER-DEFINED | NO | `—` |
| `name` | text | NO | `—` |
| `description` | text | NO | `''::text` |
| `embedding` | ARRAY | YES | `—` |
| `confidence` | real | NO | `0.5` |
| `use_count` | bigint | NO | `0` |
| `success_count` | bigint | NO | `0` |
| `applicable_languages` | ARRAY | NO | `'{}'::text[]` |
| `applicable_frameworks` | ARRAY | NO | `'{}'::text[]` |
| `applicable_tasks` | ARRAY | NO | `'{}'::text[]` |
| `source_agent` | text | YES | `—` |
| `tags` | ARRAY | NO | `'{}'::text[]` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `repositories`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `uuid_generate_v4()` |
| `project_id` | uuid | NO | `—` |
| `provider` | text | NO | `'local'::text` |
| `remote_url` | text | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `root_path` | text | YES | `—` |
| `is_git_repo` | boolean | NO | `false` |
| `current_branch` | text | YES | `—` |

## `run_configurations`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `project_id` | uuid | NO | `—` |
| `label` | text | NO | `—` |
| `kind` | text | NO | `'shell'::text` |
| `command` | text | NO | `—` |
| `args` | ARRAY | NO | `'{}'::text[]` |
| `cwd` | text | YES | `—` |
| `env` | jsonb | NO | `'{}'::jsonb` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |
| `role` | text | YES | `—` |
| `essential` | boolean | NO | `false` |
| `group_label` | text | YES | `—` |

## `ruvector_collections`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `name` | text | NO | `—` |
| `description` | text | YES | `—` |
| `dim` | integer | NO | `384` |
| `max_vectors` | integer | YES | `—` |
| `ttl_seconds` | integer | YES | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `ruvector_hnsw_stats`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | bigint | NO | `nextval('ruvector_hnsw_stats_id_seq'::regclass)` |
| `collection_id` | uuid | NO | `—` |
| `num_vectors` | integer | NO | `0` |
| `num_layers` | integer | NO | `0` |
| `avg_connections` | real | NO | `0.0` |
| `last_insert_us` | bigint | YES | `—` |
| `last_search_us` | bigint | YES | `—` |
| `last_optimize_us` | bigint | YES | `—` |
| `sona_runs` | integer | NO | `0` |
| `sona_pruned` | integer | NO | `0` |
| `recorded_at` | timestamp with time zone | NO | `now()` |

## `ruvector_vectors`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `collection_id` | uuid | NO | `—` |
| `external_id` | text | NO | `—` |
| `embedding` | ARRAY | NO | `—` |
| `metadata` | jsonb | NO | `'{}'::jsonb` |
| `deleted` | boolean | NO | `false` |
| `confidence` | real | NO | `1.0` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `security_findings`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `uuid_generate_v4()` |
| `project_id` | uuid | NO | `—` |
| `file_path` | text | NO | `—` |
| `severity` | text | NO | `—` |
| `finding` | jsonb | NO | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `sessions`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `user_id` | uuid | NO | `—` |
| `token_hash` | text | NO | `—` |
| `expires_at` | timestamp with time zone | NO | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `settings`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `key` | text | NO | `—` |
| `value` | text | NO | `''::text` |
| `category` | text | NO | `'general'::text` |
| `description` | text | NO | `''::text` |
| `is_secret` | boolean | NO | `false` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `teams`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `uuid_generate_v4()` |
| `name` | text | NO | `—` |
| `slug` | text | NO | `—` |
| `created_at` | timestamp with time zone | NO | `now()` |

## `terminal_commands`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `project_id` | uuid | NO | `—` |
| `session_id` | uuid | YES | `—` |
| `command` | text | NO | `—` |
| `status` | text | NO | `'pending'::text` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `delivered_at` | timestamp with time zone | YES | `—` |
| `claimed_at` | timestamp with time zone | YES | `—` |
| `claimed_by` | text | YES | `—` |
| `failed_at` | timestamp with time zone | YES | `—` |
| `fail_reason` | text | YES | `—` |
| `output_preview` | text | YES | `—` |
| `exit_code` | integer | YES | `—` |
| `finished_at` | timestamp with time zone | YES | `—` |
| `full_output` | text | YES | `—` |

## `user_profiles`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `user_id` | uuid | YES | `—` |
| `name` | text | NO | `—` |
| `description` | text | YES | `—` |
| `avatar_emoji` | text | NO | `'🤖'::text` |
| `system_prompt` | text | NO | `''::text` |
| `default_provider` | text | YES | `—` |
| `default_model` | text | YES | `—` |
| `default_automation` | text | YES | `—` |
| `is_default` | boolean | NO | `false` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |
| `is_system` | boolean | NO | `false` |
| `source_template_key` | text | YES | `—` |

## `user_project_preferences`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `user_id` | uuid | NO | `—` |
| `project_id` | uuid | NO | `—` |
| `preferences` | jsonb | NO | `'{}'::jsonb` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `updated_at` | timestamp with time zone | NO | `now()` |

## `users`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `uuid_generate_v4()` |
| `email` | text | NO | `—` |
| `display_name` | text | NO | `—` |
| `password_hash` | text | YES | `—` |
| `role` | text | NO | `'viewer'::text` |
| `created_at` | timestamp with time zone | NO | `now()` |
| `github_id` | bigint | YES | `—` |
| `github_username` | text | YES | `—` |
| `avatar_url` | text | YES | `—` |
| `deleted_at` | timestamp with time zone | YES | `—` |

## `vector_compaction_runs`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `gen_random_uuid()` |
| `project_id` | uuid | YES | `—` |
| `trigger_type` | text | NO | `—` |
| `status` | text | NO | `'started'::text` |
| `before_count` | bigint | NO | `0` |
| `after_count` | bigint | NO | `0` |
| `dedup_count` | bigint | NO | `0` |
| `deleted_count` | bigint | NO | `0` |
| `qdrant_deleted_count` | bigint | NO | `0` |
| `details` | jsonb | NO | `'{}'::jsonb` |
| `requested_by` | uuid | YES | `—` |
| `started_at` | timestamp with time zone | NO | `now()` |
| `finished_at` | timestamp with time zone | YES | `—` |

## `workspaces`

| Colonna | Tipo | Nullable | Default |
|---|---|---|---|
| `id` | uuid | NO | `uuid_generate_v4()` |
| `project_id` | uuid | NO | `—` |
| `absolute_path` | text | NO | `—` |
| `is_primary` | boolean | NO | `false` |
| `created_at` | timestamp with time zone | NO | `now()` |

---

Fonte: query `SELECT FROM information_schema.tables JOIN columns`.
