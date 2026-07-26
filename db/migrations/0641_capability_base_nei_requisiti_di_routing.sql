-- 0641: la capability BASE dell'intent entra nei requisiti del promoter.
--
-- Causa radice dell'incidente "empty_completion su agentic_default": due tabelle
-- rispondevano alla stessa domanda ("quali capability servono per questo
-- intent?") con risposte diverse.
--
--   - nexus_intent_capability.base_capability  -> contratto INVARIANTE dell'intent
--   - nexus_intent_routing_requirements.required_capabilities -> per (intent, modo)
--
-- Su agentic_default i modi 'economica' e 'veloce' chiedevano solo {code}, mentre
-- l'intent dichiara 'reasoning' con la nota "tuttofare agentico con tool-loop".
-- Il promoter sceglieva quindi il modello piu' economico del tier privo di
-- reasoning (mistral-small-latest, 0.06/M): su un turno agentico con tool-loop
-- chiudeva a vuoto, e l'empty_completion faceva fallire il run intero.
-- La divergenza riguardava 19 righe su 7 intent, non il solo mistral.
--
-- Il punto unico della domanda vive ora in `load_requirements`
-- (crates/mcp-core/src/routing_matrix_auto_promoter.rs), che fa l'unione in
-- LETTURA: percio' questa migrazione non e' il meccanismo di enforcement, ma
-- l'igiene del dato storico, cosi' che le tabelle di configurazione mostrino nei
-- pannelli admin lo stesso requisito che il routing applica davvero.
--
-- L'allineamento e' DERIVATO dal dato, non una lista scritta a mano: aggiunge la
-- base_capability dove manca e non rimuove nulla. Idempotente.

UPDATE nexus_intent_routing_requirements r
   SET required_capabilities = r.required_capabilities || ARRAY[c.base_capability]
  FROM nexus_intent_capability c
 WHERE c.intent = r.intent
   AND COALESCE(c.base_capability, '') <> ''
   AND NOT (c.base_capability = ANY(r.required_capabilities));
