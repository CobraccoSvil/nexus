//! Punto unico (regola L) di «le RISORSE che la pagina referenzia sono
//! arrivate?».
//!
//! Quarto della famiglia che guarda un'app dal di fuori, e distinto dai tre
//! precedenti per la domanda, non per il meccanismo:
//!   - [`super::endpoint_probes`] chiede al SERVER se risponde;
//!   - [`super::browser_dialogue`] chiede se la pagina ottiene i propri DATI —
//!     le chiamate che il suo codice fa a un backend;
//!   - [`super::static_render`] chiede se il DOM e' stato GENERATO;
//!   - qui si chiede se cio' che il DOM referenzia — immagini, fogli di stile,
//!     script, media — sia effettivamente ARRIVATO.
//!
//! MISURATO il 09/08/2026. Lo stesso criterio della resa che aveva appena
//! funzionato — pagina bocciata a zero elementi, agente rimandato in
//! correzione, pagina ripassata — ha poi approvato un'app le cui SEI immagini
//! puntavano a `https://via.placeholder.com/300x200?text=Prodotto+N`, un
//! servizio esterno irraggiungibile, senza un solo file immagine nel progetto.
//! Non era un difetto di quel criterio: un'immagine rotta E' un elemento reso,
//! e contare i nodi del DOM e' esattamente cio' che quel criterio dichiara di
//! fare. Il buco stava nella copertura — la pagina aveva contenuto nel DOM e
//! nessun contenuto all'occhio.
//!
//! IL SEGNALE E' STRUTTURATO E GIA' RACCOLTO (regola M). Il browser dichiara
//! per ogni richiesta l'esito (`requestfailed` col proprio `errorText`,
//! oppure la risposta col proprio status) e il TIPO (`resourceType()`). Il
//! tipo non si deduce MAI dall'estensione dell'URL: `/api/thumb?id=3` e'
//! un'immagine e `/logo.png.txt` non lo e', e indovinare sposterebbe il
//! difetto dentro lo strumento di misura.
//!
//! NON E' UN BOOLEANO (regola Q). «Tutte le immagini rotte» e «un'icona
//! decorativa mancante» sono fatti diversi e vogliono conseguenze diverse: il
//! verdetto distingue il nulla di fatto, le assenze sparse e il TIPO
//! compromesso, e tiene separati i due modi in cui la misura non risponde
//! (nessuna risorsa dichiarata / non osservabile).
//!
//! LA CAUSA, dove e' strutturale, e' SEPARABILE e va separata: una risorsa
//! LOCALE che manca e' sempre un errore dell'app (il file non esiste al
//! percorso richiesto), una risorsa ESTERNA che non risponde puo' essere la
//! rete. Il discriminante e' l'ORIGINE dell'URL confrontata con quella della
//! pagina — un fatto, non un'euristica — e quando una delle due non e'
//! stabilibile la provenienza e' [`Provenienza::Indeterminata`], mai indovinata.
//! Le due cause NON producono verdetti diversi (a soglia raggiunta la pagina e'
//! rotta comunque, e chi la guarda non vede di chi sia la colpa): producono
//! RILIEVI diversi, perche' la correzione e' diversa.
//!
//! UN CASO CHE QUESTO MODULO NON DISTINGUE, e lo dice invece di indovinare: una
//! richiesta che il BROWSER ha annullato (`net::ERR_ABORTED` — succede a un
//! `<video>` il cui preload viene cancellato, o a una risorsa ancora in volo
//! quando la pagina viene chiusa) qui conta come non arrivata, mentre in verita'
//! il suo esito non si sa. Escluderla sarebbe corretto in linea di principio e
//! NON e' stato fatto: sui fatti raccolti finora quel codice non e' mai comparso
//! — lo script attende `networkidle` prima di misurare e chiude dopo, quindi le
//! richieste tagliate dalla chiusura non arrivano nemmeno al payload — e scrivere
//! il ramo prima di averlo visto significherebbe fissare un'ipotesi sulla forma
//! del dato invece di misurarla, che e' il modo in cui nascono le euristiche
//! inerti. Se un giorno un rilievo nominera' `net::ERR_ABORTED`, il posto dove
//! intervenire e' questo, e l'intervento e' toglierlo dal numeratore E dal
//! denominatore: un esito ignoto non e' un fallimento, e nemmeno un successo.
//!
//! CONFINE (regola L): qui SOLO il criterio puro sui fatti gia' raccolti.
//! L'I/O — aprire la pagina in Chromium e registrare le richieste — sta in
//! `mcp-core` (`agent_tools::browser_probe`), ed e' la STESSA singola
//! esecuzione che serve gli altri due interpreti: una pagina si apre una volta
//! sola, e chi ha domande diverse legge campi diversi degli stessi fatti.

use serde::{Deserialize, Serialize};

use super::browser_dialogue::{richiesta_fallita, RichiestaOsservata};

/// UNA risorsa sub-documento osservata dal browser.
///
/// E' la richiesta gia' modellata da [`RichiestaOsservata`] piu' il TIPO: si
/// riusa quella struttura invece di clonarne i campi perche' i due criteri
/// leggono gli stessi fatti dalla stessa osservazione, e due modelli della
/// stessa richiesta divergerebbero al primo campo aggiunto da un lato solo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RisorsaOsservata {
    pub richiesta: RichiestaOsservata,
    /// Il tipo DICHIARATO dal browser (`resourceType()`): `image`,
    /// `stylesheet`, `script`, `font`, `media`, ... `None` = il browser non
    /// l'ha dichiarato, che non e' «tipo sconosciuto» ma «non osservato».
    #[serde(default)]
    pub tipo: Option<String>,
}

/// Da dove veniva la risorsa che non e' arrivata. Vocabolario CHIUSO
/// (regola N), e la terza variante non e' un riempitivo: senza di essa
/// «non ho potuto stabilirlo» diventerebbe una delle altre due, cioe'
/// un'attribuzione di colpa inventata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenienza {
    /// Stessa origine della pagina: il file appartiene al progetto.
    Locale,
    /// Origine diversa: dipende da un servizio che il progetto non controlla.
    Esterna,
    /// La risorsa e' INCORPORATA nella pagina (`data:`, `blob:`): non viaggia
    /// sulla rete e non dipende da nessun servizio.
    ///
    /// Non e' un caso di `Indeterminata`: li' la provenienza non si e' potuta
    /// stabilire, qui si sa con certezza dove sta la risorsa, ed e' dentro il
    /// codice che l'ha scritta. E' anche la categoria piu' azionabile, perche'
    /// una incorporata che non rende e' sempre un difetto della pagina.
    Incorporata,
    /// Origine della pagina o della risorsa non stabilibile.
    Indeterminata,
}

impl Provenienza {
    /// Etichetta stabile per il consumatore macchina.
    pub fn key(&self) -> &'static str {
        match self {
            Self::Locale => "local",
            Self::Esterna => "external",
            Self::Incorporata => "embedded",
            Self::Indeterminata => "undetermined",
        }
    }
}

