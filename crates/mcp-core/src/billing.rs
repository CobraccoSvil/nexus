use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{auth::Claims, AppState};

// Listino: punto unico `nexus-pricing` (regola L). Qui restava una copia della
// query + del calcolo, divergente dalle altre due su filtro `is_enabled`,
// currency di default e lettura di `pricing_state`. Ri-esportati per i call site
// storici che li importano da `crate::billing`.
pub use nexus_pricing::{calculate_cost, PriceLookup};

#[derive(Debug, Clone, sqlx::FromRow)]
struct QuotaPolicy {
    scope_type: String,
    user_id: Option<Uuid>,
    project_id: Option<Uuid>,
    token_limit: Option<i64>,
    cost_limit: Option<f64>,
    valid_from: DateTime<Utc>,
    valid_to: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct UsageReservation {
    pub ledger_id: Uuid,
    /// Esito STRUTTURATO del listino al momento della prenotazione (regola M).
    ///
    /// Prima era un `PriceSnapshot` secco: quando il prezzo non era noto,
    /// `reserve_usage` era costretta a fabbricare un `{0, 0, currency}` e a
    /// infilarlo qui, cosi' `finalize_usage` calcolava un costo 0 senza sapere
    /// perche'. Il magic fallback sopravviveva alla struct. Con l'enum, "non so
    /// quanto costa" resta dichiarato fino alla scrittura del ledger.
    pub lookup: PriceLookup,
    /// Currency da annotare sulle righe di ledger di questa prenotazione. Serve
    /// anche quando il prezzo non e' noto: la colonna e' NOT NULL.
    pub currency: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageNumbers {
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub total_tokens: i32,
}

#[derive(Debug)]
pub struct QuotaExceededError {
    pub scope: String,
    pub reason: String,
}

impl std::fmt::Display for QuotaExceededError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "quota_exceeded:{}:{}", self.scope, self.reason)
    }
}

impl std::error::Error for QuotaExceededError {}

