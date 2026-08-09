//! Punto unico (regola L) di «il browser riesce a parlare col backend?».
//!
//! Gemello di [`super::endpoint_probes`] e deliberatamente DISTINTO da esso:
//! quello risponde «il server risponde?» con una richiesta lato server, questo
//! «cio' che la pagina chiede davvero arriva a destinazione?». Sono due
//! domande, e la prima non implica la seconda — e' precisamente su quella
//! implicazione mancante che l'incidente e' passato quattro volte.
//!
//! MISURATO il 06/08/2026 su biblioteca-scolastica: `curl :35976/health` ->
//! 200 e `curl :35976/api/books` -> 200 (il backend funzionava davvero),
//! mentre nel browser OGNI chiamata falliva. Due cause invisibili a una
//! probe lato server, per COSTRUZIONE e non per svista:
//!   - CORS: reqwest non manda `Origin` e non applica la same-origin policy,
//!     quindi un backend senza header CORS gli risponde 200 e viene bloccato
//!     solo dal browser;
//!   - `/api/api/books`: l'URL provato e' quello DICHIARATO dall'agente, non
//!     quello che il codice client costruisce a runtime — nessun JS eseguito,
//!     nessun modo di accorgersene.
//! Aggiungere un header `Origin` a reqwest non chiuderebbe il buco: manca il
//! motore che APPLICA la policy, non l'intestazione. Serve un browser vero.
//!
//! AGNOSTICO ALLO STACK per costruzione: qui si guarda il SINTOMO osservabile
//! (richieste fallite, errori di console), mai il meccanismo che lo produce
//! (proxy Vite, CORS, rewrite di Next, middleware .NET). Inseguire le
//! architetture a codice sarebbe la toppa che la regola H vieta: la stessa
//! misura vale identica su React+Vite, Next.js, .NET o Django, perche' una
//! pagina che non ottiene i propri dati si vede allo stesso modo ovunque.
//!
//! CONFINE (regola L): qui SOLO il criterio puro sui fatti gia' raccolti.
//! L'I/O — avviare Chromium, caricare la pagina, registrare console e rete —
//! sta in `mcp-core` (`agent_tools::browser_probe`), che porta i fatti e non
//! li giudica.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// UNA richiesta osservata dalla pagina. I campi sono quelli che il browser
/// dichiara (regola M/Q): `status` assente = la richiesta non ha mai ricevuto
/// risposta (rete fallita, CORS, DNS), che e' un fatto diverso da uno status
/// di errore e va detto come tale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RichiestaOsservata {
    pub url: String,
    /// `None` = nessuna risposta ricevuta.
    pub status: Option<u16>,
    /// Il motivo dichiarato dal browser quando la richiesta e' fallita senza
    /// risposta (`net::ERR_FAILED`, blocco CORS, ...). Vuoto se non dichiarato.
    #[serde(default)]
    pub errore: String,
}

/// I fatti raccolti da UN caricamento di pagina. Nessun giudizio: le soglie e
/// il vocabolario arrivano al criterio come parametri (regola G).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProveBrowser {
    /// Richieste di rete partite dalla pagina, nell'ordine osservato.
    #[serde(default)]
    pub richieste: Vec<RichiestaOsservata>,
    /// Messaggi di console di livello errore (inclusi gli errori non gestiti).
    #[serde(default)]
    pub errori_console: Vec<String>,
    /// La pagina si e' caricata? `false` = il browser non e' arrivato a
    /// eseguirla (servizio spento, navigazione fallita).
    #[serde(default)]
    pub pagina_caricata: bool,
}

/// Cosa impedisce alla pagina di ottenere i propri dati. Vocabolario CHIUSO
/// (regola N) e CAUSA insieme al verdetto: un rilievo che non dice quale URL e
/// quale errore manda l'agente a cercare alla cieca.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CausaDialogo {
    /// La richiesta non ha ricevuto risposta: rete fallita, CORS, host errato.
    SenzaRisposta { url: String, errore: String },
    /// Risposta ricevuta ma di errore: 404 (percorso sbagliato), 500, ...
    StatusDiErrore { url: String, status: u16 },
}

