-- 0733_default_provider_su_modelli_vivi.sql
--
-- Il default di un fornitore non puo' puntare a un modello che il catalogo ha
-- spento: quel default e' cio' che il probe di salute interroga, e un modello
-- morto faceva risultare malato un endpoint che rispondeva 200.
--
-- MISURATO il 17/08/2026 sul DB vivo, DUE fornitori su nove:
--   groq       -> llama-3.1-8b-instant  (is_enabled=false, disqualified dal
--                 15/07 per tool_smoke fallito, e rimosso dal fornitore:
--                 HTTP 404 model_not_found)
--   perplexity -> sonar                 (is_enabled=false, unqualified)
-- Conseguenza osservata nel pannello: groq `not_found` mentre il probe diretto
-- su openai/gpt-oss-20b rispondeva HTTP 200.
--
-- Il codice non si fida piu' di questa tabella (provider_health_probe::
-- modello_per_probe sostituisce un default spento con un modello vivo e lo
-- dichiara), ma il dato va comunque allineato: il default e' anche il modello
-- del routing statico, e lasciarlo puntare nel vuoto significa tenere in giro
-- una configurazione falsa che il ripiego rende soltanto innocua.
--
-- La scelta e' DERIVATA dai fatti, non scritta a mano: per ogni fornitore il
-- cui default non e' abilitato si prende il modello abilitato piu' economico —
-- lo stesso criterio del ripiego a runtime, cosi' le due risposte coincidono.
-- I fornitori senza alcun modello abilitato restano com'erano: li' non c'e'
-- niente di meglio da scrivere, e il codice dichiara `NessunoAbilitato`.

UPDATE nexus_provider_default_model d
   SET model_id = vivo.model,
       updated_at = now()
  FROM (
        SELECT DISTINCT ON (c.provider) c.provider, c.model
          FROM ai_price_catalog c
         WHERE c.is_enabled = true
         ORDER BY c.provider,
                  c.input_cost_per_million_tokens ASC NULLS LAST,
                  c.model
       ) AS vivo
 WHERE vivo.provider = d.provider
   AND NOT EXISTS (
         SELECT 1 FROM ai_price_catalog c2
          WHERE c2.provider = d.provider
            AND c2.model = d.model_id
            AND c2.is_enabled = true
       );
