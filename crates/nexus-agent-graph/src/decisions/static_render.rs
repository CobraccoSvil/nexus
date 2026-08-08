//! Punto unico (regola L) di «l'app senza server MOSTRA davvero il suo
//! contenuto?».
//!
//! Terzo di una famiglia, e distinto dai due precedenti perche' risponde a una
//! domanda che nessuno dei due puo' porre:
//!   - [`super::endpoint_probes`] chiede al SERVER se risponde;
//!   - [`super::browser_dialogue`] chiede alla PAGINA se ottiene i propri dati
//!     — ed e' costruito attorno a un'origine HTTP, cioe' a un servizio;
//!   - qui non c'e' nessun servizio a cui chiedere. Il contenuto non arriva
//!     dalla rete: lo genera il JavaScript della pagina stessa, e l'unico modo
//!     di sapere se e' arrivato e' guardare il DOM dopo che ha girato.
//!
//! MISURATO l'08/08/2026 su gestione-corsi. `landing/index.html` (11637 byte,
//! generata in autonomia, approvata dal gate al TERZO tentativo) e' corretta:
//! sei card nascono all'avvio da `filterCourses('all')`, in fondo allo script.
//! Il gate non lo sapeva e non poteva saperlo — nessun criterio attivo apriva
//! quel file. Il contenuto di quella pagina NON e' nel suo HTML: una variabile
//! non definita, un id sbagliato o un `throw` prima dell'inizializzazione
//! producono un file di sintassi valida, che supera ogni controllo statico, e
//! una griglia vuota. I due casi sono indistinguibili guardando i byte.
//!
//! I SEGNALI, in ordine di forza, e nessuno indovinato:
//!   1. un'ECCEZIONE non gestita (`pageerror`): il codice della pagina ha
//!      lanciato. E' un fatto, non un'euristica, ed e' esattamente la forma
//!      che assume il difetto descritto sopra;
//!   2. il CONTENITORE dichiarato e' rimasto vuoto: chi dichiara «qui vanno le
//!      card» dichiara anche come si accerta che ci siano;
//!   3. il BODY reso e' sotto la soglia minima: il caso della SPA il cui
//!      bundle non parte, dove `<div id="root"></div>` resta cio' che era.
//!
//! Un `console.error` NON e' fra questi. Una libreria che scrive un avviso non
//! rende la pagina rotta, e bocciare su quel segnale riporterebbe i rimandi a
//! vuoto che la lente dello stile evita apposta: entra nell'evidenza come
//! contesto per chi legge, mai nel verdetto.
//!
//! MISURATO col criterio in esercizio sulla pagina reale e su una sua copia col
//! solo `throw` aggiunto prima di `filterCourses('all')`:
//!
//! | pagina  | elementi | `#courses-grid` | eccezioni | verdetto |
//! |---------|----------|-----------------|-----------|----------|
//! | reale   | 100      | 6 figli         | nessuna   | `Resa`   |
//! | mutata  | 28       | 0 figli         | 1         | `NonResa` (2 cause) |
//!
//! Tre cose che quei numeri dicono e che valeva la pena sapere prima di
//! fidarsi. L'eccezione BASTA da sola: sulla mutata senza contenitore
//! dichiarato il verdetto resta `NonResa`, quindi il criterio chiude il caso
//! misurato senza chiedere niente all'agente. La soglia sul body NON basta: 28
//! elementi restano sopra qualunque minimo ragionevole, e da sola quella pagina
//! sarebbe passata — e' la ragione per cui il contenitore dichiarato esiste,
//! per il difetto che NON lancia (un id sbagliato letto in un `if`). E il 404
//! di console presente su ENTRAMBE non ha spostato nulla, che e' esattamente
//! cio' che deve fare.
//!
//! CONFINE (regola L): qui SOLO il criterio puro sui fatti gia' raccolti.
//! L'I/O — avviare Chromium, caricare la pagina, contare gli elementi — sta in
//! `mcp-core` (`agent_tools::browser_probe`), che porta i fatti e non li
//! giudica. Stesso taglio di [`super::browser_dialogue`].

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// Cosa e' successo al contenitore dichiarato. Tre stati e non un `usize`:
/// «non l'ho trovato» e «l'ho trovato vuoto» mandano a due correzioni diverse
/// (un id sbagliato contro una generazione che non e' partita), e collassarli
/// su zero figli direbbe la seconda quando e' vera la prima.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EsitoContenitore {
    /// Il selettore non corrisponde ad alcun elemento della pagina.
    Assente,
    /// Trovato, con questo numero di figli elemento.
    Trovato { figli: usize },
}

