//! Risoluzione dei path di LETTURA dei tool agente.
//!
//! Estratto da `mcp-core::projects` (che resta il modulo axum degli handler
//! HTTP di progetto): la funzione non aveva nulla di specifico di quel modulo,
//! e i tool file estratti nel crate basso ne dipendono. `crate::projects`
//! la re-esporta, cosi' i call site storici restano validi e l'implementazione
//! resta UNA sola (regola L).
//!
//! Il tipo di errore verso i ~53 call site resta `nexus_types::ApiError`, gia'
//! il tipo di ritorno storico: cambiare anche quello avrebbe mescolato uno
//! spostamento con una riscrittura di tutti i chiamanti.
//!
//! ## Perche' la causa e' un CAMPO e non piu' una frase sola
//!
//! `canonicalize()` fallisce per motivi diversi — il percorso non esiste, il
//! permesso e' negato, un symlink e' rotto, il volume non risponde — e ognuno
//! porta un `io::ErrorKind` STRUTTURATO. Fino al 08/08/2026 il resolver li
//! collassava tutti con `map_err(|_| ...)` in un unico "Percorso non trovato",
//! e con essi il path effettivamente cercato: il messaggio AFFERMAVA l'assenza
//! anche quando l'assenza non era stata accertata (regola M e regola Q punto 2
//! — "non ho potuto guardare" non degrada ne' a "non c'e'" ne' a "va bene").
//!
//! Il costo non e' teorico, ed e' stato pagato in diagnosi. Il 08/08/2026 sette
//! `list_files`/`read_file` di gestione-corsi risultavano falliti con quel testo
//! su cartelle che sul disco ESISTONO, e non c'era modo di distinguere un difetto
//! del resolver da un'assenza reale. Erano corretti tutti e sette: il creation
//! time mostra che ogni bersaglio e' nato DOPO la chiamata (`landing/` 25 secondi
//! dopo, `tailwind.config.ts` 8 secondi dopo, gli altri mai esistiti). L'agente
//! guardava se una cosa c'era, non c'era, e la creava — comportamento sano che il
//! messaggio faceva sembrare un guasto.
//!
//! Per questo il messaggio dichiara ora TRE cose che prima taceva: quale path e'
//! stato cercato DOPO la normalizzazione (che de-duplica la root e strippa il
//! prefisso verbatim: se un giorno `normalize_into_root` sbagliasse, oggi sarebbe
//! visibile invece di somigliare a un file mancante), quale causa e' occorsa, e
//! per l'assenza fin dove il percorso ESISTE — cosi' chi legge sa se ha sbagliato
//! l'ultimo segmento o l'intero ramo.

use std::path::{Path, PathBuf};

use nexus_types::workspace_paths::path_within;
use nexus_types::{api_error, ApiError, StatusCode};

/// Perche' un percorso non e' stato risolto.
///
/// Segnale STRUTTURATO (regola M): nasce dall'`io::ErrorKind` che il sistema
/// operativo ha restituito, mai dal testo dell'errore. Le varianti non sono
/// gradazioni della stessa cosa: `NonEsiste` e' una MISURA (ho guardato, non
/// c'e'), `NonInterrogabile` e' l'ammissione di non aver potuto guardare, e
/// confonderle e' esattamente cio' che rendeva il messaggio storico inutile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CausaPercorso {
    /// Il percorso non esiste: assenza ACCERTATA.
    NonEsiste,
    /// Il percorso esiste ma non e' leggibile: il permesso e' negato.
    /// NON e' un'assenza, e il messaggio deve dirlo — un consumatore che lo
    /// leggesse come "manca" proverebbe a ricrearlo sopra qualcosa che c'e'.
    PermessoNegato,
    /// L'esistenza non e' stata accertata: symlink rotto, volume che non
    /// risponde, nome invalido per il filesystem, altro errore di sistema.
    NonInterrogabile(std::io::ErrorKind),
    /// La ROOT del progetto stessa non esiste: il relativo non c'entra nulla.
    /// Senza questa variante il messaggio accuserebbe il path richiesto per
    /// l'assenza di cio' che lo contiene (progetto spostato o cancellato).
    RootAssente,
}

