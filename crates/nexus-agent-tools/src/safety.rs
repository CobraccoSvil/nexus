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
    // pkill/killall per NOME di runtime generico condiviso (node, python, ...):
    // colpisce per nome di processo, quindi non isola il progetto e uccide anche
    // il web-ide di Nexus (Next.js = node), il brain (python) e gli altri
    // progetti. Causa radice di un incidente reale: `pkill node` lanciato da un
    // run su un progetto-utente ha ucciso il web-ide di Nexus (violazione
    // isolamento, CLAUDE.md regola E). Il pattern NON blocca `kill <PID>`
    // numerico ne' `pkill -f <path-assoluto-progetto>`: quelli isolano davvero.
    (
        "kill_generic_runtime",
        r#"(?i)\b(?:pkill|killall)\b(?:\s+-\S+)*\s+['"]?(?:node|nodejs|npm|npx|pnpm|yarn|next|next-server|vite|nodemon|ts-node|tsx|python|python3|deno|bun|java|php|ruby)\b"#,
        "pkill/killall per nome di runtime generico (node, python, ...) colpisce anche Nexus e gli altri progetti",
        "Termina solo i TUOI processi: individua i PID con `lsof -i :<porta_del_progetto>` e usa `kill <PID>`, oppure `pkill -f <path-assoluto-della-project-root>`. Mai pkill/killall per nome di runtime.",
    ),
    (
        "iptables_route",
        // NB: `systemctl` NON e' qui: e' gestito context-aware in check_command
        // (has_system_systemctl), perche' `systemctl --user <slug>-*.service` e' il
        // modo legittimo di gestire i servizi del PROGETTO. Qui restano le
        // operazioni sysadmin reali su routing/servizi SysV.
        r"(?i)\b(?:iptables|ip\s+route|service\s+\w+\s+(?:stop|restart))\b",
        "Modifica routing/servizi di sistema vietata",
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
    // ── 8. Bypass del broker di rete Nexus ─────────────────────────────────
    // Aggiunti in PR hardening: chiusura delle vie laterali per raggiungere
    // i servizi infrastruttura Nexus dai processi del progetto.
    (
        "nc_to_nexus_db",
        r"(?i)\bnc\b[^|;&]*\b(?:127\.0\.0\.1|localhost|host\.docker\.internal)\b[^|;&]*\b(?:5432|6379|6333|4000|50051|50052|50071|50072)\b",
        "Connessione netcat a porta infrastruttura Nexus",
        "Usa solo porte allocate via request_port per i tuoi servizi.",
    ),
    (
        "curl_to_internal",
        r"(?i)\bcurl\b[^|;&]*\bhttps?://(?:127\.0\.0\.1|localhost|host\.docker\.internal):(?:4000|4010|4020|4030|4040|4050|4055|4060|4070|50051|50052|50071|50072|8001)\b",
        "curl verso microservizi Nexus interni vietato",
        "Gli endpoint pubblici Nexus sono accessibili via http://localhost:3000/api/* (web-ide proxy).",
    ),
    (
        "ssh_outbound",
        r"(?i)\bssh\s+(?:-[a-zA-Z]+\s+\S+\s+)*\S+@\S+",
        "SSH outbound dal sandbox progetto vietato",
        "Le connessioni SSH non sono permesse. Per git push usa HTTPS+token o git_push tool.",
    ),
    (
        "setcap_chmod_suid",
        r"(?i)\b(?:setcap\s+\S+|chmod\s+(?:[ug]\+s|\d?[2-7][0-7]{3}))\b",
        "Escalation privilegi via setcap o chmod SUID/SGID vietata",
        "Il container sandbox e' non-privileged (cap-drop ALL); le escalation non avrebbero effetto comunque.",
    ),
];

/// RegexSet compilato una sola volta (lazy).
///
/// safety: i pattern in `FORBIDDEN_PATTERNS` sono literal hardcoded nello
/// stesso file. Se sono validi non panica mai; se non lo sono è un bug di
/// sviluppo individuato al primo lancio. Eccezione ammessa da CLAUDE.md §F
/// ("Regex::new su pattern compilato a build-time").
static FORBIDDEN_SET: Lazy<RegexSet> = Lazy::new(|| {
    RegexSet::new(FORBIDDEN_PATTERNS.iter().map(|(_, re, _, _)| *re))
        .expect("FORBIDDEN_PATTERNS contiene regex non valida — fix in safety.rs")
});

