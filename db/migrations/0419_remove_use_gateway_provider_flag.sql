-- 0419_remove_use_gateway_provider_flag.sql
-- Consolidamento chiamate LLM del brain sul gateway Rust (passo 5).
-- Il flag brain.use_gateway_provider non e' piu' letto dal codice: dopo il
-- consolidamento il brain usa SEMPRE il gateway per le chiamate LLM (una sola
-- via, niente ramo-SDK alternativo). La riga in settings e' quindi inutile e
-- viene rimossa. Idempotente.
DELETE FROM settings WHERE key = 'brain.use_gateway_provider';
