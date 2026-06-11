//! Memory Namespace — shared context tra agenti in uno swarm
//!
//! Fornisce uno storage key-value thread-safe isolato per namespace.
//! Gli agenti in uno swarm condividono lo stesso namespace per
//! coordinarsi (pubblicare risultati intermedi, leggere stato altrui,
//! fare broadcast di eventi).
//!
//! Design:
//! - DashMap per concurrent access senza global lock
//! - Isolation per namespace (diversi swarm = namespace diversi)
//! - TTL opzionale per auto-eviction
//! - Event channel per notifiche (tokio broadcast)

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// Entry in un namespace con metadata
#[derive(Clone, Debug)]
pub struct NamespaceEntry {
    pub key: String,
    pub value: serde_json::Value,
    pub author: String, // agent name/id che ha scritto
    pub created_at: Instant,
    pub ttl: Option<Duration>,
}

impl NamespaceEntry {
    pub fn is_expired(&self) -> bool {
        match self.ttl {
            Some(ttl) => self.created_at.elapsed() > ttl,
            None => false,
        }
    }
}

/// Evento di namespace per notifiche pub/sub
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NamespaceEvent {
    /// Nuova entry scritta
    Written {
        namespace: String,
        key: String,
        author: String,
    },
    /// Entry rimossa
    Removed { namespace: String, key: String },
    /// Task completato da un agente (risultato pubblicato)
    TaskCompleted {
        namespace: String,
        task_id: String,
        agent: String,
        success: bool,
    },
    /// Evento custom
    Custom {
        namespace: String,
        event_type: String,
        payload: serde_json::Value,
    },
}

/// Memory namespace — storage condiviso con event broadcasting
pub struct MemoryNamespace {
    /// Nome del namespace (per isolation)
    name: String,
    /// Key-value store
    store: Arc<DashMap<String, NamespaceEntry>>,
    /// Event broadcaster per pub/sub
    event_tx: broadcast::Sender<NamespaceEvent>,
}

impl MemoryNamespace {
    /// Crea un nuovo namespace con nome dato e buffer eventi
    pub fn new(name: impl Into<String>) -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            name: name.into(),
            store: Arc::new(DashMap::new()),
            event_tx: tx,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Scrive una entry nel namespace
    pub fn set(
        &self,
        key: impl Into<String>,
        value: serde_json::Value,
        author: impl Into<String>,
    ) {
        let key = key.into();
        let author = author.into();
        let entry = NamespaceEntry {
            key: key.clone(),
            value,
            author: author.clone(),
            created_at: Instant::now(),
            ttl: None,
        };
        self.store.insert(key.clone(), entry);

        // Publish event (non-blocking, ignora se no subscribers)
        let _ = self.event_tx.send(NamespaceEvent::Written {
            namespace: self.name.clone(),
            key,
            author,
        });
    }

    /// Scrive con TTL
    pub fn set_with_ttl(
        &self,
        key: impl Into<String>,
        value: serde_json::Value,
        author: impl Into<String>,
        ttl: Duration,
    ) {
        let key = key.into();
        let author = author.into();
        let entry = NamespaceEntry {
            key: key.clone(),
            value,
            author: author.clone(),
            created_at: Instant::now(),
            ttl: Some(ttl),
        };
        self.store.insert(key.clone(), entry);

        let _ = self.event_tx.send(NamespaceEvent::Written {
            namespace: self.name.clone(),
            key,
            author,
        });
    }

    /// Legge una entry. Ritorna None se assente o scaduta (e la rimuove se scaduta).
    pub fn get(&self, key: &str) -> Option<NamespaceEntry> {
        let entry = self.store.get(key)?.clone();
        if entry.is_expired() {
            self.store.remove(key);
            return None;
        }
        Some(entry)
    }

    /// Rimuove una entry
    pub fn remove(&self, key: &str) -> Option<NamespaceEntry> {
        let removed = self.store.remove(key).map(|(_, v)| v)?;
        let _ = self.event_tx.send(NamespaceEvent::Removed {
            namespace: self.name.clone(),
            key: key.to_string(),
        });
        Some(removed)
    }

    /// Pubblica risultato task (helper per SwarmCoordinator)
    pub fn publish_task_result(
        &self,
        task_id: impl Into<String>,
        agent: impl Into<String>,
        success: bool,
        result: serde_json::Value,
    ) {
        let task_id = task_id.into();
        let agent = agent.into();
        let key = format!("task_result:{}", task_id);
        self.set(&key, result, &agent);

        let _ = self.event_tx.send(NamespaceEvent::TaskCompleted {
            namespace: self.name.clone(),
            task_id,
            agent,
            success,
        });
    }

