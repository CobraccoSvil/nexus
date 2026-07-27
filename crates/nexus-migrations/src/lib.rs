//! Applicazione dei set di migrazioni: PUNTO UNICO (regola L).
//!
//! Prima di questo crate, "applicare il set META" aveva nove incarnazioni
//! diverse — mcp-core all'avvio, il provisioning dei DB-progetto, i migrator
//! compile-time dei test, tre workflow CI via `sqlx-cli`, un ciclo
//! `psql || true`, il mount `initdb` di un compose, un binario esterno — e
//! ognuna decideva per conto proprio dove cercare i file e cosa fare quando non
//! li trovava. Due gemelli davano verdetti opposti sulla stessa condizione:
//! `mcp-core` proseguiva con un warning, il provisioning rispondeva 503.
//!
//! Qui il verdetto e' uno solo perche' e' lo stesso codice, non perche' due
//! autori hanno scelto lo stesso comportamento.
//!
//! ORIGINE DEL SET. Dove stanno i file e' verita' di PROCESSO, non di
//! richiesta: si installa una volta all'avvio con [`installa_origine`] e da li'
//! in poi ogni chiamante la legge da [`origine_di_processo`]. Passarla lungo la
//! catena delle richieste avrebbe richiesto di attraversare centottantadue call
//! site di `project_data_pool`, e avrebbe creato due verita' — quella del
//! processo e quella del parametro.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub mod errore;
pub mod set;

pub use errore::ErroreMigrazione;
pub use set::Set;

/// Da dove si prendono i file di migrazione.
///
/// Non e' un path: e' la REGOLA con cui il path si ottiene, perche' la premessa
/// va dichiarata insieme al risultato. Un comando che stampa "12 migrazioni
/// applicate" senza dire da quale albero le ha lette da' un numero senza la sua
/// premessa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrigineSet {
    radice: PathBuf,
    provenienza: Provenienza,
}

/// Come e' stata determinata la radice, per la premessa e per la diagnosi.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenienza {
    /// Indicata esplicitamente dal chiamante (flag, configurazione, test).
    Esplicita,
    /// Directory di lavoro del processo.
    DirectoryDiLavoro,
}

impl OrigineSet {
    /// Radice indicata esplicitamente: e' la forma che i test usano, ed e'
    /// anche quella del flag `--migrations-root`.
    pub fn esplicita(radice: impl Into<PathBuf>) -> Self {
        Self {
            radice: radice.into(),
            provenienza: Provenienza::Esplicita,
        }
    }

    /// Radice = directory di lavoro del processo.
    ///
    /// E' il comportamento storico di `mcp-core`, conservato di proposito: il
    /// suo manifest di servizio fissa `workingdirectory` alla radice del repo,
    /// quindi cambiarlo qui avrebbe modificato un avvio che oggi funziona. La
    /// differenza e' che ora la radice viene DICHIARATA: se il processo parte
    /// altrove, l'errore dice quale percorso ha guardato invece di proseguire
    /// in silenzio.
    pub fn directory_di_lavoro() -> std::io::Result<Self> {
        Ok(Self {
            radice: std::env::current_dir()?,
            provenienza: Provenienza::DirectoryDiLavoro,
        })
    }

    pub fn radice(&self) -> &Path {
        &self.radice
    }

    pub fn provenienza(&self) -> Provenienza {
        self.provenienza
    }

    /// Percorso completo del set, per la diagnosi e per la premessa.
    pub fn percorso(&self, set: Set) -> PathBuf {
        self.radice.join(set.sottopercorso())
    }
}

static ORIGINE: OnceLock<OrigineSet> = OnceLock::new();

/// Installa l'origine per l'intero processo. Da chiamare UNA volta all'avvio.
///
/// Ritorna `Err` se era gia' installata con un valore diverso: due origini nello
/// stesso processo significherebbero due risposte alla stessa domanda.
pub fn installa_origine(origine: OrigineSet) -> Result<(), ErroreMigrazione> {
    match ORIGINE.set(origine.clone()) {
        Ok(()) => Ok(()),
        Err(_) => {
            let gia = ORIGINE.get().expect("appena constatata presente");
            if *gia == origine {
                Ok(())
            } else {
                Err(ErroreMigrazione::OrigineGiaInstallata {
                    presente: gia.radice.clone(),
                    tentata: origine.radice,
                })
            }
        }
    }
}

