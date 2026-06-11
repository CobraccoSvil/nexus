-- 0397_continuity_gate_semantico.sql
--
-- Continuity gate semantico: contesto per-turno selezionato per PERTINENZA.
--
-- Domanda utente (2026-06-11): "non possiamo gestire questi provider in maniera
-- opportuna? Anche con provider piu' forti ci sarebbe uno spreco grande di token".
-- Causa: ogni turno trascina TUTTA la storia della sessione (compressa), anche
-- quando la richiesta corrente non c'entra nulla (es. "quante tabelle ci sono
-- nel db" su una sessione di 2 giorni di lavoro figma): token sprecati su OGNI
-- provider e deragliamenti sui modelli di riserva.
--
-- Fix (brain, helpers.apply_continuity_trim): al PRIMO turno del run si misura
-- la pertinenza semantica della richiesta corrente vs la storia (similarita'
-- coseno sull'embedding MiniLM LOCALE: millisecondi, zero LLM, language-
-- independent). Sotto soglia la history inline si riduce agli ultimi
-- keep_recent messaggi + un puntatore al recupero on-demand
-- (nexus_search_semantic, infrastruttura RAG gia' esistente). Fail-open:
-- errore/embedding assente -> nessun trim.
--
-- Idempotente.

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.context.continuity_gate_enabled',
    'true',
    'agent',
    'Se true, al primo turno di un run la history inline viene ridotta quando la richiesta corrente NON e'' semanticamente pertinente alla storia della sessione (cosine embedding locale sotto continuity_min_score). La storia resta recuperabile via nexus_search_semantic.'
)
ON CONFLICT (key) DO NOTHING;

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.context.continuity_min_score',
    '0.30',
    'agent',
    'Soglia di similarita'' coseno (0..1) sotto la quale la richiesta corrente e'' considerata argomento NUOVO rispetto alla storia della sessione (continuity gate). Piu'' alta = trim piu'' aggressivo.'
)
ON CONFLICT (key) DO NOTHING;

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.context.continuity_keep_recent',
    '2',
    'agent',
    'Numero di messaggi recenti della history mantenuti inline quando il continuity gate rileva un argomento nuovo.'
)
ON CONFLICT (key) DO NOTHING;
