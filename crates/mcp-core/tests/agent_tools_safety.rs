//! Contract test (PR-4 Livello 3): i pattern di safety sono COMPILATI nel binario.
//!
//! mcp-core e' bin-only (no lib), quindi i pattern di safety non sono importabili
//! in un integration test. Si verifica allora che le loro stringhe siano presenti
//! nell'artefatto release: se una categoria della blacklist viene rimossa o
//! rinominata, qui si vede. La suite ricca vive negli unit test di
//! `src/agent_tools/safety.rs` (modulo `tests`).
//!
//! # Portabilita' (2026-07-26)
//!
//! Il test era Linux-only per due motivi, entrambi silenziosi: cercava
//! `target/release/mcp-core` senza `EXE_SUFFIX` — su Windows l'artefatto si chiama
//! `mcp-core.exe`, quindi `bin.exists()` era sempre falso — e leggeva le stringhe
//! invocando `strings(1)`, che su Windows non esiste. Su Windows saltava sempre, e
//! prima che lo skip diventasse visibile questo si presentava come un test
//! superato.
//!
//! Ora il percorso include il suffisso della piattaforma e le stringhe si cercano
//! nell'artefatto letto in memoria: `strings(1)` estrae le sequenze stampabili di
//! almeno 4 caratteri, ma per stabilire se un letterale e' stato compilato basta
//! cercarlo nel file — e' l'informazione che serve, senza dipendere da un
//! programma esterno ne' dalla sua soglia di lunghezza.
//!
//! Il confronto passa da `str::contains` su una conversione lossy, non da una
//! scansione `windows().any()` byte per byte: la seconda e' O(n*m) e su un
//! artefatto release costava 73 secondi per i quattro test (misurato), mentre
//! `contains` usa una ricerca a tempo quasi lineare. I byte non-UTF8 diventano
//! U+FFFD, ma un letterale ASCII e' contiguo nel file e resta intatto.

use nexus_test_preconditions::{salta, Motivo};
use std::sync::OnceLock;

/// Percorso dell'artefatto release, col suffisso eseguibile della piattaforma
/// (`.exe` su Windows, vuoto altrove): senza, il file non viene trovato dove
/// pure esiste.
fn release_binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("radice del workspace")
        .join("target")
        .join("release")
        .join(format!("mcp-core{}", std::env::consts::EXE_SUFFIX))
}

/// Contenuto dell'artefatto, letto e convertito UNA volta per l'intero binario di
/// test: i quattro test interrogano lo stesso file, e rileggerlo ogni volta
/// costerebbe centinaia di MB di I/O senza aggiungere niente.
fn testo_artefatto() -> Option<&'static str> {
    static CACHE: OnceLock<Option<String>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let bin = release_binary();
            if !bin.exists() {
                return None;
            }
            let byte = std::fs::read(&bin).ok()?;
            Some(String::from_utf8_lossy(&byte).into_owned())
        })
        .as_deref()
}

/// Il contenuto dell'artefatto, oppure skip dichiarato se non c'e'.
///
/// Distingue i due modi di non averlo: assente (va compilato) e illeggibile (c'e'
/// ma non si apre, es. un antivirus che lo tiene). Il vecchio messaggio unico
/// "binary non trovato" copriva anche il secondo caso, mandando a cercare una
/// build che c'era gia'.
fn artefatto_o_salta() -> Option<&'static str> {
    match testo_artefatto() {
        Some(b) => Some(b),
        None if !release_binary().exists() => {
            salta(Motivo::ArtefattoAssente(
                "binario release di mcp-core (cargo build -p mcp-core --release)",
            ));
            None
        }
        None => {
            salta(Motivo::ArtefattoAssente(
                "binario release di mcp-core presente ma illeggibile",
            ));
            None
        }
    }
}

#[test]
fn binary_contiene_tutti_i_pattern_safety_attesi() {
    let Some(artefatto) = artefatto_o_salta() else {
        return;
    };
    let attesi = [
        "db_access_nexus",
        "db_access_postgres",
        "db_default_target",
        "prisma_migrate_reset",
        "prisma_db_push_force",
        "sql_drop_database",
        "sql_drop_table_nexus",
        "sql_truncate_nexus",
        "sql_delete_nexus",
        "docker_exec_ideai",
        "docker_stop_ideai",
        "docker_compose_ideai",
        "docker_system_prune",
        "docker_stop_all",
        "fs_write_ideai",
        "fs_rm_rf_root",
        "kill_brain_mcp",
        "iptables_route",
        "systemctl_system",
        "database_url_nexus",
        "cat_env_nexus",
    ];
    for cat in attesi {
        assert!(
            artefatto.contains(cat),
            "pattern category '{cat}' assente nel binario release. Possibile regressione M63/M70."
        );
    }
}

/// Il contro-caso della ricerca: un letterale che NON e' nel codice non deve
/// risultare presente. Senza, gli altri tre test passerebbero anche se
/// l'artefatto fosse una stringa che contiene tutto (o se la lettura tornasse
/// spazzatura che matcha per caso): proverebbero se stessi invece del binario
/// (regola O). La stringa e' scelta per non poter comparire per caso.
#[test]
fn la_ricerca_nell_artefatto_non_e_sempre_vera() {
    let Some(artefatto) = artefatto_o_salta() else {
        return;
    };
    assert!(
        !artefatto.contains("pattern_che_non_esiste_in_nessun_sorgente_nexus_42"),
        "la ricerca nell'artefatto trova anche cio' che non c'e': le asserzioni \
         degli altri test non provano nulla"
    );
}

#[test]
fn binary_contiene_env_injection_db_progetto() {
    let Some(artefatto) = artefatto_o_salta() else {
        return;
    };
    for sym in [
        "NEXUS_PROJECT_DB_URL",
        "NEXUS_PROJECT_DB_NAME",
        "ensure_project_db_url",
        "/bin/bash",
    ] {
        assert!(
            artefatto.contains(sym),
            "stringa '{sym}' assente: regressione M72/L6 (env injection DB progetto + bash brace expansion)"
        );
    }
}

#[test]
fn binary_contiene_tool_subagent_poll_resume() {
    let Some(artefatto) = artefatto_o_salta() else {
        return;
    };
    for sym in [
        "nexus_subagent_poll",
        "nexus_subagent_resume",
        "dispatch_subagent",
        "nexus_todo_write",
    ] {
        assert!(
            artefatto.contains(sym),
            "tool '{sym}' assente nel binario: regressione PR-3"
        );
    }
}
