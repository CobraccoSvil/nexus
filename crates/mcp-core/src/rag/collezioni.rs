//! «Quale collection Qdrant risponde per questo [`SourceKind`], e CHI la
//! scrive?» — PUNTO UNICO (regola L) della coppia nome + scrittore.
//!
//! # Perche' le due domande stanno insieme
//!
//! Il RAG interroga otto sorgenti e ne SCRIVE tre. Per le altre cinque e' un
//! lettore ospite: la collection la crea e la popola qualcun altro, che ne
//! risolve il nome da una chiave sua. Finche' il nome era un letterale scritto
//! qui, «leggo da X» e «qualcuno scrive in X» erano due affermazioni
//! indipendenti, e una poteva restare vera mentre l'altra diventava falsa senza
//! che nulla fallisse: una ricerca su una collection inesistente non e' un
//! errore del run, e' uno ZERO — indistinguibile da «cercato e non trovato».
//!
//! Col nome e lo scrittore nella STESSA struttura, una collection senza
//! scrittore non e' piu' rappresentabile: per nominarla bisogna dire chi la
//! produce, e il test la confronta con cio' che quello scrittore risolve
//! davvero (regola O).
//!
//! # La misura
//!
//! Il difetto e' stato osservato TRE volte sulla stessa faglia, e le prime due
//! sono state chiuse una istanza alla volta:
//!
//! - `Code` -> `code_embeddings`, collection mai esistita (misurato il
//!   31/07/2026: 404 sulla prima, 111 punti in `project_code_index`).
//! - `Kb` -> `kb_chunks`. Lo scrittore ESISTEVA — `knowledge_create_note`
//!   indicizzava ogni nota di progetto con `index_text(SourceKind::Kb, ...)` —
//!   ed e' stato RIMOSSO dal commit `eb5e47a5` (knowledge graph unificato,
//!   ADR 0017 v2, 04/06/2026), che ha spostato le note in `wiki_docs` +
//!   `wiki_content`. Il lettore e' rimasto sul nome di prima. MISURATO il
//!   13/08/2026 su Qdrant vivo: dieci collection, `kb_chunks` non fra queste, e
//!   nel log di mcp-core cinque `404 Collection kb_chunks doesn't exist` in 116
//!   millisecondi — una per figura convocata, a ogni run, perche'
//!   `agent.subagent.mandate_recall_kinds` vale `kb,code`: meta' del richiamo
//!   del mandato era morta e l'unico segnale era un WARN che nessuno legge.
//! - `MetaDoc` -> `nexus_meta_docs`, il nome PRE-unificazione della stessa
//!   collection del wiki (la tabella omonima l'ha rimossa la mig 0295).
//!
//! # Perche' non si CREA `kb_chunks`
//!
//! Crearla dove si creano le altre farebbe sparire il 404 e lascerebbe una
//! collection vuota per sempre, perche' nessuno scrive piu' li'. Lo zero
//! tornerebbe a essere indistinguibile da «non trovato» — la forma silenziosa
//! dello stesso difetto, che in questo deployment si vede gia': `project_docs`
//! esiste con ZERO punti. Il contenuto del knowledge base non e' sparito: sono
//! 6762 punti in `wiki_content` (6733 di scope `project`, 29 `meta`), scritti
//! dal wiki. Il RAG deve leggere DI LA'.
//!
//! # Lo scope non e' un dettaglio
//!
//! `wiki_content` non e' partizionata: meta e progetto stanno insieme e si
//! distinguono per payload. Un kind che la interrogasse senza il filtro di
//! scope leggerebbe i documenti degli ALTRI progetti (regola E). Chiave e
//! valore vengono dal wiki (`CHIAVE_SCOPE`, [`WikiScope::as_str`]), gli stessi
//! due punti da cui li prende `knowledge.rs::project_qdrant_filter`.

use serde_json::{json, Value};

use nexus_wiki::content_points::CHIAVE_SCOPE;
use nexus_wiki::model::WikiScope;

use super::config::RagConfig;
use super::SourceKind;

/// Chi PRODUCE i punti di una collection.
///
/// Non e' documentazione: e' il campo che rende impossibile nominare una
/// collection senza dire chi la riempie, ed e' il discriminante con cui
/// l'indexer rifiuta di scrivere in casa d'altri.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scrittore {
    /// L'indexer di questo modulo ([`super::indexer::index_text`]), che la CREA
    /// alla prima scrittura. Qui il nome e' davvero configurazione: lettore e
    /// scrittore sono la stessa funzione, quindi non possono divergere.
    IndexerRag,
    /// Un punto unico FUORI dal RAG. Il nome lo risolve LUI; qui si legge e
    /// basta, e la collection non si crea mai (crearla vuota mentirebbe).
    Esterno {
        /// Il modulo autoritativo, per chi legge una diagnosi e deve sapere
        /// dove andare.
        punto: &'static str,
    },
}

