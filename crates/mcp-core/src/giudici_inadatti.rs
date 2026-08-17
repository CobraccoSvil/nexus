//! PUNTO UNICO (regola L) della domanda: **questa coppia (fornitore, modello)
//! ha gia' dimostrato di non saper produrre il verdetto che il gate pretende?**
//!
//! ## Il difetto che ha reso necessario il modulo (17/08/2026)
//!
//! Progetto `app-completa-17-08`, run reale dalla UI. Un sub-run `implement` e'
//! rimasto oltre 400 secondi su UN passo, e non per lentezza: due comandi di
//! sola lettura (`node -e "require('./backend/package.json')"` e `jq
//! '.dependencies | keys' backend/package.json`) non sono mai stati eseguiti.
//! Il meta_step `step_validation` registra la catena per intero: gatekeeper
//! `mistral/magistral-small-latest` che APPROVA («non si rilevano rischi di
//! blast radius o di distruzione irreversibile»), challenger `kimi/kimi-k2.6`
//! che si astiene con `schema_mismatch`. Su un passo critico un parere solo non
//! basta: rimando. L'agente riprova, la selezione ripropone LA STESSA COPPIA di
//! giudici, stesso esito, tetto dei rimandi, `retries_exhausted`, run chiuso.
//!
//! Il sistema non aveva modo di saperlo: `kimi-k2.6` e' a catalogo
//! `supports_tool_use = true` e `qualified`, e la dichiarazione non e' falsa —
//! quel modello le tool call le fa. Non regge lo schema STRICT del verdetto, che
//! e' un fatto su QUESTO schema e non sul fornitore. Nessuno lo registrava.
//!
//! ## Perche' in memoria e non a DB
//!
//! E' un fatto sul COMPORTAMENTO OSSERVATO di un modello rispetto a uno schema
//! NOSTRO, non sulla salute del fornitore: `nexus_provider_health` risponde a
//! un'altra domanda e con un'altra portata (li' un'esclusione toglie il modello
//! a TUTTO il lavoro, mentre qui quel modello resta perfettamente usabile — non
//! sa fare IL GIUDICE su QUESTO schema). La memoria di processo e' gia' la sede
//! degli altri fatti operativi analoghi: il carico in volo
//! ([`crate::provider_inflight`]) e il cooldown vivo
//! ([`crate::provider_cooldown`]). Se un domani servisse la serie storica, la
//! sede giusta sarebbe `ai_model_health_history` — non una tabella nuova.
//!
//! ## Non e' un cooldown, e non deve diventarlo
//!
//! Il registro NON entra in [`crate::orchestrator::model_selection::esclusioni_selezione`]:
//! quella e' la lista dei fornitori che il ROUTING non puo' usare, e mettercelo
//! toglierebbe un modello sano al lavoro ordinario per un difetto che riguarda
//! un solo schema. Lo consulta la sola selezione dei validatori del gate
//! ([`crate::agent_graph_adapter::step_validation`]).
//!
//! ## Il TTL, e perche' non e' una condanna
//!
//! Un modello cambia col deploy del fornitore: l'osservazione scade
//! (`orchestrator.step_validator_inadatto_ttl_s`, regola G — mai una costante
//! nel codice) e la coppia torna eleggibile. TTL a zero = registro SPENTO, ed e'
//! il kill switch senza una seconda chiave: nessuna marcatura, nessuna
//! esclusione, comportamento bit-identico a prima del 17/08.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Setting (regola G) della durata di un'osservazione di inadeguatezza.
pub const KEY_TTL_S: &str = "orchestrator.step_validator_inadatto_ttl_s";

/// Quanto dura un'osservazione se il DB tace. Un'ora: abbastanza da coprire un
/// run intero e i suoi rimandi (che e' il caso misurato), troppo poco per
/// sopravvivere al deploy di un modello corretto dal fornitore.
pub const DEFAULT_TTL_S: u64 = 3600;

/// Cosa il registro sa di UNA coppia. Tre casi e non un `bool` (regola Q): il
/// terzo non e' un «no» come gli altri — il registro puo' non essere
/// interrogabile (mutex avvelenato), e chiamarlo «adatta» direbbe che si e'
/// guardato quando non e' vero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GiudizioSullaCoppia {
    /// Osservata inadatta, e l'osservazione non e' ancora scaduta.
    Inadatta {
        /// La causa STRUTTURATA dell'astensione che l'ha prodotta (regola M).
        causa: String,
        /// Quanto resta prima che la coppia torni eleggibile.
        residuo: Duration,
    },
    /// Nessuna osservazione a carico, o gia' scaduta.
    NessunaOsservazione,
    /// Registro non leggibile: non si esclude nessuno, e lo si DICHIARA invece
    /// di far passare la cosa per un'assoluzione.
    NonInterrogabile,
}

