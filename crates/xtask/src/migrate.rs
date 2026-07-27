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

/// Le migrazioni pendenti rispetto al registro del database.
async fn pendenti(pool: &sqlx::PgPool, versioni: &[i64]) -> anyhow::Result<Vec<i64>> {
    let applicate = applicate_sul_db(pool).await?;
    Ok(versioni
        .iter()
        .copied()
        .filter(|v| !applicate.contains(v))
        .collect())
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
        _ => {
            let p = pendenti(&pool, &versioni).await?;
            if p.is_empty() {
                println!("nessuna migrazione pendente");
                0
            } else {
                let elenco: Vec<String> = p.iter().map(|v| v.to_string()).collect();
                println!("{} migrazioni pendenti: {}", p.len(), elenco.join(", "));
                println!("per applicarle: cargo xtask migrate --set {set} --apply");
                USCITA_PENDENTI
            }
        }
    };
    pool.close().await;
    Ok(esito)
}

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    let o = parse(args)?;
    let Some(modo) = o.modo else {
        eprintln!(
            "uso: cargo xtask migrate --set meta|project (--dry-run | --check | --apply) \
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

/// Versioni gia' applicate, lette con l'API di sqlx.
async fn applicate_sul_db(pool: &sqlx::PgPool) -> anyhow::Result<Vec<i64>> {
    use sqlx::migrate::Migrate;
    let mut conn = pool.acquire().await.context("connessione per il registro")?;
    // `ensure_migrations_table` e' idempotente: su un DB vergine crea il
    // registro vuoto invece di far fallire la lettura con "relation does not
    // exist", che sarebbe un errore travestito da assenza.
    conn.ensure_migrations_table()
        .await
        .context("registro delle migrazioni")?;
    let applicate = conn
        .list_applied_migrations()
        .await
        .context("lettura del registro delle migrazioni")?;
    Ok(applicate.into_iter().map(|m| m.version).collect())
}

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
