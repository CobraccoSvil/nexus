//! `panel_quorum`: PUNTO UNICO (regola L) della logica di QUORUM + VETO di un
//! panel di agenti. Astrae la PRECEDENZA tra gli esiti da un conteggio di voti
//! GENERICO, indipendente dal vocabolario specifico dei verdetti: il panel di
//! review a valle (pass/fail/needs_changes, [`super::adversarial_review`]) e il
//! panel advisory a monte (proceed/block/proceed_with_changes,
//! [`super::advisory_panel`]) mappano i loro enum su questa primitiva e delegano
//! qui la classificazione. Cosi' la regola "soglia -> veto -> condizionale ->
//! approva" e il veto avversario su high-severity vivono in UN solo posto.
//!
//! Funzione PURA (regola L): nessun I/O; la policy del quorum arriva come
//! parametro esplicito (regola G, DB-driven lato chiamante). Replay-stabile.

/// Classe di esito GENERICA di un panel. I moduli specifici la mappano sul loro
/// vocabolario (Approve->Pass/Proceed, Conditional->NeedsChanges/ProceedWithChanges,
/// Veto->Fail/Block, Inconclusive uguale).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelClass {
    /// Tutti i voti validi sono "ok": nessun veto, nessuna condizione.
    Approve,
    /// Nessun veto ma almeno un voto "con riserva" (condizionale).
    Conditional,
    /// Almeno un veto avversario (secondo la policy su high-severity).
    Veto,
    /// Voti validi sotto la soglia `min_valid`: panel non conclusivo (il
    /// chiamante NON deve trattarlo come approvazione — regola M).
    Inconclusive,
}

/// Policy del quorum GENERICA (DB-driven, regola G): caricata dal chiamante e
/// tradotta qui dai moduli specifici (che espongono i propri nomi di campo verso
/// i loro chiamanti, per non rompere quelle API).
#[derive(Debug, Clone, Copy)]
pub struct QuorumPolicy {
    /// Minimo numero di voti VALIDI perche' il panel sia conclusivo. Sotto
    /// soglia -> [`PanelClass::Inconclusive`].
    pub min_valid: usize,
    /// Veto avversario: se `true`, UN solo voto di veto con almeno un elemento
    /// high-severity basta a far vincere il veto (chi trova un difetto grave con
    /// evidenza ha ragione anche in minoranza). Se `false`, qualunque voto di
    /// veto vale come veto ordinario.
    pub veto_on_high_severity: bool,
}

impl Default for QuorumPolicy {
    /// Default sicuro (coincide con i safe-default dei panel concreti): 1 voto
    /// valido basta a essere conclusivi e il veto su high-severity e' ATTIVO.
    fn default() -> Self {
        Self {
            min_valid: 1,
            veto_on_high_severity: true,
        }
    }
}

/// Conteggio GENERICO dei voti validi rilevanti per la classificazione. Il
/// conteggio degli "approve" non entra nella precedenza: e' l'esito di default in
/// assenza di veto/condizionali.
pub struct QuorumTally {
    /// Voti validi totali (usato per la soglia).
    pub valid: usize,
    /// Voti di VETO (fail / block).
    pub veto: usize,
    /// Voti CONDIZIONALI (needs_changes / proceed_with_changes).
    pub conditional: usize,
    /// Almeno un voto di veto porta evidenza high-severity.
    pub any_high_severity_veto: bool,
}

/// Classifica l'esito GENERICO del panel dal conteggio e dalla policy (regola L:
/// unica sede della precedenza; l'ordine dei rami e' significativo — soglia ->
/// veto -> condizionale -> approve).
pub fn classify_panel(t: &QuorumTally, policy: &QuorumPolicy) -> PanelClass {
    if t.valid < policy.min_valid {
        PanelClass::Inconclusive
    } else if t.veto > 0 && (t.any_high_severity_veto || !policy.veto_on_high_severity) {
        PanelClass::Veto
    } else if t.veto > 0 || t.conditional > 0 {
        PanelClass::Conditional
    } else {
        PanelClass::Approve
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tally(valid: usize, veto: usize, conditional: usize, high: bool) -> QuorumTally {
        QuorumTally { valid, veto, conditional, any_high_severity_veto: high }
    }

    #[test]
    fn sotto_soglia_inconclusive() {
        let p = QuorumPolicy { min_valid: 2, veto_on_high_severity: true };
        assert_eq!(classify_panel(&tally(1, 0, 0, false), &p), PanelClass::Inconclusive);
    }

    #[test]
    fn veto_su_high_severity_vince_in_minoranza() {
        let p = QuorumPolicy::default();
        assert_eq!(classify_panel(&tally(3, 1, 0, true), &p), PanelClass::Veto);
    }

    #[test]
    fn veto_senza_high_severity_declassa_a_conditional() {
        let p = QuorumPolicy { min_valid: 1, veto_on_high_severity: true };
        assert_eq!(classify_panel(&tally(1, 1, 0, false), &p), PanelClass::Conditional);
    }

    #[test]
    fn veto_conta_sempre_se_policy_non_richiede_gravita() {
        let p = QuorumPolicy { min_valid: 1, veto_on_high_severity: false };
        assert_eq!(classify_panel(&tally(1, 1, 0, false), &p), PanelClass::Veto);
    }

    #[test]
    fn solo_condizionali_conditional() {
        let p = QuorumPolicy::default();
        assert_eq!(classify_panel(&tally(2, 0, 1, false), &p), PanelClass::Conditional);
    }

    #[test]
    fn tutti_ok_approve() {
        let p = QuorumPolicy::default();
        assert_eq!(classify_panel(&tally(2, 0, 0, false), &p), PanelClass::Approve);
    }
}
