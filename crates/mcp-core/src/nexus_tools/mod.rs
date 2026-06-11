//! Tool Nexus residenti in mcp-core: i 9 accoppiati a NexusBridge /
//! nexus_tool_catalog. Tutto il resto vive nel crate nexus-tool-kit
//! (split 7.4 fase B): il re-export sottostante mantiene validi i path
//! `crate::nexus_tools::*` per i ~70 moduli che li usano.
pub use nexus_tool_kit::*;

pub mod consensus_vote;
pub mod memory_ns;
pub mod meta_catalog_count;
pub mod meta_categories_list;
pub mod meta_health_summary;
pub mod meta_self_test;
pub mod ruvector_insert;
pub mod ruvector_search;
pub mod ruvector_stats;
