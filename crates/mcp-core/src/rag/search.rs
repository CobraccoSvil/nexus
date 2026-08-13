//! Search semantica RAG: embed query + search Qdrant filtrato.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::collezioni::{collection_del_kind, Scrittore};
use super::qdrant_client::EsitoRicerca;
use super::{current_config, qdrant_client, RagError, SourceKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub source_kind: String,
    pub source_id: String,
    pub chunk_index: i64,
    pub chunk_text: String,
    pub score: f32,
    pub metadata: Value,
}

/// Embedding della query tramite l'embedder ONNX in-process del bridge
/// (regola L: punto unico, niente round-trip HTTP/gRPC verso il brain Python).
/// `embed_one` e' sincrono/CPU-bound, quindi viene avvolto in `spawn_blocking`.
async fn embed_query(query: &str) -> Result<Vec<f32>, RagError> {
    let bridge = crate::nexus_bridge::NexusBridge::global()
        .ok_or_else(|| RagError::Embed("nexus bridge non inizializzato".into()))?;
    let q = query.to_string();
    tokio::task::spawn_blocking(move || bridge.embed_one(&q))
        .await
        .map_err(|e| RagError::Embed(format!("embed_query spawn_blocking join: {e}")))
}

/// Cosa e' successo interrogando UNA fonte.
///
/// Le tre varianti hanno rimedi diversi e prima erano due sole — «ho degli
/// hit» e «e' andata male, ecco la stringa» — con l'assenza della collection
/// nascosta dentro la seconda. Un'assenza non si risolve riprovando: o lo
/// scrittore non ha ancora scritto nulla, o si sta leggendo dove nessuno
/// scrive (regola Q).
#[derive(Debug, Clone, PartialEq)]
pub enum Esito {
    /// La collection ha risposto (anche con zero hit: quello e' un «non c'e'»).
    Interrogata { hits: usize },
    /// La collection non esiste. Il campo dice CHI dovrebbe averla creata:
    /// senza, la diagnosi manda a cercare a caso.
    CollectionAssente { scrittore: Scrittore },
    /// Qdrant ha risposto un guasto, o non ha risposto affatto.
    NonInterrogabile { errore: String },
}

impl Esito {
    /// True se la fonte ha davvero risposto: solo qui uno zero significa
    /// «cercato e non trovato».
    pub fn ha_risposto(&self) -> bool {
        matches!(self, Esito::Interrogata { .. })
    }
}

/// L'esito di UNA fonte, con la collection che l'ha (o non l'ha) servita.
#[derive(Debug, Clone)]
pub struct EsitoDelKind {
    pub kind: String,
    pub collection: String,
    pub esito: Esito,
}

impl EsitoDelKind {
    /// Costruttore unico dei tre esiti del ciclo di ricerca.
    ///
    /// Il `kind` diventa la sua forma di wire in UN punto: prima i tre rami
    /// ripetevano `kind.as_str().to_string()`, e la ripetizione costava anche
    /// due livelli di annidamento — l'`Esito` costruito dentro l'inizializzatore
    /// della struct dentro il ramo dentro il ciclo.
    fn nuovo(kind: SourceKind, collection: String, esito: Esito) -> Self {
        Self {
            kind: kind.as_str().to_string(),
            collection,
            esito,
        }
    }
}

/// Esito della ricerca semantica: gli hit E, per ogni fonte interrogata, che
/// cosa e' successo.
///
/// Una collection che non risponde non e' "zero risultati" (regola M): prima
/// questo esito veniva inghiottito con un `warn!` e il chiamante — incluso il
/// modello che decide la prossima mossa — vedeva `count: 0` identico a
/// "cercato e non trovato". Misurato il 31/07/2026: il correttore post-review
/// ha ripetuto 8 ricerche sul codice contro una collection inesistente,
/// leggendo ogni volta uno zero che sembrava una risposta, fino alla chiusura
/// per loop.
#[derive(Debug, Default)]
pub struct SemanticSearchReport {
    pub hits: Vec<SearchHit>,
    /// Un elemento per ogni kind interrogato, nell'ordine di interrogazione.
    pub esiti: Vec<EsitoDelKind>,
}