/// Verifica un comando shell contro la blacklist.
/// Ritorna `Some(BlockReason)` se va bloccato, `None` se OK.
/// True se il comando contiene almeno un `systemctl` di SISTEMA (cioe' NON
/// `systemctl --user`). I servizi del PROGETTO si gestiscono con
/// `systemctl --user <slug>-*.service` (lo fa Nexus stesso nel pannello
/// Run&Debug): vanno PERMESSI. `systemctl ...` di sistema e' sysadmin fuori scope:
/// va bloccato. Il crate `regex` non supporta il negative lookahead, quindi la
/// distinzione e' fatta qui con scansione manuale dei token (regola L: un solo
/// punto decide "systemctl progetto vs sistema").
fn has_system_systemctl(cmd: &str) -> bool {
    let lower = cmd.to_lowercase();
    let bytes = lower.as_bytes();
    let mut from = 0;
    while let Some(rel) = lower[from..].find("systemctl") {
        let start = from + rel;
        // Confine di parola a sinistra (evita match dentro identificatori).
        let left_ok =
            start == 0 || (!bytes[start - 1].is_ascii_alphanumeric() && bytes[start - 1] != b'_');
        if left_ok {
            let after = lower[start + "systemctl".len()..].trim_start();
            // `systemctl --user ...` -> servizi utente/progetto, permesso.
            if !after.starts_with("--user") {
                return true;
            }
        }
        from = start + "systemctl".len();
    }
    false
}

