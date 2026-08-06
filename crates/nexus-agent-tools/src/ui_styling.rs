//! `ui_styling`: PUNTO UNICO (regola L) della domanda «lo stile che il codice
//! DICHIARA ha una fonte che lo applica?».
//!
//! # Il difetto che lo motiva
//!
//! Il 29/07/2026, progetto gestione-spese, Nexus ha consegnato un'app i cui
//! componenti scrivevano `className="p-4"`, `className="text-xl font-bold mb-4"`,
//! `className="space-y-4"` — e nel progetto non esisteva NESSUN foglio di stile,
//! nessuna `tailwind.config`, nessuna dipendenza che producesse quelle utility.
//! Le classi erano stringhe inerti: il codice SEMBRAVA stilizzato, la pagina era
//! grezza. Il Consiglio ha deliberato senza rilievi, e nel medesimo run la figura
//! `ui_ux_designer` ha prodotto requisiti su porte e dipendenze.
//!
//! Non era un giudizio sbagliato: nessuno poneva la domanda. E nessun revisore
//! che legga un componente alla volta puo' porsela, perche' la risposta non sta
//! in quel file — sta nell'incrocio fra cio' che i sorgenti dichiarano, cio' che
//! il manifest installa, cio' che la configurazione abilita e cio' che i fogli
//! raggiunti dall'app definiscono davvero.
//!
//! # Perche' deterministico e non affidato al giudizio del modello
//!
//! «Bello» non e' un criterio: un giudice di gusto senza metro moltiplica i
//! rimandi a vuoto, che costano (un run del 27/07: 3 rimandi, 2,1M token, 3,08
//! USD). Questo invece non e' gusto, e' un FATTO: o esiste qualcosa che rende
//! quelle classi, o non esiste. Un fatto si misura, e misurandolo si ottiene
//! anche cio' che un prompt non puo' dare — un test che arriva alla conseguenza
//! (regola O) invece di verificare che il prompt contenga la parola «css».
//!
//! Resta il triangolo: **rilevazione certa** qui, **diagnosi e progetto** alla
//! figura (che cita il catalogo dei pattern), **chiusura oggettiva** al gate.
//!
//! # Una domanda sola, non un elenco di casi
//!
//! Il modulo NON chiede «c'e' Tailwind?». Tailwind e' un'istanza. La domanda e':
//! *le classi letterali scritte nei sorgenti hanno una fonte che le produce?*
//! Le fonti possibili sono CATEGORIE (foglio raggiunto, framework di utility
//! installato e configurato, libreria che stila a runtime, stile inline); i
//! nomi dei pacchetti sono un DATO nel DB (regola G/H), quindi un framework
//! nuovo e' una riga in `settings`, non un deploy.
//!
//! La parte pura ([`classify_styling`]) e' separata dalla raccolta dei fatti
//! ([`collect_evidence`]): il criterio si testa senza filesystem, e i test del
//! criterio passano dal produttore vero invece di fabbricare l'evidenza a mano.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::fs;

use crate::context_core::ToolContextCore;

// ─────────────────────────────────────────────────────────────────────────────
// Vocabolario: il DATO che il criterio consuma
// ─────────────────────────────────────────────────────────────────────────────

/// Un framework di utility CSS: da' stile alle classi solo se INSTALLATO **e**
/// CONFIGURATO. E' la categoria che produce il difetto piu' insidioso, perche'
/// mezzo installato somiglia moltissimo a installato.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UtilityFramework {
    /// Nome del pacchetto in `dependencies`/`devDependencies`.
    pub pacchetto: String,
    /// File di configurazione che ne provano l'abilitazione (uno basta).
    pub config_attesi: Vec<String>,
    /// Direttive che, dentro un foglio RAGGIUNTO dall'app, ne provano
    /// l'abilitazione (una basta). E' l'altra strada, quella senza file di
    /// config: `@import "tailwindcss"` della v4 non ha `tailwind.config`.
    pub direttive_attese: Vec<String>,
}

/// Vocabolario di riconoscimento. Tutto DATO (`settings`), niente elenchi in
/// questo file: la regola qui, i nomi nel DB.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StyleVocabulary {
    /// Suffissi dei file che DICHIARANO stile (i sorgenti di interfaccia).
    pub source_suffixes: Vec<String>,
    /// Suffissi dei file che POSSONO applicarlo (i fogli).
    pub stylesheet_suffixes: Vec<String>,
    pub utility_frameworks: Vec<UtilityFramework>,
    /// Pacchetti che stilano da soli, senza configurazione: librerie di
    /// componenti gia' vestiti e CSS-in-JS. Presenza in manifest = fonte attiva.
    pub runtime_packages: Vec<String>,
    /// Sotto questo numero di classi letterali distinte il difetto NON viene
    /// dichiarato: un campione minuscolo non e' una prova, e un falso positivo
    /// su un prototipo di due righe insegnerebbe a ignorare il rilievo.
    pub min_classi: usize,
}

impl StyleVocabulary {
    /// Il vocabolario e' utilizzabile? Senza suffissi di sorgente non si puo'
    /// nemmeno sapere quali file guardare, e rispondere lo stesso sarebbe
    /// rispondere a una domanda che non si e' posta.
    fn e_utilizzabile(&self) -> bool {
        !self.source_suffixes.is_empty() && !self.stylesheet_suffixes.is_empty()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Fatti: cio' che si e' potuto osservare, senza interpretazione
// ─────────────────────────────────────────────────────────────────────────────

/// I FATTI grezzi del progetto. Nessun giudizio: il giudizio e'
/// [`classify_styling`], e sta altrove proprio per poterlo testare da solo.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StylingEvidence {
    /// Quanti file di interfaccia sono stati letti. Zero = la domanda non si
    /// pone (non e' un frontend).
    pub sorgenti_interfaccia: usize,
    /// Classi letterali distinte scritte nei sorgenti, ordinate.
    pub classi_dichiarate: Vec<String>,
    /// Le classi dichiarate per cui ESISTE un selettore in un foglio raggiunto
    /// dall'app. E' cio' che distingue un foglio che stila da un foglio vuoto.
    pub classi_con_selettore: Vec<String>,
    /// Fogli che l'app carica davvero (importati da un sorgente, collegati da
    /// un HTML, o importati a loro volta da un foglio raggiunto).
    pub fogli_raggiunti: Vec<String>,
    /// Fogli presenti sul disco che nessuno raggiunge: sono l'altra faccia
    /// dello stesso difetto — stile che sembra esserci e non arriva mai.
    pub fogli_orfani: Vec<String>,
    /// Fogli riferiti dal codice che stanno FUORI dalla radice esaminata (un
    /// monorepo con `target_dir` e uno `shared/` accanto). Non si leggono, e
    /// quindi non si puo' dire che non esistano fonti: la premessa va
    /// dichiarata, altrimenti lo zero e' un'opinione (regola O).
    pub fogli_fuori_radice: Vec<String>,
    /// Direttive di framework trovate nei fogli RAGGIUNTI (per pacchetto).
    pub direttive_per_pacchetto: BTreeMap<String, String>,
    /// File di configurazione di framework trovati (per pacchetto).
    pub config_per_pacchetto: BTreeMap<String, String>,
    /// Unione di `dependencies` e `devDependencies`.
    pub dipendenze: BTreeSet<String>,
    /// Almeno un sorgente usa stili inline (`style=`): fonte povera ma vera.
    pub usa_stile_inline: bool,
    /// Sorgenti che portano un blocco `<style>` al loro interno (componenti
    /// Vue/Svelte/Astro, pagine HTML). Non passano da nessun import — li estrae
    /// il compilatore del framework — quindi cercarli fra i fogli non li
    /// troverebbe mai.
    pub sorgenti_con_stile_interno: Vec<String>,
    /// Un `package.json` e' stato letto. Se no, «non c'e' la dipendenza» non e'
    /// un'osservazione: e' un'assenza di osservazione (regola O).
    pub manifest_letto: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Verdetto: segnale strutturato, mai una frase da interpretare (regola M)
// ─────────────────────────────────────────────────────────────────────────────

/// Una fonte di stile ATTIVA, con la prova che la rende tale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StyleSource {
    /// Un foglio raggiunto dall'app che definisce almeno una classe usata.
    FoglioApplicato {
        file: String,
        classi_coperte: usize,
    },
    /// Stili scritti DENTRO il sorgente (`<style>` di un componente Vue,
    /// Svelte, Astro o di una pagina HTML). Non passa da nessun import: il
    /// framework lo estrae in fase di build. Trattarlo come assente boccerebbe
    /// ogni progetto Vue o Svelte scritto secondo la sua convenzione piu'
    /// comune, che e' il rimando a vuoto peggiore — quello sistematico.
    StileNelComponente { file: String },
    /// Framework di utility installato E abilitato (config o direttiva).
    FrameworkUtility { pacchetto: String, prova: String },
    /// Libreria che stila da sola (componenti vestiti, CSS-in-JS).
    LibreriaRuntime { pacchetto: String },
    /// Stili inline nei sorgenti.
    StileInline,
}

impl StyleSource {
    fn to_value(&self) -> Value {
        match self {
            Self::FoglioApplicato {
                file,
                classi_coperte,
            } => json!({
                "tipo": "foglio_applicato",
                "file": file,
                "classi_coperte": classi_coperte,
            }),
            Self::FrameworkUtility { pacchetto, prova } => json!({
                "tipo": "framework_utility",
                K_PACCHETTO: pacchetto,
                "prova": prova,
            }),
            Self::StileNelComponente { file } => json!({
                "tipo": "stile_nel_componente",
                "file": file,
            }),
            Self::LibreriaRuntime { pacchetto } => json!({
                "tipo": "libreria_runtime",
                K_PACCHETTO: pacchetto,
            }),
            Self::StileInline => json!({ "tipo": "stile_inline" }),
        }
    }
}

/// Perche' le classi restano inerti. E' la parte DIAGNOSTICA del verdetto: dice
/// cosa manca, quindi cosa fare. Un «non applicato» senza causa manderebbe chi
/// corregge a indovinare, ed e' esattamente il modo in cui un rilievo diventa
/// un rimando a vuoto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CausaMancata {
    /// Nessuna fonte, di nessun tipo. Il caso gestione-spese.
    NessunaFonte,
    /// Il pacchetto e' in manifest ma manca cio' che lo abilita: le sue utility
    /// non vengono generate. Mezzo installato e' peggio di non installato,
    /// perche' la dipendenza in `package.json` fa da alibi a chi controlla.
    FrameworkNonConfigurato { pacchetto: String, manca: String },
    /// Fogli raggiunti ci sono, ma NESSUNA delle classi usate vi trova un
    /// selettore: le classi vengono da un vocabolario che il progetto non
    /// possiede.
    FogliSenzaSelettori { fogli: usize },
}

