-- 0413_compact_structured_decisions.sql
--
-- Chiude la Fase 2 della continuita': la compattazione di sessione
-- (compact_session_core) ora produce un output STRUTTURATO e alimenta il
-- worklog con eventi 'decision'. Finora il digest del worklog (render_digest)
-- aveva gia' la sezione "Decisioni:" ma nessuno scriveva eventi kind='decision':
-- le decisioni architetturali/di processo restavano solo nel testo libero del
-- riassunto, perse alla compattazione successiva.
--
-- Due oggetti DB-driven (regola G/D, niente hardcode nel codice):
--   1. template 'system.session_compact_structured' in nexus_prompt_templates:
--      sposta in DB il prompt di compattazione (era inline in chat_sessions.rs)
--      e chiede un JSON {summary_markdown, decisions[]}. Il summary_markdown
--      resta la memoria di sessione (embed + prompt_corrections); le decisions
--      durature diventano eventi worklog 'decision' (source='distilled').
--   2. setting 'agent.worklog.compact_writes_decisions': gate dell'estrazione
--      (se off, la compattazione resta testuale come prima).
--
-- Fallback robusto (regola H): se il modello non produce JSON valido, il codice
-- usa il testo grezzo come riassunto, esattamente come prima di questa migrazione.
-- Idempotente.

BEGIN;

-- 1. Prompt di compattazione strutturato (fuori-chat -> esplicito, regola D).
--    Nessun placeholder: la conversazione da riassumere e' gia' nei messaggi
--    precedenti; questo e' l'ultimo messaggio-istruzione.
INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES (
    'system.session_compact_structured',
    'system',
    'Session compact strutturato (riassunto + decisioni per il worklog)',
    $PROMPT$Riassumi questa conversazione di lavoro tra utente e agente AI sviluppatore, preservando le informazioni operative critiche per continuare senza perdere contesto.

Rispondi ESCLUSIVAMENTE con un singolo oggetto JSON valido, senza testo prima o dopo, senza code fence:
{
  "summary_markdown": "<riassunto conciso in markdown con bullet: decisioni prese, cambiamenti al codice, file toccati, errori e fix, stato del task>",
  "decisions": ["<UNA decisione DURATURA e riutilizzabile: architetturale, scelta di libreria/stack, convenzione, di processo. Max 200 caratteri>"]
}

Regole per "decisions": elenca SOLO decisioni durature e generali del lavoro (max 6), MAI fatti episodici (un singolo file modificato, un comando una-tantum, lo stato istantaneo). Se non emergono decisioni durature, usa una lista vuota. Il campo "summary_markdown" e' invece il riassunto COMPLETO e va sempre valorizzato.$PROMPT$,
    'migration_0413'
)
ON CONFLICT (key) DO NOTHING;

-- 2. Gate dell'estrazione decisioni (regola G, cache settings lato Rust).
INSERT INTO settings (key, value, category, description) VALUES (
    'agent.worklog.compact_writes_decisions',
    'true',
    'agent',
    'Se true, la compattazione di sessione usa il prompt strutturato (system.session_compact_structured) ed estrae le decisioni durature come eventi worklog kind=decision. Se false, la compattazione resta testuale (comportamento legacy).'
)
ON CONFLICT (key) DO NOTHING;

COMMIT;
