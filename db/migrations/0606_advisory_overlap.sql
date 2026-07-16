-- 0606_advisory_overlap.sql
-- OVERLAP dei panel a monte col run + BARRIERA DI SCRITTURA — fase 5 del
-- paradigma di orchestrazione dimensionata.
--
-- Root cause: il consiglio era un pre-step BLOCCANTE. Il run principale non
-- partiva finche' tutte le figure non avevano deliberato: fino a ~300s di
-- silenzio prima che l'agente leggesse anche solo un file. Ma la RICOGNIZIONE
-- (leggere il codice, orientarsi) non ha bisogno del parere del consiglio: ha
-- bisogno del repo. Solo la SCRITTURA ha bisogno del verdetto.
--
-- Ora, a flag ON, il run parte SUBITO e i panel deliberano in parallelo; il
-- primo tool MUTATIVO attende la barriera:
--   - Released -> si scrive, coi requisiti del consiglio iniettati come
--     promemoria nello stesso turno (l'unico momento in cui servono davvero);
--   - Vetoed   -> il run si ferma PRIMA della prima modifica, riusando l'edge
--     esistente terminal_panel_veto (graph.rs) — zero routing nuovo;
--   - timeout/Unavailable -> il run PROSEGUE dichiarandolo, col promemoria che
--     NON ha un'approvazione (regola M: un'assenza di verdetto non e' un
--     verdetto favorevole). Mai un deadlock: una barriera che attende per
--     sempre sarebbe peggio del problema che risolve.
--
-- Il gate riusa il PUNTO UNICO "tool mutativo" (hitl::is_mutator_tool_name su
-- agent.tools.result_cache_mutators, mig 0394): la stessa domanda del gate HITL
-- di Conferma, quindi la stessa risposta.
--
-- `advisory_overlap_enabled` nasce 'false': a flag OFF il gate e' inerte e il
-- comportamento e' bit-identico (il run attende i panel come prima).
-- L'attivazione avviene con la mig 0607, dopo la verifica E2E.

INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.advisory_overlap_enabled', 'false', 'orchestrator',
   'Overlap dei panel a monte col run: se true il run parte SUBITO (la ricognizione read-only non attende nessuno) e i panel deliberano in parallelo; la prima SCRITTURA attende il verdetto (barriera nel ToolDispatchNode). Un veto ferma il run prima della prima modifica. false = pre-step bloccante come prima (bit-identico). Flip a true con mig 0607 dopo la E2E.'),
  ('orchestrator.advisory_gate_timeout_s', '300', 'orchestrator',
   'Attesa massima (secondi) della prima scrittura sulla barriera advisory prima di procedere SENZA il verdetto dei panel (dichiarandolo al modello: procedere senza approvazione non e'' avere l''approvazione). Clampato nel codice alla deadline residua del run (agent.run_time_budget_s, mig 0604): una barriera che attende oltre la deadline produrrebbe un time_budget mascherato da gate. Default 300 = il timeout tipico di una figura del consiglio (mig 0546).')
ON CONFLICT (key) DO NOTHING;
