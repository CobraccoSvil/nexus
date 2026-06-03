--
-- PostgreSQL database dump
--

\restrict wM06zbLwaNS11uksTjBQvT6NB6oLF6ldzavUMSuk1Awg2yVDgiUL2R1Al2eqspl

-- Dumped from database version 17.9 (Debian 17.9-1.pgdg12+1)
-- Dumped by pg_dump version 17.9 (Debian 17.9-1.pgdg12+1)

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
-- Name: nexus_provider_intent_health; Type: TABLE; Schema: public; Owner: nexus
--

CREATE TABLE public.nexus_provider_intent_health (
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


ALTER TABLE public.nexus_provider_intent_health OWNER TO nexus;

--
-- Name: TABLE nexus_provider_intent_health; Type: COMMENT; Schema: public; Owner: nexus
--

COMMENT ON TABLE public.nexus_provider_intent_health IS 'Q-value provider-intent (M7 del piano provider-unification). Letto da brain/router/service.py::decide_model per filtrare provider con failure_rate alto. Aggiornato da brain/providers/registry.py::_record_usage post-call.';


--
-- Name: COLUMN nexus_provider_intent_health.intent_subkind; Type: COMMENT; Schema: public; Owner: nexus
--

COMMENT ON COLUMN public.nexus_provider_intent_health.intent_subkind IS 'Chiave intent allineata a nexus_routing_matrix.intent (es. architecture, fix_complesso, figma_to_code). NON e'' un foreign key per permettere intent dinamici.';


--
-- Name: COLUMN nexus_provider_intent_health.cooldown_until; Type: COMMENT; Schema: public; Owner: nexus
--

COMMENT ON COLUMN public.nexus_provider_intent_health.cooldown_until IS 'Timestamp fino al quale il provider/model e'' escluso per questo intent. Se NULL, il provider e'' utilizzabile. La logica di filtraggio e'' in brain/router/service.py.';


--
-- Name: project_code_edges; Type: TABLE; Schema: public; Owner: nexus
--

CREATE TABLE public.project_code_edges (
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


ALTER TABLE public.project_code_edges OWNER TO nexus;

--
-- Name: project_code_nodes; Type: TABLE; Schema: public; Owner: nexus
--

CREATE TABLE public.project_code_nodes (
    project_id uuid NOT NULL,
    file_path text NOT NULL,
    lang text,
    content_hash text,
    last_seen_at timestamp with time zone DEFAULT now() NOT NULL
);


ALTER TABLE public.project_code_nodes OWNER TO nexus;

--
-- Name: project_code_tests; Type: TABLE; Schema: public; Owner: nexus
--

CREATE TABLE public.project_code_tests (
    project_id uuid NOT NULL,
    test_path text NOT NULL,
    covers_path text NOT NULL,
    method text NOT NULL,
    confidence real DEFAULT 0.6 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT project_code_tests_method_check CHECK ((method = ANY (ARRAY['naming'::text, 'import'::text, 'cochange'::text, 'manual'::text])))
);


ALTER TABLE public.project_code_tests OWNER TO nexus;

--
-- Name: project_impact_runs; Type: TABLE; Schema: public; Owner: nexus
--

CREATE TABLE public.project_impact_runs (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    run_id uuid,
    change_request_note_id uuid,
    project_id uuid,
    seed_paths text[],
    impact_paths jsonb,
    gate_status text,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


ALTER TABLE public.project_impact_runs OWNER TO nexus;

--
-- Name: nexus_provider_intent_health nexus_provider_intent_health_pkey; Type: CONSTRAINT; Schema: public; Owner: nexus
--

ALTER TABLE ONLY public.nexus_provider_intent_health
    ADD CONSTRAINT nexus_provider_intent_health_pkey PRIMARY KEY (provider, model, intent_subkind);


--
-- Name: project_code_edges project_code_edges_pkey; Type: CONSTRAINT; Schema: public; Owner: nexus
--

ALTER TABLE ONLY public.project_code_edges
    ADD CONSTRAINT project_code_edges_pkey PRIMARY KEY (project_id, from_path, to_path, edge_kind);


--
-- Name: project_code_nodes project_code_nodes_pkey; Type: CONSTRAINT; Schema: public; Owner: nexus
--

ALTER TABLE ONLY public.project_code_nodes
    ADD CONSTRAINT project_code_nodes_pkey PRIMARY KEY (project_id, file_path);


--
-- Name: project_code_tests project_code_tests_pkey; Type: CONSTRAINT; Schema: public; Owner: nexus
--

ALTER TABLE ONLY public.project_code_tests
    ADD CONSTRAINT project_code_tests_pkey PRIMARY KEY (project_id, test_path, covers_path);


--
-- Name: project_impact_runs project_impact_runs_pkey; Type: CONSTRAINT; Schema: public; Owner: nexus
--

ALTER TABLE ONLY public.project_impact_runs
    ADD CONSTRAINT project_impact_runs_pkey PRIMARY KEY (id);


--
-- Name: idx_pce_from; Type: INDEX; Schema: public; Owner: nexus
--

CREATE INDEX idx_pce_from ON public.project_code_edges USING btree (project_id, from_path);


--
-- Name: idx_pce_to; Type: INDEX; Schema: public; Owner: nexus
--

CREATE INDEX idx_pce_to ON public.project_code_edges USING btree (project_id, to_path);


--
-- Name: idx_pcn_project; Type: INDEX; Schema: public; Owner: nexus
--

CREATE INDEX idx_pcn_project ON public.project_code_nodes USING btree (project_id);


--
-- Name: idx_pct_covers; Type: INDEX; Schema: public; Owner: nexus
--

CREATE INDEX idx_pct_covers ON public.project_code_tests USING btree (project_id, covers_path);


--
-- Name: idx_pir_project; Type: INDEX; Schema: public; Owner: nexus
--

CREATE INDEX idx_pir_project ON public.project_impact_runs USING btree (project_id, created_at DESC);


--
-- Name: idx_provider_intent_health_cooldown; Type: INDEX; Schema: public; Owner: nexus
--

CREATE INDEX idx_provider_intent_health_cooldown ON public.nexus_provider_intent_health USING btree (cooldown_until) WHERE (cooldown_until IS NOT NULL);


--
-- Name: idx_provider_intent_health_visits; Type: INDEX; Schema: public; Owner: nexus
--

CREATE INDEX idx_provider_intent_health_visits ON public.nexus_provider_intent_health USING btree (((success_count + failure_count)) DESC);


--
-- Name: uq_pir_run_id; Type: INDEX; Schema: public; Owner: nexus
--

CREATE UNIQUE INDEX uq_pir_run_id ON public.project_impact_runs USING btree (run_id);


--
-- Name: project_code_edges project_code_edges_project_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: nexus
--

ALTER TABLE ONLY public.project_code_edges
    ADD CONSTRAINT project_code_edges_project_id_fkey FOREIGN KEY (project_id) REFERENCES public.projects(id) ON DELETE CASCADE;


--
-- Name: project_code_nodes project_code_nodes_project_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: nexus
--

ALTER TABLE ONLY public.project_code_nodes
    ADD CONSTRAINT project_code_nodes_project_id_fkey FOREIGN KEY (project_id) REFERENCES public.projects(id) ON DELETE CASCADE;


--
-- Name: project_code_tests project_code_tests_project_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: nexus
--

ALTER TABLE ONLY public.project_code_tests
    ADD CONSTRAINT project_code_tests_project_id_fkey FOREIGN KEY (project_id) REFERENCES public.projects(id) ON DELETE CASCADE;


--
-- PostgreSQL database dump complete
--

\unrestrict wM06zbLwaNS11uksTjBQvT6NB6oLF6ldzavUMSuk1Awg2yVDgiUL2R1Al2eqspl

