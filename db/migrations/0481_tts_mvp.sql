-- 0481_tts_mvp.sql  (PR6d — text-to-speech audio-out MVP)
--
-- CONTESTO: PR6a (mig 0478) ha aggiunto la capability supports_audio_out + il
-- backfill per-nome (suffisso "-tts", prefisso "tts-", infix "-tts-"). PR6d
-- aggiunge al gateway l'endpoint POST /v1/audio/speech con backend REALE:
--   - OpenAI -> POST /audio/speech  (JSON in: model+input+voice+response_format,
--     BODY BINARIO out) modelli gpt-4o-mini-tts, tts-1, tts-1-hd
-- e il tool nexus_text_to_speech (testo -> file audio salvato nel progetto).
--
-- PROBLEMA (stesso schema di 0479 image-gen / 0480 transcribe): nessun modello
-- audio-out e' is_enabled=true, quindi best_model_for_tier(tier=..., capability=
-- 'audio_out') NON ha candidati e il purpose 'text_to_speech' non risolve. Inoltre
-- alcuni modelli TTS sono performance_tier='medium', mentre il purpose qui e'
-- tier='light': il resolver tier-only NON-agentico interroga ESATTAMENTE il tier
-- 'light' (best_model_for_tier non degrada il tier per i purpose non-agentici),
-- quindi un modello 'medium' non verrebbe mai scelto.
--   Tier reali nel catalog (al 2026-06): openai/gpt-4o-mini-tts='light' (gia'
--   allineato, il piu' economico), openai/tts-1='medium', openai/tts-1-hd='medium'
--   (richiedono riallineamento a 'light').
--
-- SOLUZIONE (fix definitivo, regola H — non una toppa, stesso pattern di 0479/0480):
--   1. Crea il purpose interno 'text_to_speech' (tier=light,
--      required_capability=audio_out, requires_tool_use=false): non e' un modello
--      agentico. Tier-routed: provider/model_id placeholder (NOT NULL nello schema
--      mig 0102), il valore reale lo sceglie il routing per capability='audio_out'
--      dal catalog (regola G: niente model_id di business hardcoded).
--   2. Abilita SOLO i modelli audio-out OpenAI REALMENTE chiamabili dall'endpoint
--      di PR6d (POST /audio/speech). Verificati esistenti nel catalog.
--   3. Allinea il loro performance_tier a 'light' cosi' coincide col tier del
--      purpose. Senza questo allineamento il purpose light non li selezionerebbe.
--   4. Marca capability_source='manual': cosi' il catalog_sync NON tocca piu'
--      ne' supports_audio_out ne' performance_tier ne' is_enabled di queste righe
--      (il riallineamento del sync agisce SOLO su capability_source='auto'). La
--      scelta sopravvive a sync/deploy/wipe + re-apply migrazioni (regola H 2/5).
--
-- ESCLUSIONI VOLUTE:
--   - gpt-4o-mini-tts-2025-* / tts-1-1106 / tts-1-hd-1106: varianti datate
--     (snapshot). Abilitiamo solo gli alias canonici non datati, coerente con la
--     scelta di 0480 (niente abilitazione di snapshot ridondanti). Restano nel
--     catalog, disabilitati, come riferimento storico.
--
-- NOTA SULLA SALUTE REALE: questa migrazione rende i modelli SELEZIONABILI dal
-- routing; NON garantisce che il provider risponda. Se la API key OpenAI non ha
-- accesso a quel modello, la chiamata fallira' ESPLICITAMENTE (HTTP 4xx/5xx dal
-- gateway risale al tool, regola H: errore onesto, niente fallback inventato).
--
-- Idempotente.

-- 1. Purpose interno per la sintesi vocale (tier-aware, regola G: il purpose
--    seleziona per CAPABILITY, non per model_id statico). requires_tool_use=false:
--    un modello TTS non e' agentico. provider/model_id placeholder (NOT NULL nello
--    schema, mig 0102): col tier valorizzato il resolver tier-only li IGNORA
--    (best_model_for_tier sceglie dal catalog per tier+capability).
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
VALUES ('text_to_speech', '__tier_routed__', '__tier_routed__', 'light', 'audio_out', false,
        'PR6d media: risolto tier-only per capability audio_out (mig 0481); provider/model_id placeholder NOT NULL')
ON CONFLICT (purpose) DO UPDATE
    SET tier                = EXCLUDED.tier,
        required_capability = EXCLUDED.required_capability,
        requires_tool_use   = EXCLUDED.requires_tool_use,
        notes               = EXCLUDED.notes,
        updated_at          = NOW();

-- 2+3+4. OpenAI /audio/speech: gpt-4o-mini-tts (gia' tier light, il piu'
--        economico) + tts-1 + tts-1-hd (riallineati a light). Solo se la riga
--        esiste (model IN: niente abilitazione di modelli inesistenti).
UPDATE ai_price_catalog
SET is_enabled         = true,
    supports_audio_out = true,
    performance_tier   = 'light',
    capability_source  = 'manual',
    updated_at         = NOW()
WHERE provider = 'openai'
  AND model IN ('gpt-4o-mini-tts', 'tts-1', 'tts-1-hd');

-- Verifica (no-op informativa): conta i candidati instradabili dal purpose
-- 'text_to_speech' (tier=light, capability=audio_out). Deve essere >= 1.
SELECT 'audio_out_light_candidates' AS scope,
       provider,
       COUNT(*) AS count
FROM ai_price_catalog
WHERE supports_audio_out = true
  AND is_enabled = true
  AND performance_tier = 'light'
GROUP BY provider
ORDER BY provider;
