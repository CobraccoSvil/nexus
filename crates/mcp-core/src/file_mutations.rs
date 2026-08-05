//! Tracking ripristinabile delle modifiche file fatte dall'agente.
//!
//! Punto unico (regola L / ADR 0026) per:
//!   - `record_mutation`: chiamato dai tool `tool_write_file` / `tool_edit_file`
//!     subito PRIMA della sovrascrittura. Salva before+after in
//!     `file_mutations` (mig 0349).
//!   - `revert_mutation`: ripristina il file allo stato `before` di una
//!     mutazione, generando essa stessa una nuova mutazione `op='reverted'`
//!     cosi' anche il revert sia annullabile.
//!   - `list_recent_mutations`: lista per il pannello UI.
//!
//! Storage: contenuti TEXT in colonna dedicata, fino a un cap configurabile
//! (`agent.mutations.max_track_bytes`, default 5 MB). Sopra il cap registriamo
//! solo metadati (hash+size) e `before_content=NULL`: il revert non e'
//! possibile ma la storia rimane visibile (fail-loud informativo, regola H).
//!
//! Decisione di scope: registriamo il path RELATIVO alla project root
//! (es. "src/index.html"), stesso formato dei tool. Coerente con la lezione
//! mig 0348 sui duplicati per drift assoluto/relativo.

use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::path::PathBuf;
use uuid::Uuid;

/// Cap di sicurezza sul contenuto tracciato. Sopra questa soglia salviamo solo
/// hash+size, niente before/after content. Configurabile via setting
/// `agent.mutations.max_track_bytes` (DB-driven, regola G).
const DEFAULT_MAX_TRACK_BYTES: i64 = 5 * 1024 * 1024;

/// Esito della registrazione di una mutazione.
#[derive(Debug)]
pub struct RecordedMutation {
    pub id: i64,
}

/// Misura di una scrittura rispetto al `write_scope` dichiarato dal pianificatore
/// (mig 0646). Viaggia come UNA cosa sola perche' verdetto e scope dichiarato si
/// leggono insieme: il verdetto dice CHE il piano ha sbagliato, lo scope dichiarato
/// dice COME — ed e' quest'ultimo a distinguere un pianificatore che dimentica un
/// file adiacente da uno che dichiara aree troppo strette.
///
/// Il verdetto NON e' calcolato qui: nasce dal punto unico
/// `nexus_agent_graph::decisions::classify_write` e arriva gia' deciso (regola L).
#[derive(Debug, Clone, Copy)]
pub struct ScopeAudit<'a> {
    /// Identificatore canonico del verdetto (`ScopeVerdict::as_str`), oppure `None`
    /// per i chiamanti che non partecipano alla misura -> colonna NULL, distinta da
    /// `'no_scope_declared'` (che invece e' una misura effettuata).
    pub verdict: Option<&'a str>,
    /// Scope dichiarato al momento della scrittura, verbatim.
    pub declared: &'a [String],
}

impl ScopeAudit<'_> {
    /// Nessuna misura (chiamante fuori dal percorso agentico, o revert). Entrambe
    /// le colonne restano NULL.
    pub fn none() -> Self {
        Self {
            verdict: None,
            declared: &[],
        }
    }
}

/// Parte DERIVATA di una mutazione: operazione semantica, contenuti da salvare
/// (o scartare per il cap), hash e dimensioni. Nessun IO — il cap arriva gia'
/// risolto — quindi la regola di troncamento e' isolata e verificabile senza DB.
struct MutationBody<'a> {
    op: &'static str,
    stored_before: Option<&'a str>,
    stored_after: Option<&'a str>,
    before_sha: Option<String>,
    after_sha: Option<String>,
    before_size: Option<i64>,
    after_size: Option<i64>,
    /// `Some(true)` se la scrittura ha cambiato i SOLI fine-riga: contenuto
    /// identico, byte diversi. `None` quando la domanda non si pone (creazione
    /// o cancellazione: manca uno dei due lati da confrontare).
    ///
    /// Serve a `correction_progress`, che decide sul confronto degli hash e
    /// senza questo campo leggerebbe una riscrittura CRLF->LF come progresso.
    solo_fine_riga: Option<bool>,
}

