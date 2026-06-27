-- 0459_shadow_replay_turn_gap.sql
-- Shadow LLM-Replay (ReplayLlmGateway, crates/mcp-core/src/agent_graph_adapter/
-- llm_gateway.rs): tolleranza di gap temporale (in MILLISECONDI) usata per
-- raggruppare gli agent_steps del run primario per TURNO REALE invece che per
-- quoziente step_index/1000.
--
-- Causa radice: la fonte agent_steps e' SPORCA. Il brain Python su retry/fallback
-- dello stesso run RIUSA gli step_index (es. 3000-3003 appaiono in due ondate con
-- created_at distanti); il raggruppamento per quoziente /1000 accorpava le due
-- ondate in un solo mega-turno -> lo shadow vedeva N tool esplorativi ripetuti in
-- UN turno -> detect_signature_loop spurio + stop_reason "loop" + num_tool_calls
-- troncato (divergenza dal primario). Gli step di uno STESSO turno LLM sono scritti
-- nello stesso batch (created_at quasi-identico: osservato identico al microsecondo
-- per i batch singoli, fino a ~2-3ms di jitter per INSERT separati), mentre
-- turni/ondate diversi sono distanti >= centinaia di ms (minimo osservato ~356ms,
-- tipico secondi). Una soglia di 50ms separa nettamente i due regimi.
--
-- Regola G: la soglia operativa vive nel DB, niente hardcode nascosto nella logica.
-- Il codice ha solo un default documentato (DEFAULT_TURN_GAP_US = 50_000 us = 50ms)
-- come rete di sicurezza se il setting e' assente. Cache 60s lato Rust.
--
-- NB: ReplayLlmGateway e' usato SOLO nello shadow read-only (motore nativo non
-- instradato in produzione finche' nexus_orchestrator_engine resta 'python'). Fix a
-- basso rischio sul replay; il fix-radice (step_index univoco nel brain) e'
-- separato/successivo. Categoria 'agent' per renderlo navigabile in UI. Idempotente.

INSERT INTO settings (key, value, category, description) VALUES (
    'agent.shadow.replay_turn_gap_ms',
    '50',
    'agent',
    'Shadow LLM-Replay: tolleranza (millisecondi) per raggruppare gli agent_steps del run primario per turno reale via gap di created_at. Step entro questa soglia = stesso turno LLM; gap maggiore = turno nuovo. Sostituisce il raggruppamento per quoziente step_index/1000 (inaffidabile perche'' il brain riusa gli step_index su retry/fallback). Default 50ms: sta tra il gap intra-turno (<=~2.6ms) e inter-turno (>=~356ms) misurati sui dati reali. Letto da ReplayLlmGateway::turn_gap_us (llm_gateway.rs); il replay e'' usato solo in shadow.'
)
ON CONFLICT (key) DO NOTHING;
