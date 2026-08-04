//! Discendenza di un run: quali altri run compongono il lavoro di un run.
//!
//! PUNTO UNICO (regola L) della domanda "questo run da chi e' stato generato".
//! Un sub-agente e' un run a se': ha `run_id` propri in `agent_runs`, in
//! `nexus_agent_traces` e nel ledger dei costi. Chiunque debba attribuire al run
//! padre il lavoro (token, costo, provider usati) dei suoi figli deve prima
//! sapere CHI sono, e quella parentela ha una sola fonte: la colonna
//! `nexus_subagent_runs.dispatcher_run_id` (mig project 0010), cioe' il run che
//! ha dispatchato il figlio.
//!
//! ## Il difetto che ha reso necessario il punto unico (26/07/2026)
//!
//! La barra costo-per-provider della chat dichiarava la ripartizione di un run e
//! ne ometteva un provider intero: 4 cicli di review su
//! `openrouter/z-ai/glm-4.7-flash` (21 iterazioni, $0.008453 registrati in
//! `nexus_subagent_runs.cost_usd`) non comparivano accanto a `deepseek`. Il
//! numero era sbagliato per costruzione, non approssimato.
//!
//! Il motivo: il frontend deduceva i figli dai META-STEP di narrazione
//! (`subagent_started`/`subagent_progress`, canale di presentazione emesso solo
//! quando il dispatch avviene DENTRO il grafo con la narrazione del padre). Il
//! review panel gira dietro la porta del ReviewGate e non narra: nessun
//! meta-step, quindi per la barra quei run non esistevano. La misura raggiungeva
//! il suo oggetto per una strada diversa da quella della produzione (regola O):
//! la parentela nasce nel DB, non nel canale di narrazione.
//!
//! ## Perche' `dispatcher_run_id` e non `parent_run_id`
//!
//! Le due colonne rispondono a domande diverse e vanno tenute distinte:
//!
//! - `parent_run_id` e' l'ANCORA di famiglia
//!   (`subagent_native::parent_anchor` = `parent_run_id.or(session_id)`): governa
//!   depth-chain e cost-cap, e per i sub-run dispatchati da un tool vale la
//!   SESSIONE, non un run. Misurato: 15 sub-run `implement` con
//!   `parent_run_id` = sessione e `dispatcher_run_id` = run reale.
//! - `dispatcher_run_id` e' il run CORRENTE che ha dispatchato il figlio: e'
//!   esattamente la parentela run -> run che serve per attribuire il lavoro.
//!
//! Le righe anteriori alla mig project 0010 non hanno `dispatcher_run_id`: per
//! quelle si ripiega su `parent_run_id`. In entrambi i casi il genitore viene
//! restituito SOLO se e' davvero un run della sessione: un'ancora che punta alla
//! sessione non e' una parentela e non deve viaggiare come tale.

use std::collections::HashMap;

use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Parentela run -> run dei sub-run di una sessione: `figlio -> padre`.
///
/// `run_pool` e' il pool del PROGETTO (dove vivono `agent_runs` e
/// `nexus_subagent_runs`), risolto dal chiamante — stessa convenzione di
/// [`crate::trace_store`]. `user_id` filtra come le altre letture di sessione:
/// nessun leak cross-utente.
///
/// Un errore DB propaga al chiamante: qui non c'e' fallback silenzioso (una
/// mappa vuota inventata farebbe sparire di nuovo i figli dalla ripartizione,
/// che e' il difetto da cui nasce questo modulo).
pub async fn parent_run_by_child(
    run_pool: &PgPool,
    session_id: Uuid,
    user_id: Uuid,
) -> Result<HashMap<Uuid, Uuid>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT s.id AS child_id, \
                COALESCE(s.dispatcher_run_id, s.parent_run_id) AS parent_id \
         FROM nexus_subagent_runs s \
         WHERE s.id IN (SELECT id FROM agent_runs WHERE session_id = $1 AND user_id = $2) \
           AND COALESCE(s.dispatcher_run_id, s.parent_run_id) \
               IN (SELECT id FROM agent_runs WHERE session_id = $1 AND user_id = $2)",
    )
    .bind(session_id)
    .bind(user_id)
    .fetch_all(run_pool)
    .await?;

    let mut out: HashMap<Uuid, Uuid> = HashMap::new();
    for row in rows {
        let child: Uuid = match row.try_get("child_id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let parent: Uuid = match row.try_get("parent_id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Un run non e' figlio di se stesso: la riga sarebbe una catena che non
        // termina per i consumatori che risalgono la parentela.
        if child != parent {
            out.insert(child, parent);
        }
    }
    Ok(out)
}

