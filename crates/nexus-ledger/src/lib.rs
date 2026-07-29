//! Contabilita' di `ai_usage_ledger`: PUNTO UNICO (regola L / ADR 0026).
//!
//! Risponde a due domande, e a nessun'altra: *quale riga si scrive per questa
//! chiamata?* e *quanto ha gia' consumato questo scope?* Le POLICY su cosa fare
//! con la risposta restano dei chiamanti — il gateway degrada e annota, mcp-core
//! respinge la richiesta quando la quota e' esaurita.
//!
//! ## Perche' esiste
//!
//! La tabella aveva DUE scrittori in due crate che non si vedevano fra loro, piu'
//! due scrittori minori, e le SQL erano tenute gemelle a mano. Il commento sopra
//! `SQL_UPDATE_LEDGER_FINALIZE` in mcp-core lo dichiarava senza mezzi termini:
//! "Gemella di `SQL_INSERT_LEDGER_TESTO` nel gateway". Le due meta' divergevano
//! su assi che cambiano i soldi:
//!
//! | Asse | mcp-core | nexus-gateway |
//! |---|---|---|
//! | forma della riga | INSERT `reserved` -> UPDATE `finalized` | INSERT gia' `finalized` |
//! | consumo per la quota | una query batch (UNNEST) in transazione, `FOR UPDATE` | una query per quota, senza lock |
//! | chi addebita la chiamata | lo decideva da solo | non sapeva dell'altro |
//!
//! L'ultima riga e' il difetto misurato il 2026-07-27: un solo messaggio in chat,
//! DUE righe `finalized` con lo stesso `run_id`, stessi token, stesso costo
//! (0.002339 ciascuna, 0.004678 addebitati). Una la scriveva il gateway dentro la
//! richiesta HTTP, l'altra la finalizzava mcp-core al ritorno. Il doppio conteggio
//! c'era da sempre ed era invisibile solo perche' la coppia provider/modello
//! prenotata era impossibile e il listino la prezzava zero.
//!
//! Quel difetto e' chiuso da [`settle`], che legge il segnale strutturato
//! [`LedgerEntry`] invece di dedurre l'addebito dall'esito della chiamata
//! (regola M). Ma finche' i due scrittori restavano in due crate ciechi l'uno
//! all'altro, la verifica "una chiamata -> UNA riga finalizzata" non era
//! scrivibile: era spezzata in due test in due crate, e quello di mcp-core doveva
//! SEMINARE a mano la riga del gateway, perche' il suo produttore vero non era
//! raggiungibile da li'. Un test che fabbrica un input gia' prodotto altrove
//! fissa l'assunto che dovrebbe verificare (regola O).
//!
//! Qui i due produttori sono nello stesso crate, e la verifica e' un test solo:
//! `tests/una_sola_riga_finalizzata.rs`.
//!
//! ## Cosa NON vive qui
//!
//! - Il listino: `nexus-pricing` (quanto costa una chiamata). Qui si SCRIVE cio'
//!   che quello calcola.
//! - L'estrazione dell'identita' dai tipi di un chiamante (`LlmRequest` del
//!   gateway, `OrchestratorInput` di mcp-core): e' roba dei loro tipi, e ognuno
//!   la fa a casa propria prima di chiamare. Qui vive solo la REGOLA che decide
//!   se quell'identita' e' utilizzabile ([`identity_from_metadata`]): quella se
//!   la pongono in due, dai lati opposti del wire, e due copie la renderebbero
//!   inservibile proprio quando serve confrontarle.
//! - I report di consumo (viste admin, breakdown per run): sono LETTURE di
//!   presentazione, non contabilita' che decide.
//!
//! Meccanismo (regola L): logica stateless + IO singolo -> funzioni in un modulo.
//! Niente trait, niente struct con stato.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod cache_hitrate;
mod quote;
mod scrittura;

