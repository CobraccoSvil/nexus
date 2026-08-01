//! Memoria degli esiti di suite, sulla tabella `jobs` del DB-progetto.
//!
//! Non c'e' una tabella nuova di proposito: il registro degli esiti esisteva
//! gia' — ogni run Playwright scrive la sua riga, il pannello la mostra — e
//! quello che gli mancava era la CHIAVE che dice a quale stato del codice
//! quell'esito appartiene (colonne `state_key`/`suite_key`, migrazione project
//! 0014). Una seconda tabella avrebbe creato un secondo posto in cui leggere la
//! stessa cosa, e prima o poi i due sarebbero divergiti (regola L).
//!
//! Questo modulo e' anche l'UNICO punto in cui l'esito FINALE viene scritto
//! sulla riga: il runner registra cio' che ha misurato, ma `flaky` nasce DOPO,
//! dalla riesecuzione mirata. Scriverlo in due posti significherebbe una riga
//! che dice `failed` e un chiamante che dice `flaky`.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::{EsitoMemorizzato, SuiteMemo, SuiteOutcome, SuiteStats};

/// I conteggi che il runner ha scritto in `jobs.progress` alla finalizzazione:
/// un esito riusato porta con se' quanti test erano passati e quanti falliti,
/// invece di presentarsi con degli zeri che il chiamante leggerebbe come
/// "suite vuota".
/// Legge un array JSON di stringhe (nomi di test), vuoto se assente o di forma
/// diversa. Le due liste che la memoria rilegge — test instabili e spec fallite
/// — hanno la stessa forma e la stessa domanda.
fn elenco_di_stringhe(v: Option<&serde_json::Value>) -> Vec<String> {
    v.and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn conteggi_dal_progress(progress: &serde_json::Value) -> SuiteStats {
    let numero = |chiave: &str| -> usize {
        progress
            .get(chiave)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize
    };
    SuiteStats {
        passed: numero("passed"),
        failed: numero("failed"),
        skipped: numero("skipped"),
        flaky_reported: numero("flaky"),
        failed_tests: elenco_di_stringhe(progress.get("failed_specs")),
    }
}

/// Memoria su `jobs` del DB del progetto.
pub struct PgSuiteMemo {
    pool: PgPool,
    project_id: Uuid,
}

impl PgSuiteMemo {
    /// `pool` e' quello del DB di PROGETTO (dove vive `jobs`), non il meta.
    pub fn new(pool: PgPool, project_id: Uuid) -> Self {
        Self { pool, project_id }
    }

    /// L'ultima riga riusabile per (suite, stato) entro il TTL, con la sua
    /// eta'.
    ///
    /// `status <> 'running'`: una suite IN CORSO non e' un esito, e riusarla
    /// direbbe "verificato" su una verifica che non e' finita. L'eta' la calcola
    /// il DB da `updated_at`, non il processo: due orologi darebbero due eta'
    /// diverse per la stessa riga.
    async fn riga_riusabile(
        &self,
        suite_key: &str,
        state_key: &str,
        ttl: Duration,
    ) -> Option<(Uuid, serde_json::Value, serde_json::Value, f64)> {
        sqlx::query_as::<_, (Uuid, serde_json::Value, serde_json::Value, f64)>(
            "SELECT id, input, progress, EXTRACT(EPOCH FROM (now() - updated_at))::float8 \
             FROM jobs \
             WHERE project_id = $1 AND kind = 'playwright_test' \
               AND suite_key = $2 AND state_key = $3 \
               AND status <> 'running' \
               AND updated_at > now() - make_interval(secs => $4::double precision) \
             ORDER BY updated_at DESC LIMIT 1",
        )
        .bind(self.project_id)
        .bind(suite_key)
        .bind(state_key)
        .bind(ttl.as_secs().max(1) as f64)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
    }
}

#[async_trait]
impl SuiteMemo for PgSuiteMemo {
    async fn cerca(
        &self,
        suite_key: &str,
        state_key: &str,
        ttl: Duration,
    ) -> Option<EsitoMemorizzato> {
        let (job_id, input, progress, eta_s) =
            self.riga_riusabile(suite_key, state_key, ttl).await?;
        let stats = conteggi_dal_progress(&progress);
        // L'esito si legge dal campo canonico, non dallo `status`: `status` e'
        // la resa per il pannello (tre valori), `outcome` e' il vocabolario
        // (quattro). Un `outcome` assente o ignoto NON si interpreta: si
        // riesegue (`None`).
        let outcome = input
            .get("outcome")
            .and_then(serde_json::Value::as_str)
            .and_then(SuiteOutcome::da_str)?;
        let messaggio = input
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let test_instabili = elenco_di_stringhe(input.get("flaky_tests"));

        Some(EsitoMemorizzato {
            job_id,
            outcome,
            eta: Duration::from_secs(eta_s.max(0.0) as u64),
            messaggio,
            test_instabili,
            stats,
        })
    }

    async fn registra_esito(
        &self,
        job_id: Uuid,
        outcome: SuiteOutcome,
        test_instabili: &[String],
        chiavi: Option<(&str, &str)>,
    ) {
        let (suite_key, state_key) = match chiavi {
            Some((s, k)) => (Some(s), Some(k)),
            None => (None, None),
        };
        let esito = sqlx::query(
            "UPDATE jobs SET \
                status = $1, \
                suite_key = COALESCE($2, suite_key), \
                state_key = COALESCE($3, state_key), \
                input = jsonb_set( \
                    jsonb_set(input, '{outcome}', to_jsonb($4::text), true), \
                    '{flaky_tests}', $5::jsonb, true) \
             WHERE id = $6 AND project_id = $7",
        )
        .bind(outcome.job_status())
        .bind(suite_key)
        .bind(state_key)
        .bind(outcome.as_str())
        .bind(json!(test_instabili))
        .bind(job_id)
        .bind(self.project_id)
        .execute(&self.pool)
        .await;

        match esito {
            Ok(r) => tracing::debug!(
                target: "mcp_core::suite_verification",
                job_id = %job_id,
                outcome = outcome.as_str(),
                righe = r.rows_affected(),
                memorizzata = chiavi.is_some(),
                "esito di suite registrato"
            ),
            Err(e) => tracing::warn!(
                target: "mcp_core::suite_verification",
                job_id = %job_id,
                error = %e,
                "registrazione dell'esito di suite fallita: la prossima verifica rieseguira'"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Semina un job playwright come lo scrive il runner (senza chiave: e'
    /// `registra_esito` a posarla).
    async fn seed_job(pool: &PgPool, project_id: Uuid, status: &str) -> Uuid {
        sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO jobs (project_id, kind, status, input) \
             VALUES ($1, 'playwright_test', $2, '{\"message\": \"19/21 test passati\"}'::jsonb) \
             RETURNING id",
        )
        .bind(project_id)
        .bind(status)
        .fetch_one(pool)
        .await
        .expect("insert job")
    }

    /// Il giro completo sullo schema REALE del DB-progetto (regola O: le
    /// colonne della memoria nascono dalla migrazione, non da un CREATE TABLE
    /// ricopiato). Registra un esito, poi lo ritrova a chiave identica.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn registra_e_ritrova_a_chiave_identica(pool: PgPool) {
        let progetto = Uuid::new_v4();
        let memo = PgSuiteMemo::new(pool.clone(), progetto);
        let job = seed_job(&pool, progetto, "failed").await;

        memo.registra_esito(
            job,
            SuiteOutcome::Flaky,
            &["e2e/home.spec.ts:5:3".to_string()],
            Some(("app|npx playwright test", "stato-A")),
        )
        .await;

        let hit = memo
            .cerca(
                "app|npx playwright test",
                "stato-A",
                Duration::from_secs(900),
            )
            .await
            .expect("esito memorizzato trovato");
        assert_eq!(hit.job_id, job);
        assert_eq!(hit.outcome, SuiteOutcome::Flaky);
        assert_eq!(hit.test_instabili, vec!["e2e/home.spec.ts:5:3".to_string()]);

        // Lo status della riga passa a 'flaky': e' il pannello a doverlo
        // mostrare come debito di test, non come app rotta.
        let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = $1")
            .bind(job)
            .fetch_one(&pool)
            .await
            .expect("status");
        assert_eq!(status, "flaky");
    }

    /// Chiave di stato diversa = nessun riuso. E' la meta' del presidio che
    /// impedisce a un esito di sopravvivere alle modifiche.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn chiave_diversa_non_risponde(pool: PgPool) {
        let progetto = Uuid::new_v4();
        let memo = PgSuiteMemo::new(pool.clone(), progetto);
        let job = seed_job(&pool, progetto, "passed").await;
        memo.registra_esito(job, SuiteOutcome::Passed, &[], Some(("s", "stato-A")))
            .await;

        assert!(memo
            .cerca("s", "stato-B", Duration::from_secs(900))
            .await
            .is_none());
        assert!(memo
            .cerca("altra-suite", "stato-A", Duration::from_secs(900))
            .await
            .is_none());
    }

    /// La memoria e' di QUESTO progetto: un esito di un altro progetto con le
    /// stesse chiavi non risponde.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn memoria_isolata_per_progetto(pool: PgPool) {
        let progetto = Uuid::new_v4();
        let altro = Uuid::new_v4();
        let job_altro = seed_job(&pool, altro, "passed").await;
        PgSuiteMemo::new(pool.clone(), altro)
            .registra_esito(job_altro, SuiteOutcome::Passed, &[], Some(("s", "stato-A")))
            .await;

        assert!(PgSuiteMemo::new(pool.clone(), progetto)
            .cerca("s", "stato-A", Duration::from_secs(900))
            .await
            .is_none());
    }

    /// Oltre il TTL l'esito non si riusa: la chiave copre il codice e la
    /// generazione dei servizi, non i dati o il mondo attorno — il tetto d'eta'
    /// e' il limite dichiarato di cio' che non puo' vedere.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn oltre_il_ttl_non_risponde(pool: PgPool) {
        let progetto = Uuid::new_v4();
        let memo = PgSuiteMemo::new(pool.clone(), progetto);
        let job = seed_job(&pool, progetto, "passed").await;
        memo.registra_esito(job, SuiteOutcome::Passed, &[], Some(("s", "stato-A")))
            .await;
        // `updated_at` non e' scrivibile: il trigger `trg_jobs_updated_at`
        // (BEFORE UPDATE, mig project 0002) lo riporta a `now()` a ogni
        // scrittura — ed e' cio' che rende quella colonna una misura onesta
        // dell'eta' dell'esito. Per fabbricare una riga vecchia il trigger va
        // sospeso: e' il test che dichiara la propria premessa, non il codice
        // che si adatta al test.
        sqlx::query("ALTER TABLE jobs DISABLE TRIGGER trg_jobs_updated_at")
            .execute(&pool)
            .await
            .expect("sospende il trigger");
        sqlx::query("UPDATE jobs SET updated_at = now() - interval '2 hours' WHERE id = $1")
            .bind(job)
            .execute(&pool)
            .await
            .expect("invecchia la riga");
        sqlx::query("ALTER TABLE jobs ENABLE TRIGGER trg_jobs_updated_at")
            .execute(&pool)
            .await
            .expect("ripristina il trigger");

        assert!(memo
            .cerca("s", "stato-A", Duration::from_secs(900))
            .await
            .is_none());
        assert!(memo
            .cerca("s", "stato-A", Duration::from_secs(24 * 3600))
            .await
            .is_some());
    }

    /// Una suite IN CORSO non e' un esito: riusarla direbbe "verificato" su una
    /// verifica che non e' finita.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn suite_in_corso_non_e_un_esito(pool: PgPool) {
        let progetto = Uuid::new_v4();
        let memo = PgSuiteMemo::new(pool.clone(), progetto);
        let job = seed_job(&pool, progetto, "running").await;
        sqlx::query(
            "UPDATE jobs SET suite_key = 's', state_key = 'stato-A', \
             input = jsonb_set(input, '{outcome}', '\"passed\"', true) WHERE id = $1",
        )
        .bind(job)
        .execute(&pool)
        .await
        .expect("marca la riga in corso");

        assert!(memo
            .cerca("s", "stato-A", Duration::from_secs(900))
            .await
            .is_none());
    }
}