/// Mappa l'`ErrorKind` sulla causa. PURA e totale: nessun IO, cosi' la
/// corrispondenza kind -> variante e' verificabile da sola (regola O).
pub fn classifica_causa(kind: std::io::ErrorKind) -> CausaPercorso {
    match kind {
        std::io::ErrorKind::NotFound => CausaPercorso::NonEsiste,
        std::io::ErrorKind::PermissionDenied => CausaPercorso::PermessoNegato,
        altro => CausaPercorso::NonInterrogabile(altro),
    }
}

/// Esito negativo della risoluzione, coi campi che il messaggio dichiara.
///
/// Il testo si compone DAI campi (regola Q punto 3), mai il contrario: nessun
/// consumatore deve poter ricostruire la causa dalla prosa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorePercorso {
    pub causa: CausaPercorso,
    /// Il path relativo come il resolver l'ha NORMALIZZATO, cioe' cio' che ha
    /// davvero cercato — non l'input grezzo del chiamante.
    pub relativo_normalizzato: String,
    /// Solo per [`CausaPercorso::NonEsiste`]: il tratto piu' profondo che
    /// esiste davvero. `None` se gia' il primo segmento manca.
    pub radice_esistente: Option<String>,
}

impl ErrorePercorso {
    /// Lo status HTTP corretto per la causa. `NonInterrogabile` non e' un 404:
    /// dire "non trovato" a un volume che non risponde e' un'affermazione che
    /// nessuno ha verificato.
    pub fn status(&self) -> StatusCode {
        match self.causa {
            CausaPercorso::NonEsiste | CausaPercorso::RootAssente => StatusCode::NOT_FOUND,
            CausaPercorso::PermessoNegato => StatusCode::FORBIDDEN,
            CausaPercorso::NonInterrogabile(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Il messaggio, composto dai campi.
    ///
    /// Il prefisso "Percorso non trovato" e' CONSERVATO per il solo caso di
    /// assenza: e' cio' che il pannello editor riconosce (`ide-shell.tsx`) per
    /// mostrare "riferimento stantio" invece del toast tecnico. Le altre cause
    /// non lo portano apposta — la' quel suggerimento sarebbe falso.
    pub fn messaggio(&self) -> String {
        let p = &self.relativo_normalizzato;
        match self.causa {
            CausaPercorso::NonEsiste => match &self.radice_esistente {
                Some(radice) => format!(
                    "Percorso non trovato: '{p}' non esiste sotto la root del progetto \
                     (il tratto esistente piu' profondo e' '{radice}')"
                ),
                None => format!(
                    "Percorso non trovato: '{p}' non esiste sotto la root del progetto \
                     (nessun tratto iniziale esiste)"
                ),
            },
            CausaPercorso::PermessoNegato => format!(
                "Percorso non leggibile: permesso negato su '{p}'. Il percorso ESISTE: \
                 non ricrearlo"
            ),
            CausaPercorso::NonInterrogabile(kind) => format!(
                "Percorso non verificabile: '{p}' ha prodotto un errore di sistema \
                 ({kind:?}). L'esistenza NON e' stata accertata: non dedurne l'assenza"
            ),
            CausaPercorso::RootAssente => format!(
                "Percorso non trovato: la ROOT del progetto non esiste sul filesystem, \
                 quindi '{p}' non e' risolvibile. Non e' il path richiesto a mancare"
            ),
        }
    }
}

impl From<ErrorePercorso> for ApiError {
    fn from(e: ErrorePercorso) -> Self {
        api_error(e.status(), e.messaggio())
    }
}

/// Esito negativo della risoluzione, nelle sue DUE nature.
///
/// Non e' una gradazione: un rifiuto della RICHIESTA (path vuoto, traversal,
/// caratteri invalidi) e' un errore del chiamante e ha gia' il suo messaggio col
/// rimedio; una causa di FILESYSTEM e' un fatto osservato sul disco. Tenerle
/// separate nel tipo evita che la seconda erediti il vocabolario della prima.
#[derive(Debug)]
pub enum ErroreRisoluzione {
    /// Il percorso e' ben formato, ma il filesystem non lo ha risolto.
    Filesystem(ErrorePercorso),
    /// La richiesta stessa e' inammissibile: non si e' arrivati al disco.
    Richiesta(ApiError),
}

impl From<ErroreRisoluzione> for ApiError {
    fn from(e: ErroreRisoluzione) -> Self {
        match e {
            ErroreRisoluzione::Filesystem(p) => p.into(),
            ErroreRisoluzione::Richiesta(api) => api,
        }
    }
}

/// Il tratto iniziale piu' profondo di `relativo` che esiste sotto `root`.
///
/// Un `exists()` per segmento, solo nel ramo d'errore e su un path che ha pochi
/// segmenti. Si ferma al primo assente: risalire oltre non direbbe nulla di piu'.
fn radice_esistente(root: &Path, relativo: &str) -> Option<String> {
    let mut corrente = root.to_path_buf();
    let mut profondita = Vec::new();
    for segmento in relativo.split('/').filter(|s| !s.is_empty()) {
        corrente.push(segmento);
        if !corrente.exists() {
            break;
        }
        profondita.push(segmento);
    }
    if profondita.is_empty() {
        None
    } else {
        Some(profondita.join("/"))
    }
}

/// Fase 1 — la RICHIESTA: normalizzazione e de-duplicazione della root.
///
/// Delega al PUNTO UNICO condiviso con la scrittura (regola L): gestisce il caso
/// in cui l'LLM passa un path che DUPLICA la project_root (es.
/// `home/administrator/projects/Foo/src/x.ts`) o la root assoluta. Prima questo
/// resolver di LETTURA non strippava la root, percio' `read_file` falliva con
/// "Percorso non trovato" sugli stessi file che `edit_file`
/// (`resolve_write_target`, che gia' de-duplicava) aveva appena scritto.
///
/// I suoi errori NON sono cause di filesystem: sono rifiuti della richiesta
/// (path vuoto, caratteri invalidi, fuori root) e hanno gia' il proprio
/// messaggio col rimedio in `WorkspaceTargetError::message`.
fn normalizza_richiesta(root: &Path, relative: &str) -> Result<String, ErroreRisoluzione> {
    use nexus_types::workspace_paths::{normalize_into_root, WorkspaceTargetError};

    normalize_into_root(root, relative).map_err(|e| {
        let status = match e {
            WorkspaceTargetError::OutsideRoot => StatusCode::FORBIDDEN,
            WorkspaceTargetError::EmptyPath | WorkspaceTargetError::InvalidChars => {
                StatusCode::BAD_REQUEST
            }
        };
        ErroreRisoluzione::Richiesta(api_error(status, e.message()))
    })
}

/// Fase 2 — la ROOT, che si canonicalizza PRIMA del target.
///
/// Se manca lei, il relativo non c'entra nulla e accusarlo manderebbe a cercare
/// la cosa sbagliata. Per ogni ALTRA causa si prosegue col path non
/// canonicalizzato (comportamento storico): se poi il target fallira', sara' la
/// sua causa a parlare, e sara' quella giusta.
fn canonicalizza_root(root: &Path, clean: &str) -> Result<PathBuf, ErroreRisoluzione> {
    match root.canonicalize() {
        Ok(c) => Ok(c),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(ErroreRisoluzione::Filesystem(ErrorePercorso {
                causa: CausaPercorso::RootAssente,
                relativo_normalizzato: clean.to_string(),
                radice_esistente: None,
            }))
        }
        Err(_) => Ok(root.to_path_buf()),
    }
}

/// Come [`resolve_relative_path`] ma con l'esito NEGATIVO tipizzato.
///
/// E' questa la forma autoritativa: `resolve_relative_path` vi delega e
/// appiattisce l'errore su `ApiError` per i call site storici.
pub fn resolve_relative_path_detailed(
    root: &Path,
    relative: &str,
) -> Result<PathBuf, ErroreRisoluzione> {
    let clean = normalizza_richiesta(root, relative)?;
    let root_canonical = canonicalizza_root(root, &clean)?;

    let target = if clean.is_empty() {
        root_canonical.clone()
    } else {
        if relative.trim() != clean {
            tracing::debug!(
                original = %relative.trim(),
                normalized = %clean,
                "resolve_relative_path: root duplicata/assoluta strippata dal path del tool"
            );
        }
        root_canonical.join(&clean)
    };

    let canonical = target.canonicalize().map_err(|e| {
        let causa = classifica_causa(e.kind());
        ErroreRisoluzione::Filesystem(ErrorePercorso {
            causa,
            radice_esistente: match causa {
                CausaPercorso::NonEsiste => radice_esistente(&root_canonical, &clean),
                _ => None,
            },
            relativo_normalizzato: clean.clone(),
        })
    })?;

    if !path_within(&root_canonical, &canonical) {
        return Err(ErroreRisoluzione::Richiesta(api_error(
            StatusCode::FORBIDDEN,
            "Percorso non autorizzato",
        )));
    }

    Ok(canonical)
}

pub fn resolve_relative_path(root: &Path, relative: &str) -> Result<PathBuf, ApiError> {
    resolve_relative_path_detailed(root, relative).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    // I test attraversano `resolve_relative_path_detailed` REALE su un
    // filesystem vero (regola O): costruire a mano un `ErrorePercorso` per
    // asserire il suo messaggio proverebbe solo che `format!` funziona.

    fn root_di_prova() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("school-courses-fe/src")).expect("albero");
        std::fs::write(dir.path().join("school-courses-fe/src/app.ts"), "x").expect("file");
        dir
    }

