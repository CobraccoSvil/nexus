-- 0355_service_observability.sql
-- Layer di osservabilita' dei servizi delle APP UTENTE (worker periodico in
-- mcp-core: project_workspace::service_observer::spawn_service_observer).
-- Distinto dal watchdog dei microservizi Nexus (mig 0272): scope = servizi del
-- progetto (prefisso "{slug}-"), insieme disgiunto dalla lista
-- agent.watchdog.services. L'observer NON riavvia: osserva (metriche /proc,
-- health, log) e diagnostica (anomalie + crash -> agente Debugger).
--
-- Regola G: ogni soglia/flag/intervallo vive qui in settings, niente hardcoded.
-- Sicurezza: l'osservazione passiva e' attiva di default (innocua); il trigger
-- automatico dell'agente Debugger (che spende token) richiede opt-in esplicito
-- via agent.observer.auto_diagnose_enabled.
-- Idempotente.

INSERT INTO settings (key, value, category, description) VALUES
  ('agent.observer.enabled', 'true', 'agent',
   'Abilita il layer di osservabilita delle app utente (metriche /proc, health, tail log, anomalie). Solo osservazione: non riavvia servizi.'),
  ('agent.observer.interval_seconds', '15', 'agent',
   'Intervallo in secondi tra i cicli di raccolta dell observer.'),
  ('agent.observer.metrics_enabled', 'true', 'agent',
   'Abilita la raccolta metriche OS per processo (CPU/RSS/IO da /proc) ed emissione evento ServiceMetrics.'),
  ('agent.observer.latency_degraded_ms', '2000', 'agent',
   'Soglia in ms del tempo di connessione TCP oltre la quale la latenza e considerata degradata (anomalia).'),
  ('agent.observer.error_rate_max_per_min', '10', 'agent',
   'Numero massimo di righe di log error/exception al minuto oltre il quale scatta anomalia error-rate.'),
  ('agent.observer.restart_rate_max', '3', 'agent',
   'Delta NRestarts entro una finestra oltre il quale scatta anomalia restart ripetuti.'),
  ('agent.observer.restart_rate_window_s', '300', 'agent',
   'Finestra in secondi su cui valutare il rate di restart.'),
  ('agent.observer.cpu_pct_threshold', '90', 'agent',
   'Soglia CPU percentuale per processo oltre la quale scatta anomalia (CPU sostenuta).'),
  ('agent.observer.rss_bytes_threshold', '1073741824', 'agent',
   'Soglia RSS in byte (default 1GB) per processo oltre la quale scatta anomalia memoria.'),
  ('agent.observer.tail_only_with_subscribers', 'true', 'agent',
   'Se true il tail log continuo si attiva solo per progetti con almeno un client SSE connesso (anti-overhead).'),
  ('agent.observer.auto_diagnose_enabled', 'false', 'agent',
   'Kill-switch del trigger automatico dell agente Debugger su crash o anomalia grave. Default false: l auto-debug spende token e va abilitato esplicitamente.'),
  ('agent.observer.diagnose_cooldown_seconds', '600', 'agent',
   'Cooldown in secondi tra due diagnosi automatiche per lo stesso servizio e firma di errore.'),
  ('agent.observer.diagnose_max_per_hour', '5', 'agent',
   'Cap di diagnosi automatiche per progetto all ora (anti-loop di run costosi).')
ON CONFLICT (key) DO NOTHING;
