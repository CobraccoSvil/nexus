//! Qualificazione EMPIRICA dei modelli (fase 4 del gate, mig 0591/0593).
//!
//! Il catalog DICHIARA (capabilities scritte da sync/migrazioni/admin); questo
//! modulo PROVA: esegue sulla riga candidata la batteria di profili di
//! `ai_model_probe_profile` (configurazione, regola G) con richieste REALI
//! (schemi tool veri, system prompt veri), registra ogni tentativo in
//! `ai_model_probe_evidence` (segnali strutturati, regola M) e deriva
//! `qualification_state` + `qualified_capabilities` col punto unico PURO
//! [`derive_capabilities`] (regola L). E' l'UNICO writer della promozione a
//! `qualified` (il CHECK `chk_qualified_implies_probe` blocca chiunque altro).
//!
//! Root cause coperta (incidenti 2026-07-14/15): un modello entrava nel
//! routing agentico sulla sola parola del catalog; la prima richiesta di
//! produzione faceva da probe e la pagavano le figure del consiglio.
//!
//! Prudenza ereditata dal probe (regola H): un esito TRANSIENT/provider-wide
//! non e' MAI punitivo — il giro e' inconclusivo, backoff e si ritenta. Solo
//! un fallimento MODEL-SPECIFIC conclusivo (es. `empty_completion`) squalifica.

use std::time::Duration;

use serde_json::{json, Value};
use sqlx::{PgPool, Row};

use crate::orchestrator::Orchestrator;

/// Chiavi settings (mig 0591/0593, regola G).
const KEY_ROUND_ENABLED: &str = "agent.model_qualification.round_enabled";
const KEY_MAX_PER_ROUND: &str = "agent.model_qualification.max_models_per_round";
const KEY_TTL_DAYS: &str = "agent.model_qualification.requalify_ttl_days";
const KEY_BACKOFF_HOURS: &str = "agent.model_qualification.backoff_hours";

/// Cap del backoff esponenziale (7 giorni): oltre non ha senso attendere di piu'.
const BACKOFF_CAP_HOURS: i64 = 168;
/// Lock `probing` stantio: oltre questa eta' il claim e' di un worker morto.
const STALE_PROBING_MINUTES: i64 = 15;

/// Capability MISURATE dalla suite v1 (P0 chat + P2 agentic): vengono SOLO dai
/// `grants` dei profili superati. Le altre (es. `reasoning`, `vision`) non sono
/// ancora misurate direttamente: vengono EREDITATE dal dichiarato SOLO quando
/// l'intera batteria bloccante passa (il modello regge il carico agentico
/// reale). La misura diretta del reasoning arriva con `thinking_matrix`
/// (fase 5): finche' non c'e', un'eredita' condizionata al probe superato e'
/// il compromesso DICHIARATO — mai un'eredita' incondizionata.
const MEASURED_V1: [&str; 2] = ["chat", "code"];

/// Un profilo della batteria (riga di `ai_model_probe_profile`).
#[derive(Debug, Clone)]
pub(crate) struct ProbeProfile {
    pub profile_key: String,
    pub suite_version: i32,
    pub kind: String,
    pub is_blocking: bool,
    pub applies_when: Option<Value>,
    pub grants: Vec<String>,
    pub payload: Value,
    pub pass_predicate: Value,
}

/// Esito STRUTTURATO di UN tentativo (regola M): deriva da error_class /
/// stop_reason / tool_use_blocks / result, mai dalla prosa.
#[derive(Debug, Clone)]
pub(crate) struct AttemptOutcome {
    pub pass: bool,
    /// `true` = esito non attribuibile al modello (transient, provider-wide,
    /// timeout): non conta ne' come pass ne' come fail conclusivo.
    pub inconclusive: bool,
    pub reason: String,
    pub error_class: Option<String>,
    pub tool_call_count: i64,
    pub content_chars: i64,
    pub stop_reason: String,
}

