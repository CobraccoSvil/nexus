-- 0486_model_reprobe_backoff.sql
--
-- ROOT CAUSE (regola H, nodo strutturale): i modelli auto-disabilitati per un
-- QUIRK GATEWAY (tool-probe fallito -> auto_disabled_reason LIKE
-- 'tool_probe_failed:%', o 'malformed_tool_calls' dal runtime) restavano
-- disabilitati PER SEMPRE anche dopo che il quirk del gateway era stato corretto
-- in produzione. Il worker model_health_probe ri-probava SOLO i modelli con
-- is_enabled=true (load_enabled_models); un modello con is_enabled=false non
-- veniva mai piu' caricato -> mai ri-probato -> nessun re-enable. Effetto reale:
-- interi provider (mistral, google) a 0 modelli abilitati dopo un quirk gateway,
-- richiedendo reset manuali nel DB (una toppa, vietata dalla regola H).
--
-- FIX: model_health_probe::run_one_round ora, dopo il giro sui modelli enabled,
-- carica ESPLICITAMENTE i candidati disabilitati per quirk gateway
-- (load_reprobe_candidates), li ri-proba con chat-probe + tool-probe (le stesse
-- funzioni del giro principale, regola L) e li riabilita se ENTRAMBI passano.
-- Sono ESCLUSI dal re-probe i reason che NON si risolvono ri-probando subito:
-- billing/quota (cooldown, gestiti altrove), missing_from_api (modello sparito),
-- %model_selection_policy% (decisione amministrativa), e i lock manuali
-- (capability_source='manual' o reason 'manual:%').
--
-- BACKOFF DB-driven (regola G, niente hardcode): un candidato viene ri-probato
-- solo se e' passato almeno `agent.model_reprobe.backoff_minutes` dall'ultimo
-- tentativo (auto_disabled_at, aggiornato a ogni re-probe fallito). Default 30
-- min: con il loop del worker a >=5 min evita di martellare ogni giro un modello
-- ancora rotto, ma lo riabilita entro mezz'ora dalla correzione del quirk.

BEGIN;

INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('agent.model_reprobe.backoff_minutes', '30', 'agent',
     'Backoff (minuti) fra due re-probe consecutivi dello stesso modello disabilitato per quirk gateway (auto_disabled_reason tool_probe_failed:% o malformed_tool_calls). Il worker model_health_probe ri-proba il candidato solo se e'' passato almeno questo intervallo da auto_disabled_at; se chat-probe + tool-probe passano lo riabilita (is_enabled=true). Default codice se assente: 30. Niente hardcode (regola G).',
     NOW())
ON CONFLICT (key) DO NOTHING;

COMMIT;
