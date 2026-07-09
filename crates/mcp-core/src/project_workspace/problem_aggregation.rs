//! Aggregazione problemi del pannello Problemi (regola L).
//!
//! Due fasi:
//! 1. Dedup esatto cross-fonte (stesso file+riga+messaggio).
//! 2. Raggruppamento semantico per errori ripetitivi della stessa classe
//!    (es. violazioni `port/require_allocation` su porte diverse, crash ripetuti
//!    sullo stesso servizio, stesso titolo quality su righe diverse).
//!
//! Il punto unico di identita' di gruppo e' [`problem_group_key`]: ogni fonte
//! contribuisce con una chiave stabile che ignora i token variabili (porte,
//! numeri di riga, timestamp) quando la classe di errore e' la stessa.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};

fn normalize_path_for_dedup(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .to_ascii_lowercase()
}

fn normalize_message_for_dedup(message: &str) -> String {
    message
        .split_whitespace()
        .take(24)
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
        .chars()
        .take(160)
        .collect()
}

fn severity_rank(severity: &str) -> i32 {
    match severity.to_ascii_lowercase().as_str() {
        "error" | "critical" | "high" => 0,
        "warning" | "warn" | "medium" => 1,
        _ => 2,
    }
}

fn source_priority_for_dedup(source: &str) -> i32 {
    if source.starts_with("quality:") {
        0
    } else if source.starts_with("policy:") {
        1
    } else if source.starts_with("service_observer:") {
        2
    } else if source == "security" {
        3
    } else if source.starts_with("runtime:") {
        4
    } else {
        5
    }
}

static POLICY_RULE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)violazione risorse \[([^\]]+)\]").unwrap()
});

static SERVICE_UNIT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)servizio ([^:\n]+):").unwrap()
});

static UUID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
    )
    .unwrap()
});

static LOC_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)[\w./\\-]+\.(?:ts|tsx|js|jsx|py|rs|go|java|yml|yaml|json|toml|conf|ini|sh|cs|php|mjs|cjs|md)(?::\d+){1,2}").unwrap()
});

static NUM_TOKEN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b\d{4,5}\b").unwrap()
});

/// Normalizza un messaggio per il fallback generico: rimuove token variabili
/// (porte, UUID, path:riga) mantenendo la struttura semantica dell'errore.
fn normalize_semantic_message(message: &str) -> String {
    let mut out = message.to_ascii_lowercase();
    out = UUID_RE.replace_all(&out, " ").to_string();
    out = LOC_RE.replace_all(&out, "<loc>").to_string();
    out = NUM_TOKEN_RE.replace_all(&out, "<n>").to_string();
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(120)
        .collect()
}

/// Estrae la regola policy da messaggi tipo
/// `Violazione risorse [port/require_allocation]: ...`.
fn extract_policy_rule(message: &str) -> Option<String> {
    POLICY_RULE_RE
        .captures(message)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_ascii_lowercase())
}

/// Estrae l'unita' servizio da messaggi tipo `Servizio foo.service: crash`.
fn extract_service_unit(message: &str) -> Option<String> {
    SERVICE_UNIT_RE
        .captures(message)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_ascii_lowercase())
}

/// Punto unico (regola L) dell'identita' di gruppo per errori ripetitivi.
/// Ignora token variabili quando la classe di errore e' la stessa.
pub fn problem_group_key(item: &Value) -> String {
    let source = item
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("");
    let message = item
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("");

    if source.starts_with("policy:") {
        if let Some(rule) = extract_policy_rule(message) {
            return format!("policy:{rule}");
        }
        return format!("{source}:{}", normalize_semantic_message(message));
    }

    if source.starts_with("service_observer:") {
        let kind = source.strip_prefix("service_observer:").unwrap_or(source);
        if let Some(unit) = extract_service_unit(message) {
            return format!("service_observer:{kind}:{unit}");
        }
        return format!("service_observer:{kind}:{}", normalize_semantic_message(message));
    }

    if source.starts_with("quality:") {
        let file = item
            .get("filePath")
            .and_then(Value::as_str)
            .map(normalize_path_for_dedup)
            .unwrap_or_default();
        let title = normalize_semantic_message(message);
        return format!("{source}:{file}:{title}");
    }

    format!("{source}:{}", normalize_semantic_message(message))
}

fn problem_exact_dedup_key(item: &Value) -> (String, i32, String) {
    let file = item
        .get("filePath")
        .and_then(Value::as_str)
        .map(normalize_path_for_dedup)
        .unwrap_or_default();
    let line = item
        .get("line")
        .and_then(Value::as_i64)
        .map(|l| (l / 10) as i32)
        .unwrap_or(-1);
    let message = item
        .get("message")
        .and_then(Value::as_str)
        .map(normalize_message_for_dedup)
        .unwrap_or_default();
    (file, line, message)
}

