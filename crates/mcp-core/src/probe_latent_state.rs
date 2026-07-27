//! Il profilo `latent_state`, che certifica `frontier`: il modello deve SEGUIRE gli
//! aggiornamenti di uno stato lungo un contesto lungo, non trovare una riga da
//! copiare.
//!
//! # Perche' il needle e' morto, e cosa lo sostituisce
//!
//! `agentic_longctx` nascondeva `CODICE-PRATICA: NX7K2P9QW4` in 100k caratteri e
//! chiedeva di ritrovarlo. Due difetti, entrambi fatali:
//!
//! 1. MISURATO: 40 evidenze su 40 inconclusive. Non ha mai dato un verdetto.
//! 2. DALLA LETTERATURA: cinque gruppi (NoLiMa, RULER, Michelangelo, BABILong,
//!    FLenQA) hanno mostrato che il needle e' una lookup su dizionario. Il nostro
//!    caso era il peggiore: la domanda ("una riga che inizia con CODICE-PRATICA:") e
//!    la riga da trovare avevano sovrapposizione lessicale MASSIMA — la scorciatoia
//!    che NoLiMa quantifica (GPT-4o dal 99,3% al 69,7% appena la togli).
//!
//! Qui non c'e' niente da copiare. Il registro racconta un'entita' che cambia stato
//! (fascicoli assegnati e poi evasi) e la domanda chiede lo stato FINALE: quali sono
//! ancora in carico. Quella risposta — l'INSIEME dei superstiti — non e' scritta in
//! nessuna riga; va COSTRUITA applicando gli aggiornamenti in ordine. E' la forma
//! "Latent List"/"MRCR" di Michelangelo, e nega per costruzione la lookup lessicale.
//!
//! # I distrattori, e perche' il fallimento e' diagnostico
//!
//! Cinque degli otto codici sono stati in carico e non lo sono piu'. Vivono nel testo
//! nella stessa forma dei superstiti, e la riga che li apre e' identica a quella dei
//! tre che restano. Un modello che cerca per somiglianza li trova tutti e ne riporta
//! di STALE; solo chi ha seguito le chiusure arriva ai tre giusti.
//!
//! E la sovrapposizione lessicale, qui, e' una TRAPPOLA e non una scorciatoia: la
//! domanda dice "in carico", e "in carico" compare sia in "risulta in carico" sia in
//! "non risulta piu' in carico". Chi abbina la stringa raccoglie tutti e otto e
//! sbaglia; chi legge distingue. E' l'inverso esatto del needle.
//!
//! Il valore stale RIPORTATO dice a che punto della catena il modello si e' perso:
//! `stale_closure:2/5` e' "ha mancato la seconda delle cinque chiusure", che e' una
//! diagnosi, non un voto.
//!
//! # Ground truth per costruzione, nessun giudice
//!
//! L'harness genera gli aggiornamenti, quindi CONOSCE lo stato finale (back-instruct,
//! TaskBench). Il confronto e' un'uguaglianza di insiemi su token da ~47 bit, non un
//! parere: BFCL, tau-bench e LiveBench escludono i giudici LLM dallo SCORING, e
//! l'umano (e l'LLM) stanno nell'authoring, mai nella valutazione.

use serde_json::{json, Value};

use crate::probe_agentic_loop::TurnSource;
use crate::probe_world::TokenSeed;

/// Il kind del profilo. Nominato dal dispatch, dal guard del predicato e da qui: se
/// una delle copie divergesse, il profilo girerebbe col predicato muto (regola N).
pub(crate) const KIND_LATENT_STATE: &str = "latent_state";

/// La chiave del predicato che chiede la verifica dello stato finale. Sta nel
/// vocabolario CHIUSO `CHIAVI_PREDICATO` e vale SOLO su questo kind: altrove non
/// avrebbe misure da leggere e sarebbe muta, cioe' un pass regalato.
pub(crate) const K_REQUIRES_FINAL_STATE: &str = "requires_final_state";

/// La riga finale che la domanda impone. Serve a separare il RAGIONAMENTO dalla
/// RISPOSTA: senza, un modello che ragiona ad alta voce ("F-Q7W2K9BCDF e' stato evaso,
/// quindi lo escludo") nominerebbe un codice chiuso e verrebbe bocciato per una
/// deduzione GIUSTA. Il marcatore non e' una prova di obbedienza, e' cio' che rende
/// la risposta una regione delimitata invece che prosa da interpretare (regola M).
const MARCATORE: &str = "RISPOSTA:";

const CONTEXT_CHARS_DEFAULT: i64 = 40_000;
/// Il PAVIMENTO del contesto, e non e' un numero tondo a caso: le 13 righe di
/// aggiornamento pesano ~1200 byte, e sotto questa misura non stanno nei loro quarti.
///
/// MISURATO: a 4000 byte le righe sono il 29% del testo, i quarti ne valgono 1000
/// l'uno, e il superstite del primo quarto veniva spinto a 1050 — oltre il confine di
/// 1044, per 6 byte. `verifica_disposizione` lo rifiutava e il giro chiudeva
/// INCONCLUSIVO: la malattia di `agentic_longctx`, che in 40 giri non ha mai deciso.
/// Il clamp e' il posto giusto per dirlo — un profilo di contesto lungo a 4k non e'
/// un profilo di contesto lungo, e non deve poter essere configurato.
const CONTEXT_CHARS_MIN: i64 = 12_000;
const CONTEXT_CHARS_MAX: i64 = 200_000;

/// I quarti di contesto. La posizione conta (RULER e BABILong misurano il degrado
/// per posizione): il piano assegna a ogni aggiornamento il suo quarto, il seme
/// decide dove cade dentro il quarto.
const ZONE: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// Il fascicolo entra in carico.
    Apre(usize),
    /// Il fascicolo esce dallo stato: da qui in poi e' un DISTRATTORE.
    Chiude(usize),
}

