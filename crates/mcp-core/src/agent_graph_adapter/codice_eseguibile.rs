//! Confine I/O del punto unico
//! [`nexus_agent_graph::decisions::codice_eseguibile`]: porta i FATTI con cui si
//! risponde a «i file di codice che questo run ha PRODOTTO si caricano?», e non
//! ne giudica nessuno.
//!
//! I fatti nascono da tre letture, tutte fuori dal criterio:
//!   - le SCRITTURE, dal registro `file_mutations` (DB META, mig 0349), scritte
//!     da [`crate::file_mutations::record_mutation`] con un percorso gia'
//!     relativo alla radice del run — la stessa fonte che
//!     [`super::mutation_progress`] usa per il progresso di una correzione e
//!     [`super::pagina_del_run`] per la pagina da misurare;
//!   - l'ESISTENZA del file sull'albero al momento della verifica: una scrittura
//!     resta un fatto anche quando il file non c'e' piu';
//!   - l'ESITO del comando di prova, che e' il codice d'uscita del runtime
//!     (segnale strutturato, regola M) piu' il suo messaggio, che serve solo a
//!     chi dovra' correggere.
//!
//! QUALI file, e con quale comando, NON si decide qui: lo decide il criterio
//! ([`codice_eseguibile::pianifica_prova`]) sul vocabolario che il DB dichiara.
//! Un filtro scritto anche in SQL divergerebbe dal criterio al primo linguaggio
//! aggiunto, e senza che nulla fallisca (regola L).
//!
//! PERIMETRO: la SESSIONE, non il solo run — stessa scelta, e stessa ragione,
//! delle altre due porte che leggono questo registro: le scritture di un sub-run
//! portano il `run_id` del figlio e la `session_id` del padre, quindi cercare
//! sotto il solo run del padre renderebbe invisibile tutto il lavoro DELEGATO.
//! Il codice scritto da un coder convocato e' codice che questo run ha prodotto.
//!
//! IL MOMENTO E' LA VERIFICA e non t=0, per la ragione gia' misurata sulla
//! pagina: a t=0 il run non ha ancora scritto niente, e i file da provare sono
//! esattamente quelli che nasceranno dopo.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use sqlx::{PgPool, Row};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use uuid::Uuid;

use nexus_agent_graph::decisions::codice_eseguibile::{
    self, CausaNonProvato, EsitoFile, FattoFile, PassoProva, PianoProva, VocabolarioRuntime,
};

/// Tetto di righe lette dal registro. Non e' il tetto dei file PROVATI (quello
/// e' configurazione, `max_file`): e' il tetto della lettura, generoso perche'
/// una sessione lunga scrive molte volte gli stessi file e la deduplica avviene
/// dopo. Si prendono le piu' RECENTI: lo stato che conta e' l'ultimo.
const MAX_SCRITTURE: i64 = 2000;

/// Caratteri di messaggio d'errore conservati per file. Il messaggio del runtime
/// va all'agente perche' e' l'unica cosa con cui puo' correggere; la coda di uno
/// stack trace non aggiunge nulla e gonfia l'evidenza persistita.
const MAX_CAUSA_CHARS: usize = 800;

/// Margine sul timeout del singolo comando, oltre il quale il processo si
/// uccide. Un runtime che non esce lascerebbe un processo orfano a ogni
/// invocazione del gate.
const MARGINE_KILL_S: u64 = 5;

/// Raccoglie i fatti per
/// [`codice_eseguibile::classifica_esecuzione`].
///
/// `Err` = non si e' potuto leggere il REGISTRO. Non si degrada a «nessun file
/// scritto»: quel ripiego direbbe «niente da provare» a un run che ha prodotto
/// codice, cioe' rimetterebbe in piedi in silenzio il difetto che il criterio
/// chiude. Chi chiama lo DICHIARA inconcludente.
pub async fn fatti_codice(
    meta_db: &PgPool,
    project_id: Uuid,
    session_id: Uuid,
    root: &Path,
    voc: &VocabolarioRuntime,
    max_file: usize,
    timeout_s: f64,
) -> Result<Vec<FattoFile>, String> {
    let scritti = percorsi_scritti(meta_db, project_id, session_id, root).await?;
    let mut fatti = Vec::with_capacity(scritti.len());
    let mut budget = max_file;
    for rel in scritti {
        let esito = esito_del_file(root, &rel, voc, &mut budget, timeout_s).await;
        fatti.push(FattoFile { path: rel, esito });
    }
    Ok(fatti)
}

