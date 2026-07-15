-- 0595_enforce_qualification_gate.sql
-- FLIP del gate di qualificazione: il routing AGENTICO seleziona solo modelli
-- con qualification_state='qualified' non scaduto e verifica le capability sul
-- PROVATO (qualified_capabilities) invece che sul dichiarato.
--
-- Perche' e' sicuro accenderlo ora (difese gia' in campo, mig 0591-0594):
-- 1. GRANDFATHER: il parco enabled+tool_use e' 'qualified' (suite 0, scadenza
--    jitter 7gg) -> il pool NON e' vuoto al flip.
-- 2. RIQUALIFICAZIONE IN SHADOW: i qualified scaduti/suite-vecchia restano nel
--    pool mentre il worker li ri-prova; un giro inconclusivo NON li degrada.
-- 3. FAIL-CLOSED SORVEGLIATO: se il pool qualificato si svuota davvero, il
--    selettore logga un WARN che punta al worker (mai un fallback silenzioso
--    su modelli non provati, regola G/H).
--
-- Rollback: migrazione successiva che rimette 'false' (mai UPDATE a mano,
-- regola H).

UPDATE settings
   SET value = 'true'
 WHERE key = 'agent.model_qualification.enforce_routing_gate';
