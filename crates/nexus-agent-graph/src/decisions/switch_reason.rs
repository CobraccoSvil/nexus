//! PUNTO UNICO (regola L) del motivo per cui il motore cambia provider/modello,
//! e della sua descrizione per l'utente.
//!
//! Il motivo viaggiava come `&str` libero verso il payload della card "CAMBIO
//! PROVIDER": `"final_gate_nonconvergence"`, `"signature_loop"`,
//! `"repeated_action"`. Il frontend aveva una tabella scritta a mano
//! (`SWITCH_CAUSE_LABELS`) per la CAUSA strutturata, ma per il motivo no: quando
//! non trovava una corrispondenza mostrava il codice grezzo dentro un `<code>`.
//! Nella card si leggeva quindi `Motivo: final_gate_nonconvergence`, che e' un
//! identificatore per le macchine, non una frase per una persona.
//!
//! Le due copie divergevano per costruzione: aggiungere un motivo nel backend non
//! obbligava nessuno ad aggiungerne la descrizione altrove. Con un vocabolario
//! CHIUSO (regola N) il compilatore chiude la porta: un nuovo motivo e' una nuova
//! variante, e il `match` di [`SwitchReason::descrizione`] non compila finche' non
//! la si descrive.
//!
//! Il codice canonico resta sul wire (`reason`) per la logica e i test; la frase
//! viaggia accanto (`reason_description`). Non si sostituisce l'uno con l'altra:
//! sono due canali, come `message` e `detail` per gli errori.

use serde::{Deserialize, Serialize};

/// Perche' il motore ha cambiato provider o modello.
///
/// Ogni variante e' un fatto diverso, e all'utente interessa la differenza: un
/// provider caduto non e' un modello che non converge, e nessuno dei due e' un
/// agente che si ripete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchReason {
    /// Il provider ha fallito la chiamata: si passa al successivo in catena.
    ProviderFailover,
    /// Il provider risponde ma il turno non produce avanzamento.
    ProviderNoProgress,
    /// Il modello ripete la stessa firma di risposta: e' bloccato su se stesso.
    SignatureLoop,
    /// L'agente rilegge/riesplora senza agire: serve una testa diversa.
    Exploration,
    /// La stessa azione ripetuta senza progresso.
    RepeatedAction,
    /// Recupero da stallo conclamato.
    StallRecovery,
    /// La verifica finale non passa entro i tentativi previsti: si sale di
    /// capacita' (escalation).
    FinalGateNonconvergence,
    /// Tetto di iterazioni raggiunto.
    IterationCap,
    /// Tetto del gate G1 raggiunto (troppi turni senza chiudere un obiettivo).
    G1Cap,
    /// Budget di token del run esaurito.
    BudgetToken,
    /// Budget di tempo del run esaurito.
    TimeBudget,
}

