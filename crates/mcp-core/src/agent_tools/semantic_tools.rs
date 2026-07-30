//! Tool agente di ricerca semantica: codebase (Qdrant), recall contesto,
//! ricerca TF-IDF in-file.
//!
//! Estratto da mod.rs (refactor god-file).

use serde_json::Value;

use super::AgentToolContext;
use crate::projects::resolve_relative_path;
use crate::vector_memory;

pub(super) async fn tool_search_codebase_semantic(
    ctx: &AgentToolContext,
    query: &str,
    limit: usize,
) -> String {
    if query.is_empty() {
        return "Errore: query vuota".to_string();
    }
    // Guard: se Qdrant o embedder sono down, ritorna subito
    {
        use std::sync::atomic::Ordering;
        let qdrant_ok = ctx.dependency_status.qdrant.load(Ordering::Relaxed);
        let embedder_ok = ctx.dependency_status.embedder.load(Ordering::Relaxed);
        if !qdrant_ok || !embedder_ok {
            return format!(
                "Ricerca semantica non disponibile (qdrant={}, embedder={}). \
                 Usa 'grep' o 'find_files' per cercare nel codice.",
                if qdrant_ok { "ok" } else { "down" },
                if embedder_ok { "ok" } else { "down" },
            );
        }
    }
    // Embed la query
    let embedding = match ctx.neural.embed_text("", query).await {
        Ok(v) => v,
        Err(e) => return format!("Errore embedding: {e}"),
    };
    // Cerca in Qdrant
    let hits =
        match vector_memory::search_code_index(&ctx.db, &embedding, ctx.project_id, limit).await {
            Ok(h) => h,
            Err(e) => return format!("Errore ricerca: {e}"),
        };
    if hits.is_empty() {
        return "Nessun risultato trovato. Il codebase potrebbe non essere ancora indicizzato — prova ad analizzare il progetto prima.".to_string();
    }
    // Formatta risultati
    let results: Vec<String> = hits
        .iter()
        .enumerate()
        .map(|(i, hit)| {
            let file = hit
                .payload
                .get("file_path")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let chunk = hit
                .payload
                .get("chunk_index")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let labels = hit
                .payload
                .get("ui_labels")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let score = (hit.score * 100.0).round() as u64;
            let mut parts = vec![format!("{}. {} (score: {}%)", i + 1, file, score)];
            if !labels.is_empty() {
                parts.push(format!("   Label UI: {labels}"));
            }
            if chunk > 0 {
                parts.push(format!("   Chunk: {chunk}"));
            }
            parts.join("\n")
        })
        .collect();
    format!("Risultati per '{query}':\n\n{}", results.join("\n\n"))
}

