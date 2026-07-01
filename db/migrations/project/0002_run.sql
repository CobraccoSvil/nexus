-- 0002_run.sql (db/migrations/project/)
-- Schema per-progetto DOMINIO RUN/AGENT (15 tabelle: agent_runs/steps/processes,
-- orchestrator, subagent, plans/todos/clarifications, verifier, traces,
-- meta_steps, jobs, graph/langgraph checkpoints), applicato al DB metadati
-- <slug>_nexus dopo 0001_chat. Generato da pg_dump --schema-only e ripulito:
-- rimosse le FK verso tabelle GLOBALI (projects, users) -> id logici uuid. Le FK
-- intra-run e cross-dominio verso CHAT (es. agent_runs.run_message_id ->
-- chat_messages) sono mantenute: chat e' creato da 0001, applicato prima.
--
-- Estensioni richieste dallo schema (uuid_generate_v4, gen_random_uuid, indici
-- trigram). Sono tutte "trusted": le crea anche nexus_app (proprietario del DB).
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE EXTENSION IF NOT EXISTS pgcrypto;

-- Funzioni trigger (self-contained) usate dalle tabelle del dominio.
CREATE OR REPLACE FUNCTION public.jobs_set_updated_at()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
  NEW.updated_at = NOW();
  RETURN NEW;
END;
$function$
;

--
-- PostgreSQL database dump
--


-- Dumped from database version 17.10
-- Dumped by pg_dump version 17.10

SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET transaction_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: agent_processes; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.agent_processes (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    project_id uuid NOT NULL,
    session_id uuid,
    label text DEFAULT ''::text NOT NULL,
    command text NOT NULL,
    working_dir text,
    pid integer,
    status text DEFAULT 'starting'::text NOT NULL,
    exit_code integer,
    output text DEFAULT ''::text NOT NULL,
    error_output text DEFAULT ''::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    started_at timestamp with time zone,
    stopped_at timestamp with time zone,
    sandboxed boolean DEFAULT false NOT NULL,
    kind text DEFAULT 'service'::text NOT NULL,
    resume_dispatched_at timestamp with time zone
);


--
-- Name: agent_runs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.agent_runs (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    session_id uuid NOT NULL,
    project_id uuid NOT NULL,
    user_id uuid NOT NULL,
    run_message_id uuid,
    status text DEFAULT 'running'::text NOT NULL,
    automation_mode text DEFAULT 'confirm'::text NOT NULL,
    provider text,
    model text,
    iteration_count integer DEFAULT 0 NOT NULL,
    final_answer text,
    pending_actions_json jsonb,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    parent_run_id uuid,
    messages_json text,
    supervisor_mode text DEFAULT 'none'::text NOT NULL,
    nexus_override_applied boolean DEFAULT false NOT NULL,
    nexus_agent_type text,
    nexus_q_value real,
    nexus_task_type text,
    prompt_tokens integer DEFAULT 0 NOT NULL,
    completion_tokens integer DEFAULT 0 NOT NULL,
    total_tokens integer DEFAULT 0 NOT NULL,
    total_cost double precision DEFAULT 0.0 NOT NULL,
    upscale_from text,
    upscale_to text,
    upscale_reason text,
    est_tokens_at_call integer,
    kb_ingested boolean,
    cancellation_requested timestamp with time zone,
    cancellation_reason text,
    generation_ended_at timestamp with time zone,
    updated_at timestamp with time zone,
    engine text
);


