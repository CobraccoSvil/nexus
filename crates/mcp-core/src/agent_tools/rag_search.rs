//! Tool agente `nexus_search_semantic`: ricerca semantica unificata
//! su allegati, knowledge base, chat history, tool results cached.
//!
//! Fa parte della pipeline RAG strutturale (ADR 0015). Permette agli
//! agent di recuperare frammenti rilevanti senza ri-leggere interi file.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::rag::{self, SourceKind};

use super::AgentToolContext;
use nexus_types::tool_outcome::tool_failure;

/// Costruisce l'esito FALLITO del tool: marker + payload JSON (contratto
/// `nexus_types::tool_outcome`). Senza il marker in testa questi fallimenti
/// erano indistinguibili da una ricerca riuscita per anti-loop/supervisore/
/// final_gate, che leggono solo `is_tool_failure`.
fn search_failure(payload: Value) -> String {
    tool_failure(payload.to_string())
}

pub async fn tool_nexus_search_semantic(ctx: &AgentToolContext, input: &Value) -> String {
    use nexus_agent_tools::input_contract::InputTool;
    use nexus_agent_tools::tool_inputs::NexusSearchSemanticInput;

    // Il contratto d'ingresso sostituisce cinque `input.get(...)` scritti a
    // mano, e con essi il difetto peggiore di questo handler: `source_kinds`
    // passava da un `filter_map` che SCARTAVA i valori non riconosciuti in
    // silenzio. Chiedere di filtrare su una sorgente inesistente non produceva
    // un errore — produceva la lista vuota, cioe' il DEFAULT, cioe' tutte le
    // sorgenti: il modello credeva di aver ristretto la ricerca e otteneva
    // l'opposto. Ora un valore fuori vocabolario e' un fallimento rimediabile
    // che elenca gli ammessi.
    let params = match NexusSearchSemanticInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return search_failure(json!({"error": risposta.testo})),
    };

    let query = params.query.trim().to_string();
    if query.is_empty() {
        return search_failure(json!({"error": "campo 'query' obbligatorio"}));
    }
    let top_k = params.top_k.and_then(|n| usize::try_from(n).ok());
    let kinds: Vec<SourceKind> = params.source_kinds.unwrap_or_default();
    let filter_session_id = params
        .filter_session_id
        .as_deref()
        .and_then(|s| Uuid::parse_str(s).ok());

    let mut extra: Vec<(String, Value)> = Vec::new();
    if let Some(att_id) = params.filter_attachment_id {
        extra.push(("source_id".to_string(), json!(att_id)));
    }

    match rag::search_semantic(
        &ctx.db,
        &query,
        kinds,
        Some(ctx.project_id),
        filter_session_id,
        top_k,
        extra,
    )
    .await
    {
        Ok(report) => render_search_result(&query, report),
        Err(e) => {
            tracing::warn!("nexus_search_semantic: {}", e);
            search_failure(json!({"error": format!("rag search fallita: {e}"), "hits": []}))
        }
    }
}

