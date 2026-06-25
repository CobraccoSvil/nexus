-- 0452_turn_focus_directive.sql
-- Anti-contaminazione history (regola L + H): la "turn focus directive" ancora
-- ogni turno all'ultima richiesta utente, declassando la cronologia a contesto
-- di supporto. Causa radice: con una history grande su un task (osservato: chat
-- da 1M token su bookingService.ts), i modelli small seguono il peso del
-- contesto storico invece dell'ultima istruzione ("crea index.html" ignorato).
-- Il continuity gate semantico (mig 0397, cosine MiniLM locale) NON basta: due
-- task di sviluppo sullo stesso progetto sono lessicalmente simili, lo score
-- resta sopra soglia e non trimma. La directive testuale e' la rete di sicurezza
-- robusta, indipendente dalla similarita', sempre attiva di default.
--
-- Letto da brain/agents/nodes/helpers.py (_load_continuity_config,
-- get_bool_setting) e applicato in executor_node + planner_node tramite il punto
-- unico build_turn_focus_directive / _inject_turn_focus. Il default True vive nel
-- codice solo come rete di sicurezza per DB down (get_bool_setting non solleva).
-- Disattivabile a runtime (cache 60s) senza redeploy. Idempotente.
INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.context.turn_focus_enabled',
    'true',
    'agent',
    'Anti-contaminazione history: se true, inietta nel system_text dell''executor e del planner la turn focus directive che ancora l''ultima richiesta utente come obiettivo prioritario e declassa la cronologia a contesto di supporto. Risolve il caso in cui una history grande su un task precedente trascina l''agente sul vecchio argomento (il continuity gate cosine non scatta su task lessicalmente simili). Punto unico build_turn_focus_directive (helpers.py). Cache 60s.'
)
ON CONFLICT (key) DO NOTHING;
