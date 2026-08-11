-- 0699 — La pagina che il gate misura e' quella che il RUN ha scritto, e si
--        risolve alla VERIFICA. La copertura nuova nasce in OSSERVAZIONE.
--
-- ROOT CAUSE (misurata l'11/08/2026, DUE forme con cause diverse). Il criterio
-- della resa statica (mig 0685) si costruisce in `build_native_engine`, cioe' a
-- t=0: prima dei nodi, prima dello stato iniziale, prima che il run abbia
-- scritto una riga. La pagina veniva risolta LI'.
--
--   FORMA 1 — il gate TACE. Progetto `test-11-08-listino`, nuovo e VUOTO.
--   L'agente scrive `listino.html`, che non funziona (Uncaught SyntaxError,
--   contenitore `productsGrid` con 0 figli, body di 90 caratteri). Il run chiude
--   «task complete ok». A t=0 l'albero e' vuoto: il rilevatore ritorna nulla, la
--   natura e' «senza pagina», il criterio NON NASCE. Non e' il nome del file:
--   `detect_static_entry` ha un terzo passo che ripiega sul primo `.html` della
--   radice e `listino.html` l'avrebbe trovata. E' il MOMENTO.
--
--   FORMA 2 — il gate misura il file SBAGLIATO, ed e' la piu' costosa. Progetto
--   `verifica-fix-10-08`: a t=0 esistono `index.html` (una todo app del giorno
--   prima: 1 elemento, body 234 caratteri) e `test-todo.html`. Il run produce
--   `galleria.html`, che FUNZIONA (6 card, body 885 caratteri). Il rilevatore,
--   al primo passo, trova l'entry canonica `index.html`, e il gate misura
--   QUELLA: 1 elemento contro `min_elements=5`, bocciata. In chat: «final_gate
--   non superata, nuovo tentativo 1/2», poi «chiusa al limite tentativi», con un
--   cambio di provider (mistral -> openrouter qwen3-235b, «passo a un modello
--   piu' capace») e 254.938 token spesi su un ciclo che non poteva convergere —
--   correggere `galleria.html` non fa crescere `index.html`.
--
-- DUE RIMEDI, E UNO SOLO NON BASTA. Spostare il solo MOMENTO chiude la forma 1 e
-- non la forma 2: a gate time i candidati sarebbero `index.html`,
-- `galleria.html` e `test-todo.html`, e il primo passo del rilevatore
-- sceglierebbe ancora `index.html`. Percio':
--   (a) la pagina si risolve al momento della VERIFICA, non a t=0;
--   (b) fra i candidati vince quella che il RUN ha scritto — fatto gia'
--       persistito in `file_mutations` (mig 0349), col percorso gia' relativo
--       alla radice. Il perimetro e' la SESSIONE, non il solo run: le scritture
--       di un sub-run portano il `run_id` del sub-run e la `session_id` del
--       padre, quindi il solo run perderebbe tutto il lavoro DELEGATO e
--       ricadrebbe sul rilevatore, cioe' rifarebbe la forma 2. E' lo stesso
--       confine, e la stessa ragione, di `MutationProgressPort`.
--   Dove il run non ha scritto pagine si ripiega sul rilevatore, DICHIARANDO
--   nell'evidenza che si tratta di un ripiego.
--
-- UNA SOLA pagina misurata, mai N. Con un `min_elements` unico, misurarne
-- diverse moltiplicherebbe i falsi rossi; e i consumatori del criterio lo
-- cercano con `.find(|c| c.criterion_type == ...)`, quindi N criteri
-- resterebbero VERDI misurando solo il primo — un cambiamento invisibile ai test
-- che dovrebbero fermarlo.
--
-- PERCHE' `observe` E NON `enforce`. Finche' la pagina era risolta a t=0 questo
-- criterio non nasceva quasi mai: pretendeva una pagina gia' sull'albero, e un
-- progetto generato da zero non ce l'ha. Risolverla alla verifica gli consegna
-- d'un colpo una popolazione di run che NESSUNO ha mai misurato, e accenderla
-- bocciando produrrebbe falsi rossi su codice sano di cui non conosciamo ne' la
-- forma ne' la frequenza. Un caso e' gia' prevedibile: una SPA scaffoldata
-- DURANTE il run, il cui `index.html` referenzia `/src/main.tsx` che la route di
-- anteprima serve come `application/octet-stream` — pagina vuota per
-- costruzione, e non per colpa dell'agente. In `observe` il criterio MISURA,
-- scrive l'evidenza per intero (cause comprese, piu' il campo `would_fail`) e
-- non produce mai un `Failed`. Si passa a `enforce` sui dati, non a intuito.
-- Vocabolario modellato su `orchestrator.critical_step_gate_mode` (mig 0677),
-- che risponde alla stessa domanda per un altro presidio.
--
-- LA CHIAVE BOOLEANA SPARISCE, e non e' pulizia: `static_render_enabled` e
-- `static_render_mode` risponderebbero alla stessa domanda («questo criterio
-- agisce?») in due posti, e il giorno che divergono una delle due mente con
-- l'aria di una configurazione (regole G e L). La modalita' e' l'unica fonte:
-- `off` e' cio' che `enabled=false` significava.
--
-- Punti unici: `nexus-agent-graph/src/decisions/pagina_del_run.rs` (quale pagina
-- di QUESTO run) e `static_render.rs` (`ModalitaResa`, il criterio, il
-- discriminante `classifica_natura` a cui il primo delega). Confine I/O:
-- `mcp-core/src/agent_graph_adapter/pagina_del_run.rs` (registro delle scritture
-- + rilevatore dell'albero). `detect_static_entry` NON e' toccata: resta il
-- punto unico di «qual e' l'entry di questo sito?», la domanda del pannello
-- Servizi, e qui e' il ripiego.
--
-- ROLLBACK: UPDATE settings SET value='off'
--           WHERE key='agent.final_gate.static_render_mode';
-- ACCENSIONE (dopo aver letto l'evidenza dei run in osservazione):
--           UPDATE settings SET value='enforce'
--           WHERE key='agent.final_gate.static_render_mode';

INSERT INTO settings (key, value, category, description, is_secret)
VALUES
  (
    'agent.final_gate.static_render_mode',
    'observe',
    'agent',
    'Quanto pesa sul run il criterio «l''app senza server mostra il proprio contenuto?». `off` = il criterio non nasce; `observe` = apre la pagina, misura e SCRIVE l''evidenza (cause comprese) senza mai bocciare; `enforce` = un verdetto negativo boccia il run. La pagina misurata e'' quella che il run ha SCRITTO (registro file_mutations), risolta al momento della verifica e non alla costruzione del motore; dove il run non ha scritto pagine si ripiega sul rilevamento dell''albero, dichiarandolo. Default `observe`: la risoluzione tardiva consegna al criterio una popolazione di run mai misurata prima, e si guarda l''evidenza prima di bocciare.',
    false
  )
ON CONFLICT (key) DO NOTHING;

-- La chiave booleana che questa modalita' sostituisce. Nessun codice la legge
-- piu': lasciarla significherebbe tenere in tabella una manopola che non
-- comanda nulla — la forma peggiore di configurazione, perche' chi la trova
-- crede di poterla usare.
DELETE FROM settings WHERE key = 'agent.final_gate.static_render_enabled';

-- Guard: la chiave nuova deve esistere e la vecchia no. Senza la prima il
-- codice degrada a criterio SPENTO (dichiarandolo in un WARN, regola G) e la
-- migrazione sarebbe passata a vuoto; con la seconda ancora in tabella
-- resterebbero due fonti per la stessa domanda.
DO $$
DECLARE
  modalita TEXT;
  vecchia INT;
BEGIN
  SELECT value INTO modalita FROM settings
   WHERE key = 'agent.final_gate.static_render_mode';
  IF modalita IS NULL THEN
    RAISE EXCEPTION 'mig 0699: manca agent.final_gate.static_render_mode';
  END IF;
  IF modalita NOT IN ('off', 'observe', 'enforce') THEN
    RAISE EXCEPTION
      'mig 0699: modalita'' fuori vocabolario (%): attesi off|observe|enforce', modalita;
  END IF;

  SELECT COUNT(*) INTO vecchia FROM settings
   WHERE key = 'agent.final_gate.static_render_enabled';
  IF vecchia <> 0 THEN
    RAISE EXCEPTION
      'mig 0699: agent.final_gate.static_render_enabled e'' ancora in tabella: due fonti per la stessa domanda';
  END IF;
END $$;

-- Guard: le chiavi RIUSATE devono esistere davvero. La soglia sul body e'
-- quella della 0685, l'attesa quella della 0681: se una delle due mancasse, il
-- criterio userebbe il proprio default in silenzio — il "magic fallback" che la
-- regola G vieta.
DO $$
DECLARE
  presenti INT;
BEGIN
  SELECT COUNT(*) INTO presenti
  FROM settings
  WHERE key IN (
    'agent.final_gate.static_render_min_elements',
    'agent.final_gate.browser_settle_ms'
  );
  IF presenti <> 2 THEN
    RAISE EXCEPTION
      'mig 0699: attese 2 chiavi riusate (min_elements mig 0685, browser_settle_ms mig 0681), trovate %', presenti;
  END IF;
END $$;