/// Cosa si accerta di UN file prodotto: il piano dal vocabolario, il tetto,
/// l'esistenza sull'albero, e solo alla fine i comandi.
///
/// `budget` scala sui soli file EFFETTIVAMENTE provati: contarlo su tutti gli
/// scritti lo farebbe esaurire dai `.md`, cioe' proprio dove non costa niente.
/// L'ordine dei tre controlli e' load-bearing — un `.md` oltre il tetto e' fuori
/// vocabolario, non «escluso dal tetto», e dirlo al contrario manderebbe a
/// cercare un tetto stretto dove il file non era codice.
async fn esito_del_file(
    root: &Path,
    rel: &str,
    voc: &VocabolarioRuntime,
    budget: &mut usize,
    timeout_s: f64,
) -> EsitoFile {
    let passi = match codice_eseguibile::pianifica_prova(rel, voc) {
        PianoProva::NonProvabile { causa } => return EsitoFile::NonProvato { causa },
        PianoProva::Prova { passi } => passi,
    };
    if *budget == 0 {
        return EsitoFile::NonProvato {
            causa: CausaNonProvato::OltreIlTetto,
        };
    }
    // Una scrittura resta un fatto anche quando il file non c'e' piu' (spostato,
    // rimosso da un comando che il registro non vede): non c'e' codice da
    // provare, e non e' un difetto.
    if !root.join(rel).is_file() {
        return EsitoFile::NonProvato {
            causa: CausaNonProvato::FileAssente,
        };
    }
    *budget -= 1;
    prova_file(root, rel, &passi, timeout_s).await
}

/// I percorsi RELATIVI scritti nella sessione, deduplicati e confinati alla
/// radice, nell'ordine in cui sono stati scritti.
///
/// La deduplica sta qui e non nel criterio: un file riscritto dieci volte e' UN
/// file da provare, e provarlo dieci volte darebbe dieci fatti identici piu'
/// dieci processi. E' una proprieta' della fonte (il registro e' append-only per
/// scrittura), non una regola di giudizio.
///
/// Il confinamento delega al punto unico
/// [`nexus_types::workspace_paths::normalize_into_root`]: un percorso che esce
/// dalla radice non e' un file di questo progetto, e provarlo eseguirebbe un
/// runtime su un file che nessuno ha dichiarato (regola E).
async fn percorsi_scritti(
    meta_db: &PgPool,
    project_id: Uuid,
    session_id: Uuid,
    root: &Path,
) -> Result<Vec<String>, String> {
    let righe = sqlx::query(
        "SELECT file_path, op \
           FROM file_mutations \
          WHERE project_id = $1 AND session_id = $2 \
          ORDER BY id DESC \
          LIMIT $3",
    )
    .bind(project_id)
    .bind(session_id)
    .bind(MAX_SCRITTURE)
    .fetch_all(meta_db)
    .await
    .map_err(|e| format!("registro delle scritture non interrogabile: {e}"))?;

    let mut visti: HashSet<String> = HashSet::new();
    let mut elenco: Vec<String> = Vec::new();
    // Dalla piu' recente alla piu' vecchia: e' l'ULTIMA operazione su un file a
    // dire se oggi c'e'. Il risultato si rimette poi in ordine di scrittura.
    for r in righe {
        let Ok(path) = r.try_get::<String, _>("file_path") else {
            continue;
        };
        let op: String = r.try_get("op").unwrap_or_default();
        let Ok(rel) = nexus_types::workspace_paths::normalize_into_root(root, &path) else {
            continue;
        };
        if rel.is_empty() || !visti.insert(rel.clone()) {
            continue;
        }
        // Una cancellazione e' l'ultima parola su quel file: non e' un file
        // sparito per caso, e' un file che il run ha voluto togliere. Non
        // produce alcun fatto — non c'e' codice da provare ne' niente da
        // dichiarare all'agente.
        if op == crate::file_mutations::OP_CANCELLATO {
            continue;
        }
        elenco.push(rel);
    }
    elenco.reverse();
    Ok(elenco)
}

/// Esegue i passi del piano IN ORDINE e traduce il primo rifiuto in un esito.
///
/// La radice e' la working dir del processo, quindi il file si passa RELATIVO:
/// e' il modo in cui gli import relativi del modulo si risolvono come si
/// risolverebbero eseguendolo davvero (regola O). Resta la resa per un processo
/// esterno sulla radice, che su Windows puo' arrivare in forma verbatim
/// (`\\?\D:\...`) — forma che le API del filesystem accettano e un processo no.
async fn prova_file(root: &Path, rel: &str, passi: &[PassoProva], timeout_s: f64) -> EsitoFile {
    let mut ultimo = None;
    for passo in passi {
        match esegui_passo(root, rel, passo, timeout_s).await {
            Ok((0, _)) => {
                ultimo = Some(passo.livello);
            }
            Ok((code, messaggio)) => {
                return EsitoFile::NonCaricato {
                    livello: passo.livello,
                    exit_code: Some(code),
                    causa: tronca(&messaggio),
                }
            }
            // Il runtime non e' partito o non ha risposto: NON e' un difetto del
            // codice, ed e' l'unico caso in cui il criterio dichiara di non aver
            // guardato invece di assolvere.
            Err(dettaglio) => {
                return EsitoFile::NonProvato {
                    causa: CausaNonProvato::RuntimeNonDisponibile { dettaglio },
                }
            }
        }
    }
    match ultimo {
        Some(livello) => EsitoFile::Caricato { livello },
        // Piano senza passi: non e' rappresentabile oggi (`pianifica_prova` ne
        // mette sempre almeno uno) e non si assolve per costruzione.
        None => EsitoFile::NonProvato {
            causa: CausaNonProvato::VocabolarioNonEseguibile {
                dettaglio: "piano di prova senza passi".to_string(),
            },
        },
    }
}

