//! Enforcement quota e registrazione usage nel ledger.
//!
//! Porting fedele delle funzioni `enforceQuota` / `recordUsageToLedger` /
//! `resolveActivePrice` / `calculateCost` di `server.ts`, che a loro volta
//! parlano alle stesse tabelle del `crates/billing-service`
//! (`ai_quota_policies`, `ai_usage_ledger`, `ai_price_catalog`).
//!
//! Convenzione Nexus (come nel server.ts): `tenant_id = project_id` (UUID).
//!
//! Regola L: la logica di prezzo/quota vive nel `billing-service` come API
//! interna autoritativa (`/internal/reserve|finalize|release`). Qui pero' il
//! gateway Node NON chiama quell'API: implementa la stima inline come "guardrail
//! perfetto" PRIMA della chiamata al provider (preview). Replichiamo lo stesso
//! comportamento per parita' funzionale alla Fase 5; la Fase 6 potra' far
//! convergere il gateway sull'API interna del billing-service (cosi' resta UN
//! solo punto di reservation/ledger transazionale). Documentato qui per non
//! perdere la traccia del consolidamento.
//!
//! Regola F: nessun prompt/response/segreto nei log; solo importi/conteggi.

use anyhow::Result;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::types::{LlmRequest, LlmResponse, MessageContent};

/// Snapshot di prezzo (parita' con `PriceSnapshot` del billing-service).
#[derive(Debug, Clone, sqlx::FromRow)]
struct PriceSnapshot {
    input_cost_per_million_tokens: f64,
    output_cost_per_million_tokens: f64,
    currency: String,
}

/// Riga di quota attiva (parita' con la query del server.ts).
#[derive(Debug, Clone, sqlx::FromRow)]
struct QuotaRow {
    scope_type: String,
    token_limit: Option<i64>,
    cost_limit: Option<f64>,
    valid_from: chrono::DateTime<chrono::Utc>,
    valid_to: chrono::DateTime<chrono::Utc>,
}

/// Quota superata: tradotta in HTTP 403 dal chiamante (come `QUOTA_EXCEEDED` -> 403).
#[derive(Debug, thiserror::Error)]
#[error("quota_exceeded:{scope}:{reason}")]
pub struct QuotaExceeded {
    pub scope: String,
    pub reason: String,
}

/// Currency di piattaforma (`billing_base_currency`, default EUR). Letta col
/// punto unico settings.
async fn platform_currency(db: &PgPool) -> String {
    nexus_auth::get_setting(db, "billing_base_currency")
        .await
        .map(|v| v.trim().to_uppercase())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "EUR".to_string())
}

/// Prezzo attivo per (provider, model) nella currency di piattaforma. `None` se
/// la voce non e' censita: il costo si tratta come 0 (parita' col server.ts).
async fn resolve_active_price(
    db: &PgPool,
    provider: &str,
    model: &str,
) -> Result<Option<PriceSnapshot>> {
    let currency = platform_currency(db).await;
    let price = sqlx::query_as::<_, PriceSnapshot>(
        r#"
        SELECT
            input_cost_per_million_tokens::float8 AS input_cost_per_million_tokens,
            output_cost_per_million_tokens::float8 AS output_cost_per_million_tokens,
            currency
        FROM ai_price_catalog
        WHERE provider = $1
          AND model = $2
          AND currency = $3
          AND is_enabled = TRUE
          AND effective_from <= NOW()
          AND (effective_to IS NULL OR effective_to > NOW())
        ORDER BY effective_from DESC
        LIMIT 1
        "#,
    )
    .bind(provider)
    .bind(model)
    .bind(&currency)
    .fetch_optional(db)
    .await?;
    Ok(price)
}

/// Costo (input, output, totale) dato il prezzo e i token. Parita' con `calculateCost`.
fn calculate_cost(price: &PriceSnapshot, prompt_tokens: i64, completion_tokens: i64) -> (f64, f64, f64) {
    let input_cost = (prompt_tokens.max(0) as f64 / 1_000_000.0) * price.input_cost_per_million_tokens;
    let output_cost =
        (completion_tokens.max(0) as f64 / 1_000_000.0) * price.output_cost_per_million_tokens;
    (input_cost, output_cost, input_cost + output_cost)
}

/// Stima i token di input dai messaggi (char/4, parita' col server.ts).
pub fn estimate_prompt_tokens(req: &LlmRequest) -> i64 {
    let chars: usize = req
        .messages
        .iter()
        .map(|m| match &m.content {
            MessageContent::Text(t) => t.len(),
            // I blocchi non-testo non contribuiscono alla stima char-based.
            MessageContent::Blocks(_) => 0,
        })
        .sum();
    // Ceil division per 4 (parita' con Math.ceil(chars/4) del server.ts).
    ((chars as i64) + 3) / 4
}

