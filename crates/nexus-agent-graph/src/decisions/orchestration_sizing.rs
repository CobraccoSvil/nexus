//! `orchestration_sizing`: PUNTO UNICO (regola L) del DIMENSIONAMENTO dei panel
//! multi-agente (consiglio, review adversariale, multi-provider, debate) in base
//! al problema e al budget del run, al posto dei cap fissi.
//!
//! CONFINI coi moduli affini (concern DISGIUNTI, nessun tipo condiviso):
//!   - [`super::orchestration_reason`]: meta-reasoner delle MOSSE di orchestrazione
//!     a run avviato (plan-phase, decompose, delega) — LLM-driven, enum chiuso.
//!     Qui invece si decide QUANTI membri convocare in ciascun panel, PRIMA che
//!     il panel parta, in modo DETERMINISTICO.
//!   - [`super::scale_reason`]: dimensionamento del SINGOLO agente (tier modello,
//!     compressione contesto) tramite postura LLM bounded. Qui non c'e' postura
//!     LLM: gli input sono GIA' giudizi LLM strutturati (complexity del
//!     classificatore) + numeri (budget residuo); un secondo giudizio sarebbe
//!     ridondante. Se in futuro servisse una postura, `apply_sizing_gate` e' il
//!     precedente da clonare.
//!
//! Funzione PURA (regola L): nessun I/O; profili, backstop, stima unitaria e
//! budget arrivano come parametri espliciti gia' risolti dal call site mcp-core
//! (regola G, stesso patto di `AdvisoryPolicy`). Replay-stabile, golden-abile.
//!
//! Modello del calcolo: DOMANDA (profilo per-classe configurato da admin,
//! `orchestrator.sizing_profile_*`) vs OFFERTA (doppio vincolo costo+tempo:
//! vince il PIU' STRETTO, regola M sul campo `sized_by`). Se l'offerta non copre
//! la domanda si degrada nell'ordine INVERSO di `panel_priority`; ogni panel ha
//! un floor di quorum sotto il quale va a ZERO, mai convocato monco (lezione mig
//! 0589: un panel da 1 con min_valid=2 e' inconclusivo garantito = spesa inutile).
//! I cap storici (`council_max_figures`, `review_panel_size`, ...) restano come
//! BACKSTOP assoluti, letti dalle stesse chiavi: la DECISIONE e' del resolver.

use serde_json::{json, Value};

/// Classe di complessita' del task dichiarata dal classificatore LLM.
/// PUNTO UNICO del parse (regola N): `complexity_label_score` in
/// [`super::helpers`] e i call site mcp-core delegano qui, nessun secondo
/// riconoscimento delle label nel codebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskComplexity {
    Low,
    Medium,
    High,
}

impl TaskComplexity {
    /// Parse canonico (trim + case-insensitive). Qualunque altra label -> `None`
    /// (il chiamante degrada al piano legacy, mai a un default nascosto).
    pub fn try_parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    /// Identificatore canonico (regola N), per meta-step e chiavi profilo.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Panel dimensionabili, in vocabolario canonico (regola N). Il CSV
/// `orchestrator.sizing.panel_priority` si parsa SOLO qui.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKind {
    Council,
    MultiProvider,
    Debate,
    Review,
}

impl PanelKind {
    /// Parse canonico di un token del CSV di priorita'.
    pub fn try_parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "council" => Some(Self::Council),
            "multi_provider" => Some(Self::MultiProvider),
            "debate" => Some(Self::Debate),
            "review" => Some(Self::Review),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Council => "council",
            Self::MultiProvider => "multi_provider",
            Self::Debate => "debate",
            Self::Review => "review",
        }
    }
}

/// Ordine di degrado di default (l'ULTIMO si sacrifica per primo), usato quando
/// il CSV di priorita' e' vuoto o interamente malformato. Coincide col seed
/// della mig 0602: non e' un fallback di comportamento (regola G) ma il
/// safe-default del parser, gemello di `AdvisoryPolicy::default`.
pub const DEFAULT_PANEL_PRIORITY: [PanelKind; 4] = [
    PanelKind::Council,
    PanelKind::MultiProvider,
    PanelKind::Debate,
    PanelKind::Review,
];

