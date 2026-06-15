-- 0443: estrazione nel DB dei prompt Python ad alto impatto (regola G/D).
--
-- B5 system.closure_judge        <- closure_judge.py (giudice "task compiuto?")
-- B10 system.choices_extractor   <- next_actions.py::_build_extractor_prompt
--
-- Entrambi hanno struttura "istruzioni fisse + variabili interpolate": il
-- template DB usa placeholder {{...}} che il codice sostituisce a runtime. Il
-- codice legge via prompt_registry.get_prompt con FALLBACK alla costruzione
-- hardcoded (graceful degradation se DB down). Caricabili dal brain grazie al
-- loader esteso ai system.% (mig 0441).
--
-- Idempotente: ON CONFLICT (key) DO NOTHING.

INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES
(
    'system.closure_judge',
    'system',
    'Giudice semantico: la richiesta utente risulta portata a termine?',
    $judge$Sei un valutatore neutrale. Ti vengono dati: la RICHIESTA di un utente a un agente software e la RISPOSTA FINALE dell'agente. Giudica SOLO se la richiesta risulta portata a termine in base alla risposta, ignorando la lingua e lo stile. Una risposta che rimanda il lavoro, dichiara di non poter procedere, chiede all'utente di fare lui, o promette un'azione futura non ancora svolta NON e' compiuta. Una risposta che riporta un lavoro effettivamente svolto (anche con limiti dichiarati) e' compiuta.

REGOLA AGGIUNTIVA: una risposta che ELENCA esplicitamente passi ANCORA da svolgere come parte del compito richiesto (es. una sezione "Prossimi passi necessari", "Next steps", "Remaining steps", "Da fare", "TODO" con due o piu' item numerati o puntati) NON e' compiuta, anche se descrive lavoro gia' svolto: il task resta APERTO finche' quei passi non sono eseguiti. Solo passi opzionali / di follow-up extra (chiaramente etichettati come tali, non parte del compito richiesto) non bloccano.

Rispondi ESCLUSIVAMENTE con un oggetto JSON, senza testo attorno:
{"fulfilled": true|false, "reason": "<max 12 parole>"}

RICHIESTA:
{{task}}

RISPOSTA FINALE:
{{result}}

JSON:$judge$,
    'migration_0443'
),
(
    'system.choices_extractor',
    'system',
    'Estrattore di scelte cliccabili dalla risposta dell assistente (fallback LLM)',
    $extract$Sei un estrattore. Ti viene data la risposta di un assistente AI.
Se la risposta propone all'utente delle SCELTE su come proseguire (opzioni, varianti, prossimi passi suggeriti), estraile.

Restituisci ESCLUSIVAMENTE un array JSON, senza testo aggiuntivo, nel formato:
[{"label":"<testo breve del pulsante, max 40 caratteri>","prompt":"<istruzione completa e non ambigua, pronta da inviare come messaggio utente per proseguire con quella scelta>"}]

Regole per il campo `prompt` (CRITICHE: un prompt mal posto confonde l'assistente che lo ricevera' e lo costringe a chiedere chiarimenti invece di agire):
- Scrivilo come ISTRUZIONE COMPLETA e NON AMBIGUA in italiano, in seconda persona verso l'assistente (es. 'Descrivimi...', 'Genera...', 'Modifica...').
- Dichiara SEMPRE in modo esplicito l'OUTPUT ATTESO e l'OGGETTO preciso (quale sezione/elemento/file e con quale obiettivo), cosi' l'assistente possa eseguire SENZA chiedere chiarimenti.
- VIETATE le formule vaghe come 'approfondisci', 'parlami di', 'esplora la proposta', 'vorrei capire meglio': non dicono cosa produrre. Trasformale in richieste concrete (es. invece di 'approfondisci la Hero Section' -> 'Descrivimi in dettaglio come rinnovare la Hero Section: struttura, contenuti, stile e testo della call-to-action').
- Se la scelta e' una spiegazione/discussione e NON una modifica al codice, esplicitalo aggiungendo in coda: 'Per ora forniscimi solo la proposta dettagliata, senza modificare i file.'
- label: conciso, orientato all'azione, in italiano (max 40 caratteri).
- Se la risposta NON propone scelte, restituisci esattamente: []
- Massimo 6 scelte.

RISPOSTA DELL'ASSISTENTE:
<<<
{{assistant_text}}
>>>$extract$,
    'migration_0443'
)
ON CONFLICT (key) DO NOTHING;
