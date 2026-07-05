//! `scale_reason`: parte PURA (regola L) dello SCALE-CONTROLLER bidirezionale
//! (up/down del tier modello). GEMELLO di [`crate::decisions::meta_reason`] e
//! [`crate::decisions::orchestration_reason`] (stesso stile, golden-abile in
//! isolamento), su tipi DISGIUNTI: nessun tipo condiviso, nessun enum wrapper
//! (regola L, design SCALE-CONTROLLER §4).
//!
//! Il ragionamento LLM contestuale (SE e A CHE tier salire/scendere) vive dietro
//! il metodo [`crate::runtime::ports::MetaReasonerPort::assess_scale`]. Qui sta
//! SOLO la logica deterministica (regola M: nessuna prosa, solo segnali
//! strutturati). L'LLM sceglie SOLO tier + confidence; i 5 gate di
//! [`apply_hysteresis`] applicano l'anti-oscillazione DOPO la validazione —
//! l'LLM NON li scavalca:
//!   - [`build_scale_context`]: costruzione DETERMINISTICA del
//!     [`ScaleContext`] dai segnali gia' risolti dall'executor (FIX-A:
//!     `current_tier` letto dallo stato checkpointato, fallback deterministico
//!     [`ScaleTier::Medium`] se assente — default catalog mig 0032; ZERO I/O).
//!   - [`scale_cache_key`]: TUPLE STRUTTURATA serializzata (FIX-C: NON un intero
//!     sommato — due stati diversi non collidono mai; `error_ratchet` MONOTONO).
//!   - [`validate_scale_move`]: PUNTO UNICO di validazione dell'output JSON
//!     dell'LLM contro l'enum CHIUSO [`ScaleMove`] (tier fuori vocabolario /
//!     confidence fuori `[0,1]` / enum sconosciuto -> [`ScaleMove::KeepTier`]).
//!   - [`apply_hysteresis`]: 5 gate deterministici (confidenza, banda-morta
//!     asimmetrica, cooldown, clamp 1 gradino, reversal-pin FIX-D).
//!   - [`context_window_ok`]: gate hard finestra (FIX-B, predicato puro).
//!   - [`scale_trigger`]: QUANDO valutare (OR di segnali strutturati + gate
//!     break-even + precedenza stallo FIX-E).
//!
//! PR-A: nessun nodo/detector chiama ancora queste funzioni (quello e' PR-B).
//! Con `agent.scale.enabled=false` (default) restano inerti -> bit-identico.

use crate::decisions::context_reduction::{CtxMgmtConfig, TokenBrakeConfig};
use crate::runtime::ports::{
    ContextPressure, ScaleContext, ScaleMove, ScaleTier, SizingOverrides, SizingPosture,
};

/// Configurazione DB-driven dell'anti-oscillazione (regola G: le soglie arrivano
/// come parametro esplicito dai settings `agent.scale.*`, mai hardcoded qui). La
/// funzione resta PURA e testabile. I default in test/golden replicano i seed
/// conservativi della mig 0516 (documentati sul campo), MA a runtime i valori
/// sono SEMPRE letti dal DB dal call site mcp-core (PR-B): questa struct e' solo
/// il trasporto.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScaleHysteresisConfig {
    /// `agent.scale.downscale_enabled`: se `false`, ogni `DownscaleTo` degrada a
    /// `KeepTier` (rollout: prima solo up-consolidation).
    pub downscale_enabled: bool,
    /// `agent.scale.min_confidence` (default 0.70): soglia sotto cui KeepTier.
    pub min_confidence: f64,
    /// `agent.scale.change_cooldown_turns` (default 2): sotto -> KeepTier.
    pub change_cooldown_turns: i64,
    /// `agent.scale.downscale_clean_window` (default 3): streak pulita richiesta
    /// per il downscale (banda-morta asimmetrica).
    pub downscale_clean_window: i64,
    /// `agent.scale.max_reversals` (default 2): oltre -> pin al tier PIU' ALTO.
    pub max_reversals: i64,
    /// `agent.scale.window_overhead_ratio` (default 1.3): overhead per il vincolo
    /// finestra nel downscale (FIX-B).
    pub window_overhead_ratio: f64,
}

impl ScaleHysteresisConfig {
    /// Config con i seed conservativi della mig 0516 (SOLO per test/golden e come
    /// riferimento leggibile; a runtime i valori vengono dal DB, regola G).
    pub fn conservative_defaults() -> Self {
        Self {
            downscale_enabled: false,
            min_confidence: 0.70,
            change_cooldown_turns: 2,
            downscale_clean_window: 3,
            max_reversals: 2,
            window_overhead_ratio: 1.3,
        }
    }
}

/// Configurazione DB-driven del TRIGGER (regola G: settings `agent.scale.*`). PURA.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaleTriggerConfig {
    /// `agent.scale.enabled`: kill-switch. `false` -> nessun trigger (bit-identico).
    pub enabled: bool,
    /// `agent.scale.eval_every_iters` (default 4): cadenza di valutazione.
    pub eval_every_iters: i64,
    /// `agent.scale.min_tail_iters` (default 6): gate break-even. Sotto -> niente
    /// trigger (costo netto zero su run corti, FIX costo).
    pub min_tail_iters: i64,
}

impl ScaleTriggerConfig {
    /// Config con i seed conservativi (OFF) della mig 0516 (test/golden + riferimento).
    pub fn conservative_defaults() -> Self {
        Self {
            enabled: false,
            eval_every_iters: 4,
            min_tail_iters: 6,
        }
    }
}

/// Etichetta della banda di pressione contesto per la chiave-cache (segnale
/// stabile: `low`/`medium`/`high`). Punto unico locale del mapping enum->stringa.
fn pressure_band(p: ContextPressure) -> &'static str {
    match p {
        ContextPressure::Low => "low",
        ContextPressure::Medium => "medium",
        ContextPressure::High => "high",
    }
}

/// Costruisce il [`ScaleContext`] DETERMINISTICAMENTE dai segnali gia' risolti
/// dall'executor (regola M: tutti strutturati; FIX-A: ZERO I/O — `current_tier`
/// arriva dallo stato checkpointato come stringa opaca, gli altri campi come
/// segnali). `current_tier_raw` = `AgentState.current_tier`: se `None`/malformato
/// -> fallback DETERMINISTICO [`ScaleTier::Medium`] (default catalog mig 0032),
/// NON un lookup DB a volo (romperebbe il determinismo di replay).
///
/// `tail_headroom` e `error_free_streak` sono derivati qui in modo puro (max con 0
/// per input sporco, mai underflow). Gli altri sono passati gia' risolti a monte.
#[allow(clippy::too_many_arguments)]
pub fn build_scale_context(
    current_tier_raw: Option<&str>,
    intent_tier_floor: ScaleTier,
    user_intent: Option<&str>,
    behavior_mode: &str,
    iterations: i64,
    iteration_cap: i64,
    task_complexity_est: i64,
    task_critical: bool,
    context_pressure: ContextPressure,
    est_tokens: i64,
    token_headroom_ratio: f64,
    files_modified_delta: i64,
    todos_closed: i64,
    error_count: i64,
    error_free_streak: i64,
    repeated_action_failed: bool,
    escalations_done: i64,
    escalation_lock_active: bool,
    cost_spent_usd: f64,
    cost_cap_usd: f64,
    required_capability: Option<&str>,
    requires_tool_use: bool,
    turns_since_change: i64,
    reversal_count: i64,
) -> ScaleContext {
    // FIX-A: current_tier dallo stato checkpointato; fallback deterministico Medium
    // (default catalog) se assente/malformato. Nessun DB qui.
    let current_tier = current_tier_raw
        .and_then(ScaleTier::parse)
        .unwrap_or_default();
    let tail_headroom = (iteration_cap - iterations).max(0);
    ScaleContext {
        current_tier,
        intent_tier_floor,
        user_intent: user_intent.map(str::to_string),
        behavior_mode: behavior_mode.to_string(),
        iterations: iterations.max(0),
        iteration_cap: iteration_cap.max(0),
        tail_headroom,
        task_complexity_est: task_complexity_est.max(0),
        task_critical,
        context_pressure,
        est_tokens: est_tokens.max(0),
        token_headroom_ratio,
        files_modified_delta: files_modified_delta.max(0),
        todos_closed: todos_closed.max(0),
        error_count: error_count.max(0),
        error_free_streak: error_free_streak.max(0),
        repeated_action_failed,
        escalations_done: escalations_done.max(0),
        escalation_lock_active,
        cost_spent_usd,
        cost_cap_usd,
        required_capability: required_capability.map(str::to_string),
        requires_tool_use,
        turns_since_change: turns_since_change.max(0),
        reversal_count: reversal_count.max(0),
        // Segnali di SIZING (mig 0524): NON popolati qui (regola L: il costruttore
        // della decisione TIER non li conosce). Il detector li valorizza
        // post-costruzione SOLO quando `sizing_enabled`; a sizing OFF restano `None`
        // -> omessi dal JSON serializzato all'LLM (bit-identico per il flusso tier).
        ..Default::default()
    }
}

