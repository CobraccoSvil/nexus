use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedBlock {
    pub id: String,
    pub purpose: String,
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub dependencies: Vec<String>,
    pub invariants: Vec<String>,
    pub throws: Vec<String>,
    pub side_effects: Vec<String>,
    pub related_blocks: Vec<String>,
    pub steps: Vec<StepInfo>,
    pub warnings: Vec<String>,
    pub todos: Vec<String>,
    pub security_notes: Vec<String>,
    pub performance_notes: Vec<String>,
    pub complexity: Option<String>,
    pub code_hash: String,
    pub last_modified: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepInfo {
    pub id: String,
    pub description: String,
    pub line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum StripMode {
    Full,
    Selective,
    Convert,
    PreserveJsdoc,
}

/// Parse all @ai-* comments from source code and extract structured blocks.
pub fn parse_ai_comments(file_path: &str, source: &str) -> Vec<ParsedBlock> {
    let lines: Vec<&str> = source.lines().collect();
    let mut blocks = Vec::new();
    let mut current_block: Option<HashMap<String, Vec<String>>> = None;
    let mut block_start_line = 0;

    let tag_re = Regex::new(r"@ai-(\w+)[\s:]?\s*(.*)").unwrap();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim().trim_start_matches('*').trim();

        if let Some(cap) = tag_re.captures(trimmed) {
            let tag = cap[1].to_string();
            let value = cap[2].trim().to_string();

            if tag == "block" {
                // Save previous block if any
                if let Some(ref attrs) = current_block {
                    blocks.push(build_block(file_path, attrs, block_start_line, i, &lines));
                }
                let mut attrs = HashMap::new();
                attrs.insert("block".to_string(), vec![value]);
                current_block = Some(attrs);
                block_start_line = i + 1;
            } else if let Some(ref mut attrs) = current_block {
                attrs.entry(tag).or_default().push(value);
            } else {
                // Inline tags outside a block - collect as standalone
                if matches!(
                    tag.as_str(),
                    "step" | "todo" | "warn" | "perf" | "security" | "legacy" | "test"
                ) {
                    // Store for the nearest block or ignore
                }
            }
        }
    }

    // Finalize last block
    if let Some(ref attrs) = current_block {
        blocks.push(build_block(
            file_path,
            attrs,
            block_start_line,
            lines.len(),
            &lines,
        ));
    }

    // Second pass: collect inline tags and attach to blocks
    for block in &mut blocks {
        let start_idx = block.start_line.saturating_sub(1);
        for (rel, item) in lines[start_idx..block.end_line.min(lines.len())].iter().enumerate() {
            let i = start_idx + rel;
            let trimmed = item.trim();
            if let Some(cap) = tag_re.captures(trimmed) {
                let tag = &cap[1];
                let value = cap[2].trim().to_string();
                match tag {
                    "step" => {
                        let parts: Vec<&str> = value.splitn(2, ':').collect();
                        block.steps.push(StepInfo {
                            id: parts.first().unwrap_or(&"").trim().to_string(),
                            description: parts.get(1).unwrap_or(&"").trim().to_string(),
                            line: i + 1,
                        });
                    }
                    "todo" => block.todos.push(value),
                    "warn" => block.warnings.push(value),
                    "perf" => block.performance_notes.push(value),
                    "security" => block.security_notes.push(value),
                    _ => {}
                }
            }
        }
    }

    blocks
}

fn build_block(
    file_path: &str,
    attrs: &HashMap<String, Vec<String>>,
    start_line: usize,
    end_line: usize,
    lines: &[&str],
) -> ParsedBlock {
    let get_first = |key: &str| -> String {
        attrs
            .get(key)
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_default()
    };
    let get_all = |key: &str| -> Vec<String> { attrs.get(key).cloned().unwrap_or_default() };
    let parse_list = |key: &str| -> Vec<String> {
        get_all(key)
            .iter()
            .flat_map(|v| {
                v.trim_matches(|c| c == '[' || c == ']' || c == '\'')
                    .split(',')
                    .map(|s| s.trim().trim_matches('\'').trim_matches('"').to_string())
                    .filter(|s| !s.is_empty())
            })
            .collect()
    };

    // Compute simple hash of the code block
    let code_section: String = lines[start_line.saturating_sub(1)..end_line.min(lines.len())]
        .iter()
        .filter(|l| !l.trim().starts_with('*') && !l.trim().starts_with("//"))
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let code_hash = format!("{:x}", simple_hash(&code_section));

    ParsedBlock {
        id: get_first("block"),
        purpose: get_first("purpose"),
        file_path: file_path.to_string(),
        start_line,
        end_line,
        inputs: parse_list("inputs"),
        outputs: parse_list("outputs"),
        dependencies: parse_list("dependencies"),
        invariants: get_all("invariants"),
        throws: get_all("throws"),
        side_effects: get_all("sideeffects"),
        related_blocks: parse_list("related"),
        steps: vec![], // Filled in second pass
        warnings: vec![],
        todos: vec![],
        security_notes: vec![],
        performance_notes: vec![],
        complexity: attrs.get("complexity").and_then(|v| v.first()).cloned(),
        code_hash,
        last_modified: Utc::now(),
    }
}

fn simple_hash(s: &str) -> u64 {
    let mut hash: u64 = 5381;
    for byte in s.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(byte as u64);
    }
    hash
}

