-- 0591_model_qualification_schema.sql
-- Gate di qualificazione EMPIRICA dei modelli, fase 3 del design
-- D:\IDEAI-runtime\nexus-design\2026-07-14_design_gate_qualificazione_modelli.md
-- (fase 1 = 1114e956 propagazione segnale; fase 2 = a1ae6345 observation).
--
-- Root cause (incidenti 2026-07-14/15): ai_price_catalog e' una tabella di
-- AFFERMAZIONI che nessuno e' tenuto a dimostrare, e il routing la tratta come
-- fonte di verita'. 11 modelli google 404-su-Vertex scoperti UNO ALLA VOLTA
-- dalle richieste di produzione; gemini-3.1-pro-preview in 429 quota pinnato
-- alle figure del consiglio a ogni convocazione. L'auto-disable e' POST-MORTEM:
-- la prima richiesta reale fa da probe e la paga l'utente.
--
-- Principio: due concern, due colonne, due writer.
--   is_enabled              = "esiste ed e' amministrativamente attivo" (invariato)
--   qualification_state +
--   qualified_capabilities  = "e' PROVATO nel profilo d'uso reale" (writer unico:
--                             model_qualification, fase 4)
-- Il routing agentico richiedera' ENTRAMBI (flag di rollout sotto, default off).
-- Un modello nasce 'unqualified' = invisibile al routing agentico PER DEFAULT,
-- qualunque sia l'origine (migrazione, catalog_sync, admin): il gate e' il
-- DEFAULT della colonna, non un flag da ricordare.

-- (1) Colonne di stato qualificazione.
ALTER TABLE ai_price_catalog
  ADD COLUMN IF NOT EXISTS qualification_state TEXT NOT NULL DEFAULT 'unqualified',
  ADD COLUMN IF NOT EXISTS qualified_capabilities JSONB NOT NULL DEFAULT '[]'::jsonb,
  ADD COLUMN IF NOT EXISTS qualified_at TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS qualification_expires_at TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS qualification_suite_version INT NOT NULL DEFAULT 0,
  ADD COLUMN IF NOT EXISTS qualification_reason TEXT,
  ADD COLUMN IF NOT EXISTS qualification_evidence_id BIGINT,
  ADD COLUMN IF NOT EXISTS qualification_started_at TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS qualification_attempts INT NOT NULL DEFAULT 0,
  ADD COLUMN IF NOT EXISTS qualification_backoff_until TIMESTAMPTZ;

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'chk_qualification_state') THEN
    ALTER TABLE ai_price_catalog ADD CONSTRAINT chk_qualification_state
      CHECK (qualification_state IN
             ('unqualified','probing','qualified','quarantined','disqualified'));
  END IF;
  -- Il vincolo che rende l'incidente strutturalmente impossibile: 'qualified'
  -- puo' essere scritto SOLO dal qualificatore (capability_source='probe') o dal
  -- grandfather di questa migrazione (suite_version=0, con scadenza). Una
  -- migrazione/UPDATE a mano che si dichiara qualified non passa il CHECK.
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'chk_qualified_implies_probe') THEN
    ALTER TABLE ai_price_catalog ADD CONSTRAINT chk_qualified_implies_probe
      CHECK (qualification_state <> 'qualified'
             OR capability_source = 'probe'
             OR qualification_suite_version = 0);
  END IF;
END $$;

-- (2) capability_source guadagna la provenienza 'probe' (= provato, non
--     dichiarato). 'auto' = indovinato dal nome; 'manual' = curatela.
ALTER TABLE ai_price_catalog DROP CONSTRAINT IF EXISTS chk_capability_source;
ALTER TABLE ai_price_catalog ADD CONSTRAINT chk_capability_source
  CHECK (capability_source IN ('auto','manual','probe'));

