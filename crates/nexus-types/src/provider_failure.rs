//! «Che cosa il gateway ha DICHIARATO di stare rifiutando, e per quanto» —
//! il contratto del blocco `details.failures` letto dai DUE lati del confine.
//!
//! ## Il difetto che questo modulo chiude
//!
//! Lo stato «questo fornitore e' utilizzabile adesso?» aveva DUE risposte in
//! DUE processi, e chi SCEGLIE consultava quella che non sapeva nulla:
//!
//!   - il gateway tiene il proprio `CooldownManager` (in-memory, suo processo)
//!     e vi scrive quando riceve un 429, un `Retry-After` o un billing error —
//!     cioe' impara dalle chiamate VERE;
//!   - mcp-core tiene `provider_cooldown` (in-memory, altro processo) e la
//!     selezione dei modelli esclude i fornitori che vi trova
//!     (`EligibilityFilter::apply_cooldown`) — ma quel registro imparava solo
//!     dal proprio probe periodico e dal pannello provider, MAI dai rifiuti che
//!     il gateway gli comunicava a ogni chiamata.
//!
//! Il segnale attraversava gia' il confine, tipizzato (`details.primary_cause`
//! + `details.failures[].class`), e un consumatore lo leggeva gia' per decidere
//! il failover (`classify_gateway_error`): nessuno pero' ne traeva la
//! conseguenza sul registro locale, quindi mcp-core continuava a CONVOCARE
//! fornitori che il gateway avrebbe rifiutato.
//!
//! MISURATO il 12/08/2026 sul gate duale dopo il deploy delle 22:50: tre
//! validatori convocati, `openai`, `kimi` e `openrouter`, e tutti e tre
//! astenuti con causa `cooldown` — il gateway li stava rifiutando senza
//! chiamarli, mentre la selezione di mcp-core li aveva appena scelti come i
//! migliori disponibili.
//!
//! Prima del fix del ripristino (commit `119acd55`) la desincronizzazione
//! durava al massimo un ciclo di re-probe del gateway (10 minuti), perche' era
//! il gateway stesso a cancellare i propri cooldown troppo presto. Ora che una
//! scadenza dichiarata dal fornitore viene onorata fino in fondo — che e' il
//! comportamento giusto — la cecita' di mcp-core dura quanto il cooldown: per
//! un `Retry-After` di groq sono ~1800s, per un billing 6h. Il fix precedente
//! non ha creato questo difetto: lo ha reso strutturale, e quindi visibile.
//!
//! ## Perche' il vocabolario sta QUI e non nei due lati
//!
//! Le CLASSI e i nomi dei campi sono il contratto, e un contratto scritto due
//! volte diverge in silenzio: un rename da un lato lascerebbe l'altro a leggere
//! una chiave che non esiste piu', cioe' un trasporto muto con tutti i test
//! verdi da entrambe le parti (regola O). Vivendo in `nexus-types` — da cui
//! ENTRAMBI i crate dipendono gia' — a rompersi e' la compilazione, non
//! l'esercizio.

use serde_json::Value;

/// Le classi con cui il gateway qualifica il fallimento di UN fornitore della
/// chain. Vocabolario CHIUSO e canonico (regola N): identificatori in inglese,
/// scritti da `nexus-gateway` e letti da `mcp-core`.
pub mod classe {
    /// La chiamata e' stata fatta e il fornitore ha risposto «niente credito».
    pub const BILLING: &str = "billing";
    /// Il fornitore e' stato SALTATO senza chiamarlo: cooldown di credito attivo.
    pub const COOLDOWN_BILLING: &str = "cooldown_billing";
    /// Il fornitore e' stato SALTATO senza chiamarlo: cooldown transitorio attivo.
    pub const COOLDOWN: &str = "cooldown";
    /// La chiamata e' stata fatta ed e' fallita per una causa transitoria
    /// (429, 5xx, timeout del tentativo): il gateway ha marcato un cooldown breve.
    pub const TRANSIENT: &str = "transient";
    /// Errore deterministico della richiesta: ritentarla non cambia nulla, e il
    /// fornitore e' sano.
    pub const CLIENT_ERROR: &str = "client_error";
    /// Richiesta troppo grande per QUESTO modello: un altro puo' accettarla.
    pub const CONTEXT_TOO_LONG: &str = "context_too_long";
    /// Ammissione rifiutata: la prenotazione della richiesta supera il credito
    /// RESIDUO del fornitore, che pero' ha credito e sta servendo. Non e'
    /// [`BILLING`]: li' il rimedio e' ricaricare e il cooldown dura ore, qui
    /// basta una richiesta piu' piccola (o un altro fornitore, subito).
    pub const REQUEST_EXCEEDS_CREDIT: &str = "request_exceeds_credit";
    /// HTTP 200 senza output utile: il fornitore e' sano, il turno improduttivo.
    pub const EMPTY_COMPLETION: &str = "empty_completion";
}