pub use cache_hitrate::{
    observed_cache_hit_rates, HitRateWindow, MIN_SAMPLES_SETTING, WINDOW_SETTING,
};
pub use quote::{
    active_quotas, check_quota, usage_for_quotas, usage_for_scope, Consumption, QuotaLock,
    QuotaPolicy,
};
pub use scrittura::{
    finalize, insert_marker, record_media, record_tokens, release, reserve, settle,
};

/// Chi ha fatto la chiamata. Le due colonne sono NOT NULL e portano una FK:
/// arrivano qui gia' risolte, questo crate non le indovina.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    pub user_id: Uuid,
    pub project_id: Uuid,
}

/// La REGOLA unica che decide se un'identita' contabile e' utilizzabile
/// (regola L).
///
/// Non e' l'estrazione dai tipi di un chiamante — quella resta a casa sua — ma
/// il criterio, e vive qui perche' e' la stessa domanda posta dai due lati
/// opposti del wire: chi ESEGUE la pone sulla richiesta che riceve, per decidere
/// se scrivere la riga; chi ha PRENOTATO la pone sulla richiesta che ha mandato,
/// per sapere se un "non ho scritto" e' legittimo o sospetto (vedi
/// [`Declaration::audit`]). Con due copie della regola quel confronto sarebbe una
/// recita: ciascun lato direbbe di se' cio' che gli fa comodo, e le due meta'
/// divergerebbero senza che nulla le tenga insieme.
///
/// La convenzione Nexus e' `tenant_id = project_id`.
pub fn identity_from_metadata(project_id: &str, user_id: &str) -> Option<Identity> {
    let project_id = project_id.trim();
    let user_id = user_id.trim();
    if project_id.is_empty() || user_id.is_empty() {
        return None;
    }
    match (Uuid::parse_str(project_id), Uuid::parse_str(user_id)) {
        (Ok(project_id), Ok(user_id)) => Some(Identity {
            user_id,
            project_id,
        }),
        _ => None,
    }
}

/// La riga di ledger che uno scrittore DICHIARA di aver scritto.
///
/// Non e' telemetria: e' il segnale STRUTTURATO (regola M) su cui l'altro
/// scrittore decide di non addebitare una seconda volta la stessa chiamata. La
/// domanda a cui risponde e' "il gateway ha scritto?", che NON e' "la chiamata e'
/// riuscita": ci sono percorsi in cui la chiamata riesce e la riga non si scrive
/// (richiesta senza identita', identita' non-UUID, INSERT fallita), e dedurre
/// l'una dall'altra perderebbe l'addebito.
///
/// Vive qui perche' e' il vocabolario CONDIVISO fra i due scrittori: prima era
/// due struct gemelle, una per lato del wire (`nexus_gateway::types::LedgerEntry`
/// e `mcp_core::nexus_gateway::GwLedgerEntry`), che nessun compilatore teneva
/// allineate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// `ai_usage_ledger.id` della riga scritta: permette di CORRELARE la
    /// prenotazione del chiamante alla riga che porta davvero l'addebito.
    pub id: Uuid,
    /// Costo totale REGISTRATO sulla riga (non una stima ricalcolata altrove).
    pub total_cost: f64,
    /// Currency della riga, come scritta nel ledger.
    pub currency: String,
}

