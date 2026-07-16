//! `xtask quality-scan` — gate ratchet sulle metriche di qualita del codice
//! Rust del workspace, basato su [`mcp_quality::scan`] (punto unico, regola L).
//!
//! Metriche sottoposte a gate (possono solo SCENDERE rispetto alla baseline):
//!   - `total`            findings totali
//!   - `long_functions`   funzioni > 50 righe
//!   - `complexity_high`  funzioni con complessita ciclomatica > 20
//!   - `security`         findings di categoria security
//!
//! Modalita:
//!   --gate     (default) confronta con la baseline; esce non-zero se una
//!              metrica e' PEGGIORATA. Se e' migliorata, invita a --update.
//!   --update   ricalcola e riscrive la baseline (riallineamento al ribasso).
//!
//! Flag:
//!   --baseline <PATH>   file baseline (default: scripts/quality-baseline.json
//!                       DELL'ALBERO misurato, vedi [`baseline_path`])
//!   --root <PATH>       radice da scansionare, ripetibile (default crates)
//!   --include-tests     includi i test nei conteggi: sia i file dedicati
//!                       (`tests/`, `*_tests.rs`) sia i moduli `#[cfg(test)]`
//!                       inline. Di default sono esclusi: il gate misura il
//!                       debito del codice di PRODUZIONE, e contare i test
//!                       penalizzerebbe chi ne aggiunge (regola H: ogni fix
//!                       vuole il suo test di regressione).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mcp_quality::scan::{QualityCounts, scan_counts, scan_targets};

const DEFAULT_BASELINE: &str = "scripts/quality-baseline.json";

struct Opts {
    update: bool,
    /// `None` = nessun `--baseline` da riga di comando: il path viene derivato
    /// dall'albero misurato (vedi [`baseline_path`]).
    baseline: Option<String>,
    roots: Vec<PathBuf>,
    include_tests: bool,
    export: Option<String>,
}

fn parse(args: &[String]) -> Opts {
    let mut update = false;
    let mut include_tests = false;
    let mut baseline: Option<String> = None;
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut export: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--update" => update = true,
            "--gate" => update = false,
            "--include-tests" => include_tests = true,
            "--baseline" => {
                if let Some(v) = it.next() {
                    baseline = Some(v.clone());
                }
            }
            "--root" => {
                if let Some(v) = it.next() {
                    roots.push(PathBuf::from(v));
                }
            }
            "--export" => {
                if let Some(v) = it.next() {
                    export = Some(v.clone());
                }
            }
            _ => {}
        }
    }
    if roots.is_empty() {
        roots.push(PathBuf::from("crates"));
    }
    Opts {
        update,
        baseline,
        roots,
        include_tests,
        export,
    }
}

/// Path assoluto e normalizzato (via i `.`/`..`) di `p`, che se relativo si
/// intende — come qualunque argomento da riga di comando — a partire da `cwd`.
/// `cwd` e' un parametro e non `current_dir()` per non dipendere da stato
/// globale di processo, che nei test paralleli non e' isolabile (regola F).
fn absolutize(p: &Path, cwd: &Path) -> PathBuf {
    let joined = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
    std::path::absolute(&joined).unwrap_or(joined)
}

/// Albero di lavoro che contiene `dir`, cioe' il primo antenato con il marker
/// `.git` — che e' una directory nel clone principale e un FILE nei worktree,
/// quindi si guarda l'esistenza, non il tipo.
fn tree_root_of(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find(|d| d.join(".git").exists()).map(PathBuf::from)
}

/// Path della baseline: appartiene all'albero MISURATO, non alla directory da
/// cui si invoca il comando.
///
/// `--root <altro worktree>` misura quell'albero, quindi e' la baseline di
/// QUELL'albero che va confrontata (`--gate`) o riscritta (`--update`).
/// Ancorare il default alla cwd faceva misurare un albero e scrivere quella di
/// un altro — con la cwd sul repo principale, `--root <worktree> --update`
/// riallineava la baseline del repo principale a metriche mai misurate su di
/// esso. Quando i root stanno tutti nell'albero della cwd (il caso di
/// `scripts/quality-scan.sh`, che vi fa `cd`) il risultato non cambia.
///
/// `--baseline <PATH>` esplicito resta un'istruzione dell'utente: si rispetta
/// verbatim, relativo alla cwd come qualunque path da riga di comando.
fn baseline_path(baseline: Option<&str>, roots: &[PathBuf], cwd: &Path) -> Result<PathBuf> {
    if let Some(explicit) = baseline {
        return Ok(absolutize(Path::new(explicit), cwd));
    }
    let mut trees: Vec<PathBuf> = Vec::new();
    for root in roots {
        // Un root fuori da qualunque albero di lavoro (una directory qualsiasi
        // da scansionare) non ha una baseline propria: resta quella della cwd.
        let tree = tree_root_of(&absolutize(root, cwd)).unwrap_or_else(|| cwd.to_path_buf());
        if !trees.contains(&tree) {
            trees.push(tree);
        }
    }
    let [tree] = trees.as_slice() else {
        // Root in alberi diversi: non esiste "la" baseline da aggiornare, e
        // sceglierne una in silenzio ne falserebbe una delle due.
        bail!(
            "--root punta ad alberi di lavoro diversi ({}): la baseline sarebbe ambigua. \
             Esegui una scansione per albero, o indica il file con --baseline.",
            trees.iter().map(|t| t.display().to_string()).collect::<Vec<_>>().join(", ")
        );
    };
    let mut path = tree.clone();
    path.extend(DEFAULT_BASELINE.split('/'));
    Ok(path)
}

