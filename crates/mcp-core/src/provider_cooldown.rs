//! Cooldown e circuit breaker per provider LLM.
//!
//! Estratto da `agent_loop.rs` durante la Fase 4 del refactor Nexus: i symbol
//! `is_provider_in_cooldown`, `put_provider_in_cooldown` e
//! `reset_provider_failures` sono usati anche fuori dal loop agente
//! (es. `orchestrator.rs`).

use nexus_types::provider_failure::EsclusioneDichiarata;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};

/// Tempi di health/cooldown provider, DB-driven (regola G). Inizializzati da
/// `main.rs` all'avvio leggendo la tabella `settings` (chiavi `provider.*`,
/// migrazioni 0253/0255). Se `init_provider_health_timings` non viene chiamato (es.
/// nei test unitari), si usano i default storici qui sotto — cosi' il modulo
/// resta utilizzabile senza dipendere dal DB.
#[derive(Debug, Clone, Copy)]
pub struct ProviderHealthTimings {
    pub cooldown_default_s: u64,
    pub cooldown_min_s: u64,
    pub cooldown_max_s: u64,
    pub cooldown_long_s: u64,
    pub circuit_breaker_window_s: u64,
    pub circuit_breaker_threshold: usize,
    pub circuit_breaker_extended_cooldown_s: u64,
    pub health_probe_timeout_s: u64,
    pub slow_cooldown_s: u64,
    pub outage_threshold: usize,
    pub billing_recovery_interval_s: u64,
    pub recovery_probe_timeout_s: u64,
    /// GOVERNANCE: TTL adattivo del cooldown LUNGO (billing) per TIPO d'errore
    /// (`agent.governance.cooldown_adaptive_ttl`, regola G, default OFF). Con OFF il
    /// cooldown lungo resta `cooldown_long_s` (bit-identico). NON tocca il
    /// circuit-breaker ne' la fallback-chain (reti di sicurezza FISSE).
    pub adaptive_billing_cooldown_enabled: bool,
    /// TTL ridotto (s) usato dal cooldown adattivo per gli errori billing a recupero
    /// periodico PREVEDIBILE (quota/rate), quando il flag e' ON
    /// (`agent.governance.cooldown_adaptive_ttl_min_s`). Clampato in
    /// `[cooldown_min_s, cooldown_long_s]`. Gli errori hard (credit/balance/payment,
    /// ricarica manuale imprevedibile) restano a `cooldown_long_s`.
    pub adaptive_billing_cooldown_min_s: u64,
}

impl Default for ProviderHealthTimings {
    fn default() -> Self {
        Self {
            cooldown_default_s: 300,
            cooldown_min_s: 10,
            cooldown_max_s: 3600,
            cooldown_long_s: 6 * 3600,
            circuit_breaker_window_s: 60,
            circuit_breaker_threshold: 3,
            circuit_breaker_extended_cooldown_s: 600,
            health_probe_timeout_s: 30,
            slow_cooldown_s: 60,
            outage_threshold: 3,
            billing_recovery_interval_s: 60,
            recovery_probe_timeout_s: 30,
            // Governance TTL adattivo OFF di default (opt-in): con OFF il cooldown
            // lungo resta 6h (bit-identico). Min ridotto storico-neutro: 2h.
            adaptive_billing_cooldown_enabled: false,
            adaptive_billing_cooldown_min_s: 2 * 3600,
        }
    }
}

/// TTL adattivo del cooldown LUNGO (billing) per TIPO d'errore, derivato dalla
/// `reason` STRUTTURATA (regola M: la reason e' la categoria d'errore, non prosa da
/// classificare). Funzione PURA (testabile senza stato globale). Opt-in
/// (`adaptive_billing_cooldown_enabled`): con OFF ritorna sempre `cooldown_long_s`
/// -> comportamento bit-identico. NON tocca il circuit-breaker ne' la fallback-chain
/// (reti di sicurezza FISSE, per vincolo del task).
///
/// Il re-probe periodico (`BILLING_REPROBE_INTERVAL_S`) recupera comunque in
/// anticipo se il credito torna: il TTL e' solo il LIMITE SUPERIORE. La riduzione e'
/// quindi a basso rischio.
///   - hard billing (`credit`/`balance`/`payment`, ricarica MANUALE imprevedibile)
///     -> `cooldown_long_s` pieno (nessuna riduzione).
///   - quota/rate (recupero periodico PREVEDIBILE) -> `adaptive_billing_cooldown_min_s`
///     clampato in `[cooldown_min_s, cooldown_long_s]`.
///   - ogni altra reason -> `cooldown_long_s` (conservativo).
pub fn adaptive_billing_cooldown_secs(reason: &str, timings: &ProviderHealthTimings) -> u64 {
    if !timings.adaptive_billing_cooldown_enabled {
        return timings.cooldown_long_s;
    }
    let r = reason.to_ascii_lowercase();
    // Hard billing: ricarica manuale -> nessuna riduzione.
    if r.contains("credit") || r.contains("balance") || r.contains("payment") {
        return timings.cooldown_long_s;
    }
    // Quota/rate: recupero periodico prevedibile -> TTL ridotto (clampato).
    if r.contains("quota") || r.contains("rate") {
        // Clamp robusto: `cooldown_min_s`/`cooldown_long_s` vengono da settings DB
        // INDIPENDENTI, senza validazione reciproca. Se un operatore li invertisse
        // (min > long) `u64::clamp` panicherebbe (`assert!(min <= max)`) proprio sul
        // path di gestione errori billing (quando il sistema e' gia' in difficolta').
        // `min(min_s, long_s)` garantisce lower <= upper: config invertita -> degrada
        // al cooldown lungo pieno (conservativo), mai panic.
        return timings.adaptive_billing_cooldown_min_s.clamp(
            timings.cooldown_min_s.min(timings.cooldown_long_s),
            timings.cooldown_long_s,
        );
    }
    // Conservativo: qualunque altra causa resta al cooldown lungo pieno.
    timings.cooldown_long_s
}

static HEALTH_TIMINGS: OnceLock<ProviderHealthTimings> = OnceLock::new();

/// Inizializza i tempi health/cooldown dai settings DB. Idempotente: la prima
/// chiamata vince (OnceLock). Va invocata una sola volta all'avvio da main.rs.
pub fn init_provider_health_timings(timings: ProviderHealthTimings) {
    let _ = HEALTH_TIMINGS.set(timings);
}

/// Ritorna i tempi correnti (copia). Default storici se non ancora inizializzati.
pub fn provider_health_timings() -> ProviderHealthTimings {
    HEALTH_TIMINGS.get().copied().unwrap_or_default()
}

static PROVIDER_COOLDOWN: OnceLock<Mutex<HashMap<ChiaveCooldown, std::time::Instant>>> =
    OnceLock::new();

// -- Circuit breaker state --
// Traccia gli istanti dei fallimenti recenti per provider. Se la soglia di
// fallimenti viene superata entro la finestra, entriamo in stato OPEN con
// cooldown esteso (durate da `provider_health_timings()`).
static PROVIDER_FAILURES: OnceLock<Mutex<HashMap<String, Vec<std::time::Instant>>>> =
    OnceLock::new();

/// Intervallo di ri-probe per i provider ancora in cooldown BILLING: il credito
/// puo' tornare con una ricarica imprevedibile, quindi non si aspetta la scadenza
/// del cooldown lungo (6h) ma si riprova ogni 10 minuti.
const BILLING_REPROBE_INTERVAL_S: u64 = 600;

/// Ultimo istante in cui il recovery loop ha ri-probato un provider ancora in
/// cooldown. Limita la frequenza dei re-probe (vedi BILLING_REPROBE_INTERVAL_S),
/// cosi' il probe periodico non martella il provider ad ogni giro del loop.
static LAST_RECOVERY_PROBE: OnceLock<Mutex<HashMap<String, std::time::Instant>>> = OnceLock::new();

/// True se il provider in cooldown va ri-probato adesso (mai probato, o ultimo
/// probe piu' vecchio di `interval`); in tal caso aggiorna il timestamp.
fn should_reprobe_cooldown(provider: &str, interval: std::time::Duration) -> bool {
    let store = LAST_RECOVERY_PROBE.get_or_init(|| Mutex::new(HashMap::new()));
    match store.lock() {
        Ok(mut map) => {
            let now = std::time::Instant::now();
            let due = map
                .get(provider)
                .map(|last| now.duration_since(*last) >= interval)
                .unwrap_or(true);
            if due {
                map.insert(provider.to_string(), now);
            }
            due
        }
        Err(_) => false,
    }
}

/// La PORTATA di un cooldown: il solo fornitore, oppure una sua coppia col
/// modello. Il vocabolario e' canonico e in inglese (regola N) perche' viaggia
/// sul wire degli endpoint che mostrano lo stato dei cooldown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PortataCooldown {
    /// Il fornitore e' escluso per INTERO: ogni suo modello lo e' con lui.
    Provider,
    /// E' escluso un SOLO modello di quel fornitore; gli altri restano usabili.
    Model,
}

