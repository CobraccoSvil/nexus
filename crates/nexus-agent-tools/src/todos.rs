//! PR-1 Plan/Act/Verify: handler MCP `nexus_todo_write`.
//!
//! Tool riservato al planner_node (e in casi limitati all'executor per
//! aggiornare lo status di un singolo todo). Persiste su `nexus_agent_plans`
//! + `nexus_agent_todos` con isolation per project_id.
//!
//! Schema input:
//! {
//!   "action": "create" | "check" | "add" | "update",
//!   "run_id": "uuid",                  // obbligatorio: run_id dell'agent_run corrente
//!   "todos": [
//!     {
//!       "id": "uuid",                  // opzionale per "create"/"add", obbligatorio per "check"/"update"
//!       "seq": int,                    // opzionale, derivato per "add"
//!       "content": "string",
//!       "status": "pending"|"in_progress"|"completed"|"blocked"|"skipped",
//!       "priority": "high"|"normal"|"low",
//!       "acceptance_criteria": [...],  // opzionale, array di check spec JSON
//!       "node_key": "string",          // opzionale, chiave logica DAG del todo
//!       "dep_keys": ["string"],        // opzionale, node_key delle dipendenze
//!       "write_scope": ["string"]      // opzionale (mig project 0006), aree file
//!                                      //   dichiarate: alimenta l'isolamento parallelo
//!     }
//!   ],
//!   "planner_model": "string"          // opzionale, per action=create (default 'unknown')
//! }
//!
//! Output: JSON con `{ok, action, affected, plan_id, todo_ids[]}`; l'esito NON
//! viaggia piu' in testa al testo ma nel campo di
//! [`nexus_types::tool_outcome::RispostaTool`] (regola Q).
//!
//! # Perche' l'input NON passa dal contratto d'ingresso
//!
//! `NexusTodoWriteInput` esiste (`tool_inputs.rs`) ed e' generato dal catalogo
//! di `tool_schema.rs`, ma quel catalogo non e' l'unico: il planner ne dichiara
//! uno PROPRIO (`nexus_agent_graph::nodes::planner::tool_catalog`) che promette
//! al modello anche `rationale`, `constraints`, `alternatives`, `node_key`,
//! `dep_keys`, `write_scope` — e `build_tool_input` vi aggiunge `user_intent` e
//! `behavior_mode`, mentre `materialize_delegation_block` emette SEMPRE
//! `node_key`/`dep_keys`/`write_scope`. Il contratto e' `deny_unknown_fields`:
//! adottarlo qui farebbe rifiutare proprio le chiamate del planner, cioe' il
//! percorso principale con cui un piano nasce. La divergenza si chiude
//! dichiarando quei campi nel contratto E nel catalogo (due file che non
//! appartengono a questo intervento), non allargando il parsing di nascosto.

use serde_json::{json, Value};
use sqlx::Row;
use std::collections::HashMap;
use uuid::Uuid;

use nexus_types::tool_outcome::{NaturaFallimento, RispostaTool};

use super::ToolContextCore;

/// Stati ammessi (mirror del CHECK constraint in mig 0148).
const VALID_STATUSES: &[&str] = &["pending", "in_progress", "completed", "blocked", "skipped"];
const VALID_PRIORITIES: &[&str] = &["high", "normal", "low"];
/// Le azioni accettate. Elenco unico: il `match` finale e il messaggio d'errore
/// che le nomina all'agente leggono di qui, e non possono divergere.
const VALID_ACTIONS: &[&str] = &["create", "check", "add", "update"];

/// Un input che l'agente puo' correggere da solo.
///
/// La natura sta nel campo (regola Q) e il messaggio porta il COME: dire
/// «rimediabile» senza nominare il parametro e i valori ammessi sarebbe una
/// promessa non mantenuta.
fn rifiuta(messaggio: impl std::fmt::Display) -> RispostaTool {
    crate::errore_tool(messaggio, NaturaFallimento::Rimediabile)
}

/// Il DB non ha fatto cio' che gli era stato chiesto.
///
/// `DelSistema` e non `Transitorio`: da un `sqlx::Error` non si distingue una
/// connessione caduta — dove ritentare identico avrebbe senso — da un CHECK
/// violato, dove la stessa INSERT rifallira'. In dubbio la direttiva giusta e'
/// «cambia strada» invece di far ripetere una chiamata che rifallira'.
fn errore_db(contesto: &str, e: sqlx::Error) -> RispostaTool {
    crate::errore_tool(format!("{contesto}: {e}"), NaturaFallimento::DelSistema)
}

