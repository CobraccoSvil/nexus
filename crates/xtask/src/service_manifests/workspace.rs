//! Quali binari il workspace produce davvero, chiesto a cargo.
//!
//! Non una lista di crate ricopiata: e' la stessa strada per cui un binario
//! esiste in produzione (regola O). Una lista scritta a mano qui sarebbe la
//! terza copia dopo il catalogo e il generatore, cioe' il difetto di partenza.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context};

/// Mappa `nome package` -> `nome del target [[bin]]`.
///
/// Il join con il catalogo e' su `name`: la voce di catalogo deve chiamarsi come
/// il package. Cargo non lo impone (e' un vincolo di progetto dichiarato
/// nell'ADR), quindi il messaggio di `BinarioAssente` lo ricorda.
pub fn bin_targets(root: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    let out = Command::new("cargo")
        .args([
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
        ])
        .current_dir(root)
        .output()
        .context(
            "esecuzione di `cargo metadata`: serve cargo nel PATH. Senza, l'inventario \
             dei binari non e' derivabile e generare i manifest sarebbe indovinare.",
        )?;
    if !out.status.success() {
        bail!(
            "`cargo metadata` fallito ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let mappa = estrai_bin(&out.stdout)?;
    if mappa.is_empty() {
        bail!(
            "`cargo metadata` non ha riportato alcun binario: l'inventario vuoto \
             renderebbe ogni voce di catalogo un BinarioAssente"
        );
    }
    Ok(mappa)
}

/// Estrae i target `bin` dall'output di `cargo metadata`, mappati sul package.
fn estrai_bin(stdout: &[u8]) -> anyhow::Result<BTreeMap<String, String>> {
    let v: serde_json::Value =
        serde_json::from_slice(stdout).context("output di `cargo metadata` non JSON")?;
    let pacchetti = v
        .get("packages")
        .and_then(|p| p.as_array())
        .context("`cargo metadata` senza campo packages")?;
    let mut mappa = BTreeMap::new();
    for p in pacchetti {
        let (Some(nome), Some(targets)) = (
            p.get("name").and_then(|n| n.as_str()),
            p.get("targets").and_then(|t| t.as_array()),
        ) else {
            continue;
        };
        for t in targets {
            let e_bin = t
                .get("kind")
                .and_then(|k| k.as_array())
                .is_some_and(|k| k.iter().any(|x| x.as_str() == Some("bin")));
            if e_bin {
                if let Some(bin) = t.get("name").and_then(|n| n.as_str()) {
                    mappa.insert(nome.to_string(), bin.to_string());
                }
            }
        }
    }
    Ok(mappa)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chiede a cargo, nel repo vero, e verifica che i servizi che devono
    /// esistere ci siano. MUTAZIONE: sostituire `bin_targets` con una lista
    /// scritta a mano e questo test resterebbe verde solo finche' la lista
    /// combacia — ma allora la lista sarebbe l'ennesima copia, che e' il punto.
    #[test]
    fn il_workspace_dichiara_i_binari_dei_servizi() {
        let root = super::super::repo_root().expect("repo root");
        let bins = bin_targets(&root).expect("cargo metadata");
        for atteso in [
            "mcp-core",
            "nexus-gateway",
            "admin-service",
            "doc-service",
            "plugin-service",
            "browser-bridge-mcp",
        ] {
            assert!(
                bins.contains_key(atteso),
                "il workspace non produce un binario '{atteso}': se il crate e' stato \
                 rimosso va tolto anche dal catalogo"
            );
        }
        // Il fossile non deve esserci: se tornasse, il piano lo segnalerebbe.
        assert!(!bins.contains_key("billing-service"));
    }

    /// Il vincolo su cui si regge il join, verificato sul repo reale.
    #[test]
    fn per_i_servizi_il_nome_del_package_e_quello_del_binario() {
        let root = super::super::repo_root().expect("repo root");
        let bins = bin_targets(&root).expect("cargo metadata");
        for s in [
            "mcp-core",
            "nexus-gateway",
            "admin-service",
            "doc-service",
            "plugin-service",
            "browser-bridge-mcp",
        ] {
            assert_eq!(
                bins.get(s).map(String::as_str),
                Some(s),
                "il join catalogo<->workspace e' su `name`: per '{s}' package e [[bin]] \
                 devono coincidere"
            );
        }
    }
}