/// La classe con cui il gateway decide retry e cooldown di UNA chiamata.
///
/// Vive QUI, accanto al vocabolario di wire, perche' la sua proiezione sul wire
/// e' [`classe`] — che mcp-core legge gia' da questo crate. Finche' l'enum stava
/// nel gateway, la traduzione `classe -> stringa` era scritta a mano in DUE
/// punti a 700 righe di distanza (`provider_facts_from_error` e
/// `CallFailure::from_error`): la stessa domanda con due risposte, che e'
/// esattamente la forma di difetto che la regola L vieta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClasseErrore {
    /// Crediti/quota/fatturazione: il fornitore e' inutilizzabile finche' non si
    /// ricarica. NIENTE retry; cooldown lungo (`mark_billing`).
    Billing,
    /// Errore lato richiesta o configurazione (4xx client): ritentare NON aiuta
    /// e mettere in cooldown il FORNITORE e' sbagliato (il problema e' la
    /// singola richiesta o il singolo modello). NIENTE retry, NIENTE cooldown.
    ClientError,
    /// Richiesta troppo grande per QUESTO fornitore (413 request_too_large).
    /// Come `ClientError` per il retry, ma cross-provider-RECUPERABILE: un
    /// fornitore a finestra piu' grande accetterebbe la stessa richiesta, quindi
    /// a livello motore e' un failover e non una chiusura.
    ContextTooLong,
    /// La richiesta non e' stata AMMESSA perche' la sua prenotazione supera il
    /// credito RESIDUO: il fornitore ha credito e sta servendo, e' questa
    /// richiesta a non starci dentro. Come [`Self::ContextTooLong`] — ritentare
    /// identico e' inutile, il fornitore NON va in cooldown, il motore ripiega
    /// cross-provider — ma la causa e' un'altra e con essa il rimedio: qui si
    /// ricarica o si chiede meno, li' serve una finestra piu' grande.
    ///
    /// Distinta da [`Self::Billing`] perche' quel cooldown dura sei ore: il
    /// saldo openrouter misurato il 13/08/2026 era di 10,01 dollari RESIDUI, e i
    /// 41 messaggi distinti dicono tutti «can only afford N» con N fra 432 e
    /// 64811 token — mai zero.
    ///
    /// Non produce esclusione nel registro locale: [`EsclusioneDichiarata`] la
    /// lascia cadere su [`EsclusioneDichiarata::Nessuna`], che e' il punto —
    /// `Credito` li' significa sei ore fuori per un fornitore che sta servendo.
    RequestExceedsCredit,
    /// Transitorio (429 rate-limit, 5xx, timeout, connessione) o IGNOTO:
    /// ritentabile con backoff. Dopo l'ultimo tentativo, cooldown breve.
    Transient,
}

impl ClasseErrore {
    /// L'UNICA traduzione classe -> stringa di wire.
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Billing => classe::BILLING,
            Self::ClientError => classe::CLIENT_ERROR,
            Self::ContextTooLong => classe::CONTEXT_TOO_LONG,
            Self::RequestExceedsCredit => classe::REQUEST_EXCEEDS_CREDIT,
            Self::Transient => classe::TRANSIENT,
        }
    }

    /// La semantica HTTP, e nient'altro: il 402 lo fissa l'RFC, non l'admin.
    ///
    /// E' l'ULTIMO anello della classificazione — quello che decide quando NON
    /// sappiamo — e resta invariata rispetto alla tabella storica del gateway:
    /// e' cio' che rende la migrazione al catalogo dei codici non-regressiva per
    /// costruzione (dove nulla e' riconosciuto, il risultato e' identico a
    /// prima). E' anche l'unico anello disponibile quando non c'e' un body:
    /// MISURATE migliaia di righe di errore senza status ne' codice (trasporto).
    pub fn da_status(status: u16) -> Self {
        match status {
            402 => Self::Billing,
            413 => Self::ContextTooLong,
            400 | 401 | 403 | 404 | 405 | 406 | 409 | 410 | 415 | 422 => Self::ClientError,
            _ => Self::Transient,
        }
    }
}