/// Cosa ha fatto della contabilita' chi ha ESEGUITO la chiamata, dichiarato da
/// lui sul wire.
///
/// Prima questo posto era occupato da un `Option<LedgerEntry>`, e tre risposte
/// diverse collassavano tutte in `None` — anzi in NIENTE, perche'
/// `skip_serializing_if` toglie dal JSON anche il `null`. Chi legge non poteva
/// distinguere "ho deciso di non scrivere" da "non parlo questo contratto", e la
/// differenza e' denaro: un gateway di build precedente la riga l'ha scritta
/// comunque, quindi il suo silenzio letto come "nessuno ha addebitato" fa
/// rinascere il doppio addebito del 2026-07-27 — in silenzio, coi test verdi.
///
/// Da qui in poi l'unica cosa non dichiarabile e' l'ASSENZA del campo, e
/// significa una cosa sola: chi ha eseguito non parla questa versione del
/// contratto. E' [`Declaration::Muta`], e non e' innocua.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum LedgerOutcome {
    /// Riga scritta: e' lei a portare l'addebito, e chi ha prenotato rilascia.
    Written(LedgerEntry),
    /// Nessuna identita' contabile nella richiesta (`GwMetadata::default`, o
    /// stringhe non-UUID): non c'era nessuno a cui addebitare. Chi ha prenotato
    /// DEVE finalizzare, o l'addebito sparisce.
    NoIdentity,
    /// La INSERT e' fallita. Il consumo di chi ha eseguito non e' nel ledger,
    /// l'addebito lo porta la prenotazione, e il fallimento va detto: la
    /// scrittura e' best-effort, cioe' fallisce senza che nessuno se ne accorga.
    WriteFailed,
}

impl LedgerOutcome {
    /// La riga scritta, quando c'e'. E' l'unica domanda su cui si decide chi
    /// addebita: gli altri due esiti sono "non ho scritto" detti con precisione.
    pub fn entry(&self) -> Option<&LedgerEntry> {
        match self {
            LedgerOutcome::Written(e) => Some(e),
            LedgerOutcome::NoIdentity | LedgerOutcome::WriteFailed => None,
        }
    }

    /// Identificatore canonico per i log (regola N). Combacia coi tag serde:
    /// e' la stessa parola che viaggia sul wire.
    pub fn as_str(&self) -> &'static str {
        match self {
            LedgerOutcome::Written(_) => "written",
            LedgerOutcome::NoIdentity => "no_identity",
            LedgerOutcome::WriteFailed => "write_failed",
        }
    }
}

/// Cio' che chi ha prenotato ha potuto LEGGERE della dichiarazione contabile.
///
/// Tre stati, non due, e il terzo e' il motivo per cui questo tipo esiste: un
/// campo presente ma non deserializzabile non e' un campo assente. Finiva in
/// `None` per via di un `.ok()`, e `None` significa "nessuno ha addebitato" ->
/// finalizza -> doppio addebito, di nuovo in silenzio.
#[derive(Debug, Clone)]
pub enum Declaration {
    /// Chi ha eseguito ha dichiarato, e la dichiarazione si legge.
    Detta(LedgerOutcome),
    /// Nessuna dichiarazione: il campo non c'era. Chi ha eseguito non parla
    /// questa versione del contratto — potrebbe aver scritto lo stesso.
    Muta,
    /// Il campo c'era e non si e' potuto leggere: i due lati del wire hanno
    /// contratti divergenti. Non e' un'assenza, e' un difetto.
    Illeggibile,
}

impl Declaration {
    /// Dalla forma che viaggia sul wire non-streaming (`Option<LedgerOutcome>`,
    /// dove l'assenza del campo e' gia' stata risolta da serde).
    pub fn dal_wire(outcome: Option<LedgerOutcome>) -> Self {
        match outcome {
            Some(o) => Declaration::Detta(o),
            None => Declaration::Muta,
        }
    }

    /// La riga scritta da chi ha eseguito, se ne ha dichiarata una.
    pub fn entry(&self) -> Option<&LedgerEntry> {
        match self {
            Declaration::Detta(o) => o.entry(),
            Declaration::Muta | Declaration::Illeggibile => None,
        }
    }

