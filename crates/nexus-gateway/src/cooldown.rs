//! Cooldown dei provider con RE-PROBE reattivo (il fix del bug "OpenAI non
//! torna dopo la ricarica").
//!
//! ## Bug che questo modulo risolve
//!
//! Nel gateway Node il cooldown di billing era in-memory e ASPETTAVA la scadenza
//! (impostata su ore) senza mai ri-provare il provider. Conseguenza: dopo che
//! l'utente ricaricava i crediti su OpenAI, il provider restava marcato giallo
//! (in cooldown) per ore, perche' nessuno verificava che fosse di nuovo sano.
//! In piu', vivendo nel processo Node separato da mcp-core, lo stato non era
//! condiviso con il resto del runtime.
//!
//! ## Soluzione
//!
//! [`CooldownManager`] tiene lo stato di cooldown per provider in una
//! `DashMap` thread-safe. La novita' rispetto al Node e' il RE-PROBE LOOP
//! ([`spawn_recovery_loop`]): un task tokio periodico che, per OGNI provider
//! attualmente in cooldown, esegue [`crate::provider::LlmProvider::healthcheck`].
//! Se il provider torna sano, il cooldown viene rimosso SUBITO -- quindi dopo
//! una ricarica il provider rientra entro un intervallo di re-probe (minuti),
//! NON dopo la scadenza nominale (ore).
//!
//! ## Configurazione DB-driven (regola G)
//!
//! Durate e intervallo NON sono hardcoded nella business logic: arrivano dai
//! `settings` con cache TTL ([`nexus_cache::TtlCache`], punto unico regola L).
//! Le costanti di questo modulo sono SOLO il fallback di sicurezza usato se il
//! DB e' irraggiungibile, documentate come tali. Chiavi lette:
//!   - `provider.cooldown_long_s`           (default 21600s) — la stessa che
//!     legge mcp-core: la durata dell'esclusione per credito e' UNA, e il
//!     perche' sta in [`nexus_types::provider_failure::durata`]
//!   - `gateway.cooldown.transient_seconds` (default 30s)
//!   - `gateway.cooldown.reprobe_interval_seconds` (default 600s)
//!
//! Regola F: i log non contengono prompt/response; solo nome provider e durate.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use nexus_cache::TtlCache;
use nexus_types::provider_failure::{durata, portata, stato_salute};
use sqlx::PgPool;

use crate::provider::LlmProvider;
use crate::types::ProviderStatus;

/// Fallback durata cooldown billing se il DB e' irraggiungibile. NON e' un
/// valore di business (quello sta in `settings`), e' la rete di sicurezza — e
/// non e' piu' un numero di questo modulo: la durata dell'esclusione per credito
/// e' UNA, dichiarata in [`durata`] insieme al perche' vinca il tetto di sei ore
/// sull'attesa cieca di un'ora che stava scritta qui.
pub const DEFAULT_BILLING_SECONDS: i64 = durata::COOLDOWN_LUNGO_DEFAULT_S as i64;

/// Fallback durata cooldown transitorio (errori di rete/5xx): 30 secondi.
pub const DEFAULT_TRANSIENT_SECONDS: i64 = 30;

/// Fallback intervallo di re-probe: 600 secondi (10 minuti).
pub const DEFAULT_REPROBE_INTERVAL_SECONDS: u64 = 600;

/// Fallback numero massimo di tentativi sullo STESSO modello (strict pin): 3.
pub const DEFAULT_RETRY_MAX_ATTEMPTS: u32 = 3;

/// Fallback ritardo base del backoff esponenziale: 500ms.
pub const DEFAULT_RETRY_BASE_DELAY_MS: u64 = 500;

/// Fallback tetto del backoff esponenziale: 8s.
pub const DEFAULT_RETRY_MAX_BACKOFF_MS: u64 = 8000;

/// Fallback tetto di attesa di un cooldown transitorio BREVE prima di ritentare
/// lo stesso modello (strict pin): 45s. Oltre questo, si propaga l'errore invece
/// di bloccare la richiesta troppo a lungo.
pub const DEFAULT_WAIT_SHORT_COOLDOWN_CAP_S: i64 = 45;

/// Politica di retry sullo stesso modello (strict pin). Risolta da DB o fallback.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Tentativi totali sullo stesso provider+model prima di arrendersi.
    pub max_attempts: u32,
    /// Ritardo base del backoff esponenziale (ms).
    pub base_delay_ms: u64,
    /// Tetto del backoff esponenziale (ms).
    pub max_backoff_ms: u64,
    /// Massimo cooldown transitorio da attendere prima di ritentare (secondi).
    pub wait_short_cooldown_cap_s: i64,
}

impl RetryPolicy {
    /// Ritardo (ms) prima del tentativo `attempt` (1-based, dopo il 1o fallimento
    /// `attempt=1`): `min(base * 2^(attempt-1), max)` + jitter deterministico
    /// derivato da `seed` (nessun `rand`: `seed` e' fornito dal chiamante, es.
    /// nanosecondi dell'istante, cosi' resta testabile). Il jitter e' fino al 25%.
    pub fn backoff_ms(&self, attempt: u32, seed: u64) -> u64 {
        let exp = attempt.saturating_sub(1).min(16);
        let raw = self.base_delay_ms.saturating_mul(1u64 << exp);
        let capped = raw.min(self.max_backoff_ms).max(1);
        let jitter = seed % (capped / 4 + 1);
        capped.saturating_add(jitter)
    }
}

/// TTL della cache settings (60s, allineato al resto del gateway).
const SETTINGS_TTL: Duration = Duration::from_secs(60);

/// Causa del cooldown. Discrimina la durata applicata e l'audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CooldownReason {
    /// Crediti esauriti / problema di fatturazione: cooldown lungo, ma il
    /// re-probe lo annulla appena il provider torna a rispondere 200.
    Billing,
    /// Errore transitorio (rete, 5xx, timeout): cooldown breve.
    Transient,
}

impl CooldownReason {
    /// La ragione del COOLDOWN, per il log. Due valori, perche' due sono le
    /// durate che questo manager sa applicare.
    fn as_str(self) -> &'static str {
        match self {
            CooldownReason::Billing => "billing",
            CooldownReason::Transient => "transient",
        }
    }

    /// Il nome con cui questo stato si registra in
    /// `nexus_provider_health_history.error_kind`, dal vocabolario condiviso coi
    /// due scrittori di quella colonna ([`stato_salute`]).
    ///
    /// NON e' [`Self::as_str`], e la differenza e' il difetto misurato il
    /// 13/08/2026: quella colonna vuole una CAUSA, e vi finiva la CLASSE con cui
    /// questo modulo decide. Percio' openai senza credito compariva come
    /// `billing` scritto da qui e come `credit_balance_too_low` scritto dal probe
    /// di mcp-core, nello stesso millisecondo, e nessun filtro li trovava
    /// entrambi.
    fn nome_nello_storico(self) -> &'static str {
        match self {
            CooldownReason::Billing => stato_salute::CREDIT_BALANCE_TOO_LOW,
            CooldownReason::Transient => stato_salute::TRANSIENT,
        }
    }
}

/// CHI ha stabilito quando il cooldown finisce.
///
/// Non e' un dettaglio di provenienza: decide se un probe abbia titolo per
/// abbreviarlo. Una scadenza che il FORNITORE ha dichiarato e' un fatto sul suo
/// servizio; una che abbiamo stimato noi e' una precauzione, e una precauzione
/// puo' essere revocata da una misura.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrigineScadenza {
    /// Il fornitore ha detto QUANDO ritentare (`Retry-After`).
    Dichiarata,
    /// L'abbiamo stimata noi dalle durate di configurazione.
    Stimata,
}

/// Stato di cooldown di un singolo provider.
#[derive(Debug, Clone)]
pub struct CooldownState {
    /// Istante (UTC) fino al quale il provider resta in cooldown.
    pub until: DateTime<Utc>,
    /// Causa del cooldown.
    pub reason: CooldownReason,
    /// Ultimo messaggio d'errore osservato (gia' privo di prompt/response).
    pub last_error: Option<String>,
    /// Chi ha stabilito `until`. Vedi [`OrigineScadenza`] e
    /// [`il_probe_puo_liberare`].
    pub origine: OrigineScadenza,
}

