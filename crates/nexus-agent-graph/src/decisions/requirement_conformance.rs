//! PUNTO UNICO (regola L) della domanda: **i requisiti emessi dal Consiglio
//! delle Competenze sono stati applicati?**
//!
//! ## Il difetto che ha reso necessario il modulo (29/07/2026)
//!
//! Il Consiglio emette requisiti concreti e verificabili, il coordinatore li
//! riceve nel prompt (`pre_run_advisory_synthesis`), e nessuno confronta poi il
//! codice prodotto con cio' che era stato chiesto. Il segnale era prodotto bene,
//! consegnato bene, e mai riscontrato: se domani i requisiti venissero ignorati,
//! il ciclo si chiuderebbe identico a oggi.
//!
//! Misurato a mano sul progetto `gestione-spese` (28/07): il Consiglio aveva
//! emesso `verdict=block` con, fra gli altri, "Rimuovere `port: 33649` da
//! `frontend/vite.config.js`" e "Modificare vite.config.js per includere
//! `server: { strictPort: false }`". Entrambi risultavano applicati — ma la
//! verifica e' costata tre `grep` a un umano il giorno dopo, e il sistema non lo
//! sapeva e non lo diceva. Stessa forma del difetto del ciclo di review prima del
//! fix del 28/07 (contava i rimandi senza guardare se qualcosa fosse cambiato):
//! il meccanismo funzionava, il FATTO non veniva mai guardato.
//!
//! ## Il criterio
//!
//! Non "l'agente dichiara di aver applicato il vincolo" ma il CONTENUTO del file
//! (regola M). Un requisito come "rimuovi quella riga da quel file" o "aggiungi
//! quell'opzione" e' controllabile leggendo il file, senza LLM e senza costo:
//! e' un confronto testuale su un file gia' scritto.
//!
//! ## Requisiti e raccomandazioni non sono la stessa cosa
//!
//! La sintesi produce due liste separate ([`super::advisory_panel::AdvisorySynthesis`]:
//! `requirements` e `recommendations`). Qui entrano SOLO i requisiti: una
//! raccomandazione non applicata non e' uno scostamento, e trattarla come tale
//! renderebbe il rilievo rumore da ignorare. La separazione e' del produttore, e
//! si rispetta al consumo — [`requirements_from_synthesis`] legge quel campo e
//! solo quello.
//!
//! ## Perche' l'incertezza degrada sempre a NON VERIFICABILE
//!
//! Un requisito e' testo in linguaggio naturale: e' l'unica forma in cui esiste
//! (lo scrive una figura). L'estrazione del criterio ([`derive_criterion`]) NON
//! e' una lettura dello stato tecnico dal testo — quella la vieta la regola M —
//! ma la costruzione della DOMANDA da porre al filesystem. La RISPOSTA viene poi
//! solo dal contenuto del file.
//!
//! Da questa asimmetria discende l'invariante del modulo: ogni volta che la
//! domanda non si riesce a formulare senza ambiguita' (nessun file nominato, piu'
//! file, nessun letterale, piu' letterali, file assente, progetto assente) l'esito
//! e' [`RequirementOutcome::NonVerificabile`] con il motivo dichiarato, MAI
//! `Soddisfatto`. Uno zero silenzioso qui — un requisito che nessuno puo'
//! controllare contato come rispettato — sarebbe esattamente il difetto che il
//! modulo esiste per chiudere, spostato di un metro.
//!
//! Il degrado e' anche cio' che rende innocuo un path estratto per sbaglio: un
//! "Node.js" letto come nome di file non produce un verdetto falso, produce un
//! `FileAssente` — e quindi un "non verificabile" onesto.
//!
//! ## Asimmetria della prova: assenza e presenza non pesano uguale
//!
//! - **Assenza** ("rimuovi X da Y"): trovare X nel file e' una prova NETTA di
//!   violazione. Nessuna riformulazione equivalente puo' salvare il requisito —
//!   la stringa che doveva sparire e' ancora li'.
//! - **Presenza** ("aggiungi X a Y"): trovare X e' una prova netta di conformita';
//!   NON trovarlo e' un fatto piu' debole (il vincolo puo' essere stato scritto in
//!   forma equivalente). L'esito resta `NonSoddisfatto`, ma l'evidenza dichiara
//!   letteralmente cio' che si e' osservato ("il letterale non compare nel file"),
//!   perche' chi legge possa giudicare. E' informazione, non un gate: il Consiglio
//!   e' advisory per decisione di prodotto del 13/07/2026 (commit 7a311454) e
//!   questo modulo non cambia quel contratto.
//!
//! ## Confine (regola L)
//!
//! Qui vive la REGOLA (come si formula la domanda, come si giudica la risposta),
//! pura e verificabile senza filesystem. L'I/O — leggere il file dal workspace —
//! resta al chiamante, che porta i fatti come [`FileEvidence`] e non li giudica:
//! due giudizi in due posti darebbero due idee diverse di "applicato".
//!
//! ## Costo
//!
//! Zero chiamate al modello. La verifica e' una lettura di file e un confronto di
//! stringhe: i run con rimandi ripetuti hanno gia' toccato 2,1M token e $3,08, e
//! un ulteriore giro di LLM per giudicare la conformita' costerebbe piu' del
//! difetto che chiude. Dove servirebbe un giudizio semantico, il caso e'
//! `NonVerificabile` per costruzione — non un'altra chiamata al modello.

use serde_json::Value;

/// Identificatori canonici degli esiti (regola N: un solo nome per esito, in
/// inglese). Sono anche le chiavi dei conteggi nel payload: il conteggio di un
/// esito e quell'esito hanno lo stesso nome per costruzione, non per coincidenza
/// mantenuta a mano in due punti.
pub const ESITO_SODDISFATTO: &str = "satisfied";
/// Vedi [`ESITO_SODDISFATTO`].
pub const ESITO_SCOSTAMENTO: &str = "violated";
/// Vedi [`ESITO_SODDISFATTO`].
pub const ESITO_NON_VERIFICABILE: &str = "unverifiable";
/// Campo della sintesi del Consiglio da cui nasce la misura. Nominato perche' e'
/// il confine con un altro produttore ([`super::advisory_panel`]): se quel nome
/// cambiasse, deve cambiare in un posto solo.
pub const CAMPO_REQUISITI: &str = "requirements";

/// Cosa il requisito chiede al file. Le due direzioni NON sono simmetriche nella
/// forza della prova (vedi doc di modulo): la doc di [`RequirementOutcome`] dice
/// cosa significa ciascun esito per ciascuna.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Il letterale NON deve piu' comparire nel file ("rimuovi X da Y").
    DeveMancare,
    /// Il letterale DEVE comparire nel file ("aggiungi X a Y").
    DevePresenziare,
}

impl Direction {
    /// Identificatore canonico (regola N).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DeveMancare => "must_be_absent",
            Self::DevePresenziare => "must_be_present",
        }
    }

    /// Riconosce l'identificatore canonico (regola N). `None` fuori
    /// vocabolario: mai un default indovinato, ne' qui ne' nel chiamante.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "must_be_absent" => Some(Self::DeveMancare),
            "must_be_present" => Some(Self::DevePresenziare),
            _ => None,
        }
    }
}

/// La domanda meccanica derivata da UN requisito: "il letterale L compare (o non
/// compare) nel file P?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequirementCriterion {
    /// Path del file, relativo alla radice del progetto, come nominato dal
    /// requisito.
    pub path: String,
    /// Il letterale da cercare, nella forma originale (l'evidenza lo cita cosi').
    pub literal: String,
    /// Cosa ci si aspetta di trovare.
    pub direction: Direction,
}

