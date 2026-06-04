use super::*;

#[allow(dead_code)]
pub(crate) fn build_static_system_context(github_username: Option<&str>) -> String {
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
pub(crate) async fn build_knowledge_context(
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

    // 3. Search Qdrant (ADR 0017 v2 F8: collection unificata `wiki_content`,
    //    filtro Qdrant su scope=project + project_id esatto).
    use serde_json::json;
    let filter = json!({
        "must": [
            { "key": "scope", "match": { "value": "project" } },
            { "key": "project_id", "match": { "value": project_id.to_string() } },
        ]
    });
    let hits = match crate::vector_memory::search_wiki_content_points_filtered(
        &state.db,
        vector,
        top_k * 2, // overfetch, filtreremo per score
        min_score as f64,
        Some(filter),
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
        .filter_map(|h| h.point_id.parse::<Uuid>().ok().map(|id| (id, h.score as f32)))
        .take(top_k)
        .collect();
    if note_ids.is_empty() {
        return None;
    }

    // 4. Carica title+body+tags+intent dai wiki_docs (scope=project).
    use sqlx::Row;
    let ids: Vec<Uuid> = note_ids.iter().map(|(id, _)| *id).collect();
    let rows = sqlx::query(
        r#"
        SELECT id, title, body_md, tags, intent
        FROM wiki_docs
        WHERE id = ANY($1)
          AND scope = 'project'
          AND project_id = $2
        "#,
    )
    .bind(&ids)
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .ok()?;
    if rows.is_empty() {
        return None;
    }

    // 5. Format markdown (ordinato per score). `status` non esiste piu' in
    //    wiki_docs (ADR 0017 v2): manteniamo placeholder "active" per non
    //    rompere il rendering del prompt.
    let mut by_id: std::collections::HashMap<
        Uuid,
        (String, String, Vec<String>, Option<String>, String),
    > = std::collections::HashMap::new();
    for r in &rows {
        let id: Uuid = r.try_get("id").ok()?;
        let title: String = r.try_get("title").unwrap_or_default();
        let body: String = r.try_get("body_md").unwrap_or_default();
        let tags: Vec<String> = r.try_get("tags").unwrap_or_default();
        let intent: Option<String> = r.try_get("intent").ok().flatten();
        let status: String = "active".to_string();
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
        "_(Fonte: wiki_docs scope=project — KB unificata del progetto, ADR 0017 v2.)_\n",
    );

    tracing::info!(
        project_id = %project_id,
        notes_injected = note_ids.len(),
        "build_knowledge_context: contesto KB iniettato"
    );

    Some(out)
}
#[allow(dead_code)]
pub(crate) async fn load_project_analysis_summary(db: &PgPool, project_id: Uuid) -> Option<String> {
    sqlx::query_scalar::<_, serde_json::Value>("SELECT analysis_json FROM projects WHERE id = $1")
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
pub(crate) async fn build_composed_system_context(
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

    format!(
        "{}{}{}{}",
        services_block, project_header, profile_prompt_block, base
    )
}
/// Carica gli ultimi `limit` messaggi della sessione come turn LLM strutturati.
/// Restituisce un Vec di { "role": "user"|"assistant", "content": "..." }
/// pronti da passare come history iniziale all'agent loop.
pub(crate) async fn build_recent_conversation_history(
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
    rows.into_iter()
        .rev()
        .filter_map(|row| {
            let role = row.try_get::<String, _>("role").ok()?;
            let content = row.try_get::<String, _>("content").ok()?;
            if content.trim().is_empty() {
                return None;
            }
            // Normalizza il ruolo per compatibilità con il formato messages LLM.
            // 'summary' (iniettato dal compact) viene inviato come messaggio user
            // — il content e' gia' prefissato con "[Riassunto ...]".
            let llm_role = match role.as_str() {
                "assistant" => "assistant",
                _ => "user",
            };
            Some(serde_json::json!({ "role": llm_role, "content": content }))
        })
        .collect()
}
/// Versione testuale compatta (usata solo per logging)
pub(crate) async fn build_recent_conversation_context(
    db: &PgPool,
    session_id: Uuid,
    limit: i64,
) -> String {
    let msgs = build_recent_conversation_history(db, session_id, limit).await;
    if msgs.is_empty() {
        return String::new();
    }
    let entries: Vec<String> = msgs
        .iter()
        .filter_map(|m| {
            let role = m.get("role")?.as_str()?;
            let content = m.get("content")?.as_str()?;
            let compact = content
                .replace('\n', " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let clipped = if compact.chars().count() > 120 {
                format!("{}...", compact.chars().take(120).collect::<String>())
            } else {
                compact
            };
            Some(format!("- {}: {}", role, clipped))
        })
        .collect();
    if entries.is_empty() {
        String::new()
    } else {
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
pub(crate) async fn build_message_with_recent_context_for_classifier(
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
/// Salva l'embedding di un turno conversazionale in Qdrant (fire-and-forget).
pub(crate) fn spawn_embed_conversation_turn(
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
                    v.len(),
                    session_id,
                    role,
                    message_id
                );
                v
            }
            Err(e) => {
                tracing::warn!(
                    "conversation_embed: FALLITO session={} role={} msg_id={}: {e}",
                    session_id,
                    role,
                    message_id
                );
                return;
            }
        };
        let point_id = vector_memory::conversation_point_id(session_id, message_id);
        let now = chrono::Utc::now().to_rfc3339();
        if let Err(e) = vector_memory::upsert_conversation_turn(
            &db, &point_id, &vector, session_id, &role, &content, &now,
        )
        .await
        {
            tracing::warn!(
                "conversation_upsert: FALLITO point={} session={}: {e}",
                point_id,
                session_id
            );
        } else {
            tracing::info!(
                "conversation_upsert: OK point={} session={}",
                point_id,
                session_id
            );
        }
    });
}
/// Costruisce la conversation history usando una strategia ibrida:
/// ultimi `recent_count` messaggi raw (contesto immediato) + top-K
/// messaggi semanticamente rilevanti dalla collection Qdrant.
/// I risultati vengono deduplicati e ordinati cronologicamente.
pub(crate) async fn build_vectorized_conversation_history(
    db: &PgPool,
    neural: &crate::orchestrator::NeuralCoreClient,
    session_id: Uuid,
    current_message: &str,
    recent_count: i64,
    semantic_top_k: u64,
) -> Vec<serde_json::Value> {
    const RAW_FALLBACK: i64 = 8;

    // ── Fast-path messaggi conversazionali brevi ───────────────────────────
    // L'embedding + ricerca semantica Qdrant costa ~6-8s in totale (model
    // call all-MiniLM + Qdrant search + dedup). Per messaggi tipo "ciao",
    // "grazie", "ok" non c'e' valore aggiunto: la ricerca semantica ritorna
    // hit irrilevanti e il modello risponde comunque dal contesto recente.
    // Saltiamo direttamente al raw fallback risparmiando ~8s per turno.
    //
    // Soglia: 24 char (es. "ciao come stai oggi?" = 20 char, "grazie mille tante" = 18).
    // Sotto questa soglia il messaggio e' quasi sempre conversazionale e la
    // ricerca semantica non aggiunge segnale utile.
    let trimmed = current_message.trim();
    if trimmed.len() < 24 {
        tracing::info!(
            "vectorized history: fast-path msg breve (len={}) per session={}, skip embedding+Qdrant, uso {RAW_FALLBACK} raw",
            trimmed.len(), session_id,
        );
        return build_recent_conversation_history(db, session_id, RAW_FALLBACK).await;
    }

    let recent = build_recent_conversation_history(db, session_id, recent_count).await;

    // Costruzione dell'input di embedding: includiamo l'ULTIMA iterazione
    // (user+assistant) prima del messaggio corrente. Questo aggancia la
    // ricerca semantica al tema della conversazione, non solo al testo letterale
    // del turno corrente. Esempio: "si elenca" da solo matcha qualsiasi "elenca
    // X" passato; con il turno precedente ("quanti utenti / 4 utenti") il
    // vettore si concentra sul tema utenti.
    let mut embed_input = String::new();
    if let Some(last_turn) = recent
        .iter()
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .iter()
        .rev()
        .fold(Some(String::new()), |acc, msg| {
            let mut s = acc?;
            let role = msg.get("role")?.as_str()?;
            let content = msg.get("content")?.as_str()?;
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(role);
            s.push_str(": ");
            s.push_str(content);
            Some(s)
        })
    {
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
    let vector =
        match neural.embed_text("", &embed_input).await {
            Ok(v) => {
                tracing::warn!(
                "vectorized history: embed OK (con ultimo turno), dim={}, session={}, input_len={}",
                v.len(), session_id, embed_input.len()
            );
                v
            }
            Err(e) => {
                tracing::warn!(
                    "vectorized history: embedding fallito, fallback a {RAW_FALLBACK} raw: {e}"
                );
                return build_recent_conversation_history(db, session_id, RAW_FALLBACK).await;
            }
        };

    let semantic_hits = match vector_memory::search_conversation_context(
        db,
        &vector,
        session_id,
        semantic_top_k,
        0.40,
    )
    .await
    {
        Ok(hits) => {
            let scores: Vec<f64> = hits.iter().map(|h| h.score).collect();
            tracing::warn!(
                "vectorized history: ricerca Qdrant OK, {} hit(s) per session={}, scores={:?}",
                hits.len(),
                session_id,
                scores
            );
            hits
        }
        Err(e) => {
            tracing::warn!(
                "vectorized history: ricerca Qdrant fallita, fallback a {RAW_FALLBACK} raw: {e}"
            );
            return build_recent_conversation_history(db, session_id, RAW_FALLBACK).await;
        }
    };

    if semantic_hits.is_empty() {
        tracing::warn!(
            "vectorized history: 0 hit semantici per session={}, fallback a {RAW_FALLBACK} raw",
            session_id
        );
        return build_recent_conversation_history(db, session_id, RAW_FALLBACK).await;
    }

    // Raccogli i contenuti recenti per deduplicazione
    let recent_contents: std::collections::HashSet<String> = recent
        .iter()
        .filter_map(|m| m.get("content").and_then(|v| v.as_str()).map(String::from))
        .collect();

    // Il timestamp del piu' vecchio dei recenti: tutto cio' che e' >= a questo
    // e' gia' coperto dai raw, quindi va escluso dai semantici per evitare
    // doppioni e per preservare l'ordine cronologico finale.
    let oldest_recent_ts: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar::<_, chrono::DateTime<chrono::Utc>>(
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
        let role = hit
            .payload
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("user");
        let content = hit
            .payload
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let created_at = hit
            .payload
            .get("created_at")
            .and_then(|v| v.as_str())
            .unwrap_or("");
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
        let llm_role = if role == "assistant" {
            "assistant"
        } else {
            "user"
        };
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
    let mut combined: Vec<serde_json::Value> =
        semantic_msgs.into_iter().map(|(_, _, m)| m).collect();
    combined.extend(recent);
    combined
}