    /// Broadcast di un evento custom
    pub fn publish_custom(
        &self,
        event_type: impl Into<String>,
        payload: serde_json::Value,
    ) {
        let _ = self.event_tx.send(NamespaceEvent::Custom {
            namespace: self.name.clone(),
            event_type: event_type.into(),
            payload,
        });
    }

    /// Sottoscrive al feed di eventi
    pub fn subscribe(&self) -> broadcast::Receiver<NamespaceEvent> {
        self.event_tx.subscribe()
    }

    /// Numero di entry (non filtra scadute)
    pub fn len(&self) -> usize {
        self.store.len()
    }

    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// Rimuove tutte le entry scadute
    pub fn evict_expired(&self) -> usize {
        let keys_to_remove: Vec<String> = self
            .store
            .iter()
            .filter(|e| e.is_expired())
            .map(|e| e.key().clone())
            .collect();

        let count = keys_to_remove.len();
        for k in keys_to_remove {
            self.store.remove(&k);
        }
        count
    }

    /// Elenco delle chiavi attualmente memorizzate
    pub fn keys(&self) -> Vec<String> {
        self.store.iter().map(|e| e.key().clone()).collect()
    }

    /// Recupera tutti i risultati di task presenti nel namespace
    pub fn all_task_results(&self) -> Vec<(String, NamespaceEntry)> {
        self.store
            .iter()
            .filter_map(|e| {
                let key = e.key();
                key.strip_prefix("task_result:").map(|task_id| (task_id.to_string(), e.value().clone()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_namespace_set_get() {
        let ns = MemoryNamespace::new("test");
        ns.set("key1", serde_json::json!({"v": 1}), "agent1");
        let entry = ns.get("key1").unwrap();
        assert_eq!(entry.author, "agent1");
        assert_eq!(entry.value["v"], 1);
    }

    #[test]
    fn test_namespace_remove() {
        let ns = MemoryNamespace::new("test");
        ns.set("k", serde_json::json!(42), "a");
        assert!(ns.get("k").is_some());
        ns.remove("k");
        assert!(ns.get("k").is_none());
    }

    #[test]
    fn test_ttl_expiration() {
        let ns = MemoryNamespace::new("test");
        ns.set_with_ttl(
            "k",
            serde_json::json!("value"),
            "a",
            Duration::from_millis(50),
        );
        assert!(ns.get("k").is_some());
        std::thread::sleep(Duration::from_millis(80));
        assert!(ns.get("k").is_none()); // scaduto
    }

    #[test]
    fn test_evict_expired() {
        let ns = MemoryNamespace::new("test");
        ns.set_with_ttl("k1", serde_json::json!(1), "a", Duration::from_millis(10));
        ns.set("k2", serde_json::json!(2), "a");
        std::thread::sleep(Duration::from_millis(30));
        let evicted = ns.evict_expired();
        assert_eq!(evicted, 1);
        assert!(ns.get("k2").is_some());
    }

    #[tokio::test]
    async fn test_event_subscription() {
        let ns = MemoryNamespace::new("test");
        let mut rx = ns.subscribe();

        ns.set("k", serde_json::json!("v"), "a");

        let event = rx.recv().await.unwrap();
        match event {
            NamespaceEvent::Written { key, author, .. } => {
                assert_eq!(key, "k");
                assert_eq!(author, "a");
            }
            _ => panic!("Unexpected event"),
        }
    }

    #[tokio::test]
    async fn test_task_result_publishing() {
        let ns = MemoryNamespace::new("swarm1");
        let mut rx = ns.subscribe();

        ns.publish_task_result("task1", "coder", true, serde_json::json!({"output": "done"}));

        let event = rx.recv().await.unwrap();
        match event {
            NamespaceEvent::Written { .. } => {} // first event is Write
            _ => panic!("Expected Written event first"),
        }

        let event = rx.recv().await.unwrap();
        match event {
            NamespaceEvent::TaskCompleted {
                task_id, success, ..
            } => {
                assert_eq!(task_id, "task1");
                assert!(success);
            }
            _ => panic!("Expected TaskCompleted event"),
        }

        let results = ns.all_task_results();
        assert_eq!(results.len(), 1);
    }
}
