//! PUNTO UNICO delle precondizioni degli integration test, e dello SKIP che ne
//! consegue (regola L per il punto unico, regola O per l'onesta' del segnale).
//!
//! # Il difetto che questo crate chiude
//!
//! Gli integration test "opportunistici" girano se trovano un DB, un servizio in
//! ascolto, un JWT. Quando non li trovavano facevano `eprintln!("skip: ...")` e
//! `return`: 48 occorrenze in 11 file di tre crate, alcune delle quali
//! stampavano il solo `"skip"` senza dire di cosa. Un test che salta e un test
//! che ha verificato il contratto producono lo STESSO verde, e nel conteggio di
//! `cargo test` sono indistinguibili — il gate diceva "ok" su contratti che
//! nessuno aveva interrogato.
//!
//! # Perche' un crate, e non un modulo per crate
//!
//! `mcp-core` e' bin-only, quindi i suoi integration test non possono importare
//! nulla da lui: la prima versione di questo codice viveva in
//! `mcp-core/tests/support/mod.rs`. Ma `nexus-auth` e `nexus-project-pools` sono
//! DIPENDENZE di mcp-core e non possono risalire fino a lui, e ricopiarci il
//! modulo avrebbe prodotto tre copie della stessa decisione, libere di divergere
//! (esattamente cio' che la regola L vieta). Un crate di supporto, usato come
//! `dev-dependencies` da chiunque ne abbia bisogno, e' l'unica forma che tiene UN
//! solo punto di verita' — stesso ruolo che `nexus-migrations-embedded` ha per lo schema
//! di test.
//!
//! # Cosa c'e' davvero in CI, misurato (2026-07-26)
//!
//! Il job `verify` monta un Postgres di servizio, esporta `DATABASE_URL` e applica
//! le migrazioni. Quindi:
//!
//! - `DATABASE_URL` C'E': i test che dipendono dal solo DB girano davvero;
//! - `NEXUS_TEST_JWT` NON c'e': ogni test che firma una richiesta salta sempre;
//! - nessun servizio e' in ascolto su `MCP_CORE_URL`: ogni test al wire salta;
//! - il DB e' vuoto (migrazioni, nessun seed): i test che pretendono righe
//!   preesistenti saltano;
//! - il cluster app (5434) non esiste.
//!
//! # Il contratto
//!
//! - con `REQUIRE_INTEGRATION_TESTS=1` una precondizione mancante e' un
//!   **FALLIMENTO** che la nomina (modalita' di un ambiente che dichiara di avere
//!   tutto: un pezzo assente e' una configurazione rotta, non un test da saltare);
//! - altrimenti il test salta stampando `NEXUS_TEST_SKIP <categoria>: <motivo>`,
//!   marker cercabile nei log e preteso dal guard `test-skip-visibile`.
//!
//! Il binario di test in cui il marker compare e' gia' nell'output di
//! `cargo test` ("Running tests/<nome>.rs"), quindi il marker non ripete il file.
//!
//! Qui vivono anche `base_url`, `jwt_o_salta` e `db_o_salta`, che erano ricopiati
//! in sei file di mcp-core piu' due degli altri crate (`base_url` x4, `jwt` x4,
//! `db`/`pool_or_skip` x6): copie della stessa domanda, libere di divergere.

use std::env;
use std::fmt;

use sqlx::PgPool;

/// Env var che trasforma lo skip in fallimento. La imposta chi dichiara di avere
/// l'ambiente completo (job di integrazione, verifica locale prima di un push).
pub const REQUIRE_ENV: &str = "REQUIRE_INTEGRATION_TESTS";

/// Prefisso del marker di skip su stdout: stringa cercabile nei log di CI e dal
/// guard `test-skip-visibile` in `scripts/check-single-source.sh`.
pub const MARKER_SKIP: &str = "NEXUS_TEST_SKIP";

/// URL di default di mcp-core quando `MCP_CORE_URL` non e' impostata.
const URL_DEFAULT: &str = "http://localhost:4000";

/// Nome della variabile che porta la connessione al DB di metadati. Costante e non
/// letterale ripetuto: il nome compare nel messaggio di skip, nella lettura e nella
/// sonda della sentinella, e devono essere lo stesso nome.
pub const ENV_DATABASE_URL: &str = "DATABASE_URL";

