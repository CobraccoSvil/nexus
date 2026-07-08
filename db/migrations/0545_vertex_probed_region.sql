-- Migration 0545: cache model->region Vertex persistita nel catalog.
--
-- Razionale (regola G: config/cache con UN solo posto, il DB; regola H: fix
-- definitivo che sopravvive a restart/deploy): oggi il gateway tiene la mappa
-- model->region funzionante SOLO in-memory (mig 0476, TTL 300s). Al restart del
-- gateway la cache e' persa, quindi il PRIMO uso di ogni modello Google ripaga il
-- fallback 404 sulla region primaria (es. gemini-3.5-flash e' 404 in europe-west4
-- ma OK in global) prima di trovare quella funzionante.
--
-- Persistendo la region nel catalog, il gateway la legge al primo uso post-restart
-- e va DIRETTO alla region giusta (con le altre discovery_locations come fallback).
-- La scrive best-effort al primo successo non-404 (fail-open: un errore di
-- persistenza NON rompe l'inference). NULL = modello mai probato.
ALTER TABLE ai_price_catalog
  ADD COLUMN IF NOT EXISTS vertex_probed_region TEXT;

COMMENT ON COLUMN ai_price_catalog.vertex_probed_region IS
  'Region Vertex confermata funzionante per il modello (cache model->region '
  'persistita, mig 0545). Scritta dal gateway al primo successo non-404, letta '
  'al primo uso post-restart per andare diretto alla region giusta. NULL = mai '
  'probato. Rilevante solo per provider=google backend Vertex.';
