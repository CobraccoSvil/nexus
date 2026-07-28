//! Memorie di progetto nel prompt: punto unico (regola L) di QUALI entrano e
//! COME si rendono.
//!
//! Sono le voci del pannello "Memoria del progetto" della chat: riassunti di
//! sessioni compattate, salvati in `prompt_corrections` (DB del progetto) con un
//! punto gemello nel motore vettoriale. L'utente le attiva una per una; una
//! memoria attiva viene RICHIAMATA quando e' semanticamente vicina alla domanda
//! del turno, non inclusa sempre.
//!
//! ROOT CAUSE che ha reso necessario questo modulo: il consumo viveva dentro
//! `Orchestrator::run`, cioe' su un ramo solo. `run` e' raggiungibile unicamente
//! da `chat_messages::run.rs::run_turn`, e in modalita' Conferma/Automatico
//! l'handler dispatcha a `spawn_agent_run` e ritorna PRIMA di arrivarci: le
//! memorie non entravano affatto nei run agentici. Il ramo che le carica ora e'
//! uno solo per due chiamanti — se restasse ricopiato, la prossima soglia o il
//! prossimo filtro divergerebbe di nuovo in silenzio fra i due percorsi.
//!
//! Il confine ESTERNO del richiamo (embedding + ricerca vettoriale) sta dietro
//! [`MemoryRecall`]: la produzione usa [`VectorRecall`], i test uno store in
//! memoria che applica il filtro VERO (`vector_memory::prompt_correction_filter`)
//! invece di riscriverlo. Tutto cio' che decide - gate, soglia, taglio, forma del
//! blocco - resta di qua dal confine ed e' attraversato da entrambi.

use anyhow::Context;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::orchestrator::NeuralCoreClient;
use crate::vector_memory::{self, VectorPointHit};

/// Quante memorie al massimo si chiedono al motore vettoriale per un turno.
const RICHIAMO_MAX: u64 = 5;

/// Sotto questa similarita' una memoria non e' considerata pertinente alla
/// domanda e non entra nel prompt, per quanto sia attiva.
const SOGLIA_PERTINENZA: f64 = 0.78;

/// Confine esterno del richiamo: dalla domanda agli hit del motore vettoriale.
///
/// Esiste per una ragione sola: e' l'unico pezzo della catena che parla con
/// servizi fuori processo (embedder e Qdrant). Tenerlo dietro un tratto permette
/// a un test di misurare cio' che viene DOPO - gate, soglia, taglio, rendering e
/// innesto nel prompt - senza dipendere da quei servizi e senza fabbricare da se'
/// il prompt che vuole verificare (regola O).
#[async_trait::async_trait]
pub(crate) trait MemoryRecall: Send + Sync {
    async fn hits(
        &self,
        db: &PgPool,
        project_id: Uuid,
        query: &str,
        top_k: u64,
    ) -> anyhow::Result<Vec<VectorPointHit>>;
}

/// Il richiamo di produzione: embedding in-process della domanda + ricerca nella
/// collection delle correzioni/memorie, filtrata per progetto e per stato attivo.
pub(crate) struct VectorRecall<'a> {
    neural: &'a NeuralCoreClient,
}

impl<'a> VectorRecall<'a> {
    pub(crate) fn new(neural: &'a NeuralCoreClient) -> Self {
        Self { neural }
    }
}

#[async_trait::async_trait]
impl MemoryRecall for VectorRecall<'_> {
    async fn hits(
        &self,
        db: &PgPool,
        project_id: Uuid,
        query: &str,
        top_k: u64,
    ) -> anyhow::Result<Vec<VectorPointHit>> {
        // I due contesti restano distinti nella catena anyhow: chi logga stampa
        // `{error:#}` e vede se e' caduto l'embedding o la ricerca.
        let embedding = self
            .neural
            .embed_text("", query)
            .await
            .context("embedding della domanda per il richiamo delle memorie")?;
        vector_memory::search_prompt_correction_points(db, &embedding, project_id, top_k)
            .await
            .context("ricerca delle memorie nel motore vettoriale")
    }
}

