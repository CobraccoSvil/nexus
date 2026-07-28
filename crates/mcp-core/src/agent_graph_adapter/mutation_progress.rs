//! Adapter del trait [`nexus_agent_graph::runtime::ports::MutationProgressPort`].
//!
//! Porta al ReviewGate i FATTI delle scritture registrate in `file_mutations`
//! (mig 0349): gli hash del contenuto prima e dopo, cosi' come sono stati
//! scritti da `crate::file_mutations::record_mutation`. Nient'altro.
//!
//! ## Perche' la query non filtra per hash diversi
//!
//! Sarebbe una riga di SQL in meno per il chiamante (`WHERE before_sha256 IS
//! DISTINCT FROM after_sha256`) e il criterio finirebbe per vivere in due posti:
//! qui e nel modulo puro `decisions::correction_progress` (regola L). Il costo di
//! quella comodita' e' concreto: una riscrittura a contenuto identico sparirebbe
//! prima di essere contata come tale, e il gate non potrebbe piu' distinguere
//! "non ha scritto niente" da "ha riscritto file identici" — che sono due
//! comportamenti diversi dell'agente e vanno detti come tali nel rimando.
//!
//! ## Perche' il filtro e' la SESSIONE e non il run
//!
//! Le scritture di un sub-run sono registrate col `run_id` del SUB-RUN (vedi
//! `agent_tools::context`: e' cio' che permette di attribuire una violazione di
//! `write_scope` al passo di piano che l'ha commessa). Se il gate cercasse le
//! correzioni sotto il solo `run_id` del padre, un agente che corregge delegando
//! a un coder risulterebbe fermo: falso negativo che sopprimerebbe la ri-review
//! di un lavoro fatto davvero — l'errore opposto, e piu' grave, di quello che la
//! misura chiude.
//!
//! I sub-run ereditano la `session_id` del padre (`subagent_native`: "STESSA
//! session_id del parent"), quindi la sessione li contiene tutti. Il join esatto
//! con `nexus_subagent_runs.dispatcher_run_id` (punto unico della discendenza,
//! `run_lineage`) non e' possibile in una query sola: quella tabella vive nel DB
//! del PROGETTO, `file_mutations` nel DB META. La sessione e' il confine piu'
//! stretto disponibile da un lato solo, e il watermark fa il resto del lavoro:
//! la finestra e' "dopo il rimando", non "in questa sessione".

use async_trait::async_trait;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use nexus_agent_graph::decisions::WriteFact;
use nexus_agent_graph::runtime::ports::{MutationProgressPort, PortError, WriteScan};

/// Tetto di righe per scansione. Non e' un campione: la finestra e' delimitata
/// dal watermark (le scritture di UN ciclo di correzione, decine al massimo) e
/// l'ordinamento e' crescente, quindi un eventuale taglio lascia il watermark
/// sull'ultima riga LETTA e il giro successivo riprende esattamente da li'.
/// Nessuna scrittura viene saltata, al piu' viene osservata un giro dopo.
const MAX_FACTS: i64 = 1000;

/// Adapter [`MutationProgressPort`] -> `file_mutations` (DB META).
pub struct MutationProgressAdapter {
    /// Pool META: `file_mutations` vive qui.
    db: PgPool,
    /// Sessione del run: confine della ricerca (vedi doc di modulo).
    session_id: Uuid,
}

impl MutationProgressAdapter {
    /// Costruisce l'adapter con le dipendenze gia' risolte dal call site.
    pub fn new(db: PgPool, session_id: Uuid) -> Self {
        Self { db, session_id }
    }
}

