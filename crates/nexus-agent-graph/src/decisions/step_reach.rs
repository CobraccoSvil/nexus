//! «Che cosa RAGGIUNGE questo passo, e chi lo puo' disfare?»
//!
//! Punto unico (regola L) della PORTATA di un passo: la proprieta' strutturale
//! su cui il gate duale decide il proprio livello base, al posto del solo
//! vocabolario dei mutatori.
//!
//! # Il difetto che questo modulo chiude
//!
//! Fino all'08/08/2026 il livello base di [`super::step_gate::classify_step`]
//! nasceva da `is_mutator_tool_name`: dentro/fuori il vocabolario dei mutatori,
//! cioe' `Mutating` oppure `ReadOnly`. E `Mutating` non convoca in nessuna
//! modalita' — la decisione del 04/08 («le write ordinarie non pagano due
//! chiamate LLM») e' giusta per una `edit_file`, e finiva per applicarsi anche
//! a `run_command`, che nello stesso vocabolario ci sta.
//!
//! Sono due cose diverse, e la differenza non e' di grado:
//!
//! - una `edit_file` tocca un file dell'albero di lavoro. Quell'effetto ha gia'
//!   due reti: lo snapshot di sessione (`mcp-core::session_autocommit`, che
//!   fotografa l'albero con git plumbing) e i lettori che quel file lo
//!   RILEGGONO (ciclo review, `final_gate`, `correction_progress`);
//! - una `run_command` esegue una riga di shell. Puo' raggiungere qualunque
//!   cosa raggiunga la shell: un database, un servizio, il registro delle
//!   porte, la macchina, la rete. Nessuna di quelle reti la copre — rileggere
//!   un file non dice nulla di una migrazione di schema gia' applicata.
//!
//! MISURATO il 09/08/2026 su gestione-corsi: `dotnet ef database update`
//! eseguito 5 volte e `dotnet ef migrations add` 6 volte, tutte classificate
//! `Mutating`, tutte passate senza che nessun giudice le vedesse. Nello stesso
//! DB `nexus_agent_meta_steps` ha 45 righe `step_validation` e l'ultima e'
//! dell'08/08 alle 10:40, cioe' dello sviluppo del gate: in esercizio reale il
//! gate non e' MAI scattato.
//!
//! # Perche' non si aggiunge una riga alle regole
//!
//! `orchestrator.critical_step_rules` non nomina `dotnet ef database update`.
//! Aggiungercelo chiuderebbe l'istanza e lascerebbe aperta la classe: domani
//! `prisma migrate deploy`, `alembic upgrade head`, `sqlx migrate run`. La
//! lista e' incompleta PER COSTRUZIONE, e finche' l'assenza da quella lista
//! significa «innocuo» il giudizio agentico non avviene affatto — la lista non
//! sta a monte del giudizio, sta al posto suo per tutto cio' che non nomina
//! (regola H: la toppa e' inseguire le varianti a codice).
//!
//! Qui il criterio e' rovesciato: la portata la dichiara il CONTRATTO DEL TOOL,
//! non il testo del comando. `run_command` ha portata non confinabile perche'
//! esegue una riga di shell, non perche' quella riga dica «database». Con
//! questo criterio `dotnet ef database update`, `prisma migrate deploy` e
//! qualunque variante futura ricadono nella stessa classe senza che nessuno le
//! abbia previste.
//!
//! # L'ignoto non degrada a innocuo
//!
//! [`StepReach::Undetermined`] esiste perche' il difetto originario non era
//! solo la lista: era che il silenzio fosse indistinguibile da «passo
//! innocuo». Un tool mutatore il cui input non si sa collocare (nessun path
//! riconoscibile, nessuna riga eseguita) NON scende a `WorkingTree`: dichiara
//! di non essere stato collocato e tiene il pavimento alto (regola Q, punto 2).
//! E' anche cio' che rende non portante l'appartenenza a
//! [`super::step_gate::TOOL_CON_COMANDO`]: un tool che esegue righe e non fosse
//! in quell'elenco resterebbe comunque sopra la soglia, invece di passare.
//!
//! # La soglia sul costo che non c'era: perche' l'elenco che ASSOLVE e' sparito
//!
//! Fino al 18/08/2026 esisteva una sesta variante, `Observation`, e con lei un
//! vocabolario DB (`orchestrator.step_reach.observation_commands`, mig 0688)
//! che riportava sotto la soglia le righe fatte di soli comandi di
//! osservazione: `ls`, `cat`, `git status`, `node --version`. Serviva a non
//! pagare due chiamate LLM per un `ls`, e l'argomento era che un elenco che
//! ASSOLVE ha polarita' opposta a uno che accusa — cio' che non nomina viene
//! giudicato, quindi la sua incompletezza costa denaro e latenza, non un buco.
//!
//! L'argomento reggeva. Quello che non reggeva era la PREMESSA: che l'agente
//! usi la shell per guardare. MISURATO il 18/08/2026 sui due progetti con
//! attivita' (`agent_steps`, tool che eseguono una riga):
//!
//! - app-libri-18-08, 21 righe: `curl` 8, `npm` 5, `go` 1, `netstat` 1,
//!   `python` 1, `chmod` 1, `sqlite3` 1, `git diff` 1, piu' due righe che
//!   nominavano un altro tool;
//! - audit-verifica-17-08, 5 righe: `node --test` 3, `npx jest` 2.
//!
//! Ventisei righe, e il vocabolario in esercizio — ventotto voci — ne ha
//! assolta UNA (`git diff backend/db/schema.sql`). Le 33 convocazioni reali del
//! gate su quei due progetti sono TUTTE su `unconfined`. La ragione e'
//! strutturale e non congiunturale: per guardare l'agente ha tool DEDICATI
//! (`read_file`, `list_files`, `search_in_files`), e la shell la usa per
//! COSTRUIRE ed ESEGUIRE. Il vocabolario era progettato per un uso della shell
//! che non avviene.
//!
//! LIMITE DELLA MISURA, dichiarato: due soli progetti, entrambi di generazione
//! di app. Un task di DIAGNOSI userebbe piu' `cat`/`grep`, e li' qualcosa
//! avrebbe risparmiato. La rimozione e' decisa sapendolo.
//!
//! La ragione VERA della rimozione non e' pero' che fosse quasi inerte: e' che
//! un elenco che assolve SUGGERISCE la soluzione sbagliata al problema
//! successivo. Il caso del 18/08 sono cinque `curl` respinti dal gate, e la
//! strada che veniva spontanea era «aggiungi `curl` al vocabolario con qualche
//! flag vietato» — che avrebbe chiuso l'istanza `curl` e ripresentato lo stesso
//! problema con `wget`, con un client HTTP in node, con `psql`. E' la stessa
//! forma di toppa che questo modulo esiste per NON commettere (regola H); il
//! rimedio giusto e' dare al giudice i fatti che gli mancano, non allungare la
//! lista di chi non deve essere giudicato.
//!
//! Il costo residuo resta un limite reale del gate, e ha gia' i suoi freni
//! (`critical_step_max_rejections`, e il rollback `critical_step_gate_mode` a
//! `enforce_irreversible`). Non ha piu' un elenco.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::step_gate::StepCriticality;