/// Parse del CSV `orchestrator.sizing.panel_priority` (punto unico). Token
/// malformati o duplicati vengono ignorati; panel assenti vengono ACCODATI
/// nell'ordine di default (ogni panel ha SEMPRE una posizione di degrado).
pub fn parse_panel_priority(csv: &str) -> Vec<PanelKind> {
    let mut out: Vec<PanelKind> = Vec::with_capacity(4);
    for token in csv.split(',') {
        if let Some(kind) = PanelKind::try_parse(token) {
            if !out.contains(&kind) {
                out.push(kind);
            }
        }
    }
    for kind in DEFAULT_PANEL_PRIORITY {
        if !out.contains(&kind) {
            out.push(kind);
        }
    }
    out
}

/// DOMANDA di dimensionamento: il profilo per-classe configurato da admin
/// (chiavi `orchestrator.sizing_profile_low|medium|high`, JSON). Il call site
/// seleziona il profilo della classe e lo passa gia' parsato.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelDemand {
    pub council_figures: usize,
    pub reviewers: usize,
    pub providers: usize,
    pub advocates: usize,
}

/// Budget RESIDUO del run al momento del dimensionamento. `None` = vincolo non
/// configurato (nessun cap su quell'asse), MAI un default nascosto.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrchestrationBudgets {
    /// Capienza residua in costo: `run_cost_budget_usd - run_cost_cumulative_usd`.
    pub cost_remaining_usd: Option<f64>,
    /// Capienza residua in tempo: `deadline - now` (fase 3; `None` finche' la
    /// deadline non esiste).
    pub time_remaining_s: Option<i64>,
}

/// Stima UNITARIA di un sub-run advisory (costo e durata attesi), caricata dal
/// call site: modello risolto VIA TIER del purpose (mai per nome), prezzo dal
/// listino `nexus-pricing`, token/durata attesi dai settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanelUnitEstimate {
    pub cost_usd: f64,
    pub duration_s: i64,
}

/// BACKSTOP assoluti: i cap storici, letti dalle STESSE chiavi esistenti
/// (nessuna seconda fonte di verita'). Clamp finali, non piu' la decisione.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrchestrationBackstops {
    /// `orchestrator.council_max_figures`.
    pub council_max: usize,
    /// `orchestrator.review_panel_size` (tetto del rinforzo programmatico).
    pub review_max: usize,
    /// `orchestrator.multi_provider_min_providers` (floor di quorum del panel).
    pub multi_provider_min: usize,
    /// `orchestrator.multi_provider_max_providers`.
    pub multi_provider_max: usize,
    /// `orchestrator.debate_max_advocates` (mig 0605).
    pub debate_max: usize,
    /// `orchestrator.subagent_fanout_max_parallel` (mig 0596).
    pub fanout_max_parallel: usize,
}

/// Config del resolver (chiavi mig 0602, lette dal call site).
#[derive(Debug, Clone, PartialEq)]
pub struct OrchestrationSizingConfig {
    /// Kill-switch nested `orchestrator.sizing_enabled`: OFF -> piano legacy
    /// bit-identico ai cap storici.
    pub enabled: bool,
    /// `orchestrator.sizing.budget_share_pct`: quota % del budget residuo
    /// spendibile nei panel (il run principale deve restare finanziato).
    pub budget_share_pct: u8,
    /// Ordine di degrado (`orchestrator.sizing.panel_priority`, CSV canonico).
    pub panel_priority: Vec<PanelKind>,
}