/// Valuta UN turno di probe contro il `pass_predicate` del profilo. PURA.
pub(crate) fn evaluate_attempt(turn: &Value, predicate: &Value, latency_ms: i64) -> AttemptOutcome {
    let stop_reason = turn
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let tool_call_count = turn
        .get("tool_use_blocks")
        .and_then(Value::as_array)
        .map(|a| a.len() as i64)
        .unwrap_or(0);
    let content_chars = turn
        .get("result")
        .and_then(Value::as_str)
        .map(|s| s.trim().chars().count() as i64)
        .unwrap_or(0);
    let error_class = turn
        .get("error_class")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    // 1. Errore classificato: la classificazione canonica decide se e' colpa
    //    del modello (conclusivo) o no (inconclusivo). Punto unico riusato dal
    //    probe (regola L).
    if let Some(ec) = &error_class {
        use crate::model_health_probe::Classification;
        let (pass, inconclusive, reason) =
            match crate::model_health_probe::classification_from_error_class(ec) {
                Classification::ModelSpecific(kind, _) => {
                    (false, false, format!("error_class:{kind}"))
                }
                Classification::ProviderWide(kind, _) => {
                    (false, true, format!("provider_wide:{kind}"))
                }
                Classification::Transient(kind, _) => (false, true, format!("transient:{kind}")),
                Classification::Ok => (false, false, format!("error_class:{ec}")),
            };
        return AttemptOutcome {
            pass,
            inconclusive,
            reason,
            error_class,
            tool_call_count,
            content_chars,
            stop_reason,
        };
    }
    // 2. stop_reason=error senza classe: inconclusivo (stessa prudenza del probe).
    if stop_reason == "error" {
        return AttemptOutcome {
            pass: false,
            inconclusive: true,
            reason: "stop_reason_error".into(),
            error_class,
            tool_call_count,
            content_chars,
            stop_reason,
        };
    }
    // 3. Predicato del profilo (soglie dal DB, regola G).
    let min_tool_calls = predicate
        .get("min_tool_calls")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let min_content_chars = predicate
        .get("min_content_chars")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let max_latency_ms = predicate.get("max_latency_ms").and_then(Value::as_i64);
    let mut fail_reason: Option<String> = None;
    if tool_call_count < min_tool_calls {
        fail_reason = Some(format!(
            "no_tool_call:{tool_call_count}<{min_tool_calls}{}",
            if stop_reason.is_empty() {
                String::new()
            } else {
                format!(":{stop_reason}")
            }
        ));
    } else if content_chars < min_content_chars {
        fail_reason = Some(format!("empty_content:{content_chars}<{min_content_chars}"));
    } else if let Some(cap) = max_latency_ms {
        if latency_ms > cap {
            fail_reason = Some(format!("latency:{latency_ms}>{cap}"));
        }
    }
    match fail_reason {
        None => AttemptOutcome {
            pass: true,
            inconclusive: false,
            reason: "ok".into(),
            error_class,
            tool_call_count,
            content_chars,
            stop_reason,
        },
        Some(reason) => AttemptOutcome {
            pass: false,
            inconclusive: false,
            reason,
            error_class,
            tool_call_count,
            content_chars,
            stop_reason,
        },
    }
}

/// Esito aggregato di UN profilo eseguito (`repeat` tentativi).
#[derive(Debug, Clone)]
pub(crate) struct ProfileRun {
    pub profile_key: String,
    pub grants: Vec<String>,
    pub is_blocking: bool,
    pub passes: u32,
    pub conclusive_fails: u32,
    pub inconclusive: u32,
    /// Pass minimi per promuovere (dal `pass_predicate`, default = repeat).
    pub promote_min: u32,
    pub first_fail_reason: Option<String>,
}

impl ProfileRun {
    fn passed(&self) -> bool {
        self.passes >= self.promote_min
    }
}

/// Stato derivato dall'esecuzione della batteria.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DerivedState {
    Qualified,
    Disqualified,
    /// Nessun verdetto attribuibile al modello (transient/provider-wide):
    /// stato invariato + backoff, MAI punitivo (regola H).
    Inconclusive,
}

#[derive(Debug, Clone)]
pub(crate) struct Derived {
    pub state: DerivedState,
    pub qualified_capabilities: Vec<String>,
    pub reason: String,
    /// Policy thinking DERIVATA dalla `thinking_matrix` (fase 5):
    /// `Some((agentic_thinking_policy, uses_thinking_mode))`. `None` = matrice
    /// non eseguita o inconclusiva: la policy del catalog resta invariata.
    pub thinking: Option<(&'static str, bool)>,
}

/// Esito AGGREGATO di una configurazione della thinking_matrix (fase 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigOutcome {
    Pass,
    FailConclusive,
    Inconclusive,
}

impl ConfigOutcome {
    fn from_run(run: &ProfileRun) -> Self {
        if run.passed() {
            Self::Pass
        } else if run.conclusive_fails > 0 {
            Self::FailConclusive
        } else {
            Self::Inconclusive
        }
    }
}

/// PUNTO UNICO PURO (regola L) della matrice thinking (fase 5 del design):
/// DERIVA `agentic_thinking_policy` dai FATTI osservati (il modello lavora con
/// thinking spento? acceso?) invece di dichiararla a mano — era il campo che
/// nessuno verificava (GAP-5: glm dichiarato reasoning con policy 'none' inerte).
///
/// | off  | native | -> policy (uses_thinking_mode)                |
/// |------|--------|-----------------------------------------------|
/// | PASS | PASS   | none (il thinking non serve: piu' economico)  |
/// | PASS | FAIL   | disable_for_tools (dual-mode: spegni nei tool)|
/// | FAIL | PASS   | native, uses=true (rifiuta thinking spento)   |
/// | FAIL | FAIL   | exclude (non regge il carico in nessun modo)  |
///
/// Qualunque esito inconclusivo -> `None`: nessuna scrittura (mai derivare una
/// policy da un giro non attribuibile al modello, regola H). Match esaustivo:
/// un esito nuovo non compila finche' non ne dichiara la semantica.
pub(crate) fn derive_thinking_policy(
    off: ConfigOutcome,
    native: ConfigOutcome,
) -> Option<(&'static str, bool)> {
    use ConfigOutcome::*;
    match (off, native) {
        (Pass, Pass) => Some(("none", false)),
        (Pass, FailConclusive) => Some(("disable_for_tools", false)),
        (FailConclusive, Pass) => Some(("native", true)),
        (FailConclusive, FailConclusive) => Some(("exclude", false)),
        (Inconclusive, _) | (_, Inconclusive) => None,
    }
}

