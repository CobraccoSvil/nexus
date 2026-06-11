-- 0396_figma_playbook_intent_gate.sql
--
-- Gate intent sul trigger del playbook implement.figma_make.
--
-- Incidente run 5df5cef2/5ec12cad (2026-06-11): il playbook matchava su
-- QUALSIASI turno della sessione Beauty-Book — anche "quante tabelle ci sono
-- nel db" — perche' la keyword ".make" veniva trovata nel blocco di sistema
-- <allegati_sessione> (filename PL.make) prepended a ogni messaggio. La guida
-- all'estrazione figma iniettata su domande non pertinenti faceva deragliare i
-- modelli (estrazione su file allucinato "figma.png", risposta finale corrotta).
--
-- Primo strato del fix nel codice (task_playbook._user_text_only): le keywords
-- matchano solo sul testo utente PULITO. Questo secondo strato vincola il
-- playbook agli intent per cui ha senso (implementazione/scaffolding), cosi'
-- anche una domanda informativa che cita "figma" nel testo utente (es. "come
-- funziona il file figma?", intent code_read/chat) NON riceve la guida
-- operativa di estrazione-e-bootstrap.
--
-- Idempotente.

UPDATE nexus_task_playbooks
SET trigger_json = trigger_json || '{"intent": ["implement", "scaffold", "code", "frontend", "build", "architecture", "agentic_default", "file_ops"]}'::jsonb,
    updated_at = now()
WHERE key = 'implement.figma_make';
