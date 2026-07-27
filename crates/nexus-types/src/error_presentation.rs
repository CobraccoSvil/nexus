//! PUNTO UNICO (regola L) della PRESENTAZIONE di un errore all'utente.
//!
//! Il repo aveva gia' il punto unico della CLASSIFICAZIONE — `classify_provider_error`,
//! `classify_gateway_error`, `ProviderFailureCause` — ma non il suo gemello: nessuna
//! funzione traduceva i fatti strutturati in una FRASE. Conseguenza obbligata: ogni
//! superficie che doveva mostrare qualcosa usava l'unico testo disponibile, cioe' il
//! `Display` degli errori tipizzati, che per contratto dichiarato porta il body grezzo
//! (`ProviderHttpError.message`, `GatewayHttpError.body`) o la catena diagnostica nata
//! per i log (`transport_error_detail`). L'utente leggeva in chat cose come
//! `MetadataMap { headers: {...} }`, `details: []`, `error sending request for url (...)
//! <- io(ConnectionReset, os_error=10054)`.
//!
//! Da quel vuoto sono nate quattro toppe testuali gemelle che tagliavano CARATTERI
//! invece di tradurre (`compact_provider_error` alla prima graffa, `humanize_ai_error`
//! sulla prima riga, `format_compact_error` a 300 char, `humanizeTraceText` a regex sul
//! frontend). Erano la prova del difetto, non la cura: cieche a ogni Debug che non
//! comincia con una graffa, e in violazione della regola M perche' decidevano guardando
//! il testo.
//!
//! ## Il contratto
//!
//! - [`ErrorFacts`] porta i SEGNALI STRUTTURATI che il produttore dell'errore conosce
//!   ancora (dominio, status HTTP, codice macchina, provider, natura del trasporto) piu'
//!   il `detail` tecnico integrale.
//! - [`render_user_error`] decide SOLO da quei segnali. Non ispeziona mai `detail`:
//!   se lo facesse, il testo umano tornerebbe a essere fonte di verita' tecnica ed
//!   avremmo ricostruito la toppa dentro il punto unico.
//! - [`RenderedError`] tiene `message` (per l'occhio) e `detail` (per il debugger)
//!   SEPARATI. Il `Display` stampa solo `message`: e' cio' che impedisce al blob di
//!   rientrare dalla finestra ogni volta che qualcuno scrive `format!("{e}")`.
//!
//! ## Cosa questo modulo NON deve diventare
//!
//! Il rischio opposto al blob e' il messaggio inutile. "Errore del provider AI." senza
//! dire QUALE provider e QUALE status non e' un fix: ha solo spostato il problema
//! dall'utente al debugger. Ogni messaggio prodotto qui nomina cio' che sa, e il
//! dettaglio tecnico resta sempre raggiungibile in `detail`.

use serde::{Deserialize, Serialize};

/// Da quale mondo viene l'errore. Determina il vocabolario del messaggio: un
/// utente non deve sapere che "gateway" e "provider" sono cose diverse, ma il
/// messaggio giusto da leggere e' diverso (uno e' un servizio interno che non
/// risponde, l'altro un fornitore esterno che rifiuta).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorDomain {
    /// Il fornitore AI esterno ha rifiutato o e' fallito.
    Provider,
    /// Il Nexus Gateway ha risposto con un errore applicativo.
    Gateway,
    /// La richiesta non e' mai arrivata a destinazione (socket, DNS, timeout).
    Transport,
    /// L'esecuzione di un tool e' fallita.
    Tool,
    /// Un plugin MCP ha fallito.
    Plugin,
    /// Il database non e' raggiungibile o ha rifiutato la query.
    Db,
}

/// Natura STRUTTURATA di un fallimento di trasporto, dai predicati tipizzati di
/// reqwest e dal codice OS — non dal testo (regola M).
///
/// `os_error` e' il segnale piu' specifico che esista su Windows: 10061 =
/// connessione rifiutata (nessuno in ascolto), 10054 = reset dal peer, 10060 =
/// timeout, 10055 = buffer di sistema esauriti.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportFacts {
    pub is_connect: bool,
    pub is_timeout: bool,
    /// `std::io::ErrorKind` reso stringa dal produttore (il tipo non e' serializzabile).
    pub io_kind: Option<String>,
    pub os_error: Option<i32>,
    /// Host:porta o URL verso cui la richiesta e' fallita. Senza, un
    /// "connessione rifiutata" non dice all'utente CHI non risponde.
    pub target: Option<String>,
}

