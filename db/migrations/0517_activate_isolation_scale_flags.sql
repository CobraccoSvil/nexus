-- Rollout: accensione dei flag isolamento fisico sub-agenti (Fase 2) e
-- scale-controller bidirezionale (task #14).
--
-- Fa seguito a mig 0513 (che accese i flag Fase 1: meta-reasoner stall_recovery,
-- orchestrazione LLM-driven, redazione contestuale PII). I seed conservativi delle
-- mig 0515 (orchestrator.subagent_isolation_enabled) e 0516 (agent.scale.*) sono
-- 'false' per default bit-identico; questa migrazione li porta a ON come rollout
-- deciso, rendendo l'accensione DUREVOLE (sopravvive a wipe DB + re-apply): senza
-- di essa l'accensione vivrebbe solo come UPDATE operativo su settings (volatile).
--
-- Comportamento attivato:
--   orchestrator.subagent_isolation_enabled = true
--     -> i sub-agenti paralleli con write_scope disgiunti girano in git worktree
--        effimeri con apply serializzato per-root (PR2/PR3/PR4). Fuori dal caso
--        disgiunto -> degrada a sequenziale (sicuro). Prerequisito runtime:
--        probe_isolatable(project_root) fail-closed sul singolo progetto.
--   agent.scale.enabled = true
--     -> il detector PRE-LLM valuta la scala-tier a cadenza (gate break-even +
--        precedenza stallo); il nodo ScaleControl consulta scale_assess.
--   agent.scale.downscale_enabled = true
--     -> abilita anche il downscale (con vincolo finestra FIX-B + floor per intent
--        + reversal-pin + tetto max_tier_changes_per_run). Senza questo il gate 2
--        di apply_hysteresis blocca ogni DownscaleTo (solo up-consolidation).
--
-- Idempotente: se gia' 'true' (accensione operativa via settings) resta 'true';
-- nessuna riga inserita, solo UPDATE dei valori esistenti.
UPDATE settings
   SET value = 'true'
 WHERE key IN (
   'orchestrator.subagent_isolation_enabled',
   'agent.scale.enabled',
   'agent.scale.downscale_enabled'
 )
   AND value <> 'true';
