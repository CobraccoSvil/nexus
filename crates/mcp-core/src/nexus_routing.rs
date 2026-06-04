//! Nexus Routing A/B — active co-routing feature flag.
//!
//! Blocco B della Fase 9: promuove il suggerimento del `NexusBridge` Q-Learning
//! da log osservazionale a override attivo di `self.provider` / `self.model`
//! in `agent_loop.rs`, controllato da una percentuale letta dal DB.
//!
//! Schema:
//! - `nexus_active_routing_pct` (chiave in tabella `settings`) — valore intero
//!   in [0, 100]. Percentuale di richieste per cui, se il bridge produce una
//!   decisione e il mapping `AgentType → (provider, model)` ha successo,
//!   l'override viene applicato. Default: 0 (feature off).
//!
//! - `agent_type_to_model(AgentType, &RoutingMatrix) -> Option<(provider, model)>`
//!   — assegna un tier (opus/sonnet/haiku) a ciascun AgentType e lo risolve
//!   tramite `purpose_model("agent_tier_*")` dalla matrice DB (mig 0104).
//!   Se un AgentType non e' mappato o il tier manca nel DB, l'override viene
//!   saltato e si incrementa `NEXUS_AB_FALLBACK_TOTAL`.
//!
//! - 4 contatori atomici esposti in Prometheus da `nexus_bridge::nexus_prometheus`:
//!     * `nexus_ab_decisions_total` — quante volte è stato valutato il coin-flip
//!     * `nexus_ab_overrides_total` — quante volte abbiamo sostituito provider/model
//!     * `nexus_ab_fallback_total`  — quante volte decisione presente ma non mappabile
//!     * `nexus_ab_forced_total`    — quante volte il routing è stato forzato da client
//!
//! Il design è intenzionalmente conservativo: il fallback silenzioso non rompe
//! mai il flusso principale. Se il DB fallisce, la percentuale è 0 (off); se
//! il mapping fallisce, usiamo il provider/model originale.

use crate::routing_matrix::RoutingMatrix;
use nexus_orchestrator::AgentType;
use sqlx::PgPool;
use std::sync::atomic::{AtomicU64, Ordering};

/// Chiave di settings che controlla la percentuale di routing attivo.
pub const SETTINGS_KEY_ACTIVE_ROUTING_PCT: &str = "nexus_active_routing_pct";

/// Contatore: quante volte abbiamo valutato il coin-flip del routing A/B.
/// Incrementato ogni volta che `agent_loop` raggiunge l'hook (indipendentemente
/// dall'esito del coin-flip).
pub static NEXUS_AB_DECISIONS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Contatore: quante volte abbiamo effettivamente sostituito provider/model
/// con la raccomandazione del bridge. Questo è il "sample size" dell'A/B.
pub static NEXUS_AB_OVERRIDES_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Contatore: quante volte il bridge ha suggerito un AgentType per cui non
/// abbiamo un mapping provider/model. In questi casi teniamo la config
/// originale. Un valore crescente qui è un segnale che dovremmo estendere
/// `agent_type_to_model`.
pub static NEXUS_AB_FALLBACK_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Contatore: quante volte il client ha specificato `agent_type_hint`
/// esplicitamente (SelectionStrategy::Forced), bypassando il Q-Learning.
pub static NEXUS_AB_FORCED_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Mapping `AgentType → tier → (provider, model)` letto dalla matrice DB.
///
/// Copre tutti i 60 agent types concreti registrati nel `NexusBridge`.
/// Il `Custom(_)` variant non e' mappato perche' e' string-based.
///
/// Filosofia di assegnazione tier:
/// - **Opus** per decisioni strutturali ad alto impatto (Architect,
///   SecurityArchitect, CloudArchitect, TechLead, APIDesigner, AgentEngineer,
///   SecurityAuditor, ComplianceOfficer) dove un errore costa molto.
/// - **Sonnet** per lavoro quotidiano di codifica/review ad alto volume
///   (Coder, Reviewer, GitHub agents, SRE, General engineers).
/// - **Haiku** per task brevi/ripetitivi dove latenza e costo contano
///   piu' della profondita' (Tester, TechWriter, monitoring, ETL, i18n, ecc.).
///
/// I nomi modello concreti vengono da `nexus_purpose_model` (mig 0104):
///   `agent_tier_opus`, `agent_tier_sonnet`, `agent_tier_haiku`.
///
/// Se un variant non e' in questa tabella o il tier non e' nel DB, il sito
/// di chiamata incrementa `NEXUS_AB_FALLBACK_TOTAL` e mantiene la config originale.
pub fn agent_type_to_model(
    agent_type: &AgentType,
    matrix: &RoutingMatrix,
) -> Option<(String, String)> {
    let tier_key = agent_type_to_tier(agent_type)?;
    matrix.purpose_model(tier_key)
}

