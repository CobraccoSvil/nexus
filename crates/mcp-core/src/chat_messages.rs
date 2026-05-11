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

    format!("{}{}{}", project_header, profile_prompt_block, base)
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
          AND role IN ('user', 'assistant')
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
        // Normalizza il ruolo per compatibilità con il formato messages LLM
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
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("conversation embed fallito (non bloccante): {e}");
                return;
            }
        };
        let point_id = vector_memory::conversation_point_id(session_id, message_id);
        let now = chrono::Utc::now().to_rfc3339();
        if let Err(e) = vector_memory::upsert_conversation_turn(
            &db, &point_id, &vector, session_id, &role, &content, &now,
        ).await {
            tracing::debug!("conversation turn upsert fallito (non bloccante): {e}");
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
    let recent = build_recent_conversation_history(db, session_id, recent_count).await;

    let embed_input = if current_message.len() > 1000 {
        &current_message[..1000]
    } else {
        current_message
    };
    let vector = match neural.embed_text("", embed_input).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("vectorized history: embedding fallito, uso solo recenti: {e}");
            return recent;
        }
    };

    let semantic_hits = match vector_memory::search_conversation_context(
        db, &vector, session_id, semantic_top_k, 0.65,
    ).await {
        Ok(hits) => hits,
        Err(e) => {
            tracing::debug!("vectorized history: ricerca Qdrant fallita, uso solo recenti: {e}");
            return recent;
        }
    };

    if semantic_hits.is_empty() {
        return recent;
    }

    // Raccogli i contenuti recenti per deduplicazione
    let recent_contents: std::collections::HashSet<String> = recent.iter()
        .filter_map(|m| m.get("content").and_then(|v| v.as_str()).map(String::from))
        .collect();

    // Converti hit semantici in messaggi, escludendo duplicati
    let mut semantic_msgs: Vec<(String, serde_json::Value)> = Vec::new();
    for hit in &semantic_hits {
        let role = hit.payload.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = hit.payload.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let created_at = hit.payload.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        if content.is_empty() || recent_contents.contains(content) {
            continue;
        }
        let llm_role = if role == "assistant" { "assistant" } else { "user" };
        semantic_msgs.push((
            created_at.to_string(),
            json!({ "role": llm_role, "content": content }),
        ));
    }

    if semantic_msgs.is_empty() {
        return recent;
    }

    // Ordina semantici per data
    semantic_msgs.sort_by(|a, b| a.0.cmp(&b.0));

    // Combina: semantici prima (contesto storico), poi recenti (contesto immediato)
    let mut combined: Vec<serde_json::Value> = semantic_msgs.into_iter().map(|(_, m)| m).collect();
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

    // Routing intelligente: Neural Core classifica l'intent e sceglie il provider ottimale
    // (es. "fix" → anthropic, "chat" → openai, ecc.) invece di usare sempre il primo in lista.
    // Il profile_provider ha priorità sul routing automatico, ma non sul provider_override utente.
    let effective_override = params
        .provider_override
        .filter(|v| !v.trim().is_empty())
        .or_else(|| params.profile_provider.filter(|v| !v.trim().is_empty()));
    let effective_model_override = params
        .model_override
        .filter(|v| !v.trim().is_empty())
        .or_else(|| params.profile_model.filter(|v| !v.trim().is_empty()));

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

    // History ibrida: ultimi 2 raw + top-6 semanticamente rilevanti da Qdrant.
    // Se Qdrant/embedding non disponibile, fallback a ultimi 8 raw.
    let vec_deps_ok = state.dependency_status.qdrant.load(std::sync::atomic::Ordering::Relaxed)
        && state.dependency_status.embedder.load(std::sync::atomic::Ordering::Relaxed);
    let recent_history = if vec_deps_ok {
        build_vectorized_conversation_history(
            &state.db,
            &state.orchestrator.neural,
            params.session_id,
            &params.content,
            2,  // ultimi 2 messaggi sempre inclusi
            6,  // top-6 semantici
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
            "SELECT name, engine, connection_secret, is_primary \
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

    let system_text = format!(
        "{}{}{}{}{}{}", project_header, project_custom_instructions,
        automation_instructions, test_instructions,
        params.profile_prompt_block, params.system_context
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

    // Calcola il payload tools dinamico (discovery mode vs inline) prima dello spawn
    let tools_json_for_brain = crate::brain_agent_client::build_tools_json_for_agent(
        &state.db,
        params.user_id,
        params.project_id,
    )
    .await;

    tokio::spawn(async move {
        tracing::info!(
            "spawn_agent_run: delega al brain LangGraph run_id={}",
            run_id
        );

        // ── Loop di retry con fallback automatico tra provider ───────────────
        // Se il run fallisce per "credit too low" / "quota exceeded", il provider
        // viene messo in cooldown lungo (in brain_agent_client). Qui rileviamo
        // il fallimento e ritentiamo con il prossimo provider della gerarchia
        // ammin (escludendo quelli in cooldown).
        const MAX_PROVIDER_FALLBACKS: usize = 4;
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

        loop {
            tried.insert(current_provider.to_lowercase());
            tracing::info!(
                "agent_run {}: tentativo {}/{} con provider={} model={}",
                run_id, fallback_attempt + 1, MAX_PROVIDER_FALLBACKS, current_provider, current_model
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
            )
            .await;

            // Decide se ritentare: nuova logica basata su error_class strutturato
            // propagato dal brain via SSE, oltre allo stato cooldown del provider.
            // Casi che giustificano un retry su altro provider:
            //   - provider in cooldown (lungo o breve, gia' marcato dal brain_agent_client)
            //   - error_class in {billing_error, rate_limit, provider_error}
            //   - il run e' fallito con stop_reason=error (anche senza classify, ritenta una volta)
            let should_retry = matches!(result.status, AgentRunStatus::Failed) && {
                let in_cooldown = crate::provider_cooldown::is_provider_in_cooldown(&current_provider);
                let retriable_class = matches!(
                    result.error_class.as_deref(),
                    Some("billing_error") | Some("rate_limit") | Some("provider_error")
                );
                in_cooldown || retriable_class
            };
            if !should_retry || fallback_attempt + 1 >= MAX_PROVIDER_FALLBACKS {
                break;
            }

            // Cerca il prossimo provider nella gerarchia, non già provato e non in cooldown
            let next = provider_hierarchy.iter().find(|p| {
                !tried.contains(*p) && !crate::provider_cooldown::is_provider_in_cooldown(p)
            });
            let Some(next_provider) = next else {
                tracing::warn!("agent_run {}: nessun provider alternativo disponibile, mantengo errore", run_id);
                break;
            };
            current_provider = next_provider.clone();
            // Modello di default per il nuovo provider, letto da DB (registry routing).
            // Se la matrice non e' caricata o il provider non e' configurato,
            // mantengo il modello corrente invece di applicare un fallback hardcoded.
            // Background task: log warn ma non blocco il run.
            current_model = match routing_matrix_for_loop.current_async().await {
                Ok(matrix_arc) => matrix_arc
                    .default_model(&current_provider)
                    .unwrap_or_else(|| {
                        tracing::warn!(
                            "agent_run {}: provider '{}' non configurato in nexus_provider_default_model, mantengo modello corrente",
                            run_id, current_provider
                        );
                        current_model
                    }),
                Err(e) => {
                    tracing::error!(
                        "agent_run {}: routing_matrix non disponibile ({}), mantengo modello corrente",
                        run_id, e
                    );
                    current_model
                }
            };
            fallback_attempt += 1;
            tracing::warn!(
                "agent_run {}: provider precedente in cooldown — fallback automatico a {}/{}",
                run_id, current_provider, current_model
            );
        }
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

        // Save final answer as assistant message
        if let Some(ref answer) = result.final_answer {
            let meta = json!({
                "provider": &result.provider,
                "model": &result.model,
                "agentRunId": run_id.to_string(),
                "iterationCount": result.iteration_count,
                "automationMode": "agent",
                "privacyRerouted": privacy_rerouted,
            });
            let _ = sqlx::query(
                r#"INSERT INTO chat_messages
                   (id, session_id, project_id, role, content, metadata, request_message_id, created_at)
                   VALUES (gen_random_uuid(),$1,$2,'assistant',$3,$4,$5,NOW())"#,
            )
            .bind(session_id_cp)
            .bind(project_id_cp)
            .bind(answer)
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
                answer.clone(),
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
        .execute(&db_clone)
        .await;
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
                        )
                        .await;

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
            let check_provider = effective_provider_override.as_deref().unwrap_or("anthropic");
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
    let run_id = metadata
        .get("runId")
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok());

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

    let messages_json = serde_json::to_string(&json!([
        { "role": "user", "content": message }
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