/// Che FORMA ha questo URL. Punto unico del riconoscimento dello schema: un
/// secondo test di schema altrove darebbe due idee diverse di «incorporata».
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormaUrl {
    /// `schema://autorita`, gia' normalizzata.
    ConAutorita(String),
    /// Il contenuto viaggia dentro l'URL stesso (`data:`, `blob:`).
    Incorporata,
    /// Nessuna delle due: relativa, vuota, `about:`, schema sconosciuto.
    NonRiconoscibile,
}

/// Gli schemi che portano il contenuto DENTRO l'URL. Chiusi per costruzione
/// (li fissa la specifica del browser), quindi nel codice e non in una tabella.
const SCHEMI_INCORPORATI: [&str; 2] = ["data:", "blob:"];

/// La forma di un URL, decisa in un posto solo.
pub fn forma_url(url: &str) -> FormaUrl {
    let url = url.trim();
    let minuscolo = url.to_ascii_lowercase();
    if SCHEMI_INCORPORATI.iter().any(|s| minuscolo.starts_with(s)) {
        return FormaUrl::Incorporata;
    }
    let Some((schema, resto)) = url.split_once("://") else {
        return FormaUrl::NonRiconoscibile;
    };
    if schema.is_empty() || schema.contains('/') {
        return FormaUrl::NonRiconoscibile;
    }
    let autorita = resto
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if autorita.is_empty() {
        return FormaUrl::NonRiconoscibile;
    }
    let schema = schema.to_ascii_lowercase();
    let autorita = match schema.as_str() {
        "http" => autorita.strip_suffix(":80").unwrap_or(&autorita).to_string(),
        "https" => autorita
            .strip_suffix(":443")
            .unwrap_or(&autorita)
            .to_string(),
        _ => autorita,
    };
    FormaUrl::ConAutorita(format!("{schema}://{autorita}"))
}

/// L'origine di un URL assoluto: `schema://autorita`, minuscola, con la porta
/// di default omessa.
///
/// La porta si normalizza perche' `http://host:80/x` e `http://host/x` sono la
/// stessa origine e differiscono nei byte: senza normalizzarla una risorsa
/// locale scritta nella forma esplicita risulterebbe ESTERNA, cioe' il criterio
/// attribuirebbe a un servizio di terzi un file del progetto.
///
/// `None` = non e' un URL assoluto con autorita' (`data:`, `blob:`, `about:`):
/// di quelle risorse non si puo' dire la provenienza, e non si finge.
pub fn origine_di(url: &str) -> Option<String> {
    match forma_url(url) {
        FormaUrl::ConAutorita(o) => Some(o),
        FormaUrl::Incorporata | FormaUrl::NonRiconoscibile => None,
    }
}

/// Locale, esterna o incorporata rispetto alla pagina che l'ha chiesta.
///
/// L'INCORPORATA si riconosce PRIMA di guardare l'origine della pagina: quel
/// fatto non dipende da dove sta la pagina, e metterlo dopo il ritorno
/// anticipato lo renderebbe irraggiungibile ogni volta che l'origine non e'
/// nota — cioe' proprio nei casi in cui serve di piu'.
pub fn provenienza(url_risorsa: &str, origine_pagina: Option<&str>) -> Provenienza {
    if forma_url(url_risorsa) == FormaUrl::Incorporata {
        return Provenienza::Incorporata;
    }
    let Some(pagina) = origine_pagina.and_then(origine_di) else {
        return Provenienza::Indeterminata;
    };
    match origine_di(url_risorsa) {
        Some(o) if o == pagina => Provenienza::Locale,
        Some(_) => Provenienza::Esterna,
        None => Provenienza::Indeterminata,
    }
}

/// Che ne e' stato della risorsa che un elemento della pagina porta.
///
/// Vocabolario CHIUSO (regola N) e distinto da quello della rete: qui la
/// domanda non e' «e' arrivata?» ma «si e' vista?», e per gli URL incorporati
/// le due risposte divergono — MISURATO il 10/08/2026: sei `img` con un data
/// URI troncato danno `complete: true, naturalWidth: 0` e ZERO eventi di rete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EsitoResa {
    /// Caricamento concluso e contenuto utilizzabile.
    Resa,
    /// Caricamento concluso e contenuto INUTILIZZABILE: arrivato e non
    /// decodificato, oppure mai partito.
    NonResa,
    /// Al momento dell'osservazione il caricamento non era concluso. NON e' un
    /// difetto: e' l'ignoto dichiarato (regola Q), e non entra nel numeratore.
    NonConclusa,
    /// L'elemento non dichiara alcuna sorgente: non porta risorse, quindi non
    /// puo' mancarne. Distinto da `NonResa` perche' `<img>` senza `src` e
    /// `<img>` rotta espongono la STESSA coppia `complete: true,
    /// naturalWidth: 0`, e senza la sorgente fra gli input sarebbero
    /// indistinguibili.
    SenzaRisorsa,
}

impl EsitoResa {
    /// Etichetta stabile per il consumatore macchina.
    pub fn key(&self) -> &'static str {
        match self {
            Self::Resa => "rendered",
            Self::NonResa => "not_rendered",
            Self::NonConclusa => "pending",
            Self::SenzaRisorsa => "no_source",
        }
    }
}

/// Il criterio sull'elemento: PURO, e prende la SORGENTE fra gli input perche'
/// senza di essa la quarta variante non sarebbe derivabile.
pub fn classifica_elemento(dichiara_sorgente: bool, concluso: bool, utilizzabile: bool) -> EsitoResa {
    if !dichiara_sorgente {
        return EsitoResa::SenzaRisorsa;
    }
    if !concluso {
        return EsitoResa::NonConclusa;
    }
    if utilizzabile {
        EsitoResa::Resa
    } else {
        EsitoResa::NonResa
    }
}

/// UN elemento della pagina che porta una risorsa, con l'esito della sua resa.
///
/// Fatto NUOVO accanto a [`RisorsaOsservata`] e non un suo caso particolare:
/// confonderli renderebbe indistinguibile «non e' arrivata» da «non si e'
/// vista», che sono due difetti con due correzioni diverse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElementoPortante {
    /// Lo stesso vocabolario dei tipi della rete (`image`, `media`, ...): un
    /// secondo vocabolario impedirebbe di unire i due canali per tipo.
    pub tipo: String,
    /// L'URL della sorgente effettivamente scelta dal browser.
    pub url: String,
    pub resa: EsitoResa,
}

/// QUALE canale ha visto il fallimento. Non e' un dettaglio di resoconto: da
/// esso dipende cosa si puo' AFFERMARE sulla rete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanaleFallito {
    /// La richiesta e' partita e non e' arrivata.
    Rete,
    /// Nessuna richiesta osservata: l'elemento che la porta non l'ha resa. Su
    /// un URL incorporato nessuna richiesta parte MAI, quindi qui parlare di
    /// «nessuna risposta ricevuta» sarebbe un'affermazione falsa su una rete
    /// che non e' stata interrogata.
    Elemento,
    /// Entrambi: la richiesta e' fallita E l'elemento non ha reso.
    Entrambi,
}

impl CanaleFallito {
    /// Etichetta stabile per il consumatore macchina.
    pub fn key(&self) -> &'static str {
        match self {
            Self::Rete => "network",
            Self::Elemento => "element",
            Self::Entrambi => "both",
        }
    }

    /// Il canale ha osservato una richiesta? Decide cosa si puo' dire dello
    /// status.
    fn ha_visto_la_rete(&self) -> bool {
        matches!(self, Self::Rete | Self::Entrambi)
    }
}

