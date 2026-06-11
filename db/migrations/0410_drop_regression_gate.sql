-- 0410: rimozione del regression gate (impact-informed) ormai morto.
--
-- Causa radice: il commit eb5e47a (ADR 0017 v2, knowledge graph unificato) ha
-- cancellato crates/mcp-core/src/knowledge/impact.rs senza rimuovere il
-- chiamante brain/agents/regression_gate_node.py. Da allora il nodo girava a
-- ogni fine run, chiamava POST /api/internal/impact/tests-for-run e
-- /api/internal/impact/record-run su mcp-core (endpoint inesistenti), riceveva
-- 404 e ritornava {"ok": False}: la feature era silenziosamente non
-- funzionante. Lo stesso commit aveva gia' rimosso il writer che popolava
-- project_code_edges/project_code_tests (knowledge/code_graph.rs), sostituito
-- dal code-graph su wiki_concept_triples (wiki/code_graph.rs). L'intera
-- pipeline impact era quindi morta a monte e a valle.
--
-- Decisione utente (2026-06-11): rimozione completa, coerente con la nota di
-- eb5e47a ("feature accessorie da reimplementare quando servono"). Rimossi nel
-- codice: il nodo regression_gate dal grafo (brain/agents/graph.py),
-- regression_gate_node.py, route_after_regression_gate (routing.py) e il guard
-- gate_status su project_impact_runs in auto_commit (agent_types.rs).
--
-- Le 6 settings impact.* erano gia' state rimosse dalla mig 0406. Qui si
-- rimuovono le 6 settings regression_gate.* (lette solo dal nodo eliminato) e
-- si droppano le 4 tabelle del sotto-sistema impact, ora senza alcun
-- scrittore/lettore vivo. Nessuna FK in entrata le referenzia (verificato).
--
-- Idempotente: DELETE su chiavi esplicite + DROP TABLE IF EXISTS.

DELETE FROM settings WHERE key IN (
    'regression_gate.enabled',
    'regression_gate.hard_block',
    'regression_gate.max_cycles',
    'regression_gate.max_tests',
    'regression_gate.soft_only',
    'regression_gate.test_timeout_s'
);

-- Tabelle del sotto-sistema impact analysis (mig 0243), non piu' popolate ne'
-- lette da alcun codice vivo dopo eb5e47a. Il code-graph attuale vive su
-- wiki_concept_triples (wiki/code_graph.rs), non su queste tabelle.
DROP TABLE IF EXISTS project_impact_runs;
DROP TABLE IF EXISTS project_code_tests;
DROP TABLE IF EXISTS project_code_edges;
DROP TABLE IF EXISTS project_code_nodes;
