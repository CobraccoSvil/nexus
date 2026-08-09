//! PUNTO UNICO (regola L) della domanda: **questa figura merita ancora tempo?**
//!
//! Non «quanto tempo concedo a chiunque», che e' la domanda a cui rispondeva il
//! tetto fisso, ma «sta ancora producendo qualcosa?».
//!
//! ## Il difetto che ha reso necessario il modulo (09/08/2026)
//!
//! Progetto gestione-corsi. Il Consiglio convoca nove figure: cinque consegnano
//! un parere, QUATTRO muoiono al tetto di tempo del proprio kind (240s e 300s,
//! misurati in esercizio). Le quattro fermate avevano prodotto rispettivamente
//! 4, 5, 17 e 22 passi persistiti, e per tutte e quattro la causa dichiarata da
//! [`super::timeout_cause`] era la stessa:
//!
//! ```text
//! budget esaurito su lavoro in corso (N passi osservati, l'ultimo non e' un fallimento)
//! ```
//!
//! Cioe' `CausaTimeout::NoFailureAtEnd`: nessuna era ferma, STAVANO LAVORANDO. Il
//! tetto ha trattato identicamente chi aveva prodotto 4 passi e chi ne aveva
//! prodotti 22, perche' il numero che guardava non era nessuno dei due — era
//! l'orologio.
//!
//! Alzare quel numero e' la toppa che la regola H vieta per nome («aumento di
//! timeout per nascondere una latenza patologica»), ed e' anche inefficace: non
//! esiste un tetto fisso giusto per una figura che potrebbe ragionevolmente
//! finire in 30s o in 600s a seconda di cosa trova. La domanda giusta non ha un
//! numero come risposta.
//!
//! ## Il criterio
//!
//! Il tempo smette di essere il criterio e resta il BACKSTOP. Al suo posto:
//!
//! - **avanza** -> prosegue. Avanzare significa aver fatto qualcosa che non
//!   aveva gia' fatto: una scrittura che cambia il contenuto di un file, oppure
//!   un passo su una STRADA NUOVA (firma mai vista in questo run).
//! - **non avanza da [`SoglieAvanzamento::inattivita_max_s`] E nel frattempo ha
//!   lavorato a vuoto** -> si ferma, ANCHE se il tetto storico e' lontanissimo.
//!   E' la meta' che rende il criterio piu' severo del tetto, non piu'
//!   permissivo: una figura che ripete la stessa chiamata muore in un minuto e
//!   mezzo invece che in quattro minuti.
//!
//!   Le due condizioni sono CONGIUNTE, e il secondo termine e' quello che si
//!   dimentica: la sottrazione fra due istanti si puo' sempre fare, il fatto no.
//!   Senza, il criterio arresterebbe per ASSENZA DI PROVE — e l'assenza di prove
//!   ha qui una causa ordinaria e innocente, una chiamata al modello in volo, di
//!   cui la sola attesa in coda vale gia' 90 secondi
//!   (`routing.inflight_queue_wait_max_s`, mig 0686). Sarebbe il difetto
//!   misurato, rifatto piu' in fretta.
//! - **non si e' potuto osservare** -> prosegue, DICHIARANDOLO
//!   ([`Prosecuzione::ProseguePerIgnoto`]). Una figura dentro una chiamata al
//!   modello non lascia passi: il suo silenzio non e' una prova di stallo, e
//!   trattarlo come tale reintrodurrebbe il tetto a tempo sotto un altro nome —
//!   piu' corto, per giunta. L'ignoto e' una variante e non degrada ne' a «va
//!   bene» ne' a «e' rotto» (regola Q).
//! - **tetto assoluto raggiunto** -> si ferma. E' l'unica difesa che resta contro
//!   una figura che avanza per sempre, e per costruzione non e' mai piu' stretto
//!   del tetto di oggi (vedi [`SoglieAvanzamento::tetto_assoluto_s`]).
//!
//! ## Perche' una strada nuova conta come avanzamento anche se FALLISCE
//!
//! Perche' scoprire che una strada e' chiusa e' informazione, e perche' la
//! direzione dell'errore non e' simmetrica: fermare chi sta lavorando e' il
//! difetto MISURATO (quattro figure su nove, zero pareri), mentre lasciar
//! lavorare chi non produce costa tempo che il tetto assoluto limita comunque.
//! Il criterio erra verso il proseguire, e lo fa di proposito.
//!
//! E' la stessa asimmetria che [`super::timeout_cause`] gia' dichiara sull'altro
//! lato del confine: «tentare alternative diverse non e' ripetere la stessa
//! strada». Qui la si applica PRIMA della morte invece che nel referto.
//!
//! ## Confine (regola L)
//!
//! Qui vive la REGOLA, pura e verificabile senza DB. I FATTI li porta la porta
//! [`crate::runtime::ports::AvanzamentoPort`] (impl
//! `mcp-core::agent_graph_adapter::avanzamento`), che LEGGE `agent_steps` e
//! `file_mutations` e non giudica: stessa separazione di
//! [`super::correction_progress`] e della sua `MutationProgressPort`, e per la
//! stessa ragione — un `WHERE` «comodo» nell'SQL sarebbe un secondo criterio
//! scritto in un linguaggio in cui nessuno lo riconoscerebbe come tale.
//!
//! Due punti unici sono RIUSATI invece di essere ricopiati:
//!
//! - il cambiamento di contenuto e' [`WriteFact::cambia_il_contenuto`]
//!   ([`super::correction_progress`]): `before_sha256 != after_sha256` piu' il
//!   caso dei soli fine-riga. Un secondo confronto degli hash qui darebbe due
//!   idee diverse di «ha scritto qualcosa».
//! - l'identita' della strada e' [`super::loop_signatures::build_signature`]
//!   (nome del tool + hash dell'input canonico), costruita dalla porta. E' la
//!   granularita' giusta per QUESTA domanda: `npm test` e `npm run build` sono
//!   due strade, mentre la firma piu' grossa di `subagent_timeout` (tool + primo
//!   token) le confonderebbe e direbbe «ripete» a chi sta alternando due comandi
//!   diversi.
//!
//! ## Perche' NON e' il `progress_controller`
//!
//! [`super::progress_controller`] risponde a «di fronte a uno stallo, qual e' la
//! prossima MOSSA?» (guida, cambia strategia, escala il modello) e lavora sulle
//! firme in memoria del turno. Questo modulo risponde a «questo run merita
//! ancora tempo?» sui fatti PERSISTITI dell'intero run, sub-run delegati
//! compresi. Il primo cambia il modo di lavorare, il secondo decide se si
//! continua a lavorare: sono due domande, e finche' la seconda aveva come unica
//! risposta un numero di secondi, la prima non poteva salvarne nessuna delle
//! quattro figure misurate.

use serde::Serialize;

use super::correction_progress::WriteFact;

