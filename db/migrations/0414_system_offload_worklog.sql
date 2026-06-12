-- 0414_system_offload_worklog.sql
--
-- Fix di integrazione del budget del system_text: i blocchi <session_worklog>
-- (mig 0411) e <learned_instructions> (mig 0412) venivano APPESI al system_text
-- senza essere nella lista delle sezioni offloadabili. Sotto pressione di budget
-- (system_text > soglia token) l'offload estraeva solo examples/reflection/
-- knowledge_base_progetto, quindi worklog/learned restavano inline e il taglio
-- head d'emergenza decapitava le DIRETTIVE operative critiche.
--
-- Soluzione (regola G, config nel DB): rende <session_worklog> offloadabile come
-- ULTIMA RISORSA (in fondo alla lista = priorita' piu' bassa). E' sicuro perche'
-- il worklog resta recuperabile via il tool dedicato nexus_get_worklog (il dato
-- e' nel DB, non serve ri-embedderlo). <learned_instructions> NON e' incluso:
-- e' piccolo e senza tool di recupero garantito, resta sempre inline.
--
-- L'ORDINE del CSV e' la priorita' di offload (da sinistra si offloada per
-- primo); l'estrazione e' budget-aware (1-a-1, si ferma appena rientra sotto
-- soglia), vedi brain/agents/nodes/helpers.py::_offload_system_prompt_if_huge.
-- Idempotente. Il setting esiste gia' nel DB col vecchio default: l'UPDATE
-- aggiunge session_worklog SOLO se il valore e' ancora il vecchio default (non
-- tocca eventuali personalizzazioni admin); l'INSERT copre il caso in cui non
-- esista. Cosi' il fix vale comunque ma rispetta la config esistente.

BEGIN;

UPDATE settings
SET value = 'examples,reflection,knowledge_base_progetto,session_worklog'
WHERE key = 'agent.context.system_offload_sections'
  AND value = 'examples,reflection,knowledge_base_progetto';

INSERT INTO settings (key, value, category, description) VALUES (
    'agent.context.system_offload_sections',
    'examples,reflection,knowledge_base_progetto,session_worklog',
    'agent',
    'CSV ORDINATO (priorita'' di offload, da sinistra) delle sezioni del system_text estraibili in Qdrant sotto pressione di budget token. Le DIRETTIVE operative e <learned_instructions> restano SEMPRE inline; <session_worklog> e'' offloadabile come ultima risorsa (recuperabile via il tool nexus_get_worklog).'
)
ON CONFLICT (key) DO NOTHING;

COMMIT;
