use super::*;

/// Parametri condivisi per avviare un agent run (usato da send e resend).
pub(crate) struct SpawnAgentParams {
    pub(crate) user_id: Uuid,
    pub(crate) session_id: Uuid,
    pub(crate) project_id: Uuid,
    pub(crate) user_message_id: Uuid,
    pub(crate) content: String,
    pub(crate) automation_mode: AutomationMode,
    pub(crate) supervisor_mode: SupervisorMode,
    pub(crate) profile_prompt_block: String,
    pub(crate) system_context: String,
    pub(crate) provider_override: Option<String>,
    pub(crate) model_override: Option<String>,
    pub(crate) profile_provider: Option<String>,
    pub(crate) profile_model: Option<String>,
    pub(crate) attachments: Vec<ChatAttachment>,
    /// Ruolo utente JWT (es. "admin", "editor") — per i tool nexus_builtin
    pub(crate) user_role: String,
    /// Agent type hint dal client (bypassa Q-Learning se presente)
    pub(crate) nexus_agent_type_hint: Option<String>,
}
/// Risultato di spawn_agent_run: (run_id, provider, model)
pub(crate) struct SpawnAgentResult {
    pub(crate) run_id: Uuid,
    pub(crate) provider: String,
    pub(crate) model: String,
}
/// Troncamento per caratteri (mai per byte: evita di spezzare sequenze UTF-8).
pub(crate) fn trunc_chars(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        s
    } else {
        s.chars().take(max).collect()
    }
}