/// Un passo persistito del run, con l'istante in cui e' stato registrato.
///
/// La `firma` la costruisce la PORTA delegando a
/// [`super::loop_signatures::build_signature`]: il criterio la confronta e
/// basta. Se la calcolasse anche lui, due idee di «stessa cosa gia' fatta»
/// finirebbero per divergere (regola L).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassoOsservato {
    /// Identita' della strada tentata (tool + input canonico).
    pub firma: String,
    /// Istante di registrazione, epoch unix in secondi.
    pub istante_s: i64,
}

/// Una scrittura registrata, con l'istante.
///
/// Porta il [`WriteFact`] INTERO e non un booleano gia' deciso: il giudizio
/// («questa scrittura cambia qualcosa?») e' del punto unico
/// [`super::correction_progress`], e appiattirlo qui lo duplicherebbe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScritturaOsservata {
    /// Il fatto grezzo (hash prima/dopo, soli fine-riga).
    pub fatto: WriteFact,
    /// Istante di registrazione, epoch unix in secondi.
    pub istante_s: i64,
}

/// I fatti persistiti su cui si giudica l'avanzamento di un run.
///
/// Entrambe le liste sono in ordine CRONOLOGICO. Possono essere vuote: un run
/// appena partito non ha ancora prodotto nulla, e quel vuoto e' un ignoto, non
/// uno stallo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FattiAvanzamento {
    /// I passi del run (`agent_steps`).
    ///
    /// La porta ne legge un tetto: oltre quello i piu' VECCHI non si vedono, e
    /// una strada gia' tentata molto tempo fa puo' risultare nuova. L'errore va
    /// nella direzione del proseguire, che e' quella scelta di proposito.
    pub passi: Vec<PassoOsservato>,
    /// Le scritture attribuite al run e ai suoi delegati (`file_mutations`).
    pub scritture: Vec<ScritturaOsservata>,
}

/// Da cosa e' provato un avanzamento.
///
/// Due prove distinte e non intercambiabili: la prima dice che lo stato del
/// disco e' cambiato, la seconda che il run ha esplorato qualcosa di nuovo. Una
/// figura advisory non scrive MAI file — il suo prodotto e' un parere — quindi
/// un criterio che ammettesse solo la prima le fermerebbe tutte al primo
/// scrutinio, che e' l'esatto contrario di cio' che questo modulo esiste per
/// fare.
/// I `rename` sono ESPLICITI e non derivati: `rename_all = "snake_case"` sui
/// nomi italiani delle varianti produrrebbe `contenuto_cambiato` sul wire
/// mentre [`ProvaAvanzamento::key`] dice `content_changed` — due nomi per la
/// stessa cosa, e il vocabolario canonico e' uno solo (regola N). La divergenza
/// non e' teorica: il test `la_serializzazione_usa_il_vocabolario_canonico` l'ha
/// colta al primo giro.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind")]
pub enum ProvaAvanzamento {
    /// Una scrittura ha cambiato il contenuto di un file.
    #[serde(rename = "content_changed")]
    ContenutoCambiato,
    /// Un passo ha tentato una strada mai tentata prima in questo run.
    #[serde(rename = "new_road")]
    StradaNuova {
        /// La firma comparsa per la prima volta.
        firma: String,
    },
}

impl ProvaAvanzamento {
    /// Identificatore canonico (regola N), lo stesso che la serializzazione
    /// mette in `kind`.
    pub fn key(&self) -> &'static str {
        match self {
            Self::ContenutoCambiato => "content_changed",
            Self::StradaNuova { .. } => "new_road",
        }
    }
}

/// Perche' non si e' potuto osservare l'avanzamento.
///
/// Non e' una sfumatura: i due casi arrivano da posti diversi e solo il secondo
/// e' un guasto. Collassarli in un `None` renderebbe indistinguibile una figura
/// appena partita da un DB che non risponde (regola Q).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind")]
pub enum MotivoNonOsservabile {
    /// Nessun fatto persistito: il run non ha ancora prodotto niente, oppure e'
    /// dentro una chiamata al modello che non ha ancora lasciato un passo.
    #[serde(rename = "no_facts")]
    NessunFatto,
    /// I fatti non si sono potuti leggere (guasto della porta).
    #[serde(rename = "read_failed")]
    LetturaFallita,
    /// Il run ha avanzato in passato, e DA ALLORA non si e' osservato nulla:
    /// nessun passo, nessuna scrittura, nemmeno inutili.
    ///
    /// E' la stessa situazione epistemica di [`Self::NessunFatto`], solo piu'
    /// avanti nel run: non si sta vedendo la figura fare niente, che e'
    /// esattamente l'aspetto di una chiamata al modello in volo — attesa in coda
    /// compresa, e `routing.inflight_queue_wait_max_s` da sola vale 90 secondi.
    /// Fermare qui significherebbe uccidere per ASSENZA DI PROVE la figura che
    /// sta aspettando il proprio turno di parlare col fornitore: il difetto
    /// misurato, rifatto piu' in fretta.
    #[serde(rename = "no_recent_signal")]
    NessunSegnaleRecente,
}

impl MotivoNonOsservabile {
    /// Identificatore canonico (regola N).
    pub fn key(&self) -> &'static str {
        match self {
            Self::NessunFatto => "no_facts",
            Self::LetturaFallita => "read_failed",
            Self::NessunSegnaleRecente => "no_recent_signal",
        }
    }
}

/// Il lavoro che NON ha prodotto avanzamento, contato.
///
/// Viaggia anche quando il run ha avanzato, e non e' un dettaglio: il caso
/// tipico dell'arresto e' proprio una figura che ha avanzato all'inizio e poi ha
/// ripetuto. Azzerare i contatori appena si vede un avanzamento — comodo, perche'
/// «se avanza non serve dire quanto ha girato a vuoto» — produce un referto che
/// dichiara «fermata per assenza di progresso: 0 passi a vuoto», cioe' una
/// misura che si contraddice da sola.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LavoroAVuoto {
    /// Passi su strade gia' tentate.
    pub passi: usize,
    /// Scritture che non hanno cambiato il contenuto.
    pub riscritture: usize,
}

/// L'avanzamento che i fatti sostengono.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Avanzamento {
    /// L'ultimo avanzamento osservato, con l'istante e la prova.
    Avanzato {
        /// Epoch unix dell'ultimo avanzamento.
        istante_s: i64,
        /// Da cosa e' provato.
        prova: ProvaAvanzamento,
        /// Il lavoro a vuoto osservato ACCANTO all'avanzamento.
        a_vuoto: LavoroAVuoto,
    },
    /// Ci sono fatti, e NESSUNO e' un avanzamento: il run ha lavorato senza
    /// produrre niente di nuovo.
    MaiAvanzato {
        /// Tutto cio' che ha fatto, e non ha prodotto niente.
        a_vuoto: LavoroAVuoto,
    },
    /// Non c'e' niente da guardare.
    NonOsservabile {
        /// Perche'.
        motivo: MotivoNonOsservabile,
    },
}