/// La chiave sotto cui vive un cooldown: un fornitore, e l'eventuale MODELLO.
///
/// PERCHE' DUE FORME. Un rate limit e' del MODELLO: i tetti token-al-minuto
/// sono per singolo modello, e `gemini-2.5-flash` ha la sua quota anche mentre
/// `gemini-2.5-pro` e' saturo. Il credito esaurito e le credenziali non valide
/// sono invece dell'ACCOUNT: riguardano ogni modello di quel fornitore.
///
/// Finche' la chiave era una sola — il fornitore — un 429 su un modello
/// escludeva tutti gli altri dello stesso fornitore per 60 secondi. MISURATO
/// nella notte fra il 6 e il 7/08/2026: 479 cooldown brevi (picchi di 88
/// all'ora), 22 failover su 39 escalation di un solo run, tutti con motivo
/// `cooldown`, su fornitori che nel DB non avevano alcun cooldown persistente.
/// L'utente lo ha descritto cosi': «molti cambi di provider per cooldown, e
/// poco dopo rifunzionavano» — poco dopo erano i 60 secondi, perche' il
/// fornitore non era mai stato guasto.
///
/// PERCHE' E' UN TIPO E NON UNA STRINGA COMPOSTA. La portata per coppia nasce
/// il 07/08/2026, e la chiave era `provider` oppure `provider\u{1}model`: una
/// STRINGA, che lo snapshot proiettava in un campo di nome `provider` e nove
/// consumatori fuori da questo modulo leggevano come nome di fornitore.
/// MISURATO sul sistema vivo il 13/08/2026, `GET /api/internal/routing/cooldown`
/// rispondeva `{"provider":"groq<U+0001>openai/gpt-oss-20b", ...}` — una stringa
/// che nessun `provider` del catalogo eguagliera' mai. La conseguenza non era
/// un errore visibile: la selezione del modello, che quella lista la inietta in
/// `AND LOWER(provider) <> ALL($1)`, semplicemente non ANTICIPAVA piu' nulla —
/// sceglieva la coppia in cooldown, la mandava, e il gateway (che il cooldown lo
/// applica bene, via `is_model_in_cooldown`) la rifiutava attendendo
/// (`attendo cooldown transitorio breve prima di ritentare wait_s=25`). Un giro
/// di selezione sprecato piu' l'attesa, per ogni occorrenza.
///
/// Coi campi separati quello scambio non e' piu' rappresentabile: chi legge non
/// riceve una stringa che POTREBBE essere un nome di fornitore, riceve il
/// fornitore e — distinto — il modello, e deve dichiarare quale delle due
/// domande sta ponendo. I campi sono privati e la costruzione passa dai
/// costruttori, che normalizzano una volta sola.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChiaveCooldown {
    provider: String,
    model: Option<String>,
}

impl ChiaveCooldown {
    /// Tutto il fornitore: credito, credenziali, budget, endpoint irraggiungibile.
    pub fn fornitore(provider: &str) -> Self {
        Self::nuova(provider, None)
    }

    /// La sola coppia: un tetto che riguarda quel modello e nessun altro.
    pub fn coppia(provider: &str, model: &str) -> Self {
        Self::nuova(provider, Some(model))
    }

    /// Costruttore generale. `None` — e un modello vuoto o di soli spazi, che
    /// non e' un modello — ricadono sul fornitore intero.
    pub fn nuova(provider: &str, model: Option<&str>) -> Self {
        let normalizza = |s: &str| s.trim().to_lowercase();
        Self {
            provider: normalizza(provider),
            model: model.map(normalizza).filter(|m| !m.is_empty()),
        }
    }

    /// Il fornitore, in lowercase. E' SEMPRE un nome di fornitore: mai una
    /// chiave composta.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Il modello, se il cooldown ne riguarda uno solo.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// True se l'esclusione vale per OGNI modello del fornitore.
    pub fn esclude_il_fornitore(&self) -> bool {
        self.model.is_none()
    }

    /// Il vocabolario della portata, per chi la deve mostrare o serializzare.
    pub fn portata(&self) -> PortataCooldown {
        if self.esclude_il_fornitore() {
            PortataCooldown::Provider
        } else {
            PortataCooldown::Model
        }
    }

    /// Testo per l'umano, composto DAI campi (regola Q punto 3): nessun codice
    /// lo rilegge per ricavarne il fornitore.
    pub fn etichetta(&self) -> String {
        match &self.model {
            Some(m) => format!("{}/{}", self.provider, m),
            None => self.provider.clone(),
        }
    }
}

/// Il FORNITORE e' escluso? Vero solo per i cooldown di account (credito, auth,
/// budget): un limite su un singolo modello non risponde si' a questa domanda.
///
/// Chi deve scegliere una coppia fornitore+modello usa
/// [`is_model_in_cooldown`], che e' la domanda completa.
pub fn is_provider_in_cooldown(provider: &str) -> bool {
    scaduto_o_attivo(&ChiaveCooldown::fornitore(provider))
}

/// Questa COPPIA e' utilizzabile adesso?
///
/// Vero se il fornitore e' escluso come account, oppure se lo e' questo modello
/// in particolare. E' la domanda che si pone chi sta per instradare una
/// richiesta: entrambe le esclusioni la impediscono, ma per ragioni diverse e
/// con durate diverse.
pub fn is_model_in_cooldown(provider: &str, model: &str) -> bool {
    is_provider_in_cooldown(provider) || scaduto_o_attivo(&ChiaveCooldown::coppia(provider, model))
}

/// Secondi che restano su UNA chiave (0 se scaduta o assente).
///
/// Punto unico della lettura del residuo. Nasceva prendendo la chiave come
/// STRINGA, e il suo commento diceva perche': «il campo `provider` di
/// `cooldown_snapshot_entries` porta la chiave grezza, quindi cercarvi un
/// fornitore per nome non trova mai una coppia». Era un aggiramento del difetto
/// D1, non una domanda a se' — ma la domanda che pone serve ancora, ed e' quella
/// del confronto «ALLINEARE, MAI ACCORCIARE» in
/// [`registra_esclusione_dichiarata`]. Ora la chiave e' un TIPO, quindi non c'e'
/// piu' niente da aggirare: chi chiede il residuo dichiara di quale esclusione
/// lo sta chiedendo, e un'attesa sulla coppia non si misura contro il cooldown
/// del fornitore (sono due esclusioni distinte e non si accorciano a vicenda).
pub fn residuo_della_chiave(chiave: &ChiaveCooldown) -> u64 {
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    let now = std::time::Instant::now();
    store
        .lock()
        .ok()
        .and_then(|map| map.get(chiave).copied())
        .filter(|until| *until > now)
        .map(|until| (until - now).as_secs())
        .unwrap_or(0)
}

fn scaduto_o_attivo(chiave: &ChiaveCooldown) -> bool {
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(map) = store.lock() {
        if let Some(&until) = map.get(chiave) {
            return std::time::Instant::now() < until;
        }
    }
    false
}

/// Severita' REGISTRATA del cooldown di un provider: non una deduzione dal
/// testo della `reason`, ma il fatto di QUALE delle due funzioni lo ha messo in
/// cooldown ([`put_provider_in_long_cooldown`] vs [`put_provider_in_short_cooldown`]).
/// E' gia' la classificazione giusta per costruzione: le due funzioni sono
/// chiamate esclusivamente dai rami billing/auth/budget (Long) e dai rami
/// transient (Short) — chi chiama sa gia' quale dei due sta invocando.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CooldownSeverity {
    /// Persistente (billing/quota/credito/auth/budget): il recovery e' del
    /// loop dedicato, mai del probe periodico generico.
    Long,
    /// Transiente (rate-limit/5xx/rete): il probe periodico lo ripinga.
    Short,
}

/// Registro della severita' di ogni cooldown attivo, in parallelo a
/// `PROVIDER_COOLDOWN_REASONS`. Sostituisce la vecchia `reason_is_billing`, che
/// indovinava "e' billing?" da 4 sottostringhe INGLESI sulla `reason` — un campo
/// che i produttori riempiono liberamente, anche in italiano (`"API key non
/// valida"`, `"budget_exhausted"`): nessuna delle due matchava, e quei provider
/// restavano fuori dalla protezione del loop dedicato, martellati dal probe
/// periodico generico ogni ~5 minuti (la stessa classe di incidente gia' fissata
/// per gli altri billing, Beauty-Book, ma non per questi due). Qui la domanda
/// "e' un cooldown lungo?" ha gia' una risposta certa: quale funzione lo ha
/// creato, non cosa dice il messaggio.
/// Chiavato come le scadenze e i motivi: la severita' di un tetto su un modello
/// e' del tetto, non del fornitore. Finche' questa mappa conosceva la sola forma
/// «fornitore», un `Short` su una coppia sovrascriveva il `Long` dell'account, e
/// il probe periodico — che da qui decide se il credito e' ancora KO — avrebbe
/// tolto un cooldown billing in anticipo.
static PROVIDER_COOLDOWN_SEVERITY: OnceLock<Mutex<HashMap<ChiaveCooldown, CooldownSeverity>>> =
    OnceLock::new();

fn set_cooldown_severity(chiave: &ChiaveCooldown, severity: CooldownSeverity) {
    let store = PROVIDER_COOLDOWN_SEVERITY.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        map.insert(chiave.clone(), severity);
    }
}

/// True se il provider e' ATTUALMENTE in cooldown e la severita' REGISTRATA e'
/// `Long` (billing/quota/credito/auth/budget esaurito). Il re-probe dei provider
/// in cooldown lungo e' gestito ESCLUSIVAMENTE dal loop dedicato
/// `billing_cooldown_recovery_loop` (re-probe a `BILLING_REPROBE_INTERVAL_S`):
/// il probe periodico generico (`run_one_round`) li salta, cosi' non rinnova il
/// cooldown ne' martella il gateway con 500 a cascata (incidente Beauty-Book).
/// I cooldown short restano invece pingati dal probe periodico per il recovery
/// rapido. Assenza di severita' registrata (mai dovrebbe capitare quando il
/// cooldown esiste) -> `false`, conservativo: meglio un probe di troppo che un
/// billing mai ripescato.
pub fn is_provider_in_billing_cooldown(provider: &str) -> bool {
    if !is_provider_in_cooldown(provider) {
        return false;
    }
    let key = ChiaveCooldown::fornitore(provider);
    PROVIDER_COOLDOWN_SEVERITY
        .get()
        .and_then(|s| s.lock().ok())
        .and_then(|m| m.get(&key).copied())
        .map(|s| s == CooldownSeverity::Long)
        .unwrap_or(false)
}

/// Registra un fallimento per il provider e restituisce true se la soglia
/// del circuit breaker e' stata superata (3+ fallimenti in 60s).
fn record_provider_failure(provider: &str) -> bool {
    let store = PROVIDER_FAILURES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        let t = provider_health_timings();
        let now = std::time::Instant::now();
        let window = std::time::Duration::from_secs(t.circuit_breaker_window_s);
        let entry = map.entry(provider.to_lowercase()).or_insert_with(Vec::new);
        entry.retain(|&ts| now.duration_since(ts) < window);
        entry.push(now);
        entry.len() >= t.circuit_breaker_threshold
    } else {
        false
    }
}

/// Reset del contatore fallimenti (chiamare su successo = stato CLOSED).