impl<'a> MutationBody<'a> {
    /// Deriva op/hash/size e applica il cap di tracking.
    ///
    /// Troncamento: se un lato supera `cap` NON salviamo il contenuto (solo
    /// metadati). `before_content` e' la chiave del revert, quindi troncarlo
    /// rende la mutazione "non ripristinabile" ma ancora visibile nella UI.
    fn derive(before: Option<&'a str>, after: Option<&'a str>, cap: i64) -> Self {
        let op = match (before.is_some(), after.is_some()) {
            (false, true) => "created",
            (true, true) => "modified",
            (true, false) => "deleted",
            // Non dovrebbe mai accadere (chiamata vuota). Trattato come modified
            // per non perdere il record; before e after sono entrambi NULL.
            (false, false) => "modified",
        };
        let before_bytes = before.map(str::as_bytes);
        let after_bytes = after.map(str::as_bytes);
        let before_size = before_bytes.map(|b| b.len() as i64);
        let after_size = after_bytes.map(|b| b.len() as i64);
        let over_cap = |size: Option<i64>| size.map(|s| s > cap).unwrap_or(false);
        Self {
            op,
            stored_before: if over_cap(before_size) { None } else { before },
            stored_after: if over_cap(after_size) { None } else { after },
            before_sha: before_bytes.map(sha256_hex),
            after_sha: after_bytes.map(sha256_hex),
            before_size,
            after_size,
            solo_fine_riga: solo_fine_riga(before_bytes, after_bytes),
        }
    }
}

/// La scrittura ha cambiato i soli fine-riga?
///
/// `None` quando la domanda non si pone: senza uno dei due lati (file creato o
/// cancellato) non c'e' nulla da confrontare, e un `false` affermerebbe che il
/// contenuto e' cambiato davvero — vero in quei due casi, ma per una ragione
/// diversa da quella che questo campo misura. L'ignoto resta ignoto (regola Q).
///
/// Il criterio non e' scritto qui: e' `nexus_migrations::fine_riga`, che
/// risponde alla stessa domanda anche per i checksum delle migrazioni. Tenerne
/// una seconda versione qui avrebbe garantito che divergessero.
fn solo_fine_riga(before: Option<&[u8]>, after: Option<&[u8]>) -> Option<bool> {
    use nexus_migrations::fine_riga::{classifica_contenuto, EsitoFineRiga};
    let (b, a) = (before?, after?);
    Some(matches!(classifica_contenuto(b, a), EsitoFineRiga::SoloFineRiga))
}

/// Calcola lo SHA-256 in hex di un blocco di byte.
fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

/// Legge la soglia di tracking dal DB (regola G).
async fn max_track_bytes(db: &PgPool) -> i64 {
    // get_setting_nonempty propaga errori; in caso di problema ricadiamo sul
    // default conservativo (non blocca la mutazione, solo il tracking esteso).
    match nexus_auth::get_int_setting(db, "agent.mutations.max_track_bytes").await {
        Ok(Some(v)) if v > 0 => v,
        _ => DEFAULT_MAX_TRACK_BYTES,
    }
}