/// UNA risorsa che non e' arrivata, con tutto cio' che serve a correggerla.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RisorsaMancante {
    pub url: String,
    /// Il tipo dichiarato dal browser, quando c'e'.
    #[serde(default)]
    pub tipo: Option<String>,
    /// Lo status della risposta. `None` = nessuna risposta ricevuta.
    #[serde(default)]
    pub status: Option<u16>,
    /// Il motivo dichiarato dal browser quando non c'e' stata risposta.
    #[serde(default)]
    pub errore: String,
    pub provenienza: Provenienza,
    /// Da quale canale e' stato visto il fallimento.
    #[serde(default = "canale_di_rete")]
    pub canale: CanaleFallito,
}

/// Il canale storico: le righe scritte prima che il canale esistesse vengono
/// dalla sola rete, ed e' cio' che quel dato significava.
fn canale_di_rete() -> CanaleFallito {
    CanaleFallito::Rete
}

impl RisorsaMancante {
    /// La riga che l'agente legge. Composta DAI campi (regola Q punto 3).
    pub fn descrizione(&self) -> String {
        let tipo = self.tipo.as_deref().unwrap_or("risorsa");
        let motivo = match self.status {
            Some(s) => format!("HTTP {s}"),
            None if !self.errore.trim().is_empty() => self.errore.trim().to_string(),
            // Senza osservazione di rete non si afferma nulla sulla rete: il
            // fatto e' che l'elemento non ha mostrato la risorsa.
            None if !self.canale.ha_visto_la_rete() => {
                "arrivata ma non resa dall'elemento".to_string()
            }
            None => "nessuna risposta ricevuta".to_string(),
        };
        let dove = match self.provenienza {
            Provenienza::Locale => "locale",
            Provenienza::Esterna => "esterna",
            Provenienza::Incorporata => "incorporata nella pagina",
            Provenienza::Indeterminata => "origine non stabilita",
        };
        format!("{tipo} {} -> {motivo} ({dove})", self.url)
    }
}

/// Un TIPO di risorsa la cui quota fallita ha raggiunto la soglia: cio' che
/// quel tipo doveva mostrare, sulla pagina, non c'e'.
///
/// I conteggi per provenienza restano SEPARATI invece di collassare in una
/// provenienza «prevalente»: una pagina con tre immagini locali mancanti e tre
/// esterne irraggiungibili ha due difetti distinti da correggere, e sceglierne
/// uno come etichetta ne nasconderebbe l'altro.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TipoCompromesso {
    pub tipo: String,
    pub falliti: usize,
    pub osservati: usize,
    pub locali: usize,
    pub esterne: usize,
    #[serde(default)]
    pub incorporate: usize,
    pub indeterminate: usize,
}

impl TipoCompromesso {
    /// La riga che l'agente legge, con la CORREZIONE implicita nella causa.
    pub fn descrizione(&self) -> String {
        let mut s = format!(
            "{}: {} su {} non caricate",
            self.tipo, self.falliti, self.osservati
        );
        if self.locali > 0 {
            s.push_str(&format!(
                "; {} locali (il file non esiste al percorso richiesto)",
                self.locali
            ));
        }
        if self.esterne > 0 {
            s.push_str(&format!(
                "; {} da un dominio esterno che non risponde (il contenuto \
                 dell'app non puo' dipendere da un servizio irraggiungibile)",
                self.esterne
            ));
        }
        if self.incorporate > 0 {
            s.push_str(&format!(
                "; {} incorporate nella pagina che il browser non ha saputo \
                 mostrare (il difetto e' nel codice che le genera, non nella rete)",
                self.incorporate
            ));
        }
        if self.indeterminate > 0 {
            s.push_str(&format!(
                "; {} di origine non stabilita",
                self.indeterminate
            ));
        }
        s
    }
}

/// La politica che governa la misura, TUTTA dal DB (regola G): quali tipi di
/// risorsa contano e a quale quota di fallimenti un tipo si dice compromesso.
///
/// Nessuno dei due valori ha un ripiego nel codice. L'elenco vuoto e la soglia
/// assente non significano «giudica tutto» ne' «non giudicare niente»:
/// significano che la configurazione non c'e', e il criterio lo DICHIARA
/// invece di rispondere con numeri che nessuno ha scelto.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PoliticaRisorse {
    /// I tipi (nel vocabolario del browser) la cui assenza si vede guardando
    /// la pagina. Il `font` ne resta fuori per decisione, non per svista: un
    /// carattere che non carica lascia il testo in un ripiego, e la pagina
    /// mostra comunque il proprio contenuto.
    #[serde(default)]
    pub tipi_governati: Vec<String>,
    /// Quota di risorse fallite, RAPPORTATA AL TIPO, da cui il tipo si dice
    /// compromesso. `None` = non configurata.
    #[serde(default)]
    pub soglia: Option<f64>,
}

impl PoliticaRisorse {
    pub fn nuova(tipi_governati: Vec<String>, soglia: Option<f64>) -> Self {
        Self {
            tipi_governati,
            soglia,
        }
    }

    /// La politica basta a rispondere?
    pub fn e_utilizzabile(&self) -> bool {
        self.soglia.is_some()
            && self
                .tipi_governati
                .iter()
                .any(|t| !t.trim().is_empty())
    }

    /// I tipi governati, normalizzati e senza ripetizioni, in ordine
    /// deterministico.
    ///
    /// La deduplica non e' cosmetica: la configurazione e' una stringa CSV
    /// scritta a mano, e un `image,Image` conterebbe due volte gli stessi
    /// oggetti — cioe' dimezzerebbe la quota di un tipo compromesso proprio
    /// mentre l'amministratore crede di non aver cambiato nulla.
    fn tipi_normalizzati(&self) -> Vec<String> {
        let mut tipi: Vec<String> = self
            .tipi_governati
            .iter()
            .map(|t| t.trim().to_ascii_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        tipi.sort();
        tipi.dedup();
        tipi
    }
}

/// L'esito della misura. Cinque varianti, e le ultime due non sono dettagli:
/// sono cio' che impedisce a «la pagina non ha chiesto niente» e a «non ho
/// potuto guardare» di diventare «tutto a posto».
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdettoRisorse {
    /// Ogni risorsa governata e' arrivata.
    TutteCaricate { osservate: usize },
    /// Qualcuna non e' arrivata, ma nessun tipo raggiunge la soglia. E' un
    /// RILIEVO, non un difetto: entra nell'evidenza e non ferma nulla.
    AlcuneMancanti {
        mancanti: Vec<RisorsaMancante>,
        osservate: usize,
    },
    /// IL difetto: per almeno un tipo la quota fallita raggiunge la soglia.
    TipiCompromessi {
        tipi: Vec<TipoCompromesso>,
        mancanti: Vec<RisorsaMancante>,
    },
    /// La pagina non referenzia risorse di alcun tipo governato: non c'e'
    /// nulla di cui rispondere. Una pagina puo' essere completa e autosufficiente.
    NessunaDichiarata,
    /// La misura non e' stata possibile, col motivo dichiarato.
    NonOsservabile { motivo: String },
}

impl VerdettoRisorse {
    /// Etichetta stabile per il consumatore macchina.
    pub fn key(&self) -> &'static str {
        match self {
            Self::TutteCaricate { .. } => "all_loaded",
            Self::AlcuneMancanti { .. } => "some_missing",
            Self::TipiCompromessi { .. } => "type_compromised",
            Self::NessunaDichiarata => "none_declared",
            Self::NonOsservabile { .. } => "not_observable",
        }
    }