const TIER_OPUS: &str = "agent_tier_opus";
const TIER_SONNET: &str = "agent_tier_sonnet";
const TIER_HAIKU: &str = "agent_tier_haiku";

fn agent_type_to_tier(agent_type: &AgentType) -> Option<&'static str> {
    match agent_type {
        // ── Core (4) ─────────────────────────────────────────────────────
        AgentType::Coder => Some(TIER_SONNET),
        AgentType::Tester => Some(TIER_HAIKU),
        AgentType::Reviewer => Some(TIER_SONNET),
        AgentType::Architect => Some(TIER_OPUS),

        // ── Specializations (12) ─────────────────────────────────────────
        AgentType::SecurityArchitect => Some(TIER_OPUS),
        AgentType::CloudArchitect => Some(TIER_OPUS),
        AgentType::DatabaseDesigner => Some(TIER_OPUS),
        AgentType::TechLead => Some(TIER_OPUS),
        AgentType::PerformanceEngineer => Some(TIER_SONNET),
        AgentType::FrontendSpecialist => Some(TIER_SONNET),
        AgentType::BackendSpecialist => Some(TIER_SONNET),
        AgentType::DevOpsEngineer => Some(TIER_SONNET),
        AgentType::MobileSpecialist => Some(TIER_SONNET),
        AgentType::DataScientist => Some(TIER_SONNET),
        AgentType::MLEngineer => Some(TIER_SONNET),
        AgentType::QASpecialist => Some(TIER_SONNET),

        // ── GitHub Integration (13) ───────────────────────────────────────
        AgentType::GitHubPRManager => Some(TIER_SONNET),
        AgentType::GitHubCodeReviewer => Some(TIER_SONNET),
        AgentType::GitHubIssueAnalyzer => Some(TIER_SONNET),
        AgentType::GitHubReleaseManager => Some(TIER_SONNET),
        AgentType::GitHubWorkflowManager => Some(TIER_SONNET),
        AgentType::GitHubSecurityAnalyzer => Some(TIER_OPUS),
        AgentType::GitHubDependencyManager => Some(TIER_SONNET),
        AgentType::GitHubProjectManager => Some(TIER_SONNET),
        AgentType::GitHubWikiManager => Some(TIER_HAIKU),
        AgentType::GitHubDiscussionModerator => Some(TIER_HAIKU),
        AgentType::GitHubActionsOptimizer => Some(TIER_SONNET),
        AgentType::GitHubStatusMonitor => Some(TIER_HAIKU),
        AgentType::GitHubIntegrationBot => Some(TIER_SONNET),

        // ── Other core specialized (4) ────────────────────────────────────
        AgentType::Researcher => Some(TIER_SONNET),
        AgentType::Analyst => Some(TIER_SONNET),
        AgentType::Optimizer => Some(TIER_SONNET),
        AgentType::Documenter => Some(TIER_HAIKU),

        // ── SpecializedAgent roles (4) ────────────────────────────────────
        AgentType::APIDesigner => Some(TIER_OPUS),
        AgentType::AgentEngineer => Some(TIER_OPUS),
        AgentType::SREEngineer => Some(TIER_SONNET),
        AgentType::PromptEngineer => Some(TIER_SONNET),

        // ── GeneralAgent roles (23) ───────────────────────────────────────
        AgentType::Debugger => Some(TIER_SONNET),
        AgentType::Refactorer => Some(TIER_SONNET),
        AgentType::InfraEngineer => Some(TIER_SONNET),
        AgentType::DatabaseAdmin => Some(TIER_SONNET),
        AgentType::UIDesigner => Some(TIER_SONNET),
        AgentType::DataEngineer => Some(TIER_SONNET),
        AgentType::AutomationEngineer => Some(TIER_SONNET),
        AgentType::IntegrationEngineer => Some(TIER_SONNET),
        AgentType::MigrationEngineer => Some(TIER_SONNET),
        AgentType::ChatbotEngineer => Some(TIER_SONNET),
        AgentType::EmbeddingEngineer => Some(TIER_SONNET),
        AgentType::ProductOwner => Some(TIER_SONNET),
        AgentType::SecurityAuditor => Some(TIER_OPUS),
        AgentType::ComplianceOfficer => Some(TIER_OPUS),
        AgentType::Profiler => Some(TIER_HAIKU),
        AgentType::AccessibilityEngineer => Some(TIER_HAIKU),
        AgentType::ETLEngineer => Some(TIER_HAIKU),
        AgentType::MonitoringEngineer => Some(TIER_HAIKU),
        AgentType::TechWriter => Some(TIER_HAIKU),
        AgentType::BenchmarkEngineer => Some(TIER_HAIKU),
        AgentType::TestAutomationEngineer => Some(TIER_HAIKU),
        AgentType::ReportingEngineer => Some(TIER_HAIKU),
        AgentType::I18nEngineer => Some(TIER_HAIKU),

        _ => None,
    }
}