impl Op {
    fn codice(self) -> usize {
        match self {
            Op::Apre(k) | Op::Chiude(k) => k,
        }
    }
}

/// Quanti codici vivono nel registro.
const N_CODICI: usize = 8;

/// IL PIANO degli aggiornamenti, in ordine narrativo, con il quarto in cui cade
/// ognuno. 8 aperture, 5 chiusure, 3 superstiti.
///
/// I PARAMETRI sono fissi e il seme non li tocca: stessa difficolta' per tutti, o due
/// modelli verrebbero misurati con metri diversi. Il seme varia l'ISTANZA (i codici e
/// la posizione dentro il quarto), mai la banda.
///
/// Perche' 8 e 5: chi fa lookup raccoglie tutti gli otto codici, e cinque su otto
/// (62%) sono stale — l'errore e' DECISIVO, non marginale. Perche' 3 superstiti:
/// scegliere a caso 3 fra 8 azzecca l'insieme 1 volta su 56 (1,8%), e i codici non
/// sono inventabili (~47 bit); con 4 tentativi e 3 pass richiesti, la fortuna e'
/// esclusa per misura, non per convinzione.
///
/// La DISPOSIZIONE e' il cuore del test, e ogni riga ha un perche':
/// - il codice 0 e' un superstite aperto nel PRIMO quarto e mai piu' nominato: chi ha
///   un bias di recency e legge la coda lo perde;
/// - il codice 1 e' aperto nel primo quarto e chiuso nell'ULTIMO: chi ha un bias di
///   primacy e legge la testa lo crede ancora in carico;
/// - il codice 4 e' un superstite aperto in mezzo: e' la zona del "lost in the
///   middle", quella che si perde per prima;
/// - il codice 7 e' un superstite aperto nell'ultimo quarto: chi legge solo la testa
///   lo manca.
///
/// I tre superstiti stanno quindi in tre quarti diversi: per averli tutti bisogna
/// aver letto tutto. E ogni lettura parziale consegna almeno un codice stale.
///
/// Causalita': ogni chiusura sta in un quarto STRETTAMENTE successivo alla sua
/// apertura, quindi il sorteggio della posizione dentro il quarto non puo' mai
/// scambiarle. `verifica_invarianti` lo ricontrolla sul testo prodotto.
const PIANO: [(Op, usize); 13] = [
    (Op::Apre(0), 0),
    (Op::Apre(1), 0),
    (Op::Apre(2), 0),
    (Op::Apre(3), 1),
    (Op::Chiude(2), 1),
    (Op::Apre(4), 1),
    (Op::Apre(5), 2),
    (Op::Chiude(3), 2),
    (Op::Apre(6), 2),
    (Op::Apre(7), 3),
    (Op::Chiude(1), 3),
    (Op::Chiude(5), 3),
    (Op::Chiude(6), 3),
];

/// Prosa d'archivio neutra: dimensiona il CARICO e non porta codici, cosi' l'unica
/// cosa che conta nel registro sono gli aggiornamenti. Non e' una riga sola ripetuta
/// perche' un testo degenere si comprime a vista e il contesto lungo diventa finto.
const FILLER: [&str; 8] = [
    "Nota di servizio: la corrispondenza in entrata viene protocollata entro il giorno lavorativo successivo.\n",
    "Promemoria: le richieste di accesso agli atti seguono la procedura ordinaria \
     e non modificano lo stato dei fascicoli.\n",
    "Comunicazione interna: l'orario di apertura al pubblico dello sportello resta invariato per tutto il periodo.\n",
    "Avviso: la numerazione di protocollo prosegue senza interruzioni anche durante le giornate di chiusura.\n",
    "Circolare: la trasmissione dei documenti fra uffici avviene tramite il sistema di gestione documentale.\n",
    "Nota: le comunicazioni di cortesia non producono effetti sul carico di lavoro assegnato agli operatori.\n",
    "Memorandum: l'archiviazione fisica dei documenti cartacei segue un calendario separato da quello digitale.\n",
    "Informativa: il registro riporta gli eventi in ordine cronologico, senza raggruppamenti per ufficio.\n",
];

/// La riga che mette un fascicolo IN CARICO.
fn riga_apertura(codice: &str) -> String {
    format!("Il fascicolo {codice} viene assegnato all'ufficio e da questo momento risulta in carico.\n")
}

/// La riga che lo TOGLIE dal carico. Da qui in poi quel codice e' un distrattore:
/// c'e', si trova, e non e' piu' la risposta.
fn riga_chiusura(codice: &str) -> String {
    format!("Il fascicolo {codice} e' stato evaso e da questo momento non risulta piu' in carico.\n")
}

/// La domanda. Sta DOPO il registro, e non prima: cosi' il modello non puo' filtrare
/// il testo in una passata sapendo gia' cosa cercare — deve averlo letto. E' anche
/// la disposizione che la guida di Anthropic raccomanda per i documenti lunghi.
///
/// Non dice "traccia lo stato" ne' "attenzione ai fascicoli evasi": spiegare la
/// strategia misurerebbe l'obbedienza al nostro prompt invece della capacita' di
/// capire che, per rispondere, gli aggiornamenti vanno seguiti.
fn domanda() -> String {
    format!(
        "Il registro sopra riporta, in ordine cronologico, gli eventi di un ufficio.\n\
         Domanda: alla fine del registro, quali fascicoli risultano ancora in carico \
         all'ufficio?\n\
         Termina la risposta con una riga nella forma esatta:\n\
         {MARCATORE} <codici separati da virgola>"
    )
}