fn prefer_problem_candidate(left: &Value, right: &Value) -> bool {
    let left_sev = left
        .get("severity")
        .and_then(Value::as_str)
        .map(severity_rank)
        .unwrap_or(2);
    let right_sev = right
        .get("severity")
        .and_then(Value::as_str)
        .map(severity_rank)
        .unwrap_or(2);
    if left_sev != right_sev {
        return left_sev < right_sev;
    }
    let left_src = left
        .get("source")
        .and_then(Value::as_str)
        .map(source_priority_for_dedup)
        .unwrap_or(5);
    let right_src = right
        .get("source")
        .and_then(Value::as_str)
        .map(source_priority_for_dedup)
        .unwrap_or(5);
    if left_src != right_src {
        return left_src < right_src;
    }
    let left_at = left.get("createdAt").and_then(Value::as_str).unwrap_or("");
    let right_at = right.get("createdAt").and_then(Value::as_str).unwrap_or("");
    left_at > right_at
}

/// Dedup esatto cross-fonte: stesso file+riga+messaggio (normalizzati).
fn deduplicate_exact(items: &mut Vec<Value>) {
    use std::collections::HashMap;

    let mut best_idx: HashMap<(String, i32, String), usize> = HashMap::new();
    let mut deduped: Vec<Value> = Vec::new();

    for item in items.drain(..) {
        let key = problem_exact_dedup_key(&item);
        if key.2.is_empty() {
            deduped.push(item);
            continue;
        }
        if let Some(&idx) = best_idx.get(&key) {
            if prefer_problem_candidate(&item, &deduped[idx]) {
                deduped[idx] = item;
            }
        } else {
            let idx = deduped.len();
            best_idx.insert(key, idx);
            deduped.push(item);
        }
    }

    *items = deduped;
}

fn problem_instance_summary(item: &Value) -> Value {
    json!({
        "id": item.get("id").cloned().unwrap_or(Value::Null),
        "filePath": item.get("filePath").cloned().unwrap_or(Value::Null),
        "line": item.get("line").cloned().unwrap_or(Value::Null),
        "column": item.get("column").cloned().unwrap_or(Value::Null),
        "message": item.get("message").and_then(Value::as_str).unwrap_or(""),
        "createdAt": item.get("createdAt").cloned().unwrap_or(Value::Null),
    })
}

fn format_location_brief(item: &Value) -> Option<String> {
    let path = item.get("filePath").and_then(Value::as_str)?;
    let line = item.get("line").and_then(Value::as_i64);
    Some(match line {
        Some(l) => format!("{path}:{l}"),
        None => path.to_string(),
    })
}

fn build_grouped_message(representative: &Value, members: &[Value]) -> String {
    let base = representative
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("");
    if members.len() <= 1 {
        return base.to_string();
    }

    let mut locs: Vec<String> = members
        .iter()
        .filter_map(format_location_brief)
        .collect();
    locs.sort();
    locs.dedup();

    let loc_preview: String = if locs.is_empty() {
        format!("{} occorrenze simili", members.len())
    } else {
        let shown: Vec<&str> = locs.iter().take(6).map(String::as_str).collect();
        let mut part = shown.join(", ");
        if locs.len() > shown.len() {
            part.push_str(&format!(" +{} altre", locs.len() - shown.len()));
        }
        format!("{} occorrenze in {}", members.len(), part)
    };

    format!("{base}\n\n— Raggruppate {loc_preview}")
}

fn build_aggregated_item(representative: Value, members: Vec<Value>) -> Value {
    let count = members.len();
    let related_ids: Vec<Value> = members
        .iter()
        .filter_map(|m| m.get("id").cloned())
        .collect();
    let instances: Vec<Value> = members.iter().map(problem_instance_summary).collect();
    let group_key = problem_group_key(&representative);
    let message = build_grouped_message(&representative, &members);

    let mut out = representative;
    if let Some(obj) = out.as_object_mut() {
        obj.insert("message".into(), json!(message));
        obj.insert("groupKey".into(), json!(group_key));
        obj.insert("occurrenceCount".into(), json!(count));
        obj.insert("relatedIds".into(), json!(related_ids));
        obj.insert("instances".into(), json!(instances));
    }
    out
}

/// Raggruppa errori ripetitivi della stessa classe in una sola riga con metadati
/// `occurrenceCount`, `relatedIds`, `instances` per marker editor e prompt chat.
fn aggregate_repetitive(items: &mut Vec<Value>) {
    use std::collections::HashMap;

    let mut groups: HashMap<String, Vec<Value>> = HashMap::new();
    for item in items.drain(..) {
        let key = problem_group_key(&item);
        groups.entry(key).or_default().push(item);
    }

    let mut aggregated: Vec<Value> = Vec::new();
    for members in groups.into_values() {
        if members.len() == 1 {
            let mut single = members.into_iter().next().unwrap_or(json!({}));
            let gk = problem_group_key(&single);
            if let Some(obj) = single.as_object_mut() {
                obj.entry("groupKey".to_string())
                    .or_insert_with(|| json!(gk));
            }
            aggregated.push(single);
            continue;
        }

        let mut representative = members[0].clone();
        for member in members.iter().skip(1) {
            if prefer_problem_candidate(member, &representative) {
                representative = member.clone();
            }
        }
        aggregated.push(build_aggregated_item(representative, members));
    }

    *items = aggregated;
}