/// Registra una mutazione file. Da chiamare PRIMA di sovrascrivere il file:
/// `before_content` deve essere lo stato corrente del file (`None` se non
/// esisteva), `after_content` il nuovo stato che sta per essere scritto.
///
/// Fail-loud: se l'INSERT fallisce ritorna l'errore al chiamante. Il chiamante
/// decide se procedere comunque con la write (per non bloccare l'agente in caso
/// di tabella momentaneamente irraggiungibile) loggando il problema.
///
/// `scope` porta la MISURA delle scritture fuori dallo scope dichiarato dal
/// pianificatore (mig 0646): e' un'osservazione, non un permesso — nessun ramo di
/// questa funzione rifiuta una scrittura in base ad essa.
pub async fn record_mutation(
    db: &PgPool,
    project_id: Uuid,
    session_id: Option<Uuid>,
    // Run che sta scrivendo. Per un sub-run e' il proxy del passo di piano: senza, "quante
    // scritture fuori scope" non si puo' trasformare in "su quanti passi", e una
    // violazione ripetuta da un solo passo sarebbe indistinguibile da molti passi
    // che sbagliano una volta ciascuno.
    run_id: Option<Uuid>,
    user_id: Option<Uuid>,
    relative_path: &str,
    tool_name: &str,
    before_content: Option<&str>,
    after_content: Option<&str>,
    scope: ScopeAudit<'_>,
) -> Result<RecordedMutation, sqlx::Error> {
    let cap = max_track_bytes(db).await;
    let body = MutationBody::derive(before_content, after_content, cap);
    let MutationBody {
        op,
        stored_before,
        stored_after,
        before_sha,
        after_sha,
        before_size,
        after_size,
        solo_fine_riga,
    } = body;

    // Lo scope dichiarato si persiste SOLO quando c'e' un verdetto: un array senza
    // verdetto non direbbe nulla che il verdetto non dica gia', e riempirebbe la
    // colonna di `{}` indistinguibili da "dichiarato vuoto".
    let declared_scope: Option<Vec<String>> = scope.verdict.map(|_| scope.declared.to_vec());

    let row = sqlx::query(
        r#"INSERT INTO file_mutations
            (project_id, session_id, user_id, file_path, tool_name, op,
             before_content, after_content,
             before_sha256, after_sha256, before_size, after_size,
             scope_verdict, declared_write_scope, run_id, solo_fine_riga)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
           RETURNING id"#,
    )
    .bind(project_id)
    .bind(session_id)
    .bind(user_id)
    .bind(relative_path)
    .bind(tool_name)
    .bind(op)
    .bind(stored_before)
    .bind(stored_after)
    .bind(before_sha)
    .bind(after_sha)
    .bind(before_size)
    .bind(after_size)
    .bind(scope.verdict)
    .bind(declared_scope)
    .bind(run_id)
    .bind(solo_fine_riga)
    .fetch_one(db)
    .await?;

    let id: i64 = row.try_get("id")?;
    Ok(RecordedMutation { id })
}