    /// Identificatore canonico dello stato letto, per i log (regola N).
    pub fn as_str(&self) -> &'static str {
        match self {
            Declaration::Detta(o) => o.as_str(),
            Declaration::Muta => "undeclared",
            Declaration::Illeggibile => "unreadable",
        }
    }

    /// Il verdetto su questa dichiarazione, dato cio' che il chiamante SA di
    /// aver mandato.
    ///
    /// `identita_inviata` non e' un'opinione: si misura sulla richiesta VERA con
    /// [`identity_from_metadata`], la stessa regola che usa chi esegue per
    /// decidere se scrivere. E' quel confronto a rendere sospetto il silenzio:
    /// senza identita' nessuno avrebbe scritto e tacere e' corretto, con
    /// l'identita' il silenzio vale "o sei una build vecchia che ha scritto
    /// comunque, o la INSERT e' fallita" — e il primo dei due costa il doppio.
    pub fn audit(&self, identita_inviata: bool) -> DeclarationAudit {
        match self {
            Declaration::Detta(LedgerOutcome::Written(_)) => DeclarationAudit::Coerente,
            Declaration::Detta(LedgerOutcome::NoIdentity) => {
                if identita_inviata {
                    DeclarationAudit::IdentitaPersa
                } else {
                    DeclarationAudit::Coerente
                }
            }
            Declaration::Detta(LedgerOutcome::WriteFailed) => DeclarationAudit::ScritturaFallita,
            Declaration::Muta => {
                if identita_inviata {
                    DeclarationAudit::NonDichiarata
                } else {
                    DeclarationAudit::Coerente
                }
            }
            Declaration::Illeggibile => DeclarationAudit::Illeggibile,
        }
    }
}

/// Verdetto su una dichiarazione contabile. Segnale STRUTTURATO (regola M): chi
/// indaga filtra su [`DeclarationAudit::code`], non sul testo del messaggio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationAudit {
    /// Cio' che e' stato dichiarato e' cio' che ci si aspettava.
    Coerente,
    /// Nessuna dichiarazione su una chiamata che PORTAVA identita' contabile.
    NonDichiarata,
    /// "Nessuna identita'" dichiarato su una richiesta che l'identita' ce
    /// l'aveva: si e' persa fra i due processi.
    IdentitaPersa,
    /// Chi ha eseguito ha provato a scrivere e non c'e' riuscito.
    ScritturaFallita,
    /// Dichiarazione presente e illeggibile.
    Illeggibile,
}

impl DeclarationAudit {
    /// Identificatore canonico (regola N): e' il campo su cui si filtra.
    pub fn code(&self) -> &'static str {
        match self {
            DeclarationAudit::Coerente => "coherent",
            DeclarationAudit::NonDichiarata => "undeclared",
            DeclarationAudit::IdentitaPersa => "identity_lost",
            DeclarationAudit::ScritturaFallita => "write_failed",
            DeclarationAudit::Illeggibile => "unreadable",
        }
    }

    /// Vero quando qualcuno deve andare a guardare.
    pub fn sospetta(&self) -> bool {
        !matches!(self, DeclarationAudit::Coerente)
    }

    /// Cosa rischia chi legge la riga di log. Sta qui e non nel call site
    /// perche' la frase e' una CONSEGUENZA del verdetto, e un verdetto nuovo
    /// deve costringere a scriverne una.
    pub fn conseguenza(&self) -> &'static str {
        match self {
            DeclarationAudit::Coerente => "",
            DeclarationAudit::NonDichiarata => {
                "chi ha eseguito potrebbe aver scritto la riga lo stesso (build che non \
                 dichiara l'esito contabile): questa chiamata sta per essere addebitata DUE volte"
            }
            DeclarationAudit::IdentitaPersa => {
                "l'identita' contabile mandata non e' arrivata a chi ha eseguito: il suo \
                 consumo resta fuori dal ledger, addebita la prenotazione"
            }
            DeclarationAudit::ScritturaFallita => {
                "la INSERT di chi ha eseguito e' fallita: l'addebito lo porta la prenotazione, \
                 ma il suo database sta rifiutando le scritture"
            }
            DeclarationAudit::Illeggibile => {
                "dichiarazione presente e non deserializzabile: i due lati del wire hanno \
                 contratti divergenti, e l'addebito si decide alla cieca"
            }
        }
    }
}

