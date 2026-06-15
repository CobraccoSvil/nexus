-- 0435_run_command_truncation_max_chars.sql
-- Fix loop "ripeti npm run build" (2026-06-15): l'output combinato di
-- `run_command` veniva troncato in modo DISTRUTTIVO tenendo i PRIMI 8000
-- caratteri (hardcoded) e scartando la CODA. I build tsc/cargo/npm elencano
-- gli errori in ordine col totale "Found N errors" IN FONDO: l'agente vedeva
-- i primi errori, perdeva la coda + il totale, correggeva alcuni file e
-- ri-eseguiva il build per "vedere gli altri" (loop razionale ma sterile).
--
-- mcp-core ora usa troncamento testa+coda non distruttivo (stesso punto unico
-- di run_tests, regola L) con cap DB-driven (regola G).
--
-- Setting:
--   agent.command.run_command_max_chars
--     Cap (caratteri) dell'output combinato esposto dall'esecuzione di
--     run_command. Default 16000: deliberatamente >= del cap del brain
--     (tool_result_max_chars, vista 0318/mig 0240, default 6000) cosi' mcp-core
--     NON e' mai il primo collo di bottiglia che decapita la coda con gli
--     ultimi errori prima che l'output arrivi al brain. Configurabile
--     dall'admin senza redeploy; nessuna cache (letto una volta per comando).
--
-- Idempotente.
INSERT INTO settings (key, value) VALUES
  ('agent.command.run_command_max_chars', '16000')
ON CONFLICT (key) DO NOTHING;
