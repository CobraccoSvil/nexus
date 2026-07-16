//! `debate_panel`: PUNTO UNICO (regola L) delle TESI CONTRAPPOSTE — assegnazione
//! delle posizioni agli avvocati e composizione dell'esito del dibattito.
//!
//! Perche' esiste (gap architetturale colmato): il motore sapeva MISURARE il
//! dissenso emerso ([`super::advisory_panel::AdvisorySynthesis::dissent`]) e dare
//! il veto alla minoranza con evidenza, ma non sapeva PROVOCARE il disaccordo:
//! tutte le figure del consiglio ricevono lo stesso task e nessun prompt dice
//! "argomenta contro". Su una decisione architetturale (A vs B) il consenso
//! apparente di sei lenti concordi non e' una prova che l'alternativa sia
//! peggiore: nessuno l'ha difesa. Qui ogni avvocato riceve una POSIZIONE
//! ASSEGNATA e la difende con evidenza dal codice.
//!
//! Confini (concern disgiunti, nessun tipo condiviso):
//!   - [`super::advisory_panel`]: pareri di lenti diverse sullo STESSO oggetto ->
//!     verdetto approva/veta. Qui: posizioni AVVERSE su opzioni alternative ->
//!     SELEZIONE di un'opzione.
//!   - [`super::panel_quorum`]: gli si delega la SOGLIA di conclusivita'
//!     ([`required_valid`]) — e' letteralmente la stessa domanda ("quanti voti
//!     validi servono?"). NON gli si delega [`classify_panel`]: quella risponde
//!     ad "approva o veta?", che nel debate non e' la domanda (regola L: si
//!     condivide cio' che e' la stessa domanda, non si forza cio' che non lo e').
//!   - [`super::severity`]: punto unico del vocabolario di gravita'.
//!
//! Regola M: legge SOLO i segnali strutturati del tool `debate_position`
//! (`success`, `debate.assigned_position`, `debate.stance`,
//! `debate.risks[].severity`), MAI la prosa. Funzioni PURE: nessun I/O,
//! replay-stabili.
//!
//! **Il segnale piu' forte del dibattito e' l'avvocato che ARRENDE la propria
//! tesi**: `stance = "oppose"` significa "ho provato a difendere la posizione
//! assegnata e non regge". Non e' un voto per l'alternativa (l'avvocato non l'ha
//! studiata): e' evidenza CONTRO la propria, e con gravita' `alta` squalifica
//! l'opzione anche se altri la sostengono.

use serde_json::{json, Value};

use super::panel_quorum::{required_valid, QuorumPolicy};

/// Assegnazione di UNA posizione a UN avvocato (output del piano del dibattito).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebateAssignment {
    /// Indice dell'avvocato nel fan-out (0-based): identifica il sub-run.
    pub advocate_index: usize,
    /// La tesi che questo avvocato deve DIFENDERE (testo dell'opzione).
    pub assigned_position: String,
    /// Le posizioni AVVERSE che deve attaccare (tutte le altre opzioni).
    pub opposing_positions: Vec<String>,
}

/// Posizione dichiarata da UN avvocato via il tool `debate_position`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stance {
    /// L'avvocato SOSTIENE la posizione che gli e' stata assegnata.
    Support,
    /// L'avvocato, pur dovendola difendere, CONCLUDE contro la propria
    /// posizione assegnata (resa intellettualmente onesta): evidenza contro
    /// quell'opzione, non un voto per un'altra.
    Oppose,
}

impl Stance {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "support" => Some(Self::Support),
            "oppose" => Some(Self::Oppose),
            _ => None,
        }
    }

    /// Etichetta canonica (regola N). `const` cosi' `VALID_DEBATE_STANCES` (che
    /// il test di coerenza confronta con l'enum dello schema del tool) si derivi
    /// da qui invece di ripetere i letterali.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Support => "support",
            Self::Oppose => "oppose",
        }
    }
}

