//! Worker `model_health_probe` — pinga ogni singolo modello del catalog
//! `ai_price_catalog` con `is_enabled = true` per accertarne la salute reale,
//! a differenza di `provider_health_probe` che pinga solo UN modello per
//! provider.
//!
//! Motivazione: un provider puo' essere globalmente "up" ma alcuni modelli
//! del suo catalog possono essere broken (modello deprecato, hollow content,
//! capability non supportata). Esempi reali rilevati in produzione:
//!   - `deepseek-v3` / `deepseek-v3.2` / `deepseek-r1`: provider risponde,
//!     ma DeepSeek API ritorna 400 "supported model names are deepseek-v4-...".
//!   - `gemini-1.5-flash` / `gemini-2.0-flash`: 404 "no longer available
//!     to new users".
//!   - `gemini-3.5-flash`: enabled nel catalog ma hollow_completion costante
//!     (modello "pre-rilasciato").
//!
//! Il worker:
//!   1. Lista tutti i modelli `is_enabled = true` dal catalog.
//!   2. Salta i modelli appartenenti a provider in cooldown lungo (quota/billing).
//!   3. Pinga ognuno con un prompt minimale ("hi", max_tokens generosi per
//!      evitare falsi positivi su modelli "thinking-only" tipo gemini-2.5-pro).
//!   4. Classifica il risultato:
//!       - OK con content non-vuoto -> reset `consecutive_failures` a 0; se
//!         il modello era stato auto-disabled, lo riabilita.
//!       - Errore "provider-wide" (quota_exceeded, billing_required,
//!         rate_limit): NON incrementa il counter (e' colpa del provider,
//!         non del modello).
//!       - Errore "model-specific" (model_not_found, invalid_request,
//!         hollow_completion, unsupported, ecc.): incrementa
//!         `consecutive_failures`. Se >= soglia, auto-disable.
//!   5. Persiste tutto in `ai_model_health_history` (append-only).
//!
//! Costo: 200-300 modelli enabled, una chiamata da ~50 token / 30 min →
//! 12k-18k token/h totali, circa $0.02/giorno con i prezzi attuali.
//! Lo si puo' ridurre alzando l'interval (settings.model_health_probe_interval_s).

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::{PgPool, Row};
use tokio::time::sleep;

use crate::orchestrator::Orchestrator;
use crate::provider_cooldown::{
    is_provider_in_cooldown, put_provider_in_long_cooldown, put_provider_in_short_cooldown,
};

/// Prompt minimale. Stesso usato da `provider_health_probe`.
const PROBE_PROMPT: &str = "ping";

/// FIX 1 (probe tool-aware): nome del tool fittizio usato per verificare che
/// il modello sappia EFFETTIVAMENTE produrre una tool call (path agente), non
/// solo rispondere a una chat "ping". Diagnostica l'inversione in cui un
/// modello "ping-healthy" fallisce poi sul tool-forcing reale (es. alias
/// Mistral che risolve a un modello Labs 403, o gemini-2.5-pro che ritorna
/// MALFORMED_FUNCTION_CALL).
const TOOL_PROBE_TOOL_NAME: &str = "nexus_probe_tool";
const TOOL_PROBE_MAX_TOKENS: u32 = 256;

/// Timeout per la singola chiamata al modello. Piu' generoso del provider
/// probe (30s) perche' i modelli "thinking" (gemini-2.5-pro) possono
/// spendere molto tempo nella fase di reasoning.
const PROBE_TIMEOUT_S: u64 = 60;

/// Intervallo minimo configurabile (sotto questo, troppe chiamate API).
const MIN_INTERVAL_S: u64 = 300;

/// Pausa tra una probe model e la successiva per non saturare il rate
/// limit del provider (anche se ogni probe e' poche dozzine di token,
/// 200 probes in burst possono triggerare 429).
const INTER_PROBE_SLEEP_MS: u64 = 250;

/// Avvia il worker in background. Restituisce subito.
pub fn spawn_model_health_probe(
    orchestrator: Arc<Orchestrator>,
    db: PgPool,
    enabled: bool,
    interval_s: u64,
    failure_threshold: i32,
) {
    let enabled = match std::env::var("NEXUS_MODEL_HEALTH_PROBE_ENABLED").as_deref() {
        Ok("false") | Ok("0") => false,
        Ok("true") | Ok("1") => true,
        _ => enabled,
    };
    if !enabled {
        tracing::info!("model_health_probe: DISABILITATO (model_health_probe_enabled=false)");
        return;
    }
    let interval_s = std::env::var("NEXUS_MODEL_HEALTH_PROBE_INTERVAL_S")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(interval_s)
        .max(MIN_INTERVAL_S);
    tracing::info!(
        "model_health_probe: avvio worker (interval={interval_s}s, threshold={failure_threshold})",
    );
    tokio::spawn(async move {
        // Aspetta 60s al primo avvio: piu' del provider_health_probe (30s)
        // perche' vogliamo che quello finisca prima il suo primo giro e
        // popoli i cooldown dei provider non-funzionanti.
        sleep(Duration::from_secs(60)).await;
        loop {
            run_one_round(&orchestrator, &db, failure_threshold).await;
            sleep(Duration::from_secs(interval_s)).await;
        }
    });
}

/// Esegue UNA ronda di probe: pinga tutti i modelli enabled non skipped.
/// Esportato `pub(crate)` per consentire trigger manuale dall'endpoint
/// `POST /api/admin/probe-models`.
pub(crate) async fn run_one_round(
    orchestrator: &Orchestrator,
    db: &PgPool,
    failure_threshold: i32,
) -> ProbeRoundStats {
    let models = match load_enabled_models(db).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("model_health_probe: impossibile leggere catalog: {e}");
            return ProbeRoundStats::default();
        }
    };
    let mut stats = ProbeRoundStats {
        total: models.len(),
        ..Default::default()
    };

    let tool_cfg = load_tool_probe_config(db).await;

    for pm in models {
        probe_model_round(
            orchestrator,
            db,
            pm,
            failure_threshold,
            &tool_cfg,
            &mut stats,
        )
        .await;
    }

    run_reprobe_phase(orchestrator, db, &mut stats).await;

    tracing::info!(
        "model_health_probe: round completato — total={} healthy={} provider_errors={} \
         model_errors={} auto_disabled={} transient={} skipped={} tool_probe_ok={} \
         tool_probe_failed={} tool_probe_disabled={} reprobe_candidates={} reprobe_reenabled={}",
        stats.total,
        stats.healthy,
        stats.provider_wide_errors,
        stats.model_errors,
        stats.auto_disabled,
        stats.transient,
        stats.skipped_provider_cooldown,
        stats.tool_probe_ok,
        stats.tool_probe_failed,
        stats.tool_probe_disabled,
        stats.reprobe_candidates,
        stats.reprobe_reenabled,
    );
    stats
}