/// Riga di una mutazione, esportabile come JSON al frontend.
#[derive(Debug, sqlx::FromRow, serde::Serialize)]
pub struct MutationRow {
    pub id: i64,
    pub project_id: Uuid,
    pub session_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub file_path: String,
    pub tool_name: String,
    pub op: String,
    pub before_size: Option<i64>,
    pub after_size: Option<i64>,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
    /// True se il contenuto e' presente in DB e quindi revertibile.
    pub revertible: bool,
    pub reverted_at: Option<chrono::DateTime<chrono::Utc>>,
    pub reverts_mutation_id: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Lista le mutazioni piu' recenti del progetto. Non include il contenuto
/// (potrebbe essere grosso): la UI lo carica on-demand per il diff.
pub async fn list_recent_mutations(
    db: &PgPool,
    project_id: Uuid,
    limit: i64,
) -> Result<Vec<MutationRow>, sqlx::Error> {
    let limit = limit.clamp(1, 500);
    let rows = sqlx::query(
        r#"SELECT id, project_id, session_id, user_id, file_path, tool_name, op,
                  before_size, after_size, before_sha256, after_sha256,
                  (before_content IS NOT NULL) AS revertible,
                  reverted_at, reverts_mutation_id, created_at
             FROM file_mutations
            WHERE project_id = $1
            ORDER BY created_at DESC, id DESC
            LIMIT $2"#,
    )
    .bind(project_id)
    .bind(limit)
    .fetch_all(db)
    .await?;

    let out = rows
        .into_iter()
        .map(|r| MutationRow {
            id: r.get("id"),
            project_id: r.get("project_id"),
            session_id: r.try_get("session_id").ok().flatten(),
            user_id: r.try_get("user_id").ok().flatten(),
            file_path: r.get("file_path"),
            tool_name: r.get("tool_name"),
            op: r.get("op"),
            before_size: r.try_get("before_size").ok().flatten(),
            after_size: r.try_get("after_size").ok().flatten(),
            before_sha256: r.try_get("before_sha256").ok().flatten(),
            after_sha256: r.try_get("after_sha256").ok().flatten(),
            revertible: r.try_get::<bool, _>("revertible").unwrap_or(false),
            reverted_at: r.try_get("reverted_at").ok().flatten(),
            reverts_mutation_id: r.try_get("reverts_mutation_id").ok().flatten(),
            created_at: r.get("created_at"),
        })
        .collect();
    Ok(out)
}

/// Carica una singola mutazione con i contenuti before/after, per visualizzare
/// il diff nella UI.
pub async fn get_mutation_full(
    db: &PgPool,
    project_id: Uuid,
    mutation_id: i64,
) -> Result<Option<serde_json::Value>, sqlx::Error> {
    let row = sqlx::query(
        r#"SELECT id, project_id, session_id, user_id, file_path, tool_name, op,
                  before_content, after_content,
                  before_size, after_size, before_sha256, after_sha256,
                  reverted_at, reverts_mutation_id, created_at
             FROM file_mutations
            WHERE id = $1 AND project_id = $2"#,
    )
    .bind(mutation_id)
    .bind(project_id)
    .fetch_optional(db)
    .await?;
    let Some(r) = row else { return Ok(None) };
    Ok(Some(serde_json::json!({
        "id": r.get::<i64, _>("id"),
        "file_path": r.get::<String, _>("file_path"),
        "tool_name": r.get::<String, _>("tool_name"),
        "op": r.get::<String, _>("op"),
        "before_content": r.try_get::<Option<String>, _>("before_content").ok().flatten(),
        "after_content": r.try_get::<Option<String>, _>("after_content").ok().flatten(),
        "before_size": r.try_get::<Option<i64>, _>("before_size").ok().flatten(),
        "after_size": r.try_get::<Option<i64>, _>("after_size").ok().flatten(),
        "before_sha256": r.try_get::<Option<String>, _>("before_sha256").ok().flatten(),
        "after_sha256": r.try_get::<Option<String>, _>("after_sha256").ok().flatten(),
        "reverted_at": r.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("reverted_at").ok().flatten(),
        "reverts_mutation_id": r.try_get::<Option<i64>, _>("reverts_mutation_id").ok().flatten(),
        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
    })))
}

/// Esito di un revert.
#[derive(Debug)]
pub enum RevertOutcome {
    /// Ripristino eseguito con successo. `new_mutation_id` punta alla mutazione
    /// `op='reverted'` appena creata.
    Reverted { new_mutation_id: i64 },
    /// La mutazione non esiste o non e' del progetto.
    NotFound,
    /// La mutazione e' marcata come gia' revertita.
    AlreadyReverted,
    /// before_content non disponibile (es. file nuovo creato o contenuto sopra
    /// il cap di tracking): non e' possibile ripristinare uno stato precedente.
    NotRevertible(&'static str),
    /// Lo stato corrente del file su disco non corrisponde all'`after_sha256`
    /// registrato: significa che l'utente o un altro tool ha modificato il file
    /// dopo questa mutazione. Per non perdere quelle modifiche silenziosamente
    /// rifiutiamo (regola H) — il chiamante puo' forzare con `force=true`.
    Conflict {
        current_sha: String,
        expected_sha: String,
    },
    /// Errore I/O o DB.
    IoError(String),
}

/// Ripristina il file allo stato `before` della mutazione indicata.
///
/// `force=false` (default): se lo stato corrente del file non corrisponde a
/// `after_sha256` segnala conflitto. `force=true`: sovrascrive comunque
/// (l'utente ha confermato).
pub async fn revert_mutation(
    db: &PgPool,
    project_id: Uuid,
    project_root: &PathBuf,
    user_id: Option<Uuid>,
    session_id: Option<Uuid>,
    mutation_id: i64,
    force: bool,
) -> RevertOutcome {
    // 1) Carica la mutazione.
    let row = match sqlx::query(
        r#"SELECT file_path, op, before_content, after_sha256, reverted_at
             FROM file_mutations
            WHERE id = $1 AND project_id = $2
            FOR UPDATE"#,
    )
    .bind(mutation_id)
    .bind(project_id)
    .fetch_optional(db)
    .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return RevertOutcome::NotFound,
        Err(e) => return RevertOutcome::IoError(e.to_string()),
    };

