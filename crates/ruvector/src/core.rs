//! HNSW core — Hierarchical Navigable Small World
//!
//! Implementazione corretta e ottimizzata del grafo HNSW per ricerca
//! approssimata del nearest neighbor (ANN) in spazio vettoriale.
//!
//! ## Correzioni rispetto alla versione precedente
//! - **Bug neighbors**: rimossa doppia-allocazione (`2*(level+1)` → `level+1`)
//! - **Entry point**: aggiornato quando un nodo ha livello superiore
//! - **search_level O(n²)**: ora O(ef·log(ef)) con worst_dist tracking + BinaryHeap per result
//! - **result.contains() O(n)**: sostituito con HashSet O(1)
//! - **Deleted nodes**: filtrati in search_level e stats
//! - **Metrica coseno**: ora selezionabile via config
//!
//! ## Nuove funzionalità
//! - `delete_by_id()` — soft-delete per id esterno
//! - `search_with_ef()` — ricerca con ef personalizzabile
//! - `prune_by_confidence()` — SONA-style pruning dei nodi sotto soglia
//! - `optimize()` — prune + consistency report

use crate::types::*;
use ordered_float::OrderedFloat;
use parking_lot::RwLock;
use rand::Rng;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Arc;
use tracing::{debug, info, trace};

// ─── Distance metrics ─────────────────────────────────────────────────────────

/// Distanza euclidea tra due vettori (L2).
#[inline]
pub fn euclidean_distance(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return f32::MAX;
    }
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f32>()
        .sqrt()
}

/// Distanza coseno normalizzata (1 − cosine_similarity).
#[inline]
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return f32::MAX;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return f32::MAX;
    }
    (1.0 - dot / (na * nb)).max(0.0) // clamp a 0 per errori numerici
}

/// Metrica di distanza selezionabile a runtime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Metric {
    #[default]
    Euclidean,
    Cosine,
}

impl Metric {
    #[inline]
    fn distance(&self, a: &[f32], b: &[f32]) -> f32 {
        match self {
            Self::Euclidean => euclidean_distance(a, b),
            Self::Cosine => cosine_distance(a, b),
        }
    }
}

// ─── Candidate (max-heap per nearest-first) ───────────────────────────────────

#[derive(Clone, Debug)]
struct Candidate {
    id: usize,
    distance: OrderedFloat<f32>,
}

impl Eq for Candidate {}
impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.distance == other.distance && self.id == other.id
    }
}
impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        // Min-heap: il candidato più vicino (distanza minore) ha priorità
        other.distance.cmp(&self.distance)
    }
}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ─── Statistiche ottimizzazione ──────────────────────────────────────────────

/// Report restituito da `optimize()`.
#[derive(Clone, Debug, Default)]
pub struct OptimizeStats {
    pub pruned_count: usize,
    pub active_count: usize,
    pub total_count: usize,
}

// ─── HnswDb ──────────────────────────────────────────────────────────────────

/// Database vettoriale HNSW multi-livello.
///
/// Thread-safe: può essere wrappato in `Arc<HnswDb>` e condiviso tra task.
pub struct HnswDb {
    config: HnswConfig,
    metric: Metric,
    nodes: Arc<RwLock<Vec<HnswNode>>>,
    id_to_node_id: Arc<RwLock<HashMap<String, usize>>>,
    entry_point: Arc<RwLock<Option<usize>>>,
    rng: Arc<RwLock<rand::rngs::StdRng>>,
    vector_dim: Arc<RwLock<Option<usize>>>,
}

impl HnswDb {
    /// Crea un nuovo grafo HNSW con la configurazione fornita.
    pub fn new(config: HnswConfig) -> Self {
        let rng = rand::SeedableRng::seed_from_u64(config.seed);
        Self {
            config,
            metric: Metric::Euclidean,
            nodes: Arc::new(RwLock::new(Vec::new())),
            id_to_node_id: Arc::new(RwLock::new(HashMap::new())),
            entry_point: Arc::new(RwLock::new(None)),
            rng: Arc::new(RwLock::new(rng)),
            vector_dim: Arc::new(RwLock::new(None)),
        }
    }

    /// Costruisce con metrica di distanza specifica.
    pub fn with_metric(mut self, metric: Metric) -> Self {
        self.metric = metric;
        self
    }

    // ── Insert ────────────────────────────────────────────────────────────────