/// Chiavi del blocco `details` che il gateway compone e mcp-core rilegge.
pub mod chiave {
    /// Array dei fallimenti per fornitore, in ordine di tentativo.
    pub const FAILURES: &str = "failures";
    /// Classe del fallimento PRIMARIO (il primo dell'array).
    pub const PRIMARY_CAUSE: &str = "primary_cause";
    pub const CLASSE: &str = "class";
    pub const PROVIDER: &str = "provider";
    pub const MODELLO: &str = "model";
    /// Secondi che il gateway dichiara di dover ancora attendere su quel
    /// fornitore. E' il RESIDUO letto dal registro che lo ha appena scritto,
    /// non una durata ricalcolata: qualunque ramo abbia messo il cooldown, il
    /// numero descrive lo stato vero al momento della risposta.
    pub const ATTESA_S: &str = "retry_after_seconds";
    /// CHI e' escluso dall'attesa dichiarata: vedi [`super::portata`].
    ///
    /// Serve un campo suo perche' [`MODELLO`] risponde a un'altra domanda —
    /// «quale modello si stava per chiamare» — ed e' valorizzato anche quando
    /// l'esclusione riguarda tutto il fornitore. Leggerlo come portata
    /// restringerebbe ogni esclusione alla coppia, cioe' renderebbe mcp-core
    /// piu' permissivo del gateway che glielo ha detto.
    pub const PORTATA: &str = "cooldown_scope";
}

/// I valori di [`chiave::PORTATA`]. Vocabolario chiuso e canonico (regola N).
pub mod portata {
    /// L'esclusione vale per tutto il fornitore (credito, auth, trasporto).
    pub const PROVIDER: &str = "provider";
    /// L'esclusione vale per la sola coppia fornitore+modello (tetto di
    /// frequenza o di volume, che e' del modello).
    pub const MODEL: &str = "model";
}

/// Con quale NOME uno stato di salute del fornitore si registra nella colonna
/// `nexus_provider_health_history.error_kind`.
///
/// PERCHE' STA QUI. Quella colonna ha DUE scrittori in due processi — il probe
/// periodico di mcp-core e il `CooldownManager` di nexus-gateway — e fino al
/// 13/08/2026 nominavano lo STESSO stato in due modi. MISURATO sul DB vivo:
/// openai senza credito produceva due righe nello stesso millisecondo,
/// `18:32:09.333824 billing` (gateway) e `18:32:09.335162 credit_balance_too_low`
/// (probe); sull'intero storico, 4245 righe `billing` contro 4893
/// `credit_balance_too_low`. Una query che filtra `error_kind = 'billing'` conta
/// anthropic e perde openai, e viceversa — cioe' la colonna su cui si diagnostica
/// non risponde alla domanda che le si pone.
///
/// PERCHE' VINCE `credit_balance_too_low`, e non `billing`:
///   - la colonna vuole una CAUSA, e il suo vocabolario e' dichiarato dalla
///     mig 0097 (`quota_exceeded`, `credit_balance_too_low`, `billing_required`,
///     `rate_limit`, `timeout`, `auth_error`, `connection_error`, `unknown`):
///     `credit_balance_too_low` vi appartiene, `billing` no;
///   - `billing` non e' una causa, e' la CLASSE con cui il gateway decide
///     ([`ClasseErrore::Billing`], due valori), e non identifica uno stato solo:
///     `ClasseErrore::da_status(402)` ci fa cadere anche il 402 di ammissione di
///     openrouter, che fino alla mig 0709 era credito esaurito per errore e non
///     lo e' mai stato (saldo misurato a 10,01 dollari residui);
///   - `credit_balance_too_low` e' gia' il valore su cui DECIDONO
///     `provider_health_probe::outcome_from_error_class`,
///     `model_health_probe::is_reprobe_candidate` (che lo rilegge da
///     `ai_price_catalog.disabled_reason`, cioe' da dati persistiti) e
///     `agent_turn_setup::classify_by_error_class`. Coniare un terzo nome
///     canonico costringerebbe a riscrivere quei dati per un guadagno estetico.
///
/// Il posto e' `nexus-types` perche' e' l'unico crate che entrambi gli scrittori
/// vedono: nexus-gateway non dipende da mcp-core, e mcp-core dipende da
/// nexus-gateway ma non viceversa.
pub mod stato_salute {
    /// Il fornitore non ha credito: nessuna sua chiamata passa finche' non si
    /// ricarica. E' lo stato che [`ClasseErrore::Billing`] rappresenta al
    /// gateway e che `error_class = "billing_error"` rappresenta in mcp-core.
    pub const CREDIT_BALANCE_TOO_LOW: &str = "credit_balance_too_low";
    /// Guasto non conclusivo (rete, 5xx, timeout, tetto di frequenza): non dice
    /// che il fornitore sia inutilizzabile, dice che questa chiamata non e'
    /// riuscita. I due scrittori usavano gia' questa stessa parola.
    pub const TRANSIENT: &str = "transient";
}

