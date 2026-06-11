//! Nexus Tool Catalog — registro estendibile dei tool Ruflo (target: 314 tool).
//!
//! Scaffold di Fase 6: definisce la struttura per descrivere i tool che Ruflo
//! espone nativi (AST analysis, security scanning, dependency audit, ecc.) e
//! li organizza per categoria. I tool NON sono ancora implementati, ma il
//! catalogo fornisce il registry e le categorie canoniche su cui appoggiarsi
//! nelle fasi successive.
//!
//! Uso:
//! - `NexusToolCatalog::new()` crea un catalogo vuoto
//! - `NexusToolCatalog::with_builtins()` carica il seed iniziale (categorie note)
//! - `register(spec)` aggiunge un tool
//! - `list_by_category(cat)` filtra per categoria
//! - `len()` / `is_empty()` per statistiche
//!
//! Il catalogo è thread-safe (DashMap) ed esponibile come singleton tramite
//! `init_global` / `global`, sullo stesso pattern di `NexusBridge`.

use crate::nexus_tools::{NexusToolContext, NexusToolError, SharedHandler};
use dashmap::DashMap;
use serde_json::Value;
use std::sync::{Arc, OnceLock};

mod build_deploy;
mod code_analysis;
mod database;
mod dependencies;
mod documentation;
mod github;
mod memory_meta;
mod performance;
mod security;
mod testing;
mod utility;
mod vcs;

/// Categorie canoniche per i 314 tool Ruflo.
///
/// Sono raggruppate per dominio funzionale. Numeri indicativi per riferimento
/// alla spec Ruflo v3.5 — i count effettivi verranno popolati nelle fasi
/// successive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NexusToolCategory {
    /// Analisi AST, parsing, semantic analysis
    CodeAnalysis,
    /// Formatting, linting, style enforcement
    CodeQuality,
    /// Test generation, coverage, mutation testing
    Testing,
    /// SAST, secret scanning, dependency audit
    Security,
    /// Refactoring, code transformation
    Refactoring,
    /// Documentation generation
    Documentation,
    /// Dependency management
    Dependencies,
    /// Build, compilation, packaging
    Build,
    /// Git/VCS operations
    Vcs,
    /// GitHub/GitLab integration
    GitHub,
    /// Profiling, benchmarking, metrics
    Performance,
    /// Database schema, queries, migrations
    Database,
    /// API design, OpenAPI, GraphQL
    Api,
    /// Deployment, CI/CD
    Deployment,
    /// Memory/namespace operations
    Memory,
    /// Generic utilities (string ops, regex, ecc.)
    Utility,
    /// Altri
    Other,
}

impl NexusToolCategory {
    pub fn name(&self) -> &'static str {
        match self {
            Self::CodeAnalysis => "code_analysis",
            Self::CodeQuality => "code_quality",
            Self::Testing => "testing",
            Self::Security => "security",
            Self::Refactoring => "refactoring",
            Self::Documentation => "documentation",
            Self::Dependencies => "dependencies",
            Self::Build => "build",
            Self::Vcs => "vcs",
            Self::GitHub => "github",
            Self::Performance => "performance",
            Self::Database => "database",
            Self::Api => "api",
            Self::Deployment => "deployment",
            Self::Memory => "memory",
            Self::Utility => "utility",
            Self::Other => "other",
        }
    }

    /// Tutte le categorie per iterazione
    pub fn all() -> &'static [NexusToolCategory] {
        &[
            Self::CodeAnalysis,
            Self::CodeQuality,
            Self::Testing,
            Self::Security,
            Self::Refactoring,
            Self::Documentation,
            Self::Dependencies,
            Self::Build,
            Self::Vcs,
            Self::GitHub,
            Self::Performance,
            Self::Database,
            Self::Api,
            Self::Deployment,
            Self::Memory,
            Self::Utility,
            Self::Other,
        ]
    }
}

/// Specifica di un tool registrato nel catalogo
#[derive(Debug, Clone)]
pub struct NexusToolSpec {
    /// Nome univoco (es. "ast_parse_rust", "audit_deps_cargo")
    pub name: String,
    /// Categoria
    pub category: NexusToolCategory,
    /// Breve descrizione per LLM / UI
    #[expect(
        dead_code,
        reason = "superficie descrittiva del catalogo tool, esposizione UI/LLM pianificata; popolata da ~300 seed"
    )]
    pub description: String,
    /// Tool implementato (true) o scaffold stub (false)
    pub implemented: bool,
}

