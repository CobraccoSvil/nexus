//! Costruisce il blocco "MEMORIA PROGETTO" da iniettare nel system prompt.
//!
//! Tre sezioni con header stabili (cache-friendly per Anthropic/OpenAI):
//!   1. ## Project Facts  — metadata + memory_entries ns_type='project'
//!   2. ## Conventions    — memory_entries ns_type IN ('global','swarm')
//!   3. ## Relevant Knowledge — RAG Qdrant project_context (opzionale, richiede embedding)
//!
//! Budget hard cap: 10_000 caratteri ≈ 2500 token.
//! Non fallisce mai: qualsiasi errore produce stringa vuota.

use sqlx::{PgPool, Row};
use uuid::Uuid;
use serde_json::Value;

use crate::orchestrator::NeuralCoreClient;

const MAX_BLOCK_CHARS: usize = 10_000;
const RAG_MIN_SCORE: f64 = 0.72;
const RAG_TOP_K: u64 = 5;
const MAX_PROJECT_ENTRIES: i64 = 8;
const MAX_GLOBAL_ENTRIES: i64 = 8;

const BLOCK_START: &str =
    "\n=== MEMORIA PROGETTO (non chiedere queste info: sono già disponibili qui) ===";
const BLOCK_END: &str = "=== FINE MEMORIA PROGETTO ===";

/// Costruisce il blocco contesto progetto da DB + opzionalmente Qdrant.
/// Non fallisce mai: restituisce stringa vuota in caso di errore.
pub async fn build_project_context_block(
    db: &PgPool,
    project_id: Uuid,
    neural: &NeuralCoreClient,
    user_query: &str,
) -> String {
    let mut sections: Vec<String> = Vec::new();

    if let Some(facts) = query_project_facts(db, project_id).await {
        sections.push(facts);
    }

    if let Some(conv) = query_global_conventions(db).await {
        sections.push(conv);
    }

    // Porte riservate dall'infrastruttura Nexus — sempre iniettate.
    // Previene conflitti quando l'agente sceglie porte per nuovi servizi.
    sections.push(nexus_reserved_ports_section());

    if user_query.len() >= 20 {
        if let Some(rag) = search_qdrant_context(db, neural, project_id, user_query).await {
            sections.push(rag);
        }
    }

    if sections.is_empty() {
        return String::new();
    }

    let body = sections.join("\n\n");
    let body = if body.len() > MAX_BLOCK_CHARS {
        body[..MAX_BLOCK_CHARS].to_string()
    } else {
        body
    };

    format!("{BLOCK_START}\n{body}\n{BLOCK_END}")
}

