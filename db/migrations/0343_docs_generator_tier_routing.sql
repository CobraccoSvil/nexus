-- 0343_docs_generator_tier_routing.sql
--
-- Routing per TIER del generatore documenti (nexus_doc_generate).
--
-- Problema: la riga `docs_generator` in nexus_purpose_model aveva `tier = NULL`,
-- quindi la risoluzione cadeva sempre sul (provider, model_id) STATICO
-- (google/gemini-2.5-flash) bypassando il routing: nessuna scelta per tier dal
-- catalog, nessun fallback automatico su cooldown/billing_error.
--
-- Fix: valorizziamo il `tier` cosi' che resolve_purpose_model[_db] (punto unico,
-- internal_routing.rs) selezioni dinamicamente il miglior modello del tier dal
-- catalog (ai_price_catalog) escludendo i provider in cooldown, con il
-- (provider, model_id) statico mantenuto SOLO come ultimo fallback.
--
-- Scelta tier = 'heavy': la generazione di documenti professionali (analisi
-- funzionale/tecnica IEEE 830, ecc.) e' output strutturato lungo e richiede un
-- modello capace; lo storico mostra che un modello lite (gpt-4.1-nano)
-- produceva sezioni vuote. requires_tool_use = false: il docs_generator emette
-- un JSON, non esegue un loop tool-use.
--
-- DB-driven (regola G): il tier e i fallback restano interamente configurabili
-- via questa tabella, nessun nome modello hardcoded nel codice.
-- Idempotente: l'UPDATE e' ripetibile senza effetti collaterali.

UPDATE nexus_purpose_model
SET tier = 'heavy',
    requires_tool_use = false,
    notes = 'Generatore documenti (nexus_doc_generate). Routing per tier=heavy '
            || '(output strutturato lungo, modello capace); google/gemini-2.5-flash '
            || 'resta come fallback statico. Mig 0343.',
    updated_at = NOW()
WHERE purpose = 'docs_generator';