impl GiudizioSullaCoppia {
    /// L'unica conseguenza che la selezione deriva da qui.
    pub fn esclude(&self) -> bool {
        matches!(self, Self::Inadatta { .. })
    }
}

/// Esito della marcatura. Tipizzato per la stessa ragione: «non ho registrato
/// perche' il registro e' spento» e «non ho potuto registrare» sono due fatti
/// diversi, e il secondo va detto nei log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marcatura {
    /// Registrata: la coppia resta fuori dai candidati per questa durata.
    Registrata { residuo: Duration },
    /// TTL a zero: il registro e' spento per configurazione.
    RegistroSpento,
    /// Registro non leggibile: nessuna marcatura.
    NonInterrogabile,
}

/// Cio' che il registro conserva di una coppia.
struct Osservazione {
    causa: String,
    scade: Instant,
}

static INADATTI: OnceLock<Mutex<HashMap<(String, String), Osservazione>>> = OnceLock::new();

fn registro() -> &'static Mutex<HashMap<(String, String), Osservazione>> {
    INADATTI.get_or_init(|| Mutex::new(HashMap::new()))
}

/// L'identita' del giudice: DELEGATA al punto unico che gia' la definisce
/// ([`crate::internal_routing::judge_key_di`]). Due normalizzazioni darebbero
/// due idee diverse di «stesso giudice» ai due capi — chi marca e chi filtra.
fn chiave(provider: &str, model: &str) -> (String, String) {
    crate::internal_routing::judge_key_di(provider, model)
}

/// Registra che questa coppia si e' astenuta per una causa STRUTTURALE.
///
/// La causa la porta il chiamante gia' classificata (`natura_astensione` in
/// `decisions::step_gate`): qui non si giudica quale astensione conti, si
/// registra il fatto. Il TTL arriva dal DB per la stessa ragione.
///
/// La coppia registrata e' quella EFFETTIVA della risposta (`provider_used` /
/// `model_used`), non il candidato scelto: se il gateway ha instradato altrove,
/// chi non ha saputo produrre il verdetto e' chi ha risposto (regola M).
pub fn segna_inadatto(provider: &str, model: &str, causa: &str, ttl: Duration) -> Marcatura {
    if ttl.is_zero() {
        return Marcatura::RegistroSpento;
    }
    let Ok(mut mappa) = registro().lock() else {
        return Marcatura::NonInterrogabile;
    };
    let ora = Instant::now();
    // Potatura all'inserimento: la mappa e' piccola, ma senza questo le voci
    // scadute resterebbero per tutta la vita del processo.
    mappa.retain(|_, o| o.scade > ora);
    mappa.insert(
        chiave(provider, model),
        Osservazione {
            causa: causa.to_string(),
            scade: ora + ttl,
        },
    );
    Marcatura::Registrata { residuo: ttl }
}

/// Cosa sappiamo di questa coppia ADESSO.
pub fn giudizio_sulla_coppia(provider: &str, model: &str) -> GiudizioSullaCoppia {
    let Ok(mappa) = registro().lock() else {
        return GiudizioSullaCoppia::NonInterrogabile;
    };
    let ora = Instant::now();
    match mappa.get(&chiave(provider, model)) {
        Some(o) if o.scade > ora => GiudizioSullaCoppia::Inadatta {
            causa: o.causa.clone(),
            residuo: o.scade - ora,
        },
        // Voce scaduta: non si toglie qui (la lettura non muta il registro), la
        // potatura e' della scrittura. Per chi chiede, e' come se non ci fosse.
        _ => GiudizioSullaCoppia::NessunaOsservazione,
    }
}