pub(crate) fn reset_provider_failures(provider: &str) {
    let store = PROVIDER_FAILURES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        map.remove(&provider.to_lowercase());
    }
}

/// Allinea il registro LOCALE all'esclusione che il gateway ha appena
/// dichiarato sul wire (`details.failures[0]`).
///
/// ## Perche' esiste
///
/// La domanda «questo fornitore e' utilizzabile adesso?» aveva due risposte in
/// due processi, e chi SCEGLIE consultava quella cieca: la selezione dei
/// modelli esclude i fornitori di QUESTO registro
/// ([`crate::orchestrator::model_selection::esclusioni_selezione`]), che pero'
/// imparava solo dal probe periodico e dal pannello provider — mai dai rifiuti
/// che il gateway comunica a ogni chiamata. Il segnale attraversava gia' il
/// confine tipizzato e un consumatore lo leggeva gia' per il failover
/// ([`crate::agent_graph_adapter::llm_gateway`]): mancava chi ne traesse la
/// conseguenza sul registro.
///
/// MISURATO il 12/08/2026 sul gate duale: tre validatori convocati — `openai`,
/// `kimi`, `openrouter` — e tutte e tre le astensioni con causa `cooldown`. Il
/// gateway li stava rifiutando senza chiamarli, mentre la selezione li aveva
/// appena scelti come i migliori disponibili.
///
/// ## Le due portate, e perche' la durata la decide chi guarisce
///
/// - [`EsclusioneDichiarata::Credito`] delega a
///   [`put_provider_in_long_cooldown`], che e' il punto unico del credito:
///   registra la severita' `Long`, persiste su Redis e mette il TTL su
///   `nexus_provider_health`, cosi' il `billing_cooldown_recovery_loop` (probe
///   -then-reenable) puo' liberarlo appena il credito torna. Il residuo
///   dichiarato dal gateway NON viene usato come durata: il credito non scade
///   col timer di un altro processo, e chi lo libera qui e' un ciclo che lo
///   VERIFICA.
/// - [`EsclusioneDichiarata::Attesa`] delega a [`metti_in_cooldown_breve`] con i
///   secondi dichiarati, e con la PORTATA che il gateway ha dichiarato: li' la
///   durata E' l'informazione (viene dal `Retry-After` del fornitore o dal
///   cooldown che il gateway ha appena impostato, e ricalcolarla la
///   inventerebbe), e dal 13/08/2026 lo e' anche CHI resta fuori — un tetto di
///   frequenza e' del modello, e registrarlo sul fornitore toglierebbe dalla
///   selezione modelli che hanno quota propria.
///
/// Nulla si registra su [`EsclusioneDichiarata::Nessuna`]: un errore di
/// richiesta, un contesto troppo lungo o un turno vuoto non dicono nulla sulla
/// disponibilita' del fornitore, ed escluderlo sarebbe un danno — la stessa
/// asimmetria che `classify_provider_error` applica gia' al ripiego lessicale.
pub fn registra_esclusione_dichiarata(esclusione: &EsclusioneDichiarata) {
    match esclusione {
        EsclusioneDichiarata::Credito { provider } => {
            // Non si rinnova un cooldown lungo gia' attivo: ogni chiamata
            // rifiutata ne allungherebbe la scadenza, e un fornitore ricaricato
            // aspetterebbe l'ultimo rifiuto invece del primo.
            if !is_provider_in_cooldown(provider) {
                put_provider_in_long_cooldown(provider, "gateway: fornitore senza credito");
                tracing::warn!(
                    target: "provider_cooldown",
                    provider = %provider,
                    "esclusione dichiarata dal gateway: credito, registrata nel registro locale"
                );
            }
        }
        EsclusioneDichiarata::Attesa {
            provider,
            model,
            secondi,
        } => {
            // ALLINEARE, MAI ACCORCIARE. `metti_in_cooldown_breve` fa un insert
            // incondizionato e registra severita' `Short`: senza questa guardia
            // un'attesa da 30 secondi SOSTITUIREBBE un cooldown di credito da 6
            // ore, e con la severita' cadrebbe anche
            // `is_provider_in_billing_cooldown` — cioe' il probe periodico
            // tornerebbe a interrogare un fornitore senza credito, che e' la
            // protezione documentata poco sopra. Il percorso non e' teorico:
            // basta una chiamata pinnata su quel fornitore (il probe stesso ne
            // fa) perche' il gateway dichiari un'attesa breve.
            //
            // Il verso e' quello sicuro: un'attesa piu' LUNGA di quella nota
            // aggiorna, una piu' corta si ignora. L'errore cade sul tenere
            // fuori qualcuno un po' piu' del necessario, mai sul rimettere in
            // gioco un fornitore che non puo' servire.
            //
            // Il residuo da confrontare e' quello della STESSA chiave, e ora e'
            // il TIPO a dirlo: `ChiaveCooldown::nuova(provider, model)` e' la
            // stessa chiave che `metti_in_cooldown_breve` scrivera' subito
            // sotto, quindi il confronto non puo' finire sull'esclusione
            // sbagliata (un'attesa sulla coppia contro il cooldown del
            // fornitore, che sono due esclusioni distinte).
            let model = model.as_deref();
            let chiave = ChiaveCooldown::nuova(provider, model);
            let residuo_noto = residuo_della_chiave(&chiave);
            if *secondi <= residuo_noto {
                tracing::debug!(
                    target: "provider_cooldown",
                    provider = %provider,
                    model,
                    attesa_s = *secondi,
                    residuo_noto,
                    "esclusione dichiarata dal gateway: gia' escluso piu' a lungo, non si accorcia"
                );
                return;
            }
            // La PORTATA e' quella che il gateway ha dichiarato: un tetto di
            // frequenza e' del modello, e registrarlo sul fornitore toglierebbe
            // dalla selezione anche i modelli che hanno quota propria (difetto
            // misurato il 13/08/2026 su groq).
            metti_in_cooldown_breve(
                provider,
                model,
                "gateway: attesa dichiarata dal fornitore",
                *secondi,
            );
            tracing::warn!(
                target: "provider_cooldown",
                provider = %provider,
                model,
                attesa_s = *secondi,
                "esclusione dichiarata dal gateway: la selezione smette di convocarlo per questo tempo"
            );
        }
        EsclusioneDichiarata::Nessuna => {}
    }
}

/// Rimuove completamente il cooldown e il contatore failures per un provider.
/// Usato dall'endpoint admin per forzare il rientro in servizio di un provider.
///
/// PORTATA della rimozione: TUTTO cio' che escludeva quel fornitore, comprese
/// le sue coppie col modello. «Rimetti groq in servizio» non puo' lasciare in
/// piedi il tetto su `groq/openai/gpt-oss-20b`: l'admin ha chiesto il fornitore,
/// e un residuo per modello renderebbe l'azione vera a meta' senza dirlo.
pub fn remove_cooldown(provider: &str) {
    let key = provider.trim().to_lowercase();
    // Rimuovi cooldown timer (fornitore + ogni sua coppia)
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        map.retain(|c, _| c.provider() != key);
    }
    // Rimuovi contatore failures (circuit breaker)
    reset_provider_failures(provider);
    // Rimuovi reason
    let reasons = PROVIDER_COOLDOWN_REASONS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = reasons.lock() {
        map.retain(|c, _| c.provider() != key);
    }
    // Rimuovi severita' registrata (fornitore + ogni sua coppia)
    let severities = PROVIDER_COOLDOWN_SEVERITY.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = severities.lock() {
        map.retain(|c, _| c.provider() != key);
    }
    // Rimuovi da Redis (se persistito)
    if let Some(conn) = REDIS_CLIENT.get() {
        let mut conn = conn.clone();
        let redis_key = format!("nexus:billing_cooldown:{}", key);
        tokio::spawn(async move {
            let _: Result<(), _> = redis::cmd("DEL")
                .arg(&redis_key)
                .query_async(&mut conn)
                .await;
        });
    }
    // Azzera il TTL persistente su nexus_provider_health (fonte GIUSTA del
    // billing cooldown: ha scadenza). Cosi' un restart non lo ripristina.
    if let Some(pool) = DB_POOL.get() {
        let pool = pool.clone();
        let key = key.clone();
        tokio::spawn(async move {
            if let Err(e) = sqlx::query(
                "UPDATE nexus_provider_health \
                 SET billing_cooldown_until = NULL, updated_at = NOW() \
                 WHERE LOWER(provider) = $1 AND billing_cooldown_until IS NOT NULL",
            )
            .bind(&key)
            .execute(&pool)
            .await
            {
                tracing::warn!(
                    "remove_cooldown: clear TTL nexus_provider_health fallito per '{}': {}",
                    key,
                    e
                );
            }
        });
    }
    tracing::info!(
        "Provider '{}' cooldown rimosso manualmente (admin)",
        provider
    );
}

/// Mette un provider in cooldown. Se `retry_after_seconds` e' fornito dal
/// provider (header Retry-After), lo usa con un cap a [10s, 3600s]. Altrimenti
/// default 300s. Se il circuit breaker scatta, cooldown esteso a 600s.
pub(crate) fn put_provider_in_cooldown(provider: &str, retry_after_seconds: Option<u64>) {
    let t = provider_health_timings();
    let breaker_tripped = record_provider_failure(provider);
    let base_secs = retry_after_seconds
        .map(|s| s.clamp(t.cooldown_min_s, t.cooldown_max_s))
        .unwrap_or(t.cooldown_default_s);
    let secs = if breaker_tripped {
        base_secs.max(t.circuit_breaker_extended_cooldown_s)
    } else {
        base_secs
    };
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        if breaker_tripped {
            tracing::warn!(
                "Provider '{}' circuit breaker OPEN: cooldown esteso {}s (>= {} fallimenti in {}s)",
                provider,
                secs,
                t.circuit_breaker_threshold,
                t.circuit_breaker_window_s
            );
        } else {
            tracing::warn!(
                "Provider '{}' in cooldown per {}s (retry_after={:?})",
                provider,
                secs,
                retry_after_seconds
            );
        }
        map.insert(ChiaveCooldown::fornitore(provider), until);
    }
}

