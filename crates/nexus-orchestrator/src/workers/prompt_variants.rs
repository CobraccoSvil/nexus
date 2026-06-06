//! Utilita' condivise per la gestione delle varianti di prompt.
//!
//! Punto unico (regola L) per la logica riusata dai worker che generano
//! varianti di prompt e registrano esperimenti A/B:
//! - `PromptOptimizerWorker` (loop reflection-driven)
//! - `GuidelineAlignmentWorker` (loop guideline-driven)
//!
//! Concentra qui due responsabilita' che prima vivevano dentro
//! `prompt_optimizer.rs`:
//! 1. la safelist dei prefissi di chiave non ottimizzabili automaticamente;
//! 2. l'inserimento atomico di una variante + esperimento canary, ereditando
//!    `schema_type` e `placeholder_vars` dalla riga baseline (fix del bug per
//!    cui venivano scritti valori hardcoded uguali per ogni prompt).

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use tracing::{error, info};

/// Modalita' della chiamata `POST /agent/prompt-revise` del brain.
#[derive(Debug, Clone, Copy)]
pub enum ReviseMode {
    /// Solo valutazione di conformita' (nessuna riscrittura del template).
    Evaluate,
    /// Valutazione + riscrittura: la risposta include `revised_template`.
    EvaluateAndRevise,
}

impl ReviseMode {
    fn as_str(self) -> &'static str {
        match self {
            ReviseMode::Evaluate => "evaluate",
            ReviseMode::EvaluateAndRevise => "evaluate_and_revise",
        }
    }
}

/// Origine dei segnali passati al brain: il loop guideline-driven o quello
/// reflection-driven. Determina come il brain pesa i criteri di valutazione.
#[derive(Debug, Clone, Copy)]
pub enum SignalKind {
    Guideline,
    Reflection,
}

impl SignalKind {
    fn as_str(self) -> &'static str {
        match self {
            SignalKind::Guideline => "guideline",
            SignalKind::Reflection => "reflection",
        }
    }
}

/// Esito strutturato di `POST /agent/prompt-revise` (contratto brain).
#[derive(Debug, Deserialize)]
pub struct PromptReviseResult {
    pub status: String,
    #[serde(default)]
    pub overall_score: f64,
    #[serde(default)]
    pub dimensions: serde_json::Value,
    #[serde(default)]
    pub issues: serde_json::Value,
    #[serde(default)]
    pub revised_template: Option<String>,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub model_used: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
}

impl PromptReviseResult {
    /// Normalizza `dimensions`/`issues` (mai NULL) a un JSON serializzabile.
    pub fn dimensions_json(&self) -> serde_json::Value {
        if self.dimensions.is_null() {
            serde_json::json!({})
        } else {
            self.dimensions.clone()
        }
    }

    pub fn issues_json(&self) -> serde_json::Value {
        if self.issues.is_null() {
            serde_json::json!([])
        } else {
            self.issues.clone()
        }
    }
}

#[derive(Debug, Serialize)]
struct PromptReviseSignals<'a> {
    kind: &'a str,
    weaknesses: &'a [String],
    metrics: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct PromptReviseRequest<'a> {
    current_template: &'a str,
    prompt_key: &'a str,
    mode: &'a str,
    signals: PromptReviseSignals<'a>,
}

/// Risolve l'URL base del brain dal DB (settings.brain_rest_url, regola G:
/// niente env hardcoded, unica fonte di verita'). `Err` se non configurato.
async fn brain_base_url(pool: &PgPool) -> Result<String, String> {
    nexus_auth::get_setting(pool, "brain_rest_url")
        .await
        .map(|v| v.trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            "settings.brain_rest_url non configurato: impossibile raggiungere /agent/prompt-revise"
                .to_string()
        })
}

/// Punto unico (regola L) per chiamare il conformance/revision check del brain.
///
/// `weaknesses`/`metrics` sono i segnali (vuoti per il loop guideline). La
/// scelta del modello e' responsabilita' del brain (routing tier-only, purpose
/// `prompt_conformance_check`): nessun modello/URL hardcoded qui.
///
/// Ritorna `None` se l'URL non e' configurato, la chiamata HTTP fallisce o la
/// risposta non e' valida (errore loggato, mai panico).
pub async fn call_prompt_revise(
    pool: &PgPool,
    prompt_key: &str,
    current_template: &str,
    mode: ReviseMode,
    kind: SignalKind,
    weaknesses: &[String],
    metrics: serde_json::Value,
) -> Option<PromptReviseResult> {
    let base = match brain_base_url(pool).await {
        Ok(b) => b,
        Err(e) => {
            error!("prompt_revise: {e}");
            return None;
        }
    };

    let request = PromptReviseRequest {
        current_template,
        prompt_key,
        mode: mode.as_str(),
        signals: PromptReviseSignals {
            kind: kind.as_str(),
            weaknesses,
            metrics,
        },
    };

    let client = nexus_http::build_client();
    let response = match client
        .post(format!("{base}/agent/prompt-revise"))
        .json(&request)
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            error!("prompt_revise: errore HTTP brain /agent/prompt-revise: {e}");
            return None;
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        let body_text = response.text().await.unwrap_or_default();
        error!(
            "prompt_revise: brain /agent/prompt-revise error {} per '{}': {}",
            status,
            prompt_key,
            &body_text[..body_text.len().min(200)]
        );
        return None;
    }

    match response.json::<PromptReviseResult>().await {
        Ok(result) => Some(result),
        Err(e) => {
            error!("prompt_revise: parse risposta brain per '{prompt_key}': {e}");
            None
        }
    }
}

