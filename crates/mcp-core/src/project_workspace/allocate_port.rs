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

use super::service_ownership::{self, ServiceOwnership};
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
/// e la scelta dipende da CHI la occupa e dalla liberabilita'.
#[derive(Debug, PartialEq, Eq)]
enum ActivePortAction {
    /// L'occupante E' il servizio richiesto (appartenenza provata da un dato
    /// strutturato): la porta e' sua, si riusa (existing).
    ReuseOwn,
    /// Occupante non attribuibile e non governato, ma la porta e' stata liberata
    /// (try_free_port): si riusa la stessa porta, ora libera.
    ReuseFreed,
    /// Porta occupata da un servizio ALTRUI, oppure da un occupante non
    /// attribuibile e non liberabile: mai ritornare una porta occupata
    /// (EADDRINUSE garantito al bind del nuovo processo) — si alloca una porta
    /// nuova segnalando l'occupante.
    ReallocateNew,
}

/// La decisione dipende dall'APPARTENENZA, non dal solo "e' tracciato dal
/// progetto": quel criterio rispondeva a una domanda piu' larga di quella
/// necessaria e faceva passare per legittimo l'occupante di un ALTRO servizio.
///
/// `freed` e' l'esito di `try_free_port`, che il chiamante invoca soltanto per
/// occupanti non attribuibili e non governati: un servizio altrui non si uccide
/// per prendergli la porta (lo spegnerebbe), e un processo governato dal progetto
/// nemmeno.
fn active_port_action(ownership: &ServiceOwnership, freed: bool) -> ActivePortAction {
    match ownership {
        ServiceOwnership::Own { .. } => ActivePortAction::ReuseOwn,
        // Appartenenza a un altro servizio PROVATA: la sua porta non e' la nostra.
        ServiceOwnership::Other { .. } => ActivePortAction::ReallocateNew,
        // Appartenenza IGNOTA: se la porta e' tornata libera si riusa, altrimenti
        // si alloca altrove. Non si eredita la porta di un processo di cui non si
        // sa nulla solo perche' vive nel progetto.
        ServiceOwnership::Unknown => {
            if freed {
                ActivePortAction::ReuseFreed
            } else {
                ActivePortAction::ReallocateNew
            }
        }
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
            // Beaty-Book 2026-07-02). Poi la domanda e' diventata "e' governato
            // dal progetto?", che rende legittimo anche l'occupante di un ALTRO
            // servizio. Ora la si pone stretta: "e' IL servizio richiesto?"
            // (punto unico `service_ownership`).
            let occupant = super::port_recovery::port_occupant(p).await;
            let ownership = match &occupant {
                Some((pid, _)) => {
                    service_ownership::ownership_of_occupant(db, project_id, *pid, label).await
                }
                None => ServiceOwnership::Unknown,
            };
            let tracked = match &occupant {
                Some((pid, _)) => super::port_recovery::is_tracked_pid(db, project_id, *pid).await,
                None => false,
            };
            // try_free_port ha side effect (kill dell'albero occupante): si
            // invoca SOLO per occupanti non governati dal progetto E non
            // attribuibili a un servizio. Uccidere un servizio altrui per
            // liberargli la porta e' il danno, non il rimedio. Rifiuta da se'
            // PID 0 e mcp-core stesso, e ritorna false se la porta resta occupata.
            let freed = if tracked || !matches!(ownership, ServiceOwnership::Unknown) {
                false
            } else {
                super::port_recovery::try_free_port(p).await
            };
            let occupant_pid = occupant.as_ref().map(|(pid, _)| *pid);
            let occupant_program = occupant
                .as_ref()
                .map(|(_, prog)| prog.clone())
                .unwrap_or_default();
            match active_port_action(&ownership, freed) {
                ActivePortAction::ReuseOwn => {
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
                    // Porta occupata da un servizio altrui, o da un occupante non
                    // attribuibile e non liberabile: si rialloca nel bucket (la
                    // funzione deterministica salta le porte gia' allocate in DB e
                    // quelle che non superano il bind di prova) e si aggiorna la
                    // riga della label. L'occupante viene SEGNALATO, mai ignorato.
                    let new_port =
                        deterministic_project_port_for_key(&project_id, label, registry).await;
                    tracing::warn!(
                        label = %label, busy_port = p, new_port,
                        occupant_pid = ?occupant_pid, occupant_program = %occupant_program,
                        ownership = %ownership.reason(),
                        "find_or_allocate: la porta allocata e' occupata da un processo che non e' questo servizio, rialloco su porta nuova"
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
                                "ownership": ownership.reason(),
                                "reason": "occupied_by_other_service",
                            })),
                    );
                    return Ok(AllocatedPort {
                        port: new_port,
                        mode: "reallocated",
                    });
                }
            }
        }
        // Allocazione "stale": nessuno in ascolto. Cerca processi del bucket che
        // siano DI QUESTO servizio (utente li ha lanciati manualmente con .env
        // hardcoded, oppure un avvio precedente di Nexus non e' stato tracciato).
        //
        // CAUSA RADICE chiusa qui (incidente gestione-spese 2026-07-28): il
        // criterio era "primo processo del bucket che somigli a un server", cioe'
        // rispondeva a "e' del progetto?" invece che a "e' IL servizio per cui sto
        // allocando?". Allocando per `frontend` adottava il BACKEND, e la porta di
        // quest'ultimo (occupata) finiva iniettata come PORT al frontend, che
        // ripiegava fuori bucket. L'appartenenza ora viene da dati strutturati
        // (`service_ownership`), mai dal nome del programma.
        let orphans = super::port_recovery::scan_bucket_orphans(db, project_id).await;
        let mut candidates = Vec::with_capacity(orphans.len());
        for (found_port, pid, program) in orphans {
            let ownership = service_ownership::ownership_of_bucket_listener(
                db, project_id, pid, found_port, label,
            )
            .await;
            candidates.push(service_ownership::ListenerFacts {
                port: found_port,
                pid,
                program,
                ownership,
            });
        }
        match service_ownership::resolve_stale_adoption(p, &candidates) {
            service_ownership::StaleAdoption::AdoptOrphan {
                port: found_port,
                pid,
            } => {
                tracing::info!(
                    label = %label, stale_port = p, adopted_port = found_port, pid,
                    "find_or_allocate: allocazione stale, adotto il processo DI QUESTO servizio"
                );
                let _ = sqlx::query(
                    "UPDATE nexus_port_allocations \
                     SET port = $1, allocation_mode = 'adopted', updated_at = NOW() \
                     WHERE project_id = $2 AND label = $3",
                )
                .bind(found_port as i32)
                .bind(project_id)
                .bind(label)
                .execute(db)
                .await;
                record_audit(
                    AuditEntry::allowed(project_id, "port_adopt", "port")
                        .with_resource(found_port.to_string())
                        .with_details(serde_json::json!({
                            "label": label, "stale_port": p, "pid": pid,
                            "reason": "listener_owned_by_service",
                        })),
                );
                return Ok(AllocatedPort {
                    port: found_port,
                    mode: "adopted",
                });
            }
            service_ownership::StaleAdoption::ReuseStale { .. } => {}
        }
        // Nessun processo del bucket e' dimostrabilmente di questo servizio. La
        // porta stale risulta libera (il probe TCP poco sopra e' negativo):
        // ADOTTALA riusando la STESSA porta — per stabilita' tra restart — invece
        // di eliminarla e riallocarne una nuova. La riga resta (UNIQUE
        // project_id,label) con mode='adopted'. Questo chiude il deadlock
        // allocazione stantia: run_service prosegue e riusa la stessa porta per la
        // label, senza dipendere da `service_restart`.
        //
        // I candidati SCARTATI si dichiarano (regola O: il verdetto senza la sua
        // premessa non e' diagnosticabile), cosi' dal log si vede quale processo e'
        // stato visto e perche' non e' stato adottato.
        if !candidates.is_empty() {
            let scartati: Vec<String> = candidates
                .iter()
                .map(|c| {
                    format!(
                        "porta {} pid {} ({}) -> {}",
                        c.port,
                        c.pid,
                        c.program,
                        c.ownership.reason()
                    )
                })
                .collect();
            tracing::info!(
                label = %label, stale_port = p, scartati = %scartati.join("; "),
                "find_or_allocate: nessun processo del bucket appartiene a questo servizio, riuso la sua porta"
            );
        }
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

    // 1-bis. Consapevolezza risorse: nessuna riga DB con QUESTA label esatta.
    //    Prima di allocare una porta nuova si guarda se il servizio e' gia' ATTIVO
    //    sotto un'altra label ("backend" -> "Backend Nodemon"): in tal caso se ne
    //    riusa la porta come 'existing' e si persiste la riga per questa label
    //    (idempotenza reale via UNIQUE(project_id,label)). E' il ramo che impedisce
    //    all'agente di accumulare allocazioni variando il contorno della label
    //    (causa radice del loop request_port).
    //
    //    Il candidato lo sceglie il verdetto di appartenenza, non il matching per
    //    CLASSE di `resolve_for_label`: a quel criterio basta che due label siano
    //    entrambe "di frontend" (la classe include web, ui, client, vue, next,
    //    react), quindi risponde a «stesso SCOPO?» dove qui serve «stesso
    //    SERVIZIO?». Con `owned_listener` si riusa solo la porta di un processo che
    //    un dato strutturato lega a QUESTO servizio; altrimenti si alloca dal
    //    bucket. Per i processi NON registrati la questione e' chiusa a monte:
    //    `orphan_placeholder_label` da' loro un identificatore posizionale, non uno
    //    scopo indovinato dal nome del programma (regola M).
    //
    //    Misurato prima di stringere (28-29/07/2026, DB dev): zero eventi
    //    `port_reuse` e zero righe `allocation_mode='existing'` su 114 eventi porta
    //    e 3 allocazioni, contro 59 `port_adopt` — nessun riuso legittimo dipendeva
    //    da questo ramo, il loop era retto dall'adozione. PREMESSA:
    //    `nexus_resource_audit` copriva solo quei due giorni, mentre il ramo esiste
    //    dal 15/06 (22618285).
    let attive = super::resource_resolver::resolve_project_resources(registry, project_id).await;
    let mut in_ascolto = Vec::new();
    for res in attive.services.iter().filter(|s| s.listening) {
        let (Some(res_port), Some(res_pid)) = (res.port, res.pid) else {
            continue;
        };
        let ownership = service_ownership::ownership_of_bucket_listener(
            db, project_id, res_pid, res_port, label,
        )
        .await;
        in_ascolto.push(service_ownership::ListenerFacts {
            port: res_port,
            pid: res_pid,
            program: res.program.clone().unwrap_or_default(),
            ownership,
        });
    }
    if let Some(mio) = service_ownership::owned_listener(&in_ascolto) {
        let existing_port = mio.port;
        tracing::info!(
            label = %label,
            port = existing_port,
            pid = mio.pid,
            program = %mio.program,
            ownership = %mio.ownership.reason(),
            "find_or_allocate: questo servizio e' gia' ATTIVO su un'altra porta, la riuso (existing)"
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
                    "ownership": mio.ownership.reason(),
                    "mode": "existing",
                })),
        );
        return Ok(AllocatedPort {
            port: existing_port,
            mode: "existing",
        });
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