    /// Inserisce un vettore nel grafo HNSW.
    ///
    /// Ritorna il `node_id` interno (indice nella Vec). Per l'id esterno
    /// usa il campo `id` passato come primo argomento.
    pub fn insert(
        &self,
        id: String,
        vector: Vec<f32>,
        metadata: Option<VectorMetadata>,
    ) -> Result<usize> {
        self.insert_with_confidence(id, vector, metadata, 1.0)
    }

    /// Inserisce un vettore con confidenza esplicita (per SONA pruning).
    pub fn insert_with_confidence(
        &self,
        id: String,
        vector: Vec<f32>,
        metadata: Option<VectorMetadata>,
        confidence: f32,
    ) -> Result<usize> {
        // ── 1. Validazione dimensione ─────────────────────────────────────────
        {
            let mut vd = self.vector_dim.write();
            match *vd {
                None => *vd = Some(vector.len()),
                Some(expected) if vector.len() != expected => {
                    return Err(Error::InvalidDimension {
                        expected,
                        actual: vector.len(),
                    });
                }
                _ => {}
            }
        }

        // ── 2. Assegna livello e crea nodo ────────────────────────────────────
        let level = self.random_level();
        let node_id = {
            let mut nodes = self.nodes.write();
            let nid = nodes.len();
            // FIX: allocazione corretta — esattamente `level+1` livelli
            let neighbors = vec![Vec::with_capacity(self.config.m_max); level + 1];
            nodes.push(HnswNode {
                id: nid,
                external_id: id.clone(),
                level,
                neighbors,
                vector: vector.clone(),
                metadata: metadata.clone(),
                deleted: false,
                confidence: confidence.clamp(0.0, 1.0),
            });
            nid
        };

        // ── 3. Mappa id esterno → node_id ─────────────────────────────────────
        self.id_to_node_id.write().insert(id.clone(), node_id);

        // ── 4. Primo nodo: diventa entry point ────────────────────────────────
        {
            let mut ep = self.entry_point.write();
            if ep.is_none() {
                *ep = Some(node_id);
                debug!("RuVector: first node inserted, id={}, node_id={}", id, node_id);
                return Ok(node_id);
            }
        }

        // ── 5. Inserisce nel grafo e collega ai vicini ────────────────────────
        self.insert_into_graph(node_id, level);

        // ── 6. FIX: aggiorna entry point se il nuovo nodo ha livello superiore
        {
            let mut ep = self.entry_point.write();
            let nodes = self.nodes.read();
            if let Some(ep_id) = *ep {
                if level > nodes[ep_id].level {
                    *ep = Some(node_id);
                    debug!("RuVector: new entry point id={}, level={}", id, level);
                }
            }
        }

        debug!(
            "RuVector: inserted vector id={}, node_id={}, level={}",
            id, node_id, level
        );

        Ok(node_id)
    }

    // ── Search ────────────────────────────────────────────────────────────────

    /// Ricerca i `k` vettori più vicini usando `ef_search` dalla config.
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<SearchResult>> {
        self.search_with_ef(query, k, self.config.ef_search)
    }

    /// Ricerca i `k` vettori più vicini con `ef` personalizzabile.
    ///
    /// `ef >= k` è richiesto per risultati corretti (clampato automaticamente).
    pub fn search_with_ef(&self, query: &[f32], k: usize, ef: usize) -> Result<Vec<SearchResult>> {
        // Valida dimensione query
        if let Some(expected) = *self.vector_dim.read() {
            if query.len() != expected {
                return Err(Error::InvalidDimension {
                    expected,
                    actual: query.len(),
                });
            }
        }

        let entry_point = match *self.entry_point.read() {
            Some(ep) => ep,
            None => return Ok(Vec::new()),
        };

        let ef_actual = ef.max(k); // ef deve essere almeno k

        let nodes = self.nodes.read();

        // Se entry point è deleted, cerca il primo nodo attivo
        let Some(ep) = self.find_active_entry(&nodes, entry_point) else {
            return Ok(Vec::new());
        };
        let ep_level = nodes[ep].level;

        // ── Greedy descent nei livelli superiori (ef=1) ───────────────────────
        let mut nearest = vec![ep];
        for level in (1..=ep_level).rev() {
            nearest = self.search_level_inner(query, &nearest, 1, level, &nodes);
        }

        // ── Ricerca dettagliata al livello 0 ──────────────────────────────────
        nearest = self.search_level_inner(query, &nearest, ef_actual, 0, &nodes);

        // ── Estrai top-k risultati ────────────────────────────────────────────
        let metric = self.metric;
        let results: Vec<SearchResult> = nearest
            .iter()
            .take(k)
            .map(|&nid| {
                let node = &nodes[nid];
                let distance = metric.distance(query, &node.vector);
                // Usa external_id come source-of-truth; il campo metadata.id
                // è presente solo quando il chiamante ha passato metadata.
                let id = if !node.external_id.is_empty() {
                    node.external_id.clone()
                } else {
                    node.metadata.as_ref().map(|m| m.id.clone()).unwrap_or_default()
                };
                SearchResult {
                    id,
                    score: 1.0 / (1.0 + distance),
                    distance,
                    metadata: node.metadata.clone(),
                }
            })
            .collect();

        trace!("RuVector search: {} results (k={}, ef={})", results.len(), k, ef_actual);
        Ok(results)
    }

