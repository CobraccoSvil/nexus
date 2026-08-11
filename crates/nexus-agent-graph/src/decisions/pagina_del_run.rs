//! Punto unico (regola L) di «QUALE pagina va misurata alla chiusura di QUESTO
//! run?».
//!
//! Domanda NUOVA, e distinta da quella che `static_preview::detect_static_entry`
//! (mcp-core) risponde gia': quella e' «qual e' l'entry di questo sito?», la
//! stessa del pulsante «Apri nel browser» del pannello Servizi, e resta il suo
//! punto unico. Qui si delega come RIPIEGO — mai si ricopia — e la precedenza
//! del SERVIZIO si delega a [`super::static_render::classifica_natura`], che
//! quella decisione la incarna gia'.
//!
//! MISURATO l'11/08/2026, in DUE forme con cause diverse. Entrambe nascono da
//! un unico difetto strutturale: la pagina era risolta a t=0, in
//! `build_native_engine`, cioe' PRIMA che il run scrivesse una sola riga.
//!
//!   FORMA 1 — il gate TACE. Progetto `test-11-08-listino`, nuovo e VUOTO.
//!   L'agente scrive `listino.html`, che non funziona (`Uncaught SyntaxError`,
//!   contenitore `productsGrid` con 0 figli, body di 90 caratteri). Il run
//!   chiude «task complete ok». A t=0 l'albero e' vuoto: il rilevatore ritorna
//!   `None`, la natura e' `SenzaPagina` e il criterio NON NASCE. Non e' colpa
//!   del nome del file — il rilevatore ha un terzo passo che ripiega sul primo
//!   `.html` della radice, e `listino.html` l'avrebbe trovata: e' colpa del
//!   MOMENTO.
//!
//!   FORMA 2 — il gate misura il file SBAGLIATO, ed e' la piu' costosa.
//!   Progetto `verifica-fix-10-08`: a t=0 esistono gia' `index.html` (una todo
//!   app del giorno prima: 1 elemento, body 234 caratteri) e `test-todo.html`.
//!   Il run produce `galleria.html`, che FUNZIONA (6 card, body 885 caratteri).
//!   Il rilevatore, al suo primo passo, trova l'entry canonica `index.html` — e
//!   il gate misura QUELLA: 1 elemento contro `min_elements=5`, bocciata. In
//!   chat: «final_gate non superata, nuovo tentativo 1/2», poi «chiusa al
//!   limite tentativi», con un cambio di provider (mistral -> openrouter
//!   qwen3-235b, «passo a un modello piu' capace») e 254.938 token spesi su un
//!   ciclo che non poteva convergere: correggere `galleria.html` non fa
//!   crescere `index.html`.
//!
//! LE DUE FORME VOGLIONO DUE RIMEDI, e uno solo non basta. Spostare il MOMENTO
//! chiude la forma 1 e non la forma 2: a gate time i candidati sarebbero
//! `index.html`, `galleria.html` e `test-todo.html`, e il primo passo del
//! rilevatore sceglierebbe ancora `index.html`. Percio' qui la precedenza e' un
//! FATTO gia' persistito — le pagine che il run ha SCRITTO (registro
//! `file_mutations`) — e il rilevatore resta il ripiego per i progetti in cui il
//! run non ha toccato nessuna pagina.
//!
//! UNA SOLA pagina, mai N. Con un `min_elements` unico, misurarne diverse
//! moltiplicherebbe i falsi rossi (una pagina di dettaglio scarna boccia il run
//! di un sito che funziona); e i consumatori del criterio lo cercano con
//! `.find(|c| c.criterion_type == CRITERION_TYPE)`, quindi N criteri
//! resterebbero VERDI misurando solo il primo — un cambiamento invisibile ai
//! test che dovrebbero fermarlo.
//!
//! E UNA PAGINA CHE STIA DOVE STA UN SITO. Le due strade che portano una pagina
//! al gate non possono avere due idee di quanto in fondo il sito possa stare: il
//! rilevatore si ferma alla radice e a una sottocartella di primo livello, di
//! proposito, e altrettanto fa la scelta fra le scritture (vedi
//! [`PROFONDITA_MASSIMA`] e [`e_una_pagina_del_sito`]). Che le pagine arrivino
//! dal registro delle scritture NON e' gia' un filtro: il registro porta ogni
//! file che i tool hanno scritto, comprese le pagine che nessuno pubblica.
//!
//! CONFINE (regola L): qui SOLO il criterio puro. La raccolta dei fatti — la
//! query sul registro delle scritture e la scansione dell'albero — sta in
//! mcp-core (`agent_graph_adapter::pagina_del_run`), che porta i fatti e non li
//! giudica.
//!
//! LIMITE NOTO, dichiarato e non risolto qui: l'`origine_frontend` che decide
//! la precedenza del servizio e' fotografata a t=0, come prima. E' il difetto
//! GEMELLO di questo, governa quale dei due criteri browser nasce, e riguarda
//! entrambi: chiuderlo a meta' — solo per la resa statica — creerebbe due
//! verita' sullo stesso fatto dentro lo stesso gate. Va chiuso a parte, per
//! tutti e due i consumatori insieme.

