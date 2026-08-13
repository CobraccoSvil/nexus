//! «Di che cosa parla questo errore fornitore?» — il punto unico (regola L)
//! della classificazione STRUTTURALE, al posto del confronto per sottostringa.
//!
//! ## Il difetto che questo modulo chiude
//!
//! Il `contains()` era la meta' visibile. La meta' che decideva stava PRIMA,
//! nell'estrattore: `extract_structured_error_code` valutava sei puntatori JSON
//! e ritornava il PRIMO PRESENTE, `Option<String>`. Da quel punto in poi gli
//! altri cinque non esistevano piu'.
//!
//! MISURATO il 13/08/2026 su `nexus_provider_health_history`: openai risponde al
//! credito esaurito con
//!
//! ```text
//! 429 {"type":"insufficient_quota","code":"credit_balance_exhausted"}
//! ```
//!
//! Il primo campo presente e' `code`, che non contiene "quota": il vocabolario
//! di sottostringhe non lo riconosceva, e `insufficient_quota` — che ne faceva
//! parte — era gia' stato buttato via quando la decisione cominciava. Restava lo
//! status: 429 -> Transient. **4439 righe dal 30/07 al 13/08**, contro **1960
//! righe corrette** (code = `insufficient_quota`) dal 06/07 al 30/07. Gli
//! intervalli sono DISGIUNTI: non e' oscillazione, e' una regressione con una
//! data. openai non ha smesso di dichiarare il credito — e' il `code` NUOVO, che
//! ha la precedenza, a oscurare il `type` rimasto giusto.
//!
//! Ne segue la forma del rimedio, che e' strutturale e non lessicale:
//!
//! > **L'estrattore smette di scegliere. Decide il primo candidato
//! > RICONOSCIUTO, non il primo PRESENTE.**
//!
//! Questa sola regola chiude il caso openai anche senza la riga che lo nomina
//! (il `type` e' dichiarato), conserva groq (il `code` dichiarato vince, il
//! `type` "tokens" e' ambiguo e non compete) e trasforma perplexity da
//! corretto-per-accidente — il suo `code` e' NUMERICO e viene scartato da
//! `as_str()` — a corretto per regola.
//!
//! ## La divisione fra codice e DB
//!
//! | cosa | dove | perche' |
//! |---|---|---|
//! | vocabolario delle CAUSE + proiezione sulle 4 classi | codice ([`CausaErrore`]) | il significato e' nostro: una `UPDATE` non deve poter inventare una classe |
//! | assegnazione `(fornitore, valore) -> causa` | DB (mig 0705) | e' la parte che cambia quando cambia un fornitore, e cambia senza preavviso |
//!
//! ## La precedenza fra i campi cambia natura
//!
//! L'ordine `code` prima di `type` RESTA (fu introdotto per groq il 16/07 ed e'
//! giusto: dove un fornitore valorizza entrambi, `code` e' l'identificatore e
//! `type` la categoria). Ma oggi era l'UNICO meccanismo, e per questo era anche
//! l'unico modo di sbagliare. Ora e' uno SPAREGGIO fra candidati entrambi
//! riconosciuti.
//!
//! ### Perche' la precedenza NON diventa una tabella per fornitore
//!
//! MISURATO il 13/08/2026 inviando a tutti e nove i fornitori una chiave non
//! valida — stesso evento semantico, nove forme diverse — e riprodotto due volte
//! in modo indipendente: **openai e deepseek usano lo stesso envelope con i
//! RUOLI INVERTITI**. openai mette lo specifico in `code` (`invalid_api_key`) e
//! il generico in `type` (`invalid_request_error`); deepseek fa l'esatto
//! contrario (`type: authentication_error`, `code: invalid_request_error`).
//! Ne segue, correttamente, che *non esiste un ordine di campi giusto per
//! entrambi* — ma solo finche' e' il RANGO a selezionare.
//!
//! Qui il rango non seleziona: seleziona il RICONOSCIMENTO. Il caso deepseek si
//! chiude con una riga di catalogo — `('deepseek','invalid_request_error',401)
//! -> auth_denied` — cioe' con lo strumento che il catalogo ha gia' (lo status
//! nella chiave, che serviva gia' per il 402 vs 400 dello stesso fornitore).
//! Una tabella di precedenze per fornitore sarebbe un SECONDO elenco da tenere
//! allineato al primo, e la regola L la vieta per la ragione esatta per cui
//! questo modulo esiste: quando la stessa domanda ha due posti dove essere
//! risposta, i due divergono in silenzio.
//!
//! Lo stesso vale per il PERCORSO del campo: non si dichiara per fornitore, si
//! AGGIUNGE al vocabolario dei campi osservati. Un percorso che un fornitore non
//! valorizza non produce candidati e non costa nulla agli altri — e' cosi' che
//! `metadata.limit_source` (openrouter) e `details[].reason` (google) convivono
//! con `code`/`type` senza che nessuno debba dichiarare chi guarda dove.
//!
//! Due forme restano SENZA candidati, e va bene cosi': mistral risponde alla
//! chiave non valida con `{"detail":"Invalid API Key"}` — nessun envelope, solo
//! prosa — e openrouter con un `code` INTERO. In entrambi i casi non c'e' un
//! identificatore macchina da leggere: decide lo status, che e' il minimo comun
//! denominatore affidabile, e la prosa NON entra nel vocabolario (regola M).
//!
//! ### Da dove viene una riga (e perche' la colonna `origine` ha tre valori)
//!
//! Le righe non hanno tutte la stessa autorita', e la differenza e' AZIONABILE:
//!
//! - `spec` — l'enum sta in una specifica versionata e SCARICABILE, quindi un
//!   job puo' DIFFARLA e accorgersi il giorno stesso che il fornitore ha
//!   aggiunto un valore. Sono tre: **anthropic** (`ErrorType`, 9 valori chiusi,
//!   nella spec Stainless versionata insieme all'SDK), **openrouter**
//!   (`ApiErrorType`, 27 valori, che la spec dichiara «canonical [...] stable
//!   across all API formats») e **google** (`google/api/error_reason.proto`,
//!   46 valori, che e' il livello utile: `google/rpc/code.proto` ne ha 17 ma
//!   sono categorie).
//! - `doc` — prosa o tabella HTML: la legge un umano, non un job. Le due fonti
//!   dello stesso fornitore possono divergere, ed e' misurato: `request_too_large`
//!   e' nella tabella HTML di anthropic e NON nel suo enum.
//! - `measured` — l'abbiamo visto noi. Per **openai** resta l'unica strada: la
//!   sua spec OpenAPI dichiara `Error.code` come `string | null` SENZA enum,
//!   cioe' definisce la FORMA e mai il LESSICO. Idem mistral, deepseek, groq,
//!   perplexity, kimi.
//!
//! **Nessuno dei nove espone un endpoint che elenchi i propri codici**: sondati
//! `/errors`, `/error-codes`, `/meta`, `/capabilities` su tutti e nove con le
//! chiavi reali, 36 probe e 36 volte 404. E' scritto qui perche' nessuno lo
//! ricerchi: il censimento per osservazione non e' un ripiego, e' l'unica strada
//! per sei fornitori su nove.
//!
//! ### Fuori portata, dichiarato
//!
//! openrouter e' un AGGREGATORE, e `metadata.provider_name` dice CHI ha fallito
//! davvero (MISURATO: presente in 132 corpi su 133 che portano `metadata`). Un
//! rate limit di Alibaba via openrouter non e' un fatto SU openrouter: metterlo
//! in cooldown esclude un aggregatore che avrebbe altri fornitori a valle da
//! provare — lo suggerisce il suo stesso `remedy_hint`, «route to another
//! provider». Ma e' una decisione sulla PORTATA del cooldown, non sulla
//! classificazione, e non entra in un intervento che promette di non cambiare
//! comportamento oltre i casi dichiarati. Il passo concreto, quando si fara':
//! quel campo viaggia gia' nel body, quindi il lavoro e' portarlo dove il
//! cooldown decide, non aggiungere una regola qui.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use nexus_types::provider_failure::ClasseErrore;
use sqlx::{PgPool, Row};

use crate::providers::ProviderHttpError;

/// Chiave DB dell'intervallo di ricarica del catalogo.
pub const REFRESH_SETTING: &str = "gateway.error_catalog.refresh_seconds";
/// Chiave DB della finestra di dedup delle scritture di codice ignoto.
pub const DEDUP_SETTING: &str = "gateway.error_catalog.unknown_dedup_seconds";
const DEFAULT_REFRESH_SECONDS: u64 = 60;
const DEFAULT_DEDUP_SECONDS: u64 = 60;
/// Tentativi di caricamento all'avvio, con la stessa disciplina di
/// `RoutingMatrixCache::init` in mcp-core (CLAUDE.md, regola G).
const TENTATIVI_AVVIO: u32 = 5;
const ATTESA_FRA_TENTATIVI: Duration = Duration::from_secs(5);
/// Taglio dell'`error.message` che accompagna un codice ignoto: serve a chi
/// dovra' classificarlo, non a conservare il body.
const ESEMPIO_MAX_CHARS: usize = 300;

