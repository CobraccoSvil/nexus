//! Perche' l'applicazione di un set e' fallita, in forma tipizzata.
//!
//! Il chiamante decide sulla VARIANTE, mai sul testo (regola M). Il testo serve
//! a chi legge, e porta la cura oltre al sintomo: un errore che dice cosa e'
//! successo senza dire cosa fare costringe chi lo incontra a indovinare.

use std::path::PathBuf;

use crate::{Provenienza, Set};

#[derive(Debug, thiserror::Error)]
pub enum ErroreMigrazione {
    /// I file del set non sono raggiungibili dalla radice dichiarata.
    #[error(
        "set '{set}' non raggiungibile in {percorso_tentato:?} (radice {radice:?}, \
         {provenienza:?}). Il processo non ha lo schema che dichiara di avere: \
         eseguire da un albero che contiene il set, oppure indicare la radice \
         esplicitamente. Causa: {fonte}"
    )]
    SetNonRaggiungibile {
        set: Set,
        percorso_tentato: PathBuf,
        radice: PathBuf,
        provenienza: Provenienza,
        #[source]
        fonte: sqlx::migrate::MigrateError,
    },

    /// Il file di una migrazione gia' applicata e' cambiato dopo l'applicazione.
    #[error(
        "la migrazione {versione} del set '{set}' e' cambiata dopo essere stata \
         applicata{nota}. Una migrazione applicata e' immutabile: se serve \
         correggerla, si aggiunge una migrazione nuova."
    )]
    ChecksumDivergente {
        set: Set,
        versione: i64,
        /// Diagnosi mirata: il caso quasi sempre vero e' il fine-riga, e i
        /// versi in cui puo' presentarsi sono due.
        nota: String,
    },

    /// Lo schema c'e' gia' ma il registro delle migrazioni non lo sa: e' il DB
    /// nato da un percorso che ha scritto le tabelle senza passare dal migrator
    /// (il mount `initdb` di un compose, un restore parziale, un psql a mano).
    #[error(
        "la migrazione {versione} del set '{set}' ha trovato un oggetto che \
         esiste gia' (SQLSTATE {sqlstate}): il database ha lo schema ma non il \
         registro `_sqlx_migrations`. Un DB nato cosi' non e' adottabile in \
         automatico: fare un dump dei dati, ricreare il database vuoto, \
         applicare le migrazioni e ricaricare i dati (su Windows: \
         deploy/db-restore.ps1 -Recreate)."
    )]
    SchemaPreesistenteNonRegistrato {
        set: Set,
        versione: i64,
        sqlstate: String,
    },

    /// Errore di esecuzione non riconducibile ai casi sopra.
    #[error("applicazione del set '{set}' fallita: {fonte}")]
    Esecuzione {
        set: Set,
        #[source]
        fonte: sqlx::migrate::MigrateError,
    },

    /// Nessuno ha installato l'origine del set in questo processo.
    #[error(
        "origine dei set di migrazione non installata in questo processo: \
         chiamare `installa_origine` all'avvio. Non esiste un default: una \
         radice indovinata applicherebbe le migrazioni di un albero che il \
         chiamante non ha scelto."
    )]
    OrigineNonInstallata,

    /// Due origini diverse nello stesso processo.
    #[error(
        "origine gia' installata su {presente:?}, tentata {tentata:?}: due \
         radici nello stesso processo sarebbero due risposte alla stessa domanda"
    )]
    OrigineGiaInstallata {
        presente: PathBuf,
        tentata: PathBuf,
    },
}

impl ErroreMigrazione {
    /// Codice macchina per i chiamanti che devono decidere (regola M): mai il
    /// testo, che cambia con la lingua e con la revisione.
    pub fn codice(&self) -> &'static str {
        match self {
            Self::SetNonRaggiungibile { .. } => "set_non_raggiungibile",
            Self::ChecksumDivergente { .. } => "checksum_divergente",
            Self::SchemaPreesistenteNonRegistrato { .. } => "schema_preesistente_non_registrato",
            Self::Esecuzione { .. } => "esecuzione",
            Self::OrigineNonInstallata => "origine_non_installata",
            Self::OrigineGiaInstallata { .. } => "origine_gia_installata",
        }
    }
}

/// SQLSTATE che indicano "l'oggetto esiste gia'": e' la firma di uno schema
/// creato fuori dal migrator. Sono codici standard, non messaggi.
const SQLSTATE_OGGETTO_DUPLICATO: [&str; 4] = [
    "42P07", // duplicate_table
    "42P06", // duplicate_schema
    "42701", // duplicate_column
    "42710", // duplicate_object
];

/// Traduce l'errore di sqlx nella nostra diagnosi.
///
/// La distinzione fra "schema preesistente" e un errore qualunque si fa sullo
/// SQLSTATE della migrazione che ha fallito, non ispezionando `public` prima di
/// cominciare: quattro compose del repo creano tabelle su `public` che NON
/// collidono col set, e un controllo preventivo le avrebbe dichiarate non
/// migrabili impedendo l'avvio a installazioni sane.
pub(crate) fn da_migrate_error(
    set: Set,
    origine: &crate::OrigineSet,
    fonte: sqlx::migrate::MigrateError,
) -> ErroreMigrazione {
    match &fonte {
        sqlx::migrate::MigrateError::VersionMismatch(v) => {
            let versione = *v;
            ErroreMigrazione::ChecksumDivergente {
                set,
                versione,
                nota: nota(set, origine, versione),
            }
        }
        sqlx::migrate::MigrateError::ExecuteMigration(sqlx::Error::Database(e), v) => {
            match e.code() {
                Some(code) if SQLSTATE_OGGETTO_DUPLICATO.contains(&code.as_ref()) => {
                    ErroreMigrazione::SchemaPreesistenteNonRegistrato {
                        set,
                        versione: *v,
                        sqlstate: code.to_string(),
                    }
                }
                _ => ErroreMigrazione::Esecuzione { set, fonte },
            }
        }
        _ => ErroreMigrazione::Esecuzione { set, fonte },
    }
}

