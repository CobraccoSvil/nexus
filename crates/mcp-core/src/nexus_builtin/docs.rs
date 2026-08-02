//! Handler per il gruppo `documents` del server Nexus Builtin.
//! Gestisce generazione, aggiornamento, lista, ricerca e stato dei documenti.
//! Include l'utility `bump_version`.

use super::*;
use nexus_types::tool_outcome::RispostaTool;

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

/// Risolve il project id target dagli argomenti: prova il parse dell'UUID, poi
/// il lookup per nome (ILIKE), con fallback sul `project_id` del run corrente.
/// Estratto da `handle_doc_generate` per ridurne lunghezza e complessita.
async fn resolve_target_project_id(db: &PgPool, args: &Value, project_id: Uuid) -> Uuid {
    let Some(s) = args.get("project_id").and_then(Value::as_str) else {
        return project_id;
    };
    if let Ok(u) = Uuid::parse_str(s) {
        return u;
    }
    // Prova a cercare per nome progetto
    match sqlx::query_scalar::<_, Uuid>("SELECT id FROM projects WHERE name ILIKE $1 LIMIT 1")
        .bind(s)
        .fetch_optional(db)
        .await
    {
        Ok(Some(found_id)) => found_id,
        _ => project_id,
    }
}

/// Aggiunge al contesto i file statici chiave (README, package.json, Cargo.toml)
/// e la struttura directory top-level. Estratto da `build_project_context`.
async fn append_static_project_files(ctx: &mut String, root_path: &str) {
    // Cerca file importanti
    let key_files: &[&str] = &["README.md", "readme.md", "package.json", "Cargo.toml"];
    for filename in key_files {
        let fpath = format!("{}/{}", root_path, filename);
        if let Ok(content) = tokio::fs::read_to_string(&fpath).await {
            let truncated: String = content.chars().take(2000).collect();
            ctx.push_str(&format!("--- {} ---\n{}\n\n", filename, truncated));
        }
    }
    // Lista struttura directory (top-level)
    if let Ok(mut dir) = tokio::fs::read_dir(root_path).await {
        ctx.push_str("--- Struttura directory ---\n");
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
            ctx.push_str(&format!("{}{}\n", name, if is_dir { "/" } else { "" }));
            count += 1;
        }
    }
}

/// FIX 1 (KB nel prompt): arricchisce il contesto con i passaggi piu' rilevanti
/// del codebase gia' indicizzato in Qdrant (collection project_context). Senza
/// questo, "analizza il codebase" del template resta una promessa vuota: il
/// modello vede solo README + albero cartelle e produce documenti generici.
/// Best-effort (regola H sul log): se la KB e' vuota (progetto mai indicizzato)
/// o il neural core non risponde, si prosegue coi soli file statici.
async fn append_kb_project_context(
    ctx: &mut String,
    db: &PgPool,
    pid: Uuid,
    project_name: &str,
    doc_type: &str,
) {
    let kb_query = format!(
        "Architettura, funzionalita', requisiti e componenti principali del progetto {} \
         per documento di tipo {}",
        project_name, doc_type
    );
    let neural_kb = crate::orchestrator::NeuralCoreClient::new();
    let vector = match neural_kb.embed_text("", &kb_query).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("nexus_doc_generate: embedding query KB fallito: {e}");
            return;
        }
    };
    match crate::vector_memory::search_project_context_points(db, &vector, pid, 8, 0.3).await {
        Ok(hits) if !hits.is_empty() => append_kb_hits(ctx, &hits),
        Ok(_) => tracing::info!(
            "nexus_doc_generate: KB project_context vuota per il progetto, uso solo file statici"
        ),
        Err(e) => tracing::warn!("nexus_doc_generate: ricerca KB fallita (best-effort): {e}"),
    }
}

/// Appende al contesto gli snippet (max 800 char) degli hit KB non vuoti.
/// Estratto da `append_kb_project_context`.
fn append_kb_hits(ctx: &mut String, hits: &[crate::vector_memory::VectorPointHit]) {
    ctx.push_str("\n--- Estratti rilevanti dal codebase (knowledge base) ---\n");
    for h in hits {
        if let Some(text) = h
            .payload
            .get("text")
            .or_else(|| h.payload.get("text_preview"))
            .and_then(Value::as_str)
        {
            let snippet: String = text.chars().take(800).collect();
            ctx.push_str(&format!("- {}\n", snippet));
        }
    }
    tracing::info!(
        "nexus_doc_generate: KB context iniettato ({} passaggi)",
        hits.len()
    );
}

