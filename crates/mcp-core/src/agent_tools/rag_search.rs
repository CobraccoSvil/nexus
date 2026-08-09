//! Tool agente `nexus_search_semantic`: ricerca semantica unificata
//! su allegati, knowledge base, chat history, tool results cached.
//!
//! Fa parte della pipeline RAG strutturale (ADR 0015). Permette agli
//! agent di recuperare frammenti rilevanti senza ri-leggere interi file.
//!
//! Esito (regola Q): il tool ritorna [`RispostaTool`] — il payload JSON sta nel
//! testo, l'esito e la NATURA del fallimento stanno nei campi. Prima il
//! fallimento viaggiava come marker anteposto al JSON: spezzava il payload, e
//! chi doveva sapere com'era andata era costretto a rileggere il testo.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::rag::{self, RagError, SourceKind};

use super::AgentToolContext;
use nexus_agent_tools::input_contract::InputTool;
use nexus_agent_tools::tool_inputs::NexusSearchSemanticInput;
use nexus_types::tool_outcome::{NaturaFallimento, RispostaTool};

/// Il messaggio per il modello quando una o piu' fonti non hanno risposto.
/// Non e' un "non trovato": la ricerca non ha potuto guardare.
const HINT_FONTI_MUTE: &str = "una o piu' fonti non hanno potuto rispondere: questo NON e' un \
     'non trovato'. Non ripetere la stessa query; usa read_file / \
     search_in_files sui percorsi che gia' conosci.";

/// Il messaggio per lo zero pulito: l'indice ha risposto, e ha risposto niente.
const HINT_ZERO_PULITO: &str = "nessun risultato nell'indice semantico: ripetere la stessa query \
     dara' ancora zero. Per cercare nei sorgenti usa search_in_files \
     o leggi direttamente i file citati nel task.";

/// I parametri della ricerca gia' letti dal contratto e VALIDATI.
///
/// I tre campi che qui si controllano venivano prima ridotti al default in
/// silenzio: chi aveva chiesto una restrizione riceveva la ricerca LARGA
/// credendo di averla ristretta.
#[derive(Debug)]
struct ParametriRicerca {
    query: String,
    kinds: Vec<SourceKind>,
    top_k: Option<usize>,
    filter_session_id: Option<Uuid>,
    extra: Vec<(String, Value)>,
}

pub async fn tool_nexus_search_semantic(ctx: &AgentToolContext, input: &Value) -> RispostaTool {
    let params = match leggi_parametri(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };

    match rag::search_semantic(
        &ctx.db,
        &params.query,
        params.kinds,
        Some(ctx.project_id),
        params.filter_session_id,
        params.top_k,
        params.extra,
    )
    .await
    {
        Ok(report) => render_search_result(&params.query, report),
        Err(e) => {
            tracing::warn!("nexus_search_semantic: {}", e);
            errore_ricerca(&e)
        }
    }
}

/// Legge l'input dal contratto e valida cio' che lo schema non puo' vincolare
/// da solo. Ogni rifiuto e' RIMEDIABILE e nomina il campo da correggere.
///
/// Il contratto d'ingresso sostituisce cinque `input.get(...)` scritti a mano,
/// e con essi il difetto peggiore di questo handler: `source_kinds` passava da
/// un `filter_map` che SCARTAVA i valori non riconosciuti in silenzio. Chiedere
/// di filtrare su una sorgente inesistente non produceva un errore — produceva
/// la lista vuota, cioe' il DEFAULT, cioe' tutte le sorgenti: il modello
/// credeva di aver ristretto la ricerca e otteneva l'opposto.
fn leggi_parametri(input: &Value) -> Result<ParametriRicerca, RispostaTool> {
    let letto = NexusSearchSemanticInput::leggi(input)?;

    let query = letto.query.trim().to_string();
    if query.is_empty() {
        return Err(RispostaTool::fallito_rimediabile(
            "Il campo 'query' e' vuoto: passa il testo da cercare \
             (es. \"cosa fa il bottone Send nel chat input?\").",
        ));
    }

    Ok(ParametriRicerca {
        query,
        kinds: letto.source_kinds.unwrap_or_default(),
        top_k: leggi_top_k(letto.top_k)?,
        filter_session_id: leggi_session_id(letto.filter_session_id.as_deref())?,
        extra: leggi_filtro_allegato(letto.filter_attachment_id.as_deref())?,
    })
}

