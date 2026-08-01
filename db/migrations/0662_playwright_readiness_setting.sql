-- Migrazione 0662 — Readiness del servizio bersaglio prima della suite Playwright.
--
-- Aggiunge `agent.playwright.readiness_timeout_seconds` (default 60): la
-- finestra massima in cui run_playwright_tests attende che la porta bersaglio
-- della BASE_URL risponda STABILMENTE (contratto della remediation:
-- probe_port + stable_enough, service_recovery.rs) prima di lanciare la suite.
-- Scaduta la finestra l'esito e' setup_failed con causa "servizio non pronto":
-- la suite non parte mai a freddo.
--
-- Trigger: misurato il 31/07/2026 su bacheca-attivita — 53 esecuzioni della
-- suite, 31 failed / 21 passed sulla stessa app, coi rossi concentrati nei
-- giri partiti subito dopo un riavvio del frontend (Vite "Re-optimizing
-- dependencies": la pagina risponde ma i test sensibili scadono). La catena
-- riavvio -> suite a freddo -> rosso flaky -> ciclo di correzione ha
-- fabbricato due regressioni reali (css_syntax_error, TS2322) su codice sano.
--
-- Il gate scatta solo se la porta e' legata a una unit di servizio
-- (nexus_port_allocations.service_unit): senza unit non c'e' contratto — e' il
-- caso del webServer avviato dalla suite stessa. `0` riduce il gate alla sola
-- finestra di stabilita'.

INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('agent.playwright.readiness_timeout_seconds', '60', 'agent',
     'Finestra massima (secondi) in cui run_playwright_tests attende che la porta del servizio bersaglio risponda stabilmente prima di lanciare la suite. Scaduta la finestra: setup_failed con causa "servizio non pronto", la suite non parte. Vale solo se la porta e'' legata a una unit di servizio del progetto. Default 60.',
     NOW())
ON CONFLICT (key) DO NOTHING;
