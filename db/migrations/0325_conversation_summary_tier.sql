-- 0325_conversation_summary_tier.sql  (regola G + regola L)
--
-- Il purpose 'conversation_summary' (compaction chat) aveva il modello risolto
-- staticamente (google/gemini-2.5-flash) con fallback hardcoded nel codice a
-- openai/gpt-4.1-mini. Se la routing matrix falliva o il provider statico era
-- in cooldown, la compaction cadeva sul fallback hardcoded -> provider morto
-- (openai in billing cooldown) -> "Richiesta non processabile dal provider".
--
-- Fix: aggiungiamo tier='light' (la compaction e' un task leggero di
-- riassunto, non serve reasoning) cosi' resolve_purpose sceglie DINAMICAMENTE
-- il miglior modello light disponibile (come tutti gli altri purpose).
-- Lo statico (google/gemini-2.5-flash) resta come fallback se il tier non ha
-- candidati. Il fallback hardcoded nel codice e' stato rimosso (commit collegato).

UPDATE nexus_purpose_model
SET tier = 'light', updated_at = NOW()
WHERE purpose = 'conversation_summary'
  AND tier IS NULL;