fn load_baseline(path: &Path) -> Result<QualityCounts> {
    let raw = std::fs::read_to_string(path).with_context(|| {
        format!(
            "baseline non trovata: {} (esegui `xtask quality-scan --update`)",
            path.display()
        )
    })?;
    let counts: QualityCounts = serde_json::from_str(&raw)
        .with_context(|| format!("baseline non valida: {}", path.display()))?;
    Ok(counts)
}

fn write_baseline(path: &Path, counts: &QualityCounts) -> Result<()> {
    let json = serde_json::to_string_pretty(counts)?;
    std::fs::write(path, format!("{json}\n"))
        .with_context(|| format!("scrittura baseline {}", path.display()))?;
    Ok(())
}

/// Restituisce l'exit code (0 = ok, 1 = regressione).
pub fn run(args: &[String]) -> Result<i32> {
    let opts = parse(args);

    // Export work-list: i file con finding-target, ordinati per priorita.
    // Indipendente da gate/update — serve al batch di refactor notturno.
    if let Some(path) = &opts.export {
        let targets = scan_targets(&opts.roots, opts.include_tests);
        let (mut tot_lf, mut tot_cx, mut tot_sec) = (0usize, 0usize, 0usize);
        for t in &targets {
            tot_lf += t.long_functions;
            tot_cx += t.complexity_high;
            tot_sec += t.security;
        }
        let json = serde_json::to_string_pretty(&targets)?;
        std::fs::write(path, format!("{json}\n"))
            .with_context(|| format!("scrittura export {path}"))?;
        println!(
            "xtask quality-scan: work-list esportata in {path}\n  file target: {}\n  long-fn: {} | complexity-high: {} | security: {}",
            targets.len(),
            tot_lf,
            tot_cx,
            tot_sec
        );
        return Ok(0);
    }

    let cwd = std::env::current_dir().context("cwd non leggibile")?;
    let baseline = baseline_path(opts.baseline.as_deref(), &opts.roots, &cwd)?;

    let current = scan_counts(&opts.roots, opts.include_tests);

    println!(
        "xtask quality-scan: {} file analizzati ({})",
        current.files_scanned,
        if opts.include_tests { "test inclusi" } else { "test esclusi" }
    );

    if opts.update {
        write_baseline(&baseline, &current)?;
        println!("Baseline aggiornata in {}:", baseline.display());
        print_counts("  ", &current);
        return Ok(0);
    }

    let base = load_baseline(&baseline)?;
    let metrics: [(&str, usize, usize); 4] = [
        ("findings totali", current.total, base.total),
        ("funzioni >50 righe", current.long_functions, base.long_functions),
        ("complessita >20", current.complexity_high, base.complexity_high),
        ("security", current.security, base.security),
    ];

    println!("  {:<22} {:>8} {:>8} {:>8}", "metrica", "attuale", "baseline", "delta");
    let mut regressions: Vec<String> = Vec::new();
    let mut improvements = 0usize;
    for (name, cur, b) in metrics {
        let delta = cur as i64 - b as i64;
        println!("  {name:<22} {cur:>8} {b:>8} {delta:>+8}");
        if cur > b {
            regressions.push(format!("{name}: {b} -> {cur} (+{})", cur - b));
        } else if cur < b {
            improvements += 1;
        }
    }

    if !regressions.is_empty() {
        eprintln!("\nxtask quality-scan: REGRESSIONE qualita (le metriche possono solo scendere):");
        for r in &regressions {
            eprintln!("  - {r}");
        }
        eprintln!("Riduci le violazioni introdotte, oppure se sono giustificate aggiorna la baseline con `xtask quality-scan --update`.");
        bail!("quality-scan gate fallito");
    }

    if improvements > 0 {
        println!(
            "\nQualita migliorata su {improvements} metrica/e: riallinea la baseline al ribasso con `xtask quality-scan --update`."
        );
    } else {
        println!("\nxtask quality-scan: nessuna regressione.");
    }
    Ok(0)
}

