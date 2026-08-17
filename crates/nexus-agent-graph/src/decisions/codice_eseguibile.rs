//! PUNTO UNICO (regola L) della domanda: **i file di codice che questo run ha
//! PRODOTTO si caricano nel loro runtime?**
//!
//! ## Il difetto, misurato il 17/08/2026
//!
//! Run reale dalla UI su progetto vuoto, task «crea `calcolatrice.js` con
//! quattro funzioni e `calcolatrice.test.js` con cinque test».
//!
//! `calcolatrice.js` funziona (verificato a mano: `somma(2,3)=5`, la divisione
//! per zero lancia). `calcolatrice.test.js` NON PARTE: `ReferenceError: describe
//! is not defined` — sintassi Jest in un progetto senza Jest, senza
//! `package.json` e senza `node_modules`. Il run ha chiuso `completed=true` e il
//! final gate ha dichiarato «passato» DUE volte (`cycle=2 inconclusive=2`, poi
//! `cycle=1 inconclusive=2`).
//!
//! La catena si e' rotta in tre punti, e il primo aveva visto giusto:
//!
//!  1. il Consiglio aveva previsto ESATTAMENTE questo — fra i rischi emessi,
//!     «senza un framework di test dichiarato il file di test puo' essere non
//!     eseguibile col runner predefinito», con la raccomandazione «preferire
//!     `node:test` se il progetto non ha ancora un framework». L'agente ha
//!     scelto Jest lo stesso;
//!  2. il riscontro dei requisiti non poteva accorgersene: 15 dei 17 requisiti
//!     erano `non_verificabili` (prosa, non letterali cercabili), limite gia'
//!     dichiarato in [`super::requirement_conformance`];
//!  3. il final gate non aveva NIENTE da chiedere: nessuna porta registrata ->
//!     niente [`super::browser_dialogue`], niente [`super::static_render`],
//!     niente [`super::endpoint_probes`], nessuna suite dichiarata. Ha chiuso
//!     col beneficio del dubbio.
//!
//! ## Perche' e' un buco strutturale e non sfortuna
//!
//! La famiglia dei criteri copre l'app col server, la pagina statica, la suite
//! E2E, lo stile applicato. Mancava **il caso base**: il codice prodotto si
//! carica? E' la stessa forma di difetto che l'audit ha gia' chiuso tre volte
//! (gate cieco su app statiche, gate cieco senza browser, suite eseguita da tre
//! attori): la verifica che manca e' sempre quella del caso piu' semplice.
//!
//! ## Cosa NON e' questa domanda
//!
//! NON «i test passano»: quella e' un'altra domanda, ha gia' il suo punto unico
//! (`mcp-core::suite_verification`), e un test rosso e' INFORMAZIONE mentre un
//! test che non parte e' codice rotto. La distinzione non e' teorica: e' la
//! ragione per cui il livello di caricamento gira con un filtro che non esegue
//! alcun test (vedi [`LivelloProva::Caricamento`]).
//!
//! ## Confine (regola L)
//!
//! Qui vive il CRITERIO, puro e verificabile senza DB ne' processi: quali file
//! si provano ([`pianifica_prova`]) e cosa dicono gli esiti raccolti
//! ([`classifica_esecuzione`]). L'I/O — leggere il registro delle scritture,
//! eseguire i comandi, raccogliere gli esiti — vive in
//! `mcp-core::agent_graph_adapter::codice_eseguibile`, che porta i FATTI e non
//! li giudica.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// Il tipo di criterio nel vocabolario del runner (regola N).
pub const CRITERION_TYPE: &str = "codice_eseguibile";

/// Chiavi della spec, con un solo punto di scrittura (i test le referenziano da
/// qui, mai come letterali sparsi).
pub const CHIAVE_VOCABOLARIO: &str = "runtimes";
pub const CHIAVE_MAX_FILE: &str = "max_files";

/// Quanto di un file si e' potuto accertare, e con quale prova.
///
/// I due livelli non sono gradazioni della stessa misura: il primo NON esegue
/// niente (`node --check`, `python -m py_compile`), il secondo carica davvero il
/// modulo. Il caso misurato il 17/08/2026 PASSA il primo e cade al secondo, ed
/// e' la ragione per cui il secondo esiste.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LivelloProva {
    /// Il runtime accetta il testo del file senza eseguirlo.
    Sintassi,
    /// Il runtime CARICA il modulo: import risolti, simboli del contorno
    /// presenti. E' il livello che vede `describe is not defined`.
    Caricamento,
}

impl LivelloProva {
    /// Identificatore canonico (regola N) per evidenza e log.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sintassi => "syntax",
            Self::Caricamento => "load",
        }
    }
}

/// Perche' un file prodotto non e' stato provato.
///
/// Variante e non stringa (regola Q): le cause hanno conseguenze OPPOSTE sul
/// verdetto del run — «non e' codice che sappiamo provare» e' una risposta,
/// «il runtime non e' partito» e' un non aver guardato — e collassarle in un
/// motivo testuale renderebbe le due indistinguibili proprio dove si decide se
/// il gate abbia misurato qualcosa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CausaNonProvato {
    /// L'estensione non ha un runtime dichiarato nel vocabolario: un `.md`, un
    /// `.json`, un `.png`. Non c'e' niente da accertare, e non e' una lacuna.
    FuoriVocabolario,
    /// Il file non e' piu' sull'albero al momento della verifica (cancellato o
    /// spostato dopo la scrittura). La scrittura resta un fatto, il file no.
    FileAssente,
    /// La riga di vocabolario non e' un comando semplice eseguibile (vuota, con
    /// redirezioni, con piu' comandi in catena): e' un errore di
    /// CONFIGURAZIONE, non del codice prodotto.
    VocabolarioNonEseguibile { dettaglio: String },
    /// Il runtime dichiarato non e' partito o non ha risposto: programma
    /// assente dal PATH, timeout. NON e' un difetto del codice — ed e' l'unica
    /// causa che significa «non ho potuto guardare».
    RuntimeNonDisponibile { dettaglio: String },
    /// Oltre il tetto di file provabili per run: il file c'era, non lo si e'
    /// provato per non far durare il gate quanto una build.
    OltreIlTetto,
}