/// Chiave del testo che spiega la causa a chi legge il risultato.
const K_SPIEGAZIONE: &str = "spiegazione";
/// Chiave del pacchetto nominato in un payload.
const K_PACCHETTO: &str = "pacchetto";

const SPIEGA_NESSUNA_FONTE: &str =
    "il progetto non ha fogli di stile raggiunti, ne' un framework di utility installato e \
     configurato, ne' una libreria che stili a runtime: ogni classe scritta nei componenti \
     e' inerte";

const SPIEGA_FOGLI_SENZA_SELETTORI: &str =
    "i fogli che l'app carica non definiscono nessuna delle classi usate nei componenti: quelle \
     classi vengono da un vocabolario che il progetto non possiede";

impl CausaMancata {
    fn to_value(&self) -> Value {
        match self {
            Self::NessunaFonte => json!({
                "causa": "nessuna_fonte",
                K_SPIEGAZIONE: SPIEGA_NESSUNA_FONTE,
            }),
            Self::FrameworkNonConfigurato { pacchetto, manca } => json!({
                "causa": "framework_non_configurato",
                K_PACCHETTO: pacchetto,
                "manca": manca,
                K_SPIEGAZIONE: spiega_framework_a_meta(pacchetto, manca),
            }),
            Self::FogliSenzaSelettori { fogli } => json!({
                "causa": "fogli_senza_selettori",
                "fogli_raggiunti": fogli,
                K_SPIEGAZIONE: SPIEGA_FOGLI_SENZA_SELETTORI,
            }),
        }
    }
}

/// Il caso in cui la dipendenza c'e' e fa da alibi: va detto per esteso, o chi
/// corregge aggiunge il pacchetto che era gia' installato.
fn spiega_framework_a_meta(pacchetto: &str, manca: &str) -> String {
    format!(
        "'{pacchetto}' e' fra le dipendenze ma non e' abilitato ({manca}): le sue utility non \
         vengono generate, quindi le classi restano inerti"
    )
}

/// L'esito. Cinque varianti, e la distinzione fra la terza e la quarta e' il
/// cuore: un'app senza stile e' grezza ma ONESTA; un'app che dichiara classi
/// che nulla produce MENTE a chi ne legge il codice — incluso il revisore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StylingVerdict {
    /// Nessun sorgente di interfaccia: la domanda non si pone.
    NonApplicabile,
    /// Nessuna classe dichiarata e nessuna fonte: pagina grezza, codice onesto.
    /// Vale un suggerimento, mai un rilievo bloccante.
    NessunoStileDichiarato,
    /// Almeno una fonte attiva copre cio' che il codice dichiara.
    StileApplicato { fonti: Vec<StyleSource> },
    /// IL difetto: il codice scrive classi che nulla definisce.
    StileDichiaratoNonApplicato {
        classi_orfane: usize,
        campione: Vec<String>,
        causa: CausaMancata,
    },
    /// Il vocabolario di riconoscimento non e' configurato: non si risponde.
    /// Un verdetto tirato a indovinare su un vocabolario vuoto direbbe
    /// «nessuna fonte» per QUALUNQUE progetto (regola M: il silenzio della
    /// configurazione non e' un'osservazione sul progetto).
    VocabolarioAssente,
    /// Nessuna fonte fra quelle che si sono POTUTE guardare, ma il codice ne
    /// riferisce altre fuori dalla radice esaminata. «Non ho visto» non e' «non
    /// c'e'»: qui un difetto dichiarato sarebbe un rimando a vuoto, e il modo
    /// di chiuderlo e' ri-eseguire con la radice giusta.
    NonConcludente { fogli_non_esaminati: Vec<String> },
}

/// Quante classi mostrare come campione: bastano a riconoscere il difetto senza
/// riversare l'intero vocabolario del progetto nel contesto di ogni turno.
const CAMPIONE_CLASSI: usize = 12;

impl StylingVerdict {
    /// Etichetta stabile per il consumatore macchina.
    pub fn key(&self) -> &'static str {
        match self {
            Self::NonApplicabile => "non_applicabile",
            Self::NessunoStileDichiarato => "nessuno_stile_dichiarato",
            Self::StileApplicato { .. } => "stile_applicato",
            Self::StileDichiaratoNonApplicato { .. } => "stile_dichiarato_non_applicato",
            Self::VocabolarioAssente => "vocabolario_assente",
            Self::NonConcludente { .. } => "non_concludente",
        }
    }

    /// Il verdetto vale un rilievo BLOCCANTE? Solo il difetto vero: le altre
    /// varianti sono osservazioni, e trattarle come veti riporterebbe i rimandi
    /// a vuoto che questa lente deve evitare.
    pub fn e_bloccante(&self) -> bool {
        matches!(self, Self::StileDichiaratoNonApplicato { .. })
    }
}

/// Il nome del criterio con cui il final gate interroga questa lente.
///
/// La costante vive QUI, accanto al criterio che nomina, e non nel nodo che la
/// consuma: il tipo di un criterio e la logica che lo giudica sono la stessa
/// cosa vista da due lati, e due letterali uguali in due crate divergono al
/// primo rinominamento (regola L).
///
/// PERCHE' UN CRITERIO DI GATE e non solo un tool. Questa lente esisteva gia',
/// completa e testata, ed era senza effetto sulla chiusura di un run: era
/// offerta come tool a due figure, e nessun nodo del grafo la interrogava.
/// MISURATO il 06/08/2026 su agenda-medica, e il tool era perfino stato CHIAMATO
/// dall'agente nel run: i componenti scrivevano `min-h-screen bg-gray-50`,
/// `max-w-7xl mx-auto`, `tailwindcss` era in `package.json` — e non esisteva
/// alcuna `tailwind.config`, nessun file `.css`, nessun import di foglio. La
/// pagina servita era HTML grezzo (verificata dal browser), e il run si e'
/// chiuso «completato». Una misura che nessun gate interroga si e' costruita,
/// non e' entrata in esercizio.
pub const CRITERION_TYPE: &str = "ui_styling";

// ─────────────────────────────────────────────────────────────────────────────
// Il criterio: funzione PURA
// ─────────────────────────────────────────────────────────────────────────────

/// Dato cio' che si e' osservato, lo stile dichiarato e' applicato?
///
/// PUNTO UNICO del criterio (regola L): la figura del consiglio, il revisore di
/// interfaccia e chiunque dopo di loro pongono questa domanda qui, e ottengono
/// la stessa risposta. Pura per costruzione — nessun filesystem, nessun DB —
/// cosi' il criterio si esercita da solo e l'evidenza arriva dal suo produttore.
pub fn classify_styling(ev: &StylingEvidence, voc: &StyleVocabulary) -> StylingVerdict {
    if !voc.e_utilizzabile() {
        return StylingVerdict::VocabolarioAssente;
    }
    if ev.sorgenti_interfaccia == 0 {
        return StylingVerdict::NonApplicabile;
    }

    let (fonti, framework_a_meta) = fonti_attive(ev, voc);
    if !fonti.is_empty() {
        return StylingVerdict::StileApplicato { fonti };
    }

    // Nessuna fonte fra quelle guardate. Prima di dichiarare un difetto, si
    // verifica di aver potuto guardare: un foglio riferito e non esaminato puo'
    // essere proprio la fonte mancante.
    if !ev.fogli_fuori_radice.is_empty() {
        return StylingVerdict::NonConcludente {
            fogli_non_esaminati: ev.fogli_fuori_radice.clone(),
        };
    }

    // Il codice dichiara abbastanza classi da poter affermare il difetto?
    if ev.classi_dichiarate.len() < voc.min_classi.max(1) {
        return StylingVerdict::NessunoStileDichiarato;
    }

    let causa = match framework_a_meta {
        Some((pacchetto, manca)) => CausaMancata::FrameworkNonConfigurato { pacchetto, manca },
        None if !ev.fogli_raggiunti.is_empty() => CausaMancata::FogliSenzaSelettori {
            fogli: ev.fogli_raggiunti.len(),
        },
        None => CausaMancata::NessunaFonte,
    };

    StylingVerdict::StileDichiaratoNonApplicato {
        classi_orfane: ev.classi_dichiarate.len(),
        campione: ev
            .classi_dichiarate
            .iter()
            .take(CAMPIONE_CLASSI)
            .cloned()
            .collect(),
        causa,
    }
}