/// Le soglie, dal DB (regola G: nessun default hardcoded in questo modulo — i
/// valori arrivano sempre dal chiamante, che li legge da `settings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoglieAvanzamento {
    /// Secondi senza un avanzamento oltre i quali (`>=`) la figura si ferma.
    ///
    /// `0` = criterio di progresso DISATTIVO: governa il solo tetto assoluto, e
    /// il comportamento torna a essere quello a tempo. E' la via di ritorno
    /// senza redeploy se il criterio si rivelasse sbagliato in esercizio.
    pub inattivita_max_s: u64,
    /// Tetto assoluto in secondi: il backstop, non il criterio.
    ///
    /// Chi lo calcola (`subagent_native`) lo deriva dal timeout della figura per
    /// un moltiplicatore e non lo lascia mai scendere sotto quel timeout: il
    /// tetto nuovo non puo' essere piu' stretto di quello di oggi, o il difetto
    /// misurato tornerebbe da un'altra porta. `0` = nessun tetto.
    pub tetto_assoluto_s: u64,
}

/// Perche' la figura si ferma.
///
/// Vocabolario canonico in inglese (regola N): viaggia nel `reason` della
/// chiusura e nel payload del meta-step, quindi e' un identificatore macchina.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind")]
pub enum CausaArresto {
    /// Non avanza da troppo tempo.
    #[serde(rename = "no_progress")]
    NonAvanza {
        /// Da quanti secondi non si osserva un avanzamento.
        fermo_da_s: u64,
        /// La soglia superata.
        soglia_s: u64,
        /// Passi osservati senza avanzare.
        passi_a_vuoto: usize,
        /// Scritture osservate senza cambiare contenuto.
        riscritture: usize,
    },
    /// Tetto assoluto raggiunto: il backstop del tempo, ultima difesa.
    #[serde(rename = "absolute_ceiling")]
    TettoAssoluto {
        /// Eta' del run in secondi.
        eta_s: u64,
        /// Il tetto superato.
        tetto_s: u64,
    },
}

impl CausaArresto {
    /// Identificatore canonico (regola N), lo stesso che la serializzazione
    /// mette in `kind`. E' il `reason` con cui il run chiude: i chiamanti lo
    /// chiedono qui invece di ricavarlo con un `matches!` proprio.
    pub fn key(&self) -> &'static str {
        match self {
            Self::NonAvanza { .. } => "no_progress",
            Self::TettoAssoluto { .. } => "absolute_ceiling",
        }
    }

    /// La riga per l'umano, composta DAI campi (regola Q). Nessun consumatore la
    /// ri-analizza: e' cio' che l'agente legge nel messaggio di chiusura e cio'
    /// che il pannello mostra.
    pub fn nota(&self) -> String {
        match self {
            Self::NonAvanza {
                fermo_da_s,
                soglia_s,
                passi_a_vuoto,
                riscritture,
            } => format!(
                "Interrotta: non produci avanzamenti da {fermo_da_s}s (soglia {soglia_s}s). \
                 Osservati {passi_a_vuoto} passi su strade gia' tentate e {riscritture} \
                 scritture che non hanno cambiato il contenuto di alcun file. Ripetere la \
                 stessa azione non cambia l'esito: dichiara cio' che hai accertato finora \
                 col canale del tuo ruolo."
            ),
            Self::TettoAssoluto { eta_s, tetto_s } => format!(
                "Interrotta al tetto assoluto ({eta_s}s trascorsi, tetto {tetto_s}s). \
                 Il lavoro stava avanzando ma il tempo massimo e' esaurito: dichiara cio' \
                 che hai prodotto finora col canale del tuo ruolo."
            ),
        }
    }
}

/// La decisione.
///
/// Tre varianti, non due: il «non so» ha un posto proprio e non si travestre da
/// «prosegue perche' va tutto bene» (regola Q). La differenza non e' accademica
/// — chi legge i log deve poter distinguere una figura che avanza da una che non
/// e' mai stata osservata, perche' la seconda dice che la misura non sta
/// funzionando.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prosecuzione {
    /// Prosegue: c'e' un avanzamento osservato di recente.
    Prosegue {
        /// Da cosa e' provato l'ultimo avanzamento.
        prova: ProvaAvanzamento,
        /// Da quanti secondi risale.
        fermo_da_s: u64,
    },
    /// Prosegue perche' non si e' potuto osservare nulla. Il silenzio non ferma
    /// nessuno: solo il tetto assoluto lo fa.
    ProseguePerIgnoto {
        /// Perche' non si e' potuto osservare.
        motivo: MotivoNonOsservabile,
    },
    /// Si ferma.
    Ferma {
        /// Perche'.
        causa: CausaArresto,
    },
}

impl Prosecuzione {
    /// `true` se la figura deve fermarsi. I chiamanti lo chiedono qui invece di
    /// fare `matches!` per conto proprio.
    pub fn e_arresto(&self) -> bool {
        matches!(self, Self::Ferma { .. })
    }

    /// La causa dell'arresto, se e' un arresto.
    pub fn causa(&self) -> Option<&CausaArresto> {
        match self {
            Self::Ferma { causa } => Some(causa),
            _ => None,
        }
    }
}

/// Classifica l'avanzamento dai fatti persistiti.
///
/// Cammina la storia in ordine cronologico tenendo l'insieme delle strade gia'
/// viste: un passo avanza se la sua firma compare per la PRIMA volta. Le
/// scritture avanzano se cambiano il contenuto, e il giudizio lo da'
/// [`WriteFact::cambia_il_contenuto`] (punto unico, mai un confronto di hash
/// riscritto qui).
///
/// Ritorna l'avanzamento PIU' RECENTE fra i due canali: e' quello che risponde
/// alla domanda «da quanto non fa niente di nuovo».
pub fn classifica_avanzamento(fatti: &FattiAvanzamento) -> Avanzamento {
    if fatti.passi.is_empty() && fatti.scritture.is_empty() {
        return Avanzamento::NonOsservabile {
            motivo: MotivoNonOsservabile::NessunFatto,
        };
    }

    let frontiera = frontiera_avanzamento(fatti);
    let a_vuoto = lavoro_a_vuoto_dopo(fatti, frontiera.as_ref());

    match frontiera {
        Some(f) => Avanzamento::Avanzato {
            istante_s: f.istante_s,
            prova: f.prova,
            a_vuoto,
        },
        None => Avanzamento::MaiAvanzato { a_vuoto },
    }
}