/// Che cosa raggiunge un passo. Vocabolario CHIUSO e canonico (regola N),
/// ordinato per confinamento DECRESCENTE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepReach {
    /// Il passo non muta nulla: il tool e' fuori dal vocabolario dei mutatori.
    ReadOnly,
    /// Muta SOLO artefatti che il progetto sa rigenerare (output di build).
    /// Cancellarli e' il gesto piu' ordinario di un ciclo di sviluppo.
    Regenerable,
    /// Muta file DENTRO l'albero di lavoro. Lo snapshot di sessione e' la rete,
    /// e chi rilegge quei file (review, final_gate) vede l'effetto.
    WorkingTree,
    /// Il passo esegue una riga di shell o una statement SQL: puo' raggiungere
    /// qualunque cosa raggiunga la shell. NON confinabile per costruzione —
    /// e' una proprieta' del CONTRATTO del tool, mai del testo del comando.
    Unconfined,
    /// Tool mutatore che non si e' potuto collocare: input di forma ignota.
    /// Non e' una prova d'innocenza, ed e' l'unica variante che dichiara di
    /// non aver misurato (regola Q).
    Undetermined,
}

impl StepReach {
    /// L'identificatore canonico (regola N). E' cio' che finisce nel payload
    /// del meta_step `step_validation` e nel prompt dei giudici: un solo nome
    /// per portata, in inglese, uguale ovunque.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Regenerable => "regenerable",
            Self::WorkingTree => "working_tree",
            Self::Unconfined => "unconfined",
            Self::Undetermined => "undetermined",
        }
    }

    /// Il PAVIMENTO di criticita' che questa portata impone. Le regole
    /// lessicali possono solo ALZARLO (un `rm -rf` riconosciuto e'
    /// irreversibile con certezza), mai abbassarlo: e' il punto in cui
    /// «assente dalla lista» smette di significare «innocuo».
    pub fn livello_minimo(self) -> StepCriticality {
        match self {
            // Il tool non e' un mutatore: non c'e' niente da disfare.
            Self::ReadOnly => StepCriticality::ReadOnly,
            // Coperti dallo snapshot di sessione e riletti da review /
            // final_gate: restano fuori dal gate duale, come deciso il 04/08.
            Self::Regenerable | Self::WorkingTree => StepCriticality::Mutating,
            // Nessuna rete esistente li disfa: il giudizio serve PRIMA.
            Self::Unconfined | Self::Undetermined => StepCriticality::Critical,
        }
    }

    /// Il motivo, in chiaro, per cui un passo di questa portata viene guardato.
    /// E' DISPLAY composto DAL campo (regola Q, punto 3): lo legge il prompt dei
    /// giudici, che senza di esso vedrebbe «categoria: -» proprio sui passi che
    /// nessuna regola nomina — cioe' sulla maggioranza dei convocati.
    pub fn motivo(self) -> &'static str {
        match self {
            Self::ReadOnly => "non muta nulla",
            Self::Regenerable => "tocca artefatti che il progetto sa rigenerare",
            Self::WorkingTree => {
                "scrive nell'albero di lavoro, coperto dallo snapshot di sessione"
            }
            Self::Unconfined => {
                "esegue una riga di shell o SQL: puo' raggiungere database, servizi o \
                 la macchina, e nessuna rete del progetto disfa quell'effetto"
            }
            Self::Undetermined => {
                "muta, ma non si e' potuto stabilire che cosa tocchi: l'ignoto non \
                 e' una prova d'innocenza"
            }
        }
    }

    /// La portata e' stata accertata? `false` per la sola [`Self::Undetermined`]:
    /// serve a chi COMPONE la spiegazione, che deve poter dire «non l'ho
    /// collocato» invece di affermare una portata che non ha misurato.
    pub fn accertata(self) -> bool {
        self != Self::Undetermined
    }
}

