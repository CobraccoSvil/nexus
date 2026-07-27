//! Adapter contabile di mcp-core, piu' i report admin.
//!
//! Regola L. Qui non vive piu' ne' il LISTINO (crate `nexus-pricing`) ne' la
//! CONTABILITA' (crate `nexus-ledger`: quale riga si scrive, quanto ha consumato
//! uno scope, chi addebita una chiamata). Restano tre cose, e sono davvero di
//! questo crate:
//!
//! 1. gli ADAPTER dai tipi di mcp-core (la `GwUsage` del client HTTP, il `Value`
//!    di una completion) verso il vocabolario del ledger;
//! 2. i REPORT (`usage_report`, `get_session_usage`) e gli handler HTTP di
//!    amministrazione di listino e quote: sono letture di presentazione e
//!    scritture di POLICY, non la contabilita' di una chiamata;
//! 3. niente SQL su `ai_usage_ledger` che scriva: quello ha un solo posto.
//!
//! Prima di questa separazione, le funzioni contabili di questo modulo erano le
//! gemelle di quelle del gateway, tenute allineate a mano — il commento sopra
//! `SQL_UPDATE_LEDGER_FINALIZE` lo dichiarava — e divergevano gia'. Il racconto
//! per esteso e le divergenze misurate stanno nel doc del crate `nexus-ledger`.

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{auth::Claims, AppState};

// Contabilita': punto unico `nexus-ledger`. Il vocabolario e' ri-esportato
// perche' i call site di mcp-core lo nominano nelle proprie firme.
//
// I ponti verso `nexus-pricing` (`calculate_cost`, `PriceLookup`) non ci sono
// piu': esistevano per i call site che li importavano da qui quando il listino
// viveva in questo modulo, e oggi nessuno lo fa — chi prezza chiama il punto
// unico direttamente.
pub use nexus_ledger::{Declaration, LedgerUsage, Reservation, Settlement};

/// I token di una chiamata come li registra il ledger, dalla risposta del
/// gateway.
///
/// PUNTO UNICO della conversione (regola L): prima ogni call site ricostruiva i
/// conteggi dai soli `input_tokens`/`output_tokens`, dimenticando i campi di
/// cache che `GwUsage` porta gia' — ed era il terzo percorso in cui quel dato
/// spariva.
pub fn usage_from_gateway(u: &crate::nexus_gateway::GwUsage) -> LedgerUsage {
    LedgerUsage::derived(nexus_pricing::TokenUsage {
        prompt_tokens: u.input_tokens as i64,
        completion_tokens: u.output_tokens as i64,
        cache_read_tokens: u.cache_read_tokens.unwrap_or(0) as i64,
        cache_creation_tokens: u.cache_creation_tokens.unwrap_or(0) as i64,
    })
}

/// L'esito contabile dichiarato dal gateway dentro un `Value` di completion
/// (`metadata.ledger`, scritto da
/// `orchestrator::neural_client::completion_value_from_gw`).
///
/// Gemella di [`extract_usage_numbers`] e per lo stesso motivo: i call site che
/// lavorano sul `Value` invece che sulla `GwResponse` devono poter porre la
/// STESSA domanda, o la decisione di `nexus_ledger::settle` si biforca.
///
/// I tre esiti di lettura restano DISTINTI. Prima la funzione chiudeva con un
/// `.ok()`, e una malformazione del JSON diventava `None`: cioe' "nessuno ha
/// addebitato", cioe' finalizza, cioe' doppio addebito — la stessa fine del
/// difetto del 2026-07-27, presa da un'altra porta e in silenzio. Un campo
/// presente e illeggibile non e' un campo assente: e' un contratto divergente
/// fra i due lati del wire, e come tale si DICE (regola M).
pub fn extract_ledger_declaration(completion: &Value) -> Declaration {
    // `Value::get` su un valore non-oggetto ritorna `None`: il `metadata`
    // mancante e il `ledger` mancante collassano qui, e sono la stessa cosa.
    let dichiarato = match completion.get("metadata").and_then(|m| m.get("ledger")) {
        None | Some(Value::Null) => return Declaration::Muta,
        Some(v) => v,
    };
    match serde_json::from_value(dichiarato.clone()) {
        Ok(outcome) => Declaration::Detta(outcome),
        Err(e) => {
            // Regola F: solo l'errore di deserializzazione, nessun payload.
            tracing::error!(
                target: "billing",
                error = %e,
                "ledger: dichiarazione contabile presente e ILLEGGIBILE nella completion. \
                 I due lati del wire hanno contratti divergenti: chi addebita questa \
                 chiamata si sta decidendo alla cieca"
            );
            Declaration::Illeggibile
        }
    }
}

