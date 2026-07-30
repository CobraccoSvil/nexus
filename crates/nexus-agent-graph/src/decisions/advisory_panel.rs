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

use super::panel_quorum::{classify_panel, required_valid, PanelClass, QuorumPolicy, QuorumTally};
use super::requirement_conformance::Direction;
use super::severity::rank as severity_rank;

/// Roster del panel advisory: il DENOMINATORE del quorum e' un input esplicito
/// del chiamante, mai dedotto dalla presenza dei voti (regola M: una figura in
/// timeout e' un'ASSENZA che pesa, non una riga che sparisce dal conteggio).
/// Incidente reale: 4 figure su 5 in timeout/errore -> `total_advisories = 1`
/// -> quorum "raggiunto" con 1 voto -> il consiglio dichiarava `proceed` come
/// consenso. Enum (non `Option<usize>`) cosi' ogni call site DICHIARA la
/// semantica del proprio denominatore e un caso nuovo non compila da solo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvisoryRoster {
    /// Il chiamante CONOSCE quante figure ha convocato al voto (consiglio a
    /// monte, panel multi-provider): il quorum e' relativo alle convocate.
    Convened(usize),
    /// Batch generico (fan-in `dispatch_subagents`): le figure advisory si
    /// riconoscono solo dal voto dichiarato, il roster non e' noto. Denominatore
    /// = advisory presenti; vale la sola soglia assoluta (semantica storica).
    SelfDeclared,
}

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
    /// Minimo numero ASSOLUTO di pareri VALIDI perche' il panel sia conclusivo
    /// (pavimento). Sotto la soglia effettiva -> `Inconclusive`.
    pub min_valid_advisories: usize,
    /// Percentuale (0-100) delle figure CONVOCATE che deve aver deliberato
    /// perche' il panel sia conclusivo, quando il roster e' noto
    /// ([`AdvisoryRoster::Convened`]). La soglia effettiva e'
    /// `max(min_valid_advisories, ceil(convocate * quorum_pct / 100))`.
    pub quorum_pct: u8,
    /// Veto avversario: se `true`, UN solo `block` con almeno un rischio di
    /// severity `alta` basta a far vincere il veto (una figura che trova un
    /// rischio grave con evidenza ha ragione anche in minoranza). Se `false`,
    /// qualunque `block` conta come veto ordinario.
    pub block_on_high_severity: bool,
}

impl Default for AdvisoryPolicy {
    /// Default sicuro: 1 parere valido come pavimento assoluto, quorum al 50%
    /// delle convocate quando il roster e' noto, veto su high-severity ATTIVO
    /// (il consiglio deve poter bloccare su un solo rischio grave con evidenza).
    /// Coincide coi safe-default delle chiavi `orchestrator.council_advisory_*`
    /// (mig 0548 + 0589): il DB resta la fonte di verita' (regola G).
    fn default() -> Self {
        Self {
            min_valid_advisories: 1,
            quorum_pct: 50,
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
    /// Figure CONVOCATE al voto (denominatore del quorum). Per
    /// [`AdvisoryRoster::SelfDeclared`] coincide con `total_advisories`.
    pub convened: usize,
    /// Soglia EFFETTIVA di pareri validi usata per il quorum: sotto questa il
    /// verdetto e' `Inconclusive`. Il lettore puo' dichiarare "X su N (quorum
    /// Y)" senza ricalcolare nulla.
    pub required_valid: usize,
    pub proceed: usize,
    pub proceed_with_changes: usize,
    pub block: usize,
    /// Le figure valide NON concordano (piu' di un verdetto distinto).
    pub dissent: bool,
    /// Requisiti/vincoli uniti da tutti i pareri validi, deduplicati mantenendo
    /// l'ordine di prima apparizione (input per il piano dell'esecuzione), con
    /// la direzione dichiarata da ciascuna figura quando presente.
    pub requirements: Vec<Requirement>,
    /// Rischi aggregati da tutti i pareri validi, ordinati per severity
    /// (alta -> media -> bassa) con ordine di apparizione stabile a parita'.
    pub risks: Vec<Value>,
    /// Raccomandazioni unite, deduplicate mantenendo l'ordine.
    pub recommendations: Vec<String>,
    /// Decisione architetturale CONTESA, se una figura ne ha dichiarata una:
    /// `{topic, options[]}`. Vince la PRIMA dichiarazione valida nell'ordine
    /// degli `outcomes` (deterministico e replay-stabile; le successive sono
    /// ignorate — un dibattito ha UN oggetto, non uno per figura). E' il segnale
    /// strutturato che innesca le tesi contrapposte (regola M: mai dedotto dalla
    /// prosa dei pareri).
    pub contested_decision: Option<Value>,
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
            "convened": self.convened,
            "required_valid": self.required_valid,
            "tally": {
                "proceed": self.proceed,
                "proceed_with_changes": self.proceed_with_changes,
                "block": self.block,
            },
            "dissent": self.dissent,
            "requirements": self.requirements.iter().map(Requirement::to_value).collect::<Vec<_>>(),
            "risks": self.risks,
            "recommendations": self.recommendations,
            "contested_decision": self.contested_decision,
        })
    }
}