/// La DURATA dell'esclusione di un fornitore senza credito: una sola, in un
/// solo posto.
///
/// IL DIFETTO CHE CHIUDE. Il 13/08/2026 lo stesso evento aveva due durate in due
/// processi: il gateway registrava `duration_seconds=3600`
/// (`gateway.cooldown.billing_seconds`) e mcp-core scriveva
/// `billing_cooldown_until` a sei ore (`provider.cooldown_long_s`), nello stesso
/// istante e per lo stesso fornitore.
///
/// PERCHE' SOPRAVVIVE LA PIU' LUNGA. Non sono due opinioni sullo stesso numero:
/// sono un TETTO e un'ATTESA CIECA, e solo una delle due e' la cosa giusta per
/// un'esclusione che finisce quando qualcuno RICARICA. In mcp-core il numero e'
/// un tetto, perche' il `billing_cooldown_recovery_loop` riprova con una
/// completion vera — che il credito lo esercita — e libera al primo successo:
/// sbagliare per eccesso costa al massimo un intervallo di re-probe. Nel gateway
/// non c'e' niente che possa accorciarlo con cognizione: il suo `healthcheck()`
/// e' un `GET /models`, che risponde 200 mentre le completion sono rifiutate per
/// credito (regola O: lo strumento non raggiunge il suo oggetto). Adottare 3600
/// significherebbe prendere il numero prodotto dal processo SENZA verificatore e
/// imporlo a quello che ce l'ha: dopo un'ora il fornitore senza credito
/// tornerebbe eleggibile e lo si riscoprirebbe con una chiamata a pagamento.
///
/// MISURATO il 14/08/2026 fra le 03:31 e le 04:47 su `nexus_provider_health_history`:
/// il cooldown billing del gateway e' stato azzerato OTTO volte in 76 minuti,
/// sempre ~600s dopo essere stato messo, dal suo `GET /models` — l'alternanza
/// `healthy=f billing` / `healthy=t` per openai e anthropic ogni ~10 minuti. I
/// 3600 non sono mai stati la durata di niente: la vera esclusione del gateway
/// durava l'intervallo di re-probe. Allinearla al tetto non allunga percio'
/// nessuna esclusione osservata; toglie il secondo numero che una diagnosi
/// poteva leggere.
///
/// CONSEGUENZA DA CONOSCERE PRIMA DI STRINGERE IL RILASCIO DEL GATEWAY: finche'
/// il gateway tiene un cooldown billing, una richiesta pinnata su quel fornitore
/// viene respinta in `run_fallback` SENZA raggiungerlo — e il verificatore di
/// mcp-core e' esattamente una richiesta pinnata. Oggi quella finestra e'
/// limitata dal rilascio reattivo descritto sopra (~600s); chi un domani
/// rendesse `il_probe_puo_liberare` piu' severo anche per il billing la
/// porterebbe a coincidere con questo tetto, e con essa il ritardo con cui una
/// ricarica viene notata.
pub mod durata {
    /// La chiave `settings` UNICA da cui entrambi i processi leggono la durata
    /// dell'esclusione per credito (mig 0253). `gateway.cooldown.billing_seconds`
    /// e' stata rimossa dalla mig 0712: un secondo valore che nessuno legge e'
    /// una trappola per chi lo modifica aspettandosi un effetto.
    pub const CHIAVE_COOLDOWN_LUNGO: &str = "provider.cooldown_long_s";
    /// Rete di sicurezza usata SOLO se il DB e' irraggiungibile: 6 ore, lo
    /// stesso valore che la mig 0253 mette nella chiave qui sopra.
    pub const COOLDOWN_LUNGO_DEFAULT_S: u64 = 6 * 3600;
}