/// Il system prompt del giro. Neutro: dire qui che si tratta di seguire uno stato
/// anticiperebbe la domanda, che per progetto arriva dopo il registro.
pub(crate) fn system_text() -> String {
    "Sei in una verifica di lettura di un registro cronologico. Rispondi in modo \
     conciso, senza commenti superflui."
        .to_string()
}

/// UN'ISTANZA del compito: i codici, chi sopravvive, chi e' stato chiuso e in che
/// ordine, e il registro che li racconta.
#[derive(Debug, Clone)]
pub(crate) struct IstanzaStato {
    codici: Vec<String>,
    /// Gli indici dei codici ancora in carico alla fine: LA RISPOSTA. Non e' scritta
    /// in nessuna riga del registro.
    superstiti: Vec<usize>,
    /// Gli indici dei codici chiusi, in ordine di chiusura nel testo. Sono i
    /// DISTRATTORI, e l'ordine e' cio' che rende diagnostico il fallimento.
    chiusure: Vec<usize>,
    testo: String,
}

impl IstanzaStato {
    /// I codici ancora in carico alla fine (la risposta attesa).
    pub(crate) fn superstiti(&self) -> Vec<&str> {
        self.superstiti.iter().map(|k| self.codici[*k].as_str()).collect()
    }

    /// I codici che ERANO in carico e non lo sono piu' (i distrattori), in ordine di
    /// chiusura.
    pub(crate) fn chiusi(&self) -> Vec<&str> {
        self.chiusure.iter().map(|k| self.codici[*k].as_str()).collect()
    }

    /// I messaggi del giro, nella forma che il gateway si aspetta.
    ///
    /// UN solo messaggio utente: registro e domanda insieme. Due messaggi `user`
    /// consecutivi sono una forma che alcuni provider rifiutano, e un profilo che
    /// certifica il vertice non puo' permettersi di essere inconclusivo per la forma
    /// della richiesta (`agentic_longctx`, che li usava, ha chiuso 40 giri su 40
    /// senza un verdetto).
    pub(crate) fn messaggi(&self) -> String {
        json!([{ "role": "user", "content": format!("{}\n{}", self.testo, domanda()) }]).to_string()
    }

    /// Quali dei NOSTRI codici compaiono in `regione`.
    ///
    /// Si cercano i codici emessi, non "codici di forma plausibile": un token da ~47
    /// bit il modello non puo' inventarlo, quindi la presenza e' un fatto e non serve
    /// estrarre nulla dalla prosa (regola M).
    fn codici_in(&self, regione: &str) -> Vec<usize> {
        (0..self.codici.len())
            .filter(|k| regione.contains(&self.codici[*k]))
            .collect()
    }
}

/// Il contesto in caratteri, dal payload (regola G) e clampato: e' un limite di
/// sicurezza del probe, non configurazione di routing.
fn context_chars(payload: &Value) -> usize {
    payload
        .get("context_chars")
        .and_then(Value::as_i64)
        .unwrap_or(CONTEXT_CHARS_DEFAULT)
        .clamp(CONTEXT_CHARS_MIN, CONTEXT_CHARS_MAX) as usize
}

/// La posizione [0,1) di ogni aggiornamento: il suo quarto piu' il sorteggio dentro
/// il quarto. I quarti sono disgiunti, quindi l'ordine del piano (e con esso la
/// causalita' apertura->chiusura) sopravvive all'ordinamento.
fn posizioni(seed: &TokenSeed) -> Vec<(f64, Op)> {
    let mut p: Vec<(f64, Op)> = PIANO
        .iter()
        .enumerate()
        .map(|(i, (op, zona))| {
            // Margine 0,10..0,80 dentro il quarto. Il tetto e' 0,80 e non 0,95 perche'
            // un aggiornamento non cade dove lo si punta: le righe che lo precedono
            // l'hanno gia' spostato a destra, e il riempimento va a blocchi interi.
            // Con 0,95 un'operazione puntata in cima al suo quarto scavalcava il
            // confine e finiva in quello dopo — e il guard, giustamente, rifiutava
            // l'istanza. Il margine e' cio' che rende il piano vero nel testo.
            let dentro = 0.10 + seed.frazione(&format!("pos:{i}")) * 0.70;
            ((*zona as f64 + dentro) / ZONE as f64, *op)
        })
        .collect();
    p.sort_by(|a, b| a.0.total_cmp(&b.0));
    p
}

/// Il registro: filler fino alla posizione dell'aggiornamento, poi la riga
/// dell'aggiornamento, e cosi' via.
fn tessi_registro(codici: &[String], ordine: &[(f64, Op)], chars: usize) -> String {
    let mut testo = String::with_capacity(chars + 1024);
    let riempi_fino = |t: &mut String, bersaglio: usize| {
        while t.len() < bersaglio {
            t.push_str(FILLER[(t.len() / 97) % FILLER.len()]);
        }
    };
    for (pos, op) in ordine {
        riempi_fino(&mut testo, (pos * chars as f64) as usize);
        let c = &codici[op.codice()];
        testo.push_str(&match op {
            Op::Apre(_) => riga_apertura(c),
            Op::Chiude(_) => riga_chiusura(c),
        });
    }
    riempi_fino(&mut testo, chars);
    testo
}

/// La riga di `testo` che contiene il byte `offset`.
fn riga_attorno(testo: &str, offset: usize) -> &str {
    let inizio = testo[..offset].rfind('\n').map_or(0, |i| i + 1);
    let fine = testo[offset..].find('\n').map_or(testo.len(), |i| offset + i);
    &testo[inizio..fine]
}

