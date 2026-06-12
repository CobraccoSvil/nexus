//! session_worklog.rs — Storia di lavoro canonica e provider-agnostica per
//! sessione (mig 0411).
//!
//! Continuita' operativa cross-run e cross-provider: gli eventi strutturati
//! (file toccati, comandi con esito, errori, tentativi falliti da non
//! ripetere, stato) sono derivati DETERMINISTICAMENTE dagli step del run
//! (nessuna chiamata LLM) e materializzati in un digest testuale neutro
//! (`nexus_session_worklog.rendered_block`) che il brain inietta nel
//! system_text di ogni run della sessione. Essendo solo testo nel system, il
//! digest sopravvive identico a qualunque cambio di provider/modello
//! (cascade fallback, re-route, supersede last-wins, run interrupted).
//!
//! Punto unico (regola L): `collect_step_facts` e' l'UNICA estrazione di
//! fatti strutturati dagli step — `collect_actions` (recap ADR 0025 in
//! agent_run.rs) delega qui. Il rendering eventi->markdown vive UNA volta
//! (`render_digest`); il brain fa solo SELECT del blocco materializzato.
//!
//! Budget token (D8): il digest e' compatto (cap `agent.worklog.inject_max_chars`);
//! il dettaglio completo resta in `nexus_session_worklog_events`, servito
//! on-demand dal tool read-only `nexus_get_worklog` (zero LLM, zero embedding).
//!
//! Privacy (regola F): nei log tracing solo conteggi e id; gli estratti di
//! errore vanno SOLO nel payload in tabella, troncati.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::agent_types::{AgentStep, AgentStepStatus};

/// Cap di default dell'estratto errore quando i settings non sono disponibili
/// (allineato al default DB `agent.worklog.error_excerpt_max_chars`).
pub const DEFAULT_ERROR_EXCERPT_CHARS: usize = 200;

// ───────────────────────────────────────────────────────────────────────────
// Settings DB-driven (cache 60s, pattern wiki::run_summary_worker)
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct WorklogSettings {
    pub enabled: bool,
    /// `true` = render completo (entro budget); `false` = digest compatto.
    pub inject_full: bool,
    pub inject_max_chars: usize,
    pub digest_max_items: usize,
    pub tool_page_size: i64,
    pub events_max_per_session: i64,
    pub error_excerpt_max_chars: usize,
}

impl WorklogSettings {
    const fn safe_defaults() -> Self {
        Self {
            enabled: true,
            inject_full: false,
            inject_max_chars: 1200,
            digest_max_items: 8,
            tool_page_size: 50,
            events_max_per_session: 300,
            error_excerpt_max_chars: DEFAULT_ERROR_EXCERPT_CHARS,
        }
    }
}

const SETTINGS_CACHE_TTL: Duration = Duration::from_secs(60);

static SETTINGS_CACHE: once_cell::sync::Lazy<RwLock<Option<(WorklogSettings, Instant)>>> =
    once_cell::sync::Lazy::new(|| RwLock::new(None));

pub async fn current_settings(db: &PgPool) -> WorklogSettings {
    {
        let guard = SETTINGS_CACHE.read().await;
        if let Some((v, exp)) = *guard {
            if Instant::now() < exp {
                return v;
            }
        }
    }
    let value = match load_settings(db).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "session_worklog: lettura settings fallita, uso safe_defaults");
            WorklogSettings::safe_defaults()
        }
    };
    let mut guard = SETTINGS_CACHE.write().await;
    *guard = Some((value, Instant::now() + SETTINGS_CACHE_TTL));
    value
}

