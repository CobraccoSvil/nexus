//! Chiave dello STATO su cui una suite gira: «se rieseguissi adesso, sarebbe la
//! stessa domanda?».
//!
//! Due componenti, perche' una suite E2E interroga due cose:
//!
//! - il CODICE dell'albero su cui il run lavora (digest di percorso, dimensione
//!   e istante di modifica dei sorgenti);
//! - la GENERAZIONE dei servizi del progetto (pid e istante d'avvio dei processi
//!   vivi). Senza, un `passed` memorizzato varrebbe anche dopo lo spegnimento
//!   del servizio che lo aveva reso vero: la memoria diventerebbe un fail-open,
//!   cioe' il difetto peggiore fra quelli che questo presidio esiste per
//!   togliere. Con, un riavvio del servizio invalida la memoria da se' — ed e'
//!   esattamente il momento in cui i test erano instabili nella serata misurata.
//!
//! Le esclusioni sono LOAD-BEARING, non igiene: `test-results/` e
//! `playwright-report/` li riscrive Playwright a OGNI esecuzione. Contarli
//! renderebbe la chiave diversa subito dopo ogni run, la memoria non
//! risponderebbe MAI e il presidio sarebbe inerte pur essendo tutto "scritto e
//! testato" — la forma di guasto che non si vede (regola O). Il test
//! `esecuzione_della_suite_non_cambia_la_chiave` esiste per quello.
//!
//! Il criterio del digest e' (percorso, dimensione, mtime), non il contenuto:
//! qui la domanda non e' "e' cambiato davvero?" (per quella c'e'
//! `correction_progress`, che confronta gli sha) ma "posso ancora fidarmi
//! dell'esito?". Un mtime che si muove senza contenuto costa una riesecuzione
//! in piu'; il contrario — contenuto cambiato senza che la chiave se ne accorga
//! — costerebbe un esito bugiardo.

use std::collections::BTreeMap;
use std::path::Path;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

/// Directory che NON entrano nella chiave: prodotte dagli strumenti (incluso il
/// runner stesso), non scritte da chi sviluppa.
pub const DIRECTORY_ESCLUSE: &[&str] = &[
    // Prodotte dal runner di test a ogni esecuzione: vedi nota del modulo.
    "test-results",
    "playwright-report",
    "blob-report",
    // Dipendenze e artefatti di build.
    "node_modules",
    "dist",
    "build",
    "out",
    "target",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".output",
    "coverage",
    ".turbo",
    ".cache",
    ".vite",
    ".parcel-cache",
    ".pytest_cache",
    "__pycache__",
    ".venv",
    "venv",
    // Metadati e log.
    ".git",
    ".idea",
    ".vscode",
    "logs",
];

/// Estensioni che non entrano nella chiave: file che l'applicazione stessa
/// riscrive mentre gira (un log che cresce renderebbe ogni chiave unica).
const ESTENSIONI_ESCLUSE: &[&str] = &["log", "tmp", "swp", "pid", "lock~"];

/// Tetto di file scandagliati. Oltre, la chiave NON si calcola: un albero
/// enorme costerebbe piu' della riesecuzione che eviterebbe, e una chiave
/// calcolata "a metà" sarebbe peggio di nessuna chiave.
const MAX_FILE: usize = 30_000;

/// Profondita' massima della discesa.
const MAX_DEPTH: u32 = 12;

/// Chiave dello stato, oppure la dichiarazione che non e' calcolabile.
///
/// Il ramo negativo esiste perche' "non ho potuto guardare" non diventi "non e'
/// cambiato niente": senza chiave non si memorizza e non si riclassifica, si
/// riesegue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateKey {
    Calcolata(String),
    NonCalcolabile(&'static str),
}

impl StateKey {
    /// La chiave, se calcolabile. `None` significa "non ho potuto guardare":
    /// niente memoria e niente riclassificazione, si riesegue.
    pub fn valore(&self) -> Option<String> {
        match self {
            StateKey::Calcolata(s) => Some(s.clone()),
            StateKey::NonCalcolabile(_) => None,
        }
    }

    /// Perche' la chiave non e' calcolabile, quando non lo e'.
    pub fn motivo(&self) -> Option<&'static str> {
        match self {
            StateKey::Calcolata(_) => None,
            StateKey::NonCalcolabile(m) => Some(m),
        }
    }
}

