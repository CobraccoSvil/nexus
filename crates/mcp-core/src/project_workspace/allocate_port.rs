//! Fix M33-B: endpoint REST per allocazione dinamica di porte di progetto.
//! Esteso nel PR hardening con quota check + audit trail centralizzato.
//!
//! POST /api/projects/:id/services/allocate-port
//!
//! Body: `{label: string}` (es. "backend", "frontend", "api")
//!
//! L'agente AI chiama questo endpoint (o `find_or_allocate` interno) quando deve
//! scegliere una porta per un servizio del progetto. Nexus:
//! 1. Verifica quota porte (`security::quotas::check_can_allocate_port`)
//! 2. Sceglie una porta libera nel bucket deterministico del progetto via
//!    `find_free_project_port`.
//! 3. INSERT in `nexus_port_allocations` con allocation_mode='dynamic'.
//! 4. Scrive `nexus_resource_audit` (allowed/blocked).
//! 5. Ritorna `{port, label, allocation_mode}` per uso dell'agente.

use super::services::deterministic_project_port_for_key;
use super::*;
use crate::port_registry::PortRegistryCache;
use crate::security::{record_audit, AuditEntry};
use sqlx::PgPool;

#[derive(serde::Deserialize)]
pub struct AllocatePortBody {
    pub label: String,
}

/// Risultato di una chiamata a `find_or_allocate`.
pub struct AllocatedPort {
    pub port: u16,
    pub mode: &'static str, // "existing" | "dynamic" | "adopted" | "reallocated"
}

/// Decisione PURA (testabile, regola L) su cosa fare quando la porta di una
/// allocazione esistente risponde al probe TCP: la porta e' DAVVERO occupata,
/// e la scelta dipende solo da chi la occupa e dalla liberabilita'.
#[derive(Debug, PartialEq, Eq)]
enum ActivePortAction {
    /// Occupante tracciato in `agent_processes` (running/starting): e' il
    /// servizio legittimo del progetto, la porta si riusa (existing).
    ReuseTracked,
    /// Occupante NON tracciato ma la porta e' stata liberata (try_free_port):
    /// si riusa la stessa porta, ora libera.
    ReuseFreed,
    /// Occupante NON tracciato e porta NON liberabile: mai ritornare una porta
    /// occupata (EADDRINUSE garantito al bind del nuovo processo) — si alloca
    /// una porta nuova segnalando l'occupante.
    ReallocateNew,
}

fn active_port_action(occupant_tracked: bool, freed: bool) -> ActivePortAction {
    if occupant_tracked {
        ActivePortAction::ReuseTracked
    } else if freed {
        ActivePortAction::ReuseFreed
    } else {
        ActivePortAction::ReallocateNew
    }
}

