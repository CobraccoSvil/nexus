//! Worker `deepseek_balance_sync` — unico provider AI consumer (insieme a
//! pochi altri) che espone un endpoint pubblico per il balance:
//! `GET https://api.deepseek.com/user/balance`. Lo usiamo per sincronizzare
//! `provider_budget_status` con il dato REALE invece dello stimato.
//!
//! Risposta API DeepSeek:
//! ```json
//! {
//!   "is_available": true,
//!   "balance_infos": [
//!     {"currency": "USD", "total_balance": "12.34", "granted_balance": "0.00", "topped_up_balance": "12.34"}
//!   ]
//! }
//! ```
//!
//! Logica:
//!   - Leggi `monthly_budget_usd` corrente dal DB
//!   - Leggi `total_balance` reale dall'API
//!   - Calcola `spent_real = monthly_budget_usd - total_balance` (clipped a >=0)
//!   - UPDATE spent_current_period_usd con il valore reale
//!
//! Cadenza default: 15 min (sufficiente per rilevare ricariche senza spammare).

use sqlx::PgPool;
use std::time::Duration;
use tokio::time::sleep;

const MIN_INTERVAL_S: u64 = 60;
const DEEPSEEK_BALANCE_URL: &str = "https://api.deepseek.com/user/balance";

pub fn spawn_deepseek_balance_sync(db: PgPool, enabled: bool, interval_s: u64) {
    let enabled = match std::env::var("NEXUS_DEEPSEEK_BALANCE_SYNC_ENABLED").as_deref() {
        Ok("false") | Ok("0") => false,
        Ok("true") | Ok("1") => true,
        _ => enabled,
    };
    if !enabled {
        tracing::info!("deepseek_balance_sync: DISABILITATO");
        return;
    }
    let interval_s = std::env::var("NEXUS_DEEPSEEK_BALANCE_SYNC_INTERVAL_S")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(interval_s)
        .max(MIN_INTERVAL_S);
    tracing::info!("deepseek_balance_sync: avvio worker (interval={interval_s}s)");
    tokio::spawn(async move {
        // Aspetta 90s al primo avvio per dare tempo agli altri servizi.
        sleep(Duration::from_secs(90)).await;
        loop {
            run_one_sync(&db).await;
            sleep(Duration::from_secs(interval_s)).await;
        }
    });
}

async fn run_one_sync(db: &PgPool) {
    // Recupera la API key DeepSeek dal DB settings.
    let api_key: Option<String> = sqlx::query_scalar(
        "SELECT value FROM settings WHERE key = 'deepseek_api_key' AND value <> ''",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let Some(api_key) = api_key else {
        tracing::debug!("deepseek_balance_sync: deepseek_api_key non configurata, skip");
        return;
    };

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("deepseek_balance_sync: client build fallito: {e}");
            return;
        }
    };

    let resp = match client
        .get(DEEPSEEK_BALANCE_URL)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("deepseek_balance_sync: HTTP fallito: {e}");
            return;
        }
    };

    if !resp.status().is_success() {
        tracing::warn!(
            "deepseek_balance_sync: HTTP {} dall'API balance",
            resp.status()
        );
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("deepseek_balance_sync: parse JSON fallito: {e}");
            return;
        }
    };

    // Estrai total_balance USD (string) dalla response.
    let total_balance_str = body
        .get("balance_infos")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|info| info.get("currency").and_then(|c| c.as_str()) == Some("USD"))
        })
        .and_then(|info| info.get("total_balance"))
        .and_then(|v| v.as_str());

    let Some(balance_str) = total_balance_str else {
        tracing::warn!("deepseek_balance_sync: total_balance USD non trovato in response");
        return;
    };

    let balance: f64 = match balance_str.parse() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("deepseek_balance_sync: balance '{balance_str}' non parseable: {e}");
            return;
        }
    };

    let result = persisti_balance(db, balance, balance_str).await;

    match result {
        Ok(rows) if rows > 0 => {
            tracing::info!(
                "deepseek_balance_sync: aggiornato spent da balance reale {balance_str} USD"
            );
        }
        Ok(_) => {
            tracing::debug!(
                "deepseek_balance_sync: nessuna riga deepseek in provider_budget_status"
            );
        }
        Err(e) => {
            tracing::warn!("deepseek_balance_sync: UPDATE fallito: {e}");
        }
    }
}

