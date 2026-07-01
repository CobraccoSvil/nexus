-- 0500_routing_retry_loop_settings.sql
-- Fix routing/anti-loop (piano A+B). Seed dei default DB-driven (regola G:
-- niente fallback hardcoded nella business logic; il codice ha solo la rete di
-- sicurezza costante se il DB e' irraggiungibile).
--
-- FASE B1 (retry stesso modello, strict pin): quando il provider e' pinnato
-- (scelta utente o routing gia' risolto) NON viene mai sostituito da un altro.
-- Su errore transitorio (429/5xx/timeout) si ritenta lo STESSO modello con
-- backoff esponenziale+jitter; su cooldown transitorio BREVE si attende il
-- residuo invece di fallire subito. Elimina il sintomo
-- "tutti i provider hanno fallito -> google (in cooldown, 21s rimanenti)".
--   - gateway.retry.max_attempts             tentativi totali sullo stesso modello
--   - gateway.retry.base_delay_ms            ritardo base del backoff (ms)
--   - gateway.retry.max_backoff_ms           tetto del backoff (ms)
--   - gateway.retry.wait_short_cooldown_cap_s attesa max di un cooldown breve (s)
--
-- FASE 4 (governor anti-loop): una LETTURA idempotente ripetuta (read_file/
-- list_files/grep) non e' uno stallo "produttivo" come un build che fallisce.
-- Soglia dedicata piu' alta cosi' una rilettura accidentale non fa scattare
-- subito la macchina GUIDE->ABORT su un modello capace.
--   - agent.repeated_action_threshold.read_only  soglia ripetizione read-only
--
-- Idempotente: i valori restano se gia' presenti (l'admin puo' sovrascriverli;
-- ricarica a caldo TtlCache 60s lato gateway / cache settings lato executor).

INSERT INTO settings (key, value) VALUES
  ('gateway.retry.max_attempts', '3'),
  ('gateway.retry.base_delay_ms', '500'),
  ('gateway.retry.max_backoff_ms', '8000'),
  ('gateway.retry.wait_short_cooldown_cap_s', '45'),
  ('agent.repeated_action_threshold.read_only', '4')
ON CONFLICT (key) DO NOTHING;