    fn errore(root: &Path, relativo: &str) -> ErrorePercorso {
        match resolve_relative_path_detailed(root, relativo) {
            Ok(p) => panic!("atteso errore, risolto {p:?}"),
            Err(ErroreRisoluzione::Filesystem(e)) => e,
            Err(ErroreRisoluzione::Richiesta(api)) => {
                panic!("attesa causa di filesystem, ottenuto rifiuto della richiesta {api:?}")
            }
        }
    }

    #[test]
    fn assenza_reale_dichiara_il_tratto_esistente() {
        // Il caso misurato l'08/08/2026 su gestione-corsi: il modello chiede un
        // sottopath di una cartella che ESISTE. Il messaggio storico ("Percorso
        // non trovato") non distingueva questo da "l'intero ramo non c'e'".
        let dir = root_di_prova();
        let e = errore(dir.path(), "school-courses-fe/SchoolCoursesApi");

        assert_eq!(e.causa, CausaPercorso::NonEsiste);
        assert_eq!(e.radice_esistente.as_deref(), Some("school-courses-fe"));
        assert_eq!(e.status(), StatusCode::NOT_FOUND);
        let msg = e.messaggio();
        assert!(msg.contains("school-courses-fe/SchoolCoursesApi"), "{msg}");
        assert!(msg.contains("il tratto esistente piu' profondo"), "{msg}");
    }

