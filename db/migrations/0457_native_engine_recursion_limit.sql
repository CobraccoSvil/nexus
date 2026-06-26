-- 0457_native_engine_recursion_limit.sql
-- F3 (motore nativo Rust): espone come setting DB-driven il cap anti-loop del
-- grafo agentico Rust (nexus-agent-graph), letto da
-- crates/mcp-core/src/native_engine.rs::build_native_engine (regola G: niente
-- fallback hardcoded nascosto, la configurazione vive nel DB).
--
-- Il valore replica il safe-default di RoutingConfig::default().recursion_limit
-- (150), a sua volta allineato al recursion_limit di LangGraph
-- (MAX_AGENT_ITERATIONS con margine per i nodi non-executor del path Python). E'
-- la rete di sicurezza ultima contro un grafo non convergente: un run sano non
-- la raggiunge mai. Modificabile a caldo (cache 60s lato Rust).
--
-- NB: il motore nativo NON e' instradato in produzione (nexus_orchestrator_engine
-- resta 'python', mig 0451): questo setting alimenta il path nativo
-- eseguibile/testato ma non chiamato sul flusso reale. Categoria 'agent' per
-- renderlo navigabile in UID. Idempotente.

INSERT INTO settings (key, value, category, description) VALUES (
    'agent.graph.recursion_limit',
    '150',
    'agent',
    'Cap massimo di superstep del grafo agentico Rust (nexus-agent-graph) per run: oltre questa soglia un grafo che non converge si ferma invece di girare all''infinito (rete di sicurezza, un run sano non la raggiunge). Allineato al recursion_limit di LangGraph del path Python. Letto da native_engine.rs (motore nativo F3, non instradato in produzione finche'' nexus_orchestrator_engine resta python).'
)
ON CONFLICT (key) DO NOTHING;