/// Esito AGGREGATO del dibattito (vocabolario canonico, regola N).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebatePanelVerdict {
    /// Un'opzione ha il supporto piu' alto, in solitaria, e non e' squalificata
    /// da evidenza grave contro.
    OptionSelected,
    /// Nessun vincitore netto: parita' di supporto, tutte le opzioni squalificate,
    /// o nessuna opzione sostenuta. Il coordinatore decide, informato.
    Split,
    /// Voti validi sotto la soglia di quorum: il dibattito non ha deliberato (il
    /// coordinatore NON deve leggerlo come una scelta — regola M).
    Inconclusive,
}

impl DebatePanelVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OptionSelected => "option_selected",
            Self::Split => "split",
            Self::Inconclusive => "inconclusive",
        }
    }
}

/// Supporto raccolto da UNA opzione.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionTally {
    pub option: String,
    /// Avvocati che la sostengono (`stance=support`).
    pub support: usize,
    /// Avvocati assegnati a difenderla che l'hanno ARRESA (`stance=oppose`).
    pub surrendered: usize,
    /// Squalificata: almeno una resa porta evidenza `alta` (secondo policy).
    pub disqualified: bool,
}

/// Minimo di opzioni che devono aver avuto ALMENO UNA voce (a favore o contro)
/// perche' il dibattito abbia confrontato qualcosa. Sotto questa soglia l'esito
/// e' `Inconclusive` a prescindere dal numero di voti: dieci avvocati che
/// parlano tutti della stessa opzione non hanno tenuto un dibattito. Invariante
/// di forma del confronto, non config di business (come il minimo di 2 opzioni
/// in [`plan_debate`]: e' la definizione stessa di "contraddittorio").
const MIN_OPTIONS_HEARD: usize = 2;

/// Sintesi strutturata del dibattito per il coordinatore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebateSynthesis {
    pub verdict: DebatePanelVerdict,
    /// L'opzione vincente, solo su [`DebatePanelVerdict::OptionSelected`].
    pub selected_option: Option<String>,
    /// Supporto per opzione, nell'ordine delle opzioni del dibattito (stabile).
    pub tally: Vec<OptionTally>,
    /// Voti validi / avvocati che hanno dichiarato una posizione.
    pub valid: usize,
    pub total_positions: usize,
    /// Avvocati CONVOCATI (denominatore del quorum, dichiarato dal chiamante).
    pub convened: usize,
    /// Soglia effettiva di voti validi (il lettore dichiara "X su N" senza
    /// ricalcolare nulla).
    pub required_valid: usize,
    /// Avvocati che hanno riecheggiato una posizione DIVERSA da quella assegnata:
    /// voto scartato (non interpretabile) ma CONTATO — un dibattito con molti
    /// misattributed dice che il prompt o il task non stanno passando (regola M:
    /// il difetto si vede in un numero, non si deduce dal silenzio).
    pub misattributed: usize,
    /// Opzioni che hanno avuto almeno una voce (a favore o contro): sotto
    /// [`MIN_OPTIONS_HEARD`] non c'e' stato confronto.
    pub options_heard: usize,
    /// Argomenti chiave uniti, dedup stabile.
    pub key_arguments: Vec<String>,
    /// Rischi aggregati, ordinati per gravita' (sort stabile).
    pub risks: Vec<Value>,
}

impl DebateSynthesis {
    /// Forma JSON del blocco `debate_synthesis` (regola M: il coordinatore legge
    /// questi campi, non la prosa).
    pub fn to_value(&self) -> Value {
        json!({
            "verdict": self.verdict.as_str(),
            "selected_option": self.selected_option,
            "valid": self.valid,
            "total_positions": self.total_positions,
            "convened": self.convened,
            "required_valid": self.required_valid,
            "misattributed": self.misattributed,
            "options_heard": self.options_heard,
            "tally": self.tally.iter().map(|t| json!({
                "option": t.option,
                "support": t.support,
                "surrendered": t.surrendered,
                "disqualified": t.disqualified,
            })).collect::<Vec<_>>(),
            "key_arguments": self.key_arguments,
            "risks": self.risks,
        })
    }
}

