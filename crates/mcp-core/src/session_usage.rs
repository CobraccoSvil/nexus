//! «Quanto e' costato cio' che il contatore di chat sta mostrando?»
//!
//! PUNTO UNICO (regola L) del PERIMETRO contabile del contatore sotto la chat:
//! quali run compongono l'insieme di cui si dichiara token e costo. La SOMMA su
//! un insieme di run l'ha gia' il suo punto unico (`nexus_ledger::usage_for_runs`);
//! qui si stabilisce SU CHE COSA sommare, e lo si stabilisce in un posto solo.
//!
//! ## Il difetto da cui nasce (MISURATO l'08/08/2026 su gestione-corsi)
//!
//! Il contatore mostrava `639 token - $2.14` mentre il ledger, per la stessa
//! sessione, aveva 758 righe finalizzate per **27.813.580 token e $2,6024**. I
//! token erano lo 0,0023% del reale e il costo andava nella direzione opposta:
//! non due sviste, due letture di INSIEMI DIVERSI presentate come una coppia
//! coerente.
//!
//! La causa non era una formula sbagliata: erano QUATTRO produttori che
//! scrivevano lo stesso contatore con quattro perimetri diversi, e uno solo
//! leggeva il ledger.
//!
//! | canale | perimetro che portava |
//! |---|---|
//! | `GET /api/billing/session-usage` (questo) | sessione, dal ledger |
//! | evento SSE `agent_usage` | il TURNO (`executor.rs`, che lo dichiara) |
//! | evento `ChatMessageAdded` | il turno singolo di chat non agentica |
//! | evento `ChatSessionCompacted` | somma dei metadata dei MESSAGGI |
//!
//! Gli ultimi tre erano *segnali di avanzamento*, non misure: ora innescano una
//! rilettura di questa fonte invece di scrivere un numero (vedi `use-chat.ts`).
//!
//! ## I due perimetri, e perche' non e' lo stesso insieme
//!
//! - [`Perimetro::Sessione`] risponde «quanto e' costata questa conversazione».
//!   E' la domanda gia' chiusa il 06/08/2026 spostando l'endpoint dai metadata
//!   dei messaggi al ledger: i messaggi portano il costo del solo run PRINCIPALE
//!   del turno, mentre il lavoro DELEGATO gira su sub-run con `run_id` propri.
//! - [`Perimetro::RunConDiscendenza`] risponde «quanto e' costato QUESTO run»,
//!   che e' la domanda su cui si decide se un run e' costato troppo. Sullo stesso
//!   istante misurato sopra i due valgono $2,6024 e $0,1272: venti volte, quindi
//!   non sono intercambiabili e il consumatore deve dichiarare quale sta usando.
//!
//! ## La discendenza non si deduce dalla sessione
//!
//! Per la sessione basta `agent_runs.session_id`: la gemella `agent_runs` di un
//! sub-run porta la `session_id` del padre, quindi chiedere alla sessione i suoi
//! run e' gia' la domanda completa. Per un RUN no: quella parentela ha una fonte
//! sola, `nexus_subagent_runs.dispatcher_run_id`, e il suo punto unico e'
//! [`crate::run_lineage`], a cui qui si delega. Dedurla dai meta-step di
//! narrazione e' il difetto gia' documentato la' (il review panel non narra).

use sqlx::PgPool;
use uuid::Uuid;

/// L'insieme di run di cui il contatore dichiara il consumo.
///
/// E' un enum e non due funzioni sciolte perche' il perimetro va DICHIARATO da
/// chi legge: un numero che non dice a quale insieme si riferisce e' esattamente
/// cio' che ha reso il difetto invisibile per mesi.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Perimetro {
    /// Tutti i run della conversazione, sub-run inclusi.
    Sessione(Uuid),
    /// Un run e il lavoro che ha delegato (figli diretti).
    RunConDiscendenza(Uuid),
}