#[derive(Debug, Deserialize)]
pub struct CreatePriceRequest {
    pub provider: String,
    pub model: String,
    pub input_cost_per_million_tokens: f64,
    pub output_cost_per_million_tokens: f64,
    pub currency: Option<String>,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_to: Option<DateTime<Utc>>,
    pub is_enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePriceRequest {
    pub input_cost_per_million_tokens: Option<f64>,
    pub output_cost_per_million_tokens: Option<f64>,
    pub currency: Option<String>,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_to: Option<DateTime<Utc>>,
    pub is_enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateQuotaRequest {
    pub scope_type: String,
    pub user_id: Option<String>,
    pub project_id: Option<String>,
    pub token_limit: Option<i64>,
    pub cost_limit: Option<f64>,
    pub currency: Option<String>,
    pub valid_from: DateTime<Utc>,
    pub valid_to: DateTime<Utc>,
    pub is_enabled: Option<bool>,
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateQuotaRequest {
    pub token_limit: Option<i64>,
    pub cost_limit: Option<f64>,
    pub currency: Option<String>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub is_enabled: Option<bool>,
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UsageQuery {
    pub date_from: Option<DateTime<Utc>>,
    pub date_to: Option<DateTime<Utc>>,
    pub user_id: Option<String>,
    pub project_id: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub status: Option<String>,
}

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

async fn read_active_quotas(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    project_id: Uuid,
) -> anyhow::Result<Vec<QuotaPolicy>> {
    let quotas = sqlx::query_as::<_, QuotaPolicy>(
        r#"
        SELECT scope_type, user_id, project_id, token_limit, cost_limit, valid_from, valid_to
        FROM ai_quota_policies
        WHERE is_enabled = TRUE
          AND valid_from <= NOW()
          AND valid_to > NOW()
          AND (
                (scope_type = 'user' AND user_id = $1) OR
                (scope_type = 'project' AND project_id = $2) OR
                (scope_type = 'user_project' AND user_id = $1 AND project_id = $2)
          )
        FOR UPDATE
        "#,
    )
    .bind(user_id)
    .bind(project_id)
    .fetch_all(&mut **tx)
    .await?;

    Ok(quotas)
}

/// Calcola l'usage corrente (tokens, cost) per TUTTE le quote attive in una sola
/// query, evitando l'N+1 di una SELECT SUM() separata per ogni quota.
///
/// Ogni quota e' identificata posizionalmente: la riga i-esima del risultato
/// (ordinata da `idx`) corrisponde a `quotas[i]`. La semantica per-quota e'
/// preservata esattamente:
///   - predicato di scope (`user` / `project` / `user_project`)
///   - finestra temporale propria della quota (`valid_from`..`valid_to`)
///   - status IN ('reserved', 'finalized')
///
/// Il LEFT JOIN garantisce una riga per ogni quota anche quando non c'e' alcun
/// consumo (tokens=0, cost=0), come faceva il COALESCE(SUM(...), 0) per-query.
/// Gira nella stessa transazione `tx` delle altre letture quota.
async fn usage_for_quotas(
    tx: &mut Transaction<'_, Postgres>,
    quotas: &[QuotaPolicy],
) -> anyhow::Result<Vec<(i64, f64)>> {
    if quotas.is_empty() {
        return Ok(Vec::new());
    }

    let mut idxs: Vec<i32> = Vec::with_capacity(quotas.len());
    let mut scope_types: Vec<String> = Vec::with_capacity(quotas.len());
    let mut user_ids: Vec<Uuid> = Vec::with_capacity(quotas.len());
    let mut project_ids: Vec<Uuid> = Vec::with_capacity(quotas.len());
    let mut valid_froms: Vec<DateTime<Utc>> = Vec::with_capacity(quotas.len());
    let mut valid_tos: Vec<DateTime<Utc>> = Vec::with_capacity(quotas.len());
    for (i, q) in quotas.iter().enumerate() {
        idxs.push(i as i32);
        scope_types.push(q.scope_type.clone());
        // user_id/project_id possono essere NULL nelle quote di scope opposto;
        // il predicato CASE per scope li usa solo quando rilevanti, quindi un
        // valore segnaposto (nil) non altera il risultato.
        user_ids.push(q.user_id.unwrap_or_else(Uuid::nil));
        project_ids.push(q.project_id.unwrap_or_else(Uuid::nil));
        valid_froms.push(q.valid_from);
        valid_tos.push(q.valid_to);
    }

    let rows = sqlx::query(
        r#"
        WITH q AS (
            SELECT * FROM UNNEST(
                $1::int[], $2::text[], $3::uuid[], $4::uuid[],
                $5::timestamptz[], $6::timestamptz[]
            ) AS t(idx, scope_type, user_id, project_id, valid_from, valid_to)
        )
        SELECT
            q.idx AS idx,
            COALESCE(SUM(l.total_tokens), 0)::bigint AS tokens,
            COALESCE(SUM(l.total_cost), 0)::float8 AS cost
        FROM q
        LEFT JOIN ai_usage_ledger l
            ON l.status IN ('reserved', 'finalized')
           AND l.created_at >= q.valid_from
           AND l.created_at < q.valid_to
           AND (
                (q.scope_type = 'user' AND l.user_id = q.user_id)
             OR (q.scope_type = 'project' AND l.project_id = q.project_id)
             OR (q.scope_type = 'user_project'
                 AND l.user_id = q.user_id AND l.project_id = q.project_id)
           )
        GROUP BY q.idx
        ORDER BY q.idx
        "#,
    )
    .bind(&idxs)
    .bind(&scope_types)
    .bind(&user_ids)
    .bind(&project_ids)
    .bind(&valid_froms)
    .bind(&valid_tos)
    .fetch_all(&mut **tx)
    .await?;

    let mut out = vec![(0i64, 0.0f64); quotas.len()];
    for row in rows {
        let idx = row.try_get::<i32, _>("idx").unwrap_or(0) as usize;
        let tokens = row.try_get::<i64, _>("tokens").unwrap_or(0);
        let cost = row.try_get::<f64, _>("cost").unwrap_or(0.0);
        if let Some(slot) = out.get_mut(idx) {
            *slot = (tokens, cost);
        }
    }
    Ok(out)
}

pub async fn reserve_usage(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    provider: &str,
    model: &str,
    prompt_tokens: i32,
    estimated_completion_tokens: i32,
    details: Value,
) -> anyhow::Result<UsageReservation> {
    let lookup = nexus_pricing::resolve_active_price(db, provider, model).await?;
    let currency = nexus_pricing::platform_currency(db).await?;
    if matches!(lookup, PriceLookup::Unknown) {
        // Stima a 0 su prezzo IGNOTO: non blocca la richiesta (sarebbe un cambio
        // di policy sulle quote), ma va detto — una stima 0 non consuma quota di
        // costo e lascia sforare senza che nessuno se ne accorga. Il fix
        // strutturale e' a monte: un modello a prezzo ignoto non deve essere
        // routabile (`model_catalog_sync::price_unknown_sql`).
        tracing::warn!(
            target: "billing",
            "reserve_usage: prezzo IGNOTO (pricing_state='unknown') -> stima quota a 0 \
             (provider={provider}, model={model})",
        );
    }
    let estimated_total_tokens = prompt_tokens.saturating_add(estimated_completion_tokens);
    let (input_cost, output_cost, estimated_total_cost) = match &lookup {
        PriceLookup::Priced(p) => calculate_cost(
            p,
            prompt_tokens as i64,
            estimated_completion_tokens as i64,
        ),
        _ => (0.0, 0.0, 0.0),
    };

    let mut tx = db.begin().await?;
    let quotas = read_active_quotas(&mut tx, user_id, project_id).await?;
    let usage = usage_for_quotas(&mut tx, &quotas).await?;

    for (quota, &(used_tokens, used_cost)) in quotas.iter().zip(usage.iter()) {
        if let Some(limit) = quota.token_limit {
            let projected = used_tokens.saturating_add(estimated_total_tokens as i64);
            if projected > limit {
                let rejected_id = Uuid::new_v4();
                let _ = sqlx::query(
                    r#"
                    INSERT INTO ai_usage_ledger (
                        id, user_id, project_id, provider, model, prompt_tokens, completion_tokens, total_tokens,
                        input_cost, output_cost, total_cost, currency, status, rejection_reason, details, finalized_at
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'rejected', $13, $14, NOW())
                    "#,
                )
                .bind(rejected_id)
                .bind(user_id)
                .bind(project_id)
                .bind(provider)
                .bind(model)
                .bind(prompt_tokens)
                .bind(estimated_completion_tokens)
                .bind(estimated_total_tokens)
                .bind(input_cost)
                .bind(output_cost)
                .bind(estimated_total_cost)
                .bind(&currency)
                .bind(format!("quota_exceeded:{}:token_limit", quota.scope_type))
                .bind(details.clone())
                .execute(&mut *tx)
                .await;
                tx.commit().await?;
                return Err(anyhow::Error::new(QuotaExceededError {
                    scope: quota.scope_type.clone(),
                    reason: "token_limit".to_string(),
                }));
            }
        }

        if let Some(limit) = quota.cost_limit {
            let projected = used_cost + estimated_total_cost;
            if projected > limit {
                let rejected_id = Uuid::new_v4();
                let _ = sqlx::query(
                    r#"
                    INSERT INTO ai_usage_ledger (
                        id, user_id, project_id, provider, model, prompt_tokens, completion_tokens, total_tokens,
                        input_cost, output_cost, total_cost, currency, status, rejection_reason, details, finalized_at
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'rejected', $13, $14, NOW())
                    "#,
                )
                .bind(rejected_id)
                .bind(user_id)
                .bind(project_id)
                .bind(provider)
                .bind(model)
                .bind(prompt_tokens)
                .bind(estimated_completion_tokens)
                .bind(estimated_total_tokens)
                .bind(input_cost)
                .bind(output_cost)
                .bind(estimated_total_cost)
                .bind(&currency)
                .bind(format!("quota_exceeded:{}:cost_limit", quota.scope_type))
                .bind(details.clone())
                .execute(&mut *tx)
                .await;
                tx.commit().await?;
                return Err(anyhow::Error::new(QuotaExceededError {
                    scope: quota.scope_type.clone(),
                    reason: "cost_limit".to_string(),
                }));
            }
        }
    }

    let ledger_id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO ai_usage_ledger (
            id, user_id, project_id, provider, model,
            prompt_tokens, completion_tokens, total_tokens,
            input_cost, output_cost, total_cost, currency,
            status, details
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'reserved', $13)
        "#,
    )
    .bind(ledger_id)
    .bind(user_id)
    .bind(project_id)
    .bind(provider)
    .bind(model)
    .bind(prompt_tokens)
    .bind(estimated_completion_tokens)
    .bind(estimated_total_tokens)
    .bind(input_cost)
    .bind(output_cost)
    .bind(estimated_total_cost)
    .bind(&currency)
    .bind(details)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(UsageReservation {
        ledger_id,
        lookup,
        currency,
    })
}

pub async fn finalize_usage(
    db: &PgPool,
    reservation: &UsageReservation,
    run_id: Uuid,
    usage: &UsageNumbers,
) -> anyhow::Result<(f64, f64, f64, String)> {
    // Costo dai token REALI, ma solo se il listino era noto: su prezzo ignoto il
    // ledger registra uno zero DICHIARATO (`price_state`), non un costo calcolato
    // su un prezzo placeholder.
    let (input_cost, output_cost, total_cost) = match &reservation.lookup {
        PriceLookup::Priced(p) => calculate_cost(
            p,
            usage.prompt_tokens as i64,
            usage.completion_tokens as i64,
        ),
        _ => (0.0, 0.0, 0.0),
    };

    sqlx::query(
        r#"
        UPDATE ai_usage_ledger
        SET run_id = $2,
            prompt_tokens = $3,
            completion_tokens = $4,
            total_tokens = $5,
            input_cost = $6,
            output_cost = $7,
            total_cost = $8,
            status = 'finalized',
            finalized_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(reservation.ledger_id)
    .bind(run_id)
    .bind(usage.prompt_tokens)
    .bind(usage.completion_tokens)
    .bind(usage.total_tokens)
    .bind(input_cost)
    .bind(output_cost)
    .bind(total_cost)
    .execute(db)
    .await?;

    Ok((
        input_cost,
        output_cost,
        total_cost,
        reservation.currency.clone(),
    ))
}

pub async fn release_usage(db: &PgPool, reservation: &UsageReservation, reason: &str) {
    let _ = sqlx::query(
        r#"
        UPDATE ai_usage_ledger
        SET status = 'released',
            rejection_reason = $2,
            finalized_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(reservation.ledger_id)
    .bind(reason)
    .execute(db)
    .await;
}

pub fn extract_usage_numbers(
    completion: &Value,
    prompt_tokens_fallback: i32,
    completion_tokens_fallback: i32,
) -> UsageNumbers {
    let usage = completion["metadata"]["usage"].clone();

    let prompt_tokens = usage["prompt_tokens"]
        .as_i64()
        .or_else(|| usage["input_tokens"].as_i64())
        .unwrap_or(prompt_tokens_fallback as i64)
        .max(0) as i32;
    let completion_tokens = usage["completion_tokens"]
        .as_i64()
        .or_else(|| usage["output_tokens"].as_i64())
        .or_else(|| usage["candidates_token_count"].as_i64())
        .unwrap_or(completion_tokens_fallback as i64)
        .max(0) as i32;
    let total_tokens = usage["total_tokens"]
        .as_i64()
        .unwrap_or((prompt_tokens + completion_tokens) as i64)
        .max(0) as i32;

    UsageNumbers {
        prompt_tokens,
        completion_tokens,
        total_tokens,
    }
}

fn parse_uuid(id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(id).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid UUID" })),
        )
    })
}

fn claims_user_id(claims: &Claims) -> Result<Uuid, ApiError> {
    parse_uuid(&claims.sub)
}

pub async fn list_prices(State(state): State<AppState>) -> ApiResult {
    let rows = sqlx::query(
        r#"
        SELECT id, provider, model,
               input_cost_per_million_tokens::float8,
               output_cost_per_million_tokens::float8,
               currency, effective_from, effective_to, is_enabled, updated_at
        FROM ai_price_catalog
        ORDER BY provider, model, effective_from DESC
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    let prices: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<Uuid, _>("id"),
                "provider": row.get::<String, _>("provider"),
                "model": row.get::<String, _>("model"),
                "input_cost_per_million_tokens": row.get::<f64, _>("input_cost_per_million_tokens"),
                "output_cost_per_million_tokens": row.get::<f64, _>("output_cost_per_million_tokens"),
                "currency": row.get::<String, _>("currency"),
                "effective_from": row.get::<DateTime<Utc>, _>("effective_from"),
                "effective_to": row.get::<Option<DateTime<Utc>>, _>("effective_to"),
                "is_enabled": row.get::<bool, _>("is_enabled"),
                "updated_at": row.get::<DateTime<Utc>, _>("updated_at"),
            })
        })
        .collect();

    Ok(Json(json!({ "prices": prices })))
}

