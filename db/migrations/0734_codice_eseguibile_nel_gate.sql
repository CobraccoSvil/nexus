-- 0734 — Il codice che il run ha PRODOTTO entra nel final gate: si carica?
--
-- ROOT CAUSE (misurata il 17/08/2026). Run reale dalla UI su progetto vuoto,
-- task «crea calcolatrice.js con quattro funzioni e calcolatrice.test.js con
-- cinque test». `calcolatrice.js` funziona (verificato a mano: somma(2,3)=5, la
-- divisione per zero lancia). `calcolatrice.test.js` NON PARTE:
--   ReferenceError: describe is not defined
-- sintassi Jest in un progetto senza Jest, senza package.json e senza
-- node_modules; il file contiene anche una riga priva di senso
-- (`expect(somma ? sottrai(5, 2) : 0).toBe(3)`). Il run ha chiuso
-- completed=true e il final gate ha dichiarato «passato» DUE volte
-- (cycle=2 inconclusive=2, poi cycle=1 inconclusive=2).
--
-- La catena si e' rotta in tre punti, e il primo aveva visto giusto:
--   1. il Consiglio aveva emesso il rischio ESATTO — «senza un framework di
--      test dichiarato il file di test puo' essere non eseguibile col runner
--      predefinito», con la raccomandazione «preferire node:test se il progetto
--      non ha ancora un framework». L'agente ha scelto Jest lo stesso;
--   2. il riscontro dei requisiti non poteva accorgersene: 15 dei 17 requisiti
--      erano `non_verificabili` (prosa, non letterali cercabili) — limite gia'
--      dichiarato in `requirement_conformance`;
--   3. il final gate non aveva NIENTE da chiedere: nessuna porta registrata ->
--      niente browser_dialogue (0681), niente static_render (0685), niente
--      endpoint_probes, nessuna suite dichiarata. Ha chiuso col beneficio del
--      dubbio.
--
-- PERCHE' E' UN BUCO STRUTTURALE. La famiglia dei criteri copre l'app col
-- server, la pagina statica, la suite E2E e lo stile applicato (0682). Mancava
-- IL CASO BASE: il codice prodotto si carica? E' la stessa forma di difetto
-- gia' chiusa tre volte in questo repo — la verifica che manca e' sempre quella
-- del caso piu' semplice.
--
-- COSA NON FA. Non giudica se i test PASSANO: quella domanda ha gia' il suo
-- punto unico (`mcp-core::suite_verification`), e un test rosso e' INFORMAZIONE
-- mentre un test che non parte e' codice rotto. E' anche il motivo per cui il
-- livello di caricamento gira con un filtro di nome che non incontra alcun test
-- (vedi il vocabolario sotto): MISURATO il 17/08/2026 sui tre casi —
--   node --test --test-name-pattern=<nessuno>  su Jest-senza-Jest -> exit 1
--   idem su un file node:test con un assert FALLITO                -> exit 0
--   idem su un file node:test che passa                            -> exit 0
-- cioe' il discriminante e' il codice d'uscita (segnale strutturato, regola M),
-- senza leggere una riga di output e senza eseguire un solo corpo di test.
-- Senza quel filtro il criterio boccerebbe l'esito dei test, invadendo la
-- domanda di un altro criterio.
--
-- COSA NON BOCCIA. Solo un file che il suo runtime RIFIUTA. Un file fuori
-- vocabolario, un file sparito dall'albero, un progetto che scrive solo
-- documentazione: sono RISPOSTE, e passano. Un runtime ASSENTE dal PATH non e'
-- ne' l'una ne' l'altra cosa — il criterio dichiara di non aver potuto misurare
-- e il run chiude `completed_unverified`, mai `passed` (regola Q).
--
-- SICUREZZA. Nessun comando esegue il codice utente a scopo generale:
-- `node --check` e `python -m py_compile` non eseguono niente, e il livello di
-- caricamento carica il solo modulo di test senza eseguirne i casi. I comandi
-- girano con la radice del run come working dir e un timeout per file.
--
-- Punto unico del criterio: crates/nexus-agent-graph/src/decisions/codice_eseguibile.rs
-- (`pianifica_prova` + `classifica_esecuzione`, puri). Il criterio del gate lo
-- costruisce `mcp-core::native_engine::criterio_codice_eseguibile`; i fatti li
-- raccoglie `mcp-core::agent_graph_adapter::codice_eseguibile`; l'unico I/O del
-- gate sta in `criteria_runner`.
--
-- ROLLBACK: UPDATE settings SET value = 'false'
--            WHERE key = 'agent.final_gate.codice_eseguibile_enabled';