/// Il probe di ripristino ha titolo per liberare questo cooldown?
///
/// MISURATO il 12/08/2026, ed e' il difetto che questa funzione chiude. Il
/// gateway faceva la cosa giusta due volte e la disfaceva alla terza: leggeva il
/// `Retry-After` del 429, lo onorava in [`CooldownManager::mark_transient_after`]
/// (fix del 10/08), e poi `run_recovery_pass` cancellava tutto appena
/// `healthcheck()` rispondeva — dove `healthcheck()` e' un `GET /models`, che NON
/// e' soggetto al tetto sui token ne' al credito esaurito.
///
/// Le due misure: groq con cooldown 565s liberato dopo 170s (395 PRIMA della
/// scadenza) e un nuovo 429 QUATTRO SECONDI dopo, col medesimo Retry-After di
/// 395s; e lo stesso su 1790s, liberato 1304s prima. Non e' specifico di groq —
/// anthropic in cooldown billing 3600s risultava «ripristinato» una quarantina di
/// volte con anticipo di 3000-3500s, riprendendo il billing error pochi minuti
/// dopo, in ciclo. Il codice lo dichiarava gia' senza trarne la conseguenza
/// (`anthropic.rs:345-348`: su billing error le `complete` restano 4xx «ma il
/// probe modelli resta valido per il re-probe reattivo» — non e' valido: risponde
/// a un'altra domanda).
///
/// Il criterio non e' «quale fornitore» ne' «quale causa»: e' se la scadenza sia
/// un FATTO dichiarato da chi serve o una nostra stima. Contro un fatto, una
/// misura che riguarda un'altra operazione non ha titolo; contro una stima, si'.
pub fn il_probe_puo_liberare(state: &CooldownState) -> bool {
    matches!(state.origine, OrigineScadenza::Stimata)
}

/// Durate di cooldown + politica di retry effettive (gia' risolte da DB o
/// fallback). Un solo set globale in cache (regola L): cooldown e retry sono lo
/// stesso concern di affidabilita' del gateway.
#[derive(Debug, Clone, Copy)]
struct Durations {
    billing_seconds: i64,
    transient_seconds: i64,
    retry_max_attempts: u32,
    retry_base_delay_ms: u64,
    retry_max_backoff_ms: u64,
    wait_short_cooldown_cap_s: i64,
}

impl Default for Durations {
    fn default() -> Self {
        Self {
            billing_seconds: DEFAULT_BILLING_SECONDS,
            transient_seconds: DEFAULT_TRANSIENT_SECONDS,
            retry_max_attempts: DEFAULT_RETRY_MAX_ATTEMPTS,
            retry_base_delay_ms: DEFAULT_RETRY_BASE_DELAY_MS,
            retry_max_backoff_ms: DEFAULT_RETRY_MAX_BACKOFF_MS,
            wait_short_cooldown_cap_s: DEFAULT_WAIT_SHORT_COOLDOWN_CAP_S,
        }
    }
}

/// Separatore fra fornitore e modello nella chiave di [`chiave_cooldown`].
///
/// `\u{1}` (SOH) e' lo stesso che usa il registro gemello di mcp-core
/// (`provider_cooldown::chiave_cooldown`), e per la stessa ragione: non compare
/// in nessun nome di fornitore ne' di modello, quindi la chiave e' invertibile.
const SEPARATORE_CHIAVE: char = '\u{1}';

/// La chiave sotto cui vive UN cooldown, e con essa la sua PORTATA.
///
/// `model: None` = tutto il fornitore (credito, autenticazione: sono
/// dell'account). `Some(m)` = quella coppia soltanto, ed e' la forma giusta per
/// un rate limit, che e' un tetto DEL MODELLO.
///
/// MISURATO il 13/08/2026: groq risponde
/// «Rate limit reached for model `openai/gpt-oss-20b` ... TPD Limit 200000,
/// Used 199788 ... try again in 23m44.3s» e il gateway escludeva groq INTERO per
/// 24 minuti — cioe' anche i modelli che avevano quota propria e avrebbero
/// servito. mcp-core aveva gia' chiuso lo stesso difetto il 07/08 (479 cooldown
/// in una notte, di cui 205 per rate limit); il gateway non lo aveva recepito, e
/// dal 13/08 la sua portata si PROPAGA a mcp-core attraverso
/// `registra_esclusione_dichiarata`.
pub fn chiave_cooldown(provider: &str, model: Option<&str>) -> String {
    match model {
        Some(m) if !m.trim().is_empty() => format!(
            "{}{SEPARATORE_CHIAVE}{}",
            provider.to_lowercase(),
            m.trim().to_lowercase()
        ),
        _ => provider.to_lowercase(),
    }
}

/// HTTP 429 Too Many Requests: un tetto di frequenza per definizione del
/// protocollo, quindi non serve un codice del fornitore per riconoscerlo.
const STATUS_TROPPE_RICHIESTE: u16 = 429;

/// Frammento del codice STRUTTURATO con cui un fornitore dichiara un tetto di
/// frequenza o di volume (`rate_limit_exceeded`, `rate_limit_error`, ...). E' lo
/// stesso frammento su cui `classify_by_status_code` decide `Transient`: due
/// vocabolari darebbero due idee di che cosa sia un rate limit.
const CODICE_RATE_LIMIT: &str = "rate_limit";

/// CHI resta escluso da un fallimento transitorio.
///
/// E' un tipo e non un `Option<&str>` sul call site perche' la domanda ha una
/// risposta sola per ogni causa, e va data dove la causa si conosce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortataCooldown {
    /// L'endpoint non ha risposto (trasporto, 5xx, timeout del tentativo): non
    /// risponde per nessun modello.
    Fornitore,
    /// Un tetto di frequenza o di volume: e' del MODELLO. Un altro modello dello
    /// stesso fornitore ha quota propria e resta usabile.
    Modello,
}

impl PortataCooldown {
    /// Il criterio, PURO, dai segnali STRUTTURATI (regola M): il codice d'errore
    /// dichiarato dal fornitore e lo status HTTP, mai la prosa del messaggio.
    ///
    /// Il codice viene PRIMA dello status perche' lo status da solo mente: groq
    /// manda `413` (non 429) quando la richiesta supera il tetto token del piano,
    /// dichiarandolo in `code = rate_limit_exceeded` — la stessa asimmetria che
    /// `classify_by_status_code` gia' documenta. Un `429` nudo e' un tetto di
    /// frequenza per definizione HTTP.
    pub fn da_segnale(status: Option<u16>, code: Option<&str>) -> Self {
        if code.is_some_and(|c| c.contains(CODICE_RATE_LIMIT))
            || status == Some(STATUS_TROPPE_RICHIESTE)
        {
            Self::Modello
        } else {
            Self::Fornitore
        }
    }

    /// Il modello da passare a chi marca, dato quello della richiesta.
    pub fn modello(self, model: &str) -> Option<&str> {
        match self {
            Self::Modello => Some(model),
            Self::Fornitore => None,
        }
    }

    /// Il valore da mettere in [`nexus_types::provider_failure::chiave::PORTATA`]
    /// perche' mcp-core registri la stessa portata, invece di indovinarla.
    pub fn wire(self) -> &'static str {
        match self {
            Self::Modello => portata::MODEL,
            Self::Fornitore => portata::PROVIDER,
        }
    }
}

/// Gestore dei cooldown. Clonabile a basso costo (condivide lo store via `Arc`),
/// cosi' puo' essere riposto nello stato applicativo e nel task di re-probe.
#[derive(Clone)]
pub struct CooldownManager {
    /// Chiave = [`chiave_cooldown`]: il solo fornitore, oppure la coppia
    /// fornitore+modello. La portata la decide chi conosce la CAUSA.
    states: Arc<DashMap<String, CooldownState>>,
    /// Cache delle durate lette dai settings (chiave unit: un solo set globale).
    durations: TtlCache<(), Durations>,
    /// Pool DB per la persistenza dell'ultimo errore per provider
    /// (`nexus_provider_health.last_error` + history, migrazione 0536).
    /// Vuoto nei test unit: la persistenza diventa un no-op.
    db: Arc<std::sync::OnceLock<PgPool>>,
}

