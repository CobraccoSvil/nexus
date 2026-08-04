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

use crate::runtime::ports::{CriterionProvenance, CriterionSpec};

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

/// Prova gli stessi endpoint dichiarati ATTRAVERSO l'origine del frontend, per
/// accertare che i due servizi si parlino.
///
/// ROOT CAUSE, misurata il 04/08/2026 su inventario-magazzino. Backend corretto
/// (`GET /api/articles` -> 200, e il vincolo di business rispettato), frontend
/// servito e compilato (home -> 200): il gate ha chiuso verde. Ma
/// `vite.config.ts` portava
/// `proxy: {'/api': {target, rewrite: p => p.replace(/^\/api/, '')}}` mentre il
/// backend espone proprio `/api/articles` — il proxy inoltrava a
/// `localhost:36376/articles`, che non esiste. Misurato:
///
/// | chiamata                          | esito |
/// |-----------------------------------|-------|
/// | backend `/api/articles`           | 200   |
/// | backend `/articles`               | 404   |
/// | via proxy frontend `/api/articles`| 404   |
///
/// Pagina vuota, nessun errore in console. Le due meta' erano sane e
/// l'applicazione non esisteva: nessuno verificava l'unica cosa che le rende un
/// insieme. E' la stessa forma di difetto che il modulo gia' chiude — si misura
/// cio' che e' comodo misurare (il pezzo risponde) invece di cio' che conta (il
/// pezzo fa il suo lavoro).
///
/// Che cosa si prova, e perche' solo quello:
/// - **solo i GET**: una POST rieseguita attraverso il frontend creerebbe un
///   secondo record, e il costo non compra nulla — un proxy rotto lo e' per
///   qualunque metodo;
/// - **solo il PATH**, riattaccato all'origine del frontend: e' esattamente
///   l'URL che il browser chiamerebbe;
/// - **mai la root** (`/`): sul frontend e' la sua pagina, e risponderebbe 200
///   dicendo nulla sull'integrazione.
///
/// Il fallback della SPA e' coperto da `reject_html`, e non passa dal corpo: un
/// endpoint di API che risponde `Content-Type: text/html` ha servito la pagina
/// del frontend, non i dati.
///
/// Era il limite dichiarato di questo criterio quando fu scritto, e si e'
/// materializzato al primo giro (biblioteca-scolastica, 04/08/2026): il
/// `rewrite` del proxy toglieva `/api`, il backend rispondeva 404 e Vite
/// ripiegava su `index.html` con **status 200**. Deciso sul solo status, il
/// gate approvava un'applicazione le cui due meta' non si parlavano. Vedi
/// `criteria_runner::risposta_e_html`.
pub fn criteri_integrazione_frontend(
    declared_outcome: Option<&Value>,
    origine_frontend: Option<&str>,
    timeout_s: f64,
) -> Vec<CriterionSpec> {
    let Some(origine) = origine_frontend.map(str::trim).filter(|o| !o.is_empty()) else {
        return Vec::new();
    };
    let origine = origine.trim_end_matches('/');
    declared_outcome
        .and_then(|d| d.get("endpoints"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|e| criterio_integrazione(e, origine, timeout_s))
        .collect()
}

/// Il criterio d'integrazione di UNA voce dichiarata, se ha senso provarla.
fn criterio_integrazione(
    endpoint: &Value,
    origine_frontend: &str,
    timeout_s: f64,
) -> Option<CriterionSpec> {
    let obj = endpoint.as_object()?;
    let metodo = obj.get("method").and_then(Value::as_str).unwrap_or("GET");
    if !metodo.eq_ignore_ascii_case("GET") {
        return None;
    }
    let percorso = percorso_di(obj.get("url").and_then(Value::as_str)?);
    if percorso.is_empty() || percorso == "/" {
        return None;
    }
    let mut spec = Map::new();
    spec.insert("url".to_string(), json!(format!("{origine_frontend}{percorso}")));
    spec.insert("method".to_string(), json!("GET"));
    // Provenienza distinta dalle altre due: nell'evidence si deve vedere che il
    // rosso viene dall'INTEGRAZIONE, non dal backend — altrimenti la diagnosi
    // manda a cercare il difetto nel servizio sbagliato.
    spec.insert("source".to_string(), json!("frontend_integration"));
    Some(CriterionSpec {
        criterion_type: "http".to_string(),
        provenance: CriterionProvenance::Gate,
        spec: Value::Object(spec),
        // `reject_html`: attraverso il frontend, un endpoint di API deve
        // restituire il DATO del backend. Se risponde HTML ha servito la propria
        // pagina — il proxy non raggiunge l'API e il fallback della SPA maschera
        // il 404 con un 200. Vale solo per questi criteri: sull'origine del
        // BACKEND una risposta HTML puo' essere legittima.
        expected: json!({
            "status": DEFAULT_SUCCESS_STATUSES.to_vec(),
            "reject_html": true,
        }),
        timeout_s: Some(timeout_s),
    })
}

/// Percorso (con query) di un URL assoluto o gia' relativo.
///
/// Scomposizione a mano invece di una dipendenza da un parser di URL: qui serve
/// solo separare `schema://host:porta` dal resto, e l'origine del frontend la
/// fornisce il chiamante.
fn percorso_di(url: &str) -> &str {
    let url = url.trim();
    match url.find("://") {
        Some(i) => match url[i + 3..].find('/') {
            Some(j) => &url[i + 3 + j..],
            // `http://host:porta` senza percorso: e' la root.
            None => "/",
        },
        None if url.starts_with('/') => url,
        None => "",
    }
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
        provenance: CriterionProvenance::Gate,
        spec: Value::Object(spec),
        expected: json!({ "status": statuses }),
        timeout_s: Some(timeout_s),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Costruisce la dichiarazione come la produce la PORTA della produzione
    /// (`normalize_endpoints`), non a mano: un test che fabbrica l'input gia'
    /// prodotto altrove fissa l'assunto invece di verificarlo (regola O).
    fn dichiarazione(voci: Value) -> Value {
        json!({ "outcome": "done", "endpoints": normalize_endpoints(Some(&voci)) })
    }

    /// IL CASO MISURATO il 04/08/2026 su inventario-magazzino: backend sano,
    /// frontend servito, proxy che riscrive `/api` via -> 404, gate VERDE.
    ///
    /// MUTAZIONE: far ritornare `Vec::new()` a `criteri_integrazione_frontend`
    /// fa rosseggiare la prima asserzione — nessuna prova attraverso il
    /// frontend, cioe' esattamente il gate che ha lasciato passare due servizi
    /// vivi che non si parlavano.
    #[test]
    fn gli_endpoint_si_provano_anche_attraverso_il_frontend() {
        let declared = dichiarazione(json!([
            {"method": "GET", "url": "http://127.0.0.1:36376/api/articles"},
        ]));
        let out = criteri_integrazione_frontend(
            Some(&declared),
            Some("http://127.0.0.1:36354"),
            15.0,
        );
        assert_eq!(out.len(), 1, "l'integrazione va provata: {out:?}");
        assert_eq!(
            out[0].spec["url"],
            json!("http://127.0.0.1:36354/api/articles"),
            "stesso PERCORSO, origine del frontend"
        );
        assert_eq!(out[0].spec["source"], json!("frontend_integration"),
            "la provenienza distingue il rosso d'integrazione da quello del backend");
    }

    #[test]
    fn senza_frontend_non_si_prova_niente() {
        let declared = dichiarazione(json!([
            {"method": "GET", "url": "http://127.0.0.1:36376/api/articles"},
        ]));
        // Nessun servizio frontend, o porta non risolvibile: mai un host indovinato.
        assert!(criteri_integrazione_frontend(Some(&declared), None, 15.0).is_empty());
        assert!(criteri_integrazione_frontend(Some(&declared), Some("  "), 15.0).is_empty());
    }

    #[test]
    fn attraverso_il_frontend_solo_le_letture_e_mai_la_root() {
        let declared = dichiarazione(json!([
            // Una POST rieseguita creerebbe un secondo record, e un proxy rotto
            // lo e' per qualunque metodo.
            {"method": "POST", "url": "http://127.0.0.1:36376/api/articles",
             "body": {"code": "X"}, "status": 201},
            // La root del frontend e' la sua pagina: 200 che non dice nulla.
            {"method": "GET", "url": "http://127.0.0.1:36376/"},
            {"method": "GET", "url": "http://127.0.0.1:36376/api/health"},
        ]));
        let out = criteri_integrazione_frontend(
            Some(&declared),
            Some("http://127.0.0.1:36354/"),
            15.0,
        );
        assert_eq!(out.len(), 1, "solo la GET non-root: {out:?}");
        assert_eq!(
            out[0].spec["url"],
            json!("http://127.0.0.1:36354/api/health"),
            "l'origine si normalizza senza doppia barra"
        );
    }

    #[test]
    fn il_percorso_si_estrae_da_url_assoluti_e_relativi() {
        assert_eq!(percorso_di("http://h:1/api/x?y=1"), "/api/x?y=1");
        assert_eq!(percorso_di("https://h/api/x"), "/api/x");
        // Origine nuda: e' la root, e la root non si prova.
        assert_eq!(percorso_di("http://127.0.0.1:36376"), "/");
        assert_eq!(percorso_di("/api/x"), "/api/x");
        // Ne' assoluto ne' relativo: non se ne ricava un percorso.
        assert_eq!(percorso_di("api/x"), "");
    }

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
