//! Lo stato del REGISTRO rispetto ai file del set, e la sua riparazione.
//!
//! Il registro `_sqlx_migrations` conserva, per ogni migrazione applicata, lo
//! SHA-384 dei BYTE del file al momento in cui e' stata applicata. Il migrator
//! ricalcola quell'hash a ogni avvio dai byte che trova sul disco e rifiuta di
//! proseguire se non coincide: e' la garanzia che una migrazione applicata sia
//! immutabile, e va difesa.
//!
//! Ma i byte di un file di testo dipendono anche da COME il checkout li ha
//! materializzati. Con `core.autocrlf=true` un file il cui blob e' LF puo'
//! comparire su disco con CRLF: stesso contenuto, dieci byte in piu' ogni dieci
//! righe, hash diverso. `git status` non lo mostra — normalizza nel confronto —
//! quindi il difetto e' invisibile a chi guarda il repo e visibile solo a chi
//! legge i byte. Ed e' asimmetrico: il DB migrato da quel checkout registra
//! l'hash CRLF, e da quel momento ogni albero CONFORME viene rifiutato.
//!
//! MISURATO il 05/08/2026 su `D:\IDEAI`: 2 file su 695 con l'attributo
//! `text eol=lf` erano materializzati CRLF (le migrazioni 117 e 118), e i due
//! checksum nel registro erano esattamente lo SHA-384 dei loro byte CRLF. Ogni
//! worktree creato dopo l'aggiunta di `.gitattributes` era percio' respinto, e
//! con esso l'avvio di mcp-core. Era la seconda occorrenza: la prima
//! (2026-07-02, migrazione 0500) e' quella che `.gitattributes` cita.
//!
//! PERCHE' LA RIPARAZIONE NON PUO' ESSERE UNA MIGRAZIONE. La regola H vuole che
//! un dato da correggere viaggi in una migrazione versionata, mai in un `psql`
//! a mano. Qui non e' possibile, e non per comodita': il migrator valida TUTTI
//! i checksum registrati prima di applicare qualunque cosa, quindi una
//! migrazione che riparasse il registro non verrebbe mai eseguita. Il rimedio
//! deve stare fuori dal migrator, ed e' questo modulo — che percio' non e' un
//! `UPDATE` travestito da comando: scrive solo dove ha una PROVA, e la prova e'
//! costruttiva (vedi [`classifica`]).

use sqlx::migrate::{AppliedMigration, Migrate, MigrateError};
use sqlx::{Acquire, Postgres};

use crate::{ErroreMigrazione, OrigineSet, Set};

/// L'unica scrittura di questo crate, come LETTERALE.
///
/// Il nome della tabella lo fissa sqlx e non e' configurabile, quindi non c'e'
/// niente da comporre: un `format!` con una costante avrebbe prodotto la stessa
/// stringa, ma e' la forma che il detector di SQL-injection riconosce (ADR
/// 0021), e vale piu' una query che non somiglia a una query costruita che una
/// costante riusata una volta sola.
const SQL_RIALLINEA_CHECKSUM: &str =
    "UPDATE _sqlx_migrations SET checksum = $1 WHERE version = $2";

/// Perche' il checksum registrato non e' quello dei byte sul disco.
///
/// Enum chiuso e non un `bool` "riparabile" (regola Q): le tre cause vogliono
/// tre azioni diverse, e collassarle in un si'/no costringerebbe chi legge a
/// indovinare quale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CausaDivergenza {
    /// Il file sul disco e' canonico e il registro porta l'hash degli STESSI
    /// byte con l'altra convenzione di fine-riga. E' l'unico caso riparabile, e
    /// la prova e' costruttiva: la variante e' stata generata e il suo hash
    /// coincide con quello registrato.
    FineRigaNelRegistro,
    /// Il file SUL DISCO ha CRLF. Il registro puo' anche avere ragione: qui si
    /// normalizza il file, non si tocca il registro. Riparare in questo verso
    /// fisserebbe l'hash di byte che nessun checkout conforme riprodurra' mai,
    /// cioe' renderebbe permanente il difetto invece di chiuderlo.
    FineRigaSulDisco,
    /// Nessuna variante di soli fine-riga produce il checksum registrato: il
    /// contenuto e' un altro. Una migrazione applicata e' immutabile — qui non
    /// si ripara nulla, si aggiunge una migrazione nuova.
    ContenutoDiverso,
}

