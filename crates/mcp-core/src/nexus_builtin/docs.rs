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

pub async fn handle_doc_generate(
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
        Some(v) if !v.is_null() && v.as_object().is_none_or(|o| !o.is_empty()) => v.clone(),
        _ => {
            // Raccogli informazioni sul progetto per generare il contenuto automaticamente
            tracing::info!(
                "nexus_doc_generate: content_json mancante, auto-generazione per {}",
                doc_type
            );
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
                    let is_dir = entry.file_type().await.is_ok_and(|t| t.is_dir());
                    project_context.push_str(&format!(
                        "{}{}\n",
                        name,
                        if is_dir { "/" } else { "" }
                    ));
                    count += 1;
                }
            }
            // FIX 1 (KB nel prompt): arricchisci il contesto con i passaggi piu'
            // rilevanti del codebase gia' indicizzato in Qdrant (collection
            // project_context). Senza questo, "analizza il codebase" del template
            // resta una promessa vuota: il modello vede solo README + albero
            // cartelle e produce documenti generici. Best-effort (regola H sul
            // log): se la KB e' vuota (progetto mai indicizzato) o il neural core
            // non risponde, si prosegue coi soli file statici.
            let neural_url_kb = crate::auth::get_setting(db, "neural_core_url")
                .await
                .unwrap_or_else(|| {
                    std::env::var("NEURAL_CORE_URL")
                        .unwrap_or_else(|_| "http://127.0.0.1:50051".to_string())
                });
            let kb_query = format!(
                "Architettura, funzionalita', requisiti e componenti principali del progetto {} \
                 per documento di tipo {}",
                project_name, doc_type
            );
            match crate::orchestrator::NeuralCoreClient::connect(&neural_url_kb).await {
                Ok(neural_kb) => match neural_kb.embed_text("", &kb_query).await {
                    Ok(vector) => {
                        match crate::vector_memory::search_project_context_points(
                            db, &vector, pid, 8, 0.3,
                        )
                        .await
                        {
                            Ok(hits) if !hits.is_empty() => {
                                project_context.push_str(
                                    "\n--- Estratti rilevanti dal codebase (knowledge base) ---\n",
                                );
                                for h in &hits {
                                    if let Some(text) = h
                                        .payload
                                        .get("text")
                                        .or_else(|| h.payload.get("text_preview"))
                                        .and_then(Value::as_str)
                                    {
                                        let snippet: String = text.chars().take(800).collect();
                                        project_context.push_str(&format!("- {}\n", snippet));
                                    }
                                }
                                tracing::info!(
                                    "nexus_doc_generate: KB context iniettato ({} passaggi)",
                                    hits.len()
                                );
                            }
                            Ok(_) => tracing::info!(
                                "nexus_doc_generate: KB project_context vuota per il progetto, \
                                 uso solo file statici"
                            ),
                            Err(e) => tracing::warn!(
                                "nexus_doc_generate: ricerca KB fallita (best-effort): {e}"
                            ),
                        }
                    }
                    Err(e) => {
                        tracing::warn!("nexus_doc_generate: embedding query KB fallito: {e}")
                    }
                },
                Err(e) => tracing::warn!(
                    "nexus_doc_generate: connessione neural core per KB fallita: {e}"
                ),
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
            // Modello risolto tramite il ROUTING CANONICO per tier (punto unico
            // regola L): resolve_purpose_model_db applica tier-rule (mig 0203) ->
            // miglior modello del catalog fuori cooldown -> fallback statico ->
            // enforcement cooldown. NIENTE piu' query statica che ignorava il
            // tier e il routing. handle_doc_generate non ha AppState, quindi usa
            // l'adapter `_db` (stessa logica, fonte DB invece della matrix cache).
            let (gen_provider, gen_model): (String, String) =
                match crate::internal_routing::resolve_purpose_model_db(db, "docs_generator").await
                {
                    crate::internal_routing::PurposeResolution::Resolved {
                        provider,
                        model,
                        rationale,
                    } => {
                        tracing::info!(
                            "nexus_doc_generate: modello risolto {provider}/{model} ({rationale})"
                        );
                        (provider, model)
                    }
                    crate::internal_routing::PurposeResolution::NoCapableModel { tier } => {
                        tracing::warn!(
                            "nexus_doc_generate: nessun modello del tier '{tier}' disponibile"
                        );
                        return format!(
                            "[Errore] Generazione documento non disponibile: nessun modello del \
                             tier '{tier}' (purpose docs_generator) e' disponibile (capability \
                             mancante o provider in cooldown). Riprova piu' tardi."
                        );
                    }
                    crate::internal_routing::PurposeResolution::NotFound => {
                        tracing::error!(
                            "purpose 'docs_generator' non configurato o privo di tier."
                        );
                        return "[Errore] purpose 'docs_generator' non configurato (o privo di tier) in nexus_purpose_model".to_string();
                    }
                    crate::internal_routing::PurposeResolution::MatrixUnavailable(e) => {
                        tracing::error!("nexus_doc_generate: routing non disponibile: {e}");
                        return format!("[Errore] routing docs_generator non disponibile: {e}");
                    }
                };
            // Chiamata LLM al Nexus Gateway Rust (punto unico routing/cooldown,
            // regola L): il brain Python non e' piu' coinvolto. Il provider+modello
            // sono gia' decisi a monte via routing matrix DB (resolve_purpose_model_db
            // sopra), quindi si pinna il provider per evitare un secondo routing
            // divergente (regola G). max_tokens alto: un documento strutturato lungo
            // (~13k char) col default veniva TRONCATO, da cui il "JSON parse error"
            // osservato; 16000 token coprono i casi reali. Il prompt impone gia'
            // output JSON puro e `parse_llm_json` sotto tollera fence/preamboli.
            let gw = crate::nexus_gateway::NexusGatewayClient::from_db(db).await;
            let gw_req = crate::nexus_gateway::GwRequest {
                model: format!("{gen_provider}/{gen_model}"),
                messages: vec![crate::nexus_gateway::GwMessage {
                    role: "user".to_string(),
                    content: serde_json::json!(gen_prompt),
                    tool_calls: None,
                    tool_call_id: None,
                }],
                max_tokens: Some(16000),
                pin_provider: Some(gen_provider.clone()),
                metadata: crate::nexus_gateway::GwMetadata {
                    feature: "docs_generator".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            };
            let resp_text = match gw.complete(gw_req).await {
                Ok(r) => r.content,
                Err(e) => {
                    return format!("[Errore] Generazione automatica contenuto fallita: {e}")
                }
            };
            // FIX 5 (anti-malformazione): parsing tramite il punto unico
            // `llm_json::parse_llm_json` (gestisce fence ```json, wrapper
            // content/text, preamboli). Fail-loud (regola H): se il modello non
            // produce un oggetto JSON con un array `sections` valido, NON
            // costruiamo piu' una pseudo-sezione col testo raw (era la causa dei
            // documenti .docx malformati: prosa o JSON troncato finiva dentro
            // un'unica sezione). Ritorniamo un errore azionabile.
            let content_val: Value = match crate::llm_json::parse_llm_json(&resp_text) {
                Ok(v) if v.get("sections").and_then(Value::as_array).is_some() => v,
                Ok(_) => {
                    tracing::warn!(
                        doc_type = %doc_type,
                        "nexus_doc_generate: JSON valido ma senza array 'sections'"
                    );
                    return format!(
                        "[Errore] Generazione '{}' fallita: il modello docs_generator ha \
                         prodotto un JSON privo dell'array 'sections'. Riprova; se persiste, \
                         verifica provider/modello in nexus_purpose_model (purpose='docs_generator').",
                        doc_type
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        doc_type = %doc_type,
                        "nexus_doc_generate: output docs_generator non parsabile come JSON: {e}"
                    );
                    return format!(
                        "[Errore] Generazione '{}' fallita: l'output del modello docs_generator \
                         non e' un JSON valido ({}). Il documento non e' stato creato (nessun file \
                         malformato salvato). Riprova; se persiste, verifica il modello in \
                         nexus_purpose_model (purpose='docs_generator').",
                        doc_type, e
                    );
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
    let content_str = match serde_json::to_string(&content) {
        Ok(s) => s,
        Err(e) => return format!("[Errore] Serializzazione contenuto documento fallita: {e}"),
    };

    let final_title = if title.is_empty() {
        slug.replace('-', " ")
    } else {
        title.clone()
    };

    // Rendering .docx nel PUNTO UNICO Rust (regola L): nessun round-trip gRPC al
    // brain Python. `render_document` e' sincrono e CPU-bound (zip + XML), quindi
    // gira in `spawn_blocking` per non bloccare l'executor tokio. Verso zero-Python:
    // questo era l'ultimo RPC AI-adiacente ancora servito dal brain.
    let render_result = {
        let doc_type = doc_type.clone();
        let final_title = final_title.clone();
        let project_name = project_name.clone();
        let standard = standard.clone();
        tokio::task::spawn_blocking(move || {
            crate::docx_render::render_document(
                &doc_type,
                &content_str,
                &abs_output,
                &standard,
                &final_title,
                &project_name,
            )
        })
        .await
    };

    let render_result = match render_result {
        Ok(r) => r,
        Err(e) => return format!("[Errore] Task di rendering documento interrotto: {e}"),
    };

    match render_result {
        Ok(crate::docx_render::RenderedDoc {
            file_path,
            page_count,
            section_count,
        }) => {
            tracing::info!(
                doc_type = %doc_type, file = %file_path, pages = page_count,
                sections = section_count,
                "nexus_doc_generate: .docx renderizzato in-process (renderer Rust)"
            );
            // Salva nel DB. FIX 2: prima era `let _ =`, quindi se l'INSERT
            // falliva il .docx restava sul filesystem ma orfano dal catalogo: non
            // appariva nel pannello DOCUMENTI (che lista da project_documents) e
            // l'auto-discovery recuperava solo i .md, mai i .docx. Ora l'errore e'
            // propagato (regola H): meglio un errore visibile che un file fantasma.
            let doc_id = Uuid::new_v4();
            if let Err(e) = sqlx::query(
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
            .await
            {
                tracing::error!(
                    doc_type = %doc_type, file = %relative_path,
                    "nexus_doc_generate: INSERT project_documents fallito: {e}"
                );
                return format!(
                    "[Errore] Documento generato su disco ({}) ma registrazione nel catalogo \
                     fallita: {e}. Il file non comparirebbe nel pannello. Riprova.",
                    relative_path
                );
            }

            // FIX 3 (realtime via chat): emette l'evento SSE DocumentGenerated.
            // Il tool builtin non ha accesso ad AppState/project_channels, quindi
            // usa il registry globale inizializzato in main.rs
            // (dispatcher::init_global). Il pannello DOCUMENTI, ricevendo l'evento
            // via /event-stream, fa il refresh anche quando la generazione e'
            // partita dalla chat e non dal pulsante.
            let _ = nexus_events::dispatcher::emit_global(
                pid,
                nexus_events::event::ProjectEvent::DocumentGenerated {
                    document_id: doc_id,
                    doc_type: doc_type.clone(),
                    title: final_title.clone(),
                    version: version.clone(),
                    file_path: relative_path.clone(),
                },
            );

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

#[cfg(test)]
mod docs_gateway_tests {
    use super::*;

    #[test]
    fn doc_generate_gateway_request_pinna_provider() {
        // La generazione contenuto documenti chiama il gateway pinnando il
        // provider risolto dal purpose docs_generator (regola G/L). Verifica la
        // forma wire della richiesta.
        let provider = "openai";
        let model = "gpt-4.1-nano";
        let prompt = "Genera un documento strutturato JSON".to_string();
        let req = crate::nexus_gateway::GwRequest {
            model: format!("{provider}/{model}"),
            messages: vec![crate::nexus_gateway::GwMessage {
                role: "user".to_string(),
                content: json!(prompt),
                tool_calls: None,
                tool_call_id: None,
            }],
            max_tokens: Some(16000),
            pin_provider: Some(provider.to_string()),
            metadata: crate::nexus_gateway::GwMetadata {
                feature: "docs_generator".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let wire = serde_json::to_value(&req).expect("serializza GwRequest");
        assert_eq!(wire["model"], "openai/gpt-4.1-nano");
        assert_eq!(wire["pin_provider"], "openai");
        assert_eq!(wire["max_tokens"], 16000);
        assert_eq!(wire["metadata"]["feature"], "docs_generator");
        // Niente tool: e' una generazione testuale JSON.
        assert!(wire.get("tools").is_none());
    }
}