/// I fatti raccolti da UN caricamento di pagina. Nessun giudizio: la soglia
/// arriva al criterio come parametro (regola G).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProveResa {
    /// La pagina si e' caricata? `false` = il browser non e' arrivato a
    /// eseguirla (file assente, route di preview muta).
    #[serde(default)]
    pub pagina_caricata: bool,
    /// Elementi di contenuto nel `body` DOPO l'esecuzione del JS. `None` = non
    /// contati: la misura non e' riuscita, e non e' uno zero.
    #[serde(default)]
    pub elementi_resi: Option<usize>,
    /// Il contenitore dichiarato, quando c'e'. `None` = nessuna dichiarazione,
    /// non «contenitore assente».
    #[serde(default)]
    pub contenitore: Option<EsitoContenitore>,
    /// Eccezioni non gestite lanciate dalla pagina, nell'ordine osservato.
    #[serde(default)]
    pub errori_esecuzione: Vec<String>,
    /// Messaggi di console di livello errore. INFORMATIVI: entrano
    /// nell'evidenza, mai nel verdetto (vedi doc del modulo).
    #[serde(default)]
    pub errori_console: Vec<String>,
}

/// Cosa impedisce alla pagina di mostrare il proprio contenuto. Vocabolario
/// CHIUSO (regola N) e CAUSA insieme al verdetto: un rilievo che non dice cosa
/// e' rimasto vuoto manda l'agente a cercare alla cieca.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CausaNonResa {
    /// Il codice della pagina ha lanciato: tutto cio' che seguiva non e' girato.
    EsecuzioneInterrotta { messaggio: String },
    /// Il selettore dichiarato non esiste nella pagina.
    ContenitoreAssente { selettore: String },
    /// Il contenitore c'e' ed e' rimasto sotto il minimo dichiarato.
    ContenitoreVuoto {
        selettore: String,
        trovati: usize,
        attesi: usize,
    },
    /// Il `body` reso non contiene abbastanza elementi per essere una pagina.
    PaginaVuota { elementi: usize, minimo: usize },
}

impl CausaNonResa {
    /// La riga che l'agente legge. Composta DAI campi (regola Q punto 3).
    pub fn descrizione(&self) -> String {
        match self {
            Self::EsecuzioneInterrotta { messaggio } => {
                let m = messaggio.trim();
                let m = if m.is_empty() {
                    "eccezione non gestita"
                } else {
                    m
                };
                format!("il JavaScript della pagina ha lanciato: {m}")
            }
            Self::ContenitoreAssente { selettore } => {
                format!("il contenitore '{selettore}' non esiste nella pagina resa")
            }
            Self::ContenitoreVuoto {
                selettore,
                trovati,
                attesi,
            } => format!("il contenitore '{selettore}' ha {trovati} elementi, attesi almeno {attesi}"),
            Self::PaginaVuota { elementi, minimo } => format!(
                "la pagina resa ha {elementi} elementi, sotto il minimo di {minimo}: \
                 nulla e' stato mostrato"
            ),
        }
    }
}

/// L'esito della misura. `NonConcludente` NON e' un dettaglio: e' cio' che
/// impedisce a «non ho potuto guardare» di diventare «va tutto bene».
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdettoResa {
    /// La pagina mostra il proprio contenuto.
    Resa { elementi: usize },
    /// La pagina non mostra cio' che dovrebbe, con le cause.
    NonResa { cause: Vec<CausaNonResa> },
    /// La misura non e' stata possibile, col motivo dichiarato.
    NonConcludente { motivo: String },
}

