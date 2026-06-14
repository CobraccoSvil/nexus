//! Mappa di reidratazione placeholder -> valore originale (per-request).
//!
//! Porting di `packages/llm-gateway/src/redaction/redaction-map.ts`.
//!
//! La mappa vive nel ciclo di vita di una SINGOLA richiesta: i valori sensibili
//! sostituiti con placeholder in pre-flight vengono ripristinati in post-flight
//! sulla risposta del provider. TTL breve (default 5 min) come safety net.
//!
//! Regola F: questa struttura CONTIENE i valori originali (segreti/PII) per
//! poterli reidratare. Non deve MAI essere loggata, ne' i suoi valori esposti.
//! `audit_snapshot()` ritorna solo placeholder+tipo, mai l'originale.

use std::time::{Duration, Instant};

/// TTL di default della mappa (5 minuti), come il TS (`300_000` ms).
const DEFAULT_TTL: Duration = Duration::from_secs(300);

/// Voce della mappa: placeholder, valore originale, tipo e istante di creazione.
#[derive(Debug, Clone)]
struct RedactionEntry {
    original: String,
    kind: String,
    created_at: Instant,
}

/// Mappa di reidratazione per una request. Ordine di inserimento preservato
/// (vettore) per riprodurre il comportamento deterministico del TS.
#[derive(Debug)]
pub struct RedactionMap {
    request_id: String,
    ttl: Duration,
    // (placeholder -> entry). Vec per ordine stabile; il numero di entry per
    // request e' piccolo, la ricerca lineare e' adeguata.
    entries: Vec<(String, RedactionEntry)>,
    counter: usize,
}

impl RedactionMap {
    /// Crea una mappa con il TTL di default.
    pub fn new(request_id: impl Into<String>) -> Self {
        Self::with_ttl(request_id, DEFAULT_TTL)
    }

    /// Crea una mappa con un TTL esplicito.
    pub fn with_ttl(request_id: impl Into<String>, ttl: Duration) -> Self {
        Self {
            request_id: request_id.into(),
            ttl,
            entries: Vec::new(),
            counter: 0,
        }
    }

    /// Identificativo della request associata (per audit).
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Registra un valore originale e ritorna il placeholder da iniettare nel
    /// testo. Deduplica: lo stesso (valore, tipo) ritorna lo stesso placeholder.
    pub fn store(&mut self, original: &str, kind: &str) -> String {
        for (placeholder, entry) in &self.entries {
            if entry.original == original && entry.kind == kind {
                return placeholder.clone();
            }
        }

        self.counter += 1;
        let placeholder = format!("__NEXUS_{}_{}__", kind.to_ascii_uppercase(), self.counter);
        self.entries.push((
            placeholder.clone(),
            RedactionEntry {
                original: original.to_string(),
                kind: kind.to_string(),
                created_at: Instant::now(),
            },
        ));
        placeholder
    }

    /// Reidrata il testo: sostituisce ogni placeholder ancora valido con il suo
    /// valore originale. Le entry scadute (oltre TTL) vengono rimosse e NON
    /// reidratate (il placeholder resta nel testo).
    pub fn rehydrate(&mut self, text: &str) -> String {
        let ttl = self.ttl;
        // Rimuove le entry scadute prima di reidratare.
        self.entries
            .retain(|(_, entry)| entry.created_at.elapsed() <= ttl);

        let mut result = text.to_string();
        for (placeholder, entry) in &self.entries {
            if result.contains(placeholder.as_str()) {
                result = result.replace(placeholder.as_str(), &entry.original);
            }
        }
        result
    }

    /// Numero di entry attive.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` se la mappa e' vuota.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Tipi distinti presenti nella mappa.
    pub fn types(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (_, entry) in &self.entries {
            if !out.contains(&entry.kind) {
                out.push(entry.kind.clone());
            }
        }
        out
    }

    /// Snapshot per audit: solo placeholder + tipo, MAI il valore originale
    /// (regola F).
    pub fn audit_snapshot(&self) -> Vec<(String, String)> {
        self.entries
            .iter()
            .map(|(placeholder, entry)| (placeholder.clone(), entry.kind.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_e_rehydrate_round_trip() {
        let mut map = RedactionMap::new("req-1");
        let ph = map.store("valore-segreto", "secret_value");
        assert!(ph.starts_with("__NEXUS_SECRET_VALUE_"));
        let testo = format!("ecco il dato: {ph} fine");
        let reidratato = map.rehydrate(&testo);
        assert_eq!(reidratato, "ecco il dato: valore-segreto fine");
    }

    #[test]
    fn store_deduplica_stesso_valore_e_tipo() {
        let mut map = RedactionMap::new("req-2");
        let a = map.store("x", "identifier");
        let b = map.store("x", "identifier");
        assert_eq!(a, b);
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn store_distingue_tipi_diversi() {
        let mut map = RedactionMap::new("req-3");
        let a = map.store("x", "identifier");
        let b = map.store("x", "secret_value");
        assert_ne!(a, b);
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn rehydrate_scaduto_non_sostituisce() {
        // TTL nullo: ogni entry e' immediatamente scaduta.
        let mut map = RedactionMap::with_ttl("req-4", Duration::from_secs(0));
        let ph = map.store("segreto", "secret_value");
        // L'entry e' inserita con created_at "adesso", TTL 0 -> gia' scaduta.
        let out = map.rehydrate(&format!("dato {ph}"));
        assert!(out.contains(&ph));
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn audit_snapshot_non_espone_originale() {
        let mut map = RedactionMap::new("req-5");
        map.store("segretissimo", "secret_value");
        let snap = map.audit_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].1, "secret_value");
        // Nessun valore originale nello snapshot.
        assert!(!snap[0].0.contains("segretissimo"));
        assert!(!snap[0].1.contains("segretissimo"));
    }

    #[test]
    fn types_distinti() {
        let mut map = RedactionMap::new("req-6");
        map.store("a", "identifier");
        map.store("b", "identifier");
        map.store("c", "secret_value");
        let mut types = map.types();
        types.sort();
        assert_eq!(types, vec!["identifier", "secret_value"]);
    }
}
