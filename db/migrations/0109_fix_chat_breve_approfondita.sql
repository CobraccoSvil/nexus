-- Migrazione 0109: corregge incoerenza in nexus_routing_matrix per chat_breve × approfondita.
--
-- Stato attuale (mig 0101): chat_breve × approfondita -> mistral/mistral-small-latest.
-- "Approfondita" promette al utente un modello capable, ma mistral-small non e'
-- in grado di orchestrare tool call complessi. Questa entry era seed letterale
-- dell'orchestrator.rs; rappresenta un bug, non una scelta di design.
--
-- Fix: sposta a anthropic/claude-haiku-4-5 (capable per tool use, costo basso).
-- Allineamento con chat_lunga × bilanciata che gia' usa claude-haiku.

UPDATE nexus_routing_matrix
SET provider = 'anthropic',
    model_id = 'claude-haiku-4-5-20251001',
    notes = COALESCE(notes, '') || ' | mig 0109: fix incoerenza chat_breve approfondita (era mistral-small, non capable per tool use)',
    updated_at = NOW()
WHERE intent = 'chat_breve'
  AND behavior_mode = 'approfondita'
  AND model_id = 'mistral-small-latest';

-- Allineamento simile per chat_lunga × veloce e docs × veloce: oggi puntano a
-- mistral-small-latest, ma se l'utente sceglie "veloce" su un task lungo o docs
-- breve si vuole comunque qualita' decente. Lasciamo sui modelli "lite" piu'
-- capable: gpt-4.1-nano per docs (output coerente), gemini-flash per chat lunga
-- (context window 1M).
UPDATE nexus_routing_matrix
SET provider = 'google',
    model_id = 'gemini-2.5-flash',
    notes = COALESCE(notes, '') || ' | mig 0109: chat_lunga veloce -> gemini-flash (1M ctx, tool use OK)',
    updated_at = NOW()
WHERE intent = 'chat_lunga'
  AND behavior_mode = 'veloce'
  AND model_id = 'mistral-small-latest';

UPDATE nexus_routing_matrix
SET provider = 'openai',
    model_id = 'gpt-4.1-nano',
    notes = COALESCE(notes, '') || ' | mig 0109: docs veloce -> gpt-4.1-nano (output strutturato migliore)',
    updated_at = NOW()
WHERE intent = 'docs'
  AND behavior_mode = 'veloce'
  AND model_id = 'mistral-small-latest';
