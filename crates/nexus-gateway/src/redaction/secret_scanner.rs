//! Re-export del secret scanner su STRINGA in-memory.
//!
//! Il punto unico (regola L / ADR 0026) vive in
//! `nexus_tool_kit::secret_text_scanner`: nato qui come porting di
//! `packages/shared/src/secret-scanner.ts`, e' stato spostato nel crate
//! condiviso perche' serve anche a `mcp-core` (redazione output processi,
//! difesa in profondita' post-incidente Beaty-Book 2026-07-02). I call site
//! del gateway (`pipeline.rs`, `sensitivity_classifier.rs`) restano invariati
//! grazie a questo re-export.
//!
//! Il tipo tier ritornato (`secret_text_scanner::SensitivityTier`) e' lo
//! stesso alias `u8` di `crate::types::SensitivityTier`.

pub use nexus_tool_kit::secret_text_scanner::{
    FoundPattern, PatternType, ScanResult, SecretScanner,
};
