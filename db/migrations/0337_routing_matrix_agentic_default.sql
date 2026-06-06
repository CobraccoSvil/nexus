-- 0337_routing_matrix_agentic_default.sql
--
-- Intent di SISTEMA `agentic_default`: fallback neutro quando il classifier LLM
-- (`/classify-intent-agentic`) non risponde. Nella nuova architettura (solo
-- interpretazione semantica LLM, niente piu' keyword) il sistema NON degrada
-- piu' a una classificazione keyword: assegna `agentic_default`, che attiva il
-- _LAZY_MINIMAL_TOOLKIT lato agente (discovery + lettura) e deve quindi essere
-- routato su modelli tool-robust, non su un modello "lite" conversazionale.
--
-- Come per `code_read` (mig 0336), eredita la config cascade corrente di `debug`
-- (modelli tool-robust non-thinking: mistral-large-2411 + deepseek-v4-pro,
-- gemini-2.5-pro come ultima riserva), invece di hardcodare nomi modello.
--
-- Contesto codice: model_routing.rs mappa "agentic_default" => "agentic_default"
-- in intent_key_for; il lookup (agentic_default, behavior_mode) trova queste
-- righe. Idempotente: ON CONFLICT (intent, behavior_mode, provider).

BEGIN;

INSERT INTO nexus_routing_matrix
    (intent, behavior_mode, provider, model_id, priority, is_active, manual_override, notes)
SELECT
    'agentic_default', behavior_mode, provider, model_id, priority, is_active, manual_override,
    '0337: agentic_default eredita la config tool-robust di debug (vedi mig 0268/0270)'
FROM nexus_routing_matrix
WHERE intent = 'debug'
ON CONFLICT (intent, behavior_mode, provider) DO UPDATE
SET model_id = EXCLUDED.model_id,
    priority = EXCLUDED.priority,
    is_active = EXCLUDED.is_active,
    manual_override = EXCLUDED.manual_override,
    notes = EXCLUDED.notes,
    updated_at = now();

COMMIT;
