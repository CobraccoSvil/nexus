//! `xtask service-manifests` — genera i manifest di servizio Windows DERIVANDOLI
//! dal catalogo, invece di leggerli da una lista scritta a mano.
//!
//! Il difetto che chiude: il generatore precedente viveva fuori dal controllo di
//! versione (`D:\IDEAI-runtime\winsw\gen-winsw.ps1`) e teneva una propria lista
//! di servizi. Quella lista non conteneva `browser-bridge-mcp`, che pero' il
//! catalogo dichiara sorvegliato: il servizio non aveva manifest, il watchdog
//! provava a riavviarlo a ogni ciclo e falliva sempre. Nella stessa lista
//! sopravvivevano `chat-service` e `billing-service`, crate rimossi dal repo.
//!
//! Il confine fra le tre fonti, dichiarato una volta:
//!   - CHI sono i servizi -> catalogo DB (`system.services_catalog`);
//!   - COME si avvia un binario del workspace -> `cargo metadata`;
//!   - COME si avviano i tre processi non-workspace -> TOML versionato.
//!
//! Un servizio dichiarato nel catalogo che il workspace non costruisce e' un
//! ERRORE prima di scrivere qualunque file, non un manifest silenziosamente
//! sbagliato che genera un servizio in crash-loop.

pub mod overrides;
pub mod plan;
pub mod winsw;
pub mod workspace;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};

use plan::{Ambiente, ServizioRisolto};

/// Radice del repository, risolta dal marker `.git` risalendo dal file corrente.
///
/// Non da `current_dir`: uno strumento che si orienta sulla directory da cui e'
/// stato invocato misura un albero e ne dichiara un altro (e' gia' successo con
/// `quality-scan --root`).
pub fn repo_root() -> anyhow::Result<PathBuf> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if p.join(".git").exists() {
            return Ok(p);
        }
        if !p.pop() {
            bail!("radice del repository non trovata risalendo da CARGO_MANIFEST_DIR");
        }
    }
}

/// Legge il .env del repo come mappa. Solo i nomi delle chiavi finiscono nel
/// TOML versionato; i valori restano qui e nel manifest generato.
fn leggi_dotenv(repo: &Path) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    let Ok(testo) = std::fs::read_to_string(repo.join(".env")) else {
        return m;
    };
    for riga in testo.lines() {
        let r = riga.trim();
        if r.is_empty() || r.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = r.split_once('=') {
            m.insert(k.trim().to_string(), v.trim().trim_end_matches('\r').to_string());
        }
    }
    m
}

/// Porta di `DATABASE_URL`, per riconoscere il servizio che ospita il catalogo
/// MISURANDOLA invece di dedurla dal nome.
fn porta_di(url: &str) -> Option<u16> {
    // Lo schema va tolto per primo: i suoi `:` e `//` altrimenti si mescolano
    // con quelli di credenziali e porta. Un URL senza credenziali (nessun `@`)
    // e' il caso normale in sviluppo, e sbagliarlo renderebbe l'anti-circolarita'
    // inerte proprio dove serve.
    let senza_schema = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let dopo_credenziali = senza_schema.rsplit('@').next()?;
    let hostport = dopo_credenziali.split(['/', '?']).next()?;
    hostport.rsplit(':').next()?.parse().ok()
}


struct Opzioni {
    out_dir: Option<PathBuf>,
    check: bool,
    write: bool,
    dry_run: bool,
    profilo: String,
}

fn parse(args: &[String]) -> Opzioni {
    let mut o = Opzioni {
        out_dir: None,
        check: false,
        write: false,
        dry_run: false,
        profilo: "debug".to_string(),
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out-dir" => {
                if let Some(v) = args.get(i + 1) {
                    o.out_dir = Some(PathBuf::from(v));
                    i += 1;
                }
            }
            "--profile" => {
                if let Some(v) = args.get(i + 1) {
                    o.profilo = v.clone();
                    i += 1;
                }
            }
            "--check" => o.check = true,
            "--write" => o.write = true,
            "--dry-run" => o.dry_run = true,
            _ => {}
        }
        i += 1;
    }
    o
}

/// Esito di un confronto fra piano e disco.
#[derive(Debug, PartialEq, Eq)]
enum Anomalia {
    Mancante { id: String, path: String },
    Divergente { id: String, path: String },
    Orfano { id: String, path: String },
}

