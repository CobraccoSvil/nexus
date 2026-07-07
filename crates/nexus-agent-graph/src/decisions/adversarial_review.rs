//! `adversarial_review`: PUNTO UNICO (regola L) della COMPOSIZIONE dei verdetti
//! di un PANEL di revisori (Fase C ultracode). Prende gli esiti STRUTTURATI di N
//! sub-run di review (il blocco `structured_verdict` prodotto in Fase A, col
//! campo `review` di Fase B) e ne deriva un verdetto di panel DETERMINISTICO.
//!
//! Regola M: legge SOLO segnali strutturati (`success`, `review.verdict`,
//! `review.findings[].severity`), MAI la prosa di `summary`. Regola G: la policy
//! del quorum arriva come parametro esplicito (il chiamante la carica dal DB),
//! niente soglia hardcoded qui. Funzione PURA: nessun I/O, testabile e
//! replay-stabile.
//!
//! Un "voto" e' VALIDO solo se il sub-run revisore e' arrivato a un esito
//! (`success == true`) E ha dichiarato un `review` col tool `review_verdict`: un
//! revisore andato in timeout/errore NON vota (astensione), non conta come pass.

use serde_json::Value;

/// Verdetto canonico dichiarato da UN revisore via `review_verdict` (Fase B).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewVerdict {
    Pass,
    Fail,
    NeedsChanges,
}

impl ReviewVerdict {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "pass" => Some(Self::Pass),
            "fail" => Some(Self::Fail),
            "needs_changes" => Some(Self::NeedsChanges),
            _ => None,
        }
    }
}

/// Verdetto AGGREGATO del panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelVerdict {
    /// Tutti i voti validi sono `pass`.
    Pass,
    /// Almeno un veto avversario (fail, o high-severity secondo policy).
    Fail,
    /// Nessun fail ma almeno un `needs_changes`.
    NeedsChanges,
    /// Voti validi sotto la soglia `min_valid_verdicts`: panel non conclusivo
    /// (il coordinatore NON deve trattarlo come pass — regola M).
    Inconclusive,
}

impl PanelVerdict {
    /// Etichetta canonica per il JSON del tool_result (stesso vocabolario del
    /// tool `review_verdict`, piu' `inconclusive`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::NeedsChanges => "needs_changes",
            Self::Inconclusive => "inconclusive",
        }
    }

    /// `true` solo per `Pass`: il lavoro e' approvato dal panel. Ogni altro esito
    /// (incluso `Inconclusive`) NON e' un'approvazione.
    pub fn is_approved(self) -> bool {
        matches!(self, Self::Pass)
    }
}

/// Policy del quorum (DB-driven, regola G): caricata dal chiamante e passata qui.
#[derive(Debug, Clone, Copy)]
pub struct QuorumPolicy {
    /// Minimo numero di voti VALIDI (revisore completato + `review` presente)
    /// perche' il panel sia conclusivo. Sotto soglia -> `Inconclusive`.
    pub min_valid_verdicts: usize,
    /// Veto avversario: se `true`, UN solo `fail` con almeno un finding di
    /// severity `alta` basta a far fallire il panel (un revisore che trova un
    /// difetto grave con evidenza ha ragione anche in minoranza). Se `false`,
    /// un `fail` conta come voto ordinario e serve comunque la presenza di
    /// almeno un fail per il verdetto Fail.
    pub fail_on_high_severity: bool,
}

impl Default for QuorumPolicy {
    /// Default sicuro: 1 voto valido basta a essere conclusivi, e il veto
    /// avversario su high-severity e' ATTIVO (un panel di review deve poter
    /// bocciare su un solo difetto grave con evidenza).
    fn default() -> Self {
        Self {
            min_valid_verdicts: 1,
            fail_on_high_severity: true,
        }
    }
}

/// Esito composto del panel (segnali strutturati per il coordinatore).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelOutcome {
    pub verdict: PanelVerdict,
    /// Voti validi / voti totali di review trovati.
    pub valid: usize,
    pub total_reviews: usize,
    pub pass: usize,
    pub fail: usize,
    pub needs_changes: usize,
    /// I revisori validi NON concordano (piu' di un verdetto distinto).
    pub dissent: bool,
    /// Findings aggregati di TUTTI i voti validi (uniti, non deduplicati: la
    /// provenienza per-file resta nell'oggetto finding).
    pub findings: Vec<Value>,
}