// ─────────────────────────────────────────────────────────────────────────────
// I candidati: l'estrattore OSSERVA, non sceglie
// ─────────────────────────────────────────────────────────────────────────────

/// Dove il fornitore ha messo un possibile identificatore d'errore.
///
/// **L'ordine di dichiarazione E' il rango**: `giudica` lo usa come spareggio
/// fra candidati entrambi riconosciuti, mai come criterio di selezione.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CampoErrore {
    /// Candidato SINTETICO prodotto da un quirk dell'adapter (vedi
    /// `normalizza_codice_provider`): rango massimo perche' nasce dove sappiamo
    /// di CHI e' la risposta, cioe' con piu' informazione di qualunque campo.
    QuirkFornitore,
    /// `/error/code`
    ErrorCode,
    /// `/error/type`
    ErrorType,
    /// `/error/details[]/reason` — `ErrorInfo.reason` di google.rpc, che e'
    /// l'identificatore STABILE dell'errore; `/error/status` gli sta sotto ed e'
    /// la CATEGORIA (`google.rpc.Code`, 17 valori). E' la stessa relazione fra
    /// `code` e `type` che giustifica l'ordine di quei due, applicata al
    /// dialetto google — per questo la reason ha rango piu' alto dello status.
    ///
    /// MISURATO il 13/08/2026: a una chiave non valida google risponde **400
    /// INVALID_ARGUMENT**, non 401, e l'unica cosa che dice «e' la credenziale»
    /// e' `details[].reason = API_KEY_INVALID`. Senza questo campo quel caso si
    /// classifica come richiesta malformata e innesca una sanificazione della
    /// history che non puo' ripararlo.
    ErrorDetailsReason,
    /// `/error/status` (google.rpc.Code)
    ErrorStatus,
    /// `/error/metadata/limit_source` (openrouter). MISURATO: 79 righe con
    /// `openrouter_credits` e 4 con `upstream_provider_shared_pool`. NON viene
    /// esportato sul wire (vedi [`CandidatiErrore::codice_esportato`]).
    ErrorMetadataLimitSource,
    /// `code` top-level (mistral, che non usa l'involucro `error`)
    TopLevelCode,
    /// `type` top-level
    TopLevelType,
    /// `status` top-level
    TopLevelStatus,
}

impl CampoErrore {
    /// Il nome con cui il campo viene registrato fra i non dichiarati: e' cio'
    /// che chi legge la tabella dovra' andare a guardare nel body.
    pub fn puntatore(self) -> &'static str {
        match self {
            Self::QuirkFornitore => "quirk",
            Self::ErrorCode => "/error/code",
            Self::ErrorType => "/error/type",
            Self::ErrorDetailsReason => "/error/details/reason",
            Self::ErrorStatus => "/error/status",
            Self::ErrorMetadataLimitSource => "/error/metadata/limit_source",
            Self::TopLevelCode => "code",
            Self::TopLevelType => "type",
            Self::TopLevelStatus => "status",
        }
    }
}

/// Un valore osservato in un campo del body d'errore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidato {
    pub campo: CampoErrore,
    /// Normalizzato (trim + lowercase), come le righe del catalogo.
    pub valore: String,
}

/// Tutti i campi osservati, in ordine di rango, piu' il codice che viaggia sul
/// wire per compatibilita'.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidatiErrore {
    decisionali: Vec<Candidato>,
    esportato: Option<String>,
}

impl CandidatiErrore {
    /// Osserva il body JSON. Non sceglie, non decide, non normalizza semantica.
    ///
    /// **REGOLA DELL'INVOLUCRO**: se il body ha un OGGETTO `error`, i campi
    /// top-level non sono candidati. MISURATO: anthropic manda
    /// `{"type":"error","error":{…}}` — quel `"error"` top-level e' l'involucro,
    /// non un identificatore, e senza questa regola entrerebbe fra i codici da
    /// dichiarare **1806 volte**, rendendo rosso per sempre il gate del
    /// censimento su un non-problema. Il controllo e' `is_object()` e non
    /// "esiste la chiave": mistral manda `{"object":"error", …}` — una STRINGA —
    /// e i suoi campi top-level sono l'unica cosa che c'e'.
    pub fn dal_body(body: &str) -> Self {
        let esportato = codice_storico_sul_wire(body);
        let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
            return Self {
                decisionali: Vec::new(),
                esportato,
            };
        };
        let ha_involucro = v.get("error").is_some_and(|e| e.is_object());
        let mut decisionali = Vec::new();
        let mut aggiungi = |campo: CampoErrore, raw: Option<&serde_json::Value>| {
            if let Some(s) = raw.and_then(|x| x.as_str()) {
                let valore = normalizza(s);
                if !valore.is_empty() {
                    decisionali.push(Candidato { campo, valore });
                }
            }
        };
        aggiungi(CampoErrore::ErrorCode, v.pointer("/error/code"));
        aggiungi(CampoErrore::ErrorType, v.pointer("/error/type"));
        // `details` e' un ARRAY di oggetti tipizzati (`@type`): si prende la
        // PRIMA `reason` che compare, perche' `ErrorInfo` e' unico per risposta
        // (google.rpc) e gli altri elementi portano altro (`Help`, `BadRequest`).
        aggiungi(
            CampoErrore::ErrorDetailsReason,
            v.pointer("/error/details")
                .and_then(|d| d.as_array())
                .and_then(|a| a.iter().find_map(|e| e.get("reason"))),
        );
        aggiungi(CampoErrore::ErrorStatus, v.pointer("/error/status"));
        aggiungi(
            CampoErrore::ErrorMetadataLimitSource,
            v.pointer("/error/metadata/limit_source"),
        );
        if !ha_involucro {
            aggiungi(CampoErrore::TopLevelCode, v.get("code"));
            aggiungi(CampoErrore::TopLevelType, v.get("type"));
            aggiungi(CampoErrore::TopLevelStatus, v.get("status"));
        }
        Self {
            decisionali,
            esportato,
        }
    }

    /// Aggiunge il candidato SINTETICO di un quirk, in testa (rango massimo).
    ///
    /// Il quirk SOSTITUISCE anche il codice esportato: e' cio' che
    /// `normalizza_codice_provider` faceva gia' — anthropic con credito esaurito
    /// mette `billing_error` in `failures[].code` — e cambiarlo qui sposterebbe
    /// un valore che i consumatori a valle leggono per confronto esatto.
    pub fn con_quirk(mut self, valore: &str) -> Self {
        let valore = normalizza(valore);
        if valore.is_empty() {
            return self;
        }
        self.esportato = Some(valore.clone());
        self.decisionali.insert(
            0,
            Candidato {
                campo: CampoErrore::QuirkFornitore,
                valore,
            },
        );
        self
    }

    /// COMPATIBILITA' DI WIRE: il valore che finisce in `ProviderHttpError.code`
    /// e da li' in `failures[].code`, in `ErrorFacts.code` e nei log.
    ///
    /// Considera i SOLI sei campi storici: `metadata.limit_source` decide ma non
    /// esporta, o 127 righe openrouter passerebbero da `null` a
    /// `"openrouter_credits"` senza che nessuno l'abbia chiesto.
    pub fn codice_esportato(&self) -> Option<&str> {
        self.esportato.as_deref()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Candidato> {
        self.decisionali.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.decisionali.is_empty()
    }
}

/// Il codice come lo estraeva `extract_structured_error_code`: primo campo
/// stringa non vuoto fra i sei storici, in quest'ordine.
///
/// E' CONGELATA di proposito, e non condivide la regola dell'involucro con i
/// candidati decisionali: risponde a un'altra domanda — non «di che cosa parla
/// questo errore» ma «quale valore i consumatori a valle stanno gia' leggendo».
/// Cambiarla sarebbe un cambiamento di contratto, non un miglioramento della
/// classificazione (test: `il_codice_esportato_sul_wire_non_cambia`).
fn codice_storico_sul_wire(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let candidati = [
        v.pointer("/error/code"),
        v.pointer("/error/type"),
        v.pointer("/error/status"),
        v.get("code"),
        v.get("type"),
        v.get("status"),
    ];
    for c in candidati.into_iter().flatten() {
        if let Some(s) = c.as_str() {
            if !s.is_empty() {
                return Some(s.to_ascii_lowercase());
            }
        }
    }
    None
}

fn normalizza(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

// ─────────────────────────────────────────────────────────────────────────────
// Il vocabolario CHIUSO: il DB sceglie fra queste cause, non ne inventa
// ─────────────────────────────────────────────────────────────────────────────

/// Di che cosa parla l'errore. Vocabolario canonico e CHIUSO (regola N): il
/// `CHECK` della migrazione 0705 lo replica in SQL, cosi' una riga fuori
/// vocabolario non entra invece di entrare e non combaciare mai.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CausaErrore {
    CreditExhausted,
    RateLimit,
    Overloaded,
    ProviderFault,
    ModelNotFound,
    MalformedRequest,
    AuthDenied,
    RequestTooLarge,
}