/// Costruisce l'istanza. `Err` col motivo se un invariante non regge: il chiamante
/// chiude INCONCLUSIVO, perche' un'istanza malfatta e' colpa nostra e addebitarla al
/// modello lo declasserebbe a torto.
pub(crate) fn costruisci(seed: &TokenSeed, chars: usize) -> Result<IstanzaStato, String> {
    let codici: Vec<String> = (0..N_CODICI).map(|k| seed.codice(k)).collect();
    let ordine = posizioni(seed);
    let testo = tessi_registro(&codici, &ordine, chars);
    let chiusure: Vec<usize> = ordine
        .iter()
        .filter_map(|(_, op)| match op {
            Op::Chiude(k) => Some(*k),
            Op::Apre(_) => None,
        })
        .collect();
    let superstiti: Vec<usize> = (0..N_CODICI).filter(|k| !chiusure.contains(k)).collect();
    let istanza = IstanzaStato { codici, superstiti, chiusure, testo };
    verifica_invarianti(&istanza)?;
    Ok(istanza)
}

/// GLI INVARIANTI, verificati sul TESTO PRODOTTO e non sul piano che credevamo di
/// aver seguito.
///
/// `agentic_longctx` affidava la stessa promessa a un commento ("il needle non
/// compare MAI nel system prompt"). Qui sono guard: costano qualche `find` e
/// trasformano una promessa in un fatto. Se uno cade, il giro non parte — meglio un
/// inconclusivo dichiarato che una misura che non misura.
fn verifica_invarianti(ist: &IstanzaStato) -> Result<(), String> {
    if ist.superstiti.len() < 2 || ist.chiusure.len() < 2 {
        return Err(format!(
            "piano degenere: {} superstiti, {} chiusure",
            ist.superstiti.len(),
            ist.chiusure.len()
        ));
    }
    for (k, c) in ist.codici.iter().enumerate() {
        if ist.codici.iter().filter(|x| *x == c).count() > 1 {
            return Err(format!("codice {k} duplicato: i distrattori sarebbero indistinguibili"));
        }
        let Some(primo) = ist.testo.find(c.as_str()) else {
            return Err(format!("codice {k} assente dal registro"));
        };
        // NESSUNA RIGA PORTA LA RISPOSTA: se una riga nominasse due codici, quella
        // riga sarebbe copiabile e il compito tornerebbe a essere una lookup.
        if ist.codici.iter().filter(|x| riga_attorno(&ist.testo, primo).contains(x.as_str())).count() > 1 {
            return Err(format!("la riga del codice {k} ne nomina un altro"));
        }
    }
    verifica_causalita(ist)?;
    verifica_disposizione(ist)
}

/// Ogni chiusura viene DOPO la sua apertura, e le due righe sono quelle giuste.
fn verifica_causalita(ist: &IstanzaStato) -> Result<(), String> {
    for k in &ist.chiusure {
        let c = &ist.codici[*k];
        let (Some(apre), Some(chiude)) = (ist.testo.find(c.as_str()), ist.testo.rfind(c.as_str()))
        else {
            return Err(format!("codice {k} assente"));
        };
        if apre >= chiude {
            return Err(format!("codice {k}: chiusura non successiva all'apertura"));
        }
        if !riga_attorno(&ist.testo, apre).contains("viene assegnato") {
            return Err(format!("codice {k}: la prima riga non e' un'apertura"));
        }
        if !riga_attorno(&ist.testo, chiude).contains("stato evaso") {
            return Err(format!("codice {k}: l'ultima riga non e' una chiusura"));
        }
    }
    // Un superstite compare UNA volta sola: se comparisse due volte, la seconda
    // sarebbe una chiusura e non sarebbe un superstite.
    for k in &ist.superstiti {
        let c = &ist.codici[*k];
        if ist.testo.find(c.as_str()) != ist.testo.rfind(c.as_str()) {
            return Err(format!("superstite {k} nominato due volte"));
        }
    }
    Ok(())
}

/// LA DISPOSIZIONE, che e' cio' che rende il compito non scorciatoiabile da un
/// estremo: serve un superstite nel primo quarto (chi legge la coda lo perde) e una
/// chiusura nell'ultimo quarto di un codice aperto nel primo (chi legge la testa lo
/// crede ancora in carico). Senza entrambi, mezzo contesto basterebbe.
fn verifica_disposizione(ist: &IstanzaStato) -> Result<(), String> {
    let n = ist.testo.len() as f64;
    let quarto = |off: usize| (off as f64 / n * ZONE as f64) as usize;
    let superstite_antico = ist
        .superstiti
        .iter()
        .any(|k| ist.testo.find(ist.codici[*k].as_str()).map(&quarto) == Some(0));
    if !superstite_antico {
        return Err("nessun superstite nel primo quarto: la coda basterebbe".into());
    }
    let chiusura_tardiva = ist.chiusure.iter().any(|k| {
        let c = ist.codici[*k].as_str();
        ist.testo.find(c).map(&quarto) == Some(0)
            && ist.testo.rfind(c).map(&quarto) == Some(ZONE - 1)
    });
    if !chiusura_tardiva {
        return Err("nessun codice aperto nel primo quarto e chiuso nell'ultimo: la testa basterebbe".into());
    }
    Ok(())
}

/// Cosa il modello ha dimostrato. Sono fatti, non voti.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EsitoStato {
    /// Ha ricostruito lo stato finale: esattamente i superstiti.
    Corretto,
    /// Ha riportato un codice che ERA in carico e non lo e' piu': ha guardato invece
    /// di seguire. `mancata` = quale delle chiusure si e' perso (1-based), che e' la
    /// diagnosi.
    Stale { mancata: usize, totale: usize },
    /// Ha nominato codici nostri, nessuno stale, ma non l'insieme giusto: ha perso il
    /// filo senza abboccare.
    Incompleto { trovati: usize, attesi: usize },
    /// Nessun codice nostro nella regione di risposta.
    SenzaRisposta,
}

