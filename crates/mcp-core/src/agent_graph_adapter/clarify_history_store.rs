//! Adapter del trait [`nexus_agent_graph::runtime::ports::ClarifyHistoryPort`].
//!
//! IMPLEMENTA il detector clarification CROSS-RUN che chiude il loop email
//! (chat Beaty-Book): `clarify_or_expand` imposta `pending_clarify` -> il grafo
//! va a END -> il turno TERMINA (il run chiude `Completed`) -> il messaggio
//! successivo dell'utente avvia un RUN NUOVO con stato ricostruito da zero. Il
//! detector NON puo' vivere in `AgentState` (azzerato per-run): interroga il DB
//! della sessione UNA volta all'avvio del run.
//!
//! FONTE STRUTTURATA (regola M): i `nexus_agent_meta_steps` `kind='clarify'` dei
//! run della sessione (join `agent_runs.session_id`). L'ESISTENZA del meta_step
//! `kind='clarify'` E' la dichiarazione strutturata "questo turno ha posto una
//! domanda-chiarimento all'utente" — il canale d'esito di QUESTO tipo di turno:
//! un turno di clarify chiude `Completed` + `pending_clarify=true`, NON
//! `blocked_needs_input` (verificato: `clarify_or_expand` -> END -> `Completed`,
//! `pending_clarify` non e' un interrupt-resume), quindi il segnale dichiarato
//! del canale clarify e' il meta_step, non `agent_runs.status`. La DECISIONE di
//! contare deriva da quel segnale + dal payload strutturato (`payload->>'question'`);
//! la firma-testo (sha1 della domanda normalizzata) e' SOLO l'euristica di
//! loop-detection che decide se e' la STESSA domanda ripetuta (analogo di
//! `name|sha1` per i tool).
//!
//! POOL: la lettura gira sul pool del DOMINIO RUN (`run_db`, separazione DB
//! per-progetto): `agent_runs` e `nexus_agent_meta_steps` sono tabelle migrate al
//! DB del progetto (regola L, [`crate::project_db_routes`]). Il call site risolve
//! il pool via `project_data_pool_by_session_from` e lo passa qui.
//!
//! FAIL-OPEN (sicurezza, come [`super::escalation_port`]/[`super::billing_cooldown_port`]):
//! un guasto di lettura DB ritorna conteggio 0 (nessuna ripetizione nota -> asse
//! `RepeatedUserQuestion` mai attivo -> comportamento invariato sui run normali),
//! MAI un `PortError`. CONFINE (regola L): qui SOLO l'I/O; la firma e la DECISIONE
//! d'asse (soglia) restano nel punto unico
//! [`nexus_agent_graph::decisions::clarify_signature`] /
//! [`nexus_agent_graph::decisions::progress_controller`].

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use nexus_agent_graph::decisions::clarify_signature::clarify_signature;
use nexus_agent_graph::runtime::ports::{ClarifyHistoryPort, PortError};

/// Numero massimo di meta_step `kind='clarify'` recenti della sessione letti per
/// il conteggio. Cap prudente: il loop email si manifesta in pochi turni; oltre
/// questa finestra non serve guardare (e limita il costo della query). Non e' una
/// soglia di comportamento (quella e' `agent.loop.repeated_user_question_threshold`,
/// DB-driven), e' un tetto di lettura -> costante interna e' accettabile (regola G
/// riguarda config di comportamento/modelli, non i limiti tecnici di una query).
const CLARIFY_HISTORY_SCAN_LIMIT: i64 = 20;

/// Adapter [`ClarifyHistoryPort`] -> `nexus_agent_meta_steps` (kind='clarify')
/// join `agent_runs` (session_id), sul pool del dominio run.
pub struct PgClarifyHistoryStore {
    /// Pool Postgres del DOMINIO RUN (progetto): dove vivono `agent_runs` e
    /// `nexus_agent_meta_steps`. Risolto dal call site via il routing per-sessione.
    run_db: PgPool,
}

impl PgClarifyHistoryStore {
    /// Costruisce lo store sul pool del dominio run (progetto) risolto dal
    /// call site (separazione DB per-progetto, regola L).
    pub fn new(run_db: PgPool) -> Self {
        Self { run_db }
    }

