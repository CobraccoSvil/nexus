//! Punto unico di governance degli accessi alle risorse di sistema (regola L).
//!
//! Ogni richiesta di risorsa dei run agentici passa dai tool Nexus dedicati;
//! questo modulo orchestra i guard-rail trasversali:
//!   - catalogo policy DB-driven (`nexus_resource_policies`, mig 0397, cache 60s):
//!     UN posto per accendere/spegnere/configurare ogni regola (regola G);
//!   - `enforce_on_write`: dispatcher dei sub-scanner sui tool di scrittura
//!     (porte -> `agent_tools::port_scanner`, URL interni ->
//!     `agent_tools::url_scanner`); alla prima violazione bloccante audita e
//!     ritorna il messaggio di rifiuto;
//!   - `open_resource_violation`: registra una violazione come diagnosi
//!     `service_diagnoses.signal_kind='policy_violation'` (visibile nel
//!     pannello Problemi) con firma di dedup.
//!
//! I flag legacy (es. `agent.enforce_port_allocation`) restano come override
//! retro-compatibili: il sub-scanner porte li legge al suo interno.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use sqlx::PgPool;
use tokio::sync::RwLock;
use uuid::Uuid;

const POLICY_CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct ResourcePolicy {
    pub enabled: bool,
    #[expect(
        dead_code,
        reason = "mirror del catalogo DB nexus_resource_policies (mig 0397): dovra' sostituire gli hardcode \"error\" in resource_linter/port_enforcer, gap da chiudere senza amputare il contratto"
    )]
    pub severity: String,
    #[expect(
        dead_code,
        reason = "mirror del catalogo DB nexus_resource_policies (mig 0397): concetto vivo, oggi letto via SQL diretto in resource_violation_remediation"
    )]
    pub auto_remediate: bool,
    #[expect(
        dead_code,
        reason = "mirror del catalogo DB nexus_resource_policies (mig 0397): configurazione dei sub-scanner per il lavoro residuo fs/db/container"
    )]
    pub params: serde_json::Value,
}

static POLICY_CACHE: Lazy<RwLock<Option<(HashMap<(String, String), ResourcePolicy>, Instant)>>> =
    Lazy::new(|| RwLock::new(None));

