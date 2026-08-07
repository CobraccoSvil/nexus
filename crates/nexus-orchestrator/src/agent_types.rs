//! Enum `AgentType` — 60+ varianti di ruoli agente.
//!
//! Copiato verbatim da `nexus-agents/src/base.rs` durante la fase 5e
//! del refactor (opzione B): il trait `Agent` e l'infrastruttura di
//! esecuzione lato Rust sono eliminati. Lo orchestrator continua a
//! conoscere i tipi astratti di agente perche' il router Q-Learning
//! e il brain LangGraph ragionano su di essi.

use serde::{Deserialize, Serialize};

/// Tipo di agente
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum AgentType {
    // Core roles (4)
    Coder,
    Tester,
    Reviewer,
    Architect,

    // Specializations (12)
    SecurityArchitect,
    PerformanceEngineer,
    DatabaseDesigner,
    FrontendSpecialist,
    BackendSpecialist,
    DevOpsEngineer,
    CloudArchitect,
    MobileSpecialist,
    DataScientist,
    MLEngineer,
    QASpecialist,
    TechLead,

    // GitHub integration (13)
    GitHubPRManager,
    GitHubCodeReviewer,
    GitHubIssueAnalyzer,
    GitHubReleaseManager,
    GitHubWorkflowManager,
    GitHubSecurityAnalyzer,
    GitHubDependencyManager,
    GitHubProjectManager,
    GitHubWikiManager,
    GitHubDiscussionModerator,
    GitHubActionsOptimizer,
    GitHubStatusMonitor,
    GitHubIntegrationBot,

    // Other specialized — originally 4, now extended
    Researcher,
    Analyst,
    Optimizer,
    Documenter,

    // Specialized roles (new — SpecializedAgent)
    SREEngineer,
    APIDesigner,
    PromptEngineer,
    AgentEngineer,

    // General roles (new — GeneralAgent)
    Debugger,
    Refactorer,
    Profiler,
    InfraEngineer,
    DatabaseAdmin,
    SecurityAuditor,
    ComplianceOfficer,
    UIDesigner,
    AccessibilityEngineer,
    DataEngineer,
    ETLEngineer,
    AutomationEngineer,
    IntegrationEngineer,
    MonitoringEngineer,
    MigrationEngineer,
    ChatbotEngineer,
    EmbeddingEngineer,
    TechWriter,
    ProductOwner,
    BenchmarkEngineer,
    TestAutomationEngineer,
    ReportingEngineer,
    I18nEngineer,

    Custom(String),
}

/// Converte un nome snake_case (`coder`, `github_pr_manager`, `sre_engineer`)
/// nel PascalCase che [`AgentType::from_name`] si aspetta. Idempotente: un nome
/// gia' PascalCase torna invariato.
///
/// PUNTO UNICO (regola L), e sta QUI perche' le sigle che riconosce — `SRE`,
/// `API`, `ML`, `QA`, `UI`, `ETL`, `PRManager`, `GitHub` — non sono una lista
/// arbitraria: sono la grafia esatta delle varianti dell'enum qui sopra.
/// Separarle dall'enum e' cio' che ha permesso la divergenza.
///
/// COSA E' ACCADUTO (misurato il 07/08/2026). La funzione esisteva in DUE copie:
/// una privata in `mcp-core::agent_router_server` con tutte le sigle, una
/// `pub(crate)` in `mcp-core::internal_learning` che conosceva solo `GitHub`.
/// Il commento sopra la seconda dichiarava: «Manteniamo la logica identica per
/// coerenza tra i due path» — e non lo erano piu'. Chi usava la copia povera
/// (la chat e `force_agent_type_from_hint`) otteneva da `sre_engineer` il nome
/// `SreEngineer`, che `from_name` non riconosce e che ricade su
/// `Custom("SreEngineer")`: un tipo agente DIVERSO da `SREEngineer`, senza che
/// niente fallisca — il prompt di sistema risulta vuoto e il premio del bandit
/// si accumula su un tipo fantasma.
///
/// Verificato prima di dichiararlo attivo: in `nexus_q_values` non esiste una
/// sola chiave malformata e gli unici hint interni sono `"debugger"`. Era una
/// trappola armata, che sarebbe scattata al primo `sre_engineer` passato
/// dall'API.
pub fn snake_to_pascal(name: &str) -> String {
    if name.chars().next().map(char::is_uppercase).unwrap_or(false) && !name.contains('_') {
        return name.to_string();
    }
    let mut out = String::with_capacity(name.len());
    let mut capitalize_next = true;
    for ch in name.chars() {
        if ch == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            out.extend(ch.to_uppercase());
            capitalize_next = false;
        } else {
            out.push(ch);
        }
    }
    // La capitalizzazione base rende "SreEngineer": qui si riallineano le sigle
    // alla grafia delle varianti.
    out.replace("Github", "GitHub")
        .replace("Sre", "SRE")
        .replace("Api", "API")
        .replace("Ml", "ML")
        .replace("Qa", "QA")
        .replace("Ui", "UI")
        .replace("Etl", "ETL")
        .replace("PrManager", "PRManager")
}