/// QUALE vincolo ha deciso i numeri del piano (regola M: osservabilita'
/// strutturata nel meta-step, mai dedotta dalla prosa).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizingConstraint {
    /// La domanda del profilo e' passata integra: ha deciso la classe.
    Complexity,
    /// Il budget di COSTO ha stretto la domanda.
    CostBudget,
    /// Il budget di TEMPO ha stretto la domanda.
    TimeBudget,
    /// Un backstop assoluto ha limato la domanda (profilo sopra i cap).
    Backstop,
    /// Resolver spento o complexity non risolta: piano legacy.
    Disabled,
}

impl SizingConstraint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complexity => "complexity",
            Self::CostBudget => "cost_budget",
            Self::TimeBudget => "time_budget",
            Self::Backstop => "backstop",
            Self::Disabled => "disabled",
        }
    }
}

/// Piano di orchestrazione risolto: quanti membri per panel. 0 = panel NON
/// convocato (mai convocato sotto il proprio floor di quorum).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrchestrationPlan {
    pub council_figures: usize,
    pub review_panel_size: usize,
    pub multi_provider_providers: usize,
    pub debate_advocates: usize,
    /// Parallelismo del fan-out per QUESTO piano (<= backstop).
    pub fanout_parallelism: usize,
    pub sized_by: SizingConstraint,
}

impl OrchestrationPlan {
    /// Forma JSON per il meta-step `orchestration_plan` (chiavi canoniche).
    pub fn to_value(self) -> Value {
        json!({
            "council_figures": self.council_figures,
            "review_panel_size": self.review_panel_size,
            "multi_provider_providers": self.multi_provider_providers,
            "debate_advocates": self.debate_advocates,
            "fanout_parallelism": self.fanout_parallelism,
            "sized_by": self.sized_by.as_str(),
        })
    }
}

/// Floor di quorum per panel: sotto questo numero il panel NON si convoca
/// (va a 0). Council/debate richiedono pluralita' di lenti (2); la review
/// funziona anche con 1 revisore (quorum min_valid=1 storico); il floor del
/// multi-provider e' il suo setting `multi_provider_min` (parametrico).
fn panel_floor(kind: PanelKind, backstops: &OrchestrationBackstops) -> usize {
    match kind {
        PanelKind::Council => 2,
        PanelKind::MultiProvider => backstops.multi_provider_min.max(1),
        PanelKind::Debate => 2,
        PanelKind::Review => 1,
    }
}

/// Piano LEGACY: il comportamento ODIERNO dei cap fissi, bit-identico a flag
/// OFF (council/review/mp ai loro cap, debate assente).
pub fn legacy_plan(backstops: &OrchestrationBackstops) -> OrchestrationPlan {
    OrchestrationPlan {
        council_figures: backstops.council_max,
        review_panel_size: backstops.review_max,
        multi_provider_providers: backstops.multi_provider_max,
        debate_advocates: 0,
        fanout_parallelism: backstops.fanout_max_parallel.max(1),
        sized_by: SizingConstraint::Disabled,
    }
}

/// Normalizza la domanda di UN panel contro floor e backstop: domanda 0 resta 0
/// (l'admin ha escluso il panel dalla classe); domanda >0 viene alzata al floor
/// (l'intento admin di convocare il panel si rispetta al minimo di quorum) e
/// limata al backstop. Ritorna anche se il backstop ha effettivamente limato.
fn normalize(demand: usize, floor: usize, cap: usize) -> (usize, bool) {
    if demand == 0 || cap == 0 {
        return (0, demand > cap);
    }
    let raised = demand.max(floor);
    if raised > cap {
        // Cap sotto il floor = panel non convocabile in forma valida -> 0.
        if cap < floor {
            (0, true)
        } else {
            (cap, true)
        }
    } else {
        (raised, false)
    }
}

/// Quanti sub-run l'offerta consente su un asse. Valori non positivi o non
/// finiti della stima unitaria = vincolo NON calcolabile -> nessun limite su
/// quell'asse (il loader ha gia' l'obbligo di fornire stime valide; qui si
/// resta robusti senza inventare numeri).
fn affordable_by_cost(budgets: &OrchestrationBudgets, unit: &PanelUnitEstimate, share_pct: u8) -> Option<usize> {
    let remaining = budgets.cost_remaining_usd?;
    if !(remaining.is_finite() && unit.cost_usd.is_finite()) || unit.cost_usd <= 0.0 {
        return None;
    }
    let share = f64::from(share_pct.min(100)) / 100.0;
    let panel_budget = (remaining.max(0.0)) * share;
    Some((panel_budget / unit.cost_usd).floor() as usize)
}

