//! PUNTO UNICO (regola L) della domanda: «questa richiesta costruisce
//! un'interfaccia senza dire come deve essere?».
//!
//! Se la risposta e' si', chiedere PRIMA costa un turno; indovinare costa un
//! run intero, e il risultato va rifatto. Ma una domanda a ogni task e'
//! peggio del difetto che cura: il gate deve scattare solo quando l'indicazione
//! manca DAVVERO e il task e' davvero di interfaccia.
//!
//! Il vocabolario d'ambito e' lo STESSO dell'asse `ui` del consiglio
//! (`orchestrator.council_ui_keywords`): due liste separate direbbero "questo
//! task riguarda l'interfaccia" in due modi, e divergerebbero alla prima
//! aggiunta. Qui si legge quella, non una sua copia.
//!
//! La decisione e' PURA e testabile senza DB ([`needs_style_clarification`]);
//! il caricamento dei vocabolari e' l'unico confine con l'esterno.

use sqlx::PgPool;

/// Vocabolari e interruttore del gate, dal DB (regola G).
#[derive(Debug, Clone, Default)]
pub struct UiClarificationConfig {
    /// Kill-switch `agent.ui_style_clarification_enabled`.
    pub enabled: bool,
    /// Ambito interfaccia: la stessa chiave che convoca la figura UI.
    pub ui_keywords: Vec<String>,
    /// Segnali che un'indicazione di stile/layout C'E' GIA':
    /// `agent.ui_style_indication_keywords`.
    pub style_keywords: Vec<String>,
}

/// Perche' il gate NON ha chiesto. Enum e non `bool` perche' i motivi finiscono
/// nei log: «non ho chiesto» senza il motivo e' indistinguibile da «il gate non
/// gira piu'» (regola M).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Kill-switch spento, o vocabolario d'ambito non configurato.
    Disabled,
    /// Il task non riguarda un'interfaccia.
    NotUiTask,
    /// La richiesta dice gia' come deve essere (stile, riferimento, sistema di
    /// componenti).
    StyleAlreadyStated,
    /// Ci sono allegati: un mockup, uno screenshot o un file Figma SONO
    /// l'indicazione, e piu' precisi di qualunque risposta a parole.
    AttachmentsProvided,
    /// L'abbiamo gia' chiesto in questa sessione e questo messaggio e' la
    /// risposta. Senza questo controllo il gate richiederebbe all'infinito:
    /// la risposta dell'utente non contiene per forza le parole che il gate
    /// cerca.
    AlreadyAsked,
}

/// Esito del gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClarificationDecision {
    Ask,
    Skip(SkipReason),
}

impl ClarificationDecision {
    pub fn should_ask(self) -> bool {
        matches!(self, Self::Ask)
    }

    /// Codice macchina per i log (regola M).
    pub fn reason_code(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Skip(SkipReason::Disabled) => "disabled",
            Self::Skip(SkipReason::NotUiTask) => "not_ui_task",
            Self::Skip(SkipReason::StyleAlreadyStated) => "style_already_stated",
            Self::Skip(SkipReason::AttachmentsProvided) => "attachments_provided",
            Self::Skip(SkipReason::AlreadyAsked) => "already_asked",
        }
    }
}

/// Fatti del turno che il gate osserva, oltre al testo. Raggruppati in una
/// struct perche' sono tre booleani dello stesso tipo: passarli sciolti rende
/// invisibile uno scambio fra due di essi al punto di chiamata.
#[derive(Debug, Clone, Copy)]
pub struct TurnFacts {
    /// Il messaggio porta allegati.
    pub has_attachments: bool,
    /// In questa sessione abbiamo gia' posto la domanda sullo stile.
    pub already_asked: bool,
}

/// La decisione, PURA. Ordine dei controlli: prima quelli che spengono il gate,
/// poi quelli che leggono la richiesta.
pub fn needs_style_clarification(
    user_text: &str,
    cfg: &UiClarificationConfig,
    facts: TurnFacts,
) -> ClarificationDecision {
    use ClarificationDecision::{Ask, Skip};
    if !cfg.enabled || cfg.ui_keywords.is_empty() {
        return Skip(SkipReason::Disabled);
    }
    if facts.already_asked {
        return Skip(SkipReason::AlreadyAsked);
    }
    if !crate::prompt_templates::touches_domain_keyword(user_text, &cfg.ui_keywords) {
        return Skip(SkipReason::NotUiTask);
    }
    if facts.has_attachments {
        return Skip(SkipReason::AttachmentsProvided);
    }
    if crate::prompt_templates::touches_domain_keyword(user_text, &cfg.style_keywords) {
        return Skip(SkipReason::StyleAlreadyStated);
    }
    Ask
}

/// Carica i vocabolari dal DB. Il gate e' spento se manca il suo interruttore:
/// una funzionalita' che INTERROMPE il turno dell'utente non si accende da
/// sola perche' una chiave non e' stata seminata.
pub async fn read_config(db: &PgPool) -> UiClarificationConfig {
    let enabled = nexus_auth::get_bool_setting(db, "agent.ui_style_clarification_enabled")
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    if !enabled {
        return UiClarificationConfig::default();
    }
    UiClarificationConfig {
        enabled,
        // La stessa chiave dell'asse `ui` del consiglio: un solo vocabolario.
        ui_keywords: nexus_auth::get_csv_setting(db, "orchestrator.council_ui_keywords").await,
        style_keywords: nexus_auth::get_csv_setting(db, "agent.ui_style_indication_keywords").await,
    }
}

