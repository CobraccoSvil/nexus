-- Migrazione 0209: clarify tecnico/prodotto RAG-informed (Cluster 4).
--
-- Due capacita', entrambe gated default-OFF:
-- 1. Lookup decisione gia' presa: prima di chiedere un chiarimento, cerca tra
--    le note intent=decision (via il servizio di ricerca vettoriale esistente
--    /api/internal/knowledge/search) se la decisione e' gia' stata presa; se
--    si', la applica e prosegue SENZA interrompere.
-- 2. Conferma decisioni di prodotto/irreversibili anche in automatico: il
--    clarify classifica la richiesta (technical/product/irreversible). Le
--    decisioni di prodotto o irreversibili chiedono conferma all'utente ANCHE
--    in modalita' automatica (umano-in-the-loop strategico). Le decisioni
--    tecniche/reversibili proseguono autonome come oggi.
--
-- Le chiavi usano il prefisso clarify.* (lette da clarify_or_expand_node._load_config),
-- categoria orchestrator. Nessun nuovo purpose: la classificazione sta nello
-- stesso tool_use di clarify_expand (modello gia' tier light).

INSERT INTO settings (key, value, category, description) VALUES
    ('clarify.decision_lookup_enabled', 'false', 'orchestrator',
     'Se true, prima di chiedere un chiarimento cerca se la decisione e'' gia'' stata presa (note intent=decision) e la applica.'),
    ('clarify.decision_min_score', '0.7', 'orchestrator',
     'Soglia minima di similarita'' per considerare una decisione passata come gia'' presa.'),
    ('clarify.decision_topk', '5', 'orchestrator',
     'Quante note decision recuperare nel lookup.'),
    ('clarify.confirm_irreversible_in_auto', 'false', 'orchestrator',
     'Se true, le decisioni di prodotto/irreversibili chiedono conferma anche in modalita'' automatica; le tecniche/reversibili proseguono autonome.')
ON CONFLICT (key) DO NOTHING;