pub async fn tool_nexus_todo_write(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    // 1. Parse e validazione input.
    let action = match input.get("action").and_then(Value::as_str) {
        Some(a) if VALID_ACTIONS.contains(&a) => a,
        Some(a) => {
            return rifiuta(format!(
                "action '{a}' non valida: usa uno fra {}",
                VALID_ACTIONS.join("|")
            ))
        }
        None => {
            return rifiuta(format!(
                "parametro 'action' obbligatorio: uno fra {}",
                VALID_ACTIONS.join("|")
            ))
        }
    };

    let run_id = match input
        .get("run_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(r) => r,
        None => {
            return rifiuta(
                "parametro 'run_id' obbligatorio: l'uuid dell'agent_run corrente, \
                 quello dichiarato nel prompt (RUN_ID).",
            )
        }
    };

    let todos_in = match input.get("todos").and_then(Value::as_array) {
        Some(t) if !t.is_empty() => t,
        Some(_) => return rifiuta("parametro 'todos' vuoto: passa almeno un todo"),
        None => return rifiuta("parametro 'todos' obbligatorio: array di todo"),
    };

    let project_id = ctx.project_id;

    // 2. Isolation multi-tenant: il run deve essere di QUESTO progetto.
    if let Err(risposta) = verifica_run_del_progetto(ctx, run_id, project_id).await {
        return risposta;
    }

    // 3. Dispatch per action. Il ramo finale e' `update` per costruzione:
    // `action` e' gia' stata validata contro VALID_ACTIONS.
    match action {
        "create" => create_plan(ctx, run_id, project_id, todos_in, input).await,
        "check" => update_status(ctx, run_id, todos_in, true).await,
        "add" => add_todos(ctx, run_id, project_id, todos_in).await,
        _ => update_status(ctx, run_id, todos_in, false).await,
    }
}

/// Il run esiste ED e' del progetto corrente.
///
/// Il ramo del DB che non risponde e' SEPARATO da quello del run assente: prima
/// la lettura era `.ok().flatten()`, quindi un errore di query diventava un
/// `None` e l'agente riceveva «run_id non trovato» — cioe' veniva mandato a
/// correggere un parametro che era giusto, mentre il problema era altrove.
async fn verifica_run_del_progetto(
    ctx: &ToolContextCore,
    run_id: Uuid,
    project_id: Uuid,
) -> Result<(), RispostaTool> {
    let riga: Option<(Uuid,)> =
        sqlx::query_as("SELECT project_id FROM agent_runs WHERE id = $1 LIMIT 1")
            .bind(run_id)
            .fetch_optional(&*ctx.run_db)
            .await
            .map_err(|e| errore_db("lettura di agent_runs fallita", e))?;
    match riga {
        Some((p,)) if p == project_id => Ok(()),
        Some(_) => Err(rifiuta(format!(
            "run_id {run_id} appartiene a un altro progetto: usa il RUN_ID del run corrente."
        ))),
        None => Err(rifiuta(format!(
            "run_id {run_id} non trovato in agent_runs: usa il RUN_ID del run corrente."
        ))),
    }
}

/// I campi del PIANO (non dei suoi todo) letti dall'input.
struct CampiPiano {
    planner_model: String,
    thread_id: String,
    acceptance: Value,
    rationale: String,
    constraints: Value,
    alternatives: Value,
    user_intent: Option<String>,
    behavior_mode: Option<String>,
}

/// Cluster 1 (mig 0206) + intent/behavior_mode (mig 0328). Tutti opzionali:
/// colonne nullable con default, se assenti dal payload restano vuote.
fn campi_del_piano(ctx: &ToolContextCore, run_id: Uuid, input: &Value) -> CampiPiano {
    let testo = |chiave: &str| input.get(chiave).and_then(Value::as_str);
    let oggetto = |chiave: &str| input.get(chiave).cloned().unwrap_or_else(|| json!([]));
    CampiPiano {
        planner_model: testo("planner_model").unwrap_or("unknown").to_string(),
        // thread_id (sessione) per indicizzazione; senza sessione vale il run.
        thread_id: ctx
            .session_id
            .map(|u| u.to_string())
            .unwrap_or_else(|| run_id.to_string()),
        acceptance: oggetto("plan_acceptance_criteria"),
        rationale: testo("rationale").unwrap_or("").to_string(),
        constraints: oggetto("constraints"),
        alternatives: oggetto("alternatives"),
        // Nullable: i piani senza questi campi restano riusabili come prima
        // (invalidazione intent-aware del riuso, mig 0328).
        user_intent: testo("user_intent").map(str::to_string),
        behavior_mode: testo("behavior_mode").map(str::to_string),
    }
}