pub async fn create_price(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<CreatePriceRequest>,
) -> ApiResult {
    let created_by = claims_user_id(&claims)?;
    let currency = body
        .currency
        .unwrap_or_else(|| "EUR".to_string())
        .trim()
        .to_uppercase();
    let id = Uuid::new_v4();

    // pricing_state derivato dal costo: > 0 -> 'priced', altrimenti 'unknown'
    // (placeholder). Mai 'free' qui: il gratuito reale e' una scelta esplicita
    // di seed/admin, non un effetto collaterale di un INSERT a costo 0 (mig 0477).
    sqlx::query(
        r#"
        INSERT INTO ai_price_catalog (
            id, provider, model, input_cost_per_million_tokens, output_cost_per_million_tokens,
            currency, effective_from, effective_to, is_enabled, created_by, pricing_state, updated_at
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, COALESCE($9, TRUE), $10,
            CASE WHEN COALESCE($4, 0) > 0 OR COALESCE($5, 0) > 0 THEN 'priced' ELSE 'unknown' END,
            NOW()
        )
        "#,
    )
    .bind(id)
    .bind(body.provider.trim().to_lowercase())
    .bind(body.model.trim())
    .bind(body.input_cost_per_million_tokens)
    .bind(body.output_cost_per_million_tokens)
    .bind(currency)
    .bind(body.effective_from.unwrap_or_else(Utc::now))
    .bind(body.effective_to)
    .bind(body.is_enabled)
    .bind(created_by)
    .execute(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    Ok(Json(json!({ "id": id, "status": "created" })))
}

pub async fn update_price(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdatePriceRequest>,
) -> ApiResult {
    let price_id = parse_uuid(&id)?;

    // pricing_state coerente col costo risultante: se il prezzo effettivo (post
    // COALESCE) e' > 0 -> 'priced'. Altrimenti NON si degrada lo stato esistente:
    // un 'free' confermato a mano resta 'free', un 'unknown' resta 'unknown'
    // (mig 0477; mai promozione automatica a 'free').
    sqlx::query(
        r#"
        UPDATE ai_price_catalog
        SET input_cost_per_million_tokens = COALESCE($2, input_cost_per_million_tokens),
            output_cost_per_million_tokens = COALESCE($3, output_cost_per_million_tokens),
            currency = COALESCE($4, currency),
            effective_from = COALESCE($5, effective_from),
            effective_to = COALESCE($6, effective_to),
            is_enabled = COALESCE($7, is_enabled),
            pricing_state = CASE
                WHEN COALESCE($2, input_cost_per_million_tokens) > 0
                  OR COALESCE($3, output_cost_per_million_tokens) > 0 THEN 'priced'
                ELSE pricing_state
            END,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(price_id)
    .bind(body.input_cost_per_million_tokens)
    .bind(body.output_cost_per_million_tokens)
    .bind(body.currency.map(|c| c.trim().to_uppercase()))
    .bind(body.effective_from)
    .bind(body.effective_to)
    .bind(body.is_enabled)
    .execute(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    Ok(Json(json!({ "id": price_id, "status": "updated" })))
}

pub async fn list_quotas(State(state): State<AppState>) -> ApiResult {
    let rows = sqlx::query(
        r#"
        SELECT id, scope_type, user_id, project_id, token_limit, cost_limit, currency,
               valid_from, valid_to, is_enabled, note, updated_at
        FROM ai_quota_policies
        ORDER BY valid_from DESC
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    let quotas: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<Uuid, _>("id"),
                "scope_type": row.get::<String, _>("scope_type"),
                "user_id": row.get::<Option<Uuid>, _>("user_id"),
                "project_id": row.get::<Option<Uuid>, _>("project_id"),
                "token_limit": row.get::<Option<i64>, _>("token_limit"),
                "cost_limit": row.get::<Option<f64>, _>("cost_limit"),
                "currency": row.get::<Option<String>, _>("currency"),
                "valid_from": row.get::<DateTime<Utc>, _>("valid_from"),
                "valid_to": row.get::<DateTime<Utc>, _>("valid_to"),
                "is_enabled": row.get::<bool, _>("is_enabled"),
                "note": row.get::<String, _>("note"),
                "updated_at": row.get::<DateTime<Utc>, _>("updated_at"),
            })
        })
        .collect();

    Ok(Json(json!({ "quotas": quotas })))
}

