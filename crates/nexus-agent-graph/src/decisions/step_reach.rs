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
//! # La soglia sul costo, e perche' e' un ELENCO CHE ASSOLVE
//!
//! Con la sola inversione qui sopra ogni `run_command` diventa `Unconfined`, e
//! in `enforce` questo significa due chiamate LLM prima di `ls`, `cat`,
//! `git status`. Il costo e' il limite vero di questo gate: renderlo
//! insostenibile e' il modo piu' sicuro di farlo spegnere, cioe' di tornare al
//! punto di partenza per un'altra strada.
//!
//! La soglia e' [`StepReach::Observation`], ed e' un elenco — ma di POLARITA'
//! opposta a quello che il difetto ha prodotto, e l'asimmetria e' tutto:
//!
//! - `critical_step_rules` e' un elenco che ACCUSA. Cio' che non nomina passa.
//!   La sua incompletezza costa SICUREZZA, e non si vede: `dotnet ef database
//!   update` e' passato cinque volte senza che nulla lo dichiarasse;
//! - `observation_commands` e' un elenco che ASSOLVE. Cio' che non nomina viene
//!   GIUDICATO. La sua incompletezza costa DENARO e LATENZA, e si vede subito —
//!   un giudice convocato su un `tree` che nessuno aveva previsto e' rumore
//!   misurabile, non un buco.
//!
//! Un elenco incompleto che fallisce verso il giudizio non e' la stessa cosa di
//! un elenco incompleto che fallisce verso il passaggio, ed e' il motivo per cui
//! qui un elenco e' ammesso e li' no. Vocabolario in DB (regola G, chiave
//! `orchestrator.step_reach.observation_commands`, mig 0688): VUOTO significa
//! «nulla e' provatamente innocuo», cioe' tutto e' giudicato — nessun ripiego
//! cablato nel codice, e il ripiego mancante fallisce verso la cautela.
//!
//! Il riconoscimento passa dallo scompositore unico
//! [`super::shell_command::comandi`], mai da un `contains` sulla riga: una voce
//! del vocabolario e' un PREFISSO DI PAROLE, quindi `git status` assolve
//! `git status --short` e non ha nulla da dire su `git push`.

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
    /// Il tool esegue una riga, e OGNI comando di quella riga e' stato
    /// riconosciuto come osservazione (vocabolario DB, nessuna redirezione,
    /// nessun env inline). E' l'unica variante che ASSOLVE, e assolve per
    /// riconoscimento positivo: cio' che non e' riconosciuto non finisce qui.
    Observation,
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
            Self::Observation => "observation",
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
            // Riconosciuta come osservazione: non e' «non l'ho vista», e'
            // «l'ho vista e non tocca nulla». Sotto la soglia del gate.
            Self::ReadOnly | Self::Observation => StepCriticality::ReadOnly,
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
            Self::Observation => {
                "esegue una riga fatta di soli comandi di osservazione riconosciuti"
            }
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