--
-- Name: agent_steps; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.agent_steps (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    run_id uuid NOT NULL,
    step_index integer NOT NULL,
    tool_name text NOT NULL,
    tool_input jsonb NOT NULL,
    tool_result text,
    status text DEFAULT 'running'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: jobs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.jobs (
    id uuid DEFAULT public.uuid_generate_v4() NOT NULL,
    project_id uuid NOT NULL,
    kind text NOT NULL,
    status text DEFAULT 'queued'::text NOT NULL,
    input jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    output_log text,
    progress jsonb DEFAULT '{}'::jsonb NOT NULL
);


--
-- Name: langgraph_checkpoints; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.langgraph_checkpoints (
    thread_id text NOT NULL,
    checkpoint_id text NOT NULL,
    checkpoint_data jsonb NOT NULL,
    metadata jsonb DEFAULT '{}'::jsonb NOT NULL,
    versions jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp without time zone DEFAULT CURRENT_TIMESTAMP
);


--
-- Name: nexus_agent_clarifications; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.nexus_agent_clarifications (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    run_id uuid NOT NULL,
    project_id uuid NOT NULL,
    questions jsonb NOT NULL,
    user_answers jsonb,
    applied_defaults jsonb,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    answered_at timestamp with time zone
);


--
-- Name: nexus_agent_meta_steps; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.nexus_agent_meta_steps (
    id bigint NOT NULL,
    run_id uuid NOT NULL,
    kind text NOT NULL,
    title text DEFAULT ''::text NOT NULL,
    payload jsonb DEFAULT '{}'::jsonb NOT NULL,
    correlation_id text,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: nexus_agent_meta_steps_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.nexus_agent_meta_steps_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: nexus_agent_meta_steps_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.nexus_agent_meta_steps_id_seq OWNED BY public.nexus_agent_meta_steps.id;


--
-- Name: nexus_agent_plans; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.nexus_agent_plans (
    run_id uuid NOT NULL,
    project_id uuid NOT NULL,
    thread_id text NOT NULL,
    acceptance_criteria jsonb DEFAULT '[]'::jsonb NOT NULL,
    planner_model text NOT NULL,
    approved_at timestamp with time zone,
    approved_by uuid,
    score double precision,
    plan_revisions integer DEFAULT 0 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    rationale text,
    constraints jsonb DEFAULT '[]'::jsonb NOT NULL,
    alternatives jsonb DEFAULT '[]'::jsonb NOT NULL,
    user_intent text,
    behavior_mode text
);


--
-- Name: nexus_agent_todos; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.nexus_agent_todos (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    run_id uuid NOT NULL,
    project_id uuid NOT NULL,
    seq integer NOT NULL,
    content text NOT NULL,
    status text NOT NULL,
    priority text DEFAULT 'normal'::text NOT NULL,
    acceptance_criteria jsonb DEFAULT '[]'::jsonb NOT NULL,
    verify_failures integer DEFAULT 0 NOT NULL,
    iteration_seen integer DEFAULT 0 NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    depends_on uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    dep_keys text[],
    node_key text,
    dag_layer integer,
    edited_by text,
    carry_over boolean DEFAULT false NOT NULL,
    origin_run_id uuid,
    CONSTRAINT nexus_agent_todos_priority_check CHECK ((priority = ANY (ARRAY['high'::text, 'normal'::text, 'low'::text]))),
    CONSTRAINT nexus_agent_todos_status_check CHECK ((status = ANY (ARRAY['pending'::text, 'in_progress'::text, 'completed'::text, 'blocked'::text, 'skipped'::text])))
);


--
-- Name: nexus_agent_traces; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.nexus_agent_traces (
    id bigint NOT NULL,
    session_id uuid NOT NULL,
    run_id uuid NOT NULL,
    seq integer DEFAULT 0 NOT NULL,
    payload jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: nexus_agent_traces_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.nexus_agent_traces_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: nexus_agent_traces_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.nexus_agent_traces_id_seq OWNED BY public.nexus_agent_traces.id;


--
-- Name: nexus_agent_verifier_runs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.nexus_agent_verifier_runs (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    run_id uuid NOT NULL,
    todo_id uuid,
    cycle integer NOT NULL,
    criteria_results jsonb NOT NULL,
    passed boolean NOT NULL,
    duration_ms integer,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: nexus_graph_checkpoints; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.nexus_graph_checkpoints (
    run_id uuid NOT NULL,
    superstep bigint NOT NULL,
    next_node text NOT NULL,
    state jsonb NOT NULL,
    metadata jsonb,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: nexus_subagent_runs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.nexus_subagent_runs (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    parent_run_id uuid NOT NULL,
    project_id uuid NOT NULL,
    kind text NOT NULL,
    task_description text NOT NULL,
    context_blob text,
    expected_format text,
    status text NOT NULL,
    is_background boolean DEFAULT false NOT NULL,
    resumable_token text,
    final_summary text,
    artifacts text[] DEFAULT '{}'::text[],
    iterations integer DEFAULT 0,
    tokens_prompt integer DEFAULT 0,
    tokens_completion integer DEFAULT 0,
    cost_usd numeric(12,6) DEFAULT 0,
    depth integer DEFAULT 1 NOT NULL,
    source text DEFAULT 'db'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    CONSTRAINT nexus_subagent_runs_source_check CHECK ((source = ANY (ARRAY['db'::text, 'project_override'::text]))),
    CONSTRAINT nexus_subagent_runs_status_check CHECK ((status = ANY (ARRAY['pending'::text, 'running'::text, 'completed'::text, 'failed'::text, 'timeout'::text, 'cancelled'::text, 'paused'::text])))
);


--
-- Name: orchestrator_audit_events; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.orchestrator_audit_events (
    id uuid DEFAULT public.uuid_generate_v4() NOT NULL,
    run_id uuid NOT NULL,
    event_type text NOT NULL,
    payload jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: orchestrator_runs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.orchestrator_runs (
    id uuid DEFAULT public.uuid_generate_v4() NOT NULL,
    project_id uuid NOT NULL,
    session_id uuid,
    profile_id uuid,
    status text DEFAULT 'started'::text NOT NULL,
    audit jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    user_id uuid,
    audit_json jsonb
);


--
-- Name: nexus_agent_meta_steps id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_agent_meta_steps ALTER COLUMN id SET DEFAULT nextval('public.nexus_agent_meta_steps_id_seq'::regclass);


--
-- Name: nexus_agent_traces id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_agent_traces ALTER COLUMN id SET DEFAULT nextval('public.nexus_agent_traces_id_seq'::regclass);


--
-- Name: agent_processes agent_processes_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.agent_processes
    ADD CONSTRAINT agent_processes_pkey PRIMARY KEY (id);


--
-- Name: agent_runs agent_runs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.agent_runs
    ADD CONSTRAINT agent_runs_pkey PRIMARY KEY (id);


--
-- Name: agent_steps agent_steps_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.agent_steps
    ADD CONSTRAINT agent_steps_pkey PRIMARY KEY (id);


--
-- Name: jobs jobs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.jobs
    ADD CONSTRAINT jobs_pkey PRIMARY KEY (id);


--
-- Name: langgraph_checkpoints langgraph_checkpoints_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.langgraph_checkpoints
    ADD CONSTRAINT langgraph_checkpoints_pkey PRIMARY KEY (thread_id, checkpoint_id);


--
-- Name: nexus_agent_clarifications nexus_agent_clarifications_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_agent_clarifications
    ADD CONSTRAINT nexus_agent_clarifications_pkey PRIMARY KEY (id);


--
-- Name: nexus_agent_meta_steps nexus_agent_meta_steps_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_agent_meta_steps
    ADD CONSTRAINT nexus_agent_meta_steps_pkey PRIMARY KEY (id);


--
-- Name: nexus_agent_plans nexus_agent_plans_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_agent_plans
    ADD CONSTRAINT nexus_agent_plans_pkey PRIMARY KEY (run_id);


--
-- Name: nexus_agent_todos nexus_agent_todos_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_agent_todos
    ADD CONSTRAINT nexus_agent_todos_pkey PRIMARY KEY (id);


--
-- Name: nexus_agent_traces nexus_agent_traces_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_agent_traces
    ADD CONSTRAINT nexus_agent_traces_pkey PRIMARY KEY (id);


--
-- Name: nexus_agent_verifier_runs nexus_agent_verifier_runs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_agent_verifier_runs
    ADD CONSTRAINT nexus_agent_verifier_runs_pkey PRIMARY KEY (id);


--
-- Name: nexus_graph_checkpoints nexus_graph_checkpoints_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_graph_checkpoints
    ADD CONSTRAINT nexus_graph_checkpoints_pkey PRIMARY KEY (run_id, superstep);


--
-- Name: nexus_subagent_runs nexus_subagent_runs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_subagent_runs
    ADD CONSTRAINT nexus_subagent_runs_pkey PRIMARY KEY (id);


--
-- Name: orchestrator_audit_events orchestrator_audit_events_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.orchestrator_audit_events
    ADD CONSTRAINT orchestrator_audit_events_pkey PRIMARY KEY (id);


--
-- Name: orchestrator_runs orchestrator_runs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.orchestrator_runs
    ADD CONSTRAINT orchestrator_runs_pkey PRIMARY KEY (id);


--
-- Name: idx_agent_processes_created; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_agent_processes_created ON public.agent_processes USING btree (project_id, created_at DESC);


--
-- Name: idx_agent_processes_project; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_agent_processes_project ON public.agent_processes USING btree (project_id, status);


--
-- Name: idx_agent_processes_resume_pending; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_agent_processes_resume_pending ON public.agent_processes USING btree (stopped_at) WHERE ((resume_dispatched_at IS NULL) AND (session_id IS NOT NULL));


--
-- Name: idx_agent_processes_session_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_agent_processes_session_id ON public.agent_processes USING btree (session_id);


--
-- Name: idx_agent_runs_kb_ingest_pending; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_agent_runs_kb_ingest_pending ON public.agent_runs USING btree (completed_at DESC) WHERE ((kb_ingested IS NULL) AND (status = ANY (ARRAY['completed'::text, 'failed'::text, 'aborted'::text])));


--
-- Name: idx_agent_runs_nexus_override; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_agent_runs_nexus_override ON public.agent_runs USING btree (created_at DESC) WHERE (nexus_override_applied = true);


--
-- Name: idx_agent_runs_parent; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_agent_runs_parent ON public.agent_runs USING btree (parent_run_id);


--
-- Name: idx_agent_runs_project_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_agent_runs_project_id ON public.agent_runs USING btree (project_id);


--
-- Name: idx_agent_runs_run_message_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_agent_runs_run_message_id ON public.agent_runs USING btree (run_message_id);


--
-- Name: idx_agent_runs_running_heartbeat; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_agent_runs_running_heartbeat ON public.agent_runs USING btree (updated_at) WHERE (status = 'running'::text);


--
-- Name: idx_agent_runs_session; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_agent_runs_session ON public.agent_runs USING btree (session_id);


--
-- Name: idx_agent_runs_session_active; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_agent_runs_session_active ON public.agent_runs USING btree (session_id) WHERE (status = ANY (ARRAY['running'::text, 'awaiting_confirmation'::text]));


--
-- Name: idx_agent_runs_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_agent_runs_user ON public.agent_runs USING btree (user_id);


--
-- Name: idx_agent_steps_run_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_agent_steps_run_id ON public.agent_steps USING btree (run_id, step_index);


--
-- Name: idx_checkpoints_thread_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_checkpoints_thread_id ON public.langgraph_checkpoints USING btree (thread_id, created_at DESC);


--
-- Name: idx_clarifications_pending; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_clarifications_pending ON public.nexus_agent_clarifications USING btree (run_id) WHERE ((user_answers IS NULL) AND (applied_defaults IS NULL));


--
-- Name: idx_clarifications_project; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_clarifications_project ON public.nexus_agent_clarifications USING btree (project_id, created_at DESC);


--
-- Name: idx_clarifications_run; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_clarifications_run ON public.nexus_agent_clarifications USING btree (run_id);


--
-- Name: idx_jobs_project_status_updated; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_jobs_project_status_updated ON public.jobs USING btree (project_id, status, updated_at DESC);


--
-- Name: idx_nexus_agent_meta_steps_kind; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_nexus_agent_meta_steps_kind ON public.nexus_agent_meta_steps USING btree (kind);


--
-- Name: idx_nexus_agent_meta_steps_run; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_nexus_agent_meta_steps_run ON public.nexus_agent_meta_steps USING btree (run_id, created_at);


--
-- Name: idx_nexus_agent_traces_run; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_nexus_agent_traces_run ON public.nexus_agent_traces USING btree (run_id, seq);


--
-- Name: idx_nexus_agent_traces_session; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_nexus_agent_traces_session ON public.nexus_agent_traces USING btree (session_id);


--
-- Name: idx_nexus_graph_checkpoints_run_superstep; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_nexus_graph_checkpoints_run_superstep ON public.nexus_graph_checkpoints USING btree (run_id, superstep DESC);


--
-- Name: idx_nexus_subagent_runs_project_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_nexus_subagent_runs_project_id ON public.nexus_subagent_runs USING btree (project_id);


--
-- Name: idx_orchestrator_audit_events_run_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_orchestrator_audit_events_run_id ON public.orchestrator_audit_events USING btree (run_id);


--
-- Name: idx_orchestrator_runs_project; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_orchestrator_runs_project ON public.orchestrator_runs USING btree (project_id, created_at);


--
-- Name: idx_orchestrator_runs_session_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_orchestrator_runs_session_id ON public.orchestrator_runs USING btree (session_id);


--
-- Name: idx_orchestrator_runs_user_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_orchestrator_runs_user_id ON public.orchestrator_runs USING btree (user_id);


--
-- Name: idx_plans_project; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_plans_project ON public.nexus_agent_plans USING btree (project_id, created_at DESC);


--
-- Name: idx_plans_thread; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_plans_thread ON public.nexus_agent_plans USING btree (thread_id);


--
-- Name: idx_subagent_runs_bg; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_subagent_runs_bg ON public.nexus_subagent_runs USING btree (parent_run_id, is_background) WHERE (status = ANY (ARRAY['running'::text, 'paused'::text]));


--
-- Name: idx_subagent_runs_kind_status; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_subagent_runs_kind_status ON public.nexus_subagent_runs USING btree (kind, status);


--
-- Name: idx_subagent_runs_parent; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_subagent_runs_parent ON public.nexus_subagent_runs USING btree (parent_run_id);


--
-- Name: idx_subagent_runs_project; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_subagent_runs_project ON public.nexus_subagent_runs USING btree (project_id, created_at DESC);


--
-- Name: idx_todos_carryover; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_todos_carryover ON public.nexus_agent_todos USING btree (project_id, carry_over) WHERE (carry_over = true);


--
-- Name: idx_todos_depends_on; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_todos_depends_on ON public.nexus_agent_todos USING gin (depends_on);


--
-- Name: idx_todos_project; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_todos_project ON public.nexus_agent_todos USING btree (project_id);


--
-- Name: idx_todos_run_seq; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX IF NOT EXISTS idx_todos_run_seq ON public.nexus_agent_todos USING btree (run_id, seq);


--
-- Name: idx_todos_status; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_todos_status ON public.nexus_agent_todos USING btree (run_id, status);


--
-- Name: idx_verifier_runs_run; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_verifier_runs_run ON public.nexus_agent_verifier_runs USING btree (run_id, created_at DESC);


--
-- Name: idx_verifier_runs_todo; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_verifier_runs_todo ON public.nexus_agent_verifier_runs USING btree (todo_id);


--
-- Name: jobs trg_jobs_updated_at; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_jobs_updated_at BEFORE UPDATE ON public.jobs FOR EACH ROW EXECUTE FUNCTION public.jobs_set_updated_at();


--
-- Name: agent_processes agent_processes_project_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: agent_processes agent_processes_session_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.agent_processes
    ADD CONSTRAINT agent_processes_session_id_fkey FOREIGN KEY (session_id) REFERENCES public.chat_sessions(id) ON DELETE SET NULL;


--
-- Name: agent_runs agent_runs_parent_run_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.agent_runs
    ADD CONSTRAINT agent_runs_parent_run_id_fkey FOREIGN KEY (parent_run_id) REFERENCES public.agent_runs(id) ON DELETE SET NULL;


--
-- Name: agent_runs agent_runs_project_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: agent_runs agent_runs_run_message_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.agent_runs
    ADD CONSTRAINT agent_runs_run_message_id_fkey FOREIGN KEY (run_message_id) REFERENCES public.chat_messages(id) ON DELETE SET NULL;


--
-- Name: agent_runs agent_runs_session_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.agent_runs
    ADD CONSTRAINT agent_runs_session_id_fkey FOREIGN KEY (session_id) REFERENCES public.chat_sessions(id) ON DELETE CASCADE;


--
-- Name: agent_runs agent_runs_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: agent_steps agent_steps_run_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.agent_steps
    ADD CONSTRAINT agent_steps_run_id_fkey FOREIGN KEY (run_id) REFERENCES public.agent_runs(id) ON DELETE CASCADE;


--
-- Name: nexus_subagent_runs fk_nexus_subagent_runs_project; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: jobs jobs_project_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: nexus_agent_clarifications nexus_agent_clarifications_project_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: nexus_agent_clarifications nexus_agent_clarifications_run_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_agent_clarifications
    ADD CONSTRAINT nexus_agent_clarifications_run_id_fkey FOREIGN KEY (run_id) REFERENCES public.agent_runs(id) ON DELETE CASCADE;


--
-- Name: nexus_agent_plans nexus_agent_plans_project_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: nexus_agent_todos nexus_agent_todos_run_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_agent_todos
    ADD CONSTRAINT nexus_agent_todos_run_id_fkey FOREIGN KEY (run_id) REFERENCES public.nexus_agent_plans(run_id) ON DELETE CASCADE;


--
-- Name: nexus_agent_verifier_runs nexus_agent_verifier_runs_run_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_agent_verifier_runs
    ADD CONSTRAINT nexus_agent_verifier_runs_run_id_fkey FOREIGN KEY (run_id) REFERENCES public.nexus_agent_plans(run_id) ON DELETE CASCADE;


--
-- Name: nexus_agent_verifier_runs nexus_agent_verifier_runs_todo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_agent_verifier_runs
    ADD CONSTRAINT nexus_agent_verifier_runs_todo_id_fkey FOREIGN KEY (todo_id) REFERENCES public.nexus_agent_todos(id) ON DELETE CASCADE;


--
-- Name: orchestrator_audit_events orchestrator_audit_events_run_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.orchestrator_audit_events
    ADD CONSTRAINT orchestrator_audit_events_run_id_fkey FOREIGN KEY (run_id) REFERENCES public.orchestrator_runs(id) ON DELETE CASCADE;


--
-- Name: orchestrator_runs orchestrator_runs_project_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: orchestrator_runs orchestrator_runs_session_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.orchestrator_runs
    ADD CONSTRAINT orchestrator_runs_session_id_fkey FOREIGN KEY (session_id) REFERENCES public.chat_sessions(id) ON DELETE SET NULL;


--
-- Name: orchestrator_runs orchestrator_runs_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- PostgreSQL database dump complete
--