impl EsitoStato {
    fn motivo(&self) -> String {
        match self {
            EsitoStato::Corretto => "ok".into(),
            EsitoStato::Stale { mancata, totale } => format!("stale_closure:{mancata}/{totale}"),
            EsitoStato::Incompleto { trovati, attesi } => format!("incomplete:{trovati}/{attesi}"),
            EsitoStato::SenzaRisposta => "no_codes".into(),
        }
    }
}

/// La regione di risposta: la riga del marcatore (l'ULTIMA, se il modello lo ripete
/// nel ragionamento), altrimenti l'ultima riga non vuota.
///
/// Il ripiego non e' indulgenza: un modello che risponde "F-A, F-B, F-C" senza
/// marcatore ha risposto, e bocciarlo misurerebbe la nostra convenzione. Ma resta una
/// regione DELIMITATA: la prosa non si interpreta mai.
fn regione_risposta(content: &str) -> &str {
    match content.rfind(MARCATORE) {
        Some(i) => {
            let coda = &content[i + MARCATORE.len()..];
            coda.split('\n').next().unwrap_or(coda)
        }
        None => content.lines().rev().find(|r| !r.trim().is_empty()).unwrap_or(""),
    }
}

/// Il giudizio, STRUTTURALE: uguaglianza di insiemi su token che il modello non puo'
/// inventare. Nessun giudice, nessuna interpretazione della prosa.
pub(crate) fn giudica(content: &str, ist: &IstanzaStato) -> EsitoStato {
    let presenti = ist.codici_in(regione_risposta(content));
    if presenti.is_empty() {
        return EsitoStato::SenzaRisposta;
    }
    // PRIMA lo stale: e' il fatto diagnostico. Un insieme che contiene un codice
    // chiuso dice COSA e' andato storto, e va detto invece di finire in "incompleto".
    if let Some(pos) = presenti.iter().find_map(|k| ist.chiusure.iter().position(|c| c == k)) {
        return EsitoStato::Stale { mancata: pos + 1, totale: ist.chiusure.len() };
    }
    if presenti.len() != ist.superstiti.len() {
        return EsitoStato::Incompleto { trovati: presenti.len(), attesi: ist.superstiti.len() };
    }
    EsitoStato::Corretto
}

/// I fatti nella forma che il predicato sa leggere, appesi al turno REALE (che
/// conserva `content`/`stop_reason` per l'evidenza).
fn misure(esito: &EsitoStato) -> Value {
    json!({
        "final_state_ok": *esito == EsitoStato::Corretto,
        "state_verdict": esito.motivo(),
    })
}

/// Il predicato `requires_final_state`, confrontato coi FATTI misurati.
///
/// Misure ASSENTI = fallimento, mai un pass: un predicato che tace su cio' che non
/// trova regala la banda a chiunque, ed e' il difetto peggiore possibile qui perche'
/// un test che non misura e' indistinguibile da un test che passa.
pub(crate) fn motivo_fallimento(turn: &Value, predicate: &Value) -> Option<String> {
    if predicate.get(K_REQUIRES_FINAL_STATE).and_then(Value::as_bool) != Some(true) {
        return None;
    }
    if turn.pointer("/measures/final_state_ok").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    Some(
        turn.pointer("/measures/state_verdict")
            .and_then(Value::as_str)
            .unwrap_or("no_state_measure")
            .to_string(),
    )
}

/// I parametri di UN giro. Raggruppati per non spargere sette argomenti posizionali
/// nel dispatch.
pub(crate) struct ParametriGiro<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub profile_key: &'a str,
    pub attempt: i32,
    /// Fresco a ogni tentativo e REGISTRATO in `ai_model_probe_evidence.seed` (mig
    /// 0610): da quella riga il giro si rigioca bit a bit.
    pub seme: u64,
    pub payload: &'a Value,
}

/// UN tentativo: costruisce l'istanza, la manda, giudica la risposta.
///
/// `Err` = giro NON attribuibile al modello (il chiamante chiude inconclusivo).
/// `Ok(turno)` = il turno arricchito con le misure, che il predicato giudichera'.
pub(crate) async fn tentativo(
    fonte: &impl TurnSource,
    p: ParametriGiro<'_>,
) -> Result<Value, String> {
    let seed = TokenSeed {
        provider: p.provider.to_string(),
        model: p.model.to_string(),
        profile_key: p.profile_key.to_string(),
        attempt: p.attempt,
        seed: p.seme,
    };
    let istanza = costruisci(&seed, context_chars(p.payload))
        .map_err(|m| format!("istanza_non_costruibile:{m}"))?;
    gira(fonte, &istanza).await
}

