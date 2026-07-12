-- 0564_version_orchestrator_feature_flags.sql
-- Versiona l'accensione dei feature flag dell'orchestratore che oggi vivono solo
-- come UPDATE operativo sul DB (volatile), non nelle migrazioni.
--
-- Causa radice (regola H, stessa classe della mig 0517 e del cutover engine 0532):
-- questi flag sono seedati 'false' dalle rispettive migrazioni originali (feature
-- introdotte OFF), poi accesi a mano sul DB di produzione via UPDATE su settings.
-- L'accensione NON e' mai stata versionata: su un wipe + re-migrate del DB il
-- default tornerebbe 'false' e l'orchestratore avanzato (planner, understanding,
-- sub-agenti, verifier, worker mode, DAG parallelo) si spegnerebbe silenziosamente,
-- degradando al flusso diretto. La configurazione deve seguire il codice
-- (regola G): il comportamento di default e' quello vivo, quindi va versionato.
--
-- Flag inclusi (tutti VERIFICATI letti dal codice Rust attuale + divergenti
-- seed='false' / vivo='true' / nessun UPDATE-a-true versionato):
--   plan_phase_enabled              (planner_node, todo_store.rs)
--   verifier_enabled                (verifier_node, native_engine.rs)
--   plan_rationale_enabled          (native_engine.rs)
--   subagents_enabled               (subagent_native.rs; kill-switch globale)
--   worker_mode_enabled             (executor.rs)
--   dag_parallel_enabled            (executor.rs / dag_scheduler)
--   exploratory_verify_enabled      (verifier.rs)
--   understanding_enabled           (understanding.rs)
--   understanding_fanout_enabled    (understanding.rs)
--   understanding_synthesize_enabled(understanding.rs)
--
-- Deliberatamente ESCLUSI (NON sono landmine da accendere):
--   - subagent_isolation_enabled  -> gia' versionato a 'true' dalla mig 0517
--   - dag_topological_enabled     -> gia' versionato a 'true' dalla mig 0466
--   - verify_panel_enabled        -> gia' seedato 'true' dalla mig 0439
--   - adaptive_gating_enabled, adaptive_classifier_enabled,
--     plan_rationale_persist_as_note, clarify.require_llm_classifier,
--     orchestrator.meta_steps.*  -> chiavi MORTE nel codice Rust (residui del
--     brain Python rimosso): nessun consumatore le legge. Versionarle sarebbe
--     configurazione fantasma (l'opposto della pulizia fatta dalla mig 0463).
--     Vanno semmai rimosse, non accese: audit separato.
--   - flag di sicurezza/policy (dlp_allow_cloud_*, *_enabled provider) e namespace
--     'agent.*': fuori scope, valori NON uniformi, audit dedicato.
--
-- Idempotente: solo UPDATE dei valori esistenti, con guardia `value <> 'true'`.
-- Se un flag e' gia' 'true' (accensione operativa) resta invariato: sul DB vivo
-- questa migrazione tocca 0 righe; su un DB rigenerato riporta i 10 flag a 'true'
-- dopo che i seed originali li avevano messi 'false'. Nessuna riga inserita:
-- le chiavi esistono gia' (le migrazioni di seed girano prima di questa).

UPDATE settings
   SET value = 'true',
       updated_at = NOW()
 WHERE key IN (
   'orchestrator.plan_phase_enabled',
   'orchestrator.verifier_enabled',
   'orchestrator.plan_rationale_enabled',
   'orchestrator.subagents_enabled',
   'orchestrator.worker_mode_enabled',
   'orchestrator.dag_parallel_enabled',
   'orchestrator.exploratory_verify_enabled',
   'orchestrator.understanding_enabled',
   'orchestrator.understanding_fanout_enabled',
   'orchestrator.understanding_synthesize_enabled'
 )
   AND value <> 'true';
