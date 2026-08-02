use axum::{
    body::Body,
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use nexus_types::documents_dto::{
    delete_document_db, document_row_to_json, docx_attachment_response, fetch_document_file_path,
    fetch_document_row, fetch_project_documents, fetch_versions, parse_document_id,
};
use serde_json::{json, Value};
use tokio::fs;

use crate::{
    auth::Claims,
    chat_learning::{api_error, parse_project_id, parse_user_id, ApiError, ApiResult},
    projects::load_project_context,
    AppState,
};

/// GET /api/projects/:id/documents
///
/// Restituisce la lista documenti dal DB project_documents arricchita
/// con auto-discovery dei .md presenti in `docs/` ma non ancora catalogati.
/// I file orfani vengono inseriti nel DB con status='draft' e doc_type
/// inferito dal nome (technical_analysis, functional_analysis, ecc.) per
/// essere visibili nel pannello DOCUMENTI anche se generati da write_file.
pub async fn list_documents(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = parse_project_id(&id)?;

    // Auto-discovery: scansiona la cartella docs/ e auto-registra i file
    // orfani (presenti sul filesystem ma assenti dal DB).
    // FIX 2: include i .docx oltre ai .md. Il flusso canonico
    // (nexus_doc_generate) salva .docx; prima la discovery li ignorava, quindi
    // un .docx orfano (es. INSERT mai avvenuto) non veniva mai recuperato.
    if let Ok(ctx) = load_project_context(&state.db, project_id, user_id).await {
        let docs_dir = ctx.root_path.join("docs");
        if let Ok(mut entries) = fs::read_dir(&docs_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                    continue;
                };
                if !matches!(ext.to_ascii_lowercase().as_str(), "md" | "docx") {
                    continue;
                }
                let Some(file_name) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                // FIX duplicati: normalizza al path RELATIVO alla project root,
                // cosi' coincide col formato salvato da nexus_doc_generate
                // (es. "docs/functional-analysis-v1.0.0.docx"). Prima usavamo
                // path.to_string_lossy() (assoluto) e la guardia NOT EXISTS non
                // matchava il record canonico -> doppio INSERT. La UNIQUE
                // constraint introdotta in mig 0348 fa da rete di sicurezza.
                let relative_path = path
                    .strip_prefix(&ctx.root_path)
                    .ok()
                    .and_then(|p| p.to_str())
                    .map(|s| s.replace('\\', "/"))
                    .unwrap_or_else(|| path.to_string_lossy().to_string());

                // Inferisci doc_type dal nome file.
                let doc_type = infer_doc_type(file_name);
                let title = humanize_filename(file_name);

                // INSERT atomico con guardia NOT EXISTS: previene duplicati
                // sotto chiamate concorrenti (React StrictMode invoca l'effect
                // due volte). La UNIQUE constraint (mig 0348) e' la rete finale
                // in caso di drift dei path tra call site.
                let _ = sqlx::query(
                    "INSERT INTO project_documents
                     (project_id, doc_type, title, version, file_path, status, metadata)
                     SELECT $1, $2, $3, '1.0.0', $4, 'draft', $5
                     WHERE NOT EXISTS (
                         SELECT 1 FROM project_documents
                         WHERE project_id = $1 AND file_path = $4
                     )
                     ON CONFLICT (project_id, file_path) DO NOTHING",
                )
                .bind(project_id)
                .bind(&doc_type)
                .bind(&title)
                .bind(&relative_path)
                .bind(json!({ "source": "auto_discovery", "discovered_at": chrono::Utc::now().to_rfc3339() }))
                .execute(&state.db)
                .await;
            }
        }
    }

    // Query + mapping nel punto unico documents_dto (regola L, cluster E4).
    let docs = fetch_project_documents(&state.db, project_id).await?;

    Ok(Json(json!({ "documents": docs })))
}

