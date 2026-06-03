-- 0258_deepseek_v4_catalog_context_window.sql
--
-- Completa il fix della migrazione 0256. La 0256 ha corretto il context window
-- dei modelli DeepSeek V4 (deepseek-v4-flash, deepseek-v4-pro) da 8192 a 131072,
-- ma SOLO nella tabella nexus_provider_capabilities.max_context_tokens.
--
-- Il guard predittivo del brain (_predictive_cap_check in brain/agents/nodes.py)
-- legge pero' il context window da ai_price_catalog.context_window, rimasto a
-- 8192. Conseguenza: cap predittivo = 50% * 8192 = 4096 token. Il context
-- agentico (system prompt + tool + history) supera quasi sempre 4096, quindi
-- OGNI tool_call (es. list_files) veniva bloccata da predictive_cap con
-- "[ERROR: chiamata bloccata da predictive context cap]". Il modello non poteva
-- eseguire alcun tool -> hollow completion / loop -> la chat "non risolveva".
--
-- Fix: allinea ai_price_catalog.context_window a 131072 (context ufficiale
-- DeepSeek, gia' verificato empiricamente in 0256 con prompt da ~78.000 token).
-- I default/hard output tokens NON c'entrano e restano invariati.
--
-- Regola G/H: la verita' resta nel DB via migrazione versionata, niente UPDATE
-- ad-hoc fuori migrazione. Idempotente.

UPDATE ai_price_catalog
SET context_window = 131072
WHERE model IN ('deepseek-v4-flash', 'deepseek-v4-pro')
  AND (context_window IS NULL OR context_window < 131072);