/// Quota superata. I chiamanti la traducono nel proprio confine: HTTP 403 nel
/// gateway, `billing_rejected` nella chat.
///
/// Prima era due tipi gemelli (`QuotaExceeded` nel gateway,
/// `QuotaExceededError` in mcp-core) con lo stesso `Display` ricopiato a mano.
#[derive(Debug, thiserror::Error)]
#[error("quota_exceeded:{scope}:{reason}")]
pub struct QuotaExceeded {
    pub scope: String,
    pub reason: String,
}

/// Una prenotazione aperta: la riga `reserved` piu' cio' che serve a chiuderla.
#[derive(Debug, Clone)]
pub struct Reservation {
    pub ledger_id: Uuid,
    /// Esito STRUTTURATO del listino al momento della prenotazione (regola M).
    ///
    /// Prima era un `PriceSnapshot` secco: quando il prezzo non era noto, chi
    /// prenotava era costretto a fabbricare un `{0, 0, currency}` e a infilarlo
    /// qui, cosi' la finalizzazione calcolava un costo 0 senza sapere perche'. Il
    /// magic fallback sopravviveva alla struct. Con l'enum, "non so quanto costa"
    /// resta dichiarato fino alla scrittura del ledger.
    pub lookup: nexus_pricing::PriceLookup,
    /// Currency da annotare sulle righe di questa prenotazione. Serve anche
    /// quando il prezzo non e' noto: la colonna e' NOT NULL.
    pub currency: String,
}

/// Chi porta l'addebito di una chiamata chiusa.
///
/// Enum e non booleano perche' il valore finisce in un log su cui si indaga, e
/// `charged_by=executor` dice a chi legge QUALE riga andare a guardare nel
/// ledger — la riga di chi ha eseguito, non la prenotazione, che a quel punto e'
/// `released` con importo zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargedBy {
    /// La riga di chi ha ESEGUITO la chiamata; la prenotazione e' stata
    /// rilasciata e punta a quella con `superseded_by_ledger_id`.
    Executor,
    /// La prenotazione di chi ha chiamato, finalizzata coi numeri reali.
    Reservation,
}

impl ChargedBy {
    /// Identificatore canonico per i log (regola N).
    pub fn as_str(&self) -> &'static str {
        match self {
            ChargedBy::Executor => "executor",
            ChargedBy::Reservation => "reservation",
        }
    }
}

/// Esito della chiusura contabile di una chiamata RIUSCITA.
#[derive(Debug, Clone)]
pub struct Settlement {
    /// Costo effettivamente REGISTRATO nel ledger per questa chiamata.
    pub total_cost: f64,
    pub currency: String,
    /// Quale delle due righe porta l'addebito. E' il campo che rende
    /// riconciliabile un importo mostrato all'utente con una riga del ledger:
    /// senza, davanti a un costo che non torna non si sa nemmeno da che parte
    /// cominciare a cercare.
    pub charged_by: ChargedBy,
}

/// I conteggi come il ledger li registra.
///
/// `tokens.prompt_tokens` e' il LORDO e i due conteggi di cache ne sono
/// SOTTOINSIEMI: la convenzione e' fissata alla fonte da `LlmUsage::normalized`
/// (`crates/nexus-gateway/src/types.rs`), qui si trasporta e basta.
#[derive(Debug, Clone)]
pub struct LedgerUsage {
    pub tokens: nexus_pricing::TokenUsage,
    /// Totale scritto in colonna. Di norma il DERIVATO (prompt lordo +
    /// completion): i token di cache sono gia' dentro il prompt e sommarli di
    /// nuovo li conterebbe due volte. Resta esplicito perche' alcune fonti
    /// dichiarano un totale proprio, e in quel caso quello prevale.
    pub total_tokens: i64,
}

impl LedgerUsage {
    /// Il totale DERIVATO da prompt lordo + completion.
    pub fn derived(tokens: nexus_pricing::TokenUsage) -> Self {
        let total_tokens = tokens.total_tokens();
        Self {
            tokens,
            total_tokens,
        }
    }

    /// Il totale DICHIARATO dalla fonte, quando ne porta uno proprio.
    pub fn with_declared_total(tokens: nexus_pricing::TokenUsage, total_tokens: i64) -> Self {
        Self {
            tokens,
            total_tokens: total_tokens.max(0),
        }
    }
}

