-- 0240_provider_capabilities.sql
-- M0 (provider abstraction) — Capability matrix per (provider, model).
--
-- Fonte unica di verita' per i parametri per-modello che oggi erano hardcoded
-- nei provider Python (max_tokens, tool_choice style, dialetto schema, dialetto
-- documentazione tool, cache prompt, timeout, soglie soft-failure, compaction
-- history, compressione schema). Regola G del CLAUDE.md: niente fallback
-- hardcoded nel codice, la configurazione vive solo qui.
--
-- Risoluzione lato loader: si cerca prima la riga esatta (provider, model);
-- se assente si ricade sulla riga wildcard (provider, '*'). I provider a
-- modello dinamico (vllm, ollama) hanno solo la riga wildcard. Se nemmeno la
-- wildcard esiste -> CapabilityUnavailable (errore visibile, niente magia).

CREATE TABLE IF NOT EXISTS nexus_provider_capabilities (
    provider                       TEXT    NOT NULL,
    model                          TEXT    NOT NULL,  -- '*' = default di provider

    -- Budget risposta
    max_output_tokens              INTEGER NOT NULL DEFAULT 8192,

    -- Tool calling
    supports_tool_use              BOOLEAN NOT NULL DEFAULT TRUE,
    tool_choice_style              TEXT    NOT NULL DEFAULT 'openai',
    tool_choice_first_turn_force   BOOLEAN NOT NULL DEFAULT TRUE,
    max_tools_in_request           INTEGER,                       -- NULL = nessun cap
    schema_dialect                 TEXT    NOT NULL DEFAULT 'openai_strict',
    tool_documentation_dialect     TEXT    NOT NULL DEFAULT 'markdown',
    tool_result_max_chars          INTEGER NOT NULL DEFAULT 20000,

    -- Capacita' modello
    supports_vision                BOOLEAN NOT NULL DEFAULT FALSE,
    supports_thinking              BOOLEAN NOT NULL DEFAULT FALSE,
    prompt_cache_dialect           TEXT    NOT NULL DEFAULT 'none',

    -- Timeout richiesta (secondi)
    request_timeout_seconds        INTEGER NOT NULL DEFAULT 90,

    -- History compaction
    history_keep_recent            INTEGER NOT NULL DEFAULT 12,
    history_max_old_result_chars   INTEGER NOT NULL DEFAULT 2000,
    history_max_old_result_chars_min INTEGER NOT NULL DEFAULT 400,

    -- Soft-failure detection (M4)
    soft_failure_iter_threshold    INTEGER NOT NULL DEFAULT 2,
    soft_failure_content_threshold INTEGER NOT NULL DEFAULT 200,

    -- Compressione schema (M0 H-16)
    schema_descr_max               INTEGER NOT NULL DEFAULT 200,
    schema_enum_max                INTEGER NOT NULL DEFAULT 10,
    schema_tool_descr_max          INTEGER NOT NULL DEFAULT 400,

    notes                          TEXT,
    updated_at                     TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    PRIMARY KEY (provider, model),
    CONSTRAINT chk_tool_choice_style
        CHECK (tool_choice_style IN ('openai', 'anthropic', 'google', 'none')),
    CONSTRAINT chk_schema_dialect
        CHECK (schema_dialect IN ('anthropic', 'openai_strict', 'openai_loose', 'google', 'text_fallback')),
    CONSTRAINT chk_tool_doc_dialect
        CHECK (tool_documentation_dialect IN ('xml', 'markdown', 'json')),
    CONSTRAINT chk_prompt_cache_dialect
        CHECK (prompt_cache_dialect IN ('anthropic', 'none'))
);

-- ── Righe wildcard: default calibrato per provider ────────────────────────────
-- Valori scelti per riprodurre ESATTAMENTE il comportamento attuale (nessun
-- cambiamento funzionale, solo ridirezione della fonte).

INSERT INTO nexus_provider_capabilities
    (provider, model, max_output_tokens, supports_tool_use, tool_choice_style,
     tool_choice_first_turn_force, max_tools_in_request, schema_dialect,
     tool_documentation_dialect, supports_vision, supports_thinking,
     prompt_cache_dialect, request_timeout_seconds, notes)
