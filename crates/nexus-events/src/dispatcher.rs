//! Dispatcher per-progetto. Mappa `project_id -> ProjectChannel`.
//!
//! Pattern derivato da `crates/mcp-core/src/playwright_live.rs`, generalizzato:
//! - per-project invece di per-job
//! - ring buffer per replay con `Last-Event-ID`
//! - seq monotono per ordering e gap detection
//! - classifier opzionale per arricchire `UiHint`
//!
//! Uso tipico:
//! ```ignore
//! let env = dispatcher::emit(&channels, project_id, ProjectEvent::PortReleased { port: 3000 });
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use dashmap::DashMap;
use parking_lot::Mutex;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::classifier::Classifier;
use crate::event::{EnvelopedEvent, ProjectEvent, UiHint};
use crate::ring_buffer::RingBuffer;

/// Capienza buffer broadcast per consumer lento (oltre questa, `Lagged`).
const BROADCAST_CAPACITY: usize = 256;

/// Stato per-project: broadcast channel, seq counter, ring buffer di replay.
#[derive(Debug)]
pub struct ProjectChannel {
    pub tx: broadcast::Sender<EnvelopedEvent>,
    seq: AtomicU64,
    ring: Mutex<RingBuffer>,
}

impl ProjectChannel {
    fn new() -> Self {
        Self {
            tx: broadcast::channel(BROADCAST_CAPACITY).0,
            seq: AtomicU64::new(0),
            ring: Mutex::new(RingBuffer::default()),
        }
    }

    /// Subscribers attivi (informativo, per cleanup background).
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Seq corrente del canale (ultimo numero di sequenza emesso), senza
    /// incrementare. Punto unico (regola L) per lo snapshot SSE: il client lo usa
    /// come `since` per riagganciare lo stream senza buchi/duplicati.
    pub fn current_seq(&self) -> u64 {
        self.seq.load(std::sync::atomic::Ordering::SeqCst)
    }
}

pub type ProjectChannels = Arc<DashMap<Uuid, Arc<ProjectChannel>>>;

/// Crea un registry vuoto. Tipicamente uno per processo, in `AppState`.
pub fn new_registry() -> ProjectChannels {
    Arc::new(DashMap::new())
}

/// Handle al canale di un progetto. Ritornato da [`register`].
#[derive(Clone)]
pub struct RegistryHandle(pub Arc<ProjectChannel>);

impl RegistryHandle {
    pub fn subscribe(&self) -> broadcast::Receiver<EnvelopedEvent> {
        self.0.tx.subscribe()
    }
}

/// Restituisce (creando se assente) il canale di un progetto.
pub fn register(channels: &ProjectChannels, project_id: Uuid) -> RegistryHandle {
    let ch = channels
        .entry(project_id)
        .or_insert_with(|| Arc::new(ProjectChannel::new()))
        .clone();
    RegistryHandle(ch)
}

/// Emette un evento sul canale del progetto, decorandolo con UiHint via
/// classifier. Crea il canale se non esiste (lazy).
///
/// Restituisce l'`EnvelopedEvent` realmente emesso (con seq assegnato e
/// hint applicato), utile per audit/logging del chiamante.
pub fn emit(
    channels: &ProjectChannels,
    project_id: Uuid,
    event: ProjectEvent,
) -> EnvelopedEvent {
    emit_with_classifier(channels, project_id, event, &Classifier::rules_only())
}

/// Variante che permette di passare un classifier custom (es. con LLM fallback).
pub fn emit_with_classifier(
    channels: &ProjectChannels,
    project_id: Uuid,
    event: ProjectEvent,
    classifier: &Classifier,
) -> EnvelopedEvent {
    let ch = channels
        .entry(project_id)
        .or_insert_with(|| Arc::new(ProjectChannel::new()))
        .clone();

    let seq = ch.seq.fetch_add(1, Ordering::SeqCst) + 1;
    let hint: Option<UiHint> = classifier.classify(&event);
    let env = EnvelopedEvent::new(project_id, seq, event, hint);

    // Salva in ring buffer per replay con Last-Event-ID
    ch.ring.lock().push(env.clone());

    // Send non blocca: se non ci sono subscriber, il messaggio finisce nel
    // buffer interno del channel (max BROADCAST_CAPACITY) o viene scartato
    // dai receiver Lagged. Nessun errore propagato al chiamante.
    let send_result = ch.tx.send(env.clone());
    tracing::info!(
        project_id = %project_id,
        seq = seq,
        kind = env.payload.kind_name(),
        topic = env.topic,
        subscribers = ch.tx.receiver_count(),
        delivered = send_result.is_ok(),
        "dispatcher::emit"
    );

    env
}

/// Replay degli eventi successivi a `last_seq`. Se il gap supera la capacita'
/// del ring buffer, ritorna `None` e il chiamante deve emettere
/// `SnapshotRequired`.
pub fn replay_since(
    channels: &ProjectChannels,
    project_id: Uuid,
    last_seq: u64,
) -> Option<Vec<EnvelopedEvent>> {
    let ch = channels.get(&project_id)?;
    let result = ch.ring.lock().replay_since(last_seq);
    result
}