pub(super) async fn tool_recall_context(
    ctx: &AgentToolContext,
    query: &str,
    source: &str,
    limit: usize,
) -> String {
    if query.is_empty() {
        return "Errore: query vuota".to_string();
    }
    // Guard: se Qdrant o embedder sono down, ritorna subito
    {
        use std::sync::atomic::Ordering;
        let qdrant_ok = ctx.dependency_status.qdrant.load(Ordering::Relaxed);
        let embedder_ok = ctx.dependency_status.embedder.load(Ordering::Relaxed);
        if !qdrant_ok || !embedder_ok {
            return "Recall context non disponibile: dipendenze vettoriali temporaneamente offline.".to_string();
        }
    }
    let embedding = match ctx.neural.embed_text("", query).await {
        Ok(v) => v,
        Err(e) => return format!("Errore embedding: {e}"),
    };

    let search_conversation = source == "conversation" || source == "all";
    let search_project = source == "project" || source == "all";
    let mut sections: Vec<String> = Vec::new();

    if search_conversation {
        if let Some(sid) = ctx.session_id {
            match vector_memory::search_conversation_context(
                &ctx.db,
                &embedding,
                sid,
                limit as u64,
                0.55,
            )
            .await
            {
                Ok(hits) if !hits.is_empty() => {
                    let mut conv_results: Vec<String> = Vec::new();
                    for (i, hit) in hits.iter().enumerate() {
                        let role = hit
                            .payload
                            .get("role")
                            .and_then(Value::as_str)
                            .unwrap_or("?");
                        let preview = hit
                            .payload
                            .get("text_preview")
                            .and_then(Value::as_str)
                            .or_else(|| hit.payload.get("content").and_then(Value::as_str))
                            .unwrap_or("");
                        let score = (hit.score * 100.0).round() as u64;
                        conv_results.push(format!(
                            "{}. [{}] (pertinenza: {}%)\n{}",
                            i + 1,
                            role,
                            score,
                            if preview.len() > 1500 {
                                &preview[..1500]
                            } else {
                                preview
                            }
                        ));
                    }
                    sections.push(format!(
                        "--- Contesto conversazionale ---\n{}",
                        conv_results.join("\n\n")
                    ));
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("recall_context: errore ricerca conversazione: {e}");
                }
            }
        }
    }

    if search_project {
        match vector_memory::search_project_context_points(
            &ctx.db,
            &embedding,
            ctx.project_id,
            limit as u64,
            0.60,
        )
        .await
        {
            Ok(hits) if !hits.is_empty() => {
                let mut proj_results: Vec<String> = Vec::new();
                for (i, hit) in hits.iter().enumerate() {
                    let title = hit
                        .payload
                        .get("section_title")
                        .and_then(Value::as_str)
                        .unwrap_or("Contesto progetto");
                    let preview = hit
                        .payload
                        .get("text_preview")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let score = (hit.score * 100.0).round() as u64;
                    proj_results.push(format!(
                        "{}. {} (pertinenza: {}%)\n{}",
                        i + 1,
                        title,
                        score,
                        if preview.len() > 1500 {
                            &preview[..1500]
                        } else {
                            preview
                        }
                    ));
                }
                sections.push(format!(
                    "--- Contesto progetto ---\n{}",
                    proj_results.join("\n\n")
                ));
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("recall_context: errore ricerca progetto: {e}");
            }
        }
    }

    if sections.is_empty() {
        return format!(
            "Nessun contesto rilevante trovato per '{}'. La conversazione potrebbe non essere ancora indicizzata o la query potrebbe essere troppo specifica.",
            query
        );
    }

    format!(
        "Contesto recuperato per '{query}':\n\n{}",
        sections.join("\n\n")
    )
}

