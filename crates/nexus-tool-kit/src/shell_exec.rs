use super::exec::run_cmd_owned;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::Value;

pub struct ShellExecTool;

#[async_trait]
impl NexusToolHandler for ShellExecTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("command required".into()))?
            .to_string();

        let timeout_secs: u64 = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(300); // default 5 min: sufficiente per docker build / npm install

        // Punto unico (regola L): crate::sandbox::agent_shell -> bash su Unix,
        // Git Bash su Windows (tail/grep/&&/| nativi). Eseguito con "-c".
        let shell = crate::sandbox::agent_shell();
        let out = run_cmd_owned(
            &shell,
            &["-c", command.as_str()],
            &ctx.project_root,
            timeout_secs,
        )
        .await?;

        let mut map = serde_json::Map::new();
        map.insert("ok".into(), serde_json::Value::Bool(out.exit_code == 0));
        map.insert(
            "exit_code".into(),
            serde_json::Value::Number(out.exit_code.into()),
        );
        map.insert(
            "stdout".into(),
            serde_json::Value::String(out.stdout.clone()),
        );
        map.insert(
            "stderr".into(),
            serde_json::Value::String(out.stderr.clone()),
        );
        map.insert(
            "duration_ms".into(),
            serde_json::Value::Number(out.duration_ms.into()),
        );
        map.insert("command".into(), serde_json::Value::String(command));
        Ok(serde_json::Value::Object(map))
    }

    fn input_schema(&self) -> Value {
        serde_json::from_str(r#"{"type":"object","required":["command"],"properties":{"command":{"type":"string"},"project_id":{"type":"string"},"timeout_secs":{"type":"integer"}}}"#).unwrap_or(Value::Null)
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::write_subproc()
    }
}