/// Config DB-driven del tool-probe (FIX 1, regola G: niente hardcode).
/// Ritorna `(tool_probe_enabled, tool_failure_threshold)`.
/// - `agent.model_tool_probe.enabled` (default true): abilita il tool-probe.
/// - `agent.model_tool_failure_threshold` (default 3, mig 0269): soglia oltre
///   la quale un modello che fallisce il tool-forcing viene marcato
///   supports_tool_use=false (NON is_enabled=false).
async fn load_tool_probe_config(db: &PgPool) -> (bool, i32) {
    let tool_probe_enabled = crate::settings::get_setting(db, "agent.model_tool_probe.enabled")
        .await
        .ok()
        .flatten()
        .map(|v| {
            !matches!(
                v.trim().to_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true);
    let tool_failure_threshold =
        crate::settings::get_setting(db, "agent.model_tool_failure_threshold")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse::<i32>().ok())
            .filter(|t| *t > 0)
            .unwrap_or(3);
    (tool_probe_enabled, tool_failure_threshold)
}

/// Esegue il chat-probe e (quando eleggibile) il tool-probe di UN modello del
/// giro principale, aggiornando i contatori in `stats`. Estratta da
/// `run_one_round` per contenerne la lunghezza; nessun cambio di comportamento.
async fn probe_model_round(
    orchestrator: &Orchestrator,
    db: &PgPool,
    pm: ProbeModel,
    failure_threshold: i32,
    tool_cfg: &(bool, i32),
    stats: &mut ProbeRoundStats,
) {
    // Salta se il provider e' in cooldown lungo: faremmo solo rumore
    // (tutte le probe ritornerebbero errore di quota/billing che e'
    // gia' noto al sistema).
    if is_provider_in_cooldown(&pm.provider) {
        stats.skipped_provider_cooldown += 1;
        return;
    }

    let outcome = probe_one_model(
        orchestrator,
        db,
        &pm.provider,
        &pm.model,
        pm.consecutive_failures,
        failure_threshold,
    )
    .await;
    stats.record_chat_probe(&outcome);

    sleep(Duration::from_millis(INTER_PROBE_SLEEP_MS)).await;

    maybe_tool_probe_round(orchestrator, db, &pm, tool_cfg, stats).await;
}

/// Esegue (se eleggibile) il tool-probe di un modello del giro principale e
/// aggiorna i contatori tool in `stats`. Estratta da `probe_model_round`.
///
/// Il tool-probe gira sui candidati agentici (supports_tool_use=true) E
/// sui modelli AUTO-DEGRADATI da un writer automatico (supports_tool_use=
/// false con reason 'tool_probe_failed:%' dal probe O 'malformed_tool_calls'
/// dal runtime, solo capability_source='auto'). Senza questo secondo caso
/// il re-enable promesso era IRRAGGIUNGIBILE: un modello marcato
/// non-tool-capable non veniva mai piu' ri-testato (catch-22) e restava
/// degradato anche dopo che il provider tornava sano (es. i magistral; e
/// il degrado runtime era riabilitabile SOLO se la routing matrix
/// continuava a scegliere il modello — caso deepseek-v4-pro). I modelli
/// pure-chat (mai tool-capable) e le curature manual
/// (capability_source='manual') NON vengono toccati. Provider in cooldown
/// gia' saltato dal chiamante.
/// PUNTO UNICO (regola L): il criterio di riaggancio vive in
/// tool_capability::was_auto_degraded e copre ANCHE le righe ORFANE
/// (reason NULL con counter > 0): un re-enable esterno (catalog_sync
/// "ricomparso API", ciclo billing cooldown) puo' azzerare il reason
/// senza ripristinare il flag — senza questo ramo il degrado restava
/// permanente (incidente magistral-small-2509, 2026-06-10).
async fn maybe_tool_probe_round(
    orchestrator: &Orchestrator,
    db: &PgPool,
    pm: &ProbeModel,
    tool_cfg: &(bool, i32),
    stats: &mut ProbeRoundStats,
) {
    let (tool_probe_enabled, tool_failure_threshold) = *tool_cfg;
    let tool_probe_was_auto_degraded = crate::tool_capability::was_auto_degraded(
        pm.supports_tool_use,
        &pm.capability_source,
        pm.auto_disabled_reason.as_deref(),
        pm.consecutive_tool_failures,
    );
    if tool_probe_enabled && (pm.supports_tool_use || tool_probe_was_auto_degraded) {
        match tool_probe_one_model(
            orchestrator,
            db,
            &pm.provider,
            &pm.model,
            tool_failure_threshold,
        )
        .await
        {
            ToolProbeOutcome::Ok => stats.tool_probe_ok += 1,
            ToolProbeOutcome::FailedCounted => stats.tool_probe_failed += 1,
            ToolProbeOutcome::MarkedNonToolCapable => {
                stats.tool_probe_failed += 1;
                stats.tool_probe_disabled += 1;
            }
            ToolProbeOutcome::Skipped | ToolProbeOutcome::Transient => {}
        }
        sleep(Duration::from_millis(INTER_PROBE_SLEEP_MS)).await;
    }
}

/// Fase di RE-PROBE dei candidati disabilitati per QUIRK GATEWAY.
/// FIX nodo strutturale (regola H): i modelli auto-disabilitati per un quirk
/// del gateway (tool-probe fallito / malformed tool calls) NON venivano piu'
/// caricati da load_enabled_models (WHERE is_enabled=true) -> mai ri-probati
/// -> restavano disabilitati per sempre anche dopo che il quirk era corretto
/// in produzione (es. mistral/google a 0 modelli abilitati). Qui ricarichiamo
/// ESPLICITAMENTE quei candidati e, con backoff DB-driven, li ri-probiamo
/// (chat-probe + tool-probe via le stesse funzioni del giro principale,
/// regola L). Se entrambi passano, li riabilitiamo da soli.
async fn run_reprobe_phase(orchestrator: &Orchestrator, db: &PgPool, stats: &mut ProbeRoundStats) {
    let backoff_min = crate::settings::get_setting(db, "agent.model_reprobe.backoff_minutes")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|m| *m > 0)
        .unwrap_or(REPROBE_BACKOFF_DEFAULT_MIN);
    match load_reprobe_candidates(db).await {
        Ok(candidates) => {
            for cand in candidates {
                // Provider in cooldown lungo: inutile re-probare (errori billing/
                // quota gia' noti). Riprovato quando il provider torna su.
                if is_provider_in_cooldown(&cand.provider) {
                    continue;
                }
                match reprobe_one_candidate(orchestrator, db, &cand, backoff_min).await {
                    ReprobeResult::Backoff => continue,
                    ReprobeResult::Reenabled => {
                        stats.reprobe_candidates += 1;
                        stats.reprobe_reenabled += 1;
                    }
                    ReprobeResult::ProviderWide
                    | ReprobeResult::StillBroken
                    | ReprobeResult::Inconclusive => {
                        stats.reprobe_candidates += 1;
                    }
                }
                sleep(Duration::from_millis(INTER_PROBE_SLEEP_MS)).await;
            }
        }
        Err(e) => {
            tracing::warn!("model_health_probe: impossibile leggere candidati re-probe: {e}");
        }
    }
}

#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct ProbeRoundStats {
    pub total: usize,
    pub healthy: usize,
    pub provider_wide_errors: usize,
    pub model_errors: usize,
    pub auto_disabled: usize,
    /// Esiti inconclusivi (errore opaco/transitorio): nessuna penalizzazione del
    /// modello, ritentati al round successivo. Conteggiati a parte per non
    /// inquinare `model_errors` (che governa la lettura "modelli rotti").
    pub transient: usize,
    pub skipped_provider_cooldown: usize,
    /// FIX 1: modelli tool-capable che hanno superato il tool-probe.
    pub tool_probe_ok: usize,
    /// FIX 1: modelli che hanno fallito il tool-probe (sotto soglia).
    pub tool_probe_failed: usize,
    /// FIX 1: modelli marcati supports_tool_use=false dal tool-probe (soglia).
    pub tool_probe_disabled: usize,
    /// RE-PROBE: candidati disabilitati per quirk gateway considerati nel giro
    /// (oltre il backoff). I candidati ancora in backoff NON sono conteggiati.
    pub reprobe_candidates: usize,
    /// RE-PROBE: candidati riabilitati (is_enabled=true) perche' chat-probe +
    /// tool-probe sono ora entrambi OK (quirk gateway risolto).
    pub reprobe_reenabled: usize,
}

impl ProbeRoundStats {
    /// Aggrega l'esito di un chat-probe nei contatori del round.
    fn record_chat_probe(&mut self, outcome: &ProbeOutcome) {
        match outcome {
            ProbeOutcome::Ok => self.healthy += 1,
            ProbeOutcome::ProviderWide => self.provider_wide_errors += 1,
            ProbeOutcome::ModelSpecificCounted => self.model_errors += 1,
            ProbeOutcome::AutoDisabled => {
                self.model_errors += 1;
                self.auto_disabled += 1;
            }
            ProbeOutcome::Transient => self.transient += 1,
        }
    }
}

enum ProbeOutcome {
    Ok,
    ProviderWide,
    ModelSpecificCounted,
    AutoDisabled,
    /// Esito inconclusivo (errore opaco/transitorio): nessuna azione sui
    /// contatori/is_enabled, conteggiato a parte per diagnostica.
    Transient,
}

/// Esito del tool-probe (FIX 1). Non tocca mai `is_enabled`: solo
/// `supports_tool_use` + `consecutive_tool_failures`.
enum ToolProbeOutcome {
    /// Tool call valida ricevuta -> reset contatore + riabilita tool-capability.
    Ok,
    /// Tool-forcing fallito ma sotto soglia -> incremento contatore.
    FailedCounted,
    /// Soglia raggiunta -> supports_tool_use=false (modello resta per chat).
    MarkedNonToolCapable,
    /// Errore provider-wide (cooldown gia' applicato dal probe chat) -> nessuna
    /// azione tool-specific (non punisce il modello per colpa del provider).
    Skipped,
    /// Esito inconclusivo (errore opaco/transitorio): nessuna azione su
    /// supports_tool_use ne' sul contatore, ritentato al round successivo.
    Transient,
}

/// Riga del catalog per il probe. `capability_source` e `auto_disabled_reason`
/// servono al tool-probe per decidere se RI-testare un modello gia' marcato
/// non-tool-capable (chiude il catch-22 del re-enable, vedi loop principale).
struct ProbeModel {
    provider: String,
    model: String,
    consecutive_failures: i32,
    supports_tool_use: bool,
    capability_source: String,
    auto_disabled_reason: Option<String>,
    /// Counter del ciclo tool-capability: serve al gate di ri-test per
    /// riconoscere le righe orfane (false + reason NULL + counter > 0).
    consecutive_tool_failures: i32,
}

/// Legge i modelli enabled dal catalog (con i campi necessari al tool-probe).
async fn load_enabled_models(db: &PgPool) -> sqlx::Result<Vec<ProbeModel>> {
    let rows = sqlx::query(
        "SELECT provider, model, consecutive_failures, \
                COALESCE(supports_tool_use, false) AS supports_tool_use, \
                COALESCE(capability_source, 'auto') AS capability_source, \
                auto_disabled_reason, \
                COALESCE(consecutive_tool_failures, 0) AS consecutive_tool_failures
           FROM ai_price_catalog
          WHERE is_enabled = true
          ORDER BY provider, model",
    )
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| ProbeModel {
            provider: r.try_get("provider").unwrap_or_default(),
            model: r.try_get("model").unwrap_or_default(),
            consecutive_failures: r.try_get("consecutive_failures").unwrap_or(0),
            supports_tool_use: r.try_get("supports_tool_use").unwrap_or(false),
            capability_source: r
                .try_get("capability_source")
                .unwrap_or_else(|_| "auto".to_string()),
            auto_disabled_reason: r.try_get("auto_disabled_reason").ok().flatten(),
            consecutive_tool_failures: r.try_get("consecutive_tool_failures").unwrap_or(0),
        })
        .collect())
}

/// Default del backoff fra due re-probe consecutivi dello stesso candidato
/// disabilitato (minuti). Usato solo se il setting DB e' assente/illeggibile.
/// 30 min: con il loop del worker a >=5 min, un candidato viene riprovato al
/// massimo ogni ~30 min finche' non torna sano (poi e' riabilitato e basta).
const REPROBE_BACKOFF_DEFAULT_MIN: i64 = 30;

/// Riga candidata al RE-PROBE: modello disabilitato per un QUIRK GATEWAY
/// ri-testabile (tool-probe fallito / malformed tool calls), da riprovare dopo
/// il backoff per riabilitarlo quando il quirk e' stato corretto in produzione.
struct ReprobeCandidate {
    provider: String,
    model: String,
    /// Reason che lo ha disabilitato (per audit log).
    reason: String,
    /// Timestamp dell'ultimo tentativo (= auto_disabled_at): governa il backoff.
    auto_disabled_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Era stata degradata anche la tool-capability? Se sì, va ripristinata al
    /// re-enable.
    supports_tool_use: bool,
}