/// Digest dei sorgenti dell'albero `root`.
///
/// Deterministico per costruzione: i percorsi si ordinano (`BTreeMap`) prima di
/// entrare nell'hash, perche' l'ordine di `read_dir` non e' garantito e due
/// letture dello stesso albero devono dare la stessa chiave.
pub fn digest_albero(root: &Path) -> StateKey {
    if !root.is_dir() {
        return StateKey::NonCalcolabile("radice del run inesistente");
    }
    let mut file: BTreeMap<String, (u64, i128)> = BTreeMap::new();
    if !raccogli(root, root, 0, &mut file) {
        return StateKey::NonCalcolabile("albero troppo grande per una chiave affidabile");
    }
    if file.is_empty() {
        return StateKey::NonCalcolabile("nessun file sorgente sotto la radice del run");
    }
    let mut hasher = Sha256::new();
    for (path, (len, mtime)) in &file {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(len.to_le_bytes());
        hasher.update(mtime.to_le_bytes());
        hasher.update(b"\n");
    }
    let digest = hex(&hasher.finalize());
    tracing::debug!(
        target: "mcp_core::suite_verification",
        file = file.len(),
        digest = %digest,
        "chiave di stato: digest dell'albero"
    );
    StateKey::Calcolata(digest)
}

/// Ritorna `false` se il tetto di file e' stato superato (chiave inaffidabile).
fn raccogli(
    root: &Path,
    dir: &Path,
    depth: u32,
    out: &mut BTreeMap<String, (u64, i128)>,
) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        // Una directory illeggibile non invalida la chiave: e' un pezzo di
        // albero che nessuno dei due lati (esecuzione e riesecuzione) puo'
        // vedere, quindi non introduce differenza fra le due misure.
        return true;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let nome = entry.file_name().to_string_lossy().to_string();
        let Ok(tipo) = entry.file_type() else {
            continue;
        };
        if tipo.is_dir() {
            if DIRECTORY_ESCLUSE.contains(&nome.as_str()) {
                continue;
            }
            if !raccogli(root, &path, depth + 1, out) {
                return false;
            }
            continue;
        }
        if !tipo.is_file() {
            continue;
        }
        if let Some((rel, fatto)) = fatto_del_file(root, &entry, &path) {
            out.insert(rel, fatto);
            if out.len() > MAX_FILE {
                return false;
            }
        }
    }
    true
}

/// I due fatti che entrano nella chiave per un file: dimensione e istante di
/// modifica, sotto il suo percorso relativo. `None` se il file e' escluso per
/// estensione o se i metadati non sono leggibili — un file che non si e'
/// potuto guardare non entra, e non entra allo stesso modo nelle due letture
/// che vengono confrontate.
fn fatto_del_file(
    root: &Path,
    entry: &std::fs::DirEntry,
    path: &Path,
) -> Option<(String, (u64, i128))> {
    let escluso = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| ESTENSIONI_ESCLUSE.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false);
    if escluso {
        return None;
    }
    let meta = entry.metadata().ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(-1);
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    Some((rel, (meta.len(), mtime)))
}