async fn create_plan(
    ctx: &ToolContextCore,
    run_id: Uuid,
    project_id: Uuid,
    todos_in: &[Value],
    input: &Value,
) -> RispostaTool {
    let campi = campi_del_piano(ctx, run_id, input);

    let mut tx = match ctx.run_db.begin().await {
        Ok(t) => t,
        Err(e) => return errore_db("apertura della transazione fallita", e),
    };

    if let Err(e) = upsert_piano(&mut tx, run_id, project_id, &campi).await {
        return errore_db("INSERT nexus_agent_plans fallita", e);
    }

    // create = reset completo: i todo precedenti del run se ne vanno.
    if let Err(e) = sqlx::query("DELETE FROM nexus_agent_todos WHERE run_id = $1")
        .bind(run_id)
        .execute(&mut *tx)
        .await
    {
        return errore_db("DELETE dei todo precedenti fallita", e);
    }

    let inseriti = match inserisci_todi(&mut tx, run_id, project_id, todos_in).await {
        Ok(i) => i,
        Err(risposta) => return risposta,
    };

    // Comp.3a: risolvi dep_keys -> depends_on (UUID[]) con cycle detection.
    let dipendenze =
        match resolve_and_persist_deps(&mut tx, &inseriti.key_to_id, &inseriti.deps).await {
            Ok(d) => d,
            Err(e) => return errore_db("risoluzione delle dipendenze del DAG fallita", e),
        };

    if let Err(e) = tx.commit().await {
        return errore_db("commit della transazione fallita", e);
    }

    persisti_meta_piano(ctx, run_id).await;

    RispostaTool::riuscito(esito_create(run_id, &inseriti.ids, dipendenze).to_string())
}

/// Upsert del plan (PRIMARY KEY = run_id, quindi ON CONFLICT su run_id).
async fn upsert_piano(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
    project_id: Uuid,
    campi: &CampiPiano,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO nexus_agent_plans
             (run_id, project_id, thread_id, acceptance_criteria, planner_model,
              rationale, constraints, alternatives, user_intent, behavior_mode)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
           ON CONFLICT (run_id) DO UPDATE SET
             acceptance_criteria = EXCLUDED.acceptance_criteria,
             planner_model = EXCLUDED.planner_model,
             rationale = EXCLUDED.rationale,
             constraints = EXCLUDED.constraints,
             alternatives = EXCLUDED.alternatives,
             user_intent = EXCLUDED.user_intent,
             behavior_mode = EXCLUDED.behavior_mode,
             plan_revisions = nexus_agent_plans.plan_revisions"#,
    )
    .bind(run_id)
    .bind(project_id)
    .bind(&campi.thread_id)
    .bind(&campi.acceptance)
    .bind(&campi.planner_model)
    .bind(&campi.rationale)
    .bind(&campi.constraints)
    .bind(&campi.alternatives)
    .bind(campi.user_intent.as_deref())
    .bind(campi.behavior_mode.as_deref())
    .execute(&mut **tx)
    .await
    .map(|_| ())
}

/// Cio' che resta dopo l'inserimento dei todo di un `create`.
///
/// `key_to_id` e `deps` servono al secondo passaggio: il planner non conosce
/// gli UUID generati e ragiona su chiavi logiche.
struct TodiInseriti {
    ids: Vec<String>,
    key_to_id: HashMap<String, Uuid>,
    deps: Vec<(Uuid, Vec<String>)>,
}

async fn inserisci_todi(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
    project_id: Uuid,
    todos_in: &[Value],
) -> Result<TodiInseriti, RispostaTool> {
    let mut out = TodiInseriti {
        ids: Vec::with_capacity(todos_in.len()),
        key_to_id: HashMap::new(),
        deps: Vec::with_capacity(todos_in.len()),
    };
    for (idx, t) in todos_in.iter().enumerate() {
        let campi = campi_del_todo(idx, t)?;
        let riga = sqlx::query(
            r#"INSERT INTO nexus_agent_todos
                 (run_id, project_id, seq, content, status, priority,
                  acceptance_criteria, node_key, dep_keys, write_scope)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
               RETURNING id"#,
        )
        .bind(run_id)
        .bind(project_id)
        .bind(campi.seq)
        .bind(&campi.content)
        .bind(&campi.status)
        .bind(&campi.priority)
        .bind(&campi.acceptance)
        .bind(campi.node_key.as_deref())
        .bind(&campi.dep_keys)
        .bind(&campi.write_scope)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| errore_db(&format!("INSERT del todo[{idx}] fallita"), e))?;
        let id: Uuid = riga.get("id");
        if let Some(k) = campi.node_key {
            out.key_to_id.insert(k, id);
        }
        out.deps.push((id, campi.dep_keys));
        out.ids.push(id.to_string());
    }
    Ok(out)
}