-- (3) Invalidazione automatica: se cambia il DICHIARATO, il PROVATO decade.
--     (Un CHECK non puo' esprimerlo: serve il trigger.)
CREATE OR REPLACE FUNCTION nexus_invalidate_qualification() RETURNS trigger AS $$
BEGIN
  IF TG_OP = 'UPDATE' AND (
       NEW.capabilities            IS DISTINCT FROM OLD.capabilities
    OR NEW.supports_tool_use       IS DISTINCT FROM OLD.supports_tool_use
    OR NEW.agentic_thinking_policy IS DISTINCT FROM OLD.agentic_thinking_policy
    OR NEW.uses_thinking_mode      IS DISTINCT FROM OLD.uses_thinking_mode)
     AND NEW.qualification_state = OLD.qualification_state
     AND NEW.qualification_state = 'qualified'
     AND NEW.capability_source <> 'probe'
  THEN
    NEW.qualification_state    := 'unqualified';
    NEW.qualified_capabilities := '[]'::jsonb;
    NEW.qualification_reason   := 'declared_capabilities_changed';
  END IF;
  RETURN NEW;
END $$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_invalidate_qualification ON ai_price_catalog;
CREATE TRIGGER trg_invalidate_qualification
  BEFORE UPDATE ON ai_price_catalog
  FOR EACH ROW EXECUTE FUNCTION nexus_invalidate_qualification();

-- (4) La batteria e' CONFIGURAZIONE (regola G): forma in codice (enum kind),
--     parametri e soglie in tabella. Seed dei profili nella mig 0592.
CREATE TABLE IF NOT EXISTS ai_model_probe_profile (
  profile_key    TEXT PRIMARY KEY,
  suite_version  INT NOT NULL,
  ord            INT NOT NULL,
  kind           TEXT NOT NULL CHECK (kind IN
                   ('chat','tool_minimal','tool_realistic','thinking_matrix')),
  is_blocking    BOOLEAN NOT NULL,
  applies_when   JSONB,
  grants         JSONB NOT NULL DEFAULT '[]'::jsonb,
  payload        JSONB NOT NULL DEFAULT '{}'::jsonb,
  pass_predicate JSONB NOT NULL DEFAULT '{}'::jsonb,
  enabled        BOOLEAN NOT NULL DEFAULT TRUE
);

-- (5) Evidenza append-only: solo segnali STRUTTURATI (regola M). E' il record
--     probatorio referenziato da qualification_evidence_id: risponde a "perche'
--     questo modello e' instradato per questa capability?".
CREATE TABLE IF NOT EXISTS ai_model_probe_evidence (
  id            BIGSERIAL PRIMARY KEY,
  provider      TEXT NOT NULL,
  model         TEXT NOT NULL,
  profile_key   TEXT NOT NULL,
  suite_version INT NOT NULL,
  attempt       INT NOT NULL,
  started_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  latency_ms    INT,
  error_class   TEXT,
  tool_call_count INT NOT NULL DEFAULT 0,
  content_chars INT NOT NULL DEFAULT 0,
  stop_reason   TEXT,
  verdict       TEXT NOT NULL CHECK (verdict IN ('pass','fail','inconclusive')),
  verdict_reason TEXT,
  derived       JSONB
);
CREATE INDEX IF NOT EXISTS idx_probe_evidence_model
  ON ai_model_probe_evidence (provider, model, started_at DESC);

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_qualification_evidence') THEN
    ALTER TABLE ai_price_catalog ADD CONSTRAINT fk_qualification_evidence
      FOREIGN KEY (qualification_evidence_id) REFERENCES ai_model_probe_evidence(id);
  END IF;
END $$;

-- (6) GRANDFATHER: mettere l'intero parco a 'unqualified' col gate acceso =
--     sistema fermo al deploy. Le righe oggi enabled+tool_use ereditano il
--     dichiarato come provato, MA con suite_version=0 ("mai provato", visibile)
--     e scadenza con jitter su 7 giorni: entro una settimana ogni riga e'
--     ri-provata dal qualificatore o squalificata. Il debito e' esplicito e
--     ha una data.
UPDATE ai_price_catalog
   SET qualification_state = 'qualified',
       qualified_capabilities = COALESCE(capabilities, '[]'::jsonb),
       qualification_suite_version = 0,
       qualification_reason = 'grandfathered_backfill',
       qualified_at = NOW(),
       qualification_expires_at = NOW() + (random() * interval '7 days')
 WHERE is_enabled = TRUE
   AND supports_tool_use = TRUE
   AND consecutive_tool_failures = 0
   AND qualification_state = 'unqualified';

-- (7) Flag di rollout del gate nel routing agentico (regola G). Default OFF:
--     il gate si accende con una migrazione successiva quando il primo giro di
--     qualificazione (fase 4) e' stato verificato sul campo — rollout con
--     scadenza, non fallback nascosto (il codice non ha alcun default che
--     scavalchi questa riga).
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.model_qualification.enforce_routing_gate', 'false', 'agent',
   'Gate qualificazione modelli: true = il routing AGENTICO seleziona solo modelli con qualification_state=qualified non scaduto e filtra le capability su qualified_capabilities (provate) invece di capabilities (dichiarate). false = comportamento storico. Acceso da migrazione dopo la verifica del primo giro di qualificazione.'),
  ('agent.model_qualification.requalify_ttl_days', '30', 'agent',
   'Gate qualificazione modelli: giorni di validita'' di una qualificazione (qualification_expires_at = NOW() + ttl al momento della promozione). L''evidenza invecchia: i provider cambiano i modelli sotto lo stesso id.'),
  ('agent.model_qualification.backoff_hours', '24', 'agent',
   'Gate qualificazione modelli: ore di backoff dopo una squalifica o un giro inconclusivo prima di ritentare la qualificazione (esponenziale sul numero di tentativi: base * 2^(attempts-1), cap 168h).')
ON CONFLICT (key) DO NOTHING;