/// Resoconto deterministico delle azioni eseguite dall'agente (ADR 0025).
///
/// Usato come risposta finale quando il modello chiude il turno senza body
/// (hollow / completamento vuoto) MA ha comunque eseguito tool: invece di un
/// generico "nessuna risposta", l'utente vede cosa e' stato fatto. Nessuna
/// chiamata LLM: rete di sicurezza garantita, indipendente da provider/cooldown.
/// Ritorna `None` se non c'e' alcuna azione concreta (l'agente non ha fatto
/// nulla) — in quel caso il chiamante usa il placeholder generico.
fn build_action_recap(steps: &[AgentStep]) -> Option<String> {
    use std::collections::BTreeSet;
    let mut lines: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut files_touched: BTreeSet<String> = BTreeSet::new();
    for step in steps {
        if step.status != AgentStepStatus::Completed || step.tool_name.is_empty() {
            continue;
        }
        // Dettaglio leggibile dall'input del tool, in ordine di preferenza.
        let detail = ["path", "file", "command", "pattern", "query"]
            .iter()
            .find_map(|k| step.tool_input.get(*k).and_then(|v| v.as_str()))
            .map(|s| trunc_chars(s.to_string(), 120));
        let line = match &detail {
            Some(d) => format!("- `{}`: {}", step.tool_name, d),
            None => format!("- `{}`", step.tool_name),
        };
        if seen.insert(line.clone()) {
            lines.push(line);
        }
        if matches!(
            step.tool_name.as_str(),
            "write_file" | "edit_file" | "create_file" | "apply_patch"
        ) {
            if let Some(p) = step.tool_input.get("path").and_then(|v| v.as_str()) {
                files_touched.insert(p.to_string());
            }
        }
    }
    if lines.is_empty() {
        return None;
    }
    let mut out = String::from("Task completato. Azioni eseguite dall'agente:\n");
    out.push_str(&lines.join("\n"));
    if !files_touched.is_empty() {
        let files: Vec<String> = files_touched.iter().map(|f| format!("`{f}`")).collect();
        out.push_str(&format!("\n\nFile creati/modificati: {}", files.join(", ")));
    }
    out.push_str(
        "\n\n_(Riepilogo generato automaticamente: l'agente ha eseguito le azioni \
         sopra ma non ha prodotto un messaggio finale. Verifica i risultati.)_",
    );
    Some(out)
}
/// Costruisce il messaggio iniziale per il brain arricchendolo con il contenuto
/// reale degli allegati (pre-extraction nel prompt — ADR 0010/0011/0012).
///
/// ROOT CAUSE storico: senza questo arricchimento, quando l'utente allega un
/// file (es. un `.make` Figma con la specifica di un'app) il modello riceve solo
/// il testo "crea l'app descritta nel file" SENZA il contenuto del file, e finisce
/// per allucinare o generare un Hello World. Qui pre-estraiamo il contenuto
/// autoritativo (Figma/PDF/DOCX/testo) e lo iniettiamo in un blocco `<allegati>`.
///
/// Politica "mai troncare-e-buttare" (mig 0216): il contenuto completo degli
/// allegati e' indicizzato in RAG (vedi `rag::index_attachment`) e recuperato
/// semanticamente qui (`rag::search_semantic`), senza budget arbitrario che
/// tagli dati. Gli estratti iniettati nel prompt sono i chunk piu' rilevanti
/// (cap difensivo per-chunk `CHUNK_INJECT_CAP`, non un budget di sessione); il
/// resto resta accessibile via `nexus_search_semantic`. Estrazioni fallite
/// degradano con metadata + nota, mai panic.
pub(crate) async fn build_initial_msg_with_attachments(
    db: &PgPool,
    content: &str,
    attachments: &[crate::orchestrator::ChatAttachment],
    user_message_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
) -> String {
    if attachments.is_empty() {
        return content.to_string();
    }

    let n = attachments.len();

    // Cap difensivo per singolo chunk iniettato: i chunk RAG sono gia' limitati
    // da chunk_size, ma evitiamo che un chunk patologico gonfi il prompt.
    const CHUNK_INJECT_CAP: usize = 8_000;

    // Risolvo path fisici e stato di indicizzazione leggendo direttamente
    // chat_message_attachments: serve file_path (per index sincrono di
    // fallback), mime/kind e chunk_count.
    struct AttRow {
        id: String,
        file_name: String,
        file_path: String,
        mime_type: String,
        chunk_count: i64,
    }
    let saved_rows: Vec<AttRow> = match sqlx::query(
        r#"SELECT id, file_name, file_path, mime_type, chunk_count
           FROM chat_message_attachments WHERE message_id = $1 ORDER BY created_at ASC"#,
    )
    .bind(user_message_id)
    .fetch_all(db)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|r| AttRow {
                id: r
                    .try_get::<Uuid, _>("id")
                    .map(|u| u.to_string())
                    .unwrap_or_default(),
                file_name: r.try_get("file_name").unwrap_or_default(),
                file_path: r.try_get("file_path").unwrap_or_default(),
                mime_type: r.try_get("mime_type").unwrap_or_default(),
                chunk_count: r.try_get("chunk_count").unwrap_or(0),
            })
            .collect(),
        Err(e) => {
            tracing::warn!(
                user_message_id = %user_message_id,
                error = %e,
                "initial_msg: lettura chat_message_attachments fallita, fallback metadata"
            );
            Vec::new()
        }
    };

    // Blocco di fallback con soli metadata + istruzione tool, usato quando il
    // RAG e' disabilitato o non produce hit. Mai contenuto inventato.
    let metadata_block = |reason: &str| -> String {
        let mut b = String::new();
        b.push_str("<allegati>\n");
        b.push_str(&format!(
            "L'utente ha allegato {} file. Il contenuto integrale non e' inline qui ({}). \
             DEVI investigarlo prima di rispondere.\n\n## File allegati:\n",
            n, reason
        ));
        for att in attachments.iter() {
            let id_label = att.id.map(|u| format!(" [ID: {}]", u)).unwrap_or_default();
            b.push_str(&format!(
                "- {} ({}, {} byte){}\n",
                att.name, att.mime_type, att.size_bytes, id_label
            ));
        }
        b.push_str(
            "\nISTRUZIONE: per ogni allegato chiama nexus_inspect_attachment(id) e poi il tool \
             di estrazione consigliato (nexus_extract_pdf_text / nexus_extract_docx_text / \
             nexus_extract_figma_structure / nexus_read_attachment), oppure \
             nexus_search_semantic(query, filter_attachment_id) sul contenuto vettorializzato. \
             NON generare un placeholder, NON un Hello World, NON inventare un dominio diverso.\n",
        );
        b.push_str("</allegati>");
        b
    };

    // RAG abilitato? Se no o config non disponibile, fallback metadata.
    let cfg = match crate::rag::current_config(db).await {
        Ok(c) if c.enabled => c,
        Ok(_) => {
            tracing::info!("initial_msg: RAG disabilitato, fallback metadata + tool");
            return format!("{}\n\n{}", content, metadata_block("RAG disabilitato"));
        }
        Err(e) => {
            tracing::warn!("initial_msg: config RAG non disponibile ({e}), fallback metadata");
            return format!(
                "{}\n\n{}",
                content,
                metadata_block("configurazione RAG non disponibile")
            );
        }
    };

    // Index sincrono di fallback: l'auto-index al persist e' fire-and-forget,
    // quindi i chunk potrebbero non essere pronti. Per ogni allegato non ancora
    // indicizzato (chunk_count=0) indicizziamo ORA, sincrono, prima della search.
    let mut current_ids: Vec<String> = Vec::with_capacity(n);
    for att in attachments.iter() {
        let row = att
            .id
            .map(|id| id.to_string())
            .and_then(|id_str| saved_rows.iter().find(|r| r.id == id_str))
            .or_else(|| saved_rows.iter().find(|r| r.file_name == att.name));
        let Some(row) = row else {
            continue;
        };
        current_ids.push(row.id.clone());

        if row.chunk_count <= 0 {
            let Ok(att_uuid) = Uuid::parse_str(&row.id) else {
                continue;
            };
            match crate::rag::index_attachment(
                db,
                att_uuid,
                std::path::PathBuf::from(&row.file_path),
                row.mime_type.clone(),
                row.file_name.clone(),
                Some(project_id),
                Some(session_id),
            )
            .await
            {
                Ok(nc) => tracing::info!(
                    "initial_msg: index sincrono allegato {} -> {} chunks",
                    row.id,
                    nc
                ),
                Err(e) => tracing::warn!(
                    "initial_msg: index sincrono allegato {} fallito: {}",
                    row.id,
                    e
                ),
            }
        }
    }

    // RAG retrieval: cerca i chunk piu' rilevanti tra i soli allegati.
    let hits = match crate::rag::search_semantic(
        db,
        content,
        vec![crate::rag::SourceKind::Attachment],
        Some(project_id),
        Some(session_id),
        Some(cfg.top_k_default),
        Vec::new(),
    )
    .await
    {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("initial_msg RAG: search fallita ({e}), fallback metadata");
            return format!(
                "{}\n\n{}",
                content,
                metadata_block("recupero semantico non disponibile")
            );
        }
    };

    // Tieni solo gli hit appartenenti agli allegati di QUESTO messaggio.
    let relevant: Vec<_> = hits
        .into_iter()
        .filter(|h| current_ids.iter().any(|id| id == &h.source_id))
        .collect();

    if relevant.is_empty() {
        tracing::info!("initial_msg RAG: 0 hit rilevanti, fallback metadata + tool");
        return format!(
            "{}\n\n{}",
            content,
            metadata_block("nessun estratto rilevante recuperato dal contenuto vettorializzato")
        );
    }

    let name_for = |source_id: &str| -> String {
        attachments
            .iter()
            .find(|a| a.id.map(|u| u.to_string()).as_deref() == Some(source_id))
            .map(|a| a.name.clone())
            .or_else(|| {
                saved_rows
                    .iter()
                    .find(|r| r.id == source_id)
                    .map(|r| r.file_name.clone())
            })
            .unwrap_or_else(|| source_id.to_string())
    };

    let mut block = String::new();
    block.push_str("<allegati>\n");
    block.push_str(&format!(
        "L'utente ha allegato {} file. Sotto trovi gli estratti piu' rilevanti rispetto alla tua \
         richiesta, recuperati semanticamente dal contenuto completo dei file (vettorializzato). \
         Il contenuto completo e' disponibile via il tool nexus_search_semantic(query, \
         filter_attachment_id) per approfondire qualsiasi aspetto.\n\n## File allegati:\n",
        n
    ));
    for att in attachments.iter() {
        let id_label = att.id.map(|u| format!(" [ID: {}]", u)).unwrap_or_default();
        block.push_str(&format!(
            "- {} ({}, {} byte){}\n",
            att.name, att.mime_type, att.size_bytes, id_label
        ));
    }

    block.push_str("\n## Estratti rilevanti:\n");
    let n_hits = relevant.len();
    for h in relevant.iter() {
        let chunk = trunc_chars(h.chunk_text.clone(), CHUNK_INJECT_CAP);
        block.push_str(&format!(
            "\n[score {:.2}, da {}]\n{}\n",
            h.score,
            name_for(&h.source_id),
            chunk
        ));
    }

    block.push_str(
        "\nISTRUZIONE: il contenuto sopra e' la specifica reale fornita dall'utente. Implementa \
         ESATTAMENTE quanto descritto, con le funzionalita' specifiche indicate. Se ti serve piu' \
         contesto chiama nexus_search_semantic(query=\"...\", filter_attachment_id=\"<id>\"). NON \
         generare un placeholder, NON un Hello World, NON inventare un dominio diverso da quello \
         descritto.\n",
    );
    block.push_str("</allegati>");

    tracing::info!(
        attachments = n,
        chunks_retrieved = n_hits,
        block_chars = block.len(),
        "initial_msg RAG: {} allegati, {} chunk recuperati, blocco {} chars",
        n,
        n_hits,
        block.len()
    );

    format!("{}\n\n{}", content, block)
}
/// Logica condivisa: carica progetto, costruisce contesto, avvia AgentLoop in background.
/// Ritorna `None` se il progetto non è caricabile (fallback al singolo turn).
pub(crate) async fn spawn_agent_run(
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
    )
    .await;
    let classified = state
        .orchestrator
        .classify_intent_full(&classifier_input)
        .await;
    if classified.is_ambiguous && !matches!(params.automation_mode, AutomationMode::Automatic) {
        tracing::info!(
            "spawn_agent_run: intent ambiguo (conf={:.2}, candidati={}), chiedo disambiguazione",
            classified.confidence,
            classified.candidates.len(),
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
    let slot_routing_hit = if params.provider_override.is_none() && params.model_override.is_none()
    {
        state
            .orchestrator
            .route_by_slots(&effective_slots, 0.60)
            .await
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
    let effective_model_override =
        if let Some((_slot_provider, slot_model, _src)) = &slot_routing_hit {
            Some(slot_model.clone())
        } else {
            params
                .model_override
                .filter(|v| !v.trim().is_empty())
                .or_else(|| params.profile_model.filter(|v| !v.trim().is_empty()))
        };
    if let Some((p, m, src)) = &slot_routing_hit {
        tracing::info!("spawn_agent_run: routing slot-based {} → {}/{}", src, p, m);
    }

    // Conta i messaggi esistenti nella sessione per calibrare il routing:
    // sessioni con molti messaggi indicano task lunghi (es. "continua") che
    // richiedono modelli più capaci anche se il messaggio è breve.
    let context_message_count =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM chat_messages WHERE session_id = $1")
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
            params.session_id,
            alert_msg,
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
            thinking_delta: None,
            meta_step: None,
        });
        return Some(SpawnAgentResult {
            run_id,
            provider,
            model: model_str,
        });
    }

    // ── AUTO-COMPACT a soglia (regola H: fix strutturale all'overflow del
    // contesto) ─────────────────────────────────────────────────────────────
    // Prima di costruire la history e avviare il turno, valuta il rapporto
    // token sessione / context window del modello risolto. Se supera la soglia
    // configurabile (DB-driven, regola G), compatta automaticamente la sessione
    // riusando la stessa logica del compact manuale (compact_session_core).
    // Best-effort: ogni fallimento e' loggato WARN e il turno prosegue.
    maybe_auto_compact(
        state,
        params.session_id,
        params.project_id,
        &provider,
        &model_str,
    )
    .await;

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
    let vec_deps_ok = state
        .dependency_status
        .qdrant
        .load(std::sync::atomic::Ordering::Relaxed)
        && state
            .dependency_status
            .embedder
            .load(std::sync::atomic::Ordering::Relaxed);
    let recent_history = if vec_deps_ok {
        build_vectorized_conversation_history(
            &state.db,
            &state.orchestrator.neural,
            params.session_id,
            &params.content,
            4, // ultimi 4 messaggi raw = 2 turni completi user+assistant
            6, // top-6 semantici dalla storia piu' vecchia (soglia 0.40)
        )
        .await
    } else {
        // Dipendenze vettoriali down: usa solo gli ultimi messaggi raw
        build_recent_conversation_history(&state.db, params.session_id, 8).await
    };
    // Versione testuale compatta solo per logging
    let recent_context = build_recent_conversation_context(&state.db, params.session_id, 4).await;
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

    let analysis_summary: Option<String> =
        analysis_json_opt.and_then(|analysis: serde_json::Value| {
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
                        name,
                        label,
                        engine.as_deref().unwrap_or("unknown"),
                        dsn_val
                    ));
                } else {
                    block.push_str(&format!(
                        "  - {}{}: engine={} (nessuna connection string configurata)\n",
                        name,
                        label,
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
        let is_test_intent = l.contains("test")
            || l.contains("testa")
            || l.contains("verifica che funzion")
            || l.contains("tdd")
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
    let self_ref_hint = build_self_referential_hint(&state.db, params.session_id, &params.content)
        .await
        .unwrap_or_default();

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
        "{}{}{}{}{}{}{}{}",
        project_header,
        project_custom_instructions,
        automation_instructions,
        o_series_instructions,
        test_instructions,
        params.profile_prompt_block,
        params.system_context,
        self_ref_hint
    );
    // Costruzione del messaggio iniziale arricchito con il contenuto reale
    // degli allegati (ADR 0010/0011/0012 — pre-extraction nel prompt).
    //
    // ROOT CAUSE storico: senza questo blocco, quando l'utente allega un file
    // (es. un .make Figma con la specifica di un'app) il modello riceve solo il
    // testo "crea l'app descritta nel file" SENZA il contenuto del file, e
    // finisce per allucinare o generare un Hello World. Qui pre-estraiamo il
    // contenuto autoritativo e lo iniettiamo in un blocco <allegati>.
    //
    // La history recente viene passata come turns strutturati via resume_history.
    // La costruzione del blocco <allegati> e' estratta in build_initial_msg_with_attachments
    // (funzione dedicata) per non gonfiare ulteriormente spawn_agent_run, che e'
    // gia' enorme: una closure complessa inline qui faceva degenerare il typeck
    // del compilatore (ICE).
    let initial_msg = build_initial_msg_with_attachments(
        &state.db,
        &params.content,
        &params.attachments,
        params.user_message_id,
        params.project_id,
        params.session_id,
    )
    .await;

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
    // Cloni per i monitor automatici del pannello Monitor (regola H: il run si
    // auto-documenta senza dipendere dal fatto che il modello chiami
    // `dispatcher_update_monitor`). Usati dal task ascoltatore sotto e dal
    // monitor finale (completato/errore) emesso a fine run.
    let monitor_registry_for_run = state.monitor_registry.clone();
    let project_channels_for_run = state.project_channels.clone();
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
    let sse_max_silence_secs: u64 =
        match state.orchestrator.routing_thresholds.current_async().await {
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

    // ── Monitor automatici del run (regola H, indipendenti dall'LLM) ─────────
    // Il pannello Monitor si popola guardando lo STREAM degli step del run
    // (eventi gia' prodotti dal parsing SSE del brain), non da chiamate del
    // modello a `dispatcher_update_monitor`. Un task dedicato si sottoscrive al
    // broadcast del run e traduce gli step in poche card chiave:
    //   - `agent_run`  -> stato del run ("in corso", poi "completato"/"errore")
    //   - `agent_tool` -> nome dell'ultimo tool eseguito (+ target file in label)
    //   - `agent_files`-> contatore file toccati (write_file/edit_file)
    // Sottoscriviamo PRIMA di spawnare il run cosi' nessun evento si perde.
    {
        let mut step_rx = tx_for_brain.subscribe();
        let mon_reg = monitor_registry_for_run.clone();
        let mon_ch = project_channels_for_run.clone();
        let mon_project = project_id_cp;
        // Stato iniziale immediato: il pannello mostra "in corso" appena parte.
        crate::agent_tools::monitor::set_monitor(
            &mon_reg,
            &mon_ch,
            mon_project,
            "agent_run",
            serde_json::Value::String("in corso".to_string()),
            Some("avvio run agente".to_string()),
        );
        tokio::spawn(async move {
            let mut files_touched: u64 = 0;
            loop {
                let ev = match step_rx.recv().await {
                    Ok(ev) => ev,
                    // Lagged: alcuni eventi persi (buffer pieno). Continuiamo:
                    // i monitor sono best-effort, non un log esaustivo.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    // Tutti i sender chiusi: il run e' finito. Lo stato finale
                    // (completato/errore) lo emette il corpo del run con
                    // result.status (qui non lo conosciamo). Usciamo.
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                if ev.is_final {
                    break;
                }
                let Some(step) = ev.step else { continue };
                // Aggiorna `agent_tool` quando un tool inizia (status Running):
                // value = nome tool, label = target file se ricavabile dall'input.
                if step.status == AgentStepStatus::Running && !step.tool_name.is_empty() {
                    let target = step
                        .tool_input
                        .get("file_path")
                        .or_else(|| step.tool_input.get("path"))
                        .or_else(|| step.tool_input.get("file"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string);
                    let label = target
                        .clone()
                        .map(|t| format!("step {} · {}", step.step_index, t))
                        .unwrap_or_else(|| format!("step {}", step.step_index));
                    crate::agent_tools::monitor::set_monitor(
                        &mon_reg,
                        &mon_ch,
                        mon_project,
                        "agent_tool",
                        serde_json::Value::String(step.tool_name.clone()),
                        Some(label),
                    );
                }
                // Contatore file toccati: incrementa quando write_file/edit_file
                // si completa con successo.
                if step.status == AgentStepStatus::Completed
                    && matches!(step.tool_name.as_str(), "write_file" | "edit_file")
                {
                    files_touched = files_touched.saturating_add(1);
                    crate::agent_tools::monitor::set_monitor(
                        &mon_reg,
                        &mon_ch,
                        mon_project,
                        "agent_files",
                        serde_json::Value::from(files_touched),
                        Some("file modificati".to_string()),
                    );
                }
            }
        });
    }

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
                    AND agentic_thinking_policy <> 'exclude'
                    AND consecutive_failures = 0",
                )
                .fetch_one(&db_clone)
                .await
                .unwrap_or(4);
                std::cmp::max(2, (n as usize).saturating_add(1))
            };
            let provider_hierarchy: Vec<String> = {
                let row: Option<String> = sqlx::query_scalar(
                    "SELECT value FROM settings WHERE key = 'provider_hierarchy' LIMIT 1",
                )
                .fetch_optional(&db_clone)
                .await
                .ok()
                .flatten();
                row.map(|s| {
                    s.split(',')
                        .map(|t| t.trim().to_lowercase())
                        .filter(|t| !t.is_empty())
                        .collect()
                })
                .unwrap_or_else(|| {
                    vec![
                        "anthropic".into(),
                        "openai".into(),
                        "google".into(),
                        "deepseek".into(),
                        "mistral".into(),
                    ]
                })
            };

            let mut current_provider = provider_clone.clone();
            let mut current_model = model_clone.clone();
            let mut tried: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut result;
            let mut fallback_attempt: usize = 0;

            // ── Fix B+C: stima tokens richiesti e scelta context-aware ──────────
            // Approssimazione (1 token = ~4 caratteri): system prompt + msg utente
            // + storia conversazione + descrizioni tool. Usata per:
            //   B) troncare history se eccede 70% ctx del modello selezionato
            //   C) pre-filtrare il routing escludendo modelli con ctx insufficiente
            let estimated_input_chars: usize = {
                let history_chars: usize = recent_history_for_brain
                    .iter()
                    .map(|m| {
                        m.get("content")
                            .and_then(|c| c.as_str())
                            .map(|s| s.len())
                            .unwrap_or(0)
                    })
                    .sum();
                let tools_chars: usize = serde_json::to_string(&tools_json_for_brain)
                    .map(|s| s.len())
                    .unwrap_or(0);
                system_text_clone.len() + initial_msg_clone.len() + history_chars + tools_chars
            };
            let estimated_input_tokens: i64 = (estimated_input_chars / 4) as i64;
            tracing::info!(
                "agent_run {}: input stimato {} tokens (~{} chars)",
                run_id,
                estimated_input_tokens,
                estimated_input_chars
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
                // Re-routing context-aware: path AGENTICO. PUNTO UNICO di selezione
                // (regola L): l'eleggibilita' agentica (tool_use, policy<>'exclude',
                // consecutive_failures, cooldown) e' definita una sola volta in
                // select_agentic_model. Vincolo extra: context_window >= ctx_needed.
                let alt = crate::orchestrator::select_agentic_model(
                    &db_clone,
                    &[],
                    None,
                    ctx_needed,
                    &[],
                    "input_cost_per_million_tokens ASC NULLS LAST",
                )
                .await;
                if let Some((p, m)) = alt {
                    tracing::info!(
                        "agent_run {}: routing context-aware: {} -> {}/{}",
                        run_id,
                        current_model,
                        p,
                        m
                    );
                    current_provider = p;
                    current_model = m;
                }
            }

            // ADR 0023 (Fix 3a): se il re-routing context-aware ha cambiato il
            // modello rispetto a quello registrato a spawn (provider_clone/
            // model_clone), allinea il record agent_runs al modello EFFETTIVO
            // con cui il run partira'. Cosi' header e badge dei meta-step (che
            // leggono agentRun.provider/model) convergono sul modello reale.
            // Best-effort: un fallimento qui non deve bloccare il run.
            if current_provider != provider_clone || current_model != model_clone {
                let _ = sqlx::query("UPDATE agent_runs SET provider = $1, model = $2 WHERE id = $3")
                    .bind(&current_provider)
                    .bind(&current_model)
                    .bind(run_id)
                    .execute(&db_clone)
                    .await;
                tracing::info!(
                    "agent_run {}: agent_runs.provider/model aggiornato al modello effettivo {}/{} (era {}/{})",
                    run_id,
                    current_provider,
                    current_model,
                    provider_clone,
                    model_clone
                );
            }

            loop {
                tried.insert(current_provider.to_lowercase());
                tracing::info!(
                    "agent_run {}: tentativo {}/{} con provider={} model={} (ctx_needed={})",
                    run_id,
                    fallback_attempt + 1,
                    max_provider_fallbacks,
                    current_provider,
                    current_model,
                    ctx_needed
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
                let is_infrastructure_error = result
                    .final_answer
                    .as_ref()
                    .map(|s| {
                        let lower = s.to_lowercase();
                        lower.contains("sandbox")
                            && (lower.contains("gr pc")
                                || lower.contains("grpc")
                                || lower.contains("connession")
                                || lower.contains("non e' raggiungibile")
                                || lower.contains("non raggiungibile"))
                            || lower.contains("50500")
                            || lower.contains("tool_runner")
                            || lower.contains("toolrunner")
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
                        && result
                            .final_answer
                            .as_ref()
                            .map(|s| !s.trim().is_empty())
                            .unwrap_or(false);

                    // ── B: tool-failure model-specific (MALFORMED / output-vuoto su tool) ──
                    // `hollow_no_tools` = il modello aveva tool esposti ma non ne ha
                    // invocato nessuno al primo turno: e' il segnale runtime di
                    // finish_reason=MALFORMED_FUNCTION_CALL / output vuoto sul
                    // tool-forcing (es. gemini-2.5-pro sui task agentici). Questo NON
                    // significa che il modello sia rotto in assoluto: funziona per i
                    // task chat. Quindi NON tocchiamo is_enabled (che lo escluderebbe
                    // ANCHE dai task chat) ma incrementiamo un contatore DEDICATO
                    // (consecutive_tool_failures) e a soglia marchiamo
                    // supports_tool_use=false. L'auto-promoter, che per gli intent con
                    // requires_tool_use filtra su supports_tool_use, lo escludera' dai
                    // soli intent agentici lasciandolo per chat; il cleanup pass (A)
                    // disattivera' poi la riga matrix agentica gia' presente.
                    if result.hollow_no_tools {
                        let tool_threshold: i32 = crate::settings::get_setting(
                            &db_clone,
                            "agent.model_tool_failure_threshold",
                        )
                        .await
                        .ok()
                        .flatten()
                        .and_then(|v| v.trim().parse::<i32>().ok())
                        .filter(|n| *n > 0)
                        .unwrap_or(3);

                        // Incrementa il contatore DEDICATO e decide l'azione con la
                        // funzione pura testata (agent_types::tool_failure_action).
                        let new_count: Option<i32> = sqlx::query_scalar(
                            "UPDATE ai_price_catalog
                            SET consecutive_tool_failures = consecutive_tool_failures + 1,
                                updated_at = NOW()
                          WHERE provider = $1 AND model = $2
                        RETURNING consecutive_tool_failures",
                        )
                        .bind(&result.provider)
                        .bind(&result.model)
                        .fetch_optional(&db_clone)
                        .await
                        .ok()
                        .flatten();
                        if let Some(n) = new_count {
                            let action = crate::agent_types::tool_failure_action(
                                true,
                                true,
                                true,
                                false,
                                n - 1,
                                tool_threshold,
                            );
                            tracing::warn!(
                            "agent_run {}: tool-failure (MALFORMED/empty su tool) su {}/{} — tool_counter={}/{}",
                            run_id, result.provider, result.model, n, tool_threshold
                        );
                            if matches!(
                                action,
                                crate::agent_types::ToolCapabilityAction::MarkNonToolCapable
                            ) {
                                let _ = sqlx::query(
                                    "UPDATE ai_price_catalog
                                    SET supports_tool_use = false,
                                        auto_disabled_reason = 'malformed_tool_calls',
                                        updated_at = NOW()
                                  WHERE provider = $1 AND model = $2
                                    AND supports_tool_use = true",
                                )
                                .bind(&result.provider)
                                .bind(&result.model)
                                .execute(&db_clone)
                                .await;
                                tracing::warn!(
                                "MARK NON-TOOL-CAPABLE {}/{} dopo {} tool-failure consecutivi (supports_tool_use=false). Resta disponibile per i task chat.",
                                result.provider, result.model, n
                            );
                            }
                        }
                    } else if result.hollow_completion {
                        // Hollow generico NON dovuto al tool-forcing (empty answer /
                        // resigned con content): mantiene la semantica storica sul
                        // contatore consecutive_failures -> is_enabled=false a soglia 3.
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
                                run_id,
                                result.provider,
                                result.model,
                                n
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
                                    result.provider,
                                    result.model,
                                    n
                                );
                            }
                        }
                    } else if success_now {
                        // Turno-con-tool andato a buon fine: reset di ENTRAMBI i
                        // contatori (generico e tool-specific) e riabilita la
                        // tool-capability se era stata revocata per malformed.
                        let _ = sqlx::query(
                            "UPDATE ai_price_catalog
                            SET consecutive_failures = 0,
                                consecutive_tool_failures = 0,
                                supports_tool_use = CASE
                                    WHEN auto_disabled_reason = 'malformed_tool_calls' THEN true
                                    ELSE supports_tool_use END,
                                auto_disabled_at = NULL,
                                auto_disabled_reason = CASE
                                    WHEN auto_disabled_reason = 'malformed_tool_calls' THEN NULL
                                    ELSE auto_disabled_reason END,
                                updated_at = NOW()
                          WHERE provider = $1 AND model = $2
                            AND (consecutive_failures > 0
                                 OR consecutive_tool_failures > 0
                                 OR auto_disabled_reason = 'malformed_tool_calls')",
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
                    let in_cooldown =
                        crate::provider_cooldown::is_provider_in_cooldown(&current_provider);
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
                //
                // G1 override: se il messaggio utente e' una richiesta d'azione
                // (avvia/installa/configura/docker/...) forziamo il retry ANCHE se
                // l'intent classifier ha classificato come "chat" — in questo caso
                // la classificazione e' probabile mente errata e la risposta senza
                // tool e' sempre un fallimento.
                let is_action_request =
                    crate::agent_types::detect_action_request(&initial_msg_clone);
                // Intent AUTORITATIVO: quello del router del brain propagato in
                // nexus_task_type, NON la pre-classificazione locale di mcp-core
                // (che diverge: mcp-core passa i tool, il brain li azzera per le
                // chat dirette -> had_tools=true marcava hollow a torto). Evita il
                // retry/cascade hollow spurio quando il brain ha instradato come
                // 'chat'. Fallback al locale se il task_type non e' propagato.
                let brain_intent = result
                    .nexus_task_type
                    .as_deref()
                    .unwrap_or(classified_intent_for_loop);
                let hollow_retry = result.hollow_completion
                    && (brain_intent != "chat" || is_action_request);
                let should_retry = failed_retry || hollow_retry;

                if !should_retry || fallback_attempt + 1 >= max_provider_fallbacks {
                    break;
                }

                if hollow_retry {
                    tracing::warn!(
                        "agent_run {}: hollow completion da {}/{} — il modello ha risposto \
                     senza usare tool, ritento con un modello piu capace",
                        run_id,
                        current_provider,
                        current_model
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
                    // Escalation su hollow ricorrente: PUNTO UNICO di selezione
                    // (regola L). Eleggibilita' agentica + cooldown definiti una
                    // sola volta in select_agentic_model. Esclude i provider gia'
                    // provati; preferisce i piu' "potenti" (tier desc, costo desc) e
                    // con context_window sufficiente.
                    crate::orchestrator::select_agentic_model(
                        &db_clone,
                        &[],
                        None,
                        ctx_needed,
                        &tried_models,
                        "CASE performance_tier WHEN 'heavy' THEN 2 WHEN 'medium' THEN 1 ELSE 0 END DESC, \
                         input_cost_per_million_tokens DESC NULLS LAST, \
                         output_cost_per_million_tokens DESC NULLS LAST",
                    )
                    .await
                    .map(|(p, m)| {
                        tracing::warn!(
                            "agent_run {}: ESCALATION hollow ricorrente — salto a {}/{} (selettore unico)",
                            run_id, p, m
                        );
                        (p, m)
                    })
                } else {
                    None
                };

                let (chosen_provider, chosen_model) = if let Some(pair) = next_pair {
                    pair
                } else {
                    // Cerca il prossimo provider nella gerarchia che sia:
                    //   - non gia' provato in questo run
                    //   - non in cooldown billing/quota
                    //   - dotato di un default model in nexus_provider_default_model
                    //   - con coppia (provider, model) coerente (guard-rail anti-mismatch)
                    //
                    // INVARIANTE: provider e model devono SEMPRE appartenere allo
                    // stesso provider. Un provider senza default model viene SKIPPATO
                    // nel fallback, mai accoppiato al model del provider precedente.
                    // Fonte di verita: nexus_provider_default_model (regola G); i
                    // prefix in model_belongs_to_provider sono detection. Vedi ADR 0016.
                    //
                    // Se la routing_matrix non e disponibile non si puo decidere un
                    // model coerente -> break (manteniamo il result corrente).
                    let matrix_arc = match routing_matrix_for_loop.current_async().await {
                        Ok(m) => m,
                        Err(e) => {
                            tracing::error!(
                            "agent_run {}: routing_matrix non disponibile ({}), interrompo fallback e mantengo risultato",
                            run_id, e
                        );
                            break;
                        }
                    };
                    let mut chosen: Option<(String, String)> = None;
                    for candidate in provider_hierarchy.iter() {
                        if tried.contains(candidate)
                            || crate::provider_cooldown::is_provider_in_cooldown(candidate)
                        {
                            continue;
                        }
                        let Some(candidate_model) = matrix_arc.default_model(candidate) else {
                            tracing::warn!(
                            "agent_run {}: provider '{}' senza default model in nexus_provider_default_model, skip nel fallback",
                            run_id, candidate
                        );
                            continue;
                        };
                        // Guard-rail: la coppia (provider, model) deve essere coerente.
                        // Previene QUALSIASI mismatch: se il default model non
                        // appartiene al provider, NON tentiamo la chiamata (404).
                        if !model_belongs_to_provider(candidate, &candidate_model) {
                            tracing::error!(
                            "agent_run {}: coppia incoerente provider='{}' model='{}' in nexus_provider_default_model, skip nel fallback",
                            run_id, candidate, candidate_model
                        );
                            continue;
                        }
                        chosen = Some((candidate.clone(), candidate_model));
                        break;
                    }
                    let Some(pair) = chosen else {
                        tracing::warn!(
                        "agent_run {}: nessun provider alternativo con default model coerente disponibile, mantengo risultato",
                        run_id
                    );
                        break;
                    };
                    pair
                };
                // Invariante difensiva finale: anche i candidati da escalation
                // hollow (next_pair) passano per il guard-rail. Una coppia
                // incoerente non deve mai diventare current_provider/model.
                if !model_belongs_to_provider(&chosen_provider, &chosen_model) {
                    tracing::error!(
                    "agent_run {}: coppia incoerente scelta provider='{}' model='{}', interrompo fallback (guard-rail)",
                    run_id, chosen_provider, chosen_model
                );
                    break;
                }
                current_provider = chosen_provider;
                current_model = chosen_model;
                fallback_attempt += 1;
                tracing::warn!(
                    "agent_run {}: fallback automatico a {}/{} (motivo: {})",
                    run_id,
                    current_provider,
                    current_model,
                    if hollow_retry {
                        "hollow completion"
                    } else {
                        "provider error/cooldown"
                    }
                );
                // Meta-step `fallback` pubblicato in chat per trasparenza:
                // utente vede in tempo reale che il sistema ha cambiato
                // provider/modello (es. anthropic -> openai per quota_exceeded).
                let reason = if hollow_retry {
                    "hollow_completion"
                } else {
                    "provider_error_or_cooldown"
                };
                let _ = tx_for_brain.send(AgentStepEvent {
                    run_id: run_id.to_string(),
                    step: None,
                    trace: None,
                    is_final: false,
                    token_delta: None,
                    thinking_delta: None,
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
                thinking_delta: None,
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
            // senza invocare alcun tool. Per intent `chat` questo e' atteso (il
            // brain azzera i tool: chat diretta) e NON va segnalato come avviso.
            // Intent AUTORITATIVO dal router del brain (nexus_task_type), non la
            // pre-classificazione locale di mcp-core: quest'ultima divergeva e,
            // combinata con had_tools=true (mcp-core passa i tool, il brain li
            // azzera), produceva l'avviso "0 tool / risposta generica" fuorviante
            // sulle chat dirette. Fallback al locale se il task_type non c'e'.
            let effective_intent = result
                .nexus_task_type
                .as_deref()
                .unwrap_or(classified_intent_for_loop);
            let conversational_intent = effective_intent == "chat";
            let report_hollow = result.hollow_completion && !conversational_intent;
            if report_hollow {
                tracing::warn!(
                    "agent_run {}: hollow completion rilevato — il modello {}/{} \
                 non ha eseguito alcun tool. La risposta potrebbe essere incompleta.",
                    run_id,
                    result.provider,
                    result.model
                );

                // ── QW2: diagnostica persistente in nexus_provider_empty_responses ──
                // Toggle via setting agent.diagnostics.empty_response_log_enabled.
                let diag_enabled: bool = sqlx::query_scalar::<_, String>(
                    "SELECT value FROM settings WHERE key = 'agent.diagnostics.empty_response_log_enabled'",
                )
                .fetch_optional(&db_clone)
                .await
                .ok()
                .flatten()
                .map(|v| v.trim().eq_ignore_ascii_case("true"))
                .unwrap_or(true);

                if diag_enabled {
                    let max_bytes: usize = sqlx::query_scalar::<_, String>(
                        "SELECT value FROM settings WHERE key = 'agent.diagnostics.empty_response_excerpt_max_bytes'",
                    )
                    .fetch_optional(&db_clone)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(8192usize);

                    let raw = result.final_answer.as_deref().unwrap_or("");
                    let excerpt: String = if raw.len() > max_bytes {
                        let mut end = max_bytes;
                        while !raw.is_char_boundary(end) && end > 0 {
                            end -= 1;
                        }
                        format!("{}\n[...truncated at {} bytes...]", &raw[..end], max_bytes)
                    } else {
                        raw.to_string()
                    };

                    let suspected = match result.hollow_completion_kind.as_str() {
                        "EMPTY_ANSWER" | "EMPTY_ANSWER+NO_TOOLS" => {
                            if raw.trim().is_empty() {
                                "empty_completion_unknown"
                            } else {
                                "empty_after_text"
                            }
                        }
                        "RESIGNED" => "resigned_after_few_steps",
                        "NO_TOOLS" => "no_tool_calls",
                        _ => "unknown",
                    };

                    let _ = sqlx::query(
                        r#"
                        INSERT INTO nexus_provider_empty_responses
                            (agent_run_id, chat_session_id, project_id, provider, model,
                             intent, kind, iteration, steps_count, final_answer_chars,
                             est_input_tokens, est_output_tokens,
                             raw_response_excerpt, suspected_cause)
                        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
                        "#,
                    )
                    .bind(run_id)
                    .bind(session_id_cp)
                    .bind(project_id_cp)
                    .bind(&result.provider)
                    .bind(&result.model)
                    .bind(classified_intent_for_loop.as_ref() as &str)
                    .bind(if result.hollow_completion_kind.is_empty() {
                        "UNKNOWN"
                    } else {
                        result.hollow_completion_kind.as_ref()
                    })
                    .bind(result.iteration_count as i32)
                    .bind(result.steps.len() as i32)
                    .bind(raw.len() as i32)
                    .bind(result.prompt_tokens as i32)
                    .bind(result.completion_tokens as i32)
                    .bind(&excerpt)
                    .bind(suspected)
                    .execute(&db_clone)
                    .await
                    .inspect_err(|e| {
                        tracing::debug!(
                            "diagnostica empty_response: INSERT best-effort fallita: {e}"
                        );
                    });
                }
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
                // Final answer mancante o vuoto: siamo qui DOPO il retry loop
                // (hollow_completion confermato e tentativi esauriti). Se l'agente
                // ha comunque ESEGUITO azioni concrete (tool completati), produci
                // un recap deterministico (ADR 0025) cosi' l'utente vede cosa e'
                // stato fatto invece di un generico "nessuna risposta". Solo se
                // non c'e' alcuna azione si usa il placeholder generico.
                _ if report_hollow => build_action_recap(&result.steps).or_else(|| {
                    Some(format!(
                        "_(Nessuna risposta utile prodotta dall'agente — {} / {} ha chiuso \
                     il turno con un completamento vuoto dopo aver esaurito i tentativi \
                     di fallback. Riformula la richiesta o cambia provider/modello manualmente.)_",
                        result.provider, result.model
                    ))
                }),
                _ => None,
            };

            if let Some(ref answer) = answer_owned {
                // Annota la risposta solo se l'intent richiedeva tool e l'agente
                // ha prodotto un body (per evitare doppia annotazione sul placeholder).
                let had_real_body = result
                    .final_answer
                    .as_ref()
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
            // ADR 0023 (Fix 3a): aggiorna anche provider/model col valore
            // EFFETTIVO usato dal run (result.provider/result.model). Cattura i
            // cascade fallback avvenuti dentro il loop (es. primario -> openai
            // su billing_error), che il blocco context-aware pre-loop non vede.
            // L'header, leggendo agentRun dopo getAgentRun(), mostra il modello
            // reale dell'esecuzione, non quello registrato a spawn.
            let _ = sqlx::query(
                "UPDATE agent_runs SET status=$2, final_answer=$3, iteration_count=$4, \
             prompt_tokens=$5, completion_tokens=$6, total_tokens=$7, total_cost=$8, \
             nexus_override_applied=$9, nexus_agent_type=$10, nexus_task_type=$11, \
             provider=$12, model=$13, \
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
            .bind(&result.provider)
            .bind(&result.model)
            .execute(&db_clone)
            .await;

            // ── Monitor finale del run (regola H, indipendente dall'LLM) ───────
            // Porta la card `agent_run` allo stato terminale. Non cancelliamo i
            // monitor: restano visibili come ultimo stato del run.
            let (run_state, run_label): (&str, String) = match result.status {
                AgentRunStatus::Completed => (
                    "completato",
                    format!("{} step · {} iter", result.steps.len(), result.iteration_count),
                ),
                AgentRunStatus::AwaitingConfirmation => {
                    ("in attesa conferma", "conferma utente richiesta".to_string())
                }
                _ => (
                    "errore",
                    result
                        .error_class
                        .clone()
                        .unwrap_or_else(|| status_str.to_string()),
                ),
            };
            crate::agent_tools::monitor::set_monitor(
                &monitor_registry_for_run,
                &project_channels_for_run,
                project_id_cp,
                "agent_run",
                serde_json::Value::String(run_state.to_string()),
                Some(run_label),
            );

            // ── G4: memorizza startup_command dopo avvio servizio riuscito ─────
            // Se il run è completato con successo e ha eseguito un `docker compose up`,
            // salva il comando in memory_entries → al turno successivo l'agente lo
            // trova in "Memoria di progetto" e sa già cosa eseguire.
            if matches!(result.status, AgentRunStatus::Completed) {
                crate::agent_types::save_startup_command_if_needed(
                    &db_clone,
                    project_id_cp,
                    &result.steps,
                )
                .await;
            }

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
            let cost_to_charge: f64 =
                if result.total_cost > 0.0 {
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
                        let output_cost =
                            (result.completion_tokens as f64) * p.output_cost / 1_000_000.0;
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
                thinking_delta: None,
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
