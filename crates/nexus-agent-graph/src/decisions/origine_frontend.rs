//! PUNTO UNICO (regola L) di «qual e' l'ORIGINE del frontend di questo
//! progetto?», cioe' l'indirizzo su cui il final_gate carica la pagina per
//! chiederle se ottiene i propri dati.
//!
//! CAUSA RADICE, misurata il 09/08/2026 su gestione-corsi. Il criterio
//! [`super::browser_dialogue`] non e' MAI nato: zero occorrenze su 74 gate
//! storici, e zero su 6 gate lanciati con i servizi del progetto VIVI e in
//! ascolto. Il log dichiarava la rinuncia — quella meta' era fatta bene — ma la
//! causa stava a monte: l'origine si sceglieva prendendo la PRIMA allocazione
//! di porta la cui LABEL somigliasse alla parola «frontend».
//!
//! | porta | label               | cosa era davvero            |
//! |-------|---------------------|-----------------------------|
//! | 34894 | `schoolcoursesapi`  | backend .NET, in ascolto    |
//! | 34859 | `schoolcoursesfe`   | frontend Next, in ascolto   |
//! | 34853 | `school-courses-fe` | allocata, nessuno in ascolto|
//!
//! Nessuna delle tre contiene «frontend»/«web»/«ui» o un'altra parola del
//! vocabolario di `similar_service_labels`: `fe` e' l'abbreviazione piu' diffusa
//! per frontend e non e' riconosciuta. Aggiungercela sarebbe la toppa che la
//! regola H vieta per nome — un elenco di parole al posto di una proprieta'
//! strutturale — e qui con un'aggravante: l'elenco ASSOLVE PER OMISSIONE. Non
//! nominare un servizio non significa giudicarlo male, significa non guardarlo
//! affatto, e il gate resta cieco in silenzio.
//!
//! IL CRITERIO E' STRUTTURALE. Un frontend e' cio' che SERVE UNA PAGINA, non
//! cio' che si chiama in un certo modo: si interroga la radice di ogni porta
//! registrata al progetto e si guarda che cosa risponde. E' lo STESSO segnale
//! che [`super::endpoint_probes`] legge all'inverso — li' un endpoint di API che
//! risponde HTML ha servito la pagina del frontend invece dei dati del backend —
//! ed e' per questo che il predicato e' UNO solo, [`dichiara_html`], e non due
//! copie destinate a divergere.
//!
//! PERCHE' NON BASTA «RISPONDE HTML». Express serve al proprio root
//! `Cannot GET /` con `Content-Type: text/html` e status 404; una pagina di
//! eccezione di sviluppo e' HTML con 500. Sono risposte d'ERRORE, non pagine
//! servite. Il successo (2xx) e' la meta' del criterio che le tiene fuori senza
//! nominare nessun framework: inseguire le varianti a codice riporterebbe
//! l'elenco di parole da un'altra porta.
//!
//! LA VITA NON E' UN SECONDO CRITERIO, E' LO STESSO. Una porta che serve una
//! pagina sta rispondendo, quindi e' viva per costruzione — e in un senso piu'
//! forte di «qualcuno ascolta», che e' cio' che sanno dire
//! `service_liveness`/`port_recovery`: un socket aperto che non risponde nulla
//! non e' un'origine che un browser possa caricare. Nel caso misurato e'
//! esattamente cio' che distingue 34859 da 34853, che hanno label gemelle e
//! nessun altro segnale che le separi.
//!
//! CONFINE (regola L): qui SOLO il criterio puro, sui fatti gia' raccolti. L'I/O
//! — leggere `nexus_port_allocations`, interrogare le radici — sta in mcp-core
//! (`project_workspace::origine_frontend`), che porta i fatti e non li giudica.

use serde::{Deserialize, Serialize};

/// L'origine su cui una porta di progetto si apre, in loopback.
///
/// UNA sola costruzione, usata sia dalla PROVA sia dal VERDETTO: se la prova
/// interrogasse un indirizzo e il verdetto ne dichiarasse un altro, il gate
/// misurerebbe una pagina e poi ne aprirebbe un'altra (regola O).
pub fn origine_di(porta: u16) -> String {
    format!("http://127.0.0.1:{porta}")
}