/// Legge dalla tabella `settings` la percentuale corrente di routing attivo.
/// Range valido [0, 100]; valori fuori range o errori DB ritornano 0 (feature off).
///
/// Nota: questa funzione è chiamata al massimo una volta per agent run (hook
/// iniziale), quindi non serve caching aggressivo. Se in futuro serviranno
/// cadence più alte, valutare una cache atomica con TTL ~5s.
pub async fn read_nexus_active_routing_pct(db: &PgPool) -> u8 {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM settings WHERE key = $1 LIMIT 1")
            .bind(SETTINGS_KEY_ACTIVE_ROUTING_PCT)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();

    let Some((raw,)) = row else {
        return 0;
    };

    raw.trim().parse::<u8>().unwrap_or(0).min(100)
}

/// Decide se applicare l'override A/B per questa richiesta.
///
/// Ritorna `true` con probabilità `pct/100`. Usa `rand::thread_rng()` per il
/// coin flip (nessuno stato condiviso, non serve determinismo).
pub fn should_override_ab(pct: u8) -> bool {
    if pct == 0 {
        return false;
    }
    if pct >= 100 {
        return true;
    }
    use rand::Rng;
    let roll: u8 = rand::thread_rng().gen_range(0..100);
    roll < pct
}

/// Mappa ogni `AgentType` alla chiave DB (`nexus_prompt_templates.key`)
/// che contiene il system prompt appropriato.
///
/// Per i tipi che hanno varianti focus-dipendenti (Reviewer, Architect)
/// ritorna la chiave "general" — la variante specifica viene scelta
/// dall'agente stesso in base al task. Per Coder/Tester ritorna la chiave
/// base con placeholder (`{{lang_hint}}` / `{{type_hint}}`).
///
/// `Custom(_)` e varianti senza mapping ritornano `""`.
pub fn agent_type_to_prompt_key(agent_type: &AgentType) -> &'static str {
    match agent_type {
        // ── Core (4) ─────────────────────────────────────────────────────
        AgentType::Coder => "agent.coder.base",
        AgentType::Tester => "agent.tester.base",
        AgentType::Reviewer => "agent.reviewer.general",
        AgentType::Architect => "agent.architect.general",

        // ── Specializations ───────────────────────────────────────────────
        AgentType::SecurityArchitect => "agent.specialized.security_architect",
        AgentType::PerformanceEngineer => "agent.specialized.performance_engineer",
        AgentType::DatabaseDesigner => "agent.specialized.database_designer",
        AgentType::FrontendSpecialist => "agent.specialized.frontend_specialist",
        AgentType::BackendSpecialist => "agent.specialized.backend_specialist",
        AgentType::DevOpsEngineer => "agent.specialized.devops_engineer",
        AgentType::CloudArchitect => "agent.specialized.cloud_architect",
        AgentType::MobileSpecialist => "agent.specialized.mobile_specialist",
        AgentType::DataScientist => "agent.specialized.data_scientist",
        AgentType::MLEngineer => "agent.specialized.ml_engineer",
        AgentType::QASpecialist => "agent.specialized.qa_specialist",
        AgentType::TechLead => "agent.specialized.tech_lead",
        AgentType::SREEngineer => "agent.specialized.sre_engineer",
        AgentType::APIDesigner => "agent.specialized.api_designer",
        AgentType::PromptEngineer => "agent.specialized.prompt_engineer",
        AgentType::AgentEngineer => "agent.specialized.agent_engineer",
        AgentType::Researcher => "agent.specialized.researcher",
        AgentType::Analyst => "agent.specialized.analyst",
        AgentType::Optimizer => "agent.specialized.optimizer",
        AgentType::Documenter => "agent.specialized.documenter",

        // ── GitHub Integration (13) ───────────────────────────────────────
        AgentType::GitHubPRManager => "agent.github.pr_manager",
        AgentType::GitHubCodeReviewer => "agent.github.code_reviewer",
        AgentType::GitHubIssueAnalyzer => "agent.github.issue_analyzer",
        AgentType::GitHubReleaseManager => "agent.github.release_manager",
        AgentType::GitHubWorkflowManager => "agent.github.workflow_manager",
        AgentType::GitHubSecurityAnalyzer => "agent.github.security_analyzer",
        AgentType::GitHubDependencyManager => "agent.github.dependency_manager",
        AgentType::GitHubProjectManager => "agent.github.project_manager",
        AgentType::GitHubWikiManager => "agent.github.wiki_manager",
        AgentType::GitHubDiscussionModerator => "agent.github.discussion_moderator",
        AgentType::GitHubActionsOptimizer => "agent.github.actions_optimizer",
        AgentType::GitHubStatusMonitor => "agent.github.status_monitor",
        AgentType::GitHubIntegrationBot => "agent.github.integration_bot",

        // ── General roles (23) ────────────────────────────────────────────
        AgentType::Debugger => "agent.general.debugger",
        AgentType::Refactorer => "agent.general.refactorer",
        AgentType::Profiler => "agent.general.profiler",
        AgentType::InfraEngineer => "agent.general.infra_engineer",
        AgentType::DatabaseAdmin => "agent.general.database_admin",
        AgentType::SecurityAuditor => "agent.general.security_auditor",
        AgentType::ComplianceOfficer => "agent.general.compliance_officer",
        AgentType::UIDesigner => "agent.general.ui_designer",
        AgentType::AccessibilityEngineer => "agent.general.accessibility_engineer",
        AgentType::DataEngineer => "agent.general.data_engineer",
        AgentType::ETLEngineer => "agent.general.etl_engineer",
        AgentType::AutomationEngineer => "agent.general.automation_engineer",
        AgentType::IntegrationEngineer => "agent.general.integration_engineer",
        AgentType::MonitoringEngineer => "agent.general.monitoring_engineer",
        AgentType::MigrationEngineer => "agent.general.migration_engineer",
        AgentType::ChatbotEngineer => "agent.general.chatbot_engineer",
        AgentType::EmbeddingEngineer => "agent.general.embedding_engineer",
        AgentType::TechWriter => "agent.general.tech_writer",
        AgentType::ProductOwner => "agent.general.product_owner",
        AgentType::BenchmarkEngineer => "agent.general.benchmark_engineer",
        AgentType::TestAutomationEngineer => "agent.general.test_automation_engineer",
        AgentType::ReportingEngineer => "agent.general.reporting_engineer",
        AgentType::I18nEngineer => "agent.general.i18n_engineer",

        // Custom / unknown — nessun mapping
        _ => "",
    }
}

