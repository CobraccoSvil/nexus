-- 0255_provider_health_cooldown.sql
-- Auto-disable provider su billing_error/quota (regola H): persiste il cooldown
-- billing in DB invece che solo in-memory nel brain.
--
-- Problema: il cooldown era un dict Python in-memory (_provider_cooldown_until),
-- perso a ogni restart del brain e non condiviso tra processi/servizi. Cosi'
-- dopo un restart il sistema ricominciava a chiamare Anthropic/OpenAI (senza
-- credito) come primari, sprecando un tentativo fallito per ogni turno.
--
-- Con questa tabella il cooldown sopravvive ai restart ed e' leggibile anche da
-- mcp-core per escludere i provider dalla scelta del primario.
--   - billing_cooldown_until: istante fino al quale il provider e' escluso.
--   - last_error: ultima causa (es. 'billing_error', 'quota').
-- Ripristino automatico: il brain azzera la riga al primo 200 del provider.
-- Idempotente.

CREATE TABLE IF NOT EXISTS nexus_provider_health (
    provider               text PRIMARY KEY,
    billing_cooldown_until timestamptz,
    last_error             text,
    updated_at             timestamptz NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_provider_health_cooldown
    ON nexus_provider_health (billing_cooldown_until)
    WHERE billing_cooldown_until IS NOT NULL;
