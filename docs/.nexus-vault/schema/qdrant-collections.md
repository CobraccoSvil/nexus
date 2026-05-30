---
id: 6c351aa2-e7d0-41c3-a7c8-35db708465e6
kind: schema
title: Collection Qdrant
slug: qdrant-collections
tags:
  - schema
  - qdrant
source_commit: 73c57b761a39d3489cef7f23ff7df54866360875
source_files:
  - crates/mcp-core/src/vector_memory.rs
auto_generated: true
created_at: 2026-05-23T07:20:00Z
updated_at: 2026-05-28T11:39:02Z
nexus_meta_version: 1
---

Collection Qdrant attualmente create. Generato chiamando `GET /collections`.

| Nome | Status |
|---|---|
| `prompt_corrections` | listed |
| `conversation_context` | listed |
| `project_code_index` | listed |
| `knowledge_notes` | listed |
| `nexus_meta_docs` | listed |
| `project_docs` | listed |
| `code_embeddings` | listed |
| `agent_interactions` | listed |
| `project_context` | listed |
| `mcp_tools` | listed |

---

Vedi anche: [[crates-rust]], [[postgres-tables]], [[knowledge-base-funzionamento]], [[meta-vault-architettura]] per l'uso programmatico delle collection.
