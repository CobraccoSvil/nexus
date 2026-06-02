-- 0251_backfill_note_answers.sql
-- Backfill una-tantum: arricchisce le note 'chat' storiche con la risposta
-- finale dell'AI del run collegato.
--
-- Contesto: fino alla mig 0250 / commit 266f9f3 le note 'chat' (richiesta
-- utente) contenevano solo la richiesta, non la risposta. Dal fix, le nuove
-- note vengono arricchite a fine run; questa migrazione allinea quelle gia'
-- esistenti cosi' contengono richiesta + risposta (utili per decisioni future).
--
-- Idempotente: salta le note che hanno gia' la sezione "## Risposta di Nexus".
-- Sicura su re-apply dopo wipe DB. Collega la nota al run sia per source_run_id
-- sia per source_message_id == run_message_id (note chat hanno source_run_id
-- NULL). DISTINCT ON sceglie il run completato piu' recente per nota.

UPDATE project_knowledge_notes n
SET body_md = n.body_md || E'\n\n---\n\n## Risposta di Nexus\n\n' || sub.final_answer,
    updated_at = NOW()
FROM (
    SELECT DISTINCT ON (n2.id)
           n2.id AS note_id,
           r.final_answer
    FROM project_knowledge_notes n2
    JOIN agent_runs r
      ON (r.id = n2.source_run_id
          OR (r.run_message_id = n2.source_message_id AND r.project_id = n2.project_id))
    WHERE r.status = 'completed'
      AND r.final_answer IS NOT NULL
      AND trim(r.final_answer) <> ''
      AND n2.kind = 'chat'
      AND position('## Risposta di Nexus' in n2.body_md) = 0
    ORDER BY n2.id, r.completed_at DESC NULLS LAST
) sub
WHERE n.id = sub.note_id
  AND position('## Risposta di Nexus' in n.body_md) = 0;