/// Carica il catalogo policy (cache 60s). Se la tabella e' irraggiungibile o
/// vuota, il catalogo e' vuoto e `policy()` ritorna il default (enabled=true,
/// no auto-remediate): i guard di sicurezza non si spengono per un errore DB.
async fn catalog(db: &PgPool) -> HashMap<(String, String), ResourcePolicy> {
    {
        let guard = POLICY_CACHE.read().await;
        if let Some((map, loaded_at)) = guard.as_ref() {
            if loaded_at.elapsed() < POLICY_CACHE_TTL {
                return map.clone();
            }
        }
    }
    let mut map = HashMap::new();
    match sqlx::query_as::<_, (String, String, bool, String, bool, serde_json::Value)>(
        "SELECT resource_kind, rule_key, enabled, severity, auto_remediate, params \
         FROM nexus_resource_policies",
    )
    .fetch_all(db)
    .await
    {
        Ok(rows) => {
            for (kind, rule, enabled, severity, auto_remediate, params) in rows {
                map.insert(
                    (kind, rule),
                    ResourcePolicy {
                        enabled,
                        severity,
                        auto_remediate,
                        params,
                    },
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "resource_governance: lettura nexus_resource_policies fallita — default enabled"
            );
        }
    }
    let mut guard = POLICY_CACHE.write().await;
    *guard = Some((map.clone(), Instant::now()));
    map
}

/// Policy per (kind, rule). Default fail-safe: enabled=true (i guard non si
/// spengono se la riga manca), auto_remediate=false (nessuna riparazione
/// automatica non dichiarata).
pub async fn policy(db: &PgPool, kind: &str, rule: &str) -> ResourcePolicy {
    catalog(db)
        .await
        .get(&(kind.to_string(), rule.to_string()))
        .cloned()
        .unwrap_or(ResourcePolicy {
            enabled: true,
            severity: "error".to_string(),
            auto_remediate: false,
            params: serde_json::Value::Object(Default::default()),
        })
}

#[cfg(test)]
pub async fn _reset_policy_cache_for_tests() {
    let mut guard = POLICY_CACHE.write().await;
    *guard = None;
}

/// Dispatcher di enforcement sui tool di scrittura (write_file/edit_file).
/// Esegue i sub-scanner abilitati dal catalogo; alla prima violazione audita
/// (dentro il sub-scanner) e ritorna `Some(messaggio di rifiuto)`.
pub async fn enforce_on_write(
    ctx: &nexus_agent_tools::ToolContextCore,
    tool_name: &str,
    path: &str,
    content: &str,
) -> Option<String> {
    // Placeholder di redazione copiati come valori (incidente Beaty-Book
    // 2026-07-02): un `[REDACTED:...]` / `__NEXUS_..._N__` persistito in un
    // file e' sempre un segnaposto, mai un valore. Punto unico:
    // security::redaction_guard (regola L).
    if let Some(msg) = crate::security::redaction_guard::enforce_no_redacted_placeholder(
        ctx, tool_name, "content", content,
    )
    .await
    {
        return Some(msg);
    }

    // Porte (ADR 0010): il sub-scanner contiene gia' il gate legacy
    // `agent.enforce_port_allocation` + audit. Le due regole del catalogo
    // (enforce_hardcode, require_allocation) sono valutate insieme: il
    // sub-scanner fa entrambe le passate.
    let port_policy = policy(ctx.db.as_ref(), "port", "enforce_hardcode").await;
    if port_policy.enabled {
        if let Some(msg) =
            crate::agent_tools::port_scanner::enforce_write_ports(ctx, tool_name, path, content)
                .await
        {
            return Some(msg);
        }
    }

    // URL interni hardcoded (classe network).
    let url_policy = policy(ctx.db.as_ref(), "network", "no_hardcoded_internal").await;
    if url_policy.enabled {
        let findings = crate::agent_tools::url_scanner::collect_internal_urls(path, content);
        if !findings.is_empty() {
            audit_url_rejection(ctx, tool_name, path, &findings);
            return Some(crate::agent_tools::url_scanner::format_url_reject_message(
                path, &findings,
            ));
        }
    }

    // Quota disco (classe file): blocca la scrittura se il progetto e' oltre
    // max_disk_mb. Solo blocco (decisione utente: niente auto-fix su FS).
    let disk_policy = policy(ctx.db.as_ref(), "file", "disk_quota").await;
    if disk_policy.enabled {
        let root = ctx.root_path.to_string_lossy();
        if let Err(msg) =
            crate::security::quotas::check_can_use_disk(ctx.db.as_ref(), ctx.project_id, &root)
                .await
        {
            let mut entry = crate::security::AuditEntry::blocked(
                ctx.project_id,
                "fs_disk_quota_blocked",
                "file",
            )
            .with_resource(path.to_string())
            .with_details(serde_json::json!({ "tool": tool_name, "path": path }))
            .with_actor_user(ctx.user_id);
            if let Some(s) = ctx.session_id {
                entry = entry.with_actor_session(s);
            }
            crate::security::record_audit(entry);
            return Some(format!("[Errore: scrittura su '{path}' rifiutata. {msg}]"));
        }
    }

    None
}

/// Audit di una violazione URL respinta (resource_kind `network`).
fn audit_url_rejection(
    ctx: &nexus_agent_tools::ToolContextCore,
    tool_name: &str,
    path: &str,
    findings: &[crate::agent_tools::url_scanner::UrlFinding],
) {
    let detail_findings: Vec<serde_json::Value> = findings
        .iter()
        .take(5)
        .map(|f| {
            let snippet: String = f.snippet.chars().take(120).collect();
            serde_json::json!({ "line": f.line, "url": f.url, "snippet": snippet })
        })
        .collect();
    let resource_id: String = findings
        .first()
        .map(|f| f.url.chars().take(120).collect())
        .unwrap_or_default();
    let mut entry =
        crate::security::AuditEntry::blocked(ctx.project_id, "url_hardcode_rejected", "network")
            .with_resource(resource_id)
            .with_details(serde_json::json!({
                "tool": tool_name,
                "path": path,
                "count": findings.len(),
                "findings": detail_findings,
            }))
            .with_actor_user(ctx.user_id);
    if let Some(s) = ctx.session_id {
        entry = entry.with_actor_session(s);
    }
    crate::security::record_audit(entry);
}

/// Registra una violazione di governance come diagnosi `policy_violation` in
/// `service_diagnoses` (pannello Problemi). Dedup per firma: se esiste gia' una
/// diagnosi della stessa firma in stato open/diagnosing/failed_remediation,
/// non riapre. Ritorna l'id della diagnosi creata (None se dedup o errore).
///
/// `kind` = classe risorsa ('port'|'network'|...); `rule` = regola violata;
/// `file_path` = sorgente localizzato (relativo alla root) se noto.
#[allow(clippy::too_many_arguments)]
pub async fn open_resource_violation(
    db: &PgPool,
    project_id: Uuid,
    kind: &str,
    rule: &str,
    resource_value: f64,
    file_path: Option<&str>,
    detail: &str,
    signature: &str,
) -> Option<Uuid> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM service_diagnoses \
          WHERE project_id = $1 AND signal_kind = 'policy_violation' \
            AND error_signature_hash = $2 \
            AND status IN ('open', 'diagnosing', 'failed_remediation') \
          LIMIT 1",
    )
    .bind(project_id)
    .bind(signature)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    if existing.is_some() {
        return None;
    }

    let unit_label = file_path
        .map(|p| p.to_string())
        .unwrap_or_else(|| format!("runtime:{kind}"));
    match sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO service_diagnoses \
            (project_id, unit, signal_kind, metric, value, error_signature_hash, \
             status, detail, file_path) \
         VALUES ($1, $2, 'policy_violation', $3, $4, $5, 'open', $6, $7) \
         RETURNING id",
    )
    .bind(project_id)
    .bind(&unit_label)
    .bind(format!("{kind}/{rule}"))
    .bind(resource_value)
    .bind(signature)
    .bind(detail)
    .bind(file_path)
    .fetch_one(db)
    .await
    {
        Ok(id) => {
            tracing::warn!(
                project_id = %project_id,
                kind,
                rule,
                file_path = file_path.unwrap_or("-"),
                "resource_governance: violazione registrata come diagnosi {id}"
            );
            Some(id)
        }
        Err(e) => {
            tracing::warn!(error = %e, "resource_governance: INSERT diagnosi fallita");
            None
        }
    }
}

