//! Confine I/O del punto unico
//! [`nexus_agent_graph::decisions::pagina_del_run`]: porta i FATTI con cui si
//! decide QUALE pagina misurare alla chiusura di un run, e non ne giudica
//! nessuno.
//!
//! I fatti sono due, e vengono da due posti diversi:
//!   - le SCRITTURE, dal registro `file_mutations` (DB META, mig 0349), scritte
//!     da [`crate::file_mutations::record_mutation`] con un percorso gia'
//!     relativo alla radice;
//!   - l'entry RILEVATA sull'albero, dal punto unico gia' in esercizio nel
//!     pannello Servizi ([`crate::static_preview::detect_static_entry`]), che
//!     qui e' il RIPIEGO per i progetti in cui nessuno ha scritto una pagina.
//!
//! IL MOMENTO E' IL PUNTO. Questi fatti si raccolgono quando il gate MISURA,
//! non quando il motore si costruisce: a t=0 l'albero di un progetto nuovo e'
//! vuoto (nessuna pagina, criterio che non nasce) e quello di un progetto vivo
//! porta il lavoro di IERI (pagina sbagliata, ciclo di correzione che non puo'
//! convergere). Entrambe le forme misurate l'11/08/2026; il razionale completo
//! sta nel modulo del criterio.
//!
//! PERIMETRO: la SESSIONE, non il solo run. Le scritture di un sub-run portano
//! il `run_id` del sub-run e la `session_id` del padre, quindi cercarle sotto il
//! solo run del padre renderebbe invisibile tutto il lavoro DELEGATO — e
//! ricadere sul rilevatore e' esattamente il difetto da chiudere. E' la stessa
//! ragione, e lo stesso confine, di
//! [`crate::agent_graph_adapter::mutation_progress`]. Chi ha scritto la pagina
//! resta comunque distinguibile: il campo `del_run` lo dichiara, e la
//! precedenza la applica il criterio.
//!
//! NESSUN FILTRO QUI. La porta non seleziona le pagine dal SQL: quale file sia
//! una pagina, e quale abbia la precedenza, e' il criterio — e un filtro
//! scritto in due encoding (qui una `LIKE`, li' `e_una_pagina`) divergerebbe al
//! primo formato aggiunto senza che nulla fallisca (regola L).
//!
//! IL VOCABOLARIO delle cartelle che non sono il sito viaggia coi fatti, e non
//! e' un'eccezione a quella regola: qui non si DECIDE nulla con quell'elenco, lo
//! si PRENDE dal suo unico proprietario
//! ([`crate::static_preview::CARTELLE_ESCLUSE`]) e lo si consegna al criterio,
//! che vive in un crate che non vede mcp-core. Il ponte che prova che l'elenco
//! REALE arriva fino al criterio sta qui sotto (regola O): senza, un nome
//! aggiunto a quella costante lascerebbe verdi i test del criterio, che il
//! vocabolario se lo riproducono a mano.

use std::path::Path;

use sqlx::{PgPool, Row};
use uuid::Uuid;

use nexus_agent_graph::decisions::pagina_del_run::{FattiPagina, ScritturaOsservata};

/// Tetto di righe lette dal registro. NON e' un campione ed e' generoso di
/// proposito: si prendono le piu' RECENTI (e si rimettono in ordine di
/// scrittura), quindi il taglio puo' solo perdere pagine scritte prima di
/// centinaia di altri file nella stessa sessione — nel qual caso il criterio
/// ricade sul rilevatore, che e' il comportamento storico e non un errore
/// nuovo.
const MAX_SCRITTURE: i64 = 2000;

