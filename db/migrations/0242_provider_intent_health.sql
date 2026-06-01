-- 0242_provider_intent_health.sql
-- M7 (provider abstraction) — Q-value per (provider, model, intent_subkind).
--
-- Tiene il conteggio successi/fallimenti/soft-failure per combinazione
-- provider+model+intent, usato da brain/router/service.py::decide_model per
-- escludere candidati con failure_rate alto, e aggiornato da
-- brain/providers/registry.py::_record_usage dopo ogni chiamata.
-- Tabella di runtime (nessun dato seed): si popola in esercizio.
-- Ricostruzione fedele allo schema applicato in produzione. Idempotente.

CREATE TABLE IF NOT EXISTS nexus_provider_intent_health (
    provider           text   NOT NULL,
    model              text   NOT NULL,
    intent_subkind     text   NOT NULL,
    success_count      bigint NOT NULL DEFAULT 0,
    failure_count      bigint NOT NULL DEFAULT 0,
    soft_failure_count bigint NOT NULL DEFAULT 0,
    last_seen_at       timestamptz NOT NULL DEFAULT now(),
    last_success_at    timestamptz,
    last_failure_at    timestamptz,
    cooldown_until     timestamptz,
    cooldown_reason    text,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT nexus_provider_intent_health_pkey PRIMARY KEY (provider, model, intent_subkind)
);

CREATE INDEX IF NOT EXISTS idx_provider_intent_health_cooldown
    ON nexus_provider_intent_health USING btree (cooldown_until)
    WHERE (cooldown_until IS NOT NULL);
CREATE INDEX IF NOT EXISTS idx_provider_intent_health_visits
    ON nexus_provider_intent_health USING btree (((success_count + failure_count)) DESC);

-- I settings di degradazione routing (routing.degradation.*) sono in
-- 0245_settings_plan_remainder.sql (dump fedele dal DB di produzione).
