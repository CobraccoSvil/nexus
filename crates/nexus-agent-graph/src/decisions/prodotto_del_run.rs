//! Punto unico (regola L): «che cosa deve PRODURRE questo run — il LAVORO, o un
//! PARERE su di esso?» E, di conseguenza, puo' decomporre il compito e delegarlo?
//!
//! La domanda non era ponibile, e il gate d'ingresso alla plan-phase
//! ([`crate::nodes::PlannerConfig::is_eligible`]) rispondeva a tutt'altro: i suoi
//! quattro cancelli guardano il MODO (`behavior_mode`), l'INTENTO (`intent`) e il
//! BUDGET, cioe' «questo compito merita un piano?». Nessuno chiede «pianificare e
//! delegare e' il MIO mestiere?», che e' una proprieta' del run, non del compito.
//!
//! MISURATO il 10/08/2026 sul progetto `batteria-todo-app`, una todo app come
//! pagina HTML statica su progetto VUOTO. Sul disco sono comparsi TRE alberi
//! paralleli e incompleti — `/index.html` + `/style.css` + `/app.js` + `/script.js`,
//! `/todo-app/index.html`, `/todo-app-static/index.html` — e per i primi sei
//! secondi tre run diversi si sono sovrascritti a vicenda lo stesso
//! `index.html` (453 -> 526 -> 284 -> 453 -> 284 byte, `file_mutations`).
//!
//! La lettura sbagliata sarebbe «il modello disperde la struttura». I dati dicono
//! altro: **18 dispatcher distinti**, ciascuno con un piano PROPRIO da 5-8 passi
//! dell'INTERA app, per un totale di **99 sub-run `implement`**, 5,6M token e
//! ~$0,42 per una pagina statica. I passi portavano nomi diversi per la stessa
//! cosa — `html`, `html_structure`, `html_skeleton`, `index_html`, `create_index` —
//! perche' erano piani indipendenti, non un piano reso piu' volte. Il compito non
//! e' stato pianificato una volta: e' stato pianificato diciotto volte ed eseguito
//! diciotto volte.
//!
//! **Undici di quei diciotto dispatcher erano figure ADVISORY** —
//! `provider_analyst`, `ui_ux_designer`, `program_manager`, `functional_analyst`,
//! `software_architect`, `security_engineer`, `project_manager` — e i CINQUE run
//! che si sono contesi `index.html` erano tutti figli loro. Le figure advisory
//! sono l'intera provenienza dei tre alberi.
//!
//! ## Perche' il contratto advisory non bastava
//!
//! [`nexus_types::figure_advisory::is_advisory_kind`] dichiara la promessa
//! («analizza, non tocca») e la verifica sulla `tool_whitelist`: verdetto
//! strutturato presente, nessun tool mutatore. Il roster reale la rispetta —
//! `ui_ux_designer` ha undici tool e nemmeno uno scrive.
//!
//! Il contratto era enforced solo contro le MANI della figura. Delegare e' una
//! mano: [`crate::nodes::TodoRunnerNode`] esegue ogni todo chiamando
//! `dispatch_subagents` come NODO, non come tool scelto dal modello, quindi la
//! whitelist non lo vede nemmeno passare. Una figura convocata per commentare
//! riceveva un planner, ne usciva con un piano dell'app intera e la faceva
//! scrivere da otto figli che i tool per scrivere li avevano.
//!
//! Il comportamento osservato era, di nuovo, l'unico possibile — la stessa forma
//! della mig 0697 (`assets` come file vuoto perche' `fs_mkdir` non era in nessuna
//! whitelist): il mandato del Consiglio e' il testo utente INTEGRALE, quindi
//! all'imperativo («Crea una todo app come pagina HTML statica: ...»), e la sola
//! strada per obbedirlo, con le mani legate, era delegarlo.
//!
//! ## Il criterio, e perche' non e' un elenco di nomi
//!
//! Non «e' un sub-run» (un sub-run puo' legittimamente avere il lavoro come
//! prodotto) e non «e' uno di questi kind» (un kind nuovo domani non sarebbe
//! nell'elenco). E' cio' che il run deve CONSEGNARE, che discende dal contratto
//! della figura ed e' deciso all'origine, dove la whitelist si conosce
//! (`subagent_native::prepare_subagent_run`), MAI dedotto qui dal nome.
//!
//! ## Portata deliberata
//!
//! Chiude la delega delle figure ADVISORY. NON tocca l'annidamento delle figure
//! che hanno il lavoro come prodotto: cinque `implement` hanno ri-pianificato
//! l'app intera da un passo solo (`0c4839b5`, mandato «(init_files) Crea la
//! struttura di base», piano `create_app`/`create_index`/`create_style`/
//! `verify_all`), ed e' un difetto vicino ma DIVERSO — li' il prodotto e'
//! davvero il lavoro, e la domanda giusta e' se il mandato ricevuto autorizzi a
//! ridecomporlo, non se pianificare sia il suo mestiere. Restringere anche quello
//! da qui vorrebbe dire rispondere a due domande con un tipo solo.