/// PUNTO UNICO PURO (regola L): l'evidenza diventa stato + capability PROVATE.
/// `declared` = jsonb `capabilities` della riga (il dichiarato).
pub(crate) fn derive_capabilities(declared: &[String], runs: &[ProfileRun]) -> Derived {
    // Un blocking con fallimenti CONCLUSIVI sotto soglia squalifica.
    for r in runs {
        if r.is_blocking && !r.passed() && r.conclusive_fails > 0 {
            return Derived {
                state: DerivedState::Disqualified,
                qualified_capabilities: Vec::new(),
                reason: format!(
                    "{}:{}",
                    r.profile_key,
                    r.first_fail_reason.as_deref().unwrap_or("failed")
                ),
                thinking: None,
            };
        }
    }
    // Nessun fallimento conclusivo ma qualche blocking non ha raggiunto la
    // soglia (troppi inconclusivi): giro non attribuibile al modello.
    if runs.iter().any(|r| r.is_blocking && !r.passed()) {
        return Derived {
            state: DerivedState::Inconclusive,
            qualified_capabilities: Vec::new(),
            reason: "inconclusive_round".into(),
            thinking: None,
        };
    }
    // Batteria superata: il PROVATO = unione dei grants dei profili passati,
    // piu' i tag dichiarati che la suite v1 non misura (vedi MEASURED_V1).
    let mut caps: Vec<String> = Vec::new();
    for r in runs.iter().filter(|r| r.passed()) {
        for g in &r.grants {
            if !caps.contains(g) {
                caps.push(g.clone());
            }
        }
    }
    for d in declared {
        if !MEASURED_V1.contains(&d.as_str()) && !caps.contains(d) {
            caps.push(d.clone());
        }
    }
    Derived {
        state: DerivedState::Qualified,
        qualified_capabilities: caps,
        reason: "suite_passed".into(),
        thinking: None,
    }
}

// ── Orchestrazione (I/O) ────────────────────────────────────────────────────

async fn setting_i64(db: &PgPool, key: &str, default: i64) -> i64 {
    crate::settings::get_setting(db, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

/// Carica i profili ENABLED della batteria, ordinati per `ord`.
async fn load_profiles(db: &PgPool) -> Vec<ProbeProfile> {
    let rows = sqlx::query(
        "SELECT profile_key, suite_version, kind, is_blocking, applies_when, \
                grants, payload, pass_predicate \
           FROM ai_model_probe_profile WHERE enabled = TRUE ORDER BY ord",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|r| ProbeProfile {
            profile_key: r.get("profile_key"),
            suite_version: r.get("suite_version"),
            kind: r.get("kind"),
            is_blocking: r.get("is_blocking"),
            applies_when: r.get("applies_when"),
            grants: r
                .get::<Value, _>("grants")
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            payload: r.get("payload"),
            pass_predicate: r.get("pass_predicate"),
        })
        .collect()
}

/// `applies_when.declared_capabilities_contains`: il profilo gira solo se il
/// dichiarato contiene il tag (es. thinking_matrix solo sui reasoning).
fn profile_applies(profile: &ProbeProfile, declared: &[String]) -> bool {
    let Some(cond) = &profile.applies_when else {
        return true;
    };
    match cond
        .get("declared_capabilities_contains")
        .and_then(Value::as_str)
    {
        Some(tag) => declared.iter().any(|c| c == tag),
        None => true,
    }
}

/// Costruisce `(tools_json, messages_json, system_text)` per il profilo.
/// `Err(reason)` se il profilo non e' costruibile (es. template mancante):
/// esito INCONCLUSIVO visibile, mai un fallback silenzioso (regola G/H).
async fn build_profile_request(
    db: &PgPool,
    profile: &ProbeProfile,
) -> Result<(String, String, String), String> {
    match profile.kind.as_str() {
        "chat" => Ok((
            "[]".to_string(),
            json!([{ "role": "user",
                     "content": "Verifica operativa: rispondi con la sola parola: ok" }])
            .to_string(),
            "Sei in una verifica di raggiungibilita'. Rispondi in modo conciso.".to_string(),
        )),
        "tool_minimal" => Ok(crate::model_health_probe::build_tool_probe_request()),
        "tool_realistic" | "thinking_matrix" => {
            let tool_names: Vec<String> = profile
                .payload
                .get("tool_names")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            if tool_names.is_empty() {
                return Err("payload.tool_names vuoto".into());
            }
            // Schemi REALI dal catalogo statico (punto unico, regola L): la
            // prova usa gli artefatti di produzione, non repliche giocattolo.
            let tools = crate::agent_tools::subagent_native::build_tools_json(&tool_names);
            if tools.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                return Err("nessun tool della whitelist esiste nel catalogo statico".into());
            }
            let template_key = profile
                .payload
                .get("system_template_key")
                .and_then(Value::as_str)
                .unwrap_or("");
            if template_key.is_empty() {
                return Err("payload.system_template_key assente".into());
            }
            let system: Option<String> = sqlx::query_scalar(
                "SELECT content FROM nexus_prompt_templates \
                  WHERE key = $1 ORDER BY version DESC LIMIT 1",
            )
            .bind(template_key)
            .fetch_optional(db)
            .await
            .map_err(|e| format!("lettura template '{template_key}': {e}"))?;
            let Some(system) = system.filter(|s| !s.trim().is_empty()) else {
                return Err(format!("template '{template_key}' assente o vuoto"));
            };
            let history_chars = profile
                .payload
                .get("history_chars")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .max(0) as usize;
            // Filler DETERMINISTICO che simula la history reale di una figura
            // (contesto progetto + richiesta): dimensiona il carico, non il
            // contenuto.
            let filler_unit = "Contesto di progetto: applicazione web con autenticazione JWT, \
                               database Postgres, servizi containerizzati e pipeline di build. ";
            let filler: String = filler_unit
                .chars()
                .cycle()
                .take(history_chars)
                .collect();
            let messages = json!([
                { "role": "user",
                  "content": format!("Materiale di contesto per l'analisi:\n{filler}") },
                { "role": "assistant",
                  "content": "Ho letto il contesto. Procedo con l'analisi richiesta." },
                { "role": "user",
                  "content": "Analizza i rischi dell'autenticazione del progetto: inizia \
                              ispezionando i file rilevanti con i tool a disposizione, poi \
                              dichiara il tuo parere strutturato." }
            ]);
            Ok((tools.to_string(), messages.to_string(), system))
        }
        other => Err(format!("kind profilo non implementato: {other}")),
    }
}

/// Registra un tentativo in `ai_model_probe_evidence` e ritorna l'id.
#[allow(clippy::too_many_arguments)]
async fn insert_evidence(
    db: &PgPool,
    provider: &str,
    model: &str,
    profile: &ProbeProfile,
    attempt: i32,
    latency_ms: i64,
    outcome: &AttemptOutcome,
) -> Option<i64> {
    let verdict = if outcome.inconclusive {
        "inconclusive"
    } else if outcome.pass {
        "pass"
    } else {
        "fail"
    };
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO ai_model_probe_evidence \
         (provider, model, profile_key, suite_version, attempt, latency_ms, error_class, \
          tool_call_count, content_chars, stop_reason, verdict, verdict_reason) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING id",
    )
    .bind(provider)
    .bind(model)
    .bind(&profile.profile_key)
    .bind(profile.suite_version)
    .bind(attempt)
    .bind(latency_ms)
    .bind(&outcome.error_class)
    .bind(outcome.tool_call_count)
    .bind(outcome.content_chars)
    .bind(&outcome.stop_reason)
    .bind(verdict)
    .bind(&outcome.reason)
    .fetch_one(db)
    .await
    .map_err(|e| {
        tracing::warn!(error = %e, "model_qualification: insert evidence fallita");
        e
    })
    .ok()
}

