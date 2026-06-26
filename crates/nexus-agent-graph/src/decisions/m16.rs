//! `m16`: gate PURO di validazione tool-in-list (discovery-first) + parser dei tool
//! scoperti via `nexus_mcp_tool_search`. Porting 1:1 da
//! `brain/agents/nodes/__init__.py` (tool_dispatch_node, blocchi ~3645-3699 e ~4116-4196).
//!
//! Punto unico (regola L) di:
//!   - costruzione dell'insieme dei tool ammessi ([`build_m16_allowed`]);
//!   - decisione di ammissibilita' di una chiamata ([`is_tool_allowed`]);
//!   - parsing del payload di `nexus_mcp_tool_search` ([`parse_discovered_tools`]) e
//!     accumulo dedup per il run ([`merge_discovered_run`]).
//!
//! INCIDENTE STORICO (loop M16 da truncation): il payload di `nexus_mcp_tool_search`
//! viene troncato per il prompt; il parser DEVE ricevere il raw_content INTEGRO
//! (pre-truncation). Se il JSON e' comunque TRONCATO/malformato, il parser ritorna
//! 0/parziale SENZA panic, identico al `try/except` Python che logga e prosegue. Il
//! golden col caso troncato e' cruciale.
//!
//! Distinzione load-bearing nello state (NON gestita qui, ma documentata): nel Python
//! `discovered_tools_next_turn = None` e' un no-op (reducer non tocca), mentre `[]`
//! AZZERA i discovered del turno precedente (overwrite reducer, durata esatta 1 turno).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::py_json::{py_json_dumps, SortKeys};

/// I due meta-tool di discovery (`_M16_META_TOOLS` Python).
pub const M16_META_TOOLS: &[&str] = &["nexus_mcp_tool_search", "nexus_mcp_tool_call"];

/// Cap sulla lunghezza della `description` di un tool scoperto (Python `[:500]`).
const DISCOVERED_DESCRIPTION_MAX: usize = 500;

/// Un tool scoperto, nella forma iniettabile come native al turno successivo.
/// Corrisponde al dict `{"name", "description", "input_schema"}` Python.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveredTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Costruisce l'insieme dei tool ammessi dalla validazione M16. 1:1 con
/// `_M16_ALLOWED` Python: unione di meta-tool di discovery, whitelist DB, always-on del
/// profilo e brain-only ({task_complete, run_notes}).
///
/// I quattro insiemi arrivano gia' risolti dal chiamante (regola G: la whitelist e gli
/// always-on sono DB-driven / fonte unica del profilo; qui restiamo puri).
pub fn build_m16_allowed(
    meta: &[&str],
    whitelist: &[String],
    always_on: &[String],
    brain_tools: &[&str],
) -> HashSet<String> {
    let mut allowed: HashSet<String> = HashSet::new();
    allowed.extend(meta.iter().map(|s| s.to_string()));
    allowed.extend(whitelist.iter().cloned());
    allowed.extend(always_on.iter().cloned());
    allowed.extend(brain_tools.iter().map(|s| s.to_string()));
    allowed
}

/// True se la chiamata al tool `name` e' ammessa: in `allowed` OPPURE in `discovered`
/// (i tool scoperti in questo turno). 1:1 con `name not in _M16_ALLOWED and name not
/// in _disc_now` (negato).
pub fn is_tool_allowed(name: &str, allowed: &HashSet<String>, discovered: &HashSet<String>) -> bool {
    allowed.contains(name) || discovered.contains(name)
}

