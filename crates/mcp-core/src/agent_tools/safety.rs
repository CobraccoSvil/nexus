//! Guardrail di sicurezza per comandi shell e accesso a infrastruttura Nexus.
//!
//! Background: il 16/05/2026 un agent run ha droppato tabelle critiche
//! del DB Nexus (`agent_runs`, `agent_steps`, `nexus_agent_*`, `chat_sessions`)
//! eseguendo via `run_command` un comando psql/prisma che puntava al DB `nexus`
//! invece che al DB applicativo del progetto. Era anche stato eseguito
//! probabilmente `prisma migrate reset` o `db push --force-reset` su
//! connection string sbagliata.
//!
//! Questo modulo blocca a livello di tool dispatch i pattern noti come
//! distruttivi per l'infrastruttura Nexus, indipendentemente da:
//! - prompt utente
//! - intent/modalita' agente
//! - profile loader
//!
//! E' una difesa in profondita': anche se il prompt fosse compromesso
//! (jailbreak), il sanitizer DOVREBBE bloccare comunque.

use once_cell::sync::Lazy;
use regex::RegexSet;

/// Motivo del blocco (mostrato all'agente nel tool_result).
#[derive(Debug, Clone)]
pub struct BlockReason {
    pub category: &'static str,
    pub pattern_index: usize,
    pub message: &'static str,
    pub remediation: &'static str,
}