/// Predicato PURO (testabile senza DB) che decide se una riga del catalog e'
/// un candidato al re-probe: deve essere disabilitata per un QUIRK GATEWAY
/// ri-testabile e NON per una causa che non si risolve ri-probando subito
/// (billing/quota -> cooldown; missing_from_api -> non esiste piu'; policy ->
/// decisione amministrativa; lock manuale). Punto unico (regola L) del criterio
/// di inclusione/esclusione: la query SQL e i test usano questa funzione.
fn is_reprobe_candidate(is_enabled: bool, capability_source: &str, reason: Option<&str>) -> bool {
    if is_enabled {
        return false;
    }
    // Lock manuale: mai ri-probato in automatico (curatela admin).
    if capability_source == "manual" {
        return false;
    }
    let Some(reason) = reason.map(str::trim).filter(|r| !r.is_empty()) else {
        return false;
    };
    if reason.starts_with("manual:") {
        return false;
    }
    // Reason di QUIRK GATEWAY ri-testabile: tool-probe fallito o malformed tool
    // calls dal runtime (lo stesso ciclo tool-capability, regola L).
    reason.starts_with(crate::tool_capability::REASON_TOOL_PROBE_PREFIX)
        || reason == crate::tool_capability::REASON_MALFORMED_TOOL_CALLS
}

/// Decide se il candidato ha atteso abbastanza dal suo ultimo tentativo
/// (`auto_disabled_at`) per essere ri-probato. PURO/testabile. Se
/// `auto_disabled_at` e' NULL (mai marcato) il candidato e' eleggibile subito.
fn reprobe_due(
    auto_disabled_at: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
    backoff_min: i64,
) -> bool {
    match auto_disabled_at {
        None => true,
        Some(ts) => now.signed_duration_since(ts) >= chrono::Duration::minutes(backoff_min.max(1)),
    }
}

/// Legge i candidati al re-probe dal catalog. Il filtro SQL replica
/// `is_reprobe_candidate` (inclusi: reason che inizia con `tool_probe_failed:`
/// oppure `malformed_tool_calls`; esclusi: is_enabled=true, capability_source=
/// 'manual', reason `manual:%`, e implicitamente tutti gli altri reason del
/// ciclo is_enabled — billing/quota cooldown, missing_from_api, %policy%,
/// hollow_completion, not_chat_compatible — che NON vanno ri-probati cosi').
async fn load_reprobe_candidates(db: &PgPool) -> sqlx::Result<Vec<ReprobeCandidate>> {
    let predicate = crate::tool_capability::TOOL_REASON_PREDICATE_SQL;
    let sql = format!(
        "SELECT provider, model, auto_disabled_reason, auto_disabled_at, \
                COALESCE(capability_source, 'auto') AS capability_source, \
                COALESCE(supports_tool_use, false) AS supports_tool_use \
           FROM ai_price_catalog \
          WHERE is_enabled = false \
            AND COALESCE(capability_source, 'auto') <> 'manual' \
            AND auto_disabled_reason IS NOT NULL \
            AND auto_disabled_reason NOT LIKE 'manual:%' \
            AND {predicate} \
          ORDER BY provider, model"
    );
    let rows = sqlx::query(&sql).fetch_all(db).await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let reason: Option<String> = r
                .try_get::<Option<String>, _>("auto_disabled_reason")
                .ok()
                .flatten();
            let capability_source: String = r
                .try_get("capability_source")
                .unwrap_or_else(|_| "auto".to_string());
            // PUNTO UNICO (regola L): il criterio di inclusione/esclusione vive
            // in is_reprobe_candidate. La query lo replica per efficienza, ma il
            // filtro Rust e' la fonte autorevole — cosi' SQL e codice non possono
            // divergere e la funzione e' davvero usata (non solo testata).
            if !is_reprobe_candidate(false, &capability_source, reason.as_deref()) {
                return None;
            }
            Some(ReprobeCandidate {
                provider: r.try_get("provider").unwrap_or_default(),
                model: r.try_get("model").unwrap_or_default(),
                reason: reason.unwrap_or_default(),
                auto_disabled_at: r.try_get("auto_disabled_at").ok().flatten(),
                supports_tool_use: r.try_get("supports_tool_use").unwrap_or(false),
            })
        })
        .collect())
}

/// Esito del re-probe di UN candidato.
enum ReprobeResult {
    /// Backoff non ancora scaduto: nessun tentativo.
    Backoff,
    /// Chat-probe + tool-probe entrambi OK -> riabilitato (is_enabled=true).
    Reenabled,
    /// Errore provider-wide (billing/quota/rate): cooldown gia' applicato dal
    /// chat-probe, nessuna azione sul candidato (riprovato al prossimo giro).
    ProviderWide,
    /// Ancora rotto (chat o tool falliti per causa model-specific): timestamp di
    /// backoff aggiornato.
    StillBroken,
    /// Esito inconclusivo (errore opaco/transitorio): NON tocca il backoff —
    /// altrimenti ogni blip rinvierebbe il re-probe di un intero ciclo di
    /// backoff (micro circolo vizioso). Riprovato gia' al prossimo giro.
    Inconclusive,
}

/// Re-proba un singolo candidato disabilitato per quirk gateway. Riusa il
/// chat-probe SENZA side-effect (`probe_model_on_insert`) e il tool-probe SENZA
/// side-effect (`run_tool_probe`): solo qui decide il re-enable. Se entrambi
/// passano riabilita il modello (is_enabled=true, reason NULL, contatori a 0,
/// e ripristina supports_tool_use se era stato degradato).
async fn reprobe_one_candidate(
    orchestrator: &Orchestrator,
    db: &PgPool,
    cand: &ReprobeCandidate,
    backoff_min: i64,
) -> ReprobeResult {
    if !reprobe_due(cand.auto_disabled_at, chrono::Utc::now(), backoff_min) {
        return ReprobeResult::Backoff;
    }
    let provider = cand.provider.as_str();
    let model = cand.model.as_str();

    // 1. Chat-probe (no side-effect). Se fallisce, il verdetto e' gia' definitivo.
    if let Some(early) = reprobe_chat_step(orchestrator, db, provider, model).await {
        return early;
    }

    // 2. Tool-probe (no side-effect). Deve passare anch'esso: il quirk era
    //    proprio sul tool-forcing.
    if let Some(early) = reprobe_tool_step(orchestrator, db, provider, model).await {
        return early;
    }

    // 3. Entrambi OK: riabilita.
    reenable_candidate(db, cand, provider, model).await;
    ReprobeResult::Reenabled
}

/// Esegue lo step chat-probe del re-probe: ritorna `Some(risultato)` se il
/// verdetto e' gia' definitivo (rimanda/resta rotto/inconclusivo), `None` se il
/// chat-probe e' passato e si puo' procedere al tool-probe. Il ProviderWide non
/// tocca il backoff (cooldown gia' gestito altrove); ModelBroken/Inconclusive lo
/// aggiornano; il Transient NON lo tocca (regola H: niente micro circolo vizioso).
async fn reprobe_chat_step(
    orchestrator: &Orchestrator,
    db: &PgPool,
    provider: &str,
    model: &str,
) -> Option<ReprobeResult> {
    match probe_model_on_insert(orchestrator, provider, model).await {
        ProbeOnInsertResult::Healthy => None,
        ProbeOnInsertResult::ProviderDown(kind) => {
            tracing::debug!(
                "model_health_probe[reprobe]: {provider}/{model} chat-probe provider-wide '{kind}' -> rimando"
            );
            Some(ReprobeResult::ProviderWide)
        }
        ProbeOnInsertResult::ModelBroken(kind) | ProbeOnInsertResult::Inconclusive(kind) => {
            touch_reprobe_backoff(db, provider, model).await;
            tracing::debug!(
                "model_health_probe[reprobe]: {provider}/{model} chat-probe ancora rotto ({kind}) -> resta disabilitato"
            );
            Some(ReprobeResult::StillBroken)
        }
        ProbeOnInsertResult::Transient(kind) => {
            // INCONCLUSIVO: NON tocca il backoff (regola H). Resta disabilitato,
            // ma viene riprovato gia' al prossimo giro del worker.
            tracing::debug!(
                "model_health_probe[reprobe]: {provider}/{model} chat-probe inconclusivo ({kind}) -> backoff invariato, ritento al prossimo round"
            );
            Some(ReprobeResult::Inconclusive)
        }
    }
}

/// Esegue lo step tool-probe del re-probe: stessa semantica di `reprobe_chat_step`
/// (`Some` = verdetto definitivo, `None` = passato). Il quirk gateway era proprio
/// sul tool-forcing, quindi il tool-probe deve passare per riabilitare.
async fn reprobe_tool_step(
    orchestrator: &Orchestrator,
    db: &PgPool,
    provider: &str,
    model: &str,
) -> Option<ReprobeResult> {
    let (verdict, _latency) = run_tool_probe(orchestrator, provider, model).await;
    match verdict {
        ToolProbeVerdict::Success => None,
        ToolProbeVerdict::ProviderWide(kind) => {
            tracing::debug!(
                "model_health_probe[reprobe]: {provider}/{model} tool-probe provider-wide '{kind}' -> rimando"
            );
            Some(ReprobeResult::ProviderWide)
        }
        ToolProbeVerdict::ToolFailed(kind) => {
            touch_reprobe_backoff(db, provider, model).await;
            tracing::debug!(
                "model_health_probe[reprobe]: {provider}/{model} tool-probe ancora rotto ({kind}) -> resta disabilitato"
            );
            Some(ReprobeResult::StillBroken)
        }
        ToolProbeVerdict::Transient(kind) => {
            // INCONCLUSIVO: come il chat-probe, NON tocca il backoff.
            tracing::debug!(
                "model_health_probe[reprobe]: {provider}/{model} tool-probe inconclusivo ({kind}) -> backoff invariato, ritento al prossimo round"
            );
            Some(ReprobeResult::Inconclusive)
        }
    }
}