/// Esegue UN comando di prova. `Ok((exit_code, messaggio))` = il runtime ha
/// risposto; `Err` = non e' partito o non ha risposto entro il tempo.
///
/// Il programma si invoca DIRETTAMENTE e non attraverso una shell: la riga di
/// vocabolario e' gia' scomposta dal punto unico, e passare da una shell
/// riaprirebbe la quotatura di un percorso che puo' contenere spazi.
async fn esegui_passo(
    root: &Path,
    rel: &str,
    passo: &PassoProva,
    timeout_s: f64,
) -> Result<(i32, String), String> {
    let cwd: PathBuf = nexus_types::workspace_paths::path_per_processo_esterno(root).into();
    let mut cmd = Command::new(&passo.programma);
    cmd.args(&passo.argomenti)
        .arg(rel)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        format!(
            "avvio '{}' fallito ({e}): runtime non disponibile",
            passo.programma
        )
    })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let budget = Duration::from_secs_f64(timeout_s.max(1.0))
        .saturating_add(Duration::from_secs(MARGINE_KILL_S));
    let stato = match tokio::time::timeout(budget, child.wait()).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(format!("attesa di '{}' fallita: {e}", passo.programma)),
        Err(_) => {
            let _ = child.start_kill();
            return Err(format!(
                "'{}' non ha risposto entro {}s",
                passo.programma,
                budget.as_secs()
            ));
        }
    };
    // Un processo terminato da un SEGNALE non ha exit code: e' un
    // «non ha risposto», non un rifiuto del file.
    let Some(code) = stato.code() else {
        return Err(format!(
            "'{}' terminato senza codice d'uscita",
            passo.programma
        ));
    };
    Ok((code, messaggio_del_processo(stdout, stderr).await))
}

/// Il messaggio del runtime: stderr per primo, che e' dove i runtime scrivono
/// l'errore, e stdout come contorno quando stderr tace.
async fn messaggio_del_processo(
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
) -> String {
    let mut out = String::new();
    if let Some(mut s) = stdout {
        let _ = s.read_to_string(&mut out).await;
    }
    let mut err = String::new();
    if let Some(mut s) = stderr {
        let _ = s.read_to_string(&mut err).await;
    }
    match (err.trim(), out.trim()) {
        ("", o) => o.to_string(),
        (e, "") => e.to_string(),
        (e, o) => format!("{e}\n{o}"),
    }
}

/// Taglia il messaggio del runtime conservando la TESTA: la prima riga porta il
/// tipo dell'errore e la posizione, la coda e' stack.
fn tronca(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= MAX_CAUSA_CHARS {
        return s.to_string();
    }
    let testa: String = s.chars().take(MAX_CAUSA_CHARS).collect();
    format!("{testa}[...]")
}

#[cfg(test)]
mod tests {
    use super::*;

    use nexus_agent_graph::decisions::codice_eseguibile::{
        classifica_esecuzione, LivelloProva, VerdettoEsecuzione,
    };

    use crate::file_mutations::{record_mutation, ScopeAudit};

    /// Il vocabolario dei test e' quello della MIGRAZIONE, non una copia scritta
    /// a mano: se la riga del DB smettesse di provare i file di test, questi
    /// test rosseggerebbero invece di misurare un vocabolario che non esiste
    /// (regola O). Lo legge dal file della migrazione, che e' la fonte.
    fn vocabolario_dalla_migrazione() -> VocabolarioRuntime {
        let sql = include_str!("../../../../db/migrations/0734_codice_eseguibile_nel_gate.sql");
        let inizio = sql
            .find(MARCATORE_INIZIO)
            .map(|p| p + MARCATORE_INIZIO.len())
            .expect("marcatore di inizio del vocabolario");
        let fine = sql[inizio..]
            .find(MARCATORE_FINE)
            .map(|p| inizio + p)
            .expect("marcatore di fine del vocabolario");
        // Fra i marcatori c'e' il LETTERALE SQL: si tolgono gli apici che lo
        // delimitano e si scioglie il raddoppio dell'apice, cosi' cio' che si
        // legge e' esattamente cio' che Postgres scrivera' in `settings.value`.
        let letterale = sql[inizio..fine].trim();
        let json = letterale
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .expect("il blocco fra i marcatori e' un letterale SQL fra apici")
            .replace("''", "'");
        VocabolarioRuntime::parse(&json).expect("il vocabolario della migrazione e' valido")
    }