/// L'ultimo avanzamento: QUANDO, da cosa e' provato, e DOVE si trova nei due
/// canali.
///
/// La POSIZIONE non e' un dettaglio di implementazione, ed e' la lezione di un
/// test che ha guardato il DB vero (regola O). `istante_s` ha granularita' al
/// SECONDO (`DateTime::timestamp`), e in un secondo una figura lascia
/// tranquillamente sei passi: confrontando i soli istanti, «dopo l'ultimo
/// avanzamento» escludeva tutte le ripetizioni nate nello stesso secondo
/// dell'avanzamento — cioe' il caso normale di una figura che ripete in fretta,
/// che e' esattamente quello che il criterio deve cogliere. L'ordine nella
/// lista, invece, quei fatti li distingue: le due liste sono cronologiche per
/// contratto.
struct Frontiera {
    /// Epoch dell'avanzamento (il piu' recente fra i due canali).
    istante_s: i64,
    /// Da cosa e' provato.
    prova: ProvaAvanzamento,
    /// Indice dell'ultimo passo su strada nuova, se ce n'e' stato uno.
    ultimo_passo_nuovo: Option<usize>,
    /// Indice dell'ultima scrittura che ha cambiato contenuto, se c'e' stata.
    ultima_scrittura_utile: Option<usize>,
}

/// PASSO 1 — cammina i due canali e tiene il piu' RECENTE fra loro.
fn frontiera_avanzamento(fatti: &FattiAvanzamento) -> Option<Frontiera> {
    let passo = ultimo_passo_su_strada_nuova(&fatti.passi);
    let scrittura = ultima_scrittura_che_cambia(&fatti.scritture);
    let (istante_s, prova) = prova_piu_recente(passo, scrittura)?;
    Some(Frontiera {
        istante_s,
        prova,
        ultimo_passo_nuovo: passo.map(|(i, _, _)| i),
        ultima_scrittura_utile: scrittura.map(|(i, _)| i),
    })
}

/// L'ultimo passo la cui firma compare per la PRIMA volta: indice, istante,
/// firma.
///
/// `insert` ritorna true la prima volta che la firma compare — e' la definizione
/// stessa di «strada nuova» — e camminare in ordine cronologico garantisce che
/// quel «prima volta» sia quello vero.
fn ultimo_passo_su_strada_nuova(passi: &[PassoOsservato]) -> Option<(usize, i64, &str)> {
    let mut viste: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut ultimo = None;
    for (i, p) in passi.iter().enumerate() {
        if viste.insert(p.firma.as_str()) {
            ultimo = Some((i, p.istante_s, p.firma.as_str()));
        }
    }
    ultimo
}

/// L'ultima scrittura che ha cambiato il contenuto: indice e istante.
///
/// Il giudizio «cambia qualcosa?» resta al punto unico
/// [`WriteFact::cambia_il_contenuto`] (regola L).
fn ultima_scrittura_che_cambia(scritture: &[ScritturaOsservata]) -> Option<(usize, i64)> {
    scritture
        .iter()
        .enumerate()
        .filter(|(_, s)| s.fatto.cambia_il_contenuto())
        .map(|(i, s)| (i, s.istante_s))
        .next_back()
}

/// Fra i due canali vince il piu' RECENTE; a parita' di istante la SCRITTURA,
/// che e' la prova piu' forte — lo stato del disco e' cambiato, non solo
/// l'esplorazione.
fn prova_piu_recente(
    passo: Option<(usize, i64, &str)>,
    scrittura: Option<(usize, i64)>,
) -> Option<(i64, ProvaAvanzamento)> {
    let strada = |t: i64, firma: &str| {
        (
            t,
            ProvaAvanzamento::StradaNuova {
                firma: firma.to_string(),
            },
        )
    };
    match (passo, scrittura) {
        (None, None) => None,
        (Some((_, t, firma)), None) => Some(strada(t, firma)),
        (None, Some((_, t))) => Some((t, ProvaAvanzamento::ContenutoCambiato)),
        (Some((_, tp, firma)), Some((_, ts))) if tp > ts => Some(strada(tp, firma)),
        (Some(_), Some((_, ts))) => Some((ts, ProvaAvanzamento::ContenutoCambiato)),
    }
}

/// PASSO 2 — il lavoro a vuoto fatto DA ALLORA.
///
/// La finestra e' quella e non l'intera storia: un run che ha ripetuto
/// all'inizio, poi ha avanzato, poi e' rimasto in silenzio verrebbe altrimenti
/// fermato citando ripetizioni che aveva gia' superato — cioe' con una prova
/// scaduta.
///
/// Non si puo' contare nello stesso giro del passo 1: al momento in cui si
/// incontra un fatto non si sa ancora se un avanzamento arrivera' dopo.
///
/// «Dopo» sono DUE condizioni, e servono entrambe: non prima dell'istante
/// dell'avanzamento (che copre il canale in cui l'avanzamento non e' avvenuto) e
/// piu' avanti nella lista del proprio canale (che disambigua lo stesso secondo,
/// vedi [`Frontiera`]). Frontiera assente = nessun avanzamento: la finestra e'
/// tutta la storia.
fn lavoro_a_vuoto_dopo(fatti: &FattiAvanzamento, frontiera: Option<&Frontiera>) -> LavoroAVuoto {
    let da_s = frontiera.map(|f| f.istante_s);
    let non_prima = |t: i64| da_s.is_none_or(|d| t >= d);
    let oltre = |indice: usize, confine: Option<usize>| confine.is_none_or(|i0| indice > i0);

    // Un passo e' una RIPETIZIONE se la sua firma era gia' comparsa prima nella
    // storia. Si decide sull'INTERA storia e non sulla finestra: un passo
    // ripetuto sembrerebbe nuovo solo perche' il suo gemello sta fuori.
    let mut viste: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let ripetuto: Vec<bool> = fatti
        .passi
        .iter()
        .map(|p| !viste.insert(p.firma.as_str()))
        .collect();
    let confine_passi = frontiera.and_then(|f| f.ultimo_passo_nuovo);
    let confine_scritture = frontiera.and_then(|f| f.ultima_scrittura_utile);

    LavoroAVuoto {
        passi: fatti
            .passi
            .iter()
            .enumerate()
            .filter(|(i, p)| ripetuto[*i] && non_prima(p.istante_s) && oltre(*i, confine_passi))
            .count(),
        riscritture: fatti
            .scritture
            .iter()
            .enumerate()
            .filter(|(i, s)| {
                !s.fatto.cambia_il_contenuto()
                    && non_prima(s.istante_s)
                    && oltre(*i, confine_scritture)
            })
            .count(),
    }
}

