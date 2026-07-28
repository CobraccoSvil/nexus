//! PUNTO UNICO (regola L) della domanda: **quali chiamate HTTP prova il gate
//! finale, e come**.
//!
//! Ci sono DUE fonti di endpoint, e confluiscono entrambe qui:
//!
//! 1. la configurazione per-progetto (`run_configurations` con `role='endpoint'`
//!    e `http_spec`), risolta a monte in mcp-core e passata al nodo in
//!    `FinalGateConfig::endpoint_criteria` (regola G: il nodo non legge il DB);
//! 2. la DICHIARAZIONE dell'agente (`task_complete.endpoints`, ADR 0034),
//!    normalizzata da [`normalize_endpoints`] e tradotta in criteri da
//!    [`endpoint_criteria_from_declaration`].
//!
//! ## Perche' la dichiarazione, e non le altre due strade
//!
//! Il difetto che questo modulo chiude: il gate chiudeva "superato" su
//! applicazioni con endpoint rotti perche' non ne provava NESSUNO (caso reale
//! gestione-spese, 2026-07-28: `GET /api/expenses` 200, `POST /api/expenses`
//! 500 per uno schema SQLite disallineato; gate "superato" piu' volte). La
//! cablatura della fonte (1) mancava del tutto nel motore nativo, ma da sola non
//! basterebbe: e' una configurazione MANUALE, e una configurazione che nessuno
//! compila equivale a nessuna verifica — il progetto dell'incidente non l'aveva.
//!
//! Le alternative valutate per DERIVARE gli endpoint invece di attenderli:
//!
//! - **derivazione dal codice delle route**: richiede un parser per framework
//!   (Express, Fastify, FastAPI, Axum, ...). Fragile per costruzione, e un
//!   parser che non riconosce il framework tace invece di fallire — lo stesso
//!   modo di mentire che stiamo correggendo;
//! - **derivazione dalle chiamate HTTP gia' osservate nella history**: copre
//!   SOLO cio' che l'agente ha gia' provato, cioe' tipicamente le sole GET. Nel
//!   caso reale l'agente aveva provato `curl /api/health` e `curl /api/expenses`
//!   (GET): derivare da li' avrebbe riprodotto ESATTAMENTE il falso positivo,
//!   con in piu' l'aria di una verifica. La history resta usata per un'altra
//!   domanda — "il silenzio e' sospetto?" — non per costruire criteri;
//! - **dichiarazione strutturata dell'agente** (scelta): e' l'unica fonte che
//!   conosce i metodi di SCRITTURA appena creati, ed e' il canale che il resto
//!   dell'architettura usa gia' per i fatti macchina (ADR 0034 / regola M:
//!   segnale strutturato, mai prosa). Il suo limite — se l'agente non dichiara
//!   nulla non si verifica nulla — non e' nascosto: il gate lo DICHIARA e chiude
//!   "svolto ma non verificato" invece di spacciarlo per verificato (vedi
//!   `FinalGateNode::run`).
//!
//! ## Effetto collaterale delle prove di scrittura
//!
//! Provare una `POST` crea un record nell'applicazione verificata. E' accettato
//! e documentato: l'alternativa — pretendere che l'app generata esponga un
//! endpoint di pulizia — imporrebbe un requisito che nessuno soddisferebbe,
//! cioe' di nuovo la configurazione manuale mai compilata che e' la causa di
//! questo difetto. La descrizione del tool `task_complete` chiede percio'
//! all'agente un `body` di PROVA riconoscibile, e la spec del criterio lo
//! riporta per intero: il record spurio resta rintracciabile.
//!
//! ## Regola M
//!
//! L'esito di una prova nasce dallo **status HTTP** (segnale strutturato).
//! Nessun criterio derivato da una dichiarazione usa `body_contains`: il corpo
//! della risposta entra solo nell'evidence, per la diagnosi umana.

use serde_json::{json, Map, Value};

use crate::runtime::ports::CriterionSpec;

/// Metodi HTTP dichiarabili in `task_complete.endpoints` (enum dello schema del
/// tool: il vocabolario e' UNO, regola N).
pub const VALID_ENDPOINT_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE"];

