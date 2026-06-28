-- 0478_media_capabilities.sql  (PR6a — FONDAMENTA media routing per capability)
--
-- ROOT CAUSE: i modelli MEDIA (image-generation, audio, video) sono oggi
-- SCARTATI all'insert da `is_chat_compatible_model` (blacklist binaria in
-- model_catalog_sync.rs): dall-e/imagen/tts/whisper non entrano mai nel catalog.
-- Conseguenza: non c'e' modo di classificarli ne' instradarli per CAPABILITY, e
-- se per qualche via entrassero, il routing chat-agentico li sceglierebbe per
-- errore (non distingue un image-gen da un chat).
--
-- Difetto strutturale parallelo a quello che la mig 0318 ha risolto per `vision`:
-- la capability "media" non ha una colonna canonica nel catalog ne' e' esposta
-- dalla vista unica `v_model_capabilities`. Senza colonna canonica:
--   - il selettore (model_selection.rs) non puo' filtrare per media kind;
--   - non si puo' escludere i media dai purpose TESTUALI (un image-gen
--     risalirebbe la classifica del routing chat).
--
-- SOLUZIONE (stesso pattern della 0318/0319 per vision, regola L "punto unico"):
--   1. 4 colonne booleane canoniche nel catalog (UNICA casa fisica dei flag).
--   2. la vista v_model_capabilities deriva i 4 flag dal catalog (il brain li
--      legge da li', niente seconda colonna scrivibile -> drift impossibile).
--   3. backfill una-tantum per nome, SOLO sulle righe non curate a mano
--      (capability_source IS DISTINCT FROM 'manual'): rispetta gli override admin.
--
-- Questo PR e' DORMIENTE: nessun endpoint gateway, nessun tool. Abilita solo
-- schema + classificazione + selezione. Gli endpoint/tool seguiranno coi
-- rispettivi purpose. Per ora si seed-a solo il purpose 'generate_image'.
--
-- Idempotente.

-- 1. Colonne canoniche nel catalog (concetto media, fonte unica fisica).
ALTER TABLE ai_price_catalog
    ADD COLUMN IF NOT EXISTS supports_image_gen boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS supports_audio_in  boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS supports_audio_out boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS supports_video_gen boolean NOT NULL DEFAULT false;

-- 2. Vista unica: stessa struttura della 0319 (ULTIMA definizione) + 4 colonne
--    media in coda (CREATE OR REPLACE compatibile: si aggiungono solo colonne
--    finali, l'ordine delle esistenti resta invariato — agentic_thinking_policy
--    resta dov'e').
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
       cap.tool_result_max_lines,
       COALESCE(c.agentic_thinking_policy, 'none') AS agentic_thinking_policy,
       COALESCE(c.supports_image_gen, false) AS image_gen,
       COALESCE(c.supports_audio_in, false)  AS audio_in,
       COALESCE(c.supports_audio_out, false) AS audio_out,
       COALESCE(c.supports_video_gen, false) AS video_gen
FROM nexus_provider_capabilities cap
LEFT JOIN ai_price_catalog c
       ON c.provider = cap.provider AND c.model = cap.model;

-- 3. Backfill una-tantum per nome (stesse regex di classify_media_kind in
--    model_catalog_sync.rs, regola L: il punto unico Rust e questo seed
--    condividono le STESSE regole). SOLO righe non curate a mano: rispetta
--    gli override admin (capability_source IS DISTINCT FROM 'manual').
UPDATE ai_price_catalog
SET supports_image_gen = true, updated_at = NOW()
WHERE capability_source IS DISTINCT FROM 'manual'
  AND model ~* '(dall-?e|imagen|gpt-image|-image$|-image-|nano-banana)';

UPDATE ai_price_catalog
SET supports_audio_in = true, updated_at = NOW()
WHERE capability_source IS DISTINCT FROM 'manual'
  AND model ~* '(whisper|-transcribe|transcribe-|voxtral)';

UPDATE ai_price_catalog
SET supports_audio_out = true, updated_at = NOW()
WHERE capability_source IS DISTINCT FROM 'manual'
  AND model ~* '(-tts$|tts-|-tts-)';

UPDATE ai_price_catalog
SET supports_video_gen = true, updated_at = NOW()
WHERE capability_source IS DISTINCT FROM 'manual'
  AND model ~* '(veo|sora)';

-- 4. Indici parziali per le selezioni per-capability media (analoghi a
--    idx_ai_price_catalog_* esistenti). Solo righe enabled (le sole instradabili).
CREATE INDEX IF NOT EXISTS idx_ai_price_catalog_image_gen
    ON ai_price_catalog (provider, model)
    WHERE supports_image_gen = true AND is_enabled = true;
CREATE INDEX IF NOT EXISTS idx_ai_price_catalog_audio_in
    ON ai_price_catalog (provider, model)
    WHERE supports_audio_in = true AND is_enabled = true;
CREATE INDEX IF NOT EXISTS idx_ai_price_catalog_audio_out
    ON ai_price_catalog (provider, model)
    WHERE supports_audio_out = true AND is_enabled = true;
CREATE INDEX IF NOT EXISTS idx_ai_price_catalog_video_gen
    ON ai_price_catalog (provider, model)
    WHERE supports_video_gen = true AND is_enabled = true;

-- 5. Purpose interno per la generazione immagini (tier-aware, regola G: il
--    purpose seleziona per CAPABILITY, non per model_id statico). Solo image_gen
--    per ora; audio/video seguiranno coi rispettivi endpoint. requires_tool_use
--    = false: un image-gen non e' un modello agentico.
--    provider/model_id sono NOT NULL nello schema (mig 0102): col tier valorizzato
--    il resolver tier-only li IGNORA (best_model_for_tier sceglie dal catalog per
--    tier+capability), restano come placeholder dello schema. Niente model_id di
--    business hardcoded (regola G): il valore reale lo sceglie il routing per
--    capability='image_gen' dal catalog.
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
VALUES ('generate_image', '__tier_routed__', '__tier_routed__', 'light', 'image_gen', false,
        'PR6a media: risolto tier-only per capability image_gen (mig 0478); provider/model_id placeholder NOT NULL')
ON CONFLICT (purpose) DO UPDATE
    SET tier                = EXCLUDED.tier,
        required_capability = EXCLUDED.required_capability,
        requires_tool_use   = EXCLUDED.requires_tool_use,
        notes               = EXCLUDED.notes,
        updated_at          = NOW();