impl NexusToolSpec {
    pub fn new(
        name: impl Into<String>,
        category: NexusToolCategory,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            category,
            description: description.into(),
            implemented: false,
        }
    }
}

/// Entry interna del catalog: avvolge la spec con un handler opzionale.
///
/// Fase 9A: i tool con `handler: Some(...)` sono eseguibili via
/// `NexusToolCatalog::execute`; quelli con `handler: None` restano scaffold
/// descrittivi (i 27 stub del seed). L'API pubblica `get(name)` continua a
/// ritornare solo la spec, per retrocompatibilità con l'endpoint
/// `/nexus/tools` e con i test esistenti.
#[derive(Clone)]
struct NexusToolEntry {
    spec: NexusToolSpec,
    handler: Option<SharedHandler>,
}

impl std::fmt::Debug for NexusToolEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NexusToolEntry")
            .field("spec", &self.spec)
            .field("has_handler", &self.handler.is_some())
            .finish()
    }
}

/// Catalogo dei tool nexus. Thread-safe via DashMap.
pub struct NexusToolCatalog {
    tools: DashMap<String, NexusToolEntry>,
}

impl NexusToolCatalog {
    pub fn new() -> Self {
        Self {
            tools: DashMap::new(),
        }
    }

    /// Inizializza con il seed canonico di categorie (scaffold — tool stub
    /// rappresentativi per ogni categoria). Le implementazioni reali verranno
    /// aggiunte nelle fasi successive.
    pub fn with_builtins() -> Self {
        let catalog = Self::new();

        // Seed rappresentativo — ~1-2 tool stub per categoria.
        // Questi nomi sono placeholder coerenti con Ruflo v3.5 spec.
        let seed: Vec<NexusToolSpec> = vec![
            // Code analysis
            NexusToolSpec::new(
                "ast_parse",
                NexusToolCategory::CodeAnalysis,
                "Parse source into AST (Rust/TS/Python)",
            ),
            NexusToolSpec::new(
                "ast_query",
                NexusToolCategory::CodeAnalysis,
                "Query AST nodes by type/name",
            ),
            // Code quality
            NexusToolSpec::new(
                "lint_run",
                NexusToolCategory::CodeQuality,
                "Run linters (clippy, eslint, ruff)",
            ),
            NexusToolSpec::new(
                "format_code",
                NexusToolCategory::CodeQuality,
                "Apply formatter (rustfmt, prettier, black)",
            ),
            // Testing
            NexusToolSpec::new(
                "test_generate",
                NexusToolCategory::Testing,
                "Generate test cases from function signature",
            ),
            NexusToolSpec::new(
                "coverage_report",
                NexusToolCategory::Testing,
                "Compute code coverage",
            ),
            // Security
            NexusToolSpec::new(
                "sast_scan",
                NexusToolCategory::Security,
                "Static security analysis",
            ),
            NexusToolSpec::new(
                "secret_scan",
                NexusToolCategory::Security,
                "Detect secrets / credentials",
            ),
            NexusToolSpec::new(
                "deps_audit",
                NexusToolCategory::Security,
                "Audit dependencies for known CVEs",
            ),
            // Refactoring
            NexusToolSpec::new(
                "rename_symbol",
                NexusToolCategory::Refactoring,
                "Rename a symbol across the project",
            ),
            NexusToolSpec::new(
                "extract_function",
                NexusToolCategory::Refactoring,
                "Extract selection into new function",
            ),
            // Documentation
            NexusToolSpec::new(
                "doc_generate",
                NexusToolCategory::Documentation,
                "Generate docstrings from code",
            ),
            // Dependencies
            NexusToolSpec::new(
                "deps_tree",
                NexusToolCategory::Dependencies,
                "Show dependency tree",
            ),
            // Build
            NexusToolSpec::new(
                "build_project",
                NexusToolCategory::Build,
                "Run the project build pipeline",
            ),
            // VCS
            NexusToolSpec::new("git_diff", NexusToolCategory::Vcs, "Show git diff"),
            NexusToolSpec::new(
                "git_blame",
                NexusToolCategory::Vcs,
                "Show git blame for a file",
            ),
            // GitHub
            NexusToolSpec::new(
                "gh_pr_create",
                NexusToolCategory::GitHub,
                "Create a GitHub pull request",
            ),
            NexusToolSpec::new(
                "gh_issue_list",
                NexusToolCategory::GitHub,
                "List GitHub issues",
            ),
            // Performance
            NexusToolSpec::new(
                "profile_run",
                NexusToolCategory::Performance,
                "Profile a function / endpoint",
            ),
            NexusToolSpec::new(
                "bench_run",
                NexusToolCategory::Performance,
                "Run benchmarks",
            ),
            // Database
            NexusToolSpec::new(
                "db_schema_inspect",
                NexusToolCategory::Database,
                "Inspect database schema",
            ),
            NexusToolSpec::new(
                "db_query_explain",
                NexusToolCategory::Database,
                "Explain query plan",
            ),
            // API
            NexusToolSpec::new(
                "openapi_validate",
                NexusToolCategory::Api,
                "Validate OpenAPI spec",
            ),
            // Deployment
            NexusToolSpec::new(
                "deploy_check",
                NexusToolCategory::Deployment,
                "Pre-deploy readiness checks",
            ),
            // Memory
            NexusToolSpec::new(
                "memory_ns_read",
                NexusToolCategory::Memory,
                "Read from a memory namespace",
            ),
            NexusToolSpec::new(
                "memory_ns_write",
                NexusToolCategory::Memory,
                "Write into a memory namespace",
            ),
            // Utility
            NexusToolSpec::new(
                "regex_match",
                NexusToolCategory::Utility,
                "Run a regex over text",
            ),
        ];

        for spec in seed {
            catalog.register(spec);
        }

        catalog
    }