impl CausaDivergenza {
    /// Solo una delle tre cause autorizza a scrivere sul registro.
    pub const fn riparabile(self) -> bool {
        matches!(self, Self::FineRigaNelRegistro)
    }

    /// Cosa fare, per chi legge la diagnosi.
    pub const fn cura(self) -> &'static str {
        match self {
            Self::FineRigaNelRegistro => {
                "riparabile: xtask migrate --set <set> --repair-checksums"
            }
            // Il comando RICREA il file. `git checkout-index -f` sembrerebbe
            // piu' pulito e non funziona: esce 0 senza scrivere quando l'indice
            // considera il file aggiornato, cioe' sempre in questo caso — per
            // git quel file non e' modificato. Misurato sul repo reale: mtime
            // identico dopo l'esecuzione.
            Self::FineRigaSulDisco => {
                "ricreare il file nel working tree (rm <file> && git checkout -- <file>), \
                 non toccare il registro"
            }
            Self::ContenutoDiverso => {
                "una migrazione applicata e' immutabile: aggiungere una migrazione nuova"
            }
        }
    }
}

/// Lo stato di UNA versione, guardando insieme il set e il registro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerdettoVersione {
    /// Applicata, e i byte sul disco sono quelli registrati.
    Allineata,
    /// Nel set ma non nel registro: e' cio' che `--apply` applicherebbe.
    Pendente,
    /// Applicata con un checksum diverso da quello dei byte sul disco.
    Divergente(CausaDivergenza),
    /// Applicata, ma il set non ha il file. L'albero non contiene lo schema che
    /// il database dichiara di avere: succede a un worktree indietro rispetto
    /// al branch da cui il DB e' stato migrato. Anche questo blocca `--apply`,
    /// con un errore diverso, e per questo il censimento lo distingue.
    ApplicataSenzaFile,
}

/// Una versione e il suo verdetto, coi due hash che l'hanno prodotto.
#[derive(Debug, Clone)]
pub struct Voce {
    pub versione: i64,
    pub verdetto: VerdettoVersione,
    /// SHA-384 dei byte sul disco, quando il file c'e'. Non lo calcola questo
    /// modulo: e' quello che sqlx ha gia' prodotto leggendo il file, cioe'
    /// esattamente il valore che il migrator confrontera' (regola O).
    pub checksum_sul_disco: Option<Vec<u8>>,
    /// Quello nel registro, quando la versione risulta applicata.
    pub checksum_registrato: Option<Vec<u8>>,
}

/// Il set e il registro messi a confronto, versione per versione.
#[derive(Debug, Clone)]
pub struct Censimento {
    pub set: Set,
    pub voci: Vec<Voce>,
}

impl Censimento {
    /// Cio' che `--apply` applicherebbe: nel set, non nel registro.
    pub fn pendenti(&self) -> Vec<i64> {
        self.versioni_con(|v| matches!(v.verdetto, VerdettoVersione::Pendente))
    }

    /// Applicate di cui questo albero non ha il file. Non e' un difetto del
    /// database: e' l'albero a non avere lo schema che il database dichiara.
    pub fn senza_file(&self) -> Vec<i64> {
        self.versioni_con(|v| matches!(v.verdetto, VerdettoVersione::ApplicataSenzaFile))
    }

    /// Tutte le divergenze, riparabili o no: e' l'elenco che la diagnosi mostra.
    pub fn divergenti(&self) -> Vec<&Voce> {
        self.voci_con(|v| matches!(v.verdetto, VerdettoVersione::Divergente(_)))
    }

    /// Le versioni che questo modulo e' autorizzato a riscrivere.
    pub fn riparabili(&self) -> Vec<&Voce> {
        self.voci_con(|v| match v.verdetto {
            VerdettoVersione::Divergente(c) => c.riparabile(),
            _ => false,
        })
    }

    /// Cio' che impedisce a `--apply` di partire e che la riparazione NON
    /// risolve. Serve a non promettere, dopo aver riparato, che ora si applichi.
    pub fn bloccanti_non_riparabili(&self) -> Vec<&Voce> {
        self.voci_con(|v| match v.verdetto {
            VerdettoVersione::Divergente(c) => !c.riparabile(),
            VerdettoVersione::ApplicataSenzaFile => true,
            _ => false,
        })
    }

    /// Il selettore, in un punto solo. Le cinque interrogazioni sopra
    /// differiscono per il CRITERIO, non per come si scorre: scritte per esteso
    /// erano cinque copie della stessa riga, e il detector di blocchi duplicati
    /// le contava come tali (misurato: 3 finding su questo file).
    fn voci_con(&self, criterio: impl Fn(&Voce) -> bool) -> Vec<&Voce> {
        self.voci.iter().filter(|v| criterio(v)).collect()
    }

