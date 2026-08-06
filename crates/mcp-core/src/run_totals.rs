//! «Quanto lavoro ha fatto DAVVERO un run che nessuno ha chiuso?» — il punto
//! unico dei totali ricavati dai FATTI PERSISTITI (regola L).
//!
//! Un run che chiude normalmente consolida i propri totali dal finalizzatore:
//! `apply_ledger_reconciliation` legge il ledger, `mark_run` scrive la riga. Un
//! run che muore da FUORI — il servizio riavviato, il task tokio sparito, il
//! reaper che trova una riga senza battito — non passa da nessuno dei due, e la
//! sua riga resta ai valori iniziali. Che sono zero.
//!
//! MISURATO il 06/08/2026 sul DB di agenda-medica: i 2 run `interrupted` hanno
//! entrambi `total_tokens = 0`, `total_cost = 0`, `iteration_count = 0`; uno
//! (`6ab05037`) aveva 107 passi persistiti, 99 iterazioni deducibili, e 87 righe
//! di ledger FINALIZZATE per 4.899.738 token e $0.0363. La riga diceva che quel
//! run non era mai partito, il ledger che era il piu' caro della giornata. Non
//! e' una spesa persa — il ledger e' la fonte autoritativa e la porta — e'
//! l'unica vista che l'utente ha del run che mente, e mente verso il basso:
//! e' esattamente il segnale con cui si decide se un run e' costato troppo.
//!
//! I fatti stanno in DUE database e nessuna JOIN li unisce (pool separati, crate
//! `nexus-project-pools`): il consumo nel ledger (META), i passi in `agent_steps`
//! (progetto). L'unione la fa questo modulo, in un punto solo, perche' era gia'
//! sul punto di diventare la TERZA copia dell'inversa `step_index`: la formula
//! girava in `mark_run` (sub-run morti senza outcome) e in
//! `native_recursion_limit_result` (tetto superstep), e il reaper stava per
//! aggiungerne una sua. La costante `STEP_INDEX_STRIDE` era gia' condivisa; la
//! formula che la usa non lo era.
//!
//! Cosa NON fa: non stima. Se il ledger non ha righe per il run, i token restano
//! come sono — un run senza contabilita' non ne guadagna una qui (la stima dal
//! catalog e' un'altra decisione, presa altrove e per un altro caso).

use sqlx::PgPool;
use uuid::Uuid;

/// L'inversa della convenzione `step_index = iteration * STRIDE + idx`, come
/// FRAMMENTO SQL: l'ultima iterazione che ha lasciato un passo persistito.
///
/// Esiste come frammento e non solo come funzione perche' un consumatore
/// (`mark_run`) la compone dentro una CTE che aggiorna due tabelle in una sola
/// statement: estrarla in Rust gli costerebbe l'atomicita'. Il `bind_run_id` e'
/// il segnaposto del run nella statement ospite (`$1`, `$7`, ...), che ogni
/// chiamante numera a modo suo.
///
/// `COALESCE(.., 0)`: nessun passo persistito -> 0, e li' lo zero e' vero.
pub(crate) fn sql_ultima_iterazione(bind_run_id: &str) -> String {
    format!(
        "(SELECT COALESCE(MAX(step_index) / {stride}, 0) \
           FROM agent_steps WHERE run_id = {bind_run_id})",
        stride = nexus_agent_graph::runtime::ports::STEP_INDEX_STRIDE,
    )
}

/// Le iterazioni che un run ha effettivamente fatto, dai passi persistiti.
///
/// `None` = «non ho potuto guardare» (DB progetto irraggiungibile, query
/// respinta), che NON e' `Some(0)` = «non ha lasciato passi». I due casi
/// portano a decisioni diverse — il primo non autorizza a scrivere niente — e
/// un `unwrap_or(0)` li appiattirebbe sul secondo, che e' quello che il difetto
/// diceva gia' da solo.
pub(crate) async fn iterazioni_persistite(pool: &PgPool, run_id: Uuid) -> Option<i64> {
    // Lo stesso frammento che `mark_run` compone nella sua CTE, qui come
    // sub-select di una statement minima: la formula resta una sola.
    //
    // Il detector SQL-injection (ADR 0021) segnala questa riga, e il suo criterio
    // e' per-riga: vede una `format!` con una keyword SQL e non vede il `.bind()`
    // due righe sotto. Nella stringa entrano SOLO una costante `i64` del codice
    // (`STEP_INDEX_STRIDE`) e il letterale `"$1"`; l'unico valore, `run_id`, passa
    // dal bind. La composizione dinamica non e' eliminabile senza duplicare la
    // formula: i due chiamanti numerano i propri segnaposto in modo diverso
    // (`$1` qui, `$7` dentro la CTE di `mark_run`), ed e' precisamente la
    // duplicazione che questo modulo esiste per togliere.
    let sql = format!("SELECT {}::int8", sql_ultima_iterazione("$1"));
    sqlx::query_scalar::<_, i64>(&sql)
        .bind(run_id)
        .fetch_one(pool)
        .await
        .inspect_err(|e| {
            tracing::warn!(
                run_id = %run_id, error = %e,
                "totali del run: iterazioni non leggibili dai passi persistiti"
            );
        })
        .ok()
}

