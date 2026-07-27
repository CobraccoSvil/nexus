-- 0592_exclude_preview_agentic.sql
-- Esclusione dei modelli PRE-GA (preview/experimental) dal routing AGENTICO.
--
-- Root cause (incidenti consiglieri 2026-07-14/15 + indagine web con fonti):
-- i modelli *-preview su Vertex girano su capacita' CONDIVISA best-effort
-- (Dynamic Shared Quota): 429 RESOURCE_EXHAUSTED a intermittenza anche a
-- volumi bassi, per congestione GLOBALE del pool, non per quota nostra —
-- comportamento DOCUMENTATO, non un'anomalia. E i preview vengono ritirati
-- con ~2 settimane di preavviso: 404 "Publisher model not found" improvvisi
-- su tutte le region (11 modelli auto-disabilitati il 14/07). Google stessa
-- dichiara gli experimental non adatti alla produzione. Le chain agentiche
-- (figure del consiglio) muoiono su un singolo 429/404: i pre-GA non devono
-- entrare nella selezione agentica di default.
--
-- Il criterio vive nel punto unico select_models_tierchain (regola L,
-- predicato PRE_GA_MODEL_PREDICATE_SQL); questo flag lo governa (regola G).
-- Il PIN esplicito dell'utente non passa dal selettore e resta libero: chi
-- vuole un preview lo chiede per nome.
--
-- Acceso subito ('true'): il rischio e' basso per costruzione — i modelli GA
-- restano candidati e la tier-chain degrada al tier successivo se un tier
-- resta senza candidati. Spegnibile via migrazione successiva.

INSERT INTO settings (key, value, category, description) VALUES
  ('agent.model_qualification.exclude_preview_agentic', 'true', 'agent',
   'Routing agentico: true = esclude dalla selezione i modelli pre-GA (nomi con preview/experimental/-exp), che girano su capacita'' condivisa best-effort (429 intermittenti) e vengono ritirati con breve preavviso (404). I purpose non-agentici e il pin esplicito non sono toccati. Spegnere solo via migrazione.')
ON CONFLICT (key) DO NOTHING;