/// Rende l'esito della ricerca per il MODELLO, distinguendo i tre casi che
/// prima collassavano tutti su `count: 0` (regola M):
/// - hit trovati: il risultato utile;
/// - zero hit con collection fallite: NON e' "non trovato" — la fonte non ha
///   potuto rispondere, e il campo `hint` dice di non ripetere la stessa query;
/// - zero hit puliti: la risposta e' davvero "qui non c'e'", e l'hint indirizza
///   verso gli strumenti che leggono i FILE (una query ripetuta su un indice
///   che ha gia' risposto zero non produrra' un risultato diverso).
///
/// Misurato il 31/07/2026 (run 49fbc5d7): 8 ricerche identiche a zero risultati
/// consecutive, chiusura per loop. Nessuna riga diceva al modello che
/// l'insistere era strutturalmente inutile.
fn render_search_result(query: &str, report: rag::SemanticSearchReport) -> String {
    let mut out = serde_json::Map::new();
    out.insert("query".into(), json!(query));
    out.insert("count".into(), json!(report.hits.len()));
    out.insert("hits".into(), json!(report.hits));
    if !report.collections_fallite.is_empty() {
        out.insert(
            "collections_fallite".into(),
            json!(report
                .collections_fallite
                .iter()
                .map(|(k, e)| json!({"kind": k, "errore": e}))
                .collect::<Vec<_>>()),
        );
        out.insert(
            "hint".into(),
            json!(
                "una o piu' fonti non hanno potuto rispondere: questo NON e' un \
                 'non trovato'. Non ripetere la stessa query; usa read_file / \
                 search_in_files sui percorsi che gia' conosci."
            ),
        );
    } else if report.hits.is_empty() {
        out.insert(
            "hint".into(),
            json!(
                "nessun risultato nell'indice semantico: ripetere la stessa query \
                 dara' ancora zero. Per cercare nei sorgenti usa search_in_files \
                 o leggi direttamente i file citati nel task."
            ),
        );
    }
    Value::Object(out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_failure_dichiara_il_fallimento_e_preserva_il_payload() {
        // Chiama il PRODUTTORE reale usato dai 2 rami di errore del tool
        // (query mancante, ricerca semantica fallita).
        let out = search_failure(json!({"error": "rag search fallita: qdrant down", "hits": []}));
        assert!(nexus_types::tool_outcome::is_tool_failure(&out));
        let after_marker = out
            .trim_start_matches(nexus_types::tool_outcome::TOOL_FAILURE_MARKER)
            .trim_start();
        let parsed: Value =
            serde_json::from_str(after_marker).expect("payload dopo il marker e' JSON valido");
        assert_eq!(parsed["error"], "rag search fallita: qdrant down");
    }

    /// Zero hit con collection fallite: il risultato DISTINGUE il guasto dal
    /// "non trovato" e l'hint vieta la ripetizione. Parte dal produttore reale
    /// (`render_search_result`), non da un JSON scritto a mano (regola O).
    #[test]
    fn una_collection_fallita_non_e_un_non_trovato() {
        let report = rag::SemanticSearchReport {
            hits: Vec::new(),
            collections_fallite: vec![("code".into(), "HTTP 404 collection assente".into())],
        };
        let out = render_search_result("frontend App counters", report);
        let v: Value = serde_json::from_str(&out).expect("json valido");
        assert_eq!(v["count"], 0);
        assert_eq!(v["collections_fallite"][0]["kind"], "code");
        let hint = v["hint"].as_str().expect("hint presente");
        assert!(
            hint.contains("NON e' un 'non trovato'"),
            "l'hint deve dire che la fonte non ha risposto: {hint}"
        );
    }

    /// Zero hit puliti: niente allarme, ma l'hint dice che insistere e' inutile.
    /// E' il caso delle 8 ricerche identiche del run 49fbc5d7.
    #[test]
    fn zero_hit_puliti_scoraggiano_la_ripetizione() {
        let out = render_search_result("query senza riscontri", rag::SemanticSearchReport::default());
        let v: Value = serde_json::from_str(&out).expect("json valido");
        assert_eq!(v["count"], 0);
        assert!(v.get("collections_fallite").is_none());
        let hint = v["hint"].as_str().expect("hint presente");
        assert!(hint.contains("ripetere la stessa query"), "hint: {hint}");
    }

    /// Con risultati veri l'hint NON compare: un suggerimento appeso a un
    /// esito riuscito diventerebbe rumore che il modello impara a ignorare.
    #[test]
    fn con_hit_nessun_hint() {
        let report = rag::SemanticSearchReport {
            hits: vec![rag::search::SearchHit {
                source_kind: "code".into(),
                source_id: "frontend/src/App.tsx".into(),
                chunk_index: 0,
                chunk_text: "export default function App()".into(),
                score: 0.9,
                metadata: Value::Null,
            }],
            collections_fallite: Vec::new(),
        };
        let v: Value = serde_json::from_str(&render_search_result("App", report)).unwrap();
        assert_eq!(v["count"], 1);
        assert!(v.get("hint").is_none());
        // L'hit di codice porta il SUO file: senza, non e' azionabile.
        assert_eq!(v["hits"][0]["source_id"], "frontend/src/App.tsx");
    }
}