/// Le fonti ATTIVE, e — se ne resta uno a meta' — il framework installato ma
/// non abilitato, che non e' una fonte ma e' la causa piu' utile da riportare.
///
/// Separata dal verdetto perche' risponde a una domanda diversa: qui si guarda
/// cosa C'E', li' si decide cosa significa che non ci sia niente.
fn fonti_attive(
    ev: &StylingEvidence,
    voc: &StyleVocabulary,
) -> (Vec<StyleSource>, Option<(String, String)>) {
    let mut fonti: Vec<StyleSource> = Vec::new();

    // (a) Fogli che coprono davvero cio' che il codice usa. Un foglio raggiunto
    //     che non definisce nessuna classe usata NON e' una fonte per queste
    //     classi: e' il caso del `index.css` col solo reset, sotto un albero di
    //     componenti pieno di utility di un framework assente.
    //     Si nomina il primo foglio, non tutti: la prova e' la COPERTURA, e
    //     l'elenco completo costerebbe contesto a ogni turno senza aggiungerla.
    if !ev.classi_con_selettore.is_empty() {
        if let Some(foglio) = ev.fogli_raggiunti.first() {
            fonti.push(StyleSource::FoglioApplicato {
                file: foglio.clone(),
                classi_coperte: ev.classi_con_selettore.len(),
            });
        }
    }

    // (b) Stili scritti dentro il componente. La convenzione piu' diffusa di Vue
    //     e Svelte: nessun import da seguire, il compilatore del framework li
    //     estrae. Cercarli fra i fogli non li troverebbe mai, e l'assenza
    //     verrebbe scambiata per il difetto — su OGNI progetto scritto cosi'.
    if let Some(file) = ev.sorgenti_con_stile_interno.first() {
        fonti.push(StyleSource::StileNelComponente { file: file.clone() });
    }

    // (c) Framework di utility: installato E abilitato.
    let (abilitati, a_meta) = framework_installati(ev, voc);
    fonti.extend(abilitati);

    // (d) Librerie che stilano da sole: la presenza in manifest basta.
    for pkg in &voc.runtime_packages {
        if ev.dipendenze.contains(pkg) {
            fonti.push(StyleSource::LibreriaRuntime {
                pacchetto: pkg.clone(),
            });
        }
    }

    // (e) Stile inline: povero, ma applicato.
    if ev.usa_stile_inline {
        fonti.push(StyleSource::StileInline);
    }

    (fonti, a_meta)
}

/// Fra i framework di utility INSTALLATI, quali sono anche abilitati (e quindi
/// fonti) e — se ce n'e' uno — quale resta a meta'.
///
/// Se ne riporta uno solo a meta': la causa serve a dire cosa fare, e un elenco
/// di framework mezzi installati non e' un caso reale.
fn framework_installati(
    ev: &StylingEvidence,
    voc: &StyleVocabulary,
) -> (Vec<StyleSource>, Option<(String, String)>) {
    let mut abilitati = Vec::new();
    let mut a_meta: Option<(String, String)> = None;
    for fw in &voc.utility_frameworks {
        if !ev.dipendenze.contains(&fw.pacchetto) {
            continue;
        }
        match prova_di_abilitazione(ev, fw) {
            Some(prova) => abilitati.push(StyleSource::FrameworkUtility {
                pacchetto: fw.pacchetto.clone(),
                prova,
            }),
            None if a_meta.is_none() => {
                a_meta = Some((fw.pacchetto.clone(), descrivi_cosa_manca(fw)));
            }
            None => {}
        }
    }
    (abilitati, a_meta)
}

/// Le due strade con cui un framework di utility risulta abilitato: il file di
/// configurazione, oppure la direttiva dentro un foglio raggiunto (la v4 di
/// Tailwind non ha piu' un file di config, e pretenderlo boccerebbe un progetto
/// corretto solo perche' aggiornato).
fn prova_di_abilitazione(ev: &StylingEvidence, fw: &UtilityFramework) -> Option<String> {
    ev.config_per_pacchetto
        .get(&fw.pacchetto)
        .map(|c| format!("configurazione '{c}'"))
        .or_else(|| {
            ev.direttive_per_pacchetto
                .get(&fw.pacchetto)
                .map(|d| format!("direttiva '{d}' in un foglio raggiunto"))
        })
}

/// Cosa manca perche' un framework installato sia abilitato, detto con i nomi
/// che chi corregge deve cercare.
fn descrivi_cosa_manca(fw: &UtilityFramework) -> String {
    let mut parti: Vec<String> = Vec::new();
    if !fw.config_attesi.is_empty() {
        parti.push(format!("nessuno fra {}", fw.config_attesi.join(", ")));
    }
    if !fw.direttive_attese.is_empty() {
        parti.push(format!(
            "nessuna direttiva {} in un foglio raggiunto",
            fw.direttive_attese.join(" / ")
        ));
    }
    if parti.is_empty() {
        return "configurazione non trovata".to_string();
    }
    parti.join("; ")
}

// ─────────────────────────────────────────────────────────────────────────────
// Raccolta dei fatti: l'unico confine con il filesystem
// ─────────────────────────────────────────────────────────────────────────────

/// Profondita' massima del walk. Un progetto frontend tiene i sorgenti molto
/// piu' in alto; oltre, si paga tempo su alberi che non rispondono alla domanda.
const MAX_DEPTH: usize = 12;

/// Tetto ai file letti per categoria: la risposta non migliora leggendo il
/// millesimo componente, e un run non deve poter fermarsi qui.
const MAX_FILE_PER_CATEGORIA: usize = 400;

/// Classi letterali in `className="..."` / `class="..."` / `className={"..."}`.
/// Solo le LETTERALI: `className={cn(x)}` non dice quali classi finiranno nel
/// DOM, e contarlo come dichiarazione produrrebbe un rilievo su un'incognita.
static RE_CLASSI: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?:className|class)\s*=\s*\{?\s*["'`]([^"'`]*)["'`]"#)
        .expect("regex classi letterali valida")
});

/// Selettori di classe dentro un pezzo di CSS: `.nome`. Vale per un foglio
/// esterno come per un blocco `<style>` interno — il CSS e' lo stesso, cambia
/// solo la strada con cui arriva al browser.
static RE_SELETTORI: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\.([A-Za-z_][A-Za-z0-9_-]*)").expect("regex selettori valida"));

/// Import/`@import`/`href` di un foglio. Copre le tre strade con cui un foglio
/// entra davvero nell'app: modulo che lo importa, foglio che ne importa un
/// altro, HTML che lo collega.
static RE_RIFERIMENTI_FOGLIO: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?:import\s+(?:[^"'`]*\s+from\s+)?|@import\s+(?:url\()?|href\s*=\s*)["']([^"']+)["']"#)
        .expect("regex riferimenti a foglio valida")
});

/// Attributo di stile inline: `style="..."` o `style={{...}}`.
static RE_STILE_INLINE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"style\s*=\s*[{"']"#).expect("regex stile inline valida"));

/// Blocco `<style>` dentro un sorgente: Vue, Svelte, Astro, HTML. Contenuto
/// catturato per estrarne i selettori, che valgono quanto quelli di un foglio.
static RE_BLOCCO_STYLE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?is)<style[^>]*>(.*?)</style>").expect("regex blocco style valida")
});

/// Impalcatura della raccolta: cio' che si accumula camminando l'albero e che
/// NON fa parte del risultato. I `PathBuf` e l'insieme dei selettori servono
/// solo a rispondere alla domanda finale — «quali classi dichiarate hanno un
/// selettore?» — e tenerli fuori da [`StylingEvidence`] impedisce a un
/// consumatore di confonderli con un fatto sul progetto.
#[derive(Default)]
struct RaccoltaCss {
    classi: BTreeSet<String>,
    selettori: BTreeSet<String>,
    raggiunti: BTreeSet<PathBuf>,
    fuori: BTreeSet<String>,
}

impl RaccoltaCss {
    /// Un riferimento risolto va da una parte o dall'altra: dentro la radice si
    /// legge, fuori si dichiara e basta.
    fn smista(&mut self, riferiti: Vec<FoglioRiferito>) {
        for r in riferiti {
            match r {
                FoglioRiferito::Dentro(p) => {
                    self.raggiunti.insert(p);
                }
                FoglioRiferito::Fuori(s) => {
                    self.fuori.insert(s);
                }
            }
        }
    }
}