    /// Il verdetto vale un rilievo BLOCCANTE? Solo il difetto accertato: le
    /// altre varianti sono osservazioni o non-risposte, e trattarle come veti
    /// riporterebbe i rimandi a vuoto che questa famiglia di lenti evita.
    pub fn e_bloccante(&self) -> bool {
        matches!(self, Self::TipiCompromessi { .. })
    }

    /// L'evidenza per chi legge il gate, composta DAI campi (regola Q punto 3).
    ///
    /// Le assenze SOTTO soglia si riportano anche quando il verdetto passa: e'
    /// il dato con cui si decidera' un domani se abbassare la soglia, e senza
    /// di esso quella decisione si prenderebbe a intuito.
    pub fn evidenza(&self) -> serde_json::Value {
        use serde_json::{json, Map, Value};
        let mut m = Map::new();
        // Il verdetto c'e' SEMPRE, e si scrive una volta sola: e' il campo da
        // cui chi legge l'evidenza capisce quale delle cinque risposte ha in
        // mano, e ripeterlo per ramo lo renderebbe omissibile per distrazione.
        m.insert("verdict".to_string(), json!(self.key()));
        let mut aggiungi = |k: &str, v: Value| {
            m.insert(k.to_string(), v);
        };
        match self {
            Self::TutteCaricate { osservate } => aggiungi(K_OSSERVATE, json!(osservate)),
            Self::AlcuneMancanti {
                mancanti,
                osservate,
            } => {
                aggiungi(K_OSSERVATE, json!(osservate));
                aggiungi(K_MANCANTI, json!(descrizioni(mancanti)));
            }
            Self::TipiCompromessi { tipi, mancanti } => {
                let quali: Vec<String> = tipi.iter().map(TipoCompromesso::descrizione).collect();
                aggiungi("compromised_types", json!(quali));
                aggiungi(K_MANCANTI, json!(descrizioni(mancanti)));
            }
            Self::NessunaDichiarata => {}
            Self::NonOsservabile { motivo } => aggiungi("reason", json!(motivo)),
        }
        Value::Object(m)
    }
}

/// Chiavi dell'evidenza che compaiono in piu' rami, scritte in un posto solo:
/// due rami che nominassero diversamente lo stesso dato darebbero a chi legge
/// il gate due campi per la stessa cosa.
const K_OSSERVATE: &str = "observed";
const K_MANCANTI: &str = "missing";

/// Quante risorse mancanti si nominano: bastano a riconoscere il difetto senza
/// riversare l'intera pagina nel contesto di ogni turno.
pub const CAMPIONE_MANCANTI: usize = 8;

fn descrizioni(mancanti: &[RisorsaMancante]) -> Vec<String> {
    mancanti
        .iter()
        .take(CAMPIONE_MANCANTI)
        .map(RisorsaMancante::descrizione)
        .collect()
}

/// Il criterio: PURO, testabile senza browser.
///
/// `risorse` a `None` significa che l'osservazione non le ha riportate, che e'
/// diverso da una lista vuota: la seconda dice «la pagina non ha chiesto
/// nulla», la prima «non l'ho guardato», e collassarle darebbe un verde a chi
/// non e' stato misurato.
///
/// `origine_pagina` serve SOLO ad attribuire la provenienza, mai a decidere se
/// una risorsa sia fallita: senza di essa il verdetto resta lo stesso e i
/// rilievi dicono «origine non stabilita».
pub fn classifica_risorse(
    richieste: Option<&[RisorsaOsservata]>,
    elementi: Option<&[ElementoPortante]>,
    origine_pagina: Option<&str>,
    politica: &PoliticaRisorse,
) -> VerdettoRisorse {
    if let Some(v) = non_misurabile(richieste, elementi, politica) {
        return v;
    }
    let richieste = richieste.unwrap_or(&[]);
    let elementi = elementi.unwrap_or(&[]);

    let mut mancanti: Vec<RisorsaMancante> = Vec::new();
    let mut osservate = 0usize;
    let mut per_tipo: Vec<(String, usize, Vec<RisorsaMancante>)> = Vec::new();

    // Si itera sui TIPI GOVERNATI e non sulle richieste: filtrare le richieste
    // faceva uscire il criterio prima di contare, e su una pagina le cui
    // risorse sono tutte incorporate (zero eventi di rete) rispondeva «non
    // referenzia nulla» di una pagina che ne referenzia sei.
    for tipo in politica.tipi_normalizzati() {
        let oggetti = oggetti_del_tipo(&tipo, richieste, elementi, origine_pagina);
        if oggetti.totale == 0 {
            continue;
        }
        osservate += oggetti.totale;
        mancanti.extend(oggetti.mancanze.iter().cloned());
        per_tipo.push((tipo, oggetti.totale, oggetti.mancanze));
    }

    if osservate == 0 {
        return VerdettoRisorse::NessunaDichiarata;
    }
    if mancanti.is_empty() {
        return VerdettoRisorse::TutteCaricate { osservate };
    }

    let tipi = tipi_compromessi(&per_tipo, politica);
    if tipi.is_empty() {
        return VerdettoRisorse::AlcuneMancanti {
            mancanti,
            osservate,
        };
    }
    VerdettoRisorse::TipiCompromessi { tipi, mancanti }
}

/// I modi in cui la domanda NON ha risposta, raccolti in un posto solo perche'
/// e' la meta' che si sbaglia per omissione: chi la rifa' per conto proprio ne
/// dimentica uno, e quello diventa un verde. `None` = si puo' misurare.
fn non_misurabile(
    richieste: Option<&[RisorsaOsservata]>,
    elementi: Option<&[ElementoPortante]>,
    politica: &PoliticaRisorse,
) -> Option<VerdettoRisorse> {
    if !politica.e_utilizzabile() {
        return Some(VerdettoRisorse::NonOsservabile {
            motivo: "politica delle risorse non configurata: nessun tipo governato \
                     o soglia assente"
                .to_string(),
        });
    }
    // Nessuno dei due canali ha guardato: non si puo' dire nulla. Basta che UNO
    // dei due abbia riportato — anche una lista vuota e' un'osservazione.
    if richieste.is_none() && elementi.is_none() {
        return Some(VerdettoRisorse::NonOsservabile {
            motivo: "il browser non ha riportato ne' le richieste ne' gli elementi \
                     della pagina"
                .to_string(),
        });
    }
    // Il tipo lo dichiara il browser. Se ci sono richieste e NESSUNA lo porta,
    // quel canale non sa dire nulla; ma se il canale degli elementi ha
    // guardato, la misura resta possibile e non si dichiara cieca.
    let richieste_mute = richieste
        .is_some_and(|r| !r.is_empty() && r.iter().all(|x| x.tipo.is_none()));
    if richieste_mute && elementi.is_none_or(|e| e.is_empty()) {
        return Some(VerdettoRisorse::NonOsservabile {
            motivo: "il browser non ha dichiarato il tipo di alcuna risorsa".to_string(),
        });
    }
    None
}