/// Token dal `metadata.usage` di una completion.
///
/// La forma la produce `orchestrator::neural_client::usage_value_from_gw`, che
/// scrive `input_tokens`/`output_tokens` piu' `cache_read_tokens` /
/// `cache_creation_tokens` quando il provider li riporta. Le due chiavi di cache
/// erano gia' li' e venivano scartate qui: e' il punto in cui il dato spariva.
pub fn extract_usage_numbers(
    completion: &Value,
    prompt_tokens_fallback: i32,
    completion_tokens_fallback: i32,
) -> LedgerUsage {
    let usage = completion["metadata"]["usage"].clone();

    let prompt_tokens = usage["prompt_tokens"]
        .as_i64()
        .or_else(|| usage["input_tokens"].as_i64())
        .unwrap_or(prompt_tokens_fallback as i64)
        .max(0);
    let completion_tokens = usage["completion_tokens"]
        .as_i64()
        .or_else(|| usage["output_tokens"].as_i64())
        .or_else(|| usage["candidates_token_count"].as_i64())
        .unwrap_or(completion_tokens_fallback as i64)
        .max(0);
    let tokens = nexus_pricing::TokenUsage {
        prompt_tokens,
        completion_tokens,
        cache_read_tokens: usage["cache_read_tokens"].as_i64().unwrap_or(0).max(0),
        cache_creation_tokens: usage["cache_creation_tokens"].as_i64().unwrap_or(0).max(0),
    };

    // `total_tokens` dichiarato dalla fonte prevale sul derivato (percorsi che
    // riportano un totale proprio); altrimenti resta prompt lordo + completion.
    match usage["total_tokens"].as_i64() {
        Some(dichiarato) => LedgerUsage::with_declared_total(tokens, dichiarato),
        None => LedgerUsage::derived(tokens),
    }
}

/// Prenota il consumo prima della chiamata. Adapter: mcp-core porta l'identita'
/// come due UUID sciolti, il punto unico la vuole insieme.
pub async fn reserve_usage(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    provider: &str,
    model: &str,
    prompt_tokens: i32,
    estimated_completion_tokens: i32,
    details: Value,
) -> anyhow::Result<Reservation> {
    nexus_ledger::reserve(
        db,
        nexus_ledger::Identity {
            user_id,
            project_id,
        },
        provider,
        model,
        prompt_tokens,
        estimated_completion_tokens,
        details,
    )
    .await
}

/// Rilascia una prenotazione: la riga esce dalla contabilita' e dalle quote.
pub async fn release_usage(
    db: &PgPool,
    reservation: &Reservation,
    reason: &str,
    extra_details: Option<Value>,
) {
    nexus_ledger::release(db, reservation, reason, extra_details).await
}