/// Recupera il system prompt per un `AgentType` dal registry globale.
///
/// Risolve i placeholder residui con valori neutri in modo da non esporre
/// token non sostituiti al LLM:
/// - `{{lang_hint}}` → `""` (linguaggio neutro)
/// - `{{type_hint}}` → `"test"` (tipo generico)
/// - `{{project}}`  → `""` (nessun progetto specifico)
///
/// Ritorna `String::new()` se il type non ha un mapping o se il registry
/// non è ancora stato inizializzato (es. durante startup).
pub fn get_agent_system_prompt(agent_type: &AgentType) -> String {
    let key = agent_type_to_prompt_key(agent_type);
    if key.is_empty() {
        return String::new();
    }
    let prompt = nexus_orchestrator::prompt_registry::get_prompt(key);
    // Rimpiazza placeholder che richiedono contesto runtime che non abbiamo qui.
    // L'agente specializzato (Coder, Tester, ecc.) farà la sostituzione precisa
    // nel suo proprio system_prompt(); qui usiamo default sensati.
    prompt
        .replace("{{lang_hint}}", "")
        .replace("{{type_hint}}", "task")
        .replace("{{project}}", "")
}

/// Incrementa un contatore atomico con ordering `Relaxed`.
/// I contatori sono monotoni, non servono garanzie di ordering tra di loro.
#[inline]
pub fn incr(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

/// Snapshot dei 4 contatori A/B per esposizione in Prometheus.
pub struct AbCounters {
    pub decisions: u64,
    pub overrides: u64,
    pub fallback: u64,
    /// Routing forzati dal client via `agentTypeHint` (SelectionStrategy::Forced)
    pub forced: u64,
}

pub fn snapshot_counters() -> AbCounters {
    AbCounters {
        decisions: NEXUS_AB_DECISIONS_TOTAL.load(Ordering::Relaxed),
        overrides: NEXUS_AB_OVERRIDES_TOTAL.load(Ordering::Relaxed),
        fallback: NEXUS_AB_FALLBACK_TOTAL.load(Ordering::Relaxed),
        forced: NEXUS_AB_FORCED_TOTAL.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Instant;

    fn test_matrix() -> RoutingMatrix {
        let mut purpose_models = HashMap::new();
        purpose_models.insert(
            TIER_OPUS.to_string(),
            (
                "test_provider_opus".to_string(),
                "test_model_opus".to_string(),
            ),
        );
        purpose_models.insert(
            TIER_SONNET.to_string(),
            (
                "test_provider_sonnet".to_string(),
                "test_model_sonnet".to_string(),
            ),
        );
        purpose_models.insert(
            TIER_HAIKU.to_string(),
            (
                "test_provider_haiku".to_string(),
                "test_model_haiku".to_string(),
            ),
        );
        RoutingMatrix {
            by_intent_mode: HashMap::new(),
            default_models: HashMap::new(),
            purpose_models,
            purpose_tiers: HashMap::new(),
            escalations: HashMap::new(),
            loaded_at: Instant::now(),
        }
    }

    #[test]
    fn test_agent_type_to_model_core_agents() {
        let m = test_matrix();
        let sonnet = Some((
            "test_provider_sonnet".to_string(),
            "test_model_sonnet".to_string(),
        ));
        let haiku = Some((
            "test_provider_haiku".to_string(),
            "test_model_haiku".to_string(),
        ));
        let opus = Some((
            "test_provider_opus".to_string(),
            "test_model_opus".to_string(),
        ));

        assert_eq!(agent_type_to_model(&AgentType::Coder, &m), sonnet);
        assert_eq!(agent_type_to_model(&AgentType::Tester, &m), haiku);
        assert_eq!(agent_type_to_model(&AgentType::Reviewer, &m), sonnet);
        assert_eq!(agent_type_to_model(&AgentType::Architect, &m), opus);
    }

    #[test]
    fn test_agent_type_to_model_all_60_variants_mapped() {
        let m = test_matrix();
        let registered: Vec<AgentType> = vec![
            AgentType::Coder,
            AgentType::Tester,
            AgentType::Reviewer,
            AgentType::Architect,
            AgentType::SecurityArchitect,
            AgentType::PerformanceEngineer,
            AgentType::DatabaseDesigner,
            AgentType::FrontendSpecialist,
            AgentType::BackendSpecialist,
            AgentType::DevOpsEngineer,
            AgentType::CloudArchitect,
            AgentType::MobileSpecialist,
            AgentType::DataScientist,
            AgentType::MLEngineer,
            AgentType::QASpecialist,
            AgentType::TechLead,
            AgentType::GitHubPRManager,
            AgentType::GitHubCodeReviewer,
            AgentType::GitHubIssueAnalyzer,
            AgentType::GitHubReleaseManager,
            AgentType::GitHubWorkflowManager,
            AgentType::GitHubSecurityAnalyzer,
            AgentType::GitHubDependencyManager,
            AgentType::GitHubProjectManager,
            AgentType::GitHubWikiManager,
            AgentType::GitHubDiscussionModerator,
            AgentType::GitHubActionsOptimizer,
            AgentType::GitHubStatusMonitor,
            AgentType::GitHubIntegrationBot,
            AgentType::Researcher,
            AgentType::Analyst,
            AgentType::Optimizer,
            AgentType::Documenter,
            AgentType::SREEngineer,
            AgentType::APIDesigner,
            AgentType::PromptEngineer,
            AgentType::AgentEngineer,
            AgentType::Debugger,
            AgentType::Refactorer,
            AgentType::Profiler,
            AgentType::InfraEngineer,
            AgentType::DatabaseAdmin,
            AgentType::SecurityAuditor,
            AgentType::ComplianceOfficer,
            AgentType::UIDesigner,
            AgentType::AccessibilityEngineer,
            AgentType::DataEngineer,
            AgentType::ETLEngineer,
            AgentType::AutomationEngineer,
            AgentType::IntegrationEngineer,
            AgentType::MonitoringEngineer,
            AgentType::MigrationEngineer,
            AgentType::ChatbotEngineer,
            AgentType::EmbeddingEngineer,
            AgentType::TechWriter,
            AgentType::ProductOwner,
            AgentType::BenchmarkEngineer,
            AgentType::TestAutomationEngineer,
            AgentType::ReportingEngineer,
            AgentType::I18nEngineer,
        ];
        assert_eq!(registered.len(), 60, "expected 60 registered variants");
        for variant in &registered {
            assert!(
                agent_type_to_model(variant, &m).is_some(),
                "missing model mapping for {:?}",
                variant
            );
        }
    }

    #[test]
    fn test_agent_type_to_tier_assignments() {
        assert_eq!(agent_type_to_tier(&AgentType::Architect), Some(TIER_OPUS));
        assert_eq!(
            agent_type_to_tier(&AgentType::SecurityArchitect),
            Some(TIER_OPUS)
        );
        assert_eq!(agent_type_to_tier(&AgentType::APIDesigner), Some(TIER_OPUS));
        assert_eq!(
            agent_type_to_tier(&AgentType::AgentEngineer),
            Some(TIER_OPUS)
        );
        assert_eq!(
            agent_type_to_tier(&AgentType::SecurityAuditor),
            Some(TIER_OPUS)
        );
        assert_eq!(
            agent_type_to_tier(&AgentType::ComplianceOfficer),
            Some(TIER_OPUS)
        );

        assert_eq!(agent_type_to_tier(&AgentType::Coder), Some(TIER_SONNET));
        assert_eq!(
            agent_type_to_tier(&AgentType::SREEngineer),
            Some(TIER_SONNET)
        );
        assert_eq!(agent_type_to_tier(&AgentType::Debugger), Some(TIER_SONNET));

        assert_eq!(agent_type_to_tier(&AgentType::Tester), Some(TIER_HAIKU));
        assert_eq!(agent_type_to_tier(&AgentType::TechWriter), Some(TIER_HAIKU));
        assert_eq!(
            agent_type_to_tier(&AgentType::I18nEngineer),
            Some(TIER_HAIKU)
        );
        assert_eq!(
            agent_type_to_tier(&AgentType::MonitoringEngineer),
            Some(TIER_HAIKU)
        );
    }

    #[test]
    fn test_agent_type_to_model_custom_is_none() {
        let m = test_matrix();
        assert!(agent_type_to_model(&AgentType::Custom("anything".to_string()), &m).is_none());
    }

    #[test]
    fn test_agent_type_to_model_returns_none_when_tier_missing_from_db() {
        let empty_matrix = RoutingMatrix {
            by_intent_mode: HashMap::new(),
            default_models: HashMap::new(),
            purpose_models: HashMap::new(),
            purpose_tiers: HashMap::new(),
            escalations: HashMap::new(),
            loaded_at: Instant::now(),
        };
        assert!(agent_type_to_model(&AgentType::Coder, &empty_matrix).is_none());
    }

    #[test]
    fn test_should_override_boundary_cases() {
        // 0% → mai
        for _ in 0..100 {
            assert!(!should_override_ab(0));
        }
        // 100% → sempre
        for _ in 0..100 {
            assert!(should_override_ab(100));
        }
        // 255% (clamp implicito via u8) → sempre
        assert!(should_override_ab(200));
    }

    #[test]
    fn test_should_override_distribution_roughly_matches_pct() {
        // Con 50% su 2000 trial, l'intervallo atteso è [900, 1100] con
        // ampio margine di sicurezza (3-sigma sarebbe ~[933, 1067]).
        let mut hits = 0;
        for _ in 0..2000 {
            if should_override_ab(50) {
                hits += 1;
            }
        }
        assert!(
            (900..=1100).contains(&hits),
            "50% distribution out of band: got {} hits",
            hits
        );
    }

    #[test]
    fn test_counters_start_at_zero_and_increment() {
        // Non possiamo assumere zero assoluto (altri test possono averli
        // incrementati), quindi testiamo in delta.
        let before = snapshot_counters();
        incr(&NEXUS_AB_DECISIONS_TOTAL);
        incr(&NEXUS_AB_OVERRIDES_TOTAL);
        incr(&NEXUS_AB_FALLBACK_TOTAL);
        let after = snapshot_counters();
        assert_eq!(after.decisions, before.decisions + 1);
        assert_eq!(after.overrides, before.overrides + 1);
        assert_eq!(after.fallback, before.fallback + 1);
    }

    #[test]
    fn test_agent_type_to_prompt_key_core_agents() {
        assert_eq!(
            agent_type_to_prompt_key(&AgentType::Coder),
            "agent.coder.base"
        );
        assert_eq!(
            agent_type_to_prompt_key(&AgentType::Tester),
            "agent.tester.base"
        );
        assert_eq!(
            agent_type_to_prompt_key(&AgentType::Reviewer),
            "agent.reviewer.general"
        );
        assert_eq!(
            agent_type_to_prompt_key(&AgentType::Architect),
            "agent.architect.general"
        );
    }

    #[test]
    fn test_agent_type_to_prompt_key_all_60_non_empty() {
        // Tutti i 60 variant concreti devono avere una prompt key non vuota.
        let registered: Vec<AgentType> = vec![
            AgentType::Coder,
            AgentType::Tester,
            AgentType::Reviewer,
            AgentType::Architect,
            AgentType::SecurityArchitect,
            AgentType::PerformanceEngineer,
            AgentType::DatabaseDesigner,
            AgentType::FrontendSpecialist,
            AgentType::BackendSpecialist,
            AgentType::DevOpsEngineer,
            AgentType::CloudArchitect,
            AgentType::MobileSpecialist,
            AgentType::DataScientist,
            AgentType::MLEngineer,
            AgentType::QASpecialist,
            AgentType::TechLead,
            AgentType::GitHubPRManager,
            AgentType::GitHubCodeReviewer,
            AgentType::GitHubIssueAnalyzer,
            AgentType::GitHubReleaseManager,
            AgentType::GitHubWorkflowManager,
            AgentType::GitHubSecurityAnalyzer,
            AgentType::GitHubDependencyManager,
            AgentType::GitHubProjectManager,
            AgentType::GitHubWikiManager,
            AgentType::GitHubDiscussionModerator,
            AgentType::GitHubActionsOptimizer,
            AgentType::GitHubStatusMonitor,
            AgentType::GitHubIntegrationBot,
            AgentType::Researcher,
            AgentType::Analyst,
            AgentType::Optimizer,
            AgentType::Documenter,
            AgentType::SREEngineer,
            AgentType::APIDesigner,
            AgentType::PromptEngineer,
            AgentType::AgentEngineer,
            AgentType::Debugger,
            AgentType::Refactorer,
            AgentType::Profiler,
            AgentType::InfraEngineer,
            AgentType::DatabaseAdmin,
            AgentType::SecurityAuditor,
            AgentType::ComplianceOfficer,
            AgentType::UIDesigner,
            AgentType::AccessibilityEngineer,
            AgentType::DataEngineer,
            AgentType::ETLEngineer,
            AgentType::AutomationEngineer,
            AgentType::IntegrationEngineer,
            AgentType::MonitoringEngineer,
            AgentType::MigrationEngineer,
            AgentType::ChatbotEngineer,
            AgentType::EmbeddingEngineer,
            AgentType::TechWriter,
            AgentType::ProductOwner,
            AgentType::BenchmarkEngineer,
            AgentType::TestAutomationEngineer,
            AgentType::ReportingEngineer,
            AgentType::I18nEngineer,
        ];
        assert_eq!(registered.len(), 60, "expected 60 registered variants");
        for variant in &registered {
            let key = agent_type_to_prompt_key(variant);
            assert!(!key.is_empty(), "missing prompt key for {:?}", variant);
            assert!(
                key.starts_with("agent."),
                "prompt key should start with 'agent.' for {:?}, got '{}'",
                variant,
                key
            );
        }
    }

    #[test]
    fn test_agent_type_to_prompt_key_custom_is_empty() {
        // Custom(_) non deve avere un mapping
        assert_eq!(
            agent_type_to_prompt_key(&AgentType::Custom("foo".to_string())),
            ""
        );
    }

    #[test]
    fn test_get_agent_system_prompt_custom_returns_empty() {
        // Custom(_) → prompt key "" → deve ritornare stringa vuota senza panic
        let result = get_agent_system_prompt(&AgentType::Custom("foo".to_string()));
        assert!(
            result.is_empty(),
            "Custom(_) should return empty prompt, got: {}",
            result
        );
    }

    #[test]
    fn test_get_agent_system_prompt_strips_placeholders_if_registry_has_them() {
        // Con registry non ancora inizializzato, get_prompt ritorna "".
        // In quel caso get_agent_system_prompt deve restituire "" senza panic.
        // La sostituzione placeholder è coperta dai test in nexus-agents stessi.
        let result = get_agent_system_prompt(&AgentType::Coder);
        // Non deve contenere placeholder non sostituiti, qualunque cosa ritorni
        assert!(
            !result.contains("{{lang_hint}}"),
            "lang_hint placeholder not stripped"
        );
        assert!(
            !result.contains("{{type_hint}}"),
            "type_hint placeholder not stripped"
        );
        assert!(
            !result.contains("{{project}}"),
            "project placeholder not stripped"
        );
    }
}
