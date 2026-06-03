-- Bug #34: mcp-core non terminava dopo SIGTERM (worker tokio detached o blocco
-- nel drop del runtime tenevano vivo il processo, port 4000 occupata 1+ minuto).
-- Fix: watchdog su std::thread che chiama std::process::exit(0) dopo N secondi,
-- piu' process::exit(0) esplicito dopo il graceful drain di axum.
-- Questo setting controlla N (timeout del watchdog di force-exit), regola G:
-- nessun valore hardcoded nascosto, la fonte di verita' e' il DB.

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('shutdown.force_exit_timeout_seconds', '10', 'runtime',
     'Secondi massimi che mcp-core attende dopo aver ricevuto SIGTERM/Ctrl-C prima di forzare std::process::exit(0) via watchdog su thread OS dedicato. Garantisce che il processo (e il bind su :4000) venga sempre rilasciato anche se un worker detached non risponde a cancellation. Default 10. La unit systemd ha TimeoutStopSec come ulteriore rete (SIGKILL).',
     FALSE)
ON CONFLICT (key) DO NOTHING;