impl CausaNonProvato {
    /// Identificatore canonico (regola N).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FuoriVocabolario => "out_of_vocabulary",
            Self::FileAssente => "file_gone",
            Self::VocabolarioNonEseguibile { .. } => "vocabulary_not_runnable",
            Self::RuntimeNonDisponibile { .. } => "runtime_unavailable",
            Self::OltreIlTetto => "over_cap",
        }
    }

    /// C'era qualcosa da accertare e non si e' potuto?
    ///
    /// E' la domanda che separa una risposta da un silenzio, e la pone solo
    /// [`classifica_esecuzione`]. `FuoriVocabolario` e `FileAssente` sono
    /// risposte: non c'e' codice da provare, o non c'e' piu' il file.
    /// `OltreIlTetto` non lo e' — ma per costruzione compare solo quando altri
    /// file SONO stati provati, quindi non decide mai da solo.
    pub fn e_un_non_guardato(&self) -> bool {
        matches!(
            self,
            Self::VocabolarioNonEseguibile { .. }
                | Self::RuntimeNonDisponibile { .. }
                | Self::OltreIlTetto
        )
    }

    /// Il dettaglio dichiarato dalla causa, quando ne porta uno.
    pub fn dettaglio(&self) -> Option<&str> {
        match self {
            Self::VocabolarioNonEseguibile { dettaglio }
            | Self::RuntimeNonDisponibile { dettaglio } => Some(dettaglio),
            _ => None,
        }
    }
}

/// Cosa si e' potuto accertare di UN file prodotto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EsitoFile {
    /// Il runtime lo ha accettato fino al livello indicato.
    Caricato { livello: LivelloProva },
    /// Il runtime lo ha RIFIUTATO: sintassi non valida, import irrisolto,
    /// simbolo non definito. E' codice che non parte.
    ///
    /// `causa` e' il messaggio del RUNTIME, troncato — non un giudizio composto
    /// a mano (regola Q): chi legge il rimando deve vedere l'errore vero, che e'
    /// anche l'unica cosa con cui l'agente puo' correggere.
    NonCaricato {
        livello: LivelloProva,
        exit_code: Option<i32>,
        causa: String,
    },
    /// Non si e' provato, e il perche' e' dichiarato.
    NonProvato { causa: CausaNonProvato },
}

/// Un file prodotto dal run e cio' che se ne e' accertato.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FattoFile {
    /// Percorso RELATIVO alla radice del run, come lo registra `file_mutations`.
    pub path: String,
    pub esito: EsitoFile,
}

/// Il verdetto sul RUN.
///
/// Le due varianti che non giudicano il codice sono DUE e non una, e il design
/// ne prevedeva una sola (`NonApplicabile`). Le loro conseguenze sul gate sono
/// OPPOSTE — «non c'era codice da provare» passa, «c'era e non ho potuto
/// guardare» dichiara che la verifica non c'e' stata — e collassarle avrebbe
/// fatto passare per «niente da provare» un `node` assente dal PATH: la forma
/// esatta del silenzio che la regola Q vieta, e per giunta proprio nel criterio
/// che nasce da un gate che aveva chiuso col beneficio del dubbio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerdettoEsecuzione {
    /// Almeno un file provato, nessuno rifiutato.
    CodiceCaricabile { provati: usize },
    /// Almeno un file RIFIUTATO dal suo runtime: il run non ha finito.
    CodiceRotto { rotti: Vec<FattoFile> },
    /// Nessun file di codice fra quelli prodotti: non c'e' niente da provare, ed
    /// e' una RISPOSTA. Un run che scrive documentazione o configurazione non
    /// deve chiudere non-verificato per un criterio che non lo riguarda.
    NienteDaProvare { scritti: usize },
    /// C'erano file da provare e nessuno si e' potuto accertare.
    NonAccertabile {
        motivo: String,
        non_provati: usize,
    },
}

impl VerdettoEsecuzione {
    /// Identificatore canonico (regola N).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CodiceCaricabile { .. } => "code_loads",
            Self::CodiceRotto { .. } => "code_broken",
            Self::NienteDaProvare { .. } => "nothing_to_prove",
            Self::NonAccertabile { .. } => "not_ascertainable",
        }
    }

    /// Il verdetto BOCCIA il run? Solo il codice rifiutato dal proprio runtime.
    pub fn e_bloccante(&self) -> bool {
        matches!(self, Self::CodiceRotto { .. })
    }

    /// Il criterio ha MISURATO qualcosa?
    ///
    /// `false` solo per [`VerdettoEsecuzione::NonAccertabile`]: li' il gate deve
    /// dichiarare di non aver verificato (`completed_unverified`), mai «va
    /// bene». Distinto da [`VerdettoEsecuzione::NienteDaProvare`], che e' una
    /// risposta e passa.
    pub fn ha_misurato(&self) -> bool {
        !matches!(self, Self::NonAccertabile { .. })
    }

    /// Il FATTO da opporre all'agente quando il verdetto boccia. `None` quando
    /// non c'e' niente da contestare.
    ///
    /// E' l'unico punto in cui la misura diventa testo (regola Q): nasce DAI
    /// campi, e i chiamanti non ricompongono una loro descrizione del conteggio.
    pub fn fatto_opponibile(&self) -> Option<String> {
        let Self::CodiceRotto { rotti } = self else {
            return None;
        };
        let elenco: Vec<String> = rotti
            .iter()
            .map(|f| match &f.esito {
                EsitoFile::NonCaricato { causa, .. } => format!("{}: {causa}", f.path),
                // Irraggiungibile per costruzione (`rotti` contiene solo
                // `NonCaricato`), e non si inventa un messaggio: si dichiara.
                _ => format!("{}: causa non dichiarata", f.path),
            })
            .collect();
        Some(format!(
            "{} file prodotti da questo run NON si caricano nel loro runtime: {}",
            rotti.len(),
            elenco.join(" | ")
        ))
    }
}

