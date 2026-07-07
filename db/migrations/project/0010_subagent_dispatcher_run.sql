-- Fase D fan-in (isolamento per-run dei figli): colonna `dispatcher_run_id` su
-- nexus_subagent_runs = il RUN CORRENTE che ha dispatchato il figlio
-- (`ctx.core.run_id`, cioe' il run sospeso su `awaiting_subagents` che il fan-in
-- deve riprendere). DISTINTA da `parent_run_id` (che porta l'ANCHOR depth-chain =
-- COALESCE(parent_run_id, session_id) del ctx e degenera in session_id, essendo
-- ctx.core.parent_run_id sempre None): l'anchor raggruppa la FAMIGLIA per
-- depth/cost e resta invariato; il dispatcher isola i figli DIRETTI di un singolo
-- run.
--
-- MOTIVAZIONE (ALTA 1): COUNT/fetch/backstop del fan-in raggruppavano su
-- parent_run_id = anchor = session_id -> vedevano TUTTI i sub-run della sessione
-- (inclusi nipoti dispatchati da un altro figlio annidato ancora vivo). Con
-- l'annidamento (P dispatcha Cp1,Cp2; Cp1 dispatcha Cs1) il COUNT di P contava
-- Cs1 e il fetch di P iniettava Cs1 (nipote mai dispatchato da P). Raggruppando
-- per dispatcher_run_id la COUNT/fetch di P vedono SOLO Cp1,Cp2.
--
-- `nexus_subagent_runs` e' una tabella MIGRATA: vive nei DB-PROGETTO (set
-- db/migrations/project). Nel meta e' stata decommissionata (rename fail-fast,
-- mig 0507): l'ALTER va qui, nel set project, mai nel meta.
--
-- Backfill NON necessario: feature nuova (il background fan-in e' opt-in, nessuna
-- riga storica ha figli background). NULL = sub-run pre-feature o non-background:
-- il fan-in filtra comunque su is_background = true.
--
-- Idempotente (ADD COLUMN / CREATE INDEX IF NOT EXISTS): ri-applicabile.
ALTER TABLE public.nexus_subagent_runs
    ADD COLUMN IF NOT EXISTS dispatcher_run_id UUID;

-- Indice parziale per la COUNT del fan-in (figli DIRETTI ancora attivi di un
-- dato dispatcher): scandisce solo le righe non-terminali dei dispatch background.
CREATE INDEX IF NOT EXISTS idx_subagent_runs_dispatcher_active
    ON public.nexus_subagent_runs (dispatcher_run_id, is_background)
    WHERE status IN ('running', 'paused');