/// Inferisce il doc_type dal nome del file. Default: technical_analysis
/// (compatibile con il check constraint del DB).
fn infer_doc_type(file_stem: &str) -> String {
    let lower = file_stem.to_lowercase();
    if lower.contains("funzionale") || lower.contains("functional") {
        "functional_analysis".to_string()
    } else if lower.contains("er") && (lower.contains("diagram") || lower.contains("model")) {
        "er_diagram".to_string()
    } else if lower.contains("project_management")
        || lower.contains("gestione")
        || lower.contains("piano")
    {
        "project_management".to_string()
    } else if lower.contains("release") {
        "release_notes".to_string()
    } else {
        // Default: tutto il resto e' technical_analysis (es. README, governance,
        // threat-model, ecc.). Il check constraint del DB vincola a 5 valori.
        "technical_analysis".to_string()
    }
}

/// Trasforma `analisi-tecnica-redemptor` -> `Analisi Tecnica Redemptor`.
fn humanize_filename(file_stem: &str) -> String {
    file_stem
        .replace(['_', '-'], " ")
        .split_whitespace()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().chain(chars).collect(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// GET /api/projects/:id/documents/:doc_id
pub async fn get_document(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, doc_id)): AxumPath<(String, String)>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let _project_id = parse_project_id(&id)?;
    let document_id = parse_document_id(&doc_id)?;

    // Punto unico query + mapping JSON in nexus_types::documents_dto (regola L, S62).
    let row = fetch_document_row(&state.db, document_id).await?;
    Ok(Json(document_row_to_json(&row)))
}

/// GET /api/projects/:id/documents/:doc_id/download
pub async fn download_document(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, doc_id)): AxumPath<(String, String)>,
) -> Result<axum::response::Response<Body>, ApiError> {
    let user_id = parse_user_id(&claims)?;
    let project_id = parse_project_id(&id)?;
    let document_id = parse_document_id(&doc_id)?;

    let file_path = fetch_document_file_path(&state.db, document_id, project_id).await?;

    // Resolve absolute path from project root
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let abs_path = context.root_path.join(&file_path);

    if !abs_path.exists() {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "File documento non trovato sul filesystem",
        ));
    }

    let bytes = fs::read(&abs_path).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Errore lettura file: {e}"),
        )
    })?;

    docx_attachment_response(&abs_path, bytes)
}

/// GET /api/projects/:id/documents/:doc_id/versions
pub async fn list_versions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((_id, doc_id)): AxumPath<(String, String)>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let document_id = parse_document_id(&doc_id)?;

    let versions = fetch_versions(&state.db, document_id).await?;

    Ok(Json(json!({ "versions": versions })))
}

/// DELETE /api/projects/:id/documents/:doc_id
pub async fn delete_document(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, doc_id)): AxumPath<(String, String)>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = parse_project_id(&id)?;
    let document_id = parse_document_id(&doc_id)?;

    // Fetch riferimenti + DELETE riga (cascade sulle versioni) nel punto unico.
    let (file_path, qdrant_point_ids) =
        delete_document_db(&state.db, document_id, project_id).await?;

    // Delete file from filesystem
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let abs_path = context.root_path.join(&file_path);
    let _ = fs::remove_file(&abs_path).await;

    // Delete Qdrant points
    if !qdrant_point_ids.is_empty() {
        let _ = crate::vector_memory::delete_doc_points_by_ids(&state.db, &qdrant_point_ids).await;
    }

    Ok(Json(json!({ "deleted": true })))
}

