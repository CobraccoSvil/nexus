-- 0518_remove_orphan_settings.sql
-- Rimozione di settings ORFANE (nel DB ma senza reader nel codice) rimaste dopo il
-- porting LangGraph->Rust. Il brain Python e' stato eliminato; queste chiavi non
-- hanno consumatore nel grafo Rust (verificato via grep su crates/**). Le settings
-- seguono il codice (regola G): si rimuovono ora e si ri-aggiungeranno con la
-- migrazione che portera' la feature, se e quando verra' implementata. Stesso
-- pattern di mig 0463 (che ne rimosse 27 nello stesso spirito).
--
-- GRUPPO A — sotto-gate KB del nodo clarify + grounding RAG sub-agent (mai portati):
--   * clarify.decision_lookup_enabled / decision_min_score / decision_topk (mig 0209)
--       -> lookup-decisione RAG (Cluster 4): richiede porta KB-search inesistente.
--          NB: clarify.confirm_irreversible_in_auto (STESSA mig 0209) E' portata e
--          attiva (clarify_or_expand.rs:384) -> NON si rimuove.
--   * orchestrator.subagent_rag_grounding_enabled / _topk / _min_score / _snippet_max
--     e orchestrator.subagent_inherit_plan_rationale (mig 0221)
--       -> grounding RAG + rationale ai sub-agent (Componente B): non iniettato dal
--          dispatch sub-agent nel grafo Rust.
--   * clarify.intake_gate_enabled / intake_match_min_score / intake_topk (mig 0226)
--       -> intake gate multi-asse (Componente 1): richiede porta KB-search + LLM.
--          Anche il purpose 'intake_gate' e' orfano.
--
-- GRUPPO B — chiave orfana del meta-reasoner stall-recovery (mig 0510):
--   * agent.loop.max_ask_user_per_session
--       -> introdotta dalla mig 0510 (stall-recovery) ma MAI cablata a un reader nel
--          motore nativo (wiring stall-recovery non completato). Nata orfana (unica
--          traccia: l'INSERT in 0510, nessun get_setting in crates/**). Rimossa qui
--          per riportare a zero le settings 'morte' del gate audit-settings; il
--          wiring stall-recovery la re-inserira' insieme al reader (regola G).
--
-- NB: routing.intent_health_* (mig 0249) e' gia' stato rimosso in mig 0463.
--
-- Idempotente: DELETE su chiavi/purpose gia' assenti e' un no-op.

-- Chiavi elencate pulite, SENZA commenti inline nel corpo IN(...): il parser di
-- xtask audit-settings (apply_delete_statements) usa la regex `... IN \(([^)]*)\)`
-- e una parentesi in un commento la troncherebbe, mancando la DELETE. Il dettaglio
-- per gruppo/migrazione e' nell'header sopra. Ordine: 0209, 0221, 0226, poi 0510.
DELETE FROM settings WHERE key IN (
    'clarify.decision_lookup_enabled',
    'clarify.decision_min_score',
    'clarify.decision_topk',
    'orchestrator.subagent_rag_grounding_enabled',
    'orchestrator.subagent_rag_grounding_topk',
    'orchestrator.subagent_rag_grounding_min_score',
    'orchestrator.subagent_rag_grounding_snippet_max',
    'orchestrator.subagent_inherit_plan_rationale',
    'clarify.intake_gate_enabled',
    'clarify.intake_match_min_score',
    'clarify.intake_topk',
    'agent.loop.max_ask_user_per_session'
);

-- Purpose model del gate di intake: orfano (nessun codice Rust risolve 'intake_gate';
-- le mig 0338/0364/0384 lo citano solo come esempio testuale). Rimosso col gate.
DELETE FROM nexus_purpose_model WHERE purpose = 'intake_gate';