impl Scrittore {
    /// Il modulo che produce i punti, in forma leggibile.
    pub fn punto(&self) -> &'static str {
        match self {
            Scrittore::IndexerRag => "mcp-core::rag::indexer::index_text",
            Scrittore::Esterno { punto } => punto,
        }
    }

    /// True se e' il RAG a scrivere (e quindi a poter creare la collection).
    pub fn e_del_rag(&self) -> bool {
        matches!(self, Scrittore::IndexerRag)
    }
}

const SCRITTORE_WIKI: Scrittore = Scrittore::Esterno {
    punto: "nexus-wiki::content_points (wiki_docs -> wiki_content)",
};
const SCRITTORE_CODICE: Scrittore = Scrittore::Esterno {
    punto: "mcp-core::vector_memory (indice di codice)",
};
const SCRITTORE_CONVERSAZIONE: Scrittore = Scrittore::Esterno {
    punto: "mcp-core::vector_memory (contesto conversazione)",
};
const SCRITTORE_CORREZIONI: Scrittore = Scrittore::Esterno {
    punto: "mcp-core::vector_memory (correzioni di prompt)",
};

/// La collection che risponde per un kind: il nome, chi lo scrive, e i filtri
/// che ISOLANO il kind dentro una collection condivisa con altri.
#[derive(Clone, Debug)]
pub struct CollectionDelKind {
    pub nome: String,
    pub scrittore: Scrittore,
    /// Filtri di payload sempre applicati per questo kind, in aggiunta a quelli
    /// del chiamante. Vuoto quando la collection e' dedicata.
    pub filtri_kind: Vec<(String, Value)>,
}

/// Il filtro di scope del wiki, dai due punti unici del wiki stesso.
fn filtro_scope(scope: WikiScope) -> Vec<(String, Value)> {
    vec![(CHIAVE_SCOPE.to_string(), json!(scope.as_str()))]
}