/// I segnali strutturati che il produttore dell'errore conosce ancora, piu' il
/// dettaglio tecnico da conservare.
///
/// Si costruisce dove l'informazione e' VIVA (l'adapter che ha in mano la
/// risposta HTTP, il client che ha in mano l'errore reqwest), mai a valle da una
/// stringa gia' appiattita: ricostruirla da li' richiederebbe una regex sulla
/// prosa, che e' esattamente il difetto da cui veniamo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorFacts {
    pub domain: ErrorDomain,
    /// Status HTTP numerico: certo e standard, il segnale primario (ADR 0033).
    pub http_status: Option<u16>,
    /// Codice d'errore MACCHINA (`PROVIDER_ERROR`, `insufficient_quota`,
    /// `context_length_exceeded`, il `Code` di gRPC): identificatore stabile.
    pub code: Option<String>,
    /// Classe gia' decisa a monte da un classificatore (`billing`, `cooldown`,
    /// `client_error`, ...). Trasportata, non ri-derivata.
    pub class: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub transport: Option<TransportFacts>,
    /// La frase scritta da CHI ha generato l'errore, quando esiste: il
    /// `message` di un `tonic::Status`, il testo d'errore di un plugin MCP, la
    /// riga applicativa di un servizio interno.
    ///
    /// E' l'unico pezzo di testo che il render puo' usare, e solo per
    /// CONCATENARLO: non viene mai ispezionato per decidere. La distinzione
    /// conta — un `Debug` di struttura non e' un messaggio, e non va qui ma in
    /// [`ErrorFacts::detail`].
    pub upstream_message: Option<String>,
    /// Il testo tecnico integrale: body del provider, catena delle cause,
    /// `Debug` di `tonic::Status`. Trasportato e mostrato a richiesta, MAI
    /// ispezionato per decidere.
    pub detail: String,
}

impl ErrorFacts {
    /// Fatti minimi quando l'unica cosa disponibile e' il dettaglio tecnico.
    ///
    /// E' il ripiego onesto: il messaggio sara' generico ma il detail resta
    /// intero. Da usare solo dove i segnali strutturati non esistono davvero,
    /// non come scorciatoia per non estrarli.
    pub fn opaque(domain: ErrorDomain, detail: impl Into<String>) -> Self {
        Self {
            domain,
            http_status: None,
            code: None,
            class: None,
            provider: None,
            model: None,
            transport: None,
            upstream_message: None,
            detail: detail.into(),
        }
    }

    /// La frase di chi ha generato l'errore, quando c'e'. Vuota o fatta di soli
    /// spazi equivale ad assente.
    pub fn with_upstream(mut self, message: impl Into<String>) -> Self {
        let m = message.into();
        self.upstream_message = Some(m).filter(|s| !s.trim().is_empty());
        self
    }

    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.http_status = Some(status);
        self
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }
}

/// Un errore pronto per essere mostrato. `message` e `detail` restano separati
/// per costruzione: e' la separazione che impedisce al blob di tornare in chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedError {
    /// Identificatore canonico della classe (regola N: inglese, univoco). Serve
    /// al frontend per scegliere un'icona o un'azione, e ai test per asserire
    /// senza dipendere dalla formulazione italiana.
    pub code: String,
    /// La frase per l'utente. In italiano, nomina cio' che si sa.
    pub message: String,
    /// Il tecnico. Va mostrato dietro un "Mostra dettaglio tecnico" e finisce
    /// nei log, mai nel corpo del messaggio.
    pub detail: String,
}