impl CausaErrore {
    /// `None` = valore fuori vocabolario: la riga si SCARTA e si CONTA
    /// ([`Mappa::righe_scartate`]), non sparisce in silenzio.
    pub fn dal_db(s: &str) -> Option<Self> {
        match s.trim() {
            "credit_exhausted" => Some(Self::CreditExhausted),
            "rate_limit" => Some(Self::RateLimit),
            "overloaded" => Some(Self::Overloaded),
            "provider_fault" => Some(Self::ProviderFault),
            "model_not_found" => Some(Self::ModelNotFound),
            "malformed_request" => Some(Self::MalformedRequest),
            "auth_denied" => Some(Self::AuthDenied),
            "request_too_large" => Some(Self::RequestTooLarge),
            _ => None,
        }
    }

    pub fn come_stringa(self) -> &'static str {
        match self {
            Self::CreditExhausted => "credit_exhausted",
            Self::RateLimit => "rate_limit",
            Self::Overloaded => "overloaded",
            Self::ProviderFault => "provider_fault",
            Self::ModelNotFound => "model_not_found",
            Self::MalformedRequest => "malformed_request",
            Self::AuthDenied => "auth_denied",
            Self::RequestTooLarge => "request_too_large",
        }
    }

    /// La proiezione sulle 4 classi con cui il gateway decide retry e cooldown.
    /// Funzione TOTALE: non esiste una causa senza classe, quindi non esiste lo
    /// stato «riconosciuto ma non so cosa farne».
    pub fn classe(self) -> ClasseErrore {
        match self {
            Self::CreditExhausted => ClasseErrore::Billing,
            Self::RateLimit | Self::Overloaded | Self::ProviderFault => ClasseErrore::Transient,
            Self::ModelNotFound | Self::MalformedRequest | Self::AuthDenied => {
                ClasseErrore::ClientError
            }
            Self::RequestTooLarge => ClasseErrore::ContextTooLong,
        }
    }
}

/// Che cosa il catalogo dice di un valore. Tre stati, e sono distinti perche' i
/// rimedi lo sono (regola Q): `Ambiguo` e' una DICHIARAZIONE — «questo valore
/// non e' un identificatore» — e non va confusa con «non lo conosciamo ancora».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dichiarazione {
    Causa(CausaErrore),
    Ambiguo,
    Assente,
}

// ─────────────────────────────────────────────────────────────────────────────
// Il verdetto (regola Q: l'esito nei campi, il testo dopo)
// ─────────────────────────────────────────────────────────────────────────────

/// CHI ha deciso. Oggi «deciso perche' sappiamo» e «deciso perche' non
/// sappiamo» erano indistinguibili, ed e' precisamente la casella in cui openai
/// e' rimasto 14 giorni.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FonteVerdetto {
    /// Una riga del catalogo ha riconosciuto QUESTO candidato.
    Dichiarata {
        campo: CampoErrore,
        valore: String,
        causa: CausaErrore,
    },
    /// Candidati presenti (o nessuno) e nessuno dichiarato: decide la tabella
    /// per status. E' un ripiego DICHIARATO, non un silenzio.
    DalloStatus { status: u16 },
    /// Nessun [`ProviderHttpError`] nella catena: errore di trasporto
    /// (timeout/connessione). MISURATE migliaia di righe in questo stato.
    Trasporto,
}

/// Il verdetto completo su un errore fornitore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerdettoErrore {
    pub classe: ClasseErrore,
    pub causa: Option<CausaErrore>,
    pub fonte: FonteVerdetto,
    /// Il debito da colmare: candidati presenti che il catalogo non dichiara.
    pub non_dichiarati: Vec<Candidato>,
    /// Due candidati riconosciuti che indicano classi DIVERSE. Vince il rango, e
    /// il perdente finisce qui: un vocabolario che si contraddice non decide in
    /// silenzio.
    pub discordanti: Vec<(Candidato, CausaErrore)>,
}

impl VerdettoErrore {
    /// C'e' qualcosa da dichiarare o da correggere nel catalogo.
    pub fn richiede_intervento(&self) -> bool {
        !self.non_dichiarati.is_empty() || !self.discordanti.is_empty()
    }

    fn dallo_status(status: u16, non_dichiarati: Vec<Candidato>) -> Self {
        Self {
            classe: ClasseErrore::da_status(status),
            causa: None,
            fonte: FonteVerdetto::DalloStatus { status },
            non_dichiarati,
            discordanti: Vec::new(),
        }
    }

    fn trasporto() -> Self {
        Self {
            classe: ClasseErrore::Transient,
            causa: None,
            fonte: FonteVerdetto::Trasporto,
            non_dichiarati: Vec::new(),
            discordanti: Vec::new(),
        }
    }
}

