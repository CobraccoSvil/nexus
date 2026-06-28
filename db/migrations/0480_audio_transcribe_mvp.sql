-- 0480_audio_transcribe_mvp.sql  (PR6c — audio transcription speech-to-text MVP)
--
-- CONTESTO: PR6a (mig 0478) ha aggiunto la capability supports_audio_in + il
-- backfill per-nome (whisper/-transcribe/voxtral). PR6c aggiunge al gateway
-- l'endpoint POST /v1/audio/transcriptions con backend REALE:
--   - OpenAI -> POST /audio/transcriptions  (multipart: file + model + response_format)
--     modelli whisper-1, gpt-4o-transcribe, gpt-4o-mini-transcribe
-- e il tool nexus_transcribe_audio (allegato audio -> testo).
--
-- PROBLEMA (stesso schema di 0479 per image-gen): nessun modello audio-in e'
-- is_enabled=true, quindi best_model_for_tier(tier=..., capability='audio_in')
-- NON ha candidati e il purpose 'transcribe_audio' non risolve. Inoltre alcuni
-- modelli transcribe sono performance_tier='medium', mentre il purpose qui e'
-- tier='light': il resolver tier-only NON-agentico interroga ESATTAMENTE il tier
-- 'light' (best_model_for_tier non degrada il tier per i purpose non-agentici),
-- quindi un modello 'medium' non verrebbe mai scelto.
--   Tier reali nel catalog (al 2026-06): openai/gpt-4o-mini-transcribe='light'
--   (gia' allineato), openai/whisper-1='medium', openai/gpt-4o-transcribe='medium'
--   (richiedono riallineamento a 'light').
--
-- SOLUZIONE (fix definitivo, regola H — non una toppa, stesso pattern di 0479):
--   1. Crea il purpose interno 'transcribe_audio' (tier=light,
--      required_capability=audio_in, requires_tool_use=false): non e' un modello
--      agentico. Tier-routed: provider/model_id placeholder (NOT NULL nello schema
--      mig 0102), il valore reale lo sceglie il routing per capability='audio_in'
--      dal catalog (regola G: niente model_id di business hardcoded).
--   2. Abilita SOLO i modelli audio-in OpenAI REALMENTE chiamabili dall'endpoint
--      di PR6c (POST /audio/transcriptions). Verificati esistenti nel catalog.
--   3. Allinea il loro performance_tier a 'light' cosi' coincide col tier del
--      purpose. Senza questo allineamento il purpose light non li selezionerebbe.
--   4. Marca capability_source='manual': cosi' il catalog_sync NON tocca piu'
--      ne' supports_audio_in ne' performance_tier ne' is_enabled di queste righe
--      (il riallineamento del sync agisce SOLO su capability_source='auto'). La
--      scelta sopravvive a sync/deploy/wipe + re-apply migrazioni (regola H 2/5).
--
-- ESCLUSIONI VOLUTE:
--   - mistral/voxtral-*: trascrivono via API Mistral con un dialetto diverso;
--     l'endpoint di PR6c implementa SOLO il dialetto OpenAI /audio/transcriptions
--     (il provider Mistral del gateway non sovrascrive supports_audio_in, default
--     false -> non instradabile). Abilitarli ora porterebbe a una chiamata sul
--     dialetto sbagliato. Restano come alternativa quando il provider Mistral
--     implementera' transcribe_audio.
--   - gpt-4o-transcribe-diarize / gpt-realtime-whisper / *-realtime-*: varianti
--     speciali (diarizzazione / realtime streaming) non coperte dall'endpoint
--     batch /audio/transcriptions di PR6c.
--
-- NOTA SULLA SALUTE REALE: questa migrazione rende i modelli SELEZIONABILI dal
-- routing; NON garantisce che il provider risponda. Se la API key OpenAI non ha
-- accesso a quel modello, la chiamata fallira' ESPLICITAMENTE (HTTP 4xx/5xx dal
-- gateway risale al tool, regola H: errore onesto, niente fallback inventato).
--
-- Idempotente.

-- 1. Purpose interno per la trascrizione audio (tier-aware, regola G: il purpose
--    seleziona per CAPABILITY, non per model_id statico). requires_tool_use=false:
--    un modello transcribe non e' agentico. provider/model_id placeholder (NOT
--    NULL nello schema, mig 0102): col tier valorizzato il resolver tier-only li
--    IGNORA (best_model_for_tier sceglie dal catalog per tier+capability).
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
VALUES ('transcribe_audio', '__tier_routed__', '__tier_routed__', 'light', 'audio_in', false,
        'PR6c media: risolto tier-only per capability audio_in (mig 0480); provider/model_id placeholder NOT NULL')
ON CONFLICT (purpose) DO UPDATE
    SET tier                = EXCLUDED.tier,
        required_capability = EXCLUDED.required_capability,
        requires_tool_use   = EXCLUDED.requires_tool_use,
        notes               = EXCLUDED.notes,
        updated_at          = NOW();

-- 2+3+4. OpenAI /audio/transcriptions: gpt-4o-mini-transcribe (gia' tier light,
--        il piu' economico) + whisper-1 + gpt-4o-transcribe (riallineati a light).
--        Solo se la riga esiste (model IN: niente abilitazione di modelli inesistenti).
UPDATE ai_price_catalog
SET is_enabled        = true,
    supports_audio_in = true,
    performance_tier  = 'light',
    capability_source = 'manual',
    updated_at        = NOW()
WHERE provider = 'openai'
  AND model IN ('gpt-4o-mini-transcribe', 'whisper-1', 'gpt-4o-transcribe');

-- Verifica (no-op informativa): conta i candidati instradabili dal purpose
-- 'transcribe_audio' (tier=light, capability=audio_in). Deve essere >= 1.
SELECT 'audio_in_light_candidates' AS scope,
       provider,
       COUNT(*) AS count
FROM ai_price_catalog
WHERE supports_audio_in = true
  AND is_enabled = true
  AND performance_tier = 'light'
GROUP BY provider
ORDER BY provider;
