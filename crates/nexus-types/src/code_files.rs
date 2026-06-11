//! Classificazione file di codice (punto unico, regola L / ADR 0026).
//!
//! Estratto da mcp-core::projects (split 7.4) per i consumer fuori dal
//! monolite (nexus-wiki::code_graph); mcp-core::projects re-esporta.

/// Estensioni considerate "codice sorgente" per indicizzazione semantica,
/// code graph e quality scan. Include i linguaggi di programmazione E il
/// markup `html`/`htm` (le pagine sono contenuto indicizzabile e ricercabile
/// semanticamente nella KB del progetto).
pub const CODE_EXTENSIONS: &[&str] = &[
    "tsx", "jsx", "ts", "js", "rs", "py", "cs", "go", "vue", "html", "htm",
];