/// Cio' che si e' potuto misurare di un run chiuso da fuori. Ogni campo e'
/// `Option` per lo stesso motivo (regola Q): il fatto assente non degrada a
/// zero, perche' zero e' anche un valore legittimo e i due non si confondono.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct TotaliDaiFatti {
    /// Dal ledger (META), somma delle righe FINALIZZATE. `None` = nessuna riga:
    /// il gateway non ha contabilizzato questo run.
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub total_cost: Option<f64>,
    /// Da `agent_steps` (progetto).
    pub iterazioni: Option<i64>,
}

impl TotaliDaiFatti {
    /// True se c'e' almeno un fatto da scrivere: senza, l'UPDATE non parte.
    pub fn ha_qualcosa(&self) -> bool {
        self.total_tokens.is_some() || self.iterazioni.is_some()
    }
}

/// Raccoglie i fatti dai due database. Best-effort su entrambi i lati: un DB
/// che non risponde lascia `None` nei suoi campi, mai uno zero inventato.
pub(crate) async fn raccogli_totali(
    meta: &PgPool,
    pool_progetto: &PgPool,
    run_id: Uuid,
) -> TotaliDaiFatti {
    let ledger = crate::chat_messages::agent_run::fetch_ledger_totals(meta, run_id).await;
    let contabilizzato = ledger.rows > 0;
    TotaliDaiFatti {
        prompt_tokens: contabilizzato.then_some(ledger.prompt_tokens),
        completion_tokens: contabilizzato.then_some(ledger.completion_tokens),
        total_tokens: contabilizzato.then_some(ledger.total_tokens),
        total_cost: contabilizzato.then_some(ledger.total_cost),
        iterazioni: iterazioni_persistite(pool_progetto, run_id).await,
    }
}

/// Scrive i fatti su `agent_runs`, SOLO dove la riga dice ancora zero.
///
/// Lo zero e' il discriminante perche' li' non e' una misura: e' l'assenza di
/// consolidamento (il valore iniziale della riga). Dove invece un totale c'e'
/// gia', viene da chi ha chiuso il run sapendo cosa aveva fatto, ed e' piu'
/// informato di questa ricostruzione — un `SET` incondizionato lo
/// sovrascriverebbe con la somma delle sole chiamate che il gateway ha potuto
/// attribuire al run.
///
/// Idempotente per costruzione: la seconda esecuzione non trova piu' zeri.
pub(crate) async fn applica_totali(
    pool_progetto: &PgPool,
    run_id: Uuid,
    t: TotaliDaiFatti,
) -> Result<u64, sqlx::Error> {
    if !t.ha_qualcosa() {
        return Ok(0);
    }
    sqlx::query(
        "UPDATE agent_runs SET
            prompt_tokens     = CASE WHEN COALESCE(prompt_tokens, 0) = 0
                                     THEN COALESCE($2, prompt_tokens) ELSE prompt_tokens END,
            completion_tokens = CASE WHEN COALESCE(completion_tokens, 0) = 0
                                     THEN COALESCE($3, completion_tokens) ELSE completion_tokens END,
            total_tokens      = CASE WHEN COALESCE(total_tokens, 0) = 0
                                     THEN COALESCE($4, total_tokens) ELSE total_tokens END,
            total_cost        = CASE WHEN COALESCE(total_cost, 0) = 0
                                     THEN COALESCE($5, total_cost) ELSE total_cost END,
            iteration_count   = CASE WHEN COALESCE(iteration_count, 0) = 0
                                     THEN COALESCE($6, iteration_count) ELSE iteration_count END
          WHERE id = $1",
    )
    .bind(run_id)
    .bind(t.prompt_tokens.map(|v| v.clamp(0, i32::MAX as i64) as i32))
    .bind(t.completion_tokens.map(|v| v.clamp(0, i32::MAX as i64) as i32))
    .bind(t.total_tokens.map(|v| v.clamp(0, i32::MAX as i64) as i32))
    .bind(t.total_cost)
    .bind(t.iterazioni.map(|v| v.clamp(0, i32::MAX as i64) as i32))
    .execute(pool_progetto)
    .await
    .map(|r| r.rows_affected())
}

