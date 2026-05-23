//! Generators per arricchire la Knowledge Base per-progetto con note
//! tecniche, funzionali e di test (oltre alle chat auto-create).
//!
//! Pattern simile ai meta_docs generators ma per progetto specifico:
//! ogni generator legge da DB/filesystem del progetto e produce
//! `Vec<GeneratedProjectNote>` che vengono UPSERT in `project_knowledge_notes`
//! con `kind='technical'|'functional'|'test'` e `source_message_id=NULL`.

use crate::vector_memory;
use crate::AppState;
use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::json;
use sqlx::Row;
use std::collections::HashMap;
use uuid::Uuid;

/// Output di un generator: una nota pronta da UPSERT.
#[derive(Debug, Clone)]
pub struct GeneratedProjectNote {
    pub kind: String, // 'technical' | 'functional' | 'test'
    pub title: String,
    pub body_md: String,
    pub intent: Option<String>,
    pub tags: Vec<String>,
    pub file_paths: Vec<String>,
}

/// Applica una nota generata: UPSERT idempotente in `project_knowledge_notes` +
/// upsert Qdrant + scrittura tag aggregati. La unique key e' `(project_id, kind, title)`
/// emulata via SELECT-then-INSERT.
pub async fn apply_project_note(
    state: &AppState,
    project_id: Uuid,
    note: &GeneratedProjectNote,
) -> Result<Uuid> {
    // Cerca esistente con stesso (project_id, kind, title)
    let existing: Option<Uuid> = sqlx::query_scalar(
        r#"
        SELECT id FROM project_knowledge_notes
        WHERE project_id = $1 AND kind = $2 AND title = $3
        LIMIT 1
        "#,
    )
    .bind(project_id)
    .bind(&note.kind)
    .bind(&note.title)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    let note_id = existing.unwrap_or_else(Uuid::new_v4);

    // Genera embedding (best-effort)
    let embed_text = if note.body_md.len() > 2000 {
        &note.body_md[..2000]
    } else {
        &note.body_md
    };
    let combined = format!("{}\n\n{}", note.title, embed_text);
    let qdrant_point_id = match state
        .orchestrator
        .neural
        .embed_text("", &combined)
        .await
    {
        Ok(vector) => {
            let point_id = Uuid::new_v4().to_string();
            let payload = json!({
                "project_id": project_id.to_string(),
                "note_id": note_id.to_string(),
                "intent": note.intent.clone().unwrap_or_else(|| note.kind.clone()),
                "status": "active",
                "kind": note.kind,
            });
            match vector_memory::upsert_knowledge_point(&state.db, &point_id, vector, payload).await
            {
                Ok(_) => Some(point_id),
                Err(_) => None,
            }
        }
        Err(_) => None,
    };

    if existing.is_some() {
        sqlx::query(
            r#"
            UPDATE project_knowledge_notes SET
                body_md = $1,
                intent = $2,
                tags = $3,
                file_paths = $4,
                qdrant_point_id = COALESCE($5, qdrant_point_id),
                status = 'active',
                updated_at = NOW()
            WHERE id = $6
            "#,
        )
        .bind(&note.body_md)
        .bind(note.intent.as_deref())
        .bind(&note.tags)
        .bind(&note.file_paths)
        .bind(qdrant_point_id.as_deref())
        .bind(note_id)
        .execute(&state.db)
        .await
        .context("UPDATE project_knowledge_notes")?;
    } else {
        sqlx::query(
            r#"
            INSERT INTO project_knowledge_notes
                (id, project_id, kind, intent, title, body_md, status, qdrant_point_id, tags, file_paths)
            VALUES ($1, $2, $3, $4, $5, $6, 'active', $7, $8, $9)
            "#,
        )
        .bind(note_id)
        .bind(project_id)
        .bind(&note.kind)
        .bind(note.intent.as_deref())
        .bind(&note.title)
        .bind(&note.body_md)
        .bind(qdrant_point_id.as_deref())
        .bind(&note.tags)
        .bind(&note.file_paths)
        .execute(&state.db)
        .await
        .context("INSERT project_knowledge_notes")?;
    }

    // Tag aggregati
    for tag in &note.tags {
        let _ = sqlx::query(
            r#"
            INSERT INTO project_knowledge_tags (project_id, tag, note_count, last_used_at)
            VALUES ($1, $2, 1, NOW())
            ON CONFLICT (project_id, tag) DO UPDATE SET
                note_count = project_knowledge_tags.note_count + 1,
                last_used_at = NOW()
            "#,
        )
        .bind(project_id)
        .bind(tag)
        .execute(&state.db)
        .await;
    }

    Ok(note_id)
}