/// Funzione internamente riusabile: trova una porta gia' allocata con la stessa
/// label OPPURE ne alloca una nuova nel bucket del progetto.
///
/// Applica quota check (`max_ports`) prima di allocare. In caso di violazione
/// quota, scrive audit `port_allocate` blocked e ritorna `Err`.
///
/// Idempotente: chiamate ripetute con la stessa `(project_id, label)` ritornano
/// la stessa porta (modalita' "existing").
pub async fn find_or_allocate(
    db: &PgPool,
    registry: &PortRegistryCache,
    project_id: Uuid,
    label: &str,
) -> Result<AllocatedPort, String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("label vuota: specifica un nome (es. 'backend', 'frontend')".to_string());
    }

    // 1. Idempotenza: se esiste gia' una allocazione con questa label, riusala
    //    SOLO se qualcuno e' davvero in ascolto sulla porta. Altrimenti tentiamo
    //    di adottare un processo orfano del bucket prima di re-allocare.
    if let Ok(Some((existing_port,))) = sqlx::query_as::<_, (i32,)>(
        "SELECT port FROM nexus_port_allocations WHERE project_id = $1 AND label = $2 LIMIT 1",
    )
    .bind(project_id)
    .bind(label)
    .fetch_optional(db)
    .await
    {
        let p = existing_port as u16;
        if super::port_recovery::tcp_probe(p, 300).await {
            // La porta risponde: qualcuno ascolta DAVVERO. Storicamente questo
            // ramo ritornava 'existing' senza chiedersi CHI: se l'occupante era
            // un processo non tracciato (orfano di uno stop non verificato,
            // avvio manuale con .env stantio), run_service iniettava una porta
            // occupata al nuovo processo -> EADDRINUSE garantito (incidente
            // Beaty-Book 2026-07-02). Ora la decisione passa dal punto unico
            // `active_port_action`.
            let occupant = super::port_recovery::port_occupant(p).await;
            let tracked = match &occupant {
                Some((pid, _)) => {
                    super::port_recovery::is_tracked_pid(db, project_id, *pid).await
                }
                None => false,
            };
            // try_free_port ha side effect (kill dell'albero occupante): si
            // invoca SOLO per occupanti non tracciati. Rifiuta da se' PID 0 e
            // mcp-core stesso, e ritorna false se la porta resta occupata.
            let freed = if tracked {
                false
            } else {
                super::port_recovery::try_free_port(p).await
            };
            let occupant_pid = occupant.as_ref().map(|(pid, _)| *pid);
            let occupant_program = occupant
                .as_ref()
                .map(|(_, prog)| prog.clone())
                .unwrap_or_default();
            match active_port_action(tracked, freed) {
                ActivePortAction::ReuseTracked => {
                    return Ok(AllocatedPort {
                        port: p,
                        mode: "existing",
                    });
                }
                ActivePortAction::ReuseFreed => {
                    tracing::warn!(
                        label = %label, port = p, occupant_pid = ?occupant_pid,
                        occupant_program = %occupant_program,
                        "find_or_allocate: porta occupata da processo NON tracciato, liberata prima del riuso"
                    );
                    record_audit(
                        AuditEntry::allowed(project_id, "port_freed", "port")
                            .with_resource(p.to_string())
                            .with_details(serde_json::json!({
                                "label": label,
                                "occupant_pid": occupant_pid,
                                "occupant_program": occupant_program,
                            })),
                    );
                    return Ok(AllocatedPort {
                        port: p,
                        mode: "existing",
                    });
                }
                ActivePortAction::ReallocateNew => {
                    // Porta non liberabile: si rialloca nel bucket (la funzione
                    // deterministica salta le porte gia' allocate in DB e quelle
                    // che non superano il bind di prova) e si aggiorna la riga
                    // della label. L'occupante viene SEGNALATO, mai ignorato.
                    let new_port =
                        deterministic_project_port_for_key(&project_id, label, registry).await;
                    tracing::warn!(
                        label = %label, busy_port = p, new_port,
                        occupant_pid = ?occupant_pid, occupant_program = %occupant_program,
                        "find_or_allocate: porta occupata da processo NON tracciato e non liberabile, rialloco su porta nuova"
                    );
                    if let Err(e) = sqlx::query(
                        "UPDATE nexus_port_allocations \
                         SET port = $1, allocation_mode = 'dynamic', updated_at = NOW() \
                         WHERE project_id = $2 AND label = $3",
                    )
                    .bind(new_port as i32)
                    .bind(project_id)
                    .bind(label)
                    .execute(db)
                    .await
                    {
                        tracing::warn!(
                            "find_or_allocate: UPDATE riallocazione fallito (porta {} label {}): {}",
                            new_port,
                            label,
                            e
                        );
                    }
                    record_audit(
                        AuditEntry::allowed(project_id, "port_realloc", "port")
                            .with_resource(new_port.to_string())
                            .with_details(serde_json::json!({
                                "label": label,
                                "busy_port": p,
                                "occupant_pid": occupant_pid,
                                "occupant_program": occupant_program,
                                "reason": "occupied_by_untracked_unkillable",
                            })),
                    );
                    return Ok(AllocatedPort {
                        port: new_port,
                        mode: "reallocated",
                    });
                }
            }
        }
        // Allocazione "stale": nessuno in ascolto. Cerca processi orfani del
        // bucket (utente li ha lanciati manualmente con .env hardcoded, oppure
        // un avvio precedente di Nexus non e' stato tracciato).
        let orphans = super::port_recovery::scan_bucket_orphans(db, project_id).await;
        if let Some((found_port, pid, program)) = orphans
            .iter()
            .find(|(_, _, prog)| super::port_recovery::looks_like_server_process(prog))
        {
            tracing::info!(
                label = %label, stale_port = p, adopted_port = *found_port,
                pid = *pid, program = %program,
                "find_or_allocate: allocazione stale, adotto processo orfano del bucket"
            );
            let _ = sqlx::query(
                "UPDATE nexus_port_allocations \
                 SET port = $1, allocation_mode = 'adopted', updated_at = NOW() \
                 WHERE project_id = $2 AND label = $3",
            )
            .bind(*found_port as i32)
            .bind(project_id)
            .bind(label)
            .execute(db)
            .await;
            record_audit(
                AuditEntry::allowed(project_id, "port_adopt", "port")
                    .with_resource(found_port.to_string())
                    .with_details(serde_json::json!({
                        "label": label, "stale_port": p, "pid": pid, "program": program
                    })),
            );
            return Ok(AllocatedPort {
                port: *found_port,
                mode: "adopted",
            });
        }
        // Nessun orfano adottabile. La porta stale risulta libera (il probe TCP
        // poco sopra e' negativo): ADOTTALA riusando la STESSA porta — per
        // stabilita' tra restart — invece di eliminarla e riallocarne una nuova.
        // La riga resta (UNIQUE project_id,label) con mode='adopted'. Questo
        // chiude il deadlock allocazione stantia: run_service prosegue e riusa la
        // stessa porta per la label, senza dipendere da `service_restart`.
        let _ = sqlx::query(
            "UPDATE nexus_port_allocations \
             SET allocation_mode = 'adopted', updated_at = NOW() \
             WHERE project_id = $1 AND label = $2",
        )
        .bind(project_id)
        .bind(label)
        .execute(db)
        .await;
        record_audit(
            AuditEntry::allowed(project_id, "port_adopt", "port")
                .with_resource(p.to_string())
                .with_details(serde_json::json!({
                    "label": label, "stale_port": p, "mode": "adopted",
                    "reason": "stale_no_listener"
                })),
        );
        tracing::info!(
            label = %label, adopted_port = p,
            "find_or_allocate: allocazione stale riusata sulla stessa porta (adopted)"
        );
        return Ok(AllocatedPort {
            port: p,
            mode: "adopted",
        });
    }

    // 1-bis. Consapevolezza risorse (punto unico, regola L): nessuna riga DB con
    //    QUESTA label esatta. Prima di allocare una porta nuova, chiedi al
    //    resolver se un servizio dello STESSO scopo (label/classe) e' gia' ATTIVO
    //    nel progetto. In tal caso riusa la sua porta come 'existing' e persisti
    //    la riga per questa label (idempotenza reale via UNIQUE(project_id,label)).
    //    Cosi' variare il contorno della label ("backend" -> "Backend Nodemon")
    //    non genera una nuova allocazione 'dynamic' (causa radice del loop
    //    request_port). Il matching e' a 2 classi disgiunte: una richiesta
    //    "backend" non riusa mai la porta di un "frontend" attivo.
    if let Some(res) =
        super::resource_resolver::resolve_for_label(registry, project_id, label).await
    {
        if res.listening {
            if let Some(existing_port) = res.port {
                tracing::info!(
                    label = %label,
                    matched_label = %res.label,
                    port = existing_port,
                    program = res.program.as_deref().unwrap_or("?"),
                    "find_or_allocate: servizio gia' ATTIVO dello stesso scopo, riuso la porta esistente (existing)"
                );
                // Persisti/aggiorna la riga per questa label: ON CONFLICT su
                // (project_id,label) garantisce idempotenza reale (indice mig 0434).
                let upsert = sqlx::query(
                    r#"
                    INSERT INTO nexus_port_allocations (project_id, port, label, allocation_mode)
                    VALUES ($1, $2, $3, 'existing')
                    ON CONFLICT (project_id, label)
                    DO UPDATE SET port = EXCLUDED.port,
                                  allocation_mode = EXCLUDED.allocation_mode,
                                  updated_at = NOW()
                    "#,
                )
                .bind(project_id)
                .bind(existing_port as i32)
                .bind(label)
                .execute(db)
                .await;
                if let Err(e) = upsert {
                    tracing::warn!(
                        "find_or_allocate: upsert existing fallito (porta {} label {}): {}",
                        existing_port,
                        label,
                        e
                    );
                }
                record_audit(
                    AuditEntry::allowed(project_id, "port_reuse", "port")
                        .with_resource(existing_port.to_string())
                        .with_details(serde_json::json!({
                            "label": label,
                            "matched_label": res.label,
                            "mode": "existing",
                        })),
                );
                return Ok(AllocatedPort {
                    port: existing_port,
                    mode: "existing",
                });
            }
        }
    }

    // 2. Quota check: non superare max_ports allocate per il progetto.
    if let Err(reason) = crate::security::quotas::check_can_allocate_port(db, project_id).await {
        record_audit(
            AuditEntry::blocked(project_id, "port_allocate", "port")
                .with_resource(label.to_string())
                .with_details(serde_json::json!({"reason": reason})),
        );
        return Err(reason);
    }

    // 3. Prima allocazione: porta DETERMINISTICA per (project_id, label) nel bucket
    //    (offset hash della label), non la prima libera dal basso. Cosi' la porta
    //    coincide con quella PROPOSTA da service_discovery/run_configs per la stessa
    //    label (entrambi usano deterministic_project_port_for_key) e la prima scelta
    //    non dipende dall'ordine di richiesta (A3: niente swap). La funzione fa gia'
    //    fallback a find_free_project_port se la porta ideale e' occupata.
    let port = deterministic_project_port_for_key(&project_id, label, registry).await;

    // 4. INSERT in DB. Idempotenza reale per (project_id, label) via indice
    //    UNIQUE (mig 0434): variare il contorno della label NON crea piu' righe
    //    duplicate. DO UPDATE aggiorna porta/mode/updated_at sull'allocazione
    //    esistente per quella label.
    let insert_result = sqlx::query(
        r#"
        INSERT INTO nexus_port_allocations (project_id, port, label, allocation_mode)
        VALUES ($1, $2, $3, 'dynamic')
        ON CONFLICT (project_id, label)
        DO UPDATE SET port = EXCLUDED.port,
                      allocation_mode = EXCLUDED.allocation_mode,
                      updated_at = NOW()
        "#,
    )
    .bind(project_id)
    .bind(port as i32)
    .bind(label)
    .execute(db)
    .await;
    if let Err(e) = insert_result {
        tracing::warn!(
            "allocate_port: INSERT fallito (porta {} label {}): {}",
            port,
            label,
            e
        );
    }

    // 5. Audit allocato
    record_audit(
        AuditEntry::allowed(project_id, "port_allocate", "port")
            .with_resource(port.to_string())
            .with_details(serde_json::json!({"label": label, "mode": "dynamic"})),
    );

    Ok(AllocatedPort {
        port,
        mode: "dynamic",
    })
}