/// POST /api/projects/:id/documents/generate
///
/// FIX 3/4: generazione documento SENZA passare per l'agente conversazionale.
/// Prima il pulsante "Genera" del pannello DOCUMENTI inviava un messaggio in
/// chat (`onSendToChat`) che instradava la richiesta sull'agente generico: dopo
/// la chiamata al tool l'agente non era vincolato a fermarsi e proseguiva con
/// una "revisione" del progetto non richiesta; inoltre il pannello si
/// aggiornava solo a fine turno (evento window `nexus:documents:refresh`),
/// quindi con timing impredicibile.
///
/// Questo endpoint chiama direttamente `nexus_builtin::handle_doc_generate`
/// (stesso punto unico usato dal tool) e ritorna l'esito sincrono: il frontend
/// puo' fare il refresh subito, deterministico.
/// Il toast da mostrare all'utente, composto DAI CAMPI della risposta
/// (regola Q): `None` quando non c'e' niente da notificare.
///
/// L'esito e' `risposta.esito`, punto. Prima questo endpoint lo ricostruiva dal
/// testo con `is_tool_failure`: non passa dal dispatch dei tool agente — chiama
/// `handle_doc_generate` direttamente — quindi non incontrava mai il ponte
/// `RispostaTool::da_testo_legacy`, l'unico autorizzato a quel lavoro (regola
/// L), e se ne era fatto uno proprio. Con l'esito nel campo, il testo torna a
/// essere solo testo e comporlo non puo' piu' cambiare la decisione.
fn notifica_di_fallimento(risposta: &nexus_types::tool_outcome::RispostaTool) -> Option<String> {
    if !risposta.esito.e_fallito() {
        return None;
    }
    Some(messaggio_leggibile_del_fallimento(&risposta.testo))
}

/// Toglie dal testo l'etichetta "[Errore]"/"[Errore DB]" che alcuni handler
/// antepongono ancora al messaggio.
///
/// E' PRESENTAZIONE e nient'altro, e si applica DOPO che l'esito e' stato letto
/// dal campo: la frase che incornicia il messaggio ("Generazione documento
/// fallita: ...") dice gia' che e' un errore, e ripeterlo suona come due errori
/// distinti. Non tocca il marker: da un handler migrato il testo non ne porta.
fn messaggio_leggibile_del_fallimento(testo: &str) -> String {
    let testo = testo.trim_start();
    testo
        .strip_prefix("[Errore DB]")
        .or_else(|| testo.strip_prefix("[Errore]"))
        .unwrap_or(testo)
        .trim()
        .to_string()
}

