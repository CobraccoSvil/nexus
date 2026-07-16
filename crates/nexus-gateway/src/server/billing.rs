//! Enforcement quota e registrazione usage nel ledger.
//!
//! Convenzione Nexus: `tenant_id = project_id` (UUID).
//!
//! Regola L: il LISTINO (quanto costa un modello) vive nel punto unico
//! `nexus-pricing`, non qui. Questo modulo tiene solo cio' che e' suo: la POLICY
//! di quota e la scrittura del ledger. La differenza conta — la domanda "quanto
//! costa (provider, model)?" e' una sola, mentre "cosa faccio se non lo so"
//! dipende dal chiamante, e qui la risposta e' sempre "degrada e annota, mai
//! respingere la richiesta".
//!
//! NB storica: una versione precedente di questa doc indicava
//! `crates/billing-service` come "API interna autoritativa" verso cui far
//! convergere il gateway alla "Fase 6". Era la direzione sbagliata: quel crate e'
//! un fork divergente che non scrive alcuna riga di ledger e porta ancora i
//! difetti (default currency EUR, filtro `is_enabled` sulla contabilita') che
//! mcp-core aveva gia' corretto. La convergenza e' su `nexus-pricing`.
//!
//! Regola F: nessun prompt/response/segreto nei log; solo importi/conteggi.

use anyhow::Result;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use nexus_pricing::{calculate_cost, resolve_active_price_in, PriceLookup};

use crate::types::{LlmRequest, LlmResponse, MessageContent};

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

/// Listino di (provider, model) + currency di piattaforma, dal punto unico.
///
/// DEGRADO ESPLICITO (policy del gateway): se la currency non e' configurata o il
/// DB del listino non risponde, questa funzione NON propaga l'errore. Il motivo e'
/// che i suoi chiamanti stanno sul percorso della richiesta: `enforce_quota`
/// propaga con `?` e il suo errore diventa una richiesta RESPINTA. Far fallire una
/// chiamata LLM perche' non sappiamo prezzarla sostituirebbe una sottostima con un
/// outage — un prezzo troppo alto per un problema di contabilita'.
///
/// La visibilita' che la regola G esige non viene sacrificata: si ottiene ALL'AVVIO
/// con `nexus_pricing::assert_configured`, dove fallire e' gratuito, piu' il WARN
/// qui sotto e `details.price_state` sulla riga di ledger.
async fn lookup_price(db: &PgPool, provider: &str, model: &str) -> PriceLookup {
    let currency = match nexus_pricing::platform_currency(db).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "gateway-billing: currency di piattaforma non risolvibile -> costo non calcolabile \
                 (la richiesta prosegue: vedi assert_configured all'avvio)"
            );
            return PriceLookup::NotInCatalog;
        }
    };
    match resolve_active_price_in(db, provider, model, &currency).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, provider = %provider, model = %model,
                "gateway-billing: lettura listino fallita -> costo non calcolabile");
            PriceLookup::NotInCatalog
        }
    }
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

    // Solo per il log della quota superata: se non e' risolvibile lo si dice,
    // non si inventa una valuta (regola G).
    let currency = nexus_pricing::platform_currency(db)
        .await
        .unwrap_or_else(|_| "currency non configurata".to_string());

    let estimated_prompt = estimate_prompt_tokens(req);
    let estimated_completion = req.max_tokens.map(|t| t as i64).unwrap_or(0);
    let estimated_total = estimated_prompt + estimated_completion;

    // Stima per le quote: senza listino resta 0 (non si inventa un prezzo, e
    // rifiutare qui sarebbe un cambio di policy). Lo zero e' pero' dichiarato,
    // non implicito: `Unknown` viene loggato perche' una stima a 0 non consuma
    // quota di costo e lascia sforare in silenzio.
    let estimated_cost = match lookup_price(db, provider, model).await {
        PriceLookup::Priced(p) => calculate_cost(&p, estimated_prompt, estimated_completion).2,
        PriceLookup::Unknown => {
            tracing::warn!(
                provider = %provider,
                model = %model,
                "gateway-quota: prezzo IGNOTO (pricing_state='unknown') -> stima costo 0, \
                 la quota di costo non viene consumata per questa chiamata"
            );
            0.0
        }
        PriceLookup::NotInCatalog => 0.0,
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

    let price = lookup_price(db, provider, model).await;
    // `price_state` e' il segnale STRUTTURATO del perche' di un costo: chi legge il
    // ledger distingue "0 perche' gratis" da "0 perche' non so quanto costa" senza
    // dedurlo dall'importo. `price_missing` resta per i lettori esistenti ed e'
    // `true` in ENTRAMBI i casi di costo non calcolabile.
    let price_state = price.state_label();
    let price_missing = price.is_missing();
    let (currency, input_cost, output_cost, total_cost) = match &price {
        PriceLookup::Priced(p) => {
            let (i, o, t) = calculate_cost(p, prompt_tokens, completion_tokens);
            (p.currency.trim().to_uppercase(), i, o, t)
        }
        _ => {
            if matches!(price, PriceLookup::Unknown) {
                tracing::warn!(
                    provider = %provider,
                    model = %model,
                    prompt_tokens,
                    completion_tokens,
                    "gateway-ledger: prezzo IGNOTO (pricing_state='unknown') -> costo NON calcolabile, \
                     registro 0 esplicito. Il modello non dovrebbe essere routabile: vedi il ciclo \
                     reconcile_disable_price_unknown del catalog_sync"
                );
            }
            // Costo 0 -> la currency e' vacua, ma la colonna e' NOT NULL: si annota
            // quella di piattaforma. Se nemmeno quella e' leggibile il DB e' giu' e
            // l'INSERT qui sotto fallisce comunque, quindi la stringa vuota non
            // raggiunge una riga persistita.
            let cur = nexus_pricing::platform_currency(db).await.unwrap_or_default();
            (cur, 0.0, 0.0, 0.0)
        }
    };

    let details = json!({
        "request_id": req.metadata.request_id,
        "feature": req.metadata.feature,
        "price_missing": price_missing,
        "price_state": price_state,
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
                    reasoning: None,
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

    // NB: il test `calculate_cost_scala_per_milione` (e il clamp dei token
    // negativi) vive ora accanto alla funzione, in `nexus-pricing`. Riprodurlo qui
    // testerebbe una funzione che questo crate non possiede piu'.

    #[test]
    fn quota_exceeded_display() {
        let e = QuotaExceeded {
            scope: "user".into(),
            reason: "token_limit".into(),
        };
        assert_eq!(e.to_string(), "quota_exceeded:user:token_limit");
    }
}
