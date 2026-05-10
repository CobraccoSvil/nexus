-- Migrazione 0127: regola anti-narrazione per tutti gli agenti Nexus.
--
-- Contesto: episodio osservato di "narrate-without-act" su una run reale.
-- Un agente ha prodotto ~4900 token di prosa con frasi del tipo "ora eseguo
-- edit_file", "procedo con la modifica", "eseguo definitivamente" ripetute
-- decine di volte, terminando con 0 step (zero tool call). Una seconda
-- invocazione ha poi applicato la modifica reale, ma l'utente non aveva
-- modo di distinguere lavoro effettivo da narrazione a vuoto.
--
-- Il blocco <anti_loop> esistente (mig 0086) copre solo "stesso file letto
-- due volte / stessa ipotesi non confermata", non il pattern di annuncio
-- senza azione. Aggiungiamo un blocco <anti_narration> in append a TUTTI
-- i template agente attivi (key LIKE 'agent.%') in una sola query.
-- Esclusi: system.*, docs.*, chat.*, quality.* — non sono agenti esecutivi
-- e in chat conversazionale la narrazione e' accettabile.
--
-- Aggiungiamo anche due chiavi nella tabella settings (categoria "agent")
-- per rendere le soglie UI configurabili dall'admin panel senza toccare
-- il codice.
--
-- Idempotente via sentinel string come la migrazione 0096.

DO $$
DECLARE
    sentinel TEXT := '<!-- 0127:anti_narration -->';
    rules_block TEXT := E'\n\n<!-- 0127:anti_narration -->\n<anti_narration>\nPattern vietato: annunciare un''azione senza eseguirla nello stesso turno.\n\n1. NIENTE FRASI DI ANNUNCIO RIPETUTE.\n   Mai produrre frasi tipo "ora eseguo X", "procedo con Y", "uso edit_file"\n   seguite da altro testo invece della tool call vera. Una frase di intent\n   massimo per ogni tool call. Se serve pianificare prima, fai prima la\n   tool call di lettura, POI riassumi cosa hai trovato.\n\n2. AUTO-DETECTION DEL LOOP DI NARRAZIONE.\n   Se ti accorgi di aver scritto 2+ frasi del tipo "sto per fare X" senza\n   che X sia stata effettivamente chiamata come tool, INTERROMPI immediatamente:\n     a) chiama il tool nel turno successivo, oppure\n     b) dichiara esplicitamente "non posso eseguirlo perche'' [motivo]" e fermati.\n   Mai continuare a produrre prosa quando l''azione e'' bloccata.\n\n3. NIENTE RI-LETTURE DELLO STESSO INTERVALLO.\n   Mai chiamare read_file/Read sullo stesso intervallo di righe gia'' letto\n   in questa run. Se hai gia'' i dati, modificali; se non bastano, leggi un\n   intervallo DIVERSO. Letture ridondanti sono indicatore di loop.\n\n4. PRIMA TOOL CALL ENTRO 500 TOKEN.\n   In una run di lavoro tecnico (modifica file, debug, test) la prima tool\n   call deve arrivare entro circa 500 token di output. Se hai bisogno di piu''\n   pianificazione, e'' segno che la richiesta non e'' chiara: chiedi un\n   chiarimento all''utente invece di narrare ipotesi.\n</anti_narration>';
BEGIN
    -- Applica a tutti i template agente attivi (agent.*) in una sola query.
    UPDATE nexus_prompt_templates
    SET content = content || rules_block
    WHERE key LIKE 'agent.%'
      AND is_active = TRUE
      AND content NOT LIKE '%' || sentinel || '%';

    RAISE NOTICE 'Migrazione 0127 applicata: regola anti-narrazione su tutti i template agent.*';
END
$$;

-- Soglie per il badge di narrazione nella UI (modificabili dall'admin panel).
-- Il frontend legge questi valori all'avvio e usa le costanti hardcoded come
-- fallback se l'API e' irraggiungibile.
INSERT INTO settings (key, value, category, description, is_secret)
VALUES
    (
        'agent_narration_warn_after_ms',
        '30000',
        'agent',
        'Millisecondi di run senza tool call dopo i quali il badge UI passa in stato warning (possibile loop di narrazione).',
        FALSE
    ),
    (
        'agent_narration_warn_after_chars',
        '1500',
        'agent',
        'Caratteri di testo streamed senza tool call dopo i quali il badge UI passa in stato warning.',
        FALSE
    )
ON CONFLICT (key) DO NOTHING;