/// Raccoglie e scrive: il gesto completo per UN run chiuso da fuori.
/// Best-effort e MAI muto — un consolidamento respinto lascerebbe la riga a
/// zero senza che nessuno lo sappia, che e' il difetto di partenza.
pub(crate) async fn consolida_run(meta: &PgPool, pool_progetto: &PgPool, run_id: Uuid) {
    let totali = raccogli_totali(meta, pool_progetto, run_id).await;
    match applica_totali(pool_progetto, run_id, totali).await {
        Ok(n) if n > 0 => tracing::info!(
            run_id = %run_id,
            total_tokens = ?totali.total_tokens,
            total_cost = ?totali.total_cost,
            iterazioni = ?totali.iterazioni,
            "totali del run consolidati dai fatti persistiti (ledger + passi)"
        ),
        Ok(_) => {}
        Err(e) => tracing::warn!(
            run_id = %run_id, error = %e,
            "totali del run: consolidamento respinto, la riga resta a zero"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Semina un run con passi fino a `step_index`, come li scrive
    /// `PgAgentStepStore` (colonne reali dal migrator del set project).
    async fn run_con_passi(pool: &PgPool, indici: &[i64]) -> Uuid {
        let run = crate::test_support::seed_agent_run(pool).await;
        for i in indici {
            sqlx::query(
                "INSERT INTO agent_steps (id, run_id, step_index, tool_name, tool_input, \
                 tool_result, status, created_at) \
                 VALUES (gen_random_uuid(), $1, $2, 'run_command', '{}'::jsonb, 'ok', \
                 'completed', NOW())",
            )
            .bind(run)
            .bind(*i as i32)
            .execute(pool)
            .await
            .expect("seed agent_steps");
        }
        run
    }

    async fn riga(pool: &PgPool, run: Uuid) -> (i32, i32, f64) {
        sqlx::query_as::<_, (i32, i32, f64)>(
            "SELECT total_tokens, iteration_count, total_cost::float8 \
               FROM agent_runs WHERE id = $1",
        )
        .bind(run)
        .fetch_one(pool)
        .await
        .expect("rilettura agent_runs")
    }

    /// Il fatto misurato: 107 passi fino a step_index 99000 = 99 iterazioni.
    /// MUTAZIONE: togliere la divisione per lo stride in `sql_ultima_iterazione`
    /// -> 99000, il numero che il difetto reale avrebbe prodotto al contrario
    /// (il run reale ne dichiarava 0 avendone fatte 99).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn le_iterazioni_vengono_dai_passi_persistiti(pool: PgPool) {
        let run = run_con_passi(&pool, &[0, 1, 23_000, 99_000, 99_001]).await;
        assert_eq!(iterazioni_persistite(&pool, run).await, Some(99));
    }

    /// Un run senza passi: zero E' la risposta, e si distingue dal non aver
    /// potuto guardare (che sarebbe `None`).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn nessun_passo_e_uno_zero_vero(pool: PgPool) {
        let run = crate::test_support::seed_agent_run(&pool).await;
        assert_eq!(iterazioni_persistite(&pool, run).await, Some(0));
    }

    /// Il caso del difetto: riga a zero, fatti disponibili -> la riga li prende.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn i_totali_a_zero_prendono_i_fatti(pool: PgPool) {
        let run = run_con_passi(&pool, &[99_000]).await;
        let t = TotaliDaiFatti {
            prompt_tokens: Some(4_800_000),
            completion_tokens: Some(99_738),
            total_tokens: Some(4_899_738),
            total_cost: Some(0.0363),
            iterazioni: Some(99),
        };
        assert_eq!(applica_totali(&pool, run, t).await.expect("update"), 1);
        let (tok, iter, cost) = riga(&pool, run).await;
        assert_eq!(tok, 4_899_738);
        assert_eq!(iter, 99);
        assert!((cost - 0.0363).abs() < 1e-9, "costo scritto: {cost}");
    }

    /// MUTAZIONE dell'invariante opposta: un totale gia' consolidato da chi ha
    /// chiuso il run normalmente NON viene sostituito da questa ricostruzione.
    /// Togliere il `CASE WHEN ... = 0` -> il valore buono viene sovrascritto.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn un_totale_gia_scritto_non_viene_sovrascritto(pool: PgPool) {
        let run = run_con_passi(&pool, &[5_000]).await;
        sqlx::query(
            "UPDATE agent_runs SET total_tokens = 777, iteration_count = 12, \
             total_cost = 1.5 WHERE id = $1",
        )
        .bind(run)
        .execute(&pool)
        .await
        .expect("consolidamento preesistente");
        let t = TotaliDaiFatti {
            total_tokens: Some(1),
            total_cost: Some(0.001),
            iterazioni: Some(5),
            ..Default::default()
        };
        applica_totali(&pool, run, t).await.expect("update");
        let (tok, iter, cost) = riga(&pool, run).await;
        assert_eq!(tok, 777, "il totale gia' consolidato e' stato sovrascritto");
        assert_eq!(iter, 12);
        assert!((cost - 1.5).abs() < 1e-9);
    }

    /// Niente da scrivere -> nessuna statement (e nessuna riga toccata).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn senza_fatti_non_si_scrive(pool: PgPool) {
        let run = crate::test_support::seed_agent_run(&pool).await;
        let vuoto = TotaliDaiFatti::default();
        assert!(!vuoto.ha_qualcosa());
        assert_eq!(applica_totali(&pool, run, vuoto).await.expect("noop"), 0);
    }
}