/// Modalita' della chiamata non-testuale. Finisce in `ai_usage_ledger.usage_kind`,
/// che ha un CHECK: le stringhe qui sotto devono combaciare con la mig 0634.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    /// Audio in ingresso: trascrizione.
    AudioIn,
    /// Audio in uscita: sintesi vocale.
    AudioOut,
}

impl MediaKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Video => "video",
            MediaKind::AudioIn => "audio_in",
            MediaKind::AudioOut => "audio_out",
        }
    }
}

/// Da dove viene la quantita' registrata.
///
/// Non e' un dettaglio: nessuno dei quattro provider dichiara oggi quanto ha
/// prodotto (OpenAI Images scarta l'usage, la trascrizione gira con
/// `response_format=json` che non porta la durata, il video non riporta i
/// secondi effettivi). Fatturare cio' che abbiamo CHIESTO e' accettabile, ma chi
/// legge la riga deve poter distinguere un dato misurato da una stima.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantitySource {
    /// Contata sulla risposta del provider.
    Provider,
    /// Dedotta da cio' che e' stato richiesto.
    Request,
    /// Non conoscibile.
    None,
}

impl QuantitySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            QuantitySource::Provider => "provider",
            QuantitySource::Request => "request",
            QuantitySource::None => "none",
        }
    }
}

/// Quanto e' stato consumato da una chiamata non-testuale.
#[derive(Debug, Clone)]
pub struct MediaUsage {
    pub kind: MediaKind,
    /// `None` quando la quantita' non e' conoscibile: il CHECK
    /// `chk_ledger_quantity_coerente` impone che allora `source` sia `None`, cosi'
    /// "non lo so" non puo' travestirsi da zero.
    pub quantity: Option<f64>,
    pub unit: nexus_pricing::UsageUnit,
    pub source: QuantitySource,
}

impl MediaUsage {
    /// Quantita' contata sulla risposta del provider.
    pub fn misurata(kind: MediaKind, unit: nexus_pricing::UsageUnit, quantity: f64) -> Self {
        Self {
            kind,
            quantity: Some(quantity),
            unit,
            source: QuantitySource::Provider,
        }
    }

    /// Quantita' dedotta dalla richiesta (il provider non la dichiara).
    pub fn da_richiesta(kind: MediaKind, unit: nexus_pricing::UsageUnit, quantity: f64) -> Self {
        Self {
            kind,
            quantity: Some(quantity),
            unit,
            source: QuantitySource::Request,
        }
    }