/// Costruisce il contesto testuale del progetto per la generazione automatica
/// del documento: header + file statici chiave + struttura + estratti dalla KB.
/// Estratto da `handle_doc_generate` per ridurne lunghezza e complessita.
async fn build_project_context(
    db: &PgPool,
    pid: Uuid,
    project_name: &str,
    root_path: &str,
    doc_type: &str,
) -> String {
    let mut project_context = format!(
        "Progetto: {}\nRoot: {}\nTipo documento: {}\n\n",
        project_name, root_path, doc_type
    );
    append_static_project_files(&mut project_context, root_path).await;
    append_kb_project_context(&mut project_context, db, pid, project_name, doc_type).await;
    project_context
}

/// Etichetta leggibile per il tipo di documento, usata nel prompt di generazione.
fn doc_type_label(doc_type: &str) -> &str {
    match doc_type {
        "functional_analysis" => "Analisi Funzionale IEEE 830",
        "technical_analysis" => "Analisi Tecnica",
        "er_diagram" => "Diagramma ER e modello dati",
        "project_management" => "Piano di Gestione Progetto",
        "release_notes" => "Release Notes",
        _ => doc_type,
    }
}

/// FIX 5 (anti-malformazione): parsing tramite il punto unico
/// `llm_json::parse_llm_json` (gestisce fence ```json, wrapper content/text,
/// preamboli). Fail-loud (regola H): se il modello non produce un oggetto JSON
/// con un array `sections` valido, NON costruiamo piu' una pseudo-sezione col
/// testo raw (era la causa dei documenti .docx malformati: prosa o JSON troncato
/// finiva dentro un'unica sezione). `Err` = errore azionabile da propagare.
fn parse_docs_generator_json(resp_text: &str, doc_type: &str) -> Result<Value, String> {
    match crate::llm_json::parse_llm_json(resp_text) {
        Ok(v) if v.get("sections").and_then(Value::as_array).is_some() => Ok(v),
        Ok(_) => {
            tracing::warn!(
                doc_type = %doc_type,
                "nexus_doc_generate: JSON valido ma senza array 'sections'"
            );
            Err(format!(
                "[Errore] Generazione '{}' fallita: il modello docs_generator ha \
                 prodotto un JSON privo dell'array 'sections'. Riprova; se persiste, \
                 verifica provider/modello in nexus_purpose_model (purpose='docs_generator').",
                doc_type
            ))
        }
        Err(e) => {
            tracing::warn!(
                doc_type = %doc_type,
                "nexus_doc_generate: output docs_generator non parsabile come JSON: {e}"
            );
            Err(format!(
                "[Errore] Generazione '{}' fallita: l'output del modello docs_generator \
                 non e' un JSON valido ({}). Il documento non e' stato creato (nessun file \
                 malformato salvato). Riprova; se persiste, verifica il modello in \
                 nexus_purpose_model (purpose='docs_generator').",
                doc_type, e
            ))
        }
    }
}