/// Enforce quota PRIMA della chiamata al provider (guardrail). Stima i token e il
/// costo, somma all'uso corrente del periodo e blocca se sfora un limite attivo.
/// No-op se mancano `tenant_id`/`user_id` o se non ci sono quote (parita' server.ts).
pub async fn enforce_quota(
    db: &PgPool,
    req: &LlmRequest,
    provider: &str,
    model: &str,
) -> Result<()> {
    let project_id = req.metadata.tenant_id.trim();
    let user_id = req.metadata.user_id.trim();
    if project_id.is_empty() || user_id.is_empty() {
        return Ok(());
    }
    let (Ok(project_uuid), Ok(user_uuid)) = (Uuid::parse_str(project_id), Uuid::parse_str(user_id))
    else {
        // Metadati non-UUID: il gateway non puo' applicare quote -> passa (no-op).
        return Ok(());
    };

    let currency = platform_currency(db).await;

    let estimated_prompt = estimate_prompt_tokens(req);
    let estimated_completion = req.max_tokens.map(|t| t as i64).unwrap_or(0);
    let estimated_total = estimated_prompt + estimated_completion;

    let estimated_cost = match resolve_active_price(db, provider, model).await? {
        Some(p) => calculate_cost(&p, estimated_prompt, estimated_completion).2,
        None => 0.0,
    };

    let quotas = sqlx::query_as::<_, QuotaRow>(
        r#"
        SELECT scope_type, token_limit::bigint AS token_limit, cost_limit::float8 AS cost_limit,
               valid_from, valid_to
        FROM ai_quota_policies
        WHERE is_enabled = TRUE
          AND valid_from <= NOW()
          AND valid_to > NOW()
          AND (
            (scope_type = 'user' AND user_id = $1) OR
            (scope_type = 'project' AND project_id = $2) OR
            (scope_type = 'user_project' AND user_id = $1 AND project_id = $2)
          )
        ORDER BY scope_type ASC
        "#,
    )
    .bind(user_uuid)
    .bind(project_uuid)
    .fetch_all(db)
    .await?;

    for q in &quotas {
        let (used_tokens, used_cost) =
            usage_for_scope(db, &q.scope_type, user_uuid, project_uuid, q.valid_from, q.valid_to)
                .await?;

        if let Some(limit) = q.token_limit {
            if used_tokens + estimated_total > limit {
                return Err(anyhow::Error::new(QuotaExceeded {
                    scope: q.scope_type.clone(),
                    reason: "token_limit".to_string(),
                }));
            }
        }
        if let Some(limit) = q.cost_limit {
            if used_cost + estimated_cost > limit {
                tracing::warn!(
                    scope = %q.scope_type,
                    currency = %currency,
                    provider,
                    "gateway: quota costo superata"
                );
                return Err(anyhow::Error::new(QuotaExceeded {
                    scope: q.scope_type.clone(),
                    reason: "cost_limit".to_string(),
                }));
            }
        }
    }

    Ok(())
}

/// Uso corrente (token, costo) per uno scope nel periodo del vincolo, su ledger
/// `reserved`/`finalized`. Parita' con la query del server.ts.
async fn usage_for_scope(
    db: &PgPool,
    scope_type: &str,
    user_id: Uuid,
    project_id: Uuid,
    valid_from: chrono::DateTime<chrono::Utc>,
    valid_to: chrono::DateTime<chrono::Utc>,
) -> Result<(i64, f64)> {
    // La clausola scope discrimina i predicati: usiamo $1/$2 condizionati su scope.
    let row = sqlx::query_as::<_, (i64, f64)>(
        r#"
        SELECT
            COALESCE(SUM(total_tokens), 0)::bigint AS tokens,
            COALESCE(SUM(total_cost), 0)::float8 AS cost
        FROM ai_usage_ledger
        WHERE status IN ('reserved', 'finalized')
          AND created_at >= $4
          AND created_at <  $5
          AND (
            ($3 = 'user'         AND user_id = $1) OR
            ($3 = 'project'      AND project_id = $2) OR
            ($3 = 'user_project' AND user_id = $1 AND project_id = $2)
          )
        "#,
    )
    .bind(user_id)
    .bind(project_id)
    .bind(scope_type)
    .bind(valid_from)
    .bind(valid_to)
    .fetch_one(db)
    .await?;
    Ok(row)
}

