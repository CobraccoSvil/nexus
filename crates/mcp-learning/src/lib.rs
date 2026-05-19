// safety: le `Regex::new("...").unwrap()` in questo modulo sono pattern
// literal hardcoded ammessi da CLAUDE.md §F. Refactor opportuno
// (LazyLock<Regex>) ma non e' una violazione.

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// --- Core Types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedPattern {
    pub id: String,
    pub category: PatternCategory,
    pub name: String,
    pub description: String,
    pub confidence: f32,
    pub occurrences: usize,
    pub related_files: Vec<String>,
    pub code_signature: String,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PatternCategory {
    ErrorHandling,
    Authentication,
    DataValidation,
    CachingStrategy,
    ApiPattern,
    DatabaseAccess,
    TestingPattern,
    ConfigPattern,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBundle {
    pub version: String,
    pub generated_at: DateTime<Utc>,
    pub source_project: String,
    pub patterns: Vec<ExtractedPattern>,
    pub summary: BundleSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleSummary {
    pub total_patterns: usize,
    pub by_category: HashMap<String, usize>,
    pub avg_confidence: f32,
    pub top_files: Vec<String>,
}

// --- Provider Sync ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncTarget {
    pub provider: String,
    pub format: SyncFormat,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SyncFormat {
    ClaudeMemory,
    OpenAICustomGpt,
    GeminiGem,
    Markdown,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    pub provider: String,
    pub status: String,
    pub patterns_synced: usize,
    pub timestamp: DateTime<Utc>,
}

// --- Feedback Loop ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternFeedback {
    pub pattern_id: String,
    pub feedback_type: FeedbackType,
    pub comment: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FeedbackType {
    Confirmed,
    Rejected,
    Modified,
}

// --- Pattern Extraction ---

pub struct PatternExtractor {
    extractors: Vec<(&'static str, Regex, PatternCategory)>,
}

impl PatternExtractor {
    pub fn new() -> Self {
        let extractors = vec![
            (
                "try-catch-pattern",
                Regex::new(r"(?:try\s*\{|\.catch\(|\.unwrap_or|anyhow::Result|Result<)").unwrap(),
                PatternCategory::ErrorHandling,
            ),
            (
                "auth-pattern",
                Regex::new(r"(?i)(?:jwt|oauth|bearer|token|auth|session|login|password|credentials)").unwrap(),
                PatternCategory::Authentication,
            ),
            (
                "validation-pattern",
                Regex::new(r"(?:validate|sanitize|is_valid|check_|assert_|zod\.|yup\.|\.parse\()").unwrap(),
                PatternCategory::DataValidation,
            ),
            (
                "cache-pattern",
                Regex::new(r"(?i)(?:cache|memoize|ttl|expire|invalidate|redis\.get|lru|memo)").unwrap(),
                PatternCategory::CachingStrategy,
            ),
            (
                "api-pattern",
                Regex::new(r"(?:fetch\(|axios\.|httpx\.|reqwest::|\.get\(|\.post\(|endpoint|route|handler)").unwrap(),
                PatternCategory::ApiPattern,
            ),
            (
                "db-pattern",
                Regex::new(r"(?i)(?:select\s|insert\s|update\s|delete\s|query\(|execute\(|sqlx::|prisma\.|\.findMany|\.create\()").unwrap(),
                PatternCategory::DatabaseAccess,
            ),
            (
                "test-pattern",
                Regex::new(r"(?:#\[test\]|describe\(|it\(|test\(|expect\(|assert|mock|stub|fixture)").unwrap(),
                PatternCategory::TestingPattern,
            ),
            (
                "config-pattern",
                Regex::new(r"(?:\.env|dotenv|config\.|settings\.|env::|getenv|process\.env)").unwrap(),
                PatternCategory::ConfigPattern,
            ),
        ];
        Self { extractors }
    }

    pub fn extract_from_source(&self, file_path: &str, source: &str) -> Vec<ExtractedPattern> {
        let mut patterns = Vec::new();

        for (id, regex, category) in &self.extractors {
            let matches: Vec<_> = regex.find_iter(source).collect();
            if matches.is_empty() {
                continue;
            }

            let occurrences = matches.len();
            let confidence = self.compute_confidence(occurrences, source.lines().count());

            // Extract a representative code signature (first match with context)
            let first_match = &matches[0];
            let start = first_match.start().saturating_sub(40);
            let end = (first_match.end() + 40).min(source.len());
            let signature = source[start..end].trim().to_string();

            patterns.push(ExtractedPattern {
                id: format!("{}::{}", file_path, id),
                category: category.clone(),
                name: id.to_string(),
                description: format!(
                    "Found {} occurrences of {} pattern in {}",
                    occurrences, id, file_path
                ),
                confidence,
                occurrences,
                related_files: vec![file_path.to_string()],
                code_signature: signature,
                last_seen: Utc::now(),
            });
        }
        patterns
    }

    fn compute_confidence(&self, occurrences: usize, line_count: usize) -> f32 {
        // More occurrences relative to file size = higher confidence
        let density = occurrences as f32 / (line_count.max(1) as f32);
        let base = 0.5 + (density * 10.0).min(0.4);
        // Cap at 0.95
        base.min(0.95_f32)
    }

    pub fn extract_from_files(&self, files: &[(&str, &str)]) -> Vec<ExtractedPattern> {
        let mut all_patterns: HashMap<String, ExtractedPattern> = HashMap::new();

        for (path, source) in files {
            for pattern in self.extract_from_source(path, source) {
                let key = pattern.name.clone();
                if let Some(existing) = all_patterns.get_mut(&key) {
                    existing.occurrences += pattern.occurrences;
                    if !existing.related_files.contains(&path.to_string()) {
                        existing.related_files.push(path.to_string());
                    }
                    existing.confidence = (existing.confidence + pattern.confidence) / 2.0;
                } else {
                    all_patterns.insert(key, pattern);
                }
            }
        }

        let mut patterns: Vec<_> = all_patterns.into_values().collect();
        patterns.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        patterns
    }
}

impl Default for PatternExtractor {
    fn default() -> Self {
        Self::new()
    }
}

// --- Knowledge Synthesizer ---

pub fn synthesize_bundle(project: &str, patterns: Vec<ExtractedPattern>) -> KnowledgeBundle {
    let total = patterns.len();

    let mut by_category: HashMap<String, usize> = HashMap::new();
    for p in &patterns {
        *by_category.entry(format!("{:?}", p.category)).or_insert(0) += 1;
    }

    let avg_confidence = if total > 0 {
        patterns.iter().map(|p| p.confidence).sum::<f32>() / total as f32
    } else {
        0.0
    };

    // Top files by pattern count
    let mut file_counts: HashMap<&str, usize> = HashMap::new();
    for p in &patterns {
        for f in &p.related_files {
            *file_counts.entry(f.as_str()).or_insert(0) += 1;
        }
    }
    let mut top_files: Vec<_> = file_counts.into_iter().collect();
    top_files.sort_by(|a, b| b.1.cmp(&a.1));
    let top_files: Vec<String> = top_files
        .into_iter()
        .take(10)
        .map(|(f, _)| f.to_string())
        .collect();

    KnowledgeBundle {
        version: "0.2.0".to_string(),
        generated_at: Utc::now(),
        source_project: project.to_string(),
        summary: BundleSummary {
            total_patterns: total,
            by_category,
            avg_confidence,
            top_files,
        },
        patterns,
    }
}

// --- Sync Engine ---

/// Format a knowledge bundle for a sync target.
///
/// `system_prefix`: loaded from DB key `automation.learning_bundle_format`
/// (with `{{project}}` replaced at call site). Pass `""` if unavailable.
pub fn format_for_sync(bundle: &KnowledgeBundle, target: &SyncTarget, system_prefix: &str) -> String {
    match target.format {
        SyncFormat::Json => serde_json::to_string_pretty(bundle).unwrap_or_default(),
        SyncFormat::Markdown => format_as_markdown(bundle),
        SyncFormat::ClaudeMemory => format_for_claude(bundle),
        SyncFormat::OpenAICustomGpt => format_for_openai(bundle, system_prefix),
        SyncFormat::GeminiGem => format_for_openai(bundle, system_prefix), // same format
    }
}

fn format_as_markdown(bundle: &KnowledgeBundle) -> String {
    let mut md = format!(
        "# Knowledge Bundle: {}\n\nGenerated: {}\nPatterns: {}\nAvg Confidence: {:.0}%\n\n",
        bundle.source_project,
        bundle.generated_at.format("%Y-%m-%d %H:%M"),
        bundle.summary.total_patterns,
        bundle.summary.avg_confidence * 100.0,
    );

    for pattern in &bundle.patterns {
        md.push_str(&format!(
            "## {} ({:?})\n- Confidence: {:.0}%\n- Occurrences: {}\n- Files: {}\n- Signature: `{}`\n\n",
            pattern.name,
            pattern.category,
            pattern.confidence * 100.0,
            pattern.occurrences,
            pattern.related_files.join(", "),
            pattern.code_signature.chars().take(80).collect::<String>(),
        ));
    }
    md
}

fn format_for_claude(bundle: &KnowledgeBundle) -> String {
    let mut output = format!(
        "Project: {}\nThis project uses the following patterns:\n\n",
        bundle.source_project
    );
    for p in &bundle.patterns {
        if p.confidence >= 0.6 {
            output.push_str(&format!(
                "- {}: {} (confidence {:.0}%, seen in {})\n",
                p.name,
                p.description,
                p.confidence * 100.0,
                p.related_files.join(", "),
            ));
        }
    }
    output
}

/// Format for OpenAI/Gemini. `system_prefix` should come from DB key
/// `automation.learning_bundle_format` with `{{project}}` already replaced.
/// If empty, no system prefix is prepended.
fn format_for_openai(bundle: &KnowledgeBundle, system_prefix: &str) -> String {
    let mut output = if !system_prefix.is_empty() {
        format!("{}\n", system_prefix.replace("{{project}}", &bundle.source_project))
    } else {
        String::new()
    };
    for p in &bundle.patterns {
        if p.confidence >= 0.6 {
            output.push_str(&format!("- {}: {}\n", p.name, p.description));
        }
    }
    output
}

pub fn apply_feedback(patterns: &mut Vec<ExtractedPattern>, feedback: &[PatternFeedback]) {
    for fb in feedback {
        if let Some(pattern) = patterns.iter_mut().find(|p| p.id == fb.pattern_id) {
            match fb.feedback_type {
                FeedbackType::Confirmed => {
                    pattern.confidence = (pattern.confidence + 0.1).min(0.99);
                }
                FeedbackType::Rejected => {
                    pattern.confidence = (pattern.confidence - 0.3).max(0.0);
                }
                FeedbackType::Modified => {
                    pattern.confidence = (pattern.confidence + 0.05).min(0.99);
                }
            }
        }
    }
    // Remove patterns with very low confidence after feedback
    patterns.retain(|p| p.confidence > 0.1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_patterns() {
        let source = r#"
use anyhow::Result;
use sqlx::PgPool;

pub async fn get_user(pool: &PgPool, id: i32) -> Result<User> {
    let user = sqlx::query_as!(User, "SELECT * FROM users WHERE id = $1", id)
        .fetch_one(pool)
        .await?;
    Ok(user)
}

fn validate_email(email: &str) -> bool {
    email.contains('@') && is_valid(email)
}
"#;
        let extractor = PatternExtractor::new();
        let patterns = extractor.extract_from_source("user.rs", source);

        assert!(patterns
            .iter()
            .any(|p| p.category == PatternCategory::ErrorHandling));
        assert!(patterns
            .iter()
            .any(|p| p.category == PatternCategory::DatabaseAccess));
        assert!(patterns
            .iter()
            .any(|p| p.category == PatternCategory::DataValidation));
    }

    #[test]
    fn test_synthesize_bundle() {
        let patterns = vec![ExtractedPattern {
            id: "test::auth".into(),
            category: PatternCategory::Authentication,
            name: "auth-pattern".into(),
            description: "Auth pattern found".into(),
            confidence: 0.85,
            occurrences: 3,
            related_files: vec!["auth.rs".into()],
            code_signature: "jwt::verify(token)".into(),
            last_seen: Utc::now(),
        }];

        let bundle = synthesize_bundle("my-project", patterns);
        assert_eq!(bundle.summary.total_patterns, 1);
        assert!(bundle.summary.avg_confidence > 0.8);
    }

    #[test]
    fn test_feedback_loop() {
        let mut patterns = vec![
            ExtractedPattern {
                id: "p1".into(),
                category: PatternCategory::CachingStrategy,
                name: "cache".into(),
                description: "".into(),
                confidence: 0.7,
                occurrences: 2,
                related_files: vec![],
                code_signature: "".into(),
                last_seen: Utc::now(),
            },
            ExtractedPattern {
                id: "p2".into(),
                category: PatternCategory::Other("noise".into()),
                name: "noise".into(),
                description: "".into(),
                confidence: 0.3,
                occurrences: 1,
                related_files: vec![],
                code_signature: "".into(),
                last_seen: Utc::now(),
            },
        ];

        let feedback = vec![
            PatternFeedback {
                pattern_id: "p1".into(),
                feedback_type: FeedbackType::Confirmed,
                comment: "good".into(),
                timestamp: Utc::now(),
            },
            PatternFeedback {
                pattern_id: "p2".into(),
                feedback_type: FeedbackType::Rejected,
                comment: "false positive".into(),
                timestamp: Utc::now(),
            },
        ];

        apply_feedback(&mut patterns, &feedback);
        assert!(patterns.iter().find(|p| p.id == "p1").unwrap().confidence > 0.7);
        // p2 should be removed (confidence dropped below 0.1)
        assert!(patterns.iter().find(|p| p.id == "p2").is_none());
    }

    #[test]
    fn test_sync_formats() {
        let bundle = synthesize_bundle("test", vec![]);
        let target = SyncTarget {
            provider: "claude".into(),
            format: SyncFormat::Markdown,
        };
        let md = format_for_sync(&bundle, &target, "");
        assert!(md.contains("Knowledge Bundle"));

        let target_json = SyncTarget {
            provider: "api".into(),
            format: SyncFormat::Json,
        };
        let json = format_for_sync(&bundle, &target_json, "");
        assert!(json.contains("test"));
    }

    #[test]
    fn test_multi_file_extraction() {
        let extractor = PatternExtractor::new();
        let files: Vec<(&str, &str)> = vec![
            ("a.rs", "fn main() { let token = jwt::verify(t); }"),
            (
                "b.rs",
                "fn login() { check_password(pwd); let session = create_session(); }",
            ),
        ];
        let patterns = extractor.extract_from_files(&files);
        let auth = patterns.iter().find(|p| p.name == "auth-pattern");
        assert!(auth.is_some());
        assert!(auth.unwrap().related_files.len() >= 2);
    }
}