/// I run che compongono il perimetro, sul pool del PROGETTO (dove vivono
/// `agent_runs` e `nexus_subagent_runs`).
///
/// Il chiamante passa POI questo stesso elenco sia al totale sia alla
/// ripartizione: due derivazioni diverse darebbero un elenco che non somma al
/// totale che gli sta sopra (la nota e' gia' su `usage_by_model_for_runs`).
///
/// Un errore DB propaga: un elenco vuoto inventato produrrebbe un onesto
/// «$0,0000» al posto di un guasto, che e' il fallback silenzioso vietato dalla
/// regola G.
pub(crate) async fn run_ids_del_perimetro(
    run_pool: &PgPool,
    perimetro: Perimetro,
) -> Result<Vec<Uuid>, sqlx::Error> {
    match perimetro {
        Perimetro::Sessione(session_id) => {
            sqlx::query_scalar("SELECT id FROM agent_runs WHERE session_id = $1")
                .bind(session_id)
                .fetch_all(run_pool)
                .await
        }
        Perimetro::RunConDiscendenza(run_id) => {
            let mut ids = vec![run_id];
            ids.extend(crate::run_lineage::child_runs_of(run_pool, run_id).await?);
            Ok(ids)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{insert_agent_run_as, seed_chat_session, seed_subagent_run};

    /// Il perimetro di SESSIONE prende tutti i run, sub-run compresi: e' la
    /// proprieta' su cui poggia il fix del 06/08 (i sub-run portano la
    /// `session_id` del padre, quindi non serve risalire la discendenza).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn la_sessione_comprende_i_sub_run(pool: PgPool) {
        let user_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let session_id = seed_chat_session(&pool, project_id).await;
        let padre = insert_agent_run_as(&pool, session_id, project_id, user_id, "completed").await;
        let figlio = seed_subagent_run(
            &pool,
            session_id,
            project_id,
            user_id,
            session_id,
            Some(padre),
            "review",
        )
        .await;

        let ids = run_ids_del_perimetro(&pool, Perimetro::Sessione(session_id))
            .await
            .expect("lettura ok");
        assert!(ids.contains(&padre), "manca il run principale: {ids:?}");
        assert!(ids.contains(&figlio), "manca il sub-run: {ids:?}");
    }

    /// Il perimetro di RUN comprende il run stesso e cio' che ha delegato, e NON
    /// il lavoro di un altro run della stessa sessione.
    ///
    /// MUTAZIONE: togliere l'`extend` con `child_runs_of` fa rosseggiare
    /// l'asserzione sul figlio — cioe' fa sparire dal costo del run tutto il
    /// lavoro delegato, che e' il 72% misurato il 06/08 su agenda-medica.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn il_run_comprende_la_sua_discendenza_e_non_quella_altrui(pool: PgPool) {
        let user_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let session_id = seed_chat_session(&pool, project_id).await;
        let mio = insert_agent_run_as(&pool, session_id, project_id, user_id, "running").await;
        let altrui = insert_agent_run_as(&pool, session_id, project_id, user_id, "completed").await;
        let mio_figlio = seed_subagent_run(
            &pool,
            session_id,
            project_id,
            user_id,
            session_id,
            Some(mio),
            "implement",
        )
        .await;
        let figlio_altrui = seed_subagent_run(
            &pool,
            session_id,
            project_id,
            user_id,
            session_id,
            Some(altrui),
            "review",
        )
        .await;

        let ids = run_ids_del_perimetro(&pool, Perimetro::RunConDiscendenza(mio))
            .await
            .expect("lettura ok");
        assert!(ids.contains(&mio), "manca il run stesso: {ids:?}");
        assert!(
            ids.contains(&mio_figlio),
            "il lavoro delegato deve entrare nel costo del run: {ids:?}"
        );
        assert!(
            !ids.contains(&figlio_altrui),
            "il figlio di un altro run non e' lavoro di questo: {ids:?}"
        );
    }

    /// Un run senza deleghe e' il perimetro di se stesso: nessun elenco vuoto,
    /// che a valle diventerebbe un «$0,0000» indistinguibile da un run gratis.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn un_run_senza_deleghe_resta_il_perimetro_di_se_stesso(pool: PgPool) {
        let user_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let session_id = seed_chat_session(&pool, project_id).await;
        let solo = insert_agent_run_as(&pool, session_id, project_id, user_id, "running").await;

        let ids = run_ids_del_perimetro(&pool, Perimetro::RunConDiscendenza(solo))
            .await
            .expect("lettura ok");
        assert_eq!(ids, vec![solo]);
    }
}
