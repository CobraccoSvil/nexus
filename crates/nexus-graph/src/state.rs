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
/// Il runtime usa solo questi due metodi: NON conosce i campi concreti dello
/// stato. `merge` e' il reducer (punto unico, regola L).
///
/// L'UNICO predicato di interrupt-resume del runtime e' `is_awaiting_interrupt`
/// (il run si SOSPENDE e riprende dallo stesso punto). Il motore resta AGNOSTICO
/// al MOTIVO dell'interrupt (conferma umana HITL, attesa dei sub-run background,
/// ...): lo stato concreto compone i propri flag in questo unico predicato e il
/// resume azzera quello giusto. NON esiste un predicato `is_pending_clarify` nel
/// contratto del runtime: `pending_clarify` e' uno stato TERMINALE (il run CHIUDE
/// con `Completed`, il prossimo input avvia un nuovo run dall'entry), gestito
/// dalla TOPOLOGIA con un edge condizionale a `End`, non dal motore. Replica 1:1
/// `brain/agents/graph.py` (`_route_after_clarify_or_expand` -> END vs
/// `interrupt_before=["executor"]`).
pub trait GraphState: Send + Sync {
    /// Applica un delta allo stato secondo la semantica di canale (append vs
    /// overwrite). Implementazione concreta nel crate dei nodi.
    fn merge(&mut self, delta: StateDelta);

    /// `true` se lo stato deve SOSPENDERE il run in attesa di un evento esterno
    /// che lo riprendera' dallo stesso punto (conferma umana HITL oppure
    /// completamento dei sub-run background). Il motore sospende con `Interrupted`
    /// senza conoscere il motivo; il resume inietta il delta che azzera il flag
    /// specifico. PUNTO UNICO dell'interrupt-resume (regola L).
    fn is_awaiting_interrupt(&self) -> bool;
}