fn affordable_by_time(
    budgets: &OrchestrationBudgets,
    unit: &PanelUnitEstimate,
    parallelism: usize,
) -> Option<usize> {
    let remaining = budgets.time_remaining_s?;
    if unit.duration_s <= 0 {
        return None;
    }
    let waves = (remaining.max(0) / unit.duration_s) as usize;
    Some(waves.saturating_mul(parallelism.max(1)))
}

/// Domanda del profilo normalizzata su floor e backstop, per i quattro panel.
/// Ritorna `(council, review, providers, advocates, backstop_clamped)`.
///
/// Bump sistemico: uno `scope=system_wide` alza di 1 le lenti (consiglio) e i
/// provider — un problema che tocca tutto il sistema merita una voce in piu'.
/// Un panel che l'admin ha escluso dalla classe (0 nel profilo) resta 0: il
/// bump non resuscita un panel spento.
///
/// Il debate e' l'unico dimensionato da un segnale di RUNTIME
/// (`decision_detected`): senza decisione contesa dichiarata non si riserva
/// spesa per un dibattito che potrebbe non esserci.
fn normalized_demand(
    demand: &PanelDemand,
    scope_system_wide: bool,
    decision_detected: bool,
    backstops: &OrchestrationBackstops,
) -> (usize, usize, usize, usize, bool) {
    let bump = usize::from(scope_system_wide);
    let raw_council = demand.council_figures.saturating_add(bump);
    let raw_providers = if demand.providers > 0 {
        demand.providers.saturating_add(bump)
    } else {
        0
    };
    let raw_advocates = if decision_detected { demand.advocates } else { 0 };

    let (council, clamp_c) = normalize(
        raw_council,
        panel_floor(PanelKind::Council, backstops),
        backstops.council_max,
    );
    let (review, clamp_r) = normalize(
        demand.reviewers,
        panel_floor(PanelKind::Review, backstops),
        backstops.review_max,
    );
    let (providers, clamp_p) = normalize(
        raw_providers,
        panel_floor(PanelKind::MultiProvider, backstops),
        backstops.multi_provider_max,
    );
    let (advocates, clamp_a) = normalize(
        raw_advocates,
        panel_floor(PanelKind::Debate, backstops),
        backstops.debate_max,
    );
    (
        council,
        review,
        providers,
        advocates,
        clamp_c || clamp_r || clamp_p || clamp_a,
    )
}

/// Sub-run che l'OFFERTA consente in totale, e QUALE dei due assi (costo o
/// tempo) e' il piu' stretto — cioe' quello che va dichiarato in `sized_by`
/// (regola M: il vincolo che ha deciso e' un dato osservabile, non un'ipotesi
/// del lettore).
///
/// `None` = nessun vincolo calcolabile (budget non configurati o stima unitaria
/// degenere): il piano resta quello della domanda.
fn offer_and_tighter(
    budgets: &OrchestrationBudgets,
    unit: &PanelUnitEstimate,
    cfg: &OrchestrationSizingConfig,
    parallelism: usize,
) -> (Option<usize>, SizingConstraint) {
    let by_cost = affordable_by_cost(budgets, unit, cfg.budget_share_pct);
    let by_time = affordable_by_time(budgets, unit, parallelism);
    let offer = match (by_cost, by_time) {
        (Some(c), Some(t)) => Some(c.min(t)),
        (Some(c), None) => Some(c),
        (None, Some(t)) => Some(t),
        (None, None) => None,
    };
    let tighter = match (by_cost, by_time) {
        (Some(c), Some(t)) if t < c => SizingConstraint::TimeBudget,
        (None, Some(_)) => SizingConstraint::TimeBudget,
        _ => SizingConstraint::CostBudget,
    };
    (offer, tighter)
}