/// Rimuove dal registry i canali con 0 receiver da almeno `_grace_secs`
/// secondi. Da chiamare in background ogni ~60s. Per ora rimuove
/// immediatamente quelli a 0 receiver: ci pensa il bootstrap a ricreare.
///
/// Ritorna il numero di canali rimossi.
pub fn cleanup_idle(channels: &ProjectChannels) -> usize {
    let mut removed = 0;
    channels.retain(|_, ch| {
        if ch.receiver_count() == 0 {
            removed += 1;
            false
        } else {
            true
        }
    });
    removed
}

// ── Singleton globale per emit da contesti senza &ProjectChannels ────────

static GLOBAL_CHANNELS: OnceLock<ProjectChannels> = OnceLock::new();

/// Inizializza il registry globale. Chiamare una sola volta in `main()`.
/// Chiamate successive sono no-op (il primo vince).
pub fn init_global(channels: ProjectChannels) {
    let _ = GLOBAL_CHANNELS.set(channels);
}

/// Ritorna il registry globale se inizializzato.
pub fn global_channels() -> Option<&'static ProjectChannels> {
    GLOBAL_CHANNELS.get()
}

/// Emette un evento usando il registry globale. No-op silenzioso se non
/// inizializzato (pre-main o in test senza setup). Utile per tool che non
/// hanno accesso diretto a `&ProjectChannels` (es. NexusToolHandler).
pub fn emit_global(project_id: Uuid, event: ProjectEvent) -> Option<EnvelopedEvent> {
    global_channels().map(|ch| emit(ch, project_id, event))
}

/// Broadcast di un evento system-wide a TUTTI i canali progetto attivi.
/// Usato per eventi che non appartengono a un progetto specifico
/// (es. ProviderHealthChanged, SettingChanged).
/// Clona l'evento per ogni canale. No-op se nessun canale registrato.
///
/// IMPORTANTE: raccoglie gli ID prima di emettere per evitare deadlock.
/// `channels.iter()` tiene un read lock sullo shard corrente del DashMap;
/// se dentro l'iter si chiama `emit_with_classifier` che fa `channels.entry()`
/// (write lock sullo stesso shard), il write lock blocca sul read lock
/// detenuto dall'iter -> deadlock del runtime tokio.
pub fn broadcast_all(channels: &ProjectChannels, event: ProjectEvent)
where
    ProjectEvent: Clone,
{
    // Raccogli ID con read lock breve (poi rilasciato).
    let pids: Vec<Uuid> = channels.iter().map(|e| *e.key()).collect();
    let classifier = Classifier::rules_only();
    for pid in pids {
        emit_with_classifier(channels, pid, event.clone(), &classifier);
    }
}

/// Come [`broadcast_all`] ma usa il registry globale.
pub fn broadcast_all_global(event: ProjectEvent)
where
    ProjectEvent: Clone,
{
    if let Some(ch) = global_channels() {
        broadcast_all(ch, event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast::error::TryRecvError;

    fn ev() -> ProjectEvent {
        ProjectEvent::PortReleased { port: 3000 }
    }

    #[tokio::test]
    async fn emit_assigns_monotonic_seq() {
        let reg = new_registry();
        let pid = Uuid::new_v4();
        let e1 = emit(&reg, pid, ev());
        let e2 = emit(&reg, pid, ev());
        let e3 = emit(&reg, pid, ev());
        assert_eq!(e1.seq, 1);
        assert_eq!(e2.seq, 2);
        assert_eq!(e3.seq, 3);
    }

    #[tokio::test]
    async fn subscribers_receive_events() {
        let reg = new_registry();
        let pid = Uuid::new_v4();
        let h = register(&reg, pid);
        let mut rx = h.subscribe();
        emit(&reg, pid, ev());
        let got = rx.recv().await.unwrap();
        assert_eq!(got.seq, 1);
    }

    #[tokio::test]
    async fn replay_works_within_buffer() {
        let reg = new_registry();
        let pid = Uuid::new_v4();
        for _ in 0..5 {
            emit(&reg, pid, ev());
        }
        let r = replay_since(&reg, pid, 2).unwrap();
        assert_eq!(r.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3, 4, 5]);
    }

    #[tokio::test]
    async fn no_subscribers_do_not_error() {
        let reg = new_registry();
        let pid = Uuid::new_v4();
        // Emit senza subscriber non causa errori
        let env = emit(&reg, pid, ev());
        assert_eq!(env.seq, 1);
        // Ora un subscriber non vede l'evento gia' emesso (broadcast non e' replay)
        let h = register(&reg, pid);
        let mut rx = h.subscribe();
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[tokio::test]
    async fn cleanup_removes_idle_channels() {
        let reg = new_registry();
        let pid = Uuid::new_v4();
        register(&reg, pid); // handle scope drops -> no subscribers
        // Subito drop dell'handle non chiude i receiver perche' RegistryHandle
        // tiene un Arc al ProjectChannel ma il broadcast::Sender ha receiver=0.
        // (Il sender stesso non e' un receiver.)
        let removed = cleanup_idle(&reg);
        assert_eq!(removed, 1);
        assert!(reg.is_empty());
    }
}
