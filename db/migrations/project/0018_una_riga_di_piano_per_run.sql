-- 0018_una_riga_di_piano_per_run.sql
-- Il PIANO di un run e' uno STATO, non una cronologia: una sola riga
-- `nexus_agent_meta_steps` con kind='plan' per run_id.
--
-- Causa radice (corretta nel codice insieme a questa migrazione: punto unico
-- crates/nexus-agent-tools/src/meta_piano.rs, a cui delegano i DUE produttori —
-- crates/nexus-agent-tools/src/todos.rs::persisti_meta_piano e
-- crates/mcp-core/src/agent_graph_adapter/meta_step_store.rs).
--
-- A rispondere alla domanda «qual e' il piano di questo run?» erano due
-- produttori con due discipline diverse:
--   - il tool `nexus_todo_write`, che applicava gia' "una riga per run"
--     (UPDATE ... WHERE run_id AND kind='plan', INSERT solo se l'UPDATE non
--     tocca nulla);
--   - il nodo planner, che passa dalla porta generica `MetaStepStore`, la cui
--     impl e' una INSERT cieca — corretta per ogni altro kind, che e'
--     append-only per natura (subagent_progress, routing, escalation...).
-- L'UPDATE del tool girava per primo e non trovava nulla da aggiornare; subito
-- dopo il planner inseriva la SECONDA riga. Nessuna delle due discipline era
-- sbagliata da sola: mancava il punto unico.
--
-- MISURATO il 10/08/2026 sul progetto batteria-todo-deepseek, run
-- 92a6c7f2-5f2b-4b96-a786-70f166289e9c:
--   kind='plan'  title='Piano — 8 step'         created_at 15:21:04.513664
--   kind='plan'  title='Piano creato — 8 step'  created_at 15:21:04.516001
-- Stesso array di todo: stessi 8 id, stessi stati. Non due versioni successive
-- del piano — due copie a 2,3 ms di distanza. Nel DOM della chat: 2 blocchi
-- "PIANO" identici, resi dallo STESSO nastro attivita' (il nastro non
-- duplicava: rendeva fedelmente due eventi che il backend gli mandava).
--
-- I due produttori portano meta' informazione ciascuno — il tool lo STATO dei
-- todo (`todos`, `n`), il planner la PROVENIENZA (`plan_id`, `provider`,
-- `model`, `active_todo_id`) — quindi lo storico si FONDE invece di scegliere
-- un vincitore: nessun campo si perde.
--
-- L'invariante finisce nello SCHEMA e non solo nel codice: una regola applicata
-- dai soli produttori vale finche' tutti la conoscono, e questo difetto e' nato
-- proprio da un produttore che non la conosceva.

-- 1) Fusione dei payload dei duplicati nella riga piu' recente.
--    jsonb_object_agg con chiavi ripetute tiene l'ULTIMA aggregata: ordinando
--    dalla piu' vecchia alla piu' recente, la piu' recente vince sulle chiavi in
--    comune e le altre sopravvivono dove tace.
WITH duplicati AS (
    SELECT run_id
    FROM public.nexus_agent_meta_steps
    WHERE kind = 'plan'
    GROUP BY run_id
    HAVING count(*) > 1
),
tenuta AS (
    SELECT DISTINCT ON (m.run_id) m.id, m.run_id
    FROM public.nexus_agent_meta_steps m
    JOIN duplicati d ON d.run_id = m.run_id
    WHERE m.kind = 'plan'
    ORDER BY m.run_id, m.created_at DESC, m.id DESC
),
fuso AS (
    SELECT m.run_id,
           jsonb_object_agg(kv.key, kv.value ORDER BY m.created_at ASC, m.id ASC) AS payload
    FROM public.nexus_agent_meta_steps m
    JOIN duplicati d ON d.run_id = m.run_id
    CROSS JOIN LATERAL jsonb_each(m.payload) AS kv
    WHERE m.kind = 'plan'
    GROUP BY m.run_id
)
UPDATE public.nexus_agent_meta_steps m
SET payload = f.payload
FROM tenuta t
JOIN fuso f ON f.run_id = t.run_id
WHERE m.id = t.id;

-- 2) Via i doppioni: resta la piu' recente, che ora porta anche i campi delle
--    altre.
DELETE FROM public.nexus_agent_meta_steps m
WHERE m.kind = 'plan'
  AND EXISTS (
      SELECT 1
      FROM public.nexus_agent_meta_steps piu_recente
      WHERE piu_recente.kind = 'plan'
        AND piu_recente.run_id = m.run_id
        AND (piu_recente.created_at, piu_recente.id) > (m.created_at, m.id)
  );

-- 3) Campi DERIVATI allineati al criterio del punto unico: `n` e' la lunghezza
--    di `todos` e il titolo si compone da `n` (regola Q: il testo dai campi).
--    Prima i due produttori dichiaravano due titoli diversi per lo stesso piano.
UPDATE public.nexus_agent_meta_steps
SET payload = payload || jsonb_build_object('n', jsonb_array_length(payload -> 'todos')),
    title = 'Piano — ' || jsonb_array_length(payload -> 'todos') || ' step'
WHERE kind = 'plan'
  AND jsonb_typeof(payload -> 'todos') = 'array';

-- 4) L'invariante. Parziale: gli altri kind restano append-only.
CREATE UNIQUE INDEX IF NOT EXISTS uq_nexus_agent_meta_steps_piano_per_run
    ON public.nexus_agent_meta_steps (run_id)
    WHERE kind = 'plan';
