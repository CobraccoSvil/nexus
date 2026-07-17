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

use serde::Deserialize;
use sqlx::{PgPool, Row};
use tracing::{error, info};

/// Esito canonico di una revisione andata a buon fine. E' il valore che
/// `PromptOptimizerWorker` e `GuidelineAlignmentWorker` verificano prima di
/// usare il risultato: sta qui, una volta sola, invece di essere una stringa
/// ripetuta nei call site (regola L/N).
pub const REVISE_STATUS_COMPLETED: &str = "completed";

/// Modalita' della valutazione richiesta al modello.
#[derive(Debug, Clone, Copy)]
pub enum ReviseMode {
    /// Solo valutazione di conformita' (nessuna riscrittura del template).
    Evaluate,
    /// Valutazione + riscrittura: la risposta include `revised_template`.
    EvaluateAndRevise,
}

impl ReviseMode {
    /// Istruzione che la modalita' impone al modello. Sta sull'enum: aggiungere
    /// una modalita' obbliga il compilatore a dichiararne l'istruzione.
    const fn instruction(self) -> &'static str {
        match self {
            ReviseMode::Evaluate => {
                "NON riscrivere il template: valuta soltanto. Lascia `revised_template` a null."
            }
            ReviseMode::EvaluateAndRevise => {
                "Riscrivi il template correggendo i problemi trovati, conservandone scopo, \
                 placeholder e struttura. Metti la versione riscritta in `revised_template`."
            }
        }
    }
}

/// Origine dei segnali: il loop guideline-driven o quello reflection-driven.
/// Determina come il modello pesa i criteri di valutazione.
#[derive(Debug, Clone, Copy)]
pub enum SignalKind {
    Guideline,
    Reflection,
}

impl SignalKind {
    /// Come il modello deve pesare i criteri, data l'origine dei segnali.
    const fn weighting(self) -> &'static str {
        match self {
            SignalKind::Guideline => {
                "I segnali vengono dalle linee guida di progetto: pesa la conformita' alle direttive."
            }
            SignalKind::Reflection => {
                "I segnali vengono dalla reflection sui run reali: pesa l'efficacia osservata sul campo."
            }
        }
    }
}