/// Il giro vero: una domanda, una risposta, un giudizio.
pub(crate) async fn gira(fonte: &impl TurnSource, ist: &IstanzaStato) -> Result<Value, String> {
    let turn = fonte.turn(&ist.messaggi()).await;
    // Un errore del provider e' gia' classificato alla fonte (regola M): lo giudica
    // `evaluate_attempt`, che sa distinguere il transitorio dal model-specific. La
    // batteria non ri-classifica.
    let stop = turn.get("stop_reason").and_then(Value::as_str).unwrap_or("");
    if turn.get("error_class").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
        || stop == "error"
    {
        return Ok(turn);
    }
    let esito = giudica(turn.get("content").and_then(Value::as_str).unwrap_or(""), ist);
    // TRONCATO DAL NOSTRO CAP: la risposta e' stata tagliata a meta', e cio' che manca
    // e' colpa del nostro budget, non del modello — misurarlo qui misurerebbe noi.
    // Se invece la risposta e' GIUSTA nonostante il taglio, e' un pass: abbiamo cio'
    // che ci serve, e buttarlo sarebbe zelo.
    //
    // Il taglio si legge da `finish_reason`, non da `stop_reason`: sono due domande
    // diverse e il produttore risponde a entrambe. `stop_reason` dice la FORMA del
    // turno (ha chiamato tool o no) ed e' derivato dai blocchi, quindi non puo'
    // valere "max_tokens" per costruzione; `finish_reason` dice PERCHE' si e'
    // fermato e viene dal wire, tradotto dal punto unico ("length" -> "max_tokens").
    // Leggere il taglio da `stop_reason` renderebbe questo controllo inerte: e'
    // esattamente il difetto che il controllo esiste per riparare.
    let finish = turn.get("finish_reason").and_then(Value::as_str).unwrap_or("");
    if esito != EsitoStato::Corretto && finish == "max_tokens" {
        return Err("truncated_max_tokens".to_string());
    }
    let mut turno = turn;
    turno["measures"] = misure(&esito);
    Ok(turno)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn seme(attempt: i32) -> TokenSeed {
        TokenSeed {
            provider: "p".into(),
            model: "m".into(),
            profile_key: KIND_LATENT_STATE.into(),
            attempt,
            seed: 99,
        }
    }

    /// L'istanza come la costruisce la PRODUZIONE. Nessun test di questo modulo
    /// fabbrica un registro a mano: il compito su cui si misura il predicato e' lo
    /// stesso che vede il modello (regola O).
    fn istanza() -> IstanzaStato {
        costruisci(&seme(1), 12_000).expect("gli invarianti devono reggere")
    }

    // ── I MODELLI FINTI ────────────────────────────────────────────────────────
    //
    // Non recitano un copione: LEGGONO il registro che hanno ricevuto e ne ricavano
    // una risposta con la loro strategia. E' l'unico modo perche' il test provi che
    // la strategia sbagliata FALLISCE — un copione proverebbe che sappiamo scriverlo.

    /// Estrae i codici nominati in una riga. E' cio' che farebbe un modello: il
    /// registro e' testo, e i codici hanno una forma riconoscibile.
    fn codici_di(riga: &str) -> Vec<String> {
        riga.match_indices("F-")
            .map(|(i, _)| riga[i..].chars().take(12).collect())
            .collect()
    }

    /// IL MODELLO CHE FA LOOKUP: la domanda dice "in carico", lui raccoglie i codici
    /// di ogni riga che dice "in carico" e li riporta. E' esattamente la scorciatoia
    /// che NoLiMa quantifica, applicata al nostro testo.
    struct Lookup;
    /// IL MODELLO CHE SEGUE GLI AGGIORNAMENTI: applica apertura e chiusura in ordine
    /// e riporta cio' che resta.
    struct Tracker;
    /// Un modello che risponde cio' che gli si dice, col `finish_reason` che gli si
    /// dice: serve ai casi limite (troncamento).
    struct Fisso(String, &'static str);
    /// Registra i messaggi che ha ricevuto.
    struct Spia(RefCell<String>);

    /// Un turno come lo consegna la PRODUZIONE: da una `GwResponse` del gateway
    /// attraverso `agent_turn_value_from_gw`, che ne e' l'UNICO produttore.
    ///
    /// `finish` e' il vocabolario WIRE del gateway (`stop`/`length`/`tool_calls`),
    /// non quello della porta: chi scrive il test non decide lo `stop_reason` — lo
    /// decide il produttore, come in produzione. Fabbricare il turno a mano fisserebbe
    /// l'assunto da verificare, ed e' esattamente cosi' che il controllo del
    /// troncamento e' rimasto codice morto con un test verde (regola O).
    fn turno(content: &str, finish: &str) -> Value {
        use crate::nexus_gateway::{GwResponse, GwUsage};
        let resp = GwResponse {
            content: content.to_string(),
            tool_calls: None,
            usage: GwUsage {
                input_tokens: 10_000,
                output_tokens: 40,
                cache_read_tokens: None,
                cache_creation_tokens: None,
            },
            model_used: "m".to_string(),
            provider_used: "p".to_string(),
            latency_ms: 900,
            finish_reason: finish.to_string(),
            privacy_rerouted: None,
            reasoning: None,
            thinking_signature: None,
            citations: None,
            ledger_entry: None,
        };
        crate::orchestrator::neural_client::agent_turn_value_from_gw("p", "m", &resp)
    }

    /// Il registro come arriva al modello: dai messaggi, non dall'istanza.
    fn registro_da(messages_json: &str) -> String {
        let v: Value = serde_json::from_str(messages_json).unwrap();
        v[0]["content"].as_str().unwrap().to_string()
    }

    impl TurnSource for Lookup {
        async fn turn(&self, m: &str) -> Value {
            let trovati: Vec<String> = registro_da(m)
                .lines()
                .filter(|r| r.contains("in carico"))
                .flat_map(codici_di)
                .collect();
            turno(&format!("{MARCATORE} {}", trovati.join(", ")), "stop")
        }
    }

    impl TurnSource for Tracker {
        async fn turn(&self, m: &str) -> Value {
            let mut aperti: Vec<String> = Vec::new();
            for riga in registro_da(m).lines() {
                for c in codici_di(riga) {
                    if riga.contains("viene assegnato") {
                        aperti.push(c);
                    } else if riga.contains("stato evaso") {
                        aperti.retain(|x| *x != c);
                    }
                }
            }
            turno(&format!("{MARCATORE} {}", aperti.join(", ")), "stop")
        }
    }

    impl TurnSource for Fisso {
        async fn turn(&self, _m: &str) -> Value {
            turno(&self.0, self.1)
        }
    }

    impl TurnSource for Spia {
        async fn turn(&self, m: &str) -> Value {
            *self.0.borrow_mut() = m.to_string();
            turno("", "stop")
        }
    }

    /// Il generatore e' deterministico (l'evidenza si rigioca dalla colonna `seed`) e
    /// insieme fresco (l'istanza non e' memorizzabile).
    #[test]
    fn l_istanza_e_deterministica_ma_fresca() {
        assert_eq!(istanza().superstiti(), istanza().superstiti(), "stesso seme -> stessa istanza");
        let altro = costruisci(&seme(2), 12_000).unwrap();
        assert_ne!(
            istanza().superstiti(),
            altro.superstiti(),
            "tentativo diverso -> codici diversi: il compito non e' memorizzabile"
        );
    }

    /// LA PROPRIETA' CHE UCCIDE IL NEEDLE: la risposta non e' scritta da nessuna
    /// parte. Nessuna riga del registro contiene l'insieme dei superstiti — non c'e'
    /// niente da copiare, va costruito.
    #[test]
    fn nessuna_riga_contiene_la_risposta() {
        let ist = istanza();
        let superstiti = ist.superstiti();
        for riga in ist.testo.lines() {
            let quanti = superstiti.iter().filter(|c| riga.contains(**c)).count();
            assert!(quanti <= 1, "una riga sola porta piu' superstiti: sarebbe copiabile: {riga}");
        }
    }

    /// I distrattori sono la MAGGIORANZA dei codici: chi guarda invece di seguire non
    /// sbaglia di poco, sbaglia in modo decisivo.
    #[test]
    fn i_distrattori_sono_la_maggioranza() {
        let ist = istanza();
        assert_eq!(ist.chiusi().len(), 5);
        assert_eq!(ist.superstiti().len(), 3);
        assert!(ist.chiusi().len() > ist.superstiti().len());
    }

    /// IL TEST SIMMETRICO, il primo che serve: un modello che fa LOOKUP — la
    /// strategia del needle, applicata alla lettera — FALLISCE. E fallisce da stale:
    /// ha riportato un valore che era lo stato e non lo e' piu'.
    ///
    /// Non e' un copione: `Lookup` legge il registro vero e applica la scorciatoia
    /// vera. Senza questo test il predicato potrebbe promuovere chiunque e non lo
    /// sapremmo.
    #[tokio::test]
    async fn il_modello_che_fa_lookup_fallisce_da_stale() {
        let ist = istanza();
        let turno = gira(&Lookup, &ist).await.expect("ha risposto: e' un verdetto, non un inconclusivo");
        assert_eq!(turno["measures"]["final_state_ok"], false);
        // La CONSEGUENZA, non la stringa: il predicato reale del profilo boccia.
        let motivo = motivo_fallimento(&turno, &json!({ K_REQUIRES_FINAL_STATE: true }));
        assert!(
            motivo.as_deref().is_some_and(|m| m.starts_with("stale_closure:")),
            "la lookup deve fallire riportando un codice STALE, invece: {motivo:?}"
        );
    }

    /// L'ALTRA META': un modello che SEGUE gli aggiornamenti passa. Senza, il
    /// predicato potrebbe essere impossibile e non lo sapremmo.
    #[tokio::test]
    async fn il_modello_che_segue_gli_aggiornamenti_passa() {
        let turno = gira(&Tracker, &istanza()).await.unwrap();
        assert_eq!(turno["measures"]["final_state_ok"], true);
        assert_eq!(motivo_fallimento(&turno, &json!({ K_REQUIRES_FINAL_STATE: true })), None);
    }

    /// Chi nomina UN SOLO codice giusto non ha ricostruito lo stato: e' incompleto,
    /// ed e' un fatto diverso dallo stale (che invece ha abboccato).
    #[tokio::test]
    async fn chi_ne_azzecca_uno_solo_e_incompleto_non_corretto() {
        let ist = istanza();
        let esito = giudica(&format!("{MARCATORE} {}", ist.superstiti()[0]), &ist);
        assert_eq!(esito, EsitoStato::Incompleto { trovati: 1, attesi: 3 });
    }

    /// IL RAGIONAMENTO AD ALTA VOCE NON E' UNA RISPOSTA SBAGLIATA: chi cita un codice
    /// chiuso per ESCLUDERLO e poi risponde bene, passa. Senza il marcatore lo
    /// boccheremmo per una deduzione giusta.
    #[tokio::test]
    async fn citare_un_chiuso_nel_ragionamento_non_e_stale() {
        let ist = istanza();
        let content = format!(
            "Vedo che {} e' stato evaso, quindi lo escludo.\n{MARCATORE} {}",
            ist.chiusi()[0],
            ist.superstiti().join(", ")
        );
        assert_eq!(giudica(&content, &ist), EsitoStato::Corretto);
    }

    /// Senza marcatore si guarda l'ultima riga non vuota: chi risponde bene senza
    /// seguire la forma non viene bocciato per la forma.
    #[tokio::test]
    async fn la_risposta_senza_marcatore_vale_lo_stesso() {
        let ist = istanza();
        assert_eq!(giudica(&ist.superstiti().join(", "), &ist), EsitoStato::Corretto);
    }

    /// IL TRONCAMENTO DAL NOSTRO CAP E' INCONCLUSIVO, non una bocciatura: se no la
    /// banda misura il nostro budget invece del modello.
    ///
    /// La catena e' INTERA: il gateway dice `length` (vocabolario wire), il produttore
    /// lo traduce in `max_tokens`, e qui diventa un inconclusivo. Il test non scrive
    /// `max_tokens` da nessuna parte — se il produttore smettesse di propagare il
    /// segnale, questo test lo direbbe. Prima non lo diceva: il controllo gemello nel
    /// loop multi-step si costruiva il turno a mano ed era verde su codice morto.
    #[tokio::test]
    async fn il_troncamento_e_inconclusivo_non_una_bocciatura() {
        let e = gira(&Fisso("sto ragion".into(), "length"), &istanza()).await;
        assert_eq!(e.unwrap_err(), "truncated_max_tokens");
    }

    /// Ma un troncamento su una risposta GIUSTA resta un pass: abbiamo cio' che ci
    /// serve, e scartarlo sarebbe zelo.
    #[tokio::test]
    async fn il_troncamento_su_una_risposta_giusta_resta_un_pass() {
        let ist = istanza();
        let content = format!("{MARCATORE} {}", ist.superstiti().join(", "));
        let turno = gira(&Fisso(content, "length"), &ist).await.unwrap();
        assert_eq!(turno["measures"]["final_state_ok"], true);
    }

    /// Un errore del provider non e' una bocciatura: il turno passa intatto a
    /// `evaluate_attempt`, che lo classifica col punto unico. Anche il turno d'errore
    /// viene dal suo produttore vero (`error_agent_turn_value`): la classe la decide
    /// il classificatore, non il test.
    #[tokio::test]
    async fn un_errore_di_provider_non_diventa_una_misura() {
        struct Rotto;
        impl TurnSource for Rotto {
            async fn turn(&self, _m: &str) -> Value {
                crate::orchestrator::neural_client::error_agent_turn_value(
                    "google",
                    "gemini-x",
                    "429 rate limit exceeded",
                )
            }
        }
        let turno = gira(&Rotto, &istanza()).await.unwrap();
        assert_eq!(turno["error_class"], "rate_limit", "la classe viene dal punto unico");
        assert!(turno.get("measures").is_none(), "niente misure su un giro che non ha misurato");
    }

    /// MISURE ASSENTI = FALLIMENTO, mai un pass per assenza di dati. E' l'invariante
    /// che impedisce a `frontier` di essere gratis se il giro si rompe a monte.
    #[test]
    fn senza_misure_il_predicato_boccia() {
        let turno = json!({ "content": "qualcosa", "stop_reason": "end_turn" });
        assert_eq!(
            motivo_fallimento(&turno, &json!({ K_REQUIRES_FINAL_STATE: true })),
            Some("no_state_measure".to_string())
        );
    }

    /// Il predicato spento non giudica niente: un profilo che non chiede lo stato
    /// finale non deve essere toccato da questo verificatore.
    #[test]
    fn senza_la_chiave_il_verificatore_tace() {
        assert_eq!(motivo_fallimento(&json!({}), &json!({ "min_tool_calls": 1 })), None);
    }

    /// LA STRADA DELLA PRODUZIONE: il modello riceve UN messaggio utente con dentro il
    /// registro E la domanda. Il giro non parte da una richiesta vuota.
    #[tokio::test]
    async fn il_modello_riceve_il_registro_e_la_domanda() {
        let spia = Spia(RefCell::new(String::new()));
        let ist = istanza();
        gira(&spia, &ist).await.unwrap();
        let visti: Value = serde_json::from_str(&spia.0.borrow()).unwrap();
        let msgs = visti.as_array().unwrap();
        assert_eq!(msgs.len(), 1, "un solo messaggio utente: due 'user' di fila li rifiutano");
        let c = msgs[0]["content"].as_str().unwrap();
        assert!(c.contains(MARCATORE), "la domanda deve viaggiare col registro");
        assert!(c.contains(ist.superstiti()[0]), "il registro deve esserci davvero");
        // La domanda sta DOPO il registro: il modello non puo' filtrare in una passata.
        assert!(c.find(ist.superstiti()[0]) < c.find("Domanda:"));
    }

    /// GLI INVARIANTI REGGONO ALLA MISURA DELLA PRODUZIONE, non solo a quella comoda
    /// del test: `context_chars` viene dal payload della riga (mig 0611), e le altre
    /// prove qui girano a 12k per essere veloci.
    ///
    /// E' la lezione di `agentic_longctx`, che ha chiuso 40 giri su 40 senza mai dare
    /// un verdetto: un profilo che non si COSTRUISCE e' inconclusivo per sempre, e in
    /// silenzio. Un `Err` qui sarebbe indistinguibile da un modello che non ce la fa —
    /// tranne che non sarebbe colpa sua. Si prova su tutta la banda ammessa dal clamp
    /// e su piu' tentativi, perche' le posizioni cambiano col seme e un'istanza fuori
    /// zona nascerebbe solo per certi semi.
    #[test]
    fn l_istanza_si_costruisce_a_ogni_misura_ammessa_e_a_ogni_seme() {
        for chars in [
            CONTEXT_CHARS_MIN as usize,
            12_000,
            CONTEXT_CHARS_DEFAULT as usize,
            CONTEXT_CHARS_MAX as usize,
        ] {
            // Molti semi, non i quattro tentativi di un giro: le posizioni nascono dal
            // seme, quindi un'istanza fuori zona si affaccia solo per certi semi. Con
            // 4 tentativi in produzione, un difetto che colpisce 1 seme su 50 sarebbe
            // un inconclusivo ogni 12 giri — invisibile e inspiegabile.
            for attempt in 1..=60 {
                let ist = costruisci(&seme(attempt), chars).unwrap_or_else(|e| {
                    panic!("istanza non costruibile a {chars} char, tentativo {attempt}: {e}")
                });
                assert_eq!(ist.superstiti().len(), 3);
                assert_eq!(ist.chiusi().len(), 5);
            }
        }
    }

    /// Un contesto assurdo non passa dal payload: e' un limite di sicurezza del probe.
    #[test]
    fn il_contesto_e_clampato() {
        assert_eq!(context_chars(&json!({ "context_chars": 9_000_000 })), CONTEXT_CHARS_MAX as usize);
        assert_eq!(context_chars(&json!({})), CONTEXT_CHARS_DEFAULT as usize);
    }
}
