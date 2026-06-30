//! `other::meta_version_info` — crate version, profile, target.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct MetaVersionInfoTool;

#[async_trait]
impl NexusToolHandler for MetaVersionInfoTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let pkg_name = env!("CARGO_PKG_NAME");
        let pkg_version = env!("CARGO_PKG_VERSION");
        let profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        let target_os = std::env::consts::OS;
        let target_arch = std::env::consts::ARCH;
        Ok(json!({
            "ok": true,
            "crate": pkg_name,
            "version": pkg_version,
            "profile": profile,
            "os": target_os,
            "arch": target_arch,
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
