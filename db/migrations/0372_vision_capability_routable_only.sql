-- 0372: allinea ai_price_catalog.supports_vision all'instradabilita' REALE per
-- la descrizione immagini (brain/grpc_server/routes/vision.py, rami implementati:
-- google, anthropic, openai).
--
-- Bug: il purpose `vision_describe` e' tier-only (mig 0344) con
-- required_capability='vision'. La risoluzione best_model_for_tier(light, vision)
-- ordina per (is_featured DESC, input_cost ASC) e sceglieva `mistral-small-latest`,
-- marcato supports_vision=true dal metadata LiteLLM ("accetta immagini in input").
-- Ma /vision/describe non instradava mistral -> HTTP 501 ->
-- nexus_describe_image_attachment falliva e l'agente non poteva descrivere le
-- immagini allegate dall'utente.
--
-- Il fix di codice in `classify_capabilities` (crates/mcp-core/src/
-- model_catalog_sync.rs) impedisce nuovi falsi positivi (il flag vision e' deciso
-- per provider PRIMA di consultare meta_vision; provider senza ramo vision.py ->
-- false). Questa migrazione riconcilia i dati GIA' presenti: il ramo
-- Some(existing) del catalog_sync non riallinea le capability dei modelli gia'
-- inseriti, quindi i falsi positivi resterebbero stantii.
--
-- Tocca solo righe capability_source='auto' (le 'manual' curate a mano restano
-- intatte, ADR 0024). Idempotente.

UPDATE ai_price_catalog
   SET supports_vision = false,
       updated_at = NOW()
 WHERE supports_vision = true
   AND capability_source = 'auto'
   AND provider NOT IN ('google', 'anthropic', 'openai');