/// Ricerca semantica TF-IDF in-process all'interno di un singolo file.
/// Divide il file in chunk sovrapposti, scorea ogni chunk vs query e
/// restituisce le sezioni più rilevanti con i numeri di riga.
pub(super) async fn tool_search_file_semantic(
    ctx: &AgentToolContext,
    path_str: &str,
    query: &str,
    top_k: usize,
    chunk_lines: usize,
) -> String {
    if query.is_empty() {
        return "\u{274C} [Errore: parametro 'query' mancante]".to_string();
    }
    if path_str.is_empty() {
        return "\u{274C} [Errore: parametro 'path' mancante]".to_string();
    }

    // Risolvi il percorso (supporta assoluti e relativi alla root progetto)
    let target = if std::path::Path::new(path_str).is_absolute() {
        std::path::PathBuf::from(path_str)
    } else {
        match resolve_relative_path(&ctx.root_path, path_str) {
            Ok(p) => p,
            Err(e) => {
                return format!(
                    "\u{274C} [Errore percorso: {}]",
                    e.1["error"].as_str().unwrap_or("path error")
                )
            }
        }
    };

    let content = match tokio::fs::read_to_string(&target).await {
        Ok(c) => c,
        Err(e) => return format!("\u{274C} [Errore lettura '{}': {}]", path_str, e),
    };

    let all_lines: Vec<&str> = content.lines().collect();
    let total_lines = all_lines.len();

    if total_lines == 0 {
        return format!("Il file '{}' è vuoto.", path_str);
    }

    // Tokenizza la query: lowercase, split su non-alfanumerici, filtra token brevi
    let query_tokens: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() >= 2)
        .map(str::to_string)
        .collect();

    if query_tokens.is_empty() {
        return "\u{274C} [Errore: query non contiene termini di ricerca validi]".to_string();
    }

    // Overlap: 20% del chunk_lines per non perdere contesto ai bordi
    let overlap = (chunk_lines / 5).max(5);
    let step = chunk_lines.saturating_sub(overlap).max(1);

    // Costruisci chunk con scoring TF-IDF semplificato
    struct ScoredChunk {
        start_line: usize, // 1-based
        end_line: usize,   // 1-based
        score: f32,
        text: String,
    }

    let mut chunks: Vec<ScoredChunk> = Vec::new();
    let mut chunk_start = 0usize;

    while chunk_start < total_lines {
        let chunk_end = (chunk_start + chunk_lines).min(total_lines);
        let chunk_text = all_lines[chunk_start..chunk_end].join("\n");
        let chunk_lower = chunk_text.to_lowercase();

        // Score = somma pesata delle occorrenze dei token della query
        // Penalty per chunk troppo corti (pochi token di testo effettivo)
        let word_count = chunk_lower
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|t| !t.is_empty())
            .count()
            .max(1) as f32;

        let mut raw_score = 0.0f32;
        for token in &query_tokens {
            // Conta occorrenze del token nel chunk
            let count = chunk_lower.matches(token.as_str()).count() as f32;
            if count > 0.0 {
                // TF puro, log-normalizzato per ridurre l'influenza di token ripetuti
                raw_score += (1.0 + count.ln())
                    * (total_lines as f32 / (chunks.len() + 1).max(1) as f32)
                        .ln()
                        .max(1.0);
            }
        }

        // Normalizza per densità (token utili per riga)
        let density_bonus = (word_count / (chunk_end - chunk_start) as f32).min(2.0);
        let score = raw_score * density_bonus;

        chunks.push(ScoredChunk {
            start_line: chunk_start + 1,
            end_line: chunk_end,
            score,
            text: chunk_text,
        });

        chunk_start += step;
        if chunk_start >= total_lines {
            break;
        }
    }

    if chunks.is_empty() {
        return format!(
            "File '{}' ({} righe): nessun chunk prodotto.",
            path_str, total_lines
        );
    }

    // Ordina per score decrescente
    chunks.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Deduplica: salta chunk il cui range sovrappone un chunk già selezionato
    let mut selected: Vec<&ScoredChunk> = Vec::new();
    'outer: for chunk in &chunks {
        for sel in &selected {
            let overlap_start = chunk.start_line.max(sel.start_line);
            let overlap_end = chunk.end_line.min(sel.end_line);
            if overlap_start <= overlap_end {
                let overlap_len = overlap_end - overlap_start + 1;
                let min_len =
                    (chunk.end_line - chunk.start_line + 1).min(sel.end_line - sel.start_line + 1);
                if overlap_len * 2 > min_len {
                    continue 'outer; // sovrappone troppo: salta
                }
            }
        }
        selected.push(chunk);
        if selected.len() >= top_k {
            break;
        }
    }

    // Ri-ordina i selezionati per numero di riga (ordine naturale del file)
    selected.sort_by_key(|c| c.start_line);

    let header = format!(
        "File: {} ({} righe totali) — {} sezioni rilevanti per '{}'\n",
        path_str,
        total_lines,
        selected.len(),
        query
    );

    let sections: Vec<String> = selected
        .iter()
        .map(|c| {
            format!(
                "── Righe {}-{} (score: {:.0}) ──\n{}",
                c.start_line, c.end_line, c.score, c.text
            )
        })
        .collect();

    format!("{}\n{}", header, sections.join("\n\n"))
}
