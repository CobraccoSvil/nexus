//! Helper riusabile per aggiornare i monitor del pannello Monitor.
//!
//! Centralizza la logica condivisa tra:
//! - il tool agente `dispatcher_update_monitor` (aggiornamento esplicito dal
//!   modello AI);
//! - gli aggiornamenti AUTOMATICI emessi da `chat_messages::agent_run` durante
//!   un run (avvio, ogni tool eseguito, fine) — vedi `auto_monitor`.
//!
//! Cosi' il pannello si popola SENZA dipendere dal fatto che il modello chiami
//! il tool (i modelli non lo fanno in modo affidabile). Regola H: un'unica
//! implementazione, niente duplicazione.

use std::collections::HashMap;
use std::sync::Arc;

use nexus_events::{dispatcher, event::ProjectEvent, ProjectChannels};
use parking_lot::RwLock;
use serde_json::Value;
use uuid::Uuid;

/// Tipo del registro monitor in-memory: `project_id -> { monitor_id -> {value,label,updated_at} }`.
pub(crate) type MonitorRegistry = Arc<RwLock<HashMap<Uuid, HashMap<String, Value>>>>;

/// Aggiorna un monitor nel registry in-memory ed emette `MonitorUpdated` sul
/// dispatcher del progetto. Ritorna il numero di sequenza dell'evento emesso.
///
/// Questa e' la singola fonte di verita' della logica monitor: sia il tool
/// agente sia gli aggiornamenti automatici del run la chiamano.
pub fn set_monitor(
    monitor_registry: &MonitorRegistry,
    project_channels: &ProjectChannels,
    project_id: Uuid,
    monitor_id: &str,
    value: Value,
    label: Option<String>,
) -> u64 {
    {
        let mut reg = monitor_registry.write();
        let project_map = reg.entry(project_id).or_default();
        project_map.insert(
            monitor_id.to_string(),
            serde_json::json!({
                "value": value,
                "label": label,
                "updated_at": chrono::Utc::now().to_rfc3339(),
            }),
        );
    }

    let env = dispatcher::emit(
        project_channels,
        project_id,
        ProjectEvent::MonitorUpdated {
            monitor_id: monitor_id.to_string(),
            value,
            label,
        },
    );
    env.seq
}