/// Raccoglie i fatti per [`nexus_agent_graph::decisions::pagina_del_run::risolvi_pagina`].
///
/// `Err` = NON si e' potuto guardare il registro. Non si degrada al solo
/// rilevatore: quel ripiego silenzioso rimetterebbe in piedi il difetto
/// misurato — la pagina di ieri misurata al posto di quella di oggi — e lo
/// renderebbe indistinguibile dal caso buono. Chi chiama lo DICHIARA
/// (inconcludente), che e' il canale giusto per «non ho potuto guardare».
pub async fn fatti_pagina(
    meta_db: &PgPool,
    project_id: Uuid,
    session_id: Uuid,
    run_id: Uuid,
    root: &Path,
) -> Result<FattiPagina, String> {
    let righe = sqlx::query(
        "SELECT file_path, op, run_id \
           FROM file_mutations \
          WHERE project_id = $1 AND session_id = $2 \
          ORDER BY id DESC \
          LIMIT $3",
    )
    .bind(project_id)
    .bind(session_id)
    .bind(MAX_SCRITTURE)
    .fetch_all(meta_db)
    .await
    .map_err(|e| format!("registro delle scritture non interrogabile: {e}"))?;

    // Le righe arrivano dalla piu' recente: l'ordine che il criterio si aspetta
    // e' quello di SCRITTURA, e glielo deve dare chi lo conosce (l'id del
    // registro), non chi lo indovina dal contenuto.
    let scritture: Vec<ScritturaOsservata> = righe
        .into_iter()
        .rev()
        .filter_map(|r| {
            let path: String = r.try_get("file_path").ok()?;
            let op: String = r.try_get("op").unwrap_or_default();
            let scritto_da: Option<Uuid> = r.try_get("run_id").ok().flatten();
            Some(ScritturaOsservata {
                path,
                cancellata: op == crate::file_mutations::OP_CANCELLATO,
                del_run: scritto_da == Some(run_id),
            })
        })
        .collect();

    Ok(FattiPagina {
        scritture,
        entry_rilevata: crate::static_preview::detect_static_entry(&root.to_string_lossy()).await,
        // Delegato, mai ricopiato: il criterio scarta le STESSE cartelle che il
        // rilevatore qui sopra ignora.
        cartelle_escluse: crate::static_preview::CARTELLE_ESCLUSE
            .iter()
            .map(|c| (*c).to_string())
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use nexus_agent_graph::decisions::pagina_del_run::{
        risolvi_pagina, PaginaDaMisurare, ProvenienzaPagina,
    };

    use crate::file_mutations::{record_mutation, ScopeAudit};

    /// Scrive il file sull'albero E la riga nel registro, come fanno i tool di
    /// scrittura (`tool_write_file` chiama `record_mutation` subito prima di
    /// sovrascrivere). Il produttore e' quello di produzione: gli `op` non sono
    /// costanti scritte nel test, li deriva `record_mutation` dai contenuti
    /// (regola O).
    async fn scrivi_pagina(
        pool: &PgPool,
        project_id: Uuid,
        session_id: Uuid,
        user_id: Uuid,
        run_id: Option<Uuid>,
        root: &Path,
        rel: &str,
        contenuto: &str,
    ) {
        let assoluto = root.join(rel);
        if let Some(genitore) = assoluto.parent() {
            std::fs::create_dir_all(genitore).expect("mkdir");
        }
        let prima = std::fs::read_to_string(&assoluto).ok();
        std::fs::write(&assoluto, contenuto).expect("write");
        record_mutation(
            pool,
            project_id,
            Some(session_id),
            run_id,
            Some(user_id),
            rel,
            "write_file",
            prima.as_deref(),
            Some(contenuto),
            ScopeAudit::none(),
        )
        .await
        .expect("mutazione registrata");
    }

    /// IL CASO `verifica-fix-10-08`, nella sua forma esatta. A t=0 l'albero ha
    /// gia' `index.html` (la todo app del giorno prima, 1 elemento) e
    /// `test-todo.html`; il run produce `galleria.html`, che funziona.
    ///
    /// Il rilevatore, al suo primo passo, propone l'entry CANONICA — ed e'
    /// corretto per la SUA domanda («qual e' l'entry di questo sito?»). La
    /// domanda del gate e' un'altra, e la risposta e' la pagina che il run ha
    /// scritto.
    ///
    /// MUTAZIONE M1: ignorare le scritture del run (svuotare `scritture`, cioe'
    /// il comportamento precedente) fa tornare la scelta su `index.html` —
    /// misurato: `1 < min_elements=5`, «final_gate non superata, nuovo
    /// tentativo 1/2», poi «chiusa al limite tentativi» con cambio di provider
    /// e 254.938 token, su un ciclo che non poteva convergere perche'
    /// correggere `galleria.html` non fa crescere `index.html`.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_pagina_scritta_dal_run_vince_sull_entry_canonica(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let session_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let albero = tempfile::tempdir().expect("tempdir");
        let root = albero.path();

        // Lo stato dell'albero a t=0: il lavoro di IERI, che questa sessione
        // non ha scritto e che il registro non conosce.
        std::fs::write(root.join("index.html"), "<html><body><h1>x</h1></body></html>")
            .expect("write");
        std::fs::write(root.join("test-todo.html"), "<html></html>").expect("write");

        // Il lavoro di OGGI.
        scrivi_pagina(
            &pool,
            project_id,
            session_id,
            user_id,
            Some(run_id),
            root,
            "galleria.html",
            "<html><body><div class=\"card\"></div></body></html>",
        )
        .await;

        let fatti = fatti_pagina(&pool, project_id, session_id, run_id, root)
            .await
            .expect("fatti");
        assert_eq!(
            fatti.entry_rilevata.as_deref(),
            Some("index.html"),
            "il rilevatore risponde alla SUA domanda, e non e' un difetto suo"
        );
        assert_eq!(
            risolvi_pagina(None, &fatti),
            PaginaDaMisurare::Una {
                entry: "galleria.html".to_string(),
                provenienza: ProvenienzaPagina::ScrittaDalRun,
            }
        );

        // M1, esplicita: senza le scritture del run si torna a misurare la todo
        // app del giorno prima.
        let cieco = FattiPagina {
            scritture: Vec::new(),
            ..fatti
        };
        assert_eq!(
            risolvi_pagina(None, &cieco),
            PaginaDaMisurare::Una {
                entry: "index.html".to_string(),
                provenienza: ProvenienzaPagina::Rilevata,
            },
            "e' il difetto: la pagina di ieri misurata al posto di quella di oggi"
        );
    }

    /// IL CASO `test-11-08-listino`: progetto NUOVO e VUOTO. L'agente scrive
    /// `listino.html`, che non funziona, e il run chiude «task complete».
    ///
    /// MUTAZIONE M2: risolvere l'entry a t=0 — cioe' sull'albero com'era prima
    /// che il run scrivesse — non produce alcuna pagina, quindi alcun criterio,
    /// quindi nessuna misura: e' esattamente il silenzio osservato. Il test
    /// asserisce entrambi i momenti, cosi' che riportare la risoluzione a t=0
    /// faccia rosseggiare il secondo.
    ///
    /// Il nome del file NON c'entra: `detect_static_entry` ha un terzo passo che
    /// ripiega sul primo `.html` della radice, e a gate time trova `listino.html`
    /// da se'. Il difetto e' il MOMENTO.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn su_un_progetto_vuoto_la_pagina_esiste_solo_dopo_il_lavoro(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let session_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let albero = tempfile::tempdir().expect("tempdir");
        let root = albero.path();

        // M2: la risoluzione a t=0, sull'albero vuoto.
        let a_zero = fatti_pagina(&pool, project_id, session_id, run_id, root)
            .await
            .expect("fatti");
        assert_eq!(
            risolvi_pagina(None, &a_zero),
            PaginaDaMisurare::NessunaPagina,
            "a t=0 non c'e' niente da misurare: e' qui che il criterio non nasceva"
        );

        scrivi_pagina(
            &pool,
            project_id,
            session_id,
            user_id,
            Some(run_id),
            root,
            "listino.html",
            "<html><body><div id=\"productsGrid\"></div></body></html>",
        )
        .await;

        let alla_verifica = fatti_pagina(&pool, project_id, session_id, run_id, root)
            .await
            .expect("fatti");
        assert_eq!(
            risolvi_pagina(None, &alla_verifica),
            PaginaDaMisurare::Una {
                entry: "listino.html".to_string(),
                provenienza: ProvenienzaPagina::ScrittaDalRun,
            },
            "alla verifica la pagina c'e', ed e' quella che il run ha scritto"
        );
    }

    /// Il lavoro DELEGATO non si perde: un sub-run scrive col PROPRIO `run_id`
    /// e la `session_id` del padre. Col perimetro sul solo run, la pagina
    /// sarebbe invisibile e si ricadrebbe sul rilevatore — cioe' si rifarebbe
    /// il difetto, proprio nei run che delegano.
    ///
    /// MUTAZIONE: aggiungere `AND run_id = $run` alla query -> la pagina del
    /// sub-run sparisce e la scelta torna all'entry canonica.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_pagina_di_un_sub_run_resta_nel_perimetro(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let session_id = Uuid::new_v4();
        let dispatcher = Uuid::new_v4();
        let figlio = Uuid::new_v4();
        let albero = tempfile::tempdir().expect("tempdir");
        let root = albero.path();
        std::fs::write(root.join("index.html"), "<html></html>").expect("write");

        scrivi_pagina(
            &pool,
            project_id,
            session_id,
            user_id,
            Some(figlio),
            root,
            "catalogo.html",
            "<html><body><ul></ul></body></html>",
        )
        .await;

        let fatti = fatti_pagina(&pool, project_id, session_id, dispatcher, root)
            .await
            .expect("fatti");
        assert_eq!(
            risolvi_pagina(None, &fatti),
            PaginaDaMisurare::Una {
                entry: "catalogo.html".to_string(),
                provenienza: ProvenienzaPagina::ScrittaNellaSessione,
            }
        );
    }

    /// Il confine e' la SESSIONE: il lavoro di un'altra sessione sullo stesso
    /// progetto non e' il lavoro di questo run. E' il rovescio della scelta di
    /// non filtrare per run — larga abbastanza da contenere i sub-run, non
    /// abbastanza da contenere un altro lavoro.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn le_pagine_di_un_altra_sessione_non_entrano(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let mia = Uuid::new_v4();
        let altrui = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let albero = tempfile::tempdir().expect("tempdir");
        let root = albero.path();
        std::fs::write(root.join("index.html"), "<html></html>").expect("write");

        scrivi_pagina(
            &pool,
            project_id,
            altrui,
            user_id,
            Some(Uuid::new_v4()),
            root,
            "vecchia.html",
            "<html></html>",
        )
        .await;

        let fatti = fatti_pagina(&pool, project_id, mia, run_id, root)
            .await
            .expect("fatti");
        assert!(
            fatti.scritture.is_empty(),
            "le scritture di un'altra sessione non sono fatti di questa"
        );
        assert_eq!(
            risolvi_pagina(None, &fatti),
            PaginaDaMisurare::Una {
                entry: "index.html".to_string(),
                provenienza: ProvenienzaPagina::Rilevata,
            }
        );
    }

    /// IL PONTE FRA IL VOCABOLARIO E IL CRITERIO (regola O). L'elenco delle
    /// cartelle che non sono il sito ha UN proprietario
    /// (`static_preview::CARTELLE_ESCLUSE`) e un lettore che vive in un altro
    /// crate, dove nei test se lo riproduce a mano: senza questo attraversamento
    /// un nome aggiunto alla costante lascerebbe verdi i test del criterio su un
    /// criterio che quel nome non conosce.
    ///
    /// Qui la pagina la scrive il produttore REALE dentro una dipendenza, e la
    /// scelta deve ignorarla: una `node_modules/.../index.html` e' una scrittura
    /// legittima del run — un file corretto a mano — e non e' il sito che
    /// qualcuno pubblichera'.
    ///
    /// MUTAZIONE: svuotare `cartelle_escluse` nella porta e la pagina della
    /// dipendenza a profondita' uno diventa il bersaglio del gate.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_vocabolario_delle_cartelle_escluse_arriva_al_criterio(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let session_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let albero = tempfile::tempdir().expect("tempdir");
        let root = albero.path();

        scrivi_pagina(
            &pool,
            project_id,
            session_id,
            user_id,
            Some(run_id),
            root,
            "vetrina.html",
            "<html><body><div class=\"card\"></div></body></html>",
        )
        .await;
        // Una pagina dentro una dipendenza, alla profondita' che il vincolo da
        // solo non basta a escludere: qui decide il vocabolario.
        scrivi_pagina(
            &pool,
            project_id,
            session_id,
            user_id,
            Some(run_id),
            root,
            "node_modules/esempio.html",
            "<html></html>",
        )
        .await;

        let fatti = fatti_pagina(&pool, project_id, session_id, run_id, root)
            .await
            .expect("fatti");
        assert_eq!(
            fatti.cartelle_escluse.len(),
            crate::static_preview::CARTELLE_ESCLUSE.len(),
            "il vocabolario arriva dal suo unico proprietario, per intero"
        );
        assert!(
            fatti.scritture.iter().any(|s| s.path.contains("node_modules")),
            "la porta non filtra: la scrittura c'e', ed e' il criterio a scartarla"
        );
        assert_eq!(
            risolvi_pagina(None, &fatti),
            PaginaDaMisurare::Una {
                entry: "vetrina.html".to_string(),
                provenienza: ProvenienzaPagina::ScrittaDalRun,
            },
            "una pagina dentro una dipendenza non e' il sito del progetto"
        );
    }

    /// Una pagina CANCELLATA dal run non e' un candidato, e il fatto arriva dal
    /// produttore reale: l'`op` la deriva `record_mutation` dai contenuti
    /// (`after = None` -> cancellazione), non il test.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn una_pagina_cancellata_dal_run_non_e_un_candidato(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let session_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let albero = tempfile::tempdir().expect("tempdir");
        let root = albero.path();
        std::fs::write(root.join("index.html"), "<html></html>").expect("write");

        scrivi_pagina(
            &pool,
            project_id,
            session_id,
            user_id,
            Some(run_id),
            root,
            "bozza.html",
            "<html></html>",
        )
        .await;
        // La rimozione, col produttore reale: `after_content = None`.
        std::fs::remove_file(root.join("bozza.html")).expect("rm");
        record_mutation(
            &pool,
            project_id,
            Some(session_id),
            Some(run_id),
            Some(user_id),
            "bozza.html",
            "write_file",
            Some("<html></html>"),
            None,
            ScopeAudit::none(),
        )
        .await
        .expect("mutazione registrata");

        let fatti = fatti_pagina(&pool, project_id, session_id, run_id, root)
            .await
            .expect("fatti");
        assert!(
            fatti.scritture.iter().any(|s| s.cancellata),
            "la cancellazione e' un fatto e la porta lo riporta: e' il criterio a scartarla"
        );
        assert_eq!(
            risolvi_pagina(None, &fatti),
            PaginaDaMisurare::Una {
                entry: "index.html".to_string(),
                provenienza: ProvenienzaPagina::Rilevata,
            }
        );
    }
}