/// Metodi il cui esito dipende da un payload: senza `body` la chiamata misura la
/// validazione dell'input, non la funzionalita' dell'endpoint (una `POST` nuda a
/// un'API JSON risponde 400 e produrrebbe un rosso che non riguarda il lavoro
/// svolto). `DELETE` non e' incluso: e' legittimo senza corpo.
pub const BODY_REQUIRED_METHODS: &[&str] = &["POST", "PUT", "PATCH"];

/// Status accettati quando l'agente non ne dichiara uno: la famiglia 2xx usata
/// dalle API REST (200 letture/aggiornamenti, 201 creazioni, 202 accettato, 204
/// senza corpo). Un 4xx/5xx fallisce il criterio, ed e' il punto: il 500 su POST
/// dell'incidente cade qui.
pub const DEFAULT_SUCCESS_STATUSES: &[i64] = &[200, 201, 202, 204];

/// Cap sul numero di endpoint accettati da una dichiarazione: il gate esegue
/// chiamate REALI e il run ha un budget di tempo. Oltre il cap le voci in eccesso
/// sono ignorate (le prime N restano, nell'ordine dichiarato).
pub const ENDPOINTS_CAP: usize = 12;

/// Intervallo degli status HTTP validi (RFC 9110): uno `status` dichiarato fuori
/// da qui non e' un'attesa, e' un refuso — la voce cade sul default 2xx.
const STATUS_MIN: i64 = 100;
const STATUS_MAX: i64 = 599;

/// Normalizza il campo `endpoints` di `task_complete` (ADR 0034). Ritorna le sole
/// voci PROVABILI, nell'ordine dichiarato — l'ordine e' load-bearing: permette
/// all'agente di dichiarare la `POST` che crea prima della `GET`/`DELETE` che
/// dipendono da quel dato.
///
/// Una voce e' provabile se: `url` assoluto http/https, `method` nell'enum
/// [`VALID_ENDPOINT_METHODS`], e `body` presente per i metodi di
/// [`BODY_REQUIRED_METHODS`]. Le altre sono scartate (come `blocker` fuori enum:
/// il resto della dichiarazione resta valido). Uno scarto NON e' silenzio del
/// gate: se dopo il filtro non resta alcun endpoint, il gate dichiara di non aver
/// verificato nulla.
pub fn normalize_endpoints(raw: Option<&Value>) -> Vec<Value> {
    let Some(Value::Array(arr)) = raw else {
        return Vec::new();
    };
    let mut out: Vec<Value> = Vec::new();
    for item in arr {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let url = obj
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            continue;
        }
        let method = obj
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .trim()
            .to_uppercase();
        if !VALID_ENDPOINT_METHODS.contains(&method.as_str()) {
            continue;
        }
        let body = obj.get("body").filter(|b| !b.is_null());
        if BODY_REQUIRED_METHODS.contains(&method.as_str()) && body.is_none() {
            continue;
        }
        let mut voce = Map::new();
        voce.insert("method".to_string(), Value::String(method));
        voce.insert("url".to_string(), Value::String(url));
        if let Some(b) = body {
            voce.insert("body".to_string(), b.clone());
        }
        // `status` atteso: intero valido. Fuori range o assente -> il criterio
        // cade su DEFAULT_SUCCESS_STATUSES.
        if let Some(s) = obj.get("status").and_then(Value::as_i64) {
            if (STATUS_MIN..=STATUS_MAX).contains(&s) {
                voce.insert("status".to_string(), json!(s));
            }
        }
        out.push(Value::Object(voce));
        if out.len() >= ENDPOINTS_CAP {
            break;
        }
    }
    out
}

