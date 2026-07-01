use super::*;

/// Soglia minima di messaggi user/assistant non soft-deletati sotto la quale
/// l'auto-compact NON scatta: su sessioni cortissime il compact sarebbe un
/// no-op (o quasi) e sprecherebbe una chiamata di summarization.
pub(crate) const AUTO_COMPACT_MIN_MESSAGES: i64 = 4;
/// Context window di fallback (token) quando il modello risolto per il turno
/// non ha una riga in `ai_price_catalog`. Valore prudente: meglio compattare
/// un po' prima del necessario che mai. Loggato a WARN quando usato.
pub(crate) const AUTO_COMPACT_FALLBACK_CONTEXT_WINDOW: i64 = 128_000;
/// Valuta il rapporto token sessione / context window del modello risolto e,
/// se l'auto-compact e' abilitato e la soglia e' superata, compatta la sessione
/// PRIMA del turno agente. Best-effort: niente errore propagato, il turno
/// prosegue sempre. Nessun leak in log (solo ratio/soglia/counts/session_id).
pub(crate) async fn maybe_auto_compact(
    state: &AppState,
    session_id: Uuid,
    project_id: Uuid,
    provider: &str,
    model: &str,
) {
    let cfg = crate::context_settings::current(&state.db).await;
    if !cfg.enabled {
        return;
    }

    // chat_messages e' migrata al DB per-progetto: risolvi il pool una volta e
    // usalo per le query su chat_messages (le metriche di compattazione).
    let chat_pool = crate::project_db_routes::project_data_pool(state, project_id).await;

    // Conteggio messaggi user/assistant compattabili: evita il compact su
    // sessioni troppo corte (sarebbe un no-op).
    let compactable: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM chat_messages \
         WHERE session_id = $1 AND deleted_at IS NULL \
           AND role IN ('user', 'assistant')",
    )
    .bind(session_id)
    .fetch_one(&chat_pool)
    .await
    .unwrap_or(0);
    if compactable < AUTO_COMPACT_MIN_MESSAGES {
        return;
    }

    // Token sessione: somma di (metadata->>'totalTokens') sui messaggi non
    // soft-deletati; fallback a stima ~4 char/token sul contenuto quando il
    // dato non e' persistito. COALESCE per messaggi senza metriche.
    let session_tokens: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM( \
            COALESCE( \
              NULLIF(metadata->>'totalTokens','')::bigint, \
              GREATEST(char_length(content) / 4, 1) \
            ) \
         ), 0)::bigint \
         FROM chat_messages \
         WHERE session_id = $1 AND deleted_at IS NULL",
    )
    .bind(session_id)
    .fetch_one(&chat_pool)
    .await
    .unwrap_or(0);
    if session_tokens <= 0 {
        return;
    }

    // Context window del modello risolto, dal catalog. Fallback prudente con WARN.
    let context_window: i64 = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT context_window FROM ai_price_catalog \
         WHERE provider = $1 AND model = $2 LIMIT 1",
    )
    .bind(provider)
    .bind(model)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .flatten()
    .filter(|w| *w > 0)
    .unwrap_or_else(|| {
        tracing::warn!(
            %session_id, provider, model,
            "auto_compact: context_window non risolvibile dal catalog, uso fallback {}",
            AUTO_COMPACT_FALLBACK_CONTEXT_WINDOW
        );
        AUTO_COMPACT_FALLBACK_CONTEXT_WINDOW
    });

    let ratio = session_tokens as f64 / context_window as f64;
    if ratio < cfg.ratio {
        return;
    }

    tracing::info!(
        %session_id,
        ratio = format!("{ratio:.3}"),
        soglia = format!("{:.3}", cfg.ratio),
        session_tokens,
        context_window,
        compactable_messages = compactable,
        "auto_compact: sessione sopra soglia -> compattazione automatica"
    );

    match crate::chat_sessions::compact_session_core(state, session_id, project_id).await {
        Ok(outcome) => {
            tracing::info!(
                %session_id,
                soft_deleted = outcome.soft_deleted,
                "auto_compact: sessione compattata"
            );
        }
        Err(e) => {
            // Best-effort: il turno prosegue comunque, solo con piu' contesto.
            tracing::warn!(
                %session_id,
                status = %e.status,
                error = %e,
                "auto_compact: compattazione fallita, proseguo col turno senza compact"
            );
        }
    }
}