/// Esito strutturato della valutazione di un prompt.
///
/// I campi valutativi (`overall_score`, `dimensions`, `issues`,
/// `revised_template`, `rationale`) vengono dal modello; `status`, `model_used`
/// e `duration_ms` li valorizza [`call_prompt_revise`] coi fatti misurati.
#[derive(Debug, Deserialize)]
pub struct PromptReviseResult {
    /// Esito della chiamata, non un giudizio del modello: vedi
    /// [`REVISE_STATUS_COMPLETED`]. `default` perche' non arriva dal JSON.
    #[serde(default)]
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

/// Purpose da cui si risolve il modello, VIA TIER (mig 0346). Il nome del
/// modello non compare mai qui (regola G).
const PROMPT_REVISE_PURPOSE: &str = "prompt_conformance_check";

/// Rende leggibili al modello le debolezze osservate. Una lista vuota si
/// DICHIARA: un blocco vuoto lascerebbe il modello a indovinare se i segnali
/// mancano o se non sono stati raccolti.
fn format_weaknesses(weaknesses: &[String]) -> String {
    if weaknesses.is_empty() {
        return "(nessuna debolezza segnalata)".to_string();
    }
    weaknesses
        .iter()
        .map(|w| format!("- {w}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Costruisce il prompt della valutazione/revisione.
///
/// Questo blocco e' un call site FUORI CHAT (worker schedulato): nessuna
/// modalita' UI viene ereditata, quindi il contratto di output va dichiarato
/// per intero qui (regola D).
fn build_revise_prompt(
    prompt_key: &str,
    current_template: &str,
    mode: ReviseMode,
    kind: SignalKind,
    weaknesses: &[String],
    metrics: &serde_json::Value,
) -> String {
    let weaknesses_block = format_weaknesses(weaknesses);
    let revise_clause = mode.instruction();
    let kind_clause = kind.weighting();

    format!(
        "Sei un revisore di prompt di sistema. Valuta il prompt qui sotto e rispondi \
         ESCLUSIVAMENTE con un oggetto JSON, senza testo attorno.\n\n\
         CHIAVE PROMPT: {prompt_key}\n\
         {kind_clause}\n\
         {revise_clause}\n\n\
         SEGNALI / DEBOLEZZE OSSERVATE:\n{weaknesses_block}\n\n\
         METRICHE:\n{metrics}\n\n\
         TEMPLATE ATTUALE:\n---\n{current_template}\n---\n\n\
         FORMATO DELLA RISPOSTA:\n\
         {{\"overall_score\": <0-100>, \
         \"dimensions\": {{\"chiarezza\": <0-100>, \"completezza\": <0-100>, \
         \"specificita\": <0-100>, \"robustezza\": <0-100>}}, \
         \"issues\": [{{\"severity\": \"alta|media|bassa\", \"description\": \"<problema concreto>\"}}], \
         \"revised_template\": <stringa o null>, \"rationale\": \"<perche' questo punteggio>\"}}"
    )
}

/// Punto unico (regola L) della valutazione/revisione di un prompt.
///
/// `weaknesses`/`metrics` sono i segnali (vuoti per il loop guideline). Il
/// modello si risolve VIA TIER dal purpose `prompt_conformance_check` e la
/// chiamata passa dal gateway Rust: nessun modello, URL o porta hardcoded qui.
///
/// Storicamente questa era una POST al brain Python (`/agent/prompt-revise`),
/// che decideva il modello e produceva il JSON. Il brain e' stato eliminato: la
/// valutazione la fa ora direttamente il modello via gateway, e il contratto di
/// output e' imposto dal prompt e verificato al parse. Un modello che non
/// rispetta il formato produce `None` (variante scartata), mai un punteggio
/// inventato.
///
/// Ritorna `None` se il modello non e' risolvibile, la chiamata fallisce o la
/// risposta non e' il JSON atteso (errore loggato, mai panico).
pub async fn call_prompt_revise(
    pool: &PgPool,
    prompt_key: &str,
    current_template: &str,
    mode: ReviseMode,
    kind: SignalKind,
    weaknesses: &[String],
    metrics: serde_json::Value,
) -> Option<PromptReviseResult> {
    let started = std::time::Instant::now();

    let (provider, model) =
        match nexus_types::routing_client::resolve_purpose_via_http(pool, PROMPT_REVISE_PURPOSE)
            .await
        {
            Ok(pm) => pm,
            Err(e) => {
                error!(
                    "prompt_revise: modello non risolvibile per il purpose \
                     '{PROMPT_REVISE_PURPOSE}' (prompt '{prompt_key}'): {e}"
                );
                return None;
            }
        };

    let prompt = build_revise_prompt(prompt_key, current_template, mode, kind, weaknesses, &metrics);

    let content = match nexus_types::gateway_client::gateway_text_complete(
        pool,
        &provider,
        &model,
        &prompt,
        PROMPT_REVISE_PURPOSE,
        None,
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            error!("prompt_revise: chiamata al gateway fallita per '{prompt_key}': {e}");
            return None;
        }
    };

    parse_revise_response(&content, prompt_key, &provider, &model, started)
}

/// Estrae il giudizio dalla risposta e vi innesta i fatti tecnici misurati.
///
/// `status`, `model_used` e `duration_ms` NON vengono dal modello (regola M):
/// la chiamata e' arrivata fin qui e il JSON e' conforme, quindi l'esito e'
/// `completed` — il valore che i chiamanti verificano. Chiederlo al modello
/// significherebbe fargli decidere se se stesso e' riuscito.
fn parse_revise_response(
    content: &str,
    prompt_key: &str,
    provider: &str,
    model: &str,
    started: std::time::Instant,
) -> Option<PromptReviseResult> {
    let Some(parsed) = nexus_types::llm_json::extract_json_block(content) else {
        error!(
            "prompt_revise: il modello {provider}/{model} non ha risposto col JSON richiesto \
             per '{prompt_key}' — variante scartata"
        );
        return None;
    };

    match serde_json::from_value::<PromptReviseResult>(parsed) {
        Ok(mut result) => {
            result.status = REVISE_STATUS_COMPLETED.to_string();
            result.model_used = Some(format!("{provider}/{model}"));
            result.duration_ms = Some(started.elapsed().as_millis() as i64);
            Some(result)
        }
        Err(e) => {
            error!("prompt_revise: JSON non conforme al contratto per '{prompt_key}': {e}");
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
