-- 0391_closure_judge_shadow.sql
--
-- WAVE 3.3 (de-lessicalizzazione governance chat): giudice LLM binario di
-- riserva per l'esito "lavoro compiuto / non compiuto" di un run d'azione,
-- quando il modello NON ha dichiarato l'esito via task_complete (WAVE 3) e i
-- segnali strutturali non sono netti.
--
-- Causa radice (regola H): l'esito incompiuto e' oggi inferito dal TESTO
-- dell'output con blacklist monolingua (_detect_unfulfilled_intent ~150 frasi
-- it/en + 2 regex morfologiche, resigned_patterns 16 frasi). Qualsiasi lingua
-- o stile nuovo le rompe -> Nexus non e' universale (la domanda di partenza).
--
-- Fix universale: il modello DICHIARA (task_complete, WAVE 3, gia' attivo);
-- dove la dichiarazione manca, un judge LLM language-independent giudica al
-- posto delle blacklist. MA non si promuove alla cieca (regola H, niente
-- toppe): questa migrazione introduce il purpose in modalita' SHADOW
-- (agent.closure_judge.shadow_enabled). In shadow il judge gira sui casi
-- ambigui, registra in telemetria il proprio verdetto E il disaccordo con la
-- blacklist (lexical_fallback) SENZA cambiare la decisione. Dopo ~2 settimane
-- di confronto (log closure_judge_disagreement) si valuta la promozione a fonte
-- ATTIVA e la rimozione delle blacklist. Niente rimozione cieca.
--
-- regola G: nessun modello hardcoded. Il purpose usa tier='light' +
-- required_capability='reasoning' (stesso schema di intent_classifier, mig 0338):
-- il router seleziona il miglior modello leggero dal catalog con cooldown/
-- fallback automatici; lo (provider, model_id) statico e' solo ultimo fallback
-- se il catalog e' vuoto.
--
-- Idempotente.

BEGIN;

INSERT INTO nexus_purpose_model
    (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
VALUES (
    'closure_judge', 'google', 'gemini-2.5-flash', 'light', 'reasoning', false,
    'Giudice binario chiusura run (mig 0391, WAVE 3.3). Risolto via tier=light+reasoning dal router (regola G). Decide "lavoro compiuto si/no" in modo language-independent quando manca task_complete; sostituira'' le blacklist lessicali _detect_unfulfilled_intent/resigned_patterns dopo la finestra di confronto telemetrico. Lo statico google/gemini-2.5-flash e'' solo ultimo fallback.'
)
ON CONFLICT (purpose) DO UPDATE
SET tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = now();

-- Interruttore SHADOW (default true): il judge gira solo per raccogliere la
-- telemetria di confronto, non decide. Si disattiva per debug locale o per
-- annullare il costo LLM extra; mai per produzione finche' si raccolgono dati.
INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.closure_judge.shadow_enabled',
    'true',
    'agent',
    'WAVE 3.3: abilita il giudice LLM di chiusura in modalita'' SHADOW (registra verdetto e disaccordo con le blacklist lessicali senza cambiare la decisione). Prerequisito per la futura rimozione delle blacklist _detect_unfulfilled_intent/resigned_patterns. Cache 60s lato brain.'
)
ON CONFLICT (key) DO NOTHING;

-- Soglia minima di lunghezza del result per attivare il judge: sotto questa
-- soglia il run e' (quasi) vuoto e l'esito e' gia' deciso a monte (hollow/soft).
-- Evita chiamate LLM inutili su risultati banali. DB-driven (regola G).
INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.closure_judge.min_result_chars',
    '40',
    'agent',
    'WAVE 3.3: lunghezza minima (caratteri) del result perche'' il closure_judge venga invocato in shadow. Sotto soglia l''esito e'' gia'' deciso dai gate hollow/soft a monte.'
)
ON CONFLICT (key) DO NOTHING;

COMMIT;