    /// Registra un tool SENZA handler (scaffold stub). Idempotente.
    pub fn register(&self, spec: NexusToolSpec) {
        let name = spec.name.clone();
        self.tools.insert(
            name,
            NexusToolEntry {
                spec,
                handler: None,
            },
        );
    }

    /// Registra un tool CON handler eseguibile. Marca automaticamente
    /// `spec.implemented = true`.
    ///
    /// Fase 9A: questa è l'API usata dai 20 handler reali. Sovrascrive
    /// eventuali stub pre-esistenti con lo stesso nome, unificando la
    /// rappresentazione (prima scaffold, ora eseguibile).
    pub fn register_with_handler(&self, mut spec: NexusToolSpec, handler: SharedHandler) {
        spec.implemented = true;
        let name = spec.name.clone();
        self.tools.insert(
            name,
            NexusToolEntry {
                spec,
                handler: Some(handler),
            },
        );
    }

    /// Recupera la spec di un tool per nome (compat. con versione Fase 6).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "zero call site oggi; candidata a rimozione in un passaggio successivo di bonifica"
        )
    )]
    pub fn get(&self, name: &str) -> Option<NexusToolSpec> {
        self.tools.get(name).map(|r| r.spec.clone())
    }

    /// Recupera l'handler eseguibile di un tool, se presente.
    ///
    /// Ritorna `None` se il tool non esiste o è ancora un scaffold stub.
    pub fn get_handler(&self, name: &str) -> Option<SharedHandler> {
        self.tools
            .get(name)
            .and_then(|r| r.handler.as_ref().cloned())
    }

    /// Esegue un tool per nome. Facciata sul trait `NexusToolHandler`.
    ///
    /// Errori specifici:
    /// - `NexusToolError::BadInput("unknown tool: ...")` se il nome non
    ///   corrisponde a nessun tool registrato;
    /// - `NexusToolError::BadInput("tool is stub-only: ...")` se il tool
    ///   esiste come spec ma non ha handler associato.
    pub async fn execute(
        &self,
        name: &str,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let handler = match self.get_handler(name) {
            Some(h) => h,
            None => {
                // Distinguo tool inesistente vs stub-only per diagnostica
                if self.tools.contains_key(name) {
                    return Err(NexusToolError::BadInput(format!(
                        "tool is stub-only: {}",
                        name
                    )));
                } else {
                    return Err(NexusToolError::BadInput(format!("unknown tool: {}", name)));
                }
            }
        };
        // Molti handler NexusTool usano std::fs::* bloccante dentro execute().
        // Eseguiamo su spawn_blocking per non congelare i worker tokio.
        let ctx = ctx.clone();
        let args = args.clone();
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || handle.block_on(handler.execute(&ctx, &args)))
            .await
            .map_err(|e| {
                NexusToolError::Io(std::io::Error::other(
                    format!("tool join error: {e}"),
                ))
            })?
    }

    /// Lista tool di una specifica categoria
    pub fn list_by_category(&self, category: NexusToolCategory) -> Vec<NexusToolSpec> {
        self.tools
            .iter()
            .filter(|r| r.spec.category == category)
            .map(|r| r.spec.clone())
            .collect()
    }

    /// Numero totale di tool registrati
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "richiesto dal lint clippy len_without_is_empty accanto a len()"
        )
    )]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Numero di tool effettivamente implementati.
    ///
    /// Un tool è considerato implementato se ha un handler associato
    /// (registrato tramite `register_with_handler`). Il vecchio flag
    /// `spec.implemented` è mantenuto per retrocompatibilità ma il
    /// conteggio di verità arriva dalla presenza dell'handler.
    pub fn implemented_count(&self) -> usize {
        self.tools.iter().filter(|r| r.handler.is_some()).count()
    }

    /// Breakdown per categoria — utile per diagnostica
    pub fn breakdown(&self) -> Vec<(NexusToolCategory, usize)> {
        NexusToolCategory::all()
            .iter()
            .map(|c| (*c, self.list_by_category(*c).len()))
            .collect()
    }
}

