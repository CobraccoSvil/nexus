use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

/// Flag globale per il classificatore LLM degli intent.
/// Inizializzato da `main.rs` dopo la lettura del DB (settings.llm_classifier_enabled).
/// L'env var `NEXUS_LLM_CLASSIFIER_ENABLED` resta come override di emergenza
/// (applicata a ogni chiamata, priorita' piu' alta del valore atomico).
pub(crate) static LLM_CLASSIFIER_ENABLED: AtomicBool = AtomicBool::new(true);

/// Imposta il valore del flag dal DB all'avvio. Chiamato da `main.rs`.
pub fn set_llm_classifier_enabled(val: bool) {
    LLM_CLASSIFIER_ENABLED.store(val, Ordering::Relaxed);
}
use serde_json::Value;
use uuid::Uuid;

// `NeuralCoreClient` non incapsula piu' un canale gRPC verso il brain: tutti i
// suoi metodi delegano all'embedder ONNX in-process o al Nexus LLM Gateway. Il
// proto neural_core.proto e il tipo generato `NeuralCoreServiceClient` sono stati
// rimossi col brain: l'ultimo uso (`generate_document`) e' migrato in-process in
// `crate::docx_render`. `GenerateCompletion`/`GenerateAgentTurn` erano gia'
// cablati al gateway in `neural_client.rs`.

use crate::nexus_gateway::NexusGatewayClient;

pub(crate) const KNOWN_PROVIDERS: [&str; 5] =
    ["anthropic", "openai", "google", "deepseek", "mistral"];
pub(crate) const KNOWN_INTENTS: [&str; 6] =
    ["fix", "refactor", "test", "docs", "architecture", "chat"];

// ---------------------------------------------------------------------------
// Routing semantico inline — nessuna chiamata gRPC, zero latenza aggiuntiva
// ---------------------------------------------------------------------------

mod core;
mod intent;
pub(crate) mod model_routing;
mod model_selection;
pub(crate) mod model_service;
pub(crate) mod neural_client;
pub(crate) mod provider_choice;
#[cfg(test)]
mod tests;

// Re-export interni: rende visibili a core.rs/test (via super::*) e al
// resto del crate i simboli pub(crate)/pub dei sottomoduli, mantenendo
// invariati i call site esistenti.
pub(crate) use intent::*;
pub(crate) use model_routing::*;
pub(crate) use model_selection::*;
pub(crate) use neural_client::*;
pub(crate) use provider_choice::{
    ProviderChoice, ProviderOverrideMode, ProviderPin, ProviderVeto,
};

#[derive(Debug, Clone)]
pub struct ChatAttachment {
    /// UUID dell'allegato in `chat_message_attachments`. Popolato dopo
    /// `persist_message_attachments` per consentire al prompt iniziale di
    /// stampare un suggerimento `nexus_inspect_attachment(attachment_id=...)`.
    /// `None` quando l'allegato non e' ancora stato persistito (caso non
    /// raggiunto in produzione, mantenuto Option per backward compat).
    pub id: Option<Uuid>,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub text_content: String,
    pub base64_content: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutomationMode {
    Study,
    Confirm,
    Automatic,
}

/// Identificatore automation_mode non canonico (solo `study|confirm|automatic`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidAutomationMode;

impl AutomationMode {
    pub const CANONICAL: [&'static str; 3] = ["study", "confirm", "automatic"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Study => "study",
            Self::Confirm => "confirm",
            Self::Automatic => "automatic",
        }
    }

    /// Parsa l'identificatore canonico (ASCII lowercase esatto). Punto unico
    /// (regola L): `parse_automation_mode` e i lettori di
    /// `chat_sessions.automation_mode` delegano qui. Valore mancante/vuoto ->
    /// `Confirm` (default conservativo della colonna DB).
    pub fn parse(value: Option<&str>) -> Self {
        Self::try_parse(value).unwrap_or(Self::Confirm)
    }

    /// Parsa l'identificatore canonico. Rifiuta sinonimi e varianti non inglesi.
    pub fn try_parse(value: Option<&str>) -> Result<Self, InvalidAutomationMode> {
        let s = value
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or(InvalidAutomationMode)?;
        match s {
            "study" => Ok(Self::Study),
            "confirm" => Ok(Self::Confirm),
            "automatic" => Ok(Self::Automatic),
            _ => Err(InvalidAutomationMode),
        }
    }

    /// Mappa nel enum del grafo agente (None = study, niente automazione mutativa).
    pub fn to_graph_mode(self) -> nexus_agent_graph::state::AutomationMode {
        use nexus_agent_graph::state::AutomationMode as GraphMode;
        match self {
            Self::Study => GraphMode::None,
            Self::Confirm => GraphMode::Confirm,
            Self::Automatic => GraphMode::Automatic,
        }
    }

