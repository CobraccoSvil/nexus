//! `advisory_panel`: PUNTO UNICO (regola L) della COMPOSIZIONE dei pareri di un
//! panel di figure di ANALISI a MONTE (il "consiglio" prima dell'esecuzione).
//! Gemello di [`super::adversarial_review`] (che aggrega i verdetti di review a
//! VALLE): entrambi delegano la precedenza/veto al punto unico generico
//! [`super::panel_quorum`], su vocabolari di verdetto diversi.
//!
//! Prende gli esiti STRUTTURATI dei sub-run delle figure (il blocco
//! `structured_verdict`, col campo `advisory` prodotto dal tool `advisory_verdict`)
//! e ne deriva una SINTESI deterministica: requisiti/vincoli uniti, rischi
//! ordinati per severity, raccomandazioni unite, e un verdetto di panel
//! (Proceed / ProceedWithChanges / Block / Inconclusive) con veto avversario.
//!
//! Regola M: legge SOLO segnali strutturati (`success`, `advisory.verdict`,
//! `advisory.risks[].severity`), MAI la prosa di `summary`. Regola G: la policy
//! del quorum arriva come parametro (il chiamante la carica dal DB). Funzione
//! PURA: nessun I/O, replay-stabile.
//!
//! Un "voto" e' VALIDO solo se il sub-run della figura e' arrivato a esito
//! (`success == true`) E ha dichiarato un `advisory` col verdetto: una figura
//! andata in timeout/errore NON vota (astensione), non conta come proceed.

use serde_json::Value;

use super::panel_quorum::{classify_panel, PanelClass, QuorumPolicy, QuorumTally};

/// Verdetto canonico dichiarato da UNA figura via `advisory_verdict`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvisoryVerdict {
    /// Si puo' procedere senza vincoli aggiuntivi da questa figura.
    Proceed,
    /// Si puo' procedere ma rispettando i requisiti/le correzioni indicate.
    ProceedWithChanges,
    /// Veto: la figura ritiene la richiesta non eseguibile cosi' com'e'.
    Block,
}

impl AdvisoryVerdict {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "proceed" => Some(Self::Proceed),
            "proceed_with_changes" => Some(Self::ProceedWithChanges),
            "block" => Some(Self::Block),
            _ => None,
        }
    }
}

/// Verdetto AGGREGATO del panel advisory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvisoryPanelVerdict {
    /// Tutti i voti validi sono `proceed`.
    Proceed,
    /// Nessun veto ma almeno un `proceed_with_changes` (o un `block` senza
    /// evidenza high-severity secondo policy).
    ProceedWithChanges,
    /// Almeno un veto avversario (`block` con evidenza high-severity, o qualunque
    /// `block` se la policy non richiede la gravita').
    Block,
    /// Voti validi sotto la soglia `min_valid_advisories`: panel non conclusivo
    /// (il coordinatore NON deve trattarlo come via libera — regola M).
    Inconclusive,
}

impl AdvisoryPanelVerdict {
    /// Etichetta canonica per il JSON del tool_result (stesso vocabolario del
    /// tool `advisory_verdict`, piu' `inconclusive`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proceed => "proceed",
            Self::ProceedWithChanges => "proceed_with_changes",
            Self::Block => "block",
            Self::Inconclusive => "inconclusive",
        }
    }

    /// `true` solo per `Proceed`: via libera piena. Ogni altro esito richiede al
    /// coordinatore di incorporare vincoli (ProceedWithChanges), fermarsi (Block)
    /// o non trattare il panel come approvazione (Inconclusive).
    pub fn is_clear(self) -> bool {
        matches!(self, Self::Proceed)
    }

    /// `true` se il panel VETA l'esecuzione cosi' com'e' (Block): il coordinatore
    /// deve incorporare i requisiti bloccanti nel piano o fermarsi.
    pub fn is_veto(self) -> bool {
        matches!(self, Self::Block)
    }
}

/// Policy del quorum advisory (DB-driven, regola G): caricata dal chiamante. Nomi
/// di campo specifici del panel advisory; tradotti in
/// [`super::panel_quorum::QuorumPolicy`] al momento della classificazione.
#[derive(Debug, Clone, Copy)]
pub struct AdvisoryPolicy {
    /// Minimo numero di pareri VALIDI perche' il panel sia conclusivo. Sotto
    /// soglia -> `Inconclusive`.
    pub min_valid_advisories: usize,
    /// Veto avversario: se `true`, UN solo `block` con almeno un rischio di
    /// severity `alta` basta a far vincere il veto (una figura che trova un
    /// rischio grave con evidenza ha ragione anche in minoranza). Se `false`,
    /// qualunque `block` conta come veto ordinario.
    pub block_on_high_severity: bool,
}

