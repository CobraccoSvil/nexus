use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};

use crate::AppState;

#[derive(Debug, Serialize)]
pub struct PurposeModelEntry {
    pub purpose: String,
    /// Fascia di capacita' della selezione dinamica dal catalog (mig 0203,
    /// tier-only). Il pin statico provider/model_id NON esiste piu' (mig 0723):
    /// mostrava una configurazione che il resolver ignorava — misurato il
    /// 2026-07-16, figure dichiarate deepseek giravano su groq. `None` = riga
    /// storica senza tier: non risolvibile, e l'update la rifiuta (400).
    pub tier: Option<String>,
    pub required_capability: Option<String>,
    pub requires_tool_use: bool,
    pub notes: Option<String>,
    pub updated_at: String,
    /// Cosa risolve DAVVERO questo purpose adesso, chiedendolo al resolver.
    ///
    /// Perche' esiste: il pannello mostrava `provider/model_id` come se fosse il
    /// modello del purpose. Misurato il 2026-07-16: le figure del consiglio
    /// dichiaravano `deepseek/deepseek-v4-flash` mentre giravano su
    /// `groq/gpt-oss-20b` (agentic_index 3.1) — il resolver ignora il campo
    /// statico e la pagina mostrava un dato che nessuno usa. Un pannello che
    /// mostra una configurazione invece dell'effetto e' peggio di un pannello
    /// vuoto: sembra una verifica, ed e' una bugia.
    pub resolved: Option<ResolvedPurposeModel>,
}

/// La risoluzione LIVE di un purpose: chi risponderebbe adesso, e perche'.
#[derive(Debug, Serialize)]
pub struct ResolvedPurposeModel {
    pub provider: String,
    pub model: String,
    /// Segnale STRUTTURATO del servizio (regola M): `tier=medium:auto` |
    /// `tier=medium:degraded_to=light` | `tier=medium:upgraded_to=high`.
    pub rationale: String,
}

#[derive(Debug, Serialize)]
pub struct ListPurposeModelsResponse {
    pub items: Vec<PurposeModelEntry>,
}

