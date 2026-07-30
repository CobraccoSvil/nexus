//! Orchestrazione NATIVA dei sub-agenti (porting Rust di `/agent/subagent-run`).
//!
//! Verso zero-Python: prima `dispatch_subagent` (in `nexus-agent-tools`) chiamava
//! l'endpoint REST `POST /agent/subagent-run` del brain Python, che eseguiva un
//! sub-run sul grafo agentico LangGraph. Ora il sub-run gira sul GRAFO NATIVO Rust
//! (`crate::native_engine::run_native`), in-process, senza alcuna dipendenza dal
//! brain.
//!
//! ## Perche' qui (gerarchia crate, regola L)
//!
//! Il grafo nativo (`native_engine`) vive in `mcp-core`; `nexus-agent-tools` e' una
//! DEP di `mcp-core` (mcp-core -> nexus-agent-tools, non viceversa), quindi
//! `subagent.rs` NON puo' chiamare `native_engine`. Il tool sub-agent viene percio'
//! INTERCETTATO qui in `mcp-core` (dove `run_native` e' chiamabile), prima di
//! delegare. E' la via piu' pulita data la gerarchia: l'orchestrazione del sub-run
//! sta dove vive il motore.
//!
//! ## Guard replicate dal brain (DB-driven, regola G)
//!
//! - `orchestrator.subagents_enabled`  (false -> tool ritorna disabilitato)
//! - `orchestrator.subagent_kinds_whitelist` (CSV dei kind ammessi)
//! - `orchestrator.subagent_max_depth` (rifiuta se depth supera il cap -> anti-ricorsione)
//! - `orchestrator.subagent_cost_cap_per_run_usd` (hard cap di spesa per parent)
//! - `orchestrator.subagent_default_timeout_s` (timeout se la definition non lo specifica)
//!
//! Le soglie sono lette dal DB (`settings`): niente fallback hardcoded sul
//! comportamento di business; i safe-default coincidono coi default del brain.
//!
//! ## Anti-ricorsione (depth DB-driven)
//!
//! La catena di sub-run e' ancorata a `parent_anchor` = `parent_run_id.or(session_
//! id)` (raggruppamento per famiglia). La profondita' corrente e' derivata dalla
//! CATENA in `nexus_subagent_runs`: il depth del nuovo sub-run e' `1 + max(depth)`
//! tra i sub-run con stesso `parent_anchor` ancora `running`. Cosi' un sub-agente
//! che chiama un altro sub-agente incrementa il depth e, al raggiungimento di
//! `max_depth`, il dispatch viene rifiutato: niente loop infinito.
//!
//! ## Fan-in background: il run CORRENTE, non l'anchor
//!
//! Il path Real del grafo (`ToolRunnerExecutorAdapter::execute_real`) valorizza
//! `ctx.core.run_id` col run CORRENTE che invoca il tool (dalla narrazione del run
//! invocante). Per il fan-in background e' QUESTO il run che il motore sospende
//! (`awaiting_subagents`) e che il worker deve riprendere: l'enqueue usa
//! [`fanin_target_run_id`] (= `ctx.core.run_id`), NON `parent_anchor` (che punta
//! alla sessione per i sub-run di primo livello). Sono concern distinti: l'anchor
//! raggruppa la famiglia (depth/cost/COUNT figli), il run corrente e' il target del
//! resume.

use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use super::AgentToolContext;
use crate::native_engine::{verdict_keys, NativeDeps, NativeRunInput, NativeRunOutcome};
use nexus_agent_graph::decisions::{
    AdvisoryPanelVerdict, AdvisoryPolicy, AdvisoryRoster, AdvisorySynthesis, QuorumPolicy,
};

/// Marker d'errore (stesso contratto di `subagent.rs`: prefisso U+274C ->
/// `tool_result_is_error` deriva `is_error=true`).
fn err(msg: &str) -> String {
    format!("\u{274C} [dispatch_subagent] {msg}")
}

// Chiavi ricorrenti dei payload di narrazione/tool_result del sub-run: un solo
// literal per chiave (i consumatori — frontend e test — leggono le stesse).
const K_SUB_RUN_ID: &str = "subagent_run_id";
const K_SUB_KIND: &str = "subagent_kind";
const K_SUMMARY: &str = "summary";
const K_TIMEOUT_S: &str = "timeout_s";
const K_TARGET: &str = "target";
const K_IS_ERROR: &str = "is_error";
const K_PROVIDER: &str = "provider";
const K_MODEL: &str = "model";

/// Soglie sub-agent lette dal DB (regola G). I default coincidono coi default del
/// brain (`orchestrator_config` + mig 0153): safe-default se la chiave manca, MAI
/// magic fallback sul comportamento.
struct SubagentSettings {
    enabled: bool,
    whitelist: Vec<String>,
    max_depth: i64,
    cost_cap_usd: f64,
    default_timeout_s: i64,
    /// Narrazione del sub-run sul run PADRE (meta-step avvio/progresso/chiusura,
    /// mig 0535). Kill-switch UX: OFF -> chat muta durante il dispatch (storico).
    narration_enabled: bool,
    /// Heartbeat "al lavoro" nei silenzi del sub-run (secondi, mig 0535).
    /// 0 -> heartbeat disabilitato (restano avvio/progressi/chiusura).
    narration_heartbeat_s: i64,
}

async fn read_subagent_settings(ctx: &AgentToolContext) -> Result<SubagentSettings, String> {
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN (
            'orchestrator.subagents_enabled',
            'orchestrator.subagent_kinds_whitelist',
            'orchestrator.subagent_max_depth',
            'orchestrator.subagent_cost_cap_per_run_usd',
            'orchestrator.subagent_default_timeout_s',
            'orchestrator.subagent_narration_enabled',
            'orchestrator.subagent_narration_heartbeat_s'
        )",
    )
    .fetch_all(&*ctx.core.db)
    .await
    .map_err(|e| format!("query settings: {e}"))?;

    let mut s = SubagentSettings {
        enabled: false,
        whitelist: Vec::new(),
        max_depth: 2,
        cost_cap_usd: 5.0,
        default_timeout_s: 300,
        narration_enabled: true,
        narration_heartbeat_s: 20,
    };
    for row in rows {
        let k: String = row.get("key");
        let v: String = row.get("value");
        apply_subagent_setting(&mut s, &k, &v);
    }
    Ok(s)
}

/// Flag booleano nel formato accettato dalla tabella `settings`.
fn settings_flag(v: &str) -> bool {
    matches!(v.to_lowercase().as_str(), "true" | "1" | "yes" | "on")
}

/// Applica UNA riga di `settings` alle soglie sub-agent (chiave ignota: no-op).
fn apply_subagent_setting(s: &mut SubagentSettings, key: &str, value: &str) {
    let v = value.trim();
    match key {
        "orchestrator.subagents_enabled" => s.enabled = settings_flag(v),
        "orchestrator.subagent_kinds_whitelist" => {
            s.whitelist = v
                .split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        }
        "orchestrator.subagent_max_depth" => s.max_depth = v.parse().unwrap_or(2),
        "orchestrator.subagent_cost_cap_per_run_usd" => s.cost_cap_usd = v.parse().unwrap_or(5.0),
        "orchestrator.subagent_default_timeout_s" => s.default_timeout_s = v.parse().unwrap_or(300),
        "orchestrator.subagent_narration_enabled" => s.narration_enabled = settings_flag(v),
        "orchestrator.subagent_narration_heartbeat_s" => {
            s.narration_heartbeat_s = v.parse().unwrap_or(20)
        }
        _ => {}
    }
}

/// Definition di un kind di sub-agent (`nexus_subagent_definitions`).
struct SubagentDefinition {
    prompt_key: String,
    tool_whitelist: Vec<String>,
    model_purpose: String,
    timeout_s: i64,
}

async fn fetch_definition(
    db: &sqlx::PgPool,
    kind: &str,
) -> Result<Option<SubagentDefinition>, String> {
    let row = sqlx::query(
        "SELECT prompt_key, tool_whitelist, model_purpose, timeout_s, is_enabled
         FROM nexus_subagent_definitions WHERE kind = $1 LIMIT 1",
    )
    .bind(kind)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("query definition: {e}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let is_enabled: bool = row.get::<bool, _>("is_enabled");
    if !is_enabled {
        return Err(format!("kind '{kind}' disabilitato"));
    }
    Ok(Some(SubagentDefinition {
        prompt_key: row.get::<String, _>("prompt_key"),
        tool_whitelist: row
            .try_get::<Vec<String>, _>("tool_whitelist")
            .unwrap_or_default(),
        model_purpose: row
            .try_get::<Option<String>, _>("model_purpose")
            .ok()
            .flatten()
            .unwrap_or_default(),
        timeout_s: row.get::<i32, _>("timeout_s") as i64,
    }))
}

/// Kind di sub-agent CONVOCABILI dal run principale (regola G/L: registry DB unica
/// fonte, niente enum hardcoded nello schema tool). Sono i kind con definition
/// abilitata E presenti nella whitelist runtime
/// (`orchestrator.subagent_kinds_whitelist`) — lo STESSO gate del Guard 1 del
/// dispatcher, cosi' il modello vede esattamente i kind che potra' davvero
/// dispatchare. Usati da `build_tools_json_for_agent` per generare a runtime l'enum
/// `kind` dei tool dispatch_subagent(s), sostituendo il SEED statico del catalogo.
/// Ordinati per output stabile. Lista vuota (DB irraggiungibile) -> il chiamante
/// mantiene il seed statico.
pub async fn convocable_kinds(db: &sqlx::PgPool) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT d.kind
           FROM nexus_subagent_definitions d
          WHERE d.is_enabled
            AND d.kind = ANY(string_to_array(
                COALESCE((SELECT value FROM settings
                           WHERE key = 'orchestrator.subagent_kinds_whitelist'), ''), ','))
          ORDER BY d.kind",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default()
}

/// Risolve il system_text del sub-agente dal registry prompt (`nexus_prompt_templates`,
/// stesso punto del brain `prompt_registry.get_prompt`). Vuoto -> errore (parita'
/// col brain: prompt mancante -> sub-run fallito).
async fn resolve_system_text(ctx: &AgentToolContext, prompt_key: &str) -> String {
    sqlx::query_scalar::<_, String>(
        "SELECT content FROM nexus_prompt_templates WHERE key = $1 AND is_active = true",
    )
    .bind(prompt_key)
    .fetch_optional(&*ctx.core.db)
    .await
    .ok()
    .flatten()
    .unwrap_or_default()
}

/// Costruisce l'array tools del sub-run filtrando lo schema REALE
/// (`AGENT_TOOLS_JSON`, fonte unica degli schema tool) sulla `tool_whitelist`
/// della definition. Migliora la fedelta' rispetto ai descrittori minimali che il
/// brain costruiva a mano (`_filter_tools_by_whitelist`): il modello vede lo schema
/// vero. Whitelist vuota -> nessun tool (sub-agente puramente conversazionale).
/// Canali con cui un sub-run DICHIARA il proprio esito (ADR 0034): impostano
/// `declared_outcome`, cosi' il gate G1 (`route_after_executor`) onora la
/// chiusura su `end_turn` invece di re-instradare all'executor. `task_complete`
/// e' il canale UNIVERSALE (builtin del run principale); le figure del consiglio
/// usano `advisory_verdict`, il revisore `review_verdict` e l'avvocato del
/// dibattito `debate_position` come loro equivalente di ruolo (dichiarazione
/// strutturata = dichiarazione d'esito).
const COMPLETION_CHANNEL_TOOLS: [&str; 4] = [
    "task_complete",
    "advisory_verdict",
    "review_verdict",
    "debate_position",
];

pub(crate) fn build_tools_json(whitelist: &[String]) -> Value {
    if whitelist.is_empty() {
        return json!([]);
    }
    let all: Value = serde_json::from_str(nexus_agent_tools::tool_schema::AGENT_TOOLS_JSON)
        .unwrap_or_else(|_| json!([]));
    let mut filtered: Vec<Value> = all
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|t| {
                    t.get("name")
                        .and_then(Value::as_str)
                        .map(|n| whitelist.iter().any(|w| w == n))
                        .unwrap_or(false)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    // INVARIANTE ADR 0034 (regola L, punto unico): ogni sub-run con dei tool deve
    // avere un canale per DICHIARARE l'esito, altrimenti non puo' cortocircuitare
    // i nudge G1 su `end_turn` e cicla fino al cap (incidente run a6f25c1e: coder
    // senza `task_complete` -> 39 iterazioni, chiusura confusa). Se la whitelist
    // NON espone gia' un canale di completamento, iniettiamo `task_complete` (il
    // builtin universale). Le figure/review, che hanno il loro verdetto
    // strutturato di ruolo, restano intatte: quello E' il loro canale.
    let has_completion_channel = filtered.iter().any(|t| {
        t.get("name")
            .and_then(Value::as_str)
            .map(|n| COMPLETION_CHANNEL_TOOLS.contains(&n))
            .unwrap_or(false)
    });
    if !has_completion_channel {
        if let Some(tc) = all.as_array().and_then(|arr| {
            arr.iter().find(|t| {
                t.get("name").and_then(Value::as_str) == Some("task_complete")
            })
        }) {
            filtered.push(tc.clone());
        }
    }
    json!(filtered)
}

/// Ancora del parent per la catena di sub-run: il run genitore se noto, altrimenti
/// la sessione (usata per depth-chain / cost-cap / ensure_child, valori raggruppati
/// per famiglia di sub-run). `Uuid::nil` solo se manca anche la sessione (caso
/// degenere). NON e' il target del fan-in: quello e' il run CORRENTE (vedi
/// [`fanin_target_run_id`]).
fn parent_anchor(ctx: &AgentToolContext) -> Uuid {
    ctx.core
        .parent_run_id
        .or(ctx.core.session_id)
        .unwrap_or_else(Uuid::nil)
}

/// PUNTO UNICO (regola L) del run da RIPRENDERE nel fan-in background: il run
/// CORRENTE che ha invocato `dispatch_subagents` (`ctx.core.run_id`), cioe' l'id
/// che il motore marca `awaiting_subagents` e che il worker fan-in cerca con il
/// CAS `agent_runs.id = parent_run_id`. Diverso da [`parent_anchor`] (che e'
/// `parent_run_id.or(session_id)`, usato per la catena depth/cost): accodare
/// l'anchor romperebbe il CAS (il run sospeso e' il chiamante, non l'anchor).
/// `None` fuori dal grafo Real (dove il background non e' comunque attivo): in tal
/// caso ricade su `anchor` per non regredire (best-effort) — ma nel flusso reale
/// il grafo Real valorizza sempre `run_id`.
fn fanin_target_run_id(ctx: &AgentToolContext, anchor: Uuid) -> Uuid {
    ctx.core.run_id.unwrap_or(anchor)
}

/// Profondita' del nuovo sub-run nella CATENA ANTENATI (anti-ricorsione, punto
/// unico). E' `1 + depth del DISPATCHER` (il run chiamante immediato), NON il
/// `MAX(depth)` tra i sub-run `running` sotto l'anchor: quest'ultimo contava i
/// FRATELLI paralleli (fan-out) come antenati e gonfiava la profondita', facendo
/// rifiutare le figure del consiglio convocate in parallelo ("depth 3 > max 2"
/// pur essendo tutte figlie DIRETTE del run principale). La profondita' deve
/// misurare la LUNGHEZZA della catena padre->figlio, immune al numero di fratelli
/// concorrenti (stessa distinzione anchor-vs-dispatcher gia' adottata dalla COUNT
/// del fan-in). Il dispatcher, se e' a sua volta un sub-run, ha una row con
/// `id = dispatcher_run_id` e la sua depth -> il figlio e' depth+1; se il
/// dispatcher e' il run PRINCIPALE (nessuna row) -> 0 -> figlio depth 1.
async fn current_chain_depth(pool: &sqlx::PgPool, dispatcher_run_id: Uuid) -> i64 {
    // pool: gia' instradato sul progetto dal chiamante (nexus_subagent_runs migrata).
    sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(depth, 0)::bigint FROM nexus_subagent_runs WHERE id = $1",
    )
    .bind(dispatcher_run_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(0)
}

/// Adotta i totali AUTORITATIVI del ledger sui contatori del sub-run.
///
/// Un sub-run e' un run a se': il gateway gli scrive le proprie righe di
/// `ai_usage_ledger`, una per chiamata LLM. `NativeRunOutcome` invece porta i
/// contatori dell'ULTIMO TURNO — lo stato del grafo usa un reducer di tipo
/// overwrite (vedi la doc dei campi in `native_engine.rs`). Pubblicarli come
/// totale del sub-run sottostima tutte le iterazioni precedenti: misurato
/// $0,0338 contro $0,1510 reali (4,5x) sul sub-run software_architect della
/// chat 25, con la stessa firma su ogni sub-run multi-turno.
///
/// Il run di chat riconcilia gia' cosi' (`reconcile_run_cost_from_ledger`); per i
/// sub-run la riconciliazione non era disattivata: non esisteva. La conseguenza
/// peggiore non era cosmetica — `cumulative_cost` alimenta l'HARD CAP di spesa
/// leggendo `nexus_subagent_runs.cost_usd`, quindi il freno vedeva $0,10 dove la
/// spesa reale era $1,69 (16x).
///
/// `meta_db` DEVE essere il pool META: `ai_usage_ledger` vive li', mentre
/// `nexus_subagent_runs`/`agent_runs` stanno sul pool del progetto.
/// Best-effort come per il padre: senza righe contabilizzate i contatori del
/// grafo restano invariati (nessun dato inventato).
async fn adopt_ledger_totals(meta_db: &sqlx::PgPool, sub_run_id: Uuid, o: &mut NativeRunOutcome) {
    let ledger = crate::chat_messages::fetch_ledger_totals(meta_db, sub_run_id).await;
    apply_ledger_to_outcome(o, &ledger);
}

/// Parte PURA di [`adopt_ledger_totals`] (testabile senza DB), speculare a
/// `reconcile_run_cost_from_ledger` del run di chat. Ritorna `true` se ha adottato
/// i totali del ledger.
fn apply_ledger_to_outcome(
    o: &mut NativeRunOutcome,
    ledger: &crate::chat_messages::LedgerTotals,
) -> bool {
    if !ledger.has_rows() {
        return false;
    }
    o.total_cost = ledger.total_cost;
    o.prompt_tokens = ledger.prompt_tokens;
    o.completion_tokens = ledger.completion_tokens;
    o.total_tokens = ledger.coherent_total_tokens();
    true
}

/// Costo cumulativo gia' speso dai sub-run su questo `parent_anchor` (hard cap).
///
/// PUNTO UNICO (regola L) dello speso di una catena di sub-run: lo usano il
/// Guard 4 del prepare e il dimensionamento del dibattito (che deve vedere cosa
/// il consiglio ha gia' consumato prima di finanziare gli avvocati). NB: i
/// sub-run hanno `run_id` PROPRI nel ledger, quindi un'aggregazione per-run del
/// padre NON li vedrebbe: la fonte e' `nexus_subagent_runs.cost_usd`, popolata
/// alla finalizzazione di ogni figlio.
pub(crate) async fn cumulative_cost(pool: &sqlx::PgPool, anchor: Uuid) -> f64 {
    // pool: gia' instradato sul progetto dal chiamante (nexus_subagent_runs migrata).
    sqlx::query_scalar::<_, f64>(
        "SELECT COALESCE(SUM(cost_usd), 0)::double precision \
         FROM nexus_subagent_runs WHERE parent_run_id = $1",
    )
    .bind(anchor)
    .fetch_one(pool)
    .await
    .unwrap_or(0.0)
}

/// Legge il param `background` (bool, default false) dall'input di
/// `dispatch_subagent`/`dispatch_subagents` (regola M: campo strutturato, mai
/// parsing di prosa). Dalla Fase D turn-on il param E' nello schema del tool; la
/// sua ATTIVAZIONE effettiva passa da [`background_active`] (kill-switch DB).
fn read_background_flag(input: &Value) -> bool {
    input
        .get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// PUNTO UNICO (regola L) del "background EFFETTIVAMENTE attivo": il chiamante ha
/// passato `background=true` E il kill-switch DB-driven
/// `orchestrator.background_fanin_enabled` e' ON (stesso flag del worker fan-in,
/// [`crate::fanin_worker::background_fanin_enabled`]). Se il flag e' OFF il param
/// viene IGNORATO e il dispatch resta SINCRONO: spegnere il flag riporta tutto a
/// sincrono a runtime (60s, senza redeploy) senza lasciare padri appesi (regola
/// G/H). Rete di sicurezza per il turn-on del fan-in async.
async fn background_active(ctx: &AgentToolContext, input: &Value) -> bool {
    read_background_flag(input) && crate::fanin_worker::background_fanin_enabled(&ctx.core.db).await
}

/// Handler del tool `dispatch_subagent` (singolo sub-run nativo).
pub async fn tool_dispatch_subagent(ctx: &AgentToolContext, input: &Value) -> String {
    let kind = match input.get("kind").and_then(Value::as_str) {
        Some(k) if !k.is_empty() => k.to_string(),
        _ => return err("parametro 'kind' obbligatorio"),
    };
    let task = match input.get("task").and_then(Value::as_str) {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => return err("parametro 'task' obbligatorio e non vuoto"),
    };
    let context_blob = input
        .get("context")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let expected_format = input
        .get("expected_output_format")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    // Fase D fan-in: dispatch asincrono opt-in dal param `background` (ora nello
    // schema del tool), gattato dal kill-switch DB (regola G/H). Default e flag-OFF
    // -> sincrono, comportamento invariato. Letto come CAMPO strutturato (regola M).
    let background = background_active(ctx, input).await;

    run_single_subagent(
        ctx,
        &kind,
        &task,
        &context_blob,
        &expected_format,
        None,
        background,
        // Il dispatch SINGOLO non e' il canale del piano: il todo_runner passa
        // sempre da `dispatch_subagents` (plurale), che porta `write_scope` per
        // task. Qui non c'e' uno scope dichiarato da misurare.
        &[],
    )
    .await
    .to_string()
}

/// Handler del tool `dispatch_subagents` (batch parallelo di sub-run nativi).
pub async fn tool_dispatch_subagents(ctx: &AgentToolContext, input: &Value) -> String {
    let tasks = match input.get("tasks").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a.clone(),
        _ => return err("parametro 'tasks' (array non vuoto) obbligatorio"),
    };
    let batch_max_tasks = read_batch_max_tasks(ctx).await;
    if tasks.len() as u64 > batch_max_tasks {
        return err(&format!("troppi task in un batch (max {batch_max_tasks})"));
    }
    let configured_max = read_max_parallel_subagents(ctx).await;
    let max_parallel = input
        .get("max_parallel")
        .and_then(|v| v.as_u64())
        .unwrap_or(configured_max)
        .clamp(1, configured_max) as usize;
    // Fase D fan-in: dispatch asincrono dell'intero batch, opt-in dal param
    // `background` (ora nello schema) gattato dal kill-switch DB (regola G/H). Il
    // ramo ISOLATO non supporta il background (l'apply serializzato dei worktree
    // esige che i sub-run siano TERMINATI prima di promuovere): quando
    // `background=true` il batch salta l'isolamento e va sempre sul ramo
    // sequenziale con `is_background=true` (scelta piu' semplice e sicura). Letto
    // come CAMPO strutturato (regola M).
    let background = background_active(ctx, input).await;

    let mut parsed: Vec<ParsedTask> = Vec::with_capacity(tasks.len());
    for (i, t) in tasks.iter().enumerate() {
        let kind = t
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let task = t
            .get("task")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if kind.is_empty() || task.trim().is_empty() {
            return err(&format!("task[{i}]: 'kind' e 'task' sono obbligatori"));
        }
        let context_blob = t
            .get("context")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let expected = t
            .get("expected_output_format")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // FASE 2: `write_scope` dichiarato dal task (aree file che il sub-run
        // scrive). Assente/vuoto -> `subtasks_are_disjoint` degradera' a false ->
        // ramo sequenziale (sicuro). Fonte: `Todo.write_scope` in `dispatch_wave`.
        let write_scope = write_scope_of(t);
        parsed.push(ParsedTask {
            kind,
            task,
            context_blob,
            expected,
            write_scope,
        });
    }

    // ── Gating isolamento (regola G + L): il ramo ISOLATO scatta SOLO se
    //    (1) il flag DB `orchestrator.subagent_isolation_enabled` e' ON (cache 60s)
    //        E la root del progetto e' isolabile (probe git fail-closed), E
    //    (2) i `write_scope` dei task sono banalmente disgiunti (funzione pura di
    //        PR1, punto unico). Altrimenti DEGRADA al ramo sequenziale/condiviso
    //        (identico a oggi). Con flag OFF (default) -> sempre sequenziale ->
    //        comportamento BIT-IDENTICO. ─────────────────────────────────────────
    //
    // L'apply e' l'unica fonte del commit del sub-run isolato e avviene solo qui.
    let scopes: Vec<Vec<String>> = parsed.iter().map(|p| p.write_scope.clone()).collect();
    // I/O (flag DB + probe git) separato dalla decisione pura: il probe scatta solo
    // se il flag e' ON (compute_isolation_available corto-circuita), poi la
    // disgiunzione e' pura e testabile (should_isolate_batch).
    // Il background salta l'isolamento (vedi doc sopra): il ramo isolato esige
    // sub-run terminati per l'apply serializzato, incompatibile con il fire-and-forget.
    let isolation_available = !background && compute_isolation_available(ctx).await;
    if should_isolate_batch(isolation_available, &scopes) {
        return run_batch_isolated(ctx, &parsed, max_parallel).await;
    }

    // ── Ramo sequenziale/condiviso (invariato con background=false) ────────────
    // Esecuzione a ondate concorrenti (cap conservativo). I guard per-sub
    // (enabled/whitelist/depth/cost) sono valutati per ogni sub-run; nel batch
    // il cost cap e' best-effort (race tollerata dato il cap conservativo).
    let results: Vec<Value> = if background {
        // BACKGROUND: PREPARE-ALL poi SPAWN-ALL (bug fan-in prematuro). Se
        // preparassimo+spawnassimo una alla volta, il 1o figlio potrebbe terminare
        // (task detached) mentre gli altri non sono ancora in nexus_subagent_runs
        // -> la COUNT del fan-in vedrebbe 0 rimasti -> enqueue PREMATURO del parent.
        // Inserendo TUTTE le row (running, is_background=true) PRIMA di spawnare
        // qualunque esecuzione, la COUNT e' corretta gia' quando il 1o figlio
        // chiude. I guard falliti producono un Value di errore (nessuna row
        // inserita, nessuno spawn per quel task).
        run_batch_background(ctx, &parsed).await
    } else {
        let mut results: Vec<Value> = Vec::with_capacity(parsed.len());
        for wave in parsed.chunks(max_parallel) {
            let futs = wave.iter().map(|p| run_parsed_task(ctx, p, None));
            let wave_res = futures::future::join_all(futs).await;
            results.extend(wave_res);
        }
        results
    };

    // Fan-in (regola M): raccogli i figli che hanno risposto col SEGNALE
    // STRUTTURATO `background_dispatched` (mai parsing di prosa) + i loro run_id,
    // cosi' il padre si sospende e riprende quando i sub-run background terminano.
    let child_run_ids: Vec<Value> = results
        .iter()
        .filter(|r| r.get("background_dispatched").and_then(Value::as_bool) == Some(true))
        .filter_map(|r| r.get(K_SUB_RUN_ID).cloned())
        .collect();
    let any_background = !child_run_ids.is_empty();

    let ok = results.iter().filter(|r| r.get("error").is_none()).count();
    let mut out = json!({
        "count": results.len(),
        "ok": ok,
        "failed": results.len() - ok,
        "results": results,
    });
    if any_background {
        out["background_dispatched"] = json!(true);
        out["child_run_ids"] = json!(child_run_ids);
    }
    // Fase C (coordinatore avversario, regola L/M): se il batch e' un PANEL di
    // review (almeno un sub-run ha dichiarato un `review`), compone il verdetto
    // aggregato dai segnali strutturati `outcome.review` — mai dalla prosa. I
    // sub-run non-review non hanno `review` e il panel non viene aggiunto. Le
    // review sono read-only (write_scope vuoto) -> restano nel ramo sequenziale.
    let outcomes: Vec<Value> = results
        .iter()
        .map(|r| r.get("outcome").cloned().unwrap_or(Value::Null))
        .collect();
    if let Some(panel) = nexus_agent_graph::decisions::compose_panel_verdict(
        &outcomes,
        &read_quorum_policy(ctx).await,
    ) {
        out["panel_verdict"] = panel.to_value();
    }
    // Consiglio a monte (regola L/M): se il batch e' un PANEL advisory (almeno una
    // figura ha dichiarato un `advisory`), compone la SINTESI aggregata dai segnali
    // strutturati `outcome.advisory` — mai dalla prosa. Simmetrico al panel di
    // review; il coordinatore (run padre) legge `advisory_synthesis` per costruire
    // il piano rispettando i requisiti e fermandosi sui veti (verdict=block).
    // Roster SelfDeclared: in un batch misto le figure advisory si riconoscono
    // solo dal voto dichiarato, il numero di convocate non e' noto (a differenza
    // del consiglio a monte e del panel multi-provider, che dichiarano il roster).
    if let Some(synthesis) = nexus_agent_graph::decisions::compose_advisory_synthesis(
        &outcomes,
        &read_advisory_policy(ctx).await,
        AdvisoryRoster::SelfDeclared,
    ) {
        out["advisory_synthesis"] = synthesis.to_value();
    }
    out.to_string()
}

/// Un task del batch, parsato dall'input del tool. `write_scope` alimenta il gating
/// dell'isolamento fisico (FASE 2).
/// PUNTO UNICO (regola L) di estrazione del `write_scope` da un task del batch:
/// lista di prefissi file dichiarati dal task; assente/non-array -> vuoto (il
/// gating di isolamento degrada a sequenziale, ramo sicuro).
fn write_scope_of(t: &Value) -> Vec<String> {
    t.get("write_scope")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

struct ParsedTask {
    kind: String,
    task: String,
    context_blob: String,
    expected: String,
    write_scope: Vec<String>,
}

/// Chiave della setting kill-switch dell'isolamento fisico (regola G, opt-in).
const ISOLATION_ENABLED_SETTING: &str = "orchestrator.subagent_isolation_enabled";

/// Legge il flag `orchestrator.subagent_isolation_enabled` (default false).
/// PUNTO UNICO (regola L) della lettura del flag: lo condividono il batch tool
/// (`compute_isolation_available`) e il run-init del grafo
/// (`native_engine::compute_run_isolation_available`), cosi' il gate del planner e
/// l'esecuzione reale dell'isolamento vedono lo STESSO valore. Prende `&PgPool`
/// (unico bisogno reale): DB down o chiave assente -> `false` (fail-safe: nessun
/// isolamento, ramo sequenziale come oggi).
///
/// La cache 60s e' quella del punto unico dei settings (`nexus_auth`), chiavata
/// per DATABASE. Qui sopra ce n'era una SECONDA, di processo e con chiave `()`:
/// non toglieva un round-trip (sotto c'era gia' `get_bool_setting`, che cacha) e
/// perdeva la distinzione fra i database — il flag letto sul meta rispondeva
/// anche per un `<slug>_nexus`, e nei test il primo lettore decideva per tutti
/// (stessa causa dei sei test flaky di `internal_routing`, 2026-07-27).
pub(crate) async fn isolation_flag_enabled(db: &sqlx::PgPool) -> bool {
    nexus_auth::get_bool_setting(db, ISOLATION_ENABLED_SETTING)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// Isolamento DISPONIBILE per questo batch (regola G): flag DB ON (cache 60s) AND
/// la root del progetto e' un repo git in cui `git worktree` e' utilizzabile
/// (probe FAIL-CLOSED, punto unico `nexus_tool_kit::worktree::probe_isolatable`).
/// Corto-circuito: se `is_git_repo` (dalla sessione) e' false, niente probe git
/// (nessun I/O sul path caldo su progetti non-git). Flag OFF -> `false` senza
/// alcun probe.
async fn compute_isolation_available(ctx: &AgentToolContext) -> bool {
    if !isolation_flag_enabled(&ctx.core.db).await {
        return false;
    }
    if !ctx.core.is_git_repo {
        return false;
    }
    nexus_tool_kit::worktree::probe_isolatable(&ctx.core.root_path).await
}

/// Decisione PURA del gating isolamento (regola L + M, testabile senza I/O): il
/// ramo isolato scatta SOLO se l'isolamento e' disponibile (flag ON + root
/// isolabile, gia' risolto a monte) E i `write_scope` dei task sono banalmente
/// disgiunti (funzione pura di PR1, punto unico). Con `isolation_available=false`
/// (flag OFF, default) ritorna SEMPRE false -> ramo sequenziale -> bit-identico.
fn should_isolate_batch(isolation_available: bool, scopes: &[Vec<String>]) -> bool {
    isolation_available && nexus_agent_graph::decisions::subtasks_are_disjoint(scopes)
}

/// Esegue il batch di sub-run in ISOLAMENTO FISICO (FASE 2). Precondizione (gia'
/// verificata dal chiamante): flag ON, root isolabile, `write_scope` disgiunti.
///
/// Flusso (regola H, mai toppe):
///  1. GC best-effort dei worktree orfani del progetto (evita accumulo su crash).
///  2. `base = head_commit(root)` una volta per il batch (persistito per replay).
///  3. Per ogni task: crea un worktree effimero e prenota la row sub-run.
///  4. Esegue i sub-run IN PARALLELO (a ondate `max_parallel`), ciascuno con
///     `working_root = Some(worktree)` -> ctx isolato (autocommit/reindex soppressi).
///  5. Dopo che TUTTI sono terminati, APPLY SERIALIZZATO (un worktree alla volta,
///     mai concorrente: index/refs `.git` condivisi) via `apply_worktree_atomic`.
///     - Applied  -> raccoglie i file promossi per il reindex-once.
///     - Conflict -> quel sub-run e' marcato fallito (root intatta), gli altri
///                   proseguono (fail-loud, regola M: esito da `ApplyOutcome`).
///     - NoChanges-> nulla da promuovere.
///  6. Reindex UNA volta sui soli file realmente promossi alla root.
///  7. CLEANUP GARANTITO: `remove_worktree` per OGNI handle in OGNI esito (il
///     cleanup e' esplicito e async — mai Drop sincrono su runtime tokio, finding
///     del design; teardown idempotente e tollerante ai lock Windows).
async fn run_batch_isolated(
    ctx: &AgentToolContext,
    parsed: &[ParsedTask],
    max_parallel: usize,
) -> String {
    let root = ctx.core.root_path.clone();
    let project_id = ctx.core.project_id;
    let proj_pool =
        match crate::project_db_routes::project_data_pool_from(&ctx.core.db, project_id).await {
            Ok(p) => p,
            Err(e) => {
                // DB progetto non disponibile: impossibile prenotare le row
                // sub-run dell'isolamento. Stesso trattamento di head_commit
                // fallito: degrada al ramo sequenziale (i singoli sub-run
                // produrranno i loro rifiuti strutturati se il DB resta giu').
                tracing::warn!(
                    target: "mcp_core::subagent_native",
                    project_id = %project_id,
                    error = %e,
                    "isolamento: DB progetto non disponibile, degrado a sequenziale"
                );
                return run_batch_sequential(ctx, parsed, max_parallel).await;
            }
        };

    // (0) LOCK PER-ROOT CROSS-BATCH (regola H + L, punto unico in worktree.rs):
    //     serializza l'INTERA sezione che tocca l'area `.git`/worktree condivisa del
    //     progetto (GC -> creazione worktree -> ondate -> apply -> cleanup). Due batch
    //     isolati concorrenti sullo STESSO progetto (session distinte, stessa
    //     project_root: la guardia 409 e' per-session, non per-progetto) vengono
    //     serializzati qui. Chiude insieme D1 (merge/commit concorrenti sulla stessa
    //     .git -> index corrotto) e D2 (il GC di un batch che cancella i worktree in
    //     volo di un altro: quando il secondo batch ottiene il lock, il primo ha gia'
    //     applicato e ripulito i suoi worktree). Il guard e' `OwnedMutexGuard<()>`
    //     (Send, `'static`): tenerlo attraverso i `.await` delle ondate join_all e'
    //     corretto. Rilascio naturale a fine funzione (Drop).
    let _root_guard = nexus_tool_kit::worktree::lock_project_root(&root).await;

    // (1) GC orfani: bonifica i worktree residui di batch PRECEDENTI (crash / remove
    //     fallito). Preserva i worktree dei sub-run ancora `running` (batch
    //     concorrente sullo stesso progetto): passiamo come "attivi" i loro run_id
    //     dal DB, cosi' il GC non tocca risorse legittime (regola E). Best-effort.
    let active_run_ids = running_subagent_ids(&proj_pool, project_id).await;
    let removed = nexus_tool_kit::worktree::gc_orphan_worktrees(&root, &active_run_ids).await;
    if removed > 0 {
        tracing::info!(
            target: "mcp_core::subagent_native",
            removed,
            "isolamento: GC ha bonificato worktree orfani prima del batch"
        );
    }

    // (2) base commit del batch (una volta, riusato da ogni worktree e in replay).
    let base_commit = match nexus_tool_kit::worktree::head_commit(&root).await {
        Ok(b) => b,
        Err(e) => {
            // Impossibile risolvere HEAD: degrada al ramo sequenziale (mai fallire
            // l'intero batch per un problema di setup isolamento).
            tracing::warn!(
                target: "mcp_core::subagent_native",
                error = %e,
                "isolamento: head_commit fallito, degrado a sequenziale"
            );
            return run_batch_sequential(ctx, parsed, max_parallel).await;
        }
    };

    // (3) Crea un worktree per ogni task, PRIMA dell'esecuzione. Se la creazione di
    //     uno fallisce, degrada l'intero batch a sequenziale (cleanup di quelli gia'
    //     creati garantito) — mai un batch a isolamento parziale.
    let mut handles: Vec<nexus_tool_kit::worktree::WorktreeHandle> =
        Vec::with_capacity(parsed.len());
    for _ in parsed {
        let run_id = Uuid::new_v4();
        match nexus_tool_kit::worktree::create_ephemeral_worktree(&root, run_id, &base_commit).await
        {
            Ok(h) => handles.push(h),
            Err(e) => {
                tracing::warn!(
                    target: "mcp_core::subagent_native",
                    error = %e,
                    "isolamento: create_ephemeral_worktree fallito, degrado a sequenziale"
                );
                // Cleanup dei worktree gia' creati prima di degradare.
                for h in &handles {
                    let _ = nexus_tool_kit::worktree::remove_worktree(h).await;
                }
                return run_batch_sequential(ctx, parsed, max_parallel).await;
            }
        }
    }

    // (4) Esecuzione PARALLELA a ondate, ogni sub-run sul proprio worktree.
    let mut results: Vec<Value> = Vec::with_capacity(parsed.len());
    for wave in parsed
        .iter()
        .zip(handles.iter())
        .collect::<Vec<_>>()
        .chunks(max_parallel)
    {
        let futs = wave.iter().map(|(p, h)| {
            let slot = IsolationSlot {
                run_id: h.run_id,
                worktree_path: h.path.clone(),
                base_commit: h.base_commit.clone(),
            };
            async move {
                run_parsed_task(ctx, p, Some(&slot))
                .await
            }
        });
        let wave_res = futures::future::join_all(futs).await;
        results.extend(wave_res);
    }

    // (5) APPLY SERIALIZZATO (un worktree alla volta, mai concorrente) + raccolta
    //     dei file promossi per il reindex-once. Solo i sub-run RIUSCITI (nessun
    //     campo "error" nel result) vengono applicati; un sub-run fallito lascia il
    //     suo worktree scartato = rollback naturale.
    let mut promoted: Vec<String> = Vec::new();
    for (idx, (result, handle)) in results.iter_mut().zip(handles.iter()).enumerate() {
        if result.get("error").is_some() {
            continue; // sub-run fallito: niente apply (worktree scartato).
        }
        match nexus_tool_kit::worktree::apply_worktree_atomic(&root, handle).await {
            Ok(nexus_tool_kit::worktree::ApplyOutcome::Applied) => {
                promoted.extend(nexus_tool_kit::worktree::promoted_files(handle).await);
            }
            Ok(nexus_tool_kit::worktree::ApplyOutcome::NoChanges) => {}
            Ok(nexus_tool_kit::worktree::ApplyOutcome::Conflict { files }) => {
                // Esito strutturato (regola M): il sub-run e' fallito per conflitto,
                // la root e' intatta (merge --abort), gli altri proseguono.
                tracing::warn!(
                    target: "mcp_core::subagent_native",
                    task_index = idx,
                    conflicted_files = ?files,
                    "isolamento: apply in CONFLITTO, root intatta, sub-run marcato fallito"
                );
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("status".into(), json!("conflict"));
                    obj.insert(
                        "error".into(),
                        json!(format!("apply in conflitto su {} file", files.len())),
                    );
                    obj.insert("conflicted_files".into(), json!(files));
                }
            }
            Err(e) => {
                tracing::error!(
                    target: "mcp_core::subagent_native",
                    task_index = idx,
                    error = %e,
                    "isolamento: apply fallito (errore git), sub-run marcato fallito"
                );
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("status".into(), json!("apply_failed"));
                    obj.insert("error".into(), json!(format!("apply fallito: {e}")));
                }
            }
        }
    }

    // (6) CLEANUP GARANTITO di OGNI worktree, in OGNI esito (successo, conflitto,
    //     errore). Esplicito e async (mai Drop sincrono). Idempotente e tollerante
    //     ai lock Windows (remove --force nel modulo).
    for handle in &handles {
        if let Err(e) = nexus_tool_kit::worktree::remove_worktree(handle).await {
            tracing::warn!(
                target: "mcp_core::subagent_native",
                run_id = %handle.run_id,
                error = %e,
                "isolamento: cleanup worktree fallito (verra' ripreso dal GC al prossimo batch)"
            );
        }
    }

    // (7) Reindex UNA volta sui soli file promossi alla root (dedup dei path: piu'
    //     sub-run potrebbero aver toccato lo stesso file solo se non-disgiunti, che
    //     qui non accade, ma il dedup e' innocuo e barato).
    reindex_promoted_once(ctx, &promoted).await;

    let ok = results.iter().filter(|r| r.get("error").is_none()).count();
    json!({
        "count": results.len(),
        "ok": ok,
        "failed": results.len() - ok,
        "isolated": true,
        "promoted_files": promoted.len(),
        "results": results,
    })
    .to_string()
}

/// Ramo BATCH BACKGROUND (Fase D fan-in): PREPARE-ALL poi SPAWN-ALL. Insersce
/// TUTTE le row `nexus_subagent_runs` (running, is_background=true) PRIMA di
/// spawnare qualunque esecuzione, cosi' la COUNT del fan-in (`fanin_enqueue_if_
/// last`) e' corretta anche se il 1o figlio termina prestissimo (bug enqueue
/// prematuro). Ordine dei risultati = ordine dei task. I task il cui prepare
/// fallisce (guard non superato / INSERT fallito) non generano row ne' spawn: il
/// loro Value di errore entra tra i risultati e NON conta nel fan-in.
///
/// PARALLELISMO: nel background non c'e' `max_parallel` da rispettare qui — il
/// prepare e' sequenziale (guard depth/cost coerenti, catena `running`), poi
/// OGNI figlio gira nel proprio task detached (il concorrere e' del runtime, non
/// di questo loop). Il padre e' sospeso comunque finche' l'ultimo non chiude.
async fn run_batch_background(ctx: &AgentToolContext, parsed: &[ParsedTask]) -> Vec<Value> {
    // (1) PREPARE-ALL: guard + INSERT + ensure_child, SEQUENZIALE. Ogni Ok porta un
    //     SubagentExecInputs (row gia' inserita, `running`); ogni Err un Value di
    //     errore da restituire in ordine. Nessuno spawn ancora.
    let mut prepared: Vec<Result<SubagentExecInputs, Value>> = Vec::with_capacity(parsed.len());
    for p in parsed {
        prepared.push(
            prepare_subagent_run(
                ctx,
                &p.kind,
                &p.task,
                &p.context_blob,
                &p.expected,
                None,
                true,
                None,
                &p.write_scope,
            )
            .await,
        );
    }
    // (2) SPAWN-ALL: solo ORA che TUTTE le row sono inserite, spawna le esecuzioni.
    //     Il 1o figlio che chiude vede in nexus_subagent_runs anche gli altri
    //     (ancora `running`) -> COUNT>0 -> nessun enqueue prematuro.
    prepared
        .into_iter()
        .map(|r| match r {
            Ok(exec) => spawn_background_subagent(exec),
            Err(err) => err,
        })
        .collect()
}

/// Ramo sequenziale/condiviso estratto per il degrado dall'isolamento (stessa
/// logica del ramo principale di `tool_dispatch_subagents`, punto unico). `None`
/// come `working_root` -> comportamento invariato.
async fn run_batch_sequential(
    ctx: &AgentToolContext,
    parsed: &[ParsedTask],
    max_parallel: usize,
) -> String {
    let mut results: Vec<Value> = Vec::with_capacity(parsed.len());
    for wave in parsed.chunks(max_parallel) {
        let futs = wave.iter().map(|p| {
            // Degrado dell'isolamento a sequenziale: mai background (questo ramo e'
            // raggiunto solo dal fallback dell'isolato, che esclude il background).
            run_parsed_task(ctx, p, None)
        });
        let wave_res = futures::future::join_all(futs).await;
        results.extend(wave_res);
    }
    let ok = results.iter().filter(|r| r.get("error").is_none()).count();
    json!({
        "count": results.len(),
        "ok": ok,
        "failed": results.len() - ok,
        "results": results,
    })
    .to_string()
}

/// Reindex UNA volta i file effettivamente promossi alla root dopo l'apply
/// serializzato (i sub-run isolati hanno il reindex per-scrittura SOPPRESSO in PR3).
/// Punto unico di reindex (regola L): `crate::projects::reindex_single_file`.
///
/// `promoted` (da `git diff --name-only base..branch`) INCLUDE anche i file
/// CANCELLATI dal sub-run. Per un file cancellato `reindex_single_file` farebbe
/// `read_to_string` -> Err scartato, lasciando i chunk vettoriali STANTII nell'indice
/// semantico. Per questo, prima di reindicizzare, si controlla l'esistenza del file
/// sul filesystem (segnale strutturato, regola M): se NON esiste, si cancellano i suoi
/// punti dal code index via il punto unico ESISTENTE
/// `vector_memory::delete_code_index_file_points` (regola L, stesso path relativo con
/// separatori '/' che usa `indexing.rs`); se esiste, comportamento invariato.
///
/// Best-effort: un fallimento del reindex/cleanup non e' un errore del batch.
async fn reindex_promoted_once(ctx: &AgentToolContext, promoted: &[String]) {
    if promoted.is_empty() {
        return;
    }
    // Dedup dei path (stabile, senza ordinare per non introdurre non-determinismo).
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let root = &ctx.core.root_path;
    for rel in promoted {
        if !seen.insert(rel.as_str()) {
            continue;
        }
        let target = root.join(rel);
        // File cancellato dal sub-run (promosso ma non piu' sul filesystem): purga i
        // chunk dall'indice invece di tentare un reindex che fallirebbe in read.
        match tokio::fs::try_exists(&target).await {
            Ok(false) => {
                let rel_slash = rel.replace('\\', "/");
                let _ = crate::vector_memory::delete_code_index_file_points(
                    &ctx.core.db,
                    ctx.core.project_id,
                    &rel_slash,
                )
                .await;
            }
            // Esiste (Ok(true)) o esito incerto (Err su try_exists): comportamento
            // invariato, reindex del file promosso.
            _ => {
                let _ = crate::projects::reindex_single_file(
                    &ctx.core.db,
                    &ctx.neural,
                    ctx.core.project_id,
                    root,
                    &target,
                )
                .await;
            }
        }
    }
    tracing::info!(
        target: "mcp_core::subagent_native",
        promoted = seen.len(),
        "isolamento: reindex-once dei file promossi completato"
    );
}

/// Run-id dei sub-run ancora `running` per il progetto (worktree potenzialmente
/// vivi di un batch concorrente). Usati come whitelist del GC filesystem: il GC
/// preserva le dir il cui run_id e' qui, rimuove le altre (regola E: mai toccare
/// risorse legittime di un altro batch). Best-effort: DB down -> lista vuota (il
/// GC diventa piu' aggressivo ma resta filtrato per la root del progetto).
async fn running_subagent_ids(pool: &sqlx::PgPool, project_id: Uuid) -> Vec<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM nexus_subagent_runs WHERE project_id = $1 AND status = 'running'",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default()
}

/// Tetto di sicurezza per l'ampiezza dell'ondata concorrente di sub-agenti.
const MAX_PARALLEL_HARD_CAP: u64 = 8;

/// Legge `orchestrator.max_parallel_subagents` (default 3) come tetto effettivo del
/// parallelismo (PUNTO UNICO condiviso col pannello admin "Agenti Paralleli").
async fn read_max_parallel_subagents(ctx: &AgentToolContext) -> u64 {
    let v: Option<String> = sqlx::query_scalar(
        "SELECT value FROM settings WHERE key = 'orchestrator.max_parallel_subagents'",
    )
    .fetch_optional(&*ctx.core.db)
    .await
    .ok()
    .flatten();
    v.and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(3)
        .clamp(1, MAX_PARALLEL_HARD_CAP)
}

/// Backstop assoluto al numero di task di UN batch `dispatch_subagents`: previene
/// che un valore di setting insensato faccia esplodere il numero di sub-run.
const BATCH_MAX_TASKS_HARD_CAP: u64 = 32;

/// Numero massimo di task in UN batch `dispatch_subagents` (regola G: DB-driven,
/// niente literal hardcoded). Default 8, clampato al backstop
/// [`BATCH_MAX_TASKS_HARD_CAP`]. Sostituisce il vecchio `tasks.len() > 8` fisso.
async fn read_batch_max_tasks(ctx: &AgentToolContext) -> u64 {
    let v: Option<String> = sqlx::query_scalar(
        "SELECT value FROM settings WHERE key = 'orchestrator.subagent_batch_max_tasks'",
    )
    .fetch_optional(&*ctx.core.db)
    .await
    .ok()
    .flatten();
    v.and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(8)
        .clamp(1, BATCH_MAX_TASKS_HARD_CAP)
}

/// Policy del quorum del panel di review (Fase C, regola G: DB-driven, niente
/// hardcode). Safe-default coincidente con `QuorumPolicy::default` se le chiavi
/// mancano: 1 voto valido conclusivo, veto avversario su high-severity attivo.
async fn read_quorum_policy(ctx: &AgentToolContext) -> QuorumPolicy {
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN (
            'orchestrator.review_quorum_min_valid',
            'orchestrator.review_fail_on_high_severity'
        )",
    )
    .fetch_all(&*ctx.core.db)
    .await
    .unwrap_or_default();
    let mut policy = QuorumPolicy::default();
    for row in rows {
        let k: String = row.get("key");
        let v: String = row.get("value");
        match k.as_str() {
            "orchestrator.review_quorum_min_valid" => {
                if let Ok(n) = v.trim().parse::<usize>() {
                    policy.min_valid_verdicts = n.max(1);
                }
            }
            "orchestrator.review_fail_on_high_severity" => {
                policy.fail_on_high_severity = settings_flag(v.trim());
            }
            _ => {}
        }
    }
    policy
}

/// Policy del quorum del panel ADVISORY (gemello di [`read_quorum_policy`], regola
/// L): letta dai settings `orchestrator.council_advisory_*` (mig 0548) e passata al
/// punto unico PURO `compose_advisory_synthesis`. Niente hardcode; safe-default
/// coincidente con `AdvisoryPolicy::default` se le chiavi mancano.
pub(crate) async fn read_advisory_policy(ctx: &AgentToolContext) -> AdvisoryPolicy {
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN (
            'orchestrator.council_advisory_min_valid',
            'orchestrator.council_advisory_quorum_pct',
            'orchestrator.council_advisory_block_on_high_severity'
        )",
    )
    .fetch_all(&*ctx.core.db)
    .await
    .unwrap_or_default();
    let mut policy = AdvisoryPolicy::default();
    for row in rows {
        let k: String = row.get("key");
        let v: String = row.get("value");
        match k.as_str() {
            "orchestrator.council_advisory_min_valid" => {
                if let Ok(n) = v.trim().parse::<usize>() {
                    policy.min_valid_advisories = n.max(1);
                }
            }
            "orchestrator.council_advisory_quorum_pct" => {
                if let Ok(n) = v.trim().parse::<u8>() {
                    policy.quorum_pct = n.min(100);
                }
            }
            "orchestrator.council_advisory_block_on_high_severity" => {
                policy.block_on_high_severity = settings_flag(v.trim());
            }
            _ => {}
        }
    }
    policy
}

// ─── Consiglio a monte: convocazione PROGRAMMATICA delle figure (regola L/M) ──
//
// A differenza del panel di review a VALLE (dispatchato dal modello via
// dispatch_subagents), il consiglio a MONTE viene convocato dal MOTORE — non dal
// modello — PRIMA che il run primario agisca: i modelli non-frontier non convocano
// le figure da soli nonostante la direttiva <consiglio_analisi>. `spawn_agent_run`
// costruisce il ctx, seleziona le figure pertinenti e chiama `convene_council`; la
// sintesi risultante viene iniettata nel primo messaggio del run. Best-effort:
// qualunque fallimento -> None e il run primario prosegue invariato.

/// Configurazione DB-driven (regola G) della SELEZIONE figure del consiglio.
/// Caricata dai settings (mig 0553); passata al selettore PURO
/// [`select_council_figures`]. Nessun kind hardcoded nel codice: le figure sono un
/// DATO nel DB (sono `kind`, non nomi-modello).
pub(crate) struct CouncilConfig {
    /// Figure sempre convocate (trasversali): CSV `orchestrator.council_figures`.
    /// Sono un DEFAULT cieco: nessun segnale del task le ha scelte.
    pub base_figures: Vec<String>,
    /// Assi d'ambito: ognuno convoca le proprie figure quando il testo del task
    /// tocca le sue keyword. PUNTO UNICO dell'attivazione per ambito (regola L):
    /// prima esisteva il solo asse infra, scritto a mano nel selettore; un
    /// secondo ambito (interfaccia) avrebbe richiesto di ricopiarne il ramo.
    /// L'elenco degli assi e' un DATO (`orchestrator.council_domain_axes`).
    pub domain_axes: Vec<CouncilDomainAxis>,
    /// Cap del numero di figure convocate (`orchestrator.council_max_figures`).
    pub max_figures: usize,
}

/// Un asse d'ambito del consiglio: keyword che lo attivano + figure che convoca.
pub(crate) struct CouncilDomainAxis {
    /// Nome dell'asse (`infra`, `ui`, ...): e' il prefisso delle sue chiavi
    /// `orchestrator.council_<name>_{figures,keywords}` e l'etichetta nei log.
    pub name: String,
    /// Keyword d'ambito, match substring case-insensitive.
    pub keywords: Vec<String>,
    /// Figure convocate quando l'asse e' attivo.
    pub figures: Vec<String>,
}

/// Configurazione DB-driven del panel multi-provider. Il kind e il purpose sono
/// dati nel DB: il codice decide solo il flusso, non quali provider/modelli usare.
pub(crate) struct MultiProviderConfig {
    pub enabled: bool,
    pub kind: String,
    pub purpose: String,
    pub max_providers: usize,
    pub min_providers: usize,
}

/// Segnale strutturato (regola M) del degrado del panel multi-provider quando
/// non ci sono abbastanza provider distinti o il purpose non e' risolvibile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MultiProviderDegradeReason {
    PurposeUnavailable,
    InsufficientProviderDiversity,
}

/// Motivo strutturato del degrado del Consiglio delle Competenze (regola M).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CouncilDegradeReason {
    SubagentsDisabled,
    BuildCtxFailed,
    SynthesisUnavailable,
}

/// Stato strutturato del parere di UNA figura del consiglio (regola M).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FigureAdvisoryStatus {
    PrepareFailed,
    RunFailed,
    RunTimeout,
    CompletedNoAdvisory,
    InvalidAdvisory,
    AdvisoryOk,
}

/// Stato UI di UNA figura durante la convocazione parallela del consiglio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CouncilFigureTaskStatus {
    Pending,
    Running,
    Done,
    Failed,
}

/// Task del consiglio mostrato in UI mentre le figure lavorano in parallelo.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct CouncilFigureTask {
    pub kind: String,
    pub status: CouncilFigureTaskStatus,
}

/// Costruisce la lista task per la UI: figure ancora in corso -> `running`,
/// completate con parere valido -> `done`, altrimenti `failed`.
pub(crate) fn council_figure_tasks(
    figures: &[String],
    completed_reports: &[FigureAdvisoryReport],
) -> Vec<CouncilFigureTask> {
    use std::collections::HashMap;
    let done: HashMap<_, _> = completed_reports
        .iter()
        .map(|r| (r.kind.as_str(), r))
        .collect();
    figures
        .iter()
        .map(|kind| {
            let status = match done.get(kind.as_str()) {
                Some(r) if r.status == FigureAdvisoryStatus::AdvisoryOk => {
                    CouncilFigureTaskStatus::Done
                }
                Some(_) => CouncilFigureTaskStatus::Failed,
                None if completed_reports.is_empty() => CouncilFigureTaskStatus::Running,
                None => CouncilFigureTaskStatus::Running,
            };
            CouncilFigureTask {
                kind: kind.clone(),
                status,
            }
        })
        .collect()
}

/// Report per-figura del consiglio: esito macchina + messaggio display.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct FigureAdvisoryReport {
    pub kind: String,
    pub status: FigureAdvisoryStatus,
    pub detail_code: String,
    pub detail_message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advisory_verdict: Option<String>,
    /// Parere strutturato COMPLETO della figura (verdict + requirements + risks +
    /// recommendations + concerns), propagato alla UI per far leggere il testo di
    /// ogni consiglio. `None` se la figura non ha prodotto un parere valido.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advisory: Option<serde_json::Value>,
    /// Provider/model EFFETTIVI su cui la figura ha girato (segnale strutturato,
    /// non prosa). `None` per le figure respinte a monte (es. guard depth) che non
    /// hanno mai risolto un modello. Propagato alla UI per mostrare la provenienza
    /// di ogni parere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_run_id: Option<String>,
}

/// Esito del pre-step Consiglio delle Competenze: sintesi attiva o degrado esplicito.
/// `None` dal call site significa che i gate pre-convocazione non sono passati
/// (feature off, keyword miss, nessuna figura): nessun segnale UI.
///
/// `synthesis` e' BOXATA: la sintesi e' molto piu' grande del ramo degradato
/// (che porta solo un enum di causa), e senza indirezione ogni valore dell'enum
/// — anche i degradi — pagherebbe la dimensione del caso peggiore.
#[derive(Debug, Clone)]
pub(crate) enum CouncilConveneOutcome {
    Active {
        synthesis: Box<AdvisorySynthesis>,
        figures: Vec<String>,
        figure_reports: Vec<FigureAdvisoryReport>,
    },
    Degraded {
        reason: CouncilDegradeReason,
        figures: Vec<String>,
        figure_reports: Vec<FigureAdvisoryReport>,
    },
}

impl CouncilConveneOutcome {
    pub(crate) fn degradation_reason_code(&self) -> Option<&'static str> {
        match self {
            Self::Active { .. } => None,
            Self::Degraded {
                reason: CouncilDegradeReason::SubagentsDisabled,
                ..
            } => Some("subagents_disabled"),
            Self::Degraded {
                reason: CouncilDegradeReason::BuildCtxFailed,
                ..
            } => Some("build_ctx_failed"),
            Self::Degraded {
                reason: CouncilDegradeReason::SynthesisUnavailable,
                ..
            } => Some("synthesis_unavailable"),
        }
    }

    /// Blocco `<consiglio_sintesi>` da anteporre al primo messaggio; vuoto se degradato.
    pub(crate) fn render_block(&self) -> String {
        match self {
            Self::Active { synthesis, .. } => render_council_synthesis(synthesis),
            Self::Degraded { .. } => String::new(),
        }
    }

    /// Valore STRUTTURATO della sintesi (regola M) per il seed pre-run
    /// (`pre_run_advisory_synthesis`) e l'enforcement al tool_dispatch: cosi' un
    /// verdetto `block` del consiglio FERMA l'esecuzione in modo deterministico,
    /// non solo come guida testuale. Simmetrico a
    /// [`MultiProviderPanelOutcome::advisory_synthesis_value`]. `None` se degradato.
    pub(crate) fn advisory_synthesis_value(&self) -> Option<serde_json::Value> {
        match self {
            Self::Active { synthesis, .. } => Some(synthesis.to_value()),
            Self::Degraded { .. } => None,
        }
    }

    pub(crate) fn figures(&self) -> &[String] {
        match self {
            Self::Active { figures, .. } | Self::Degraded { figures, .. } => figures.as_slice(),
        }
    }

    pub(crate) fn figure_reports(&self) -> &[FigureAdvisoryReport] {
        match self {
            Self::Active { figure_reports, .. } | Self::Degraded { figure_reports, .. } => {
                figure_reports.as_slice()
            }
        }
    }
}

/// Coppia provider/model usata da un analista del panel multi-provider.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct PanelProviderEntry {
    pub provider: String,
    pub model: String,
}

/// Esito del panel multi-provider: sintesi attiva oppure degrado esplicito.
/// `synthesis` BOXATA per lo stesso motivo di [`CouncilConveneOutcome`]: il ramo
/// degradato porta solo due contatori e non deve pagare la dimensione della
/// sintesi.
#[derive(Debug, Clone)]
pub(crate) enum MultiProviderPanelOutcome {
    Active {
        synthesis: Box<AdvisorySynthesis>,
        provider_count: usize,
        panel_providers: Vec<PanelProviderEntry>,
        /// Parere INDIVIDUALE di ogni provider (prima della sintesi): provenienza
        /// + advisory completo per figura, cosi' la UI mostra la DIFFERENZA tra i
        /// provider, non solo l'aggregato. Riusa `FigureAdvisoryReport` e la
        /// classificazione delle figure del consiglio (regola L).
        provider_reports: Vec<FigureAdvisoryReport>,
    },
    Degraded {
        reason: MultiProviderDegradeReason,
        got: usize,
        min: usize,
    },
}

impl MultiProviderPanelOutcome {
    pub(crate) fn degradation_reason_code(&self) -> Option<&'static str> {
        match self {
            Self::Active { .. } => None,
            Self::Degraded {
                reason: MultiProviderDegradeReason::PurposeUnavailable,
                ..
            } => Some("purpose_unavailable"),
            Self::Degraded {
                reason: MultiProviderDegradeReason::InsufficientProviderDiversity,
                ..
            } => Some("insufficient_provider_diversity"),
        }
    }

    /// Blocco `<multi_provider_sintesi>` da anteporre al primo messaggio; vuoto se degradato.
    pub(crate) fn render_block(&self) -> String {
        match self {
            Self::Active { synthesis, .. } => render_multi_provider_synthesis(synthesis),
            Self::Degraded { .. } => String::new(),
        }
    }

    /// Valore strutturato per il seed pre-run nel grafo (regola M).
    pub(crate) fn advisory_synthesis_value(&self) -> Option<serde_json::Value> {
        match self {
            Self::Active { synthesis, .. } => Some(synthesis.to_value()),
            Self::Degraded { .. } => None,
        }
    }
}

/// Carica la config di selezione figure dai settings (regola G). Safe-default se le
/// chiavi mancano: nessuna figura base -> il selettore ritorna vuoto -> nessuna
/// convocazione (fail-closed sulla selezione; la feature va accesa esplicitamente coi
/// settings di mig 0553 oltre al kill-switch `orchestrator.council_enabled`).
pub(crate) async fn read_council_config(db: &sqlx::PgPool) -> CouncilConfig {
    // Formato CSV dei settings: punto unico in `nexus_auth` (regola L).
    let base = nexus_auth::get_csv_setting(db, "orchestrator.council_figures").await;
    // Gli assi d'ambito sono un DATO: aggiungerne uno e' una riga di settings,
    // non un ramo nel selettore (regola G). `infra` resta il default storico se
    // la chiave manca, cosi' il comportamento pre-esistente non dipende dalla
    // migrazione che introduce l'elenco.
    let axis_names = {
        let declared = nexus_auth::get_csv_setting(db, "orchestrator.council_domain_axes").await;
        if declared.is_empty() {
            vec!["infra".to_string()]
        } else {
            declared
        }
    };
    let mut domain_axes = Vec::with_capacity(axis_names.len());
    for name in axis_names {
        let figures =
            nexus_auth::get_csv_setting(db, &format!("orchestrator.council_{name}_figures")).await;
        let keywords =
            nexus_auth::get_csv_setting(db, &format!("orchestrator.council_{name}_keywords")).await;
        // Un asse senza figure o senza keyword non puo' convocare nulla: si
        // scarta qui invece di portarselo dietro come riga muta.
        if figures.is_empty() || keywords.is_empty() {
            continue;
        }
        domain_axes.push(CouncilDomainAxis {
            name,
            keywords,
            figures,
        });
    }
    let max_figures = nexus_auth::get_setting(db, "orchestrator.council_max_figures")
        .await
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(6)
        .max(1);
    CouncilConfig {
        base_figures: base,
        domain_axes,
        max_figures,
    }
}

/// Selettore PURO (regola L, testabile senza DB) delle figure del consiglio per un
/// dato testo, gia' DIMENSIONATO al `target` del piano di orchestrazione.
///
/// Le figure "d'ambito" entrano per prime, e il taglio non le tocca: le ha
/// scelte un segnale del task, mentre le base sono un default cieco. Il
/// `target` decide QUANTE figure in tutto, non quali si possono perdere: le
/// base riempiono i posti che restano.
///
/// Prima il taglio stava in DUE punti — `truncate(max_figures)` qui e
/// `truncate(target)` nel chiamante — e mordeva sempre la CODA, cioe' proprio le
/// figure d'ambito. Col profilo `medium` (target 3) e 5 figure base, `sysadmin`
/// non e' mai stato convocato nemmeno su un task di deploy: la lente scelta dal
/// testo veniva scartata a favore delle prime tre voci di un CSV. Ogni asse
/// nuovo avrebbe ereditato lo stesso silenzio.
///
/// `declared_competencies` e' il giudizio SEMANTICO del classificatore
/// (`intent_classifier::AgenticIntent::competencies`, gia' validato contro il
/// roster figure): quando presente, GOVERNA la scelta d'ambito al posto delle
/// keyword — sono le competenze che il task richiede DAVVERO, non quelle che
/// hanno la fortuna di condividere una parola col testo. `Some(vec![])` e' un
/// giudizio valido ("nessuna lente d'ambito serve"), diverso da `None`
/// ("non dichiarabile": classifier caduto o vocabolario non iniettato), che fa
/// ripiegare sulle keyword d'ambito — l'unico caso in cui restano usate.
/// L'attivazione da keyword riusa il PUNTO UNICO del match a parola intera
/// [`crate::prompt_templates::touches_domain_keyword`] (regola L): a
/// sottostringa un vocabolario d'ambito non e' affidabile — `log` trova
/// `login`, `app` trova `approccio`.
/// `target`: `None` = nessun piano (si convoca fino a `max_figures`), `Some(0)`
/// = panel azzerato dal budget -> nessuna figura.
pub(crate) fn select_council_figures(
    user_text: &str,
    cfg: &CouncilConfig,
    target: Option<usize>,
    declared_competencies: Option<&[String]>,
) -> Vec<String> {
    let push = |f: &String, figures: &mut Vec<String>| {
        if !f.is_empty() && !figures.iter().any(|e| e == f) {
            figures.push(f.clone());
        }
    };
    let mut figures: Vec<String> = Vec::new();
    if let Some(declared) = declared_competencies {
        // Giudizio semantico gia' validato dal classificatore: le competenze
        // dichiarate SONO le figure d'ambito, niente match testuale.
        for f in declared {
            push(f, &mut figures);
        }
    } else {
        // Ripiego keyword: SOLO quando il classificatore non ha potuto
        // dichiarare (caduto, o vocabolario non iniettato nel prompt).
        for axis in &cfg.domain_axes {
            if crate::prompt_templates::touches_domain_keyword(user_text, &axis.keywords) {
                for f in &axis.figures {
                    push(f, &mut figures);
                }
            }
        }
    }
    // Posti totali. Il target puo' ALLARGARSI per ospitare le figure d'ambito
    // (sono obbligatorie), mai oltre il backstop assoluto `max_figures`.
    let posti = match target {
        Some(0) => return Vec::new(),
        Some(t) => t.max(figures.len()).min(cfg.max_figures),
        None => cfg.max_figures,
    };
    figures.truncate(posti);
    for f in &cfg.base_figures {
        if figures.len() >= posti {
            break;
        }
        push(f, &mut figures);
    }
    figures
}

pub(crate) async fn read_multi_provider_config(db: &sqlx::PgPool) -> Option<MultiProviderConfig> {
    let enabled = nexus_auth::get_bool_setting(db, "orchestrator.multi_provider_enabled")
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    if !enabled {
        return None;
    }
    let kind = nexus_auth::get_setting(db, "orchestrator.multi_provider_kind")
        .await?
        .trim()
        .to_string();
    let purpose = nexus_auth::get_setting(db, "orchestrator.multi_provider_purpose")
        .await?
        .trim()
        .to_string();
    if kind.is_empty() || purpose.is_empty() {
        return None;
    }
    let max_providers = nexus_auth::get_setting(db, "orchestrator.multi_provider_max_providers")
        .await
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(3)
        .max(1);
    let min_providers = nexus_auth::get_setting(db, "orchestrator.multi_provider_min_providers")
        .await
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(2)
        .max(1)
        .min(max_providers);
    Some(MultiProviderConfig {
        enabled,
        kind,
        purpose,
        max_providers,
        min_providers,
    })
}

// ── Dimensionamento orchestrazione (mig 0602) ──────────────────────────────
// Loader I/O del resolver PURO `orchestration_sizing` (regola L: la decisione
// vive nel modulo puro di nexus-agent-graph; qui SOLO la lettura di settings,
// definitions e listino, coi safe-default che coincidono coi seed della mig).

/// Backstop ASSOLUTI del dimensionamento: le STESSE chiavi storiche dei cap
/// (nessuna seconda fonte di verita'). Il resolver decide, questi limano.
pub(crate) async fn read_orchestration_backstops(
    db: &sqlx::PgPool,
) -> nexus_agent_graph::decisions::orchestration_sizing::OrchestrationBackstops {
    async fn usize_setting(db: &sqlx::PgPool, key: &str, default: usize) -> usize {
        nexus_auth::get_setting(db, key)
            .await
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(default)
    }
    let multi_provider_max =
        usize_setting(db, "orchestrator.multi_provider_max_providers", 3)
            .await
            .max(1);
    nexus_agent_graph::decisions::orchestration_sizing::OrchestrationBackstops {
        council_max: usize_setting(db, "orchestrator.council_max_figures", 6)
            .await
            .max(1),
        review_max: usize_setting(db, "orchestrator.review_panel_size", 2)
            .await
            .max(1),
        multi_provider_min: usize_setting(db, "orchestrator.multi_provider_min_providers", 2)
            .await
            .max(1)
            .min(multi_provider_max),
        multi_provider_max,
        debate_max: usize_setting(db, "orchestrator.debate_max_advocates", 4).await,
        fanout_max_parallel: fanout_max_parallel(db).await,
    }
}

/// Config del resolver di dimensionamento (chiavi mig 0602, regola G).
pub(crate) async fn read_orchestration_sizing_config(
    db: &sqlx::PgPool,
) -> nexus_agent_graph::decisions::orchestration_sizing::OrchestrationSizingConfig {
    use nexus_agent_graph::decisions::orchestration_sizing::parse_panel_priority;
    let enabled = nexus_auth::get_bool_setting(db, "orchestrator.sizing_enabled")
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    let budget_share_pct = nexus_auth::get_setting(db, "orchestrator.sizing.budget_share_pct")
        .await
        .and_then(|v| v.trim().parse::<u8>().ok())
        .unwrap_or(20)
        .min(100);
    let priority_csv = nexus_auth::get_setting(db, "orchestrator.sizing.panel_priority")
        .await
        .unwrap_or_default();
    nexus_agent_graph::decisions::orchestration_sizing::OrchestrationSizingConfig {
        enabled,
        budget_share_pct,
        panel_priority: parse_panel_priority(&priority_csv),
    }
}

/// Profilo di DOMANDA per-classe (`orchestrator.sizing_profile_<class>`, JSON).
/// Profilo assente o malformato -> `None`: il chiamante degrada al piano legacy
/// (fail-safe, mai numeri inventati).
pub(crate) async fn read_sizing_profile(
    db: &sqlx::PgPool,
    complexity: nexus_agent_graph::decisions::orchestration_sizing::TaskComplexity,
) -> Option<nexus_agent_graph::decisions::orchestration_sizing::PanelDemand> {
    let key = format!("orchestrator.sizing_profile_{}", complexity.as_str());
    let raw = nexus_auth::get_setting(db, &key).await?;
    let v: Value = serde_json::from_str(raw.trim()).ok()?;
    let field = |k: &str| v.get(k).and_then(Value::as_u64).map(|n| n as usize);
    Some(
        nexus_agent_graph::decisions::orchestration_sizing::PanelDemand {
            council_figures: field("council_figures")?,
            reviewers: field("reviewers")?,
            providers: field("providers")?,
            advocates: field("advocates")?,
        },
    )
}

/// Stima UNITARIA di un sub-run advisory per il vincolo di budget. Il modello
/// rappresentativo e' quello del purpose della PRIMA figura base del consiglio,
/// risolto VIA TIER (`resolve_purpose_model_db`, mai un nome modello) e prezzato
/// dal punto unico `nexus-pricing`. Se un anello manca (figura, purpose, tier,
/// listino unknown) il costo resta 0.0 = vincolo NON calcolabile: il resolver
/// non applica il cap di costo (regola M: nessun prezzo inventato).
pub(crate) async fn read_panel_unit_estimate(
    db: &sqlx::PgPool,
) -> nexus_agent_graph::decisions::orchestration_sizing::PanelUnitEstimate {
    let est_tokens = nexus_auth::get_setting(db, "orchestrator.sizing.est_subrun_tokens")
        .await
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(60_000);
    let duration_s = nexus_auth::get_setting(db, "orchestrator.sizing.est_subrun_duration_s")
        .await
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(240);
    let cost_usd = panel_unit_cost(db, est_tokens).await.unwrap_or(0.0);
    nexus_agent_graph::decisions::orchestration_sizing::PanelUnitEstimate {
        cost_usd,
        duration_s,
    }
}

/// Tempo RESIDUO (secondi) della deadline del run primario (fase 3, mig 0604).
/// PUNTO UNICO (regola L) usato dal clamp del timeout sub-run in prepare, dal
/// resolver di dimensionamento pre-run e dalla review post-run. Derivato DAL DB
/// (`agent_runs.created_at` del run ancorato + setting `agent.run_time_budget_s`):
/// nessun threading di Instant nei ctx, vale per OGNI percorso di dispatch.
/// `None` = deadline disattivata (setting 0/assente) o run non ancorato.
pub(crate) async fn run_time_remaining_s(
    meta_db: &sqlx::PgPool,
    run_pool: &sqlx::PgPool,
    anchor_run_id: Uuid,
) -> Option<i64> {
    let budget_s = nexus_auth::get_setting(meta_db, "agent.run_time_budget_s")
        .await
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|b| *b > 0)?;
    let started: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT created_at FROM agent_runs WHERE id = $1")
            .bind(anchor_run_id)
            .fetch_optional(run_pool)
            .await
            .ok()
            .flatten()?;
    let elapsed_s = (chrono::Utc::now() - started).num_seconds().max(0);
    Some(budget_s - elapsed_s)
}

/// Floor del timeout sub-run sotto deadline (`orchestrator.subagent_min_timeout_s`,
/// mig 0604): sotto questo residuo una figura NON parte (prepare_reject
/// strutturato), un timeout ridicolo produrrebbe solo spesa senza esito.
pub(crate) async fn subagent_min_timeout_s(db: &sqlx::PgPool) -> i64 {
    nexus_auth::get_setting(db, "orchestrator.subagent_min_timeout_s")
        .await
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(30)
}

/// Costo atteso di UN sub-run advisory dal listino (tier-only + nexus-pricing).
async fn panel_unit_cost(db: &sqlx::PgPool, est_tokens: i64) -> Option<f64> {
    let figures_csv = nexus_auth::get_setting(db, "orchestrator.council_figures").await?;
    let first_figure = figures_csv
        .split(',')
        .map(str::trim)
        .find(|s| !s.is_empty())?
        .to_string();
    let purpose: String = sqlx::query_scalar(
        "SELECT model_purpose FROM nexus_subagent_definitions \
          WHERE kind = $1 AND is_enabled = true",
    )
    .bind(&first_figure)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()?;
    let (provider, model) = crate::internal_routing::resolve_purpose_model_db(db, &purpose)
        .await
        .into_model(&purpose)
        .ok()?;
    let lookup = nexus_pricing::resolve_active_price(db, &provider, &model)
        .await
        .ok()?;
    let nexus_pricing::PriceLookup::Priced(price) = lookup else {
        // Unknown/NotInCatalog: prezzo ignoto NON e' prezzo zero (mig 0477).
        return None;
    };
    // Ripartizione DETERMINISTICA del budget token atteso: i sub-run advisory
    // sono read-heavy (contesto + tool_result >> risposta). Derivazione di
    // calcolo, non config di business (come le soglie di orchestration_reason).
    let prompt_tokens = est_tokens * 4 / 5;
    let completion_tokens = est_tokens - prompt_tokens;
    let (_, _, total) = nexus_pricing::calculate_cost(&price, prompt_tokens, completion_tokens);
    Some(total)
}

/// Segnale strutturato di rifiuto in fase PREPARE (regola M): il coordinatore
/// legge `error_code`, non il testo di `error`.
fn prepare_reject(error_code: &'static str, message: impl Into<String>) -> Value {
    json!({
        "error": message.into(),
        "error_code": error_code,
        "status": "prepare_failed",
        "outcome": terminal_verdict("failed", error_code),
    })
}

/// Classifica l'esito di UNA figura del consiglio dal tool_result strutturato.
fn classify_council_figure_result(kind: &str, result: &Value) -> FigureAdvisoryReport {
    let subagent_run_id = result
        .get(K_SUB_RUN_ID)
        .and_then(Value::as_str)
        .map(str::to_owned);
    // Provenienza EFFETTIVA della figura (stampata dai finalize_* nel result):
    // assente per le figure respinte a monte (guard depth/whitelist) che non hanno
    // mai risolto un modello.
    let provider = result.get(K_PROVIDER).and_then(Value::as_str).map(str::to_owned);
    let model = result.get(K_MODEL).and_then(Value::as_str).map(str::to_owned);
    if let Some(code) = result.get("error_code").and_then(Value::as_str) {
        let message = result
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or(code)
            .to_string();
        return FigureAdvisoryReport {
            kind: kind.to_string(),
            status: FigureAdvisoryStatus::PrepareFailed,
            detail_code: code.to_string(),
            detail_message: message,
            advisory_verdict: None,
            advisory: None,
            provider: provider.clone(),
            model: model.clone(),
            subagent_run_id,
        };
    }
    let lifecycle = result.get("status").and_then(Value::as_str).unwrap_or("");
    if lifecycle == "timeout" {
        return FigureAdvisoryReport {
            kind: kind.to_string(),
            status: FigureAdvisoryStatus::RunTimeout,
            detail_code: "run_timeout".to_string(),
            detail_message: result
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("Sub-agent in timeout")
                .to_string(),
            advisory_verdict: None,
            advisory: None,
            provider: provider.clone(),
            model: model.clone(),
            subagent_run_id,
        };
    }
    if lifecycle == "failed" || lifecycle == "prepare_failed" {
        let outcome = result.get("outcome").cloned().unwrap_or(Value::Null);
        let code = outcome
            .get("error_class")
            .or_else(|| outcome.get("verdict"))
            .and_then(Value::as_str)
            .unwrap_or("run_failed")
            .to_string();
        let message = result
            .get("error")
            .or_else(|| result.get(K_SUMMARY))
            .and_then(Value::as_str)
            .unwrap_or("Sub-run fallito")
            .to_string();
        return FigureAdvisoryReport {
            kind: kind.to_string(),
            status: FigureAdvisoryStatus::RunFailed,
            detail_code: code,
            detail_message: message,
            advisory_verdict: None,
            advisory: None,
            provider: provider.clone(),
            model: model.clone(),
            subagent_run_id,
        };
    }
    let outcome = result.get("outcome").cloned().unwrap_or(Value::Null);
    if outcome.get("success").and_then(Value::as_bool) != Some(true) {
        let code = outcome
            .get("error_class")
            .or_else(|| outcome.get("verdict"))
            .and_then(Value::as_str)
            .unwrap_or("run_failed")
            .to_string();
        return FigureAdvisoryReport {
            kind: kind.to_string(),
            status: FigureAdvisoryStatus::RunFailed,
            detail_code: code,
            detail_message: "Sub-run terminato senza esito positivo".to_string(),
            advisory_verdict: None,
            advisory: None,
            provider: provider.clone(),
            model: model.clone(),
            subagent_run_id,
        };
    }
    let advisory = match outcome.get("advisory") {
        Some(a) if !a.is_null() => a,
        _ => {
            return FigureAdvisoryReport {
                kind: kind.to_string(),
                status: FigureAdvisoryStatus::CompletedNoAdvisory,
                detail_code: "no_advisory".to_string(),
                detail_message: "Sub-run completato senza chiamare advisory_verdict".to_string(),
                advisory_verdict: None,
                advisory: None,
                provider: provider.clone(),
                model: model.clone(),
                subagent_run_id,
            };
        }
    };
    let verdict = advisory.get("verdict").and_then(Value::as_str);
    let valid_verdict = verdict.is_some_and(|v| {
        matches!(v, "proceed" | "proceed_with_changes" | "block")
    });
    if !valid_verdict {
        return FigureAdvisoryReport {
            kind: kind.to_string(),
            status: FigureAdvisoryStatus::InvalidAdvisory,
            detail_code: "invalid_advisory".to_string(),
            detail_message: "Parere advisory presente ma verdetto non valido".to_string(),
            advisory_verdict: verdict.map(str::to_owned),
            advisory: Some(advisory.clone()),
            provider: provider.clone(),
            model: model.clone(),
            subagent_run_id,
        };
    }
    FigureAdvisoryReport {
        kind: kind.to_string(),
        status: FigureAdvisoryStatus::AdvisoryOk,
        detail_code: "advisory_ok".to_string(),
        detail_message: "Parere advisory valido".to_string(),
        advisory_verdict: verdict.map(str::to_owned),
        advisory: Some(advisory.clone()),
        provider: provider.clone(),
        model: model.clone(),
        subagent_run_id,
    }
}

/// Esito della convocazione parallela: report per-figura + sintesi opzionale.
#[derive(Debug, Clone)]
pub(crate) struct CouncilConvokeResult {
    pub figure_reports: Vec<FigureAdvisoryReport>,
    pub synthesis: Option<AdvisorySynthesis>,
}

/// Setting DB (regola G) del tetto di sub-run REALMENTE concorrenti di un
/// fan-out (consiglio, panel di review, panel multi-provider). Mig 0596.
const KEY_FANOUT_MAX_PARALLEL: &str = "orchestrator.subagent_fanout_max_parallel";
/// Default se la riga manca: nessun tetto artificiale sotto il fan-out nominale
/// del consiglio (6 figure). Non e' un "numero magico" di comportamento: e' il
/// valore che conserva la semantica storica (tutte insieme) quando il DB tace.
const DEFAULT_FANOUT_MAX_PARALLEL: usize = 6;

/// Tetto di concorrenza del fan-out dal DB (regola G), clampato a >=1.
async fn fanout_max_parallel(db: &sqlx::PgPool) -> usize {
    crate::settings::get_setting(db, KEY_FANOUT_MAX_PARALLEL)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_FANOUT_MAX_PARALLEL)
        .max(1)
}

/// Setting DB (regola G) del tetto di sub-run concorrenti dell'INTERO processo
/// (mig 0603). Serve da quando i panel girano in parallelo (`tokio::join!` di
/// consiglio + multi-provider): ogni fan-out ha il suo semaforo locale, quindi
/// K panel insieme = K x permits senza un tetto globale — questo lo mette.
const KEY_FANOUT_PROCESS_MAX_PARALLEL: &str = "orchestrator.fanout_process_max_parallel";
/// Default se la riga manca: 2 panel pieni (2 x 6) come i due panel pre-run.
const DEFAULT_FANOUT_PROCESS_MAX_PARALLEL: usize = 12;

/// Semaforo di PROCESSO dei fan-out top-level. Dimensionato UNA volta al primo
/// uso (riavvio del servizio per applicare una modifica del setting: un semaforo
/// non e' ridimensionabile a caldo senza reintrodurre stati transitori).
static PROCESS_FANOUT_SEM: tokio::sync::OnceCell<std::sync::Arc<tokio::sync::Semaphore>> =
    tokio::sync::OnceCell::const_new();

async fn process_fanout_semaphore(db: &sqlx::PgPool) -> std::sync::Arc<tokio::sync::Semaphore> {
    PROCESS_FANOUT_SEM
        .get_or_init(|| async {
            let permits = crate::settings::get_setting(db, KEY_FANOUT_PROCESS_MAX_PARALLEL)
                .await
                .ok()
                .flatten()
                .and_then(|v| v.trim().parse::<usize>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(DEFAULT_FANOUT_PROCESS_MAX_PARALLEL)
                .max(1);
            std::sync::Arc::new(tokio::sync::Semaphore::new(permits))
        })
        .await
        .clone()
}

/// Ambito del fan-out rispetto al governo della concorrenza, DICHIARATO dal
/// call site (regola M: segnale esplicito, non inferito dal ctx — il ctx del
/// consiglio ha `parent_run_id` valorizzato pur essendo del coordinatore).
///
/// - `TopLevel`: convocazione del COORDINATORE (consiglio, review panel,
///   multi-provider, debate). Acquisisce ANCHE il semaforo di processo.
/// - `Nested`: fan-out dentro un sub-run. SOLO semaforo locale: un membro
///   padre che tenesse un permesso di processo mentre il figlio ne attende un
///   altro dallo stesso semaforo creerebbe hold-and-wait (deadlock di classe);
///   la concorrenza dei nested resta bounded dal semaforo locale + depth guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FanoutScope {
    TopLevel,
    Nested,
}

/// PUNTO UNICO (regola L) del FAN-OUT di sub-run: esegue `n` sub-run
/// CONCORRENTI, ognuno nel PROPRIO task tokio, con un tetto di parallelismo.
///
/// Perche' esiste (incidente consiglio 2026-07-15, difetto D3 PROVATO):
/// i tre fan-out (consiglio, review panel, multi-provider) usavano
/// `FuturesUnordered` e pushavano le future DENTRO il task chiamante — nessun
/// `tokio::spawn`. Le N figure erano quindi concorrenza COOPERATIVA su UN SOLO
/// task, e con loro i loro `tokio::time::timeout`: un `Timeout` ritorna
/// `Elapsed` solo quando viene POLLATO, quindi bastava un membro che bloccasse
/// il thread dentro il proprio `poll()` per congelare TUTTI gli altri e i loro
/// timer. Firma misurata sul campo: 4 sub-run con `t0` DIVERSI (10:30:28 e
/// 10:32:10) e `timeout_s=240` uguale, morti tutti allo STESSO millisecondo
/// (10:37:00.157) dopo 408s — impossibile per 4 timer indipendenti.
/// Con `spawn` ogni sub-run ha il proprio task: un panic resta confinato e i
/// timer non dipendono piu' dal poll di un membro vicino. Da qui NON segue la
/// garanzia che il timer scatti al suo valore quando un altro membro blocca il
/// thread: non scatta, e il test `spawn_non_protegge_dal_blocking_sincrono_
/// limite_dichiarato` lo dimostra. `spawn` toglie l'accoppiamento cooperativo
/// dentro un task, non toglie il blocking dal path.
///
/// Semaforo (mai `chunks()` + `join_all`): il permesso si libera appena UN
/// sub-run finisce, quindi il successivo parte subito. Una barriera a ondate
/// riprodurrebbe proprio la firma "tutti insieme" che stiamo eliminando.
///
/// `JoinError` (panic dentro un sub-run) e' catturato e tradotto in `Value`
/// d'errore: dentro il vecchio `FuturesUnordered` un panic avrebbe abbattuto
/// l'INTERO fan-out in silenzio.
async fn spawn_fanout<F, Fut>(db: &sqlx::PgPool, n: usize, scope: FanoutScope, make: F) -> Vec<Value>
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = Value> + Send + 'static,
{
    let permits = fanout_max_parallel(db).await;
    let local = std::sync::Arc::new(tokio::sync::Semaphore::new(permits));
    let process = match scope {
        FanoutScope::TopLevel => Some(process_fanout_semaphore(db).await),
        FanoutScope::Nested => None,
    };
    spawn_fanout_with(local, process, n, make).await
}

/// Corpo del fan-out con semafori ESPLICITI (testabile senza stato globale).
/// Ordine di acquisizione FISSO e uniforme: LOCALE -> PROCESSO. Il grafo delle
/// attese resta aciclico: chi tiene un permesso di processo non attende mai un
/// semaforo locale altrui (ogni fan-out ha il proprio), quindi niente cicli.
async fn spawn_fanout_with<F, Fut>(
    local: std::sync::Arc<tokio::sync::Semaphore>,
    process: Option<std::sync::Arc<tokio::sync::Semaphore>>,
    n: usize,
    make: F,
) -> Vec<Value>
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = Value> + Send + 'static,
{
    let mut handles = Vec::with_capacity(n);
    for i in 0..n {
        let fut = make(i);
        let local = local.clone();
        let process = process.clone();
        handles.push(tokio::spawn(async move {
            // I permessi vivono quanto il sub-run; si liberano al drop (fine
            // del task), non a fine "ondata".
            let _local_permit = match local.acquire_owned().await {
                Ok(p) => p,
                // Semaforo chiuso: non succede (mai `close()`), ma non si
                // inventa un esito -> errore strutturato.
                Err(e) => {
                    return json!({
                        "error": format!("fanout: semaforo chiuso: {e}"),
                        "error_code": "fanout_semaphore_closed",
                        "status": "prepare_failed",
                        "outcome": terminal_verdict("failed", "fanout_semaphore_closed"),
                    })
                }
            };
            let _process_permit = match process {
                Some(sem) => match sem.acquire_owned().await {
                    Ok(p) => Some(p),
                    Err(e) => {
                        return json!({
                            "error": format!("fanout: semaforo di processo chiuso: {e}"),
                            "error_code": "fanout_semaphore_closed",
                            "status": "prepare_failed",
                            "outcome": terminal_verdict("failed", "fanout_semaphore_closed"),
                        })
                    }
                },
                None => None,
            };
            fut.await
        }));
    }
    let mut out = Vec::with_capacity(n);
    for h in handles {
        match h.await {
            Ok(v) => out.push(v),
            Err(e) => {
                // Panic o cancellazione del sub-run: esito STRUTTURATO (regola
                // M), il fan-out prosegue con gli altri.
                tracing::error!(error = %e, "fanout: sub-run terminato da panic/cancellazione");
                out.push(json!({
                    "error": format!("sub-run interrotto: {e}"),
                    "error_code": "subrun_panicked",
                    "status": "failed",
                    "outcome": terminal_verdict("failed", "subrun_panicked"),
                }));
            }
        }
    }
    out
}

/// Convoca le figure `kinds` in PARALLELO (sub-run read-only, sincroni) e compone la
/// SINTESI del loro parere col punto unico PURO `compose_advisory_synthesis` (regola
/// L/M: legge i segnali strutturati `outcome.advisory`, mai la prosa di `summary`).
/// `None` se la lista e' vuota o nessuna figura ha prodotto un parere valido.
/// Best-effort: i Guard di `prepare_subagent_run` (whitelist/depth/cost) restano
/// attivi; una figura in errore/timeout semplicemente non vota.
pub(crate) async fn convene_council(
    ctx: &AgentToolContext,
    task: &str,
    kinds: &[&str],
    policy: &AdvisoryPolicy,
    progress_tx: Option<tokio::sync::mpsc::Sender<FigureAdvisoryReport>>,
) -> CouncilConvokeResult {
    if kinds.is_empty() {
        return CouncilConvokeResult {
            figure_reports: Vec::new(),
            synthesis: None,
        };
    }
    // Le figure hanno gia' l'istruzione advisory_verdict nel loro prompt (mig 0548);
    // qui ribadiamo il formato atteso come promemoria operativo.
    let expected = "Concludi la tua analisi chiamando il tool advisory_verdict \
                    (verdict, requirements, risks[severity+description], recommendations).";
    // Fan-out sul PUNTO UNICO (regola L): ogni figura nel proprio task tokio,
    // col proprio timer (difetto D3). Il progresso UI viene emesso appena il
    // singolo sub-run rientra, senza attendere gli altri.
    let owned_kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
    // Provider DISTINTI decisi PRIMA del fan-out (sequenziale, quindi
    // accumulabile): in parallelo ogni figura sceglieva da sola e il piu'
    // economico eleggibile era lo stesso per tutte -- due figure sullo stesso
    // provider+modello non sono due pareri. `None` = percorso storico.
    let assignments = council_assignments(ctx, &owned_kinds).await;
    let results = {
        let kinds_for_make = owned_kinds.clone();
        spawn_fanout(&ctx.core.db, kinds_for_make.len(), FanoutScope::TopLevel, |i| {
            let ctx = ctx.clone();
            let task = task.to_string();
            let expected = expected.to_string();
            let kind = kinds_for_make[i].clone();
            let pin = assignments[i].clone();
            async move { run_council_figure(&ctx, &kind, &task, &expected, pin).await }
        })
        .await
    };
    let pairs = classify_and_stream_reports(&owned_kinds, results, progress_tx.as_ref()).await;
    let figure_reports: Vec<FigureAdvisoryReport> = pairs.iter().map(|(r, _)| r.clone()).collect();
    let raw_results: Vec<Value> = pairs.into_iter().map(|(_, v)| v).collect();
    // Roster esplicito = figure CONVOCATE (regola M): una figura in timeout/errore
    // e' un'astensione che pesa nel quorum, non una riga che sparisce dal conteggio.
    let synthesis = compose_council_synthesis(&raw_results, policy, kinds.len());
    CouncilConvokeResult {
        figure_reports,
        synthesis,
    }
}

/// Convoca un PANEL di revisori (kind=review) in PARALLELO e compone il verdetto
/// AVVERSARIO col punto unico `compose_panel_verdict` (Fase C ultracode, regola
/// L/M: legge `outcome.review`, mai la prosa). Usato dal RINFORZO PROGRAMMATICO
/// (post-step in agent_run): rende la review deterministica invece che affidata
/// alla sola direttiva LLM `<revisione_finale>`. `None` se nessun revisore produce
/// un verdetto valido. Best-effort: i guard di `prepare_subagent_run` restano
/// attivi; il routing esclude il provider del padre (indipendenza avversaria,
/// review). `reviewers` e' clampato a >=1.
/// Purpose da cui il panel di review pesca i provider dei revisori. E' una
/// CHIAVE di configurazione, non un modello: provider e modello concreti stanno
/// in `nexus_purpose_model` ed e' li' che si cambiano (regola G).
const REVIEW_PANEL_PURPOSE: &str = "reviewer";

/// Cosa rende due revisori "diversi" per questo panel. Costante NOMINATA, non
/// un argomento scritto nel punto di chiamata: cosi' il test la legge da qui
/// invece di ricopiarla, e mutarla in `PerProvider` — il difetto del 26/07 —
/// fa rosseggiare il test invece di lasciarlo verde su un criterio suo
/// (regola O).
const REVIEW_PANEL_DIVERSITY: crate::internal_routing::CandidateDiversity =
    crate::internal_routing::CandidateDiversity::PerProviderAndModel;

/// Lancia gli `n` revisori, ciascuno pinnato su un GIUDICE distinto.
///
/// Senza pin il routing instrada tutti gli N revisori allo stesso
/// provider/modello: non e' un quorum, e' UN solo giudizio contato N volte, e
/// quando quel modello sbaglia il run viene rimandato in correzione fino al cap
/// dei tentativi con l'apparenza di una verifica plurale. I candidati arrivano
/// dal purpose, come nel panel multi-provider (regola L).
/// Giudici distinti su cui pinnare i revisori. Vuoto se il purpose non e'
/// risolvibile: il panel prosegue senza pin, con un WARN che nomina il rischio.
///
/// Chiede `PerProviderAndModel`, non `PerProvider`: quando un solo provider e'
/// sano — il 2026-07-26 openai e anthropic erano in cooldown billing — la dedup
/// per provider lasciava UN candidato e il panel a due si riduceva a un giudice
/// solo, benche' quel provider offrisse dieci modelli qualificati nello stesso
/// tier. Due modelli diversi non sono due provider, ma sono due pareri; uno
/// solo, riconvocato a ogni ciclo, non lo e' mai.
async fn candidati_revisori(
    ctx: &AgentToolContext,
    n: usize,
) -> Vec<crate::internal_routing::PurposeProviderCandidate> {
    crate::internal_routing::resolve_purpose_provider_candidates_db_by(
        &ctx.core.db,
        REVIEW_PANEL_PURPOSE,
        n,
        1,
        REVIEW_PANEL_DIVERSITY,
    )
    .await
    .unwrap_or_else(|resolution| {
        tracing::warn!(
            purpose = %REVIEW_PANEL_PURPOSE,
            resolution = ?resolution,
            "review panel: purpose non risolvibile, revisori senza pin (giudizio unico replicato)"
        );
        Vec::new()
    })
}

/// Chi convoca davvero il panel: al piu' `richiesti` GIUDICI distinti, presi
/// nell'ordine di preferenza dei candidati.
///
/// Mai due revisori sulla stessa coppia (provider, model): non sono un quorum,
/// sono un giudizio unico contato due volte, e il panel li conteggerebbe come
/// due pareri indipendenti. Meglio un panel piu' piccolo e onesto che uno grande
/// e finto.
///
/// La garanzia sta QUI, dove il panel si compone, e delega la nozione di "stesso
/// giudice" al punto unico `giudici_distinti` (regola L). Non si assume che la
/// selezione abbia gia' deduplicato come serve a un panel (regola O: era proprio
/// l'assunzione sbagliata — si contava `candidates.len()` credendolo il numero
/// dei giudici distinti, mentre la fonte deduplicava per solo provider).
///
/// Lista vuota = purpose non risolvibile: nessun pin da assegnare, e il numero
/// dei revisori resta quello richiesto (li instrada il routing).
fn panel_revisori(
    candidati: &[crate::internal_routing::PurposeProviderCandidate],
    richiesti: usize,
) -> Vec<crate::internal_routing::PurposeProviderCandidate> {
    crate::internal_routing::giudici_distinti(candidati, richiesti)
}

/// Chi votera', nell'ordine degli slot. E' l'unico punto che lo sa: gli outcome
/// dei revisori non portano la propria provenienza, quindi senza questa
/// dichiarazione la pluralita' del panel resta invisibile a valle.
///
/// Provider e modello restano SEPARATI fino al consumatore (regola L): il
/// modello puo' contenere `/` e ricomporli qui costringerebbe il frontend a
/// indovinare dove tagliare. `auto` quando nessun candidato e' stato risolto
/// (revisore senza pin: lo instrada il routing, e a priori non sappiamo dove).
fn riferimenti_revisori(
    candidates: &[crate::internal_routing::PurposeProviderCandidate],
    n: usize,
) -> Vec<nexus_agent_graph::decisions::ReviewerRef> {
    (0..n)
        .map(|i| match candidates.get(i) {
            Some(c) => nexus_agent_graph::decisions::ReviewerRef {
                provider: c.provider.clone(),
                model: c.model.clone(),
            },
            None => nexus_agent_graph::decisions::ReviewerRef {
                provider: "auto".to_string(),
                model: String::new(),
            },
        })
        .collect()
}

/// Dichiara il taglio del panel: quanti revisori si volevano, quanti ne restano
/// e soprattutto CHI sono. Il taglio e' dichiarato, non silenzioso.
///
/// I nomi non sono decorazione: `convocati=1` da solo non distingue un catalog
/// povero da un cooldown del giorno, e il 26/07 quella riga di log e' comparsa
/// sei volte senza far sospettare che dietro ci fosse sempre lo stesso giudice.
fn dichiara_taglio_panel(
    panel: &[crate::internal_routing::PurposeProviderCandidate],
    richiesti: usize,
    convocati: usize,
) {
    if convocati >= richiesti {
        return;
    }
    let giudici: Vec<String> = panel
        .iter()
        .map(|c| format!("{}/{}", c.provider, c.model))
        .collect();
    tracing::warn!(
        richiesti,
        convocati,
        giudici = ?giudici,
        purpose = %REVIEW_PANEL_PURPOSE,
        "review panel ridotto: giudici distinti insufficienti, meglio meno \
         revisori che due voti dallo stesso provider+modello contati come \
         indipendenti"
    );
}

/// Kind del revisore con la lente di interfaccia. E' una CHIAVE di definizione
/// nel DB (`nexus_subagent_definitions`), non un modello: prompt, tool e
/// purpose si cambiano li' (regola G).
const UI_REVIEWER_KIND: &str = "ui_reviewer";

/// Questo kind GIUDICA il lavoro di qualcun altro? PUNTO UNICO (regola L) della
/// domanda, interrogato da tutti e tre i luoghi in cui vale «giudice != worker»:
/// la selezione del modello, il veto che segue il sub-run nel ripiego, e la
/// costruzione del suo input.
///
/// Esiste perche' quella regola era scritta come `kind == "review"` in tre
/// punti. Finche' il panel aveva un solo tipo di giudice la ripetizione non si
/// vedeva; il primo giudice con un kind diverso — il revisore di interfaccia —
/// sarebbe nato SENZA il vincolo, cioe' libero di girare sul fornitore che ha
/// appena scritto il codice che deve giudicare. Non un difetto nuovo: lo stesso
/// del 26/07 (dieci revisori sul fornitore del padre), raggiunto per una terza
/// strada.
fn e_un_giudice(kind: &str) -> bool {
    kind == "review" || kind == UI_REVIEWER_KIND
}

/// I kind dei revisori da convocare, uno per posto disponibile.
///
/// La lente di interfaccia, quando serve, sta in TESTA: se i giudici distinti
/// non bastano il panel si riduce dalla coda, e una lente accesa da un fatto
/// del run (i file toccati) non deve essere la prima a cadere. Non si SOMMA ai
/// revisori richiesti — prende il posto di un generico — cosi' accendere la
/// lente non allarga il costo del panel.
fn kinds_dei_revisori(richiesti: usize, lente_ui: bool) -> Vec<String> {
    let n = richiesti.max(1);
    let mut kinds: Vec<String> = Vec::with_capacity(n);
    if lente_ui {
        kinds.push(UI_REVIEWER_KIND.to_string());
    }
    while kinds.len() < n {
        kinds.push("review".to_string());
    }
    kinds.truncate(n);
    kinds
}

async fn spawn_reviewers(
    ctx: &AgentToolContext,
    kinds: &[String],
    task: &str,
    expected: &str,
    assegnati: &mut Vec<nexus_agent_graph::decisions::ReviewerRef>,
) -> Vec<Value> {
    let richiesti = kinds.len();
    let candidates = candidati_revisori(ctx, richiesti).await;
    // Mai due revisori sullo stesso (provider, model): si riduce il panel invece
    // di duplicare.
    let panel = panel_revisori(&candidates, richiesti);
    let n = if panel.is_empty() {
        richiesti
    } else {
        panel.len()
    };
    dichiara_taglio_panel(&panel, richiesti, n);
    assegnati.extend(riferimenti_revisori(&panel, n));
    let kinds: Vec<String> = kinds.iter().take(n).cloned().collect();

    // Stesso punto unico di fan-out del consiglio (regola L, difetto D3).
    spawn_fanout(&ctx.core.db, n, FanoutScope::TopLevel, move |i| {
        let ctx = ctx.clone();
        let task = task.to_string();
        let expected = expected.to_string();
        // Lo stesso `panel` che ha prodotto i riferimenti dichiarati a valle:
        // chi vota e chi risulta aver votato non possono divergere.
        let pin = panel.get(i).map(|c| (c.provider.clone(), c.model.clone()));
        let kind = kinds[i].clone();
        async move {
            match pin {
                Some((provider, model)) => {
                    run_single_subagent_with_model_pin(
                        &ctx, &kind, &task, "", &expected, &provider, &model,
                    )
                    .await
                }
                None => {
                    run_single_subagent(&ctx, &kind, &task, "", &expected, None, false, &[]).await
                }
            }
        }
    })
    .await
}

pub(crate) async fn convene_review_panel(
    ctx: &AgentToolContext,
    task: &str,
    reviewers: usize,
    policy: &nexus_agent_graph::decisions::QuorumPolicy,
    lente_ui: bool,
) -> Option<nexus_agent_graph::decisions::PanelOutcome> {
    let expected = "Rivedi SOLO le modifiche indicate e chiudi chiamando review_verdict \
                    (verdict pass|fail|needs_changes; findings con file, severity ed evidenza \
                    concreta). Un fail richiede almeno un finding grave con evidenza.";
    let mut assegnati: Vec<nexus_agent_graph::decisions::ReviewerRef> = Vec::new();
    let kinds = kinds_dei_revisori(reviewers, lente_ui);
    let results = spawn_reviewers(ctx, &kinds, task, expected, &mut assegnati).await;
    let outcomes: Vec<Value> = results
        .into_iter()
        .map(|r| r.get("outcome").cloned().unwrap_or(Value::Null))
        .collect();
    nexus_agent_graph::decisions::compose_panel_verdict(&outcomes, policy)
        .map(|p| p.con_reviewers(assegnati))
}

/// Convoca lo stesso analista read-only su provider diversi, scelti dal catalog
/// tramite purpose tier-aware. Nessuna chiamata diretta ai provider: sono sub-run
/// nativi con provider/model pin derivati dal DB e passati al grafo.
pub(crate) async fn convene_multi_provider_panel(
    ctx: &AgentToolContext,
    task: &str,
    cfg: &MultiProviderConfig,
    policy: &AdvisoryPolicy,
) -> Option<MultiProviderPanelOutcome> {
    if !cfg.enabled {
        return None;
    }
    // Tetto e soglia arrivano ENTRAMBI alla selezione: `max_providers` e' quanti
    // se ne vorrebbero, `min_providers` sotto quanti il panel non ha senso. Con
    // la sola prima la tier-chain usciva al primo tier non vuoto e il quorum
    // veniva giudicato su un pool mai cercato davvero.
    let candidates = match crate::internal_routing::resolve_purpose_provider_candidates_db(
        &ctx.core.db,
        &cfg.purpose,
        cfg.max_providers,
        cfg.min_providers,
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                purpose = %cfg.purpose,
                resolution = ?e,
                "multi-provider panel: purpose non risolvibile"
            );
            return Some(MultiProviderPanelOutcome::Degraded {
                reason: MultiProviderDegradeReason::PurposeUnavailable,
                got: 0,
                min: cfg.min_providers,
            });
        }
    };
    if candidates.len() < cfg.min_providers {
        tracing::warn!(
            purpose = %cfg.purpose,
            got = candidates.len(),
            min = cfg.min_providers,
            "multi-provider panel: provider distinti insufficienti"
        );
        return Some(MultiProviderPanelOutcome::Degraded {
            reason: MultiProviderDegradeReason::InsufficientProviderDiversity,
            got: candidates.len(),
            min: cfg.min_providers,
        });
    }
    let expected = "Analizza la richiesta dalla prospettiva del tuo provider/modello \
                    e chiudi chiamando advisory_verdict (verdict, requirements, \
                    risks[severity+description], recommendations).";
    // Stesso punto unico di fan-out (regola L, difetto D3): un task per
    // analista. Prima era `join_all` — stessa trappola del FuturesUnordered:
    // tutte le future in UN task, quindi tutti i timer ostaggio del membro
    // piu' sfortunato.
    let cands: Vec<PanelProviderEntry> = candidates
        .iter()
        .map(|c| PanelProviderEntry {
            provider: c.provider.clone(),
            model: c.model.clone(),
        })
        .collect();
    let results = {
        let cands_for_make = cands.clone();
        let kind = cfg.kind.clone();
        spawn_fanout(&ctx.core.db, cands_for_make.len(), FanoutScope::TopLevel, |i| {
            let ctx = ctx.clone();
            let task = task.to_string();
            let expected = expected.to_string();
            let kind = kind.clone();
            let c = cands_for_make[i].clone();
            let context = format!(
                "Provider assegnato a questa analisi: {}. Modello assegnato: {}. \
                 Confronta la richiesta dalla prospettiva dei trade-off e dei failure-mode \
                 tipici del provider/modello assegnato, senza assumere che gli altri \
                 provider arrivino alla stessa conclusione.",
                c.provider, c.model
            );
            async move {
                run_single_subagent_with_model_pin(
                    &ctx,
                    &kind,
                    &task,
                    &context,
                    &expected,
                    &c.provider,
                    &c.model,
                )
                .await
            }
        })
        .await
    };
    let provider_count = candidates.len();
    let panel_providers: Vec<PanelProviderEntry> = candidates
        .iter()
        .map(|c| PanelProviderEntry {
            provider: c.provider.clone(),
            model: c.model.clone(),
        })
        .collect();
    // Parere individuale di ogni provider (provenienza + advisory), riusando la
    // classificazione delle figure (regola L): la UI puo' mostrare la differenza.
    let provider_reports: Vec<FigureAdvisoryReport> = results
        .iter()
        .map(|r| classify_council_figure_result(&cfg.kind, r))
        .collect();
    compose_multi_provider_synthesis(&results, policy, provider_count).map(|synthesis| {
        MultiProviderPanelOutcome::Active {
            synthesis: Box::new(synthesis),
            provider_count,
            panel_providers,
            provider_reports,
        }
    })
}

/// Parte PURA (regola L, testabile senza DB) di [`convene_council`]: estrae i blocchi
/// `outcome` dai tool_result delle figure (regola M) e delega al punto unico PURO
/// `compose_advisory_synthesis`. `convened` = figure CONVOCATE (denominatore del
/// quorum). Separata cosi' il mapping result->outcome->synthesis e' coperto da unit
/// test con result mock.
fn compose_council_synthesis(
    results: &[Value],
    policy: &AdvisoryPolicy,
    convened: usize,
) -> Option<AdvisorySynthesis> {
    let outcomes: Vec<Value> = results
        .iter()
        .map(|r| r.get("outcome").cloned().unwrap_or(Value::Null))
        .collect();
    nexus_agent_graph::decisions::compose_advisory_synthesis(
        outcomes.as_slice(),
        policy,
        AdvisoryRoster::Convened(convened),
    )
}

fn compose_multi_provider_synthesis(
    results: &[Value],
    policy: &AdvisoryPolicy,
    convened: usize,
) -> Option<AdvisorySynthesis> {
    compose_council_synthesis(results, policy, convened)
}

/// Config del DIBATTITO a tesi contrapposte (mig 0605, regola G). `None` se il
/// dibattito e' spento o il kind non e' configurato: il coordinatore non convoca
/// (fail-closed, come il multi-provider).
pub(crate) struct DebateConfig {
    pub kind: String,
    pub max_advocates: usize,
}

pub(crate) async fn read_debate_config(db: &sqlx::PgPool) -> Option<DebateConfig> {
    let enabled = nexus_auth::get_bool_setting(db, "orchestrator.debate_enabled")
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    if !enabled {
        return None;
    }
    let kind = nexus_auth::get_setting(db, "orchestrator.debate_advocate_kind")
        .await?
        .trim()
        .to_string();
    if kind.is_empty() {
        return None;
    }
    let max_advocates = nexus_auth::get_setting(db, "orchestrator.debate_max_advocates")
        .await
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(4);
    Some(DebateConfig {
        kind,
        max_advocates,
    })
}

/// Esito della convocazione del dibattito (segnali strutturati per il meta-step).
pub(crate) struct DebatePanelOutcome {
    pub topic: String,
    pub assignments: Vec<nexus_agent_graph::decisions::debate_panel::DebateAssignment>,
    pub synthesis: nexus_agent_graph::decisions::debate_panel::DebateSynthesis,
}

/// Task di UN avvocato: la posizione assegnata e' dichiarata nella PRIMA riga in
/// forma canonica (`POSIZIONE ASSEGNATA: <testo>`), perche' e' la stringa che
/// l'avvocato deve ripetere alla lettera in `assigned_position` — la chiave con
/// cui `compose_debate_synthesis` attribuisce il voto. Funzione PURA: testabile
/// senza DB (regola L: unica sede della forma del task avvocato).
pub(crate) fn build_advocate_task(
    topic: &str,
    assignment: &nexus_agent_graph::decisions::debate_panel::DebateAssignment,
    user_task: &str,
) -> String {
    let opposing = assignment
        .opposing_positions
        .iter()
        .map(|o| format!("- {o}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "POSIZIONE ASSEGNATA: {assigned}\n\n\
         Sei un avvocato in un dibattito a tesi contrapposte su una decisione \
         architetturale del progetto.\n\n\
         DECISIONE IN DISCUSSIONE: {topic}\n\n\
         POSIZIONI AVVERSE (difese da altri avvocati, in parallelo a te):\n{opposing}\n\n\
         RICHIESTA ORIGINALE DELL'UTENTE (contesto):\n{user_task}\n\n\
         Il tuo compito: studia il codice del progetto e costruisci il caso PIU' \
         FORTE possibile per la tua posizione assegnata, con evidenza concreta \
         (file:riga). Attacca i punti deboli delle posizioni avverse con prove, \
         non con retorica. Se studiando scopri che la tua posizione NON regge, \
         dichiaralo onestamente con stance=oppose e i rischi che l'hanno demolita: \
         e' il contributo piu' prezioso che puoi dare, non una sconfitta.\n\n\
         Chiudi OBBLIGATORIAMENTE con il tool debate_position, ripetendo in \
         assigned_position ESATTAMENTE la posizione assegnata qui sopra.",
        assigned = assignment.assigned_position,
    )
}

/// Fan-out degli avvocati: un task tokio per assegnazione, col proprio timer,
/// sotto il governor di processo. Stesso punto unico del consiglio (regola L,
/// difetto D3 dell'incidente fan-out congelato). L'ordine dei risultati segue
/// quello delle assegnazioni: e' l'ancora dell'attribuzione posizionale.
async fn spawn_advocates(
    ctx: &AgentToolContext,
    cfg: &DebateConfig,
    topic: &str,
    assignments: &[nexus_agent_graph::decisions::debate_panel::DebateAssignment],
    user_task: &str,
) -> Vec<Value> {
    let expected = "Chiudi chiamando debate_position (assigned_position ripetuta alla \
                    lettera, stance support|oppose, key_arguments con evidenza, risks \
                    con severity). Un oppose richiede almeno un rischio con evidenza.";
    let assignments_for_make = assignments.to_vec();
    let topic_owned = topic.to_string();
    let kind = cfg.kind.clone();
    spawn_fanout(
        &ctx.core.db,
        assignments_for_make.len(),
        FanoutScope::TopLevel,
        |i| {
            let ctx = ctx.clone();
            let expected = expected.to_string();
            let kind = kind.clone();
            let task = build_advocate_task(&topic_owned, &assignments_for_make[i], user_task);
            async move {
                run_single_subagent(&ctx, &kind, &task, "", &expected, None, false, &[]).await
            }
        },
    )
    .await
}

/// Convoca gli AVVOCATI del dibattito in PARALLELO (sub-run read-only) e compone
/// l'esito col punto unico PURO `compose_debate_synthesis` (regola L/M: legge i
/// segnali strutturati `outcome.debate`, mai la prosa).
///
/// `None` se il piano e' vuoto (meno di 2 opzioni o meno di 2 avvocati: senza
/// contraddittorio non e' un dibattito) o se nessun avvocato produce una
/// posizione valida.
pub(crate) async fn convene_debate_panel(
    ctx: &AgentToolContext,
    cfg: &DebateConfig,
    topic: &str,
    // Opzioni dichiarate dal consiglio: `plan_debate` le taglia a quelle che
    // avranno davvero un difensore (mai un'opzione indifesa in gara).
    options: &[String],
    advocates: usize,
    user_task: &str,
    // Tipo GENERICO del quorum (`panel_quorum::QuorumPolicy`, path esplicito): il
    // re-export `decisions::QuorumPolicy` e' quello della review, con vocabolario
    // proprio (min_valid_verdicts/fail_on_high_severity). I due non sono
    // ri-esportati insieme di proposito.
    policy: &nexus_agent_graph::decisions::panel_quorum::QuorumPolicy,
    quorum_pct: u8,
) -> Option<DebatePanelOutcome> {
    use nexus_agent_graph::decisions::debate_panel::{compose_debate_synthesis, plan_debate};
    let n = advocates.min(cfg.max_advocates);
    let assignments = plan_debate(options, n);
    if assignments.is_empty() {
        return None;
    }
    let results = spawn_advocates(ctx, cfg, topic, &assignments, user_task).await;
    let outcomes: Vec<Value> = results
        .into_iter()
        .map(|r| r.get("outcome").cloned().unwrap_or(Value::Null))
        .collect();
    // `outcomes[i]` e' il sub-run di `assignments[i]`: spawn_fanout preserva
    // l'ordine (awaita gli handle in sequenza). E' su questa corrispondenza che
    // poggia l'attribuzione POSIZIONALE del voto (regola M): la posizione difesa
    // e' un fatto deciso da noi, non una stringa che il modello ricopia.
    // Il roster (denominatore del quorum) e' assignments.len(): un avvocato
    // morto e' un'astensione che pesa (lezione mig 0589).
    let synthesis = compose_debate_synthesis(&outcomes, &assignments, policy, quorum_pct)?;
    Some(DebatePanelOutcome {
        topic: topic.to_string(),
        assignments,
        synthesis,
    })
}

/// Corpo del blocco dibattito: sostegno per opzione, argomenti e rischi.
fn render_debate_tally(
    s: &nexus_agent_graph::decisions::debate_panel::DebateSynthesis,
) -> String {
    let mut out = String::from("Sostegno per opzione:\n");
    for t in &s.tally {
        let flag = if t.disqualified {
            " [SQUALIFICATA: arresa dal suo stesso avvocato con evidenza grave]"
        } else {
            ""
        };
        out.push_str(&format!(
            "- {}: {} a favore, {} arrese{}\n",
            t.option, t.support, t.surrendered, flag
        ));
    }
    if !s.key_arguments.is_empty() {
        out.push_str("Argomenti chiave emersi:\n");
        for a in &s.key_arguments {
            out.push_str(&format!("- {a}\n"));
        }
    }
    if !s.risks.is_empty() {
        out.push_str("Rischi emersi (per severity):\n");
        for risk in &s.risks {
            let sev = risk.get("severity").and_then(Value::as_str).unwrap_or("?");
            let desc = risk.get("description").and_then(Value::as_str).unwrap_or("");
            out.push_str(&format!("- [{sev}] {desc}\n"));
        }
    }
    out
}

/// Rende l'esito del dibattito in un blocco `<dibattito_sintesi>` da anteporre al
/// primo messaggio del run. Deriva dai campi STRUTTURATI (regola M). Dichiara
/// SEMPRE la base dei voti: un esito senza base dichiarata e' il proxy che ha
/// prodotto l'incidente "proceed con 1 parere su 5" (mig 0589).
pub(crate) fn render_debate_synthesis(o: &DebatePanelOutcome) -> String {
    use nexus_agent_graph::decisions::debate_panel::DebatePanelVerdict;
    let s = &o.synthesis;
    let mut out = String::from("<dibattito_sintesi>\n");
    out.push_str(&format!("Decisione in discussione: {}\n", o.topic));
    out.push_str(&format!("Esito del dibattito: {}\n", s.verdict.as_str()));
    out.push_str(&format!(
        "Posizioni valide: {} su {} avvocati convocati (quorum minimo: {}); opzioni \
         effettivamente discusse: {}\n",
        s.valid, s.convened, s.required_valid, s.options_heard
    ));
    if s.misattributed > 0 {
        out.push_str(&format!(
            "NB: {} avvocati hanno argomentato una posizione diversa da quella assegnata: \
             i loro voti sono stati scartati.\n",
            s.misattributed
        ));
    }
    out.push_str(&render_debate_tally(s));
    // Coda decisa dal VERDETTO con match ESAUSTIVO (regola M): un esito nuovo non
    // compila finche' non ne viene dichiarata la semantica testuale.
    let closing = match s.verdict {
        DebatePanelVerdict::OptionSelected => format!(
            "(Il dibattito ha selezionato: {}. Non e' un ordine: e' l'opzione che ha \
             retto al contraddittorio. Se decidi diversamente, dichiara perche'.)\n",
            s.selected_option.as_deref().unwrap_or("?")
        ),
        DebatePanelVerdict::Split => String::from(
            "(Il dibattito NON ha un vincitore: le posizioni si equivalgono o sono state \
             tutte demolite. Decidi tu sul merito degli argomenti qui sopra, dichiarando \
             il criterio che usi.)\n",
        ),
        DebatePanelVerdict::Inconclusive => format!(
            "(ATTENZIONE: il dibattito NON ha deliberato — {} posizioni valide su {} \
             avvocati convocati (minimo {}), e solo {} opzioni hanno avuto voce. Quanto \
             sopra e' parziale, NON un confronto concluso: se una sola posizione e' stata \
             difesa, il fatto che regga non dice nulla sulle alternative, che nessuno ha \
             sostenuto. Non trattarlo come una scelta.)\n",
            s.valid, s.convened, s.required_valid, s.options_heard
        ),
    };
    out.push_str(&closing);
    out.push_str("</dibattito_sintesi>");
    out
}

/// Coda del blocco sintesi, decisa dal VERDETTO con match ESAUSTIVO (regola M):
/// la frase "parere convergente" e' lecita solo quando il panel ha davvero
/// deliberato; sotto quorum il testo DICHIARA la parzialita'. Un verdetto nuovo
/// non compila finche' qualcuno non ne dichiara la semantica testuale (stesso
/// pattern di `FinalGateVerdict`).
fn synthesis_closing_note(s: &AdvisorySynthesis, panel_label: &str) -> String {
    match s.verdict {
        AdvisoryPanelVerdict::Proceed
        | AdvisoryPanelVerdict::ProceedWithChanges
        | AdvisoryPanelVerdict::Block => format!(
            "(Questo e' il parere convergente {panel_label}: rispetta i requisiti \
             obbligatori e fermati sui rischi bloccanti.)\n"
        ),
        AdvisoryPanelVerdict::Inconclusive => format!(
            "(ATTENZIONE: quorum NON raggiunto — {} pareri validi su {} convocate, \
             minimo richiesto {}. Il panel NON ha deliberato: i punti sopra sono \
             pareri PARZIALI delle sole figure che hanno risposto, non un consenso. \
             Non trattarli come approvazione.)\n",
            s.valid, s.convened, s.required_valid
        ),
    }
}

/// Rende la sintesi del consiglio in un blocco testuale `<consiglio_sintesi>` da
/// anteporre al primo messaggio del run (il modello legge requisiti/rischi/verdetto
/// come vincoli operativi). Deriva dai campi STRUTTURATI (regola M), non dalla prosa.
/// Dichiara SEMPRE la base dei voti (validi/convocate/quorum): un verdetto senza
/// base dichiarata e' il proxy che ha prodotto l'incidente "proceed con 1 parere
/// su 5".
pub(crate) fn render_council_synthesis(s: &AdvisorySynthesis) -> String {
    let mut out = String::from("<consiglio_sintesi>\n");
    out.push_str(&format!("Verdetto del consiglio: {}\n", s.verdict.as_str()));
    out.push_str(&format!(
        "Pareri validi: {} su {} figure convocate (quorum minimo: {})\n",
        s.valid, s.convened, s.required_valid
    ));
    if !s.requirements.is_empty() {
        out.push_str("Requisiti obbligatori:\n");
        for r in &s.requirements {
            out.push_str(&format!("- {}\n", r.text));
        }
    }
    if !s.risks.is_empty() {
        out.push_str("Rischi (per severity):\n");
        for risk in &s.risks {
            let sev = risk.get("severity").and_then(Value::as_str).unwrap_or("?");
            let desc = risk
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            out.push_str(&format!("- [{sev}] {desc}\n"));
        }
    }
    if !s.recommendations.is_empty() {
        out.push_str("Raccomandazioni:\n");
        for r in &s.recommendations {
            out.push_str(&format!("- {r}\n"));
        }
    }
    out.push_str(&synthesis_closing_note(s, "del consiglio di figure"));
    out.push_str("</consiglio_sintesi>");
    out
}

pub(crate) fn render_multi_provider_synthesis(s: &AdvisorySynthesis) -> String {
    let mut out = String::from("<multi_provider_sintesi>\n");
    out.push_str(&format!(
        "Verdetto del panel multi-provider: {}\n",
        s.verdict.as_str()
    ));
    out.push_str(&format!(
        "Pareri validi: {} su {} provider convocati (quorum minimo: {}); dissenso: {}\n",
        s.valid, s.convened, s.required_valid, s.dissent
    ));
    if !s.requirements.is_empty() {
        out.push_str("Requisiti obbligatori convergenti:\n");
        for r in &s.requirements {
            out.push_str(&format!("- {}\n", r.text));
        }
    }
    if !s.risks.is_empty() {
        out.push_str("Rischi multi-provider (per severity):\n");
        for risk in &s.risks {
            let sev = risk.get("severity").and_then(Value::as_str).unwrap_or("?");
            let desc = risk
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            out.push_str(&format!("- [{sev}] {desc}\n"));
        }
    }
    if !s.recommendations.is_empty() {
        out.push_str("Raccomandazioni:\n");
        for r in &s.recommendations {
            out.push_str(&format!("- {r}\n"));
        }
    }
    out.push_str(&synthesis_closing_note(s, "di modelli/provider diversi"));
    out.push_str("</multi_provider_sintesi>");
    out
}

// ─── Narrazione del sub-run sul run PADRE (ADR 0037) ───────────────────────
//
// Il dispatch sub-agente e' BLOCCANTE e puo' durare minuti: senza narrazione la
// chat del run padre resta muta (nessun meta-step, `updated_at` fermo) e il run
// sembra bloccato mentre il figlio lavora. Qui il tool — che conosce il canale
// SSE del run invocante via `ctx.parent_narration` — emette sul PADRE:
//   1. `subagent_started`  all'avvio (kind + task);
//   2. `subagent_progress` per ogni tool CONCLUSO del figlio (esito dal segnale
//      strutturato `AgentStepStatus`, regola M) + heartbeat nei silenzi;
//   3. `subagent_completed`/`subagent_failed` alla chiusura, col summary.
// Ogni step porta `correlation_id = subagent_run_id`. La composizione
// live+storico delega al PUNTO UNICO `emit_phase_meta_correlated` (regola L).

/// Compositore della narrazione sul run PADRE: sink SSE + store meta-step del
/// run invocante. `say` delega al punto unico `emit_phase_meta_correlated`.
/// Porta anche il PIN provider/model del sub-run (risolto dal model_purpose al
/// dispatch, vuoto = routing di default): ogni meta `subagent_*` lo espone nel
/// payload via [`ParentNarrator::with_pin`], cosi' il nastro frontend attribuisce
/// ai blocchi del figlio la SUA provenienza (non quella del padre).
struct ParentNarrator {
    sink: crate::agent_graph_adapter::event_sink::SseEventSinkAdapter,
    store: crate::agent_graph_adapter::meta_step_store::PgMetaStepStore,
    pin_provider: String,
    pin_model: String,
}

impl ParentNarrator {
    /// Costruisce il narratore per il run invocante. `None` se la narrazione e'
    /// disabilitata (setting) o il ctx non porta il canale (tool invocato fuori
    /// dal grafo nativo): il dispatch degrada al comportamento muto storico.
    /// `provider`/`model`: pin del sub-run quando risolto (vuoto = omesso).
    fn from_ctx(
        ctx: &AgentToolContext,
        proj_pool: &sqlx::PgPool,
        enabled: bool,
        provider: &str,
        model: &str,
    ) -> Option<std::sync::Arc<Self>> {
        if !enabled {
            return None;
        }
        let pn = ctx.parent_narration.as_ref()?;
        Some(std::sync::Arc::new(Self {
            sink: crate::agent_graph_adapter::event_sink::SseEventSinkAdapter::with_persistence(
                pn.step_tx.clone(),
                pn.run_id,
                pn.session_id,
                proj_pool.clone(),
            ),
            store: crate::agent_graph_adapter::meta_step_store::PgMetaStepStore::new(
                proj_pool.clone(),
                pn.run_id,
            ),
            pin_provider: provider.to_string(),
            pin_model: model.to_string(),
        }))
    }

    /// Arricchisce un payload `subagent_*` col pin provider/model del figlio
    /// (punto unico, delega a [`put_model_fields`]): campi presenti SOLO quando
    /// il pin e' risolto, mai inventati (il frontend degrada a '?').
    fn with_pin(&self, mut payload: Value) -> Value {
        put_model_fields(&mut payload, &self.pin_provider, &self.pin_model);
        payload
    }

    /// Emette (live) + persiste (storico) UN meta-step della narrazione,
    /// correlato al sub-run. Best-effort: mai un errore al chiamante.
    async fn say(&self, kind: &str, title: String, payload: Value, subagent_run_id: Uuid) {
        nexus_agent_graph::nodes::emit_phase_meta_correlated(
            &self.sink,
            &self.store,
            kind,
            Some(subagent_run_id.to_string()),
            title,
            payload,
        )
        .await;
    }

    /// UN evento del sub-run sul ponte: memorizza il target dei ToolUse
    /// `Running` e narra i tool CONCLUSI (`concluded_tool_step`, regola M).
    /// Ritorna true se ha emesso un meta-step (azzera il silenzio heartbeat).
    async fn bridge_step(
        &self,
        targets: &mut std::collections::HashMap<u32, String>,
        ev: &crate::agent_types::AgentStepEvent,
        sub_kind: &str,
        sub_run_id: Uuid,
    ) -> bool {
        if let Some((idx, target)) = running_tool_target(ev) {
            targets.insert(idx, target);
        }
        let Some((is_error, tool, idx)) = concluded_tool_step(ev) else {
            return false;
        };
        let target = targets.remove(&idx);
        let (title, payload) =
            tool_progress_meta(sub_kind, sub_run_id, &tool, target.as_deref(), is_error);
        self.say(
            "subagent_progress",
            title,
            self.with_pin(payload),
            sub_run_id,
        )
        .await;
        true
    }

    /// Heartbeat "al lavoro" sul run padre, SOLO se il periodo e' trascorso in
    /// silenzio (nessun progresso emesso dall'ultimo tick).
    async fn maybe_heartbeat(
        &self,
        emitted_since_tick: bool,
        sub_kind: &str,
        sub_run_id: Uuid,
        started: &std::time::Instant,
    ) {
        if emitted_since_tick {
            return;
        }
        let elapsed = started.elapsed().as_secs();
        self.say(
            "subagent_progress",
            format!("Subagente {sub_kind}: al lavoro da {elapsed}s"),
            self.with_pin(json!({
                "phase": "working",
                "elapsed_s": elapsed,
                K_SUB_RUN_ID: sub_run_id.to_string(),
                K_SUB_KIND: sub_kind,
            })),
            sub_run_id,
        )
        .await;
    }
}

/// Arricchisce il payload di un meta `subagent_*` con provider/model del
/// figlio QUANDO NOTI (risoluzione dal model_purpose al dispatch; stringa
/// vuota = routing di default nel motore -> campo omesso, mai inventato).
/// PURA: il frontend (icona provenienza del nastro) legge questi campi.
fn put_model_fields(payload: &mut Value, provider: &str, model: &str) {
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    if !provider.trim().is_empty() {
        obj.insert(K_PROVIDER.into(), json!(provider));
    }
    if !model.trim().is_empty() {
        obj.insert(K_MODEL.into(), json!(model));
    }
}

/// Decisione PURA (regola M): uno step del sub-run va inoltrato al padre solo
/// se CONCLUSO — `Completed` (ok) o `Failed` (errore) dal segnale strutturato
/// `AgentStepStatus`, mai dal testo. `Running` e' escluso (doppione per tool);
/// uno step senza nome (ToolResult orfano) non produce narrazione utile.
fn concluded_tool_step(ev: &crate::agent_types::AgentStepEvent) -> Option<(bool, String, u32)> {
    let step = ev.step.as_ref()?;
    let is_error = match step.status {
        crate::agent_types::AgentStepStatus::Completed => false,
        crate::agent_types::AgentStepStatus::Failed => true,
        _ => return None,
    };
    if step.tool_name.is_empty() {
        return None;
    }
    Some((is_error, step.tool_name.clone(), step.step_index))
}

/// Target leggibile dallo step `Running` del sub-run: il ToolUse porta l'input
/// REALE in `tool_input.input` (il ToolResult no — vedi `SseEventSinkAdapter`),
/// quindi il ponte lo memorizza per `step_index` e lo riusa nel titolo alla
/// conclusione. Estrazione col punto unico del grafo (`tool_target_from_input`).
fn running_tool_target(ev: &crate::agent_types::AgentStepEvent) -> Option<(u32, String)> {
    let step = ev.step.as_ref()?;
    if step.status != crate::agent_types::AgentStepStatus::Running {
        return None;
    }
    let target = step
        .tool_input
        .get("input")
        .and_then(nexus_agent_graph::nodes::tool_target_from_input)?;
    Some((step.step_index, target))
}

/// Compone titolo + payload del meta-step `subagent_progress` per un tool
/// concluso del sub-run (PURA, testabile). Stesso registro della narrazione
/// tool del run corrente ("tool edit_file — src/x.ts" / "errore run_command").
fn tool_progress_meta(
    sub_kind: &str,
    subagent_run_id: Uuid,
    tool: &str,
    target: Option<&str>,
    is_error: bool,
) -> (String, Value) {
    let esito = if is_error { "errore" } else { "tool" };
    let title = match target {
        Some(t) => format!("subagente {sub_kind}: {esito} {tool} — {t}"),
        None => format!("subagente {sub_kind}: {esito} {tool}"),
    };
    let payload = json!({
        "phase": "tool",
        "tool": tool,
        K_TARGET: target,
        K_IS_ERROR: is_error,
        K_SUB_RUN_ID: subagent_run_id.to_string(),
        K_SUB_KIND: sub_kind,
    });
    (title, payload)
}

/// Ponte narrazione: consuma gli eventi SSE del SUB-run (prima scartati:
/// `_sub_rx`, la feature era muta) e li traduce in meta-step
/// `subagent_progress` sul run PADRE, con heartbeat nei silenzi (LLM call
/// lunghe senza tool). Termina alla chiusura del canale; il chiamante lo
/// aborta comunque prima del meta-step di chiusura.
fn spawn_narration_bridge(
    narrator: std::sync::Arc<ParentNarrator>,
    mut rx: tokio::sync::broadcast::Receiver<crate::agent_types::AgentStepEvent>,
    sub_run_id: Uuid,
    sub_kind: String,
    heartbeat_s: i64,
) -> tokio::task::JoinHandle<()> {
    use tokio::sync::broadcast::error::RecvError;
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        let hb_enabled = heartbeat_s > 0;
        // Con heartbeat disabilitato il branch del tick resta spento (guard
        // `if hb_enabled`): il periodo placeholder non scatta mai.
        let period = if hb_enabled { heartbeat_s as u64 } else { 3600 };
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(period));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Il primo tick di `interval()` e' immediato: consumato subito, il
        // heartbeat parte dal primo periodo pieno.
        tick.tick().await;
        let mut targets: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        let mut emitted_since_tick = false;
        loop {
            tokio::select! {
                recv = rx.recv() => {
                    // Canale chiuso = fine del sub-run. Lagged: il ponte e'
                    // narrazione best-effort, gli eventi persi non lo fermano.
                    if matches!(recv, Err(RecvError::Closed)) { break; }
                    let Ok(ev) = recv else { continue };
                    let said = narrator.bridge_step(&mut targets, &ev, &sub_kind, sub_run_id).await;
                    emitted_since_tick = emitted_since_tick || said;
                }
                _ = tick.tick(), if hb_enabled => {
                    narrator.maybe_heartbeat(emitted_since_tick, &sub_kind, sub_run_id, &started).await;
                    emitted_since_tick = false;
                }
            }
        }
    })
}

/// Slot di isolamento fisico di un sub-run (FASE 2): il worktree effimero
/// pre-creato dal batch e i metadati da persistire su `nexus_subagent_runs` per
/// audit/reconcile. `run_id` E' l'identita' del sub-run (row DB) E del worktree
/// (dir/branch): un solo id per entrambi, cosi' il GC filesystem (dir name =
/// run_id) e la row DB restano allineati.
pub(crate) struct IsolationSlot {
    /// Identita' condivisa row sub-run + worktree.
    run_id: Uuid,
    /// Path del worktree effimero (override root del sub-run).
    worktree_path: std::path::PathBuf,
    /// Commit da cui il worktree e' stato staccato (persistito per replay/reconcile).
    base_commit: String,
}

/// Esegue UNA sub-run sul GRAFO NATIVO. Ritorna sempre un `Value`: il sommario in
/// caso di successo, `{"error": "..."}` su guasto. Replica le guard del brain e
/// mappa l'esito del run nativo al tool_result atteso dal main.
///
/// `isolation`: `None` (default, ramo sequenziale/condiviso) -> il sub-run scrive
/// sulla root del progetto, il `run_id` e' generato dal DB (`DEFAULT`),
/// comportamento invariato. `Some(slot)` (ramo ISOLATO, valorizzato solo dal batch
/// parallelo di `tool_dispatch_subagents`) -> il sub-run usa `slot.run_id` come id,
/// scrive nel worktree `slot.worktree_path` (ctx isolato: autocommit/reindex
/// soppressi, PR3) e persiste `worktree_path`/`base_commit`. L'apply dei
/// cambiamenti alla root e' responsabilita' SERIALIZZATA del chiamante batch.
/// Esegue un task GIA' PARSATO del batch come sub-run sequenziale. PUNTO UNICO
/// (regola L) dei tre rami che dispatchano un `ParsedTask`: ondata sequenziale,
/// ramo isolato (con lo slot worktree) e degrado dall'isolamento.
///
/// Erano tre chiamate identiche a [`run_single_subagent`], che passavano gli
/// stessi sei campi di `p` nello stesso ordine: ogni volta che la firma cambia,
/// tre punti vanno aggiornati in modo coerente, e basta dimenticarne uno perche'
/// un ramo dispatchi con un campo in meno senza che nulla lo segnali. Un campo
/// aggiunto qui raggiunge tutti e tre per costruzione.
///
/// `isolation` e' l'unica differenza reale fra i chiamanti; nessuno dei tre e'
/// mai background (l'apply serializzato del ramo isolato esige sub-run conclusi).
async fn run_parsed_task(
    ctx: &AgentToolContext,
    p: &ParsedTask,
    isolation: Option<&IsolationSlot>,
) -> Value {
    run_single_subagent(
        ctx,
        &p.kind,
        &p.task,
        &p.context_blob,
        &p.expected,
        isolation,
        false,
        &p.write_scope,
    )
    .await
}

pub(crate) async fn run_single_subagent(
    ctx: &AgentToolContext,
    kind: &str,
    task: &str,
    context_blob: &str,
    expected_format: &str,
    isolation: Option<&IsolationSlot>,
    is_background: bool,
    write_scope: &[String],
) -> Value {
    run_single_subagent_inner(
        ctx,
        kind,
        task,
        context_blob,
        expected_format,
        isolation,
        is_background,
        None,
        write_scope,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_single_subagent_with_model_pin(
    ctx: &AgentToolContext,
    kind: &str,
    task: &str,
    context_blob: &str,
    expected_format: &str,
    provider: &str,
    model: &str,
) -> Value {
    run_single_subagent_inner(
        ctx,
        kind,
        task,
        context_blob,
        expected_format,
        None,
        false,
        Some((provider, model)),
        // Le figure a modello pinnato (panel di review, consiglio) non eseguono un
        // passi del piano: non hanno uno scope dichiarato da misurare.
        &[],
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_single_subagent_inner(
    ctx: &AgentToolContext,
    kind: &str,
    task: &str,
    context_blob: &str,
    expected_format: &str,
    isolation: Option<&IsolationSlot>,
    is_background: bool,
    model_pin: Option<(&str, &str)>,
    write_scope: &[String],
) -> Value {
    // FASE PREPARE (guard + INSERT + ensure_child, tutto SINCRONO fail-fast): il
    // punto unico condiviso dal ramo singolo e dal batch background (regola L).
    let exec = match prepare_subagent_run(
        ctx,
        kind,
        task,
        context_blob,
        expected_format,
        isolation,
        is_background,
        model_pin,
        write_scope,
    )
    .await
    {
        Ok(exec) => exec,
        Err(err) => return err,
    };

    if is_background {
        // ── Dispatch ASINCRONO (Fase D fan-in): il padre non attende. Il prepare
        //    (insert + ensure_child) e' gia' avvenuto: l'id esiste e la row e'
        //    `running`. L'ESECUZIONE parte in un task DETACHED (input owned
        //    `'static`). Il Value ritornato porta il SEGNALE STRUTTURATO
        //    `background_dispatched` (regola M) che il ToolDispatchNode legge per
        //    sospendere il motore. ─────────────────────────────────────────────
        return spawn_background_subagent(exec);
    }

    execute_subagent_run(exec).await
}

/// FASE PREPARE di un sub-run (PUNTO UNICO, regola L): guard settings/whitelist/
/// depth/cost + INSERT `nexus_subagent_runs` (status='running') + `ensure_child_
/// agent_run`. Tutto SINCRONO e fail-fast: al ritorno `Ok` la row esiste ed e'
/// `running`, cosi' la catena depth/cost e il fan-in COUNT la vedono. Ritorna
/// `Err(Value)` con l'errore da restituire al chiamante (guard non superato /
/// INSERT fallito).
///
/// PERCHE' e' separata dallo spawn (bug fan-in prematuro nel batch background): il
/// batch DEVE inserire TUTTE le row PRIMA di spawnare qualunque esecuzione. Se
/// inserisse+spawnasse una alla volta, il 1o figlio potrebbe terminare (task
/// detached) mentre gli altri non sono ancora in `nexus_subagent_runs` -> la COUNT
/// del fan-in vedrebbe 0 rimasti -> enqueue PREMATURO del parent (riesumato senza
/// i risultati degli altri figli). Inserendo tutte le row prima, la COUNT e'
/// corretta appena il 1o figlio chiude.
#[allow(clippy::too_many_arguments)]
async fn prepare_subagent_run(
    ctx: &AgentToolContext,
    kind: &str,
    task: &str,
    context_blob: &str,
    expected_format: &str,
    isolation: Option<&IsolationSlot>,
    is_background: bool,
    model_pin: Option<(&str, &str)>,
    write_scope: &[String],
) -> Result<SubagentExecInputs, Value> {
    let db = &*ctx.core.db;
    let project_id = ctx.core.project_id;
    let session_id = match ctx.core.session_id {
        Some(s) => s,
        None => {
            return Err(prepare_reject(
                "session_missing",
                "sub-agent richiede una sessione chat (session_id assente)",
            ))
        }
    };
    // Routing separazione DB: nexus_subagent_runs e' tabella migrata, vive nel DB
    // del progetto. Risolvo una volta il pool per-progetto e lo riuso per la catena
    // depth/costo, l'INSERT e le mark_run. DB non disponibile -> rifiuto
    // strutturato del prepare (regola M: il codice macchina, non la prosa).
    let proj_pool = match crate::project_db_routes::project_data_pool_from(db, project_id).await {
        Ok(p) => p,
        Err(e) => return Err(prepare_reject(e.error_code(), e.to_string())),
    };

    // ── Guard 1: settings (enabled / whitelist / depth / cost) ────────────────
    let settings = match read_subagent_settings(ctx).await {
        Ok(v) => v,
        Err(e) => {
            return Err(prepare_reject(
                "settings_read_failed",
                format!("lettura settings fallita: {e}"),
            ))
        }
    };
    if !settings.enabled {
        return Err(prepare_reject(
            "subagents_disabled",
            "sub-agents disabilitati (orchestrator.subagents_enabled=false)",
        ));
    }
    if !settings.whitelist.iter().any(|w| w == kind) {
        return Err(prepare_reject(
            "kind_not_whitelisted",
            format!("kind '{kind}' non in whitelist: {:?}", settings.whitelist),
        ));
    }

    // ── Guard 2: definition del kind ──────────────────────────────────────────
    let definition = match fetch_definition(db, kind).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            return Err(prepare_reject(
                "kind_not_found",
                format!("kind '{kind}' non trovato in nexus_subagent_definitions"),
            ))
        }
        Err(e) => return Err(prepare_reject("definition_fetch_failed", e)),
    };

    // ── Guard 3: anti-ricorsione (depth DB-driven dalla catena ANTENATI) ──────
    // La profondita' del figlio = depth del DISPATCHER (padre immediato) + 1, NON
    // il MAX tra i fratelli running sotto l'anchor: i fratelli paralleli (es. le 6
    // figure del consiglio) non devono contarsi a vicenda come annidamento.
    let anchor = parent_anchor(ctx);
    let dispatcher = fanin_target_run_id(ctx, anchor);
    let current_depth = current_chain_depth(&proj_pool, dispatcher).await + 1;
    if current_depth > settings.max_depth {
        return Err(prepare_reject(
            "depth_exceeded",
            format!(
                "depth {current_depth} > max {}: annidamento sub-agent eccessivo (anti-ricorsione)",
                settings.max_depth
            ),
        ));
    }

    // ── Guard 4: cost cap cumulativo per parent ───────────────────────────────
    let spent = cumulative_cost(&proj_pool, anchor).await;
    if spent >= settings.cost_cap_usd {
        return Err(prepare_reject(
            "cost_cap_reached",
            format!(
                "cost cap raggiunto per parent={anchor} ({spent:.4} >= {:.4})",
                settings.cost_cap_usd
            ),
        ));
    }

    // ── Risoluzione system_text + tools + modello worker (DB-driven) ──────────
    let system_text = resolve_system_text(ctx, &definition.prompt_key).await;
    if system_text.trim().is_empty() {
        return Err(prepare_reject(
            "prompt_missing",
            format!("prompt '{}' non trovato o vuoto", definition.prompt_key),
        ));
    }
    let tools_json = build_tools_json(&definition.tool_whitelist);

    let (provider, model) = if let Some((provider, model)) = model_pin {
        (provider.to_string(), model.to_string())
    } else {
        resolve_worker_model(db, &proj_pool, kind, &definition, anchor, session_id).await
    };

    let timeout_s = if definition.timeout_s > 0 {
        definition.timeout_s
    } else {
        settings.default_timeout_s
    };
    // Clamp alla DEADLINE del run primario (fase 3, mig 0604): un sub-run non
    // vive oltre il tempo residuo del run che lo ha convocato. Sotto il floor
    // (`subagent_min_timeout_s`) la figura NON parte: rifiuto strutturato
    // (regola M), un timeout ridicolo produrrebbe solo spesa senza esito.
    let timeout_s = match run_time_remaining_s(&ctx.core.db, &proj_pool, anchor).await {
        Some(remaining_s) => {
            let floor_s = subagent_min_timeout_s(&ctx.core.db).await;
            if remaining_s < floor_s {
                return Err(prepare_reject(
                    "deadline_exhausted",
                    format!(
                        "deadline del run quasi esaurita per parent={anchor} \
                         (residuo {remaining_s}s < floor {floor_s}s)"
                    ),
                ));
            }
            timeout_s.min(remaining_s)
        }
        None => timeout_s,
    };

    // Embedding del context/expected nel task (parita' col brain `run_subagent`).
    let mut initial_msg = task.trim().to_string();
    if !context_blob.trim().is_empty() {
        initial_msg.push_str("\n\n## Contesto aggiuntivo\n");
        initial_msg.push_str(context_blob.trim());
    }
    if !expected_format.trim().is_empty() {
        initial_msg.push_str("\n\n## Formato output atteso\n");
        initial_msg.push_str(expected_format.trim());
    }

    // ── Crea row in nexus_subagent_runs (status='running' subito: la catena depth
    //    si basa sui 'running'; il sub-run e' bloccante, non c'e' fase 'pending'
    //    osservabile). ────────────────────────────────────────────────────────────
    let insert = NewSubagentRun {
        anchor,
        // Run CORRENTE che dispatcha: isola i figli DIRETTI per COUNT/fetch del
        // fan-in (mig project 0010). E' lo STESSO id che finisce in
        // `fanin_parent_run_id` (il target del resume): la COUNT sui figli con
        // dispatcher = questo run e la coda che punta a questo run coincidono.
        dispatcher_run_id: fanin_target_run_id(ctx, anchor),
        project_id,
        kind,
        task,
        context_blob,
        expected_format,
        depth: current_depth as i32,
        is_background,
    };
    let subagent_run_id: Uuid = match insert_subagent_run(&proj_pool, &insert, isolation).await {
        Ok(id) => id,
        Err(e) => {
            return Err(prepare_reject(
                "insert_failed",
                format!("creazione riga nexus_subagent_runs fallita: {e}"),
            ))
        }
    };

    // ── Riga agent_runs del SUB-run (osservabilita', regola M) ────────────────
    // Il sub-run gira sul grafo nativo con run_id=subagent_run_id ma non aveva
    // alcuna riga in agent_runs: il guard "untracked_run" di PgAgentStepStore
    // scartava in SILENZIO ogni tool_result del figlio (agent_steps vuoto ->
    // errori dei tool illeggibili, incidente 2026-07-06). La riga rende il
    // figlio un run TRACCIATO: step (input+result+esito) persistiti e
    // ispezionabili. `nexus_agent_type='subagent'` e' il discriminatore per le
    // query UI top-level (active-run, timeline, guard 409); `parent_run_id` e'
    // valorizzato solo se l'ancora e' un run tracciato (FK su agent_runs).
    ensure_child_agent_run(
        &proj_pool,
        subagent_run_id,
        session_id,
        project_id,
        ctx.core.user_id,
        anchor,
        &provider,
        &model,
    )
    .await;

    // ── Input OWNED della parte "execute" (regola L: punto unico condiviso dai
    //    due rami sync/background). Tutto e' `'static` (String/Uuid/Value/PgPool
    //    clonabile): l'helper puo' essere `await`ato inline oppure spostato in un
    //    task detached senza catturare riferimenti allo stack del chiamante. ─────
    Ok(SubagentExecInputs {
        ctx: ctx.clone(),
        proj_pool,
        subagent_run_id,
        session_id,
        anchor,
        kind: kind.to_string(),
        task: task.to_string(),
        provider,
        model,
        system_text,
        prompt_key: definition.prompt_key.clone(),
        initial_msg,
        tools_json,
        current_depth,
        timeout_s,
        narration_enabled: settings.narration_enabled,
        narration_heartbeat_s: settings.narration_heartbeat_s,
        // Il ramo background NON supporta isolamento (vedi doc su tool_dispatch_*):
        // qui `working_root` porta il worktree solo per il ramo sequenziale isolato.
        working_root: isolation.map(|s| s.worktree_path.clone()),
        // Scope dichiarato dal pianificatore per il passo di piano di questo sub-run. A
        // differenza di `working_root` viaggia in ENTRAMBI i rami (isolato e
        // condiviso) e anche in background: e' una misura, e misurare solo il ramo
        // isolato — che senza il flag di isolamento non scatta mai — vorrebbe dire
        // non misurare niente e leggere zeri rassicuranti.
        write_scope: write_scope.to_vec(),
        isolated: isolation.is_some(),
        // Fase D fan-in: il ramo background enqueue il parent al termine se e'
        // l'ultimo figlio background terminale (vedi execute_subagent_run).
        is_background,
        // Run da riprendere nel fan-in: il run CORRENTE (sospeso su
        // awaiting_subagents), NON l'anchor depth-chain (bug: il CAS del worker
        // cerca agent_runs.id = questo id).
        fanin_parent_run_id: fanin_target_run_id(ctx, anchor),
    })
}

/// Spawna l'esecuzione DETACHED di un sub-run gia' preparato (row inserita) e
/// ritorna subito il SEGNALE STRUTTURATO `background_dispatched` (regola M). Punto
/// unico (regola L) del ramo background, riusato dal singolo e dal batch: cosi' il
/// batch puo' preparare TUTTE le row (COUNT fan-in corretta) e POI spawnare tutte.
fn spawn_background_subagent(exec: SubagentExecInputs) -> Value {
    let subagent_run_id = exec.subagent_run_id;
    let kind_out = exec.kind.clone();
    tokio::spawn(async move {
        let _ = execute_subagent_run(exec).await;
    });
    json!({
        "background_dispatched": true,
        K_SUB_RUN_ID: subagent_run_id.to_string(),
        "kind": kind_out,
        "status": "running",
    })
}

/// Input OWNED (`'static`) della parte "execute" di un sub-run: narrazione avvio ->
/// grafo nativo -> finalize. Raggruppa i parametri gia' risolti da
/// [`run_single_subagent`] cosi' l'helper puo' essere `await`ato inline (ramo
/// sincrono) o spostato in un `tokio::spawn` (ramo background) senza catturare
/// riferimenti allo stack del chiamante (Fase D fan-in). Tutti i campi sono owned
/// e clonabili -> il task detached e' `'static`.
struct SubagentExecInputs {
    ctx: AgentToolContext,
    proj_pool: sqlx::PgPool,
    subagent_run_id: Uuid,
    session_id: Uuid,
    anchor: Uuid,
    kind: String,
    task: String,
    provider: String,
    model: String,
    system_text: String,
    /// Chiave del template di sistema del sub-run
    /// (`nexus_subagent_definitions.prompt_key`): viaggia fino allo stato del
    /// grafo perche' il ReflectionNode attribuisca la reflection al prompt
    /// giusto invece che a quello del run principale.
    prompt_key: String,
    initial_msg: String,
    tools_json: Value,
    current_depth: i64,
    timeout_s: i64,
    narration_enabled: bool,
    narration_heartbeat_s: i64,
    /// Root del sub-run (worktree isolato) o `None` (root condivisa / background).
    working_root: Option<std::path::PathBuf>,
    /// Aree file dichiarate dal pianificatore per il passo di piano di questo sub-run
    /// (`ParsedTask::write_scope`, che risale a `nexus_agent_todos.write_scope`).
    /// Scende nel motore e da li' nel ctx dei tool, dove l'hook delle mutazioni
    /// MISURA quante scritture cadono fuori. Vuoto = non dichiarato.
    write_scope: Vec<String>,
    /// `true` = ramo isolato (payload narrazione avvio). Il background e' sempre
    /// `false` (isolamento non supportato in background).
    isolated: bool,
    /// `true` = figlio dispatchato in BACKGROUND (Fase D fan-in): al termine, se
    /// e' l'ULTIMO background terminale del suo parent, accoda il parent nella
    /// coda META `subagent_fanin_resume_queue` (il worker fan-in lo riprende).
    /// `false` (sincrono/sequenziale) -> nessun enqueue (il padre non e' sospeso).
    is_background: bool,
    /// Run da RIPRENDERE nel fan-in (= run CORRENTE che ha dispatchato, marcato
    /// `awaiting_subagents`). PUNTO UNICO [`fanin_target_run_id`]: e' l'id che il
    /// CAS del worker cerca in `agent_runs`, NON l'`anchor` (depth-chain).
    fanin_parent_run_id: Uuid,
}

/// Parte "execute" di un sub-run (PUNTO UNICO, regola L): narrazione avvio ->
/// esecuzione sul grafo nativo -> finalize. Riusata IDENTICA dal ramo sincrono
/// (`await` inline) e dal ramo background (`tokio::spawn`). Prende input OWNED
/// (`SubagentExecInputs`, `'static`) cosi' il task detached non cattura riferimenti
/// allo stack del chiamante. Ritorna il `Value` tool_result del sub-run (summary /
/// timeout / failure); nel ramo background il valore e' scartato (il fan-in legge
/// la row DB), ma tutti gli effetti (mark_run, narrazione) avvengono comunque.
async fn execute_subagent_run(exec: SubagentExecInputs) -> Value {
    let SubagentExecInputs {
        ctx,
        proj_pool,
        subagent_run_id,
        session_id,
        anchor,
        kind,
        task,
        provider,
        model,
        system_text,
        prompt_key,
        initial_msg,
        tools_json,
        current_depth,
        timeout_s,
        narration_enabled,
        narration_heartbeat_s,
        working_root,
        write_scope,
        isolated,
        is_background,
        fanin_parent_run_id,
    } = exec;

    // ── NARRAZIONE sul run PADRE (ADR 0037): avvio ────────────────────────────
    // Il dispatch (sincrono) e' bloccante e puo' durare minuti: senza questi
    // meta-step la chat resta muta e il run padre sembra bloccato mentre il figlio
    // lavora. Nel ramo background la narrazione racconta il progresso del figlio
    // sul run padre gia' sospeso (il fan-in arrivera' via poll).
    let narrator = ParentNarrator::from_ctx(&ctx, &proj_pool, narration_enabled, &provider, &model);
    if let Some(n) = &narrator {
        let task_head: String = task.trim().chars().take(160).collect();
        n.say(
            "subagent_started",
            format!("Subagente {kind} avviato — {task_head}"),
            n.with_pin(json!({
                K_SUB_RUN_ID: subagent_run_id.to_string(),
                K_SUB_KIND: kind,
                "task": task_head,
                "depth": current_depth,
                K_TIMEOUT_S: timeout_s,
                "isolated": isolated,
            })),
            subagent_run_id,
        )
        .await;
    }

    // ── Esecuzione sul GRAFO NATIVO (in-process, niente brain) ────────────────
    // Il sub-run e' un run a se': run_id = subagent_run_id (= thread del grafo),
    // STESSA session_id del parent (eredita root/permessi/canali). Lo stato porta
    // parent_run_id + subagent_depth -> il grafo applica i guard di annidamento
    // (UnderstandingNode salta il fan-out explore se depth>=1).
    let deps = build_native_deps_for_tool(&ctx, timeout_s).await;
    // Canale SSE proprio del sub-run: NON instrada al frontend (l'output utente
    // resta quello del main, che riceve solo il summary). Il receiver alimenta
    // il PONTE narrazione verso il padre (prima era scartato: feature muta);
    // senza narratore il receiver e' droppato e il comportamento e' lo storico.
    let (sub_tx, sub_rx) = tokio::sync::broadcast::channel(256);
    let bridge = narrator.as_ref().map(|n| {
        spawn_narration_bridge(
            n.clone(),
            sub_rx,
            subagent_run_id,
            kind.clone(),
            narration_heartbeat_s,
        )
    });

    // provider/model risolti: clonati per i rami finalize timeout/failure, che
    // girano DOPO che `native_input` prende possesso degli originali. Il ramo
    // success usa invece la provenienza EFFETTIVA da `o` (post-failover).
    let provider_resolved = provider.clone();
    let model_resolved = model.clone();
    // Prima della costruzione: `native_input` prende possesso di parte di `exec`,
    // e il veto ha bisogno del pool META che vive dentro il contesto.
    let provider_veto = veto_del_giudice(&ctx.core.db, &kind, anchor).await;
    let native_input = NativeRunInput {
        run_id: subagent_run_id,
        session_id,
        provider,
        model,
        // DECISIONE (misurata): il vincolo duro NON scende ai figli. I 19 kind
        // chiedono 4 tier diversi (medium 12, light 3, heavy 3, high 1) mentre un
        // provider ne copre da 1 a 5: con deepseek pinnato — 1 tier su 5 nel
        // catalogo — un pin ereditato lascerebbe senza modello 16 kind su 19, e
        // un solo fornitore in cooldown fermerebbe l'intero panel. Il provider
        // scelto per la chat li orienta comunque, ma come preferenza-forte che
        // degrada (`resolve_worker_model`): il figlio riparte altrove invece di
        // non partire. Per il kind `review` il vincolo sarebbe anche contrario a
        // quello piu' forte che gia' vige — il giudice non puo' essere il worker.
        provider_pin: crate::orchestrator::ProviderPin::none(),
        // ...e per lo stesso motivo quel vincolo, qui, va nella direzione
        // OPPOSTA: non «usa questo fornitore» ma «non usare quello del worker»,
        // e deve valere per tutto il run, non solo per la scelta iniziale del
        // modello. Senza questa riga il giudice tornava sul fornitore del padre
        // al primo ripiego.
        provider_veto,
        system_text,
        // Il sub-run porta la chiave della PROPRIA definizione, cosi' le sue
        // reflection sono attribuite al prompt giusto e non a quello del run
        // principale.
        prompt_key: Some(prompt_key.clone()),
        initial_msg,
        // Sub-run: il task e' descritto nel prompt, nessun allegato proprio.
        attachment_kinds: Vec::new(),
        // Sub-run isolato: NIENTE history del main (parita' col brain `run_subagent`,
        // che parte da messages=[Human(task)]).
        conversation_history: Vec::new(),
        tools_json,
        // Sub-agente auto-approvato e DIRETTO: nessun intent_hint, nessun giudizio
        // del classifier (il task e' gia' descritto). Il RouterNode decide.
        intent_hint: None,
        requires_tools: None,
        agentic_score: None,
        authorizes_changes: None,
        classifier_resolved: false,
        action_oriented_min_score: crate::intent_classifier::DEFAULT_ACTION_ORIENTED_MIN_SCORE,
        // Sub-agente eseguito in autonomia (parita' col brain: behavior_mode
        // "automatico", approved=true).
        automation_mode: "automatic".to_string(),
        supervisor_mode: nexus_agent_graph::SupervisorMode::None,
        step_tx: sub_tx,
        parent_run_id: Some(anchor),
        subagent_depth: Some(current_depth),
        sizing_complexity: None,
        sizing_scope_system_wide: false,
        classifier_intent: None,
        // Il tetto REALE di questa figura (lo stesso del `tokio::time::timeout`
        // esterno qui sotto) entra nel motore: senza, il gate a tempo dell'executor
        // userebbe il setting globale `agent.run_time_budget_s` (0 per policy) e
        // resterebbe inerte, lasciando la figura morire muta allo scadere.
        run_time_budget_s: Some(timeout_s.max(0) as u64),
        // FASE 2: override root del sub-run. `None` (ramo sequenziale/condiviso o
        // background) -> scrive sulla root del progetto, comportamento invariato.
        // `Some(worktree)` (ramo ISOLATO, valorizzato dal batch parallelo di
        // `tool_dispatch_subagents`) -> scrive nel worktree effimero, ctx isolato
        // (autocommit/reindex soppressi, PR3). L'apply serializzato e' del batch.
        working_root,
        // Lo scope dichiarato per il passo arriva fin dentro il ctx dei tool del
        // sub-run: e' l'anello senza il quale il confronto non scatterebbe MAI e la
        // misura si riempirebbe di "non dichiarato" facendo sembrare preciso un
        // pianificatore che non e' stato misurato.
        write_scope,
        pre_run_advisory_synthesis: None,
        pre_run_advisory_source: None,
        // Un SUB-RUN non ha barriera: i panel a monte sono del coordinatore, non
        // suoi. Un figlio che attendesse il consiglio del padre sarebbe un'attesa
        // circolare (il padre sta aspettando lui).
        advisory_gate: None,
    };

    // Timeout duro sull'esecuzione del sub-run (parita' col brain `asyncio.wait_for`).
    // In OGNI ramo il ponte e' fermato e ATTESO (`stop_bridge`) prima del meta-step
    // di chiusura: senza l'await, abort() e' cooperativo (cancella al prossimo poll)
    // e un progress/heartbeat gia' dentro persist_meta_step potrebbe ottenere un
    // NOW() > del completed -> ordine invertito nella timeline storica (ordinata
    // per created_at). L'await del handle garantisce che nessuna INSERT del ponte
    // segua quella di chiusura (race chiusa alla radice, regola H).
    let run_fut = crate::native_engine::run_native(&deps, &native_input);
    // L'istante da cui il budget decorre, in Rust. Serve al ramo di scadenza:
    // `completed_at` sulla riga del sub-run e' il NOW() del server Postgres,
    // scritto DOPO stop_bridge + fetch_ledger_totals + mark_run, cioe' misura la
    // CHIUSURA e non lo scatto. Finche' i due erano indistinguibili, un ritardo
    // nella scrittura (pool esaurito) somigliava a un timer che scatta tardi:
    // due cause opposte con la stessa faccia.
    let t_budget_start = std::time::Instant::now();
    let outcome: anyhow::Result<NativeRunOutcome> =
        match tokio::time::timeout(std::time::Duration::from_secs(timeout_s as u64), run_fut).await
        {
            Ok(res) => res,
            Err(_) => {
                // Misurato PRIMA di ogni await successivo: da qui in poi qualunque
                // attesa (il ponte, il ledger, una connessione dal pool) sposta
                // solo la scrittura, non lo scatto.
                let scatto_ms = t_budget_start.elapsed().as_millis() as u64;
                let ritardo_ms = ritardo_scatto_ms(scatto_ms, timeout_s);
                tracing::warn!(
                    sub_run_id = %subagent_run_id,
                    kind = %kind,
                    timeout_s,
                    scatto_ms,
                    ritardo_ms,
                    "sub-run scaduto: ritardo dello scatto rispetto al budget"
                );
                stop_bridge(bridge).await;
                // Il timeout marca comunque la riga TERMINALE (status='timeout'):
                // deve passare per l'enqueue fan-in come gli altri rami (senza,
                // un parent con l'ultimo figlio andato in timeout resterebbe
                // sospeso per sempre). Non esce con return diretto.
                let out = finalize_timeout(
                    &proj_pool,
                    &ctx.core.db,
                    narrator.as_deref(),
                    subagent_run_id,
                    &kind,
                    timeout_s,
                    &provider_resolved,
                    &model_resolved,
                )
                .await;
                maybe_enqueue_fanin_resume(
                    &ctx,
                    &proj_pool,
                    anchor,
                    fanin_parent_run_id,
                    session_id,
                    is_background,
                )
                .await;
                return out;
            }
        };
    stop_bridge(bridge).await;

    let out = match outcome {
        Ok(mut o) => {
            // Costo/token del sub-run INTERO dal ledger (punto unico), non
            // dell'ultimo turno: intercetto qui, cosi' mark_run, la narrazione e
            // il tool_result al padre vedono tutti lo stesso numero onesto.
            adopt_ledger_totals(&ctx.core.db, subagent_run_id, &mut o).await;
            finalize_success(
                &proj_pool,
                narrator.as_deref(),
                subagent_run_id,
                &kind,
                current_depth,
                &o,
            )
            .await
        }
        Err(e) => {
            finalize_failure(
                &proj_pool,
                &ctx.core.db,
                narrator.as_deref(),
                subagent_run_id,
                &kind,
                &e,
                &provider_resolved,
                &model_resolved,
            )
            .await
        }
    };
    // Fan-in (Fase D): il figlio si e' marcato TERMINALE nel finalize sopra. Se e'
    // il ramo background e nessun altro figlio background del parent e' ancora
    // attivo, accoda il parent nella coda META (il worker fan-in lo riprende).
    maybe_enqueue_fanin_resume(
        &ctx,
        &proj_pool,
        anchor,
        fanin_parent_run_id,
        session_id,
        is_background,
    )
    .await;
    out
}

/// Enqueue idempotente del parent nella coda fan-in META
/// (`subagent_fanin_resume_queue`) al completamento di un figlio BACKGROUND, SOLO
/// se e' l'ULTIMO background terminale del suo parent (Fase D Slice 3).
///
/// Race-free by design: il figlio si e' gia' marcato terminale (mark_run nel
/// finalize precede questa chiamata), quindi la COUNT dei background ancora
/// attivi (`running`/`paused`) NON conta se stesso. Se 0 -> tutti i background
/// del parent hanno finito -> accoda il parent. L'INSERT e' idempotente (PK
/// `parent_run_id`, `ON CONFLICT DO NOTHING`): se piu' figli chiudono in
/// concorrenza e vedono 0 rimasti, la coda ha comunque UNA sola riga.
///
/// Best-effort (regola: non deve far fallire il figlio): ogni errore e' loggato
/// WARN e ignorato. La coda vive nel META (`ctx.core.db`); i background del parent
/// si contano sul PROJECT pool (`proj_pool`, dove vive `nexus_subagent_runs`).
async fn maybe_enqueue_fanin_resume(
    ctx: &AgentToolContext,
    proj_pool: &sqlx::PgPool,
    anchor: Uuid,
    resume_run_id: Uuid,
    session_id: Uuid,
    is_background: bool,
) {
    if !is_background {
        return;
    }
    // Deriva project_id e coda META dal ctx; la meccanica DB (COUNT + INSERT) e'
    // nel punto unico testabile `fanin_enqueue_if_last`. `resume_run_id` e' il run
    // CORRENTE (= dispatcher_run_id dei figli): la COUNT filtra sui SUOI figli
    // diretti e la coda punta a lui (quello che il CAS del worker riprende).
    if let Err(e) = fanin_enqueue_if_last(
        &ctx.core.db,
        proj_pool,
        resume_run_id,
        ctx.core.project_id,
        session_id,
    )
    .await
    {
        tracing::warn!(
            target: "mcp_core::subagent_native",
            parent_run_id = %resume_run_id,
            anchor = %anchor,
            error = %e,
            "fan-in: enqueue del parent fallito (best-effort, il worker ritentera' al prossimo figlio)"
        );
    }
}

/// Se e' l'ULTIMO figlio background DIRETTO di `resume_run_id` a terminare,
/// accoda quel run nella coda META `subagent_fanin_resume_queue` (idempotente).
/// Punto unico testabile della meccanica DB del fan-in (regola L): COUNT background
/// non-terminali con `dispatcher_run_id = resume_run_id` sul PROJECT pool + INSERT
/// ON CONFLICT sulla coda META. Ritorna `Ok(true)` se ha accodato (o la riga
/// esisteva gia'), `Ok(false)` se altri background diretti sono ancora attivi. Il
/// chiamante e' gia' terminale (mark_run eseguito prima), per cui la COUNT non
/// conta se stesso: 0 rimasti -> tutti i figli DIRETTI finiti (i nipoti annidati
/// hanno un dispatcher diverso e NON entrano nella COUNT: ALTA 1).
async fn fanin_enqueue_if_last(
    meta: &sqlx::PgPool,
    proj_pool: &sqlx::PgPool,
    resume_run_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
) -> Result<bool, sqlx::Error> {
    // "Tutti i figli DIRETTI di QUESTO run terminali?" — segnale strutturato
    // (status lifecycle, regola M): 0 in stato non-terminale ('running'/'paused')
    // = tutti hanno finito. La COUNT filtra su `dispatcher_run_id = resume_run_id`
    // (i figli dispatchati DA QUESTO run), NON su parent_run_id = anchor (che
    // degenera in session_id e includerebbe nipoti annidati di altri figli:
    // ALTA 1, mig project 0010). `resume_run_id` = ctx.core.run_id = il dispatcher.
    let remaining: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM nexus_subagent_runs \
         WHERE dispatcher_run_id = $1 AND is_background = true \
           AND status IN ('running', 'paused')",
    )
    .bind(resume_run_id)
    .fetch_one(proj_pool)
    .await?;
    if remaining > 0 {
        return Ok(false); // altri background ancora attivi: aspetta l'ultimo
    }
    // Ultimo background terminale: accoda il RUN CORRENTE (idempotente via PK).
    // `resume_run_id` (non `anchor`) e' l'id marcato `awaiting_subagents` che il
    // CAS del worker cerca in agent_runs: e' quello da riprendere.
    let res = sqlx::query(
        "INSERT INTO subagent_fanin_resume_queue (parent_run_id, project_id, session_id) \
         VALUES ($1, $2, $3) ON CONFLICT (parent_run_id) DO NOTHING",
    )
    .bind(resume_run_id)
    .bind(project_id)
    .bind(session_id)
    .execute(meta)
    .await?;
    tracing::info!(
        target: "mcp_core::subagent_native",
        parent_run_id = %resume_run_id,
        inserted = res.rows_affected() > 0,
        "fan-in: ultimo sub-run background diretto terminato, dispatcher accodato alla coda di resume"
    );
    Ok(true)
}

/// Modello del worker dal `model_purpose` della definition (regola G,
/// tier-aware). Non risolto -> nessun override: l'executor usa il routing di
/// default (parita' col brain).
///
/// VINCOLO GIUDICE != WORKER (Fase C2): per un sub-run di `kind == "review"` la
/// risoluzione ESCLUDE il provider del run PADRE (il worker che ha prodotto il
/// lavoro), cosi' la verifica avversaria gira su un provider diverso (indipendenza
/// reale). Se l'esclusione svuota il pool (unico provider capable) si ripiega
/// SENZA esclusione con un WARN: il vincolo e' una preferenza forte, non un hard
/// filter — meglio un review sullo stesso provider che nessun review.
///
/// PIN DEL PROVIDER (propagazione all'utente): per i kind WORKER (NON review) se
/// l'utente ha pinnato un provider sulla chat (`chat_sessions.preferred_provider`)
/// la risoluzione del purpose viene RISTRETTA a quel provider (preferenza-forte
/// tier-aware: tier + capability + tool_use invariati, solo il provider e'
/// vincolato — regola L: unica sorgente del vincolo, NON si propaga il
/// preferred_model, il modello si deriva sempre dal tier via catalog, regola G).
/// Se il provider pinnato non offre un modello capable del tier (o e' in cooldown)
/// si RIPIEGA sulla risoluzione SENZA pin, con un WARN strutturato: il figlio non
/// resta bloccato (preferenza forte, non hard filter). Per `kind == "review"` il
/// pin e' IGNORATO: il vincolo giudice != worker vince (indipendenza avversaria).
///
/// SEPARAZIONE DB: `db` e' il pool META (purpose-resolution via catalog +
/// `parent_provider` da `agent_runs`); `proj_pool` e' il pool del PROGETTO, dove
/// vive `chat_sessions` (il pin). Le due letture NON possono condividere lo stesso
/// pool (incidente separazione DB: `chat_sessions` non e' nel meta).
/// Setting kill-switch diversita' tier ultracode (mig 0555, regola G).
const ULTRACODE_TIER_DIVERSITY_SETTING: &str = "orchestrator.ultracode_tier_diversity_enabled";

/// Risolve il purpose model per un sub-agent ultracode leggendo i settings
/// `orchestrator.ultracode_*_purpose` con fallback a
/// `nexus_subagent_definitions.model_purpose` (regola G/L).
async fn resolve_worker_model_purpose(
    db: &sqlx::PgPool,
    kind: &str,
    definition: &SubagentDefinition,
) -> String {
    let tier_diversity = nexus_auth::get_bool_setting(db, ULTRACODE_TIER_DIVERSITY_SETTING)
        .await
        .ok()
        .flatten()
        .unwrap_or(true);
    if !tier_diversity {
        return definition.model_purpose.clone();
    }
    let setting_key = match kind {
        "implement" => Some("orchestrator.ultracode_implement_purpose"),
        "verify" => Some("orchestrator.ultracode_verify_purpose"),
        "review" => Some("orchestrator.ultracode_review_purpose"),
        _ => None,
    };
    if let Some(key) = setting_key {
        if let Some(v) = nexus_auth::get_setting(db, key).await {
            let trimmed = v.trim().to_string();
            if !trimmed.is_empty() {
                return trimmed;
            }
        }
    }
    definition.model_purpose.clone()
}

async fn resolve_worker_model(
    db: &sqlx::PgPool,
    proj_pool: &sqlx::PgPool,
    kind: &str,
    definition: &SubagentDefinition,
    anchor: Uuid,
    session_id: Uuid,
) -> (String, String) {
    let purpose = resolve_worker_model_purpose(db, kind, definition).await;
    if purpose.trim().is_empty() {
        return (String::new(), String::new());
    }
    let purpose = purpose.as_str();

    // Ramo REVIEW: preferenza IGNORATA, vincolo giudice != worker invariato.
    // Provider del padre da escludere (astensione da auto-certificazione).
    if e_un_giudice(kind) {
        // Stessa regola che vieta il ripiego a valle, letta dallo stesso punto:
        // se qui e la porta di escalation la calcolassero per conto proprio,
        // basterebbe un domani a farle divergere — ed e' la divergenza che ha
        // prodotto il difetto (selezione rispettata, failover no).
        let exclude: Vec<String> = veto_del_giudice(db, kind, anchor)
            .await
            .provider()
            .map(str::to_string)
            .into_iter()
            .collect();
        return resolve_model_excluding(db, purpose, kind, &exclude).await;
    }

    // Ramo WORKER: il fornitore che la SESSIONE ricorda restringe la risoluzione
    // del purpose (preferenza-forte tier-aware), e degrada se dentro non c'e' un
    // modello adatto.
    //
    // E' una PREFERENZA, non il vincolo duro del composer: quello vale per la
    // richiesta in cui l'utente lo da' e si ferma al run principale
    // (`NativeRunInput::provider_pin`, valorizzato `none` per i sub-run). La
    // distinzione conta perche' qui il degrado e' la regola — un figlio che non
    // trova modello nel fornitore preferito riparte altrove — mentre un vincolo
    // duro, per definizione, non degrada: ereditarlo qui fermerebbe i figli
    // invece di spostarli.
    if let Some(pinned) = session_preferred_provider(proj_pool, session_id).await {
        match crate::internal_routing::resolve_purpose_model_db_pinned(db, purpose, Some(&pinned))
            .await
        {
            crate::internal_routing::PurposeResolution::Resolved {
                provider, model, ..
            } => return (provider, model),
            // NoCapableModel/NotFound col pin: il provider pinnato non offre un
            // modello capable del tier (o e' in cooldown). FALLBACK senza pin
            // (regola H fail-loud: il figlio non resta bloccato). WARN strutturato
            // (regola M: la decisione e' sull'enum PurposeResolution, il testo e'
            // solo display).
            other => {
                tracing::warn!(
                    kind = %kind,
                    purpose = %purpose,
                    pinned = %pinned,
                    resolution = ?other,
                    "subagent_native: pin provider non risolvibile per il tier del purpose, \
                     fallback senza pin"
                );
            }
        }
    }

    // Nessun pin (o pin degradato): risoluzione worker standard (parita' col
    // comportamento pre-pin).
    match crate::internal_routing::resolve_purpose_model_db(db, purpose).await {
        crate::internal_routing::PurposeResolution::Resolved {
            provider, model, ..
        } => (provider, model),
        other => {
            tracing::warn!(
                kind = %kind,
                model_purpose = %purpose,
                resolution = ?other,
                "subagent_native: model_purpose non risolto, routing di default"
            );
            (String::new(), String::new())
        }
    }
}

/// Risoluzione del modello di un sub-run con ESCLUSIONE di provider, e fallback
/// SENZA esclusione + WARN se il pool si svuota (unico provider capable).
///
/// Punto unico (regola L) per due vincoli della stessa natura:
///   - review: giudice != worker (esclude il provider del padre);
///   - consiglio: figure su provider DISTINTI (esclude i provider gia'
///     assegnati alle figure precedenti, vedi `resolve_council_assignments`).
/// In entrambi i casi l'esclusione e' una PREFERENZA forte, non un vincolo che
/// lascia il posto vuoto: meglio un duplicato dichiarato (WARN) che una figura
/// in meno.
async fn resolve_model_excluding(
    db: &sqlx::PgPool,
    purpose: &str,
    kind: &str,
    exclude: &[String],
) -> (String, String) {
    match crate::internal_routing::resolve_purpose_model_db_excluding(db, purpose, exclude).await {
        crate::internal_routing::PurposeResolution::Resolved {
            provider, model, ..
        } => (provider, model),
        other if !exclude.is_empty() => {
            // L'esclusione del provider del worker ha svuotato il pool: fallback
            // SENZA esclusione (il review gira comunque, vedi doc). WARN esplicito.
            tracing::warn!(
                kind = %kind, model_purpose = %purpose, excluded = ?exclude,
                resolution = ?other,
                "subagent_native: giudice != worker impossibile (unico provider capable), \
                 review sullo stesso provider del worker"
            );
            match crate::internal_routing::resolve_purpose_model_db(db, purpose).await {
                crate::internal_routing::PurposeResolution::Resolved {
                    provider, model, ..
                } => (provider, model),
                _ => (String::new(), String::new()),
            }
        }
        other => {
            tracing::warn!(
                kind = %kind,
                model_purpose = %purpose,
                resolution = ?other,
                "subagent_native: model_purpose non risolto, routing di default"
            );
            (String::new(), String::new())
        }
    }
}

/// Provider del run PADRE (`agent_runs.provider`) per il vincolo giudice != worker
/// (Fase C2). `None` se l'anchor non e' un run tracciato (sessione senza riga in
/// `agent_runs`) o il provider e' vuoto: in tal caso nessuna esclusione.
/// Il fornitore che un sub-run NON puo' usare: punto unico (regola L) della
/// regola «giudice != worker».
///
/// Esiste perche' la regola aveva DUE vite separate e una sola la applicava. La
/// selezione del modello escludeva il fornitore del padre
/// (`resolve_model_excluding`), ma il ripiego a valle no: `failover_provider`
/// riceve i fornitori «gia' tentati in questo turno», e quello del worker non e'
/// mai fra loro. Misurato il 26/07/2026 (run 609000c1): 10 revisori scelti su
/// openrouter, le loro trace su `deepseek-v4-flash` e `deepseek-v4-pro`, cioe' il
/// fornitore del padre. Il vincolo reggeva alla selezione e cadeva al failover.
///
/// Ora la regola si esprime in un posto e la interrogano entrambi i consumatori:
/// la selezione per non scegliere quel fornitore, la porta di escalation
/// ([`crate::orchestrator::ProviderVeto`]) per non ripiegarci. Vale solo per
/// `review`: un worker o una figura non hanno nulla da cui astenersi.
async fn veto_del_giudice(
    db: &sqlx::PgPool,
    kind: &str,
    anchor: Uuid,
) -> crate::orchestrator::ProviderVeto {
    if !e_un_giudice(kind) {
        return crate::orchestrator::ProviderVeto::none();
    }
    // Padre senza fornitore noto: nessun veto. Non e' un ripiego silenzioso —
    // vietare un nome vuoto escluderebbe TUTTI i candidati (o nessuno, a seconda
    // del confronto), e in entrambi i casi il motivo non si leggerebbe da nessuna
    // parte.
    parent_provider(db, anchor)
        .await
        .map_or_else(crate::orchestrator::ProviderVeto::none, |p| {
            crate::orchestrator::ProviderVeto::su(&p)
        })
}

async fn parent_provider(db: &sqlx::PgPool, anchor: Uuid) -> Option<String> {
    let provider: Option<String> =
        sqlx::query_scalar("SELECT provider FROM agent_runs WHERE id = $1")
            .bind(anchor)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    provider.filter(|p| !p.trim().is_empty())
}

/// Provider PINNATO dall'utente sulla chat (`chat_sessions.preferred_provider`).
/// DISTINTA da [`parent_provider`]: legge dal pool PROGETTO (`chat_sessions` vive
/// nel DB del progetto, NON nel meta — incidente separazione DB), non da
/// `agent_runs`. `None` se la sessione non ha pin, il valore e' NULL/vuoto, o la
/// query fallisce (fail-open: nessun pin -> routing worker standard). Il pin e'
/// SOLO il provider: il modello si deriva sempre dal tier via catalog (regola G).
/// Classifica l'esito di ogni figura e lo emette sul canale di progresso UI
/// appena disponibile (estratto da `convene_council`, comportamento invariato).
async fn classify_and_stream_reports(
    kinds: &[String],
    results: Vec<Value>,
    progress_tx: Option<&tokio::sync::mpsc::Sender<FigureAdvisoryReport>>,
) -> Vec<(FigureAdvisoryReport, Value)> {
    let mut pairs: Vec<(FigureAdvisoryReport, Value)> = Vec::with_capacity(kinds.len());
    for (kind, result) in kinds.iter().zip(results.into_iter()) {
        let report = classify_council_figure_result(kind, &result);
        if let Some(tx) = progress_tx {
            let _ = tx.send(report.clone()).await;
        }
        pairs.push((report, result));
    }
    pairs
}

/// Risolve il pool del progetto e delega a [`resolve_council_assignments`]
/// (che resta parametrica sui pool per i test, regola O).
async fn council_assignments(
    ctx: &AgentToolContext,
    kinds: &[String],
) -> Vec<Option<(String, String)>> {
    let proj_pool = match crate::project_db_routes::project_data_pool_from(
        &ctx.core.db,
        ctx.core.project_id,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            // Pre-assegnazione best-effort: senza DB progetto non si legge il
            // pin di sessione -> nessuna pre-assegnazione, ogni figura segue il
            // percorso storico (None), che produce i suoi rifiuti strutturati.
            tracing::warn!(
                project_id = %ctx.core.project_id,
                error = %e,
                "council_assignments: DB progetto non disponibile, nessuna pre-assegnazione"
            );
            return vec![None; kinds.len()];
        }
    };
    resolve_council_assignments(&ctx.core.db, &proj_pool, ctx.core.session_id, kinds).await
}

/// Esegue UNA figura del Consiglio: pinnata sul provider pre-assegnato
/// ([`resolve_council_assignments`]) quando c'e', percorso storico altrimenti.
async fn run_council_figure(
    ctx: &AgentToolContext,
    kind: &str,
    task: &str,
    expected: &str,
    pin: Option<(String, String)>,
) -> Value {
    match pin {
        Some((provider, model)) => {
            run_single_subagent_with_model_pin(ctx, kind, task, "", expected, &provider, &model)
                .await
        }
        None => run_single_subagent(ctx, kind, task, "", expected, None, false, &[]).await,
    }
}

/// Pre-assegna alle figure del Consiglio provider DISTINTI (preferenza forte).
///
/// Difetto misurato il 20/07: `software_architect` e `security_engineer`
/// (stesso purpose tier) hanno ricevuto lo STESSO provider e lo STESSO modello
/// (openrouter/qwen3-235b), perche' ogni figura risolveva il proprio modello in
/// parallelo e in isolamento -- il piu' economico eleggibile e' uguale per
/// tutte. Due pareri dello stesso modello non sono due pareri: la diversita'
/// che il Consiglio promette va decisa PRIMA del fan-out, quando l'assegnazione
/// e' ancora sequenziale.
///
/// Regole:
///   - ogni figura risolve col SUO purpose (nessun purpose condiviso imposto),
///     escludendo i provider gia' assegnati alle figure precedenti;
///   - pool esaurito -> la figura tiene il provider duplicato (WARN dentro
///     `resolve_model_excluding`): meglio un parere in piu' che una figura in
///     meno;
///   - pin di sessione presente -> nessuna pre-assegnazione (il pin si propaga
///     ai subagenti per scelta deliberata; la diversita' non si applica);
///   - ogni esito non risolvibile -> `None`: la figura segue il percorso
///     storico (`resolve_worker_model` dentro il prepare), che produce i suoi
///     rifiuti strutturati.
async fn resolve_council_assignments(
    db: &sqlx::PgPool,
    proj_pool: &sqlx::PgPool,
    session_id: Option<Uuid>,
    kinds: &[String],
) -> Vec<Option<(String, String)>> {
    let nessuna: Vec<Option<(String, String)>> = vec![None; kinds.len()];
    let Some(session_id) = session_id else {
        return nessuna;
    };
    if session_preferred_provider(proj_pool, session_id).await.is_some() {
        return nessuna;
    }
    let mut exclude: Vec<String> = Vec::new();
    let mut out: Vec<Option<(String, String)>> = Vec::with_capacity(kinds.len());
    for kind in kinds {
        let Ok(Some(definition)) = fetch_definition(db, kind).await else {
            out.push(None);
            continue;
        };
        let purpose = resolve_worker_model_purpose(db, kind, &definition).await;
        if purpose.trim().is_empty() {
            out.push(None);
            continue;
        }
        let (provider, model) = resolve_model_excluding(db, &purpose, kind, &exclude).await;
        if provider.is_empty() || model.is_empty() {
            out.push(None);
            continue;
        }
        let pl = provider.to_lowercase();
        if !exclude.contains(&pl) {
            exclude.push(pl);
        }
        out.push(Some((provider, model)));
    }
    out
}

/// Il fornitore che la SESSIONE ricorda (`chat_sessions.preferred_provider`,
/// scritto dal solo cambio del dropdown). Si chiamava `session_pinned_provider`:
/// il nome prometteva un vincolo dove c'e' un ricordo, ed e' la stessa confusione
/// fra "scelto" e "imposto" che ha tenuto il pulsante "Forza" senza effetto per
/// mesi. Un pin non si legge mai da qui — non si eredita da una sessione.
async fn session_preferred_provider(proj_pool: &sqlx::PgPool, session_id: Uuid) -> Option<String> {
    let preferito: Option<String> =
        sqlx::query_scalar("SELECT preferred_provider FROM chat_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(proj_pool)
            .await
            .ok()
            .flatten();
    preferito.filter(|p| !p.trim().is_empty())
}

/// Campi comuni della riga `nexus_subagent_runs` (i rami isolato/sequenziale
/// differiscono solo per id/worktree, portati da `IsolationSlot`).
struct NewSubagentRun<'a> {
    anchor: Uuid,
    /// Run CORRENTE che dispatcha questo figlio (`ctx.core.run_id` via
    /// [`fanin_target_run_id`]): isola i figli DIRETTI di un run per COUNT/fetch
    /// del fan-in (mig project 0010). Distinto da `anchor` (depth-chain di
    /// famiglia): l'anchor degenera in session_id, il dispatcher no.
    dispatcher_run_id: Uuid,
    project_id: Uuid,
    kind: &'a str,
    task: &'a str,
    context_blob: &'a str,
    expected_format: &'a str,
    depth: i32,
    /// Dispatch asincrono opt-in (Fase D fan-in): il padre NON attende il sub-run,
    /// si sospende e riprende al fan-in. `false` = comportamento bloccante storico.
    is_background: bool,
}

/// INSERT della riga sub-run, query UNICA per i due rami. Ramo ISOLATO
/// (`slot` presente): id = run_id del worktree (allineamento row DB <->
/// dir/branch) + persistenza worktree_path/base_commit per audit/reconcile.
/// Ramo sequenziale: id generato dal DB (`COALESCE(NULL, gen_random_uuid())` =
/// stesso DEFAULT della tabella, mig 0151) e colonne worktree NULL (parita'
/// con l'INSERT storico che non le elencava, mig project 0005 senza default).
async fn insert_subagent_run(
    pool: &sqlx::PgPool,
    row: &NewSubagentRun<'_>,
    slot: Option<&IsolationSlot>,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO nexus_subagent_runs
           (id, parent_run_id, project_id, kind, task_description, context_blob, expected_format,
            status, is_background, depth, source, worktree_path, base_commit, dispatcher_run_id)
           VALUES (COALESCE($1, gen_random_uuid()), $2, $3, $4, $5, $6, $7,
                   'running', $9, $8, 'db', $10, $11, $12)
           RETURNING id"#,
    )
    .bind(slot.map(|s| s.run_id))
    .bind(row.anchor)
    .bind(row.project_id)
    .bind(row.kind)
    .bind(row.task)
    .bind(row.context_blob)
    .bind(row.expected_format)
    .bind(row.depth)
    .bind(row.is_background)
    .bind(slot.map(|s| s.worktree_path.to_string_lossy().to_string()))
    .bind(slot.map(|s| s.base_commit.as_str()))
    .bind(row.dispatcher_run_id)
    .fetch_one(pool)
    .await
}

/// Ferma il ponte narrazione e ATTENDE che sia davvero finito (vedi commento
/// sul timeout in `run_single_subagent`: chiude la race abort/INSERT-in-volo).
/// Quanto lo scatto del timeout ha ecceduto il budget, in millisecondi.
///
/// Un valore vicino a zero dice che il timer e' stato puntuale e che un eventuale
/// ritardo osservato a valle (su `completed_at`) sta nella scrittura, non nello
/// scatto. Un valore grande dice il contrario: sono due diagnosi opposte, e senza
/// questa misura si distinguono solo per congettura.
fn ritardo_scatto_ms(scatto_ms: u64, timeout_s: i64) -> u64 {
    let budget_ms = (timeout_s.max(0) as u64).saturating_mul(1_000);
    scatto_ms.saturating_sub(budget_ms)
}

async fn stop_bridge(bridge: Option<tokio::task::JoinHandle<()>>) {
    if let Some(b) = bridge {
        b.abort();
        let _ = b.await;
    }
}

/// Chiusura del sub-run in TIMEOUT: mark_run + narrazione + tool_result.
async fn finalize_timeout(
    pool: &sqlx::PgPool,
    meta_db: &sqlx::PgPool,
    narrator: Option<&ParentNarrator>,
    sub_run_id: Uuid,
    kind: &str,
    timeout_s: i64,
    provider: &str,
    model: &str,
) -> Value {
    let verdict = terminal_verdict("timed_out", "timeout");
    // Un timeout non azzera la spesa gia' fatturata: la prendo dal ledger (META).
    let ledger = crate::chat_messages::fetch_ledger_totals(meta_db, sub_run_id).await;
    let _ = mark_run(
        pool,
        sub_run_id,
        SubRunClosure::from_ledger("timeout", "[Sub-agent timeout]", verdict.clone(), &ledger),
    )
    .await;
    if let Some(n) = narrator {
        n.say(
            "subagent_failed",
            format!("Subagente {kind} in timeout dopo {timeout_s}s"),
            n.with_pin(json!({
                K_SUB_RUN_ID: sub_run_id.to_string(),
                K_SUB_KIND: kind,
                "status": "timeout",
                K_TIMEOUT_S: timeout_s,
            })),
            sub_run_id,
        )
        .await;
    }
    let mut out = json!({
        K_SUB_RUN_ID: sub_run_id.to_string(),
        "kind": kind,
        "status": "timeout",
        "error": "[Sub-agent timeout]",
        // Verdetto ESITO strutturato (regola M): il coordinatore legge
        // success/verdict qui, mai dalla prosa di `error`.
        "outcome": verdict,
    });
    put_model_fields(&mut out, provider, model);
    out
}

/// Narrazione (meta-step live sul run PADRE) della chiusura OK del sub-run.
/// Estratta da `finalize_success` (regola L / lunghezza): no-op se `narrator`
/// e' `None` (kill-switch UX `subagent_narration_enabled`).
async fn narrate_completed(
    narrator: Option<&ParentNarrator>,
    sub_run_id: Uuid,
    kind: &str,
    status: &str,
    summary: &str,
    o: &NativeRunOutcome,
) {
    let Some(n) = narrator else { return };
    let esito = if o.completed {
        "completato"
    } else {
        "in pausa"
    };
    n.say(
        "subagent_completed",
        format!("Subagente {kind} {esito} ({} iterazioni)", o.iterations),
        n.with_pin(json!({
            K_SUB_RUN_ID: sub_run_id.to_string(),
            K_SUB_KIND: kind,
            "status": status,
            K_SUMMARY: compact_summary(summary),
            "iterations": o.iterations,
            "cost_usd": o.total_cost,
        })),
        sub_run_id,
    )
    .await;
}

/// Chiusura del ramo OK del sub-run: mark_run + log + narrazione + tool_result
/// (il main riceve SOLO questo summary, non l'intera conversazione del figlio).
async fn finalize_success(
    pool: &sqlx::PgPool,
    narrator: Option<&ParentNarrator>,
    sub_run_id: Uuid,
    kind: &str,
    depth: i64,
    o: &NativeRunOutcome,
) -> Value {
    let summary = o.final_answer.clone().unwrap_or_default();
    // `status` LIFECYCLE del sub-run (completed = arrivato a End, paused = fermato
    // su interrupt HITL). Backward-compat: todo_runner e il batch lo leggono come
    // {completed|paused}; NON va sostituito col verdetto canonico (regredirebbe i
    // completed_unverified). Il VERDETTO del lavoro (segnali strutturati, regola M)
    // viaggia separato nel blocco `outcome`, senza toccare il lifecycle.
    let status = if o.completed { "completed" } else { "paused" };
    let verdict = o.structured_verdict();
    let _ = mark_run(
        pool,
        sub_run_id,
        SubRunClosure {
            status,
            summary: &summary,
            iterations: o.iterations,
            tokens_prompt: o.prompt_tokens,
            tokens_completion: o.completion_tokens,
            cost_usd: o.total_cost,
            verdict: verdict.clone(),
        },
    )
    .await;
    tracing::info!(
        kind = %kind,
        subagent_run_id = %sub_run_id,
        depth,
        completed = o.completed,
        iterations = o.iterations,
        summary_len = summary.len(),
        "subagent_native: sub-run eseguito sul grafo nativo"
    );
    narrate_completed(narrator, sub_run_id, kind, status, &summary, o).await;
    let mut out = json!({
        K_SUB_RUN_ID: sub_run_id.to_string(),
        "kind": kind,
        "status": status,
        K_SUMMARY: compact_summary(&summary),
        "iterations": o.iterations,
        "cost_usd": o.total_cost,
        "tokens": { "prompt": o.prompt_tokens, "completion": o.completion_tokens },
        // Verdetto ESITO strutturato (regola M): il coordinatore legge
        // success/verdict qui invece di dedurre l'esito dalla prosa di `summary`.
        "outcome": verdict,
    });
    // Provenienza EFFETTIVA (regola M): provider/model realmente usati dal grafo
    // (post-eventuale failover), non solo il risolto a monte. Letta dal report di
    // figura (classify_council_figure_result) per mostrarla in UI.
    put_model_fields(
        &mut out,
        o.provider_used.as_deref().unwrap_or_default(),
        o.model_used.as_deref().unwrap_or_default(),
    );
    out
}

/// Chiusura del ramo ERRORE del sub-run (fallback onesto: errore al chiamante).
async fn finalize_failure(
    pool: &sqlx::PgPool,
    meta_db: &sqlx::PgPool,
    narrator: Option<&ParentNarrator>,
    sub_run_id: Uuid,
    kind: &str,
    e: &anyhow::Error,
    provider: &str,
    model: &str,
) -> Value {
    let msg = format!("\u{274C} [Errore grafo nativo: {e}]");
    let verdict = terminal_verdict("failed", "engine_error");
    // Come per il timeout: un errore del grafo non cancella le chiamate gia'
    // fatturate prima del guasto.
    let ledger = crate::chat_messages::fetch_ledger_totals(meta_db, sub_run_id).await;
    let _ = mark_run(
        pool,
        sub_run_id,
        SubRunClosure::from_ledger("failed", &msg, verdict.clone(), &ledger),
    )
    .await;
    tracing::warn!(
        kind = %kind,
        subagent_run_id = %sub_run_id,
        error = %e,
        "subagent_native: sub-run fallito"
    );
    if let Some(n) = narrator {
        n.say(
            "subagent_failed",
            format!("Subagente {kind} fallito"),
            n.with_pin(json!({
                K_SUB_RUN_ID: sub_run_id.to_string(),
                K_SUB_KIND: kind,
                "status": "failed",
                "error": compact_summary(&msg),
            })),
            sub_run_id,
        )
        .await;
    }
    let mut out = json!({
        "error": msg,
        K_SUB_RUN_ID: sub_run_id.to_string(),
        "kind": kind,
        // Verdetto ESITO strutturato (regola M): mai dedurre il fallimento
        // dalla prosa di `error`.
        "outcome": verdict,
    });
    put_model_fields(&mut out, provider, model);
    out
}

/// Crea la riga `agent_runs` del SUB-run (run TRACCIATO: senza, il guard
/// "untracked_run" di `PgAgentStepStore` scarta in silenzio ogni step del
/// figlio). `nexus_agent_type='subagent'` discrimina i sub-run nelle query UI
/// top-level; `parent_run_id` e' impostato via subquery SOLO se l'ancora e' un
/// run tracciato (FK verso agent_runs: l'ancora puo' essere la sessione).
/// Best-effort ma LOGGATO forte: se fallisce, gli step del figlio non verranno
/// persistiti (la stessa cecita' dell'incidente 2026-07-06).
#[allow(clippy::too_many_arguments)]
async fn ensure_child_agent_run(
    db: &sqlx::PgPool,
    run_id: Uuid,
    session_id: Uuid,
    project_id: Uuid,
    user_id: Uuid,
    anchor: Uuid,
    provider: &str,
    model: &str,
) {
    let provider = (!provider.trim().is_empty()).then_some(provider);
    let model = (!model.trim().is_empty()).then_some(model);
    let res = sqlx::query(
        r#"INSERT INTO agent_runs
           (id, session_id, project_id, user_id, status, automation_mode,
            provider, model, nexus_agent_type, parent_run_id)
           VALUES ($1, $2, $3, $4, 'running', 'automatic', $5, $6, 'subagent',
                   (SELECT id FROM agent_runs WHERE id = $7))
           ON CONFLICT (id) DO NOTHING"#,
    )
    .bind(run_id)
    .bind(session_id)
    .bind(project_id)
    .bind(user_id)
    .bind(provider)
    .bind(model)
    .bind(anchor)
    .execute(db)
    .await;
    if let Err(e) = res {
        tracing::warn!(
            target: "mcp_core::subagent_native",
            subagent_run_id = %run_id,
            error = %e,
            "riga agent_runs del sub-run NON creata: gli step del figlio non saranno persistiti"
        );
    }
}

/// Esito di chiusura di un sub-run: status + summary + metriche, condiviso
/// dalle due righe da chiudere (nexus_subagent_runs + gemella agent_runs).
struct SubRunClosure<'a> {
    status: &'a str,
    summary: &'a str,
    iterations: i64,
    tokens_prompt: i64,
    tokens_completion: i64,
    cost_usd: f64,
    /// Blocco esito STRUTTURATO del sub-run (regola M / ADR 0034), distinto dallo
    /// `status` lifecycle: persistito su `nexus_subagent_runs.verdict` (mig
    /// project/0009) per il fan-in asincrono (poll) e i coordinatori.
    verdict: Value,
}

impl<'a> SubRunClosure<'a> {
    /// Chiusura dei rami TERMINALI senza [`NativeRunOutcome`] (timeout, errore del
    /// grafo): le metriche vengono dal ledger, l'unica fonte che sopravvive alla
    /// morte del run.
    ///
    /// Sostituisce una `without_metrics` che azzerava i contatori sulla premessa
    /// "il run e' morto prima di produrre usage". Premessa misuratamente FALSA: un
    /// sub-run va in timeout DOPO aver bruciato iterazioni, e il gateway le ha gia'
    /// fatturate (misurati 3 sub-run con iterations=0 e ledger $0,80 / $0,37 /
    /// $0,04; sulla chat 25, 19 run in timeout dichiaravano $0 contro $1,28 reali).
    /// Azzerare qui non "non contava": sottraeva spesa reale al cap.
    ///
    /// `iterations` resta 0: il conteggio vive solo nello stato del grafo, che qui
    /// non c'e'. Il ledger conosce il costo, non le iterazioni — meglio un
    /// contatore onestamente ignoto che un costo inventato.
    fn from_ledger(
        status: &'a str,
        summary: &'a str,
        verdict: Value,
        ledger: &crate::chat_messages::LedgerTotals,
    ) -> Self {
        Self {
            status,
            summary,
            iterations: 0,
            tokens_prompt: ledger.prompt_tokens,
            tokens_completion: ledger.completion_tokens,
            cost_usd: ledger.total_cost,
            verdict,
        }
    }
}

/// Blocco esito strutturato per i rami TERMINALI del sub-run privi di un
/// [`NativeRunOutcome`] (timeout / errore del motore): stessa forma di
/// [`NativeRunOutcome::structured_verdict`] cosi' il coordinatore e il poll
/// leggono SEMPRE lo stesso schema. Le chiavi vengono dal PUNTO UNICO
/// [`verdict_keys`] (regola L): la parita' con `structured_verdict` e' quindi
/// strutturale (stesse costanti), non solo asserita — piu' il test
/// `terminal_verdict_stessa_forma_di_structured_verdict` come rete. `verdict`
/// usa il vocabolario canonico (`AgentRunStatus::as_str`: `timed_out` /
/// `failed`), `success` e' sempre `false`.
fn terminal_verdict(verdict: &str, error_class: &str) -> Value {
    use verdict_keys as k;
    json!({
        k::VERDICT: verdict,
        k::SUCCESS: false,
        k::DECLARED: Value::Null,
        k::REVIEW: Value::Null,
        k::ADVISORY: Value::Null,
        k::DEBATE: Value::Null,
        k::FINAL_GATE_PASSED: Value::Null,
        k::FINAL_GATE_UNVERIFIED: Value::Null,
        k::FINAL_GATE_FAILED_PENDING: false,
        k::FORCED_CLOSE_UNVERIFIED: false,
        k::ERROR_CLASS: error_class,
    })
}

/// Marca una sub-run come conclusa su `nexus_subagent_runs` E chiude la riga
/// gemella `agent_runs` del figlio, in UNA statement atomica (CTE): stessi
/// bind per entrambe. La gemella esiste solo per i figli tracciati (la WHERE
/// id limita l'effetto); stessi status (completed/paused/timeout/failed):
/// `agent_runs.status` e' TEXT libero. Il `verdict` strutturato (regola M) va
/// SOLO sulla riga sub (la gemella eredita l'esito dal finalizzatore del padre).
/// Best-effort per i chiamanti (la finalizzazione non deve fallire per un
/// errore di persistenza) ma MAI muto: l'UPDATE respinto lascerebbe la riga
/// 'running' per sempre (depth chain bloccata, poll che non converge) e senza
/// log sarebbe invisibile — WARN qui, nel punto unico (regola L).
async fn mark_run(
    db: &sqlx::PgPool,
    run_id: Uuid,
    c: SubRunClosure<'_>,
) -> Result<(), sqlx::Error> {
    let compact = c.summary.chars().take(4000).collect::<String>();
    let res = sqlx::query(
        "WITH sub AS (
            UPDATE nexus_subagent_runs SET
                status = $1, final_summary = $2, iterations = $3,
                tokens_prompt = $4, tokens_completion = $5, cost_usd = $6,
                verdict = $8, completed_at = NOW()
             WHERE id = $7
        )
        UPDATE agent_runs SET
            status = $1, final_answer = $2, iteration_count = $3,
            prompt_tokens = $4, completion_tokens = $5,
            total_tokens = $4 + $5, total_cost = $6, completed_at = NOW()
         WHERE id = $7",
    )
    .bind(c.status)
    .bind(&compact)
    .bind(c.iterations as i32)
    .bind(c.tokens_prompt as i32)
    .bind(c.tokens_completion as i32)
    .bind(c.cost_usd)
    .bind(run_id)
    .bind(c.verdict)
    .execute(db)
    .await
    .map(|_| ());
    if let Err(e) = &res {
        tracing::warn!(
            subagent_run_id = %run_id,
            status = c.status,
            error = %e,
            "mark_run: esito del sub-run NON persistito (la riga resta 'running')"
        );
    }
    res
}

const TRUNC_SUFFIX: &str = "...[truncated]";

/// Tronca il summary inviato al main (riceve solo questo, non l'intera conversazione).
fn compact_summary(text: &str) -> String {
    const MAX: usize = 600;
    if text.chars().count() <= MAX {
        return text.to_string();
    }
    let keep = MAX.saturating_sub(TRUNC_SUFFIX.len());
    let head: String = text.chars().take(keep).collect();
    format!("{head}{TRUNC_SUFFIX}")
}

/// Assembla le `NativeDeps` (ToolRunner in-process + client gateway) dal
/// `AgentToolContext`. Specchio di `build_native_deps` (agent_run.rs) ma a partire
/// dal ctx del tool (che non porta `AppState`): riusa il PUNTO UNICO del cablaggio
/// gateway (`NexusGatewayClient::from_db`, regola L).
///
/// `run_timeout_s` e' il timeout REALE della figura (gia' clampato dalla
/// deadline del padre): da li' nasce il budget d'attesa verso il gateway. Prima
/// il budget veniva derivato dal default globale (300s) anche per una figura che
/// vive 240s.
async fn build_native_deps_for_tool(ctx: &AgentToolContext, run_timeout_s: i64) -> NativeDeps {
    let db = (*ctx.core.db).clone();
    let tool_runner_deps = crate::tool_runner_server::ToolRunnerDeps {
        db: db.clone(),
        neural: ctx.neural.clone(),
        playwright_channels: ctx.playwright_channels.clone(),
        dependency_status: ctx.dependency_status.clone(),
        project_channels: ctx.core.project_channels.clone(),
        monitor_registry: ctx.core.monitor_registry.clone(),
        port_registry: ctx.port_registry.clone(),
    };
    let gateway = crate::nexus_gateway::NexusGatewayClient::from_db_for_run(
        &db,
        u64::try_from(run_timeout_s).ok().filter(|&s| s > 0),
    )
    .await;
    NativeDeps {
        db,
        tool_runner_deps,
        gateway,
    }
}

/// `nexus_subagent_poll` — stato di una sub-run da `nexus_subagent_runs` (DB-only,
/// niente brain). Il main lo usa per i kind background.
/// Pool del progetto per i tool sub-agent (poll/resume), o messaggio d'errore
/// gia' formattato col marker del tool (il contratto dei tool_result e' la
/// stringa). Punto unico locale del pattern (regola L): i due tool condividono
/// identica risoluzione e identico esito di indisponibilita'.
async fn subagent_tool_pool(ctx: &AgentToolContext, tool: &str) -> Result<sqlx::PgPool, String> {
    crate::project_db_routes::project_data_pool_from(&ctx.core.db, ctx.core.project_id)
        .await
        .map_err(|e| format!("\u{274C} [{tool}] DB progetto non disponibile: {e}"))
}

pub async fn tool_nexus_subagent_poll(ctx: &AgentToolContext, input: &Value) -> String {
    let run_id = match input.get(K_SUB_RUN_ID).and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            return "\u{274C} [nexus_subagent_poll] parametro 'subagent_run_id' obbligatorio"
                .to_string()
        }
    };
    // Routing separazione DB: nexus_subagent_runs e' migrata; il sub-run e' nel DB
    // del progetto corrente (stesso project_id che lo ha dispatchato).
    let proj_pool = match subagent_tool_pool(ctx, "nexus_subagent_poll").await {
        Ok(p) => p,
        Err(msg) => return msg,
    };
    let row = sqlx::query(
        "SELECT id::text, status, kind, final_summary, artifacts, iterations,
                tokens_prompt, tokens_completion, cost_usd, depth, source, is_background,
                verdict
         FROM nexus_subagent_runs WHERE id::text = $1",
    )
    .bind(&run_id)
    .fetch_optional(&proj_pool)
    .await;
    match row {
        Ok(Some(r)) => json!({
            K_SUB_RUN_ID: r.try_get::<String, _>("id").unwrap_or_default(),
            "status": r.try_get::<String, _>("status").unwrap_or_default(),
            "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
            "summary": r.try_get::<Option<String>, _>("final_summary").unwrap_or(None),
            "artifacts": r.try_get::<Option<Vec<String>>, _>("artifacts").unwrap_or(None).unwrap_or_default(),
            "iterations": r.try_get::<i32, _>("iterations").unwrap_or(0),
            "tokens": {
                "prompt": r.try_get::<i32, _>("tokens_prompt").unwrap_or(0),
                "completion": r.try_get::<i32, _>("tokens_completion").unwrap_or(0),
            },
            "cost_usd": r.try_get::<f64, _>("cost_usd").unwrap_or(0.0),
            "depth": r.try_get::<i32, _>("depth").unwrap_or(1),
            "source": r.try_get::<String, _>("source").unwrap_or_else(|_| "db".into()),
            "is_background": r.try_get::<bool, _>("is_background").unwrap_or(false),
            // Verdetto ESITO strutturato (mig project/0009): `null` finche' il
            // sub-run non e' finalizzato (fase running). Il coordinatore che fa
            // poll legge success/verdict qui senza dedurre l'esito dalla prosa
            // di `summary` (regola M).
            "outcome": r.try_get::<Option<Value>, _>("verdict").unwrap_or(None),
        })
        .to_string(),
        Ok(None) => format!("\u{274C} [nexus_subagent_poll] sub-agent run '{run_id}' non trovato"),
        Err(e) => format!("\u{274C} [nexus_subagent_poll] query fallita: {e}"),
    }
}

/// `nexus_subagent_resume` — riprende una sub-run sul GRAFO NATIVO (niente brain).
/// Il run nativo gira da capo dal task della row (i checkpoint del sub-run vivono
/// su `nexus_graph_checkpoints`, ma la ripresa nativa qui ri-esegue il sub-run dal
/// suo `run_id`: la stessa thread del grafo riprende dal checkpoint persistente).
pub async fn tool_nexus_subagent_resume(ctx: &AgentToolContext, input: &Value) -> String {
    let run_id_str = match input.get(K_SUB_RUN_ID).and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            return "\u{274C} [nexus_subagent_resume] parametro 'subagent_run_id' obbligatorio"
                .to_string()
        }
    };
    let run_id = match Uuid::parse_str(&run_id_str) {
        Ok(u) => u,
        Err(_) => {
            return format!(
                "\u{274C} [nexus_subagent_resume] subagent_run_id non valido: {run_id_str}"
            )
        }
    };

    // Routing separazione DB: nexus_subagent_runs e' migrata; il sub-run da riprendere
    // e' nel DB del progetto corrente (stesso project_id che lo ha dispatchato).
    let proj_pool = match subagent_tool_pool(ctx, "nexus_subagent_resume").await {
        Ok(p) => p,
        Err(msg) => return msg,
    };
    let row = sqlx::query(
        "SELECT kind, task_description, context_blob, expected_format, status, depth, parent_run_id
         FROM nexus_subagent_runs WHERE id = $1",
    )
    .bind(run_id)
    .fetch_optional(&proj_pool)
    .await;
    let row = match row {
        Ok(Some(r)) => r,
        Ok(None) => {
            return format!("\u{274C} [nexus_subagent_resume] sub-agent run '{run_id}' non trovato")
        }
        Err(e) => return format!("\u{274C} [nexus_subagent_resume] query fallita: {e}"),
    };
    let status: String = row.get("status");
    if !matches!(status.as_str(), "paused" | "running" | "timeout") {
        return json!({"status": "noop", K_SUB_RUN_ID: run_id_str, "current_status": status})
            .to_string();
    }
    let kind: String = row.get("kind");
    let task: String = row.get("task_description");
    let context_blob: String = row
        .try_get::<Option<String>, _>("context_blob")
        .ok()
        .flatten()
        .unwrap_or_default();
    let expected: String = row
        .try_get::<Option<String>, _>("expected_format")
        .ok()
        .flatten()
        .unwrap_or_default();

    // Ripresa = ri-esecuzione del sub-run sul kind dato. Riusa lo stesso percorso
    // del dispatch (guard + grafo nativo): semantica onesta, niente notifica brain.
    // Il resume gira SEMPRE sulla root condivisa (nessun isolamento fisico): la
    // ripresa di un singolo sub-run non e' un batch parallelo-che-scrive.
    // Il resume e' SINCRONO (bloccante): riprende un singolo sub-run e ne attende
    // l'esito, non e' un dispatch background.
    let res =
        run_single_subagent(ctx, &kind, &task, &context_blob, &expected, None, false, &[]).await;
    res.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// REGRESSIONE (2026-07-26, osservata sul progetto e2e-todo): con openai e
    /// anthropic in cooldown billing restava il solo openrouter, la selezione
    /// deduplicava PER PROVIDER e consegnava UN candidato. Il panel a due si
    /// riduceva onestamente a uno — e il gate lo riconvocava a ogni ciclo,
    /// sempre `openrouter/z-ai/glm-4.7-flash`: sei sub-run di review, sei volte
    /// lo stesso giudice, mentre quel provider offriva DIECI modelli qualificati
    /// nello stesso tier che nessuno guardava.
    ///
    /// Il test parte dai candidati come li produce la produzione
    /// (`resolve_purpose_provider_candidates_db_by` sul purpose reale, letto
    /// dalle migrazioni reali), non da una lista scritta a mano: la lista esiste
    /// gia' altrove, e fabbricarla qui fisserebbe proprio l'assunto in esame
    /// (regola O).
    ///
    /// MUTAZIONE: rimettendo `CandidateDiversity::PerProvider` la selezione
    /// torna a un solo candidato e l'asserzione fallisce nominando il giudice
    /// unico.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn un_solo_provider_sano_da_comunque_giudici_distinti(pool: sqlx::PgPool) {
        // Il tier del purpose reale, letto dal seed invece che ricopiato.
        let tier: String = sqlx::query_scalar(
            "SELECT tier FROM nexus_purpose_model WHERE purpose = $1 AND tier IS NOT NULL",
        )
        .bind(REVIEW_PANEL_PURPOSE)
        .fetch_one(&pool)
        .await
        .expect("il purpose dei revisori deve avere un tier");

        // La condizione del 26/07: openai e anthropic in cooldown billing, un
        // solo provider eleggibile. Il cooldown non e' riproducibile qui, ma il
        // suo EFFETTO sul pool si': quei provider non sono selezionabili.
        sqlx::query("UPDATE ai_price_catalog SET is_enabled = false WHERE provider <> 'openrouter'")
            .execute(&pool)
            .await
            .expect("cooldown degli altri provider");

        // Due modelli distinti dello stesso provider, entrambi idonei al tier
        // del purpose. Upsert: il catalog seminato dalle migrazioni li contiene
        // gia', e quello che serve al test e' che siano eleggibili — non che
        // siano suoi. `last_probe_healthy_at` NON e' decorazione: senza, il
        // trigger `ai_price_catalog_enforce_probe_before_enable` rimette
        // `is_enabled=false` con reason `unverified_no_probe`, e il catalog
        // resterebbe vuoto senza che nulla lo dica. La fixture a mano usata
        // altrove quel trigger non ce l'ha (regola O: il migratore reale e' piu'
        // severo dello schema ricopiato).
        for (model, costo) in [("z-ai/glm-4.7-flash", 0.07), ("qwen/qwen3-235b-a22b-2507", 0.071)] {
            sqlx::query(
                "INSERT INTO ai_price_catalog \
                   (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
                    performance_tier, capabilities, input_cost_per_million_tokens, \
                    output_cost_per_million_tokens, currency, qualification_state, \
                    last_probe_healthy_at, supports_image_gen, supports_audio_in, \
                    supports_audio_out, supports_video_gen) \
                 VALUES ('openrouter', $1, true, true, 'none', $2, '[\"reasoning\"]'::jsonb, \
                         $3, $3, 'USD', 'qualified', NOW(), false, false, false, false) \
                 ON CONFLICT (provider, model) DO UPDATE SET \
                   is_enabled = true, supports_tool_use = true, \
                   agentic_thinking_policy = 'none', performance_tier = EXCLUDED.performance_tier, \
                   capabilities = EXCLUDED.capabilities, \
                   qualified_capabilities = EXCLUDED.capabilities, \
                   input_cost_per_million_tokens = EXCLUDED.input_cost_per_million_tokens, \
                   qualification_state = 'qualified', qualification_expires_at = NULL, \
                   last_probe_healthy_at = NOW(), \
                   supports_image_gen = false, supports_audio_in = false, \
                   supports_audio_out = false, supports_video_gen = false, \
                   auto_disabled_at = NULL, auto_disabled_reason = NULL",
            )
            .bind(model)
            .bind(&tier)
            .bind(costo)
            .execute(&pool)
            .await
            .expect("catalog");
        }

        let candidati = crate::internal_routing::resolve_purpose_provider_candidates_db_by(
            &pool,
            REVIEW_PANEL_PURPOSE,
            2,
            1,
            // Il criterio della PRODUZIONE, non uno scelto qui.
            REVIEW_PANEL_DIVERSITY,
        )
        .await
        .expect("candidati revisori");

        let panel = panel_revisori(&candidati, 2);
        let giudici: Vec<String> = panel
            .iter()
            .map(|c| format!("{}/{}", c.provider, c.model))
            .collect();
        assert_eq!(
            panel.len(),
            2,
            "un provider solo ma piu' modelli qualificati deve dare DUE giudici: \
             ridursi a uno e riconvocarlo ogni ciclo non e' un quorum piu' piccolo, \
             e' nessun quorum. Convocati: {giudici:?}"
        );
        assert_eq!(
            panel[0].judge_key().1,
            "z-ai/glm-4.7-flash",
            "l'ordine di preferenza (costo) resta quello della selezione: {giudici:?}"
        );
    }

    /// La garanzia del panel non dipende da come la fonte ha ordinato o filtrato:
    /// da candidati che ripetono lo stesso giudice esce un panel RIDOTTO, e i
    /// convocati sono esattamente le coppie (provider, model) distinte.
    ///
    /// Qui la lista e' costruita a mano di proposito: e' l'input patologico che
    /// la selezione NON produce (il catalog ha un indice unico su provider+model),
    /// quindi non esiste altrove da cui prenderlo. E' la difesa del punto unico,
    /// non una riproduzione del percorso di produzione.
    #[test]
    fn due_istanze_dello_stesso_modello_non_sono_due_giudici() {
        use crate::internal_routing::PurposeProviderCandidate as C;
        let c = |p: &str, m: &str| C {
            provider: p.to_string(),
            model: m.to_string(),
            tier: Some("high".to_string()),
        };

        // Tre slot, tre candidati, ma due sono lo stesso giudice: si convoca in due.
        let panel = panel_revisori(
            &[
                c("openrouter", "z-ai/glm-4.7-flash"),
                c("openrouter", "z-ai/glm-4.7-flash"),
                c("openrouter", "z-ai/glm-5.2"),
            ],
            3,
        );
        assert_eq!(panel.len(), 2, "convocati = giudici distinti: {panel:?}");
        assert_eq!(panel[1].model, "z-ai/glm-5.2");

        // Stesso modello, provider diverso: sono due giudici (infrastrutture,
        // quote e versioni distinte), non una duplicazione.
        let cross = panel_revisori(
            &[c("openrouter", "z-ai/glm-5.2"), c("zai", "z-ai/glm-5.2")],
            2,
        );
        assert_eq!(cross.len(), 2);

        // Il confronto e' insensibile al caso: la stessa coppia scritta in due
        // modi resta lo stesso giudice.
        let maiuscole = panel_revisori(
            &[c("OpenRouter", "Z-AI/GLM-5.2"), c("openrouter", "z-ai/glm-5.2")],
            2,
        );
        assert_eq!(maiuscole.len(), 1, "{maiuscole:?}");

        // Candidati in abbondanza: si tiene la dimensione richiesta.
        assert_eq!(
            panel_revisori(&[c("a", "m1"), c("b", "m2"), c("c", "m3")], 2).len(),
            2
        );
        // Nessun candidato: nessun pin da assegnare (i revisori partono senza,
        // e li instrada il routing).
        assert!(panel_revisori(&[], 3).is_empty());
    }

    /// REGRESSIONE (2026-07-26): tutti i revisori giravano sullo STESSO
    /// provider/modello, perche' `convene_review_panel` li lanciava senza pin e
    /// il routing li instradava identici. N revisori identici non sono un
    /// quorum: sono un giudizio unico contato N volte, e quando quel modello
    /// sbaglia il run viene bocciato fino al cap dei tentativi con l'apparenza
    /// di una verifica plurale (osservato: 8 sub-run di review, tutti
    /// mistral/mistral-small-latest, 4 review consecutive non superate).
    ///
    /// Se il purpose sparisce dal seed il pin non viene applicato e il difetto
    /// torna in silenzio: questo test lo impedisce, girando sulle migrazioni
    /// reali invece che su uno schema ricopiato (regola O).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_purpose_dei_revisori_esiste_nel_seed(pool: sqlx::PgPool) {
        let presente: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM nexus_purpose_model WHERE purpose = $1)",
        )
        .bind(REVIEW_PANEL_PURPOSE)
        .fetch_one(&pool)
        .await
        .expect("query purpose");
        assert!(
            presente,
            "il purpose '{REVIEW_PANEL_PURPOSE}' deve esistere in nexus_purpose_model: \
             senza di esso i revisori tornano tutti sullo stesso provider e il panel \
             smette di essere un quorum"
        );
    }

    // Nomi fixture ricorrenti dei test del ponte narrazione.
    const T_EDIT: &str = "edit_file";
    const T_RUN: &str = "run_command";
    const FIX_PATH: &str = "src/a.rs";

    /// La misura che distingue "il timer e' scattato tardi" da "la scrittura di
    /// chiusura e' arrivata tardi": nell'incidente del 19/07 le due cause erano
    /// indistinguibili perche' l'unico timestamp disponibile era quello della
    /// scrittura, e il ritardo era in realta' l'attesa di una connessione.
    #[test]
    fn il_ritardo_dello_scatto_e_l_eccesso_sul_budget() {
        // Timer puntuale: nessun ritardo da segnalare.
        assert_eq!(ritardo_scatto_ms(300_000, 300), 0);
        assert_eq!(ritardo_scatto_ms(300_120, 300), 120);
        // Il caso dell'incidente, se lo scatto fosse davvero tardato.
        assert_eq!(ritardo_scatto_ms(427_000, 300), 127_000);
        // Scatto anticipato (clock non monotono altrove): mai negativo.
        assert_eq!(ritardo_scatto_ms(299_000, 300), 0);
        // Budget non valorizzato: l'intero tempo trascorso e' l'eccesso, e non
        // si moltiplica un negativo.
        assert_eq!(ritardo_scatto_ms(5_000, 0), 5_000);
        assert_eq!(ritardo_scatto_ms(5_000, -7), 5_000);
    }

    #[test]
    fn err_ha_marker_errore() {
        let m = err("boom");
        assert!(m.starts_with('\u{274C}'));
        assert!(m.contains("dispatch_subagent"));
    }

    /// Outcome minimo per i test dei contatori: tutti i campi a zero/None.
    fn outcome_zero() -> crate::native_engine::NativeRunOutcome {
        crate::native_engine::NativeRunOutcome {
            completed: true,
            awaiting_subagents: false,
            final_answer: None,
            stop_reason: None,
            provider_used: None,
            model_used: None,
            resume_at: None,
            iterations: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            total_cost: 0.0,
            user_intent: None,
            reasoning: None,
            messages_json: None,
            declared_outcome: None,
            review_verdict: None,
            advisory_verdict: None,
            debate_position: None,
            error_class: None,
            provider_error_close: false,
            forced_close_unverified: false,
            final_gate_passed: None,
            final_gate_unverified: None,
            final_gate_failed_pending: false,
            review_panel_rejected: false,
            review_panel_no_correction: false,
            review_panel_last: None,
            pending_actions: Vec::new(),
            council_requirements: Vec::new(),
            council_conformance: None,
        }
    }

    /// REGRESSIONE (misurata, chat 25): il sub-run pubblicava i contatori
    /// dell'ULTIMO TURNO come totale — $0,0338 contro $0,1510 reali (4,5x). Lo
    /// stesso numero alimenta l'hard cap di spesa via nexus_subagent_runs.cost_usd.
    #[test]
    fn sub_run_adotta_i_totali_del_ledger_non_quelli_dell_ultimo_turno() {
        let mut o = outcome_zero();
        // Contatori del grafo = ultima chiamata (openrouter/x-ai/grok-4.5).
        o.total_cost = 0.033842;
        o.prompt_tokens = 13_780;
        o.completion_tokens = 1_047;
        o.total_tokens = 14_827;
        let ledger = crate::chat_messages::LedgerTotals {
            total_cost: 0.150972,
            prompt_tokens: 63_177,
            completion_tokens: 4_103,
            total_tokens: 67_280,
            rows: 8,
        };
        assert!(super::apply_ledger_to_outcome(&mut o, &ledger));
        assert!(
            (o.total_cost - 0.150972).abs() < 1e-9,
            "costo del sub-run INTERO"
        );
        assert_eq!(o.prompt_tokens, 63_177);
        assert_eq!(o.total_tokens, 67_280);
    }

    #[test]
    fn sub_run_senza_righe_ledger_conserva_i_contatori_del_grafo() {
        // Nessuna chiamata contabilizzata (provider che non scrive ledger):
        // non inventare, lascia quello che il grafo ha misurato.
        let mut o = outcome_zero();
        o.total_cost = 0.02;
        o.prompt_tokens = 900;
        let vuoto = crate::chat_messages::LedgerTotals::default();
        assert!(!super::apply_ledger_to_outcome(&mut o, &vuoto));
        assert!((o.total_cost - 0.02).abs() < 1e-9);
        assert_eq!(o.prompt_tokens, 900);
    }

    /// Il ramo TIMEOUT azzerava i contatori sulla premessa "il run e' morto prima
    /// di produrre usage": falsa (misurati 3 sub-run con iterations=0 e ledger
    /// $0,80 / $0,37 / $0,04). La spesa reale va al cap, non persa.
    #[test]
    fn chiusura_terminale_prende_la_spesa_dal_ledger() {
        let ledger = crate::chat_messages::LedgerTotals {
            total_cost: 0.801292,
            prompt_tokens: 120_000,
            completion_tokens: 3_400,
            total_tokens: 123_400,
            rows: 5,
        };
        let c = super::SubRunClosure::from_ledger("timeout", "[Sub-agent timeout]", json!({}), &ledger);
        assert!(
            (c.cost_usd - 0.801292).abs() < 1e-9,
            "il timeout non cancella la spesa gia' fatturata"
        );
        assert_eq!(c.tokens_prompt, 120_000);
        assert_eq!(c.status, "timeout");
    }

    #[test]
    fn build_tools_json_filtra_per_whitelist() {
        let tools = build_tools_json(&["read_file".to_string(), "write_file".to_string()]);
        let arr = tools.as_array().expect("array");
        // Solo i tool in whitelist (+ il canale di completamento auto-iniettato),
        // con schema REALE (campo input_schema presente).
        assert!(
            !arr.is_empty(),
            "lo schema reale deve contenere i tool richiesti"
        );
        for t in arr {
            let name = t.get("name").and_then(Value::as_str).unwrap_or("");
            assert!(
                name == "read_file" || name == "write_file" || name == "task_complete",
                "tool inatteso: {name}"
            );
            assert!(t.get("input_schema").is_some(), "schema reale atteso");
        }
        let names: Vec<&str> = arr
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
    }

    // ── Fan-out: un task per sub-run (difetto D3, incidente 2026-07-15) ──────

    /// Cio' che `spawn` GARANTISCE (difetto D3): un sub-run che PANICA non
    /// abbatte piu' il fan-out. Col vecchio `FuturesUnordered` senza spawn i
    /// membri vivevano nel task del chiamante: un panic in uno di essi
    /// propagava e uccideva l'INTERO consiglio, in silenzio.
    /// La rete di sicurezza (`tokio::time::timeout`) e' FUORI dalla funzione
    /// sotto esame: una regressione fallisce netta invece di appendere la suite.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn un_membro_che_panica_non_abbatte_il_fanout() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let conclusi = Arc::new(AtomicUsize::new(0));
        let n = 4usize;
        let sem = Arc::new(tokio::sync::Semaphore::new(n));
        let mut handles = Vec::new();
        for i in 0..n {
            let sem = sem.clone();
            let conclusi = conclusi.clone();
            handles.push(tokio::spawn(async move {
                let _p = sem.acquire_owned().await.expect("permit");
                if i == 0 {
                    panic!("sub-run esploso (simulato)");
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                conclusi.fetch_add(1, Ordering::SeqCst);
            }));
        }
        let mut panicati = 0usize;
        for h in handles {
            // JoinError = panic del sub-run: spawn_fanout lo traduce in un
            // Value d'errore strutturato e prosegue (regola M).
            if h.await.is_err() {
                panicati += 1;
            }
        }
        assert_eq!(panicati, 1, "il panic resta CONFINATO al suo task");
        assert_eq!(
            conclusi.load(Ordering::SeqCst),
            n - 1,
            "gli altri membri devono concludere: col FuturesUnordere il panic              avrebbe abbattuto l'intero fan-out"
        );
    }

    /// `spawn_fanout` REALE: un membro che panica diventa un esito STRUTTURATO
    /// (regola M) e il fan-out ritorna comunque N risultati, uno per membro.
    /// Senza la traduzione del `JoinError` il consiglio perderebbe una figura
    /// IN SILENZIO: il roster direbbe 6, i risultati sarebbero 5, e il quorum
    /// (b152ef0d) conterebbe su un denominatore sbagliato.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn spawn_fanout_traduce_il_panic_in_esito_strutturato(pool: sqlx::PgPool) {
        sqlx::query("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .execute(&pool)
            .await
            .expect("settings");
        // Scope Nested: il test resta ermetico (non tocca il semaforo di
        // processo globale OnceCell, condiviso tra i test).
        let out = spawn_fanout(&pool, 3, FanoutScope::Nested, |i| async move {
            if i == 1 {
                panic!("sub-run esploso (simulato)");
            }
            json!({ "status": "completed", "outcome": { "success": true, "i": i } })
        })
        .await;
        assert_eq!(out.len(), 3, "un risultato per membro, sempre");
        assert_eq!(
            out[1]["error_code"], "subrun_panicked",
            "il panic deve diventare un esito strutturato, non una riga persa"
        );
        assert_eq!(out[0]["outcome"]["success"], json!(true));
        assert_eq!(out[2]["outcome"]["success"], json!(true));
    }

    /// GOVERNOR (mig 0603): due fan-out top-level CONCORRENTI condividono il
    /// semaforo di processo — la concorrenza TOTALE non supera mai il suo tetto,
    /// anche se i semafori locali permetterebbero di piu'. Semafori ESPLICITI
    /// via `spawn_fanout_with`: niente stato globale nel test (regola F).
    #[tokio::test]
    async fn fanout_process_semaphore_limita_la_concorrenza_totale() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let process = Arc::new(tokio::sync::Semaphore::new(4));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let make = |in_flight: Arc<AtomicUsize>, peak: Arc<AtomicUsize>| {
            move |_i: usize| {
                let in_flight = in_flight.clone();
                let peak = peak.clone();
                async move {
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    json!({ "ok": true })
                }
            }
        };
        // Due panel da 6 con semafori locali larghi (6): senza il semaforo di
        // processo la concorrenza arriverebbe a 12.
        let (a, b) = tokio::join!(
            spawn_fanout_with(
                Arc::new(tokio::sync::Semaphore::new(6)),
                Some(process.clone()),
                6,
                make(in_flight.clone(), peak.clone()),
            ),
            spawn_fanout_with(
                Arc::new(tokio::sync::Semaphore::new(6)),
                Some(process.clone()),
                6,
                make(in_flight.clone(), peak.clone()),
            ),
        );
        assert_eq!(a.len(), 6);
        assert_eq!(b.len(), 6);
        assert!(
            peak.load(Ordering::SeqCst) <= 4,
            "picco {} oltre il tetto di processo 4",
            peak.load(Ordering::SeqCst)
        );
    }

    /// GOVERNOR: un fan-out `Nested` NON tocca il semaforo di processo — anche
    /// con il processo SATURO (zero permessi) i nested completano. E' la
    /// proprieta' anti-deadlock: un figlio non attende mai un permesso tenuto
    /// dal proprio padre.
    #[tokio::test]
    async fn fanout_nested_non_attende_il_semaforo_di_processo() {
        use std::sync::Arc;
        let process = Arc::new(tokio::sync::Semaphore::new(1));
        // Satura il processo: un top-level fittizio tiene l'unico permesso.
        let _held = process.clone().acquire_owned().await.expect("permesso");
        // Il nested (process=None) deve completare comunque, entro un timeout
        // stretto: se per errore acquisisse il semaforo saturo, appenderebbe.
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            spawn_fanout_with(
                Arc::new(tokio::sync::Semaphore::new(2)),
                None,
                3,
                |i| async move { json!({ "i": i }) },
            ),
        )
        .await
        .expect("il nested non deve attendere il semaforo di processo");
        assert_eq!(out.len(), 3);
    }

    /// Il tetto di concorrenza viene dal DB (regola G) e clampa a >=1.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn fanout_max_parallel_dal_db(pool: sqlx::PgPool) {
        sqlx::query("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .execute(&pool)
            .await
            .expect("settings");
        // Riga assente -> default storico (nessun tetto piu' stretto del
        // fan-out nominale del consiglio).
        assert_eq!(fanout_max_parallel(&pool).await, DEFAULT_FANOUT_MAX_PARALLEL);
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES ('orchestrator.subagent_fanout_max_parallel', '2')",
        )
        .execute(&pool)
        .await
        .expect("insert");
        // Scrittura diretta: lettura cache-ata, il test invalida esplicitamente.
        nexus_auth::invalidate_setting_cache(&pool, "orchestrator.subagent_fanout_max_parallel");
        assert_eq!(fanout_max_parallel(&pool).await, 2, "il DB governa (regola G)");
        sqlx::query("UPDATE settings SET value = '0' WHERE key = 'orchestrator.subagent_fanout_max_parallel'")
            .execute(&pool)
            .await
            .expect("update");
        nexus_auth::invalidate_setting_cache(&pool, "orchestrator.subagent_fanout_max_parallel");
        assert_eq!(
            fanout_max_parallel(&pool).await,
            DEFAULT_FANOUT_MAX_PARALLEL,
            "0 non e' un tetto valido: si ricade sul default, mai su zero permessi"
        );
    }

    /// LIMITE MISURATO E DICHIARATO (non un difetto del test, non una scusa):
    /// il timer di un sub-run NON e' protetto dal blocking sincrono di un altro
    /// sub-run, nemmeno dando a ognuno il proprio task con `spawn`.
    ///
    /// Meccanica, misurata il 16/07 (e diversa da quella che questa nota
    /// affermava prima): tokio non ha un thread dedicato ai timer. La scadenza
    /// viene consegnata da un worker che polla il time driver, e un worker fermo
    /// dentro un `poll()` bloccante non polla nulla. Quando nessun worker libero
    /// raccoglie il driver, slittano insieme TUTTI i timer del runtime: la firma
    /// e' che le vittime riportano lo STESSO ritardo al millisecondo (5 su 5 a
    /// 1571ms), mai un ritardo a macchia di leopardo. E' un fattore globale del
    /// runtime, non la vicinanza al bloccante: la spiegazione precedente (i task
    /// nella coda locale del worker bloccato, non salvati dal work-stealing) e'
    /// falsificata da quella firma e dal fatto che qui i task nascono dal thread
    /// del test, quindi dalla coda globale, non da un worker.
    ///
    /// Perche' `worker_threads = 1` e non gli 8 di prima: con un worker i worker
    /// liberi sono zero PER COSTRUZIONE e il limite si riproduce sempre. Con piu'
    /// worker il fenomeno e' lo stesso, ma il suo manifestarsi diventa un lancio
    /// di dadi: dipende da quale worker parcheggia (e quindi raccoglie il driver)
    /// dopo che il blocco e' cominciato. Gradiente misurato, stessa macchina,
    /// 6 task di cui 1 bloccante:
    ///   1 worker -> 5/5 riprodotto      2 worker  -> 6/6
    ///   8 worker -> 7/8 a vuoto, 3/6 sotto carico leggero, 1/8 a CPU sature
    ///   32 worker -> 0/5 a vuoto (i timer restano puntuali)
    /// La versione a 8 worker falliva ~1 volta su 3 nella suite (regola F): non
    /// per una misura imprecisa, ma perche' asseriva un esito che lo scheduler
    /// non garantisce. Quella flakiness ERA il limite: se il trascinamento fosse
    /// garantito `spawn` sarebbe inutile, se lo fosse la puntualita' `spawn`
    /// sarebbe la cura; non e' ne' l'uno ne' l'altro. Il test tiene quindi il
    /// caso limite (zero worker liberi), che e' deterministico e dimostra la
    /// non-garanzia; il gradiente qui sopra resta come misura, non come assert.
    ///
    /// Conseguenza, da dire senza giri di parole: `spawn_fanout` da' isolamento
    /// dei panic e un timer per sub-run, e resta igiene strutturale necessaria,
    /// ma NON e' la cura del blocco collettivo del 15/07. La cura e' togliere
    /// il blocking dal path (misura che lo nomina: tokio-console).
    ///
    /// Come e' costruito: le figure sane armano il timer e lo DICHIARANO su un
    /// canale (`send` e arm stanno nello stesso poll, senza await in mezzo:
    /// quando la conferma parte il timer c'e' gia'); il bloccante comincia solo
    /// quando TUTTE hanno dichiarato. E' l'ordine dell'incidente — le altre
    /// figure erano gia' partite — ottenuto pero' con una barriera causale e non
    /// con uno sleep di cortesia che il carico puo' sfasare: un test in cui il
    /// bloccante viene pollato per primo sarebbe CIECO, le vittime armerebbero i
    /// timer a danno gia' fatto. L'assert e' sull'ORDINE degli eventi e non su
    /// soglie dell'orologio: i timer scadono a 200ms dentro un blocco da 800ms
    /// cominciato dopo che erano armati, quindi se il blocking non li
    /// trascinasse il primo evento osservato sarebbe un TimerScattato. Che non
    /// sia cieco e' verificato per mutation: sostituendo il blocco con
    /// `tokio::time::sleep`, che cede il worker invece di rubarlo, il test
    /// diventa rosso 10 volte su 10.
    ///
    /// Un rosso qui significa che il blocking sincrono non trascina piu' i timer
    /// nemmeno a worker liberi zero: tokio ha cambiato meccanica, o il blocco di
    /// questo test non blocca piu'. Aggiornare questa nota e quella su
    /// `spawn_fanout`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn spawn_non_protegge_dal_blocking_sincrono_limite_dichiarato() {
        #[derive(Debug, PartialEq, Eq)]
        enum Evento {
            Sbloccato,
            TimerScattato,
        }
        const SANI: usize = 5;
        const TIMER: std::time::Duration = std::time::Duration::from_millis(200);
        const BLOCCO: std::time::Duration = std::time::Duration::from_millis(800);

        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<Evento>();
        let (armato_tx, mut armato_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

        for _ in 0..SANI {
            let ev_tx = ev_tx.clone();
            let armato_tx = armato_tx.clone();
            tokio::spawn(async move {
                armato_tx.send(()).expect("conferma armato");
                let _ = tokio::time::timeout(TIMER, std::future::pending::<()>()).await;
                let _ = ev_tx.send(Evento::TimerScattato);
            });
        }

        tokio::spawn(async move {
            for _ in 0..SANI {
                armato_rx.recv().await.expect("timer armato");
            }
            std::thread::sleep(BLOCCO);
            ev_tx.send(Evento::Sbloccato).expect("sblocco");
        });

        let mut sequenza = Vec::with_capacity(SANI + 1);
        for _ in 0..(SANI + 1) {
            sequenza.push(ev_rx.recv().await.expect("evento"));
        }
        assert_eq!(
            sequenza[0],
            Evento::Sbloccato,
            "LIMITE CAMBIATO: un timer da {TIMER:?} e' scattato durante un blocco \
             sincrono da {BLOCCO:?} ({sequenza:?}). Se e' voluto (blocking rimosso \
             o isolato in spawn_blocking), aggiornare questo test e la nota su \
             spawn_fanout."
        );
    }

    /// Il semaforo NON e' una barriera a ondate: con 1 permesso i membri sono
    /// seriali, ma il successivo parte appena il precedente finisce (nessun
    /// allineamento delle conclusioni, che ricreerebbe la firma "tutti insieme").
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn il_semaforo_libera_il_permesso_a_ogni_conclusione() {
        use std::sync::Arc;
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let t0 = std::time::Instant::now();
        let mut handles = Vec::new();
        for _ in 0..3 {
            let sem = sem.clone();
            handles.push(tokio::spawn(async move {
                let _p = sem.acquire_owned().await.expect("permit");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                t0.elapsed().as_millis()
            }));
        }
        let mut fine = Vec::new();
        for h in handles {
            fine.push(h.await.expect("task"));
        }
        fine.sort();
        // Conclusioni SCAGLIONATE (~100/200/300ms), non allineate.
        assert!(
            fine[1] - fine[0] >= 50 && fine[2] - fine[1] >= 50,
            "le conclusioni devono scaglionarsi, non allinearsi: {fine:?}"
        );
    }

    #[test]
    fn build_tools_json_vuoto_se_whitelist_vuota() {
        let tools = build_tools_json(&[]);
        assert_eq!(tools.as_array().map(|a| a.len()), Some(0));
    }

    /// INVARIANTE ADR 0034 (regola L): un sub-run coder senza canale di
    /// completamento nella whitelist riceve `task_complete` auto-iniettato, cosi'
    /// puo' dichiarare l'esito e chiudere invece di ciclare sui nudge G1
    /// (incidente run a6f25c1e: 39 iterazioni).
    #[test]
    fn build_tools_json_inietta_task_complete_se_mancante() {
        let tools = build_tools_json(&["read_file".to_string(), "run_command".to_string()]);
        let names: Vec<&str> = tools
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert!(
            names.contains(&"task_complete"),
            "task_complete deve essere iniettato: {names:?}"
        );
    }

    /// Una figura del consiglio (whitelist con `advisory_verdict`) NON riceve
    /// `task_complete`: il suo verdetto strutturato di ruolo E' gia' il canale di
    /// completamento (evita che la figura chiuda saltando il parere strutturato).
    #[test]
    fn build_tools_json_non_inietta_se_ha_verdetto_di_ruolo() {
        let tools = build_tools_json(&["read_file".to_string(), "advisory_verdict".to_string()]);
        let names: Vec<&str> = tools
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert!(
            names.contains(&"advisory_verdict"),
            "advisory_verdict atteso: {names:?}"
        );
        assert!(
            !names.contains(&"task_complete"),
            "task_complete NON deve essere iniettato se c'e' gia' advisory_verdict: {names:?}"
        );
    }

    #[test]
    fn compact_summary_tronca_oltre_soglia() {
        let breve = "ciao";
        assert_eq!(compact_summary(breve), "ciao");
        let lungo = "x".repeat(1000);
        let out = compact_summary(&lungo);
        assert!(out.ends_with(TRUNC_SUFFIX));
        assert!(out.chars().count() <= 600);
    }

    fn sc(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn should_isolate_batch_flag_off_sempre_sequenziale() {
        // Con isolamento NON disponibile (flag OFF, default) il ramo isolato non
        // scatta MAI, qualunque siano gli scope -> bit-identico allo storico.
        let scopes = vec![sc(&["crates/a"]), sc(&["crates/b"])];
        assert!(
            !should_isolate_batch(false, &scopes),
            "flag OFF -> sempre sequenziale"
        );
        // Anche con scope vuoti / sovrapposti resta false (irrilevante a flag OFF).
        assert!(!should_isolate_batch(
            false,
            &[sc(&["src/a"]), sc(&["src/a"])]
        ));
        assert!(!should_isolate_batch(false, &[]));
    }

    #[test]
    fn should_isolate_batch_flag_on_solo_se_disgiunti() {
        // Disponibile + scope disgiunti -> ISOLATO.
        assert!(should_isolate_batch(
            true,
            &[sc(&["crates/a"]), sc(&["crates/b"])]
        ));
        // Disponibile ma scope SOVRAPPOSTI -> degrada a sequenziale.
        assert!(!should_isolate_batch(
            true,
            &[sc(&["src/a"]), sc(&["src/a/b"])]
        ));
        // Disponibile ma uno scope VUOTO (nessun write_scope dichiarato: caso di
        // dispatch_wave finche' la colonna DB non e' persistita) -> sequenziale.
        assert!(!should_isolate_batch(true, &[sc(&["src/a"]), sc(&[])]));
        // Disponibile ma scope tocca la denylist (lockfile) -> sequenziale.
        assert!(!should_isolate_batch(
            true,
            &[sc(&["Cargo.lock"]), sc(&["crates/b"])]
        ));
    }

    /// Copertura del ramo DELETE di `reindex_promoted_once` (D3): il branch e'
    /// governato dal segnale strutturato `tokio::fs::try_exists` sul path promosso
    /// (regola M, niente parsing). `reindex_promoted_once` non e' isolabile
    /// (dipende da `AgentToolContext` = DB + neural), quindi si verifica qui il
    /// predicato che seleziona il ramo: un file promosso ma CANCELLATO risulta
    /// `try_exists == Ok(false)` (-> ramo delete via
    /// `vector_memory::delete_code_index_file_points`); un file esistente risulta
    /// `Ok(true)` (-> ramo reindex invariato).
    #[tokio::test]
    async fn reindex_promoted_delete_branch_su_file_inesistente() {
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path();

        // File promosso ma cancellato: NON esiste sul filesystem -> ramo delete.
        let cancellato = root.join("src").join("rimosso.rs");
        assert!(
            !tokio::fs::try_exists(&cancellato).await.unwrap(),
            "un file promosso ma cancellato deve risultare inesistente -> ramo delete"
        );

        // File promosso ancora presente: esiste -> ramo reindex (invariato).
        std::fs::create_dir_all(root.join("src")).expect("mkdir src");
        let presente = root.join("src").join("vivo.rs");
        std::fs::write(&presente, "// vivo\n").expect("write vivo");
        assert!(
            tokio::fs::try_exists(&presente).await.unwrap(),
            "un file promosso ancora presente deve risultare esistente -> ramo reindex"
        );

        // Normalizzazione path relativo a separatori '/' (come indexing.rs), input di
        // delete_code_index_file_points.
        let rel = "src\\rimosso.rs";
        assert_eq!(rel.replace('\\', "/"), "src/rimosso.rs");
    }

    /// Costruttore di comodo per gli eventi step del sub-run nei test del ponte.
    fn step_event(
        status: crate::agent_types::AgentStepStatus,
        tool: &str,
        idx: u32,
        input: Value,
    ) -> crate::agent_types::AgentStepEvent {
        crate::agent_types::AgentStepEvent {
            run_id: "r".into(),
            step: Some(crate::agent_types::AgentStep {
                run_id: "r".into(),
                step_index: idx,
                tool_name: tool.into(),
                tool_input: input,
                tool_result: None,
                status,
                created_at: String::new(),
            }),
            trace: None,
            is_final: false,
            token_delta: None,
            thinking_delta: None,
            meta_step: None,
        }
    }

    /// La decisione d'inoltro guarda SOLO il segnale strutturato dello step
    /// (regola M): conclusi si', Running no, orfani (nome vuoto) no.
    #[test]
    fn concluded_tool_step_solo_esiti_conclusi() {
        use crate::agent_types::AgentStepStatus as S;
        let ok = step_event(S::Completed, "read_file", 3, json!({}));
        assert_eq!(
            concluded_tool_step(&ok),
            Some((false, "read_file".to_string(), 3))
        );
        let ko = step_event(S::Failed, T_RUN, 5, json!({}));
        assert_eq!(concluded_tool_step(&ko), Some((true, T_RUN.to_string(), 5)));
        let running = step_event(S::Running, "read_file", 1, json!({}));
        assert_eq!(concluded_tool_step(&running), None, "Running: doppione");
        let orfano = step_event(S::Completed, "", 2, json!({}));
        assert_eq!(
            concluded_tool_step(&orfano),
            None,
            "senza nome: nessuna riga utile"
        );
        let senza_step = crate::agent_types::AgentStepEvent {
            run_id: "r".into(),
            step: None,
            trace: None,
            is_final: false,
            token_delta: None,
            thinking_delta: None,
            meta_step: None,
        };
        assert_eq!(concluded_tool_step(&senza_step), None);
    }

    /// Il target si estrae dallo step `Running` (il ToolUse porta l'input reale
    /// in `tool_input.input`), col punto unico del grafo (path/command/...).
    #[test]
    fn running_tool_target_dal_tool_use() {
        use crate::agent_types::AgentStepStatus as S;
        let ev = step_event(
            S::Running,
            T_EDIT,
            7,
            json!({"id": "tu_1", "input": {"path": FIX_PATH}}),
        );
        assert_eq!(running_tool_target(&ev), Some((7, FIX_PATH.to_string())));
        // Il Completed non porta l'input reale: nessun target da qui.
        let done = step_event(S::Completed, T_EDIT, 7, json!({"id": "tu_1"}));
        assert_eq!(running_tool_target(&done), None);
    }

    /// Titolo e payload del progresso: stesso registro della narrazione tool del
    /// run corrente, con l'esito dall'is_error strutturato.
    #[test]
    fn tool_progress_meta_compone_titolo_e_payload() {
        let id = Uuid::nil();
        let (title, payload) = tool_progress_meta("coder", id, T_EDIT, Some(FIX_PATH), false);
        assert_eq!(title, "subagente coder: tool edit_file — src/a.rs");
        assert_eq!(payload["phase"], json!("tool"));
        assert_eq!(payload["tool"], json!(T_EDIT));
        assert_eq!(payload[K_TARGET], json!(FIX_PATH));
        assert_eq!(payload[K_IS_ERROR], json!(false));
        assert_eq!(payload[K_SUB_KIND], json!("coder"));
        assert_eq!(payload[K_SUB_RUN_ID], json!(id.to_string()));

        let (title_err, payload_err) = tool_progress_meta("tester", id, T_RUN, None, true);
        assert_eq!(title_err, "subagente tester: errore run_command");
        assert_eq!(payload_err[K_IS_ERROR], json!(true));
        assert_eq!(payload_err[K_TARGET], json!(null));
    }

    /// Il parse dei task del batch estrae `write_scope` dal JSON col punto
    /// unico `write_scope_of` (alimenta il gating di isolamento).
    #[test]
    fn write_scope_parsato_dal_task_json() {
        let t = json!({
            "kind": "coder",
            "task": "x",
            "write_scope": ["crates/a", "docs/a.md"],
        });
        assert_eq!(write_scope_of(&t), sc(&["crates/a", "docs/a.md"]));

        // Task senza write_scope -> vec vuoto -> gating degrada a sequenziale.
        let ws2 = write_scope_of(&json!({ "kind": "coder", "task": "y" }));
        assert!(ws2.is_empty());
        assert!(!should_isolate_batch(true, &[ws2]));
    }

    async fn fetch_worktree_cols(
        pool: &sqlx::PgPool,
        id: Uuid,
    ) -> (Option<String>, Option<String>) {
        sqlx::query_as("SELECT worktree_path, base_commit FROM nexus_subagent_runs WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("fetch cols")
    }

    /// `insert_subagent_run` (query UNICA con COALESCE) deve produrre lo STESSO
    /// esito dei due INSERT storici, blindando il refactor di de-duplicazione
    /// (regola H): ramo sequenziale -> id generato dal DB (DEFAULT) + colonne
    /// worktree NULL; ramo isolato -> id = run_id del worktree + worktree_path/
    /// base_commit persistiti.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn insert_subagent_run_equivalenza_rami(pool: sqlx::PgPool) {
        let row = NewSubagentRun {
            anchor: Uuid::new_v4(),
            dispatcher_run_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            kind: "coder",
            task: "t",
            context_blob: "",
            expected_format: "",
            depth: 1,
            is_background: false,
        };

        // Ramo sequenziale: id dal DB (non nil), colonne worktree NULL.
        let seq_id = insert_subagent_run(&pool, &row, None)
            .await
            .expect("insert seq");
        assert!(!seq_id.is_nil(), "id generato dal DB non nullo");
        assert_eq!(fetch_worktree_cols(&pool, seq_id).await, (None, None));

        // Ramo isolato: id = run_id del worktree, colonne worktree persistite.
        let slot = IsolationSlot {
            run_id: Uuid::new_v4(),
            worktree_path: std::path::PathBuf::from("/tmp/wt"),
            base_commit: "abc123".to_string(),
        };
        let iso_id = insert_subagent_run(&pool, &row, Some(&slot))
            .await
            .expect("insert iso");
        assert_eq!(
            iso_id, slot.run_id,
            "ramo isolato: id = run_id del worktree"
        );
        assert_eq!(
            fetch_worktree_cols(&pool, iso_id).await,
            (
                Some(slot.worktree_path.to_string_lossy().to_string()),
                Some(slot.base_commit)
            ),
        );
    }

    /// Il pin provider/model entra nel payload SOLO quando risolto; vuoto =
    /// campo omesso (il nastro frontend degrada a '?', mai valori inventati).
    #[test]
    fn put_model_fields_solo_quando_risolto() {
        let mut payload = json!({"phase": "tool"});
        put_model_fields(&mut payload, "mistral", "mistral-medium-3");
        assert_eq!(payload[K_PROVIDER], json!("mistral"));
        assert_eq!(payload[K_MODEL], json!("mistral-medium-3"));

        let mut vuoto = json!({"phase": "tool"});
        put_model_fields(&mut vuoto, "", "  ");
        assert!(
            vuoto.get(K_PROVIDER).is_none(),
            "pin non risolto: campo omesso"
        );
        assert!(vuoto.get(K_MODEL).is_none());
    }

    /// Helper: crea il run TRACCIATO del figlio con ids freschi (punto unico
    /// dei call-site di test, evita blocchi duplicati).
    async fn ensure_child(
        pool: &sqlx::PgPool,
        child: Uuid,
        anchor: Uuid,
        provider: &str,
        model: &str,
    ) {
        // La sessione dev'essere REALE: `agent_runs.session_id` e' vincolato da
        // una FK verso `chat_sessions(id)` (la vecchia fixture, dichiaratamente
        // "senza FK di sessione", accettava run appesi al nulla).
        let project = Uuid::new_v4();
        let session = crate::test_support::seed_chat_session(pool, project).await;
        ensure_child_agent_run(
            pool,
            child,
            session,
            project,
            Uuid::new_v4(),
            anchor,
            provider,
            model,
        )
        .await;
    }

    /// Il sub-run diventa un run TRACCIATO (riga agent_runs marcata
    /// 'subagent'): senza, il guard untracked_run dello step store scartava in
    /// silenzio ogni tool_result del figlio (incidente 2026-07-06). L'ancora
    /// NON tracciata (sessione) NON deve violare la FK: parent_run_id resta
    /// NULL via subquery.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn ensure_child_agent_run_traccia_il_figlio(pool: sqlx::PgPool) {
        let child = Uuid::new_v4();
        let anchor_non_tracciato = Uuid::new_v4(); // sessione: NON in agent_runs
        ensure_child(
            &pool,
            child,
            anchor_non_tracciato,
            "mistral",
            "mistral-medium-3",
        )
        .await;
        let row: (String, Option<String>, Option<Uuid>, Option<String>) = sqlx::query_as(
            "SELECT status, nexus_agent_type, parent_run_id, model FROM agent_runs WHERE id = $1",
        )
        .bind(child)
        .fetch_one(&pool)
        .await
        .expect("riga figlio presente");
        assert_eq!(row.0, "running");
        assert_eq!(row.1.as_deref(), Some("subagent"));
        assert_eq!(
            row.2, None,
            "ancora non tracciata -> parent NULL (no FK rotta)"
        );
        assert_eq!(row.3.as_deref(), Some("mistral-medium-3"));

        // Idempotente sul retry (ON CONFLICT DO NOTHING).
        ensure_child(&pool, child, anchor_non_tracciato, "", "").await;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_runs WHERE id = $1")
            .bind(child)
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 1);
    }

    /// Con l'ancora TRACCIATA (run padre reale) parent_run_id viene valorizzato:
    /// e' il collegamento padre<->figlio per audit e query.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn ensure_child_agent_run_collega_il_padre_tracciato(pool: sqlx::PgPool) {
        let parent = crate::test_support::seed_agent_run(&pool).await;
        let child = Uuid::new_v4();
        ensure_child(&pool, child, parent, "google", "gemini-2.5-flash").await;
        let linked: Option<Uuid> =
            sqlx::query_scalar("SELECT parent_run_id FROM agent_runs WHERE id = $1")
                .bind(child)
                .fetch_one(&pool)
                .await
                .expect("figlio");
        assert_eq!(linked, Some(parent));
    }

    /// Semina un padre tracciato su `provider` e due modelli 'light' capable
    /// (worker-provider + un alternativo) + il purpose tier-only. Ritorna
    /// (parent_run_id, definition_reviewer).
    async fn seed_review_routing(
        pool: &sqlx::PgPool,
        parent_provider: &str,
        catalog: &[(&str, &str)],
    ) -> (Uuid, SubagentDefinition) {
        // Il DB di test di questo modulo e' bare (#[sqlx::test] non applica le
        // migrazioni meta): creo le tabelle di routing come fanno gli altri test.
        crate::test_support::create_ai_price_catalog_table(pool).await;
        sqlx::query(
            "CREATE TABLE nexus_purpose_model ( \
                 purpose TEXT PRIMARY KEY, \
                 tier TEXT, \
                 required_capability TEXT, \
                 requires_tool_use BOOLEAN NOT NULL DEFAULT false \
             )",
        )
        .execute(pool)
        .await
        .expect("create nexus_purpose_model");
        // Padre TRACCIATO su una sessione reale (FK agent_runs.session_id).
        let project = Uuid::new_v4();
        let session = crate::test_support::seed_chat_session(pool, project).await;
        let parent =
            crate::test_support::insert_agent_run(pool, session, project, "running").await;
        sqlx::query("UPDATE agent_runs SET provider = $2, model = 'pm' WHERE id = $1")
            .bind(parent)
            .bind(parent_provider)
            .execute(pool)
            .await
            .expect("provider/model del padre");
        for (prov, model) in catalog {
            sqlx::query(
                "INSERT INTO ai_price_catalog \
                 (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, \
                  input_cost_per_million_tokens) VALUES ($1, $2, true, 'none', 'light', 1.0)",
            )
            .bind(prov)
            .bind(model)
            .execute(pool)
            .await
            .expect("catalog");
        }
        // Purpose tier-only: requires_tool_use=true -> ramo agentico (select_agentic_model).
        sqlx::query(
            "INSERT INTO nexus_purpose_model (purpose, tier, requires_tool_use) \
             VALUES ('reviewer', 'light', true)",
        )
        .execute(pool)
        .await
        .expect("purpose");
        let def = SubagentDefinition {
            prompt_key: String::new(),
            tool_whitelist: vec![],
            model_purpose: "reviewer".to_string(),
            timeout_s: 0,
        };
        (parent, def)
    }

    /// C2: un sub-run `kind == "review"` risolve il modello ESCLUDENDO il provider
    /// del padre (worker) -> il giudice gira su un provider diverso.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn review_esclude_il_provider_del_worker(pool: sqlx::PgPool) {
        let (parent, def) =
            seed_review_routing(&pool, "alpha", &[("alpha", "a1"), ("beta", "b1")]).await;
        let (provider, _model) =
            resolve_worker_model(&pool, &pool, "review", &def, parent, Uuid::new_v4()).await;
        assert_eq!(
            provider, "beta",
            "il review deve evitare il provider del worker (alpha)"
        );
    }

    /// C2 fallback: se il provider del worker e' l'UNICO capable, il review gira
    /// comunque su quel provider (preferenza forte, non hard filter).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn review_fallback_se_unico_provider_capable(pool: sqlx::PgPool) {
        let (parent, def) = seed_review_routing(&pool, "alpha", &[("alpha", "a1")]).await;
        let (provider, _model) =
            resolve_worker_model(&pool, &pool, "review", &def, parent, Uuid::new_v4()).await;
        assert_eq!(
            provider, "alpha",
            "unico provider capable -> fallback senza esclusione"
        );
    }

    /// Il revisore di INTERFACCIA e' un giudice quanto quello generico, e la
    /// regola vale per lui allo stesso modo: nel panel di review i due siedono
    /// affiancati, e uno solo dei due che si astiene dall'auto-certificazione
    /// non e' una regola, e' un caso.
    ///
    /// MUTAZIONE: riportando `e_un_giudice` a `kind == "review"` questo test
    /// ritorna "alpha", cioe' il fornitore che ha scritto il codice.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn il_revisore_di_interfaccia_esclude_il_provider_del_worker(pool: sqlx::PgPool) {
        let (parent, def) =
            seed_review_routing(&pool, "alpha", &[("alpha", "a1"), ("beta", "b1")]).await;
        let (provider, _model) =
            resolve_worker_model(&pool, &pool, UI_REVIEWER_KIND, &def, parent, Uuid::new_v4())
                .await;
        assert_eq!(
            provider, "beta",
            "il revisore di interfaccia deve evitare il provider del worker (alpha)"
        );
    }

    /// C2 parita': un kind NON review non esclude nulla (il provider del padre e'
    /// ammesso, comportamento invariato).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn non_review_non_esclude_il_provider_del_padre(pool: sqlx::PgPool) {
        // Solo 'alpha' capable; un worker (kind implement) risolve su 'alpha'
        // senza alcuna esclusione anche se il padre e' 'alpha'.
        let (parent, mut def) = seed_review_routing(&pool, "alpha", &[("alpha", "a1")]).await;
        def.model_purpose = "reviewer".to_string();
        let (provider, _model) =
            resolve_worker_model(&pool, &pool, "implement", &def, parent, Uuid::new_v4()).await;
        assert_eq!(provider, "alpha", "kind non-review: nessuna esclusione");
    }

    // ── DIVERSITA' PROVIDER FRA LE FIGURE DEL CONSIGLIO ───────────────────────
    //
    // Difetto 20/07: due figure con lo stesso purpose tier risolvevano in
    // parallelo e in isolamento -> stesso provider E stesso modello
    // (openrouter/qwen3-235b su software_architect + security_engineer).
    // `resolve_council_assignments` decide PRIMA del fan-out, in sequenza.

    /// Tabella `nexus_subagent_definitions` per i kind delle figure (il DB dei
    /// test e' bare). Tutte sul MEDESIMO purpose: e' il caso del difetto.
    async fn seed_figure_definitions(pool: &sqlx::PgPool, kinds: &[&str], purpose: &str) {
        sqlx::query(
            "CREATE TABLE nexus_subagent_definitions ( \
                 kind TEXT PRIMARY KEY, \
                 prompt_key TEXT NOT NULL, \
                 tool_whitelist TEXT[] NOT NULL DEFAULT '{}', \
                 model_purpose TEXT, \
                 timeout_s INTEGER NOT NULL DEFAULT 0, \
                 is_enabled BOOLEAN NOT NULL DEFAULT true \
             )",
        )
        .execute(pool)
        .await
        .expect("create nexus_subagent_definitions");
        for kind in kinds {
            sqlx::query(
                "INSERT INTO nexus_subagent_definitions (kind, prompt_key, model_purpose) \
                 VALUES ($1, 'p', $2)",
            )
            .bind(kind)
            .bind(purpose)
            .execute(pool)
            .await
            .expect("definition");
        }
    }

    /// Due figure, due provider capable -> assegnazioni DISTINTE: la seconda
    /// figura esclude il provider gia' dato alla prima.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn figure_del_consiglio_su_provider_distinti(pool: sqlx::PgPool) {
        let (_parent, _def) =
            seed_review_routing(&pool, "alpha", &[("alpha", "a1"), ("beta", "b1")]).await;
        seed_figure_definitions(&pool, &["software_architect", "security_engineer"], "reviewer")
            .await;
        let kinds = vec![
            "software_architect".to_string(),
            "security_engineer".to_string(),
        ];
        let out = resolve_council_assignments(&pool, &pool, Some(Uuid::new_v4()), &kinds).await;
        let providers: Vec<String> = out
            .iter()
            .map(|o| o.as_ref().expect("figura assegnata").0.clone())
            .collect();
        assert_ne!(
            providers[0], providers[1],
            "due figure devono girare su provider DISTINTI: {providers:?} \
             (era il difetto: entrambe sul piu' economico eleggibile)"
        );
    }

    /// Pool monoprovider: la seconda figura tiene il DUPLICATO invece di saltare
    /// (preferenza forte, non hard filter: meglio un parere in piu' che una
    /// figura in meno).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn consiglio_monoprovider_duplica_invece_di_perdere_figure(pool: sqlx::PgPool) {
        let (_parent, _def) = seed_review_routing(&pool, "alpha", &[("alpha", "a1")]).await;
        seed_figure_definitions(&pool, &["software_architect", "security_engineer"], "reviewer")
            .await;
        let kinds = vec![
            "software_architect".to_string(),
            "security_engineer".to_string(),
        ];
        let out = resolve_council_assignments(&pool, &pool, Some(Uuid::new_v4()), &kinds).await;
        let providers: Vec<String> = out
            .iter()
            .map(|o| o.as_ref().expect("figura assegnata").0.clone())
            .collect();
        assert_eq!(
            providers,
            vec!["alpha".to_string(), "alpha".to_string()],
            "unico provider capable -> entrambe su alpha, nessuna figura persa"
        );
    }

    /// Pin di sessione presente -> NESSUNA pre-assegnazione: il pin si propaga
    /// ai subagenti per scelta deliberata e la diversita' non si applica.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn pin_di_sessione_disattiva_la_diversita(pool: sqlx::PgPool) {
        let (_parent, _def) =
            seed_review_routing(&pool, "alpha", &[("alpha", "a1"), ("beta", "b1")]).await;
        seed_figure_definitions(&pool, &["software_architect", "security_engineer"], "reviewer")
            .await;
        let session = Uuid::new_v4();
        seed_session_pin(&pool, session, Some("alpha")).await;
        let kinds = vec![
            "software_architect".to_string(),
            "security_engineer".to_string(),
        ];
        let out = resolve_council_assignments(&pool, &pool, Some(session), &kinds).await;
        assert!(
            out.iter().all(Option::is_none),
            "col pin di sessione le figure seguono il percorso storico: {out:?}"
        );
    }

    // ── PROPAGAZIONE PIN PROVIDER AI SUB-AGENTI WORKER ────────────────────────
    //
    // Un catalog reale per il pin: tier 'medium' + capability 'code' + tool_use
    // (il purpose worker 'frontend'). Colonna di catalog: `capabilities` jsonb
    // (la capability 'code' non ha colonna dedicata -> `capabilities @> ["code"]`).

    /// Riga di catalog completa per i test pin: provider/model, tier, capability
    /// 'code' (jsonb), costo (per il cost-first ASC). `enabled=true`.
    async fn seed_catalog_row(
        pool: &sqlx::PgPool,
        provider: &str,
        model: &str,
        tier: &str,
        cost: f64,
    ) {
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
              performance_tier, capabilities, input_cost_per_million_tokens) \
             VALUES ($1, $2, true, true, 'none', $3, '[\"code\"]'::jsonb, $4)",
        )
        .bind(provider)
        .bind(model)
        .bind(tier)
        .bind(cost)
        .execute(pool)
        .await
        .expect("catalog row");
    }

    /// Semina catalog + purpose worker + tabella chat_sessions per i test pin.
    /// `catalog`: (provider, model, tier, cost). Il purpose 'frontend' e' tier
    /// 'medium' + capability 'code' + tool_use (ramo agentico, cost-first). Ritorna
    /// il session_id (per il pin) e la definition worker.
    async fn seed_pin_routing(
        pool: &sqlx::PgPool,
        catalog: &[(&str, &str, &str, f64)],
    ) -> (Uuid, SubagentDefinition) {
        crate::test_support::create_ai_price_catalog_table(pool).await;
        sqlx::query(
            "CREATE TABLE nexus_purpose_model ( \
                 purpose TEXT PRIMARY KEY, \
                 tier TEXT, \
                 required_capability TEXT, \
                 requires_tool_use BOOLEAN NOT NULL DEFAULT false \
             )",
        )
        .execute(pool)
        .await
        .expect("create nexus_purpose_model");
        for (prov, model, tier, cost) in catalog {
            seed_catalog_row(pool, prov, model, tier, *cost).await;
        }
        sqlx::query(
            "INSERT INTO nexus_purpose_model (purpose, tier, required_capability, requires_tool_use) \
             VALUES ('frontend', 'medium', 'code', true)",
        )
        .execute(pool)
        .await
        .expect("purpose frontend");
        let def = SubagentDefinition {
            prompt_key: String::new(),
            tool_whitelist: vec![],
            model_purpose: "frontend".to_string(),
            timeout_s: 0,
        };
        (Uuid::new_v4(), def)
    }

    /// Inserisce una sessione con `preferred_provider` = pin. `project_id` e'
    /// NOT NULL nello schema reale (la vecchia fixture a due colonne lo ignorava).
    async fn seed_session_pin(pool: &sqlx::PgPool, session_id: Uuid, pin: Option<&str>) {
        sqlx::query(
            "INSERT INTO chat_sessions (id, project_id, preferred_provider) \
             VALUES ($1, gen_random_uuid(), $2)",
        )
        .bind(session_id)
        .bind(pin)
        .execute(pool)
        .await
        .expect("session pin");
    }

    /// TEST 1 — pin propagato felice: pin='deepseek' + purpose worker medium/code/
    /// tool_use -> ritorna il modello DEEPSEEK del tier (non mistral, non il light).
    /// Senza pin vincerebbe mistral (piu' economico): il pin sposta la scelta.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn pin_propagato_sceglie_provider_pinnato(pool: sqlx::PgPool) {
        let (session, def) = seed_pin_routing(
            &pool,
            &[
                ("mistral", "mistral-medium", "medium", 1.0),
                ("deepseek", "deepseek-medium", "medium", 2.0),
                ("deepseek", "deepseek-flash", "light", 0.5),
            ],
        )
        .await;
        seed_session_pin(&pool, session, Some("deepseek")).await;
        let (provider, model) =
            resolve_worker_model(&pool, &pool, "implement", &def, Uuid::new_v4(), session).await;
        assert_eq!(
            provider, "deepseek",
            "il pin deve vincere il cost-first (mistral piu' economico)"
        );
        assert_eq!(
            model, "deepseek-medium",
            "tier medium rispettato (non il light deepseek-flash)"
        );
    }

    /// TEST 2 — pin non-capable -> degrado: il provider pinnato non ha un modello
    /// medium+code+tool_use -> NoCapableModel col pin -> fallback SENZA pin ->
    /// modello purpose normale (mistral). MAI ("","").
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn pin_non_capable_degrada_al_purpose_normale(pool: sqlx::PgPool) {
        // deepseek ha solo un modello 'light' (tier sbagliato): non capable per
        // il tier 'medium' del purpose. mistral ha il medium capable.
        let (session, def) = seed_pin_routing(
            &pool,
            &[
                ("mistral", "mistral-medium", "medium", 1.0),
                ("deepseek", "deepseek-flash", "light", 0.5),
            ],
        )
        .await;
        seed_session_pin(&pool, session, Some("deepseek")).await;
        let (provider, model) =
            resolve_worker_model(&pool, &pool, "implement", &def, Uuid::new_v4(), session).await;
        assert_eq!(
            provider, "mistral",
            "pin non-capable -> fallback senza pin al purpose normale"
        );
        assert_eq!(model, "mistral-medium");
        assert!(
            !provider.is_empty() && !model.is_empty(),
            "mai (\"\",\"\") se il purpose e' risolvibile"
        );
    }

    /// TEST 3 — pin in cooldown -> degrado: il provider pinnato e' capable ma in
    /// cooldown (escluso dalla query) -> query pinnata vuota -> fallback senza pin
    /// (il figlio non resta bloccato).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn pin_in_cooldown_degrada_senza_bloccare(pool: sqlx::PgPool) {
        // Provider con nomi DEDICATI: il cooldown e' uno snapshot GLOBALE in-memory
        // condiviso tra i test paralleli, quindi non riuso 'deepseek'/'mistral'
        // (evita interferenze cross-test). Il pin va sul provider pinnato-in-cooldown.
        let pinned = "pincd-provider";
        let fallback = "fbcd-provider";
        let (session, def) = seed_pin_routing(
            &pool,
            &[
                (fallback, "fb-medium", "medium", 1.0),
                (pinned, "pin-medium", "medium", 2.0),
            ],
        )
        .await;
        seed_session_pin(&pool, session, Some(pinned)).await;
        // Cooldown in-memory sul provider pinnato: la WHERE `apply_cooldown` lo esclude.
        crate::provider_cooldown::put_provider_in_cooldown(pinned, Some(300));
        let (provider, model) =
            resolve_worker_model(&pool, &pool, "implement", &def, Uuid::new_v4(), session).await;
        // Rimuovo il cooldown per non contaminare altri test (stato globale).
        crate::provider_cooldown::remove_cooldown(pinned);
        assert_eq!(provider, fallback, "pin in cooldown -> fallback senza pin");
        assert_eq!(model, "fb-medium");
    }

    /// TEST 5 — Auto (nessun pin) bit-identico: preferred_provider NULL -> stesso
    /// (provider, model) del comportamento pre-pin (il cost-first sceglie mistral).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn nessun_pin_bit_identico_al_comportamento_attuale(pool: sqlx::PgPool) {
        let (session, def) = seed_pin_routing(
            &pool,
            &[
                ("mistral", "mistral-medium", "medium", 1.0),
                ("deepseek", "deepseek-medium", "medium", 2.0),
            ],
        )
        .await;
        seed_session_pin(&pool, session, None).await;
        let (provider, model) =
            resolve_worker_model(&pool, &pool, "implement", &def, Uuid::new_v4(), session).await;
        // Senza pin, il cost-first (ASC) sceglie il piu' economico: mistral.
        assert_eq!(provider, "mistral");
        assert_eq!(model, "mistral-medium");
    }

    /// TEST 4 — review ignora il pin: kind='review', pin = provider del padre ->
    /// il modello risolto NON e' del provider padre (l'esclusione giudice != worker
    /// vince sul pin). Nota: qui il purpose e' tier-only agentico su due provider.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn review_ignora_il_pin_esclusione_vince(pool: sqlx::PgPool) {
        let (parent, def) =
            seed_review_routing(&pool, "alpha", &[("alpha", "a1"), ("beta", "b1")]).await;
        // La sessione ha pinnato ALPHA (lo stesso del padre). Se il pin fosse
        // rispettato il review girerebbe su alpha; l'esclusione deve prevalere.
        let session = Uuid::new_v4();
        seed_session_pin(&pool, session, Some("alpha")).await;
        let (provider, _model) =
            resolve_worker_model(&pool, &pool, "review", &def, parent, session).await;
        assert_eq!(
            provider, "beta",
            "review: il pin e' ignorato, l'esclusione del padre vince"
        );
    }

    /// `mark_run` chiude ENTRAMBE le righe del figlio (nexus_subagent_runs +
    /// gemella agent_runs) nella stessa statement, con status/metriche coerenti.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn mark_run_chiude_anche_la_riga_agent_runs(pool: sqlx::PgPool) {
        let child = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO nexus_subagent_runs \
                 (id, parent_run_id, project_id, kind, task_description, status) \
             VALUES ($1, gen_random_uuid(), gen_random_uuid(), 'coder', 'task di test', \
                     'running')",
        )
        .bind(child)
        .execute(&pool)
        .await
        .expect("sub-run");
        ensure_child(&pool, child, Uuid::new_v4(), "mistral", "mistral-medium-3").await;
        mark_run(
            &pool,
            child,
            SubRunClosure {
                status: "completed",
                summary: "fatto",
                iterations: 7,
                tokens_prompt: 1000,
                tokens_completion: 200,
                cost_usd: 0.05,
                verdict: json!({"verdict": "completed", "success": true}),
            },
        )
        .await
        .expect("mark ok");
        let sub: (String, Option<String>, i32, Option<Value>) = sqlx::query_as(
            "SELECT status, final_summary, iterations, verdict \
             FROM nexus_subagent_runs WHERE id = $1",
        )
        .bind(child)
        .fetch_one(&pool)
        .await
        .expect("sub");
        assert_eq!(sub.0, "completed");
        assert_eq!(sub.1.as_deref(), Some("fatto"));
        assert_eq!(sub.2, 7);
        // Il verdetto strutturato (regola M) e' persistito SOLO sulla riga sub:
        // e' il canale del fan-in asincrono (poll) e dei coordinatori.
        let verdict = sub.3.expect("verdict persistito");
        assert_eq!(verdict["verdict"], json!("completed"));
        assert_eq!(verdict["success"], json!(true));
        let run: (String, Option<String>, i32, i32, bool) = sqlx::query_as(
            "SELECT status, final_answer, iteration_count, total_tokens, \
             completed_at IS NOT NULL FROM agent_runs WHERE id = $1",
        )
        .bind(child)
        .fetch_one(&pool)
        .await
        .expect("run gemello");
        assert_eq!(run.0, "completed");
        assert_eq!(run.1.as_deref(), Some("fatto"));
        assert_eq!(run.2, 7);
        assert_eq!(run.3, 1200, "total = prompt + completion");
        assert!(run.4, "completed_at valorizzato");
    }

    /// GUARD di forma (regola L): il blocco esito dei rami TERMINALI
    /// (`terminal_verdict`, senza `NativeRunOutcome`) e quello del ramo normale
    /// (`NativeRunOutcome::structured_verdict`) devono avere le STESSE chiavi —
    /// il coordinatore e il poll leggono un unico schema. Un campo aggiunto a
    /// uno solo dei due (com'era successo con `review` in Fase B) e' un drift
    /// silenzioso: questo test lo rende un errore di build.
    #[test]
    fn terminal_verdict_stessa_forma_di_structured_verdict() {
        use std::collections::BTreeSet;
        let outcome = crate::native_engine::NativeRunOutcome {
            completed: true,
            awaiting_subagents: false,
            final_answer: None,
            stop_reason: None,
            provider_used: None,
            model_used: None,
            resume_at: None,
            iterations: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            total_cost: 0.0,
            user_intent: None,
            reasoning: None,
            messages_json: None,
            declared_outcome: None,
            review_verdict: None,
            advisory_verdict: None,
            debate_position: None,
            error_class: None,
            provider_error_close: false,
            forced_close_unverified: false,
            final_gate_passed: None,
            final_gate_unverified: None,
            final_gate_failed_pending: false,
            review_panel_rejected: false,
            review_panel_no_correction: false,
            review_panel_last: None,
            pending_actions: Vec::new(),
            council_requirements: Vec::new(),
            council_conformance: None,
        };
        let keys = |v: &Value| -> BTreeSet<String> {
            v.as_object()
                .expect("blocco esito e' un oggetto")
                .keys()
                .cloned()
                .collect()
        };
        assert_eq!(
            keys(&terminal_verdict("failed", "engine_error")),
            keys(&outcome.structured_verdict()),
            "terminal_verdict e structured_verdict hanno chiavi divergenti"
        );
    }

    /// Schema minimo (bare DB del `#[sqlx::test]`) per il fan-in: `nexus_subagent_
    /// runs` con `parent_run_id`/`is_background`/`status` + la coda META. In un DB
    /// reale i due vivono su pool diversi (project/meta); qui lo stesso pool fa da
    /// entrambi: la funzione riceve i due handle separati e il test passa lo
    /// stesso, cosi' si verifica la meccanica SQL (COUNT + INSERT ON CONFLICT).
    async fn create_fanin_tables(pool: &sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE subagent_fanin_resume_queue ( \
                 parent_run_id UUID PRIMARY KEY, \
                 project_id UUID NOT NULL, \
                 session_id UUID NOT NULL, \
                 enqueued_at TIMESTAMPTZ NOT NULL DEFAULT NOW() )",
        )
        .execute(pool)
        .await
        .expect("create subagent_fanin_resume_queue");
    }

    /// Inserisce un figlio con `parent_run_id = dispatcher_run_id = dispatcher`
    /// (il caso comune: dispatcher e anchor coincidono nei test non-annidati).
    async fn insert_child(pool: &sqlx::PgPool, dispatcher: Uuid, is_bg: bool, status: &str) {
        insert_child_of(pool, dispatcher, dispatcher, is_bg, status).await;
    }

    /// Inserisce un figlio distinguendo `parent_run_id` (anchor depth-chain) da
    /// `dispatcher_run_id` (run che ha dispatchato): serve al test di ANNIDAMENTO,
    /// dove i nipoti hanno lo STESSO anchor dei figli (session_id) ma un dispatcher
    /// diverso (il figlio annidato).
    async fn insert_child_of(
        pool: &sqlx::PgPool,
        anchor: Uuid,
        dispatcher: Uuid,
        is_bg: bool,
        status: &str,
    ) {
        sqlx::query(
            "INSERT INTO nexus_subagent_runs \
                 (parent_run_id, dispatcher_run_id, project_id, kind, task_description, \
                  is_background, status) \
             VALUES ($1, $2, gen_random_uuid(), 'coder', 'task di test', $3, $4)",
        )
        .bind(anchor)
        .bind(dispatcher)
        .bind(is_bg)
        .bind(status)
        .execute(pool)
        .await
        .expect("insert child");
    }

    async fn queue_len(pool: &sqlx::PgPool, parent: Uuid) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM subagent_fanin_resume_queue WHERE parent_run_id = $1",
        )
        .bind(parent)
        .fetch_one(pool)
        .await
        .expect("count queue")
    }

    /// Con un figlio background ancora `running`, l'enqueue NON scatta: si aspetta
    /// l'ULTIMO. Quando tutti i background sono terminali, accoda (idempotente).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn fanin_enqueue_solo_quando_tutti_background_terminali(pool: sqlx::PgPool) {
        create_fanin_tables(&pool).await;
        let parent = Uuid::new_v4();
        let project = Uuid::new_v4();
        let session = Uuid::new_v4();

        // Due figli background: uno completed, uno ancora running.
        insert_child(&pool, parent, true, "completed").await;
        insert_child(&pool, parent, true, "running").await;
        // Un figlio NON-background terminale non deve influenzare il conteggio.
        insert_child(&pool, parent, false, "completed").await;

        // Ancora uno running -> non accoda.
        let enq = fanin_enqueue_if_last(&pool, &pool, parent, project, session)
            .await
            .expect("enqueue query ok");
        assert!(!enq, "con un background ancora running non deve accodare");
        assert_eq!(queue_len(&pool, parent).await, 0);

        // L'ultimo background diventa terminale.
        sqlx::query("UPDATE nexus_subagent_runs SET status = 'timeout' WHERE dispatcher_run_id = $1 AND status = 'running'")
            .bind(parent)
            .execute(&pool)
            .await
            .expect("chiudi ultimo bg");

        // Ora tutti terminali -> accoda.
        let enq2 = fanin_enqueue_if_last(&pool, &pool, parent, project, session)
            .await
            .expect("enqueue query ok");
        assert!(enq2, "tutti i background terminali -> accoda");
        assert_eq!(queue_len(&pool, parent).await, 1);

        // Idempotente: un secondo enqueue non duplica (PK parent_run_id).
        let enq3 = fanin_enqueue_if_last(&pool, &pool, parent, project, session)
            .await
            .expect("enqueue query ok");
        assert!(enq3, "ancora tutti terminali -> ritorna true");
        assert_eq!(
            queue_len(&pool, parent).await,
            1,
            "nessun duplicato in coda"
        );
    }

    /// BUG #1 (ALTA FATALE): l'enqueue deve accodare il RUN CORRENTE
    /// (`resume_run_id`), NON l'`anchor` depth-chain. I figli sono registrati con
    /// `parent_run_id = anchor` (COUNT su anchor), ma la coda deve puntare al run
    /// sospeso su `awaiting_subagents`, che il CAS del worker cerca in `agent_runs.
    /// id`. Se accodasse l'anchor (= session_id per i sub-run di primo livello) il
    /// CAS non troverebbe mai il run -> padre appeso per sempre.
    ///
    /// Senza il fix `fanin_enqueue_if_last` accodava `parent_run_id` (unico id, =
    /// anchor): questo test — con anchor != resume_run_id — fallirebbe perche' in
    /// coda ci sarebbe l'anchor e il CAS sul run corrente non lo troverebbe.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn fanin_enqueue_usa_run_corrente_non_anchor(pool: sqlx::PgPool) {
        create_fanin_tables(&pool).await;

        // anchor (famiglia figli) e run corrente (sospeso) DISTINTI: e' il caso del
        // run principale (anchor = session_id, run corrente = agent_runs.id). La
        // sessione e' reale: `agent_runs.session_id` ha una FK verso `chat_sessions`.
        let project = Uuid::new_v4();
        let session = crate::test_support::seed_chat_session(&pool, project).await;
        let anchor = session; // per un run di primo livello l'anchor E' la sessione
        // Il run corrente e' sospeso su awaiting_subagents (lo marca il finalize).
        let resume_run_id = crate::test_support::insert_agent_run(
            &pool,
            session,
            project,
            "awaiting_subagents",
        )
        .await;

        // Un solo figlio background, gia' terminale: e' l'ultimo -> accoda. Il
        // figlio ha anchor (session_id) DISTINTO dal dispatcher (run corrente):
        // e' proprio il caso reale di un run principale (ctx.parent_run_id=None ->
        // anchor=session_id; ctx.run_id=resume_run_id -> dispatcher).
        insert_child_of(&pool, anchor, resume_run_id, true, "completed").await;

        let enq = fanin_enqueue_if_last(&pool, &pool, resume_run_id, project, session)
            .await
            .expect("enqueue query ok");
        assert!(enq, "ultimo background terminale -> accoda");

        // La coda deve contenere il RUN CORRENTE, non l'anchor.
        let queued_resume: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM subagent_fanin_resume_queue WHERE parent_run_id = $1",
        )
        .bind(resume_run_id)
        .fetch_one(&pool)
        .await
        .expect("count resume");
        assert_eq!(queued_resume, 1, "la coda deve puntare al run CORRENTE");
        let queued_anchor: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM subagent_fanin_resume_queue WHERE parent_run_id = $1",
        )
        .bind(anchor)
        .fetch_one(&pool)
        .await
        .expect("count anchor");
        assert_eq!(queued_anchor, 0, "la coda NON deve puntare all'anchor");

        // Il CAS del worker (awaiting_subagents -> running su parent_run_id) trova
        // il run corrente accodato: senza il fix cercherebbe l'anchor e mancherebbe.
        let cas: Option<Uuid> = sqlx::query_scalar(
            "UPDATE agent_runs SET status = 'running' \
             WHERE id = (SELECT parent_run_id FROM subagent_fanin_resume_queue LIMIT 1) \
               AND status = 'awaiting_subagents' RETURNING id",
        )
        .fetch_optional(&pool)
        .await
        .expect("cas ok");
        assert_eq!(
            cas,
            Some(resume_run_id),
            "il CAS deve trovare e vincere sul run corrente accodato"
        );
    }

    /// BUG #4 (ALTA): nel batch background le row vanno INSERITE tutte PRIMA di
    /// spawnare, altrimenti il 1o figlio che finisce vede COUNT=0 (gli altri non
    /// ancora inseriti) e accoda il parent PREMATURAMENTE. Questo test riproduce la
    /// meccanica al livello di `fanin_enqueue_if_last`: con un solo figlio inserito
    /// (scenario insert-one-at-a-time) l'enqueue scatta (rischio); con TUTTE le row
    /// inserite PRIMA (come fa `run_batch_background` prepare-all/spawn-all) il 1o
    /// che termina NON accoda (ne restano 2).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn fanin_batch_background_no_enqueue_prematuro(pool: sqlx::PgPool) {
        create_fanin_tables(&pool).await;
        let anchor = Uuid::new_v4();
        let project = Uuid::new_v4();
        let session = Uuid::new_v4();

        // SCENARIO BUGGY (insert-one-at-a-time): solo il 1o figlio e' inserito, poi
        // "termina". Con COUNT sui soli inseriti, remaining=0 -> enqueue prematuro.
        insert_child(&pool, anchor, true, "completed").await;
        let premature = fanin_enqueue_if_last(&pool, &pool, anchor, project, session)
            .await
            .expect("enqueue query ok");
        assert!(
            premature,
            "con un solo figlio inserito la COUNT dice 0 rimasti (mostra il rischio)"
        );

        // Ripulisco e ricreo lo SCENARIO FIXATO (prepare-all): TUTTI e 3 i figli
        // inseriti PRIMA che il 1o termini.
        sqlx::query("DELETE FROM subagent_fanin_resume_queue")
            .execute(&pool)
            .await
            .expect("clear queue");
        sqlx::query("DELETE FROM nexus_subagent_runs")
            .execute(&pool)
            .await
            .expect("clear children");
        insert_child(&pool, anchor, true, "running").await;
        insert_child(&pool, anchor, true, "running").await;
        insert_child(&pool, anchor, true, "running").await;
        // Il 1o figlio finisce (finalize -> mark_run terminale).
        sqlx::query(
            "UPDATE nexus_subagent_runs SET status = 'completed' WHERE ctid IN \
             (SELECT ctid FROM nexus_subagent_runs WHERE dispatcher_run_id = $1 AND status = 'running' LIMIT 1)",
        )
        .bind(anchor)
        .execute(&pool)
        .await
        .expect("chiudi 1o figlio");
        let after_first = fanin_enqueue_if_last(&pool, &pool, anchor, project, session)
            .await
            .expect("enqueue query ok");
        assert!(
            !after_first,
            "con tutte le row gia' inserite, il 1o figlio che chiude NON accoda (ne restano 2)"
        );
        assert_eq!(
            queue_len(&pool, anchor).await,
            0,
            "nessun enqueue prematuro col fix prepare-all"
        );
    }

    /// SCENARIO 2 (ALTA 1, isolamento per-run della COUNT): con l'annidamento entro
    /// subagent_max_depth, P dispatcha Cp1,Cp2 (dispatcher = P) e Cp1 dispatcha Cs1
    /// (dispatcher = Cp1). TUTTI condividono `parent_run_id = session` (anchor
    /// degenere). Quando Cp1,Cp2 terminano ma Cs1 e' ANCORA running, la COUNT di P
    /// (dispatcher = P) deve essere 0 -> P si accoda; il nipote Cs1 (dispatcher =
    /// Cp1) NON entra nella COUNT di P. Senza il fix (COUNT su `parent_run_id =
    /// anchor = session`) Cs1 conterebbe come figlio di P -> P NON si accoderebbe
    /// (solo il backstop lo salverebbe) e il fetch inietterebbe il nipote.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn fanin_annidamento_count_isolata_per_dispatcher(pool: sqlx::PgPool) {
        create_fanin_tables(&pool).await;
        let session = Uuid::new_v4(); // anchor condiviso da TUTTI i sub-run
        let p_run = Uuid::new_v4();
        let cp1_run = Uuid::new_v4();
        let project = Uuid::new_v4();

        // Cp1, Cp2: figli DIRETTI di P (anchor=session, dispatcher=P), terminali.
        insert_child_of(&pool, session, p_run, true, "completed").await;
        insert_child_of(&pool, session, p_run, true, "completed").await;
        // Cs1: NIPOTE, figlio DIRETTO di Cp1 (anchor=session, dispatcher=Cp1),
        // ANCORA running (Cp1 e' detached e vivo, entro max_depth=2).
        insert_child_of(&pool, session, cp1_run, true, "running").await;

        // COUNT di P (dispatcher=P): i suoi 2 figli sono terminali, Cs1 (dispatcher
        // diverso) NON conta -> P si ACCODA.
        let enq_p = fanin_enqueue_if_last(&pool, &pool, p_run, project, session)
            .await
            .expect("enqueue P ok");
        assert!(
            enq_p,
            "ALTA 1: COUNT di P isolata per dispatcher -> P si accoda anche col nipote Cs1 running"
        );
        assert_eq!(queue_len(&pool, p_run).await, 1, "in coda c'e' P");

        // Contro-prova: la COUNT su `parent_run_id = session` (vecchio filtro rotto)
        // vedrebbe Cs1 running -> NON accoderebbe. Verifico che i sub-run con quel
        // filtro siano 3 (P conterebbe il nipote), mentre col dispatcher sono 2.
        let via_anchor: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM nexus_subagent_runs WHERE parent_run_id = $1 AND is_background = true",
        )
        .bind(session)
        .fetch_one(&pool)
        .await
        .expect("count anchor");
        assert_eq!(
            via_anchor, 3,
            "il vecchio filtro per anchor vedeva anche il nipote"
        );
        let via_dispatcher: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM nexus_subagent_runs WHERE dispatcher_run_id = $1 AND is_background = true",
        )
        .bind(p_run)
        .fetch_one(&pool)
        .await
        .expect("count dispatcher");
        assert_eq!(
            via_dispatcher, 2,
            "il fix per dispatcher vede SOLO i figli diretti di P"
        );

        // Cp1 NON si accoda ancora: il suo figlio diretto Cs1 e' running.
        let enq_cp1 = fanin_enqueue_if_last(&pool, &pool, cp1_run, project, session)
            .await
            .expect("enqueue Cp1 ok");
        assert!(!enq_cp1, "Cp1 ha Cs1 ancora running -> non si accoda");
    }

    // ─── Consiglio a monte: selezione figure + composizione sintesi ─────────────

    fn axis(name: &str, keywords: &[&str], figures: &[&str]) -> CouncilDomainAxis {
        CouncilDomainAxis {
            name: name.to_string(),
            keywords: keywords.iter().map(|s| s.to_string()).collect(),
            figures: figures.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn council_cfg() -> CouncilConfig {
        CouncilConfig {
            base_figures: vec![
                "functional_analyst".to_string(),
                "software_architect".to_string(),
                "security_engineer".to_string(),
                "project_manager".to_string(),
            ],
            domain_axes: vec![
                axis("infra", &["deploy", "docker"], &["sysadmin"]),
                axis("ui", &["interfaccia", "pagina"], &["ui_ux_designer"]),
            ],
            max_figures: 6,
        }
    }

    #[test]
    fn select_figures_solo_base_senza_ambito_infra() {
        let cfg = council_cfg();
        let got = select_council_figures("aggiungi il login con OTP via email", &cfg, None, None);
        assert_eq!(
            got,
            vec![
                "functional_analyst",
                "software_architect",
                "security_engineer",
                "project_manager"
            ],
        );
        assert!(!got.iter().any(|f| f == "sysadmin"));
    }

    #[test]
    fn select_figures_aggiunge_sysadmin_su_ambito_deploy() {
        let cfg = council_cfg();
        let got = select_council_figures("prepara il deploy con docker in produzione", &cfg, None, None);
        assert!(
            got.iter().any(|f| f == "sysadmin"),
            "atteso sysadmin: {got:?}"
        );
        assert_eq!(got.len(), 5);
    }

    /// Il difetto che rendeva MUTA qualunque figura d'ambito: col profilo
    /// `medium` (target 3) e 5 figure base, il taglio prendeva le prime tre voci
    /// del CSV e scartava la lente scelta dal testo. Vale per `sysadmin` (che
    /// esisteva da mig 0553) come per ogni asse aggiunto dopo.
    #[test]
    fn target_non_scarta_la_figura_scelta_dal_task() {
        let cfg = council_cfg();
        let got =
            select_council_figures("prepara il deploy con docker in produzione", &cfg, Some(3), None);
        assert_eq!(got.len(), 3, "il target dimensiona il panel: {got:?}");
        assert_eq!(
            got[0], "sysadmin",
            "la figura d'ambito entra per prima, non si taglia: {got:?}"
        );
        // I posti restanti vanno alle base, nell'ordine dichiarato.
        assert_eq!(got[1], "functional_analyst");
        assert_eq!(got[2], "software_architect");
    }

    /// Due assi attivi insieme: entrambe le lenti entrano, e il target si
    /// allarga quanto basta a ospitarle (mai oltre `max_figures`).
    #[test]
    fn due_assi_attivi_allargano_il_target_fino_al_cap() {
        let cfg = council_cfg();
        let got = select_council_figures(
            "rifai l'interfaccia della pagina e sistema il deploy docker",
            &cfg,
            Some(1),
            None,
        );
        assert_eq!(
            got,
            vec!["sysadmin", "ui_ux_designer"],
            "target 1 ma due figure obbligatorie: {got:?}"
        );
    }

    #[test]
    fn cap_massimo_vince_anche_sulle_figure_dambito() {
        let cfg = CouncilConfig {
            base_figures: vec!["functional_analyst".to_string()],
            domain_axes: vec![axis("ui", &["pagina"], &["ui_ux_designer", "functional_analyst"])],
            max_figures: 1,
        };
        let got = select_council_figures("sistema la pagina", &cfg, Some(5), None);
        assert_eq!(got, vec!["ui_ux_designer"], "cap assoluto: {got:?}");
    }

    #[test]
    fn target_zero_non_convoca_nessuno() {
        let cfg = council_cfg();
        assert!(select_council_figures("rifai la pagina", &cfg, Some(0), None).is_empty());
    }

    #[test]
    fn select_figures_dedup_e_cap() {
        let cfg = CouncilConfig {
            base_figures: vec![
                "software_architect".to_string(),
                "software_architect".to_string(),
                "security_engineer".to_string(),
            ],
            domain_axes: vec![axis(
                "infra",
                &["deploy"],
                &["security_engineer", "sysadmin"],
            )],
            max_figures: 2,
        };
        let got = select_council_figures("deploy dell'app", &cfg, None, None);
        // Dedup: security_engineer una sola volta (asse + base); cap 2 -> tronca.
        assert_eq!(got, vec!["security_engineer", "sysadmin"]);
    }

    // ─── Review panel: la lente di interfaccia ─────────────────────────────

    #[test]
    fn senza_lente_ui_il_panel_resta_di_soli_revisori_generici() {
        assert_eq!(kinds_dei_revisori(2, false), vec!["review", "review"]);
    }

    /// La lente PRENDE IL POSTO di un revisore generico invece di sommarsi:
    /// accenderla non deve allargare il costo del panel.
    #[test]
    fn la_lente_ui_non_allarga_il_panel() {
        let kinds = kinds_dei_revisori(2, true);
        assert_eq!(kinds.len(), 2, "il panel resta di due: {kinds:?}");
        assert_eq!(kinds, vec![UI_REVIEWER_KIND, "review"]);
    }

    /// Sta in testa perche' il panel si riduce dalla CODA quando i giudici
    /// distinti non bastano (`spawn_reviewers` tronca a `panel.len()`): in
    /// fondo, la lente accesa dai file toccati sarebbe la prima a sparire, e
    /// sparirebbe in silenzio.
    #[test]
    fn la_lente_ui_sopravvive_al_taglio_del_panel() {
        let kinds = kinds_dei_revisori(3, true);
        let ridotto: Vec<String> = kinds.iter().take(1).cloned().collect();
        assert_eq!(
            ridotto,
            vec![UI_REVIEWER_KIND],
            "col panel ridotto a uno resta la lente pertinente: {kinds:?}"
        );
    }

    /// Il caso di campo (28/07, progetto gestione-spese) misurato sul DB VERO:
    /// settings, vocabolario d'ambito e figure arrivano dalle migrazioni, non da
    /// una `CouncilConfig` scritta a mano che confermerebbe se stessa (regola O).
    ///
    /// La richiesta non nomina l'interfaccia — dice "app", che e' il modo in cui
    /// un utente la nomina. Col profilo `medium` (target 3) il consiglio deve
    /// convocare la lente UI, non le prime tre voci del CSV.
    ///
    /// MUTAZIONE: togliendo `ui` da `orchestrator.council_domain_axes`, o
    /// rimettendo il taglio per posizione, l'asserzione fallisce nominando le
    /// figure convocate.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn richiesta_di_unapp_convoca_la_lente_ui(pool: sqlx::PgPool) {
        let cfg = read_council_config(&pool).await;
        assert!(
            cfg.domain_axes.iter().any(|a| a.name == "ui"),
            "l'asse ui deve esistere nei settings seminati"
        );

        let target: usize = sqlx::query_scalar::<_, String>(
            "SELECT value FROM settings WHERE key = 'orchestrator.sizing_profile_medium'",
        )
        .fetch_one(&pool)
        .await
        .ok()
        .and_then(|v| serde_json::from_str::<Value>(&v).ok())
        .and_then(|v| v.get("council_figures").and_then(Value::as_u64))
        .expect("il profilo medium deve dichiarare council_figures") as usize;

        let figures = select_council_figures(
            "creami un'app per la gestione delle spese di casa",
            &cfg,
            Some(target),
            None,
        );
        assert!(
            figures.iter().any(|f| f == "ui_ux_designer"),
            "convocate {figures:?} (target {target}): nessuno guarda l'interfaccia"
        );
        assert_eq!(
            figures.len(),
            target,
            "la lente pertinente prende il posto di una base, non si somma: {figures:?}"
        );
    }

    /// L'anello successivo (regola O): il parere della figura deve ARRIVARE nel
    /// prompt del run che implementa. Parte dall'esito strutturato di un sub-run
    /// — la forma che `advisory_verdict` produce davvero — e finisce nel blocco
    /// testuale che viene anteposto al messaggio iniziale.
    #[test]
    fn il_vincolo_di_interfaccia_arriva_nel_blocco_del_prompt() {
        let parere = mock_figure_result(
            "proceed_with_changes",
            "la lista spese non rende lo stato vuoto",
            None,
        );
        let synth = compose_council_synthesis(&[parere], &AdvisoryPolicy::default(), 1)
            .expect("una figura che vota produce una sintesi");
        let blocco = render_council_synthesis(&synth);
        assert!(
            blocco.contains("la lista spese non rende lo stato vuoto"),
            "il vincolo non arriva a chi implementa: {blocco}"
        );
    }

    #[test]
    fn select_figures_vuoto_se_base_vuota() {
        let cfg = CouncilConfig {
            base_figures: vec![],
            domain_axes: vec![],
            max_figures: 6,
        };
        assert!(select_council_figures("qualunque testo", &cfg, None, None).is_empty());
    }

    // ─── Competenze dichiarate dal classificatore governano al posto delle keyword ───

    /// Le competenze dichiarate ENTRANO come figure d'ambito anche se il testo
    /// non contiene NESSUNA keyword d'asse: e' il punto del fix — un giudizio
    /// semantico non e' vincolato a condividere una parola col messaggio.
    #[test]
    fn competenze_dichiarate_convocano_senza_bisogno_di_keyword() {
        let cfg = council_cfg();
        let got = select_council_figures(
            "gestisci la messa in sicurezza dell'accesso",
            &cfg,
            None,
            Some(&["sysadmin".to_string()]),
        );
        assert!(
            got.iter().any(|f| f == "sysadmin"),
            "la competenza dichiarata deve convocare sysadmin: {got:?}"
        );
    }

    /// `Some(vec![])` e' un giudizio: "nessuna lente d'ambito serve". Anche col
    /// testo che contiene una keyword d'asse (qui "deploy"), le competenze
    /// dichiarate governano — la vecchia strada keyword non deve piu' scattare.
    #[test]
    fn competenze_dichiarate_vuote_non_convocano_ambito_anche_con_keyword_nel_testo() {
        let cfg = council_cfg();
        let got = select_council_figures(
            "prepara il deploy con docker in produzione",
            &cfg,
            None,
            Some(&[]),
        );
        assert!(
            !got.iter().any(|f| f == "sysadmin"),
            "competencies=Some(vec![]) e' un giudizio esplicito, niente ripiego keyword: {got:?}"
        );
        assert_eq!(got, vec![
            "functional_analyst",
            "software_architect",
            "security_engineer",
            "project_manager"
        ]);
    }

    /// `None` (classificatore non ha potuto dichiarare) e' l'UNICO caso in cui
    /// le keyword d'ambito restano usate: comportamento pre-esistente invariato.
    #[test]
    fn competenze_non_dichiarabili_ripiegano_sulle_keyword() {
        let cfg = council_cfg();
        let got =
            select_council_figures("prepara il deploy con docker in produzione", &cfg, None, None);
        assert!(
            got.iter().any(|f| f == "sysadmin"),
            "ripiego keyword atteso quando competencies=None: {got:?}"
        );
    }

    #[test]
    fn council_figure_tasks_progresso_ui() {
        use super::{council_figure_tasks, FigureAdvisoryReport, FigureAdvisoryStatus};
        let figures = vec![
            "security_engineer".to_string(),
            "software_architect".to_string(),
        ];
        let start = council_figure_tasks(&figures, &[]);
        assert_eq!(start.len(), 2);
        assert_eq!(start[0].status, super::CouncilFigureTaskStatus::Running);
        assert_eq!(start[1].status, super::CouncilFigureTaskStatus::Running);

        let partial = council_figure_tasks(
            &figures,
            &[FigureAdvisoryReport {
                kind: "security_engineer".to_string(),
                status: FigureAdvisoryStatus::AdvisoryOk,
                detail_code: "advisory_ok".to_string(),
                detail_message: "ok".to_string(),
                advisory_verdict: Some("proceed".to_string()),
                advisory: None,
                provider: None,
                model: None,
                subagent_run_id: None,
            }],
        );
        assert_eq!(partial[0].status, super::CouncilFigureTaskStatus::Done);
        assert_eq!(partial[1].status, super::CouncilFigureTaskStatus::Running);

        let failed = council_figure_tasks(
            &figures,
            &[FigureAdvisoryReport {
                kind: "software_architect".to_string(),
                status: FigureAdvisoryStatus::RunTimeout,
                detail_code: "run_timeout".to_string(),
                detail_message: "timeout".to_string(),
                advisory_verdict: None,
                advisory: None,
                provider: None,
                model: None,
                subagent_run_id: None,
            }],
        );
        assert_eq!(failed[0].status, super::CouncilFigureTaskStatus::Running);
        assert_eq!(failed[1].status, super::CouncilFigureTaskStatus::Failed);
    }

    #[test]
    fn council_outcome_degraded_codes_strutturati() {
        let figures = vec!["security_engineer".to_string()];
        assert_eq!(
            CouncilConveneOutcome::Degraded {
                reason: CouncilDegradeReason::SubagentsDisabled,
                figures: figures.clone(),
                figure_reports: Vec::new(),
            }
            .degradation_reason_code(),
            Some("subagents_disabled")
        );
        assert_eq!(
            CouncilConveneOutcome::Degraded {
                reason: CouncilDegradeReason::SynthesisUnavailable,
                figures: figures.clone(),
                figure_reports: Vec::new(),
            }
            .degradation_reason_code(),
            Some("synthesis_unavailable")
        );
        assert!(
            CouncilConveneOutcome::Degraded {
                reason: CouncilDegradeReason::BuildCtxFailed,
                figures,
                figure_reports: Vec::new(),
            }
            .render_block()
            .is_empty()
        );
    }

    #[test]
    fn classify_figure_prepare_failed_usa_error_code() {
        let result = json!({
            "error": "depth 3 > max 2",
            "error_code": "depth_exceeded",
            "status": "prepare_failed",
        });
        let report = classify_council_figure_result("security_engineer", &result);
        assert_eq!(report.status, FigureAdvisoryStatus::PrepareFailed);
        assert_eq!(report.detail_code, "depth_exceeded");
    }

    /// REGRESSIONE consiglio "depth 3 > max 2": la profondita' del sub-run deriva
    /// dalla CATENA ANTENATI (depth del dispatcher), NON dal MAX tra i fratelli
    /// paralleli sotto l'anchor. Le 6 figure convocate dal run principale sono
    /// tutte figlie DIRETTE -> depth 1, non 1/2/3 (che faceva rifiutare le ultime).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn chain_depth_immune_ai_fratelli_paralleli(pool: sqlx::PgPool) {
        let main = Uuid::new_v4(); // run principale (dispatcher figure): NON ha row
        let anchor = Uuid::new_v4(); // anchor condiviso dalle figure

        // 3 figure gia' registrate sotto lo stesso anchor, running, depth 1 (tutte
        // figlie dirette del main). Col vecchio MAX(depth running under anchor) la
        // 4a figura avrebbe preso 1 -> depth 2, poi 3 -> rifiuto.
        for _ in 0..3 {
            sqlx::query(
                "INSERT INTO nexus_subagent_runs \
                     (parent_run_id, dispatcher_run_id, project_id, kind, task_description, \
                      depth, status) \
                 VALUES ($1, $2, gen_random_uuid(), 'coder', 'task di test', 1, 'running')",
            )
            .bind(anchor)
            .bind(main)
            .execute(&pool)
            .await
            .expect("insert figura");
        }
        // Dispatcher = main (nessuna row) -> base 0 -> figlio depth 1, a prescindere
        // dai 3 fratelli running. (Vecchio codice: 2.)
        assert_eq!(
            current_chain_depth(&pool, main).await,
            0,
            "il main non ha row -> depth base 0; i fratelli paralleli NON contano"
        );

        // Catena reale: un dispatcher che E' un sub-run a depth 1 -> figlio depth 2.
        let d1 = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO nexus_subagent_runs \
                 (id, parent_run_id, dispatcher_run_id, project_id, kind, task_description, \
                  depth, status) \
             VALUES ($1, $2, $3, gen_random_uuid(), 'coder', 'task di test', 1, 'running')",
        )
        .bind(d1)
        .bind(anchor)
        .bind(main)
        .execute(&pool)
        .await
        .expect("insert dispatcher depth1");
        assert_eq!(
            current_chain_depth(&pool, d1).await,
            1,
            "figlio di un dispatcher a depth 1 -> base 1 (+1 = depth 2)"
        );
    }

    #[test]
    fn classify_figure_no_advisory() {
        let result = json!({
            "status": "completed",
            "outcome": { "success": true },
        });
        let report = classify_council_figure_result("software_architect", &result);
        assert_eq!(report.status, FigureAdvisoryStatus::CompletedNoAdvisory);
    }

    #[test]
    fn classify_figure_advisory_ok() {
        let result = mock_figure_result("proceed", "test", None);
        let report = classify_council_figure_result("project_manager", &result);
        assert_eq!(report.status, FigureAdvisoryStatus::AdvisoryOk);
        assert_eq!(report.advisory_verdict.as_deref(), Some("proceed"));
    }

    /// Un tool_result mock di una figura col blocco `outcome.advisory` (regola M).
    fn mock_figure_result(verdict: &str, req: &str, risk_sev: Option<&str>) -> Value {
        let mut advisory = json!({
            "verdict": verdict,
            "requirements": [req],
            "recommendations": [],
        });
        if let Some(sev) = risk_sev {
            advisory["risks"] = json!([{ "severity": sev, "description": "rischio con evidenza" }]);
        } else {
            advisory["risks"] = json!([]);
        }
        json!({
            "status": "completed",
            "outcome": { "success": true, "advisory": advisory },
        })
    }

    /// Result mock di una figura CONVOCATA ma senza esito valido (timeout/errore
    /// provider): astensione che deve pesare nel denominatore del quorum.
    fn mock_failed_figure_result() -> Value {
        json!({
            "status": "completed",
            "outcome": { "success": false, "advisory": Value::Null },
        })
    }

    #[test]
    fn compose_council_synthesis_da_result_mock() {
        let results = vec![
            mock_figure_result("proceed_with_changes", "validare input", None),
            mock_figure_result("proceed", "logging strutturato", None),
        ];
        let synth = compose_council_synthesis(&results, &AdvisoryPolicy::default(), 2)
            .expect("almeno un advisory -> sintesi presente");
        assert_eq!(synth.verdict.as_str(), "proceed_with_changes");
        assert_eq!(synth.valid, 2);
        assert_eq!((synth.convened, synth.required_valid), (2, 1));
        assert!(synth.requirements.iter().any(|r| r.text == "validare input"));
        assert!(synth
            .requirements
            .iter()
            .any(|r| r.text == "logging strutturato"));
    }

    #[test]
    fn compose_council_synthesis_veto_su_high_severity() {
        let results = vec![
            mock_figure_result("proceed", "ok", None),
            mock_figure_result("block", "non esporre il segreto", Some("alta")),
        ];
        let synth = compose_council_synthesis(&results, &AdvisoryPolicy::default(), 2)
            .expect("sintesi presente");
        // Veto avversario attivo di default: un block con severity alta vince.
        assert_eq!(synth.verdict.as_str(), "block");
        assert!(synth.verdict.is_veto());
    }

    #[test]
    fn compose_council_synthesis_none_senza_advisory() {
        // Result senza outcome.advisory (worker ordinario) -> non e' un panel.
        let results = vec![json!({ "status": "completed", "outcome": { "success": true } })];
        assert!(compose_council_synthesis(&results, &AdvisoryPolicy::default(), 1).is_none());
    }

    #[test]
    fn compose_council_synthesis_sotto_quorum_inconclusive() {
        // Il caso di campo (incidente 2026-07-14): 5 convocate, 4 senza esito,
        // 1 proceed. Quorum 50% di 5 = 3 -> Inconclusive, mai proceed.
        let results = vec![
            mock_figure_result("proceed", "ok", None),
            mock_failed_figure_result(),
            mock_failed_figure_result(),
            mock_failed_figure_result(),
            mock_failed_figure_result(),
        ];
        let synth = compose_council_synthesis(&results, &AdvisoryPolicy::default(), 5)
            .expect("un advisory presente -> sintesi presente");
        assert_eq!(synth.verdict.as_str(), "inconclusive");
        assert_eq!(
            (synth.valid, synth.convened, synth.required_valid),
            (1, 5, 3)
        );
    }

    #[test]
    fn render_synthesis_contiene_requisiti_rischi_verdetto() {
        let results = vec![mock_figure_result(
            "proceed_with_changes",
            "cifrare i dati a riposo",
            Some("media"),
        )];
        let synth = compose_council_synthesis(&results, &AdvisoryPolicy::default(), 1).unwrap();
        let block = render_council_synthesis(&synth);
        assert!(block.starts_with("<consiglio_sintesi>"));
        assert!(block.ends_with("</consiglio_sintesi>"));
        assert!(block.contains("proceed_with_changes"));
        assert!(block.contains("cifrare i dati a riposo"));
        assert!(block.contains("[media]"));
    }

    #[test]
    fn render_council_synthesis_dichiara_base_voti() {
        // La base dei voti (validi/convocate/quorum) e' SEMPRE dichiarata: il
        // lettore non deve mai dedurre il consenso dalla sola presenza del blocco.
        let results = vec![
            mock_figure_result("proceed", "ok", None),
            mock_figure_result("proceed", "ok2", None),
        ];
        let synth = compose_council_synthesis(&results, &AdvisoryPolicy::default(), 2).unwrap();
        let block = render_council_synthesis(&synth);
        assert!(block.contains("Pareri validi: 2 su 2 figure convocate (quorum minimo: 1)"));
        assert!(block.contains("parere convergente"));
    }

    #[test]
    fn render_council_synthesis_sotto_quorum_dichiara_parzialita() {
        // Opzione (b): sotto quorum la sintesi viene comunque iniettata ma NON
        // puo' affermare un consenso che non c'e'.
        let results = vec![
            mock_figure_result("proceed", "ok", None),
            mock_failed_figure_result(),
            mock_failed_figure_result(),
            mock_failed_figure_result(),
            mock_failed_figure_result(),
        ];
        let synth = compose_council_synthesis(&results, &AdvisoryPolicy::default(), 5).unwrap();
        let block = render_council_synthesis(&synth);
        assert!(block.contains("Verdetto del consiglio: inconclusive"));
        assert!(block.contains("Pareri validi: 1 su 5 figure convocate (quorum minimo: 3)"));
        assert!(block.contains("quorum NON raggiunto"));
        assert!(block.contains("pareri PARZIALI"));
        assert!(
            !block.contains("parere convergente"),
            "sotto quorum la frase 'parere convergente' e' una menzogna"
        );
    }

    #[test]
    fn render_multi_provider_synthesis_contiene_quorum_e_verdetto() {
        let results = vec![
            mock_figure_result("proceed", "test idempotenti", None),
            mock_figure_result("proceed_with_changes", "punto unico routing", None),
        ];
        let synth = compose_multi_provider_synthesis(&results, &AdvisoryPolicy::default(), 2)
            .expect("sintesi multi-provider");
        let block = render_multi_provider_synthesis(&synth);
        assert!(block.starts_with("<multi_provider_sintesi>"));
        assert!(block.contains("Verdetto del panel multi-provider"));
        assert!(block.contains("Pareri validi: 2 su 2 provider convocati (quorum minimo: 1)"));
    }

    #[test]
    fn multi_provider_panel_outcome_degraded_non_inietta_blocco() {
        let outcome = MultiProviderPanelOutcome::Degraded {
            reason: MultiProviderDegradeReason::InsufficientProviderDiversity,
            got: 1,
            min: 2,
        };
        assert!(outcome.render_block().is_empty());
        assert!(outcome.advisory_synthesis_value().is_none());
        assert_eq!(
            outcome.degradation_reason_code(),
            Some("insufficient_provider_diversity")
        );
    }

    /// Mig 0555: purpose ultracode da settings con fallback a model_purpose.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn ultracode_purpose_da_settings_con_fallback(pool: sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .expect("settings");
        sqlx::query(
            "CREATE TABLE nexus_purpose_model ( \
                 purpose TEXT PRIMARY KEY, \
                 tier TEXT, \
                 required_capability TEXT, \
                 requires_tool_use BOOLEAN NOT NULL DEFAULT false \
             )",
        )
        .execute(&pool)
        .await
        .expect("nexus_purpose_model");
        crate::test_support::create_ai_price_catalog_table(&pool).await;
        for (purpose, tier, provider, model) in [
            ("worker_implement", "medium", "openai", "gpt-a"),
            ("worker_verify", "light", "anthropic", "claude-v"),
            ("reviewer", "high", "deepseek", "ds-r"),
        ] {
            sqlx::query(
                "INSERT INTO nexus_purpose_model (purpose, tier, required_capability, requires_tool_use) \
                 VALUES ($1, $2, 'code', true)",
            )
            .bind(purpose)
            .bind(tier)
            .execute(&pool)
            .await
            .expect("purpose");
            seed_catalog_row(&pool, provider, model, tier, 1.0).await;
        }
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES \
             ('orchestrator.ultracode_tier_diversity_enabled', 'true'), \
             ('orchestrator.ultracode_implement_purpose', 'worker_implement'), \
             ('orchestrator.ultracode_verify_purpose', 'worker_verify'), \
             ('orchestrator.ultracode_review_purpose', 'reviewer')",
        )
        .execute(&pool)
        .await
        .expect("settings");
        let def = SubagentDefinition {
            prompt_key: "x".to_string(),
            tool_whitelist: vec![],
            model_purpose: "worker_implement".to_string(),
            timeout_s: 0,
        };
        let (provider, _model) =
            resolve_worker_model(&pool, &pool, "verify", &def, Uuid::new_v4(), Uuid::new_v4())
                .await;
        assert_eq!(
            provider, "anthropic",
            "kind verify deve usare orchestrator.ultracode_verify_purpose"
        );
    }
}
