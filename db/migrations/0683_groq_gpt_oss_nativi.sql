-- 0683 — Il deny che protegge dagli aggregatori escludeva i modelli NATIVI di groq.
--
-- ROOT CAUSE (misurata il 07/08/2026, indagando perche' groq non venisse mai
-- chiamato). Il deny condiviso fra i provider contiene `^openai/`, che esiste
-- per una ragione buona: impedire che un aggregatore rivenda i modelli OpenAI
-- come propri, creando doppioni con prezzi e quote diversi dallo stesso
-- modello. Su groq pero' `openai/gpt-oss-120b` e `openai/gpt-oss-20b` NON sono
-- modelli OpenAI rivenduti: sono i modelli open-weight che groq serve sul
-- proprio hardware, col proprio prezzo e la propria quota. Il prefisso
-- `openai/` e' il nome che l'upstream gli da', non l'identita' del fornitore.
--
-- Erano i due candidati migliori del parco groq: `gpt-oss-120b` e' reasoning,
-- tier medium, $0.15/M in ingresso — piu' economico di deepseek-v4-pro (0.435)
-- e di glm-5.2 (0.42) sulla stessa fascia.
--
-- LO STATO MISURATO, che mostra come groq fosse escluso da CAUSE INDIPENDENTI e
-- non da una sola:
--
--   groq/compound, compound-mini, qwen3.6-27b  passano la policy, ma hanno
--       pricing_state='unknown' e costo 0 -> `reconcile_enable_returning_to_policy`
--       richiede `NOT price_unknown` e non li riaccende. Restano fuori, ed e'
--       giusto: un modello a prezzo ignoto non deve entrare nel routing (il
--       costo verrebbe contabilizzato a zero).
--   openai/gpt-oss-120b, gpt-oss-20b           hanno prezzo noto, ma il deny li blocca. <- QUESTA migrazione
--   llama-3.1-8b-instant                       ha prezzo, ma ha FALLITO la prova
--       tool (`tool_smoke:no_tool_call:0<1:end_turn`): non deve rientrare.
--   llama-3.3-70b-versatile                    ha prezzo, ma l'allow chiede
--       `^meta-llama/llama-4` e questo e' un 3.3.
--
-- Il correttivo tocca SOLO la causa dimostrata. Non si allarga l'allow ai
-- llama-3.x — uno dei due ha gia' fallito la prova d'uso dei tool — e non si
-- forzano i prezzi dei modelli a costo ignoto: sarebbe la toppa che la regola H
-- vieta, e il prezzo inventato produrrebbe una contabilita' falsa.
--
-- COSA ACCADE DOPO. Il riallineamento policy->catalog
-- (`reconcile_catalog_with_policy`, che gira a ogni tick) trovera' i due
-- gpt-oss con `is_enabled=false`, `auto_disabled_reason` che contiene 'policy'
-- e prezzo noto: li riabilitera' da solo. Nessun UPDATE manuale sul catalogo —
-- il rientro passa dalla stessa regola che li aveva esclusi.
--
-- I due restano poi soggetti alla batteria di qualificazione: oggi entrambi
-- hanno `qualification_reason='inconclusive_round'`, cioe' non sono mai stati
-- misurati. Con il gate di qualificazione acceso resteranno fuori dal routing
-- agentico finche' non passeranno una prova vera — ed e' il comportamento
-- voluto: questa migrazione li rende CANDIDATI, non promossi.
--
-- ROLLBACK: rimettere '^openai/' nel deny di groq
--   UPDATE nexus_model_selection_policy
--      SET denied_patterns = array_append(denied_patterns, '^openai/')
--    WHERE provider = 'groq';

-- Il rischio che il deny copriva resta coperto in modo PIU' PRECISO: su groq gli
-- unici `openai/*` sono i gpt-oss, e un eventuale modello OpenAI vero rivenduto
-- avrebbe un nome della famiglia gpt-4/gpt-5/o1. Un solo assegnamento per
-- colonna: la rimozione e l'aggiunta si compongono in una espressione sola.
UPDATE nexus_model_selection_policy
   SET denied_patterns = array_cat(
           array_remove(denied_patterns, '^openai/'),
           ARRAY['^openai/gpt-[45]', '^openai/o[13]']
       ),
       updated_at = NOW()
 WHERE provider = 'groq'
   AND '^openai/' = ANY(denied_patterns);

-- L'allow deve nominarli, o resterebbero fuori per mancata ammissione: la
-- policy ammette solo cio' che un allow non vuoto elenca.
UPDATE nexus_model_selection_policy
   SET allowed_patterns = array_append(allowed_patterns, '^openai/gpt-oss'),
       updated_at = NOW()
 WHERE provider = 'groq'
   AND NOT ('^openai/gpt-oss' = ANY(allowed_patterns));