    const MARCATORE_INIZIO: &str = "-- <<vocabolario>>";
    const MARCATORE_FINE: &str = "-- <</vocabolario>>";

    /// Il vocabolario che i test attraversano e' quello della MIGRAZIONE, e la
    /// migrazione dichiara cio' che serve al caso misurato. Senza questa
    /// asserzione, togliere `carica_test` dalla riga `js` renderebbe il criterio
    /// cieco sul difetto che lo ha fatto nascere e i test di sopra
    /// rosseggerebbero senza dire perche'.
    #[test]
    fn la_migrazione_dichiara_il_livello_di_caricamento_per_javascript() {
        let voc = vocabolario_dalla_migrazione();
        let js = voc.estensioni.get("js").expect("la riga js esiste");
        assert_eq!(js.carica, "node --check");
        let test = js
            .carica_test
            .as_deref()
            .expect("javascript dichiara il livello di caricamento");
        assert!(
            test.contains("--test-name-pattern="),
            "senza il filtro di nome il livello ESEGUE i test, e il criterio \
             boccerebbe un assert rosso invadendo suite_verification: {test}"
        );
        assert!(
            voc.e_un_test("calcolatrice.test.js"),
            "i marcatori della migrazione riconoscono il file del caso misurato"
        );
    }

    /// Scrive il file sull'albero E la riga nel registro, come fanno i tool di
    /// scrittura (`tool_write_file` chiama `record_mutation` subito prima di
    /// sovrascrivere). Il produttore e' quello di produzione: l'`op` non e' una
    /// costante del test, la deriva `record_mutation` dai contenuti (regola O).
    async fn scrivi(
        pool: &PgPool,
        project_id: Uuid,
        session_id: Uuid,
        user_id: Uuid,
        run_id: Option<Uuid>,
        root: &Path,
        rel: &str,
        contenuto: &str,
    ) {
        let assoluto = root.join(rel);
        if let Some(genitore) = assoluto.parent() {
            std::fs::create_dir_all(genitore).expect("mkdir");
        }
        let prima = std::fs::read_to_string(&assoluto).ok();
        std::fs::write(&assoluto, contenuto).expect("write");
        record_mutation(
            pool,
            project_id,
            Some(session_id),
            run_id,
            Some(user_id),
            rel,
            "write_file",
            prima.as_deref(),
            Some(contenuto),
            ScopeAudit::none(),
        )
        .await
        .expect("mutazione registrata");
    }

    /// La sorgente e i due file di test del caso misurato: quello Jest (che non
    /// si carica) e la sua riscrittura con `node:test` (che si carica, e ha un
    /// test che FALLISCE apposta).
    const CALCOLATRICE: &str = "function somma(a, b) { return a + b; }\n\
                                function dividi(a, b) { if (b === 0) throw new Error('div0'); return a / b; }\n\
                                module.exports = { somma, dividi };\n";
    const TEST_JEST: &str = "const { somma } = require('./calcolatrice.js');\n\
                             describe('somma', () => { it('funziona', () => { expect(somma(2, 3)).toBe(5); }); });\n";
    const TEST_NODE: &str = "const test = require('node:test');\n\
                             const assert = require('node:assert');\n\
                             const { somma } = require('./calcolatrice.js');\n\
                             test('somma sbagliata apposta', () => { assert.strictEqual(somma(2, 3), 99); });\n";

    /// IL CASO MISURATO, end-to-end sui FATTI reali e coi comandi VERI (regola
    /// O): registro -> piano dal vocabolario della migrazione -> `node`
    /// eseguito davvero -> verdetto.
    ///
    /// Il run del 17/08/2026 aveva esattamente questi due file:
    /// `calcolatrice.js` funzionante e `calcolatrice.test.js` con sintassi Jest
    /// in un progetto senza Jest. Il gate ha chiuso «passato» due volte.
    ///
    /// MUTAZIONE: togliere `carica_test` dalla riga `js` del vocabolario (cioe'
    /// fermarsi a `node --check`, che quel file lo PASSA) riporta il verdetto a
    /// `CodiceCaricabile` e questo test rosseggia — e' il gate cieco di prima.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_test_jest_senza_jest_non_si_carica(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let session_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let albero = tempfile::tempdir().expect("tempdir");
        let root = albero.path();
        let voc = vocabolario_dalla_migrazione();

        scrivi(
            &pool, project_id, session_id, user_id, Some(run_id), root,
            "calcolatrice.js", CALCOLATRICE,
        )
        .await;
        scrivi(
            &pool, project_id, session_id, user_id, Some(run_id), root,
            "calcolatrice.test.js", TEST_JEST,
        )
        .await;

