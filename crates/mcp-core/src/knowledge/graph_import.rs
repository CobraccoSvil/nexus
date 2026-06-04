//! Comp.2: parser di grafi esterni per l'import nella knowledge base.
//!
//! Supporta tre formati:
//!   - `json` / `node-link`: formato canonico {nodes:[{id,label,type,content?}],
//!     edges:[{source,target,type?}]} (accetta anche `links` e id numerici).
//!   - `mermaid`: flowchart (estrae nodi con label inline e archi `-->`/`---`).
//!   - `dot` / `graphviz`: digraph (estrae archi `->` e nodi con `label="..."`).
//!
//! Il parsing di Mermaid/DOT e' best-effort (struttura principale), senza
//! dipendenze esterne (niente crate regex). La logica di persistenza (embed +
//! upsert + INSERT note/link) vive nel chiamante (tool MCP / endpoint), che ha
//! accesso a db + neural.

use serde_json::Value;
use std::collections::HashMap;

/// Un nodo del grafo importato (prima della mappatura a nota KB).
#[derive(Debug, Clone)]
pub struct ImportedNode {
    pub ext_id: String,
    pub label: String,
    pub node_type: Option<String>,
    pub content: Option<String>,
}

/// Un arco del grafo importato (riferito agli ext_id dei nodi).
#[derive(Debug, Clone)]
pub struct ImportedEdge {
    pub source: String,
    pub target: String,
    pub edge_type: Option<String>,
}

/// Grafo importato e normalizzato.
#[derive(Debug, Default)]
pub struct ImportedGraph {
    pub nodes: Vec<ImportedNode>,
    pub edges: Vec<ImportedEdge>,
}

/// Mappa il tipo di arco esterno a un rel_type della KB
/// (CHECK di project_knowledge_links). Le dipendenze (`depends`/`requires`)
/// diventano `blocked_by` (source dipende da target => target prima di source):
/// alimentano il DAG come dipendenze HARD.
pub fn edge_type_to_rel(edge_type: Option<&str>) -> &'static str {
    let t = match edge_type {
        Some(s) => s.trim().to_lowercase(),
        None => return "relates",
    };
    if t.is_empty() {
        "relates"
    } else if t.contains("depend")
        || t.contains("require")
        || t.contains("need")
        || t.contains("blocked")
    {
        "blocked_by"
    } else if t == "blocks" || t.contains("block") {
        "blocks"
    } else if t.contains("contain")
        || t.contains("parent")
        || t.contains("include")
        || t.contains("refine")
        || t.contains("part_of")
        || t.contains("subtask")
    {
        "refinement"
    } else if t.contains("duplicate") || t.contains("same") || t.contains("alias") {
        "duplicate"
    } else if t.contains("follow") || t.contains("then") || t.contains("next") || t.contains("seq")
    {
        "followup"
    } else if t.contains("correct")
        || t.contains("contradic")
        || t.contains("supersede")
        || t.contains("replace")
    {
        "correction"
    } else {
        "relates"
    }
}

/// Punto d'ingresso: dispatch sul formato.
pub fn parse_graph(format: &str, content: &str) -> Result<ImportedGraph, String> {
    match format.trim().to_lowercase().as_str() {
        "json" | "node-link" | "nodelink" | "node_link" => parse_json_node_link(content),
        "mermaid" | "mmd" => Ok(parse_mermaid(content)),
        "dot" | "graphviz" | "gv" => Ok(parse_dot(content)),
        other => Err(format!(
            "formato non supportato: '{other}' (usa json|mermaid|dot)"
        )),
    }
}

fn value_to_id(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        let s = s.trim();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    } else {
        v.as_i64().map(|i| i.to_string())
    }
}