pub async fn allocate_port(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<AllocatePortBody>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    let result = find_or_allocate(&state.db, &state.port_registry, project_id, &body.label)
        .await
        .map_err(|e| api_error(StatusCode::CONFLICT, &e))?;

    Ok(Json(json!({
        "port": result.port,
        "label": body.label.trim(),
        "allocation_mode": result.mode,
        "ok": true,
    })))
}

#[derive(serde::Deserialize)]
pub struct KillPortBody {
    pub port: u16,
}

/// POST /api/projects/:id/services/kill-port-process
///
/// Termina il processo in ascolto sulla porta specificata e rilascia
/// l'allocazione corrispondente da `nexus_port_allocations`. Utilizzato dal
/// pulsante "kill" del pannello Porte per pulire una porta sola alla volta.
pub async fn kill_port_process(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<KillPortBody>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _ctx = load_project_context(&state.db, project_id, user_id).await?;

    let freed = super::port_recovery::try_free_port(body.port).await;
    let deleted =
        sqlx::query("DELETE FROM nexus_port_allocations WHERE project_id = $1 AND port = $2")
            .bind(project_id)
            .bind(body.port as i32)
            .execute(&state.db)
            .await
            .map(|r| r.rows_affected())
            .unwrap_or(0);

    record_audit(
        AuditEntry::allowed(project_id, "port_kill", "port")
            .with_resource(body.port.to_string())
            .with_details(serde_json::json!({"freed": freed, "deleted_allocations": deleted})),
    );

    Ok(Json(json!({
        "ok": true,
        "port": body.port,
        "freed": freed,
        "deleted_allocations": deleted,
    })))
}