/// Collega l'allocazione porta di `(project_id, label)` al `service_unit` del
/// servizio managed che la possiede.
///
/// Perche' serve (regola H, causa radice del drift Beaty-Book su Windows): il GC
/// `port_registry::cleanup_orphaned_ports` rilascia le allocazioni non-`manual`
/// rimaste senza listener TCP oltre la grace period, TRANNE quelle il cui
/// `service_unit` "riserva" la porta (`service_unit_reserves_port`). Su Windows
/// quella riserva scatta per la sola PRESENZA di un `service_unit` non vuoto
/// (non c'e' alcun file systemd da leggere). Ma le righe create da
/// `find_or_allocate` nascono con `service_unit = NULL`: senza questo link un
/// servizio managed FERMO perderebbe la sua porta al primo giro di GC (drift
/// 31792 -> 31798, pannello Porte svuotato).
///
/// Punto unico del linking allocazione -> unit (regola L): i call site che
/// avviano un servizio managed annotano qui la riga gia' materializzata da
/// `find_or_allocate`, invece di re-implementare l'UPDATE.
pub async fn link_allocation_to_service_unit(
    db: &PgPool,
    project_id: Uuid,
    label: &str,
    service_unit: &str,
) {
    let label = label.trim();
    let service_unit = service_unit.trim();
    if label.is_empty() || service_unit.is_empty() {
        return;
    }
    if let Err(e) = sqlx::query(
        "UPDATE nexus_port_allocations SET service_unit = $1, updated_at = NOW() \
         WHERE project_id = $2 AND label = $3",
    )
    .bind(service_unit)
    .bind(project_id)
    .bind(label)
    .execute(db)
    .await
    {
        tracing::warn!(
            "link_allocation_to_service_unit: UPDATE service_unit fallito (label {} unit {}): {}",
            label,
            service_unit,
            e
        );
    }
}

