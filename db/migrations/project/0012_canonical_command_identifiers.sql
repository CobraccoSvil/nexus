-- 0012_canonical_command_identifiers.sql
-- Normalizza identificatori non canonici (sinonimi IT / alias) verso valori
-- inglesi univoci nei DB-PROGETTO. Meta-DB: solo settings (mig 0558).

-- automation_mode (chat_sessions + agent_runs): study | confirm | automatic
UPDATE chat_sessions
SET automation_mode = CASE lower(trim(automation_mode))
    WHEN 'automatico' THEN 'automatic'
    WHEN 'auto' THEN 'automatic'
    WHEN 'continuo' THEN 'automatic'
    WHEN 'conferma' THEN 'confirm'
    WHEN 'studio' THEN 'study'
    ELSE automation_mode
END
WHERE lower(trim(automation_mode)) IN ('automatico', 'auto', 'continuo', 'conferma', 'studio');

UPDATE agent_runs
SET automation_mode = CASE lower(trim(automation_mode))
    WHEN 'automatico' THEN 'automatic'
    WHEN 'auto' THEN 'automatic'
    WHEN 'continuo' THEN 'automatic'
    WHEN 'conferma' THEN 'confirm'
    WHEN 'studio' THEN 'study'
    ELSE automation_mode
END
WHERE lower(trim(automation_mode)) IN ('automatico', 'auto', 'continuo', 'conferma', 'studio');

-- supervisor_mode: none | anomaly | interleaved | continuous (niente alias a/b/c)
UPDATE agent_runs
SET supervisor_mode = CASE lower(trim(supervisor_mode))
    WHEN 'a' THEN 'interleaved'
    WHEN 'b' THEN 'continuous'
    WHEN 'c' THEN 'anomaly'
    ELSE supervisor_mode
END
WHERE lower(trim(supervisor_mode)) IN ('a', 'b', 'c');
