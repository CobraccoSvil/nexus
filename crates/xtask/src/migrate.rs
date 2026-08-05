//! `xtask migrate` — applica un set di migrazioni senza avviare un servizio.
//!
//! IL CICLO CHE ROMPE. Il catalogo `system.services_catalog` arriva dalle
//! migrazioni; le migrazioni le applicava soltanto mcp-core all'avvio; per
//! avviare mcp-core come servizio serve il suo manifest; il manifest lo genera
//! `xtask service-manifests` leggendo il catalogo dal DB. Su un database
//! vergine quel giro non ha un punto di ingresso: catalogo <- migrazioni <-
//! mcp-core <- manifest <- generatore <- catalogo.
//!
//! Questo comando e' il punto di ingresso, e non porta un motore proprio:
//! delega a `nexus-migrations`, lo stesso codice che mcp-core esegue all'avvio.
//! Se ne portasse uno suo sarebbe la decima incarnazione del concern, cioe' il
//! difetto che quel crate esiste per chiudere.
//!
//! `--repair-checksums` e' l'unico modo che scrive sul registro
//! `_sqlx_migrations`, e serve al caso in cui il registro conservi l'hash degli
//! stessi byte con altri fine-riga (vedi `nexus-migrations::registro`). Non puo'
//! essere una migrazione: il migrator valida i checksum PRIMA di applicare, e
//! una migrazione riparatrice non verrebbe mai eseguita.

use anyhow::Context;
use nexus_migrations::{OrigineSet, Set};

use crate::premessa::db_dichiarato;

/// Esito del comando, come codice di uscita.
///
/// Il 3 e' distinto dall'1 di proposito: un gate deve poter separare "il DB e'
/// indietro" (azione: applicare) da "non ho potuto guardare" (azione: capire
/// perche'). Un unico codice costringerebbe chi lo consuma a leggere il testo,
/// che e' cio' che la regola M vieta.
const USCITA_PENDENTI: i32 = 3;

/// Il registro non corrisponde al set: `--apply` non partirebbe affatto, e la
/// cura non e' applicare. Separato dal 3 per la stessa ragione per cui il 3 e'
/// separato dall'1 — sono tre azioni diverse.
const USCITA_REGISTRO_DISALLINEATO: i32 = 4;

struct Opzioni {
    set: Set,
    database_url: Option<String>,
    radice: Option<std::path::PathBuf>,
    modo: Option<Modo>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Modo {
    /// Elenca cosa farebbe, senza toccare il DB.
    DryRun,
    /// Verifica se ci sono migrazioni pendenti; non applica.
    Check,
    /// Applica.
    Apply,
    /// Riallinea il checksum registrato delle sole versioni la cui divergenza
    /// e' provata essere di soli fine-riga. Non applica nulla.
    RepairChecksums,
}

fn parse(args: &[String]) -> anyhow::Result<Opzioni> {
    let mut o = Opzioni {
        set: Set::Meta,
        database_url: None,
        radice: None,
        modo: None,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--set" => {
                let v = args.get(i + 1).context("--set richiede un valore")?;
                o.set = Set::try_parse(v).with_context(|| {
                    format!("set '{v}' sconosciuto: valori ammessi 'meta' e 'project'")
                })?;
                i += 1;
            }
            "--database-url" => {
                o.database_url = args.get(i + 1).cloned();
                i += 1;
            }
            "--migrations-root" => {
                o.radice = args.get(i + 1).map(std::path::PathBuf::from);
                i += 1;
            }
            "--dry-run" => o.modo = Some(Modo::DryRun),
            "--check" => o.modo = Some(Modo::Check),
            "--apply" => o.modo = Some(Modo::Apply),
            "--repair-checksums" => o.modo = Some(Modo::RepairChecksums),
            altro => anyhow::bail!("argomento sconosciuto: {altro}"),
        }
        i += 1;
    }
    Ok(o)
}