    /// Restituisce la chiave DB del template per le istruzioni di modalità.
    /// pub(crate): usata sia da orchestrator::compose_prompt sia dal path chat
    /// (agent_run::spawn_agent_run) — punto unico delle istruzioni di modalità.
    pub(crate) fn prompt_instruction_template_key(self) -> &'static str {
        match self {
            Self::Study => "automation.mode_study_instruction",
            Self::Confirm => "automation.mode_confirm_instruction",
            Self::Automatic => "automation.mode_automatic_instruction",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OrchestratorRequest {
    pub user_id: String,
    pub project_id: String,
    pub profile_id: String,
    pub message: String,
    pub active_files: Vec<String>,
    pub session_id: Option<String>,
    pub request_message_id: Option<String>,
    /// La scelta di provider dell'utente CON la sua forza (preferenza o pin).
    /// Era un `Option<String>` — il solo nome del provider — e chi lo leggeva
    /// doveva dedurre quanto vincolasse: e' la deduzione che
    /// [`ProviderChoice`] elimina.
    pub provider_choice: ProviderChoice,
    pub model_override: Option<String>,
    pub automation_mode: AutomationMode,
    pub attachments: Vec<ChatAttachment>,
}

#[derive(Debug, Clone)]
pub struct OrchestratorResult {
    pub payload: Value,
}

#[derive(Clone)]
pub struct Orchestrator {
    pub(crate) neural: NeuralCoreClient,
    pub(crate) template_cache: crate::prompt_templates::TemplateCache,
    /// Client del Nexus Gateway. NON e' `Option`: senza gateway non esiste alcun
    /// modo di chiamare un LLM (il brain Python e' stato rimosso), quindi un
    /// orchestrator senza gateway non e' uno stato valido — e il tipo lo rende
    /// impossibile.
    ///
    /// Prima era `Option`, valorizzata all'avvio solo se una probe `is_healthy()`
    /// rispondeva in quell'istante: il 2026-07-16 il gateway ha finito di nascere
    /// 1,4s DOPO la probe e mcp-core e' rimasto senza gateway per tutta la vita
    /// del processo, con il classificatore fermo in fallback. La disponibilita'
    /// di un servizio e' uno stato che cambia: si scopre quando lo si chiama, non
    /// la si congela all'avvio (regola M).
    pub(crate) nexus_gateway: NexusGatewayClient,
    /// Cache della matrice di routing letta da DB (nexus_routing_matrix).
    /// Refresh background ogni 60s. Sostituisce i model name hardcoded
    /// che erano sparsi in `route_model_with_mode` e `default_model_for_provider`.
    /// Inizializzata in main.rs e clonata qui (la cache interna e' Arc<RwLock<...>>).
    pub(crate) routing_matrix: crate::routing_matrix::RoutingMatrixCache,
    /// Cache parametri routing (settings.routing.*) — mig 0111. Refresh 60s.
    pub(crate) routing_thresholds: crate::routing_config::RoutingThresholdsCache,
    /// Cache mapping intent -> tier/capability/preferred_provider — mig 0110.
    pub(crate) intent_capability: crate::routing_config::IntentCapabilityCache,
    /// Cache della matrice slot-based (mig 0133, Livello 4 NLU).
    /// Lookup gerarchico (action_verb, target_type, framework, scope) →
    /// (provider, model). Piu' precisa di (intent, behavior_mode); il
    /// router la prova per prima e cade su routing classico se no-match.
    pub(crate) slots_matrix: crate::routing_slots::SlotsRoutingMatrixCache,
}

#[cfg(test)]
mod automation_mode_tests {
    use super::{AutomationMode, InvalidAutomationMode};

    #[test]
    fn try_parse_accepts_canonical_only() {
        assert_eq!(
            AutomationMode::try_parse(Some("automatic")).unwrap(),
            AutomationMode::Automatic
        );
        assert_eq!(
            AutomationMode::try_parse(Some("confirm")).unwrap(),
            AutomationMode::Confirm
        );
        assert_eq!(
            AutomationMode::try_parse(Some("study")).unwrap(),
            AutomationMode::Study
        );
    }

    #[test]
    fn try_parse_rejects_synonyms() {
        for bad in ["automatico", "auto", "continuo", "conferma", "studio", "Automatic"] {
            assert_eq!(
                AutomationMode::try_parse(Some(bad)),
                Err(InvalidAutomationMode),
                "unexpected accept: {bad}"
            );
        }
    }

    #[test]
    fn parse_defaults_missing_to_confirm() {
        assert_eq!(AutomationMode::parse(None), AutomationMode::Confirm);
        assert_eq!(AutomationMode::parse(Some("")), AutomationMode::Confirm);
    }
}