/// Perche' un requisito NON e' meccanicamente controllabile. Enum e non stringa
/// libera: il resoconto e chiunque aggreghi questi esiti devono poter distinguere
/// "il requisito e' vago" da "il file non c'e'" senza leggere una frase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unverifiable {
    /// Il requisito non nomina alcun file.
    NessunFile,
    /// Il requisito nomina piu' file distinti: quale controllare sarebbe una
    /// scelta arbitraria, e una scelta arbitraria produce un verdetto arbitrario.
    PiuFile,
    /// Il requisito non porta alcun letterale da cercare (e' una richiesta
    /// descrittiva: "aggiungi un health probe che verifichi la disponibilita'").
    NessunLetterale,
    /// Piu' letterali distinti: la direzione di ciascuno sarebbe un'ipotesi
    /// ("rimuovi A e sostituiscilo con B" chiede due cose opposte).
    PiuLetterali,
    /// Nessun verbo riconoscibile di aggiunta o rimozione: non si sa cosa il
    /// requisito si aspetti di trovare nel file.
    DirezioneAssente,
    /// Il path esce dalla radice del progetto (assoluto o con risalite): non e'
    /// un file del workspace e non si legge.
    PathFuoriProgetto,
    /// Il file nominato non esiste al path indicato. NON e' "soddisfatto": un
    /// file assente puo' voler dire path estratto male tanto quanto vincolo
    /// rispettato, e le due cose non si distinguono da qui.
    FileAssente,
    /// Il file esiste ma non e' leggibile come testo (binario, permessi, I/O).
    FileIlleggibile,
    /// Non c'e' una radice di progetto su cui risolvere i path (run senza
    /// progetto): nessun requisito e' controllabile, e va detto per tutti.
    ProgettoAssente,
}

impl Unverifiable {
    /// Identificatore canonico (regola N): un solo nome per motivo, in inglese,
    /// per il payload e per chi aggrega.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NessunFile => "no_file_named",
            Self::PiuFile => "multiple_files_named",
            Self::NessunLetterale => "no_literal_to_check",
            Self::PiuLetterali => "multiple_literals",
            Self::DirezioneAssente => "no_direction",
            Self::PathFuoriProgetto => "path_outside_project",
            Self::FileAssente => "file_not_found",
            Self::FileIlleggibile => "file_unreadable",
            Self::ProgettoAssente => "no_project_root",
        }
    }

    /// Spiegazione in parole per il resoconto. Nasce DAL motivo strutturato: chi
    /// scrive il resoconto non ricompone una descrizione per conto proprio,
    /// altrimenti due lettori dello stesso esito direbbero due cose diverse.
    pub fn spiegazione(self) -> &'static str {
        match self {
            Self::NessunFile => "non nomina un file su cui controllare",
            Self::PiuFile => "nomina piu' file: quale controllare sarebbe arbitrario",
            Self::NessunLetterale => "non porta un testo preciso da cercare nel file",
            Self::PiuLetterali => "porta piu' testi distinti, con direzioni diverse",
            Self::DirezioneAssente => "non dice se il testo debba comparire o sparire",
            Self::PathFuoriProgetto => "indica un percorso fuori dalla radice del progetto",
            Self::FileAssente => "il file indicato non esiste nel progetto",
            Self::FileIlleggibile => "il file indicato non e' leggibile come testo",
            Self::ProgettoAssente => "il run non ha una radice di progetto su cui controllare",
        }
    }
}

/// Il FATTO osservato sul file. Lo porta il chiamante (I/O) e non lo giudica:
/// qui dentro non si apre nulla.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileEvidence {
    /// Contenuto testuale del file.
    Contenuto(String),
    /// Il file non esiste al path indicato.
    Assente,
    /// Il file esiste ma non e' testo leggibile.
    Illeggibile,
}

/// Esito della verifica di UN requisito.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequirementOutcome {
    /// Il file soddisfa il criterio. Per `DeveMancare`: il letterale non compare
    /// piu'. Per `DevePresenziare`: il letterale compare.
    Soddisfatto {
        /// Cosa e' stato osservato, in una riga.
        evidenza: String,
    },
    /// Il file NON soddisfa il criterio.
    NonSoddisfatto {
        /// Cosa e' stato osservato, in una riga (per `DeveMancare` include la
        /// riga in cui il letterale compare ancora).
        evidenza: String,
    },
    /// Il requisito non e' meccanicamente controllabile: dichiarato, mai contato
    /// come soddisfatto.
    NonVerificabile {
        /// Perche'.
        motivo: Unverifiable,
    },
}

impl RequirementOutcome {
    /// Identificatore canonico (regola N).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Soddisfatto { .. } => ESITO_SODDISFATTO,
            Self::NonSoddisfatto { .. } => ESITO_SCOSTAMENTO,
            Self::NonVerificabile { .. } => ESITO_NON_VERIFICABILE,
        }
    }

    /// `true` SOLO per un requisito osservato conforme. Un `NonVerificabile` non
    /// e' conforme: e' ignoto, e i due non vanno confusi da nessun chiamante —
    /// per questo la domanda si pone qui e non con un `matches!` sparso.
    pub fn e_soddisfatto(&self) -> bool {
        matches!(self, Self::Soddisfatto { .. })
    }

    /// `true` solo per uno scostamento OSSERVATO (il fatto contraddice il
    /// requisito).
    pub fn e_scostamento(&self) -> bool {
        matches!(self, Self::NonSoddisfatto { .. })
    }
}

/// Verifica di UN requisito: il testo originale (che resta la cosa che l'utente
/// legge) e l'esito.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequirementVerdict {
    /// Il requisito come lo ha scritto la figura.
    pub requirement: String,
    /// Il criterio derivato, quando derivabile (`None` se l'esito e'
    /// `NonVerificabile` per un motivo di formulazione).
    pub criterion: Option<RequirementCriterion>,
    /// L'esito.
    pub outcome: RequirementOutcome,
}

impl RequirementVerdict {
    /// Serializza in `Value` per il payload strutturato.
    pub fn to_value(&self) -> Value {
        let mut o = serde_json::Map::new();
        o.insert(
            "requirement".to_string(),
            Value::String(self.requirement.clone()),
        );
        o.insert(
            "outcome".to_string(),
            Value::String(self.outcome.as_str().to_string()),
        );
        match &self.outcome {
            RequirementOutcome::Soddisfatto { evidenza }
            | RequirementOutcome::NonSoddisfatto { evidenza } => {
                o.insert("evidence".to_string(), Value::String(evidenza.clone()));
            }
            RequirementOutcome::NonVerificabile { motivo } => {
                o.insert(
                    "reason".to_string(),
                    Value::String(motivo.as_str().to_string()),
                );
            }
        }
        if let Some(c) = &self.criterion {
            o.insert("file".to_string(), Value::String(c.path.clone()));
            o.insert("literal".to_string(), Value::String(c.literal.clone()));
            o.insert(
                "direction".to_string(),
                Value::String(c.direction.as_str().to_string()),
            );
        }
        Value::Object(o)
    }
}

/// Esito della verifica di TUTTI i requisiti di un run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceReport {
    /// Un verdetto per requisito, nell'ordine in cui il Consiglio li ha emessi.
    pub verdicts: Vec<RequirementVerdict>,
}

impl ConformanceReport {
    /// Quanti requisiti risultano applicati.
    pub fn satisfied(&self) -> usize {
        self.verdicts
            .iter()
            .filter(|v| v.outcome.e_soddisfatto())
            .count()
    }

    /// Quanti risultano NON applicati (scostamento osservato).
    pub fn violated(&self) -> usize {
        self.verdicts
            .iter()
            .filter(|v| v.outcome.e_scostamento())
            .count()
    }

    /// Quanti non erano meccanicamente controllabili. Conteggio ESPOSTO e non
    /// derivato per differenza dal lettore: e' il numero che dice quanta parte
    /// del parere del Consiglio resta fuori dalla misura, e nasconderlo
    /// farebbe leggere "3 su 3 applicati" dove il vero dato e' "3 su 10 misurati".
    pub fn unverifiable(&self) -> usize {
        self.verdicts
            .iter()
            .filter(|v| matches!(v.outcome, RequirementOutcome::NonVerificabile { .. }))
            .count()
    }

