-- 0407: Materializza le settings FANTASMA (audit configurazioni 2026-06-11).
--
-- Chiavi LETTE dal codice ma assenti dal DB: i lettori usavano default
-- hardcoded che mascheravano l'assenza (violazione regola G). Con la riga in
-- DB diventano amministrabili dalla dashboard; i default qui sotto sono i
-- valori osservati nei call site (comportamento invariato).
--
-- Idempotente: ON CONFLICT (key) DO NOTHING.

INSERT INTO settings (key, value, category, description, is_secret, updated_at) VALUES
    ('agent.g1_max_nudges', '3', 'agent',
     'Numero massimo di nudge del gate G1 prima dell escalation (lettore: brain/agents/nodes/helpers.py::_load_g1_max_nudges, cache 60s).',
     false, NOW()),
    ('agent.mutations.max_track_bytes', '5242880', 'agent',
     'Dimensione massima (byte) di un file tracciato per il revert delle mutazioni; sopra soglia si salvano solo metadati (lettore: crates/mcp-core/src/file_mutations.rs).',
     false, NOW()),
    ('agent.verification_directive_text', 'L''utente ha chiesto ESPLICITAMENTE di verificare/testare il risultato. Prima di dichiarare completato DEVI eseguire davvero la verifica con una tool call concreta (es. run_command con curl/HTTP per provare il login o l''endpoint, oppure il comando di test pertinente) e riportare l''ESITO REALE osservato (codice di stato, output, successo/fallimento). NON inventare ne'' assumere il risultato. Se non puoi eseguire la verifica (strumento mancante, credenziali non disponibili, ambiente non avviabile), DICHIARALO esplicitamente spiegando cosa manca, invece di dare per scontato che funzioni.', 'agent',
     'Direttiva iniettata quando l utente chiede esplicitamente una verifica (lettore: brain/agents/nodes/helpers.py::_load_verification_directive; companion: agent.verification_directive_enabled).',
     false, NOW()),
    ('agent.wiki.qdrant_collection', 'wiki_content', 'embeddings',
     'Collection Qdrant unificata del wiki (ADR 0017 v2; lettore: crates/mcp-core/src/vector_memory.rs::qdrant_wiki_content_config).',
     false, NOW()),
    ('ollama_url', 'http://localhost:11434', 'providers',
     'Base URL del provider Ollama locale (lettore: brain/grpc_server/runtime.py; prima cadeva su env OLLAMA_URL/default hardcoded in violazione regola G).',
     false, NOW()),
    ('providers.google.thinking_budget', '8192', 'providers',
     'Budget di thinking token per i modelli Gemini thinking: clampato a [128, max_tokens] e sommato al tetto output per evitare hollow completion (lettore: brain/providers/google_provider.py).',
     false, NOW()),
    ('qdrant_code_index_collection', 'project_code_index', 'embeddings',
     'Collection Qdrant dell indice semantico del codice (lettore: crates/mcp-core/src/vector_memory.rs::qdrant_code_index_config).',
     false, NOW()),
    ('qdrant_conversation_context_collection', 'conversation_context', 'embeddings',
     'Collection Qdrant del contesto conversazionale (lettore: crates/mcp-core/src/vector_memory.rs::qdrant_conversation_context_config).',
     false, NOW()),
    ('qdrant_prompt_corrections_collection', 'prompt_corrections', 'embeddings',
     'Collection Qdrant delle prompt corrections (lettore: crates/mcp-core/src/vector_memory.rs::qdrant_config; sistema chat_learning).',
     false, NOW())
ON CONFLICT (key) DO NOTHING;

-- Fix wiring M14.4: il loader di clarify_or_expand_node legge SOLO
-- category='orchestrator' AND key LIKE 'clarify.%', ma la chiave era stata
-- seminata come kb.intake.confirm_if_implemented (categoria kb): mai caricata,
-- il gate girava sempre col default True. Rinominata per entrare nel contratto
-- del loader (il mapping elif e' aggiunto nello stesso change set).
UPDATE settings
SET key = 'clarify.confirm_if_implemented',
    category = 'orchestrator',
    description = 'M14.4: chiede conferma prima di rifare una richiesta gia'' implementata-e-verificata, anche in automatico (lettore: brain/agents/clarify_or_expand_node.py).'
WHERE key = 'kb.intake.confirm_if_implemented'
  AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'clarify.confirm_if_implemented');