/// Raccoglie i fatti sotto `root`. UNICO punto che tocca il filesystem: cio'
/// che ne esce e' [`StylingEvidence`], che il criterio giudica senza sapere da
/// dove viene.
pub async fn collect_evidence(root: &Path, voc: &StyleVocabulary) -> StylingEvidence {
    let mut ev = StylingEvidence::default();
    let (sorgenti, fogli) = raccogli_percorsi(root, voc);
    let mut css = RaccoltaCss::default();

    leggi_manifest(root, &mut ev).await;
    ev.config_per_pacchetto = configurazioni_presenti(root, voc);
    scansiona_sorgenti(root, &sorgenti, voc, &mut ev, &mut css).await;
    espandi_fogli_transitivi(root, voc, &mut css).await;
    scansiona_fogli_raggiunti(root, voc, &mut ev, &mut css).await;

    // I fogli che nessuno raggiunge: stile che sembra esserci e non arriva mai.
    ev.fogli_orfani = fogli
        .iter()
        .filter(|f| !css.raggiunti.contains(*f))
        .map(|f| rel(root, f))
        .collect();
    ev.fogli_fuori_radice = css.fuori.into_iter().collect();
    ev.classi_con_selettore = css
        .classi
        .iter()
        .filter(|c| css.selettori.contains(*c))
        .cloned()
        .collect();
    ev.classi_dichiarate = css.classi.into_iter().collect();
    ev.fogli_raggiunti.sort();
    ev.fogli_orfani.sort();
    ev
}

/// Dipendenze dichiarate dal manifest. `manifest_letto` distingue «non c'e'
/// quella dipendenza» da «non ho letto alcun manifest»: la seconda non e'
/// un'osservazione sul progetto (regola O).
async fn leggi_manifest(root: &Path, ev: &mut StylingEvidence) {
    let Ok(txt) = fs::read_to_string(root.join(MANIFEST_NPM)).await else {
        return;
    };
    let Ok(pkg) = serde_json::from_str::<Value>(&txt) else {
        return;
    };
    ev.manifest_letto = true;
    for campo in CAMPI_DIPENDENZE {
        if let Some(map) = pkg.get(campo).and_then(Value::as_object) {
            ev.dipendenze.extend(map.keys().cloned());
        }
    }
}

/// Manifest da cui si leggono le dipendenze dichiarate.
const MANIFEST_NPM: &str = "package.json";

/// I campi del manifest che valgono come «installato». Entrambi contano: un
/// framework di build sta quasi sempre in `devDependencies`, e guardarne uno
/// solo direbbe «non installato» del caso piu' comune.
const CAMPI_DIPENDENZE: [&str; 2] = ["dependencies", "devDependencies"];

/// File di configurazione dei framework presenti nella radice: uno basta per
/// pacchetto, ed e' una delle due prove che il framework e' abilitato.
fn configurazioni_presenti(root: &Path, voc: &StyleVocabulary) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for fw in &voc.utility_frameworks {
        if let Some(trovato) = fw.config_attesi.iter().find(|c| root.join(c).exists()) {
            out.insert(fw.pacchetto.clone(), trovato.clone());
        }
    }
    out
}

/// Cosa DICHIARANO i sorgenti: classi letterali, stile inline, stili scritti
/// dentro il componente e i fogli che fanno entrare nell'app.
async fn scansiona_sorgenti(
    root: &Path,
    sorgenti: &[PathBuf],
    voc: &StyleVocabulary,
    ev: &mut StylingEvidence,
    css: &mut RaccoltaCss,
) {
    for file in sorgenti.iter().take(MAX_FILE_PER_CATEGORIA) {
        let Ok(testo) = fs::read_to_string(file).await else {
            continue;
        };
        ev.sorgenti_interfaccia += 1;
        for cap in RE_CLASSI.captures_iter(&testo) {
            if let Some(gruppo) = cap.get(1) {
                css.classi.extend(classi_letterali(gruppo.as_str()));
            }
        }
        if RE_STILE_INLINE.is_match(&testo) {
            ev.usa_stile_inline = true;
        }
        if raccogli_stile_interno(&testo, &mut css.selettori) {
            ev.sorgenti_con_stile_interno.push(rel(root, file));
        }
        css.smista(fogli_riferiti(file, &testo, root, voc));
    }
}

/// Selettori dei blocchi `<style>` NON vuoti di un sorgente. Ritorna `true` se
/// il file porta stile al suo interno: un blocco vuoto non e' una fonte, e i
/// generatori di scaffold lo scrivono vuoto.
fn raccogli_stile_interno(testo: &str, selettori: &mut BTreeSet<String>) -> bool {
    let mut trovato = false;
    for cap in RE_BLOCCO_STYLE.captures_iter(testo) {
        let Some(corpo) = cap.get(1) else { continue };
        if corpo.as_str().trim().is_empty() {
            continue;
        }
        trovato = true;
        selettori.extend(selettori_di_classe(corpo.as_str()));
    }
    trovato
}

/// Un foglio raggiunto puo' importarne altri. UN livello basta a coprire il caso
/// ordinario (`index.css` che importa i parziali) senza inseguire catene
/// arbitrarie, che costerebbero letture senza cambiare quasi mai la risposta.
async fn espandi_fogli_transitivi(root: &Path, voc: &StyleVocabulary, css: &mut RaccoltaCss) {
    let diretti: Vec<PathBuf> = css.raggiunti.iter().cloned().collect();
    for foglio in &diretti {
        if let Ok(testo) = fs::read_to_string(foglio).await {
            css.smista(fogli_riferiti(foglio, &testo, root, voc));
        }
    }
}

/// Cosa DEFINISCONO i fogli che l'app carica davvero: selettori di classe e
/// direttive di framework (l'altra prova di abilitazione, quella senza file di
/// configurazione).
async fn scansiona_fogli_raggiunti(
    root: &Path,
    voc: &StyleVocabulary,
    ev: &mut StylingEvidence,
    css: &mut RaccoltaCss,
) {
    for foglio in css.raggiunti.iter().take(MAX_FILE_PER_CATEGORIA) {
        let Ok(testo) = fs::read_to_string(foglio).await else {
            continue;
        };
        ev.fogli_raggiunti.push(rel(root, foglio));
        css.selettori.extend(selettori_di_classe(&testo));
        for fw in &voc.utility_frameworks {
            if ev.direttive_per_pacchetto.contains_key(&fw.pacchetto) {
                continue;
            }
            if let Some(d) = fw.direttive_attese.iter().find(|d| testo.contains(*d)) {
                ev.direttive_per_pacchetto
                    .insert(fw.pacchetto.clone(), d.clone());
            }
        }
    }
}

/// Percorsi dei sorgenti di interfaccia e dei fogli sotto `root`, in un walk
/// solo. Delega le esclusioni al punto unico `nexus_tool_kit::is_skipped_dir`:
/// una lista propria divergerebbe, e un `node_modules` visitato qui vorrebbe
/// dire leggere decine di migliaia di file per una domanda che non li riguarda.
fn raccogli_percorsi(root: &Path, voc: &StyleVocabulary) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut sorgenti: Vec<PathBuf> = Vec::new();
    let mut fogli: Vec<PathBuf> = Vec::new();
    nexus_tool_kit::fs_scan::walk_project_with(
        root,
        MAX_DEPTH,
        &|name: &str| nexus_tool_kit::is_skipped_dir(name),
        &mut |path: &Path, name: &str| {
            let n = name.to_ascii_lowercase();
            if voc.source_suffixes.iter().any(|s| n.ends_with(s)) {
                sorgenti.push(path.to_path_buf());
            } else if voc.stylesheet_suffixes.iter().any(|s| n.ends_with(s)) {
                fogli.push(path.to_path_buf());
            }
        },
    );
    (sorgenti, fogli)
}

/// I nomi di classe definiti da un pezzo di CSS. Punto unico dell'estrazione
/// (regola L): la usano sia i fogli esterni sia i blocchi `<style>` interni, e
/// due estrazioni diverse darebbero due idee diverse di "questa classe e'
/// definita" a seconda di dove sta scritta.
fn selettori_di_classe(css: &str) -> Vec<String> {
    RE_SELETTORI
        .captures_iter(css)
        .filter_map(|c| c.get(1).map(|g| g.as_str().to_string()))
        .collect()
}