/// Gli OGGETTI di un tipo, cioe' le cose che la pagina doveva mostrare, con
/// quelle che non ha mostrato.
struct OggettiDelTipo {
    totale: usize,
    mancanze: Vec<RisorsaMancante>,
}

/// L'unione dei due canali per UN tipo.
///
/// Quando il canale degli elementi ha visto qualcosa di questo tipo, sono gli
/// ELEMENTI a fare da denominatore: e' cio' che la pagina mostra, ed e' la
/// domanda. Il canale di rete vi contribuisce i fallimenti, appaiati per URL,
/// cosi' che un elemento ancora in caricamento la cui richiesta e' gia' fallita
/// non venga assolto dalla propria astensione. Dove gli elementi non arrivano
/// (`stylesheet`, `script`) il denominatore resta quello di rete: e' l'unico
/// che c'e', e dichiararlo assente sarebbe una regressione.
///
/// I due canali non si sommano MAI: una stessa immagine puo' comparire in
/// entrambi, e sommarli la conterebbe due volte.
fn oggetti_del_tipo(
    tipo: &str,
    richieste: &[RisorsaOsservata],
    elementi: &[ElementoPortante],
    origine_pagina: Option<&str>,
) -> OggettiDelTipo {
    let del_tipo: Vec<&ElementoPortante> = elementi
        .iter()
        .filter(|e| stesso_tipo(Some(&e.tipo), tipo))
        // Un elemento senza sorgente non porta risorse: non e' un'assenza.
        .filter(|e| e.resa != EsitoResa::SenzaRisorsa)
        .collect();

    let richieste_tipo: Vec<&RisorsaOsservata> = richieste
        .iter()
        .filter(|r| stesso_tipo(r.tipo.as_deref(), tipo))
        .collect();

    if del_tipo.is_empty() {
        // Nessun elemento di questo tipo: vale il solo canale di rete.
        let mancanze = richieste_tipo
            .iter()
            .filter(|r| richiesta_fallita(&r.richiesta))
            .map(|r| mancante_da_rete(r, origine_pagina))
            .collect();
        return OggettiDelTipo {
            totale: richieste_tipo.len(),
            mancanze,
        };
    }

    OggettiDelTipo {
        totale: del_tipo.len(),
        mancanze: mancanze_dagli_elementi(&del_tipo, &richieste_tipo, origine_pagina),
    }
}

/// Le mancanze quando il denominatore sono gli ELEMENTI: per ciascuno, l'esito
/// della sua resa piu' l'eventuale fallimento di rete appaiato per URL.
fn mancanze_dagli_elementi(
    del_tipo: &[&ElementoPortante],
    richieste_tipo: &[&RisorsaOsservata],
    origine_pagina: Option<&str>,
) -> Vec<RisorsaMancante> {
    del_tipo
        .iter()
        .filter_map(|e| {
            let rete = richieste_tipo
                .iter()
                .find(|r| r.richiesta.url == e.url && richiesta_fallita(&r.richiesta));
            match (e.resa, rete) {
                // La rete ha accertato un fallimento: l'astensione
                // dell'elemento non lo cancella.
                (_, Some(r)) => Some(RisorsaMancante {
                    canale: if e.resa == EsitoResa::NonResa {
                        CanaleFallito::Entrambi
                    } else {
                        CanaleFallito::Rete
                    },
                    ..mancante_da_rete(r, origine_pagina)
                }),
                (EsitoResa::NonResa, None) => Some(RisorsaMancante {
                    url: e.url.clone(),
                    tipo: Some(e.tipo.clone()),
                    status: None,
                    errore: String::new(),
                    provenienza: provenienza(&e.url, origine_pagina),
                    canale: CanaleFallito::Elemento,
                }),
                // Reso, oppure non concluso senza fallimento di rete: nel
                // secondo caso e' l'ignoto dichiarato, non un'assenza (regola Q).
                (EsitoResa::Resa, None)
                | (EsitoResa::NonConclusa, None)
                | (EsitoResa::SenzaRisorsa, None) => None,
            }
        })
        .collect()
}

/// Una mancanza vista dal canale di rete, con la provenienza attribuita.
///
/// L'attribuzione avviene QUI e non nei fatti: la provenienza e' un giudizio
/// (dipende da dove sta la pagina), e i fatti portano il solo URL.
fn mancante_da_rete(r: &RisorsaOsservata, origine_pagina: Option<&str>) -> RisorsaMancante {
    RisorsaMancante {
        url: r.richiesta.url.clone(),
        tipo: r.tipo.clone(),
        status: r.richiesta.status,
        errore: r.richiesta.errore.clone(),
        provenienza: provenienza(&r.richiesta.url, origine_pagina),
        canale: CanaleFallito::Rete,
    }
}

/// I tipi che raggiungono la soglia, in ordine deterministico (per nome): due
/// esecuzioni sugli stessi fatti devono produrre lo stesso rilievo, o il
/// confronto fra due giri diventa impossibile.
fn tipi_compromessi(
    per_tipo: &[(String, usize, Vec<RisorsaMancante>)],
    politica: &PoliticaRisorse,
) -> Vec<TipoCompromesso> {
    let soglia = politica.soglia.unwrap_or_default();
    let mut fuori: Vec<TipoCompromesso> = per_tipo
        .iter()
        .filter_map(|(tipo, osservati, mancanze)| {
            let falliti = mancanze.len();
            if falliti == 0 || *osservati == 0 {
                return None;
            }
            if (falliti as f64) / (*osservati as f64) < soglia {
                return None;
            }
            let riferimenti: Vec<&RisorsaMancante> = mancanze.iter().collect();
            Some(TipoCompromesso {
                tipo: tipo.clone(),
                falliti,
                osservati: *osservati,
                locali: conta(&riferimenti, Provenienza::Locale),
                esterne: conta(&riferimenti, Provenienza::Esterna),
                incorporate: conta(&riferimenti, Provenienza::Incorporata),
                indeterminate: conta(&riferimenti, Provenienza::Indeterminata),
            })
        })
        .collect();
    fuori.sort_by(|a, b| a.tipo.cmp(&b.tipo));
    fuori
}

fn stesso_tipo(tipo: Option<&str>, atteso: &str) -> bool {
    tipo.is_some_and(|t| t.trim().eq_ignore_ascii_case(atteso))
}