/// Indice del panel in `slots` di [`degrade_to_offer`] (ordine fisso: council,
/// review, providers, advocates).
fn slot_index(kind: PanelKind) -> usize {
    match kind {
        PanelKind::Council => 0,
        PanelKind::Review => 1,
        PanelKind::MultiProvider => 2,
        PanelKind::Debate => 3,
    }
}

/// Riduce i panel finche' il totale non sta nell'offerta, IN DUE PASSI.
///
/// Passo A — riduzione verso i floor in ordine INVERSO di priorita' (l'ultimo si
/// sacrifica per primo), con riduzioni PARZIALI: si ferma appena l'offerta copre
/// la domanda.
///
/// Passo B — se anche tutti-al-floor eccede l'offerta: ricostruzione FIT-FIRST
/// in ordine DIRETTO di priorita' — ogni panel entra al proprio floor solo se ci
/// sta PER INTERO nella capienza residua, altrimenti va a 0 (mai monco, lezione
/// mig 0589). Cosi' con capienza minima sopravvive il panel a priorita' piu' alta
/// che ci sta davvero: non si azzera tutto e non si spreca offerta.
fn degrade_to_offer(
    slots: &mut [usize; 4],
    offer_total: usize,
    backstops: &OrchestrationBackstops,
    cfg: &OrchestrationSizingConfig,
) {
    let priority = if cfg.panel_priority.is_empty() {
        DEFAULT_PANEL_PRIORITY.to_vec()
    } else {
        cfg.panel_priority.clone()
    };
    let total = |s: &[usize; 4]| s.iter().sum::<usize>();

    // Passo A: verso i floor, in ordine inverso, riduzioni parziali.
    for kind in priority.iter().rev() {
        let excess = total(slots).saturating_sub(offer_total);
        if excess == 0 {
            return;
        }
        let i = slot_index(*kind);
        if slots[i] == 0 {
            continue;
        }
        let reducible = slots[i].saturating_sub(panel_floor(*kind, backstops));
        slots[i] -= reducible.min(excess);
    }
    if total(slots) <= offer_total {
        return;
    }
    // Passo B: floors ancora oltre l'offerta -> fit-first per priorita'.
    let mut remaining = offer_total;
    for kind in priority.iter() {
        let i = slot_index(*kind);
        if slots[i] > 0 && slots[i] <= remaining {
            remaining -= slots[i];
        } else {
            slots[i] = 0;
        }
    }
}

/// Risolve il piano di orchestrazione: domanda per-classe vs offerta a doppio
/// vincolo, degrado per priorita', clamp sui backstop. PUNTO UNICO (regola L):
/// `spawn_agent_run` lo chiama pre-run e `maybe_convene_review_panel` lo
/// RICHIAMA post-run coi budget residui reali; il debate viene ri-dimensionato
/// quando il consiglio dichiara `contested_decision` (`decision_detected=true`).
/// A `decision_detected=false` gli avvocati sono 0: non si riserva spesa per un
/// debate che potrebbe non esserci.
#[allow(clippy::too_many_arguments)]
pub fn resolve_orchestration_plan(
    complexity: Option<TaskComplexity>,
    scope_system_wide: bool,
    decision_detected: bool,
    budgets: &OrchestrationBudgets,
    unit: &PanelUnitEstimate,
    demand: &PanelDemand,
    backstops: &OrchestrationBackstops,
    cfg: &OrchestrationSizingConfig,
) -> OrchestrationPlan {
    // Fail-safe (regola M): spento o classe non risolta -> comportamento odierno,
    // mai un piano dimensionato su un fallback del classificatore.
    if !cfg.enabled || complexity.is_none() {
        return legacy_plan(backstops);
    }

    // 1) DOMANDA: profilo della classe, normalizzata su floor e backstop.
    let (council, review, providers, advocates, backstop_clamped) =
        normalized_demand(demand, scope_system_wide, decision_detected, backstops);
    let mut slots = [council, review, providers, advocates];

    // 2) OFFERTA: doppio vincolo, vince il piu' stretto (regola M su sized_by).
    let parallelism = backstops.fanout_max_parallel.max(1);
    let (offer, tighter) = offer_and_tighter(budgets, unit, cfg, parallelism);

    // 3) DEGRADO se l'offerta non copre la domanda (vedi [`degrade_to_offer`]).
    let mut budget_bound: Option<SizingConstraint> = None;
    if let Some(offer_total) = offer {
        if slots.iter().sum::<usize>() > offer_total {
            budget_bound = Some(tighter);
            degrade_to_offer(&mut slots, offer_total, backstops, cfg);
        }
    }

    let sized_by = match (budget_bound, backstop_clamped) {
        (Some(bound), _) => bound,
        (None, true) => SizingConstraint::Backstop,
        (None, false) => SizingConstraint::Complexity,
    };
    plan_from(slots, parallelism, sized_by)
}

