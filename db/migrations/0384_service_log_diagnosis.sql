-- 0384_service_log_diagnosis.sql
--
-- Diagnosi LLM cross-tecnologia dei log dei servizi di progetto, in sostituzione
-- del pattern-matching hardcoded (detect_crash in service_observer.rs). Il
-- service_observer rileva in modo STRUTTURALE che un servizio non funziona
-- (porta non in ascolto dopo l'avvio / stato failed / restart-loop) e poi passa
-- le ultime righe di log a un LLM che le classifica, senza chiavi fisse
-- per-linguaggio. Funziona per Node, Python, .NET, Java, Go, Rust, React, ecc.
--
-- Tre oggetti DB-driven (regola G, niente hardcode nel codice):
--   1. purpose 'service_log_diagnosis' in nexus_purpose_model (tier-based: il
--      modello concreto lo sceglie il routing per tier, mig 0102/0203/0226).
--   2. template prompt 'system.service_log_diagnosis' in nexus_prompt_templates
--      (il prompt e' configurabile a caldo, CLAUDE.md sez. D: prompt fuori-chat).
--   3. setting 'agent.observer.readiness_grace_seconds' (attesa post-avvio prima
--      del readiness check, per non segnalare un servizio ancora in bind).
-- Idempotente.

BEGIN;

-- 1. Purpose tier-based: analisi log = lettura + ragionamento, nessun tool,
--    modello leggero (tier light). Il routing risolve provider/modello dal
--    catalog per tier+capability (resolve_purpose_model, internal_routing.rs).
INSERT INTO nexus_purpose_model
    (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
VALUES (
    'service_log_diagnosis',
    'google', 'gemini-2.5-flash',          -- ultimo fallback se il catalog e' vuoto (come understanding/intake_gate)
    'light', 'reasoning', false,
    'service_observer: classifica i log di un servizio non funzionante (cross-tecnologia, no pattern fissi). Tier light, reasoning, no tool use.'
)
ON CONFLICT (purpose) DO UPDATE SET
    tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = NOW();

-- 2. Prompt template (configurabile a caldo). Placeholder {logs} sostituito dal
--    codice con le ultime righe di log (gia' ripulite dagli escape ANSI).
INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES (
    'system.service_log_diagnosis',
    'system',
    'Service log diagnosis (cross-tech, no pattern fissi)',
    $PROMPT$Sei un analizzatore di log di servizi software. Ricevi le ultime righe di log di un servizio di progetto che, secondo un controllo STRUTTURALE (porta non in ascolto dopo l'avvio, oppure processo terminato/failed, oppure restart-loop), NON sta funzionando. Il servizio puo' essere di QUALSIASI tecnologia (Node, Python, .NET, Java, Go, Rust, PHP, React/Vite, ecc.): non assumere un linguaggio specifico, deducilo dai log.

Analizza i log e determina la causa del malfunzionamento. Rispondi ESCLUSIVAMENTE con un singolo oggetto JSON valido, senza testo prima o dopo, senza code fence. Campi:
- "is_error" (boolean): true se i log mostrano un errore/crash reale che impedisce al servizio di avviarsi o restare in ascolto; false se i log appaiono normali (nessun errore evidente: il problema potrebbe essere transitorio).
- "error_kind" (string): categoria sintetica e tecnologia-agnostica in snake_case, es. "dependency_missing", "syntax_error", "config_invalid", "port_in_use", "uncaught_exception", "build_error", "connection_refused", "permission_denied", "out_of_memory".
- "language" (string): linguaggio/runtime dedotto dai log (es. "node", "python", "dotnet", "java", "go", "rust", "unknown").
- "summary" (string): UNA frase in italiano con la causa radice e, se evidente, il file o punto coinvolto. Max 200 caratteri.
- "severity" (string): "error" se il servizio non puo' funzionare, "warning" se non bloccante.

LOG DEL SERVIZIO (ultime righe):
{logs}$PROMPT$,
    'migration_0384'
)
ON CONFLICT (key) DO NOTHING;

-- 3. Grace period readiness (secondi). Dopo l'avvio di un servizio, attendere
--    questo intervallo prima di considerare "porta non in ascolto" un problema
--    (evita falsi positivi mentre il processo fa il bind). Regola G: niente
--    hardcode nel codice, cache settings lato Rust.
INSERT INTO settings (key, value, category, description) VALUES (
    'agent.observer.readiness_grace_seconds',
    '12',
    'agent',
    'Secondi di attesa dopo l''avvio di un servizio di progetto prima che il service_observer consideri "porta non in ascolto" un malfunzionamento (readiness check TCP). Evita falsi positivi durante il bind.'
)
ON CONFLICT (key) DO NOTHING;

COMMIT;