/// Riabilita un candidato che ha superato chat-probe + tool-probe: is_enabled=true,
/// reason/timestamp azzerati, contatori a 0, e ripristino di supports_tool_use se
/// era stato degradato. Non tocca le righe manual (guard nella WHERE).
async fn reenable_candidate(db: &PgPool, cand: &ReprobeCandidate, provider: &str, model: &str) {
    let restore_tool = !cand.supports_tool_use;
    let _ = sqlx::query(
        "UPDATE ai_price_catalog \
            SET is_enabled = true, \
                effective_from = NOW(), \
                auto_disabled_at = NULL, \
                auto_disabled_reason = NULL, \
                consecutive_failures = 0, \
                consecutive_tool_failures = 0, \
                supports_tool_use = CASE WHEN $3 THEN true ELSE supports_tool_use END, \
                updated_at = NOW() \
          WHERE provider = $1 AND model = $2 \
            AND is_enabled = false \
            AND COALESCE(capability_source, 'auto') <> 'manual'",
    )
    .bind(provider)
    .bind(model)
    .bind(restore_tool)
    .execute(db)
    .await;
    tracing::info!(
        "model_health_probe[reprobe]: RE-ENABLE {provider}/{model} (era disabilitato per '{}', quirk gateway risolto: chat-probe + tool-probe OK)",
        cand.reason
    );
}

/// Aggiorna il timestamp di backoff (`auto_disabled_at`) a NOW() per un
/// candidato che ha appena fallito il re-probe, cosi' il prossimo tentativo
/// rispetta `backoff_min`. NON tocca is_enabled/reason: resta disabilitato.
async fn touch_reprobe_backoff(db: &PgPool, provider: &str, model: &str) {
    let _ = sqlx::query(
        "UPDATE ai_price_catalog SET auto_disabled_at = NOW(), updated_at = NOW() \
          WHERE provider = $1 AND model = $2 AND is_enabled = false",
    )
    .bind(provider)
    .bind(model)
    .execute(db)
    .await;
}

/// Pinga un singolo modello e applica la logica di counter / auto-disable.
async fn probe_one_model(
    orchestrator: &Orchestrator,
    db: &PgPool,
    provider: &str,
    model: &str,
    prior_failures: i32,
    failure_threshold: i32,
) -> ProbeOutcome {
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(PROBE_TIMEOUT_S),
        orchestrator
            .neural
            .generate_completion(provider, model, PROBE_PROMPT),
    )
    .await;
    let latency_ms = started.elapsed().as_millis() as i32;

    let outcome = match result {
        Ok(Ok(response)) => classify_probe_response(orchestrator, provider, &response).await,
        Ok(Err(e)) => {
            // Errore di trasporto/gRPC: classificazione via il punto UNICO.
            let ec = orchestrator
                .neural
                .classify_error(&e.to_string(), provider)
                .await;
            classification_from_error_class(&ec)
        }
        // Timeout del probe: INCONCLUSIVO, non model-specific. Un modello sano
        // dietro un provider lento (cold-start, coda) andava in auto-disable.
        Err(_timeout_elapsed) => Classification::Transient(
            "timeout".to_string(),
            Some(format!("no response in {PROBE_TIMEOUT_S}s")),
        ),
    };

    persist_probe_history(db, provider, model, &outcome, latency_ms).await;

    apply_probe_outcome(
        db,
        provider,
        model,
        outcome,
        prior_failures,
        failure_threshold,
    )
    .await
}

