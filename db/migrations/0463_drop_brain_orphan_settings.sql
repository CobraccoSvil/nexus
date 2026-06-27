-- 0463_drop_brain_orphan_settings.sql
-- Zero-Python (eliminazione brain): rimuove le settings rimaste SENZA consumatore
-- dopo che il grafo agentico e' passato a Rust e il brain Python e' stato eliminato.
--
-- Contesto: l'audit configurazioni (scripts/audit_settings.py, gate ratchet) ha
-- segnalato 42 chiavi "morte" (presenti in settings ma non lette da alcun codice)
-- una volta rimosso il brain. Di queste:
--   - 15 sono state CABLATE nel grafo Rust (native_engine.rs load_routing_config/
--     load_executor_config/load_todo_runner_config + reflection enabled): ora sono
--     lette dal DB (regola G) -> tornano VIVE, NON vanno cancellate.
--   - le 27 qui sotto NON hanno (piu') un consumatore in Rust: o la feature del
--     brain non e' (ancora) portata nel grafo Rust, o il Rust usa un meccanismo
--     diverso. Tenerle nel DB e' configurazione fantasma (debito). Le settings
--     seguono il codice: si rimuovono ora e si ri-aggiungeranno con la migrazione
--     che porta la feature, se e quando verra' implementata.
--
-- Dettaglio per gruppo (motivo della rimozione):
--   * brain_grpc_port / brain_rest_port / brain_billing_enabled
--       -> infra del servizio brain, eliminato. Obsolete.
--   * routing.classifier_provider / classifier_model / classifier_cache_max_entries
--       -> il classifier Rust (intent_classifier.rs) risolve il modello via purpose
--          'intent_classifier' (nexus_purpose_model, regola G) e usa TtlCache senza
--          cap hard. Le vecchie chiavi del brain non sono piu' consultate.
--   * routing.intent_health_enabled / _cooldown_secs / _failure_threshold_pct /
--     _min_attempts
--       -> intent-health probe del brain NON portato (il Rust usa provider_cooldown).
--   * gateway.complete_timeout_seconds / gateway.stream_timeout_seconds
--       -> non lette dal client gateway Rust ne' dal gateway. Nessun consumatore.
--   * nexus_gateway_url
--       -> il Rust risolve il gateway da 'nexus_gateway_port' (URL = 127.0.0.1:porta).
--   * knowledge.rag_injection_mode / rag.chunker.algorithm
--       -> non portate: il RAG/chunker Rust non espone questi tuning.
--   * extended_thinking_enabled / extended_thinking_budget_tokens
--       -> il Rust gestisce il thinking via capability del modello (vista 0318 /
--          should_disable_thinking), non via questi flag.
--   * agent.fallback.adapt_enabled
--       -> fallback-adapt non portato come tuning configurabile.
--   * agent.closure_judge.active / _min_result_chars / _shadow_enabled
--       -> closure_judge NON portato (era shadow/OFF di default, TODO esplicito nel
--          learner Rust).
--   * agent.final_gate.build_check_enabled / endpoint_check_enabled /
--     endpoint_timeout_seconds / runtime_log_command / runtime_log_command_per_project
--       -> nel grafo Rust build/log/endpoint sono risolti per-progetto a monte
--          (FinalGateConfig.build_command/log_command/endpoint_criterion): questi
--          flag del brain non hanno consumatore.
--   * agent.upscale.cost_cap_usd_per_run
--       -> ExecutorConfig non espone un cost-cap per-run (upscale.enabled e
--          target_overhead_ratio SONO cablati, mig di pari passo).
--
-- Idempotente: DELETE su chiavi gia' assenti e' un no-op.

DELETE FROM settings WHERE key IN (
    'brain_grpc_port',
    'brain_rest_port',
    'brain_billing_enabled',
    'routing.classifier_provider',
    'routing.classifier_model',
    'routing.classifier_cache_max_entries',
    'routing.intent_health_enabled',
    'routing.intent_health_cooldown_secs',
    'routing.intent_health_failure_threshold_pct',
    'routing.intent_health_min_attempts',
    'gateway.complete_timeout_seconds',
    'gateway.stream_timeout_seconds',
    'nexus_gateway_url',
    'knowledge.rag_injection_mode',
    'rag.chunker.algorithm',
    'extended_thinking_enabled',
    'extended_thinking_budget_tokens',
    'agent.fallback.adapt_enabled',
    'agent.closure_judge.active',
    'agent.closure_judge.min_result_chars',
    'agent.closure_judge.shadow_enabled',
    'agent.final_gate.build_check_enabled',
    'agent.final_gate.endpoint_check_enabled',
    'agent.final_gate.endpoint_timeout_seconds',
    'agent.final_gate.runtime_log_command',
    'agent.final_gate.runtime_log_command_per_project',
    'agent.upscale.cost_cap_usd_per_run'
);