pub async fn generate_document(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = parse_project_id(&id)?;

    let doc_type = body
        .get("doc_type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Parametro 'doc_type' obbligatorio"))?;
    let title = body
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    // Costruisce gli stessi argomenti del tool nexus_doc_generate. content_json
    // omesso: il backend auto-genera (con KB injection, FIX 1).
    let args = json!({
        "project_id": project_id.to_string(),
        "doc_type": doc_type,
        "title": title,
    });

    // Generazione ASINCRONA: con un modello heavy/thinking la completion puo'
    // durare minuti. Se la legassimo alla connessione HTTP, il proxy la
    // chiuderebbe in timeout (-> 500) e axum cancellerebbe l'handler a meta',
    // lasciando nessun documento. Avviamo in background e ritorniamo subito 202:
    // il completamento arriva al pannello via evento SSE DocumentGenerated (su
    // successo) o Notification (su errore), gia' ascoltati dal frontend.
    let db = state.db.clone();
    let doc_type_for_log = doc_type.clone();
    tokio::spawn(async move {
        let risposta =
            crate::nexus_builtin::handle_doc_generate(&db, project_id, user_id, &args).await;
        if let Some(msg) = notifica_di_fallimento(&risposta) {
            tracing::warn!(doc_type = %doc_type_for_log, "generate_document (async): {msg}");
            // Notifica il fallimento al pannello (toast), altrimenti l'utente
            // resterebbe in attesa di un documento che non arrivera' mai.
            let _ = nexus_events::dispatcher::emit_global(
                project_id,
                nexus_events::event::ProjectEvent::Notification {
                    severity: "error".to_string(),
                    message: format!("Generazione documento fallita: {msg}"),
                    panel: Some("documents".to_string()),
                    ttl_ms: Some(10000),
                    run_id: None,
                },
            );
        }
        // Su successo handle_doc_generate ha gia' emesso DocumentGenerated.
    });

    Ok(Json(json!({
        "status": "accepted",
        "message": "Generazione avviata: il documento comparira' nel pannello al termine."
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_types::tool_outcome::{RispostaTool, TOOL_FAILURE_MARKER};

    /// La risposta parte dai costruttori di `RispostaTool`, gli stessi che
    /// `handle_doc_generate` usa (regola O): comporre a mano una struct qui
    /// fisserebbe l'assunto da verificare.
    #[test]
    fn il_prefisso_errore_db_non_arriva_all_utente() {
        let risposta = RispostaTool::fallito("[Errore DB] connessione rifiutata");
        assert_eq!(
            notifica_di_fallimento(&risposta).as_deref(),
            Some("connessione rifiutata")
        );
    }

    #[test]
    fn il_prefisso_errore_semplice_non_arriva_all_utente() {
        let risposta = RispostaTool::fallito("[Errore] doc_type non riconosciuto");
        assert_eq!(
            notifica_di_fallimento(&risposta).as_deref(),
            Some("doc_type non riconosciuto")
        );
    }

    /// Senza prefisso il messaggio resta integro: la spogliatura non deve
    /// mangiare testo che non sia l'etichetta.
    #[test]
    fn un_fallimento_senza_prefisso_resta_integro() {
        let risposta = RispostaTool::fallito("il modello non ha prodotto contenuto");
        assert_eq!(
            notifica_di_fallimento(&risposta).as_deref(),
            Some("il modello non ha prodotto contenuto")
        );
    }

    /// LA prova del fix. Un handler migrato dichiara il fallimento nel CAMPO e
    /// il suo testo non porta il marker: chi decideva guardando la testa della
    /// stringa (`is_tool_failure`) non vedeva piu' nulla e taceva — nessun
    /// toast, e l'utente in attesa di un documento che non arrivera' mai.
    ///
    /// MUTAZIONE: si sostituisce il corpo di `notifica_di_fallimento` con
    /// `if !nexus_types::tool_outcome::is_tool_failure(&risposta.testo) { return None; }`
    /// e questo test rosseggia con `None` — il valore esatto del difetto.
    #[test]
    fn un_fallimento_senza_marker_nel_testo_viene_comunque_notificato() {
        let risposta = RispostaTool::fallito("[Errore] Progetto non trovato o workspace mancante");
        assert!(
            !risposta.testo.contains(TOOL_FAILURE_MARKER),
            "un handler migrato non scrive il marker nel testo"
        );
        assert_eq!(
            notifica_di_fallimento(&risposta).as_deref(),
            Some("Progetto non trovato o workspace mancante"),
            "l'esito sta nel campo: il testo non ha voce in capitolo"
        );
    }

    /// Il duale: un successo il cui testo NOMINA un errore (il JSON di esito
    /// puo' contenerne il titolo del documento) non deve produrre un toast.
    #[test]
    fn un_successo_che_nomina_un_errore_non_notifica_nulla() {
        let risposta = RispostaTool::riuscito(
            r#"{"ok":true,"title":"[Errore] analisi dei log","file_path":"docs/x.docx"}"#,
        );
        assert_eq!(notifica_di_fallimento(&risposta), None);
    }

    /// Il marker non deve arrivare al toast: e' un segnale macchina, mostrarlo
    /// lo trasformerebbe in decorazione.
    #[test]
    fn il_marker_non_arriva_mai_al_toast() {
        for testo in ["[Errore DB] x", "[Errore] y", "z"] {
            let out = notifica_di_fallimento(&RispostaTool::fallito(testo))
                .expect("un fallimento dichiarato produce sempre un toast");
            assert!(!out.contains(TOOL_FAILURE_MARKER), "marker residuo in {out:?}");
        }
    }
}