/// Generazione dei servizi vivi del progetto: pid e istante d'avvio.
///
/// `None` = il registro non e' interrogabile (DB non raggiungibile). Non si
/// ripiega su "nessun servizio": non sapere che servizi girano e' diverso da
/// sapere che non ne gira nessuno, e la differenza e' precisamente quella fra
/// una memoria affidabile e un fail-open.
pub async fn generazione_ambiente(db: &PgPool, project_id: Uuid) -> Option<String> {
    let righe: Vec<(String, Option<i32>, Option<chrono::DateTime<chrono::Utc>>)> = sqlx::query_as(
        "SELECT label, pid, started_at FROM agent_processes \
         WHERE project_id = $1 AND status IN ('running', 'starting') \
         ORDER BY label ASC, pid ASC",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .ok()?;

    let mut hasher = Sha256::new();
    for (label, pid, started) in &righe {
        hasher.update(label.as_bytes());
        hasher.update(b"\0");
        hasher.update(pid.unwrap_or(-1).to_le_bytes());
        hasher.update(
            started
                .map(|t| t.timestamp_millis())
                .unwrap_or(-1)
                .to_le_bytes(),
        );
        hasher.update(b"\n");
    }
    Some(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Implementazione reale di [`super::ChiaveDiStato`]: albero + servizi vivi.
pub struct ChiaveAlberoEServizi {
    root: std::path::PathBuf,
    db: PgPool,
    project_id: Uuid,
}

impl ChiaveAlberoEServizi {
    /// `root` e' la radice del CODICE che i test esercitano (la radice del
    /// run), non quella da cui il comando parte.
    pub fn new(root: std::path::PathBuf, db: PgPool, project_id: Uuid) -> Self {
        Self {
            root,
            db,
            project_id,
        }
    }
}

#[async_trait]
impl super::ChiaveDiStato for ChiaveAlberoEServizi {
    async fn chiave(&self) -> StateKey {
        let root = self.root.clone();
        // Il digest e' I/O bloccante su un albero intero: fuori dal reattore.
        let albero = tokio::task::spawn_blocking(move || digest_albero(&root))
            .await
            .unwrap_or(StateKey::NonCalcolabile(
                "calcolo del digest interrotto",
            ));
        let StateKey::Calcolata(codice) = albero else {
            return albero;
        };
        let Some(ambiente) = generazione_ambiente(&self.db, self.project_id).await else {
            return StateKey::NonCalcolabile(
                "registro dei processi non interrogabile: generazione d'ambiente ignota",
            );
        };
        StateKey::Calcolata(format!("{codice}:{ambiente}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn albero_di_prova(nome: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nexus-statekey-{nome}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src")).expect("crea src");
        std::fs::write(dir.join("src/app.ts"), "export const a = 1;").expect("scrivi sorgente");
        dir
    }

    /// Il presidio intero dipende da questo: se l'esecuzione della suite
    /// cambiasse la chiave, la memoria non risponderebbe MAI e il fix
    /// sembrerebbe applicato mentre e' inerte. Mutazione: togliendo
    /// "test-results" da `DIRECTORY_ESCLUSE`, questo test diventa rosso.
    #[test]
    fn esecuzione_della_suite_non_cambia_la_chiave() {
        let dir = albero_di_prova("artefatti");
        let prima = digest_albero(&dir).valore().expect("chiave iniziale");

        // Cio' che Playwright scrive a ogni run.
        std::fs::create_dir_all(dir.join("test-results/home-spec-chromium"))
            .expect("crea test-results");
        std::fs::write(
            dir.join("test-results/.last-run.json"),
            r#"{"status":"failed","failedTests":["abc"]}"#,
        )
        .expect("scrivi last-run");
        std::fs::write(
            dir.join("test-results/home-spec-chromium/test-failed-1.png"),
            [0u8; 32],
        )
        .expect("scrivi screenshot");
        std::fs::create_dir_all(dir.join("playwright-report")).expect("crea report");
        std::fs::write(dir.join("playwright-report/index.html"), "<html></html>")
            .expect("scrivi report");

        let dopo = digest_albero(&dir).valore().expect("chiave dopo il run");
        assert_eq!(
            prima, dopo,
            "gli artefatti prodotti dal runner non devono cambiare la chiave"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Una scrittura nel codice invalida la chiave: e' il modo in cui la
    /// memoria si accorge che l'esito non vale piu'.
    #[test]
    fn una_scrittura_nel_codice_cambia_la_chiave() {
        let dir = albero_di_prova("scrittura");
        let prima = digest_albero(&dir).valore().expect("chiave iniziale");
        std::fs::write(dir.join("src/nuovo.tsx"), "export const b = 2;").expect("nuovo file");
        let dopo = digest_albero(&dir).valore().expect("chiave dopo");
        assert_ne!(prima, dopo);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn albero_inesistente_non_e_calcolabile() {
        let dir = std::env::temp_dir().join(format!("nexus-assente-{}", Uuid::new_v4()));
        assert!(matches!(
            digest_albero(&dir),
            StateKey::NonCalcolabile(_)
        ));
    }

    /// Le dipendenze non entrano: `pnpm install` non deve invalidare gli esiti
    /// (e su un albero con node_modules il digest costerebbe quanto la suite).
    #[test]
    fn node_modules_non_entra_nella_chiave() {
        let dir = albero_di_prova("deps");
        let prima = digest_albero(&dir).valore().expect("chiave iniziale");
        std::fs::create_dir_all(dir.join("node_modules/pacchetto")).expect("crea node_modules");
        std::fs::write(dir.join("node_modules/pacchetto/index.js"), "module.exports={}")
            .expect("scrivi dipendenza");
        assert_eq!(prima, digest_albero(&dir).valore().expect("chiave dopo"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
