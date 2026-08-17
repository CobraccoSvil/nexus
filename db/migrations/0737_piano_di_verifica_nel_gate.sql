-- 0737 — Il piano di verifica e' un PRODOTTO del run, non un catalogo di casi previsti
--
-- ROOT CAUSE. Il final gate ha SETTE criteri, ognuno con la sua domanda cablata:
-- il server risponde? la pagina mostra contenuto? la suite passa? lo stile
-- dichiarato e' applicato? il codice prodotto si carica (mig 0734)? Ogni volta
-- che il sistema ha sbagliato in un modo NUOVO, il rimedio e' stato aggiungere
-- una voce.
--
-- MISURATO il 17/08/2026: su un progetto senza porte il gate ha dichiarato
-- «passato» DUE volte su un run che aveva prodotto un file di test non
-- eseguibile (`ReferenceError: describe is not defined`). Non aveva niente da
-- chiedere. Il catalogo e' incompleto PER COSTRUZIONE: nessuna lista conterra'
-- mai «crea un libro via POST, rileggilo via GET, controlla che sia nella
-- tabella, cancellalo e verifica che sparisca». Quella prova la sa scrivere solo
-- chi conosce il task.
--
-- E IL SISTEMA LA SA GIA' SCRIVERE. Per lo stesso task il Consiglio aveva emesso
-- 17 requisiti — fra cui il rischio ESATTO del difetto — ma in PROSA, e il
-- riscontro ha potuto dire soltanto `applicati=0, non_applicati=2,
-- non_verificabili=15`. Quindici requisiti giusti e inerti. Non e' un caso
-- isolato: sul parco progetti sono 89 requisiti unici e UNO SOLO porta un
-- letterale cercabile (misurato il 10/08/2026, `advisory_requirements`).
--
-- IL SALTO: far emettere PROVE ESEGUIBILI invece di frasi, ed eseguirle. Il
-- modello PROPONE la prova, la MACCHINA emette il verdetto — nessuna attesa
-- ammette un giudizio del modello: codice d'uscita, testo presente, testo
-- assente. E' la generalizzazione di cio' che `task_complete.endpoints` gia' fa.
--
-- IL PAVIMENTO RESTA e non passa di qui. Le tre domande universali — il codice
-- prodotto si carica (0734), il servizio con una porta allocata risponde
-- (endpoint_probes), la pagina non e' vuota (0685) — sono criteri PROPRI del
-- gate e restano tali. Tre ragioni misurate: il silenzio non e' innocuo (il run
-- che non dichiara nulla e' tipicamente quello in difficolta'); chi ha sbagliato
-- non conosce il proprio errore (l'agente che ha scritto il test Jest in un
-- progetto senza Jest non si sarebbe mai autoimposto «verifica che il test
-- parta»); giudice != worker e' gia' regola di casa.
--
-- CHI SCRIVE IL PIANO, in ordine di preferenza e con la provenienza in ogni
-- prova: (1) il Consiglio delle Competenze e (2) il panel multi-provider, che
-- deliberano PRIMA del lavoro e non hanno scritto il codice; (3) l'agente
-- esecutore, in coda, che puo' solo AGGIUNGERE prove — la dedup conserva la
-- PRIMA provenienza, quindi non puo' intestarsi ne' sostituire la prova di chi
-- lo giudica.
--
-- DEVIAZIONI DAL DESIGN, dichiarate:
--
--   * NIENTE attesa `Http`. La prova HTTP ha gia' il suo punto unico —
--     `task_complete.endpoints` -> `endpoint_probes` -> criterio `http`, col
--     proprio vocabolario di status accettati, il proprio client e la propria
--     attesa di readiness. Riprodurla qui sarebbe una SECONDA strada per la
--     stessa domanda, con due idee di «2xx accettabile» destinate a divergere
--     (regola L). Chi deve provare un endpoint lo dichiara dove il gate gia' lo
--     chiama.
--
--   * NIENTE origine `Pavimento` e NIENTE origine `Revisore` nel vocabolario.
--     Il pavimento non e' una prova dichiarata (sono criteri che esistono gia');
--     il ciclo di review e' un gate di CHIUSURA gemello del final gate, non lo
--     precede, quindi le sue prove non raggiungerebbero alcun esecutore. Una
--     variante senza produttore e' vocabolario inerte.
--
-- SICUREZZA — il piano NON e' un canale privilegiato. Una prova e' un
-- `run_command` a tutti gli effetti e viene CLASSIFICATA come tale dal punto
-- unico del gate duale (`step_gate::classify_step`), con lo stesso vocabolario
-- DB: nessun secondo elenco di comandi pericolosi nasce qui.
--
-- Il gate duale NON e' convocabile da un criterio: il `criteria_runner` non ha
-- la porta di validazione e non puo' chiedere un parere a due fornitori.
-- Restano tre strade e due sono chiuse — eseguire tutto e' il canale
-- privilegiato che il design vieta; rifiutare tutto rende il criterio inerte,
-- perche' `run_command` e' `unconfined` per CONTRATTO del tool e il suo
-- pavimento e' quindi `critical`. La terza e' la soglia qui sotto.
--
-- LIMITE DICHIARATO: con la soglia di default (`critical`) una prova
-- classificata `critical` — per esempio una migrazione di schema travestita da
-- prova — VIENE eseguita. Cio' che le regole lessicali marcano `irreversible`
-- (`rm -rf`, `DROP`, `TRUNCATE`, ...) non viene eseguito affatto. Chi vuole
-- chiudere anche il residuo porta la soglia a `observation`: da li' passano le
-- sole righe fatte di comandi del vocabolario di osservazione, al prezzo di
-- rifiutare quasi ogni prova utile. E' una decisione dell'amministratore, e sta
-- nel DB perche' possa prenderla senza un deploy.
--
-- Punto unico del criterio:
--   crates/nexus-agent-graph/src/decisions/piano_di_verifica.rs
-- Il criterio del gate lo costruisce `mcp-core::native_engine::criterio_piano_verifica`;
-- il PIANO lo inietta `FinalGateNode::build_criteria` (l'unico punto che vede lo
-- stato del run); l'unico I/O sta in `criteria_runner::check_piano_verifica`.
--
-- ROLLBACK: UPDATE settings SET value = 'false'
--            WHERE key = 'agent.final_gate.piano_verifica_enabled';