/// Il fine-riga e' la causa quasi sempre vera di un checksum divergente in
/// questo repo, e si presenta in DUE versi opposti che vogliono cure opposte.
/// La nota li distingue, perche' indicare la cura sbagliata costa piu' del non
/// dire nulla.
///
/// Verso A — il file sul disco ha CRLF: il checkout ha ignorato
/// `.gitattributes`. Si ricrea il file; il registro ha ragione.
///
/// Verso B — il file sul disco e' canonico: allora e' il REGISTRO a poter
/// conservare l'hash di byte CRLF, perche' il database e' stato migrato da un
/// checkout non conforme. E' il verso realmente accaduto (05/08/2026, migrazioni
/// 117 e 118), ed e' quello che la diagnosi non copriva: guardava solo il file,
/// lo trovava a posto e taceva, lasciando "la migrazione e' cambiata" come unica
/// spiegazione di un albero che nessuno aveva toccato.
///
/// Qui non si CLASSIFICA la divergenza: per farlo servirebbe il checksum
/// registrato, che a questo punto della catena non c'e' (`VersionMismatch`
/// porta solo la versione). La nota rimanda percio' allo strumento che quel
/// confronto lo fa davvero — `--check`, che passa da
/// [`crate::registro::classifica`] — invece di affermare una causa che non ha
/// misurato.
fn nota(set: Set, origine: &crate::OrigineSet, versione: i64) -> String {
    let dir = origine.percorso(set);
    let Ok(voci) = std::fs::read_dir(&dir) else {
        return String::new();
    };
    let prefisso = format!("{versione:04}");
    for v in voci.flatten() {
        let nome = v.file_name();
        let nome = nome.to_string_lossy();
        if !nome.starts_with(&prefisso) || !nome.ends_with(".sql") {
            continue;
        }
        let Ok(contenuto) = std::fs::read(v.path()) else {
            break;
        };
        if contenuto.windows(2).any(|w| w == b"\r\n") {
            return format!(
                " (il file {nome} sul disco ha fine-riga CRLF: il checkout ha \
                 ignorato .gitattributes, che per *.sql dichiara eol=lf — e' \
                 questo a cambiare il checksum, non il contenuto. Rimedio: \
                 rm '{nome}' && git checkout -- '{nome}')"
            );
        }
        return format!(
            " (il file {nome} sul disco e' canonico: se il database e' stato \
             migrato da un checkout che lo materializzava CRLF, e' il registro a \
             conservare l'hash di quei byte. Per saperlo: cargo run -p xtask -- \
             migrate --set {set} --check, che distingue il fine-riga da un \
             contenuto davvero cambiato)"
        );
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ogni_variante_ha_un_codice_macchina_stabile() {
        let e = ErroreMigrazione::OrigineNonInstallata;
        assert_eq!(e.codice(), "origine_non_installata");
        let e = ErroreMigrazione::SchemaPreesistenteNonRegistrato {
            set: Set::Meta,
            versione: 42,
            sqlstate: "42P07".into(),
        };
        assert_eq!(e.codice(), "schema_preesistente_non_registrato");
        // Il testo porta la CURA, non solo il sintomo.
        let t = e.to_string();
        assert!(t.contains("dump"), "l'errore deve dire cosa fare: {t}");
    }

    /// I due versi vogliono due cure, e una nota che le confonde manda a
    /// riparare la cosa sbagliata.
    #[test]
    fn la_nota_distingue_i_due_versi_del_fine_riga() {
        let dir = tempfile::tempdir().expect("tempdir");
        let set_dir = dir.path().join("db").join("migrations");
        std::fs::create_dir_all(&set_dir).expect("mkdir");
        let origine = crate::OrigineSet::esplicita(dir.path());
        let file = set_dir.join("0117_prova.sql");

        std::fs::write(&file, b"SELECT 1;\r\nSELECT 2;\r\n").expect("write");
        let crlf = nota(Set::Meta, &origine, 117);
        assert!(crlf.contains("CRLF"), "{crlf}");
        assert!(
            crlf.contains("git checkout --"),
            "il verso col file sporco manda a ricreare il file: {crlf}"
        );

        std::fs::write(&file, b"SELECT 1;\nSELECT 2;\n").expect("write");
        let lf = nota(Set::Meta, &origine, 117);
        assert!(
            lf.contains("--check"),
            "col file canonico la nota deve mandare allo strumento che classifica, \
             non affermare una causa non misurata: {lf}"
        );
        assert!(
            !lf.contains("git checkout --"),
            "qui ricreare il file non cambierebbe nulla: {lf}"
        );
    }

    #[test]
    fn gli_sqlstate_di_oggetto_duplicato_sono_codici_non_frasi() {
        for c in SQLSTATE_OGGETTO_DUPLICATO {
            assert_eq!(c.len(), 5, "uno SQLSTATE ha cinque caratteri: {c}");
            assert!(c.chars().all(|ch| ch.is_ascii_alphanumeric()));
        }
    }
}
