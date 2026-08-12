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
//! IL QUARTO SEGNALE, aggiunto il 09/08/2026 e MISURATO in esercizio lo stesso
//! giorno: le RISORSE che la pagina referenzia e non riesce a caricare. Il
//! ciclo qui sopra aveva appena funzionato — pagina bocciata a zero elementi,
//! agente rimandato, pagina ripassata — e l'app approvata aveva tutte le
//! immagini rotte: sei `<img>` verso `https://via.placeholder.com/...`, un
//! servizio esterno irraggiungibile, e nessun file immagine nel progetto. I tre
//! segnali sopra non potevano vederlo, e non per svista: **un'immagine rotta E'
//! un elemento reso**, quindi il conteggio del DOM la contava, il contenitore
//! aveva i suoi figli e nessuna eccezione era stata lanciata. Il criterio
//! faceva esattamente cio' che dichiara; mancava la domanda.
//! Il giudizio sulle risorse e' il punto unico [`super::risorse_pagina`], e non
//! e' un booleano: entra qui come causa SOLO quando un TIPO e' compromesso —
//! cioe' quando la quota di fallimenti di quel tipo raggiunge la soglia DB —
//! mentre le assenze sparse restano evidenza. La ragione e' la stessa del
//! `console.error`: «una icona decorativa manca» e «nessuna immagine di questa
//! pagina e' arrivata» non sono lo stesso fatto, e trattarli allo stesso modo
//! comprerebbe il caso misurato al prezzo dei rimandi a vuoto.
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
//! QUALE PAGINA si misura NON e' una domanda di questo modulo, e dall'11/08/2026
//! ha il suo punto unico: [`super::pagina_del_run`]. Qui resta il DISCRIMINANTE
//! ([`classifica_natura`]), a cui quel modulo delega per la precedenza del
//! servizio. La ragione della separazione e' un difetto misurato: la pagina era
//! risolta a t=0, prima che il run scrivesse alcunche', quindi su un progetto
//! nuovo il criterio non nasceva (nessuna pagina da rilevare) e su un progetto
//! con una pagina preesistente misurava QUELLA invece di cio' che il run aveva
//! prodotto — un ciclo di correzione che non poteva convergere.
//!
//! CONFINE (regola L): qui SOLO il criterio puro sui fatti gia' raccolti.
//! L'I/O — avviare Chromium, caricare la pagina, contare gli elementi — sta in
//! `mcp-core` (`agent_tools::browser_probe`), che porta i fatti e non li
//! giudica. Stesso taglio di [`super::browser_dialogue`].

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::risorse_pagina::{
    self, ElementoPortante, PoliticaRisorse, RisorsaMancante, RisorsaOsservata, TipoCompromesso,
    VerdettoRisorse,
};

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
    pub errori_esecuzione: Vec<EccezionePagina>,
    /// Messaggi di console di livello errore. INFORMATIVI: entrano
    /// nell'evidenza, mai nel verdetto (vedi doc del modulo).
    #[serde(default)]
    pub errori_console: Vec<String>,
    /// Le risorse sub-documento che la pagina ha chiesto, con l'esito e il tipo
    /// dichiarati dal browser. `None` = non riportate dall'osservazione, che
    /// NON e' una lista vuota: quella direbbe «la pagina non ha chiesto nulla».
    #[serde(default)]
    pub risorse: Option<Vec<RisorsaOsservata>>,
    /// Gli ELEMENTI della pagina che portano una risorsa, con l'esito della
    /// loro resa. Canale distinto da [`ProveResa::risorse`], non un suo
    /// doppione: quello dice se la risorsa e' ARRIVATA, questo se si e' VISTA.
    ///
    /// `None` = l'osservazione non li ha riportati. Serve perche' per gli URL
    /// incorporati (`data:`) il browser non emette alcun evento di rete —
    /// MISURATO il 10/08/2026: `requests: []` su una pagina con sei immagini
    /// rotte — e senza questo canale l'unica risposta possibile sarebbe «la
    /// pagina non referenzia nulla».
    #[serde(default)]
    pub elementi: Option<Vec<ElementoPortante>>,
    /// L'URL su cui la pagina si e' fermata, come lo dichiara il browser (dopo
    /// eventuali redirezioni). Serve SOLO ad attribuire la provenienza di una
    /// risorsa mancante: senza, il verdetto e' lo stesso e i rilievi dicono
    /// «origine non stabilita» invece di indovinare di chi sia la colpa.
    #[serde(default)]
    pub origine: Option<String>,
}