impl PanelOutcome {
    /// Serializza in `Value` per il campo `panel_verdict` del tool_result
    /// (regola M: il coordinatore legge questi campi, non la prosa).
    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "verdict": self.verdict.as_str(),
            "approved": self.verdict.is_approved(),
            "valid": self.valid,
            "total_reviews": self.total_reviews,
            "tally": { "pass": self.pass, "fail": self.fail, "needs_changes": self.needs_changes },
            "dissent": self.dissent,
            "findings": self.findings,
        })
    }
}

/// Un voto valido estratto da un outcome (regola M).
struct Vote {
    verdict: ReviewVerdict,
    has_high_severity: bool,
    findings: Vec<Value>,
}

/// Conteggio dei voti validi del panel rilevanti per la CLASSIFICAZIONE (il
/// conteggio `pass` non entra nella precedenza: pass e' l'esito di default in
/// assenza di fail/needs_changes).
struct Tally {
    valid: usize,
    fail: usize,
    needs_changes: usize,
    any_high_severity_fail: bool,
}

/// Classifica il verdetto di panel dal conteggio dei voti e dalla policy
/// (regola L: unica sede della precedenza tra esiti; l'ordine dei rami e'
/// significativo — soglia -> veto fail -> needs_changes -> pass).
fn classify_panel_verdict(t: &Tally, policy: &QuorumPolicy) -> PanelVerdict {
    if t.valid < policy.min_valid_verdicts {
        PanelVerdict::Inconclusive
    } else if t.fail > 0 && (t.any_high_severity_fail || !policy.fail_on_high_severity) {
        // Veto avversario: con fail_on_high_severity, un fail con evidenza grave
        // basta; senza, qualunque fail vale come voto negativo.
        PanelVerdict::Fail
    } else if t.fail > 0 || t.needs_changes > 0 {
        // fail senza high-severity (policy che richiede gravita') o needs_changes:
        // difetto reale ma non bloccante di per se'.
        PanelVerdict::NeedsChanges
    } else {
        PanelVerdict::Pass
    }
}

/// Estrae il voto di review da UN blocco esito strutturato (`outcome`). `None`
/// se non e' un voto valido: il revisore non e' arrivato a esito
/// (`success != true`) oppure non ha dichiarato un `review` col verdetto (regola
/// M: un'astensione non e' un pass).
fn extract_vote(outcome: &Value) -> Option<Vote> {
    if outcome.get("success").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let review = outcome.get("review")?;
    let verdict = ReviewVerdict::parse(review.get("verdict").and_then(Value::as_str)?)?;
    let findings: Vec<Value> = review
        .get("findings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let has_high_severity = findings.iter().any(|f| {
        f.get("severity").and_then(Value::as_str) == Some("alta")
    });
    Some(Vote {
        verdict,
        has_high_severity,
        findings,
    })
}

