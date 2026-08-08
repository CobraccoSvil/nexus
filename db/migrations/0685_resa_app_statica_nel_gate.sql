-- 0685 — L'app SENZA server entra nel final gate: aperta, non letta.
--
-- ROOT CAUSE (misurata l'08/08/2026 su gestione-corsi). Il gate approvava
-- un'applicazione statica guardando i FILE. `landing/index.html` (11637 byte,
-- generata in autonomia, approvata al TERZO tentativo) e' corretta: sei card
-- nascono all'avvio da `filterCourses('all')`, in fondo allo script — verificato
-- eseguendone il JavaScript con un DOM minimo. Ma il gate non lo sapeva, e non
-- poteva saperlo: nessun criterio attivo apriva quel file.
--
-- IL BUCO. Il contenuto di quella pagina non e' nel suo HTML: lo genera il
-- JavaScript. Una pagina cosi' puo' esistere, avere sintassi valida, superare
-- ogni controllo statico, e mostrare una griglia VUOTA per un errore a runtime
-- (variabile non definita, id sbagliato, `throw` prima dell'inizializzazione).
-- Guardando i byte i due casi sono indistinguibili.
--
-- PERCHE' LA MIG 0681 NON LO COPRE. Il criterio `browser_dialogue` nasce proprio
-- dalla lezione «curl non e' un browser», ed e' costruito attorno a un ENDPOINT
-- HTTP: osserva le richieste che la pagina manda al proprio backend. Un'app
-- statica non ha un servizio a cui chiedere e non manda richieste — quel
-- criterio, su questo caso, non ha nulla da misurare. La domanda e' un'altra:
-- non «la pagina ottiene i propri dati», ma «la pagina MOSTRA il proprio
-- contenuto». Serve un terzo criterio, non un'estensione del secondo.
--
-- I SEGNALI, e nessuno indovinato:
--   1. un'ECCEZIONE non gestita (`pageerror`): il codice ha lanciato. E' un
--      fatto, ed e' la forma esatta del difetto descritto sopra;
--   2. il CONTENITORE dichiarato dall'agente (`task_complete.rendered_container`)
--      e' rimasto vuoto. DICHIARATO e non dedotto: un `<div>` vuoto puo' essere
--      una griglia mai riempita o una finestra modale che si apre al click, e
--      sono lo stesso markup — indovinare sceglierebbe a caso fra un difetto e
--      un falso rosso;
--   3. il BODY reso e' sotto la soglia minima: il caso della SPA il cui bundle
--      non parte, dove `<div id="root"></div>` resta cio' che era.
-- Un `console.error` NON e' fra questi: una libreria che scrive un avviso non
-- rende rotta la pagina, e bocciare su quel segnale riporterebbe i rimandi a
-- vuoto. Entra nell'evidenza come contesto, mai nel verdetto.
--
-- IL DISCRIMINANTE E' DICHIARATO DAI FATTI, mai dedotto dal testo del task: un
-- progetto ha un servizio frontend con porta allocata (e allora la domanda
-- completa la pone gia' il dialogo), oppure ha una pagina rilevata da
-- `detect_static_entry` (e allora nasce questo criterio), oppure non ha
-- interfaccia. Dove la natura non e' `statica` il criterio NON NASCE — non
-- nasce e si dichiara inconcludente, perche' un inconcludente chiude il run
-- `completed_unverified` e un criterio inapplicabile non deve declassare i run
-- a cui non si applica.
--
-- L'INDIRIZZO e' la route `/preview/<project_id>/<entry>` di mcp-core, non un
-- `file:///`. E' la strada che l'utente percorre davvero quando apre la pagina
-- dal pannello Servizi, quindi la misura raggiunge il suo oggetto come la
-- produzione (regola O); e su `file:///` un `fetch('./dati.json')` legittimo
-- sarebbe bloccato dalla same-origin policy, cioe' il criterio inventerebbe un
-- difetto che sotto HTTP non esiste. L'URL base viene da `settings.mcp_core_url`
-- (mig 0190): senza, il criterio non nasce.
--
-- Punto unico del criterio: nexus-agent-graph/src/decisions/static_render.rs
-- (puro, e vi sta anche il discriminante `classifica_natura`); confine col
-- browser: mcp-core/src/agent_tools/browser_probe.rs, lo STESSO script del
-- dialogo — una sola esecuzione, due interpreti (regola L).
--
-- ROLLBACK: UPDATE settings SET value='false'
--           WHERE key='agent.final_gate.static_render_enabled';

INSERT INTO settings (key, value, category, description, is_secret)
VALUES
  (
    'agent.final_gate.static_render_enabled',
    'true',
    'agent',
    'Il final gate apre in un browser reale le app senza server (sito statico rilevato, nessun servizio frontend con porta allocata) e pretende che il contenuto sia RESO. Accerta cio'' che nessun controllo sul file puo'' vedere: una pagina il cui contenuto lo genera il JavaScript e'' valida anche quando quel JavaScript non gira. Il criterio nasce solo per i progetti la cui natura e'' statica.',
    false
  ),
  (
    'agent.final_gate.static_render_min_elements',
    '5',
    'agent',
    'Minimo di elementi di contenuto nel body reso (esclusi script, stili e metadati) sotto il quale la pagina non ha mostrato nulla. Copre la SPA il cui bundle non parte, dove il contenitore di montaggio resta vuoto senza lanciare nulla di osservabile. Non e'' un giudizio sul merito: accerta che qualcosa sia stato reso, non che sia fatto bene.',
    false
  )
ON CONFLICT (key) DO NOTHING;

-- L'attesa che la pagina si calmi NON ha una chiave propria: e'
-- `agent.final_gate.browser_settle_ms` (mig 0681), la stessa domanda per lo
-- stesso browser. Una seconda chiave sarebbe un secondo posto in cui la stessa
-- decisione puo' divergere (regola L).

-- Guard: le due chiavi devono esistere, altrimenti il codice le leggerebbe dal
-- proprio default (criterio SPENTO) e la migrazione sarebbe passata a vuoto.
DO $$
DECLARE
  presenti INT;
BEGIN
  SELECT COUNT(*) INTO presenti
  FROM settings
  WHERE key IN (
    'agent.final_gate.static_render_enabled',
    'agent.final_gate.static_render_min_elements'
  );
  IF presenti <> 2 THEN
    RAISE EXCEPTION 'mig 0685: attese 2 chiavi della resa statica, trovate %', presenti;
  END IF;
END $$;

-- Guard: la chiave riusata deve esistere davvero. Se la 0681 non fosse
-- applicata, il criterio userebbe il proprio default silenziosamente — ed e'
-- proprio il "magic fallback" che la regola G vieta.
DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM settings WHERE key = 'agent.final_gate.browser_settle_ms'
  ) THEN
    RAISE EXCEPTION
      'mig 0685: manca agent.final_gate.browser_settle_ms (mig 0681), riusata come attesa di questo criterio';
  END IF;
END $$;
