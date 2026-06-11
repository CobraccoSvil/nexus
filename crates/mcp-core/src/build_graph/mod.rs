//! Build graph derivato dai config di progetto (ADR 0020).
//!
//! Sostituisce ADR 0019 L1 (preflight grep) + L2 (directory policy DB-driven)
//! con un approccio strutturale: i resolver per linguaggio leggono i file di
//! configurazione (tsconfig.json, Cargo.toml, pyproject.toml, go.mod) ed
//! emettono `BuildGraphInfo` con include/exclude glob, entry point, generated
//! dirs e monorepo members.
//!
//! API pubblica:
//! - `BuildGraphCache::init_global(db)` / `BuildGraphCache::global()` — singleton
//! - `is_in_build_graph(project_id, file_path)` — lookup runtime
//! - `BuildGraphInfo`, `BuildGraphMembership` — modelli condivisi
//! - `handle_build_graph_info(...)` — handler MCP tool `nexus_build_graph_info`

pub mod cache;
pub mod membership;
pub mod model;
pub mod resolver_go;
pub mod resolver_python;
pub mod resolver_rust;
pub mod resolver_typescript;
pub mod tool;

pub use cache::BuildGraphCache;
pub use membership::is_in_build_graph;
pub use model::BuildGraphMembership;
pub use tool::handle_build_graph_info;
