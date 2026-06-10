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
    /// True se contenuto > cap: revert non possibile, solo metadati salvati.
    pub truncated: bool,
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
pub async fn record_mutation(
    db: &PgPool,
    project_id: Uuid,
    session_id: Option<Uuid>,
    user_id: Option<Uuid>,
    relative_path: &str,
    tool_name: &str,
    before_content: Option<&str>,
    after_content: Option<&str>,
) -> Result<RecordedMutation, sqlx::Error> {
    let op = match (before_content.is_some(), after_content.is_some()) {
        (false, true) => "created",
        (true, true) => "modified",
        (true, false) => "deleted",
        // Non dovrebbe mai accadere (chiamata vuota). Trattato come modified
        // per non perdere il record; before e after sono entrambi NULL.
        (false, false) => "modified",
    };

    let before_bytes = before_content.map(str::as_bytes);
    let after_bytes = after_content.map(str::as_bytes);

    let before_size = before_bytes.map(|b| b.len() as i64);
    let after_size = after_bytes.map(|b| b.len() as i64);
    let before_sha = before_bytes.map(sha256_hex);
    let after_sha = after_bytes.map(sha256_hex);

    // Decisione di troncamento: se uno dei due lati supera il cap, NON salviamo
    // il contenuto (solo metadati). before_content e' la chiave del revert: se
    // viene troncato lo stato resta visibile come "non ripristinabile" nella UI.
    let cap = max_track_bytes(db).await;
    let truncate_before = before_size.map(|s| s > cap).unwrap_or(false);
    let truncate_after = after_size.map(|s| s > cap).unwrap_or(false);
    let truncated = truncate_before || truncate_after;

    let stored_before: Option<&str> = if truncate_before {
        None
    } else {
        before_content
    };
    let stored_after: Option<&str> = if truncate_after { None } else { after_content };

    let row = sqlx::query(
        r#"INSERT INTO file_mutations
            (project_id, session_id, user_id, file_path, tool_name, op,
             before_content, after_content,
             before_sha256, after_sha256, before_size, after_size)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
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
    .fetch_one(db)
    .await?;

    let id: i64 = row.try_get("id")?;
    Ok(RecordedMutation { id, truncated })
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
        user_id,
        &file_path,
        "revert",
        current_str,
        new_after_content.as_deref(),
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
    crate::session_autocommit::snapshot_after_mutation(
        db,
        project_root,
        is_git,
        session_id,
        "revert",
        &file_path,
    )
    .await;

    RevertOutcome::Reverted {
        new_mutation_id: recorded.id,
    }
}