impl CausaDialogo {
    /// La riga che l'agente legge. Composta DAI campi (regola Q punto 3).
    pub fn descrizione(&self) -> String {
        match self {
            Self::SenzaRisposta { url, errore } => {
                let motivo = if errore.trim().is_empty() {
                    "nessuna risposta ricevuta".to_string()
                } else {
                    errore.trim().to_string()
                };
                format!("{url} -> {motivo}")
            }
            Self::StatusDiErrore { url, status } => format!("{url} -> HTTP {status}"),
        }
    }
}

/// L'esito della misura. `NonConcludente` NON e' un dettaglio: e' cio' che
/// impedisce a «non ho potuto guardare» di diventare «va tutto bene», ed e' la
/// lezione dell'unica lente gia' esistente in questo repo che nasce corretta e
/// nessun gate interroga (vedi nota in fondo al modulo).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdettoDialogo {
    /// La pagina ha ottenuto i propri dati: nessuna richiesta fallita.
    Dialoga { richieste_osservate: usize },
    /// Almeno una richiesta della pagina non e' arrivata a destinazione.
    Rotto { cause: Vec<CausaDialogo> },
    /// La misura non e' stata possibile, col motivo dichiarato.
    NonConcludente { motivo: String },
}

/// Soglia minima di richieste osservate sotto la quale non si dichiara nulla:
/// una pagina che non ha ancora chiesto niente non e' una pagina rotta. Sotto
/// soglia e senza fallimenti l'esito e' `NonConcludente`, mai `Dialoga` — il
/// silenzio non e' una prova di salute.
pub const MIN_RICHIESTE_OSSERVATE: usize = 1;

/// Il criterio: PURO, testabile senza browser.
///
/// UN FALLIMENTO BASTA. Il successo delle altre richieste non assolve: e'
/// l'errore da cui questo modulo si guarda deliberatamente — una `GET /health`
/// verde mentre ogni `POST` fallisce e' esattamente la forma dell'incidente, e
/// un criterio che facesse l'OR delle prove lo mancherebbe come lo ha mancato
/// la probe lato server.
///
/// `terze_parti` sono i prefissi di URL da ignorare (CDN, telemetria, font):
/// un font che non carica non e' un difetto d'integrazione. Vocabolario dal DB
/// (regola G), mai un elenco cablato qui.
pub fn classifica_dialogo(prove: &ProveBrowser, terze_parti: &[String]) -> VerdettoDialogo {
    if !prove.pagina_caricata {
        return VerdettoDialogo::NonConcludente {
            motivo: "la pagina non si e' caricata: servizio non raggiungibile".to_string(),
        };
    }
    let pertinenti: Vec<&RichiestaOsservata> = prove
        .richieste
        .iter()
        .filter(|r| !e_terza_parte(&r.url, terze_parti))
        .collect();

    let cause: Vec<CausaDialogo> = pertinenti.iter().filter_map(|r| causa_di(r)).collect();
    if !cause.is_empty() {
        return VerdettoDialogo::Rotto { cause };
    }
    if pertinenti.len() < MIN_RICHIESTE_OSSERVATE {
        return VerdettoDialogo::NonConcludente {
            motivo: "nessuna richiesta osservata dalla pagina: non c'e' dialogo da misurare"
                .to_string(),
        };
    }
    VerdettoDialogo::Dialoga {
        richieste_osservate: pertinenti.len(),
    }
}

/// «Questa richiesta osservata e' fallita?»
///
/// PUNTO UNICO del predicato, e non e' una comodita': la stessa domanda se la
/// pone [`super::risorse_pagina`] sulle risorse sub-documento della STESSA
/// osservazione, e due encoding della regola divergerebbero al primo status
/// che uno dei due decidesse di trattare diversamente. La risposta e'
/// letteralmente [`causa_di`], cosi' la regola resta scritta una volta sola.
pub fn richiesta_fallita(r: &RichiestaOsservata) -> bool {
    causa_di(r).is_some()
}

/// La causa di UNA richiesta, se e' fallita. `None` = e' andata a buon fine.
fn causa_di(r: &RichiestaOsservata) -> Option<CausaDialogo> {
    match r.status {
        None => Some(CausaDialogo::SenzaRisposta {
            url: r.url.clone(),
            errore: r.errore.clone(),
        }),
        Some(s) if s >= 400 => Some(CausaDialogo::StatusDiErrore {
            url: r.url.clone(),
            status: s,
        }),
        Some(_) => None,
    }
}