/// Chiave-cache dello scale-move come TUPLE STRUTTURATA serializzata (FIX-C: NON un
/// intero sommato — sommare un ordinale REVERSIBILE come `pressure_band` con
/// contatori monotoni collide, e due stati diversi finirebbero sulla stessa chiave
/// riusando una mossa stale). Qui ogni segnale-trigger e' un CAMPO SEPARATO nella
/// stringa: due stati diversi non collidono MAI, una mossa e' riusata SOLO se TUTTI
/// i segnali sono identici. `error_ratchet` (=`error_count`, MONOTONO a cricchetto)
/// e' incluso come livello-di-difficolta' (ogni livello valutato una volta sola).
///
/// Forma: `scale::{floor(iters/eval_every)}::{todos_closed}::{pressure_band}::{escalations}::{error_ratchet}`
pub fn scale_cache_key(ctx: &ScaleContext, eval_every_iters: i64) -> String {
    let n = eval_every_iters.max(1);
    let iters_bucket = ctx.iterations.max(0) / n;
    format!(
        "scale::{}::{}::{}::{}::{}",
        iters_bucket,
        ctx.todos_closed,
        pressure_band(ctx.context_pressure),
        ctx.escalations_done,
        // error_ratchet MONOTONO: usiamo error_count (accumulato, non decresce nel
        // costruttore) come livello di difficolta'.
        ctx.error_count,
    )
}

/// Valida l'output JSON dell'LLM contro l'enum CHIUSO [`ScaleMove`]. Qualunque
/// forma non deserializzabile, tier fuori `{light,medium,heavy}`, o `confidence`
/// fuori `[0,1]` (o non finita) degrada a [`ScaleMove::KeepTier`] (rete di
/// sicurezza). Punto unico (regola L): il nodo e l'impl della porta chiamano SOLO
/// questa funzione.
pub fn validate_scale_move(raw: &serde_json::Value) -> ScaleMove {
    let mv: ScaleMove = match serde_json::from_value(raw.clone()) {
        Ok(m) => m,
        Err(_) => return ScaleMove::KeepTier,
    };
    let conf_ok = |c: f64| c.is_finite() && (0.0..=1.0).contains(&c);
    match &mv {
        // ScaleTier / SizingPosture deserializzano gia' l'enum CHIUSO: un valore fuori
        // vocabolario non deserializza (ramo Err sopra). Qui resta da validare solo la
        // confidence (comune a upscale/downscale/adjust_sizing).
        ScaleMove::UpscaleTo { confidence, .. }
        | ScaleMove::DownscaleTo { confidence, .. }
        | ScaleMove::AdjustSizing { confidence, .. }
            if !conf_ok(*confidence) =>
        {
            ScaleMove::KeepTier
        }
        _ => mv,
    }
}

/// Gate HARD finestra (FIX-B, predicato PURO): il tier target ha finestra
/// sufficiente per il contesto corrente. Qui NON si risolve il modello (quello e'
/// a valle in PR-B via `select_agentic_model` con `min_context_window`): si valuta
/// il predicato sui segnali gia' presenti nel contesto. `est_tokens * overhead` e'
/// il fabbisogno; `window_hint` e' la finestra minima attesa per il tier target
/// (segnale gia' risolto a monte, `0` = ignoto -> gate NON superato, fail-safe).
///
/// Ritorna `true` se la finestra ipotizzata copre il fabbisogno. In PR-A e' usato
/// dal golden per fissare il contratto; il consumo reale nel downscale e' PR-B.
pub fn context_window_ok(window_hint: i64, est_tokens: i64, overhead_ratio: f64) -> bool {
    if window_hint <= 0 {
        // Finestra ignota: fail-safe (non autorizzare il downscale al buio).
        return false;
    }
    let overhead = if overhead_ratio.is_finite() && overhead_ratio >= 1.0 {
        overhead_ratio
    } else {
        // Overhead malformato: usa 1.0 (nessuno sconto), conservativo.
        1.0
    };
    let required = (est_tokens.max(0) as f64 * overhead).ceil() as i64;
    window_hint >= required
}

/// Il tier `target` e' il tier PIU' ALTO fra `a`/`b` (per il reversal-pin: al
/// raggiungimento delle inversioni si pinna verso l'alto, safety-biased).
fn higher_tier(a: ScaleTier, b: ScaleTier) -> ScaleTier {
    if a.rank() >= b.rank() {
        a
    } else {
        b
    }
}

/// Clampa il tier target a UN SOLO gradino dal corrente (gate 4): sulla scala a 5
/// livelli (light<medium<high<heavy<frontier) mai un salto diretto di 2+ gradini in
/// una sola epoca. Ritorna il target se e' gia' adiacente, altrimenti AVANZA/ARRETRA
/// di un solo gradino verso di esso (punto unico [`ScaleTier::from_rank`]).
fn clamp_one_step(current: ScaleTier, target: ScaleTier) -> ScaleTier {
    let (c, t) = (current.rank(), target.rank());
    if (t - c).abs() <= 1 {
        return target;
    }
    // Salto multi-gradino: muovi di UN solo gradino verso il target.
    let stepped = if t > c { c + 1 } else { c - 1 };
    ScaleTier::from_rank(stepped)
}