/// Chiude la contabilita' di una chiamata riuscita.
///
/// Chi addebita lo decide il punto unico, dal segnale strutturato che il gateway
/// emette solo se ha davvero scritto la sua riga (regola M). La dichiarazione
/// arriva INTERA e non gia' ridotta a "c'e' una riga / non c'e'": la riduzione
/// e' la decisione, e un call site che la facesse per conto suo ne avrebbe una
/// copia (regola L).
pub async fn settle_usage(
    db: &PgPool,
    reservation: &Reservation,
    run_id: Uuid,
    usage: &LedgerUsage,
    declaration: &Declaration,
) -> anyhow::Result<Settlement> {
    nexus_ledger::settle(db, reservation, run_id, usage, declaration).await
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nexus_gateway::GwResponse;

    // I test della SCRITTURA del ledger (quali colonne, quali bind, una sola
    // riga finalizzata per chiamata, le quote prima e dopo) vivono nel punto
    // unico: `crates/nexus-ledger`. Qui restano quelli degli ADAPTER, che sono
    // cio' che questo modulo fa davvero.

    /// Risposta del gateway come arriva DAVVERO: deserializzata dal JSON del
    /// wire, la stessa strada di `NexusGatewayClient::complete`. Costruire la
    /// `GwUsage` a mano fisserebbe l'assunto che si vuole verificare.
    fn gw_resp_con_cache() -> GwResponse {
        // `input_tokens` e' il prompt LORDO: i 2M letti da cache e i 0.5M scritti
        // ne fanno parte, quindi 1M resta a tariffa piena.
        serde_json::from_str(
            r#"{
                "content": "ok",
                "usage": {
                    "input_tokens": 3500000,
                    "output_tokens": 400000,
                    "cache_read_tokens": 2000000,
                    "cache_creation_tokens": 500000
                },
                "model_used": "claude-x",
                "provider_used": "anthropic",
                "latency_ms": 12,
                "finish_reason": "stop"
            }"#,
        )
        .expect("payload wire del gateway")
    }

    /// `usage_from_gateway` e' il PUNTO UNICO della conversione: prima ogni call
    /// site ricostruiva i conteggi dai soli input/output e le due quantita' di
    /// cache sparivano qui. Il test parte dal wire, non da una `GwUsage`
    /// fabbricata.
    #[test]
    fn usage_dal_wire_porta_le_quattro_quantita() {
        let n = usage_from_gateway(&gw_resp_con_cache().usage);
        assert_eq!(n.tokens.prompt_tokens, 3_500_000, "lordo, come sul wire");
        assert_eq!(n.tokens.completion_tokens, 400_000);
        assert_eq!(n.tokens.cache_read_tokens, 2_000_000);
        assert_eq!(n.tokens.cache_creation_tokens, 500_000);
        // Il totale e' prompt lordo + completion: la cache e' gia' dentro.
        assert_eq!(n.total_tokens, 3_900_000);
    }

    /// Provider che non riporta la cache: i due campi assenti dal wire valgono
    /// zero e il totale torna a essere prompt + completion.
    #[test]
    fn usage_senza_cache_sul_wire_resta_la_somma_di_due() {
        let resp: GwResponse = serde_json::from_str(
            r#"{"content":"","usage":{"input_tokens":30,"output_tokens":12},
                "model_used":"m","provider_used":"p","latency_ms":1,"finish_reason":"stop"}"#,
        )
        .expect("payload wire senza campi di cache");
        let n = usage_from_gateway(&resp.usage);
        assert_eq!(n.tokens.cache_read_tokens, 0);
        assert_eq!(n.tokens.cache_creation_tokens, 0);
        assert_eq!(n.total_tokens, 42);
    }

    /// Dal `Value` di una completion: stessa domanda, altra forma dell'input.
    /// I due estrattori devono dare la STESSA risposta, o la decisione di
    /// `settle` si biforca a seconda di quale strada ha preso la chiamata.
    #[test]
    fn i_due_estrattori_leggono_gli_stessi_quattro_numeri() {
        let completion = json!({
            "content": "ok",
            "metadata": { "usage": {
                "input_tokens": 3_500_000,
                "output_tokens": 400_000,
                "cache_read_tokens": 2_000_000,
                "cache_creation_tokens": 500_000
            }}
        });
        let dal_value = extract_usage_numbers(&completion, 0, 0);
        let dal_wire = usage_from_gateway(&gw_resp_con_cache().usage);
        assert_eq!(dal_value.tokens, dal_wire.tokens);
        assert_eq!(dal_value.total_tokens, dal_wire.total_tokens);
    }

    /// Il totale DICHIARATO dalla fonte prevale sul derivato, e i fallback
    /// entrano in gioco solo quando il campo manca del tutto.
    #[test]
    fn il_totale_dichiarato_prevale_e_i_fallback_coprono_lassenza() {
        let con_totale = json!({ "metadata": { "usage": {
            "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 99
        }}});
        assert_eq!(extract_usage_numbers(&con_totale, 0, 0).total_tokens, 99);

        // Nessun usage: restano i fallback del chiamante (la stima).
        let vuoto = json!({ "content": "ok" });
        let n = extract_usage_numbers(&vuoto, 700, 300);
        assert_eq!(n.tokens.prompt_tokens, 700);
        assert_eq!(n.tokens.completion_tokens, 300);
        assert_eq!(n.total_tokens, 1000);
    }

    /// L'ASSENZA e' legittima e si legge come tale.
    ///
    /// Il giro completo sul `Value` — dal produttore vero al consumatore vero —
    /// vive dove il produttore e' raggiungibile
    /// (`orchestrator::neural_client`, `la_dichiarazione_del_gateway_sopravvive_al_giro`):
    /// qui restano le forme che quel produttore non puo' produrre, cioe' proprio
    /// quelle su cui la funzione decideva in silenzio.
    #[test]
    fn una_completion_senza_dichiarazione_e_muta() {
        // Nessun `metadata`: percorsi che non passano dal gateway.
        assert_eq!(
            extract_ledger_declaration(&json!({ "content": "ok" })).as_str(),
            "undeclared"
        );
        // `metadata` senza la chiave: gateway che non emette il campo.
        assert_eq!(
            extract_ledger_declaration(&json!({ "metadata": { "usage": {} } })).as_str(),
            "undeclared"
        );
        // Chiave presente e `null`: il gateway non ha dichiarato nulla.
        assert_eq!(
            extract_ledger_declaration(&json!({ "metadata": { "ledger": null } })).as_str(),
            "undeclared"
        );
    }

    /// Una dichiarazione presente e ILLEGGIBILE non e' un'assenza.
    ///
    /// E' il difetto [5]: con `.ok()` la malformazione diventava `None`, `None`
    /// significa "nessuno ha addebitato", e la prenotazione veniva finalizzata
    /// davanti a una riga che il gateway PUO' aver scritto. Il doppio addebito,
    /// di nuovo, in silenzio.
    ///
    /// MUTAZIONE: rimettendo `serde_json::from_value(...).ok()` al posto del
    /// match, tutte e tre le forme malformate diventano `undeclared` e questo
    /// test rosseggia con "illeggibile scambiato per assente".
    #[test]
    fn una_dichiarazione_illeggibile_non_e_unassenza() {
        for malformata in [
            // Tag sconosciuto: contratto piu' nuovo dall'altra parte.
            json!({ "outcome": "partially_written" }),
            // Forma vecchia (la `LedgerEntry` nuda, senza tag): e' esattamente
            // cio' che manderebbe un gateway rimasto indietro di un deploy.
            json!({ "id": "6f1b1d0e-0000-4000-8000-000000000000",
                    "total_cost": 0.5, "currency": "USD" }),
            // Tipo sbagliato su un campo che porta denaro.
            json!({ "outcome": "written", "id": "6f1b1d0e-0000-4000-8000-000000000000",
                    "total_cost": "0.5", "currency": "USD" }),
        ] {
            let completion = json!({ "metadata": { "ledger": malformata.clone() } });
            let letto = extract_ledger_declaration(&completion);
            assert_eq!(
                letto.as_str(),
                "unreadable",
                "illeggibile scambiato per assente: {malformata}"
            );
            assert!(letto.entry().is_none());
            // E il verdetto lo dice, comunque sia partita la chiamata: e' un
            // difetto di contratto, non un caso legittimo.
            assert_eq!(
                letto.audit(false),
                nexus_ledger::DeclarationAudit::Illeggibile
            );
            assert!(letto.audit(true).sospetta());
        }
    }
}
