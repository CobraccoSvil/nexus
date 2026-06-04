//! Registrazione handler dominio: dependencies
//!
//! Generato dal refactor di `nexus_tool_catalog.rs` (god-file split).
//! Nessun cambiamento di comportamento: spostamento puro delle
//! chiamate `register_with_handler` raggruppate per dominio.

use super::{NexusToolCatalog, NexusToolCategory, NexusToolSpec};
use std::sync::Arc;

pub(super) fn register(c: &NexusToolCatalog) {
    use crate::nexus_tools::{
        cargo_dep_versions::CargoDepVersionsTool, cargo_features_list::CargoFeaturesListTool,
        cargo_install_list::CargoInstallListTool, cargo_lockfile_check::CargoLockfileCheckTool,
        cargo_outdated::CargoOutdatedTool, cargo_search::CargoSearchTool,
        cargo_tree::CargoTreeTool, cargo_update::CargoUpdateTool, deps_tree::DepsTreeTool,
    };

    // Dependencies
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_tree",
            NexusToolCategory::Dependencies,
            "Run `cargo tree` and return dependency tree",
        ),
        Arc::new(CargoTreeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_outdated",
            NexusToolCategory::Dependencies,
            "Run `cargo outdated --format json` and return outdated deps",
        ),
        Arc::new(CargoOutdatedTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_update",
            NexusToolCategory::Dependencies,
            "Run `cargo update` to refresh Cargo.lock",
        ),
        Arc::new(CargoUpdateTool),
    );

    // Dependencies (Fase 9D)
    c.register_with_handler(
        NexusToolSpec::new(
            "deps_tree",
            NexusToolCategory::Dependencies,
            "Multi-stack dep tree (cargo tree / npm list / pipdeptree)",
        ),
        Arc::new(DepsTreeTool),
    );

    // Dependencies (Fase 9H)
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_install_list",
            NexusToolCategory::Dependencies,
            "Parse `cargo install --list` into name/version pairs",
        ),
        Arc::new(CargoInstallListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_search",
            NexusToolCategory::Dependencies,
            "`cargo search <query> --limit N` (network egress)",
        ),
        Arc::new(CargoSearchTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_features_list",
            NexusToolCategory::Dependencies,
            "Parse `[features]` section of root Cargo.toml",
        ),
        Arc::new(CargoFeaturesListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_dep_versions",
            NexusToolCategory::Dependencies,
            "Detect duplicate packages (same name, multiple versions)",
        ),
        Arc::new(CargoDepVersionsTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_lockfile_check",
            NexusToolCategory::Dependencies,
            "Verify Cargo.lock presence, version and package count",
        ),
        Arc::new(CargoLockfileCheckTool),
    );
}