/// Traduce gli endpoint DICHIARATI (gia' normalizzati da [`normalize_endpoints`],
/// cioe' letti da `declared_outcome["endpoints"]`) in criteri `http` del gate.
/// PURA: nessun I/O, nessuna lettura DB.
///
/// `expected.status` e' una LISTA (il runner accetta intero o lista): quello
/// dichiarato, altrimenti [`DEFAULT_SUCCESS_STATUSES`]. Nessun `body_contains`
/// (regola M: si decide sullo status, non sul corpo).
pub fn endpoint_criteria_from_declaration(
    declared_outcome: Option<&Value>,
    timeout_s: f64,
) -> Vec<CriterionSpec> {
    let endpoints = declared_outcome
        .and_then(|d| d.get("endpoints"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    endpoints
        .iter()
        .filter_map(|e| criterion_from_endpoint(e, timeout_s))
        .collect()
}

/// Costruisce il criterio `http` di UNA voce gia' normalizzata. `None` se la voce
/// non ha i campi minimi (difesa: `declared_outcome` puo' arrivare da un
/// checkpoint scritto da una versione precedente della normalizzazione).
fn criterion_from_endpoint(endpoint: &Value, timeout_s: f64) -> Option<CriterionSpec> {
    let obj = endpoint.as_object()?;
    let url = obj.get("url").and_then(Value::as_str)?;
    let method = obj.get("method").and_then(Value::as_str).unwrap_or("GET");
    let mut spec = Map::new();
    spec.insert("url".to_string(), json!(url));
    spec.insert("method".to_string(), json!(method));
    if let Some(body) = obj.get("body") {
        spec.insert("body".to_string(), body.clone());
    }
    // Provenienza: distingue nell'evidence una prova DICHIARATA dall'agente da
    // una configurata nel progetto (diagnosi, non decisione).
    spec.insert("source".to_string(), json!("declared"));
    let statuses: Vec<i64> = match obj.get("status").and_then(Value::as_i64) {
        Some(s) => vec![s],
        None => DEFAULT_SUCCESS_STATUSES.to_vec(),
    };
    Some(CriterionSpec {
        criterion_type: "http".to_string(),
        spec: Value::Object(spec),
        expected: json!({ "status": statuses }),
        timeout_s: Some(timeout_s),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizza_scarta_le_voci_non_provabili() {
        let raw = json!([
            {"method": "get", "url": "http://localhost:3000/api/expenses"},
            // POST senza body: non provabile (misurerebbe la validazione).
            {"method": "POST", "url": "http://localhost:3000/api/expenses"},
            // URL relativo: il gate non sa su quale host chiamarlo.
            {"method": "GET", "url": "/api/expenses"},
            // Metodo fuori enum.
            {"method": "TRACE", "url": "http://localhost:3000/api/expenses"},
            {"method": "POST", "url": "http://localhost:3000/api/expenses",
             "body": {"amount": 1}, "status": 201},
        ]);
        let out = normalize_endpoints(Some(&raw));
        assert_eq!(out.len(), 2, "restano solo le voci provabili: {out:?}");
        assert_eq!(out[0]["method"], json!("GET"), "il metodo si normalizza maiuscolo");
        assert_eq!(out[1]["method"], json!("POST"));
        assert_eq!(out[1]["status"], json!(201));
    }

    #[test]
    fn criteri_dichiarati_usano_lo_status_e_mai_il_corpo() {
        let declared = json!({
            "outcome": "done",
            "endpoints": normalize_endpoints(Some(&json!([
                {"method": "GET", "url": "http://localhost:3000/api/expenses"},
                {"method": "POST", "url": "http://localhost:3000/api/expenses",
                 "body": {"amount": 12.5, "description": "prova gate"}},
            ]))),
        });
        let crits = endpoint_criteria_from_declaration(Some(&declared), 15.0);
        assert_eq!(crits.len(), 2);
        assert!(crits.iter().all(|c| c.criterion_type == "http"));
        // Nessun body_contains: la decisione e' lo status (regola M).
        assert!(crits.iter().all(|c| c.expected.get("body_contains").is_none()));
        // Senza status dichiarato: la famiglia 2xx. Un 500 (l'incidente) e' fuori.
        assert_eq!(crits[1].expected["status"], json!(DEFAULT_SUCCESS_STATUSES));
        assert_eq!(crits[1].spec["method"], json!("POST"));
        assert_eq!(crits[1].spec["body"]["amount"], json!(12.5));
        assert_eq!(crits[1].timeout_s, Some(15.0));
    }

    #[test]
    fn nessuna_dichiarazione_nessun_criterio() {
        assert!(endpoint_criteria_from_declaration(None, 15.0).is_empty());
        let solo_outcome = json!({"outcome": "done", "summary": "fatto"});
        assert!(endpoint_criteria_from_declaration(Some(&solo_outcome), 15.0).is_empty());
    }
}