/// Radice del repository dal marker `.git`, risalendo dall'albero di
/// COMPILAZIONE.
///
/// Non dalla directory di lavoro: uno strumento che si orienta su dove e' stato
/// invocato misura un albero e ne dichiara un altro. Il limite e' dichiarato:
/// un binario compilato in un albero ed eseguito da un altro guarda il primo,
/// e per questo la premessa stampa sempre la radice effettiva e `--migrations-root`
/// permette di scavalcarla.
fn radice_dal_marker() -> anyhow::Result<std::path::PathBuf> {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if p.join(".git").exists() {
            return Ok(p);
        }
        if !p.pop() {
            anyhow::bail!(
                "radice del repository non trovata risalendo da CARGO_MANIFEST_DIR: \
                 indicare il set con --migrations-root"
            );
        }
    }
}

/// L'URL del database e da dove viene.
///
/// La provenienza si misura PRIMA di `dotenvy`: `dotenv()` non sovrascrive una
/// variabile gia' presente, quindi dopo non si distinguerebbe piu' chi l'ha
/// fornita — e la premessa direbbe una cosa non verificata.
fn risolvi_url(esplicito: Option<&String>) -> anyhow::Result<(String, &'static str)> {
    let da_ambiente = std::env::var("DATABASE_URL").ok();
    dotenvy::dotenv().ok();
    match (esplicito, da_ambiente) {
        (Some(u), _) => Ok((u.clone(), "--database-url")),
        (None, Some(u)) => Ok((u, "ambiente")),
        (None, None) => Ok((
            std::env::var("DATABASE_URL").context(
                "DATABASE_URL non impostata: le migrazioni hanno bisogno di sapere \
                 QUALE database portare avanti, e non esiste un default. \
                 Valorizzarla nell'ambiente, nel .env del repo, oppure passare \
                 --database-url.",
            )?,
            ".env del repo",
        )),
    }
}

/// Le prime cifre di un checksum: bastano a distinguerlo, e una riga di
/// diagnosi con due hash interi non la legge nessuno.
fn breve(checksum: Option<&Vec<u8>>) -> String {
    match checksum {
        Some(c) => c.iter().take(8).map(|b| format!("{b:02x}")).collect(),
        None => "-".into(),
    }
}

/// Cosa dice il censimento, oltre al numero delle pendenti.
///
/// Sta qui e non nel crate perche' e' resa, non decisione: il verdetto e' gia'
/// stato preso in `nexus-migrations::registro`.
fn stampa_divergenti(divergenti: &[&nexus_migrations::registro::Voce]) {
    println!("{} migrazioni col checksum divergente:", divergenti.len());
    for v in divergenti {
        let nexus_migrations::VerdettoVersione::Divergente(causa) = v.verdetto else {
            continue;
        };
        println!(
            "  {} registrato {} disco {} -> {:?}: {}",
            v.versione,
            breve(v.checksum_registrato.as_ref()),
            breve(v.checksum_sul_disco.as_ref()),
            causa,
            causa.cura()
        );
    }
}

fn stampa_senza_file(senza_file: &[i64]) {
    let elenco: Vec<String> = senza_file.iter().map(|v| v.to_string()).collect();
    println!(
        "{} migrazioni applicate senza file nel set: {}. Questo albero non \
         contiene lo schema che il database dichiara: allinearlo al branch da \
         cui il DB e' stato migrato, oppure indicare l'albero giusto con \
         --migrations-root.",
        senza_file.len(),
        elenco.join(", ")
    );
}