fn parse_json_node_link(content: &str) -> Result<ImportedGraph, String> {
    let v: Value = serde_json::from_str(content).map_err(|e| format!("JSON non valido: {e}"))?;
    let mut g = ImportedGraph::default();

    let nodes = v
        .get("nodes")
        .and_then(|n| n.as_array())
        .ok_or_else(|| "il grafo JSON deve avere un array 'nodes'".to_string())?;
    for n in nodes {
        let ext_id = match n.get("id").and_then(value_to_id) {
            Some(id) => id,
            None => continue,
        };
        let label = n
            .get("label")
            .or_else(|| n.get("name"))
            .or_else(|| n.get("title"))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| ext_id.clone());
        let node_type = n
            .get("type")
            .or_else(|| n.get("kind"))
            .and_then(|x| x.as_str())
            .map(String::from);
        let cont = n
            .get("content")
            .or_else(|| n.get("body"))
            .or_else(|| n.get("description"))
            .and_then(|x| x.as_str())
            .map(String::from);
        g.nodes.push(ImportedNode {
            ext_id,
            label,
            node_type,
            content: cont,
        });
    }

    let edges = v
        .get("edges")
        .or_else(|| v.get("links"))
        .and_then(|e| e.as_array());
    if let Some(edges) = edges {
        for e in edges {
            let source = match e
                .get("source")
                .or_else(|| e.get("from"))
                .and_then(value_to_id)
            {
                Some(s) => s,
                None => continue,
            };
            let target = match e
                .get("target")
                .or_else(|| e.get("to"))
                .and_then(value_to_id)
            {
                Some(t) => t,
                None => continue,
            };
            let edge_type = e
                .get("type")
                .or_else(|| e.get("rel"))
                .or_else(|| e.get("relation"))
                .or_else(|| e.get("label"))
                .and_then(|x| x.as_str())
                .map(String::from);
            g.edges.push(ImportedEdge {
                source,
                target,
                edge_type,
            });
        }
    }
    Ok(g)
}

/// Estrae (id, label) da un token tipo `A[Label]`, `A(Label)`, `A{Label}`.
/// Se non c'e' delimitatore di label, ritorna (token_pulito, None).
fn parse_node_token(tok: &str) -> (String, Option<String>) {
    let tok = tok.trim();
    for (open, close) in [('[', ']'), ('(', ')'), ('{', '}')] {
        if let Some(oi) = tok.find(open) {
            let id = tok[..oi].trim().trim_matches('"').to_string();
            let rest = &tok[oi + 1..];
            if let Some(ci) = rest.rfind(close) {
                let label = rest[..ci]
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                let label = if label.is_empty() { None } else { Some(label) };
                return (id, label);
            }
        }
    }
    (tok.trim_matches('"').to_string(), None)
}

fn upsert_node(map: &mut HashMap<String, ImportedNode>, id: &str, label: Option<String>) {
    if id.is_empty() {
        return;
    }
    let entry = map.entry(id.to_string()).or_insert_with(|| ImportedNode {
        ext_id: id.to_string(),
        label: id.to_string(),
        node_type: None,
        content: None,
    });
    if let Some(l) = label {
        if !l.is_empty() {
            entry.label = l;
        }
    }
}

fn parse_mermaid(content: &str) -> ImportedGraph {
    let mut nodes: HashMap<String, ImportedNode> = HashMap::new();
    let mut edges: Vec<ImportedEdge> = Vec::new();
    // Connettori in ordine di lunghezza decrescente per match corretto.
    let connectors = ["-.->", "==>", "-->", "---", "--x", "--o", "->"];

    for raw in content.lines() {
        let line = raw.trim().trim_end_matches(';').trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        let lower = line.to_lowercase();
        if lower.starts_with("graph ")
            || lower.starts_with("flowchart")
            || lower.starts_with("subgraph")
            || lower == "end"
            || lower.starts_with("classdef")
            || lower.starts_with("class ")
            || lower.starts_with("style ")
            || lower.starts_with("linkstyle")
            || lower.starts_with("direction ")
        {
            continue;
        }

        let mut connector_hit: Option<(usize, &str)> = None;
        for c in connectors {
            if let Some(pos) = line.find(c) {
                connector_hit = Some((pos, c));
                break;
            }
        }

        if let Some((pos, c)) = connector_hit {
            let left = &line[..pos];
            let mut right = &line[pos + c.len()..];
            let mut edge_label: Option<String> = None;
            let rt = right.trim_start();
            if let Some(stripped) = rt.strip_prefix('|') {
                if let Some(end) = stripped.find('|') {
                    edge_label = Some(stripped[..end].trim().to_string());
                    right = &stripped[end + 1..];
                }
            }
            let (sid, slabel) = parse_node_token(left);
            let (tid, tlabel) = parse_node_token(right);
            if sid.is_empty() || tid.is_empty() {
                continue;
            }
            upsert_node(&mut nodes, &sid, slabel);
            upsert_node(&mut nodes, &tid, tlabel);
            edges.push(ImportedEdge {
                source: sid,
                target: tid,
                edge_type: edge_label,
            });
        } else {
            // Riga con un solo nodo dichiarato con label.
            let (id, label) = parse_node_token(line);
            if !id.is_empty() && label.is_some() {
                upsert_node(&mut nodes, &id, label);
            }
        }
    }

    ImportedGraph {
        nodes: nodes.into_values().collect(),
        edges,
    }
}

