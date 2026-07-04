-- 0523: GOVERNANCE telemetria-aware delle decisioni trasversali su modelli/
-- provider (scelta a RUNTIME tra candidati gia' ammissibili, per probabilita' di
-- successo). Scope DISGIUNTO dal meta-reasoner LLM: nessun LLM, scelta
-- DETERMINISTICA da segnali strutturati (regola M). Il routing BASE resta
-- DB-driven (regola G): la governance NON sceglie la config, RIORDINA i candidati
-- che il routing ha gia' selezionato.
--
-- Opt-in (vincolo primario): tutti i flag default OFF -> con OFF il ranking/gate
-- non viene invocato e il comportamento e' BIT-IDENTICO a prima. Config letta a
-- runtime (cache 60s nexus-auth), nessun redeploy per un futuro ON/OFF (UPDATE).
--
-- Segnali (regola M, tutti strutturati, gia' raccolti alla fonte):
--   - esiti recenti (healthy/latency_ms/error_kind) dal worker model_health_probe
--     (ai_model_health_history, mig 0172);
--   - contatori consecutive_failures / consecutive_tool_failures dal catalog
--     (ai_price_catalog, mig 0172/0269);
--   - provider_in_cooldown dal gate ADR 0020 (snapshot in-memory).
--
-- Punti unici (regola L): SELEZIONE = nexus_agent_graph::decisions::governance
-- (puro); I/O telemetria = mcp-core::governance_telemetry.
--
-- Le reti di sicurezza NON vengono agentificate (per design): fallback_chain,
-- max_escalations, circuit_breaker e safety di scope restano FISSI.

-- ── Master flag + soglie del ranking telemetria-aware (escalation + selezione
--    dinamica dal catalog). Con OFF: escalation e catalog usano l'ordine fisso
--    attuale (catena per rank / featured DESC + cost ASC).
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.governance.telemetry_aware', 'false', 'agent',
   'Master flag della governance telemetria-aware: riordina i candidati modello gia'' ammissibili (escalation intra-provider + selezione dinamica dal catalog) per probabilita'' di successo (telemetria strutturata). OFF = ordine fisso attuale, bit-identico.'),
  ('agent.governance.recent_window', '10', 'agent',
   'Numero di check recenti per modello (ai_model_health_history) considerati nell''error-rate del ranking. Clamp [1, 100] lato codice.'),
  ('agent.governance.exclude_error_rate', '0.5', 'agent',
   'Error-rate recente (recent_failures/recent_checks) oltre cui un candidato e'' "recently_failed" e viene RETROCESSO nel ranking. Range [0,1].'),
  ('agent.governance.exclude_consecutive_failures', '2', 'agent',
   'consecutive_failures (o consecutive_tool_failures) del catalog oltre cui un candidato e'' "recently_failed" e viene RETROCESSO. Valore > 0.'),
  ('agent.governance.min_recent_checks', '2', 'agent',
   'Check recenti minimi perche'' l''error-rate sia considerato affidabile: sotto questa soglia lo storico e'' troppo rumoroso e non penalizza.'),
  ('agent.governance.latency_ref_ms', '20000', 'agent',
   'Latenza (ms) di riferimento per la penalita'' di latenza del ranking (solo tie-breaker, cappata al 10% del punteggio).')
ON CONFLICT (key) DO NOTHING;

-- ── Governance costo/beneficio del rolling-summary: salta il summary quando il
--    prefisso da riassumere e' troppo piccolo per giustificare il costo LLM.
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.governance.rolling_summary_adaptive', 'false', 'agent',
   'Governance costo/beneficio del rolling-summary: quando ON, salta il summary se il prefisso da riassumere e'' sotto agent.governance.rolling_summary_min_prefix. OFF = decide solo select_rolling_summary_cutoff (bit-identico).'),
  ('agent.governance.rolling_summary_min_prefix', '6', 'agent',
   'Soglia minima di messaggi del prefisso sotto cui il rolling-summary NON vale il costo della chiamata LLM. Usata solo se agent.governance.rolling_summary_adaptive = true.')
ON CONFLICT (key) DO NOTHING;

-- ── Governance: TTL adattivo del cooldown LUNGO (billing) per TIPO d'errore.
--    NON tocca il circuit-breaker ne' la fallback-chain (reti di sicurezza FISSE).
--    Il re-probe periodico recupera comunque in anticipo: il TTL e'' solo il
--    limite superiore -> riduzione a basso rischio.
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.governance.cooldown_adaptive_ttl', 'false', 'agent',
   'TTL adattivo del cooldown lungo (billing) per tipo d''errore: quota/rate (recupero periodico prevedibile) -> TTL ridotto; hard billing (credit/balance/payment, ricarica manuale) -> cooldown lungo pieno. OFF = 6h fissi (bit-identico). NON tocca circuit-breaker/fallback-chain.'),
  ('agent.governance.cooldown_adaptive_ttl_min_s', '7200', 'agent',
   'TTL ridotto (secondi) usato dal cooldown adattivo per gli errori billing a recupero prevedibile (quota/rate) quando agent.governance.cooldown_adaptive_ttl = true. Clampato in [provider.cooldown_min_s, provider.cooldown_long_s]. Default 2h.')
ON CONFLICT (key) DO NOTHING;