    /// Serializza in forma strutturata (regola M: chi decide legge questi campi,
    /// non la nota in prosa).
    ///
    /// Il consumatore in PRODUZIONE oggi e' uno solo, il resoconto del run, e
    /// usa [`Self::nota`]. Questa serializzazione e' il contratto per un
    /// consumatore strutturato — un pannello, una telemetria — e va detto che
    /// oggi non ne esiste uno: dichiararla cablata sarebbe la stessa bugia che
    /// questo modulo esiste per togliere.
    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "total": self.verdicts.len(),
            ESITO_SODDISFATTO: self.satisfied(),
            ESITO_SCOSTAMENTO: self.violated(),
            ESITO_NON_VERIFICABILE: self.unverifiable(),
            CAMPO_REQUISITI: self.verdicts.iter().map(RequirementVerdict::to_value).collect::<Vec<_>>(),
        })
    }

    /// La nota per il resoconto del run. `None` quando non c'e' nulla da dire
    /// (nessun requisito).
    ///
    /// E' l'unico punto in cui la misura diventa testo, come
    /// [`super::correction_progress::CorrectionProgress::fatto_opponibile`]: il
    /// chiamante non ricompone una descrizione dei propri conteggi.
    ///
    /// La nota si scrive ANCHE quando tutto risulta applicato. Un rilievo che
    /// appare solo in caso di scostamento non dice niente sul silenzio: chi
    /// legge non puo' distinguere "verificato, conforme" da "nessuno ha
    /// guardato", ed e' precisamente la distinzione che questo modulo esiste per
    /// rendere visibile.
    pub fn nota(&self) -> Option<String> {
        if self.verdicts.is_empty() {
            return None;
        }
        let mut s = format!(
            "{} Riscontro DETERMINISTICO sul contenuto dei file (nessun \
             giudizio del modello, nessuna dichiarazione dell'agente).",
            self.titolo()
        );
        // Prima gli scostamenti: sono il motivo per cui si legge questa nota.
        s.push_str(&self.rilievi());
        if self.violated() > 0 {
            s.push_str(
                "\n\nIl Consiglio e' advisory: questi scostamenti non hanno fermato il run. \
                 Valuta se il lavoro va completato prima di considerarlo concluso.",
            );
        }
        Some(s)
    }

    /// La prima riga della nota: dice in che situazione si e'.
    ///
    /// Forma NOMINALE per costruzione: "1 su 3 non risultano applicati" e'
    /// sbagliato in italiano, e questa riga la legge un utente.
    fn titolo(&self) -> String {
        let (tot, ok, ko, nv) = (
            self.verdicts.len(),
            self.satisfied(),
            self.violated(),
            self.unverifiable(),
        );
        if ko > 0 {
            format!(
                "**Requisiti del Consiglio NON applicati: {ko} su {tot}** \
                 ({ok} applicati, {nv} non verificabili automaticamente)."
            )
        } else if nv == tot {
            format!(
                "**Requisiti del Consiglio: nessuno verificabile automaticamente \
                 ({tot} in totale).** Il rispetto dei vincoli non e' stato riscontrato: \
                 vanno controllati a mano."
            )
        } else {
            format!(
                "**Requisiti del Consiglio applicati: {ok} su {tot}** \
                 ({nv} non verificabili automaticamente, {ko} non applicati)."
            )
        }
    }

    /// L'elenco per esteso: prima gli scostamenti osservati, poi cio' che non si
    /// e' potuto controllare. I due gruppi restano SEPARATI perche' chiedono
    /// azioni diverse — il primo si corregge, il secondo si guarda a mano — e
    /// mescolarli renderebbe l'elenco una lista di cose "non a posto" in cui la
    /// differenza si perde.
    fn rilievi(&self) -> String {
        let scostamenti = self.verdicts.iter().filter_map(|v| match &v.outcome {
            RequirementOutcome::NonSoddisfatto { evidenza } => {
                Some(format!("\n- NON APPLICATO: {} ({evidenza})", v.requirement))
            }
            _ => None,
        });
        let ignoti = self.verdicts.iter().filter_map(|v| match &v.outcome {
            RequirementOutcome::NonVerificabile { motivo } => Some(format!(
                "\n- NON VERIFICABILE: {} ({})",
                v.requirement,
                motivo.spiegazione()
            )),
            _ => None,
        });
        scostamenti
            .take(MAX_RILIEVI)
            .chain(ignoti.take(MAX_RILIEVI))
            .collect()
    }
}

/// Tetto di rilievi elencati per categoria nella nota. Non e' un campione: i
/// conteggi nel titolo restano completi, e il payload strutturato porta tutti i
/// verdetti. Il taglio riguarda solo quanti se ne stampano per esteso.
const MAX_RILIEVI: usize = 8;

/// Estrae i requisiti dalla sintesi del Consiglio (`advisory_synthesis`).
///
/// Legge il campo `requirements` e SOLO quello: `recommendations` e' l'altra
/// lista, e una raccomandazione non applicata non e' uno scostamento (vedi doc di
/// modulo). Delega al parser unico [`super::advisory_panel::requirement_list`]
/// (regola L): e' lo STESSO formato `{text, direction?}` — o stringa nuda,
/// storico — che quella funzione gia' sa leggere per il livello sotto (il
/// parere grezzo di UNA figura); qui il `Value` e' quello, gia' composto,
/// della sintesi intera dopo l'andata e ritorno attraverso `state.extra`.
/// Vuoto se il campo manca o non e' un array.
pub fn requirements_from_synthesis(synthesis: &Value) -> Vec<super::advisory_panel::Requirement> {
    super::advisory_panel::requirement_list(synthesis, CAMPO_REQUISITI)
}

/// Verbi che indicano "questo deve sparire".
const VERBI_RIMOZIONE: &[&str] = &[
    "rimuov", "rimoz", "elimin", "toglier", "togli", "cancell", "remove", "removal", "delete",
    "drop ", "sopprim",
];

/// Verbi che indicano "questo deve esserci".
///
/// Ne' questa lista ne' [`VERBI_RIMOZIONE`] pretendono di essere una tassonomia
/// dei modi di chiedere una cosa: un requisito il cui verbo non e' riconosciuto
/// diventa [`Unverifiable::DirezioneAssente`], che e' il degrado giusto — si
/// dichiara di non aver capito cosa controllare invece di indovinarlo. I prefissi
/// sono tenuti abbastanza lunghi da non finire dentro parole comuni ("usar" e non
/// "usa", che vivrebbe dentro "causa").
const VERBI_PRESENZA: &[&str] = &[
    "aggiung", "aggiunt", "includ", "inclus", "impost", "inseri", "sostitu", "configur",
    "definisc", "definir", "usar", "utilizz", "add ", "include", "including", "set ", "ensure",
    "introdu",
];

/// Deriva il criterio meccanico da UN requisito, oppure dice perche' non si puo'.
///
/// Non "legge lo stato tecnico dal testo" (lo vieta la regola M): costruisce la
/// DOMANDA da porre al file. La risposta arriva solo da [`judge`], sul contenuto.
///
/// `declared_direction` e' la direzione dichiarata dalla figura ALLA FONTE
/// (campo strutturato `advisory_verdict.requirements[].direction`, regola M):
/// quando presente VINCE su [`direzione`], che resta l'euristica sui verbi
/// solo per i requisiti senza dichiarazione (formato storico, o produttore
/// che non l'ha valorizzata). E' la correzione del bug reale (30/07/2026):
/// "Sostituire `port: 33649`" contiene solo verbi di presenza per
/// l'euristica, che da sola lo leggerebbe come "deve presenziare" invertendo
/// il verdetto su un requisito di rimozione.
pub fn derive_criterion(
    text: &str,
    declared_direction: Option<Direction>,
) -> Result<RequirementCriterion, Unverifiable> {
    derive_criterion_da(text, declared_direction, None, None)
}

