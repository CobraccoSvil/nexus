-- 0403_cache_prices_catalog.sql
--
-- P1 roadmap contesto: prezzi del prompt caching nel catalog per TUTTI i
-- provider che lo offrono (prima solo anthropic era valorizzato: il ledger
-- fatturava a prezzo pieno token gia' scontati dal provider, sovrastimando i
-- costi e rendendo invisibile il risparmio reale).
--
-- Prerequisito gia' nel codice (stesso commit): _record_usage decurta i
-- cache_read_tokens da prompt_tokens per i provider non-anthropic (che li
-- INCLUDONO nella usage), eliminando il double-counting.
--
-- Sconti dai listini ufficiali (rapporto sul prezzo input):
--   deepseek  : cache hit ~0.1x  (context caching automatico su disco)
--   google    : implicit caching 2.5+ ~0.25x (sconto 75%)
--   openai    : gpt-4o ~0.5x; gpt-4.1/gpt-5 ~0.25x
--   mistral   : ~0.1x (prompt caching con blocchi 64 token)
--   anthropic : gia' valorizzato (0.1x read / 1.25x write); qui si completa
--               l'eventuale riga incompleta (claude-opus-4-8 a 0).
-- I valori sono derivati dal prezzo input del catalog stesso (proporzione),
-- cosi' restano coerenti se l'admin aggiorna il prezzo base. Aggiornabili da
-- admin in ogni momento (regola G).
--
-- cache_creation resta 0 per i provider con caching automatico (nessun costo
-- di scrittura separato: il miss paga il prezzo input normale).
-- Idempotente: tocca solo le righe con cache_read a 0/NULL.

UPDATE ai_price_catalog
SET cache_read_cost_per_million_tokens = input_cost_per_million_tokens * 0.1
WHERE provider = 'deepseek'
  AND COALESCE(cache_read_cost_per_million_tokens, 0) = 0
  AND input_cost_per_million_tokens > 0;

UPDATE ai_price_catalog
SET cache_read_cost_per_million_tokens = input_cost_per_million_tokens * 0.25
WHERE provider = 'google'
  AND model LIKE 'gemini-2.5%'
  AND COALESCE(cache_read_cost_per_million_tokens, 0) = 0
  AND input_cost_per_million_tokens > 0;

UPDATE ai_price_catalog
SET cache_read_cost_per_million_tokens = input_cost_per_million_tokens * 0.5
WHERE provider = 'openai'
  AND model LIKE 'gpt-4o%'
  AND COALESCE(cache_read_cost_per_million_tokens, 0) = 0
  AND input_cost_per_million_tokens > 0;

UPDATE ai_price_catalog
SET cache_read_cost_per_million_tokens = input_cost_per_million_tokens * 0.25
WHERE provider = 'openai'
  AND (model LIKE 'gpt-4.1%' OR model LIKE 'gpt-5%')
  AND COALESCE(cache_read_cost_per_million_tokens, 0) = 0
  AND input_cost_per_million_tokens > 0;

UPDATE ai_price_catalog
SET cache_read_cost_per_million_tokens = input_cost_per_million_tokens * 0.1
WHERE provider = 'mistral'
  AND COALESCE(cache_read_cost_per_million_tokens, 0) = 0
  AND input_cost_per_million_tokens > 0;

-- Completa le righe anthropic incomplete (es. claude-opus-4-8 censito a 0).
UPDATE ai_price_catalog
SET cache_read_cost_per_million_tokens = input_cost_per_million_tokens * 0.1,
    cache_creation_cost_per_million_tokens = input_cost_per_million_tokens * 1.25
WHERE provider = 'anthropic'
  AND COALESCE(cache_read_cost_per_million_tokens, 0) = 0
  AND input_cost_per_million_tokens > 0;