/// Un'eccezione non gestita, coi campi che il browser ha dichiarato.
///
/// E' un TIPO e non una stringa per la ragione misurata il 12/08/2026: con
/// `Vec<String>` l'unica cosa che raggiungeva l'agente era «il JavaScript della
/// pagina ha lanciato: Invalid or unexpected token», e su un file di 244 righe
/// quella frase non dice dove guardare. Nella traccia del run: cinque letture
/// consecutive senza una scrittura (`list_files`, `read_file`,
/// `read_file_lines` x2, `list_files`), poi una promozione di modello innescata
/// da un segnale che quelle letture le legge come «descrive senza agire» — cioe'
/// il sistema ha scambiato per inerzia una RICERCA che aveva reso necessaria lui.
///
/// La posizione e' `Option` perche' esiste una classe di eccezioni per cui
/// nessun canale la porta; l'assenza si DICHIARA e non degrada a uno zero
/// (regola Q). `classe` e' separata dal messaggio perche' e' l'informazione che
/// dice se cercare un errore di sintassi o di logica, ed era l'unica che il
/// produttore reale gia' aveva e buttava via (`e.name`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EccezionePagina {
    /// Il messaggio, come il browser lo consegna.
    pub messaggio: String,
    /// `SyntaxError`, `ReferenceError`, ... `None` = non dichiarata.
    #[serde(default)]
    pub classe: Option<String>,
    /// Il file che ha lanciato. `None` = non attribuito: una pagina puo' avere
    /// piu' script, e indovinare quale manderebbe a correggere il file sbagliato.
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub riga: Option<u32>,
    #[serde(default)]
    pub colonna: Option<u32>,
}

/// Un'eccezione di cui si conosce il SOLO messaggio. E' un caso reale, non una
/// comodita' per i test: esiste una classe di eccezioni per cui nessun canale
/// porta la posizione, e per quelle questa e' la forma completa.
impl<S: Into<String>> From<S> for EccezionePagina {
    fn from(messaggio: S) -> Self {
        Self {
            messaggio: messaggio.into(),
            ..Default::default()
        }
    }
}

impl EccezionePagina {
    /// Solo il messaggio, per i consumatori che non hanno un posto dove mettere
    /// il resto (il dialogo browser, che di un'eccezione non fa un verdetto).
    pub fn testo(&self) -> String {
        match self.classe.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            Some(c) if !self.messaggio.starts_with(c) => format!("{c}: {}", self.messaggio),
            _ => self.messaggio.clone(),
        }
    }

    /// `file:riga:colonna`, con quello che c'e'. `None` = nessun canale l'ha
    /// portata: si tace invece di scrivere una posizione inventata.
    pub fn posizione(&self) -> Option<String> {
        let riga = self.riga?;
        let file = self
            .file
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty())
            // Il percorso completo di una URL non aiuta chi deve aprire il file:
            // il nome basta a scegliere fra gli script di una pagina.
            .map(|f| f.rsplit(['/', '\\']).next().unwrap_or(f).to_string())
            .unwrap_or_else(|| "la pagina".to_string());
        Some(match self.colonna {
            Some(col) => format!("{file}:{riga}:{col}"),
            None => format!("{file}:{riga}"),
        })
    }

    /// La riga che l'agente legge: cosa e' successo E dove. Composta DAI campi
    /// (regola Q punto 3).
    pub fn descrizione(&self) -> String {
        let t = self.testo();
        let t = if t.trim().is_empty() {
            "eccezione non gestita".to_string()
        } else {
            t
        };
        match self.posizione() {
            Some(p) => format!("{t} (in {p})"),
            None => t,
        }
    }
}

/// Cosa impedisce alla pagina di mostrare il proprio contenuto. Vocabolario
/// CHIUSO (regola N) e CAUSA insieme al verdetto: un rilievo che non dice cosa
/// e' rimasto vuoto manda l'agente a cercare alla cieca.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CausaNonResa {
    /// Il codice della pagina ha lanciato: tutto cio' che seguiva non e' girato.
    EsecuzioneInterrotta { eccezione: EccezionePagina },
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
    /// Un TIPO di risorsa non e' arrivato: la pagina ha i suoi nodi e non ha
    /// cio' che quei nodi dovevano mostrare. Il giudizio lo da'
    /// [`super::risorse_pagina`]; qui si porta il suo verdetto nel rilievo.
    RisorseNonCaricate {
        tipi: Vec<TipoCompromesso>,
        mancanti: Vec<RisorsaMancante>,
    },
}

