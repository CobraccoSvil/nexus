-- 0393_fallback_prompt_adapt.sql
--
-- Adattamento del prompt al modello di FALLBACK (cascade registry).
--
-- Causa radice (caso reale 2026-06-11, run 7a4a1dca su Beauty-Book): quando la
-- cascade fa fallback (es. mistral-large timeout -> deepseek-v4-pro), il turno
-- viene rigirato al modello subentrante IDENTICO: stesso prompt, stessi 12 tool,
-- stesse istruzioni pensate per il modello pianificato dal router. Un modello di
-- riserva meno capace, davanti a un task complesso ereditato a meta', degenera
-- in esplorazione senza sintesi (15 letture -> abort anti-loop, 70K token).
--
-- Il sistema GIA' adatta i limiti QUANTITATIVI al modello (max_tokens clampato
-- dal catalog, soglie contesto rapportate a context_window, forced-RAG, upscale
-- finestra): questa migrazione aggiunge l'adattamento QUALITATIVO minimo — una
-- direttiva di lavoro iniettata una sola volta quando un fallback subentra, che
-- impone passi piccoli e chiusura onesta invece di ereditare silenziosamente un
-- compito tarato su un modello piu' capace.
--
-- DB-driven (regola G): flag + testo configurabili, nessun hardcode nel codice
-- (default identici nel codice solo come fallback se le chiavi mancano).
-- Idempotente.

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.fallback.adapt_enabled',
    'true',
    'agent',
    'Se true, quando la cascade provider fa fallback su un modello diverso il turno viene adattato: una direttiva di lavoro (passi piccoli, una tool call alla volta, chiusura onesta) viene iniettata una sola volta per turno. Evita che il modello di riserva erediti pari-pari un task tarato su un modello piu'' capace.'
)
ON CONFLICT (key) DO NOTHING;

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.fallback.adapt_directive_text',
    'Sei subentrato come modello di riserva su questo task dopo un fallimento del modello precedente. Adatta il lavoro alle tue capacita'': procedi in passi PICCOLI e concreti, una sola tool call alla volta; non ripetere letture o esplorazioni gia'' fatte (i risultati sono nella cronologia); appena hai informazioni sufficienti produci il risultato concreto. Se il task e'' troppo ampio per un solo turno, completa la prima parte utile e dichiara esplicitamente cosa resta da fare (usa task_complete se disponibile). Non descrivere il lavoro: eseguilo.',
    'agent',
    'Testo della direttiva di adattamento iniettata al fallback (placeholder {model} sostituito col modello subentrante se presente). Vedi agent.fallback.adapt_enabled.'
)
ON CONFLICT (key) DO NOTHING;