/// IL CRITERIO, PURO: nessun I/O, nessun DB, nessun orologio.
///
/// Vince il primo candidato RICONOSCIUTO in ordine di rango. Il rango non
/// seleziona piu': fa da spareggio.
///
/// La DISCORDANZA — due candidati riconosciuti che indicano classi diverse —
/// viene registrata, ma NON quando a vincere e' un quirk: un quirk esiste
/// proprio perche' il campo del fornitore mente (anthropic dice
/// `invalid_request_error` a un credito esaurito), quindi la divergenza dagli
/// altri candidati e' il suo scopo e non una contraddizione del catalogo.
/// Registrarla accenderebbe `richiede_intervento()` su 1806 casi in cui non c'e'
/// nulla da correggere.
pub fn giudica(
    provider: &str,
    status: u16,
    candidati: &CandidatiErrore,
    mappa: &Mappa,
) -> VerdettoErrore {
    let mut vincitore: Option<(&Candidato, CausaErrore)> = None;
    let mut non_dichiarati = Vec::new();
    let mut riconosciuti: Vec<(&Candidato, CausaErrore)> = Vec::new();

    for c in candidati.iter() {
        match mappa.dichiarazione(provider, &c.valore, status) {
            Dichiarazione::Causa(causa) => {
                riconosciuti.push((c, causa));
                if vincitore.is_none() {
                    vincitore = Some((c, causa));
                }
            }
            // Dichiarato inutilizzabile: non decide, e non e' debito.
            Dichiarazione::Ambiguo => {}
            Dichiarazione::Assente => non_dichiarati.push(c.clone()),
        }
    }

    let Some((vinto, causa)) = vincitore else {
        return VerdettoErrore::dallo_status(status, non_dichiarati);
    };

    let discordanti = if vinto.campo == CampoErrore::QuirkFornitore {
        Vec::new()
    } else {
        riconosciuti
            .iter()
            .filter(|(c, cz)| c.campo != vinto.campo && cz.classe() != causa.classe())
            .map(|(c, cz)| ((*c).clone(), *cz))
            .collect()
    };

    VerdettoErrore {
        classe: causa.classe(),
        causa: Some(causa),
        fonte: FonteVerdetto::Dichiarata {
            campo: vinto.campo,
            valore: vinto.valore.clone(),
            causa,
        },
        non_dichiarati,
        discordanti,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// La mappa (dati) e il vocabolario (l'unico I/O)
// ─────────────────────────────────────────────────────────────────────────────

/// Chiave di una riga del catalogo: `(fornitore, valore, status)`, con `None` =
/// «qualunque status».
type Chiave = (String, String, Option<i16>);

/// Il catalogo `nexus_provider_error_code` in memoria.
#[derive(Debug, Default)]
pub struct Mappa {
    righe: HashMap<Chiave, Option<CausaErrore>>,
    pub righe_valide: usize,
    /// Righe con una `causa` fuori dal vocabolario del codice: si CONTANO,
    /// invece di sparire dalla mappa senza che nessuno lo sappia.
    pub righe_scartate: usize,
    pub caricata_at: Option<DateTime<Utc>>,
}

/// Identificatore di CONVENZIONE: vale per qualunque fornitore che non abbia
/// una riga propria. Mai un pattern — resta un valore ESATTO.
pub const PROVIDER_JOLLY: &str = "*";

impl Mappa {
    /// Costruisce da righe grezze `(provider, valore, status, causa)`.
    /// Usato dal caricamento DB e dai test del criterio.
    pub fn da_righe<'a>(
        righe: impl IntoIterator<Item = (&'a str, &'a str, Option<i16>, Option<&'a str>)>,
    ) -> Self {
        let mut mappa = Self::default();
        for (provider, valore, status, causa) in righe {
            let dichiarazione = match causa {
                None => None,
                Some(c) => match CausaErrore::dal_db(c) {
                    Some(causa) => Some(causa),
                    None => {
                        mappa.righe_scartate += 1;
                        continue;
                    }
                },
            };
            mappa.righe.insert(
                (normalizza(provider), normalizza(valore), status),
                dichiarazione,
            );
            mappa.righe_valide += 1;
        }
        mappa.caricata_at = Some(Utc::now());
        mappa
    }

    /// TRE GRADINI DICHIARATI, dal piu' specifico al piu' generale:
    /// `(fornitore, valore, status)` -> `(fornitore, valore, *)` ->
    /// `('*', valore, *)`. Mai `contains`, mai prefissi, mai regex.
    ///
    /// La ricerca esatta sul fornitore vince SEMPRE sul jolly: una riga
    /// specifica non puo' essere sovrascritta da una convenzione. E' cio' che
    /// tiene fermo deepseek, dove `invalid_request_error` significa CREDITO sul
    /// 402 e FORMATO sul 400.
    pub fn dichiarazione(&self, provider: &str, valore: &str, status: u16) -> Dichiarazione {
        let provider = normalizza(provider);
        let valore = normalizza(valore);
        let status = i16::try_from(status).ok();
        let gradini = [
            (provider.clone(), valore.clone(), status),
            (provider, valore.clone(), None),
            (PROVIDER_JOLLY.to_string(), valore, None),
        ];
        for chiave in gradini {
            if let Some(riga) = self.righe.get(&chiave) {
                return match riga {
                    Some(causa) => Dichiarazione::Causa(*causa),
                    None => Dichiarazione::Ambiguo,
                };
            }
        }
        Dichiarazione::Assente
    }
}

/// Il catalogo vivo: mappa in memoria, ricarica periodica, registro dei codici
/// che non sappiamo ancora leggere.
///
/// Clonabile a basso costo (tutto dietro `Arc`), come `CooldownManager`.
#[derive(Clone)]
pub struct VocabolarioErrori {
    /// L'ULTIMA mappa valida, sempre presente: caricata all'avvio (o panic), e
    /// sostituita solo da un refresh RIUSCITO. Cosi' lo stato «mappa assente
    /// che decide in silenzio» non e' rappresentabile, e il percorso di
    /// decisione non prende mai una connessione — il DB puo' essere proprio la
    /// cosa che sta male.
    mappa: Arc<RwLock<Arc<Mappa>>>,
    db: Arc<std::sync::OnceLock<PgPool>>,
    /// Dedup del `warn!`: una volta per processo per codice.
    gia_segnalati: Arc<DashMap<(String, String, String), ()>>,
    /// Occorrenze accumulate e istante dell'ultima scrittura, per chiave. Il
    /// contatore in memoria non e' un campione: la scrittura somma il DELTA, e
    /// il numero in tabella resta vero anche con la finestra di dedup.
    pendenti: Arc<DashMap<(String, String, String), (u64, Option<Instant>)>>,
    dedup: Arc<RwLock<Duration>>,
}

impl VocabolarioErrori {
    /// Costruisce da una mappa gia' nota, senza toccare il DB. Lo usano il
    /// caricamento all'avvio (che la mappa l'ha appena letta) e i test del
    /// criterio, che una mappa DB non ce l'hanno.
    pub fn con_mappa(mappa: Mappa) -> Self {
        Self {
            mappa: Arc::new(RwLock::new(Arc::new(mappa))),
            db: Arc::new(std::sync::OnceLock::new()),
            gia_segnalati: Arc::new(DashMap::new()),
            pendenti: Arc::new(DashMap::new()),
            dedup: Arc::new(RwLock::new(Duration::from_secs(DEFAULT_DEDUP_SECONDS))),
        }
    }

    /// Carica il catalogo all'avvio: 5 tentativi x 5s, poi **panic** nominando
    /// la migrazione.
    ///
    /// Non e' un modo di morire NUOVO: `bin/server.rs` gia' non fa partire il
    /// gateway senza un DB raggiungibile (`connect` eager) ne' senza listino
    /// (`nexus_pricing::assert_configured`). Quello che aggiunge e' la
    /// distinzione fra «DB giu'» — gia' coperta — e «migrazione non applicata»,
    /// che oggi sarebbe silenziosa: il gateway partirebbe classificando tutto
    /// dallo status, cioe' col difetto identico a prima e senza che nessuno lo
    /// veda.
    pub async fn carica_o_panica(pool: &PgPool) -> Self {
        let mut ultimo_errore = String::new();
        for tentativo in 1..=TENTATIVI_AVVIO {
            match carica_mappa(pool).await {
                Ok(mappa) if mappa.righe_valide > 0 => {
                    tracing::info!(
                        righe = mappa.righe_valide,
                        scartate = mappa.righe_scartate,
                        "gateway: catalogo dei codici errore fornitore caricato"
                    );
                    let v = Self::con_mappa(mappa);
                    let _ = v.db.set(pool.clone());
                    v.refresh_dedup(pool).await;
                    return v;
                }
                Ok(mappa) => {
                    ultimo_errore = format!(
                        "tabella presente ma vuota ({} righe scartate)",
                        mappa.righe_scartate
                    );
                }
                Err(e) => ultimo_errore = e.to_string(),
            }
            tracing::warn!(
                tentativo,
                errore = %ultimo_errore,
                "gateway: catalogo codici errore non caricato, ritento"
            );
            tokio::time::sleep(ATTESA_FRA_TENTATIVI).await;
        }
        panic!(
            "nexus-gateway: catalogo dei codici errore fornitore non caricabile dopo {TENTATIVI_AVVIO} \
             tentativi ({ultimo_errore}). Applicare la migrazione \
             db/migrations/0705_la_classe_di_un_errore_si_dichiara.sql: senza quel catalogo ogni \
             errore verrebbe classificato dal solo status HTTP, che e' il difetto per cui il \
             credito esaurito di openai e' stato ritentato per 14 giorni."
        );
    }

    /// Ricarica il catalogo. Un fallimento mantiene l'ultima mappa valida e
    /// lascia un `warn!`, come `CooldownManager::refresh_settings`.
    pub async fn refresh(&self, pool: &PgPool) {
        match carica_mappa(pool).await {
            Ok(nuova) if nuova.righe_valide > 0 => {
                if nuova.righe_scartate > 0 {
                    tracing::warn!(
                        scartate = nuova.righe_scartate,
                        "gateway: righe del catalogo errori con causa fuori vocabolario, ignorate"
                    );
                }
                *self.mappa.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(nuova);
            }
            Ok(_) => tracing::warn!(
                "gateway: catalogo codici errore vuoto al refresh, mantengo la mappa corrente"
            ),
            Err(e) => tracing::warn!(
                error = %e,
                "gateway: refresh del catalogo codici errore fallito, mantengo la mappa corrente"
            ),
        }
        self.refresh_dedup(pool).await;
    }

    async fn refresh_dedup(&self, pool: &PgPool) {
        let secondi = nexus_auth::get_setting(pool, DEDUP_SETTING)
            .await
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_DEDUP_SECONDS);
        *self.dedup.write().unwrap_or_else(|e| e.into_inner()) = Duration::from_secs(secondi);
    }

    pub fn mappa(&self) -> Arc<Mappa> {
        // Un lock AVVELENATO non rende invalido il dato: la mappa e' immutabile
        // dietro `Arc` e viene sostituita in un colpo solo. Propagare qui il
        // panic di un altro thread renderebbe incapace di classificare proprio
        // il percorso d'errore, che e' l'unico che passa di qui.
        self.mappa.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// L'UNICO ingresso dei call site: dall'errore al verdetto.
    ///
    /// Registra i codici non dichiarati come EFFETTO, non su richiesta del
    /// chiamante: un canale di scoperta che dipende da chi si ricorda di
    /// chiamarlo e' il canale che non copre il ramo dimenticato (regola O). Il
    /// log non puo' farne le veci — MISURATO, il `code` esce solo dal ramo
    /// ClientError e delle 4439 chiamate sbagliate non e' rimasta una riga.
    pub fn verdetto(&self, err: &anyhow::Error) -> VerdettoErrore {
        let mappa = self.mappa();
        for cause in err.chain() {
            if let Some(http) = cause.downcast_ref::<ProviderHttpError>() {
                let v = giudica(&http.provider, http.status, &http.candidati, &mappa);
                self.registra(&http.provider, http.status, &v, http.structured_message());
                return v;
            }
            if let Some(re) = cause.downcast_ref::<reqwest::Error>() {
                return match re.status() {
                    Some(s) => VerdettoErrore::dallo_status(s.as_u16(), Vec::new()),
                    // Nessuno status = trasporto: transitorio CERTO (predicati
                    // tipizzati, non testo).
                    None => VerdettoErrore::trasporto(),
                };
            }
        }
        VerdettoErrore::trasporto()
    }

    /// Il verdetto senza effetti: per i chiamanti che classificano un errore
    /// gia' registrato (resa HTTP dello stesso fallimento).
    pub fn verdetto_muto(&self, err: &anyhow::Error) -> VerdettoErrore {
        let mappa = self.mappa();
        for cause in err.chain() {
            if let Some(http) = cause.downcast_ref::<ProviderHttpError>() {
                return giudica(&http.provider, http.status, &http.candidati, &mappa);
            }
            if let Some(re) = cause.downcast_ref::<reqwest::Error>() {
                return match re.status() {
                    Some(s) => VerdettoErrore::dallo_status(s.as_u16(), Vec::new()),
                    None => VerdettoErrore::trasporto(),
                };
            }
        }
        VerdettoErrore::trasporto()
    }

    fn registra(
        &self,
        provider: &str,
        status: u16,
        verdetto: &VerdettoErrore,
        esempio: Option<String>,
    ) {
        if verdetto.non_dichiarati.is_empty() {
            return;
        }
        let finestra = *self.dedup.read().unwrap_or_else(|e| e.into_inner());
        for c in &verdetto.non_dichiarati {
            let chiave = (
                normalizza(provider),
                c.campo.puntatore().to_string(),
                c.valore.clone(),
            );
            if self.gia_segnalati.insert(chiave.clone(), ()).is_none() {
                tracing::warn!(
                    provider = %chiave.0,
                    campo = %chiave.1,
                    valore = %chiave.2,
                    status,
                    classe_di_ripiego = verdetto.classe.as_wire(),
                    "gateway: codice errore fornitore NON dichiarato -> deciso dallo status. \
                     Il rimedio e' una riga in nexus_provider_error_code (mig 0705)"
                );
            }
            self.programma_scrittura(chiave, status, verdetto, esempio.as_deref(), finestra);
        }
    }

    /// Accumula l'occorrenza e, se la finestra di dedup e' scaduta, spawna
    /// l'UPSERT col DELTA accumulato.
    ///
    /// Il delta si azzera SOLO se la scrittura parte davvero: pool e runtime si
    /// risolvono PRIMA di prenderlo. Prendendolo prima, un `verdetto()` fuori da
    /// un runtime tokio (o con pool non collegato) perderebbe le occorrenze
    /// accumulate — e il conteggio in tabella, che serve a decidere quali codici
    /// dichiarare per primi, sarebbe piu' basso del vero senza che nulla lo dica.
    fn programma_scrittura(
        &self,
        chiave: (String, String, String),
        status: u16,
        verdetto: &VerdettoErrore,
        esempio: Option<&str>,
        finestra: Duration,
    ) {
        let Some(pool) = self.db.get().cloned() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let mut voce = self.pendenti.entry(chiave.clone()).or_insert((0, None));
        voce.0 += 1;
        // La finestra non e' ancora scaduta: l'occorrenza resta accumulata.
        if voce.1.is_some_and(|t| t.elapsed() < finestra) {
            return;
        }
        let delta = std::mem::take(&mut voce.0);
        voce.1 = Some(Instant::now());
        drop(voce);

        let classe = verdetto.classe.as_wire().to_string();
        let esempio = esempio.map(|m| tronca(m, ESEMPIO_MAX_CHARS));
        handle.spawn(async move {
            persisti_ignoto(pool, chiave, status, classe, delta, esempio).await;
        });
    }
}

fn tronca(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Il caricamento del catalogo, per i test di ALTRI moduli che devono
/// confrontare le proprie righe di prova con il seed vero. Stessa funzione del
/// percorso di produzione: un secondo lettore misurerebbe un'altra tabella.
#[cfg(test)]
pub async fn carica_mappa_per_test(pool: &PgPool) -> Result<Mappa, sqlx::Error> {
    carica_mappa(pool).await
}

/// Carica il catalogo dal DB. Il crate legge SOLO questa tabella per questa
/// domanda: il guard `tassonomia-errori` di `check-single-source.sh` blocca una
/// seconda `SELECT` altrove.
async fn carica_mappa(pool: &PgPool) -> Result<Mappa, sqlx::Error> {
    let righe = sqlx::query(
        "SELECT provider, valore, http_status, causa FROM nexus_provider_error_code",
    )
    .fetch_all(pool)
    .await?;
    let grezze: Vec<(String, String, Option<i16>, Option<String>)> = righe
        .iter()
        .map(|r| {
            (
                r.get::<String, _>("provider"),
                r.get::<String, _>("valore"),
                r.get::<Option<i16>, _>("http_status"),
                r.get::<Option<String>, _>("causa"),
            )
        })
        .collect();
    Ok(Mappa::da_righe(grezze.iter().map(|(p, v, s, c)| {
        (p.as_str(), v.as_str(), *s, c.as_deref())
    })))
}

/// UPSERT del codice ignoto: la riga nasce alla prima occorrenza e cresce col
/// DELTA accumulato in memoria.
async fn persisti_ignoto(
    pool: PgPool,
    chiave: (String, String, String),
    status: u16,
    classe_di_ripiego: String,
    delta: u64,
    esempio: Option<String>,
) {
    let (provider, campo, valore) = chiave;
    let res = sqlx::query(
        "INSERT INTO nexus_provider_error_code_unknown \
           (provider, campo, valore, status_ultimo, classe_di_ripiego, occorrenze, esempio) \
         VALUES ($1,$2,$3,$4,$5,$6,$7) \
         ON CONFLICT (provider, campo, valore) DO UPDATE SET \
           occorrenze = nexus_provider_error_code_unknown.occorrenze + EXCLUDED.occorrenze, \
           ultimo_visto = now(), \
           status_ultimo = EXCLUDED.status_ultimo, \
           classe_di_ripiego = EXCLUDED.classe_di_ripiego, \
           esempio = COALESCE(EXCLUDED.esempio, nexus_provider_error_code_unknown.esempio)",
    )
    .bind(&provider)
    .bind(&campo)
    .bind(&valore)
    .bind(i16::try_from(status).unwrap_or(0))
    .bind(&classe_di_ripiego)
    .bind(i64::try_from(delta).unwrap_or(1))
    .bind(esempio.as_deref())
    .execute(&pool)
    .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, "gateway: registrazione del codice ignoto fallita");
    }
}

/// Loop dedicato di ricarica del catalogo.
///
/// NON si appende a `spawn_recovery_loop` (600s): il repo promette <=60s per il
/// refresh di una tabella di configurazione, e la ragione per cui il catalogo
/// sta nel DB e' proprio che una riga nuova valga subito.
pub fn spawn_vocabolario_loop(
    vocabolario: VocabolarioErrori,
    pool: PgPool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let secondi = nexus_auth::get_setting(&pool, REFRESH_SETTING)
                .await
                .and_then(|s| s.trim().parse::<u64>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(DEFAULT_REFRESH_SECONDS);
            tokio::time::sleep(Duration::from_secs(secondi)).await;
            vocabolario.refresh(&pool).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Corpo VERBATIM letto il 13/08/2026 da `nexus_provider_health`: e' il
    /// difetto, non una sua imitazione.
    pub(crate) const BODY_OPENAI_CREDITO: &str = r#"{
    "error": {
        "message": "You have no credits remaining. Add credits to continue using the API at https://platform.openai.com/settings/organization/billing/.",
        "type": "insufficient_quota",
        "param": null,
        "code": "credit_balance_exhausted"
    }
}"#;

    const BODY_ANTHROPIC_CREDITO: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing to upgrade or purchase credits."},"request_id":"req_011CdzfeaaT5KYCK6W78XoZk"}"#;

    const BODY_MISTRAL_1500: &str = r#"{"object":"error","message":"Invalid model: codestral-mamba-latest","type":"invalid_model","param":null,"code":"1500","raw_status_code":400}"#;

    fn mappa_minima() -> Mappa {
        Mappa::da_righe([
            (
                "openai",
                "credit_balance_exhausted",
                None,
                Some("credit_exhausted"),
            ),
            ("*", "insufficient_quota", None, Some("credit_exhausted")),
            ("*", "rate_limit_exceeded", None, Some("rate_limit")),
            ("*", "invalid_request_error", None, Some("malformed_request")),
            ("groq", "tokens", None, None),
            (
                "deepseek",
                "invalid_request_error",
                Some(402),
                Some("credit_exhausted"),
            ),
            ("anthropic", "billing_error", Some(400), Some("credit_exhausted")),
        ])
    }

    #[test]
    fn il_campo_col_rango_piu_alto_non_decide_se_non_e_riconosciuto() {
        // IL TEST DELLA CLASSE, non dell'istanza: il `code` con rango massimo e'
        // ignoto, il `type` piu' in basso e' dichiarato -> decide il type.
        // Riportare la scelta al primo candidato PRESENTE fa rosseggiare qui.
        let mappa = Mappa::da_righe([("*", "insufficient_quota", None, Some("credit_exhausted"))]);
        let candidati = CandidatiErrore::dal_body(BODY_OPENAI_CREDITO);
        let v = giudica("openai", 429, &candidati, &mappa);
        assert_eq!(v.classe, ClasseErrore::Billing);
        assert_eq!(
            v.fonte,
            FonteVerdetto::Dichiarata {
                campo: CampoErrore::ErrorType,
                valore: "insufficient_quota".into(),
                causa: CausaErrore::CreditExhausted,
            },
            "il code ignoto non deve impedire al type dichiarato di decidere"
        );
        assert_eq!(v.non_dichiarati.len(), 1);
        assert_eq!(v.non_dichiarati[0].valore, "credit_balance_exhausted");
        assert!(v.richiede_intervento());
    }

    #[test]
    fn il_credito_esaurito_di_openai_e_billing_e_si_sa_quale_riga_decide() {
        let candidati = CandidatiErrore::dal_body(BODY_OPENAI_CREDITO);
        let v = giudica("openai", 429, &candidati, &mappa_minima());
        assert_eq!(v.classe, ClasseErrore::Billing);
        assert_eq!(
            v.fonte,
            FonteVerdetto::Dichiarata {
                campo: CampoErrore::ErrorCode,
                valore: "credit_balance_exhausted".into(),
                causa: CausaErrore::CreditExhausted,
            }
        );
        assert!(!v.richiede_intervento(), "entrambi i campi sono dichiarati");

        // MUTAZIONE 1: tolte ENTRAMBE le righe, torna il difetto misurato.
        let vuota = Mappa::da_righe([]);
        let cieco = giudica("openai", 429, &candidati, &vuota);
        assert_eq!(
            cieco.classe,
            ClasseErrore::Transient,
            "senza catalogo si ricade sullo status: e' il comportamento di oggi, dichiarato"
        );
        assert!(matches!(cieco.fonte, FonteVerdetto::DalloStatus { .. }));
        assert_eq!(cieco.non_dichiarati.len(), 2);
    }

    #[test]
    fn il_413_di_groq_resta_una_attesa_e_il_tipo_ambiguo_non_compete() {
        // Corpo 413 REALE: il code dice rate_limit_exceeded, il type dice
        // "tokens" (la CATEGORIA della quota). Lo status da solo direbbe
        // ContextTooLong e il motore farebbe failover invece di attendere.
        let body = r#"{"error":{"message":"Request too large for model `openai/gpt-oss-20b` on tokens per minute (TPM): Limit 8000, Requested 8637","type":"tokens","code":"rate_limit_exceeded"}}"#;
        let candidati = CandidatiErrore::dal_body(body);
        let v = giudica("groq", 413, &candidati, &mappa_minima());
        assert_eq!(v.classe, ClasseErrore::Transient);
        assert_eq!(v.causa, Some(CausaErrore::RateLimit));
        assert!(
            v.non_dichiarati.is_empty(),
            "`tokens` e' dichiarato AMBIGUO: non decide, e non e' debito"
        );
    }

    #[test]
    fn la_stessa_stringa_vale_due_cose_a_due_status() {
        // deepseek: `invalid_request_error` significa CREDITO sul 402 e FORMATO
        // sul 400. E' il caso che pretende lo status nella chiave; unificare le
        // due righe in una sola fa rosseggiare uno dei due assert.
        let body = r#"{"error":{"message":"Insufficient Balance","type":"unknown_error","param":null,"code":"invalid_request_error"}}"#;
        let credito = giudica("deepseek", 402, &CandidatiErrore::dal_body(body), &mappa_minima());
        assert_eq!(credito.classe, ClasseErrore::Billing);
        assert_eq!(credito.causa, Some(CausaErrore::CreditExhausted));

        let formato = r#"{"error":{"message":"The reasoning_content in the thinking mode must be passed back to the API","type":"invalid_request_error","param":null,"code":"invalid_request_error"}}"#;
        let v = giudica("deepseek", 400, &CandidatiErrore::dal_body(formato), &mappa_minima());
        assert_eq!(v.classe, ClasseErrore::ClientError);
        assert_eq!(
            v.causa,
            Some(CausaErrore::MalformedRequest),
            "il 400 di formato DEVE restare riconoscibile: e' cio' che innesca la sanificazione"
        );
    }

    #[test]
    fn la_riga_esatta_vince_sempre_sul_jolly() {
        // Il jolly dice `malformed_request`; la riga (deepseek, ..., 402) dice
        // credito. Se il jolly potesse sovrascrivere, il 402 tornerebbe
        // ClientError e il fornitore senza credito verrebbe ri-chiamato.
        let body = r#"{"error":{"code":"invalid_request_error"}}"#;
        let v = giudica("deepseek", 402, &CandidatiErrore::dal_body(body), &mappa_minima());
        assert_eq!(v.causa, Some(CausaErrore::CreditExhausted));
    }

    #[test]
    fn la_regola_dell_involucro_non_fa_del_contenitore_un_candidato() {
        // anthropic: {"type":"error","error":{…}}. Senza questa regola il valore
        // "error" entrerebbe fra i codici da dichiarare 1844 volte.
        let candidati = CandidatiErrore::dal_body(BODY_ANTHROPIC_CREDITO);
        let valori: Vec<&str> = candidati.iter().map(|c| c.valore.as_str()).collect();
        assert_eq!(valori, ["invalid_request_error"]);

        // mistral NON ha involucro (`"object":"error"` e' una STRINGA): i suoi
        // campi top-level sono l'unica cosa che c'e'.
        let mistral = CandidatiErrore::dal_body(BODY_MISTRAL_1500);
        let campi: Vec<CampoErrore> = mistral.iter().map(|c| c.campo).collect();
        assert_eq!(
            campi,
            [CampoErrore::TopLevelCode, CampoErrore::TopLevelType],
            "senza is_object() la regola dell'involucro renderebbe mistral muto"
        );
    }

    #[test]
    fn il_quirk_vince_e_non_si_contraddice_col_campo_che_traduce() {
        let candidati = CandidatiErrore::dal_body(BODY_ANTHROPIC_CREDITO).con_quirk("billing_error");
        let v = giudica("anthropic", 400, &candidati, &mappa_minima());
        assert_eq!(v.classe, ClasseErrore::Billing);
        assert!(
            v.discordanti.is_empty(),
            "il quirk esiste PERCHE' il campo del fornitore mente: la divergenza e' il suo scopo"
        );
        assert_eq!(
            candidati.codice_esportato(),
            Some("billing_error"),
            "il codice sul wire resta quello che i consumatori leggono gia'"
        );
    }

    #[test]
    fn nessun_candidato_e_un_pattern() {
        // Un valore che CONTIENE una riga dichiarata non combacia. E' il caso
        // Moonshot: `exceeded_current_quota_error` passava perche' conteneva
        // "quota" per caso.
        let mappa = Mappa::da_righe([("*", "quota", None, Some("rate_limit"))]);
        assert_eq!(
            mappa.dichiarazione("kimi", "exceeded_current_quota_error", 429),
            Dichiarazione::Assente
        );
        assert_eq!(
            mappa.dichiarazione("kimi", "quota", 429),
            Dichiarazione::Causa(CausaErrore::RateLimit)
        );
    }

    #[test]
    fn una_causa_fuori_vocabolario_si_conta_invece_di_sparire() {
        let mappa = Mappa::da_righe([
            ("openai", "x", None, Some("causa_inventata")),
            ("openai", "y", None, Some("rate_limit")),
        ]);
        assert_eq!(mappa.righe_valide, 1);
        assert_eq!(mappa.righe_scartate, 1);
        assert_eq!(mappa.dichiarazione("openai", "x", 429), Dichiarazione::Assente);
    }

    #[test]
    fn due_candidati_riconosciuti_con_classi_diverse_lo_dichiarano() {
        let mappa = Mappa::da_righe([
            ("acme", "a", None, Some("credit_exhausted")),
            ("acme", "b", None, Some("malformed_request")),
        ]);
        let body = r#"{"error":{"code":"a","type":"b"}}"#;
        let v = giudica("acme", 429, &CandidatiErrore::dal_body(body), &mappa);
        assert_eq!(v.classe, ClasseErrore::Billing, "vince il rango");
        assert_eq!(v.discordanti.len(), 1);
        assert!(v.richiede_intervento());
    }

    #[test]
    fn il_codice_esportato_ignora_il_campo_che_decide_ma_non_viaggia() {
        // openrouter 402: il /error/code e' NUMERICO (scartato), e il campo che
        // decide sta in metadata. Sul wire il code resta `null`, come oggi.
        let body = r#"{"error":{"message":"This request requires more credits","code":402,"metadata":{"limit_source":"openrouter_credits"}}}"#;
        let candidati = CandidatiErrore::dal_body(body);
        assert_eq!(candidati.codice_esportato(), None);
        let mappa = Mappa::da_righe([(
            "openrouter",
            "openrouter_credits",
            None,
            Some("credit_exhausted"),
        )]);
        let v = giudica("openrouter", 402, &candidati, &mappa);
        assert_eq!(v.classe, ClasseErrore::Billing);
        assert_eq!(
            v.fonte,
            FonteVerdetto::Dichiarata {
                campo: CampoErrore::ErrorMetadataLimitSource,
                valore: "openrouter_credits".into(),
                causa: CausaErrore::CreditExhausted,
            }
        );
    }

    // ── Il corpus reale, contro il catalogo REALE ───────────────────────────
    //
    // I test qui sotto NON costruiscono la mappa a mano: la caricano dalla
    // migrazione 0705 con `META_MIGRATOR`, cioe' dalla stessa strada per cui il
    // catalogo nasce in produzione (regola O). Una mappa scritta nel test
    // proverebbe che il criterio sa leggere le righe che il test sa scrivere.

    /// Una voce del corpus: corpo VERBATIM misurato, con la classe che il
    /// gateway produceva PRIMA di questo intervento.
    struct VoceCorpus {
        provider: &'static str,
        status: u16,
        body: &'static str,
        /// La classe di OGGI. Per i fallimenti che hanno prodotto un cooldown e'
        /// la colonna `error_kind` di `nexus_provider_health_history` (osservata
        /// in esercizio, non dedotta); per i `client_error`, che non producono
        /// riga, e' la classe stampata nei log del gateway.
        classe_oggi: ClasseErrore,
        /// Valorizzato SOLO dove il cambiamento e' voluto: una terza differenza
        /// fa rosseggiare il test differenziale.
        differenza_dichiarata: Option<&'static str>,
        /// Il corpo persistito e' troncato a 500 char da `truncate_chars`: dove
        /// il JSON non chiudeva, la chiusura e' stata ricostruita e i campi che
        /// contano (`limit_source`) sono quelli misurati.
        troncato_in_persistenza: bool,
    }

    /// Tutti i corpi d'errore distinti misurati il 13/08/2026 su
    /// `nexus_provider_health_history` (piu' mistral 1500, che vive nei log
    /// perche' un `client_error` non produce riga di health).
    fn corpus() -> Vec<VoceCorpus> {
        let v = |provider, status, body, classe_oggi| VoceCorpus {
            provider,
            status,
            body,
            classe_oggi,
            differenza_dichiarata: None,
            troncato_in_persistenza: false,
        };
        vec![
            VoceCorpus {
                provider: "openai",
                status: 429,
                body: BODY_OPENAI_CREDITO,
                classe_oggi: ClasseErrore::Transient,
                differenza_dichiarata: Some(
                    "IL DIFETTO: 4439 righe dal 30/07 al 13/08. `credit_balance_exhausted` non \
                     contiene \"quota\", quindi cadeva sullo status 429 -> Transient, e un account \
                     senza credito veniva ri-provato ogni ~62s per 14 giorni",
                ),
                troncato_in_persistenza: false,
            },
            v(
                "openai",
                429,
                r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#,
                ClasseErrore::Billing,
            ),
            v("anthropic", 400, BODY_ANTHROPIC_CREDITO, ClasseErrore::Billing),
            v(
                "anthropic",
                400,
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages.1: Expected `thinking` or `redacted_thinking`, but found `text`"}}"#,
                ClasseErrore::ClientError,
            ),
            v(
                "anthropic",
                500,
                r#"{"type":"error","error":{"type":"api_error","message":"Internal server error"},"request_id":"req_011CdXoJihdGRVQ4uJ6puQTr"}"#,
                ClasseErrore::Transient,
            ),
            v(
                "anthropic",
                529,
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"},"request_id":"req_011CdY1cvkp8FAnViAtD3Kfa"}"#,
                ClasseErrore::Transient,
            ),
            v(
                "deepseek",
                402,
                r#"{"error":{"message":"Insufficient Balance","type":"unknown_error","param":null,"code":"invalid_request_error"}}"#,
                ClasseErrore::Billing,
            ),
            v(
                "deepseek",
                400,
                r#"{"error":{"message":"The reasoning_content in the thinking mode must be passed back to the API","type":"invalid_request_error","param":null,"code":"invalid_request_error"}}"#,
                ClasseErrore::ClientError,
            ),
            v(
                "google",
                429,
                r#"{"error":{"code":429,"message":"Resource exhausted. Please try again later.","status":"RESOURCE_EXHAUSTED"}}"#,
                ClasseErrore::Transient,
            ),
            v(
                "groq",
                413,
                r#"{"error":{"message":"Request too large for model `openai/gpt-oss-20b` in organization `org_x` service tier `on_demand` on tokens per minute (TPM): Limit 8000, Requested 8637","type":"tokens","code":"rate_limit_exceeded"}}"#,
                ClasseErrore::Transient,
            ),
            v(
                "groq",
                429,
                r#"{"error":{"message":"Rate limit reached for model `openai/gpt-oss-20b` on tokens per day (TPD): Limit 200000, Used 199250","type":"tokens","code":"rate_limit_exceeded"}}"#,
                ClasseErrore::Transient,
            ),
            v(
                "kimi",
                429,
                r#"{"error":{"message":"The engine is currently overloaded, please try again later","type":"engine_overloaded_error"}}"#,
                ClasseErrore::Transient,
            ),
            v(
                "kimi",
                429,
                r#"{"error":{"message":"Your account org-7870205d5982417a8a69c72cb690a1bb is suspended due to insufficient balance, please recharge","type":"exceeded_current_quota_error"}}"#,
                ClasseErrore::Billing,
            ),
            v("mistral", 429, r#"{"object":"error","message":"Capacity exceeded for this model.","type":"engine_max_pending_tokens","param":null,"code":"3810","raw_status_code":429}"#, ClasseErrore::Transient),
            v("mistral", 503, r#"{"object":"error","message":"Service unavailable.","type":"internal_server_error","param":null,"code":"3800","raw_status_code":503}"#, ClasseErrore::Transient),
            // La CLASSE non cambia (era ClientError e resta ClientError): a
            // cambiare e' il RAMO, e ha il suo test dedicato.
            v("mistral", 400, BODY_MISTRAL_1500, ClasseErrore::ClientError),
            VoceCorpus {
                provider: "openrouter",
                status: 402,
                body: r#"{"error":{"message":"This request requires more credits, or fewer max_tokens. You requested up to 65536 tokens, but can only afford 62186.","code":402,"metadata":{"limit_source":"openrouter_credits","remedy_hint":"Add credits at https://openrouter.ai/settings/credits"}}}"#,
                classe_oggi: ClasseErrore::Billing,
                differenza_dichiarata: None,
                troncato_in_persistenza: true,
            },
            VoceCorpus {
                provider: "openrouter",
                status: 429,
                body: r#"{"error":{"message":"Provider returned error","code":429,"metadata":{"raw":"qwen/qwen3.6-plus is temporarily rate-limited upstream","provider_name":"Alibaba","is_byok":false,"limit_source":"upstream_provider_shared_pool"}}}"#,
                classe_oggi: ClasseErrore::Transient,
                differenza_dichiarata: None,
                troncato_in_persistenza: true,
            },
            v(
                "openrouter",
                500,
                r#"{"error":{"message":"Internal Server Error","code":500}}"#,
                ClasseErrore::Transient,
            ),
            v(
                "perplexity",
                401,
                r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details.","type":"insufficient_quota","code":401}}"#,
                ClasseErrore::Billing,
            ),
        ]
    }

    /// Le NOVE forme con cui i fornitori dichiarano una credenziale rifiutata,
    /// misurate il 13/08/2026 inviando a ciascuno una chiave non valida e
    /// riprodotte due volte in modo indipendente. Stesso evento semantico, nove
    /// forme: e' il banco di prova piu' severo del criterio, ed e' provocabile a
    /// costo zero (nessun token consumato).
    ///
    /// La CLASSE e' `ClientError` per tutti, con e senza catalogo: qui non si
    /// misura la classe, si misura che la CAUSA sia quella giusta — perche' e' la
    /// causa a decidere se parta una sanificazione della history, che una
    /// credenziale rifiutata non la ripara.
    fn corpus_credenziale_rifiutata() -> Vec<(&'static str, u16, &'static str, Option<CausaErrore>)>
    {
        vec![
            ("openai", 401, r#"{"error":{"message":"Incorrect API key provided.","type":"invalid_request_error","param":null,"code":"invalid_api_key"}}"#, Some(CausaErrore::AuthDenied)),
            ("groq", 401, r#"{"error":{"message":"Invalid API Key","type":"invalid_request_error","code":"invalid_api_key"}}"#, Some(CausaErrore::AuthDenied)),
            // I RUOLI INVERTITI rispetto a openai: lo specifico e' nel `type`.
            ("deepseek", 401, r#"{"error":{"message":"Authentication Fails","type":"authentication_error","param":null,"code":"invalid_request_error"}}"#, Some(CausaErrore::AuthDenied)),
            ("anthropic", 401, r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"},"request_id":"req_x"}"#, Some(CausaErrore::AuthDenied)),
            ("kimi", 401, r#"{"error":{"message":"Invalid Authentication","type":"invalid_authentication_error"}}"#, Some(CausaErrore::AuthDenied)),
            ("perplexity", 401, r#"{"error":{"message":"Invalid API key","type":"invalid_api_key","code":401}}"#, Some(CausaErrore::AuthDenied)),
            // Lo status MENTE: google risponde 400, non 401, e l'unico campo che
            // dice «e' la credenziale» e' details[].reason.
            ("google", 400, r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"API_KEY_INVALID","domain":"googleapis.com"}]}}"#, Some(CausaErrore::AuthDenied)),
            // Nessun identificatore macchina: decide lo status, e va bene cosi'.
            // La prosa NON entra nel vocabolario (regola M).
            ("openrouter", 401, r#"{"error":{"message":"Missing Authentication header","code":401}}"#, None),
            ("mistral", 401, r#"{"detail":"Invalid API Key"}"#, None),
        ]
    }

    #[test]
    fn il_corpus_dichiara_quali_corpi_sono_stati_ricostruiti() {
        // La premessa del corpus, esplicita: `persist_last_error` tronca a 500
        // caratteri, quindi DUE corpi openrouter non chiudevano il JSON. La
        // chiusura e' ricostruita; il campo che DECIDE e' nella parte misurata,
        // e questo assert lo pretende — altrimenti il corpus proverebbe una
        // classificazione su un body che nessuno ha mai visto.
        let ricostruiti: Vec<(&str, u16)> = corpus()
            .iter()
            .filter(|v| v.troncato_in_persistenza)
            .map(|v| (v.provider, v.status))
            .collect();
        assert_eq!(ricostruiti, [("openrouter", 402), ("openrouter", 429)]);
        for v in corpus().iter().filter(|v| v.troncato_in_persistenza) {
            assert!(
                v.body.contains("limit_source"),
                "{} {}: il campo che decide deve venire dalla parte MISURATA",
                v.provider,
                v.status
            );
        }
    }

    /// La voce del difetto: si riconosce dal campo che la DICHIARA tale, non
    /// dalla posizione nel vettore. Un riordino del corpus non deve poter
    /// spostare in silenzio l'oggetto di due test.
    fn voce_del_difetto() -> VoceCorpus {
        let mut dichiarate: Vec<VoceCorpus> = corpus()
            .into_iter()
            .filter(|v| v.differenza_dichiarata.is_some())
            .collect();
        assert_eq!(
            dichiarate.len(),
            1,
            "il corpus dichiara una sola differenza voluta: se ne compaiono altre,              vanno motivate una per una"
        );
        dichiarate.remove(0)
    }

    /// Il catalogo come nasce in produzione: dalla migrazione, non da una mappa
    /// scritta nel test.
    async fn catalogo_reale(pool: &PgPool) -> Mappa {
        carica_mappa(pool).await.expect("catalogo dalla migrazione 0705")
    }

    /// Dal body alla classe per la strada della produzione: `from_response`
    /// osserva (compreso il quirk), `giudica` confronta col catalogo.
    fn verdetto_del_corpo(mappa: &Mappa, voce: &VoceCorpus) -> VerdettoErrore {
        let http = crate::providers::ProviderHttpError::from_response(
            voce.provider,
            voce.status,
            voce.body.to_string(),
        );
        giudica(voce.provider, voce.status, &http.candidati, mappa)
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn ogni_causa_della_migrazione_e_nel_vocabolario_del_codice(pool: PgPool) {
        let mappa = catalogo_reale(&pool).await;
        assert!(mappa.righe_valide > 0, "il seed della 0705 non e' arrivato");
        assert_eq!(
            mappa.righe_scartate, 0,
            "una riga con `causa` fuori dal vocabolario di CausaErrore non sparisce \
             in silenzio dalla mappa: si conta, e questo test la vede"
        );
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_corpus_reale_non_cambia_classe_tranne_nel_caso_dichiarato(pool: PgPool) {
        let mappa = catalogo_reale(&pool).await;
        let mut differenze = Vec::new();
        for voce in corpus() {
            let v = verdetto_del_corpo(&mappa, &voce);
            match (v.classe == voce.classe_oggi, voce.differenza_dichiarata) {
                (true, None) => {}
                (true, Some(motivo)) => panic!(
                    "{} {}: la differenza DICHIARATA non si e' prodotta ({motivo})",
                    voce.provider, voce.status
                ),
                (false, Some(_)) => {}
                (false, None) => differenze.push(format!(
                    "{} {} -> {:?} (era {:?})",
                    voce.provider, voce.status, v.classe, voce.classe_oggi
                )),
            }
        }
        assert!(
            differenze.is_empty(),
            "cambiamenti di classe NON dichiarati: {differenze:?}. Dove nulla e' \
             riconosciuto decide la tabella per status, che non e' stata toccata: \
             una differenza qui e' una riga di catalogo sbagliata"
        );
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_credito_esaurito_di_openai_e_billing_col_catalogo_vero(pool: PgPool) {
        let mappa = catalogo_reale(&pool).await;
        let voce = voce_del_difetto();
        let v = verdetto_del_corpo(&mappa, &voce);
        assert_eq!(v.classe, ClasseErrore::Billing);
        assert_eq!(
            v.fonte,
            FonteVerdetto::Dichiarata {
                campo: CampoErrore::ErrorCode,
                valore: "credit_balance_exhausted".into(),
                causa: CausaErrore::CreditExhausted,
            }
        );
        assert!(!v.richiede_intervento());
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_corpus_reale_non_produce_nessun_codice_da_dichiarare(pool: PgPool) {
        let mappa = catalogo_reale(&pool).await;
        let mut debito = Vec::new();
        for voce in corpus() {
            let v = verdetto_del_corpo(&mappa, &voce);
            for c in &v.non_dichiarati {
                debito.push(format!(
                    "{} {} {}={}",
                    voce.provider,
                    voce.status,
                    c.campo.puntatore(),
                    c.valore
                ));
            }
            assert!(
                v.discordanti.is_empty(),
                "{} {}: il catalogo si contraddice su {:?}",
                voce.provider,
                voce.status,
                v.discordanti
            );
        }
        assert!(
            debito.is_empty(),
            "il seed non copre codici che il nostro traffico produce GIA': {debito:?}. \
             Ogni voce qui e' una riga di `nexus_provider_error_code_unknown` che \
             nascerebbe al primo minuto di esercizio"
        );
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_credenziale_rifiutata_non_diventa_una_richiesta_malformata(pool: PgPool) {
        // NOVE forme per lo stesso evento. Il criterio non ha una precedenza per
        // fornitore e non ne ha bisogno: dove i ruoli di `code` e `type` sono
        // INVERTITI (deepseek contro openai) decide una riga con lo status nella
        // chiave, che il catalogo ha gia' per il 402-vs-400 dello stesso
        // fornitore. Una tabella di precedenze sarebbe un secondo elenco da
        // tenere allineato al primo.
        let mappa = catalogo_reale(&pool).await;
        for (provider, status, body, attesa) in corpus_credenziale_rifiutata() {
            let http = crate::providers::ProviderHttpError::from_response(
                provider,
                status,
                body.to_string(),
            );
            let v = giudica(provider, status, &http.candidati, &mappa);
            assert_eq!(
                v.causa, attesa,
                "{provider} {status}: causa attesa {attesa:?}, letta {:?} (fonte {:?})",
                v.causa, v.fonte
            );
            assert_eq!(
                v.classe,
                ClasseErrore::ClientError,
                "{provider} {status}: una credenziale rifiutata resta un errore client"
            );
            assert!(
                !crate::history_sanitizer::is_history_related_client_error(v.causa),
                "{provider} {status}: sanificare la history non ripara una credenziale \
                 rifiutata, e costa una chiamata in piu'"
            );
            assert!(
                v.non_dichiarati.is_empty(),
                "{provider} {status}: codici da dichiarare {:?}",
                v.non_dichiarati
            );
        }
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_quirk_emette_un_valore_che_il_catalogo_dichiara(pool: PgPool) {
        // Se il quirk emettesse un valore senza riga, il credito di anthropic
        // tornerebbe invisibile: 1806 righe che ricadono sullo status 400 ->
        // ClientError, cioe' un fornitore senza credito trattato come una
        // richiesta malformata (l'incidente del 26/07).
        let mappa = catalogo_reale(&pool).await;
        assert_eq!(
            mappa.dichiarazione(
                "anthropic",
                crate::providers::openai_compat::CODICE_BILLING_NORMALIZZATO,
                400
            ),
            Dichiarazione::Causa(CausaErrore::CreditExhausted)
        );
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn so_quale_riga_tiene_verde_il_caso_openai(pool: PgPool) {
        // MUTAZIONE sul catalogo VERO: si tolgono le righe una per volta e si
        // guarda chi decide al loro posto. E' cio' che distingue «il test e'
        // verde» da «so perche' e' verde».
        let togli = |valore: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query("DELETE FROM nexus_provider_error_code WHERE valore = $1")
                    .bind(valore)
                    .execute(&pool)
                    .await
                    .expect("delete di prova");
            }
        };

        togli("credit_balance_exhausted").await;
        let v = verdetto_del_corpo(&catalogo_reale(&pool).await, &voce_del_difetto());
        assert_eq!(
            v.classe,
            ClasseErrore::Billing,
            "tolta la riga sul code, DEVE decidere il `type`: a chiudere il difetto e' \
             la REGOLA (vince il primo riconosciuto), non la riga. Se questo assert \
             cade, il fix e' una toppa che copre una sola stringa"
        );
        assert_eq!(
            v.fonte,
            FonteVerdetto::Dichiarata {
                campo: CampoErrore::ErrorType,
                valore: "insufficient_quota".into(),
                causa: CausaErrore::CreditExhausted,
            }
        );

        togli("insufficient_quota").await;
        let cieco = verdetto_del_corpo(&catalogo_reale(&pool).await, &voce_del_difetto());
        assert_eq!(
            cieco.classe,
            ClasseErrore::Transient,
            "tolte ENTRAMBE, si ricade sullo status: e' il difetto misurato, e il fatto \
             che ricompaia dimostra che sono quelle due righe a chiuderlo"
        );
        assert_eq!(cieco.non_dichiarati.len(), 2);
        assert!(cieco.richiede_intervento());
    }

    #[test]
    fn senza_body_decide_lo_status_e_lo_dichiara() {
        let candidati = CandidatiErrore::dal_body("non e' json");
        assert!(candidati.is_empty());
        assert_eq!(candidati.codice_esportato(), None);
        let v = giudica("openai", 500, &candidati, &mappa_minima());
        assert_eq!(v.classe, ClasseErrore::Transient);
        assert_eq!(v.fonte, FonteVerdetto::DalloStatus { status: 500 });
        assert!(
            !v.richiede_intervento(),
            "nessun candidato non e' un debito: non c'e' niente da dichiarare"
        );
    }
}