impl CausaNonResa {
    /// La riga che l'agente legge. Composta DAI campi (regola Q punto 3).
    pub fn descrizione(&self) -> String {
        match self {
            Self::EsecuzioneInterrotta { eccezione } => {
                format!(
                    "il JavaScript della pagina ha lanciato: {}",
                    eccezione.descrizione()
                )
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
            Self::RisorseNonCaricate { tipi, mancanti } => {
                let quali: Vec<String> = tipi.iter().map(TipoCompromesso::descrizione).collect();
                let esempi: Vec<String> = mancanti
                    .iter()
                    .take(ESEMPI_RISORSE)
                    .map(RisorsaMancante::descrizione)
                    .collect();
                format!(
                    "la pagina referenzia risorse che non arrivano, e i suoi nodi restano \
                     vuoti all'occhio ({}); per esempio: {}",
                    quali.join("; "),
                    esempi.join(", ")
                )
            }
        }
    }
}

/// Quante risorse mancanti si nominano nel rilievo: bastano a riconoscere il
/// difetto senza riversare l'intera pagina nel contesto del turno.
const ESEMPI_RISORSE: usize = 3;

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
///
/// `politica` governa il quarto segnale, le risorse (anch'essa dal DB): entra
/// fra le cause SOLO quando un tipo e' compromesso, e cio' che non raggiunge
/// quella soglia — o non e' stato osservabile — non produce nulla qui e resta
/// nell'evidenza, dove lo mette [`risorse_della_pagina`].
pub fn classifica_resa(
    prove: &ProveResa,
    minimo_elementi: usize,
    politica: &PoliticaRisorse,
) -> VerdettoResa {
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
        .map(|e| CausaNonResa::EsecuzioneInterrotta {
            eccezione: e.clone(),
        })
        .collect();

    cause.extend(causa_contenitore(prove.contenitore.as_ref()));

    if elementi < minimo_elementi {
        cause.push(CausaNonResa::PaginaVuota {
            elementi,
            minimo: minimo_elementi,
        });
    }

    // Il quarto segnale entra per ULTIMO, ed e' un ordine e non un caso: le tre
    // cause sopra dicono «la pagina non e' stata generata», questa dice «e'
    // stata generata e non ha di che mostrarsi». Chi legge il rilievo affronta
    // prima cio' che impedisce alla pagina di esistere.
    if let VerdettoRisorse::TipiCompromessi { tipi, mancanti } =
        risorse_della_pagina(prove, politica)
    {
        cause.push(CausaNonResa::RisorseNonCaricate { tipi, mancanti });
    }

    if cause.is_empty() {
        VerdettoResa::Resa { elementi }
    } else {
        VerdettoResa::NonResa { cause }
    }
}

/// Il verdetto sulle risorse di QUESTA pagina: lega i fatti gia' raccolti al
/// punto unico che li giudica ([`super::risorse_pagina::classifica_risorse`]).
///
/// Esiste come funzione perche' il legame fra i campi di [`ProveResa`] e gli
/// argomenti del criterio sia scritto in un posto solo: lo usano
/// [`classifica_resa`], per decidere, e il runner del gate, per l'evidenza —
/// che deve riportare anche le assenze SOTTO soglia, cioe' proprio i casi in
/// cui il verdetto della resa non dice nulla. E' pura e deterministica, quindi
/// le due chiamate sono la stessa risposta, non due risposte.
pub fn risorse_della_pagina(prove: &ProveResa, politica: &PoliticaRisorse) -> VerdettoRisorse {
    risorse_pagina::classifica_risorse(
        prove.risorse.as_deref(),
        prove.elementi.as_deref(),
        prove.origine.as_deref(),
        politica,
    )
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

/// Quanto pesa il verdetto di questo criterio sul run. Vocabolario CHIUSO
/// (regola N), modellato su [`super::step_gate::StepGateMode`], che risponde
/// alla stessa domanda per un altro presidio.
///
/// PERCHE' SI PARTE APPLICANDO (`enforce`, mig 0700), per ogni pagina e senza
/// distinzione fra quelle che il run ha scritto e quelle rilevate sull'albero.
///
///   1. Osservare e basta NON CHIUDE il caso che motiva la risoluzione tardiva.
///      `test-11-08-listino`: pagina rotta (eccezione non gestita, contenitore a
///      zero figli, body di 90 caratteri) e run chiuso «task complete». Ora il
///      criterio nasce e la misura e' negativa — ma in osservazione l'esito
///      resta `Passed`, quindi quel run si chiuderebbe di nuovo «completato».
///   2. Il criterio NON e' nuovo: e' in esercizio dalla mig 0685 e la sua chiave
///      booleana valeva `true` in produzione, con la conseguenza piena. Nascere
///      in osservazione non sarebbe stata prudenza verso una copertura nuova,
///      sarebbe stato un depotenziamento di una difesa gia' attiva.
///   3. La MISURA e' la stessa nei due regimi: la modalita' non entra nel merito
///      del verdetto, e non c'e' niente da guadagnare aspettando (vedi
///      `in_osservazione` nel runner del gate).
///
/// [`ModalitaResa::Osserva`] resta, e non e' decorativa: e' il ripiego per il
/// rischio dichiarato — la popolazione di run che prima non veniva mai misurata,
/// dove una SPA scaffoldata DURANTE il run puo' referenziare un modulo che la
/// route di anteprima serve con un content-type generico, cioe' restare vuota
/// per costruzione e non per colpa dell'agente. Se quella forma si presentasse,
/// il ripiego e' una riga di UPDATE e la correzione vera sta nel content-type
/// della route, non nella soglia del criterio.
///
/// Il [`Default`] resta [`ModalitaResa::Off`], e non contraddice quanto sopra:
/// e' cio' che vale quando la configurazione NON si e' potuta leggere, e li' un
/// criterio che si accende da se' sarebbe il magic fallback che la regola G
/// vieta. Il valore con cui il sistema gira lo scrive il DB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModalitaResa {
    /// Il criterio non nasce: nessuna misura, nessun costo.
    #[default]
    Off,
    /// Misura e SCRIVE l'evidenza, senza mai produrre un `Failed`: e' la
    /// telemetria con cui si decidera', sui dati, se accendere `Applica`.
    Osserva,
    /// Il verdetto negativo boccia il run.
    Applica,
}

impl ModalitaResa {
    /// Parse dell'identificatore canonico. `None` su valore ignoto: il
    /// chiamante degrada a [`ModalitaResa::Off`] DICHIARANDOLO, perche' un
    /// criterio che si accende per un typo e' peggio di uno spento visibilmente.
    pub fn try_parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "observe" => Some(Self::Osserva),
            "enforce" => Some(Self::Applica),
            _ => None,
        }
    }

    /// Identificatore canonico, per la spec e per l'evidenza.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Osserva => "observe",
            Self::Applica => "enforce",
        }
    }

    /// Il criterio NASCE?
    pub fn nasce(self) -> bool {
        self != Self::Off
    }

    /// Un verdetto NEGATIVO boccia il run, o resta osservazione dichiarata?
    pub fn boccia(self) -> bool {
        self == Self::Applica
    }
}