/// «Questa risposta e' un documento HTML?»
///
/// PUNTO UNICO (regola L) di un segnale che due criteri leggono in versi
/// OPPOSTI, e che percio' non puo' avere due implementazioni:
/// - su un endpoint di API attraverso il frontend (`reject_html`, vedi
///   [`super::endpoint_probes::criteri_integrazione_frontend`]) una risposta
///   HTML e' il DIFETTO: il proxy non raggiunge l'API e il fallback della SPA
///   maschera il 404 con un 200 (misurato su biblioteca-scolastica, 04/08/2026);
/// - sulla radice di un servizio e' invece la PROVA che li' c'e' un frontend.
///
/// Il `Content-Type` e' la fonte: e' cio' che il server DICHIARA di aver mandato
/// (regola M). Il corpo interviene solo quando l'header manca — un
/// `<!DOCTYPE html` in testa e' sintassi, non prosa, e senza header non c'e'
/// altro da chiedere.
pub fn dichiara_html(content_type: Option<&str>, corpo_iniziale: &str) -> bool {
    if let Some(ct) = content_type {
        let ct = ct.to_ascii_lowercase();
        // `text/html`, `text/html; charset=utf-8`, `application/xhtml+xml`.
        return ct.contains("text/html") || ct.contains("xhtml");
    }
    let inizio = corpo_iniziale.trim_start().to_ascii_lowercase();
    inizio.starts_with("<!doctype html") || inizio.starts_with("<html")
}

/// Che cosa ha risposto la RADICE di una porta registrata al progetto.
///
/// Quattro varianti e non un booleano (regola Q): «non serve pagine» e «non
/// l'ho potuta interrogare» portano a due verdetti opposti sull'intero
/// progetto — un accertamento contro un ignoto — e collassarle in un `bool`
/// rimetterebbe il gate a dichiarare «nessun frontend» proprio dove non ha
/// guardato.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RispostaRadice {
    /// Successo (2xx) e `Content-Type` che dichiara un documento HTML: qui una
    /// pagina viene servita.
    Pagina { status: u16 },
    /// Ha parlato HTTP, e non ha servito una pagina: un'API JSON, un 404, una
    /// pagina d'errore.
    NonPagina {
        status: u16,
        content_type: Option<String>,
    },
    /// Nessuna risposta HTTP: la porta risulta registrata e non c'e' nessuno.
    Muta,
    /// La prova non e' stata fatta, o non ha prodotto una risposta
    /// interpretabile (timeout, porta non autorizzata a questo progetto). NON
    /// e' «non e' un frontend»: e' «non lo so».
    NonProvata { motivo: String },
}

/// UNA porta registrata al progetto, con cio' che si e' osservato su di essa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidataOrigine {
    pub porta: u16,
    pub label: String,
    pub risposta: RispostaRadice,
    /// SEGNALE AUSILIARIO, mai discriminante: la label somiglia a «frontend»?
    ///
    /// Serve solo a rompere una parita' fra due porte che servono ENTRAMBE una
    /// pagina. Non puo' far entrare nulla (una label che dice frontend su una
    /// porta muta resta muta) ne' far uscire nulla (`schoolcoursesfe` dice
    /// `false` ed e' il frontend): e' precisamente la degradazione da
    /// discriminante a indizio che questo modulo esiste per fare.
    pub label_dice_frontend: bool,
}

/// Perche' e' stata scelta QUESTA fra le pagine servite. Sta nel verdetto e non
/// solo nel log: un'origine scelta fra due e' un'informazione diversa da
/// un'origine unica, e chi legge il gate deve poterle distinguere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MotivoScelta {
    /// Unica porta del progetto che serve una pagina.
    UnicaPagina,
    /// Piu' d'una serve una pagina, e la label di questa e' la sola che dica
    /// «frontend».
    LabelDistintiva { altre_pagine: usize },
    /// Piu' d'una serve una pagina e nessuna label le distingue: si prende la
    /// porta piu' bassa. Ordine STABILE e dichiarato — mai «la prima che il DB
    /// restituisce», che senza `ORDER BY` non e' nemmeno una scelta.
    PortaPiuBassa { altre_pagine: usize },
}