/// Prefissi di chiave prompt che non vengono mai ottimizzati automaticamente.
/// I prompt `system.*` e `automation.*` sono protetti: per loro si generano
/// proposte di revisione da approvare a mano, mai varianti auto-applicate.
const SAFELIST_PREFIXES: &[&str] = &[
    "system.",
    "automation.",
    "system.nexus",
    "automation.mode_",
];

/// Restituisce true se la chiave e' nella safelist (prompt protetto, non
/// ottimizzabile automaticamente via esperimento A/B).
pub fn is_safelisted(key: &str) -> bool {
    SAFELIST_PREFIXES
        .iter()
        .any(|prefix| key.starts_with(prefix))
}

/// Inserisce una variante come nuova versione inattiva/sperimentale del template
/// e registra l'esperimento canary in `prompt_ab_experiments`.
///
/// Punto unico (regola L) condiviso tra optimizer e alignment worker.
///
/// La nuova versione e' `baseline_version + 1`. `schema_type` e
/// `placeholder_vars` vengono EREDITATI dalla riga baseline
/// (`nexus_prompt_templates` chiave+versione): in passato erano scritti
/// hardcoded (`'xml'` + lista placeholder fissa) per ogni prompt, il che
/// corrompeva le varianti di template con schema o placeholder diversi.
///
/// Tutte le scritture sono idempotenti (`ON CONFLICT DO NOTHING`).
pub async fn insert_variant_and_experiment(
    pool: &PgPool,
    prompt_key: &str,
    baseline_version: i32,
    variant_content: &str,
    traffic_pct: i64,
) -> Result<(), sqlx::Error> {
    let new_version = baseline_version + 1;

    // Eredita schema_type e placeholder_vars dalla baseline: niente hardcode.
    // Se la baseline non esiste (caso anomalo), usa valori neutri coerenti col
    // default di schema ('xml' + array vuoto) senza inventare placeholder.
    let baseline = sqlx::query(
        r#"
        SELECT schema_type, placeholder_vars
        FROM nexus_prompt_templates
        WHERE key = $1 AND version = $2
        "#,
    )
    .bind(prompt_key)
    .bind(baseline_version)
    .fetch_optional(pool)
    .await?;

    let (schema_type, placeholder_vars): (String, serde_json::Value) = match baseline {
        Some(row) => (
            row.get::<String, _>("schema_type"),
            row.get::<serde_json::Value, _>("placeholder_vars"),
        ),
        None => ("xml".to_string(), serde_json::json!([])),
    };

    // Inserisce la nuova versione come inattiva e sperimentale, ereditando
    // schema_type e placeholder_vars dalla baseline.
    sqlx::query(
        r#"
        INSERT INTO nexus_prompt_templates
            (key, version, content, is_active, experimental,
             schema_type, placeholder_vars, updated_by)
        VALUES ($1, $2, $3, FALSE, TRUE, $4, $5, 'prompt_variant')
        ON CONFLICT (key, version) DO NOTHING
        "#,
    )
    .bind(prompt_key)
    .bind(new_version)
    .bind(variant_content)
    .bind(&schema_type)
    .bind(&placeholder_vars)
    .execute(pool)
    .await?;

    // Registra l'esperimento canary.
    sqlx::query(
        r#"
        INSERT INTO prompt_ab_experiments
            (prompt_key, baseline_version, variant_version,
             traffic_pct, status, auto_promote_enabled)
        VALUES ($1, $2, $3, $4, 'running', FALSE)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(prompt_key)
    .bind(baseline_version)
    .bind(new_version)
    .bind(traffic_pct as i32)
    .execute(pool)
    .await?;

    info!(
        "prompt_variant: variante v{} inserita per '{}' (traffic={}%, schema={})",
        new_version, prompt_key, traffic_pct, schema_type
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_safelisted_protected_prefixes() {
        assert!(is_safelisted("system.nexus_base"));
        assert!(is_safelisted("automation.mode_confirm"));
        assert!(is_safelisted("system.foo"));
        assert!(is_safelisted("automation.bar"));
    }

    #[test]
    fn test_is_safelisted_agent_not_protected() {
        assert!(!is_safelisted("agent.coder.base"));
        assert!(!is_safelisted("agent.general.debugger"));
        assert!(!is_safelisted("foo.bar"));
    }
}
