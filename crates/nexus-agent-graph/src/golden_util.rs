//! Punto unico (regola L) per il CARICAMENTO dei golden-test di parita' Rust<->Python.
//!
//! I golden-test `#[ignore]` del crate confrontano l'output di una funzione Rust
//! con un oracolo (JSON in `/tmp`) generato a suo tempo dal riferimento Python
//! durante il porting. Dopo l'eliminazione del brain Python (zero-Python) i golden
//! NON sono piu' auto-rigenerabili: i JSON storici restano la fonte di verita'.
//!
//! [`load_golden`] centralizza il caricamento:
//!   - se il JSON esiste in `/tmp`, lo legge e ritorna `Some(contenuto)`;
//!   - se manca, ritorna `None`: il test chiamante salta in modo pulito (niente
//!     falso fallimento da file assente, niente parita' finta).
//!
//! NON re-implementare il caricamento di un golden altrove: delegare sempre qui.

#![cfg(test)]

use std::path::PathBuf;

/// Path del JSON golden in `std::env::temp_dir()` (su Linux: `/tmp`).
fn golden_json_path(json_filename: &str) -> PathBuf {
    std::env::temp_dir().join(json_filename)
}

/// Carica il contenuto di un golden-test dal JSON in `/tmp`.
///
/// - `json_filename`: nome del file JSON in `/tmp` (es. `"golden_planner.json"`).
/// - `_script_filename`: storicamente lo script generatore Python; conservato nella
///   firma per i call site ma non piu' usato (brain Python eliminato, zero-Python).
///
/// Ritorna `Some(contenuto)` se il golden e' presente, `None` se manca: in tal caso
/// il test chiamante deve saltare (`return`), evitando il falso fallimento da file
/// assente.
pub fn load_golden(json_filename: &str, _script_filename: &str) -> Option<String> {
    let json_path = golden_json_path(json_filename);
    if !json_path.exists() {
        eprintln!(
            "golden saltato: manca {} (rigenerazione Python rimossa: brain eliminato)",
            json_path.display()
        );
        return None;
    }
    let raw = std::fs::read_to_string(&json_path)
        .unwrap_or_else(|e| panic!("impossibile leggere il golden {}: {e}", json_path.display()));
    Some(raw)
}