/// IL CRITERIO, in un posto solo.
///
/// L'ordine di precedenza e' load-bearing e va letto in questo ordine:
///
///  1. basta UN file rifiutato perche' il run non abbia finito, per quanti file
///     sani lo accompagnino. Il contrario (pretendere che tutti siano rotti)
///     assolverebbe il caso misurato, dove un file su due funzionava;
///  2. basta UN file caricato perche' il criterio abbia una misura positiva: i
///     `NonProvato` che lo accompagnano non declassano nulla, perche' l'assenza
///     di un runtime non e' un difetto del codice;
///  3. senza nemmeno una prova, decide la CAUSA per cui non se ne sono fatte.
pub fn classifica_esecuzione(fatti: &[FattoFile]) -> VerdettoEsecuzione {
    let rotti: Vec<FattoFile> = fatti
        .iter()
        .filter(|f| matches!(f.esito, EsitoFile::NonCaricato { .. }))
        .cloned()
        .collect();
    if !rotti.is_empty() {
        return VerdettoEsecuzione::CodiceRotto { rotti };
    }
    let provati = fatti
        .iter()
        .filter(|f| matches!(f.esito, EsitoFile::Caricato { .. }))
        .count();
    if provati > 0 {
        return VerdettoEsecuzione::CodiceCaricabile { provati };
    }
    let non_guardati: Vec<&CausaNonProvato> = fatti
        .iter()
        .filter_map(|f| match &f.esito {
            EsitoFile::NonProvato { causa } if causa.e_un_non_guardato() => Some(causa),
            _ => None,
        })
        .collect();
    if non_guardati.is_empty() {
        return VerdettoEsecuzione::NienteDaProvare {
            scritti: fatti.len(),
        };
    }
    // Il motivo nasce dalla PRIMA causa non guardata e ne porta il dettaglio del
    // runtime: e' cio' che dice a chi legge se rimediare al PATH o al file.
    let prima = non_guardati[0];
    let motivo = match prima.dettaglio() {
        Some(d) => format!("{} ({d})", prima.as_str()),
        None => prima.as_str().to_string(),
    };
    VerdettoEsecuzione::NonAccertabile {
        motivo,
        non_provati: non_guardati.len(),
    }
}

// ─── Vocabolario: come si prova un file, per famiglia (regola G) ──────────────

/// I comandi con cui si prova una famiglia di file.
///
/// Due campi e non uno perche' i livelli sono due e il secondo NON e' sempre
/// dichiarabile: per Python il vocabolario di default non dichiara un livello di
/// caricamento, e l'assenza significa «per questa estensione la domanda si ferma
/// alla compilazione», non «usa un ripiego».
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeEstensione {
    /// Comando che accetta o rifiuta il TESTO del file senza eseguirlo.
    pub carica: String,
    /// Comando che CARICA un file di test senza eseguirne i casi. `None` = la
    /// domanda non si pone per questa estensione.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carica_test: Option<String>,
}

/// Il vocabolario, dal DB (regola G): un linguaggio nuovo e' una riga, non una
/// patch.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VocabolarioRuntime {
    /// Estensione (senza punto, minuscola) -> comandi.
    #[serde(default)]
    pub estensioni: BTreeMap<String, RuntimeEstensione>,
    /// Suffissi del NOME che dichiarano un file di test (`.test`, `.spec`,
    /// `_test`), confrontati con lo stem — cioe' il nome senza l'estensione.
    /// Un suffisso e non una sottostringa: `.spec` non deve riconoscere
    /// `spec_helper.js`, che non e' un test.
    #[serde(default)]
    pub marcatori_test: Vec<String>,
}

impl VocabolarioRuntime {
    /// Legge il vocabolario dal valore del setting. `None` = assente o
    /// illeggibile: il chiamante lo DICHIARA (criterio inconcludente) invece di
    /// ripiegare su un elenco cablato, che sarebbe la seconda verita' che la
    /// regola G vieta.
    pub fn parse(raw: &str) -> Option<Self> {
        let v: Self = serde_json::from_str(raw.trim()).ok()?;
        if v.estensioni.is_empty() {
            return None;
        }
        Some(v)
    }

    /// Il file dichiara di essere un test?
    ///
    /// Lo dice il NOME, ed e' l'unico segnale disponibile prima di caricarlo: il
    /// contenuto lo direbbe solo eseguendolo, che e' precisamente la cosa che si
    /// sta decidendo se fare.
    pub fn e_un_test(&self, path: &str) -> bool {
        let nome = nome_file(path);
        let Some(stem) = nome.rsplit_once('.').map(|(s, _)| s) else {
            return false;
        };
        self.marcatori_test
            .iter()
            .map(|m| m.trim())
            .filter(|m| !m.is_empty())
            .any(|m| stem.len() > m.len() && stem.ends_with(m))
    }
}

