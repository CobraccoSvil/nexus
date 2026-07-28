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
use std::collections::HashMap;
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
        // `prompt_corrections` vive nel DB del progetto; `settings` e
        // `project_learning_config`, che il gate legge, restano su meta. La
        // risoluzione passa dal punto unico del pattern "pool o WARN": DB progetto
        // irraggiungibile -> contatori saltati, il turno prosegue con le sue
        // memorie.
        let cpool = crate::project_db_routes::project_data_pool_or_warn(
            db,
            project_id,
            "memorie di progetto: contatori di recupero",
        )
        .await;
        Self::load_con_pool(db, cpool.as_ref(), recall, project_id, query).await
    }

    /// Come [`load`](Self::load) ma col pool dei contatori gia' risolto.
    ///
    /// Separata per una ragione sola: e' l'ULTIMO confine esterno rimasto dentro
    /// la catena. Sopra c'e' `MemoryRecall`, che isola embedder e Qdrant; qui si
    /// isola "da quale DB si scrivono i contatori", che in produzione dipende
    /// dalla directory di routing e in un test non esiste. Cosi' un test misura
    /// gate, soglia, taglio E contabilizzazione su un DB vero, sostituendo il solo
    /// pezzo che non puo' avere - senza fabbricare l'input che vuole verificare
    /// (regola O).
    async fn load_con_pool(
        db: &PgPool,
        cpool: Option<&PgPool>,
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

        let (mut items, punti_richiamati) = collect_from_hits(hits);
        if let (false, Some(cpool)) = (punti_richiamati.is_empty(), cpool) {
            let righe = bump_retrieval(cpool, &punti_richiamati).await;
            // L'audit del turno porta l'id della RIGA, non piu' una stringa vuota:
            // lo conosce la query che ha contabilizzato, e costa zero perche' e' la
            // stessa. Resta vuoto per un punto senza riga corrispondente, che e' il
            // fatto da vedere e non da nascondere.
            for item in &mut items {
                let point_id = item
                    .get("pointId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let Some(id) = point_id.and_then(|point_id| righe.get(&point_id)) else {
                    continue;
                };
                item["id"] = json!(id.to_string());
            }
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

/// Memorie pertinenti a partire dagli hit, piu' i PUNTI da contabilizzare.
/// Scarta chi sta sotto [`SOGLIA_PERTINENZA`] e chi non ha testo.
///
/// Il recupero si contabilizza per `point_id`, che l'hit porta sempre, e non per
/// un campo scritto dentro il payload. ROOT CAUSE: qui si leggeva
/// `payload["correction_id"]`, e i tre produttori di quei punti lo scrivevano in
/// tre modi diversi - la compattazione non lo scriveva affatto, le correzioni
/// admin ci mettevano l'id del PUNTO invece di quello della riga. Per due delle
/// tre famiglie il contatore restava dunque a zero per sempre, e il pruner
/// notturno (`chat_learning`, ramo `unused_ttl`) disattivava dopo 90 giorni
/// memorie richiamate ogni giorno, cancellandone il punto vettoriale. Il legame
/// per punto vale per tutte e tre e non dipende da cosa ciascun produttore ha
/// scritto dentro al payload.
fn collect_from_hits(hits: Vec<VectorPointHit>) -> (Vec<Value>, Vec<String>) {
    let mut memories = Vec::new();
    let mut punti_richiamati = Vec::<String>::new();
    for hit in hits {
        if hit.score < SOGLIA_PERTINENZA {
            continue;
        }
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
        punti_richiamati.push(hit.point_id.clone());
        memories.push(json!({
            // Vuoto finche' il bump non risolve la riga: l'id vero e' quello di
            // `prompt_corrections`, e chi lo conosce e' la query che contabilizza.
            // Prima ci finiva `payload["correction_id"]`, che per le memorie di
            // sessione non esisteva e per le correzioni admin era l'id del punto:
            // un id che non identificava alcuna riga.
            "id": "",
            "text": text,
            "score": hit.score,
            "intent": hit.payload.get("intent").and_then(Value::as_str).unwrap_or("chat"),
            "pointId": hit.point_id,
        }));
    }
    (memories, punti_richiamati)
}

/// Contabilizza il recupero dei punti richiamati e restituisce, per ciascun
/// punto, l'id della riga contata.
///
/// L'aggancio e' `qdrant_point_id`, che ha un UNIQUE INDEX
/// (`db/migrations/project/0001_chat.sql`) ed e' scritto da tutti e tre i
/// produttori perche' e' l'id del punto stesso, non un campo che ognuno compila a
/// modo suo. Il contatore che qui si incrementa non e' telemetria: e' il criterio
/// con cui la compattazione notturna decide di disattivare una memoria e di
/// cancellarne il punto vettoriale (ramo `unused_ttl`), operazione irreversibile
/// lato Qdrant. Un bump che non arriva non e' una metrica mancante: e' una
/// memoria che l'utente perde.
///
/// L'errore SQL resta non fatale per la risposta del turno, ma viene loggato: un
/// fallimento silenzioso qui e' indistinguibile da "nessuna memoria richiamata",
/// che e' precisamente il modo in cui il difetto e' rimasto invisibile.
async fn bump_retrieval(cpool: &PgPool, punti_richiamati: &[String]) -> HashMap<String, Uuid> {
    let righe = sqlx::query_as::<_, (Uuid, String)>(
        r#"
        UPDATE prompt_corrections
        SET retrieved_count = retrieved_count + 1,
            last_retrieved_at = NOW(),
            updated_at = NOW()
        WHERE qdrant_point_id = ANY($1)
        RETURNING id, qdrant_point_id
        "#,
    )
    .bind(punti_richiamati)
    .fetch_all(cpool)
    .await;

    match righe {
        Ok(righe) => righe
            .into_iter()
            .map(|(id, point_id)| (point_id, id))
            .collect(),
        Err(error) => {
            tracing::warn!(
                punti = punti_richiamati.len(),
                error = %error,
                "memorie di progetto: contatori di recupero non aggiornati"
            );
            HashMap::new()
        }
    }
}

#[cfg(test)]
mod tests {
    //! Il richiamo di una memoria ne CONTABILIZZA il recupero.
    //!
    //! Non e' telemetria: `retrieved_count` e' il criterio con cui la compattazione
    //! notturna (`chat_learning`, ramo `unused_ttl`) disattiva una memoria e ne
    //! cancella il punto vettoriale dopo 90 giorni. Finche' il bump si agganciava a
    //! `payload["correction_id"]`, per le memorie di sessione - che quel campo non
    //! l'hanno mai avuto - il contatore restava a zero per sempre: il pannello
    //! "Memoria del progetto" perdeva voci richiamate ogni giorno.
    //!
    //! Percio' questi test non guardano il valore di ritorno del richiamo ma la
    //! CONSEGUENZA nel DB, sulla riga vera, con lo schema della migrazione reale.
    //! E il payload NON e' scritto qui: viene da
    //! `chat_sessions::payload_memoria_di_sessione`, cioe' dal produttore. Un test
    //! che se lo fabbrica resta verde proprio quando produttore e consumatore
    //! divergono - che e' il modo in cui questo difetto e' rimasto invisibile per
    //! l'intera vita della feature (regola O).

    use super::recall_di_test::RecallInMemoria;
    use super::*;
    use crate::test_support::{seed_chat_session, seed_memoria_di_sessione};
    use chrono::{DateTime, Utc};

    const DOMANDA: &str = "come si fa il deploy dello stack?";
    const MEMORIA: &str = "Il deploy locale si fa con deploy/deploy-local.ps1, mai a mano.";
    const ALTRA_MEMORIA: &str = "Le porte dei servizi si chiedono con request_port.";

    /// La memoria nasce inattiva e l'utente la attiva dal pannello: sul punto
    /// vettoriale l'effetto e' quello di `vector_memory::set_point_active`, cioe'
    /// un set PARZIALE del solo campo `active`. Il resto del payload resta quello
    /// del produttore.
    fn attivata_dal_pannello(mut payload: Value) -> Value {
        payload["active"] = json!(true);
        payload
    }

    /// Stato dei contatori di una riga, cioe' cio' che il pruner notturno legge.
    async fn contatori(pool: &PgPool, memory_id: Uuid) -> (i64, Option<DateTime<Utc>>) {
        sqlx::query_as(
            "SELECT retrieved_count, last_retrieved_at FROM prompt_corrections WHERE id = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await
        .expect("lettura contatori")
    }

    /// Semina una memoria attivata e ne restituisce (id di riga, punto indicizzabile).
    async fn memoria_attiva(pool: &PgPool, project_id: Uuid, text: &str) -> (Uuid, String, Value) {
        let session_id = seed_chat_session(pool, project_id).await;
        let point_id = Uuid::new_v4().to_string();
        let memory_id =
            seed_memoria_di_sessione(pool, project_id, session_id, &point_id, text).await;
        let payload = attivata_dal_pannello(crate::chat_sessions::payload_memoria_di_sessione(
            project_id, session_id, text,
        ));
        (memory_id, point_id, payload)
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn il_richiamo_di_una_memoria_di_sessione_ne_contabilizza_il_recupero(pool: PgPool) {
        let project_id = Uuid::new_v4();
        let (memory_id, point_id, payload) = memoria_attiva(&pool, project_id, MEMORIA).await;

        // Partenza dal DEFAULT dello schema: e' lo stato che il pruner considera
        // "mai richiamata". Senza questa asserzione un contatore gia' a 1 per altre
        // ragioni farebbe passare il test senza che il bump sia mai avvenuto.
        assert_eq!(
            contatori(&pool, memory_id).await,
            (0, None),
            "una memoria appena creata non risulta mai richiamata"
        );

        let recall = RecallInMemoria::nuovo().con_punto_id(&point_id, 0.91, payload);
        let memories =
            ProjectMemories::load_con_pool(&pool, Some(&pool), &recall, project_id, DOMANDA).await;

        assert_eq!(memories.len(), 1, "la memoria attiva non e' stata richiamata");

        let (count, last) = contatori(&pool, memory_id).await;
        assert_eq!(
            count, 1,
            "il richiamo non ha contabilizzato il recupero: con retrieved_count a zero \
             la compattazione notturna disattiva questa memoria dopo 90 giorni e ne \
             cancella il punto vettoriale, per quanto la si richiami ogni giorno"
        );
        assert!(
            last.is_some(),
            "last_retrieved_at non valorizzato: la riga resta indistinguibile da una mai usata"
        );
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn si_contabilizza_solo_cio_che_entra_davvero_nel_prompt(pool: PgPool) {
        let project_id = Uuid::new_v4();
        let (pertinente, punto_pertinente, payload_pertinente) =
            memoria_attiva(&pool, project_id, MEMORIA).await;
        let (lontana, punto_lontano, payload_lontano) =
            memoria_attiva(&pool, project_id, ALTRA_MEMORIA).await;

        let recall = RecallInMemoria::nuovo()
            .con_punto_id(&punto_pertinente, 0.91, payload_pertinente)
            // Sotto SOGLIA_PERTINENZA: richiamata dal motore, scartata dal gate.
            .con_punto_id(&punto_lontano, 0.42, payload_lontano);

        ProjectMemories::load_con_pool(&pool, Some(&pool), &recall, project_id, DOMANDA).await;

        assert_eq!(
            contatori(&pool, pertinente).await.0,
            1,
            "la memoria entrata nel prompt non e' stata contata"
        );
        // Prova che il bump segue la SELEZIONE e non l'elenco degli hit: contare
        // anche gli scartati terrebbe in vita memorie che non servono a nessuno.
        assert_eq!(
            contatori(&pool, lontana).await.0,
            0,
            "contata una memoria che il gate di pertinenza aveva scartato"
        );
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn l_audit_del_turno_porta_l_id_della_riga_contata(pool: PgPool) {
        let project_id = Uuid::new_v4();
        let (memory_id, point_id, payload) = memoria_attiva(&pool, project_id, MEMORIA).await;

        let recall = RecallInMemoria::nuovo().con_punto_id(&point_id, 0.91, payload);
        let memories =
            ProjectMemories::load_con_pool(&pool, Some(&pool), &recall, project_id, DOMANDA).await;

        // `applied_corrections`: prima ci finiva una stringa vuota per le memorie di
        // sessione (il payload non portava alcun id di riga) e per le correzioni
        // admin un id che non identificava alcuna riga.
        let voci = memories.into_values();
        let voce = &voci[0];
        assert_eq!(
            voce["id"].as_str(),
            Some(memory_id.to_string().as_str()),
            "l'audit del turno non porta l'id della riga davvero contabilizzata"
        );
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn senza_pool_di_progetto_il_turno_conserva_le_sue_memorie(pool: PgPool) {
        let project_id = Uuid::new_v4();
        let (_, point_id, payload) = memoria_attiva(&pool, project_id, MEMORIA).await;

        let recall = RecallInMemoria::nuovo().con_punto_id(&point_id, 0.91, payload);
        // DB del progetto irraggiungibile: i contatori si perdono, il turno no.
        let memories =
            ProjectMemories::load_con_pool(&pool, None, &recall, project_id, DOMANDA).await;

        assert_eq!(
            memories.len(),
            1,
            "un contatore non scrivibile non deve togliere all'utente le sue memorie"
        );
    }
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

        /// Come [`con_punto`](Self::con_punto) ma con l'id del punto ESPLICITO.
        ///
        /// Serve ai test che verificano la contabilizzazione del recupero: quella
        /// passa da `qdrant_point_id`, quindi l'id del punto e' cio' che lega
        /// l'hit alla riga di `prompt_corrections`. Con un id inventato dallo
        /// store il bump non troverebbe nulla e il test misurerebbe il vuoto.
        pub(crate) fn con_punto_id(mut self, point_id: &str, score: f64, payload: Value) -> Self {
            self.punti.push((point_id.to_string(), score, payload));
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
