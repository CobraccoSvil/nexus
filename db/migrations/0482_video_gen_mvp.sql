-- 0482_video_gen_mvp.sql  (PR6e — text-to-video video-gen MVP, Google Veo)
--
-- CONTESTO: PR6a (mig 0478) ha aggiunto la capability supports_video_gen + il
-- backfill per-nome (regex veo|sora). PR6e aggiunge al gateway l'endpoint
-- POST /v1/videos con backend REALE ASYNC:
--   - Vertex Veo -> POST :predictLongRunning -> operation -> poll GET {op} ->
--     video (bytesBase64Encoded inline oppure gcsUri). Il poll-loop e'
--     incapsulato DENTRO il gateway (richiesta/risposta sincrona per il client),
--     con timeout DB-driven (questo setting). Modelli veo-3.x-generate-001.
-- e il tool nexus_generate_video (prompt -> file MP4 salvato nel progetto).
--
-- DIFFERENZA CHIAVE rispetto a image/audio: Veo e' long-running. Il LIMITE del
-- poll-loop e' il setting media.video.poll_timeout_s (regola H: niente attesa
-- infinita; al timeout il gateway risponde con errore esplicito).
--
-- PROBLEMA (stesso schema di 0479 image-gen / 0481 tts): nessun modello video-gen
-- e' is_enabled=true, quindi best_model_for_tier(tier=light, capability=
-- 'video_gen') NON ha candidati e il purpose 'generate_video' non risolve. I Veo
-- non-preview nel catalog sono gia' performance_tier='light' (al 2026-06),
-- coincidente col tier del purpose: il riallineamento e' una no-op difensiva.
--
-- SOLUZIONE (fix definitivo, regola H — non una toppa, stesso pattern di 0479/0481):
--   1. Setting media.video.poll_timeout_s (default '300'): LIMITE del poll-loop
--      video lato gateway. Letto da nexus-gateway (provider Google) e dal client
--      del tool (nexus-agent-tools) per dimensionare il timeout HTTP >= poll-loop.
--   2. Crea il purpose interno 'generate_video' (tier=light,
--      required_capability=video_gen, requires_tool_use=false): non e' un modello
--      agentico. Tier-routed: provider/model_id placeholder (NOT NULL nello schema
--      mig 0102), il valore reale lo sceglie il routing per capability='video_gen'
--      dal catalog (regola G: niente model_id di business hardcoded).
--   3. Abilita SOLO i modelli Veo REALMENTE chiamabili dall'endpoint di PR6e
--      (Vertex :predictLongRunning). Verificati esistenti nel catalog (model IN).
--   4. Allinea il loro performance_tier a 'light' cosi' coincide col tier del
--      purpose (no-op se gia' light). Marca capability_source='manual': cosi' il
--      catalog_sync NON tocca piu' ne' supports_video_gen ne' performance_tier ne'
--      is_enabled di queste righe (il riallineamento del sync agisce SOLO su
--      capability_source='auto'). La scelta sopravvive a sync/deploy/wipe +
--      re-apply migrazioni (regola H punto 2/5).
--
-- ESCLUSIONI VOLUTE:
--   - veo-*-preview: snapshot in anteprima (performance_tier='medium'), non gli
--     alias canonici. Restano nel catalog, disabilitati, come riferimento.
--   - veo-2.0-generate-001: generazione legacy; abilitiamo la sola famiglia 3.x
--     (qualita' migliore, stesso endpoint).
--   - openai/sora-*: backend Sora NON implementato in PR6e (l'endpoint video del
--     gateway oggi parla solo il dialetto Vertex :predictLongRunning). Abilitarli
--     ora porterebbe a una chiamata che fallisce sul backend sbagliato.
--   - video-speech-transcription / video-text-detection: NON sono video-gen
--     (falsi positivi del backfill per-nome 'video', supports_video_gen=false).
--
-- NOTA SULLA SALUTE REALE: questa migrazione rende i modelli SELEZIONABILI dal
-- routing; NON garantisce che il provider risponda. L'accesso Veo sul progetto
-- Vertex potrebbe essere NEGATO (404 no-access, come i gemini-3 PRO): in tal caso
-- la chiamata fallira' ESPLICITAMENTE (HTTP 4xx dal gateway risale al tool, regola
-- H: errore onesto, niente fallback inventato). E' il comportamento atteso:
-- l'azione di abilitazione Veo e' lato GCP, non nel codice.
--
-- Idempotente.

-- 1. Setting del LIMITE del poll-loop video (regola G: DB unica fonte; regola H:
--    timeout esplicito, niente attesa infinita). 300s = 5 minuti, generoso per le
--    generazioni Veo (decine di secondi a qualche minuto).
INSERT INTO settings (key, value, category, description) VALUES (
    'media.video.poll_timeout_s',
    '300',
    'media',
    'Timeout (secondi) del poll-loop video-gen lato gateway (Veo :predictLongRunning -> poll GET operation). Oltre questo tempo il poll si interrompe con errore esplicito (regola H: niente attesa infinita). Letto da nexus-gateway (provider Google) e dal client del tool nexus_generate_video per dimensionare il timeout HTTP >= poll-loop. Cache 60s lato Rust, niente env var (regola G).'
) ON CONFLICT (key) DO NOTHING;

-- 2. Purpose interno per la generazione video (tier-aware, regola G: il purpose
--    seleziona per CAPABILITY, non per model_id statico). requires_tool_use=false:
--    un modello video-gen non e' agentico. provider/model_id placeholder (NOT NULL
--    nello schema, mig 0102): col tier valorizzato il resolver tier-only li IGNORA
--    (best_model_for_tier sceglie dal catalog per tier+capability).
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
VALUES ('generate_video', '__tier_routed__', '__tier_routed__', 'light', 'video_gen', false,
        'PR6e media: risolto tier-only per capability video_gen (mig 0482); provider/model_id placeholder NOT NULL')
ON CONFLICT (purpose) DO UPDATE
    SET tier                = EXCLUDED.tier,
        required_capability = EXCLUDED.required_capability,
        requires_tool_use   = EXCLUDED.requires_tool_use,
        notes               = EXCLUDED.notes,
        updated_at          = NOW();

-- 3+4. Vertex Veo :predictLongRunning: famiglia 3.x non-preview (generate +
--      fast + lite), riallineati a 'light' e marcati 'manual'. Solo se la riga
--      esiste (model IN: niente abilitazione di modelli inesistenti).
UPDATE ai_price_catalog
SET is_enabled         = true,
    supports_video_gen = true,
    performance_tier   = 'light',
    capability_source  = 'manual',
    updated_at         = NOW()
WHERE provider = 'google'
  AND model IN (
      'veo-3.0-generate-001',
      'veo-3.0-fast-generate-001',
      'veo-3.1-generate-001',
      'veo-3.1-fast-generate-001',
      'veo-3.1-lite-generate-001'
  );

-- Verifica (no-op informativa): conta i candidati instradabili dal purpose
-- 'generate_video' (tier=light, capability=video_gen). Deve essere >= 1.
SELECT 'video_gen_light_candidates' AS scope,
       provider,
       COUNT(*) AS count
FROM ai_price_catalog
WHERE supports_video_gen = true
  AND is_enabled = true
  AND performance_tier = 'light'
GROUP BY provider
ORDER BY provider;