/// Ultimo segmento del percorso, con entrambi i separatori: il registro delle
/// scritture porta percorsi POSIX, ma su Windows un valore col backslash arriva
/// dagli stessi tool, e riconoscere un separatore solo perderebbe il nome.
fn nome_file(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Estensione minuscola senza punto, `None` se il nome non ne ha.
fn estensione(path: &str) -> Option<String> {
    let nome = nome_file(path);
    nome.rsplit_once('.')
        .filter(|(stem, ext)| !stem.is_empty() && !ext.is_empty())
        .map(|(_, ext)| ext.to_ascii_lowercase())
}

/// UN passo di prova: il programma e i suoi argomenti, gia' scomposti. Il file
/// NON e' fra gli argomenti: lo aggiunge chi esegue, che e' anche l'unico a
/// sapere come renderlo per un processo esterno.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassoProva {
    pub livello: LivelloProva,
    pub programma: String,
    pub argomenti: Vec<String>,
}

/// Il piano di prova di UN file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PianoProva {
    /// I passi da eseguire IN ORDINE: il primo che rifiuta il file decide, e i
    /// successivi non si eseguono (un file con un errore di sintassi non ha
    /// niente da dire sul caricamento).
    Prova { passi: Vec<PassoProva> },
    /// Non si prova, e il motivo e' gia' una causa dichiarata.
    NonProvabile { causa: CausaNonProvato },
}

/// Come si prova questo file, dato il vocabolario.
///
/// La riga del DB si scompone col punto unico [`super::shell_command::comandi`]
/// e non con uno `split_whitespace`: due scompositori divergerebbero in
/// silenzio, ed e' gia' successo in questo repo. Una riga che non sia UN comando
/// semplice (catena, redirezioni, env inline, vuota) non e' un runtime: e' una
/// configurazione sbagliata, e lo si dichiara invece di eseguirne un pezzo.
pub fn pianifica_prova(path: &str, voc: &VocabolarioRuntime) -> PianoProva {
    let Some(ext) = estensione(path) else {
        return PianoProva::NonProvabile {
            causa: CausaNonProvato::FuoriVocabolario,
        };
    };
    let Some(rt) = voc.estensioni.get(&ext) else {
        return PianoProva::NonProvabile {
            causa: CausaNonProvato::FuoriVocabolario,
        };
    };
    let mut passi = Vec::new();
    match passo_da_riga(LivelloProva::Sintassi, &rt.carica) {
        Ok(p) => passi.push(p),
        Err(causa) => return PianoProva::NonProvabile { causa },
    }
    if voc.e_un_test(path) {
        if let Some(riga) = rt.carica_test.as_deref() {
            match passo_da_riga(LivelloProva::Caricamento, riga) {
                Ok(p) => passi.push(p),
                Err(causa) => return PianoProva::NonProvabile { causa },
            }
        }
    }
    PianoProva::Prova { passi }
}

/// Traduce UNA riga di vocabolario in un passo eseguibile.
fn passo_da_riga(livello: LivelloProva, riga: &str) -> Result<PassoProva, CausaNonProvato> {
    let non_eseguibile = |dettaglio: String| CausaNonProvato::VocabolarioNonEseguibile {
        dettaglio: format!("'{}': {dettaglio}", riga.trim()),
    };
    let comandi = super::shell_command::comandi(riga);
    if comandi.len() != 1 {
        return Err(non_eseguibile(format!(
            "attesa una sola invocazione, trovate {}",
            comandi.len()
        )));
    }
    let c = &comandi[0];
    if c.redirezioni {
        return Err(non_eseguibile("contiene una redirezione".to_string()));
    }
    if !c.env.is_empty() {
        return Err(non_eseguibile(
            "contiene assegnazioni env inline".to_string(),
        ));
    }
    let mut parole = c.parole.iter();
    let Some(programma) = parole.next().filter(|p| !p.is_empty()).cloned() else {
        return Err(non_eseguibile("nessun programma".to_string()));
    };
    Ok(PassoProva {
        livello,
        programma,
        argomenti: parole.cloned().collect(),
    })
}

// ─── Il criterio del gate ─────────────────────────────────────────────────────

/// I parametri della misura, risolti dal DB da chi costruisce il criterio
/// (regola G) e non dal runner. Struct e non argomenti sciolti: due numeri in
/// fila nella firma sono due occasioni di scambiarli senza che nulla se ne
/// accorga.
#[derive(Debug, Clone, PartialEq)]
pub struct ParametriEsecuzione {
    /// La chiave lo accende (`agent.final_gate.codice_eseguibile_enabled`).
    pub abilitato: bool,
    /// Pazienza per UN comando di prova.
    pub timeout_s: f64,
    /// Tetto di file effettivamente PROVATI in un giro di gate: oltre, il file
    /// resta un fatto dichiarato ([`CausaNonProvato::OltreIlTetto`]) e non una
    /// prova in piu'. Senza, un run che tocca cinquecento sorgenti farebbe
    /// durare il gate quanto una build.
    pub max_file: usize,
}

