---
id: 71429a62-431c-4e8f-baa2-0686e17d4a3e
kind: other
title: Glossario Nexus
slug: glossario
tags:
  - concept
  - glossario
  - terminologia
source_files:
  - crates/mcp-core/src/meta_docs/generators/concepts.rs
auto_generated: true
created_at: 2026-05-23T11:09:01Z
updated_at: 2026-06-03T15:25:40Z
nexus_meta_version: 1
---

# Glossario Nexus

| Termine | Significato |
|---|---|
| **Agent kind** | Categoria di agente AI (Coder, Tester, Reviewer, Architect, ...). 60+ varianti definite in `crates/nexus-orchestrator/src/agent_types.rs`. |
| **Behavior mode** | Modalita' del routing AI: bilanciata, veloce, approfondita, economica. |
| **Brain** | Servizio Python (FastAPI + LangGraph) che incapsula gli AI provider. Vedi [[brain-python]]. |
| **ChangeDrafter** | Workflow di modifica supervisionata. Vedi [[change-drafter]]. |
| **Intent** | Etichetta semantica per messaggio user (fix, feature, refactor, ...) classificata da LLM. |
| **Knowledge Base (KB)** | Vault per-progetto. Vedi [[knowledge-base-funzionamento]]. |
| **LearningWorker** | Pattern worker async. Vedi [[pattern-learning-worker]]. |
| **MCP tool** | Funzione callable da agent loop. Vedi [[pattern-mcp-tool]]. |
| **Meta-vault** | Doc di Nexus stesso. Vedi [[meta-vault-architettura]]. |
| **Provider** | Vendor AI (OpenAI, Anthropic, Google, Mistral, DeepSeek). |
| **Purpose** | Chiave usata per `nexus_purpose_model` (task interno specifico). |
| **Q-learning router** | Sistema di self-improvement che ottimizza scelta modelli via reward. |
| **Routing matrix** | Tabella DB che mappa intent+mode -> provider+model. Vedi [[multi-provider-routing]]. |
| **Sub-agent** | Agente specializzato Claude Code. Vedi [[sub-agents-claude-code]]. |
| **Vault** | Cartella Obsidian-compatible (`.md` + frontmatter YAML). |

Vedi anche [[nexus-funzionale]], [[nexus-architetturale]].