/// Quanto si attende che la porta del servizio torni bindabile dopo lo stop che
/// precede ogni avvio: il SO impiega qualche centinaio di ms a rilasciare il
/// listener del processo terminato. Stessa grazia di `ensure_process_stopped`
/// (10 x 300ms), che misura lo stesso fenomeno dall'altro lato — non una soglia
/// scelta a parte, che divergerebbe al primo ritocco.
const PORT_BINDABLE_ATTEMPTS: u32 = 10;
const PORT_BINDABLE_STEP_MS: u64 = 300;

/// PUNTO UNICO (regola L) dell'ALLOCA+INIETTA di un web service: alloca la porta
/// stabile del bucket, la lega all'unit del servizio e ne ricava l'env
/// `PORT`/`HOST` per lo spawn. I tre percorsi di avvio (pannello Servizi,
/// `service_manager`, tool agente `run_service`) delegano qui invece di ricopiare
/// la stessa sequenza.
///
/// PRETENDE una porta effettivamente bindabile, e questa e' la parte che mancava
/// (incidente gestione-spese 2026-07-28): iniettare `PORT` e' una PROMESSA al
/// processo che sta per nascere, e un framework che non e' in strictPort — Vite —
/// non fallisce se la trova occupata: ripiega su `port+1`, FUORI dal bucket del
/// progetto, in silenzio. Il servizio risulta avviato, il pannello mostra la
/// porta promessa, e l'indirizzo vero non lo conosce nessuno. Se la porta non si
/// libera entro la grazia si ritorna ERRORE: un avvio mancato e' visibile e
/// diagnosticabile, un servizio fuori bucket no.
pub async fn web_service_port_env(
    db: &PgPool,
    registry: &PortRegistryCache,
    project_id: Uuid,
    label: &str,
) -> Result<std::collections::HashMap<String, String>, String> {
    let alloc = find_or_allocate(db, registry, project_id, label)
        .await
        .map_err(|e| format!("allocazione porta per '{label}' fallita: {e}"))?;

    // L'allocazione va LEGATA all'unit del servizio, altrimenti nasce con
    // `service_unit` NULL e il GC (`cleanup_orphaned_ports`) la rilascia appena il
    // servizio e' fermo: al riavvio la porta cambia e il pannello non ha piu' un
    // indirizzo attendibile (drift 31792->31798, incidente Beaty-Book).
    if let Some(unit) = super::services::project_service_unit(db, project_id, label).await {
        link_allocation_to_service_unit(db, project_id, label, &unit).await;
    }

    if !super::port_recovery::wait_port_bindable(
        alloc.port,
        PORT_BINDABLE_ATTEMPTS,
        PORT_BINDABLE_STEP_MS,
    )
    .await
    {
        let occupante = super::port_recovery::port_occupant(alloc.port)
            .await
            .map(|(pid, prog)| format!("pid {pid} ({prog})"))
            .unwrap_or_else(|| "occupante non risolvibile".to_string());
        tracing::warn!(
            label = %label, port = alloc.port, mode = alloc.mode, occupante = %occupante,
            "web_service_port_env: la porta del servizio non e' bindabile, avvio non instradato"
        );
        return Err(format!(
            "la porta {} del servizio '{}' e' ancora occupata ({}): fermalo prima di riavviarlo. \
             Avviarlo ora lo farebbe ripiegare su una porta fuori dal bucket del progetto.",
            alloc.port, label, occupante
        ));
    }

    tracing::info!(
        port = alloc.port, label = %label, mode = alloc.mode,
        "web_service_port_env: PORT allocato, libero e iniettato"
    );
    let mut env = std::collections::HashMap::new();
    env.insert("PORT".to_string(), alloc.port.to_string());
    env.insert("HOST".to_string(), "0.0.0.0".to_string());
    Ok(env)
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

    use super::service_ownership::{classify_ownership, OwnershipFacts};
    use super::{active_port_action, ActivePortAction};

    /// I verdetti nascono da `classify_ownership` sulle label grezze (regola O:
    /// il test attraversa il produttore, non fabbrica il valore che verifica).
    fn occupante(process_label: Option<&str>, richiesta: &str) -> super::ServiceOwnership {
        let facts = OwnershipFacts::own_port_occupant(process_label.map(str::to_string));
        classify_ownership(richiesta, &facts)
    }

    /// Regressione EADDRINUSE (incidente Beaty-Book 2026-07-02) + appartenenza
    /// (incidente gestione-spese 2026-07-28): con la porta dell'allocazione in
    /// LISTEN la decisione dipende da CHI la occupa. Il primo comportamento
    /// (sempre 'existing') iniettava porte occupate nel nuovo processo; il
    /// secondo ("e' tracciato dal progetto?") faceva passare per legittimo
    /// l'occupante di un altro servizio.
    #[test]
    fn porta_attiva_decisione_per_appartenenza() {
        // L'occupante E' questo servizio: riusa, mai killare.
        assert_eq!(
            active_port_action(&occupante(Some("frontend"), "frontend"), false),
            ActivePortAction::ReuseOwn
        );
        // Variante del contorno della label: sempre lo stesso servizio.
        assert_eq!(
            active_port_action(&occupante(Some("frontend-dev"), "frontend"), false),
            ActivePortAction::ReuseOwn
        );
        // L'occupante e' un ALTRO servizio: mai ereditarne la porta, nemmeno se
        // risultasse liberata (non si uccide un servizio sano per la sua porta).
        assert_eq!(
            active_port_action(&occupante(Some("backend"), "frontend"), false),
            ActivePortAction::ReallocateNew
        );
        assert_eq!(
            active_port_action(&occupante(Some("backend"), "frontend"), true),
            ActivePortAction::ReallocateNew
        );
        // Occupante non attribuibile, porta liberata: riusa la stessa porta.
        assert_eq!(
            active_port_action(&occupante(None, "frontend"), true),
            ActivePortAction::ReuseFreed
        );
        // Non attribuibile e non liberabile: MAI ritornare la porta occupata.
        assert_eq!(
            active_port_action(&occupante(None, "frontend"), false),
            ActivePortAction::ReallocateNew
        );
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
                 service_unit TEXT, \
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
        assert_eq!(
            port, 21055,
            "DO UPDATE deve aggiornare la porta della riga esistente"
        );
    }

    #[sqlx::test]
    async fn label_diverse_righe_distinte(pool: sqlx::PgPool) {
        create_port_allocations_table(&pool).await;
        let proj = Uuid::new_v4();

        // Scopi DIVERSI (label diverse) -> righe distinte: l'idempotenza e' per
        // label, non globale. Un servizio nuovo deve poter allocare.
        upsert_alloc(&pool, proj, 21001, "backend").await;
        upsert_alloc(&pool, proj, 21002, "frontend").await;

        let total: i64 =
            sqlx::query("SELECT COUNT(*) AS n FROM nexus_port_allocations WHERE project_id = $1")
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

    async fn lookup_service_unit(
        pool: &sqlx::PgPool,
        project_id: Uuid,
        label: &str,
    ) -> Option<String> {
        sqlx::query(
            "SELECT service_unit FROM nexus_port_allocations \
             WHERE project_id = $1 AND label = $2 LIMIT 1",
        )
        .bind(project_id)
        .bind(label)
        .fetch_one(pool)
        .await
        .expect("lookup service_unit")
        .get::<Option<String>, _>("service_unit")
    }

    /// Regressione drift Beaty-Book su Windows: un'allocazione instradata da
    /// `find_or_allocate` nasce con `service_unit = NULL` -> il GC la rilascerebbe
    /// al primo giro (servizio managed fermo -> porta persa). `link_allocation_to_service_unit`
    /// deve annotare la riga esistente col service_unit, senza duplicarla.
    #[sqlx::test]
    async fn link_service_unit_annota_riga_esistente(pool: sqlx::PgPool) {
        create_port_allocations_table(&pool).await;
        let proj = Uuid::new_v4();

        // La riga nasce senza service_unit (come dall'upsert di find_or_allocate).
        upsert_alloc(&pool, proj, 31787, "backend").await;
        assert_eq!(
            lookup_service_unit(&pool, proj, "backend").await,
            None,
            "l'upsert di find_or_allocate non popola service_unit (nasce NULL)"
        );

        super::link_allocation_to_service_unit(
            &pool,
            proj,
            "backend",
            "beaty-book-backend.service",
        )
        .await;

        assert_eq!(
            lookup_service_unit(&pool, proj, "backend").await.as_deref(),
            Some("beaty-book-backend.service"),
            "dopo il link la riga deve riportare il service_unit (cosi' il GC la preserva)"
        );
        assert_eq!(
            count_rows(&pool, proj, "backend").await,
            1,
            "il link e' un UPDATE della riga esistente, non deve crearne una nuova"
        );
    }

    /// Guardie del punto unico: label o unit vuota -> no-op (nessun UPDATE, niente
    /// panic). Evita di sovrascrivere righe con un service_unit vuoto/spurio.
    #[sqlx::test]
    async fn link_service_unit_no_op_su_input_vuoto(pool: sqlx::PgPool) {
        create_port_allocations_table(&pool).await;
        let proj = Uuid::new_v4();
        upsert_alloc(&pool, proj, 31798, "frontend").await;

        super::link_allocation_to_service_unit(&pool, proj, "frontend", "   ").await;
        super::link_allocation_to_service_unit(&pool, proj, "", "x.service").await;

        assert_eq!(
            lookup_service_unit(&pool, proj, "frontend").await,
            None,
            "input vuoto (unit o label) non deve toccare service_unit"
        );
    }
}