/// La spec del criterio, costruita QUI e non dai chiamanti: il produttore e' uno
/// solo, cosi' i test possono attraversarlo invece di fabbricare la spec a mano
/// (regola O).
///
/// Il VOCABOLARIO viaggia nella spec e non si rilegge al momento della verifica,
/// per la stessa disciplina di [`super::static_render::criterio_resa`]: e'
/// configurazione, la risolve a monte chi legge il DB, e cosi' la misura resta
/// leggibile in cio' che ha dichiarato di aver usato.
///
/// `voc = None` NON impedisce al criterio di nascere, ed e' deliberato: un
/// criterio che sparisse quando la sua configurazione manca sarebbe un gate
/// silenziosamente inerte — cioe' il punto di partenza. Nasce, e chi verifica
/// dichiara di non aver potuto misurare.
pub fn criterio_esecuzione(
    voc: Option<&VocabolarioRuntime>,
    p: &ParametriEsecuzione,
) -> Option<crate::runtime::ports::CriterionSpec> {
    use crate::runtime::ports::{CriterionProvenance, CriterionSpec};
    if !p.abilitato {
        return None;
    }
    let mut spec = Map::new();
    spec.insert(CHIAVE_MAX_FILE.to_string(), json!(p.max_file));
    // La chiave entra solo se il vocabolario c'e': assente significa «non l'ho
    // potuto leggere», e un oggetto vuoto scritto qui sarebbe indistinguibile
    // da un vocabolario che non dichiara nulla.
    if let Some(v) = voc {
        if let Ok(serializzato) = serde_json::to_value(v) {
            spec.insert(CHIAVE_VOCABOLARIO.to_string(), serializzato);
        }
    }
    Some(CriterionSpec {
        criterion_type: CRITERION_TYPE.to_string(),
        provenance: CriterionProvenance::Gate,
        spec: Value::Object(spec),
        expected: json!({}),
        timeout_s: Some(p.timeout_s),
    })
}

/// L'evidenza del criterio, composta DAI campi del verdetto e dei fatti
/// (regola Q): nessun consumatore ricostruisce il verdetto da questo testo.
pub fn evidenza_criterio(verdetto: &VerdettoEsecuzione, fatti: &[FattoFile]) -> Value {
    let mut out = json!({
        "verdict": verdetto.as_str(),
        "bloccante": verdetto.e_bloccante(),
        "misurato": verdetto.ha_misurato(),
        "file": {
            "considerati": fatti.len(),
            "caricati": fatti.iter().filter(|f| matches!(f.esito, EsitoFile::Caricato { .. })).count(),
            "non_caricati": fatti.iter().filter(|f| matches!(f.esito, EsitoFile::NonCaricato { .. })).count(),
            "non_provati": fatti.iter().filter(|f| matches!(f.esito, EsitoFile::NonProvato { .. })).count(),
        },
    });
    let Some(o) = out.as_object_mut() else {
        return out;
    };
    // I file ROTTI per intero: sono cio' su cui l'agente deve tornare, e un
    // taglio li' toglierebbe proprio l'informazione che serve a correggere.
    o.insert("rotti".to_string(), json!(dettaglio_rotti(fatti)));
    // I non provati si riportano AGGREGATI per causa: l'elenco per nome di un
    // run che scrive cento file di documentazione sarebbe rumore, il conteggio
    // per causa e' il dato con cui si decide se il vocabolario va esteso o il
    // PATH sistemato.
    o.insert(
        "non_provati_per_causa".to_string(),
        json!(non_provati_per_causa(fatti)),
    );
    if let Some(fatto) = verdetto.fatto_opponibile() {
        o.insert("verdict_text".to_string(), json!(fatto));
    }
    if let VerdettoEsecuzione::NonAccertabile { motivo, .. } = verdetto {
        o.insert("skipped_reason".to_string(), json!(motivo));
    }
    out
}

/// I file rifiutati, uno per uno, coi campi con cui si corregge.
fn dettaglio_rotti(fatti: &[FattoFile]) -> Vec<Value> {
    fatti.iter().filter_map(riga_rotto).collect()
}

/// La riga di UN file rifiutato; `None` per ogni altro esito.
fn riga_rotto(f: &FattoFile) -> Option<Value> {
    let EsitoFile::NonCaricato {
        livello,
        exit_code,
        causa,
    } = &f.esito
    else {
        return None;
    };
    Some(json!({
        "path": f.path,
        "livello": livello.as_str(),
        "exit_code": exit_code,
        "causa": causa,
    }))
}