async fn load_settings(db: &PgPool) -> Result<WorklogSettings> {
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN ( \
            'agent.worklog.enabled', \
            'agent.worklog.inject_mode', \
            'agent.worklog.inject_max_chars', \
            'agent.worklog.digest_max_items', \
            'agent.worklog.tool_page_size', \
            'agent.worklog.events_max_per_session', \
            'agent.worklog.error_excerpt_max_chars' \
         )",
    )
    .fetch_all(db)
    .await
    .context("SELECT settings agent.worklog.*")?;

    let mut out = WorklogSettings::safe_defaults();
    for row in rows {
        let key: String = row.try_get("key").unwrap_or_default();
        let raw: String = row.try_get("value").unwrap_or_default();
        match key.as_str() {
            "agent.worklog.enabled" => {
                out.enabled = matches!(
                    raw.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                );
            }
            "agent.worklog.inject_mode" => {
                out.inject_full = raw.trim().eq_ignore_ascii_case("full");
            }
            "agent.worklog.inject_max_chars" => {
                if let Ok(v) = raw.trim().parse::<usize>() {
                    out.inject_max_chars = v.clamp(200, 20_000);
                }
            }
            "agent.worklog.digest_max_items" => {
                if let Ok(v) = raw.trim().parse::<usize>() {
                    out.digest_max_items = v.clamp(1, 50);
                }
            }
            "agent.worklog.tool_page_size" => {
                if let Ok(v) = raw.trim().parse::<i64>() {
                    out.tool_page_size = v.clamp(1, 200);
                }
            }
            "agent.worklog.events_max_per_session" => {
                if let Ok(v) = raw.trim().parse::<i64>() {
                    out.events_max_per_session = v.max(20);
                }
            }
            "agent.worklog.error_excerpt_max_chars" => {
                if let Ok(v) = raw.trim().parse::<usize>() {
                    out.error_excerpt_max_chars = v.clamp(40, 2000);
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

// ───────────────────────────────────────────────────────────────────────────
// Estrazione deterministica dei fatti dagli step (punto unico, regola L)
// ───────────────────────────────────────────────────────────────────────────

/// Troncamento per caratteri (mai per byte): stessa semantica di
/// `agent_run::trunc_chars`, duplicata qui solo come helper privato perche'
/// quella e' `pub(crate)` dentro chat_messages (modulo fratello).
fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

#[derive(Debug, Clone)]
pub struct CommandFact {
    pub command: String,
    pub ok: bool,
    /// exit_code strutturato (W1) se il tool_result era JSON con quel campo.
    pub exit_code: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ErrorFact {
    pub tool: String,
    pub detail: String,
    pub excerpt: String,
}

#[derive(Debug, Clone)]
pub struct RepeatFact {
    pub detail: String,
    pub count: u32,
}

/// Fatti strutturati estratti dagli step di un run. `action_lines` e
/// `files_touched` replicano ESATTAMENTE il comportamento storico di
/// `collect_actions` (recap ADR 0025): righe-azione deduplicate dagli step
/// COMPLETATI e file creati/modificati (destinazione inclusa per i rename).
#[derive(Debug, Default)]
pub struct StepFacts {
    pub action_lines: Vec<String>,
    /// path -> azione ("write" | "edit" | "create" | "patch" | "move")
    pub files_touched: BTreeMap<String, String>,
    pub commands: Vec<CommandFact>,
    pub errors: Vec<ErrorFact>,
    /// Azione fallita (>=1) il cui ultimo esito resta un fallimento: da NON
    /// ripetere. Il `count` e' il numero di fallimenti nel run.
    pub failed_attempts: Vec<RepeatFact>,
    /// Azione prima fallita poi riuscita (errore-e-fix deterministico).
    pub retry_ok: Vec<String>,
    pub last_tool: Option<String>,
}

/// Dettaglio leggibile dall'input del tool, in ordine di preferenza ("to"
/// copre rename_file/fs_move che usano from/to, non path).
fn step_detail(step: &AgentStep) -> Option<String> {
    ["path", "file", "command", "pattern", "query", "to"]
        .iter()
        .find_map(|k| step.tool_input.get(*k).and_then(|v| v.as_str()))
        .map(|s| trunc(s, 120))
}

/// exit_code strutturato (W1) dal tool_result, se serializzato come JSON.
fn exit_code_from_result(tool_result: Option<&str>) -> Option<i64> {
    let raw = tool_result?;
    let v: Value = serde_json::from_str(raw.trim()).ok()?;
    v.get("exit_code").and_then(Value::as_i64)
}

/// Punto unico (regola L) di estrazione dei fatti strutturati dagli step.
/// `collect_actions` (agent_run.rs) e l'ingest del worklog delegano qui:
/// nessuna seconda implementazione della domanda "cosa ha fatto il run?".
pub fn collect_step_facts(steps: &[AgentStep], error_excerpt_max_chars: usize) -> StepFacts {
    use std::collections::BTreeSet;
    let mut facts = StepFacts::default();
    let mut seen_lines: BTreeSet<String> = BTreeSet::new();
    // signature -> (numero fallimenti, ultimo esito ok?)
    let mut outcomes: BTreeMap<String, (u32, bool)> = BTreeMap::new();

    for step in steps {
        if step.tool_name.is_empty() {
            continue;
        }
        let detail = step_detail(step);
        facts.last_tool = Some(step.tool_name.clone());

        let signature = format!(
            "{}|{}",
            step.tool_name,
            detail.as_deref().unwrap_or_default()
        );

        // Invariante: action_lines e files_touched sono popolati SOLO dal ramo
        // Completed (parita' col recap storico di collect_actions). Failed e
        // ProviderUnavailable alimentano solo errors/commands/outcomes.
        match step.status {
            AgentStepStatus::Completed => {
                let entry = outcomes.entry(signature).or_insert((0, true));
                entry.1 = true;

                // Comportamento storico di collect_actions: righe-azione
                // deduplicate + file toccati, SOLO dagli step completati.
                let line = match &detail {
                    Some(d) => format!("- `{}`: {}", step.tool_name, d),
                    None => format!("- `{}`", step.tool_name),
                };
                if seen_lines.insert(line.clone()) {
                    facts.action_lines.push(line);
                }
                let file_action = match step.tool_name.as_str() {
                    "write_file" => Some("write"),
                    "edit_file" => Some("edit"),
                    "create_file" => Some("create"),
                    "apply_patch" => Some("patch"),
                    _ => None,
                };
                if let Some(action) = file_action {
                    if let Some(p) = step.tool_input.get("path").and_then(|v| v.as_str()) {
                        facts.files_touched.insert(p.to_string(), action.to_string());
                    }
                }
                // Spostamenti: la destinazione E' un file/albero toccato
                // (incidente Beauty-Book, vedi recap ADR 0025).
                if matches!(step.tool_name.as_str(), "rename_file" | "fs_move") {
                    if let Some(to) = step.tool_input.get("to").and_then(|v| v.as_str()) {
                        facts.files_touched.insert(to.to_string(), "move".to_string());
                    }
                }
                if matches!(step.tool_name.as_str(), "run_command" | "run_tests") {
                    if let Some(cmd) = step.tool_input.get("command").and_then(|v| v.as_str()) {
                        facts.commands.push(CommandFact {
                            command: trunc(cmd, 120),
                            ok: true,
                            exit_code: exit_code_from_result(step.tool_result.as_deref()),
                        });
                    }
                }
            }
            AgentStepStatus::Failed => {
                let entry = outcomes.entry(signature).or_insert((0, false));
                entry.0 += 1;
                entry.1 = false;

                facts.errors.push(ErrorFact {
                    tool: step.tool_name.clone(),
                    detail: detail.clone().unwrap_or_default(),
                    excerpt: trunc(
                        step.tool_result.as_deref().unwrap_or_default(),
                        error_excerpt_max_chars,
                    ),
                });
                if matches!(step.tool_name.as_str(), "run_command" | "run_tests") {
                    if let Some(cmd) = step.tool_input.get("command").and_then(|v| v.as_str()) {
                        facts.commands.push(CommandFact {
                            command: trunc(cmd, 120),
                            ok: false,
                            exit_code: exit_code_from_result(step.tool_result.as_deref()),
                        });
                    }
                }
            }
            AgentStepStatus::ProviderUnavailable => {
                // Provider non disponibile durante questo tool (cooldown/billing):
                // fallimento INFRASTRUTTURALE, non dell'azione. Lo registriamo
                // come errore informativo per il modello subentrante (continuita'
                // cross-provider), ma NON tocchiamo `outcomes`: "non ripetere"
                // vale per i fallimenti dell'azione, non per quelli del provider.
                facts.errors.push(ErrorFact {
                    tool: step.tool_name.clone(),
                    detail: detail.clone().unwrap_or_default(),
                    excerpt: trunc(
                        step.tool_result.as_deref().unwrap_or_default(),
                        error_excerpt_max_chars,
                    ),
                });
            }
            // Running/AwaitingConfirmation/Skipped: stati transitori o non-esiti,
            // nessun fatto operativo certo da registrare.
            _ => {}
        }
    }

    // retry_ok: fallita >=1 ma l'ultimo esito e' ok (errore-e-fix).
    // failed_attempts: fallita >=1 e l'ultimo esito resta un fallimento
    // (copre anche il singolo fallimento mai recuperato: da NON ripetere).
    for (signature, (fails, last_ok)) in &outcomes {
        let detail = signature.replace('|', ": ");
        if *fails >= 1 && *last_ok {
            facts.retry_ok.push(detail.clone());
        } else if *fails >= 1 && !*last_ok {
            facts.failed_attempts.push(RepeatFact {
                detail,
                count: *fails,
            });
        }
    }
    facts
}

// ───────────────────────────────────────────────────────────────────────────
// Ingest eventi (idempotente: UNIQUE(session_id, dedup_key))
// ───────────────────────────────────────────────────────────────────────────

fn facts_to_events(facts: &StepFacts, run_id: Uuid, status_label: &str) -> Vec<(String, Value, String)> {
    let mut events: Vec<(String, Value, String)> = Vec::new();
    // Tutti i dedup_key includono run_id: la provenance per-run e' preservata
    // (due run della stessa sessione che toccano lo stesso file generano due
    // eventi distinti, non una sovrascrittura). L'aggregazione cross-run per la
    // lettura avviene nel render (es. failed_attempt sommati per detail).
    let rid = run_id.to_string();
    for (path, action) in &facts.files_touched {
        events.push((
            "file_touched".into(),
            json!({"path": path, "action": action}),
            format!("file|{rid}|{path}"),
        ));
    }
    for c in &facts.commands {
        events.push((
            "command".into(),
            json!({"command": c.command, "ok": c.ok, "exit_code": c.exit_code}),
            format!("cmd|{rid}|{}|{}", c.command, c.ok),
        ));
    }
    for e in &facts.errors {
        events.push((
            "error".into(),
            json!({"tool": e.tool, "detail": e.detail, "excerpt": e.excerpt}),
            format!("err|{rid}|{}|{}", e.tool, e.detail),
        ));
    }
    for r in &facts.retry_ok {
        events.push((
            "retry_ok".into(),
            json!({"detail": r}),
            format!("rok|{rid}|{r}"),
        ));
    }
    for f in &facts.failed_attempts {
        events.push((
            "failed_attempt".into(),
            json!({"detail": f.detail, "count": f.count}),
            format!("fail|{rid}|{}", f.detail),
        ));
    }
    // Stato del run: deterministico, latest-wins nel render (un evento per run).
    let status_text = format!(
        "run {} {}: {} azioni, {} file toccati, {} errori{}",
        &run_id.to_string()[..8],
        status_label,
        facts.action_lines.len(),
        facts.files_touched.len(),
        facts.errors.len(),
        facts
            .last_tool
            .as_deref()
            .map(|t| format!("; ultimo tool: {t}"))
            .unwrap_or_default()
    );
    events.push((
        "status".into(),
        json!({"text": status_text}),
        format!("status|run|{run_id}"),
    ));
    events
}

/// Inserisce gli eventi derivati dai fatti e rinfresca il digest materializzato.
/// Best-effort by-design: i chiamanti ignorano l'errore (mai bloccare la chat).
pub async fn ingest_facts(
    db: &PgPool,
    session_id: Uuid,
    project_id: Option<Uuid>,
    run_id: Uuid,
    status_label: &str,
    facts: &StepFacts,
) -> Result<usize> {
    let settings = current_settings(db).await;
    if !settings.enabled {
        return Ok(0);
    }
    let events = facts_to_events(facts, run_id, status_label);
    let mut inserted = 0usize;
    for (kind, payload, dedup_key) in &events {
        // dedup_key per-run (include run_id): l'ON CONFLICT scatta solo per
        // re-ingest dello STESSO run (idempotente) — DO UPDATE riallinea
        // payload/status all'ultima finalizzazione. Cross-run non collide.
        let res = sqlx::query(
            "INSERT INTO nexus_session_worklog_events \
             (session_id, project_id, run_id, kind, payload, source, dedup_key) \
             VALUES ($1, $2, $3, $4, $5, 'deterministic', $6) \
             ON CONFLICT (session_id, dedup_key) \
             DO UPDATE SET payload = EXCLUDED.payload, run_id = EXCLUDED.run_id",
        )
        .bind(session_id)
        .bind(project_id)
        .bind(run_id)
        .bind(kind)
        .bind(payload)
        .bind(dedup_key)
        .execute(db)
        .await;
        match res {
            Ok(_) => inserted += 1,
            Err(e) => {
                tracing::warn!(error = %e, %session_id, "session_worklog: insert evento fallito");
            }
        }
    }
    prune_events(db, session_id, &settings).await;
    refresh_rendered(db, session_id, project_id, &settings).await?;
    tracing::info!(
        %session_id,
        %run_id,
        events = inserted,
        "session_worklog: ingest completato"
    );
    Ok(inserted)
}

/// Wrapper per i call-site che hanno gli step in memoria (fine run spawn/resume).
pub async fn ingest_steps_for_run(
    db: &PgPool,
    session_id: Uuid,
    project_id: Option<Uuid>,
    run_id: Uuid,
    status_label: &str,
    steps: &[AgentStep],
) -> Result<usize> {
    if steps.is_empty() {
        return Ok(0);
    }
    let settings = current_settings(db).await;
    if !settings.enabled {
        return Ok(0);
    }
    let facts = collect_step_facts(steps, settings.error_excerpt_max_chars);
    ingest_facts(db, session_id, project_id, run_id, status_label, &facts).await
}

/// Ingest dagli `agent_steps` persistiti (supersede last-wins, run reapati
/// 'interrupted'): gli step sono GIA' in DB grazie alla persistenza
/// incrementale del brain (M68), quindi questo e' affidabile anche per i run
/// interrotti a meta'.
pub async fn ingest_from_db_steps(db: &PgPool, run_id: Uuid, status_label: &str) -> Result<usize> {
    let settings = current_settings(db).await;
    if !settings.enabled {
        return Ok(0);
    }
    let run_row = sqlx::query(
        "SELECT session_id, project_id FROM agent_runs WHERE id = $1",
    )
    .bind(run_id)
    .fetch_optional(db)
    .await
    .context("SELECT agent_runs per worklog ingest")?;
    let Some(run_row) = run_row else {
        return Ok(0);
    };
    let session_id: Uuid = run_row.try_get("session_id")?;
    let project_id: Option<Uuid> = run_row.try_get("project_id").ok();

    let rows = sqlx::query(
        "SELECT step_index, tool_name, tool_input, tool_result, status \
         FROM agent_steps WHERE run_id = $1 ORDER BY step_index ASC",
    )
    .bind(run_id)
    .fetch_all(db)
    .await
    .context("SELECT agent_steps per worklog ingest")?;
    if rows.is_empty() {
        return Ok(0);
    }

    let steps: Vec<AgentStep> = rows
        .iter()
        .map(|r| {
            let status_raw: String = r.try_get("status").unwrap_or_default();
            AgentStep {
                run_id: run_id.to_string(),
                step_index: r.try_get::<i32, _>("step_index").unwrap_or(0).max(0) as u32,
                tool_name: r.try_get("tool_name").unwrap_or_default(),
                tool_input: r
                    .try_get::<Value, _>("tool_input")
                    .unwrap_or(Value::Null),
                tool_result: r.try_get::<Option<String>, _>("tool_result").unwrap_or(None),
                // Copertura esplicita di TUTTE le varianti note di
                // AgentStepStatus: uno status non mappato non deve sparire in
                // silenzio (la perdita sarebbe invisibile). Lo logghiamo (regola
                // F: solo l'etichetta, mai il payload dello step) e lo trattiamo
                // come Running (ignorato a valle, senza fabbricare fatti falsi).
                status: match status_raw.as_str() {
                    "completed" => AgentStepStatus::Completed,
                    "failed" => AgentStepStatus::Failed,
                    "awaiting_confirmation" => AgentStepStatus::AwaitingConfirmation,
                    "skipped" => AgentStepStatus::Skipped,
                    "provider_unavailable" => AgentStepStatus::ProviderUnavailable,
                    "running" => AgentStepStatus::Running,
                    other => {
                        tracing::warn!(
                            status = %other,
                            run_id = %run_id,
                            "session_worklog: status step non riconosciuto in agent_steps"
                        );
                        AgentStepStatus::Running
                    }
                },
                created_at: String::new(),
            }
        })
        .collect();

    let facts = collect_step_facts(&steps, settings.error_excerpt_max_chars);
    ingest_facts(db, session_id, project_id, run_id, status_label, &facts).await
}

/// Pruning oltre soglia: rimuove gli eventi piu' VECCHI dei kind non critici
/// (mai failed_attempt/status/decision: sono lo stato corrente da preservare).
async fn prune_events(db: &PgPool, session_id: Uuid, settings: &WorklogSettings) {
    let res = sqlx::query(
        "DELETE FROM nexus_session_worklog_events \
         WHERE session_id = $1 \
           AND kind IN ('file_touched', 'command', 'error', 'retry_ok') \
           AND id NOT IN ( \
               SELECT id FROM nexus_session_worklog_events \
               WHERE session_id = $1 \
                 AND kind IN ('file_touched', 'command', 'error', 'retry_ok') \
               ORDER BY created_at DESC \
               LIMIT $2 \
           )",
    )
    .bind(session_id)
    .bind(settings.events_max_per_session)
    .execute(db)
    .await;
    if let Ok(r) = res {
        if r.rows_affected() > 0 {
            tracing::debug!(
                %session_id,
                pruned = r.rows_affected(),
                "session_worklog: pruning eventi oltre soglia"
            );
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Rendering digest (punto unico: il brain legge SOLO il blocco materializzato)
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct WorklogEvent {
    pub kind: String,
    pub payload: Value,
}

fn payload_str(p: &Value, key: &str) -> String {
    p.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// Render del digest provider-neutro (solo markdown, mai tool_call_id o
/// thinking). Priorita' sezioni: stato > da-non-ripetere > errori > risolti
/// dopo retry > file > comandi. `max_items` voci per sezione, troncamento
/// totale a `max_chars`. Stringa vuota se non ci sono eventi.
pub fn render_digest(
    events: &[WorklogEvent],
    total_events: usize,
    max_items: usize,
    max_chars: usize,
) -> String {
    if events.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "Storia di lavoro della sessione (generata dal sistema dai run precedenti; \
         provider-neutra). Non ripetere azioni gia' completate ne' tentativi gia' \
         falliti; per il dettaglio completo chiama il tool nexus_get_worklog.\n",
    );

    // Stato: l'evento status piu' recente (gli eventi arrivano ordinati per
    // created_at ASC, quindi l'ultimo vince).
    if let Some(st) = events.iter().rev().find(|e| e.kind == "status") {
        out.push_str(&format!("\nStato: {}\n", payload_str(&st.payload, "text")));
    }

    let section = |title: &str, lines: Vec<String>| {
        if lines.is_empty() {
            return String::new();
        }
        let shown = lines.len().min(max_items);
        let mut s = format!("\n{title}\n");
        for l in lines.iter().take(max_items) {
            s.push_str(l);
            s.push('\n');
        }
        if lines.len() > shown {
            s.push_str(&format!("- (... altre {} voci via nexus_get_worklog)\n", lines.len() - shown));
        }
        s
    };

    // failed_attempt e' per-run (dedup_key con run_id): aggreghiamo per detail
    // sommando i count cross-run, cosi' il digest mostra una riga per azione.
    let mut failed_agg: BTreeMap<String, u64> = BTreeMap::new();
    for e in events.iter().filter(|e| e.kind == "failed_attempt") {
        let detail = payload_str(&e.payload, "detail");
        if detail.is_empty() {
            continue;
        }
        let c = e.payload.get("count").and_then(Value::as_u64).unwrap_or(1);
        *failed_agg.entry(detail).or_insert(0) += c;
    }
    let failed: Vec<String> = failed_agg
        .into_iter()
        .map(|(detail, count)| format!("- {detail} (fallita {count} volte)"))
        .collect();
    out.push_str(&section("Da NON ripetere (tentativi falliti):", failed));

    let errors: Vec<String> = events
        .iter()
        .filter(|e| e.kind == "error")
        .rev()
        .map(|e| {
            let tool = payload_str(&e.payload, "tool");
            let detail = payload_str(&e.payload, "detail");
            let excerpt = payload_str(&e.payload, "excerpt");
            let head: String = excerpt.chars().take(80).collect();
            if detail.is_empty() {
                format!("- `{tool}`: {head}")
            } else {
                format!("- `{tool}` {detail}: {head}")
            }
        })
        .collect();
    out.push_str(&section("Errori incontrati:", errors));

    let retried: Vec<String> = events
        .iter()
        .filter(|e| e.kind == "retry_ok")
        .map(|e| format!("- {}", payload_str(&e.payload, "detail")))
        .collect();
    out.push_str(&section("Risolti dopo retry:", retried));

    let decisions: Vec<String> = events
        .iter()
        .filter(|e| e.kind == "decision")
        .rev()
        .map(|e| format!("- {}", payload_str(&e.payload, "text")))
        .collect();
    out.push_str(&section("Decisioni:", decisions));

    let files: Vec<String> = events
        .iter()
        .filter(|e| e.kind == "file_touched")
        .rev()
        .map(|e| {
            format!(
                "- `{}` ({})",
                payload_str(&e.payload, "path"),
                payload_str(&e.payload, "action")
            )
        })
        .collect();
    out.push_str(&section("File gia' creati/modificati:", files));

    let commands: Vec<String> = events
        .iter()
        .filter(|e| e.kind == "command")
        .rev()
        .map(|e| {
            let ok = e.payload.get("ok").and_then(Value::as_bool).unwrap_or(true);
            format!(
                "- `{}` ({})",
                payload_str(&e.payload, "command"),
                if ok { "ok" } else { "FALLITO" }
            )
        })
        .collect();
    out.push_str(&section("Comandi eseguiti:", commands));

    out.push_str(&format!(
        "\nEventi totali registrati: {total_events}. Dettaglio: nexus_get_worklog.\n"
    ));

    // Troncamento budget-safe: il footer e' misurato dinamicamente cosi' il
    // risultato resta SEMPRE <= max_chars qualunque sia la lunghezza del footer.
    let footer = "\n(... digest troncato; dettaglio: nexus_get_worklog)\n";
    if out.chars().count() > max_chars {
        let budget = max_chars.saturating_sub(footer.chars().count());
        let mut t: String = out.chars().take(budget).collect();
        t.push_str(footer);
        return t;
    }
    out
}

/// Rilegge gli eventi della sessione e materializza il digest in
/// `nexus_session_worklog.rendered_block`.
///
/// Concorrenza: piu' ingest sulla stessa sessione (supersede sincrono + spawn
/// async + reaper) possono materializzare in parallelo. E' best-effort
/// last-writer-wins: ogni scrittura accoppia un `rendered_block` e un
/// `events_count` letti dallo STESSO snapshot, quindi internamente coerenti; il
/// digest resta sempre valido. Gli eventi critici (failed_attempt/status/
/// decision) sono esclusi dal pruning, quindi non si perdono in queste finestre.
pub async fn refresh_rendered(
    db: &PgPool,
    session_id: Uuid,
    project_id: Option<Uuid>,
    settings: &WorklogSettings,
) -> Result<()> {
    let rows = sqlx::query(
        "SELECT kind, payload \
         FROM nexus_session_worklog_events \
         WHERE session_id = $1 ORDER BY created_at ASC",
    )
    .bind(session_id)
    .fetch_all(db)
    .await
    .context("SELECT worklog events per render")?;

    let events: Vec<WorklogEvent> = rows
        .iter()
        .map(|r| WorklogEvent {
            kind: r.try_get("kind").unwrap_or_default(),
            payload: r.try_get::<Value, _>("payload").unwrap_or(Value::Null),
        })
        .collect();

    let max_items = if settings.inject_full {
        usize::MAX / 2
    } else {
        settings.digest_max_items
    };
    let block = render_digest(&events, events.len(), max_items, settings.inject_max_chars);

    sqlx::query(
        "INSERT INTO nexus_session_worklog (session_id, project_id, rendered_block, events_count, updated_at) \
         VALUES ($1, $2, $3, $4, NOW()) \
         ON CONFLICT (session_id) \
         DO UPDATE SET rendered_block = EXCLUDED.rendered_block, \
                       events_count = EXCLUDED.events_count, \
                       project_id = COALESCE(EXCLUDED.project_id, nexus_session_worklog.project_id), \
                       updated_at = NOW()",
    )
    .bind(session_id)
    .bind(project_id)
    .bind(&block)
    .bind(events.len() as i32)
    .execute(db)
    .await
    .context("UPSERT nexus_session_worklog")?;
    Ok(())
}

/// Ingest di decisioni distillate dalla compattazione di sessione (mig 0413)
/// come eventi kind='decision' source='distilled'. Le decisioni durano a livello
/// SESSIONE (dedup_key per contenuto normalizzato, senza run_id): il render le
/// mostra nella sezione "Decisioni:" del digest, sopravvivendo alla
/// compattazione successiva. Best-effort: i chiamanti ignorano l'errore.
pub async fn ingest_decisions(
    db: &PgPool,
    session_id: Uuid,
    project_id: Option<Uuid>,
    decisions: &[String],
) -> Result<usize> {
    let settings = current_settings(db).await;
    if !settings.enabled || decisions.is_empty() {
        return Ok(0);
    }
    let mut inserted = 0usize;
    for d in decisions {
        let text = d.trim();
        if text.is_empty() {
            continue;
        }
        let dedup_key = format!("decision|{}", normalized_hash(text));
        let res = sqlx::query(
            "INSERT INTO nexus_session_worklog_events \
             (session_id, project_id, run_id, kind, payload, source, dedup_key) \
             VALUES ($1, $2, NULL, 'decision', $3, 'distilled', $4) \
             ON CONFLICT (session_id, dedup_key) DO UPDATE SET payload = EXCLUDED.payload",
        )
        .bind(session_id)
        .bind(project_id)
        .bind(json!({ "text": text }))
        .bind(&dedup_key)
        .execute(db)
        .await;
        match res {
            Ok(_) => inserted += 1,
            Err(e) => {
                tracing::warn!(error = %e, %session_id, "session_worklog: insert decision fallito");
            }
        }
    }
    if inserted > 0 {
        refresh_rendered(db, session_id, project_id, &settings).await?;
        tracing::info!(%session_id, decisions = inserted, "session_worklog: decisioni distillate ingerite");
    }
    Ok(inserted)
}

/// Hash del testo normalizzato (lowercase, spazi collassati): dedup robusto a
/// differenze cosmetiche, condiviso da decisioni e (concettualmente) regole.
fn normalized_hash(text: &str) -> String {
    let normalized = text.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Legge il digest materializzato della sessione (punto unico di lettura lato
/// Rust, gemello di `fetch_worklog_block` lato brain). `None` se assente, vuoto
/// o su errore (fail-open): i chiamanti degradano senza bloccare.
pub async fn fetch_rendered_block(db: &PgPool, session_id: Uuid) -> Option<String> {
    let settings = current_settings(db).await;
    if !settings.enabled {
        return None;
    }
    let row = sqlx::query(
        "SELECT rendered_block FROM nexus_session_worklog WHERE session_id = $1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()?;
    let block: String = row.try_get("rendered_block").ok()?;
    let trimmed = block.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Sintesi compatta per la nota supersede: stato dell'ultimo run interrotto.
/// Stringa vuota se il worklog e' vuoto o disabilitato (fail-open).
pub async fn supersede_summary(db: &PgPool, session_id: Uuid, superseded: &[Uuid]) -> String {
    let settings = current_settings(db).await;
    if !settings.enabled || superseded.is_empty() {
        return String::new();
    }
    let row = sqlx::query(
        "SELECT payload FROM nexus_session_worklog_events \
         WHERE session_id = $1 AND kind = 'status' AND run_id = ANY($2) \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(session_id)
    .bind(superseded)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let Some(row) = row else {
        return String::new();
    };
    let payload: Value = row.try_get("payload").unwrap_or(Value::Null);
    let text = payload_str(&payload, "text");
    if text.is_empty() {
        return String::new();
    }
    format!(
        "\nLavoro gia' svolto dal run interrotto: {text}. Il dettaglio e' nel blocco \
         <session_worklog> del system: NON ripetere le azioni gia' completate."
    )
}

// ───────────────────────────────────────────────────────────────────────────
// Tool read-only nexus_get_worklog (D8: drill-down on-demand, zero LLM)
// ───────────────────────────────────────────────────────────────────────────

pub async fn tool_nexus_get_worklog(
    ctx: &crate::agent_tools::AgentToolContext,
    input: &Value,
) -> String {
    let Some(session_id) = ctx.session_id else {
        return "nexus_get_worklog: disponibile solo nelle sessioni chat (nessuna sessione nel contesto).".to_string();
    };
    let db: &PgPool = ctx.db.as_ref();
    let settings = current_settings(db).await;
    let kind = input.get("kind").and_then(Value::as_str);
    let run_id = input
        .get("run_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok());
    let limit = input
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(settings.tool_page_size)
        .clamp(1, settings.tool_page_size);
    let offset = input.get("offset").and_then(Value::as_i64).unwrap_or(0).max(0);

    let rows = sqlx::query(
        "SELECT kind, payload, run_id, source, created_at::text AS created_at, \
                COUNT(*) OVER() AS total \
         FROM nexus_session_worklog_events \
         WHERE session_id = $1 \
           AND ($2::text IS NULL OR kind = $2) \
           AND ($3::uuid IS NULL OR run_id = $3) \
         ORDER BY created_at DESC \
         LIMIT $4 OFFSET $5",
    )
    .bind(session_id)
    .bind(kind)
    .bind(run_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(db)
    .await;

    let rows = match rows {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, %session_id, "nexus_get_worklog: query fallita");
            return "nexus_get_worklog: errore di lettura del worklog (riprova).".to_string();
        }
    };
    let total: i64 = rows
        .first()
        .and_then(|r| r.try_get::<i64, _>("total").ok())
        .unwrap_or(0);
    let events: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                "payload": r.try_get::<Value, _>("payload").unwrap_or(Value::Null),
                "run_id": r.try_get::<Option<Uuid>, _>("run_id").ok().flatten().map(|u| u.to_string()),
                "source": r.try_get::<String, _>("source").unwrap_or_default(),
                "created_at": r.try_get::<String, _>("created_at").unwrap_or_default(),
            })
        })
        .collect();
    serde_json::to_string(&json!({
        "session_id": session_id.to_string(),
        "total": total,
        "offset": offset,
        "limit": limit,
        "events": events,
    }))
    .unwrap_or_else(|_| "nexus_get_worklog: errore di serializzazione.".to_string())
}

// ───────────────────────────────────────────────────────────────────────────
// Test (puri, senza DB)
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn step(
        idx: u32,
        tool: &str,
        input: Value,
        result: Option<&str>,
        status: AgentStepStatus,
    ) -> AgentStep {
        AgentStep {
            run_id: "r".into(),
            step_index: idx,
            tool_name: tool.into(),
            tool_input: input,
            tool_result: result.map(str::to_string),
            status,
            created_at: String::new(),
        }
    }

    #[test]
    fn facts_replicano_collect_actions_storico() {
        // Righe-azione deduplicate + file toccati (incluso il "to" dei rename).
        let steps = vec![
            step(1, "write_file", json!({"path": "src/a.ts"}), Some("ok"), AgentStepStatus::Completed),
            step(2, "write_file", json!({"path": "src/a.ts"}), Some("ok"), AgentStepStatus::Completed),
            step(3, "rename_file", json!({"from": "x", "to": "src/app"}), Some("ok"), AgentStepStatus::Completed),
        ];
        let facts = collect_step_facts(&steps, DEFAULT_ERROR_EXCERPT_CHARS);
        assert_eq!(facts.action_lines.len(), 2, "righe deduplicate: {:?}", facts.action_lines);
        assert!(facts.files_touched.contains_key("src/a.ts"));
        assert!(facts.files_touched.contains_key("src/app"), "il 'to' del rename e' un file toccato");
    }

    #[test]
    fn failed_attempt_su_doppio_fallimento_senza_successo() {
        let steps = vec![
            step(1, "edit_file", json!({"path": "src/b.ts"}), Some("old_string non trovato"), AgentStepStatus::Failed),
            step(2, "edit_file", json!({"path": "src/b.ts"}), Some("old_string non trovato"), AgentStepStatus::Failed),
        ];
        let facts = collect_step_facts(&steps, DEFAULT_ERROR_EXCERPT_CHARS);
        assert_eq!(facts.failed_attempts.len(), 1);
        assert_eq!(facts.failed_attempts[0].count, 2);
        assert!(facts.retry_ok.is_empty());
        assert_eq!(facts.errors.len(), 2);
    }

    #[test]
    fn failed_attempt_su_singolo_fallimento_mai_recuperato() {
        // Buco logico chiuso: un fallimento unico, mai riuscito, e' "da NON
        // ripetere" tanto quanto uno ripetuto (soglia >=1, non >=2).
        let steps = vec![step(
            1,
            "edit_file",
            json!({"path": "src/c.ts"}),
            Some("old_string non trovato"),
            AgentStepStatus::Failed,
        )];
        let facts = collect_step_facts(&steps, DEFAULT_ERROR_EXCERPT_CHARS);
        assert_eq!(facts.failed_attempts.len(), 1, "il singolo fallimento e' segnalato");
        assert_eq!(facts.failed_attempts[0].count, 1);
        assert!(facts.retry_ok.is_empty());
    }

    #[test]
    fn provider_unavailable_e_errore_informativo_non_failed_attempt() {
        // Fallimento infrastrutturale (provider in cooldown): errore per la
        // continuita' cross-provider, ma NON "da non ripetere" (l'azione non
        // ha fallito, il provider si').
        let steps = vec![step(
            1,
            "run_command",
            json!({"command": "deploy"}),
            Some("provider in cooldown"),
            AgentStepStatus::ProviderUnavailable,
        )];
        let facts = collect_step_facts(&steps, DEFAULT_ERROR_EXCERPT_CHARS);
        assert_eq!(facts.errors.len(), 1, "registrato come errore informativo");
        assert!(facts.failed_attempts.is_empty(), "non e' un failed_attempt");
        assert!(facts.action_lines.is_empty(), "non e' un'azione completata");
    }

    #[test]
    fn retry_ok_su_fallimento_poi_successo() {
        // Stessa signature: prima fallisce, poi riesce -> errore-e-fix, NON da-non-ripetere.
        let steps = vec![
            step(1, "run_command", json!({"command": "npm test"}), Some("{\"exit_code\": 1}"), AgentStepStatus::Failed),
            step(2, "run_command", json!({"command": "npm test"}), Some("{\"exit_code\": 0}"), AgentStepStatus::Completed),
        ];
        let facts = collect_step_facts(&steps, DEFAULT_ERROR_EXCERPT_CHARS);
        assert_eq!(facts.retry_ok.len(), 1);
        assert!(facts.failed_attempts.is_empty());
        // exit_code strutturato W1 estratto dal tool_result JSON.
        assert_eq!(facts.commands[0].exit_code, Some(1));
        assert_eq!(facts.commands[1].exit_code, Some(0));
    }

    #[test]
    fn excerpt_errore_troncato_al_cap() {
        let long = "x".repeat(500);
        let steps = vec![step(
            1,
            "run_command",
            json!({"command": "boom"}),
            Some(long.as_str()),
            AgentStepStatus::Failed,
        )];
        let facts = collect_step_facts(&steps, 100);
        assert_eq!(facts.errors[0].excerpt.chars().count(), 100);
    }

    fn ev(kind: &str, payload: Value) -> WorklogEvent {
        WorklogEvent {
            kind: kind.into(),
            payload,
        }
    }

    #[test]
    fn digest_vuoto_senza_eventi() {
        assert!(render_digest(&[], 0, 8, 1200).is_empty());
    }

    #[test]
    fn digest_entro_budget_e_con_priorita() {
        let mut events = vec![
            ev("status", json!({"text": "run 12345678 completed: 3 azioni, 2 file, 0 errori"})),
            ev("failed_attempt", json!({"detail": "edit_file: src/b.ts", "count": 3})),
        ];
        for i in 0..200 {
            events.push(ev("file_touched", json!({"path": format!("src/f{i}.ts"), "action": "write"})));
        }
        let out = render_digest(&events, events.len(), 8, 1200);
        assert!(out.chars().count() <= 1200, "budget sforato: {}", out.chars().count());
        assert!(out.contains("Stato:"), "lo stato ha priorita' massima");
        assert!(out.contains("Da NON ripetere"), "i failed_attempt devono sopravvivere al budget");
        assert!(out.contains("nexus_get_worklog"), "puntatore al drill-down sempre presente");
    }

    #[test]
    fn digest_max_items_per_sezione() {
        let mut events = vec![];
        for i in 0..20 {
            events.push(ev("file_touched", json!({"path": format!("src/f{i}.ts"), "action": "write"})));
        }
        let out = render_digest(&events, 20, 5, 100_000);
        let shown = out.matches("- `src/f").count();
        assert_eq!(shown, 5, "max_items per sezione: {out}");
        assert!(out.contains("altre 15 voci"));
    }

    #[test]
    fn digest_failed_aggregati_per_detail_cross_run() {
        // Due run distinti (eventi failed_attempt per-run con stesso detail):
        // il render aggrega in UNA riga sommando i count.
        let events = vec![
            ev("failed_attempt", json!({"detail": "edit_file: src/b.ts", "count": 2})),
            ev("failed_attempt", json!({"detail": "edit_file: src/b.ts", "count": 1})),
        ];
        let out = render_digest(&events, events.len(), 8, 100_000);
        let rows = out.matches("edit_file: src/b.ts").count();
        assert_eq!(rows, 1, "una sola riga aggregata: {out}");
        assert!(out.contains("fallita 3 volte"), "count sommato cross-run: {out}");
    }

    #[test]
    fn digest_mostra_decisioni_distillate() {
        // Le decisioni (mig 0413, dalla compattazione) appaiono nella sezione
        // dedicata del digest provider-neutro.
        let events = vec![
            ev("status", json!({"text": "run completato"})),
            ev("decision", json!({"text": "Adottato pattern repository per il data layer"})),
        ];
        let out = render_digest(&events, events.len(), 8, 100_000);
        assert!(out.contains("Decisioni:"), "sezione decisioni assente: {out}");
        assert!(out.contains("pattern repository"));
    }

    #[test]
    fn normalized_hash_dedup_decisioni() {
        assert_eq!(normalized_hash("Usa pnpm"), normalized_hash("usa   PNPM"));
        assert_ne!(normalized_hash("Usa pnpm"), normalized_hash("Usa yarn"));
    }

    #[test]
    fn digest_rispetta_budget_anche_stretto() {
        // Budget molto piccolo: il troncamento budget-safe garantisce <= max_chars
        // qualunque sia la lunghezza del footer.
        let mut events = vec![ev("status", json!({"text": "run completato"}))];
        for i in 0..50 {
            events.push(ev("file_touched", json!({"path": format!("src/very/long/path/file{i}.ts"), "action": "write"})));
        }
        for cap in [80usize, 120, 300, 1200] {
            let out = render_digest(&events, events.len(), 8, cap);
            assert!(out.chars().count() <= cap, "cap {cap} sforato: {}", out.chars().count());
        }
    }
}