/// Connessione Redis globale per persistere il cooldown lungo (sopravvive
/// al riavvio di mcp-core). Inizializzata da `main.rs::main` dopo init_redis.
static REDIS_CLIENT: OnceLock<redis::aio::MultiplexedConnection> = OnceLock::new();

/// Inizializza il client Redis globale per la persistenza dei cooldown
/// lunghi. Chiamato una volta da `main.rs` dopo `cache::init_redis`.
/// Il client e' clonabile (è un Arc internamente) — ne salviamo un clone.
pub fn init_redis_client(client: redis::aio::MultiplexedConnection) {
    let _ = REDIS_CLIENT.set(client);
}

/// Pool DB globale per la persistenza TTL del billing cooldown su
/// `nexus_provider_health` (la fonte persistente GIUSTA: ha scadenza, non
/// disabilita il catalog). Inizializzato da `main.rs` all'avvio.
static DB_POOL: OnceLock<sqlx::PgPool> = OnceLock::new();

/// Espone il pool DB a `provider_cooldown` per l'UPSERT/clear del TTL
/// billing su `nexus_provider_health`. Idempotente (OnceLock).
pub fn init_db_pool(pool: sqlx::PgPool) {
    let _ = DB_POOL.set(pool);
}

/// Worker periodico: ogni `interval_secs` controlla i provider che hanno righe
/// ancora disabilitate dal billing cooldown e, se il cooldown locale e' scaduto,
/// li riabilita nel DB **solo dopo un probe attivo andato a buon fine**
/// (probe-then-reenable).
///
/// Prima del fix, la riabilitazione avveniva alla cieca allo scadere del timer:
/// se il billing non era stato ricaricato, il provider tornava attivo, veniva
/// scelto per un run reale, falliva di nuovo e rientrava in cooldown (ciclo).
/// Ora, allo scadere del cooldown:
///   - probe Healthy   -> riabilita (catalog + matrix).
///   - probe Billing    -> il credito e' ancora KO: rinnova il cooldown lungo,
///                         niente riabilitazione.
///   - probe Transient  -> errore non conclusivo (rate-limit/timeout/rete):
///                         applica un cooldown breve e riprova al prossimo giro.
///
/// `interval_secs` e il timeout del probe sono DB-driven (settings `provider.*`,
/// migrazione 0252) — vedi `provider_health_timings()`.
pub async fn billing_cooldown_recovery_loop(
    orchestrator: Arc<crate::orchestrator::Orchestrator>,
    db: sqlx::PgPool,
    interval_secs: u64,
) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(5)));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await; // skip immediate

    loop {
        ticker.tick().await;

        let providers_disabled: Vec<String> = match sqlx::query_scalar::<_, String>(
            "SELECT LOWER(provider) FROM nexus_provider_health \
             WHERE billing_cooldown_until IS NOT NULL",
        )
        .fetch_all(&db)
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("billing_cooldown_recovery: query fallita: {}", e);
                continue;
            }
        };

        for provider in providers_disabled {
            // Cooldown ancora attivo: di norma si aspetta la scadenza. MA per i
            // billing-cooldown (6h) la ricarica del credito e' un evento esterno
            // imprevedibile: ri-proviamo a intervalli regolari (BILLING_REPROBE_
            // INTERVAL_S) invece di tenere il provider giu' per ore dopo che
            // l'utente ha gia' ricaricato.
            if is_provider_in_cooldown(&provider)
                && !should_reprobe_cooldown(
                    &provider,
                    std::time::Duration::from_secs(BILLING_REPROBE_INTERVAL_S),
                )
            {
                continue;
            }
            // Probe-then-reenable: il cooldown e' scaduto (o e' ora di ri-provare)
            // ma prima di riabilitare accertiamo che il provider sia DAVVERO
            // tornato operativo.
            let probe_timeout = provider_health_timings().recovery_probe_timeout_s;
            match crate::provider_health_probe::probe_provider_once(
                &orchestrator,
                &provider,
                probe_timeout,
            )
            .await
            {
                crate::provider_health_probe::ProbeOutcome::Healthy => {
                    tracing::info!(
                        target: "provider_cooldown",
                        provider = %provider,
                        "probe-then-reenable: provider sano, esco dal cooldown e riabilito nel DB"
                    );
                    // Esce SUBITO dal cooldown (anche se non ancora scaduto): il
                    // re-probe periodico ha rilevato che il credito e' tornato.
                    // remove_cooldown azzera anche il TTL su nexus_provider_health.
                    remove_cooldown(&provider);
                }
                crate::provider_health_probe::ProbeOutcome::Billing(kind) => {
                    tracing::warn!(
                        target: "provider_cooldown",
                        provider = %provider,
                        kind = %kind,
                        "probe-then-reenable: billing ancora KO, rinnovo cooldown lungo (niente riabilitazione)"
                    );
                    put_provider_in_long_cooldown(&provider, &kind);
                }
                crate::provider_health_probe::ProbeOutcome::Transient(kind) => {
                    let slow = provider_health_timings().slow_cooldown_s;
                    tracing::warn!(
                        target: "provider_cooldown",
                        provider = %provider,
                        kind = %kind,
                        "probe-then-reenable: esito non conclusivo, cooldown breve e nuovo tentativo al prossimo giro"
                    );
                    put_provider_in_short_cooldown(&provider, &kind, slow);
                }
            }
        }
    }
}

/// Variante "lunga" di `put_provider_in_cooldown` per errori semantici tipo
/// "credit balance too low" / "quota exceeded" che non si risolvono in pochi
/// minuti (servono soldi/giorni). Bypassa il circuit breaker.
///
/// Persistenza: oltre allo store in-memory, scrive anche su Redis con TTL
/// pari al cooldown — cosi' al riavvio mcp-core ricarica il cooldown via
/// `restore_cooldown` invece di partire pulito (quel restart era il bug
/// "LED openai verde" segnalato dall'utente).
pub fn put_provider_in_long_cooldown(provider: &str, reason: &str) {
    // TTL adattivo per tipo d'errore (governance, opt-in): con flag OFF ritorna
    // `cooldown_long_s` (6h, bit-identico). Il re-probe periodico recupera comunque
    // in anticipo; il TTL e' solo il limite superiore.
    let long_secs = adaptive_billing_cooldown_secs(reason, &provider_health_timings());
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(long_secs);
        map.insert(ChiaveCooldown::fornitore(provider), until);
        tracing::warn!(
            "Provider '{}' in COOLDOWN LUNGO ({}s, {} ore) per: {}",
            provider,
            long_secs,
            long_secs / 3600,
            reason,
        );
    }
    // Salva anche il motivo nel registro motivazioni
    let reasons = PROVIDER_COOLDOWN_REASONS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = reasons.lock() {
        map.insert(ChiaveCooldown::fornitore(provider), reason.to_string());
    }
    set_cooldown_severity(&ChiaveCooldown::fornitore(provider), CooldownSeverity::Long);
    // Persistenza Redis fire-and-forget. Stesso schema usato da
    // `gateway_providers_handler` (chiave `nexus:billing_cooldown:<provider>`)
    // cosi' il restore al riavvio funziona uniformemente.
    if let Some(conn) = REDIS_CLIENT.get() {
        let provider = provider.to_lowercase();
        let reason = reason.to_string();
        let mut conn = conn.clone();
        tracing::info!(
            "put_provider_in_long_cooldown: avvio persistenza Redis per '{}'",
            provider,
        );
        tokio::spawn(async move {
            let now_ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let until_ts = now_ts.saturating_add(long_secs);
            let key = format!("nexus:billing_cooldown:{}", provider);
            let value = format!("{}|{}", until_ts, reason);
            let res: Result<(), _> = redis::cmd("SET")
                .arg(&key)
                .arg(&value)
                .arg("EX")
                .arg(long_secs + 60)
                .query_async(&mut conn)
                .await;
            match &res {
                Ok(()) => tracing::info!(
                    "put_provider_in_long_cooldown: Redis SET ok per '{}' (chiave={})",
                    provider,
                    key,
                ),
                Err(e) => tracing::warn!(
                    "put_provider_in_long_cooldown: persistenza Redis fallita per '{}': {}",
                    provider,
                    e,
                ),
            }
        });
    } else {
        tracing::warn!(
            "put_provider_in_long_cooldown: REDIS_CLIENT non inizializzato, cooldown solo in-memory per '{}'",
            provider,
        );
    }
    // Persistenza TTL su nexus_provider_health (fonte PERSISTENTE GIUSTA del
    // billing cooldown: ha scadenza, non disabilita il catalog). E' la parte
    // BUONA spostata qui dall'ex propagate_billing_disable_to_db: writer unico
    // Rust (regola L/ADR 0020), letta al boot da restore_billing_cooldowns_from_db.
    // TTL = cooldown lungo DB-driven (cooldown_long_s, gia' in long_secs).
    if let Some(pool) = DB_POOL.get() {
        let pool = pool.clone();
        let provider = provider.to_lowercase();
        let reason = reason.to_string();
        let ttl_secs = (long_secs as i64).to_string();
        tokio::spawn(async move {
            if let Err(e) = sqlx::query(
                "INSERT INTO nexus_provider_health \
                   (provider, billing_cooldown_until, last_error, last_error_at, last_error_source, updated_at) \
                 VALUES ($1, NOW() + ($2 || ' seconds')::interval, $3, NOW(), 'mcp-core', NOW()) \
                 ON CONFLICT (provider) DO UPDATE SET \
                   billing_cooldown_until = EXCLUDED.billing_cooldown_until, \
                   last_error = EXCLUDED.last_error, last_error_at = NOW(), \
                   last_error_source = 'mcp-core', updated_at = NOW()",
            )
            .bind(&provider)
            .bind(&ttl_secs)
            .bind(&reason)
            .execute(&pool)
            .await
            {
                tracing::warn!(
                    "put_provider_in_long_cooldown: UPSERT TTL nexus_provider_health fallito per '{}': {}",
                    provider,
                    e
                );
            }
        });
    }
}

