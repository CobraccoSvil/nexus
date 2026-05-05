//! `utility::time_now` — restituisce timestamp corrente in vari formati.
//!
//! Output: `{unix, unix_ms, iso8601, rfc3339, day_of_week, year, month, day,
//! hour, minute, second, tz}`. Usa `chrono::Utc` (già in workspace).

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use chrono::{Datelike, Timelike, Utc};
use serde_json::{json, Value};

pub struct TimeNowTool;

#[async_trait]
impl NexusToolHandler for TimeNowTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let now = Utc::now();
        Ok(json!({
            "ok": true,
            "unix": now.timestamp(),
            "unix_ms": now.timestamp_millis(),
            "iso8601": now.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "rfc3339": now.to_rfc3339(),
            "year": now.year(),
            "month": now.month(),
            "day": now.day(),
            "hour": now.hour(),
            "minute": now.minute(),
            "second": now.second(),
            "day_of_week": now.weekday().to_string(),
            "tz": "UTC",
        }))
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_time_now() {
        let ctx = NexusToolContext::new(std::env::temp_dir(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = TimeNowTool.execute(&ctx, &json!({})).await.unwrap();
        assert!(out["unix"].as_i64().unwrap() > 1_700_000_000);
        assert_eq!(out["tz"], "UTC");
        assert!(out["iso8601"].as_str().unwrap().ends_with('Z'));
    }
}