/// Le classi letterali di un attributo: si scartano i frammenti che contengono
/// interpolazione (`${...}`, `{...}`), perche' li' il nome della classe non e'
/// deciso nel sorgente.
fn classi_letterali(grezzo: &str) -> Vec<String> {
    grezzo
        .split_whitespace()
        .filter(|t| !t.contains('{') && !t.contains('}') && !t.contains('$'))
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// Esito della risoluzione di un riferimento a foglio.
enum FoglioRiferito {
    /// Risolto dentro la radice esaminata, e il file esiste.
    Dentro(PathBuf),
    /// Esiste ma sta fuori dalla radice esaminata: non si legge, si dichiara.
    Fuori(String),
}

/// I fogli che `testo` (contenuto di `file`) fa entrare nell'app, risolti su
/// disco. Un riferimento che non risolve non e' un foglio raggiunto: e' un
/// import rotto, e contarlo come fonte direbbe «stilizzata» a una pagina che
/// non carica nulla.
fn fogli_riferiti(
    file: &Path,
    testo: &str,
    root: &Path,
    voc: &StyleVocabulary,
) -> Vec<FoglioRiferito> {
    let Some(base) = file.parent() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for cap in RE_RIFERIMENTI_FOGLIO.captures_iter(testo) {
        let Some(g) = cap.get(1) else { continue };
        let riferimento = g.as_str();
        let pulito = riferimento.split(['?', '#']).next().unwrap_or(riferimento);
        let minuscolo = pulito.to_ascii_lowercase();
        if !voc.stylesheet_suffixes.iter().any(|s| minuscolo.ends_with(s)) {
            continue;
        }
        // Assoluto rispetto alla radice servita (`/src/index.css` in un HTML) o
        // relativo al file che lo nomina.
        let grezzo = match pulito.strip_prefix('/') {
            Some(resto) => root.join(resto),
            None => base.join(pulito),
        };
        let candidato = normalizza_lessicale(&grezzo);
        if !candidato.is_file() {
            continue;
        }
        // `..` che esce dalla radice esaminata: legittimo nel progetto, ma qui
        // non lo si puo' leggere. Dichiararlo e' cio' che impedisce a un
        // «nessuna fonte» di nascere da un'assenza di osservazione.
        if nexus_types::workspace_paths::path_within(root, &candidato) {
            out.push(FoglioRiferito::Dentro(candidato));
        } else {
            out.push(FoglioRiferito::Fuori(
                candidato.to_string_lossy().replace('\\', "/"),
            ));
        }
    }
    out
}

/// Risolve `.` e `..` LESSICALMENTE, senza toccare il filesystem.
///
/// Non delega a `workspace_paths::normalize_into_root`, che risponde a un'altra
/// domanda: li' un `..` e' un tentativo di traversal da RIFIUTARE, perche' il
/// path arriva dall'agente. Qui il path arriva dal codice del progetto, dove
/// `../styles/app.css` e' un import ordinario: rifiutarlo scarterebbe fonti
/// vere e produrrebbe il difetto che il modulo esiste per non inventare.
///
/// Senza questa normalizzazione lo stesso foglio, riferito come `./index.css` da
/// un file e come `index.css` da un altro, sarebbe DUE percorsi distinti: letto
/// due volte, contato due volte, e riportato all'agente con un `./` in mezzo al
/// percorso che deve riaprire.
fn normalizza_lessicale(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                // Un `..` in testa (nessun componente da togliere) va tenuto:
                // scartarlo cambierebbe il percorso in uno che punta altrove.
                if !out.pop() {
                    out.push("..");
                }
            }
            altro => out.push(altro.as_os_str()),
        }
    }
    out
}

/// Percorso relativo alla radice, con separatori uniformi: e' cio' che l'agente
/// deve poter riaprire.
fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

// ─────────────────────────────────────────────────────────────────────────────
// Vocabolario dal DB
// ─────────────────────────────────────────────────────────────────────────────

const K_SOURCE: &str = "agent.ui_styling.source_suffixes";
const K_SHEET: &str = "agent.ui_styling.stylesheet_suffixes";
const K_FRAMEWORKS: &str = "agent.ui_styling.utility_frameworks";
const K_RUNTIME: &str = "agent.ui_styling.runtime_packages";
const K_MIN_CLASSI: &str = "agent.ui_styling.min_classi";

/// Carica il vocabolario. Nessun default hardcoded (regola G/H): se le chiavi
/// non ci sono il criterio lo DICE (`VocabolarioAssente`) invece di rispondere
/// «nessuna fonte» a qualunque progetto — che sarebbe il falso positivo peggiore
/// possibile, perche' avrebbe l'aria di una diagnosi.
pub async fn load_vocabulary(db: &PgPool) -> StyleVocabulary {
    StyleVocabulary {
        source_suffixes: csv_minuscolo(db, K_SOURCE).await,
        stylesheet_suffixes: csv_minuscolo(db, K_SHEET).await,
        utility_frameworks: parse_frameworks(&leggi(db, K_FRAMEWORKS).await),
        runtime_packages: csv(db, K_RUNTIME).await,
        min_classi: leggi(db, K_MIN_CLASSI)
            .await
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0),
    }
}

