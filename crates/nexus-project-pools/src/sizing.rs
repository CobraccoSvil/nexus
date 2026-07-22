//! Punto unico (regola L) del dimensionamento dei pool verso il DB per-progetto
//! `<slug>_nexus`.
//!
//! Lo stesso database veniva aperto con due tetti diversi, decisi in due punti
//! che si ignoravano: 5 connessioni sul percorso caldo di mcp-core (da cui passa
//! tutto il dominio chat/run dopo la separazione, mig 0527) e 3 in questo crate.
//! Chi voleva capire "quante connessioni ho verso il DB del progetto" doveva
//! sapere quale delle due strade aveva preso il chiamante, e una patch applicata
//! all'una era un no-op sull'altra.
//!
//! Qui il tetto NON viene alzato: consolidare non e' l'occasione per cambiare un
//! numero che nessuno ha misurato (regola H). Si adotta ovunque il valore del
//! percorso che regge il carico vero.

use std::time::Duration;

/// Tetto di connessioni verso il DB per-progetto.
///
/// E' il valore che il percorso caldo (mcp-core) usava gia': i sub-run del
/// Consiglio scrivono meta-step e trace da qui. Il crate lo portava a 3 con la
/// premessa "servizi a basso QPS", vera per i suoi due soli chiamanti ma non
/// per il fan-out. Allineare al 5 non tocca il percorso caldo.
///
/// Se un giorno si misura una saturazione (acquire in errore, non "sembra
/// poco"), questo e' l'unico posto da cambiare.
pub const PROJECT_POOL_MAX_CONNECTIONS: u32 = 5;

/// Attesa massima per ottenere una connessione dal pool.
///
/// Volutamente NON configurabile: una manopola su questo valore sarebbe
/// l'invito ad allungarlo finche' un sintomo sparisce, che e' la toppa che la
/// regola H vieta. Il default di sqlx sarebbe 30s: qui si preferisce fallire
/// presto e visibilmente.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// Nessuna connessione tenuta aperta a vuoto: un progetto che non si sta usando
/// non deve occupare slot del ruolo.
const MIN_CONNECTIONS: u32 = 0;

/// Chiusura delle connessioni INATTIVE.
///
/// Il tetto per pool da solo non basta, perche' a crescere non e' il singolo
/// pool ma il LORO NUMERO: uno per progetto aperto (e piu' d'uno quando lo
/// stesso DB e' raggiunto da strade diverse). Misurato il 2026-07-22 dopo una
/// giornata su cinque progetti: 50 connessioni verso il cluster app, TUTTE
/// `idle`, esattamente il `rolconnlimit` del ruolo `nexus_app` -- da li' in poi
/// qualunque query falliva con "troppe connessioni per il ruolo" e il sistema
/// era fermo per intero, non un singolo run.
///
/// Un minuto e' abbondante per il lavoro a raffica di un run (le connessioni
/// vengono riusate, non riaperte a ogni query) e restituisce gli slot dei
/// progetti lasciati indietro. NON si alza il `rolconnlimit`: sarebbe la toppa
/// (regola H), il numero di progetti continuerebbe a crescere.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Opzioni di pool verso un DB per-progetto. Ogni apertura passa di qui, cosi'
/// tetto, attesa e ciclo di vita delle connessioni restano un fatto solo.
pub fn project_pool_options() -> sqlx::postgres::PgPoolOptions {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(PROJECT_POOL_MAX_CONNECTIONS)
        .min_connections(MIN_CONNECTIONS)
        .idle_timeout(IDLE_TIMEOUT)
        .acquire_timeout(ACQUIRE_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fissa il valore che le opzioni portano, cosi' un cambio del tetto e'
    /// deliberato e non un effetto collaterale.
    ///
    /// Non prova che i call site deleghino: un test unitario non vede chi apre
    /// i pool, e dichiarare il contrario sarebbe teatro. Quel controllo e' il
    /// guard `project-pool-sizing` in `scripts/check-single-source.sh`, che
    /// fallisce se un `.max_connections(` ricompare sulle strade del DB
    /// per-progetto.
    #[test]
    fn le_opzioni_portano_il_tetto_del_punto_unico() {
        let opts = project_pool_options();

        assert_eq!(opts.get_max_connections(), PROJECT_POOL_MAX_CONNECTIONS);
        assert_eq!(opts.get_acquire_timeout(), ACQUIRE_TIMEOUT);
    }

    /// Le connessioni INATTIVE devono essere restituite. E' il termine che
    /// mancava: col solo tetto per pool, il totale cresce col numero di progetti
    /// aperti finche' satura il `rolconnlimit` del ruolo e blocca TUTTI i
    /// progetti (misurato: 50 connessioni idle su cinque database, cluster
    /// inutilizzabile).
    #[test]
    fn le_connessioni_inattive_vengono_restituite() {
        let opts = project_pool_options();

        assert_eq!(
            opts.get_min_connections(),
            0,
            "un progetto inattivo non deve tenere connessioni aperte a vuoto"
        );
        assert_eq!(
            opts.get_idle_timeout(),
            Some(IDLE_TIMEOUT),
            "senza idle_timeout gli slot non tornano mai al ruolo"
        );
    }
}
