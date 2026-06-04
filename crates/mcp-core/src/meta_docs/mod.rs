// ═══════════════════════════════════════════════════════════════════════════
// meta_docs — Documentazione del meta-progetto Nexus (vault Obsidian-compatible)
//
// Schema: db/migrations/0177_nexus_meta_docs.sql
// Vault path: docs/.nexus-vault/ dentro la repository Nexus
// Riferimento ADR: docs/.nexus-vault/adr/0005-meta-docs-vault.md
// ═══════════════════════════════════════════════════════════════════════════

pub mod apply;
pub mod generators;
pub mod routes;
pub mod vault;