impl Default for AdvisoryPolicy {
    /// Default sicuro: 1 parere valido basta a essere conclusivi e il veto su
    /// high-severity e' ATTIVO (il consiglio deve poter bloccare su un solo
    /// rischio grave con evidenza).
    fn default() -> Self {
        Self {
            min_valid_advisories: 1,
            block_on_high_severity: true,
        }
    }
}

/// Sintesi composta del panel advisory (segnali strutturati per il coordinatore).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisorySynthesis {
    pub verdict: AdvisoryPanelVerdict,
    /// Pareri validi / pareri advisory totali trovati.
    pub valid: usize,
    pub total_advisories: usize,
    pub proceed: usize,
    pub proceed_with_changes: usize,
    pub block: usize,
    /// Le figure valide NON concordano (piu' di un verdetto distinto).
    pub dissent: bool,
    /// Requisiti/vincoli uniti da tutti i pareri validi, deduplicati mantenendo
    /// l'ordine di prima apparizione (input per il piano dell'esecuzione).
    pub requirements: Vec<String>,
    /// Rischi aggregati da tutti i pareri validi, ordinati per severity
    /// (alta -> media -> bassa) con ordine di apparizione stabile a parita'.
    pub risks: Vec<Value>,
    /// Raccomandazioni unite, deduplicate mantenendo l'ordine.
    pub recommendations: Vec<String>,
}

impl AdvisorySynthesis {
    /// Serializza in `Value` per il campo `advisory_synthesis` del tool_result
    /// (regola M: il coordinatore legge questi campi, non la prosa).
    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "verdict": self.verdict.as_str(),
            "clear": self.verdict.is_clear(),
            "veto": self.verdict.is_veto(),
            "valid": self.valid,
            "total_advisories": self.total_advisories,
            "tally": {
                "proceed": self.proceed,
                "proceed_with_changes": self.proceed_with_changes,
                "block": self.block,
            },
            "dissent": self.dissent,
            "requirements": self.requirements,
            "risks": self.risks,
            "recommendations": self.recommendations,
        })
    }
}

/// Un voto valido estratto da un outcome (regola M).
struct Advice {
    verdict: AdvisoryVerdict,
    has_high_severity: bool,
    requirements: Vec<String>,
    risks: Vec<Value>,
    recommendations: Vec<String>,
}

/// Rank di severity per l'ordinamento dei rischi (piu' basso = piu' grave, cosi'
/// il sort ascendente porta le `alta` in cima). Severity ignota -> in fondo.
fn severity_rank(v: &Value) -> u8 {
    match v.get("severity").and_then(Value::as_str) {
        Some("alta") => 0,
        Some("media") => 1,
        Some("bassa") => 2,
        _ => 3,
    }
}