use serde::{Deserialize, Serialize};

use super::static_render::{classifica_natura, NaturaApp};

/// Una scrittura che il registro delle mutazioni dichiara avvenuta. E' un
/// FATTO, non una pagina: il registro porta ogni file toccato, e quali di
/// quelli siano pagine lo decide questo modulo (la porta non filtra, come per
/// `MutationProgressPort`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScritturaOsservata {
    /// Percorso RELATIVO alla radice, come il registro lo ha scritto.
    pub path: String,
    /// L'operazione dichiarata dal registro era una CANCELLAZIONE. Tradotto dal
    /// vocabolario del registro (`op`) da chi legge la riga: qui arriva come
    /// campo, non come stringa da riconoscere (regola M).
    pub cancellata: bool,
    /// L'ha scritta il run che si sta chiudendo, o un altro run della stessa
    /// sessione (tipicamente un sub-run delegato)?
    pub del_run: bool,
}

/// I fatti gia' raccolti. Nessun giudizio: la precedenza la da' [`risolvi_pagina`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FattiPagina {
    /// Le scritture osservate nel PERIMETRO, in ordine di scrittura (la piu'
    /// recente per ultima). L'ordine lo dichiara il produttore — l'id crescente
    /// del registro — e non si deduce dal contenuto.
    pub scritture: Vec<ScritturaOsservata>,
    /// L'entry che il rilevatore del pannello Servizi propone, quando c'e'.
    /// `None` = nessuna pagina rilevata sull'albero.
    pub entry_rilevata: Option<String>,
    /// Le cartelle che non contengono mai il sito del progetto: dipendenze,
    /// cache di build, metadati di VCS.
    ///
    /// NON e' un elenco di questo modulo e non deve diventarlo. Lo possiede
    /// `static_preview::CARTELLE_ESCLUSE` (mcp-core), che e' gia' il punto unico
    /// del rilevamento sull'albero, e arriva qui coi fatti perche' questo crate
    /// non vede quello (regola L: due elenchi darebbero due idee di «cartella
    /// che non e' il sito», e divergerebbero al primo nome aggiunto senza che
    /// nulla fallisca). Che il vocabolario REALE arrivi davvero fin qui lo
    /// verifica il ponte in `mcp-core::agent_graph_adapter::pagina_del_run`,
    /// dove il produttore esiste (regola O).
    ///
    /// Vuoto = nessuna cartella nominata. Non spegne il vincolo: la profondita'
    /// massima vale comunque, e da sola tiene fuori tutto cio' che sta in fondo
    /// a una dipendenza.
    pub cartelle_escluse: Vec<String>,
}