    /// Legge le domande-chiarimento recenti della sessione (piu' recente prima):
    /// le `payload->>'question'` non nulle/non vuote degli ultimi meta_step
    /// `kind='clarify'` dei run della sessione. Best-effort: su errore DB ritorna
    /// lista vuota (fail-open). SOLA LETTURA.
    async fn recent_clarify_questions(&self, session_id: Uuid) -> Vec<String> {
        let rows: Result<Vec<(String,)>, sqlx::Error> = sqlx::query_as(
            "SELECT ms.payload->>'question' AS question \
             FROM nexus_agent_meta_steps ms \
             JOIN agent_runs ar ON ar.id = ms.run_id \
             WHERE ar.session_id = $1 \
               AND ms.kind = 'clarify' \
               AND ms.payload->>'question' IS NOT NULL \
               AND btrim(ms.payload->>'question') <> '' \
             ORDER BY ms.created_at DESC \
             LIMIT $2",
        )
        .bind(session_id)
        .bind(CLARIFY_HISTORY_SCAN_LIMIT)
        .fetch_all(&self.run_db)
        .await;
        match rows {
            Ok(rows) => rows.into_iter().map(|(q,)| q).collect(),
            Err(e) => {
                // Fail-open: un guasto di lettura non deve bloccare l'avvio del run.
                tracing::warn!(
                    target: "mcp_core::clarify_history_store",
                    session_id = %session_id,
                    error = %e,
                    "lettura storia clarify fallita (fail-open, conteggio 0)"
                );
                Vec::new()
            }
        }
    }

    /// Firma della domanda-chiarimento piu' recente della sessione (la "corrente"
    /// da cui parte la loop-detection: il messaggio utente in arrivo e' la
    /// RISPOSTA a questa domanda; se il run corrente la ri-porra' identica siamo
    /// in loop) + il conteggio delle sue occorrenze nella storia. `None`/0 se non
    /// c'e' alcuna domanda clarify nella sessione. PUNTO comodo per il call site
    /// che deve alimentare `AgentState::repeated_clarify_count` senza conoscere a
    /// priori la firma (all'avvio del run non sa ancora quale domanda porra').
    pub async fn latest_signature_and_repeat_count(
        &self,
        session_id: Uuid,
    ) -> (Option<String>, i64) {
        let questions = self.recent_clarify_questions(session_id).await;
        let Some(latest) = questions.first() else {
            return (None, 0);
        };
        let latest_sig = clarify_signature(latest);
        let count = questions
            .iter()
            .filter(|q| clarify_signature(q) == latest_sig)
            .count() as i64;
        (Some(latest_sig), count)
    }
}

#[async_trait]
impl ClarifyHistoryPort for PgClarifyHistoryStore {
    /// Conta i turni della sessione la cui domanda-chiarimento ha la STESSA firma
    /// di `current_question_signature`. Firma vuota -> 0 (nessuna domanda da
    /// contare). Regola M: conta il segnale strutturato (meta_step `kind='clarify'`
    /// con quella domanda), non la prosa. FAIL-OPEN: guasto DB -> `Ok(0)`.
    async fn repeated_clarify_count(
        &self,
        session_id: Uuid,
        current_question_signature: &str,
    ) -> Result<i64, PortError> {
        if current_question_signature.trim().is_empty() {
            return Ok(0);
        }
        let questions = self.recent_clarify_questions(session_id).await;
        let count = questions
            .iter()
            .filter(|q| clarify_signature(q) == current_question_signature)
            .count() as i64;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Ricrea lo schema minimale (agent_runs + nexus_agent_meta_steps) per i test
    /// di lettura: solo le colonne che la query usa.
    async fn create_schema(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE agent_runs ( \
                 id UUID PRIMARY KEY, \
                 session_id UUID NOT NULL \
             )",
        )
        .execute(pool)
        .await
        .expect("create agent_runs");
        sqlx::query(
            "CREATE TABLE nexus_agent_meta_steps ( \
                 id BIGSERIAL PRIMARY KEY, \
                 run_id UUID NOT NULL, \
                 kind TEXT NOT NULL, \
                 payload JSONB NOT NULL DEFAULT '{}'::jsonb, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW() \
             )",
        )
        .execute(pool)
        .await
        .expect("create nexus_agent_meta_steps");
    }

