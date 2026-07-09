-- 0552_long_running_watcher_patterns.sql
-- FIX (causa radice) dei "residui invisibili" del worker process_resume.
--
-- process_resume (crates/mcp-core/src/process_resume.rs) risveglia l'agente quando
-- un servizio del progetto va stopped/failed, per fargli reagire ai crash. Una
-- guardia (is_long_running_service) esclude i dev-server/watcher confrontando il
-- command coi pattern di long_running_patterns (match SUBSTRING). Ma il catalogo
-- (mig 0024) copre nodemon/next dev/vite/tsc --watch/cargo watch/... e NON il
-- WATCHER NATIVO di Node `node --watch` (ne' tsx watch / deno / air): un servizio
-- come `node --watch server.js` (tipico backend dev) a ogni scrittura file si
-- riavvia -> stopped/failed transitorio -> process_resume NON lo riconosce come
-- long-running -> risveglia l'agente con un run invisibile (source='process_resume')
-- che tocca i file -> puo' loopare (il codice documenta "14 run in 18 min").
--
-- Fix: aggiungere i pattern watcher mancanti. `--watch` (flag generico) copre in un
-- colpo node/tsx/webpack/jest/... in watch mode; gli altri sono espliciti per
-- chiarezza. Match substring case-insensitive, quindi bastano le radici.
--
-- Idempotente: ON CONFLICT (pattern) DO NOTHING.

INSERT INTO long_running_patterns (pattern, description) VALUES
    ('node --watch',  'Node.js watcher nativo (node --watch <file>)'),
    ('--watch',       'Flag watcher generico (node/tsx/webpack/jest/... in watch mode)'),
    ('tsx watch',     'tsx watch mode (TypeScript exec watcher)'),
    ('deno task',     'Deno task (spesso dev/watch)'),
    ('deno run',      'Deno run (watch con --watch)'),
    ('air',           'Air live-reload per Go'),
    ('watchexec',     'watchexec: file watcher generico')
ON CONFLICT (pattern) DO NOTHING;