/// Da dove viene la pagina scelta. Non e' un dettaglio di log: e' la differenza
/// fra «ho misurato il lavoro di questo run» e «ho misurato cio' che ho trovato
/// sull'albero», e chi legge un rosso deve poterle distinguere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenienzaPagina {
    /// L'ha scritta il run che si sta chiudendo.
    ScrittaDalRun,
    /// L'ha scritta un altro run della stessa sessione: tipicamente una figura
    /// a cui il lavoro e' stato DELEGATO. Le scritture di un sub-run portano il
    /// `run_id` del sub-run (e la `session_id` del padre), quindi cercarle sotto
    /// il solo run del padre le perderebbe — ed e' la stessa ragione per cui
    /// `MutationProgressPort` prende la sessione come confine.
    ScrittaNellaSessione,
    /// Nessuno l'ha scritta in questo perimetro: e' il RIPIEGO del rilevatore.
    Rilevata,
}

impl ProvenienzaPagina {
    /// Identificatore canonico (regola N) per l'evidenza del gate.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ScrittaDalRun => "written_by_run",
            Self::ScrittaNellaSessione => "written_in_session",
            Self::Rilevata => "detected",
        }
    }

    /// La riga che chi legge il rilievo trova accanto all'URL. Composta DAI
    /// campi (regola Q punto 3).
    pub fn descrizione(self) -> &'static str {
        match self {
            Self::ScrittaDalRun => "pagina scritta da questo run",
            Self::ScrittaNellaSessione => "pagina scritta in questa sessione (lavoro delegato)",
            Self::Rilevata => {
                "nessuna pagina scritta in questo perimetro: pagina rilevata sull'albero"
            }
        }
    }
}

/// La risposta. `NessunaPagina` NON e' un ignoto ed e' la variante che va
/// difesa: un backend, una CLI, una libreria non hanno interfaccia, e per loro
/// «non c'e' pagina» e' una RISPOSTA. Degradarla a inconcludente farebbe
/// chiudere `completed_unverified` ogni progetto senza interfaccia, cioe'
/// pagherebbe il difetto misurato col declassamento di tutti gli altri.
///
/// Il caso «non ho potuto guardare» non e' qui: non e' un esito di questo
/// criterio, e' l'assenza dei suoi fatti — lo dichiara chi li raccoglie.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaginaDaMisurare {
    /// C'e' un servizio: la domanda completa la pone gia' il dialogo browser.
    ConServizio,
    /// Questa pagina, e come ci si e' arrivati.
    Una {
        entry: String,
        provenienza: ProvenienzaPagina,
    },
    /// Nessuna pagina: non c'e' interfaccia da guardare.
    NessunaPagina,
}

/// Il criterio: PURO, i fatti entrano gia' raccolti.
///
/// La precedenza e' in tre gradini, tutti FATTI e nessuno indovinato:
///   1. le pagine scritte da QUESTO run (la piu' recente);
///   2. le pagine scritte nella stessa sessione (la piu' recente): e' il lavoro
///      DELEGATO, che porta un `run_id` diverso e altrimenti si perderebbe;
///   3. l'entry che il rilevatore propone.
///
/// La precedenza del SERVIZIO non e' scritta qui: si delega a
/// [`classifica_natura`], che la incarna gia' e la motiva (dove un progetto
/// serve il proprio frontend, misurare un file dell'albero guarderebbe qualcosa
/// che non e' cio' che il progetto espone).
pub fn risolvi_pagina(origine_servizio: Option<&str>, fatti: &FattiPagina) -> PaginaDaMisurare {
    let scelta = scelta_fra_le_scritture(&fatti.scritture, &fatti.cartelle_escluse);
    let entry = scelta
        .as_ref()
        .map(|(p, _)| p.as_str())
        .or(fatti.entry_rilevata.as_deref());
    match classifica_natura(origine_servizio, entry) {
        NaturaApp::ConServizio => PaginaDaMisurare::ConServizio,
        NaturaApp::SenzaPagina => PaginaDaMisurare::NessunaPagina,
        NaturaApp::Statica { entry } => PaginaDaMisurare::Una {
            entry,
            provenienza: match scelta {
                Some((_, true)) => ProvenienzaPagina::ScrittaDalRun,
                Some((_, false)) => ProvenienzaPagina::ScrittaNellaSessione,
                None => ProvenienzaPagina::Rilevata,
            },
        },
    }
}