/// Strip @ai-* comments from source code.
pub fn strip_ai_comments(source: &str, mode: StripMode) -> String {
    let ai_tag_re = Regex::new(r"@ai-\w+").unwrap();

    match mode {
        StripMode::Full => strip_full(source, &ai_tag_re),
        StripMode::Selective => strip_selective(source),
        StripMode::Convert => convert_tags(source, &ai_tag_re),
        StripMode::PreserveJsdoc => strip_preserve_jsdoc(source, &ai_tag_re),
    }
}

fn strip_full(source: &str, ai_tag_re: &Regex) -> String {
    let mut result = Vec::new();
    let mut in_jsdoc = false;
    let mut jsdoc_had_only_ai = true;
    let mut jsdoc_buffer = Vec::new();

    for line in source.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("/**") {
            in_jsdoc = true;
            jsdoc_had_only_ai = true;
            jsdoc_buffer.clear();
            jsdoc_buffer.push(line.to_string());
            continue;
        }

        if in_jsdoc {
            if ai_tag_re.is_match(trimmed) {
                // Skip this line
            } else if trimmed == "*/" {
                jsdoc_buffer.push(line.to_string());
                in_jsdoc = false;
                if !jsdoc_had_only_ai {
                    result.append(&mut jsdoc_buffer);
                }
                // If all lines were @ai-*, skip entire block
            } else {
                let content = trimmed.trim_start_matches('*').trim();
                if !content.is_empty() {
                    jsdoc_had_only_ai = false;
                }
                jsdoc_buffer.push(line.to_string());
            }
            continue;
        }

        // Inline comments
        if trimmed.starts_with("//") && ai_tag_re.is_match(trimmed) {
            continue;
        }

        result.push(line.to_string());
    }

    result.join("\n")
}

fn strip_selective(source: &str) -> String {
    // Only strip @ai-todo and @ai-warn, keep the rest
    let remove_re = Regex::new(r"@ai-(todo|warn)").unwrap();
    source
        .lines()
        .filter(|line| !remove_re.is_match(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn convert_tags(source: &str, ai_tag_re: &Regex) -> String {
    // Convert @ai-purpose to a standard description comment
    let purpose_re = Regex::new(r"\*\s*@ai-purpose\s+(.+)").unwrap();
    source
        .lines()
        .map(|line| {
            if let Some(cap) = purpose_re.captures(line) {
                format!(" * {}", &cap[1])
            } else if ai_tag_re.is_match(line) {
                String::new()
            } else {
                line.to_string()
            }
        })
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_preserve_jsdoc(source: &str, ai_tag_re: &Regex) -> String {
    let preserve_re =
        Regex::new(r"@(param|returns?|throws|deprecated|example|see|since|version)").unwrap();
    source
        .lines()
        .filter(|line| {
            if ai_tag_re.is_match(line) && !preserve_re.is_match(line) {
                return false;
            }
            true
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_full_block() {
        let source = r#"
/**
 * @ai-block auth-handler
 * @ai-purpose Handles OAuth2 authentication flow
 * @ai-inputs { token: string, refresh?: string }
 * @ai-outputs { user: User, session: Session }
 * @ai-dependencies ['./user-service', './session-store']
 * @ai-invariants Token must be validated before session creation
 * @ai-throws AuthError if token expired
 * @ai-sideeffects Writes session to Redis
 * @ai-related ['token-refresh', 'logout-handler']
 */
export async function handleAuth(token: string) {
    // @ai-step validate: Verify JWT signature
    // @ai-step create-session: Create new session
    // @ai-warn: Do not modify operation order
    // @ai-perf: Cache result for 5 minutes
    return result;
}
"#;
        let blocks = parse_ai_comments("auth.ts", source);
        assert_eq!(blocks.len(), 1);
        let block = &blocks[0];
        assert_eq!(block.id, "auth-handler");
        assert!(block.purpose.contains("OAuth2"));
        assert!(!block.dependencies.is_empty());
        assert!(!block.steps.is_empty());
        assert!(!block.warnings.is_empty());
    }

    #[test]
    fn test_strip_full() {
        let source = r#"/**
 * @ai-block user-validator
 * @ai-purpose Validates user data
 */
function validate(data) {
    // @ai-step check: Verify email
    if (!isValid(data.email)) throw new Error();
    // @ai-security: Input sanitized
    return true;
}"#;
        let stripped = strip_ai_comments(source, StripMode::Full);
        assert!(!stripped.contains("@ai-"));
        assert!(stripped.contains("function validate"));
        assert!(stripped.contains("isValid"));
    }
}