fn print_counts(prefix: &str, c: &QualityCounts) {
    println!("{prefix}findings totali   : {}", c.total);
    println!("{prefix}funzioni >50 righe: {}", c.long_functions);
    println!("{prefix}complessita >20   : {}", c.complexity_high);
    println!("{prefix}security          : {}", c.security);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Directory temporanea, rimossa a fine test. Il nome e' unico per processo
    /// e per chiamata: i test restano indipendenti dall'ordine e dalla
    /// concorrenza (regola F).
    struct TmpDir(PathBuf);

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl std::ops::Deref for TmpDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    fn fake_dir(label: &str) -> TmpDir {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "xtask-quality-scan-{}-{n}-{label}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir tmp");
        TmpDir(dir)
    }

    /// Albero di lavoro finto: il marker `.git` (un FILE, come nei worktree) e
    /// le directory che un root di scansione o la baseline possono abitare.
    fn fake_tree(label: &str) -> TmpDir {
        let dir = fake_dir(label);
        std::fs::create_dir_all(dir.join("scripts")).expect("mkdir scripts");
        std::fs::create_dir_all(dir.join("crates")).expect("mkdir crates");
        std::fs::write(dir.join(".git"), "gitdir: altrove\n").expect("marker .git");
        dir
    }

    fn baseline_of(tree: &Path) -> PathBuf {
        tree.join("scripts").join("quality-baseline.json")
    }

    /// Regressione: `--root <altro albero> --update` invocato da un'altra cwd
    /// scriveva la baseline della cwd, cioe' misurava un albero e riallineava
    /// quella di un altro.
    #[test]
    fn baseline_di_default_segue_l_albero_misurato_non_la_cwd() {
        let misurato = fake_tree("misurato");
        let cwd = fake_tree("cwd");

        let path = baseline_path(None, &[misurato.to_path_buf()], &cwd).expect("baseline risolta");

        assert_eq!(path, baseline_of(&misurato));
        assert_ne!(path, baseline_of(&cwd), "la baseline della cwd non va toccata");
    }

    /// Un root DENTRO l'albero misurato risale comunque alla sua radice.
    #[test]
    fn root_annidato_risale_alla_radice_dell_albero() {
        let misurato = fake_tree("annidato");
        let cwd = fake_tree("cwd");

        let path = baseline_path(None, &[misurato.join("crates")], &cwd).expect("baseline risolta");

        assert_eq!(path, baseline_of(&misurato));
    }

    /// Caso di `scripts/quality-scan.sh`, che fa `cd` nella radice e usa il
    /// root relativo di default: il comportamento storico non cambia.
    #[test]
    fn root_relativo_resta_ancorato_all_albero_della_cwd() {
        let tree = fake_tree("relativo");

        let path = baseline_path(None, &[PathBuf::from("crates")], &tree).expect("baseline risolta");

        assert_eq!(path, baseline_of(&tree));
    }

    /// `--baseline` esplicito e' un'istruzione dell'utente: si rispetta, e se
    /// relativo si intende dalla cwd come qualunque path da riga di comando.
    #[test]
    fn baseline_esplicita_ha_la_precedenza_sull_albero_misurato() {
        let misurato = fake_tree("esplicita-misurato");
        let cwd = fake_tree("esplicita-cwd");

        let path = baseline_path(Some("altra/baseline.json"), &[misurato.to_path_buf()], &cwd)
            .expect("baseline risolta");

        assert_eq!(path, cwd.join("altra").join("baseline.json"));
    }

    /// Root in alberi diversi: nessuna baseline e' "quella giusta", e sceglierne
    /// una in silenzio ne falserebbe una delle due.
    #[test]
    fn root_in_alberi_diversi_e_un_errore_esplicito() {
        let a = fake_tree("ambiguo-a");
        let b = fake_tree("ambiguo-b");
        let cwd = fake_tree("ambiguo-cwd");

        let err =
            baseline_path(None, &[a.to_path_buf(), b.to_path_buf()], &cwd).expect_err("deve fallire");

        assert!(err.to_string().contains("ambigua"), "messaggio: {err}");
    }

    /// Piu' root nello STESSO albero restano una sola baseline.
    #[test]
    fn root_multipli_nello_stesso_albero_non_sono_ambigui() {
        let tree = fake_tree("multi-root");
        let cwd = fake_tree("multi-cwd");

        let path = baseline_path(None, &[tree.join("crates"), tree.join("scripts")], &cwd)
            .expect("baseline risolta");

        assert_eq!(path, baseline_of(&tree));
    }

    /// Directory fuori da qualunque albero di lavoro: non ha una baseline
    /// propria, resta quella della cwd (comportamento storico).
    #[test]
    fn root_fuori_da_un_albero_di_lavoro_ricade_sulla_cwd() {
        let cwd = fake_tree("fuori-cwd");
        let orfana = fake_dir("orfana");

        let path = baseline_path(None, &[orfana.to_path_buf()], &cwd).expect("baseline risolta");

        assert_eq!(path, baseline_of(&cwd));
    }
}