    fn versioni_con(&self, criterio: impl Fn(&Voce) -> bool) -> Vec<i64> {
        self.voci_con(criterio)
            .into_iter()
            .map(|v| v.versione)
            .collect()
    }
}

/// Il verdetto sul confronto fra i byte di un file e un checksum registrato.
///
/// PARTE PURA: niente database, niente filesystem. La domanda e' "esiste una
/// variante di soli fine-riga di questi byte il cui hash e' quello registrato?",
/// e la si risponde COSTRUENDO le due varianti e confrontandone l'hash — non
/// riconoscendo una firma nel testo dell'errore ne' fidandosi di un'euristica
/// sul numero di byte di differenza.
pub fn classifica(byte_sul_disco: &[u8], checksum_registrato: &[u8]) -> Option<CausaDivergenza> {
    if sha384(byte_sul_disco) == checksum_registrato {
        return None;
    }
    let disco_ha_crlf = byte_sul_disco.windows(2).any(|w| w == b"\r\n");
    // Un file gia' canonico: la variante da provare e' quella CRLF, cioe' il
    // registro scritto da un checkout non conforme.
    // Un file CRLF: la variante da provare e' quella LF, cioe' il registro
    // scritto da un checkout conforme. In quel verso non si ripara comunque, ma
    // dirlo con la causa giusta manda alla cura giusta.
    let variante = if disco_ha_crlf {
        a_lf(byte_sul_disco)
    } else {
        a_crlf(byte_sul_disco)
    };
    if sha384(&variante) != checksum_registrato {
        return Some(CausaDivergenza::ContenutoDiverso);
    }
    Some(if disco_ha_crlf {
        CausaDivergenza::FineRigaSulDisco
    } else {
        CausaDivergenza::FineRigaNelRegistro
    })
}

/// Censisce il set contro il registro del database.
///
/// Non applica e non scrive: e' la lettura da cui dipendono sia `--check` (che
/// senza di essa direbbe "nessuna migrazione pendente" mentre `--apply`
/// fallirebbe) sia la riparazione.
pub async fn censisci<'a, A>(
    connessione: A,
    set: Set,
    origine: &OrigineSet,
) -> Result<Censimento, ErroreMigrazione>
where
    A: Acquire<'a, Database = Postgres>,
{
    let migrator = crate::risolvi(set, origine).await?;
    let mut conn = connessione
        .acquire()
        .await
        .map_err(|e| esecuzione(set, MigrateError::Execute(e)))?;

    // Idempotente: su un database vergine crea il registro vuoto invece di far
    // fallire la lettura con "relation does not exist", che sarebbe un errore
    // travestito da assenza.
    conn.ensure_migrations_table()
        .await
        .map_err(|e| esecuzione(set, e))?;
    let applicate = conn
        .list_applied_migrations()
        .await
        .map_err(|e| esecuzione(set, e))?;

    let mut voci = voci_del_set(&migrator, &applicate);
    voci.extend(voci_applicate_senza_file(&migrator, &applicate));
    voci.sort_by_key(|v| v.versione);
    Ok(Censimento { set, voci })
}

/// Il verdetto per ogni versione che il set contiene.
fn voci_del_set(migrator: &sqlx::migrate::Migrator, applicate: &[AppliedMigration]) -> Vec<Voce> {
    migrator
        .iter()
        .map(|m| {
            let registrata = applicate.iter().find(|a| a.version == m.version);
            let verdetto = match registrata {
                None => VerdettoVersione::Pendente,
                Some(a) => match classifica(m.sql.as_bytes(), &a.checksum) {
                    None => VerdettoVersione::Allineata,
                    Some(causa) => VerdettoVersione::Divergente(causa),
                },
            };
            Voce {
                versione: m.version,
                verdetto,
                checksum_sul_disco: Some(m.checksum.to_vec()),
                checksum_registrato: registrata.map(|a| a.checksum.to_vec()),
            }
        })
        .collect()
}