/// POST /api/projects/:id/services/kill-orphan-processes
///
/// Termina i processi del bucket porte del progetto che NON sono tracciati in
/// `agent_processes` (status running/starting). Risolve la proliferazione di
/// processi avviati fuori da Nexus (es. `pnpm dev` manuale lasciato attivo) che
/// occupano porte del bucket impedendo riallocazione pulita.
pub async fn kill_orphan_processes(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    let killed = super::port_recovery::kill_bucket_orphans(&state.db, project_id).await;
    record_audit(
        AuditEntry::allowed(project_id, "process_kill", "process")
            .with_resource(format!("orphans:{}", killed.len()))
            .with_details(serde_json::json!({ "pids": killed })),
    );

    Ok(Json(json!({
        "ok": true,
        "killed": killed.len(),
        "pids": killed,
    })))
}

#[cfg(test)]
mod tests {
    //! Test DB-gated (`#[sqlx::test]`: DB temporaneo isolato, niente ordine,
    //! niente stato condiviso). Verificano l'INVARIANTE introdotta dalla mig 0434
    //! e usata dal ramo idempotente di `find_or_allocate`: l'upsert su
    //! (project_id, label) con indice UNIQUE produce SEMPRE una sola riga e la
    //! stessa porta per la stessa label. E' il cuore del fix al loop request_port
    //! (variare il contorno della label NON deve creare righe duplicate / porte
    //! nuove). La funzione `find_or_allocate` completa dipende da `ss` (porte in
    //! LISTEN), quota e audit globali, quindi qui si testa la query SQL
    //! autoritativa in isolamento.
    use sqlx::Row;
    use uuid::Uuid;