/// Registra l'usage effettivo nel ledger come `finalized` (parita' con
/// `recordUsageToLedger`). No-op se mancano metadati o usage. Best-effort: gli
/// errori sono loggati ma non interrompono la risposta al chiamante.
pub async fn record_usage_to_ledger(db: &PgPool, req: &LlmRequest, resp: &LlmResponse) {
    let project_id = req.metadata.tenant_id.trim();
    let user_id = req.metadata.user_id.trim();
    if project_id.is_empty() || user_id.is_empty() {
        return;
    }
    let (Ok(project_uuid), Ok(user_uuid)) = (Uuid::parse_str(project_id), Uuid::parse_str(user_id))
    else {
        return;
    };

    let provider = &resp.provider_used;
    let model = &resp.model_used;
    let prompt_tokens = resp.usage.input_tokens as i64;
    let completion_tokens = resp.usage.output_tokens as i64;
    let total_tokens = prompt_tokens + completion_tokens;

    let price = match resolve_active_price(db, provider, model).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "gateway-ledger: lettura prezzo fallita, registro costo 0");
            None
        }
    };
    let (currency, input_cost, output_cost, total_cost, price_missing) = match &price {
        Some(p) => {
            let (i, o, t) = calculate_cost(p, prompt_tokens, completion_tokens);
            (p.currency.trim().to_uppercase(), i, o, t, false)
        }
        None => (platform_currency(db).await, 0.0, 0.0, 0.0, true),
    };

    let details = json!({
        "request_id": req.metadata.request_id,
        "feature": req.metadata.feature,
        "price_missing": price_missing,
    });

    // run_id (= request_id nei metadata): abilita il breakdown costo per run /
    // sessione (M71). NULL se il chiamante non lo passa o non e' un UUID valido.
    let run_uuid = Uuid::parse_str(req.metadata.request_id.trim()).ok();

    let res = sqlx::query(
        r#"
        INSERT INTO ai_usage_ledger (
            user_id, project_id, run_id, provider, model,
            prompt_tokens, completion_tokens, total_tokens,
            input_cost, output_cost, total_cost,
            currency, status, details
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'finalized', $13
        )
        "#,
    )
    .bind(user_uuid)
    .bind(project_uuid)
    .bind(run_uuid)
    .bind(provider)
    .bind(model)
    .bind(prompt_tokens)
    .bind(completion_tokens)
    .bind(total_tokens)
    .bind(input_cost)
    .bind(output_cost)
    .bind(total_cost)
    .bind(currency)
    .bind(details)
    .execute(db)
    .await;

    if let Err(e) = res {
        // Regola F: solo l'errore SQL, nessun payload.
        tracing::warn!(error = %e, "gateway-ledger: insert ledger fallita (best-effort)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LlmMessage, RequestMetadata};

    fn req(messages: Vec<&str>, max_tokens: Option<u32>) -> LlmRequest {
        LlmRequest {
            model: "m".into(),
            messages: messages
                .into_iter()
                .map(|t| LlmMessage {
                    role: "user".into(),
                    content: MessageContent::Text(t.into()),
                    tool_call_id: None,
                    tool_calls: None,
                    name: None,
                    thinking_signature: None,
                })
                .collect(),
            temperature: None,
            max_tokens,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: RequestMetadata {
                tenant_id: "t".into(),
                user_id: "u".into(),
                request_id: "r".into(),
                sensitivity_tier: 0,
                feature: "chat".into(),
            },
        }
    }

    #[test]
    fn stima_token_char_su_4_arrotonda_per_eccesso() {
        // 9 char -> ceil(9/4) = 3.
        assert_eq!(estimate_prompt_tokens(&req(vec!["123456789"], None)), 3);
        // 8 char -> 2; somma su piu' messaggi.
        assert_eq!(
            estimate_prompt_tokens(&req(vec!["1234", "5678"], None)),
            2
        );
        // vuoto -> 0.
        assert_eq!(estimate_prompt_tokens(&req(vec![""], None)), 0);
    }

    #[test]
    fn calculate_cost_scala_per_milione() {
        let p = PriceSnapshot {
            input_cost_per_million_tokens: 2.0,
            output_cost_per_million_tokens: 6.0,
            currency: "EUR".into(),
        };
        // 1M input -> 2.0 ; 1M output -> 6.0 ; totale 8.0.
        let (i, o, t) = calculate_cost(&p, 1_000_000, 1_000_000);
        assert!((i - 2.0).abs() < 1e-9);
        assert!((o - 6.0).abs() < 1e-9);
        assert!((t - 8.0).abs() < 1e-9);
        // token negativi clampati a 0.
        let (i2, _, _) = calculate_cost(&p, -100, 0);
        assert_eq!(i2, 0.0);
    }

    #[test]
    fn quota_exceeded_display() {
        let e = QuotaExceeded {
            scope: "user".into(),
            reason: "token_limit".into(),
        };
        assert_eq!(e.to_string(), "quota_exceeded:user:token_limit");
    }
}