pub async fn create_quota(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<CreateQuotaRequest>,
) -> ApiResult {
    let created_by = claims_user_id(&claims)?;
    let id = Uuid::new_v4();
    let scope_type = body.scope_type.trim().to_lowercase();
    let user_id = body.user_id.as_deref().map(parse_uuid).transpose()?;
    let project_id = body.project_id.as_deref().map(parse_uuid).transpose()?;
    let currency = body.currency.map(|c| c.trim().to_uppercase());

    sqlx::query(
        r#"
        INSERT INTO ai_quota_policies (
            id, scope_type, user_id, project_id, token_limit, cost_limit, currency,
            valid_from, valid_to, is_enabled, created_by, note, updated_at
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, COALESCE($10, TRUE), $11, COALESCE($12, ''), NOW()
        )
        "#,
    )
    .bind(id)
    .bind(scope_type)
    .bind(user_id)
    .bind(project_id)
    .bind(body.token_limit)
    .bind(body.cost_limit)
    .bind(currency)
    .bind(body.valid_from)
    .bind(body.valid_to)
    .bind(body.is_enabled)
    .bind(created_by)
    .bind(body.note)
    .execute(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    Ok(Json(json!({ "id": id, "status": "created" })))
}

pub async fn update_quota(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateQuotaRequest>,
) -> ApiResult {
    let quota_id = parse_uuid(&id)?;

    sqlx::query(
        r#"
        UPDATE ai_quota_policies
        SET token_limit = COALESCE($2, token_limit),
            cost_limit = COALESCE($3, cost_limit),
            currency = COALESCE($4, currency),
            valid_from = COALESCE($5, valid_from),
            valid_to = COALESCE($6, valid_to),
            is_enabled = COALESCE($7, is_enabled),
            note = COALESCE($8, note),
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(quota_id)
    .bind(body.token_limit)
    .bind(body.cost_limit)
    .bind(body.currency.map(|c| c.trim().to_uppercase()))
    .bind(body.valid_from)
    .bind(body.valid_to)
    .bind(body.is_enabled)
    .bind(body.note)
    .execute(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    Ok(Json(json!({ "id": quota_id, "status": "updated" })))
}

pub async fn admin_usage_report(
    State(state): State<AppState>,
    Query(query): Query<UsageQuery>,
) -> ApiResult {
    usage_report(&state.db, query).await
}

pub async fn my_usage_report(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(mut query): Query<UsageQuery>,
) -> ApiResult {
    query.user_id = Some(claims.sub.clone());
    usage_report(&state.db, query).await
}

pub async fn project_usage_report(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(project_id): Path<String>,
    Query(mut query): Query<UsageQuery>,
) -> ApiResult {
    let requested_project_id = parse_uuid(&project_id)?;
    let requester = claims_user_id(&claims)?;

    if claims.role != "admin" {
        let is_owner = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = $1 AND owner_user_id = $2)",
        )
        .bind(requested_project_id)
        .bind(requester)
        .fetch_one(&state.db)
        .await
        .unwrap_or(false);

        if !is_owner {
            return Err((
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "Not allowed to view this project usage" })),
            ));
        }
    }

    query.project_id = Some(requested_project_id.to_string());
    usage_report(&state.db, query).await
}