/// **ProjectTechGenerator** — analizza `file_snapshots` e `repositories` del
/// progetto e produce note tecniche su architettura, file structure, schema.
pub async fn generate_technical_notes(
    state: &AppState,
    project_id: Uuid,
) -> Result<Vec<GeneratedProjectNote>> {
    let mut notes: Vec<GeneratedProjectNote> = Vec::new();

    // 1. Lingue + framework (da `projects.analysis_json` se popolato)
    let row = sqlx::query(
        "SELECT name, repository_root_path, analysis_json FROM projects WHERE id = $1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await?;
    if let Some(row) = row {
        let proj_name: String = row.try_get("name").unwrap_or_default();
        let repo_root: String = row.try_get("repository_root_path").unwrap_or_default();
        let analysis: Option<serde_json::Value> = row.try_get("analysis_json").ok();

        // File structure overview (da file_index_hashes)
        let count_by_ext = sqlx::query(
            r#"
            SELECT
                regexp_replace(file_path, '^.*\.', '') AS ext,
                count(*) AS files
            FROM file_index_hashes
            WHERE project_id = $1
            GROUP BY regexp_replace(file_path, '^.*\.', '')
            ORDER BY files DESC
            LIMIT 20
            "#,
        )
        .bind(project_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

        if !count_by_ext.is_empty() {
            let mut body = String::new();
            body.push_str(&format!(
                "Distribuzione file del progetto `{proj_name}` per estensione (da `file_index_hashes`).\n\n"
            ));
            body.push_str("| Estensione | File |\n|---|---|\n");
            let mut exts: Vec<String> = Vec::new();
            let mut total = 0i64;
            for r in &count_by_ext {
                let ext: String = r.try_get("ext").unwrap_or_default();
                let files: i64 = r.try_get("files").unwrap_or(0);
                let ext_disp = if ext.is_empty() { "(no-ext)".to_string() } else { ext.clone() };
                body.push_str(&format!("| `{ext_disp}` | {files} |\n"));
                total += files;
                if !ext.is_empty() {
                    exts.push(ext);
                }
            }
            body.push_str(&format!("\n**Totale file indicizzati**: {total}\n**Repository root**: `{repo_root}`\n"));
            notes.push(GeneratedProjectNote {
                kind: "technical".to_string(),
                title: format!("File structure del progetto {proj_name}"),
                body_md: body,
                intent: Some("architecture".to_string()),
                tags: vec!["technical".to_string(), "structure".to_string()],
                file_paths: vec![],
            });

            if let Some(top) = exts.first().cloned() {
                let lang_name = match top.as_str() {
                    "rs" => "Rust",
                    "ts" | "tsx" => "TypeScript",
                    "js" | "jsx" => "JavaScript",
                    "py" => "Python",
                    "go" => "Go",
                    "java" => "Java",
                    "cs" => "C#",
                    "vue" => "Vue.js",
                    _ => top.as_str(),
                };
                notes.push(GeneratedProjectNote {
                    kind: "technical".to_string(),
                    title: format!("Linguaggio principale: {lang_name}"),
                    body_md: format!(
                        "Il progetto `{proj_name}` ha **{lang_name}** (estensione `.{top}`) come linguaggio dominante per numero di file.\n\nQuesto influenza la scelta di toolchain (linter, formatter, test runner) e le convenzioni di codice.\n"
                    ),
                    intent: Some("architecture".to_string()),
                    tags: vec!["technical".to_string(), "language".to_string(), lang_name.to_lowercase()],
                    file_paths: vec![],
                });
            }
        }

        // 2. Endpoint API (da file_index_hashes con pattern path)
        let api_files = sqlx::query(
            r#"
            SELECT file_path FROM file_index_hashes
            WHERE project_id = $1
              AND file_path ~* '(router|route|api|controller|endpoint|handler)'
              AND file_path ~* '\.(rs|ts|tsx|py|go|java)$'
            ORDER BY file_path
            LIMIT 50
            "#,
        )
        .bind(project_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

        if !api_files.is_empty() {
            let mut body = String::new();
            body.push_str("File relativi all'API del progetto (pattern path: router/route/api/controller/endpoint/handler).\n\n");
            let mut paths: Vec<String> = Vec::new();
            for r in api_files.iter().take(30) {
                let p: String = r.try_get("file_path").unwrap_or_default();
                body.push_str(&format!("- `{p}`\n"));
                paths.push(p);
            }
            if api_files.len() > 30 {
                body.push_str(&format!("\n_({} file aggiuntivi non mostrati)_\n", api_files.len() - 30));
            }
            notes.push(GeneratedProjectNote {
                kind: "technical".to_string(),
                title: format!("API endpoints del progetto {proj_name}"),
                body_md: body,
                intent: Some("api".to_string()),
                tags: vec!["technical".to_string(), "api".to_string()],
                file_paths: paths,
            });
        }

        // 3. Schema DB (file SQL/Prisma)
        let sql_files = sqlx::query(
            "SELECT file_path FROM file_index_hashes WHERE project_id = $1 AND file_path ~* '\\.(sql|prisma)$' ORDER BY file_path LIMIT 30",
        )
        .bind(project_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        if !sql_files.is_empty() {
            let mut body = String::new();
            body.push_str("File schema database del progetto (SQL/Prisma).\n\n");
            let mut paths: Vec<String> = Vec::new();
            for r in sql_files.iter().take(30) {
                let p: String = r.try_get("file_path").unwrap_or_default();
                body.push_str(&format!("- `{p}`\n"));
                paths.push(p);
            }
            notes.push(GeneratedProjectNote {
                kind: "technical".to_string(),
                title: format!("Schema database del progetto {proj_name}"),
                body_md: body,
                intent: Some("schema".to_string()),
                tags: vec!["technical".to_string(), "database".to_string(), "schema".to_string()],
                file_paths: paths,
            });
        }

        // 4. Componenti frontend (React/Vue)
        let component_files = sqlx::query(
            r#"
            SELECT file_path FROM file_index_hashes
            WHERE project_id = $1
              AND file_path ~* '/(components?|pages?|views?)/.*\.(tsx|jsx|vue|ts|js)$'
            ORDER BY file_path
            LIMIT 50
            "#,
        )
        .bind(project_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        if !component_files.is_empty() {
            let mut body = String::new();
            body.push_str("Componenti frontend identificati (pattern path components/pages/views).\n\n");
            let mut paths: Vec<String> = Vec::new();
            for r in component_files.iter().take(40) {
                let p: String = r.try_get("file_path").unwrap_or_default();
                body.push_str(&format!("- `{p}`\n"));
                paths.push(p);
            }
            if component_files.len() > 40 {
                body.push_str(&format!("\n_({} componenti aggiuntivi)_\n", component_files.len() - 40));
            }
            notes.push(GeneratedProjectNote {
                kind: "technical".to_string(),
                title: format!("Componenti frontend del progetto {proj_name}"),
                body_md: body,
                intent: Some("frontend".to_string()),
                tags: vec!["technical".to_string(), "frontend".to_string(), "ui".to_string()],
                file_paths: paths,
            });
        }

        // 5. Config files (Dockerfile, package.json, Cargo.toml, ecc.)
        let config_files = sqlx::query(
            r#"
            SELECT file_path FROM file_index_hashes
            WHERE project_id = $1
              AND (
                file_path ~* '(dockerfile|docker-compose|package\.json|cargo\.toml|pyproject\.toml|go\.mod|requirements\.txt|tsconfig)'
                AND file_path !~* 'node_modules'
              )
            ORDER BY file_path
            LIMIT 30
            "#,
        )
        .bind(project_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        if !config_files.is_empty() {
            let mut body = String::new();
            body.push_str("File di configurazione e build del progetto.\n\n");
            let mut paths: Vec<String> = Vec::new();
            for r in config_files.iter().take(30) {
                let p: String = r.try_get("file_path").unwrap_or_default();
                body.push_str(&format!("- `{p}`\n"));
                paths.push(p);
            }
            notes.push(GeneratedProjectNote {
                kind: "technical".to_string(),
                title: format!("Config & build files del progetto {proj_name}"),
                body_md: body,
                intent: Some("config".to_string()),
                tags: vec!["technical".to_string(), "config".to_string(), "build".to_string()],
                file_paths: paths,
            });
        }

        // 4. Framework/dependencies (se analysis_json popolato)
        if let Some(analysis) = analysis {
            if let Some(frameworks) = analysis.get("frameworks").and_then(|v| v.as_array()) {
                if !frameworks.is_empty() {
                    let mut body = String::new();
                    body.push_str("Framework rilevati nell'analisi del progetto:\n\n");
                    for fw in frameworks.iter().take(20) {
                        if let Some(name) = fw.as_str() {
                            body.push_str(&format!("- **{name}**\n"));
                        } else if let Some(name) = fw.get("name").and_then(|v| v.as_str()) {
                            body.push_str(&format!("- **{name}**\n"));
                        }
                    }
                    notes.push(GeneratedProjectNote {
                        kind: "technical".to_string(),
                        title: format!("Framework e dipendenze del progetto {proj_name}"),
                        body_md: body,
                        intent: Some("architecture".to_string()),
                        tags: vec!["technical".to_string(), "framework".to_string(), "dependencies".to_string()],
                        file_paths: vec![],
                    });
                }
            }
        }
    }

    Ok(notes)
}

/// **ProjectFunctionalGenerator** — raggruppa chat_messages user per intent e
/// produce cluster note funzionali (feature/requirement/decision/...).
pub async fn generate_functional_notes(
    state: &AppState,
    project_id: Uuid,
) -> Result<Vec<GeneratedProjectNote>> {
    let proj_name: String = sqlx::query_scalar("SELECT name FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or_else(|| "(unknown)".to_string());

    // Aggrega chat_messages user per intent (dal metadata)
    let rows = sqlx::query(
        r#"
        SELECT
            COALESCE(cm.metadata->>'intent', 'unknown') AS intent,
            count(*) AS msg_count,
            array_agg(left(cm.content, 200) ORDER BY cm.created_at DESC) AS samples
        FROM chat_messages cm
        JOIN chat_sessions cs ON cs.id = cm.session_id
        WHERE cs.project_id = $1
          AND cm.role = 'user'
          AND length(cm.content) BETWEEN 20 AND 4000
        GROUP BY COALESCE(cm.metadata->>'intent', 'unknown')
        ORDER BY count(*) DESC
        LIMIT 12
        "#,
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let mut notes: Vec<GeneratedProjectNote> = Vec::new();
    let mut intent_total: HashMap<String, i64> = HashMap::new();

    for r in &rows {
        let intent: String = r.try_get("intent").unwrap_or_else(|_| "unknown".to_string());
        let count: i64 = r.try_get("msg_count").unwrap_or(0);
        let samples: Vec<String> = r.try_get("samples").unwrap_or_default();
        intent_total.insert(intent.clone(), count);
        if count < 1 || intent == "unknown" {
            continue;
        }
        // Crea una nota cluster per ogni intent significativo
        let title = format!("Cluster {}: {} richieste utente", intent, count);
        let mut body = format!(
            "Cluster di **{count}** messaggi user con intent `{intent}` nel progetto `{proj_name}`.\n\nRichieste rappresentative (ultime {}):\n\n",
            samples.len().min(5)
        );
        for (i, s) in samples.iter().take(5).enumerate() {
            let snippet = s.trim();
            body.push_str(&format!("{}. {snippet}\n\n", i + 1));
        }
        body.push_str(&format!(
            "_Fonte: aggregazione `chat_messages.metadata->>'intent'` (intent classifier semantico). Aggiorna `intent` nelle note KB chat per coerenza._\n"
        ));
        notes.push(GeneratedProjectNote {
            kind: "functional".to_string(),
            title,
            body_md: body,
            intent: Some(intent.clone()),
            tags: vec!["functional".to_string(), "cluster".to_string(), intent.clone()],
            file_paths: vec![],
        });
    }

    // Nota overview funzionale
    if !intent_total.is_empty() {
        let mut body = String::new();
        body.push_str(&format!(
            "Distribuzione **funzionale** delle richieste utente nel progetto `{proj_name}`.\n\n| Intent | Numero |\n|---|---|\n"
        ));
        let mut sorted: Vec<(&String, &i64)> = intent_total.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));
        for (intent, count) in &sorted {
            body.push_str(&format!("| `{intent}` | {count} |\n"));
        }
        body.push_str(&format!(
            "\n**Totale messaggi user**: {}\n",
            intent_total.values().sum::<i64>()
        ));
        notes.push(GeneratedProjectNote {
            kind: "functional".to_string(),
            title: format!("Overview funzionale {proj_name}"),
            body_md: body,
            intent: Some("overview".to_string()),
            tags: vec!["functional".to_string(), "overview".to_string()],
            file_paths: vec![],
        });
    }

    Ok(notes)
}

/// **ProjectTestGenerator** — scansiona file_snapshots per identificare test
/// (per linguaggio + pattern path) e produce note descrittive.
pub async fn generate_test_notes(
    state: &AppState,
    project_id: Uuid,
) -> Result<Vec<GeneratedProjectNote>> {
    let proj_name: String = sqlx::query_scalar("SELECT name FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or_else(|| "(unknown)".to_string());

    // Pattern test file (da file_index_hashes)
    let test_files = sqlx::query(
        r#"
        SELECT file_path FROM file_index_hashes
        WHERE project_id = $1
          AND (
            file_path ~* '(_test\.|\.test\.|\.spec\.|^tests?/|/test/|/tests/|/__tests__/)'
            OR file_path ~* '(playwright|pytest|cypress|jest|vitest)'
          )
        ORDER BY file_path
        LIMIT 200
        "#,
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let mut notes: Vec<GeneratedProjectNote> = Vec::new();
    if test_files.is_empty() {
        return Ok(notes);
    }

    // Raggruppa per linguaggio (estensione) e per tipologia (unit/integration/e2e)
    let mut by_lang: HashMap<String, Vec<String>> = HashMap::new();
    let mut by_type: HashMap<&str, Vec<String>> = HashMap::new();
    by_type.insert("e2e", Vec::new());
    by_type.insert("integration", Vec::new());
    by_type.insert("unit", Vec::new());

    for r in &test_files {
        let path: String = r.try_get("file_path").unwrap_or_default();
        // Deduce lang da estensione
        let lang = if path.ends_with(".rs") {
            "Rust"
        } else if path.ends_with(".ts") || path.ends_with(".tsx") {
            "TypeScript"
        } else if path.ends_with(".js") || path.ends_with(".jsx") {
            "JavaScript"
        } else if path.ends_with(".py") {
            "Python"
        } else if path.ends_with(".go") {
            "Go"
        } else {
            "other"
        };
        by_lang.entry(lang.to_string()).or_default().push(path.clone());

        let p_lower = path.to_lowercase();
        let test_type = if p_lower.contains("e2e") || p_lower.contains("playwright") || p_lower.contains("cypress") {
            "e2e"
        } else if p_lower.contains("integration") || p_lower.contains("/it/") {
            "integration"
        } else {
            "unit"
        };
        by_type.get_mut(test_type).unwrap().push(path);
    }

    // 1. Overview test
    let total = test_files.len();
    let mut body = format!(
        "Test trovati nel progetto `{proj_name}`: **{total}** file totali.\n\n## Per tipologia\n\n"
    );
    for (ttype, paths) in &by_type {
        body.push_str(&format!("- **{ttype}**: {} file\n", paths.len()));
    }
    body.push_str("\n## Per linguaggio\n\n");
    for (lang, paths) in &by_lang {
        body.push_str(&format!("- `{lang}`: {} file\n", paths.len()));
    }
    body.push_str("\n_Fonte: `file_snapshots` filtrato per pattern path noti (test/__tests__/.spec/.test/playwright/pytest/cypress/jest)._\n");

    notes.push(GeneratedProjectNote {
        kind: "test".to_string(),
        title: format!("Overview test del progetto {proj_name}"),
        body_md: body,
        intent: Some("test".to_string()),
        tags: vec!["test".to_string(), "overview".to_string()],
        file_paths: test_files
            .iter()
            .filter_map(|r| r.try_get::<String, _>("file_path").ok())
            .collect(),
    });

    // 2. Una nota per tipologia (solo se presenti)
    for (ttype, paths) in &by_type {
        if paths.is_empty() {
            continue;
        }
        let mut body = format!(
            "Test di tipo **{ttype}** del progetto `{proj_name}` ({} file).\n\n",
            paths.len()
        );
        for p in paths.iter().take(50) {
            body.push_str(&format!("- `{p}`\n"));
        }
        if paths.len() > 50 {
            body.push_str(&format!("\n_(...{} file aggiuntivi non mostrati)_\n", paths.len() - 50));
        }
        notes.push(GeneratedProjectNote {
            kind: "test".to_string(),
            title: format!("Test {ttype} - {proj_name}"),
            body_md: body,
            intent: Some("test".to_string()),
            tags: vec!["test".to_string(), ttype.to_string()],
            file_paths: paths.clone(),
        });
    }

    Ok(notes)
}

/// Wrapper che chiama tutti e 3 i generator + applica le note.
/// Ritorna `(generated_count, applied_count)`.
pub async fn generate_and_apply_all(
    state: &AppState,
    project_id: Uuid,
) -> Result<(usize, usize)> {
    let mut all_notes: Vec<GeneratedProjectNote> = Vec::new();
    if let Ok(mut n) = generate_technical_notes(state, project_id).await {
        all_notes.append(&mut n);
    }
    if let Ok(mut n) = generate_functional_notes(state, project_id).await {
        all_notes.append(&mut n);
    }
    if let Ok(mut n) = generate_test_notes(state, project_id).await {
        all_notes.append(&mut n);
    }

    let total = all_notes.len();
    let mut applied = 0usize;
    for note in &all_notes {
        if apply_project_note(state, project_id, note).await.is_ok() {
            applied += 1;
        }
    }
    tracing::info!(
        project_id = %project_id,
        total,
        applied,
        "generate_and_apply_all: completato"
    );
    Ok((total, applied))
}
