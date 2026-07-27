-- 0001_chat.sql (db/migrations/project/)
-- Schema per-progetto DOMINIO CHAT, applicato al DB metadati Nexus <slug>_nexus
-- (Fase 1/2 separazione DB). Generato da pg_dump --schema-only del meta-DB e
-- ripulito: rimosse le FK verso tabelle GLOBALI (projects, users) e cross-dominio
-- non ancora presenti nel DB-progetto (workspaces, orchestrator_runs): quei
-- riferimenti diventano id logici uuid (validazione applicativa centralizzata).
-- Le FK intra-dominio chat sono mantenute.
--
-- Estensioni richieste dallo schema (uuid_generate_v4, gen_random_uuid, indici
-- trigram). Sono tutte "trusted": le crea anche nexus_app (proprietario del DB).
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE EXTENSION IF NOT EXISTS pgcrypto;

-- Funzioni trigger (self-contained) usate dalle tabelle del dominio.
CREATE OR REPLACE FUNCTION public.chat_message_attachments_set_display_id()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
DECLARE
    sess_short TEXT;
    id_short TEXT;
BEGIN
    IF NEW.display_id IS NOT NULL THEN
        RETURN NEW;
    END IF;

    -- Recupera session_id dal message_id (best-effort: se il messaggio non
    -- esiste piu' o e' orphan, usa '0000' come prefisso).
    SELECT SUBSTRING(REPLACE(session_id::text, '-', ''), 1, 4)
    INTO sess_short
    FROM chat_messages
    WHERE id = NEW.message_id
    LIMIT 1;

    IF sess_short IS NULL THEN
        sess_short := '0000';
    END IF;

    id_short := SUBSTRING(REPLACE(NEW.id::text, '-', ''), 1, 8);
    NEW.display_id := 'att_' || sess_short || '_' || id_short;
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
-- Il set_config('search_path','',...) del preambolo pg_dump e' stato RIMOSSO:
-- sqlx esegue la migrazione e l'INSERT di registrazione in _sqlx_migrations
-- nella STESSA transazione, quindi azzerare il search_path (anche is_local)
-- fa fallire quell'INSERT non qualificato con "relazione non esiste" e nessun
-- DB-progetto vergine puo' nascere. Gli oggetti sono comunque tutti qualificati
-- con lo schema public, la riga era un residuo inerte del dump.
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: ai_response_feedback; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.ai_response_feedback (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    project_id uuid NOT NULL,
    session_id uuid NOT NULL,
    message_id uuid NOT NULL,
    orchestrator_run_id uuid,
    user_id uuid NOT NULL,
    feedback_type text DEFAULT 'error'::text NOT NULL,
    intent text,
    provider text,
    model text,
    error_comment text NOT NULL,
    status text DEFAULT 'open'::text NOT NULL,
    review_note text,
    reviewed_by uuid,
    reviewed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: chat_message_attachments; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.chat_message_attachments (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    message_id uuid NOT NULL,
    project_id uuid NOT NULL,
    file_name text NOT NULL,
    file_path text NOT NULL,
    mime_type text NOT NULL,
    size_bytes bigint NOT NULL,
    kind text NOT NULL,
    kb_note_id uuid,
    indexed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    chunk_count integer DEFAULT 0 NOT NULL,
    content_hash text,
    display_id text,
    CONSTRAINT chat_message_attachments_kind_chk CHECK ((kind = ANY (ARRAY['text'::text, 'image'::text, 'binary'::text]))),
    CONSTRAINT chat_message_attachments_size_chk CHECK ((size_bytes >= 0))
);


--
-- Name: chat_messages; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.chat_messages (
    id uuid DEFAULT public.uuid_generate_v4() NOT NULL,
    session_id uuid NOT NULL,
    role text NOT NULL,
    content text NOT NULL,
    metadata jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    project_id uuid NOT NULL,
    request_message_id uuid,
    deleted_at timestamp with time zone,
    deleted_by_user_id uuid,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    kb_ingested boolean
);


--
-- Name: chat_sessions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.chat_sessions (
    id uuid DEFAULT public.uuid_generate_v4() NOT NULL,
    project_id uuid NOT NULL,
    user_id uuid,
    profile_id uuid,
    title text DEFAULT 'New Session'::text NOT NULL,
    status text DEFAULT 'active'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    preferred_provider text,
    preferred_model text,
    privacy_rerouted_at timestamp with time zone,
    automation_mode text DEFAULT 'confirm'::text NOT NULL
);


--
-- Name: nexus_conversation_summaries; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.nexus_conversation_summaries (
    id bigint NOT NULL,
    thread_id text NOT NULL,
    replaced_msg_count integer NOT NULL,
    summary_text text NOT NULL,
    model_used text NOT NULL,
    latency_ms integer NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: nexus_conversation_summaries_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.nexus_conversation_summaries_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: nexus_conversation_summaries_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.nexus_conversation_summaries_id_seq OWNED BY public.nexus_conversation_summaries.id;


--
-- Name: nexus_session_worklog; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.nexus_session_worklog (
    session_id uuid NOT NULL,
    project_id uuid,
    rendered_block text DEFAULT ''::text NOT NULL,
    events_count integer DEFAULT 0 NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: nexus_session_worklog_events; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.nexus_session_worklog_events (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    session_id uuid NOT NULL,
    project_id uuid,
    run_id uuid,
    kind text NOT NULL,
    payload jsonb NOT NULL,
    source text DEFAULT 'deterministic'::text NOT NULL,
    dedup_key text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT nexus_session_worklog_events_kind_check CHECK ((kind = ANY (ARRAY['file_touched'::text, 'command'::text, 'error'::text, 'retry_ok'::text, 'failed_attempt'::text, 'status'::text, 'decision'::text]))),
    CONSTRAINT nexus_session_worklog_events_source_check CHECK ((source = ANY (ARRAY['deterministic'::text, 'distilled'::text])))
);


--
-- Name: project_open_sessions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.project_open_sessions (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    project_id uuid NOT NULL,
    workspace_id uuid,
    active_file_paths jsonb DEFAULT '[]'::jsonb NOT NULL,
    terminal_cwd text,
    last_opened_at timestamp with time zone DEFAULT now() NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: prompt_corrections; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE IF NOT EXISTS public.prompt_corrections (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    project_id uuid NOT NULL,
    feedback_id uuid,
    session_id uuid,
    message_id uuid,
    orchestrator_run_id uuid,
    intent text,
    provider text,
    model text,
    correction_text text NOT NULL,
    normalized_hint_hash text NOT NULL,
    qdrant_point_id text NOT NULL,
    active boolean DEFAULT true NOT NULL,
    status text DEFAULT 'open'::text NOT NULL,
    retrieved_count bigint DEFAULT 0 NOT NULL,
    last_retrieved_at timestamp with time zone,
    resolved_at timestamp with time zone,
    deleted_at timestamp with time zone,
    metadata jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    type text DEFAULT 'correction'::text NOT NULL
);


--
-- Name: nexus_conversation_summaries id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_conversation_summaries ALTER COLUMN id SET DEFAULT nextval('public.nexus_conversation_summaries_id_seq'::regclass);


--
-- Name: ai_response_feedback ai_response_feedback_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_response_feedback
    ADD CONSTRAINT ai_response_feedback_pkey PRIMARY KEY (id);


--
-- Name: chat_message_attachments chat_message_attachments_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_message_attachments
    ADD CONSTRAINT chat_message_attachments_pkey PRIMARY KEY (id);


--
-- Name: chat_messages chat_messages_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_messages
    ADD CONSTRAINT chat_messages_pkey PRIMARY KEY (id);


--
-- Name: chat_sessions chat_sessions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_sessions
    ADD CONSTRAINT chat_sessions_pkey PRIMARY KEY (id);


--
-- Name: nexus_conversation_summaries nexus_conversation_summaries_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_conversation_summaries
    ADD CONSTRAINT nexus_conversation_summaries_pkey PRIMARY KEY (id);


--
-- Name: nexus_session_worklog_events nexus_session_worklog_events_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_session_worklog_events
    ADD CONSTRAINT nexus_session_worklog_events_pkey PRIMARY KEY (id);


--
-- Name: nexus_session_worklog_events nexus_session_worklog_events_session_id_dedup_key_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_session_worklog_events
    ADD CONSTRAINT nexus_session_worklog_events_session_id_dedup_key_key UNIQUE (session_id, dedup_key);


--
-- Name: nexus_session_worklog nexus_session_worklog_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_session_worklog
    ADD CONSTRAINT nexus_session_worklog_pkey PRIMARY KEY (session_id);


--
-- Name: project_open_sessions project_open_sessions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_open_sessions
    ADD CONSTRAINT project_open_sessions_pkey PRIMARY KEY (id);


--
-- Name: project_open_sessions project_open_sessions_user_id_project_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_open_sessions
    ADD CONSTRAINT project_open_sessions_user_id_project_id_key UNIQUE (user_id, project_id);


--
-- Name: prompt_corrections prompt_corrections_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.prompt_corrections
    ADD CONSTRAINT prompt_corrections_pkey PRIMARY KEY (id);


--
-- Name: idx_ai_response_feedback_intent_provider_created; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_ai_response_feedback_intent_provider_created ON public.ai_response_feedback USING btree (project_id, intent, provider, created_at DESC);


--
-- Name: idx_ai_response_feedback_message_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_ai_response_feedback_message_id ON public.ai_response_feedback USING btree (message_id);


--
-- Name: idx_ai_response_feedback_orchestrator_run_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_ai_response_feedback_orchestrator_run_id ON public.ai_response_feedback USING btree (orchestrator_run_id);


--
-- Name: idx_ai_response_feedback_project_status_created; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_ai_response_feedback_project_status_created ON public.ai_response_feedback USING btree (project_id, status, created_at DESC);


--
-- Name: idx_ai_response_feedback_reviewed_by; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_ai_response_feedback_reviewed_by ON public.ai_response_feedback USING btree (reviewed_by);


--
-- Name: idx_ai_response_feedback_session_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_ai_response_feedback_session_id ON public.ai_response_feedback USING btree (session_id);


--
-- Name: idx_ai_response_feedback_user_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_ai_response_feedback_user_id ON public.ai_response_feedback USING btree (user_id);


--
-- Name: idx_chat_message_attachments_display_id; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX IF NOT EXISTS idx_chat_message_attachments_display_id ON public.chat_message_attachments USING btree (display_id) WHERE (display_id IS NOT NULL);


--
-- Name: idx_chat_message_attachments_indexed_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_chat_message_attachments_indexed_at ON public.chat_message_attachments USING btree (indexed_at);


--
-- Name: idx_chat_message_attachments_kb_note; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_chat_message_attachments_kb_note ON public.chat_message_attachments USING btree (kb_note_id) WHERE (kb_note_id IS NOT NULL);


--
-- Name: idx_chat_message_attachments_message; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_chat_message_attachments_message ON public.chat_message_attachments USING btree (message_id);


--
-- Name: idx_chat_message_attachments_project; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_chat_message_attachments_project ON public.chat_message_attachments USING btree (project_id);


--
-- Name: idx_chat_message_attachments_project_hash; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_chat_message_attachments_project_hash ON public.chat_message_attachments USING btree (project_id, content_hash) WHERE (content_hash IS NOT NULL);


--
-- Name: idx_chat_messages_deleted_by_user_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_chat_messages_deleted_by_user_id ON public.chat_messages USING btree (deleted_by_user_id);


--
-- Name: idx_chat_messages_kb_ingest_pending; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_chat_messages_kb_ingest_pending ON public.chat_messages USING btree (created_at) WHERE ((role = 'user'::text) AND (kb_ingested IS NULL));


--
-- Name: idx_chat_messages_request_message_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_chat_messages_request_message_id ON public.chat_messages USING btree (request_message_id);


--
-- Name: idx_chat_messages_session; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_chat_messages_session ON public.chat_messages USING btree (session_id, created_at);


--
-- Name: idx_chat_messages_session_project_created; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_chat_messages_session_project_created ON public.chat_messages USING btree (session_id, project_id, created_at);


--
-- Name: idx_chat_sessions_user_project_updated; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_chat_sessions_user_project_updated ON public.chat_sessions USING btree (user_id, project_id, updated_at DESC);


--
-- Name: idx_conv_summaries_thread_created; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_conv_summaries_thread_created ON public.nexus_conversation_summaries USING btree (thread_id, created_at DESC);


--
-- Name: idx_project_open_sessions_user_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_project_open_sessions_user_id ON public.project_open_sessions USING btree (user_id, updated_at DESC);


--
-- Name: idx_project_open_sessions_workspace_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_project_open_sessions_workspace_id ON public.project_open_sessions USING btree (workspace_id);


--
-- Name: idx_prompt_corrections_feedback_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_prompt_corrections_feedback_id ON public.prompt_corrections USING btree (feedback_id);


--
-- Name: idx_prompt_corrections_message_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_prompt_corrections_message_id ON public.prompt_corrections USING btree (message_id);


--
-- Name: idx_prompt_corrections_orchestrator_run_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_prompt_corrections_orchestrator_run_id ON public.prompt_corrections USING btree (orchestrator_run_id);


--
-- Name: idx_prompt_corrections_project_active_created; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_prompt_corrections_project_active_created ON public.prompt_corrections USING btree (project_id, active, created_at DESC);


--
-- Name: idx_prompt_corrections_project_hash; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_prompt_corrections_project_hash ON public.prompt_corrections USING btree (project_id, normalized_hint_hash);


--
-- Name: idx_prompt_corrections_qdrant_point_id; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX IF NOT EXISTS idx_prompt_corrections_qdrant_point_id ON public.prompt_corrections USING btree (qdrant_point_id);


--
-- Name: idx_prompt_corrections_session_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_prompt_corrections_session_id ON public.prompt_corrections USING btree (session_id);


--
-- Name: idx_prompt_corrections_type; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_prompt_corrections_type ON public.prompt_corrections USING btree (project_id, type);


--
-- Name: idx_session_worklog_events_session_created; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX IF NOT EXISTS idx_session_worklog_events_session_created ON public.nexus_session_worklog_events USING btree (session_id, created_at);


--
-- Name: chat_message_attachments trg_chat_message_attachments_set_display_id; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_chat_message_attachments_set_display_id BEFORE INSERT ON public.chat_message_attachments FOR EACH ROW EXECUTE FUNCTION public.chat_message_attachments_set_display_id();


--
-- Name: ai_response_feedback ai_response_feedback_message_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_response_feedback
    ADD CONSTRAINT ai_response_feedback_message_id_fkey FOREIGN KEY (message_id) REFERENCES public.chat_messages(id) ON DELETE CASCADE;


--
-- Name: ai_response_feedback ai_response_feedback_orchestrator_run_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: ai_response_feedback ai_response_feedback_project_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: ai_response_feedback ai_response_feedback_reviewed_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: ai_response_feedback ai_response_feedback_session_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_response_feedback
    ADD CONSTRAINT ai_response_feedback_session_id_fkey FOREIGN KEY (session_id) REFERENCES public.chat_sessions(id) ON DELETE CASCADE;


--
-- Name: ai_response_feedback ai_response_feedback_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: chat_message_attachments chat_message_attachments_message_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_message_attachments
    ADD CONSTRAINT chat_message_attachments_message_id_fkey FOREIGN KEY (message_id) REFERENCES public.chat_messages(id) ON DELETE CASCADE;


--
-- Name: chat_message_attachments chat_message_attachments_project_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: chat_messages chat_messages_deleted_by_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: chat_messages chat_messages_project_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: chat_messages chat_messages_request_message_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_messages
    ADD CONSTRAINT chat_messages_request_message_id_fkey FOREIGN KEY (request_message_id) REFERENCES public.chat_messages(id) ON DELETE SET NULL;


--
-- Name: chat_messages chat_messages_session_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_messages
    ADD CONSTRAINT chat_messages_session_id_fkey FOREIGN KEY (session_id) REFERENCES public.chat_sessions(id) ON DELETE CASCADE;


--
-- Name: chat_sessions chat_sessions_project_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: chat_sessions chat_sessions_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: nexus_session_worklog_events nexus_session_worklog_events_session_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_session_worklog_events
    ADD CONSTRAINT nexus_session_worklog_events_session_id_fkey FOREIGN KEY (session_id) REFERENCES public.chat_sessions(id) ON DELETE CASCADE;


--
-- Name: nexus_session_worklog nexus_session_worklog_session_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.nexus_session_worklog
    ADD CONSTRAINT nexus_session_worklog_session_id_fkey FOREIGN KEY (session_id) REFERENCES public.chat_sessions(id) ON DELETE CASCADE;


--
-- Name: project_open_sessions project_open_sessions_project_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: project_open_sessions project_open_sessions_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: project_open_sessions project_open_sessions_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: prompt_corrections prompt_corrections_feedback_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.prompt_corrections
    ADD CONSTRAINT prompt_corrections_feedback_id_fkey FOREIGN KEY (feedback_id) REFERENCES public.ai_response_feedback(id) ON DELETE SET NULL;


--
-- Name: prompt_corrections prompt_corrections_message_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.prompt_corrections
    ADD CONSTRAINT prompt_corrections_message_id_fkey FOREIGN KEY (message_id) REFERENCES public.chat_messages(id) ON DELETE SET NULL;


--
-- Name: prompt_corrections prompt_corrections_orchestrator_run_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: prompt_corrections prompt_corrections_project_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: prompt_corrections prompt_corrections_session_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.prompt_corrections
    ADD CONSTRAINT prompt_corrections_session_id_fkey FOREIGN KEY (session_id) REFERENCES public.chat_sessions(id) ON DELETE SET NULL;


--
-- PostgreSQL database dump complete
--


