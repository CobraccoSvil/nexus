//! Punto unico (regola L) per il CARICAMENTO dei golden-test di parita' Rust<->Python.
//!
//! Tutti i golden-test `#[ignore]` del crate confrontano l'output di una funzione
//! Rust con un oracolo generato in Python e salvato come JSON in `/tmp`. Prima di
//! questo modulo ogni test ripeteva lo stesso `std::fs::read_to_string(...).expect(...)`
//! che PANICAVA se il file `/tmp` mancava (es. dopo una pulizia di `/tmp`, cambio di
//! sessione o di data): un FALSO fallimento (file assente, non divergenza di parita').
//!
//! [`load_golden`] centralizza il caricamento:
//!   - se il JSON esiste in `/tmp`, lo legge;
//!   - se manca ma lo script generatore esiste in `crates/nexus-agent-graph/scripts/`,
//!     lo esegue (`python3 <script>` con cwd = root del repo) e poi legge il JSON;
//!   - se manca e lo script generatore NON esiste nel repo, ritorna `None`: il test
//!     chiamante salta in modo pulito (niente falso fallimento, niente parita' finta).
//!
//! Il refresh manuale resta `python3 crates/nexus-agent-graph/scripts/gen_golden_X.py`:
//! l'helper rigenera SOLO se il JSON manca, per non rallentare le esecuzioni ripetute.
//!
//! NON re-implementare il caricamento di un golden altrove: delegare sempre qui.

#![cfg(test)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Directory degli script generatori, relativa a `CARGO_MANIFEST_DIR`.
fn scripts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts")
}

/// Root del repo: `CARGO_MANIFEST_DIR` e' `<root>/crates/nexus-agent-graph`,
/// quindi la root e' due livelli sopra. E' la cwd corretta per eseguire gli
/// script generatori (che importano il package `brain`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Path del JSON golden in `std::env::temp_dir()` (su Linux: `/tmp`).
fn golden_json_path(json_filename: &str) -> PathBuf {
    std::env::temp_dir().join(json_filename)
}

/// Carica il contenuto di un golden-test, auto-generandolo se manca.
///
/// - `json_filename`: nome del file JSON in `/tmp` (es. `"golden_planner.json"`).
/// - `script_filename`: nome dello script generatore in `scripts/`
///   (es. `"gen_golden_planner.py"`).
///
/// Ritorna `Some(contenuto)` se il golden e' disponibile (gia' presente o
/// auto-generato), `None` se non e' generabile perche' lo script non esiste nel
/// repo: in tal caso il test chiamante deve saltare (`return`), evitando il falso
/// fallimento da file mancante.
///
/// PANICA solo per problemi REALI: `python3` assente, script presente ma fallito,
/// o JSON illeggibile dopo una generazione riuscita. Questi non vanno mascherati.
pub fn load_golden(json_filename: &str, script_filename: &str) -> Option<String> {
    let json_path = golden_json_path(json_filename);

    if !json_path.exists() {
        let script_path = scripts_dir().join(script_filename);
        if !script_path.exists() {
            // Script generatore non versionato nel repo: il golden non e'
            // auto-generabile. Skip pulito (niente parita' finta).
            eprintln!(
                "golden saltato: manca {} e lo script generatore {} non esiste nel repo",
                json_path.display(),
                script_path.display(),
            );
            return None;
        }
        generate_golden(&script_path, &json_path);
    }

    let raw = std::fs::read_to_string(&json_path).unwrap_or_else(|e| {
        panic!(
            "impossibile leggere il golden {} dopo la generazione: {e}",
            json_path.display()
        )
    });
    Some(raw)
}

/// Esegue lo script generatore con `python3`, cwd = root del repo. Panica con
/// messaggio chiaro se `python3` manca o se lo script termina con errore: sono
/// problemi reali della pipeline, non vanno inghiottiti.
fn generate_golden(script_path: &Path, expected_json: &Path) {
    let output = Command::new("python3")
        .arg(script_path)
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "impossibile eseguire 'python3 {}': {e} (python3 installato e nel PATH?)",
                script_path.display()
            )
        });

    if !output.status.success() {
        panic!(
            "lo script generatore '{}' e' fallito (status {:?}):\nstdout:\n{}\nstderr:\n{}",
            script_path.display(),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    if !expected_json.exists() {
        panic!(
            "lo script '{}' e' terminato con successo ma non ha prodotto {}",
            script_path.display(),
            expected_json.display(),
        );
    }
}
