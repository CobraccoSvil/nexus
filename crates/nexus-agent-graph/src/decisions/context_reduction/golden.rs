//! Golden-test di PARITA' 1:1 vs Python per la parte PURA di context_reduction.
//!
//! Lo script `scripts/gen_golden_context_reduction.py` esercita le funzioni
//! (REALI dal brain dove IO-free, altrimenti replica byte-fedele dell'algoritmo)
//! e salva `/tmp/golden_context_reduction.json`: lista di {group, case_id, input,
//! output}. Qui ricostruiamo l'input, chiamiamo la funzione Rust e verifichiamo
//! `output == golden Python`.
//!
//! `#[ignore]` perche' dipende dal file generato. Comando:
//!   python3 crates/nexus-agent-graph/scripts/gen_golden_context_reduction.py
//!   cargo test -p nexus-agent-graph --lib golden_context_reduction -- --ignored
//!
//! Nota sui due confini I/O: `compress_old_tool_results` e `apply_token_brake`
//! sono confrontati con l'offload DISABILITATO (marker "degraded") e un token
//! estimator DETERMINISTICO (somma dei char dei content stringa) — esattamente i
//! parametri puri che il modulo porta. La parte I/O (offload RAG / tiktoken) e'
//! fuori dalla parte pura (TODO trait dedicati).

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

/// Spec messaggio dello script -> [`HistoryMessage`]. Allineata alle convenzioni
/// dello script Python: `content` assente -> `""` (default `_Msg.content`);
/// `anthropic_content` assente o `null` -> `Value::Null`; flag default false.
fn spec_to_msg(spec: &Value) -> HistoryMessage {
    HistoryMessage {
        is_human: spec.get("is_human").and_then(Value::as_bool).unwrap_or(false),
        content: spec
            .get("content")
            .cloned()
            .unwrap_or_else(|| Value::String(String::new())),
        anthropic_content: spec.get("anthropic_content").cloned().unwrap_or(Value::Null),
        nexus_summary: spec.get("nexus_summary").and_then(Value::as_bool).unwrap_or(false),
        rolling_summary: spec.get("rolling_summary").and_then(Value::as_bool).unwrap_or(false),
    }
}

fn specs_to_msgs(arr: &Value) -> Vec<HistoryMessage> {
    arr.as_array()
        .map(|a| a.iter().map(spec_to_msg).collect())
        .unwrap_or_default()
}

/// Confronta una lista di [`HistoryMessage`] Rust con la lista di spec Python.
/// Normalizza i due lati a `Vec<HistoryMessage>` (spec -> msg) cosi' `null` vs
/// assente e i flag default coincidono.
fn assert_msgs_eq(got: &[HistoryMessage], expected_specs: &Value, ctx: &str) {
    let expected: Vec<HistoryMessage> = specs_to_msgs(expected_specs);
    assert_eq!(
        got, expected,
        "PARITA' FALLITA {ctx}:\n  rust   = {got:#?}\n  python = {expected:#?}"
    );
}

/// Token estimator deterministico identico a `_fb_token_estimator` Python.
fn det_estimator(messages: &[HistoryMessage]) -> i64 {
    messages
        .iter()
        .map(|m| match &m.content {
            Value::String(s) => s.chars().count() as i64,
            _ => 0,
        })
        .sum()
}