#[async_trait]
impl MutationProgressPort for MutationProgressAdapter {
    async fn scan_writes(&self, after: Option<i64>) -> Result<WriteScan, PortError> {
        // `after = None`: si vuole solo il watermark corrente (primo rimando).
        // Una scansione da 0 restituirebbe l'intera storia della sessione, che
        // non e' la domanda e costerebbe righe per nulla.
        let Some(after) = after else {
            let row = sqlx::query(
                "SELECT COALESCE(MAX(id), 0) AS wm FROM file_mutations WHERE session_id = $1",
            )
            .bind(self.session_id)
            .fetch_one(&self.db)
            .await
            .map_err(|e| PortError::Tool(format!("watermark file_mutations: {e}").into()))?;
            let watermark: i64 = row.try_get("wm").unwrap_or(0);
            return Ok(WriteScan {
                watermark,
                facts: Vec::new(),
            });
        };

        let rows = sqlx::query(
            "SELECT id, before_sha256, after_sha256 \
               FROM file_mutations \
              WHERE session_id = $1 AND id > $2 \
              ORDER BY id ASC \
              LIMIT $3",
        )
        .bind(self.session_id)
        .bind(after)
        .bind(MAX_FACTS)
        .fetch_all(&self.db)
        .await
        .map_err(|e| PortError::Tool(format!("scan file_mutations: {e}").into()))?;

        let mut watermark = after;
        let mut facts = Vec::with_capacity(rows.len());
        for r in rows {
            if let Ok(id) = r.try_get::<i64, _>("id") {
                watermark = watermark.max(id);
            }
            facts.push(WriteFact {
                before_sha256: r.try_get("before_sha256").ok().flatten(),
                after_sha256: r.try_get("after_sha256").ok().flatten(),
            });
        }
        Ok(WriteScan { watermark, facts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_agent_graph::decisions::{classify_correction_progress, CorrectionProgress};

    use crate::file_mutations::{record_mutation, ScopeAudit};

    /// Scrive una mutazione REALE con `record_mutation` (il produttore di
    /// produzione, regola O) e ritorna l'id. Gli hash non sono passati a mano:
    /// li deriva la funzione dai contenuti, che e' esattamente il passaggio che
    /// il criterio poi interroga.
    async fn scrivi(
        pool: &PgPool,
        project_id: Uuid,
        session_id: Uuid,
        user_id: Uuid,
        path: &str,
        before: Option<&str>,
        after: Option<&str>,
    ) -> i64 {
        record_mutation(
            pool,
            project_id,
            Some(session_id),
            Some(Uuid::new_v4()),
            Some(user_id),
            path,
            "write_file",
            before,
            after,
            ScopeAudit::none(),
        )
        .await
        .expect("mutazione registrata")
        .id
    }

    /// I fatti arrivano al criterio per la strada della produzione: gli hash che
    /// l'adapter legge sono quelli che `record_mutation` ha calcolato dai
    /// contenuti, non costanti scritte nel test.
    ///
    /// I tre casi del difetto, misurati sullo schema META reale:
    ///  (a) contenuto diverso -> progresso;
    ///  (b) nessuna scrittura dopo il watermark -> nessun progresso;
    ///  (c) write dello STESSO contenuto -> nessun progresso.
    ///
    /// MUTAZIONE: aggiungere `AND before_sha256 IS DISTINCT FROM after_sha256`
    /// alla query (il filtro "comodo" che sposterebbe il criterio nell'SQL) rende
    /// il caso (c) indistinguibile da (b): `SoloRiscritture` diventa
    /// `NessunaScrittura` e l'asserzione cade.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn i_tre_casi_del_criterio_sullo_schema_reale(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let session_id = Uuid::new_v4();
        let port = MutationProgressAdapter::new(pool.clone(), session_id);

        // Watermark iniziale: nessuna scrittura ancora.
        let wm0 = port.scan_writes(None).await.expect("watermark").watermark;

        // (b) nulla e' stato scritto dopo il rimando.
        let scan = port.scan_writes(Some(wm0)).await.expect("scan");
        assert_eq!(
            classify_correction_progress(&scan.facts),
            CorrectionProgress::NessunaScrittura
        );
        assert_eq!(scan.watermark, wm0, "senza righe il watermark non avanza");

        // (c) il file viene riscritto IDENTICO: il write c'e', il contenuto no.
        scrivi(
            &pool,
            project_id,
            session_id,
            user_id,
            "src/api.js",
            Some("const A = 1;"),
            Some("const A = 1;"),
        )
        .await;
        let scan = port.scan_writes(Some(wm0)).await.expect("scan");
        assert_eq!(
            classify_correction_progress(&scan.facts),
            CorrectionProgress::SoloRiscritture { riscritture: 1 },
            "una riscrittura identica non e' una correzione, ma non e' nemmeno \
             'non ha scritto': le due cose vanno distinte"
        );
        let wm1 = scan.watermark;
        assert!(wm1 > wm0, "il watermark avanza sulla riga letta");

        // (a) correzione vera.
        scrivi(
            &pool,
            project_id,
            session_id,
            user_id,
            "src/api.js",
            Some("const A = 1;"),
            Some("const A = 2;"),
        )
        .await;
        let scan = port.scan_writes(Some(wm1)).await.expect("scan");
        assert_eq!(
            classify_correction_progress(&scan.facts),
            CorrectionProgress::Effettivo { scritture_efficaci: 1 }
        );
    }

    /// Il watermark delimita la finestra: cio' che e' stato scritto PRIMA del
    /// rimando non conta come correzione. Senza, il lavoro che ha prodotto i
    /// difetti verrebbe letto come il lavoro che li corregge, e il gate
    /// riconvocherebbe il panel per sempre credendo di vedere progresso.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn le_scritture_precedenti_al_rimando_non_contano(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let session_id = Uuid::new_v4();
        let port = MutationProgressAdapter::new(pool.clone(), session_id);

        // Lavoro PRIMA del rimando (quello che ha prodotto i difetti).
        scrivi(
            &pool,
            project_id,
            session_id,
            user_id,
            "vite.config.js",
            None,
            Some("proxy: process.env.API_URL"),
        )
        .await;
        let wm = port.scan_writes(None).await.expect("watermark").watermark;

        let scan = port.scan_writes(Some(wm)).await.expect("scan");
        assert_eq!(
            classify_correction_progress(&scan.facts),
            CorrectionProgress::NessunaScrittura,
            "il watermark preso al rimando esclude cio' che c'era prima"
        );
    }

    /// La sessione e' il confine: le scritture di un'ALTRA sessione non contano
    /// come correzione di questa. E' il rovescio della scelta di filtrare per
    /// sessione invece che per run — larga abbastanza da contenere i sub-run del
    /// padre, non abbastanza da contenere un altro lavoro.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn le_scritture_di_un_altra_sessione_non_contano(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let mia = Uuid::new_v4();
        let altrui = Uuid::new_v4();
        let port = MutationProgressAdapter::new(pool.clone(), mia);
        let wm = port.scan_writes(None).await.expect("watermark").watermark;

        scrivi(
            &pool,
            project_id,
            altrui,
            user_id,
            "src/altro.js",
            Some("vecchio"),
            Some("nuovo"),
        )
        .await;

        let scan = port.scan_writes(Some(wm)).await.expect("scan");
        assert_eq!(
            classify_correction_progress(&scan.facts),
            CorrectionProgress::NessunaScrittura
        );
    }
}
