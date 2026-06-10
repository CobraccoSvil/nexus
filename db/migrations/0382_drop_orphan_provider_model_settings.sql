-- 0382_drop_orphan_provider_model_settings.sql
--
-- Cleanup dei settings ORFANI provider_model_* e default_model (regola G/H/L).
--
-- Dopo il consolidamento del selettore modello (ADR 0030, fix fallback __no_model__)
-- RoutingConfig::from_settings NON legge piu' questi settings: il default-per-provider
-- ha come fonte UNICA la tabella nexus_provider_default_model (mig 0101). Le chiavi
-- restavano nel DB come dati orfani e venivano RI-SCRITTE dal pannello admin Routing AI
-- (apps/web-ide/components/settings/routing-config); il controllo UI "modello per
-- provider" e' stato rimosso contestualmente perche' scriveva config inerte (non letta
-- dal routing). Ora le righe vanno eliminate per chiudere il giro (niente config morta).
--
-- LISTA ESPLICITA (NON un LIKE): si eliminano SOLO le chiavi del controllo provider-model
-- del pannello routing. Restano intatti gli ALTRI settings con suffisso _model, che hanno
-- consumatori reali e propri (reflection_model, supervisor_model, google_batch_model,
-- embedding_model, routing.classifier_model, agent.context.rolling_summary_model).
--
-- Idempotente: se una chiave non esiste, la DELETE non fa nulla.

DELETE FROM settings
 WHERE key IN (
     'default_model',
     'provider_model_anthropic',
     'provider_model_openai',
     'provider_model_google',
     'provider_model_deepseek',
     'provider_model_mistral'
 );