/// Pattern di comandi VIETATI nell'ambiente Nexus.
///
/// La regex usa flags case-insensitive. Per match piu' robusti, normalizziamo
/// l'input rimuovendo whitespace ridondante prima di applicare il match.
///
/// Ogni regola e' commentata col motivo del blocco.
static FORBIDDEN_PATTERNS: &[(&str, &str, &str, &str)] = &[
    // ── 1. Accesso diretto al DB Nexus via psql ─────────────────────────────
    (
        "db_access_nexus",
        r"(?i)\bpsql\b[^|;&]*\B-d\s+nexus\b",
        "Accesso al DB 'nexus' (infrastruttura Nexus) vietato",
        "Usa il DB applicativo del progetto (es. -d <slug_progetto>). Mai -d nexus.",
    ),
    (
        "db_access_postgres",
        r"(?i)\bpsql\b[^|;&]*\B-d\s+postgres\b",
        "Accesso al DB 'postgres' (admin cluster) vietato",
        "Usa il DB applicativo del progetto. Il DB 'postgres' e' admin-only.",
    ),
    (
        "db_default_target",
        r#"(?i)\bpsql\b\s+-h\s+localhost(?:\s+-p\s+\d+)?(?:\s+-U\s+\w+)?\s*(?:-c|<|")"#,
        "psql senza -d esplicito: il default e' il DB del user, potrebbe puntare a 'nexus'",
        "Specifica sempre -d <slug_progetto>. Esempio: psql -h localhost -p 5433 -U nexus -d rental -c '...'",
    ),
    // ── 2. Prisma comandi distruttivi ──────────────────────────────────────
    (
        "prisma_migrate_reset",
        r"(?i)\bprisma\s+migrate\s+reset\b",
        "prisma migrate reset cancella tutti i dati: distruttivo",
        "Usa 'prisma migrate dev --name <descrizione>' che e' additivo.",
    ),
    (
        "prisma_db_push_force",
        r"(?i)\bprisma\s+db\s+push\b[^|;&]*--(?:force-reset|accept-data-loss)\b",
        "prisma db push --force-reset/--accept-data-loss puo' cancellare dati",
        "Usa prisma migrate dev per cambiamenti schema controllati.",
    ),
    // ── 3. SQL DDL distruttivo ─────────────────────────────────────────────
    (
        "sql_drop_database",
        r"(?i)\bDROP\s+DATABASE\b",
        "DROP DATABASE e' distruttivo e non revocabile",
        "Niente DROP DATABASE in un task agente. Se serve cleanup, chiedi all'utente.",
    ),
    (
        "sql_drop_table_nexus",
        r"(?i)\bDROP\s+TABLE\b[^;]*\b(?:agent_runs|agent_steps|nexus_\w+|chat_sessions|settings|projects|users)\b",
        "DROP TABLE su tabella infrastruttura Nexus vietato",
        "Le tabelle Nexus (nexus_*, agent_*, chat_sessions, settings, projects, users) sono protette.",
    ),
    (
        "sql_truncate_nexus",
        r"(?i)\bTRUNCATE\b[^;]*\b(?:agent_runs|agent_steps|nexus_\w+|chat_sessions|settings)\b",
        "TRUNCATE su tabella infrastruttura Nexus vietato",
        "Le tabelle Nexus non vanno svuotate da un agente.",
    ),
    (
        "sql_delete_nexus",
        r"(?i)\bDELETE\s+FROM\s+(?:agent_runs|agent_steps|nexus_\w+|chat_sessions|settings|projects|users)\b",
        "DELETE FROM tabella infrastruttura Nexus vietato",
        "Non cancellare dati delle tabelle Nexus.",
    ),
    // ── 4. Docker su container Nexus ────────────────────────────────────────
    (
        "docker_exec_ideai",
        r"(?i)\bdocker\s+exec\b[^|;&]*\bideai-\S+",
        "docker exec su container Nexus (ideai-*) vietato",
        "I container ideai-* sono infrastruttura Nexus. Non chiamarli mai direttamente.",
    ),
    (
        "docker_stop_ideai",
        r"(?i)\bdocker\s+(?:stop|kill|rm|restart)\b[^|;&]*\bideai-\S+",
        "docker stop/kill/rm/restart su container Nexus vietato",
        "Non toccare container ideai-* (postgres-nexus, redis, qdrant, ecc.).",
    ),
    (
        "docker_compose_ideai",
        r"(?i)\bdocker\s+compose\b[^|;&]*\bideai-\S+",
        "docker compose su Nexus stack vietato",
        "Non eseguire docker compose sul compose Nexus.",
    ),
    (
        "docker_system_prune",
        r"(?i)\bdocker\s+system\s+prune\b",
        "docker system prune e' globale e tocca container Nexus",
        "Per cleanup, usa solo `docker compose -f <COMPOSE_PROGETTO> down` (compose del progetto utente).",
    ),
    (
        "docker_stop_all",
        r"(?i)\bdocker\s+stop\s+\$\(docker\s+ps",
        "docker stop $(docker ps) ferma TUTTI i container, inclusi Nexus",
        "Mai operazioni globali su docker ps. Filtra sempre.",
    ),
    // ── 5. Filesystem Nexus (path-traversal) ───────────────────────────────
    (
        "fs_write_ideai",
        r"(?i)(?:rm|cp|mv|chmod|chown|sed\s+-i|tee\b)[^|;&]*(?:/home/administrator/ideai|\$IDEAI_ROOT)",
        "Modifica filesystem Nexus (/home/administrator/ideai/...) vietata",
        "Resta dentro la project_root del task corrente.",
    ),
    (
        "fs_rm_rf_root",
        r"(?i)\brm\s+(?:-[rRfF]+|-r\s+-f|-f\s+-r)\s+/(?:etc|usr|bin|sbin|lib|lib64|opt|root|boot|home/administrator)(?:[/\s]|$)",
        "rm -rf su path di sistema o /home/administrator vietato",
        "Usa percorsi relativi alla project_root. Mai rm -rf su /, /home/administrator, /etc, /usr, ecc.",
    ),
    // ── 6. Network/system manipulation ─────────────────────────────────────
    (
        "kill_brain_mcp",
        r"(?i)\b(?:pkill|killall|kill\s+-9|kill\s+-KILL)\b[^|;&]*\b(?:mcp-core|brain\.grpc|nexus|postgres-nexus)\b",
        "kill processi infrastruttura Nexus vietato",
        "Non killare mcp-core, brain.grpc_server, postgres-nexus.",
    ),
    (
        "iptables_route",
        r"(?i)\b(?:iptables|ip\s+route|systemctl|service\s+\w+\s+(?:stop|restart))\b",
        "Modifica routing/servizi systemd vietata",
        "Operazioni sysadmin fuori scope progetto.",
    ),
    // ── 6b. DATABASE_URL puntato al DB nexus (M70) ─────────────────────────
    // L'agente NON deve mai usare il DB 'nexus' come target applicativo:
    // pattern match su DATABASE_URL=...@.../nexus o postgres:// senza dbname
    // o DATABASE_URL=...@.../postgres (cluster admin DB).
    (
        "database_url_nexus",
        r"(?i)\bDATABASE_URL\s*=\s*[^\s;|&]*@[^/\s]+(?::\d+)?/(?:nexus|postgres)\b",
        "DATABASE_URL puntato al DB nexus o postgres (infrastruttura): vietato",
        "Usa il DB applicativo dedicato al progetto. La variabile NEXUS_PROJECT_DB_URL viene iniettata automaticamente da Nexus e punta al DB dedicato del progetto attivo.",
    ),
    // ── 7. Secrets exfiltration ────────────────────────────────────────────
    (
        "cat_env_nexus",
        r"(?i)\b(?:cat|head|tail|less|more|grep)\b[^|;&]*(?:/home/administrator/ideai/[^|;&]*\.env|~/.ssh/|/etc/shadow)",
        "Lettura secrets/ssh/env Nexus vietata",
        "I file .env del workspace Nexus e le chiavi SSH sono off-limits.",
    ),
];