/// Classifica una response del brain andata a buon fine sul trasporto (chat-probe).
/// Estratta da `probe_one_model`: prima legge l'`error_class` canonico (fonte
/// autorevole, language-agnostic), poi ricade sull'analisi del content
/// (hollow_completion / "[Error:" ingoiato / OK).
async fn classify_probe_response(
    orchestrator: &Orchestrator,
    provider: &str,
    response: &serde_json::Value,
) -> Classification {
    // PRIMA dell'analisi del content: il brain riformatta gli errori
    // provider in un messaggio umano (multilingua) che NON inizia con
    // "[Error:", mascherando il fallimento. Ma ritorna anche il campo
    // strutturato `error_class`: lo usiamo come fonte autorevole
    // (language-agnostic). Mappa error_class -> Classification.
    let ec = response
        .get("error_class")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            response
                .get("metadata")
                .and_then(|m| m.get("error_class"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("");
    // error_class CANONICO prodotto dal brain (unico classificatore):
    // mappiamo valore->azione, senza ri-classificare il testo.
    if !ec.is_empty() {
        return classification_from_error_class(ec);
    }
    // Fallback: analisi del content (hollow_completion / "[Error:" / OK).
    let content_text = extract_content_text(response);
    let trimmed = content_text.trim();
    if trimmed.is_empty() {
        Classification::ModelSpecific(
            "hollow_completion".to_string(),
            Some("response had 0 chars of content".to_string()),
        )
    } else if trimmed.starts_with("[Error:") || trimmed.starts_with("[error:") {
        // Errore ingoiato dal brain. Estrai messaggio e classifica.
        let inner = trimmed
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim_start_matches("Error:")
            .trim_start_matches("error:")
            .trim();
        // Classificazione via il punto UNICO (brain gRPC).
        let ec = orchestrator.neural.classify_error(inner, provider).await;
        classification_from_error_class(&ec)
    } else {
        Classification::Ok
    }
}

/// Persiste l'esito del chat-probe in `ai_model_health_history` (fire-and-forget,
/// nessun impatto se fallisce). Il Transient viene tracciato come unhealthy SOLO
/// per diagnostica (storico append-only): non tocca contatori ne' is_enabled.
/// error_kind prefissato 'transient:' lo distingue dai guasti reali nelle query
/// dello storico.
async fn persist_probe_history(
    db: &PgPool,
    provider: &str,
    model: &str,
    outcome: &Classification,
    latency_ms: i32,
) {
    let (healthy, error_kind, error_message) = match outcome {
        Classification::Ok => (true, None, None),
        Classification::ProviderWide(kind, msg) | Classification::ModelSpecific(kind, msg) => {
            (false, Some(kind.clone()), msg.clone())
        }
        Classification::Transient(kind, msg) => {
            (false, Some(format!("transient:{kind}")), msg.clone())
        }
    };
    let _ = sqlx::query(
        r#"INSERT INTO ai_model_health_history
           (provider, model, healthy, latency_ms, error_kind, error_message)
           VALUES ($1, $2, $3, $4, $5, $6)"#,
    )
    .bind(provider)
    .bind(model)
    .bind(healthy)
    .bind(latency_ms)
    .bind(error_kind.as_deref())
    .bind(error_message.as_deref().map(|s| truncate(s, 500)))
    .execute(db)
    .await;
}

/// Categorie di `error_class` che NON sono colpa del MODELLO ma dell'ambiente o
/// del provider (credito, quota, contesto, infrastruttura, rete): un run che
/// fallisce con una di queste NON deve abbassare la likelihood del modello.
/// Allineato a `is_provider_wide_error` della governance (regola M): match su
/// sottostringa della categoria STRUTTURATA, mai sul testo umano.
pub fn run_error_is_environmental(error_class: Option<&str>) -> bool {
    let Some(ec) = error_class else {
        return false;
    };
    let ec = ec.trim().to_ascii_lowercase();
    if ec.is_empty() {
        return false;
    }
    const ENV: &[&str] = &[
        "infrastructure",
        "context_overflow",
        "billing",
        "quota",
        "rate_limit",
        "overload",
        "service_unavailable",
        "credit",
        "balance",
        "insufficient",
        "timeout",
        "network",
    ];
    ENV.iter().any(|k| ec.contains(k))
}

/// PUNTO UNICO (regola L, M): decide se l'esito di un RUN e' un fallimento
/// attribuibile al MODELLO (che deve abbassarne la likelihood) o no. Pura,
/// testabile. Colpa del modello SOLO negli esiti dove il modello ha davvero
/// girato e non ha completato la sostanza del task:
///   - `Failed` / `LoopAborted` / `FailedDiagnosed` (non-convergenza, loop, cap);
/// ED esclusi i casi che NON sono colpa sua:
///   - `BlockedNeedsInput` (blocco ambientale REALE dichiarato) e ogni altro
///     status non-fallito -> non entrano (mappati fuori dall'enum-target);
///   - `error_class` ambientale/provider-wide (credito, quota, contesto, infra);
///   - `hollow_*` -> gia' gestiti dai loro contatori dedicati (tool_capability /
///     consecutive_failures), non doppiare.
pub fn run_outcome_blames_model(
    status: crate::agent_types::AgentRunStatus,
    error_class: Option<&str>,
    hollow_completion: bool,
    hollow_no_tools: bool,
) -> bool {
    use crate::agent_types::AgentRunStatus::{Failed, FailedDiagnosed, LoopAborted};
    matches!(status, Failed | LoopAborted | FailedDiagnosed)
        && !hollow_completion
        && !hollow_no_tools
        && !run_error_is_environmental(error_class)
}

/// Registra l'esito di un RUN reale nella telemetria di salute del modello
/// (`ai_model_health_history`), cosi' la governance (`likelihood_score`) lo
/// consuma nella finestra scorrevole. Segnale MORBIDO: a differenza del probe e
/// dell'hollow, NON tocca `is_enabled` ne' i contatori di auto-disable — abbassa
/// o rialza solo l'error-rate recente, e il "reset" e' implicito (i run di
/// successo escono l'error-rate dalla finestra). Un falso positivo occasionale
/// (es. debito preesistente su un formato non ancora coperto dal gate) retrocede
/// il modello di poco e viene recuperato al primo successo. `error_kind` vuoto o
/// `healthy=true` -> nessuna categoria d'errore (record positivo).
pub async fn record_run_outcome_health(
    db: &PgPool,
    provider: &str,
    model: &str,
    healthy: bool,
    error_kind: &str,
) {
    let ek = if healthy || error_kind.trim().is_empty() {
        None
    } else {
        Some(error_kind)
    };
    let _ = sqlx::query(
        r#"INSERT INTO ai_model_health_history
           (provider, model, healthy, latency_ms, error_kind, error_message)
           VALUES ($1, $2, $3, NULL, $4, NULL)"#,
    )
    .bind(provider)
    .bind(model)
    .bind(healthy)
    .bind(ek)
    .execute(db)
    .await;
}

/// Applica la logica counter / auto-disable / auto-reenable in base alla
/// classificazione dell'esito del chat-probe. Estratta da `probe_one_model`.
async fn apply_probe_outcome(
    db: &PgPool,
    provider: &str,
    model: &str,
    outcome: Classification,
    prior_failures: i32,
    failure_threshold: i32,
) -> ProbeOutcome {
    match outcome {
        Classification::Ok => handle_probe_ok(provider, model, prior_failures),
        Classification::ProviderWide(kind, _) => {
            // Errore di provider, non del modello: counter del modello invariato.
            // Oltre al conteggio statistico, propaga il problema a
            // `provider_cooldown` mappando il `kind` sul cooldown appropriato: se
            // no, con un provider in quota/billing ogni modello del catalog
            // classificava "provider-wide error" senza mai mettere il provider in
            // cooldown, e ogni round ritentava tutti i suoi modelli con lo stesso
            // 429 (bug architetturale corretto qui).
            apply_provider_wide_cooldown(provider, model, &kind).await;
            ProbeOutcome::ProviderWide
        }
        Classification::ModelSpecific(kind, _msg) => {
            record_model_specific_failure(
                db,
                provider,
                model,
                &kind,
                prior_failures,
                failure_threshold,
            )
            .await
        }
        Classification::Transient(kind, _) => {
            // INCONCLUSIVO: non e' colpa ne' del modello ne' (con certezza) del
            // provider. Lo stato resta INVARIATO — niente incremento di
            // consecutive_failures, niente is_enabled=false, niente cooldown.
            // Si ritenta al round successivo. Questo e' il fix di radice
            // (regola H): un cold-start auth Vertex / 5xx gateway / error_class
            // generico non deve piu' marciare verso l'auto-disable.
            tracing::debug!(
                "model_health_probe: {provider}/{model} esito inconclusivo ({kind}) -> stato invariato, ritento al prossimo round"
            );
            ProbeOutcome::Transient
        }
    }
}

/// Gestisce l'esito OK del chat-probe (solo logging, nessuna scrittura DB).
///
/// Il probe usa prompt "ping" (1-2 token output) — un account con budget quasi
/// vuoto puo' passare il probe ma fallire sui workload reali (es. anthropic con
/// credit basso risponde a "hi" ma fallisce su 5000+ token). Quindi il probe-OK
/// NON resetta il counter di consecutive_failures: solo i run REALI (in
/// chat_messages.rs::2117+) possono resettarlo, perche' solo loro testano
/// workload reale. Il probe puo' SOLO segnalare success per logging.
fn handle_probe_ok(provider: &str, model: &str, prior_failures: i32) -> ProbeOutcome {
    if prior_failures > 0 {
        tracing::debug!(
            "model_health_probe: {provider}/{model} probe-OK ma prior_failures={} (non reset, attende run reale)",
            prior_failures
        );
    }
    ProbeOutcome::Ok
}

/// Registra un fallimento model-specific: incrementa `consecutive_failures` e,
/// a soglia, auto-disabilita il modello (is_enabled=false + reason). Estratta
/// dal ramo ModelSpecific di `apply_probe_outcome`.
async fn record_model_specific_failure(
    db: &PgPool,
    provider: &str,
    model: &str,
    kind: &str,
    prior_failures: i32,
    failure_threshold: i32,
) -> ProbeOutcome {
    let new_count = prior_failures + 1;
    // invalid_model / model_not_found: auto-disable IMMEDIATO (regola H, incidente
    // mistral-small deprecato). Non attendere N consecutive_failures.
    let immediate_disable = kind == "invalid_model" || kind == "model_not_found";
    let should_disable = immediate_disable || new_count >= failure_threshold;
    if should_disable {
        let _ = sqlx::query(
            "UPDATE ai_price_catalog
                SET is_enabled = false,
                    consecutive_failures = $3,
                    auto_disabled_at = NOW(),
                    auto_disabled_reason = $4,
                    updated_at = NOW()
              WHERE provider = $1 AND model = $2",
        )
        .bind(provider)
        .bind(model)
        .bind(new_count)
        .bind(kind)
        .execute(db)
        .await;
        tracing::warn!(
            "model_health_probe: AUTO-DISABLE {provider}/{model} (failures={new_count}, reason={kind}, immediate={immediate_disable})"
        );
        ProbeOutcome::AutoDisabled
    } else {
        let _ = sqlx::query(
            "UPDATE ai_price_catalog
                SET consecutive_failures = $3, updated_at = NOW()
              WHERE provider = $1 AND model = $2",
        )
        .bind(provider)
        .bind(model)
        .bind(new_count)
        .execute(db)
        .await;
        tracing::debug!(
            "model_health_probe: {provider}/{model} fail #{new_count}/{failure_threshold} ({kind})"
        );
        ProbeOutcome::ModelSpecificCounted
    }
}

/// FIX 1: esito puro della valutazione di una response di tool-probe.
/// Testabile senza DB/rete.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ToolProbeVerdict {
    /// Tool call valida per `nexus_probe_tool` ricevuta.
    Success,
    /// Tool-forcing fallito (no tool call / malformed / output vuoto). Il
    /// modello e' raggiungibile ma non sa rispettare il tool-forcing.
    ToolFailed(String),
    /// Errore provider-wide (billing/quota/auth/rate_limit): non e' colpa del
    /// modello. Niente azione tool-specific.
    ProviderWide(String),
    /// Esito INCONCLUSIVO: errore opaco/transitorio (error_class generico,
    /// stop_reason='error' senza causa nota, timeout, 5xx gateway). NON degrada
    /// supports_tool_use, NON incrementa consecutive_tool_failures, NON sposta
    /// il backoff. Si ritenta al round successivo.
    Transient(String),
}

/// Mappa un error_class canonico sul verdetto del tool-probe (punto unico del
/// mapping Classification -> ToolProbeVerdict, regola L). `ok_fallback` e' il
/// `kind` usato per il caso — non atteso — di error_class classificato Ok.
fn verdict_from_error_class(ec: &str, ok_fallback: &str) -> ToolProbeVerdict {
    match classification_from_error_class(ec) {
        Classification::ProviderWide(kind, _) => ToolProbeVerdict::ProviderWide(kind),
        Classification::ModelSpecific(kind, _) => ToolProbeVerdict::ToolFailed(kind),
        Classification::Transient(kind, _) => ToolProbeVerdict::Transient(kind),
        Classification::Ok => ToolProbeVerdict::ToolFailed(ok_fallback.into()),
    }
}

/// Valuta la response JSON del brain (`GenerateAgentTurn`) per il tool-probe.
/// Logica:
/// - error_class provider-wide -> ProviderWide (non punire il modello).
/// - error_class model-specific / forbidden / not_found -> ToolFailed.
/// - presenza di un `tool_use_blocks` con name == nexus_probe_tool -> Success.
/// - altrimenti (stop_reason=error, malformed, nessuna tool call) -> ToolFailed.
pub(crate) fn evaluate_tool_probe(response: &serde_json::Value) -> ToolProbeVerdict {
    // 1. error_class autorevole dal brain.
    let ec = response
        .get("error_class")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    if !ec.is_empty() {
        return verdict_from_error_class(ec, "unexpected_ok_with_error_class");
    }

    // 2. stop_reason=error senza error_class: INCONCLUSIVO, non tool-failure.
    //    Prima questo ramo marciava verso il degrado di supports_tool_use anche
    //    quando l'errore era un blip generico al confine gateway/provider
    //    (incidente Gemini: tool-probe e chat-probe accoppiati, entrambi
    //    error_kind='error', NON dipendenti dai tool). Senza un error_class
    //    model-specific riconosciuto non possiamo attribuire il guasto al
    //    modello: Transient, stato invariato, ritento al round successivo.
    let stop_reason = response
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if stop_reason == "error" {
        return ToolProbeVerdict::Transient("stop_reason_error".into());
    }

    // 3. Tool call valida verso nexus_probe_tool?
    let has_valid_tool_call = response
        .get("tool_use_blocks")
        .and_then(|v| v.as_array())
        .map(|blocks| {
            blocks
                .iter()
                .any(|b| b.get("name").and_then(|n| n.as_str()) == Some(TOOL_PROBE_TOOL_NAME))
        })
        .unwrap_or(false);
    if has_valid_tool_call {
        return ToolProbeVerdict::Success;
    }

    // 4. Nessuna tool call (output vuoto / malformed / il modello ha chiacchierato
    //    invece di chiamare il tool forzato).
    ToolProbeVerdict::ToolFailed(if stop_reason.is_empty() {
        "no_tool_call".into()
    } else {
        format!("no_tool_call:{stop_reason}")
    })
}

/// Costruisce la richiesta di tool-probe: ritorna `(tools_json, messages_json,
/// system_text)`. Tool fittizio minimale + tool_choice forzato via messaggio: lo
/// schema generate_agent_turn non ha un campo tool_choice dedicato, ma i provider
/// OpenAI-compatible accettano la forzatura via messaggio + presenza tool.
fn build_tool_probe_request() -> (String, String, String) {
    let tools_json = serde_json::json!([
        {
            "name": TOOL_PROBE_TOOL_NAME,
            "description": "Tool di verifica capacita: rispondi chiamandolo con ok=true.",
            "input_schema": {
                "type": "object",
                "properties": { "ok": { "type": "boolean" } },
                "required": ["ok"]
            }
        }
    ])
    .to_string();
    let messages_json = serde_json::json!([
        {
            "role": "user",
            "content": format!("Verifica capacita tool: chiama {TOOL_PROBE_TOOL_NAME} con ok=true.")
        }
    ])
    .to_string();
    let system_text =
        format!("Devi rispondere ESCLUSIVAMENTE chiamando il tool {TOOL_PROBE_TOOL_NAME}.");
    (tools_json, messages_json, system_text)
}

/// FIX 1: esegue il tool-probe su un singolo modello tool-capable, sul PATH
/// AGENTE (`generate_agent_turn`), forzando una tool call su un tool fittizio.
/// Applica la stessa semantica del runtime (`tool_failure_action`): a soglia
/// marca `supports_tool_use=false` SENZA toccare `is_enabled`.
/// Esegue il tool-probe (chiamata `generate_agent_turn` con tool-forcing) e
/// ritorna SOLO il verdetto, SENZA side-effect su DB/contatori. Punto unico
/// (regola L) della costruzione della richiesta di tool-probe e della latenza:
/// sia `tool_probe_one_model` (che applica counter/degrado) sia il re-probe dei
/// candidati disabilitati (che riabilita is_enabled) lo riusano, senza
/// duplicare tools_json / messages_json / mapping errore->verdetto.
async fn run_tool_probe(
    orchestrator: &Orchestrator,
    provider: &str,
    model: &str,
) -> (ToolProbeVerdict, i32) {
    let (tools_json, messages_json, system_text) = build_tool_probe_request();

    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(PROBE_TIMEOUT_S),
        orchestrator.neural.generate_agent_turn(
            provider,
            model,
            &messages_json,
            &tools_json,
            TOOL_PROBE_MAX_TOKENS,
            &system_text,
        ),
    )
    .await;
    let latency_ms = started.elapsed().as_millis() as i32;

    let verdict = match result {
        Ok(Ok(response)) => evaluate_tool_probe(&response),
        Ok(Err(e)) => {
            let ec = orchestrator
                .neural
                .classify_error(&e.to_string(), provider)
                .await;
            verdict_from_error_class(&ec, "transport_ok_no_tool")
        }
        // Timeout del tool-probe: INCONCLUSIVO (cold-start / coda provider), non
        // un guasto del tool-forcing del modello.
        Err(_timeout) => ToolProbeVerdict::Transient("tool_probe_timeout".into()),
    };
    (verdict, latency_ms)
}