/// Chiude (resolved) le violazioni porta RUNTIME (senza sorgente localizzato:
/// `file_path IS NULL`, unit fittizia `runtime:port`) la cui porta NON risulta
/// piu' in violazione nella scansione corrente del port_enforcer.
///
/// PUNTO UNICO del ciclo di vita di queste diagnosi (regola L): le violazioni
/// STATICHE (con file sorgente) vengono richiuse dal resource_linter quando il
/// file smette di contenerle; quelle runtime non avevano NESSUN meccanismo di
/// chiusura e restavano 'open' a vita nel pannello Problemi anche quando il
/// processo era sparito da giorni (o era un falso positivo da PID riciclato).
/// `current`: coppie (project_id, porta) ancora in violazione in QUESTO scan —
/// tutte le altre diagnosi runtime aperte vengono risolte. Ritorna le righe
/// (project_id, id) risolte per il refresh realtime del pannello.
pub async fn resolve_stale_runtime_port_violations(
    db: &PgPool,
    current: &[(Uuid, f64)],
) -> Vec<(Uuid, Uuid)> {
    let (proj_ids, ports): (Vec<Uuid>, Vec<f64>) = current.iter().copied().unzip();
    sqlx::query_as::<_, (Uuid, Uuid)>(
        r#"UPDATE service_diagnoses d
           SET status = 'resolved', resolved_at = NOW()
           WHERE d.signal_kind = 'policy_violation'
             AND d.metric LIKE 'port/%'
             AND d.file_path IS NULL
             AND d.status IN ('open', 'diagnosing', 'failed_remediation')
             AND NOT EXISTS (
               SELECT 1
                 FROM UNNEST($1::uuid[], $2::float8[]) AS cur(project_id, port)
                WHERE cur.project_id = d.project_id
                  AND cur.port = d.value
             )
           RETURNING d.project_id, d.id"#,
    )
    .bind(&proj_ids)
    .bind(&ports)
    .fetch_all(db)
    .await
    .unwrap_or_default()
}