impl Default for CooldownManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CooldownManager {
    /// Crea un manager vuoto con le durate di fallback. Le durate reali vengono
    /// caricate da [`Self::refresh_settings`] (chiamato dal re-probe loop).
    pub fn new() -> Self {
        Self {
            states: Arc::new(DashMap::new()),
            durations: TtlCache::new(SETTINGS_TTL),
            db: Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Collega il pool DB per la persistenza degli errori provider. Chiamato
    /// una volta dal bootstrap; chiamate successive sono ignorate (OnceLock).
    pub fn attach_db(&self, pool: PgPool) {
        let _ = self.db.set(pool);
    }

    /// Durate correnti: da cache settings se valide, altrimenti fallback.
    fn current_durations(&self) -> Durations {
        self.durations.get(&()).unwrap_or_default()
    }

    /// SOLO test: inietta durate/retry rapide (backoff ~1ms) cosi' i test del
    /// retry non introducono sleep reali. Non usato in produzione.
    #[cfg(test)]
    pub fn set_fast_for_test(&self) {
        self.durations.insert(
            (),
            Durations {
                billing_seconds: DEFAULT_BILLING_SECONDS,
                transient_seconds: 30,
                retry_max_attempts: 3,
                retry_base_delay_ms: 1,
                retry_max_backoff_ms: 2,
                wait_short_cooldown_cap_s: 45,
            },
        );
    }

    /// Politica di retry corrente (strict pin): da cache settings se valida,
    /// altrimenti fallback. Punto unico (regola L): `run_fallback` la legge da qui.
    pub fn retry_policy(&self) -> RetryPolicy {
        let d = self.current_durations();
        RetryPolicy {
            max_attempts: d.retry_max_attempts.max(1),
            base_delay_ms: d.retry_base_delay_ms.max(1),
            max_backoff_ms: d.retry_max_backoff_ms.max(1),
            wait_short_cooldown_cap_s: d.wait_short_cooldown_cap_s.max(0),
        }
    }

    /// Marca un provider in cooldown billing. Usa l'orologio reale.
    pub fn mark_billing(&self, provider: &str, last_error: Option<String>) {
        let secs = self.current_durations().billing_seconds;
        self.mark_at(provider, CooldownReason::Billing, last_error, Utc::now(), secs);
    }

    /// Marca un provider in cooldown transitorio. Usa l'orologio reale.
    ///
    /// Delega a [`Self::mark_transient_after`] senza `Retry-After`: la durata del
    /// cooldown transitorio si decide in UN punto solo (regola L).
    pub fn mark_transient(&self, provider: &str, model: Option<&str>, last_error: Option<String>) {
        self.mark_transient_after(provider, model, last_error, None);
    }

    /// Marca un provider in cooldown transitorio ONORANDO il `Retry-After` che il
    /// provider ha dichiarato, quando c'e'.
    ///
    /// Regola M: `retry_after_seconds` e' un segnale STRUTTURATO e autoritativo — il
    /// provider sta dicendo QUANDO tornera' a servire. Scartarlo per applicare il
    /// cooldown fisso significa ripresentarsi prima, prendere lo stesso errore e
    /// riaccodare lo stesso cooldown: un ciclo che non converge e che consuma quota
    /// (o quantomeno richieste) a ogni giro.
    ///
    /// MISURATO il 10/08/2026 su groq, con il tetto GIORNALIERO (TPD) esaurito: il
    /// provider chiedeva minuti, il cooldown ne applicava 30 secondi, e il log del
    /// gateway mostra la sequenza "provider in cooldown 30s -> attendo -> ritento ->
    /// 429 -> cooldown 30s" ininterrotta dalle 11:39:50 alle 12:09 — mezz'ora di
    /// tentativi contro un muro che, trattandosi di un tetto giornaliero, poteva
    /// durare l'intera giornata. La distinzione fra un tetto al MINUTO e uno al
    /// GIORNO non sta negli header di rate-limit (MISURATO: groq espone solo i
    /// contatori al minuto): sta proprio nel `Retry-After`, che percio' e' l'unico
    /// segnale con cui i due casi si possono separare senza leggere la prosa.
    ///
    /// Si prende il PIU' LUNGO dei due: il provider dice quando ritentare al piu'
    /// presto, il nostro minimo resta una protezione nostra. Il tetto superiore lo
    /// mette gia' `parse_retry_after` (clamp difensivo a 3600s), quindi un provider
    /// che chiedesse giorni non blocca il fornitore oltre l'ora.
    ///
    /// `model` dichiara la PORTATA (vedi [`chiave_cooldown`]): un rate limit e'
    /// del modello, un errore di trasporto e' del fornitore. Chi il modello lo
    /// conosce lo passa; `None` esclude l'intero fornitore e va scelto sapendolo.
    pub fn mark_transient_after(
        &self,
        provider: &str,
        model: Option<&str>,
        last_error: Option<String>,
        retry_after_seconds: Option<u64>,
    ) {
        let base = self.current_durations().transient_seconds;
        let secs = match retry_after_seconds {
            Some(s) => base.max(i64::try_from(s).unwrap_or(base)),
            None => base,
        };
        // Dove il fornitore ha DETTO quando tornera', la scadenza e' un fatto suo
        // e non una nostra stima: il probe di ripristino non ha titolo per
        // abbreviarla (vedi `il_probe_puo_liberare`).
        let origine = match retry_after_seconds {
            Some(_) => OrigineScadenza::Dichiarata,
            None => OrigineScadenza::Stimata,
        };
        self.mark_at_con_origine(
            provider,
            model,
            CooldownReason::Transient,
            last_error,
            Utc::now(),
            secs,
            origine,
        );
    }

    /// Marca un FORNITORE con `now` e durata espliciti, con la scadenza come
    /// nostra stima. `mark_billing` e i test ci delegano, cosi' possono iniettare
    /// un istante deterministico senza usare `Utc::now()`.
    pub fn mark_at(
        &self,
        provider: &str,
        reason: CooldownReason,
        last_error: Option<String>,
        now: DateTime<Utc>,
        duration_seconds: i64,
    ) {
        self.mark_at_con_origine(
            provider,
            None,
            reason,
            last_error,
            now,
            duration_seconds,
            OrigineScadenza::Stimata,
        );
    }

    /// Il punto unico della marcatura (regola L): dichiara CHI ha stabilito la
    /// scadenza — il solo chiamante che passa `Dichiarata` e' quello che ha letto
    /// un `Retry-After` dal fornitore — e su CHI vale (`model`).
    ///
    /// La chiave dello store la compone [`chiave_cooldown`]; la PERSISTENZA
    /// riceve invece il solo `provider`, perche' `nexus_provider_health` e la sua
    /// history sono per fornitore — scriverci una chiave composta significherebbe
    /// inventare un fornitore che non esiste.
    #[allow(clippy::too_many_arguments)]
    pub fn mark_at_con_origine(
        &self,
        provider: &str,
        model: Option<&str>,
        reason: CooldownReason,
        last_error: Option<String>,
        now: DateTime<Utc>,
        duration_seconds: i64,
        origine: OrigineScadenza,
    ) {
        let until = now + chrono::Duration::seconds(duration_seconds);
        // Regola F: logghiamo nome provider, modello e durata, MAI il payload.
        // last_error qui e' il messaggio d'errore del provider (no prompt utente).
        tracing::warn!(
            provider,
            model,
            reason = reason.as_str(),
            duration_seconds,
            "gateway: cooldown"
        );
        self.persist_last_error(provider, reason, last_error.as_deref());
        self.states.insert(
            chiave_cooldown(provider, model),
            CooldownState {
                until,
                reason,
                last_error,
                origine,
            },
        );
    }

    /// Persiste l'ultimo errore osservato per il provider (migrazione 0536):
    /// UPSERT su `nexus_provider_health` (SOLO last_error/last_error_at/
    /// last_error_source: `billing_cooldown_until` resta di proprieta'
    /// esclusiva di mcp-core, writer unico regola L) + riga append-only in
    /// `nexus_provider_health_history` con source='gateway'. Senza questa
    /// persistenza l'errore HTTP esatto di un failover transiente non era
    /// ricostruibile da nessuna parte (incidente run a5db0985, 2026-07-06).
    ///
    /// Fire-and-forget: nessun errore DB blocca la pipeline di routing. No-op
    /// se il pool non e' collegato (test unit) o fuori da un runtime tokio
    /// (delegato a [`Self::spawn_persist`], punto unico regola L).
    fn persist_last_error(&self, provider: &str, reason: CooldownReason, last_error: Option<&str>) {
        // Regola F: e' il messaggio d'errore del provider (status+codice), mai
        // prompt/response. Troncato a 500 char come la history del probe.
        let message = truncate_chars(last_error.unwrap_or(""), 500);
        let provider = provider.to_lowercase();
        // La colonna e' `error_kind`: vuole la causa, non la classe di cooldown.
        let kind = reason.nome_nello_storico();
        self.spawn_persist(move |pool| persist_provider_error(pool, provider, kind, message));
    }

    /// Punto unico (regola L) del fire-and-forget "collega pool + runtime,
    /// spawna". `persist_last_error` e `persist_recovery` differiscono solo
    /// nella query da eseguire; questo evita di ripetere la stessa sequenza
    /// `db.get().cloned()` + `Handle::try_current()` in entrambi (il gate
    /// qualita' la segnalava come blocco duplicato).
    fn spawn_persist<F, Fut>(&self, make_future: F)
    where
        F: FnOnce(PgPool) -> Fut,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let Some(pool) = self.db.get().cloned() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(make_future(pool));
    }

    /// Lo stato di cooldown del provider, se ne ha uno registrato.
    ///
    /// Serve a chi deve decidere COME trattarlo e non solo se esiste: il probe
    /// di ripristino ha bisogno dell'origine della scadenza (vedi
    /// [`il_probe_puo_liberare`]).
    pub fn state(&self, provider: &str) -> Option<CooldownState> {
        self.states.get(provider).map(|s| s.clone())
    }

    /// Il FORNITORE e' escluso? Vero solo per i cooldown di account (credito,
    /// auth, trasporto): un limite su un singolo modello non risponde si' a
    /// questa domanda.
    ///
    /// Chi sta per instradare una richiesta conosce anche il MODELLO e deve
    /// usare [`Self::is_model_in_cooldown`]: sono due domande, e confonderle e'
    /// precisamente il difetto del 13/08/2026.
    pub fn is_in_cooldown(&self, provider: &str) -> bool {
        self.is_in_cooldown_at(provider, Utc::now())
    }

    /// Variante con istante iniettato (deterministica per i test).
    pub fn is_in_cooldown_at(&self, provider: &str, now: DateTime<Utc>) -> bool {
        self.attivo_at(&chiave_cooldown(provider, None), now)
    }

    /// Questa COPPIA e' utilizzabile adesso? Vero se e' escluso il fornitore
    /// come account, oppure questo modello in particolare.
    pub fn is_model_in_cooldown(&self, provider: &str, model: &str) -> bool {
        self.is_model_in_cooldown_at(provider, model, Utc::now())
    }

    /// Variante con istante iniettato (deterministica per i test).
    pub fn is_model_in_cooldown_at(&self, provider: &str, model: &str, now: DateTime<Utc>) -> bool {
        self.is_in_cooldown_at(provider, now)
            || self.attivo_at(&chiave_cooldown(provider, Some(model)), now)
    }

    fn attivo_at(&self, chiave: &str, now: DateTime<Utc>) -> bool {
        match self.states.get(chiave) {
            Some(s) => s.until > now,
            None => false,
        }
    }

    /// `true` se il provider e' in cooldown ATTIVO per motivo Billing (crediti).
    /// Usato per arricchire il messaggio d'errore del 500: cosi' il brain (che
    /// legge il body) riconosce il billing e applica il cooldown lungo invece di
    /// riprovare il provider a ogni iterazione.
    pub fn is_billing_cooldown(&self, provider: &str) -> bool {
        match self.states.get(provider) {
            Some(s) => s.until > Utc::now() && s.reason == CooldownReason::Billing,
            None => false,
        }
    }

    /// Secondi rimanenti di cooldown del FORNITORE (0 se non in cooldown).
    pub fn seconds_remaining(&self, provider: &str) -> i64 {
        self.seconds_remaining_at(provider, Utc::now())
    }

    /// Variante con istante iniettato (deterministica per i test).
    pub fn seconds_remaining_at(&self, provider: &str, now: DateTime<Utc>) -> i64 {
        self.residuo_at(&chiave_cooldown(provider, None), now)
    }

    /// Secondi rimanenti prima che questa COPPIA torni utilizzabile: il PIU'
    /// LUNGO fra l'esclusione del fornitore e quella del modello, perche' finche'
    /// una delle due vale la coppia resta fuori.
    pub fn seconds_remaining_for_model(&self, provider: &str, model: &str) -> i64 {
        self.seconds_remaining_for_model_at(provider, model, Utc::now())
    }

    /// Variante con istante iniettato (deterministica per i test).
    pub fn seconds_remaining_for_model_at(
        &self,
        provider: &str,
        model: &str,
        now: DateTime<Utc>,
    ) -> i64 {
        self.seconds_remaining_at(provider, now)
            .max(self.residuo_at(&chiave_cooldown(provider, Some(model)), now))
    }

    /// CHI e' escluso, se questa coppia lo e' adesso. `None` = nessuna
    /// esclusione attiva.
    ///
    /// Il FORNITORE prevale sulla coppia quando entrambe le esclusioni sono
    /// attive: e' cio' che questo gateway rifiutera' comunque, e dichiararla
    /// piu' stretta manderebbe il chiamante a riprovare contro un muro.
    pub fn portata_attiva(&self, provider: &str, model: &str) -> Option<PortataCooldown> {
        let now = Utc::now();
        if self.is_in_cooldown_at(provider, now) {
            Some(PortataCooldown::Fornitore)
        } else if self.attivo_at(&chiave_cooldown(provider, Some(model)), now) {
            Some(PortataCooldown::Modello)
        } else {
            None
        }
    }

    fn residuo_at(&self, chiave: &str, now: DateTime<Utc>) -> i64 {
        match self.states.get(chiave) {
            Some(s) => (s.until - now).num_seconds().max(0),
            None => 0,
        }
    }

    /// Rimuove il cooldown di un provider (usato dal re-probe al ripristino e
    /// dal path di richiesta reale su un 200 dopo cooldown). Se il provider
    /// era EFFETTIVAMENTE in cooldown, persiste una riga `healthy=true` in
    /// `nexus_provider_health_history` (fix bug "re-probe cieco": prima il
    /// ripristino restava SOLO in-memory, quindi `checked_at` in DB restava
    /// fermo all'ultimo fallimento anche dopo un recovery reale confermato
    /// da questo stesso processo — la riga piu' recente vista da
    /// `fetch_provider_health_map` (mcp-core) e' quella che decide lo stato
    /// esposto da `/health`, e senza questa scrittura un provider tornato
    /// sano restava marcato down fino al prossimo giro del probe periodico
    /// separato di mcp-core, con cadenza propria non sincronizzata).
    pub fn clear(&self, provider: &str) {
        if self.states.remove(&chiave_cooldown(provider, None)).is_some() {
            tracing::info!(provider, "gateway: provider ripristinato (cooldown rimosso)");
            self.persist_recovery(provider);
        }
    }

    /// Libera la coppia fornitore+modello DOPO una sua risposta riuscita, e con
    /// essa il fornitore.
    ///
    /// Un 200 su `modelA` prova che il fornitore serve, quindi il cooldown di
    /// account cade; NON tocca `modelB`, il cui tetto e' suo e non e' stato
    /// misurato da questa chiamata. E' la stessa asimmetria per cui il rate
    /// limit nasce sulla coppia.
    pub fn clear_model(&self, provider: &str, model: &str) {
        self.states
            .remove(&chiave_cooldown(provider, Some(model)));
        self.clear(provider);
    }

    /// Persiste il ripristino: INSERT `healthy=true` in
    /// `nexus_provider_health_history` con `source='gateway'`, via
    /// [`Self::spawn_persist`] (stesso punto unico di `persist_last_error`).
    fn persist_recovery(&self, provider: &str) {
        let provider = provider.to_lowercase();
        self.spawn_persist(move |pool| persist_provider_recovery(pool, provider));
    }

    /// Snapshot dello stato di tutti i provider in cooldown, come
    /// [`ProviderStatus`] (per esposizione su `/status`). I provider non
    /// presenti nella mappa sono considerati sani e NON compaiono qui.
    pub fn snapshot(&self) -> Vec<ProviderStatus> {
        self.snapshot_at(Utc::now())
    }

    /// Variante con istante iniettato (deterministica per i test).
    /// Variante con istante iniettato (deterministica per i test).
    ///
    /// Riporta i soli cooldown di FORNITORE: `ProviderStatus.name` e' un
    /// fornitore, e un modello a tetto raggiunto non rende il fornitore non
    /// sano — dirlo qui rimetterebbe in circolo, in forma di stato esposto,
    /// proprio la conflazione che questo modulo ha appena tolto.
    pub fn snapshot_at(&self, now: DateTime<Utc>) -> Vec<ProviderStatus> {
        self.states
            .iter()
            .filter(|e| e.until > now && !e.key().contains(SEPARATORE_CHIAVE))
            .map(|e| {
                let s = e.value();
                ProviderStatus {
                    name: e.key().clone(),
                    healthy: false,
                    last_check: now,
                    last_error: s.last_error.clone(),
                    billing_error: if s.reason == CooldownReason::Billing {
                        s.last_error.clone()
                    } else {
                        None
                    },
                }
            })
            .collect()
    }

    /// Lista dei FORNITORI attualmente in cooldown (per il re-probe loop).
    ///
    /// Le chiavi di coppia restano fuori di proposito: il probe e' un
    /// `GET /models`, che parla del fornitore e non ha nulla da dire sul tetto
    /// token di un singolo modello. Quei cooldown scadono da soli, alla durata
    /// che il fornitore ha dichiarato.
    fn providers_in_cooldown(&self, now: DateTime<Utc>) -> Vec<String> {
        self.states
            .iter()
            .filter(|e| e.until > now && !e.key().contains(SEPARATORE_CHIAVE))
            .map(|e| e.key().clone())
            .collect()
    }

    /// Ricarica le durate dai `settings` nella cache TTL 60s. `force=true`
    /// ignora la cache. Se il DB e' down mantiene i valori correnti (graceful):
    /// il routing non si blocca. Le durate effettive si leggono poi via
    /// `mark_billing`/`mark_transient` (che usano `current_durations`).
    pub async fn refresh_settings(&self, pool: &PgPool, force: bool) {
        if !force && self.durations.get(&()).is_some() {
            return;
        }

        // `provider.cooldown_long_s` e non piu' `gateway.cooldown.billing_seconds`:
        // vedi [`durata`]. La chiave e' nominata dalla costante condivisa, cosi'
        // il giorno in cui cambia non resta un letterale a divergere.
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT key, value FROM settings \
             WHERE key IN ($1, \
                           'gateway.cooldown.transient_seconds', \
                           'gateway.retry.max_attempts', \
                           'gateway.retry.base_delay_ms', \
                           'gateway.retry.max_backoff_ms', \
                           'gateway.retry.wait_short_cooldown_cap_s')",
        )
        .bind(durata::CHIAVE_COOLDOWN_LUNGO)
        .fetch_all(pool)
        .await;

        match rows {
            Ok(rows) => {
                let mut d = Durations::default();
                for (key, value) in &rows {
                    let v = value.trim();
                    match key.as_str() {
                        k if k == durata::CHIAVE_COOLDOWN_LUNGO => {
                            if let Ok(n) = v.parse::<i64>() {
                                d.billing_seconds = n;
                            }
                        }
                        "gateway.cooldown.transient_seconds" => {
                            if let Ok(n) = v.parse::<i64>() {
                                d.transient_seconds = n;
                            }
                        }
                        "gateway.retry.max_attempts" => {
                            if let Ok(n) = v.parse::<u32>() {
                                d.retry_max_attempts = n;
                            }
                        }
                        "gateway.retry.base_delay_ms" => {
                            if let Ok(n) = v.parse::<u64>() {
                                d.retry_base_delay_ms = n;
                            }
                        }
                        "gateway.retry.max_backoff_ms" => {
                            if let Ok(n) = v.parse::<u64>() {
                                d.retry_max_backoff_ms = n;
                            }
                        }
                        "gateway.retry.wait_short_cooldown_cap_s" => {
                            if let Ok(n) = v.parse::<i64>() {
                                d.wait_short_cooldown_cap_s = n;
                            }
                        }
                        _ => {}
                    }
                }
                self.durations.insert((), d);
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "gateway-cooldown: refresh durate fallito, mantengo i valori correnti (fallback)"
                );
            }
        }
    }
}

