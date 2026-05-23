use std::sync::Arc;

use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    agent_types::{AgentRunStatus, AgentStep, AgentStepEvent, AgentStepStatus, SupervisorMode},
    agent_tools::AgentToolContext,
    auth::Claims,
    chat_learning::{
        api_error, apply_project_learning, dedup_on_write, ensure_project_access, hash_hint,
        normalize_text, parse_project_id, parse_user_id, ApiError, ApiResult,
    },
    chat_sessions::{load_session_context, update_user_active_project},
    orchestrator::{AutomationMode, ChatAttachment, OrchestratorRequest, OrchestratorResult},
    profiles::fetch_profile_context,
    projects::load_project_context,
    vector_memory, AppState,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendChatMessageRequest {
    pub content: String,
    pub profile_id: Option<String>,
    #[serde(default)]
    pub active_files: Vec<String>,
    pub provider_override: Option<String>,
    pub model_override: Option<String>,
    pub automation_mode: Option<String>,
    pub supervisor_mode: Option<String>,
    #[serde(default)]
    pub attachments: Vec<ChatAttachmentRequest>,
    /// Hint opzionale: nome dell'AgentType da usare (es. "Coder", "Tester").
    /// Se presente, bypassa il Q-Learning router e forza quel tipo di agente.
    pub agent_type_hint: Option<String>,
    /// Se true, il messaggio e' generato automaticamente dal sistema
    /// (es. auto-continuazione in modalita' "automatic") e NON deve essere
    /// mostrato nella UI come messaggio utente. Viene comunque persistito
    /// nel DB e usato per triggerare l'agent run.
    #[serde(default)]
    pub synthetic: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachmentRequest {
    pub name: String,
    pub mime_type: String,
    pub size_bytes: i64,
    #[serde(default)]
    pub text_content: String,
    #[serde(default)]
    pub base64_content: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackErrorRequest {
    pub comment: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackPositiveRequest {
    /// Commento opzionale (es. "perfetto", "soluzione elegante"). Salvato per audit ma non genera correzioni.
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyChatRequest {
    pub project_id: String,
    pub profile_id: String,
    pub message: String,
    #[serde(default)]
    pub active_files: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ChatMessageView {
    id: String,
    session_id: String,
    project_id: String,
    role: String,
    content: String,
    request_message_id: Option<String>,
    deleted_at: Option<String>,
    created_at: String,
    provider: Option<String>,
    model: Option<String>,
    intent: Option<String>,
    run_id: Option<String>,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    total_tokens: Option<i64>,
    total_cost: Option<f64>,
    currency: Option<String>,
    automation_mode: Option<String>,
    resend_of_message_id: Option<String>,
    /// True quando il messaggio e' auto-generato dal sistema (es. auto-continuazione).
    /// La UI nasconde questi messaggi per non confondere l'utente.
    synthetic: bool,
}

fn to_message_view(row: &sqlx::postgres::PgRow) -> Result<ChatMessageView, ApiError> {
    let id: Uuid = row
        .try_get("id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let session_id: Uuid = row
        .try_get("session_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let project_id: Uuid = row
        .try_get("project_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let role: String = row
        .try_get("role")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let content: String = row
        .try_get("content")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let request_message_id: Option<Uuid> = row.try_get("request_message_id").unwrap_or(None);
    let deleted_at: Option<DateTime<Utc>> = row.try_get("deleted_at").unwrap_or(None);
    let created_at: DateTime<Utc> = row
        .try_get("created_at")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let metadata: Value = row.try_get("metadata").unwrap_or_else(|_| json!({}));

    Ok(ChatMessageView {
        id: id.to_string(),
        session_id: session_id.to_string(),
        project_id: project_id.to_string(),
        role,
        content,
        request_message_id: request_message_id.map(|value| value.to_string()),
        deleted_at: deleted_at.map(|value| value.to_rfc3339()),
        created_at: created_at.to_rfc3339(),
        provider: metadata
            .get("provider")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        model: metadata
            .get("model")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        intent: metadata
            .get("intent")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        run_id: metadata
            .get("runId")
            .or_else(|| metadata.get("agentRunId"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        prompt_tokens: metadata.get("promptTokens").and_then(Value::as_i64),
        completion_tokens: metadata.get("completionTokens").and_then(Value::as_i64),
        total_tokens: metadata.get("totalTokens").and_then(Value::as_i64),
        total_cost: metadata.get("totalCost").and_then(Value::as_f64),
        currency: metadata
            .get("currency")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        automation_mode: metadata
            .get("automationMode")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        resend_of_message_id: metadata
            .get("resendOf")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        synthetic: metadata
            .get("synthetic")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn parse_automation_mode(value: Option<&str>) -> AutomationMode {
    match value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("confirm")
        .to_lowercase()
        .as_str()
    {
        "study" | "studio" => AutomationMode::Study,
        "automatic" | "automatico" | "auto" => AutomationMode::Automatic,
        _ => AutomationMode::Confirm,
    }
}

#[allow(dead_code)]
fn build_static_system_context(github_username: Option<&str>) -> String {
    let mut ctx = String::from(
        "Sei Nexus, agente operativo di sviluppo. Regole:\n\
         Output: testo pulito, markdown standard (no emoji, no caratteri grafici).\n\
         Tool iniziali: read_file, list_files, search_in_files, write_file, edit_file, run_command.\n\
         Tool aggiuntivi: usa request_tools(categories) per sbloccare categorie extra quando ne hai bisogno:\n\
         - \"git\": git_status, git_stage, git_commit, git_push, git_pull\n\
         - \"service\": run_service, read_service_output, stop_service (servizi long-running)\n\
         - \"files_advanced\": delete_file, rename_file\n\
         - \"profile\": create_profile, update_profile\n\
         - \"subtask\": dispatch_subtask (sotto-task paralleli)\n\
         - \"mcp\": tool da server MCP esterni (plugin, Figma, ecc.)\n\
         - \"quality\": scan_code_quality (analisi qualità: complessità, typing, SQL, commenti, dead code)\n\
         - \"semantic\": search_codebase_semantic (ricerca semantica nel codebase), recall_context (recupera contesto conversazionale/progetto precedente)\n\
         Puoi richiedere piu' categorie insieme: request_tools(\"git,service\").\n\
         Tool Dispatcher (sempre disponibili, NON serve request_tools, sono FUNCTION CALL non comandi shell):\n\
         dispatcher_set_flag(key, value) — flag progetto nel pannello Monitor (prefissi: build_, test_, deploy_, custom_, feature_).\n\
         dispatcher_update_monitor(monitor_id, value, label) — widget numerico nel pannello Monitor.\n\
         dispatcher_post_notification(severity, message) — toast utente (info/success/warning/error).\n\
         dispatcher_emit_event(kind, resource, payload) — evento custom sul bus eventi.\n\
         dispatcher_highlight_panel(panel) — flash su un pannello IDE.\n\
         Generazione documenti — SEMPRE DISPONIBILI (non serve request_tools):\n\
         Quando l'utente chiede di generare documentazione, analisi, diagrammi ER, release notes o documenti di progetto, DEVI usare il tool nexus_doc_generate.\n\
         NON rispondere con testo in chat — genera SEMPRE un documento .docx reale.\n\
         Flusso OBBLIGATORIO (segui ESATTAMENTE questi passi):\n\
         Passo 1: ANALISI APPROFONDITA — Esplora l'intero codebase con list_files (anche nelle sottodirectory). Leggi ALMENO 15-20 file chiave: entry point, configurazioni, modelli dati, controller/routes, servizi, migrazioni DB, test, Dockerfile/compose. NON fermarti dopo 3-4 file.\n\
         Passo 2: Costruisci il content_json COMPLETO con sezioni strutturate. Formato:\n\
         {\"sections\":[{\"number\":\"1\",\"title\":\"Introduzione\",\"content\":\"Testo della sezione...\",\"subsections\":[{\"number\":\"1.1\",\"title\":\"Scopo\",\"content\":\"Testo...\"}]},{\"number\":\"2\",\"title\":\"Requisiti Funzionali\",\"content\":\"Testo...\",\"subsections\":[]}]}\n\
         IMPORTANTE: content_json DEVE essere un oggetto JSON valido con il campo \"sections\" (array). Ogni sezione ha: number (stringa), title (stringa), content (testo lungo e dettagliato), subsections (array, puo' essere vuoto).\n\
         Passo 3: Chiama nexus_doc_generate(project_id=<id progetto attivo>, doc_type=<tipo>, content_json=<il JSON costruito al passo 2>).\n\
         doc_type disponibili: functional_analysis, technical_analysis, er_diagram, project_management, release_notes.\n\
         STANDARD DI QUALITA' OBBLIGATORIO:\n\
         - OGNI sezione deve contenere almeno 5-8 frasi CONCRETE basate sul codice reale analizzato. Cita nomi file, classi, funzioni, tabelle, endpoint specifici.\n\
         - Includi ALMENO 8-12 sezioni principali, ciascuna con 2-4 sottosezioni.\n\
         - NON usare placeholder generici come \"...\" o \"da definire\". Ogni contenuto deve essere SPECIFICO per il progetto.\n\
         - Le versioni delle dipendenze devono essere ESATTE (lette da package.json, Cargo.toml, .csproj).\n\
         - Gli endpoint API devono includere metodo HTTP, path completo, parametri, body di esempio.\n\
         - Lo schema DB deve elencare TUTTE le tabelle con colonne, tipi e relazioni FK lette dalle migrazioni.\n\
         - NON chiedere conferma prima di generare — quando l'utente dice \"genera\" o \"procedi\", esegui SUBITO.\n\
         - Se l'utente dice \"procedi\" o \"confermo\", DEVI procedere immediatamente con la generazione.\n\
         Il documento .docx verra' salvato nel pannello Documenti del progetto.\n\
         Per generare documentazione API del codice (cargo doc, npm docs): usa nexus_api_docs (tool separato).\n\
         Altri tool documenti: nexus_doc_list (elenca), nexus_doc_search (cerca nei documenti), nexus_doc_status (cambia stato draft/review/approved).\n\
         Autonomia: NON chiedere mai struttura, tecnologia, OS, comandi — ricava tutto dal contesto progetto qui sotto o con list_files/read_file.\n\
         PERO' SE ti mancano informazioni che NON puoi ricavare autonomamente (connection string, API keys, credenziali, \
         configurazioni specifiche dell'ambiente, password, URL di servizi esterni), DEVI fermarti e chiedere all'utente. \
         Non tentare di indovinare valori sensibili. Spiega cosa ti serve e perche'.\n\
         Avvio servizi: leggi gli script dal contesto progetto e usa run_service direttamente.\n\
         Accesso ambiente: NON dichiarare mai \"non ho accesso al filesystem/terminale\" quando il progetto e' attivo; esplora e agisci con i tool disponibili.\n\
         Persistenza: se un'operazione fallisce, leggi l'errore e riprova. Verifica sempre con run_command (ss -tlnp, ps aux, curl) che il servizio sia attivo prima di dichiararlo avviato.\n\
         Git: usa credenziali utente autenticato. Per cloni parti da $NEXUS_TERMINAL_ROOT.\n\
         File protetti — REGOLA ASSOLUTA: non modificare MAI questi file anche se richiesto esplicitamente:\n\
         .env, .env.*, nexus.env, *.key, *.pem, secrets, credentials, Cargo.lock, pnpm-lock.yaml.\n\
         Se una modifica richiede di aggiornare la configurazione ambiente, descrivi il cambiamento da fare e chiedi all'utente di eseguirlo manualmente.\n\
         Sviluppo self-hosted (progetto Nexus su /opt/ai-orchestrator): quando lavori sul progetto Nexus stesso:\n\
         1. Modifica i sorgenti Rust in crates/mcp-core/src/ o il frontend in apps/web-ide/\n\
         2. Per compilare il backend: run_command(\"cd /opt/ai-orchestrator && ~/.cargo/bin/cargo build -p mcp-core --release\")\n\
         3. Per deployare con zero-downtime: run_command(\"cd /opt/ai-orchestrator && bash scripts/deploy-nexus.sh --rust-only\") oppure --web-only o --full\n\
         4. Il deploy verifica automaticamente che il nuovo binario sia in esecuzione (build_time nel /api/health)\n\
         5. NON modificare direttamente il binario in target/release/ — usa sempre il processo di build\n\
         6. Il frontend si riconnette automaticamente dopo il restart del backend (~3s)\n\
         Profili: quando noti che l'utente lavora ripetutamente su uno stack tecnico specifico (C#/.NET, React, Python/Django, Rust, DevOps, ecc.) \
         o ha preferenze ricorrenti, crea un profilo con create_profile — senza aspettare che lo chieda. \
         Includi nel system_prompt le best practice, i pattern preferiti e lo stile di risposta ottimale per quel dominio. \
         Se il profilo esiste gia', aggiornalo con update_profile.\n\
         Ricerca semantica: quando non sai in quale file si trova un componente, funzione o feature, usa SEMPRE search_codebase_semantic prima di usare search_in_files. \
         Esempi: cerchi una card UI -> search_codebase_semantic(\"card Dettagli richiesta\"); cerchi la logica di autenticazione -> search_codebase_semantic(\"autenticazione login JWT\"). \
         Per usare search_codebase_semantic richiedi prima la categoria: request_tools(\"semantic\").\n\
         File grandi — REGOLA CRITICA PER PERFORMANCE:\n\
         read_file restituisce solo le prime 300 righe. Se il file è più grande, segui questo flusso:\n\
         1. read_file(path) — ottieni le prime 300 righe + numero totale righe\n\
         2. Se ti servono righe specifiche: read_file_lines(path, start_line, end_line) — legge max 400 righe per chiamata\n\
         3. Se non sai dove si trova la funzione/sezione: request_tools(\"semantic\"), poi search_codebase_semantic(\"descrizione\") — ti dà il numero di riga esatto, poi usa read_file_lines\n\
         NON chiamare read_file più volte sullo stesso file grande. NON caricare l'intero file se ti serve solo una funzione.\n\
         Esempio corretto per modificare una funzione in un file da 2000 righe:\n\
         - search_codebase_semantic(\"nome funzione o comportamento\") → trovi riga ~500\n\
         - read_file_lines(path, 490, 560) → leggi solo quella sezione\n\
         - edit_file(path, old_str, new_str) → modifica chirurgica\n\
         Regole OBBLIGATORIE per edit_file — seguile sempre senza eccezioni:\n\
         1. LEGGI PRIMA: usa read_file_lines per leggere la sezione ESATTA che vuoi modificare. Non usare edit_file su codice che non hai letto nello stesso turno.\n\
         2. old_string LUNGO: includi almeno 5 righe di contesto sopra e sotto il punto da modificare. old_string troppo corto → rischio di match errato o modifica del punto sbagliato.\n\
         3. VERIFICA SUBITO: dopo ogni edit_file, esegui run_command con la verifica sintattica del file (es. 'npx tsc --noEmit src/path/to/file.ts' per TypeScript, 'cargo check' per Rust). Se la verifica fallisce, correggi immediatamente prima di procedere.\n\
         4. UN EDIT ALLA VOLTA: non fare mai piu' edit_file sullo stesso file senza leggere l'aggiornamento dopo ciascuno.\n\
         5. NON DUPLICARE CODICE: verifica che new_string non reintroduca codice già presente nel file. Leggi i blocchi circostanti prima di scrivere.\n\
         6. Se edit_file ritorna '[Errore: old_string non trovato]': il messaggio di errore include GIA' le prime 80 righe del file con numerazione. Confronta il tuo old_string con quelle righe e correggi le differenze (spazi, newline, testo). NON chiamare read_file o read_file_lines — il contenuto e' gia' nell'errore. Se la sezione non e' nelle prime 80 righe, usa read_file_lines con start_line/end_line diversi da quelli usati in precedenza.\n\
         Commenti nel codice — regole tassative:\n\
         1. NON commentare ogni riga. Commenti su ogni riga rendono il codice illeggibile e non aggiungono valore.\n\
         2. Commenta SOLO: (a) funzioni/metodi esportati o pubblici con JSDoc/docstring che spiegano scopo, parametri, return; \
         (b) blocchi di logica complessa o non ovvia con 1-2 righe che spiegano il PERCHE', non il cosa; \
         (c) costanti o configurazioni non intuitive; (d) workaround o fix temporanei con spiegazione.\n\
         3. I commenti devono spiegare il PERCHE' (motivazione, decisione architetturale, constraint), mai il COSA (che si legge dal codice).\n\
         4. Quando refactori o modifichi codice esistente, mantieni i commenti utili gia' presenti, rimuovi quelli obsoleti o ovvi.\n\
         5. Per documentare una funzione usa il formato nativo del linguaggio: JSDoc per TS/JS, /// per Rust, \"\"\" per Python, /// per C#.\n\
         Limite iterazioni — REGOLA CRITICA:\n\
         Hai un budget massimo di 50 iterazioni per completare il task. Ogni tool call consuma una iterazione.\n\
         - NON fare polling: non chiamare lo stesso tool piu' di 3 volte di fila per monitorare uno stato (es. read_service_output, run_command su stesso processo).\n\
         - Se qualcosa non risponde dopo 3 tentativi, fermati e riferisci all'utente.\n\
         - Preferisci azioni dirette: leggi, modifica, verifica. Evita loop di attesa.\n\
         - Se stai usando piu' di 10 iterazioni consecutive su run_command/read_service_output, stai probabilmente ciclando: FERMATI.\n\
         Routing del modello — REGOLA ASSOLUTA:\n\
         Il sistema gestisce autonomamente la selezione del provider e del modello AI. NON fare MAI commenti su quale modello stai usando, NON dire mai all'utente di cambiare il modello dall'interfaccia, NON menzionare provider, token, costi o limitazioni tecniche legate al modello. Se l'utente chiede di usare un modello diverso, il sistema lo gestisce automaticamente: tu rispondi semplicemente al contenuto della richiesta.",
    );
    if let Some(gh) = github_username {
        if !gh.trim().is_empty() {
            ctx.push_str(&format!(" Account GitHub: @{gh}."));
        }
    }
    ctx
}

/// Costruisce un blocco "Contesto progetto (Knowledge Base)" da iniettare nel system prompt.
///
/// Pipeline:
///   1. Legge settings `knowledge.context_injection_*` (enabled, top_k, min_score)
///   2. Genera embedding del messaggio user via brain
///   3. Cerca top-K note simili in Qdrant filtrate per project_id + status (active/draft)
///   4. Carica title+body delle note matching da `project_knowledge_notes`
///   5. Formatta come Markdown con tag + intent + snippet
///
/// Failsafe: se brain down, Qdrant vuoto, o KB disabilitata, ritorna `None` e il
/// flow normale prosegue senza KB context.
async fn build_knowledge_context(
    state: &AppState,
    project_id: Uuid,
    user_message: &str,
) -> Option<String> {
    // 1. Settings (cache-friendly: una sola query in batch)
    let enabled: bool = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'knowledge.context_injection_enabled'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .map(|v| v.trim().eq_ignore_ascii_case("true"))
    .unwrap_or(true);
    if !enabled {
        return None;
    }
    let top_k: usize = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'knowledge.context_injection_top_k'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .and_then(|s| s.parse::<usize>().ok())
    .unwrap_or(5)
    .clamp(1, 20);
    let min_score: f32 = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'knowledge.context_injection_min_score'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .and_then(|s| s.parse::<f32>().ok())
    .unwrap_or(0.5);

    // 2. Embed messaggio user
    let embed_text = if user_message.len() > 2000 {
        &user_message[..2000]
    } else {
        user_message
    };
    let vector = match state.orchestrator.neural.embed_text("", embed_text).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, "build_knowledge_context: embed fallito (brain down?), skip");
            return None;
        }
    };

    // 3. Search Qdrant
    let hits = match crate::vector_memory::search_knowledge_points(
        &state.db,
        vector,
        project_id,
        top_k * 2, // overfetch, filtreremo per score
    )
    .await
    {
        Ok(h) => h,
        Err(e) => {
            tracing::debug!(error = %e, "build_knowledge_context: Qdrant search fallita");
            return None;
        }
    };
    let note_ids: Vec<(Uuid, f32)> = hits
        .iter()
        .filter(|h| (h.score as f32) >= min_score)
        .filter_map(|h| {
            h.payload
                .get("note_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<Uuid>().ok())
                .map(|id| (id, h.score as f32))
        })
        .take(top_k)
        .collect();
    if note_ids.is_empty() {
        return None;
    }

    // 4. Carica title+body+tags+intent dalle note
    use sqlx::Row;
    let ids: Vec<Uuid> = note_ids.iter().map(|(id, _)| *id).collect();
    let rows = sqlx::query(
        r#"
        SELECT id, title, body_md, tags, intent, status
        FROM project_knowledge_notes
        WHERE id = ANY($1)
          AND status IN ('active', 'draft')
        "#,
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await
    .ok()?;
    if rows.is_empty() {
        return None;
    }

    // 5. Format markdown (ordinato per score)
    let mut by_id: std::collections::HashMap<Uuid, (String, String, Vec<String>, Option<String>, String)> =
        std::collections::HashMap::new();
    for r in &rows {
        let id: Uuid = r.try_get("id").ok()?;
        let title: String = r.try_get("title").unwrap_or_default();
        let body: String = r.try_get("body_md").unwrap_or_default();
        let tags: Vec<String> = r.try_get("tags").unwrap_or_default();
        let intent: Option<String> = r.try_get("intent").ok();
        let status: String = r.try_get("status").unwrap_or_default();
        by_id.insert(id, (title, body, tags, intent, status));
    }

    let mut out = String::with_capacity(1024);
    out.push_str("## Contesto dal Knowledge Base del progetto\n\n");
    out.push_str("Note pertinenti dalla cronologia del progetto (ordinate per rilevanza). ");
    out.push_str("Considera questi precedenti per evitare duplicazioni e mantenere coerenza con le decisioni gia' prese:\n\n");
    for (id, score) in &note_ids {
        let Some((title, body, tags, intent, status)) = by_id.get(id) else {
            continue;
        };
        let snippet = body.chars().take(280).collect::<String>();
        let intent_disp = intent.as_deref().unwrap_or("-");
        let tags_disp = if tags.is_empty() {
            "".to_string()
        } else {
            format!(" #{}", tags.join(" #"))
        };
        out.push_str(&format!(
            "- **{}** _(intent: {}, status: {}, rilevanza: {:.2}){}_\n  {}{}\n\n",
            title.trim(),
            intent_disp,
            status,
            score,
            tags_disp,
            snippet.trim(),
            if body.len() > 280 { "..." } else { "" }
        ));
    }
    out.push_str(
        "_(Fonte: project_knowledge_notes — KB automatica del progetto. Aggiornata ad ogni messaggio utente.)_\n",
    );

    tracing::info!(
        project_id = %project_id,
        notes_injected = note_ids.len(),
        "build_knowledge_context: contesto KB iniettato"
    );

    Some(out)
}

#[allow(dead_code)]
async fn load_project_analysis_summary(db: &PgPool, project_id: Uuid) -> Option<String> {
    sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT analysis_json FROM projects WHERE id = $1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .and_then(|analysis: serde_json::Value| {
        let langs = analysis
            .get("languages")
            .and_then(|l: &serde_json::Value| l.as_array())
            .map(|arr: &Vec<serde_json::Value>| {
                arr.iter()
                    .take(5)
                    .filter_map(|e: &serde_json::Value| {
                        e.get("language")
                            .and_then(|v: &serde_json::Value| v.as_str())
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let frameworks = analysis
            .get("frameworks")
            .and_then(|f: &serde_json::Value| f.as_array())
            .map(|arr: &Vec<serde_json::Value>| {
                arr.iter()
                    .take(6)
                    .filter_map(|v: &serde_json::Value| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let scripts = analysis
            .get("dependencies")
            .and_then(|d: &serde_json::Value| d.get("node"))
            .and_then(|n: &serde_json::Value| n.get("scripts"))
            .and_then(|s: &serde_json::Value| s.as_object())
            .map(|scripts_map: &serde_json::Map<String, serde_json::Value>| {
                scripts_map
                    .iter()
                    .take(8)
                    .map(|(k, v): (&String, &serde_json::Value)| {
                        format!("  {} -> {}", k, v.as_str().unwrap_or(""))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();

        if langs.is_empty() && frameworks.is_empty() {
            None
        } else {
            let mut summary = format!("Linguaggi: {}\nFramework/stack: {}", langs, frameworks);
            if !scripts.is_empty() {
                summary.push_str(&format!("\nScript disponibili:\n{}", scripts));
            }
            Some(summary)
        }
    })
}

#[allow(dead_code)]
async fn build_composed_system_context(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    profile_prompt_block: &str,
) -> String {
    let github_username: Option<String> =
        sqlx::query_scalar("SELECT github_username FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(db)
            .await
            .unwrap_or(None)
            .flatten();

    let base = build_static_system_context(github_username.as_deref());
    let analysis_summary = load_project_analysis_summary(db, project_id).await;

    // Servizi attivi e porte allocate: l'agente DEVE sapere quali servizi
    // sono gia' attivi cosi' non lancia duplicati ne' invoca curl su porte
    // sbagliate. Hardcoded list di allocazioni + (best effort) servizi systemd.
    let services_block = {
        let allocations: Vec<(i32, String, String)> = sqlx::query_as(
            "SELECT port, label, allocation_mode FROM nexus_port_allocations \
             WHERE project_id = $1 ORDER BY port",
        )
        .bind(project_id)
        .fetch_all(db)
        .await
        .unwrap_or_default();
        if allocations.is_empty() {
            String::new()
        } else {
            let mut s = String::from("=== INFRASTRUTTURA DEL PROGETTO (informativo) ===\n");
            s.push_str("I seguenti servizi sono GIA' avviati e raggiungibili. Sono solo info di context — NON sostituiscono il task dell'utente, devi comunque procedere col compito richiesto usando i tool a disposizione.\n");
            for (port, label, mode) in &allocations {
                s.push_str(&format!(
                    "- {} → http://localhost:{} ({})\n",
                    label, port, mode
                ));
            }
            s.push_str("\nQuando hai bisogno di:\n");
            s.push_str("- chiamare curl al backend: usa una delle URL qui sopra invece di porte default (3001, 3000, 5173, 8080)\n");
            s.push_str("- riavviare un servizio: usa service_restart con la sua label, NON lanciare un nuovo pnpm dev\n");
            s.push_str("- modificare file del progetto: procedi normalmente con read_file/edit_file/write_file/run_command (questo blocco NON ti impedisce di lavorare)\n");
            s.push_str("=== FINE INFRASTRUTTURA ===\n\n");
            s
        }
    };

    let project_header = match load_project_context(db, project_id, user_id).await {
        Ok(proj) => {
            if let Some(summary) = analysis_summary {
                format!(
                    "=== CONTESTO PROGETTO (non chiedere queste informazioni: sono gia' qui) ===\n\
                     Progetto: {} | Root: {} | Git: {}\n\
                     {}\n\
                     === FINE CONTESTO PROGETTO ===\n\n",
                    proj.details.name,
                    proj.repository_root_path.display(),
                    if proj.is_git_repo { "si" } else { "no" },
                    summary
                )
            } else {
                format!(
                    "=== CONTESTO PROGETTO ===\n\
                     Progetto: {} | Root: {} | Git: {}\n\
                     (Nessuna analisi disponibile: usa list_files per esplorare la struttura)\n\
                     === FINE CONTESTO PROGETTO ===\n\n",
                    proj.details.name,
                    proj.repository_root_path.display(),
                    if proj.is_git_repo { "si" } else { "no" }
                )
            }
        }
        Err(_) => format!(
            "=== CONTESTO PROGETTO ===\n\
             ProjectId: {}\n\
             (Dettagli progetto non disponibili: usa list_files/read_file per ricostruire contesto)\n\
             === FINE CONTESTO PROGETTO ===\n\n",
            project_id
        ),
    };

    format!("{}{}{}{}", services_block, project_header, profile_prompt_block, base)
}

#[allow(dead_code)]
fn parse_provider_hierarchy(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.starts_with('[') {
        if let Ok(items) = serde_json::from_str::<Vec<String>>(trimmed) {
            return items
                .into_iter()
                .map(|item| item.trim().to_lowercase())
                .filter(|item| !item.is_empty())
                .collect();
        }
    }
    trimmed
        .split(',')
        .map(|item| item.trim().to_lowercase())
        .filter(|item| !item.is_empty())
        .collect()
}

// default_model_for_provider e load_agent_provider_defaults rimossi dopo refactor 0101.
// Erano duplicati di logica in orchestrator.rs e marcati #[allow(dead_code)].
// Per leggere il default per provider usare:
//   crate::orchestrator::default_model_for_provider(matrix, provider)
// con matrix ottenuta da state.orchestrator.routing_matrix.current().

fn humanize_ai_error(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("429")
        || lower.contains("529")
        || lower.contains("rate_limit")
        || lower.contains("rate limit")
        || lower.contains("overloaded")
        || lower.contains("quota")
        || lower.contains("resource_exhausted")
        || lower.contains("service unavailable")
        || lower.contains("503")
    {
        return "Il provider AI è temporaneamente sovraccarico (overloaded). Sto ritentando automaticamente con backoff; riprova tra poco se persiste.".to_string();
    }
    if lower.contains("timeout") {
        return "La richiesta AI e' scaduta per timeout. Riprova con un prompt piu' corto o tra qualche secondo.".to_string();
    }

    let first_line = raw
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("Richiesta AI non completata");
    let trimmed = first_line.trim();
    if trimmed.chars().count() > 220 {
        format!("{}...", trimmed.chars().take(220).collect::<String>())
    } else {
        trimmed.to_string()
    }
}

/// Azzera la preferenza di provider della sessione e segna l'evento privacy.
/// Va chiamato ogni volta che il gateway ha re-instradato su un provider locale per privacy.
/// Al messaggio successivo il sistema userà il routing automatico invece della preferenza precedente.
async fn clear_session_preferred_provider_after_privacy(db: &sqlx::PgPool, session_id: uuid::Uuid) {
    let _ = sqlx::query(
        "UPDATE chat_sessions \
         SET preferred_provider = NULL, preferred_model = NULL, privacy_rerouted_at = NOW() \
         WHERE id = $1",
    )
    .bind(session_id)
    .execute(db)
    .await;
}

/// Rileva se il messaggio è un comando di reset al routing automatico.
fn detect_model_reset(content: &str) -> bool {
    let lower = content.trim().to_lowercase();
    if lower.chars().count() > 80 {
        return false;
    }
    lower == "routing automatico"
        || lower == "modello automatico"
        || lower == "reimposta modello"
        || lower == "reset modello"
        || lower == "reset routing"
        || lower.contains("routing auto")
        || lower.contains("modello auto")
        || lower.contains("torna al default")
        || lower.contains("torna al routing")
        || lower.contains("modello di default")
        || lower.contains("provider di default")
}

/// Rileva se il messaggio dell'utente è un comando esplicito di cambio provider/modello.
///
/// Restituisce `Some((provider, modello_specifico))` se rilevato, `None` altrimenti.
/// Considera solo messaggi brevi (< 100 caratteri) per evitare falsi positivi.
///
/// I PROVIDER ID (mistral, anthropic, openai, google, deepseek) sono identificatori
/// stabili Nexus, quindi keyword-based. I MODELLI invece sono letti dal DB
/// (`ai_price_catalog`) — cosi' aggiungere claude-opus-5 al DB lo rende
/// automaticamente riconoscibile in chat senza modifiche al codice.
async fn detect_model_switch(
    db: &sqlx::PgPool,
    content: &str,
) -> Option<(String, Option<String>)> {
    let lower = content.trim().to_lowercase();
    // Ignora messaggi lunghi: quasi certamente non è un puro comando di switch
    if lower.chars().count() > 100 {
        return None;
    }

    // Identifica il provider richiesto in base a keyword nel messaggio.
    // I 5 provider id sono identificatori stabili (slug Nexus) — non cambiano.
    let provider: &'static str = if lower.contains("mistral")
        || lower.contains("codestral")
        || lower.contains("mixtral")
    {
        "mistral"
    } else if lower.contains("claude")
        || lower.contains("anthropic")
        || lower.contains("sonnet")
        || lower.contains("opus")
        || lower.contains("haiku")
    {
        "anthropic"
    } else if lower.contains("openai")
        || lower.contains("gpt")
        || lower.contains("chatgpt")
        || lower.contains("o1")
        || lower.contains("o3")
    {
        "openai"
    } else if lower.contains("gemini") || lower.contains("google") || lower.contains("bard") {
        "google"
    } else if lower.contains("deepseek") {
        "deepseek"
    } else {
        return None;
    };

    // Verifica che sia presente un verbo d'azione (switch, usa, cambia, ecc.)
    let has_action = lower.starts_with("usa ")
        || lower.starts_with("use ")
        || lower == "usa mistral"
        || lower == "usa claude"
        || lower == "usa openai"
        || lower == "usa gemini"
        || lower == "usa deepseek"
        || lower.contains("cambia")
        || lower.contains("passa a")
        || lower.contains("passa su")
        || lower.contains("switch to")
        || lower.contains("switch su")
        || lower.contains("rispondi con")
        || lower.contains("utilizza ")
        || lower.contains("voglio usare")
        || lower.contains("voglio ")
        || lower.contains("usa il modello")
        || lower.contains("use the model")
        || lower.contains("imposta ")
        || lower.contains("setta ");

    if !has_action {
        return None;
    }

    // Modello specifico: query DB per i modelli enabled del provider scelto.
    // Match: il messaggio contiene il model_id intero, o l'ultima componente
    // (es. "sonnet" matcha "claude-sonnet-4-6") quando il modello ha trattini.
    // Ordinato per is_featured DESC + costo ASC: se ci sono ambiguita' (es.
    // "claude" matcha sia haiku che sonnet), vince il piu' "in evidenza".
    let candidates: Vec<String> = sqlx::query_scalar(
        "SELECT model FROM ai_price_catalog \
         WHERE provider = $1 AND is_enabled = TRUE \
         ORDER BY is_featured DESC, input_cost_per_million_tokens ASC"
    )
    .bind(provider)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let specific_model: Option<String> = candidates.into_iter().find(|m| {
        let m_lower = m.to_lowercase();
        // Match diretto: lower contiene l'intero model_id
        if lower.contains(&m_lower) {
            return true;
        }
        // Match per "famiglia": ogni componente split-trattino del modello
        // (es. "claude-opus-4-6" -> ["claude", "opus", "4", "6"]) — se nel
        // messaggio c'e' una componente "opus" (>=4 char per evitare match
        // di numeri o suffissi tipo "4"), considera match.
        m_lower.split('-').any(|part| {
            part.len() >= 4 && lower.contains(part)
        })
    });

    Some((provider.to_string(), specific_model))
}

fn normalize_attachments(input: &[ChatAttachmentRequest]) -> Vec<ChatAttachment> {
    input
        .iter()
        .filter_map(|attachment| {
            let name = attachment.name.trim();
            let text_content = attachment.text_content.trim();
            let has_text = !name.is_empty() && !text_content.is_empty();
            let has_image = !name.is_empty() && attachment.base64_content.as_ref().map_or(false, |b| !b.is_empty());
            if !has_text && !has_image {
                return None;
            }
            Some(ChatAttachment {
                name: name.to_string(),
                mime_type: attachment.mime_type.trim().to_string(),
                size_bytes: attachment.size_bytes.max(0),
                text_content: text_content.to_string(),
                base64_content: attachment.base64_content.clone(),
            })
        })
        .collect()
}

/// Carica gli ultimi `limit` messaggi della sessione come turn LLM strutturati.
/// Restituisce un Vec di { "role": "user"|"assistant", "content": "..." }
/// pronti da passare come history iniziale all'agent loop.
async fn build_recent_conversation_history(
    db: &PgPool,
    session_id: Uuid,
    limit: i64,
) -> Vec<serde_json::Value> {
    let rows = sqlx::query(
        r#"
        SELECT role, content
        FROM chat_messages
        WHERE session_id = $1
          AND deleted_at IS NULL
          AND role IN ('user', 'assistant', 'summary')
        ORDER BY created_at DESC
        LIMIT $2
        "#,
    )
    .bind(session_id)
    .bind(limit)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    if rows.is_empty() {
        return vec![];
    }

    // Le righe sono DESC → le rovesciamo per avere ordine cronologico
    rows.into_iter().rev().filter_map(|row| {
        let role = row.try_get::<String, _>("role").ok()?;
        let content = row.try_get::<String, _>("content").ok()?;
        if content.trim().is_empty() { return None; }
        // Normalizza il ruolo per compatibilità con il formato messages LLM.
        // 'summary' (iniettato dal compact) viene inviato come messaggio user
        // — il content e' gia' prefissato con "[Riassunto ...]".
        let llm_role = match role.as_str() {
            "assistant" => "assistant",
            _ => "user",
        };
        Some(serde_json::json!({ "role": llm_role, "content": content }))
    }).collect()
}

/// Versione testuale compatta (usata solo per logging)
async fn build_recent_conversation_context(
    db: &PgPool,
    session_id: Uuid,
    limit: i64,
) -> String {
    let msgs = build_recent_conversation_history(db, session_id, limit).await;
    if msgs.is_empty() { return String::new(); }
    let entries: Vec<String> = msgs.iter().filter_map(|m| {
        let role = m.get("role")?.as_str()?;
        let content = m.get("content")?.as_str()?;
        let compact = content.replace('\n', " ").split_whitespace().collect::<Vec<_>>().join(" ");
        let clipped = if compact.chars().count() > 120 {
            format!("{}...", compact.chars().take(120).collect::<String>())
        } else { compact };
        Some(format!("- {}: {}", role, clipped))
    }).collect();
    if entries.is_empty() { String::new() } else {
        format!("Contesto conversazione recente:\n{}", entries.join("\n"))
    }
}

/// Arricchisce il messaggio da classificare con un piccolo contesto dei
/// turni precedenti. Risolve il problema "messaggio troppo generico" per
/// follow-up contestuali come "riepiloga animali", "applica tutte le ultime",
/// "continua", "riprendi", che da soli sono ambigui ma in contesto chiari.
///
/// Strategia: prependiamo gli ultimi 2 turni (max 4 messaggi) in formato
/// compatto, poi delimitiamo chiaramente il messaggio da classificare.
/// Limite totale 800 char per non saturare il classifier.
async fn build_message_with_recent_context_for_classifier(
    db: &PgPool,
    session_id: Uuid,
    current_message: &str,
) -> String {
    let msgs = build_recent_conversation_history(db, session_id, 4).await;
    if msgs.is_empty() {
        return current_message.to_string();
    }
    let mut ctx_parts: Vec<String> = Vec::new();
    for m in &msgs {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let compact = content
            .replace('\n', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let clipped: String = if compact.chars().count() > 140 {
            format!("{}...", compact.chars().take(140).collect::<String>())
        } else {
            compact
        };
        if !clipped.is_empty() {
            ctx_parts.push(format!("{}: {}", role, clipped));
        }
    }
    if ctx_parts.is_empty() {
        return current_message.to_string();
    }
    let mut ctx = ctx_parts.join("\n");
    if ctx.len() > 600 {
        // UTF-8 safe truncate (vedi nota analoga in embed_input)
        let mut cut = 600;
        while cut > 0 && !ctx.is_char_boundary(cut) {
            cut -= 1;
        }
        ctx.truncate(cut);
        ctx.push_str("...");
    }
    format!(
        "[Cronologia recente della conversazione]\n{}\n\n[Messaggio attuale da classificare]\n{}",
        ctx, current_message
    )
}

/// Tipo di query meta auto-referenziale rilevato. Determina quale
/// messaggio precedente significativo va citato nell'hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelfRefIntent {
    /// Riferito al messaggio piu' recente (precedente alla query stessa).
    Last,
    /// Riferito al primo messaggio significativo della sessione.
    First,
}

/// Rileva se il messaggio utente e' una domanda meta auto-referenziale
/// e ritorna il tipo (Last/First). Esempi:
/// - "qual era l'ultima richiesta?", "ripeti l'ultimo", "e l'ultima" → Last
/// - "qual era la prima richiesta?", "e la prima" → First
///
/// In questi casi il LLM deve sapere che il messaggio corrente NON conta
/// come "ultima/prima richiesta" e va riferito al precedente messaggio
/// significativo nella cronologia.
fn detect_self_referential_intent(message: &str) -> Option<SelfRefIntent> {
    let m = message.trim().to_lowercase();
    if m.is_empty() {
        return None;
    }

    // Token "richiesta-target": il messaggio sembra parlare DI un'altra
    // richiesta (vs. essere una richiesta concreta). Sia "richiesta" che
    // "domanda" che "messaggio" qualificano.
    let target_tokens = [
        "richiesta",
        "domanda",
        "messaggio",
        "cosa ho chiesto",
        "cosa ti ho chiesto",
        "cosa avevo chiesto",
        "cosa ti avevo chiesto",
    ];
    let has_target = target_tokens.iter().any(|t| m.contains(t));

    // Token "precedente": frasi che esprimono precedenza temporale.
    let prev_tokens = [
        "ripeti",
        "precedente",
        "prima di",
        "appena chiesto",
        "appena fatto",
    ];
    let has_prev = prev_tokens.iter().any(|t| m.contains(t));

    // Match per "First": qualunque combinazione di "prima" o "iniziale"
    // con target o riferimento alla chat.
    let first_phrases = [
        "prima richiesta",
        "prima domanda",
        "primo messaggio",
        "prima cosa",
        "all'inizio",
        "all'avvio",
        "iniziale",
        "inizio della chat",
        "inizio conversazione",
        "inizio della conversazione",
    ];
    if first_phrases.iter().any(|p| m.contains(p)) {
        return Some(SelfRefIntent::First);
    }
    // Forme abbreviate tipo "e la prima", "qual era la prima"
    if (m.contains("la prima") || m.starts_with("prima ") || m == "la prima" || m == "prima")
        && (has_target || m.len() < 30)
    {
        return Some(SelfRefIntent::First);
    }

    // Match per "Last": pattern espliciti
    let last_phrases = [
        "ultima richiesta",
        "ultima domanda",
        "ultimo messaggio",
        "ultima cosa",
        "qual era la richiesta",
        "qual era la domanda",
        "ripeti l'ultim",
        "ripeti ultim",
        "ripeti la richiesta",
        "ripeti la domanda",
    ];
    if last_phrases.iter().any(|p| m.contains(p)) {
        return Some(SelfRefIntent::Last);
    }
    // Forme abbreviate tipo "e l'ultima", "l'ultima", "qual era l'ultima"
    if m.contains("l'ultima")
        || m.contains("l ultima")
        || (m == "ultima" || m.starts_with("ultima "))
    {
        return Some(SelfRefIntent::Last);
    }
    // "richiesta/domanda/messaggio precedente"
    if has_target && has_prev {
        return Some(SelfRefIntent::Last);
    }

    None
}

/// Backward-compatible wrapper: ritorna true se il messaggio e' una qualsiasi
/// variante di query meta auto-referenziale.
fn detect_self_referential_query(message: &str) -> bool {
    detect_self_referential_intent(message).is_some()
}

/// Cerca un messaggio utente significativo nella sessione, scartando saluti,
/// conferme brevi e meta-domande auto-referenziali. La direzione di scansione
/// dipende dall'intent:
/// - `Last`: dal piu' recente al piu' vecchio (precedente alla query corrente)
/// - `First`: dal piu' vecchio al piu' recente (primo messaggio della chat)
async fn find_target_user_message(
    db: &PgPool,
    session_id: Uuid,
    intent: SelfRefIntent,
) -> Option<String> {
    let order = match intent {
        SelfRefIntent::Last => "DESC",
        SelfRefIntent::First => "ASC",
    };
    let query = format!(
        r#"
        SELECT content FROM chat_messages
        WHERE session_id = $1 AND deleted_at IS NULL AND role = 'user'
        ORDER BY created_at {order}
        LIMIT 20
        "#,
        order = order,
    );
    let rows = sqlx::query(&query)
        .bind(session_id)
        .fetch_all(db)
        .await
        .ok()?;

    let trivial_patterns: &[&str] = &["ok", "si", "sì", "no", "grazie", "ciao", "ok grazie"];
    for row in rows.iter() {
        let content: String = row.try_get("content").ok()?;
        let trimmed = content.trim();
        let lower = trimmed.to_lowercase();
        if trimmed.is_empty() || trimmed.len() < 5 {
            continue;
        }
        if trivial_patterns.iter().any(|p| lower == *p) {
            continue;
        }
        if detect_self_referential_query(trimmed) {
            continue;
        }
        let compact = trimmed
            .replace('\n', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if compact.chars().count() > 120 {
            return Some(format!("{}...", compact.chars().take(120).collect::<String>()));
        }
        return Some(compact);
    }
    None
}

/// Costruisce l'istruzione contestuale "precedente significativo" con un
/// few-shot example *reale* tratto dalla cronologia attuale. Differenzia
/// tra richiesta "Last" (precedente) e "First" (prima della chat). Se la
/// cronologia non contiene messaggi significativi (o non c'e' match),
/// ritorna `None`: il prompt non subisce iniezione spuria.
async fn build_self_referential_hint(
    db: &PgPool,
    session_id: Uuid,
    current_message: &str,
) -> Option<String> {
    let intent = detect_self_referential_intent(current_message)?;
    let target = find_target_user_message(db, session_id, intent).await;
    let (label_role, label_example) = match intent {
        SelfRefIntent::Last => ("precedente", "La tua precedente richiesta era"),
        SelfRefIntent::First => ("prima", "La tua prima richiesta in questa chat e' stata"),
    };
    let core_rule = format!(
        "Istruzione contestuale: l'utente sta chiedendo informazioni sulla sua \
         richiesta {label_role}. Il messaggio attuale e' la domanda meta — \
         NON considerarlo come 'ultima/prima richiesta'. Riferisciti al \
         corretto messaggio utente significativo nella cronologia (saltando \
         saluti, conferme brevi tipo 'ok'/'si'/'no'/'grazie' e altre \
         meta-domande auto-referenziali ricorsive)."
    );
    let current_short: String = current_message
        .replace('\n', " ")
        .chars()
        .take(80)
        .collect();
    match target {
        Some(example) => Some(format!(
            "\n\n{core_rule}\n\nEsempio tratto da questa conversazione: \
             il messaggio utente da citare e' \"{example}\". \
             Per una domanda come '{current_short}' la risposta corretta inizia con: \
             \"{label_example}: ...\" citando quel testo, NON il messaggio \
             attuale.",
        )),
        None => Some(format!("\n\n{core_rule}")),
    }
}

/// Salva l'embedding di un turno conversazionale in Qdrant (fire-and-forget).
fn spawn_embed_conversation_turn(
    neural: crate::orchestrator::NeuralCoreClient,
    db: PgPool,
    session_id: Uuid,
    message_id: Uuid,
    role: String,
    content: String,
) {
    tokio::spawn(async move {
        // Guard: se embedder/qdrant sono down, skip silenzioso (fire-and-forget)
        // Nota: usiamo una lettura globale rapida — nessun overhead di Arc qui.
        // Il DependencyStatus non e' disponibile in questa funzione standalone,
        // ma il fallback sotto (embed_text Err) gestisce gia' il caso.
        let embed_input = if content.len() > 1000 {
            content[..1000].to_string()
        } else {
            content.clone()
        };
        let vector = match neural.embed_text("", &embed_input).await {
            Ok(v) => {
                tracing::info!(
                    "conversation_embed: OK dim={} session={} role={} msg_id={}",
                    v.len(), session_id, role, message_id
                );
                v
            }
            Err(e) => {
                tracing::warn!("conversation_embed: FALLITO session={} role={} msg_id={}: {e}", session_id, role, message_id);
                return;
            }
        };
        let point_id = vector_memory::conversation_point_id(session_id, message_id);
        let now = chrono::Utc::now().to_rfc3339();
        if let Err(e) = vector_memory::upsert_conversation_turn(
            &db, &point_id, &vector, session_id, &role, &content, &now,
        ).await {
            tracing::warn!("conversation_upsert: FALLITO point={} session={}: {e}", point_id, session_id);
        } else {
            tracing::info!("conversation_upsert: OK point={} session={}", point_id, session_id);
        }
    });
}

/// Costruisce la conversation history usando una strategia ibrida:
/// ultimi `recent_count` messaggi raw (contesto immediato) + top-K
/// messaggi semanticamente rilevanti dalla collection Qdrant.
/// I risultati vengono deduplicati e ordinati cronologicamente.
async fn build_vectorized_conversation_history(
    db: &PgPool,
    neural: &crate::orchestrator::NeuralCoreClient,
    session_id: Uuid,
    current_message: &str,
    recent_count: i64,
    semantic_top_k: u64,
) -> Vec<serde_json::Value> {
    const RAW_FALLBACK: i64 = 8;

    let recent = build_recent_conversation_history(db, session_id, recent_count).await;

    // Costruzione dell'input di embedding: includiamo l'ULTIMA iterazione
    // (user+assistant) prima del messaggio corrente. Questo aggancia la
    // ricerca semantica al tema della conversazione, non solo al testo letterale
    // del turno corrente. Esempio: "si elenca" da solo matcha qualsiasi "elenca
    // X" passato; con il turno precedente ("quanti utenti / 4 utenti") il
    // vettore si concentra sul tema utenti.
    let mut embed_input = String::new();
    if let Some(last_turn) = recent.iter().rev().take(2).collect::<Vec<_>>().iter().rev().fold(
        Some(String::new()),
        |acc, msg| {
            let mut s = acc?;
            let role = msg.get("role")?.as_str()?;
            let content = msg.get("content")?.as_str()?;
            if !s.is_empty() { s.push('\n'); }
            s.push_str(role);
            s.push_str(": ");
            s.push_str(content);
            Some(s)
        },
    ) {
        if !last_turn.is_empty() {
            embed_input.push_str(&last_turn);
            embed_input.push('\n');
        }
    }
    embed_input.push_str("user: ");
    embed_input.push_str(current_message);
    if embed_input.len() > 1500 {
        // Truncate UTF-8 safe: cerca il char boundary piu' vicino sotto 1500
        // per evitare panic "assertion failed: self.is_char_boundary(new_len)"
        // se 1500 cade in mezzo a un byte multi-byte (accentate, emoji, ecc.).
        let mut cut = 1500;
        while cut > 0 && !embed_input.is_char_boundary(cut) {
            cut -= 1;
        }
        embed_input.truncate(cut);
    }
    let vector = match neural.embed_text("", &embed_input).await {
        Ok(v) => {
            tracing::warn!(
                "vectorized history: embed OK (con ultimo turno), dim={}, session={}, input_len={}",
                v.len(), session_id, embed_input.len()
            );
            v
        }
        Err(e) => {
            tracing::warn!("vectorized history: embedding fallito, fallback a {RAW_FALLBACK} raw: {e}");
            return build_recent_conversation_history(db, session_id, RAW_FALLBACK).await;
        }
    };

    let semantic_hits = match vector_memory::search_conversation_context(
        db, &vector, session_id, semantic_top_k, 0.40,
    ).await {
        Ok(hits) => {
            let scores: Vec<f64> = hits.iter().map(|h| h.score).collect();
            tracing::warn!(
                "vectorized history: ricerca Qdrant OK, {} hit(s) per session={}, scores={:?}",
                hits.len(), session_id, scores
            );
            hits
        }
        Err(e) => {
            tracing::warn!("vectorized history: ricerca Qdrant fallita, fallback a {RAW_FALLBACK} raw: {e}");
            return build_recent_conversation_history(db, session_id, RAW_FALLBACK).await;
        }
    };

    if semantic_hits.is_empty() {
        tracing::warn!("vectorized history: 0 hit semantici per session={}, fallback a {RAW_FALLBACK} raw", session_id);
        return build_recent_conversation_history(db, session_id, RAW_FALLBACK).await;
    }

    // Raccogli i contenuti recenti per deduplicazione
    let recent_contents: std::collections::HashSet<String> = recent.iter()
        .filter_map(|m| m.get("content").and_then(|v| v.as_str()).map(String::from))
        .collect();

    // Il timestamp del piu' vecchio dei recenti: tutto cio' che e' >= a questo
    // e' gia' coperto dai raw, quindi va escluso dai semantici per evitare
    // doppioni e per preservare l'ordine cronologico finale.
    let oldest_recent_ts: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar::<_, chrono::DateTime<chrono::Utc>>(
        r#"
        SELECT created_at FROM chat_messages
        WHERE session_id = $1 AND deleted_at IS NULL AND role IN ('user','assistant')
        ORDER BY created_at DESC
        OFFSET $2 LIMIT 1
        "#,
    )
    .bind(session_id)
    .bind((recent_count - 1).max(0))
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    // Converti hit semantici in messaggi, escludendo duplicati e quelli che
    // cadono nella finestra "recente" (gia' coperti dai raw).
    let mut semantic_msgs: Vec<(String, f64, serde_json::Value)> = Vec::new();
    for hit in &semantic_hits {
        let role = hit.payload.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = hit.payload.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let created_at = hit.payload.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        if content.is_empty() || recent_contents.contains(content) {
            continue;
        }
        // Filtra messaggi semantici che ricadono nella finestra recente
        if let Some(min_recent) = oldest_recent_ts {
            if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(created_at) {
                if ts.with_timezone(&chrono::Utc) >= min_recent {
                    continue;
                }
            }
        }
        let llm_role = if role == "assistant" { "assistant" } else { "user" };
        semantic_msgs.push((
            created_at.to_string(),
            hit.score,
            json!({ "role": llm_role, "content": content }),
        ));
    }

    if semantic_msgs.is_empty() {
        tracing::warn!("vectorized history: {} hit semantici tutti duplicati o nella finestra recente, fallback a {RAW_FALLBACK} raw", semantic_hits.len());
        return build_recent_conversation_history(db, session_id, RAW_FALLBACK).await;
    }

    // Ordina semantici per data ascendente (vecchio → nuovo) per coerenza cronologica.
    // La rilevanza semantica e' gia' stata applicata via threshold + top_k.
    semantic_msgs.sort_by(|a, b| a.0.cmp(&b.0));

    tracing::warn!(
        "vectorized history: combinazione finale: {} semantici (storico) + {} recenti (immediato) per session={}",
        semantic_msgs.len(), recent.len(), session_id
    );

    // Combina: semantici prima (contesto storico, ordine cronologico),
    // poi recenti (contesto immediato, ordine cronologico).
    // L'ULTIMO messaggio della lista e' la risposta assistant dell'ultimo
    // turno, posizionato direttamente prima del messaggio user corrente
    // gestito dal caller: questo garantisce che il LLM "veda" il contesto
    // immediato come elemento dominante per la risposta.
    let mut combined: Vec<serde_json::Value> = semantic_msgs.into_iter().map(|(_, _, m)| m).collect();
    combined.extend(recent);
    combined
}

fn summarize_title(content: &str) -> String {
    let normalized = content.replace('\n', " ").trim().to_string();
    if normalized.is_empty() {
        return "Nuova sessione".to_string();
    }
    if normalized.chars().count() <= 64 {
        return normalized;
    }
    normalized.chars().take(61).collect::<String>() + "..."
}

/// Mappa intent canonico → descrizione human-readable in italiano per
/// il messaggio di disambiguazione mostrato all'utente. Ritorna `None` per
/// intent sconosciuti (il caller usa il nome canonico come fallback).
fn intent_human_description(intent: &str) -> Option<&'static str> {
    Some(match intent {
        "chat" => "rispondere con una spiegazione testuale",
        "debug" => "analizzare l'errore o il fallimento per trovare la causa radice",
        "fix" => "applicare una correzione mirata a un bug noto",
        "refactor" => "riorganizzare il codice senza cambiarne il comportamento",
        "test" => "scrivere o migliorare test (nuovi casi di test)",
        "docs" => "scrivere/aggiornare documentazione",
        "code_read" => "leggere ed esaminare file di codice esistenti",
        "architecture" => "fare design o pianificare una migrazione",
        "file_ops" => "creare/eliminare/spostare file",
        "system_admin" => "configurare servizi, utenti o deploy",
        _ => return None,
    })
}

/// Costruisce il messaggio di disambiguazione mostrato all'utente quando
/// il classifier non e' sicuro tra 2+ intent plausibili. Lista le opzioni
/// con etichetta (A/B/C) per facilitare la risposta.
fn build_disambiguation_message(c: &crate::orchestrator::ClassifiedIntent) -> String {
    let mut s = String::from(
        "Per dare la risposta giusta ho bisogno di un chiarimento — la tua richiesta puo' \
         essere interpretata in piu' modi. Quale di queste opzioni descrive meglio cosa vuoi?\n\n"
    );
    let labels = ["A", "B", "C"];
    for (idx, cand) in c.candidates.iter().take(3).enumerate() {
        let desc = intent_human_description(&cand.intent).unwrap_or(cand.intent.as_str());
        s.push_str(&format!(
            "**{}.** {} (intent: `{}`, confidence {:.0}%)\n",
            labels[idx],
            desc,
            cand.intent,
            cand.confidence * 100.0,
        ));
    }
    s.push_str(
        "\nRispondi indicando la lettera (es. \"A\") oppure descrivi piu' precisamente \
         cosa vuoi che faccia. Se preferisci che proceda senza chiedere, imposta la \
         modalita' di automazione su \"Automatico\"."
    );
    s
}

async fn insert_message(
    db: &PgPool,
    session_id: Uuid,
    project_id: Uuid,
    role: &str,
    content: &str,
    metadata: Value,
    request_message_id: Option<Uuid>,
) -> Result<Uuid, ApiError> {
    let message_id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO chat_messages (
            id, session_id, project_id, role, content, metadata, request_message_id, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())
        "#,
    )
    .bind(message_id)
    .bind(session_id)
    .bind(project_id)
    .bind(role)
    .bind(content)
    .bind(metadata)
    .bind(request_message_id)
    .execute(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(message_id)
}

async fn load_message_by_id(
    db: &PgPool,
    message_id: Uuid,
) -> Result<sqlx::postgres::PgRow, ApiError> {
    sqlx::query(
        r#"
        SELECT
            m.id,
            m.session_id,
            m.project_id,
            m.role,
            m.content,
            m.metadata,
            m.request_message_id,
            m.deleted_at,
            m.created_at
        FROM chat_messages m
        WHERE m.id = $1
        "#,
    )
    .bind(message_id)
    .fetch_one(db)
    .await
    .map_err(|_| api_error(StatusCode::NOT_FOUND, "Messaggio non trovato"))
}

async fn run_turn(
    state: &AppState,
    user_id: Uuid,
    session_id: Uuid,
    project_id: Uuid,
    profile_id: String,
    user_content: String,
    request_message_id: Uuid,
    active_files: Vec<String>,
    system_context: Option<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
    automation_mode: AutomationMode,
    attachments: Vec<ChatAttachment>,
) -> Result<(ChatMessageView, OrchestratorResult), ApiError> {
    let enriched_message = match system_context {
        Some(ctx) if !ctx.is_empty() => format!("{ctx}\n\n{user_content}"),
        _ => user_content.clone(),
    };
    let orchestrator_output = state
        .orchestrator
        .run(
            &state.db,
            OrchestratorRequest {
                user_id: user_id.to_string(),
                project_id: project_id.to_string(),
                profile_id,
                message: enriched_message,
                active_files,
                session_id: Some(session_id.to_string()),
                request_message_id: Some(request_message_id.to_string()),
                provider_override,
                model_override,
                automation_mode,
                attachments,
            },
        )
        .await
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?;

    let payload = &orchestrator_output.payload;
    let raw_content = payload["completion"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // Se il gateway ha re-instradato la richiesta su provider locale per privacy:
    // 1. Azzerare la preferenza di sessione → al prossimo msg si torna al routing automatico
    // 2. Anteporre una nota informativa alla risposta
    let assistant_content = if let Some(pr) = payload["completion"]["privacy_rerouted"].as_object() {
        let provider = pr.get("provider").and_then(|v| v.as_str()).unwrap_or("locale");
        let tier = pr.get("blocked_tier").and_then(|v| v.as_u64()).unwrap_or(0);
        // Azzera la preferenza di sessione
        clear_session_preferred_provider_after_privacy(&state.db, session_id).await;
        format!(
            "> **Privacy attiva** — contenuto sensibile rilevato (livello {tier}). \
             Risposta generata dal modello locale `{provider}` per proteggere i tuoi dati.\n\n{}",
            raw_content
        )
    } else {
        raw_content
    };
    let run_id = payload["run_id"].as_str().unwrap_or("").to_string();
    let metadata = json!({
        "provider": payload["provider"].as_str().unwrap_or(""),
        "model": payload["model"].as_str().unwrap_or(""),
        "intent": payload["intent"].as_str().unwrap_or("chat"),
        "runId": run_id,
        "promptTokens": payload["prompt_tokens"].as_i64().unwrap_or(0),
        "completionTokens": payload["completion_tokens"].as_i64().unwrap_or(0),
        "totalTokens": payload["total_tokens"].as_i64().unwrap_or(0),
        "totalCost": payload["total_cost"].as_f64().unwrap_or(0.0),
        "currency": payload["currency"].as_str().unwrap_or("EUR"),
        "automationMode": payload["automation_mode"].as_str().unwrap_or("confirm"),
    });

    let assistant_id = insert_message(
        &state.db,
        session_id,
        project_id,
        "assistant",
        &assistant_content,
        metadata,
        Some(request_message_id),
    )
    .await?;

    nexus_events::dispatcher::emit(
        &state.project_channels,
        project_id,
        nexus_events::ProjectEvent::ChatMessageAdded {
            session_id,
            message_id: assistant_id,
            role: "assistant".into(),
            total_tokens: payload["total_tokens"].as_i64(),
            total_cost_usd: payload["total_cost"].as_f64(),
        },
    );

    sqlx::query(
        r#"
        UPDATE chat_sessions
        SET updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(session_id)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let row = load_message_by_id(&state.db, assistant_id).await?;
    let view = to_message_view(&row)?;
    Ok((view, orchestrator_output))
}

pub async fn list_chat_messages(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(session_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&session_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Session id non valido"))?;
    let context = load_session_context(&state.db, session_id, user_id).await?;

    let rows = sqlx::query(
        r#"
        SELECT id, session_id, project_id, role, content, metadata, request_message_id, deleted_at, created_at
        FROM chat_messages
        WHERE session_id = $1
        ORDER BY created_at ASC
        "#,
    )
    .bind(context.session_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut messages = Vec::with_capacity(rows.len());
    for row in &rows {
        messages.push(to_message_view(row)?);
    }

    Ok(Json(json!({
        "sessionId": context.session_id.to_string(),
        "projectId": context.project_id.to_string(),
        "messages": messages
    })))
}

/// Parametri condivisi per avviare un agent run (usato da send e resend).
struct SpawnAgentParams {
    user_id: Uuid,
    session_id: Uuid,
    project_id: Uuid,
    user_message_id: Uuid,
    content: String,
    automation_mode: AutomationMode,
    supervisor_mode: SupervisorMode,
    profile_prompt_block: String,
    system_context: String,
    provider_override: Option<String>,
    model_override: Option<String>,
    profile_provider: Option<String>,
    profile_model: Option<String>,
    attachments: Vec<ChatAttachment>,
    /// Ruolo utente JWT (es. "admin", "editor") — per i tool nexus_builtin
    user_role: String,
    /// Agent type hint dal client (bypassa Q-Learning se presente)
    nexus_agent_type_hint: Option<String>,
}

/// Risultato di spawn_agent_run: (run_id, provider, model)
struct SpawnAgentResult {
    run_id: Uuid,
    provider: String,
    model: String,
}

/// Logica condivisa: carica progetto, costruisce contesto, avvia AgentLoop in background.
/// Ritorna `None` se il progetto non è caricabile (fallback al singolo turn).
async fn spawn_agent_run(
    state: &AppState,
    params: SpawnAgentParams,
) -> Option<SpawnAgentResult> {
    let project_ctx = load_project_context(&state.db, params.project_id, params.user_id).await;
    let proj = match project_ctx {
        Ok(p) => p,
        Err(_) => return None,
    };

    let run_id = Uuid::new_v4();
    let (tx, _rx) = broadcast::channel::<AgentStepEvent>(256);
    state.agent_channels.insert(run_id, tx.clone());

    // ─────────────────────────────────────────────────────────────────
    // Disambiguation step (best practice NLU)
    // ─────────────────────────────────────────────────────────────────
    // Se il classifier marca il task come ambiguo (top confidence < 0.70
    // oppure margine < 0.15 sul secondo candidato) E l'utente NON e' in
    // modalita' "automatic", inseriamo un messaggio assistant che chiede
    // chiarimenti invece di indovinare. Riferimento: Rasa/Dialogflow/LUIS.
    //
    // Modalita' automatic salta la disambiguazione: l'utente vuole che il
    // sistema agisca anche con incertezza moderata (top candidato vince).
    // Arricchiamo il messaggio passato al classifier con un breve contesto
    // dei turni recenti: il classifier LLM NON ha accesso autonomo alla
    // cronologia, quindi senza questo prefisso messaggi tipo "riepiloga
    // animali" o "applica tutte le ultime" verrebbero marcati ambigui
    // ("messaggio troppo generico"). L'originale resta invariato per il
    // resto del flusso (`params.content`).
    let classifier_input = build_message_with_recent_context_for_classifier(
        &state.db,
        params.session_id,
        &params.content,
    ).await;
    let classified = state
        .orchestrator
        .classify_intent_full(&classifier_input)
        .await;
    if classified.is_ambiguous
        && !matches!(params.automation_mode, AutomationMode::Automatic)
    {
        tracing::info!(
            "spawn_agent_run: intent ambiguo (conf={:.2}, candidati={}), chiedo disambiguazione",
            classified.confidence, classified.candidates.len(),
        );
        let disambig_msg = build_disambiguation_message(&classified);
        let meta = json!({
            "kind": "disambiguation_request",
            "intent": classified.intent,
            "confidence": classified.confidence,
            "candidates": classified.candidates,
        });
        let _ = insert_message(
            &state.db,
            params.session_id,
            params.project_id,
            "assistant",
            &disambig_msg,
            meta,
            Some(params.user_message_id),
        )
        .await;
        // Rimuoviamo il canale broadcast: non avviamo l'agent run.
        state.agent_channels.remove(&run_id);
        return None;
    }

    // ─────────────────────────────────────────────────────────────────
    // Routing slot-based (Livello 4 NLU, mig 0133)
    // ─────────────────────────────────────────────────────────────────
    // Prima del routing classico (intent, behavior_mode), proviamo la
    // matrice slots: e' piu' precisa perche' indicizzata su 4 slot
    // canonici (action_verb, target_type, framework, scope) estratti
    // dal classifier LLM. Se nessun match O slots incompleti, cadiamo
    // sul routing classico testato. Soglia confidence: 0.60.
    //
    // Safety-net: se il classifier LLM non ha estratto slot (es. JSON
    // parse fail con Gemini Flash) ma il messaggio chiaramente descrive
    // una "test failure resolution" via keyword detection, ricostruiamo
    // slots minimi euristicamente per non perdere il routing capable.
    let effective_slots = if classified.slots.is_complete() {
        classified.slots.clone()
    } else {
        crate::routing_slots::infer_slots_heuristic(&params.content)
    };
    let slot_routing_hit = if params.provider_override.is_none()
        && params.model_override.is_none()
    {
        state.orchestrator.route_by_slots(&effective_slots, 0.60).await
    } else {
        None
    };

    // Routing intelligente: Neural Core classifica l'intent e sceglie il provider ottimale
    // (es. "fix" → anthropic, "chat" → openai, ecc.) invece di usare sempre il primo in lista.
    // Il profile_provider ha priorità sul routing automatico, ma non sul provider_override utente.
    let effective_override = if let Some((slot_provider, _slot_model, _src)) = &slot_routing_hit {
        // Slot routing ha vinto: forziamo il provider scelto come override
        // (il modello viene applicato sotto, dopo il routing classico, sovrascrivendolo).
        Some(slot_provider.clone())
    } else {
        params
            .provider_override
            .filter(|v| !v.trim().is_empty())
            .or_else(|| params.profile_provider.filter(|v| !v.trim().is_empty()))
    };
    let effective_model_override = if let Some((_slot_provider, slot_model, _src)) = &slot_routing_hit {
        Some(slot_model.clone())
    } else {
        params
            .model_override
            .filter(|v| !v.trim().is_empty())
            .or_else(|| params.profile_model.filter(|v| !v.trim().is_empty()))
    };
    if let Some((p, m, src)) = &slot_routing_hit {
        tracing::info!(
            "spawn_agent_run: routing slot-based {} → {}/{}",
            src, p, m
        );
    }

    // Conta i messaggi esistenti nella sessione per calibrare il routing:
    // sessioni con molti messaggi indicano task lunghi (es. "continua") che
    // richiedono modelli più capaci anche se il messaggio è breve.
    let context_message_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM chat_messages WHERE session_id = $1",
    )
    .bind(params.session_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0) as usize;

    // Versione "detailed": ritorna anche `no_capable_provider` e
    // `providers_in_cooldown`. Se nessun provider e' utilizzabile fermiamo
    // il run prima di chiamare il brain — emettiamo invece un evento SSE
    // `provider_unavailable` che la UI consuma per mostrare un banner.
    let routing_result = state
        .orchestrator
        .resolve_agent_provider_detailed(
            &state.db,
            &params.project_id.to_string(),
            "",
            &params.content,
            effective_override.as_deref(),
            effective_model_override.as_deref(),
            context_message_count,
            None, // behavior_mode_session: nessun override per il pre-check routing
        )
        .await;

    let provider = routing_result.provider.clone();
    let model_str = routing_result.model.clone();

    // Stop & alert se nessun provider capable: niente run, niente brain call.
    if routing_result.no_capable_provider {
        let providers_list = routing_result.providers_in_cooldown.join(", ");
        let alert_msg = if providers_list.is_empty() {
            "Nessun provider AI configurato disponibile. Verifica le API key in admin.".to_string()
        } else {
            format!(
                "Tutti i provider AI configurati sono in cooldown (quota/credito esaurito): {}. \
                 Aggiungi credito o aspetta il reset, poi riprova.",
                providers_list,
            )
        };
        tracing::warn!(
            "spawn_agent_run: no_capable_provider per session={} → STOP + alert. {}",
            params.session_id, alert_msg,
        );
        // Persist run come "failed" con errore strutturato.
        let _ = sqlx::query(
            r#"INSERT INTO agent_runs
               (id, session_id, project_id, user_id, run_message_id, status,
                automation_mode, provider, model, supervisor_mode, iteration_count, error, created_at)
               VALUES ($1,$2,$3,$4,$5,'failed',$6,$7,$8,$9,0,$10,NOW())"#,
        )
        .bind(run_id)
        .bind(params.session_id)
        .bind(params.project_id)
        .bind(params.user_id)
        .bind(params.user_message_id)
        .bind(params.automation_mode.as_str())
        .bind(&provider)
        .bind(&model_str)
        .bind(params.supervisor_mode.as_str())
        .bind(&alert_msg)
        .execute(&state.db)
        .await;
        // Emit evento SSE con status `provider_unavailable`. La UI lo intercetta
        // (vedi chat-panel.tsx) per mostrare il banner rosso.
        let alert_step = AgentStep {
            run_id: run_id.to_string(),
            step_index: 0,
            tool_name: String::new(),
            tool_input: serde_json::json!({
                "providers_in_cooldown": routing_result.providers_in_cooldown,
                "rationale": routing_result.rationale,
            }),
            tool_result: Some(alert_msg.clone()),
            status: AgentStepStatus::ProviderUnavailable,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        let _ = tx.send(AgentStepEvent {
            run_id: run_id.to_string(),
            step: Some(alert_step),
            trace: None,
            is_final: true,
            token_delta: None,
            meta_step: None,
        });
        return Some(SpawnAgentResult {
            run_id,
            provider,
            model: model_str,
        });
    }

    // Persist initial run in DB
    let _ = sqlx::query(
        r#"INSERT INTO agent_runs
           (id, session_id, project_id, user_id, run_message_id, status,
            automation_mode, provider, model, supervisor_mode, iteration_count, created_at)
           VALUES ($1,$2,$3,$4,$5,'running',$6,$7,$8,$9,0,NOW())"#,
    )
    .bind(run_id)
    .bind(params.session_id)
    .bind(params.project_id)
    .bind(params.user_id)
    .bind(params.user_message_id)
    .bind(params.automation_mode.as_str())
    .bind(&provider)
    .bind(&model_str)
    .bind(params.supervisor_mode.as_str())
    .execute(&state.db)
    .await;

    // Il loop agente gira integralmente nel brain LangGraph (Python): qui
    // serve solo il Sender broadcast per ri-emettere gli eventi SSE.
    let tx_for_brain = tx.clone();
    // Consumato dal tokio::spawn sotto per non lasciare dangling clone.
    drop(tx);

    // History ibrida: ultimi 4 raw (2 turni completi) + top-6 semanticamente
    // rilevanti da Qdrant. L'embedding di ricerca include l'ULTIMO turno
    // user+assistant insieme al messaggio corrente, cosi' la ricerca
    // semantica si aggancia al tema della conversazione e non al solo testo
    // letterale del turno corrente. I semantici che ricadono nella finestra
    // recente vengono filtrati per evitare duplicazione.
    // Se Qdrant/embedding non disponibile, fallback a ultimi 8 raw.
    let vec_deps_ok = state.dependency_status.qdrant.load(std::sync::atomic::Ordering::Relaxed)
        && state.dependency_status.embedder.load(std::sync::atomic::Ordering::Relaxed);
    let recent_history = if vec_deps_ok {
        build_vectorized_conversation_history(
            &state.db,
            &state.orchestrator.neural,
            params.session_id,
            &params.content,
            4,  // ultimi 4 messaggi raw = 2 turni completi user+assistant
            6,  // top-6 semantici dalla storia piu' vecchia (soglia 0.40)
        ).await
    } else {
        // Dipendenze vettoriali down: usa solo gli ultimi messaggi raw
        build_recent_conversation_history(&state.db, params.session_id, 8).await
    };
    // Versione testuale compatta solo per logging
    let recent_context =
        build_recent_conversation_context(&state.db, params.session_id, 4).await;
    // Legge analysis_json + custom_instructions in un'unica query
    let (analysis_json_opt, custom_instructions_opt): (Option<serde_json::Value>, Option<String>) =
        sqlx::query_as::<_, (Option<serde_json::Value>, Option<String>)>(
            "SELECT analysis_json, custom_instructions FROM projects WHERE id = $1",
        )
        .bind(params.project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or((None, None));

    let analysis_summary: Option<String> = analysis_json_opt.and_then(|analysis: serde_json::Value| {
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
        let frameworks = analysis
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
        let scripts = analysis
            .get("dependencies")
            .and_then(|d| d.get("node"))
            .and_then(|n| n.get("scripts"))
            .and_then(|s| s.as_object())
            .map(|scripts_map| {
                scripts_map
                    .iter()
                    .take(8)
                    .map(|(k, v)| format!("  {} → {}", k, v.as_str().unwrap_or("")))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if langs.is_empty() && frameworks.is_empty() {
            None
        } else {
            let mut summary = format!("Linguaggi: {}\nFramework/stack: {}", langs, frameworks);
            if !scripts.is_empty() {
                summary.push_str(&format!("\nScript disponibili:\n{}", scripts));
            }
            Some(summary)
        }
    });

    let db_connections_block = {
        let rows = sqlx::query(
            "SELECT name, engine, ENCODE(connection_secret, 'escape') AS connection_secret, is_primary \
             FROM project_database_config \
             WHERE project_id = $1 \
             ORDER BY is_primary DESC, LOWER(name)"
        )
        .bind(params.project_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        if rows.is_empty() {
            String::new()
        } else {
            let mut block = String::from("\nDatabase configurati (usa questi per connetterti, NON chiedere credenziali all'utente):\n");
            for r in &rows {
                let name: String = r.try_get("name").unwrap_or_default();
                let engine: Option<String> = r.try_get("engine").unwrap_or(None);
                let dsn: Option<String> = r.try_get("connection_secret").unwrap_or(None);
                let primary: bool = r.try_get("is_primary").unwrap_or(false);
                let label = if primary { " [PRIMARY]" } else { "" };
                if let Some(ref dsn_val) = dsn {
                    block.push_str(&format!(
                        "  - {}{}: engine={} connection_string=\"{}\"\n",
                        name, label,
                        engine.as_deref().unwrap_or("unknown"),
                        dsn_val
                    ));
                } else {
                    block.push_str(&format!(
                        "  - {}{}: engine={} (nessuna connection string configurata)\n",
                        name, label,
                        engine.as_deref().unwrap_or("unknown")
                    ));
                }
            }
            block
        }
    };

    let project_header = if let Some(ref summary) = analysis_summary {
        format!(
            "=== CONTESTO PROGETTO (non chiedere queste informazioni: sono gia' qui) ===\n\
             Progetto: {} | Root: {} | Git: {}\n\
             {}{}\n\
             === FINE CONTESTO PROGETTO ===\n\n",
            proj.details.name,
            proj.repository_root_path.display(),
            if proj.is_git_repo { "si" } else { "no" },
            summary,
            db_connections_block
        )
    } else {
        format!(
            "=== CONTESTO PROGETTO ===\n\
             Progetto: {} | Root: {} | Git: {}{}\n\
             (Nessuna analisi disponibile: usa list_files per esplorare la struttura)\n\
             === FINE CONTESTO PROGETTO ===\n\n",
            proj.details.name,
            proj.repository_root_path.display(),
            if proj.is_git_repo { "si" } else { "no" },
            db_connections_block
        )
    };

    // Istruzioni specifiche per modalità automazione
    let automation_instructions = match params.automation_mode {
        AutomationMode::Automatic => "\n=== MODALITÀ AUTOMATICA ===\n\
            Sei in modalità AUTOMATICA. Regole assolute:\n\
            1. NON chiedere mai conferma prima di eseguire operazioni (modifica file, esecuzione comandi, ecc.)\n\
            2. NON chiedere \"Vuoi che proceda?\", \"Posso continuare?\", \"Devo modificare?\"\n\
            3. Esegui direttamente tutte le azioni necessarie senza interruzioni\n\
            4. Se hai dubbi su un approccio, scegli quello più ragionevole e procedi\n\
            5. Per ogni modifica a un file: PRIMA leggi la sezione esatta con read_file_lines, POI usa edit_file con old_string di almeno 5 righe di contesto, POI verifica subito con run_command (build/typecheck/lint) che non ci siano errori sintattici.\n\
            5b. Se edit_file ritorna 'old_string non trovato': l'errore include GIA' le prime 80 righe del file. Confronta direttamente — NON chiamare read_file_lines (bloccato dal loop-detector se gia' usato). Se la sezione non e' nelle prime 80 righe, usa read_file_lines con start_line/end_line DIVERSI da quelli precedenti.\n\
            6. Se il build fallisce dopo un edit_file: leggi SUBITO l'errore, identifica quale modifica ha causato il problema, e correggila prima di procedere con altri edit.\n\
            7. Alla fine, VERIFICA il lavoro svolto con run_command (build completo) per confermare che tutto compili senza errori.\n\
            8. Concludi SEMPRE con un messaggio che riporta il risultato della verifica finale (build OK / errori rimasti).\n\
            === FINE MODALITÀ AUTOMATICA ===\n",
        AutomationMode::Confirm => "\n=== MODALITÀ CONFERMA ===\n\
            Prima di modificare file o eseguire comandi, descrivi il piano.\n\
            NON chiedere \"Confermo?\" o \"Procedo?\" come messaggio testuale — procedi direttamente con le operazioni.\n\
            Il sistema mostrerà automaticamente i bottoni Approva/Annulla all'utente per ogni write_file o comando.\n\
            NON aspettare risposta testuale: esegui subito le azioni, la conferma avverrà tramite UI.\n\
            === FINE MODALITÀ CONFERMA ===\n",
        AutomationMode::Study => "",
    };

    // Istruzioni TDD per cicli test-fix-test iterativi
    let test_instructions = {
        let l = params.content.to_lowercase();
        let is_test_intent = l.contains("test") || l.contains("testa")
            || l.contains("verifica che funzion") || l.contains("tdd")
            || l.contains("fai passare");
        if is_test_intent {
            "\n=== MODALITA TEST-FIX-TEST ===\n\
            Stai lavorando in modalita' iterativa di test. Regole:\n\
            1. Usa il tool `run_tests` (NON `run_command`) per eseguire i test\n\
            2. Analizza i fallimenti UNO alla volta: identifica l'errore piu' critico\n\
            3. Correggi UN SOLO problema per volta, poi ri-esegui `run_tests`\n\
            4. Se lo stesso test fallisce 3 volte con lo stesso errore, FERMATI e chiedi all'utente\n\
            5. Procedi incrementalmente — non correggere tutto insieme\n\
            6. Dopo ogni fix, spiega brevemente cosa hai cambiato e perche'\n\
            7. Hai massimo 7 esecuzioni test per sessione — usale con giudizio\n\
            8. Se i test passano tutti, concludi con un riepilogo delle modifiche effettuate\n\
            9. Per eseguire test specifici, usa il parametro 'filter' di run_tests\n\
            === FINE MODALITA TEST ===\n"
        } else {
            ""
        }
    };

    // Istruzioni specifiche per-progetto (auto-generate da analyze_project o modificate manualmente)
    let project_custom_instructions = custom_instructions_opt
        .filter(|s| !s.trim().is_empty())
        .map(|s| format!("\n{}\n", s))
        .unwrap_or_default();

    // Iniezione istruzione "precedente significativo": quando l'utente fa
    // una domanda meta auto-referenziale ("qual era l'ultima richiesta?",
    // "ripeti l'ultimo"), il LLM rischia di interpretare letteralmente
    // l'ultimo messaggio (la domanda stessa) invece di scalare al precedente
    // messaggio utente significativo. L'hint e' auto-aggiornato: include un
    // few-shot example tratto dalla cronologia reale di questa sessione.
    let self_ref_hint = build_self_referential_hint(
        &state.db,
        params.session_id,
        &params.content,
    ).await.unwrap_or_default();

    // Istruzioni specifiche per modelli o-series (o1/o3/o4-mini): forzano
    // l'uso esplicito dei tool instead of narrare le azioni come testo.
    let o_series_instructions = if crate::brain_agent_client::is_o_series_model_pub(&model_str) {
        "\n=== ISTRUZIONI TOOL (MODELLO REASONING) ===\n\
            REGOLA CRITICA: Devi SEMPRE usare i tool per eseguire azioni. Non narrare mai le azioni come testo.\n\
            - Per creare/modificare file: usa write_file o edit_file (NON scrivere il contenuto come testo nella risposta)\n\
            - Per eseguire comandi: usa run_command (NON descrivere cosa faresti)\n\
            - Per leggere file: usa read_file o read_file_lines (NON immaginare il contenuto)\n\
            - Per cercare: usa search_in_files o search_codebase_semantic\n\
            Hai un set essenziale di tool disponibili. Se hai bisogno di un tool non presente (es. git_push, \
            run_playwright_tests, service-related), usa nexus_mcp_tool_search per cercarlo e nexus_mcp_tool_call \
            per eseguirlo.\n\
            VIETATO: rispondere con codice inline senza tool call. Ogni riga di codice DEVE passare da write_file/edit_file.\n\
            === FINE ISTRUZIONI TOOL ===\n"
    } else {
        ""
    };

    let system_text = format!(
        "{}{}{}{}{}{}{}{}", project_header, project_custom_instructions,
        automation_instructions, o_series_instructions, test_instructions,
        params.profile_prompt_block, params.system_context, self_ref_hint
    );
    // Il messaggio iniziale è solo il contenuto corrente (senza prefisso testuale)
    // La history recente viene passata come turns strutturati via resume_history
    let initial_msg = params.content.clone();

    tracing::warn!(
        "TOKEN_OPT: system_text_len={} initial_msg_len={} recent_ctx_len={} history_turns={}",
        system_text.len(),
        initial_msg.len(),
        recent_context.len(),
        recent_history.len(),
    );

    let db_clone = state.db.clone();
    let channels_clone = state.agent_channels.clone();
    let session_id_cp = params.session_id;
    let project_id_cp = params.project_id;
    let user_message_id = params.user_message_id;
    // Cattura il provider che era stato impostato come preferenza di sessione.
    // Se al termine del run il gateway ha usato un provider locale diverso (vllm),
    // significa che è avvenuto un re-routing privacy → azzeriamo la preferenza.
    let requested_provider_clone = provider.clone();
    let had_session_override = effective_override.is_some();

    // Fase 4 del refactor Nexus: il loop agente gira sempre nel brain
    // LangGraph (Python). Non c'e' piu' un path AgentLoop locale.
    let provider_clone = provider.clone();
    let model_clone = model_str.clone();
    let initial_msg_clone = initial_msg.clone();
    let system_text_clone = system_text.clone();
    // Clono la routing matrix cache per il loop di fallback dentro lo spawn
    // (non posso catturare `state: &AppState` con lifetime locale dentro
    // `tokio::spawn(async move {...})` che richiede 'static).
    let routing_matrix_for_loop = state.orchestrator.routing_matrix.clone();
    let neural_for_embed = state.orchestrator.neural.clone();
    let recent_history_for_brain = recent_history;
    // L'intent classificato pilota la decisione di retry "hollow completion":
    // per chat/docs e' normale che il modello risponda senza tool, NON e' un
    // bug del modello e non giustifica un fallback su un modello piu' costoso.
    // `classified.intent` e' `&'static str` quindi e' Copy: nessun clone serve.
    let classified_intent_for_loop: &'static str = classified.intent;
    // Modalita' automazione propagata al brain (per clarify_or_expand skip).
    let automation_mode_for_brain: String = params.automation_mode.as_str().to_string();

    // Calcola il payload tools dinamico (discovery mode vs inline) prima dello spawn.
    // Il filtering per automation_mode avviene dentro build_tools_json_for_agent:
    // in `study` esporta solo tool read-only (gating difensivo), in `confirm` e
    // `automatic` esporta la lista completa.
    let tools_json_for_brain = crate::brain_agent_client::build_tools_json_for_agent(
        &state.db,
        params.user_id,
        params.project_id,
        &params.automation_mode,
        &provider,
        &model_str,
    )
    .await;

    // Lettura della soglia SSE silence da settings (mig 0132). Cache 60s
    // tramite RoutingThresholdsCache: la doppia chiamata e' gratis.
    // Fallback al default tecnico (120s) se DB non disponibile.
    let sse_max_silence_secs: u64 = match state.orchestrator.routing_thresholds.current_async().await {
        Ok(t) => t.sse_heartbeat_max_silence_secs,
        Err(_) => 120,
    };

    // Cloni dedicati al panic-handler: se il corpo principale del tokio::spawn
    // panica, dobbiamo comunque emettere is_final e marcare il run come failed
    // nel DB. Senza questi cloni esterni, il `move` cattura tutto e il branch
    // di recovery non avrebbe accesso ai canali/DB. (Garanzia anti-blocco UI.)
    let panic_tx = tx_for_brain.clone();
    let panic_db = db_clone.clone();
    let panic_channels = channels_clone.clone();
    let panic_run_id = run_id;
    let panic_session_id = session_id_cp;
    let panic_project_id = project_id_cp;
    let panic_user_msg_id = user_message_id;

    tokio::spawn(async move {
        use futures::FutureExt;

        tracing::info!(
            "spawn_agent_run: delega al brain LangGraph run_id={}",
            run_id
        );

        let agent_body = std::panic::AssertUnwindSafe(async move {
        // ── Loop di retry con fallback automatico tra provider ───────────────
        // Se il run fallisce per "credit too low" / "quota exceeded", il provider
        // viene messo in cooldown lungo (in brain_agent_client). Qui rileviamo
        // il fallimento e ritentiamo con il prossimo provider della gerarchia
        // ammin (escludendo quelli in cooldown).
        //
        // Limite dinamico: tante iterazioni quanti sono i provider con almeno
        // un modello idoneo nel catalog (is_enabled + supports_tool_use +
        // consecutive_failures=0). Il +1 copre il tentativo iniziale. Floor=2
        // per garantire almeno un fallback se il catalog e' parziale.
        let max_provider_fallbacks: usize = {
            let n: i64 = sqlx::query_scalar(
                "SELECT COUNT(DISTINCT provider)
                   FROM ai_price_catalog
                  WHERE is_enabled = true
                    AND supports_tool_use = true
                    AND consecutive_failures = 0"
            )
            .fetch_one(&db_clone)
            .await
            .unwrap_or(4);
            std::cmp::max(2, (n as usize).saturating_add(1))
        };
        let provider_hierarchy: Vec<String> = {
            let row: Option<String> = sqlx::query_scalar(
                "SELECT value FROM settings WHERE key = 'provider_hierarchy' LIMIT 1"
            )
            .fetch_optional(&db_clone)
            .await
            .ok()
            .flatten();
            row.map(|s| s.split(',').map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()).collect())
                .unwrap_or_else(|| vec![
                    "anthropic".into(), "openai".into(), "google".into(),
                    "deepseek".into(), "mistral".into(),
                ])
        };

        let mut current_provider = provider_clone.clone();
        let mut current_model    = model_clone.clone();
        let mut tried: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut result;
        let mut fallback_attempt: usize = 0;

        // ── Fix B+C: stima tokens richiesti e scelta context-aware ──────────
        // Approssimazione (1 token = ~4 caratteri): system prompt + msg utente
        // + storia conversazione + descrizioni tool. Usata per:
        //   B) troncare history se eccede 70% ctx del modello selezionato
        //   C) pre-filtrare il routing escludendo modelli con ctx insufficiente
        let estimated_input_chars: usize = {
            let history_chars: usize = recent_history_for_brain.iter()
                .map(|m| m.get("content").and_then(|c| c.as_str()).map(|s| s.len()).unwrap_or(0))
                .sum();
            let tools_chars: usize = serde_json::to_string(&tools_json_for_brain)
                .map(|s| s.len()).unwrap_or(0);
            system_text_clone.len() + initial_msg_clone.len() + history_chars + tools_chars
        };
        let estimated_input_tokens: i64 = (estimated_input_chars / 4) as i64;
        tracing::info!(
            "agent_run {}: input stimato {} tokens (~{} chars)",
            run_id, estimated_input_tokens, estimated_input_chars
        );
        // Se il modello primario non ha context_window sufficiente (con margine
        // 30% per output), cerca subito un modello idoneo per ctx.
        let primary_ctx: i64 = sqlx::query_scalar(
            "SELECT context_window FROM ai_price_catalog WHERE provider=$1 AND model=$2 LIMIT 1"
        )
        .bind(&current_provider)
        .bind(&current_model)
        .fetch_optional(&db_clone)
        .await
        .ok().flatten().unwrap_or(8192);
        let ctx_needed: i64 = (estimated_input_tokens as f64 * 1.3) as i64;
        if primary_ctx < ctx_needed {
            tracing::warn!(
                "agent_run {}: ctx insufficiente per primario {}/{} ({} < {}), cerco modello con ctx >= {}",
                run_id, current_provider, current_model, primary_ctx, ctx_needed, ctx_needed
            );
            let alt: Option<(String, String)> = sqlx::query_as::<_, (String, String)>(
                "SELECT provider, model FROM ai_price_catalog
                  WHERE is_enabled=true AND supports_tool_use=true
                    AND consecutive_failures=0
                    AND context_window >= $1
                  ORDER BY input_cost_per_million_tokens ASC NULLS LAST
                  LIMIT 1"
            )
            .bind(ctx_needed)
            .fetch_optional(&db_clone)
            .await
            .ok().flatten();
            if let Some((p, m)) = alt {
                tracing::info!(
                    "agent_run {}: routing context-aware: {} -> {}/{}",
                    run_id, current_model, p, m
                );
                current_provider = p;
                current_model = m;
            }
        }

        loop {
            tried.insert(current_provider.to_lowercase());
            tracing::info!(
                "agent_run {}: tentativo {}/{} con provider={} model={} (ctx_needed={})",
                run_id, fallback_attempt + 1, max_provider_fallbacks, current_provider, current_model, ctx_needed
            );
            result = crate::brain_agent_client::run_via_brain(
                run_id,
                session_id_cp,
                current_provider.clone(),
                current_model.clone(),
                system_text_clone.clone(),
                initial_msg_clone.clone(),
                tx_for_brain.clone(),
                recent_history_for_brain.clone(),
                tools_json_for_brain.clone(),
                sse_max_silence_secs,
                false, // emit_final_event: emesso manualmente dopo il break del retry loop
                automation_mode_for_brain.clone(),
            )
            .await;

            // ── Fix D: detection errore infrastrutturale ────────────────────
            // Se la risposta menziona ToolRunner/sandbox down, NON e' colpa del
            // modello — non incrementare consecutive_failures (evita di
            // auto-disabilitare modelli sani per problemi infra) e termina
            // subito senza scalare (gli altri provider avrebbero lo stesso esito).
            let is_infrastructure_error = result.final_answer.as_ref()
                .map(|s| {
                    let lower = s.to_lowercase();
                    lower.contains("sandbox") && (lower.contains("gr pc") || lower.contains("grpc")
                        || lower.contains("connession") || lower.contains("non e' raggiungibile")
                        || lower.contains("non raggiungibile"))
                    || lower.contains("50500")
                    || lower.contains("tool_runner") || lower.contains("toolrunner")
                    || lower.contains("tcp handshaker")
                })
                .unwrap_or(false);
            if is_infrastructure_error {
                tracing::warn!(
                    "agent_run {}: errore INFRASTRUTTURALE rilevato (ToolRunner/sandbox down) — \
                     non incremento consecutive_failures per {}/{}, termino senza fallback (altri \
                     provider hanno lo stesso ToolRunner)",
                    run_id, result.provider, result.model
                );
                break;
            }

            // ── Counter hollow per modello (auto-disable) ────────────────────
            // Se il run e' hollow_completion REALE in produzione, incrementa
            // il counter consecutive_failures su ai_price_catalog. Questo e'
            // piu' affidabile del model_health_probe perche' rileva il problema
            // su workload reali (prompt lunghi, max_tokens reali) — non con
            // "ping" che a volte passa anche su modelli broken (es. gemini-3.5-flash
            // risponde a "ping" in 5s ma da hollow su prompt agente).
            //
            // Soglia 3 fallimenti consecutivi → is_enabled=false. Reset a 0 al
            // primo successo (status=Completed e final_answer NON vuoto).
            let intent_uses_tools = classified_intent_for_loop != "chat";
            if matches!(result.status, AgentRunStatus::Completed) && intent_uses_tools {
                let success_now = !result.hollow_completion
                    && result.final_answer.as_ref()
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false);
                if result.hollow_completion {
                    let new_count: Option<i32> = sqlx::query_scalar(
                        "UPDATE ai_price_catalog
                            SET consecutive_failures = consecutive_failures + 1,
                                updated_at = NOW()
                          WHERE provider = $1 AND model = $2
                        RETURNING consecutive_failures",
                    )
                    .bind(&result.provider)
                    .bind(&result.model)
                    .fetch_optional(&db_clone)
                    .await
                    .ok()
                    .flatten();
                    if let Some(n) = new_count {
                        tracing::warn!(
                            "agent_run {}: hollow run reale su {}/{} — counter={}/3",
                            run_id, result.provider, result.model, n
                        );
                        if n >= 3 {
                            let _ = sqlx::query(
                                "UPDATE ai_price_catalog
                                    SET is_enabled = false,
                                        auto_disabled_at = NOW(),
                                        auto_disabled_reason = 'hollow_completion_runtime',
                                        updated_at = NOW()
                                  WHERE provider = $1 AND model = $2
                                    AND is_enabled = true",
                            )
                            .bind(&result.provider)
                            .bind(&result.model)
                            .execute(&db_clone)
                            .await;
                            tracing::warn!(
                                "AUTO-DISABLE runtime {}/{} dopo {} hollow consecutivi",
                                result.provider, result.model, n
                            );
                        }
                    }
                } else if success_now {
                    let _ = sqlx::query(
                        "UPDATE ai_price_catalog
                            SET consecutive_failures = 0,
                                auto_disabled_at = NULL,
                                auto_disabled_reason = NULL,
                                updated_at = NOW()
                          WHERE provider = $1 AND model = $2 AND consecutive_failures > 0",
                    )
                    .bind(&result.provider)
                    .bind(&result.model)
                    .execute(&db_clone)
                    .await;
                }
            }

            // Decide se ritentare: nuova logica basata su error_class strutturato
            // propagato dal brain via SSE, oltre allo stato cooldown del provider.
            // Casi che giustificano un retry su altro provider:
            //   - provider in cooldown (lungo o breve, gia' marcato dal brain_agent_client)
            //   - error_class in {billing_error, rate_limit, provider_error}
            //   - il run e' fallito con stop_reason=error (anche senza classify, ritenta una volta)
            //   - hollow_completion: il modello ha risposto senza usare tool (0 step)
            let failed_retry = matches!(result.status, AgentRunStatus::Failed) && {
                let in_cooldown = crate::provider_cooldown::is_provider_in_cooldown(&current_provider);
                let retriable_class = matches!(
                    result.error_class.as_deref(),
                    Some("billing_error") | Some("rate_limit") | Some("provider_error")
                );
                in_cooldown || retriable_class
            };
            // Hollow completion: il modello ha risposto senza usare tool.
            // Per intent `chat` (chiacchierata, domande conversazionali,
            // meta-domande) la risposta senza tool e' attesa e corretta —
            // disabilitiamo il retry. Per altri intent (anche `docs` quando
            // l'utente chiede di scrivere/leggere documentazione) il retry
            // serve perche' il modello dovrebbe usare tool.
            let hollow_retry = result.hollow_completion
                && classified_intent_for_loop != "chat";
            let should_retry = failed_retry || hollow_retry;

            if !should_retry || fallback_attempt + 1 >= max_provider_fallbacks {
                break;
            }

            if hollow_retry {
                tracing::warn!(
                    "agent_run {}: hollow completion da {}/{} — il modello ha risposto \
                     senza usare tool, ritento con un modello piu capace",
                    run_id, current_provider, current_model
                );
            }

            // ── ESCALATION su hollow ricorrente ─────────────────────────────
            // Se gia' 1 hollow nel run (questo e' il 2o tentativo dopo hollow),
            // smetti di girare in tondo sui modelli small e scala al primo
            // modello "di ordine superiore" disponibile nel catalog:
            // performance_tier='heavy' AND is_enabled, ordinato per qualita'
            // (costo input desc = proxy di capacita'). Provider-agnostic:
            // sceglie qualunque heavy disponibile non gia' tried/in-cooldown.
            //
            // Esempi attesi (sort cost desc):
            //   anthropic/claude-opus-4-7 > openai/gpt-5 > anthropic/claude-sonnet-4-6
            //   > mistral/mistral-large-latest > google/gemini-2.5-pro > deepseek/deepseek-reasoner
            //
            // Conta come "hollow precedente" se hollow_retry == true ora E
            // questo e' fallback_attempt >= 1 (cioe' siamo gia' al 2o turno).
            let escalate_on_hollow = hollow_retry && fallback_attempt >= 1;
            let next_pair: Option<(String, String)> = if escalate_on_hollow {
                let tried_models: Vec<String> = tried.iter().cloned().collect();
                // Ordina i candidati in base alla "potenza" desumibile dal catalog:
                //   1. tier_rank: heavy(2) > medium(1) > light(0)
                //   2. input_cost_per_million_tokens desc (proxy di capacita')
                // Filtri:
                //   - is_enabled
                //   - supports_tool_use (l'intent richiede tool)
                //   - consecutive_failures = 0 (non ha gia' dato hollow di recente)
                //   - provider non gia' tried in questo run
                //   - provider non in cooldown billing/quota
                let candidates: Vec<(String, String, String)> = sqlx::query_as::<_, (String, String, String)>(
                    "SELECT provider, model, performance_tier
                       FROM ai_price_catalog
                      WHERE is_enabled = true
                        AND supports_tool_use = true
                        AND consecutive_failures = 0
                        AND NOT (provider = ANY($1))
                        AND context_window >= $2
                      ORDER BY CASE performance_tier
                                 WHEN 'heavy' THEN 2
                                 WHEN 'medium' THEN 1
                                 ELSE 0
                               END DESC,
                               input_cost_per_million_tokens DESC NULLS LAST,
                               output_cost_per_million_tokens DESC NULLS LAST",
                )
                .bind(&tried_models)
                .bind(ctx_needed)
                .fetch_all(&db_clone)
                .await
                .unwrap_or_default();
                // Primo candidato il cui provider non e' in cooldown
                candidates.into_iter().find(|(p, _, _)| {
                    !crate::provider_cooldown::is_provider_in_cooldown(p)
                }).map(|(p, m, tier)| {
                    tracing::warn!(
                        "agent_run {}: ESCALATION hollow ricorrente — salto a tier={} {}/{} (provider-agnostic)",
                        run_id, tier, p, m
                    );
                    (p, m)
                })
            } else { None };

            let (chosen_provider, chosen_model) = if let Some(pair) = next_pair {
                pair
            } else {
                // Cerca il prossimo provider nella gerarchia, non gia' provato e non in cooldown
                let next = provider_hierarchy.iter().find(|p| {
                    !tried.contains(*p) && !crate::provider_cooldown::is_provider_in_cooldown(p)
                });
                let Some(next_provider) = next else {
                    tracing::warn!("agent_run {}: nessun provider alternativo disponibile, mantengo risultato", run_id);
                    break;
                };
                let new_provider = next_provider.clone();
                // Modello di default per il nuovo provider, letto da DB (registry routing).
                let new_model = match routing_matrix_for_loop.current_async().await {
                    Ok(matrix_arc) => matrix_arc
                        .default_model(&new_provider)
                        .unwrap_or_else(|| {
                            tracing::warn!(
                                "agent_run {}: provider '{}' non configurato in nexus_provider_default_model, mantengo modello corrente",
                                run_id, new_provider
                            );
                            current_model.clone()
                        }),
                    Err(e) => {
                        tracing::error!(
                            "agent_run {}: routing_matrix non disponibile ({}), mantengo modello corrente",
                            run_id, e
                        );
                        current_model.clone()
                    }
                };
                (new_provider, new_model)
            };
            current_provider = chosen_provider;
            current_model = chosen_model;
            fallback_attempt += 1;
            tracing::warn!(
                "agent_run {}: fallback automatico a {}/{} (motivo: {})",
                run_id, current_provider, current_model,
                if hollow_retry { "hollow completion" } else { "provider error/cooldown" }
            );
            // Meta-step `fallback` pubblicato in chat per trasparenza:
            // utente vede in tempo reale che il sistema ha cambiato
            // provider/modello (es. anthropic -> openai per quota_exceeded).
            let reason = if hollow_retry { "hollow_completion" } else { "provider_error_or_cooldown" };
            let _ = tx_for_brain.send(AgentStepEvent {
                run_id: run_id.to_string(),
                step: None,
                trace: None,
                is_final: false,
                token_delta: None,
                meta_step: Some(crate::agent_types::AgentMetaStep {
                    kind: "fallback".to_string(),
                    title: format!("Fallback su {}/{}", current_provider, current_model),
                    payload: serde_json::json!({
                        "to_provider": current_provider,
                        "to_model": current_model,
                        "reason": reason,
                        "attempt": fallback_attempt,
                    }),
                    correlation_id: None,
                    created_at: chrono::Utc::now().to_rfc3339(),
                }),
            });
        }
        // Emetti is_final solo DOPO la fine del retry loop, cosi' il
        // frontend non chiude lo stream SSE dopo il primo tentativo fallito
        // perdendo i successivi tentativi di fallback.
        let _ = tx_for_brain.send(AgentStepEvent {
            run_id: run_id.to_string(),
            step: None,
            trace: None,
            is_final: true,
            token_delta: None,
            meta_step: None,
        });
        channels_clone.remove(&run_id);

        // Se il gateway ha re-instradato su provider locale per privacy
        // (il provider finale differ da quello richiesto ed è "vllm" o altro locale),
        // azzeriamo la preferenza di sessione → al prossimo messaggio torna il routing automatico.
        let privacy_rerouted = had_session_override
            && result.provider != requested_provider_clone
            && matches!(result.provider.as_str(), "vllm" | "local" | "ollama");
        if privacy_rerouted {
            clear_session_preferred_provider_after_privacy(&db_clone, session_id_cp).await;
        }

        // ── Hollow completion: il modello ha dichiarato di aver completato
        // senza invocare alcun tool. Per intent `chat` questo e' atteso —
        // non aggiungiamo avvisi spuri.
        let conversational_intent = classified_intent_for_loop == "chat";
        let report_hollow = result.hollow_completion && !conversational_intent;
        if report_hollow {
            tracing::warn!(
                "agent_run {}: hollow completion rilevato — il modello {}/{} \
                 non ha eseguito alcun tool. La risposta potrebbe essere incompleta.",
                run_id, result.provider, result.model
            );
        }

        // Save final answer as assistant message.
        // Se l'agente ha completato ma final_answer e' None o whitespace-only
        // (caso hollow EMPTY_ANSWER, es. deepseek-coder che chiude il turno
        // senza emettere body), generiamo comunque un messaggio chiaro per
        // l'utente — altrimenti la UI mostra solo lo status "completed"
        // senza alcun contenuto, lasciando l'utente con l'impressione che il
        // sistema abbia "fatto qualcosa" che in realta' non e' avvenuto.
        let answer_owned: Option<String> = match result.final_answer.as_ref() {
            Some(s) if !s.trim().is_empty() => Some(s.clone()),
            // Final answer mancante o vuoto: fabrichiamo un placeholder solo
            // se siamo arrivati qui DOPO il retry loop (hollow_completion
            // confermato e tentativi esauriti). Altrimenti il body fantasma
            // confonderebbe la storia turni.
            _ if report_hollow => Some(format!(
                "_(Nessuna risposta utile prodotta dall'agente — {} / {} ha chiuso \
                 il turno con un completamento vuoto dopo aver esaurito i tentativi \
                 di fallback. Riformula la richiesta o cambia provider/modello manualmente.)_",
                result.provider, result.model
            )),
            _ => None,
        };

        if let Some(ref answer) = answer_owned {
            // Annota la risposta solo se l'intent richiedeva tool e l'agente
            // ha prodotto un body (per evitare doppia annotazione sul placeholder).
            let had_real_body = result.final_answer.as_ref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            let effective_answer = if report_hollow && had_real_body {
                format!(
                    "{answer}\n\n---\n*Avviso: l'agente ({}/{}) ha risposto senza \
                     eseguire alcun tool (0 step). La risposta potrebbe essere \
                     incompleta o generica. Riprova con un modello piu' capace \
                     o riformula la richiesta.*",
                    result.provider, result.model
                )
            } else {
                answer.clone()
            };
            let meta = json!({
                "provider": &result.provider,
                "model": &result.model,
                "agentRunId": run_id.to_string(),
                "iterationCount": result.iteration_count,
                "automationMode": "agent",
                "privacyRerouted": privacy_rerouted,
                "hollowCompletion": result.hollow_completion,
                // Usage tracking: senza questi campi il TokenUsageBar resta
                // invisibile (la query in billing::get_session_usage somma
                // metadata->>'totalTokens'). I valori sono gia' calcolati e
                // scritti su agent_runs subito sotto.
                "promptTokens": result.prompt_tokens,
                "completionTokens": result.completion_tokens,
                "totalTokens": result.total_tokens,
                "totalCost": result.total_cost,
                "currency": "USD",
            });
            let _ = sqlx::query(
                r#"INSERT INTO chat_messages
                   (id, session_id, project_id, role, content, metadata, request_message_id, created_at)
                   VALUES (gen_random_uuid(),$1,$2,'assistant',$3,$4,$5,NOW())"#,
            )
            .bind(session_id_cp)
            .bind(project_id_cp)
            .bind(&effective_answer)
            .bind(meta)
            .bind(user_message_id)
            .execute(&db_clone)
            .await;

            spawn_embed_conversation_turn(
                neural_for_embed.clone(),
                db_clone.clone(),
                session_id_cp,
                Uuid::new_v4(),
                "assistant".to_string(),
                effective_answer.clone(),
            );
        }

        // Update run status in DB
        let status_str = match result.status {
            AgentRunStatus::Completed => "completed",
            AgentRunStatus::AwaitingConfirmation => "awaiting_confirmation",
            AgentRunStatus::Failed => "failed",
            AgentRunStatus::TimedOut => "timed_out",
            AgentRunStatus::Cancelled => "cancelled",
            AgentRunStatus::Running => "running",
            AgentRunStatus::LoopAborted => "loop_aborted",
            AgentRunStatus::ProviderUnavailable => "provider_unavailable",
        };
        let _ = sqlx::query(
            "UPDATE agent_runs SET status=$2, final_answer=$3, iteration_count=$4, \
             prompt_tokens=$5, completion_tokens=$6, total_tokens=$7, total_cost=$8, \
             nexus_override_applied=$9, nexus_agent_type=$10, nexus_task_type=$11, \
             completed_at=NOW() WHERE id=$1",
        )
        .bind(run_id)
        .bind(status_str)
        .bind(result.final_answer.as_deref())
        .bind(result.iteration_count as i32)
        .bind(result.prompt_tokens as i32)
        .bind(result.completion_tokens as i32)
        .bind(result.total_tokens as i32)
        .bind(result.total_cost)
        .bind(result.nexus_override_applied)
        .bind(result.nexus_agent_type.as_deref())
        .bind(result.nexus_task_type.as_deref())
        .execute(&db_clone)
        .await;

        // ── Budget tracking ──────────────────────────────────────────────
        // Incrementa il `spent_current_period_usd` per il provider del run.
        // Strategia comune a tutti i 5 provider visto che la maggior parte
        // (anthropic/openai/google/mistral) non espone balance via API: il
        // budget va stimato sommando il cost dei run reali.
        //
        // Calcolo del cost:
        //   - Se brain ha propagato result.total_cost > 0 -> usalo.
        //   - Altrimenti: calcolo da prompt_tokens/completion_tokens × prezzi
        //     dal catalog (caso comune: brain non emette total_cost nelle
        //     SSE events, ma propaga i token usage che sono affidabili).
        let cost_to_charge: f64 = if result.total_cost > 0.0 {
            result.total_cost
        } else if result.prompt_tokens > 0 || result.completion_tokens > 0 {
            // Look up prezzi dal catalog. Costo per milione di token.
            #[derive(sqlx::FromRow)]
            struct PriceRow {
                input_cost: f64,
                output_cost: f64,
            }
            let prices: Option<PriceRow> = sqlx::query_as::<_, PriceRow>(
                "SELECT input_cost_per_million_tokens::float8 AS input_cost,
                        output_cost_per_million_tokens::float8 AS output_cost
                   FROM ai_price_catalog
                  WHERE provider = $1 AND model = $2 AND is_enabled = true
                  ORDER BY effective_from DESC LIMIT 1",
            )
            .bind(&result.provider)
            .bind(&result.model)
            .fetch_optional(&db_clone)
            .await
            .ok()
            .flatten();
            if let Some(p) = prices {
                let input_cost = (result.prompt_tokens as f64) * p.input_cost / 1_000_000.0;
                let output_cost = (result.completion_tokens as f64) * p.output_cost / 1_000_000.0;
                let total = input_cost + output_cost;
                if total > 0.0 {
                    // Aggiorna anche agent_runs.total_cost per coerenza UI.
                    let _ = sqlx::query(
                        "UPDATE agent_runs SET total_cost = $2 WHERE id = $1 AND total_cost = 0",
                    )
                    .bind(run_id)
                    .bind(total)
                    .execute(&db_clone)
                    .await;
                    tracing::debug!(
                        "budget: cost calcolato da Rust per {}/{} = ${:.6} (prompt={} comp={})",
                        result.provider, result.model, total,
                        result.prompt_tokens, result.completion_tokens
                    );
                }
                total
            } else {
                0.0
            }
        } else {
            0.0
        };
        if cost_to_charge > 0.0 {
            let _ = sqlx::query(
                "INSERT INTO provider_budget_status (provider, spent_current_period_usd)
                   VALUES ($1, $2)
                 ON CONFLICT (provider) DO UPDATE
                   SET spent_current_period_usd = provider_budget_status.spent_current_period_usd + EXCLUDED.spent_current_period_usd,
                       updated_at = NOW()",
            )
            .bind(&result.provider)
            .bind(cost_to_charge)
            .execute(&db_clone)
            .await;
        }

        // Persisti gli step del run su agent_steps (fix bug: la tabella veniva letta
        // da chat_agent.rs:121,195 ma non scritta — dashboard "AI Workspace" mostrava
        // sempre storia vuota, reflection non poteva correlare step con outcome).
        // Gli step sono gia' raccolti in-memory dal brain_agent_client durante il loop SSE.
        if !result.steps.is_empty() {
            for step in &result.steps {
                let _ = sqlx::query(
                    "INSERT INTO agent_steps \
                     (id, run_id, step_index, tool_name, tool_input, tool_result, status, created_at) \
                     VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6, NOW())",
                )
                .bind(run_id)
                .bind(step.step_index as i32)
                .bind(&step.tool_name)
                .bind(&step.tool_input)
                .bind(step.tool_result.as_deref())
                .bind(step.status.as_str())
                .execute(&db_clone)
                .await;
            }
            tracing::debug!(
                "agent_run {}: {} step persistiti in agent_steps",
                run_id,
                result.steps.len()
            );
        }
        }); // chiude AssertUnwindSafe(async move { ... })

        // Cattura panic dell'intero body: senza questo, un panic dentro lo
        // spawn lascia il run con status='running' per sempre e l'UI bloccata
        // (il canale broadcast non riceve mai is_final).
        if let Err(panic_payload) = agent_body.catch_unwind().await {
            let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "panic non-stringificato".to_string()
            };
            tracing::error!(
                "agent_run {}: PANIC catturato nel tokio::spawn — emetto is_final di fallback. Payload: {}",
                panic_run_id, panic_msg
            );

            // 1. Emetti is_final per sbloccare l'UI
            let _ = panic_tx.send(AgentStepEvent {
                run_id: panic_run_id.to_string(),
                step: None,
                trace: None,
                is_final: true,
                token_delta: None,
                meta_step: None,
            });
            panic_channels.remove(&panic_run_id);

            // 2. Aggiorna agent_runs come failed
            let _ = sqlx::query(
                "UPDATE agent_runs SET status='failed', completed_at=NOW(), \
                 final_answer=$2 WHERE id=$1",
            )
            .bind(panic_run_id)
            .bind(format!(
                "Errore interno: il task agente e' terminato in modo imprevisto ({}). Riprova.",
                panic_msg
            ))
            .execute(&panic_db)
            .await;

            // 3. Inserisci un messaggio assistant per far vedere l'errore in chat
            let _ = sqlx::query(
                r#"INSERT INTO chat_messages
                   (id, session_id, project_id, role, content, metadata, request_message_id, created_at)
                   VALUES (gen_random_uuid(),$1,$2,'assistant',$3,$4,$5,NOW())"#,
            )
            .bind(panic_session_id)
            .bind(panic_project_id)
            .bind(format!(
                "⚠ Errore interno: il task agente e' terminato in modo imprevisto.\n\n```\n{}\n```\n\nPuoi riprovare la richiesta.",
                panic_msg
            ))
            .bind(json!({"errorClass": "internal_panic", "agentRunId": panic_run_id.to_string()}))
            .bind(panic_user_msg_id)
            .execute(&panic_db)
            .await;
        }
    });

    Some(SpawnAgentResult {
        run_id,
        provider,
        model: model_str,
    })
}

pub async fn send_chat_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(session_id): AxumPath<String>,
    Json(body): Json<SendChatMessageRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let session_id = Uuid::parse_str(&session_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Session id non valido"))?;
    let context = load_session_context(&state.db, session_id, user_id).await?;

    let content = body.content.trim();
    if content.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il contenuto del messaggio e' obbligatorio",
        ));
    }

    let user_message_id = insert_message(
        &state.db,
        context.session_id,
        context.project_id,
        "user",
        content,
        json!({
            "providerOverride": body.provider_override.clone(),
            "modelOverride": body.model_override.clone(),
            "automationMode": body.automation_mode.clone().unwrap_or_else(|| "confirm".to_string()),
            "attachments": body.attachments.clone(),
            // Marca i messaggi auto-generati dal sistema (es. auto-continuazione).
            // Il frontend filtra questi messaggi dalla UI per non confondere l'utente
            // facendogli credere di averli scritti lui.
            "synthetic": body.synthetic,
        }),
        None,
    )
    .await?;
    let user_row = load_message_by_id(&state.db, user_message_id).await?;
    let user_message = to_message_view(&user_row)?;

    spawn_embed_conversation_turn(
        state.orchestrator.neural.clone(),
        state.db.clone(),
        context.session_id,
        user_message_id,
        "user".to_string(),
        content.to_string(),
    );

    // ── Hook: auto-creazione nota Knowledge Base ───────────────────────────
    // Ogni messaggio utente genera una nota in background (non blocca il turno).
    {
        let db_clone = state.db.clone();
        let neural_clone = state.orchestrator.neural.clone();
        let channels_clone = state.project_channels.clone();
        let pid = context.project_id;
        let mid = user_message_id;
        let cnt = content.to_string();
        let intent_val: Option<String> = None; // l'intent verra' aggiornato dal classifier
        // Recupera repo root per vault PUSH
        let repo_root: Option<String> = sqlx::query_scalar(
            "SELECT repository_root_path FROM projects WHERE id = "
        )
        .bind(pid)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
        tokio::spawn(async move {
            crate::knowledge::create_note_from_user_message(
                db_clone, neural_clone, pid, mid, cnt, intent_val, repo_root, channels_clone,
            )
            .await;
        });
    }

    // ── Rilevamento cambio modello esplicito ────────────────────────────────
    // Se il messaggio è un comando "usa mistral / cambia a claude / ecc." e
    // il client non ha già impostato un override manuale, gestiamo lo switch
    // automaticamente: salviamo la preferenza nella sessione e rispondiamo con
    // un messaggio di conferma senza coinvolgere l'AI.
    if body.provider_override.is_none() {
        // Reset al routing automatico
        if detect_model_reset(content) {
            let _ = sqlx::query(
                "UPDATE chat_sessions SET preferred_provider = NULL, preferred_model = NULL WHERE id = $1",
            )
            .bind(context.session_id)
            .execute(&state.db)
            .await;

            let ack_id = insert_message(
                &state.db,
                context.session_id,
                context.project_id,
                "assistant",
                "Routing automatico ripristinato. Il sistema sceglierà il modello ottimale per ogni richiesta.",
                json!({ "provider": "system", "model": "auto", "intent": "model_reset" }),
                Some(user_message_id),
            )
            .await?;
            let ack_row = load_message_by_id(&state.db, ack_id).await?;
            let ack_message = to_message_view(&ack_row)?;
            return Ok(Json(json!({
                "sessionId": context.session_id.to_string(),
                "userMessage": user_message,
                "assistantMessage": ack_message,
            })));
        }

        if let Some((switched_provider, switched_model)) = detect_model_switch(&state.db, content).await {
            // Persiste la preferenza nella sessione per i messaggi futuri
            let _ = sqlx::query(
                "UPDATE chat_sessions SET preferred_provider = $1, preferred_model = $2 WHERE id = $3",
            )
            .bind(&switched_provider)
            .bind(&switched_model)
            .bind(context.session_id)
            .execute(&state.db)
            .await;

            // Genera un messaggio assistant di conferma e salvalo nel DB
            let model_label = switched_model.clone().unwrap_or_else(|| switched_provider.clone());
            let ack_content = format!(
                "Modello impostato su **{}**{}. I prossimi messaggi in questa sessione useranno questo provider.",
                switched_provider,
                if switched_model.is_some() {
                    format!(" ({})", model_label)
                } else {
                    String::new()
                }
            );
            let ack_meta = json!({
                "provider": switched_provider,
                "model": model_label,
                "intent": "model_switch",
                "automationMode": "confirm",
            });
            let ack_id = insert_message(
                &state.db,
                context.session_id,
                context.project_id,
                "assistant",
                &ack_content,
                ack_meta,
                Some(user_message_id),
            )
            .await?;
            let ack_row = load_message_by_id(&state.db, ack_id).await?;
            let ack_message = to_message_view(&ack_row)?;

            return Ok(Json(json!({
                "sessionId": context.session_id.to_string(),
                "userMessage": user_message,
                "assistantMessage": ack_message,
            })));
        }
    }

    // ── Carica preferenza modello della sessione ────────────────────────────
    // Se l'utente aveva già impostato un provider preferito in questa sessione
    // (tramite un comando precedente "usa mistral"), lo usa come override di default.
    let (session_preferred_provider, session_preferred_model): (Option<String>, Option<String>) =
        if body.provider_override.is_none() {
            sqlx::query_as::<_, (Option<String>, Option<String>)>(
                "SELECT preferred_provider, preferred_model FROM chat_sessions WHERE id = $1",
            )
            .bind(context.session_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
            .map(|(prov, model)| {
                (
                    prov.filter(|s| !s.is_empty()),
                    model.filter(|s| !s.is_empty()),
                )
            })
            .unwrap_or((None, None))
        } else {
            (None, None)
        };
    // Override effettivo: esplicito dal client > preferenza di sessione
    let effective_provider_override = body
        .provider_override
        .clone()
        .or(session_preferred_provider);
    let effective_model_override = body
        .model_override
        .clone()
        .or(session_preferred_model);

    let profile_id = body
        .profile_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "default".to_string());

    // Carica contesto profilo (system_prompt, provider/model/automation override)
    // Passa il testo della richiesta per la selezione automatica (profile_id == "auto")
    let (profile_prompt_block, profile_provider, profile_model, profile_automation) =
        fetch_profile_context(&state.db, user_id, &profile_id, &body.content).await;

    let automation_mode = parse_automation_mode(
        body.automation_mode.as_deref()
            .or(profile_automation.as_deref())
    );
    let supervisor_mode = SupervisorMode::from_str(
        body.supervisor_mode.as_deref().unwrap_or("none")
    );

    // Fetch user info to build system context
    let github_username: Option<String> = sqlx::query_scalar(
        "SELECT github_username FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None)
    .flatten();

    let system_prompt = crate::prompt_templates::get_template_or_default(
        &state.db,
        &state.template_cache,
        "system.nexus_base",
    ).await;

    let system_context = {
        let mut ctx = system_prompt;
        if automation_mode != AutomationMode::Study {
            ctx.push_str(crate::prompt_templates::AGENT_ACT_FIRST_SUFFIX);
        }
        if let Some(ref gh) = github_username {
            ctx.push_str(&format!(" Account GitHub: @{gh}."));
        }
        // ── Iniezione Knowledge Base (top-K note semanticamente rilevanti) ──
        // Embed del messaggio user → search Qdrant `knowledge_notes` filtrata per progetto
        // → carica title+body delle top hit → prependi come "Contesto progetto" al system prompt.
        // Failsafe: se brain down o KB vuota, il flow normale prosegue (no contesto KB).
        if let Some(kb_block) = build_knowledge_context(&state, context.project_id, content).await {
            ctx.push_str("\n\n");
            ctx.push_str(&kb_block);
        }
        ctx
    };

    // ── Ripresa run interrotto (riprendi / continua / resume) ─────────────
    if automation_mode != AutomationMode::Study {
        let is_resume_request = {
            let lower = content.trim().to_lowercase();
            lower == "riprendi"
                || lower == "continua"
                || lower == "resume"
                || lower == "riprendi dall'interruzione"
                || lower.starts_with("riprendi ")
                || lower.starts_with("continua da")
        };

        if is_resume_request {
            // Cerca l'ultimo run interrupted di questa sessione con history salvata
            let interrupted_run = sqlx::query(
                r#"SELECT id, provider, model, messages_json, iteration_count, supervisor_mode
                   FROM agent_runs
                   WHERE session_id = $1
                     AND status = 'interrupted'
                     AND messages_json IS NOT NULL
                     AND messages_json != ''
                   ORDER BY created_at DESC
                   LIMIT 1"#,
            )
            .bind(context.session_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();

            if let Some(prev_run) = interrupted_run {
                let prev_run_id: Uuid = prev_run.get("id");
                let prev_provider: String = prev_run.get("provider");
                let prev_model: String = prev_run.get("model");
                let prev_messages_json: String = prev_run.get("messages_json");
                let prev_iterations: i32 = prev_run.get("iteration_count");

                tracing::info!(
                    "Resuming interrupted run {} (iter={}, supervisor={}) for session {}",
                    prev_run_id, prev_iterations,
                    prev_run.try_get::<String, _>("supervisor_mode").unwrap_or_else(|_| "none".into()),
                    context.session_id
                );

                // Crea nuovo run collegato al precedente
                let new_run_id = Uuid::new_v4();
                let (tx, _rx) = broadcast::channel::<AgentStepEvent>(256);
                state.agent_channels.insert(new_run_id, tx.clone());

                let prev_supervisor_str: String = prev_run.try_get("supervisor_mode")
                    .unwrap_or_else(|_| "none".to_string());
                let prev_supervisor = SupervisorMode::from_str(&prev_supervisor_str);

                let _ = sqlx::query(
                    r#"INSERT INTO agent_runs
                       (id, session_id, project_id, user_id, run_message_id, status,
                        automation_mode, provider, model, supervisor_mode, iteration_count, parent_run_id, created_at)
                       VALUES ($1,$2,$3,$4,$5,'running',$6,$7,$8,$9,0,$10,NOW())"#,
                )
                .bind(new_run_id)
                .bind(context.session_id)
                .bind(context.project_id)
                .bind(user_id)
                .bind(user_message_id)
                .bind(automation_mode.as_str())
                .bind(&prev_provider)
                .bind(&prev_model)
                .bind(prev_supervisor.as_str())
                .bind(prev_run_id)
                .execute(&state.db)
                .await;

                // Marca il vecchio run come ripreso
                let _ = sqlx::query(
                    "UPDATE agent_runs SET status='cancelled', final_answer='Ripreso da nuovo run.' WHERE id=$1"
                )
                .bind(prev_run_id)
                .execute(&state.db)
                .await;

                // Carica contesto progetto per il nuovo run
                if let Ok(proj) = load_project_context(&state.db, context.project_id, user_id).await {
                    let db_clone2 = state.db.clone();
                    let channels2 = state.agent_channels.clone();
                    let neural2 = state.orchestrator.neural.clone();
                    let term2 = state.terminal_consumers.clone();
                    let session_id_r = context.session_id;
                    let project_id_r = context.project_id;
                    let msg_id_r = user_message_id;
                    let provider_r = prev_provider.clone();
                    let model_r = prev_model.clone();
                    let automation_r = automation_mode.clone();
                    let supervisor_r = prev_supervisor;
                    let template_cache_r = state.template_cache.clone();
                    let routing_thresholds_for_resume = state.orchestrator.routing_thresholds.clone();
                    let user_role_r = claims.role.clone();

                    let _ = (&neural2, &term2, &automation_r, &supervisor_r, &user_role_r, &proj, &prev_messages_json);
                    tokio::spawn(async move {
                        let resume_tpl = crate::prompt_templates::get_template_or_default(
                            &db_clone2,
                            &template_cache_r,
                            "automation.run_resume_instruction",
                        )
                        .await;
                        let resume_prompt = resume_tpl.replace("{{prev_iterations}}", &prev_iterations.to_string());

                        let resume_history =
                            build_recent_conversation_history(&db_clone2, session_id_r, 8).await;

                        let tools_for_resume = crate::brain_agent_client::build_tools_json_for_agent(
                            &db_clone2,
                            user_id,
                            project_id_r,
                            &automation_r,
                            &provider_r,
                            &model_r,
                        )
                        .await;

                        // Re-leggo soglia SSE silence (mig 0132) — cache 60s.
                        let sse_silence_resume: u64 = match routing_thresholds_for_resume
                            .current_async()
                            .await
                        {
                            Ok(t) => t.sse_heartbeat_max_silence_secs,
                            Err(_) => 120,
                        };

                        let result = crate::brain_agent_client::run_via_brain(
                            new_run_id,
                            session_id_r,
                            provider_r,
                            model_r,
                            String::new(),
                            resume_prompt,
                            tx,
                            resume_history,
                            tools_for_resume,
                            sse_silence_resume,
                            true, // emit_final_event: caller singolo-shot, nessun retry loop
                            automation_r.as_str().to_string(),
                        )
                        .await;
                        channels2.remove(&new_run_id);

                        if let Some(ref answer) = result.final_answer {
                            let meta = serde_json::json!({
                                "provider": &result.provider,
                                "model": &result.model,
                                "agentRunId": new_run_id.to_string(),
                                "iterationCount": result.iteration_count,
                                "automationMode": "automatic",
                                "resumed": true,
                                "promptTokens": result.prompt_tokens,
                                "completionTokens": result.completion_tokens,
                                "totalTokens": result.total_tokens,
                                "totalCost": result.total_cost,
                                "currency": "USD",
                            });
                            let _ = sqlx::query(
                                r#"INSERT INTO chat_messages
                                   (id, session_id, project_id, role, content, metadata, request_message_id, created_at)
                                   VALUES (gen_random_uuid(),$1,$2,'assistant',$3,$4,$5,NOW())"#,
                            )
                            .bind(session_id_r)
                            .bind(project_id_r)
                            .bind(answer)
                            .bind(meta)
                            .bind(msg_id_r)
                            .execute(&db_clone2)
                            .await;
                        }

                        crate::agent_types::finalize_agent_run(
                            &db_clone2, new_run_id,
                            result.status, result.final_answer.as_deref(),
                            result.iteration_count,
                        ).await;
                    });

                    return Ok(Json(json!({
                        "sessionId": context.session_id.to_string(),
                        "userMessage": user_message,
                        "agentRun": {
                            "runId": new_run_id.to_string(),
                            "status": "running",
                            "provider": prev_provider,
                            "model": prev_model,
                            "resumed": true,
                        }
                    })));
                }
            }
        }
    }

    // ── DLP check (Nexus Sicurezza & Privacy) ────────────────────────────────
    // Classifica la sensibilità del contenuto utente prima di inviarlo al brain.
    // Eseguito qui — prima sia di spawn_agent_run sia di run_turn — così copre
    // tutti i percorsi (modalità agente + studio + fallback).
    {
        let tier = crate::dlp::classify_sensitivity(content);
        if tier >= crate::dlp::SensitivityTier::Sensitive {
            // Provider per il check DLP: usa l'override se presente, altrimenti
            // il primo default dalla routing matrix (DB-driven, niente hardcoded).
            let matrix_provider: Option<String> = state.orchestrator.routing_matrix.current()
                .ok()
                .and_then(|m| m.default_models.keys().next().cloned());
            let check_provider = effective_provider_override.as_deref()
                .or(matrix_provider.as_deref())
                .unwrap_or("system");
            if let Some(dlp_msg) = crate::dlp::check_dlp_policy_db(check_provider, tier, &state.db).await {
                if dlp_msg.contains("DLP Block") {
                    // Salva il messaggio di errore come risposta assistant in DB
                    // così l'utente vede il motivo del blocco nell'interfaccia.
                    let err_id = insert_message(
                        &state.db,
                        context.session_id,
                        context.project_id,
                        "assistant",
                        &dlp_msg,
                        json!({
                            "provider": "system",
                            "model": "dlp",
                            "intent": "dlp_block",
                        }),
                        Some(user_message_id),
                    )
                    .await
                    .ok();
                    if let Some(err_msg_id) = err_id {
                        if let Ok(err_row) = load_message_by_id(&state.db, err_msg_id).await {
                            if let Ok(err_msg) = to_message_view(&err_row) {
                                return Ok(Json(json!({
                                    "sessionId": context.session_id.to_string(),
                                    "userMessage": user_message,
                                    "assistantMessage": err_msg,
                                    "dlpBlocked": true,
                                })));
                            }
                        }
                    }
                    return Err(api_error(StatusCode::FORBIDDEN, dlp_msg));
                } else {
                    tracing::warn!("DLP: {}", dlp_msg);
                }
            }
        }
    }

    // ── Modalita' agente: dispatcha al loop agente invece del singolo turn ──
    if automation_mode != AutomationMode::Study {
        if let Some(result) = spawn_agent_run(&state, SpawnAgentParams {
            user_id,
            session_id: context.session_id,
            project_id: context.project_id,
            user_message_id,
            content: content.to_string(),
            automation_mode: automation_mode.clone(),
            supervisor_mode,
            profile_prompt_block,
            system_context: system_context.clone(),
            provider_override: effective_provider_override.clone(),
            model_override: effective_model_override.clone(),
            profile_provider: profile_provider.clone(),
            profile_model: profile_model.clone(),
            attachments: normalize_attachments(&body.attachments),
            user_role: claims.role.clone(),
            nexus_agent_type_hint: body.agent_type_hint.clone(),
        }).await {
            // Avvia il file watcher anche in modalita' agente asincrona.
            update_user_active_project(&state, user_id, context.project_id).await;
            return Ok(Json(json!({
                "sessionId": context.session_id.to_string(),
                "userMessage": user_message,
                "agentRun": {
                    "runId": result.run_id.to_string(),
                    "status": "running",
                    "provider": result.provider,
                    "model": result.model,
                }
            })));
        }
        // Se il caricamento del progetto fallisce, fallback al singolo turn
    }

    let run_turn_result = run_turn(
        &state,
        user_id,
        context.session_id,
        context.project_id,
        profile_id,
        content.to_string(),
        user_message_id,
        body.active_files.clone(),
        Some(system_context),
        effective_provider_override,
        effective_model_override,
        automation_mode,
        normalize_attachments(&body.attachments),
    )
    .await;

    let (assistant_message, orchestrator) = match run_turn_result {
        Ok(result) => result,
        Err(error) => {
            let fallback_metadata = json!({
                "provider": "none",
                "model": "none",
                "intent": "chat",
                "runId": "",
                "error": error.1["error"].as_str().unwrap_or("generation_error"),
                "promptTokens": 0,
                "completionTokens": 0,
                "totalTokens": 0,
                "totalCost": 0.0,
                "currency": "EUR",
                "automationMode": automation_mode.as_str(),
            });
            let assistant_id = insert_message(
                &state.db,
                context.session_id,
                context.project_id,
                "assistant",
                &format!(
                    "Operazione non completata: {}",
                    humanize_ai_error(
                        error.1["error"]
                            .as_str()
                            .unwrap_or("Richiesta non completata")
                    )
                ),
                fallback_metadata,
                Some(user_message_id),
            )
            .await?;
            let row = load_message_by_id(&state.db, assistant_id).await?;
            let assistant = to_message_view(&row)?;
            return Ok(Json(json!({
                "sessionId": context.session_id.to_string(),
                "userMessage": user_message,
                "assistantMessage": assistant,
            })));
        }
    };

    let _ = sqlx::query(
        r#"
        UPDATE chat_sessions
        SET
            title = CASE
                WHEN title = 'New Session' OR title = 'Nuova sessione' THEN $2
                ELSE title
            END,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(context.session_id)
    .bind(summarize_title(content))
    .execute(&state.db)
    .await;

    update_user_active_project(&state, user_id, context.project_id).await;

    Ok(Json(json!({
        "sessionId": context.session_id.to_string(),
        "userMessage": user_message,
        "assistantMessage": assistant_message,
        "run": {
            "id": orchestrator.payload["run_id"].as_str().unwrap_or(""),
            "provider": orchestrator.payload["provider"].as_str().unwrap_or(""),
            "model": orchestrator.payload["model"].as_str().unwrap_or(""),
            "intent": orchestrator.payload["intent"].as_str().unwrap_or("chat"),
        }
    })))
}

pub async fn resend_chat_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(message_id): AxumPath<String>,
    Json(body): Json<SendChatMessageRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let message_id = Uuid::parse_str(&message_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Message id non valido"))?;

    let row = sqlx::query(
        r#"
        SELECT
            m.id,
            m.session_id,
            m.project_id,
            m.role,
            m.content,
            m.request_message_id,
            m.created_at,
            s.user_id
        FROM chat_messages m
        JOIN chat_sessions s ON s.id = m.session_id
        WHERE m.id = $1
        "#,
    )
    .bind(message_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Err(api_error(StatusCode::NOT_FOUND, "Messaggio non trovato"));
    };

    let owner: Option<Uuid> = row.try_get("user_id").unwrap_or(None);
    if owner != Some(user_id) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Messaggio non accessibile",
        ));
    }

    let session_id: Uuid = row
        .try_get("session_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let project_id: Uuid = row
        .try_get("project_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let role: String = row
        .try_get("role")
        .unwrap_or_else(|_| "assistant".to_string());
    let source_user_message_id: Uuid = if role == "user" {
        message_id
    } else if let Some(request_message_id) = row
        .try_get::<Option<Uuid>, _>("request_message_id")
        .unwrap_or(None)
    {
        request_message_id
    } else {
        let created_at: DateTime<Utc> = row
            .try_get("created_at")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT id
            FROM chat_messages
            WHERE session_id = $1
              AND role = 'user'
              AND created_at <= $2
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(session_id)
        .bind(created_at)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            api_error(
                StatusCode::BAD_REQUEST,
                "Impossibile determinare il messaggio utente da reinviare",
            )
        })?
    };

    let source_prompt = sqlx::query_scalar::<_, String>(
        r#"
        SELECT content
        FROM chat_messages
        WHERE id = $1
          AND role = 'user'
        "#,
    )
    .bind(source_user_message_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| {
        api_error(
            StatusCode::BAD_REQUEST,
            "Messaggio utente originale non trovato",
        )
    })?;
    let source_metadata = sqlx::query_scalar::<_, Value>(
        r#"
        SELECT metadata
        FROM chat_messages
        WHERE id = $1
        "#,
    )
    .bind(source_user_message_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .unwrap_or_else(|| json!({}));

    let profile_id = body
        .profile_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "default".to_string());
    let provider_override = body.provider_override.clone().or_else(|| {
        source_metadata
            .get("providerOverride")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    });
    let model_override = body.model_override.clone().or_else(|| {
        source_metadata
            .get("modelOverride")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    });
    let automation_mode = if body.automation_mode.is_some() {
        parse_automation_mode(body.automation_mode.as_deref())
    } else {
        parse_automation_mode(
            source_metadata
                .get("automationMode")
                .and_then(Value::as_str),
        )
    };
    let attachments = if body.attachments.is_empty() {
        serde_json::from_value::<Vec<ChatAttachmentRequest>>(
            source_metadata
                .get("attachments")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )
        .map(|value| normalize_attachments(&value))
        .unwrap_or_default()
    } else {
        normalize_attachments(&body.attachments)
    };
    let attachments_metadata = if body.attachments.is_empty() {
        source_metadata
            .get("attachments")
            .cloned()
            .unwrap_or_else(|| json!([]))
    } else {
        json!(body.attachments.clone())
    };

    let resent_user_message_id = insert_message(
        &state.db,
        session_id,
        project_id,
        "user",
        &source_prompt,
        json!({
            "providerOverride": provider_override.clone(),
            "modelOverride": model_override.clone(),
            "automationMode": automation_mode.as_str(),
            "attachments": attachments_metadata,
            "resendOf": source_user_message_id.to_string(),
        }),
        None,
    )
    .await?;
    let resent_user_row = load_message_by_id(&state.db, resent_user_message_id).await?;
    let resent_user_message = to_message_view(&resent_user_row)?;

    // ── Agent mode per resend (usa la stessa funzione condivisa di send) ──
    if automation_mode != AutomationMode::Study {
        let (profile_prompt_block, _, _, _) =
            fetch_profile_context(&state.db, user_id, &profile_id, &source_prompt).await;
        let github_username: Option<String> = sqlx::query_scalar(
            "SELECT github_username FROM users WHERE id = $1",
        )
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .unwrap_or(None)
        .flatten();
        let system_context_str = {
            let mut ctx = String::from(
                "Sei Nexus, agente operativo di sviluppo. Regole:\n\
                 Output: testo pulito, markdown standard (no emoji, no caratteri grafici).\n\
                 Tool iniziali: read_file, list_files, search_in_files, write_file, edit_file, run_command.\n\
                 Tool aggiuntivi: usa request_tools(categories) per sbloccare categorie extra:\n\
                 - \"git\": git_status, git_stage, git_commit, git_push, git_pull\n\
                 - \"service\": run_service, read_service_output, stop_service\n\
                 - \"files_advanced\": delete_file, rename_file\n\
                 - \"profile\": create_profile, update_profile\n\
                 - \"subtask\": dispatch_subtask\n\
                 - \"mcp\": tool da server MCP esterni\n\
                 Autonomia: NON chiedere mai struttura, tecnologia, OS, comandi — ricava tutto dal contesto progetto o con list_files/read_file.\n\
                 PERO' SE ti mancano informazioni che NON puoi ricavare autonomamente (connection string, API keys, credenziali, \
                 configurazioni specifiche dell'ambiente, password, URL di servizi esterni), DEVI chiedere all'utente. \
                 Non tentare di indovinare valori sensibili. Interrompi il flusso, spiega cosa ti serve e perche', e attendi la risposta.\n\
                 File grandi — REGOLA CRITICA PER PERFORMANCE:\n\
                 read_file restituisce solo le prime 300 righe. Se il file e' piu' grande, usa questo flusso:\n\
                 1. read_file(path) — ottieni le prime 300 righe + totale righe\n\
                 2. read_file_lines(path, start_line, end_line) — leggi un range specifico (max 400 righe per chiamata)\n\
                 3. Se non sai dove si trova la sezione: usa search_in_files o search_codebase_semantic, poi read_file_lines\n\
                 NON caricare file interi grandi. Usa sempre lettura chirurgica per sezioni specifiche.\n\
                 Avvio servizi — REGOLE TASSATIVE:\n\
                 1) Per avviare servizi (server, watcher, processi long-running), usa run_service con label descrittiva.\n\
                 2) Dopo OGNI run_service, LEGGI l'output restituito. Se serve piu' output, usa read_service_output col process_id.\n\
             ANTI-LOOP: non chiamare read_service_output piu' di 3 volte consecutive sullo stesso process_id. Se dopo 3 letture il servizio non e' pronto, smetti di aspettare e riferisci all'utente lo stato attuale. Non eseguire run_command in loop per monitorare uno stesso processo.\n\
                 3) Se l'output contiene errori (exit code != 0, Error, Exception, failed), CORREGGI e RILANCIA (stop_service + run_service).\n\
                 4) Dopo che i servizi sono avviati, VERIFICA con run_command(\"ss -tlnp | grep PORTA\") che le porte siano in ascolto.\n\
                 5) Nella risposta finale, fornisci SEMPRE i link URL (es. http://localhost:5000, http://localhost:5173) dove l'utente puo' aprire i servizi.\n\
                 Errori comuni e correzioni:\n\
                 - Porta occupata: run_command(\"lsof -t -i:PORTA | xargs kill -9\") poi rilancia\n\
                 - .NET TargetFramework errato: controlla con run_command(\"dotnet --list-sdks\"), aggiorna .csproj, rilancia\n\
                 - Build fallita: leggi output, correggi con edit_file, rilancia\n\
                 - npm module not found: run_command(\"npm install\") poi rilancia\n\
                 - SEMPRE rilancia dopo una correzione. Mai fermarsi dopo un fix senza verificare.\n\
                 Persistenza: se un'operazione fallisce, leggi l'errore, analizzalo e riprova. Non arrenderti al primo errore.\n\
                 Git: usa credenziali utente autenticato. Per cloni parti da $NEXUS_TERMINAL_ROOT.\n\
                 Profili: quando noti stack tecnico ricorrente, crea/aggiorna profilo con create_profile/update_profile.",
            );
            if automation_mode != AutomationMode::Study {
                ctx.push_str(crate::prompt_templates::AGENT_ACT_FIRST_SUFFIX);
            }
            if let Some(ref gh) = github_username {
                ctx.push_str(&format!(" Account GitHub: @{gh}."));
            }
            ctx
        };

        if let Some(result) = spawn_agent_run(&state, SpawnAgentParams {
            user_id,
            session_id,
            project_id,
            user_message_id: resent_user_message_id,
            content: source_prompt.clone(),
            automation_mode: automation_mode.clone(),
            supervisor_mode: SupervisorMode::default(),
            profile_prompt_block,
            system_context: system_context_str,
            provider_override: provider_override.clone(),
            model_override: model_override.clone(),
            profile_provider: None,
            profile_model: None,
            attachments: attachments.clone(),
            user_role: claims.role.clone(),
            nexus_agent_type_hint: None, // resend non usa hint
        }).await {
            update_user_active_project(&state, user_id, project_id).await;
            return Ok(Json(json!({
                "sessionId": session_id.to_string(),
                "userMessage": resent_user_message,
                "agentRun": {
                    "runId": result.run_id.to_string(),
                    "status": "running",
                    "provider": result.provider,
                    "model": result.model,
                }
            })));
        }
    }

    // Fallback: orchestrator singolo turno (Study mode o progetto non trovato)
    let (assistant_message, orchestrator) = run_turn(
        &state,
        user_id,
        session_id,
        project_id,
        profile_id,
        source_prompt.clone(),
        resent_user_message_id,
        body.active_files.clone(),
        None,
        provider_override,
        model_override,
        automation_mode,
        attachments,
    )
    .await?;

    update_user_active_project(&state, user_id, project_id).await;

    Ok(Json(json!({
        "sessionId": session_id.to_string(),
        "userMessage": resent_user_message,
        "assistantMessage": assistant_message,
        "run": {
            "id": orchestrator.payload["run_id"].as_str().unwrap_or(""),
            "provider": orchestrator.payload["provider"].as_str().unwrap_or(""),
            "model": orchestrator.payload["model"].as_str().unwrap_or(""),
        }
    })))
}

pub async fn delete_chat_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(message_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let message_id = Uuid::parse_str(&message_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Message id non valido"))?;

    let row = sqlx::query(
        r#"
        UPDATE chat_messages m
        SET deleted_at = NOW(),
            deleted_by_user_id = $2,
            updated_at = NOW()
        FROM chat_sessions s
        WHERE m.id = $1
          AND m.session_id = s.id
          AND s.user_id = $2
        RETURNING m.id, m.session_id, s.project_id
        "#,
    )
    .bind(message_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "Messaggio non trovato o non autorizzato",
        ));
    };

    let project_id: Uuid = row
        .try_get("project_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    update_user_active_project(&state, user_id, project_id).await;

    Ok(Json(json!({
        "ok": true,
        "messageId": message_id.to_string()
    })))
}

pub async fn feedback_error(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(message_id): AxumPath<String>,
    Json(body): Json<FeedbackErrorRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let message_id = Uuid::parse_str(&message_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Message id non valido"))?;
    let comment = body.comment.trim();
    if comment.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il commento di errore e' obbligatorio",
        ));
    }

    let row = sqlx::query(
        r#"
        SELECT
            m.id,
            m.session_id,
            m.project_id,
            m.role,
            m.content,
            m.metadata,
            s.user_id
        FROM chat_messages m
        JOIN chat_sessions s ON s.id = m.session_id
        WHERE m.id = $1
        "#,
    )
    .bind(message_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Err(api_error(StatusCode::NOT_FOUND, "Messaggio non trovato"));
    };

    let owner: Option<Uuid> = row.try_get("user_id").unwrap_or(None);
    if owner != Some(user_id) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Messaggio non accessibile",
        ));
    }

    let role: String = row.try_get("role").unwrap_or_default();
    if role != "assistant" {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il feedback errore e' consentito solo sui messaggi AI",
        ));
    }

    let session_id: Uuid = row
        .try_get("session_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let project_id: Uuid = row
        .try_get("project_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let metadata: Value = row.try_get("metadata").unwrap_or_else(|_| json!({}));
    let ai_response_content: String = row.try_get("content").unwrap_or_default();
    let intent = metadata
        .get("intent")
        .and_then(Value::as_str)
        .unwrap_or("chat")
        .to_lowercase();
    let provider = metadata
        .get("provider")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let model = metadata
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let raw_run_id = metadata
        .get("runId")
        .or_else(|| metadata.get("agentRunId"))
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok());
    // Verifica esistenza in orchestrator_runs (FK target).
    // I run agent moderni vivono in `agent_runs`, ma la FK del feedback punta
    // a `orchestrator_runs`. Se l'ID non esiste li', settiamo NULL invece di
    // far fallire l'insert (la colonna ammette NULL).
    let run_id: Option<Uuid> = match raw_run_id {
        Some(id) => {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM orchestrator_runs WHERE id = $1)",
            )
            .bind(id)
            .fetch_one(&state.db)
            .await
            .unwrap_or(false);
            if exists { Some(id) } else { None }
        }
        None => None,
    };

    // Recupera il messaggio utente precedente nella stessa sessione:
    // è la domanda che ha generato questa risposta AI — usata per costruire
    // un embedding semanticamente ricco che matchi domande simili future.
    let preceding_user_message: Option<String> = sqlx::query_scalar(
        r#"
        SELECT content FROM chat_messages
        WHERE session_id = $1
          AND role = 'user'
          AND created_at < (SELECT created_at FROM chat_messages WHERE id = $2)
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(session_id)
    .bind(message_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    // ── Testo per l'embedding ──────────────────────────────────────────────
    // Concatena domanda utente + commento di correzione.
    // Così quando arriva una domanda semanticamente simile in futuro,
    // il vettore viene trovato con alta similarità.
    let embed_input = match &preceding_user_message {
        Some(q) if !q.is_empty() => format!(
            "{}\n\nCorrezione: {}",
            q.chars().take(800).collect::<String>(),
            comment
        ),
        _ => comment.to_string(),
    };

    // ── correction_text: testo che viene iniettato nel system prompt ───────
    // Deve essere una istruzione chiara e azionabile per l'AI.
    let correction_text = match &preceding_user_message {
        Some(q) if !q.is_empty() => format!(
            "[{}] Quando viene chiesto: «{}» — {}",
            intent,
            q.chars().take(200).collect::<String>(),
            comment
        ),
        _ => format!("[{}] {}", intent, comment),
    };

    // Preview della risposta AI sbagliata (per audit/debug, max 500 chars)
    let ai_response_preview: String = ai_response_content.chars().take(500).collect();

    let feedback_id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO ai_response_feedback (
            id, project_id, session_id, message_id, orchestrator_run_id, user_id,
            feedback_type, intent, provider, model, error_comment, status, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, 'error', $7, $8, $9, $10, 'open', NOW(), NOW())
        "#,
    )
    .bind(feedback_id)
    .bind(project_id)
    .bind(session_id)
    .bind(message_id)
    .bind(run_id)
    .bind(user_id)
    .bind(&intent)
    .bind(&provider)
    .bind(&model)
    .bind(comment)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let correction_id = Uuid::new_v4();
    let point_id = correction_id.to_string();
    let normalized = normalize_text(&correction_text);
    let normalized_hash = hash_hint(project_id, &intent, &normalized);

    sqlx::query(
        r#"
        INSERT INTO prompt_corrections (
            id, project_id, feedback_id, session_id, message_id, orchestrator_run_id,
            intent, provider, model, correction_text, normalized_hint_hash, qdrant_point_id,
            active, status, metadata, created_at, updated_at
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
            TRUE, 'open', $13, NOW(), NOW()
        )
        "#,
    )
    .bind(correction_id)
    .bind(project_id)
    .bind(feedback_id)
    .bind(session_id)
    .bind(message_id)
    .bind(run_id)
    .bind(&intent)
    .bind(&provider)
    .bind(&model)
    .bind(&correction_text)
    .bind(&normalized_hash)
    .bind(&point_id)
    .bind(json!({
        "source": "chat_feedback",
        "requestedBy": user_id.to_string(),
        "userComment": comment,
        "aiResponsePreview": ai_response_preview,
        "userQuestionPreview": preceding_user_message.as_deref().unwrap_or("").chars().take(300).collect::<String>(),
    }))
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Guard: se embedder/qdrant sono down, skip vettorializzazione (la correzione e' gia' in DB)
    let qdrant_ok = state.dependency_status.qdrant.load(std::sync::atomic::Ordering::Relaxed);
    let embedder_ok = state.dependency_status.embedder.load(std::sync::atomic::Ordering::Relaxed);
    if !qdrant_ok || !embedder_ok {
        tracing::info!(
            "corrections: skip vettorializzazione (qdrant={}, embedder={})",
            qdrant_ok, embedder_ok
        );
    } else {
        let vector = state
            .orchestrator
            .embed_text(&embed_input)
            .await
            .map_err(|e| api_error(StatusCode::BAD_GATEWAY, e.to_string()))?;
        vector_memory::upsert_prompt_correction_point(
            &state.db,
            &point_id,
            &vector,
            json!({
                "project_id": project_id.to_string(),
                "correction_id": correction_id.to_string(),
                "feedback_id": feedback_id.to_string(),
                "intent": intent,
                "provider": provider,
                "model": model,
                "text": correction_text,
                "active": true,
                "status": "open",
                "created_at": Utc::now().to_rfc3339(),
                "normalized_hint_hash": normalized_hash,
            }),
        )
        .await
        .map_err(|e| api_error(StatusCode::BAD_GATEWAY, e.to_string()))?;
    }

    let dedup_count = dedup_on_write(
        &state.db,
        project_id,
        &intent,
        &normalized_hash,
        correction_id,
    )
    .await?;
    let learning_action =
        apply_project_learning(&state.db, project_id, user_id, Some(&intent), false).await?;

    Ok(Json(json!({
        "ok": true,
        "feedbackId": feedback_id.to_string(),
        "correctionId": correction_id.to_string(),
        "deduplicatedCount": dedup_count,
        "learning": learning_action
    })))
}

/// Handler feedback positivo (pollice su): conferma esplicita che la risposta AI e' corretta.
///
/// A differenza di `feedback_error`:
/// - registra in `ai_response_feedback` con `feedback_type='positive'`
/// - NON genera `prompt_corrections` ne' embedding Qdrant (positivo = "lascia tutto com'e'")
/// - rinforza il Q-value con reward=1.0 sul `NexusBridge` se il messaggio ha run_id + intent
pub async fn feedback_positive(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(message_id): AxumPath<String>,
    Json(body): Json<FeedbackPositiveRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let message_id = Uuid::parse_str(&message_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Message id non valido"))?;
    let comment = body
        .comment
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");

    let row = sqlx::query(
        r#"
        SELECT
            m.id,
            m.session_id,
            m.project_id,
            m.role,
            m.metadata,
            s.user_id
        FROM chat_messages m
        JOIN chat_sessions s ON s.id = m.session_id
        WHERE m.id = $1
        "#,
    )
    .bind(message_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Err(api_error(StatusCode::NOT_FOUND, "Messaggio non trovato"));
    };

    let owner: Option<Uuid> = row.try_get("user_id").unwrap_or(None);
    if owner != Some(user_id) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Messaggio non accessibile",
        ));
    }

    let role: String = row.try_get("role").unwrap_or_default();
    if role != "assistant" {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il feedback positivo e' consentito solo sui messaggi AI",
        ));
    }

    let session_id: Uuid = row
        .try_get("session_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let project_id: Uuid = row
        .try_get("project_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let metadata: Value = row.try_get("metadata").unwrap_or_else(|_| json!({}));
    let intent = metadata
        .get("intent")
        .and_then(Value::as_str)
        .unwrap_or("chat")
        .to_lowercase();
    let provider = metadata
        .get("provider")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let model = metadata
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let raw_run_id = metadata
        .get("runId")
        .or_else(|| metadata.get("agentRunId"))
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok());
    // Verifica esistenza in orchestrator_runs (FK target).
    // I run agent moderni vivono in `agent_runs`, ma la FK del feedback punta
    // a `orchestrator_runs`. Se l'ID non esiste li', settiamo NULL invece di
    // far fallire l'insert (la colonna ammette NULL).
    let run_id: Option<Uuid> = match raw_run_id {
        Some(id) => {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM orchestrator_runs WHERE id = $1)",
            )
            .bind(id)
            .fetch_one(&state.db)
            .await
            .unwrap_or(false);
            if exists { Some(id) } else { None }
        }
        None => None,
    };
    let agent_type_hint = metadata
        .get("agentType")
        .or_else(|| metadata.get("profile"))
        .and_then(Value::as_str)
        .unwrap_or("chat_default")
        .to_string();

    // Idempotenza: se gia' esiste un feedback positivo per questo messaggio, ritorna quello.
    let existing: Option<Uuid> = sqlx::query_scalar(
        r#"
        SELECT id FROM ai_response_feedback
        WHERE message_id = $1 AND user_id = $2 AND feedback_type = 'positive'
        LIMIT 1
        "#,
    )
    .bind(message_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    if let Some(existing_id) = existing {
        return Ok(Json(json!({
            "ok": true,
            "feedbackId": existing_id.to_string(),
            "alreadyRecorded": true,
            "newQValue": null,
        })));
    }

    let feedback_id = Uuid::new_v4();
    // `error_comment` e' NOT NULL nello schema: salva commento utente o sentinel.
    let comment_to_store = if comment.is_empty() {
        "[positive feedback senza commento]".to_string()
    } else {
        comment.to_string()
    };
    sqlx::query(
        r#"
        INSERT INTO ai_response_feedback (
            id, project_id, session_id, message_id, orchestrator_run_id, user_id,
            feedback_type, intent, provider, model, error_comment, status, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, 'positive', $7, $8, $9, $10, 'resolved', NOW(), NOW())
        "#,
    )
    .bind(feedback_id)
    .bind(project_id)
    .bind(session_id)
    .bind(message_id)
    .bind(run_id)
    .bind(user_id)
    .bind(&intent)
    .bind(&provider)
    .bind(&model)
    .bind(&comment_to_store)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Rinforza Q-learning: reward=1.0 (successo confermato dall'utente).
    let mut new_q_value: Option<f32> = None;
    if let Some(bridge) = crate::nexus_bridge::NexusBridge::global() {
        let task_id = run_id
            .map(|u| u.to_string())
            .unwrap_or_else(|| message_id.to_string());
        let pascal = crate::internal_learning::snake_to_pascal(&agent_type_hint);
        let agent_type = nexus_orchestrator::AgentType::from_name(&pascal);
        let q = bridge.record_outcome(
            &task_id,
            &intent,
            agent_type,
            true,   // success
            1.0,    // reward massimo
            0,      // duration_ms non disponibile qui
            None,
        );
        new_q_value = Some(q);
        tracing::info!(
            "feedback_positive: Q-update task={} intent={} agent={} new_q={}",
            task_id, intent, pascal, q,
        );
    }

    Ok(Json(json!({
        "ok": true,
        "feedbackId": feedback_id.to_string(),
        "alreadyRecorded": false,
        "newQValue": new_q_value,
    })))
}

pub async fn legacy_chat(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<LegacyChatRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = parse_project_id(&body.project_id)?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let existing_session = sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT id
        FROM chat_sessions
        WHERE project_id = $1
          AND user_id = $2
        ORDER BY updated_at DESC
        LIMIT 1
        "#,
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let session_id = if let Some(session_id) = existing_session {
        session_id
    } else {
        let new_session_id = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO chat_sessions (id, project_id, user_id, title, status, created_at, updated_at)
            VALUES ($1, $2, $3, 'Nuova sessione', 'active', NOW(), NOW())
            "#,
        )
        .bind(new_session_id)
        .bind(project_id)
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        new_session_id
    };

    let user_message_id = insert_message(
        &state.db,
        session_id,
        project_id,
        "user",
        &body.message,
        json!({
            "automationMode": "confirm",
            "attachments": [],
        }),
        None,
    )
    .await?;

    let (assistant_message, _) = run_turn(
        &state,
        user_id,
        session_id,
        project_id,
        body.profile_id.clone(),
        body.message.clone(),
        user_message_id,
        body.active_files.clone(),
        None,
        None,
        None,
        AutomationMode::Confirm,
        Vec::new(),
    )
    .await?;

    Ok(Json(json!({
        "content": assistant_message.content,
        "provider": assistant_message.provider,
        "model": assistant_message.model,
        "tokens_used": assistant_message.total_tokens.unwrap_or(0),
        "prompt_tokens": assistant_message.prompt_tokens.unwrap_or(0),
        "completion_tokens": assistant_message.completion_tokens.unwrap_or(0),
        "total_tokens": assistant_message.total_tokens.unwrap_or(0),
        "total_cost": assistant_message.total_cost.unwrap_or(0.0),
        "currency": assistant_message.currency.unwrap_or_else(|| "EUR".to_string()),
        "quota_status": "ok",
        "session_id": session_id.to_string(),
        "request_message_id": user_message_id.to_string(),
        "assistant_message_id": assistant_message.id,
    })))
}

// ── Pre-check messaggio ────────────────────────────────────────────────────
// Analizza un messaggio prima dell'invio: rileva errori ortografici/grammaticali
// e segnala richieste troppo vaghe che richiederebbero contesto aggiuntivo.
// Usa un modello economico/veloce (gpt-4.1-nano) con risposta JSON stretta.

#[derive(Debug, Deserialize)]
pub struct PrecheckRequest {
    pub message: String,
    /// Sessione corrente: se presente, il precheck riceve la cronologia
    /// recente per valutare il messaggio in contesto. Senza, valuta in
    /// isolamento (comportamento pre-fix, marca contestuali come generici).
    #[serde(default, alias = "sessionId")]
    pub session_id: Option<Uuid>,
}


pub async fn precheck_chat_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<PrecheckRequest>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let message = body.message.trim();

    // Non fare il precheck per messaggi molto brevi
    if message.len() < 15 || message.split_whitespace().count() < 3 {
        return Ok(Json(json!({
            "ok": true, "correctedText": null,
            "contextSuggestion": null, "issues": [], "reason": null
        })));
    }

    // Non fare il precheck se sembra codice
    let looks_like_code = message.contains('`')
        || message.contains("```")
        || message.starts_with('/')
        || message.contains("./")
        || message.contains(":\\");
    if looks_like_code {
        return Ok(Json(json!({
            "ok": true, "correctedText": null,
            "contextSuggestion": null, "issues": [], "reason": null
        })));
    }

    let system_prompt = crate::prompt_templates::get_template_or_default(
        &state.db,
        &state.template_cache,
        "chat.precheck_message",
    ).await;

    // Arricchimento contestuale: se il client passa session_id, il precheck
    // riceve gli ultimi turni della conversazione. Risolve i falsi-positivi
    // su follow-up contestuali (es. "riepiloga gli animali" dopo una chat
    // sugli animali) che in isolamento sembrano "troppo generici" ma in
    // contesto sono chiarissimi.
    let effective_message = if let Some(sid) = body.session_id {
        build_message_with_recent_context_for_classifier(&state.db, sid, message).await
    } else {
        message.to_string()
    };

    let messages_json = serde_json::to_string(&json!([
        { "role": "user", "content": effective_message }
    ])).unwrap_or_default();

    // Modello purpose-specific letto da DB (purpose: chat_feedback_generator).
    // Errore esplicito 503 se la matrice non e' caricata o il purpose non e' configurato.
    let matrix_arc = state.orchestrator.routing_matrix.current_async().await
        .map_err(|e| api_error(StatusCode::SERVICE_UNAVAILABLE,
            format!("routing_matrix non disponibile: {e}. Verifica DB e migrazioni 0101/0102.")))?;
    let (provider_pf, model_pf) = matrix_arc
        .purpose_model("chat_feedback_generator")
        .ok_or_else(|| api_error(StatusCode::SERVICE_UNAVAILABLE,
            "purpose 'chat_feedback_generator' non configurato in nexus_purpose_model. \
             Esegui INSERT su nexus_purpose_model con il modello desiderato.".to_string()))?;
    let raw = match state.orchestrator.neural
        .generate_agent_turn(&provider_pf, &model_pf, &messages_json, "[]", 300, &system_prompt)
        .await
    {
        Ok(val) => val
            .get("content").and_then(Value::as_str)
            .unwrap_or("").to_string(),
        Err(_) => {
            // Se il modello non risponde non bloccare l'utente
            return Ok(Json(json!({
                "ok": true, "correctedText": null,
                "contextSuggestion": null, "issues": [], "reason": null
            })));
        }
    };

    // Estrae il JSON anche se il modello ha aggiunto testo prima/dopo
    let json_start = raw.find('{').unwrap_or(0);
    let json_end = raw.rfind('}').map(|i| i + 1).unwrap_or(raw.len());
    let parsed: Value = serde_json::from_str(&raw[json_start..json_end]).unwrap_or_else(|_| json!({
        "ok": true, "correctedText": null,
        "contextSuggestion": null, "issues": [], "reason": null
    }));

    let ok = parsed.get("ok").and_then(Value::as_bool).unwrap_or(true);
    let corrected_text = parsed.get("correctedText").and_then(Value::as_str).map(ToOwned::to_owned)
        // Scarta solo se esattamente identico (byte-by-byte) o vuoto — non usare to_lowercase()
        // perché perderebbe correzioni su accenti o caratteri speciali
        .filter(|c| !c.trim().is_empty() && c.trim() != message.trim());
    let context_suggestion = parsed.get("contextSuggestion").and_then(Value::as_str).map(ToOwned::to_owned)
        .filter(|s| !s.trim().is_empty());
    let issues: Vec<String> = parsed.get("issues").and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(ToOwned::to_owned)).collect())
        .unwrap_or_default();
    let reason = parsed.get("reason").and_then(Value::as_str).map(ToOwned::to_owned);

    // ok=false solo se c'è davvero qualcosa di utile da mostrare
    let effective_ok = if corrected_text.is_none() && context_suggestion.is_none() && issues.is_empty() {
        true
    } else {
        ok
    };

    Ok(Json(json!({
        "ok": effective_ok,
        "correctedText": corrected_text,
        "contextSuggestion": context_suggestion,
        "issues": issues,
        "reason": reason
    })))
}

// ---------------------------------------------------------------------------
// POST /api/chat/feedback-assist
// Aiuta l'utente a formulare una descrizione precisa dell'anomalia nella
// risposta AI. Usa un modello economico; restituisce il testo suggerito.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackAssistRequest {
    /// Contenuto della risposta AI problematica (può essere troncato)
    pub message_content: String,
    /// Descrizione parziale già scritta dall'utente (può essere vuota)
    #[serde(default)]
    pub partial_description: String,
}

pub async fn feedback_assist_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<FeedbackAssistRequest>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;

    let system_prompt = crate::prompt_templates::get_template_or_default(
        &state.db,
        &state.template_cache,
        "chat.feedback_assist",
    ).await;

    if system_prompt.is_empty() {
        return Err(api_error(StatusCode::SERVICE_UNAVAILABLE, "Template non disponibile".to_string()));
    }

    // Tronca il contenuto del messaggio per non eccedere il contesto
    let msg_preview: String = body.message_content.chars().take(1200).collect();
    let partial = body.partial_description.trim().to_string();

    let user_content = if partial.is_empty() {
        format!("RISPOSTA AI:\n{}", msg_preview)
    } else {
        format!("RISPOSTA AI:\n{}\n\nDESCRIZIONE PARZIALE DELL'UTENTE:\n{}", msg_preview, partial)
    };

    let messages_json = serde_json::to_string(&json!([
        { "role": "user", "content": user_content }
    ])).unwrap_or_default();

    // Modello purpose-specific letto da DB (purpose: chat_title_generator).
    // Errore esplicito 503 se la matrice non e' caricata o il purpose non e' configurato.
    let matrix_arc = state.orchestrator.routing_matrix.current_async().await
        .map_err(|e| api_error(StatusCode::SERVICE_UNAVAILABLE,
            format!("routing_matrix non disponibile: {e}. Verifica DB e migrazioni 0101/0102.")))?;
    let (provider_pt, model_pt) = matrix_arc
        .purpose_model("chat_title_generator")
        .ok_or_else(|| api_error(StatusCode::SERVICE_UNAVAILABLE,
            "purpose 'chat_title_generator' non configurato in nexus_purpose_model.".to_string()))?;
    let raw = match state.orchestrator.neural
        .generate_agent_turn(&provider_pt, &model_pt, &messages_json, "[]", 400, &system_prompt)
        .await
    {
        Ok(val) => val.get("content").and_then(Value::as_str).unwrap_or("").trim().to_string(),
        Err(e) => {
            tracing::warn!("feedback_assist LLM error: {}", e);
            return Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
        }
    };

    let suggestion = raw.trim().trim_matches('"').to_string();

    Ok(Json(json!({ "suggestion": suggestion })))
}