/// Testo della domanda posta all'utente.
///
/// Chiede POCO e in modo concreto: tre righe, con la possibilita' esplicita di
/// non decidere. Una domanda lunga davanti a un compito breve viene saltata, e
/// il gate diventa un ostacolo invece di un risparmio.
pub fn clarification_question() -> String {
    "Prima di costruirla: hai un riferimento per l'aspetto dell'interfaccia?\n\n\
     - **Un riferimento**: un'app o un sito a cui somigliare, uno screenshot, un mockup \
     (puoi allegarlo).\n\
     - **Uno stile**: per esempio essenziale e compatto, oppure ampio e arioso; \
     chiaro o scuro; una libreria di componenti gia' scelta.\n\
     - **Nessuna preferenza**: rispondi *procedi* e uso i layout di riferimento standard \
     (elenco con form, panoramica, procedura a passi) scegliendo io la struttura.\n\n\
     Se hai vincoli sul dispositivo (soprattutto telefono, oppure solo desktop) dimmelo qui."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> UiClarificationConfig {
        UiClarificationConfig {
            enabled: true,
            ui_keywords: vec!["app".into(), "interfaccia".into(), "dashboard".into()],
            style_keywords: vec![
                "stile".into(),
                "come".into(),
                "tailwind".into(),
                "mockup".into(),
            ],
        }
    }

    fn facts() -> TurnFacts {
        TurnFacts {
            has_attachments: false,
            already_asked: false,
        }
    }

    /// Il caso che apre il capitolo: si chiede un'app, non si dice come deve
    /// essere fatta.
    #[test]
    fn chiede_quando_lindicazione_manca() {
        let d = needs_style_clarification(
            "creami un'app per la gestione delle spese",
            &cfg(),
            facts(),
        );
        assert_eq!(d, ClarificationDecision::Ask);
        assert!(d.should_ask());
    }

    #[test]
    fn non_chiede_se_il_task_non_e_di_interfaccia() {
        let d = needs_style_clarification("aggiungi un indice alla tabella ordini", &cfg(), facts());
        assert_eq!(d, ClarificationDecision::Skip(SkipReason::NotUiTask));
    }

    #[test]
    fn non_chiede_se_lo_stile_e_gia_dichiarato() {
        let d = needs_style_clarification(
            "creami un'app spese con stile essenziale in tailwind",
            &cfg(),
            facts(),
        );
        assert_eq!(
            d,
            ClarificationDecision::Skip(SkipReason::StyleAlreadyStated)
        );
    }

    /// Un mockup allegato e' un'indicazione migliore di qualunque risposta a
    /// parole: chiedere sarebbe far ripetere all'utente cio' che ha gia' dato.
    #[test]
    fn non_chiede_se_ci_sono_allegati() {
        let d = needs_style_clarification(
            "creami un'app per le spese",
            &cfg(),
            TurnFacts {
                has_attachments: true,
                ..facts()
            },
        );
        assert_eq!(
            d,
            ClarificationDecision::Skip(SkipReason::AttachmentsProvided)
        );
    }

    /// Senza questo controllo il gate non termina: la risposta dell'utente
    /// ("procedi") non contiene le parole di stile, quindi rientrerebbe nel
    /// ramo Ask a ogni giro.
    #[test]
    fn non_richiede_due_volte_nella_stessa_sessione() {
        let d = needs_style_clarification(
            "procedi",
            &cfg(),
            TurnFacts {
                already_asked: true,
                ..facts()
            },
        );
        assert_eq!(d, ClarificationDecision::Skip(SkipReason::AlreadyAsked));
        // E vale anche per un secondo messaggio che riparla di interfaccia.
        let d2 = needs_style_clarification(
            "rifai la dashboard dell'app",
            &cfg(),
            TurnFacts {
                already_asked: true,
                ..facts()
            },
        );
        assert_eq!(d2, ClarificationDecision::Skip(SkipReason::AlreadyAsked));
    }

    #[test]
    fn spento_di_default_e_senza_vocabolario() {
        let spento = UiClarificationConfig {
            enabled: false,
            ..cfg()
        };
        assert_eq!(
            needs_style_clarification("creami un'app", &spento, facts()),
            ClarificationDecision::Skip(SkipReason::Disabled)
        );
        let senza_vocabolario = UiClarificationConfig {
            ui_keywords: vec![],
            ..cfg()
        };
        assert_eq!(
            needs_style_clarification("creami un'app", &senza_vocabolario, facts()),
            ClarificationDecision::Skip(SkipReason::Disabled)
        );
        assert!(
            !UiClarificationConfig::default().enabled,
            "un gate che interrompe il turno non si accende da solo"
        );
    }

    /// I motivi finiscono nei log: se due skip diversi avessero lo stesso
    /// codice, la diagnosi non potrebbe distinguerli.
    #[test]
    fn i_codici_dei_motivi_sono_distinti() {
        let codici = [
            ClarificationDecision::Ask,
            ClarificationDecision::Skip(SkipReason::Disabled),
            ClarificationDecision::Skip(SkipReason::NotUiTask),
            ClarificationDecision::Skip(SkipReason::StyleAlreadyStated),
            ClarificationDecision::Skip(SkipReason::AttachmentsProvided),
            ClarificationDecision::Skip(SkipReason::AlreadyAsked),
        ]
        .map(ClarificationDecision::reason_code);
        let mut unici = codici.to_vec();
        unici.sort_unstable();
        unici.dedup();
        assert_eq!(unici.len(), codici.len(), "codici duplicati: {codici:?}");
    }
}