    /// Consumo avvenuto ma non quantificabile: la riga si scrive lo stesso
    /// (chi, cosa, quale modello), senza inventare un numero.
    pub fn non_quantificabile(kind: MediaKind, unit: nexus_pricing::UsageUnit) -> Self {
        Self {
            kind,
            quantity: None,
            unit,
            source: QuantitySource::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_pricing::UsageUnit;

    // ── La regola dell'identita' contabile ─────────────────────

    /// La stessa regola per i due lati del wire: chi esegue la applica alla
    /// richiesta che riceve, chi ha prenotato a quella che ha mandato.
    #[test]
    fn lidentita_e_utilizzabile_solo_se_sono_due_uuid() {
        let (u, p) = (Uuid::new_v4(), Uuid::new_v4());
        let id = identity_from_metadata(&p.to_string(), &u.to_string()).expect("due UUID buoni");
        // `tenant_id` e' il PROGETTO, `user_id` l'UTENTE: lo scambio non e' un
        // errore di compilazione e addebiterebbe al progetto sbagliato.
        assert_eq!(id.project_id, p);
        assert_eq!(id.user_id, u);

        assert!(identity_from_metadata("", &u.to_string()).is_none());
        assert!(identity_from_metadata(&p.to_string(), "   ").is_none());
        assert!(identity_from_metadata("non-un-uuid", "nemmeno").is_none());
    }

    // ── Dichiarazione contabile: cosa e' stato detto ───────────

    fn una_riga() -> LedgerEntry {
        LedgerEntry {
            id: Uuid::new_v4(),
            total_cost: 9.0,
            currency: "USD".into(),
        }
    }

    /// I tre esiti restano DISTINTI sul wire. Prima erano tutti `None`, e due di
    /// essi non arrivavano nemmeno nel JSON.
    #[test]
    fn i_tre_esiti_viaggiano_distinti_sul_wire() {
        let riga = una_riga();
        let scritto = serde_json::to_value(LedgerOutcome::Written(riga.clone())).expect("serde");
        assert_eq!(scritto["outcome"], "written");
        assert_eq!(scritto["id"], serde_json::json!(riga.id.to_string()));
        assert_eq!(scritto["currency"], "USD");

        assert_eq!(
            serde_json::to_value(LedgerOutcome::NoIdentity).expect("serde")["outcome"],
            "no_identity"
        );
        assert_eq!(
            serde_json::to_value(LedgerOutcome::WriteFailed).expect("serde")["outcome"],
            "write_failed"
        );

        // E si rileggono per quello che sono: solo `Written` porta una riga.
        for (json, atteso, porta_riga) in [
            (scritto, "written", true),
            (serde_json::json!({ "outcome": "no_identity" }), "no_identity", false),
            (serde_json::json!({ "outcome": "write_failed" }), "write_failed", false),
        ] {
            let o: LedgerOutcome = serde_json::from_value(json).expect("rilettura");
            assert_eq!(o.as_str(), atteso);
            assert_eq!(o.entry().is_some(), porta_riga);
        }
    }

    /// Il verdetto dipende da cio' che il chiamante SA di aver mandato: lo stesso
    /// silenzio e' innocuo senza identita' e sospetto con l'identita'.
    ///
    /// MUTAZIONE: facendo ritornare `Coerente` al ramo `Muta` con identita'
    /// inviata, la riga di `NonDichiarata` fallisce — ed e' il caso in cui una
    /// chiamata viene addebitata due volte.
    #[test]
    fn il_verdetto_confronta_la_dichiarazione_con_cio_che_e_stato_mandato() {
        use DeclarationAudit::*;
        let casi: [(Declaration, bool, DeclarationAudit); 8] = [
            (Declaration::Detta(LedgerOutcome::Written(una_riga())), true, Coerente),
            (Declaration::Detta(LedgerOutcome::Written(una_riga())), false, Coerente),
            // Senza identita' mandata, "non ho scritto" e' la risposta giusta.
            (Declaration::Detta(LedgerOutcome::NoIdentity), false, Coerente),
            // Con identita' mandata, la stessa frase dice che si e' persa.
            (Declaration::Detta(LedgerOutcome::NoIdentity), true, IdentitaPersa),
            (Declaration::Detta(LedgerOutcome::WriteFailed), true, ScritturaFallita),
            (Declaration::Detta(LedgerOutcome::WriteFailed), false, ScritturaFallita),
            // Il silenzio: innocuo senza identita' (nessuno avrebbe scritto),
            // sospetto con l'identita' (una build vecchia ha scritto lo stesso).
            (Declaration::Muta, false, Coerente),
            (Declaration::Muta, true, NonDichiarata),
        ];
        for (decl, identita_inviata, atteso) in casi {
            let verdetto = decl.audit(identita_inviata);
            assert_eq!(
                verdetto, atteso,
                "dichiarazione '{}' con identita_inviata={identita_inviata}",
                decl.as_str()
            );
        }
        assert_eq!(Declaration::Illeggibile.audit(false), Illeggibile);
        assert_eq!(Declaration::Illeggibile.audit(true), Illeggibile);
    }

    /// Ogni verdetto diverso da `Coerente` e' sospetto e porta con se' la propria
    /// conseguenza: un verdetto nuovo senza frase resterebbe muto proprio nel log
    /// in cui qualcuno lo sta cercando.
    #[test]
    fn ogni_verdetto_sospetto_dice_cosa_si_rischia() {
        use DeclarationAudit::*;
        assert!(!Coerente.sospetta());
        assert!(Coerente.conseguenza().is_empty());
        for v in [NonDichiarata, IdentitaPersa, ScritturaFallita, Illeggibile] {
            assert!(v.sospetta(), "{} deve essere sospetto", v.code());
            assert!(
                !v.conseguenza().is_empty(),
                "{}: verdetto sospetto senza conseguenza dichiarata",
                v.code()
            );
        }
    }

    // ── Vocabolario del consumo media ──────────────────────────
    //
    // Le stringhe di `MediaKind`, `QuantitySource` e `UsageUnit` finiscono in
    // colonne con un CHECK: se divergono dalla migrazione, l'INSERT fallisce a
    // RUNTIME ed e' best-effort, cioe' il consumo torna invisibile — il difetto
    // che la mig 0634 ha appena chiuso, di nuovo e in silenzio.
    //
    // Il testo della migrazione e' incluso a compile-time dal file VERO applicato
    // al database (regola O): non e' una copia delle costanti riscritta nel test,
    // che direbbe soltanto che il codice e' uguale a se stesso.
    const MIGRAZIONE_0634: &str = include_str!("../../../db/migrations/0634_media_usage_units.sql");

    /// Ogni `usage_kind` che il codice sa produrre deve essere ammesso dal CHECK.
    #[test]
    fn i_kind_del_codice_sono_ammessi_dalla_migrazione() {
        for kind in [
            MediaKind::Image,
            MediaKind::Video,
            MediaKind::AudioIn,
            MediaKind::AudioOut,
        ] {
            let atteso = format!("'{}'", kind.as_str());
            assert!(
                MIGRAZIONE_0634.contains(&atteso),
                "usage_kind {atteso} non compare nella migrazione 0634: l'INSERT fallirebbe sul CHECK"
            );
        }
    }

    /// Idem per la provenienza della quantita'.
    #[test]
    fn le_fonti_della_quantita_sono_ammesse_dalla_migrazione() {
        for source in [
            QuantitySource::Provider,
            QuantitySource::Request,
            QuantitySource::None,
        ] {
            let atteso = format!("'{}'", source.as_str());
            assert!(
                MIGRAZIONE_0634.contains(&atteso),
                "quantity_source {atteso} non compare nella migrazione 0634"
            );
        }
    }

    /// E per le unita', che vivono nel crate pricing ma finiscono in due CHECK
    /// (ledger e listino unitario).
    #[test]
    fn le_unita_sono_ammesse_dalla_migrazione() {
        for unit in [UsageUnit::Image, UsageUnit::Second, UsageUnit::Character] {
            let atteso = format!("'{}'", unit.as_str());
            assert!(
                MIGRAZIONE_0634.contains(&atteso),
                "quantity_unit {atteso} non compare nella migrazione 0634"
            );
        }
    }

    /// "Non lo so" e "zero" restano distinguibili: il costruttore per il consumo
    /// non quantificabile non deve produrre una quantita'.
    #[test]
    fn non_quantificabile_non_inventa_uno_zero() {
        let u = MediaUsage::non_quantificabile(MediaKind::AudioIn, UsageUnit::Second);
        assert_eq!(u.quantity, None);
        assert_eq!(u.source, QuantitySource::None);
        // E' la coppia che il CHECK chk_ledger_quantity_coerente impone:
        // (quantity_source = 'none') = (quantity IS NULL).
        let misurata = MediaUsage::misurata(MediaKind::Image, UsageUnit::Image, 3.0);
        assert_eq!(misurata.quantity, Some(3.0));
        assert_ne!(misurata.source, QuantitySource::None);
        let stimata = MediaUsage::da_richiesta(MediaKind::Video, UsageUnit::Second, 8.0);
        assert_eq!(stimata.source, QuantitySource::Request);
        assert!(stimata.quantity.is_some());
    }
}
