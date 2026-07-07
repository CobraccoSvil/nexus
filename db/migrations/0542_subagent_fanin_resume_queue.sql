-- 0541: coda durevole di resume per il fan-in deterministico dei sub-run
-- background (Fase D, Slice 3).
--
-- CONTESTO: quando il padre dispatcha figli con `background=true`, il tool
-- ritorna `background_dispatched` e il ToolDispatchNode setta
-- `awaiting_subagents` -> il motore SOSPENDE il padre (Slice 1+2). Serve poi un
-- TRIGGER che RIPRENDA il padre quando l'ultimo figlio background completa. Un
-- hook in-process sarebbe fragile (lost-wakeup se il figlio completa PRIMA che
-- il padre si sospenda; perso a un restart). La coda durevole in DB e' il punto
-- di rendez-vous race-free e restart-safe: il figlio (al completamento
-- terminale, se e' l'ULTIMO background del suo parent) accoda; un worker
-- periodico consuma con CAS `awaiting_subagents -> running` (idempotente).
--
-- La coda vive nel META (state.db): e' cross-progetto e il worker la interroga
-- una volta per ciclo senza dover iterare tutti i pool progetto. `project_id`
-- serve al worker per risolvere il pool del progetto (dove vive agent_runs) e
-- applicare il CAS; `session_id` per costruire l'input minimale del resume.
CREATE TABLE IF NOT EXISTS subagent_fanin_resume_queue (
    parent_run_id UUID PRIMARY KEY,
    project_id    UUID NOT NULL,
    session_id    UUID NOT NULL,
    enqueued_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Kill-switch del trigger fan-in background (regola G, DB-driven, niente env).
-- Default ON: il background e' opt-in (nessuna coda si popola finche' un padre
-- non dispatcha figli background), quindi ON e' INERTE finche' non c'e' lavoro.
-- OFF -> il worker non consuma la coda (le righe restano, ripartono a ON).
INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.background_fanin_enabled', 'true', 'orchestrator',
   'Fase D Slice 3: abilita il worker che riprende il run padre sospeso quando i sub-run background completano (drena subagent_fanin_resume_queue via CAS awaiting_subagents->running). Default ON, ma inerte finche un padre non dispatcha figli background. DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