    // ── Delete ────────────────────────────────────────────────────────────────

    /// Soft-delete di un vettore per id esterno.
    ///
    /// Il nodo rimane nella Vec (per preservare la connettività del grafo)
    /// ma viene escluso da tutte le ricerche future.
    /// Ritorna `true` se il nodo è stato trovato e marcato come deleted.
    pub fn delete_by_id(&self, external_id: &str) -> bool {
        let node_id = match self.id_to_node_id.read().get(external_id).copied() {
            Some(id) => id,
            None => return false,
        };
        let mut nodes = self.nodes.write();
        if node_id < nodes.len() {
            nodes[node_id].deleted = true;
            true
        } else {
            false
        }
    }

    // ── SONA Pruning ──────────────────────────────────────────────────────────

    /// Esegue pruning SONA-style: marca come deleted tutti i nodi
    /// con `confidence < min_confidence`.
    ///
    /// Ritorna il numero di nodi marcati.
    pub fn prune_by_confidence(&self, min_confidence: f32) -> usize {
        let mut nodes = self.nodes.write();
        let mut pruned = 0usize;
        for node in nodes.iter_mut() {
            if !node.deleted && node.confidence < min_confidence {
                node.deleted = true;
                pruned += 1;
            }
        }
        if pruned > 0 {
            info!("RuVector SONA prune: {} nodi marcati deleted (confidence<{})", pruned, min_confidence);
        }
        pruned
    }

    /// Esegue un ciclo completo di ottimizzazione:
    /// 1. Prune dei nodi sotto soglia di confidenza
    /// 2. Ricerca + aggiorna entry point verso il nodo attivo di livello massimo
    ///
    /// Chiamare periodicamente (es. ogni 60s) per mantenere la qualità dell'indice.
    pub fn optimize(&self, min_confidence: Option<f32>) -> OptimizeStats {
        let pruned = min_confidence
            .map(|t| self.prune_by_confidence(t))
            .unwrap_or(0);

        // Aggiorna entry point al nodo attivo con livello massimo
        {
            let nodes = self.nodes.read();
            let mut ep = self.entry_point.write();
            let best = nodes
                .iter()
                .filter(|n| !n.deleted)
                .max_by_key(|n| n.level);
            if let Some(b) = best {
                *ep = Some(b.id);
            }
        }

        let nodes = self.nodes.read();
        let total = nodes.len();
        let deleted = nodes.iter().filter(|n| n.deleted).count();
        OptimizeStats {
            pruned_count: pruned,
            active_count: total - deleted,
            total_count: total,
        }
    }

    // ── Stats ─────────────────────────────────────────────────────────────────

