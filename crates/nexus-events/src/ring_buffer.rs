//! Ring buffer per replay degli ultimi N eventi di un progetto.
//!
//! Quando un client SSE si riconnette con header `Last-Event-ID: <seq>`,
//! il dispatcher gli restituisce gli eventi mancanti finche' la finestra
//! e' ancora in buffer. Se il gap e' maggiore del buffer, emette
//! `SnapshotRequired` e il client e' tenuto a rifare bootstrap REST.
//!
//! Implementazione: `VecDeque` con capacita' fissa, push_back/pop_front.
//! Operazioni O(1). Niente reallocazioni.

use std::collections::VecDeque;

use crate::event::EnvelopedEvent;

const DEFAULT_CAPACITY: usize = 512;

#[derive(Debug)]
pub struct RingBuffer {
    inner: VecDeque<EnvelopedEvent>,
    capacity: usize,
}

impl Default for RingBuffer {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }
}

impl RingBuffer {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            inner: VecDeque::with_capacity(cap),
            capacity: cap,
        }
    }

    pub fn push(&mut self, ev: EnvelopedEvent) {
        if self.inner.len() == self.capacity {
            self.inner.pop_front();
        }
        self.inner.push_back(ev);
    }

    /// Restituisce gli eventi con `seq > since`. Ordine cronologico.
    /// Se `since` e' precedente al primo seq disponibile, ritorna `None`
    /// (il chiamante deve emettere `SnapshotRequired`).
    pub fn replay_since(&self, since: u64) -> Option<Vec<EnvelopedEvent>> {
        if let Some(first) = self.inner.front() {
            // Gap troppo grande: il client e' indietro di piu' del buffer.
            if first.seq > since.saturating_add(1) {
                return None;
            }
        }
        Some(
            self.inner
                .iter()
                .filter(|e| e.seq > since)
                .cloned()
                .collect(),
        )
    }

    /// Lo snapshot completo del buffer (per bootstrap iniziale opzionale).
    pub fn all(&self) -> Vec<EnvelopedEvent> {
        self.inner.iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ProjectEvent;
    use uuid::Uuid;

    fn ev(seq: u64) -> EnvelopedEvent {
        EnvelopedEvent::new(
            Uuid::new_v4(),
            seq,
            ProjectEvent::PortReleased { port: seq as i32 },
            None,
        )
    }

    #[test]
    fn ring_drops_oldest_at_capacity() {
        let mut rb = RingBuffer::with_capacity(3);
        for i in 1..=5 {
            rb.push(ev(i));
        }
        assert_eq!(rb.len(), 3);
        assert_eq!(rb.inner.front().unwrap().seq, 3);
        assert_eq!(rb.inner.back().unwrap().seq, 5);
    }

    #[test]
    fn replay_since_returns_only_newer() {
        let mut rb = RingBuffer::with_capacity(10);
        for i in 1..=5 {
            rb.push(ev(i));
        }
        let r = rb.replay_since(2).unwrap();
        assert_eq!(r.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3, 4, 5]);
    }

    #[test]
    fn replay_returns_none_on_gap_larger_than_buffer() {
        let mut rb = RingBuffer::with_capacity(3);
        for i in 100..=102 {
            rb.push(ev(i));
        }
        // Cliente fermo a seq=10, buffer parte da 100 → gap insanabile.
        assert!(rb.replay_since(10).is_none());
    }

    #[test]
    fn replay_returns_empty_when_caught_up() {
        let mut rb = RingBuffer::with_capacity(10);
        for i in 1..=3 {
            rb.push(ev(i));
        }
        assert!(rb.replay_since(3).unwrap().is_empty());
    }
}