/// Il corpo di risposta di `create`.
///
/// Il ciclo nel DAG e' DICHIARATO in un campo invece di restare un `warn!` nei
/// log: il piano c'e' (quindi non e' un fallimento), ma le dipendenze che
/// l'agente aveva chiesto sono state buttate, e senza dirlo `dag_deps_resolved:
/// 0` era indistinguibile da «non ne avevi dichiarate».
fn esito_create(run_id: Uuid, ids: &[String], dipendenze: EsitoDipendenze) -> Value {
    let mut out = json!({
        "ok": true,
        "action": "create",
        "dag_deps_resolved": dipendenze.todo_con_dipendenze(),
        "affected": ids.len(),
        "plan_id": run_id.to_string(),
        "todo_ids": ids,
    });
    if matches!(dipendenze, EsitoDipendenze::CicloRilevato) {
        out["dag_cycle_detected"] = json!(true);
        out["hint"] = json!(
            "I dep_keys dichiarati formano un ciclo: sono stati IGNORATI e i todo \
             girano in ordine di seq. Per ottenere il DAG richiesto, richiama \
             action='create' con dipendenze acicliche."
        );
    }
    out
}

/// I campi di UN todo, validati.
#[derive(Debug)]
struct CampiTodo {
    seq: i32,
    content: String,
    status: String,
    priority: String,
    acceptance: Value,
    node_key: Option<String>,
    dep_keys: Vec<String>,
    write_scope: Vec<String>,
}

/// Legge e valida un todo. PURA: nessun I/O, quindi provabile senza DB.
///
/// I default (`pending`, `normal`, `seq` = posizione) sono quelli che il
/// catalogo dichiara. Un valore fuori vocabolario NON scende fino al CHECK del
/// DB: li' il messaggio non nomina piu' il campo da correggere.
fn campi_del_todo(idx: usize, t: &Value) -> Result<CampiTodo, RispostaTool> {
    let content = match t.get("content").and_then(Value::as_str) {
        Some(c) if !c.trim().is_empty() => c.to_string(),
        _ => {
            return Err(rifiuta(format!(
                "todo[{idx}]: 'content' obbligatorio e non vuoto"
            )))
        }
    };
    let status = t.get("status").and_then(Value::as_str).unwrap_or("pending");
    if !VALID_STATUSES.contains(&status) {
        return Err(rifiuta(format!(
            "todo[{idx}]: status '{status}' non valido: usa uno fra {}",
            VALID_STATUSES.join("|")
        )));
    }
    let priority = t
        .get("priority")
        .and_then(Value::as_str)
        .unwrap_or("normal");
    if !VALID_PRIORITIES.contains(&priority) {
        return Err(rifiuta(format!(
            "todo[{idx}]: priority '{priority}' non valida: usa uno fra {}",
            VALID_PRIORITIES.join("|")
        )));
    }
    let seq = t
        .get("seq")
        .and_then(Value::as_i64)
        .unwrap_or((idx + 1) as i64) as i32;
    Ok(CampiTodo {
        seq,
        content,
        status: status.to_string(),
        priority: priority.to_string(),
        acceptance: t
            .get("acceptance_criteria")
            .cloned()
            .unwrap_or_else(|| json!([])),
        // Comp.3a: chiave logica del nodo e chiavi delle dipendenze.
        node_key: t
            .get("node_key")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        dep_keys: elenco_di_stringhe(t, "dep_keys"),
        // PR5 (mig project 0006): aree file dichiarate dal todo. Segnale
        // strutturato (regola M) letto a valle da `dispatch_wave` e valutato dal
        // punto unico `subtasks_are_disjoint`.
        write_scope: elenco_di_stringhe(t, "write_scope"),
    })
}

/// Le stringhe di un array OPZIONALE: assente o malformato vale vuoto, che e'
/// il default delle colonne (NOT NULL DEFAULT '{}'). Non inghiotte un errore —
/// qui non ce n'e' uno: l'assenza del campo e' un caso legittimo.
fn elenco_di_stringhe(t: &Value, campo: &str) -> Vec<String> {
    t.get(campo)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

async fn add_todos(
    ctx: &ToolContextCore,
    run_id: Uuid,
    project_id: Uuid,
    todos_in: &[Value],
) -> RispostaTool {
    let piano_esiste: bool = match sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM nexus_agent_plans WHERE run_id = $1)",
    )
    .bind(run_id)
    .fetch_one(&*ctx.run_db)
    .await
    {
        Ok(v) => v,
        // Prima era `.unwrap_or(false)`: un DB muto diventava «piano
        // inesistente», e l'agente veniva mandato a rifare il piano da capo.
        Err(e) => return errore_db("lettura di nexus_agent_plans fallita", e),
    };
    if !piano_esiste {
        return rifiuta(format!(
            "nessun piano per run_id={run_id}: chiama prima action='create'"
        ));
    }

    // Prima era `.ok()` + `unwrap_or(0)`: un errore di lettura riportava la
    // numerazione a zero, cioe' produceva seq duplicati in silenzio.
    let base: i32 = match sqlx::query_scalar(
        "SELECT COALESCE(MAX(seq), 0) FROM nexus_agent_todos WHERE run_id = $1",
    )
    .bind(run_id)
    .fetch_one(&*ctx.run_db)
    .await
    {
        Ok(v) => v,
        Err(e) => return errore_db("lettura del seq massimo fallita", e),
    };

    let mut tx = match ctx.run_db.begin().await {
        Ok(t) => t,
        Err(e) => return errore_db("apertura della transazione fallita", e),
    };
    let mut inserted_ids: Vec<String> = Vec::with_capacity(todos_in.len());
    for (idx, t) in todos_in.iter().enumerate() {
        let campi = match campi_del_todo(idx, t) {
            Ok(c) => c,
            Err(risposta) => return risposta,
        };
        match appendi_todo(&mut tx, run_id, project_id, base + 1 + idx as i32, &campi).await {
            Ok(id) => inserted_ids.push(id.to_string()),
            Err(e) => return errore_db(&format!("INSERT del todo[{idx}] fallita"), e),
        }
    }
    if let Err(e) = tx.commit().await {
        return errore_db("commit della transazione fallita", e);
    }
    persisti_meta_piano(ctx, run_id).await;

    let esito = json!({
        "ok": true,
        "action": "add",
        "affected": inserted_ids.len(),
        "todo_ids": inserted_ids,
    });
    RispostaTool::riuscito(esito.to_string())
}