/// Causa per cui una statement e' respinta dal guard SQL. Enum chiuso (regola
/// Q): l'audit aggrega la [`regola`](MotivoBlocco::regola) canonica, il testo
/// per l'umano si compone DAI campi e non viene mai riletto da codice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MotivoBlocco {
    /// Statement NON di sola lettura che nomina un catalogo di sistema.
    ScritturaSuCatalogo { oggetto: String },
    /// Oggetto di infrastruttura Nexus (tabelle `nexus_*`, registro migrazioni).
    InfrastrutturaNexus { oggetto: String },
    /// `DROP DATABASE` / `DROP SCHEMA`.
    DropDatabaseOSchema,
    /// `DELETE` / `UPDATE` senza `WHERE`.
    ScritturaDiMassa { verbo: &'static str },
}

impl MotivoBlocco {
    /// Chiave macchina canonica (regola N): e' cio' che l'audit aggrega.
    pub fn regola(&self) -> &'static str {
        match self {
            MotivoBlocco::ScritturaSuCatalogo { .. } => "catalog_write",
            MotivoBlocco::InfrastrutturaNexus { .. } => "nexus_infrastructure",
            MotivoBlocco::DropDatabaseOSchema => "drop_database_or_schema",
            MotivoBlocco::ScritturaDiMassa { .. } => "mass_write_without_where",
        }
    }

    /// Testo per l'umano e per il modello, composto DAI campi (regola Q).
    pub fn motivo(&self) -> String {
        match self {
            MotivoBlocco::ScritturaSuCatalogo { oggetto } => format!(
                "la statement MODIFICA il catalogo di sistema '{oggetto}': consentita la sola \
                 lettura. Lo schema si cambia con DDL sugli oggetti applicativi, mai scrivendo \
                 sul catalogo"
            ),
            MotivoBlocco::InfrastrutturaNexus { oggetto } => format!(
                "la statement tocca un oggetto di infrastruttura Nexus ('{oggetto}'): fuori dallo \
                 scope del progetto (regola E). Per lo schema applicativo usa le sue tabelle"
            ),
            MotivoBlocco::DropDatabaseOSchema => {
                "DROP DATABASE/SCHEMA non consentito (oltre lo scope del singolo oggetto \
                 applicativo)"
                    .to_string()
            }
            MotivoBlocco::ScritturaDiMassa { verbo } => format!(
                "{verbo} senza clausola WHERE: cancellerebbe/modificherebbe l'intera tabella. \
                 Aggiungi un WHERE esplicito (o usa TRUNCATE consapevolmente sulla singola \
                 tabella)"
            ),
        }
    }
}

/// Cataloghi di sistema del database SU CUI la statement gira. Non sono
/// configurazione e non stanno nel DB (regola G non si applica): `pg_catalog` e
/// `information_schema` sono fissati da Postgres e dallo standard SQL, e una
/// seconda verita' da tenere allineata a mano mentirebbe con l'aria di un
/// setting. Il kill-switch della regola resta `nexus_resource_policies`.
const CATALOGHI_DI_SISTEMA: [&str; 2] = ["pg_catalog", "information_schema"];