/// Nome della variabile col token di test per le richieste autenticate.
pub const ENV_JWT: &str = "NEXUS_TEST_JWT";

/// Perche' un test non puo' girare. Le categorie sono quelle realmente presenti
/// nei crate; tenerle distinte serve a leggere il log senza indovinare: una env
/// var che manca si esporta, un servizio giu' si avvia, dei dati assenti si
/// seminano, un 401 si corregge.
pub enum Motivo<'a> {
    /// Variabile d'ambiente assente o vuota (se ne indica il NOME, mai il valore).
    EnvAssente(&'a str),
    /// Endpoint o cluster che non risponde (se ne indica un'etichetta leggibile).
    ServizioGiu(&'a str),
    /// La fonte e' raggiungibile ma non contiene le righe che il test pretende.
    DatiAssenti(&'a str),
    /// Un artefatto di build (binario, file generato) non c'e'.
    ArtefattoAssente(&'a str),
    /// Il servizio risponde, ma con uno status che impedisce di proseguire.
    ///
    /// Categoria distinta da [`Motivo::ServizioGiu`] perche' il rimedio e'
    /// opposto: un 401 non si risolve avviando il servizio, si risolve
    /// correggendo la richiesta (o il token). Tenerli separati evita di leggere
    /// "servizio non raggiungibile" davanti a un servizio che ha risposto
    /// benissimo di no.
    RispostaInattesa { status: u16, path: &'a str },
}

impl Motivo<'_> {
    /// Etichetta breve della categoria, per raggruppare i marker nei log.
    fn categoria(&self) -> &'static str {
        match self {
            Motivo::EnvAssente(_) => "env",
            Motivo::ServizioGiu(_) => "servizio",
            Motivo::DatiAssenti(_) => "dati",
            Motivo::ArtefattoAssente(_) => "artefatto",
            Motivo::RispostaInattesa { .. } => "risposta",
        }
    }
}

impl fmt::Display for Motivo<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Motivo::EnvAssente(nome) => write!(f, "{nome} non impostata"),
            Motivo::ServizioGiu(che) => write!(f, "{che} non raggiungibile"),
            Motivo::DatiAssenti(che) => write!(f, "dati assenti: {che}"),
            Motivo::ArtefattoAssente(che) => write!(f, "artefatto assente: {che}"),
            Motivo::RispostaInattesa { status, path } => {
                write!(f, "GET {path} ha risposto {status}")
            }
        }
    }
}

/// Vero se l'ambiente dichiara di avere tutte le precondizioni
/// (`REQUIRE_INTEGRATION_TESTS=1`): allora una precondizione mancante e' un
/// fallimento, non uno skip.
pub fn richiede_integrazione() -> bool {
    env::var(REQUIRE_ENV).is_ok_and(|v| v.trim() == "1")
}

/// PUNTO UNICO dello skip: panica se l'ambiente esige l'integrazione, altrimenti
/// stampa il marker preteso dal gate.
///
/// Nessun test deve stampare uno skip da solo: fuori da qui il salto torna
/// invisibile, ed e' esattamente il difetto che questo crate chiude.
pub fn salta(motivo: Motivo) {
    assert!(
        !richiede_integrazione(),
        "{REQUIRE_ENV}=1 ma una precondizione manca ({motivo}): \
         il test NON puo' essere considerato eseguito."
    );
    println!("{MARKER_SKIP} {}: {motivo}", motivo.categoria());
}

/// URL base di mcp-core: `MCP_CORE_URL` oppure il default di sviluppo.
pub fn base_url() -> String {
    env::var("MCP_CORE_URL").unwrap_or_else(|_| URL_DEFAULT.into())
}

/// Token di test per le richieste autenticate.
///
/// NB il token viaggia nel COOKIE (`nexus_auth::validate_token` lo estrae solo da
/// li'): con `bearer_auth` la risposta e' 401 qualunque sia il contratto sotto
/// test.
pub fn jwt_o_salta() -> Option<String> {
    match env::var(ENV_JWT).ok().filter(|s| !s.is_empty()) {
        Some(t) => Some(t),
        None => {
            salta(Motivo::EnvAssente(ENV_JWT));
            None
        }
    }
}

