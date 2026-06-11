use super::*;

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
        .filter_map(|h| {
            h.point_id
                .parse::<Uuid>()
                .ok()
                .map(|id| (id, h.score as f32))
        })
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
    out.push_str("_(Fonte: wiki_docs scope=project — KB unificata del progetto, ADR 0017 v2.)_\n");

    tracing::info!(
        project_id = %project_id,
        notes_injected = note_ids.len(),
        "build_knowledge_context: contesto KB iniettato"
    );

    Some(out)
}

/// Punto unico (regola L) di conversione del ruolo DB -> ruolo LLM. I ruoli
/// interni di Nexus non standard per i provider (in particolare 'summary',
/// iniettato dal compact) vengono mappati a 'user'; 'assistant' resta tale.
/// Necessario perche' i servizi a valle (gateway/brain) accettano solo
/// system/user/assistant/tool e rifiutano 'summary' con un errore di
/// deserializzazione ("unknown variant `summary`"). Va usato OVUNQUE si
/// costruiscano i `messages` da inviare a un LLM a partire da chat_messages.
pub(crate) fn db_role_to_llm_role(role: &str) -> &'static str {
    match role {
        "assistant" => "assistant",
        _ => "user",
    }
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
            // Normalizza il ruolo per compatibilità con il formato messages LLM
            // (punto unico, regola L): 'summary' -> 'user'.
            let llm_role = db_role_to_llm_role(&role);
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
        .try_fold(String::new(), |mut s, msg| {
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
        let llm_role = db_role_to_llm_role(role);
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

#[cfg(test)]
mod role_map_tests {
    use super::db_role_to_llm_role;

    #[test]
    fn summary_role_e_mappato_a_user() {
        // Regressione: il ruolo interno 'summary' (compact) NON deve mai
        // arrivare grezzo a un LLM (errore "unknown variant ").
        assert_eq!(db_role_to_llm_role("summary"), "user");
        assert_eq!(db_role_to_llm_role("assistant"), "assistant");
        assert_eq!(db_role_to_llm_role("user"), "user");
        assert_eq!(db_role_to_llm_role("qualsiasi_altro"), "user");
    }
}
