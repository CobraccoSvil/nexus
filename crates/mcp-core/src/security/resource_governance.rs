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

/// Guard SQL per-query (statement-level) per i tool DB del progetto. Distinto
/// dal detector di SQL-injection sui sorgenti (ADR 0021, tool
/// `sec_sql_injection_check`): qui si bloccano le query DISTRUTTIVE DI MASSA
/// eseguite dall'agente sul DB applicativo (wipe accidentale o accesso a
/// oggetti di sistema). Solo blocco (decisione utente: niente auto-fix su DB).
/// Funzione pura testabile. Ritorna `Some(motivo)` se la query va bloccata.
///
/// Conservativo per non rompere lo sviluppo legittimo dello schema:
///   - DELETE / UPDATE senza WHERE (wipe involontario dell'intera tabella);
///   - qualsiasi statement che tocca `pg_catalog`, `information_schema`, o
///     tabelle `nexus_*` / `_sqlx_migrations` (infrastruttura, fuori scope
///     progetto — il blocco del DB Nexus a livello connessione e' ortogonale);
///   - `DROP DATABASE` / `DROP SCHEMA` (oltre il singolo oggetto applicativo).
///     DROP/TRUNCATE di una singola tabella applicativa restano permessi (sviluppo).
pub fn check_dangerous_sql(sql: &str) -> Option<String> {
    // Normalizza: minuscolo, spazi collassati, niente commenti di riga.
    let lower = sql.to_lowercase();
    let no_comments: String = lower
        .lines()
        .map(|l| match l.find("--") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let norm = no_comments.split_whitespace().collect::<Vec<_>>().join(" ");

    // Oggetti di sistema / infrastruttura Nexus.
    for needle in [
        "pg_catalog",
        "information_schema",
        "_sqlx_migrations",
        " nexus_",
        "(nexus_",
        ".nexus_",
    ] {
        if norm.contains(needle) {
            return Some(format!(
                "la query tocca oggetti di sistema/infrastruttura ('{}'): operazione non consentita sul DB del progetto",
                needle.trim()
            ));
        }
    }

    // DROP/ALTER di interi database o schemi (oltre il singolo oggetto).
    if norm.contains("drop database") || norm.contains("drop schema") {
        return Some(
            "DROP DATABASE/SCHEMA non consentito (oltre lo scope del singolo oggetto applicativo)"
                .to_string(),
        );
    }

    // DELETE / UPDATE di massa senza WHERE (wipe involontario).
    let starts_delete = norm.starts_with("delete from ") || norm.starts_with("delete ");
    let starts_update = norm.starts_with("update ");
    if (starts_delete || starts_update) && !norm.contains(" where ") {
        let verb = if starts_delete { "DELETE" } else { "UPDATE" };
        return Some(format!(
            "{verb} senza clausola WHERE: cancellerebbe/modificherebbe l'intera tabella. Aggiungi un WHERE esplicito (o usa TRUNCATE consapevolmente sulla singola tabella)"
        ));
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

    #[test]
    fn dangerous_sql_blocca_mass_e_sistema() {
        // DELETE/UPDATE senza WHERE: bloccati.
        assert!(check_dangerous_sql("DELETE FROM users").is_some());
        assert!(check_dangerous_sql("update users set active = false").is_some());
        // Con WHERE: permessi.
        assert!(check_dangerous_sql("DELETE FROM users WHERE id = 1").is_none());
        assert!(check_dangerous_sql("UPDATE users SET active=false WHERE id=1").is_none());
        // Oggetti di sistema/infra: bloccati.
        assert!(check_dangerous_sql("SELECT * FROM pg_catalog.pg_tables").is_some());
        assert!(check_dangerous_sql("SELECT * FROM information_schema.tables").is_some());
        assert!(check_dangerous_sql("DROP TABLE nexus_routing_matrix").is_some());
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