impl AgentType {
    /// Converte un nome stringa (es. "Coder", "GitHubPRManager") in `AgentType`.
    /// Per nomi non riconosciuti ritorna `Custom(name)`.
    pub fn from_name(name: &str) -> Self {
        match name {
            "Coder"                    => AgentType::Coder,
            "Tester"                   => AgentType::Tester,
            "Reviewer"                 => AgentType::Reviewer,
            "Architect"                => AgentType::Architect,
            "SecurityArchitect"        => AgentType::SecurityArchitect,
            "PerformanceEngineer"      => AgentType::PerformanceEngineer,
            "DatabaseDesigner"         => AgentType::DatabaseDesigner,
            "FrontendSpecialist"       => AgentType::FrontendSpecialist,
            "BackendSpecialist"        => AgentType::BackendSpecialist,
            "DevOpsEngineer"           => AgentType::DevOpsEngineer,
            "CloudArchitect"           => AgentType::CloudArchitect,
            "MobileSpecialist"         => AgentType::MobileSpecialist,
            "DataScientist"            => AgentType::DataScientist,
            "MLEngineer"               => AgentType::MLEngineer,
            "QASpecialist"             => AgentType::QASpecialist,
            "TechLead"                 => AgentType::TechLead,
            "GitHubPRManager"          => AgentType::GitHubPRManager,
            "GitHubCodeReviewer"       => AgentType::GitHubCodeReviewer,
            "GitHubIssueAnalyzer"      => AgentType::GitHubIssueAnalyzer,
            "GitHubReleaseManager"     => AgentType::GitHubReleaseManager,
            "GitHubWorkflowManager"    => AgentType::GitHubWorkflowManager,
            "GitHubSecurityAnalyzer"   => AgentType::GitHubSecurityAnalyzer,
            "GitHubDependencyManager"  => AgentType::GitHubDependencyManager,
            "GitHubProjectManager"     => AgentType::GitHubProjectManager,
            "GitHubWikiManager"        => AgentType::GitHubWikiManager,
            "GitHubDiscussionModerator"=> AgentType::GitHubDiscussionModerator,
            "GitHubActionsOptimizer"   => AgentType::GitHubActionsOptimizer,
            "GitHubStatusMonitor"      => AgentType::GitHubStatusMonitor,
            "GitHubIntegrationBot"     => AgentType::GitHubIntegrationBot,
            "Researcher"               => AgentType::Researcher,
            "Analyst"                  => AgentType::Analyst,
            "Optimizer"                => AgentType::Optimizer,
            "Documenter"               => AgentType::Documenter,
            "SREEngineer"              => AgentType::SREEngineer,
            "APIDesigner"              => AgentType::APIDesigner,
            "PromptEngineer"           => AgentType::PromptEngineer,
            "AgentEngineer"            => AgentType::AgentEngineer,
            "Debugger"                 => AgentType::Debugger,
            "Refactorer"               => AgentType::Refactorer,
            "Profiler"                 => AgentType::Profiler,
            "InfraEngineer"            => AgentType::InfraEngineer,
            "DatabaseAdmin"            => AgentType::DatabaseAdmin,
            "SecurityAuditor"          => AgentType::SecurityAuditor,
            "ComplianceOfficer"        => AgentType::ComplianceOfficer,
            "UIDesigner"               => AgentType::UIDesigner,
            "AccessibilityEngineer"    => AgentType::AccessibilityEngineer,
            "DataEngineer"             => AgentType::DataEngineer,
            "ETLEngineer"              => AgentType::ETLEngineer,
            "AutomationEngineer"       => AgentType::AutomationEngineer,
            "IntegrationEngineer"      => AgentType::IntegrationEngineer,
            "MonitoringEngineer"       => AgentType::MonitoringEngineer,
            "MigrationEngineer"        => AgentType::MigrationEngineer,
            "ChatbotEngineer"          => AgentType::ChatbotEngineer,
            "EmbeddingEngineer"        => AgentType::EmbeddingEngineer,
            "TechWriter"               => AgentType::TechWriter,
            "ProductOwner"             => AgentType::ProductOwner,
            "BenchmarkEngineer"        => AgentType::BenchmarkEngineer,
            "TestAutomationEngineer"   => AgentType::TestAutomationEngineer,
            "ReportingEngineer"        => AgentType::ReportingEngineer,
            "I18nEngineer"             => AgentType::I18nEngineer,
            other                      => AgentType::Custom(other.to_string()),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            AgentType::Coder => "Coder",
            AgentType::Tester => "Tester",
            AgentType::Reviewer => "Reviewer",
            AgentType::Architect => "Architect",
            AgentType::SecurityArchitect => "SecurityArchitect",
            AgentType::PerformanceEngineer => "PerformanceEngineer",
            AgentType::DatabaseDesigner => "DatabaseDesigner",
            AgentType::FrontendSpecialist => "FrontendSpecialist",
            AgentType::BackendSpecialist => "BackendSpecialist",
            AgentType::DevOpsEngineer => "DevOpsEngineer",
            AgentType::CloudArchitect => "CloudArchitect",
            AgentType::MobileSpecialist => "MobileSpecialist",
            AgentType::DataScientist => "DataScientist",
            AgentType::MLEngineer => "MLEngineer",
            AgentType::QASpecialist => "QASpecialist",
            AgentType::TechLead => "TechLead",
            AgentType::GitHubPRManager => "GitHubPRManager",
            AgentType::GitHubCodeReviewer => "GitHubCodeReviewer",
            AgentType::GitHubIssueAnalyzer => "GitHubIssueAnalyzer",
            AgentType::GitHubReleaseManager => "GitHubReleaseManager",
            AgentType::GitHubWorkflowManager => "GitHubWorkflowManager",
            AgentType::GitHubSecurityAnalyzer => "GitHubSecurityAnalyzer",
            AgentType::GitHubDependencyManager => "GitHubDependencyManager",
            AgentType::GitHubProjectManager => "GitHubProjectManager",
            AgentType::GitHubWikiManager => "GitHubWikiManager",
            AgentType::GitHubDiscussionModerator => "GitHubDiscussionModerator",
            AgentType::GitHubActionsOptimizer => "GitHubActionsOptimizer",
            AgentType::GitHubStatusMonitor => "GitHubStatusMonitor",
            AgentType::GitHubIntegrationBot => "GitHubIntegrationBot",
            AgentType::Researcher => "Researcher",
            AgentType::Analyst => "Analyst",
            AgentType::Optimizer => "Optimizer",
            AgentType::Documenter => "Documenter",
            AgentType::SREEngineer => "SREEngineer",
            AgentType::APIDesigner => "APIDesigner",
            AgentType::PromptEngineer => "PromptEngineer",
            AgentType::AgentEngineer => "AgentEngineer",
            AgentType::Debugger => "Debugger",
            AgentType::Refactorer => "Refactorer",
            AgentType::Profiler => "Profiler",
            AgentType::InfraEngineer => "InfraEngineer",
            AgentType::DatabaseAdmin => "DatabaseAdmin",
            AgentType::SecurityAuditor => "SecurityAuditor",
            AgentType::ComplianceOfficer => "ComplianceOfficer",
            AgentType::UIDesigner => "UIDesigner",
            AgentType::AccessibilityEngineer => "AccessibilityEngineer",
            AgentType::DataEngineer => "DataEngineer",
            AgentType::ETLEngineer => "ETLEngineer",
            AgentType::AutomationEngineer => "AutomationEngineer",
            AgentType::IntegrationEngineer => "IntegrationEngineer",
            AgentType::MonitoringEngineer => "MonitoringEngineer",
            AgentType::MigrationEngineer => "MigrationEngineer",
            AgentType::ChatbotEngineer => "ChatbotEngineer",
            AgentType::EmbeddingEngineer => "EmbeddingEngineer",
            AgentType::TechWriter => "TechWriter",
            AgentType::ProductOwner => "ProductOwner",
            AgentType::BenchmarkEngineer => "BenchmarkEngineer",
            AgentType::TestAutomationEngineer => "TestAutomationEngineer",
            AgentType::ReportingEngineer => "ReportingEngineer",
            AgentType::I18nEngineer => "I18nEngineer",
            AgentType::Custom(name) => name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_type_names() {
        assert_eq!(AgentType::Coder.name(), "Coder");
        assert_eq!(AgentType::GitHubPRManager.name(), "GitHubPRManager");
    }

    #[test]
    fn test_agent_type_from_name_roundtrip() {
        let t = AgentType::from_name("Architect");
        assert_eq!(t, AgentType::Architect);
        let u = AgentType::from_name("SomethingUnknown");
        assert!(matches!(u, AgentType::Custom(_)));
    }
}

#[cfg(test)]
mod tests_snake_to_pascal {
    use super::{snake_to_pascal, AgentType};

    /// Il test arriva alla CONSEGUENZA, non alla stringa (regola O): quel nome
    /// serve a `from_name`, e cio' che conta e' che ne esca la variante giusta
    /// e non un `Custom` che le somiglia.
    ///
    /// MUTAZIONE: togliendo una qualunque delle sigle dal punto unico, il caso
    /// corrispondente rosseggia con `Custom("SreEngineer")` al posto di
    /// `SREEngineer` — esattamente il difetto che la copia povera produceva.
    #[test]
    fn le_sigle_producono_la_variante_e_non_un_custom() {
        let casi = [
            ("sre_engineer", AgentType::SREEngineer),
            ("api_designer", AgentType::APIDesigner),
            ("ml_engineer", AgentType::MLEngineer),
            ("github_pr_manager", AgentType::GitHubPRManager),
            ("coder", AgentType::Coder),
            ("tech_lead", AgentType::TechLead),
        ];
        for (snake, atteso) in casi {
            let ottenuto = AgentType::from_name(&snake_to_pascal(snake));
            assert_eq!(
                ottenuto, atteso,
                "'{snake}' deve dare la variante, non un Custom che le somiglia"
            );
            assert!(
                !matches!(ottenuto, AgentType::Custom(_)),
                "'{snake}' e' finito in Custom: la sigla non e' stata riallineata"
            );
        }
    }

    /// Idempotenza: un nome gia' PascalCase attraversa la funzione intatto.
    /// Senza, una seconda conversione dello stesso valore lo corromperebbe.
    #[test]
    fn un_nome_gia_pascal_resta_invariato() {
        for gia_pascal in ["Coder", "SREEngineer", "GitHubPRManager"] {
            assert_eq!(snake_to_pascal(gia_pascal), gia_pascal);
        }
    }
}