/// RegexSet compilato una sola volta (lazy).
static FORBIDDEN_SET: Lazy<RegexSet> = Lazy::new(|| {
    RegexSet::new(FORBIDDEN_PATTERNS.iter().map(|(_, re, _, _)| *re))
        .expect("FORBIDDEN_PATTERNS contiene regex non valida (bug)")
});

/// Verifica un comando shell contro la blacklist.
/// Ritorna `Some(BlockReason)` se va bloccato, `None` se OK.
pub fn check_command(cmd: &str) -> Option<BlockReason> {
    let normalized = cmd.trim();
    if normalized.is_empty() {
        return None;
    }
    let matches: Vec<usize> = FORBIDDEN_SET.matches(normalized).into_iter().collect();
    if matches.is_empty() {
        return None;
    }
    let idx = matches[0];
    let (category, _re, message, remediation) = FORBIDDEN_PATTERNS[idx];
    Some(BlockReason {
        category,
        pattern_index: idx,
        message,
        remediation,
    })
}

/// Formatta il messaggio di blocco per il tool_result (visibile all'agente).
pub fn format_blocked_result(cmd: &str, reason: &BlockReason) -> String {
    format!(
        "\u{274C} [security_guardrail] COMANDO BLOCCATO\n\nCategoria: {}\nMotivo: {}\n\nRimediazione: {}\n\nComando rifiutato: {}",
        reason.category,
        reason.message,
        reason.remediation,
        truncate_for_log(cmd, 300),
    )
}