INSERT INTO settings (key, value, description, category)
VALUES (
    'agent.final_gate.codice_eseguibile_enabled',
    'true',
    'Final gate: i file di codice che il run ha PRODOTTO si caricano nel loro '
    'runtime? Deterministico (nessun modello). Boccia SOLO un file che il '
    'runtime rifiuta; non giudica l''esito dei test, che e'' la domanda di '
    'suite_verification.',
    'agent'
)
ON CONFLICT (key) DO UPDATE
    SET value = EXCLUDED.value,
        description = EXCLUDED.description;

-- Il VOCABOLARIO (regola G): un linguaggio nuovo e' una riga, non una patch.
--
-- `carica`      = accerta il file SENZA eseguirlo.
-- `carica_test` = CARICA un file di test (import risolti, simboli del contorno
--                 presenti) senza eseguirne i casi. Assente = per quella
--                 estensione la domanda si ferma alla compilazione: e' una
--                 dichiarazione, non un ripiego da riempire.
-- `marcatori_test` = suffissi dello STEM (il nome senza estensione). Suffisso e
--                 non sottostringa: `spec_helper.js` non e' un test, e caricarlo
--                 eseguirebbe codice di supporto che nessuno ha chiesto.
--
-- Il file da provare NON e' nella riga: lo aggiunge in coda chi esegue, che e'
-- anche l'unico a sapere come renderlo per un processo esterno.
--
-- I marcatori attorno al valore delimitano il blocco che i test di
-- `agent_graph_adapter::codice_eseguibile` rileggono da QUESTO file: il
-- vocabolario provato e' quello che il DB riceve, non una copia scritta a mano
-- che potrebbe divergerne restando verde (regola O).
INSERT INTO settings (key, value, description, category)
VALUES (
    'agent.final_gate.runtime_per_estensione',
-- <<vocabolario>>
    '{
      "marcatori_test": [".test", ".spec", "_test"],
      "estensioni": {
        "js":  {"carica": "node --check",
                "carica_test": "node --test --test-name-pattern=nexus_nessun_test_da_eseguire"},
        "cjs": {"carica": "node --check",
                "carica_test": "node --test --test-name-pattern=nexus_nessun_test_da_eseguire"},
        "mjs": {"carica": "node --check",
                "carica_test": "node --test --test-name-pattern=nexus_nessun_test_da_eseguire"},
        "py":  {"carica": "python -m py_compile"}
      }
    }'
-- <</vocabolario>>
    ,
    'Final gate / codice_eseguibile: estensione -> comandi con cui si accerta '
    'che un file prodotto si carichi. `carica` non esegue nulla; `carica_test` '
    'carica un file di test senza eseguirne i casi (il filtro di nome e'' cio'' '
    'che tiene fuori l''esito dei test, misurato). Estensione non dichiarata = '
    'file fuori vocabolario, nessuna prova e nessun difetto.',
    'agent'
)
ON CONFLICT (key) DO UPDATE
    SET value = EXCLUDED.value,
        description = EXCLUDED.description;

INSERT INTO settings (key, value, description, category)
VALUES (
    'agent.final_gate.codice_eseguibile_timeout_s',
    '30',
    'Final gate / codice_eseguibile: pazienza per UN comando di prova. Oltre, '
    'il processo si uccide e il file resta NON PROVATO (mai rifiutato: un '
    'runtime che non risponde non e'' un difetto del codice).',
    'agent'
)
ON CONFLICT (key) DO UPDATE
    SET value = EXCLUDED.value,
        description = EXCLUDED.description;

INSERT INTO settings (key, value, description, category)
VALUES (
    'agent.final_gate.codice_eseguibile_max_file',
    '50',
    'Final gate / codice_eseguibile: tetto di file effettivamente PROVATI in un '
    'giro di gate. Conta i provati, non gli scritti: cinquanta .md non devono '
    'consumare il budget di un sorgente. Oltre il tetto il file resta un fatto '
    'dichiarato, non una prova in piu'' e nemmeno un silenzio.',
    'agent'
)
ON CONFLICT (key) DO UPDATE
    SET value = EXCLUDED.value,
        description = EXCLUDED.description;