impl MotivoScelta {
    pub fn descrizione(&self) -> String {
        match self {
            MotivoScelta::UnicaPagina => "unica porta che serve una pagina".to_string(),
            MotivoScelta::LabelDistintiva { altre_pagine } => format!(
                "serve una pagina come altre {altre_pagine}, ed e' la sola la cui label dica \
                 «frontend»"
            ),
            MotivoScelta::PortaPiuBassa { altre_pagine } => format!(
                "serve una pagina come altre {altre_pagine} e nessuna label le distingue: \
                 scelta la porta piu' bassa (ordine stabile)"
            ),
        }
    }
}

/// Il verdetto sul progetto. Tre facce, e la terza non e' un ripiego: «non c'e'
/// un frontend» e «non ho trovato un'origine» sono cose diverse, e il gate ne
/// trae conseguenze diverse — la prima e' un progetto legittimo (un backend, o
/// un'app statica: la guarda [`super::static_render`]), la seconda e' una misura
/// che non e' avvenuta e va detta come tale (regola Q).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrigineFrontend {
    Trovata {
        origine: String,
        porta: u16,
        label: String,
        motivo: MotivoScelta,
    },
    /// ACCERTATO: tutte le porte registrate hanno risposto, e nessuna serve una
    /// pagina. `porte_esaminate == 0` significa che il progetto non ne ha
    /// nessuna registrata — anche quello e' un fatto letto, non un ignoto.
    NessunFrontend { porte_esaminate: usize },
    /// NON accertato: qualcosa non si e' potuto guardare.
    NonAccertata { motivo: String },
}

impl OrigineFrontend {
    /// L'origine da dare ai criteri, quando c'e'.
    pub fn origine(&self) -> Option<&str> {
        match self {
            OrigineFrontend::Trovata { origine, .. } => Some(origine.as_str()),
            _ => None,
        }
    }

    /// Il testo per il log, composto DAI campi (regola Q punto 3): nessun
    /// chiamante ricompone la frase, e nessuno la rilegge per dedurne l'esito.
    pub fn descrizione(&self) -> String {
        match self {
            OrigineFrontend::Trovata {
                origine,
                label,
                motivo,
                ..
            } => format!("origine {origine} (servizio '{label}'): {}", motivo.descrizione()),
            OrigineFrontend::NessunFrontend { porte_esaminate: 0 } => {
                "nessun frontend: il progetto non ha porte registrate, non c'e' niente da \
                 interrogare"
                    .to_string()
            }
            OrigineFrontend::NessunFrontend { porte_esaminate } => format!(
                "nessun frontend: {porte_esaminate} porte interrogate, nessuna serve una pagina"
            ),
            OrigineFrontend::NonAccertata { motivo } => {
                format!("origine non accertata: {motivo}")
            }
        }
    }
}