/// Un voto valido estratto da un outcome (regola M).
struct Advice {
    verdict: AdvisoryVerdict,
    has_high_severity: bool,
    requirements: Vec<Requirement>,
    risks: Vec<Value>,
    recommendations: Vec<String>,
    /// Decisione architetturale CONTESA dichiarata da questa figura (innesca il
    /// dibattito a tesi contrapposte). Gia' normalizzata alla frontiera.
    contested_decision: Option<Value>,
}

/// Estrae la lista di stringhe non vuote da un campo array (`recommendations`),
/// con trim; ignora elementi non-stringa o vuoti.
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

/// Un requisito con la direzione dichiarata dalla figura ALLA FONTE, quando
/// l'ha dichiarata (regola M): mai piu' indovinata da un parser testuale a
/// valle. Bug reale chiuso da questo campo (30/07/2026): "Sostituire
/// `port: 33649`" contiene solo verbi di presenza per l'euristica di
/// [`super::requirement_conformance`], che lo leggerebbe come "deve
/// presenziare" invertendo il verdetto su un requisito di rimozione.
///
/// `direction: None` quando l'elemento arriva nel formato storico (stringa
/// nuda) o la figura non ha valorizzato il campo: il consumatore degrada
/// onestamente all'euristica sui verbi, comportamento invariato.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub text: String,
    pub direction: Option<Direction>,
}

impl Requirement {
    fn to_value(&self) -> Value {
        let mut o = serde_json::Map::new();
        o.insert("text".to_string(), Value::String(self.text.clone()));
        if let Some(d) = self.direction {
            o.insert("direction".to_string(), Value::String(d.as_str().to_string()));
        }
        Value::Object(o)
    }
}

impl From<&str> for Requirement {
    /// Costruisce un requisito SENZA direzione dichiarata: comodo per i
    /// chiamanti (e i test) che esercitano solo l'euristica sui verbi in
    /// [`super::requirement_conformance`].
    fn from(text: &str) -> Self {
        Self {
            text: text.to_string(),
            direction: None,
        }
    }
}

impl From<String> for Requirement {
    fn from(text: String) -> Self {
        Self {
            text,
            direction: None,
        }
    }
}

/// Estrae i requisiti dal campo `key` (`requirements` per `advisory_verdict`,
/// [`super::requirement_conformance::CAMPO_REQUISITI`] per la sintesi
/// composta — stesso parser per entrambi i livelli, regola L). Ogni elemento
/// e' o una stringa nuda (formato storico, direzione assente) o un oggetto
/// `{text, direction?}` (formato dichiarato, emesso da
/// [`super::tool_dispatch::normalize_advisory_verdict`]): entrambe le forme
/// sono tollerate qui perche' e' anche il punto in cui i test di questo
/// modulo costruiscono l'`advisory` a mano, bypassando la normalizzazione.
/// Elementi senza testo non vuoto sono scartati.
pub(super) fn requirement_list(advisory: &Value, key: &str) -> Vec<Requirement> {
    advisory
        .get(key)
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(parse_one_requirement).collect())
        .unwrap_or_default()
}