/// Prefisso delle tabelle di infrastruttura Nexus e registro delle migrazioni.
const PREFISSO_TABELLE_NEXUS: &str = "nexus_";
const REGISTRO_MIGRAZIONI: &str = "_sqlx_migrations";

/// I SOLI comandi che Postgres ammette dentro una CTE, cioe' gli unici modi in
/// cui una statement classificata "lettura" puo' comunque modificare qualcosa
/// (`WITH d AS (DELETE FROM pg_catalog.x ...) SELECT 1`). Non e' una lista di
/// parole che "sembrano pericolose": e' cio' che il motore consente in quella
/// posizione. Le DDL non entrano in una CTE, e se aprono la statement questa
/// non e' classificata lettura.
const COMANDI_CHE_MODIFICANO: [&str; 4] = ["insert", "update", "delete", "merge"];

/// Guard SQL per-statement per i tool DB del progetto. Distinto dal detector di
/// SQL-injection sui sorgenti (ADR 0021, tool `sec_sql_injection_check`): quello
/// chiede "questo CODICE costruisce SQL in modo non sicuro?", questo chiede
/// "questa STATEMENT puo' girare sul DB applicativo del progetto?". Due domande
/// diverse, due punti (regola L). Solo blocco (decisione utente: niente auto-fix
/// su DB). Funzione pura testabile.
///
/// ## Perche' leggere `information_schema` e' legittimo
///
/// MISURATO il 09/08/2026 su gestione-corsi: l'agente aveva appena eseguito
/// `dotnet ef database update` e il task gli chiedeva di verificare che lo
/// schema risultante contenesse le tabelle attese. La sua
/// `SELECT ... FROM information_schema.tables` e' stata respinta con "la query
/// tocca oggetti di sistema/infrastruttura", e non esisteva altro modo di
/// accertare il proprio lavoro.
///
/// Il criterio confondeva DUE domande:
///   - "questa query tocca l'infrastruttura di NEXUS?" — divieto giusto, e gia'
///     applicato DOVE si decide: [`nexus_project_db::exec::classifica_connessione`]
///     rifiuta sia il DB META sia il DB metadati per-progetto. Il tool non puo'
///     aprirci un pool, quindi nessuna analisi del testo serve a impedirlo;
///   - "questa query legge i metadati dello schema DEL PROGETTO?" — sola
///     lettura, sul database che l'agente ha appena creato e migrato.
///
/// Che il divieto fosse lessicale e non strutturale lo dice il codice stesso:
/// sulla STESSA connessione, `nexus_db_tables` e `nexus_db_describe`
/// interrogano `information_schema.tables`/`.columns`/`pg_indexes`, e
/// `project_db_routes::query::count_public_tables` esegue letteralmente
/// `SELECT COUNT(*) FROM information_schema.tables` passando da `execute_query`.
/// Il pannello SQL (umano) non ha alcun guard. Era vietato solo cio' che
/// l'agente DIGITAVA.
///
/// ## Cosa resta vietato
///
///   - qualunque statement che nomini un oggetto `nexus_*` o `_sqlx_migrations`
///     (seconda linea rispetto al criterio di connessione: copre la riga
///     registrata a mano che punti a un DB Nexus dichiarandosi applicativa);
///   - le SCRITTURE sui cataloghi di sistema, comprese quelle nascoste in una
///     CTE dentro una statement che per il primo token sembra una lettura;
///   - `DROP DATABASE` / `DROP SCHEMA` (DROP/TRUNCATE di una singola tabella
///     applicativa restano permessi: e' sviluppo dello schema);
///   - `DELETE` / `UPDATE` senza `WHERE` (wipe involontario).
///
/// ## Per-statement, con lo stesso splitter dell'esecutore
///
/// Il giudizio si applica a OGNI statement dello script, ottenute da
/// [`nexus_project_db::exec::split_statements`] — lo stesso punto unico che
/// `execute_query` usa per decidere cosa eseguire (regola O). Prima il testo
/// era normalizzato come un blocco unico e la regola di massa guardava il primo
/// token: `SELECT 1; DELETE FROM users` non veniva vista, perche' il blocco
/// "inizia con select".
pub fn check_dangerous_sql(sql: &str) -> Option<MotivoBlocco> {
    crate::project_db::exec::split_statements(sql)
        .iter()
        .find_map(|stmt| blocco_di_statement(stmt))
}