/// Le memorie richiamate per un turno, gia' filtrate per pertinenza.
#[derive(Debug, Default, Clone)]
pub(crate) struct ProjectMemories {
    items: Vec<Value>,
}

impl ProjectMemories {
    /// Richiama le memorie attive pertinenti a `query` per il progetto.
    ///
    /// E' il punto unico: gate di progetto, taglio a [`RICHIAMO_MAX`], soglia
    /// [`SOGLIA_PERTINENZA`] e contabilizzazione del recupero stanno qui e
    /// nessun chiamante li ripete. Non fallisce mai verso l'alto: un turno senza
    /// memorie e' un turno valido, un turno rotto perche' Qdrant non risponde no.
    pub(crate) async fn load(
        db: &PgPool,
        recall: &dyn MemoryRecall,
        project_id: Uuid,
        query: &str,
    ) -> Self {
        if !enabled(db, project_id).await {
            return Self::default();
        }

        let hits = match recall.hits(db, project_id, query, RICHIAMO_MAX).await {
            Ok(hits) => hits,
            Err(error) => {
                tracing::warn!("memorie di progetto non richiamate: {error:#}");
                return Self::default();
            }
        };

        let (items, retrieved_ids) = collect_from_hits(hits);
        if !retrieved_ids.is_empty() {
            bump_retrieval(db, project_id, &retrieved_ids).await;
        }

        Self { items }
    }

    /// Blocco di prompt "Correzioni note", `None` quando non c'e' nulla da dire.
    /// La forma e' identica per il turno singolo e per il run agentico: e' lo
    /// stesso testo, prodotto qui una volta sola.
    pub(crate) fn section(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut block = String::from("Correzioni note (da rispettare se pertinenti):\n");
        for memory in &self.items {
            if let Some(text) = memory.get("text").and_then(Value::as_str) {
                block.push_str("- ");
                block.push_str(text.trim());
                block.push('\n');
            }
        }
        Some(block.trim().to_string())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    /// Le memorie richiamate, per l'audit del turno (`applied_corrections`).
    pub(crate) fn into_values(self) -> Vec<Value> {
        self.items
    }
}

/// Le memorie sono attive per questo progetto? Richiede il flag globale
/// (`settings.learning_prompt_corrections_enabled`) E quello di progetto
/// (`project_learning_config.prompt_corrections_enabled`), entrambi con default
/// `true` quando assenti o illeggibili.
async fn enabled(db: &PgPool, project_id: Uuid) -> bool {
    let globally_enabled = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'learning_prompt_corrections_enabled'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .map(|value| value.trim().eq_ignore_ascii_case("true"))
    .unwrap_or(true);
    if !globally_enabled {
        return false;
    }

    sqlx::query_scalar::<_, bool>(
        r#"
        SELECT prompt_corrections_enabled
        FROM project_learning_config
        WHERE project_id = $1
        "#,
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .unwrap_or(true)
}

/// Memorie pertinenti a partire dagli hit, piu' gli id da contabilizzare.
/// Scarta chi sta sotto [`SOGLIA_PERTINENZA`] e chi non ha testo.
fn collect_from_hits(hits: Vec<VectorPointHit>) -> (Vec<Value>, Vec<Uuid>) {
    let mut memories = Vec::new();
    let mut retrieved_ids = Vec::<Uuid>::new();
    for hit in hits {
        if hit.score < SOGLIA_PERTINENZA {
            continue;
        }
        let correction_id = hit
            .payload
            .get("correction_id")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok());
        let text = hit
            .payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if text.is_empty() {
            continue;
        }
        if let Some(correction_id) = correction_id {
            retrieved_ids.push(correction_id);
        }
        memories.push(json!({
            "id": correction_id.map(|value| value.to_string()).unwrap_or_default(),
            "text": text,
            "score": hit.score,
            "intent": hit.payload.get("intent").and_then(Value::as_str).unwrap_or("chat"),
            "pointId": hit.point_id,
        }));
    }
    (memories, retrieved_ids)
}

/// Contabilizza il recupero delle memorie usate; l'errore resta ignorato
/// (contatore non critico per la risposta).
///
/// `prompt_corrections` vive nel DB del progetto (separazione DB); `settings` e
/// `project_learning_config` restano su meta, e la ricerca vettoriale e'
/// multi-tenant per payload.
async fn bump_retrieval(db: &PgPool, project_id: Uuid, retrieved_ids: &[Uuid]) {
    // Telemetria best-effort: DB progetto non disponibile -> update saltato con
    // WARN (niente fallback al meta).
    let cpool = match crate::project_db_routes::project_data_pool_from(db, project_id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(project_id = %project_id, error = %e, "memorie di progetto: DB progetto non disponibile, bump retrieved_count saltato");
            return;
        }
    };
    let _ = sqlx::query(
        r#"
        UPDATE prompt_corrections
        SET retrieved_count = retrieved_count + 1,
            last_retrieved_at = NOW(),
            updated_at = NOW()
        WHERE id = ANY($1)
        "#,
    )
    .bind(retrieved_ids)
    .execute(&cpool)
    .await;
}