/// Valore di `last_error_source` / `source` scritto dal gateway (mig 0536).
const LAST_ERROR_SOURCE: &str = "gateway";

/// Scrittura DB dell'errore provider (vedi [`CooldownManager::persist_last_error`]):
/// UPSERT dell'ultimo errore + riga history append-only con source='gateway'.
async fn persist_provider_error(pool: PgPool, provider: String, kind: &'static str, message: String) {
    let upsert = sqlx::query(
        "INSERT INTO nexus_provider_health \
           (provider, last_error, last_error_at, last_error_source, updated_at) \
         VALUES ($1, $2, NOW(), $3, NOW()) \
         ON CONFLICT (provider) DO UPDATE SET \
           last_error = EXCLUDED.last_error, \
           last_error_at = NOW(), \
           last_error_source = EXCLUDED.last_error_source, \
           updated_at = NOW()",
    )
    .bind(&provider)
    .bind(&message)
    .bind(LAST_ERROR_SOURCE)
    .execute(&pool)
    .await;
    let history = sqlx::query(
        "INSERT INTO nexus_provider_health_history \
           (provider, healthy, error_kind, error_message, source) \
         VALUES ($1, false, $2, $3, $4)",
    )
    .bind(&provider)
    .bind(kind)
    .bind(&message)
    .bind(LAST_ERROR_SOURCE)
    .execute(&pool)
    .await;
    let esiti = [
        ("UPSERT nexus_provider_health", upsert),
        ("INSERT nexus_provider_health_history", history),
    ];
    for (what, res) in esiti {
        if let Err(e) = res {
            tracing::warn!(provider, error = %e, "gateway-cooldown: {} fallito", what);
        }
    }
}