/// Compone il verdetto del panel dagli esiti strutturati dei sub-run di review.
/// `outcomes` e' la lista dei blocchi `outcome` (structured_verdict) dei sub-run;
/// gli elementi senza `review` valido sono ignorati (non sono revisori).
///
/// `None` se NESSUN outcome porta un `review` (il batch non e' un panel di
/// review): il coordinatore non aggiunge alcun `panel_verdict`. `Some(_)` con
/// `Inconclusive` se ci sono review ma i voti validi sono sotto la soglia.
///
/// Determinismo: l'ordine dei findings segue l'ordine degli `outcomes` (stabile).
pub fn compose_panel_verdict(outcomes: &[Value], policy: &QuorumPolicy) -> Option<PanelOutcome> {
    // Un batch e' un "panel di review" se almeno un outcome ha il campo review
    // (anche solo dichiarato): distingue un panel da un batch di soli worker.
    let total_reviews = outcomes
        .iter()
        .filter(|o| o.get("review").map(|r| !r.is_null()).unwrap_or(false))
        .count();
    if total_reviews == 0 {
        return None;
    }

    let votes: Vec<Vote> = outcomes.iter().filter_map(extract_vote).collect();
    let mut pass = 0;
    let mut fail = 0;
    let mut needs_changes = 0;
    let mut any_high_severity_fail = false;
    let mut findings: Vec<Value> = Vec::new();
    for v in &votes {
        match v.verdict {
            ReviewVerdict::Pass => pass += 1,
            ReviewVerdict::Fail => {
                fail += 1;
                if v.has_high_severity {
                    any_high_severity_fail = true;
                }
            }
            ReviewVerdict::NeedsChanges => needs_changes += 1,
        }
        findings.extend(v.findings.iter().cloned());
    }
    let valid = votes.len();
    let distinct = usize::from(pass > 0) + usize::from(fail > 0) + usize::from(needs_changes > 0);
    let verdict = classify_panel_verdict(
        &Tally { valid, fail, needs_changes, any_high_severity_fail },
        policy,
    );

    Some(PanelOutcome {
        verdict,
        valid,
        total_reviews,
        pass,
        fail,
        needs_changes,
        dissent: distinct > 1,
        findings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Blocco outcome di un revisore che ha VOTATO (success + review).
    fn reviewer(verdict: &str, findings: Value) -> Value {
        json!({
            "verdict": "completed",
            "success": true,
            "review": { "verdict": verdict, "summary": "x", "findings": findings },
        })
    }

    #[test]
    fn nessun_review_nel_batch_ritorna_none() {
        // Batch di soli worker (outcome senza review): non e' un panel.
        let worker = json!({"verdict": "completed", "success": true, "review": Value::Null});
        assert!(compose_panel_verdict(&[worker], &QuorumPolicy::default()).is_none());
    }

    #[test]
    fn tutti_pass_e_approved() {
        let out = compose_panel_verdict(
            &[reviewer("pass", json!([])), reviewer("pass", json!([]))],
            &QuorumPolicy::default(),
        )
        .unwrap();
        assert_eq!(out.verdict, PanelVerdict::Pass);
        assert!(out.verdict.is_approved());
        assert_eq!((out.pass, out.fail, out.needs_changes), (2, 0, 0));
        assert!(!out.dissent);
    }

    #[test]
    fn veto_avversario_su_high_severity() {
        // Un solo fail con finding alta -> panel Fail anche in minoranza.
        let high = json!([{"file": "a.rs", "severity": "alta", "description": "bug"}]);
        let out = compose_panel_verdict(
            &[reviewer("pass", json!([])), reviewer("fail", high)],
            &QuorumPolicy::default(),
        )
        .unwrap();
        assert_eq!(out.verdict, PanelVerdict::Fail);
        assert!(!out.verdict.is_approved());
        assert!(out.dissent, "pass + fail = dissenso");
        assert_eq!(out.findings.len(), 1);
    }

    #[test]
    fn fail_senza_high_severity_declassa_a_needs_changes() {
        // Con fail_on_high_severity, un fail con solo finding media -> needs_changes.
        let media = json!([{"file": "a.rs", "severity": "media", "description": "nit"}]);
        let out = compose_panel_verdict(
            &[reviewer("fail", media)],
            &QuorumPolicy { min_valid_verdicts: 1, fail_on_high_severity: true },
        )
        .unwrap();
        assert_eq!(out.verdict, PanelVerdict::NeedsChanges);
    }

    #[test]
    fn fail_conta_sempre_se_policy_non_richiede_gravita() {
        let media = json!([{"file": "a.rs", "severity": "media", "description": "nit"}]);
        let out = compose_panel_verdict(
            &[reviewer("fail", media)],
            &QuorumPolicy { min_valid_verdicts: 1, fail_on_high_severity: false },
        )
        .unwrap();
        assert_eq!(out.verdict, PanelVerdict::Fail);
    }

    #[test]
    fn needs_changes_senza_fail() {
        let out = compose_panel_verdict(
            &[reviewer("pass", json!([])), reviewer("needs_changes", json!([]))],
            &QuorumPolicy::default(),
        )
        .unwrap();
        assert_eq!(out.verdict, PanelVerdict::NeedsChanges);
        assert!(out.dissent);
    }

    #[test]
    fn astensione_non_conta_come_voto() {
        // Un revisore in timeout (success=false, review assente) NON vota: sotto
        // min_valid_verdicts=2 il panel e' inconclusive nonostante un pass valido.
        let abstain = json!({"verdict": "timed_out", "success": false, "review": Value::Null});
        let out = compose_panel_verdict(
            &[reviewer("pass", json!([])), abstain],
            &QuorumPolicy { min_valid_verdicts: 2, fail_on_high_severity: true },
        )
        .unwrap();
        assert_eq!(out.verdict, PanelVerdict::Inconclusive);
        assert!(!out.verdict.is_approved());
        assert_eq!(out.valid, 1);
        assert_eq!(out.total_reviews, 1, "l'astensione senza review non conta come review");
    }

    #[test]
    fn deterministico_ordine_findings_stabile() {
        let f1 = json!([{"file": "a.rs", "severity": "alta", "description": "1"}]);
        let f2 = json!([{"file": "b.rs", "severity": "media", "description": "2"}]);
        let out = compose_panel_verdict(
            &[reviewer("fail", f1), reviewer("needs_changes", f2)],
            &QuorumPolicy::default(),
        )
        .unwrap();
        // Ordine findings = ordine outcomes (stabile).
        assert_eq!(out.findings[0]["file"], json!("a.rs"));
        assert_eq!(out.findings[1]["file"], json!("b.rs"));
    }
}
