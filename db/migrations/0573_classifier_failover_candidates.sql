-- 0573_classifier_failover_candidates.sql
-- Failover tier-aware del classificatore di intent.
--
-- Il classificatore risolveva UN solo provider dal tier (`light`) e cadeva su
-- neutro (`agentic_default`) al primo timeout/errore. Con Vertex/Google
-- pathologicamente lento (~8s, sopra il timeout del classificatore) il classifier
-- falliva SEMPRE -> tutto il routing semantico degradava a neutro. Ora il
-- classifier prova N candidati DISTINTI del tier con failover (regola L/G: stesso
-- `resolve_purpose_provider_candidates_db` del routing live; niente provider
-- hardcoded), arrendendosi al neutro solo se TUTTI falliscono.
--
-- Questo setting governa quanti candidati provare (default nel codice: 3).
-- Idempotente: ON CONFLICT DO NOTHING.

INSERT INTO settings (key, value, category, description) VALUES
  ('routing.classifier_failover_candidates', '3', 'routing',
   'Numero di provider DISTINTI del tier che il classificatore di intent prova in ordine con failover prima di arrendersi al neutro (agentic_default). Riusa resolve_purpose_provider_candidates_db (health/cooldown-aware). Clampato a >=1 nel codice. Rende il classifier resiliente a un provider lento/instabile (es. Vertex cold-start) senza hardcodare alcun provider.')
ON CONFLICT (key) DO NOTHING;