/// I campi dell'input che portano una riga ESEGUITA (shell o SQL). Non e' un
/// elenco di comandi pericolosi: e' la forma dell'input dei tool che eseguono,
/// e la sua incompletezza non fa passare nulla — un mutatore che non espone
/// nessuno di questi campi ne' un path finisce in [`StepReach::Undetermined`],
/// che ha lo stesso pavimento.
const CAMPI_RIGA_ESEGUITA: &[&str] = &["command", "cmd", "sql"];

/// I campi dell'input che nominano un PATH scritto.
const CAMPI_PATH: &[&str] = &["path", "file_path", "target_path", "destination"];

/// Classifica la portata di UN passo. PURA: i due vocabolari (mutatori,
/// artefatti rigenerabili) arrivano dal chiamante (regola G), niente letture
/// qui.
///
/// L'ordine dei criteri non e' arbitrario: si guarda PRIMA se il passo esegue
/// una riga, poi dove scrive. Un tool che esegue e nomina anche un path (es.
/// `--output`) resta non confinato — la riga puo' fare molto piu' di quel file.
///
/// Chi esegue una riga non ha ECCEZIONI: nessun elenco assolve piu' un comando
/// per il suo nome. Il perche' — e la misura che lo decide — sta in testa al
/// modulo.
pub fn classifica_portata(
    tool_name: &str,
    tool_input: &Value,
    fs_mutator_tools: &[String],
    artefatti_rigenerabili: &[String],
) -> StepReach {
    if !super::hitl::is_mutator_tool_name(tool_name, fs_mutator_tools) {
        return StepReach::ReadOnly;
    }
    if CAMPI_RIGA_ESEGUITA
        .iter()
        .any(|c| tool_input.get(c).and_then(Value::as_str).is_some())
    {
        return StepReach::Unconfined;
    }
    let Some(path) = CAMPI_PATH
        .iter()
        .find_map(|c| tool_input.get(c).and_then(Value::as_str))
    else {
        // Mutatore che non dice ne' cosa esegue ne' dove scrive: non si e'
        // potuto collocare, e non lo si dichiara confinato.
        return StepReach::Undetermined;
    };
    match colloca_path(path, artefatti_rigenerabili) {
        CollocazionePath::Rigenerabile => StepReach::Regenerable,
        CollocazionePath::DentroAlbero => StepReach::WorkingTree,
        // Un path che ESCE dall'albero non e' coperto dallo snapshot di
        // sessione, che fotografa la sola radice del progetto.
        CollocazionePath::FuoriAlbero => StepReach::Unconfined,
    }
}

