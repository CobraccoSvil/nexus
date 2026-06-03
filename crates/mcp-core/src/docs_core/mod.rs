// ═══════════════════════════════════════════════════════════════════════════
// docs_core — Layer condiviso per la documentazione wiki di Nexus.
//
// Unifica la logica comune tra il meta-vault admin (`meta_docs`) e la
// Knowledge Base per-progetto (`knowledge`), evitando la duplicazione storica
// di serializzazione vault, parsing frontmatter ed estrazione wikilink.
//
// Lo scope (meta vs progetto) e i generatori restano specifici dei rispettivi
// moduli; qui vivono solo gli helper agnostici allo scope.
//
// Fase 1 (estrazione vault): vedi piano wiki. I sotto-moduli storage/links/
// search/tree/revisions/protect/watcher vengono aggiunti nelle fasi successive.
// ═══════════════════════════════════════════════════════════════════════════

pub mod vault;