/// Chiavi della spec, con un solo punto di scrittura (i test le referenziano da
/// qui, mai come letterali sparsi).
pub const CHIAVE_CONTENITORE: &str = "container_selector";
pub const CHIAVE_MIN_ELEMENTI: &str = "min_elements";
pub const CHIAVE_ATTESA_MS: &str = "settle_ms";
pub const CHIAVE_TIPI_RISORSA: &str = "resource_types";
pub const CHIAVE_SOGLIA_RISORSE: &str = "broken_resource_ratio";
/// Radice degli indirizzi di anteprima. La PAGINA non e' nella spec: la
/// risolve chi verifica, non chi costruisce il criterio (vedi
/// [`super::pagina_del_run`]).
pub const CHIAVE_BASE_ANTEPRIMA: &str = "preview_base";
/// L'origine del servizio frontend, quando il progetto ne ha uno. Viaggia nella
/// spec perche' la precedenza del servizio si decide insieme alla pagina, in un
/// punto solo e al momento della verifica.
pub const CHIAVE_ORIGINE_SERVIZIO: &str = "frontend_origin";
/// Quanto pesa il verdetto ([`ModalitaResa`]).
pub const CHIAVE_MODALITA: &str = "mode";

/// I parametri della misura che NON dipendono dalla pagina: li risolve dal DB
/// chi costruisce il criterio (regola G) e viaggiano insieme perche' sono la
/// stessa configurazione. Struct e non argomenti sciolti: sei numeri in fila
/// nella firma sono sei occasioni di scambiarne due senza che nulla se ne
/// accorga.
#[derive(Debug, Clone, PartialEq)]
pub struct ParametriMisura {
    /// Soglia sul body reso (`agent.final_gate.static_render_min_elements`).
    pub minimo_elementi: usize,
    /// Pazienza concessa all'osservazione.
    pub timeout_s: f64,
    /// Attesa che la pagina si calmi (`agent.final_gate.browser_settle_ms`).
    pub attesa_ms: u64,
    /// Politica delle risorse (mig 0692).
    pub politica: PoliticaRisorse,
    /// Quanto pesa il verdetto.
    pub modalita: ModalitaResa,
}