impl std::fmt::Display for Anomalia {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mancante { id, path } => {
                write!(f, "MANCANTE  {id}: nessun manifest in {path}")
            }
            Self::Divergente { id, path } => write!(
                f,
                "DIVERGENTE {id}: il manifest in {path} non corrisponde al piano"
            ),
            Self::Orfano { id, path } => write!(
                f,
                "ORFANO    {id}: {path} non corrisponde ad alcuna voce di catalogo"
            ),
        }
    }
}

/// Confronta il piano con cio' che c'e' sul disco, senza modificarlo.
fn confronta(piano: &[ServizioRisolto], out: &Path) -> Vec<Anomalia> {
    let mut anomalie = Vec::new();
    for s in piano {
        let path = out.join(&s.winsw_id).join(format!("{}.xml", s.winsw_id));
        match std::fs::read_to_string(&path) {
            Err(_) => anomalie.push(Anomalia::Mancante {
                id: s.winsw_id.clone(),
                path: path.display().to_string(),
            }),
            Ok(esistente) => {
                let atteso = winsw::parse_winsw(&winsw::emit_winsw(s));
                if winsw::parse_winsw(&esistente) != atteso {
                    anomalie.push(Anomalia::Divergente {
                        id: s.winsw_id.clone(),
                        path: path.display().to_string(),
                    });
                }
            }
        }
    }
    // Directory sul disco che il piano non conosce: sono i manifest dei servizi
    // rimossi, che nessuno ha mai notato.
    let attesi: std::collections::BTreeSet<&str> =
        piano.iter().map(|s| s.winsw_id.as_str()).collect();
    if let Ok(letture) = std::fs::read_dir(out) {
        for e in letture.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let Some(nome) = e.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if nome.starts_with('_') || attesi.contains(nome.as_str()) {
                continue;
            }
            if e.path().join(format!("{nome}.xml")).exists() {
                anomalie.push(Anomalia::Orfano {
                    id: nome.clone(),
                    path: e.path().display().to_string(),
                });
            }
        }
    }
    anomalie
}

/// Scrive i manifest e li RILEGGE prima di considerarli buoni.
fn scrivi(piano: &[ServizioRisolto], out: &Path) -> anyhow::Result<()> {
    for s in piano {
        let dir = out.join(&s.winsw_id);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creazione di {}", dir.display()))?;
        let path = dir.join(format!("{}.xml", s.winsw_id));
        let xml = winsw::emit_winsw(s);
        std::fs::write(&path, xml.as_bytes())
            .with_context(|| format!("scrittura di {}", path.display()))?;

        // Readback attraverso il consumatore: si rilegge dal disco cio' che si
        // e' appena scritto e lo si confronta col piano. Asserire sulla stringa
        // appena composta proverebbe solo che sappiamo concatenare.
        let riletto = std::fs::read_to_string(&path)
            .with_context(|| format!("rilettura di {}", path.display()))?;
        let facts = winsw::parse_winsw(&riletto);
        if facts.executable != s.executable
            || facts.working_directory != s.working_directory
            || facts.arguments != s.arguments
            || facts.env != s.env
        {
            bail!(
                "{}: il manifest riletto non corrisponde al piano (scrittura o \
                 formato non fedeli)",
                path.display()
            );
        }
    }
    Ok(())
}

/// Costruisce il piano leggendo catalogo, workspace e file versionato.
async fn costruisci_piano(
    db_url: &str,
    repo: &Path,
    profilo: &str,
) -> anyhow::Result<Vec<ServizioRisolto>> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(db_url)
        .await
        .with_context(|| {
            format!(
                "connessione a {} per leggere il catalogo servizi",
                crate::premessa::db_dichiarato(db_url)
            )
        })?;
    let piano = piano_da_pool(&pool, repo, profilo, Some(db_url)).await;
    pool.close().await;
    piano
}