async fn query_project_facts(db: &PgPool, project_id: Uuid) -> Option<String> {
    let row: Option<(String, Option<Value>, Option<String>, Option<String>)> =
        sqlx::query_as(
            "SELECT name, analysis_json, custom_instructions, repository_root_path \
             FROM projects WHERE id = $1",
        )
        .bind(project_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let mut lines: Vec<String> = vec!["## Project Facts".to_string()];
    let mut has_content = false;

    if let Some((name, analysis_json, custom_instr, root_path)) = row {
        has_content = true;
        lines.push(format!("Progetto: {name}"));
        if let Some(ref root) = root_path {
            lines.push(format!("Root: {root}"));
            // G2: sezione Docker Services — comandi precisi per avviare/fermare/loggare
            if let Some(docker_section) = build_docker_section(root) {
                lines.push(docker_section);
            }
        }
        if let Some(analysis) = analysis_json {
            if let Some(langs) = extract_languages(&analysis) {
                lines.push(format!("Linguaggi: {langs}"));
            }
            if let Some(fws) = extract_frameworks(&analysis) {
                lines.push(format!("Framework/stack: {fws}"));
            }
            if let Some(scripts) = extract_scripts(&analysis) {
                lines.push(format!("Script disponibili:\n{scripts}"));
            }
        }
        if let Some(ci) = custom_instr.filter(|s| !s.trim().is_empty()) {
            lines.push(format!("Istruzioni specifiche:\n{ci}"));
        }
    }

    let entries = query_project_memory_entries(db, project_id).await;
    if !entries.is_empty() {
        has_content = true;
        lines.push("Memoria di progetto:".to_string());
        for (k, v) in entries {
            lines.push(format!("  {k}: {v}"));
        }
    }

    if !has_content {
        None
    } else {
        Some(lines.join("\n"))
    }
}

async fn query_global_conventions(db: &PgPool) -> Option<String> {
    let entries = query_global_memory_entries(db).await;
    if entries.is_empty() {
        return None;
    }
    let mut lines = vec!["## Conventions".to_string()];
    for (k, v) in entries {
        lines.push(format!("  {k}: {v}"));
    }
    Some(lines.join("\n"))
}

async fn query_project_memory_entries(db: &PgPool, project_id: Uuid) -> Vec<(String, String)> {
    let rows = sqlx::query(
        "SELECT me.entry_key, me.value \
         FROM memory_entries me \
         JOIN memory_namespaces mn ON mn.id = me.namespace_id \
         WHERE mn.project_id = $1 \
           AND mn.ns_type = 'project' \
           AND mn.active = TRUE \
           AND me.deleted = FALSE \
           AND (me.expires_at IS NULL OR me.expires_at > NOW()) \
         ORDER BY me.updated_at DESC \
         LIMIT $2",
    )
    .bind(project_id)
    .bind(MAX_PROJECT_ENTRIES)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    extract_kv_rows(rows)
}

async fn query_global_memory_entries(db: &PgPool) -> Vec<(String, String)> {
    let rows = sqlx::query(
        "SELECT me.entry_key, me.value \
         FROM memory_entries me \
         JOIN memory_namespaces mn ON mn.id = me.namespace_id \
         WHERE mn.ns_type IN ('global', 'swarm') \
           AND mn.active = TRUE \
           AND me.deleted = FALSE \
           AND (me.expires_at IS NULL OR me.expires_at > NOW()) \
         ORDER BY me.updated_at DESC \
         LIMIT $1",
    )
    .bind(MAX_GLOBAL_ENTRIES)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    extract_kv_rows(rows)
}

fn extract_kv_rows(rows: Vec<sqlx::postgres::PgRow>) -> Vec<(String, String)> {
    rows.iter()
        .filter_map(|r| {
            let key: String = r.try_get("entry_key").ok()?;
            let val: Value = r.try_get("value").ok()?;
            let val_str = match &val {
                Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            Some((key, val_str))
        })
        .collect()
}

async fn search_qdrant_context(
    db: &PgPool,
    neural: &NeuralCoreClient,
    project_id: Uuid,
    user_query: &str,
) -> Option<String> {
    let embedding = match neural.embed_text("", user_query).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("project_context: embedding fallito, skip RAG: {e}");
            return None;
        }
    };

    let hits = match crate::vector_memory::search_project_context_points(
        db,
        &embedding,
        project_id,
        RAG_TOP_K,
        RAG_MIN_SCORE,
    )
    .await
    {
        Ok(h) => h,
        Err(e) => {
            tracing::debug!("project_context: qdrant search fallita, skip RAG: {e}");
            return None;
        }
    };

    if hits.is_empty() {
        return None;
    }

    let mut lines = vec!["## Relevant Knowledge".to_string()];
    for hit in hits.iter().take(5) {
        let text = hit
            .payload
            .get("text")
            .or_else(|| hit.payload.get("content"))
            .or_else(|| hit.payload.get("text_preview"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if !text.is_empty() {
            let preview = if text.len() > 500 { &text[..500] } else { text };
            lines.push(format!("- {preview}"));
        }
    }

    if lines.len() <= 1 {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn extract_languages(analysis: &Value) -> Option<String> {
    let langs = analysis
        .get("languages")
        .and_then(|l| l.as_array())
        .map(|arr| {
            arr.iter()
                .take(5)
                .filter_map(|e| e.get("language").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    if langs.is_empty() { None } else { Some(langs) }
}

fn extract_frameworks(analysis: &Value) -> Option<String> {
    let fws = analysis
        .get("frameworks")
        .and_then(|f| f.as_array())
        .map(|arr| {
            arr.iter()
                .take(6)
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    if fws.is_empty() { None } else { Some(fws) }
}

fn extract_scripts(analysis: &Value) -> Option<String> {
    let scripts = analysis
        .get("dependencies")
        .and_then(|d| d.get("node"))
        .and_then(|n| n.get("scripts"))
        .and_then(|s| s.as_object())
        .map(|m| {
            m.iter()
                .take(8)
                .map(|(k, v)| format!("  {} → {}", k, v.as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if scripts.is_empty() { None } else { Some(scripts) }
}

/// Sezione fissa con le porte riservate dall'infrastruttura Nexus.
/// Iniettata sempre nel system prompt per evitare che l'agente scelga
/// porte già occupate quando avvia o configura nuovi servizi di progetto.
fn nexus_reserved_ports_section() -> String {
    "## Porte riservate Nexus (NON usare per nuovi servizi)\n\
     PostgreSQL Nexus:  5432, 5433, 5434\n\
     Qdrant:            6333, 6334\n\
     Redis:             6379\n\
     Grafana:           3001\n\
     mcp-core API:      8000\n\
     brain Python:      8001\n\
     web-ide (Next.js): 3000\n\
     Usa 5440+ per PostgreSQL di progetto, 8080+ per backend, 5173+ per frontend Vite."
        .to_string()
}

/// G2 — Costruisce la sezione `## Docker Services` da iniettare in Project Facts.
/// Rileva i file docker-compose nella root del progetto, estrae i servizi e
/// produce i comandi precisi per avvio/stop/log — pronti all'uso da parte dell'agente.
/// Budget massimo: ~300 caratteri (rientra nel cap 10k di build_project_context_block).
fn build_docker_section(root_path: &str) -> Option<String> {
    let root = std::path::Path::new(root_path);
    let compose_files = crate::project_workspace::collect_compose_files(root);
    if compose_files.is_empty() {
        return None;
    }
    let primary = &compose_files[0];
    let file_name = primary.file_name()?.to_string_lossy().to_string();

    let services = crate::project_workspace::parse_compose_services(primary);
    let services_str = if services.is_empty() {
        "(servizi non rilevati)".to_string()
    } else {
        services.join(", ")
    };

    // Costruisce il flag `-f <file>` solo se il nome non è quello di default
    let f_flag = if file_name == "docker-compose.yml"
        || file_name == "compose.yml"
        || file_name == "docker-compose.yaml"
        || file_name == "compose.yaml"
    {
        String::new()
    } else {
        format!(" -f {file_name}")
    };

    let mut lines = vec![
        "## Docker Services".to_string(),
        format!("File compose: {file_name}"),
        format!("Servizi: {services_str}"),
        format!("Avvio:  docker compose{f_flag} up -d"),
        format!("Avvio+build: docker compose{f_flag} up -d --build"),
        format!("Stop:   docker compose{f_flag} down"),
        format!("Log:    docker compose{f_flag} logs --tail=80 <servizio>"),
        format!("Stato:  docker compose{f_flag} ps"),
    ];

    // Segnala eventuali file compose alternativi
    if compose_files.len() > 1 {
        let others: Vec<String> = compose_files[1..]
            .iter()
            .filter_map(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .take(3)
            .collect();
        if !others.is_empty() {
            lines.push(format!("Altri file: {}", others.join(", ")));
        }
    }

    Some(lines.join("\n"))
}
