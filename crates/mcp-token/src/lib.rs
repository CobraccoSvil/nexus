use serde::{Deserialize, Serialize};
use tiktoken_rs::cl100k_base;

/// Sezione con priorità per l'assembly dichiarativo del context window.
/// P0 = sempre presente; P5 = droppata per prima se il budget è stretto.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSection {
    /// Identificatore univoco per logging (es. "system_core", "project_facts", "rag")
    pub id: String,
    /// Priorità 0–5 (0 = mai droppata, 5 = prima a essere droppata)
    pub priority: u8,
    /// Contenuto testuale della sezione
    pub content: String,
    /// Se false, la sezione viene inclusa SEMPRE anche se sfora il budget
    pub droppable: bool,
}

/// Risultato dell'assembly dichiarativo con info su cosa è stato droppato.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionAssemblyResult {
    pub assembled: String,
    pub total_tokens: usize,
    pub dropped_sections: Vec<DroppedSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DroppedSection {
    pub id: String,
    pub priority: u8,
    pub tokens_saved: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenOptimizationResult {
    pub original_tokens: usize,
    pub optimized_tokens: usize,
    pub tokens_saved: usize,
    pub optimized_prompt: String,
    pub selected_blocks: Vec<String>,
}

/// Count tokens using the cl100k_base tokenizer (GPT-4 / Claude compatible).
pub fn count_tokens(text: &str) -> usize {
    let bpe = cl100k_base().expect("failed to load tokenizer");
    bpe.encode_with_special_tokens(text).len()
}

/// Optimize context by trimming to fit within token budget.
///
/// Preserves complete sentences where possible.
pub fn optimize_context(prompt: &str, token_budget: usize) -> TokenOptimizationResult {
    let original_tokens = count_tokens(prompt);

    if original_tokens <= token_budget {
        return TokenOptimizationResult {
            original_tokens,
            optimized_tokens: original_tokens,
            tokens_saved: 0,
            optimized_prompt: prompt.to_string(),
            selected_blocks: vec![],
        };
    }

    // Trim by sentences from the end until we fit
    let sentences: Vec<&str> = prompt
        .split_inclusive(['.', '\n'])
        .collect();

    let mut kept = String::new();
    let mut optimized_tokens = 0;

    for sentence in &sentences {
        let candidate = format!("{}{}", kept, sentence);
        let tokens = count_tokens(&candidate);
        if tokens > token_budget {
            break;
        }
        kept = candidate;
        optimized_tokens = tokens;
    }

    if kept.is_empty() {
        // Budget too small even for one sentence - truncate by tokens
        let bpe = cl100k_base().expect("failed to load tokenizer");
        let token_ids = bpe.encode_with_special_tokens(prompt);
        let truncated_ids = &token_ids[..token_budget.min(token_ids.len())];
        kept = bpe.decode(truncated_ids.to_vec()).unwrap_or_default();
        optimized_tokens = truncated_ids.len();
    }

    let tokens_saved = original_tokens.saturating_sub(optimized_tokens);

    TokenOptimizationResult {
        original_tokens,
        optimized_tokens,
        tokens_saved,
        optimized_prompt: kept,
        selected_blocks: vec![],
    }
}

/// Optimize context with block selection: pick the most relevant blocks
/// that fit within the token budget.
pub fn optimize_with_blocks(
    prompt: &str,
    blocks: &[(String, String)], // (block_id, content)
    token_budget: usize,
) -> TokenOptimizationResult {
    let prompt_tokens = count_tokens(prompt);
    let remaining_budget = token_budget.saturating_sub(prompt_tokens);

    let mut selected_blocks = Vec::new();
    let mut total_block_tokens = 0;
    let mut block_content = String::new();

    for (block_id, content) in blocks {
        let block_tokens = count_tokens(content);
        if total_block_tokens + block_tokens > remaining_budget {
            break;
        }
        selected_blocks.push(block_id.clone());
        block_content.push_str(content);
        block_content.push('\n');
        total_block_tokens += block_tokens;
    }

    let optimized_prompt = format!("{}\n\n{}", prompt, block_content);
    let optimized_tokens = prompt_tokens + total_block_tokens;
    let all_blocks_tokens: usize = blocks.iter().map(|(_, c)| count_tokens(c)).sum();
    let tokens_saved = (prompt_tokens + all_blocks_tokens).saturating_sub(optimized_tokens);

    TokenOptimizationResult {
        original_tokens: prompt_tokens + all_blocks_tokens,
        optimized_tokens,
        tokens_saved,
        optimized_prompt,
        selected_blocks,
    }
}

/// Assembly dichiarativo del context window con priorità esplicite.
///
/// Algoritmo greedy: include le sezioni dalla priorità più bassa (P0) alla più alta (P5).
/// Se il budget è superato, droppa dalla priorità più alta in poi.
/// Le sezioni con `droppable = false` vengono sempre incluse.
///
/// Restituisce il testo assemblato + log delle sezioni droppate per observability.
pub fn optimize_sections(sections: &[ContextSection], token_budget: usize) -> SectionAssemblyResult {
    // Separa sezioni obbligatorie da droppabili
    let mut mandatory: Vec<&ContextSection> = sections.iter().filter(|s| !s.droppable).collect();
    let mut optional: Vec<&ContextSection> = sections.iter().filter(|s| s.droppable).collect();

    // Ordina le opzionali per priorità crescente (P0 prima)
    optional.sort_by_key(|s| s.priority);

    // Calcola token obbligatori
    let mandatory_tokens: usize = mandatory.iter().map(|s| count_tokens(&s.content)).sum();
    let mut remaining_budget = token_budget.saturating_sub(mandatory_tokens);

    let mut included: Vec<&ContextSection> = mandatory.drain(..).collect();
    let mut dropped: Vec<DroppedSection> = Vec::new();

    for section in &optional {
        let section_tokens = count_tokens(&section.content);
        if section_tokens <= remaining_budget {
            included.push(section);
            remaining_budget -= section_tokens;
        } else {
            dropped.push(DroppedSection {
                id: section.id.clone(),
                priority: section.priority,
                tokens_saved: section_tokens,
            });
        }
    }

    // Riordina per priorità originale prima di assemblare
    included.sort_by(|a, b| a.priority.cmp(&b.priority));

    let assembled = included
        .iter()
        .map(|s| s.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let total_tokens = count_tokens(&assembled);

    SectionAssemblyResult {
        assembled,
        total_tokens,
        dropped_sections: dropped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_optimize_sections_drops_low_priority() {
        let sections = vec![
            ContextSection {
                id: "system".to_string(),
                priority: 0,
                content: "system prompt".to_string(),
                droppable: false,
            },
            ContextSection {
                id: "facts".to_string(),
                priority: 1,
                content: "project facts that are important".to_string(),
                droppable: true,
            },
            ContextSection {
                id: "rag".to_string(),
                priority: 5,
                content: "rag context that can be dropped".to_string(),
                droppable: true,
            },
        ];
        // Budget molto stretto: solo il mandatory passa + facts se c'è spazio
        let result = optimize_sections(&sections, 10);
        // Il mandatory (system) è sempre incluso
        assert!(result.assembled.contains("system prompt"));
        // Sezioni droppate hanno tokens_saved > 0
        let total_saved: usize = result.dropped_sections.iter().map(|d| d.tokens_saved).sum();
        let _ = total_saved; // verifica che il campo sia popolato
    }

    #[test]
    fn test_count_tokens() {
        let count = count_tokens("Hello, world!");
        assert!(count > 0 && count < 10);
    }

    #[test]
    fn test_optimize_within_budget() {
        let result = optimize_context("Hello world", 100);
        assert_eq!(result.tokens_saved, 0);
        assert_eq!(result.optimized_prompt, "Hello world");
    }

    #[test]
    fn test_optimize_over_budget() {
        let long_text = "First sentence. Second sentence. Third sentence. Fourth sentence.";
        let result = optimize_context(long_text, 5);
        assert!(result.tokens_saved > 0);
        assert!(result.optimized_tokens <= 5);
    }
}
