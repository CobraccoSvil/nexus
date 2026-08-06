//! ReplicationWorker — sincronizza namespace entries su storage persistente
//!
//! Worker periodico che prepara un batch di entry dal namespace per la
//! replica su PostgreSQL. La logica di scrittura vera e propria avviene
//! tramite il router (che ha accesso al pool) o viene emessa come payload
//! serializzato nel namespace sotto la chiave `replication:pending`.
//!
//! ## Strategia di replicazione
//!
//! Il worker opera in due modalità:
//!
//! 1. **Con router**: usa `router.persist_namespace_batch()` se disponibile
//!    (fire-and-forget asincrono)
//! 2. **Senza router**: serializza le entry sotto `replication:pending`
//!    per un consumer esterno (es. un servizio dedicato)
//!
//! Le entry `session:*`, `metrics:*`, `version:*` e `pattern:*` vengono
//! replicate in priorità.
//!
//! ## Performance
//!
//! Il worker non blocca — la serializzazione è sincrona ma leggera.
//! La scrittura su DB (se presente) è asincrona (tokio::spawn).

use crate::learning_loop::{LearningContext, LearningWorker, WorkerOutcome, WorkerTrigger};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Prefissi di chiave prioritari da replicare
const PRIORITY_PREFIXES: &[&str] = &["session:", "metrics:", "version:", "pattern:"];

/// Prefisso delle chiavi di batch in attesa di replica. PUNTO UNICO (regola L)
/// condiviso con il consumatore (`mcp-core::nexus_bridge::flush_replication_pending`):
/// produttore e consumatore devono concordare sul nome, e concordare per
/// costruzione e' l'unico modo di non divergere.
///
/// Ogni batch ha la SUA chiave (`replication:pending:<istante>-<seq>`). Prima
/// esisteva una chiave sola, `replication:pending`, riscritta a ogni giro: il
/// worker prepara un batch ogni 180s mentre il consumatore svuota su tutt'altra
/// cadenza, quindi ogni batch non ancora consumato veniva sovrascritto e perso —
/// in silenzio, per giunta, perche' il worker dichiarava comunque successo.
pub const REPLICATION_PENDING_PREFIX: &str = "replication:pending:";

/// Chiave singola usata prima delle chiavi per-batch. Il consumatore la legge
/// ancora per non perdere un batch preparato dalla versione precedente e rimasto
/// in memoria durante l'aggiornamento.
pub const REPLICATION_PENDING_LEGACY_KEY: &str = "replication:pending";

/// Questa chiave e' un batch accodato da questo stesso worker?
///
/// I batch sono un artefatto di TRASPORTO — il consumatore
/// (`nexus_bridge::flush_replication_pending`) li legge e li rimuove — non un
/// dato di dominio, quindi non rientrano mai in un batch successivo. Il
/// discriminante sta qui e non fra i [`PRIORITY_PREFIXES`]: quella lista
/// risponde a un'altra domanda ("cosa replicare per primo"), e una chiave che
/// non vi figura finisce comunque nel ramo delle rimanenti.
fn is_replication_artifact(key: &str) -> bool {
    key.starts_with(REPLICATION_PENDING_PREFIX) || key == REPLICATION_PENDING_LEGACY_KEY
}

/// Batch serializzato da replicare
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplicationBatch {
    pub namespace_id: String,
    pub prepared_at: String,
    pub entry_count: usize,
    pub entries: Vec<ReplicationEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplicationEntry {
    pub key: String,
    pub value: serde_json::Value,
    pub author: String,
}

pub struct ReplicationWorker {
    interval: Duration,
    /// Max entry per batch (evita batch troppo grandi)
    max_batch_size: usize,
    /// Discriminante dei batch preparati nello stesso istante: `prepared_at` ha
    /// risoluzione al secondo, e due batch nello stesso secondo si
    /// sovrascriverebbero a vicenda proprio come faceva la chiave unica.
    seq: AtomicU64,
}

impl Default for ReplicationWorker {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(180), // ogni 3 minuti
            max_batch_size: 100,
            seq: AtomicU64::new(0),
        }
    }
}

impl ReplicationWorker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    pub fn with_max_batch(mut self, max: usize) -> Self {
        self.max_batch_size = max;
        self
    }
}

#[async_trait]
impl LearningWorker for ReplicationWorker {
    fn name(&self) -> &str {
        "replication"
    }