    use super::{active_port_action, ActivePortAction};

    /// Regressione EADDRINUSE (incidente Beaty-Book 2026-07-02): con la porta
    /// dell'allocazione in LISTEN la decisione dipende dall'occupante. Il
    /// vecchio comportamento (sempre 'existing') iniettava porte occupate da
    /// orfani non tracciati nel nuovo processo.
    #[test]
    fn porta_attiva_decisione_per_occupante() {
        // Occupante tracciato (servizio legittimo): riusa, mai killare.
        assert_eq!(active_port_action(true, false), ActivePortAction::ReuseTracked);
        // `freed` e' irrilevante se tracciato (try_free_port non viene invocato).
        assert_eq!(active_port_action(true, true), ActivePortAction::ReuseTracked);
        // Orfano non tracciato liberato: riusa la stessa porta.
        assert_eq!(active_port_action(false, true), ActivePortAction::ReuseFreed);
        // Orfano non liberabile: MAI ritornare la porta occupata, rialloca.
        assert_eq!(active_port_action(false, false), ActivePortAction::ReallocateNew);
    }

    /// Crea uno schema minimo: solo le colonne usate dall'upsert + i due vincoli
    /// rilevanti (UNIQUE(port) di mig 0114 e UNIQUE(project_id,label) di mig 0434).
    async fn create_port_allocations_table(pool: &sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE nexus_port_allocations ( \
                 id UUID NOT NULL DEFAULT gen_random_uuid(), \
                 project_id UUID NOT NULL, \
                 port INT NOT NULL, \
                 label TEXT NOT NULL DEFAULT '', \
                 allocation_mode TEXT NOT NULL DEFAULT 'auto', \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                 CONSTRAINT uq_port UNIQUE (port) \
             )",
        )
        .execute(pool)
        .await
        .expect("create table nexus_port_allocations");
        sqlx::query(
            "CREATE UNIQUE INDEX uq_port_alloc_project_label \
             ON nexus_port_allocations (project_id, label)",
        )
        .execute(pool)
        .await
        .expect("create unique index project_label");
    }

    /// Replica esatta dell'upsert usato da `find_or_allocate` (sezione 4).
    async fn upsert_alloc(pool: &sqlx::PgPool, project_id: Uuid, port: i32, label: &str) {
        sqlx::query(
            r#"
            INSERT INTO nexus_port_allocations (project_id, port, label, allocation_mode)
            VALUES ($1, $2, $3, 'dynamic')
            ON CONFLICT (project_id, label)
            DO UPDATE SET port = EXCLUDED.port,
                          allocation_mode = EXCLUDED.allocation_mode,
                          updated_at = NOW()
            "#,
        )
        .bind(project_id)
        .bind(port)
        .bind(label)
        .execute(pool)
        .await
        .expect("upsert allocazione");
    }

    /// Replica dell'adozione di un'allocazione stantia usata da `find_or_allocate`
    /// (riuso della STESSA porta con mode='adopted', niente DELETE + re-alloc).
    async fn adopt_stale(pool: &sqlx::PgPool, project_id: Uuid, label: &str) {
        sqlx::query(
            "UPDATE nexus_port_allocations SET allocation_mode = 'adopted', updated_at = NOW() \
             WHERE project_id = $1 AND label = $2",
        )
        .bind(project_id)
        .bind(label)
        .execute(pool)
        .await
        .expect("adopt stale");
    }

    async fn count_rows(pool: &sqlx::PgPool, project_id: Uuid, label: &str) -> i64 {
        sqlx::query(
            "SELECT COUNT(*) AS n FROM nexus_port_allocations \
             WHERE project_id = $1 AND label = $2",
        )
        .bind(project_id)
        .bind(label)
        .fetch_one(pool)
        .await
        .expect("count")
        .get::<i64, _>("n")
    }

    #[sqlx::test]
    async fn upsert_stessa_label_una_sola_riga(pool: sqlx::PgPool) {
        create_port_allocations_table(&pool).await;
        let proj = Uuid::new_v4();

        // Due chiamate consecutive con la STESSA (project_id, label): l'indice
        // UNIQUE + ON CONFLICT DO UPDATE deve lasciare UNA sola riga, non due.
        upsert_alloc(&pool, proj, 21001, "backend").await;
        upsert_alloc(&pool, proj, 21001, "backend").await;

        assert_eq!(
            count_rows(&pool, proj, "backend").await,
            1,
            "due upsert sulla stessa (project,label) devono produrre UNA riga (idempotenza reale, mig 0434)"
        );
    }

    #[sqlx::test]
    async fn upsert_aggiorna_porta_stessa_label(pool: sqlx::PgPool) {
        create_port_allocations_table(&pool).await;
        let proj = Uuid::new_v4();

        upsert_alloc(&pool, proj, 21001, "backend").await;
        // Riuso di un servizio attivo (ramo 'existing'): la porta cambia ma la
        // label e' la stessa -> DO UPDATE aggiorna la riga esistente.
        upsert_alloc(&pool, proj, 21055, "backend").await;

        assert_eq!(count_rows(&pool, proj, "backend").await, 1);
        let port: i32 = sqlx::query(
            "SELECT port FROM nexus_port_allocations WHERE project_id = $1 AND label = $2",
        )
        .bind(proj)
        .bind("backend")
        .fetch_one(&pool)
        .await
        .expect("fetch port")
        .get::<i32, _>("port");
        assert_eq!(port, 21055, "DO UPDATE deve aggiornare la porta della riga esistente");
    }

    #[sqlx::test]
    async fn label_diverse_righe_distinte(pool: sqlx::PgPool) {
        create_port_allocations_table(&pool).await;
        let proj = Uuid::new_v4();

        // Scopi DIVERSI (label diverse) -> righe distinte: l'idempotenza e' per
        // label, non globale. Un servizio nuovo deve poter allocare.
        upsert_alloc(&pool, proj, 21001, "backend").await;
        upsert_alloc(&pool, proj, 21002, "frontend").await;

        let total: i64 = sqlx::query("SELECT COUNT(*) AS n FROM nexus_port_allocations WHERE project_id = $1")
            .bind(proj)
            .fetch_one(&pool)
            .await
            .expect("count total")
            .get::<i64, _>("n");
        assert_eq!(total, 2, "label distinte devono restare righe distinte");
    }

    #[sqlx::test]
    async fn adozione_stale_preserva_porta_e_riga(pool: sqlx::PgPool) {
        create_port_allocations_table(&pool).await;
        let proj = Uuid::new_v4();

        // Allocazione esistente (poi spenta): l'adozione deve RIUSARE la stessa
        // porta e mantenere UNA sola riga, marcandola 'adopted'. Niente DELETE +
        // riallocazione -> la porta resta stabile tra restart (fix deadlock
        // allocazione stantia).
        upsert_alloc(&pool, proj, 21951, "backend").await;
        adopt_stale(&pool, proj, "backend").await;

        assert_eq!(count_rows(&pool, proj, "backend").await, 1);
        let row = sqlx::query(
            "SELECT port, allocation_mode FROM nexus_port_allocations \
             WHERE project_id = $1 AND label = $2",
        )
        .bind(proj)
        .bind("backend")
        .fetch_one(&pool)
        .await
        .expect("fetch row adottata");
        assert_eq!(
            row.get::<i32, _>("port"),
            21951,
            "la porta stale deve essere riusata, non riallocata"
        );
        assert_eq!(
            row.get::<String, _>("allocation_mode"),
            "adopted",
            "l'allocazione stale adottata deve avere mode='adopted'"
        );
    }

    /// Replica del lookup idempotente (ramo 1 di `find_or_allocate`): la porta
    /// gia' persistita per (project_id, label) viene riusata.
    async fn lookup_port(pool: &sqlx::PgPool, project_id: Uuid, label: &str) -> Option<i32> {
        sqlx::query(
            "SELECT port FROM nexus_port_allocations \
             WHERE project_id = $1 AND label = $2 LIMIT 1",
        )
        .bind(project_id)
        .bind(label)
        .fetch_optional(pool)
        .await
        .expect("lookup")
        .map(|r| r.get::<i32, _>("port"))
    }

    #[sqlx::test]
    async fn binding_canonico_niente_swap_tra_restart(pool: sqlx::PgPool) {
        // A3: il binding porta<->servizio e' ancorato a (project_id, label) nel DB,
        // non all'ordine di avvio. Una volta che il wizard converge su
        // find_or_allocate, ogni servizio RIPRENDE la sua porta persistita a ogni
        // riavvio, indipendentemente da quale parte prima. Senza questo (vecchio
        // percorso deterministic_project_port_for_key hash + linear-probing
        // order-dependent) frontend e backend potevano scambiarsi la porta, e il
        // proxy /api del frontend finiva su una porta sbagliata (incidente login
        // Beauty-Book: VITE_API_URL verso la porta del frontend stesso).
        create_port_allocations_table(&pool).await;
        let proj = Uuid::new_v4();

        // Primo avvio: porte distinte e persistite.
        upsert_alloc(&pool, proj, 21950, "frontend").await;
        upsert_alloc(&pool, proj, 21976, "backend").await;

        // "Riavvio in ordine inverso": il lookup ancorato alla label ritorna SEMPRE
        // la stessa porta per ogni servizio. Niente swap.
        assert_eq!(
            lookup_port(&pool, proj, "backend").await,
            Some(21976),
            "backend deve riprendere la SUA porta persistita anche risolto per primo"
        );
        assert_eq!(
            lookup_port(&pool, proj, "frontend").await,
            Some(21950),
            "frontend deve riprendere la SUA porta persistita"
        );

        // Ri-risolvere non duplica righe ne' cambia le porte (idempotenza per label).
        assert_eq!(count_rows(&pool, proj, "frontend").await, 1);
        assert_eq!(count_rows(&pool, proj, "backend").await, 1);
    }
}