/// Parsifica il payload INTEGRO (pre-truncation) di `nexus_mcp_tool_search` ed estrae i
/// tool scoperti. 1:1 con il blocco M16 del tool_dispatch_node:
///   - JSON malformato/troncato -> `Vec` VUOTO (Python: `except -> continue`, nessun panic);
///   - itera `results`, per ogni elemento DICT prende `tool_name` o `name` (skip se
///     assente/vuoto);
///   - `input_schema` o default `{"type":"object","properties":{}}`; se la serializzazione
///     dello schema supera `schema_max_bytes`, lo schema e' azzerato al default;
///   - `description` troncata a 500 char;
///   - DEDUP per nome: la PRIMA occorrenza vince (Python: `if not any(name == ...)`).
///
/// NB sul cap schema: Python usa `len(json.dumps(_schema))` (default `ensure_ascii=True`,
/// senza `sort_keys`). Qui usiamo [`py_json_dumps`] con [`SortKeys::No`]
/// (`ensure_ascii=False`): per gli schemi ASCII (caso reale: tipi e proprieta' in inglese)
/// le due lunghezze coincidono. Documentato per i golden, che usano schemi ASCII.
pub fn parse_discovered_tools(raw_json: &str, schema_max_bytes: usize) -> Vec<DiscoveredTool> {
    let mut out: Vec<DiscoveredTool> = Vec::new();
    // JSON troncato/malformato -> nessun tool (parita' col try/except Python).
    let Ok(payload) = serde_json::from_str::<Value>(raw_json) else {
        return out;
    };
    let Some(results) = payload.get("results").and_then(Value::as_array) else {
        return out;
    };
    for res in results {
        let Some(obj) = res.as_object() else {
            continue;
        };
        // name = tool_name or name (truthy, non vuoto).
        let name = obj
            .get("tool_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| obj.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()));
        let Some(name) = name else {
            continue;
        };
        // input_schema o default; cap dimensione -> default.
        let default_schema = json!({ "type": "object", "properties": {} });
        let mut schema = match obj.get("input_schema") {
            Some(s) if !s.is_null() => s.clone(),
            _ => default_schema.clone(),
        };
        if py_json_dumps(&schema, SortKeys::No).len() > schema_max_bytes {
            schema = default_schema;
        }
        // description troncata a 500 char (codepoint, come Python `[:500]`).
        let description: String = obj
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(DISCOVERED_DESCRIPTION_MAX)
            .collect();
        // Dedup per nome: la prima occorrenza vince.
        if !out.iter().any(|d| d.name == name) {
            out.push(DiscoveredTool {
                name: name.to_string(),
                description,
                input_schema: schema,
            });
        }
    }
    out
}

/// Accumula i tool scoperti nel run (P3 prefix stabile): merge dedup per nome, l'ULTIMO
/// schema vince. 1:1 con il blocco `discovered_tools_run` Python (dict per nome aggiornato
/// con i nuovi). L'ordine d'uscita preserva: prima i precedenti nell'ordine originale
/// (eventualmente aggiornati di valore), poi i nuovi nomi nell'ordine d'arrivo — coerente
/// con `dict(prev); for t in new: d[t.name]=t; list(d.values())` (insertion-order di dict).
pub fn merge_discovered_run(
    previous: &[DiscoveredTool],
    discovered_next: &[DiscoveredTool],
) -> Vec<DiscoveredTool> {
    // Replica l'insertion-order di un dict Python: chiave esistente -> valore aggiornato
    // mantenendo la posizione; chiave nuova -> appesa in coda.
    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::HashMap<String, DiscoveredTool> = std::collections::HashMap::new();
    for t in previous {
        if !map.contains_key(&t.name) {
            order.push(t.name.clone());
        }
        map.insert(t.name.clone(), t.clone());
    }
    for t in discovered_next {
        if !map.contains_key(&t.name) {
            order.push(t.name.clone());
        }
        map.insert(t.name.clone(), t.clone());
    }
    order
        .into_iter()
        .filter_map(|k| map.remove(&k))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn allowed_unione() {
        let allowed = build_m16_allowed(
            M16_META_TOOLS,
            &["read_file".to_string(), "list_files".to_string()],
            &["write_file".to_string()],
            &["task_complete", "nexus_run_notes"],
        );
        assert!(allowed.contains("nexus_mcp_tool_search"));
        assert!(allowed.contains("read_file"));
        assert!(allowed.contains("write_file"));
        assert!(allowed.contains("task_complete"));
        assert!(!allowed.contains("tool_sconosciuto"));
    }

    #[test]
    fn tool_allowed_o_discovered() {
        let allowed = set(&["read_file"]);
        let disc = set(&["nexus_foo"]);
        assert!(is_tool_allowed("read_file", &allowed, &disc));
        assert!(is_tool_allowed("nexus_foo", &allowed, &disc));
        assert!(!is_tool_allowed("nexus_bar", &allowed, &disc));
    }

    #[test]
    fn parse_normale() {
        let raw = r#"{"results":[
            {"tool_name":"a","description":"da","input_schema":{"type":"object"}},
            {"name":"b"}
        ]}"#;
        let got = parse_discovered_tools(raw, 8192);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "a");
        assert_eq!(got[1].name, "b");
        // default schema per b.
        assert_eq!(got[1].input_schema, json!({"type":"object","properties":{}}));
    }

    #[test]
    fn parse_troncato_nessun_panic() {
        // JSON troncato a meta' -> Vec vuoto, senza panic.
        let raw = r#"{"results":[{"tool_name":"a","input_sch"#;
        assert_eq!(parse_discovered_tools(raw, 8192), Vec::new());
    }

    #[test]
    fn parse_dedup_prima_vince() {
        let raw = r#"{"results":[
            {"tool_name":"dup","description":"primo"},
            {"tool_name":"dup","description":"secondo"}
        ]}"#;
        let got = parse_discovered_tools(raw, 8192);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].description, "primo");
    }

    #[test]
    fn parse_schema_oversize_azzerato() {
        // schema enorme -> sostituito col default.
        let big_props: String = (0..2000).map(|i| format!("\"p{i}\":{{}}")).collect::<Vec<_>>().join(",");
        let raw = format!(r#"{{"results":[{{"tool_name":"big","input_schema":{{"properties":{{{big_props}}}}}}}]}}"#);
        let got = parse_discovered_tools(&raw, 100);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].input_schema, json!({"type":"object","properties":{}}));
    }

    #[test]
    fn merge_ultimo_vince() {
        let prev = vec![
            DiscoveredTool { name: "a".into(), description: "vecchio a".into(), input_schema: json!({}) },
            DiscoveredTool { name: "b".into(), description: "b".into(), input_schema: json!({}) },
        ];
        let next = vec![
            DiscoveredTool { name: "a".into(), description: "nuovo a".into(), input_schema: json!({}) },
            DiscoveredTool { name: "c".into(), description: "c".into(), input_schema: json!({}) },
        ];
        let merged = merge_discovered_run(&prev, &next);
        // a aggiornato (in posizione 0), b resta, c appeso.
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].name, "a");
        assert_eq!(merged[0].description, "nuovo a");
        assert_eq!(merged[1].name, "b");
        assert_eq!(merged[2].name, "c");
    }
}