/// Minimo di figli che un contenitore DICHIARATO deve avere per dirsi popolato.
/// Uno: la domanda e' «e' stato generato qualcosa?», non «quanto».
pub const MIN_FIGLI_CONTENITORE: usize = 1;

/// Il criterio: PURO, testabile senza browser.
///
/// UN FALLIMENTO BASTA, e le cause si raccolgono TUTTE: una pagina che lancia
/// E resta vuota deve dirlo in una volta sola, o il ciclo di correzione fa due
/// giri per due sintomi della stessa causa.
///
/// `minimo_elementi` e' la soglia sul body, dal DB (regola G): sotto quel
/// numero non c'e' pagina, sopra non si giudica il merito — questo criterio
/// accerta che qualcosa sia stato RESO, non che sia bello (per quello c'e' la
/// lente dello stile, che e' un'altra domanda e un altro criterio).
pub fn classifica_resa(prove: &ProveResa, minimo_elementi: usize) -> VerdettoResa {
    if !prove.pagina_caricata {
        return VerdettoResa::NonConcludente {
            motivo: "la pagina non si e' caricata: file non raggiungibile".to_string(),
        };
    }
    let Some(elementi) = prove.elementi_resi else {
        return VerdettoResa::NonConcludente {
            motivo: "contenuto della pagina non misurabile: nessun conteggio del DOM".to_string(),
        };
    };

    let mut cause: Vec<CausaNonResa> = prove
        .errori_esecuzione
        .iter()
        .map(|m| CausaNonResa::EsecuzioneInterrotta {
            messaggio: m.clone(),
        })
        .collect();

    cause.extend(causa_contenitore(prove.contenitore.as_ref()));

    if elementi < minimo_elementi {
        cause.push(CausaNonResa::PaginaVuota {
            elementi,
            minimo: minimo_elementi,
        });
    }

    if cause.is_empty() {
        VerdettoResa::Resa { elementi }
    } else {
        VerdettoResa::NonResa { cause }
    }
}

/// La causa che riguarda il contenitore, se ce n'e' una. `None` = nessuna
/// dichiarazione, oppure il contenitore e' popolato.
///
/// Gemella di `causa_di` in [`super::browser_dialogue`]: un fatto per volta, e
/// il criterio che le raccoglie resta leggibile. Il selettore NON e' qui —
/// lo porta la spec, e lo innesta [`cause_con_selettore`].
fn causa_contenitore(contenitore: Option<&EsitoContenitore>) -> Option<CausaNonResa> {
    match contenitore? {
        EsitoContenitore::Assente => Some(CausaNonResa::ContenitoreAssente {
            selettore: String::new(),
        }),
        EsitoContenitore::Trovato { figli } if *figli < MIN_FIGLI_CONTENITORE => {
            Some(CausaNonResa::ContenitoreVuoto {
                selettore: String::new(),
                trovati: *figli,
                attesi: MIN_FIGLI_CONTENITORE,
            })
        }
        EsitoContenitore::Trovato { .. } => None,
    }
}

/// Nomina il contenitore nelle cause che lo riguardano.
///
/// Esiste perche' i FATTI non portano il selettore — lo porta la spec — e un
/// rilievo che dice «il contenitore e' vuoto» senza dire QUALE e' inutile a chi
/// deve correggerlo. Separata da [`classifica_resa`] per non far dipendere il
/// criterio da un parametro che non usa per decidere.
pub fn cause_con_selettore(verdetto: VerdettoResa, selettore: &str) -> VerdettoResa {
    let VerdettoResa::NonResa { cause } = verdetto else {
        return verdetto;
    };
    let cause = cause
        .into_iter()
        .map(|c| match c {
            CausaNonResa::ContenitoreAssente { .. } => CausaNonResa::ContenitoreAssente {
                selettore: selettore.to_string(),
            },
            CausaNonResa::ContenitoreVuoto {
                trovati, attesi, ..
            } => CausaNonResa::ContenitoreVuoto {
                selettore: selettore.to_string(),
                trovati,
                attesi,
            },
            altro => altro,
        })
        .collect();
    VerdettoResa::NonResa { cause }
}

