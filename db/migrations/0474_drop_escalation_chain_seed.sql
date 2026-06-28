-- 0474_drop_escalation_chain_seed.sql
-- DROP della tabella seed ZOMBIE nexus_model_escalation_chain (mig 0128).
--
-- Causa radice (regola H, no big-bang): la mig 0471 ha trasformato la catena di
-- escalation LIVELLO B (loop intra-provider) in una VISTA derivata dal catalog
-- (v_model_escalation_chain) e ha ricablato l'unico consumer Rust
-- (PgEscalationPort::chain_for) su quella vista. Da allora la tabella seed
-- manuale nexus_model_escalation_chain non ha piu' alcun reader ne' writer in
-- produzione: e' una terza copia (zombie) della stessa decisione "qual e' il
-- modello piu' capace", in violazione della regola L (punto unico).
--
-- La mig 0471 aveva volutamente lasciato in piedi la tabella per la finestra di
-- osservazione (no DROP nello stesso commit del ricablaggio). Conclusa la
-- finestra, la rimuoviamo qui in una migrazione separata.
--
-- IDEMPOTENTE: IF EXISTS copre il caso in cui la tabella sia gia' assente
-- (ambienti dove non fu mai creata o gia' droppata).

DROP TABLE IF EXISTS nexus_model_escalation_chain;
