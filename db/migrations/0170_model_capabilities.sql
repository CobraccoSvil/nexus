-- M170: Capability per modello (es. extended thinking) come JSONB su ai_price_catalog.
--
-- Motivazione: CLAUDE.md §G richiede che la registry DB sia l'unica fonte di
-- verita' per modelli/capabilities. Prima di questa migrazione il set di
-- modelli con capability "thinking" era hardcoded come `THINKING_MODELS`
-- in `brain/providers/anthropic_provider.py` (linee 310, 533) — violazione
-- §G. Spostiamo l'informazione su ai_price_catalog dove gia' vivono i metadati
-- di costo e abilitazione.
--
-- Schema: colonna `capabilities JSONB NOT NULL DEFAULT '{}'::jsonb`.
-- Convenzione chiavi:
--   - "thinking": bool — il modello supporta extended thinking (beta).
--   - "tools":    bool — il modello supporta tool use / function calling.
--   - "vision":   bool — il modello accetta input immagini.
-- Estendibile senza ALTER TABLE.
--
-- Idempotente: ADD COLUMN IF NOT EXISTS + ON CONFLICT-free UPDATE via merge JSONB.

ALTER TABLE ai_price_catalog
    ADD COLUMN IF NOT EXISTS capabilities JSONB NOT NULL DEFAULT '{}'::jsonb;

-- Popola capability 'thinking' = true per i modelli Anthropic che supportano
-- il beta interleaved-thinking-2025-05-14 (Sonnet/Opus 4.5 e 4.6, Opus 4.7).
-- Source: docs Anthropic + listino effettivamente abilitato in prod (mig 0143).
UPDATE ai_price_catalog
SET capabilities = capabilities || '{"thinking": true}'::jsonb
WHERE provider = 'anthropic'
  AND model IN (
      'claude-sonnet-4-5',
      'claude-sonnet-4-6',
      'claude-opus-4-5',
      'claude-opus-4-6',
      'claude-opus-4-7'
  );

-- Index parziale per lookup "modelli con capability X" (uso futuro).
CREATE INDEX IF NOT EXISTS idx_ai_price_catalog_thinking
    ON ai_price_catalog(provider, model)
    WHERE (capabilities ->> 'thinking')::boolean IS TRUE
      AND is_enabled = TRUE;
