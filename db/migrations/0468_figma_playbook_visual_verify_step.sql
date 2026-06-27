-- 0468_figma_playbook_visual_verify_step.sql
-- P5.B: aggiunge il passo di VERIFICA VISIVA allo steps_json del playbook
-- implement.figma_make, cosi' il planner genera un TODO DETERMINISTICO per
-- nexus_visual_compare.
--
-- Causa radice (test E2E Beauty-Book): la verifica visiva col design Figma era SOLO
-- nel system prompt (blocco <visual_verification>, mig 0215) -> direttiva discorsiva
-- IGNORABILE. Gli step operativi del playbook (steps_json, mig 0395) si fermavano a
-- "verifica nel browser fino a pagina funzionante", senza confronto col design ->
-- l'agente chiudeva "completed" con un layout non conforme al figma.
--
-- Fix (complementa il gate design_verify, mig 0467 + final_gate.rs): il passo di
-- confronto visivo diventa un todo strutturato. Cosi':
--   - il planner lo decompone come step obbligatorio (steps_json -> todos);
--   - il gate final_gate design_verify blocca comunque la chiusura se l'ultimo
--     similarity_score < soglia.
-- Niente loop hardcoded nel codice (CLAUDE.md sez. D): l'iterazione resta guidata
-- dall'agente in modalita' Continuo; qui si garantisce solo che il passo venga
-- pianificato. Soglia/modello vision restano DB-driven (sez. G, mig 0214).
--
-- Idempotente: append solo se il passo non e' gia' presente.
UPDATE nexus_task_playbooks
SET steps_json = steps_json || '["VERIFICA VISIVA col design Figma: esegui nexus_visual_compare(url del frontend avviato, reference = attachment del design .make) e confronta la resa con il design. Se similarity_score e'' sotto la soglia, correggi SOLO stile/layout/spaziature/palette/tipografia/componenti per avvicinarti al design e ri-esegui nexus_visual_compare, ITERANDO finche'' la resa corrisponde al figma. NON considerare il task completo finche'' la resa visiva non e'' conforme al design."]'::jsonb,
    updated_at = now()
WHERE key = 'implement.figma_make'
  AND steps_json::text NOT LIKE '%nexus_visual_compare%';