impl std::fmt::Display for RenderedError {
    /// SOLO `message`. Ogni `format!("{e}")` sparso nel codebase — e ce ne sono
    /// molti — deve produrre testo leggibile senza sapere nulla di questo
    /// modulo. E' la ragione per cui il tipo esiste invece di una funzione che
    /// ritorna due stringhe.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RenderedError {}

/// PONTE per i punti che hanno gia' un messaggio umano e nessun fatto tecnico
/// da conservare: stub di test, errori applicativi scritti a mano, guard interni.
///
/// NON e' la strada normale. Chi ha in mano un errore di trasporto o una
/// risposta HTTP deve costruire [`ErrorFacts`] e passare da
/// [`render_user_error`]: e' li' che il messaggio nasce leggibile e il dettaglio
/// tecnico resta separato. Questa conversione esiste perche' il tipo attraversi
/// il codebase senza obbligare a inventare fatti che non ci sono.
impl From<String> for RenderedError {
    fn from(message: String) -> Self {
        Self {
            code: "unspecified".to_string(),
            message,
            detail: String::new(),
        }
    }
}

impl From<&str> for RenderedError {
    fn from(message: &str) -> Self {
        Self::from(message.to_string())
    }
}

impl RenderedError {
    /// Annota la resa col fatto che il provider era FORZATO dall'utente.
    ///
    /// Quando la richiesta viaggia con un pin, il servizio esegue QUEL fornitore
    /// e basta: nessun ripiego su un altro (nel gateway il pin costruisce una
    /// chain di un solo elemento). Il fallimento e' allora la conseguenza
    /// diretta della scelta dell'utente, non un guasto generico — e senza questa
    /// riga l'utente legge "X ha rifiutato la richiesta" senza sapere che
    /// togliendo la forzatura la richiesta sarebbe stata instradata altrove.
    ///
    /// Annota SOLO `message`: `code` resta quello deciso dai fatti (il frontend
    /// ci sceglie icona e azione) e `detail` resta il tecnico integrale.
    pub fn con_provider_forzato(mut self, provider: &str) -> Self {
        self.message = format!(
            "{} Provider forzato dall'utente ({provider}): con la forzatura attiva \
             non viene tentato nessun altro fornitore.",
            self.message.trim_end()
        );
        self
    }

    /// Il testo per i log: messaggio E dettaglio. Da usare in `tracing`, mai
    /// verso l'utente.
    pub fn log_line(&self) -> String {
        if self.detail.is_empty() {
            self.message.clone()
        } else {
            format!("{} | detail: {}", self.message, self.detail)
        }
    }

    /// Aggiunge la resa a un oggetto JSON gia' esistente, con le TRE chiavi
    /// additive del contratto: `user_message` (la frase), `user_code`
    /// (l'identificatore su cui il client sceglie icona e azione, mai il testo)
    /// e `user_detail` (il tecnico integrale).
    ///
    /// PUNTO UNICO (regola L) del NOME di quelle chiavi: scrittura qui, lettura
    /// in [`RenderedError::from_wire`]. Se vivessero in posti diversi — il
    /// gateway che scrive, mcp-core che legge — un rename lascerebbe verdi
    /// entrambi i lati e romperebbe solo il trasporto, cioe' l'unica cosa che
    /// nessuno dei due test guarda.
    ///
    /// Si chiama `user_detail` e non `detail` perche' sulle risposte del gateway
    /// convive gia' `details` con significato opposto (i fallimenti per-provider
    /// su cui decide il motore): due chiavi a un carattere di distanza sono la
    /// trappola che in questo repo ha gia' prodotto il bug dei costi a $0.00.
    pub fn write_into(&self, target: &mut serde_json::Value) {
        target["user_message"] = serde_json::Value::String(self.message.clone());
        target["user_code"] = serde_json::Value::String(self.code.clone());
        target["user_detail"] = serde_json::Value::String(self.detail.clone());
    }

    /// La resa trasportata da un payload JSON, se c'e'. `None` quando il
    /// produttore non la porta (servizio vecchio, endpoint non ancora migrato):
    /// il chiamante ripiega onestamente, senza dedurre nulla dal testo.
    pub fn from_wire(payload: &serde_json::Value) -> Option<Self> {
        let stringa = |chiave: &str| payload.get(chiave).and_then(|v| v.as_str());
        let message = stringa("user_message").filter(|s| !s.trim().is_empty())?;
        Some(Self {
            code: stringa("user_code").unwrap_or("unspecified").to_string(),
            message: message.to_string(),
            detail: stringa("user_detail").unwrap_or_default().to_string(),
        })
    }
}

/// Chi produce errori sa estrarre i propri segnali strutturati.
///
/// Composizione, non ereditarieta' (regola L): i tipi d'errore esistenti restano
/// dove sono e implementano questo trait; nessuno di loro duplica la RESA, che
/// vive solo in [`render_user_error`].
pub trait HasErrorFacts {
    fn error_facts(&self) -> ErrorFacts;