/// Un singolo elemento dell'array `requirements` (stringa nuda o oggetto
/// `{text, direction?}`), estratto in funzione propria: tenere il match fuori
/// dalla closure di [`requirement_list`] evita l'annidamento profondo di due
/// livelli (iteratore + match) sullo stesso corpo.
fn parse_one_requirement(v: &Value) -> Option<Requirement> {
    match v {
        Value::String(s) => {
            let t = s.trim();
            (!t.is_empty()).then(|| Requirement {
                text: t.to_string(),
                direction: None,
            })
        }
        Value::Object(o) => {
            let text = o.get("text").and_then(Value::as_str)?.trim();
            (!text.is_empty()).then(|| Requirement {
                text: text.to_string(),
                direction: o
                    .get("direction")
                    .and_then(Value::as_str)
                    .and_then(Direction::parse),
            })
        }
        _ => None,
    }
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
    // Punto unico del vocabolario gravita' (regola L): il test dell'evidenza
    // grave e' lo stesso di review e debate — vive in `severity`, non qui.
    let has_high_severity = super::severity::any_high(&risks);
    Some(Advice {
        verdict,
        has_high_severity,
        requirements: requirement_list(advisory, "requirements"),
        risks,
        recommendations: string_list(advisory, "recommendations"),
        // Ri-validata QUI e non data per buona: l'outcome puo' arrivare da un
        // sub-run vecchio o da un percorso che non e' passato dal normalizzatore
        // del tool (regola M: si valida al confine che si attraversa).
        contested_decision: super::tool_dispatch::normalize_contested_decision(
            advisory.get("contested_decision"),
        ),
    })
}

/// Aggiunge gli elementi nuovi (non gia' presenti) a `acc`, preservando l'ordine
/// di prima apparizione (dedup stabile, regola L: unica sede della dedup del
/// panel, generica su stringhe e su [`Requirement`] invece di due funzioni
/// copiate). Per `Requirement` l'uguaglianza e' sull'intero valore (testo E
/// direzione): due dichiarazioni con lo stesso testo ma direzione diversa sono
/// un disaccordo fra figure, non un duplicato — restano entrambe visibili
/// invece di scomparire silenziosamente sotto la prima.
fn extend_dedup<T: PartialEq>(acc: &mut Vec<T>, items: Vec<T>) {
    for it in items {
        if !acc.iter().any(|e| e == &it) {
            acc.push(it);
        }
    }
}