/// Esegue `repeat` tentativi di UNA richiesta di profilo con configurazione
/// thinking opzionale e aggrega l'esito. `label` distingue le configurazioni
/// della thinking_matrix nell'evidence (prefisso del verdict_reason); vuoto
/// per i profili ordinari. Punto unico del ciclo tentativo->evidence (regola L).
#[allow(clippy::too_many_arguments)]
async fn run_profile_attempts(
    orchestrator: &Orchestrator,
    db: &PgPool,
    provider: &str,
    model: &str,
    profile: &ProbeProfile,
    request: &(String, String, String),
    repeat: u32,
    timeout_s: u64,
    max_tokens: u32,
    promote_min: u32,
    thinking: Option<crate::nexus_gateway::GwThinkingConfig>,
    label: &str,
    last_evidence: &mut Option<i64>,
) -> ProfileRun {
    let (tools_json, messages_json, system_text) = request;
    let mut run = ProfileRun {
        profile_key: profile.profile_key.clone(),
        grants: profile.grants.clone(),
        is_blocking: profile.is_blocking,
        passes: 0,
        conclusive_fails: 0,
        inconclusive: 0,
        promote_min,
        first_fail_reason: None,
    };
    for attempt in 1..=repeat {
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(timeout_s),
            orchestrator.neural.generate_agent_turn_with_thinking(
                provider,
                model,
                messages_json,
                tools_json,
                max_tokens,
                system_text,
                thinking.clone(),
            ),
        )
        .await;
        let latency_ms = started.elapsed().as_millis() as i64;
        let mut outcome = match result {
            Ok(Ok(turn)) => evaluate_attempt(&turn, &profile.pass_predicate, latency_ms),
            Ok(Err(e)) => {
                // Errore di chiamata: classifica via error_class canonico
                // (punto unico del classifier).
                let ec = orchestrator
                    .neural
                    .classify_error(&e.to_string(), provider)
                    .await;
                evaluate_attempt(
                    &json!({ "error_class": ec }),
                    &profile.pass_predicate,
                    latency_ms,
                )
            }
            Err(_elapsed) => AttemptOutcome {
                pass: false,
                inconclusive: true,
                reason: format!("probe_timeout:{timeout_s}s"),
                error_class: None,
                tool_call_count: 0,
                content_chars: 0,
                stop_reason: String::new(),
            },
        };
        if !label.is_empty() {
            outcome.reason = format!("{label}{}", outcome.reason);
        }
        if let Some(id) = insert_evidence(
            db,
            provider,
            model,
            profile,
            attempt as i32,
            latency_ms,
            &outcome,
        )
        .await
        {
            *last_evidence = Some(id);
        }
        if outcome.inconclusive {
            run.inconclusive += 1;
        } else if outcome.pass {
            run.passes += 1;
        } else {
            run.conclusive_fails += 1;
            if run.first_fail_reason.is_none() {
                run.first_fail_reason = Some(outcome.reason.clone());
            }
        }
    }
    run
}