/// Appende un todo a un piano gia' esistente.
///
/// `seq` lo decide il chiamante (coda della numerazione), non il todo: e' cio'
/// che il catalogo dichiara con «auto per create/add». Le colonne del DAG
/// (`node_key`, `dep_keys`, `write_scope`) NON sono qui: risolvere le
/// dipendenze contro i todo gia' in tabella e' un'altra domanda da quella che
/// `resolve_and_persist_deps` risponde su un piano appena creato.
async fn appendi_todo(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
    project_id: Uuid,
    seq: i32,
    campi: &CampiTodo,
) -> Result<Uuid, sqlx::Error> {
    let riga = sqlx::query(
        r#"INSERT INTO nexus_agent_todos
             (run_id, project_id, seq, content, status, priority, acceptance_criteria)
           VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id"#,
    )
    .bind(run_id)
    .bind(project_id)
    .bind(seq)
    .bind(&campi.content)
    .bind(&campi.status)
    .bind(&campi.priority)
    .bind(&campi.acceptance)
    .fetch_one(&mut **tx)
    .await?;
    Ok(riga.get("id"))
}

/// Lo status che il todo deve assumere.
///
/// `check` non guarda l'input: l'azione stessa significa «completato». In
/// `update` lo status manca o e' fuori vocabolario, e i due casi restano
/// distinti — prima erano lo stesso messaggio, che non diceva quale dei due.
fn stato_richiesto(idx: usize, t: &Value, check_mode: bool) -> Result<String, RispostaTool> {
    if check_mode {
        return Ok("completed".to_string());
    }
    match t.get("status").and_then(Value::as_str) {
        Some(s) if VALID_STATUSES.contains(&s) => Ok(s.to_string()),
        Some(s) => Err(rifiuta(format!(
            "todo[{idx}]: status '{s}' non valido: usa uno fra {}",
            VALID_STATUSES.join("|")
        ))),
        None => Err(rifiuta(format!(
            "todo[{idx}]: 'status' obbligatorio per action='update': uno fra {}",
            VALID_STATUSES.join("|")
        ))),
    }
}

/// Aggiorna lo status (e altri campi) di todos esistenti.
/// `check_mode=true` significa azione "check" (target esplicito = mark completed).
async fn update_status(
    ctx: &ToolContextCore,
    run_id: Uuid,
    todos_in: &[Value],
    check_mode: bool,
) -> RispostaTool {
    let mut tx = match ctx.run_db.begin().await {
        Ok(t) => t,
        Err(e) => return errore_db("apertura della transazione fallita", e),
    };

    let mut affected = 0_usize;
    // M15.1: traccia (id, status) aggiornati per emettere TodoUpdated dopo il commit.
    let mut updated: Vec<(Uuid, String)> = Vec::new();
    for (idx, t) in todos_in.iter().enumerate() {
        let id = match t
            .get("id")
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            Some(u) => u,
            None => {
                return rifiuta(format!(
                    "todo[{idx}]: 'id' uuid obbligatorio per action='check'/'update'"
                ))
            }
        };
        let new_status = match stato_richiesto(idx, t, check_mode) {
            Ok(s) => s,
            Err(risposta) => return risposta,
        };

        let res = sqlx::query(
            r#"UPDATE nexus_agent_todos
               SET status = $1, updated_at = NOW()
               WHERE id = $2 AND run_id = $3"#,
        )
        .bind(&new_status)
        .bind(id)
        .bind(run_id)
        .execute(&mut *tx)
        .await;
        match res {
            Ok(r) if r.rows_affected() == 0 => {
                return rifiuta(format!(
                    "todo {id} non appartiene a run_id={run_id}: rileggi il piano e usa \
                     un id di quel run"
                ))
            }
            Ok(r) => {
                affected += r.rows_affected() as usize;
                updated.push((id, new_status));
            }
            Err(e) => return errore_db(&format!("UPDATE del todo[{idx}] fallita"), e),
        }
    }

    if let Err(e) = tx.commit().await {
        return errore_db("commit della transazione fallita", e);
    }

    emetti_progresso_live(ctx, run_id, &updated).await;
    persisti_meta_piano(ctx, run_id).await;

    let azione = if check_mode { "check" } else { "update" };
    let esito = json!({ "ok": true, "action": azione, "affected": affected });
    RispostaTool::riuscito(esito.to_string())
}