/// Cooldown breve (default 60s) per errori transient (5xx, rate limit short window).
/// Diverso dal long cooldown (6h) usato per billing/quota esaurita: qui il provider
/// si presume tornera' funzionante in pochi secondi/minuti, quindi non vale la pena
/// escluderlo per ore. Solo in-memory: non persistito su Redis perche' il valore di
/// 60s e' minore del tempo medio di restart del processo.
pub fn put_provider_in_short_cooldown(provider: &str, reason: &str, duration_secs: u64) {
    metti_in_cooldown_breve(provider, None, reason, duration_secs);
}

/// Cooldown breve sulla COPPIA fornitore+modello: la forma giusta per un rate
/// limit, che e' un tetto del modello e non dell'account.
///
/// `model: None` ricade sul fornitore intero, ed e' il caso di chi non sa quale
/// modello fosse in gioco — resta possibile, ma va chiamato sapendo che esclude
/// tutto. Il chiamante che il modello lo conosce deve passarlo: un 429 su
/// `gemini-2.5-pro` non ha nulla da dire su `gemini-2.5-flash`.
pub fn metti_in_cooldown_breve(
    provider: &str,
    model: Option<&str>,
    reason: &str,
    duration_secs: u64,
) {
    let chiave = ChiaveCooldown::nuova(provider, model);
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(duration_secs);
        map.insert(chiave.clone(), until);
        match model {
            Some(m) => tracing::warn!(
                "Modello '{}/{}' in COOLDOWN BREVE ({}s) per: {} (gli altri modelli di '{}' restano disponibili)",
                provider, m, duration_secs, reason, provider
            ),
            None => tracing::warn!(
                "Provider '{}' in COOLDOWN BREVE ({}s) per: {}",
                provider, duration_secs, reason
            ),
        }
    }
    let reasons = PROVIDER_COOLDOWN_REASONS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = reasons.lock() {
        map.insert(chiave.clone(), reason.to_string());
    }
    // La severita' si registra sulla STESSA chiave della scadenza. Il recovery
    // loop ragiona per account e interroga la chiave del FORNITORE: un cooldown
    // di modello non cambia la natura del fornitore, e scrivendolo sulla chiave
    // del fornitore la cambierebbe — degradando a `Short` un `Long` di credito.
    set_cooldown_severity(&chiave, CooldownSeverity::Short);
}

/// Registro dei motivi di cooldown ("credit balance too low", "rate limit", …).
/// Esposto al frontend via [`cooldown_snapshot_entries`] → reason mostrato nel
/// LED tooltip. Chiavato come le scadenze: il motivo di un tetto su un modello
/// non e' il motivo del fornitore.
static PROVIDER_COOLDOWN_REASONS: OnceLock<Mutex<HashMap<ChiaveCooldown, String>>> =
    OnceLock::new();

/// Una riga dello snapshot dei cooldown attivi.
///
/// La `reason` e' testo per l'umano (display, log, tooltip): non e' un criterio.
/// Cio' su cui si DECIDE e' [`CooldownEntry::severity`], la severita' REGISTRATA
/// da chi il cooldown lo ha messo. Prima lo snapshot portava solo la terna
/// `(provider, secondi, reason)` e ogni consumatore che avesse bisogno di sapere
/// "e' un cooldown di credito?" era COSTRETTO a ri-classificare quel testo con le
/// stesse 4 sottostringhe inglesi (`credit`/`quota`/`billing`/`balance`) che
/// questo modulo aveva gia' sostituito con un registro tipizzato: la
/// classificazione esisteva, ma non usciva dalla porta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CooldownEntry {
    /// CHI e' escluso: il fornitore, e l'eventuale modello. Non e' una stringa
    /// (vedi [`ChiaveCooldown`]): il campo `provider` che c'era qui portava la
    /// chiave GREZZA, e i consumatori la usavano come nome di fornitore.
    pub chiave: ChiaveCooldown,
    pub remaining_seconds: u64,
    /// Motivo per l'umano. Riempito liberamente dai produttori, anche in
    /// italiano: mai un segnale su cui decidere (regola M/Q).
    pub reason: Option<String>,
    /// `None` = severita' NON registrata. E' una variante, non un ripiego
    /// comodo: significa che il cooldown esiste ma nessuna delle funzioni che
    /// lo classificano lo ha creato, e chi legge deve trattarlo come ignoto,
    /// non come transiente.
    pub severity: Option<CooldownSeverity>,
}