impl SemanticSearchReport {
    /// Le fonti che NON hanno potuto rispondere (assenti o in guasto): il
    /// perimetro su cui un chiamante decide se lo zero e' credibile.
    pub fn non_hanno_risposto(&self) -> impl Iterator<Item = &EsitoDelKind> {
        self.esiti.iter().filter(|e| !e.esito.ha_risposto())
    }
}

/// I kind interrogati quando il chiamante non ne specifica: tutte le fonti
/// per-progetto CHE QUALCUNO POPOLA, incluso il codice. Code ne era escluso
/// quando la sua collection era un nome mai esistito ("code_embeddings"):
/// tolta la faglia, escluderlo renderebbe la ricerca cieca proprio sui
/// sorgenti — la domanda piu' frequente di un run di correzione.
///
/// `Kb` NON e' qui, ma la RAGIONE non e' piu' quella per cui ne fu tolto il
/// 10/08/2026 («la sua collection non ha alcuno scrittore»): quella premessa
/// oggi e' falsa. `Kb` legge la collection del wiki (vedi
/// [`super::collezioni`]), che uno scrittore ce l'ha ed e' popolata — MISURATO
/// il 13/08/2026: 6733 punti di scope `project`.
///
/// Resta fuori dai default per un motivo DIVERSO e misurato: il payload del
/// wiki porta il TITOLO del documento, non il chunk (il corpo vive in
/// `wiki_docs`), quindi nei default competerebbe per gli stessi top-K con
/// chunk di codice e allegati che il testo intero ce l'hanno — diluendoli con
/// righe che da sole non bastano a rispondere.
///
/// Interrogabile a richiesta esplicita, ed e' cosi' che la usa chi la vuole:
/// il recall del mandato chiede `kb,code` per configurazione
/// (`agent.subagent.mandate_recall_kinds`), e la famiglia `knowledge_*`
/// interroga la stessa collection idratando il corpo da Postgres.
pub(crate) fn default_kinds() -> Vec<SourceKind> {
    vec![
        SourceKind::Attachment,
        SourceKind::ChatHistory,
        SourceKind::ToolResult,
        SourceKind::Code,
    ]
}

/// I filtri di UNA interrogazione, nell'ordine in cui vengono applicati.
///
/// I filtri del KIND vengono PRIMA di quelli del chiamante e non sono
/// un'opzione: sono cio' che isola la sorgente dentro una collection condivisa
/// con altre (il wiki porta meta e progetto insieme). Sta in una funzione, e
/// non inline nel ciclo, perche' l'invariante «il filtro del kind c'e' sempre»
/// sia verificabile dove la produzione lo costruisce (regola O).
fn filtri_della_interrogazione(
    risolta: &super::collezioni::CollectionDelKind,
    kind: SourceKind,
    project_id: Option<Uuid>,
    session_id: Option<Uuid>,
    extra: &[(String, Value)],
) -> Vec<(String, Value)> {
    let mut filters: Vec<(String, Value)> = risolta.filtri_kind.clone();
    // Non tutte le sorgenti portano `project_id` nel payload: Conversation usa
    // `session_id`, MetaDoc e' globale dentro lo scope `meta`.
    if let Some(p) = project_id {
        if kind.supports_project_filter() {
            filters.push(("project_id".to_string(), json!(p.to_string())));
        }
    }
    if let Some(s) = session_id {
        if kind.uses_session_filter() {
            filters.push(("session_id".to_string(), json!(s.to_string())));
        }
    }
    filters.extend(extra.iter().cloned());
    filters
}

