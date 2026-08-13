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
//! 0462/0532) e la prima passata ne ha rimosse 501 righe, dichiarando che «quello
//! che resta non ha mai parlato col brain». Non era vero: restavano 711 righe di
//! handler degli eventi SSE che il brain emetteva — gli `handle_*` di
//! `assistant_delta`/`tool_use`/`end_turn`, le due struct di accumulo del run, la
//! traccia di turno. Il produttore non c'era piu' e nessuno le chiamava, ma il
//! codice restava, e con esso i suoi campi: `declared_summary` veniva scritto da
//! `apply_end_turn_routing_meta` e non era mai letto da nessuno. Rimosse
//! l'08/08/2026 col censimento del compilatore (22 item `never used`).
//!
//! Perche' nessuno le vedeva: in locale `#![cfg_attr(windows, allow(dead_code))]`
//! (lib.rs) le silenzia, e la contromisura dichiarata li' — «il CI Linux continua
//! a catturare il dead-code GENUINO» — non poteva scattare: `verify.yml` muore su
//! `@ai-orchestrator/web-ide#test`, cioe' nella fase turbo, prima che clippy con
//! `-D warnings` venga eseguito.

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::agent_tools::AGENT_TOOLS_JSON;

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

/// Gli scope ammessi da `nexus_verify_change`, nel catalogo che vede QUESTO
/// progetto: `quick`/`full` piu' gli step del suo profilo di verifica.
///
/// Stesso motivo dell'enum `kind` qui sopra, ma il difetto era peggiore: li'
/// il seed statico era un ripiego ragionevole, qui l'enum statico
/// (`typecheck|build|lint|test`) PROMETTEVA valori che l'esecutore rifiuta.
/// Il profilo e' inferito per progetto e i suoi step portano il nome del
/// pacchetto — `lint-frontend`, `typecheck-backend` — quindi un agente che
/// chiedeva `lint`, cioe' esattamente uno dei valori dichiarati nello schema,
/// otteneva `invalid_scope`. Non era il modello a indovinare: era il contratto
/// a mentire, e il modello a credergli (regola Q: lo schema E' il contratto).
///
/// Profilo vuoto (progetto nuovo, DB irraggiungibile) -> resta il SEED, come
/// per i kind: meglio un enum generico che nessuno scope.
fn apply_verify_scope_enum(tools: &mut Value, steps: &[String]) {
    if steps.is_empty() {
        return;
    }
    let mut scopes = vec![Value::String("quick".into()), Value::String("full".into())];
    scopes.extend(steps.iter().map(|s| Value::String(s.clone())));
    let Some(arr) = tools.as_array_mut() else {
        return;
    };
    for t in arr.iter_mut() {
        if t.get("name").and_then(Value::as_str) == Some("nexus_verify_change") {
            if let Some(e) = t.pointer_mut("/input_schema/properties/scope/enum") {
                *e = Value::Array(scopes.clone());
            }
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

    /// Lo scope che il catalogo DICHIARA deve essere uno che l'esecutore
    /// ACCETTA. Il difetto reale: l'enum statico prometteva `lint`, il profilo
    /// del progetto aveva `lint-frontend`, e l'agente che sceglieva un valore
    /// dichiarato riceveva `invalid_scope`.
    ///
    /// MUTAZIONE: se `apply_verify_scope_enum` non viene chiamato (o non trova
    /// il tool), l'enum resta quello statico e questo test rosseggia
    /// mostrando `lint` al posto di `lint-frontend`.
    #[test]
    fn lo_scope_dichiarato_e_quello_del_profilo_del_progetto() {
        let mut tools = json!([
            {"name": "nexus_verify_change", "input_schema": {"properties": {
                "scope": {"type": "string", "enum": ["quick", "full", "typecheck", "build", "lint", "test"]}
            }}},
            {"name": "read_file", "input_schema": {"properties": {}}}
        ]);
        apply_verify_scope_enum(
            &mut tools,
            &[
                "typecheck-backend".to_string(),
                "lint-frontend".to_string(),
                "build-frontend".to_string(),
            ],
        );
        assert_eq!(
            tools[0]["input_schema"]["properties"]["scope"]["enum"],
            json!(["quick", "full", "typecheck-backend", "lint-frontend", "build-frontend"]),
            "l'enum deve elencare gli step REALI del profilo, non quelli generici"
        );
        // Gli scope generici restano: non sono step del profilo, li risolve il
        // tool stesso.
        let dichiarati = tools[0]["input_schema"]["properties"]["scope"]["enum"].clone();
        for generico in ["quick", "full"] {
            assert!(
                dichiarati.as_array().unwrap().iter().any(|v| v == generico),
                "'{generico}' deve restare ammesso"
            );
        }
        // Un valore che l'esecutore rifiuterebbe non deve piu' essere promesso.
        assert!(
            !dichiarati.as_array().unwrap().iter().any(|v| v == "lint"),
            "'lint' non e' uno step di questo profilo: promuoverlo e' cio' che              produceva invalid_scope"
        );
        // Tool non coinvolto intatto.
        assert!(tools[1]["input_schema"]["properties"].get("scope").is_none());
    }

    /// Profilo vuoto (progetto nuovo, DB muto): resta il seed. Meglio un enum
    /// generico che nessuno scope — e' la stessa scelta dei kind.
    #[test]
    fn profilo_vuoto_mantiene_il_seed() {
        let mut tools = json!([
            {"name": "nexus_verify_change", "input_schema": {"properties": {
                "scope": {"enum": ["quick", "full"]}
            }}}
        ]);
        apply_verify_scope_enum(&mut tools, &[]);
        assert_eq!(
            tools[0]["input_schema"]["properties"]["scope"]["enum"],
            json!(["quick", "full"])
        );
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
    // Stessa regola per gli scope di `nexus_verify_change`, che pero' sono
    // per-PROGETTO: il profilo di verifica e' inferito dall'albero, e i suoi
    // step portano il nome del pacchetto.
    apply_verify_scope_enum(
        &mut base_tools,
        &crate::verify_profile::profile_steps(db, project_id)
            .await
            .into_iter()
            .map(|s| s.step)
            .collect::<Vec<_>>(),
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

/// Reason di un cooldown BREVE nato da un SOSPETTO lessicale di credito esaurito.
/// Distinta da [`BILLING_REASON`] perche' descrive un fatto diverso: li' il gateway
/// ha dichiarato il credito finito, qui lo ha solo suggerito la prosa. Tenerle
/// separate serve a chi legge il registro dei cooldown — e a non far passare per
/// accertato cio' che non lo e'.
const BILLING_SOSPETTO_REASON: &str =
    "Sospetto credito o quota esaurita, non accertato: nuovo tentativo a breve";

/// Cosa dice la classe STRUTTURATA sul cooldown del fornitore.
///
/// Esiste per distinguere due risposte che prima erano lo stesso `None`, e che
/// hanno conseguenze opposte: "classe nota, e dice di non toccare il fornitore"
/// contro "classe assente, guarda la prosa". Confuse insieme, un errore
/// model-specific gia' classificato dal gateway (`context_too_long`,
/// `invalid_request`, `not_found`) finiva comunque al ripiego lessicale — e la
/// prosa di un 413 di groq, che nomina la pagina di fatturazione nell'URL di
/// documentazione, lo trasformava in un cooldown billing su un fornitore sano.
/// Un `None` che significa due cose e' un segnale perso (regola M).
enum ClasseStrutturata {
    /// Classe nota: questa e' la decisione. Il testo non la rivede.
    Cooldown(&'static str, CooldownKind, &'static str),
    /// Classe nota, e dice che il fornitore NON c'entra: la richiesta era
    /// malformata, o il modello non esiste, o non ci stava nel contesto. Nessun
    /// cooldown E nessun ripiego: il gateway ha gia' risposto alla domanda.
    NessunCooldown,
    /// Classe assente o fuori vocabolario: e' l'UNICO caso in cui si guarda la prosa.
    Sconosciuta,
}

/// Step 1 della classificazione: `error_class` STRUTTURATO, cioe' la classe che
/// il gateway ha dichiarato alla fonte (status HTTP + codice macchina del
/// provider, via `error_class_from_primary_cause`). Ritorna `None` se non e' uno
/// dei valori noti: allora, e solo allora, si passa al ripiego lessicale.
///
/// La classe strutturata NON viene rivista dal testo. Prima il ramo `rate_limit`
/// accettava un `has_billing_marker` dedotto dalla prosa e, se acceso, promuoveva
/// a `billing_error` + cooldown 6h: il ripiego scavalcava il segnale. E' l'incidente
/// groq del 2026-07-16, documentato in `provider_error_classifier`
/// (`error_class_from_primary_cause`): un 413 per tetto token/minuto il cui
/// messaggio invitava ad alzare il piano "at https://console.groq.com/settings/
/// billing" -> la parola `billing` trovata DENTRO L'URL DELLA DOCUMENTAZIONE ->
/// groq spento 6h su tutto il sistema mentre rispondeva 200 alle chiamate normali.
///
/// Il caso che la promozione voleva coprire — un 429 mis-classificato a monte —
/// non si cura indovinando: si cura ALLA FONTE, dove `billing` e `transient` sono
/// gia' due `primary_cause` distinte del gateway.
fn classify_by_error_class(error_class: Option<&str>) -> ClasseStrutturata {
    use ClasseStrutturata as C;
    match error_class {
        Some("billing_error")
        | Some("billing_required")
        | Some("quota_exceeded")
        | Some("credit_balance_too_low")
        | Some("insufficient_quota") => {
            C::Cooldown("billing_error", CooldownKind::Long, BILLING_REASON)
        }
        Some("rate_limit") => C::Cooldown("rate_limit", CooldownKind::Short, "Rate limit raggiunto"),
        Some("overloaded")
        | Some("provider_error")
        | Some("server_error")
        | Some("service_unavailable")
        | Some("bad_gateway")
        | Some("internal_server_error") => C::Cooldown(
            "provider_error",
            CooldownKind::Short,
            "Provider sovraccarico o errore temporaneo",
        ),
        Some("auth_error") | Some("forbidden") => C::Cooldown(
            "auth_error",
            CooldownKind::Long,
            "Credenziali o accesso provider non validi",
        ),
        // Il modello non esiste, la richiesta non era valida, il contesto non ci
        // stava: il FORNITORE e' sano e ha risposto correttamente. Il gateway lo ha
        // gia' stabilito — non si chiede una seconda opinione alla prosa.
        Some("not_found") | Some("invalid_model") => C::NessunCooldown,
        Some("invalid_request")
        | Some("unprocessable")
        | Some("context_too_long")
        // 402 di ammissione: il credito c'e' (misurato con 64.811 token di
        // residuo), non ci sta questa richiesta. Un cooldown lo toglierebbe
        // dalla selezione mentre serve.
        | Some("request_exceeds_credit")
        | Some("unsupported") => C::NessunCooldown,
        _ => C::Sconosciuta,
    }
}

/// Il RIPIEGO LESSICALE: la classe dedotta dalla PROSA dell'errore
/// (`provider_error_classifier::classify_text`), usata SOLO quando il segnale
/// strutturato non c'e'. Ritorna `(classe, frase)` — e non la SEVERITA'.
///
/// Questo tipo di ritorno e' la regola, scritta nella firma perche' non possa
/// essere aggirata da un ramo aggiunto in futuro: **un ripiego lessicale non puo'
/// spegnere un fornitore in modo permanente**. La severita' la mette il chiamante,
/// ed e' sempre [`CooldownKind::Short`].
///
/// Il motivo e' la sproporzione fra la certezza del segnale e la sua conseguenza:
/// `billing_error` toglie un fornitore dalla routing matrix per SEI ORE, per tutto
/// il sistema — chat, worker e batterie insieme. Una regex su prosa che il provider
/// puo' riscrivere a ogni versione dell'API non e' un accertamento sufficiente a
/// tanto (la sola parola `billing` in un URL di documentazione bastava). Un sospetto
/// non accertato ottiene al massimo un cooldown transitorio: se il credito e'
/// davvero finito, il tentativo successivo fallisce di nuovo e questa volta il
/// gateway lo dira' in modo strutturato (402 / `insufficient_quota`), che e' la sola
/// provenienza legittima di `billing_error`.
///
/// La CLASSE resta quella vera (finisce in `last_error_class` e nei log): a essere
/// negata e' la conseguenza permanente, non la diagnosi.
fn classe_dal_ripiego_lessicale(
    c: &crate::provider_error_classifier::ClassifiedError,
) -> Option<(&'static str, &'static str)> {
    match c.stop_reason.as_str() {
        "billing_error" => Some(("billing_error", BILLING_SOSPETTO_REASON)),
        "rate_limit" => Some(("rate_limit", "Rate limit raggiunto")),
        "overloaded" | "service_unavailable" | "bad_gateway" | "provider_error" => Some((
            "provider_error",
            "Provider sovraccarico o errore temporaneo",
        )),
        "timeout" | "connection_error" => Some((
            "timeout",
            "Provider sovraccarico o errore temporaneo",
        )),
        "auth_error" | "forbidden" => Some((
            "auth_error",
            "Credenziali o accesso provider non validi (sospetto non accertato)",
        )),
        // Model-specific / client: nessun cooldown provider.
        "not_found" | "invalid_request" | "context_too_long" | "unprocessable" | "unsupported" => {
            None
        }
        _ => None,
    }
}

/// Classifica un errore del provider e la severita' del cooldown che merita.
/// Ritorna `(error_class_normalizzato, kind, human_reason)`.
///
/// Due gradini, in ordine di CERTEZZA (regola M):
///  1. [`classify_by_error_class`] sulla classe STRUTTURATA — status HTTP e codice
///     macchina, dichiarati dal gateway alla fonte. E' l'unico gradino che puo'
///     produrre un cooldown permanente ([`CooldownKind::Long`]);
///  2. [`classe_dal_ripiego_lessicale`] sulla prosa, quando il segnale non c'e'.
///     La severita' non gliela chiediamo: e' `Short` qui, in un punto solo.
///
/// Il gradino 1 non viene rivisto dal 2. Un ripiego che contraddice un accertamento
/// non e' una "difesa in profondita'": e' il ripiego che vince sul fatto.
fn classify_provider_error(
    error_class: Option<&str>,
    msg: &str,
) -> Option<(&'static str, CooldownKind, &'static str)> {
    if let Some(ec) = error_class.map(str::trim).filter(|s| !s.is_empty() && *s != "ok") {
        match classify_by_error_class(Some(ec)) {
            ClasseStrutturata::Cooldown(class, kind, reason) => return Some((class, kind, reason)),
            ClasseStrutturata::NessunCooldown => return None,
            ClasseStrutturata::Sconosciuta => {}
        }
    }
    let classified = crate::provider_error_classifier::classify_text(msg);
    // `CooldownKind::Short` compare QUI e solo qui per il ripiego: e' cosi' che il
    // divieto di spegnere un fornitore su un sospetto resta vero anche domani.
    classe_dal_ripiego_lessicale(&classified)
        .map(|(class, reason)| (class, CooldownKind::Short, reason))
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

    /// Il corpo REALE del 500 aggregato del gateway (`routes.rs`: `primary_cause`
    /// = classe del primo fallimento, `failures[].status` = status del provider).
    /// I test partono da qui e non da una struct riempita a mano: e' la stessa
    /// strada della produzione (regola O).
    fn errore_dal_gateway(primary_cause: &str, status: u16, messaggio: &str) -> anyhow::Error {
        let body = json!({
            "error": format!("tutti i provider hanno fallito -> groq ({messaggio})"),
            "code": "PROVIDER_ERROR",
            "details": {
                "primary_cause": primary_cause,
                "failures": [{ "provider": "groq", "status": status, "class": primary_cause }],
            },
        })
        .to_string();
        anyhow::Error::new(crate::nexus_gateway::GatewayHttpError::from_response(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            body,
        ))
    }

    /// La classe e il messaggio come li vede chi decide il cooldown: prodotti dal
    /// punto unico del turno d'errore, non ricostruiti dal test.
    fn classe_e_messaggio(err: &anyhow::Error) -> (Option<String>, String) {
        let turn =
            crate::orchestrator::neural_client::error_agent_turn_from_error("groq", "llama", err);
        (
            turn.get("error_class")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            turn.get("error")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        )
    }

    #[test]
    fn il_413_di_groq_non_tocca_un_fornitore_sano() {
        // 2026-07-16, incidente reale. groq rifiuta per tetto token/minuto (413) e
        // il messaggio invita ad alzare il piano "at https://console.groq.com/
        // settings/billing". Il gateway lo classifica `context_too_long`: il
        // FORNITORE e' sano, e' la singola richiesta a non entrarci.
        //
        // Prima: la classe strutturata cadeva nel ripiego, la regex trovava la
        // parola `billing` DENTRO L'URL DELLA DOCUMENTAZIONE, e groq spariva dalla
        // routing matrix per sei ore mentre rispondeva 200 a tutto il resto.
        let err = errore_dal_gateway(
            "context_too_long",
            413,
            "Request too large for model. Limit 8000, Requested 20083. \
             Need more tokens? Upgrade at https://console.groq.com/settings/billing",
        );
        let (classe, messaggio) = classe_e_messaggio(&err);
        assert_eq!(classe.as_deref(), Some("context_too_long"));
        assert!(
            messaggio.contains("billing"),
            "il messaggio deve contenere la parola che traeva in inganno, \
             altrimenti il test non misura il difetto"
        );
        assert_eq!(
            classify_provider_error(classe.as_deref(), &messaggio),
            None,
            "un errore model-specific gia' classificato dal gateway non mette in \
             cooldown il fornitore, e la sua prosa non viene riletta"
        );
    }

    #[test]
    fn il_credito_esaurito_accertato_dal_gateway_spegne_il_fornitore() {
        // Il rovescio del test sopra: quando il credito e' finito DAVVERO, il
        // gateway lo dichiara (402 / codice `insufficient_quota` -> causa
        // `billing`) e il cooldown lungo si applica. Il fix non ha disarmato
        // l'unica provenienza legittima di `billing_error`.
        let err = errore_dal_gateway("billing", 402, "insufficient credit balance");
        let (classe, messaggio) = classe_e_messaggio(&err);
        assert_eq!(classe.as_deref(), Some("billing_error"));
        let (class, kind, _) = classify_provider_error(classe.as_deref(), &messaggio)
            .expect("il credito esaurito accertato deve produrre un cooldown");
        assert_eq!(class, "billing_error");
        assert_eq!(kind, CooldownKind::Long);
    }

    #[test]
    fn quota_dal_solo_testo_non_spegne_il_fornitore_per_ore() {
        // Nessuna classe strutturata: resta solo la prosa. La diagnosi puo' anche
        // essere giusta (`billing_error`), ma un ripiego lessicale non e' un
        // accertamento e non puo' togliere un fornitore a tutto il sistema per sei
        // ore: al massimo un cooldown transitorio. Se il credito e' finito davvero,
        // il tentativo dopo fallisce ancora e il gateway lo dira' in modo
        // strutturato.
        let msg = "Error code: 429 - {'error': {'message': 'You exceeded your current quota, please check your plan and billing details.', 'type': 'insufficient_quota'}}";
        let (class, kind, _) =
            classify_provider_error(None, msg).expect("429 quota deve essere classificato");
        assert_eq!(class, "billing_error", "la diagnosi resta quella vera");
        assert_eq!(
            kind,
            CooldownKind::Short,
            "a essere negata e' la conseguenza permanente, non la diagnosi"
        );
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
    fn la_prosa_non_promuove_una_classe_strutturata_a_billing() {
        // Questo test asseriva l'OPPOSTO, e cosi' proteggeva il difetto: con
        // `error_class=rate_limit` gia' dichiarato, un messaggio "billing" faceva
        // promuovere a cooldown 6h. Era il ripiego che vinceva sull'accertamento.
        //
        // Il caso che la promozione voleva coprire — un 429 mis-classificato a
        // monte — si cura ALLA FONTE: `billing` e `transient` sono due
        // `primary_cause` distinte del gateway, e chi le confonde va corretto li'.
        // Indovinare a valle non recupera il segnale: lo sovrascrive, e con la
        // stessa disinvoltura sovrascrive quelli giusti.
        let msg = "You exceeded your current quota, please check your plan and billing details.";
        let (class, kind, _) =
            classify_provider_error(Some("rate_limit"), msg).expect("deve essere classificato");
        assert_eq!(class, "rate_limit", "vince la classe strutturata");
        assert_eq!(kind, CooldownKind::Short);
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
    fn billing_hard_limit_dal_solo_testo_resta_transitorio() {
        // Marker OpenAI `billing_hard_limit_reached` visto nella sola prosa: la
        // classe e' billing, la conseguenza no (vedi
        // `quota_dal_solo_testo_non_spegne_il_fornitore_per_ore`).
        let msg = "Error code: 429 - billing_hard_limit_reached";
        let (class, kind, _) =
            classify_provider_error(None, msg).expect("deve essere classificato");
        assert_eq!(class, "billing_error");
        assert_eq!(kind, CooldownKind::Short);
    }

    #[test]
    fn nessun_ripiego_lessicale_puo_produrre_un_cooldown_permanente() {
        // La regola, verificata sull'INTERO vocabolario del ripiego invece che su
        // un caso per volta: nessun testo, quale che sia, puo' spegnere un
        // fornitore in modo permanente. Un ramo aggiunto domani con
        // `CooldownKind::Long` fa rosseggiare qui — che e' il punto: la firma di
        // `classe_dal_ripiego_lessicale` non espone la severita', e questo test
        // difende quella scelta dal call site.
        let prose = [
            "credit balance is too low",
            "insufficient_quota",
            "upgrade or purchase credits",
            "payment required",
            "account is not active",
            "invalid api key",
            "unauthorized",
            "rate limit reached",
            "503 service unavailable",
            "connection timed out",
        ];
        for msg in prose {
            if let Some((class, kind, _)) = classify_provider_error(None, msg) {
                assert_eq!(
                    kind,
                    CooldownKind::Short,
                    "il ripiego lessicale ha prodotto un cooldown permanente su \
                     '{msg}' (classe {class}): solo un segnale strutturato puo'"
                );
            }
        }
    }

    #[test]
    fn una_classe_strutturata_nota_non_viene_riletta_dalla_prosa() {
        // I due `None` che prima erano lo stesso valore. `not_found` e
        // `invalid_request` sono classi NOTE che dicono "il fornitore e' sano":
        // devono fermare la catena, non farla scivolare al ripiego. Il messaggio
        // porta di proposito la parola che il ripiego cercherebbe.
        for classe in ["not_found", "invalid_request", "context_too_long", "unsupported"] {
            assert_eq!(
                classify_provider_error(Some(classe), "see your billing page for credits"),
                None,
                "la classe '{classe}' dice che il fornitore non c'entra: nessun \
                 cooldown e nessuna seconda opinione dalla prosa"
            );
        }
        // Una classe FUORI vocabolario e' un'altra cosa: li' la prosa e' tutto
        // quello che resta, e si guarda.
        assert!(classify_provider_error(Some("boh_mai_visto"), "rate limit reached").is_some());
    }
}

#[cfg(test)]
mod tests_cooldown_empty_completion {
    use super::*;

    /// Una risposta VUOTA e' un difetto del MODELLO, non del fornitore: il
    /// provider ha risposto 200 e ha fatto il suo lavoro. Metterlo in cooldown
    /// lo esclude per 60 secondi mentre e' sano.
    ///
    /// MISURATO il 07/08/2026 su gestione-corsi: 22 failover su 39 escalation,
    /// tutti con motivo `cooldown`, su openrouter/deepseek/mistral/google — che
    /// nel DB non avevano ALCUN cooldown di billing. L'utente lo ha descritto
    /// cosi': «molti cambi di provider per cooldown, e poco dopo rifunzionavano».
    /// Poco dopo = i 60 secondi del cooldown breve.
    #[test]
    fn una_risposta_vuota_non_mette_il_fornitore_in_cooldown() {
        // Il gateway riporta la degenerazione come 500 (non ha un altro codice
        // HTTP per dire «200 ma vuoto»), e la classe strutturata la dichiara.
        let esito = classify_provider_error(
            Some("empty_completion"),
            "Nexus Gateway 500 Internal Server Error: {\"error\":\"PROVIDER_ERROR\"}",
        );
        assert!(
            esito.is_none(),
            "empty_completion e' del modello: nessun cooldown al fornitore, invece: {esito:?}"
        );
    }

    /// Il contrappunto: un 5xx VERO resta un cooldown breve. Senza questo, il
    /// test sopra sarebbe compatibile con «non mettere mai nessuno in cooldown».
    #[test]
    fn un_errore_vero_del_fornitore_resta_in_cooldown() {
        let esito = classify_provider_error(Some("service_unavailable"), "503");
        assert!(matches!(esito, Some((_, CooldownKind::Short, _))));
        let billing = classify_provider_error(Some("billing_error"), "402");
        assert!(matches!(billing, Some((_, CooldownKind::Long, _))));
    }
}