/// `top_k` arriva come `i64` e veniva convertito con `usize::try_from(..).ok()`:
/// un valore NEGATIVO diventava "campo assente", cioe' il default. Chi chiede
/// -1 hit non sta chiedendo il default, sta sbagliando la chiamata, e dirlo e'
/// meglio che ignorarlo. Il tetto di 100 resta al chiamato, che lo clampa: il
/// catalogo lo promette, quindi non e' una sorpresa da dichiarare qui.
fn leggi_top_k(grezzo: Option<i64>) -> Result<Option<usize>, RispostaTool> {
    match grezzo {
        None => Ok(None),
        // Positivo: la conversione non puo' fallire su 64 bit, e il valore
        // enorme lo taglia comunque il clamp del chiamato.
        Some(n) if n > 0 => Ok(Some(usize::try_from(n).unwrap_or(usize::MAX))),
        Some(n) => Err(RispostaTool::fallito_rimediabile(format!(
            "'top_k' deve essere un intero positivo (ricevuto {n}): e' il numero \
             di hit da riportare, al massimo 100. Omettilo per il default \
             configurato in agent.rag.top_k_default."
        ))),
    }
}

/// Un UUID malformato veniva scartato con `.ok()`: il filtro che il modello
/// aveva CHIESTO non veniva applicato, e la risposta tornava con gli hit di
/// tutte le sessioni come se il filtro avesse funzionato. E' lo stesso difetto
/// gia' chiuso per `source_kinds`, nell'ultimo campo in cui era rimasto.
fn leggi_session_id(grezzo: Option<&str>) -> Result<Option<Uuid>, RispostaTool> {
    let Some(testo) = grezzo.map(str::trim) else {
        return Ok(None);
    };
    Uuid::parse_str(testo).map(Some).map_err(|_| {
        RispostaTool::fallito_rimediabile(format!(
            "'filter_session_id' non e' un UUID: ricevuto '{testo}'. Passa la \
             session_id nella forma 8-4-4-4-12, oppure ometti il campo per \
             cercare in tutte le sessioni."
        ))
    })
}

/// Costruisce il filtro per singolo allegato. Un id VUOTO finiva in
/// `source_id: ""`, cioe' un filtro che nessun punto indicizzato soddisfa: la
/// ricerca tornava zero hit per costruzione e il modello la leggeva come
/// "questo allegato non contiene nulla".
fn leggi_filtro_allegato(grezzo: Option<&str>) -> Result<Vec<(String, Value)>, RispostaTool> {
    let Some(testo) = grezzo.map(str::trim) else {
        return Ok(Vec::new());
    };
    if testo.is_empty() {
        return Err(RispostaTool::fallito_rimediabile(
            "'filter_attachment_id' e' vuoto: passa l'attachment_id che ritorna \
             nexus_list_attachments, oppure ometti il campo per cercare in tutti \
             gli allegati.",
        ));
    }
    Ok(vec![("source_id".to_string(), json!(testo))])
}

/// La natura viene dalla VARIANTE dell'errore, mai dal suo messaggio (regola M):
/// il testo di `RagError` e' composto per l'umano e cambia con i provider sotto.
fn natura_rag(e: &RagError) -> NaturaFallimento {
    match e {
        // RAG spento da settings, configurazione invalida, DB che non risponde:
        // nessun parametro della chiamata li cambia, e ripeterla ridara' lo
        // stesso errore. All'agente resta un'altra strada (search_in_files).
        RagError::Disabled | RagError::Config(_) | RagError::Db(_) => NaturaFallimento::DelSistema,
        // Le due chiamate di rete della ricerca: l'embedder del brain e Qdrant.
        // Un endpoint saturo o momentaneamente irraggiungibile e' esattamente il
        // caso in cui ritentare identico e' la strategia giusta.
        RagError::Embed(_) | RagError::Qdrant(_) => NaturaFallimento::Transitorio,
        // Qui l'`ErrorKind` e' ancora intero: la natura la legge lui, non io.
        RagError::Io(io) => NaturaFallimento::da_errore_io(io),
    }
}

/// L'esito FALLITO della ricerca: il payload JSON nel testo, la natura nel campo.
fn errore_ricerca(e: &RagError) -> RispostaTool {
    let payload = json!({ "error": format!("rag search fallita: {e}"), "hits": [] });
    RispostaTool::fallito(payload.to_string()).con_natura(natura_rag(e))
}

/// Il payload JSON dell'esito, composto DAI fatti del report (regola Q punto 3):
/// il testo si compone dai campi, mai il contrario.
fn payload_ricerca(query: &str, report: &rag::SemanticSearchReport) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("query".into(), json!(query));
    out.insert("count".into(), json!(report.hits.len()));
    out.insert("hits".into(), json!(report.hits));
    if !report.collections_fallite.is_empty() {
        let fallite: Vec<Value> = report
            .collections_fallite
            .iter()
            .map(|(k, e)| json!({"kind": k, "errore": e}))
            .collect();
        out.insert("collections_fallite".into(), Value::Array(fallite));
        out.insert("hint".into(), json!(HINT_FONTI_MUTE));
    } else if report.hits.is_empty() {
        out.insert("hint".into(), json!(HINT_ZERO_PULITO));
    }
    Value::Object(out)
}