        let fatti = fatti_codice(&pool, project_id, session_id, root, &voc, 50, 30.0)
            .await
            .expect("fatti");
        assert_eq!(fatti.len(), 2, "due file scritti, due fatti");

        let sorgente = fatti.iter().find(|f| f.path == "calcolatrice.js").unwrap();
        assert_eq!(
            sorgente.esito,
            EsitoFile::Caricato {
                livello: LivelloProva::Sintassi
            },
            "la sorgente funziona, e il criterio non ha altro da chiederle"
        );

        let verdetto = classifica_esecuzione(&fatti);
        let VerdettoEsecuzione::CodiceRotto { rotti } = &verdetto else {
            panic!("atteso CodiceRotto, ottenuto {verdetto:?} su {fatti:?}");
        };
        assert_eq!(rotti.len(), 1);
        assert_eq!(rotti[0].path, "calcolatrice.test.js");
        let EsitoFile::NonCaricato {
            livello, causa, ..
        } = &rotti[0].esito
        else {
            panic!("atteso NonCaricato");
        };
        assert_eq!(
            *livello,
            LivelloProva::Caricamento,
            "il primo livello (node --check) questo file lo PASSA: e' il secondo a vederlo"
        );
        assert!(
            causa.contains("describe is not defined"),
            "la causa e' il messaggio del RUNTIME, non un giudizio composto: {causa}"
        );
    }

    /// I DUE FILE VERI del run del 17/08/2026, byte per byte.
    ///
    /// Le fixture di sopra sono riduzioni scritte a mano: dicono che il criterio
    /// riconosce *un* file con la sintassi Jest. Queste sono l'ARTEFATTO — i 320
    /// e 625 byte che il run ha davvero prodotto sul progetto
    /// `audit-verifica-17-08`, righe 6519 e 6520 di `file_mutations` nel DB META.
    ///
    /// Non si possono leggere dall'albero del progetto, e la ragione e' essa
    /// stessa una misura: alle 15:12:15 UTC — 32 minuti dopo la creazione — la
    /// riga 6521 ha RISCRITTO `calcolatrice.test.js` con `node:test`. Sul disco
    /// oggi c'e' quella versione, che si carica e supera tutti e cinque i casi
    /// (verificato: `node --test` esce 0, 5 pass). Il file rotto sopravvive solo
    /// nel registro delle scritture, ed e' da li' che queste due costanti
    /// vengono.
    const CALCOLATRICE_REALE: &str = r#"function somma(a, b) {
  return a + b;
}

function sottrai(a, b) {
  return a - b;
}

function moltiplica(a, b) {
  return a * b;
}

function dividi(a, b) {
  if (b === 0) {
    throw new Error("Divisione per zero non consentita");
  }
  return a / b;
}

module.exports = {
  somma,
  sottrai,
  moltiplica,
  dividi
};
"#;

    const TEST_JEST_REALE: &str = r#"const { somma, sottrai, moltiplica, dividi } = require('./calcolatrice');

