-- 0688 — Il gate duale esiste, e' deployato, e non scatta mai.
--
-- ROOT CAUSE (misurata il 09/08/2026). Il livello base di
-- `step_gate::classify_step` nasceva dal solo vocabolario dei mutatori: dentro
-- o fuori, cioe' `Mutating` oppure `ReadOnly`. E `Mutating` non convoca in
-- NESSUNA modalita' — la decisione del 04/08 («le write ordinarie non pagano
-- due chiamate LLM») e' giusta per una `edit_file`, e finiva per applicarsi
-- anche a `run_command`, che in quel vocabolario ci sta.
--
-- Sono due cose diverse, e la differenza non e' di grado:
--   - una `edit_file` tocca un file dell'albero di lavoro. Quell'effetto ha
--     gia' due reti: lo snapshot di sessione (`session_autocommit`) e i lettori
--     che quel file lo RILEGGONO (ciclo review, final_gate);
--   - una `run_command` esegue una riga di shell. Puo' raggiungere un database,
--     un servizio, il registro delle porte, la macchina. Nessuna di quelle reti
--     la copre: rileggere un file non dice nulla di una migrazione di schema
--     gia' applicata.
--
-- COSA E' ACCADUTO (gestione-corsi, DB `gestione_corsi_nexus`):
--   - `nexus_agent_meta_steps` con kind='step_validation': 45 righe in tutto, e
--     l'ultima e' dell'08/08 alle 10:40 — cioe' dello sviluppo del gate. In
--     esercizio reale il gate non e' MAI scattato. Stessa firma sugli altri due
--     progetti vivi: 29 righe (agenda-medica, ultima 06/08) e 13
--     (biblioteca-scolastica, ultima 06/08);
--   - nello stesso progetto `dotnet ef database update` e' stato eseguito 5
--     volte e `dotnet ef migrations add` 6 volte. Tutte classificate
--     `Mutating`, tutte passate senza che nessun giudice le vedesse.
--
-- PERCHE' NON SI AGGIUNGE UNA RIGA A `critical_step_rules`. La lista non nomina
-- `dotnet ef database update`. Aggiungercelo chiuderebbe l'istanza e lascerebbe
-- aperta la classe: domani `prisma migrate deploy`, `alembic upgrade head`,
-- `sqlx migrate run`. La lista e' incompleta PER COSTRUZIONE, e finche'
-- l'assenza da quella lista significa «innocuo» il giudizio agentico non
-- avviene affatto — la lista non sta a monte del giudizio, sta al posto suo per
-- tutto cio' che non nomina (regola H).
--
-- IL FIX (punto unico `decisions/step_reach.rs`): il default e' ROVESCIATO. Il
-- livello base viene dalla PORTATA del passo — che cosa raggiunge, e chi lo puo'
-- disfare — e la portata la dichiara il CONTRATTO DEL TOOL, non il testo del
-- comando:
--   read_only    -> il tool non muta
--   regenerable  -> artefatti che il progetto rigenera        } pavimento
--   working_tree -> file dell'albero, coperti dallo snapshot   } Mutating
--   unconfined   -> esegue una riga di shell o SQL             } pavimento
--   undetermined -> mutatore che non si e' potuto collocare    } Critical
--
-- Con questo criterio `dotnet ef database update` e ogni variante futura
-- ricadono nella stessa classe senza che nessuno le abbia previste. Le regole
-- lessicali RESTANO e restano utili, ma possono solo ALZARE il livello: un
-- `rm -rf` riconosciuto e' irreversibile con certezza. Cio' che sparisce e' che
-- l'assenza dalla lista implichi innocuita'.
--
-- `undetermined` non degrada a innocuo (regola Q): un mutatore che non dice ne'
-- cosa esegue ne' dove scrive DICHIARA di non essere stato collocato e tiene il
-- pavimento alto. E' anche cio' che rende non portante l'elenco dei tool che
-- eseguono una riga: un tool che eseguisse e non fosse in quell'elenco
-- resterebbe comunque sopra la soglia, invece di passare.
--
-- ────────────────────────────────────────────────────────────────────────────
-- LA SOGLIA SUL COSTO: `orchestrator.step_reach.observation_commands`
-- ────────────────────────────────────────────────────────────────────────────
--
-- Con la sola inversione qui sopra OGNI `run_command` diventa `unconfined`, e
-- in `enforce` questo significa due chiamate LLM prima di `ls`, `cat`,
-- `git status`. Il costo e' il limite vero di questo gate: renderlo
-- insostenibile e' il modo piu' sicuro di farlo spegnere, cioe' di tornare al
-- punto di partenza per un'altra strada.
--
-- La soglia e' la portata `observation`, ed e' un elenco — ma di POLARITA'
-- opposta a quello che ha prodotto il difetto, e l'asimmetria e' tutto:
--
--   `critical_step_rules` ACCUSA.  Cio' che non nomina PASSA.
--     -> la sua incompletezza costa SICUREZZA, e non si vede.
--   `observation_commands` ASSOLVE. Cio' che non nomina viene GIUDICATO.
--     -> la sua incompletezza costa DENARO e LATENZA, e si vede subito.
--
-- Un elenco incompleto che fallisce verso il giudizio non e' la stessa cosa di
-- uno che fallisce verso il passaggio. Aggiungere una voce qui e' un DATO, non
-- una patch: nessun redeploy, cache 60s.
--
-- Ogni voce e' un PREFISSO DI PAROLE sulla riga scomposta dallo scompositore
-- unico (`decisions::shell_command::comandi`), mai un contains: `git status`
-- assolve `git status --short` e non ha nulla da dire su `git push`. Assolve
-- solo se TUTTI i comandi della catena sono riconosciuti, nessuno ha
-- redirezioni (`cat piano.md > src/main.rs` scrive) e nessuno ha assegnazioni
-- env in testa.
--
-- Cosa NON c'e', e perche': `env` (esegue un altro programma), `curl` (`-o`,
-- `-X POST`), `find` (`-delete`), `sed` (`-i`), `git branch` (`-D`),
-- `git checkout`. Le voci scelte sono innocue sotto QUALUNQUE flag, oppure
-- portano il flag nel prefisso (`node --version` non assolve `node -e ...`).
-- Nel dubbio si lascia fuori: costa una convocazione.
--
-- QUESTA MIGRAZIONE porta anche il gate da `enforce_irreversible` a `enforce`,
-- cioe' il livello che convoca i giudici sui Critical. Senza questo passaggio
-- il criterio sarebbe una lente che nessun gate interroga: costruita, non
-- entrata in esercizio.
--
-- ROLLBACK, senza toccare codice ne' redeploy (regola G): riportare
-- `orchestrator.critical_step_gate_mode` a `enforce_irreversible`; i passi a
-- portata non confinata restano CLASSIFICATI e PERSISTITI come meta_step
-- `step_validation` (con il campo `reach` nel payload), quindi il punto cieco
-- resta misurabile anche da spenta. Se invece il gate risulta troppo rumoroso
-- su un comando preciso, il rimedio e' aggiungerlo QUI, non spegnere il gate.

