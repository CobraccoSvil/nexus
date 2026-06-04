-- Migrazione 0285 — ADR 0016: completamento pipeline RAG strutturale + safety net.
--
-- Aggiunge tutti i settings per Fase A.1, A.2, A.3, A.4, A.5, A.6, B, C, D.
-- Estende `agent_runs` con tracking dell'upscale e della stima token pre-call.
-- Idempotente: tutti gli INSERT usano ON CONFLICT (key) DO NOTHING; ALTER TABLE
-- usa IF NOT EXISTS.

BEGIN;

-- ── Settings: tutti i parametri della pipeline RAG completa ───────────────
INSERT INTO settings (key, value, category, description, updated_at) VALUES
    -- ── Fase A.1: offload preventivo system prompt + project context ──
    ('agent.context.system_prompt_offload_threshold_tokens', '8000', 'agent',
     'Soglia (token) sopra cui il system prompt + project context viene offloadato in Qdrant tool_results_chunks con source_kind=system_context. Sotto soglia: inline (default). Default 8000.',
     NOW()),
    ('agent.context.system_prompt_summary_max_tokens', '800', 'agent',
     'Lunghezza massima (token) del summary che sostituisce il blocco offloadato. L''agente recupera i dettagli con nexus_search_semantic(source_kinds=system_context).',
     NOW()),

    -- ── Fase A.2: tool discovery on-demand ──
    ('agent.tools.inline_core_count', '15', 'agent',
     'Numero di tool che restano inline nel prompt (i piu'' usati statisticamente). Gli altri sono indicizzati in Qdrant agent_tools_descriptors e raggiungibili con nexus_mcp_tool_search. Default 15.',
     NOW()),
    ('agent.tools.inline_core_whitelist', 'read_file,write_file,edit_file,list_files,search_in_files,run_command,git_status,git_commit,run_tests,nexus_search_semantic,nexus_mcp_tool_search,nexus_mcp_tool_call,nexus_db_query,nexus_todo_write,knowledge_search', 'agent',
     'CSV dei tool core sempre inline nel prompt agente. Modificabile via UI Admin/Agenti.',
     NOW()),
    ('agent.tools.discovery_enabled', 'true', 'agent',
     'Se true, attiva la modalita'' tool discovery: solo i core inline, gli altri via nexus_mcp_tool_search. Default true per i nuovi run.',
     NOW()),

    -- ── Fase A.3: rolling summary cross-turno ──
    ('agent.context.rolling_summary_enabled', 'true', 'agent',
     'Se true, ogni rolling_window_turns sostituisce i messaggi vecchi con un summary compatto (originali retrievabili via nexus_search_semantic source_kinds=chat_history).',
     NOW()),
    ('agent.context.rolling_window_turns', '5', 'agent',
     'Frequenza compaction: ogni N turni esegue il rolling summary dei turni piu'' vecchi. Default 5.',
     NOW()),
    ('agent.context.rolling_keep_recent_turns', '3', 'agent',
     'Numero di turni recenti SEMPRE preservati integri (mai sostituiti dal summary). Default 3.',
     NOW()),
    ('agent.context.rolling_summary_model', 'google/gemini-2.5-flash-lite', 'agent',
     'Modello usato per generare il summary di compaction. Deve essere veloce ed economico (input compresso).',
     NOW()),

    -- ── Fase A.4: forced offload trigger ──
    ('agent.context.forced_rag_threshold_ratio', '0.40', 'agent',
     'Sopra ratio*window scatta forced_rag_reminder nel system prompt: l''agente e'' istruito a usare nexus_search_semantic prima di rispondere. Default 0.40 (40% del context window).',
     NOW()),
    ('agent.context.forced_rag_reminder_text',
     'Il contesto disponibile e'' parzialmente offloadato in tool_results_chunks. Prima di rispondere a richieste che richiedono dettagli specifici, chiama nexus_search_semantic(query=...). Non assumere di vedere tutto il contesto: chiedi quello che ti serve.',
     'agent',
     'Testo della reminder iniettata nel system prompt + ultimo HumanMessage quando est_tokens > forced_rag_threshold_ratio * window. Modificabile per A/B testing.',
     NOW()),

    -- ── Fase A.5: cross-turn tool_result cache ──
    ('agent.tools.result_cache_enabled', 'true', 'agent',
     'Se true, tool_result vengono cachati in Redis con key sha256(tool_name+args). Replay identici ritornano cache_ref invece del payload.',
     NOW()),
    ('agent.tools.result_cache_ttl_seconds', '1800', 'agent',
     'TTL della cache tool_result (Redis). Default 1800s = 30 min.',
     NOW()),
    ('agent.tools.result_cache_skip_for',
     'run_command,run_tests,git_commit,git_push,write_file,edit_file,delete_file,fs_mkdir,fs_move,fs_copy,nexus_db_query,build_project_image,dispatch_subagent,dispatch_subagents',
     'agent',
     'CSV dei tool con side-effect o output non deterministico, MAI cachati. Aggiungi qui tool nuovi con side effect.',
     NOW()),

    -- ── Fase A.6: KB graph summary mode ──
    ('agent.kb.graph_summary_threshold_topk', '20', 'agent',
     'Sopra top_k threshold, knowledge_search ritorna clusters {theme, count, sample_titles} invece di N body completi. Default 20.',
     NOW()),
    ('agent.kb.cluster_method', 'embedding_kmeans', 'agent',
     'Metodo di clustering per KB graph summary. Valori: embedding_kmeans (default), tags_groupby, manual.',
     NOW()),

    -- ── Fase B: compattazione tool descriptions ──
    ('agent.tools.max_description_tokens', '40', 'agent',
     'Target massimo (token) per ciascuna description di tool. Lint redazionale in CI segnala violazioni. Default 40.',
     NOW()),
    ('agent.tools.regression_test_enabled', 'true', 'agent',
     'Worker tool_selection_regression_worker: confronta scelta tool su 30 prompt baseline vs definitions correnti. Soglia accettazione 95%.',
     NOW()),

    -- ── Fase C: smart upscale ──
    ('agent.upscale.enabled', 'true', 'agent',
     'Se true, prima di chiamare il provider, se est_tokens > 0.9*model.context_window cerca un modello con window maggiore nella routing matrix.',
     NOW()),
    ('agent.upscale.target_overhead_ratio', '1.2', 'agent',
     'Margine di sicurezza: il modello upscaled deve avere context_window >= est_tokens * target_overhead_ratio. Default 1.2 (20% margine).',
     NOW()),
    ('agent.upscale.preferred_targets',
     'claude-opus-4-6,gemini-2.5-pro,gpt-5.5,claude-sonnet-4-6',
     'agent',
     'CSV ordinato dei modelli con window grande preferiti per l''upscale. Il primo disponibile e abilitato in ai_price_catalog viene scelto.',
     NOW()),
    ('agent.upscale.cost_cap_usd_per_run', '0.50', 'agent',
     'Cap di sicurezza: se il modello upscaled costerebbe > cap stimato per il singolo run, errore in UI invece di procedere. Default 0.50 USD.',
     NOW()),

    -- ── Fase D: brake con tokenizer reale + fail-fast ──
    ('agent.context.tokenizer', 'cl100k_base', 'agent',
     'Tokenizer per stima token (tiktoken). Valori: cl100k_base (default, accurato per Claude/GPT/Mistral), o200k_base (GPT-4o), p50k_base (legacy).',
     NOW()),
    ('agent.context.hard_cap_ratio', '0.95', 'agent',
     'Cap hard finale: se dopo offload+upscale il payload supera ratio*window, errore esplicito (no hallucinazione silenziosa). Default 0.95.',
     NOW()),
    ('agent.context.overflow_message_key', 'system.context_overflow', 'agent',
     'Chiave in nexus_prompt_templates per il messaggio di overflow visualizzato in UI. Permette override redazionale senza redeploy.',
     NOW())
