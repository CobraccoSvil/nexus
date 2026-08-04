//! Punto unico (regola L): «questa figura da' un PARERE, o mette le mani nel
//! codice?».
//!
//! La domanda ha due consumatori che finora non si parlavano, e la divergenza
//! e' costata un progetto intero.
//!
//! Il contratto era gia' scritto — `admin-service::figures::validate_advisory_readonly`,
//! «una figura advisory analizza e non muta lo stato» — ma valeva solo per le
//! figure create dal wizard admin. Quelle seedate da migrazione non ci sono mai
//! passate, e alla CONVOCAZIONE nessuno riapriva il contratto:
//! `select_council_figures` accettava qualunque nome il classificatore avesse
//! dichiarato come competenza pertinente.
//!
//! MISURATO il 04/08/2026 su prenotazioni-sala. Quattro figure convocate per
//! dare un parere — `test_author`, `frontend_implementer`, `implement`,
//! `db_architect` — hanno ricevuto lo STESSO mandato (il testo utente
//! integrale, un solo md5 su otto righe) e hanno scritto 107 file, ciascuna il
//! proprio stack completo. Il risultato sul disco: `backend/Cargo.toml` e
//! `backend/package.json` nella stessa cartella, Rust e Node insieme. L'app non
//! e' mai partita.
//!
//! La lettura sbagliata sarebbe «hanno scritto INSIEME»: la catena
//! `before_sha256` -> `after_sha256` in `file_mutations` e' intatta su 14 righe
//! su 14, nessuna scrittura troncata, nessun lost-update. Serializzarle non
//! avrebbe cambiato una riga — la seconda avrebbe trovato l'applicazione della
//! prima e l'avrebbe sostituita con la propria. Il difetto non e' che
//! scrivevano insieme: e' che scrivevano.
//!
//! Il criterio non e' un elenco di nomi (`test_author` e' scrittrice oggi, e un
//! kind nuovo domani non sarebbe nell'elenco): e' cosa la figura PUO' FARE,
//! dichiarato dalla sua `tool_whitelist`.

/// Tool con cui una figura dichiara il proprio verdetto al Consiglio.
///
/// Non e' un dettaglio di forma: e' il canale STRUTTURATO su cui il verdetto
/// viene contato (regola Q). Una figura che non lo possiede non puo' esprimere
/// un parere che qualcuno raccolga — comunque la si convochi.
pub const ADVISORY_VERDICT_TOOL: &str = "advisory_verdict";

/// I tool di `tools` che mutano lo stato, secondo il vocabolario dato.
///
/// Appartenenza ESATTA, mai per prefisso o sottostringa: `write_file_x` non e'
/// `write_file`.
pub fn mutator_tools_in(tools: &[String], mutator_tools: &[String]) -> Vec<String> {
    tools
        .iter()
        .filter(|t| mutator_tools.iter().any(|m| m == *t))
        .cloned()
        .collect()
}

/// `true` se questa `tool_whitelist` e' quella di una figura advisory: sa
/// dichiarare un verdetto e non sa mutare nulla.
///
/// Le due condizioni sono una promessa sola («analizza, non tocca») e vanno
/// verificate insieme: una figura che dichiara il verdetto E scrive non e' meta'
/// advisory, e' una scrittrice che parla.
///
/// `mutator_tools` arriva dal chiamante (regola G: la configurazione e' un
/// parametro, cosi' la funzione resta pura e testabile) e viene da
/// `settings.agent.tools.result_cache_mutators`, lo stesso vocabolario che
/// governa il gate HITL.
pub fn is_advisory_kind(tool_whitelist: &[String], mutator_tools: &[String]) -> bool {
    tool_whitelist.iter().any(|t| t == ADVISORY_VERDICT_TOOL)
        && mutator_tools_in(tool_whitelist, mutator_tools).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mutatori() -> Vec<String> {
        // Estratto del vocabolario reale (`agent.tools.result_cache_mutators`).
        ["write_file", "edit_file", "delete_file", "run_command", "run_service"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn tools(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// Le whitelist REALI misurate il 04/08/2026 su `nexus_subagent_definitions`.
    ///
    /// MUTAZIONE: togliere la clausola `mutator_tools_in(...).is_empty()` da
    /// `is_advisory_kind` non fa rosseggiare nulla qui (nessuna figura reale ha
    /// entrambi); toglierla insieme al controllo su `ADVISORY_VERDICT_TOOL` fa
    /// passare per advisory tutte e quattro le scrittrici del disastro.
    #[test]
    fn le_figure_che_scrivono_non_danno_pareri() {
        // Le otto figure advisory reali: advisory_verdict, nessun mutatore.
        for f in ["functional_analyst", "software_architect", "security_engineer"] {
            assert!(
                is_advisory_kind(&tools(&["advisory_verdict", "read_file", "list_files"]), &mutatori()),
                "{f} deve restare advisory"
            );
        }
        // Le quattro del disastro: nessun advisory_verdict, tool che scrivono.
        let scrittrice = tools(&["write_file", "edit_file", "run_command", "read_file"]);
        assert!(
            !is_advisory_kind(&scrittrice, &mutatori()),
            "test_author/frontend_implementer/implement/db_architect scrivono: non sono pareri"
        );
        // `review` e `ui_reviewer`: nessun verdetto strutturato e run_command.
        assert!(!is_advisory_kind(&tools(&["run_command", "read_file"]), &mutatori()));
    }

    #[test]
    fn le_due_condizioni_valgono_insieme() {
        // Sa dichiarare il verdetto MA scrive: non e' meta' advisory.
        assert!(!is_advisory_kind(
            &tools(&["advisory_verdict", "write_file"]),
            &mutatori()
        ));
        // Non scrive ma non sa dichiarare: il suo parere non verrebbe contato.
        assert!(!is_advisory_kind(&tools(&["read_file", "list_files"]), &mutatori()));
        // Nessun tool: non fa niente, tanto meno un parere.
        assert!(!is_advisory_kind(&[], &mutatori()));
    }

    #[test]
    fn l_appartenenza_e_esatta_non_per_prefisso() {
        // `write_file_x` non e' `write_file`: un match per sottostringa
        // escluderebbe figure legittime.
        assert!(is_advisory_kind(
            &tools(&["advisory_verdict", "write_file_x"]),
            &mutatori()
        ));
        assert_eq!(
            mutator_tools_in(&tools(&["read_file", "write_file_x", "edit_file"]), &mutatori()),
            vec!["edit_file".to_string()]
        );
    }
}