describe('Calcolatrice', () => {
  test('somma due numeri', () => {
    expect(somma(2, 3)).toBe(5);
  });

  test('sottrae due numeri', () => {
    expect(somma ? sottrai(5, 2) : 0).toBe(3);
    expect(sottrai(5, 2)).toBe(3);
  });

  test('moltiplica due numeri', () => {
    expect(moltiplica(4, 3)).toBe(12);
  });

  test('divide due numeri', () => {
    expect(dividi(10, 2)).toBe(5);
  });

  test('lancia un errore in caso di divisione per zero', () => {
    expect(() => dividi(5, 0)).toThrow("Divisione per zero non consentita");
  });
});
"#;

    /// Le impronte che il registro porta per quelle due righe. Servono a
    /// DIMOSTRARE che le fixture sono l'artefatto invece di affermarlo: chi le
    /// ritoccasse «per renderle piu' leggibili» starebbe misurando un altro
    /// caso, e il test glielo dice. Si ricalcolano con la funzione che ha
    /// scritto `after_sha256` sul registro, non con una seconda copia (regola O).
    const SHA_SORGENTE_REALE: &str =
        "678814cb71077fc307ed949769e67b87dda36d4bd7b775d487dc65fc81513f98";
    const SHA_TEST_REALE: &str =
        "eaaca1ae1c98b26faa8cc04fba8bc1adaa159fc7f649850e58b87496f6d20bdd";

    /// IL CASO CHE HA MOTIVATO IL CRITERIO, contro i file veri e la raccolta
    /// vera: `fatti_codice` -> vocabolario della migrazione -> `node` eseguito
    /// davvero -> verdetto.
    ///
    /// Il 17/08/2026 il final gate ha dichiarato «passato» DUE volte su questi
    /// esatti due file. Qui il primo esce `Caricato` e il secondo `NonCaricato`
    /// con dentro la parola del runtime, e il verdetto e' bloccante.
    ///
    /// DUE DETTAGLI LOAD-BEARING, entrambi misurati sui file veri:
    ///
    ///  - la sorgente va scritta INSIEME al test. Il `require('./calcolatrice')`
    ///    sta alla riga 1 e si risolve PRIMA che `describe` venga chiamato alla
    ///    riga 3: senza la sorgente il file fallirebbe lo stesso, ma con
    ///    `MODULE_NOT_FOUND` — cioe' il test sarebbe rosso per la causa
    ///    SBAGLIATA, e non proverebbe piu' niente sul difetto. L'asserzione
    ///    sulla causa e' cio' che tiene distinti i due casi;
    ///  - il messaggio del runtime arriva su STDOUT e non su stderr (misurato:
    ///    stderr 0 byte, stdout 1099). E' `messaggio_del_processo` a coprirlo,
    ///    e questo test e' l'unico posto in cui quella scelta viene esercitata
    ///    dal file vero.
    ///
    /// MUTAZIONE ESEGUITA: degradato `NonCaricato` a `NonProvato` in
    /// `prova_file`. Il test rosseggia sull'esito del file — «dev'essere
    /// NonCaricato, ottenuto NonProvato { RuntimeNonDisponibile }» col
    /// `ReferenceError` finito dentro il dettaglio — e non arriva al verdetto,
    /// che a quel punto sarebbe `CodiceCaricabile { provati: 1 }`: nessun file
    /// rotto, la sola sorgente contata fra i provati, quindi un PASSED. E' il
    /// gate del 17/08 che riapprova il file rotto.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn i_due_file_veri_del_run_del_17_agosto(pool: PgPool) {
        assert_eq!(
            crate::file_mutations::sha256_hex(CALCOLATRICE_REALE.as_bytes()),
            SHA_SORGENTE_REALE,
            "la fixture della sorgente non e' piu' il file che il run ha prodotto"
        );
        assert_eq!(
            crate::file_mutations::sha256_hex(TEST_JEST_REALE.as_bytes()),
            SHA_TEST_REALE,
            "la fixture del test non e' piu' il file che il run ha prodotto"
        );

        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let session_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let albero = tempfile::tempdir().expect("tempdir");
        let root = albero.path();

        for (rel, contenuto) in [
            ("calcolatrice.js", CALCOLATRICE_REALE),
            ("calcolatrice.test.js", TEST_JEST_REALE),
        ] {
            scrivi(
                &pool,
                project_id,
                session_id,
                user_id,
                Some(run_id),
                root,
                rel,
                contenuto,
            )
            .await;
        }

        let fatti = fatti_codice(
            &pool,
            project_id,
            session_id,
            root,
            &vocabolario_dalla_migrazione(),
            50,
            30.0,
        )
        .await
        .expect("fatti");
        assert_eq!(fatti.len(), 2, "due file prodotti, due fatti: {fatti:?}");

        let sorgente = fatti
            .iter()
            .find(|f| f.path == "calcolatrice.js")
            .expect("la sorgente e' fra i fatti");
        assert_eq!(
            sorgente.esito,
            EsitoFile::Caricato {
                livello: LivelloProva::Sintassi
            },
            "la sorgente si carica: e' il file che l'audit ha verificato a mano"
        );

        let prova = fatti
            .iter()
            .find(|f| f.path == "calcolatrice.test.js")
            .expect("il file di test e' fra i fatti");
        let EsitoFile::NonCaricato {
            livello,
            exit_code,
            causa,
        } = &prova.esito
        else {
            panic!(
                "il file che il 17/08 ha chiuso il run come «completato» \
                 dev'essere NonCaricato, ottenuto {:?}",
                prova.esito
            );
        };
        assert_eq!(
            *livello,
            LivelloProva::Caricamento,
            "`node --check` questo file lo PASSA (misurato: exit 0): e' il \
             secondo livello a vederlo"
        );
        assert_eq!(*exit_code, Some(1));
        assert!(
            causa.contains("describe is not defined"),
            "la causa e' la parola del RUNTIME, e distingue il difetto vero da \
             un MODULE_NOT_FOUND: {causa}"
        );

        let verdetto = classifica_esecuzione(&fatti);
        let VerdettoEsecuzione::CodiceRotto { rotti } = &verdetto else {
            panic!("atteso CodiceRotto, ottenuto {verdetto:?}");
        };
        assert_eq!(rotti.len(), 1, "un solo file rotto dei due");
        assert_eq!(rotti[0].path, "calcolatrice.test.js");
        assert!(
            verdetto.e_bloccante(),
            "il gate del 17/08 aveva chiuso «passato»: qui non deve"
        );
    }

    /// LO STESSO FILE riscritto con `node:test` si carica — e il suo test
    /// FALLISCE apposta.
    ///
    /// E' la meta' che distingue questo criterio da `suite_verification`: un
    /// test rosso e' informazione, un test che non parte e' codice rotto. Se il
    /// livello di caricamento eseguisse i casi (cioe' `node --test` nudo, come
    /// diceva il design), questo file uscirebbe `CodiceRotto` e il gate
    /// boccerebbe un run per un assert sbagliato — invadendo la domanda di un
    /// altro criterio.
    ///
    /// MUTAZIONE: togliere `--test-name-pattern=...` dalla riga `carica_test`
    /// della migrazione fa girare il test, che esce 1, e questo test rosseggia
    /// con `CodiceRotto`.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn un_test_che_parte_e_fallisce_resta_codice_caricabile(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let session_id = Uuid::new_v4();
        let albero = tempfile::tempdir().expect("tempdir");
        let root = albero.path();
        let voc = vocabolario_dalla_migrazione();

        scrivi(
            &pool, project_id, session_id, user_id, Some(Uuid::new_v4()), root,
            "calcolatrice.js", CALCOLATRICE,
        )
        .await;
        scrivi(
            &pool, project_id, session_id, user_id, Some(Uuid::new_v4()), root,
            "calcolatrice.test.js", TEST_NODE,
        )
        .await;

        let fatti = fatti_codice(&pool, project_id, session_id, root, &voc, 50, 30.0)
            .await
            .expect("fatti");
        let test = fatti
            .iter()
            .find(|f| f.path == "calcolatrice.test.js")
            .expect("il file di test e' fra i fatti");
        assert_eq!(
            test.esito,
            EsitoFile::Caricato {
                livello: LivelloProva::Caricamento
            },
            "si carica: il criterio non giudica l'esito dei test"
        );
        assert_eq!(
            classifica_esecuzione(&fatti),
            VerdettoEsecuzione::CodiceCaricabile { provati: 2 }
        );
    }

    /// Un errore di SINTASSI cade al primo livello, che non esegue niente.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn un_errore_di_sintassi_cade_al_primo_livello(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let session_id = Uuid::new_v4();
        let albero = tempfile::tempdir().expect("tempdir");
        let root = albero.path();

        scrivi(
            &pool, project_id, session_id, user_id, Some(Uuid::new_v4()), root,
            "src/rotto.js", "function somma(a, b) { return a + \n",
        )
        .await;

        let fatti = fatti_codice(
            &pool,
            project_id,
            session_id,
            root,
            &vocabolario_dalla_migrazione(),
            50,
            30.0,
        )
        .await
        .expect("fatti");
        let EsitoFile::NonCaricato { livello, .. } = &fatti[0].esito else {
            panic!("atteso NonCaricato, ottenuto {:?}", fatti[0].esito);
        };
        assert_eq!(*livello, LivelloProva::Sintassi);
        assert!(classifica_esecuzione(&fatti).e_bloccante());
    }

    /// Cio' che non e' codice non produce prove e non declassa nulla: un run che
    /// scrive documentazione passa. E' la variante che tiene il criterio
    /// utilizzabile fuori dai progetti JavaScript.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn i_file_che_non_sono_codice_non_producono_prove(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let session_id = Uuid::new_v4();
        let albero = tempfile::tempdir().expect("tempdir");
        let root = albero.path();

        for (rel, contenuto) in [("README.md", "# titolo\n"), ("dati.json", "{}\n")] {
            scrivi(
                &pool, project_id, session_id, user_id, Some(Uuid::new_v4()), root, rel, contenuto,
            )
            .await;
        }

        let fatti = fatti_codice(
            &pool,
            project_id,
            session_id,
            root,
            &vocabolario_dalla_migrazione(),
            50,
            30.0,
        )
        .await
        .expect("fatti");
        assert_eq!(
            classifica_esecuzione(&fatti),
            VerdettoEsecuzione::NienteDaProvare { scritti: 2 }
        );
    }

    /// IL LAVORO DELEGATO non si perde: un sub-run scrive col PROPRIO `run_id` e
    /// la `session_id` del padre. Col perimetro sul solo run del padre, il file
    /// rotto scritto da un coder convocato sarebbe invisibile e il gate
    /// chiuderebbe di nuovo «completato».
    ///
    /// MUTAZIONE: aggiungere `AND run_id = $run` alla query -> il fatto sparisce
    /// e il verdetto diventa `NienteDaProvare`.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_codice_di_un_sub_run_resta_nel_perimetro(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let session_id = Uuid::new_v4();
        let albero = tempfile::tempdir().expect("tempdir");
        let root = albero.path();

        scrivi(
            &pool, project_id, session_id, user_id, Some(Uuid::new_v4()), root,
            "api.test.js", TEST_JEST,
        )
        .await;

        let fatti = fatti_codice(
            &pool,
            project_id,
            session_id,
            root,
            &vocabolario_dalla_migrazione(),
            50,
            30.0,
        )
        .await
        .expect("fatti");
        assert!(classifica_esecuzione(&fatti).e_bloccante());
    }

    /// Il confine e' la SESSIONE: il codice di un'altra sessione sullo stesso
    /// progetto non e' cio' che questo run ha prodotto. E' il rovescio della
    /// scelta di non filtrare per run.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_codice_di_un_altra_sessione_non_entra(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let mia = Uuid::new_v4();
        let altrui = Uuid::new_v4();
        let albero = tempfile::tempdir().expect("tempdir");
        let root = albero.path();

        scrivi(
            &pool, project_id, altrui, user_id, Some(Uuid::new_v4()), root,
            "vecchio.test.js", TEST_JEST,
        )
        .await;

        let fatti = fatti_codice(
            &pool,
            project_id,
            mia,
            root,
            &vocabolario_dalla_migrazione(),
            50,
            30.0,
        )
        .await
        .expect("fatti");
        assert_eq!(
            classifica_esecuzione(&fatti),
            VerdettoEsecuzione::NienteDaProvare { scritti: 0 }
        );
    }

    /// Un file CANCELLATO dal run non e' un fatto: l'ultima parola su di lui e'
    /// che non deve esistere. L'`op` la deriva `record_mutation` dai contenuti
    /// (`after = None`), non il test.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn un_file_cancellato_non_si_prova(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let session_id = Uuid::new_v4();
        let albero = tempfile::tempdir().expect("tempdir");
        let root = albero.path();

        scrivi(
            &pool, project_id, session_id, user_id, Some(Uuid::new_v4()), root,
            "bozza.test.js", TEST_JEST,
        )
        .await;
        std::fs::remove_file(root.join("bozza.test.js")).expect("rm");
        record_mutation(
            &pool,
            project_id,
            Some(session_id),
            Some(Uuid::new_v4()),
            Some(user_id),
            "bozza.test.js",
            "write_file",
            Some(TEST_JEST),
            None,
            ScopeAudit::none(),
        )
        .await
        .expect("mutazione registrata");

        let fatti = fatti_codice(
            &pool,
            project_id,
            session_id,
            root,
            &vocabolario_dalla_migrazione(),
            50,
            30.0,
        )
        .await
        .expect("fatti");
        assert!(
            fatti.is_empty(),
            "un file cancellato non produce prove: {fatti:?}"
        );
    }

    /// Lo stesso file riscritto piu' volte e' UN file da provare: la deduplica
    /// e' una proprieta' della fonte (il registro e' append-only per scrittura),
    /// non una regola di giudizio. Senza, un run che salva dieci volte pagherebbe
    /// dieci processi e produrrebbe dieci fatti identici.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn le_riscritture_dello_stesso_file_producono_un_fatto_solo(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let session_id = Uuid::new_v4();
        let albero = tempfile::tempdir().expect("tempdir");
        let root = albero.path();

        for i in 0..3 {
            scrivi(
                &pool,
                project_id,
                session_id,
                user_id,
                Some(Uuid::new_v4()),
                root,
                "src/app.js",
                &format!("const versione = {i};\n"),
            )
            .await;
        }

        let fatti = fatti_codice(
            &pool,
            project_id,
            session_id,
            root,
            &vocabolario_dalla_migrazione(),
            50,
            30.0,
        )
        .await
        .expect("fatti");
        assert_eq!(fatti.len(), 1, "tre scritture, un file: {fatti:?}");
    }

    /// Il tetto conta i file PROVATI, non gli scritti: cinquanta `.md` non
    /// devono consumare il budget di prova di un sorgente. Oltre il tetto il file
    /// resta un fatto DICHIARATO, non una prova in piu' e nemmeno un silenzio.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_tetto_conta_i_provati_e_dichiara_gli_esclusi(pool: PgPool) {
        let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let session_id = Uuid::new_v4();
        let albero = tempfile::tempdir().expect("tempdir");
        let root = albero.path();

        scrivi(
            &pool, project_id, session_id, user_id, Some(Uuid::new_v4()), root,
            "note.md", "# note\n",
        )
        .await;
        for i in 0..3 {
            scrivi(
                &pool,
                project_id,
                session_id,
                user_id,
                Some(Uuid::new_v4()),
                root,
                &format!("src/m{i}.js"),
                "const a = 1;\n",
            )
            .await;
        }

        let fatti = fatti_codice(
            &pool,
            project_id,
            session_id,
            root,
            &vocabolario_dalla_migrazione(),
            2,
            30.0,
        )
        .await
        .expect("fatti");
        let provati = fatti
            .iter()
            .filter(|f| matches!(f.esito, EsitoFile::Caricato { .. }))
            .count();
        assert_eq!(provati, 2, "il tetto vale sui provati");
        assert_eq!(
            fatti
                .iter()
                .filter(|f| matches!(
                    f.esito,
                    EsitoFile::NonProvato {
                        causa: CausaNonProvato::OltreIlTetto
                    }
                ))
                .count(),
            1,
            "il terzo sorgente e' dichiarato, non taciuto"
        );
    }
}