pub fn check_command(cmd: &str) -> Option<BlockReason> {
    let normalized = cmd.trim();
    if normalized.is_empty() {
        return None;
    }
    // systemctl context-aware (vedi has_system_systemctl): `systemctl --user`
    // (servizi del progetto) e' permesso; `systemctl` di sistema e' bloccato.
    if has_system_systemctl(normalized) {
        return Some(BlockReason {
            category: "systemctl_system",
            message: "Gestione servizi systemd di SISTEMA vietata",
            remediation: "Per i servizi del progetto usa `systemctl --user <slug>-<servizio>.service`. `systemctl` di sistema e' sysadmin, fuori scope progetto.",
        });
    }
    let matches: Vec<usize> = FORBIDDEN_SET.matches(normalized).into_iter().collect();
    if matches.is_empty() {
        return None;
    }
    let idx = matches[0];
    let (category, _re, message, remediation) = FORBIDDEN_PATTERNS[idx];
    Some(BlockReason {
        category,
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
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...[truncated]", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permette_systemctl_user_servizi_progetto() {
        // I servizi del progetto si gestiscono con systemctl --user: permesso.
        assert!(check_command("systemctl --user restart beauty-book-frontend.service").is_none());
        assert!(check_command(
            "systemctl --user daemon-reload && systemctl --user restart beauty-book-backend-dev.service"
        )
        .is_none());
        assert!(check_command("systemctl --user status beauty-book-frontend.service").is_none());
    }

    #[test]
    fn blocca_systemctl_di_sistema() {
        assert_eq!(
            check_command("systemctl restart nginx").unwrap().category,
            "systemctl_system"
        );
        assert!(check_command("sudo systemctl stop docker").is_some());
        // Misto: un --user e uno di sistema -> bloccato (c'e' un systemctl sistema).
        assert!(check_command("systemctl --user restart x && systemctl restart nginx").is_some());
    }

    #[test]
    fn blocca_ancora_iptables_e_iproute() {
        assert_eq!(
            check_command("iptables -A INPUT -j DROP").unwrap().category,
            "iptables_route"
        );
        assert_eq!(
            check_command("ip route add default via 1.2.3.4")
                .unwrap()
                .category,
            "iptables_route"
        );
    }

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
    fn blocca_pkill_node_generico() {
        // Incidente reale: `pkill node` ha ucciso il web-ide di Nexus (Next.js).
        let r = check_command("pkill node");
        assert!(r.is_some(), "pkill node deve essere bloccato");
        assert_eq!(r.unwrap().category, "kill_generic_runtime");
    }

    #[test]
    fn blocca_pkill_f_e_killall_runtime() {
        assert_eq!(
            check_command("pkill -f node").unwrap().category,
            "kill_generic_runtime"
        );
        assert_eq!(
            check_command("pkill -9 -f nodemon").unwrap().category,
            "kill_generic_runtime"
        );
        assert_eq!(
            check_command("killall -9 node").unwrap().category,
            "kill_generic_runtime"
        );
        assert_eq!(
            check_command("killall python3").unwrap().category,
            "kill_generic_runtime"
        );
        assert!(check_command("pkill -f vite").is_some());
        assert!(check_command("pkill ts-node").is_some());
    }

    #[test]
    fn permette_kill_pid_numerico() {
        // Il modo CORRETTO di terminare i propri orfani: kill per PID specifico
        // individuato via lsof sulla porta del progetto. Non deve essere bloccato.
        assert!(check_command("kill -9 731383 761157 805611").is_none());
        assert!(check_command("kill 12345").is_none());
    }

    #[test]
    fn permette_pkill_f_path_progetto() {
        // pkill -f su path assoluto del progetto isola davvero: permesso.
        assert!(
            check_command("pkill -f /home/administrator/projects/Beauty-Book").is_none(),
            "pkill -f <path-progetto> isola il progetto, va permesso"
        );
        // Nome di servizio specifico del progetto (non un runtime generico).
        assert!(check_command("pkill -f beauty-book-backend").is_none());
        // node_modules nel path non deve far scattare \bnode\b.
        assert!(check_command(
            "pkill -f /home/administrator/projects/x/node_modules/.bin/server"
        )
        .is_none());
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

    // ── Test pattern PR hardening (nc, curl interno, ssh, setcap) ─────────

    #[test]
    fn blocca_nc_a_porta_postgres_nexus() {
        let r = check_command("nc -zv 127.0.0.1 5432");
        assert!(r.is_some(), "nc verso :5432 deve essere bloccato");
        assert_eq!(r.unwrap().category, "nc_to_nexus_db");
    }

    #[test]
    fn blocca_nc_a_redis_nexus() {
        let r = check_command("nc localhost 6379");
        assert!(r.is_some());
        assert_eq!(r.unwrap().category, "nc_to_nexus_db");
    }

    #[test]
    fn permette_nc_a_porta_progetto() {
        // 30050 e' nel range progetti, non e' Nexus reserved
        assert!(check_command("nc -zv 127.0.0.1 30050").is_none());
    }

    #[test]
    fn blocca_curl_a_mcp_core() {
        let r = check_command("curl http://127.0.0.1:4000/api/health");
        assert!(
            r.is_some(),
            "curl verso mcp-core :4000 deve essere bloccato"
        );
        assert_eq!(r.unwrap().category, "curl_to_internal");
    }

    #[test]
    fn blocca_curl_a_admin_service() {
        let r = check_command("curl -s http://localhost:4010/admin/users");
        assert!(r.is_some());
        assert_eq!(r.unwrap().category, "curl_to_internal");
    }

    #[test]
    fn permette_curl_a_web_ide_proxy() {
        // web-ide :3000 e' il proxy pubblico, non e' nella regex curl_to_internal
        assert!(check_command("curl http://localhost:3000/api/projects").is_none());
    }

    #[test]
    fn blocca_ssh_outbound() {
        let r = check_command("ssh user@example.com");
        assert!(r.is_some());
        assert_eq!(r.unwrap().category, "ssh_outbound");
    }

    #[test]
    fn blocca_ssh_con_flags() {
        let r = check_command("ssh -i ~/.ssh/id_rsa root@server.example.com");
        assert!(r.is_some());
        assert_eq!(r.unwrap().category, "ssh_outbound");
    }

    #[test]
    fn blocca_setcap_e_chmod_suid() {
        assert!(check_command("setcap cap_net_bind_service+ep /usr/bin/foo").is_some());
        assert!(check_command("chmod u+s /usr/bin/myprog").is_some());
        assert!(check_command("chmod 4755 /usr/bin/myprog").is_some());
    }

    #[test]
    fn permette_chmod_normale() {
        assert!(check_command("chmod 755 ./build.sh").is_none());
        assert!(check_command("chmod +x ./run.sh").is_none());
    }
}