    if row
        .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("reverted_at")
        .ok()
        .flatten()
        .is_some()
    {
        return RevertOutcome::AlreadyReverted;
    }

    let file_path: String = row.get("file_path");
    let op: String = row.get("op");
    let before_content: Option<String> = row.try_get("before_content").ok().flatten();
    let after_sha256: Option<String> = row.try_get("after_sha256").ok().flatten();

    // 2) Path assoluto confinato dentro la project root (difesa in profondita').
    let abs = project_root.join(&file_path);
    if !abs.starts_with(project_root) {
        return RevertOutcome::IoError(format!("path fuori dalla root: {file_path}"));
    }

    // 3) Conflict detection: stato corrente vs after_sha256 atteso.
    let current = tokio::fs::read(&abs).await.ok();
    if !force {
        if let (Some(cur), Some(exp)) = (current.as_ref(), after_sha256.as_ref()) {
            let cur_sha = sha256_hex(cur);
            if &cur_sha != exp {
                return RevertOutcome::Conflict {
                    current_sha: cur_sha,
                    expected_sha: exp.clone(),
                };
            }
        }
    }

    // 4) Applica il ripristino in base all'op originale.
    //
    // - op='created' -> before non esisteva, ripristino significa CANCELLARE
    //   il file. Sicurezza: revert solo se before_content e' NULL (coerente).
    // - op='modified' o 'deleted' o 'reverted' -> riscrive il file con
    //   before_content. Se before_content e' NULL (truncato), non possiamo.
    let new_after_content: Option<String>;
    let new_op: &str;
    match op.as_str() {
        "created" => {
            if before_content.is_some() {
                // Inconsistenza: op=created ma before_content presente. Per non
                // perdere dati lo ripristiniamo come scrittura.
                if let Err(e) =
                    tokio::fs::write(&abs, before_content.as_deref().unwrap_or("")).await
                {
                    return RevertOutcome::IoError(e.to_string());
                }
                new_after_content = before_content.clone();
                new_op = "modified";
            } else {
                if let Err(e) = tokio::fs::remove_file(&abs).await {
                    // Se il file non esiste piu' (es. utente l'ha gia' cancellato)
                    // consideriamo il revert idempotente.
                    if e.kind() != std::io::ErrorKind::NotFound {
                        return RevertOutcome::IoError(e.to_string());
                    }
                }
                new_after_content = None;
                new_op = "deleted";
            }
        }
        _ => {
            let Some(prev) = before_content.as_deref() else {
                return RevertOutcome::NotRevertible(
                    "contenuto pre-modifica non disponibile (truncato o non registrato)",
                );
            };
            if let Some(parent) = abs.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            if let Err(e) = tokio::fs::write(&abs, prev).await {
                return RevertOutcome::IoError(e.to_string());
            }
            new_after_content = Some(prev.to_string());
            new_op = "reverted";
        }
    }