VALUES
    ('anthropic', '*', 8192, TRUE,  'anthropic', TRUE,  NULL, 'anthropic',     'xml',      TRUE,  FALSE, 'anthropic', 90, 'default provider anthropic'),
    ('openai',    '*', 8192, TRUE,  'openai',    TRUE,  NULL, 'openai_strict', 'markdown', TRUE,  FALSE, 'none',      90, 'default provider openai'),
    ('google',    '*', 8192, TRUE,  'google',    TRUE,  NULL, 'google',        'json',     TRUE,  FALSE, 'none',      90, 'default provider google'),
    ('mistral',   '*', 8192, TRUE,  'openai',    TRUE,  NULL, 'openai_loose',  'markdown', FALSE, FALSE, 'none',      90, 'default provider mistral'),
    ('deepseek',  '*', 8192, TRUE,  'openai',    TRUE,  NULL, 'openai_loose',  'markdown', FALSE, FALSE, 'none',      90, 'default provider deepseek'),
    ('vllm',      '*', 4096, TRUE,  'none',      FALSE, NULL, 'text_fallback', 'markdown', FALSE, FALSE, 'none',     120, 'default provider vllm (tool-mute, text fallback)'),
    ('ollama',    '*', 4096, TRUE,  'none',      FALSE, NULL, 'text_fallback', 'markdown', FALSE, FALSE, 'none',     120, 'default provider ollama (tool-mute, text fallback)')
ON CONFLICT (provider, model) DO NOTHING;

-- ── Override O-series OpenAI: cap tool e niente force al primo turno ──────────
-- I modelli o1/o-series storicamente limitano il numero di tool e mal tollerano
-- tool_choice forzato. Backfill mirato sui modelli o* presenti nel catalog.
INSERT INTO nexus_provider_capabilities
    (provider, model, max_output_tokens, supports_tool_use, tool_choice_style,
     tool_choice_first_turn_force, max_tools_in_request, schema_dialect,
     tool_documentation_dialect, supports_vision, supports_thinking,
     prompt_cache_dialect, request_timeout_seconds, notes)
SELECT provider, model, 8192, supports_tool_use, 'openai',
       FALSE, 20, 'openai_strict', 'markdown', FALSE, TRUE, 'none', 120,
       'override o-series (cap 20 tool, no force, reasoning)'
FROM ai_price_catalog
WHERE provider = 'openai' AND (model = 'o1' OR model LIKE 'o1-%' OR model LIKE 'o3%' OR model LIKE 'o4%')
ON CONFLICT (provider, model) DO NOTHING;

-- ── Backfill per-modello dal catalog: vision e thinking calibrati ────────────
-- Eredita i default di provider ma valorizza supports_thinking/vision a partire
-- dalle capability gia' registrate in ai_price_catalog.capabilities (JSONB).
INSERT INTO nexus_provider_capabilities
    (provider, model, max_output_tokens, supports_tool_use, tool_choice_style,
     tool_choice_first_turn_force, schema_dialect, tool_documentation_dialect,
     supports_vision, supports_thinking, prompt_cache_dialect,
     request_timeout_seconds, notes)
SELECT c.provider, c.model, 8192, c.supports_tool_use,
       CASE c.provider WHEN 'anthropic' THEN 'anthropic' WHEN 'google' THEN 'google' ELSE 'openai' END,
       TRUE,
       CASE c.provider
            WHEN 'anthropic' THEN 'anthropic'
            WHEN 'google'    THEN 'google'
            WHEN 'mistral'   THEN 'openai_loose'
            WHEN 'deepseek'  THEN 'openai_loose'
            ELSE 'openai_strict' END,
       CASE c.provider WHEN 'anthropic' THEN 'xml' WHEN 'google' THEN 'json' ELSE 'markdown' END,
       FALSE,
       COALESCE(c.capabilities @> '[{"thinking": true}]'::jsonb, FALSE),
       CASE c.provider WHEN 'anthropic' THEN 'anthropic' ELSE 'none' END,
       CASE c.provider WHEN 'vllm' THEN 120 WHEN 'ollama' THEN 120 ELSE 90 END,
       'backfill da ai_price_catalog'
FROM ai_price_catalog c
WHERE c.is_enabled = TRUE
ON CONFLICT (provider, model) DO NOTHING;
