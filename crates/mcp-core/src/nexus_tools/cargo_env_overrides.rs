//! `build::cargo_env_overrides` — legge env var rilevanti per cargo.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Map, Value};

pub struct CargoEnvOverridesTool;

const CARGO_ENV_VARS: &[&str] = &[
    "RUSTFLAGS", "RUSTDOCFLAGS", "CARGO_TARGET_DIR", "CARGO_HOME", "CARGO_INCREMENTAL",
    "CARGO_BUILD_JOBS", "CARGO_BUILD_TARGET", "CARGO_PROFILE_RELEASE_LTO",
    "CARGO_PROFILE_RELEASE_OPT_LEVEL", "CARGO_PROFILE_RELEASE_DEBUG",
    "CARGO_REGISTRIES_CRATES_IO_PROTOCOL", "CARGO_NET_OFFLINE", "RUST_BACKTRACE",
];

#[async_trait]
impl NexusToolHandler for CargoEnvOverridesTool {
    async fn execute(&self, _ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let mut values = Map::new();
        let mut set_count = 0;
        for k in CARGO_ENV_VARS {
            if let Ok(v) = std::env::var(k) {
                values.insert(k.to_string(), Value::String(v));
                set_count += 1;
            }
        }
        Ok(json!({"ok": true, "set_count": set_count, "total_checked": CARGO_ENV_VARS.len(), "values": Value::Object(values)}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