fn racconta(censimento: &nexus_migrations::Censimento) -> i32 {
    let pendenti = censimento.pendenti();
    let divergenti = censimento.divergenti();
    let senza_file = censimento.senza_file();

    if !divergenti.is_empty() {
        stampa_divergenti(&divergenti);
    }
    if !senza_file.is_empty() {
        stampa_senza_file(&senza_file);
    }

    if !censimento.bloccanti_non_riparabili().is_empty() {
        return USCITA_REGISTRO_DISALLINEATO;
    }
    if !divergenti.is_empty() {
        // Riparabili: il DB non e' indietro, il registro e' da riallineare.
        println!("per riallinearle: cargo xtask migrate --set {} --repair-checksums", censimento.set);
        return USCITA_REGISTRO_DISALLINEATO;
    }
    if !pendenti.is_empty() {
        let elenco: Vec<String> = pendenti.iter().map(|v| v.to_string()).collect();
        println!("{} migrazioni pendenti: {}", pendenti.len(), elenco.join(", "));
        println!(
            "per applicarle: cargo xtask migrate --set {} --apply",
            censimento.set
        );
        return USCITA_PENDENTI;
    }
    println!("nessuna migrazione pendente, nessun checksum divergente");
    0
}

async fn esegui(
    modo: Modo,
    set: Set,
    origine: &OrigineSet,
    url: &str,
) -> anyhow::Result<i32> {
    // Il set si legge sempre, anche in dry-run: se non e' leggibile non c'e'
    // nulla da dire sul database.
    let migrator = nexus_migrations::risolvi(set, origine).await?;
    let versioni: Vec<i64> = migrator.iter().map(|m| m.version).collect();
    println!(
        "set leggibile: {} migrazioni, dalla {} alla {}",
        versioni.len(),
        versioni.first().copied().unwrap_or(0),
        versioni.last().copied().unwrap_or(0)
    );
    if modo == Modo::DryRun {
        return Ok(0);
    }

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(url)
        .await
        .with_context(|| format!("connessione a {}", db_dichiarato(url)))?;

    let esito = match modo {
        Modo::Apply => {
            nexus_migrations::applica(&pool, set, origine).await?;
            println!("set '{set}' applicato");
            0
        }
        Modo::RepairChecksums => ripara(&pool, set, origine).await?,
        // `--check` passa dal censimento e non da un conteggio delle versioni:
        // confrontare le sole liste diceva "nessuna migrazione pendente" anche
        // quando `--apply` sarebbe morto sul primo checksum divergente, cioe'
        // dava un verde che non prova cio' che chi lo legge crede provato
        // (regola O).
        _ => {
            let censimento = nexus_migrations::registro::censisci(&pool, set, origine).await?;
            racconta(&censimento)
        }
    };
    pool.close().await;
    Ok(esito)
}

/// Riallinea i checksum riparabili, dopo aver detto quali e perche'.
///
/// Il piano si stampa PRIMA della scrittura: e' l'unico comando del repo che
/// tocca il registro delle migrazioni, e chi lo esegue deve poter riconoscere
/// cio' che sta per cambiare senza rileggere il codice.
async fn ripara(
    pool: &sqlx::PgPool,
    set: Set,
    origine: &OrigineSet,
) -> anyhow::Result<i32> {
    let censimento = nexus_migrations::registro::censisci(pool, set, origine).await?;
    let riparabili = censimento.riparabili();
    if riparabili.is_empty() {
        println!("nessun checksum da riallineare");
        // Il resoconto resta dovuto: puo' esserci una divergenza NON riparabile,
        // e chiudere con uno 0 la nasconderebbe.
        return Ok(racconta(&censimento));
    }

    println!(
        "{} checksum da riallineare (divergenza provata di soli fine-riga, file \
         sul disco gia' canonico):",
        riparabili.len()
    );
    for v in &riparabili {
        println!(
            "  {}: {} -> {}",
            v.versione,
            breve(v.checksum_registrato.as_ref()),
            breve(v.checksum_sul_disco.as_ref())
        );
    }

    let riscritte = nexus_migrations::registro::ripara_fine_riga(pool, &censimento).await?;
    let elenco: Vec<String> = riscritte.iter().map(|v| v.to_string()).collect();
    println!("registro riallineato per le versioni: {}", elenco.join(", "));

    // Si ri-censisce invece di dichiarare il successo: il verdetto che conta e'
    // quello del prossimo avvio, e lo si misura sui dati appena scritti.
    let dopo = nexus_migrations::registro::censisci(pool, set, origine).await?;
    Ok(racconta(&dopo))
}

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    let o = parse(args)?;
    let Some(modo) = o.modo else {
        eprintln!(
            "uso: cargo xtask migrate --set meta|project \
             (--dry-run | --check | --apply | --repair-checksums) \
             [--database-url URL] [--migrations-root DIR]"
        );
        return Ok(2);
    };

    let (url, provenienza_url) = risolvi_url(o.database_url.as_ref())?;
    let radice = match &o.radice {
        Some(r) => r.clone(),
        None => radice_dal_marker()?,
    };
    let origine = OrigineSet::esplicita(&radice);

    // Premessa PRIMA di qualunque numero: da dove guardo, e con quali credenziali.
    println!(
        "xtask migrate — set '{}' da {:?} (radice {}), database {} (da {})",
        o.set,
        origine.percorso(o.set),
        radice.display(),
        db_dichiarato(&url),
        provenienza_url
    );

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("runtime tokio")?
        .block_on(esegui(modo, o.set, &origine, &url))
}