impl Default for NexusToolCatalog {
    fn default() -> Self {
        Self::with_builtins()
    }
}

/// Singleton globale del catalogo
static NEXUS_TOOL_CATALOG: OnceLock<Arc<NexusToolCatalog>> = OnceLock::new();

impl NexusToolCatalog {
    /// Inizializza il singleton globale (idempotent).
    ///
    /// Fase 9A: dopo aver caricato i 27 stub del seed, registra anche gli
    /// handler eseguibili del blocco "prototype" (cargo_check + git_status).
    /// Gli stub pre-esistenti con lo stesso nome vengono sovrascritti con la
    /// versione eseguibile tramite `register_with_handler`.
    pub fn init_global() {
        NEXUS_TOOL_CATALOG.get_or_init(|| {
            let catalog = Self::with_builtins();
            catalog.register_fase9_handlers();
            Arc::new(catalog)
        });
    }

    /// Accesso al singleton
    pub fn global() -> Option<Arc<Self>> {
        NEXUS_TOOL_CATALOG.get().cloned()
    }
    /// Registra gli handler eseguibili (target finale: 314+ tool).
    ///
    /// Il corpo storico (un singolo metodo da ~2400 righe) e' stato
    /// suddiviso per dominio nei submoduli di `nexus_tool_catalog`.
    /// Ogni submodulo espone `register(&NexusToolCatalog)` e contiene
    /// esclusivamente le chiamate `register_with_handler` della propria
    /// area. L'ordine di invocazione e l'insieme dei tool registrati
    /// restano invariati rispetto alla versione monolitica.
    fn register_fase9_handlers(&self) {
        code_analysis::register(self);
        testing::register(self);
        security::register(self);
        dependencies::register(self);
        build_deploy::register(self);
        performance::register(self);
        vcs::register(self);
        github::register(self);
        documentation::register(self);
        database::register(self);
        memory_meta::register(self);
        utility::register(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_catalog_empty() {
        let c = NexusToolCatalog::new();
        assert_eq!(c.len(), 0);
        assert!(c.is_empty());
    }

    #[test]
    fn test_catalog_with_builtins() {
        let c = NexusToolCatalog::with_builtins();
        assert!(!c.is_empty());
        // Il seed dovrebbe coprire tutte le categorie principali
        let bd = c.breakdown();
        let covered: usize = bd.iter().filter(|(_, n)| *n > 0).count();
        assert!(
            covered >= 10,
            "Expected >= 10 categories covered, got {}",
            covered
        );
    }

    #[test]
    fn test_register_and_get() {
        let c = NexusToolCatalog::new();
        c.register(NexusToolSpec::new(
            "custom_tool",
            NexusToolCategory::Utility,
            "A test tool",
        ));
        let spec = c.get("custom_tool").unwrap();
        assert_eq!(spec.name, "custom_tool");
        assert_eq!(spec.category, NexusToolCategory::Utility);
        assert!(!spec.implemented);
    }

    #[test]
    fn test_list_by_category() {
        let c = NexusToolCatalog::with_builtins();
        let security = c.list_by_category(NexusToolCategory::Security);
        assert!(security.len() >= 2);
    }

    #[test]
    fn test_implemented_field() {
        // Il builder .implemented() e' stato rimosso (mai usato in produzione,
        // bonifica 2026-06-11): il campo si assegna direttamente.
        let mut spec = NexusToolSpec::new("x", NexusToolCategory::Other, "y");
        spec.implemented = true;
        assert!(spec.implemented);
    }

    #[test]
    fn test_global_singleton() {
        NexusToolCatalog::init_global();
        assert!(NexusToolCatalog::global().is_some());
        let a = NexusToolCatalog::global().unwrap();
        let b = NexusToolCatalog::global().unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }
}