/// Scrittura DB del ripristino (vedi [`CooldownManager::persist_recovery`]):
/// riga append-only `healthy=true` con source='gateway'. Non tocca
/// `nexus_provider_health` (quella tabella e' lo snapshot dell'ULTIMO
/// errore, non ha bisogno di un "ultimo successo"): il consumatore
/// (`fetch_provider_health_map` in mcp-core) legge solo
/// `nexus_provider_health_history` per `healthy`/`checked_at`.
async fn persist_provider_recovery(pool: PgPool, provider: String) {
    let res = sqlx::query(
        "INSERT INTO nexus_provider_health_history \
           (provider, healthy, error_kind, error_message, source) \
         VALUES ($1, true, NULL, NULL, $2)",
    )
    .bind(&provider)
    .bind(LAST_ERROR_SOURCE)
    .execute(&pool)
    .await;
    if let Err(e) = res {
        tracing::warn!(
            provider,
            error = %e,
            "gateway-cooldown: INSERT ripristino in nexus_provider_health_history fallito"
        );
    }
}

/// Tronca a `max` caratteri rispettando i char boundary UTF-8 (uno slice di
/// byte `&s[..max]` panica a meta' di un carattere multibyte).
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

/// Legge l'intervallo di re-probe dai `settings`. Fallback alla costante se il
/// DB e' down o il valore manca/non e' parsabile. Non usa cache (e' letto una
/// volta all'avvio del loop e ad ogni giro per recepire i cambi a caldo).
pub async fn reprobe_interval_seconds(pool: &PgPool) -> u64 {
    let row = sqlx::query_as::<_, (String,)>(
        "SELECT value FROM settings WHERE key = 'gateway.cooldown.reprobe_interval_seconds'",
    )
    .fetch_optional(pool)
    .await;

    match row {
        Ok(Some((v,))) => v
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_REPROBE_INTERVAL_SECONDS),
        _ => DEFAULT_REPROBE_INTERVAL_SECONDS,
    }
}

/// Avvia il RE-PROBE LOOP in un task tokio dedicato. Ad ogni iterazione:
///   1. aggiorna le durate dai settings (cache 60s) e l'intervallo;
///   2. per OGNI provider in cooldown chiama `healthcheck()`;
///   3. se il provider torna sano, [`CooldownManager::clear`] lo riabilita.
///
/// Questo e' il cuore del fix: il provider non aspetta la scadenza nominale, ma
/// rientra appena un probe lo trova sano (es. dopo la ricarica crediti OpenAI).
///
/// Il loop e' infinito; il task termina quando l'handle viene droppato/abortito
/// (gestito dal chiamante, es. allo shutdown di mcp-core).
pub fn spawn_recovery_loop(
    manager: CooldownManager,
    providers: Vec<Arc<dyn LlmProvider>>,
    pool: PgPool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            // Aggiorna durate (cache) e intervallo a ogni giro: i cambi DB
            // vengono recepiti senza restart (regola G).
            manager.refresh_settings(&pool, false).await;
            let interval_secs = reprobe_interval_seconds(&pool).await;

            run_recovery_pass(&manager, &providers).await;

            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        }
    })
}

