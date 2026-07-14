//! Prova di EFFICACIA di uno step gate: il gate sa fallire? (regola H+M)
//!
//! # Perche' esiste
//!
//! Il sistema misurava sempre lo STATO dell'albero (exit code, baseline) e mai
//! il POTERE DISCRIMINANTE del criterio. L'autorita' "questo comando e' una
//! verifica" nasceva da una DICHIARAZIONE: il flag `gate` e' un booleano scritto
//! dall'LLM (`verify_profile.rs`) e `validate_steps` valida solo nome, comando
//! non vuoto e safety — cioe' "e' distruttivo?", mai "e' capace di fallire?".
//!
//! Caso reale (Beaty-Book), provato sul campo:
//! ```text
//! node --check rotto.js          -> exit 1
//! node --check sano.js rotto.js  -> exit 0   <- gli argomenti oltre il primo sono IGNORATI
//! ```
//! Lo step gate `node --check backend/server.js backend/middleware/**/*.js ...`
//! usciva 0 SEMPRE: il backend non e' mai stato verificato da nessun run, e il
//! gate lo dichiarava verde. La baseline delta-aware non salva, perche' e' fatta
//! per il DEBITO (exit non-zero stabile), non per l'INERZIA (exit zero stabile):
//! baseline 0 == post-lavoro 0 -> verde. **Un gate che passa SEMPRE e'
//! indistinguibile da un gate che passa perche' il codice e' sano.**
//!
//! # Il principio
//!
//! Niente blacklist di pattern (`node --check`, `|| true`, ...): sarebbe una
//! toppa (regola H) e coprirebbe solo i casi gia' visti. Si fa un ESPERIMENTO:
//! si introduce una rottura NOTA in un punto che il comando dichiara di coprire
//! e si guarda se il comando arrossisce. Un solo esperimento cattura in un colpo
//! glob non espansi, argomenti ignorati, `|| true`, exit-code bugiardi e comandi
//! che stampano errori uscendo 0.
//!
//! # Sicurezza: nessun file del progetto viene toccato
//!
//! Il probe NON muta i sorgenti esistenti. Pianta un file sintetico usa-e-getta
//! dentro una directory che un glob del comando dichiara di coprire, misura, e
//! lo rimuove subito. Se il processo muore nel mezzo resta al piu' un file dal
//! nome riconoscibile ([`PROBE_FILE_STEM`]), mai un sorgente dell'utente
//! danneggiato. Costo: una esecuzione extra del comando per target, una tantum
//! (l'esito si persiste nel profilo come la baseline).

use serde::{Deserialize, Serialize};

/// Prefisso del file sintetico: riconoscibile a colpo d'occhio se un crash ne
/// lascia uno indietro, e improbabile in un progetto reale.
pub const PROBE_FILE_STEM: &str = "__nexus_probe_gate__";

/// Contenuto sintatticamente invalido per qualunque linguaggio di codice: la
/// sequenza non e' un token valido in JS/TS/Rust/Python/Go/Java. Se il comando
/// legge questo file e NON arrossisce, non sta guardando quel file.
const BROKEN_SOURCE: &str = "@@@ NEXUS PROBE: SYNTAX ERROR ATTESO @@@\n";

/// Estensioni per cui un file sintetico e' un sorgente plausibile (e quindi
/// verrebbe raccolto da un glob `*.ext`).
const PROBEABLE_EXTS: &[&str] = &[
    "js", "jsx", "mjs", "cjs", "ts", "tsx", "rs", "py", "go", "java", "rb", "php", "c", "cc",
    "cpp", "h", "hpp", "cs", "swift", "kt",
];

/// Esito della prova: segnale STRUTTURATO (regola M), mai prosa da interpretare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeOutcome {
    /// Una rottura nota fa arrossire il comando: lo step SA fallire ed e' un
    /// gate vero.
    Discriminating,
    /// La rottura NON cambia l'esito: lo step e' CIECO su cio' che dichiara di
    /// coprire. Non e' una verifica, qualunque cosa dica il suo nome.
    Blind,
    /// Non provabile con questo meccanismo (nessun glob di sorgenti fra gli
    /// argomenti, es. `pnpm build`). **Non e' un giudizio**: lo step resta com'e'.
    /// Dichiararlo cieco sarebbe un falso positivo.
    NotProbed,
}