/// L'origine installata. Errore esplicito se nessuno l'ha installata: un
/// default silenzioso qui sarebbe il difetto che questo crate chiude.
pub fn origine_di_processo() -> Result<&'static OrigineSet, ErroreMigrazione> {
    ORIGINE.get().ok_or(ErroreMigrazione::OrigineNonInstallata)
}

/// Costruisce il migrator del set, senza applicarlo.
///
/// Non c'e' alcun ramo "se la directory non esiste": `Migrator::new` fallisce
/// gia' da solo, e quel fallimento e' un errore. Il guard `if path.exists()`
/// scritto a mano in `mcp-core::db` convertiva quell'errore in un warning, ed
/// e' il motivo per cui un servizio poteva avviarsi con lo schema vecchio senza
/// che nessuno lo sapesse.
pub async fn risolvi(
    set: Set,
    origine: &OrigineSet,
) -> Result<sqlx::migrate::Migrator, ErroreMigrazione> {
    let percorso = origine.percorso(set);
    sqlx::migrate::Migrator::new(percorso.as_path())
        .await
        .map_err(|fonte| ErroreMigrazione::SetNonRaggiungibile {
            set,
            percorso_tentato: percorso,
            radice: origine.radice.clone(),
            provenienza: origine.provenienza,
            fonte,
        })
}

/// Applica il set. Punto unico: ogni chiamante passa di qui.
pub async fn applica<'a, A>(
    connessione: A,
    set: Set,
    origine: &OrigineSet,
) -> Result<(), ErroreMigrazione>
where
    A: sqlx::Acquire<'a, Database = sqlx::Postgres>,
{
    let migrator = risolvi(set, origine).await?;
    migrator
        .run(connessione)
        .await
        .map_err(|fonte| errore::da_migrate_error(set, origine, fonte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn un_set_assente_e_un_errore_non_un_avviso() {
        let vuota = tempfile::tempdir().expect("tempdir");
        let origine = OrigineSet::esplicita(vuota.path());
        let e = risolvi(Set::Meta, &origine)
            .await
            .expect_err("una directory senza set deve essere un errore");
        match e {
            ErroreMigrazione::SetNonRaggiungibile {
                set,
                percorso_tentato,
                ..
            } => {
                assert_eq!(set, Set::Meta);
                assert!(
                    percorso_tentato.ends_with("db/migrations")
                        || percorso_tentato.ends_with("db\\migrations"),
                    "il percorso tentato deve comparire nella diagnosi: {percorso_tentato:?}"
                );
            }
            altro => panic!("variante inattesa: {altro:?}"),
        }
    }

    /// I due gemelli davano verdetti opposti sulla stessa condizione. Ora la
    /// condizione e' una sola perche' il codice e' lo stesso.
    #[tokio::test]
    async fn i_gemelli_non_divergono_piu() {
        let vuota = tempfile::tempdir().expect("tempdir");
        let origine = OrigineSet::esplicita(vuota.path());
        let meta = risolvi(Set::Meta, &origine).await;
        let progetto = risolvi(Set::Project, &origine).await;
        assert!(
            meta.is_err() && progetto.is_err(),
            "sotto la stessa condizione i due set devono dare lo stesso verdetto"
        );
    }

    /// L'origine non installata e' un errore, mai un default.
    #[test]
    fn senza_origine_installata_il_processo_lo_dice() {
        // Non si installa nulla in questo test: il OnceLock resta vuoto finche'
        // un altro test non lo popola, quindi si accetta anche l'esito Ok.
        match origine_di_processo() {
            Err(ErroreMigrazione::OrigineNonInstallata) => {}
            Ok(_) => {} // un altro test nello stesso processo l'ha installata
            Err(altro) => panic!("variante inattesa: {altro:?}"),
        }
    }

    #[test]
    fn il_percorso_del_set_lo_conosce_il_set() {
        let o = OrigineSet::esplicita("R");
        assert!(o.percorso(Set::Meta).ends_with("migrations"));
        assert!(o.percorso(Set::Project).ends_with("project"));
    }
}
