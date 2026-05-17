//! Background task che arricchisce eventi con metadati AI in modo asincrono.
//!
//! Architettura:
//! - Quando `emit_and_tap()` viene chiamato, l'evento parte SUBITO sul broadcast
//!   (hot path zero-latenza per i pannelli).
//! - Una copia dell'`EnvelopedEvent` viene inviata via `mpsc` al canale
//!   dell'enricher. Se il canale e' pieno (consumer lento), l'evento viene
//!   silenziosamente droppato: il sistema preferisce perdere arricchimenti
//!   piuttosto che rallentare l'hot path.
//! - L'enricher loop consuma da mpsc, prova il classifier hardcoded prima,
//!   poi (per Custom o eventi senza hint dal classifier) chiama l'LLM con
//!   timeout 800ms. Se trova un delta utile, emette un nuovo
//!   `ProjectEvent::EventEnriched` referenziando `event_id` originale.
//! - Il frontend memorizza eventi per `event_id` e fa merge dei delta
//!   `EventEnriched` quando arrivano (idempotente, ordine tollerato).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::timeout;
use uuid::Uuid;

use crate::classifier::Classifier;
use crate::dispatcher::{emit, ProjectChannels};
use crate::event::{EnvelopedEvent, ProjectEvent};
use crate::llm_classifier::{EnrichmentDelta, LlmEnricher};

/// Capacita' del canale mpsc tra `emit_and_tap` e `enricher_loop`.
/// Se il loop e' indietro (es. LLM lento), gli eventi nuovi vengono droppati
/// silenziosamente. Bilancia memoria vs perdita di arricchimenti.
pub const ENRICHER_CHANNEL_CAPACITY: usize = 1024;

/// Timeout massimo per una chiamata LLM. Oltre, si rinuncia all'arricchimento.
pub const LLM_TIMEOUT_MS: u64 = 800;

/// Crea un canale per il tap degli eventi e ritorna sender + receiver.
/// Il sender va clonato e passato a `emit_and_tap()`; il receiver al
/// `enricher_loop`.
pub fn channel() -> (mpsc::Sender<EnvelopedEvent>, mpsc::Receiver<EnvelopedEvent>) {
    mpsc::channel(ENRICHER_CHANNEL_CAPACITY)
}

/// Variante di `dispatcher::emit` che fa anche tap dell'evento sul canale
/// dell'enricher (non bloccante: drop se canale pieno).
///
/// Da usare al posto di `dispatcher::emit` direttamente quando si vuole che
/// l'enricher veda l'evento e possa arricchirlo in background.
pub fn emit_and_tap(
    channels: &ProjectChannels,
    enricher_tx: &mpsc::Sender<EnvelopedEvent>,
    project_id: Uuid,
    event: ProjectEvent,
) -> EnvelopedEvent {
    let env = emit(channels, project_id, event);
    // try_send: non bloccante, drop se buffer pieno
    if let Err(e) = enricher_tx.try_send(env.clone()) {
        tracing::debug!(
            err = ?e,
            "enricher channel full, dropping event from enrichment (hot path preserved)"
        );
    }
    env
}

/// Loop di arricchimento. Va spawned come task tokio in startup.
///
/// Per ogni evento ricevuto:
/// 1. Skip se gia' un `EventEnriched` (evita loop infinito)
/// 2. Prova il classifier hardcoded — se ha gia' dato hint nell'envelope
///    originale, skip (frontend lo ha gia' ricevuto)
/// 3. Altrimenti, prova LLM con timeout. Se ritorna delta non vuoto,
///    emette `ProjectEvent::EventEnriched` referenziando event_id originale.
pub async fn enricher_loop<E: LlmEnricher + 'static>(
    channels: ProjectChannels,
    mut rx: mpsc::Receiver<EnvelopedEvent>,
    llm: Arc<E>,
) {
    tracing::info!("enricher_loop avviato");
    let rules = Classifier::rules_only();

    while let Some(env) = rx.recv().await {
        // Skip eventi gia' arricchiti per evitare loop
        if matches!(env.payload, ProjectEvent::EventEnriched { .. }) {
            continue;
        }
        // Skip eventi che il classifier hardcoded ha gia' arricchito
        if env.ui_hint.is_some() {
            continue;
        }
        // Re-prova regole (per allineamento, in caso emit sia avvenuto senza
        // classifier — improbabile ma cheap)
        if rules.classify(&env.payload).is_some() {
            continue;
        }

        // Chiama LLM con timeout
        let llm = llm.clone();
        let payload_clone = env.payload.clone();
        let llm_result = timeout(
            Duration::from_millis(LLM_TIMEOUT_MS),
            async move { llm.classify(&payload_clone).await },
        )
        .await;

        match llm_result {
            Ok(Some(delta)) if !delta.is_empty() => {
                emit_enriched(&channels, env.project_id, env.event_id, delta);
            }
            Ok(_) => {
                // LLM ha risposto ma senza hint: silenzioso
            }
            Err(_) => {
                tracing::warn!(
                    event_id = %env.event_id,
                    kind = env.payload.kind_name(),
                    "LLM enricher timeout ({}ms)", LLM_TIMEOUT_MS
                );
            }
        }
    }

    tracing::info!("enricher_loop terminato (canale chiuso)");
}