    #[test]
    fn assenza_totale_dichiara_che_nessun_tratto_esiste() {
        // L'altro caso misurato: `landing` chiesta 25 secondi PRIMA di essere
        // creata. Nessun tratto iniziale esiste, e il messaggio deve dirlo:
        // e' la differenza fra "hai sbagliato l'ultimo segmento" e "qui non c'e'
        // proprio niente, crealo".
        let dir = root_di_prova();
        let e = errore(dir.path(), "landing");

        assert_eq!(e.causa, CausaPercorso::NonEsiste);
        assert_eq!(e.radice_esistente, None);
        assert!(e.messaggio().contains("nessun tratto iniziale esiste"));
    }

    #[test]
    fn il_messaggio_porta_il_path_normalizzato_non_l_input_grezzo() {
        // Il resolver de-duplica la root: se il modello ricopia la project_root
        // dentro il path "relativo", cio' che viene cercato NON e' cio' che ha
        // scritto. Il messaggio deve mostrare il secondo, o una futura anomalia
        // di `normalize_into_root` somiglierebbe a un file mancante.
        let dir = root_di_prova();
        let root_str = dir.path().to_string_lossy().replace('\\', "/");
        let dup = format!("{}/landing/index.html", root_str.trim_start_matches('/'));

        let e = errore(dir.path(), &dup);
        assert_eq!(e.relativo_normalizzato, "landing/index.html");
        assert!(
            !e.messaggio().contains(root_str.trim_start_matches('/')),
            "il messaggio non deve ripetere la root: {}",
            e.messaggio()
        );
    }

