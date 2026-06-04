//! MCP tools per la Knowledge Base per-progetto.
//!
//! ADR 0017 v2 F8 — Le 9 tool entry restano esposte agli agenti AI via
//! `NexusToolCatalog` (i loro nomi sono parte del contratto pubblico), ma le
//! implementazioni interne — che leggevano da `project_knowledge_notes` e
//! `project_knowledge_links` — sono state retired insieme al modulo
//! `crate::knowledge`. Le tabelle backing sono state droppate dalla mig 0295.
//!
//! Reimplementazione 1:1 su `wiki_docs` + `wiki_links` + `wiki_concept_triples`
//! e' un task separato; finche' non viene fatta, ogni tool ritorna un payload
//! `deprecated` esplicito così l'agente non viene confuso da dati arbitrari.
//! Vedi ADR 0017 v2 sezione "Cleanup moduli vecchi" + il pannello UI Wiki che
//! gia' offre search/get/create/link via REST `/api/wiki/*`.

use super::AgentToolContext;
use serde_json::{json, Value};

fn deprecated_payload(tool_name: &str) -> String {
    json!({
        "deprecated": true,
        "tool": tool_name,
        "reason": "Tool knowledge_* retired in ADR 0017 v2 F8. \
                   Implementazione interna basata su tabelle droppate \
                   (project_knowledge_notes / project_knowledge_links). \
                   Da reimplementare sul nuovo schema wiki_docs / wiki_links \
                   / wiki_concept_triples. Nel frattempo l'utente puo' usare \
                   il pannello Wiki UI o gli endpoint REST /api/wiki/*.",
        "results": [],
    })
    .to_string()
}

pub async fn tool_knowledge_search(_ctx: &AgentToolContext, _input: &Value) -> String {
    deprecated_payload("knowledge_search")
}

pub async fn tool_code_doc(_ctx: &AgentToolContext, _input: &Value) -> String {
    deprecated_payload("code_doc")
}

pub async fn tool_knowledge_get_note(_ctx: &AgentToolContext, _input: &Value) -> String {
    deprecated_payload("knowledge_get_note")
}

pub async fn tool_knowledge_create_note(_ctx: &AgentToolContext, _input: &Value) -> String {
    deprecated_payload("knowledge_create_note")
}

pub async fn tool_knowledge_get_links(_ctx: &AgentToolContext, _input: &Value) -> String {
    deprecated_payload("knowledge_get_links")
}

pub async fn tool_knowledge_get_subgraph(_ctx: &AgentToolContext, _input: &Value) -> String {
    deprecated_payload("knowledge_get_subgraph")
}

pub async fn tool_knowledge_create_link(_ctx: &AgentToolContext, _input: &Value) -> String {
    deprecated_payload("knowledge_create_link")
}

pub async fn tool_knowledge_set_relevance(_ctx: &AgentToolContext, _input: &Value) -> String {
    deprecated_payload("knowledge_set_relevance")
}

pub async fn tool_knowledge_import_graph(_ctx: &AgentToolContext, _input: &Value) -> String {
    deprecated_payload("knowledge_import_graph")
}