/// Classifica la portata di UN passo. PURA: i tre vocabolari (mutatori,
/// artefatti rigenerabili, comandi di osservazione) arrivano dal chiamante
/// (regola G), niente letture qui.
///
/// L'ordine dei criteri non e' arbitrario: si guarda PRIMA se il passo esegue
/// una riga, poi dove scrive. Un tool che esegue e nomina anche un path (es.
/// `--output`) resta non confinato — la riga puo' fare molto piu' di quel file.
pub fn classifica_portata(
    tool_name: &str,
    tool_input: &Value,
    fs_mutator_tools: &[String],
    artefatti_rigenerabili: &[String],
    comandi_di_osservazione: &[String],
) -> StepReach {
    if !super::hitl::is_mutator_tool_name(tool_name, fs_mutator_tools) {
        return StepReach::ReadOnly;
    }
    if let Some(riga) = CAMPI_RIGA_ESEGUITA
        .iter()
        .find_map(|c| tool_input.get(c).and_then(Value::as_str))
    {
        // L'unica assoluzione, e per riconoscimento POSITIVO: una riga che non
        // si e' potuta riconoscere resta non confinata.
        return if riga_e_osservazione(riga, comandi_di_osservazione) {
            StepReach::Observation
        } else {
            StepReach::Unconfined
        };
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

/// La riga e' fatta di SOLI comandi di osservazione riconosciuti?
///
/// Delega la scomposizione al punto unico [`super::shell_command::comandi`]
/// (regola L): mai un `contains` sulla riga, che non distingue un comando
/// ESEGUITO da un comando NOMINATO — e' lo stesso principio per cui il matcher
/// delle regole lessicali guarda i token e non il testo.
///
/// Tre condizioni, e devono valere per OGNI comando della catena:
///
/// 1. le sue prime parole coincidono con una voce del vocabolario. La voce e'
///    un prefisso di PAROLE, non una sottostringa: `git status` assolve
///    `git status --short` e non dice nulla su `git push`;
/// 2. nessuna redirezione. `cat piano.md > src/main.rs` e' fatto di soli
///    comandi di osservazione e scrive un file. Il campo `redirezioni` copre
///    anche `2>&1`, che non scrive nulla di interessante: escluderlo e' il
///    verso conservativo, e costa una convocazione, non un buco;
/// 3. nessuna assegnazione env in testa. `FOO=1 ls` e' innocuo, ma il
///    vocabolario assolve un PROGRAMMA e non l'ambiente in cui gira, e questa
///    e' la meta' su cui non si vuole ragionare caso per caso.
///
/// Vocabolario vuoto -> `false` per costruzione: nessun comando e' provatamente
/// innocuo finche' qualcuno non lo ha dichiarato tale (regola G, niente
/// ripiego cablato — e qui il ripiego mancante fallisce verso il giudizio).
///
/// Sul campo `sql` la scomposizione shell e' una tokenizzazione grossolana: e'
/// ammesso perche' nessuna statement SQL puo' coincidere col prefisso di una
/// voce del vocabolario, quindi l'esito e' `Unconfined`, cioe' il lato cauto.
fn riga_e_osservazione(riga: &str, vocabolario: &[String]) -> bool {
    if vocabolario.is_empty() {
        return false;
    }
    let comandi = super::shell_command::comandi(riga);
    if comandi.is_empty() {
        return false;
    }
    comandi.iter().all(|c| {
        !c.redirezioni && c.env.is_empty() && vocabolario.iter().any(|v| prefisso_di(v, &c.parole))
    })
}

/// La voce `voce` (parole separate da spazi) e' il prefisso di `parole`?
/// Confronto case-insensitive sul solo nome: i percorsi non sono normalizzati,
/// quindi `/usr/bin/ls` non e' `ls` — e non lo assolve.
fn prefisso_di(voce: &str, parole: &[String]) -> bool {
    let attese: Vec<&str> = voce.split_whitespace().collect();
    if attese.is_empty() || attese.len() > parole.len() {
        return false;
    }
    attese
        .iter()
        .zip(parole)
        .all(|(a, p)| a.eq_ignore_ascii_case(p))
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

    /// Il vocabolario che assolve, nella forma seminata dalla mig 0688 MENO
    /// `tree`: serve un comando innocuo ma NON dichiarato, per provare che
    /// l'assoluzione e' per riconoscimento e non per innocuita' apparente.
    fn osservazione() -> Vec<String> {
        [
            "ls", "pwd", "cat", "head", "tail", "wc", "echo", "which", "whoami", "date",
            "printenv", "grep", "rg", "git status", "git diff", "git log", "git show",
            "node --version", "dotnet --version",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    fn portata(tool: &str, input: Value) -> StepReach {
        classifica_portata(
            tool,
            &input,
            &mutatori(),
            &artefatti(),
            &osservazione(),
        )
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

    /// LA SOGLIA SUL COSTO. Senza di essa il criterio metterebbe due giudici
    /// davanti a `ls`, e un gate insostenibile e' un gate che verra' spento —
    /// cioe' lo stesso punto di partenza per un'altra strada.
    ///
    /// L'assoluzione e' per RICONOSCIMENTO POSITIVO: quel che il vocabolario
    /// non nomina resta giudicato. E' l'asimmetria per cui qui un elenco e'
    /// ammesso e in `critical_step_rules` no.
    ///
    /// MUTAZIONE: far ritornare `true` a `riga_e_osservazione` quando il
    /// vocabolario non riconosce il comando (cioe' assolvere per default)
    /// fa cadere ogni asserzione del secondo blocco, `dotnet ef` compreso.
    #[test]
    fn solo_l_osservazione_riconosciuta_assolve() {
        for riga in [
            "ls -la",
            "git status --short",
            "cat package.json",
            "git log --oneline | head -20",
            "pwd && ls src",
        ] {
            let p = portata("run_command", json!({ "command": riga }));
            assert_eq!(p, StepReach::Observation, "'{riga}' e' osservazione pura");
            assert_eq!(p.livello_minimo(), StepCriticality::ReadOnly);
        }
        for riga in [
            // Il caso che il modulo esiste per prendere.
            "dotnet ef database update",
            // Stesso programma, sottocomando diverso: la voce e' un prefisso di
            // PAROLE, quindi `git status` non ha nulla da dire qui.
            "git push origin main",
            "git checkout -- .",
            // Un comando di osservazione in catena con uno sconosciuto non
            // assolve la catena: `all`, non `any`.
            "ls && npm run build",
            // Innocuo quanto un `ls`, e non dichiarato: viene giudicato lo
            // stesso. L'assoluzione e' per RICONOSCIMENTO, mai per innocuita'
            // apparente — costa una convocazione, mai un buco.
            "tree -L 2",
        ] {
            assert_eq!(
                portata("run_command", json!({ "command": riga })),
                StepReach::Unconfined,
                "'{riga}': cio' che non e' riconosciuto viene giudicato"
            );
        }
        // E la cura di quel costo e' un DATO, non una patch: dichiarare `tree`
        // nel vocabolario lo assolve, senza toccare una riga di codice
        // (regola G). E' l'asimmetria per cui qui un elenco e' ammesso.
        assert_eq!(
            classifica_portata(
                "run_command",
                &json!({"command": "tree -L 2"}),
                &mutatori(),
                &artefatti(),
                &["tree".to_string()],
            ),
            StepReach::Observation
        );
    }

    /// Le due condizioni che un elenco di soli PROGRAMMI non puo' esprimere:
    /// un comando di osservazione REDIRETTO scrive, e un vocabolario vuoto non
    /// assolve nessuno.
    ///
    /// MUTAZIONE: togliere il controllo su `redirezioni` fa passare
    /// `cat piano.md > src/main.rs` come osservazione — una scrittura di file
    /// sotto la soglia del gate.
    #[test]
    fn la_redirezione_e_il_vocabolario_vuoto_non_assolvono() {
        assert_eq!(
            portata("run_command", json!({"command": "cat piano.md > src/main.rs"})),
            StepReach::Unconfined,
            "una riga di soli comandi di osservazione che REDIRIGE scrive un file"
        );
        assert_eq!(
            portata("run_command", json!({"command": "ls 2>&1"})),
            StepReach::Unconfined,
            "verso conservativo: costa una convocazione, non un buco"
        );
        assert_eq!(
            portata("run_command", json!({"command": "FOO=1 ls"})),
            StepReach::Unconfined,
            "il vocabolario assolve un programma, non l'ambiente in cui gira"
        );
        // Vocabolario assente = nulla e' provatamente innocuo (regola G:
        // niente ripiego cablato, e il ripiego mancante e' quello cauto).
        assert_eq!(
            classifica_portata(
                "run_command",
                &json!({"command": "ls -la"}),
                &mutatori(),
                &artefatti(),
                &[],
            ),
            StepReach::Unconfined
        );
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
