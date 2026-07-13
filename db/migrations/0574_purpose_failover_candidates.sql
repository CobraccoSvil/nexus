-- 0574_purpose_failover_candidates.sql
-- Failover tier-aware GENERALIZZATO ai purpose interni resolve+complete.
--
-- Dopo il classificatore (mig 0573), l'audit dei call site resolve_purpose_model
-- ha trovato altri purpose text-only che fanno UNA sola chiamata pinnata e
-- degradano al primo timeout/errore/content-vuoto senza failover
-- (conversation_summary, docs_generator, custom_instructions, project_analyzer,
-- supervisor, feedback-assist). Il punto unico complete_for_purpose_with_failover
-- (internal_routing) prova N candidati distinti del tier con failover.
--
-- Questo setting governa N per TUTTI i purpose (override per-purpose:
-- routing.<purpose>_failover_candidates). Default nel codice: 3. Il classificatore
-- mantiene la propria chiave routing.classifier_failover_candidates (mig 0573).
--
-- Idempotente: ON CONFLICT DO NOTHING.

INSERT INTO settings (key, value, category, description) VALUES
  ('routing.purpose_failover_candidates', '3', 'routing',
   'Numero di provider DISTINTI del tier che complete_for_purpose_with_failover prova in ordine con failover prima di arrendersi, per i purpose interni resolve+complete. Override per-purpose: routing.<purpose>_failover_candidates. Riusa resolve_purpose_provider_candidates_db (health/cooldown-aware). Clampato a >=1. Niente provider hardcoded (regola G).')
ON CONFLICT (key) DO NOTHING;