pub async fn get_session_usage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let session_id = params
        .get("session_id")
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Missing or invalid session_id" })),
            )
        })?;

    let requester = claims_user_id(&claims)?;

    // Separazione DB per-progetto: chat_sessions e chat_messages sono tabelle
    // migrate; instrada le letture sul pool del progetto risolto dalla sessione.
    // Niente fallback al meta (mig 0527): DB progetto non disponibile -> 503.
    let session_pool =
        crate::project_db_routes::project_data_pool_by_session_from(&state.db, session_id).await?;

    // Verify the session belongs to the requesting user (or user is admin)
    if claims.role != "admin" {
        let is_owner = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM chat_sessions WHERE id = $1 AND user_id = $2)",
        )
        .bind(session_id)
        .bind(requester)
        .fetch_one(&session_pool)
        .await
        .unwrap_or(false);

        if !is_owner {
            return Err((
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "Not allowed to view this session usage" })),
            ));
        }
    }

    // Aggregate da assistant messages. Distinzione semantica:
    // - total_tokens: solo messaggi VIVI (deleted_at IS NULL) -> usato per il
    //   context window % della TokenUsageBar, che dopo un compact deve scendere.
    // - total_cost: TUTTI i messaggi, inclusi i soft-deleted dalla compattazione
    //   -> il costo e' CUMULATIVO (gia' speso/pagato) e non deve mai azzerarsi
    //   compattando la chat. Bug storico: il filtro deleted_at azzerava anche il
    //   costo dopo un compact.
    let summary_row = sqlx::query(
        r#"
        SELECT
            COALESCE(SUM((metadata->>'totalTokens')::bigint)
                     FILTER (WHERE deleted_at IS NULL), 0)::bigint AS total_tokens,
            COALESCE(SUM((metadata->>'totalCost')::float8), 0.0)::float8 AS total_cost
        FROM chat_messages
        WHERE session_id = $1
          AND role = 'assistant'
          AND metadata->>'totalTokens' IS NOT NULL
        "#,
    )
    .bind(session_id)
    .fetch_one(&session_pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    let breakdown_rows = sqlx::query(
        r#"
        SELECT
            COALESCE(metadata->>'model', 'unknown') AS model,
            COALESCE(SUM((metadata->>'totalTokens')::bigint), 0)::bigint AS tokens,
            COALESCE(SUM((metadata->>'totalCost')::float8), 0.0)::float8 AS cost_usd
        FROM chat_messages
        WHERE session_id = $1
          AND role = 'assistant'
          AND deleted_at IS NULL
          AND metadata->>'totalTokens' IS NOT NULL
        GROUP BY metadata->>'model'
        ORDER BY tokens DESC
        "#,
    )
    .bind(session_id)
    .fetch_all(&session_pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    let breakdown: Vec<Value> = breakdown_rows
        .into_iter()
        .map(|row| {
            json!({
                "model": row.get::<String, _>("model"),
                "tokens": row.get::<i64, _>("tokens"),
                "cost_usd": row.get::<f64, _>("cost_usd"),
            })
        })
        .collect();

    Ok(Json(json!({
        "session_id": session_id,
        "total_tokens": summary_row.get::<i64, _>("total_tokens"),
        "total_cost_usd": summary_row.get::<f64, _>("total_cost"),
        "breakdown": breakdown,
    })))
}

async fn usage_report(db: &PgPool, query: UsageQuery) -> ApiResult {
    let from = query
        .date_from
        .unwrap_or_else(|| Utc::now() - chrono::Duration::days(30));
    let to = query.date_to.unwrap_or_else(Utc::now);
    let user_id = query.user_id.as_deref().map(parse_uuid).transpose()?;
    let project_id = query.project_id.as_deref().map(parse_uuid).transpose()?;
    let provider = query.provider.map(|v| v.to_lowercase());
    let model = query.model;
    let status = query.status;

    let summary = sqlx::query(
        r#"
        SELECT
            COALESCE(SUM(total_tokens), 0)::bigint AS total_tokens,
            COALESCE(SUM(total_cost), 0)::float8 AS total_cost,
            COUNT(*)::bigint AS total_runs
        FROM ai_usage_ledger
        WHERE created_at >= $1
          AND created_at < $2
          AND ($3::uuid IS NULL OR user_id = $3)
          AND ($4::uuid IS NULL OR project_id = $4)
          AND ($5::text IS NULL OR provider = $5)
          AND ($6::text IS NULL OR model = $6)
          AND ($7::text IS NULL OR status = $7)
        "#,
    )
    .bind(from)
    .bind(to)
    .bind(user_id)
    .bind(project_id)
    .bind(provider.clone())
    .bind(model.clone())
    .bind(status.clone())
    .fetch_one(db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    // Il breakdown deve usare gli stessi filtri del summary, altrimenti
    // l'utente vede righe che non corrispondono ai filtri impostati.
    let by_provider = sqlx::query(
        r#"
        SELECT provider, model,
               COALESCE(SUM(total_tokens), 0)::bigint AS total_tokens,
               COALESCE(SUM(total_cost), 0)::float8 AS total_cost,
               COUNT(*)::bigint AS runs
        FROM ai_usage_ledger
        WHERE created_at >= $1
          AND created_at < $2
          AND ($3::uuid IS NULL OR user_id = $3)
          AND ($4::uuid IS NULL OR project_id = $4)
          AND ($5::text IS NULL OR provider = $5)
          AND ($6::text IS NULL OR model = $6)
          AND ($7::text IS NULL OR status = $7)
        GROUP BY provider, model
        ORDER BY total_cost DESC
        LIMIT 100
        "#,
    )
    .bind(from)
    .bind(to)
    .bind(user_id)
    .bind(project_id)
    .bind(provider)
    .bind(model)
    .bind(status)
    .fetch_all(db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    let list: Vec<Value> = by_provider
        .into_iter()
        .map(|row| {
            json!({
                "provider": row.get::<String, _>("provider"),
                "model": row.get::<String, _>("model"),
                "total_tokens": row.get::<i64, _>("total_tokens"),
                "total_cost": row.get::<f64, _>("total_cost"),
                "runs": row.get::<i64, _>("runs"),
            })
        })
        .collect();

    Ok(Json(json!({
        "date_from": from,
        "date_to": to,
        "summary": {
            "total_tokens": summary.get::<i64, _>("total_tokens"),
            "total_cost": summary.get::<f64, _>("total_cost"),
            "total_runs": summary.get::<i64, _>("total_runs"),
        },
        "breakdown": list
    })))
}

