//! Stato del grafo e delta di aggiornamento.
//!
//! `StateDelta` e' il "cio' che un nodo ha modificato": in FASE 0 e' una struct
//! OPACA basata su `serde_json::Value` (nessuna conoscenza dei ~90 campi dello
//! stato agentico, che vivranno tipizzati in `nexus-agent-graph` nelle fasi
//! successive). Il runtime non interpreta il delta: lo passa al reducer dello
//! stato (`GraphState::merge`), che e' il PUNTO UNICO della semantica di merge
//! (regola L). Aggiungere un canale-append futuro = una riga nel reducer dello
//! stato concreto, non un `if` sparso nei nodi.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Aggiornamento di stato prodotto da un nodo. Opaco al runtime.
///
/// Internamente e' una mappa JSON: chiave assente = "non toccare" (no-op),
/// chiave presente = overwrite. La distinzione chiave-assente vs
/// chiave-presente-vuota (es. azzeramento di una lista) e' LOAD-BEARING e va
/// preservata bit-per-bit dal reducer dello stato concreto; per questo il delta
/// conserva la mappa cosi' com'e', senza normalizzazioni.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateDelta(pub Map<String, Value>);

impl StateDelta {
    /// Delta vuoto: nessuna chiave -> il merge e' un no-op completo.
    pub fn empty() -> Self {
        StateDelta(Map::new())
    }

    /// Costruisce un delta da una mappa JSON gia' pronta.
    pub fn from_map(map: Map<String, Value>) -> Self {
        StateDelta(map)
    }

    /// `true` se il delta non contiene alcuna chiave (merge no-op).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Imposta una chiave nel delta (helper per i nodi / i test).
    pub fn set(&mut self, key: impl Into<String>, value: Value) {
        self.0.insert(key.into(), value);
    }

    /// Accesso in sola lettura alla mappa sottostante (usato dal reducer).
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }
}

/// Contratto dello stato condiviso del grafo.
///
/// Il runtime usa solo questi tre metodi: NON conosce i campi concreti dello
/// stato. `merge` e' il reducer (punto unico, regola L). I due predicati di
/// interrupt mappano 1:1 sugli stati NON terminali dell'orchestratore Nexus
/// (`AwaitingConfirmation` / `BlockedNeedsInput`).
pub trait GraphState: Send + Sync {
    /// Applica un delta allo stato secondo la semantica di canale (append vs
    /// overwrite). Implementazione concreta nel crate dei nodi.
    fn merge(&mut self, delta: StateDelta);

    /// `true` se lo stato richiede una conferma umana prima di proseguire
    /// (HITL): il motore sospende con `Interrupted`.
    fn is_awaiting_confirmation(&self) -> bool;

    /// `true` se lo stato attende un chiarimento dall'utente (disambiguazione):
    /// il motore sospende con `Interrupted`.
    fn is_pending_clarify(&self) -> bool;
}