/// Persiste il balance osservato: `spent = max(0, monthly_budget - balance)`.
/// Ritorna le righe toccate (0 = nessuna riga `deepseek` in tabella).
///
/// PERCHE' `notes` SI SOSTITUISCE E NON SI ACCODA. Questa UPDATE gira a ogni
/// giro del worker (default 15 min, minimo 60s) e la sua nota era concatenata
/// con `notes = COALESCE(notes,'') || ' [sync ...]'`: una cella che cresce di
/// ~34 byte per giro, senza alcun limite e senza che nessuno la rilegga mai
/// — la `BudgetRow` del wire admin non seleziona `notes`, ed e' l'unica
/// lettura che quella colonna abbia. MISURATO il 10/08/2026 sul META vivo:
/// 5363 accodamenti della STESSA stringa per 182.499 byte (178 kB) in una sola
/// cella, e deepseek era l'unico provider con `notes` non NULL — cioe' l'unico
/// che passa di qui. La colonna e' un'annotazione per un umano che guarda la
/// riga, non un registro di eventi: il QUANDO e' gia' in `updated_at` e il
/// QUANTO nelle colonne numeriche, quindi la forma corretta e' l'ultimo valore
/// osservato, non la loro storia (regola H: il campo non e' un log).
///
/// Non diventa una colonna tipizzata (regola Q) di proposito: nessun lettore la
/// consulterebbe, e una colonna che nessun `SELECT` tocca e' il difetto che il
/// censimento delle capability ha gia' misurato altrove (20 colonne su 32 senza
/// alcun consumatore). Resta prosa per un umano, e la prosa si sostituisce.
async fn persisti_balance(db: &PgPool, balance: f64, balance_str: &str) -> Result<u64, sqlx::Error> {
    let r = sqlx::query(
        r#"UPDATE provider_budget_status
              SET spent_current_period_usd = GREATEST(monthly_budget_usd - $1, 0),
                  updated_at = NOW(),
                  notes = '[sync deepseek api: balance=' || $2 || ']'
            WHERE provider = 'deepseek'"#,
    )
    .bind(balance)
    .bind(balance_str)
    .execute(db)
    .await?;
    Ok(r.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Il worker gira in continuazione: la nota dev'essere STABILE nel tempo,
    /// non crescente. Il test attraversa la stessa funzione che il worker
    /// chiama (regola O): la crescita viveva nella statement SQL, quindi un
    /// test su un helper che compone la stringa non avrebbe potuto vederla.
    ///
    /// MUTAZIONE che lo fa rosseggiare: rimettere `notes = COALESCE(notes,'')
    /// || ' [sync ...]'` in `persisti_balance`. La lunghezza dopo il terzo giro
    /// diventa il triplo e l'assert sulla stabilita' fallisce.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_nota_di_sync_non_cresce_a_ogni_giro(db: PgPool) {
        // UPSERT: la mig 0173 semina gia' una riga `deepseek` a budget 0. Il
        // test non la ricrea (sarebbe una fixture ricopiata, regola O): prende
        // quella vera e le da' il tetto che serve al caso.
        sqlx::query(
            "INSERT INTO provider_budget_status (provider, monthly_budget_usd) VALUES ('deepseek', 20)
             ON CONFLICT (provider) DO UPDATE SET monthly_budget_usd = 20",
        )
        .execute(&db)
        .await
        .expect("seed riga deepseek");

        let lunghezza = |db: PgPool| async move {
            sqlx::query_scalar::<_, Option<i32>>(
                "SELECT length(notes)::int FROM provider_budget_status WHERE provider = 'deepseek'",
            )
            .fetch_one(&db)
            .await
            .expect("length(notes)")
        };

        assert_eq!(persisti_balance(&db, 8.0, "8.00").await.unwrap(), 1);
        let dopo_uno = lunghezza(db.clone()).await.expect("nota scritta al primo giro");
        persisti_balance(&db, 8.0, "8.00").await.unwrap();
        persisti_balance(&db, 8.0, "8.00").await.unwrap();
        let dopo_tre = lunghezza(db.clone()).await.expect("nota presente al terzo giro");

        assert_eq!(
            dopo_uno, dopo_tre,
            "la nota deve essere sostituita: accodandola, 5363 giri hanno prodotto 178 kB in una cella"
        );

        // La nota porta l'ULTIMO valore osservato, non la loro storia.
        persisti_balance(&db, 3.5, "3.50").await.unwrap();
        let nota: String = sqlx::query_scalar(
            "SELECT notes FROM provider_budget_status WHERE provider = 'deepseek'",
        )
        .fetch_one(&db)
        .await
        .expect("notes");
        assert_eq!(nota, "[sync deepseek api: balance=3.50]");

        // Lo spent resta il dato reale derivato dal balance appena osservato.
        let spent: String = sqlx::query_scalar(
            "SELECT spent_current_period_usd::text FROM provider_budget_status WHERE provider = 'deepseek'",
        )
        .fetch_one(&db)
        .await
        .expect("spent");
        assert_eq!(spent.parse::<f64>().unwrap(), 16.5);
    }
}
