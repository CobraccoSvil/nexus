//! Handler per il gruppo `documents` del server Nexus Builtin.
//! Gestisce generazione, aggiornamento, lista, ricerca e stato dei documenti.
//! Include le utility `bump_version` e `get_project_slug`.

use super::*;

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

pub(super) fn bump_version(version: &str, bump_type: &str) -> String {
    let parts: Vec<u32> = version.split('.').filter_map(|s| s.parse().ok()).collect();
    let (major, minor, patch) = (
        parts.first().copied().unwrap_or(1),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    );
    match bump_type {
        "major" => format!("{}.0.0", major + 1),
        "minor" => format!("{}.{}.0", major, minor + 1),
        _ => format!("{}.{}.{}", major, minor, patch + 1),
    }
}

pub(super) async fn get_project_slug(db: &PgPool, project_id: Uuid) -> Result<String, String> {
    let row = sqlx::query("SELECT name FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("[DB] {}", e))?;
    let name: String = row
        .ok_or_else(|| "[Errore] Progetto non trovato".to_string())?
        .try_get::<String, _>("name")
        .map_err(|e| format!("[DB] {}", e))?;
    Ok(name.to_lowercase().replace([' ', '_'], "-"))
}

// ---------------------------------------------------------------------------
// Handler: documents
// ---------------------------------------------------------------------------

/// True se il documento non ha ALCUN contenuto reale: tutte le sezioni e
/// sottosezioni hanno `content` vuoto/assente. In quel caso il .docx avrebbe
/// solo l'indice (i titoli) senza corpo: va considerato una generazione fallita
/// e NON salvato in silenzio (regola H). Capita quando il modello docs_generator
/// risponde con la sola struttura (titoli) senza riempire i content.
fn doc_content_is_empty(content: &Value) -> bool {
    fn section_has_text(sec: &Value) -> bool {
        let own = sec
            .get("content")
            .and_then(|c| c.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if own {
            return true;
        }
        if let Some(subs) = sec.get("subsections").and_then(|s| s.as_array()) {
            return subs.iter().any(section_has_text);
        }
        false
    }
    match content.get("sections").and_then(|s| s.as_array()) {
        Some(sections) => !sections.iter().any(section_has_text),
        // Nessun array "sections": consideriamo vuoto (struttura non valida).
        None => true,
    }
}

pub(super) async fn handle_doc_generate(
    db: &PgPool,
    project_id: Uuid,
    user_id: Uuid,
    args: &Value,
) -> String {
    let pid = match args.get("project_id").and_then(Value::as_str) {
        Some(s) => match Uuid::parse_str(s) {
            Ok(u) => u,
            Err(_) => {
                // Prova a cercare per nome progetto
                match sqlx::query_scalar::<_, Uuid>(
                    "SELECT id FROM projects WHERE name ILIKE $1 LIMIT 1",
                )
                .bind(s)
                .fetch_optional(db)
                .await
                {
                    Ok(Some(found_id)) => found_id,
                    _ => project_id,
                }
            }
        },
        None => project_id,
    };

    let doc_type = match args.get("doc_type").and_then(Value::as_str) {
        Some(t) => t.to_string(),
        None => return "[Errore] Parametro 'doc_type' obbligatorio".to_string(),
    };

    let title = args
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let standard = args
        .get("standard")
        .and_then(Value::as_str)
        .unwrap_or("ieee830")
        .to_string();

    // Resolve project root path
    let root_row = sqlx::query("SELECT w.absolute_path, p.name FROM workspaces w JOIN projects p ON p.id = w.project_id WHERE w.project_id = $1 AND w.is_primary = TRUE")
        .bind(pid)
        .fetch_optional(db)
        .await;

    let (root_path, project_name) = match root_row {
        Ok(Some(r)) => (
            r.try_get::<String, _>("absolute_path").unwrap_or_default(),
            r.try_get::<String, _>("name").unwrap_or_default(),
        ),
        _ => return "[Errore] Progetto non trovato o workspace mancante".to_string(),
    };

    // Se content_json manca o è vuoto, auto-genera il contenuto analizzando il progetto
    let content = match args.get("content_json") {
        Some(v) if !v.is_null() && v.as_object().map_or(true, |o| !o.is_empty()) => v.clone(),
        _ => {
            // Raccogli informazioni sul progetto per generare il contenuto automaticamente
            tracing::info!(
                "nexus_doc_generate: content_json mancante, auto-generazione per {}",
                doc_type
            );
            let brain_rest = std::env::var("NEURAL_CORE_REST_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
            // Leggi file chiave del progetto per il contesto
            let mut project_context = format!(
                "Progetto: {}\nRoot: {}\nTipo documento: {}\n\n",
                project_name, root_path, doc_type
            );
            // Cerca file importanti
            let key_files: &[&str] = &["README.md", "readme.md", "package.json", "Cargo.toml"];
            for filename in key_files {
                let fpath = format!("{}/{}", root_path, filename);
                if let Ok(content) = tokio::fs::read_to_string(&fpath).await {
                    let truncated: String = content.chars().take(2000).collect();
                    project_context.push_str(&format!("--- {} ---\n{}\n\n", filename, truncated));
                }
            }
            // Lista struttura directory (top-level)
            if let Ok(mut dir) = tokio::fs::read_dir(&root_path).await {
                project_context.push_str("--- Struttura directory ---\n");
                let mut count = 0;
                while let Ok(Some(entry)) = dir.next_entry().await {
                    if count >= 40 {
                        break;
                    }
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') {
                        continue;
                    }
                    let is_dir = entry.file_type().await.map_or(false, |t| t.is_dir());
                    project_context.push_str(&format!(
                        "{}{}\n",
                        name,
                        if is_dir { "/" } else { "" }
                    ));
                    count += 1;
                }
            }
            // Chiedi al brain di generare il content_json strutturato
            let doc_type_label = match doc_type.as_str() {
                "functional_analysis" => "Analisi Funzionale IEEE 830",
                "technical_analysis" => "Analisi Tecnica",
                "er_diagram" => "Diagramma ER e modello dati",
                "project_management" => "Piano di Gestione Progetto",
                "release_notes" => "Release Notes",
                _ => &doc_type,
            };
            let gen_prompt = format!(
                "Genera un documento strutturato di tipo '{}' per il progetto descritto sotto.\n\
                 Rispondi SOLO con JSON valido, senza markdown, senza ```.\n\
                 Il formato ESATTO deve essere:\n\
                 {{\"sections\":[{{\"number\":\"1\",\"title\":\"...\",\"content\":\"testo lungo e dettagliato\",\"subsections\":[{{\"number\":\"1.1\",\"title\":\"...\",\"content\":\"...\"}}]}}]}}\n\
                 Genera almeno 5 sezioni principali con sottosezioni. Ogni content deve essere almeno 2-3 frasi.\n\n\
                 CONTESTO PROGETTO:\n{}", doc_type_label, project_context
            );
            let http = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap();
            // Modello purpose-specific letto da DB (purpose: docs_generator)
            // invece che hardcoded. Vedi migrazione 0102.
            // Nota: handle_doc_generate non ha accesso a Orchestrator/AppState,
            // quindi facciamo una query diretta one-shot al DB.
            let (gen_provider, gen_model): (String, String) = match sqlx::query_as::<_, (String, String)>(
                "SELECT provider, model_id FROM nexus_purpose_model WHERE purpose = 'docs_generator' LIMIT 1"
            )
            .fetch_optional(db)
            .await
            {
                Ok(Some(row)) => row,
                Ok(None) => {
                    tracing::error!(
                        "purpose 'docs_generator' non configurato in nexus_purpose_model. \
                         Esegui INSERT con il modello desiderato (mig 0102)."
                    );
                    return "[Errore] purpose 'docs_generator' non configurato in nexus_purpose_model".to_string();
                }
                Err(e) => {
                    tracing::error!("query nexus_purpose_model fallita: {e}");
                    return format!("[Errore] query nexus_purpose_model fallita: {e}");
                }
            };
            let body = serde_json::json!({
                "provider": gen_provider,
                "model": gen_model,
                "prompt": gen_prompt
            });
            let resp = match http
                .post(format!("{}/complete", brain_rest))
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return format!("[Errore] Generazione automatica contenuto fallita: {e}"),
            };
            let resp_text = match resp.text().await {
                Ok(t) => t,
                Err(e) => return format!("[Errore] Lettura risposta brain fallita: {e}"),
            };
            // Prova a parsare il JSON dalla risposta
            let parsed: Result<Value, _> = serde_json::from_str(&resp_text);
            let content_val: Value = match parsed {
                Ok(v) => {
                    // La risposta potrebbe essere wrapper: {"content": "..."} o direttamente il JSON
                    if let Some(c) = v.get("content").and_then(|x| x.as_str()) {
                        let cleaned = c.trim().replace("```json", "").replace("```", "");
                        let parsed_inner: Result<Value, _> = serde_json::from_str(cleaned.trim());
                        parsed_inner.unwrap_or_else(|_| {
                            serde_json::json!({"sections": [{"number": "1", "title": doc_type_label, "content": cleaned.trim(), "subsections": []}]})
                        })
                    } else if v.get("sections").is_some() {
                        v
                    } else if let Some(text) = v.get("text").and_then(|x| x.as_str()) {
                        let cleaned = text.trim().replace("```json", "").replace("```", "");
                        let parsed_inner: Result<Value, _> = serde_json::from_str(cleaned.trim());
                        parsed_inner.unwrap_or_else(|_| {
                            serde_json::json!({"sections": [{"number": "1", "title": doc_type_label, "content": cleaned.trim(), "subsections": []}]})
                        })
                    } else {
                        v
                    }
                }
                Err(_) => {
                    // Non è JSON, usa come testo raw
                    let raw: String = resp_text.chars().take(5000).collect();
                    serde_json::json!({"sections": [{"number": "1", "title": doc_type_label, "content": raw, "subsections": []}]})
                }
            };
            let sec_count = content_val
                .get("sections")
                .and_then(|s| s.as_array())
                .map_or(0, |a: &Vec<Value>| a.len());
            tracing::info!(
                "nexus_doc_generate: contenuto auto-generato con {} sezioni",
                sec_count
            );
            content_val
        }
    };

    // Validazione anti-documento-vuoto (regola H): se tutte le sezioni hanno
    // content vuoto, il .docx avrebbe solo l'indice. Non salviamo un documento
    // vuoto in silenzio: ritorniamo un errore esplicito e azionabile. Vale sia
    // per l'auto-generazione (modello che produce solo titoli) sia per un
    // content_json fornito a mano ma vuoto.
    if doc_content_is_empty(&content) {
        tracing::warn!(
            doc_type = %doc_type,
            "nexus_doc_generate: contenuto vuoto (solo indice) — generazione rifiutata"
        );
        return format!(
            "[Errore] Generazione documento '{}' fallita: tutte le sezioni risultano prive di contenuto \
             (il documento avrebbe solo l'indice). Il modello docs_generator non ha prodotto testo. \
             Riprova la generazione; se il problema persiste, verifica il provider/modello configurato \
             in nexus_purpose_model (purpose='docs_generator').",
            doc_type
        );
    }

    // Determina versione (incrementa se lo stesso doc_type esiste già)
    let existing = sqlx::query("SELECT version FROM project_documents WHERE project_id = $1 AND doc_type = $2 ORDER BY created_at DESC LIMIT 1")
        .bind(pid)
        .bind(&doc_type)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    let version = match existing {
        Some(r) => {
            let v: String = r.try_get("version").unwrap_or_else(|_| "1.0.0".to_string());
            bump_version(&v, "minor")
        }
        None => "1.0.0".to_string(),
    };

    let slug = doc_type.replace('_', "-");
    let relative_path = format!("docs/{}-v{}.docx", slug, version);
    let abs_output = format!("{}/{}", root_path, relative_path);
    let content_str = serde_json::to_string(&content).unwrap_or_default();

    // Usa NeuralCoreClient per la generazione del documento
    let neural_url = crate::auth::get_setting(db, "neural_core_url")
        .await
        .unwrap_or_else(|| {
            std::env::var("NEURAL_CORE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:50051".to_string())
        });

    let neural = match crate::orchestrator::NeuralCoreClient::connect(&neural_url).await {
        Ok(c) => c,
        Err(e) => return format!("[Errore] Connessione a Neural Core fallita: {e}"),
    };

    let final_title = if title.is_empty() {
        slug.replace('-', " ")
    } else {
        title.clone()
    };

    match neural
        .generate_document(
            &doc_type,
            &content_str,
            &abs_output,
            &standard,
            &final_title,
            &project_name,
        )
        .await
    {
        Ok((_file_path, page_count, section_count)) => {
            // Salva nel DB
            let doc_id = Uuid::new_v4();
            let _ = sqlx::query(
                "INSERT INTO project_documents (id, project_id, doc_type, title, version, file_path, structure_json, status, created_by)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, 'draft', $8)"
            )
            .bind(doc_id)
            .bind(pid)
            .bind(&doc_type)
            .bind(&final_title)
            .bind(&version)
            .bind(&relative_path)
            .bind(&content)
            .bind(user_id)
            .execute(db)
            .await;

            // Vettorializzazione in background
            let db2 = db.clone();
            let content2 = content.clone();
            let doc_id2 = doc_id;
            let pid2 = pid;
            let doc_type2 = doc_type.clone();
            let version2 = version.clone();
            tokio::spawn(async move {
                if let Err(e) = crate::vector_memory::vectorize_document(
                    &db2, pid2, doc_id2, &doc_type2, &version2, &content2,
                )
                .await
                {
                    tracing::warn!("Vettorializzazione documento fallita: {e}");
                }
            });

            format_json(&json!({
                "ok": true,
                "document_id": doc_id.to_string(),
                "file_path": relative_path,
                "title": final_title,
                "version": version,
                "page_count": page_count,
                "section_count": section_count,
                "message": format!("Documento '{}' v{} generato in {}", final_title, version, relative_path)
            }))
        }
        Err(e) => format!("[Errore] Generazione documento fallita: {e}"),
    }
}

pub(super) async fn handle_doc_update(db: &PgPool, _project_id: Uuid, args: &Value) -> String {
    let doc_id = match parse_uuid(args, "document_id") {
        Ok(u) => u,
        Err(e) => return e,
    };

    let sections = match args.get("sections").and_then(Value::as_array) {
        Some(s) => s.clone(),
        None => return "[Errore] Parametro 'sections' obbligatorio (array)".to_string(),
    };

    let bump = args.get("bump").and_then(Value::as_str).unwrap_or("patch");

    // Carica il documento esistente
    let row = sqlx::query(
        "SELECT id, version, file_path, structure_json, title FROM project_documents WHERE id = $1",
    )
    .bind(doc_id)
    .fetch_optional(db)
    .await;

    let row = match row {
        Ok(Some(r)) => r,
        _ => return "[Errore] Documento non trovato".to_string(),
    };

    let old_version: String = row
        .try_get("version")
        .unwrap_or_else(|_| "1.0.0".to_string());
    let old_file_path: String = row.try_get("file_path").unwrap_or_default();
    let _title: String = row.try_get("title").unwrap_or_default();
    let mut structure: Value = row.try_get("structure_json").unwrap_or(json!({}));

    let new_version = bump_version(&old_version, bump);

    // Salva history versione precedente
    let _ = sqlx::query(
        "INSERT INTO project_document_versions (document_id, version, file_path, change_summary, changed_sections)
         VALUES ($1, $2, $3, $4, $5)"
    )
    .bind(doc_id)
    .bind(&old_version)
    .bind(&old_file_path)
    .bind(format!("Aggiornamento a v{}", new_version))
    .bind(sections.iter().filter_map(|s| s.get("number").and_then(Value::as_str)).map(String::from).collect::<Vec<_>>())
    .execute(db)
    .await;

    // Merge delle sezioni aggiornate nella struttura
    if let Some(existing_sections) = structure.get_mut("sections").and_then(Value::as_array_mut) {
        for update in &sections {
            let num = update.get("number").and_then(Value::as_str).unwrap_or("");
            if let Some(existing) = existing_sections
                .iter_mut()
                .find(|s| s.get("number").and_then(Value::as_str) == Some(num))
            {
                if let Some(content) = update.get("content") {
                    existing["content"] = content.clone();
                }
                if let Some(title) = update.get("title") {
                    existing["title"] = title.clone();
                }
            }
        }
    }

    // Aggiorna DB
    let _ = sqlx::query(
        "UPDATE project_documents SET version = $1, structure_json = $2, updated_at = NOW() WHERE id = $3"
    )
    .bind(&new_version)
    .bind(&structure)
    .bind(doc_id)
    .execute(db)
    .await;

    format_json(&json!({
        "ok": true,
        "document_id": doc_id.to_string(),
        "old_version": old_version,
        "new_version": new_version,
        "updated_sections": sections.len()
    }))
}

pub(super) async fn handle_doc_list(db: &PgPool, args: &Value) -> String {
    let pid = match parse_uuid(args, "project_id") {
        Ok(u) => u,
        Err(e) => return e,
    };

    let doc_type_filter = args.get("doc_type").and_then(Value::as_str).unwrap_or("");
    let status_filter = args.get("status").and_then(Value::as_str).unwrap_or("");

    let rows = sqlx::query(
        "SELECT id, doc_type, title, version, file_path, status, created_at, updated_at
         FROM project_documents WHERE project_id = $1
         AND ($2 = '' OR doc_type = $2)
         AND ($3 = '' OR status = $3)
         ORDER BY doc_type, updated_at DESC",
    )
    .bind(pid)
    .bind(doc_type_filter)
    .bind(status_filter)
    .fetch_all(db)
    .await;

    match rows {
        Ok(rows) => {
            let docs: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "id": r.try_get::<Uuid, _>("id").map(|u| u.to_string()).unwrap_or_default(),
                        "doc_type": r.try_get::<String, _>("doc_type").unwrap_or_default(),
                        "title": r.try_get::<String, _>("title").unwrap_or_default(),
                        "version": r.try_get::<String, _>("version").unwrap_or_default(),
                        "file_path": r.try_get::<String, _>("file_path").unwrap_or_default(),
                        "status": r.try_get::<String, _>("status").unwrap_or_default(),
                    })
                })
                .collect();
            format_json(&json!({ "documents": docs, "count": docs.len() }))
        }
        Err(e) => format!("[Errore DB] {e}"),
    }
}