#[cfg(test)]
pub(crate) mod recall_di_test {
    //! Store vettoriale in memoria, al posto di embedder + Qdrant.
    //!
    //! NON riscrive il filtro della produzione: lo chiede a
    //! [`vector_memory::prompt_correction_filter`] e lo APPLICA come farebbe il
    //! motore, condizione per condizione. Se la produzione smettesse di chiedere
    //! `active = true`, questo store restituirebbe anche le memorie disattivate e
    //! il test che le esclude fallirebbe - che e' esattamente il servizio che uno
    //! strumento di misura deve rendere (regola O). Una copia della regola scritta
    //! qui dentro resterebbe invece verde proprio in quel caso.

    use super::*;
    use std::cmp::Ordering;

    #[derive(Default)]
    pub(crate) struct RecallInMemoria {
        punti: Vec<(String, f64, Value)>,
    }

    impl RecallInMemoria {
        pub(crate) fn nuovo() -> Self {
            Self::default()
        }

        /// Indicizza un punto: `score` e' la similarita' che il motore
        /// restituirebbe per la domanda del test, `payload` e' quello scritto
        /// dalla compattazione (`project_id`, `active`, `text`, ...).
        pub(crate) fn con_punto(mut self, score: f64, payload: Value) -> Self {
            let id = format!("punto-{}", self.punti.len() + 1);
            self.punti.push((id, score, payload));
            self
        }
    }

    /// Il payload soddisfa tutte le condizioni `must` del filtro?
    fn soddisfa(filtro: &Value, payload: &Value) -> bool {
        let Some(must) = filtro.get("must").and_then(Value::as_array) else {
            return true;
        };
        must.iter().all(|condizione| {
            let Some(key) = condizione.get("key").and_then(Value::as_str) else {
                return true;
            };
            match (payload.get(key), condizione.pointer("/match/value")) {
                (Some(presente), Some(atteso)) => presente == atteso,
                _ => false,
            }
        })
    }

    #[async_trait::async_trait]
    impl MemoryRecall for RecallInMemoria {
        async fn hits(
            &self,
            _db: &PgPool,
            project_id: Uuid,
            _query: &str,
            top_k: u64,
        ) -> anyhow::Result<Vec<VectorPointHit>> {
            let filtro = vector_memory::prompt_correction_filter(project_id);
            let mut hits: Vec<VectorPointHit> = self
                .punti
                .iter()
                .filter(|(_, _, payload)| soddisfa(&filtro, payload))
                .map(|(point_id, score, payload)| VectorPointHit {
                    point_id: point_id.clone(),
                    score: *score,
                    payload: payload.clone(),
                })
                .collect();
            hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
            hits.truncate(top_k as usize);
            Ok(hits)
        }
    }
}