/// Emette `ProjectEvent::EventEnriched` come re-emit asincrono. Il frontend
/// fa merge per `event_id`.
pub fn emit_enriched(
    channels: &ProjectChannels,
    project_id: Uuid,
    event_id: Uuid,
    delta: EnrichmentDelta,
) {
    let _ = emit(
        channels,
        project_id,
        ProjectEvent::EventEnriched {
            event_id,
            ui_hint: delta.ui_hint,
            semantic_tags: delta.semantic_tags,
            severity_inferred: delta.severity_inferred,
            panel_target: delta.panel_target,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::{new_registry, register};
    use crate::llm_classifier::NoOpEnricher;
    use async_trait::async_trait;
    use tokio::sync::broadcast::error::TryRecvError;

    #[tokio::test]
    async fn emit_and_tap_pushes_to_both_broadcast_and_enricher_channel() {
        let registry = new_registry();
        let (tx, mut rx) = channel();
        let pid = Uuid::new_v4();
        let handle = register(&registry, pid);
        let mut sub = handle.subscribe();

        let env = emit_and_tap(
            &registry,
            &tx,
            pid,
            ProjectEvent::PortReleased { port: 3000 },
        );

        // Broadcast riceve
        let from_broadcast = sub.recv().await.unwrap();
        assert_eq!(from_broadcast.seq, env.seq);

        // Enricher channel riceve
        let from_enricher = rx.recv().await.unwrap();
        assert_eq!(from_enricher.seq, env.seq);
    }

    #[tokio::test]
    async fn emit_and_tap_does_not_block_when_channel_full() {
        let registry = new_registry();
        // Canale microscopico (capacita' 1) per simulare canale pieno
        let (tx, _rx) = mpsc::channel::<EnvelopedEvent>(1);
        let pid = Uuid::new_v4();

        // Riempie il canale
        emit_and_tap(&registry, &tx, pid, ProjectEvent::PortReleased { port: 1 });
        // Questa chiamata deve droppare silenziosamente senza panic/error
        let env = emit_and_tap(&registry, &tx, pid, ProjectEvent::PortReleased { port: 2 });
        assert_eq!(env.seq, 2); // l'emit principale funziona comunque
    }

    #[tokio::test]
    async fn enricher_loop_skips_already_classified_events() {
        let registry = new_registry();
        let (tx, rx) = channel();
        let llm = Arc::new(NoOpEnricher);

        let pid = Uuid::new_v4();
        let handle = register(&registry, pid);
        let mut sub = handle.subscribe();

        // Spawn loop in background
        let registry_clone = registry.clone();
        let llm_clone = llm.clone();
        let task = tokio::spawn(async move {
            enricher_loop(registry_clone, rx, llm_clone).await;
        });

        // Emette un PortReleased: il classifier hardcoded gia' produce hint,
        // quindi l'enricher deve skipparlo (no EventEnriched emesso).
        emit_and_tap(
            &registry,
            &tx,
            pid,
            ProjectEvent::PortReleased { port: 3000 },
        );

        // Consuma l'evento originale dal broadcast
        let original = sub.recv().await.unwrap();
        assert!(matches!(original.payload, ProjectEvent::PortReleased { .. }));
        assert!(original.ui_hint.is_some(), "classifier hardcoded dovrebbe arricchire");

        // Aspetta un attimo che l'enricher loop processi
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Non deve esserci EventEnriched
        match sub.try_recv() {
            Err(TryRecvError::Empty) => { /* atteso */ }
            other => panic!("Expected empty, got {:?}", other),
        }

        // Cleanup
        drop(tx);
        let _ = tokio::time::timeout(Duration::from_millis(100), task).await;
    }

    struct AlwaysEnricher;
    #[async_trait]
    impl LlmEnricher for AlwaysEnricher {
        async fn classify(&self, _event: &ProjectEvent) -> Option<EnrichmentDelta> {
            Some(EnrichmentDelta {
                semantic_tags: vec!["test".into()],
                ..Default::default()
            })
        }
    }

    #[tokio::test]
    async fn enricher_loop_emits_enriched_for_custom_events() {
        let registry = new_registry();
        let (tx, rx) = channel();
        let llm = Arc::new(AlwaysEnricher);

        let pid = Uuid::new_v4();
        let handle = register(&registry, pid);
        let mut sub = handle.subscribe();

        let registry_clone = registry.clone();
        let llm_clone = llm.clone();
        let task = tokio::spawn(async move {
            enricher_loop(registry_clone, rx, llm_clone).await;
        });

        // Custom event: classifier hardcoded NON da hint, enricher deve emettere
        let original = emit_and_tap(
            &registry,
            &tx,
            pid,
            ProjectEvent::Custom {
                event_name: "test_event".into(),
                resource: "x".into(),
                payload: serde_json::Value::Null,
            },
        );

        // Consuma l'originale
        let from_broadcast = sub.recv().await.unwrap();
        assert_eq!(from_broadcast.seq, original.seq);

        // L'enricher dovrebbe emettere un EventEnriched a breve
        let enriched = tokio::time::timeout(Duration::from_millis(500), sub.recv())
            .await
            .expect("enricher non ha emesso EventEnriched in tempo")
            .unwrap();

        match enriched.payload {
            ProjectEvent::EventEnriched {
                event_id,
                semantic_tags,
                ..
            } => {
                assert_eq!(event_id, original.event_id);
                assert_eq!(semantic_tags, vec!["test".to_string()]);
            }
            other => panic!("Expected EventEnriched, got {:?}", other),
        }

        drop(tx);
        let _ = tokio::time::timeout(Duration::from_millis(100), task).await;
    }
}