/// M15.1 — Progresso todo live: emette TodoUpdated per ogni todo aggiornato,
/// cosi' la checklist in chat (agent-meta-step-card.tsx) si spunta in tempo
/// reale durante l'esecuzione (gated `agent.todos.live_events`).
async fn emetti_progresso_live(ctx: &ToolContextCore, run_id: Uuid, updated: &[(Uuid, String)]) {
    let live_events = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.todos.live_events' LIMIT 1",
    )
    .fetch_optional(&*ctx.run_db)
    .await
    .ok()
    .flatten()
    .map(|s| {
        !matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "false" | "0" | "off" | "no"
        )
    })
    .unwrap_or(true);
    if !live_events {
        return;
    }
    for (id, status) in updated {
        nexus_events::dispatcher::emit_global(
            ctx.project_id,
            nexus_events::event::ProjectEvent::TodoUpdated {
                run_id: run_id.to_string(),
                todo_id: id.to_string(),
                seq: None,
                status: status.clone(),
            },
        );
    }
    // M15 — PlanUpdated: avanzamento aggregato del piano (totale/completati)
    // cosi' la UI aggiorna il contatore senza ricaricare la checklist.
    let conteggi = sqlx::query_as::<_, (i64, i64)>(
        "SELECT COUNT(*), COUNT(*) FILTER (WHERE status = 'completed') \
         FROM nexus_agent_todos WHERE run_id = $1",
    )
    .bind(run_id)
    .fetch_one(&*ctx.run_db)
    .await;
    if let Ok((total, completed)) = conteggi {
        nexus_events::dispatcher::emit_global(
            ctx.project_id,
            nexus_events::event::ProjectEvent::PlanUpdated {
                run_id: run_id.to_string(),
                total: total as i32,
                completed: completed as i32,
            },
        );
    }
}

/// Scrive (o riscrive) il meta_step `plan` del run con lo stato ATTUALE dei
/// todo, cosi' il piano resta visibile in chat dopo un refresh.
///
/// Perche' serve QUI e non solo nel nodo planner: il piano nasce da DUE
/// percorsi — il `PlannerNode`, che il suo meta_step lo emette gia'
/// (`make_plan_meta` + `emit_phase_meta`), e questo tool, chiamato
/// dall'executor, che finora emetteva solo eventi LIVE (`TodoUpdated`,
/// `PlanUpdated`). Gli eventi live non sopravvivono al reload: la card del
/// piano si vedeva mentre l'agente lavorava e spariva al refresh, mentre
/// Consiglio e multi-provider — che un meta_step lo persistono — restavano.
/// MISURATO il 06/08/2026 su agenda-medica: 1 piano e 16 todo nel DB, e ZERO
/// meta_step di kind `plan`.
///
/// La disciplina "una riga per run" viveva QUI e l'altro produttore non la
/// conosceva: si delega al punto unico [`crate::meta_piano`], dove l'invariante
/// e' dello schema (indice unico parziale) e vale per entrambi.
async fn persisti_meta_piano(ctx: &ToolContextCore, run_id: Uuid) {
    crate::meta_piano::scrivi_dai_todo(&ctx.run_db, run_id).await;
}

/// Che fine hanno fatto le dipendenze dichiarate dal piano.
///
/// Due casi e non un numero: `0` scritte e «tutte buttate perche' il grafo
/// aveva un ciclo» sono fatti diversi, e il secondo l'agente puo' correggerlo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EsitoDipendenze {
    /// Dipendenze scritte su N todo (N puo' essere 0: nessuna dichiarata).
    Scritte(usize),
    /// Ciclo nel grafo: nessuna dipendenza scritta, esecuzione lineare per seq.
    CicloRilevato,
}

impl EsitoDipendenze {
    /// Quanti todo hanno almeno una dipendenza persistita.
    fn todo_con_dipendenze(self) -> usize {
        match self {
            Self::Scritte(n) => n,
            Self::CicloRilevato => 0,
        }
    }
}