    #[test]
    fn root_assente_non_accusa_il_path_richiesto() {
        // Progetto spostato o cancellato: senza questa distinzione il messaggio
        // manderebbe a cercare 'src/app.ts' mentre a mancare e' cio' che lo
        // contiene.
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("progetto-che-non-esiste");

        let e = errore(&root, "src/app.ts");
        assert_eq!(e.causa, CausaPercorso::RootAssente);
        assert!(e.messaggio().contains("ROOT del progetto non esiste"));
        assert!(e.messaggio().contains("Non e' il path richiesto a mancare"));
    }

    #[test]
    fn percorso_esistente_risolve() {
        let dir = root_di_prova();
        let risolto = resolve_relative_path(dir.path(), "school-courses-fe/src/app.ts")
            .expect("il file esiste");
        assert!(risolto.ends_with("app.ts"));
    }

    #[test]
    fn cause_distinte_hanno_status_e_testo_distinti() {
        // Il cuore della regola M: le tre cause NON possono collassare nello
        // stesso esito. `NonInterrogabile` in particolare non e' un 404 —
        // affermerebbe un'assenza che nessuno ha verificato.
        let assente = ErrorePercorso {
            causa: CausaPercorso::NonEsiste,
            relativo_normalizzato: "x".into(),
            radice_esistente: None,
        };
        let negato = ErrorePercorso {
            causa: CausaPercorso::PermessoNegato,
            relativo_normalizzato: "x".into(),
            radice_esistente: None,
        };
        let opaco = ErrorePercorso {
            causa: CausaPercorso::NonInterrogabile(std::io::ErrorKind::BrokenPipe),
            relativo_normalizzato: "x".into(),
            radice_esistente: None,
        };

        assert_eq!(assente.status(), StatusCode::NOT_FOUND);
        assert_eq!(negato.status(), StatusCode::FORBIDDEN);
        assert_eq!(opaco.status(), StatusCode::INTERNAL_SERVER_ERROR);

        // Il permesso negato deve NEGARE l'assenza, non tacerla.
        assert!(negato.messaggio().contains("ESISTE"));
        // L'ignoto deve dichiararsi tale.
        assert!(opaco.messaggio().contains("NON e' stata accertata"));
        // E nessuno dei due deve portare il prefisso che il pannello editor
        // riconosce come "file mancante".
        assert!(!negato.messaggio().starts_with("Percorso non trovato"));
        assert!(!opaco.messaggio().starts_with("Percorso non trovato"));
    }

