-- 0245_test_informed_settings.sql
--
-- M13.6 del piano "Impact analysis": test-informed generation.
--
-- In fase di pianificazione il planner (brain/agents/planner_node.py) deve
-- essere CONSAPEVOLE dell'impact set del task: i file a rischio regressione e i
-- test esistenti che li coprono. brain/agents/impact_brief.py interroga
-- l'endpoint mcp-core /api/internal/impact/tests-for-run (M13.4) usando come
-- seed i path citati nel messaggio utente, e inietta un blocco <impact_brief>
-- nel contesto del planner. Il blocco e' SOLO informativo: guida il planner LLM
-- a generare todo di aggiornamento/creazione test + un todo finale di verifica
-- non-regressione, cosi' a fine run il regression gate (M13.4/5) trova test da
-- eseguire.
--
-- Gate: se impact.test_informed_enabled = false il planner e' invariato.
-- L'endpoint tests-for-run resta inoltre gated da `impact.enabled` (mig 0242).
--
-- Niente nuova tabella: soli settings di controllo.
-- Idempotente: ON CONFLICT (key) DO NOTHING (stile 0240).

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('impact.test_informed_enabled', 'true', 'impact', 'Abilita il blocco <impact_brief> nel planner (M13.6): il planner vede impact set e test esistenti e genera todo di test/verifica mirati.', FALSE),
    ('impact.test_informed_max_seed_paths', '12', 'impact', 'Numero massimo di seed path (file citati dall utente) inviati a tests-for-run in fase di planning.', FALSE),
    ('impact.test_informed_max_listed_tests', '15', 'impact', 'Numero massimo di test esistenti elencati nel blocco <impact_brief> (anti-rumore nel prompt del planner).', FALSE)
ON CONFLICT (key) DO NOTHING;