pub(super) async fn handle_doc_search(db: &PgPool, project_id: Uuid, args: &Value) -> String {
    let pid = match args.get("project_id").and_then(Value::as_str) {
        Some(s) => match Uuid::parse_str(s) {
            Ok(u) => u,
            Err(_) => project_id,
        },
        None => project_id,
    };
    let query = match args.get("query").and_then(Value::as_str) {
        Some(q) if !q.trim().is_empty() => q.trim().to_string(),
        _ => return "[Errore] Parametro 'query' obbligatorio".to_string(),
    };
    let doc_type = args
        .get("doc_type")
        .and_then(Value::as_str)
        .map(String::from);
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(5) as usize;

    // Embed query
    let neural_url = crate::auth::get_setting(db, "neural_core_url")
        .await
        .unwrap_or_else(|| {
            std::env::var("NEURAL_CORE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:50051".to_string())
        });

    let neural = match crate::orchestrator::NeuralCoreClient::connect(&neural_url).await {
        Ok(c) => c,
        Err(e) => return format!("[Errore] Connessione Neural Core: {e}"),
    };

    let vector = match neural.embed_text("", &query).await {
        Ok(v) => v,
        Err(e) => return format!("[Errore] Embedding: {e}"),
    };

    let results =
        crate::vector_memory::search_doc_points(db, &vector, pid, doc_type.as_deref(), limit).await;
    match results {
        Ok(hits) => {
            let results: Vec<Value> = hits.iter().map(|h| {
                let payload = &h.payload;
                json!({
                    "score": h.score,
                    "doc_type": payload.get("doc_type").and_then(Value::as_str).unwrap_or(""),
                    "section_path": payload.get("section_path").and_then(Value::as_str).unwrap_or(""),
                    "section_title": payload.get("section_title").and_then(Value::as_str).unwrap_or(""),
                    "version": payload.get("version").and_then(Value::as_str).unwrap_or(""),
                    "text_preview": payload.get("text_preview").and_then(Value::as_str).unwrap_or(""),
                })
            }).collect();
            format_json(&json!({ "results": results, "query": query }))
        }
        Err(e) => format!("[Errore] Ricerca vettoriale: {e}"),
    }
}

pub(super) async fn handle_doc_status(db: &PgPool, args: &Value) -> String {
    let doc_id = match parse_uuid(args, "document_id") {
        Ok(u) => u,
        Err(e) => return e,
    };
    let status = match args.get("status").and_then(Value::as_str) {
        Some(s) if ["draft", "review", "approved", "outdated"].contains(&s) => s.to_string(),
        _ => {
            return "[Errore] Parametro 'status' obbligatorio (draft|review|approved|outdated)"
                .to_string()
        }
    };

    match sqlx::query("UPDATE project_documents SET status = $1, updated_at = NOW() WHERE id = $2")
        .bind(&status)
        .bind(doc_id)
        .execute(db)
        .await
    {
        Ok(r) if r.rows_affected() > 0 => {
            format_json(&json!({ "ok": true, "document_id": doc_id.to_string(), "status": status }))
        }
        Ok(_) => "[Errore] Documento non trovato".to_string(),
        Err(e) => format!("[Errore DB] {e}"),
    }
}