    /// Inserisce un run della sessione con un meta_step clarify che porta `question`.
    async fn insert_clarify(pool: &PgPool, session_id: Uuid, question: &str) {
        let run_id = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_runs (id, session_id) VALUES ($1, $2)")
            .bind(run_id)
            .bind(session_id)
            .execute(pool)
            .await
            .expect("insert run");
        sqlx::query(
            "INSERT INTO nexus_agent_meta_steps (run_id, kind, payload) \
             VALUES ($1, 'clarify', $2)",
        )
        .bind(run_id)
        .bind(json!({ "question": question }))
        .execute(pool)
        .await
        .expect("insert meta_step");
    }

    /// La stessa domanda ripetuta 3 volte nella sessione -> conteggio 3 per la
    /// firma della piu' recente. Firme di domande diverse non contano.
    #[sqlx::test]
    async fn conta_domande_identiche_della_sessione(pool: PgPool) {
        create_schema(&pool).await;
        let session = Uuid::new_v4();
        insert_clarify(&pool, session, "Qual e' la tua email di login?").await;
        insert_clarify(&pool, session, "  qual e' la TUA email di login? ").await; // == a meno di spazi/case
        insert_clarify(&pool, session, "Quale database vuoi usare?").await; // domanda diversa
        insert_clarify(&pool, session, "qual e' la tua email di login?").await; // == la piu' recente

        let store = PgClarifyHistoryStore::new(pool.clone());
        let (sig, count) = store.latest_signature_and_repeat_count(session).await;
        assert!(sig.is_some());
        assert_eq!(
            count, 3,
            "3 occorrenze della stessa domanda-email (variazioni di spazi/case collidono)"
        );

        // Il metodo del trait con la firma corrente esplicita ritorna lo stesso.
        let via_trait = store
            .repeated_clarify_count(session, sig.as_deref().unwrap())
            .await
            .expect("fail-open");
        assert_eq!(via_trait, 3);
    }

    /// Isolamento per sessione: le domande di un'ALTRA sessione non contano.
    #[sqlx::test]
    async fn isola_per_sessione(pool: PgPool) {
        create_schema(&pool).await;
        let session_a = Uuid::new_v4();
        let session_b = Uuid::new_v4();
        insert_clarify(&pool, session_a, "domanda sessione A").await;
        insert_clarify(&pool, session_b, "domanda sessione A").await; // stessa testo, altra sessione

        let store = PgClarifyHistoryStore::new(pool.clone());
        let (_, count) = store.latest_signature_and_repeat_count(session_a).await;
        assert_eq!(count, 1, "conta solo la domanda della sessione A");
    }

    /// Nessuna storia clarify -> firma None, conteggio 0 (asse mai attivo,
    /// comportamento invariato).
    #[sqlx::test]
    async fn nessuna_storia_conteggio_zero(pool: PgPool) {
        create_schema(&pool).await;
        let session = Uuid::new_v4();
        let store = PgClarifyHistoryStore::new(pool.clone());
        let (sig, count) = store.latest_signature_and_repeat_count(session).await;
        assert!(sig.is_none());
        assert_eq!(count, 0);
    }

    /// Firma vuota nel metodo del trait -> 0 (nessuna domanda da contare).
    #[sqlx::test]
    async fn firma_vuota_zero(pool: PgPool) {
        create_schema(&pool).await;
        let store = PgClarifyHistoryStore::new(pool.clone());
        let count = store
            .repeated_clarify_count(Uuid::new_v4(), "  ")
            .await
            .expect("ok");
        assert_eq!(count, 0);
    }

    /// FAIL-OPEN: senza le tabelle la query fallisce -> conteggio 0, mai un errore.
    #[sqlx::test]
    async fn fail_open_su_tabelle_assenti(pool: PgPool) {
        // NON creiamo lo schema: la query fallira'.
        let store = PgClarifyHistoryStore::new(pool.clone());
        let (sig, count) = store
            .latest_signature_and_repeat_count(Uuid::new_v4())
            .await;
        assert!(sig.is_none(), "fail-open: nessuna firma");
        assert_eq!(count, 0, "fail-open: conteggio 0, mai un panico/errore");
        let via_trait = store
            .repeated_clarify_count(Uuid::new_v4(), "deadbeef0000")
            .await
            .expect("fail-open: mai PortError");
        assert_eq!(via_trait, 0);
    }
}
