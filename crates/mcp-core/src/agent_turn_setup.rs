//! Preparazione del turno agentico: QUALI tool esporre al modello e con che
//! `behavior_mode`, piu' la sanitizzazione degli errori mostrati all'utente.
//!
//! Punto unico (regola L) di `build_tools_json_for_agent`, usato sia dallo spawn
//! del run principale sia dal resume: applica in ordine discovery-first,
//! whitelist per modello o-series, filtro per `automation_mode`, tetto ai tool
//! MCP e tool disclosure. Tutto DB-driven (regola G).
//!
//! Il file si chiamava `brain_agent_client.rs` ed era il client HTTP verso il
//! brain Python (`run_via_brain` + resume SSE). Il brain e' stato eliminato (mig
//! 0462/0532) e quelle 501 righe sono state rimosse: quello che resta non ha mai
//! parlato col brain — costruiva l'input del turno, e serve al motore nativo
//! esattamente come serviva prima.

use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::agent_tools::AGENT_TOOLS_JSON;
use crate::agent_types::{
    AITraceEvent, AgentMetaStep, AgentRunStatus, AgentStep, AgentStepEvent, AgentStepStatus,
};

/// Behavior_mode con cui il PRIMARIO (brain Python) avvia ogni run agentico.
///
/// PUNTO UNICO (regola L): mcp-core lo invia nel payload `/agent/run/stream`
/// (campo `behavior_mode`) e il brain lo copia TALE E QUALE in
/// `initial_state["behavior_mode"]` (`brain/grpc_server/routes/agent.py:621`).
/// Non e' derivato dall'`automation_mode` ne' dal routing del turno: e' la
/// costante storica del client. Il motore nativo (`native_engine`) DEVE
/// valorizzare lo stesso `behavior_mode` nello stato iniziale per attraversare la
/// STESSA topologia del primario (eleggibilita' del planner gata su questo valore):
/// non si re-definisce il valore in due punti, si riusa questa costante.
///
/// NB: il valore-vero-dal-turno (derivare il behavior_mode dall'automation_mode o
/// dal routing) sarebbe un miglioramento SEPARATO, valido sia per il primario
/// Python sia per il nativo Rust; e' fuori scope qui (richiederebbe di cambiare
/// PRIMA la fonte Python, altrimenti i due motori divergerebbero).
pub const PRIMARY_BEHAVIOR_MODE: &str = "bilanciata";

/// Chiave del template di sistema del run PRINCIPALE
/// (`nexus_prompt_templates.key`). Punto unico (regola L): il letterale era
/// ripetuto nei call site che risolvono il system prompt, e serve ora anche come
/// `prompt_key` con cui il ReflectionNode persiste in `nexus_agent_reflections`.
///
/// I sub-run NON usano questa: portano la `prompt_key` della propria definizione
/// (`nexus_subagent_definitions.prompt_key`).
pub const PRIMARY_PROMPT_KEY: &str = "system.nexus_base";


/// Costruisce il JSON tools da inviare al brain applicando la discovery mode.
///
/// Logica:
/// - Legge `mcp_tool_search_hard_limit` dal DB (default 20).
/// - Conta i tool MCP esterni abilitati e accessibili a user/project.
/// - Se count < soglia: include le definizioni dei tool MCP nel payload
///   (il brain li vede direttamente senza dover cercare).
/// - Se count >= soglia: invia solo AGENT_TOOLS_JSON (che contiene già
///   `nexus_mcp_tool_search` + `nexus_mcp_tool_call`). Il brain usa la
///   ricerca semantica per scoprire i tool a runtime — discovery mode.
///
/// Fallback hardcoded dei tool essenziali per modelli o-series (o1/o3/o4-mini).
/// Questi modelli non supportano `tool_choice` e con 40+ tool tendono a
/// "narrare" invece di fare tool call. La soluzione e' passare solo i tool
/// fondamentali + `nexus_mcp_tool_search`/`nexus_mcp_tool_call` per la
/// discovery on-demand dei tool non inclusi.
///
/// La sorgente autoritativa e' `settings.automation.o_series_essential_tools`
/// (CSV). Letta da `load_o_series_essential_tools()`. Questo fallback copre
/// il caso DB down.
const O_SERIES_ESSENTIAL_TOOLS_FALLBACK: &[&str] = &[
    // Core lettura
    "read_file",
    "read_file_lines",
    "list_files",
    "search_in_files",
    // Core scrittura
    "write_file",
    "edit_file",
    "run_command",
    "fs_mkdir",
    "delete_file",
    // Git essenziale
    "git_status",
    "git_commit",
    // Test
    "run_tests",
    // Discovery (il modello scopre tool aggiuntivi a runtime)
    "nexus_mcp_tool_search",
    "nexus_mcp_tool_call",
    // Ricerca semantica
    "search_codebase_semantic",
    // Generazione documenti professionali (.docx). Audit 27/05/2026: senza
    // questo, il pannello DOCUMENTI invocava nexus_doc_generate ma l'agente
    // non lo trovava nei 15 tool del safety-net o-series e rispondeva
    // a parole "procedo con la generazione" senza generare nulla.
    "nexus_doc_generate",
    "nexus_doc_update",
    "nexus_doc_list",
    "nexus_doc_status",
];

/// Ritorna `true` se il modello e' della serie reasoning OpenAI (o1/o3/o4-mini).
/// Questi modelli non supportano il parametro `tool_choice` e hanno bisogno
/// di un set ridotto di tool + istruzioni esplicite nel system prompt.
///
/// Esposta come `is_o_series_model_pub` per uso in `chat_messages.rs`.
fn is_o_series_model(model: &str) -> bool {
    let m = model.to_lowercase();
    ["o1", "o1-mini", "o1-preview", "o3", "o3-mini", "o4-mini"]
        .iter()
        .any(|prefix| m == *prefix || m.starts_with(&format!("{}-", prefix)))
}

/// Wrapper pubblico di `is_o_series_model` per uso in altri moduli (es. chat_messages).
pub fn is_o_series_model_pub(model: &str) -> bool {
    is_o_series_model(model)
}

