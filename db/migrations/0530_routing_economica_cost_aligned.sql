-- 0530: allinea al COSTO le priority delle righe behavior_mode='economica' della
-- routing matrix di fallback. Config stantia rilevata da un audit del flusso di
-- selezione: in 'economica' vinceva (priority piu' alta) il modello piu' CARO
-- perche' la priority era slegata dal costo (es. debug/economica -> claude-sonnet
-- 3.0 $/M invece di deepseek-v4-flash 0.14 = 21x; agentic_default/economica ->
-- gpt-4.1 2.0 invece di deepseek-v4-flash 0.14). La matrix e' oggi solo il
-- FALLBACK del routing (il sistema gira in nexus_behavior_mode='dinamico', che usa
-- il catalog dinamico gia' cost-first), ma se il fallback scatta 'economica' deve
-- servire l'economico, coerente col suo nome e con l'obiettivo costi.
--
-- Meccanica: per ogni intent, la riga 'economica' col costo input minore
-- (dal catalog vivo) prende la priority piu' alta; le altre scalano. Include
-- DELIBERATAMENTE anche le righe con manual_override=true: per il mode
-- 'economica' il costo E' il criterio definitorio, quindi una priority che
-- premia il modello piu' caro (36 righe su 49 avevano override che servivano
-- gpt-4.1/claude-sonnet a 2-3 $/M al posto di deepseek-v4-flash a 0.14) e' config
-- errata a prescindere dal flag. Il flag manual_override NON viene rimosso (solo
-- la priority e' allineata): l'intento umano di "riga gestita a mano" resta, ma
-- l'ordinamento torna coerente col nome del mode. Non tocca:
--   - i behavior_mode diversi da 'economica' (bilanciata/approfondita/veloce);
--   - le righe il cui model_id non e' nel catalog (nessun costo per ordinarle);
--   - escalation_provider / escalation_model_id / le soglie (vedi nota sotto).
--
-- NOTA sulle soglie escalation-by-size (escalation_threshold_tokens): 106 righe
-- hanno soglie 16k-100k mai raggiungibili dalla stima attuale (estimate_complexity
-- cap ~6400). NON vengono toccate qui DELIBERATAMENTE: il meccanismo e' sano (task
-- grande -> modello piu' capace), il difetto e' la STIMA troppo grezza; abbassare
-- le soglie senza prima migliorare la stima farebbe scattare escalation-by-size
-- ingiustificate (l'opposto dell'obiettivo). Restano dormienti finche' la stima
-- token non riflette il contesto reale (fix separato del routing iniziale).
WITH ranked AS (
    SELECT m.id,
           row_number() OVER (
               PARTITION BY m.intent
               ORDER BY c.input_cost_per_million_tokens ASC, m.model_id ASC
           ) AS rk
    FROM nexus_routing_matrix m
    JOIN ai_price_catalog c
      ON c.provider = m.provider AND c.model = m.model_id
    WHERE m.is_active
      AND m.behavior_mode = 'economica'
)
UPDATE nexus_routing_matrix m
   SET priority = 100 - ranked.rk,
       updated_at = now()
  FROM ranked
 WHERE m.id = ranked.id
   AND m.priority <> 100 - ranked.rk;