/// Snapshot di TUTTI i cooldown attivi — fornitori interi e coppie col modello —
/// con la severita' REGISTRATA. Lettore autoritativo dei tre registri (scadenze,
/// motivi, severita'), ordinato per chiave cosi' che due letture della stessa
/// situazione diano la stessa lista.
///
/// E' l'unica porta d'uscita: la proiezione a tre campi che c'era qui accanto
/// (`cooldown_snapshot() -> Vec<(String, u64, Option<String>)>`) appiattiva la
/// chiave in un `String` chiamato `provider`, ed e' il difetto D1 — nove
/// consumatori la leggevano come nome di fornitore. Chi ha bisogno dei soli
/// fornitori interi chiama [`cooldown_fornitori_entries`] o
/// [`fornitori_in_cooldown`]; chi ha bisogno delle coppie chiama
/// [`coppie_in_cooldown`]. Nessuna delle tre e' interscambiabile con le altre.
pub fn cooldown_snapshot_entries() -> Vec<CooldownEntry> {
    let store = match PROVIDER_COOLDOWN.get() {
        Some(s) => s,
        None => return Vec::new(),
    };
    let map = match store.lock() {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    let reasons = PROVIDER_COOLDOWN_REASONS.get().and_then(|s| s.lock().ok());
    let severities = PROVIDER_COOLDOWN_SEVERITY.get().and_then(|s| s.lock().ok());
    let now = std::time::Instant::now();
    let mut out = Vec::new();
    for (chiave, &until) in map.iter() {
        if until > now {
            out.push(CooldownEntry {
                // La severita' e' quella REGISTRATA per QUESTA chiave: un tetto
                // su un modello ha la sua, e non eredita quella dell'account.
                severity: severities.as_ref().and_then(|s| s.get(chiave).copied()),
                reason: reasons.as_ref().and_then(|r| r.get(chiave).cloned()),
                remaining_seconds: (until - now).as_secs().max(1),
                chiave: chiave.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.chiave.cmp(&b.chiave));
    out
}

/// «Quali FORNITORI sono esclusi PER INTERO adesso?»
///
/// Le sole voci con portata [`PortataCooldown::Provider`]. Un tetto su un
/// modello NON entra qui, ed e' il punto: rispondere di si' per tutto groq
/// perche' `groq/openai/gpt-oss-20b` ha sforato e' esattamente il difetto che la
/// portata per coppia ha chiuso il 07/08/2026 — riproporlo a valle, nel lettore,
/// lo rimetterebbe in piedi.
pub fn cooldown_fornitori_entries() -> Vec<CooldownEntry> {
    cooldown_snapshot_entries()
        .into_iter()
        .filter(|e| e.chiave.esclude_il_fornitore())
        .collect()
}

/// I soli NOMI dei fornitori esclusi per intero, lowercase, ordinati e senza
/// duplicati. E' la domanda di chi filtra per fornitore (una WHERE su
/// `provider`, un elenco per un messaggio di fail-fast, un LED).
pub fn fornitori_in_cooldown() -> Vec<String> {
    let mut out: Vec<String> = cooldown_fornitori_entries()
        .into_iter()
        .map(|e| e.chiave.provider().to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// «Quali COPPIE (fornitore, modello) sono escluse adesso?»
///
/// Solo le coppie: un fornitore escluso per intero non compare qui, perche' la
/// sua esclusione e' gia' completa e non elencabile modello per modello (i
/// modelli di un fornitore non li conosce questo modulo). Chi filtra deve
/// applicare ENTRAMBE le liste — [`fornitori_in_cooldown`] e questa.
pub fn coppie_in_cooldown() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = cooldown_snapshot_entries()
        .into_iter()
        .filter_map(|e| {
            e.chiave
                .model()
                .map(|m| (e.chiave.provider().to_string(), m.to_string()))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Ripristina un cooldown (billing) da un timestamp letto da Redis dopo riavvio.
/// `remaining_secs`: secondi rimasti al momento della lettura. La chiave Redis
/// `nexus:billing_cooldown:*` che alimenta questa funzione e' scritta ESCLUSIVAMENTE
/// da [`put_provider_in_long_cooldown`] (vedi la sua persistenza): un cooldown
/// ripristinato da qui e' sempre `Long` per costruzione, mai dedotto dalla reason.
pub fn restore_cooldown(provider: &str, remaining_secs: u64, reason: &str) {
    let store = PROVIDER_COOLDOWN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = store.lock() {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(remaining_secs);
        map.insert(ChiaveCooldown::fornitore(provider), until);
        tracing::info!(
            "Provider '{}' cooldown ripristinato da Redis: {}s rimanenti, motivo: {}",
            provider,
            remaining_secs,
            reason
        );
    }
    let reasons = PROVIDER_COOLDOWN_REASONS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = reasons.lock() {
        map.insert(ChiaveCooldown::fornitore(provider), reason.to_string());
    }
    set_cooldown_severity(&ChiaveCooldown::fornitore(provider), CooldownSeverity::Long);
}

/// Bootstrap del cooldown billing dal DB persistente al riavvio (ADR 0020).
///
/// Principio: il polling di health e' l'UNICO che testa i provider; un run di
/// produzione deve CONSULTARE lo stato gia' accertato, non scoprirlo chiamando
/// il provider. Lo store del gate e' pero' in-memory e si azzera ad ogni restart
/// di mcp-core. Il `restore_cooldown` da Redis (main.rs) copre il caso comune ma
/// fallisce se Redis e' stato svuotato/riavviato. `nexus_provider_health`
/// (scritta dal brain via cooldown_bridge e dal gate) e' la fonte PERSISTENTE
/// piu' affidabile: la leggiamo al boot e rimettiamo i provider esausti in
/// cooldown lungo, cosi' il PRIMO run dopo un restart li salta senza ri-testarli
/// (era la causa del loop "anthropic 400 / openai 429 ad ogni turno").
///
/// I provider il cui credito e' stato nel frattempo ricaricato vengono riabilitati
/// dal `billing_cooldown_recovery_loop` (probe-then-reenable) al primo giro.
pub async fn restore_billing_cooldowns_from_db(db: &sqlx::PgPool) {
    let rows = match sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT LOWER(provider), last_error \
         FROM nexus_provider_health \
         WHERE billing_cooldown_until IS NOT NULL AND billing_cooldown_until > NOW()",
    )
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("restore_billing_cooldowns_from_db: query fallita: {e}");
            return;
        }
    };
    let n = rows.len();
    for (provider, reason) in rows {
        // Billing/quota e' persistente -> cooldown lungo (6h). Il recovery loop
        // (probe-then-reenable) lo rimuovera' appena il provider torna 200.
        put_provider_in_long_cooldown(
            &provider,
            reason
                .as_deref()
                .unwrap_or("billing_cooldown (ripristino DB al boot)"),
        );
    }
    if n > 0 {
        tracing::info!(
            "restore_billing_cooldowns_from_db: {n} provider in cooldown billing ripristinati dal DB al boot (gate allineato allo stato persistente)"
        );
    }
}

// =====================================================================
// TEST SCALABILITA' COOLDOWN PROVIDER
// =====================================================================
// NOTA: provider_cooldown usa stato globale (OnceLock<Mutex<HashMap>>).
// I test usano nomi provider univoci con prefisso `__test_<funzione>_`
// per evitare interferenze quando eseguiti in parallelo.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_sconosciuto_non_e_in_cooldown() {
        // Stato iniziale pulito: un provider mai messo in cooldown
        // ritorna false. Garanzia base del sistema.
        assert!(!is_provider_in_cooldown("__test_unknown_xyzzy"));
    }

    #[test]
    fn short_cooldown_marca_provider_indisponibile() {
        let p = "__test_short_cooldown_provider";
        assert!(!is_provider_in_cooldown(p));
        put_provider_in_short_cooldown(p, "test rate limit", 60);
        assert!(
            is_provider_in_cooldown(p),
            "dopo short cooldown il provider deve essere in cooldown"
        );
    }

    #[test]
    fn long_cooldown_marca_provider_indisponibile_per_ore() {
        // Caso billing_error: cooldown deve durare almeno 6h (test sull'effetto,
        // non sulla durata esatta — verificarla richiederebbe sleep).
        let p = "__test_long_cooldown_billing";
        put_provider_in_long_cooldown(p, "credit balance too low");
        assert!(is_provider_in_cooldown(p));
        // Snapshot deve contenere il provider con remaining > 0
        let snap = cooldown_snapshot_entries();
        let found = snap.iter().find(|e| e.chiave.provider() == p);
        assert!(
            found.is_some(),
            "long cooldown deve apparire in cooldown_snapshot_entries"
        );
        let found = found.unwrap();
        assert!(
            found.chiave.esclude_il_fornitore(),
            "il credito e' dell'account: la portata e' il fornitore intero"
        );
        assert!(
            found.remaining_seconds > 5 * 3600,
            "long cooldown deve durare >= 5h, trovato {}s",
            found.remaining_seconds
        );
        assert_eq!(found.reason.as_deref(), Some("credit balance too low"));
    }

    #[test]
    fn cooldown_e_case_insensitive_sul_nome_provider() {
        // Defense in depth: put('OpenAI') deve coincidere con is_in_cooldown('openai').
        // Tutti i call site del codice usano lowercase, ma vogliamo essere robusti.
        let p_upper = "__TEST_CASE_INSENSITIVE";
        let p_lower = "__test_case_insensitive";
        put_provider_in_short_cooldown(p_upper, "test", 60);
        assert!(is_provider_in_cooldown(p_lower));
    }

    #[test]
    fn provider_diversi_hanno_cooldown_indipendenti() {
        // Caso pratico: openai in cooldown billing non deve impattare anthropic.
        let p_a = "__test_indep_alpha";
        let p_b = "__test_indep_beta";
        put_provider_in_short_cooldown(p_a, "rate limit", 60);
        assert!(is_provider_in_cooldown(p_a));
        assert!(!is_provider_in_cooldown(p_b));
    }

    #[test]
    fn restore_cooldown_da_redis_ripristina_stato() {
        // Simula riavvio mcp-core: il cooldown viene letto da Redis e
        // restore_cooldown lo ricarica in memoria.
        let p = "__test_restore_provider";
        assert!(!is_provider_in_cooldown(p));
        restore_cooldown(p, 3600, "billing_error from redis");
        assert!(is_provider_in_cooldown(p));
        let snap = cooldown_snapshot_entries();
        let entry = snap.iter().find(|e| e.chiave.provider() == p);
        assert!(entry.is_some());
        assert_eq!(
            entry.unwrap().reason.as_deref(),
            Some("billing_error from redis")
        );
    }

    #[test]
    fn budget_exhausted_e_api_key_non_valida_sono_riconosciuti_billing() {
        // Regressione del difetto reale: `reason_is_billing` cercava 4
        // sottostringhe INGLESI ("credit"/"quota"/"billing"/"balance") su un
        // campo che i produttori riempiono liberamente. Ne' "budget_exhausted"
        // (provider_health_probe.rs, budget mensile esaurito) ne' "API key non
        // valida" (model_health_probe.rs, auth_error in italiano) contenevano
        // una di quelle parole: quei provider restavano FUORI dalla protezione
        // del loop dedicato, martellati dal probe periodico ogni ~5 minuti
        // esattamente come nell'incidente Beauty-Book che quella protezione
        // doveva coprire. Ora la severita' e' REGISTRATA da chi chiama
        // (put_provider_in_long_cooldown), non indovinata dal testo: qualunque
        // reason, in qualunque lingua, e' billing se e' arrivata da li'.
        let p_budget = "__test_budget_exhausted_probe";
        let p_auth = "__test_auth_non_valida_probe";
        put_provider_in_long_cooldown(p_budget, "budget_exhausted");
        put_provider_in_long_cooldown(p_auth, "API key non valida");
        assert!(is_provider_in_billing_cooldown(p_budget));
        assert!(is_provider_in_billing_cooldown(p_auth));
        remove_cooldown(p_budget);
        remove_cooldown(p_auth);
    }

    #[test]
    fn billing_cooldown_distinto_da_transient() {
        // Il probe periodico salta i billing (gestiti dal recovery loop) ma
        // continua a pingare i transient. is_provider_in_billing_cooldown e'
        // il discriminante, e la severita' e' quella REGISTRATA da chi ha
        // messo il provider in cooldown (quale funzione ha chiamato), non una
        // deduzione dalla reason: entrambe le reason sotto sono deliberatamente
        // ambigue sul testo per provarlo.
        let p_bill = "__test_billing_cd_probe";
        let p_trans = "__test_transient_cd_probe";
        assert!(!is_provider_in_billing_cooldown(p_bill));
        put_provider_in_long_cooldown(p_bill, "causa qualsiasi");
        assert!(is_provider_in_billing_cooldown(p_bill));
        put_provider_in_short_cooldown(p_trans, "causa qualsiasi", 60);
        assert!(is_provider_in_cooldown(p_trans));
        assert!(!is_provider_in_billing_cooldown(p_trans));
        remove_cooldown(p_bill);
        remove_cooldown(p_trans);
        assert!(!is_provider_in_billing_cooldown(p_bill));
    }

    #[test]
    fn restore_cooldown_da_redis_e_sempre_billing() {
        // La chiave Redis che alimenta restore_cooldown e' scritta SOLO da
        // put_provider_in_long_cooldown: un ripristino da li' e' Long per
        // costruzione, indipendentemente dal testo della reason.
        let p = "__test_restore_e_sempre_long";
        restore_cooldown(p, 3600, "motivo qualsiasi senza marker billing");
        assert!(is_provider_in_billing_cooldown(p));
        remove_cooldown(p);
    }

    #[test]
    fn cooldown_snapshot_esclude_provider_non_in_cooldown() {
        // Nessun provider mai messo in cooldown → snapshot vuoto (al netto di
        // altri test paralleli). Verifichiamo solo che __test_snap_excluded
        // NON appaia perche' non e' mai stato messo in cooldown.
        let p_never = "__test_snap_never_in_cooldown";
        let snap = cooldown_snapshot_entries();
        let found = snap.iter().find(|e| e.chiave.provider() == p_never);
        assert!(
            found.is_none(),
            "provider mai messo in cooldown non deve apparire in snapshot"
        );
    }

    #[test]
    fn cap_retry_after_clampato_dentro_intervallo_valido() {
        // put_provider_in_cooldown clampa retry_after in [10, 3600].
        // Test sul comportamento (cooldown attivo) — non sulla durata esatta.
        let p_lo = "__test_clamp_low";
        let p_hi = "__test_clamp_high";
        // 5s sotto il minimo (10s): deve essere clampato a 10s, cooldown attivo
        put_provider_in_cooldown(p_lo, Some(5));
        assert!(is_provider_in_cooldown(p_lo));
        // 99999s sopra il massimo (3600s): clampato a 3600s, cooldown attivo
        put_provider_in_cooldown(p_hi, Some(99999));
        assert!(is_provider_in_cooldown(p_hi));
        // Verifica via snapshot che il cap superiore sia rispettato
        let snap = cooldown_snapshot_entries();
        let entry_hi = snap.iter().find(|e| e.chiave.provider() == p_hi);
        if let Some(e) = entry_hi {
            assert!(
                e.remaining_seconds <= 3600,
                "cap superiore violato: {}s > 3600s",
                e.remaining_seconds
            );
        }
    }

    #[test]
    fn timings_default_riflettono_i_valori_storici() {
        // I default DB-driven devono coincidere con le vecchie costanti
        // hardcoded, cosi' un setting mancante non cambia il comportamento.
        let t = ProviderHealthTimings::default();
        assert_eq!(t.cooldown_default_s, 300);
        assert_eq!(t.cooldown_min_s, 10);
        assert_eq!(t.cooldown_max_s, 3600);
        assert_eq!(t.cooldown_long_s, 6 * 3600);
        assert_eq!(t.circuit_breaker_window_s, 60);
        assert_eq!(t.circuit_breaker_threshold, 3);
        assert_eq!(t.circuit_breaker_extended_cooldown_s, 600);
        assert_eq!(t.health_probe_timeout_s, 30);
        assert_eq!(t.slow_cooldown_s, 60);
        assert_eq!(t.outage_threshold, 3);
        assert_eq!(t.billing_recovery_interval_s, 60);
        assert_eq!(t.recovery_probe_timeout_s, 30);
    }

    #[test]
    fn adaptive_ttl_off_ritorna_sempre_cooldown_lungo() {
        // Flag OFF (default): qualunque reason -> cooldown lungo pieno (bit-identico).
        let t = ProviderHealthTimings::default();
        assert!(!t.adaptive_billing_cooldown_enabled);
        assert_eq!(
            adaptive_billing_cooldown_secs("credit balance too low", &t),
            t.cooldown_long_s
        );
        assert_eq!(
            adaptive_billing_cooldown_secs("quota_exceeded", &t),
            t.cooldown_long_s
        );
    }

    #[test]
    fn adaptive_ttl_on_riduce_solo_quota_e_rate() {
        let t = ProviderHealthTimings {
            adaptive_billing_cooldown_enabled: true,
            adaptive_billing_cooldown_min_s: 2 * 3600,
            ..Default::default()
        };
        // Hard billing (credit/balance/payment): ricarica manuale -> nessuna riduzione.
        assert_eq!(
            adaptive_billing_cooldown_secs("credit balance too low", &t),
            t.cooldown_long_s
        );
        assert_eq!(
            adaptive_billing_cooldown_secs("payment required", &t),
            t.cooldown_long_s
        );
        // Quota/rate: recupero prevedibile -> TTL ridotto (2h).
        assert_eq!(
            adaptive_billing_cooldown_secs("quota_exceeded", &t),
            2 * 3600
        );
        assert_eq!(adaptive_billing_cooldown_secs("rate_limit", &t), 2 * 3600);
        // Altra causa: conservativo -> cooldown lungo pieno.
        assert_eq!(adaptive_billing_cooldown_secs("boh", &t), t.cooldown_long_s);
    }

    #[test]
    fn adaptive_ttl_min_clampato_nel_range() {
        let base = ProviderHealthTimings {
            adaptive_billing_cooldown_enabled: true,
            ..Default::default()
        };
        // Min sotto cooldown_min_s (10) -> clampato a cooldown_min_s.
        let t = ProviderHealthTimings {
            adaptive_billing_cooldown_min_s: 1,
            ..base
        };
        assert_eq!(
            adaptive_billing_cooldown_secs("quota", &t),
            t.cooldown_min_s
        );
        // Min sopra cooldown_long_s -> clampato a cooldown_long_s.
        let t2 = ProviderHealthTimings {
            adaptive_billing_cooldown_min_s: 99 * 3600,
            ..base
        };
        assert_eq!(
            adaptive_billing_cooldown_secs("quota", &t2),
            t2.cooldown_long_s
        );
    }

    #[test]
    fn provider_health_timings_ritorna_default_se_non_inizializzato() {
        // Senza init (caso test/avvio precoce) si usano i default, mai panico.
        let t = provider_health_timings();
        assert_eq!(t.cooldown_long_s, 6 * 3600);
    }

    #[test]
    fn reset_provider_failures_pulisce_contatore_circuit_breaker() {
        let p = "__test_reset_failures_cb";
        // Triggera il circuit breaker con 3 fallimenti
        for _ in 0..3 {
            put_provider_in_cooldown(p, Some(60));
        }
        assert!(is_provider_in_cooldown(p));
        // Reset del contatore: il prossimo put non scatena cooldown esteso
        reset_provider_failures(p);
        // Il cooldown attivo rimane finche' non scade naturalmente, ma il
        // contatore failures e' stato resettato (verifica indiretta: assenza panic)
    }
}

/// L'esclusione che il gateway DICHIARA deve arrivare al registro che la
/// selezione interroga davvero (`is_provider_in_cooldown` /
/// `is_model_in_cooldown`, lette da `esclusioni_selezione`).
#[cfg(test)]
mod tests_esclusione_dal_gateway {
    use super::*;

    /// La voce del FORNITORE nello snapshot. Si chiede per chiave completa e non
    /// per solo nome: dopo il fix D1 lo snapshot porta anche le coppie, e
    /// filtrare sul solo `provider()` prenderebbe la prima che capita.
    fn voce_del_fornitore(p: &str) -> Option<CooldownEntry> {
        cooldown_snapshot_entries()
            .into_iter()
            .find(|e| e.chiave == ChiaveCooldown::fornitore(p))
    }

    /// L'incidente del 12/08/2026 nella sua forma minima: il gateway dichiara
    /// un'attesa, e la selezione deve smettere di convocare quel fornitore.
    ///
    /// MUTAZIONE che la fa rosseggiare, col difetto reale: rendere
    /// `registra_esclusione_dichiarata` un no-op sul ramo `Attesa` -> il
    /// fornitore resta eleggibile, viene convocato di nuovo e si astiene di
    /// nuovo con causa `cooldown`, per tutta la durata che il gateway onora.
    #[test]
    fn un_attesa_dichiarata_dal_gateway_esclude_il_fornitore_dalla_selezione() {
        let p = "prova-attesa-dal-gateway";
        assert!(!is_provider_in_cooldown(p), "premessa: parte disponibile");

        registra_esclusione_dichiarata(&EsclusioneDichiarata::Attesa {
            provider: p.to_string(),
            model: None,
            secondi: 600,
        });

        assert!(
            is_provider_in_cooldown(p),
            "dichiarata l'attesa, la selezione non deve piu' convocarlo"
        );
        // Senza modello dichiarato la PORTATA e' il fornitore, come quella del
        // gateway: registrare la sola coppia renderebbe mcp-core piu' permissivo
        // di chi rifiuta.
        assert!(is_model_in_cooldown(p, "un-modello-qualsiasi"));

        remove_cooldown(p);
    }

    /// IL CASO groq del 13/08/2026, dal lato di chi SCEGLIE: il gateway dichiara
    /// che fuori e' il modello — «Rate limit reached for model
    /// `openai/gpt-oss-20b` ... TPD Limit 200000» — e mcp-core deve registrare
    /// la stessa portata, non una piu' larga.
    ///
    /// MUTAZIONE: rimettere `put_provider_in_short_cooldown(provider, ...)` (cioe'
    /// ignorare il modello) in `registra_esclusione_dichiarata` -> l'ultimo
    /// assert cade, e cade col difetto reale: il fornitore intero sparisce dalla
    /// selezione per l'attesa di un modello solo.
    #[test]
    fn un_attesa_di_modello_non_esclude_gli_altri_modelli_del_fornitore() {
        let p = "prova-attesa-di-modello";
        let m = "openai/gpt-oss-20b";
        assert!(!is_provider_in_cooldown(p), "premessa: parte disponibile");

        registra_esclusione_dichiarata(&EsclusioneDichiarata::Attesa {
            provider: p.to_string(),
            model: Some(m.to_string()),
            secondi: 1424,
        });

        assert!(
            is_model_in_cooldown(p, m),
            "il modello che ha esaurito il proprio tetto non va convocato"
        );
        assert!(
            !is_provider_in_cooldown(p),
            "il FORNITORE non e' escluso: un tetto per-modello non dice nulla dell'account"
        );
        assert!(
            !is_model_in_cooldown(p, "llama-3.3-70b"),
            "gli altri modelli hanno quota propria e restano convocabili"
        );
        // La portata registrata e' quella dichiarata, e ora e' LEGGIBILE come
        // campo invece che come chiave da interpretare (fix D1).
        let coppia = cooldown_snapshot_entries()
            .into_iter()
            .find(|e| e.chiave == ChiaveCooldown::coppia(p, m))
            .expect("la coppia dichiarata e' nel registro");
        assert_eq!(coppia.chiave.portata(), PortataCooldown::Model);
        assert_eq!(coppia.chiave.model(), Some(m));

        remove_cooldown(p);
    }

    /// Il credito passa dal punto unico che registra la severita' `Long`: e'
    /// quello che il ciclo probe-then-reenable riconosce, ed e' la ragione per
    /// cui la durata dichiarata dal gateway non viene usata come scadenza.
    #[test]
    fn il_credito_dichiarato_registra_una_severita_da_verificare() {
        let p = "prova-credito-dal-gateway";
        registra_esclusione_dichiarata(&EsclusioneDichiarata::Credito {
            provider: p.to_string(),
        });

        assert!(is_provider_in_cooldown(p));
        let voce = voce_del_fornitore(p).expect("il fornitore e' nello snapshot");
        assert_eq!(
            voce.severity,
            Some(CooldownSeverity::Long),
            "il credito non e' un'attesa: a liberarlo dev'essere chi lo verifica"
        );

        remove_cooldown(p);
    }

    /// IL DIFETTO trovato dalla review avversaria del 13/08/2026, prima che
    /// arrivasse in esercizio: un'attesa breve NON deve poter accorciare un
    /// cooldown lungo. Senza la guardia, 30 secondi dichiarati dal gateway
    /// sostituivano le 6 ore del credito e — degradando la severita' a `Short`
    /// — spegnevano anche `is_provider_in_billing_cooldown`, cioe' la
    /// protezione che tiene il probe periodico lontano da un fornitore senza
    /// credito.
    ///
    /// MUTAZIONE che lo fa rosseggiare: togliere il confronto col residuo noto
    /// in `registra_esclusione_dichiarata`.
    #[test]
    fn un_attesa_breve_non_accorcia_un_cooldown_lungo() {
        let p = "prova-attesa-non-accorcia";
        put_provider_in_long_cooldown(p, "credito esaurito");
        assert!(
            is_provider_in_billing_cooldown(p),
            "premessa: e' fuori per credito"
        );

        registra_esclusione_dichiarata(&EsclusioneDichiarata::Attesa {
            provider: p.to_string(),
            model: None,
            secondi: 30,
        });

        assert!(
            is_provider_in_billing_cooldown(p),
            "un'attesa di 30s ha declassato un cooldown di credito: il probe \
             tornerebbe a interrogare un fornitore che non puo' servire"
        );
        let residuo = voce_del_fornitore(p)
            .map(|e| e.remaining_seconds)
            .unwrap_or(0);
        assert!(
            residuo > 30,
            "la scadenza lunga e' stata accorciata: {residuo}s"
        );

        remove_cooldown(p);
    }

    /// Il contrappunto, senza il quale il test sopra sarebbe compatibile con
    /// «non registrare mai un'attesa»: piu' LUNGA della scadenza nota, aggiorna.
    #[test]
    fn un_attesa_piu_lunga_di_quella_nota_aggiorna() {
        let p = "prova-attesa-piu-lunga";
        registra_esclusione_dichiarata(&EsclusioneDichiarata::Attesa {
            provider: p.to_string(),
            model: None,
            secondi: 30,
        });
        registra_esclusione_dichiarata(&EsclusioneDichiarata::Attesa {
            provider: p.to_string(),
            model: None,
            secondi: 1800,
        });
        let residuo = voce_del_fornitore(p)
            .map(|e| e.remaining_seconds)
            .unwrap_or(0);
        assert!(
            residuo > 1000,
            "un'attesa piu' lunga deve aggiornare la scadenza: {residuo}s"
        );

        remove_cooldown(p);
    }

    /// Il residuo si misura sulla STESSA chiave, e col tipo questo non e' piu'
    /// affidato a chi compone una stringa: un'attesa lunga sul FORNITORE non
    /// deve impedire di registrare un'attesa piu' corta su una sua COPPIA, che
    /// e' un'altra esclusione.
    #[test]
    fn il_residuo_si_confronta_per_chiave_non_per_fornitore() {
        let p = "prova-residuo-per-chiave";
        let m = "modello-suo";
        registra_esclusione_dichiarata(&EsclusioneDichiarata::Attesa {
            provider: p.to_string(),
            model: None,
            secondi: 1800,
        });
        // Piu' corta del residuo del FORNITORE, ma la coppia non ha alcun
        // residuo proprio: va registrata.
        registra_esclusione_dichiarata(&EsclusioneDichiarata::Attesa {
            provider: p.to_string(),
            model: Some(m.to_string()),
            secondi: 60,
        });
        assert!(
            residuo_della_chiave(&ChiaveCooldown::coppia(p, m)) > 0,
            "l'attesa della coppia e' stata misurata contro il cooldown del fornitore"
        );

        remove_cooldown(p);
    }

    /// La meta' che protegge dal danno opposto: un fallimento che non parla
    /// della disponibilita' del fornitore non lo esclude.
    #[test]
    fn nessuna_esclusione_dichiarata_non_tocca_il_registro() {
        let p = "prova-nessuna-esclusione";
        registra_esclusione_dichiarata(&EsclusioneDichiarata::Nessuna);
        assert!(!is_provider_in_cooldown(p));
    }
}

#[cfg(test)]
mod tests_portata_cooldown {
    use super::*;

    /// IL difetto: un tetto token/minuto e' del MODELLO, e il cooldown escludeva
    /// il FORNITORE. Gli altri modelli, che hanno quota propria, sparivano per
    /// 60 secondi insieme a lui.
    ///
    /// MISURATO nella notte fra il 6 e il 7/08/2026: 479 cooldown brevi, 205 dei
    /// quali per rate limit su TUTTI i fornitori rimasti; in un run solo, 22
    /// failover su 39 escalation con motivo `cooldown` su fornitori che nel DB
    /// non ne avevano alcuno.
    ///
    /// MUTAZIONE: rimettere la chiave a solo-fornitore (togliere il ramo
    /// `Some(m)` di `chiave_cooldown`) -> il secondo assert rosseggia, perche'
    /// il modello sano risulta escluso.
    #[test]
    fn un_limite_di_un_modello_non_esclude_gli_altri_del_fornitore() {
        metti_in_cooldown_breve("prova-google", Some("gemini-pro"), "Rate limit raggiunto", 60);

        assert!(
            is_model_in_cooldown("prova-google", "gemini-pro"),
            "il modello che ha sforato e' escluso"
        );
        assert!(
            !is_model_in_cooldown("prova-google", "gemini-flash"),
            "un altro modello dello stesso fornitore ha quota propria e resta usabile"
        );
        assert!(
            !is_provider_in_cooldown("prova-google"),
            "il FORNITORE non e' in cooldown: non e' lui ad avere un problema"
        );
    }

    /// Il contrappunto, senza il quale il test sopra sarebbe compatibile con
    /// «non escludere mai niente»: il credito e' dell'ACCOUNT, e vale per ogni
    /// modello.
    #[test]
    fn il_credito_esaurito_esclude_tutti_i_modelli_del_fornitore() {
        metti_in_cooldown_breve("prova-openai", None, "Provider non raggiungibile", 60);

        assert!(is_provider_in_cooldown("prova-openai"));
        assert!(is_model_in_cooldown("prova-openai", "gpt-qualsiasi"));
        assert!(is_model_in_cooldown("prova-openai", "un-altro-modello"));
    }

    /// Le due portate non si confondono fra loro: due fornitori distinti, e un
    /// modello con lo stesso NOME sotto entrambi.
    #[test]
    fn la_chiave_distingue_fornitore_e_modello() {
        metti_in_cooldown_breve("prova-a", Some("stesso-nome"), "Rate limit raggiunto", 60);
        assert!(is_model_in_cooldown("prova-a", "stesso-nome"));
        assert!(
            !is_model_in_cooldown("prova-b", "stesso-nome"),
            "lo stesso nome di modello sotto un ALTRO fornitore e' un'altra cosa"
        );
    }

    /// D1: lo snapshot non consegna piu' una CHIAVE dove il lettore si aspetta un
    /// fornitore.
    ///
    /// MISURATO sul sistema vivo il 13/08/2026: `/api/internal/routing/cooldown`
    /// rispondeva `{"provider":"groq\u{1}openai/gpt-oss-20b"}`. Quella stringa non
    /// eguaglia nessun `provider` del catalogo, quindi ogni consumatore che ci
    /// filtrava sopra filtrava a vuoto.
    ///
    /// La misura passa dal PRODUTTORE (`metti_in_cooldown_breve`) e dal LETTORE
    /// (`cooldown_snapshot_entries`) reali, mai da una chiave scritta a mano
    /// (regola O). MUTAZIONE: rimettendo una chiave composta come `provider`, il
    /// primo assert rosseggia con la stringa `\u{1}` dentro.
    #[test]
    fn lo_snapshot_non_spaccia_una_chiave_per_un_fornitore() {
        let p = "__test_d1_snapshot_fornitore";
        metti_in_cooldown_breve(p, Some("openai/gpt-oss-20b"), "Rate limit raggiunto", 60);
        let voce = cooldown_snapshot_entries()
            .into_iter()
            .find(|e| e.chiave.provider() == p)
            .expect("la coppia messa in cooldown deve comparire nello snapshot");
        assert_eq!(
            voce.chiave.provider(),
            p,
            "il fornitore e' il fornitore, non la chiave composta"
        );
        assert_eq!(voce.chiave.model(), Some("openai/gpt-oss-20b"));
        assert_eq!(voce.chiave.portata(), PortataCooldown::Model);
        assert!(!voce.chiave.esclude_il_fornitore());
        remove_cooldown(p);
    }

    /// Le due domande hanno risposte DIVERSE, ed e' il motivo per cui sono due
    /// funzioni: chi filtra per fornitore non deve vedere il tetto di un modello.
    ///
    /// MUTAZIONE: far ricadere `fornitori_in_cooldown` su tutte le voci dello
    /// snapshot -> il primo assert rosseggia, perche' un rate limit su un modello
    /// tornerebbe a escludere il fornitore intero dalla selezione.
    #[test]
    fn fornitori_interi_e_coppie_sono_due_elenchi_distinti() {
        let solo_modello = "__test_d1_solo_modello";
        let tutto = "__test_d1_fornitore_intero";
        metti_in_cooldown_breve(solo_modello, Some("m-saturo"), "Rate limit raggiunto", 60);
        metti_in_cooldown_breve(tutto, None, "Provider non raggiungibile", 60);

        let fornitori = fornitori_in_cooldown();
        assert!(
            !fornitori.iter().any(|p| p == solo_modello),
            "un tetto di modello NON esclude il fornitore: {fornitori:?}"
        );
        assert!(
            fornitori.iter().any(|p| p == tutto),
            "un fornitore irraggiungibile e' escluso per intero: {fornitori:?}"
        );

        let coppie = coppie_in_cooldown();
        assert!(
            coppie.contains(&(solo_modello.to_string(), "m-saturo".to_string())),
            "la coppia esclusa deve essere elencabile: {coppie:?}"
        );
        assert!(
            !coppie.iter().any(|(p, _)| p == tutto),
            "un fornitore intero non si elenca modello per modello: {coppie:?}"
        );
        remove_cooldown(solo_modello);
        remove_cooldown(tutto);
    }

    /// «Rimetti il fornitore in servizio» vale anche per le sue coppie: un
    /// residuo per modello renderebbe l'azione admin vera a meta' senza dirlo.
    #[test]
    fn rimuovere_il_cooldown_di_un_fornitore_toglie_anche_le_sue_coppie() {
        let p = "__test_d1_remove_copre_le_coppie";
        metti_in_cooldown_breve(p, Some("m1"), "Rate limit raggiunto", 60);
        metti_in_cooldown_breve(p, None, "Provider non raggiungibile", 60);
        assert!(is_model_in_cooldown(p, "m1"));
        remove_cooldown(p);
        assert!(!is_provider_in_cooldown(p));
        assert!(
            !is_model_in_cooldown(p, "m1"),
            "il tetto sul modello non sopravvive al rientro in servizio del fornitore"
        );
    }

    /// Un modello vuoto (o di soli spazi) NON e' un modello: ricade sul
    /// fornitore, e la firma lo dichiara invece di produrre una chiave con un
    /// separatore e niente dopo.
    #[test]
    fn un_modello_vuoto_ricade_sul_fornitore() {
        let p = "__test_d1_modello_vuoto";
        metti_in_cooldown_breve(p, Some("   "), "causa qualsiasi", 60);
        assert!(
            is_provider_in_cooldown(p),
            "senza un modello la portata e' il fornitore"
        );
        remove_cooldown(p);
    }
}