fn truncate_for_log(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}...[truncated]", &s[..max]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocca_psql_db_nexus() {
        let r = check_command("psql -h localhost -p 5433 -U nexus -d nexus -c 'DROP TABLE foo'");
        assert!(r.is_some(), "deve bloccare psql -d nexus");
        assert_eq!(r.unwrap().category, "db_access_nexus");
    }

    #[test]
    fn blocca_prisma_migrate_reset() {
        let r = check_command("npx prisma migrate reset --force");
        assert!(r.is_some());
        assert_eq!(r.unwrap().category, "prisma_migrate_reset");
    }

    #[test]
    fn blocca_prisma_db_push_force() {
        let r = check_command("npx prisma db push --force-reset");
        assert!(r.is_some());
        assert_eq!(r.unwrap().category, "prisma_db_push_force");
    }

    #[test]
    fn blocca_drop_database() {
        let r = check_command("psql -d rental -c 'DROP DATABASE rental'");
        assert!(r.is_some());
        assert_eq!(r.unwrap().category, "sql_drop_database");
    }

    #[test]
    fn blocca_drop_table_nexus() {
        let r = check_command("DROP TABLE nexus_agent_plans;");
        assert!(r.is_some());
        assert_eq!(r.unwrap().category, "sql_drop_table_nexus");
    }

    #[test]
    fn blocca_drop_table_agent_runs() {
        let r = check_command("DROP TABLE agent_runs CASCADE");
        assert!(r.is_some());
    }

    #[test]
    fn blocca_truncate_settings() {
        let r = check_command("TRUNCATE TABLE settings");
        assert!(r.is_some());
        assert_eq!(r.unwrap().category, "sql_truncate_nexus");
    }

    #[test]
    fn blocca_delete_from_nexus_settings() {
        let r = check_command("DELETE FROM settings WHERE key='foo'");
        assert!(r.is_some());
        assert_eq!(r.unwrap().category, "sql_delete_nexus");
    }

    #[test]
    fn blocca_docker_exec_ideai() {
        // Questo comando match TWO patterns (db_access_nexus first, docker_exec_ideai second).
        // Entrambe valide: accettiamo l'una o l'altra.
        let r = check_command("docker exec ideai-postgres-nexus-1 psql -U nexus -d nexus");
        assert!(r.is_some());
        let cat = r.unwrap().category;
        assert!(
            cat == "docker_exec_ideai" || cat == "db_access_nexus",
            "got category {cat}",
        );
    }

    #[test]
    fn blocca_docker_exec_ideai_no_psql() {
        // Comando senza match db_access: solo docker_exec_ideai deve scattare.
        let r = check_command("docker exec ideai-redis-1 redis-cli FLUSHALL");
        assert!(r.is_some());
        assert_eq!(r.unwrap().category, "docker_exec_ideai");
    }

    #[test]
    fn blocca_docker_stop_ideai() {
        let r = check_command("docker stop ideai-postgres-nexus-1");
        assert!(r.is_some());
    }

    #[test]
    fn blocca_docker_system_prune() {
        let r = check_command("docker system prune -af");
        assert!(r.is_some());
        assert_eq!(r.unwrap().category, "docker_system_prune");
    }

    #[test]
    fn blocca_rm_rf_ideai() {
        let r = check_command("rm -rf /home/administrator/ideai/backups");
        assert!(r.is_some());
        assert_eq!(r.unwrap().category, "fs_write_ideai");
    }

    #[test]
    fn blocca_pkill_mcp_core() {
        let r = check_command("pkill -9 mcp-core");
        assert!(r.is_some());
        assert_eq!(r.unwrap().category, "kill_brain_mcp");
    }

    #[test]
    fn blocca_cat_ssh_key() {
        let r = check_command("cat ~/.ssh/id_rsa");
        assert!(r.is_some());
        assert_eq!(r.unwrap().category, "cat_env_nexus");
    }

    #[test]
    fn permette_psql_db_progetto() {
        let r = check_command("psql -h localhost -p 5433 -U nexus -d rental -c 'SELECT 1'");
        assert!(r.is_none(), "psql -d rental va permesso");
    }

    #[test]
    fn permette_prisma_migrate_dev() {
        let r = check_command("npx prisma migrate dev --name init");
        assert!(r.is_none(), "prisma migrate dev e' additivo, va permesso");
    }

    #[test]
    fn permette_npm_install() {
        let r = check_command("npm install");
        assert!(r.is_none());
    }

    #[test]
    fn permette_curl_health() {
        let r = check_command("curl http://localhost:32850/api/health");
        assert!(r.is_none());
    }

    #[test]
    fn permette_pnpm_verify() {
        let r = check_command("pnpm verify");
        assert!(r.is_none());
    }

    #[test]
    fn permette_ls_in_progetto() {
        let r = check_command("ls -la /home/administrator/projects/rental-app/");
        assert!(r.is_none());
    }

    #[test]
    fn blocca_case_insensitive() {
        let r = check_command("PSQL -d NEXUS -c 'drop table x'");
        assert!(r.is_some());
    }

    #[test]
    fn format_blocked_result_include_categoria() {
        let cmd = "psql -d nexus -c 'DROP TABLE foo'";
        let reason = check_command(cmd).unwrap();
        let msg = format_blocked_result(cmd, &reason);
        assert!(msg.contains("db_access_nexus"));
        assert!(msg.contains("COMANDO BLOCCATO"));
        assert!(msg.contains("Rimediazione"));
    }
}