/// Quanti file non si sono provati, per CAUSA.
fn non_provati_per_causa(fatti: &[FattoFile]) -> BTreeMap<&'static str, usize> {
    let mut per_causa: BTreeMap<&'static str, usize> = BTreeMap::new();
    for f in fatti {
        if let EsitoFile::NonProvato { causa } = &f.esito {
            *per_causa.entry(causa.as_str()).or_default() += 1;
        }
    }
    per_causa
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caricato(path: &str) -> FattoFile {
        FattoFile {
            path: path.to_string(),
            esito: EsitoFile::Caricato {
                livello: LivelloProva::Sintassi,
            },
        }
    }

    fn rotto(path: &str, causa: &str) -> FattoFile {
        FattoFile {
            path: path.to_string(),
            esito: EsitoFile::NonCaricato {
                livello: LivelloProva::Caricamento,
                exit_code: Some(1),
                causa: causa.to_string(),
            },
        }
    }

    fn non_provato(path: &str, causa: CausaNonProvato) -> FattoFile {
        FattoFile {
            path: path.to_string(),
            esito: EsitoFile::NonProvato { causa },
        }
    }

    /// (1) della tabella del design: solo caricati -> il codice si carica.
    #[test]
    fn solo_caricati_e_codice_caricabile() {
        let fatti = vec![caricato("a.js"), caricato("b.js")];
        let v = classifica_esecuzione(&fatti);
        assert_eq!(v, VerdettoEsecuzione::CodiceCaricabile { provati: 2 });
        assert!(!v.e_bloccante());
        assert!(v.ha_misurato());
        assert_eq!(v.fatto_opponibile(), None, "niente da opporre: si carica");
    }

    /// IL CASO MISURATO, in forma di criterio: nove file sani e UNO che non
    /// parte. Il run del 17/08/2026 aveva esattamente questa forma —
    /// `calcolatrice.js` funzionante, `calcolatrice.test.js` che non si carica —
    /// e ha chiuso «completato».
    ///
    /// MUTAZIONE (quella prescritta dal design): far degradare il file rifiutato
    /// a `NonProvato` — cioe' trattare «il runtime lo ha respinto» come «non ho
    /// potuto provarlo» — riporta il verdetto a `CodiceCaricabile` e questo test
    /// rosseggia mostrando che il gate riapproverebbe il file rotto.
    #[test]
    fn un_solo_file_rifiutato_basta_a_bocciare() {
        let mut fatti: Vec<FattoFile> = (0..9).map(|i| caricato(&format!("src/m{i}.js"))).collect();
        fatti.push(rotto(
            "calcolatrice.test.js",
            "ReferenceError: describe is not defined",
        ));
        let v = classifica_esecuzione(&fatti);
        let VerdettoEsecuzione::CodiceRotto { rotti } = &v else {
            panic!("atteso CodiceRotto, ottenuto {v:?}");
        };
        assert_eq!(rotti.len(), 1);
        assert_eq!(rotti[0].path, "calcolatrice.test.js");
        assert!(v.e_bloccante(), "e' l'unico verdetto che boccia");
        assert!(v
            .fatto_opponibile()
            .expect("c'e' un fatto da opporre")
            .contains("describe is not defined"));
    }

    /// (3) della tabella: nessuno dei file prodotti e' codice che sappiamo
    /// provare -> il criterio non ha niente da dire, e PASSA. Un run che scrive
    /// documentazione non deve chiudere non-verificato.
    #[test]
    fn tutti_fuori_vocabolario_e_niente_da_provare() {
        let fatti = vec![
            non_provato("README.md", CausaNonProvato::FuoriVocabolario),
            non_provato("dati.json", CausaNonProvato::FuoriVocabolario),
        ];
        let v = classifica_esecuzione(&fatti);
        assert_eq!(v, VerdettoEsecuzione::NienteDaProvare { scritti: 2 });
        assert!(!v.e_bloccante());
        assert!(v.ha_misurato(), "e' una risposta, non un silenzio");
    }

    /// (4) della tabella: zero fatti -> niente da provare. Un run che non ha
    /// scritto file non e' un run col codice rotto.
    #[test]
    fn zero_fatti_e_niente_da_provare() {
        assert_eq!(
            classifica_esecuzione(&[]),
            VerdettoEsecuzione::NienteDaProvare { scritti: 0 }
        );
    }

    /// LA DISTINZIONE CHE IL DESIGN NON AVEVA: `node` assente dal PATH non e'
    /// «niente da provare». C'era codice, non si e' potuto guardare, e il gate
    /// deve dirlo — altrimenti il criterio nato per chiudere un gate che
    /// approvava col beneficio del dubbio ne aprirebbe un altro identico.
    ///
    /// MUTAZIONE: far ritornare `false` a
    /// [`CausaNonProvato::e_un_non_guardato`] per `RuntimeNonDisponibile`
    /// riporta il verdetto a `NienteDaProvare`, cioe' a un `Passed`.
    #[test]
    fn runtime_assente_non_e_un_via_libera() {
        let fatti = vec![non_provato(
            "calcolatrice.js",
            CausaNonProvato::RuntimeNonDisponibile {
                dettaglio: "avvio 'node' fallito: program not found".to_string(),
            },
        )];
        let v = classifica_esecuzione(&fatti);
        let VerdettoEsecuzione::NonAccertabile {
            motivo,
            non_provati,
        } = &v
        else {
            panic!("atteso NonAccertabile, ottenuto {v:?}");
        };
        assert_eq!(*non_provati, 1);
        assert!(motivo.contains("runtime_unavailable"), "motivo: {motivo}");
        assert!(motivo.contains("program not found"), "porta il dettaglio");
        assert!(!v.e_bloccante(), "non e' un difetto del CODICE");
        assert!(!v.ha_misurato(), "e non e' nemmeno un via libera");
    }

    /// Un file sparito dall'albero e' una RISPOSTA, non un silenzio: la
    /// scrittura resta un fatto, il file no, e non c'e' niente da accertare.
    #[test]
    fn un_file_sparito_non_declassa_il_criterio() {
        let fatti = vec![non_provato("tmp/scratch.js", CausaNonProvato::FileAssente)];
        assert_eq!(
            classifica_esecuzione(&fatti),
            VerdettoEsecuzione::NienteDaProvare { scritti: 1 }
        );
    }

    /// Un `NonProvato` accanto a un `Caricato` non declassa nulla: l'assenza di
    /// un runtime non e' un difetto del codice (regola Q), e la misura positiva
    /// che c'e' resta valida.
    #[test]
    fn i_non_provati_non_declassano_una_misura_positiva() {
        let fatti = vec![
            caricato("app.js"),
            non_provato("note.md", CausaNonProvato::FuoriVocabolario),
            non_provato(
                "script.rb",
                CausaNonProvato::RuntimeNonDisponibile {
                    dettaglio: "ruby non trovato".to_string(),
                },
            ),
        ];
        assert_eq!(
            classifica_esecuzione(&fatti),
            VerdettoEsecuzione::CodiceCaricabile { provati: 1 }
        );
    }

    /// Il rifiuto ha la precedenza anche sulle misure positive: nel caso reale i
    /// file sani erano la maggioranza.
    #[test]
    fn il_rifiuto_precede_la_misura_positiva() {
        let fatti = vec![
            caricato("a.js"),
            rotto("b.test.js", "SyntaxError"),
            caricato("c.js"),
        ];
        assert!(classifica_esecuzione(&fatti).e_bloccante());
    }

    /// Identificatori canonici e distinti (regola N).
    #[test]
    fn identificatori_canonici_distinti() {
        assert_eq!(
            VerdettoEsecuzione::CodiceCaricabile { provati: 1 }.as_str(),
            "code_loads"
        );
        assert_eq!(
            VerdettoEsecuzione::CodiceRotto { rotti: vec![] }.as_str(),
            "code_broken"
        );
        assert_eq!(
            VerdettoEsecuzione::NienteDaProvare { scritti: 0 }.as_str(),
            "nothing_to_prove"
        );
        assert_eq!(
            VerdettoEsecuzione::NonAccertabile {
                motivo: String::new(),
                non_provati: 0
            }
            .as_str(),
            "not_ascertainable"
        );
        assert_eq!(LivelloProva::Sintassi.as_str(), "syntax");
        assert_eq!(LivelloProva::Caricamento.as_str(), "load");
    }

    // ── vocabolario e piano di prova ─────────────────────────────────────────

    fn voc() -> VocabolarioRuntime {
        VocabolarioRuntime::parse(
            r#"{
                "marcatori_test": [".test", ".spec"],
                "estensioni": {
                    "js": {"carica": "node --check",
                           "carica_test": "node --test --test-name-pattern=nx_nessun_test"},
                    "py": {"carica": "python -m py_compile"}
                }
            }"#,
        )
        .expect("vocabolario valido")
    }

    /// Un sorgente ordinario ha UN passo: la sintassi. Il caricamento non si
    /// chiede a un modulo qualunque — caricarlo eseguirebbe il suo codice di
    /// modulo, e il criterio non esegue il codice utente a scopo generale.
    #[test]
    fn un_sorgente_ordinario_ha_il_solo_passo_di_sintassi() {
        let PianoProva::Prova { passi } = pianifica_prova("src/calcolatrice.js", &voc()) else {
            panic!("atteso un piano di prova");
        };
        assert_eq!(passi.len(), 1);
        assert_eq!(passi[0].livello, LivelloProva::Sintassi);
        assert_eq!(passi[0].programma, "node");
        assert_eq!(passi[0].argomenti, vec!["--check".to_string()]);
    }

    /// IL CASO MISURATO nel piano: un file di test ha DUE passi, ed e' il
    /// secondo a vedere il difetto. Il primo (`node --check`) lo aveva passato.
    ///
    /// MUTAZIONE: togliere il ramo `e_un_test` (cioe' fermarsi alla sintassi)
    /// lascia un passo solo, e il gate torna cieco su `calcolatrice.test.js`.
    #[test]
    fn un_file_di_test_ha_anche_il_passo_di_caricamento() {
        let PianoProva::Prova { passi } = pianifica_prova("calcolatrice.test.js", &voc()) else {
            panic!("atteso un piano di prova");
        };
        assert_eq!(passi.len(), 2, "sintassi e poi caricamento");
        assert_eq!(passi[1].livello, LivelloProva::Caricamento);
        assert_eq!(passi[1].programma, "node");
        assert_eq!(
            passi[1].argomenti,
            vec![
                "--test".to_string(),
                "--test-name-pattern=nx_nessun_test".to_string()
            ],
            "il filtro fa parte del vocabolario: il livello CARICA il file e non \
             esegue alcun test"
        );
    }

    /// Il marcatore e' un suffisso dello STEM, non una sottostringa del nome:
    /// `spec_helper.js` non e' un test, e provarne il caricamento eseguirebbe
    /// codice di supporto che nessuno ha chiesto di eseguire.
    #[test]
    fn il_marcatore_di_test_e_un_suffisso_non_una_sottostringa() {
        let v = voc();
        assert!(v.e_un_test("calcolatrice.test.js"));
        assert!(v.e_un_test("a/b/api.spec.js"));
        assert!(!v.e_un_test("spec_helper.js"));
        assert!(!v.e_un_test("testimonianze.js"));
        assert!(!v.e_un_test(".test.js"), "senza stem non e' un nome di test");
    }

    /// Un'estensione non dichiarata non e' un difetto: e' fuori vocabolario, e
    /// il criterio lo dice invece di indovinare un runtime.
    #[test]
    fn estensione_fuori_vocabolario() {
        assert_eq!(
            pianifica_prova("main.rs", &voc()),
            PianoProva::NonProvabile {
                causa: CausaNonProvato::FuoriVocabolario
            }
        );
        assert_eq!(
            pianifica_prova("Makefile", &voc()),
            PianoProva::NonProvabile {
                causa: CausaNonProvato::FuoriVocabolario
            }
        );
    }

    /// Python: nessun livello di caricamento dichiarato -> anche un file di test
    /// si ferma alla compilazione. L'assenza e' una dichiarazione, non un
    /// ripiego da riempire.
    #[test]
    fn senza_livello_di_caricamento_il_test_si_ferma_alla_compilazione() {
        let PianoProva::Prova { passi } = pianifica_prova("app.test.py", &voc()) else {
            panic!("atteso un piano di prova");
        };
        assert_eq!(passi.len(), 1);
        assert_eq!(passi[0].programma, "python");
        assert_eq!(
            passi[0].argomenti,
            vec!["-m".to_string(), "py_compile".to_string()]
        );
    }

    /// Una riga di vocabolario che non e' UN comando semplice non si esegue a
    /// pezzi: e' configurazione sbagliata, e la si dichiara. Senza, un
    /// `node --check && rm -rf .` verrebbe eseguito a meta' o per intero.
    #[test]
    fn una_riga_di_vocabolario_non_eseguibile_si_dichiara() {
        for riga in [
            "node --check && echo ok",
            "node --check > /dev/null",
            "FOO=1 node --check",
            "   ",
        ] {
            let v = VocabolarioRuntime::parse(&format!(
                r#"{{"estensioni": {{"js": {{"carica": "{}"}}}}}}"#,
                riga.replace('"', "")
            ))
            .expect("vocabolario valido");
            let piano = pianifica_prova("a.js", &v);
            assert!(
                matches!(
                    piano,
                    PianoProva::NonProvabile {
                        causa: CausaNonProvato::VocabolarioNonEseguibile { .. }
                    }
                ),
                "riga '{riga}' -> {piano:?}"
            );
        }
    }

    /// Vocabolario assente o illeggibile -> `None`, MAI un elenco cablato di
    /// ripiego (regola G). Chi chiama lo dichiara come «non ho potuto misurare».
    #[test]
    fn vocabolario_assente_non_ripiega_su_un_elenco_cablato() {
        assert!(VocabolarioRuntime::parse("").is_none());
        assert!(VocabolarioRuntime::parse("non json").is_none());
        assert!(
            VocabolarioRuntime::parse(r#"{"estensioni": {}}"#).is_none(),
            "un vocabolario senza estensioni non e' un vocabolario"
        );
    }

    // ── il criterio del gate ─────────────────────────────────────────────────

    fn parametri(abilitato: bool) -> ParametriEsecuzione {
        ParametriEsecuzione {
            abilitato,
            timeout_s: 30.0,
            max_file: 50,
        }
    }

    /// A flag spento il criterio NON nasce: il gate resta bit-identico a prima.
    #[test]
    fn a_flag_spento_il_criterio_non_nasce() {
        assert!(criterio_esecuzione(Some(&voc()), &parametri(false)).is_none());
    }

    /// Acceso, nasce con il vocabolario dentro la spec: la misura dichiara cio'
    /// che ha usato per misurare.
    #[test]
    fn il_criterio_porta_il_proprio_vocabolario_nella_spec() {
        let c = criterio_esecuzione(Some(&voc()), &parametri(true)).expect("criterio acceso");
        assert_eq!(c.criterion_type, CRITERION_TYPE);
        assert_eq!(c.timeout_s, Some(30.0));
        assert_eq!(c.spec[CHIAVE_MAX_FILE], json!(50));
        let riletto: VocabolarioRuntime =
            serde_json::from_value(c.spec[CHIAVE_VOCABOLARIO].clone()).expect("vocabolario riletto");
        assert_eq!(riletto, voc(), "la spec porta il vocabolario per intero");
    }

    /// Senza vocabolario il criterio nasce COMUNQUE, e senza la chiave: e' cio'
    /// che permette a chi verifica di dichiarare «non ho potuto misurare»
    /// invece di sparire in silenzio, che sarebbe di nuovo un gate inerte.
    #[test]
    fn senza_vocabolario_il_criterio_nasce_e_lo_dichiara() {
        let c = criterio_esecuzione(None, &parametri(true)).expect("criterio acceso");
        assert!(c.spec.get(CHIAVE_VOCABOLARIO).is_none());
    }

    /// L'evidenza nasce dai CAMPI (regola Q) e porta i rotti per intero: sono
    /// l'unica cosa con cui l'agente puo' correggere.
    #[test]
    fn l_evidenza_porta_i_rotti_e_aggrega_i_non_provati() {
        let fatti = vec![
            caricato("app.js"),
            rotto("calcolatrice.test.js", "ReferenceError: describe is not defined"),
            non_provato("README.md", CausaNonProvato::FuoriVocabolario),
            non_provato("CHANGELOG.md", CausaNonProvato::FuoriVocabolario),
        ];
        let v = classifica_esecuzione(&fatti);
        let ev = evidenza_criterio(&v, &fatti);
        assert_eq!(ev["verdict"], json!("code_broken"));
        assert_eq!(ev["bloccante"], json!(true));
        assert_eq!(ev["misurato"], json!(true));
        assert_eq!(ev["file"]["considerati"], json!(4));
        assert_eq!(ev["file"]["caricati"], json!(1));
        assert_eq!(ev["file"]["non_caricati"], json!(1));
        assert_eq!(ev["rotti"][0]["path"], json!("calcolatrice.test.js"));
        assert_eq!(ev["rotti"][0]["livello"], json!("load"));
        assert_eq!(ev["non_provati_per_causa"]["out_of_vocabulary"], json!(2));
        assert!(ev["verdict_text"]
            .as_str()
            .is_some_and(|t| t.contains("describe is not defined")));
    }

    /// L'evidenza di un `NonAccertabile` porta il motivo nel campo che il gate
    /// gia' legge per «non ho potuto guardare».
    #[test]
    fn l_evidenza_del_non_accertabile_dichiara_il_motivo() {
        let fatti = vec![non_provato(
            "a.js",
            CausaNonProvato::RuntimeNonDisponibile {
                dettaglio: "node non trovato".to_string(),
            },
        )];
        let v = classifica_esecuzione(&fatti);
        let ev = evidenza_criterio(&v, &fatti);
        assert_eq!(ev["misurato"], json!(false));
        assert!(ev["skipped_reason"]
            .as_str()
            .is_some_and(|m| m.contains("node non trovato")));
    }
}
