-- 0132: settings per disambiguation step (L2) e study mode tool gating (L3)
--
-- Regola G di CLAUDE.md: niente costanti hardcoded nel codice per parametri
-- di routing/automation. Queste chiavi diventano la fonte autoritativa per:
--   - soglie di ambiguity detection (Rasa/Dialogflow/LUIS best practice)
--   - whitelist tool read-only per modalita' "study"
--   - timeout heartbeat SSE brain → mcp-core (sezione 10 piano async)
--
-- Tutte le chiavi sono lette via cache 60s (RoutingThresholds in Rust,
-- _load_classifier_config in Python). Default tecnici nel codice sono
-- usati solo come ricovero parziale se la singola chiave manca.

BEGIN;

-- ─────────────────────────────────────────────────────────────────────
-- L2: soglie di disambiguazione classifier
-- ─────────────────────────────────────────────────────────────────────
INSERT INTO settings (key, value, category, description) VALUES
  ('routing.ambiguity_min_confidence', '0.70',
   'routing',
   'Top intent confidence sotto questa soglia → richiesta disambiguazione all''utente (NLU best practice). Range [0.0, 1.0].'),
  ('routing.ambiguity_min_margin', '0.15',
   'routing',
   'Margine (top_confidence − second_candidate_confidence) sotto questa soglia → disambiguazione. Range [0.0, 0.5].')
ON CONFLICT (key) DO NOTHING;

-- ─────────────────────────────────────────────────────────────────────
-- L3 + Heartbeat SSE: timeout silenzio stream brain
-- ─────────────────────────────────────────────────────────────────────
INSERT INTO settings (key, value, category, description) VALUES
  ('routing.sse_heartbeat_max_silence_secs', '120',
   'routing',
   'Secondi di silenzio sullo stream SSE brain→mcp-core prima di considerare il run bloccato. Il brain emette ping ogni 30s, quindi valore tipico 90-180.')
ON CONFLICT (key) DO NOTHING;

-- ─────────────────────────────────────────────────────────────────────
-- L3: whitelist tool read-only per modalita' "study"
-- ─────────────────────────────────────────────────────────────────────
-- Formato: CSV di nomi tool. Solo tool sicuri (lettura, search, status).
-- Aggiungere un tool qui significa permettere all'agente di chiamarlo
-- anche quando l'utente ha selezionato "Studio" (no scritture, no comandi).
INSERT INTO settings (key, value, category, description) VALUES
  ('automation.study_mode_readonly_tools',
   'read_file,read_file_lines,list_files,search_in_files,search_codebase_semantic,get_project_structure,get_file_diff,git_status,git_log,git_diff,list_services,read_service_output,nexus_mcp_tool_search,list_profiles,get_profile',
   'automation',
   'CSV di tool esposti all''agente in automation_mode=study (gating difensivo: anche se l''LLM ignora il prompt di mode, NON puo'' chiamare tool fuori da questa whitelist).')
ON CONFLICT (key) DO NOTHING;

COMMIT;