/// Cerca i top-K chunk piu' rilevanti per `query` filtrati su:
/// - `source_kinds`: lista di SourceKind ammessi (default: [`default_kinds`]).
/// - `project_id`: se Some, filtra payload.project_id.
/// - `session_id`: se Some, filtra payload.session_id (rilevante per chat_history).
/// - `extra_filters`: ulteriori filtri payload arbitrari (es. ("source_id", "<uuid>")).
pub async fn search_semantic(
    db: &PgPool,
    query: &str,
    source_kinds: Vec<SourceKind>,
    project_id: Option<Uuid>,
    session_id: Option<Uuid>,
    top_k: Option<usize>,
    extra_filters: Vec<(String, Value)>,
) -> Result<SemanticSearchReport, RagError> {
    let cfg = current_config(db).await?;
    if !cfg.enabled {
        return Err(RagError::Disabled);
    }
    if query.trim().is_empty() {
        return Ok(SemanticSearchReport::default());
    }
    let top_k = top_k.unwrap_or(cfg.top_k_default).clamp(1, 100);
    let kinds = if source_kinds.is_empty() {
        default_kinds()
    } else {
        source_kinds
    };

    let http = Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| RagError::Embed(format!("reqwest: {e}")))?;
    let query_vec = embed_query(query).await?;

    let mut all_hits: Vec<SearchHit> = Vec::new();
    let mut esiti: Vec<EsitoDelKind> = Vec::new();
    for kind in kinds {
        // Nome, scrittore e filtri di isolamento dal punto unico: qui non si
        // incide nessun nome di collection (vedi `rag::collezioni`).
        let risolta = collection_del_kind(&cfg, kind);
        let collection = risolta.nome.clone();
        let filters = filtri_della_interrogazione(
            &risolta,
            kind,
            project_id,
            session_id,
            &extra_filters,
        );
        let hits = match qdrant_client::search_points(
            &http,
            &cfg.qdrant_url,
            &collection,
            query_vec.clone(),
            top_k,
            filters,
        )
        .await
        {
            Ok(EsitoRicerca::Hits(h)) => h,
            Ok(EsitoRicerca::CollectionAssente) => {
                // Un'assenza si dichiara col suo scrittore: e' l'unica
                // informazione che rende la diagnosi azionabile, e la sua
                // mancanza e' il motivo per cui `kb_chunks` e' rimasta un WARN
                // ripetuto a ogni run per due mesi.
                tracing::warn!(
                    kind = kind.as_str(),
                    collection = %collection,
                    scrittore = risolta.scrittore.punto(),
                    "rag.search_semantic: collection assente su Qdrant"
                );
                let esito = Esito::CollectionAssente {
                    scrittore: risolta.scrittore,
                };
                esiti.push(EsitoDelKind::nuovo(kind, collection, esito));
                continue;
            }
            Err(e) => {
                tracing::warn!(
                    kind = kind.as_str(),
                    collection = %collection,
                    errore = %e,
                    "rag.search_semantic: collection non interrogabile"
                );
                // Il fallimento viaggia col risultato, non solo nel log: per
                // il chiamante "collection irraggiungibile" e "cercato e non
                // trovato" sono esiti DIVERSI (regola M).
                let esito = Esito::NonInterrogabile {
                    errore: e.to_string(),
                };
                esiti.push(EsitoDelKind::nuovo(kind, collection, esito));
                continue;
            }
        };
        let esito = Esito::Interrogata { hits: hits.len() };
        esiti.push(EsitoDelKind::nuovo(kind, collection, esito));
        for h in hits {
            let p = h.payload;
            // Estrazione testo flessibile: il RAG framework usa `chunk_text`,
            // ma le collection di cui e' ospite hanno schemi diversi
            // (conversation_context -> `content`, il wiki -> `title` col corpo
            // in `wiki_docs`, prompt_corrections -> `correction`/`text`).
            // Proviamo in ordine.
            let chunk_text = p
                .get("chunk_text")
                .or_else(|| p.get("content"))
                .or_else(|| p.get("body_md"))
                .or_else(|| p.get("correction"))
                .or_else(|| p.get("text"))
                .or_else(|| p.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let chunk_index = p.get("chunk_index").and_then(|v| v.as_i64()).unwrap_or(0);
            // source_id: prova source_id, poi note_id/id specifici delle legacy.
            let source_id = p
                .get("source_id")
                .or_else(|| p.get("note_id"))
                .or_else(|| p.get("doc_id"))
                // project_code_index identifica i chunk col percorso del file:
                // un hit di codice senza il SUO file non e' azionabile.
                .or_else(|| p.get("file_path"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let metadata = p.get("metadata").cloned().unwrap_or(Value::Null);
            all_hits.push(SearchHit {
                source_kind: kind.as_str().to_string(),
                source_id,
                chunk_index,
                chunk_text,
                score: h.score,
                metadata,
            });
        }
    }
    all_hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_hits.truncate(top_k);
    let mute = esiti.iter().filter(|e| !e.esito.ha_risposto()).count();
    tracing::info!(
        "rag.search_semantic: query_len={} hits={} fonti={} senza_risposta={}",
        query.chars().count(),
        all_hits.len(),
        esiti.len(),
        mute
    );
    Ok(SemanticSearchReport {
        hits: all_hits,
        esiti,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Il default include il CODICE: e' la fonte che un run di correzione
    /// interroga piu' spesso. Ne era escluso quando la sua collection era un
    /// nome mai esistito; senza questo assert, l'esclusione potrebbe tornare
    /// in silenzio e la ricerca risponderebbe di nuovo zero sui sorgenti.
    #[test]
    fn i_kind_di_default_includono_il_codice() {
        let kinds = default_kinds();
        assert!(
            kinds.contains(&SourceKind::Code),
            "default_kinds deve includere Code: {kinds:?}"
        );
    }

    /// Il filtro del KIND raggiunge la query, e ci arriva PRIMA di quelli del
    /// chiamante. Passa dalla funzione che la produzione usa davvero (regola O):
    /// costruire la lista a mano nel test fisserebbe proprio l'assunto da
    /// verificare.
    ///
    /// MUTAZIONE: togliendo `risolta.filtri_kind` da
    /// `filtri_della_interrogazione`, questo test fallisce — ed e' il caso in
    /// cui `kb` leggerebbe anche i documenti del vault e `meta_doc` quelli di
    /// ogni progetto.
    #[test]
    fn il_filtro_del_kind_arriva_alla_query() {
        let risolta = super::super::collezioni::CollectionDelKind {
            nome: "wiki_content".into(),
            scrittore: Scrittore::Esterno { punto: "wiki" },
            filtri_kind: vec![("scope".to_string(), json!("project"))],
        };
        let progetto = Uuid::nil();
        let filtri = filtri_della_interrogazione(
            &risolta,
            SourceKind::Kb,
            Some(progetto),
            None,
            &[("source_id".to_string(), json!("x"))],
        );
        assert_eq!(
            filtri.first(),
            Some(&("scope".to_string(), json!("project"))),
            "il filtro del kind precede tutti: {filtri:?}"
        );
        assert!(filtri.contains(&("project_id".to_string(), json!(progetto.to_string()))));
        assert!(filtri.contains(&("source_id".to_string(), json!("x"))));
    }

    /// Una fonte assente e una in guasto non collassano nello stesso «non ha
    /// risposto»: le due cause hanno rimedi opposti, e solo la seconda si
    /// risolve riprovando.
    #[test]
    fn le_fonti_mute_si_distinguono_dalle_altre() {
        let report = SemanticSearchReport {
            hits: Vec::new(),
            esiti: vec![
                EsitoDelKind {
                    kind: "code".into(),
                    collection: "project_code_index".into(),
                    esito: Esito::Interrogata { hits: 0 },
                },
                EsitoDelKind {
                    kind: "kb".into(),
                    collection: "wiki_content".into(),
                    esito: Esito::CollectionAssente {
                        scrittore: Scrittore::Esterno { punto: "wiki" },
                    },
                },
            ],
        };
        let mute: Vec<&str> = report
            .non_hanno_risposto()
            .map(|m| m.kind.as_str())
            .collect();
        assert_eq!(
            mute,
            vec!["kb"],
            "uno zero da una fonte che HA risposto non e' una fonte muta"
        );
    }
}