/// Un punto in cui il probe puo' piantare il file sintetico: la directory
/// dichiarata da un glob del comando, con l'estensione che il glob raccoglie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeTarget {
    /// Directory relativa alla root (`""` = la root stessa).
    pub dir: String,
    /// Estensione senza punto (`js`, `ts`, ...).
    pub ext: String,
}

/// PURA: dai token del comando ricava i punti in cui piantare il file
/// sintetico. Nessun IO, cosi' la parte che decide si testa senza toccare disco.
///
/// Riconosce i glob di sorgenti (`src/**/*.ts`, `backend/middleware/*.js`) —
/// cioe' proprio il costrutto in cui i file "oltre il primo" spariscono. I path
/// espliciti (`backend/server.js`) non sono target: non si puo' inventare un
/// file che il comando guardi gia' per nome.
pub fn probe_targets(command: &str) -> Vec<ProbeTarget> {
    let mut out: Vec<ProbeTarget> = Vec::new();
    for raw in command.split_whitespace() {
        let tok = raw.trim_matches(|c| c == '"' || c == '\'');
        // Flag e opzioni: non sono path.
        if tok.starts_with('-') {
            continue;
        }
        if !tok.contains('*') {
            continue;
        }
        // Interessa solo il glob che raccoglie per estensione: `*.js`.
        let Some((prefix, ext)) = tok.rsplit_once("*.") else {
            continue;
        };
        let ext = ext.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if !PROBEABLE_EXTS.contains(&ext) {
            continue;
        }
        // Il prefisso e' la parte di path prima del glob: `backend/middleware/**/`
        // -> `backend/middleware`. `**` e `*` non sono directory reali.
        let dir = prefix
            .replace('\\', "/")
            .split('/')
            .filter(|seg| !seg.is_empty() && !seg.contains('*'))
            .collect::<Vec<_>>()
            .join("/");
        let t = ProbeTarget {
            dir,
            ext: ext.to_string(),
        };
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Il file sintetico per un target, relativo alla root.
pub fn probe_file_rel(t: &ProbeTarget) -> String {
    if t.dir.is_empty() {
        format!("{PROBE_FILE_STEM}.{}", t.ext)
    } else {
        format!("{}/{}.{}", t.dir, PROBE_FILE_STEM, t.ext)
    }
}

/// Verdetto PURO sulla singola misura: la rottura ha cambiato l'esito?
///
/// `baseline` = exit sull'albero sano; `probed` = exit con il file rotto
/// piantato. `None` (comando non misurabile) -> non si giudica.
pub fn verdict_from_exits(baseline: Option<i64>, probed: Option<i64>) -> ProbeOutcome {
    match (baseline, probed) {
        (Some(b), Some(p)) if b != p => ProbeOutcome::Discriminating,
        (Some(_), Some(_)) => ProbeOutcome::Blind,
        _ => ProbeOutcome::NotProbed,
    }
}

/// Esegue la prova per uno step, su ogni target dichiarato dal comando.
///
/// Semantica: il gate deve coprire TUTTO cio' che dichiara. Basta UN target
/// cieco perche' lo step abbia un buco -> [`ProbeOutcome::Blind`]. Serve che
/// TUTTI i target arrossiscano per [`ProbeOutcome::Discriminating`].
///
/// `measure` e' il punto unico di misura dell'exit code (regola L): lo stesso
/// runner della baseline, cosi' la prova e il gate misurano allo stesso modo.
pub async fn probe_step<F, Fut>(
    root: &std::path::Path,
    command: &str,
    baseline_exit: Option<i64>,
    mut measure: F,
) -> ProbeOutcome
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<i64>>,
{
    let targets = probe_targets(command);
    if targets.is_empty() || baseline_exit.is_none() {
        return ProbeOutcome::NotProbed;
    }
    let mut any_probed = false;
    for t in &targets {
        let rel = probe_file_rel(t);
        let path = root.join(&rel);
        let Some(parent) = path.parent() else {
            continue;
        };
        if !parent.is_dir() {
            // La directory dichiarata dal glob non esiste: niente da provare qui.
            continue;
        }
        if std::fs::write(&path, BROKEN_SOURCE).is_err() {
            continue;
        }
        let probed = measure().await;
        // Ripristino IMMEDIATO e incondizionato: il file sintetico non deve
        // sopravvivere alla misura, qualunque cosa essa abbia risposto.
        let _ = std::fs::remove_file(&path);

        match verdict_from_exits(baseline_exit, probed) {
            ProbeOutcome::Blind => {
                tracing::warn!(
                    probe_file = %rel,
                    baseline_exit = ?baseline_exit,
                    probed_exit = ?probed,
                    "verify_probe: gate CIECO — una rottura nota non lo fa fallire"
                );
                return ProbeOutcome::Blind;
            }
            ProbeOutcome::Discriminating => any_probed = true,
            ProbeOutcome::NotProbed => {}
        }
    }
    if any_probed {
        ProbeOutcome::Discriminating
    } else {
        ProbeOutcome::NotProbed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn riconosce_i_glob_di_sorgenti() {
        // Il comando del caso reale (Beaty-Book).
        let t = probe_targets("node --check backend/server.js backend/middleware/**/*.js backend/generate-hash.js");
        assert_eq!(
            t,
            vec![ProbeTarget {
                dir: "backend/middleware".into(),
                ext: "js".into()
            }],
            "il glob e' l'unico punto in cui si puo' piantare un file"
        );
        assert_eq!(probe_file_rel(&t[0]), "backend/middleware/__nexus_probe_gate__.js");
    }

    #[test]
    fn ignora_flag_path_espliciti_e_estensioni_non_sorgente() {
        assert!(probe_targets("node --check a.js b.js").is_empty(), "path espliciti: nessun glob");
        assert!(probe_targets("pnpm build").is_empty());
        assert!(probe_targets("npx tsc -p .").is_empty());
        assert!(probe_targets("cat logs/*.json").is_empty(), "json non e' codice sorgente");
        assert_eq!(
            probe_targets("npx eslint src/**/*.ts")[0],
            ProbeTarget { dir: "src".into(), ext: "ts".into() }
        );
    }

    #[test]
    fn glob_nella_root_ha_dir_vuota() {
        let t = probe_targets("node --check *.js");
        assert_eq!(t[0].dir, "");
        assert_eq!(probe_file_rel(&t[0]), "__nexus_probe_gate__.js");
    }

    /// Il cuore: exit invariato = il comando non ha visto la rottura.
    #[test]
    fn il_verdetto_e_il_confronto_degli_exit() {
        assert_eq!(verdict_from_exits(Some(0), Some(1)), ProbeOutcome::Discriminating);
        assert_eq!(verdict_from_exits(Some(0), Some(0)), ProbeOutcome::Blind, "il caso node --check multi-file");
        assert_eq!(verdict_from_exits(Some(2), Some(2)), ProbeOutcome::Blind, "cieco anche con debito pre-esistente");
        assert_eq!(verdict_from_exits(Some(2), Some(1)), ProbeOutcome::Discriminating);
        // Non misurabile -> nessun giudizio (mai un falso "cieco").
        assert_eq!(verdict_from_exits(None, Some(0)), ProbeOutcome::NotProbed);
        assert_eq!(verdict_from_exits(Some(0), None), ProbeOutcome::NotProbed);
    }

    /// Prova END-TO-END con IO vero (niente rete): riproduce il difetto reale.
    /// Il comando e' un gate finto che ignora gli argomenti oltre il primo,
    /// esattamente come `node --check`.
    #[tokio::test]
    async fn probe_step_smaschera_un_gate_cieco_e_promuove_uno_vero() {
        let tmp = std::env::temp_dir().join(format!("nexus_probe_test_{}", std::process::id()));
        let sub = tmp.join("src");
        std::fs::create_dir_all(&sub).expect("tmpdir");

        // GATE CIECO: qualunque cosa accada, exit 0 (baseline 0 == probed 0).
        let cieco = probe_step(&tmp, "finto --check src/**/*.js", Some(0), || async { Some(0) }).await;
        assert_eq!(cieco, ProbeOutcome::Blind);

        // GATE VERO: si accorge del file sintetico -> exit 1.
        let vero = probe_step(&tmp, "finto --check src/**/*.js", Some(0), || async {
            Some(1)
        })
        .await;
        assert_eq!(vero, ProbeOutcome::Discriminating);

        // Il file sintetico non sopravvive MAI alla misura.
        assert!(
            !sub.join(format!("{PROBE_FILE_STEM}.js")).exists(),
            "il probe ha lasciato sporcizia nel progetto"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Senza glob non si giudica: un `pnpm build` non provabile NON e' cieco.
    #[tokio::test]
    async fn senza_target_non_si_giudica() {
        let tmp = std::env::temp_dir();
        let out = probe_step(&tmp, "pnpm build", Some(0), || async { Some(0) }).await;
        assert_eq!(out, ProbeOutcome::NotProbed);
    }
}