/// Dimentica UNA coppia. Serve ai test — che seminano lo stato globale del
/// processo e devono restituirlo pulito — e all'operatore che, dopo aver
/// corretto un modello, non vuole aspettare il TTL.
pub fn dimentica(provider: &str, model: &str) {
    if let Ok(mut mappa) = registro().lock() {
        mappa.remove(&chiave(provider, model));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prefisso dei fornitori di questo modulo: il registro e' GLOBALE al
    /// processo e i test girano in parallelo. Stessa convenzione (e stessa
    /// ragione) dei test di `provider_cooldown`.
    fn forn(nome: &str) -> String {
        format!("gi1708_{nome}")
    }

    /// Il fatto e' della COPPIA, non del fornitore: e' l'intera ragione per cui
    /// questo registro non e' un cooldown. Un modello che non regge lo schema
    /// del verdetto non toglie dal pool gli altri modelli del suo fornitore, e
    /// non toglie se stesso dal lavoro ordinario.
    ///
    /// MUTAZIONE: chiavare su `provider` invece che sulla coppia -> la seconda
    /// asserzione cade, e col danno reale (un fornitore intero fuori dal pool
    /// dei giudici per il difetto di un suo modello).
    #[test]
    fn l_osservazione_e_della_coppia_non_del_fornitore() {
        let p = forn("kimi");
        segna_inadatto(&p, "kimi-k2.6", "schema_mismatch", Duration::from_secs(60));
        assert!(giudizio_sulla_coppia(&p, "kimi-k2.6").esclude());
        assert!(
            !giudizio_sulla_coppia(&p, "kimi-k3").esclude(),
            "un altro modello dello stesso fornitore non c'entra nulla"
        );
        dimentica(&p, "kimi-k2.6");
    }

    /// L'identita' e' quella del GIUDICE (punto unico `judge_key_di`): case e
    /// spazi non fanno due coppie diverse, o chi marca e chi filtra
    /// parlerebbero di due modelli.
    #[test]
    fn la_stessa_coppia_scritta_in_modi_diversi_resta_una_sola() {
        let p = forn("openrouter");
        segna_inadatto(&p, "Z-AI/GLM-4.7", "schema_mismatch", Duration::from_secs(60));
        assert!(giudizio_sulla_coppia(&p, "z-ai/glm-4.7").esclude());
        assert!(giudizio_sulla_coppia(&format!("  {p}  "), " z-ai/glm-4.7 ").esclude());
        dimentica(&p, "z-ai/glm-4.7");
    }

    /// Non e' una condanna: scaduto il TTL la coppia torna eleggibile — un
    /// modello cambia col deploy del fornitore.
    ///
    /// MUTAZIONE: ignorare `scade` nella lettura (ritornare `Inadatta` per ogni
    /// voce presente) -> la seconda asserzione cade, e la coppia resta esclusa
    /// per tutta la vita del processo.
    #[test]
    fn l_osservazione_scade() {
        let p = forn("mistral");
        segna_inadatto(&p, "magistral-small", "schema_mismatch", Duration::from_millis(40));
        assert!(giudizio_sulla_coppia(&p, "magistral-small").esclude());
        std::thread::sleep(Duration::from_millis(70));
        assert_eq!(
            giudizio_sulla_coppia(&p, "magistral-small"),
            GiudizioSullaCoppia::NessunaOsservazione,
            "scaduto il TTL la coppia torna eleggibile"
        );
        dimentica(&p, "magistral-small");
    }

    /// TTL a zero = registro spento (il kill switch della mig 0736, senza una
    /// seconda chiave): non si marca, e quindi non si esclude nessuno.
    #[test]
    fn ttl_zero_spegne_il_registro() {
        let p = forn("google");
        assert_eq!(
            segna_inadatto(&p, "gemini-x", "schema_mismatch", Duration::ZERO),
            Marcatura::RegistroSpento
        );
        assert!(!giudizio_sulla_coppia(&p, "gemini-x").esclude());
    }

    /// Una coppia mai osservata non e' «inadatta», e la distinzione fra «non
    /// c'e' nulla a suo carico» e «non ho potuto guardare» resta nel tipo.
    #[test]
    fn nessuna_osservazione_non_esclude() {
        assert_eq!(
            giudizio_sulla_coppia(&forn("mai-visto"), "modello-mai-visto"),
            GiudizioSullaCoppia::NessunaOsservazione
        );
        assert!(!GiudizioSullaCoppia::NonInterrogabile.esclude());
        assert!(!GiudizioSullaCoppia::NessunaOsservazione.esclude());
        assert!(GiudizioSullaCoppia::Inadatta {
            causa: "schema_mismatch".into(),
            residuo: Duration::from_secs(1)
        }
        .esclude());
    }

    /// La causa resta LEGGIBILE nel giudizio: chi esclude un candidato deve
    /// poter dire perche', e la sola presenza nella mappa non lo direbbe.
    #[test]
    fn il_giudizio_porta_la_causa_osservata() {
        let p = forn("deepseek");
        segna_inadatto(&p, "v4-pro", "schema_mismatch", Duration::from_secs(30));
        match giudizio_sulla_coppia(&p, "v4-pro") {
            GiudizioSullaCoppia::Inadatta { causa, residuo } => {
                assert_eq!(causa, "schema_mismatch");
                assert!(residuo <= Duration::from_secs(30) && !residuo.is_zero());
            }
            altro => panic!("attesa Inadatta, trovato {altro:?}"),
        }
        dimentica(&p, "v4-pro");
    }
}