#[test]
#[ignore = "richiede /tmp/golden_context_reduction.json generato da gen_golden_context_reduction.py"]
fn golden_context_reduction() {
    let Some(raw) = crate::golden_util::load_golden(
        "golden_context_reduction.json",
        "gen_golden_context_reduction.py",
    ) else {
        return;
    };
    let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
    assert!(cases.len() >= 35, "attesi >= 35 casi, trovati {}", cases.len());

    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();

    for c in &cases {
        let ctx = format!("{}/{}", c.group, c.case_id);
        match c.group.as_str() {
            "should_compress_now" => {
                let inp = &c.input;
                let iteration = inp.get("iteration").and_then(Value::as_i64).unwrap();
                let cfg_j = inp.get("config").unwrap();
                let cfg = CtxMgmtConfig {
                    compress_start_iter: cfg_j["compress_start_iter"].as_i64().unwrap(),
                    compress_phase_boundaries: i64_vec(&cfg_j["compress_phase_boundaries"]),
                    compress_phase_keep_recent: i64_vec(&cfg_j["compress_phase_keep_recent"]),
                    compress_phase_max_chars: i64_vec(&cfg_j["compress_phase_max_chars"]),
                };
                let (comp, params) = should_compress_now(iteration, &cfg);
                assert_eq!(comp, c.output["compress"].as_bool().unwrap(), "{ctx} compress");
                assert_eq!(params.keep_recent, c.output["keep_recent"].as_i64().unwrap(), "{ctx} keep_recent");
                assert_eq!(
                    params.max_content_chars,
                    c.output["max_content_chars"].as_i64().unwrap(),
                    "{ctx} max_content_chars"
                );
            }
            "dedup_tool_results_history" => {
                let msgs = specs_to_msgs(&c.input["messages"]);
                let got = dedup_tool_results_history(&msgs);
                assert_msgs_eq(&got, &c.output, &ctx);
            }
            "dedup_tool_results" => {
                let msgs = specs_to_msgs(&c.input["messages"]);
                let got = dedup_tool_results(&msgs);
                assert_msgs_eq(&got, &c.output, &ctx);
            }
            "looks_like_base64" => {
                let s = c.input["s"].as_str().unwrap();
                let min_len = c.input["min_len"].as_u64().unwrap() as usize;
                assert_eq!(looks_like_base64(s, min_len), c.output.as_bool().unwrap(), "{ctx}");
            }
            "drop_unused_base64_payloads" => {
                let msgs = specs_to_msgs(&c.input["messages"]);
                let max_age = c.input["max_age"].as_i64().unwrap();
                let keep_recent = c.input["keep_recent"].as_u64().unwrap() as usize;
                let got = drop_unused_base64_payloads(&msgs, max_age, keep_recent);
                assert_msgs_eq(&got, &c.output, &ctx);
            }
            "compress_old_tool_results" => {
                let msgs = specs_to_msgs(&c.input["messages"]);
                let keep_recent = c.input["keep_recent"].as_u64().unwrap() as usize;
                let max_chars = c.input["max_content_chars"].as_u64().unwrap() as usize;
                let cutoff = c.input["cutoff_index"].as_u64().map(|v| v as usize);
                let got = compress_old_tool_results(&msgs, keep_recent, max_chars, cutoff, &degraded_marker);
                assert_msgs_eq(&got, &c.output, &ctx);
            }
            "apply_token_brake" => {
                let msgs = specs_to_msgs(&c.input["messages"]);
                let window = c.input["window"].as_i64().unwrap();
                let cfg_j = c.input["config"].as_object().unwrap();
                let cfg = TokenBrakeConfig {
                    max_context_ratio: cfg_j["max_context_ratio"].as_f64().unwrap(),
                    aggressive_keep_recent: cfg_j["aggressive_keep_recent"].as_u64().unwrap() as usize,
                    aggressive_max_chars: cfg_j["aggressive_max_chars"].as_u64().unwrap() as usize,
                };
                let got = apply_token_brake(&msgs, window, &cfg, &det_estimator);
                assert_msgs_eq(&got, &c.output, &ctx);
            }
            "inject_language_reminder" => {
                let sys = c.input["system_text"].as_str().unwrap();
                let enabled = c.input["enabled"].as_bool().unwrap();
                let text = c.input["reminder_text"].as_str().unwrap();
                let got = inject_language_reminder(sys, enabled, text);
                assert_eq!(got, c.output.as_str().unwrap(), "{ctx}");
            }
            "inject_turn_focus" => {
                let sys = c.input["system_text"].as_str().unwrap();
                let directive = c.input["directive"].as_str().unwrap();
                let got = inject_turn_focus(sys, directive);
                assert_eq!(got, c.output.as_str().unwrap(), "{ctx}");
            }
            "inject_verification_directive" => {
                let sys = c.input["system_text"].as_str().unwrap();
                let detected = c.input["detected"].as_bool().unwrap();
                let enabled = c.input["enabled"].as_bool().unwrap();
                let directive = c.input["directive"].as_str().unwrap();
                let got = inject_verification_directive(sys, detected, enabled, directive);
                assert_eq!(got, c.output.as_str().unwrap(), "{ctx}");
            }
            "inject_forced_rag_reminder" => {
                let msgs = specs_to_msgs(&c.input["messages"]);
                let sys = c.input["system_text"].as_str().unwrap();
                let est = c.input["est_tokens"].as_i64().unwrap();
                let window = c.input["window"].as_i64().unwrap();
                let ratio = c.input["ratio"].as_f64().unwrap();
                let text = c.input["reminder_text"].as_str().unwrap();
                let (got_msgs, got_sys) =
                    inject_forced_rag_reminder(&msgs, sys, est, window, ratio, text);
                assert_msgs_eq(&got_msgs, &c.output["messages"], &ctx);
                assert_eq!(got_sys, c.output["system_text"].as_str().unwrap(), "{ctx} system_text");
            }
            other => panic!("gruppo golden sconosciuto: {other} (caso {})", c.case_id),
        }
        *counts.entry(c.group.clone()).or_insert(0) += 1;
    }

    // Ogni gruppo ha almeno un caso.
    for g in [
        "should_compress_now",
        "dedup_tool_results_history",
        "dedup_tool_results",
        "looks_like_base64",
        "drop_unused_base64_payloads",
        "compress_old_tool_results",
        "apply_token_brake",
        "inject_language_reminder",
        "inject_turn_focus",
        "inject_verification_directive",
        "inject_forced_rag_reminder",
    ] {
        assert!(counts.get(g).copied().unwrap_or(0) > 0, "nessun caso per il gruppo {g}");
    }
    println!("golden context_reduction: {} casi verificati per gruppo {counts:?}", cases.len());
}

/// Converte un array JSON di interi in `Vec<i64>`.
fn i64_vec(v: &Value) -> Vec<i64> {
    v.as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}
