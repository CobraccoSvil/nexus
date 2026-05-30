-- 0201_anthropic_default_model.sql
--
-- ROOT CAUSE (regola H CLAUDE.md): il fallback chain, passando a un nuovo
-- provider privo di riga in nexus_provider_default_model, riusava il
-- current_model del provider PRECEDENTE. provider_hierarchy ha anthropic come
-- primo fallback, ma nexus_provider_default_model NON conteneva anthropic:
-- routing google/gemini-2.5-pro -> google fallisce -> fallback ad anthropic ->
-- default_model(anthropic)=None -> riuso gemini-2.5-pro -> coppia impossibile
-- "anthropic / gemini-2.5-pro" (404 invalid_model).
--
-- FIX DATO: registra il default model anthropic. Il fix architetturale (skip
-- dei provider senza default + guard-rail di coerenza) e' in
-- crates/mcp-core/src/chat_messages.rs e brain/providers/registry.py. Vedi ADR 0016.
--
-- Modello scelto: claude-sonnet-4-6 (is_enabled=true, 24 entry nella
-- nexus_routing_matrix, il modello anthropic piu' presente). Fonte di verita'
-- della coppia provider/model resta questa tabella (regola G CLAUDE.md).
--
-- AUDIT provider abilitati senza default model, eseguito il 2026-05-29 PRIMA
-- di questa migrazione:
--   SELECT DISTINCT c.provider FROM ai_price_catalog c
--    WHERE c.is_enabled = true
--      AND c.provider NOT IN (SELECT provider FROM nexus_provider_default_model);
--   -> risultato: solo "anthropic". Nessun altro provider (vertex/ecc.) manca.
--   Provider abilitati totali: anthropic, deepseek, google, mistral, openai.
--   Gia' presenti in default_model: deepseek, google, mistral, openai.

INSERT INTO nexus_provider_default_model (provider, model_id, notes)
VALUES (
    'anthropic',
    'claude-sonnet-4-6',
    'mig 0201: fix mismatch anthropic/gemini-2.5-pro nel fallback chain (ADR 0016)'
)
ON CONFLICT (provider) DO NOTHING;