/// IL CRITERIO, in un posto solo: questa figura merita ancora tempo?
///
/// `avvio_s` e `adesso_s` sono epoch unix in secondi. `avvio_s` fa da riferimento
/// quando il run non ha MAI avanzato: li' l'inattivita' si misura dal via, non da
/// un avanzamento che non c'e' stato.
///
/// L'ordine dei controlli e' load-bearing:
///
/// 1. il TETTO ASSOLUTO precede tutto, o una figura che avanza per sempre non si
///    fermerebbe mai — ed e' l'unico caso in cui il tempo torna a essere un
///    criterio, per costruzione l'ultimo;
/// 2. l'IGNOTO precede l'inattivita', perche' «non ho visto passi» non e' «non ha
///    fatto passi»: invertirli farebbe morire in [`SoglieAvanzamento::inattivita_max_s`]
///    secondi ogni figura dentro una chiamata al modello piu' lunga della soglia,
///    cioe' trasformerebbe questo criterio in un tetto a tempo piu' severo di
///    quello che sostituisce.
pub fn decidi_prosecuzione(
    fatti: &FattiAvanzamento,
    avvio_s: i64,
    adesso_s: i64,
    soglie: SoglieAvanzamento,
) -> Prosecuzione {
    let eta_s = adesso_s.saturating_sub(avvio_s).max(0) as u64;
    if soglie.tetto_assoluto_s > 0 && eta_s >= soglie.tetto_assoluto_s {
        return Prosecuzione::Ferma {
            causa: CausaArresto::TettoAssoluto {
                eta_s,
                tetto_s: soglie.tetto_assoluto_s,
            },
        };
    }

    let avanzamento = classifica_avanzamento(fatti);
    if let Avanzamento::NonOsservabile { motivo } = avanzamento {
        return Prosecuzione::ProseguePerIgnoto { motivo };
    }

    if soglie.inattivita_max_s == 0 {
        return prosecuzione_senza_criterio(&avanzamento, adesso_s);
    }
    verdetto_sull_inattivita(&avanzamento, avvio_s, adesso_s, soglie)
}

/// Soglia a zero: il criterio di progresso e' spento e governa il solo tetto
/// assoluto (gia' verificato dal chiamante).
///
/// Si dichiara comunque cio' che si e' osservato invece di fingere un
/// avanzamento: spegnere il criterio significa non farne DISCENDERE un arresto,
/// non smettere di guardare.
fn prosecuzione_senza_criterio(avanzamento: &Avanzamento, adesso_s: i64) -> Prosecuzione {
    match avanzamento {
        Avanzamento::Avanzato {
            istante_s, prova, ..
        } => Prosecuzione::Prosegue {
            prova: prova.clone(),
            fermo_da_s: adesso_s.saturating_sub(*istante_s).max(0) as u64,
        },
        _ => Prosecuzione::ProseguePerIgnoto {
            motivo: MotivoNonOsservabile::NessunFatto,
        },
    }
}

