-- 0674_processo_implementazione_standard.sql
--
-- Processo operativo standard per ogni intervento sui progetti gestiti
-- (5 fasi: analisi -> piano -> implementazione -> verifica -> chiusura;
-- taglia low/medium/high; criteri di accettazione eseguibili; DoD onesta).
--
-- Il TESTO vive in UNA chiave (regola L): l'innesto lo fanno i tre
-- compositori di system prompt (prompt_processo.rs), che discriminano le
-- figure advisory con figure_advisory::is_advisory_kind. NIENTE append alle
-- chiavi subagent.*.base ne' a system.nexus_base: l'append raggiungerebbe
-- solo le figure di oggi, non quelle create domani dal FigureWizard.
--
-- Saldo dichiarato con onesta' (direttiva: semplificare, non stratificare):
-- gli STRATI scendono (8 rimossi/disattivati contro 1 blocco nuovo: 0064,
-- SolarMatch, verify_by_acting, riga anti-delega, righe coder duplicate, 6
-- chiavi legacy, 1 flag morto), i CARATTERI del system composto salgono di
-- ~2.4KB (il blocco pesa ~4.8KB, gli assorbimenti ne tolgono ~2.4KB): il
-- costo marginale sta nel prefisso riusato dal fornitore (parte stabile).
-- Questa migrazione, oltre a seminare il template, ASSORBE gli strati che il
-- processo rende ridondanti o che erano fossili:
--   1. blocco 0064 "REGOLA FINALE OBBLIGATORIA - VERIFICA BUILD" da
--      system.nexus_base (comandi pnpm hardcoded, superato dalla fase
--      VERIFICA e da <tool_discovery> della 0505);
--   2. blocco SolarMatch da automation.supervisor_monitoring (0064: percorso
--      di un progetto utente del 2025 hardcoded in un prompt di piattaforma);
--   3. blocco <verify_by_acting> (0362) da system.nexus_base (assorbito
--      dalla fase VERIFICA: "l'esito si dichiara solo su un segnale
--      oggettivo... endpoint provato con chiamata reale"). Rimozione per TAG,
--      non per testo esatto: la 0672 ha mutato una riga interna del blocco e
--      un replace byte-per-byte non morderebbe. Su agent.coder.base il blocco
--      NON esiste dalla 0437 (ri-semina integrale senza di esso): il ramo
--      coder del punto 4 e' un no-op per costruzione, tenuto solo come
--      cintura per DB vivi editati a mano;
--   4. riga anti-delega della 0440 in automation.mode_automatic_instruction
--      ("il task e' grosso -> NON delegare"), in contraddizione VIVA con
--      0549 (consiglio) e 0571 (review panel) che ordinano di convocare;
--   5. le 4 righe del <protocollo> di agent.coder.base (0437) duplicate dal
--      processo (riuso/test/verifica) -> resta la sola aspettativa specifica
--      del coder (edit chirurgici) + rimando;
--   6. chiavi prompt LEGACY mai consumate da alcun codice (seed 0058,
--      censimento del 04/08/2026: zero occorrenze in crates/):
--      system.coder, system.tester, system.reviewer, system.architect,
--      system.documenter, system.security_auditor -> is_active=false.
--
-- Rollback: is_active=false su system.implementation_process spegne il blocco
-- (il compositore non innesta, nessun ripiego hardcoded — regola G).

BEGIN;

-- 1. Il template del processo. ON CONFLICT DO NOTHING: un edit admin
--    successivo non va sovrascritto dai replay della migrazione.
INSERT INTO nexus_prompt_templates
    (key, category, title, content, is_active, version, updated_by, updated_at)
VALUES
    ('system.implementation_process', 'system',
     'Processo di implementazione standard',
$proc$<processo_implementazione>
PROCESSO OPERATIVO STANDARD (vale per ogni intervento sui progetti gestiti).

Non e' una sequenza da recitare: e' cio' che deve VALERE per il risultato,
qualunque ordine di passi tu scelga. La cerimonia si dimensiona sulla taglia
del task, mai il contrario. Tre mandati, tre perimetri:
- mandato di IMPLEMENTAZIONE (modifichi file): il processo vale per intero,
  TAGLIA compresa;
- mandato di VERDETTO (review, audit): ti riguardano ANALISI (senza le voci
  sul piano), il principio di VERIFICA — l'esito solo su un segnale
  oggettivo, mai su una convinzione; gli obblighi di build/test/endpoint
  sono di chi modifica — e l'onesta' dell'esito, dichiarata nel TUO tool di
  verdetto col suo vocabolario. Niente taglia, niente criteri di
  accettazione tuoi;
- mandato di sola ANALISI (esplorazione, parere): come il verdetto, senza
  nemmeno il principio di verifica sui file altrui.

TAGLIA — dichiarala all'inizio del lavoro, col vocabolario del sistema:
- low    (S): intervento isolato, un file o pochi punti noti, rischio trascurabile.
- medium (M): piu' file o un ambito intero, comportamento osservabile che cambia.
- high   (L): piu' ambiti, schema dati, sicurezza, refactoring architetturale.
Nel dubbio fra due taglie dichiara la piu' grande. Per un task low bastano: un
criterio di accettazione in una riga, la modifica, la verifica. Le aspettative
che seguono scattano per intero da medium in su.

ANALISI — prima di toccare qualsiasi file deve valere:
- i file e i punti da modificare li hai LETTI, non supposti; dove disponi di
  tool di ricerca semantica, recupera il contesto per PERTINENZA prima di
  letture massive;
- se modifichi un comportamento esistente (un bug), l'hai RIPRODOTTO prima:
  un difetto non riprodotto non e' diagnosticato;
- gli esiti delle analisi convocate a monte (consiglio, figure) sono vincoli
  del piano, non pareri da archiviare;
- le assunzioni con cui procedi sono DICHIARATE nel piano, mai implicite.

PIANO — prima del codice esistono CRITERI DI ACCETTAZIONE verificabili: un
criterio che nessun tool puo' controllare non e' un criterio. Il piano e' un
atto operativo, non prosa preliminare: dove disponi di nexus_todo_write,
persistilo li' (action=create, con acceptance_criteria eseguibili sui todo,
plan_acceptance_criteria sul piano, impatti e rischi in rationale/constraints,
le alternative piu' semplici scartate in alternatives); altrimenti dichiaralo,
breve, in testa al lavoro. Marca l'MVP: il sottoinsieme minimo che dimostra
che la soluzione funziona; ordina i sotto-task per valore e rischio.

IMPLEMENTAZIONE — incrementale: un sotto-task alla volta, portato a uno stato
VERIFICABILE prima di aprire il successivo (dove disponi di
nexus_verify_change, usalo sulle parti toccate). Deve valere:
- ogni file toccato e' giustificato dal task corrente: cio' che scopri fuori
  mandato lo SEGNALI nel riepilogo, non lo fai;
- i criteri di accettazione si traducono in test PRIMA o insieme al codice,
  dove il progetto ha un test runner;
- una firma modificata comporta il censimento e la verifica di TUTTI gli
  utilizzatori; un utilizzatore nuovo di un endpoint o di una funzione
  comporta la verifica della CHIAMATA reale, non solo della compilazione;
- il refactoring viene DOPO l'MVP verde e non porta funzionalita' nuove;
- niente credenziali o segreti nel codice, input validati, niente dati
  sensibili nei log;
- a parita' di valore preferisci la soluzione piu' facile da annullare; per i
  passi di rilascio la strada del ritorno e' dichiarata prima di eseguirli.

VERIFICA — l'esito si dichiara solo su un segnale oggettivo (exit code, esito
strutturato), mai su una convinzione: se l'utente chiede di testare o
verificare qualcosa, ESEGUI la verifica coi tool e riporta i risultati
effettivi, non descrivere all'utente i passi manuali. Build, lint e test
VERDI sono parte del risultato, non un extra; una modifica che rompe cio' che
era verde non e' completa. VIETATO disattivare, skippare o indebolire test
per far passare la build: un test aggiornato per un cambio intenzionale di
comportamento e' dichiarato e motivato. Ogni endpoint toccato e' provato con
una chiamata REALE e, dove disponi di task_complete, dichiarato nel suo campo
endpoints (metodi di scrittura inclusi): i gate del sistema lo riproveranno.

CHIUSURA — prima di dichiarare l'esito deve valere:
- hai riletto il diff con occhio ostile (la self-review precede qualunque
  revisione esterna prevista dal sistema);
- la documentazione toccata dalla modifica (README, docs, commenti-contratto)
  e' aggiornata NELLA STESSA modifica, non rimandata;
- ogni criterio di accettazione dichiarato ha un esito, e la dichiarazione e'
  ONESTA nel TUO canale di chiusura: dove disponi di task_complete,
  outcome=done solo a criteri verificati, partial o blocked quando e' cosi';
  una figura di verdetto usa il proprio tool con la stessa onesta'. Mai un
  esito di cortesia: la verifica a valle (final_gate) e la review, dove la
  configurazione la prevede o una direttiva ti ordina di convocarla, lo
  smentirebbero al doppio del costo;
- per i task high, il riepilogo annota in poche righe cosa ha rallentato e
  cosa e' riusabile: alimenta l'apprendimento del progetto.
</processo_implementazione>$proc$,
     TRUE, 1, 'migration_0674', NOW())