/// Rende l'esito della ricerca distinguendo i tre casi che prima collassavano
/// tutti su `count: 0` (regola M):
/// - hit trovati: il risultato utile;
/// - zero hit con collection fallite: NON e' "non trovato" — la fonte non ha
///   potuto rispondere;
/// - zero hit puliti: la risposta e' davvero "qui non c'e'".
///
/// Misurato il 31/07/2026 (run 49fbc5d7): 8 ricerche identiche a zero risultati
/// consecutive, chiusura per loop. Nessuna riga diceva al modello che
/// l'insistere era strutturalmente inutile.
///
/// RAMO NUDO CHIUSO: zero hit con fonti mute usciva come SUCCESSO, e il solo
/// modo di accorgersene era leggere il campo `hint` dentro il JSON — cioe'
/// nessuno, perche' anti-loop, supervisore e final_gate guardano l'esito. La
/// ricerca non ha risposto "niente": non ha potuto guardare, e le due cose
/// portano a decisioni opposte. La natura e' DEL SISTEMA perche' una collection
/// assente o irraggiungibile non si corregge cambiando la chiamata, ed e' cio'
/// che l'`hint` gia' diceva a parole ("Non ripetere la stessa query").
///
/// Con almeno un hit resta un SUCCESSO anche se una fonte ha taciuto: i
/// risultati ci sono e sono utilizzabili: bocciare l'intera ricerca butterebbe
/// via cio' che ha trovato. L'`hint` resta nel payload per dirlo.
fn render_search_result(query: &str, report: rag::SemanticSearchReport) -> RispostaTool {
    let fonti_mute = !report.collections_fallite.is_empty();
    let vuoto = report.hits.is_empty();
    let testo = payload_ricerca(query, &report).to_string();
    if fonti_mute && vuoto {
        return RispostaTool::fallito(testo).con_natura(NaturaFallimento::DelSistema);
    }
    RispostaTool::riuscito(testo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload_di(risposta: &RispostaTool) -> Value {
        serde_json::from_str(&risposta.testo).expect("il testo del tool e' JSON valido")
    }

    /// Parte dal PRODUTTORE reale dell'errore (`errore_ricerca` sulla variante
    /// che il chiamato emette davvero), non da un JSON scritto a mano.
    #[test]
    fn errore_del_rag_dichiara_esito_e_natura() {
        let out = errore_ricerca(&RagError::Qdrant("connection refused".into()));
        assert!(out.esito.e_fallito(), "{out:?}");
        // Qdrant e' una chiamata di rete: ritentare identico e' legittimo.
        assert_eq!(out.natura, Some(NaturaFallimento::Transitorio), "{out:?}");
        let parsed = payload_di(&out);
        assert!(
            parsed["error"]
                .as_str()
                .expect("campo error")
                .contains("connection refused"),
            "{parsed}"
        );
    }

    /// Il RAG spento non si corregge cambiando la chiamata: ripeterla ridara'
    /// lo stesso errore, quindi l'agente deve cambiare strada.
    #[test]
    fn rag_disabilitato_e_del_sistema() {
        let out = errore_ricerca(&RagError::Disabled);
        assert_eq!(out.natura, Some(NaturaFallimento::DelSistema), "{out:?}");
    }

    /// RAMO NUDO: zero hit con collection fallite non e' un "non trovato", ed
    /// e' l'esito che prima usciva come successo. Parte dal produttore reale
    /// (`render_search_result`), non da un JSON scritto a mano (regola O).
    #[test]
    fn una_collection_fallita_non_e_un_non_trovato() {
        let report = rag::SemanticSearchReport {
            hits: Vec::new(),
            collections_fallite: vec![("code".into(), "HTTP 404 collection assente".into())],
        };
        let out = render_search_result("frontend App counters", report);
        assert!(out.esito.e_fallito(), "{out:?}");
        assert_eq!(out.natura, Some(NaturaFallimento::DelSistema), "{out:?}");
        let v = payload_di(&out);
        assert_eq!(v["count"], 0);
        assert_eq!(v["collections_fallite"][0]["kind"], "code");
        let hint = v["hint"].as_str().expect("hint presente");
        assert!(
            hint.contains("NON e' un 'non trovato'"),
            "l'hint deve dire che la fonte non ha risposto: {hint}"
        );
    }

    /// Con almeno un hit la fonte muta NON boccia la ricerca: i risultati
    /// trovati sono utilizzabili, e l'hint resta nel payload per dichiararlo.
    #[test]
    fn hit_trovati_con_una_fonte_muta_restano_un_successo() {
        let report = rag::SemanticSearchReport {
            hits: vec![rag::search::SearchHit {
                source_kind: "kb".into(),
                source_id: "nota-1".into(),
                chunk_index: 0,
                chunk_text: "il bottone Send invia il messaggio".into(),
                score: 0.7,
                metadata: Value::Null,
            }],
            collections_fallite: vec![("code".into(), "HTTP 503".into())],
        };
        let out = render_search_result("Send", report);
        assert!(!out.esito.e_fallito(), "{out:?}");
        let v = payload_di(&out);
        assert_eq!(v["count"], 1);
        assert!(v["hint"].is_string(), "{v}");
    }

    /// Zero hit puliti: niente allarme (una ricerca andata a buon fine che non
    /// trova nulla e' un SUCCESSO), ma l'hint dice che insistere e' inutile.
    /// E' il caso delle 8 ricerche identiche del run 49fbc5d7.
    #[test]
    fn zero_hit_puliti_scoraggiano_la_ripetizione() {
        let out = render_search_result("query senza riscontri", rag::SemanticSearchReport::default());
        assert!(!out.esito.e_fallito(), "{out:?}");
        let v = payload_di(&out);
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
        let out = render_search_result("App", report);
        assert!(!out.esito.e_fallito(), "{out:?}");
        let v = payload_di(&out);
        assert_eq!(v["count"], 1);
        assert!(v.get("hint").is_none());
        // L'hit di codice porta il SUO file: senza, non e' azionabile.
        assert_eq!(v["hits"][0]["source_id"], "frontend/src/App.tsx");
    }

    /// Un `top_k` negativo diventava il DEFAULT: la chiamata sbagliata veniva
    /// eseguita come se fosse giusta. Ora e' un rifiuto che nomina il campo.
    #[test]
    fn top_k_non_positivo_e_rifiutato() {
        let risposta = leggi_top_k(Some(-3)).expect_err("un top_k negativo e' un errore");
        assert_eq!(risposta.natura, Some(NaturaFallimento::Rimediabile));
        assert!(risposta.testo.contains("'top_k'"), "{}", risposta.testo);
        assert!(leggi_top_k(Some(0)).is_err(), "zero hit non e' una richiesta");
        assert_eq!(leggi_top_k(Some(7)).expect("positivo ammesso"), Some(7));
        assert_eq!(leggi_top_k(None).expect("assente ammesso"), None);
    }

    /// Una session_id malformata veniva SCARTATA: la ricerca tornava larga e il
    /// modello leggeva quegli hit come se il filtro fosse stato applicato.
    #[test]
    fn session_id_malformata_e_rifiutata() {
        let risposta = leggi_session_id(Some("non-un-uuid")).expect_err("uuid invalido");
        assert_eq!(risposta.natura, Some(NaturaFallimento::Rimediabile));
        assert!(
            risposta.testo.contains("filter_session_id"),
            "il messaggio deve nominare il campo: {}",
            risposta.testo
        );
        let valido = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
        assert_eq!(
            leggi_session_id(Some(valido)).expect("uuid valido"),
            Some(Uuid::parse_str(valido).expect("uuid di test"))
        );
        assert_eq!(leggi_session_id(None).expect("assente ammesso"), None);
    }

    /// Un `filter_attachment_id` vuoto produceva un filtro che nessun punto
    /// soddisfa: zero hit per costruzione, letti come "allegato senza nulla".
    #[test]
    fn filtro_allegato_vuoto_e_rifiutato() {
        let risposta = leggi_filtro_allegato(Some("   ")).expect_err("id vuoto");
        assert_eq!(risposta.natura, Some(NaturaFallimento::Rimediabile));
        assert!(
            risposta.testo.contains("nexus_list_attachments"),
            "il rimedio deve nominare il tool che produce l'id: {}",
            risposta.testo
        );
        assert_eq!(
            leggi_filtro_allegato(Some("att-1")).expect("id valido"),
            vec![("source_id".to_string(), json!("att-1"))]
        );
        assert!(leggi_filtro_allegato(None)
            .expect("assente ammesso")
            .is_empty());
    }

    /// La query vuota passa dal contratto (lo schema non vincola la lunghezza)
    /// e va rifiutata qui: il chiamato risponderebbe zero hit puliti, cioe'
    /// "non trovato", per una domanda che non e' mai stata posta.
    #[test]
    fn query_vuota_e_rifiutata() {
        let risposta = leggi_parametri(&json!({"query": "   "})).expect_err("query vuota");
        assert_eq!(risposta.natura, Some(NaturaFallimento::Rimediabile));
        assert!(risposta.testo.contains("'query'"), "{}", risposta.testo);
    }
}