/// Applica i 5 gate deterministici dell'anti-oscillazione DOPO [`validate_scale_move`]
/// (l'LLM NON scavalca il gate). Ordine:
///   1. **Confidenza**: `confidence < min_confidence` -> `KeepTier`.
///   2. **Banda-morta ASIMMETRICA**: upscale facile (basta il segnale LLM valido);
///      downscale SOLO se `context_pressure==Low` STRETTO AND `error_free_streak >=
///      downscale_clean_window` AND zero escalation AND `files_modified_delta > 0`
///      AND `todos_closed > 0` (progresso reale definito come segnale esplicito,
///      FIX-D) AND `!repeated_action_failed` AND `!escalation_lock_active` (FIX-E)
///      AND `downscale_enabled`. Il downscale non scende mai sotto `intent_tier_floor`.
///   3. **Cooldown cambio-tier**: `turns_since_change < change_cooldown_turns` -> `KeepTier`.
///   4. **Clamp 1 gradino per epoca**: `light<->medium<->heavy`, mai salto diretto.
///   5. **Reversal-pin (FIX-D)**: `reversal_count >= max_reversals` -> pin al tier
///      PIU' ALTO fra corrente e target e stop (equivale a `already_guided`).
///
/// Ritorna sempre una [`ScaleMove`] (l'`KeepTier` e' la rete di sicurezza). Un
/// `UpscaleTo`/`DownscaleTo` che dopo i gate coincide col tier corrente degrada a
/// `KeepTier` (no-op esplicito).
pub fn apply_hysteresis(mv: ScaleMove, ctx: &ScaleContext, cfg: &ScaleHysteresisConfig) -> ScaleMove {
    // Estrai tier target + confidence + direzione; KeepTier passa dritto.
    let (target, confidence, is_down) = match &mv {
        ScaleMove::KeepTier => return ScaleMove::KeepTier,
        ScaleMove::UpscaleTo { tier, confidence } => (*tier, *confidence, false),
        ScaleMove::DownscaleTo { tier, confidence } => (*tier, *confidence, true),
        // Il sizing NON passa dai gate TIER: se una AdjustSizing raggiunge questo gate
        // (chiamata difensiva) e' un no-op tier -> KeepTier. Il gate dedicato del
        // sizing e' [`apply_sizing_gate`] (il nodo instrada per variante, regola L).
        ScaleMove::AdjustSizing { .. } => return ScaleMove::KeepTier,
    };

    // GATE 1 — confidenza.
    if !(confidence.is_finite() && confidence >= cfg.min_confidence) {
        return ScaleMove::KeepTier;
    }

    // GATE 5a — reversal-pin: se gia' oscillato troppo, PIN al tier piu' alto fra
    // corrente e target e stop (nessun ulteriore movimento su quell'asse).
    if ctx.reversal_count >= cfg.max_reversals {
        let pinned = higher_tier(ctx.current_tier, target);
        if pinned == ctx.current_tier {
            return ScaleMove::KeepTier;
        }
        // Il pin puo' solo SALIRE (safety-biased): emetti un upscale al tier pinnato,
        // clampato a 1 gradino.
        let clamped = clamp_one_step(ctx.current_tier, pinned);
        if clamped == ctx.current_tier {
            return ScaleMove::KeepTier;
        }
        return ScaleMove::UpscaleTo {
            tier: clamped,
            confidence,
        };
    }

    // GATE 2 — banda-morta asimmetrica.
    if is_down {
        // Downscale: gate stretti (FIX-D/FIX-E) + kill-switch dedicato.
        let clean = cfg.downscale_enabled
            && ctx.context_pressure == ContextPressure::Low
            && ctx.error_free_streak >= cfg.downscale_clean_window
            && ctx.escalations_done == 0
            && !ctx.escalation_lock_active
            && !ctx.repeated_action_failed
            && ctx.files_modified_delta > 0
            && ctx.todos_closed > 0;
        if !clean {
            return ScaleMove::KeepTier;
        }
        // FIX-D: mai sotto il floor per intent. Se il target scende sotto il floor,
        // clampa al floor (se il floor coincide col corrente -> KeepTier).
        let floored = if target.rank() < ctx.intent_tier_floor.rank() {
            ctx.intent_tier_floor
        } else {
            target
        };
        if floored.rank() >= ctx.current_tier.rank() {
            // Dopo il floor non e' piu' un downscale reale -> no-op.
            return ScaleMove::KeepTier;
        }
        // GATE 3 — cooldown.
        if ctx.turns_since_change < cfg.change_cooldown_turns {
            return ScaleMove::KeepTier;
        }
        // GATE 4 — clamp 1 gradino.
        let clamped = clamp_one_step(ctx.current_tier, floored);
        if clamped.rank() >= ctx.current_tier.rank() {
            return ScaleMove::KeepTier;
        }
        ScaleMove::DownscaleTo {
            tier: clamped,
            confidence,
        }
    } else {
        // Upscale: banda-morta permissiva (basta il segnale LLM valido).
        if target.rank() <= ctx.current_tier.rank() {
            // Non e' un upscale reale.
            return ScaleMove::KeepTier;
        }
        // GATE 3 — cooldown.
        if ctx.turns_since_change < cfg.change_cooldown_turns {
            return ScaleMove::KeepTier;
        }
        // GATE 4 — clamp 1 gradino.
        let clamped = clamp_one_step(ctx.current_tier, target);
        if clamped.rank() <= ctx.current_tier.rank() {
            return ScaleMove::KeepTier;
        }
        ScaleMove::UpscaleTo {
            tier: clamped,
            confidence,
        }
    }
}

/// Decide se VALUTARE lo scale-controller questo turno (QUANDO consultare l'LLM).
/// OR di segnali strutturati (regola M), MA sotto i gate hard:
///   - **Break-even (FIX costo)**: `enabled` AND `tail_headroom >= min_tail_iters`.
///   - **Precedenza stallo-vince (FIX-E)**: se `stall_active` -> MAI trigger (lo
///     stallo reattivo ha priorita' sul risparmio pre-emptivo).
///
/// Se i gate hard passano, il trigger scatta su OR di: cadenza
/// (`iterations % eval_every == 0` con iters>0), cambio-banda pressione
/// (`pressure_changed`), chiusura todo (`todo_closed_now`), salto-errori a cricchetto
/// (`error_ratchet_advanced`), salto escalation (`escalation_advanced`).
#[allow(clippy::too_many_arguments)]
pub fn scale_trigger(
    ctx: &ScaleContext,
    cfg: &ScaleTriggerConfig,
    stall_active: bool,
    pressure_changed: bool,
    todo_closed_now: bool,
    error_ratchet_advanced: bool,
    escalation_advanced: bool,
) -> bool {
    // Gate hard: kill-switch + break-even + precedenza stallo.
    if !cfg.enabled {
        return false;
    }
    if stall_active {
        return false;
    }
    if ctx.tail_headroom < cfg.min_tail_iters {
        return false;
    }
    let n = cfg.eval_every_iters.max(1);
    let cadence = ctx.iterations > 0 && ctx.iterations % n == 0;
    cadence
        || pressure_changed
        || todo_closed_now
        || error_ratchet_advanced
        || escalation_advanced
}

// ────────────────────────────────────────────────────────────────────────────
//  SIZING AGENTICO (mig 0524): terza direzione dello scale-controller.
//
//  Rende ADATTIVE (non piu' soglie fisse geometriche) le decisioni di
//  DIMENSIONAMENTO del motore: compress phases/keep_recent/max_chars +
//  compress_start_iter, token_brake ratio, rolling_summary on/off + keep_recent,
//  g1_loop_threshold. Stesso pattern del tier: l'LLM sceglie SOLO una POSTURA
//  bounded (Compact/Relax/Hold) + confidence (regola M); la traduzione in soglie
//  CONCRETE e' DETERMINISTICA e PROPORZIONALE ai segnali (durata run, crescita
//  history, rumore tool_result, progresso). Gate dedicato (kill-switch nested +
//  confidenza + cooldown anti-thrash) separato dai 5 gate TIER.
//
//  Retro-compat: con `sizing_enabled=false` (default) il gate degrada ogni
//  AdjustSizing a `KeepTier` e i consumatori (`effective_*` con override assenti)
//  lasciano invariate le soglie fisse -> BIT-IDENTICO.
// ────────────────────────────────────────────────────────────────────────────

// Bound di sicurezza del sizing: INVARIANTI dell'algoritmo (non tunabili per
// deployment; la sola manopola DB e' `aggressiveness`, regola G). Impediscono che
// una postura porti una soglia fuori da un intervallo sano (mai keep_recent 0,
// mai freno oltre 0.90, ecc.). Sono i CLAMP del trasformatore bounded, non
// "magic value" di business.
const SIZING_START_ITER_FLOOR: i64 = 1;
const SIZING_KEEP_RECENT_FLOOR: i64 = 1;
const SIZING_KEEP_RECENT_CEIL: i64 = 40;
const SIZING_MAX_CHARS_FLOOR: i64 = 100;
const SIZING_MAX_CHARS_CEIL: i64 = 8000;
const SIZING_BRAKE_RATIO_MIN: f64 = 0.50;
const SIZING_BRAKE_RATIO_MAX: f64 = 0.90;
const SIZING_G1_MULT_MIN: f64 = 0.5;
const SIZING_G1_MULT_MAX: f64 = 2.0;