/// La collection di un kind, risolta dalla config gia' caricata.
///
/// La config porta i nomi delle collection ESTERNE gia' risolti dai rispettivi
/// scrittori (vedi [`RagConfig`]): qui non si legge nessuna chiave e non si
/// incide nessun letterale.
pub fn collection_del_kind(cfg: &RagConfig, kind: SourceKind) -> CollectionDelKind {
    match kind {
        SourceKind::Attachment => CollectionDelKind {
            nome: cfg.collection_attachments.clone(),
            scrittore: Scrittore::IndexerRag,
            filtri_kind: Vec::new(),
        },
        SourceKind::ChatHistory => CollectionDelKind {
            nome: cfg.collection_chat_history.clone(),
            scrittore: Scrittore::IndexerRag,
            filtri_kind: Vec::new(),
        },
        SourceKind::ToolResult => CollectionDelKind {
            nome: cfg.collection_tool_results.clone(),
            scrittore: Scrittore::IndexerRag,
            filtri_kind: Vec::new(),
        },
        SourceKind::Code => CollectionDelKind {
            nome: cfg.collection_code.clone(),
            scrittore: SCRITTORE_CODICE,
            filtri_kind: Vec::new(),
        },
        // I due kind del wiki condividono UNA collection e si distinguono solo
        // per scope: senza il filtro, `meta_doc` restituirebbe i documenti di
        // ogni progetto e `kb` i documenti del vault.
        SourceKind::Kb => CollectionDelKind {
            nome: cfg.collection_wiki.clone(),
            scrittore: SCRITTORE_WIKI,
            filtri_kind: filtro_scope(WikiScope::Project),
        },
        SourceKind::MetaDoc => CollectionDelKind {
            nome: cfg.collection_wiki.clone(),
            scrittore: SCRITTORE_WIKI,
            filtri_kind: filtro_scope(WikiScope::Meta),
        },
        SourceKind::Conversation => CollectionDelKind {
            nome: cfg.collection_conversation.clone(),
            scrittore: SCRITTORE_CONVERSAZIONE,
            filtri_kind: Vec::new(),
        },
        SourceKind::PromptCorrection => CollectionDelKind {
            nome: cfg.collection_corrections.clone(),
            scrittore: SCRITTORE_CORREZIONI,
            filtri_kind: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rag::config::config_dal_db;

    /// L'INVARIANTE: per OGNI kind il lettore cerca dove lo scrittore scrive.
    ///
    /// Il confronto passa dalle funzioni reali degli scrittori sullo stesso DB
    /// migrato — non da letterali ricopiati qui (regola O): un rename lato
    /// scrittore rosseggia qui invece di produrre un 404 in esercizio.
    ///
    /// MUTAZIONE: riportando `Kb` a un nome inciso (`"kb_chunks"`) questo test
    /// fallisce col valore del difetto reale.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn ogni_kind_cerca_dove_il_suo_scrittore_scrive(pool: sqlx::PgPool) {
        let cfg = config_dal_db(&pool).await.expect("config RAG dal DB migrato");

        let wiki = nexus_wiki::content_points::wiki_content_collection(&pool)
            .await
            .expect("nome collection wiki dallo scrittore");
        for kind in [SourceKind::Kb, SourceKind::MetaDoc] {
            let risolta = collection_del_kind(&cfg, kind);
            assert_eq!(
                risolta.nome, wiki,
                "{}: il RAG deve leggere dove il wiki scrive",
                kind.as_str()
            );
            assert_eq!(risolta.scrittore, SCRITTORE_WIKI);
        }

        let codice = crate::vector_memory::code_index_collection(&pool).await;
        assert_eq!(collection_del_kind(&cfg, SourceKind::Code).nome, codice);

        let conversazione = crate::vector_memory::conversation_context_collection_name(&pool)
            .await
            .expect("nome collection conversazione dallo scrittore");
        assert_eq!(
            collection_del_kind(&cfg, SourceKind::Conversation).nome,
            conversazione
        );

        let correzioni = crate::vector_memory::prompt_corrections_collection_name(&pool)
            .await
            .expect("nome collection correzioni dallo scrittore");
        assert_eq!(
            collection_del_kind(&cfg, SourceKind::PromptCorrection).nome,
            correzioni
        );
    }

    /// I nomi delle tre collection MAI esistite non devono riapparire: erano
    /// tutte e tre plausibili, ed e' esattamente per questo che sono
    /// sopravvissute mesi.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn nessun_kind_nomina_una_collection_senza_scrittore(pool: sqlx::PgPool) {
        let cfg = config_dal_db(&pool).await.expect("config RAG dal DB migrato");
        for kind in SourceKind::TUTTI {
            let risolta = collection_del_kind(&cfg, kind);
            assert!(
                !risolta.nome.trim().is_empty(),
                "{}: nome collection vuoto",
                kind.as_str()
            );
            for morta in ["kb_chunks", "nexus_meta_docs", "code_embeddings"] {
                assert_ne!(
                    risolta.nome,
                    morta,
                    "{}: nomina una collection che nessuno scrive",
                    kind.as_str()
                );
            }
        }
    }

    /// I due kind del wiki leggono la stessa collection e NON gli stessi punti.
    ///
    /// MUTAZIONE: togliendo `filtri_kind` a uno dei due, questo test fallisce —
    /// ed e' il caso in cui `meta_doc` restituirebbe i 6733 punti di scope
    /// `project` di ogni progetto.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn kb_e_metadoc_non_leggono_gli_stessi_punti(pool: sqlx::PgPool) {
        let cfg = config_dal_db(&pool).await.expect("config RAG dal DB migrato");
        let kb = collection_del_kind(&cfg, SourceKind::Kb);
        let meta = collection_del_kind(&cfg, SourceKind::MetaDoc);
        assert_eq!(kb.nome, meta.nome, "condividono la collection");
        assert_ne!(
            kb.filtri_kind, meta.filtri_kind,
            "e devono distinguersi per filtro di scope"
        );
        assert_eq!(
            kb.filtri_kind,
            vec![(CHIAVE_SCOPE.to_string(), json!(WikiScope::Project.as_str()))]
        );
        assert_eq!(
            meta.filtri_kind,
            vec![(CHIAVE_SCOPE.to_string(), json!(WikiScope::Meta.as_str()))]
        );
    }

    /// L'indexer del RAG scrive SOLO dove e' lui lo scrittore. Le collection
    /// altrui si leggono e basta: scriverci dentro ne romperebbe il payload,
    /// crearle vuote mentirebbe.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn solo_tre_kind_sono_scritti_dal_rag(pool: sqlx::PgPool) {
        let cfg = config_dal_db(&pool).await.expect("config RAG dal DB migrato");
        let del_rag: Vec<&str> = SourceKind::TUTTI
            .into_iter()
            .filter(|k| collection_del_kind(&cfg, *k).scrittore.e_del_rag())
            .map(|k| k.as_str())
            .collect();
        assert_eq!(del_rag, vec!["attachment", "chat_history", "tool_result"]);
    }
}
