//! Tool sandbox: lettura e scrittura della configurazione sandbox del progetto.

use super::*;

pub(super) async fn tool_set_sandbox_config(ctx: &AgentToolContext, input: &Value) -> String {
    use crate::sandbox::{load_project_sandbox_config, save_project_sandbox_config};

    let mut cfg = load_project_sandbox_config(&ctx.db, ctx.project_id).await;

    if let Some(mb) = input.get("memory_mb").and_then(Value::as_u64) {
        cfg.memory_mb = Some(mb);
    }
    if let Some(c) = input.get("cpus").and_then(Value::as_f64) {
        cfg.cpus = Some(c);
    }
    if let Some(nm) = input.get("network_mode").and_then(Value::as_str) {
        cfg.network_mode = Some(nm.to_string());
    }
    if let Some(env_obj) = input.get("extra_env").and_then(Value::as_object) {
        let mut map = cfg.extra_env.unwrap_or_default();
        for (k, v) in env_obj {
            if let Some(vs) = v.as_str() {
                map.insert(k.clone(), vs.to_string());
            }
        }
        cfg.extra_env = Some(map);
    }

    match save_project_sandbox_config(&ctx.db, ctx.project_id, &cfg).await {
        Ok(()) => {
            let nm = cfg.network_mode.as_deref().unwrap_or("none");
            let mem = cfg.memory_mb.unwrap_or(1024);
            let cpus = cfg.cpus.unwrap_or(2.0);
            format!("Configurazione sandbox aggiornata: memoria={}MB, cpu={}, rete={}. Attiva dalla prossima esecuzione.", mem, cpus, nm)
        }
        Err(e) => format!("[Errore salvataggio sandbox config: {}]", e),
    }
}

pub(super) async fn tool_get_sandbox_config(ctx: &AgentToolContext) -> String {
    use crate::sandbox::load_project_sandbox_config;
    let cfg = load_project_sandbox_config(&ctx.db, ctx.project_id).await;
    let nm = cfg.network_mode.as_deref().unwrap_or("none (default)");
    let mem = cfg.memory_mb.map(|m| format!("{MB}", MB = m)).unwrap_or_else(|| "1024 (default)".to_string());
    let cpus = cfg.cpus.map(|c| c.to_string()).unwrap_or_else(|| "2.0 (default)".to_string());
    let env_str = cfg.extra_env.as_ref().map(|e| {
        e.iter().map(|(k, v)| format!("  {k}={v}")).collect::<Vec<_>>().join("\n")
    }).unwrap_or_else(|| "  (nessuna)".to_string());
    format!("Configurazione sandbox progetto:\n- memoria: {mem} MB\n- cpu: {cpus} core\n- rete: {nm}\n- variabili extra:\n{env_str}")
}