async fn tool_probe_one_model(
    orchestrator: &Orchestrator,
    db: &PgPool,
    provider: &str,
    model: &str,
    threshold: i32,
) -> ToolProbeOutcome {
    let (verdict, latency_ms) = run_tool_probe(orchestrator, provider, model).await;

    persist_tool_probe_history(db, provider, model, &verdict, latency_ms).await;

    match verdict {
        ToolProbeVerdict::ProviderWide(kind) => {
            tracing::debug!(
                "model_health_probe[tool]: {provider}/{model} provider-wide '{kind}' -> nessuna azione tool-specific"
            );
            ToolProbeOutcome::Skipped
        }
        ToolProbeVerdict::Transient(kind) => {
            // INCONCLUSIVO: niente degrado di supports_tool_use, niente
            // incremento di consecutive_tool_failures. Stato invariato, si
            // ritenta al round successivo (regola H).
            tracing::debug!(
                "model_health_probe[tool]: {provider}/{model} esito inconclusivo ({kind}) -> stato tool invariato"
            );
            ToolProbeOutcome::Transient
        }
        ToolProbeVerdict::Success => {
            // PUNTO UNICO (regola L): reset contatore + riabilitazione della
            // tool-capability se il degrado era automatico, da QUALUNQUE fonte
            // (tool_probe_failed:% O malformed_tool_calls dal runtime). Le
            // curature admin e is_enabled non vengono toccati.
            crate::tool_capability::reset_tool_failures_on_success(db, provider, model, false)
                .await;
            tracing::debug!("model_health_probe[tool]: {provider}/{model} tool-probe OK");
            ToolProbeOutcome::Ok
        }
        ToolProbeVerdict::ToolFailed(kind) => {
            record_tool_probe_failure(db, provider, model, threshold, &kind).await
        }
    }
}

/// Persiste l'esito del tool-probe nello storico (diagnosticabile). error_kind
/// prefissato per distinguerlo dal probe chat ('tool_probe:' fallimento,
/// 'tool_probe_provider:' provider-wide, 'tool_probe_transient:' inconclusivo).
async fn persist_tool_probe_history(
    db: &PgPool,
    provider: &str,
    model: &str,
    verdict: &ToolProbeVerdict,
    latency_ms: i32,
) {
    let (healthy, error_kind) = match verdict {
        ToolProbeVerdict::Success => (true, None),
        ToolProbeVerdict::ToolFailed(k) => (false, Some(format!("tool_probe:{k}"))),
        ToolProbeVerdict::ProviderWide(k) => (false, Some(format!("tool_probe_provider:{k}"))),
        ToolProbeVerdict::Transient(k) => (false, Some(format!("tool_probe_transient:{k}"))),
    };
    let _ = sqlx::query(
        r#"INSERT INTO ai_model_health_history
           (provider, model, healthy, latency_ms, error_kind, error_message)
           VALUES ($1, $2, $3, $4, $5, $6)"#,
    )
    .bind(provider)
    .bind(model)
    .bind(healthy)
    .bind(latency_ms)
    .bind(error_kind.as_deref())
    .bind(error_kind.as_deref())
    .execute(db)
    .await;
}

/// Registra un fallimento del tool-probe delegando a `tool_capability` (PUNTO
/// UNICO, regola L): counter + degrado a soglia con guard capability_source=
/// 'auto', enforcement del 'manual' unico in tutto il sistema (probe E runtime).
async fn record_tool_probe_failure(
    db: &PgPool,
    provider: &str,
    model: &str,
    threshold: i32,
    kind: &str,
) -> ToolProbeOutcome {
    let reason = format!("{}{kind}", crate::tool_capability::REASON_TOOL_PROBE_PREFIX);
    match crate::tool_capability::record_tool_failure(db, provider, model, threshold, &reason).await
    {
        crate::tool_capability::ToolFailureRecord::MarkedNonToolCapable { .. } => {
            ToolProbeOutcome::MarkedNonToolCapable
        }
        crate::tool_capability::ToolFailureRecord::Counted { failures } => {
            tracing::debug!(
                "model_health_probe[tool]: {provider}/{model} tool-fail #{failures}/{threshold} ({kind})"
            );
            ToolProbeOutcome::FailedCounted
        }
        crate::tool_capability::ToolFailureRecord::Protected => {
            tracing::debug!(
                "model_health_probe[tool]: {provider}/{model} riga manual — degrado non applicato ({kind})"
            );
            ToolProbeOutcome::FailedCounted
        }
    }
}

/// Applica il cooldown di provider corrispondente al `kind` rilevato dal
/// classificatore per un errore "provider-wide". Centralizzata qui per
/// evitare duplicazione e per garantire mapping consistente fra
/// `provider_health_probe` e `model_health_probe`.
///
/// Mappatura (allineata alle policy in `provider_cooldown`):
/// - `quota_exceeded` / `credit_balance_too_low` / `billing_required` /
///   `auth_error` -> cooldown lungo (6h, persistito su Redis + TTL su
///   nexus_provider_health): servono intervento manuale (ricarica credito,
///   fix billing, rotazione chiave) e ritentare ogni pochi minuti e' inutile
///   spreco di chiamate.
/// - `rate_limit` -> cooldown breve (60s): il provider torna disponibile
///   quasi subito; allinea il behaviour ai retry-after tipici di OpenAI.
/// - `connection_error` -> cooldown breve (5 min): rete/DNS transient,
///   non vale la pena escludere il provider per ore.
/// - altri `kind` -> nessun cooldown: lasciamo che il chiamante decida
///   (default conservativo per non sopprimere provider per cause ignote).
async fn apply_provider_wide_cooldown(provider: &str, model: &str, kind: &str) {
    // Errori persistenti (billing/quota/credito/auth): mettono il provider in
    // cooldown logico in-memory + TTL persistente su nexus_provider_health
    // (scritto da put_provider_in_long_cooldown, la fonte GIUSTA perche' ha
    // scadenza). NON disabilitano piu' il catalog/matrix: is_enabled significa
    // "il modello e' valido", non "ora senza credito". Il routing salta i
    // provider in cooldown via is_provider_in_cooldown/cooldown_snapshot senza
    // bisogno di is_enabled=false. La billing_cooldown_recovery_loop riprova il
    // provider (probe-then-reenable) e azzera il TTL quando il credito torna.
    // I transienti (rate_limit/connection) restano solo in-memory: tornano da
    // soli in pochi secondi.
    // Log identico per tutti i kind con cooldown automatico: emesso una volta
    // qui, ogni arm calcola solo l'azione (long/short cooldown). Il kind ignoto
    // (nessun cooldown) e' l'unico caso a parte, con log debug.
    // Errori persistenti -> long cooldown (differiscono solo per il messaggio).
    let long_msg = match kind {
        "quota_exceeded" => Some("Quota provider esaurita (HTTP 429)"),
        "credit_balance_too_low" => Some("Credito provider insufficiente"),
        "billing_required" => Some("Billing provider non configurato"),
        "auth_error" => Some("API key non valida"),
        _ => None,
    };
    // Transienti -> short cooldown (messaggio + timing dedicati).
    let timings = crate::provider_cooldown::provider_health_timings();
    let short = match kind {
        "rate_limit" => Some(("Rate limit raggiunto", timings.slow_cooldown_s)),
        "connection_error" => Some(("Provider non raggiungibile", timings.cooldown_default_s)),
        _ => None,
    };
    let known = if let Some(msg) = long_msg {
        put_provider_in_long_cooldown(provider, msg);
        true
    } else if let Some((msg, secs)) = short {
        put_provider_in_short_cooldown(provider, msg, secs);
        true
    } else {
        false
    };
    if known {
        tracing::info!(
            "model_health_probe: provider {provider} messo in cooldown per {kind} (rilevato in probe model {model})"
        );
    } else {
        tracing::debug!(
            "model_health_probe: provider {provider} errore provider-wide '{kind}' senza cooldown automatico (rilevato in probe model {model})"
        );
    }
}

pub(crate) enum Classification {
    Ok,
    /// Errore che riguarda il provider intero (non punisce il modello).
    ProviderWide(String, Option<String>),
    /// Errore specifico del modello (incrementa il counter).
    ModelSpecific(String, Option<String>),
    /// Esito INCONCLUSIVO: errore opaco/transitorio al confine gateway/provider
    /// (cold-start auth Vertex, 5xx generico, timeout di rete, error_class
    /// generico `error`/`unknown`/`provider_error`, stop_reason='error' senza
    /// causa model-specific riconosciuta). NON e' una prova ne' che il modello
    /// sia rotto ne' che il provider sia giu' in modo persistente: il verdetto
    /// non incrementa alcun contatore, non scrive is_enabled=false, non degrada
    /// supports_tool_use e non sposta il backoff del re-probe. Lo stato resta
    /// invariato e si ritenta al round successivo. Replica esattamente il
    /// pattern di `ProviderWide` (non punisce il modello), ma a differenza di
    /// quello NON applica nemmeno un cooldown provider: l'errore e' troppo
    /// generico per attribuirlo al provider con certezza (regola H: niente
    /// auto-disable su sintomi opachi).
    Transient(String, Option<String>),
}

