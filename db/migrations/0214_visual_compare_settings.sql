-- Mig 0214 - FASE 2 "resa Figma Make": verifica visiva (nexus_visual_compare).
--
-- Contesto: la FASE 1 (mig 0210) estrae il codice React+Tailwind dal .make su
-- disco; la FASE 3 (mig 0211) istruisce l'agente a estrarre-e-adattare invece
-- di reinventare. La FASE 2 (questa) da' all'agente uno strumento di VERIFICA
-- VISIVA: il tool nexus_visual_compare (crates/mcp-core/src/agent_tools/
-- visual_compare.rs) screenshotta l'app avviata e la confronta con il design
-- di riferimento Figma (thumbnail.png del .make o immagine allegata) usando un
-- modello vision. In modalita' Continuo l'agente itera per ridurre la distanza
-- visiva.
--
-- Questa migrazione registra (tutto DB-driven, regola G di CLAUDE.md: niente
-- modelli ne' soglie hardcoded nei sorgenti):
--   1) purpose 'visual_compare' in nexus_purpose_model -> provider+modello
--      vision per il confronto a due immagini. Default Google Gemini 2.0 Flash
--      (multimodale nativo, latenza/costo bassi), allineato a vision_describe
--      (mig 0194). Modificabile via UI admin senza redeploy.
--   2) settings di tuning del tool: viewport default, attesa post-load,
--      timeout screenshot, soglia di similarita' raccomandata.
--
-- Niente fallback hardcoded lato codice: se il purpose non e' configurato
-- l'endpoint brain /vision/compare ritorna 503 e il tool restituisce un JSON
-- di errore strutturato (non panica, non blocca l'agente).
--
-- Idempotente: ON CONFLICT DO NOTHING.
--
-- Riferimenti:
--   - Tool:          crates/mcp-core/src/agent_tools/visual_compare.rs
--   - Endpoint brain: POST /vision/compare in brain/grpc_server/main.py
--   - FASE 1:        db/migrations/0210_figma_make_code_extraction.sql
--   - FASE 3:        db/migrations/0211_figma_make_strategy_directive.sql
--   - vision_describe: db/migrations/0194_vision_describe_purpose.sql

-- 1) Modello vision per il confronto visivo a due immagini (screenshot vs
--    design di riferimento). Default Google Gemini 2.0 Flash.
INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes, updated_at)
VALUES (
    'visual_compare',
    'google',
    'gemini-2.0-flash-exp',
    'Modello vision usato da nexus_visual_compare per confrontare lo screenshot dell''app costruita con il design di riferimento Figma ed elencare gli scostamenti di design attuabili.',
    now()
)
ON CONFLICT (purpose) DO NOTHING;

-- 2) Tuning del tool nexus_visual_compare. Letti via settings::get_setting
--    (cache lato chiamante). Default safe documentati nel tool, ma il DB e'
--    la fonte di verita'.
INSERT INTO settings (key, value, category, description, is_secret, updated_at)
VALUES
    (
        'agent.visual_compare.viewport_width',
        '1280',
        'agent',
        'Larghezza (px) del viewport usato da nexus_visual_compare per lo screenshot quando il parametro viewport non e'' passato. Default 1280.',
        FALSE,
        NOW()
    ),
    (
        'agent.visual_compare.viewport_height',
        '800',
        'agent',
        'Altezza (px) del viewport usato da nexus_visual_compare per lo screenshot quando il parametro viewport non e'' passato. Default 800.',
        FALSE,
        NOW()
    ),
    (
        'agent.visual_compare.wait_ms',
        '1500',
        'agent',
        'Attesa (ms) dopo il load della pagina prima dello scatto in nexus_visual_compare, per dare tempo a render/animazioni/fetch. Default 1500. Override per chiamata via parametro wait_ms.',
        FALSE,
        NOW()
    ),
    (
        'agent.visual_compare.screenshot_timeout_secs',
        '45',
        'agent',
        'Timeout (secondi) per la cattura dello screenshot via Playwright in nexus_visual_compare (launch + goto + wait + scatto). Default 45.',
        FALSE,
        NOW()
    ),
    (
        'agent.visual_compare.similarity_threshold',
        '85',
        'agent',
        'Soglia di similarita'' (0-100) raccomandata: sotto questa soglia, o in presenza di differenze severita'' alta, l''agente in modalita'' Continuo dovrebbe correggere stile/layout e ripetere nexus_visual_compare. Default 85.',
        FALSE,
        NOW()
    )
ON CONFLICT (key) DO NOTHING;