/// Config DB-driven del SIZING (regola G: soglie dal DB, mai hardcoded). Trasportata
/// dal produttore executor al nodo/consumatore via `extra` (come
/// [`ScaleHysteresisConfig`]). PURA e testabile. `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScaleSizingConfig {
    /// `agent.scale.sizing_enabled`: kill-switch NESTED del sizing. `false` (default)
    /// -> ogni `AdjustSizing` degrada a `KeepTier` (bit-identico anche con scale ON).
    pub enabled: bool,
    /// Soglia di confidenza LLM sotto cui la postura degrada a `KeepTier` (riuso di
    /// `agent.scale.min_confidence`, regola L: una sola soglia trasportata a due gate).
    pub min_confidence: f64,
    /// `agent.scale.sizing_cooldown_turns`: turni minimi tra due cambi di postura
    /// (anti-thrash del sizing, DISTINTO dal cooldown tier).
    pub cooldown_turns: i64,
    /// `agent.scale.sizing_aggressiveness` in `[0,1]`: quanto una postura spinge le
    /// soglie (0 = quasi neutro, 1 = spinta massima entro i bound). E' l'UNICA
    /// manopola DB del trasformatore; i floor/ceil sono invarianti.
    pub aggressiveness: f64,
}

impl ScaleSizingConfig {
    /// Config con i seed conservativi della mig 0524 (sizing OFF). Solo per
    /// test/golden e riferimento; a runtime i valori vengono dal DB (regola G).
    pub fn conservative_defaults() -> Self {
        Self {
            enabled: false,
            min_confidence: 0.70,
            cooldown_turns: 3,
            aggressiveness: 0.5,
        }
    }
}

/// Soglie FISSE correnti da cui il risolutore calcola gli override (le legge il
/// produttore executor da `ExecutorConfig`, regola L: il calcolo puro non tocca il
/// DB). Serve per esprimere gli override come delta clampati rispetto alla base.
#[derive(Debug, Clone, PartialEq)]
pub struct SizingBaseline {
    /// `compress_start_iter` fisso.
    pub compress_start_iter: i64,
    /// `compress_phase_keep_recent` fisso (per fase).
    pub compress_phase_keep_recent: Vec<i64>,
    /// `compress_phase_max_chars` fisso (per fase).
    pub compress_phase_max_chars: Vec<i64>,
    /// `token_brake.max_context_ratio` fisso.
    pub token_brake_max_context_ratio: f64,
    /// `rolling_keep_recent` fisso.
    pub rolling_keep_recent: i64,
}

/// Gate DEDICATO del sizing (gemello di [`apply_hysteresis`] per il tier, su
/// direzione DISGIUNTA). Applica DOPO [`validate_scale_move`]:
///   0. **Kill-switch nested** (`sizing_enabled`): OFF -> `KeepTier` (bit-identico).
///   1. **Hold** -> `KeepTier` (no-op esplicito).
///   2. **Confidenza**: `< min_confidence` -> `KeepTier`.
///   3. **Cooldown anti-thrash**: `sizing_turns_since_change < cooldown_turns` ->
///      `KeepTier` (il segnale assente = primo cambio, non blocca).
///
/// Ogni forma non-`AdjustSizing` passa invariata (il nodo instrada i tier ad
/// `apply_hysteresis`; qui e' difensivo). L'LLM NON scavalca il gate.
pub fn apply_sizing_gate(mv: ScaleMove, ctx: &ScaleContext, cfg: &ScaleSizingConfig) -> ScaleMove {
    let (posture, confidence) = match &mv {
        ScaleMove::AdjustSizing { posture, confidence } => (*posture, *confidence),
        other => return other.clone(),
    };
    // GATE 0 — kill-switch nested del sizing.
    if !cfg.enabled {
        return ScaleMove::KeepTier;
    }
    // GATE 1 — Hold = no-op esplicito.
    if posture == SizingPosture::Hold {
        return ScaleMove::KeepTier;
    }
    // GATE 2 — confidenza.
    if !(confidence.is_finite() && confidence >= cfg.min_confidence) {
        return ScaleMove::KeepTier;
    }
    // GATE 3 — cooldown anti-thrash del sizing. Segnale assente (None) = valore ampio
    // -> non blocca (primo cambio di postura del run).
    let since = ctx.sizing_turns_since_change.unwrap_or(i64::MAX);
    if since < cfg.cooldown_turns {
        return ScaleMove::KeepTier;
    }
    ScaleMove::AdjustSizing { posture, confidence }
}

