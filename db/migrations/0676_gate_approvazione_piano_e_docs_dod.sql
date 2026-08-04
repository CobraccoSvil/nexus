-- 0676_gate_approvazione_piano_e_docs_dod.sql
--
-- Due presidi del processo standard (piano approvato prima del codice;
-- documentazione nello stesso change), entrambi su meccanica gia' esistente:
--
-- 1. GATE DI APPROVAZIONE DEL PIANO (Confirm, complessita' >= soglia).
--    Le colonne nexus_agent_plans.approved_at/approved_by (mig 0148)
--    esistevano da SEMPRE e nessuna UPDATE le valorizzava: il pannello admin
--    le mostrava null. Ora il planner sospende sul canale HITL esistente con
--    la pending action `plan_approval` (voci + copertura criteri eseguibili:
--    approvazione informata) e il confirm le scrive. L'approvazione del
--    piano NON pre-approva i tool mutativi (campo di stato dedicato
--    plan_approved, review A2): in Confirm l'utente continua a vedere le
--    azioni concrete al primo batch. Vale anche per il RIUSO di un piano mai
--    approvato (review A10).
--    Acceso al seed (decisione utente del 04/08: backend e UI escono nello
--    stesso deploy locale; il Default Rust resta false per il caso DB muto).
--
-- 2. DOCS NEL DoD: campo docs_updated{updated|not_needed|missing} in
--    task_complete + criterio strutturale del final_gate che misura la
--    COERENZA claim-vs-fatti sul diff (updated senza file-doc toccato ->
--    Failed; missing -> Failed; not_needed -> pass; assente -> Inconclusive,
--    fase 1: i run in volo non vengono bocciati).
--
-- Rollback: UPDATE inverso delle due chiavi *_enabled (reversibile a caldo,
-- config riletta a ogni run). Un eventuale ri-spegnimento duraturo va
-- veicolato da migrazione dedicata (regola GAP-5).

BEGIN;

INSERT INTO settings (key, value, category, description)
VALUES
    ('orchestrator.plan_approval_gate_enabled', 'true', 'orchestrator',
     'Gate di approvazione umana del piano in Confirm: il planner sospende con pending action plan_approval prima di qualunque scrittura (complessita'' >= plan_approval_min_complexity). L''approvazione scrive nexus_agent_plans.approved_at/approved_by e NON pre-approva i tool mutativi.'),
    ('orchestrator.plan_approval_min_complexity', 'medium', 'orchestrator',
     'Soglia di complessita'' (low|medium|high) da cui il gate di approvazione del piano scatta. Vocabolario canonico TaskComplexity.'),
    ('agent.final_gate.docs_criterion_enabled', 'true', 'agent',
     'Criterio docs_updated del final_gate: coerenza fra il claim di task_complete e i file toccati (updated senza file-doc -> Failed; missing -> Failed; assente -> Inconclusive).'),
    ('agent.final_gate.docs_globs', 'README*;docs/**', 'agent',
     'Glob (separati da ;) dei file considerati documentazione per il criterio docs_updated.')
ON CONFLICT (key) DO NOTHING;

DO $$
DECLARE
    v TEXT;
BEGIN
    SELECT value INTO v FROM settings WHERE key = 'orchestrator.plan_approval_gate_enabled';
    IF v IS NULL THEN
        RAISE EXCEPTION '0676: chiave plan_approval_gate_enabled non seminata';
    END IF;
    SELECT value INTO v FROM settings WHERE key = 'agent.final_gate.docs_criterion_enabled';
    IF v IS NULL THEN
        RAISE EXCEPTION '0676: chiave docs_criterion_enabled non seminata';
    END IF;
END $$;

COMMIT;