/// IL CRITERIO. Puro: dati i fatti osservati su ciascuna porta, quale origine e'
/// il frontend?
///
/// L'ordine della decisione e' dichiarato e non dipende dall'ordine dei
/// candidati:
/// 1. entrano SOLO le porte che servono una pagina ([`RispostaRadice::Pagina`]);
/// 2. fra quelle, precede chi ha la label che dice «frontend» (indizio, non
///    discriminante: se nessuna ce l'ha non esclude nessuno);
/// 3. a parita', la porta piu' bassa.
///
/// Se NESSUNA serve una pagina la risposta dipende da cosa non si e' potuto
/// guardare: con almeno una `NonProvata` il verdetto e' `NonAccertata`, perche'
/// il frontend puo' essere proprio fra quelle. Dichiarare «nessun frontend»
/// senza aver interrogato tutto e' l'errore che questo modulo esiste per non
/// commettere piu'.
pub fn scegli_origine(candidate: &[CandidataOrigine]) -> OrigineFrontend {
    let non_provate: Vec<&str> = candidate
        .iter()
        .filter_map(|c| match &c.risposta {
            RispostaRadice::NonProvata { motivo } => Some(motivo.as_str()),
            _ => None,
        })
        .collect();

    let mut pagine: Vec<&CandidataOrigine> = candidate
        .iter()
        .filter(|c| matches!(c.risposta, RispostaRadice::Pagina { .. }))
        .collect();

    if pagine.is_empty() {
        if let Some(primo) = non_provate.first() {
            return OrigineFrontend::NonAccertata {
                motivo: format!(
                    "nessuna pagina fra le {} porte interrogate, ma {} non sono state provate \
                     (la prima: {primo}): il frontend puo' essere fra quelle",
                    candidate.len() - non_provate.len(),
                    non_provate.len()
                ),
            };
        }
        return OrigineFrontend::NessunFrontend {
            porte_esaminate: candidate.len(),
        };
    }

    // `sort_by_key` e' stabile, ma la chiave non lascia parita' residue: due
    // candidate con la stessa porta non possono esistere (`nexus_port_allocations`
    // ha UNIQUE su `port`).
    pagine.sort_by_key(|c| (!c.label_dice_frontend, c.porta));
    let scelta = pagine[0];
    let altre_pagine = pagine.len() - 1;
    let con_label = pagine.iter().filter(|c| c.label_dice_frontend).count();
    let motivo = if pagine.len() == 1 {
        MotivoScelta::UnicaPagina
    } else if con_label == 1 && scelta.label_dice_frontend {
        MotivoScelta::LabelDistintiva { altre_pagine }
    } else {
        // Copre sia «nessuna label lo dice» sia «piu' d'una lo dice»: in
        // entrambi i casi l'indizio non separa, e a separare resta l'ordine.
        MotivoScelta::PortaPiuBassa { altre_pagine }
    };

    OrigineFrontend::Trovata {
        origine: origine_di(scelta.porta),
        porta: scelta.porta,
        label: scelta.label.clone(),
        motivo,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidata(porta: u16, label: &str, risposta: RispostaRadice) -> CandidataOrigine {
        CandidataOrigine {
            porta,
            label: label.to_string(),
            risposta,
            // Il vocabolario vero lo applica il chiamante (mcp-core): qui si
            // dichiara l'indizio, che e' cio' che il criterio consuma.
            label_dice_frontend: label.contains("frontend"),
        }
    }

    /// Le TRE allocazioni reali di gestione-corsi, nella forma in cui il
    /// criterio le riceve. Nessuna label e' riconoscibile, e la scelta cade
    /// comunque sulla sola che serve una pagina.
    ///
    /// MUTAZIONE: far entrare nelle `pagine` anche `NonPagina` -> vince 34853
    /// oppure 34894 (porta piu' bassa) e il test rosseggia col numero del
    /// difetto.
    #[test]
    fn fra_due_frontend_gemelli_vince_quello_che_serve_la_pagina() {
        let esito = scegli_origine(&[
            candidata(34894, "schoolcoursesapi", RispostaRadice::NonPagina {
                status: 404,
                content_type: Some("application/json".to_string()),
            }),
            candidata(34859, "schoolcoursesfe", RispostaRadice::Pagina { status: 200 }),
            candidata(34853, "school-courses-fe", RispostaRadice::Muta),
        ]);
        assert_eq!(
            esito,
            OrigineFrontend::Trovata {
                origine: "http://127.0.0.1:34859".to_string(),
                porta: 34859,
                label: "schoolcoursesfe".to_string(),
                motivo: MotivoScelta::UnicaPagina,
            },
            "l'unica che serve una pagina e' il frontend, comunque si chiami"
        );
    }

    /// La label che dice «frontend» non fa entrare una porta muta: l'indizio
    /// ordina le pagine, non le crea.
    #[test]
    fn la_label_non_promuove_una_porta_muta() {
        let esito = scegli_origine(&[
            candidata(30001, "frontend", RispostaRadice::Muta),
            candidata(30002, "schoolcoursesfe", RispostaRadice::Pagina { status: 200 }),
        ]);
        assert_eq!(esito.origine(), Some("http://127.0.0.1:30002"));
    }

    /// Due pagine, una sola label distintiva: vince la label, e il verdetto lo
    /// DICE (non e' la stessa cosa di un'origine unica).
    #[test]
    fn a_parita_di_pagina_la_label_e_l_indizio() {
        let esito = scegli_origine(&[
            candidata(30005, "api-gateway", RispostaRadice::Pagina { status: 200 }),
            candidata(30009, "frontend", RispostaRadice::Pagina { status: 200 }),
        ]);
        let OrigineFrontend::Trovata { porta, motivo, .. } = esito else {
            panic!("due pagine: una va scelta");
        };
        assert_eq!(porta, 30009, "la label rompe la parita', la porta no");
        assert_eq!(motivo, MotivoScelta::LabelDistintiva { altre_pagine: 1 });
    }

    /// Due pagine e nessuna label distintiva: la scelta e' deterministica e
    /// dichiarata, non l'ordine di arrivo delle righe.
    #[test]
    fn senza_indizi_la_scelta_resta_deterministica() {
        let candidate = [
            candidata(30009, "uno", RispostaRadice::Pagina { status: 200 }),
            candidata(30005, "due", RispostaRadice::Pagina { status: 200 }),
        ];
        let diretto = scegli_origine(&candidate);
        let invertito = scegli_origine(&[candidate[1].clone(), candidate[0].clone()]);
        assert_eq!(diretto, invertito, "l'ordine dei candidati non decide nulla");
        assert_eq!(diretto.origine(), Some("http://127.0.0.1:30005"));
    }

    /// Solo un backend: e' un ACCERTAMENTO, non un ignoto. Il gate deve poter
    /// dire «questo progetto non ha un frontend» senza che somigli a «non ho
    /// guardato».
    #[test]
    fn nessuna_pagina_accertata_e_nessun_frontend() {
        let esito = scegli_origine(&[candidata(30001, "api", RispostaRadice::NonPagina {
            status: 200,
            content_type: Some("application/json".to_string()),
        })]);
        assert_eq!(esito, OrigineFrontend::NessunFrontend { porte_esaminate: 1 });
    }

    /// Nessuna porta registrata: fatto letto (il registro e' vuoto), non ignoto.
    #[test]
    fn senza_porte_registrate_non_c_e_niente_da_interrogare() {
        assert_eq!(
            scegli_origine(&[]),
            OrigineFrontend::NessunFrontend { porte_esaminate: 0 }
        );
    }

    /// Una porta non interrogata e nessuna pagina: il verdetto NON e' «nessun
    /// frontend».
    ///
    /// MUTAZIONE: trattare `NonProvata` come `Muta` -> il test rosseggia, ed e'
    /// il caso in cui il gate tornerebbe a dichiarare cieco un progetto che ha
    /// un frontend.
    #[test]
    fn una_porta_non_provata_impedisce_di_dire_nessun_frontend() {
        let esito = scegli_origine(&[
            candidata(30001, "api", RispostaRadice::NonPagina {
                status: 200,
                content_type: Some("application/json".to_string()),
            }),
            candidata(30002, "web", RispostaRadice::NonProvata {
                motivo: "timeout".to_string(),
            }),
        ]);
        let OrigineFrontend::NonAccertata { motivo } = esito else {
            panic!("una porta non provata non autorizza a dire «nessun frontend»");
        };
        assert!(motivo.contains("timeout"), "il motivo va riportato: {motivo}");
    }

    /// Header assente: si guarda l'inizio del corpo, che e' sintassi e non
    /// prosa. Un JSON che PARLA di html non e' una pagina. (Asserzioni portate
    /// qui insieme al predicato, dal loro posto in `criteria_runner`.)
    #[test]
    fn senza_header_decide_la_sintassi_non_una_parola_nel_corpo() {
        assert!(dichiara_html(None, "  <!DOCTYPE html><html>"));
        assert!(dichiara_html(None, "<html><body>x</body></html>"));
        assert!(!dichiara_html(
            None,
            "{\"tipo\":\"text/html\",\"nota\":\"<html> nel dato\"}"
        ));
        // L'header, quando c'e', ha la precedenza sul corpo.
        assert!(!dichiara_html(Some("application/json"), "<!DOCTYPE html>"));
        assert!(dichiara_html(Some("text/html; charset=utf-8"), "{}"));
        assert!(dichiara_html(Some("application/xhtml+xml"), ""));
    }
}