/// Aggressivita' EFFETTIVA: la manopola DB `aggressiveness` amplificata dalla
/// crescita della history (piu' il contesto cresce in fretta, piu' spingiamo),
/// clampata `[0,1]`. Regola M: deriva da segnali strutturati, mai da prosa.
fn effective_aggressiveness(cfg_aggr: f64, ctx: &ScaleContext) -> f64 {
    let a = if cfg_aggr.is_finite() {
        cfg_aggr.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let growth = ctx
        .history_growth_rate
        .filter(|g| g.is_finite())
        .unwrap_or(0.0)
        .max(0.0);
    // Crescita tipica 1-4 msg/iter: boost fino a +0.3 a crescita >= 4.
    let boost = (growth / 4.0).clamp(0.0, 1.0) * 0.3;
    (a + boost).clamp(0.0, 1.0)
}

/// Fattore di rumore storia `[0,1]` dal piu' grande tool_result recente (proxy della
/// `tool_result_size_distribution`): piu' e' rumorosa, piu' stringiamo `max_chars`.
fn noise_factor(ctx: &ScaleContext) -> f64 {
    let n = ctx.tool_result_noise.unwrap_or(0).max(0) as f64;
    (n / 8000.0).clamp(0.0, 1.0)
}

/// Risolve la [`SizingPosture`] scelta dall'LLM in [`SizingOverrides`] CONCRETI,
/// deterministici e PROPORZIONALI ai segnali (durata/crescita/rumore/progresso),
/// clampati ai bound di sicurezza. PUNTO UNICO (regola L): il nodo/executor delega
/// qui, non ricalcola. `Hold` -> nessun override (bit-identico).
pub fn resolve_sizing_overrides(
    posture: SizingPosture,
    ctx: &ScaleContext,
    base: &SizingBaseline,
    cfg: &ScaleSizingConfig,
) -> SizingOverrides {
    let a = effective_aggressiveness(cfg.aggressiveness, ctx);
    let progressing = ctx.recent_productive.unwrap_or(false);
    match posture {
        SizingPosture::Hold => SizingOverrides::default(),
        SizingPosture::Compact => compact_overrides(ctx, base, a, progressing),
        SizingPosture::Relax => relax_overrides(base, a, progressing),
    }
}

/// COMPACT: contesto sotto pressione / storia rumorosa -> comprimi prima e piu'
/// forte, freno piu' aggressivo, rolling-summary ON; g1-loop con piu' respiro SOLO
/// se il run progredisce (non escalare a vuoto un modello che avanza).
fn compact_overrides(
    ctx: &ScaleContext,
    base: &SizingBaseline,
    a: f64,
    progressing: bool,
) -> SizingOverrides {
    let noise = noise_factor(ctx);
    let start = ((base.compress_start_iter as f64) * (1.0 - 0.5 * a)).round() as i64;
    let keep = base
        .compress_phase_keep_recent
        .iter()
        .map(|x| ((*x as f64) * (1.0 - 0.3 * a)).round() as i64)
        .map(|x| x.clamp(SIZING_KEEP_RECENT_FLOOR, SIZING_KEEP_RECENT_CEIL))
        .collect::<Vec<_>>();
    // Piu' rumore -> cap del singolo tool_result piu' piccolo.
    let shrink = (0.3 * a + 0.4 * noise).min(0.85);
    let chars = base
        .compress_phase_max_chars
        .iter()
        .map(|x| ((*x as f64) * (1.0 - shrink)).round() as i64)
        .map(|x| x.clamp(SIZING_MAX_CHARS_FLOOR, SIZING_MAX_CHARS_CEIL))
        .collect::<Vec<_>>();
    let brake = (base.token_brake_max_context_ratio * (1.0 - 0.15 * a))
        .clamp(SIZING_BRAKE_RATIO_MIN, SIZING_BRAKE_RATIO_MAX);
    let g1 = if progressing {
        (1.0 + 0.5 * a).clamp(SIZING_G1_MULT_MIN, SIZING_G1_MULT_MAX)
    } else {
        1.0
    };
    SizingOverrides {
        compress_start_iter: Some(start.max(SIZING_START_ITER_FLOOR)),
        compress_phase_keep_recent: Some(keep),
        compress_phase_max_chars: Some(chars),
        token_brake_max_context_ratio: Some(brake),
        rolling_summary_enabled: Some(true),
        rolling_keep_recent: Some(base.rolling_keep_recent.max(2)),
        g1_loop_threshold_mult: Some(g1),
    }
}

/// RELAX: run che fila con finestra ampia -> rilassa la compressione (piu' contesto
/// vivo), rolling-summary OFF, freno piu' largo, g1-loop con piu' respiro.
fn relax_overrides(base: &SizingBaseline, a: f64, progressing: bool) -> SizingOverrides {
    let start = ((base.compress_start_iter as f64) * (1.0 + 0.5 * a)).round() as i64;
    let keep = base
        .compress_phase_keep_recent
        .iter()
        .map(|x| ((*x as f64) * (1.0 + 0.3 * a)).round() as i64)
        .map(|x| x.clamp(SIZING_KEEP_RECENT_FLOOR, SIZING_KEEP_RECENT_CEIL))
        .collect::<Vec<_>>();
    let chars = base
        .compress_phase_max_chars
        .iter()
        .map(|x| ((*x as f64) * (1.0 + 0.4 * a)).round() as i64)
        .map(|x| x.clamp(SIZING_MAX_CHARS_FLOOR, SIZING_MAX_CHARS_CEIL))
        .collect::<Vec<_>>();
    let brake = (base.token_brake_max_context_ratio * (1.0 + 0.15 * a))
        .clamp(SIZING_BRAKE_RATIO_MIN, SIZING_BRAKE_RATIO_MAX);
    let g1 = if progressing {
        (1.0 + 0.5 * a).clamp(SIZING_G1_MULT_MIN, SIZING_G1_MULT_MAX)
    } else {
        (1.0 + 0.25 * a).clamp(SIZING_G1_MULT_MIN, SIZING_G1_MULT_MAX)
    };
    SizingOverrides {
        compress_start_iter: Some(start.max(SIZING_START_ITER_FLOOR)),
        compress_phase_keep_recent: Some(keep),
        compress_phase_max_chars: Some(chars),
        token_brake_max_context_ratio: Some(brake),
        rolling_summary_enabled: Some(false),
        rolling_keep_recent: None,
        g1_loop_threshold_mult: Some(g1),
    }
}

// ── Consumazione: merge base-config + override (bit-identico se override assenti) ──

/// [`CtxMgmtConfig`] EFFETTIVO: base + eventuali override sizing. `ov = None` (sizing
/// OFF / nessuna postura applicata) -> clone della base INVARIATA (bit-identico).
/// PUNTO UNICO del merge (regola L): l'executor delega qui.
pub fn effective_ctx_mgmt(base: &CtxMgmtConfig, ov: Option<&SizingOverrides>) -> CtxMgmtConfig {
    let mut out = base.clone();
    if let Some(o) = ov {
        if let Some(s) = o.compress_start_iter {
            out.compress_start_iter = s;
        }
        if let Some(k) = &o.compress_phase_keep_recent {
            out.compress_phase_keep_recent = k.clone();
        }
        if let Some(c) = &o.compress_phase_max_chars {
            out.compress_phase_max_chars = c.clone();
        }
    }
    out
}

/// [`TokenBrakeConfig`] EFFETTIVO: base + eventuale override di `max_context_ratio`.
/// `ov = None` -> base invariata (bit-identico).
pub fn effective_token_brake(
    base: &TokenBrakeConfig,
    ov: Option<&SizingOverrides>,
) -> TokenBrakeConfig {
    let mut out = *base;
    if let Some(r) = ov.and_then(|o| o.token_brake_max_context_ratio) {
        out.max_context_ratio = r;
    }
    out
}

/// Rolling-summary EFFETTIVO `(enabled, keep_recent)`: base + eventuali override.
/// `ov = None` -> `(base_enabled, base_keep)` (bit-identico).
pub fn effective_rolling(
    base_enabled: bool,
    base_keep: i64,
    ov: Option<&SizingOverrides>,
) -> (bool, i64) {
    let mut enabled = base_enabled;
    let mut keep = base_keep;
    if let Some(o) = ov {
        if let Some(e) = o.rolling_summary_enabled {
            enabled = e;
        }
        if let Some(k) = o.rolling_keep_recent {
            keep = k;
        }
    }
    (enabled, keep)
}

/// Soglia g1-loop EFFETTIVA: `base * g1_loop_threshold_mult` (arrotondata). `ov =
/// None` o moltiplicatore assente/non valido -> `mult = 1.0` -> base invariata
/// (bit-identico). Rende adattiva la soglia di escalation-da-loop: piu' respiro a un
/// modello che progredisce, meno se la storia cresce a vuoto.
pub fn effective_g1_threshold(base: i64, ov: Option<&SizingOverrides>) -> i64 {
    let mult = ov.and_then(|o| o.g1_loop_threshold_mult).unwrap_or(1.0);
    let mult = if mult.is_finite() && mult > 0.0 {
        mult
    } else {
        1.0
    };
    ((base as f64) * mult).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn from_rank_e_clamp_a_5_tier() {
        use ScaleTier::*;
        // round-trip rank <-> from_rank sui 5 livelli
        for t in [Light, Medium, High, Heavy, Frontier] {
            assert_eq!(ScaleTier::from_rank(t.rank()), t);
        }
        // parse dei due nuovi tier
        assert_eq!(ScaleTier::parse("high"), Some(High));
        assert_eq!(ScaleTier::parse("frontier"), Some(Frontier));
        // clamp: un salto multi-gradino avanza/arretra di UN solo gradino
        assert_eq!(clamp_one_step(Light, Frontier), Medium); // +4 -> +1
        assert_eq!(clamp_one_step(Frontier, Light), Heavy); //  -4 -> -1
        assert_eq!(clamp_one_step(Medium, Heavy), High); //    +2 -> +1
        // tier adiacenti passano invariati
        assert_eq!(clamp_one_step(High, Heavy), Heavy);
        assert_eq!(clamp_one_step(Heavy, High), High);
    }

    fn base_ctx() -> ScaleContext {
        ScaleContext {
            current_tier: ScaleTier::Medium,
            intent_tier_floor: ScaleTier::Light,
            behavior_mode: "automatic".to_string(),
            iterations: 8,
            iteration_cap: 20,
            tail_headroom: 12,
            requires_tool_use: true,
            turns_since_change: 5,
            ..Default::default()
        }
    }

    // ── build_scale_context (FIX-A) ─────────────────────────────────────────────

    #[test]
    fn build_scale_context_fallback_medium_se_tier_assente() {
        // current_tier None -> Medium (default catalog, mai lookup DB).
        let ctx = build_scale_context(
            None,
            ScaleTier::Light,
            Some("code_write"),
            "automatic",
            5,
            20,
            3,
            false,
            ContextPressure::Low,
            1000,
            0.1,
            2,
            1,
            0,
            4,
            false,
            0,
            false,
            0.0,
            0.0,
            None,
            true,
            3,
            0,
        );
        assert_eq!(ctx.current_tier, ScaleTier::Medium, "None -> Medium deterministico");
        assert_eq!(ctx.tail_headroom, 15, "tail = cap - iters, mai negativo");
    }

    #[test]
    fn build_scale_context_tier_malformato_degrada_a_medium() {
        let ctx = build_scale_context(
            Some("gigante"),
            ScaleTier::Medium,
            None,
            "study",
            30,
            20, // iters > cap: tail deve essere 0, non negativo
            0,
            false,
            ContextPressure::High,
            0,
            0.0,
            0,
            0,
            0,
            0,
            false,
            0,
            false,
            0.0,
            0.0,
            None,
            true,
            0,
            0,
        );
        assert_eq!(ctx.current_tier, ScaleTier::Medium, "tier fuori vocabolario -> Medium");
        assert_eq!(ctx.tail_headroom, 0, "tail non underflowa");
    }

    // ── scale_cache_key (FIX-C: collisione evitata) ─────────────────────────────

    #[test]
    fn cache_key_stati_diversi_chiavi_diverse() {
        // Lo scenario di collisione additiva del design: A (pressure High) e B
        // (pressure Low + 1 errore) sommavano allo stesso intero. Con la tuple NON
        // collidono.
        let a = ScaleContext {
            iterations: 12,
            todos_closed: 1,
            context_pressure: ContextPressure::High,
            escalations_done: 0,
            error_count: 0,
            ..base_ctx()
        };
        let b = ScaleContext {
            iterations: 16,
            todos_closed: 1,
            context_pressure: ContextPressure::Low,
            escalations_done: 0,
            error_count: 1,
            ..base_ctx()
        };
        assert_ne!(
            scale_cache_key(&a, 4),
            scale_cache_key(&b, 4),
            "stati diversi NON devono collidere sulla stessa chiave (FIX-C)"
        );
    }

    #[test]
    fn cache_key_stesso_stato_stessa_chiave() {
        let a = base_ctx();
        let b = base_ctx();
        assert_eq!(scale_cache_key(&a, 4), scale_cache_key(&b, 4), "deterministica");
    }

    #[test]
    fn cache_key_error_ratchet_monotono_distingue_livelli() {
        let low = ScaleContext { error_count: 0, ..base_ctx() };
        let high = ScaleContext { error_count: 3, ..base_ctx() };
        assert_ne!(
            scale_cache_key(&low, 4),
            scale_cache_key(&high, 4),
            "livelli di difficolta' (error_ratchet) diversi -> chiavi diverse"
        );
    }

    // ── validate_scale_move ─────────────────────────────────────────────────────

    #[test]
    fn validate_forme_valide() {
        assert_eq!(
            validate_scale_move(&json!({"move": "keep_tier"})),
            ScaleMove::KeepTier
        );
        assert_eq!(
            validate_scale_move(&json!({"move": "upscale_to", "tier": "heavy", "confidence": 0.8})),
            ScaleMove::UpscaleTo { tier: ScaleTier::Heavy, confidence: 0.8 }
        );
        assert_eq!(
            validate_scale_move(&json!({"move": "downscale_to", "tier": "light", "confidence": 0.9})),
            ScaleMove::DownscaleTo { tier: ScaleTier::Light, confidence: 0.9 }
        );
    }

    #[test]
    fn validate_malformato_degrada_a_keep() {
        // Enum sconosciuto.
        assert_eq!(validate_scale_move(&json!({"move": "boh"})), ScaleMove::KeepTier);
        // Tier fuori vocabolario (non deserializza ScaleTier).
        assert_eq!(
            validate_scale_move(&json!({"move": "upscale_to", "tier": "titan", "confidence": 0.9})),
            ScaleMove::KeepTier
        );
        // Confidence fuori [0,1].
        assert_eq!(
            validate_scale_move(&json!({"move": "upscale_to", "tier": "heavy", "confidence": 1.5})),
            ScaleMove::KeepTier
        );
        // Confidence negativa.
        assert_eq!(
            validate_scale_move(&json!({"move": "downscale_to", "tier": "light", "confidence": -0.1})),
            ScaleMove::KeepTier
        );
    }

    // ── GATE 1: confidenza ──────────────────────────────────────────────────────

    #[test]
    fn gate_confidenza_sotto_soglia_keep() {
        let cfg = ScaleHysteresisConfig::conservative_defaults();
        let ctx = base_ctx();
        let mv = ScaleMove::UpscaleTo { tier: ScaleTier::Heavy, confidence: 0.5 };
        assert_eq!(apply_hysteresis(mv, &ctx, &cfg), ScaleMove::KeepTier);
    }

    #[test]
    fn gate_confidenza_sopra_soglia_upscale_passa() {
        let cfg = ScaleHysteresisConfig::conservative_defaults();
        let ctx = base_ctx();
        // Target adiacente (Medium->High) per isolare il gate confidenza dal gate
        // clamp (gate 4): un salto Medium->Heavy verrebbe clampato a High.
        let mv = ScaleMove::UpscaleTo { tier: ScaleTier::High, confidence: 0.9 };
        assert_eq!(
            apply_hysteresis(mv, &ctx, &cfg),
            ScaleMove::UpscaleTo { tier: ScaleTier::High, confidence: 0.9 }
        );
    }

    // ── GATE 2: banda-morta asimmetrica ─────────────────────────────────────────

    #[test]
    fn gate_banda_upscale_facile() {
        // Upscale con un solo segnale (confidence valida) passa, anche in banda sporca.
        let cfg = ScaleHysteresisConfig::conservative_defaults();
        let ctx = ScaleContext {
            context_pressure: ContextPressure::High,
            error_count: 5,
            ..base_ctx()
        };
        // Target adiacente (Medium->High) per isolare il gate banda dal gate clamp.
        let mv = ScaleMove::UpscaleTo { tier: ScaleTier::High, confidence: 0.75 };
        assert_eq!(
            apply_hysteresis(mv, &ctx, &cfg),
            ScaleMove::UpscaleTo { tier: ScaleTier::High, confidence: 0.75 }
        );
    }

    #[test]
    fn gate_banda_downscale_richiede_banda_stretta() {
        // downscale_enabled=false (default) -> KeepTier a prescindere.
        let cfg = ScaleHysteresisConfig::conservative_defaults();
        let ctx = ScaleContext {
            context_pressure: ContextPressure::Low,
            error_free_streak: 5,
            escalations_done: 0,
            files_modified_delta: 3,
            todos_closed: 2,
            ..base_ctx()
        };
        let mv = ScaleMove::DownscaleTo { tier: ScaleTier::Light, confidence: 0.95 };
        assert_eq!(apply_hysteresis(mv, &ctx, &cfg), ScaleMove::KeepTier, "downscale OFF");

        // Con downscale ON e banda pulita: passa (clampato a 1 gradino: medium->light).
        let cfg_on = ScaleHysteresisConfig { downscale_enabled: true, ..cfg };
        assert_eq!(
            apply_hysteresis(
                ScaleMove::DownscaleTo { tier: ScaleTier::Light, confidence: 0.95 },
                &ctx,
                &cfg_on
            ),
            ScaleMove::DownscaleTo { tier: ScaleTier::Light, confidence: 0.95 }
        );

        // Banda sporca (errore recente) -> KeepTier anche con downscale ON.
        let ctx_dirty = ScaleContext { error_free_streak: 0, ..ctx };
        assert_eq!(
            apply_hysteresis(
                ScaleMove::DownscaleTo { tier: ScaleTier::Light, confidence: 0.95 },
                &ctx_dirty,
                &cfg_on
            ),
            ScaleMove::KeepTier
        );
    }

    #[test]
    fn gate_banda_downscale_vietato_da_escalation_lock() {
        // FIX-E: escalation_lock_active -> mai downscale.
        let cfg = ScaleHysteresisConfig { downscale_enabled: true, ..ScaleHysteresisConfig::conservative_defaults() };
        let ctx = ScaleContext {
            context_pressure: ContextPressure::Low,
            error_free_streak: 5,
            escalations_done: 0,
            escalation_lock_active: true,
            files_modified_delta: 3,
            todos_closed: 2,
            ..base_ctx()
        };
        let mv = ScaleMove::DownscaleTo { tier: ScaleTier::Light, confidence: 0.95 };
        assert_eq!(apply_hysteresis(mv, &ctx, &cfg), ScaleMove::KeepTier);
    }

    // ── GATE 3: cooldown ────────────────────────────────────────────────────────

    #[test]
    fn gate_cooldown_blocca_cambio_recente() {
        let cfg = ScaleHysteresisConfig::conservative_defaults();
        let ctx = ScaleContext { turns_since_change: 1, ..base_ctx() };
        let mv = ScaleMove::UpscaleTo { tier: ScaleTier::Heavy, confidence: 0.9 };
        assert_eq!(apply_hysteresis(mv, &ctx, &cfg), ScaleMove::KeepTier);
    }

    // ── GATE 4: clamp 1 gradino ─────────────────────────────────────────────────

    #[test]
    fn gate_clamp_un_gradino_light_a_heavy() {
        // Da light chiedendo heavy (salto 2 gradini) -> clampa a medium.
        let cfg = ScaleHysteresisConfig::conservative_defaults();
        let ctx = ScaleContext { current_tier: ScaleTier::Light, ..base_ctx() };
        let mv = ScaleMove::UpscaleTo { tier: ScaleTier::Heavy, confidence: 0.9 };
        assert_eq!(
            apply_hysteresis(mv, &ctx, &cfg),
            ScaleMove::UpscaleTo { tier: ScaleTier::Medium, confidence: 0.9 }
        );
    }

    // ── GATE 5: reversal-pin (FIX-D) ────────────────────────────────────────────

    #[test]
    fn gate_reversal_pin_sale_al_piu_alto_dei_due() {
        // FIX-D: reversal_count >= max_reversals sulla coppia (current, target) ->
        // pin al tier PIU' ALTO DEI DUE (safety-biased). Coppia ADIACENTE (Medium,
        // High) per non innescare il gate clamp: richiesta UpscaleTo High -> pin a High.
        let cfg = ScaleHysteresisConfig::conservative_defaults();
        let ctx = ScaleContext {
            current_tier: ScaleTier::Medium,
            reversal_count: 2,
            ..base_ctx()
        };
        let mv = ScaleMove::UpscaleTo { tier: ScaleTier::High, confidence: 0.9 };
        assert_eq!(
            apply_hysteresis(mv, &ctx, &cfg),
            ScaleMove::UpscaleTo { tier: ScaleTier::High, confidence: 0.9 },
            "reversal-pin sale al tier piu' alto dei due, safety-biased"
        );
    }

    #[test]
    fn gate_reversal_pin_downscale_verso_basso_e_keep() {
        // Coppia (Medium, Light): il piu' alto dei due e' Medium == current -> il pin
        // NON scende (safety-biased) -> KeepTier. Un downscale richiesto sotto pin e'
        // soppresso: e' proprio l'anti-oscillazione (mai riscendere su un asse gia'
        // oscillato).
        let cfg = ScaleHysteresisConfig::conservative_defaults();
        let ctx = ScaleContext {
            current_tier: ScaleTier::Medium,
            reversal_count: 2,
            ..base_ctx()
        };
        let mv = ScaleMove::DownscaleTo { tier: ScaleTier::Light, confidence: 0.9 };
        assert_eq!(
            apply_hysteresis(mv, &ctx, &cfg),
            ScaleMove::KeepTier,
            "reversal-pin non scende mai (il piu' alto dei due e' il corrente)"
        );
    }

    #[test]
    fn gate_reversal_pin_gia_al_massimo_keep() {
        // Gia' su heavy col pin attivo: nulla piu' in alto -> KeepTier.
        let cfg = ScaleHysteresisConfig::conservative_defaults();
        let ctx = ScaleContext {
            current_tier: ScaleTier::Heavy,
            reversal_count: 3,
            ..base_ctx()
        };
        let mv = ScaleMove::DownscaleTo { tier: ScaleTier::Light, confidence: 0.9 };
        assert_eq!(apply_hysteresis(mv, &ctx, &cfg), ScaleMove::KeepTier);
    }

    // ── FIX-D: floor-tier ───────────────────────────────────────────────────────

    #[test]
    fn floor_tier_downscale_non_scende_sotto_floor() {
        // floor=medium: un downscale medium->light viene bloccato (floor==current).
        let cfg = ScaleHysteresisConfig { downscale_enabled: true, ..ScaleHysteresisConfig::conservative_defaults() };
        let ctx = ScaleContext {
            current_tier: ScaleTier::Medium,
            intent_tier_floor: ScaleTier::Medium,
            context_pressure: ContextPressure::Low,
            error_free_streak: 5,
            escalations_done: 0,
            files_modified_delta: 3,
            todos_closed: 2,
            ..base_ctx()
        };
        let mv = ScaleMove::DownscaleTo { tier: ScaleTier::Light, confidence: 0.95 };
        assert_eq!(
            apply_hysteresis(mv, &ctx, &cfg),
            ScaleMove::KeepTier,
            "il downscale non scende sotto il floor per intent (FIX-D)"
        );
    }

    // ── FIX-B: context_window_ok ────────────────────────────────────────────────

    #[test]
    fn window_ok_finestra_sufficiente() {
        // 8000 token * 1.3 = 10400; finestra 16000 basta.
        assert!(context_window_ok(16000, 8000, 1.3));
    }

    #[test]
    fn window_ok_finestra_insufficiente() {
        // 8000 * 1.3 = 10400; finestra 8192 NON basta.
        assert!(!context_window_ok(8192, 8000, 1.3));
    }

    #[test]
    fn window_ok_finestra_ignota_fail_safe() {
        assert!(!context_window_ok(0, 100, 1.3), "finestra ignota -> non autorizzare");
    }

    // ── scale_trigger (break-even + precedenza stallo) ──────────────────────────

    #[test]
    fn trigger_disabilitato_mai() {
        let cfg = ScaleTriggerConfig::conservative_defaults(); // enabled=false
        let ctx = base_ctx();
        assert!(!scale_trigger(&ctx, &cfg, false, true, true, true, true));
    }

    #[test]
    fn trigger_break_even_tail_corta_no() {
        let cfg = ScaleTriggerConfig { enabled: true, eval_every_iters: 4, min_tail_iters: 6 };
        let ctx = ScaleContext { tail_headroom: 3, ..base_ctx() };
        assert!(!scale_trigger(&ctx, &cfg, false, true, true, true, true), "coda corta -> no trigger");
    }

    #[test]
    fn trigger_stallo_vince() {
        // FIX-E: stallo attivo -> mai trigger, anche con cadenza pronta.
        let cfg = ScaleTriggerConfig { enabled: true, eval_every_iters: 4, min_tail_iters: 6 };
        let ctx = ScaleContext { iterations: 8, tail_headroom: 12, ..base_ctx() };
        assert!(!scale_trigger(&ctx, &cfg, true, false, false, false, false));
    }

    #[test]
    fn trigger_cadenza_scatta() {
        let cfg = ScaleTriggerConfig { enabled: true, eval_every_iters: 4, min_tail_iters: 6 };
        let ctx = ScaleContext { iterations: 8, tail_headroom: 12, ..base_ctx() };
        assert!(scale_trigger(&ctx, &cfg, false, false, false, false, false));
    }

    #[test]
    fn trigger_cambio_banda_scatta_fuori_cadenza() {
        let cfg = ScaleTriggerConfig { enabled: true, eval_every_iters: 4, min_tail_iters: 6 };
        // iters=9 (non multiplo di 4) ma pressure_changed=true.
        let ctx = ScaleContext { iterations: 9, tail_headroom: 11, ..base_ctx() };
        assert!(scale_trigger(&ctx, &cfg, false, true, false, false, false));
        // Senza alcun segnale non scatta.
        assert!(!scale_trigger(&ctx, &cfg, false, false, false, false, false));
    }

    // ── SIZING: validate_scale_move su AdjustSizing ─────────────────────────────

    #[test]
    fn validate_adjust_sizing_forme() {
        assert_eq!(
            validate_scale_move(&json!({"move": "adjust_sizing", "posture": "compact", "confidence": 0.8})),
            ScaleMove::AdjustSizing { posture: SizingPosture::Compact, confidence: 0.8 }
        );
        // Confidence fuori [0,1] -> KeepTier.
        assert_eq!(
            validate_scale_move(&json!({"move": "adjust_sizing", "posture": "relax", "confidence": 1.4})),
            ScaleMove::KeepTier
        );
        // Postura fuori vocabolario -> non deserializza -> KeepTier.
        assert_eq!(
            validate_scale_move(&json!({"move": "adjust_sizing", "posture": "turbo", "confidence": 0.9})),
            ScaleMove::KeepTier
        );
    }

    // ── SIZING: apply_sizing_gate ───────────────────────────────────────────────

    fn sizing_cfg_on() -> ScaleSizingConfig {
        ScaleSizingConfig { enabled: true, ..ScaleSizingConfig::conservative_defaults() }
    }

    fn sizing_ctx(turns_since: Option<i64>) -> ScaleContext {
        ScaleContext { sizing_turns_since_change: turns_since, ..base_ctx() }
    }

    #[test]
    fn sizing_gate_disabilitato_keep() {
        // sizing_enabled=false (default) -> AdjustSizing degrada a KeepTier.
        let cfg = ScaleSizingConfig::conservative_defaults();
        let mv = ScaleMove::AdjustSizing { posture: SizingPosture::Compact, confidence: 0.95 };
        assert_eq!(apply_sizing_gate(mv, &sizing_ctx(Some(10)), &cfg), ScaleMove::KeepTier);
    }

    #[test]
    fn sizing_gate_hold_keep() {
        let cfg = sizing_cfg_on();
        let mv = ScaleMove::AdjustSizing { posture: SizingPosture::Hold, confidence: 0.95 };
        assert_eq!(apply_sizing_gate(mv, &sizing_ctx(Some(10)), &cfg), ScaleMove::KeepTier);
    }

    #[test]
    fn sizing_gate_confidenza_sotto_soglia_keep() {
        let cfg = sizing_cfg_on();
        let mv = ScaleMove::AdjustSizing { posture: SizingPosture::Compact, confidence: 0.5 };
        assert_eq!(apply_sizing_gate(mv, &sizing_ctx(Some(10)), &cfg), ScaleMove::KeepTier);
    }

    #[test]
    fn sizing_gate_cooldown_blocca() {
        let cfg = sizing_cfg_on(); // cooldown_turns = 3
        let mv = ScaleMove::AdjustSizing { posture: SizingPosture::Compact, confidence: 0.95 };
        assert_eq!(apply_sizing_gate(mv, &sizing_ctx(Some(1)), &cfg), ScaleMove::KeepTier);
    }

    #[test]
    fn sizing_gate_passa() {
        let cfg = sizing_cfg_on();
        let mv = ScaleMove::AdjustSizing { posture: SizingPosture::Compact, confidence: 0.95 };
        // Segnale assente (None) = primo cambio -> non blocca.
        assert_eq!(
            apply_sizing_gate(mv.clone(), &sizing_ctx(None), &cfg),
            ScaleMove::AdjustSizing { posture: SizingPosture::Compact, confidence: 0.95 }
        );
        // Cooldown rispettato (>= 3).
        assert_eq!(apply_sizing_gate(mv, &sizing_ctx(Some(5)), &cfg), ScaleMove::AdjustSizing { posture: SizingPosture::Compact, confidence: 0.95 });
    }

    // ── SIZING: resolve_sizing_overrides ────────────────────────────────────────

    fn baseline() -> SizingBaseline {
        SizingBaseline {
            compress_start_iter: 5,
            compress_phase_keep_recent: vec![8, 5, 3, 2],
            compress_phase_max_chars: vec![2000, 1000, 500, 150],
            token_brake_max_context_ratio: 0.70,
            rolling_keep_recent: 2,
        }
    }

    #[test]
    fn resolve_hold_nessun_override() {
        let ov = resolve_sizing_overrides(
            SizingPosture::Hold,
            &base_ctx(),
            &baseline(),
            &sizing_cfg_on(),
        );
        assert_eq!(ov, SizingOverrides::default(), "Hold -> tutti None (bit-identico)");
    }

    #[test]
    fn resolve_compact_stringe() {
        let ctx = ScaleContext {
            history_growth_rate: Some(4.0),
            tool_result_noise: Some(8000),
            recent_productive: Some(true),
            ..base_ctx()
        };
        let ov = resolve_sizing_overrides(SizingPosture::Compact, &ctx, &baseline(), &sizing_cfg_on());
        // compress_start_iter abbassato (comprimi prima).
        assert!(ov.compress_start_iter.unwrap() <= 5);
        // keep_recent non aumenta.
        let keep = ov.compress_phase_keep_recent.unwrap();
        assert!(keep.iter().zip([8_i64, 5, 3, 2]).all(|(&n, o)| n <= o && n >= 1));
        // freno piu' aggressivo (< base 0.70) ma sopra il floor.
        let brake = ov.token_brake_max_context_ratio.unwrap();
        assert!((0.50..0.70).contains(&brake));
        // rolling ON.
        assert_eq!(ov.rolling_summary_enabled, Some(true));
        // g1 > 1.0 perche' progredisce (piu' respiro).
        assert!(ov.g1_loop_threshold_mult.unwrap() > 1.0);
    }

    #[test]
    fn resolve_relax_allarga() {
        let ctx = ScaleContext { recent_productive: Some(false), ..base_ctx() };
        let ov = resolve_sizing_overrides(SizingPosture::Relax, &ctx, &baseline(), &sizing_cfg_on());
        // keep_recent non diminuisce.
        let keep = ov.compress_phase_keep_recent.unwrap();
        assert!(keep.iter().zip([8_i64, 5, 3, 2]).all(|(&n, o)| n >= o));
        // freno piu' largo (> base) entro il ceil.
        let brake = ov.token_brake_max_context_ratio.unwrap();
        assert!(brake > 0.70 && brake <= 0.90);
        // rolling OFF.
        assert_eq!(ov.rolling_summary_enabled, Some(false));
    }

    // ── SIZING: effective_* (bit-identico se override assenti) ───────────────────

    fn ctx_mgmt_base() -> CtxMgmtConfig {
        CtxMgmtConfig {
            compress_start_iter: 5,
            compress_phase_boundaries: vec![5, 10, 20, 50],
            compress_phase_keep_recent: vec![8, 5, 3, 2],
            compress_phase_max_chars: vec![2000, 1000, 500, 150],
        }
    }

    #[test]
    fn effective_ctx_mgmt_none_invariato() {
        let base = ctx_mgmt_base();
        assert_eq!(effective_ctx_mgmt(&base, None), base, "None -> base invariata");
    }

    #[test]
    fn effective_ctx_mgmt_applica_override() {
        let base = ctx_mgmt_base();
        let ov = SizingOverrides {
            compress_start_iter: Some(3),
            compress_phase_keep_recent: Some(vec![4, 3, 2, 1]),
            ..SizingOverrides::default()
        };
        let eff = effective_ctx_mgmt(&base, Some(&ov));
        assert_eq!(eff.compress_start_iter, 3);
        assert_eq!(eff.compress_phase_keep_recent, vec![4, 3, 2, 1]);
        // max_chars non overridden -> invariato.
        assert_eq!(eff.compress_phase_max_chars, base.compress_phase_max_chars);
        // boundaries mai toccate.
        assert_eq!(eff.compress_phase_boundaries, base.compress_phase_boundaries);
    }

    #[test]
    fn effective_token_brake_e_rolling() {
        let brake_base = TokenBrakeConfig { max_context_ratio: 0.70, aggressive_keep_recent: 3, aggressive_max_chars: 200 };
        assert_eq!(effective_token_brake(&brake_base, None), brake_base);
        let ov = SizingOverrides { token_brake_max_context_ratio: Some(0.6), rolling_summary_enabled: Some(true), rolling_keep_recent: Some(4), ..SizingOverrides::default() };
        assert_eq!(effective_token_brake(&brake_base, Some(&ov)).max_context_ratio, 0.6);
        assert_eq!(effective_rolling(false, 2, None), (false, 2));
        assert_eq!(effective_rolling(false, 2, Some(&ov)), (true, 4));
    }

    #[test]
    fn effective_g1_threshold_mult() {
        assert_eq!(effective_g1_threshold(24, None), 24, "None -> base");
        let ov = SizingOverrides { g1_loop_threshold_mult: Some(1.5), ..SizingOverrides::default() };
        assert_eq!(effective_g1_threshold(24, Some(&ov)), 36);
        // Moltiplicatore non valido -> base (fail-safe).
        let bad = SizingOverrides { g1_loop_threshold_mult: Some(f64::NAN), ..SizingOverrides::default() };
        assert_eq!(effective_g1_threshold(24, Some(&bad)), 24);
    }
}
