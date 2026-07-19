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

/// Opzioni di pool verso un DB per-progetto. Ogni apertura passa di qui, cosi'
/// tetto e attesa restano un fatto solo.
pub fn project_pool_options() -> sqlx::postgres::PgPoolOptions {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(PROJECT_POOL_MAX_CONNECTIONS)
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
}