/// Che TIPO di applicazione e' questa, per decidere QUALE criterio la misura.
///
/// La distinzione e' DICHIARATA dai fatti del progetto, mai indovinata dal
/// testo del task o dal nome dei file: un progetto ha un servizio con una porta
/// allocata, oppure ha una pagina che si apre da sola, oppure non ha
/// interfaccia. Sono tre stati osservabili e si escludono a vicenda.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NaturaApp {
    /// C'e' un servizio frontend con un'origine: la misura giusta e' il
    /// DIALOGO ([`super::browser_dialogue`]), che vede anche le chiamate dati.
    ConServizio,
    /// Nessun servizio, ma una pagina servibile: e' qui che nasce questo
    /// criterio. `entry` e' il percorso RELATIVO alla radice del progetto.
    Statica { entry: String },
    /// Nessun servizio e nessuna pagina: non c'e' interfaccia da guardare.
    SenzaPagina,
}

/// Il discriminante, PURO: i due fatti entrano gia' raccolti.
///
/// Il SERVIZIO ha la precedenza sulla pagina, e non per gerarchia: un progetto
/// che serve il proprio frontend puo' benissimo avere anche un `index.html` in
/// una sottocartella (un mockup, una landing di prova), e misurarlo come app
/// statica guarderebbe un file che non e' cio' che il progetto espone. Dove
/// c'e' un'origine, la domanda completa la pone gia' il dialogo.
pub fn classifica_natura(origine_servizio: Option<&str>, entry: Option<&str>) -> NaturaApp {
    if origine_servizio
        .map(str::trim)
        .is_some_and(|o| !o.is_empty())
    {
        return NaturaApp::ConServizio;
    }
    match entry.map(str::trim).filter(|e| !e.is_empty()) {
        Some(e) => NaturaApp::Statica {
            entry: e.to_string(),
        },
        None => NaturaApp::SenzaPagina,
    }
}

/// Il tipo di criterio nel vocabolario del runner (regola N).
pub const CRITERION_TYPE: &str = "static_render";

/// Chiavi della spec, con un solo punto di scrittura (i test le referenziano da
/// qui, mai come letterali sparsi).
pub const CHIAVE_CONTENITORE: &str = "container_selector";
pub const CHIAVE_MIN_ELEMENTI: &str = "min_elements";
pub const CHIAVE_ATTESA_MS: &str = "settle_ms";

/// La spec del criterio, costruita QUI e non dai chiamanti: il produttore del
/// criterio e' uno solo, cosi' i test possono attraversarlo invece di
/// fabbricare la spec a mano (regola O).
///
/// `url` e' l'indirizzo su cui la pagina va aperta. Non nasce senza: una
/// pagina che non si sa dove aprire non e' misurabile, e un criterio che
/// fallisse per questo boccerebbe il progetto per un difetto della misura.
pub fn criterio_resa(
    url: Option<&str>,
    contenitore: Option<&str>,
    minimo_elementi: usize,
    timeout_s: f64,
    attesa_ms: u64,
) -> Option<crate::runtime::ports::CriterionSpec> {
    use crate::runtime::ports::{CriterionProvenance, CriterionSpec};
    let url = url.map(str::trim).filter(|u| !u.is_empty())?;
    let mut spec = Map::new();
    spec.insert("url".to_string(), json!(url));
    spec.insert(CHIAVE_MIN_ELEMENTI.to_string(), json!(minimo_elementi));
    spec.insert(CHIAVE_ATTESA_MS.to_string(), json!(attesa_ms));
    if let Some(c) = contenitore.map(str::trim).filter(|c| !c.is_empty()) {
        spec.insert(CHIAVE_CONTENITORE.to_string(), json!(c));
    }
    Some(CriterionSpec {
        criterion_type: CRITERION_TYPE.to_string(),
        provenance: CriterionProvenance::Gate,
        spec: Value::Object(spec),
        expected: json!({}),
        timeout_s: Some(timeout_s),
    })
}