    /// Il produttore REALE consegna una causa diversa da `NonEsiste` quando il
    /// filesystem la produce (regola O): senza questo, la mappatura sarebbe
    /// provata solo dalla funzione pura e nulla garantirebbe che il resolver le
    /// passi davvero il kind del sistema operativo invece di assumere l'assenza.
    ///
    /// Il caso e' per piattaforma perche' lo E' il filesystem, e ogni ramo
    /// DICHIARA cosa prova invece di saltare in silenzio.
    #[cfg(windows)]
    #[test]
    fn nome_invalido_non_e_un_assenza() {
        // Caso vivo: il modello lascia un placeholder nel path (`src/<nome>`).
        // Windows rifiuta il nome, non lo cerca — e raccontarlo come "non
        // trovato" manda a creare un file che quel filesystem non puo' avere.
        //
        // Il genitore deve ESISTERE: se manca anche quello, Windows si ferma
        // prima e risponde NotFound (misurato), che e' una risposta corretta a
        // una domanda diversa.
        let dir = root_di_prova();
        let e = errore(dir.path(), "school-courses-fe/src/<component>.tsx");

        assert!(
            matches!(e.causa, CausaPercorso::NonInterrogabile(_)),
            "causa attesa non-accertata, ottenuta {:?}",
            e.causa
        );
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(e.messaggio().contains("NON e' stata accertata"));
    }

    #[cfg(unix)]
    #[test]
    fn permesso_negato_non_e_un_assenza() {
        use std::os::unix::fs::PermissionsExt;
        let dir = root_di_prova();
        let chiusa = dir.path().join("chiusa");
        std::fs::create_dir(&chiusa).expect("dir");
        std::fs::write(chiusa.join("dentro.txt"), "x").expect("file");
        std::fs::set_permissions(&chiusa, std::fs::Permissions::from_mode(0o000)).expect("chmod");

        let esito = resolve_relative_path_detailed(dir.path(), "chiusa/dentro.txt");
        // Ripristino subito: un tempdir non cancellabile resterebbe sul disco.
        let _ = std::fs::set_permissions(&chiusa, std::fs::Permissions::from_mode(0o755));

        // Girando come root il permesso non nega nulla: qui non c'e' niente da
        // misurare, e lo si DICHIARA invece di lasciar passare un verde muto.
        let e = match esito {
            Ok(_) => {
                println!(
                    "non misurabile: il processo attraversa una directory 0o000 \
                     (probabile root), il permesso non e' negabile"
                );
                return;
            }
            Err(ErroreRisoluzione::Filesystem(e)) => e,
            Err(altro) => panic!("attesa causa di filesystem, ottenuto {altro:?}"),
        };

        assert_eq!(e.causa, CausaPercorso::PermessoNegato);
        assert_eq!(e.status(), StatusCode::FORBIDDEN);
        assert!(e.messaggio().contains("ESISTE"));
    }

    #[test]
    fn classifica_causa_e_totale() {
        assert_eq!(
            classifica_causa(std::io::ErrorKind::NotFound),
            CausaPercorso::NonEsiste
        );
        assert_eq!(
            classifica_causa(std::io::ErrorKind::PermissionDenied),
            CausaPercorso::PermessoNegato
        );
        assert_eq!(
            classifica_causa(std::io::ErrorKind::TimedOut),
            CausaPercorso::NonInterrogabile(std::io::ErrorKind::TimedOut)
        );
    }

    #[test]
    fn prefisso_riconosciuto_dal_pannello_editor_conservato_sull_assenza() {
        // `ide-shell.tsx` (openFileInGroup) riconosce "Percorso non trovato" per
        // mostrare "riferimento stantio" invece del toast tecnico. Sull'assenza
        // quel suggerimento e' corretto e il prefisso va conservato.
        let dir = root_di_prova();
        assert!(errore(dir.path(), "landing")
            .messaggio()
            .starts_with("Percorso non trovato"));
    }

    #[test]
    fn rifiuti_della_richiesta_restano_api_error() {
        // Traversal, path vuoto e caratteri invalidi non sono cause di
        // filesystem: hanno gia' il loro messaggio col rimedio e non passano
        // dalla classificazione.
        let dir = root_di_prova();
        for raw in ["../fuori.txt", "a\0b"] {
            match resolve_relative_path_detailed(dir.path(), raw) {
                Err(ErroreRisoluzione::Richiesta(_)) => {}
                altro => panic!("atteso rifiuto della richiesta per {raw:?}, ottenuto {altro:?}"),
            }
        }
    }
}