/// Le applicate che il set NON contiene. Senza questo ramo un albero indietro
/// supererebbe il censimento e fallirebbe soltanto ad `--apply`, con un errore
/// che non nomina la causa.
fn voci_applicate_senza_file(
    migrator: &sqlx::migrate::Migrator,
    applicate: &[AppliedMigration],
) -> Vec<Voce> {
    let nel_set: Vec<i64> = migrator.iter().map(|m| m.version).collect();
    applicate
        .iter()
        .filter(|a| !nel_set.contains(&a.version))
        .map(|a| Voce {
            versione: a.version,
            verdetto: VerdettoVersione::ApplicataSenzaFile,
            checksum_sul_disco: None,
            checksum_registrato: Some(a.checksum.to_vec()),
        })
        .collect()
}

/// Riscrive nel registro il checksum delle sole versioni la cui divergenza e'
/// PROVATA essere di soli fine-riga, col file sul disco gia' canonico.
///
/// Ritorna le versioni riscritte. Non tocca nient'altro: le altre divergenze
/// restano, ed e' voluto — un comando che "sistema" anche cio' che non ha
/// capito e' il modo in cui una migrazione modificata a mano passa inosservata.
pub async fn ripara_fine_riga<'a, A>(
    connessione: A,
    censimento: &Censimento,
) -> Result<Vec<i64>, ErroreMigrazione>
where
    A: Acquire<'a, Database = Postgres>,
{
    let set = censimento.set;
    let da_riparare = censimento.riparabili();
    if da_riparare.is_empty() {
        return Ok(Vec::new());
    }
    let mut conn = connessione
        .acquire()
        .await
        .map_err(|e| esecuzione(set, MigrateError::Execute(e)))?;

    let mut riscritte = Vec::new();
    for voce in da_riparare {
        let Some(nuovo) = voce.checksum_sul_disco.as_ref() else {
            // Irraggiungibile per costruzione (una voce riparabile ha il file),
            // ma saltarla e' l'unica cosa sensata: mai una scrittura al buio.
            continue;
        };
        sqlx::query(SQL_RIALLINEA_CHECKSUM)
            .bind(nuovo.as_slice())
            .bind(voce.versione)
            .execute(&mut *conn)
            .await
            .map_err(|e| esecuzione(set, MigrateError::Execute(e)))?;
        riscritte.push(voce.versione);
    }
    Ok(riscritte)
}

fn esecuzione(set: Set, fonte: MigrateError) -> ErroreMigrazione {
    ErroreMigrazione::Esecuzione { set, fonte }
}

/// SHA-384, lo stesso che sqlx usa per il checksum di una migrazione.
///
/// Che sia "lo stesso" non e' un'assunzione: e' verificato dal test
/// `il_nostro_sha384_e_quello_che_sqlx_registra`, che confronta questa funzione
/// col checksum prodotto da `Migrator::new` su un file vero. Se sqlx cambiasse
/// algoritmo, quel test rosseggia prima che questo modulo scriva un hash che il
/// migrator non riconoscerebbe.
fn sha384(byte: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha384};
    Sha384::digest(byte).to_vec()
}

