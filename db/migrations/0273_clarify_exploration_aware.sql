-- Migrazione 0273: clarify e agenti esplorazione-aware.
--
-- Contesto (incidente reale, Beauty-Book): su un task tipo "crea il db per
-- QUESTA applicazione + backend node" l'agente CHIEDE all'utente che app sia
-- (e-commerce? blog?) invece di LEGGERE il progetto esistente e procedere da
-- solo. Il progetto contiene gia' tutto il dominio (figma_export/ con pagine
-- login/booking/confirmation, src/ con l'app React, package.json): e'
-- chiaramente un'app di prenotazioni, deducibile esplorando i file.
--
-- Causa: `clarify_or_expand_node` decide ask/skip con un LLM basandosi SOLO sul
-- messaggio utente + note RAG, senza esplorare i file del progetto, quindi
-- classifica la richiesta come generica -> mode=ask.
--
-- Strategia DB (lato codice il nodo ora costruisce un blocco CONTESTO PROGETTO
-- esplorando il workspace e lo inietta nel system prompt):
--   1. Direttiva nel prompt `agent.clarify.base`: se il CONTESTO PROGETTO indica
--      codice/design esistente, scegliere mode=skip e NON chiedere dominio/entita'.
--   2. Blocco <esplora_prima_di_chiedere> nei system prompt agente principali
--      (`system.nexus_base`, `agent.coder.base`).
--
-- Tutti gli append sono idempotenti via sentinel string (no duplicati su re-run).

DO $$
DECLARE
    -- ── 1. Sentinel + direttiva per il prompt clarify ────────────────────
    clarify_sentinel TEXT := '<!-- 0273:clarify_exploration -->';
    clarify_block TEXT := E'\n\n<!-- 0273:clarify_exploration -->\n<contesto_progetto>\nSe il system prompt include una sezione "CONTESTO PROGETTO" che indica che il\nworkspace contiene gia'' codice o un design importato (figma_export/, src/,\napp/, package.json, requirements.txt, *.csproj, go.mod, README, ecc.), allora\nil progetto e'' ESISTENTE: dominio, entita'' e stack sono DEDUCIBILI esplorando\nquei file. In questo caso:\n  - NON chiedere all''utente la natura/dominio dell''applicazione (e-commerce?\n    blog? prenotazioni?) ne'' l''elenco delle entita'': sono gia'' nei file.\n  - Scegli mode=skip: l''agente le dedurra'' esplorando il workspace.\n  - Usa mode=ask SOLO per decisioni davvero NON deducibili dal progetto e\n    irreversibili (es. quale motore DB usare se nulla nel progetto lo indica).\n</contesto_progetto>';

    -- ── 2. Sentinel + blocco autonomia-esplorazione per gli agenti ───────
    explore_sentinel TEXT := '<!-- 0273:esplora_prima_di_chiedere -->';
    explore_block TEXT := E'\n\n<!-- 0273:esplora_prima_di_chiedere -->\n<esplora_prima_di_chiedere>\nPer task su un progetto ESISTENTE, prima di agire o di chiedere qualcosa\nall''utente, ESPLORA il workspace: list_files sulla root, poi read_file mirati\nsu figma_export/, src/, app/, package.json, README e file di configurazione del\nlinguaggio. Da questi DEDUCI dominio applicativo, entita'' del modello e stack\ntecnologico.\n\nNON chiedere all''utente informazioni gia'' presenti nei file (es. "che tipo di\napp e''?", "quali entita'' servono?"): se il design/codice c''e'', la risposta e''\nnel progetto, non nell''utente. Chiedi SOLO per scelte non deducibili da nulla\nnel progetto e irreversibili.\n</esplora_prima_di_chiedere>';
BEGIN
    -- 1. Direttiva nel prompt clarify (append idempotente, NON sovrascrive
    --    il resto del contenuto eventualmente gia' modificato da altre mig).
    UPDATE nexus_prompt_templates
    SET content = content || clarify_block,
        updated_at = NOW()
    WHERE key = 'agent.clarify.base'
      AND is_active = TRUE
      AND content NOT LIKE '%' || clarify_sentinel || '%';

    -- 2a. Blocco esplorazione nel system prompt base.
    UPDATE nexus_prompt_templates
    SET content = content || explore_block,
        updated_at = NOW()
    WHERE key = 'system.nexus_base'
      AND is_active = TRUE
      AND content NOT LIKE '%' || explore_sentinel || '%';

    -- 2b. Blocco esplorazione nel prompt coder (l'agente che crea db/backend).
    UPDATE nexus_prompt_templates
    SET content = content || explore_block,
        updated_at = NOW()
    WHERE key = 'agent.coder.base'
      AND is_active = TRUE
      AND content NOT LIKE '%' || explore_sentinel || '%';

    RAISE NOTICE 'Migrazione 0273 applicata: clarify + agenti esplorazione-aware';
END
$$;