/// L'URL appartiene a un prefisso dichiarato di terze parti?
fn e_terza_parte(url: &str, terze_parti: &[String]) -> bool {
    terze_parti
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .any(|p| url.starts_with(p))
}

/// Il tipo di criterio nel vocabolario del runner (regola N).
pub const CRITERION_TYPE: &str = "browser_dialogue";

/// La spec del criterio, costruita QUI e non dai chiamanti: il produttore del
/// criterio e' uno solo, cosi' i test possono attraversarlo invece di
/// fabbricare la spec a mano (regola O).
///
/// `origine` e' l'URL su cui la pagina va caricata — l'origine del FRONTEND,
/// perche' e' li' che il browser applica la same-origin policy che rende
/// visibili le due cause invisibili a una probe lato server.
/// `terze_parti` e `attesa_ms` viaggiano nella SPEC e non come stato del
/// runner: il vocabolario e' configurazione (regola G) e va risolto a monte
/// dal motore, come gia' fa `docs_globs`. Cosi' il nodo resta puro e il runner
/// non legge il DB.
pub fn criterio_dialogo(
    origine: Option<&str>,
    timeout_s: f64,
    terze_parti: &[String],
    attesa_ms: u64,
) -> Option<crate::runtime::ports::CriterionSpec> {
    use crate::runtime::ports::{CriterionProvenance, CriterionSpec};
    let origine = origine.map(str::trim).filter(|o| !o.is_empty())?;
    let mut spec = Map::new();
    spec.insert("url".to_string(), json!(origine.trim_end_matches('/')));
    spec.insert(CHIAVE_TERZE_PARTI.to_string(), json!(terze_parti));
    spec.insert(CHIAVE_ATTESA_MS.to_string(), json!(attesa_ms));
    Some(CriterionSpec {
        criterion_type: CRITERION_TYPE.to_string(),
        provenance: CriterionProvenance::Gate,
        spec: Value::Object(spec),
        expected: json!({}),
        timeout_s: Some(timeout_s),
    })
}

/// Chiavi della spec, con un solo punto di scrittura (i test le referenziano
/// da qui, mai come letterali sparsi).
pub const CHIAVE_TERZE_PARTI: &str = "third_parties";
pub const CHIAVE_ATTESA_MS: &str = "settle_ms";

