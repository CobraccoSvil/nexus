-- 0336_routing_matrix_code_read.sql
--
-- L'intent `code_read` (ispezione/lettura read-only del progetto) era gia'
-- presente in ALLOWED_INTENTS (brain/router/intents.py) e nel classifier
-- keyword/embedding (brain/router/service.py), ma NON aveva alcuna riga in
-- nexus_routing_matrix. Conseguenza (regola L violata: intent senza routing):
-- route_model_with_mode (crates/mcp-core/src/orchestrator/model_routing.rs)
-- non mappava `code_read` e lo faceva cadere nel ramo default chat_breve/
-- media/lunga -> modello "lite" conversazionale, incapace di eseguire in modo
-- affidabile le tool call di lettura. Effetto: domande di ispezione sul
-- progetto ("perche' ci sono due index.html?") ricevevano risposte generiche
-- su progetti ipotetici invece dell'analisi dei file reali.
--
-- Questa migrazione popola intent='code_read' EREDITANDO la configurazione
-- cascade corrente di `debug` (intent agentico analysis-heavy, gia' allineato
-- ai modelli tool-robust non-thinking dalle mig 0268 + 0270: mistral-large-2411
-- e deepseek-v4-pro davanti, gemini-2.5-pro come ultima riserva). Si eredita
-- invece di ri-hardcodare i nomi modello (che 0270/0274 hanno gia' corretto:
-- l'alias mistral-large-latest risolve a labs 403) per non reintrodurre un
-- modello rotto e per restare allineati a future curature degli intent agentici.
--
-- Contesto codice: model_routing.rs ora mappa "code_read" => "code_read"
-- (intent_key dedicato), quindi il lookup (code_read, behavior_mode) trova
-- queste righe. Cascade preservata (priority/is_active copiate da debug).
-- Idempotente: ON CONFLICT (intent, behavior_mode, provider).

BEGIN;

INSERT INTO nexus_routing_matrix
    (intent, behavior_mode, provider, model_id, priority, is_active, manual_override, notes)
SELECT
    'code_read', behavior_mode, provider, model_id, priority, is_active, manual_override,
    '0335: code_read eredita la config tool-robust di debug (vedi mig 0268/0270)'
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