/// Il file scritto ha la FORMA di una pagina? L'estensione e' cio' che il proxy
/// `/preview` serve come documento, ed e' l'unico fatto disponibile su un
/// percorso: qui non c'e' un browser che dichiari il tipo (dove c'e', il tipo lo
/// dichiara lui e non l'URL — vedi `risorse_pagina`).
///
/// Risponde alla sola domanda dell'estensione. «E' una pagina DI QUESTO SITO»
/// e' un'altra domanda, e la pone [`e_una_pagina_del_sito`].
pub fn e_una_pagina(path: &str) -> bool {
    let p = path.trim().to_ascii_lowercase();
    p.ends_with(".html") || p.ends_with(".htm")
}

/// Quanto in fondo puo' stare una pagina perche' sia il sito di QUESTO
/// progetto: la radice (`index.html`), oppure una sua sottocartella di primo
/// livello (`landing/index.html`).
///
/// Non e' un numero scelto qui. E' lo STESSO limite che
/// `static_preview::detect_static_entry` si impone di proposito — «piu' in fondo
/// si troverebbero le pagine di esempio delle dipendenze, non il sito del
/// progetto» — e le due strade che portano una pagina al gate, la scrittura e il
/// rilevamento, non possono avere due idee diverse di dove un sito puo' stare:
/// il gate misurerebbe un bersaglio che il pannello Servizi non apre, e il verde
/// varrebbe per un file che nessuno guarda.
///
/// SERVE QUI PIU' CHE LI'. Che una pagina arrivi dal registro delle SCRITTURE
/// sembra gia' un filtro — «l'ha scritta l'agente, quindi e' il suo prodotto» —
/// e non lo e': il registro porta ogni file che i tool hanno toccato. Una
/// `node_modules/foo/docs/index.html` corretta a mano, una
/// `docs/api/v2/index.html` generata insieme al resto, una fixture HTML dentro
/// `tests/fixtures/` sono tutte scritture legittime del run e nessuna e' il sito
/// che qualcuno pubblichera'. Senza il vincolo, la piu' RECENTE fra loro
/// diventerebbe la pagina misurata — cioe' la FORMA 2 del difetto rifatta con un
/// bersaglio diverso, e stavolta scelto proprio dal criterio nuovo.
pub const PROFONDITA_MASSIMA: usize = 1;

/// Il file scritto e' una pagina DI QUESTO SITO? Estensione (la domanda di
/// [`e_una_pagina`]) piu' COLLOCAZIONE.
///
/// `cartelle_escluse` e' il vocabolario delegato che viaggia nei fatti (vedi
/// [`FattiPagina::cartelle_escluse`]): qui non se ne scrive una copia.
///
/// Il prefisso `.` non e' nell'elenco ed e' una regola a se': una cartella
/// nascosta non e' un nome di dipendenza da tenere aggiornato, e' una
/// convenzione del filesystem, e nessun sito si pubblica da li'. Vale anche per
/// `..`, che percio' non puo' portare la scelta fuori dalla radice.
pub fn e_una_pagina_del_sito(path: &str, cartelle_escluse: &[String]) -> bool {
    let normalizzato = normalizza(path);
    if !e_una_pagina(&normalizzato) {
        return false;
    }
    let segmenti: Vec<&str> = normalizzato.split('/').filter(|s| !s.is_empty()).collect();
    // Le cartelle attraversate sono i segmenti meno il file. Un percorso senza
    // segmenti non e' un file, e `checked_sub` lo dice invece di andare sotto
    // zero.
    let Some(profondita) = segmenti.len().checked_sub(1) else {
        return false;
    };
    if profondita > PROFONDITA_MASSIMA {
        return false;
    }
    segmenti
        .iter()
        .take(profondita)
        .all(|c| !cartella_esclusa(c, cartelle_escluse))
}

/// Questa cartella e' una di quelle che non contengono mai il sito?
fn cartella_esclusa(nome: &str, escluse: &[String]) -> bool {
    nome.starts_with('.') || escluse.iter().any(|e| e.eq_ignore_ascii_case(nome))
}