/// Pipeline completa: dedup esatto poi aggregazione semantica.
pub fn aggregate_problems(items: &mut Vec<Value>) {
    deduplicate_exact(items);
    aggregate_repetitive(items);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exact_dedup_keeps_higher_severity() {
        let mut items = vec![
            json!({
                "id": "1",
                "severity": "warning",
                "source": "runtime:run_command",
                "message": "Command failed npm test",
                "filePath": "src/app.ts",
                "line": 12,
                "createdAt": "2026-01-01T00:00:00Z",
            }),
            json!({
                "id": "2",
                "severity": "error",
                "source": "quality:lint",
                "message": "Command failed npm test",
                "filePath": "src/app.ts",
                "line": 14,
                "createdAt": "2026-01-01T00:00:01Z",
            }),
        ];
        deduplicate_exact(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].get("id").and_then(Value::as_str), Some("2"));
    }

    #[test]
    fn exact_dedup_keeps_distinct_messages() {
        let mut items = vec![
            json!({
                "id": "1",
                "severity": "error",
                "source": "quality:a",
                "message": "first issue",
                "filePath": "src/a.ts",
                "line": 1,
                "createdAt": "2026-01-01T00:00:00Z",
            }),
            json!({
                "id": "2",
                "severity": "error",
                "source": "quality:b",
                "message": "second issue",
                "filePath": "src/a.ts",
                "line": 1,
                "createdAt": "2026-01-01T00:00:01Z",
            }),
        ];
        deduplicate_exact(&mut items);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn policy_port_violations_group_by_rule_not_port() {
        let mut items = vec![
            json!({
                "id": "a",
                "severity": "error",
                "source": "policy:port",
                "message": "Violazione risorse [port/require_allocation]: vite.config.ts:12 21950 (port/require_allocation) | vite --port 21950",
                "filePath": "vite.config.ts",
                "line": 12,
                "createdAt": "2026-01-01T00:00:00Z",
            }),
            json!({
                "id": "b",
                "severity": "error",
                "source": "policy:port",
                "message": "Violazione risorse [port/require_allocation]: docker-compose.yml:5 21951 (port/require_allocation) | ports: 21951:3000",
                "filePath": "docker-compose.yml",
                "line": 5,
                "createdAt": "2026-01-01T00:00:01Z",
            }),
        ];
        aggregate_repetitive(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].get("occurrenceCount").and_then(Value::as_u64),
            Some(2)
        );
        let related = items[0]
            .get("relatedIds")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(related.len(), 2);
    }

    #[test]
    fn service_observer_groups_same_unit() {
        let mut items = vec![
            json!({
                "id": "1",
                "severity": "error",
                "source": "service_observer:crash",
                "message": "Servizio demo-api.service: crash — restarts=5 (soglia 3.0)",
                "createdAt": "2026-01-01T00:00:00Z",
            }),
            json!({
                "id": "2",
                "severity": "error",
                "source": "service_observer:crash",
                "message": "Servizio demo-api.service: crash — restarts=7 (soglia 3.0)",
                "createdAt": "2026-01-01T00:00:02Z",
            }),
        ];
        aggregate_repetitive(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].get("occurrenceCount").and_then(Value::as_u64),
            Some(2)
        );
    }

    #[test]
    fn quality_same_title_same_file_groups() {
        let mut items = vec![
            json!({
                "id": "1",
                "severity": "warning",
                "source": "quality:lint",
                "message": "Unexpected any",
                "filePath": "src/a.ts",
                "line": 10,
                "createdAt": "2026-01-01T00:00:00Z",
            }),
            json!({
                "id": "2",
                "severity": "warning",
                "source": "quality:lint",
                "message": "Unexpected any",
                "filePath": "src/a.ts",
                "line": 42,
                "createdAt": "2026-01-01T00:00:01Z",
            }),
        ];
        aggregate_repetitive(&mut items);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn aggregate_problems_pipeline() {
        let mut items = vec![
            json!({
                "id": "a",
                "severity": "error",
                "source": "policy:port",
                "message": "Violazione risorse [port/enforce_hardcode]: server.js:1 5000 (port/enforce_hardcode) | listen(5000)",
                "filePath": "server.js",
                "line": 1,
                "createdAt": "2026-01-01T00:00:00Z",
            }),
            json!({
                "id": "b",
                "severity": "error",
                "source": "policy:port",
                "message": "Violazione risorse [port/enforce_hardcode]: app.js:3 5173 (port/enforce_hardcode) | listen(5173)",
                "filePath": "app.js",
                "line": 3,
                "createdAt": "2026-01-01T00:00:01Z",
            }),
        ];
        aggregate_problems(&mut items);
        assert_eq!(items.len(), 1);
        assert!(items[0].get("instances").and_then(Value::as_array).is_some());
    }
}