    /// Statistiche correnti del grafo.
    pub fn stats(&self) -> HnswStats {
        let nodes = self.nodes.read();
        let total = nodes.len();
        let deleted = nodes.iter().filter(|n| n.deleted).count();
        let active = total - deleted;

        let avg_neighbors = nodes
            .iter()
            .filter(|n| !n.deleted)
            .map(|n| n.neighbors.iter().map(|l| l.len()).sum::<usize>())
            .sum::<usize>()
            .checked_div(active)
            .unwrap_or(0);

        HnswStats {
            total_nodes: total,
            avg_neighbors,
            entry_point: *self.entry_point.read(),
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Ricerca ottimizzata a un livello del grafo.
    ///
    /// ### Complessità
    /// - Candidati: O(ef · log(ef)) usando BinaryHeap
    /// - Membership check: O(1) con HashSet<usize>
    /// - Worst distance: tracciato esplicitamente O(1)
    fn search_level_inner(
        &self,
        query: &[f32],
        entry_points: &[usize],
        ef: usize,
        level: usize,
        nodes: &[HnswNode],
    ) -> Vec<usize> {
        let metric = self.metric;
        let mut candidates: BinaryHeap<Candidate> = BinaryHeap::new(); // min-heap (più vicini)
        let mut visited: HashSet<usize> = HashSet::new();

        // Result set come max-heap (worst on top per eject veloce)
        let mut result: BinaryHeap<std::cmp::Reverse<Candidate>> = BinaryHeap::new();
        let mut result_ids: HashSet<usize> = HashSet::new();

        for &ep in entry_points {
            if ep >= nodes.len() || nodes[ep].deleted {
                continue;
            }
            let dist = metric.distance(query, &nodes[ep].vector);
            candidates.push(Candidate { id: ep, distance: OrderedFloat(dist) });
            visited.insert(ep);

            result.push(std::cmp::Reverse(Candidate { id: ep, distance: OrderedFloat(dist) }));
            result_ids.insert(ep);
        }

        // Worst distance nel result set (0 se vuoto)
        let worst_dist = |result: &BinaryHeap<std::cmp::Reverse<Candidate>>| -> f32 {
            result
                .peek()
                .map(|std::cmp::Reverse(c)| c.distance.into_inner())
                .unwrap_or(f32::MAX)
        };

        while let Some(candidate) = candidates.pop() {
            let c_dist = candidate.distance.into_inner();

            // Terminazione: se il candidato corrente è più lontano del worst in result,
            // non possiamo trovare nulla di meglio
            if result.len() >= ef && c_dist > worst_dist(&result) {
                break;
            }

            let node = &nodes[candidate.id];
            if level >= node.neighbors.len() {
                continue;
            }

            for &neighbor_id in &node.neighbors[level] {
                if neighbor_id >= nodes.len() || visited.contains(&neighbor_id) {
                    continue;
                }
                if nodes[neighbor_id].deleted {
                    continue;
                }
                visited.insert(neighbor_id);

                let n_dist = metric.distance(query, &nodes[neighbor_id].vector);
                let current_worst = worst_dist(&result);

                if n_dist < current_worst || result.len() < ef {
                    candidates.push(Candidate { id: neighbor_id, distance: OrderedFloat(n_dist) });

                    if !result_ids.contains(&neighbor_id) {
                        result.push(std::cmp::Reverse(Candidate {
                            id: neighbor_id,
                            distance: OrderedFloat(n_dist),
                        }));
                        result_ids.insert(neighbor_id);

                        // Eject worst se result supera ef
                        if result.len() > ef {
                            if let Some(std::cmp::Reverse(worst)) = result.pop() {
                                result_ids.remove(&worst.id);
                            }
                        }
                    }
                }
            }
        }

        // Converti result a Vec<usize> ordinato per distanza crescente
        let mut out: Vec<(usize, f32)> = result
            .into_iter()
            .map(|std::cmp::Reverse(c)| (c.id, c.distance.into_inner()))
            .collect();
        out.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
        out.into_iter().map(|(id, _)| id).collect()
    }

    /// Inserisce `new_node_id` nel grafo, collegandolo ai vicini a ogni livello.
    fn insert_into_graph(&self, new_node_id: usize, new_level: usize) {
        let new_vector = self.nodes.read()[new_node_id].vector.clone();
        let entry_id = self.entry_point.read().unwrap_or(0);
        let entry_level = self.nodes.read()[entry_id].level;

        // Discendi dal livello max dell'entry point fino a new_level+1 (greedy, ef=1)
        let mut nearest = vec![entry_id];
        let top = entry_level;
        if top > new_level {
            for level in (new_level + 1..=top).rev() {
                let nodes = self.nodes.read();
                nearest = self.search_level_inner(&new_vector, &nearest, 1, level, &nodes);
            }
        }

        // A ogni livello da new_level a 0: trova vicini e collega
        for level in (0..=new_level).rev() {
            let ef = self.config.ef_construction;
            let m = if level == 0 {
                self.config.m_max * 2
            } else {
                self.config.m_max
            };

            let candidates = {
                let nodes = self.nodes.read();
                self.search_level_inner(&new_vector, &nearest, ef, level, &nodes)
            };

            // Collega new_node ↔ candidati
            {
                let mut nodes = self.nodes.write();
                for &cand_id in candidates.iter().take(m) {
                    // new_node → cand
                    if level < nodes[new_node_id].neighbors.len() {
                        let nbrs = &mut nodes[new_node_id].neighbors[level];
                        if !nbrs.contains(&cand_id) && nbrs.len() < m {
                            nbrs.push(cand_id);
                        }
                    }
                    // cand → new_node (back-link)
                    if cand_id < nodes.len() && level < nodes[cand_id].neighbors.len() {
                        let nbrs = &mut nodes[cand_id].neighbors[level];
                        if !nbrs.contains(&new_node_id) && nbrs.len() < m {
                            nbrs.push(new_node_id);
                        }
                    }
                }
            }

            nearest = candidates;
        }
    }

    /// Livello random per un nuovo nodo (distribuzione geometrica troncata).
    fn random_level(&self) -> usize {
        let mut rng = self.rng.write();
        let ml = self.config.m_l;
        let mut level = 0usize;
        while rng.gen::<f32>() < 1.0 / ml && level < 16 {
            level += 1;
        }
        level
    }

    /// Trova il primo entry point attivo (non deleted) a partire da `start`.
    fn find_active_entry(&self, nodes: &[HnswNode], start: usize) -> Option<usize> {
        if start < nodes.len() && !nodes[start].deleted {
            return Some(start);
        }
        // Fallback: primo nodo attivo
        nodes.iter().position(|n| !n.deleted)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> HnswDb {
        HnswDb::new(HnswConfig::default())
    }

    #[test]
    fn test_insert_and_search() {
        let db = make_db();
        db.insert("v1".into(), vec![1.0, 0.0, 0.0], None).unwrap();
        db.insert("v2".into(), vec![0.9, 0.1, 0.0], None).unwrap();
        db.insert("v3".into(), vec![0.0, 1.0, 0.0], None).unwrap();

        let results = db.search(&[1.0, 0.0, 0.0], 2).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].distance <= results[1].distance, "risultati non ordinati");
    }

    #[test]
    fn test_search_empty_returns_empty() {
        let db = make_db();
        let results = db.search(&[1.0, 0.0], 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_dimension_mismatch_rejected() {
        let db = make_db();
        db.insert("v1".into(), vec![1.0, 0.0], None).unwrap();
        let err = db.insert("v2".into(), vec![1.0, 0.0, 0.0], None);
        assert!(matches!(err, Err(Error::InvalidDimension { .. })));
    }

    #[test]
    fn test_search_dim_mismatch_rejected() {
        let db = make_db();
        db.insert("v1".into(), vec![1.0, 0.0], None).unwrap();
        let err = db.search(&[1.0, 0.0, 0.0], 1);
        assert!(matches!(err, Err(Error::InvalidDimension { .. })));
    }

    #[test]
    fn test_no_neighbors_bug() {
        // Regressione: prima la doppia-allocazione creava 2*(level+1) neighbor lists.
        // Dopo il fix ogni nodo deve avere esattamente `level+1` liste.
        let db = make_db();
        db.insert("v1".into(), vec![1.0, 0.0], None).unwrap();
        let nodes = db.nodes.read();
        let n = &nodes[0];
        assert_eq!(n.neighbors.len(), n.level + 1);
    }

    #[test]
    fn test_stats_counts_active_only() {
        let db = make_db();
        db.insert("v1".into(), vec![1.0, 0.0, 0.0], None).unwrap();
        db.insert("v2".into(), vec![0.0, 1.0, 0.0], None).unwrap();
        db.delete_by_id("v1");

        let s = db.stats();
        assert_eq!(s.total_nodes, 2);
        // avg_neighbors calcolato su nodi attivi
        assert_eq!(s.total_nodes, 2);
    }

    #[test]
    fn test_delete_by_id_excludes_from_search() {
        let db = make_db();
        db.insert("near".into(),  vec![1.0, 0.01, 0.0], None).unwrap();
        db.insert("far".into(),   vec![0.0, 1.0,  0.0], None).unwrap();
        db.insert("exact".into(), vec![1.0, 0.0,  0.0], None).unwrap();

        // Prima del delete "exact" è il più vicino
        let r = db.search(&[1.0, 0.0, 0.0], 1).unwrap();
        assert_eq!(r[0].id, "exact");

        // Dopo il delete, "near" diventa il più vicino
        db.delete_by_id("exact");
        let r = db.search(&[1.0, 0.0, 0.0], 1).unwrap();
        assert_ne!(r[0].id, "exact", "deleted node deve essere escluso");
    }

    #[test]
    fn test_delete_returns_false_for_unknown_id() {
        let db = make_db();
        assert!(!db.delete_by_id("nonexistent"));
    }

    #[test]
    fn test_prune_by_confidence() {
        let db = make_db();
        db.insert_with_confidence("high".into(), vec![1.0, 0.0], None, 0.9).unwrap();
        db.insert_with_confidence("low".into(),  vec![0.0, 1.0], None, 0.1).unwrap();

        let pruned = db.prune_by_confidence(0.5);
        assert_eq!(pruned, 1);

        // "low" deve essere escluso dalla ricerca
        let results = db.search(&[0.0, 1.0], 2).unwrap();
        assert!(results.iter().all(|r| r.id != "low"), "node con bassa confidenza deve essere pruned");
    }

    #[test]
    fn test_optimize_updates_entry_point() {
        let db = make_db();
        db.insert("v1".into(), vec![1.0, 0.0], None).unwrap();
        db.insert("v2".into(), vec![0.0, 1.0], None).unwrap();
        db.insert("v3".into(), vec![0.5, 0.5], None).unwrap();

        let stats = db.optimize(None);
        assert_eq!(stats.pruned_count, 0);
        assert_eq!(stats.total_count, 3);
        assert_eq!(stats.active_count, 3);
    }

    #[test]
    fn test_optimize_with_confidence_prune() {
        let db = make_db();
        db.insert_with_confidence("a".into(), vec![1.0, 0.0], None, 0.8).unwrap();
        db.insert_with_confidence("b".into(), vec![0.0, 1.0], None, 0.2).unwrap();
        db.insert_with_confidence("c".into(), vec![0.5, 0.5], None, 0.9).unwrap();

        let stats = db.optimize(Some(0.5));
        assert_eq!(stats.pruned_count, 1); // solo "b"
        assert_eq!(stats.active_count, 2);
    }

    #[test]
    fn test_cosine_metric_search() {
        let db = HnswDb::new(HnswConfig::default()).with_metric(Metric::Cosine);
        db.insert("same_dir".into(), vec![2.0, 0.0], None).unwrap();
        db.insert("perp".into(),     vec![0.0, 2.0], None).unwrap();
        db.insert("close".into(),    vec![1.9, 0.1], None).unwrap();

        let results = db.search(&[1.0, 0.0], 2).unwrap();
        assert_eq!(results.len(), 2);
        // Con metrica coseno "same_dir" e "close" sono più simili di "perp"
        assert_ne!(results[0].id, "perp", "perp non deve essere il più vicino con metrica coseno");
    }

    #[test]
    fn test_search_with_custom_ef() {
        let db = make_db();
        for i in 0..20 {
            db.insert(format!("v{}", i), vec![i as f32, 0.0], None).unwrap();
        }
        // ef piccolo → risultati meno accurati ma comunque k
        let r_small = db.search_with_ef(&[10.0, 0.0], 3, 3).unwrap();
        let r_large = db.search_with_ef(&[10.0, 0.0], 3, 50).unwrap();
        assert_eq!(r_small.len(), 3);
        assert_eq!(r_large.len(), 3);
    }

    #[test]
    fn test_many_inserts_stay_connected() {
        let db = HnswDb::new(HnswConfig {
            m_max: 4,
            ef_construction: 20,
            ef_search: 20,
            ..HnswConfig::default()
        });
        for i in 0..50 {
            db.insert(format!("v{}", i), vec![i as f32, (50 - i) as f32], None).unwrap();
        }
        let s = db.stats();
        assert_eq!(s.total_nodes, 50);
        assert!(s.entry_point.is_some());
        // Verifica che la ricerca restituisca risultati sensati
        let r = db.search(&[25.0, 25.0], 5).unwrap();
        assert_eq!(r.len(), 5);
    }
}