/// Normalizza una statement: minuscolo, niente commenti di riga, spazi
/// collassati. I commenti vanno via prima del controllo su `WHERE`, altrimenti
/// `DELETE FROM users -- WHERE id=1` passerebbe.
fn normalizza(stmt: &str) -> String {
    let lower = stmt.to_lowercase();
    let no_comments: String = lower
        .lines()
        .map(|l| match l.find("--") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join(" ");
    no_comments.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parole della statement normalizzata: tutto cio' che Postgres leggerebbe come
/// un identificatore. Sostituisce i needle a sottostringa (` nexus_`, `(nexus_`,
/// `.nexus_`), che erano tre varianti di una proprieta' sola e ne mancavano
/// altre — `from a,nexus_x` non combaciava con nessuna delle tre.
fn parole(norm: &str) -> impl Iterator<Item = &str> {
    norm.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty())
}

/// «Questa statement puo' SOLO leggere?» Delega la classificazione al punto
/// unico che l'esecutore usa per scegliere `fetch_all` vs `execute`, e vi
/// aggiunge la sola cosa che quello non risponde: un comando che modifica
/// dentro una CTE. Sono due domande diverse — `is_read_only` chiede "ritorna
/// righe?", questa chiede "puo' cambiare qualcosa?" — quindi non e' una seconda
/// copia dello stesso criterio.
fn e_sola_lettura(stmt: &str, norm: &str) -> bool {
    crate::project_db::exec::is_read_only(stmt)
        && !parole(norm).any(|p| COMANDI_CHE_MODIFICANO.contains(&p))
}

/// Verdetto su UNA statement gia' separata dallo script.
fn blocco_di_statement(stmt: &str) -> Option<MotivoBlocco> {
    let norm = normalizza(stmt);

    // 1. Infrastruttura Nexus: mai, in nessuna forma e in nessuna posizione.
    if let Some(oggetto) = parole(&norm)
        .find(|p| p.starts_with(PREFISSO_TABELLE_NEXUS) || *p == REGISTRO_MIGRAZIONI)
    {
        return Some(MotivoBlocco::InfrastrutturaNexus {
            oggetto: oggetto.to_string(),
        });
    }

    // 2. Cataloghi di sistema: leggibili, mai scrivibili.
    if let Some(catalogo) = parole(&norm).find(|p| CATALOGHI_DI_SISTEMA.contains(p)) {
        if !e_sola_lettura(stmt, &norm) {
            return Some(MotivoBlocco::ScritturaSuCatalogo {
                oggetto: catalogo.to_string(),
            });
        }
    }

    // 3. DROP di interi database o schemi (oltre il singolo oggetto).
    if norm.contains("drop database") || norm.contains("drop schema") {
        return Some(MotivoBlocco::DropDatabaseOSchema);
    }

    // 4. DELETE / UPDATE di massa senza WHERE (wipe involontario).
    let starts_delete = norm.starts_with("delete from ") || norm.starts_with("delete ");
    let starts_update = norm.starts_with("update ");
    if (starts_delete || starts_update) && !norm.contains(" where ") {
        return Some(MotivoBlocco::ScritturaDiMassa {
            verbo: if starts_delete { "DELETE" } else { "UPDATE" },
        });
    }

    None
}

/// Firma stabile di dedup per una violazione (project + sorgente + valore + regola).
pub fn violation_signature(
    project_id: Uuid,
    file_path: Option<&str>,
    value: &str,
    rule: &str,
) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    project_id.hash(&mut h);
    file_path.unwrap_or("runtime").hash(&mut h);
    value.hash(&mut h);
    rule.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La query REALE dell'incidente del 09/08/2026 (gestione-corsi): l'agente
    /// ha appena eseguito `dotnet ef database update` e deve accertare che lo
    /// schema contenga le tabelle attese. Passa: e' sola lettura, sul catalogo
    /// del DB che il tool raggiunge per costruzione.
    ///
    /// Il testo e' quello letto da `agent_steps.tool_input` sul DB-progetto
    /// (passo `nexus_db_query`, `status='failed'`, 13:34:40 UTC), punto e
    /// virgola compreso: e' l'input respinto, non una sua parafrasi (regola O).
    /// Sui tre DB-progetto vivi quel rifiuto e' l'UNICO che il guard abbia mai
    /// prodotto: il suo intero esercizio in produzione era un falso positivo.
    ///
    /// Mutazione che rende rosso: rimettere `information_schema` fra gli oggetti
    /// vietati incondizionatamente (cioe' spostare il controllo del catalogo
    /// prima di `e_sola_lettura`, o togliere il ramo di sola lettura).
    #[test]
    fn la_query_dell_incidente_legge_il_catalogo_del_proprio_db() {
        let incidente = "SELECT table_name FROM information_schema.tables \
                         WHERE table_schema = 'public' ORDER BY table_name;";
        assert_eq!(
            check_dangerous_sql(incidente),
            None,
            "leggere lo schema del proprio DB e' l'unico modo di verificare una \
             migrazione appena applicata"
        );
        // Stessa cosa per i cataloghi pg_*, e per le forme che i tool nativi
        // (`nexus_db_tables`, `nexus_db_describe`) gia' eseguono sulla STESSA
        // connessione senza passare da questo guard.
        assert_eq!(
            check_dangerous_sql("SELECT * FROM pg_catalog.pg_tables"),
            None
        );
        assert_eq!(
            check_dangerous_sql(
                "SELECT t.table_name, c.reltuples FROM information_schema.tables t \
                 LEFT JOIN pg_class c ON c.relname = t.table_name WHERE t.table_schema = 'public'"
            ),
            None
        );
        // EXPLAIN e WITH sono letture per l'esecutore: restano letture qui.
        assert_eq!(
            check_dangerous_sql(
                "WITH t AS (SELECT table_name FROM information_schema.tables) SELECT * FROM t"
            ),
            None
        );
    }

    /// La lettura si apre, la SCRITTURA no — inclusa quella nascosta in una CTE
    /// dentro una statement che per il primo token sembra una lettura, che e'
    /// l'unico modo in cui Postgres consente di modificare da dentro un SELECT.
    ///
    /// Mutazione che rende rosso: in `e_sola_lettura`, togliere il controllo su
    /// `COMANDI_CHE_MODIFICANO` e affidarsi al solo `is_read_only` -> la CTE
    /// distruttiva passa.
    #[test]
    fn la_scrittura_sul_catalogo_resta_vietata() {
        for scrittura in [
            "UPDATE pg_catalog.pg_class SET relname = 'x' WHERE oid = 1",
            "DELETE FROM pg_catalog.pg_class WHERE oid = 1",
            "ALTER TABLE information_schema.tables ADD COLUMN x int",
            "CREATE TABLE spia AS SELECT * FROM information_schema.columns",
            // La forma insidiosa: `is_read_only` dice si', ma la CTE cancella.
            "WITH d AS (DELETE FROM pg_catalog.pg_class RETURNING 1) SELECT * FROM d",
            "EXPLAIN ANALYZE DELETE FROM pg_catalog.pg_class WHERE oid = 1",
        ] {
            let motivo = check_dangerous_sql(scrittura)
                .unwrap_or_else(|| panic!("doveva bloccare: {scrittura}"));
            assert_eq!(motivo.regola(), "catalog_write", "su: {scrittura}");
        }
    }

    /// L'infrastruttura Nexus resta vietata in qualunque posizione. Il criterio
    /// e' la PAROLA, non tre varianti di sottostringa: `from a,nexus_x` non
    /// combaciava con nessuno dei needle storici (` nexus_`, `(nexus_`,
    /// `.nexus_`) e passava.
    ///
    /// Mutazione che rende rosso: tornare ai needle a sottostringa -> l'ultimo
    /// caso passa.
    #[test]
    fn l_infrastruttura_nexus_resta_vietata() {
        for vietata in [
            "DROP TABLE nexus_routing_matrix",
            "SELECT * FROM public.nexus_agent_plans",
            "SELECT * FROM _sqlx_migrations",
            "SELECT * FROM a,nexus_port_allocations",
        ] {
            let motivo =
                check_dangerous_sql(vietata).unwrap_or_else(|| panic!("doveva bloccare: {vietata}"));
            assert_eq!(motivo.regola(), "nexus_infrastructure", "su: {vietata}");
        }
    }

    /// Il giudizio e' PER STATEMENT, sulle stesse statement che l'esecutore
    /// eseguira' (`split_statements`). Prima il testo era un blocco unico e la
    /// regola di massa guardava il primo token: un batch che inizia con SELECT
    /// nascondeva una DELETE senza WHERE.
    ///
    /// Mutazione che rende rosso: far tornare `check_dangerous_sql` a
    /// normalizzare l'intero `sql` invece di iterare su `split_statements`.
    #[test]
    fn ogni_statement_del_batch_e_giudicata() {
        let batch = "SELECT 1; DELETE FROM users";
        let motivo = check_dangerous_sql(batch).expect("la DELETE del batch va vista");
        assert_eq!(motivo.regola(), "mass_write_without_where");

        // Anche la scrittura sul catalogo in coda a una lettura legittima.
        let misto = "SELECT table_name FROM information_schema.tables; \
                     UPDATE pg_catalog.pg_class SET relname='x' WHERE oid=1";
        assert_eq!(
            check_dangerous_sql(misto).map(|m| m.regola()),
            Some("catalog_write")
        );

        // Un batch di sole letture del catalogo resta ammesso.
        assert_eq!(
            check_dangerous_sql(
                "SELECT count(*) FROM information_schema.tables; \
                 SELECT column_name FROM information_schema.columns WHERE table_name='corsi'"
            ),
            None
        );
    }

    #[test]
    fn dangerous_sql_blocca_mass_e_drop_di_database() {
        // DELETE/UPDATE senza WHERE: bloccati.
        assert!(check_dangerous_sql("DELETE FROM users").is_some());
        assert!(check_dangerous_sql("update users set active = false").is_some());
        // Con WHERE: permessi.
        assert!(check_dangerous_sql("DELETE FROM users WHERE id = 1").is_none());
        assert!(check_dangerous_sql("UPDATE users SET active=false WHERE id=1").is_none());
        // DROP database/schema: bloccato; DROP singola tabella app: permesso.
        assert!(check_dangerous_sql("DROP DATABASE app").is_some());
        assert!(check_dangerous_sql("DROP TABLE clients").is_none());
        assert!(check_dangerous_sql("TRUNCATE clients").is_none());
        // SELECT normale: permesso.
        assert!(check_dangerous_sql("SELECT * FROM clients WHERE id = 1").is_none());
        // Commento che nasconde il where non aiuta (where commentato = niente where).
        assert!(check_dangerous_sql("DELETE FROM users -- WHERE id=1").is_some());
    }

    #[test]
    fn signature_stabile_e_distinta() {
        let pid = Uuid::nil();
        let a = violation_signature(pid, Some("server.js"), "5000", "port/enforce_hardcode");
        let b = violation_signature(pid, Some("server.js"), "5000", "port/enforce_hardcode");
        assert_eq!(a, b, "stessa violazione -> stessa firma");
        let c = violation_signature(pid, Some("server.js"), "5173", "port/enforce_hardcode");
        assert_ne!(a, c, "porta diversa -> firma diversa");
        let d = violation_signature(pid, None, "5000", "port/enforce_hardcode");
        assert_ne!(a, d, "runtime vs file -> firma diversa");
    }
}