    fn trigger(&self) -> WorkerTrigger {
        WorkerTrigger::Periodic
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    async fn run(&self, context: &LearningContext) -> WorkerOutcome {
        let start = Instant::now();

        let ns = match &context.namespace {
            Some(ns) => ns,
            None => {
                return WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
                    .with_metric("entries_queued", 0.0);
            }
        };

        // Seleziona le chiavi da replicare (priorità: prefissi noti, poi resto).
        // I batch gia' accodati sono ESCLUSI: `replication:pending:` non figura
        // fra i PRIORITY_PREFIXES e cadeva nel ramo delle chiavi rimanenti, cosi'
        // ogni giro inglobava i batch dei giri precedenti — che a loro volta
        // contenevano i loro. Con intervallo 180s e TTL 600s ne convivono 3-4
        // generazioni, ognuna copia delle altre: la crescita e' geometrica, e
        // `max_batch_size` non la ferma perche' limita il NUMERO di entry, non la
        // loro dimensione. Misurato il 06/08/2026 su mcp-core vivo da 2,7h: 21,9
        // GB di picco, un core al 100% dentro `serde_json::value::clone` (ogni
        // `ns.get` clona l'intero Value, `namespace.rs`) e 176.342 regioni
        // private frammentate.
        let all_keys: Vec<String> = ns
            .keys()
            .into_iter()
            .filter(|k| !is_replication_artifact(k))
            .collect();
        let mut priority_keys: Vec<String> = all_keys
            .iter()
            .filter(|k| PRIORITY_PREFIXES.iter().any(|p| k.starts_with(p)))
            .take(self.max_batch_size)
            .cloned()
            .collect();

        // Se c'è spazio, aggiunge le chiavi rimanenti
        if priority_keys.len() < self.max_batch_size {
            let remaining = all_keys
                .iter()
                .filter(|k| !PRIORITY_PREFIXES.iter().any(|p| k.starts_with(p)))
                .take(self.max_batch_size - priority_keys.len())
                .cloned();
            priority_keys.extend(remaining);
        }

        if priority_keys.is_empty() {
            return WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
                .with_metric("entries_queued", 0.0);
        }

        // Costruisce il batch
        let mut entries = Vec::with_capacity(priority_keys.len());
        for key in &priority_keys {
            if let Some(entry) = ns.get(key) {
                entries.push(ReplicationEntry {
                    key: key.clone(),
                    value: entry.value,
                    author: entry.author,
                });
            }
        }

        let entry_count = entries.len();
        let batch = ReplicationBatch {
            namespace_id: ns.name().to_string(),
            prepared_at: chrono::Utc::now().to_rfc3339(),
            entry_count,
            entries,
        };

        // Il batch viene ACCODATO nel namespace sotto una chiave propria; a
        // scriverlo su PostgreSQL e' il consumatore
        // (`nexus_bridge::flush_replication_pending`), che legge tutte le chiavi
        // col prefisso e le rimuove una per una.
        let value = match serde_json::to_value(&batch) {
            Ok(v) => v,
            Err(e) => {
                // Serializzazione fallita: nessun batch accodato. Prima un
                // `unwrap_or(Value::Null)` scriveva `null` sotto la chiave e il
                // worker dichiarava ugualmente le entry come replicate; il
                // consumatore trovava un valore non deserializzabile e lo
                // scartava. Le entry sparivano senza che nulla lo dicesse.
                return WorkerOutcome::fail(
                    self.name(),
                    format!("serializzazione del batch fallita: {e}"),
                    start.elapsed().as_millis() as u64,
                )
                .with_metric("entries_queued", 0.0);
            }
        };
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let key = format!(
            "{REPLICATION_PENDING_PREFIX}{}-{seq}",
            batch.prepared_at
        );
        ns.set_with_ttl(
            &key,
            value,
            self.name(),
            Duration::from_secs(600), // TTL 10 minuti — deve essere consumato entro allora
        );

        // `entries_queued`, non `entries_replicated`: questo worker accoda, non
        // replica. La metrica diceva "replicate" su entry che potevano non
        // arrivare mai a destinazione (regola M: l'esito deve riflettere il
        // fatto, non l'intenzione).
        WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
            .with_metric("entries_queued", entry_count as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::MemoryNamespace;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_replication_creates_pending_batch() {
        let ns = Arc::new(MemoryNamespace::new("rep-test"));
        ns.set("pattern:p1", serde_json::json!({"a": 1}), "ultralearn");
        ns.set("metrics:latest", serde_json::json!({"b": 2}), "metrics");
        ns.set("other:key", serde_json::json!({"c": 3}), "agent");

        let worker = ReplicationWorker::new();
        let ctx = LearningContext::new().with_namespace(ns.clone());
        let outcome = worker.run(&ctx).await;

        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("entries_queued"), Some(&3.0));

        let key = pending_keys(&ns)
            .pop()
            .expect("un batch accodato deve esistere");
        let pending = ns.get(&key).expect("pending batch must exist");
        let batch: ReplicationBatch = serde_json::from_value(pending.value).unwrap();
        assert_eq!(batch.entry_count, 3);
        // pattern: e metrics: devono essere prima degli altri
        let first_keys: Vec<&str> = batch.entries.iter().map(|e| e.key.as_str()).collect();
        assert!(first_keys.contains(&"pattern:p1"));
        assert!(first_keys.contains(&"metrics:latest"));
    }

    /// Chiavi dei batch accodati, ordinate.
    fn pending_keys(ns: &MemoryNamespace) -> Vec<String> {
        let mut k: Vec<String> = ns
            .keys()
            .into_iter()
            .filter(|k| k.starts_with(REPLICATION_PENDING_PREFIX))
            .collect();
        k.sort();
        k
    }

    #[tokio::test]
    async fn un_batch_accodato_non_rientra_in_quelli_successivi() {
        // IL difetto: `replication:pending:` non era fra i PRIORITY_PREFIXES,
        // quindi cadeva nel ramo delle chiavi rimanenti e ogni giro inglobava i
        // batch precedenti, che contenevano i loro. Con intervallo 180s e TTL
        // 600s ne convivono 3-4 generazioni, ognuna copia delle altre: crescita
        // geometrica. Misurato il 06/08/2026 su mcp-core, 21,9 GB di picco con un
        // core al 100% in clonazione ricorsiva di `serde_json::Value`.
        let ns = Arc::new(MemoryNamespace::new("auto-inclusione"));
        let worker = ReplicationWorker::new();
        let ctx = LearningContext::new().with_namespace(ns.clone());

        ns.set("pattern:dato", serde_json::json!({"giro": 1}), "ul");
        assert!(worker.run(&ctx).await.success);
        assert!(worker.run(&ctx).await.success);

        let keys = pending_keys(&ns);
        assert_eq!(keys.len(), 2, "due giri, due batch: {keys:?}");

        for k in &keys {
            let batch: ReplicationBatch =
                serde_json::from_value(ns.get(k).unwrap().value).unwrap();
            let inglobati: Vec<&str> = batch
                .entries
                .iter()
                .map(|e| e.key.as_str())
                .filter(|k| is_replication_artifact(k))
                .collect();
            assert!(
                inglobati.is_empty(),
                "il batch {k} non deve contenere batch precedenti: {inglobati:?}"
            );
            assert_eq!(
                batch.entry_count,
                1,
                "solo il dato vero in {k}: {:?}",
                batch.entries.iter().map(|e| &e.key).collect::<Vec<_>>()
            );
        }
    }

    #[tokio::test]
    async fn due_giri_accodano_due_batch_distinti() {
        // IL difetto: la chiave era una sola (`replication:pending`) e ogni giro
        // sovrascriveva il batch precedente. Il worker prepara un batch ogni
        // 180s mentre il consumatore svuota su un'altra cadenza, quindi ogni
        // batch non ancora consumato spariva — senza che nulla lo segnalasse,
        // visto che il worker dichiarava comunque successo.
        let ns = Arc::new(MemoryNamespace::new("due-giri"));
        let worker = ReplicationWorker::new();
        let ctx = LearningContext::new().with_namespace(ns.clone());

        ns.set("pattern:primo", serde_json::json!({"giro": 1}), "ul");
        assert!(worker.run(&ctx).await.success);

        ns.set("pattern:secondo", serde_json::json!({"giro": 2}), "ul");
        assert!(worker.run(&ctx).await.success);

        let keys = pending_keys(&ns);
        assert_eq!(
            keys.len(),
            2,
            "ogni giro deve accodare il suo batch, non sovrascrivere: {keys:?}"
        );

        // Il primo batch e' ancora integro e contiene quello che aveva allora.
        let primo: ReplicationBatch =
            serde_json::from_value(ns.get(&keys[0]).unwrap().value).unwrap();
        assert!(
            primo.entries.iter().any(|e| e.key == "pattern:primo"),
            "il batch del primo giro non deve essere stato perso"
        );
    }

    #[tokio::test]
    async fn test_replication_no_namespace() {
        let worker = ReplicationWorker::new();
        let ctx = LearningContext::new();
        let outcome = worker.run(&ctx).await;
        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("entries_queued"), Some(&0.0));
    }

    #[tokio::test]
    async fn test_replication_respects_batch_limit() {
        let ns = Arc::new(MemoryNamespace::new("limit-test"));
        for i in 0..10 {
            ns.set(
                format!("pattern:{i}"),
                serde_json::json!({"i": i}),
                "ul",
            );
        }

        let worker = ReplicationWorker::new().with_max_batch(3);
        let ctx = LearningContext::new().with_namespace(ns.clone());
        let outcome = worker.run(&ctx).await;

        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("entries_queued"), Some(&3.0));
    }
}
