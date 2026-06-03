-- 0260_routing_agentic_intents_off_mistral_small.sql
--
-- La routing matrix e' una cascade: per ogni (intent, behavior_mode) ci sono
-- piu' righe (una per provider) ordinate per `priority` crescente; il primario
-- e' la riga attiva con priority piu' bassa. mistral-small-latest era a
-- priority 100 (pari a google/openai) e veniva scelto come primario su molti
-- intent agentici. Verificato dal vivo: su questi task con tool-calling Mistral
-- risponde HTTP 200 ma non avanza (Step completati: 0 per 30s+), l'agente sembra
-- "morto" finche' il fallback non passa a un modello capace.
--
-- Fix (regola G/H, niente UPDATE ad-hoc fuori migrazione):
--   - google/gemini-2.5-flash -> priority 85 (primario) sugli intent agentici:
--     esegue i tool, e' veloce/economico, context 1M, risponde in italiano.
--   - mistral/mistral-small-latest -> priority 200 (ultimo fallback): resta nella
--     cascade ma non viene piu' scelto come primario.
-- manual_override=true per impedire al routing_matrix_auto_promoter di ripromuovere
-- automaticamente questi modelli annullando il fix. Idempotente.

UPDATE nexus_routing_matrix
SET priority = 85,
    manual_override = true,
    updated_at = now()
WHERE provider = 'google'
  AND model_id = 'gemini-2.5-flash'
  AND intent IN ('file_ops', 'fix_complesso', 'fix_semplice', 'refactor', 'test');

UPDATE nexus_routing_matrix
SET priority = 200,
    manual_override = true,
    updated_at = now()
WHERE provider = 'mistral'
  AND model_id = 'mistral-small-latest'
  AND intent IN ('file_ops', 'fix_complesso', 'fix_semplice', 'refactor', 'test');
