// ═══════════════════════════════════════════════════════════════════════════
// meta_docs/generators/decisions.rs — Genera decisions/YYYY-MM-DD-*.md
//
// MVP: usa pattern regex per estrarre "decisioni" da chat_messages
// (es. "decidiamo X perche' Y", "uso X invece di Y", "non usiamo X perche Y").
// Step successivi: LLM batch via purpose `decision_extractor` per rilevazione
// piu' sofisticata.
// ═══════════════════════════════════════════════════════════════════════════

use super::{GeneratedDoc, MetaDocContext, MetaDocGenerator};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use regex::Regex;
use sqlx::Row;

pub struct DecisionExtractor;

#[async_trait]
impl MetaDocGenerator for DecisionExtractor {
    fn name(&self) -> &'static str {
        "decisions"
    }

    fn relevant_for(&self, _files: &[String]) -> bool {
        // Sempre rilevante (gira anche su refresh-all completo)
        true
    }

    async fn generate(&self, ctx: &MetaDocContext<'_>) -> Result<Vec<GeneratedDoc>> {
        // Pattern decisionali (semplici, italiano)
        let patterns: Vec<Regex> = [
            r"(?i)\bdecidiamo\b[^.]*?(?:\bperche|\bperch[eè])\b[^.]*\.",
            r"(?i)\buso\b\s+\w+[^.]*?\binvece\s+di\b\s+\w+[^.]*\.",
            r"(?i)\bnon\s+(?:usiamo|usare)\b[^.]*?(?:\bperche|\bperch[eè])\b[^.]*\.",
            r"(?i)\bscelgo\b[^.]*?(?:\bperche|\bperch[eè])\b[^.]*\.",
            r"(?i)\bsostituisci(?:amo)?\b[^.]*?\bcon\b[^.]*\.",
        ]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();

        if patterns.is_empty() {
            return Ok(Vec::new());
        }

        // Carica messaggi utente recenti
        let rows = sqlx::query(
            r#"
            SELECT content, created_at, session_id
            FROM chat_messages
            WHERE role = 'user'
              AND created_at > NOW() - INTERVAL '24 hours'
              AND length(content) BETWEEN 20 AND 4000
            ORDER BY created_at DESC
            LIMIT 200
            "#,
        )
        .fetch_all(ctx.db)
        .await
        .unwrap_or_default();

        let mut found: Vec<(String, DateTime<Utc>)> = Vec::new();
        for r in &rows {
            let content: String = r.try_get("content").unwrap_or_default();
            let created_at: DateTime<Utc> = r.try_get("created_at").unwrap_or_else(|_| Utc::now());
            for pat in &patterns {
                for m in pat.find_iter(&content) {
                    let s = m.as_str().trim().to_string();
                    if s.len() >= 25 {
                        found.push((s, created_at));
                    }
                }
            }
        }

        // Deduplica + tronca
        found.sort_by_key(|(s, _)| s.to_lowercase());
        found.dedup_by_key(|(s, _)| s.to_lowercase());
        if found.is_empty() {
            return Ok(Vec::new());
        }
        let now = Utc::now();
        let slug_today = format!("decisions-{}", now.format("%Y-%m-%d"));
        let vault_path = super::super::vault::build_vault_path("decision", &slug_today, &now);

        let mut body = String::new();
        body.push_str(&format!("# Decisioni del {}\n\n", now.format("%Y-%m-%d")));
        body.push_str(&format!(
            "_Estratte automaticamente da `chat_messages` (ultime 24h)._\n\n"
        ));
        for (i, (snippet, ts)) in found.iter().take(40).enumerate() {
            body.push_str(&format!(
                "## {idx}. _{date}_\n\n> {text}\n\n",
                idx = i + 1,
                date = ts.format("%Y-%m-%d %H:%M"),
                text = snippet.replace('\n', " ").trim()
            ));
        }
        body.push_str(
            "---\n\n_Nota: il pattern matching e' MVP. La LLM-based extraction verra' \
             abilitata dal `MetaDocsRefreshWorker` (purpose `decision_extractor`)._\n",
        );

        Ok(vec![GeneratedDoc {
            kind: "decision".to_string(),
            title: format!("Decisioni del {}", now.format("%Y-%m-%d")),
            slug: slug_today,
            body_md: body,
            tags: vec!["decision".to_string(), "auto-extracted".to_string()],
            source_files: vec!["chat_messages (DB)".to_string()],
            source_commit: ctx.commit_sha.clone(),
            vault_file_path: vault_path,
            now,
        }])
    }
}