/// Cio' che il gateway ha dichiarato di stare rifiutando, in forma su cui si
/// puo' decidere (regola Q: l'esito nel campo, mai nella prosa — il residuo
/// viveva dentro `"in cooldown, {secs}s rimanenti"`, dove per leggerlo serviva
/// una regex).
///
/// La PORTATA segue quella del gateway, e non e' una scelta di questo modulo:
/// e' fedelta'. Cio' che il tipo rappresenta non e' «che limite ha imposto il
/// fornitore» ma «che cosa il gateway rifiutera' se glielo richiedo», quindi
/// escludere piu' di lui renderebbe mcp-core piu' restrittivo del suo stesso
/// gateway, ed escludere meno lo manderebbe di nuovo contro un rifiuto.
///
/// Fino al 13/08/2026 quella portata era il FORNITORE in ogni caso, perche' il
/// `CooldownManager` del gateway ragionava per account. Non era fedelta' alla
/// CAUSA: un 429 come «Rate limit reached for model `openai/gpt-oss-20b` ...
/// TPD Limit 200000» e' un tetto di QUEL modello, e groq spariva intero per 24
/// minuti. Ora il gateway distingue e lo DICHIARA nominando il modello, quindi
/// l'attesa puo' avere la portata giusta anche qui — il credito no: quello e'
/// dell'account, e resta del fornitore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EsclusioneDichiarata {
    /// Fuori per credito. La DURATA non viaggia: quanto tenerlo fuori lo decide
    /// chi ha un ciclo di guarigione (in mcp-core il probe-then-reenable, che
    /// libera appena il credito torna), non chi trasporta la notizia.
    Credito { provider: String },
    /// Fuori per un tempo che il gateway ha DICHIARATO.
    ///
    /// `model: None` = tutto il fornitore. E' cio' che si legge da un gateway
    /// che non nomina il modello, e resta il comportamento prudente: escludere
    /// di piu' costa una convocazione mancata, escludere di meno costa un
    /// rifiuto certo.
    Attesa {
        provider: String,
        model: Option<String>,
        secondi: u64,
    },
    /// Niente da propagare al registro locale: il fallimento non parla della
    /// disponibilita' del fornitore (errore di richiesta, contesto, turno
    /// vuoto), oppure parla di un'attesa di cui non e' stata dichiarata la
    /// durata.
    Nessuna,
}