    // 5) Registra la mutazione di revert (anche essa annullabile).
    let current_str = current.as_deref().and_then(|b| std::str::from_utf8(b).ok());
    let recorded = match record_mutation(
        db,
        project_id,
        session_id,
        // Il revert non appartiene a un run dell'agente: e' un'azione dell'utente
        // dal pannello.
        None,
        user_id,
        &file_path,
        "revert",
        current_str,
        new_after_content.as_deref(),
        // Un revert e' un'azione dell'utente sul pannello, non una scrittura
        // dell'agente sotto un piano: misurarlo contro uno scope non avrebbe
        // oggetto. Colonne NULL, distinte da 'no_scope_declared'.
        ScopeAudit::none(),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return RevertOutcome::IoError(format!("registrazione revert fallita: {e}")),
    };

    // Forziamo op='reverted' per il nuovo record + collega alla mutazione
    // originale (reverts_mutation_id). E aggiorniamo la mutazione originale
    // come reverted_at + reverted_by_mutation_id.
    let _ = sqlx::query(
        r#"UPDATE file_mutations
              SET op = $1, reverts_mutation_id = $2
            WHERE id = $3"#,
    )
    .bind(new_op)
    .bind(mutation_id)
    .bind(recorded.id)
    .execute(db)
    .await;

    let _ = sqlx::query(
        r#"UPDATE file_mutations
              SET reverted_at = NOW(), reverted_by_mutation_id = $1
            WHERE id = $2"#,
    )
    .bind(recorded.id)
    .bind(mutation_id)
    .execute(db)
    .await;

    // Auto-commit per sessione: il revert e' a sua volta una mutazione
    // dell'agente, va congelato nel branch nexus/session/<short>. Verifica
    // is_git_repo on-the-fly (per il revert non passiamo per ctx).
    let is_git = project_root.join(".git").exists();
    // Il revert opera SEMPRE sulla root reale del progetto (mai un sub-run
    // isolato): `isolated_subrun=false` -> autocommit attivo come oggi.
    crate::session_autocommit::snapshot_after_mutation(
        db,
        project_root,
        is_git,
        session_id,
        false,
        "revert",
        &file_path,
    )
    .await;

    RevertOutcome::Reverted {
        new_mutation_id: recorded.id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_agent_graph::decisions::{classify_write, ScopeVerdict};

    /// Le colonne della misura (mig 0646) esistono sullo schema META REALE e
    /// `record_mutation` — la funzione di produzione, non una sua copia — vi
    /// scrive il verdetto e lo scope dichiarato.
    ///
    /// Gira sul `META_MIGRATOR`, cioe' sul set `db/migrations` applicato a un DB
    /// vergine (regola O): se la 0646 dichiarasse una colonna che non crea, o se il
    /// CHECK non ammettesse un identificatore prodotto da `ScopeVerdict::as_str`,
    /// questo test lo vedrebbe. Una fixture `CREATE TABLE` scritta a mano no.
    ///
    /// Il verdetto NON e' una costante scritta nel test: viene da `classify_write`,
    /// lo stesso punto unico che lo produce in produzione. Cosi' il test lega il
    /// vocabolario dell'enum al vincolo della colonna, che e' esattamente il punto
    /// dove i due potrebbero divergere in silenzio.
    ///
    /// Mutazione che rende rosso: rimuovere `scope_verdict` dall'INSERT -> la
    /// colonna resta NULL e la prima asserzione cade.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn record_mutation_persiste_verdetto_e_scope_dichiarato(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;

        // Il caso che il numero deve contare: scope dichiarato su `crates/api`, il
        // sub-run scrive altrove. Costruito perche' l'esito ATTESO differisca da
        // quello che il sistema produrrebbe senza la misura (colonna NULL) e anche
        // da quello di uno scope assente ('no_scope_declared').
        let declared = vec!["crates/api".to_string(), "db/migrations".to_string()];
        let path = "crates/web/router.rs";
        let run_id = Uuid::new_v4();
        let verdict = classify_write(path, &declared);
        assert_eq!(
            verdict,
            ScopeVerdict::OutOfScope,
            "premessa del test: questa scrittura e' fuori dallo scope dichiarato"
        );

        let rec = record_mutation(
            &pool,
            project_id,
            None,
            Some(run_id),
            Some(user_id),
            path,
            "write_file",
            None,
            Some("contenuto"),
            ScopeAudit {
                verdict: Some(verdict.as_str()),
                declared: &declared,
            },
        )
        .await
        .expect("insert con le colonne della misura");

        let (got_verdict, got_scope, got_run): (Option<String>, Option<Vec<String>>, Option<Uuid>) =
            sqlx::query_as(
                "SELECT scope_verdict, declared_write_scope, run_id \
                   FROM file_mutations WHERE id = $1",
            )
            .bind(rec.id)
            .fetch_one(&pool)
            .await
            .expect("riga riletta");

        assert_eq!(got_verdict.as_deref(), Some("out_of_scope"));
        assert_eq!(
            got_scope,
            Some(declared),
            "lo scope dichiarato si conserva verbatim: dice COME sbagliava il \
             pianificatore, non solo che sbagliava"
        );
        assert_eq!(
            got_run,
            Some(run_id),
            "senza il run non si passa da 'quante scritture' a 'su quanti todo'"
        );
    }

