-- 0558_canonical_command_identifiers.sql
-- Normalizza identificatori non canonici (sinonimi IT / alias) verso valori
-- inglesi univoci. Regola: un solo nome per comando/enum in tutto il codebase.
--
-- chat_sessions / agent_runs vivono nei DB-PROGETTO (decommission meta 0507):
-- vedi db/migrations/project/0012_canonical_command_identifiers.sql.

-- plan_behavior_modes usava per errore sinonimi di automation_mode; corregge
-- verso behavior_mode canonici del routing matrix.
UPDATE settings
SET value = 'bilanciata,approfondita',
    updated_at = NOW()
WHERE key = 'orchestrator.plan_behavior_modes'
  AND lower(trim(value)) IN ('automatico,continuo', 'automatic,continuous');
