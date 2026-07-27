//! Il ponte fra le due strade verso lo stesso set.
//!
//! Restano due incarnazioni, e non e' un difetto: `#[sqlx::test]` accetta solo
//! uno `&'static Migrator`, quindi i test hanno bisogno del set INCORPORATO a
//! compile-time; la produzione lo LEGGE dal disco a runtime, perche' deve poter
//! applicare il set dell'albero in cui gira. Due strade legate da una
//! convenzione divergono; queste sono legate da un test.
//!
//! Il test vive QUI e non dietro una feature: una feature non-default si
//! accenderebbe solo per unificazione accidentale delle dipendenze, e
//! `cargo test -p nexus-migrations-embedded` la spegnerebbe in silenzio. Un
//! contract test che si auto-salta somiglia troppo a uno che passa.

use nexus_migrations::{OrigineSet, Set};

/// Radice del repository dal marker `.git`, risalendo dall'albero di
/// compilazione: e' lo stesso criterio del comando `xtask migrate`, e non
/// dipende dalla directory da cui i test vengono lanciati.
fn radice() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if p.join(".git").exists() {
            return p;
        }
        assert!(p.pop(), "radice del repository non trovata");
    }
}

/// Le versioni incorporate a compile-time sono ESATTAMENTE quelle sul disco.
///
/// MUTAZIONE: aggiungere un file `.sql` al set senza ricompilare questo crate
/// (o togliere il `rerun-if-changed` dal suo build.rs) e il test rosseggia,
/// invece di lasciare i test girare su uno schema che la produzione non ha piu'.
#[tokio::test]
async fn il_set_incorporato_e_quello_su_disco() {
    for (set, incorporato) in [
        (Set::Meta, &nexus_migrations_embedded::META_MIGRATOR),
        (Set::Project, &nexus_migrations_embedded::PROJECT_MIGRATOR),
    ] {
        let origine = OrigineSet::esplicita(radice());
        let dal_disco = nexus_migrations::risolvi(set, &origine)
            .await
            .unwrap_or_else(|e| panic!("set '{set}' non leggibile dal disco: {e}"));

        let v_incorporate: Vec<i64> = incorporato.iter().map(|m| m.version).collect();
        let v_disco: Vec<i64> = dal_disco.iter().map(|m| m.version).collect();
        assert_eq!(
            v_incorporate, v_disco,
            "il set '{set}' incorporato a compile-time non coincide con quello sul \
             disco: i test girerebbero su uno schema diverso da quello che la \
             produzione applica"
        );

        // Non basta che i numeri combacino: anche il CONTENUTO deve, altrimenti
        // una migrazione modificata dopo la compilazione passerebbe inosservata.
        let c_incorporate: Vec<_> = incorporato.iter().map(|m| m.checksum.clone()).collect();
        let c_disco: Vec<_> = dal_disco.iter().map(|m| m.checksum.clone()).collect();
        assert_eq!(
            c_incorporate, c_disco,
            "il set '{set}': stesse versioni ma contenuto diverso fra compile-time e \
             disco (una migrazione e' stata modificata dopo la compilazione)"
        );
    }
}

/// Il set del progetto e' una sottodirectory di quello META: il migrator META
/// non deve inglobarne le migrazioni, altrimenti applicherebbe al DB meta lo
/// schema dei DB-progetto.
#[tokio::test]
async fn i_due_set_restano_distinti() {
    let meta: Vec<i64> = nexus_migrations_embedded::META_MIGRATOR
        .iter()
        .map(|m| m.version)
        .collect();
    let project: Vec<i64> = nexus_migrations_embedded::PROJECT_MIGRATOR
        .iter()
        .map(|m| m.version)
        .collect();
    assert!(!meta.is_empty() && !project.is_empty());
    assert_ne!(
        meta.len(),
        project.len(),
        "i due set hanno lo stesso numero di migrazioni: sospetto che il migrator \
         META abbia inglobato la sottodirectory project"
    );
    // Il set project e' molto piu' piccolo: se il META lo contenesse, la sua
    // ultima versione coinciderebbe con quella del project.
    assert!(
        meta.len() > project.len(),
        "il set META deve essere piu' grande di quello project"
    );
}
