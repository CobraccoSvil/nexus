//! `memory::memory_ns_read` / `memory::memory_ns_write` — accesso key/value
//! al `MemoryNamespace` globale esposto dal `NexusBridge`.
//!
//! Le chiavi sono scopate automaticamente per project: il caller passa una
//! chiave logica (`"design_notes"`), l'handler la trasforma in
//! `"project:<uuid>:design_notes"` prima di interagire col namespace. Questo
//! garantisce isolamento tra progetti senza richiedere un registry di
//! namespace per-progetto.
//!
//! Uso tipico:
//! - Agente A scrive `memory_ns_write { key: "api_contract", value: {...} }`
//! - Agente B, nello stesso progetto, legge `memory_ns_read { key: "api_contract" }`
//! - Gli agenti in altri progetti non vedono il dato (prefisso diverso).
//!
//! Sicurezza:
//! - Il namespace è in-memory: i dati non persistono oltre il restart del
//!   processo mcp-core. Per persistenza durevole usare tool DB dedicati.
//! - Nessun subprocess, nessuna scrittura FS: safety `read_only` per read,
//!   `read_only` anche per write (niente FS, niente processi).

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

fn scoped_key(ctx: &NexusToolContext, key: &str) -> String {
    format!("project:{}:{}", ctx.project_id, key)
}

// ─────────────────────────────────────────────────────────────────────────
// memory_ns_read
// ─────────────────────────────────────────────────────────────────────────

pub struct MemoryNsReadTool;

#[async_trait]
impl NexusToolHandler for MemoryNsReadTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let key = args
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("parametro 'key' obbligatorio".into()))?;

        let bridge = crate::nexus_bridge::NexusBridge::global().ok_or_else(|| {
            NexusToolError::BadInput("NexusBridge non inizializzato".into())
        })?;

        let scoped = scoped_key(ctx, key);
        match bridge.observability_ns().get(&scoped) {
            Some(entry) => Ok(json!({
                "found": true,
                "key": key,
                "scoped_key": scoped,
                "value": entry.value,
                "author": entry.author,
                "age_seconds": entry.created_at.elapsed().as_secs(),
            })),
            None => Ok(json!({
                "found": false,
                "key": key,
                "scoped_key": scoped,
            })),
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["key"],
            "properties": {
                "key": {"type": "string", "description": "Chiave logica (scoping project-level automatico)"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}

// ─────────────────────────────────────────────────────────────────────────
// memory_ns_write
// ─────────────────────────────────────────────────────────────────────────

pub struct MemoryNsWriteTool;

#[async_trait]
impl NexusToolHandler for MemoryNsWriteTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let key = args
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("parametro 'key' obbligatorio".into()))?;

        let value = args
            .get("value")
            .cloned()
            .ok_or_else(|| NexusToolError::BadInput("parametro 'value' obbligatorio".into()))?;

        let author = args
            .get("author")
            .and_then(Value::as_str)
            .unwrap_or("mcp-tool")
            .to_string();

        let ttl_secs = args.get("ttl_seconds").and_then(Value::as_u64);

        let bridge = crate::nexus_bridge::NexusBridge::global().ok_or_else(|| {
            NexusToolError::BadInput("NexusBridge non inizializzato".into())
        })?;

        let scoped = scoped_key(ctx, key);
        let ns = bridge.observability_ns();
        match ttl_secs {
            Some(secs) if secs > 0 => {
                ns.set_with_ttl(
                    scoped.clone(),
                    value,
                    author.clone(),
                    std::time::Duration::from_secs(secs),
                );
            }
            _ => {
                ns.set(scoped.clone(), value, author.clone());
            }
        }

        Ok(json!({
            "ok": true,
            "key": key,
            "scoped_key": scoped,
            "author": author,
            "ttl_seconds": ttl_secs,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["key", "value"],
            "properties": {
                "key": {"type": "string"},
                "value": {"description": "JSON value da memorizzare"},
                "author": {"type": "string", "description": "Chi ha scritto (default: 'mcp-tool')"},
                "ttl_seconds": {"type": "integer", "description": "TTL opzionale in secondi (0/unset = nessuno)"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        // Scrive solo nel namespace in-memory: no FS, no subprocess.
        NexusToolSafety::read_only()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn ctx() -> NexusToolContext {
        NexusToolContext::new(std::env::temp_dir(), Uuid::nil(), Uuid::nil())
    }

    #[tokio::test]
    async fn test_write_then_read_roundtrip() {
        crate::nexus_bridge::NexusBridge::init_global();
        let c = ctx();

        let key = format!("rtrip-{}", Uuid::new_v4());
        let _ = MemoryNsWriteTool
            .execute(
                &c,
                &json!({"key": key, "value": {"answer": 42}, "author": "test"}),
            )
            .await
            .unwrap();

        let out = MemoryNsReadTool
            .execute(&c, &json!({"key": key}))
            .await
            .unwrap();
        assert_eq!(out["found"], true);
        assert_eq!(out["value"]["answer"], 42);
        assert_eq!(out["author"], "test");
    }

    #[tokio::test]
    async fn test_read_missing_returns_not_found() {
        crate::nexus_bridge::NexusBridge::init_global();
        let key = format!("missing-{}", Uuid::new_v4());
        let out = MemoryNsReadTool
            .execute(&ctx(), &json!({"key": key}))
            .await
            .unwrap();
        assert_eq!(out["found"], false);
    }

    #[tokio::test]
    async fn test_project_scoping_isolates() {
        crate::nexus_bridge::NexusBridge::init_global();

        let p1 = NexusToolContext::new(std::env::temp_dir(), Uuid::new_v4(), Uuid::nil());
        let p2 = NexusToolContext::new(std::env::temp_dir(), Uuid::new_v4(), Uuid::nil());

        let shared_key = format!("scopecheck-{}", Uuid::new_v4());

        let _ = MemoryNsWriteTool
            .execute(
                &p1,
                &json!({"key": shared_key, "value": "from-p1"}),
            )
            .await
            .unwrap();

        // p2 non deve vedere il dato di p1 perché scoping project-level
        let out = MemoryNsReadTool
            .execute(&p2, &json!({"key": shared_key}))
            .await
            .unwrap();
        assert_eq!(out["found"], false);
    }
}