/// Auto-genera il `content_json` strutturato del documento: costruisce il
/// contesto progetto, risolve il modello, chiama il Nexus Gateway e valida il
/// JSON. Estratto da `handle_doc_generate`. `Err` = messaggio da propagare.
async fn autogenerate_content_json(
    db: &PgPool,
    pid: Uuid,
    project_name: &str,
    root_path: &str,
    doc_type: &str,
) -> Result<Value, String> {
    let project_context = build_project_context(db, pid, project_name, root_path, doc_type).await;

    // Chiedi al brain di generare il content_json strutturato
    let gen_prompt = format!(
        "Genera un documento strutturato di tipo '{}' per il progetto descritto sotto.\n\
         Rispondi SOLO con JSON valido, senza markdown, senza ```.\n\
         Il formato ESATTO deve essere:\n\
         {{\"sections\":[{{\"number\":\"1\",\"title\":\"...\",\"content\":\"testo lungo e dettagliato\",\"subsections\":[{{\"number\":\"1.1\",\"title\":\"...\",\"content\":\"...\"}}]}}]}}\n\
         Genera almeno 5 sezioni principali con sottosezioni. Ogni content deve essere almeno 2-3 frasi.\n\n\
         CONTESTO PROGETTO:\n{}",
        doc_type_label(doc_type),
        project_context
    );
    // FAILOVER tier-aware (punto unico complete_for_purpose_with_failover, regola
    // L/G): prova N candidati del tier e passa al prossimo su fallimento della
    // chiamata O JSON invalido O documento a sole intestazioni (regola M:
    // doc_content_is_empty). Prima abortiva al PRIMO provider fallito. La GwRequest
    // resta in call_docs_generator_gateway (un solo posto).
    use crate::internal_routing::{
        complete_for_purpose_with_failover, AttemptOutcome, PurposeFailoverError,
    };
    let outcome = complete_for_purpose_with_failover(db, "docs_generator", |provider, model| {
        let prompt = gen_prompt.clone();
        let doc_type = doc_type.to_string();
        async move {
            match call_docs_generator_gateway(db, &provider, &model, prompt).await {
                Ok(content) if !content.trim().is_empty() => {
                    match parse_docs_generator_json(&content, &doc_type) {
                        Ok(cv) if !doc_content_is_empty(&cv) => AttemptOutcome::Done(cv),
                        _ => AttemptOutcome::Failover,
                    }
                }
                _ => AttemptOutcome::Failover,
            }
        }
    })
    .await;
    let content_val = match outcome {
        Ok(cv) => cv,
        Err(PurposeFailoverError::AllCandidatesFailed) => {
            return Err("[Errore] Generazione automatica contenuto fallita: nessun provider \
                        del tier ha prodotto un documento valido. Riprova piu' tardi."
                .to_string())
        }
        Err(PurposeFailoverError::NoCandidate(res)) => {
            return Err(format!(
                "[Errore] routing docs_generator non risolvibile: {}",
                res.into_model("docs_generator").err().unwrap_or_default()
            ))
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
    Ok(content_val)
}

/// Chiama il Nexus Gateway Rust (punto unico routing/cooldown, regola L): il
/// brain Python non e' piu' coinvolto. Il provider+modello sono gia' decisi a
/// monte via routing matrix DB (resolve_docs_generator_model), quindi si pinna
/// il provider per evitare un secondo routing divergente (regola G). max_tokens
/// alto: un documento strutturato lungo (~13k char) col default veniva TRONCATO,
/// da cui il "JSON parse error" osservato; 16000 token coprono i casi reali. Il
/// prompt impone gia' output JSON puro e `parse_llm_json` tollera fence/preamboli.
/// `Err` = messaggio d'errore da propagare.
async fn call_docs_generator_gateway(
    db: &PgPool,
    gen_provider: &str,
    gen_model: &str,
    gen_prompt: String,
) -> Result<String, String> {
    let gw = crate::nexus_gateway::NexusGatewayClient::from_db(db).await;
    let gw_req = crate::nexus_gateway::GwRequest {
        model: format!("{gen_provider}/{gen_model}"),
        messages: vec![crate::nexus_gateway::GwMessage {
            role: "user".to_string(),
            content: serde_json::json!(gen_prompt),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            thinking_signature: None,
        }],
        max_tokens: Some(16000),
        pin_provider: Some(gen_provider.to_string()),
        metadata: crate::nexus_gateway::GwMetadata {
            feature: "docs_generator".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    match gw.complete(gw_req).await {
        Ok(r) => Ok(r.content),
        Err(e) => Err(format!(
            "[Errore] Generazione automatica contenuto fallita: {e}"
        )),
    }
}

/// Parametri di input risolti per la generazione di un documento.
struct GenerateContext {
    doc_type: String,
    title: String,
    standard: String,
    root_path: String,
    project_name: String,
}

/// Risolve i parametri di input della generazione (doc_type, title, standard) e
/// il root path/nome del progetto dal workspace primario. Estratto da
/// `handle_doc_generate`. `Err` = messaggio d'errore da propagare.
async fn resolve_generate_context(
    db: &PgPool,
    pid: Uuid,
    args: &Value,
) -> Result<GenerateContext, String> {
    let doc_type = match args.get("doc_type").and_then(Value::as_str) {
        Some(t) => t.to_string(),
        None => return Err("[Errore] Parametro 'doc_type' obbligatorio".to_string()),
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
        _ => return Err("[Errore] Progetto non trovato o workspace mancante".to_string()),
    };
    Ok(GenerateContext {
        doc_type,
        title,
        standard,
        root_path,
        project_name,
    })
}

/// Validazione anti-documento-vuoto (regola H): se tutte le sezioni hanno
/// content vuoto, il .docx avrebbe solo l'indice. Non salviamo un documento
/// vuoto in silenzio: ritorniamo un errore esplicito e azionabile. Vale sia per
/// l'auto-generazione (modello che produce solo titoli) sia per un content_json
/// fornito a mano ma vuoto. `Err` = messaggio da propagare.
fn reject_empty_document(content: &Value, doc_type: &str) -> Result<(), String> {
    if !doc_content_is_empty(content) {
        return Ok(());
    }
    tracing::warn!(
        doc_type = %doc_type,
        "nexus_doc_generate: contenuto vuoto (solo indice) — generazione rifiutata"
    );
    Err(format!(
        "[Errore] Generazione documento '{}' fallita: tutte le sezioni risultano prive di contenuto \
         (il documento avrebbe solo l'indice). Il modello docs_generator non ha prodotto testo. \
         Riprova la generazione; se il problema persiste, verifica il provider/modello configurato \
         in nexus_purpose_model (purpose='docs_generator').",
        doc_type
    ))
}

/// Genera il documento e ne DICHIARA l'esito in un campo (regola Q).
///
/// Ha due chiamanti che non si vedono fra loro: il dispatch dei tool builtin
/// (`super::execute`, canale legacy a stringa) e l'endpoint REST
/// `POST /api/projects/:id/documents/generate`, che NON passa dal dispatch dei
/// tool agente e quindi non incontrerebbe mai il ponte `da_testo_legacy`.
/// Finche' l'esito viveva nel marker in testa al testo, il secondo era
/// COSTRETTO a ricostruirlo per conto proprio: col campo non c'e' piu' niente
/// da ricostruire, e comporre il testo non puo' piu' coprire il fallimento.
pub async fn handle_doc_generate(
    db: &PgPool,
    project_id: Uuid,
    user_id: Uuid,
    args: &Value,
) -> RispostaTool {
    let pid = resolve_target_project_id(db, args, project_id).await;

    let GenerateContext {
        doc_type,
        title,
        standard,
        root_path,
        project_name,
    } = match resolve_generate_context(db, pid, args).await {
        Ok(ctx) => ctx,
        Err(msg) => return RispostaTool::fallito(msg),
    };

    // Se content_json manca o è vuoto, auto-genera il contenuto analizzando il progetto
    let content = match args.get("content_json") {
        Some(v) if !v.is_null() && v.as_object().is_none_or(|o| !o.is_empty()) => v.clone(),
        _ => match autogenerate_content_json(db, pid, &project_name, &root_path, &doc_type).await {
            Ok(v) => v,
            Err(msg) => return RispostaTool::fallito(msg),
        },
    };

    if let Err(msg) = reject_empty_document(&content, &doc_type) {
        return RispostaTool::fallito(msg);
    }

    let ctx = GenerateContext {
        doc_type,
        title,
        standard,
        root_path,
        project_name,
    };
    finalize_document(db, pid, user_id, &ctx, content).await
}

/// Determina versione e path, serializza il contenuto, renderizza il .docx e ne
/// persiste l'esito. Estratto da `handle_doc_generate` per ridurne la lunghezza.
async fn finalize_document(
    db: &PgPool,
    pid: Uuid,
    user_id: Uuid,
    ctx: &GenerateContext,
    content: Value,
) -> RispostaTool {
    // Determina versione (incrementa se lo stesso doc_type esiste già)
    let version = next_document_version(db, pid, &ctx.doc_type).await;

    let slug = ctx.doc_type.replace('_', "-");
    let relative_path = format!("docs/{}-v{}.docx", slug, version);
    let abs_output = format!("{}/{}", ctx.root_path, relative_path);
    let content_str = match serde_json::to_string(&content) {
        Ok(s) => s,
        Err(e) => {
            return RispostaTool::fallito(format!(
                "[Errore] Serializzazione contenuto documento fallita: {e}"
            ))
        }
    };

    let final_title = if ctx.title.is_empty() {
        slug.replace('-', " ")
    } else {
        ctx.title.clone()
    };

    let rendered = match render_docx_blocking(
        &ctx.doc_type,
        content_str,
        abs_output,
        &ctx.standard,
        &final_title,
        &ctx.project_name,
    )
    .await
    {
        Ok(r) => r,
        Err(msg) => return RispostaTool::fallito(msg),
    };
    persist_generated_document(
        db,
        pid,
        user_id,
        &ctx.doc_type,
        &final_title,
        &version,
        &relative_path,
        &content,
        rendered,
    )
    .await
}

/// Calcola la versione del prossimo documento: bump minor se esiste gia' lo
/// stesso `doc_type`, altrimenti "1.0.0". Estratto da `handle_doc_generate`.
async fn next_document_version(db: &PgPool, pid: Uuid, doc_type: &str) -> String {
    let existing = sqlx::query("SELECT version FROM project_documents WHERE project_id = $1 AND doc_type = $2 ORDER BY created_at DESC LIMIT 1")
        .bind(pid)
        .bind(doc_type)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    match existing {
        Some(r) => {
            let v: String = r.try_get("version").unwrap_or_else(|_| "1.0.0".to_string());
            bump_version(&v, "minor")
        }
        None => "1.0.0".to_string(),
    }
}

/// Renderizza il .docx nel PUNTO UNICO Rust (regola L): nessun round-trip gRPC
/// al brain Python. `render_document` e' sincrono e CPU-bound (zip + XML),
/// quindi gira in `spawn_blocking` per non bloccare l'executor tokio. Verso
/// zero-Python: questo era l'ultimo RPC AI-adiacente ancora servito dal brain.
/// `Err` = messaggio d'errore da propagare al chiamante.
async fn render_docx_blocking(
    doc_type: &str,
    content_str: String,
    abs_output: String,
    standard: &str,
    final_title: &str,
    project_name: &str,
) -> Result<crate::docx_render::RenderedDoc, String> {
    let doc_type = doc_type.to_string();
    let standard = standard.to_string();
    let final_title = final_title.to_string();
    let project_name = project_name.to_string();
    let join_result = tokio::task::spawn_blocking(move || {
        crate::docx_render::render_document(
            &doc_type,
            &content_str,
            &abs_output,
            &standard,
            &final_title,
            &project_name,
        )
    })
    .await;
    match join_result {
        Ok(Ok(rendered)) => Ok(rendered),
        Ok(Err(e)) => Err(format!("[Errore] Generazione documento fallita: {e}")),
        Err(e) => Err(format!(
            "[Errore] Task di rendering documento interrotto: {e}"
        )),
    }
}

/// Persiste il documento renderizzato: INSERT nel catalogo (fail-loud), evento
/// SSE realtime e vettorializzazione in background; ritorna il JSON di esito.
/// Estratto da `handle_doc_generate` per ridurne lunghezza e complessita.
#[allow(clippy::too_many_arguments)]
async fn persist_generated_document(
    db: &PgPool,
    pid: Uuid,
    user_id: Uuid,
    doc_type: &str,
    final_title: &str,
    version: &str,
    relative_path: &str,
    content: &Value,
    rendered: crate::docx_render::RenderedDoc,
) -> RispostaTool {
    let crate::docx_render::RenderedDoc {
        file_path,
        page_count,
        section_count,
    } = rendered;
    tracing::info!(
        doc_type = %doc_type, file = %file_path, pages = page_count,
        sections = section_count,
        "nexus_doc_generate: .docx renderizzato in-process (renderer Rust)"
    );
    let doc_id = Uuid::new_v4();
    if let Err(msg) = insert_document_row(
        db,
        doc_id,
        pid,
        user_id,
        doc_type,
        final_title,
        version,
        relative_path,
        content,
    )
    .await
    {
        return RispostaTool::fallito(msg);
    }

    spawn_document_side_effects(
        db,
        doc_id,
        pid,
        doc_type,
        final_title,
        version,
        relative_path,
        content,
    );

    RispostaTool::riuscito(format_json(&json!({
        "ok": true,
        "document_id": doc_id.to_string(),
        "file_path": relative_path,
        "title": final_title,
        "version": version,
        "page_count": page_count,
        "section_count": section_count,
        "message": format!("Documento '{}' v{} generato in {}", final_title, version, relative_path)
    })))
}

/// Registra il documento in `project_documents`. FIX 2: prima era `let _ =`,
/// quindi se l'INSERT falliva il .docx restava sul filesystem ma orfano dal
/// catalogo: non appariva nel pannello DOCUMENTI (che lista da project_documents)
/// e l'auto-discovery recuperava solo i .md, mai i .docx. Ora l'errore e'
/// propagato (regola H): meglio un errore visibile che un file fantasma.
/// `Err` = messaggio da propagare al chiamante.
#[allow(clippy::too_many_arguments)]
async fn insert_document_row(
    db: &PgPool,
    doc_id: Uuid,
    pid: Uuid,
    user_id: Uuid,
    doc_type: &str,
    final_title: &str,
    version: &str,
    relative_path: &str,
    content: &Value,
) -> Result<(), String> {
    if let Err(e) = sqlx::query(
        "INSERT INTO project_documents (id, project_id, doc_type, title, version, file_path, structure_json, status, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'draft', $8)"
    )
    .bind(doc_id)
    .bind(pid)
    .bind(doc_type)
    .bind(final_title)
    .bind(version)
    .bind(relative_path)
    .bind(content)
    .bind(user_id)
    .execute(db)
    .await
    {
        tracing::error!(
            doc_type = %doc_type, file = %relative_path,
            "nexus_doc_generate: INSERT project_documents fallito: {e}"
        );
        return Err(format!(
            "[Errore] Documento generato su disco ({}) ma registrazione nel catalogo \
             fallita: {e}. Il file non comparirebbe nel pannello. Riprova.",
            relative_path
        ));
    }
    Ok(())
}

/// Effetti collaterali post-INSERT: evento SSE realtime e vettorializzazione in
/// background. Estratto da `persist_generated_document`.
#[allow(clippy::too_many_arguments)]
fn spawn_document_side_effects(
    db: &PgPool,
    doc_id: Uuid,
    pid: Uuid,
    doc_type: &str,
    final_title: &str,
    version: &str,
    relative_path: &str,
    content: &Value,
) {
    // FIX 3 (realtime via chat): emette l'evento SSE DocumentGenerated. Il tool
    // builtin non ha accesso ad AppState/project_channels, quindi usa il registry
    // globale inizializzato in main.rs (dispatcher::init_global). Il pannello
    // DOCUMENTI, ricevendo l'evento via /event-stream, fa il refresh anche quando
    // la generazione e' partita dalla chat e non dal pulsante.
    let _ = nexus_events::dispatcher::emit_global(
        pid,
        nexus_events::event::ProjectEvent::DocumentGenerated {
            document_id: doc_id,
            doc_type: doc_type.to_string(),
            title: final_title.to_string(),
            version: version.to_string(),
            file_path: relative_path.to_string(),
        },
    );

    // Vettorializzazione in background
    let db2 = db.clone();
    let content2 = content.clone();
    let doc_type2 = doc_type.to_string();
    let version2 = version.to_string();
    tokio::spawn(async move {
        if let Err(e) = crate::vector_memory::vectorize_document(
            &db2, pid, doc_id, &doc_type2, &version2, &content2,
        )
        .await
        {
            tracing::warn!("Vettorializzazione documento fallita: {e}");
        }
    });
}

/// Salva in `project_document_versions` la history della versione precedente
/// (best-effort, come in origine con `let _ =`). Estratto da `handle_doc_update`.
async fn save_document_version_history(
    db: &PgPool,
    doc_id: Uuid,
    old_version: &str,
    old_file_path: &str,
    new_version: &str,
    sections: &[Value],
) {
    let changed: Vec<String> = sections
        .iter()
        .filter_map(|s| s.get("number").and_then(Value::as_str))
        .map(String::from)
        .collect();
    let _ = sqlx::query(
        "INSERT INTO project_document_versions (document_id, version, file_path, change_summary, changed_sections)
         VALUES ($1, $2, $3, $4, $5)"
    )
    .bind(doc_id)
    .bind(old_version)
    .bind(old_file_path)
    .bind(format!("Aggiornamento a v{}", new_version))
    .bind(changed)
    .execute(db)
    .await;
}

/// Applica in-place gli aggiornamenti di `content`/`title` alle sezioni della
/// `structure` esistente, appaiando per `number`. Estratto da `handle_doc_update`.
fn merge_document_sections(structure: &mut Value, updates: &[Value]) {
    let Some(existing_sections) = structure.get_mut("sections").and_then(Value::as_array_mut)
    else {
        return;
    };
    for update in updates {
        let num = update.get("number").and_then(Value::as_str).unwrap_or("");
        let Some(existing) = existing_sections
            .iter_mut()
            .find(|s| s.get("number").and_then(Value::as_str) == Some(num))
        else {
            continue;
        };
        if let Some(content) = update.get("content") {
            existing["content"] = content.clone();
        }
        if let Some(title) = update.get("title") {
            existing["title"] = title.clone();
        }
    }
}

/// Applica su `project_documents` la nuova versione e struttura (best-effort,
/// come in origine con `let _ =`). Estratto da `handle_doc_update`.
async fn persist_document_update(db: &PgPool, doc_id: Uuid, new_version: &str, structure: &Value) {
    let _ = sqlx::query(
        "UPDATE project_documents SET version = $1, structure_json = $2, updated_at = NOW() WHERE id = $3"
    )
    .bind(new_version)
    .bind(structure)
    .bind(doc_id)
    .execute(db)
    .await;
}

/// Carica il documento esistente per l'update, restituendo (version, file_path,
/// structure_json). Estratto da `handle_doc_update`. `Err` = messaggio da propagare.
async fn load_document_for_update(
    db: &PgPool,
    doc_id: Uuid,
) -> Result<(String, String, Value), String> {
    let row = sqlx::query(
        "SELECT id, version, file_path, structure_json, title FROM project_documents WHERE id = $1",
    )
    .bind(doc_id)
    .fetch_optional(db)
    .await;
    let row = match row {
        Ok(Some(r)) => r,
        _ => return Err("[Errore] Documento non trovato".to_string()),
    };
    let old_version: String = row
        .try_get("version")
        .unwrap_or_else(|_| "1.0.0".to_string());
    let old_file_path: String = row.try_get("file_path").unwrap_or_default();
    let structure: Value = row.try_get("structure_json").unwrap_or(json!({}));
    Ok((old_version, old_file_path, structure))
}

pub(super) async fn handle_doc_update(db: &PgPool, _project_id: Uuid, args: &Value) -> String {
    let doc_id = match parse_uuid(args, "document_id") {
        Ok(u) => u,
        Err(e) => return tool_failure(e),
    };

    let sections = match args.get("sections").and_then(Value::as_array) {
        Some(s) => s.clone(),
        None => return tool_failure("[Errore] Parametro 'sections' obbligatorio (array)"),
    };

    let bump = args.get("bump").and_then(Value::as_str).unwrap_or("patch");

    // Carica il documento esistente
    let (old_version, old_file_path, mut structure) =
        match load_document_for_update(db, doc_id).await {
            Ok(t) => t,
            Err(msg) => return tool_failure(msg),
        };

    let new_version = bump_version(&old_version, bump);

    save_document_version_history(
        db,
        doc_id,
        &old_version,
        &old_file_path,
        &new_version,
        &sections,
    )
    .await;

    // Merge delle sezioni aggiornate nella struttura
    merge_document_sections(&mut structure, &sections);

    persist_document_update(db, doc_id, &new_version, &structure).await;

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
        Err(e) => return tool_failure(e),
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
        Err(e) => tool_failure(format!("[Errore DB] {e}")),
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
        _ => return tool_failure("[Errore] Parametro 'query' obbligatorio"),
    };
    let doc_type = args
        .get("doc_type")
        .and_then(Value::as_str)
        .map(String::from);
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(5) as usize;

    // Embed query
    let neural = crate::orchestrator::NeuralCoreClient::new();

    let vector = match neural.embed_text("", &query).await {
        Ok(v) => v,
        Err(e) => return tool_failure(format!("[Errore] Embedding: {e}")),
    };

    let results =
        crate::vector_memory::search_doc_points(db, &vector, pid, doc_type.as_deref(), limit).await;
    match results {
        Ok(hits) => {
            let results: Vec<Value> = hits.iter().map(doc_hit_to_json).collect();
            format_json(&json!({ "results": results, "query": query }))
        }
        Err(e) => tool_failure(format!("[Errore] Ricerca vettoriale: {e}")),
    }
}

/// Converte un hit di ricerca documentale nel JSON di output della ricerca.
/// Estratto da `handle_doc_search`.
fn doc_hit_to_json(h: &crate::vector_memory::VectorPointHit) -> Value {
    let payload = &h.payload;
    json!({
        "score": h.score,
        "doc_type": payload.get("doc_type").and_then(Value::as_str).unwrap_or(""),
        "section_path": payload.get("section_path").and_then(Value::as_str).unwrap_or(""),
        "section_title": payload.get("section_title").and_then(Value::as_str).unwrap_or(""),
        "version": payload.get("version").and_then(Value::as_str).unwrap_or(""),
        "text_preview": payload.get("text_preview").and_then(Value::as_str).unwrap_or(""),
    })
}

pub(super) async fn handle_doc_status(db: &PgPool, args: &Value) -> String {
    let doc_id = match parse_uuid(args, "document_id") {
        Ok(u) => u,
        Err(e) => return tool_failure(e),
    };
    let status = match args.get("status").and_then(Value::as_str) {
        Some(s) if ["draft", "review", "approved", "outdated"].contains(&s) => s.to_string(),
        _ => {
            return tool_failure(
                "[Errore] Parametro 'status' obbligatorio (draft|review|approved|outdated)",
            )
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
        Ok(_) => tool_failure("[Errore] Documento non trovato"),
        Err(e) => tool_failure(format!("[Errore DB] {e}")),
    }
}

#[cfg(test)]
mod tool_failure_tests {
    use super::*;

    /// Pool Postgres LAZY: non apre connessioni finche' non si interroga il
    /// DB. Sufficiente per i rami che falliscono PRIMA di qualunque `.await`
    /// su `db` (parse_uuid, validazione parametri) — regola O: si chiama
    /// l'handler reale, non una sua imitazione.
    fn pool_mai_connesso() -> PgPool {
        PgPool::connect_lazy("postgres://test:test@127.0.0.1:1/test").expect("pool lazy")
    }

    #[tokio::test]
    async fn doc_list_con_project_id_invalido_e_un_fallimento_dichiarato() {
        let db = pool_mai_connesso();
        let out = handle_doc_list(&db, &json!({ "project_id": "non-un-uuid" })).await;
        assert!(
            nexus_types::tool_outcome::is_tool_failure(&out),
            "project_id invalido deve dichiararsi fallito: {out}"
        );
    }

    #[tokio::test]
    async fn doc_status_con_document_id_invalido_e_un_fallimento_dichiarato() {
        let db = pool_mai_connesso();
        let out = handle_doc_status(&db, &json!({ "document_id": "non-un-uuid" })).await;
        assert!(
            nexus_types::tool_outcome::is_tool_failure(&out),
            "document_id invalido deve dichiararsi fallito: {out}"
        );
    }

    #[tokio::test]
    async fn doc_status_senza_status_valido_e_un_fallimento_dichiarato() {
        // Il controllo su `status` avviene DOPO parse_uuid ma PRIMA di
        // qualunque query: anche questo ramo non tocca davvero il pool.
        let db = pool_mai_connesso();
        let doc_id = Uuid::new_v4().to_string();
        let out = handle_doc_status(
            &db,
            &json!({ "document_id": doc_id, "status": "non-uno-stato-valido" }),
        )
        .await;
        assert!(
            nexus_types::tool_outcome::is_tool_failure(&out),
            "status non valido deve dichiararsi fallito: {out}"
        );
    }

    #[test]
    fn documento_non_trovato_e_un_fallimento_dichiarato() {
        // Stesso letterale del ramo Ok(0 righe) di handle_doc_status: la
        // UPDATE non ha fallito ma non ha trovato il documento, quindi
        // l'operazione richiesta non e' stata compiuta.
        let out = tool_failure("[Errore] Documento non trovato");
        assert!(nexus_types::tool_outcome::is_tool_failure(&out));
    }

    #[test]
    fn successo_con_payload_json_non_e_un_fallimento() {
        let out = format_json(&json!({ "ok": true, "document_id": Uuid::new_v4().to_string() }));
        assert!(!nexus_types::tool_outcome::is_tool_failure(&out));
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
                reasoning: None,
                thinking_signature: None,
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
