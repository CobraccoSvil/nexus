//! PUNTO UNICO delle precondizioni degli integration test di mcp-core, e dello
//! SKIP che ne consegue.
//!
//! # Il difetto che questo modulo chiude
//!
//! Gli integration test di questo crate sono "opportunistici": girano se trovano
//! un DB, un mcp-core in ascolto, un JWT valido. Quando non li trovano facevano
//! `eprintln!("skip: ...")` e `return` — 42 occorrenze in 9 file, sette delle
//! quali stampavano il solo `"skip"` senza dire di cosa. Un test che salta e un
//! test che ha verificato il contratto producono lo STESSO verde, e nel conteggio
//! di `cargo test` sono indistinguibili: il gate diceva "ok" su contratti che
//! nessuno aveva interrogato. L'intestazione di `settings_update_contract.rs` lo
//! ammetteva perfino a parole ("uno skip qui si presenta come ok nel gate") senza
//! che nulla, nel segnale, lo rendesse visibile.
//!
//! # Cosa e' presente in CI, misurato (2026-07-26)
//!
//! Il job `verify` (`.github/workflows/verify.yml`) monta un Postgres di servizio
//! ed esporta `DATABASE_URL`, poi applica le migrazioni. Quindi, in CI:
//!
//! - `DATABASE_URL` C'E': i test che dipendono dal solo DB girano davvero;
//! - `NEXUS_TEST_JWT` NON c'e': ogni test che firma una richiesta salta sempre;
//! - nessun mcp-core e' in ascolto su `MCP_CORE_URL`: ogni test al wire salta
//!   sempre;
//! - il DB e' vuoto (migrazioni, nessun seed): i test che pretendono righe
//!   preesistenti (progetti, sessioni con turni assistant) saltano sempre;
//! - il cluster app (5434) non esiste: `postgres_app_isolation` salta sempre.
//!
//! Nessuno di questi skip era visibile nel gate.
//!
//! # Il contratto di questo modulo
//!
//! Ogni precondizione passa da qui, e in assenza di precondizione:
//!
//! - con `REQUIRE_INTEGRATION_TESTS=1` il test **FALLISCE** dicendo cosa manca
//!   (modalita' di un ambiente che dichiara di avere tutto: un pezzo assente e'
//!   una configurazione rotta, non un test da saltare);
//! - altrimenti salta, stampando un marker `NEXUS_TEST_SKIP <categoria>: ...`
//!   riconoscibile nei log e contato dal gate.
//!
//! La sentinella `tests/precondizioni_integrazione.rs` dichiara, a ogni
//! esecuzione, quali precondizioni ci sono e quali no: e' il "numero con la sua
//! premessa" della regola O. Il binario di test in cui il marker compare e' gia'
//! nell'output di `cargo test` ("Running tests/<nome>.rs"), quindi il marker non
//! ripete il file.
//!
//! Qui vivono anche `base_url`, `jwt_o_salta` e `db_o_salta`, che prima erano
//! ricopiati in sei file (`base_url` x4, `jwt` x4, `db`/`pool_or_skip` x4):
//! copie che possono divergere sulla stessa domanda (regola L).

// Ogni file in `tests/` e' un binario separato che compila una propria copia di
// questo modulo, e nessuno di essi usa TUTTE le funzioni: senza questo allow, le
// non usate in quel binario diventano `dead_code` e, con `-D warnings`, errori di
// clippy. Il codice resta unico (regola L): la duplicazione e' solo nel link.
#![allow(dead_code)]

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

/// Perche' un test non puo' girare. Le quattro categorie sono quelle realmente
/// presenti nel crate; tenerle distinte serve a leggere il log senza indovinare:
/// una env var che manca si risolve esportandola, un servizio giu' si risolve
/// avviandolo, dei dati assenti si risolvono seminandoli.
pub enum Motivo<'a> {
    /// Variabile d'ambiente assente o vuota (se ne indica il NOME, mai il valore).
    EnvAssente(&'a str),
    /// Endpoint HTTP che non risponde (se ne indica l'URL).
    ServizioGiu(&'a str),
    /// Il DB e' raggiungibile ma non contiene le righe che il test pretende.
    DatiAssenti(&'a str),
    /// Un artefatto di build (binario, file generato) non c'e'.
    ArtefattoAssente(&'a str),
    /// Il servizio risponde, ma con uno status che impedisce di proseguire.
    ///
    /// Categoria distinta da [`Motivo::ServizioGiu`] perche' il rimedio e'
    /// opposto: un 401 non si risolve avviando il servizio, si risolve
    /// correggendo la richiesta (o il token). Tenerli separati evita di leggere
    /// "servizio non raggiungibile" davanti a un servizio che ha risposto benissimo
    /// di no.
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
            Motivo::ServizioGiu(url) => write!(f, "{url} non raggiungibile"),
            Motivo::DatiAssenti(che) => write!(f, "dati assenti nel DB: {che}"),
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
/// stampa il marker contato dal gate.
///
/// Nessun test deve stampare uno skip da solo: fuori da qui il salto torna
/// invisibile al conteggio, ed e' esattamente il difetto che si sta chiudendo.
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
/// test. Vedi `header("Cookie", format!("token={token}"))` nei chiamanti.
pub fn jwt_o_salta() -> Option<String> {
    match env::var("NEXUS_TEST_JWT").ok().filter(|s| !s.is_empty()) {
        Some(t) => Some(t),
        None => {
            salta(Motivo::EnvAssente("NEXUS_TEST_JWT"));
            None
        }
    }
}

/// Pool sul DB di metadati (`DATABASE_URL`). Distingue i due modi di fallire: la
/// variabile assente e la variabile presente ma il DB che non accetta connessioni
/// — prima erano lo stesso `skip: DATABASE_URL non impostata`, che mentiva nel
/// secondo caso.
pub async fn db_o_salta() -> Option<PgPool> {
    let url = match env::var("DATABASE_URL").ok().filter(|s| !s.is_empty()) {
        Some(u) => u,
        None => {
            salta(Motivo::EnvAssente("DATABASE_URL"));
            return None;
        }
    };
    match PgPool::connect(&url).await {
        Ok(pool) => Some(pool),
        Err(_) => {
            // Nessun dettaglio dell'errore: la stringa di connessione contiene la
            // password (regola F).
            salta(Motivo::ServizioGiu("il DB di DATABASE_URL"));
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