/// Come [`derive_criterion`], ma con FILE e LETTERALE eventualmente DICHIARATI
/// alla fonte (campi strutturati di `advisory_verdict.requirements[]`).
///
/// PERCHE' ESISTE. Il criterio spremeva entrambi dalla PROSA — un path fra
/// backtick o riconoscibile a occhio, un letterale fra backtick — e la prosa
/// quei due elementi non li contiene quasi mai. MISURATO l'11/08/2026 sul primo
/// run in cui il riscontro ha davvero girato: **13 requisiti emessi dal
/// Consiglio, ZERO verificabili**, con motivi che sono la fotografia del
/// difetto — «non nomina un file su cui controllare», «non porta un testo
/// preciso da cercare nel file». Il verificatore funzionava: era il requisito a
/// nascere non verificabile.
///
/// Chiedere alla figura di scrivere meglio la frase e' una speranza; chiedere
/// DUE CAMPI e' un contratto. E' la stessa correzione gia' fatta per
/// `direction` (il campo strutturato vince sull'euristica sui verbi), estesa
/// agli altri due elementi della domanda.
///
/// L'estrazione dal testo RESTA come ripiego: i requisiti storici e i produttori
/// che non valorizzano i campi continuano a essere trattati come prima, quindi
/// nessun requisito che oggi si verifica smette di verificarsi.
/// Il criterio dai soli CAMPI dichiarati: `None` se non ci sono (e allora
/// decide la prosa), `Some(esito)` se ci sono — anche quando l'esito e' un
/// rifiuto, perche' un path dichiarato fuori dal progetto non deve poi essere
/// ripescato dal testo.
///
/// Un campo dichiarato ma VUOTO non e' una dichiarazione: cade sul ripiego
/// invece di produrre un criterio che cerca la stringa vuota — che qualunque
/// file soddisfa, cioe' un requisito sempre verde. E' il modo in cui questo fix
/// si trasformerebbe nel suo contrario.
fn criterio_dai_campi(
    text: &str,
    declared_direction: Option<Direction>,
    declared_path: Option<&str>,
    declared_literal: Option<&str>,
) -> Option<Result<RequirementCriterion, Unverifiable>> {
    fn dichiarato(v: Option<&str>) -> Option<&str> {
        v.map(str::trim).filter(|s| !s.is_empty())
    }
    let path = dichiarato(declared_path)?;
    let literal = dichiarato(declared_literal)?;
    if !path_dentro_progetto(path) {
        return Some(Err(Unverifiable::PathFuoriProgetto));
    }
    let Some(direction) = declared_direction.or_else(|| direzione(text)) else {
        return Some(Err(Unverifiable::DirezioneAssente));
    };
    Some(Ok(RequirementCriterion {
        path: path.to_string(),
        literal: literal.to_string(),
        direction,
    }))
}

/// Come [`derive_criterion`], ma con FILE e LETTERALE eventualmente DICHIARATI
/// alla fonte (campi strutturati di `advisory_verdict.requirements[]`).
///
/// PERCHE' ESISTE. Il criterio spremeva entrambi dalla PROSA, e la prosa quei
/// due elementi non li contiene quasi mai. MISURATO l'11/08/2026 sul primo run
/// in cui il riscontro ha davvero girato: 13 requisiti emessi dal Consiglio,
/// ZERO verificabili, coi motivi che sono la fotografia del difetto — «non
/// nomina un file su cui controllare», «non porta un testo preciso da cercare
/// nel file». Il verificatore funzionava: era il requisito a nascere non
/// verificabile.
///
/// Chiedere alla figura di scrivere meglio la frase e' una speranza; chiedere
/// due CAMPI e' un contratto. E' la stessa correzione gia' adottata per
/// `direction` (il campo strutturato vince sull'euristica sui verbi), estesa
/// agli altri due elementi della domanda. L'estrazione dal testo RESTA come
/// ripiego, quindi nessun requisito che oggi si verifica smette di verificarsi.
pub fn derive_criterion_da(
    text: &str,
    declared_direction: Option<Direction>,
    declared_path: Option<&str>,
    declared_literal: Option<&str>,
) -> Result<RequirementCriterion, Unverifiable> {
    // Un campo dichiarato ma VUOTO non e' una dichiarazione: cade sul ripiego,
    // invece di produrre un criterio che cerca la stringa vuota (che qualunque
    // file soddisfa, cioe' un requisito sempre verde).
    if let Some(esito) =
        criterio_dai_campi(text, declared_direction, declared_path, declared_literal)
    {
        return esito;
    }
    let backticked = estrai_backtick(text);
    // I path si cercano PRIMA fra i letterali in backtick (e' li' che una figura
    // scrive un percorso), poi nel testo nudo: "Modificare vite.config.js per
    // includere `server: { strictPort: false }`" nomina il file senza backtick e
    // il letterale con.
    let mut paths: Vec<String> = backticked.iter().filter(|s| sembra_path(s)).cloned().collect();
    if paths.is_empty() {
        paths = parole_nude(text, &backticked)
            .into_iter()
            .filter(|s| sembra_path(s))
            .collect();
    }
    dedup_stabile(&mut paths);
    let path = match paths.len() {
        0 => return Err(Unverifiable::NessunFile),
        1 => paths.remove(0),
        _ => return Err(Unverifiable::PiuFile),
    };
    if !path_dentro_progetto(&path) {
        return Err(Unverifiable::PathFuoriProgetto);
    }

    // Il letterale e' cio' che sta fra backtick e NON e' il path.
    let mut literals: Vec<String> = backticked
        .into_iter()
        .filter(|s| s != &path && !normalizza(s).is_empty())
        .collect();
    dedup_stabile(&mut literals);
    let literal = match literals.len() {
        0 => return Err(Unverifiable::NessunLetterale),
        1 => literals.remove(0),
        _ => return Err(Unverifiable::PiuLetterali),
    };

    let direction = declared_direction
        .or_else(|| direzione(text))
        .ok_or(Unverifiable::DirezioneAssente)?;
    Ok(RequirementCriterion {
        path,
        literal,
        direction,
    })
}

/// La direzione del requisito dai verbi presenti.
///
/// **La rimozione ha precedenza** quando compaiono entrambe le famiglie, ed e'
/// una scelta load-bearing: la forma tipica "Rimuovere `X` e sostituirla con un
/// valore dinamico" contiene sia un verbo di rimozione sia uno di sostituzione,
/// ma il letterale nominato (`X`) e' l'oggetto della RIMOZIONE — il sostituto e'
/// quasi sempre descritto a parole, non citato. Leggerla come "deve presenziare"
/// invertirebbe il verdetto proprio sul caso piu' comune.
fn direzione(text: &str) -> Option<Direction> {
    let lower = text.to_lowercase();
    if VERBI_RIMOZIONE.iter().any(|v| lower.contains(v)) {
        return Some(Direction::DeveMancare);
    }
    if VERBI_PRESENZA.iter().any(|v| lower.contains(v)) {
        return Some(Direction::DevePresenziare);
    }
    None
}

/// Giudica il criterio sul fatto osservato. Nessun I/O: il contenuto arriva gia'
/// letto.
pub fn judge(criterion: &RequirementCriterion, evidence: &FileEvidence) -> RequirementOutcome {
    let contenuto = match evidence {
        FileEvidence::Contenuto(c) => c,
        // Un file assente NON e' un requisito soddisfatto: vedi doc di
        // `Unverifiable::FileAssente`.
        FileEvidence::Assente => {
            return RequirementOutcome::NonVerificabile {
                motivo: Unverifiable::FileAssente,
            }
        }
        FileEvidence::Illeggibile => {
            return RequirementOutcome::NonVerificabile {
                motivo: Unverifiable::FileIlleggibile,
            }
        }
    };
    let riga = riga_del_match(contenuto, &criterion.literal);
    let path = &criterion.path;
    let literal = &criterion.literal;
    match (criterion.direction, riga) {
        (Direction::DeveMancare, Some(n)) => RequirementOutcome::NonSoddisfatto {
            evidenza: format!("`{literal}` compare ancora in {path}, riga {n}"),
        },
        (Direction::DeveMancare, None) => RequirementOutcome::Soddisfatto {
            evidenza: format!("`{literal}` non compare piu' in {path}"),
        },
        (Direction::DevePresenziare, Some(n)) => RequirementOutcome::Soddisfatto {
            evidenza: format!("`{literal}` presente in {path}, riga {n}"),
        },
        (Direction::DevePresenziare, None) => RequirementOutcome::NonSoddisfatto {
            evidenza: format!("`{literal}` non compare in {path}"),
        },
    }
}