    /// Comodita': i fatti gia' resi. Non sovrascrivere — la resa e' unica.
    fn rendered(&self) -> RenderedError {
        render_user_error(&self.error_facts())
    }
}

/// LA funzione. Traduce i fatti in una frase, senza mai guardare `detail`.
pub fn render_user_error(facts: &ErrorFacts) -> RenderedError {
    let (code, message) = match facts.domain {
        ErrorDomain::Transport => render_transport(facts),
        ErrorDomain::Provider => render_provider(facts),
        ErrorDomain::Gateway => render_gateway(facts),
        ErrorDomain::Tool => render_tool(facts),
        ErrorDomain::Plugin => render_plugin(facts),
        ErrorDomain::Db => render_db(facts),
    };
    RenderedError {
        code: code.to_string(),
        message: componi(message, facts.upstream_message.as_deref()),
        detail: facts.detail.clone(),
    }
}

/// Tetto di lunghezza del messaggio mostrato. Non e' una traduzione: e' layout.
/// Il testo intero resta sempre in `detail`.
const MAX_MESSAGGIO: usize = 300;

/// Unisce la frase dedotta dai fatti con quella di chi ha generato l'errore.
///
/// La parte upstream viene NORMALIZZATA (a capo e spazi multipli collassati) e
/// troncata: qui vivevano due `format_compact_error` gemelli, uno in mcp-core e
/// uno in plugin-service, che facevano esattamente questo su copie diverse.
fn componi(base: String, upstream: Option<&str>) -> String {
    let Some(u) = upstream.map(str::trim).filter(|s| !s.is_empty()) else {
        return base;
    };
    let compatto = u.split_whitespace().collect::<Vec<_>>().join(" ");
    let compatto = tronca(&compatto, MAX_MESSAGGIO);
    if base.is_empty() {
        compatto
    } else {
        format!("{base} {compatto}")
    }
}

fn tronca(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!("{}...", s.chars().take(max).collect::<String>())
}

/// Come nominare l'interlocutore quando lo si conosce. Senza questo, ogni
/// messaggio direbbe "il servizio AI" e l'utente non saprebbe mai CHI ha
/// fallito: sarebbe il regresso opposto al blob.
fn chi(facts: &ErrorFacts) -> String {
    match (&facts.provider, &facts.model) {
        (Some(p), Some(m)) => format!("{p} ({m})"),
        (Some(p), None) => p.clone(),
        _ => "il fornitore AI".to_string(),
    }
}

fn render_transport(facts: &ErrorFacts) -> (&'static str, String) {
    let t = facts.transport.clone().unwrap_or_default();
    let dove = match t.target.as_deref() {
        Some(x) => format!(" ({x})"),
        None => String::new(),
    };
    if t.is_timeout {
        return (
            "transport_timeout",
            format!("Il servizio AI non ha risposto entro il tempo massimo{dove}."),
        );
    }
    match t.os_error {
        // 10061 WSAECONNREFUSED: nessuno in ascolto. E' il caso "gateway spento".
        Some(10061) | Some(111) => (
            "transport_unreachable",
            format!("Il servizio AI non e' raggiungibile{dove}: connessione rifiutata, il servizio non e' in ascolto."),
        ),
        // 10054 WSAECONNRESET: c'era, ha chiuso a meta'.
        Some(10054) | Some(104) => (
            "transport_reset",
            format!("La connessione al servizio AI{dove} e' stata interrotta durante la richiesta."),
        ),
        _ if t.is_connect => (
            "transport_unreachable",
            format!("Impossibile stabilire la connessione al servizio AI{dove}."),
        ),
        _ => (
            "transport_failed",
            format!("La richiesta al servizio AI{dove} non e' arrivata a destinazione."),
        ),
    }
}

fn render_provider(facts: &ErrorFacts) -> (&'static str, String) {
    let chi = chi(facts);
    // Il codice macchina del provider vince sullo status: e' piu' specifico.
    if let Some(code) = facts.code.as_deref() {
        if let Some(esito) = render_provider_code(code, &chi) {
            return esito;
        }
    }
    if let Some(esito) = facts.class.as_deref().and_then(|c| render_provider_class(c, &chi)) {
        return esito;
    }
    render_provider_status(facts.http_status, &chi)
}

/// Codici macchina ricorrenti, comuni ai provider OpenAI-compatibili.
fn render_provider_code(code: &str, chi: &str) -> Option<(&'static str, String)> {
    let esito = match code {
        "insufficient_quota" | "billing_hard_limit_reached" => (
            "provider_quota",
            format!("{chi} ha rifiutato la richiesta: credito o quota esauriti."),
        ),
        "rate_limit_exceeded" => (
            "provider_rate_limited",
            format!("{chi} ha applicato un limite di frequenza: troppe richieste ravvicinate."),
        ),
        "context_length_exceeded" | "request_too_large" => (
            "provider_request_too_large",
            format!("La richiesta e' troppo grande per {chi}."),
        ),
        "invalid_api_key" | "authentication_error" => (
            "provider_auth",
            format!("{chi} ha rifiutato le credenziali configurate."),
        ),
        "model_not_found" | "invalid_model" => (
            "provider_model_unknown",
            format!("{chi} non riconosce il modello richiesto."),
        ),
        // Codice emesso dal gateway stesso (`CallFailure::attempt_timeout`) sul
        // tentativo scaduto. Senza questo ramo cadrebbe sulla classe
        // "transient", che dice "sospeso dopo un errore recente": un provider
        // che sta ancora pensando non e' un provider sospeso.
        "attempt_timeout" => (
            "provider_timeout",
            format!("{chi} non ha risposto entro il tempo concesso al tentativo."),
        ),
        _ => return None,
    };
    Some(esito)
}

/// Classi gia' decise dal classificatore a monte (`primary_cause` del gateway).
fn render_provider_class(class: &str, chi: &str) -> Option<(&'static str, String)> {
    let esito = match class {
        "billing" | "cooldown_billing" => (
            "provider_quota",
            format!("{chi} non e' utilizzabile: problema di credito o fatturazione."),
        ),
        "cooldown" | "transient" => (
            "provider_cooldown",
            format!("{chi} e' temporaneamente sospeso dopo un errore recente."),
        ),
        "context_too_long" => (
            "provider_request_too_large",
            format!("La richiesta e' troppo grande per {chi}."),
        ),
        "empty_completion" => (
            "provider_empty",
            format!("{chi} ha risposto senza contenuto utilizzabile."),
        ),
        "policy_tier_excluded" => (
            "policy_excluded",
            format!("{chi} e' escluso dalle policy per la riservatezza di questo contenuto."),
        ),
        _ => return None,
    };
    Some(esito)
}

fn render_provider_status(status: Option<u16>, chi: &str) -> (&'static str, String) {
    match status {
        Some(401) | Some(403) => (
            "provider_auth",
            format!("{chi} ha rifiutato le credenziali configurate."),
        ),
        Some(402) => (
            "provider_quota",
            format!("{chi} ha rifiutato la richiesta: credito esaurito."),
        ),
        Some(404) => (
            "provider_model_unknown",
            format!("{chi} non espone l'endpoint o il modello richiesto."),
        ),
        Some(413) => (
            "provider_request_too_large",
            format!("La richiesta e' troppo grande per {chi}."),
        ),
        Some(429) => (
            "provider_rate_limited",
            format!("{chi} ha applicato un limite di frequenza: troppe richieste ravvicinate."),
        ),
        Some(s) if (500..600).contains(&s) => (
            "provider_unavailable",
            format!("{chi} ha avuto un guasto interno (HTTP {s})."),
        ),
        Some(s) => (
            "provider_rejected",
            format!("{chi} ha rifiutato la richiesta (HTTP {s})."),
        ),
        None => (
            "provider_failed",
            format!("La richiesta a {chi} e' fallita."),
        ),
    }
}

fn render_gateway(facts: &ErrorFacts) -> (&'static str, String) {
    match facts.code.as_deref() {
        Some("PROVIDER_ERROR") => (
            "gateway_all_providers_failed",
            "Nessun fornitore AI disponibile ha potuto completare la richiesta.".to_string(),
        ),
        Some("POLICY_TIER_EXCLUDED") | Some("TIER_BLOCKED") => (
            "policy_excluded",
            "Le policy di riservatezza escludono i fornitori disponibili per questo contenuto."
                .to_string(),
        ),
        Some("QUOTA_EXCEEDED") => (
            "quota_exceeded",
            "La quota configurata per questo ambito e' esaurita.".to_string(),
        ),
        Some("INVALID_REQUEST") => (
            "invalid_request",
            "La richiesta inviata al servizio AI non e' valida.".to_string(),
        ),
        // Literal MINUSCOLO: e' il valore storico sul wire (`code` del 504 di
        // budget), non un identificatore nuovo. Il gateway lo emette cosi' e il
        // motore agentico ci decide sopra il failover: normalizzarlo qui
        // significherebbe non riconoscerlo mai.
        Some("request_budget_exceeded") => (
            "gateway_timeout",
            "Il servizio AI non ha completato la richiesta entro il tempo massimo consentito."
                .to_string(),
        ),
        _ => match facts.http_status {
            Some(s) if (500..600).contains(&s) => (
                "gateway_error",
                format!("Il servizio AI interno ha avuto un guasto (HTTP {s})."),
            ),
            Some(s) => (
                "gateway_rejected",
                format!("Il servizio AI interno ha rifiutato la richiesta (HTTP {s})."),
            ),
            None => (
                "gateway_error",
                "Il servizio AI interno ha risposto con un errore.".to_string(),
            ),
        },
    }
}

fn render_tool(facts: &ErrorFacts) -> (&'static str, String) {
    match facts.code.as_deref() {
        Some("Unavailable") => (
            "tool_unavailable",
            "Il servizio degli strumenti non e' raggiungibile.".to_string(),
        ),
        Some("DeadlineExceeded") => (
            "tool_timeout",
            "Lo strumento non ha risposto entro il tempo massimo.".to_string(),
        ),
        Some("NotFound") => (
            "tool_not_found",
            "Lo strumento o la risorsa richiesta non esiste.".to_string(),
        ),
        Some("PermissionDenied") => (
            "tool_denied",
            "Permesso negato per l'operazione richiesta.".to_string(),
        ),
        Some(c) => ("tool_failed", format!("Lo strumento e' fallito ({c}).")),
        None => (
            "tool_failed",
            "L'esecuzione dello strumento e' fallita.".to_string(),
        ),
    }
}

fn render_plugin(facts: &ErrorFacts) -> (&'static str, String) {
    match facts.http_status {
        Some(s) => (
            "plugin_failed",
            format!("Il plugin ha risposto con un errore (HTTP {s})."),
        ),
        None => (
            "plugin_failed",
            "Il plugin non ha completato l'operazione.".to_string(),
        ),
    }
}

fn render_db(_facts: &ErrorFacts) -> (&'static str, String) {
    (
        "db_unavailable",
        "Il database del progetto non e' disponibile.".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transport(t: TransportFacts, detail: &str) -> RenderedError {
        render_user_error(&ErrorFacts {
            transport: Some(t),
            ..ErrorFacts::opaque(ErrorDomain::Transport, detail)
        })
    }

    /// L'INVARIANTE del modulo: il dettaglio tecnico non entra MAI nel messaggio.
    ///
    /// E' l'unica cosa che, se si rompe, riporta il blob in chat. Vale per ogni
    /// dominio e per detail di qualunque forma, incluse quelle che hanno
    /// prodotto il difetto originale.
    #[test]
    fn il_dettaglio_tecnico_non_entra_mai_nel_messaggio() {
        let veleni = [
            "status: Unavailable, message: \"x\", details: [], metadata: MetadataMap { headers: {} }",
            "error sending request for url (http://127.0.0.1:4060/v1/complete) [kind=connect] <- io(ConnectionRefused, os_error=10061)",
            "{\"error\":{\"message\":\"nope\",\"type\":\"invalid_request_error\"}}",
        ];
        let domini = [
            ErrorDomain::Provider,
            ErrorDomain::Gateway,
            ErrorDomain::Transport,
            ErrorDomain::Tool,
            ErrorDomain::Plugin,
            ErrorDomain::Db,
        ];
        for veleno in veleni {
            for dominio in domini {
                let r = render_user_error(&ErrorFacts::opaque(dominio, veleno));
                assert!(
                    !r.message.contains(veleno),
                    "il detail e' finito nel messaggio ({dominio:?}): {}",
                    r.message
                );
                assert!(
                    !r.message.contains('{') && !r.message.contains("os_error"),
                    "il messaggio porta gergo tecnico ({dominio:?}): {}",
                    r.message
                );
                assert_eq!(r.detail, veleno, "il detail deve restare INTATTO");
                assert_eq!(r.to_string(), r.message, "Display deve dare solo il messaggio");
            }
        }
    }

    /// Il caso riportato dall'utente: gateway spento, connessione rifiutata.
    #[test]
    fn connessione_rifiutata_dice_chi_non_risponde_e_perche() {
        let r = transport(
            TransportFacts {
                is_connect: true,
                os_error: Some(10061),
                io_kind: Some("ConnectionRefused".into()),
                target: Some("127.0.0.1:4060".into()),
                ..Default::default()
            },
            "error sending request ... <- io(ConnectionRefused, os_error=10061)",
        );
        assert_eq!(r.code, "transport_unreachable");
        assert!(r.message.contains("127.0.0.1:4060"), "deve dire CHI: {}", r.message);
        assert!(r.message.contains("rifiutata"), "deve dire PERCHE: {}", r.message);
        assert!(r.detail.contains("os_error=10061"), "il segnale diagnostico non si perde");
    }

    /// Il timeout vince sul codice OS: e' l'informazione piu' azionabile.
    #[test]
    fn il_timeout_e_riconosciuto_come_tale() {
        let r = transport(
            TransportFacts {
                is_timeout: true,
                os_error: Some(10060),
                ..Default::default()
            },
            "operation timed out",
        );
        assert_eq!(r.code, "transport_timeout");
    }

    /// Un messaggio che non nomina il provider e' il regresso opposto al blob:
    /// leggibile e inutile. Qui si fissa che non accada.
    #[test]
    fn il_messaggio_nomina_sempre_il_provider_quando_lo_conosce() {
        let r = render_user_error(
            &ErrorFacts::opaque(ErrorDomain::Provider, "{\"error\":\"...\"}")
                .with_provider("mistral")
                .with_status(429),
        );
        assert_eq!(r.code, "provider_rate_limited");
        assert!(r.message.contains("mistral"), "senza il nome e' inutile: {}", r.message);
    }

    /// Il codice macchina e' piu' specifico dello status e deve vincere: un 400
    /// con `context_length_exceeded` non e' "richiesta rifiutata" generica.
    #[test]
    fn il_codice_macchina_vince_sullo_status() {
        let solo_status = render_user_error(
            &ErrorFacts::opaque(ErrorDomain::Provider, "").with_status(400),
        );
        assert_eq!(solo_status.code, "provider_rejected");

        let col_codice = render_user_error(
            &ErrorFacts::opaque(ErrorDomain::Provider, "")
                .with_status(400)
                .with_code("context_length_exceeded"),
        );
        assert_eq!(col_codice.code, "provider_request_too_large");
    }

    /// La classe decisa a monte dal gateway viene RIUSATA, non ri-derivata.
    #[test]
    fn la_classe_del_classificatore_viene_riusata() {
        let mut facts = ErrorFacts::opaque(ErrorDomain::Provider, "").with_provider("anthropic");
        facts.class = Some("billing".into());
        let r = render_user_error(&facts);
        assert_eq!(r.code, "provider_quota");
        assert!(r.message.contains("anthropic"));
    }

    /// Il codice gRPC arriva strutturato (`status.code()`), non dal Debug.
    #[test]
    fn il_codice_grpc_diventa_una_frase() {
        let r = render_user_error(
            &ErrorFacts::opaque(ErrorDomain::Tool, "metadata: MetadataMap { headers: {} }")
                .with_code("Unavailable"),
        );
        assert_eq!(r.code, "tool_unavailable");
        assert!(!r.message.contains("MetadataMap"));
    }

    /// La frase di chi ha generato l'errore si aggiunge, normalizzata: e' cio'
    /// che facevano i due `format_compact_error` gemelli, ora una volta sola.
    #[test]
    fn la_frase_upstream_si_aggiunge_normalizzata() {
        let r = render_user_error(
            &ErrorFacts::opaque(ErrorDomain::Plugin, "raw")
                .with_upstream("connessione\n\n  rifiutata   dal   server"),
        );
        assert!(
            r.message.ends_with("connessione rifiutata dal server"),
            "a capo e spazi multipli vanno collassati: {}",
            r.message
        );

        let lunghissimo = "x".repeat(500);
        let r = render_user_error(
            &ErrorFacts::opaque(ErrorDomain::Plugin, "raw").with_upstream(&lunghissimo),
        );
        assert!(r.message.chars().count() < 400, "tetto non applicato");
        assert!(r.message.ends_with("..."));

        // Vuota o di soli spazi equivale ad assente: niente frase monca.
        let r = render_user_error(
            &ErrorFacts::opaque(ErrorDomain::Plugin, "raw").with_upstream("   "),
        );
        assert!(!r.message.ends_with(' '), "spazio in coda: {:?}", r.message);
    }

    /// Col provider forzato la frase deve dire DUE cose: cosa e' andato storto e
    /// che non c'e' stato alcun ripiego. `code` e `detail` non si toccano.
    #[test]
    fn la_forzatura_del_provider_si_legge_nel_messaggio() {
        let base = render_user_error(
            &ErrorFacts::opaque(ErrorDomain::Provider, "{\"error\":{\"code\":\"x\"}}")
                .with_provider("deepseek")
                .with_status(400),
        );
        let annotato = base.clone().con_provider_forzato("deepseek");
        assert_eq!(annotato.code, base.code, "il codice classificato non cambia");
        assert_eq!(annotato.detail, base.detail, "il dettaglio tecnico non cambia");
        assert!(
            annotato.message.starts_with(&base.message),
            "l'annotazione si AGGIUNGE, non sostituisce: {}",
            annotato.message
        );
        assert!(
            annotato.message.contains("forzato") && annotato.message.contains("deepseek"),
            "chi legge deve capire che il provider era forzato: {}",
            annotato.message
        );
        assert!(
            annotato.message.contains("nessun altro fornitore"),
            "deve dire che non c'e' stato ripiego: {}",
            annotato.message
        );
    }

    /// `log_line` e' l'altro canale: li' il dettaglio DEVE esserci, altrimenti
    /// il fix avrebbe solo spostato il problema dall'utente al debugger.
    #[test]
    fn i_log_conservano_il_dettaglio() {
        let r = render_user_error(&ErrorFacts::opaque(ErrorDomain::Gateway, "boom os_error=1"));
        assert!(r.log_line().contains("boom os_error=1"));
    }

    /// Il TRASPORTO: cio' che un lato scrive, l'altro lo rilegge identico.
    ///
    /// Il round-trip passa dai due produttori veri (`write_into`/`from_wire`),
    /// non da un letterale ricopiato: e' l'unico modo perche' un rename delle
    /// chiavi faccia ROSSEGGIARE qualcosa invece di rompere in silenzio il solo
    /// pezzo che nessuno osserva.
    #[test]
    fn la_resa_sopravvive_al_confine_json() {
        let originale = render_user_error(
            &ErrorFacts::opaque(ErrorDomain::Provider, "{\"error\":{\"code\":\"x\"}}")
                .with_provider("mistral")
                .with_status(429),
        );
        let mut body = serde_json::json!({ "error": "testo tecnico", "code": "PROVIDER_ERROR" });
        originale.write_into(&mut body);
        // I campi storici non vengono toccati: le tre chiavi sono ADDITIVE.
        assert_eq!(body["error"], "testo tecnico");
        assert_eq!(body["code"], "PROVIDER_ERROR");

        let riletto = RenderedError::from_wire(&body).expect("la resa attraversa il confine");
        assert_eq!(riletto.message, originale.message);
        assert_eq!(riletto.code, originale.code);
        assert_eq!(riletto.detail, originale.detail);

        // Nessuna resa trasportata -> None, mai una frase inventata.
        assert!(RenderedError::from_wire(&serde_json::json!({ "error": "solo tecnico" })).is_none());
        assert!(
            RenderedError::from_wire(&serde_json::json!({ "user_message": "   " })).is_none(),
            "una frase vuota non e' una resa"
        );
    }
}