// La lettura del registro non vive piu' qui: la fa `nexus-migrations::registro`,
// che e' anche il punto in cui si decide se una versione e' pendente, divergente
// o senza file. Due letture con due criteri erano il motivo per cui `--check`
// poteva dire "nessuna migrazione pendente" su un database che rifiutava di
// migrare (regola L).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn il_set_si_indica_col_nome_canonico() {
        let o = parse(&["--set".into(), "project".into(), "--apply".into()]).expect("parse");
        assert_eq!(o.set, Set::Project);
        // Regola N: niente sinonimi accettati in silenzio.
        assert!(parse(&["--set".into(), "progetto".into()]).is_err());
    }

    #[test]
    fn senza_modo_non_si_fa_nulla() {
        let o = parse(&["--set".into(), "meta".into()]).expect("parse");
        assert!(o.modo.is_none(), "nessun modo predefinito: applicare non e' un default");
    }

    #[test]
    fn la_riparazione_del_registro_e_un_modo_esplicito() {
        let o = parse(&[
            "--set".into(),
            "meta".into(),
            "--repair-checksums".into(),
        ])
        .expect("parse");
        assert!(matches!(o.modo, Some(Modo::RepairChecksums)));
        // Non e' un effetto collaterale di --apply: una scrittura sul registro
        // delle migrazioni la si chiede per nome.
        let a = parse(&["--apply".into()]).expect("parse");
        assert!(matches!(a.modo, Some(Modo::Apply)));
    }

    #[test]
    fn i_codici_di_uscita_distinguono_le_tre_azioni() {
        // "il DB e' indietro", "il registro non corrisponde" e "non ho potuto
        // guardare" vogliono tre risposte diverse da chi consuma il codice.
        assert_ne!(USCITA_PENDENTI, USCITA_REGISTRO_DISALLINEATO);
        assert_ne!(USCITA_PENDENTI, 1);
        assert_ne!(USCITA_REGISTRO_DISALLINEATO, 1);
    }

    #[test]
    fn un_argomento_sconosciuto_non_viene_ignorato() {
        // Ignorarlo silenziosamente farebbe eseguire un comando diverso da
        // quello scritto: --aply invece di --apply non deve applicare nulla.
        assert!(parse(&["--aply".into()]).is_err());
    }

    #[test]
    fn la_radice_si_risolve_dal_marker_e_contiene_i_set() {
        let r = radice_dal_marker().expect("radice");
        assert!(r.join(".git").exists());
        assert!(r.join(Set::Meta.sottopercorso()).is_dir());
        assert!(r.join(Set::Project.sottopercorso()).is_dir());
    }
}
