-- 0240_recovery_m7_m16.sql
-- MIGRAZIONE CONSOLIDATA DI RECOVERY (post-incidente 2026-06-01).
-- Ricrea schema + settings di M7-M16 persi col branch locale mai pushato.
-- INTERAMENTE IDEMPOTENTE: no-op su DB che ha gia' gli oggetti (caso attuale),
-- ricrea tutto su DB vergine (riproducibilita', regola H del CLAUDE.md).
-- Fonte: pg_dump --schema-only del DB salvato (0259) + dump settings.

-- ============ TABELLE (M7 provider-intent, M13 code graph/impact) ============

\restrict wM06zbLwaNS11uksTjBQvT6NB6oLF6ldzavUMSuk1Awg2yVDgiUL2R1Al2eqspl

CREATE TABLE IF NOT EXISTS public.nexus_provider_intent_health (
    provider text NOT NULL,
    model text NOT NULL,
    intent_subkind text NOT NULL,
    success_count bigint DEFAULT 0 NOT NULL,
    failure_count bigint DEFAULT 0 NOT NULL,
    soft_failure_count bigint DEFAULT 0 NOT NULL,
    last_seen_at timestamp with time zone DEFAULT now() NOT NULL,
    last_success_at timestamp with time zone,
    last_failure_at timestamp with time zone,
    cooldown_until timestamp with time zone,
    cooldown_reason text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);

COMMENT ON TABLE public.nexus_provider_intent_health IS 'Q-value provider-intent (M7 del piano provider-unification). Letto da brain/router/service.py::decide_model per filtrare provider con failure_rate alto. Aggiornato da brain/providers/registry.py::_record_usage post-call.';

COMMENT ON COLUMN public.nexus_provider_intent_health.intent_subkind IS 'Chiave intent allineata a nexus_routing_matrix.intent (es. architecture, fix_complesso, figma_to_code). NON e'' un foreign key per permettere intent dinamici.';

COMMENT ON COLUMN public.nexus_provider_intent_health.cooldown_until IS 'Timestamp fino al quale il provider/model e'' escluso per questo intent. Se NULL, il provider e'' utilizzabile. La logica di filtraggio e'' in brain/router/service.py.';

CREATE TABLE IF NOT EXISTS public.project_code_edges (
    project_id uuid NOT NULL,
    from_path text NOT NULL,
    to_path text NOT NULL,
    edge_kind text NOT NULL,
    weight real DEFAULT 1.0 NOT NULL,
    source text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT project_code_edges_edge_kind_check CHECK ((edge_kind = ANY (ARRAY['import'::text, 'semantic'::text]))),
    CONSTRAINT project_code_edges_source_check CHECK ((source = ANY (ARRAY['structural'::text, 'qdrant'::text])))
);

CREATE TABLE IF NOT EXISTS public.project_code_nodes (
    project_id uuid NOT NULL,
    file_path text NOT NULL,
    lang text,
    content_hash text,
    last_seen_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE IF NOT EXISTS public.project_code_tests (
    project_id uuid NOT NULL,
    test_path text NOT NULL,
    covers_path text NOT NULL,
    method text NOT NULL,
    confidence real DEFAULT 0.6 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT project_code_tests_method_check CHECK ((method = ANY (ARRAY['naming'::text, 'import'::text, 'cochange'::text, 'manual'::text])))
);

CREATE TABLE IF NOT EXISTS public.project_impact_runs (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    run_id uuid,
    change_request_note_id uuid,
    project_id uuid,
    seed_paths text[],
    impact_paths jsonb,
    gate_status text,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_pce_from ON public.project_code_edges USING btree (project_id, from_path);

CREATE INDEX IF NOT EXISTS idx_pce_to ON public.project_code_edges USING btree (project_id, to_path);

CREATE INDEX IF NOT EXISTS idx_pcn_project ON public.project_code_nodes USING btree (project_id);

CREATE INDEX IF NOT EXISTS idx_pct_covers ON public.project_code_tests USING btree (project_id, covers_path);

CREATE INDEX IF NOT EXISTS idx_pir_project ON public.project_impact_runs USING btree (project_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_provider_intent_health_cooldown ON public.nexus_provider_intent_health USING btree (cooldown_until) WHERE (cooldown_until IS NOT NULL);

CREATE INDEX IF NOT EXISTS idx_provider_intent_health_visits ON public.nexus_provider_intent_health USING btree (((success_count + failure_count)) DESC);

CREATE UNIQUE INDEX IF NOT EXISTS uq_pir_run_id ON public.project_impact_runs USING btree (run_id);

\unrestrict wM06zbLwaNS11uksTjBQvT6NB6oLF6ldzavUMSuk1Awg2yVDgiUL2R1Al2eqspl


-- ============ COLONNE AGGIUNTE (M14 context-stale, M15 todos evolution) ============
ALTER TABLE project_knowledge_notes ADD COLUMN IF NOT EXISTS context_stale_at TIMESTAMPTZ NULL;
ALTER TABLE nexus_agent_todos ADD COLUMN IF NOT EXISTS edited_by TEXT NULL;
ALTER TABLE nexus_agent_todos ADD COLUMN IF NOT EXISTS carry_over BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE nexus_agent_todos ADD COLUMN IF NOT EXISTS origin_run_id UUID NULL;

-- ============ SETTINGS (M12 ingest/autolink, M13 impact/regression, M14 lifecycle,
--                        M15 todos, M16 discovery, context, cooldown) ============
INSERT INTO settings (key, value, category) VALUES ('agent.attachment.figma_make_ai_chat_max_load_bytes', '536870912', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.attachment.image_max_bytes', '2097152', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.attachment.inspector_header_bytes', '32768', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.attachment.read_cache_ttl_seconds', '300', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.attachment.read_chunk_max_bytes', '102400', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.context.compress_phase_boundaries', '5,10,20,50', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.context.compress_phase_keep_recent', '8,5,3,2', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.context.compress_phase_max_chars', '2000,1000,500,150', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.context.compress_start_iter', '5', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.context.dedup_tool_results_enabled', 'true', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.context.drop_unused_base64_age', '3', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.context.max_chars', '400000', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.context.predictive_cap_ratio', '0.5', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.context.rag_offload.enabled', 'true', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.context.rag_offload.max_chunks_per_item', '500', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.context.rag_offload.min_chars', '2000', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.context.rag_offload.snippet_max_chars', '4000', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.context.rag_offload.top_k', '12', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.todos.carry_over_enabled', 'true', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.todos.live_events', 'true', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.todos.user_editable', 'true', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.tools.core_whitelist', 'read_file,write_file,edit_file,list_files,search_in_files,run_command,nexus_mcp_tool_search,nexus_mcp_tool_call', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.tools.discovery_first_enabled', 'true', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.tools.discovery_first_whitelist', 'nexus_mcp_tool_search,nexus_mcp_tool_call', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.tools.discovery_max_injected', '20', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.tools.discovery_schema_max_bytes', '8192', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('agent.tools.tiering_enabled', 'true', 'agent') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('impact.depth_cap', '2', 'impact') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('impact.enabled', 'true', 'impact') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('impact.max_nodes', '60', 'impact') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('impact.test_informed_enabled', 'true', 'impact') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('impact.test_informed_max_listed_tests', '15', 'impact') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('impact.test_informed_max_seed_paths', '12', 'impact') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('kb.autolink.enabled', 'true', 'kb') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('kb.autolink.semantic_threshold', '0.65', 'kb') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('kb.autolink.semantic_top_k', '3', 'kb') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('kb.autolink.wikilink_max_per_note', '10', 'kb') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('kb.changelog_cross_enabled', 'true', 'kb') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('kb.ingest.body_max_chars', '20000', 'kb') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('kb.ingest.cjk_max_ratio_pct', '20', 'knowledge') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('kb.ingest.enabled', 'true', 'kb') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('kb.ingest.min_chars', '300', 'kb') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('kb.ingest.title_max_chars', '120', 'kb') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('kb.intake.confirm_if_implemented', 'true', 'kb') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('kb.lifecycle.auto_deprecate_on_correction', 'true', 'kb') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('kb.lifecycle.context_stale_enabled', 'true', 'kb') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('providers.cooldown_bridge_timeout_seconds', '5', 'providers') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('providers.cooldown_circuit_breaker_threshold', '3', 'providers') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('regression_gate.enabled', 'true', 'regression_gate') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('regression_gate.hard_block', 'false', 'regression_gate') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('regression_gate.max_cycles', '1', 'regression_gate') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('regression_gate.max_tests', '10', 'regression_gate') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('regression_gate.soft_only', 'true', 'regression_gate') ON CONFLICT (key) DO NOTHING;
INSERT INTO settings (key, value, category) VALUES ('regression_gate.test_timeout_s', '120', 'regression_gate') ON CONFLICT (key) DO NOTHING;