/// Il contenitore DICHIARATO dall'agente in `task_complete.rendered_container`
/// (ADR 0034), quando c'e'.
///
/// Perche' DICHIARATO e non dedotto: il contenitore che il JS popola non e'
/// riconoscibile dall'HTML — un `<div>` vuoto puo' essere una griglia mai
/// riempita o una finestra modale che si apre al click, e sono lo stesso
/// markup. Indovinare sceglierebbe a caso fra un difetto e un falso rosso.
/// Chi ha scritto quel codice sa quale sia, e lo dice.
pub fn contenitore_dichiarato(declared_outcome: Option<&Value>) -> Option<String> {
    declared_outcome
        .and_then(|d| d.get("rendered_container"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Innesta il contenitore dichiarato in un criterio gia' costruito.
///
/// Le due parti nascono in posti diversi per necessita': l'URL lo risolve chi
/// conosce la radice del progetto (fuori dal grafo), la dichiarazione la
/// conosce solo il nodo che vede lo stato del run. Passa da qui perche' la
/// chiave della spec resti scritta in un posto solo.
pub fn con_contenitore(
    criterio: crate::runtime::ports::CriterionSpec,
    declared_outcome: Option<&Value>,
) -> crate::runtime::ports::CriterionSpec {
    let Some(sel) = contenitore_dichiarato(declared_outcome) else {
        return criterio;
    };
    let mut criterio = criterio;
    if let Value::Object(map) = &mut criterio.spec {
        map.insert(CHIAVE_CONTENITORE.to_string(), json!(sel));
    }
    criterio
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resa(elementi: usize) -> ProveResa {
        ProveResa {
            pagina_caricata: true,
            elementi_resi: Some(elementi),
            ..Default::default()
        }
    }

    /// L'INCIDENTE, nella sua forma esatta: la pagina esiste, il file e'
    /// valido, e il JS che genera il contenuto ha lanciato prima di girare.
    ///
    /// MUTAZIONE: e' il caso del `throw` prima di `filterCourses('all')` sulla
    /// landing di gestione-corsi. Se si toglie l'eccezione dalle cause — o si
    /// degrada a `console.error`, che il criterio ignora apposta — questo test
    /// torna `Resa` ed e' esattamente il verde che il gate dava all'08/08/2026.
    #[test]
    fn un_eccezione_non_gestita_e_un_difetto() {
        let prove = ProveResa {
            errori_esecuzione: vec!["ReferenceError: courses is not defined".into()],
            ..resa(48)
        };
        let VerdettoResa::NonResa { cause } = classifica_resa(&prove, 5) else {
            panic!("una pagina che lancia non ha reso il proprio contenuto");
        };
        assert_eq!(cause.len(), 1);
        assert!(
            cause[0].descrizione().contains("courses is not defined"),
            "la causa nomina l'errore: {}",
            cause[0].descrizione()
        );
    }

    /// Il body pieno NON assolve: la pagina dell'incidente aveva header, filtri
    /// e footer, e la sola griglia vuota. Il contenitore dichiarato e' il
    /// segnale che distingue «la pagina c'e'» da «la pagina mostra i dati», e
    /// il rilievo lo NOMINA.
    #[test]
    fn il_contenitore_vuoto_boccia_una_pagina_altrimenti_piena() {
        let prove = ProveResa {
            contenitore: Some(EsitoContenitore::Trovato { figli: 0 }),
            ..resa(48)
        };
        let v = cause_con_selettore(classifica_resa(&prove, 5), "#courses-grid");
        let VerdettoResa::NonResa { cause } = v else {
            panic!("un contenitore vuoto e' il difetto che questo criterio esiste per vedere");
        };
        assert_eq!(
            cause[0],
            CausaNonResa::ContenitoreVuoto {
                selettore: "#courses-grid".into(),
                trovati: 0,
                attesi: MIN_FIGLI_CONTENITORE,
            }
        );
        assert!(cause[0].descrizione().contains("#courses-grid"));

        // Popolato, stessa pagina: nessun rilievo.
        let ok = ProveResa {
            contenitore: Some(EsitoContenitore::Trovato { figli: 6 }),
            ..resa(48)
        };
        assert_eq!(
            classifica_resa(&ok, 5),
            VerdettoResa::Resa { elementi: 48 }
        );
    }

    /// Contenitore ASSENTE e contenitore VUOTO sono due cause distinte: la
    /// prima dice «il selettore e' sbagliato», la seconda «la generazione non
    /// e' partita». Collassarle manderebbe a correggere la cosa sbagliata.
    #[test]
    fn assente_e_vuoto_non_sono_la_stessa_causa() {
        let prove = ProveResa {
            contenitore: Some(EsitoContenitore::Assente),
            ..resa(48)
        };
        let v = cause_con_selettore(classifica_resa(&prove, 5), "#griglia");
        let VerdettoResa::NonResa { cause } = v else {
            panic!("un selettore che non esiste e' un difetto");
        };
        assert_eq!(
            cause[0],
            CausaNonResa::ContenitoreAssente {
                selettore: "#griglia".into()
            }
        );
    }

    /// Il caso della SPA il cui bundle non parte: `<div id="root"></div>` resta
    /// cio' che era, senza lanciare nulla di osservabile.
    #[test]
    fn una_pagina_quasi_vuota_non_ha_reso_niente() {
        let VerdettoResa::NonResa { cause } = classifica_resa(&resa(2), 5) else {
            panic!("due elementi non sono una pagina resa");
        };
        assert_eq!(
            cause[0],
            CausaNonResa::PaginaVuota {
                elementi: 2,
                minimo: 5
            }
        );
    }

    /// «Non ho potuto guardare» non diventa «va bene». Due NonConcludente con
    /// motivi diversi: la pagina non caricata e il conteggio mancante.
    /// MUTAZIONE: far ritornare `Resa` su `elementi_resi: None` -> cade qui.
    #[test]
    fn l_ignoto_non_degrada_a_successo() {
        let spenta = ProveResa {
            pagina_caricata: false,
            ..Default::default()
        };
        assert!(matches!(
            classifica_resa(&spenta, 5),
            VerdettoResa::NonConcludente { .. }
        ));

        let non_contata = ProveResa {
            pagina_caricata: true,
            elementi_resi: None,
            ..Default::default()
        };
        let VerdettoResa::NonConcludente { motivo } = classifica_resa(&non_contata, 5) else {
            panic!("un conteggio mancante non e' uno zero e non e' un successo");
        };
        assert!(motivo.contains("non misurabile"), "{motivo}");
    }

    /// Un avviso di libreria non rende rotta una pagina che mostra il proprio
    /// contenuto: il console.error resta evidenza, non verdetto. MUTAZIONE:
    /// aggiungerlo alle cause -> questo test cade, ed e' il falso rosso che il
    /// modulo evita apposta.
    #[test]
    fn il_console_error_non_boccia() {
        let prove = ProveResa {
            errori_console: vec!["[Violation] handler took 62ms".into()],
            ..resa(48)
        };
        assert_eq!(
            classifica_resa(&prove, 5),
            VerdettoResa::Resa { elementi: 48 }
        );
    }

    /// Le cause si raccolgono TUTTE: una pagina che lancia E resta vuota lo
    /// dice in un giro solo.
    #[test]
    fn le_cause_si_raccolgono_tutte() {
        let prove = ProveResa {
            errori_esecuzione: vec!["TypeError: null".into()],
            contenitore: Some(EsitoContenitore::Trovato { figli: 0 }),
            ..resa(1)
        };
        let VerdettoResa::NonResa { cause } = classifica_resa(&prove, 5) else {
            panic!("tre difetti insieme restano difetti");
        };
        assert_eq!(cause.len(), 3, "eccezione + contenitore vuoto + pagina vuota");
    }

    /// Il discriminante: dove c'e' un servizio la domanda la pone gia' il
    /// dialogo, e questo criterio non nasce. MUTAZIONE: invertire la
    /// precedenza -> un progetto servito verrebbe misurato su un file che non
    /// e' cio' che espone.
    #[test]
    fn il_servizio_ha_la_precedenza_sulla_pagina() {
        assert_eq!(
            classifica_natura(Some("http://127.0.0.1:35954"), Some("landing/index.html")),
            NaturaApp::ConServizio
        );
        assert_eq!(
            classifica_natura(None, Some("landing/index.html")),
            NaturaApp::Statica {
                entry: "landing/index.html".into()
            }
        );
        assert_eq!(classifica_natura(None, None), NaturaApp::SenzaPagina);
        // Un'origine vuota non e' un servizio, e un'entry vuota non e' una
        // pagina: le stringhe degeneri non creano nature.
        assert_eq!(
            classifica_natura(Some("  "), Some("index.html")),
            NaturaApp::Statica {
                entry: "index.html".into()
            }
        );
        assert_eq!(classifica_natura(None, Some("   ")), NaturaApp::SenzaPagina);
    }

    /// Il criterio si costruisce dal produttore unico, e senza URL non nasce.
    #[test]
    fn il_criterio_nasce_solo_con_un_url() {
        assert!(criterio_resa(None, None, 5, 30.0, 2000).is_none());
        assert!(criterio_resa(Some("  "), None, 5, 30.0, 2000).is_none());

        let c = criterio_resa(
            Some("http://127.0.0.1:4000/preview/e4d446ce/landing/index.html"),
            Some("#courses-grid"),
            5,
            30.0,
            2000,
        )
        .expect("criterio");
        assert_eq!(c.criterion_type, CRITERION_TYPE);
        assert_eq!(c.spec[CHIAVE_MIN_ELEMENTI], 5);
        assert_eq!(c.spec[CHIAVE_ATTESA_MS], 2000);
        assert_eq!(c.spec[CHIAVE_CONTENITORE], "#courses-grid");

        // Senza contenitore dichiarato la chiave non c'e' affatto: un
        // selettore vuoto nella spec farebbe cercare al browser un elemento
        // che nessuno ha chiesto.
        let senza = criterio_resa(Some("http://x/index.html"), Some(" "), 5, 30.0, 2000)
            .expect("criterio");
        assert!(senza.spec.get(CHIAVE_CONTENITORE).is_none());
    }

    /// La dichiarazione dell'agente arriva al criterio: l'URL lo risolve chi
    /// conosce la radice, il contenitore lo sa solo chi vede lo stato del run,
    /// e la chiave della spec resta scritta in un posto solo.
    ///
    /// MUTAZIONE: far scrivere la chiave a mano al chiamante -> due letterali
    /// per la stessa chiave, e il giorno che uno cambia il browser cerca un
    /// elemento che nessuno ha dichiarato.
    #[test]
    fn il_contenitore_dichiarato_entra_nel_criterio() {
        let base = criterio_resa(Some("http://x/index.html"), None, 5, 30.0, 2000)
            .expect("criterio");
        let dichiarato = json!({ "outcome": "done", "rendered_container": "#courses-grid" });
        let c = con_contenitore(base.clone(), Some(&dichiarato));
        assert_eq!(c.spec[CHIAVE_CONTENITORE], "#courses-grid");

        // Nessuna dichiarazione: il criterio resta quello che era, coi due
        // segnali che non richiedono di dichiarare nulla.
        assert!(con_contenitore(base.clone(), None)
            .spec
            .get(CHIAVE_CONTENITORE)
            .is_none());
        let vuota = json!({ "outcome": "done", "rendered_container": "  " });
        assert!(con_contenitore(base, Some(&vuota))
            .spec
            .get(CHIAVE_CONTENITORE)
            .is_none());
    }
}