/// Golden di parita' 1:1 vs Python per il gate M16. Carica `/tmp/golden_m16.json`
/// (vedi `gen_golden_m16.py`). Il caso `parse_discovered_tools/troncato` e' cruciale:
/// JSON troncato -> lista vuota, nessun panic, 1:1 col try/except Python.
#[cfg(test)]
mod golden {
    use super::*;
    use serde::Deserialize;
    use serde_json::Value;

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        group: String,
        case_id: String,
        input: Value,
        output: Value,
    }

    /// Deserializza una lista di tool dal JSON (sia per input previous/next sia per
    /// confrontare l'output con la serializzazione di [`DiscoveredTool`]).
    fn tools_from(v: &Value) -> Vec<DiscoveredTool> {
        serde_json::from_value(v.clone()).unwrap_or_default()
    }

    fn str_list(v: &Value) -> Vec<String> {
        v.as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default()
    }

    #[test]
    #[ignore = "richiede /tmp/golden_m16.json generato da gen_golden_m16.py"]
    fn golden_m16() {
        let Some(raw) = crate::golden_util::load_golden("golden_m16.json", "gen_golden_m16.py")
        else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(cases.len() >= 15, "attesi >= 15 casi, trovati {}", cases.len());
        let mut saw_truncated = false;
        for c in &cases {
            let inp = &c.input;
            let got: Value = match c.group.as_str() {
                "build_m16_allowed" => {
                    let meta = str_list(inp.get("meta").unwrap_or(&Value::Null));
                    let meta_ref: Vec<&str> = meta.iter().map(String::as_str).collect();
                    let wl = str_list(inp.get("whitelist").unwrap_or(&Value::Null));
                    let ao = str_list(inp.get("always_on").unwrap_or(&Value::Null));
                    let bt = str_list(inp.get("brain_tools").unwrap_or(&Value::Null));
                    let bt_ref: Vec<&str> = bt.iter().map(String::as_str).collect();
                    let allowed = build_m16_allowed(&meta_ref, &wl, &ao, &bt_ref);
                    // Output Python = sorted(set): ordiniamo per confronto deterministico.
                    let mut out: Vec<String> = allowed.into_iter().collect();
                    out.sort();
                    Value::from(out)
                }
                "is_tool_allowed" => {
                    let name = inp.get("name").and_then(Value::as_str).unwrap_or("");
                    let allowed: HashSet<String> =
                        str_list(inp.get("allowed").unwrap_or(&Value::Null)).into_iter().collect();
                    let disc: HashSet<String> =
                        str_list(inp.get("discovered").unwrap_or(&Value::Null)).into_iter().collect();
                    Value::Bool(is_tool_allowed(name, &allowed, &disc))
                }
                "parse_discovered_tools" => {
                    if c.case_id == "troncato" {
                        saw_truncated = true;
                    }
                    let raw_json = inp.get("raw_json").and_then(Value::as_str).unwrap_or("");
                    let mx = inp.get("schema_max_bytes").and_then(Value::as_u64).unwrap_or(8192)
                        as usize;
                    serde_json::to_value(parse_discovered_tools(raw_json, mx)).unwrap()
                }
                "merge_discovered_run" => {
                    let prev = tools_from(inp.get("previous").unwrap_or(&Value::Null));
                    let next = tools_from(inp.get("discovered_next").unwrap_or(&Value::Null));
                    serde_json::to_value(merge_discovered_run(&prev, &next)).unwrap()
                }
                other => panic!("gruppo golden sconosciuto: {other} (caso {})", c.case_id),
            };
            assert_eq!(
                got, c.output,
                "PARITA' FALLITA {} / {}:\n  rust   = {}\n  python = {}",
                c.group, c.case_id, got, c.output
            );
        }
        assert!(saw_truncated, "il caso CRUCIALE parse troncato deve essere presente");
        println!("golden m16: {} casi verificati (incluso troncato), tutti verdi", cases.len());
    }
}