    /// Le due assenze NON sono la stessa cosa, e la migrazione le tiene distinte:
    ///   - `ScopeAudit::none()` (revert, chiamanti fuori dal percorso agentico)
    ///     -> colonne NULL: la scrittura non partecipa alla misura;
    ///   - scope vuoto misurato -> `'no_scope_declared'`: la scrittura E' passata
    ///     dalla misura, che ha potuto dire soltanto "non era dichiarato".
    ///
    /// Confonderle e' il modo piu' diretto per leggere una propagazione rotta come
    /// un pianificatore preciso: la vista `file_mutations_scope_audit` le separa in
    /// `unmeasured_legacy` e `not_measurable` proprio per questo.
    ///
    /// Mutazione che rende rosso: far ritornare a `classify_write` `InScope` per
    /// scope vuoto -> la seconda asserzione cade.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn assenza_di_misura_e_assenza_di_scope_restano_distinte(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let run_id = Uuid::new_v4();

        // (1) Nessuna misura: colonne NULL.
        let senza = record_mutation(
            &pool,
            project_id,
            None,
            Some(run_id),
            Some(user_id),
            "a.txt",
            "revert",
            None,
            Some("x"),
            ScopeAudit::none(),
        )
        .await
        .expect("insert senza misura");

        // (2) Misurata, ma il task non aveva dichiarato nulla.
        let vuoto: Vec<String> = Vec::new();
        let misurata = record_mutation(
            &pool,
            project_id,
            None,
            Some(run_id),
            Some(user_id),
            "b.txt",
            "write_file",
            None,
            Some("y"),
            ScopeAudit {
                verdict: Some(classify_write("b.txt", &vuoto).as_str()),
                declared: &vuoto,
            },
        )
        .await
        .expect("insert misurata");

        let leggi = |id: i64| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (Option<String>, Option<Vec<String>>)>(
                    "SELECT scope_verdict, declared_write_scope FROM file_mutations WHERE id = $1",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("riga riletta")
            }
        };

        let (v1, s1) = leggi(senza.id).await;
        assert_eq!(v1, None, "nessuna misura -> verdetto NULL");
        assert_eq!(s1, None, "nessuna misura -> scope NULL, non array vuoto");

