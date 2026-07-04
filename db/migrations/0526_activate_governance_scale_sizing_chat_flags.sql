-- 0526: rollout VERSIONATO dei flag accesi finora solo via UPDATE operativo su
-- settings. Erano seminati 'false' di default (mig 0514/0523/0524) per rollout
-- graduale bit-identico; l'accensione viveva solo come UPDATE volatile (un wipe DB
-- + re-migrate li riportava a 'false', violando regola H: le modifiche dati vanno
-- veicolate da migrazione versionata). Fa seguito a 0513/0517, stesso pattern.
--
-- Config DB-driven (regola G): il valore e' letto a runtime dai loader con cache
-- 60s, nessun redeploy necessario per un futuro spegnimento (UPDATE ... 'false').
--
-- Effetto (tutti i rami OFF restano kill-switch vivi, vedi i seed originali):
--   agent.scale.sizing_enabled                 -> sizing agentico nested attivo
--     (nested sotto agent.scale.enabled, gia' ON e durevole da 0517).
--   agent.governance.telemetry_aware           -> ranking candidati modello
--     telemetria-aware (deterministico, regola M).
--   agent.governance.rolling_summary_adaptive  -> gate costo/beneficio del
--     rolling-summary.
--   agent.governance.cooldown_adaptive_ttl     -> TTL adattivo del cooldown per
--     errori billing a recupero prevedibile (quota/rate).
--   chat.activity_stream_enabled               -> rendering "activity stream"
--     della chat (ADR 0037), letto dal frontend via /api/ui-flags.
--
-- Idempotente: se gia' 'true' (accensione operativa) resta 'true'; nessuna riga
-- inserita, solo UPDATE dei valori esistenti.
UPDATE settings
   SET value = 'true'
 WHERE key IN (
   'agent.scale.sizing_enabled',
   'agent.governance.telemetry_aware',
   'agent.governance.rolling_summary_adaptive',
   'agent.governance.cooldown_adaptive_ttl',
   'chat.activity_stream_enabled'
 )
   AND value <> 'true';