/// Risultato sintetico del probe per uso in `catalog_sync::probe_on_insert`
/// (decide se abilitare un modello appena scoperto dall'API discovery).
/// Non ha side-effects sul DB — il chiamante decide cosa fare.
pub(crate) enum ProbeOnInsertResult {
    /// Modello risponde correttamente — `is_enabled=true` sicuro.
    Healthy,
    /// Errore specifico del modello (404/invalid/hollow) — `is_enabled=false`
    /// con `auto_disabled_reason='failed_initial_probe:<kind>'`.
    ModelBroken(String),
    /// Errore provider-wide (quota/billing/auth/rate_limit) — non sappiamo
    /// se il modello e' broken o solo il provider. Inseriamo `is_enabled=false`
    /// con motivazione, sara' il `model_health_probe` worker a riabilitarlo
    /// quando il provider torna up E il probe passa.
    ProviderDown(String),
    /// Probe non eseguibile (timeout, exception client) — `is_enabled=false`
    /// conservativo, sara' rivalutato al prossimo round del worker probe.
    Inconclusive(String),
    /// Esito INCONCLUSIVO da errore opaco/transitorio (error_class generico,
    /// 5xx gateway, timeout di rete). Come `Inconclusive` per il catalog_sync
    /// (modello appena scoperto: resta disabilitato in attesa del probe
    /// periodico), ma nel RE-PROBE NON deve spostare il backoff: e' un blip,
    /// non un guasto del modello. Distinto da `Inconclusive` proprio per
    /// permettere ai chiamanti di trattarlo senza penalizzare il candidato.
    Transient(String),
}

/// Probe sincrono di un singolo modello, usato da `catalog_sync` al momento
/// dell'INSERT per decidere se abilitare il modello appena scoperto.
/// A differenza di `probe_one_model`, NON aggiorna contatori ne' fa cooldown:
/// decide solo se il modello risponde e ritorna il risultato.
pub(crate) async fn probe_model_on_insert(
    orchestrator: &Orchestrator,
    provider: &str,
    model: &str,
) -> ProbeOnInsertResult {
    let result = tokio::time::timeout(
        Duration::from_secs(PROBE_TIMEOUT_S),
        orchestrator
            .neural
            .generate_completion(provider, model, PROBE_PROMPT),
    )
    .await;
    match result {
        Ok(Ok(response)) => {
            let trimmed = extract_content_text(&response).trim().to_string();
            if trimmed.is_empty() {
                return ProbeOnInsertResult::ModelBroken("hollow_completion".to_string());
            }
            if trimmed.starts_with("[Error:") || trimmed.starts_with("[error:") {
                let inner = trimmed
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .trim_start_matches("Error:")
                    .trim_start_matches("error:")
                    .trim();
                let ec = orchestrator.neural.classify_error(inner, provider).await;
                on_insert_from_classification(classification_from_error_class(&ec))
            } else {
                ProbeOnInsertResult::Healthy
            }
        }
        Ok(Err(e)) => {
            let ec = orchestrator
                .neural
                .classify_error(&e.to_string(), provider)
                .await;
            on_insert_from_classification(classification_from_error_class(&ec))
        }
        Err(_timeout) => ProbeOnInsertResult::Inconclusive(format!("timeout {PROBE_TIMEOUT_S}s")),
    }
}

/// Mappa una `Classification` sul risultato del probe-on-insert (punto unico del
/// mapping, regola L): riusato dal ramo response-`[Error:` e dal ramo trasporto.
fn on_insert_from_classification(c: Classification) -> ProbeOnInsertResult {
    match c {
        Classification::Ok => ProbeOnInsertResult::Healthy,
        Classification::ProviderWide(kind, _) => ProbeOnInsertResult::ProviderDown(kind),
        Classification::ModelSpecific(kind, _) => ProbeOnInsertResult::ModelBroken(kind),
        Classification::Transient(kind, _) => ProbeOnInsertResult::Transient(kind),
    }
}

/// Mappa l'error_class CANONICO (prodotto dal brain via il RPC ClassifyError,
/// unico classificatore del sistema) verso la Classification del probe. NON
/// contiene pattern di testo: e' solo una tabella valore->azione. mcp-core non
/// classifica piu' messaggi d'errore in proprio.
pub(crate) fn classification_from_error_class(ec: &str) -> Classification {
    match ec {
        // Persistenti -> long cooldown + disable provider
        "billing_error" => {
            Classification::ProviderWide("credit_balance_too_low".into(), Some(ec.into()))
        }
        // 401: credenziali invalide -> tutto il provider e' inutilizzabile.
        "auth_error" => Classification::ProviderWide("auth_error".into(), Some(ec.into())),
        // 403 forbidden -> NON e' un problema di credenziali ma di accesso a
        // quel modello/risorsa (es. Mistral 403 labs_not_enabled: modello Labs
        // non abilitato nell'org). E' model-specific come not_found: si
        // disabilita/conteggia il singolo modello, non si spegne l'intero
        // provider con long cooldown 6h.
        "forbidden" => Classification::ModelSpecific("model_forbidden".into(), Some(ec.into())),
        // Transienti CHIARAMENTE provider-wide (segnale netto di provider giu' o
        // throttling) -> short cooldown utile: il modello non viene punito.
        "rate_limit"
        | "overloaded"
        | "service_unavailable"
        | "bad_gateway"
        | "timeout"
        | "connection_error" => Classification::ProviderWide("rate_limit".into(), Some(ec.into())),
        // Model-specific -> conteggio/auto-disable del modello. Sono le SOLE
        // cause davvero attribuibili al modello: 404 (modello inesistente),
        // contesto troppo lungo, richiesta invalida, capability non supportata.
        "not_found" | "invalid_model" => {
            Classification::ModelSpecific("invalid_model".into(), Some(ec.into()))
        }
        "context_too_long" | "invalid_request" | "unprocessable" | "unsupported" => {
            Classification::ModelSpecific(ec.into(), Some(ec.into()))
        }
        "" | "ok" => Classification::Ok,
        // INCONCLUSIVO (regola H, causa-non-sintomo): `provider_error` (500
        // generico), `error`/`unknown` (fallback del classificatore) e QUALUNQUE
        // altro error_class non riconosciuto sono opachi. Trattarli come
        // ModelSpecific (vecchio catch-all) auto-disabilitava modelli sani per
        // cold-start auth Vertex / 5xx gateway / blip di rete. Diventano
        // Transient: stato invariato, ritento al round successivo.
        "provider_error" | "error" | "unknown" => {
            Classification::Transient(ec.into(), Some(ec.into()))
        }
        other => Classification::Transient(other.into(), Some(other.into())),
    }
}