async fn leggi(db: &PgPool, key: &str) -> Option<String> {
    nexus_auth::get_setting_checked(db, key)
        .await
        .ok()
        .flatten()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

async fn csv(db: &PgPool, key: &str) -> Vec<String> {
    leggi(db, key)
        .await
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

async fn csv_minuscolo(db: &PgPool, key: &str) -> Vec<String> {
    csv(db, key)
        .await
        .into_iter()
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

/// Formato di `agent.ui_styling.utility_frameworks`: una riga per framework,
/// `pacchetto|config1,config2|direttiva1,direttiva2`. Le due liste possono
/// essere vuote (un framework che non richiede nulla non e' mai a meta').
pub fn parse_frameworks(grezzo: &Option<String>) -> Vec<UtilityFramework> {
    let Some(testo) = grezzo else {
        return Vec::new();
    };
    testo
        .lines()
        .map(str::trim)
        .filter(|r| !r.is_empty() && !r.starts_with('#'))
        .filter_map(|riga| {
            let mut campi = riga.split('|');
            let pacchetto = campi.next().unwrap_or("").trim();
            if pacchetto.is_empty() {
                return None;
            }
            let lista = |c: Option<&str>| -> Vec<String> {
                c.unwrap_or("")
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            };
            Some(UtilityFramework {
                pacchetto: pacchetto.to_string(),
                config_attesi: lista(campi.next()),
                direttive_attese: lista(campi.next()),
            })
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tool
// ─────────────────────────────────────────────────────────────────────────────

/// `ui_styling_audit` — lo stile dichiarato dal codice e' applicato?
///
/// Input: `{ target_dir?: string }` (default: la radice del progetto; utile nei
/// monorepo dove il frontend e' una sottocartella).
///
/// Sola lettura, nessun effetto. Ritorna il verdetto strutturato, l'evidenza che
/// lo sostiene e il passo successivo: chi legge non deve dedurre nulla dal testo
/// (regola M).
pub async fn tool_ui_styling_audit(ctx: &ToolContextCore, input: &Value) -> String {
    let target_rel = input
        .get("target_dir")
        .and_then(Value::as_str)
        .map(|s| s.trim().trim_start_matches('/'))
        .filter(|s| !s.is_empty())
        .unwrap_or(".");
    let root = match nexus_types::workspace_paths::normalize_into_root(&ctx.root_path, target_rel) {
        Ok(clean) => ctx.root_path.join(&clean),
        Err(e) => return crate::errore_json(format!("target_dir non valido: {}", e.message())),
    };
    if !root.is_dir() {
        return crate::errore_json(format!("target_dir '{target_rel}' non esiste nel progetto"));
    }

    let voc = load_vocabulary(&ctx.db).await;
    if !voc.e_utilizzabile() {
        return json!({
            "verdict": StylingVerdict::VocabolarioAssente.key(),
            "error": format!(
                "vocabolario di riconoscimento non configurato ({K_SOURCE} / {K_SHEET}): \
                 applicare la migrazione che lo introduce prima di usare questo tool"
            ),
        })
        .to_string();
    }

    let ev = collect_evidence(&root, &voc).await;
    let verdict = classify_styling(&ev, &voc);
    render(&verdict, &ev, target_rel)
}

/// Serializza verdetto ed evidenza. Il campo `next_step` non e' cortesia: e'
/// cio' che trasforma un rilievo in una correzione, ed e' la ragione per cui il
/// verdetto porta la CAUSA e non solo l'esito.
/// La stessa evidenza di [`render`], come `Value` per il final gate.
///
/// Delega invece di ricomporre il payload: il gate e il tool devono dire la
/// STESSA cosa dello stesso progetto — con due composizioni, l'agente leggerebbe
/// dal tool una diagnosi e dal rimando del gate un'altra, e non saprebbe quale
/// delle due correggere. `target_dir` e' la radice del run, che qui e' sempre
/// quella: il gate non ha un sotto-albero da nominare.
pub fn evidenza_criterio(verdict: &StylingVerdict, ev: &StylingEvidence) -> Value {
    serde_json::from_str(&render(verdict, ev, ".")).unwrap_or_else(|_| {
        // Irraggiungibile: `render` compone un oggetto JSON. Se mai accadesse,
        // l'esito del criterio resta quello gia' deciso e qui si perde solo il
        // dettaglio, dichiarandolo.
        json!({ "verdict": verdict.key(), "evidenza": "non serializzabile" })
    })
}

fn render(verdict: &StylingVerdict, ev: &StylingEvidence, target_rel: &str) -> String {
    let mut out = json!({
        "verdict": verdict.key(),
        "bloccante": verdict.e_bloccante(),
        "target_dir": target_rel,
        "evidenza": {
            "sorgenti_interfaccia": ev.sorgenti_interfaccia,
            "classi_dichiarate": ev.classi_dichiarate.len(),
            "classi_con_selettore": ev.classi_con_selettore.len(),
            "fogli_raggiunti": ev.fogli_raggiunti,
            "fogli_orfani": ev.fogli_orfani,
            "fogli_fuori_radice": ev.fogli_fuori_radice,
            "sorgenti_con_stile_interno": ev.sorgenti_con_stile_interno,
            "manifest_letto": ev.manifest_letto,
            "stile_inline": ev.usa_stile_inline,
        },
    });

    let (dettaglio, next) = dettaglio_e_passo(verdict);
    if let (Some(o), Some(d)) = (out.as_object_mut(), dettaglio.as_object()) {
        o.extend(d.clone());
    }
    if let Some(o) = out.as_object_mut() {
        o.insert("next_step".to_string(), json!(next));
    }
    out.to_string()
}

// I passi successivi, uno per verdetto. Stanno in costanti e non dentro il
// `match` perche' sono il CONTRATTO verso l'agente — cio' che fara' dopo — e un
// contratto si legge tutto insieme; sotto resta una tabella verdetto -> payload.

const PASSO_NON_APPLICABILE: &str =
    "Nessun sorgente di interfaccia sotto target_dir: la domanda non si pone. Se il frontend \
     e' in una sottocartella, richiama con target_dir.";

const PASSO_NESSUNO_STILE: &str =
    "Il codice non dichiara classi e non c'e' una fonte di stile: la pagina sara' grezza, ma il \
     codice non promette nulla che non mantenga. Suggerimento, non difetto bloccante: proporre \
     un sistema di stili se il compito lo giustifica.";

const PASSO_APPLICATO: &str =
    "Lo stile dichiarato ha una fonte che lo applica. Le altre voci della lente (scala \
     tipografica, spaziature, larghezza ridotta, contrasto) restano da valutare.";

const PASSO_NON_APPLICATO: &str =
    "DIFETTO: il codice scrive classi che nulla definisce, quindi la pagina e' grezza mentre il \
     sorgente sembra stilizzato. Correggere alla fonte: installare E configurare il framework di \
     cui il codice usa le utility, oppure fornire un foglio di stile raggiunto dall'app che \
     definisca le classi usate, oppure togliere le classi inerti. Non basta aggiungere la \
     dipendenza: senza la sua configurazione le utility non vengono generate.";

const PASSO_VOCABOLARIO_ASSENTE: &str =
    "Vocabolario di riconoscimento non configurato: nessun verdetto.";

const PASSO_NON_CONCLUDENTE: &str =
    "Nessuna fonte fra quelle esaminate, ma il codice ne riferisce altre fuori da target_dir: non \
     si puo' concludere. Ri-esegui con la radice che le contiene. Nel frattempo NON riportare \
     questo come difetto: non e' un'osservazione sul progetto, e' un'assenza di osservazione.";

/// Cosa aggiungere al payload e cosa FARE, per ciascun verdetto. Il `next_step`
/// non e' cortesia: e' cio' che trasforma un rilievo in una correzione, ed e' la
/// ragione per cui il verdetto porta la CAUSA e non solo l'esito.
fn dettaglio_e_passo(verdict: &StylingVerdict) -> (Value, String) {
    match verdict {
        StylingVerdict::NonApplicabile => (json!({}), PASSO_NON_APPLICABILE.to_string()),
        StylingVerdict::NessunoStileDichiarato => (json!({}), PASSO_NESSUNO_STILE.to_string()),
        StylingVerdict::VocabolarioAssente => (json!({}), PASSO_VOCABOLARIO_ASSENTE.to_string()),
        StylingVerdict::StileApplicato { fonti } => (
            json!({ "fonti": fonti.iter().map(StyleSource::to_value).collect::<Vec<_>>() }),
            PASSO_APPLICATO.to_string(),
        ),
        StylingVerdict::StileDichiaratoNonApplicato {
            classi_orfane,
            campione,
            causa,
        } => (
            json!({
                "classi_orfane": classi_orfane,
                "campione": campione,
                "causa": causa.to_value(),
            }),
            PASSO_NON_APPLICATO.to_string(),
        ),
        StylingVerdict::NonConcludente {
            fogli_non_esaminati,
        } => (
            json!({ "fogli_non_esaminati": fogli_non_esaminati }),
            PASSO_NON_CONCLUDENTE.to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vocabolario di prova: gli stessi valori che la migrazione mette in
    /// `settings`. Non e' un'invenzione del test — se divergesse, il test
    /// misurerebbe un sistema che non esiste (regola O).
    fn voc() -> StyleVocabulary {
        StyleVocabulary {
            source_suffixes: vec![".tsx".into(), ".jsx".into(), ".html".into(), ".vue".into()],
            stylesheet_suffixes: vec![".css".into(), ".scss".into()],
            utility_frameworks: parse_frameworks(&Some(
                "tailwindcss|tailwind.config.js,tailwind.config.ts|@tailwind,@import \"tailwindcss\"\n\
                 unocss|uno.config.ts|@unocss"
                    .to_string(),
            )),
            runtime_packages: vec!["@mui/material".into(), "styled-components".into()],
            min_classi: 3,
        }
    }

    async fn scrivi(root: &Path, rel: &str, contenuto: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).await.expect("mkdir");
        }
        fs::write(&p, contenuto).await.expect("write");
    }

    /// Il componente del caso reale (gestione-spese, 29/07/2026), verbatim nelle
    /// classi che usava.
    const COMPONENTE_REALE: &str = r#"
export default function SpeseList() {
  return (
    <div className="p-4">
      <h1 className="text-xl font-bold mb-4">Spese</h1>
      <div className="p-2 mb-4 text-red-600 bg-red-100 rounded">errore</div>
      <ul className="space-y-4"></ul>
    </div>
  );
}
"#;

    const PKG_SENZA_TAILWIND: &str = r#"{
      "name": "gestione-spese-frontend",
      "dependencies": { "react": "^18.2.0", "react-dom": "^18.2.0" },
      "devDependencies": { "vite": "^5.0.0", "@vitejs/plugin-react": "^4.0.0" }
    }"#;

    /// Radice temporanea isolata per ogni test (regola F: nessuno stato
    /// condiviso, nessun ordine di esecuzione implicito).
    fn radice(nome: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ui_styling_{nome}_{}", uuid::Uuid::new_v4()))
    }

    /// IL caso. Parte da un progetto vero su disco e attraversa il produttore
    /// (`collect_evidence`), non un'evidenza costruita a mano: se un giorno il
    /// riconoscimento delle classi smettesse di funzionare, questo test lo
    /// vedrebbe. Un test che fabbricasse `classi_dichiarate` fisserebbe proprio
    /// l'assunto da verificare (regola O).
    #[tokio::test]
    async fn classi_di_un_framework_assente_sono_stile_non_applicato() {
        let root = radice("assente");
        scrivi(&root, "package.json", PKG_SENZA_TAILWIND).await;
        scrivi(&root, "src/components/SpeseList.tsx", COMPONENTE_REALE).await;
        scrivi(
            &root,
            "src/main.tsx",
            "import React from 'react';\nimport App from './App';\n",
        )
        .await;

        let ev = collect_evidence(&root, &voc()).await;
        assert!(
            ev.classi_dichiarate.len() >= 8,
            "le classi del componente reale devono essere lette: {:?}",
            ev.classi_dichiarate
        );
        assert!(
            ev.classi_con_selettore.is_empty(),
            "nessun foglio le definisce: {:?}",
            ev.classi_con_selettore
        );

        let v = classify_styling(&ev, &voc());
        match v {
            StylingVerdict::StileDichiaratoNonApplicato {
                classi_orfane,
                ref causa,
                ..
            } => {
                assert!(classi_orfane >= 8, "classi orfane: {classi_orfane}");
                assert_eq!(*causa, CausaMancata::NessunaFonte);
            }
            altro => panic!("atteso stile dichiarato non applicato, ottenuto {altro:?}"),
        }
        assert!(v.e_bloccante(), "il difetto vale un rilievo bloccante");

        let _ = fs::remove_dir_all(&root).await;
    }

    /// Mezzo installato: la dipendenza c'e' e fa da alibi, ma senza
    /// configurazione le utility non vengono generate. La causa deve NOMINARE il
    /// pacchetto, altrimenti chi corregge aggiunge la dipendenza (che c'e' gia')
    /// e il rimando torna indietro identico.
    #[tokio::test]
    async fn framework_installato_ma_non_configurato_e_una_causa_distinta() {
        let root = radice("meta");
        scrivi(
            &root,
            "package.json",
            r#"{"dependencies":{"react":"^18"},"devDependencies":{"tailwindcss":"^3.4.0"}}"#,
        )
        .await;
        scrivi(&root, "src/App.tsx", COMPONENTE_REALE).await;

        let ev = collect_evidence(&root, &voc()).await;
        assert!(ev.dipendenze.contains("tailwindcss"));

        match classify_styling(&ev, &voc()) {
            StylingVerdict::StileDichiaratoNonApplicato { causa, .. } => match causa {
                CausaMancata::FrameworkNonConfigurato { pacchetto, manca } => {
                    assert_eq!(pacchetto, "tailwindcss");
                    assert!(
                        manca.contains("tailwind.config.js"),
                        "la causa deve dire cosa cercare: {manca}"
                    );
                }
                altro => panic!("attesa causa framework non configurato, ottenuta {altro:?}"),
            },
            altro => panic!("atteso stile non applicato, ottenuto {altro:?}"),
        }

        let _ = fs::remove_dir_all(&root).await;
    }

    /// Installato E configurato: nessun rilievo. E' il controllo che impedisce
    /// al criterio di bocciare i progetti sani — senza, sarebbe un generatore di
    /// rimandi a vuoto.
    #[tokio::test]
    async fn framework_installato_e_configurato_e_stile_applicato() {
        let root = radice("ok");
        scrivi(
            &root,
            "package.json",
            r#"{"devDependencies":{"tailwindcss":"^3.4.0"}}"#,
        )
        .await;
        scrivi(&root, "tailwind.config.js", "module.exports = {};\n").await;
        scrivi(&root, "src/App.tsx", COMPONENTE_REALE).await;

        match classify_styling(&collect_evidence(&root, &voc()).await, &voc()) {
            StylingVerdict::StileApplicato { fonti } => assert!(
                fonti.iter().any(|f| matches!(
                    f,
                    StyleSource::FrameworkUtility { pacchetto, .. } if pacchetto == "tailwindcss"
                )),
                "fonti: {fonti:?}"
            ),
            altro => panic!("atteso stile applicato, ottenuto {altro:?}"),
        }

        let _ = fs::remove_dir_all(&root).await;
    }

    /// La v4 di Tailwind non ha `tailwind.config`: la direttiva nel foglio
    /// raggiunto e' l'altra prova valida. Senza questo ramo il criterio
    /// boccerebbe un progetto corretto solo perche' aggiornato.
    #[tokio::test]
    async fn direttiva_in_un_foglio_raggiunto_vale_come_configurazione() {
        let root = radice("direttiva");
        scrivi(
            &root,
            "package.json",
            r#"{"devDependencies":{"tailwindcss":"^4.0.0"}}"#,
        )
        .await;
        scrivi(&root, "src/index.css", "@import \"tailwindcss\";\n").await;
        scrivi(
            &root,
            "src/main.tsx",
            "import './index.css';\nimport App from './App';\n",
        )
        .await;
        scrivi(&root, "src/App.tsx", COMPONENTE_REALE).await;

        let ev = collect_evidence(&root, &voc()).await;
        assert_eq!(
            ev.fogli_raggiunti,
            vec!["src/index.css".to_string()],
            "il foglio importato da main.tsx deve risultare raggiunto"
        );
        assert!(matches!(
            classify_styling(&ev, &voc()),
            StylingVerdict::StileApplicato { .. }
        ));

        let _ = fs::remove_dir_all(&root).await;
    }

    /// Un foglio che c'e' ma che nessuno importa non applica niente. E' il
    /// difetto gemello: sul disco lo stile sembra esserci, nel browser no.
    #[tokio::test]
    async fn un_foglio_mai_importato_non_e_una_fonte() {
        let root = radice("orfano");
        scrivi(&root, "package.json", PKG_SENZA_TAILWIND).await;
        scrivi(
            &root,
            "src/styles.css",
            ".p-4 { padding: 1rem; }\n.text-xl { font-size: 1.25rem; }\n",
        )
        .await;
        scrivi(&root, "src/App.tsx", COMPONENTE_REALE).await;

        let ev = collect_evidence(&root, &voc()).await;
        assert!(ev.fogli_raggiunti.is_empty(), "{:?}", ev.fogli_raggiunti);
        assert_eq!(ev.fogli_orfani, vec!["src/styles.css".to_string()]);
        assert!(
            ev.classi_con_selettore.is_empty(),
            "i selettori di un foglio orfano non coprono nulla: {:?}",
            ev.classi_con_selettore
        );

        assert!(matches!(
            classify_styling(&ev, &voc()),
            StylingVerdict::StileDichiaratoNonApplicato {
                causa: CausaMancata::NessunaFonte,
                ..
            }
        ));

        let _ = fs::remove_dir_all(&root).await;
    }

    /// Un foglio raggiunto che NON definisce nessuna delle classi usate non e'
    /// una fonte per quelle classi. Senza questa distinzione basterebbe un
    /// `index.css` col solo reset per dichiarare stilizzata un'app di sole
    /// utility inerti — il caso reale a un `import` di distanza.
    #[tokio::test]
    async fn foglio_raggiunto_che_non_copre_le_classi_usate_non_assolve() {
        let root = radice("resetonly");
        scrivi(&root, "package.json", PKG_SENZA_TAILWIND).await;
        scrivi(
            &root,
            "src/index.css",
            "* { margin: 0; }\nbody { font-family: sans-serif; }\n",
        )
        .await;
        scrivi(
            &root,
            "src/main.tsx",
            "import './index.css';\nimport App from './App';\n",
        )
        .await;
        scrivi(&root, "src/App.tsx", COMPONENTE_REALE).await;

        let ev = collect_evidence(&root, &voc()).await;
        assert_eq!(ev.fogli_raggiunti, vec!["src/index.css".to_string()]);
        assert!(ev.classi_con_selettore.is_empty());

        match classify_styling(&ev, &voc()) {
            StylingVerdict::StileDichiaratoNonApplicato { causa, .. } => assert_eq!(
                causa,
                CausaMancata::FogliSenzaSelettori { fogli: 1 },
                "la causa deve dire che i fogli ci sono ma non coprono"
            ),
            altro => panic!("atteso stile non applicato, ottenuto {altro:?}"),
        }

        let _ = fs::remove_dir_all(&root).await;
    }

    /// Un foglio che definisce le classi usate e' una fonte, senza bisogno di
    /// alcun framework: il criterio non pretende un framework, pretende che le
    /// classi abbiano un'origine.
    #[tokio::test]
    async fn css_scritto_a_mano_che_definisce_le_classi_e_una_fonte() {
        let root = radice("cssmano");
        scrivi(&root, "package.json", PKG_SENZA_TAILWIND).await;
        scrivi(
            &root,
            "src/App.tsx",
            "import './app.css';\nexport default () => <div className=\"pannello titolo elenco\" />;\n",
        )
        .await;
        scrivi(
            &root,
            "src/app.css",
            ".pannello { padding: 1rem; }\n.titolo { font-size: 2rem; }\n.elenco { display: grid; }\n",
        )
        .await;

        let ev = collect_evidence(&root, &voc()).await;
        assert_eq!(ev.classi_con_selettore.len(), 3, "{:?}", ev);
        assert!(matches!(
            classify_styling(&ev, &voc()),
            StylingVerdict::StileApplicato { .. }
        ));

        let _ = fs::remove_dir_all(&root).await;
    }

    /// Una libreria che stila da sola basta: `@mui/material` non ha bisogno di
    /// fogli ne' di configurazione.
    #[tokio::test]
    async fn libreria_runtime_e_una_fonte_senza_configurazione() {
        let root = radice("mui");
        scrivi(
            &root,
            "package.json",
            r#"{"dependencies":{"@mui/material":"^5.0.0"}}"#,
        )
        .await;
        scrivi(
            &root,
            "src/App.tsx",
            "import Button from '@mui/material/Button';\nexport default () => <Button>ok</Button>;\n",
        )
        .await;

        assert!(matches!(
            classify_styling(&collect_evidence(&root, &voc()).await, &voc()),
            StylingVerdict::StileApplicato { .. }
        ));

        let _ = fs::remove_dir_all(&root).await;
    }

    /// Nessuna classe e nessuna fonte: grezzo ma onesto. Il codice non promette
    /// uno stile che non c'e', e trattarlo come il difetto vero produrrebbe un
    /// rimando su ogni prototipo di due righe.
    #[tokio::test]
    async fn nessuna_classe_e_nessuna_fonte_non_e_il_difetto() {
        let root = radice("grezzo");
        scrivi(&root, "package.json", PKG_SENZA_TAILWIND).await;
        scrivi(
            &root,
            "src/App.tsx",
            "export default () => <div><h1>Spese</h1></div>;\n",
        )
        .await;

        let v = classify_styling(&collect_evidence(&root, &voc()).await, &voc());
        assert_eq!(v, StylingVerdict::NessunoStileDichiarato);
        assert!(!v.e_bloccante());

        let _ = fs::remove_dir_all(&root).await;
    }

    /// Sotto la soglia non si dichiara il difetto: due classi non sono una
    /// prova, e un rilievo su un campione minuscolo insegna a ignorare i rilievi.
    #[tokio::test]
    async fn campione_troppo_piccolo_non_dichiara_il_difetto() {
        let root = radice("soglia");
        scrivi(&root, "package.json", PKG_SENZA_TAILWIND).await;
        scrivi(
            &root,
            "src/App.tsx",
            "export default () => <div className=\"box\"><span className=\"t\" /></div>;\n",
        )
        .await;

        let ev = collect_evidence(&root, &voc()).await;
        assert_eq!(ev.classi_dichiarate.len(), 2);
        assert_eq!(
            classify_styling(&ev, &voc()),
            StylingVerdict::NessunoStileDichiarato
        );

        let _ = fs::remove_dir_all(&root).await;
    }

    /// Niente sorgenti di interfaccia: la domanda non si pone. Un backend puro
    /// non deve ricevere un rilievo di stile.
    #[tokio::test]
    async fn progetto_senza_interfaccia_non_e_applicabile() {
        let root = radice("backend");
        scrivi(&root, "package.json", r#"{"dependencies":{"express":"^4"}}"#).await;
        scrivi(&root, "src/server.ts", "export const app = 1;\n").await;

        assert_eq!(
            classify_styling(&collect_evidence(&root, &voc()).await, &voc()),
            StylingVerdict::NonApplicabile
        );

        let _ = fs::remove_dir_all(&root).await;
    }

    /// Un componente Vue/Svelte con `<style scoped>` porta gli stili DENTRO di
    /// se': nessun import da seguire, li estrae il compilatore del framework.
    /// Cercandoli solo fra i fogli esterni non si troverebbero mai, e l'assenza
    /// verrebbe scambiata per il difetto — su OGNI progetto scritto secondo la
    /// convenzione piu' comune di quei due framework. Un falso positivo
    /// sistematico e' peggio di nessun controllo: insegna a ignorare i rilievi.
    #[tokio::test]
    async fn stili_dentro_il_componente_sono_una_fonte() {
        let root = radice("vue");
        scrivi(&root, "package.json", r#"{"dependencies":{"vue":"^3.4.0"}}"#).await;
        scrivi(
            &root,
            "src/SpeseList.vue",
            "<template>\n  <div class=\"pannello\"><h1 class=\"titolo\">Spese</h1>\n\
             <ul class=\"elenco\"><li class=\"voce\">a</li></ul></div>\n</template>\n\
             <style scoped>\n.pannello { padding: 1rem; }\n.titolo { font-size: 2rem; }\n\
             .elenco { display: grid; }\n.voce { padding: .5rem; }\n</style>\n",
        )
        .await;

        let ev = collect_evidence(&root, &voc()).await;
        assert_eq!(
            ev.sorgenti_con_stile_interno,
            vec!["src/SpeseList.vue".to_string()],
            "il blocco <style> del componente va riconosciuto"
        );
        assert_eq!(
            ev.classi_con_selettore.len(),
            4,
            "i selettori del blocco interno coprono le classi usate: {ev:?}"
        );

        match classify_styling(&ev, &voc()) {
            StylingVerdict::StileApplicato { fonti } => assert!(
                fonti
                    .iter()
                    .any(|f| matches!(f, StyleSource::StileNelComponente { .. })),
                "fonti: {fonti:?}"
            ),
            altro => panic!("atteso stile applicato, ottenuto {altro:?}"),
        }

        let _ = fs::remove_dir_all(&root).await;
    }

    /// Contro-prova del caso sopra: un `<style>` VUOTO non e' una fonte. Senza
    /// questo controllo basterebbe il tag per assolvere un componente le cui
    /// classi restano inerti — e i generatori di scaffold lo scrivono vuoto.
    #[tokio::test]
    async fn un_blocco_style_vuoto_non_assolve() {
        let root = radice("vuevuoto");
        scrivi(&root, "package.json", r#"{"dependencies":{"vue":"^3.4.0"}}"#).await;
        scrivi(
            &root,
            "src/App.vue",
            "<template>\n  <div class=\"p-4\"><h1 class=\"text-xl font-bold mb-4\">x</h1>\n\
             <ul class=\"space-y-4 rounded\"></ul></div>\n</template>\n<style scoped>\n</style>\n",
        )
        .await;

        let ev = collect_evidence(&root, &voc()).await;
        assert!(
            ev.sorgenti_con_stile_interno.is_empty(),
            "un blocco vuoto non e' stile: {:?}",
            ev.sorgenti_con_stile_interno
        );
        assert!(matches!(
            classify_styling(&ev, &voc()),
            StylingVerdict::StileDichiaratoNonApplicato { .. }
        ));

        let _ = fs::remove_dir_all(&root).await;
    }

    /// Lo stesso foglio nominato in due modi (`./index.css` da un file,
    /// `index.css` da un altro) e' UN foglio. Senza la normalizzazione erano due
    /// percorsi distinti: letto due volte, contato due volte, e riportato
    /// all'agente come `src/./index.css` — un percorso che non si riapre.
    #[tokio::test]
    async fn lo_stesso_foglio_nominato_in_due_modi_e_uno_solo() {
        let root = radice("dueforme");
        scrivi(&root, "package.json", PKG_SENZA_TAILWIND).await;
        scrivi(&root, "src/index.css", ".pannello { padding: 1rem; }\n").await;
        scrivi(&root, "src/main.tsx", "import './index.css';\n").await;
        scrivi(
            &root,
            "src/App.tsx",
            "import 'index.css';\nexport default () => <div className=\"pannello\" />;\n",
        )
        .await;

        let ev = collect_evidence(&root, &voc()).await;
        assert_eq!(
            ev.fogli_raggiunti,
            vec!["src/index.css".to_string()],
            "un solo foglio, e senza './' nel percorso"
        );
    }

    /// Un import che esce dalla radice esaminata (monorepo con `target_dir`) non
    /// si legge: non si puo' dire «nessuna fonte» di cio' che non si e' guardato.
    /// Dichiarare qui il difetto sarebbe il rimando a vuoto peggiore, perche'
    /// nascerebbe da un'assenza di osservazione travestita da diagnosi.
    #[tokio::test]
    async fn un_foglio_fuori_dalla_radice_esaminata_rende_non_concludente() {
        let base = radice("monorepo");
        let frontend = base.join("frontend");
        scrivi(&base, "shared/tema.css", ".p-4 { padding: 1rem; }\n").await;
        scrivi(&frontend, "package.json", PKG_SENZA_TAILWIND).await;
        scrivi(
            &frontend,
            "src/main.tsx",
            "import '../../shared/tema.css';\n",
        )
        .await;
        scrivi(&frontend, "src/App.tsx", COMPONENTE_REALE).await;

        // La radice esaminata e' `frontend/`: il foglio condiviso resta fuori.
        let ev = collect_evidence(&frontend, &voc()).await;
        assert!(ev.fogli_raggiunti.is_empty());
        assert_eq!(
            ev.fogli_fuori_radice.len(),
            1,
            "il riferimento fuori radice va dichiarato: {:?}",
            ev.fogli_fuori_radice
        );

        let v = classify_styling(&ev, &voc());
        assert!(
            matches!(v, StylingVerdict::NonConcludente { .. }),
            "atteso non concludente, ottenuto {v:?}"
        );
        assert!(!v.e_bloccante(), "cio' che non si e' visto non blocca");

        // Esaminando la radice che li contiene entrambi, la fonte si trova.
        let ev_completa = collect_evidence(&base, &voc()).await;
        assert!(
            matches!(
                classify_styling(&ev_completa, &voc()),
                StylingVerdict::StileApplicato { .. }
            ),
            "con la radice giusta il foglio condiviso e' una fonte: {ev_completa:?}"
        );

        let _ = fs::remove_dir_all(&base).await;
    }

    /// Vocabolario vuoto: nessun verdetto. Senza questa variante il criterio
    /// direbbe «nessuna fonte» per OGNI progetto quando la migrazione non e'
    /// applicata — un falso positivo con l'aria di una diagnosi (regola M).
    #[test]
    fn senza_vocabolario_non_si_giudica() {
        let ev = StylingEvidence {
            sorgenti_interfaccia: 3,
            classi_dichiarate: vec!["p-4".into(), "mb-4".into(), "text-xl".into()],
            ..Default::default()
        };
        let v = classify_styling(&ev, &StyleVocabulary::default());
        assert_eq!(v, StylingVerdict::VocabolarioAssente);
        assert!(!v.e_bloccante());
    }

    /// Le classi che il sorgente non decide non sono dichiarazioni: contarle
    /// produrrebbe rilievi su nomi che nel DOM non compaiono mai.
    #[test]
    fn le_interpolazioni_non_sono_classi_dichiarate() {
        assert_eq!(
            classi_letterali("p-4  ${dinamica} {altra} text-xl"),
            vec!["p-4".to_string(), "text-xl".to_string()]
        );
    }

    /// Il formato del vocabolario dei framework, letto come lo scrive la
    /// migrazione: se il parse divergesse, il criterio girerebbe su un elenco
    /// vuoto e assolverebbe tutti.
    #[test]
    fn parse_dei_framework_dal_formato_del_db() {
        let fw = parse_frameworks(&Some(
            "# commento\ntailwindcss|tailwind.config.js,tailwind.config.ts|@tailwind\n\
             solo-nome||\n"
                .to_string(),
        ));
        assert_eq!(fw.len(), 2);
        assert_eq!(fw[0].pacchetto, "tailwindcss");
        assert_eq!(fw[0].config_attesi.len(), 2);
        assert_eq!(fw[0].direttive_attese, vec!["@tailwind".to_string()]);
        assert!(fw[1].config_attesi.is_empty() && fw[1].direttive_attese.is_empty());
    }
}