INSERT INTO settings (key, value, description, category)
VALUES (
    'agent.final_gate.piano_verifica_enabled',
    'true',
    'Final gate: esegue le PROVE che gli apparati advisory e l''agente hanno '
    'dichiarato per questo run, e ne giudica l''esito in modo meccanico (codice '
    'd''uscita, testo presente, testo assente). Nessuna attesa ammette un '
    'giudizio del modello. Boccia SOLO una prova osservata e non conforme; una '
    'prova che non si e'' potuta eseguire non boccia e viene dichiarata.',
    'agent'
)
ON CONFLICT (key) DO UPDATE
    SET value = EXCLUDED.value,
        description = EXCLUDED.description;

-- La SOGLIA di ammissione. Vocabolario di `step_gate::StepCriticality`
-- (read_only < mutating < critical < irreversible): il valore e' il livello PIU'
-- ALTO ancora ammesso. Un valore fuori vocabolario NON e'' un default: il
-- criterio nasce, non esegue nulla e dichiara di non aver potuto misurare —
-- fallire verso la cautela, mai verso il silenzio.
INSERT INTO settings (key, value, description, category)
VALUES (
    'agent.final_gate.piano_prova_criticita_max',
    'critical',
    'Final gate / piano_di_verifica: livello di criticita'' PIU'' ALTO ancora '
    'ammesso per una prova, classificata col punto unico del gate duale come se '
    'fosse il run_command che e''. Vocabolario: read_only|mutating|critical|'
    'irreversible. Default `critical`: cio'' che le regole lessicali marcano '
    'irreversibile non viene eseguito. Portarlo a `observation` non e'' un valore '
    'valido — usare `read_only`, che ammette le sole righe di soli comandi del '
    'vocabolario `orchestrator.step_reach.observation_commands`. Valore fuori '
    'vocabolario = nessuna prova eseguita, dichiarato.',
    'agent'
)
ON CONFLICT (key) DO UPDATE
    SET value = EXCLUDED.value,
        description = EXCLUDED.description;

INSERT INTO settings (key, value, description, category)
VALUES (
    'agent.final_gate.prova_timeout_s',
    '60',
    'Final gate / piano_di_verifica: quanto il GATE attende UNA prova. Bound '
    'sull''attesa, non sul processo: `run_command` non ha un timeout nel proprio '
    'contratto d''ingresso e il cap del processo lo applica il tool runner. '
    'Scaduta l''attesa la prova resta NON ESEGUITA (mai fallita: un comando che '
    'non risponde non e'' un difetto del codice).',
    'agent'
)
ON CONFLICT (key) DO UPDATE
    SET value = EXCLUDED.value,
        description = EXCLUDED.description;

-- I MANDATI DELLE FIGURE (pattern della 0621/0677/0706). Il campo `prove` nello
-- schema del tool non basta da solo: il template e'' cio'' che la figura legge
-- PRIMA di analizzare, ed e'' li'' che si decide se emettera'' una frase o un
-- comando. Il tool descrive COME si dichiara, il mandato dice CHE COSA vale la
-- pena dichiarare.
--
-- REPLACE mirato sul blocco `<output_format>` che tutte le figure del consiglio
-- condividono (seed 0546, riscritto dalla 0621): i template che non lo portano
-- restano intatti, e la WHERE evita di applicarlo due volte.
UPDATE nexus_prompt_templates
SET content = REPLACE(
        content,
        '(4) raccomandazioni,',
        '(4) raccomandazioni, (4-bis) PROVE: dove un tuo requisito e'' accertabile '
        'con un COMANDO, emettilo come prova eseguibile (descrizione + comando + '
        'attesa fra codice d''uscita, testo presente, testo assente) invece che '
        'come frase — la verifica finale le esegue davvero, mentre un requisito '
        'in prosa nessuno lo puo'' eseguire,'
    ),
    version = version + 1,
    updated_at = NOW()
WHERE key LIKE 'subagent.%'
  AND is_active
  AND content LIKE '%(4) raccomandazioni,%'
  AND content NOT LIKE '%(4-bis) PROVE%';

INSERT INTO settings (key, value, description, category)
VALUES (
    'agent.final_gate.piano_max_prove',
    '20',
    'Final gate / piano_di_verifica: tetto di prove effettivamente ESEGUITE in '
    'un giro di gate. Conta le prove AMMESSE, non quelle dichiarate: dieci prove '
    'rifiutate dalla soglia non devono consumare il budget di quella che conta. '
    'Oltre il tetto la prova resta un fatto dichiarato, non una prova in piu'' e '
    'nemmeno un silenzio.',
    'agent'
)
ON CONFLICT (key) DO UPDATE
    SET value = EXCLUDED.value,
        description = EXCLUDED.description;