/// Compone il report da requisiti e fatti gia' raccolti.
///
/// `leggi` e' il confine I/O parametrizzato: riceve il path relativo dichiarato
/// dal requisito e ritorna il fatto. Il chiamante decide come leggere (e con
/// quali limiti); qui si decide solo cosa significhi cio' che ha letto.
pub fn compose_conformance<F>(
    requirements: &[super::advisory_panel::Requirement],
    mut leggi: F,
) -> ConformanceReport
where
    F: FnMut(&str) -> FileEvidence,
{
    let verdicts = requirements
        .iter()
        .map(|req| match derive_criterion(&req.text, req.direction) {
            Err(motivo) => RequirementVerdict {
                requirement: req.text.clone(),
                criterion: None,
                outcome: RequirementOutcome::NonVerificabile { motivo },
            },
            Ok(criterion) => {
                let evidence = leggi(&criterion.path);
                let outcome = judge(&criterion, &evidence);
                RequirementVerdict {
                    requirement: req.text.clone(),
                    criterion: Some(criterion),
                    outcome,
                }
            }
        })
        .collect();
    ConformanceReport { verdicts }
}

/// Report in cui NESSUN requisito e' controllabile perche' manca la radice del
/// progetto. Esiste come funzione (e non come "salta la verifica") perche' il
/// silenzio e' proprio il difetto: un run senza progetto deve dire che non ha
/// guardato, non far credere che sia tutto a posto.
pub fn conformance_senza_progetto(
    requirements: &[super::advisory_panel::Requirement],
) -> ConformanceReport {
    ConformanceReport {
        verdicts: requirements
            .iter()
            .map(|req| RequirementVerdict {
                requirement: req.text.clone(),
                criterion: None,
                outcome: RequirementOutcome::NonVerificabile {
                    motivo: Unverifiable::ProgettoAssente,
                },
            })
            .collect(),
    }
}

// ── Estrazione: parte meccanica, tenuta piccola e senza casi speciali ────────

/// Testi fra backtick singoli, nell'ordine di apparizione, senza i vuoti.
fn estrai_backtick(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut resto = text;
    while let Some(apertura) = resto.find('`') {
        let dopo = &resto[apertura + 1..];
        let Some(chiusura) = dopo.find('`') else {
            break;
        };
        let inner = dopo[..chiusura].trim();
        if !inner.is_empty() {
            out.push(inner.to_string());
        }
        resto = &dopo[chiusura + 1..];
    }
    out
}