ON CONFLICT (key) DO NOTHING;

-- ── Estensione agent_runs: tracking upscale e stima token ────────────────
ALTER TABLE agent_runs
    ADD COLUMN IF NOT EXISTS upscale_from        TEXT,
    ADD COLUMN IF NOT EXISTS upscale_to          TEXT,
    ADD COLUMN IF NOT EXISTS upscale_reason      TEXT,
    ADD COLUMN IF NOT EXISTS est_tokens_at_call  INTEGER;

-- ── Template messaggio overflow (Fase D) ─────────────────────────────────
INSERT INTO nexus_prompt_templates (key, category, title, content, placeholder_vars, updated_at) VALUES
    ('system.context_overflow', 'system', 'Context overflow user message',
     E'Il contesto della chat supera la capacita'' di tutti i modelli configurati (stima: %ESTIMATED_TOKENS% token, window massimo: %MAX_WINDOW% token).\n\nSuggerimenti:\n1. Avvia una nuova chat per spezzare il context (la KB del progetto resta accessibile via search)\n2. Riduci gli allegati attivi (rimuovi dalla chat quelli non necessari)\n3. Chiedi all''admin di abilitare modelli con context window superiore',
     '["ESTIMATED_TOKENS","MAX_WINDOW"]'::jsonb,
     NOW())
ON CONFLICT (key) DO NOTHING;

COMMIT;