/// La spec del criterio, costruita QUI e non dai chiamanti: il produttore del
/// criterio e' uno solo, cosi' i test possono attraversarlo invece di
/// fabbricare la spec a mano (regola O).
///
/// LA PAGINA NON E' QUI, ed e' il punto del cambiamento dell'11/08/2026: il
/// criterio si costruisce a t=0, cioe' prima che il run scriva alcunche', e una
/// pagina risolta li' e' la pagina di IERI (o nessuna, su un progetto nuovo).
/// La spec porta la RADICE degli indirizzi di anteprima; quale pagina comporre
/// lo decide chi verifica, col punto unico [`super::pagina_del_run`].
///
/// `base_anteprima` e' quella radice. Il criterio non nasce senza: una pagina
/// che non si sa dove aprire non e' misurabile, e un criterio che fallisse per
/// questo boccerebbe il progetto per un difetto della misura.
///
/// `origine_servizio` e' l'indirizzo del frontend quando il progetto ne ha uno.
/// Non impedisce al criterio di nascere: la precedenza del servizio la applica
/// [`super::pagina_del_run::risolvi_pagina`] insieme alla scelta della pagina,
/// in un punto solo — e cosi' il criterio DICHIARA di non essersi applicato
/// invece di sparire senza dire niente.
///
/// La `politica` delle risorse viaggia nella SPEC e non come stato del runner,
/// per la stessa ragione del vocabolario delle terze parti in
/// [`super::browser_dialogue`]: e' configurazione (regola G), la risolve a
/// monte chi legge il DB, e cosi' il runner non lo legge.
pub fn criterio_resa(
    base_anteprima: Option<&str>,
    origine_servizio: Option<&str>,
    p: &ParametriMisura,
) -> Option<crate::runtime::ports::CriterionSpec> {
    use crate::runtime::ports::{CriterionProvenance, CriterionSpec};
    if !p.modalita.nasce() {
        return None;
    }
    let base = base_anteprima
        .map(str::trim)
        .map(|b| b.trim_end_matches('/'))
        .filter(|b| !b.is_empty())?;
    let mut spec = Map::new();
    spec.insert(CHIAVE_BASE_ANTEPRIMA.to_string(), json!(base));
    spec.insert(CHIAVE_MODALITA.to_string(), json!(p.modalita.as_str()));
    spec.insert(CHIAVE_MIN_ELEMENTI.to_string(), json!(p.minimo_elementi));
    spec.insert(CHIAVE_ATTESA_MS.to_string(), json!(p.attesa_ms));
    spec.insert(
        CHIAVE_TIPI_RISORSA.to_string(),
        json!(p.politica.tipi_governati),
    );
    // La soglia entra solo se configurata: una chiave assente nella spec dice
    // al criterio «non risponderai sulle risorse», e un numero di ripiego
    // scritto qui sarebbe il magic fallback che la regola G vieta.
    if let Some(s) = p.politica.soglia {
        spec.insert(CHIAVE_SOGLIA_RISORSE.to_string(), json!(s));
    }
    // Stessa disciplina per l'origine: assente significa «nessun servizio
    // dichiarato», non «stringa vuota».
    if let Some(o) = origine_servizio.map(str::trim).filter(|o| !o.is_empty()) {
        spec.insert(CHIAVE_ORIGINE_SERVIZIO.to_string(), json!(o));
    }
    Some(CriterionSpec {
        criterion_type: CRITERION_TYPE.to_string(),
        provenance: CriterionProvenance::Gate,
        spec: Value::Object(spec),
        expected: json!({}),
        timeout_s: Some(p.timeout_s),
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

    use super::super::browser_dialogue::RichiestaOsservata;
    use super::super::risorse_pagina::Provenienza;

    fn resa(elementi: usize) -> ProveResa {
        ProveResa {
            pagina_caricata: true,
            elementi_resi: Some(elementi),
            ..Default::default()
        }
    }

    /// La politica delle risorse come la scrive la migrazione 0692.
    fn politica() -> PoliticaRisorse {
        PoliticaRisorse::nuova(
            vec![
                "image".into(),
                "stylesheet".into(),
                "script".into(),
                "media".into(),
            ],
            Some(1.0),
        )
    }

    fn immagine(url: &str, status: Option<u16>, errore: &str) -> RisorsaOsservata {
        RisorsaOsservata {
            richiesta: RichiestaOsservata {
                url: url.into(),
                status,
                errore: errore.into(),
            },
            tipo: Some("image".into()),
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
        let VerdettoResa::NonResa { cause } = classifica_resa(&prove, 5, &politica()) else {
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
        let v = cause_con_selettore(classifica_resa(&prove, 5, &politica()), "#courses-grid");
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
            classifica_resa(&ok, 5, &politica()),
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
        let v = cause_con_selettore(classifica_resa(&prove, 5, &politica()), "#griglia");
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
        let VerdettoResa::NonResa { cause } = classifica_resa(&resa(2), 5, &politica()) else {
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
            classifica_resa(&spenta, 5, &politica()),
            VerdettoResa::NonConcludente { .. }
        ));

        let non_contata = ProveResa {
            pagina_caricata: true,
            elementi_resi: None,
            ..Default::default()
        };
        let VerdettoResa::NonConcludente { motivo } = classifica_resa(&non_contata, 5, &politica())
        else {
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
            classifica_resa(&prove, 5, &politica()),
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
        let VerdettoResa::NonResa { cause } = classifica_resa(&prove, 5, &politica()) else {
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

    /// I parametri della misura come li risolve il motore dal DB.
    fn parametri(modalita: ModalitaResa) -> ParametriMisura {
        ParametriMisura {
            minimo_elementi: 5,
            timeout_s: 30.0,
            attesa_ms: 2000,
            politica: politica(),
            modalita,
        }
    }

    /// Il criterio si costruisce dal produttore unico, e senza la radice degli
    /// indirizzi di anteprima non nasce.
    ///
    /// LA PAGINA NON E' NELLA SPEC, e il test lo afferma: era li' che il
    /// difetto dell'11/08 viveva — un indirizzo composto a t=0 e' l'indirizzo
    /// della pagina di ieri.
    #[test]
    fn il_criterio_nasce_solo_con_la_base_di_anteprima() {
        let p = parametri(ModalitaResa::Applica);
        assert!(criterio_resa(None, None, &p).is_none());
        assert!(criterio_resa(Some("  "), None, &p).is_none());

        let c = criterio_resa(Some("http://127.0.0.1:4000/"), None, &p).expect("criterio");
        assert_eq!(c.criterion_type, CRITERION_TYPE);
        // La barra finale non entra: l'indirizzo si compone al momento della
        // verifica, e due barre di fila non sono lo stesso percorso.
        assert_eq!(c.spec[CHIAVE_BASE_ANTEPRIMA], "http://127.0.0.1:4000");
        assert_eq!(c.spec[CHIAVE_MODALITA], "enforce");
        assert_eq!(c.spec[CHIAVE_MIN_ELEMENTI], 5);
        assert_eq!(c.spec[CHIAVE_ATTESA_MS], 2000);
        // La politica delle risorse viaggia nella spec: il runner non legge il DB.
        assert_eq!(c.spec[CHIAVE_TIPI_RISORSA][0], "image");
        assert_eq!(c.spec[CHIAVE_SOGLIA_RISORSE], 1.0);
        assert!(
            c.spec.get("url").is_none(),
            "la pagina si risolve alla verifica, non qui: una spec con l'URL \
             riporterebbe la risoluzione a t=0"
        );

        // Senza contenitore dichiarato la chiave non c'e' affatto: un
        // selettore vuoto nella spec farebbe cercare al browser un elemento
        // che nessuno ha chiesto.
        assert!(c.spec.get(CHIAVE_CONTENITORE).is_none());

        // Soglia non configurata: la chiave NON entra nella spec. Un numero di
        // ripiego scritto qui deciderebbe al posto dell'amministratore.
        let muta = criterio_resa(
            Some("http://x"),
            None,
            &ParametriMisura {
                politica: PoliticaRisorse::nuova(vec!["image".into()], None),
                ..parametri(ModalitaResa::Applica)
            },
        )
        .expect("criterio");
        assert!(muta.spec.get(CHIAVE_SOGLIA_RISORSE).is_none());
    }

    /// L'origine del servizio viaggia nella spec e NON impedisce al criterio di
    /// nascere: la precedenza la applica `pagina_del_run` insieme alla scelta
    /// della pagina, in un punto solo, e il criterio dichiara di non essersi
    /// applicato invece di sparire in silenzio.
    #[test]
    fn l_origine_del_servizio_viaggia_nella_spec() {
        let p = parametri(ModalitaResa::Osserva);
        let c = criterio_resa(Some("http://x"), Some("http://127.0.0.1:35954"), &p)
            .expect("con un servizio il criterio nasce lo stesso");
        assert_eq!(c.spec[CHIAVE_ORIGINE_SERVIZIO], "http://127.0.0.1:35954");

        // Origine degenere: la chiave non entra. «Assente» significa nessun
        // servizio dichiarato, non stringa vuota.
        let senza = criterio_resa(Some("http://x"), Some("  "), &p).expect("criterio");
        assert!(senza.spec.get(CHIAVE_ORIGINE_SERVIZIO).is_none());
    }

    /// La modalita' governa DUE cose distinte: se il criterio nasce, e se il
    /// suo verdetto negativo ha conseguenza. A `Off` non nasce affatto; a
    /// `Osserva` nasce e misura senza bocciare; ad `Applica` boccia.
    ///
    /// MUTAZIONE: far nascere il criterio anche a `Off` -> il kill-switch non
    /// spegnerebbe piu' nulla, e un progetto lo pagherebbe in tempo di browser
    /// a ogni chiusura di run.
    #[test]
    fn la_modalita_governa_nascita_e_conseguenza() {
        assert!(criterio_resa(Some("http://x"), None, &parametri(ModalitaResa::Off)).is_none());
        assert!(criterio_resa(Some("http://x"), None, &parametri(ModalitaResa::Osserva)).is_some());

        assert_eq!(ModalitaResa::try_parse("observe"), Some(ModalitaResa::Osserva));
        assert_eq!(ModalitaResa::try_parse(" ENFORCE "), Some(ModalitaResa::Applica));
        assert_eq!(ModalitaResa::try_parse("off"), Some(ModalitaResa::Off));
        // Valore ignoto: nessun ramo inventato, il chiamante degrada
        // dichiarandolo.
        assert_eq!(ModalitaResa::try_parse("osserva"), None);
        assert_eq!(ModalitaResa::default(), ModalitaResa::Off);

        assert!(!ModalitaResa::Off.nasce());
        assert!(ModalitaResa::Osserva.nasce() && !ModalitaResa::Osserva.boccia());
        assert!(ModalitaResa::Applica.boccia());
    }

    /// IL CASO MISURATO IL 09/08/2026: la pagina si genera, ha i suoi nodi, il
    /// contenitore e' pieno, nessuna eccezione — e tutte le sue immagini sono
    /// rotte. Con i soli tre segnali storici era `Resa`.
    ///
    /// MUTAZIONE: togliere il ramo delle risorse da `classifica_resa` (o
    /// portare la soglia oltre 1.0) -> questo test torna `Resa`, ed e'
    /// esattamente il verde che il gate ha dato all'app con sei `<img>` verso
    /// `via.placeholder.com`.
    #[test]
    fn le_immagini_tutte_rotte_bocciano_una_pagina_altrimenti_resa() {
        let pagina = "http://127.0.0.1:4000/preview/e4d446ce/index.html";
        let prove = ProveResa {
            contenitore: Some(EsitoContenitore::Trovato { figli: 6 }),
            origine: Some(pagina.to_string()),
            risorse: Some(
                (1..=6)
                    .map(|n| {
                        immagine(
                            &format!("https://via.placeholder.com/300x200?text=Prodotto+{n}"),
                            None,
                            "net::ERR_NAME_NOT_RESOLVED",
                        )
                    })
                    .collect(),
            ),
            ..resa(100)
        };

        // Coi soli tre segnali storici la pagina era resa: i nodi ci sono tutti.
        assert_eq!(
            classifica_resa(&prove, 5, &PoliticaRisorse::default()),
            VerdettoResa::Resa { elementi: 100 },
            "senza politica il criterio non risponde sulle risorse, e non deve inventare"
        );

        let VerdettoResa::NonResa { cause } = classifica_resa(&prove, 5, &politica()) else {
            panic!("una pagina senza una sola immagine non mostra il proprio contenuto");
        };
        assert_eq!(cause.len(), 1, "l'unico difetto sono le risorse");
        let CausaNonResa::RisorseNonCaricate { tipi, mancanti } = &cause[0] else {
            panic!("attesa la causa delle risorse: {:?}", cause[0]);
        };
        assert_eq!(tipi[0].tipo, "image");
        assert_eq!((tipi[0].falliti, tipi[0].osservati), (6, 6));
        assert_eq!(mancanti[0].provenienza, Provenienza::Esterna);
        let d = cause[0].descrizione();
        assert!(
            d.contains("image: 6 su 6") && d.contains("via.placeholder.com"),
            "il rilievo nomina il tipo e un URL: {d}"
        );

        // La stessa pagina con le immagini locali che arrivano: nessun rilievo.
        let sane = ProveResa {
            risorse: Some(
                (1..=6)
                    .map(|n| {
                        immagine(
                            &format!("http://127.0.0.1:4000/preview/e4d446ce/img/{n}.png"),
                            Some(200),
                            "",
                        )
                    })
                    .collect(),
            ),
            ..prove.clone()
        };
        assert_eq!(
            classifica_resa(&sane, 5, &politica()),
            VerdettoResa::Resa { elementi: 100 }
        );
    }

    /// Un'assenza SOTTO soglia non ferma nulla, e nemmeno un'osservazione muta:
    /// il quarto segnale e' additivo, e la sua assenza non declassa un verdetto
    /// che gli altri tre hanno gia' dato.
    ///
    /// MUTAZIONE: far entrare `AlcuneMancanti` fra le cause -> questo test cade,
    /// ed e' il falso rosso su una icona decorativa.
    #[test]
    fn una_risorsa_sparsa_e_l_ignoto_non_bocciano() {
        let pagina = "http://127.0.0.1:4000/preview/e4d446ce/index.html";
        let mut r: Vec<RisorsaOsservata> = (1..=5)
            .map(|n| {
                immagine(
                    &format!("http://127.0.0.1:4000/preview/e4d446ce/img/{n}.png"),
                    Some(200),
                    "",
                )
            })
            .collect();
        r.push(immagine(
            "http://127.0.0.1:4000/preview/e4d446ce/img/icona.svg",
            Some(404),
            "",
        ));
        let sparsa = ProveResa {
            origine: Some(pagina.to_string()),
            risorse: Some(r),
            ..resa(100)
        };
        assert_eq!(
            classifica_resa(&sparsa, 5, &politica()),
            VerdettoResa::Resa { elementi: 100 }
        );
        // ...ma il fatto e' registrato: e' il dato con cui si decidera' se
        // abbassare la soglia, e senza di esso si deciderebbe a intuito.
        assert!(matches!(
            risorse_della_pagina(&sparsa, &politica()),
            VerdettoRisorse::AlcuneMancanti { .. }
        ));

        // Osservazione che non ha riportato le risorse: la pagina resta resa e
        // il non-osservato lo dichiara il verdetto delle risorse, non il
        // silenzio.
        let muta = ProveResa { ..resa(100) };
        assert_eq!(
            classifica_resa(&muta, 5, &politica()),
            VerdettoResa::Resa { elementi: 100 }
        );
        assert!(matches!(
            risorse_della_pagina(&muta, &politica()),
            VerdettoRisorse::NonOsservabile { .. }
        ));
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
        let base = criterio_resa(Some("http://x"), None, &parametri(ModalitaResa::Applica))
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
