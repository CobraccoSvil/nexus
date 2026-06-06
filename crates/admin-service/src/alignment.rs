//! Admin API (sola lettura): dashboard di allineamento direttive di prompt
//! engineering.
//!
//! Espone lo stato di conformita' dei template prompt, la knowledge base delle
//! direttive versionate e le proposte di revisione pending. Tutti gli handler
//! sono GET: l'MVP e' read-only, nessuna scrittura.
//!
//! Tabelle (sola lettura, mig 0346/0347):
//! - nexus_prompt_conformance  (esiti conformance, log append-only)
//! - nexus_prompt_guideline    (direttive versionate e approvate)
//! - nexus_alignment_proposal  (proposte di revisione SAFELIST)

use axum::{extract::State, http::StatusCode, Json};
use serde::Serialize;
use serde_json::Value;

use crate::AppState;

// ── Conformance per template ────────────────────────────────────────────────

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ConformanceRow {
    pub prompt_key: String,
    pub prompt_version: i32,
    /// NUMERIC(4,3) nel DB: 0.000..1.000.
    pub overall_score: f64,
    /// JSONB {alignment, structure, clarity, safety_preservation}.
    pub dimensions: Value,
    /// JSONB [{practice_key, severity, detail}].
    pub issues: Value,
    pub checked_at: chrono::DateTime<chrono::Utc>,
}

/// GET /api/admin/alignment/conformance
///
/// Per ogni prompt_key restituisce la riga di conformance piu' recente
/// (MAX checked_at) via DISTINCT ON, ordinata per score crescente cosi' i
/// template sotto soglia compaiono per primi.
pub async fn list_conformance(
    State(state): State<AppState>,
) -> Result<Json<Vec<ConformanceRow>>, StatusCode> {
    let rows = sqlx::query_as::<_, ConformanceRow>(
        "SELECT DISTINCT ON (prompt_key) \
            prompt_key, prompt_version, overall_score::float8 AS overall_score, \
            dimensions, issues, checked_at \
         FROM nexus_prompt_conformance \
         ORDER BY prompt_key, checked_at DESC",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("alignment.conformance query failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut rows = rows;
    // Ordina per score crescente: i template sotto soglia in cima.
    rows.sort_by(|a, b| {
        a.overall_score
            .partial_cmp(&b.overall_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(Json(rows))
}

// ── Direttive (knowledge base versionata) ───────────────────────────────────

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct GuidelineRow {
    pub practice_key: String,
    pub source: String,
    pub severity: String,
    pub applies_to: String,
    pub description: String,
    pub is_active: bool,
    pub approved_by: Option<String>,
}

/// GET /api/admin/alignment/guidelines
///
/// Knowledge base delle direttive: una riga per (practice_key, version), con
/// lo stato di attivazione/approvazione. Le piu' severe (must) per prime.
pub async fn list_guidelines(
    State(state): State<AppState>,
) -> Result<Json<Vec<GuidelineRow>>, StatusCode> {
    let rows = sqlx::query_as::<_, GuidelineRow>(
        "SELECT practice_key, source, severity, applies_to, description, is_active, approved_by \
         FROM nexus_prompt_guideline \
         ORDER BY \
            CASE severity WHEN 'must' THEN 0 WHEN 'should' THEN 1 ELSE 2 END, \
            practice_key, version DESC",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("alignment.guidelines query failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(rows))
}

// ── Proposte di revisione pending ───────────────────────────────────────────

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ProposalRow {
    pub id: uuid::Uuid,
    pub prompt_key: String,
    pub baseline_version: i32,
    pub rationale: Option<String>,
    pub trigger_source: String,
    pub status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// GET /api/admin/alignment/proposals
///
/// Proposte di revisione con status='pending' (prompt SAFELIST
/// system.*/automation.*), mai auto-applicate. Le piu' recenti per prime.
pub async fn list_proposals(
    State(state): State<AppState>,
) -> Result<Json<Vec<ProposalRow>>, StatusCode> {
    let rows = sqlx::query_as::<_, ProposalRow>(
        "SELECT id, prompt_key, baseline_version, rationale, trigger_source, status, created_at \
         FROM nexus_alignment_proposal \
         WHERE status = 'pending' \
         ORDER BY created_at DESC",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("alignment.proposals query failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(rows))
}
