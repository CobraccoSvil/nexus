-- 0593_seed_probe_battery.sql
-- Seed della BATTERIA di qualificazione (suite_version=1) + settings del giro.
-- Fase 4 del gate (schema: mig 0591; motore: model_qualification.rs).
--
-- La batteria e' CONFIGURAZIONE (regola G): la forma dei profili e' codice
-- (enum kind), parametri e soglie stanno qui. Ordine per `ord`: dal piu'
-- economico al piu' discriminante, early-stop al primo blocking fallito.
--
-- P0 chat_smoke     raggiungibilita': 1 chiamata mini. grants ["chat"].
-- P1 tool_smoke     il probe storico (tool fittizio), DECLASSATO a prerequisito:
--                   e' esattamente il test che dava il falso positivo
--                   sull'incidente glm -> da solo NON concede nulla (grants []).
-- P2 agentic_real   il DISCRIMINANTE: 8 schemi tool REALI (catalogo statico),
--                   system prompt REALE (nexus_prompt_templates), ~20KB di
--                   history, 3 ripetizioni, promozione 3/3. E' il carico che ha
--                   ucciso le figure del consiglio, trasformato in prova
--                   d'ingresso.
--
-- Idempotente: ON CONFLICT DO NOTHING (i profili si aggiornano con migrazioni
-- successive che alzano suite_version, forzando la ri-qualificazione del parco).

INSERT INTO ai_model_probe_profile
  (profile_key, suite_version, ord, kind, is_blocking, applies_when, grants, payload, pass_predicate, enabled)
VALUES
  ('chat_smoke', 1, 10, 'chat', TRUE, NULL,
   '["chat"]',
   '{"max_tokens": 64, "timeout_s": 45, "repeat": 1}',
   '{"min_content_chars": 1}',
   TRUE),
  ('tool_smoke', 1, 20, 'tool_minimal', TRUE, NULL,
   '[]',
   '{"max_tokens": 512, "timeout_s": 60, "repeat": 1}',
   '{"min_tool_calls": 1}',
   TRUE),
  ('agentic_real', 1, 30, 'tool_realistic', TRUE, NULL,
   '["chat", "code"]',
   '{"tool_names": ["read_file", "read_file_lines", "list_files", "search_in_files", "write_file", "edit_file", "run_command", "advisory_verdict"], "system_template_key": "system.nexus_base", "history_chars": 20000, "max_tokens": 4096, "timeout_s": 90, "repeat": 3}',
   '{"min_tool_calls": 1, "max_latency_ms": 60000, "promote_min_passes": 3, "hold_min_passes": 2}',
   TRUE)
ON CONFLICT (profile_key) DO NOTHING;

INSERT INTO settings (key, value, category, description) VALUES
  ('agent.model_qualification.round_enabled', 'true', 'agent',
   'Gate qualificazione modelli: true = il worker model_health_probe esegue la fase 0 di qualificazione a ogni giro (candidati: unqualified/scaduti/quarantined fuori backoff, solo righe enabled+tool_use). false = nessuna qualificazione automatica.'),
  ('agent.model_qualification.max_models_per_round', '4', 'agent',
   'Gate qualificazione modelli: massimo numero di modelli qualificati per giro del worker (cap di costo: ~5 chiamate reali a modello con la suite v1). Il resto attende i giri successivi.')
ON CONFLICT (key) DO NOTHING;
