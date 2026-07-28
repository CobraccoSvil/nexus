//! `build_info` — identita' temporale del binario IN ESECUZIONE. PUNTO UNICO
//! (regola L) per la domanda "quale artefatto sta girando?", che e' la domanda a
//! cui serve rispondere dopo un deploy.
//!
//! # Perche' non un timestamp inciso dalla build
//!
//! Prima il dato nasceva in `crates/mcp-core/build.rs`: `SystemTime::now()` al
//! momento in cui cargo eseguiva lo script, iniettato come `BUILD_TIMESTAMP` e
//! letto con `env!`. Ma cargo riesegue uno script di build solo quando cambiano
//! le dipendenze che lo script DICHIARA, e l'unica dichiarata era
//! `rerun-if-changed=build.rs`: il valore restava congelato all'ultima modifica
//! di quel file mentre il binario veniva ricompilato per qualunque altra
//! ragione (sorgenti del crate, crate dipendenti, feature).
//!
//! Misurato il 27/07/2026: `GET /health` dichiarava `build_time` = 1784526997
//! (20/07 07:56) su un binario linkato quel giorno alle 21:44 — e il binario
//! servito ERA quello nuovo (`Get-FileHash` di `D:\IDEAI-runtime\bin\debug\`
//! e di `target\debug\` coincidevano). Lo strumento con cui si verifica se il
//! deploy ha preso mentiva sulla versione in esecuzione (regola O).
//!
//! Aggiungere `rerun-if-changed=src` avrebbe spostato il confine senza chiuderlo
//! (i crate dipendenti restano fuori); forzare la riesecuzione a ogni build
//! avrebbe ricompilato il crate a ogni `cargo check`. Qui il valore non e' piu'
//! DEDOTTO dalla build: e' LETTO dal file da cui il processo e' stato caricato,
//! e per costruzione non puo' divergere dall'artefatto servito.
//!
//! # Perche' letto una volta sola, all'avvio
//!
//! Lo stamp e' memoizzato alla prima chiamata (che i binari fanno in fase di
//! avvio, vedi `main`). Su Unix un eseguibile puo' essere sostituito mentre il
//! processo gira: rileggere il disco a ogni `/health` farebbe dichiarare al
//! processo VECCHIO la data del binario NUOVO — l'inganno originale al
//! contrario. Il valore descrive il file da cui questo processo e' partito.

use std::path::Path;
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

/// Da dove viene il timestamp esposto: la premessa del numero (regola O — un
/// numero senza la sua premessa e' un'opinione). Identificatori canonici in
/// inglese, `snake_case` sul wire (regola N).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildStampSource {
    /// Data di ultima modifica del file da cui il processo e' stato caricato.
    ExeMtime,
    /// L'eseguibile non e' interrogabile (path irrisolvibile, file gia'
    /// sostituito sotto il processo, metadati negati). Il wire porta `"0"`:
    /// nessun ripiego che assomigli a un valore buono (regola G).
    #[default]
    Unknown,
}

/// Identita' temporale di un artefatto: il valore e la sua provenienza.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildStamp {
    pub unix_seconds: Option<u64>,
    pub source: BuildStampSource,
}

impl BuildStamp {
    /// Nessuna misura disponibile.
    pub const UNKNOWN: Self = Self {
        unix_seconds: None,
        source: BuildStampSource::Unknown,
    };

    /// Valore per il wire: secondi Unix come stringa, `"0"` se ignoto.
    ///
    /// La forma stringa e' il contratto gia' consumato dagli script di deploy
    /// (`grep -o '"build_time":"[^"]*"'` in `scripts/deploy-nexus.sh` e
    /// `scripts/dev-server-101.sh`, che poi confrontano numericamente con
    /// `.last_build_ts`): `"0"` li fa fallire in modo visibile invece di
    /// spacciare per buona una misura che non c'e'.
    pub fn wire_value(&self) -> String {
        self.unix_seconds
            .map_or_else(|| "0".to_string(), |s| s.to_string())
    }
}

/// Secondi Unix dell'ultima modifica di `path`, `None` se il file non e'
/// interrogabile. E' il fatto sul disco, non una deduzione.
pub fn mtime_unix_seconds(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Punto unico: stamp del binario in esecuzione, letto dal proprio eseguibile e
/// memoizzato alla prima chiamata (vedi nota di modulo sul perche' non si
/// rilegge).
pub fn running_binary() -> BuildStamp {
    static STAMP: OnceLock<BuildStamp> = OnceLock::new();
    *STAMP.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .as_deref()
            .and_then(mtime_unix_seconds)
            .map_or(BuildStamp::UNKNOWN, |unix_seconds| BuildStamp {
                unix_seconds: Some(unix_seconds),
                source: BuildStampSource::ExeMtime,
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("orologio di sistema dopo il 1970")
            .as_secs()
    }

    fn file_di_prova(nome: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "nexus_build_info_{}_{nome}",
            std::process::id() // niente stato condiviso fra run concorrenti (regola F)
        ));
        std::fs::write(&path, b"artefatto di prova").expect("scrittura del file di prova");
        path
    }

    /// Il valore segue la SCRITTURA del file, non il momento della
    /// compilazione: e' la proprieta' che il timestamp inciso da `build.rs` non
    /// aveva (restava fermo alla data di quel file).
    #[test]
    fn il_timestamp_segue_il_file_non_la_compilazione() {
        let prima = now_unix();
        let path = file_di_prova("scritto_ora");
        let letto = mtime_unix_seconds(&path).expect("un file appena scritto ha un mtime");
        let _ = std::fs::remove_file(&path);

        let finestra = prima.saturating_sub(5)..=prima + 5;
        assert!(
            finestra.contains(&letto),
            "mtime {letto} fuori dalla finestra della scrittura ({prima} +/- 5s): \
             il valore non viene dal file"
        );
    }

    /// Un file che non c'e' non produce un numero plausibile: produce `None`, e
    /// sul wire un `"0"` che il chiamante riconosce come "non misurato".
    #[test]
    fn un_percorso_inesistente_non_ha_timestamp() {
        let path = std::env::temp_dir().join(format!(
            "nexus_build_info_{}_mai_creato",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        assert_eq!(mtime_unix_seconds(&path), None);
        assert_eq!(BuildStamp::UNKNOWN.wire_value(), "0");
        assert_eq!(BuildStamp::UNKNOWN.source, BuildStampSource::Unknown);
    }

    /// Test di mutazione del difetto originale: se lo stamp tornasse a nascere
    /// da una costante di compilazione (`env!("BUILD_TIMESTAMP")`), non
    /// coinciderebbe piu' con l'mtime dell'eseguibile che lo sta eseguendo —
    /// che e' esattamente la divergenza misurata su `/health` il 27/07/2026.
    #[test]
    fn lo_stamp_del_processo_e_quello_del_suo_eseguibile() {
        let exe = std::env::current_exe().expect("path dell'eseguibile di test");
        let atteso = mtime_unix_seconds(&exe).expect("mtime dell'eseguibile di test");

        let stamp = running_binary();

        assert_eq!(stamp.source, BuildStampSource::ExeMtime);
        assert_eq!(stamp.unix_seconds, Some(atteso));
        assert_eq!(stamp.wire_value(), atteso.to_string());
    }

    /// La memoizzazione non deve introdurre una seconda risposta alla stessa
    /// domanda: chiamate successive danno lo stesso stamp.
    #[test]
    fn lo_stamp_e_stabile_fra_chiamate() {
        assert_eq!(running_binary(), running_binary());
    }
}
