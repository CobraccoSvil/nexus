-- 0559: recursion_limit allineato alla topologia del grafo agentico Rust
--
-- Sintomo: run nativi falliti con "recursion_limit superato (150 superstep)"
-- pur essendo run lunghi ma legittimi (molti tool call + stall_recovery attivo
-- da mig 0513). Il floor fisso 150 non copriva iteration_cap=60 con meta-
-- reasoner ON (~230 superstep teorici prima della chiusura ordinaria).
--
-- Fix definitivo (codice): native_engine calcola il cap effettivo come
--   max(agent.graph.recursion_limit, topology(iteration_cap, stall, G1, final_gate))
-- tramite nexus_agent_graph::routing::effective_recursion_limit (punto unico).
--
-- Questa migrazione aggiorna la descrizione del setting (floor esplicito) e
-- alza il seed a 200 per headroom sugli install esistenti; il calcolo topologico
-- resta la fonte autoritativa del cap effettivo.

UPDATE settings
   SET value = CASE WHEN value = '150' THEN '200' ELSE value END,
       description =
         'Pavimento minimo di superstep del grafo agentico Rust (nexus-agent-graph). '
         || 'A runtime il cap effettivo e'' max(floor, topologia calcolata da '
         || 'agent.executor.iteration_cap + nodi stall/G1/final_gate + margine contorno) '
         || 'via effective_recursion_limit (punto unico). Con iteration_cap=60 e '
         || 'stall_recovery ON il cap effettivo e'' ~231 superstep: un run sano chiude '
         || 'per iteration_cap o final_gate prima di raggiungerlo. Cache 60s.',
       updated_at = NOW()
 WHERE key = 'agent.graph.recursion_limit';

INSERT INTO settings (key, value, category, description) VALUES (
    'agent.graph.recursion_limit',
    '200',
    'agent',
    'Pavimento minimo di superstep del grafo agentico Rust (nexus-agent-graph). '
    || 'A runtime il cap effettivo e'' max(floor, topologia da iteration_cap + stall/G1/final_gate). '
    || 'Punto unico: effective_recursion_limit. Cache 60s.'
)
ON CONFLICT (key) DO NOTHING;