pub async fn list_purpose_models(
    State(state): State<AppState>,
) -> Result<Json<ListPurposeModelsResponse>, StatusCode> {
    let rows: Vec<(
        String,
        Option<String>,
        Option<String>,
        bool,
        Option<String>,
        String,
    )> = sqlx::query_as(
        r#"SELECT purpose, tier, required_capability,
                  requires_tool_use, notes, updated_at::text
           FROM nexus_purpose_model
           ORDER BY purpose"#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut items = Vec::with_capacity(rows.len());
    for (purpose, tier, required_capability, requires_tool_use, notes, updated_at) in rows {
        // La risoluzione si CHIEDE al resolver, non si ri-deriva qui (regola L):
        // e' lo stesso codice che decide durante un run, quindi il pannello non
        // puo' divergere da cio' che accade davvero. Senza tier non c'e' nulla
        // da chiedere: il purpose non risolve (il pin statico non esiste piu').
        let resolved = if tier.is_some() {
            resolve_live(&state, &purpose).await
        } else {
            None
        };
        items.push(PurposeModelEntry {
            purpose,
            tier,
            required_capability,
            requires_tool_use,
            notes,
            updated_at,
            resolved,
        });
    }

    Ok(Json(ListPurposeModelsResponse { items }))
}

/// Chiede al resolver chi risponderebbe ADESSO per `purpose`. `None` = non
/// risolvibile ora (catalog irraggiungibile, nessun modello capable, cooldown):
/// il pannello mostra l'assenza invece di un modello che non verrebbe usato.
async fn resolve_live(state: &AppState, purpose: &str) -> Option<ResolvedPurposeModel> {
    use crate::internal_routing::PurposeResolution;
    match crate::internal_routing::resolve_purpose_model_db(&state.db, purpose).await {
        PurposeResolution::Resolved {
            provider,
            model,
            rationale,
        } => Some(ResolvedPurposeModel {
            provider,
            model,
            rationale,
        }),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdatePurposeModelRequest {
    /// Categoria di modelli sulla scala a 5 livelli (light..frontier).
    /// OBBLIGATORIA dalla mig 0723: senza pin statico un purpose senza tier
    /// non risolve nulla (NotFound a runtime), e il pannello non deve poter
    /// produrre quello stato — ''/'static'/'none' rispondono 400.
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub required_capability: Option<String>,
    #[serde(default)]
    pub requires_tool_use: Option<bool>,
    pub notes: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UpdatePurposeModelResponse {
    pub status: String,
    pub purpose: String,
}

pub async fn update_purpose_model(
    State(state): State<AppState>,
    Path(purpose): Path<String>,
    Json(body): Json<UpdatePurposeModelRequest>,
) -> Result<Json<UpdatePurposeModelResponse>, StatusCode> {
    let purpose = purpose.trim();
    if purpose.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Normalizza il tier (punto unico testabile). 400 se valore non valido.
    // Il `None` normalizzato (''/'static'/'none') e' a sua volta un 400: senza
    // pin statico (mig 0723) un purpose senza tier e' configurazione morta, e
    // il pannello non deve poterla scrivere.
    let Some(tier) = normalize_tier(body.tier.as_deref()).map_err(|_| StatusCode::BAD_REQUEST)?
    else {
        return Err(StatusCode::BAD_REQUEST);
    };
    let required_capability: Option<String> = body
        .required_capability
        .as_deref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let requires_tool_use = body.requires_tool_use.unwrap_or(false);

    sqlx::query(
        r#"INSERT INTO nexus_purpose_model
               (purpose, tier, required_capability, requires_tool_use, notes)
           VALUES ($1, $2, $3, $4, $5)
           ON CONFLICT (purpose)
           DO UPDATE SET tier = EXCLUDED.tier,
                         required_capability = EXCLUDED.required_capability,
                         requires_tool_use = EXCLUDED.requires_tool_use,
                         notes = EXCLUDED.notes,
                         updated_at = NOW()"#,
    )
    .bind(purpose)
    .bind(tier)
    .bind(required_capability)
    .bind(requires_tool_use)
    .bind(body.notes.clone())
    .execute(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Invalida la cache routing matrix in modo best-effort: entro 60s si aggiorna comunque.
    // Qui non abbiamo un invalidate esplicito, quindi ci limitiamo a loggare.
    tracing::info!("admin: updated purpose_model {}", purpose);

    Ok(Json(UpdatePurposeModelResponse {
        status: "ok".to_string(),
        purpose: purpose.to_string(),
    }))
}

/// Normalizza il valore del tier ricevuto dall'admin in un `Option<String>`
/// adatto alla colonna `nexus_purpose_model.tier` (vincolo CHECK mig 0203, esteso
/// alla scala a 5 livelli light|medium|high|heavy|frontier dalla mig 0547):
///   - 'light' | 'medium' | 'high' | 'heavy' | 'frontier' (case-insensitive)
///     -> Some(valore normalizzato)
///   - None / '' / 'static' / 'none' -> None (NESSUN tier: la "selezione
///     statica" non esiste piu' dalla mig 0723, e `update_purpose_model`
///     risponde 400 su questo esito)
///   - qualunque altro valore -> Err (il chiamante risponde 400)
fn normalize_tier(raw: Option<&str>) -> Result<Option<String>, ()> {
    match raw.map(|s| s.trim().to_ascii_lowercase()) {
        // Validazione delegata al PUNTO UNICO del vocabolario tier (regola L):
        // la lista dei tier validi vive in un solo posto.
        Some(t) if nexus_agent_graph::decisions::tiers::is_performance_tier(&t) => Ok(Some(t)),
        Some(t) if t.is_empty() || t == "static" || t == "none" => Ok(None),
        None => Ok(None),
        Some(_) => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_tier_accetta_categorie_valide() {
        assert_eq!(normalize_tier(Some("light")).unwrap(), Some("light".into()));
        assert_eq!(
            normalize_tier(Some("MEDIUM")).unwrap(),
            Some("medium".into())
        );
        assert_eq!(
            normalize_tier(Some(" heavy ")).unwrap(),
            Some("heavy".into())
        );
        // Scala a 5 livelli (mig 0528/0547): high e frontier ora accettati.
        assert_eq!(normalize_tier(Some("high")).unwrap(), Some("high".into()));
        assert_eq!(
            normalize_tier(Some("FRONTIER")).unwrap(),
            Some("frontier".into())
        );
    }

    #[test]
    fn normalize_tier_mappa_statico_a_none() {
        assert_eq!(normalize_tier(None).unwrap(), None);
        assert_eq!(normalize_tier(Some("")).unwrap(), None);
        assert_eq!(normalize_tier(Some("static")).unwrap(), None);
        assert_eq!(normalize_tier(Some("none")).unwrap(), None);
    }

    #[test]
    fn normalize_tier_rifiuta_valori_non_validi() {
        assert!(normalize_tier(Some("ultra")).is_err());
        assert!(normalize_tier(Some("fast")).is_err());
    }
}
