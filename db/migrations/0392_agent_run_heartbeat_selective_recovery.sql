-- 0392_agent_run_heartbeat_selective_recovery.sql
--
-- Recovery selettivo dei run interrotti (regola H: causa radice del "la chat
-- chiude sempre con 'server riavviato'").
--
-- Causa: il recovery all'avvio di mcp-core (main.rs:370) marcava 'interrupted'
-- TUTTI i run in stato 'running' o 'awaiting_confirmation', senza distinguere un
-- run VIVO (il brain lo sta ancora elaborando: il loop agentico vive nel brain,
-- mcp-core e' solo proxy SSE) da uno ORFANO. Inoltre includeva
-- 'awaiting_confirmation', che e' uno stato resumibile via checkpoint LangGraph +
-- /agent/approve: marcarlo interrupted distruggeva stato valido.
--
-- Fix: heartbeat di liveness. Il brain batte agent_runs.updated_at a ogni
-- iterazione del loop (e' l'unico che sa se il run e' vivo, sopravvive al restart
-- di mcp-core). Il recovery — sia all'avvio sia in un reaper periodico — marca
-- 'interrupted' SOLO i run 'running' fermi oltre soglia (updated_at stale),
-- lasciando in pace i run vivi e gli 'awaiting_confirmation'. Il reaper periodico
-- copre anche l'orfano da restart del SOLO brain (mcp-core non riparte, quindi il
-- recovery di startup non scatterebbe).
--
-- Soglie DB-driven (regola G: niente hardcode nel codice).
-- Idempotente.

BEGIN;

-- Heartbeat: NULL finche' il brain non batte. La query di recovery usa
-- COALESCE(updated_at, created_at), quindi un run nuovo non ancora battuto e'
-- valutato sul created_at (recente -> non stale), e un orfano vecchio pre-fix
-- e' valutato sul created_at (vecchio -> stale, verra' reapato).
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ;

-- Index parziale per il reaper (scan solo dei run attivi).
CREATE INDEX IF NOT EXISTS idx_agent_runs_running_heartbeat
    ON agent_runs (updated_at)
    WHERE status = 'running';

-- Soglia oltre la quale un run 'running' senza battito e' considerato orfano.
-- 900s (15 min) = allineata allo storico timeout del task_watchdog: generosa per
-- non uccidere run con tool lunghi (il battito si ferma durante un tool sincrono
-- lungo). Configurabile da admin. Lo sweep periodico e' fatto dal task_watchdog
-- (gia' periodico) che delega al punto unico run_reaper::reap_stale_runs; non
-- serve un intervallo dedicato.
INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.run_recovery.stale_after_seconds',
    '900',
    'agent',
    'Secondi di inattivita'' (nessun battito agent_runs.updated_at dal brain) oltre i quali un run ''running'' e'' considerato orfano e marcato ''interrupted'' dal recovery di mcp-core (all''avvio + sweep del task_watchdog). Heartbeat battuto a ogni iterazione del loop agentico. NON tocca ''awaiting_confirmation'' (resumibile via checkpoint).'
)
ON CONFLICT (key) DO NOTHING;

COMMIT;
