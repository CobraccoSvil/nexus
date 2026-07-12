-- 0572_review_panel_autoconvene.sql
-- Rinforzo PROGRAMMATICO della review adversariale a valle (Fase C ultracode).
--
-- La direttiva <revisione_finale> (mig 0571) e' LLM-driven: un test reale ha
-- mostrato che il modello puo' saltarla (gemini-2.5-flash: run completato senza
-- convocare il panel). Questo flag abilita l'innesco DETERMINISTICO: dopo un run
-- che ha MODIFICATO codice e NON ha gia' fatto una review, mcp-core convoca il
-- panel dal codice (post-step in agent_run, simmetrico al pre-step del consiglio)
-- e riconcilia il verdetto nel resoconto finale.
--
-- - review_panel_autoconvene_enabled: kill-switch (default true, regola G).
-- - review_panel_size: numero di revisori convocati dal rinforzo (default 2;
--   il quorum e' governato da orchestrator.review_quorum_min_valid).
--
-- Idempotente: ON CONFLICT DO NOTHING (non sovrascrive un valore gia' impostato).

INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.review_panel_autoconvene_enabled', 'true', 'orchestrator',
   'Rinforzo programmatico della review adversariale a valle: se true, mcp-core convoca deterministicamente un panel di review dopo un run che ha modificato codice e non ha gia'' fatto una review, e riconcilia il panel_verdict nel resoconto. Kill-switch (regola G). Distinto dalla direttiva <revisione_finale> (mig 0571), che resta come guida LLM.'),
  ('orchestrator.review_panel_size', '2', 'orchestrator',
   'Numero di revisori (sub-run kind=review) convocati dal rinforzo programmatico della review adversariale. Clampato a >=1 nel codice. Il quorum di voti validi resta orchestrator.review_quorum_min_valid; il veto avversario orchestrator.review_fail_on_high_severity.')
ON CONFLICT (key) DO NOTHING;