INSERT INTO settings (key, value, description)
VALUES (
    'orchestrator.step_reach.observation_commands',
    'ls,pwd,cat,head,tail,wc,echo,which,whoami,date,printenv,hostname,tree,df,ps,grep,rg,git status,git diff,git log,git show,node --version,npm --version,pnpm --version,dotnet --version,python --version,cargo --version,java -version',
    'Prefissi di comando che ASSOLVONO una riga di shell nel gate duale (mig 0688): la portata scende a `observation` e il passo resta sotto la soglia di convocazione. Unico elenco del gate la cui incompletezza costa convocazioni e non sicurezza - cio'' che non nomina viene GIUDICATO. Ogni voce e'' un prefisso di PAROLE sulla riga scomposta (decisions::shell_command::comandi), mai un contains; assolve solo se tutti i comandi della catena sono riconosciuti, senza redirezioni ne'' env inline. Vuoto = nulla e'' provatamente innocuo. Punto unico: decisions/step_reach.rs. Cache 60s.'
)
ON CONFLICT (key) DO UPDATE
SET value = EXCLUDED.value,
    description = EXCLUDED.description;

UPDATE settings
SET value = 'enforce',
    description = 'Gate duale sui passi critici: off | observe (classifica e persiste, zero costo LLM) | enforce_irreversible (convoca solo sugli Irreversible) | enforce (convoca su Critical e Irreversible). Dal 09/08/2026 il livello base viene dalla PORTATA del passo (decisions/step_reach.rs): `unconfined` (esegue una riga di shell/SQL) e `undetermined` (mutatore non collocabile) hanno pavimento Critical, quindi con `enforce` una migrazione di schema arriva ai giudici anche se nessuna regola lessicale la nomina (mig 0688). La soglia sul costo e'' orchestrator.step_reach.observation_commands, che riporta sotto soglia le righe di sola osservazione. Vocabolario canonico, parse unico in decisions::step_gate::StepGateMode.'
WHERE key = 'orchestrator.critical_step_gate_mode';
