-- 0346_guideline_alignment.sql
--
-- Meccanismo "agente di allineamento direttive di prompt engineering".
-- Introduce: una knowledge base VERSIONATA e APPROVATA dall'admin delle best
-- practice (cookbook/doc ufficiale Anthropic + regole interne sezione D), gli
-- esiti di conformita' dei template prompt, e le proposte di revisione per i
-- prompt protetti dalla SAFELIST (system.*/automation.*).
--
-- Loop: direttive(DB) -> conformance check (brain POST /agent/prompt-revise)
--       -> A/B per agent.* (riusa prompt_ab_experiments, mig 0092)
--       -> proposta admin per system.*/automation.* (mai auto-applicata).
--
-- Riuso: nexus_agent_reflections (0090) per il loop reflection; formato
-- dimensions/issues allineato a prompt_eval_runs.metrics (0093); selezione
-- modello tier-only (0344) via nexus_purpose_model.
--
-- Idempotente: CREATE TABLE IF NOT EXISTS / ON CONFLICT.

-- ── Fonti esterne monitorate (popolata dalla Fase 3, non nell'MVP) ──────────
CREATE TABLE IF NOT EXISTS nexus_guideline_source (
    id            UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    url           TEXT        NOT NULL UNIQUE,
    source_type   TEXT        NOT NULL CHECK (source_type IN ('official_docs','cookbook')),
    title         TEXT        NOT NULL,
    is_active     BOOLEAN     NOT NULL DEFAULT TRUE,
    last_fetched  TIMESTAMPTZ,
    last_revision TEXT,
    last_status   TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
COMMENT ON TABLE nexus_guideline_source IS
'Fonti esterne monitorate per le direttive di prompt engineering (doc ufficiale, cookbook). Popolata dalla Fase 3 (GuidelineSyncWorker).';

-- ── Knowledge base versionata delle direttive (approvata dall'admin) ────────
CREATE TABLE IF NOT EXISTS nexus_prompt_guideline (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    practice_key    TEXT        NOT NULL,
    source          TEXT        NOT NULL CHECK (source IN ('official_docs','cookbook','internal_rule')),
    source_id       UUID        REFERENCES nexus_guideline_source(id) ON DELETE SET NULL,
    source_url      TEXT,
    source_revision TEXT,
    description     TEXT        NOT NULL,
    check_hint      TEXT        NOT NULL,
    severity        TEXT        NOT NULL DEFAULT 'should' CHECK (severity IN ('must','should','nice')),
    applies_to      TEXT        NOT NULL DEFAULT 'all'    CHECK (applies_to IN ('all','agent','system','automation')),
    version         INT         NOT NULL DEFAULT 1,
    is_active       BOOLEAN     NOT NULL DEFAULT FALSE,
    approved_by     TEXT,
    approved_at     TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (practice_key, version)
);
COMMENT ON TABLE nexus_prompt_guideline IS
'Direttive di prompt engineering strutturate e versionate. is_active=TRUE solo dopo approvazione admin (approved_by valorizzato). check_hint e'' l''istruzione operativa passata al valutatore LLM.';
CREATE INDEX IF NOT EXISTS idx_nexus_prompt_guideline_active
    ON nexus_prompt_guideline (is_active, applies_to) WHERE is_active = TRUE;
CREATE INDEX IF NOT EXISTS idx_nexus_prompt_guideline_pending
    ON nexus_prompt_guideline (practice_key) WHERE approved_by IS NULL;

-- ── Esiti conformance per (template, versione). Log append-only ─────────────
CREATE TABLE IF NOT EXISTS nexus_prompt_conformance (
    id                  BIGSERIAL    PRIMARY KEY,
    prompt_key          TEXT         NOT NULL,
    prompt_version      INT          NOT NULL,
    content_hash        TEXT         NOT NULL,
    guideline_set_hash  TEXT         NOT NULL,
    overall_score       NUMERIC(4,3) NOT NULL,
    dimensions          JSONB        NOT NULL DEFAULT '{}'::jsonb,
    issues              JSONB        NOT NULL DEFAULT '[]'::jsonb,
    checked_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    UNIQUE (prompt_key, prompt_version, content_hash, guideline_set_hash)
);
COMMENT ON TABLE nexus_prompt_conformance IS
'Esito del conformance check di un template alle guideline attive. content_hash+guideline_set_hash abilitano il dirty-check anti-costo (si rivaluta solo se cambia il template o l''insieme di guideline). Formato dimensions/issues allineato a prompt_eval_runs.metrics (mig 0093).';
CREATE INDEX IF NOT EXISTS idx_nexus_prompt_conformance_key
    ON nexus_prompt_conformance (prompt_key, prompt_version, checked_at DESC);

-- ── Proposte di revisione per i prompt SAFELIST (system.*/automation.*) ─────
CREATE TABLE IF NOT EXISTS nexus_alignment_proposal (
    id               UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    prompt_key       TEXT        NOT NULL,
    baseline_version INT         NOT NULL,
    proposed_content TEXT        NOT NULL,
    rationale        TEXT,
    trigger_source   TEXT        NOT NULL CHECK (trigger_source IN ('guideline','reflection')),
    conformance_id   BIGINT      REFERENCES nexus_prompt_conformance(id) ON DELETE SET NULL,
    status           TEXT        NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending','accepted','rejected','superseded')),
    reviewed_by      TEXT,
    reviewed_at      TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- evita piu' proposte pending duplicate per la stessa baseline
    UNIQUE (prompt_key, baseline_version, status)
);
COMMENT ON TABLE nexus_alignment_proposal IS
'Proposte di revisione per prompt protetti dalla SAFELIST (system.*/automation.*): mai auto-applicate, richiedono approvazione admin.';
CREATE INDEX IF NOT EXISTS idx_nexus_alignment_proposal_pending
    ON nexus_alignment_proposal (prompt_key) WHERE status = 'pending';

-- ── Purpose tier-only per i task LLM del meccanismo ─────────────────────────
-- provider/model_id sono morti a runtime (selezione tier-only via
-- best_model_for_tier, vedi 0344 + resolve_purpose_model_db) ma la colonna e'
-- ancora NOT NULL: si valorizzano con placeholder coerenti gia' presenti nel
-- registry. Il tier e' la sola fonte di verita' della scelta modello.
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, requires_tool_use, notes) VALUES
  ('prompt_conformance_check', 'anthropic', 'claude-haiku-4-5-20251001', 'heavy', false,
   'Valuta+rivede un prompt vs guideline (brain /agent/prompt-revise). Tier-only: provider/model_id placeholder ignorati a runtime. Mig 0346.'),
  ('guideline_extract', 'anthropic', 'claude-haiku-4-5-20251001', 'medium', false,
   'Estrae direttive strutturate da doc/cookbook (Fase 3). Tier-only: provider/model_id placeholder ignorati a runtime. Mig 0346.')
ON CONFLICT (purpose) DO UPDATE SET
  tier = EXCLUDED.tier,
  requires_tool_use = EXCLUDED.requires_tool_use,
  notes = EXCLUDED.notes,
  updated_at = NOW();

-- ── Settings del meccanismo (stile mig 0092 optimizer) ──────────────────────
INSERT INTO settings (key, value, category, description, is_secret) VALUES
  ('alignment_enabled', 'false', 'alignment',
   'Kill switch GuidelineAlignmentWorker. Default false: il worker non fa nulla finche non abilitato.', FALSE),
  ('alignment_conformance_threshold', '0.75', 'alignment',
   'Soglia overall_score sotto cui un template e'' candidato a revisione.', FALSE),
  ('alignment_check_interval_hours', '24', 'alignment',
   'Intervallo minimo tra due conformance check dello stesso template (throttling interno al worker, lo scheduler tick e'' globale a 1800s).', FALSE),
  ('alignment_max_checks_per_tick', '20', 'alignment',
   'Numero massimo di conformance check per esecuzione del worker (controllo costo LLM).', FALSE),
  ('alignment_autovariant_enabled', 'false', 'alignment',
   'Se true, per i prompt agent.* sotto soglia genera variante + esperimento A/B. Se false, solo valutazione (evaluate-only). Default false.', FALSE),
  ('alignment_sync_enabled', 'false', 'alignment',
   'Kill switch GuidelineSyncWorker (fetch doc/cookbook, Fase 3). Default false.', FALSE)
ON CONFLICT (key) DO NOTHING;
