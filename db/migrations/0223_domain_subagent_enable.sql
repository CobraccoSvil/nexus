-- Migrazione 0220: abilita i kind domain-specific nella whitelist (Componente C).
-- Senza questo, il guard Rust in agent_tools/subagent.rs rifiuta i kind non in
-- whitelist. Aggiunge i 6 nuovi kind ai 5 esistenti (idempotente: ricostruisce
-- la lista completa). orchestrator.subagents_enabled resta scelta admin a runtime.

UPDATE settings
   SET value = 'plan,explore,implement,verify,review,rust_implementer,python_implementer,frontend_implementer,db_architect,doc_writer,test_author',
       updated_at = NOW()
 WHERE key = 'orchestrator.subagent_kinds_whitelist';
