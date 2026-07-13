-- 0577: escalation di modello su NON-CONVERGENZA del final_gate (regola G/H).
--
-- Gap del run cc01d06d: quando il final_gate esaurisce `agent.final_gate.max_cycles`
-- con i criteri OGGETTIVI ancora falliti (un modello scadente che non ripara il
-- codice entro i suoi tentativi), il gate chiudeva secco `FailedDiagnosed` lasciando
-- il run inchiodato su quel modello (osservato: mistral-medium-3 per 52 iterazioni,
-- 0 escalation). Loop del gate ed escalation erano DISACCOPPIATI.
--
-- Fix: al cap, invece di chiudere, il gate cede il turno all'executor che PROMUOVE a
-- un modello piu' capace tramite il PUNTO UNICO `maybe_escalate_nonconvergence`
-- (regola L). Bound da `auto_escalations < agent.executor.max_escalations` (budget
-- condiviso, gia' esistente): esaurite le promozioni chiude come prima (backstop).
--
-- Flag ON di default. OFF -> comportamento storico bit-identico (chiusura secca).
-- Il tetto di escalation riusa `agent.executor.max_escalations` (nessuna nuova
-- chiave: e' lo stesso contatore `auto_escalations` per l'intero run).

INSERT INTO settings (key, value, category, description) VALUES
  ('agent.final_gate.escalate_on_nonconvergence', 'true', 'agent',
   'Al cap di max_cycles con criteri oggettivi falliti, il final_gate cede il turno all executor per promuovere un modello piu capace invece di chiudere secco FailedDiagnosed. Bound da agent.executor.max_escalations. OFF = chiusura secca (comportamento storico).')
ON CONFLICT (key) DO NOTHING;
