-- FASE 2 orchestrazione (PR4): colonne di audit/reconcile per l'isolamento fisico
-- dei sub-run paralleli (git worktree effimero).
--
-- `nexus_subagent_runs` e' una tabella MIGRATA: vive nei DB-PROGETTO (set
-- db/migrations/project). Nel meta e' stata decommissionata (rename fail-fast,
-- mig 0507): l'ALTER va qui, nel set project, mai nel meta.
--
-- Colonne (nullable, popolate SOLO dai sub-run del ramo ISOLATO; NULL per i
-- sub-run del ramo sequenziale/condiviso e per lo storico):
--   - worktree_path: path del worktree effimero del sub-run (per il GC/reconcile
--     e il debug: dove il sub-run ha scritto prima dell'apply serializzato).
--   - base_commit: SHA del commit da cui il worktree e' stato staccato (per
--     replay/reconcile deterministico: mai ri-derivato da HEAD).
--
-- Idempotente (ADD COLUMN IF NOT EXISTS): ri-applicabile senza effetti.
ALTER TABLE public.nexus_subagent_runs
    ADD COLUMN IF NOT EXISTS worktree_path TEXT,
    ADD COLUMN IF NOT EXISTS base_commit   TEXT;
