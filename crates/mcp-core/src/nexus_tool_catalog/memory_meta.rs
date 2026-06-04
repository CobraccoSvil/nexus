//! Registrazione handler dominio: memory_meta
//!
//! Generato dal refactor di `nexus_tool_catalog.rs` (god-file split).
//! Nessun cambiamento di comportamento: spostamento puro delle
//! chiamate `register_with_handler` raggruppate per dominio.

use super::{NexusToolCatalog, NexusToolCategory, NexusToolSpec};
use std::sync::Arc;

pub(super) fn register(c: &NexusToolCatalog) {
    use crate::nexus_tools::{
        api_endpoint_list::ApiEndpointListTool, api_graphql_check::ApiGraphqlCheckTool,
        api_grpc_check::ApiGrpcCheckTool, api_handler_count::ApiHandlerCountTool,
        api_middleware_count::ApiMiddlewareCountTool, api_openapi_files::ApiOpenapiFilesTool,
        api_postman_check::ApiPostmanCheckTool, api_route_count::ApiRouteCountTool,
        consensus_vote::ConsensusVoteTool, extract_function::ExtractFunctionTool,
        memory_evict_stats::MemoryEvictStatsTool, memory_namespace_count::MemoryNamespaceCountTool,
        memory_ns::MemoryNsReadTool, memory_ns::MemoryNsWriteTool,
        memory_pattern_list::MemoryPatternListTool, memory_recent_writes::MemoryRecentWritesTool,
        memory_size_estimate::MemorySizeEstimateTool, memory_topkeys::MemoryTopkeysTool,
        meta_catalog_count::MetaCatalogCountTool, meta_categories_list::MetaCategoriesListTool,
        meta_health_summary::MetaHealthSummaryTool, meta_self_test::MetaSelfTestTool,
        meta_version_info::MetaVersionInfoTool, openapi_validate::OpenApiValidateTool,
        rename_symbol::RenameSymbolTool, ruvector_insert::RuVectorInsertTool,
        ruvector_search::RuVectorSearchTool, ruvector_stats::RuVectorStatsTool,
        util_cpu_count::UtilCpuCountTool, util_disk_free::UtilDiskFreeTool,
        util_hostname::UtilHostnameTool, util_now_iso::UtilNowIsoTool, util_pid::UtilPidTool,
        util_uptime::UtilUptimeTool,
    };

    // Memory (Fase 9C)
    c.register_with_handler(
        NexusToolSpec::new(
            "memory_ns_read",
            NexusToolCategory::Memory,
            "Read a key from the project-scoped NexusBridge memory namespace",
        ),
        Arc::new(MemoryNsReadTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "memory_ns_write",
            NexusToolCategory::Memory,
            "Write a JSON value into the project-scoped NexusBridge memory namespace",
        ),
        Arc::new(MemoryNsWriteTool),
    );

    // Refactoring (Fase 9D)
    c.register_with_handler(
        NexusToolSpec::new(
            "rename_symbol",
            NexusToolCategory::Refactoring,
            "Rename a symbol within a single file (word-boundary regex)",
        ),
        Arc::new(RenameSymbolTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "extract_function",
            NexusToolCategory::Refactoring,
            "Mechanical extract-function scaffold for Rust/TS/JS/Python",
        ),
        Arc::new(ExtractFunctionTool),
    );

    // API / Memory / Other (Fase 9R, 20 new)
    c.register_with_handler(
        NexusToolSpec::new(
            "api_openapi_files",
            NexusToolCategory::Api,
            "Find openapi/swagger spec files",
        ),
        Arc::new(ApiOpenapiFilesTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "api_route_count",
            NexusToolCategory::Api,
            "Count axum/actix/warp/rocket route declarations",
        ),
        Arc::new(ApiRouteCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "api_handler_count",
            NexusToolCategory::Api,
            "Count async fn handlers (heuristic)",
        ),
        Arc::new(ApiHandlerCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "api_endpoint_list",
            NexusToolCategory::Api,
            "Extract endpoint paths from .route() literals",
        ),
        Arc::new(ApiEndpointListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "api_graphql_check",
            NexusToolCategory::Api,
            "Detect GraphQL schemas/usages",
        ),
        Arc::new(ApiGraphqlCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "api_grpc_check",
            NexusToolCategory::Api,
            "Detect gRPC/.proto/tonic usages",
        ),
        Arc::new(ApiGrpcCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "api_postman_check",
            NexusToolCategory::Api,
            "Find postman collection files",
        ),
        Arc::new(ApiPostmanCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "api_middleware_count",
            NexusToolCategory::Api,
            "Count tower/axum middleware layer registrations",
        ),
        Arc::new(ApiMiddlewareCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "memory_namespace_count",
            NexusToolCategory::Memory,
            "Count distinct memory namespaces in DB",
        ),
        Arc::new(MemoryNamespaceCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "memory_size_estimate",
            NexusToolCategory::Memory,
            "Estimate aggregate memory_namespace size",
        ),
        Arc::new(MemorySizeEstimateTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "memory_pattern_list",
            NexusToolCategory::Memory,
            "List distinct memory keys/patterns",
        ),
        Arc::new(MemoryPatternListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "memory_recent_writes",
            NexusToolCategory::Memory,
            "Recent memory_namespace writes",
        ),
        Arc::new(MemoryRecentWritesTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "memory_topkeys",
            NexusToolCategory::Memory,
            "Top namespaces by row count",
        ),
        Arc::new(MemoryTopkeysTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "memory_evict_stats",
            NexusToolCategory::Memory,
            "Evictable rows older than TTL",
        ),
        Arc::new(MemoryEvictStatsTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "util_disk_free",
            NexusToolCategory::Utility,
            "Best-effort disk info at project_root",
        ),
        Arc::new(UtilDiskFreeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "util_uptime",
            NexusToolCategory::Utility,
            "Process uptime in seconds since first call",
        ),
        Arc::new(UtilUptimeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "util_hostname",
            NexusToolCategory::Utility,
            "Hostname/user from environment",
        ),
        Arc::new(UtilHostnameTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "util_cpu_count",
            NexusToolCategory::Utility,
            "Logical CPU count via available_parallelism",
        ),
        Arc::new(UtilCpuCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "util_now_iso",
            NexusToolCategory::Utility,
            "Current time as RFC3339 + epoch seconds",
        ),
        Arc::new(UtilNowIsoTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "util_pid",
            NexusToolCategory::Utility,
            "Process id of running mcp-core",
        ),
        Arc::new(UtilPidTool),
    );

    // Final meta tools (Fase 9S, 5 new — total 314)
    c.register_with_handler(
        NexusToolSpec::new(
            "meta_catalog_count",
            NexusToolCategory::Other,
            "Total + implemented tool counts in catalog",
        ),
        Arc::new(MetaCatalogCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "meta_categories_list",
            NexusToolCategory::Other,
            "List all NexusToolCategory variants with counts",
        ),
        Arc::new(MetaCategoriesListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "meta_version_info",
            NexusToolCategory::Other,
            "Crate name/version + profile + os/arch",
        ),
        Arc::new(MetaVersionInfoTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "meta_health_summary",
            NexusToolCategory::Other,
            "Basic health: project_root, db, catalog",
        ),
        Arc::new(MetaHealthSummaryTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "meta_self_test",
            NexusToolCategory::Other,
            "Smoke-test a small set of read-only handlers",
        ),
        Arc::new(MetaSelfTestTool),
    );

    // Api (Fase 9D)
    c.register_with_handler(
        NexusToolSpec::new(
            "openapi_validate",
            NexusToolCategory::Api,
            "Validate OpenAPI spec (JSON parse + structural checks)",
        ),
        Arc::new(OpenApiValidateTool),
    );

    // Fase 9E: RuVector + Consensus (4 new)
    c.register_with_handler(
        NexusToolSpec::new(
            "ruvector_insert",
            NexusToolCategory::Memory,
            "Embed and insert a text into the global HNSW vector database",
        ),
        Arc::new(RuVectorInsertTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ruvector_search",
            NexusToolCategory::Memory,
            "k-NN semantic search over the global HNSW vector database",
        ),
        Arc::new(RuVectorSearchTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ruvector_stats",
            NexusToolCategory::Memory,
            "Stats for the global HNSW vector database (nodes, fan-out, entry point)",
        ),
        Arc::new(RuVectorStatsTool),
    );
    c.register_with_handler(
    NexusToolSpec::new(
        "consensus_vote",
        NexusToolCategory::Utility,
        "Evaluate multi-agent votes via ConsensusEngine (majority/supermajority/unanimous/weighted)",
    ),
    Arc::new(ConsensusVoteTool),
    );
}