impl SwitchReason {
    /// L'identificatore canonico sul wire (regola N: inglese, univoco). E' cio'
    /// che la logica e i test confrontano; NON e' cio' che si mostra.
    pub fn code(self) -> &'static str {
        match self {
            Self::ProviderFailover => "provider_failover",
            Self::ProviderNoProgress => "provider_no_progress",
            Self::SignatureLoop => "signature_loop",
            Self::Exploration => "exploration",
            Self::RepeatedAction => "repeated_action",
            Self::StallRecovery => "stall_recovery",
            Self::FinalGateNonconvergence => "final_gate_nonconvergence",
            Self::IterationCap => "iteration_cap",
            Self::G1Cap => "g1_cap",
            Self::BudgetToken => "budget_token",
            Self::TimeBudget => "time_budget",
        }
    }

    /// La frase per l'utente. Dice cosa e' successo e perche' e' stato cambiato
    /// il modello -- non ripete il codice con altre parole.
    ///
    /// Aggiungere una variante senza descriverla qui non compila: e' il punto
    /// dell'enum.
    pub fn descrizione(self) -> &'static str {
        match self {
            Self::ProviderFailover => {
                "il provider precedente ha fallito la richiesta: passo al successivo disponibile"
            }
            Self::ProviderNoProgress => {
                "il provider rispondeva senza far avanzare il lavoro: provo con un altro"
            }
            Self::SignatureLoop => {
                "il modello ripeteva la stessa risposta: cambio modello per uscire dal giro"
            }
            Self::Exploration => {
                "troppa esplorazione senza modifiche concrete: passo a un modello piu' deciso"
            }
            Self::RepeatedAction => {
                "la stessa azione veniva ripetuta senza risultato: cambio modello"
            }
            Self::StallRecovery => "il lavoro si era fermato: riparto con un altro modello",
            Self::FinalGateNonconvergence => {
                "la verifica finale non e' stata superata nei tentativi previsti: passo a un modello piu' capace"
            }
            Self::IterationCap => {
                "raggiunto il numero massimo di passaggi: continuo con un modello piu' capace"
            }
            Self::G1Cap => {
                "troppi turni senza chiudere un obiettivo: passo a un modello piu' capace"
            }
            Self::BudgetToken => {
                "esaurito il budget di token previsto per questo lavoro: cambio modello"
            }
            Self::TimeBudget => "esaurito il tempo previsto per questo lavoro: cambio modello",
        }
    }
}

impl std::fmt::Display for SwitchReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tutte le varianti, cosi' i controlli sotto valgono per l'intero
    /// vocabolario e non per il campione che mi e' venuto in mente.
    const TUTTE: &[SwitchReason] = &[
        SwitchReason::ProviderFailover,
        SwitchReason::ProviderNoProgress,
        SwitchReason::SignatureLoop,
        SwitchReason::Exploration,
        SwitchReason::RepeatedAction,
        SwitchReason::StallRecovery,
        SwitchReason::FinalGateNonconvergence,
        SwitchReason::IterationCap,
        SwitchReason::G1Cap,
        SwitchReason::BudgetToken,
        SwitchReason::TimeBudget,
    ];

    /// LA REGRESSIONE: nella card si leggeva `Motivo: final_gate_nonconvergence`.
    ///
    /// Una descrizione che contiene il proprio codice non e' una descrizione: e'
    /// lo stesso identificatore con un accento diverso.
    #[test]
    fn nessuna_descrizione_e_il_codice_travestito() {
        for r in TUTTE {
            let d = r.descrizione();
            assert!(
                !d.contains(r.code()),
                "{}: la descrizione ripete il codice: {d}",
                r.code()
            );
            assert!(
                !d.contains('_'),
                "{}: underscore = identificatore, non frase: {d}",
                r.code()
            );
            assert!(
                d.len() > 25,
                "{}: troppo corta per spiegare qualcosa: {d}",
                r.code()
            );
        }
    }

    /// I codici sono canonici (regola N) e distinti: due motivi diversi non
    /// possono presentarsi con lo stesso identificatore.
    #[test]
    fn i_codici_sono_canonici_e_distinti() {
        let mut visti = std::collections::HashSet::new();
        for r in TUTTE {
            let c = r.code();
            assert!(visti.insert(c), "codice duplicato: {c}");
            assert_eq!(c, c.to_lowercase(), "il codice e' snake_case minuscolo");
            assert!(!c.contains(' '), "il codice non ha spazi: {c}");
        }
    }

    /// Il codice resta il canale della logica: serializzando l'enum si ottiene
    /// esattamente cio' che il wire portava prima, cosi' i consumatori esistenti
    /// (e i test che confrontano `reason`) non cambiano comportamento.
    #[test]
    fn la_serializzazione_coincide_col_codice_storico() {
        for r in TUTTE {
            let json = serde_json::to_string(r).expect("serializza");
            assert_eq!(json, format!("\"{}\"", r.code()));
        }
    }
}
