-- 0529: failover cross-provider AGENTICO — il sostituto di un provider caduto
-- non e' piu' scelto da una catena fissa col pavimento 'medium'
-- (best_agentic_failover) ma dal modulo puro pick_failover_model: TUTTI i
-- candidati agentici ammissibili (ogni tier, esclusi cooldown/gia' provati),
-- ordinati per salute -> likelihood (telemetria strutturata, regola M) con il
-- tier del modello caduto come INDICAZIONE, mai come filtro.
--
-- L'indicazione e' un'affinita' moltiplicativa: per ogni livello di tier SOTTO
-- quello corrente il punteggio viene scalato di questo fattore (penalty^delta).
-- I livelli sopra non sono ne' premiati ne' penalizzati. Un candidato piu'
-- debole ma con likelihood nettamente migliore puo' quindi superare
-- l'indicazione; un downgrade resta sempre ammesso se e' l'unica opzione sana.
--
-- Config DB-driven (regola G): letto da load_governance_policy (cache 60s).
-- Range valido (0, 1]; 1.0 = indicazione disattivata; fuori range -> default
-- 0.85 lato codice.
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.governance.failover_downgrade_penalty', '0.85', 'agent',
   'Affinita'' di tier del failover agentico (pick_failover_model): moltiplicatore applicato per ogni livello di tier sotto quello del modello caduto (penalty^delta). Il tier corrente e'' un''indicazione, mai un filtro. Range (0, 1]; 1.0 = indicazione disattivata.')
ON CONFLICT (key) DO NOTHING;
