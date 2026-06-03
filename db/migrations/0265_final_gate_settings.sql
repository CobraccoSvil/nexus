-- 0265_final_gate_settings.sql
-- Final gate generale (fail-closed) anti-placeholder. Per i task software che
-- chiudono SENZA plan_phase (il verifier non gira) un gate minimo verifica che
-- il codice importato (design staged in figma_export/) non resti orfano: l'app
-- placeholder (hello-world) sopra un design importato deve fallire e tornare
-- all'executor per l'integrazione. Letto da brain/agents/orchestrator_config.py
-- (cache TTL 60s). Regola G: configurazione nel DB, niente hardcode. Idempotente.

INSERT INTO settings (key, value, category, description) VALUES
  ('agent.final_gate.enabled', 'true', 'agent',
   'Abilita il final gate generale fail-closed (anti-placeholder) per i task software senza plan_phase.'),
  ('agent.final_gate.software_intents', 'code,debug,scaffold,implement,build,frontend,fix,refactor', 'agent',
   'CSV degli intent considerati task software per cui il final gate si attiva.'),
  ('agent.final_gate.max_cycles', '2', 'agent',
   'Numero massimo di cicli di retry del final gate prima di chiudere comunque (no loop infinito).'),
  ('agent.import_staging_dirs', 'figma_export', 'agent',
   'CSV delle directory di staging del codice importato (design) controllate dal gate no_orphan_imported.'),
  ('agent.no_orphan.min_ratio', '0.4', 'agent',
   'Frazione minima di moduli staged che l''entry servito deve raggiungere via grafo import per superare il gate.'),
  ('agent.verifier.fail_closed', 'true', 'agent',
   'Se true il verifier_node, in assenza di acceptance_criteria sul todo software, esegue comunque i gate generali invece di marcare completed.')
ON CONFLICT (key) DO NOTHING;