/// Dove cade un path scritto, rispetto all'albero di lavoro del progetto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollocazionePath {
    /// Un segmento del percorso e' un artefatto rigenerabile noto.
    Rigenerabile,
    /// Relativo e non risale: sta sotto la radice del progetto.
    DentroAlbero,
    /// Assoluto, con unita' Windows, o che risale con `..`.
    FuoriAlbero,
}

/// Colloca un path rispetto all'albero. Punto unico della domanda «questo
/// bersaglio sta dentro il progetto, ed e' rigenerabile?»: vi delega anche il
/// declassamento degli irreversibili in [`super::step_gate`], perche' due
/// normalizzazioni diverse darebbero due idee diverse di «dentro».
pub fn colloca_path(bersaglio: &str, artefatti: &[String]) -> CollocazionePath {
    let b = bersaglio.replace('\\', "/");
    let b = b.trim_start_matches("./");
    if b.starts_with('/') || b.contains("../") || b == ".." || b.contains(':') {
        return CollocazionePath::FuoriAlbero;
    }
    let rigenerabile = b
        .split('/')
        .filter(|s| !s.is_empty())
        .any(|seg| artefatti.iter().any(|a| a.eq_ignore_ascii_case(seg)));
    if rigenerabile {
        CollocazionePath::Rigenerabile
    } else {
        CollocazionePath::DentroAlbero
    }
}