/// Compone la sintesi del panel advisory dagli esiti strutturati dei sub-run
/// delle figure. `outcomes` e' la lista dei blocchi `outcome` (structured_verdict);
/// gli elementi senza `advisory` valido sono ignorati (non sono figure del panel).
/// `roster` e' il denominatore del quorum dichiarato dal chiamante (regola M: le
/// figure convocate ma senza esito sono astensioni che PESANO, non righe che
/// spariscono dal conteggio).
///
/// `None` se NESSUN outcome porta un `advisory` (il batch non e' un panel di
/// analisi): il coordinatore non aggiunge alcuna sintesi. `Some(_)` con
/// `Inconclusive` se ci sono pareri ma i voti validi sono sotto la soglia
/// effettiva (`max(min_valid_advisories, ceil(convocate * quorum_pct / 100))`).
///
/// Determinismo: requisiti e raccomandazioni seguono l'ordine degli `outcomes`
/// (dedup stabile); i rischi sono ordinati per severity con sort STABILE, quindi
/// a parita' di severity restano nell'ordine degli `outcomes`.
pub fn compose_advisory_synthesis(
    outcomes: &[Value],
    policy: &AdvisoryPolicy,
    roster: AdvisoryRoster,
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
    let mut requirements: Vec<Requirement> = Vec::new();
    let mut recommendations: Vec<String> = Vec::new();
    let mut risks: Vec<Value> = Vec::new();
    let mut contested_decision: Option<Value> = None;
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
        // Prima dichiarazione valida nell'ordine degli outcomes: un dibattito ha
        // UN oggetto conteso, non uno per figura (le successive sono ignorate).
        if contested_decision.is_none() {
            contested_decision.clone_from(&a.contested_decision);
        }
    }
    // Ordine per severity, STABILE (a parita' resta l'ordine di apparizione).
    risks.sort_by_key(severity_rank);

    let valid = advices.len();
    let distinct =
        usize::from(proceed > 0) + usize::from(proceed_with_changes > 0) + usize::from(block > 0);

    // Denominatore e soglia effettiva del quorum: punto unico `required_valid`
    // (panel_quorum, regola L). Il match e' esaustivo: un roster nuovo non
    // compila finche' non dichiara il proprio denominatore.
    let (convened, roster_size) = match roster {
        AdvisoryRoster::Convened(n) => (n, Some(n)),
        AdvisoryRoster::SelfDeclared => (total_advisories, None),
    };
    let required = required_valid(policy.min_valid_advisories, roster_size, policy.quorum_pct);

    let class = classify_panel(
        &QuorumTally {
            valid,
            veto: block,
            conditional: proceed_with_changes,
            any_high_severity_veto: any_high_severity_block,
        },
        &QuorumPolicy {
            min_valid: required,
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
        convened,
        required_valid: required,
        proceed,
        proceed_with_changes,
        block,
        dissent: distinct > 1,
        requirements,
        risks,
        recommendations,
        contested_decision,
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

    /// Blocco outcome di una figura CONVOCATA ma senza esito (timeout/errore):
    /// astensione che deve pesare nel denominatore, non sparire.
    fn abstain() -> Value {
        json!({"verdict": "timed_out", "success": false, "advisory": Value::Null})
    }

    #[test]
    fn nessun_advisory_nel_batch_ritorna_none() {
        // Batch di soli worker (outcome senza advisory): non e' un panel.
        let worker = json!({"verdict": "completed", "success": true, "advisory": Value::Null});
        assert!(compose_advisory_synthesis(
            &[worker],
            &AdvisoryPolicy::default(),
            AdvisoryRoster::SelfDeclared,
        )
        .is_none());
    }

    #[test]
    fn tutti_proceed_e_clear() {
        let out = compose_advisory_synthesis(
            &[
                figure("proceed", json!([]), json!([])),
                figure("proceed", json!([]), json!([])),
            ],
            &AdvisoryPolicy::default(),
            AdvisoryRoster::Convened(2),
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
            AdvisoryRoster::Convened(2),
        )
        .unwrap();
        assert_eq!(out.verdict, AdvisoryPanelVerdict::Block);
        assert!(out.verdict.is_veto());
        assert!(out.dissent, "proceed + block = dissenso");
        assert_eq!(out.risks.len(), 1);
        assert_eq!(
            out.requirements,
            vec![Requirement {
                text: "usa PKCE".to_string(),
                direction: None
            }]
        );
    }

    #[test]
    fn block_senza_high_severity_declassa_a_proceed_with_changes() {
        let media = json!([{"severity": "media", "description": "nit"}]);
        let out = compose_advisory_synthesis(
            &[figure("block", media, json!([]))],
            &AdvisoryPolicy {
                min_valid_advisories: 1,
                quorum_pct: 50,
                block_on_high_severity: true,
            },
            AdvisoryRoster::Convened(1),
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
                quorum_pct: 50,
                block_on_high_severity: false,
            },
            AdvisoryRoster::Convened(1),
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
            AdvisoryRoster::Convened(2),
        )
        .unwrap();
        assert_eq!(out.verdict, AdvisoryPanelVerdict::ProceedWithChanges);
        assert!(out.dissent);
        assert_eq!(
            out.requirements,
            vec![Requirement {
                text: "valida input".to_string(),
                direction: None
            }]
        );
    }

    #[test]
    fn astensione_non_conta_come_voto() {
        // Una figura in timeout (success=false, advisory assente) NON vota: sotto
        // min_valid=2 il panel e' inconclusive nonostante un proceed valido.
        let out = compose_advisory_synthesis(
            &[figure("proceed", json!([]), json!([])), abstain()],
            &AdvisoryPolicy {
                min_valid_advisories: 2,
                quorum_pct: 50,
                block_on_high_severity: true,
            },
            AdvisoryRoster::Convened(2),
        )
        .unwrap();
        assert_eq!(out.verdict, AdvisoryPanelVerdict::Inconclusive);
        assert!(!out.verdict.is_clear());
        assert_eq!(out.valid, 1);
        assert_eq!(
            out.total_advisories, 1,
            "l'astensione senza advisory non conta"
        );
        assert_eq!(out.convened, 2, "ma pesa nel denominatore del quorum");
        assert_eq!(out.required_valid, 2);
    }

    #[test]
    fn quorum_relativo_una_su_cinque_inconclusive() {
        // Il caso di campo (incidente 2026-07-14): 5 figure convocate, 4 in
        // timeout/errore, 1 sola vota proceed. Il quorum relativo (50% di 5 = 3)
        // NON e' raggiunto: il panel e' Inconclusive, mai un "proceed" spacciato
        // per consenso del consiglio.
        let out = compose_advisory_synthesis(
            &[
                figure("proceed", json!([]), json!([])),
                abstain(),
                abstain(),
                abstain(),
                abstain(),
            ],
            &AdvisoryPolicy::default(),
            AdvisoryRoster::Convened(5),
        )
        .unwrap();
        assert_eq!(out.verdict, AdvisoryPanelVerdict::Inconclusive);
        assert!(!out.verdict.is_clear());
        assert_eq!(out.valid, 1);
        assert_eq!(out.convened, 5);
        assert_eq!(out.required_valid, 3);
    }

    #[test]
    fn quorum_relativo_raggiunto_delibera() {
        // 3 voti validi su 5 convocate: quorum 50% (ceil(2.5)=3) raggiunto.
        let out = compose_advisory_synthesis(
            &[
                figure("proceed", json!([]), json!([])),
                figure("proceed", json!([]), json!([])),
                figure("proceed", json!([]), json!([])),
                abstain(),
                abstain(),
            ],
            &AdvisoryPolicy::default(),
            AdvisoryRoster::Convened(5),
        )
        .unwrap();
        assert_eq!(out.verdict, AdvisoryPanelVerdict::Proceed);
        assert_eq!((out.valid, out.convened, out.required_valid), (3, 5, 3));
    }

    #[test]
    fn self_declared_mantiene_soglia_assoluta() {
        // Fan-in generico: roster ignoto, un solo advisory dichiarato resta
        // conclusivo con min_valid=1 (semantica storica del batch misto).
        let out = compose_advisory_synthesis(
            &[figure("proceed", json!([]), json!([]))],
            &AdvisoryPolicy::default(),
            AdvisoryRoster::SelfDeclared,
        )
        .unwrap();
        assert_eq!(out.verdict, AdvisoryPanelVerdict::Proceed);
        assert_eq!((out.convened, out.required_valid), (1, 1));
    }

    #[test]
    fn to_value_espone_quorum_strutturato() {
        let out = compose_advisory_synthesis(
            &[figure("proceed", json!([]), json!([])), abstain()],
            &AdvisoryPolicy::default(),
            AdvisoryRoster::Convened(2),
        )
        .unwrap();
        let v = out.to_value();
        assert_eq!(v["convened"], json!(2));
        assert_eq!(v["required_valid"], json!(1));
        assert_eq!(v["valid"], json!(1));
    }

    #[test]
    fn requisiti_deduplicati_ordine_stabile() {
        let out = compose_advisory_synthesis(
            &[
                figure("proceed_with_changes", json!([]), json!(["A", "B"])),
                figure("proceed_with_changes", json!([]), json!(["B", "C"])),
            ],
            &AdvisoryPolicy::default(),
            AdvisoryRoster::Convened(2),
        )
        .unwrap();
        // B compare una sola volta, ordine di prima apparizione.
        assert_eq!(
            out.requirements.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            vec!["A", "B", "C"]
        );
    }

    /// Il difetto reale (30/07/2026): una figura dichiara la direzione nel
    /// campo strutturato invece di lasciarla indovinare dai verbi a valle.
    /// La sintesi la porta fino in fondo, e la dedup non la perde.
    #[test]
    fn direzione_dichiarata_sopravvive_alla_sintesi() {
        let requirements = json!([
            {"text": "Sostituire `port: 33649` con una porta dinamica", "direction": "must_be_absent"},
            {"text": "Aggiungere `strictPort: false`", "direction": "must_be_present"},
        ]);
        let out = compose_advisory_synthesis(
            &[figure("proceed_with_changes", json!([]), requirements)],
            &AdvisoryPolicy::default(),
            AdvisoryRoster::Convened(1),
        )
        .unwrap();
        assert_eq!(
            out.requirements,
            vec![
                Requirement {
                    text: "Sostituire `port: 33649` con una porta dinamica".to_string(),
                    direction: Some(Direction::DeveMancare),
                },
                Requirement {
                    text: "Aggiungere `strictPort: false`".to_string(),
                    direction: Some(Direction::DevePresenziare),
                },
            ]
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
            AdvisoryRoster::Convened(3),
        )
        .unwrap();
        // alta -> media -> bassa.
        assert_eq!(out.risks[0]["severity"], json!("alta"));
        assert_eq!(out.risks[1]["severity"], json!("media"));
        assert_eq!(out.risks[2]["severity"], json!("bassa"));
    }
}