fn conta(mancanti: &[&RisorsaMancante], p: Provenienza) -> usize {
    mancanti.iter().filter(|m| m.provenienza == p).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn politica() -> PoliticaRisorse {
        PoliticaRisorse::nuova(
            vec!["image".into(), "stylesheet".into(), "script".into()],
            Some(1.0),
        )
    }

    fn risorsa(url: &str, tipo: &str, status: Option<u16>, errore: &str) -> RisorsaOsservata {
        RisorsaOsservata {
            richiesta: RichiestaOsservata {
                url: url.into(),
                status,
                errore: errore.into(),
            },
            tipo: Some(tipo.into()),
        }
    }

    const PAGINA: &str = "http://127.0.0.1:4000/preview/e4d446ce/index.html";

    /// L'INCIDENTE, nella sua forma esatta: sei immagini verso un servizio
    /// esterno irraggiungibile, e nient'altro di rotto.
    ///
    /// MUTAZIONE: portare la soglia a un valore irraggiungibile (`>1.0`) o
    /// togliere `image` dai tipi governati -> il verdetto torna
    /// `AlcuneMancanti`/`NessunaDichiarata` e non blocca piu' nulla: e'
    /// esattamente il verde che il gate ha dato il 09/08/2026.
    #[test]
    fn sei_immagini_rotte_compromettono_il_tipo() {
        let mut r: Vec<RisorsaOsservata> = (1..=6)
            .map(|n| {
                risorsa(
                    &format!("https://via.placeholder.com/300x200?text=Prodotto+{n}"),
                    "image",
                    None,
                    "net::ERR_NAME_NOT_RESOLVED",
                )
            })
            .collect();
        r.push(risorsa(
            "http://127.0.0.1:4000/preview/e4d446ce/style.css",
            "stylesheet",
            Some(200),
            "",
        ));

        let v = classifica_risorse(Some(&r), None, Some(PAGINA), &politica());
        assert!(v.e_bloccante(), "sei immagini su sei rotte: {v:?}");
        let VerdettoRisorse::TipiCompromessi { tipi, mancanti } = &v else {
            panic!("atteso un tipo compromesso: {v:?}");
        };
        assert_eq!(tipi.len(), 1, "solo le immagini: il foglio e' arrivato");
        assert_eq!(tipi[0].tipo, "image");
        assert_eq!((tipi[0].falliti, tipi[0].osservati), (6, 6));
        assert_eq!(
            (tipi[0].esterne, tipi[0].locali),
            (6, 0),
            "il dominio non e' quello della pagina"
        );
        assert_eq!(mancanti.len(), 6);
        assert!(
            tipi[0].descrizione().contains("dominio esterno"),
            "il rilievo dice cosa correggere: {}",
            tipi[0].descrizione()
        );

        // La stessa prova, con la soglia irraggiungibile: nessun blocco. E' la
        // dimostrazione che a decidere e' la configurazione, non il codice.
        let lasca = PoliticaRisorse::nuova(vec!["image".into()], Some(1.5));
        assert!(!classifica_risorse(Some(&r), None, Some(PAGINA), &lasca).e_bloccante());
    }

    /// Un'icona decorativa mancante NON e' la stessa cosa: si riporta e non
    /// blocca. MUTAZIONE: far bloccare `AlcuneMancanti` -> questo test cade, ed
    /// e' il falso rosso che riporterebbe i rimandi a vuoto.
    #[test]
    fn una_sola_icona_mancante_si_riporta_e_non_blocca() {
        let mut r: Vec<RisorsaOsservata> = (1..=5)
            .map(|n| {
                risorsa(
                    &format!("http://127.0.0.1:4000/preview/e4d446ce/img/{n}.png"),
                    "image",
                    Some(200),
                    "",
                )
            })
            .collect();
        r.push(risorsa(
            "http://127.0.0.1:4000/preview/e4d446ce/img/icona.svg",
            "image",
            Some(404),
            "",
        ));

        let v = classifica_risorse(Some(&r), None, Some(PAGINA), &politica());
        assert!(!v.e_bloccante());
        let VerdettoRisorse::AlcuneMancanti {
            mancanti,
            osservate,
        } = &v
        else {
            panic!("una su sei si riporta: {v:?}");
        };
        assert_eq!((*osservate, mancanti.len()), (6, 1));
        assert_eq!(mancanti[0].provenienza, Provenienza::Locale);
        assert!(
            mancanti[0].descrizione().contains("HTTP 404")
                && mancanti[0].descrizione().contains("locale"),
            "{}",
            mancanti[0].descrizione()
        );
        // E l'evidenza la riporta comunque: e' il dato con cui si decidera' se
        // abbassare la soglia.
        assert!(v.evidenza()["missing"].as_array().is_some_and(|a| a.len() == 1));
    }

    /// La CAUSA e' separata perche' la correzione e' diversa: stesso numero di
    /// fallimenti, due rilievi che mandano in due posti diversi.
    #[test]
    fn locale_ed_esterna_sono_due_cause_distinte() {
        let locali = vec![
            risorsa(
                "http://127.0.0.1:4000/preview/e4d446ce/img/a.png",
                "image",
                Some(404),
                "",
            ),
            risorsa(
                "http://127.0.0.1:4000/preview/e4d446ce/img/b.png",
                "image",
                Some(404),
                "",
            ),
        ];
        let VerdettoRisorse::TipiCompromessi { tipi, .. } =
            classifica_risorse(Some(&locali), None, Some(PAGINA), &politica())
        else {
            panic!("due su due e' il tipo compromesso");
        };
        assert_eq!((tipi[0].locali, tipi[0].esterne), (2, 0));
        assert!(tipi[0].descrizione().contains("non esiste al percorso"));

        // Senza sapere dove sta la pagina non si attribuisce nulla: il verdetto
        // resta, la colpa no.
        let VerdettoRisorse::TipiCompromessi { tipi, .. } =
            classifica_risorse(Some(&locali), None, None, &politica())
        else {
            panic!("il verdetto non dipende dall'origine della pagina");
        };
        assert_eq!(
            (tipi[0].locali, tipi[0].esterne, tipi[0].indeterminate),
            (0, 0, 2)
        );
    }

    /// La porta di default non cambia l'origine: senza normalizzarla un file
    /// del progetto scritto in forma esplicita sarebbe attribuito a terzi.
    #[test]
    fn l_origine_normalizza_la_porta_di_default() {
        assert_eq!(origine_di("http://host:80/a/b"), origine_di("http://host/a"));
        assert_eq!(
            origine_di("https://Host:443/a?x=1"),
            Some("https://host".to_string())
        );
        assert_eq!(origine_di("http://h:3000/a"), Some("http://h:3000".into()));
        // Non sono URL con autorita': `origine_di` non ne estrae nessuna.
        assert_eq!(origine_di("data:image/png;base64,AAAA"), None);
        assert_eq!(origine_di("/img/a.png"), None);
        // Ma non avere un'autorita' e non avere una provenienza sono due cose
        // diverse. Di un data URI si SA dove sta la risorsa — dentro la pagina
        // — e da quando il canale degli elementi la puo' vedere fallire, quella
        // distinzione decide anche il RILIEVO: «incorporata che il browser non
        // ha saputo mostrare» manda a correggere il codice che la genera,
        // «origine non stabilita» non manda da nessuna parte. Un URL relativo
        // resta indeterminato: li' l'origine dipende dalla pagina e non e'
        // ricostruibile dal solo URL.
        assert_eq!(
            provenienza("data:image/png;base64,AA", Some(PAGINA)),
            Provenienza::Incorporata
        );
        assert_eq!(
            provenienza("/img/a.png", Some(PAGINA)),
            Provenienza::Indeterminata
        );
    }

    /// «Non ho potuto guardare» non diventa «va bene», e i tre modi in cui la
    /// misura non risponde restano distinti.
    ///
    /// MUTAZIONE: far ritornare `TutteCaricate` su `risorse: None` -> cade qui,
    /// e col difetto reale (un'osservazione muta che assolve la pagina).
    #[test]
    fn l_ignoto_non_degrada_a_successo() {
        // Nessuna osservazione delle richieste.
        let v = classifica_risorse(None, None, Some(PAGINA), &politica());
        assert!(matches!(v, VerdettoRisorse::NonOsservabile { .. }));
        assert!(!v.e_bloccante());

        // Politica non configurata: il criterio non risponde, non inventa.
        let vuota = PoliticaRisorse::default();
        let VerdettoRisorse::NonOsservabile { motivo } =
            classifica_risorse(Some(&[]), None, Some(PAGINA), &vuota)
        else {
            panic!("senza vocabolario e senza soglia non si giudica");
        };
        assert!(motivo.contains("non configurata"), "{motivo}");
        // Anche con i tipi ma senza soglia: mezza configurazione non e' una
        // configurazione.
        let mezza = PoliticaRisorse::nuova(vec!["image".into()], None);
        assert!(matches!(
            classifica_risorse(Some(&[]), None, Some(PAGINA), &mezza),
            VerdettoRisorse::NonOsservabile { .. }
        ));

        // Tipo mai dichiarato dal browser: non e' «nessuna risorsa governata».
        let senza_tipo = vec![RisorsaOsservata {
            richiesta: RichiestaOsservata {
                url: "http://127.0.0.1:4000/x.png".into(),
                status: None,
                errore: "net::ERR_FAILED".into(),
            },
            tipo: None,
        }];
        let VerdettoRisorse::NonOsservabile { motivo } =
            classifica_risorse(Some(&senza_tipo), None, Some(PAGINA), &politica())
        else {
            panic!("senza tipo dichiarato non si classifica");
        };
        assert!(motivo.contains("tipo"), "{motivo}");
    }

    /// Una pagina che non chiede risorse governate e' completa, non rotta: il
    /// verdetto lo dice e non blocca.
    #[test]
    fn nessuna_risorsa_governata_non_e_un_difetto() {
        assert_eq!(
            classifica_risorse(Some(&[]), None, Some(PAGINA), &politica()),
            VerdettoRisorse::NessunaDichiarata
        );
        // Un font rotto non entra nella misura: il testo resta leggibile in un
        // ripiego, e bocciare li' e' il rimando a vuoto che si evita apposta.
        let font = vec![risorsa(
            "https://fonts.gstatic.com/s/x.woff2",
            "font",
            None,
            "net::ERR_INTERNET_DISCONNECTED",
        )];
        assert_eq!(
            classifica_risorse(Some(&font), None, Some(PAGINA), &politica()),
            VerdettoRisorse::NessunaDichiarata
        );
    }

    /// La soglia e' PER TIPO, non sul totale: cinque script su cinque a posto
    /// non assolvono le due immagini su due che mancano.
    #[test]
    fn la_soglia_e_per_tipo_non_sul_totale() {
        let mut r: Vec<RisorsaOsservata> = (1..=5)
            .map(|n| {
                risorsa(
                    &format!("http://127.0.0.1:4000/preview/e4d446ce/js/{n}.js"),
                    "script",
                    Some(200),
                    "",
                )
            })
            .collect();
        r.push(risorsa(
            "http://127.0.0.1:4000/preview/e4d446ce/a.png",
            "image",
            Some(404),
            "",
        ));
        r.push(risorsa(
            "http://127.0.0.1:4000/preview/e4d446ce/b.png",
            "image",
            Some(500),
            "",
        ));

        let VerdettoRisorse::TipiCompromessi { tipi, .. } =
            classifica_risorse(Some(&r), None, Some(PAGINA), &politica())
        else {
            panic!("2 su 2 immagini e' un tipo compromesso anche con 5 script sani");
        };
        assert_eq!(tipi.len(), 1);
        assert_eq!(tipi[0].tipo, "image");
    }

    fn elemento(url: &str, resa: EsitoResa) -> ElementoPortante {
        ElementoPortante {
            tipo: "image".to_string(),
            url: url.to_string(),
            resa,
        }
    }

    /// IL CASO MISURATO il 10/08/2026 su vetrina-statica, dopo che l'agente
    /// aveva "corretto" gli URL esterni in data URI: sei `img` con lo stesso
    /// data URI troncato, `complete: true, naturalWidth: 0`, e ZERO eventi di
    /// rete — Chromium non ne emette per gli URL incorporati.
    ///
    /// Prima di questo canale il verdetto era `NessunaDichiarata`, cioe' «la
    /// pagina non referenzia risorse di alcun tipo governato», detto di una
    /// pagina che ne referenzia sei e non ne mostra nessuna.
    ///
    /// MUTAZIONE: passare `None` come canale degli elementi (cioe' tornare a
    /// guardare la sola rete) -> `NessunaDichiarata`, e il test rosseggia col
    /// valore esatto del difetto.
    #[test]
    fn sei_immagini_incorporate_che_non_rendono_compromettono_il_tipo() {
        const ROTTO: &str = "data:image/svg+xml;utf8,<svg xmlns=";
        let elementi: Vec<ElementoPortante> = (0..6)
            .map(|_| elemento(ROTTO, EsitoResa::NonResa))
            .collect();

        let v = classifica_risorse(Some(&[]), Some(&elementi), Some(PAGINA), &politica());

        let VerdettoRisorse::TipiCompromessi { tipi, mancanti } = &v else {
            panic!("sei immagini su sei che non rendono compromettono il tipo, invece: {v:?}");
        };
        assert!(v.e_bloccante());
        assert_eq!(tipi.len(), 1);
        assert_eq!(tipi[0].tipo, "image");
        assert_eq!(
            (tipi[0].falliti, tipi[0].osservati),
            (6, 6),
            "il denominatore sono gli ELEMENTI, non le richieste: quelle sono zero"
        );
        assert_eq!(
            tipi[0].incorporate, 6,
            "incorporate, non indeterminate: la provenienza di un data URI si sa"
        );
        assert_eq!(mancanti.len(), 6);
        assert_eq!(mancanti[0].canale, CanaleFallito::Elemento);
        assert!(
            mancanti[0].descrizione().contains("non resa dall'elemento"),
            "senza osservazione di rete non si afferma nulla sulla rete: {}",
            mancanti[0].descrizione()
        );
    }

    /// La deduplica per URL non deve tornare dalla finestra: sei elementi che
    /// condividono lo stesso src sono sei cose che l'utente non vede, mentre il
    /// canale di rete ne conterebbe UNA sola.
    ///
    /// MUTAZIONE: appaiare gli elementi per URL prima di contarli -> osservati
    /// e falliti scendono a 1 e l'asserzione sui sei rosseggia.
    #[test]
    fn elementi_con_lo_stesso_url_restano_sei_oggetti() {
        let elementi: Vec<ElementoPortante> = (0..6)
            .map(|_| elemento("data:image/png;base64,AA", EsitoResa::NonResa))
            .collect();
        let VerdettoRisorse::TipiCompromessi { tipi, .. } =
            classifica_risorse(Some(&[]), Some(&elementi), Some(PAGINA), &politica())
        else {
            panic!("sei elementi identici e tutti non resi compromettono il tipo");
        };
        assert_eq!((tipi[0].falliti, tipi[0].osservati), (6, 6));
    }

    /// Un caricamento non concluso NON e' un'assenza (regola Q): entra nel
    /// denominatore perche' la pagina quella risorsa la referenzia, ma non nel
    /// numeratore, perche' nessuno ha ancora visto come e' andata.
    ///
    /// MUTAZIONE: contare `NonConclusa` fra le mancanze -> il verdetto diventa
    /// `TipiCompromessi` e il test rosseggia.
    #[test]
    fn il_caricamento_non_concluso_non_e_una_mancanza() {
        let elementi = vec![
            elemento("https://cdn.example/a.png", EsitoResa::NonConclusa),
            elemento("https://cdn.example/b.png", EsitoResa::Resa),
        ];
        assert_eq!(
            classifica_risorse(Some(&[]), Some(&elementi), Some(PAGINA), &politica()),
            VerdettoRisorse::TutteCaricate { osservate: 2 },
            "l'ignoto non e' un fallimento, ma la risorsa resta osservata"
        );
    }

    /// Un `img` senza sorgente non porta risorse, quindi non puo' mancarne:
    /// esce dal denominatore. Senza la sorgente fra gli input sarebbe
    /// indistinguibile da una rotta, perche' entrambe danno
    /// `complete: true, naturalWidth: 0`.
    ///
    /// MUTAZIONE: derivare l'esito dai soli `concluso`/`utilizzabile` ->
    /// l'elemento vuoto diventa `NonResa`, il tipo risulta compromesso 1 su 1
    /// e il test rosseggia.
    #[test]
    fn un_elemento_senza_sorgente_non_e_una_risorsa_mancante() {
        assert_eq!(
            classifica_elemento(false, true, false),
            EsitoResa::SenzaRisorsa
        );
        let elementi = vec![elemento("", EsitoResa::SenzaRisorsa)];
        assert_eq!(
            classifica_risorse(Some(&[]), Some(&elementi), Some(PAGINA), &politica()),
            VerdettoRisorse::NessunaDichiarata
        );
    }

    /// Un fallimento di rete ACCERTATO non viene cancellato dall'astensione
    /// dell'elemento che lo porta: l'elemento e' ancora in caricamento perche'
    /// la richiesta e' fallita, non nonostante.
    ///
    /// MUTAZIONE: decidere sul solo `e.resa` -> l'oggetto esce dalle mancanze e
    /// il verdetto torna `TutteCaricate`.
    #[test]
    fn la_rete_fallita_vince_sull_astensione_dell_elemento() {
        const U: &str = "https://cdn.example/rotta.png";
        let richieste = vec![risorsa(U, "image", None, "net::ERR_CONNECTION_CLOSED")];
        let elementi = vec![elemento(U, EsitoResa::NonConclusa)];
        let VerdettoRisorse::TipiCompromessi { mancanti, .. } = classifica_risorse(
            Some(&richieste),
            Some(&elementi),
            Some(PAGINA),
            &politica(),
        ) else {
            panic!("una richiesta fallita resta una mancanza anche se l'elemento si astiene");
        };
        assert_eq!(mancanti.len(), 1);
        assert_eq!(mancanti[0].canale, CanaleFallito::Rete);
        assert_eq!(
            mancanti[0].errore, "net::ERR_CONNECTION_CLOSED",
            "l'errore osservato dalla rete non si perde"
        );
    }

    /// I due canali non si sommano: la stessa immagine vista da entrambi resta
    /// UN oggetto, e il canale lo dichiara.
    ///
    /// MUTAZIONE: concatenare le mancanze dei due canali -> falliti diventa 2
    /// su 1 osservato, cioe' una quota maggiore di uno.
    #[test]
    fn i_due_canali_non_contano_due_volte_la_stessa_risorsa() {
        const U: &str = "http://127.0.0.1:4000/preview/e4d446ce/logo.png";
        let richieste = vec![risorsa(U, "image", Some(404), "")];
        let elementi = vec![elemento(U, EsitoResa::NonResa)];
        let VerdettoRisorse::TipiCompromessi { tipi, mancanti } = classifica_risorse(
            Some(&richieste),
            Some(&elementi),
            Some(PAGINA),
            &politica(),
        ) else {
            panic!("un'immagine locale mancante compromette il tipo");
        };
        assert_eq!((tipi[0].falliti, tipi[0].osservati), (1, 1));
        assert_eq!(mancanti.len(), 1);
        assert_eq!(mancanti[0].canale, CanaleFallito::Entrambi);
        assert_eq!(mancanti[0].provenienza, Provenienza::Locale);
    }

    /// Dove il canale degli elementi non arriva (`stylesheet`, `script`) il
    /// denominatore resta quello di rete: toglierlo sarebbe una regressione.
    ///
    /// MUTAZIONE: usare gli elementi come unico denominatore -> il foglio di
    /// stile non viene piu' contato e il verdetto torna `NessunaDichiarata`.
    #[test]
    fn i_tipi_senza_canale_di_elemento_restano_giudicati_dalla_rete() {
        let richieste = vec![risorsa(
            "http://127.0.0.1:4000/preview/e4d446ce/style.css",
            "stylesheet",
            Some(404),
            "",
        )];
        let elementi = vec![elemento("data:image/png;base64,AA", EsitoResa::Resa)];
        let VerdettoRisorse::TipiCompromessi { tipi, .. } = classifica_risorse(
            Some(&richieste),
            Some(&elementi),
            Some(PAGINA),
            &politica(),
        ) else {
            panic!("un foglio di stile su uno che manca compromette il suo tipo");
        };
        assert_eq!(tipi.len(), 1);
        assert_eq!(tipi[0].tipo, "stylesheet");
    }

    /// Nessuno dei due canali ha guardato: non si dichiara un verde.
    ///
    /// MUTAZIONE: trattare `None` come lista vuota -> il verdetto diventa
    /// `NessunaDichiarata`, cioe' «pagina autosufficiente» detto di una pagina
    /// mai osservata.
    #[test]
    fn senza_alcun_canale_la_misura_non_risponde() {
        let v = classifica_risorse(None, None, Some(PAGINA), &politica());
        assert!(matches!(v, VerdettoRisorse::NonOsservabile { .. }));
        assert!(!v.e_bloccante());
    }

    /// La provenienza di un URL incorporato si SA, e non e' «indeterminata»:
    /// il riconoscimento precede il controllo sull'origine della pagina, o
    /// resterebbe irraggiungibile proprio quando l'origine non e' nota.
    ///
    /// MUTAZIONE: rimettere il controllo dell'incorporata dopo il ritorno
    /// anticipato su `origine_pagina` -> il secondo caso torna
    /// `Indeterminata`.
    #[test]
    fn l_incorporata_si_riconosce_anche_senza_origine_della_pagina() {
        assert_eq!(
            provenienza("data:image/svg+xml;utf8,<svg", Some(PAGINA)),
            Provenienza::Incorporata
        );
        assert_eq!(
            provenienza("data:image/svg+xml;utf8,<svg", None),
            Provenienza::Incorporata
        );
        assert_eq!(forma_url("blob:http://x/y"), FormaUrl::Incorporata);
        assert_eq!(origine_di("data:image/png;base64,AA"), None);
    }
}