/// Esegue la batteria su UN modello candidato (gia' claimato `probing`).
/// Ritorna (Derived, ultimo evidence id).
async fn qualify_one(
    orchestrator: &Orchestrator,
    db: &PgPool,
    provider: &str,
    model: &str,
    declared: &[String],
    profiles: &[ProbeProfile],
) -> (Derived, Option<i64>) {
    let mut runs: Vec<ProfileRun> = Vec::new();
    let mut last_evidence: Option<i64> = None;
    let mut thinking_derived: Option<(&'static str, bool)> = None;
    for profile in profiles.iter().filter(|p| profile_applies(p, declared)) {
        let repeat = profile
            .payload
            .get("repeat")
            .and_then(Value::as_i64)
            .unwrap_or(1)
            .clamp(1, 5) as u32;
        let timeout_s = profile
            .payload
            .get("timeout_s")
            .and_then(Value::as_i64)
            .unwrap_or(90)
            .clamp(10, 300) as u64;
        let max_tokens = profile
            .payload
            .get("max_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(512)
            .clamp(16, 16384) as u32;
        let promote_min = profile
            .pass_predicate
            .get("promote_min_passes")
            .and_then(Value::as_i64)
            .map(|n| n.clamp(1, repeat as i64) as u32)
            .unwrap_or(repeat);

        let request = match build_profile_request(db, profile).await {
            Err(reason) => {
                // Profilo non costruibile: giro inconclusivo VISIBILE.
                tracing::warn!(
                    provider = %provider,
                    model = %model,
                    profile = %profile.profile_key,
                    reason = %reason,
                    "model_qualification: profilo non costruibile -> inconclusivo"
                );
                runs.push(ProfileRun {
                    profile_key: profile.profile_key.clone(),
                    grants: profile.grants.clone(),
                    is_blocking: profile.is_blocking,
                    passes: 0,
                    conclusive_fails: 0,
                    inconclusive: repeat,
                    promote_min,
                    first_fail_reason: None,
                });
                continue;
            }
            Ok(r) => r,
        };
        if profile.kind == "thinking_matrix" {
            // FASE 5: la matrice PROVA il modello in DUE configurazioni thinking
            // esplicite (off e native) e DERIVA agentic_thinking_policy dai
            // fatti — mai ereditare la policy del catalog che stiamo derivando.
            let budget = profile
                .payload
                .get("thinking_budget_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(2048)
                .clamp(256, 32768) as u32;
            let off = run_profile_attempts(
                orchestrator,
                db,
                provider,
                model,
                profile,
                &request,
                repeat,
                timeout_s,
                max_tokens,
                promote_min,
                Some(crate::nexus_gateway::GwThinkingConfig {
                    enabled: false,
                    budget_tokens: None,
                    mandatory: false,
                }),
                "off:",
                &mut last_evidence,
            )
            .await;
            let native = run_profile_attempts(
                orchestrator,
                db,
                provider,
                model,
                profile,
                &request,
                repeat,
                timeout_s,
                max_tokens,
                promote_min,
                Some(crate::nexus_gateway::GwThinkingConfig {
                    enabled: true,
                    budget_tokens: Some(budget),
                    mandatory: true,
                }),
                "native:",
                &mut last_evidence,
            )
            .await;
            thinking_derived = derive_thinking_policy(
                ConfigOutcome::from_run(&off),
                ConfigOutcome::from_run(&native),
            );
            runs.push(off);
            runs.push(native);
            continue;
        }
        let run = run_profile_attempts(
            orchestrator,
            db,
            provider,
            model,
            profile,
            &request,
            repeat,
            timeout_s,
            max_tokens,
            promote_min,
            None,
            "",
            &mut last_evidence,
        )
        .await;
        let blocking_conclusive_fail = run.is_blocking && !run.passed() && run.conclusive_fails > 0;
        runs.push(run);
        if blocking_conclusive_fail {
            // Early-stop: i profili successivi non cambiano il verdetto.
            break;
        }
    }
    let mut derived = derive_capabilities(declared, &runs);
    derived.thinking = thinking_derived;
    (derived, last_evidence)
}

/// Scrive lo stato derivato sulla riga (writer UNICO della promozione).
async fn apply_derived(
    db: &PgPool,
    provider: &str,
    model: &str,
    profiles_suite: i32,
    derived: &Derived,
    evidence_id: Option<i64>,
    ttl_days: i64,
    backoff_base_hours: i64,
) {
    let res = match derived.state {
        DerivedState::Qualified => {
            // Policy thinking DERIVATA dalla matrice (fase 5): scritta solo se
            // presente e MAI sopra una curatela (capability_locked). Il trigger
            // di invalidazione (0591) non scatta: NEW.capability_source='probe'.
            let (policy, uses_thinking): (Option<&str>, Option<bool>) = match derived.thinking {
                Some((p, u)) => (Some(p), Some(u)),
                None => (None, None),
            };
            sqlx::query(
                "UPDATE ai_price_catalog SET \
                     qualification_state = 'qualified', \
                     qualified_capabilities = $3, \
                     capability_source = 'probe', \
                     qualified_at = NOW(), \
                     qualification_expires_at = NOW() + make_interval(days => $4::int), \
                     qualification_suite_version = $5, \
                     qualification_reason = $6, \
                     qualification_evidence_id = $7, \
                     agentic_thinking_policy = CASE \
                         WHEN capability_locked THEN agentic_thinking_policy \
                         ELSE COALESCE($8, agentic_thinking_policy) END, \
                     uses_thinking_mode = CASE \
                         WHEN capability_locked THEN uses_thinking_mode \
                         ELSE COALESCE($9, uses_thinking_mode) END, \
                     qualification_started_at = NULL, \
                     qualification_attempts = 0, \
                     qualification_backoff_until = NULL \
                 WHERE provider = $1 AND model = $2",
            )
            .bind(provider)
            .bind(model)
            .bind(json!(derived.qualified_capabilities))
            .bind(ttl_days as i32)
            .bind(profiles_suite)
            .bind(&derived.reason)
            .bind(evidence_id)
            .bind(policy)
            .bind(uses_thinking)
            .execute(db)
            .await
        }
        DerivedState::Disqualified => {
            sqlx::query(
                "UPDATE ai_price_catalog SET \
                     qualification_state = 'disqualified', \
                     qualified_capabilities = '[]'::jsonb, \
                     qualification_reason = $3, \
                     qualification_evidence_id = $4, \
                     qualification_started_at = NULL, \
                     qualification_attempts = qualification_attempts + 1, \
                     qualification_backoff_until = NOW() + make_interval(hours => \
                         LEAST($5::int * (1 << LEAST(qualification_attempts, 6)), $6::int)) \
                 WHERE provider = $1 AND model = $2",
            )
            .bind(provider)
            .bind(model)
            .bind(&derived.reason)
            .bind(evidence_id)
            .bind(backoff_base_hours as i32)
            .bind(BACKOFF_CAP_HOURS as i32)
            .execute(db)
            .await
        }
        DerivedState::Inconclusive => {
            sqlx::query(
                "UPDATE ai_price_catalog SET \
                     qualification_state = CASE \
                         WHEN qualification_state = 'qualified' THEN 'qualified' \
                         ELSE 'unqualified' END, \
                     qualification_reason = $3, \
                     qualification_started_at = NULL, \
                     qualification_attempts = qualification_attempts + 1, \
                     qualification_backoff_until = NOW() + make_interval(hours => \
                         LEAST($4::int * (1 << LEAST(qualification_attempts, 6)), $5::int)) \
                 WHERE provider = $1 AND model = $2",
            )
            .bind(provider)
            .bind(model)
            .bind(&derived.reason)
            .bind(backoff_base_hours as i32)
            .bind(BACKOFF_CAP_HOURS as i32)
            .execute(db)
            .await
        }
    };
    if let Err(e) = res {
        tracing::warn!(
            provider = %provider,
            model = %model,
            error = %e,
            "model_qualification: scrittura stato derivato fallita"
        );
    }
}

/// FASE 0 del worker `model_health_probe`: un giro di qualificazione.
/// Candidati (cap per giro): unqualified / qualified scaduti / quarantined /
/// probing stantii, fuori backoff, SOLO righe che il routing agentico userebbe
/// (enabled + tool_use). Claim CAS `FOR UPDATE SKIP LOCKED`: niente doppio
/// probe tra worker concorrenti.
pub(crate) async fn run_qualification_round(orchestrator: &Orchestrator, db: &PgPool) -> usize {
    let enabled = crate::settings::get_setting(db, KEY_ROUND_ENABLED)
        .await
        .ok()
        .flatten()
        .map(|v| matches!(v.trim().to_lowercase().as_str(), "true" | "1" | "yes" | "on"))
        .unwrap_or(false);
    if !enabled {
        return 0;
    }
    let max_per_round = setting_i64(db, KEY_MAX_PER_ROUND, 4).await;
    let ttl_days = setting_i64(db, KEY_TTL_DAYS, 30).await;
    let backoff_hours = setting_i64(db, KEY_BACKOFF_HOURS, 24).await;

    let profiles = load_profiles(db).await;
    if profiles.is_empty() {
        tracing::warn!(
            "model_qualification: nessun profilo enabled in ai_model_probe_profile \
             (applicare mig 0593): giro saltato"
        );
        return 0;
    }
    let suite_version = profiles.iter().map(|p| p.suite_version).max().unwrap_or(1);

    // Claim CAS. I candidati GIA' qualified (scaduti o con suite vecchia) sono
    // ri-provati IN SHADOW: lo state resta 'qualified' (il pool non si svuota
    // durante la ri-qualificazione); il lock del claim e' qualification_started_at
    // (stantio oltre STALE_PROBING_MINUTES = worker morto, riclaimabile).
    let claimed: Vec<(String, String, Value)> = sqlx::query_as(
        "UPDATE ai_price_catalog c SET \
             qualification_state = CASE WHEN c.qualification_state = 'qualified' \
                                        THEN 'qualified' ELSE 'probing' END, \
             qualification_started_at = NOW() \
         FROM ( \
             SELECT provider, model FROM ai_price_catalog \
              WHERE is_enabled = TRUE AND supports_tool_use = TRUE \
                AND (qualification_backoff_until IS NULL OR qualification_backoff_until < NOW()) \
                AND (qualification_started_at IS NULL \
                     OR qualification_started_at < NOW() - make_interval(mins => $2::int)) \
                AND (qualification_state IN ('unqualified','quarantined','probing') \
                     OR (qualification_state = 'qualified' \
                         AND (qualification_expires_at < NOW() \
                              OR qualification_suite_version < $3))) \
              ORDER BY (qualification_state = 'unqualified') DESC, \
                       qualification_expires_at ASC NULLS FIRST \
              LIMIT $1 \
              FOR UPDATE SKIP LOCKED \
         ) cand \
         WHERE c.provider = cand.provider AND c.model = cand.model \
         RETURNING c.provider, c.model, c.capabilities",
    )
    .bind(max_per_round)
    .bind(STALE_PROBING_MINUTES as i32)
    .bind(suite_version)
    .fetch_all(db)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "model_qualification: claim candidati fallito");
        Vec::new()
    });
    if claimed.is_empty() {
        return 0;
    }
    tracing::info!(
        candidati = claimed.len(),
        suite_version,
        "model_qualification: giro di qualificazione avviato"
    );
    let mut done = 0usize;
    for (provider, model, caps) in &claimed {
        // Provider in cooldown: non sprecare il giro (esito non attribuibile).
        if crate::provider_cooldown::is_provider_in_cooldown(provider) {
            apply_derived(
                db,
                provider,
                model,
                suite_version,
                &Derived {
                    state: DerivedState::Inconclusive,
                    qualified_capabilities: Vec::new(),
                    reason: "provider_in_cooldown".into(),
                    thinking: None,
                },
                None,
                ttl_days,
                backoff_hours,
            )
            .await;
            continue;
        }
        let declared: Vec<String> = caps
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let (derived, evidence_id) =
            qualify_one(orchestrator, db, provider, model, &declared, &profiles).await;
        tracing::info!(
            provider = %provider,
            model = %model,
            state = ?derived.state,
            reason = %derived.reason,
            qualified_capabilities = %json!(derived.qualified_capabilities),
            "model_qualification: verdetto"
        );
        apply_derived(
            db,
            provider,
            model,
            suite_version,
            &derived,
            evidence_id,
            ttl_days,
            backoff_hours,
        )
        .await;
        done += 1;
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_run(
        key: &str,
        grants: &[&str],
        blocking: bool,
        passes: u32,
        fails: u32,
        inconclusive: u32,
        promote_min: u32,
        first_fail: Option<&str>,
    ) -> ProfileRun {
        ProfileRun {
            profile_key: key.into(),
            grants: grants.iter().map(|s| s.to_string()).collect(),
            is_blocking: blocking,
            passes,
            conclusive_fails: fails,
            inconclusive,
            promote_min,
            first_fail_reason: first_fail.map(str::to_owned),
        }
    }

    /// FIXTURE DI REGRESSIONE dell'incidente reale (design §3.3): il modello
    /// "glm-like" passa chat e tool-smoke ma produce SOLO empty_completion sul
    /// carico agentico reale -> squalificato, zero capability provate. Se la
    /// batteria smette di scartarlo, questo test diventa rosso.
    #[test]
    fn fixture_glm_empty_completion_viene_squalificato() {
        let declared = vec!["chat".into(), "code".into(), "reasoning".into()];
        let runs = vec![
            profile_run("chat_smoke", &["chat"], true, 1, 0, 0, 1, None),
            profile_run("tool_smoke", &[], true, 1, 0, 0, 1, None),
            profile_run(
                "agentic_real",
                &["chat", "code"],
                true,
                0,
                3,
                0,
                3,
                Some("error_class:empty_completion"),
            ),
        ];
        let d = derive_capabilities(&declared, &runs);
        assert_eq!(d.state, DerivedState::Disqualified);
        assert!(d.qualified_capabilities.is_empty());
        assert_eq!(d.reason, "agentic_real:error_class:empty_completion");
    }

    /// FIXTURE gemella: il modello "deepseek-like" supera l'intera batteria ->
    /// qualificato con le capability MISURATE dai grants + il `reasoning`
    /// dichiarato (non ancora misurato dalla suite v1, ereditato SOLO a
    /// batteria superata).
    #[test]
    fn fixture_deepseek_suite_superata_viene_promosso() {
        let declared = vec!["chat".into(), "code".into(), "reasoning".into()];
        let runs = vec![
            profile_run("chat_smoke", &["chat"], true, 1, 0, 0, 1, None),
            profile_run("tool_smoke", &[], true, 1, 0, 0, 1, None),
            profile_run("agentic_real", &["chat", "code"], true, 3, 0, 0, 3, None),
        ];
        let d = derive_capabilities(&declared, &runs);
        assert_eq!(d.state, DerivedState::Qualified);
        assert_eq!(
            d.qualified_capabilities,
            vec!["chat".to_string(), "code".to_string(), "reasoning".to_string()]
        );
    }

    /// Il "millantatore": dichiara reasoning ma FALLISCE la batteria -> il tag
    /// dichiarato NON viene mai ereditato (l'eredita' e' condizionata al probe).
    #[test]
    fn tag_dichiarato_non_ereditato_se_batteria_fallita() {
        let declared = vec!["reasoning".into()];
        let runs = vec![profile_run(
            "agentic_real",
            &["chat", "code"],
            true,
            1,
            2,
            0,
            3,
            Some("no_tool_call:0<1"),
        )];
        let d = derive_capabilities(&declared, &runs);
        assert_eq!(d.state, DerivedState::Disqualified);
        assert!(d.qualified_capabilities.is_empty());
    }

    /// Transient/provider-wide NON e' mai punitivo (regola H, stessa prudenza
    /// del probe): giro inconclusivo, nessuna squalifica.
    #[test]
    fn giro_inconclusivo_non_squalifica() {
        let declared = vec!["chat".into()];
        let runs = vec![profile_run(
            "agentic_real",
            &["chat", "code"],
            true,
            1,
            0,
            2,
            3,
            None,
        )];
        let d = derive_capabilities(&declared, &runs);
        assert_eq!(d.state, DerivedState::Inconclusive);
    }

    /// Isteresi: `hold_min_passes` non e' usato dalla promozione (promote_min
    /// = 3/3) ma 2 pass + 1 inconclusivo non squalifica (nessun fail conclusivo).
    #[test]
    fn due_pass_su_tre_con_un_transient_resta_inconclusivo() {
        let declared = vec!["chat".into()];
        let runs = vec![profile_run(
            "agentic_real",
            &["chat", "code"],
            true,
            2,
            0,
            1,
            3,
            None,
        )];
        let d = derive_capabilities(&declared, &runs);
        assert_eq!(d.state, DerivedState::Inconclusive);
    }

    // ── evaluate_attempt: il verdetto di UN tentativo dai segnali strutturati ──

    #[test]
    fn attempt_empty_completion_e_fail_conclusivo() {
        let turn = json!({ "error_class": "empty_completion" });
        let out = evaluate_attempt(&turn, &json!({"min_tool_calls": 1}), 100);
        assert!(!out.pass);
        assert!(!out.inconclusive, "empty_completion e' MODEL-specific");
        assert!(out.reason.starts_with("error_class:"));
    }

    #[test]
    fn attempt_rate_limit_e_inconclusivo() {
        let turn = json!({ "error_class": "rate_limit" });
        let out = evaluate_attempt(&turn, &json!({}), 100);
        assert!(!out.pass);
        assert!(out.inconclusive, "rate_limit non e' colpa del modello");
    }

    #[test]
    fn attempt_tool_call_richiesta_e_verificata() {
        let ok = json!({ "stop_reason": "tool_use",
                         "tool_use_blocks": [{"name": "read_file"}] });
        let out = evaluate_attempt(&ok, &json!({"min_tool_calls": 1}), 100);
        assert!(out.pass, "{}", out.reason);
        let ko = json!({ "stop_reason": "end_turn", "result": "chiacchiere" });
        let out = evaluate_attempt(&ko, &json!({"min_tool_calls": 1}), 100);
        assert!(!out.pass);
        assert!(out.reason.starts_with("no_tool_call"));
    }

    #[test]
    fn attempt_latency_oltre_il_cap_fallisce() {
        let turn = json!({ "stop_reason": "tool_use",
                           "tool_use_blocks": [{"name": "read_file"}] });
        let out = evaluate_attempt(&turn, &json!({"min_tool_calls": 1, "max_latency_ms": 30000}), 45000);
        assert!(!out.pass);
        assert!(out.reason.starts_with("latency:"));
    }

    // ── thinking_matrix (fase 5): la policy DERIVATA dai fatti ───────────────

    #[test]
    fn matrice_thinking_deriva_le_quattro_policy() {
        use ConfigOutcome::*;
        // Il modello lavora in entrambe le modalita': niente thinking (economia).
        assert_eq!(derive_thinking_policy(Pass, Pass), Some(("none", false)));
        // Dual-mode che degenera col thinking sotto tool: spegnilo nei tool-loop.
        assert_eq!(
            derive_thinking_policy(Pass, FailConclusive),
            Some(("disable_for_tools", false))
        );
        // Il caso gemini-3 (rifiuta thinkingBudget=0): thinking OBBLIGATORIO.
        assert_eq!(
            derive_thinking_policy(FailConclusive, Pass),
            Some(("native", true))
        );
        // Non regge il carico agentico in NESSUNA configurazione: fuori.
        assert_eq!(
            derive_thinking_policy(FailConclusive, FailConclusive),
            Some(("exclude", false))
        );
    }

    #[test]
    fn matrice_inconclusiva_non_scrive_policy() {
        use ConfigOutcome::*;
        // Qualunque lato inconclusivo -> nessuna scrittura (mai derivare una
        // policy da un giro non attribuibile al modello).
        assert_eq!(derive_thinking_policy(Inconclusive, Pass), None);
        assert_eq!(derive_thinking_policy(Pass, Inconclusive), None);
        assert_eq!(derive_thinking_policy(Inconclusive, FailConclusive), None);
        assert_eq!(derive_thinking_policy(Inconclusive, Inconclusive), None);
    }

    #[test]
    fn config_outcome_da_profile_run() {
        // passes >= promote_min -> Pass; fail conclusivi -> FailConclusive;
        // solo inconclusivi -> Inconclusive.
        let pass = profile_run("m", &[], false, 2, 0, 0, 2, None);
        assert_eq!(ConfigOutcome::from_run(&pass), ConfigOutcome::Pass);
        let fail = profile_run("m", &[], false, 1, 1, 0, 2, Some("empty"));
        assert_eq!(ConfigOutcome::from_run(&fail), ConfigOutcome::FailConclusive);
        let inc = profile_run("m", &[], false, 1, 0, 1, 2, None);
        assert_eq!(ConfigOutcome::from_run(&inc), ConfigOutcome::Inconclusive);
    }
}