/// Comp.3a: risolve i dep_keys logici in depends_on (UUID[]) e li scrive sui
/// todo, dentro la transazione corrente. Scarta i dep_keys che non
/// corrispondono a nessun node_key del piano (riferimenti fantasma) e i
/// self-link. Se il grafo risultante contiene un ciclo, azzera tutte le
/// dipendenze (fallback all'esecuzione lineare per seq) invece di rischiare un
/// deadlock dello scheduler.
async fn resolve_and_persist_deps(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    key_to_id: &HashMap<String, Uuid>,
    todo_deps: &[(Uuid, Vec<String>)],
) -> Result<EsitoDipendenze, sqlx::Error> {
    let mut resolved: Vec<(Uuid, Vec<Uuid>)> = Vec::with_capacity(todo_deps.len());
    for (id, dep_keys) in todo_deps {
        let mut deps: Vec<Uuid> = Vec::new();
        for k in dep_keys {
            if let Some(dep_id) = key_to_id.get(k) {
                if dep_id != id && !deps.contains(dep_id) {
                    deps.push(*dep_id);
                }
            }
        }
        resolved.push((*id, deps));
    }

    if detect_cycle(&resolved) {
        tracing::warn!(
            "nexus_todo_write: ciclo rilevato nel DAG dei todo, fallback lineare (depends_on non applicati)"
        );
        return Ok(EsitoDipendenze::CicloRilevato);
    }

    let mut updated = 0usize;
    for (id, deps) in &resolved {
        if deps.is_empty() {
            continue;
        }
        sqlx::query("UPDATE nexus_agent_todos SET depends_on = $1 WHERE id = $2")
            .bind(deps.as_slice())
            .bind(id)
            .execute(&mut **tx)
            .await?;
        updated += 1;
    }
    Ok(EsitoDipendenze::Scritte(updated))
}