/// Il bersaglio e' un artefatto rigenerabile DENTRO il progetto? Deriva da
/// [`colloca_path`]: fuori dall'albero il NOME di una cartella non dice piu' di
/// chi sia, quindi `../node_modules` non e' rigenerabile per questo criterio.
pub fn path_rigenerabile(bersaglio: &str, artefatti: &[String]) -> bool {
    colloca_path(bersaglio, artefatti) == CollocazionePath::Rigenerabile
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mutatori() -> Vec<String> {
        [
            "write_file",
            "edit_file",
            "delete_file",
            "run_command",
            "run_service",
            "git_command",
            "nexus_db_query",
            "git_commit",
            "stop_service",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    fn artefatti() -> Vec<String> {
        ["node_modules", ".next", "dist", "target", ".cache"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn portata(tool: &str, input: Value) -> StepReach {
        classifica_portata(tool, &input, &mutatori(), &artefatti())
    }

    /// IL CASO MISURATO il 09/08/2026 su gestione-corsi, ed e' la ragione per
    /// cui questo modulo esiste: una migrazione di schema EF Core eseguita 5
    /// volte, mai vista da un giudice perche' `run_command` cadeva in
    /// `Mutating` — lo stesso livello di una `edit_file`, e `Mutating` non
    /// convoca in NESSUNA modalita'.
    ///
    /// MUTAZIONE: far ricadere i tool che eseguono una riga su
    /// `StepReach::WorkingTree` (cioe' ripristinare la conflazione) riporta il
    /// pavimento a `Mutating` e le ultime due asserzioni cadono.
    #[test]
    fn la_migrazione_di_schema_non_e_una_write_ordinaria() {
        let cmd = json!({ "command": "dotnet ef database update --project SchoolCoursesApi" });
        assert_eq!(portata("run_command", cmd.clone()), StepReach::Unconfined);
        assert_eq!(
            portata("run_command", cmd).livello_minimo(),
            StepCriticality::Critical,
            "una migrazione di schema non la disfa ne' lo snapshot ne' il final_gate"
        );
        // La stessa classe senza che nessuna variante sia stata prevista: e'
        // il punto per cui non si aggiunge una riga alle regole lessicali.
        for riga in [
            "prisma migrate deploy",
            "alembic upgrade head",
            "sqlx migrate run",
            "npx sequelize-cli db:migrate",
        ] {
            assert_eq!(
                portata("run_command", json!({ "command": riga })),
                StepReach::Unconfined,
                "'{riga}': la portata la da' il contratto del tool, non il testo"
            );
        }
    }

    /// La meta' che NON deve cambiare: le write ordinarie restano fuori dal
    /// gate duale (decisione del 04/08). Se questo test rosseggia, il criterio
    /// ha appena messo due chiamate LLM su ogni `edit_file`.
    #[test]
    fn le_write_ordinarie_restano_fuori_dal_gate() {
        assert_eq!(
            portata("edit_file", json!({"path": "src/app.ts"})),
            StepReach::WorkingTree
        );
        assert_eq!(
            portata("write_file", json!({"file_path": "backend/Program.cs"})),
            StepReach::WorkingTree
        );
        for r in [StepReach::WorkingTree, StepReach::Regenerable] {
            assert_eq!(r.livello_minimo(), StepCriticality::Mutating);
        }
        // Un tool non mutatore non tocca nulla, e nessun input lo cambia.
        assert_eq!(
            portata("read_file", json!({"path": "src/app.ts"})),
            StepReach::ReadOnly
        );
        assert_eq!(
            portata("read_file", json!({"command": "rm -rf /"})),
            StepReach::ReadOnly,
            "il vocabolario dei mutatori resta il primo criterio"
        );
    }

    /// L'ignoto e' una variante e tiene il pavimento alto (regola Q): un
    /// mutatore che non dice ne' cosa esegue ne' dove scrive NON e' innocuo.
    ///
    /// MUTAZIONE: far degradare il ramo senza path a `WorkingTree` (l'ovvio
    /// «sara' un file del progetto») fa cadere l'asserzione sul livello.
    #[test]
    fn il_mutatore_non_collocabile_non_degrada_a_innocuo() {
        let p = portata("git_commit", json!({"message": "wip"}));
        assert_eq!(p, StepReach::Undetermined);
        assert_eq!(p.livello_minimo(), StepCriticality::Critical);
        assert!(!p.accertata(), "deve poter dichiarare di non aver misurato");
        assert!(StepReach::Unconfined.accertata());
    }

    /// Un path che ESCE dall'albero non e' coperto dallo snapshot di sessione,
    /// che fotografa la sola radice del progetto.
    #[test]
    fn la_scrittura_fuori_albero_non_e_confinata() {
        for p in [
            "/etc/hosts",
            "C:/Windows/System32/drivers/etc/hosts",
            "../../altro-progetto/src/main.rs",
        ] {
            assert_eq!(
                portata("write_file", json!({ "path": p })),
                StepReach::Unconfined,
                "'{p}' esce dall'albero: lo snapshot non lo copre"
            );
        }
        assert_eq!(
            portata("write_file", json!({"path": "dist/bundle.js"})),
            StepReach::Regenerable
        );
    }

    /// Chi esegue una riga resta non confinato anche quando nomina un file:
    /// la riga puo' fare molto piu' di quel path.
    #[test]
    fn eseguire_prevale_su_scrivere() {
        assert_eq!(
            portata(
                "run_command",
                json!({"command": "dotnet ef migrations script --output out.sql", "path": "out.sql"})
            ),
            StepReach::Unconfined
        );
        assert_eq!(
            portata("nexus_db_query", json!({"sql": "UPDATE corsi SET attivo=false"})),
            StepReach::Unconfined
        );
    }

    /// CHI ESEGUE UNA RIGA NON HA ECCEZIONI, ed e' il cambiamento del
    /// 18/08/2026. Fino a quel giorno un vocabolario DB assolveva le righe di
    /// sola osservazione (`ls`, `cat`, `git status`): la misura in testa al
    /// modulo dice che su 26 righe realmente eseguite ne assolveva UNA, e che
    /// la sua esistenza suggeriva di allungare la lista al primo comando
    /// rumoroso — la toppa della regola H.
    ///
    /// Le prime cinque righe sono quelle che il vocabolario ASSOLVEVA: sono
    /// qui perche' il cambiamento sia visibile in cio' che il criterio
    /// risponde, non solo in cio' che il modulo non contiene piu'.
    ///
    /// MUTAZIONE: reintrodurre una qualunque assoluzione per nome di comando
    /// (elenco DB, costante, default "solo per i comandi ovvi") fa cadere il
    /// primo blocco, che e' esattamente il meccanismo rimosso.
    #[test]
    fn ogni_riga_eseguita_e_non_confinata() {
        for riga in [
            // Cio' che il vocabolario assolveva.
            "ls -la",
            "git status --short",
            "cat package.json",
            "git log --oneline | head -20",
            "pwd && ls src",
            // Cio' che il vocabolario gia' non assolveva, e non deve iniziare.
            "dotnet ef database update",
            "git push origin main",
            "ls && npm run build",
            "cat piano.md > src/main.rs",
            "FOO=1 ls",
            // Le righe REALI dei due progetti misurati il 18/08/2026.
            "curl -s http://localhost:36526/api/libri",
            "npm install",
            "npx jest --coverage",
            "node --test calcolatrice.test.js",
            "chmod +x start_backend.sh",
        ] {
            let p = portata("run_command", json!({ "command": riga }));
            assert_eq!(
                p,
                StepReach::Unconfined,
                "'{riga}': chi esegue una riga di shell non e' confinabile"
            );
            assert_eq!(p.livello_minimo(), StepCriticality::Critical);
        }
    }
    /// La collocazione e' il punto unico anche del declassamento in step_gate:
    /// fuori dall'albero il nome di una cartella non dice piu' di chi sia.
    #[test]
    fn colloca_path_e_punto_unico_del_rigenerabile() {
        let a = artefatti();
        assert!(path_rigenerabile("node_modules/.cache", &a));
        assert!(path_rigenerabile("./dist", &a));
        assert!(!path_rigenerabile("src", &a));
        assert!(!path_rigenerabile("../node_modules", &a));
        assert!(!path_rigenerabile("/node_modules", &a));
        assert_eq!(colloca_path("src/app.ts", &a), CollocazionePath::DentroAlbero);
    }
}
