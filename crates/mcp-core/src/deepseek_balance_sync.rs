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

    // Aggiorna spent_current_period_usd = max(0, monthly_budget - balance_reale).
    // Cosi' la nostra vista mostra il dato reale (e l'is_exhausted scatta
    // quando balance reale < min_threshold).
    let result = sqlx::query(
        r#"UPDATE provider_budget_status
              SET spent_current_period_usd = GREATEST(monthly_budget_usd - $1, 0),
                  updated_at = NOW(),
                  notes = COALESCE(notes,'') || ' [sync deepseek api: balance=' || $2 || ']'
            WHERE provider = 'deepseek'
        RETURNING monthly_budget_usd::text AS budget, spent_current_period_usd::text AS spent"#,
    )
    .bind(balance)
    .bind(balance_str)
    .execute(db)
    .await;

    match result {
        Ok(r) if r.rows_affected() > 0 => {
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