/// I run che compongono il lavoro di `run_id`: i suoi figli DIRETTI.
///
/// Verso opposto di [`parent_run_by_child`], stesso criterio di parentela
/// (`COALESCE(dispatcher_run_id, parent_run_id)`): due definizioni di "figlio"
/// darebbero due idee diverse di che cosa un run abbia fatto.
///
/// Serve a chi deve rispondere «questo run ha prodotto qualcosa?» guardando il
/// lavoro COMPLESSIVO. Un coordinatore che delega tutto ai sub-agenti non ha
/// step propri, e chi legge i soli `agent_steps` del suo `run_id` lo vede
/// inerte. MISURATO il 04/08/2026 su biblioteca-scolastica: il run di chat
/// aveva ZERO step propri mentre i suoi figli avevano scritto 29 file, e il
/// resoconto in chat diceva «Nessuna risposta utile prodotta dall'agente».
///
/// Non ricorsivo: i figli dei figli restano fuori. Un livello copre il caso che
/// conta (il coordinatore che delega) senza aprire la ricorsione su un grafo la
/// cui profondita' non e' vincolata; se domani servisse, e' un `WITH RECURSIVE`
/// da aggiungere QUI, non in un secondo posto.
pub async fn child_runs_of(run_pool: &PgPool, run_id: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT s.id AS child_id FROM nexus_subagent_runs s \
         WHERE COALESCE(s.dispatcher_run_id, s.parent_run_id) = $1 AND s.id <> $1",
    )
    .bind(run_id)
    .fetch_all(run_pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| r.try_get::<Uuid, _>("child_id").ok())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{insert_agent_run_as, seed_chat_session, seed_subagent_run};

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn il_revisore_e_figlio_del_run_che_lo_ha_convocato(pool: PgPool) {
        let user_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let session_id = seed_chat_session(&pool, project_id).await;
        let padre =
            insert_agent_run_as(&pool, session_id, project_id, user_id, "completed").await;
        let revisore = seed_subagent_run(
            &pool,
            session_id,
            project_id,
            user_id,
            padre,
            Some(padre),
            "review",
        )
        .await;

        let mappa = parent_run_by_child(&pool, session_id, user_id)
            .await
            .expect("lettura ok");
        assert_eq!(mappa.get(&revisore), Some(&padre));

        // Verso opposto, stesso criterio: il padre deve poter elencare il lavoro
        // che ha delegato.
        let figli = child_runs_of(&pool, padre).await.expect("lettura ok");
        assert_eq!(figli, vec![revisore], "il padre non ritrova il proprio figlio");
    }

    /// IL CASO MISURATO il 04/08/2026 su biblioteca-scolastica: un coordinatore
    /// che delega TUTTO non ha step propri, e chi guardava il solo `run_id` lo
    /// dichiarava inerte — «Nessuna risposta utile prodotta dall'agente» a
    /// fronte di 29 file scritti dai figli.
    ///
    /// MUTAZIONE: far ritornare `Ok(Vec::new())` a `child_runs_of` fa rosseggiare
    /// l'asserzione sui due figli, che e' esattamente il lavoro che spariva dal
    /// resoconto.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn il_coordinatore_che_delega_ritrova_il_lavoro_dei_figli(pool: PgPool) {
        let user_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let session_id = seed_chat_session(&pool, project_id).await;
        let coordinatore =
            insert_agent_run_as(&pool, session_id, project_id, user_id, "completed").await;
        let estraneo =
            insert_agent_run_as(&pool, session_id, project_id, user_id, "completed").await;

        for kind in ["implement", "test_author"] {
            seed_subagent_run(
                &pool,
                session_id,
                project_id,
                user_id,
                session_id, // ancora di famiglia: la sessione, non il run
                Some(coordinatore),
                kind,
            )
            .await;
        }
        // Un figlio di un ALTRO run non deve entrare nel lavoro di questo.
        seed_subagent_run(
            &pool,
            session_id,
            project_id,
            user_id,
            session_id,
            Some(estraneo),
            "review",
        )
        .await;

        let figli = child_runs_of(&pool, coordinatore).await.expect("lettura ok");
        assert_eq!(
            figli.len(),
            2,
            "il coordinatore deve ritrovare i due sub-run che ha dispatchato: {figli:?}"
        );
        let altrui = child_runs_of(&pool, estraneo).await.expect("lettura ok");
        assert_eq!(altrui.len(), 1, "e non quelli di un altro run: {altrui:?}");
    }

    /// Il caso MISURATO sul progetto e2e-todo: i sub-run dispatchati da un tool
    /// hanno `parent_run_id` = SESSIONE (l'ancora di famiglia) e
    /// `dispatcher_run_id` = il run reale. La parentela e' la seconda; l'ancora
    /// non e' un run e non deve comparire come padre.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn l_ancora_di_sessione_non_e_una_parentela(pool: PgPool) {
        let user_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let session_id = seed_chat_session(&pool, project_id).await;
        let padre =
            insert_agent_run_as(&pool, session_id, project_id, user_id, "completed").await;

        // Figlio con ancora = sessione ma dispatcher = run: la parentela c'e'.
        let con_dispatcher = seed_subagent_run(
            &pool,
            session_id,
            project_id,
            user_id,
            session_id,
            Some(padre),
            "implement",
        )
        .await;
        // Figlio con ancora = sessione e NESSUN dispatcher (riga anteriore alla
        // mig project 0010): non c'e' un run padre da dichiarare.
        let senza_dispatcher = seed_subagent_run(
            &pool,
            session_id,
            project_id,
            user_id,
            session_id,
            None,
            "review",
        )
        .await;

        let mappa = parent_run_by_child(&pool, session_id, user_id)
            .await
            .expect("lettura ok");
        assert_eq!(mappa.get(&con_dispatcher), Some(&padre));
        assert_eq!(
            mappa.get(&senza_dispatcher),
            None,
            "l'ancora di sessione non e' un run padre"
        );
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn nessun_leak_cross_utente(pool: PgPool) {
        let owner = Uuid::new_v4();
        let altro = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let session_id = seed_chat_session(&pool, project_id).await;
        let padre = insert_agent_run_as(&pool, session_id, project_id, owner, "completed").await;
        seed_subagent_run(
            &pool,
            session_id,
            project_id,
            owner,
            padre,
            Some(padre),
            "review",
        )
        .await;

        let mappa = parent_run_by_child(&pool, session_id, altro)
            .await
            .expect("lettura ok");
        assert!(mappa.is_empty(), "nessun leak cross-utente");
    }
}