/// Pianifica il dibattito: distribuisce gli avvocati sulle opzioni a ROUND-ROBIN,
/// cosi' ogni opzione riceve almeno un difensore prima che una ne riceva due.
///
/// **Le opzioni in gara sono al massimo `advocates`**: se il consiglio ne dichiara
/// piu' di quanti avvocati il budget consente, le eccedenti sono TAGLIATE, non
/// lasciate indifese. Un'opzione senza difensore, attaccata da tutti gli altri
/// avvocati, perderebbe con supporto 0 in modo indistinguibile da una difesa che
/// non ha convinto: sarebbe un'esecuzione travestita da dibattito. Meglio un
/// confronto onesto su 2 opzioni che un finto confronto su 3 (il taglio e'
/// visibile: il tally elenca solo le opzioni in gara).
///
/// Vuoto (nessun dibattito) se le opzioni distinte sono meno di 2 o gli avvocati
/// meno di 2: una sola posizione non ha contraddittorio, un solo avvocato non ha
/// avversario. Gli avvocati eccedenti le opzioni in gara le rinforzano in ordine.
pub fn plan_debate(options: &[String], advocates: usize) -> Vec<DebateAssignment> {
    // Dedup case-insensitive coerente con `normalize_contested_decision` (i due
    // capi della pipeline devono concordare su cosa sia "la stessa opzione").
    let mut clean: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for o in options {
        let t = o.trim();
        if t.is_empty() {
            continue;
        }
        let key = t.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        clean.push(t.to_string());
    }
    if clean.len() < 2 || advocates < 2 {
        return Vec::new();
    }
    // Solo le opzioni che avranno un difensore entrano in gara.
    clean.truncate(advocates);
    (0..advocates)
        .map(|i| {
            let assigned = clean[i % clean.len()].clone();
            DebateAssignment {
                advocate_index: i,
                opposing_positions: clean.iter().filter(|o| **o != assigned).cloned().collect(),
                assigned_position: assigned,
            }
        })
        .collect()
}

/// Un voto valido estratto da un outcome (regola M).
struct Position {
    /// Posizione RIECHEGGIATA dal modello. NON e' la chiave di attribuzione (lo
    /// e' l'indice del sub-run): serve solo a verificare che l'avvocato abbia
    /// parlato della posizione che gli era stata assegnata.
    declared: String,
    stance: Stance,
    has_high_severity: bool,
    key_arguments: Vec<String>,
    risks: Vec<Value>,
}

/// Estrae la posizione da UN blocco `outcome`. `None` se non e' un voto valido:
/// sub-run non arrivato a esito (`success != true`) o senza `debate` dichiarato
/// (regola M: un'astensione non e' un voto).
fn extract_position(outcome: &Value) -> Option<Position> {
    if outcome.get("success").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let debate = outcome.get("debate")?;
    let declared = debate
        .get("assigned_position")
        .and_then(Value::as_str)?
        .trim()
        .to_string();
    if declared.is_empty() {
        return None;
    }
    let stance = Stance::parse(debate.get("stance").and_then(Value::as_str)?)?;
    let risks: Vec<Value> = debate
        .get("risks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Some(Position {
        declared,
        stance,
        has_high_severity: super::severity::any_high(&risks),
        key_arguments: debate
            .get("key_arguments")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        risks,
    })
}

/// `true` se la posizione riecheggiata dal modello e' quella che gli era stata
/// assegnata. Confronto normalizzato (trim + case-insensitive) coerente con la
/// dedup di `normalize_contested_decision`: i due capi della pipeline devono
/// concordare su cosa sia "la stessa opzione", altrimenti una maiuscola diversa
/// invaliderebbe un voto legittimo.
fn echoed_correctly(declared: &str, assigned: &str) -> bool {
    declared.trim().eq_ignore_ascii_case(assigned.trim())
}

