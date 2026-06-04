-- Migrazione 0288 — Pre-flight check librerie chromium-headless-shell.
--
-- Aggiunge il toggle `agent.testing.preflight_check_enabled` (default true).
-- Quando true, run_playwright_tests esegue `ldd` sul binary di Playwright
-- prima di spawnare il processo. Se ci sono librerie "not found" (libnspr4,
-- libnss3, libasound, ...) ritorna un errore esplicito all'agente con il
-- comando `sudo apt-get install` per il fix.
--
-- Trigger: incident chat 6 Beauty-Book run 7b8f7da3 — chromium-headless-shell
-- non puo' avviarsi (4 librerie sistema mancanti) ma `npx playwright test`
-- tentava 13×3 launch falliti in 1ms ciascuno; il backend Rust restava
-- bloccato in child.wait() per 7 minuti senza progresso. Il pre-flight
-- evita il job zombie e da' all'utente l'errore vero invece di "in corso..."
-- indefinitamente.

INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('agent.testing.preflight_check_enabled', 'true', 'agent',
     'Quando true, run_playwright_tests esegue ldd sul binary chromium-headless-shell prima di spawnare playwright. Se rileva librerie sistema not found, ritorna errore esplicito con istruzioni di fix invece di lasciare il browser fallire in loop. Default true.',
     NOW())
ON CONFLICT (key) DO NOTHING;
