-- 0415_turn_recap_narrative.sql
--
-- Recap narrativo dei turni "hollow" (Fase D del flusso chat leggibile): quando
-- un run chiude senza una risposta utile (completamento vuoto) ma ha eseguito
-- azioni, oggi l'utente vede un recap DETERMINISTICO secco (lista di tool/file).
-- Con questo gate ABILITATO, un LLM leggero trasforma il recap in una breve
-- narrativa ("ho fatto X perche' Y; Z e' fallito, ho ripiegato su W; stato: ...").
--
-- Tre oggetti DB-driven (regola G, niente hardcode):
--   1. purpose 'turn_recap' in nexus_purpose_model (tier-based, modello leggero).
--   2. template 'system.turn_recap_narrative' in nexus_prompt_templates.
--   3. setting 'agent.chat.narrative_recap_enabled' (DEFAULT 'false', opt-in):
--      a gate spento il comportamento e' invariato (recap deterministico), quindi
--      nessun rischio runtime ne' costo LLM finche' l'admin non lo attiva.
--
-- Fallback robusto (regola H): se il gate e' off, il purpose non e' configurato,
-- o l'LLM fallisce/non risponde, si usa il recap deterministico di sempre.
-- Idempotente.

BEGIN;

-- 1. Purpose tier-based: sintesi breve = lettura + ragionamento, nessun tool,
--    modello leggero. Routing risolto da resolve_purpose_model (internal_routing.rs).
INSERT INTO nexus_purpose_model
    (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
VALUES (
    'turn_recap',
    'google', 'gemini-2.5-flash',          -- ultimo fallback se il catalog e' vuoto
    'light', 'reasoning', false,
    'chat: trasforma il recap deterministico di un run hollow in una breve narrativa leggibile. Tier light, reasoning, no tool use. Attivo solo se agent.chat.narrative_recap_enabled=true.'
)
ON CONFLICT (purpose) DO UPDATE SET
    tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = NOW();

-- 2. Prompt template (configurabile a caldo). Placeholder sostituiti dal codice:
--    {{recap}} = recap deterministico; {{actions}} = riassunto fatti dello step.
INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES (
    'system.turn_recap_narrative',
    'system',
    'Turn recap narrativo (run hollow -> sintesi leggibile)',
    $PROMPT$Sei un assistente che riassume in modo chiaro il lavoro svolto da un agente AI sviluppatore in un turno che si e' chiuso senza una risposta testuale propria (l'agente ha eseguito azioni ma non ha narrato l'esito).

Ti vengono forniti il recap deterministico delle azioni e i fatti strutturati del run. Produci una BREVE narrativa in italiano (massimo 4-5 frasi), in prima persona, che spieghi: cosa e' stato fatto, eventuali tentativi falliti e come sono stati gestiti, e lo stato finale. Sii concreto e onesto: NON inventare risultati non presenti nei dati. Se qualcosa e' fallito, dillo. Niente preamboli, niente elenco puntato secco: prosa scorrevole e sintetica.

RECAP DETERMINISTICO:
{{recap}}

FATTI DEL RUN:
{{actions}}$PROMPT$,
    'migration_0415'
)
ON CONFLICT (key) DO NOTHING;

-- 3. Gate (opt-in, default off): a spento il recap resta deterministico.
INSERT INTO settings (key, value, category, description) VALUES (
    'agent.chat.narrative_recap_enabled',
    'false',
    'agent',
    'Se true, i run "hollow" (completamento vuoto con azioni eseguite) ricevono un recap NARRATIVO generato da LLM (purpose turn_recap) invece del recap deterministico secco. Default false: opt-in, nessun costo/latenza LLM finche'' non attivato.'
)
ON CONFLICT (key) DO NOTHING;

COMMIT;
