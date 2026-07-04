-- 0522_run_token_hard_cap.sql
-- BACKSTOP di catastrofe per il budget token cumulativo del run (motore agentico
-- nativo, nodo executor). Chiude il seam agentico dei limiti anti-runaway 4d/4e
-- (mig 0520): con il meta-reasoner ACCESO (agent.stall_recovery.enabled=true, gia'
-- ON dalla mig 0513) il limite MORBIDO agent.run_token_budget NON chiude piu'
-- direttamente il run, ma diventa il TRIGGER che consulta il giudice agentico
-- (StallReason -> nodo StallRecovery -> RecoveryMove). Il giudice puo' decidere di
-- PROSEGUIRE (es. il run e' vicino a chiudere): senza un tetto DURO un modello
-- patologico brucerebbe token all'infinito.
--
-- agent.run_token_hard_cap e' la rete di sicurezza NON-negoziabile (regola H): al
-- raggiungimento (>=) l'executor chiude d'autorita' (close_runaway) SENZA
-- consultare il giudice, come il comportamento storico di 822e083. Seminato a ~2x
-- il budget morbido (400000 -> 800000).
--
-- Retro-compat: con agent.stall_recovery.enabled=false il run_token_budget chiude
-- gia' prima (l'hard-cap e' irrilevante) -> comportamento bit-identico a 822e083.
-- 0 = hard-cap disabilitato. Cache 60s lato Rust (get_setting), regola G: niente
-- fallback hardcoded nel codice (il Default 800000 dell'ExecutorConfig vale SOLO a
-- DB irraggiungibile; il wiring load_executor_config passa il valore reale).

INSERT INTO settings (key, value, category, description) VALUES
  ('agent.run_token_hard_cap', '800000', 'agent',
   'BACKSTOP di catastrofe sul budget token cumulativo del run. Con il meta-reasoner acceso (agent.stall_recovery.enabled) il limite morbido agent.run_token_budget diventa un TRIGGER del giudice agentico (StallReason -> StallRecovery), che puo'' decidere di proseguire. Questo hard-cap e'' la rete di sicurezza non-negoziabile: al raggiungimento (>=) l''executor chiude d''autorita'' (close_runaway) SENZA consultare il giudice. Seminato a ~2x run_token_budget. 0 = disabilitato. Rilevante solo a stall_recovery.enabled=true (altrimenti run_token_budget chiude gia'' prima). Cache 60s (mig 0522).')
ON CONFLICT (key) DO NOTHING;