/// Il piano a partire da un pool gia' aperto. Separato da [`costruisci_piano`]
/// perche' e' il punto in cui un test puo' entrare con il DB ricostruito dalle
/// migrazioni vere, invece di doversi fabbricare un catalogo (regola O).
async fn piano_da_pool(
    pool: &sqlx::PgPool,
    repo: &Path,
    profilo: &str,
    db_url: Option<&str>,
) -> anyhow::Result<Vec<ServizioRisolto>> {
    let catalogo = nexus_service_catalog::load_catalog(pool).await?;
    let mut porte = BTreeMap::new();
    for e in catalogo.iter() {
        if let Some(p) = nexus_service_catalog::resolve_port(pool, e).await {
            porte.insert(e.name.clone(), p);
        }
    }

    let bins = workspace::bin_targets(repo)?;
    let toml_path = repo.join("deploy/service-exec-overrides.toml");
    let (scostamenti, ordine) = overrides::carica(&toml_path)?;

    let amb = ambiente(repo, profilo, porte, db_url.and_then(porta_di));

    println!(
        "xtask service-manifests: catalogo da {} chiave system.services_catalog, \
         {} voci, albero {}, profilo {}, ordine di avvio {} id (attese: {})",
        db_url.map(crate::premessa::db_dichiarato).unwrap_or_else(|| "pool fornito".into()),
        catalogo.len(),
        amb.repo_root,
        profilo,
        ordine.avvio.len(),
        ordine
            .attesa_dopo
            .iter()
            .map(|(k, v)| format!("{k}={v}ms"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    match plan::plan(&catalogo, &bins, &scostamenti, &ordine, &amb) {
        Ok(p) => Ok(p),
        Err(errs) => {
            eprintln!("\nxtask service-manifests: il piano non e' producibile:");
            for e in errs.iter() {
                eprintln!("  - {e}");
            }
            bail!("{} difetti nel piano dei servizi", errs.len())
        }
    }
}

/// Raccoglie in un solo posto tutto cio' che il piano deve sapere del mondo.
fn ambiente(
    repo: &Path,
    profilo: &str,
    porte: BTreeMap<String, u16>,
    porta_db: Option<u16>,
) -> Ambiente {
    let barre = |p: PathBuf| p.display().to_string().replace('\\', "/");
    Ambiente {
        repo_root: barre(repo.to_path_buf()),
        runtime_root: std::env::var("NEXUS_RUNTIME_ROOT")
            .unwrap_or_else(|_| "D:/IDEAI-runtime".to_string())
            .replace('\\', "/"),
        bin_dir: barre(repo.join("target").join(profilo)),
        exe_ext: if cfg!(windows) { ".exe" } else { "" }.to_string(),
        node: trova_node(),
        dotenv: leggi_dotenv(repo),
        porte,
        porta_db,
    }
}

fn trova_node() -> Option<String> {
    let comando = if cfg!(windows) { "where" } else { "which" };
    let out = std::process::Command::new(comando).arg("node").output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|s| s.trim().replace('\\', "/"))
}

/// Entry point del sottocomando. Ritorna l'exit code.
pub fn run(args: &[String]) -> anyhow::Result<i32> {
    let o = parse(args);
    if !o.check && !o.write && !o.dry_run {
        eprintln!(
            "uso: cargo xtask service-manifests [--dry-run | --check | --write] \
             [--out-dir DIR] [--profile debug|release]"
        );
        return Ok(2);
    }
    dotenvy::dotenv().ok();
    let db_url = std::env::var("DATABASE_URL").context(
        "DATABASE_URL non impostata: il catalogo dei servizi vive nel DB e non ha \
         un default. Valorizzala nell'ambiente o nel .env del repo.",
    )?;
    let repo = repo_root()?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("runtime tokio")?;
    let piano = rt.block_on(costruisci_piano(&db_url, &repo, &o.profilo))?;

    stampa_piano(&piano);

    if o.dry_run {
        return Ok(0);
    }

    let out = o
        .out_dir
        .context("--out-dir e' obbligatorio con --check e --write")?;

    // Il confronto viene PRIMA della scrittura: un check eseguito dopo il write
    // e' verde per costruzione e non misura nulla.
    let anomalie = riporta_anomalie(&piano, &out);

    if o.write {
        applica(&piano, &out)?;
        return Ok(0);
    }

    Ok(if anomalie.is_empty() { 0 } else { 1 })
}

/// Il piano a video, con la provenienza di ogni riga: chi legge deve poter
/// sapere quale regola ha prodotto quel manifest.
fn stampa_piano(piano: &[ServizioRisolto]) {
    println!("piano: {} servizi", piano.len());
    for s in piano.iter() {
        let prov = match &s.provenienza {
            plan::Provenienza::WorkspaceBin { bin, .. } => format!("workspace: {bin}"),
            plan::Provenienza::Scostamento { file, indice } => {
                format!("scostamento: {file}#{indice}")
            }
        };
        println!("  {:<22} {} [{}]", s.winsw_id, s.executable, prov);
    }
}

/// Confronta e riporta, senza decidere: la decisione e' del chiamante.
fn riporta_anomalie(piano: &[ServizioRisolto], out: &Path) -> Vec<Anomalia> {
    let anomalie = confronta(piano, out);
    if anomalie.is_empty() {
        println!("disco allineato al piano: nessuna anomalia in {}", out.display());
    } else {
        println!("\nanomalie fra piano e {}:", out.display());
        for a in anomalie.iter() {
            println!("  {a}");
        }
    }
    anomalie
}

/// Verifica i presupposti e scrive. I binari devono esistere: un manifest che
/// punta a un file inesistente e' un servizio in crash-loop, non un errore --
/// cattura "hai dimenticato `cargo build`" PRIMA di scrivere qualunque file.
fn applica(piano: &[ServizioRisolto], out: &Path) -> anyhow::Result<()> {
    let mancanti: Vec<&ServizioRisolto> = piano
        .iter()
        .filter(|s| !Path::new(&s.executable).exists())
        .collect();
    if !mancanti.is_empty() {
        eprintln!("\neseguibili non presenti sul disco:");
        for s in mancanti.iter() {
            eprintln!("  - {}: {}", s.winsw_id, s.executable);
        }
        bail!("esegui `cargo build` prima di generare i manifest");
    }
    scrivi(piano, out)?;
    println!("scritti {} manifest in {}", piano.len(), out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La porta del DB e' il segnale su cui si riconosce il servizio che ospita
    /// il catalogo. MUTAZIONE: togliere lo strip dello schema e il caso senza
    /// credenziali — quello di sviluppo — torna a dare None, cioe' l'anti-
    /// circolarita' smette di scattare senza dirlo.
    #[test]
    fn la_porta_del_db_si_misura_dall_url() {
        assert_eq!(porta_di("postgres://u:p@localhost:5433/nexus"), Some(5433));
        assert_eq!(porta_di("postgres://localhost:6543/x"), Some(6543));
        assert_eq!(porta_di("postgresql://nexus@127.0.0.1:5434/app"), Some(5434));
        assert_eq!(porta_di("postgres://localhost/x"), None, "senza porta");
        assert_eq!(
            porta_di("postgres://u:p@h:5433/db?sslmode=require"),
            Some(5433),
            "i parametri di query non fanno parte dell'host"
        );
    }

    #[test]
    fn la_premessa_non_espone_la_password() {
        let r = crate::premessa::db_dichiarato("postgres://utente:segretissimo@host:5433/nexus");
        assert!(!r.contains("segretissimo"), "password esposta: {r}");
        assert!(r.contains("host:5433"), "premessa illeggibile: {r}");
    }

    /// IL TEST CHE ATTRAVERSA TUTTO: catalogo dal DB ricostruito dalle
    /// migrazioni vere, binari da `cargo metadata` vero, scostamenti e ordine
    /// dal TOML VERSIONATO del repo. Nessuno dei tre input e' fabbricato qui.
    ///
    /// E' la condizione che il difetto originale rendeva invisibile: se una
    /// voce di catalogo non ha binario, se il TOML nomina un servizio che non
    /// esiste, o se l'ordine di avvio dimentica un id, il piano fallisce e
    /// questo test rosseggia — prima che qualcuno scriva un manifest.
    #[sqlx::test(migrator = "nexus_test_schema::META_MIGRATOR")]
    async fn il_piano_reale_e_producibile(pool: sqlx::PgPool) {
        let repo = repo_root().expect("repo root");
        let piano = piano_da_pool(&pool, &repo, "debug", None)
            .await
            .expect("il piano sul catalogo e sul TOML veri deve essere producibile");

        let mut ids: Vec<&str> = piano.iter().map(|s| s.winsw_id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            vec![
                "nexus-admin",
                "nexus-browser-bridge",
                "nexus-doc",
                "nexus-garnet",
                "nexus-gateway",
                "nexus-mcp-core",
                "nexus-plugin",
                "nexus-qdrant",
                "nexus-web-ide",
            ],
            "l'insieme dei manifest generati e' cambiato"
        );

        // Il servizio che il generatore precedente non conosceva.
        let bb = piano
            .iter()
            .find(|s| s.winsw_id == "nexus-browser-bridge")
            .expect("browser-bridge nel piano");
        assert!(
            bb.executable.ends_with("browser-bridge-mcp.exe")
                || bb.executable.ends_with("browser-bridge-mcp"),
            "eseguibile inatteso: {}",
            bb.executable
        );
        assert!(
            matches!(bb.provenienza, plan::Provenienza::WorkspaceBin { .. }),
            "browser-bridge deve derivare dal workspace, non da uno scostamento"
        );

        // I tre non-workspace vengono dal TOML versionato.
        for id in ["nexus-web-ide", "nexus-garnet", "nexus-qdrant"] {
            let s = piano.iter().find(|s| s.winsw_id == id).expect(id);
            assert!(
                matches!(s.provenienza, plan::Provenienza::Scostamento { .. }),
                "{id} deve venire da service-exec-overrides.toml"
            );
        }

        // La porta degli argomenti e' quella risolta dal DB, non una costante.
        let garnet = piano.iter().find(|s| s.winsw_id == "nexus-garnet").expect("garnet");
        assert!(
            garnet.arguments.iter().any(|a| a == "6379"),
            "argomenti di garnet senza la porta risolta: {:?}",
            garnet.arguments
        );
    }

    /// Il percorso `--check` MISURATO sui suoi tre esiti, sul piano reale e
    /// attraverso `riporta_anomalie`: la stessa funzione che chiama `run`, non
    /// una scorciatoia su `confronta` (regola O).
    ///
    /// E' il percorso che avrebbe dovuto accorgersi del difetto originale --
    /// manifest di crate rimossi dal repo che restavano sul disco -- e che
    /// nessuna asserzione toccava. MUTAZIONE: fatto ritornare `Vec::new()` da
    /// `confronta`, il test rosseggia su tutti e tre gli esiti.
    #[sqlx::test(migrator = "nexus_test_schema::META_MIGRATOR")]
    async fn il_confronto_distingue_mancante_allineato_e_orfano(pool: sqlx::PgPool) {
        let repo = repo_root().expect("repo root");
        let piano = piano_da_pool(&pool, &repo, "debug", None)
            .await
            .expect("il piano sul catalogo vero deve essere producibile");

        // Non `tempfile`: una dir nota, ripulita all'ingresso, rende il test
        // idempotente anche dopo un fallimento a meta' (regola F).
        let out = std::env::temp_dir().join("xtask-service-manifests-confronto");
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).expect("dir di lavoro");

        // 1. Disco vuoto: ogni servizio del piano manca, e il confronto lo dice
        //    per nome invece di limitarsi a fallire.
        let anomalie = riporta_anomalie(&piano, &out);
        assert_eq!(anomalie.len(), piano.len(), "attesa una MANCANTE per servizio");
        assert!(
            anomalie.iter().all(|a| matches!(a, Anomalia::Mancante { .. })),
            "esiti inattesi su disco vuoto: {anomalie:?}"
        );

        // 2. Dopo la scrittura il disco e' allineato: e' la condizione che rende
        //    il gate `--check` verde, e passa dal parse, non dalla stringa emessa.
        scrivi(&piano, &out).expect("scrittura dei manifest");
        assert_eq!(
            riporta_anomalie(&piano, &out),
            Vec::new(),
            "manifest appena scritti gia' divergenti dal piano"
        );

        // 3. Il manifest di un crate rimosso dal repo: il caso reale
        //    (billing-service) che il generatore precedente lasciava in
        //    crash-loop senza che nessun gate potesse accorgersene.
        let orfano = out.join("nexus-billing");
        std::fs::create_dir_all(&orfano).expect("dir orfana");
        std::fs::write(
            orfano.join("nexus-billing.xml"),
            "<service><id>nexus-billing</id></service>",
        )
        .expect("manifest orfano");
        assert_eq!(
            riporta_anomalie(&piano, &out),
            vec![Anomalia::Orfano {
                id: "nexus-billing".to_string(),
                path: orfano.display().to_string(),
            }],
            "un manifest senza voce di catalogo deve essere ORFANO"
        );
        std::fs::remove_dir_all(&orfano).expect("rimozione orfana");

        // 4. Manifest modificato a mano: DIVERGENTE, non "allineato".
        let primo = &piano[0];
        let path = out.join(&primo.winsw_id).join(format!("{}.xml", primo.winsw_id));
        let manomesso =
            std::fs::read_to_string(&path).expect("lettura").replace(
                &primo.executable,
                "C:/altrove/binario-che-nessuno-ha-pianificato.exe",
            );
        std::fs::write(&path, manomesso).expect("manomissione");
        assert_eq!(
            riporta_anomalie(&piano, &out),
            vec![Anomalia::Divergente {
                id: primo.winsw_id.clone(),
                path: path.display().to_string(),
            }],
            "un eseguibile diverso dal piano deve essere DIVERGENTE"
        );

        let _ = std::fs::remove_dir_all(&out);
    }

    /// La radice si risolve dal marker, non dalla directory di invocazione.
    #[test]
    fn la_radice_del_repo_e_quella_del_marker() {
        let r = repo_root().expect("repo root");
        assert!(r.join(".git").exists());
        assert!(r.join("db/migrations").is_dir());
        assert!(r.join("deploy/service-exec-overrides.toml").exists());
    }
}
