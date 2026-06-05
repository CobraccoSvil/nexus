//! Cache TTL generica e thread-safe (punto unico, regola L / ADR 0026).
//!
//! `TtlCache<K, V>` mantiene valori con scadenza temporale: una entry e'
//! restituita solo se l'eta' e' inferiore al TTL configurato, altrimenti e'
//! trattata come assente (la rimozione lazy avviene alla prossima `insert`).
//!
//! Sostituisce le copie di `TemplateCache` (e simili) che erano duplicate nei
//! singoli crate. La logica di scadenza vive QUI; i call site usano un tipo
//! specializzato che incapsula `TtlCache` (es. `nexus_types::TemplateCache`).

use std::borrow::Borrow;
use std::hash::Hash;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;

/// Cache chiave-valore con TTL uniforme per tutte le entry.
///
/// E' `Clone` a basso costo (condivide lo stesso store via `Arc`), quindi puo'
/// essere riposta in uno stato applicativo clonabile.
pub struct TtlCache<K, V> {
    inner: Arc<DashMap<K, (V, Instant)>>,
    ttl: Duration,
}

// Clone manuale: condivide lo store, NON richiede K/V: Clone (a differenza del derive).
impl<K, V> Clone for TtlCache<K, V> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            ttl: self.ttl,
        }
    }
}

impl<K, V> std::fmt::Debug for TtlCache<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TtlCache")
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl<K, V> TtlCache<K, V>
where
    K: Eq + Hash,
    V: Clone,
{
    /// Crea una cache con il TTL indicato.
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            ttl,
        }
    }

    /// Restituisce il valore se presente e non scaduto, altrimenti `None`.
    pub fn get<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.get(key).and_then(|e| {
            if e.1.elapsed() < self.ttl {
                Some(e.0.clone())
            } else {
                None
            }
        })
    }

    /// Inserisce/aggiorna una entry, marcandola con l'istante corrente.
    pub fn insert(&self, key: K, value: V) {
        self.inner.insert(key, (value, Instant::now()));
    }

    /// Rimuove esplicitamente una entry (usato per invalidazione su update).
    pub fn invalidate<Q>(&self, key: &Q)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.remove(key);
    }

    /// Numero di entry presenti (incluse eventuali scadute non ancora rimosse).
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` se non ci sono entry memorizzate.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_valido_ritorna_some() {
        let c = TtlCache::new(Duration::from_secs(60));
        c.insert("k".to_string(), "v".to_string());
        assert_eq!(c.get("k"), Some("v".to_string()));
    }

    #[test]
    fn ttl_scaduto_ritorna_none() {
        let c: TtlCache<String, String> = TtlCache::new(Duration::from_nanos(1));
        c.insert("k".to_string(), "v".to_string());
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(c.get("k"), None);
    }

    #[test]
    fn invalidate_rimuove_la_entry() {
        let c = TtlCache::new(Duration::from_secs(60));
        c.insert("k".to_string(), "v".to_string());
        c.invalidate("k");
        assert_eq!(c.get("k"), None);
    }

    #[test]
    fn chiave_assente_ritorna_none() {
        let c: TtlCache<String, String> = TtlCache::new(Duration::from_secs(60));
        assert!(c.get("missing").is_none());
    }
}
