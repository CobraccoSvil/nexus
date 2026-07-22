//! Contract test del REGISTRO dei pool per-progetto: un pool per database.
//!
//! Non serve un DB vivo: `connect_lazy` non apre connessioni, quindi questi test
//! esercitano il registro (chi apre, quante volte) e non il trasporto.
//!
//! Perche' esistono: il crate aveva gia' due test verdi sulle costanti di
//! `sizing.rs` mentre il cluster app era saturo. Il tetto per pool era davvero
//! rispettato -- a crescere era il NUMERO dei pool, che quei test non guardavano.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use nexus_project_pools::{cached_pool, pool_or_open};
use sqlx::PgPool;
use uuid::Uuid;

/// Pool che non contatta nessun server: l'URL basta che sia ben formata.
fn pool_finto() -> PgPool {
    PgPool::connect_lazy("postgres://x:x@127.0.0.1:1/x").expect("URL ben formata")
}

/// Il contratto che mancava: aprire due volte il pool dello STESSO progetto deve
/// restituire lo STESSO pool. Si conta quante volte l'apertura viene
/// EFFETTIVAMENTE invocata, perche' e' quel numero che saturava il ruolo.
#[tokio::test]
async fn seconda_apertura_ritrova_il_pool_della_prima() {
    let pid = Uuid::new_v4();
    let aperture = AtomicUsize::new(0);

    let primo = pool_or_open(pid, || async {
        aperture.fetch_add(1, Ordering::SeqCst);
        Ok::<_, sqlx::Error>(pool_finto())
    })
    .await
    .expect("prima apertura");

    let secondo = pool_or_open(pid, || async {
        aperture.fetch_add(1, Ordering::SeqCst);
        Ok::<_, sqlx::Error>(pool_finto())
    })
    .await
    .expect("seconda risoluzione");

    assert_eq!(
        aperture.load(Ordering::SeqCst),
        1,
        "il secondo accesso ha aperto un SECONDO pool verso lo stesso database"
    );
    assert!(
        Arc::ptr_eq(&primo, &secondo),
        "le due risoluzioni devono condividere lo stesso pool, non due gemelli"
    );
}

/// Il caso realmente osservato sul cluster: connessioni nate nello stesso
/// istante sullo stesso database, cioe' piu' task che non trovano il pool e lo
/// aprono insieme. Togliere il doppio controllo dopo il lock (o il lock stesso)
/// fa salire il contatore e rossegga questo test.
#[tokio::test]
async fn aperture_concorrenti_producono_un_solo_pool() {
    let pid = Uuid::new_v4();
    let aperture = Arc::new(AtomicUsize::new(0));

    let mut task = Vec::new();
    for _ in 0..8 {
        let aperture = Arc::clone(&aperture);
        task.push(tokio::spawn(async move {
            pool_or_open(pid, || async {
                aperture.fetch_add(1, Ordering::SeqCst);
                // Cede il controllo: senza serializzazione gli altri task entrano
                // qui prima che il primo registri il suo pool.
                tokio::task::yield_now().await;
                Ok::<_, sqlx::Error>(pool_finto())
            })
            .await
            .expect("apertura concorrente")
        }));
    }

    let mut pool = Vec::new();
    for t in task {
        pool.push(t.await.expect("task non deve andare in panic"));
    }

    assert_eq!(
        aperture.load(Ordering::SeqCst),
        1,
        "otto task concorrenti hanno aperto piu' di un pool verso lo stesso database"
    );
    for p in &pool {
        assert!(Arc::ptr_eq(p, &pool[0]), "un task ha ricevuto un altro pool");
    }
}

/// Progetti diversi restano separati: il registro non deve collassare due
/// database in un pool solo.
#[tokio::test]
async fn progetti_diversi_hanno_pool_distinti() {
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let apri = || async { Ok::<_, sqlx::Error>(pool_finto()) };

    let pa = pool_or_open(a, apri).await.expect("pool A");
    let pb = pool_or_open(b, apri).await.expect("pool B");

    assert!(!Arc::ptr_eq(&pa, &pb), "due progetti condividono un pool");
}

/// Un'apertura fallita non lascia nulla in registro: il tentativo successivo
/// deve poter riprovare, altrimenti un DB temporaneamente giu' resterebbe
/// inutilizzabile per tutta la vita del processo.
#[tokio::test]
async fn apertura_fallita_non_registra_nulla() {
    let pid = Uuid::new_v4();

    let esito = pool_or_open(pid, || async { Err::<PgPool, _>(sqlx::Error::PoolClosed) }).await;

    assert!(esito.is_err(), "l'errore dell'apertura deve propagarsi");
    assert!(
        cached_pool(pid).is_none(),
        "un'apertura fallita non deve lasciare un pool in registro"
    );
}