// NOTA DI METODO, perche' non si ripeta il difetto che questo modulo evita.
// In questo repo esiste gia' una lente costruita bene — `ui_styling`, che
// accerta se lo stile dichiarato dal codice sia davvero applicato — e che e'
// senza effetto sulla chiusura di un run: e' un tool in whitelist di
// due figure, e nessun nodo del grafo la interroga. Il risultato misurato e'
// che l'app dell'incidente aveva Tailwind dichiarato, installato e mai
// configurato, e nessuno se ne e' accorto. Per questo il criterio qui nasce
// nel dispatch del final_gate accanto a run_command|http|file_exists, non come
// tool offerto a un giudice: una misura che nessun gate interroga si e'
// costruita, non e' entrata in esercizio.

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(url: &str) -> RichiestaOsservata {
        RichiestaOsservata {
            url: url.into(),
            status: Some(200),
            errore: String::new(),
        }
    }

    fn caricata(richieste: Vec<RichiestaOsservata>) -> ProveBrowser {
        ProveBrowser {
            richieste,
            errori_console: Vec::new(),
            pagina_caricata: true,
        }
    }

    /// L'INCIDENTE, nella sua forma esatta: il browser non riceve risposta
    /// (CORS) su un URL col percorso duplicato. Mutazione: far assolvere il
    /// criterio quando almeno una richiesta riesce -> questo test cade.
    #[test]
    fn una_richiesta_fallita_basta_a_dichiarare_il_difetto() {
        let prove = caricata(vec![
            ok("http://localhost:35954/"),
            RichiestaOsservata {
                url: "http://127.0.0.1:35976/api/api/books".into(),
                status: None,
                errore: "net::ERR_FAILED (CORS)".into(),
            },
        ]);
        let v = classifica_dialogo(&prove, &[]);
        let VerdettoDialogo::Rotto { cause } = v else {
            panic!("una richiesta senza risposta rompe il dialogo: {v:?}");
        };
        assert_eq!(cause.len(), 1);
        assert!(
            cause[0].descrizione().contains("/api/api/books")
                && cause[0].descrizione().contains("CORS"),
            "la causa nomina URL e motivo: {}",
            cause[0].descrizione()
        );
    }

    /// Uno status di errore e' una causa DISTINTA dalla mancata risposta: la
    /// prima dice «percorso sbagliato», la seconda «non ci sono arrivato», e
    /// portano a due correzioni diverse.
    #[test]
    fn lo_status_di_errore_e_una_causa_distinta() {
        let prove = caricata(vec![RichiestaOsservata {
            url: "http://localhost:3000/api/statistiche".into(),
            status: Some(404),
            errore: String::new(),
        }]);
        match classifica_dialogo(&prove, &[]) {
            VerdettoDialogo::Rotto { cause } => {
                assert_eq!(
                    cause[0],
                    CausaDialogo::StatusDiErrore {
                        url: "http://localhost:3000/api/statistiche".into(),
                        status: 404
                    }
                );
                assert!(cause[0].descrizione().contains("HTTP 404"));
            }
            altro => panic!("un 404 e' un difetto di dialogo: {altro:?}"),
        }
    }

    /// «Non ho potuto guardare» non diventa «va bene»: pagina non caricata e
    /// nessuna richiesta osservata sono due NonConcludente con motivi diversi.
    /// Mutazione: far ritornare `Dialoga` su zero richieste -> cade qui.
    #[test]
    fn l_ignoto_non_degrada_a_successo() {
        let spenta = ProveBrowser {
            pagina_caricata: false,
            ..Default::default()
        };
        assert!(matches!(
            classifica_dialogo(&spenta, &[]),
            VerdettoDialogo::NonConcludente { .. }
        ));

        let muta = caricata(Vec::new());
        let VerdettoDialogo::NonConcludente { motivo } = classifica_dialogo(&muta, &[]) else {
            panic!("zero richieste non e' una prova di salute");
        };
        assert!(motivo.contains("nessuna richiesta"), "{motivo}");
    }

    /// Un font o uno script di terze parti che non carica non e' un difetto
    /// d'integrazione del progetto: il vocabolario arriva dal DB.
    #[test]
    fn le_terze_parti_non_fanno_fallire_il_progetto() {
        let prove = caricata(vec![
            ok("http://localhost:35954/api/books"),
            RichiestaOsservata {
                url: "https://fonts.googleapis.com/css2".into(),
                status: None,
                errore: "net::ERR_INTERNET_DISCONNECTED".into(),
            },
        ]);
        let terze = vec!["https://fonts.googleapis.com".to_string()];
        assert_eq!(
            classifica_dialogo(&prove, &terze),
            VerdettoDialogo::Dialoga {
                richieste_osservate: 1
            }
        );
        // Senza il vocabolario, la stessa prova e' un difetto: e' il
        // vocabolario a decidere, non il codice.
        assert!(matches!(
            classifica_dialogo(&prove, &[]),
            VerdettoDialogo::Rotto { .. }
        ));
    }

    /// Il criterio si costruisce dal produttore unico, e senza origine non
    /// nasce affatto (un progetto senza frontend non ha questo criterio).
    #[test]
    fn il_criterio_nasce_solo_con_un_origine() {
        let terze = vec!["https://cdn.example".to_string()];
        assert!(criterio_dialogo(None, 30.0, &terze, 2000).is_none());
        assert!(criterio_dialogo(Some("   "), 30.0, &terze, 2000).is_none());
        let c = criterio_dialogo(Some("http://localhost:35954/"), 30.0, &terze, 2000)
            .expect("criterio");
        assert_eq!(c.criterion_type, CRITERION_TYPE);
        assert_eq!(c.spec["url"], "http://localhost:35954");
        // Il vocabolario viaggia nella spec: il runner non legge il DB.
        assert_eq!(c.spec[CHIAVE_TERZE_PARTI][0], "https://cdn.example");
        assert_eq!(c.spec[CHIAVE_ATTESA_MS], 2000);
    }
}