/// Compone l'esito del dibattito dagli esiti strutturati dei sub-run avvocato.
///
/// **L'attribuzione del voto e' POSIZIONALE, non testuale** (regola M):
/// `outcomes[i]` e' il sub-run di `assignments[i]` — il fan-out preserva
/// l'ordine — quindi la posizione difesa da un avvocato e' un fatto STRUTTURALE
/// deciso da noi, non una stringa che il modello ricopia. La stringa dichiarata
/// serve solo a verificare che l'avvocato abbia svolto il compito assegnato: se
/// riecheggia una posizione diversa (tipicamente un'avversa), il suo voto NON e'
/// interpretabile e viene scartato come `misattributed` — contato, mai perso in
/// silenzio. Senza questo, un avvocato poteva dichiarare `assigned_position` =
/// l'opzione ALTRUI e, con un `oppose` grave, squalificare una tesi che non ha
/// mai difeso.
///
/// Le opzioni IN GARA sono le distinte di `assignments`: un'opzione esiste nel
/// tally solo se qualcuno la difende (le eccedenti sono gia' tagliate da
/// [`plan_debate`]).
///
/// Il denominatore del quorum e' `assignments.len()` — gli avvocati CONVOCATI
/// (lezione mig 0589: un avvocato in timeout e' un'assenza che pesa, non una
/// riga che sparisce dal conteggio).
///
/// `None` se nessun outcome porta un `debate` (il batch non e' un dibattito).
///
/// Precedenza dell'esito:
/// 1. voti validi sotto la soglia, OPPURE voci su meno di 2 opzioni distinte ->
///    `Inconclusive`. Il secondo gate e' il cuore: un "dibattito" in cui parla
///    solo il difensore di A non ha confrontato niente — e' l'ipotesi nulla, e
///    dichiararlo `option_selected` sarebbe la mig 0589 in versione debate
///    (consenso proclamato da una voce sola);
/// 2. opzioni squalificate: una resa con evidenza `alta` (o qualunque resa se la
///    policy non richiede la gravita') toglie l'opzione dalla corsa — il veto
///    della minoranza-con-evidenza vale qui come negli altri panel;
/// 3. massimo supporto in solitaria fra le superstiti -> `OptionSelected`;
/// 4. altrimenti -> `Split` (parita', tutte squalificate, nessun supporto).
pub fn compose_debate_synthesis(
    outcomes: &[Value],
    assignments: &[DebateAssignment],
    policy: &QuorumPolicy,
    quorum_pct: u8,
) -> Option<DebateSynthesis> {
    let total_positions = outcomes
        .iter()
        .filter(|o| o.get("debate").map(|d| !d.is_null()).unwrap_or(false))
        .count();
    if total_positions == 0 {
        return None;
    }
    let mut acc = Tallied::seeded(assignments);
    for (i, outcome) in outcomes.iter().enumerate() {
        // Fuori dal roster pianificato: il fan-out ha prodotto piu' esiti delle
        // assegnazioni (non accade dai call site reali; qui non si indovina).
        let Some(assignment) = assignments.get(i) else {
            continue;
        };
        if let Some(p) = extract_position(outcome) {
            acc.add(p, assignment, policy);
        }
    }
    acc.risks.sort_by_key(super::severity::rank);

    let convened = assignments.len();
    let required = required_valid(policy.min_valid, Some(convened), quorum_pct);
    let options_heard = acc.options_heard();
    let (verdict, selected_option) = decide(&acc.tally, acc.valid, required, options_heard);

    Some(DebateSynthesis {
        verdict,
        selected_option,
        tally: acc.tally,
        valid: acc.valid,
        total_positions,
        convened,
        required_valid: required,
        misattributed: acc.misattributed,
        options_heard,
        key_arguments: acc.key_arguments,
        risks: acc.risks,
    })
}

/// Accumulatore dei voti del dibattito: tiene il tally per opzione e i
/// sottoprodotti (argomenti, rischi, contatori). Esiste per tenere il CONTEGGIO
/// separato dalla DECISIONE ([`decide`]), che si legge meglio da sola.
struct Tallied {
    tally: Vec<OptionTally>,
    key_arguments: Vec<String>,
    risks: Vec<Value>,
    valid: usize,
    misattributed: usize,
}