/// Estrae la lista di stringhe non vuote da un campo array (`requirements`,
/// `recommendations`), con trim; ignora elementi non-stringa o vuoti.
fn string_list(advisory: &Value, key: &str) -> Vec<String> {
    advisory
        .get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Estrae il parere da UN blocco esito strutturato (`outcome`). `None` se non e'
/// un voto valido: la figura non e' arrivata a esito (`success != true`) oppure
/// non ha dichiarato un `advisory` col verdetto (regola M: un'astensione non e'
/// un proceed).
fn extract_advice(outcome: &Value) -> Option<Advice> {
    if outcome.get("success").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let advisory = outcome.get("advisory")?;
    let verdict = AdvisoryVerdict::parse(advisory.get("verdict").and_then(Value::as_str)?)?;
    let risks: Vec<Value> = advisory
        .get("risks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let has_high_severity = risks
        .iter()
        .any(|r| r.get("severity").and_then(Value::as_str) == Some("alta"));
    Some(Advice {
        verdict,
        has_high_severity,
        requirements: string_list(advisory, "requirements"),
        risks,
        recommendations: string_list(advisory, "recommendations"),
    })
}

/// Aggiunge le stringhe nuove (non gia' presenti) a `acc`, preservando l'ordine
/// di prima apparizione (dedup stabile, regola L: unica sede della dedup stringhe
/// del panel).
fn extend_dedup(acc: &mut Vec<String>, items: Vec<String>) {
    for it in items {
        if !acc.iter().any(|e| e == &it) {
            acc.push(it);
        }
    }
}

/// Compone la sintesi del panel advisory dagli esiti strutturati dei sub-run
/// delle figure. `outcomes` e' la lista dei blocchi `outcome` (structured_verdict);
/// gli elementi senza `advisory` valido sono ignorati (non sono figure del panel).
///
/// `None` se NESSUN outcome porta un `advisory` (il batch non e' un panel di
/// analisi): il coordinatore non aggiunge alcuna sintesi. `Some(_)` con
/// `Inconclusive` se ci sono pareri ma i voti validi sono sotto la soglia.
///
/// Determinismo: requisiti e raccomandazioni seguono l'ordine degli `outcomes`
/// (dedup stabile); i rischi sono ordinati per severity con sort STABILE, quindi
/// a parita' di severity restano nell'ordine degli `outcomes`.
pub fn compose_advisory_synthesis(
    outcomes: &[Value],
    policy: &AdvisoryPolicy,
) -> Option<AdvisorySynthesis> {
    // Un batch e' un "panel advisory" se almeno un outcome ha il campo advisory
    // (anche solo dichiarato): distingue un panel di analisi da un batch di worker.
    let total_advisories = outcomes
        .iter()
        .filter(|o| o.get("advisory").map(|a| !a.is_null()).unwrap_or(false))
        .count();
    if total_advisories == 0 {
        return None;
    }

    let advices: Vec<Advice> = outcomes.iter().filter_map(extract_advice).collect();
    let mut proceed = 0;
    let mut proceed_with_changes = 0;
    let mut block = 0;
    let mut any_high_severity_block = false;
    let mut requirements: Vec<String> = Vec::new();
    let mut recommendations: Vec<String> = Vec::new();
    let mut risks: Vec<Value> = Vec::new();
    for a in &advices {
        match a.verdict {
            AdvisoryVerdict::Proceed => proceed += 1,
            AdvisoryVerdict::ProceedWithChanges => proceed_with_changes += 1,
            AdvisoryVerdict::Block => {
                block += 1;
                if a.has_high_severity {
                    any_high_severity_block = true;
                }
            }
        }
        extend_dedup(&mut requirements, a.requirements.clone());
        extend_dedup(&mut recommendations, a.recommendations.clone());
        risks.extend(a.risks.iter().cloned());
    }
    // Ordine per severity, STABILE (a parita' resta l'ordine di apparizione).
    risks.sort_by_key(severity_rank);

    let valid = advices.len();
    let distinct =
        usize::from(proceed > 0) + usize::from(proceed_with_changes > 0) + usize::from(block > 0);

    let class = classify_panel(
        &QuorumTally {
            valid,
            veto: block,
            conditional: proceed_with_changes,
            any_high_severity_veto: any_high_severity_block,
        },
        &QuorumPolicy {
            min_valid: policy.min_valid_advisories,
            veto_on_high_severity: policy.block_on_high_severity,
        },
    );
    let verdict = match class {
        PanelClass::Approve => AdvisoryPanelVerdict::Proceed,
        PanelClass::Conditional => AdvisoryPanelVerdict::ProceedWithChanges,
        PanelClass::Veto => AdvisoryPanelVerdict::Block,
        PanelClass::Inconclusive => AdvisoryPanelVerdict::Inconclusive,
    };

    Some(AdvisorySynthesis {
        verdict,
        valid,
        total_advisories,
        proceed,
        proceed_with_changes,
        block,
        dissent: distinct > 1,
        requirements,
        risks,
        recommendations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Blocco outcome di una figura che ha VOTATO (success + advisory).
    fn figure(verdict: &str, risks: Value, requirements: Value) -> Value {
        json!({
            "verdict": "completed",
            "success": true,
            "advisory": {
                "verdict": verdict,
                "summary": "x",
                "requirements": requirements,
                "risks": risks,
                "recommendations": [],
                "blocking": verdict == "block",
            },
        })
    }

    #[test]
    fn nessun_advisory_nel_batch_ritorna_none() {
        // Batch di soli worker (outcome senza advisory): non e' un panel.
        let worker = json!({"verdict": "completed", "success": true, "advisory": Value::Null});
        assert!(compose_advisory_synthesis(&[worker], &AdvisoryPolicy::default()).is_none());
    }

    #[test]
    fn tutti_proceed_e_clear() {
        let out = compose_advisory_synthesis(
            &[
                figure("proceed", json!([]), json!([])),
                figure("proceed", json!([]), json!([])),
            ],
            &AdvisoryPolicy::default(),
        )
        .unwrap();
        assert_eq!(out.verdict, AdvisoryPanelVerdict::Proceed);
        assert!(out.verdict.is_clear());
        assert_eq!(
            (out.proceed, out.proceed_with_changes, out.block),
            (2, 0, 0)
        );
        assert!(!out.dissent);
    }

    #[test]
    fn veto_avversario_su_high_severity() {
        // Un solo block con rischio alta -> panel Block anche in minoranza.
        let high = json!([{"severity": "alta", "description": "falla"}]);
        let out = compose_advisory_synthesis(
            &[
                figure("proceed", json!([]), json!([])),
                figure("block", high, json!(["usa PKCE"])),
            ],
            &AdvisoryPolicy::default(),
        )
        .unwrap();
        assert_eq!(out.verdict, AdvisoryPanelVerdict::Block);
        assert!(out.verdict.is_veto());
        assert!(out.dissent, "proceed + block = dissenso");
        assert_eq!(out.risks.len(), 1);
        assert_eq!(out.requirements, vec!["usa PKCE".to_string()]);
    }

    #[test]
    fn block_senza_high_severity_declassa_a_proceed_with_changes() {
        let media = json!([{"severity": "media", "description": "nit"}]);
        let out = compose_advisory_synthesis(
            &[figure("block", media, json!([]))],
            &AdvisoryPolicy {
                min_valid_advisories: 1,
                block_on_high_severity: true,
            },
        )
        .unwrap();
        assert_eq!(out.verdict, AdvisoryPanelVerdict::ProceedWithChanges);
    }

    #[test]
    fn block_conta_sempre_se_policy_non_richiede_gravita() {
        let media = json!([{"severity": "media", "description": "nit"}]);
        let out = compose_advisory_synthesis(
            &[figure("block", media, json!([]))],
            &AdvisoryPolicy {
                min_valid_advisories: 1,
                block_on_high_severity: false,
            },
        )
        .unwrap();
        assert_eq!(out.verdict, AdvisoryPanelVerdict::Block);
    }

    #[test]
    fn proceed_with_changes_senza_block() {
        let out = compose_advisory_synthesis(
            &[
                figure("proceed", json!([]), json!([])),
                figure("proceed_with_changes", json!([]), json!(["valida input"])),
            ],
            &AdvisoryPolicy::default(),
        )
        .unwrap();
        assert_eq!(out.verdict, AdvisoryPanelVerdict::ProceedWithChanges);
        assert!(out.dissent);
        assert_eq!(out.requirements, vec!["valida input".to_string()]);
    }

    #[test]
    fn astensione_non_conta_come_voto() {
        // Una figura in timeout (success=false, advisory assente) NON vota: sotto
        // min_valid=2 il panel e' inconclusive nonostante un proceed valido.
        let abstain = json!({"verdict": "timed_out", "success": false, "advisory": Value::Null});
        let out = compose_advisory_synthesis(
            &[figure("proceed", json!([]), json!([])), abstain],
            &AdvisoryPolicy {
                min_valid_advisories: 2,
                block_on_high_severity: true,
            },
        )
        .unwrap();
        assert_eq!(out.verdict, AdvisoryPanelVerdict::Inconclusive);
        assert!(!out.verdict.is_clear());
        assert_eq!(out.valid, 1);
        assert_eq!(
            out.total_advisories, 1,
            "l'astensione senza advisory non conta"
        );
    }

    #[test]
    fn requisiti_deduplicati_ordine_stabile() {
        let out = compose_advisory_synthesis(
            &[
                figure("proceed_with_changes", json!([]), json!(["A", "B"])),
                figure("proceed_with_changes", json!([]), json!(["B", "C"])),
            ],
            &AdvisoryPolicy::default(),
        )
        .unwrap();
        // B compare una sola volta, ordine di prima apparizione.
        assert_eq!(
            out.requirements,
            vec!["A".to_string(), "B".to_string(), "C".to_string()]
        );
    }

    #[test]
    fn rischi_ordinati_per_severity() {
        let r1 = json!([{"severity": "bassa", "description": "1"}]);
        let r2 = json!([{"severity": "alta", "description": "2"}]);
        let r3 = json!([{"severity": "media", "description": "3"}]);
        let out = compose_advisory_synthesis(
            &[
                figure("proceed_with_changes", r1, json!([])),
                figure("block", r2, json!([])),
                figure("proceed_with_changes", r3, json!([])),
            ],
            &AdvisoryPolicy::default(),
        )
        .unwrap();
        // alta -> media -> bassa.
        assert_eq!(out.risks[0]["severity"], json!("alta"));
        assert_eq!(out.risks[1]["severity"], json!("media"));
        assert_eq!(out.risks[2]["severity"], json!("bassa"));
    }
}