/// Kahn topological sort: ritorna true se il grafo (id -> depends_on) contiene
/// un ciclo. depends_on[n] = nodi che devono precedere n.
fn detect_cycle(nodes: &[(Uuid, Vec<Uuid>)]) -> bool {
    let ids: std::collections::HashSet<Uuid> = nodes.iter().map(|(id, _)| *id).collect();
    let mut in_degree: HashMap<Uuid, usize> = HashMap::new();
    let mut adj: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for (id, deps) in nodes {
        in_degree.entry(*id).or_insert(0);
        for d in deps {
            if ids.contains(d) {
                *in_degree.entry(*id).or_insert(0) += 1;
                adj.entry(*d).or_default().push(*id);
            }
        }
    }
    let mut queue: Vec<Uuid> = in_degree
        .iter()
        .filter(|(_, &v)| v == 0)
        .map(|(k, _)| *k)
        .collect();
    let mut visited = 0usize;
    while let Some(n) = queue.pop() {
        visited += 1;
        if let Some(children) = adj.get(&n).cloned() {
            for c in children {
                if let Some(v) = in_degree.get_mut(&c) {
                    *v -= 1;
                    if *v == 0 {
                        queue.push(c);
                    }
                }
            }
        }
    }
    visited < nodes.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_types::tool_outcome::EsitoTool;

    #[test]
    fn valid_statuses_include_canonical_set() {
        for s in ["pending", "in_progress", "completed", "blocked", "skipped"] {
            assert!(VALID_STATUSES.contains(&s), "manca status {s}");
        }
    }

    #[test]
    fn valid_priorities_include_canonical_set() {
        for p in ["high", "normal", "low"] {
            assert!(VALID_PRIORITIES.contains(&p));
        }
    }

    /// L'esito sta nel CAMPO e il corpo resta un JSON integro: il marker in
    /// testa lo spezzava, e chi leggeva il risultato doveva guardare il primo
    /// carattere per sapere com'era andata.
    ///
    /// MUTAZIONE: sostituendo `rifiuta` con `RispostaTool::riuscito`, il campo
    /// `esito` smette di dichiarare il fallimento e questo test rosseggia.
    #[test]
    fn un_input_rifiutato_dichiara_esito_e_natura_nei_campi() {
        let r = rifiuta("boom");
        assert_eq!(r.esito, EsitoTool::Fallito);
        assert_eq!(r.natura, Some(NaturaFallimento::Rimediabile));
        let corpo: Value = serde_json::from_str(&r.testo).expect("corpo JSON integro");
        assert_eq!(corpo["error"], "boom");
    }

    /// Un errore del DB non manda l'agente a correggere la propria chiamata.
    #[test]
    fn un_errore_del_db_e_del_sistema() {
        let r = errore_db("lettura fallita", sqlx::Error::RowNotFound);
        assert_eq!(r.esito, EsitoTool::Fallito);
        assert_eq!(r.natura, Some(NaturaFallimento::DelSistema));
    }

    /// I default del catalogo li applica il parser, non il DB.
    #[test]
    fn i_default_di_un_todo_vengono_dal_catalogo() {
        let campi = campi_del_todo(0, &json!({"content": "scrivi i test"})).expect("todo valido");
        assert_eq!(campi.status, "pending");
        assert_eq!(campi.priority, "normal");
        assert_eq!(campi.seq, 1, "seq assente = posizione 1-based");
        assert!(campi.dep_keys.is_empty());
        assert!(campi.write_scope.is_empty());
        assert_eq!(campi.node_key, None);
    }

    /// Un vocabolario violato si ferma qui, e il messaggio porta i valori
    /// ammessi: e' cio' che rende `Rimediabile` una promessa mantenuta.
    ///
    /// MUTAZIONE: togliendo il controllo su VALID_PRIORITIES, il valore scende
    /// fino al CHECK del DB e questo test rosseggia.
    #[test]
    fn un_vocabolario_violato_non_arriva_al_db() {
        let t = json!({"content": "x", "status": "inventato"});
        let e = campi_del_todo(2, &t).expect_err("status fuori vocabolario");
        assert_eq!(e.natura, Some(NaturaFallimento::Rimediabile));
        for ammesso in VALID_STATUSES {
            assert!(e.testo.contains(ammesso), "manca '{ammesso}': {}", e.testo);
        }
        assert!(e.testo.contains("todo[2]"), "nomina la voce: {}", e.testo);

        let t = json!({"content": "x", "priority": "urgentissima"});
        let e = campi_del_todo(0, &t).expect_err("priority fuori vocabolario");
        assert_eq!(e.natura, Some(NaturaFallimento::Rimediabile));
        for ammessa in VALID_PRIORITIES {
            assert!(e.testo.contains(ammessa), "manca '{ammessa}': {}", e.testo);
        }
    }

    /// `check` non chiede lo status; `update` lo pretende, e distingue
    /// «mancante» da «fuori vocabolario».
    #[test]
    fn lo_status_richiesto_dipende_dall_azione() {
        let vuoto = json!({});
        assert_eq!(
            stato_richiesto(0, &vuoto, true).expect("check non guarda l'input"),
            "completed"
        );
        let e = stato_richiesto(1, &vuoto, false).expect_err("update pretende lo status");
        assert!(e.testo.contains("'status' obbligatorio"), "{}", e.testo);
        let e = stato_richiesto(1, &json!({"status": "quasi"}), false).expect_err("fuori elenco");
        assert!(e.testo.contains("quasi"), "{}", e.testo);
    }

    /// Il ciclo scartato e' DICHIARATO nel corpo: prima restava un `warn!` nei
    /// log e l'agente leggeva un `create` riuscito con zero dipendenze,
    /// indistinguibile da un piano che non ne aveva chieste.
    ///
    /// MUTAZIONE: riportando `resolve_and_persist_deps` a un `usize`, il caso
    /// del ciclo torna a coincidere con lo zero e questo test rosseggia.
    #[test]
    fn un_ciclo_scartato_si_vede_nel_corpo_della_risposta() {
        let run = Uuid::new_v4();
        let ids = vec!["a".to_string(), "b".to_string()];

        let sano = esito_create(run, &ids, EsitoDipendenze::Scritte(0));
        assert_eq!(sano["dag_deps_resolved"], 0);
        assert!(sano.get("dag_cycle_detected").is_none(), "{sano}");

        let ciclico = esito_create(run, &ids, EsitoDipendenze::CicloRilevato);
        assert_eq!(ciclico["ok"], true, "il piano c'e': non e' un fallimento");
        assert_eq!(ciclico["dag_deps_resolved"], 0);
        assert_eq!(ciclico["dag_cycle_detected"], true);
        assert!(
            ciclico["hint"].as_str().unwrap_or("").contains("ciclo"),
            "l'hint dice cosa correggere: {ciclico}"
        );
    }

    #[test]
    fn detect_cycle_acyclic_chain() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        // c depends_on b, b depends_on a: catena lineare, niente ciclo.
        let nodes = vec![(a, vec![]), (b, vec![a]), (c, vec![b])];
        assert!(!detect_cycle(&nodes));
    }

    #[test]
    fn detect_cycle_with_cycle() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        // a depends_on b e b depends_on a: ciclo.
        let nodes = vec![(a, vec![b]), (b, vec![a])];
        assert!(detect_cycle(&nodes));
    }

    #[test]
    fn detect_cycle_diamond_is_acyclic() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        let d = Uuid::new_v4();
        // d <- b,c <- a (diamante): nessun ciclo.
        let nodes = vec![(a, vec![]), (b, vec![a]), (c, vec![a]), (d, vec![b, c])];
        assert!(!detect_cycle(&nodes));
    }
}