impl EsclusioneDichiarata {
    /// Il criterio, PURO. `attesa_s` assente su una classe di attesa produce
    /// [`EsclusioneDichiarata::Nessuna`] e non una durata di ripiego: un
    /// gateway che non parla questa versione del contratto non autorizza a
    /// inventare per quanto tempo un fornitore resti fuori (regola G: niente
    /// magic fallback). L'errore cade dalla parte del convocare una volta di
    /// troppo, mai dell'escludere per un tempo che nessuno ha dichiarato.
    /// `portata` e' il valore di [`chiave::PORTATA`]: solo [`portata::MODEL`]
    /// autorizza a restringere l'esclusione alla coppia. Assente o sconosciuta
    /// vale fornitore — cioe' cio' che ogni gateway anteriore a questo campo
    /// intendeva, e il verso che non manda mcp-core contro un rifiuto certo.
    pub fn dal_fallimento(
        classe: &str,
        provider: &str,
        model: Option<&str>,
        portata: Option<&str>,
        attesa_s: Option<u64>,
    ) -> Self {
        let provider = provider.trim();
        if provider.is_empty() {
            return Self::Nessuna;
        }
        // Il modello entra SOLO se la portata lo autorizza, e normalizzato come
        // il fornitore: la coppia e' una chiave, e due grafie della stessa
        // coppia sarebbero due esclusioni diverse.
        let model = portata
            .map(str::trim)
            .filter(|p| *p == portata::MODEL)
            .and(model)
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_lowercase);
        let attesa_dichiarata = attesa_s.filter(|s| *s > 0);
        match (classe.trim(), attesa_dichiarata) {
            (classe::BILLING | classe::COOLDOWN_BILLING, _) => Self::Credito {
                provider: provider.to_lowercase(),
            },
            (classe::COOLDOWN | classe::TRANSIENT, Some(secondi)) => Self::Attesa {
                provider: provider.to_lowercase(),
                model,
                secondi,
            },
            _ => Self::Nessuna,
        }
    }

    /// Legge il fallimento PRIMARIO dal blocco `details` di una risposta
    /// d'errore del gateway.
    ///
    /// Il primario e' `failures[0]`, lo STESSO elemento da cui il gateway
    /// deriva `primary_cause`: causa ed esclusione descrivono percio' sempre il
    /// medesimo fallimento. Si legge dall'array e non da `primary_cause`
    /// perche' li' il nome del fornitore non c'e', e un'esclusione senza il
    /// fornitore non e' registrabile.
    ///
    /// Vive qui, accanto al vocabolario, ed e' il LETTORE che il produttore usa
    /// nei propri test: cosi' il json che il gateway compone e' verificato
    /// contro la funzione che lo consumera' davvero, invece che contro
    /// un'imitazione scritta nel test (regola O).
    pub fn dal_blocco_details(details: Option<&Value>) -> Self {
        let Some(primo) = details
            .and_then(|d| d.get(chiave::FAILURES))
            .and_then(|f| f.as_array())
            .and_then(|a| a.first())
        else {
            return Self::Nessuna;
        };
        let stringa = |k: &str| primo.get(k).and_then(|v| v.as_str()).unwrap_or_default();
        Self::dal_fallimento(
            stringa(chiave::CLASSE),
            stringa(chiave::PROVIDER),
            primo.get(chiave::MODELLO).and_then(|v| v.as_str()),
            primo.get(chiave::PORTATA).and_then(|v| v.as_str()),
            primo.get(chiave::ATTESA_S).and_then(|v| v.as_u64()),
        )
    }

    /// Il fornitore nominato, quando c'e' un'esclusione da registrare.
    pub fn provider(&self) -> Option<&str> {
        match self {
            Self::Credito { provider } | Self::Attesa { provider, .. } => Some(provider),
            Self::Nessuna => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn la_classe_ha_una_sola_traduzione_sul_wire() {
        // Le due copie storiche (routes.rs:271 e :999) producevano queste stesse
        // quattro stringhe: se una delle due fosse andata in drift, il campo
        // `class` avrebbe significato due cose a seconda del ramo che lo scrive.
        assert_eq!(ClasseErrore::Billing.as_wire(), classe::BILLING);
        assert_eq!(ClasseErrore::ClientError.as_wire(), classe::CLIENT_ERROR);
        assert_eq!(
            ClasseErrore::ContextTooLong.as_wire(),
            classe::CONTEXT_TOO_LONG
        );
        assert_eq!(ClasseErrore::Transient.as_wire(), classe::TRANSIENT);
    }

    #[test]
    fn la_tabella_per_status_resta_quella_storica() {
        // E' l'anello che decide quando NON sappiamo: cambiarlo qui sposterebbe
        // la classe di ogni errore non dichiarato, cioe' del caso piu' comune.
        assert_eq!(ClasseErrore::da_status(402), ClasseErrore::Billing);
        assert_eq!(ClasseErrore::da_status(413), ClasseErrore::ContextTooLong);
        for s in [400, 401, 403, 404, 405, 406, 409, 410, 415, 422] {
            assert_eq!(
                ClasseErrore::da_status(s),
                ClasseErrore::ClientError,
                "status {s}"
            );
        }
        for s in [408, 425, 429, 500, 502, 503, 529] {
            assert_eq!(
                ClasseErrore::da_status(s),
                ClasseErrore::Transient,
                "status {s}"
            );
        }
    }

    #[test]
    fn le_classi_di_credito_producono_esclusione_per_credito() {
        for c in [classe::BILLING, classe::COOLDOWN_BILLING] {
            assert_eq!(
                EsclusioneDichiarata::dal_fallimento(c, "Anthropic", None, None, Some(120)),
                EsclusioneDichiarata::Credito {
                    provider: "anthropic".into()
                },
                "classe {c}: la durata non partecipa, il credito non scade col timer del gateway"
            );
        }
        // Il credito e' dell'ACCOUNT: nemmeno una portata di modello lo
        // restringe, o si continuerebbe a chiamare un fornitore senza credito
        // con gli altri suoi modelli.
        assert_eq!(
            EsclusioneDichiarata::dal_fallimento(
                classe::BILLING,
                "anthropic",
                Some("claude-sonnet"),
                Some(portata::MODEL),
                Some(120)
            ),
            EsclusioneDichiarata::Credito {
                provider: "anthropic".into()
            }
        );
    }

    /// L'ammissione rifiutata NON e' un credito esaurito, e la differenza si
    /// vede QUI: `Credito` in questo registro significa sei ore fuori, mentre il
    /// fornitore sta servendo e basterebbe chiedere meno. MISURATO il
    /// 13/08/2026: saldo openrouter a 10,01 dollari residui mentre lo tenevamo
    /// escluso.
    ///
    /// Oggi il valore ricade sul ramo `_`, quindi il comportamento e' giusto per
    /// OMISSIONE: senza questo test, aggiungere la classe al ramo del credito
    /// non farebbe rosseggiare nulla.
    ///
    /// MUTAZIONE: mettere `classe::REQUEST_EXCEEDS_CREDIT` nel primo braccio di
    /// `dal_fallimento` -> qui compare `Credito` e il fornitore torna fuori per
    /// sei ore.
    #[test]
    fn l_ammissione_rifiutata_non_esclude_il_fornitore() {
        for attesa in [None, Some(120)] {
            assert_eq!(
                EsclusioneDichiarata::dal_fallimento(
                    classe::REQUEST_EXCEEDS_CREDIT,
                    "openrouter",
                    Some("qwen/qwen3-235b-a22b-2507"),
                    Some(portata::MODEL),
                    attesa,
                ),
                EsclusioneDichiarata::Nessuna,
                "il credito c'e' e il fornitore serve: nulla da propagare al registro"
            );
        }
    }

    #[test]
    fn le_classi_di_attesa_pretendono_una_durata_dichiarata() {
        assert_eq!(
            EsclusioneDichiarata::dal_fallimento(classe::COOLDOWN, "groq", None, None, Some(1800)),
            EsclusioneDichiarata::Attesa {
                provider: "groq".into(),
                model: None,
                secondi: 1800
            }
        );
        // Nessuna durata dichiarata: non si inventa per quanto tempo escludere.
        assert_eq!(
            EsclusioneDichiarata::dal_fallimento(classe::TRANSIENT, "groq", None, None, None),
            EsclusioneDichiarata::Nessuna
        );
        assert_eq!(
            EsclusioneDichiarata::dal_fallimento(classe::COOLDOWN, "groq", None, None, Some(0)),
            EsclusioneDichiarata::Nessuna
        );
    }

    /// IL CASO MISURATO il 13/08/2026: groq risponde «Rate limit reached for
    /// model `openai/gpt-oss-20b` ... TPD Limit 200000 ... try again in
    /// 23m44.3s». Il tetto e' di QUEL modello: gli altri modelli groq hanno
    /// quota propria e devono restare convocabili.
    ///
    /// MUTAZIONE: far ignorare la portata a `dal_fallimento` (leggere sempre il
    /// modello, o mai) -> uno dei due assert cade, e cadono con il difetto reale
    /// nei due versi opposti: fornitore intero escluso, oppure esclusione piu'
    /// stretta di quella del gateway che l'ha dichiarata.
    #[test]
    fn la_portata_dichiarata_decide_se_l_attesa_e_del_modello() {
        assert_eq!(
            EsclusioneDichiarata::dal_fallimento(
                classe::TRANSIENT,
                "groq",
                Some("openai/GPT-OSS-20B"),
                Some(portata::MODEL),
                Some(1424)
            ),
            EsclusioneDichiarata::Attesa {
                provider: "groq".into(),
                model: Some("openai/gpt-oss-20b".into()),
                secondi: 1424
            },
            "un tetto di modello non toglie di mezzo tutto il fornitore"
        );

        // Il modello viaggia SEMPRE nel blocco (dice quale si stava per
        // chiamare): senza una portata che lo autorizzi, non restringe nulla.
        assert_eq!(
            EsclusioneDichiarata::dal_fallimento(
                classe::COOLDOWN,
                "groq",
                Some("openai/gpt-oss-20b"),
                None,
                Some(1424)
            ),
            EsclusioneDichiarata::Attesa {
                provider: "groq".into(),
                model: None,
                secondi: 1424
            },
            "gateway che non parla questa versione del contratto: portata fornitore"
        );

        // Portata di modello ma modello assente: non si inventa una coppia.
        assert_eq!(
            EsclusioneDichiarata::dal_fallimento(
                classe::COOLDOWN,
                "groq",
                Some("   "),
                Some(portata::MODEL),
                Some(60)
            ),
            EsclusioneDichiarata::Attesa {
                provider: "groq".into(),
                model: None,
                secondi: 60
            }
        );
    }

    #[test]
    fn le_classi_che_non_parlano_di_disponibilita_non_escludono_nessuno() {
        for c in [
            classe::CLIENT_ERROR,
            classe::CONTEXT_TOO_LONG,
            // Il fornitore ha credito e sta servendo: e' la richiesta a non
            // starci dentro. Escluderlo per sei ore e' il difetto del 13/08.
            classe::REQUEST_EXCEEDS_CREDIT,
            classe::EMPTY_COMPLETION,
            "classe_che_non_esiste",
        ] {
            assert_eq!(
                EsclusioneDichiarata::dal_fallimento(c, "openai", None, None, Some(600)),
                EsclusioneDichiarata::Nessuna,
                "classe {c}: il fornitore e' sano, escluderlo sarebbe un danno"
            );
        }
    }

    #[test]
    fn senza_fornitore_non_si_registra_nulla() {
        assert_eq!(
            EsclusioneDichiarata::dal_fallimento(classe::COOLDOWN, "   ", None, None, Some(60)),
            EsclusioneDichiarata::Nessuna
        );
    }

    #[test]
    fn il_lettore_prende_il_primario_e_regge_i_details_incompleti() {
        let details = json!({
            chiave::PRIMARY_CAUSE: classe::COOLDOWN,
            chiave::FAILURES: [
                { chiave::PROVIDER: "kimi", chiave::CLASSE: classe::COOLDOWN, chiave::ATTESA_S: 300 },
                { chiave::PROVIDER: "openai", chiave::CLASSE: classe::BILLING },
            ],
        });
        assert_eq!(
            EsclusioneDichiarata::dal_blocco_details(Some(&details)),
            EsclusioneDichiarata::Attesa {
                provider: "kimi".into(),
                model: None,
                secondi: 300
            },
            "il primario e' failures[0], lo stesso da cui nasce primary_cause"
        );

        // Gateway che non parla questa versione del contratto: campo assente.
        let senza_campo = json!({
            chiave::FAILURES: [{ chiave::PROVIDER: "kimi", chiave::CLASSE: classe::COOLDOWN }],
        });
        assert_eq!(
            EsclusioneDichiarata::dal_blocco_details(Some(&senza_campo)),
            EsclusioneDichiarata::Nessuna
        );

        assert_eq!(
            EsclusioneDichiarata::dal_blocco_details(None),
            EsclusioneDichiarata::Nessuna
        );
        assert_eq!(
            EsclusioneDichiarata::dal_blocco_details(Some(&json!({ chiave::FAILURES: [] }))),
            EsclusioneDichiarata::Nessuna
        );
    }
}
