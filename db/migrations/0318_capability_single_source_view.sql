-- 0318_capability_single_source_view.sql  (ADR 0024)
--
-- Difetto strutturale: tre flag semantici di capability duplicati FISICAMENTE
-- su due tabelle, senza vincolo che li leghi -> drift inevitabile (incidente
-- deepseek-v4: routing lo credeva agentico, l'adapter no). I duplicati:
--   ai_price_catalog.supports_tool_use      <-> nexus_provider_capabilities.tool_use
--   ai_price_catalog.capabilities->>'vision'<-> nexus_provider_capabilities.vision
--   (concetto thinking, vedi sotto)
--
-- Due concetti "thinking" DISTINTI (non vanno fusi):
--   A) "escludi dal routing agentico" = ai_price_catalog.is_thinking  (solo Rust)
--   B) "gira in thinking mode -> non forzare tool_choice + budget"
--      = nexus_provider_capabilities.thinking (solo brain/adapter)
-- Claude ha legittimamente A=false (ottimo agentico) e B=true (extended thinking).
--
-- Soluzione definitiva (fonte unica + derivazione):
--   1. ai_price_catalog diventa l'UNICA casa fisica dei flag semantici, con
--      colonne booleane reali: supports_tool_use (gia'), is_thinking (gia', A),
--      supports_vision (nuova), uses_thinking_mode (nuova, B).
--   2. nexus_provider_capabilities tiene SOLO le meccaniche di chiamata; le
--      colonne thinking/tool_use/vision vengono DROPPATE.
--   3. La vista v_model_capabilities deriva i flag dal catalog: il brain legge
--      da li'. Niente seconda colonna scrivibile -> drift IMPOSSIBILE.
--   4. capability_source ('auto'|'manual') protegge le classificazioni curate a
--      mano dal classificatore automatico del catalog_sync.
--
-- Idempotente.

-- 1. Colonne canoniche nel catalog (concetto B + vision + sorgente classificazione).
ALTER TABLE ai_price_catalog
    ADD COLUMN IF NOT EXISTS supports_vision    boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS uses_thinking_mode boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS capability_source  text    NOT NULL DEFAULT 'auto';

ALTER TABLE ai_price_catalog DROP CONSTRAINT IF EXISTS chk_capability_source;
ALTER TABLE ai_price_catalog
    ADD CONSTRAINT chk_capability_source CHECK (capability_source IN ('auto', 'manual'));

-- 2. Riconciliazione una-tantum dalla fonte oggi popolata (nexus_provider_capabilities).
--    Le colonne jsonb capabilities->>'thinking'/'vision' sono vuote: la verita'
--    corrente vive in caps.thinking / caps.vision.
UPDATE ai_price_catalog c
SET supports_vision    = COALESCE(cap.vision, false),
    uses_thinking_mode = COALESCE(cap.thinking, false),
    updated_at         = NOW()
FROM nexus_provider_capabilities cap
WHERE cap.provider = c.provider AND cap.model = c.model;

-- supports_tool_use: allinea i (pochi) mismatch al valore dell'adapter (caps),
-- che e' la fonte storicamente usata per chiamare il provider.
UPDATE ai_price_catalog c
SET supports_tool_use = cap.tool_use,
    updated_at        = NOW()
FROM nexus_provider_capabilities cap
WHERE cap.provider = c.provider AND cap.model = c.model
  AND c.supports_tool_use IS DISTINCT FROM cap.tool_use;

-- 3. Protezione curature: marca 'manual' tutte le righe con una decisione
--    "thinking" deliberata (concetto A o B). Sono le piu' sensibili e il
--    classificatore euristico non deve ribaltarle al prossimo sync.
UPDATE ai_price_catalog
SET capability_source = 'manual',
    updated_at        = NOW()
WHERE is_thinking = true OR uses_thinking_mode = true;

-- 4. Vista unica: meccaniche da capabilities + flag semantici derivati dal catalog.
--    Guidata da capabilities (preserva eventuali righe wildcard provider,'*');
--    i flag arrivano dal catalog con default conservativi se manca la riga.
CREATE OR REPLACE VIEW v_model_capabilities AS
SELECT cap.provider,
       cap.model,
       COALESCE(c.supports_tool_use, true)   AS tool_use,
       COALESCE(c.supports_vision, false)    AS vision,
       COALESCE(c.uses_thinking_mode, false) AS thinking,
       cap.max_context_tokens,
       cap.default_max_output_tokens,
       cap.max_output_tokens_hard,
       cap.tool_choice_style,
       cap.tool_choice_first_turn_force,
       cap.schema_strict,
       cap.schema_dialect,
       cap.tool_call_format,
       cap.max_tools_in_request,
       cap.supports_prompt_cache,
       cap.prompt_cache_dialect,
       cap.supports_parallel_tools,
       cap.stop_reason_dialect,
       cap.soft_failure_iter_threshold,
       cap.soft_failure_content_threshold,
       cap.history_keep_recent_messages,
       cap.history_max_old_tool_result_chars,
       cap.request_timeout_seconds,
       cap.connect_timeout_seconds,
       cap.tool_result_max_chars,
       cap.tool_result_max_bytes,
       cap.tool_result_max_lines
FROM nexus_provider_capabilities cap
LEFT JOIN ai_price_catalog c
       ON c.provider = cap.provider AND c.model = cap.model;

-- 5. Drop dei flag duplicati: ora derivati dal catalog (fonte unica).
--    Una futura UPDATE nexus_provider_capabilities SET thinking=... fallirebbe
--    (colonna inesistente): ogni scrittura e' forzata sull'unica colonna canonica.
ALTER TABLE nexus_provider_capabilities
    DROP COLUMN IF EXISTS thinking,
    DROP COLUMN IF EXISTS tool_use,
    DROP COLUMN IF EXISTS vision;

-- 6. Indice jsonb obsoleto (capabilities->>'thinking', sempre vuoto) rimpiazzato
--    da uno sulla colonna canonica di esclusione agentica.
DROP INDEX IF EXISTS idx_ai_price_catalog_thinking;
CREATE INDEX IF NOT EXISTS idx_ai_price_catalog_is_thinking
    ON ai_price_catalog (provider, model)
    WHERE is_thinking = true AND is_enabled = true;