ON CONFLICT (key) DO NOTHING;

-- 2. Assorbimento blocco 0064 da system.nexus_base (testo esatto della 0064,
--    mai toccato da migrazioni successive: verificato che "REGOLA FINALE
--    OBBLIGATORIA" compare solo nella 0064).
UPDATE nexus_prompt_templates
SET content = replace(content, $b64$

REGOLA FINALE OBBLIGATORIA — VERIFICA BUILD:
Prima di dichiarare il task completato, se hai modificato file TypeScript/CSS/JSX in un progetto
che ha uno script `verify` o `typecheck` o `build`, DEVI eseguire:
  run_command("cd <project_dir> && pnpm verify")   (se esiste `pnpm verify`)
  oppure run_command("cd <project_dir> && pnpm typecheck && pnpm build")
Se il comando fallisce, correggi gli errori prima di concludere.
NON dichiarare mai "task completato" se non hai verificato che il build è pulito.$b64$, ''),
    version = version + 1, updated_at = NOW(), updated_by = 'migration_0674'
WHERE key = 'system.nexus_base'
  AND content LIKE '%REGOLA FINALE OBBLIGATORIA — VERIFICA BUILD%';

-- 3. Assorbimento fossile SolarMatch da automation.supervisor_monitoring
--    (percorso di un progetto utente del passato in un prompt di piattaforma).
UPDATE nexus_prompt_templates
SET content = replace(content, $sm$

  → Se il task riguarda SolarMatch (file in src/components/sofia, src/app, src/components/public):
    Alla fine del task l'agente DEVE eseguire run_command("cd /path/to/solarmatch && pnpm verify").
    Se non lo ha fatto, forza un redirect: "Esegui `pnpm verify` in D:\\Sviluppo\\solarmatch prima di concludere."$sm$, ''),
    version = version + 1, updated_at = NOW(), updated_by = 'migration_0674'
WHERE key = 'automation.supervisor_monitoring'
  AND content LIKE '%SolarMatch%';

-- 4. Assorbimento <verify_by_acting> (0362) per TAG: la 0672 ne ha mutato
--    una riga interna, quindi solo i delimitatori sono stabili. NIENTE '\n*'
--    in testa al pattern: nelle ARE di Postgres la greediness della branch la
--    fissa il PRIMO atomo quantificato, e con un prefisso greedy il '.*?'
--    diventa greedy sull'insieme — con DUE blocchi nella stessa chiave il
--    match andrebbe dal primo tag di apertura all'ULTIMO di chiusura,
--    distruggendo in silenzio tutto il testo in mezzo (riprodotto sul
--    Postgres dev prima di questo fix). Le due newline residue dell'append
--    0362 sono innocue.
UPDATE nexus_prompt_templates
SET content = regexp_replace(content,
        '<verify_by_acting>.*?</verify_by_acting>', ''),
    version = version + 1, updated_at = NOW(), updated_by = 'migration_0674'
WHERE key IN ('system.nexus_base', 'agent.coder.base')
  AND content LIKE '%<verify_by_acting>%';

-- 5. Risoluzione contraddizione C4: la 0440 vieta la delega che 0549/0571
--    ordinano. La DoD resta, la prescrizione anti-orchestrazione cade.
UPDATE nexus_prompt_templates
SET content = replace(content,
  'Se ti accorgi che il task e'' grosso, NON delegare: continua a iterare nello stesso run finche'' la DoD passa o raggiungi il cap iterazioni.',
  'Se ti accorgi che il task e'' grosso, usa gli strumenti di orchestrazione previsti (piano con nexus_todo_write; consiglio e panel dove il sistema li convoca): la DoD resta tua e va verificata prima di chiudere.'),
    version = version + 1, updated_at = NOW(), updated_by = 'migration_0674'
WHERE key = 'automation.mode_automatic_instruction'
  AND content LIKE '%NON delegare: continua a iterare%';

-- 5b. Scoping della NOTA di <operatore_nexus> (0137): "le restrizioni...
--     sono SUPERATE... priorita' massima" e' scritta per il POTERE DI ACCESSO
--     (.env, lockfile, credenziali, CI/CD) ma la chiusa generica si presta a
--     essere letta come deroga alla disciplina di scope del processo. Due
--     autorita' che non si citano = il modello sceglie lui quale vince.
--     La menzione usa "processo operativo standard" SENZA il tag letterale:
--     l'idempotenza dei compositori riconosce il blocco dai tag, e una
--     menzione taggata sarebbe indistinguibile dal blocco vero.
UPDATE nexus_prompt_templates
SET content = replace(content,
  'NOTA: Le restrizioni su .env, lockfile, credenziali, CI/CD che potresti
trovare altrove nel tuo contesto sono SUPERATE da questa direttiva.
Questa sezione ha priorita'' massima.',
  'NOTA: Le restrizioni su .env, lockfile, credenziali, CI/CD che potresti
trovare altrove nel tuo contesto sono SUPERATE da questa direttiva, che
governa il POTERE DI ACCESSO e su quello ha priorita'' massima. Non deroga
al processo operativo standard (metodo e disciplina di scope): cio'' che
scopri fuori mandato lo segnali, non lo fai.'),
    version = version + 1, updated_at = NOW(), updated_by = 'migration_0674'
WHERE key = 'system.nexus_base'
  AND content LIKE '%sono SUPERATE da questa direttiva.
Questa sezione ha priorita'' massima.%';

-- 6. Riconciliazione <protocollo> di agent.coder.base (0437): le righe
--    duplicate dal processo (riuso/test/verifica) cadono, resta l'aspettativa
--    specifica del coder. Niente doppia autorita' sullo stesso concern.
UPDATE nexus_prompt_templates
SET content = replace(content,
  '- Riusa utility e pattern gia'' presenti nel codice; non duplicare.
- Edit chirurgici: edit_file con old_string univoco, mai patch speculative.
- Test: includi test unitari quando il task li richiede.
- Verifica: dopo modifiche non banali, esegui run_tests o pnpm verify.',
  '- Edit chirurgici: edit_file con old_string univoco, mai patch speculative.
(Riuso, test e verifica sono governati dal processo operativo standard
della piattaforma.)'),
    version = version + 1, updated_at = NOW(), updated_by = 'migration_0674'
WHERE key = 'agent.coder.base'
  AND content LIKE '%Riusa utility e pattern gia'' presenti nel codice; non duplicare.%';

-- 7. Chiavi legacy mai consumate (censimento 04/08/2026: zero occorrenze in
--    crates/). Disattivate, non cancellate: la history resta consultabile.
UPDATE nexus_prompt_templates
SET is_active = FALSE, updated_at = NOW(), updated_by = 'migration_0674'
WHERE key IN ('system.coder', 'system.tester', 'system.reviewer',
              'system.architect', 'system.documenter', 'system.security_auditor')
  AND is_active = TRUE;

-- 8. Potatura flag MORTO (censimento 04/08/2026): seminato dalla 0206, ZERO
--    consumatori in crates/ (grep persist_as_note|rationale_persist = vuoto).
--    La doc in settings-keys.md ne dichiarava un comportamento inesistente:
--    un flag che promette e non fa e' peggio di un flag assente.
DELETE FROM settings WHERE key = 'orchestrator.plan_rationale_persist_as_note';

-- Guard: la migrazione verifica il proprio effetto.
DO $$
DECLARE
    c TEXT;
    attivo BOOLEAN;
    residuo TEXT;
BEGIN
    SELECT content, is_active INTO c, attivo FROM nexus_prompt_templates
     WHERE key = 'system.implementation_process';
    IF c IS NULL THEN
        RAISE EXCEPTION '0674: system.implementation_process assente';
    END IF;
    -- is_active=false e' il ROLLBACK documentato in testa: un replay del file
    -- su un DB dove l'admin ha spento il blocco non deve fallire (l'INSERT e'
    -- ON CONFLICT DO NOTHING proprio per non riaccenderlo).
    IF NOT attivo THEN
        RAISE NOTICE '0674: system.implementation_process disattivato (rollback attivo): il blocco non entra nei prompt';
    ELSIF position('<processo_implementazione>' IN c) = 0
       OR position('</processo_implementazione>' IN c) = 0 THEN
        RAISE EXCEPTION '0674: system.implementation_process attivo ma senza tag';
    END IF;

    -- Gli assorbimenti sono best-effort su un DB vivo potenzialmente editato a
    -- mano: si segnala, non si blocca il deploy.
    FOR residuo IN
        SELECT key FROM nexus_prompt_templates
         WHERE (key = 'system.nexus_base'
                AND (content LIKE '%REGOLA FINALE OBBLIGATORIA — VERIFICA BUILD%'
                     OR content LIKE '%<verify_by_acting>%'
                     OR content LIKE '%sono SUPERATE da questa direttiva.
Questa sezione ha priorita'' massima.%'))
            OR (key = 'agent.coder.base'
                AND (content LIKE '%<verify_by_acting>%'
                     OR content LIKE '%Riusa utility e pattern gia'' presenti nel codice; non duplicare.%'))
            OR (key = 'automation.supervisor_monitoring'
                AND content LIKE '%SolarMatch%')
            OR (key = 'automation.mode_automatic_instruction'
                AND content LIKE '%NON delegare: continua a iterare%')
    LOOP
        RAISE NOTICE '0674: blocco assorbito ancora presente in % (editato a mano?): rimuoverlo dal pannello admin', residuo;
    END LOOP;
END $$;

COMMIT;