impl Tallied {
    /// Semina il tally con le opzioni IN GARA = le distinte assegnate (ordine di
    /// prima assegnazione = ordine del consiglio: stabile e replay-safe).
    fn seeded(assignments: &[DebateAssignment]) -> Self {
        let mut tally: Vec<OptionTally> = Vec::new();
        for a in assignments {
            if !tally.iter().any(|t| t.option == a.assigned_position) {
                tally.push(OptionTally {
                    option: a.assigned_position.clone(),
                    support: 0,
                    surrendered: 0,
                    disqualified: false,
                });
            }
        }
        Self {
            tally,
            key_arguments: Vec::new(),
            risks: Vec::new(),
            valid: 0,
            misattributed: 0,
        }
    }

    /// Registra UN voto, attribuito all'opzione dell'ASSEGNAZIONE (non a quella
    /// riecheggiata dal modello).
    fn add(&mut self, p: Position, assignment: &DebateAssignment, policy: &QuorumPolicy) {
        if !echoed_correctly(&p.declared, &assignment.assigned_position) {
            // Il modello ha parlato di un'altra posizione: il suo stance non e'
            // riferibile ne' alla propria ne' all'altrui in modo affidabile.
            self.misattributed += 1;
            return;
        }
        let Some(slot) = self
            .tally
            .iter_mut()
            .find(|t| t.option == assignment.assigned_position)
        else {
            return;
        };
        self.valid += 1;
        match p.stance {
            Stance::Support => slot.support += 1,
            Stance::Oppose => {
                slot.surrendered += 1;
                if p.has_high_severity || !policy.veto_on_high_severity {
                    slot.disqualified = true;
                }
            }
        }
        for a in p.key_arguments {
            if !self.key_arguments.contains(&a) {
                self.key_arguments.push(a);
            }
        }
        self.risks.extend(p.risks);
    }

    /// Opzioni che hanno avuto ALMENO UNA voce (a favore o contro).
    fn options_heard(&self) -> usize {
        self.tally
            .iter()
            .filter(|t| t.support > 0 || t.surrendered > 0)
            .count()
    }
}