        let (v2, s2) = leggi(misurata.id).await;
        assert_eq!(v2.as_deref(), Some("no_scope_declared"));
        assert_eq!(
            s2,
            Some(Vec::new()),
            "misurata senza dichiarazione: array vuoto, distinto da NULL"
        );
    }

    /// La vista di aggregazione (mig 0646) e' il punto unico della domanda "quanto
    /// si scrive fuori scope?", e la sua percentuale si calcola sulle sole
    /// scritture MISURABILI. Se contasse anche le non misurabili, una propagazione
    /// rotta si presenterebbe come 0% di violazioni.
    ///
    /// Mutazione che rende rosso: togliere il filtro `IN ('in_scope',
    /// 'out_of_scope')` dal denominatore -> con 1 violazione su 2 misurabili + 2
    /// non misurabili la percentuale scenderebbe da 50.00 a 25.00.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn vista_aggrega_le_violazioni_sulle_sole_misurabili(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let scope = vec!["crates/api".to_string()];
        // DUE run distinti, e il primo viola DUE volte. I due conteggi che la vista
        // deve tenere separati sono cosi' NUMERICAMENTE diversi (2 scritture fuori
        // scope, ma 1 solo run che sbaglia): se la vista contasse le scritture al
        // posto dei run — o viceversa — l'asserzione cadrebbe. Con un run per
        // scrittura i due numeri coinciderebbero e il test passerebbe anche con la
        // colonna sbagliata.
        let run_viola = Uuid::new_v4();
        let run_ok = Uuid::new_v4();

        // 3 misurabili (1 dentro, 2 fuori dallo stesso run) + 1 non misurabile
        // + 1 non misurata.
        for (run, path, audit) in [
            (
                run_ok,
                "crates/api/a.rs",
                ScopeAudit {
                    verdict: Some(classify_write("crates/api/a.rs", &scope).as_str()),
                    declared: &scope,
                },
            ),
            (
                run_viola,
                "crates/web/b.rs",
                ScopeAudit {
                    verdict: Some(classify_write("crates/web/b.rs", &scope).as_str()),
                    declared: &scope,
                },
            ),
            (
                run_viola,
                "apps/web/c.tsx",
                ScopeAudit {
                    verdict: Some(classify_write("apps/web/c.tsx", &scope).as_str()),
                    declared: &scope,
                },
            ),
            (
                run_ok,
                "c.rs",
                ScopeAudit {
                    verdict: Some(ScopeVerdict::NoScopeDeclared.as_str()),
                    declared: &[],
                },
            ),
            (run_ok, "d.rs", ScopeAudit::none()),
        ] {
            record_mutation(
                &pool,
                project_id,
                None,
                Some(run),
                Some(user_id),
                path,
                "write_file",
                None,
                Some("x"),
                audit,
            )
            .await
            .expect("insert");
        }

        // La percentuale e' NUMERIC nella vista (niente arrotondamenti binari); il
        // test la legge come testo per confrontarla esatta, senza dipendere da una
        // feature decimale di sqlx.
        let (total, measured, out_of_scope, runs_out, runs_measured, not_measurable, legacy, pct): (
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT mutations_total, measured, out_of_scope, runs_out_of_scope, \
                    runs_measured, not_measurable, unmeasured_legacy, out_of_scope_pct::text \
               FROM file_mutations_scope_audit WHERE project_id = $1",
        )
        .bind(project_id)
        .fetch_one(&pool)
        .await
        .expect("vista interrogabile");

        assert_eq!(total, 5);
        assert_eq!(measured, 3, "solo le tre con scope dichiarato sono misurabili");
        assert_eq!(out_of_scope, 2, "due SCRITTURE fuori scope");
        assert_eq!(
            runs_out, 1,
            "ma UN SOLO todo che sbaglia: e' la differenza fra 'un piano fatto \
             male' e 'il pianificatore non sa dichiarare'"
        );
        assert_eq!(runs_measured, 2);
        assert_eq!(not_measurable, 1);
        assert_eq!(legacy, 1);
        assert_eq!(
            pct.as_deref(),
            Some("66.67"),
            "2 violazioni su 3 MISURABILI: le non misurabili non diluiscono il dato"
        );
    }
}