/// Il percorso come lo capisce la route `/preview`: separatori URL e nessun
/// prefisso relativo.
///
/// Non e' cosmesi. Il registro salva cio' che il tool gli ha passato, e su
/// Windows un `src\index.html` o un `./index.html` comporrebbero un indirizzo
/// che risponde 404 — cioe' il criterio direbbe «pagina non caricata» per un
/// file che esiste, che e' il modo peggiore di sbagliare (un difetto della
/// misura travestito da difetto del codice).
fn normalizza(path: &str) -> String {
    let p = path.trim().replace('\\', "/");
    let p = p.trim_start_matches("./");
    p.trim_start_matches('/').to_string()
}

/// La pagina scelta fra le scritture, con il suo `del_run`. `None` = nessuna
/// pagina scritta in questo perimetro.
///
/// Si guarda l'ULTIMO stato di ogni percorso: una pagina creata e poi
/// CANCELLATA non e' una pagina, e senza l'ordine delle scritture non lo si
/// saprebbe. Fra i superstiti vince chi ha il `del_run`, e a parita' la
/// scrittura piu' recente — cioe' la pagina su cui il run stava lavorando per
/// ultima, che in un ciclo di correzione e' quella appena corretta.
///
/// Perche' la piu' RECENTE e non «quella che si chiama index»: preferire un
/// nome ricopierebbe qui la convenzione che `detect_static_entry` possiede
/// (regola L), mentre l'ordine di scrittura e' un fatto che il registro porta
/// gia'.
///
/// Entrano le sole pagine che stanno DOVE sta un sito ([`e_una_pagina_del_sito`]):
/// senza quel vincolo, l'ultima pagina scritta sotto una dipendenza o in fondo a
/// una cartella di documentazione diventerebbe il bersaglio del gate.
fn scelta_fra_le_scritture(
    scritture: &[ScritturaOsservata],
    cartelle_escluse: &[String],
) -> Option<(String, bool)> {
    /// Ultimo stato noto di un percorso: quando e' stato toccato l'ultima
    /// volta, se quel tocco era una cancellazione, e chi l'ha fatto.
    struct Ultimo {
        path: String,
        indice: usize,
        cancellata: bool,
        del_run: bool,
    }

    let mut ultimi: Vec<Ultimo> = Vec::new();
    for (indice, s) in scritture.iter().enumerate() {
        let path = normalizza(&s.path);
        if path.is_empty() || !e_una_pagina_del_sito(&path, cartelle_escluse) {
            continue;
        }
        match ultimi.iter_mut().find(|u| u.path == path) {
            Some(u) => {
                u.indice = indice;
                u.cancellata = s.cancellata;
                u.del_run = s.del_run;
            }
            None => ultimi.push(Ultimo {
                path,
                indice,
                cancellata: s.cancellata,
                del_run: s.del_run,
            }),
        }
    }
    ultimi
        .into_iter()
        .filter(|u| !u.cancellata)
        .max_by_key(|u| (u.del_run, u.indice))
        .map(|u| (u.path, u.del_run))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scritta(path: &str, del_run: bool) -> ScritturaOsservata {
        ScritturaOsservata {
            path: path.to_string(),
            cancellata: false,
            del_run,
        }
    }

    /// Il vocabolario delle cartelle escluse come lo scrive il suo unico
    /// proprietario, `static_preview::CARTELLE_ESCLUSE`. Qui e' riprodotto
    /// perche' questo crate non vede quello; che il vocabolario REALE arrivi
    /// davvero fino al criterio lo verifica il ponte in
    /// `mcp-core::agent_graph_adapter::pagina_del_run` (regola O), senza il
    /// quale un nome aggiunto la' lascerebbe questi test verdi su un criterio
    /// che non lo conosce.
    fn cartelle_escluse() -> Vec<String> {
        ["node_modules", ".git", ".next", ".nuxt", ".svelte-kit", "target"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// I fatti come li consegna la porta: scritture, entry rilevata e il
    /// vocabolario delegato.
    fn fatti_con(scritture: Vec<ScritturaOsservata>, entry_rilevata: Option<&str>) -> FattiPagina {
        FattiPagina {
            scritture,
            entry_rilevata: entry_rilevata.map(str::to_string),
            cartelle_escluse: cartelle_escluse(),
        }
    }

    /// FORMA 2, la piu' costosa: l'albero porta `index.html` di IERI e il run
    /// ha prodotto `galleria.html`. Il rilevatore propone la canonica, che
    /// vince sempre al suo primo passo; la scrittura del run la scavalca.
    ///
    /// MUTAZIONE M1: ignorare le scritture del run (`scritture: vec![]`, cioe'
    /// il comportamento precedente) e la scelta torna `index.html` — il ciclo
    /// che non convergeva, perche' correggere `galleria.html` non fa crescere
    /// `index.html`.
    #[test]
    fn la_pagina_scritta_dal_run_scavalca_l_entry_rilevata() {
        let fatti = fatti_con(vec![scritta("galleria.html", true)], Some("index.html"));
        assert_eq!(
            risolvi_pagina(None, &fatti),
            PaginaDaMisurare::Una {
                entry: "galleria.html".to_string(),
                provenienza: ProvenienzaPagina::ScrittaDalRun,
            }
        );

        // La mutazione, esplicita: senza le scritture si torna a misurare la
        // todo app del giorno prima.
        let cieco = FattiPagina {
            scritture: Vec::new(),
            ..fatti
        };
        assert_eq!(
            risolvi_pagina(None, &cieco),
            PaginaDaMisurare::Una {
                entry: "index.html".to_string(),
                provenienza: ProvenienzaPagina::Rilevata,
            }
        );
    }

    /// FORMA 1: progetto vuoto a t=0. Qui il fatto e' l'ASSENZA di entry
    /// rilevata al momento in cui si guardava; la scrittura del run e' l'unica
    /// cosa che esiste, e basta a far nascere il criterio.
    #[test]
    fn su_un_progetto_vuoto_la_scrittura_del_run_e_l_unica_pagina() {
        let fatti = fatti_con(vec![scritta("listino.html", true)], None);
        assert_eq!(
            risolvi_pagina(None, &fatti),
            PaginaDaMisurare::Una {
                entry: "listino.html".to_string(),
                provenienza: ProvenienzaPagina::ScrittaDalRun,
            }
        );
    }

    /// Il lavoro DELEGATO non si perde: un sub-run scrive col proprio `run_id`,
    /// e cercare sotto il solo run del padre lo renderebbe invisibile — con la
    /// conseguenza di ricadere sul rilevatore, cioe' di rifare la forma 2.
    #[test]
    fn una_pagina_scritta_da_un_sub_run_batte_comunque_il_rilevatore() {
        let fatti = fatti_con(vec![scritta("catalogo.html", false)], Some("index.html"));
        assert_eq!(
            risolvi_pagina(None, &fatti),
            PaginaDaMisurare::Una {
                entry: "catalogo.html".to_string(),
                provenienza: ProvenienzaPagina::ScrittaNellaSessione,
            }
        );
    }

    /// Fra due pagine dello STESSO perimetro vince quella del run; fra due
    /// dello stesso rango vince l'ULTIMA scritta.
    #[test]
    fn fra_piu_pagine_ne_resta_una_sola_e_la_scelta_e_dichiarata() {
        let fatti = fatti_con(
            vec![
                scritta("vecchia.html", true),
                scritta("delegata.html", false),
                scritta("recente.html", true),
            ],
            Some("index.html"),
        );
        assert_eq!(
            risolvi_pagina(None, &fatti),
            PaginaDaMisurare::Una {
                entry: "recente.html".to_string(),
                provenienza: ProvenienzaPagina::ScrittaDalRun,
            }
        );

        // Solo lavoro delegato: vince l'ultima delegata, non il rilevatore.
        let solo_delegate = fatti_con(
            vec![scritta("a.html", false), scritta("b.html", false)],
            Some("index.html"),
        );
        assert_eq!(
            risolvi_pagina(None, &solo_delegate),
            PaginaDaMisurare::Una {
                entry: "b.html".to_string(),
                provenienza: ProvenienzaPagina::ScrittaNellaSessione,
            }
        );
    }

    /// Una pagina creata e poi CANCELLATA non e' una pagina: misurarla darebbe
    /// «non caricata» su un file che il run ha rimosso di proposito.
    #[test]
    fn una_pagina_cancellata_non_e_un_candidato() {
        let fatti = fatti_con(
            vec![
                scritta("bozza.html", true),
                ScritturaOsservata {
                    path: "bozza.html".to_string(),
                    cancellata: true,
                    del_run: true,
                },
            ],
            Some("index.html"),
        );
        assert_eq!(
            risolvi_pagina(None, &fatti),
            PaginaDaMisurare::Una {
                entry: "index.html".to_string(),
                provenienza: ProvenienzaPagina::Rilevata,
            }
        );

        // Cancellata e RISCRITTA: torna a essere un candidato, perche' conta
        // l'ULTIMO stato del percorso e non il fatto che sia stata cancellata
        // una volta.
        let riscritta = fatti_con(
            vec![
                ScritturaOsservata {
                    path: "bozza.html".to_string(),
                    cancellata: true,
                    del_run: true,
                },
                scritta("bozza.html", true),
            ],
            Some("index.html"),
        );
        assert_eq!(
            risolvi_pagina(None, &riscritta),
            PaginaDaMisurare::Una {
                entry: "bozza.html".to_string(),
                provenienza: ProvenienzaPagina::ScrittaDalRun,
            }
        );
    }

    /// Cio' che non e' una pagina non entra: un run che scrive solo codice non
    /// deve far misurare un `.js` alla route di anteprima.
    #[test]
    fn solo_i_file_di_pagina_sono_candidati() {
        assert!(e_una_pagina("index.html") && e_una_pagina("a/b/PAGINA.HTM"));
        assert!(!e_una_pagina("app.js") && !e_una_pagina("style.css") && !e_una_pagina(""));

        let fatti = fatti_con(
            vec![scritta("src/app.js", true), scritta("src/style.css", true)],
            Some("index.html"),
        );
        assert_eq!(
            risolvi_pagina(None, &fatti),
            PaginaDaMisurare::Una {
                entry: "index.html".to_string(),
                provenienza: ProvenienzaPagina::Rilevata,
            }
        );
    }

    /// UNA PAGINA IN FONDO A UN ALBERO NON E' IL SITO. Il limite e' quello che
    /// `detect_static_entry` si impone gia': radice, o una sottocartella di
    /// primo livello. Le due strade che portano una pagina al gate — scrittura e
    /// rilevamento — devono avere la STESSA idea di dove un sito puo' stare.
    ///
    /// MUTAZIONE: togliere il vincolo di profondita' (accettare qualunque
    /// percorso che finisca in `.html`) e la scelta va sull'ULTIMA scritta, cioe'
    /// `docs/api/v2/index.html` — una pagina di documentazione misurata al posto
    /// del sito, che e' la FORMA 2 del difetto rifatta con un bersaglio nuovo.
    #[test]
    fn una_pagina_troppo_in_fondo_non_e_il_sito() {
        assert!(e_una_pagina_del_sito("index.html", &cartelle_escluse()));
        assert!(e_una_pagina_del_sito(
            "landing/index.html",
            &cartelle_escluse()
        ));
        assert!(!e_una_pagina_del_sito(
            "docs/api/v2/index.html",
            &cartelle_escluse()
        ));

        let fatti = fatti_con(
            vec![
                scritta("index.html", true),
                scritta("docs/api/v2/index.html", true),
            ],
            None,
        );
        assert_eq!(
            risolvi_pagina(None, &fatti),
            PaginaDaMisurare::Una {
                entry: "index.html".to_string(),
                provenienza: ProvenienzaPagina::ScrittaDalRun,
            },
            "fra le scritture resta il sito, non la pagina piu' recente in assoluto"
        );
    }

    /// Una pagina DENTRO una dipendenza non e' il prodotto del run, nemmeno
    /// quando e' il run ad averla scritta: `node_modules/foo/docs/index.html` la
    /// tiene fuori la profondita', `node_modules/index.html` il vocabolario, e
    /// `.next/index.html` la regola sulle cartelle nascoste.
    ///
    /// MUTAZIONE: ignorare `cartelle_escluse` e la pagina di una dipendenza
    /// diventa il bersaglio del gate — misurato per un sito che nessuno
    /// pubblica.
    #[test]
    fn una_pagina_dentro_una_dipendenza_non_e_il_sito() {
        let v = cartelle_escluse();
        assert!(!e_una_pagina_del_sito("node_modules/foo/docs/index.html", &v));
        assert!(!e_una_pagina_del_sito("node_modules/index.html", &v));
        assert!(!e_una_pagina_del_sito(".next/index.html", &v));
        assert!(!e_una_pagina_del_sito("target/index.html", &v));
        // Una risalita non porta la scelta fuori dalla radice.
        assert!(!e_una_pagina_del_sito("../fuori/index.html", &v));
        // `dist` NON e' esclusa: e' li' che finisce un sito costruito. La stessa
        // scelta che il rilevatore fa, e per la stessa ragione.
        assert!(e_una_pagina_del_sito("dist/index.html", &v));

        let fatti = fatti_con(
            vec![
                scritta("vetrina.html", true),
                scritta("node_modules/pacchetto/index.html", true),
            ],
            None,
        );
        assert_eq!(
            risolvi_pagina(None, &fatti),
            PaginaDaMisurare::Una {
                entry: "vetrina.html".to_string(),
                provenienza: ProvenienzaPagina::ScrittaDalRun,
            }
        );
    }

    /// «Nessuna pagina» resta una RISPOSTA. E' la variante da difendere: un
    /// backend senza interfaccia non deve chiudere `completed_unverified`.
    ///
    /// MUTAZIONE: farla degradare a un inconcludente e ogni progetto senza
    /// pagina viene declassato — il difetto misurato pagato col declassamento
    /// di tutti gli altri.
    #[test]
    fn senza_pagina_la_risposta_e_nessuna_pagina() {
        assert_eq!(
            risolvi_pagina(None, &FattiPagina::default()),
            PaginaDaMisurare::NessunaPagina
        );
        // Un run che ha scritto solo codice: nessuna pagina, e nessun ripiego
        // da inventare.
        let solo_codice = fatti_con(vec![scritta("main.rs", true)], None);
        assert_eq!(
            risolvi_pagina(None, &solo_codice),
            PaginaDaMisurare::NessunaPagina
        );

        // Nemmeno una pagina che c'e' ma non e' del sito: scartarla riporta
        // alla stessa risposta, non a un ripiego inventato.
        let solo_dipendenze = fatti_con(vec![scritta("node_modules/x/index.html", true)], None);
        assert_eq!(
            risolvi_pagina(None, &solo_dipendenze),
            PaginaDaMisurare::NessunaPagina
        );
    }

    /// La precedenza del servizio non e' riscritta qui: si delega, e quindi
    /// vale anche quando il run ha scritto pagine. Dove il progetto serve il
    /// proprio frontend, la domanda completa la pone il dialogo browser.
    ///
    /// MUTAZIONE: far vincere la scrittura sul servizio e un'app servita
    /// verrebbe aperta su `/preview`, cioe' misurata su un file che non e'
    /// cio' che il progetto espone.
    #[test]
    fn il_servizio_ha_la_precedenza_anche_sulle_pagine_scritte() {
        let fatti = fatti_con(vec![scritta("galleria.html", true)], Some("index.html"));
        assert_eq!(
            risolvi_pagina(Some("http://127.0.0.1:35954"), &fatti),
            PaginaDaMisurare::ConServizio
        );
    }

    /// Il percorso viaggia nella forma che la route `/preview` capisce: un
    /// separatore di Windows o un `./` comporrebbero un indirizzo che risponde
    /// 404, e il criterio direbbe «pagina non caricata» per un file che esiste.
    #[test]
    fn il_percorso_scritto_si_normalizza_per_la_route() {
        let fatti = fatti_con(vec![scritta("./landing\\index.html", true)], None);
        assert_eq!(
            risolvi_pagina(None, &fatti),
            PaginaDaMisurare::Una {
                entry: "landing/index.html".to_string(),
                provenienza: ProvenienzaPagina::ScrittaDalRun,
            }
        );
    }
}