/// Decide l'esito dal tally (funzione pura, precedenza documentata su
/// [`compose_debate_synthesis`]).
fn decide(
    tally: &[OptionTally],
    valid: usize,
    required: usize,
    options_heard: usize,
) -> (DebatePanelVerdict, Option<String>) {
    if valid < required || options_heard < MIN_OPTIONS_HEARD {
        return (DebatePanelVerdict::Inconclusive, None);
    }
    let in_corsa = || tally.iter().filter(|t| !t.disqualified && t.support > 0);
    let Some(top) = in_corsa().map(|t| t.support).max() else {
        return (DebatePanelVerdict::Split, None);
    };
    let mut winners = in_corsa().filter(|t| t.support == top);
    match (winners.next(), winners.next()) {
        (Some(w), None) => (DebatePanelVerdict::OptionSelected, Some(w.option.clone())),
        _ => (DebatePanelVerdict::Split, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> QuorumPolicy {
        QuorumPolicy {
            min_valid: 1,
            veto_on_high_severity: true,
        }
    }

    fn opts() -> Vec<String> {
        vec!["A".to_string(), "B".to_string()]
    }

    fn vote(assigned: &str, stance: &str, risks: Value) -> Value {
        json!({
            "success": true,
            "debate": {
                "assigned_position": assigned,
                "stance": stance,
                "key_arguments": [format!("arg di {assigned}")],
                "risks": risks,
            }
        })
    }

    /// Sub-run morto (timeout/errore): astensione che pesa nel quorum.
    fn dead() -> Value {
        json!({"success": false, "debate": {"assigned_position": "A", "stance": "support"}})
    }

    /// Compone col piano REALE: `outcomes[i]` e' il sub-run di `assignments[i]`,
    /// come li produce `spawn_fanout`. Chiamare `plan_debate` nei test invece di
    /// costruire le assegnazioni a mano tiene i test ancorati alla catena vera.
    fn compose(outcomes: &[Value], options: &[String], advocates: usize) -> Option<DebateSynthesis> {
        let plan = plan_debate(options, advocates);
        compose_debate_synthesis(outcomes, &plan, &policy(), 50)
    }

    #[test]
    fn plan_round_robin_copre_ogni_opzione_prima_di_rinforzare() {
        let p = plan_debate(&opts(), 3);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].assigned_position, "A");
        assert_eq!(p[1].assigned_position, "B");
        assert_eq!(p[2].assigned_position, "A"); // rinforzo solo dopo la copertura
        assert_eq!(p[0].opposing_positions, vec!["B".to_string()]);
        assert_eq!(p[1].opposing_positions, vec!["A".to_string()]);
    }

    #[test]
    fn plan_degeneri_nessun_dibattito() {
        // Una sola opzione: nessun contraddittorio.
        assert!(plan_debate(&["A".to_string()], 4).is_empty());
        // Un solo avvocato: nessun avversario.
        assert!(plan_debate(&opts(), 1).is_empty());
        assert!(plan_debate(&[], 4).is_empty());
        // Opzioni vuote/whitespace scartate -> resta 1 sola -> niente dibattito.
        assert!(plan_debate(&["A".to_string(), "   ".to_string()], 4).is_empty());
        // Duplicati case-insensitive: "A" e "a" sono la stessa opzione -> 1 sola.
        assert!(plan_debate(&["A".to_string(), " a ".to_string()], 4).is_empty());
    }

    #[test]
    fn opzioni_oltre_gli_avvocati_sono_tagliate_mai_lasciate_indifese() {
        // 3 opzioni, 2 avvocati: la terza NON entra in gara. Prima restava nel
        // tally con support=0, attaccata da entrambi gli avvocati e difesa da
        // nessuno: sarebbe apparsa "sconfitta" senza essere mai stata difesa.
        let three = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let plan = plan_debate(&three, 2);
        assert_eq!(plan.len(), 2);
        let assegnate: Vec<&str> = plan.iter().map(|a| a.assigned_position.as_str()).collect();
        assert_eq!(assegnate, vec!["A", "B"]);
        // C non compare nemmeno fra le avverse: non e' in gara, non si attacca.
        for a in &plan {
            assert!(!a.opposing_positions.iter().any(|o| o == "C"));
        }
        let outcomes = vec![vote("A", "support", json!([])), vote("B", "support", json!([]))];
        let s = compose_debate_synthesis(&outcomes, &plan, &policy(), 50).expect("sintesi");
        assert_eq!(s.tally.len(), 2, "il tally elenca solo le opzioni in gara");
        assert!(!s.tally.iter().any(|t| t.option == "C"));
    }

    #[test]
    fn maggioranza_netta_seleziona_opzione() {
        // Piano: 0->A, 1->B, 2->A. Voti in ordine di assegnazione.
        let outcomes = vec![
            vote("A", "support", json!([])),
            vote("B", "support", json!([])),
            vote("A", "support", json!([])),
        ];
        let s = compose(&outcomes, &opts(), 3).expect("sintesi");
        assert_eq!(s.verdict, DebatePanelVerdict::OptionSelected);
        assert_eq!(s.selected_option.as_deref(), Some("A"));
        assert_eq!(s.valid, 3);
        assert_eq!(s.options_heard, 2);
    }

    #[test]
    fn parita_e_split() {
        let outcomes = vec![
            vote("A", "support", json!([])),
            vote("B", "support", json!([])),
        ];
        let s = compose(&outcomes, &opts(), 2).expect("sintesi");
        assert_eq!(s.verdict, DebatePanelVerdict::Split);
        assert_eq!(s.selected_option, None);
    }

    #[test]
    fn resa_con_evidenza_grave_squalifica_anche_la_favorita() {
        // Piano con 4 avvocati: 0->A, 1->B, 2->A, 3->B. A ha 2 sostenitori, ma
        // l'avvocato 3 (assegnato a B)... no: qui il 2 (assegnato ad A) la
        // arrende con evidenza alta -> A esce di corsa, vince B in solitaria.
        // E' il veto della minoranza-con-evidenza, coerente col resto dei panel.
        let outcomes = vec![
            vote("A", "support", json!([])),
            vote("B", "support", json!([])),
            vote(
                "A",
                "oppose",
                json!([{"severity": "alta", "description": "A rompe il punto unico"}]),
            ),
            vote("B", "support", json!([])),
        ];
        let s = compose(&outcomes, &opts(), 4).expect("sintesi");
        assert_eq!(s.verdict, DebatePanelVerdict::OptionSelected);
        assert_eq!(s.selected_option.as_deref(), Some("B"));
        let a = s.tally.iter().find(|t| t.option == "A").expect("A");
        assert!(a.disqualified);
        assert_eq!(a.support, 1, "il supporto resta misurato, ma A e' fuori");
    }

    #[test]
    fn resa_senza_evidenza_grave_non_squalifica() {
        // Piano: 0->A, 1->B, 2->A. L'avvocato 2 arrende A con severity media.
        let outcomes = vec![
            vote("A", "support", json!([])),
            vote("B", "support", json!([])),
            vote("A", "oppose", json!([{"severity": "media"}])),
        ];
        let s = compose(&outcomes, &opts(), 3).expect("sintesi");
        let a = s.tally.iter().find(|t| t.option == "A").expect("A");
        assert!(!a.disqualified);
        assert_eq!(a.surrendered, 1);
        // 1 support per A, 1 per B -> parita'.
        assert_eq!(s.verdict, DebatePanelVerdict::Split);
    }

    #[test]
    fn tutte_arrese_e_split_mai_una_scelta() {
        let high = json!([{"severity": "alta"}]);
        let outcomes = vec![
            vote("A", "oppose", high.clone()),
            vote("B", "oppose", high),
        ];
        let s = compose(&outcomes, &opts(), 2).expect("sintesi");
        assert_eq!(s.verdict, DebatePanelVerdict::Split);
        assert_eq!(s.selected_option, None);
    }

    #[test]
    fn sotto_quorum_inconclusive_lezione_0589() {
        // 4 avvocati convocati, 1 solo vota: col quorum al 50% servono 2 voti.
        let outcomes = vec![vote("A", "support", json!([])), dead(), dead(), dead()];
        let s = compose(&outcomes, &opts(), 4).expect("sintesi");
        assert_eq!(s.verdict, DebatePanelVerdict::Inconclusive);
        assert_eq!(s.selected_option, None);
        assert_eq!(s.required_valid, 2);
        assert_eq!(s.convened, 4);
    }

    #[test]
    fn una_sola_opzione_ascoltata_e_inconclusive_mai_una_vittoria() {
        // IL difetto della config di default: 2 avvocati (0->A, 1->B), muore
        // quello di B. Il quorum NUMERICO passa (required=1, valid=1) ma l'unica
        // voce e' il difensore di A che difende A: e' l'ipotesi nulla, zero
        // informazione comparativa, e B non e' mai stata difesa. Dichiararla
        // "option_selected" sarebbe la mig 0589 in versione dibattito.
        let outcomes = vec![vote("A", "support", json!([])), dead()];
        let s = compose(&outcomes, &opts(), 2).expect("sintesi");
        assert_eq!(s.valid, 1);
        assert_eq!(s.required_valid, 1, "il quorum numerico da solo passerebbe");
        assert_eq!(s.options_heard, 1);
        assert_eq!(
            s.verdict,
            DebatePanelVerdict::Inconclusive,
            "una sola opzione ascoltata non e' un confronto"
        );
        assert_eq!(s.selected_option, None);
    }

    #[test]
    fn tre_avvocati_ma_una_sola_opzione_ascoltata_resta_inconclusive() {
        // Variante che supera anche un quorum piu' severo: 3 avvocati (0->A,
        // 1->B, 2->A), muore l'UNICO difensore di B -> 2 voti validi, quorum
        // raggiunto, ma parlano solo gli avvocati di A.
        let outcomes = vec![
            vote("A", "support", json!([])),
            dead(),
            vote("A", "support", json!([])),
        ];
        let s = compose(&outcomes, &opts(), 3).expect("sintesi");
        assert_eq!(s.valid, 2);
        assert_eq!(s.required_valid, 2, "quorum numerico raggiunto");
        assert_eq!(s.options_heard, 1);
        assert_eq!(s.verdict, DebatePanelVerdict::Inconclusive);
    }

    #[test]
    fn avvocato_morto_non_vota() {
        let outcomes = vec![dead(), vote("B", "support", json!([]))];
        let s = compose(&outcomes, &opts(), 2).expect("sintesi");
        assert_eq!(s.valid, 1, "il sub-run fallito e' un'astensione");
        assert_eq!(s.total_positions, 2, "ma il roster lo conta");
    }

    #[test]
    fn voto_attribuito_per_indice_non_per_stringa_niente_veto_incrociato() {
        // Piano: 0->A, 1->B. L'avvocato 0 (assegnato ad A) dichiara di parlare
        // di B e la arrende con evidenza grave. Attribuendo per STRINGA, B
        // verrebbe squalificata da chi non l'ha mai difesa e A vincerebbe in
        // solitaria. Attribuendo per INDICE (verita' strutturale), il voto e'
        // scartato come misattributed: B resta in gara.
        let outcomes = vec![
            vote("B", "oppose", json!([{"severity": "alta", "description": "B non regge"}])),
            vote("B", "support", json!([])),
        ];
        let s = compose(&outcomes, &opts(), 2).expect("sintesi");
        assert_eq!(s.misattributed, 1, "il voto fuori compito e' CONTATO");
        assert_eq!(s.valid, 1);
        let b = s.tally.iter().find(|t| t.option == "B").expect("B");
        assert!(!b.disqualified, "B non e' squalificabile da chi difende A");
        assert_eq!(b.support, 1);
        // Una sola opzione ha avuto voce -> il gate del confronto scatta.
        assert_eq!(s.options_heard, 1);
        assert_eq!(s.verdict, DebatePanelVerdict::Inconclusive);
    }

    #[test]
    fn eco_con_case_o_spazi_diversi_resta_valida() {
        // Il modello riecheggia con maiuscole/spazi diversi: il voto e' suo e va
        // contato (il confronto e' normalizzato, coerente con la dedup delle
        // opzioni a monte). Prima un drift di formattazione lo faceva sparire in
        // silenzio.
        let options = vec!["Worktree dedicato".to_string(), "Lock per progetto".to_string()];
        let outcomes = vec![
            vote("  worktree DEDICATO  ", "support", json!([])),
            vote("Lock per progetto", "support", json!([])),
        ];
        let s = compose(&outcomes, &options, 2).expect("sintesi");
        assert_eq!(s.misattributed, 0);
        assert_eq!(s.valid, 2);
        assert_eq!(s.options_heard, 2);
        assert_eq!(s.verdict, DebatePanelVerdict::Split);
    }

    #[test]
    fn nessun_debate_nel_batch_ritorna_none() {
        let outcomes = vec![json!({"success": true, "advisory": {"verdict": "proceed"}})];
        let plan = plan_debate(&opts(), 2);
        assert!(compose_debate_synthesis(&outcomes, &plan, &policy(), 50).is_none());
    }

    #[test]
    fn rischi_ordinati_per_gravita_e_argomenti_dedotti() {
        let outcomes = vec![
            vote("A", "support", json!([{"severity": "bassa"}])),
            vote("B", "support", json!([{"severity": "alta"}])),
        ];
        let s = compose(&outcomes, &opts(), 2).expect("sintesi");
        assert_eq!(
            s.risks[0].get("severity").and_then(Value::as_str),
            Some("alta"),
            "i rischi gravi vengono per primi"
        );
        assert_eq!(s.key_arguments.len(), 2);
    }
}