/// Estrae il testo del content dalla response in vari formati provider.
/// Versione "text" usata per pattern-match su "[Error:".
fn extract_content_text(value: &serde_json::Value) -> String {
    if let Some(s) = value.get("content").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(arr) = value.get("choices").and_then(|v| v.as_array()) {
        if let Some(first) = arr.first() {
            if let Some(s) = first
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                return s.to_string();
            }
        }
    }
    if let Some(candidates) = value.get("candidates").and_then(|v| v.as_array()) {
        if let Some(first) = candidates.first() {
            if let Some(parts) = first
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
            {
                let mut buf = String::new();
                for p in parts {
                    if let Some(s) = p.get("text").and_then(|t| t.as_str()) {
                        buf.push_str(s);
                    }
                }
                return buf;
            }
        }
    }
    String::new()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_not_found_da_error_class() {
        let c = classification_from_error_class("not_found");
        assert!(matches!(c, Classification::ModelSpecific(ref k, _) if k == "model_not_found"));
    }

    #[test]
    fn classification_billing_da_error_class() {
        let c = classification_from_error_class("billing_error");
        assert!(
            matches!(c, Classification::ProviderWide(ref k, _) if k == "credit_balance_too_low")
        );
    }

    #[test]
    fn classification_auth_error_e_providerwide() {
        // 401 credenziali invalide -> tutto il provider e' down.
        let c = classification_from_error_class("auth_error");
        assert!(matches!(c, Classification::ProviderWide(ref k, _) if k == "auth_error"));
    }

    #[test]
    fn classification_forbidden_e_model_specific() {
        // Regressione: 403 forbidden (es. Mistral labs_not_enabled) deve essere
        // model-specific, NON provider-wide: non deve spegnere l'intero provider.
        let c = classification_from_error_class("forbidden");
        assert!(matches!(c, Classification::ModelSpecific(ref k, _) if k == "model_forbidden"));
    }

    #[test]
    fn classification_rate_limit_da_error_class() {
        let c = classification_from_error_class("rate_limit");
        assert!(matches!(c, Classification::ProviderWide(ref k, _) if k == "rate_limit"));
    }

    // --- TRANSIENT: errori opachi/transitori NON puniscono il modello --------

    #[test]
    fn classification_error_generico_e_transient() {
        // error_class 'error' (fallback del classificatore) -> Transient.
        // Regressione incidente Gemini: prima cadeva nel catch-all e diventava
        // ModelSpecific -> consecutive_failures cresceva -> auto-disable a 3.
        let c = classification_from_error_class("error");
        assert!(matches!(c, Classification::Transient(ref k, _) if k == "error"));
    }

    #[test]
    fn classification_unknown_e_provider_error_sono_transient() {
        // 'unknown' e 'provider_error' (500 generico) sono opachi: inconclusivi,
        // non attribuibili al modello.
        assert!(matches!(
            classification_from_error_class("unknown"),
            Classification::Transient(ref k, _) if k == "unknown"
        ));
        assert!(matches!(
            classification_from_error_class("provider_error"),
            Classification::Transient(ref k, _) if k == "provider_error"
        ));
    }

    #[test]
    fn classification_catch_all_sconosciuto_e_transient() {
        // Qualunque error_class non riconosciuto: Transient (mai ModelSpecific).
        // Il catch-all NON deve piu' marciare verso l'auto-disable.
        let c = classification_from_error_class("qualcosa_di_mai_visto");
        assert!(matches!(c, Classification::Transient(ref k, _) if k == "qualcosa_di_mai_visto"));
    }

    #[test]
    fn classification_model_specific_restano_punitive() {
        // Le SOLE cause davvero model-specific continuano a degradare il modello:
        // not_found, context_too_long, invalid_request, unprocessable, unsupported.
        assert!(matches!(
            classification_from_error_class("not_found"),
            Classification::ModelSpecific(ref k, _) if k == "model_not_found"
        ));
        for ec in [
            "context_too_long",
            "invalid_request",
            "unprocessable",
            "unsupported",
        ] {
            assert!(
                matches!(classification_from_error_class(ec), Classification::ModelSpecific(ref k, _) if k == ec),
                "{ec} doveva restare ModelSpecific",
            );
        }
    }

    #[test]
    fn tool_probe_transient_su_stop_reason_error() {
        // stop_reason='error' senza error_class model-specific riconosciuto:
        // INCONCLUSIVO, NON tool-failure. Prima era ToolFailed -> degradava
        // supports_tool_use anche su un blip generico (incidente Gemini: il
        // tool-probe e il chat-probe fallivano accoppiati con error_kind='error',
        // a riprova che NON dipendeva dai tool).
        let v = serde_json::json!({ "stop_reason": "error", "tool_use_blocks": [] });
        assert!(matches!(
            evaluate_tool_probe(&v),
            ToolProbeVerdict::Transient(ref k) if k == "stop_reason_error"
        ));
    }

    #[test]
    fn tool_probe_transient_su_error_class_generico() {
        // error_class 'error' nel tool-probe -> Transient (non ToolFailed):
        // non degrada la tool-capability.
        let v = serde_json::json!({ "error_class": "error", "tool_use_blocks": [] });
        assert!(matches!(
            evaluate_tool_probe(&v),
            ToolProbeVerdict::Transient(ref k) if k == "error"
        ));
    }

    #[test]
    fn tool_probe_model_specific_restano_tool_failed() {
        // not_found resta ToolFailed (degrada): solo i transient sono inconclusivi.
        let v = serde_json::json!({ "error_class": "not_found", "tool_use_blocks": [] });
        assert!(matches!(
            evaluate_tool_probe(&v),
            ToolProbeVerdict::ToolFailed(ref k) if k == "model_not_found"
        ));
    }

    #[test]
    fn extract_content_anthropic() {
        let v = serde_json::json!({"content": "Hello!"});
        assert_eq!(extract_content_text(&v), "Hello!");
    }

    #[test]
    fn extract_content_openai() {
        let v = serde_json::json!({
            "choices": [{ "message": { "content": "Hi there" } }]
        });
        assert_eq!(extract_content_text(&v), "Hi there");
    }

    #[test]
    fn extract_content_gemini() {
        let v = serde_json::json!({
            "candidates": [{
                "content": { "parts": [{ "text": "hello" }, { "text": "!" }] }
            }]
        });
        assert_eq!(extract_content_text(&v), "hello!");
    }

    #[test]
    fn extract_content_empty() {
        let v = serde_json::json!({"content": ""});
        assert_eq!(extract_content_text(&v), "");
    }

    // --- FIX 1: tool-probe verdict ------------------------------------------

    #[test]
    fn tool_probe_success_su_tool_call_valida() {
        // Il modello ha chiamato nexus_probe_tool -> Success.
        let v = serde_json::json!({
            "stop_reason": "tool_use",
            "tool_use_blocks": [{ "name": TOOL_PROBE_TOOL_NAME, "input": {"ok": true} }]
        });
        assert_eq!(evaluate_tool_probe(&v), ToolProbeVerdict::Success);
    }

    #[test]
    fn tool_probe_fail_su_nessuna_tool_call() {
        // Il modello ha risposto in chat senza chiamare il tool forzato
        // (tipico hollow_no_tools / output-vuoto sul tool-forcing).
        let v = serde_json::json!({
            "stop_reason": "end_turn",
            "content": "Certo, procedo.",
            "tool_use_blocks": []
        });
        assert!(matches!(
            evaluate_tool_probe(&v),
            ToolProbeVerdict::ToolFailed(ref k) if k.contains("no_tool_call")
        ));
    }

    #[test]
    fn tool_probe_fail_su_forbidden_model_specific() {
        // 403 forbidden (es. alias Mistral -> Labs 403) e' model-specific:
        // il tool-probe lo conta come ToolFailed, NON provider-wide.
        let v = serde_json::json!({ "error_class": "forbidden", "tool_use_blocks": [] });
        assert!(matches!(
            evaluate_tool_probe(&v),
            ToolProbeVerdict::ToolFailed(ref k) if k == "model_forbidden"
        ));
    }

    #[test]
    fn tool_probe_providerwide_su_billing() {
        // billing_error e' colpa del provider: non punisce il modello.
        let v = serde_json::json!({ "error_class": "billing_error", "tool_use_blocks": [] });
        assert!(matches!(
            evaluate_tool_probe(&v),
            ToolProbeVerdict::ProviderWide(ref k) if k == "credit_balance_too_low"
        ));
    }

    // NB: il caso stop_reason='error' e' ora coperto da
    // `tool_probe_transient_su_stop_reason_error` (verdetto Transient, non
    // ToolFailed): l'errore generico al confine gateway non degrada piu' il
    // modello.

    // --- RE-PROBE candidati disabilitati per quirk gateway ------------------

    #[test]
    fn reprobe_candidate_tool_probe_failed_incluso() {
        // (a) Modello disabilitato per tool_probe_failed: e' un quirk gateway
        // ri-testabile -> candidato.
        assert!(is_reprobe_candidate(
            false,
            "auto",
            Some("tool_probe_failed:error")
        ));
        // malformed_tool_calls (degrado runtime) e' lo stesso ciclo -> candidato.
        assert!(is_reprobe_candidate(
            false,
            "auto",
            Some("malformed_tool_calls")
        ));
    }

    #[test]
    fn reprobe_candidate_billing_missing_policy_esclusi() {
        // (b) Billing/quota -> cooldown, gestito altrove: NON ri-probato cosi'.
        assert!(!is_reprobe_candidate(
            false,
            "auto",
            Some("credit_balance_too_low")
        ));
        // missing_from_api -> il modello non esiste piu' nell'API: escluso.
        assert!(!is_reprobe_candidate(
            false,
            "auto",
            Some("missing_from_api")
        ));
        // %policy% -> decisione amministrativa, non un guasto: escluso.
        assert!(!is_reprobe_candidate(
            false,
            "auto",
            Some("fuori model_selection_policy (mig 0320)")
        ));
        // hollow_completion generico (ciclo is_enabled, non tool): escluso.
        assert!(!is_reprobe_candidate(
            false,
            "auto",
            Some("hollow_completion_runtime")
        ));
    }

    #[test]
    fn reprobe_candidate_lock_manuale_escluso() {
        // (c) Lock manuale: mai ri-probato in automatico, da entrambi i segnali.
        assert!(!is_reprobe_candidate(
            false,
            "manual",
            Some("tool_probe_failed:error")
        ));
        assert!(!is_reprobe_candidate(
            false,
            "auto",
            Some("manual:non_chat_endpoint")
        ));
    }

    #[test]
    fn reprobe_candidate_modello_abilitato_escluso() {
        // Un modello gia' abilitato non e' candidato (lo testa il giro principale).
        assert!(!is_reprobe_candidate(
            true,
            "auto",
            Some("tool_probe_failed:error")
        ));
        // Reason assente: non e' un degrado tracciato, niente re-probe.
        assert!(!is_reprobe_candidate(false, "auto", None));
    }

    #[test]
    fn reprobe_due_rispetta_il_backoff() {
        let now = chrono::Utc::now();
        let backoff = 30;
        // Mai marcato (NULL) -> eleggibile subito.
        assert!(reprobe_due(None, now, backoff));
        // Tentativo 10 min fa, backoff 30 min -> non ancora.
        assert!(!reprobe_due(
            Some(now - chrono::Duration::minutes(10)),
            now,
            backoff
        ));
        // Tentativo 31 min fa -> oltre il backoff, eleggibile.
        assert!(reprobe_due(
            Some(now - chrono::Duration::minutes(31)),
            now,
            backoff
        ));
    }

    #[test]
    fn run_error_ambientale_riconosce_le_categorie_provider_wide() {
        assert!(run_error_is_environmental(Some("context_overflow")));
        assert!(run_error_is_environmental(Some("billing_error")));
        assert!(run_error_is_environmental(Some("insufficient_quota")));
        assert!(run_error_is_environmental(Some("infrastructure")));
        assert!(run_error_is_environmental(Some("rate_limit")));
        // Non ambientali (colpa modello o sconosciuto specifico) / vuoti.
        assert!(!run_error_is_environmental(Some("model_not_found")));
        assert!(!run_error_is_environmental(Some("")));
        assert!(!run_error_is_environmental(None));
    }

    #[test]
    fn run_outcome_blames_model_distingue_modello_da_ambiente() {
        use crate::agent_types::AgentRunStatus as S;
        // Colpa modello: fallito/loop/diagnosed, non hollow, error_class non ambientale.
        assert!(run_outcome_blames_model(
            S::FailedDiagnosed,
            None,
            false,
            false
        ));
        assert!(run_outcome_blames_model(
            S::Failed,
            Some("model_not_found"),
            false,
            false
        ));
        assert!(run_outcome_blames_model(S::LoopAborted, None, false, false));
        // Ambiente: blocco reale, provider giu', errore ambientale -> NON colpa modello.
        assert!(!run_outcome_blames_model(
            S::BlockedNeedsInput,
            None,
            false,
            false
        ));
        assert!(!run_outcome_blames_model(
            S::ProviderUnavailable,
            None,
            false,
            false
        ));
        assert!(!run_outcome_blames_model(
            S::FailedDiagnosed,
            Some("context_overflow"),
            false,
            false
        ));
        // Hollow: gia' contato dai contatori dedicati, non doppiare.
        assert!(!run_outcome_blames_model(
            S::FailedDiagnosed,
            None,
            true,
            false
        ));
        assert!(!run_outcome_blames_model(S::Failed, None, false, true));
        // Successo: mai colpa.
        assert!(!run_outcome_blames_model(
            S::CompletedVerified,
            None,
            false,
            false
        ));
    }
}