/// La stessa sequenza con i fine-riga dell'altra convenzione. Passa per la
/// forma LF anche in andata, cosi' un file gia' misto non produce `\r\r\n`.
fn a_crlf(byte: &[u8]) -> Vec<u8> {
    let lf = a_lf(byte);
    let mut out = Vec::with_capacity(lf.len());
    for b in lf {
        if b == b'\n' {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
}

fn a_lf(byte: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(byte.len());
    let mut i = 0;
    while i < byte.len() {
        if byte[i] == b'\r' && byte.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(byte[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQL: &[u8] = b"-- una migrazione\nINSERT INTO t VALUES (1);\nSELECT 1;\n";

    fn crlf(b: &[u8]) -> Vec<u8> {
        a_crlf(b)
    }

    #[test]
    fn i_byte_registrati_sono_gli_stessi_a_fine_riga_diversi_e_lo_si_prova() {
        // Il caso misurato: disco canonico, registro scritto da un checkout CRLF.
        let registrato = sha384(&crlf(SQL));
        assert_eq!(
            classifica(SQL, &registrato),
            Some(CausaDivergenza::FineRigaNelRegistro)
        );
        assert!(CausaDivergenza::FineRigaNelRegistro.riparabile());
    }

    #[test]
    fn col_file_crlf_sul_disco_si_normalizza_il_file_non_il_registro() {
        // Verso opposto: il registro ha ragione, il working tree no. Riparare
        // qui fisserebbe per sempre un hash che nessun checkout conforme
        // riproduce, cioe' trasformerebbe l'incidente in una regola.
        let registrato = sha384(SQL);
        let causa = classifica(&crlf(SQL), &registrato);
        assert_eq!(causa, Some(CausaDivergenza::FineRigaSulDisco));
        assert!(!CausaDivergenza::FineRigaSulDisco.riparabile());
    }

    #[test]
    fn un_contenuto_diverso_non_diventa_mai_riparabile() {
        let altro = sha384(b"DROP TABLE t;\n");
        assert_eq!(
            classifica(SQL, &altro),
            Some(CausaDivergenza::ContenutoDiverso)
        );
        assert!(!CausaDivergenza::ContenutoDiverso.riparabile());
        // Nemmeno quando differisce di pochi byte: la prova e' l'hash, mai la
        // distanza fra le lunghezze.
        let quasi = sha384(b"-- una migrazione\nINSERT INTO t VALUES (2);\nSELECT 1;\n");
        assert_eq!(
            classifica(SQL, &quasi),
            Some(CausaDivergenza::ContenutoDiverso)
        );
    }

    #[test]
    fn byte_identici_non_sono_una_divergenza() {
        assert_eq!(classifica(SQL, &sha384(SQL)), None);
    }

    fn voce(versione: i64, verdetto: VerdettoVersione) -> Voce {
        Voce {
            versione,
            verdetto,
            checksum_sul_disco: Some(sha384(SQL)),
            checksum_registrato: Some(sha384(b"altro")),
        }
    }

    /// Il filtro che sta fra il censimento e l'UPDATE. Se lasciasse passare una
    /// causa non riparabile, il comando riscriverebbe il checksum di una
    /// migrazione davvero modificata: l'immutabilita' che il registro esiste
    /// per garantire finirebbe cancellata proprio dallo strumento che dovrebbe
    /// difenderla.
    #[test]
    fn si_ripara_solo_cio_che_e_provato_e_il_resto_resta_dichiarato() {
        let c = Censimento {
            set: Set::Meta,
            voci: vec![
                voce(1, VerdettoVersione::Allineata),
                voce(2, VerdettoVersione::Pendente),
                voce(3, VerdettoVersione::Divergente(CausaDivergenza::FineRigaNelRegistro)),
                voce(4, VerdettoVersione::Divergente(CausaDivergenza::FineRigaSulDisco)),
                voce(5, VerdettoVersione::Divergente(CausaDivergenza::ContenutoDiverso)),
                voce(6, VerdettoVersione::ApplicataSenzaFile),
            ],
        };
        let riparabili: Vec<i64> = c.riparabili().iter().map(|v| v.versione).collect();
        assert_eq!(riparabili, vec![3], "solo il verso provato e riparabile");

        // E cio' che resta bloccante viene DICHIARATO, altrimenti il comando
        // chiuderebbe con successo su un database che ancora rifiuta di migrare.
        let bloccanti: Vec<i64> = c
            .bloccanti_non_riparabili()
            .iter()
            .map(|v| v.versione)
            .collect();
        assert_eq!(bloccanti, vec![4, 5, 6]);
        assert_eq!(c.pendenti(), vec![2]);
        assert_eq!(c.senza_file(), vec![6]);
    }

    #[test]
    fn la_conversione_non_raddoppia_i_ritorni_di_un_file_misto() {
        let misto = b"a\r\nb\nc\r\n";
        assert_eq!(a_lf(misto), b"a\nb\nc\n");
        assert_eq!(a_crlf(misto), b"a\r\nb\r\nc\r\n");
    }

    /// Il ponte fra questo modulo e sqlx: se si spezzasse, la riparazione
    /// scriverebbe un hash che il migrator non riconosce — e il sintomo
    /// sarebbe identico al difetto che il modulo esiste per chiudere.
    #[tokio::test]
    async fn il_nostro_sha384_e_quello_che_sqlx_registra() {
        let dir = tempfile::tempdir().expect("tempdir");
        let set = dir.path().join("db").join("migrations");
        std::fs::create_dir_all(&set).expect("mkdir");
        std::fs::write(set.join("0001_prova.sql"), SQL).expect("write");

        let origine = OrigineSet::esplicita(dir.path());
        let migrator = crate::risolvi(Set::Meta, &origine).await.expect("migrator");
        let m = migrator.iter().next().expect("una migrazione");
        assert_eq!(
            m.checksum.as_ref(),
            sha384(SQL).as_slice(),
            "sqlx registra un hash diverso da quello che questo modulo calcola"
        );
        // E la classificazione, partendo dai byte che sqlx ha letto, concorda.
        assert_eq!(classifica(m.sql.as_bytes(), &m.checksum), None);
    }
}
