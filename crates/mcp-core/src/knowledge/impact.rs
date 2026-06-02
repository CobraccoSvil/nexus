//! M13.2 — Impact analysis: forward closure sul code graph + selezione test.
//!
//! Dato un seed set di file modificati, calcola l'impact set (i file che
//! dipendono dai seed, via `project_code_edges`) con una forward closure
//! limitata in profondita' e ampiezza (settings `impact.depth_cap`,
//! `impact.max_nodes`). Poi seleziona i test che coprono l'impact set via
//! `project_code_tests`. Consumato da:
//!   - `regression_gate_node.py` (endpoint REST `tests-for-run`)
//!   - `nexus_impact_brief` (tool agente consultivo)
//!
//! Granularita' file-level. Lo strato semantico (Qdrant) e' un enhancement
//! futuro: qui la closure e' puramente strutturale (edge_kind='import'), che
//! e' il segnale piu' affidabile per il regression gate.

use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

async fn read_int_setting(db: &PgPool, key: &str, default: i64) -> i64 {
    let v: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = $1 LIMIT 1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    v.and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(default)
}

async fn read_bool_setting(db: &PgPool, key: &str, default: bool) -> bool {
    let v: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = $1 LIMIT 1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    v.map(|s| !matches!(s.trim().to_ascii_lowercase().as_str(), "false" | "0" | "off" | "no"))
        .unwrap_or(default)
}

/// Forward closure: chi importa (direttamente o transitivamente) i file seed.
/// Query inversa su project_code_edges (to_path IN frontier -> from_path).
/// Ritorna i file impattati con la profondita' minima a cui sono stati raggiunti.
pub async fn compute_impact_set(
    db: &PgPool,
    project_id: Uuid,
    seed_paths: &[String],
) -> HashMap<String, i32> {
    let depth_cap = read_int_setting(db, "impact.depth_cap", 2).await as i32;
    let max_nodes = read_int_setting(db, "impact.max_nodes", 60).await as usize;

    let mut impacted: HashMap<String, i32> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut frontier: VecDeque<(String, i32)> = VecDeque::new();
    for p in seed_paths {
        frontier.push_back((p.clone(), 0));
        visited.insert(p.clone());
    }

    while let Some((path, depth)) = frontier.pop_front() {
        if depth >= depth_cap || impacted.len() >= max_nodes {
            continue;
        }
        // Chi importa `path`? (edge from_path -> to_path == path)
        let rows = sqlx::query(
            "SELECT DISTINCT from_path FROM project_code_edges \
             WHERE project_id = $1 AND to_path = $2 AND edge_kind = 'import' LIMIT 200",
        )
        .bind(project_id)
        .bind(&path)
        .fetch_all(db)
        .await
        .unwrap_or_default();
        for row in rows {
            let dep: String = match row.try_get("from_path") {
                Ok(d) => d,
                Err(_) => continue,
            };
            if visited.contains(&dep) {
                continue;
            }
            visited.insert(dep.clone());
            impacted.insert(dep.clone(), depth + 1);
            if impacted.len() >= max_nodes {
                break;
            }
            frontier.push_back((dep, depth + 1));
        }
    }
    impacted
}

/// Test che coprono i file dell'impact set (+ i seed). Query project_code_tests.
pub async fn select_tests_for_paths(
    db: &PgPool,
    project_id: Uuid,
    paths: &[String],
) -> Vec<Value> {
    if paths.is_empty() {
        return Vec::new();
    }
    let rows = sqlx::query(
        "SELECT DISTINCT test_path, covers_path, method, confidence \
         FROM project_code_tests \
         WHERE project_id = $1 AND covers_path = ANY($2) \
         ORDER BY confidence DESC LIMIT 50",
    )
    .bind(project_id)
    .bind(paths)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    rows.iter()
        .map(|r| {
            json!({
                "test_path": r.try_get::<String, _>("test_path").unwrap_or_default(),
                "covers_path": r.try_get::<String, _>("covers_path").unwrap_or_default(),
                "method": r.try_get::<String, _>("method").unwrap_or_default(),
                "confidence": r.try_get::<f32, _>("confidence").unwrap_or(0.0),
            })
        })
        .collect()
}

/// Backend dell'endpoint REST `tests-for-run` (consumato da regression_gate_node.py).
/// Ritorna {ok, disabled, tests:[...], impact_paths:[...]}.
pub async fn tests_for_run(db: &PgPool, project_id: Uuid, seed_paths: &[String]) -> Value {
    if !read_bool_setting(db, "impact.enabled", true).await {
        return json!({"ok": true, "disabled": true, "tests": []});
    }
    let impacted = compute_impact_set(db, project_id, seed_paths).await;
    // L'impact set per la selezione test = seed + impattati.
    let mut all_paths: Vec<String> = seed_paths.to_vec();
    for k in impacted.keys() {
        all_paths.push(k.clone());
    }
    let tests = select_tests_for_paths(db, project_id, &all_paths).await;
    json!({
        "ok": true,
        "disabled": false,
        "tests": tests,
        "impact_paths": impacted.keys().cloned().collect::<Vec<_>>(),
    })
}

/// Backend del tool agente `nexus_impact_brief`: dato un seed (file o query),
/// ritorna impact set + note KB pertinenti (ri-mappate via file_paths) + test.
pub async fn impact_brief(db: &PgPool, project_id: Uuid, seed_paths: &[String]) -> Value {
    let impacted = compute_impact_set(db, project_id, seed_paths).await;
    let mut all_paths: Vec<String> = seed_paths.to_vec();
    for k in impacted.keys() {
        all_paths.push(k.clone());
    }

    // Note KB che toccano i file impattati (decisioni/vincoli storici).
    let notes = sqlx::query(
        "SELECT DISTINCT id, title, kind FROM project_knowledge_notes \
         WHERE project_id = $1 AND file_paths && $2 \
           AND status IN ('active','draft') \
         ORDER BY id DESC LIMIT 10",
    )
    .bind(project_id)
    .bind(&all_paths)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    let related_notes: Vec<Value> = notes
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<Uuid, _>("id").map(|u| u.to_string()).unwrap_or_default(),
                "title": r.try_get::<String, _>("title").unwrap_or_default(),
                "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
            })
        })
        .collect();

    let tests = select_tests_for_paths(db, project_id, &all_paths).await;

    json!({
        "seed_paths": seed_paths,
        "impact_paths": impacted.keys().cloned().collect::<Vec<_>>(),
        "impacted_count": impacted.len(),
        "related_notes": related_notes,
        "tests": tests,
    })
}