/// Assembla il piano dagli slot risolti (ordine fisso: council, review,
/// providers, advocates — vedi [`slot_index`]). Il parallelismo non supera mai
/// il totale pianificato: aprire 6 permessi per 3 sub-run non serve a nessuno.
fn plan_from(
    slots: [usize; 4],
    parallelism: usize,
    sized_by: SizingConstraint,
) -> OrchestrationPlan {
    let planned_total: usize = slots.iter().sum();
    OrchestrationPlan {
        council_figures: slots[0],
        review_panel_size: slots[1],
        multi_provider_providers: slots[2],
        debate_advocates: slots[3],
        fanout_parallelism: parallelism.min(planned_total.max(1)),
        sized_by,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backstops() -> OrchestrationBackstops {
        OrchestrationBackstops {
            council_max: 6,
            review_max: 4,
            multi_provider_min: 2,
            multi_provider_max: 3,
            debate_max: 4,
            fanout_max_parallel: 6,
        }
    }

    fn cfg_on() -> OrchestrationSizingConfig {
        OrchestrationSizingConfig {
            enabled: true,
            budget_share_pct: 20,
            panel_priority: DEFAULT_PANEL_PRIORITY.to_vec(),
        }
    }

    fn demand_high() -> PanelDemand {
        PanelDemand {
            council_figures: 5,
            reviewers: 2,
            providers: 3,
            advocates: 2,
        }
    }

    fn unit() -> PanelUnitEstimate {
        PanelUnitEstimate {
            cost_usd: 0.05,
            duration_s: 240,
        }
    }

    fn no_budgets() -> OrchestrationBudgets {
        OrchestrationBudgets {
            cost_remaining_usd: None,
            time_remaining_s: None,
        }
    }

    #[test]
    fn disabled_ritorna_piano_legacy_bit_identico() {
        let cfg = OrchestrationSizingConfig {
            enabled: false,
            ..cfg_on()
        };
        let plan = resolve_orchestration_plan(
            Some(TaskComplexity::High),
            false,
            false,
            &no_budgets(),
            &unit(),
            &demand_high(),
            &backstops(),
            &cfg,
        );
        assert_eq!(plan, legacy_plan(&backstops()));
        assert_eq!(plan.sized_by, SizingConstraint::Disabled);
    }

    #[test]
    fn complexity_non_risolta_ritorna_legacy() {
        let plan = resolve_orchestration_plan(
            None,
            false,
            false,
            &no_budgets(),
            &unit(),
            &demand_high(),
            &backstops(),
            &cfg_on(),
        );
        assert_eq!(plan, legacy_plan(&backstops()));
    }

    #[test]
    fn high_con_budget_ampio_domanda_piena() {
        let budgets = OrchestrationBudgets {
            cost_remaining_usd: Some(100.0),
            time_remaining_s: Some(36_000),
        };
        let plan = resolve_orchestration_plan(
            Some(TaskComplexity::High),
            false,
            true,
            &budgets,
            &unit(),
            &demand_high(),
            &backstops(),
            &cfg_on(),
        );
        assert_eq!(plan.council_figures, 5);
        assert_eq!(plan.review_panel_size, 2);
        assert_eq!(plan.multi_provider_providers, 3);
        assert_eq!(plan.debate_advocates, 2);
        assert_eq!(plan.sized_by, SizingConstraint::Complexity);
    }

    #[test]
    fn senza_decisione_rilevata_zero_avvocati() {
        let plan = resolve_orchestration_plan(
            Some(TaskComplexity::High),
            false,
            false,
            &no_budgets(),
            &unit(),
            &demand_high(),
            &backstops(),
            &cfg_on(),
        );
        assert_eq!(plan.debate_advocates, 0);
    }

    #[test]
    fn costo_stretto_degrada_in_ordine_di_priorita_inversa() {
        // 20% di 1 USD = 0.20 -> 4 sub-run a 0.05. Domanda 5+2+3+2=12.
        // Priorita' default: review si sacrifica per prima, poi debate, poi mp.
        let budgets = OrchestrationBudgets {
            cost_remaining_usd: Some(1.0),
            time_remaining_s: None,
        };
        let plan = resolve_orchestration_plan(
            Some(TaskComplexity::High),
            false,
            true,
            &budgets,
            &unit(),
            &demand_high(),
            &backstops(),
            &cfg_on(),
        );
        assert_eq!(plan.sized_by, SizingConstraint::CostBudget);
        // Passo A (verso i floor, ordine inverso): review 2->1, debate resta 2
        // (gia' al floor), mp 3->2, council 5->2. Totale 7, ancora > 4.
        // Passo B (fit-first per priorita', capienza 4): council 2 entra (resto 2),
        // mp 2 entra (resto 0), debate e review non ci stanno -> 0.
        // Sopravvivono i due panel a priorita' piu' alta, entrambi al floor.
        assert_eq!(plan.council_figures, 2);
        assert_eq!(plan.multi_provider_providers, 2);
        assert_eq!(plan.debate_advocates, 0);
        assert_eq!(plan.review_panel_size, 0);
        let total = plan.council_figures + plan.multi_provider_providers;
        assert_eq!(total, 4);
    }

    #[test]
    fn tempo_stretto_vince_sul_costo_quando_piu_stringente() {
        // Costo permette 400 sub-run; tempo: 240s residui / 240s = 1 ondata x 6 = 6.
        let budgets = OrchestrationBudgets {
            cost_remaining_usd: Some(100.0),
            time_remaining_s: Some(240),
        };
        let plan = resolve_orchestration_plan(
            Some(TaskComplexity::High),
            false,
            true,
            &budgets,
            &unit(),
            &demand_high(),
            &backstops(),
            &cfg_on(),
        );
        assert_eq!(plan.sized_by, SizingConstraint::TimeBudget);
        let total = plan.council_figures
            + plan.review_panel_size
            + plan.multi_provider_providers
            + plan.debate_advocates;
        assert!(total <= 6, "totale {total} oltre l'offerta tempo");
    }

    #[test]
    fn sotto_floor_il_panel_va_a_zero_mai_monco() {
        // Offerta = 1: nessun panel con floor 2 puo' vivere; sopravvive solo la
        // review (floor 1).
        let budgets = OrchestrationBudgets {
            cost_remaining_usd: Some(0.25),
            time_remaining_s: None,
        };
        let plan = resolve_orchestration_plan(
            Some(TaskComplexity::High),
            false,
            true,
            &budgets,
            &unit(),
            &demand_high(),
            &backstops(),
            &cfg_on(),
        );
        assert_eq!(plan.council_figures + plan.review_panel_size + plan.multi_provider_providers + plan.debate_advocates, 1);
        assert_eq!(plan.review_panel_size, 1);
        assert!(plan.council_figures == 0 && plan.multi_provider_providers == 0 && plan.debate_advocates == 0);
    }

    #[test]
    fn profilo_sopra_i_backstop_viene_limato_sized_by_backstop() {
        let demand = PanelDemand {
            council_figures: 12,
            reviewers: 9,
            providers: 8,
            advocates: 7,
        };
        let plan = resolve_orchestration_plan(
            Some(TaskComplexity::High),
            false,
            true,
            &no_budgets(),
            &unit(),
            &demand,
            &backstops(),
            &cfg_on(),
        );
        assert_eq!(plan.council_figures, 6);
        assert_eq!(plan.review_panel_size, 4);
        assert_eq!(plan.multi_provider_providers, 3);
        assert_eq!(plan.debate_advocates, 4);
        assert_eq!(plan.sized_by, SizingConstraint::Backstop);
    }

    #[test]
    fn scope_system_wide_alza_council_e_provider_di_uno() {
        let demand = PanelDemand {
            council_figures: 3,
            reviewers: 1,
            providers: 2,
            advocates: 0,
        };
        let plan = resolve_orchestration_plan(
            Some(TaskComplexity::Medium),
            true,
            false,
            &no_budgets(),
            &unit(),
            &demand,
            &backstops(),
            &cfg_on(),
        );
        assert_eq!(plan.council_figures, 4);
        assert_eq!(plan.multi_provider_providers, 3);
        // providers=0 nel profilo resta 0 anche col bump (panel escluso dall'admin).
        let demand_no_mp = PanelDemand {
            providers: 0,
            ..demand
        };
        let plan2 = resolve_orchestration_plan(
            Some(TaskComplexity::Medium),
            true,
            false,
            &no_budgets(),
            &unit(),
            &demand_no_mp,
            &backstops(),
            &cfg_on(),
        );
        assert_eq!(plan2.multi_provider_providers, 0);
    }

    #[test]
    fn stima_unitaria_degenere_non_vincola() {
        let budgets = OrchestrationBudgets {
            cost_remaining_usd: Some(0.01),
            time_remaining_s: Some(1),
        };
        let degenerate = PanelUnitEstimate {
            cost_usd: 0.0,
            duration_s: 0,
        };
        let plan = resolve_orchestration_plan(
            Some(TaskComplexity::High),
            false,
            true,
            &budgets,
            &degenerate,
            &demand_high(),
            &backstops(),
            &cfg_on(),
        );
        // Stime non calcolabili -> nessun vincolo di budget applicabile.
        assert_eq!(plan.sized_by, SizingConstraint::Complexity);
        assert_eq!(plan.council_figures, 5);
    }

    #[test]
    fn parse_priorita_ignora_malformati_e_accoda_i_mancanti() {
        let p = parse_panel_priority("review, debate, banana, review");
        assert_eq!(
            p,
            vec![
                PanelKind::Review,
                PanelKind::Debate,
                PanelKind::Council,
                PanelKind::MultiProvider
            ]
        );
        assert_eq!(parse_panel_priority(""), DEFAULT_PANEL_PRIORITY.to_vec());
    }

    #[test]
    fn parse_complexity_canonico() {
        assert_eq!(TaskComplexity::try_parse(" High "), Some(TaskComplexity::High));
        assert_eq!(TaskComplexity::try_parse("MEDIUM"), Some(TaskComplexity::Medium));
        assert_eq!(TaskComplexity::try_parse("bassa"), None);
        assert_eq!(TaskComplexity::try_parse(""), None);
    }

    #[test]
    fn fanout_parallelism_non_supera_il_totale_pianificato() {
        let demand = PanelDemand {
            council_figures: 2,
            reviewers: 1,
            providers: 0,
            advocates: 0,
        };
        let plan = resolve_orchestration_plan(
            Some(TaskComplexity::Medium),
            false,
            false,
            &no_budgets(),
            &unit(),
            &demand,
            &backstops(),
            &cfg_on(),
        );
        assert_eq!(plan.fanout_parallelism, 3);
    }
}