/// Pool sul DB di metadati (`DATABASE_URL`). Distingue i due modi di fallire: la
/// variabile assente e la variabile presente ma il DB che non accetta connessioni
/// — prima erano lo stesso `skip: DATABASE_URL non impostata`, che mentiva nel
/// secondo caso.
pub async fn db_o_salta() -> Option<PgPool> {
    let url = match env::var(ENV_DATABASE_URL).ok().filter(|s| !s.is_empty()) {
        Some(u) => u,
        None => {
            salta(Motivo::EnvAssente(ENV_DATABASE_URL));
            return None;
        }
    };
    match PgPool::connect(&url).await {
        Ok(pool) => Some(pool),
        Err(_) => {
            // Nessun dettaglio dell'errore: la stringa di connessione contiene la
            // password (regola F).
            salta(Motivo::ServizioGiu("il DB di DATABASE_URL"));  // etichetta, non nome di variabile
            None
        }
    }
}

/// Pool su un'altra URL di Postgres (cluster app, DB per-progetto): `etichetta`
/// descrive di quale cluster si tratta, senza mai stampare la stringa di
/// connessione.
pub async fn db_url_o_salta(url: &str, etichetta: &str) -> Option<PgPool> {
    match PgPool::connect(url).await {
        Ok(pool) => Some(pool),
        Err(_) => {
            salta(Motivo::ServizioGiu(etichetta));
            None
        }
    }
}

/// Una precondizione e la sua disponibilita' effettiva, per le sentinelle che
/// dichiarano il quadro dell'ambiente.
pub struct Stato {
    /// Nome leggibile della precondizione.
    pub nome: &'static str,
    /// Se e' soddisfatta ADESSO.
    pub presente: bool,
    /// Cosa resta non misurato quando manca.
    pub conseguenza: &'static str,
}

/// Stampa il quadro delle precondizioni e ritorna i nomi di quelle mancanti.
///
/// Regola O, "un numero senza la sua premessa e' un'opinione": un test di
/// integrazione e' verde in due casi opposti (ha verificato il contratto, o non ha
/// trovato l'ambiente ed e' uscito subito) e nel conteggio sono identici. Una
/// sentinella che chiama questa funzione rende la premessa esplicita nell'output.
///
/// Non asserisce nulla: senza `REQUIRE_INTEGRATION_TESTS=1` un ambiente incompleto
/// e' legittimo. Per pretendere la completezza si passa il risultato a
/// [`pretendi_ambiente_completo`].
pub fn dichiara_quadro(contesto: &str, stati: &[Stato]) -> Vec<&'static str> {
    let presenti = stati.iter().filter(|s| s.presente).count();
    println!(
        "PRECONDIZIONI INTEGRAZIONE {contesto}: {presenti}/{} presenti",
        stati.len()
    );
    for s in stati {
        let segno = if s.presente { "OK  " } else { "MANCA" };
        println!("  {segno} {} -> {}", s.nome, s.conseguenza);
    }

    let mancanti: Vec<&'static str> = stati
        .iter()
        .filter(|s| !s.presente)
        .map(|s| s.nome)
        .collect();
    if !mancanti.is_empty() {
        println!(
            "  I test che dipendono da [{}] saltano: verdi senza aver misurato nulla.",
            mancanti.join(", ")
        );
    }
    mancanti
}

/// Con `REQUIRE_INTEGRATION_TESTS=1` pretende che il quadro sia completo: se
/// manca qualcosa panica elencandolo. Senza quella variabile non fa nulla.
pub fn pretendi_ambiente_completo(mancanti: &[&str], totale: usize) {
    let ambiente_incompleto = richiede_integrazione() && !mancanti.is_empty();
    assert!(
        !ambiente_incompleto,
        "{REQUIRE_ENV}=1 ma mancano {} precondizioni su {totale}: {}. \
         I contratti che ne dipendono NON sono verificati.",
        mancanti.len(),
        mancanti.join(", ")
    );
}

/// Il DB di `DATABASE_URL` accetta connessioni? Per le sentinelle: distingue
/// "variabile assente" da "DB che non risponde", due guasti con due rimedi.
pub async fn db_risponde() -> bool {
    let Some(url) = env::var(ENV_DATABASE_URL).ok().filter(|s| !s.is_empty()) else {
        return false;
    };
    PgPool::connect(&url).await.is_ok()
}

/// La variabile e' presente e non vuota?
pub fn env_presente(nome: &str) -> bool {
    env::var(nome).is_ok_and(|v| !v.is_empty())
}