/// Parole del testo FUORI dai backtick, ripulite dalla punteggiatura di coda.
/// I segmenti in backtick sono gia' stati considerati a parte: rientrassero qui,
/// un letterale come `port: 33649` verrebbe spezzato in parole e "33649"
/// finirebbe fra i candidati.
fn parole_nude(text: &str, backticked: &[String]) -> Vec<String> {
    let mut ripulito = text.to_string();
    for b in backticked {
        ripulito = ripulito.replace(b, " ");
    }
    ripulito
        .split_whitespace()
        .map(|w| w.trim_matches(PUNTEGGIATURA_DI_CONTORNO))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Punteggiatura che circonda una parola in una frase e non appartiene al token:
/// "vite.config.js." e "(vite.config.js)" nominano lo stesso file. Il punto c'e'
/// perche' una frase finisce, ed e' anche il separatore dell'estensione — per
/// questo si toglie solo ai BORDI, mai in mezzo.
const PUNTEGGIATURA_DI_CONTORNO: &[char] = &[
    '.', ',', ';', ':', '!', '?', '(', ')', '[', ']', '{', '}', '"', '\'', '`', '<', '>',
];

/// Un token "sembra un path": niente spazi, un'estensione finale MINUSCOLA di
/// 1-10 caratteri (`vite.config.js`, `.env`, `main.rs`).
///
/// Deliberatamente NON c'e' una whitelist di estensioni: sarebbe una lista di
/// varianti da inseguire a ogni linguaggio nuovo, e il costo di un falso positivo
/// qui e' contenuto — un token che non e' un file produce `FileAssente`, cioe' un
/// "non verificabile" onesto, mai un verdetto sbagliato.
///
/// Il vincolo di MINUSCOLA non e' cosmetico: senza, `process.env.PORT` e
/// `import.meta.VITE_API_URL` sono "file" a tutti gli effetti, e un requisito
/// come "in `vite.config.js` usare `process.env.PORT`" — perfettamente
/// verificabile — cadrebbe in [`Unverifiable::PiuFile`] perche' di nomi di file
/// ne conterebbe due. Le estensioni reali sono minuscole; le costanti di
/// ambiente, per convenzione altrettanto solida, non lo sono.
fn sembra_path(token: &str) -> bool {
    if token.is_empty() || token.contains(char::is_whitespace) {
        return false;
    }
    let Some((_, ext)) = token.rsplit_once('.') else {
        return false;
    };
    !ext.is_empty()
        && ext.len() <= 10
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        && ext.chars().any(|c| c.is_ascii_alphabetic())
        && !ext.chars().any(|c| c.is_ascii_uppercase())
}

/// Il path resta dentro il progetto: relativo, senza risalite, senza radice
/// assoluta (POSIX o Windows).
fn path_dentro_progetto(path: &str) -> bool {
    let normalizzato = path.replace('\\', "/");
    if normalizzato.starts_with('/') || normalizzato.contains(':') {
        return false;
    }
    !normalizzato.split('/').any(|seg| seg == "..")
}

/// Dedup stabile in loco (mantiene l'ordine di prima apparizione).
fn dedup_stabile(v: &mut Vec<String>) {
    let mut visti: Vec<String> = Vec::new();
    v.retain(|s| {
        if visti.iter().any(|e| e == s) {
            false
        } else {
            visti.push(s.clone());
            true
        }
    });
}

/// Forma di confronto di un testo: si azzerano le differenze che NON cambiano il
/// significato in un file di configurazione o di codice, e nient'altro.
///
/// E' la forma in cui si confrontano letterale e file: `server: { strictPort:
/// false }` e `server: {\n  strictPort: false,\n}` sono lo stesso vincolo
/// scritto da due formattatori diversi, e un confronto carattere-per-carattere
/// direbbe che il requisito e' stato ignorato — un falso allarme a ogni "vai a
/// capo" del formattatore.
///
/// Le tre differenze neutralizzate sono regole SINTATTICHE generali, non varianti
/// inseguite caso per caso:
///  1. la **spaziatura**, che nessun linguaggio qui rilevante rende significativa
///     dentro un'espressione;
///  2. la **virgola finale** prima di una chiusura (`false,}` = `false}`):
///     consentita e non significativa in JS/TS/Rust/Python/JSON5, ed emessa di
///     default da prettier e rustfmt — cioe' presente quasi sempre nel file e
///     quasi mai nel requisito;
///  3. il **tipo di apice** (`'x'` = `"x"`), intercambiabile negli stessi
///     linguaggi.
///
/// Il case NO: `strictPort` e `strictport` sono due identificatori diversi, e
/// abbassarlo farebbe passare per applicato un vincolo scritto sbagliato.
fn normalizza(s: &str) -> String {
    normalizza_con_righe(s).0
}

/// La normalizzazione, con la riga di provenienza di ciascun carattere
/// conservato.
///
/// E' l'UNICA implementazione della forma di confronto: [`normalizza`] ne scarta
/// la mappa. Tenerle separate significherebbe avere due idee di "stesso testo" —
/// e la seconda a divergere sarebbe quella che riporta la riga all'utente, cioe'
/// l'evidenza su cui si fida.
fn normalizza_con_righe(s: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(s.len());
    let mut righe: Vec<usize> = Vec::with_capacity(s.len());
    let mut riga = 1usize;
    for c in s.chars() {
        if c == '\n' {
            riga += 1;
        }
        if c.is_whitespace() {
            continue;
        }
        if matches!(c, '}' | ']' | ')') && out.ends_with(',') {
            out.pop();
            righe.pop();
        }
        out.push(if c == '\'' { '"' } else { c });
        righe.push(riga);
    }
    (out, righe)
}

/// Numero di riga (1-based) in cui il letterale compare, nella forma di
/// confronto di [`normalizza`]. `None` se non compare.
///
/// La ricerca e' sul contenuto INTERO normalizzato (un letterale puo' essere
/// spezzato su piu' righe dal formattatore); la riga riportata e' quella in cui
/// il match INIZIA. Una ricerca riga-per-riga sarebbe piu' semplice e non
/// vedrebbe proprio i casi multi-riga, che sono la ragione per cui si normalizza.
fn riga_del_match(contenuto: &str, literal: &str) -> Option<usize> {
    let ago = normalizza(literal);
    if ago.is_empty() {
        return None;
    }
    let (pagliaio, righe) = normalizza_con_righe(contenuto);
    let byte_pos = pagliaio.find(&ago)?;
    // Da offset in byte a indice di carattere: `righe` e' indicizzato per
    // carattere conservato, non per byte (un letterale accentato sfaserebbe i due).
    let char_idx = pagliaio[..byte_pos].chars().count();
    righe.get(char_idx).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Il vincolo del caso reale, in UNA sede: i requisiti qui sotto lo
    /// compongono invece di ricopiarlo, cosi' il valore osservato sul progetto e
    /// quello cercato nei file non possono divergere da un test all'altro.
    const PORTA_FISSA: &str = "port: 33649";
    /// Il file del caso reale, in UNA sede (come []).
    const VITE_PATH: &str = "frontend/vite.config.js";
    /// `vite.config.js` come lo lascia un run che NON ha applicato il requisito.
    const VITE_CON_PORTA_FISSA: &str = "export default {\n  server: {\n    port: 33649,\n  },\n}\n";

    /// I due requisiti REALI del run su `gestione-spese` (28/07), quelli che
    /// avevano richiesto tre grep a un umano il giorno dopo.
    fn req_rimozione() -> String {
        format!("Rimuovere `{PORTA_FISSA}` da `frontend/vite.config.js` e sostituirla con un valore dinamico")
    }
    const REQ_PRESENZA: &str =
        "Modificare vite.config.js per includere `server: { strictPort: false }`";
    /// Il terzo: una richiesta descrittiva, che nessuna lettura di file puo'
    /// controllare.
    const REQ_DESCRITTIVO: &str =
        "Aggiungere un health probe che verifichi la disponibilita' della porta prima dell'avvio";

    fn criterio(text: &str) -> RequirementCriterion {
        derive_criterion(text, None).expect("criterio derivabile")
    }

    /// Come [`criterio`], ma con la direzione dichiarata dalla figura (le
    /// nuove regressioni del 30/07/2026 la esercitano esplicitamente).
    fn criterio_con_direzione(text: &str, direction: Direction) -> RequirementCriterion {
        derive_criterion(text, Some(direction)).expect("criterio derivabile")
    }

    /// Il caso (a) della regola O: un requisito APPLICATO risulta soddisfatto.
    /// Il file e' quello che il fix reale ha prodotto (porta dinamica).
    #[test]
    fn requisito_applicato_risulta_soddisfatto() {
        let c = criterio(&req_rimozione());
        assert_eq!(c.path, VITE_PATH);
        assert_eq!(c.literal, PORTA_FISSA);
        assert_eq!(c.direction, Direction::DeveMancare);

        let file = FileEvidence::Contenuto(
            "export default {\n  server: {\n    port: process.env.PORT,\n    strictPort: false,\n  },\n}\n"
                .to_string(),
        );
        let out = judge(&c, &file);
        assert!(out.e_soddisfatto(), "la porta fissa non c'e' piu': {out:?}");
    }

    /// Il caso (b), quello per cui il modulo esiste: un requisito NON applicato
    /// risulta non soddisfatto, con la riga in cui il vincolo e' ancora violato.
    ///
    /// MUTAZIONE: rendere la verifica sempre-vera — in [`judge`], ritornare
    /// `Soddisfatto` su ogni ramo — fa rosseggiare QUESTO test per primo, con
    /// `violated: 0` dove il fatto dice 1.
    #[test]
    fn requisito_ignorato_risulta_non_soddisfatto() {
        let c = criterio(&req_rimozione());
        let file = FileEvidence::Contenuto(VITE_CON_PORTA_FISSA.to_string());
        let out = judge(&c, &file);
        assert!(out.e_scostamento(), "la porta fissa e' ancora li': {out:?}");
        assert!(!out.e_soddisfatto());
        match out {
            RequirementOutcome::NonSoddisfatto { evidenza } => {
                assert!(
                    evidenza.contains("riga 3"),
                    "l'evidenza cita la riga del fatto: {evidenza}"
                );
            }
            altro => panic!("atteso NonSoddisfatto, trovato {altro:?}"),
        }
    }

    /// Il caso (c): un requisito che nessuna lettura di file puo' controllare
    /// risulta NON VERIFICABILE, e non silenziosamente a posto.
    #[test]
    fn requisito_descrittivo_risulta_non_verificabile() {
        // Nessun file nominato: la domanda non e' nemmeno formulabile.
        assert_eq!(
            derive_criterion(REQ_DESCRITTIVO, None),
            Err(Unverifiable::NessunFile)
        );

        let report = compose_conformance(&[REQ_DESCRITTIVO.into()], |_| {
            panic!("non si legge alcun file per un requisito non formulabile")
        });
        assert_eq!(report.unverifiable(), 1);
        assert_eq!(report.satisfied(), 0, "mai contato come soddisfatto");
        assert_eq!(report.violated(), 0, "e nemmeno come scostamento");
    }

    /// La formattazione non e' il vincolo: lo stesso `server: { strictPort: false }`
    /// scritto su tre righe soddisfa il requisito. Senza la normalizzazione della
    /// spaziatura, un requisito rispettato risulterebbe violato ogni volta che il
    /// formattatore va a capo.
    #[test]
    fn la_formattazione_non_cambia_il_verdetto() {
        let c = criterio(REQ_PRESENZA);
        assert_eq!(c.path, "vite.config.js", "il path e' nominato senza backtick");
        assert_eq!(c.direction, Direction::DevePresenziare);

        let compatto = FileEvidence::Contenuto("export default {server:{strictPort:false}}".into());
        let esploso = FileEvidence::Contenuto(
            "export default {\n  server: {\n    strictPort: false,\n  },\n}\n".into(),
        );
        assert!(judge(&c, &compatto).e_soddisfatto());
        assert!(judge(&c, &esploso).e_soddisfatto());

        let assente = FileEvidence::Contenuto("export default {\n  server: {},\n}\n".into());
        assert!(judge(&c, &assente).e_scostamento());
    }

    /// Una costante d'ambiente non e' un nome di file. Senza il vincolo di
    /// estensione minuscola, `process.env.PORT` conta come secondo "file" e
    /// questo requisito — verificabilissimo — finisce in `PiuFile`.
    ///
    /// MUTAZIONE: togliere il controllo sulle maiuscole in [`sembra_path`] rende
    /// questo test rosso con `Err(PiuFile)`.
    #[test]
    fn una_costante_di_ambiente_non_e_un_file() {
        let c = criterio("In `vite.config.js` usare `process.env.PORT` per la porta");
        assert_eq!(c.path, "vite.config.js");
        assert_eq!(c.literal, "process.env.PORT");
        assert_eq!(c.direction, Direction::DevePresenziare);

        let file = FileEvidence::Contenuto("server: { port: process.env.PORT }".into());
        assert!(judge(&c, &file).e_soddisfatto());
    }

    /// La virgola finale la mette il formattatore, non il requisito: `false,}` e
    /// `false}` sono lo stesso vincolo. Senza questa normalizzazione ogni file
    /// passato da prettier o rustfmt risulterebbe non conforme.
    ///
    /// MUTAZIONE: togliere il ramo della virgola in [`normalizza_con_righe`]
    /// rende rosso questo test e `la_formattazione_non_cambia_il_verdetto`.
    #[test]
    fn la_virgola_finale_non_e_una_differenza() {
        let c = criterio(REQ_PRESENZA);
        let con_virgola = FileEvidence::Contenuto("server: { strictPort: false, }".into());
        assert!(judge(&c, &con_virgola).e_soddisfatto());
        // E la riga riportata resta quella giusta anche quando la
        // normalizzazione ha scartato dei caratteri prima del match.
        let multi = "// intestazione\nconst a = [1, 2,];\nserver: { strictPort: false, }\n";
        assert_eq!(riga_del_match(multi, "server: { strictPort: false }"), Some(3));
    }

    /// Il tipo di apice non e' il vincolo: `'0.0.0.0'` e `"0.0.0.0"` sono lo
    /// stesso valore in ogni linguaggio qui rilevante.
    #[test]
    fn gli_apici_sono_intercambiabili() {
        let c = criterio("Modificare `vite.config.js` per includere `host: '0.0.0.0'`");
        let file = FileEvidence::Contenuto("server: { host: \"0.0.0.0\" }".into());
        assert!(judge(&c, &file).e_soddisfatto());
    }

    /// Il case NON si normalizza: `strictport` non e' `strictPort`, e in JS sono
    /// due proprieta' diverse. Se lo si abbassasse, un vincolo scritto sbagliato
    /// passerebbe per applicato.
    #[test]
    fn il_case_conta() {
        let c = criterio(REQ_PRESENZA);
        let sbagliato = FileEvidence::Contenuto("server: { strictport: false }".into());
        assert!(judge(&c, &sbagliato).e_scostamento());
    }

    /// Un file che non c'e' NON e' un requisito rispettato: e' una misura che
    /// non si e' potuta fare. E' l'invariante del modulo — l'incertezza degrada
    /// a "non verificabile", mai a "soddisfatto".
    ///
    /// MUTAZIONE: trattare `FileEvidence::Assente` come "il letterale non c'e',
    /// quindi per `DeveMancare` e' soddisfatto" rende questo test rosso.
    #[test]
    fn file_assente_non_e_soddisfatto() {
        let c = criterio(&req_rimozione());
        let out = judge(&c, &FileEvidence::Assente);
        assert!(!out.e_soddisfatto());
        assert_eq!(
            out,
            RequirementOutcome::NonVerificabile {
                motivo: Unverifiable::FileAssente
            }
        );
        assert!(!judge(&c, &FileEvidence::Illeggibile).e_soddisfatto());
    }

    /// "Rimuovere X e sostituirla con ..." porta entrambe le famiglie di verbi e
    /// UN solo letterale, che e' l'oggetto della rimozione. Se vincesse la
    /// presenza, il verdetto si invertirebbe sul caso piu' comune: il file
    /// corretto (senza `port: 33649`) risulterebbe non conforme.
    #[test]
    fn la_rimozione_ha_precedenza_sui_verbi_di_sostituzione() {
        assert!(req_rimozione().to_lowercase().contains("sostitu"));
        assert_eq!(criterio(&req_rimozione()).direction, Direction::DeveMancare);
    }

    /// Il difetto reale (30/07/2026): "Sostituire `port: 33649`" (senza
    /// "rimuovere") contiene SOLO verbi di presenza per l'euristica — "sostitu"
    /// e' in `VERBI_PRESENZA` — che da sola lo leggerebbe come
    /// `DevePresenziare`, invertendo il verdetto su un requisito di rimozione.
    /// La figura dichiara la direzione alla FONTE: il criterio derivato segue
    /// la dichiarazione, non il verbo.
    ///
    /// MUTAZIONE: ignorare `declared_direction` in [`derive_criterion`]
    /// (tornare sempre a [`direzione`]) rende rosso questo test con
    /// `Direction::DevePresenziare`.
    #[test]
    fn direzione_dichiarata_vince_sul_verbo_ambiguo() {
        let solo_verbo_presenza =
            "Sostituire `port: 33649` in `frontend/vite.config.js` con un valore dinamico";
        // Prova che il testo nudo, senza dichiarazione, e' il difetto: lo
        // legge come presenza perche' "sostitu" e' classificato li'.
        assert_eq!(
            derive_criterion(solo_verbo_presenza, None)
                .expect("criterio derivabile")
                .direction,
            Direction::DevePresenziare,
            "senza dichiarazione l'euristica sui soli verbi legge (erroneamente) presenza"
        );
        assert_eq!(
            criterio_con_direzione(solo_verbo_presenza, Direction::DeveMancare).direction,
            Direction::DeveMancare,
            "la direzione dichiarata dalla figura vince sul verbo ambiguo"
        );
    }

    /// Il verso opposto: un testo senza alcun verbo riconosciuto (oggi
    /// `DirezioneAssente`) diventa verificabile grazie alla direzione
    /// dichiarata — la dichiarazione non serve solo a CORREGGERE un verbo
    /// sbagliato, basta anche da SOLA quando l'euristica non troverebbe nulla.
    #[test]
    fn direzione_dichiarata_di_presenza_e_rispettata() {
        let senza_verbo_riconosciuto = "Il file `vite.config.js` deve avere `strictPort: false`";
        assert_eq!(
            derive_criterion(senza_verbo_riconosciuto, None),
            Err(Unverifiable::DirezioneAssente),
            "senza dichiarazione e senza verbo riconosciuto resta ambiguo"
        );
        let c = criterio_con_direzione(senza_verbo_riconosciuto, Direction::DevePresenziare);
        assert_eq!(c.direction, Direction::DevePresenziare);
    }

    /// Catena intera (regola O): la figura dichiara la direzione nel campo
    /// strutturato del tool `advisory_verdict`, la sintesi del Consiglio la
    /// porta fino al riscontro finale senza che nessun punto in mezzo debba
    /// indovinarla dai verbi. Il testo e' la forma esatta del bug reale.
    #[test]
    fn la_direzione_dichiarata_attraversa_la_sintesi_del_consiglio() {
        use super::super::advisory_panel::{
            compose_advisory_synthesis, AdvisoryPolicy, AdvisoryRoster,
        };
        let testo = "Sostituire `port: 33649` in `frontend/vite.config.js` con una porta dinamica";
        let parere = serde_json::json!({
            "success": true,
            "advisory": {
                "verdict": "block",
                "risks": [{"description": "porta fissa in conflitto"}],
                "requirements": [{"text": testo, "direction": "must_be_absent"}],
                "recommendations": [],
            }
        });
        let synth = compose_advisory_synthesis(
            &[parere],
            &AdvisoryPolicy::default(),
            AdvisoryRoster::Convened(1),
        )
        .expect("sintesi composta");

        let reqs = requirements_from_synthesis(&synth.to_value());
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].direction, Some(Direction::DeveMancare));

        // Porta fissa RIMOSSA: applicato.
        let report_ok = compose_conformance(&reqs, |_| {
            FileEvidence::Contenuto("export default { server: { port: process.env.PORT } }".into())
        });
        assert_eq!(report_ok.satisfied(), 1);

        // Porta fissa ANCORA presente: non applicato (e non "presente per
        // errore", come l'euristica sui soli verbi avrebbe concluso).
        let report_ko = compose_conformance(&reqs, |_| {
            FileEvidence::Contenuto("export default { server: { port: 33649 } }".into())
        });
        assert_eq!(report_ko.violated(), 1);
    }

    /// L'ambiguita' non si risolve tirando a indovinare: piu' file o piu'
    /// letterali distinti -> non verificabile col motivo dichiarato.
    #[test]
    fn ambiguita_dichiarata_non_indovinata() {
        assert_eq!(
            derive_criterion(
                "Rimuovere `port: 1` da `a/vite.config.js` e da `b/vite.config.js`",
                None
            ),
            Err(Unverifiable::PiuFile)
        );
        assert_eq!(
            derive_criterion(
                "In `vite.config.js` sostituire `port: 3` con `process.env.PORT`",
                None
            ),
            Err(Unverifiable::PiuLetterali),
            "due letterali con direzioni opposte: quale valga sarebbe un'ipotesi"
        );
        assert_eq!(
            derive_criterion("Il file `vite.config.js` contiene `port: 3`", None),
            Err(Unverifiable::DirezioneAssente),
            "senza un verbo non si sa cosa ci si aspetti di trovare"
        );
        assert_eq!(
            derive_criterion("Rimuovere `port: 1` da `/etc/nexus/vite.config.js`", None),
            Err(Unverifiable::PathFuoriProgetto)
        );
        assert_eq!(
            derive_criterion("Rimuovere `port: 1` da `../altro/vite.config.js`", None),
            Err(Unverifiable::PathFuoriProgetto)
        );
    }

    /// SOLO i requisiti entrano nella misura: le raccomandazioni sono l'altra
    /// lista e non generano rilievi. Il payload da cui si legge e' quello vero
    /// (`AdvisorySynthesis::to_value`), non un JSON scritto a mano nel test
    /// (regola O).
    #[test]
    fn le_raccomandazioni_non_sono_requisiti() {
        use super::super::advisory_panel::{
            compose_advisory_synthesis, AdvisoryPolicy, AdvisoryRoster,
        };
        let parere = serde_json::json!({
            "success": true,
            "advisory": {
                "verdict": "block",
                "risks": [],
                "requirements": [req_rimozione()],
                "recommendations": [REQ_DESCRITTIVO],
            }
        });
        let synth = compose_advisory_synthesis(
            &[parere],
            &AdvisoryPolicy::default(),
            AdvisoryRoster::Convened(1),
        )
        .expect("sintesi composta");

        let reqs = requirements_from_synthesis(&synth.to_value());
        assert_eq!(reqs, vec![req_rimozione().into()]);
        assert!(
            !reqs.iter().any(|r| r.text == REQ_DESCRITTIVO),
            "una raccomandazione non applicata non e' uno scostamento"
        );
    }

    /// La nota si scrive anche quando tutto e' a posto: il silenzio non
    /// distingue "verificato, conforme" da "nessuno ha guardato".
    #[test]
    fn la_nota_dichiara_anche_la_conformita() {
        let report = compose_conformance(&[req_rimozione().into()], |_| {
            FileEvidence::Contenuto("server: { port: process.env.PORT }".into())
        });
        let nota = report.nota().expect("una nota c'e'");
        assert!(nota.contains("applicati: 1 su 1"), "{nota}");
        assert!(
            nota.contains("DETERMINISTICO"),
            "la nota dichiara come e' stata fatta la misura: {nota}"
        );
    }

    /// Un requisito violato e uno non verificabile: la nota li elenca separati e
    /// il conteggio non li confonde. E' la distinzione che il punto 3 del task
    /// chiede di non perdere.
    #[test]
    fn nota_separa_scostamenti_e_non_verificabili() {
        let report = compose_conformance(
            &[
                req_rimozione().into(),
                REQ_DESCRITTIVO.into(),
                REQ_PRESENZA.into(),
            ],
            |path| match path {
                // La porta fissa e' ancora li'.
                p if p == VITE_PATH => FileEvidence::Contenuto(PORTA_FISSA.into()),
                _ => FileEvidence::Contenuto("server: { strictPort: false }".into()),
            },
        );
        assert_eq!((report.satisfied(), report.violated(), report.unverifiable()), (1, 1, 1));
        let nota = report.nota().expect("una nota c'e'");
        assert!(nota.contains("NON applicati: 1 su 3"), "{nota}");
        assert!(nota.contains("NON APPLICATO:"), "{nota}");
        assert!(nota.contains("NON VERIFICABILE:"), "{nota}");
        assert!(
            nota.contains("advisory"),
            "la nota ricorda che il Consiglio non blocca: {nota}"
        );

        let v = report.to_value();
        assert_eq!(v["total"], 3);
        assert_eq!(v[ESITO_SODDISFATTO], 1);
        assert_eq!(v[ESITO_SCOSTAMENTO], 1);
        assert_eq!(v[ESITO_NON_VERIFICABILE], 1);
        assert_eq!(v[CAMPO_REQUISITI][0]["outcome"], ESITO_SCOSTAMENTO);
        assert_eq!(v[CAMPO_REQUISITI][0]["file"], VITE_PATH);
        assert_eq!(v[CAMPO_REQUISITI][1]["reason"], Unverifiable::NessunFile.as_str());
    }

    /// Senza radice di progetto la misura non si fa, e lo si DICE: il report
    /// esiste, con tutti i requisiti dichiarati non verificabili.
    #[test]
    fn senza_progetto_il_report_lo_dichiara() {
        let report = conformance_senza_progetto(&[req_rimozione().into()]);
        assert_eq!(report.unverifiable(), 1);
        assert_eq!(report.satisfied(), 0);
        let nota = report.nota().expect("una nota c'e'");
        assert!(nota.contains("nessuno verificabile automaticamente (1 in totale)"), "{nota}");
    }

    /// Nessun requisito: nessuna nota. Il Consiglio che non ha posto vincoli non
    /// deve produrre una riga di rumore nel resoconto.
    #[test]
    fn nessun_requisito_nessuna_nota() {
        let report = compose_conformance(&[], |_| panic!("niente da leggere"));
        assert!(report.nota().is_none());
        assert_eq!(report.to_value()["total"], 0);
    }

    /// IL DIFETTO MISURATO l'11/08/2026: 13 requisiti emessi dal Consiglio, ZERO
    /// verificabili. Questo e' uno di quelli veri, parola per parola — prosa
    /// impeccabile che non nomina un file ne' un testo da cercare.
    ///
    /// Coi campi DICHIARATI lo stesso requisito diventa una domanda meccanica.
    /// MUTAZIONE: ignorare `declared_path`/`declared_literal` (cioe' tornare a
    /// spremere la prosa) rende rossa la seconda meta' e riporta il caso a
    /// `NessunFile`, che e' il valore del difetto reale.
    #[test]
    fn i_campi_dichiarati_rendono_verificabile_cio_che_la_prosa_non_dice() {
        let requisito = "La griglia deve essere responsive e ogni scheda prodotto \
                         deve mostrare nome, prezzo e descrizione.";

        // Come nasce oggi dal Consiglio: non verificabile, e il motivo lo dice.
        assert_eq!(
            derive_criterion(requisito, None),
            Err(Unverifiable::NessunFile),
            "e' il caso reale: prosa senza file ne' letterale"
        );

        // Con i due campi dichiarati alla fonte diventa una domanda al file.
        let c = derive_criterion_da(
            requisito,
            Some(Direction::DevePresenziare),
            Some("galleria.html"),
            Some("class=\"product-card\""),
        )
        .expect("coi campi dichiarati il criterio nasce");
        assert_eq!(c.path, "galleria.html");
        assert_eq!(c.literal, "class=\"product-card\"");
        assert_eq!(c.direction, Direction::DevePresenziare);
    }

    /// Un campo dichiarato ma VUOTO non e' una dichiarazione: cade sul ripiego
    /// invece di produrre un criterio che cerca la stringa vuota — che qualunque
    /// file soddisfa, cioe' un requisito sempre verde. E' il modo in cui questo
    /// fix potrebbe trasformarsi nel suo contrario.
    #[test]
    fn un_campo_vuoto_non_e_una_dichiarazione() {
        let r = "In `vite.config.js` deve comparire `strictPort: true`";
        // Vuoti -> si torna a spremere la prosa, che qui basta.
        let c = derive_criterion_da(r, Some(Direction::DevePresenziare), Some("  "), Some(""))
            .expect("ripiego sulla prosa");
        assert_eq!(c.path, "vite.config.js");
        assert_eq!(c.literal, "strictPort: true");
    }

    /// Il path dichiarato resta soggetto al confine del progetto: dichiararlo
    /// non e' un lasciapassare per leggere fuori dalla radice.
    #[test]
    fn il_path_dichiarato_non_scavalca_il_confine_del_progetto() {
        assert_eq!(
            derive_criterion_da(
                "qualunque cosa",
                Some(Direction::DevePresenziare),
                Some("../../etc/passwd"),
                Some("root"),
            ),
            Err(Unverifiable::PathFuoriProgetto)
        );
    }

    /// La riga riportata e' quella giusta anche con accenti prima del match: la
    /// mappa e' per CARATTERE, non per byte. Con un `righe[byte_pos]` questo
    /// test cade (o va in panico) al primo commento accentato del file.
    #[test]
    fn riga_corretta_anche_con_accenti() {
        let contenuto = "// disponibilita' della porta\n// gia' verificato\nport: 33649\n";
        assert_eq!(riga_del_match(contenuto, PORTA_FISSA), Some(3));
    }
}