/// Un singolo passaggio di recovery: estratto come funzione pura (sul manager e
/// la lista provider) cosi' la logica e' testabile senza il loop infinito ne'
/// il timer. Per ogni provider in cooldown esegue il probe; se sano, lo libera.
pub async fn run_recovery_pass(manager: &CooldownManager, providers: &[Arc<dyn LlmProvider>]) {
    let in_cooldown = manager.providers_in_cooldown(Utc::now());
    if in_cooldown.is_empty() {
        return;
    }

    for provider in providers {
        let name = provider.name().to_string();
        if !in_cooldown.contains(&name) {
            continue;
        }
        // Una scadenza DICHIARATA dal fornitore non si abbrevia con un probe che
        // misura un'altra operazione: `healthcheck()` e' un `GET /models`, che
        // risponde anche mentre le completion sono rifiutate per quota o credito.
        // E' il difetto misurato il 12/08/2026 (vedi `il_probe_puo_liberare`).
        if let Some(stato) = manager.state(&name) {
            if !il_probe_puo_liberare(&stato) {
                tracing::debug!(
                    provider = %name,
                    scade_fra_s = (stato.until - Utc::now()).num_seconds().max(0),
                    "gateway-reprobe: scadenza dichiarata dal fornitore, il probe non la abbrevia"
                );
                continue;
            }
        }
        // Probe: NON consuma crediti di generazione (e' un /models). Se torna
        // sano, il provider rientra subito.
        if provider.healthcheck().await {
            manager.clear(&name);
        } else {
            tracing::debug!(
                provider = %name,
                "gateway-reprobe: provider ancora non sano, resta in cooldown"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use crate::provider::{ChunkStream, LlmProvider};
    use crate::types::{LlmRequest, LlmResponse, SensitivityTier};

    /// Provider finto controllabile: `healthy` decide l'esito dello healthcheck,
    /// `probe_calls` conta quante volte e' stato sondato. Nessuna rete.
    struct FakeProvider {
        name: String,
        healthy: std::sync::atomic::AtomicBool,
        probe_calls: AtomicUsize,
    }

    impl FakeProvider {
        fn new(name: &str, healthy: bool) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                healthy: std::sync::atomic::AtomicBool::new(healthy),
                probe_calls: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl LlmProvider for FakeProvider {
        fn name(&self) -> &str {
            &self.name
        }
        fn supports_tools(&self) -> bool {
            true
        }
        fn supports_streaming(&self) -> bool {
            true
        }
        fn max_context_tokens(&self) -> u32 {
            1000
        }
        fn tier_compatibility(&self) -> &[SensitivityTier] {
            &[0]
        }
        async fn complete(&self, _req: &LlmRequest) -> anyhow::Result<LlmResponse> {
            anyhow::bail!("non usato nei test cooldown")
        }
        async fn stream(&self, _req: &LlmRequest) -> anyhow::Result<ChunkStream> {
            anyhow::bail!("non usato nei test cooldown")
        }
        async fn healthcheck(&self) -> bool {
            self.probe_calls.fetch_add(1, Ordering::SeqCst);
            self.healthy.load(Ordering::SeqCst)
        }
    }

    fn t0() -> DateTime<Utc> {
        // Istante fisso e deterministico per i test (no Utc::now()).
        DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn mark_e_seconds_remaining_deterministici() {
        let m = CooldownManager::new();
        let now = t0();
        m.mark_at("openai", CooldownReason::Billing, None, now, 3600);

        // Subito dopo: ~3600s rimanenti.
        assert!(m.is_in_cooldown_at("openai", now));
        assert_eq!(m.seconds_remaining_at("openai", now), 3600);

        // A meta' durata: 1800s rimanenti.
        let mid = now + chrono::Duration::seconds(1800);
        assert!(m.is_in_cooldown_at("openai", mid));
        assert_eq!(m.seconds_remaining_at("openai", mid), 1800);

        // Dopo la scadenza: non piu' in cooldown, 0 rimanenti.
        let after = now + chrono::Duration::seconds(3601);
        assert!(!m.is_in_cooldown_at("openai", after));
        assert_eq!(m.seconds_remaining_at("openai", after), 0);
    }

    #[test]
    fn clear_rimuove_il_cooldown() {
        let m = CooldownManager::new();
        let now = t0();
        m.mark_at("openai", CooldownReason::Transient, None, now, 30);
        assert!(m.is_in_cooldown_at("openai", now));
        m.clear("openai");
        assert!(!m.is_in_cooldown_at("openai", now));
        assert_eq!(m.seconds_remaining_at("openai", now), 0);
    }

    #[test]
    fn provider_sconosciuto_non_in_cooldown() {
        let m = CooldownManager::new();
        assert!(!m.is_in_cooldown_at("ignoto", t0()));
        assert_eq!(m.seconds_remaining_at("ignoto", t0()), 0);
    }

    #[test]
    fn truncate_chars_rispetta_i_char_boundary() {
        assert_eq!(truncate_chars("ciao", 10), "ciao");
        assert_eq!(truncate_chars("ciao mondo", 4), "ciao…");
        // Multibyte: slicing per byte panicherebbe, per char no.
        assert_eq!(truncate_chars("èèèèè", 3), "èèè…");
    }

    /// Il fix del bug misurato il 31/07/2026 (deepseek healthy=false per 8+
    /// minuti dopo un riavvio, mai ri-verificato): `clear()` deve persistere
    /// il ripristino, non solo rimuovere lo stato in-memory. Attraversa il
    /// PRODUTTORE reale (`clear`), non ricostruisce l'INSERT a mano (regola
    /// O). Poll breve perche' la persistenza e' fire-and-forget
    /// (`tokio::spawn` non atteso dal chiamante, per non rallentare il path
    /// di richiesta/re-probe).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn clear_su_provider_in_cooldown_persiste_il_ripristino(pool: PgPool) {
        let m = CooldownManager::new();
        m.attach_db(pool.clone());
        m.mark_at(
            "deepseek",
            CooldownReason::Transient,
            Some("timeout".to_string()),
            Utc::now(),
            30,
        );
        // Attende che la riga d'errore sia REALMENTE scritta prima di
        // procedere: entrambe le persistenze (mark_at e clear) sono
        // fire-and-forget su `tokio::spawn`, quindi chiamarle in sequenza
        // ravvicinata senza questa attesa metterebbe i due INSERT in race
        // fra loro (ordine di `checked_at` non garantito). In produzione
        // questa race non esiste: fra un errore osservato e il recovery
        // passano minuti (il `reprobe_interval_seconds`), qui la
        // riproduciamo nel test rispettando l'ordine causale reale.
        attendi_riga_healthy(&pool, "deepseek", false).await;

        m.clear("deepseek");

        let source = attendi_riga_healthy(&pool, "deepseek", true).await;
        assert_eq!(source, "gateway");
    }

    /// Mutazione di controllo: se il provider NON era in cooldown, `clear()`
    /// non deve scrivere nulla (altrimenti ogni richiesta riuscita su un
    /// provider gia' sano genererebbe rumore append-only senza fine).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn clear_su_provider_non_in_cooldown_non_scrive_nulla(pool: PgPool) {
        let m = CooldownManager::new();
        m.attach_db(pool.clone());

        m.clear("mistral");
        // Nessuna attesa di poll: verifichiamo l'assenza dopo un breve margine,
        // sufficiente perche' una scrittura errata abbia il tempo di comparire.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM nexus_provider_health_history WHERE provider = $1",
        )
        .bind("mistral")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 0);
    }

    /// IL DIFETTO DEL NOME (misurato il 13/08/2026): questo scrittore metteva in
    /// `error_kind` la CLASSE del cooldown (`billing`), mentre il probe di
    /// mcp-core metteva la CAUSA (`credit_balance_too_low`) per lo STESSO stato —
    /// due righe nello stesso millisecondo per openai, e nessun filtro capace di
    /// trovarle entrambe.
    ///
    /// Il test attraversa il produttore reale (`mark_billing` -> persistenza) e
    /// asserisce il valore CANONICO condiviso, non una stringa ricopiata: se
    /// qualcuno cambia il vocabolario in `nexus-types`, i due lati si muovono
    /// insieme o rosseggiano insieme.
    ///
    /// MUTAZIONE: rimettere `reason.as_str()` in `persist_last_error` -> il valore
    /// letto e' `billing` e l'assert cade con la stringa del difetto reale.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn error_kind_del_cooldown_billing_e_il_nome_canonico(pool: PgPool) {
        let m = CooldownManager::new();
        m.attach_db(pool.clone());

        m.mark_billing("anthropic", Some("credit balance too low".to_string()));

        let kind = attendi_error_kind(&pool, "anthropic").await;
        assert_eq!(
            kind,
            stato_salute::CREDIT_BALANCE_TOO_LOW,
            "la colonna error_kind vuole la causa condivisa coi due scrittori, non la classe di cooldown"
        );
    }

    /// La durata dell'esclusione per credito la legge da `provider.cooldown_long_s`,
    /// la STESSA chiave di mcp-core: prima ne aveva una propria
    /// (`gateway.cooldown.billing_seconds`, 3600) e lo stesso evento aveva due
    /// durate nei due processi.
    ///
    /// Il valore di prova e' DISTINTO dal fallback apposta: se la query tornasse a
    /// nominare la chiave vecchia non troverebbe nulla e cadrebbe sul fallback —
    /// che, essendo ora lo stesso 21600 del setting, renderebbe muto un test
    /// scritto sul valore di default.
    ///
    /// MUTAZIONE: rimettere `'gateway.cooldown.billing_seconds'` nella `IN (...)`
    /// -> il residuo torna al fallback e l'assert cade.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_durata_billing_viene_dalla_chiave_condivisa(pool: PgPool) {
        const PROVA_S: i64 = 12345;
        sqlx::query("UPDATE settings SET value = $1 WHERE key = $2")
            .bind(PROVA_S.to_string())
            .bind(durata::CHIAVE_COOLDOWN_LUNGO)
            .execute(&pool)
            .await
            .unwrap();

        let m = CooldownManager::new();
        m.refresh_settings(&pool, true).await;
        let now = t0();
        m.mark_at(
            "openai",
            CooldownReason::Billing,
            None,
            now,
            m.current_durations().billing_seconds,
        );

        assert_eq!(m.seconds_remaining_at("openai", now), PROVA_S);
    }

    /// Poll fino a 2s per l'`error_kind` dell'ultima riga non sana del provider.
    /// Stessa ragione di [`attendi_riga_healthy`]: la persistenza e'
    /// fire-and-forget e non c'e' un handle da attendere.
    async fn attendi_error_kind(pool: &PgPool, provider: &str) -> String {
        for _ in 0..20 {
            if let Some(Some(kind)) = sqlx::query_scalar::<_, Option<String>>(
                "SELECT error_kind FROM nexus_provider_health_history \
                 WHERE provider = $1 AND healthy = false ORDER BY checked_at DESC LIMIT 1",
            )
            .bind(provider)
            .fetch_optional(pool)
            .await
            .unwrap()
            {
                return kind;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("nessuna riga con error_kind per '{provider}' entro 2s");
    }

    /// Poll fino a 2s per una riga con l'esatto `healthy` atteso (non
    /// semplicemente "l'ultima riga"): la scrittura e' un `tokio::spawn`
    /// fire-and-forget, non c'e' un handle da attendere dal lato del test
    /// senza cambiare la firma di produzione per farne uno strumento di test
    /// (regola O: non piegare il produttore alla misura). Cercare per
    /// `healthy` invece che per "ultima per checked_at" evita di dipendere
    /// dall'ordine di completamento relativo di due INSERT asincroni.
    async fn attendi_riga_healthy(pool: &PgPool, provider: &str, atteso: bool) -> String {
        for _ in 0..20 {
            if let Some(source) = sqlx::query_scalar::<_, String>(
                "SELECT source FROM nexus_provider_health_history \
                 WHERE provider = $1 AND healthy = $2 ORDER BY checked_at DESC LIMIT 1",
            )
            .bind(provider)
            .bind(atteso)
            .fetch_optional(pool)
            .await
            .unwrap()
            {
                return source;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "nessuna riga con healthy={atteso} per '{provider}' in \
             nexus_provider_health_history entro 2s"
        );
    }

    #[tokio::test]
    async fn mark_senza_pool_dentro_runtime_e_noop_di_persistenza() {
        // Con runtime tokio attivo ma senza pool collegato, mark_at non deve
        // ne' panicare ne' bloccarsi: la persistenza e' un no-op e lo stato
        // in-memory resta corretto.
        let m = CooldownManager::new();
        let now = t0();
        m.mark_at(
            "acme",
            CooldownReason::Transient,
            Some("HTTP 502 upstream".to_string()),
            now,
            30,
        );
        assert!(m.is_in_cooldown_at("acme", now));
    }

    /// LA REGRESSIONE (misurata il 10/08/2026 su groq, tetto GIORNALIERO esaurito):
    /// il provider dichiara un `Retry-After` di minuti, il cooldown ne applicava 30
    /// secondi fissi, e il log del gateway mostra "cooldown 30s -> ritento -> 429 ->
    /// cooldown 30s" ininterrotto per mezz'ora. Ora la durata dichiarata dal provider
    /// vince, perche' e' lui a sapere quando tornera' a servire.
    #[test]
    fn il_cooldown_transitorio_onora_il_retry_after_del_provider() {
        let m = CooldownManager::new();
        // 4m56s: il valore che groq indica quando e' il tetto giornaliero a scattare.
        m.mark_transient_after("groq", None, Some("429 TPD".to_string()), Some(296));

        let fra_un_minuto = Utc::now() + chrono::Duration::seconds(60);
        assert!(
            m.is_in_cooldown_at("groq", fra_un_minuto),
            "a +60s deve essere ancora escluso: il provider aveva chiesto 296s, \
             ripresentarsi prima significa riprendere lo stesso 429"
        );

        let fra_dieci_minuti = Utc::now() + chrono::Duration::seconds(600);
        assert!(
            !m.is_in_cooldown_at("groq", fra_dieci_minuti),
            "scaduta l'attesa dichiarata, il provider torna disponibile"
        );
    }

    /// IL CASO MISURATO il 13/08/2026. groq risponde
    /// «Rate limit reached for model `openai/gpt-oss-20b` ... TPD Limit 200000,
    /// Used 199788 ... try again in 23m44.3s»: e' il tetto GIORNALIERO di QUEL
    /// modello, e il gateway escludeva groq intero per 24 minuti.
    ///
    /// MUTAZIONE: passare `None` come modello a `mark_transient_after` (cioe' la
    /// firma di prima) -> `il_fornitore_resta_disponibile` cade, ed e' il difetto
    /// reale: un modello a tetto raggiunto porta via anche chi ha quota propria.
    #[test]
    fn un_tetto_di_modello_non_esclude_il_fornitore() {
        let m = CooldownManager::new();
        // 1424s = 23m44,3s, il Retry-After misurato.
        m.mark_transient_after(
            "groq",
            Some("openai/gpt-oss-20b"),
            Some("429 rate_limit".to_string()),
            Some(1424),
        );

        let fra_un_minuto = Utc::now() + chrono::Duration::seconds(60);
        assert!(
            m.is_model_in_cooldown_at("groq", "openai/gpt-oss-20b", fra_un_minuto),
            "il modello che ha esaurito il proprio tetto resta fuori per l'attesa dichiarata"
        );
        assert!(
            !m.is_in_cooldown_at("groq", fra_un_minuto),
            "il FORNITORE non e' escluso: e' la domanda che la selezione pone quando \
             sceglie fra i suoi altri modelli"
        );
        assert!(
            !m.is_model_in_cooldown_at("groq", "llama-3.3-70b", fra_un_minuto),
            "un altro modello dello stesso fornitore ha quota propria e resta usabile"
        );

        // Il residuo della coppia e' quello dichiarato; il fornitore non ne ha.
        assert!(m.seconds_remaining_for_model("groq", "openai/gpt-oss-20b") > 1300);
        assert_eq!(m.seconds_remaining("groq"), 0);
        assert_eq!(
            m.portata_attiva("groq", "openai/gpt-oss-20b"),
            Some(PortataCooldown::Modello)
        );
        assert_eq!(m.portata_attiva("groq", "llama-3.3-70b"), None);
    }

    /// L'altro verso: cio' che e' del FORNITORE resta del fornitore, altrimenti
    /// il fix diventerebbe «niente e' mai del fornitore» e un endpoint morto
    /// verrebbe ritentato modello per modello.
    #[test]
    fn un_guasto_di_trasporto_esclude_tutto_il_fornitore() {
        let m = CooldownManager::new();
        m.mark_transient_after("acme", None, Some("connessione rifiutata".into()), Some(120));

        let fra_un_minuto = Utc::now() + chrono::Duration::seconds(60);
        assert!(m.is_in_cooldown_at("acme", fra_un_minuto));
        for modello in ["modello-a", "modello-b"] {
            assert!(
                m.is_model_in_cooldown_at("acme", modello, fra_un_minuto),
                "l'endpoint non risponde: nessun suo modello e' utilizzabile"
            );
        }
        assert_eq!(
            m.portata_attiva("acme", "modello-a"),
            Some(PortataCooldown::Fornitore)
        );
    }

    /// La portata la decide la CAUSA, letta dai segnali strutturati (regola M):
    /// mai il testo del messaggio, che per groq direbbe «Rate limit reached for
    /// model ...» e per un altro fornitore chissa'.
    #[test]
    fn la_portata_si_deriva_da_status_e_codice() {
        // 429 nudo: tetto di frequenza per definizione HTTP.
        assert_eq!(
            PortataCooldown::da_segnale(Some(429), None),
            PortataCooldown::Modello
        );
        // groq dichiara il rate limit su un 413: lo status da solo mentirebbe.
        assert_eq!(
            PortataCooldown::da_segnale(Some(413), Some("rate_limit_exceeded")),
            PortataCooldown::Modello
        );
        // 5xx e assenza di segnali: l'endpoint, non il modello.
        assert_eq!(
            PortataCooldown::da_segnale(Some(503), None),
            PortataCooldown::Fornitore
        );
        assert_eq!(
            PortataCooldown::da_segnale(None, None),
            PortataCooldown::Fornitore
        );

        assert_eq!(PortataCooldown::Modello.modello("m"), Some("m"));
        assert_eq!(PortataCooldown::Fornitore.modello("m"), None);
    }

    /// Un 200 su un modello prova che il fornitore serve, non che il tetto di un
    /// ALTRO suo modello sia rientrato.
    #[test]
    fn il_successo_libera_la_coppia_e_il_fornitore_ma_non_gli_altri_modelli() {
        let m = CooldownManager::new();
        m.mark_transient_after("groq", Some("modello-a"), None, Some(600));
        m.mark_transient_after("groq", Some("modello-b"), None, Some(600));
        m.mark_transient_after("groq", None, None, Some(600));

        m.clear_model("groq", "modello-a");

        assert!(!m.is_model_in_cooldown("groq", "modello-a"));
        assert!(!m.is_in_cooldown("groq"));
        assert!(
            m.is_model_in_cooldown("groq", "modello-b"),
            "nessuno ha misurato modello-b: il suo tetto resta dov'era"
        );
    }

    /// Le due letture che parlano di FORNITORI non devono vedere le chiavi di
    /// coppia: `/status` esporrebbe un fornitore «non sano» che sta servendo, e
    /// il re-probe interrogherebbe `GET /models` per rispondere su un tetto
    /// token — cioe' la conflazione appena tolta, rientrata da un'altra porta.
    #[test]
    fn le_letture_per_fornitore_ignorano_le_chiavi_di_coppia() {
        let m = CooldownManager::new();
        let now = t0();
        m.mark_at_con_origine(
            "groq",
            Some("openai/gpt-oss-20b"),
            CooldownReason::Transient,
            Some("429".into()),
            now,
            600,
            OrigineScadenza::Dichiarata,
        );

        assert!(m.snapshot_at(now + chrono::Duration::seconds(1)).is_empty());
        assert!(m
            .providers_in_cooldown(now + chrono::Duration::seconds(1))
            .is_empty());
    }

    /// Il controllo dell'altro verso: senza `Retry-After` resta il default (30s), cosi'
    /// il test sopra non passerebbe per una durata alzata a tutti.
    #[test]
    fn senza_retry_after_resta_il_cooldown_transitorio_di_default() {
        let m = CooldownManager::new();
        m.mark_transient("acme", None, Some("timeout".to_string()));

        let fra_un_minuto = Utc::now() + chrono::Duration::seconds(60);
        assert!(
            !m.is_in_cooldown_at("acme", fra_un_minuto),
            "senza segnale del provider vale il transient di default, ben sotto i 60s"
        );
    }

    #[test]
    fn snapshot_riporta_solo_in_cooldown_con_billing_flag() {
        let m = CooldownManager::new();
        let now = t0();
        m.mark_at(
            "openai",
            CooldownReason::Billing,
            Some("credit balance too low".to_string()),
            now,
            3600,
        );
        m.mark_at("mistral", CooldownReason::Transient, Some("timeout".to_string()), now, 30);
        // Provider scaduto: non deve comparire.
        m.mark_at("deepseek", CooldownReason::Transient, None, now, 10);

        let later = now + chrono::Duration::seconds(20);
        let snap = m.snapshot_at(later);

        // deepseek scaduto a +20s, openai e mistral ancora attivi.
        assert_eq!(snap.len(), 2);

        let openai = snap.iter().find(|s| s.name == "openai").unwrap();
        assert!(!openai.healthy);
        assert_eq!(openai.billing_error.as_deref(), Some("credit balance too low"));

        let mistral = snap.iter().find(|s| s.name == "mistral").unwrap();
        // Transient: nessun billing_error, ma last_error presente.
        assert!(mistral.billing_error.is_none());
        assert_eq!(mistral.last_error.as_deref(), Some("timeout"));
    }

    #[tokio::test]
    async fn recovery_pass_libera_provider_tornato_sano() {
        let m = CooldownManager::new();
        // openai in cooldown billing, healthcheck simulato SANO -> verra' liberato.
        m.mark_at("openai", CooldownReason::Billing, None, Utc::now(), 3600);
        let openai = FakeProvider::new("openai", true);
        let providers: Vec<Arc<dyn LlmProvider>> = vec![openai.clone()];

        assert!(m.is_in_cooldown("openai"));
        run_recovery_pass(&m, &providers).await;

        // Probe eseguito una volta e provider liberato (il fix: rientro reattivo).
        assert_eq!(openai.probe_calls.load(Ordering::SeqCst), 1);
        assert!(!m.is_in_cooldown("openai"));
    }

    /// IL CASO MISURATO il 12/08/2026 su groq. Il fornitore risponde 429 con
    /// `Retry-After`, il gateway lo legge e lo onora (fix del 10/08), e il probe
    /// di ripristino lo cancellava perche' `GET /models` risponde: 565 secondi
    /// di cooldown liberati dopo 170, e un nuovo 429 col medesimo Retry-After
    /// QUATTRO SECONDI dopo. Su anthropic la stessa dinamica produceva una
    /// quarantina di ripristini anticipati di 3000-3500s, in ciclo.
    ///
    /// MUTAZIONE: togliere il `continue` da `run_recovery_pass` (o far ritornare
    /// `true` a `il_probe_puo_liberare` per `Dichiarata`) -> questo test cade
    /// esattamente sul difetto reale: probe eseguito e cooldown sparito.
    #[tokio::test]
    async fn il_probe_non_abbrevia_una_scadenza_dichiarata_dal_fornitore() {
        let m = CooldownManager::new();
        // 429 con Retry-After: la scadenza la dichiara il fornitore.
        m.mark_transient_after("groq", None, Some("429 rate_limit".into()), Some(565));
        let groq = FakeProvider::new("groq", true); // /models risponde: e' il punto
        let providers: Vec<Arc<dyn LlmProvider>> = vec![groq.clone()];

        run_recovery_pass(&m, &providers).await;

        assert_eq!(
            groq.probe_calls.load(Ordering::SeqCst),
            0,
            "contro una scadenza dichiarata il probe non va nemmeno eseguito"
        );
        assert!(
            m.is_in_cooldown("groq"),
            "il Retry-After del fornitore non si abbrevia con una misura che riguarda un'altra operazione"
        );
    }

    /// La contropartita: dove la scadenza e' una NOSTRA stima, una misura la
    /// puo' revocare. Senza questo, il fix diventerebbe un blocco che non si
    /// scioglie mai e il rientro reattivo sparirebbe.
    #[tokio::test]
    async fn una_scadenza_stimata_resta_revocabile_dal_probe() {
        let m = CooldownManager::new();
        // Nessun Retry-After: la durata e' la nostra.
        m.mark_transient_after("mistral", None, Some("timeout".into()), None);
        let mistral = FakeProvider::new("mistral", true);
        let providers: Vec<Arc<dyn LlmProvider>> = vec![mistral.clone()];

        run_recovery_pass(&m, &providers).await;

        assert_eq!(mistral.probe_calls.load(Ordering::SeqCst), 1);
        assert!(!m.is_in_cooldown("mistral"));
    }

    #[tokio::test]
    async fn recovery_pass_lascia_in_cooldown_provider_ancora_rotto() {
        let m = CooldownManager::new();
        m.mark_at("openai", CooldownReason::Billing, None, Utc::now(), 3600);
        // Ancora non sano (crediti non ricaricati).
        let openai = FakeProvider::new("openai", false);
        let providers: Vec<Arc<dyn LlmProvider>> = vec![openai.clone()];

        run_recovery_pass(&m, &providers).await;

        assert_eq!(openai.probe_calls.load(Ordering::SeqCst), 1);
        assert!(m.is_in_cooldown("openai"));
    }

    #[tokio::test]
    async fn recovery_pass_non_sonda_provider_non_in_cooldown() {
        let m = CooldownManager::new();
        // Nessun provider in cooldown: il pass non deve sondare nulla.
        let openai = FakeProvider::new("openai", true);
        let providers: Vec<Arc<dyn LlmProvider>> = vec![openai.clone()];

        run_recovery_pass(&m, &providers).await;

        assert_eq!(openai.probe_calls.load(Ordering::SeqCst), 0);
    }
}
