-- 0542: coda durevole di resume per il fan-in deterministico dei sub-run
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

-- Cadenza di poll della coda (regola G): il worker fan-in la legge da qui
-- (fanin_worker::load_u64, default 4s, minimo 2s). Senza questo seed il worker
-- usava il fallback hardcoded del codice: lo esplicitiamo nel DB.
INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.background_fanin_poll_seconds', '4', 'orchestrator',
   'Fase D: intervallo (secondi) tra i giri di drenaggio della coda subagent_fanin_resume_queue. Minimo effettivo 2s (guard nel worker). DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;

-- Cadenza del BACKSTOP (regola G/H): recupero periodico dei padri
-- awaiting_subagents mai accodati (figlio detached crashato/panicato, timeout DB
-- su mark_run, o restart di mcp-core). Scansiona TUTTI i progetti -> piu' rado del
-- poll della coda (default 60s, minimo 10s).
INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.background_fanin_backstop_seconds', '60', 'orchestrator',
   'Fase D: intervallo (secondi) tra le scansioni del backstop fan-in (recupera i padri awaiting_subagents mai accodati). Minimo effettivo 10s. DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;

-- Timeout (regola G/H) oltre il quale una sub-run BACKGROUND ancora running/paused
-- e' considerata ORFANA (figlio detached morto senza mark_run) e marcata 'timeout'
-- dal backstop, cosi' la COUNT del fan-in puo' scendere a 0 e il padre riprendere.
-- Generoso (default 15 min) per non uccidere sub-run legittimi a lunga durata.
INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.background_fanin_orphan_timeout_seconds', '900', 'orchestrator',
   'Fase D: eta (secondi) oltre la quale una sub-run background rimasta running e considerata orfana e marcata timeout dal backstop fan-in. Minimo effettivo 60s. DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
