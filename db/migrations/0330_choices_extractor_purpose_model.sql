-- Migrazione 0330: purpose model 'choices_extractor' per il fallback della
-- feature "scelte di proseguimento" (meta_step next_actions).
--
-- Quando una risposta dell'agente NON contiene il blocco machine-readable
-- <suggested_actions> (vedi mig 0329) ma l'euristica leggera del brain rileva
-- che la risposta sembra proporre delle scelte, il brain invoca un modello
-- leggero per ESTRARRE quelle scelte in formato {label, prompt}.
--
-- Il modello viene risolto via nexus_purpose_model (regola G: nessun modello
-- hardcoded nel codice Python; brain/agents/next_actions.py::extract_via_llm usa
-- _routing_client_singleton().purpose_model(purpose='choices_extractor')).
--
-- Modello scelto: google/gemini-2.5-flash. Coerente con gli altri task interni
-- di estrazione/generazione strutturata (docs_generator mig 0327, google_batch
-- mig 0102): leggero, veloce ed affidabile su output JSON. Non e' un task
-- agentico (niente tool use), quindi un modello non-thinking veloce e' ideale.
--
-- Idempotente: ON CONFLICT DO NOTHING preserva eventuali override dell'admin.
--
-- Riferimenti:
--  - Schema tabella: db/migrations/0102_purpose_models_registry.sql
--  - Consumatore: brain/agents/next_actions.py

INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes)
VALUES (
    'choices_extractor',
    'google',
    'gemini-2.5-flash',
    'Estrattore scelte di proseguimento (fallback feature next_actions). '
    || 'Modello leggero per estrazione JSON {label, prompt}. Seed mig 0330.'
)
ON CONFLICT (purpose) DO NOTHING;
