// ═══════════════════════════════════════════════════════════════════════════
// wiki/ — Knowledge Graph unificato (ADR 0017 v2).
//
// Modulo unico che sostituisce `meta_docs/`, `knowledge/` e `docs_core/`
// (questi ultimi restano in piedi come dead code fino a F8: compilano ma
// puntano a tabelle DB che sono state droppate dalla migrazione 0295).
//
// Le tabelle di riferimento sono:
//   - wiki_docs              (scope ∈ {meta, project})
//   - wiki_links             (FK su wiki_docs)
//   - wiki_concept_triples   (FK su wiki_docs)
//   - wiki_doc_revisions     (FK su wiki_docs, polimorfico via doc_id)
//
// L'ACL e' applicata in un punto solo (`acl::WikiAcl`) e i path REST vivono
// sotto `/api/wiki/*` con `scope` come query-param. Vedi ADR 0017 v2.
// ═══════════════════════════════════════════════════════════════════════════

pub mod acl;
pub mod chat_note_worker;
pub mod code_graph;
pub mod internal;
pub mod links_worker;
pub mod run_summary_worker;
pub mod model;
pub mod redirects;
pub mod reingest;
pub mod revisions;
pub mod routes;
pub mod search;
pub mod storage;
pub mod title_gen;
pub mod triple_extractor;
pub mod vault;
pub mod watcher;