/// Legge la whitelist tool essenziali per o-series da `settings`.
/// Ritorna sempre una lista non vuota: in caso di DB down o chiave assente,
/// usa il fallback hardcoded `O_SERIES_ESSENTIAL_TOOLS_FALLBACK`.
async fn load_o_series_essential_tools(db: &PgPool) -> Vec<String> {
    let csv: Option<String> = sqlx::query_scalar(
        "SELECT value FROM settings WHERE key = 'automation.o_series_essential_tools'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    match csv {
        Some(s) if !s.trim().is_empty() => s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        _ => O_SERIES_ESSENTIAL_TOOLS_FALLBACK
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

/// M16 — True se il setting `agent.tools.discovery_first_enabled` e' attivo.
/// Default false (opt-in). DB-driven (regola G), niente fallback hardcoded ON.
async fn is_discovery_first_enabled(db: &PgPool) -> bool {
    let v: Option<String> = sqlx::query_scalar(
        "SELECT value FROM settings WHERE key = 'agent.tools.discovery_first_enabled' LIMIT 1",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    v.map(|s| {
        !matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "false" | "0" | "off" | "no" | ""
        )
    })
    .unwrap_or(false)
}

/// M16 — Whitelist dei tool del primo turno discovery-only (CSV in
/// `agent.tools.discovery_first_whitelist`). Fallback ai 2 tool di discovery.
async fn load_discovery_tools(db: &PgPool) -> Vec<String> {
    let csv: Option<String> = sqlx::query_scalar(
        "SELECT value FROM settings WHERE key = 'agent.tools.discovery_first_whitelist' LIMIT 1",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    match csv {
        Some(s) if !s.trim().is_empty() => s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        _ => vec![
            "nexus_mcp_tool_search".to_string(),
            "nexus_mcp_tool_call".to_string(),
        ],
    }
}

/// Filtra il JSON tools lasciando solo quelli il cui `name` e' nella whitelist.
fn filter_tools_by_whitelist(tools: Value, whitelist: &[String]) -> Value {
    let arr = match tools.as_array() {
        Some(a) => a,
        None => return Value::Array(Vec::new()),
    };
    let filtered: Vec<Value> = arr
        .iter()
        .filter(|t| {
            let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
            whitelist.iter().any(|w| w == name)
        })
        .cloned()
        .collect();
    tracing::info!(
        "filter_tools_by_whitelist: {} tool totali -> {} filtrati (whitelist={} voci)",
        arr.len(),
        filtered.len(),
        whitelist.len(),
    );
    Value::Array(filtered)
}

/// Fallback hardcoded della whitelist read-only per modalita' `study`.
/// USATA SOLO se `settings.automation.study_mode_readonly_tools` non e'
/// leggibile dal DB (DB down all'avvio). Mantenuta minima: l'admin deve
/// poter ampliare la lista via UI senza redeploy.
///
/// La sorgente autoritativa e' `settings.automation.study_mode_readonly_tools`
/// (mig 0132): CSV di nomi tool. Letta da `load_study_mode_readonly_tools()`.
const STUDY_MODE_READONLY_TOOLS_FALLBACK: &[&str] = &[
    "read_file",
    "read_file_lines",
    "list_files",
    "search_in_files",
    "search_codebase_semantic",
    "get_project_structure",
    "git_status",
    "git_log",
    "git_diff",
    "nexus_mcp_tool_search",
];

/// Legge la whitelist read-only per study mode da `settings`.
/// Ritorna sempre una lista non vuota: in caso di DB down o chiave assente,
/// usa il fallback hardcoded `STUDY_MODE_READONLY_TOOLS_FALLBACK`.
async fn load_study_mode_readonly_tools(db: &PgPool) -> Vec<String> {
    let csv: Option<String> = sqlx::query_scalar(
        "SELECT value FROM settings WHERE key = 'automation.study_mode_readonly_tools'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    match csv {
        Some(s) if !s.trim().is_empty() => s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        _ => {
            tracing::warn!(
                "load_study_mode_readonly_tools: settings.automation.study_mode_readonly_tools assente, uso fallback ({} tool)",
                STUDY_MODE_READONLY_TOOLS_FALLBACK.len(),
            );
            STUDY_MODE_READONLY_TOOLS_FALLBACK
                .iter()
                .map(|s| s.to_string())
                .collect()
        }
    }
}

/// Filtra il JSON di tool in base al `automation_mode`.
///
/// - `Automatic`: nessun filtro (l'utente vuole massima autonomia)
/// - `Confirm`: nessun filtro lato Rust — il gating avviene a livello di
///   HITL nel brain Python (`AwaitingConfirmation` prima di edit/write)
/// - `Study`: solo tool read-only (filtro difensivo basato sulla whitelist
///   passata in `readonly_tools_whitelist` — letta da DB dal caller).
fn filter_tools_by_automation_mode(
    tools: Value,
    mode: &crate::orchestrator::AutomationMode,
    readonly_tools_whitelist: &[String],
) -> Value {
    use crate::orchestrator::AutomationMode;
    match mode {
        AutomationMode::Automatic | AutomationMode::Confirm => tools,
        AutomationMode::Study => {
            // In study, l'agente puo' SOLO leggere/analizzare. Filtriamo
            // qualunque tool non esplicitamente in whitelist.
            let arr = match tools.as_array() {
                Some(a) => a,
                None => return Value::Array(Vec::new()),
            };
            let filtered: Vec<Value> = arr
                .iter()
                .filter(|t| {
                    let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    readonly_tools_whitelist.iter().any(|w| w == name)
                })
                .cloned()
                .collect();
            tracing::info!(
                "automation_mode=study: filtrati {} tool → {} read-only esposti (whitelist={} tool)",
                arr.len(),
                filtered.len(),
                readonly_tools_whitelist.len(),
            );
            Value::Array(filtered)
        }
    }
}

/// Legge dal DB la soglia `mcp_tool_search_hard_limit` (default 20).
async fn load_mcp_tool_hard_limit(db: &PgPool) -> i64 {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'mcp_tool_search_hard_limit'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .and_then(|v| v.trim().parse().ok())
    .unwrap_or(20)
}

/// Conta i tool MCP esterni abilitati e accessibili a `user_id`/`project_id`.
async fn count_accessible_mcp_tools(db: &PgPool, user_id: Uuid, project_id: Uuid) -> i64 {
    sqlx::query_scalar(
        r#"SELECT COUNT(*)
           FROM mcp_servers s
           JOIN mcp_server_tools t ON t.server_id = s.id
           WHERE s.enabled = true
             AND s.transport != 'builtin'
             AND (
               s.scope = 'global'
               OR (s.scope = 'user'    AND s.user_id    = $1)
               OR (s.scope = 'project' AND s.project_id = $2)
             )"#,
    )
    .bind(user_id)
    .bind(project_id)
    .fetch_one(db)
    .await
    .unwrap_or(0_i64)
}

/// Parsea `AGENT_TOOLS_JSON` escludendo i tool riservati ai sub-agenti.
///
/// Il catalogo del run PRINCIPALE esclude i tool riservati ai sub-agenti
/// (SUBAGENT_ONLY_TOOLS, punto unico in nexus-agent-tools): quei tool
/// arrivano SOLO via tool_whitelist di nexus_subagent_definitions
/// (build_tools_json in subagent_native.rs).
///
/// PANICA se la costante non e' JSON valido, e lo fa apposta. Qui c'era un
/// `unwrap_or_else(|_| json!([]))`: un refuso nella raw string avrebbe dato a
/// OGNI run un catalogo VUOTO — l'agente senza un solo tool, che risponde a voce
/// invece di lavorare — senza un errore da nessuna parte. Il dato e' statico e
/// noto a compile-time: se non parsa, il binario e' rotto, e va scoperto al
/// primo turno invece che dedotto da settimane di run inspiegabilmente inerti.
fn load_base_agent_tools() -> Value {
    let v: Value = serde_json::from_str(AGENT_TOOLS_JSON).unwrap_or_else(|e| {
        panic!(
            "AGENT_TOOLS_JSON non e' JSON valido ({e}): il catalogo tool del run \
             principale sarebbe vuoto e l'agente non potrebbe fare nulla. \
             E' un refuso nella costante di nexus-agent-tools::tool_schema."
        )
    });
    let filtered: Vec<Value> = v
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|t| {
            t.get("name")
                .and_then(Value::as_str)
                .map(|n| !nexus_agent_tools::tool_schema::SUBAGENT_ONLY_TOOLS.contains(&n))
                .unwrap_or(true)
        })
        .collect();
    // Un catalogo vuoto DOPO un parse riuscito significherebbe che il filtro ha
    // escluso tutto: altrettanto inutilizzabile, e altrettanto silenzioso.
    if filtered.is_empty() {
        panic!(
            "catalogo tool del run principale VUOTO dopo il filtro dei tool \
             riservati ai sub-agenti: nessun tool resterebbe all'agente"
        );
    }
    json!(filtered)
}

/// Sostituisce l'enum `kind` dei tool dispatch_subagent(s) coi kind reali passati
/// (regola G/L: registry DB unica fonte, niente enum hardcoded nello schema).
/// PURA: manipola il JSON del catalogo. `kinds` vuoto -> no-op (mantiene il SEED
/// statico). PUNTO UNICO della conoscenza "dove vive l'enum kind nei due schemi
/// dispatch" (singolare: kind.enum; plurale: tasks.items.kind.enum).
fn apply_dispatch_kinds_enum(tools: &mut Value, kinds: &[String]) {
    if kinds.is_empty() {
        return;
    }
    let enum_val = Value::Array(kinds.iter().map(|k| Value::String(k.clone())).collect());
    let Some(arr) = tools.as_array_mut() else {
        return;
    };
    for t in arr.iter_mut() {
        match t.get("name").and_then(Value::as_str) {
            Some("dispatch_subagent") => {
                if let Some(e) = t.pointer_mut("/input_schema/properties/kind/enum") {
                    *e = enum_val.clone();
                }
            }
            Some("dispatch_subagents") => {
                if let Some(e) =
                    t.pointer_mut("/input_schema/properties/tasks/items/properties/kind/enum")
                {
                    *e = enum_val.clone();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod dispatch_kinds_enum_tests {
    use super::*;

    #[test]
    fn sostituisce_enum_kind_dei_due_dispatch() {
        let mut tools = json!([
            {"name": "dispatch_subagent", "input_schema": {"properties": {"kind": {"type": "string", "enum": ["plan"]}}}},
            {"name": "dispatch_subagents", "input_schema": {"properties": {"tasks": {"items": {"properties": {"kind": {"enum": ["plan"]}}}}}}},
            {"name": "read_file", "input_schema": {"properties": {}}}
        ]);
        apply_dispatch_kinds_enum(
            &mut tools,
            &["plan".to_string(), "security_engineer".to_string()],
        );
        assert_eq!(
            tools[0]["input_schema"]["properties"]["kind"]["enum"],
            json!(["plan", "security_engineer"])
        );
        assert_eq!(
            tools[1]["input_schema"]["properties"]["tasks"]["items"]["properties"]["kind"]["enum"],
            json!(["plan", "security_engineer"])
        );
        // Tool non-dispatch intatto.
        assert!(tools[2]["input_schema"]["properties"].get("kind").is_none());
    }

    #[test]
    fn kinds_vuoto_no_op_mantiene_seed() {
        let mut tools = json!([
            {"name": "dispatch_subagent", "input_schema": {"properties": {"kind": {"enum": ["plan"]}}}}
        ]);
        apply_dispatch_kinds_enum(&mut tools, &[]);
        assert_eq!(
            tools[0]["input_schema"]["properties"]["kind"]["enum"],
            json!(["plan"])
        );
    }
}

/// Compone il set completo di tool prima del gating per automation_mode:
/// se il catalogo MCP e' piccolo (< soglia, > 0) include le definizioni MCP
/// dirette; altrimenti resta in discovery mode con i soli `base_tools`.
async fn assemble_full_tools(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    mcp_tool_count: i64,
    hard_limit: i64,
    base_tools: Value,
) -> Value {
    if mcp_tool_count < hard_limit && mcp_tool_count > 0 {
        // Catalogo piccolo: include le definizioni MCP direttamente
        tracing::debug!(
            "build_tools_json: {} tool MCP < soglia {}, includo definizioni dirette",
            mcp_tool_count,
            hard_limit
        );
        let mcp_tools =
            crate::mcp_connectors::load_mcp_tools_for_agent(db, user_id, Some(project_id)).await;
        if mcp_tools.is_empty() {
            base_tools
        } else {
            let mut all = base_tools.as_array().cloned().unwrap_or_default();
            all.extend(mcp_tools);
            json!(all)
        }
    } else {
        // Discovery mode (catalogo vuoto o >= soglia): solo AGENT_TOOLS_JSON
        // nexus_mcp_tool_search è già incluso — il brain lo usa per scoprire i tool
        if mcp_tool_count >= hard_limit {
            tracing::debug!(
                "build_tools_json: discovery mode ({} tool MCP >= soglia {})",
                mcp_tool_count,
                hard_limit
            );
        }
        base_tools
    }
}

pub async fn build_tools_json_for_agent(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    automation_mode: &crate::orchestrator::AutomationMode,
    _provider: &str,
    model: &str,
) -> Value {
    let hard_limit = load_mcp_tool_hard_limit(db).await;
    let mcp_tool_count = count_accessible_mcp_tools(db, user_id, project_id).await;
    let mut base_tools = load_base_agent_tools();
    // Regola G/L: l'enum `kind` dei tool dispatch_subagent(s) e' generato a runtime
    // dal registry DB (nexus_subagent_definitions ∩ whitelist runtime), non hardcoded
    // nel catalogo statico: aggiungere un kind/figura in DB lo rende convocabile senza
    // toccare lo schema. Lista vuota (DB irraggiungibile) -> resta il SEED del catalogo.
    apply_dispatch_kinds_enum(
        &mut base_tools,
        &crate::agent_tools::subagent_native::convocable_kinds(db).await,
    );
    let full_tools = assemble_full_tools(
        db,
        user_id,
        project_id,
        mcp_tool_count,
        hard_limit,
        base_tools,
    )
    .await;

    // Gating finale per automation_mode: in `study` filtriamo a solo
    // read-only. `confirm` e `automatic` passano la lista intera.
    // La whitelist e' letta da `settings.automation.study_mode_readonly_tools`
    // (mig 0132) — niente lista hardcoded nel codice (regola G CLAUDE.md).
    let readonly_whitelist = load_study_mode_readonly_tools(db).await;
    let after_mode =
        filter_tools_by_automation_mode(full_tools, automation_mode, &readonly_whitelist);

    apply_tool_disclosure(db, after_mode, model).await
}

/// Applica la riduzione progressiva del set di tool secondo la strategia di
/// disclosure attiva (discovery-first M16 -> o-series -> ADR 0016 Fase A.2), in
/// ordine di priorita'. Se nessuna strategia e' attiva ritorna `after_mode`
/// invariato. Estratta da `build_tools_json_for_agent` (comportamento invariato).
async fn apply_tool_disclosure(db: &PgPool, after_mode: Value, model: &str) -> Value {
    // M16 — Progressive tool disclosure: se discovery-first e' attivo, il set
    // INIZIALE passato al brain contiene SOLO i 2 tool di discovery
    // (nexus_mcp_tool_search/call). Il brain (state.py + nodes.py) intercetta i
    // risultati di search e inietta i tool scoperti come native per il turno
    // successivo. Lista minima -> meno MALFORMED su Gemini, prompt piu' snello.
    // Priorita' sopra il filtro o-series. tool_choice resta 'auto'.
    if is_discovery_first_enabled(db).await {
        let discovery = load_discovery_tools(db).await;
        tracing::info!(
            "build_tools_json: discovery-first attivo (model='{}') — primo turno {} tool di discovery",
            model, discovery.len()
        );
        return filter_tools_by_whitelist(after_mode, &discovery);
    }

    // Riduzione tool per modelli o-series (o1/o3/o4-mini): questi modelli non
    // supportano `tool_choice` e con 40+ tool tendono a narrare senza fare
    // tool call. Passiamo solo i tool essenziali + nexus_mcp_tool_search per
    // la discovery on-demand dei tool rimanenti.
    if is_o_series_model(model) {
        let essential = load_o_series_essential_tools(db).await;
        tracing::info!(
            "build_tools_json: modello o-series '{}' rilevato — riduzione a {} tool essenziali + discovery",
            model, essential.len()
        );
        return filter_tools_by_whitelist(after_mode, &essential);
    }

    // ── ADR 0016 Fase A.2: tool discovery on-demand per TUTTI i modelli ────
    // Se `agent.tools.discovery_enabled` e' true, il set inline contiene SOLO
    // i tool core (`agent.tools.inline_core_whitelist`). I rimanenti (~66) sono
    // raggiungibili via nexus_mcp_tool_search / nexus_mcp_tool_call (gia' inline).
    // Risparmio ~14k token/turno di tool definitions sui 19k totali.
    if is_a2_discovery_enabled(db).await {
        let core = load_inline_core_whitelist(db).await;
        if !core.is_empty() {
            tracing::info!(
                "build_tools_json: A.2 discovery on-demand attivo (model='{}') — {} tool core inline",
                model, core.len()
            );
            return filter_tools_by_whitelist(after_mode, &core);
        }
    }

    after_mode
}

/// Settings `agent.tools.discovery_enabled` (default false: opt-in).
async fn is_a2_discovery_enabled(db: &PgPool) -> bool {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.tools.discovery_enabled'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .map(|v| v.trim().eq_ignore_ascii_case("true"))
    .unwrap_or(false)
}

/// Whitelist tool core inline (Fase A.2). CSV in
/// `agent.tools.inline_core_whitelist`. Lista vuota -> disabilita la riduzione.
async fn load_inline_core_whitelist(db: &PgPool) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.tools.inline_core_whitelist'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .map(|csv| {
        csv.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

/// Riconosce gli errori del provider AI che indicano "credito esaurito" / "quota
/// superata" / "rate limit prolungato": condizioni non recuperabili in pochi minuti.
/// In questi casi il provider va messo in cooldown lungo (ore) per evitare di
/// continuare a chiamarlo a vuoto, e Nexus deve scalare automaticamente al
/// provider successivo nella gerarchia.
/// Severita' del cooldown da applicare al provider in base al tipo di errore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CooldownKind {
    /// 6 ore: errori non recuperabili in pochi minuti (billing/quota esaurita).
    Long,
    /// 60 secondi: errori transient (5xx, rate limit short-window).
    Short,
}

/// Reason human-readable per un cooldown di tipo billing/quota. Costante di
/// modulo condivisa dai due step di classificazione (regola L: un solo punto).
const BILLING_REASON: &str = "Quota AI esaurita o credito insufficiente";

/// Marker testuali di quota/credito esaurito. Un HTTP 429 e' AMBIGUO: puo'
/// essere un rate-limit transitorio (finestra di richieste, cooldown breve)
/// oppure quota/credito esaurito (cooldown lungo come credit_balance_too_low).
/// La distinzione si fa SOLO sul contenuto del messaggio: se compare uno di
/// questi marker e' billing/quota, altrimenti rate-limit transitorio.
/// `lower` deve gia' essere lowercased dal chiamante.
fn msg_has_billing_marker(lower: &str) -> bool {
    (lower.contains("credit balance") && lower.contains("too low"))
        || lower.contains("insufficient_quota")
        || lower.contains("insufficient quota")
        || lower.contains("exceeded your current quota")
        || lower.contains("billing_hard_limit_reached")
        || lower.contains("billing hard limit")
        || lower.contains("plans & billing")
        || lower.contains("upgrade or purchase credits")
        || lower.contains("upgrade or purchase")
        || lower.contains("billing required")
        || lower.contains("payment required")
        || lower.contains("account is not active")
        || lower.contains("no credits")
        || lower.contains("quota ai esaurita")
        || lower.contains("credito insufficiente")
}

/// Step 1 della classificazione: `error_class` esplicito propagato dal brain.
/// Ritorna `None` se `error_class` non e' uno dei valori noti (si passa allo
/// step 2 sul testo).
fn classify_by_error_class(
    error_class: Option<&str>,
    has_billing_marker: bool,
) -> Option<(&'static str, CooldownKind, &'static str)> {
    match error_class {
        Some("billing_error")
        | Some("billing_required")
        | Some("quota_exceeded")
        | Some("credit_balance_too_low")
        | Some("insufficient_quota") => Some(("billing_error", CooldownKind::Long, BILLING_REASON)),
        Some("rate_limit") => {
            // Anche con error_class=rate_limit esplicito, un 429 puo' in realta'
            // essere quota/credito esaurito: il brain (o un altro provider) puo'
            // aver mappato il 429 a rate_limit senza esaminare il messaggio.
            // Se il testo contiene marker billing, promuovi a cooldown lungo,
            // altrimenti resta rate-limit transitorio (cooldown breve).
            if has_billing_marker {
                Some(("billing_error", CooldownKind::Long, BILLING_REASON))
            } else {
                Some(("rate_limit", CooldownKind::Short, "Rate limit raggiunto"))
            }
        }
        Some("overloaded")
        | Some("provider_error")
        | Some("server_error")
        | Some("service_unavailable")
        | Some("bad_gateway")
        | Some("internal_server_error") => Some((
            "provider_error",
            CooldownKind::Short,
            "Provider sovraccarico o errore temporaneo",
        )),
        Some("auth_error") | Some("forbidden") => Some((
            "auth_error",
            CooldownKind::Long,
            "Credenziali o accesso provider non validi",
        )),
        Some("not_found") | Some("invalid_model") => None,
        Some("invalid_request")
        | Some("unprocessable")
        | Some("context_too_long")
        | Some("unsupported") => None,
        _ => None,
    }
}

/// Mappa il classificatore testuale unificato (`provider_error_classifier`) su
/// cooldown. Usato SOLO quando `error_class` strutturato e' assente (regola M).
fn map_classifier_to_cooldown(
    c: &crate::provider_error_classifier::ClassifiedError,
) -> Option<(&'static str, CooldownKind, &'static str)> {
    match c.stop_reason.as_str() {
        "billing_error" => Some(("billing_error", CooldownKind::Long, BILLING_REASON)),
        "rate_limit" => Some(("rate_limit", CooldownKind::Short, "Rate limit raggiunto")),
        "overloaded" | "service_unavailable" | "bad_gateway" | "provider_error" => Some((
            "provider_error",
            CooldownKind::Short,
            "Provider sovraccarico o errore temporaneo",
        )),
        "timeout" | "connection_error" => Some((
            "timeout",
            CooldownKind::Short,
            "Provider sovraccarico o errore temporaneo",
        )),
        "auth_error" | "forbidden" => Some((
            "auth_error",
            CooldownKind::Long,
            "Credenziali o accesso provider non validi",
        )),
        // Model-specific / client: nessun cooldown provider.
        "not_found" | "invalid_request" | "context_too_long" | "unprocessable" | "unsupported" => {
            None
        }
        _ => None,
    }
}

/// Step 2 legacy rimosso: usare [`map_classifier_to_cooldown`] via
/// `provider_error_classifier` (regola M).
/// Classifica un errore del provider e suggerisce il tipo di cooldown.
/// Ritorna `(error_class_normalizzato, kind, human_reason)`.
/// Priorita': `error_class` strutturato (SSE brain/gateway), poi classificatore
/// unificato `provider_error_classifier` — MAI parsing billing ad-hoc sul testo
/// (regola M).
fn classify_provider_error(
    error_class: Option<&str>,
    msg: &str,
) -> Option<(&'static str, CooldownKind, &'static str)> {
    if let Some(ec) = error_class.map(str::trim).filter(|s| !s.is_empty() && *s != "ok") {
        // Un error_class esplicito puo' essere una MIS-classificazione del brain: un
        // 429 mappato a `rate_limit` e' in realta' quota/credito esaurito quando il
        // MESSAGGIO porta un marker billing. Passiamo il marker (dal punto unico
        // `classify_text`, regola L) cosi' `classify_by_error_class` promuove a
        // billing/cooldown-lungo invece di lasciare il provider a-crediti-zero nel
        // cascade (bug live: l'error_class esplicito short-circuitava la promozione).
        let has_billing_marker =
            crate::provider_error_classifier::classify_text(msg).stop_reason == "billing_error";
        if let Some(hit) = classify_by_error_class(Some(ec), has_billing_marker) {
            return Some(hit);
        }
    }
    let classified = crate::provider_error_classifier::classify_text(msg);
    map_classifier_to_cooldown(&classified)
}

/// Punto unico per i worker/purpose che invocano LLM fuori dallo stream agente:
/// classifica l'errore (error_class strutturato o testo) e applica cooldown provider.
pub(crate) fn handle_provider_llm_failure(
    provider: &str,
    error_class: Option<&str>,
    message: &str,
) {
    if let Some((ec, kind, reason)) = classify_provider_error(error_class, message) {
        let short_secs = crate::provider_cooldown::provider_health_timings().slow_cooldown_s;
        apply_provider_cooldown(provider, ec, kind, reason, short_secs);
    }
}

/// Applica il cooldown al provider in base alla classificazione dell'errore.
///
/// Punto unico (regola L) della doppia logica di cooldown che prima era
/// duplicata tra il ramo `assistant_delta` (errore propagato come content
/// `[Error: ...]`) e il ramo `error` dello stream SSE. `short_secs` e' la durata
/// del cooldown breve scelta dal chiamante (DB-driven `slow_cooldown_s` per il
/// content-error, valore fisso per il ramo error). L'assegnazione di
/// `last_error`/`last_error_class` resta al chiamante: ha semantica diversa nei
/// due punti.
fn apply_provider_cooldown(
    provider: &str,
    err_class: &str,
    kind: CooldownKind,
    human_reason: &str,
    short_secs: u64,
) {
    let provider_key = provider.split('/').next().unwrap_or(provider);
    match kind {
        CooldownKind::Long => {
            crate::provider_cooldown::put_provider_in_long_cooldown(provider_key, human_reason);
            tracing::warn!(
                "Provider '{}' COOLDOWN LUNGO 6h ({}): {}",
                provider,
                err_class,
                human_reason
            );
        }
        CooldownKind::Short => {
            crate::provider_cooldown::put_provider_in_short_cooldown(
                provider_key,
                human_reason,
                short_secs,
            );
            tracing::warn!(
                "Provider '{}' COOLDOWN BREVE {}s ({}): {}",
                provider,
                short_secs,
                err_class,
                human_reason
            );
        }
    }
}

/// Gestisce l'evento SSE `usage`: aggiorna gli accumulatori token/costo e
/// ri-emette lo snapshot al frontend come `usage_snapshot`.
///
/// Token cumulativi live emessi dal brain a ogni iterazione executor (vedi
/// brain/grpc_server/routes/agent.py). Vengono ritrasmessi al frontend come
/// meta_step kind="usage_snapshot" che chat_agent.rs mappa all'evento SSE
/// `agent_usage`, cosi' la barra context si aggiorna in tempo reale senza
/// polling. Riusa il campo meta_step esistente per non toccare i call site di
/// AgentStepEvent (regola: niente patch speculative).
#[allow(clippy::too_many_arguments)]
fn handle_usage_event(
    evt: &Value,
    step_tx: &broadcast::Sender<AgentStepEvent>,
    run_id_str: &str,
    acc_prompt_tokens: &mut u32,
    acc_completion_tokens: &mut u32,
    acc_total_tokens: &mut u32,
    acc_total_cost: &mut f64,
    last_prompt_tokens: &mut Option<u32>,
) {
    let u32_field = |k: &str| evt.get(k).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let prompt_t = u32_field("prompt_tokens");
    let completion_t = u32_field("completion_tokens");
    let total_t = u32_field("total_tokens");
    let cost_t = evt
        .get("total_cost")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let last_pt = u32_field("last_prompt_tokens");
    // Mantieni gli accumulatori coerenti col risultato finale anche
    // se end_turn non dovesse arrivare (es. stream troncato).
    if total_t > 0 {
        *acc_prompt_tokens = prompt_t;
        *acc_completion_tokens = completion_t;
        *acc_total_tokens = total_t;
        *acc_total_cost = cost_t;
    }
    if last_pt > 0 {
        *last_prompt_tokens = Some(last_pt);
    }
    let _ = step_tx.send(AgentStepEvent {
        run_id: run_id_str.to_string(),
        step: None,
        trace: None,
        is_final: false,
        token_delta: None,
        thinking_delta: None,
        meta_step: Some(AgentMetaStep {
            kind: "usage_snapshot".to_string(),
            title: String::new(),
            payload: json!({
                "totalTokens": total_t,
                "promptTokens": prompt_t,
                "completionTokens": completion_t,
                "lastPromptTokens": last_pt,
                "totalCostUsd": cost_t,
            }),
            correlation_id: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        }),
    });
}

/// Gestisce l'evento SSE `meta_step`: ritrasmette lo step semantico
/// (plan/routing/clarify/fallback/reflection) al frontend come AgentMetaStep.
///
/// La persistenza su nexus_agent_meta_steps avviene lato brain Python che ha
/// gia' accesso al DB (vedi event_bus.py). Se il `kind` e' vuoto lo step viene
/// scartato (equivalente al `continue` originale: nessun codice segue nel loop).
fn handle_meta_step_event(
    evt: &Value,
    step_tx: &broadcast::Sender<AgentStepEvent>,
    run_id_str: &str,
    payload: &str,
) {
    let kind = evt
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if kind.is_empty() {
        tracing::warn!("meta_step senza kind, scarto: {payload}");
        return;
    }
    let title = evt
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let payload_val = evt.get("payload").cloned().unwrap_or(json!({}));
    let correlation_id = evt
        .get("correlation_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let created_at = evt
        .get("created_at")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let _ = step_tx.send(AgentStepEvent {
        run_id: run_id_str.to_string(),
        step: None,
        trace: None,
        is_final: false,
        token_delta: None,
        thinking_delta: None,
        meta_step: Some(AgentMetaStep {
            kind,
            title,
            payload: payload_val,
            correlation_id,
            created_at,
        }),
    });
}

/// Contesto read-only necessario alla gestione dell'evento `end_turn`.
struct EndTurnCtx<'a> {
    db: &'a PgPool,
    session_id: Uuid,
    run_id: Uuid,
    run_id_str: &'a str,
    iteration: u32,
    conversation_history_len: usize,
    tools_json: &'a Value,
    last_text_segment: &'a str,
    step_tx: &'a broadcast::Sender<AgentStepEvent>,
}

/// Riferimenti agli accumulatori del run che l'evento `end_turn` aggiorna.
/// Raggruppati in una struct per non superare il limite di parametri (clippy)
/// e per mantenere `run_via_brain` privo della logica di questo ramo.
struct EndTurnOutputs<'a> {
    ended: &'a mut bool,
    acc_prompt_tokens: &'a mut u32,
    acc_completion_tokens: &'a mut u32,
    acc_total_tokens: &'a mut u32,
    acc_total_cost: &'a mut f64,
    last_prompt_tokens: &'a mut Option<u32>,
    nexus_task_type: &'a mut Option<String>,
    nexus_agent_type: &'a mut Option<String>,
    forced_close_unverified: &'a mut bool,
    final_gate_passed: &'a mut bool,
    last_error_class: &'a mut Option<String>,
    declared_outcome: &'a mut Option<String>,
    declared_summary: &'a mut Option<String>,
    effective_provider: &'a mut String,
    effective_model: &'a mut String,
    last_stop_reason: &'a mut Option<String>,
    trace_seq: &'a mut i32,
}

/// Gestisce l'evento SSE `end_turn`: marca la fine della fase generativa
/// (mig 0388), aggiorna token/costo, i metadata routing, l'esito dichiarato e i
/// provider/model effettivi post-cascade, quindi costruisce e persiste la
/// traccia gateway del turno (FIX D7) ri-emettendola live. Estratto da
/// `run_via_brain` per ridurne complessita' e lunghezza (comportamento
/// invariato).
/// Marca `generation_ended_at` sul run (mig 0388): da qui il frontend e' libero
/// e reflection/learner girano in post-processing. Il guard anti-run-concorrente
/// (handlers.rs) esclude i run con `generation_ended_at` valorizzato, evitando il
/// 409 "la chat sembra libera". Best-effort: un errore qui non deve interrompere
/// lo stream. `IS NULL`: marca una volta sola. Separazione DB: agent_runs vive
/// sul pool del progetto (risolto da session_id).
async fn mark_generation_ended(db: &PgPool, session_id: Uuid, run_id: Uuid) {
    let run_pool = match crate::project_db_routes::project_data_pool_by_session_from(db, session_id)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            // Best-effort dichiarato: la marca si salta con WARN, niente
            // fallback al meta (le tabelle run sul meta sono vuote).
            tracing::warn!(
                session_id = %session_id,
                run_id = %run_id,
                error = %e,
                "mark_generation_ended: DB progetto non disponibile, salto"
            );
            return;
        }
    };
    let _ = sqlx::query(
        "UPDATE agent_runs SET generation_ended_at = NOW() \
         WHERE id = $1 AND generation_ended_at IS NULL",
    )
    .bind(run_id)
    .execute(&run_pool)
    .await;
}

/// Copia dall'evento `end_turn` token/costo e metadata routing/esito negli
/// accumulatori del run. Nessun IO: sola trasformazione in-memory.
fn apply_end_turn_metadata(evt: &Value, out: &mut EndTurnOutputs<'_>) {
    apply_end_turn_tokens(evt, out);
    apply_end_turn_routing_meta(evt, out);
}

/// Copia token cumulativi, costo e token dell'ultima iterazione dall'evento.
fn apply_end_turn_tokens(evt: &Value, out: &mut EndTurnOutputs<'_>) {
    *out.acc_prompt_tokens = evt
        .get("prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    *out.acc_completion_tokens = evt
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    *out.acc_total_tokens = evt
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    *out.acc_total_cost = evt
        .get("total_cost")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    // Token prompt dell'ultima iterazione (per context ratio UI)
    *out.last_prompt_tokens = evt
        .get("last_prompt_tokens")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);
}

/// Copia i metadata di routing/esito (task/agent type, gate, esito dichiarato,
/// provider/model effettivi post-cascade, stop_reason) dall'evento end_turn.
fn apply_end_turn_routing_meta(evt: &Value, out: &mut EndTurnOutputs<'_>) {
    // B5: legge metadata routing propagati dal brain Python
    if let Some(tt) = evt.get("nexus_task_type").and_then(|v| v.as_str()) {
        *out.nexus_task_type = Some(tt.to_string());
    }
    if evt
        .get("forced_close_unverified")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        *out.forced_close_unverified = true;
    }
    if evt
        .get("final_gate_passed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        *out.final_gate_passed = true;
    }
    if let Some(at) = evt.get("nexus_agent_type").and_then(|v| v.as_str()) {
        *out.nexus_agent_type = Some(at.to_string());
    }
    // WAVE 2.2: error_class infrastruttura propagato nell'end_turn
    // (ToolRunner down): mcp-core non scala i provider.
    if let Some(ec) = evt.get("error_class").and_then(|v| v.as_str()) {
        if !ec.trim().is_empty() {
            *out.last_error_class = Some(ec.to_string());
        }
    }
    // WAVE 3.2: esito dichiarato {outcome, summary, ...}.
    if let Some(d) = evt.get("declared_outcome").and_then(|v| v.as_object()) {
        if let Some(o) = d.get("outcome").and_then(|v| v.as_str()) {
            *out.declared_outcome = Some(o.to_string());
        }
        if let Some(s) = d.get("summary").and_then(|v| v.as_str()) {
            if !s.trim().is_empty() {
                *out.declared_summary = Some(s.to_string());
            }
        }
    }
    apply_end_turn_effective(evt, out);
}

/// Copia i provider/model EFFETTIVI post cascade-fallback sticky (sovrascrivono i
/// valori iniziali nel risultato finale) e lo stop_reason se non gia' impostato.
fn apply_end_turn_effective(evt: &Value, out: &mut EndTurnOutputs<'_>) {
    if let Some(pu) = evt.get("provider_used").and_then(|v| v.as_str()) {
        if !pu.trim().is_empty() {
            *out.effective_provider = pu.to_string();
        }
    }
    if let Some(mu) = evt.get("model_used").and_then(|v| v.as_str()) {
        if !mu.trim().is_empty() {
            *out.effective_model = mu.to_string();
        }
    }
    if out.last_stop_reason.is_none() {
        *out.last_stop_reason = Some(
            evt.get("stop_reason")
                .and_then(|v| v.as_str())
                .unwrap_or("end_turn")
                .to_string(),
        );
    }
}

/// FIX D7: costruisce l'`AITraceEvent` con provider/model EFFETTIVI (post
/// cascade/escalation), token e stop_reason del turno, lo PERSISTE su
/// nexus_agent_traces (best-effort, punto unico trace_store — regola L) e lo
/// ri-emette LIVE cosi' il trace panel sopravvive al refresh. response_text
/// troncato (regola F: niente leak di contenuti integri nel payload persistito).
async fn persist_and_emit_turn_trace(ctx: &EndTurnCtx<'_>, out: &mut EndTurnOutputs<'_>) {
    let trace = AITraceEvent {
        run_id: ctx.run_id_str.to_string(),
        iteration: ctx.iteration,
        provider: out.effective_provider.clone(),
        model: out.effective_model.clone(),
        messages_sent: ctx.conversation_history_len as u32,
        tools_count: ctx.tools_json.as_array().map(|a| a.len()).unwrap_or(0) as u32,
        response_text: ctx.last_text_segment.chars().take(2000).collect(),
        tool_calls: Vec::new(),
        stop_reason: out.last_stop_reason.clone().unwrap_or_default(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        input_tokens: *out.acc_prompt_tokens,
        output_tokens: *out.acc_completion_tokens,
        cache_read_tokens: 0,
    };
    // Separazione DB: nexus_agent_traces vive nel DB del progetto; qui `db` e' il
    // meta -> risolvi by_session (directory O(1)), trace_store NON ri-risolve
    // (convenzione: pool gia' risolto).
    if let Ok(payload) = serde_json::to_value(&trace) {
        match crate::project_db_routes::project_data_pool_by_session_from(ctx.db, ctx.session_id)
            .await
        {
            Ok(trace_pool) => {
                crate::trace_store::persist_trace(
                    &trace_pool,
                    ctx.session_id,
                    ctx.run_id,
                    *out.trace_seq,
                    &payload,
                )
                .await;
            }
            Err(e) => {
                // Persist best-effort: la traccia non si salva ma l'emissione
                // LIVE qui sotto avviene comunque (il pannello live funziona,
                // il refresh non rivedra' questo turno).
                tracing::warn!(
                    session_id = %ctx.session_id,
                    error = %e,
                    "persist_and_emit_turn_trace: DB progetto non disponibile, traccia non persistita"
                );
            }
        }
    }
    *out.trace_seq += 1;
    // Emissione LIVE: lo STESSO evento che il frontend mostra nel pannello tracce
    // (chat_agent.rs mappa trace.is_some() -> `agent_trace`). Live e refresh ora
    // coincidono.
    let _ = ctx.step_tx.send(AgentStepEvent {
        run_id: ctx.run_id_str.to_string(),
        step: None,
        trace: Some(trace),
        is_final: false,
        token_delta: None,
        thinking_delta: None,
        meta_step: None,
    });
}

/// Gestisce l'evento SSE `end_turn` orchestrando le tre fasi (marca fine
/// generazione, applica i metadata, persiste ed emette la traccia). Estratto da
/// `run_via_brain` per ridurne complessita' e lunghezza (comportamento
/// invariato).
async fn handle_end_turn(evt: &Value, ctx: &EndTurnCtx<'_>, out: &mut EndTurnOutputs<'_>) {
    *out.ended = true;
    mark_generation_ended(ctx.db, ctx.session_id, ctx.run_id).await;
    apply_end_turn_metadata(evt, out);
    persist_and_emit_turn_trace(ctx, out).await;
}

/// Deriva lo status canonico del run (macchina a stati, mig 0386) dai segnali
/// strutturati raccolti durante lo stream. Estratto da `run_via_brain` per
/// isolarne la logica di branching (comportamento invariato).
fn determine_run_status(
    last_error: &Option<String>,
    last_stop_reason: &Option<String>,
    forced_close_unverified: bool,
    declared_outcome: &Option<String>,
    ended: bool,
    final_gate_passed: bool,
) -> AgentRunStatus {
    if last_error.is_some() {
        // Distingui la causa di terminazione con errore
        return match last_stop_reason.as_deref() {
            Some("no_capable_provider") | Some("provider_unavailable") => {
                AgentRunStatus::ProviderUnavailable
            }
            _ => AgentRunStatus::Failed,
        };
    }
    if forced_close_unverified
        || matches!(
            last_stop_reason.as_deref(),
            Some("loop_detected") | Some("loop_aborted") | Some("loop_abort")
        )
    {
        // Esito canonico (macchina a stati, mig 0386): un abort anti-loop NON e' un
        // successo ne' un errore infrastrutturale, ma un FALLIMENTO DIAGNOSTICATO.
        // Il recap M44 dell'executor porta sempre esito + file toccati + prossimo
        // passo. `forced_close_unverified` e' il segnale autoritativo: copre anche
        // il caso in cui il final_gate ha riscritto stop_reason a "end_turn" sul
        // ramo forced_close (prima un abort finiva erroneamente come Completed).
        return AgentRunStatus::FailedDiagnosed;
    }
    if matches!(
        declared_outcome.as_deref(),
        Some("blocked") | Some("needs_input")
    ) {
        // WAVE 3.2: il modello ha DICHIARATO via task_complete di essere bloccato
        // (causa esterna mancante) o di aver bisogno di input. Esito canonico
        // BlockedNeedsInput, indipendente dalla lingua del testo. Sostituisce
        // l'inferenza lessicale resigned_patterns per questo caso.
        return AgentRunStatus::BlockedNeedsInput;
    }
    if ended && final_gate_passed {
        // Verifica E2E (final_gate) superata: successo verificato (mig 0386).
        return AgentRunStatus::CompletedVerified;
    }
    // Sia il run terminato senza final_gate sia il fallback non-ended
    // convergono su Completed (i rami erano identici; semantica invariata).
    AgentRunStatus::Completed
}

/// Costruisce il messaggio-risposta a partire dall'errore quando il run non ha
/// prodotto testo. Fix M23: distingue le due cause piu' comuni di interruzione
/// SSE dal brain (chunk decode error, silenzio prolungato) dal generico errore
/// di elaborazione, dando indicazioni utili. Lo stato del progetto e gli step
/// gia' eseguiti sono persistiti: l'utente puo' riprendere senza perdere lavoro.
fn build_answer_from_error(err: &str) -> String {
    let lower = err.to_ascii_lowercase();
    if lower.starts_with("sse chunk:")
        || lower.contains("error decoding response body")
        || lower.contains("connection reset")
        || lower.contains("connection closed")
    {
        format!(
            "Il flusso dal brain si e' interrotto prima del completamento. \
             Gli step gia eseguiti e i file generati sono salvati. \
             Premi Invio (o scrivi \"continua\") per riprendere da dove eri.\n\n\
             *Dettaglio tecnico: {}*",
            sanitize_error_for_user(err)
        )
    } else if lower.contains("silenzioso") || lower.contains("timeout") {
        format!(
            "Il brain non ha risposto entro il timeout configurato. \
             Verifica che sia attivo (`curl http://localhost:8001/health`) \
             e premi Invio per riprendere.\n\n\
             *Dettaglio tecnico: {}*",
            sanitize_error_for_user(err)
        )
    } else {
        format!(
            "Si e' verificato un errore durante l'elaborazione della richiesta. \
             Riprova tra qualche secondo oppure cambia modello.\n\n\
             *Dettaglio tecnico: {}*",
            sanitize_error_for_user(err)
        )
    }
}

/// Contesto read-only condiviso da tutti gli handler di evento SSE.
struct SseCtx<'a> {
    db: &'a PgPool,
    session_id: Uuid,
    run_id: Uuid,
    run_id_str: &'a str,
    provider: &'a str,
    conversation_history_len: usize,
    tools_json: &'a Value,
    step_tx: &'a broadcast::Sender<AgentStepEvent>,
}

/// Stato mutabile del run accumulato dagli handler di evento SSE. Raggruppato in
/// una struct per centralizzare in `dispatch_sse_event` lo smistamento (regola L)
/// e togliere da `run_via_brain` complessita' e lunghezza.
struct SseState<'a> {
    final_answer: &'a mut String,
    last_text_segment: &'a mut String,
    accumulated_reasoning: &'a mut String,
    trace_seq: &'a mut i32,
    steps: &'a mut Vec<AgentStep>,
    iteration: &'a mut u32,
    ended: &'a mut bool,
    last_error: &'a mut Option<String>,
    last_error_class: &'a mut Option<String>,
    last_stop_reason: &'a mut Option<String>,
    acc_prompt_tokens: &'a mut u32,
    acc_completion_tokens: &'a mut u32,
    acc_total_tokens: &'a mut u32,
    acc_total_cost: &'a mut f64,
    last_prompt_tokens: &'a mut Option<u32>,
    nexus_task_type: &'a mut Option<String>,
    nexus_agent_type: &'a mut Option<String>,
    forced_close_unverified: &'a mut bool,
    final_gate_passed: &'a mut bool,
    declared_outcome: &'a mut Option<String>,
    declared_summary: &'a mut Option<String>,
    effective_provider: &'a mut String,
    effective_model: &'a mut String,
}

/// Gestisce l'evento `assistant_delta`: accumula il testo e, se il brain ha
/// convertito un errore provider in content `[Error: ...]`, mette il provider in
/// cooldown (senza questo il LED resterebbe verde con provider fuori uso).
fn handle_assistant_delta(evt: &Value, ctx: &SseCtx<'_>, state: &mut SseState<'_>) {
    let text = evt
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if text.is_empty() {
        return;
    }
    let trimmed = text.trim_start();
    if trimmed.starts_with("[Error:") || trimmed.starts_with("[error:") {
        if let Some((err_class, kind, human_reason)) = classify_provider_error(None, &text) {
            // Durata cooldown breve DB-driven (regola G): slow_cooldown_s.
            let short_secs = crate::provider_cooldown::provider_health_timings().slow_cooldown_s;
            apply_provider_cooldown(ctx.provider, err_class, kind, human_reason, short_secs);
            *state.last_error_class = Some(err_class.to_string());
            *state.last_error = Some(text.clone());
        }
    }
    state.final_answer.push_str(&text);
    state.last_text_segment.push_str(&text);
    let _ = ctx.step_tx.send(AgentStepEvent {
        run_id: ctx.run_id_str.to_string(),
        step: None,
        trace: None,
        is_final: false,
        token_delta: Some(text),
        thinking_delta: None,
        meta_step: None,
    });
}

/// Gestisce l'evento `tool_use`: crea un nuovo step Running e lo emette.
fn handle_tool_use(evt: &Value, ctx: &SseCtx<'_>, state: &mut SseState<'_>) {
    state.last_text_segment.clear();
    *state.iteration = state.iteration.saturating_add(1);
    let tool_name = evt
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let input = evt.get("input").cloned().unwrap_or(json!({}));
    let step = AgentStep {
        run_id: ctx.run_id_str.to_string(),
        step_index: *state.iteration,
        tool_name,
        tool_input: input,
        tool_result: None,
        status: AgentStepStatus::Running,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let _ = ctx.step_tx.send(AgentStepEvent {
        run_id: ctx.run_id_str.to_string(),
        step: Some(step.clone()),
        trace: None,
        is_final: false,
        token_delta: None,
        thinking_delta: None,
        meta_step: None,
    });
    state.steps.push(step);
}

/// Gestisce l'evento `thinking_delta`: accumula il ragionamento (FIX D4) e lo
/// ri-emette live.
fn handle_thinking_delta(evt: &Value, ctx: &SseCtx<'_>, state: &mut SseState<'_>) {
    let text = evt
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if text.is_empty() {
        return;
    }
    // Accumula per la persistenza (FIX D4): il blocco "Ragionamento" deve
    // sopravvivere al refresh.
    state.accumulated_reasoning.push_str(&text);
    let _ = ctx.step_tx.send(AgentStepEvent {
        run_id: ctx.run_id_str.to_string(),
        step: None,
        trace: None,
        is_final: false,
        token_delta: None,
        thinking_delta: Some(text),
        meta_step: None,
    });
}

/// Gestisce l'evento `tool_result`: aggiorna l'ultimo step Running con l'esito.
fn handle_tool_result(evt: &Value, ctx: &SseCtx<'_>, state: &mut SseState<'_>) {
    let tool_use_id = evt
        .get("tool_use_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let content = evt
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let is_error = evt
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // Aggiorna l'ultimo step in Running (match by step_index non disponibile
    // qui — aggiorniamo l'ultimo).
    if let Some(last) = state
        .steps
        .iter_mut()
        .rev()
        .find(|s| s.status == AgentStepStatus::Running)
    {
        last.tool_result = Some(content.clone());
        last.status = if is_error {
            AgentStepStatus::Failed
        } else {
            AgentStepStatus::Completed
        };
        let _ = ctx.step_tx.send(AgentStepEvent {
            run_id: ctx.run_id_str.to_string(),
            step: Some(last.clone()),
            trace: None,
            is_final: false,
            token_delta: None,
            thinking_delta: None,
            meta_step: None,
        });
    } else {
        tracing::warn!(
            "tool_result senza step Running (tool_use_id={})",
            tool_use_id
        );
    }
}

/// Gestisce l'evento `error`: logga, classifica l'errore provider (cooldown
/// breve fisso 60s per i transient del ramo error) e registra stop_reason/error.
fn handle_error_event(evt: &Value, ctx: &SseCtx<'_>, state: &mut SseState<'_>) {
    let msg = evt
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("errore brain")
        .to_string();
    // Lettura del campo strutturato `error_class` propagato dal brain (vedi
    // brain/providers/error_handler.py::format_error_result). Ha priorita' sul
    // pattern matching testuale: i messaggi possono essere localizzati ma
    // error_class resta stabile.
    let err_class_raw = evt.get("error_class").and_then(|v| v.as_str());
    tracing::error!("brain SSE error (error_class={:?}): {msg}", err_class_raw);

    if let Some((err_class, kind, human_reason)) = classify_provider_error(err_class_raw, &msg) {
        apply_provider_cooldown(ctx.provider, err_class, kind, human_reason, 60);
        *state.last_error_class = Some(err_class.to_string());
    } else if let Some(c) = err_class_raw {
        // error_class non riconosciuto: lo propaghiamo comunque per audit.
        *state.last_error_class = Some(c.to_string());
    }

    *state.last_stop_reason = Some("error".to_string());
    *state.last_error = Some(msg);
}

/// Adatta `SseCtx`/`SseState` a `EndTurnCtx`/`EndTurnOutputs` e delega a
/// `handle_end_turn`. Isolato dal dispatch per tenere `dispatch_sse_event`
/// compatto (il mapping dei riferimenti e' verboso ma meccanico).
async fn dispatch_end_turn(evt: &Value, ctx: &SseCtx<'_>, state: &mut SseState<'_>) {
    let end_ctx = EndTurnCtx {
        db: ctx.db,
        session_id: ctx.session_id,
        run_id: ctx.run_id,
        run_id_str: ctx.run_id_str,
        iteration: *state.iteration,
        conversation_history_len: ctx.conversation_history_len,
        tools_json: ctx.tools_json,
        last_text_segment: state.last_text_segment,
        step_tx: ctx.step_tx,
    };
    let mut out = EndTurnOutputs {
        ended: state.ended,
        acc_prompt_tokens: state.acc_prompt_tokens,
        acc_completion_tokens: state.acc_completion_tokens,
        acc_total_tokens: state.acc_total_tokens,
        acc_total_cost: state.acc_total_cost,
        last_prompt_tokens: state.last_prompt_tokens,
        nexus_task_type: state.nexus_task_type,
        nexus_agent_type: state.nexus_agent_type,
        forced_close_unverified: state.forced_close_unverified,
        final_gate_passed: state.final_gate_passed,
        last_error_class: state.last_error_class,
        declared_outcome: state.declared_outcome,
        declared_summary: state.declared_summary,
        effective_provider: state.effective_provider,
        effective_model: state.effective_model,
        last_stop_reason: state.last_stop_reason,
        trace_seq: state.trace_seq,
    };
    handle_end_turn(evt, &end_ctx, &mut out).await;
}



/// Rende un messaggio di errore API leggibile per l'utente finale.
/// Rimuove i dettagli interni (indici messaggi, nomi interni di blocchi)
/// e restituisce una descrizione sintetica.
pub(crate) fn sanitize_error_for_user(raw: &str) -> String {
    // Errore tool_use/tool_result mismatch (Anthropic)
    if raw.contains("tool_use ids were found without tool_result") {
        return "conversazione interrotta per un problema di sincronizzazione tra i passaggi interni dell'agente".to_string();
    }
    // Errore di contesto troppo lungo
    if raw.contains("context length")
        || raw.contains("too many tokens")
        || raw.contains("maximum context")
    {
        return "la conversazione ha superato la lunghezza massima consentita dal modello"
            .to_string();
    }
    // Errore di rate limit
    if raw.contains("rate_limit") || raw.contains("429") || raw.contains("Too Many Requests") {
        return "troppe richieste al provider AI, attendere qualche secondo".to_string();
    }
    // Errore di autenticazione
    if raw.contains("401") || raw.contains("authentication") || raw.contains("invalid_api_key") {
        return "errore di autenticazione con il provider AI".to_string();
    }
    // Errore 5xx del provider (503, 502, 500, 504, ecc.)
    if raw.contains("Internal server error")
        || raw.contains("internal server error")
        || raw.contains("service unavailable")
        || raw.contains("Service Unavailable")
        || raw.contains("bad gateway")
        || raw.contains("Bad Gateway")
        || raw.contains("Gateway Timeout")
        || raw.contains("gateway timeout")
        || raw.contains("503")
        || raw.contains("502")
        || raw.contains("504")
    {
        return "il provider AI e' temporaneamente non disponibile (errore server). Il sistema sta provando con un altro provider.".to_string();
    }
    // Errore generico: tronca a 200 caratteri e rimuovi stack trace
    let clean = raw.lines().next().unwrap_or(raw);
    if clean.len() > 200 {
        format!("{}...", &clean[..200])
    } else {
        clean.to_string()
    }
}




// =====================================================================
// TEST L3: Tool gating per automation_mode
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::AutomationMode;

    /// Whitelist di test (replica della seed DB in mig 0132).
    /// In produzione la whitelist autoritativa viene letta da
    /// `settings.automation.study_mode_readonly_tools`.
    fn test_whitelist() -> Vec<String> {
        vec![
            "read_file",
            "read_file_lines",
            "list_files",
            "search_in_files",
            "search_codebase_semantic",
            "get_project_structure",
            "get_file_diff",
            "git_status",
            "git_log",
            "git_diff",
            "list_services",
            "read_service_output",
            "nexus_mcp_tool_search",
            "list_profiles",
            "get_profile",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    fn make_test_tools() -> Value {
        json!([
            { "name": "read_file", "description": "..." },
            { "name": "list_files", "description": "..." },
            { "name": "search_in_files", "description": "..." },
            { "name": "write_file", "description": "..." },
            { "name": "edit_file", "description": "..." },
            { "name": "run_command", "description": "..." },
            { "name": "git_status", "description": "..." },
            { "name": "git_commit", "description": "..." },
            { "name": "delete_file", "description": "..." },
            { "name": "nexus_mcp_tool_search", "description": "..." },
        ])
    }

    #[test]
    fn automatic_mode_non_filtra_nessun_tool() {
        // Modalita' automatic: l'utente vuole massima autonomia, tutti i
        // tool restano esposti (write_file, edit_file, run_command, ecc.).
        let tools = make_test_tools();
        let original_len = tools.as_array().unwrap().len();
        let wl = test_whitelist();
        let filtered = filter_tools_by_automation_mode(tools, &AutomationMode::Automatic, &wl);
        assert_eq!(filtered.as_array().unwrap().len(), original_len);
    }

    #[test]
    fn confirm_mode_non_filtra_lato_rust() {
        // Confirm: il gating e' a livello HITL nel brain Python (interrupt prima
        // di edit/write). Lato Rust non filtriamo, esponiamo tutto.
        let tools = make_test_tools();
        let original_len = tools.as_array().unwrap().len();
        let wl = test_whitelist();
        let filtered = filter_tools_by_automation_mode(tools, &AutomationMode::Confirm, &wl);
        assert_eq!(filtered.as_array().unwrap().len(), original_len);
    }

    #[test]
    fn study_mode_filtra_a_solo_readonly() {
        // Study: l'agente DEVE solo analizzare. Filtraggio difensivo:
        // anche se il modello ignora le istruzioni del prompt, NON puo'
        // chiamare un tool che non gli e' stato esposto.
        let tools = make_test_tools();
        let wl = test_whitelist();
        let filtered = filter_tools_by_automation_mode(tools, &AutomationMode::Study, &wl);
        let names: Vec<&str> = filtered
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        // Devono restare i tool read-only
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"list_files"));
        assert!(names.contains(&"search_in_files"));
        assert!(names.contains(&"git_status"));
        assert!(names.contains(&"nexus_mcp_tool_search"));
        // NON devono esserci tool che scrivono / eseguono / eliminano
        assert!(
            !names.contains(&"write_file"),
            "write_file vietato in study"
        );
        assert!(!names.contains(&"edit_file"), "edit_file vietato in study");
        assert!(
            !names.contains(&"run_command"),
            "run_command vietato in study"
        );
        assert!(
            !names.contains(&"git_commit"),
            "git_commit vietato in study"
        );
        assert!(
            !names.contains(&"delete_file"),
            "delete_file vietato in study"
        );
    }

    #[test]
    fn study_mode_su_array_vuoto_ritorna_array_vuoto() {
        let empty = json!([]);
        let wl = test_whitelist();
        let filtered = filter_tools_by_automation_mode(empty, &AutomationMode::Study, &wl);
        assert!(filtered.as_array().unwrap().is_empty());
    }

    #[test]
    fn study_mode_su_non_array_ritorna_array_vuoto() {
        // Robustezza: se il JSON in input non e' un array (regressione/bug),
        // ritorniamo array vuoto invece di crashare.
        let bad = json!({"not": "an array"});
        let wl = test_whitelist();
        let filtered = filter_tools_by_automation_mode(bad, &AutomationMode::Study, &wl);
        assert!(filtered.as_array().unwrap().is_empty());
    }

    #[test]
    fn study_mode_whitelist_db_driven_personalizzabile() {
        // Caso paradigmatico DB-driven: l'admin puo' restringere o ampliare
        // la whitelist via UPDATE settings senza redeploy. Test simula una
        // whitelist piu' ristretta.
        let tools = make_test_tools();
        let restricted_wl = vec!["read_file".to_string()];
        let filtered =
            filter_tools_by_automation_mode(tools, &AutomationMode::Study, &restricted_wl);
        let names: Vec<&str> = filtered
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        // Solo read_file passa (whitelist personalizzata)
        assert_eq!(names, vec!["read_file"]);
    }

    #[test]
    fn study_mode_whitelist_vuota_blocca_tutti_i_tool() {
        // Edge case: se l'admin svuota la whitelist nel DB, l'agente in
        // study mode non puo' chiamare nessun tool (massima sicurezza).
        let tools = make_test_tools();
        let empty_wl: Vec<String> = Vec::new();
        let filtered = filter_tools_by_automation_mode(tools, &AutomationMode::Study, &empty_wl);
        assert!(filtered.as_array().unwrap().is_empty());
    }

    #[test]
    fn study_mode_filtra_tool_mcp_non_in_whitelist() {
        // Tool MCP esterni (non in whitelist) vengono filtrati anche se
        // sembrano innocui. Whitelist conservativa per design.
        let tools = json!([
            { "name": "read_file" },
            { "name": "external_mcp_tool_that_writes" },
            { "name": "send_email" },
        ]);
        let wl = test_whitelist();
        let filtered = filter_tools_by_automation_mode(tools, &AutomationMode::Study, &wl);
        let names: Vec<&str> = filtered
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        assert_eq!(names, vec!["read_file"]);
    }

    #[test]
    fn fallback_whitelist_contiene_tool_minimi_sicuri() {
        // Il fallback hardcoded (STUDY_MODE_READONLY_TOOLS_FALLBACK) e' usato
        // solo se il DB e' down. Deve contenere ALMENO i tool di base per
        // permettere all'agente di leggere file e cercare nel codebase.
        let must_have = ["read_file", "list_files", "search_in_files", "git_status"];
        for tool in &must_have {
            assert!(
                STUDY_MODE_READONLY_TOOLS_FALLBACK.contains(tool),
                "tool minimo '{}' mancante dalla fallback whitelist",
                tool
            );
        }
        // E NON deve contenere tool che scrivono
        let must_not_have = ["write_file", "edit_file", "run_command", "delete_file"];
        for tool in &must_not_have {
            assert!(
                !STUDY_MODE_READONLY_TOOLS_FALLBACK.contains(tool),
                "tool pericoloso '{}' presente nel fallback (deve essere escluso)",
                tool
            );
        }
    }

    #[test]
    fn is_o_series_rileva_modelli_reasoning() {
        assert!(is_o_series_model("o3"));
        assert!(is_o_series_model("o3-mini"));
        assert!(is_o_series_model("O3"));
        assert!(is_o_series_model("o1"));
        assert!(is_o_series_model("o1-preview"));
        assert!(is_o_series_model("o4-mini"));
        assert!(is_o_series_model("o4-mini-2025-04-16"));
        // NON o-series
        assert!(!is_o_series_model("gpt-4o"));
        assert!(!is_o_series_model("gpt-4o-mini"));
        assert!(!is_o_series_model("claude-sonnet-4-6"));
        assert!(!is_o_series_model("gemini-2.5-flash"));
    }

    #[test]
    fn o_series_essential_contiene_discovery_e_write() {
        // I tool essenziali per o-series devono includere sia tool di
        // scrittura (per operare) sia tool di discovery (per scoprire
        // tool aggiuntivi a runtime).
        let must_have = [
            "write_file",
            "edit_file",
            "run_command", // scrittura
            "nexus_mcp_tool_search",
            "nexus_mcp_tool_call", // discovery
            "read_file",
            "list_files", // lettura
        ];
        for tool in &must_have {
            assert!(
                O_SERIES_ESSENTIAL_TOOLS_FALLBACK.contains(tool),
                "tool essenziale '{}' mancante dal fallback o-series",
                tool
            );
        }
    }

    #[test]
    fn filter_tools_by_whitelist_filtra_correttamente() {
        let tools = make_test_tools();
        let whitelist = vec![
            "read_file".to_string(),
            "write_file".to_string(),
            "nexus_mcp_tool_search".to_string(),
        ];
        let filtered = filter_tools_by_whitelist(tools, &whitelist);
        let names: Vec<&str> = filtered
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"nexus_mcp_tool_search"));
        // Non devono esserci tool non in whitelist
        assert!(!names.contains(&"edit_file"));
        assert!(!names.contains(&"run_command"));
    }

    // ── Classificazione errori provider / cooldown ────────────────────────────

    #[test]
    fn quota_429_e_billing_cooldown_lungo() {
        // Caso live: OpenAI risponde HTTP 429 "You exceeded your current quota".
        // E' quota/credito esaurito, NON un rate-limit transitorio: deve dare
        // cooldown lungo come credit_balance_too_low.
        let msg = "Error code: 429 - {'error': {'message': 'You exceeded your current quota, please check your plan and billing details.', 'type': 'insufficient_quota'}}";
        let (class, kind, _) =
            classify_provider_error(None, msg).expect("429 quota deve essere classificato");
        assert_eq!(class, "billing_error");
        assert_eq!(kind, CooldownKind::Long);
    }

    #[test]
    fn rate_limit_429_senza_marker_quota_e_cooldown_breve() {
        // 429 generico (finestra di richieste) senza marker di quota/credito:
        // rate-limit transitorio, cooldown breve.
        let msg = "Error code: 429 - Rate limit reached for requests. Please try again later.";
        let (class, kind, _) =
            classify_provider_error(None, msg).expect("429 rate limit deve essere classificato");
        assert_eq!(class, "rate_limit");
        assert_eq!(kind, CooldownKind::Short);
    }

    #[test]
    fn error_class_rate_limit_ma_messaggio_quota_promosso_a_billing() {
        // Difesa in profondita': anche se il brain (o un provider) ha mappato
        // il 429 a error_class=rate_limit, se il messaggio contiene marker di
        // quota la classe va promossa a billing/cooldown-lungo. Senza questo,
        // l'error_class esplicito short-circuitava il pattern matching e il
        // provider con quota esaurita restava nel cascade (bug live).
        let msg = "You exceeded your current quota, please check your plan and billing details.";
        let (class, kind, _) =
            classify_provider_error(Some("rate_limit"), msg).expect("deve essere classificato");
        assert_eq!(class, "billing_error");
        assert_eq!(kind, CooldownKind::Long);
    }

    #[test]
    fn error_class_rate_limit_senza_marker_resta_rate_limit() {
        // error_class=rate_limit + messaggio senza marker quota: resta
        // rate-limit transitorio (cooldown breve).
        let (class, kind, _) = classify_provider_error(Some("rate_limit"), "Too Many Requests")
            .expect("deve essere classificato");
        assert_eq!(class, "rate_limit");
        assert_eq!(kind, CooldownKind::Short);
    }

    #[test]
    fn insufficient_quota_error_class_e_billing() {
        // error_class esplicito insufficient_quota -> billing/cooldown-lungo.
        let (class, kind, _) = classify_provider_error(Some("insufficient_quota"), "")
            .expect("deve essere classificato");
        assert_eq!(class, "billing_error");
        assert_eq!(kind, CooldownKind::Long);
    }

    #[test]
    fn billing_hard_limit_reached_e_billing() {
        // Marker OpenAI billing_hard_limit_reached -> cooldown lungo.
        let msg = "Error code: 429 - billing_hard_limit_reached";
        let (class, kind, _) =
            classify_provider_error(None, msg).expect("deve essere classificato");
        assert_eq!(class, "billing_error");
        assert_eq!(kind, CooldownKind::Long);
    }
}