use serde::{Deserialize, Serialize};

/// Che cosa questo run deve consegnare.
///
/// `Lavoro` e' il [`Default`] deliberato: un run che non dichiara nulla e' il run
/// principale o una figura che lavora, e per entrambi il comportamento resta
/// quello di prima. L'assenza di dichiarazione non deve MAI stringere il
/// comportamento di chi il campo non lo popola.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProdottoDelRun {
    /// Il run deve produrre il LAVORO: puo' decomporlo in passi e delegarli.
    #[default]
    Lavoro,
    /// Il run deve produrre un PARERE sul lavoro di altri: analizza e dichiara un
    /// verdetto, e non produce artefatti — ne' con le proprie mani ne' per
    /// interposta figura.
    Parere,
}

impl ProdottoDelRun {
    /// Questo run puo' entrare nella plan-phase, cioe' decomporre il compito e
    /// delegarne i passi a sub-run?
    ///
    /// E' il criterio, e i due consumatori lo interrogano invece di riscriverlo
    /// (regola L): il cancello dell'edge `understanding -> planner|executor`
    /// dentro [`crate::nodes::PlannerConfig::is_eligible`], e il
    /// [`crate::nodes::PlannerNode`] come precondizione PRIMA del gate
    /// orchestrazione — che altrimenti, quando acceso, potrebbe scavalcare
    /// `is_eligible` e riaprire da solo la strada che questo tipo chiude.
    pub fn decompone_e_delega(self) -> bool {
        matches!(self, Self::Lavoro)
    }

    /// Motivo dello skip da persistire nel delta del planner, quando la
    /// plan-phase e' preclusa dal prodotto del run.
    ///
    /// `None` per `Lavoro`: li' non c'e' nessuno skip da spiegare, e uno skip
    /// senza motivo non si distinguerebbe da un passaggio non avvenuto (regola Q).
    ///
    /// Identificatore in INGLESE (regola N) anche se le varianti dell'enum sono in
    /// italiano: le due cose vivono in posti diversi. Le varianti restano
    /// in-process, questa stringa finisce nel campo `plan_phase_skip_reason`
    /// accanto a `skip_in_subagent`, `budget_too_low` e `not_complex` — chi legge
    /// quel campo per capire perche' un run non ha pianificato deve trovare UN
    /// vocabolario, non due.
    pub fn motivo_dello_skip(self) -> Option<&'static str> {
        match self {
            Self::Lavoro => None,
            Self::Parere => Some("skip_advisory_product"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Il criterio, nelle due direzioni.
    ///
    /// MUTAZIONE: invertire `matches!(self, Self::Lavoro)` in `Self::Parere` fa
    /// rosseggiare entrambe le asserzioni; farlo ritornare sempre `true` — il
    /// difetto reale, cioe' il criterio assente — fa rosseggiare la seconda, che
    /// e' quella dei diciotto dispatcher.
    #[test]
    fn solo_chi_produce_il_lavoro_decompone() {
        assert!(
            ProdottoDelRun::Lavoro.decompone_e_delega(),
            "il run che deve produrre il lavoro pianifica come prima"
        );
        assert!(
            !ProdottoDelRun::Parere.decompone_e_delega(),
            "una figura advisory non delega: era la provenienza dei tre alberi"
        );
    }

    /// Il default non stringe nessuno: chi non dichiara si comporta come prima.
    #[test]
    fn il_default_e_il_lavoro() {
        assert_eq!(ProdottoDelRun::default(), ProdottoDelRun::Lavoro);
        assert!(ProdottoDelRun::default().decompone_e_delega());
    }

    /// Lo skip ha un motivo solo dove c'e' uno skip.
    #[test]
    fn il_motivo_esiste_solo_per_il_parere() {
        assert_eq!(ProdottoDelRun::Lavoro.motivo_dello_skip(), None);
        assert_eq!(
            ProdottoDelRun::Parere.motivo_dello_skip(),
            Some("skip_advisory_product")
        );
    }

    /// Il campo attraversa il checkpoint: lo stato del grafo si serializza, e un
    /// `Parere` che tornasse `Lavoro` dopo un round-trip riaprirebbe la delega a
    /// meta' run senza che nulla lo dichiari.
    #[test]
    fn sopravvive_al_round_trip_del_checkpoint() {
        for p in [ProdottoDelRun::Lavoro, ProdottoDelRun::Parere] {
            let json = serde_json::to_string(&p).expect("serializza");
            let back: ProdottoDelRun = serde_json::from_str(&json).expect("deserializza");
            assert_eq!(p, back, "round-trip di {p:?}");
        }
        // Identificatori canonici in snake_case (regola N): sono il wire.
        assert_eq!(
            serde_json::to_string(&ProdottoDelRun::Parere).unwrap(),
            "\"parere\""
        );
    }
}