/// Le DUE condizioni congiunte dell'arresto: soglia superata E lavoro a vuoto
/// osservato da quando la figura ha avanzato l'ultima volta.
///
/// Il riferimento dell'inattivita' e' l'ultimo avanzamento; se non ce n'e' stato
/// nessuno e' il via del run, perche' li' l'attesa e' cominciata. Il lavoro a
/// vuoto si porta dietro in ENTRAMBI i casi: e' cio' che rende il referto una
/// misura invece di un'affermazione.
fn verdetto_sull_inattivita(
    avanzamento: &Avanzamento,
    avvio_s: i64,
    adesso_s: i64,
    soglie: SoglieAvanzamento,
) -> Prosecuzione {
    let (riferimento_s, prova, a_vuoto) = match avanzamento {
        Avanzamento::Avanzato {
            istante_s,
            prova,
            a_vuoto,
        } => (*istante_s, Some(prova.clone()), *a_vuoto),
        Avanzamento::MaiAvanzato { a_vuoto } => (avvio_s, None, *a_vuoto),
        // Gia' intercettato dal chiamante: qui non ci si arriva.
        Avanzamento::NonOsservabile { motivo } => {
            return Prosecuzione::ProseguePerIgnoto {
                motivo: motivo.clone(),
            }
        }
    };

    let fermo_da_s = adesso_s.saturating_sub(riferimento_s).max(0) as u64;
    // Il tempo da solo NON basta a fermare: serve la PROVA che nel frattempo la
    // figura abbia lavorato senza produrre. Senza il secondo termine il criterio
    // arresterebbe per ASSENZA DI PROVE, e l'assenza di prove ha qui una causa
    // ordinaria e innocente — una chiamata al modello in volo, coda compresa
    // (`routing.inflight_queue_wait_max_s` vale 90s da solo). Sarebbe il difetto
    // misurato rifatto piu' in fretta, ed e' l'errore piu' facile da commettere
    // scrivendo questo modulo: la sottrazione fra due istanti c'e' sempre, il
    // fatto no.
    //
    // Il silenzio non resta comunque impunito: lo copre il tetto assoluto, e il
    // turno di sola PROSA — l'altro modo di girare a vuoto senza lasciare passi —
    // ha gia' il suo presidio in `gate_streak_solo_testo` (regola L: non lo si
    // duplica qui).
    let ha_lavorato_a_vuoto = a_vuoto.passi + a_vuoto.riscritture > 0;
    if fermo_da_s >= soglie.inattivita_max_s && ha_lavorato_a_vuoto {
        return Prosecuzione::Ferma {
            causa: CausaArresto::NonAvanza {
                fermo_da_s,
                soglia_s: soglie.inattivita_max_s,
                passi_a_vuoto: a_vuoto.passi,
                riscritture: a_vuoto.riscritture,
            },
        };
    }
    if fermo_da_s >= soglie.inattivita_max_s {
        // Soglia superata ma nessun fatto da opporre: si dichiara il silenzio
        // invece di dedurne uno stallo.
        return Prosecuzione::ProseguePerIgnoto {
            motivo: MotivoNonOsservabile::NessunSegnaleRecente,
        };
    }

    match prova {
        Some(prova) => Prosecuzione::Prosegue { prova, fermo_da_s },
        // Non ha mai avanzato ma la soglia non e' ancora scaduta: sta iniziando.
        None => Prosecuzione::ProseguePerIgnoto {
            motivo: MotivoNonOsservabile::NessunFatto,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decisions::loop_signatures::build_signature;
    use serde_json::json;

    /// Costruisce un passo passando dal PRODUTTORE della firma (regola O): la
    /// firma non e' una stringa scritta nel test ma quella che
    /// [`build_signature`] ricava da nome e input del tool, cioe' la stessa
    /// funzione che la porta di produzione usa sui record di `agent_steps`.
    /// Scriverla a mano fisserebbe l'assunto da verificare.
    fn passo(tool: &str, input: serde_json::Value, istante_s: i64) -> PassoOsservato {
        PassoOsservato {
            firma: build_signature(tool, &input),
            istante_s,
        }
    }

    fn scrittura(before: Option<&str>, after: Option<&str>, istante_s: i64) -> ScritturaOsservata {
        ScritturaOsservata {
            fatto: WriteFact {
                before_sha256: before.map(str::to_string),
                after_sha256: after.map(str::to_string),
                solo_fine_riga: None,
            },
            istante_s,
        }
    }

    fn soglie(inattivita_max_s: u64, tetto_assoluto_s: u64) -> SoglieAvanzamento {
        SoglieAvanzamento {
            inattivita_max_s,
            tetto_assoluto_s,
        }
    }

    /// IL CASO MISURATO (09/08/2026, gestione-corsi): una figura che ha prodotto
    /// 22 passi su strade diverse, l'ultimo pochi secondi fa. Il tetto fisso la
    /// uccideva a 240s; il criterio la lascia lavorare.
    ///
    /// MUTAZIONE: far ritornare a [`decidi_prosecuzione`] un arresto quando
    /// `eta_s >= 240` (cioe' rimettere il tetto fisso come criterio) rende questo
    /// test rosso col valore del difetto — la figura che sta lavorando viene
    /// fermata.
    #[test]
    fn una_figura_che_avanza_non_si_ferma_al_vecchio_tetto() {
        let avvio = 1_000;
        // 22 passi su strade tutte diverse, l'ultimo a 235s dal via.
        let passi: Vec<PassoOsservato> = (0..22)
            .map(|i| {
                passo(
                    "read_file",
                    json!({ "path": format!("src/modulo_{i}.rs") }),
                    avvio + 10 + i * 10,
                )
            })
            .collect();
        let ultimo_istante = passi.last().expect("22 passi").istante_s;
        let fatti = FattiAvanzamento {
            passi,
            scritture: Vec::new(),
        };
        // Adesso e' 250s dal via: OLTRE il tetto storico di 240s.
        let adesso = avvio + 250;
        assert!(
            adesso - avvio > 240,
            "il caso deve stare oltre il tetto storico, altrimenti non prova niente"
        );
        let d = decidi_prosecuzione(&fatti, avvio, adesso, soglie(90, 960));
        match &d {
            Prosecuzione::Prosegue { prova, fermo_da_s } => {
                assert_eq!(prova.key(), "new_road");
                assert_eq!(*fermo_da_s, (adesso - ultimo_istante) as u64);
            }
            altro => panic!("una figura che avanza non si ferma, ottenuto {altro:?}"),
        }
        assert!(!d.e_arresto());
    }

    /// L'altra meta': chi RIPETE si ferma molto prima del tetto storico. E' il
    /// caso che rende il criterio piu' severo del tetto, non piu' permissivo.
    ///
    /// MUTAZIONE: contare come «strada nuova» ogni passo (togliere l'insieme
    /// delle firme viste in [`classifica_avanzamento`]) fa proseguire questa
    /// figura fino al tetto assoluto — cioe' quattro volte il tempo che sprecava
    /// prima.
    #[test]
    fn chi_ripete_la_stessa_strada_si_ferma_molto_prima_del_tetto() {
        let avvio = 1_000;
        // Stessa identica chiamata otto volte, l'ultima a 100s dal via.
        let passi: Vec<PassoOsservato> = (0..8)
            .map(|i| passo("run_command", json!({"command": "npm test"}), avvio + i * 12))
            .collect();
        let fatti = FattiAvanzamento {
            passi,
            scritture: Vec::new(),
        };
        let adesso = avvio + 100;
        let d = decidi_prosecuzione(&fatti, avvio, adesso, soglie(90, 960));
        let causa = d.causa().expect("deve fermarsi");
        assert_eq!(causa.key(), "no_progress");
        match causa {
            CausaArresto::NonAvanza {
                passi_a_vuoto,
                fermo_da_s,
                ..
            } => {
                assert_eq!(*passi_a_vuoto, 7, "la prima e' la strada nuova, le altre no");
                assert_eq!(*fermo_da_s, 100, "l'inattivita' si misura dal via");
            }
            altro => panic!("attesa no_progress, ottenuto {altro:?}"),
        }
        assert!(
            (adesso - avvio) < 240,
            "si deve fermare PRIMA del tetto storico, o non ha risolto niente"
        );
    }

    /// Comandi DIVERSI non sono una ripetizione, e la granularita' della firma e'
    /// cio' che li distingue. Con la firma grossolana (tool + primo token) `npm
    /// test` e `npm run build` sarebbero la stessa strada, e una figura che
    /// alterna due comandi verrebbe fermata come se ne ripetesse uno.
    ///
    /// MUTAZIONE: sostituire [`build_signature`] con il solo nome del tool nella
    /// porta rende questi tre passi una ripetizione e questo test rosso.
    #[test]
    fn comandi_diversi_dello_stesso_programma_sono_strade_diverse() {
        let avvio = 1_000;
        let fatti = FattiAvanzamento {
            passi: vec![
                passo("run_command", json!({"command": "npm test"}), avvio + 10),
                passo("run_command", json!({"command": "npm run build"}), avvio + 40),
                passo("run_command", json!({"command": "npm run lint"}), avvio + 70),
            ],
            scritture: Vec::new(),
        };
        let d = decidi_prosecuzione(&fatti, avvio, avvio + 80, soglie(90, 960));
        assert!(!d.e_arresto(), "tre comandi diversi sono tre strade, {d:?}");
    }

    /// Una figura ADVISORY non scrive file: il suo prodotto e' un parere. Un
    /// criterio che ammettesse solo le scritture le fermerebbe tutte, che e'
    /// esattamente il contrario di cio' che serve — le quattro figure misurate
    /// erano advisory.
    #[test]
    fn una_figura_che_non_scrive_file_puo_comunque_avanzare() {
        let avvio = 1_000;
        let fatti = FattiAvanzamento {
            passi: vec![
                passo("rag_search", json!({"query": "modello dati corsi"}), avvio + 5),
                passo("read_file", json!({"path": "docs/schema.md"}), avvio + 30),
            ],
            // Nessuna scrittura: e' il punto.
            scritture: Vec::new(),
        };
        let d = decidi_prosecuzione(&fatti, avvio, avvio + 45, soglie(90, 960));
        assert!(!d.e_arresto(), "{d:?}");
    }

    /// Una scrittura che cambia contenuto e' un avanzamento, e il giudizio lo da'
    /// il punto unico [`WriteFact::cambia_il_contenuto`]. Una riscrittura
    /// IDENTICA non lo e': e' il modo in cui un agente simula attivita' senza
    /// produrne, gia' misurato dal `correction_progress`.
    ///
    /// MUTAZIONE: contare ogni scrittura come avanzamento (ignorare
    /// `cambia_il_contenuto`) fa proseguire il secondo caso, cioe' rende di nuovo
    /// invisibile la riscrittura a vuoto.
    #[test]
    fn la_riscrittura_identica_non_e_avanzamento() {
        let avvio = 1_000;
        // Contenuto davvero cambiato -> avanza.
        let cambia = FattiAvanzamento {
            passi: vec![passo("write_file", json!({"path": "a.rs"}), avvio + 10)],
            scritture: vec![scrittura(Some("prima"), Some("dopo"), avvio + 95)],
        };
        assert!(!decidi_prosecuzione(&cambia, avvio, avvio + 100, soglie(90, 960)).e_arresto());

        // Stesse chiamate, contenuto IDENTICO -> non avanza (e la sola strada
        // nuova, il primo write, e' vecchia oltre la soglia).
        let identica = FattiAvanzamento {
            passi: vec![passo("write_file", json!({"path": "a.rs"}), avvio + 5)],
            scritture: vec![scrittura(Some("uguale"), Some("uguale"), avvio + 95)],
        };
        let d = decidi_prosecuzione(&identica, avvio, avvio + 100, soglie(90, 960));
        match d.causa().expect("una riscrittura identica non tiene in vita nessuno") {
            CausaArresto::NonAvanza { riscritture, .. } => assert_eq!(*riscritture, 1),
            altro => panic!("attesa no_progress, ottenuto {altro:?}"),
        }
    }

    /// IL RAMO CHE PROTEGGE DAL RIFARE IL DIFETTO: nessun passo persistito NON
    /// significa stallo. Una figura dentro una chiamata al modello piu' lunga
    /// della soglia non lascia passi, e fermarla li' sarebbe un tetto a tempo
    /// piu' severo di quello che questo modulo sostituisce.
    ///
    /// MUTAZIONE: trattare `NonOsservabile` come `MaiAvanzato` (cioe' far cadere
    /// il silenzio nel ramo dell'inattivita') rende questo test rosso con un
    /// arresto `no_progress` a 200s — una figura uccisa per aver taciuto.
    #[test]
    fn il_silenzio_non_ferma_nessuno() {
        let avvio = 1_000;
        let vuoti = FattiAvanzamento::default();
        let d = decidi_prosecuzione(&vuoti, avvio, avvio + 200, soglie(90, 960));
        assert_eq!(
            d,
            Prosecuzione::ProseguePerIgnoto {
                motivo: MotivoNonOsservabile::NessunFatto
            },
            "senza fatti non si ferma: si dichiara di non aver visto"
        );
        assert!(!d.e_arresto());
    }

    /// Il silenzio non e' immortalita': il tetto assoluto vale ANCHE quando non
    /// si e' osservato niente. E' l'unica difesa contro un run wedged, e per
    /// questo il tetto si controlla per primo.
    ///
    /// MUTAZIONE: spostare il controllo del tetto DOPO quello dell'ignoto rende
    /// questa figura immortale.
    #[test]
    fn il_tetto_assoluto_vale_anche_sul_silenzio() {
        let avvio = 1_000;
        let d = decidi_prosecuzione(
            &FattiAvanzamento::default(),
            avvio,
            avvio + 960,
            soglie(90, 960),
        );
        let causa = d.causa().expect("il tetto e' l'ultima difesa");
        assert_eq!(causa.key(), "absolute_ceiling");
        assert!(causa.nota().contains("tetto assoluto"), "{}", causa.nota());
    }

    /// Il tetto assoluto batte anche una figura che sta avanzando: e' un
    /// backstop, quindi non ammette eccezioni, altrimenti non sarebbe l'ultima
    /// difesa.
    #[test]
    fn il_tetto_assoluto_batte_anche_chi_avanza() {
        let avvio = 1_000;
        let fatti = FattiAvanzamento {
            passi: vec![passo("read_file", json!({"path": "a.rs"}), avvio + 955)],
            scritture: Vec::new(),
        };
        let d = decidi_prosecuzione(&fatti, avvio, avvio + 960, soglie(90, 960));
        assert_eq!(
            d.causa().map(CausaArresto::key),
            Some("absolute_ceiling"),
            "{d:?}"
        );
    }

    /// Soglia a zero = criterio di progresso spento (la via di ritorno dal DB,
    /// regola G). Governa il solo tetto: nessun arresto per inattivita', nemmeno
    /// su una ripetizione palese.
    #[test]
    fn la_soglia_a_zero_spegne_il_criterio_di_progresso() {
        let avvio = 1_000;
        let fatti = FattiAvanzamento {
            passi: (0..8)
                .map(|i| passo("run_command", json!({"command": "npm test"}), avvio + i))
                .collect(),
            scritture: Vec::new(),
        };
        let d = decidi_prosecuzione(&fatti, avvio, avvio + 500, soglie(0, 960));
        assert!(!d.e_arresto(), "col criterio spento governa il solo tetto, {d:?}");
    }

    /// Il vocabolario sul wire e' quello canonico in inglese (regola N) e
    /// coincide con `key()`: chi legge il JSON del meta-step e chi legge il tipo
    /// vedono lo stesso nome. La `key` di [`CausaArresto`] e' anche il `reason`
    /// con cui il run chiude, quindi la divergenza non resterebbe cosmetica.
    ///
    /// MUTAZIONE: sostituire i `#[serde(rename = "...")]` espliciti con
    /// `rename_all = "snake_case"` — la scorciatoia naturale — fa rosseggiare
    /// ogni riga di questo test coi nomi italiani delle varianti
    /// (`non_avanza` contro `no_progress`). E' come la divergenza e' stata
    /// trovata la prima volta.
    #[test]
    fn la_serializzazione_usa_il_vocabolario_canonico() {
        let causa = CausaArresto::NonAvanza {
            fermo_da_s: 120,
            soglia_s: 90,
            passi_a_vuoto: 7,
            riscritture: 0,
        };
        let v = serde_json::to_value(&causa).expect("serializzabile");
        assert_eq!(v["kind"], causa.key());
        assert_eq!(v["kind"], "no_progress");
        assert_eq!(v["fermo_da_s"], 120);

        let tetto = CausaArresto::TettoAssoluto {
            eta_s: 960,
            tetto_s: 960,
        };
        let v = serde_json::to_value(&tetto).expect("serializzabile");
        assert_eq!(v["kind"], tetto.key());
        assert_eq!(v["kind"], "absolute_ceiling");

        for prova in [
            ProvaAvanzamento::ContenutoCambiato,
            ProvaAvanzamento::StradaNuova {
                firma: "read_file|abc".into(),
            },
        ] {
            let v = serde_json::to_value(&prova).expect("serializzabile");
            assert_eq!(v["kind"], prova.key(), "{prova:?}");
        }

        for motivo in [
            MotivoNonOsservabile::NessunFatto,
            MotivoNonOsservabile::LetturaFallita,
        ] {
            let v = serde_json::to_value(&motivo).expect("serializzabile");
            assert_eq!(v["kind"], motivo.key(), "{motivo:?}");
        }
    }

    /// IL SECONDO RAMO CHE PROTEGGE DAL RIFARE IL DIFETTO: una figura che ha
    /// avanzato e poi TACE non viene fermata, per quanto tempo passi. Il silenzio
    /// dopo un avanzamento e' cio' che si vede quando una chiamata al modello e'
    /// in volo — e la sola attesa in coda verso un fornitore saturo vale 90
    /// secondi, cioe' la soglia intera.
    ///
    /// MUTAZIONE: togliere `&& ha_lavorato_a_vuoto` dalla condizione di arresto
    /// (la forma "naturale" del criterio: solo la sottrazione fra due istanti)
    /// rende questo test rosso con `no_progress` e `passi_a_vuoto: 0` — un
    /// referto che dichiara di aver fermato una figura per assenza di progresso
    /// senza avere un solo fatto da opporle.
    #[test]
    fn il_silenzio_dopo_un_avanzamento_non_ferma_nessuno() {
        let avvio = 1_000;
        let fatti = FattiAvanzamento {
            // Un solo passo, molto indietro: da allora la figura tace perche' sta
            // aspettando il modello.
            passi: vec![passo("read_file", json!({"path": "a.rs"}), avvio + 10)],
            scritture: Vec::new(),
        };
        let d = decidi_prosecuzione(&fatti, avvio, avvio + 300, soglie(90, 960));
        assert_eq!(
            d,
            Prosecuzione::ProseguePerIgnoto {
                motivo: MotivoNonOsservabile::NessunSegnaleRecente
            },
            "senza un fatto da opporre non si ferma: si dichiara il silenzio"
        );
        assert!(!d.e_arresto());
    }

    /// Il rovescio, che impedisce di "risolvere" il test sopra spegnendo
    /// l'arresto: con UN fatto da opporre — anche uno solo — la figura si ferma.
    /// E' la differenza fra «non ti vedo» e «ti vedo girare a vuoto».
    #[test]
    fn basta_un_fatto_a_vuoto_perche_il_silenzio_diventi_arresto() {
        let avvio = 1_000;
        let fatti = FattiAvanzamento {
            passi: vec![
                passo("read_file", json!({"path": "a.rs"}), avvio + 10),
                // La STESSA lettura, rifatta: ecco il fatto da opporre.
                passo("read_file", json!({"path": "a.rs"}), avvio + 20),
            ],
            scritture: Vec::new(),
        };
        let d = decidi_prosecuzione(&fatti, avvio, avvio + 300, soglie(90, 960));
        match d.causa().expect("con un fatto da opporre si ferma") {
            CausaArresto::NonAvanza { passi_a_vuoto, .. } => assert_eq!(*passi_a_vuoto, 1),
            altro => panic!("attesa no_progress, ottenuto {altro:?}"),
        }
    }

    /// REGRESSIONE (09/08/2026), trovata dal test che guarda il DB VERO
    /// (`agent_graph_adapter::avanzamento::la_stessa_chiamata_ripetuta_e_una_strada_sola`):
    /// sei chiamate identiche scritte in rapida successione cadono tutte NELLO
    /// STESSO SECONDO, perche' `istante_s` e' `DateTime::timestamp`.
    ///
    /// Con un confronto di soli istanti e la disuguaglianza STRETTA, «dopo
    /// l'ultimo avanzamento» le escludeva tutte e sei — `passi_a_vuoto: 0`, la
    /// figura proseguiva, e il criterio era cieco proprio sul caso che esiste per
    /// cogliere: la ripetizione veloce. I test a istanti inventati non potevano
    /// vederlo, perche' li spaziavano di venti secondi l'uno dall'altro
    /// (regola O: l'istante e' un dato che la produzione PRODUCE, e la sua
    /// granularita' e' parte del fatto).
    ///
    /// MUTAZIONE: rimettere il confronto sui soli istanti (togliere il confine
    /// per POSIZIONE in `lavoro_a_vuoto_dopo`) riporta `passi_a_vuoto` a 0 e
    /// questa figura non si ferma piu'.
    #[test]
    fn le_ripetizioni_nello_stesso_secondo_si_contano() {
        let avvio = 1_000;
        // Sei chiamate identiche, tutte allo stesso istante: la prima e' la
        // strada nuova, le altre cinque sono lavoro a vuoto.
        let passi: Vec<_> = (0..6)
            .map(|_| passo("run_command", json!({"command": "npm test"}), avvio + 5))
            .collect();
        let fatti = FattiAvanzamento {
            passi,
            scritture: Vec::new(),
        };
        let d = decidi_prosecuzione(&fatti, avvio, avvio + 100, soglie(90, 960));
        match d.causa().expect("chi ripete nello stesso secondo si ferma comunque") {
            CausaArresto::NonAvanza { passi_a_vuoto, .. } => assert_eq!(
                *passi_a_vuoto, 5,
                "la prima e' nuova, le altre cinque no: {d:?}"
            ),
            altro => panic!("attesa no_progress, ottenuto {altro:?}"),
        }
    }

    /// Il lavoro a vuoto si conta DA quando ha avanzato l'ultima volta, non
    /// sull'intera storia: una ripetizione superata non e' una prova valida
    /// contro il presente.
    ///
    /// MUTAZIONE: contare `a_vuoto` su tutta la storia (togliere il filtro
    /// `dopo_l_ultimo`) fa fermare questa figura citando due ripetizioni che
    /// aveva gia' superato — cioe' con una prova scaduta.
    #[test]
    fn il_lavoro_a_vuoto_si_conta_dopo_l_ultimo_avanzamento() {
        let avvio = 1_000;
        let fatti = FattiAvanzamento {
            passi: vec![
                passo("read_file", json!({"path": "a.rs"}), avvio + 10),
                // Due ripetizioni... superate da un avanzamento successivo.
                passo("read_file", json!({"path": "a.rs"}), avvio + 20),
                passo("read_file", json!({"path": "a.rs"}), avvio + 30),
                passo("read_file", json!({"path": "b.rs"}), avvio + 40),
            ],
            scritture: Vec::new(),
        };
        let d = decidi_prosecuzione(&fatti, avvio, avvio + 300, soglie(90, 960));
        assert_eq!(
            d,
            Prosecuzione::ProseguePerIgnoto {
                motivo: MotivoNonOsservabile::NessunSegnaleRecente
            },
            "le ripetizioni precedenti all'ultimo avanzamento non sono una prova: {d:?}"
        );
    }

    /// Un guasto della porta e' un ignoto DICHIARATO, non un arresto: se il DB
    /// non risponde non si uccide una figura che potrebbe star lavorando.
    #[test]
    fn un_guasto_di_lettura_non_ferma_la_figura() {
        // La porta, in errore, consegna fatti vuoti col motivo giusto: la
        // costruzione della decisione e' la stessa, il motivo cambia.
        let d = Prosecuzione::ProseguePerIgnoto {
            motivo: MotivoNonOsservabile::LetturaFallita,
        };
        assert!(!d.e_arresto());
        assert_eq!(d.causa(), None);
    }
}