fn extract_dot_label(attrs: &str) -> Option<String> {
    let key = "label=";
    let pos = attrs.find(key)?;
    let rest = attrs[pos + key.len()..].trim_start();
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"')?;
        Some(stripped[..end].to_string())
    } else {
        let end = rest.find([',', ']', ' ']).unwrap_or(rest.len());
        let v = rest[..end].trim();
        if v.is_empty() {
            None
        } else {
            Some(v.to_string())
        }
    }
}

fn parse_dot(content: &str) -> ImportedGraph {
    let mut nodes: HashMap<String, ImportedNode> = HashMap::new();
    let mut edges: Vec<ImportedEdge> = Vec::new();

    for raw in content.lines() {
        let line = raw.trim().trim_end_matches(';').trim();
        if line.is_empty() || line.starts_with("//") || line.starts_with('#') {
            continue;
        }
        let lower = line.to_lowercase();
        if lower.starts_with("digraph")
            || lower.starts_with("graph")
            || lower.starts_with("rankdir")
            || line == "{"
            || line == "}"
            || lower.starts_with("node ")
            || lower.starts_with("node[")
            || lower.starts_with("edge ")
            || lower.starts_with("edge[")
        {
            continue;
        }

        if let Some(pos) = line.find("->") {
            let left = line[..pos].trim().trim_matches('"').to_string();
            let mut right = line[pos + 2..].trim();
            let mut edge_type: Option<String> = None;
            if let Some(bi) = right.find('[') {
                edge_type = extract_dot_label(&right[bi + 1..]);
                right = right[..bi].trim();
            }
            let right = right.trim_matches('"').to_string();
            if left.is_empty() || right.is_empty() {
                continue;
            }
            upsert_node(&mut nodes, &left, None);
            upsert_node(&mut nodes, &right, None);
            edges.push(ImportedEdge {
                source: left,
                target: right,
                edge_type,
            });
        } else if let Some(bi) = line.find('[') {
            let id = line[..bi].trim().trim_matches('"').to_string();
            if id.is_empty() {
                continue;
            }
            let label = extract_dot_label(&line[bi + 1..]);
            upsert_node(&mut nodes, &id, label);
        }
    }

    ImportedGraph {
        nodes: nodes.into_values().collect(),
        edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_node_link_basic() {
        let g = parse_graph(
            "json",
            r#"{"nodes":[{"id":"a","label":"A"},{"id":"b","label":"B"}],
                "edges":[{"source":"a","target":"b","type":"depends_on"}]}"#,
        )
        .unwrap();
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(g.edges.len(), 1);
        assert_eq!(
            edge_type_to_rel(g.edges[0].edge_type.as_deref()),
            "blocked_by"
        );
    }

    #[test]
    fn mermaid_basic() {
        let g = parse_mermaid("flowchart TD\n  A[Auth] --> B[DB]\n  B -->|usa| C[Cache]");
        assert_eq!(g.nodes.len(), 3);
        assert_eq!(g.edges.len(), 2);
        let a = g.nodes.iter().find(|n| n.ext_id == "A").unwrap();
        assert_eq!(a.label, "Auth");
    }

    #[test]
    fn dot_basic() {
        let g = parse_dot("digraph G {\n  A [label=\"Alpha\"];\n  A -> B;\n}");
        assert_eq!(g.edges.len(), 1);
        let a = g.nodes.iter().find(|n| n.ext_id == "A").unwrap();
        assert_eq!(a.label, "Alpha");
    }

    #[test]
    fn rel_mapping() {
        assert_eq!(edge_type_to_rel(Some("requires")), "blocked_by");
        assert_eq!(edge_type_to_rel(Some("contains")), "refinement");
        assert_eq!(edge_type_to_rel(None), "relates");
        assert_eq!(edge_type_to_rel(Some("foobar")), "relates");
    }
}
