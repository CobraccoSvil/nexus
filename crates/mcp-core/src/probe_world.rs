//! Il MONDO FINTO della batteria di qualificazione: risponde alle tool-call dei
//! profili multi-step (`tool_chain`, `tool_recovery`) in modo deterministico, senza
//! toccare filesystem, processi o rete.
//!
//! # Perche' finto
//!
//! Non e' un ripiego, e' la conclusione a cui e' arrivata la letteratura dopo
//! essersi scottata: API-Bank hard-codifica le risposte "to maintain result
//! consistency", e StableToolBench ha misurato che solo il 44,4% delle chiamate API
//! reali di ToolBench riesce — un benchmark che dipende da servizi vivi misura la
//! salute dei servizi, non i modelli. Con un filesystem vero `min_chained_calls`
//! misurerebbe *cosa c'e' nella tempdir*, e il recupero dipenderebbe dai messaggi
//! d'errore del sistema operativo, che cambiano per OS.
//!
//! Sintetiche sono le RISPOSTE, non gli schemi: i tool dichiarati al modello restano
//! quelli veri del catalogo. Un tool finto misurerebbe il nostro mock.
//!
//! # Gli handle opachi, e perche' non si accettano i path
//!
//! I nomi dei tool (`read_file`, `list_files`) hanno un prior enorme nel
//! pre-training: il modello ha visto milioni di `read_file("src/main.rs")` e, se il
//! mondo accettasse un path plausibile, salterebbe il primo anello della catena
//! inventandoselo — e la dipendenza di dati non sarebbe provata. Qui un bersaglio e'
//! valido solo se e' un HANDLE che questo mondo ha emesso: un path letterale prende
//! `E_HANDLE_REQUIRED`. E' l'avvertimento di ToolFailBench (il ritorno del tool deve
//! contraddire il valore plausibile in memoria parametrica) e la forma del Gorilla
//! File System, dove "the errors are not exceptions but return values".
//!
//! # Perche' la catena e' fatta cosi' (suite 8)
//!
//! Un test che tutti passano non e' severo, e' rotto — ma vale anche il contrario:
//! un test che nessuno passa misura un pavimento. Questa catena e' stata satura DUE
//! volte (100% di pass, e il 79% dei tentativi esattamente al soffitto dei turni),
//! e le due volte la causa era la stessa: la voce buona portava un'etichetta
//! (`state: "current"`) e la strategia vincente era cercare quella parola. Alzare i
//! turni ha spostato il soffitto senza toccare la causa — anche i modelli piccoli
//! arrivavano in fondo, perche' seguire riferimenti concatenati non e' piu' una
//! capacita' rara.
//!
//! Due meccanismi la rendono di nuovo discriminante, e nessuno dei due la allunga:
//!
//! 1. IL CRITERIO NON E' NELLA RISPOSTA. Le voci di ogni elenco sono simmetriche e
//!    si distinguono solo per il custode (`owner`); il custode giusto e' nominato una
//!    volta sola, nel primo messaggio. Non c'e' parola da cercare: c'e' un vincolo da
//!    ricordare per otto turni.
//! 2. LA PISTA SI INTERROMPE. A un anello deciso dal seme la voce corretta porta a un
//!    ramo chiuso, e l'errore dice di tornare all'elenco precedente e prendere la voce
//!    scartata. Chi ha appena imparato il criterio deve sospenderlo perche' il mondo
//!    gliel'ha detto. E' adattamento, ed e' cio' che manca ai modelli deboli.
//!
//! Il rimedio dell'interruzione e' DICHIARATO nell'errore: e' il vincolo di
//! raggiungibilita' di BFCL V3, imparato pagando 0/30 conclusivi due volte sul
//! profilo di recupero. Il compito resta di capacita' e non di obbedienza: nulla
//! di tutto cio' e' annunciato nell'istruzione.
//!
//! # Determinismo e freschezza insieme
//!
//! Ogni token nasce da SHA-256 di (provider, model, profile_key, attempt, anello):
//! stabile — la stessa riga di evidence si riproduce bit a bit — ma diverso a ogni
//! tentativo, quindi non memorizzabile. Una costante nel repo proverebbe la memoria,
//! non la lettura: il needle fisso `NX7K2P9QW4` di `long_context` e' esattamente
//! l'errore da non ripetere (GPT-4-base recita il GUID di BIG-bench).

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Alfabeto base32 senza vocali e senza caratteri ambigui (0/O, 1/I/L): i token
/// finiscono nei log e negli argomenti JSON, e non devono poter essere confusi a
/// occhio ne' formare parole che il modello possa "riconoscere".
const ALFABETO: &[u8] = b"23456789BCDFGHJKMNPQRSTVWXZ";

/// Da cosa nasce ogni token di un tentativo. Sta tutto in `ai_model_probe_evidence`
/// tranne il `seed`, che la migrazione 0610 aggiunge: senza, un fallimento contestato
/// non e' riproducibile e la diagnosi e' cieca (la replayable fault injection di
/// ToolMisuseBench).
#[derive(Debug, Clone)]
pub(crate) struct TokenSeed {
    pub provider: String,
    pub model: String,
    pub profile_key: String,
    pub attempt: i32,
    /// Il seme della singola esecuzione: rende l'istanza fresca. Registrato
    /// nell'evidenza per poterla rigiocare identica.
    pub seed: u64,
}

impl TokenSeed {
    /// Un token opaco di `lunghezza` caratteri per l'etichetta `label`.
    ///
    /// 10 caratteri su questo alfabeto sono ~47 bit: indovinarlo e' dell'ordine di
    /// 1e-14. "Passato per fortuna" non e' escluso per convinzione, e' escluso per
    /// misura.
    fn token(&self, label: &str, lunghezza: usize) -> String {
        let mut h = Sha256::new();
        h.update(self.provider.as_bytes());
        h.update([0u8]);
        h.update(self.model.as_bytes());
        h.update([0u8]);
        h.update(self.profile_key.as_bytes());
        h.update([0u8]);
        h.update(self.attempt.to_le_bytes());
        h.update(self.seed.to_le_bytes());
        h.update(label.as_bytes());
        let d = h.finalize();
        d.iter()
            .take(lunghezza)
            .map(|b| ALFABETO[*b as usize % ALFABETO.len()] as char)
            .collect()
    }

    /// L'handle dell'anello `k` della catena. E' il bersaglio che il modello deve
    /// portare nella chiamata successiva.
    pub(crate) fn handle(&self, k: usize) -> String {
        format!("H-{}", self.token(&format!("chain:{k}"), 10))
    }

    /// Un handle DISTRATTORE per l'anello `k`: nasce dallo stesso seme ma con
    /// etichetta diversa, quindi e' della stessa forma e della stessa lunghezza del
    /// vero. Serve a distinguere chi discrimina da chi abbina per somiglianza.
    pub(crate) fn esca(&self, k: usize) -> String {
        format!("H-{}", self.token(&format!("esca:{k}"), 10))
    }

    /// Il CUSTODE della pista: il criterio di selezione della catena. Vive
    /// UNICAMENTE nell'istruzione iniziale, e ogni voce di ogni elenco dichiara il
    /// proprio `owner`: la voce da seguire e' quella affidata a questo custode.
    ///
    /// E' il perno del ridisegno (suite 8). Prima la voce buona era marcata
    /// `state: "current"`, e la strategia vincente era una sola riga — cerca la
    /// stringa "current", prendi quel `ref`. Nessuna lettura, nessuna memoria:
    /// misurato, il 79% dei tentativi arrivava al soffitto, inclusi i modelli
    /// piccoli. Qui il criterio non e' nella risposta, e' nel PRIMO messaggio: per
    /// applicarlo al settimo elenco bisogna ancora ricordarselo. Prefisso `C-`,
    /// distinto da `H-`/`E-`/`F-`, perche' un custode non e' un bersaglio da
    /// indirizzare: e' una proprieta' da confrontare.
    pub(crate) fn custode(&self) -> String {
        format!("C-{}", self.token("custode", 8))
    }

    /// Il custode ESTRANEO dell'anello `k`: chi tiene la voce che non va seguita.
    /// Cambia a ogni anello, cosi' che "evita quel valore li'" non diventi una
    /// scorciatoia dopo il primo errore: l'unico invariante e' il custode giusto.
    pub(crate) fn custode_estraneo(&self, k: usize) -> String {
        format!("C-{}", self.token(&format!("estraneo:{k}"), 8))
    }

    /// A quale anello la pista si interrompe. Fra il secondo e il terzo: abbastanza
    /// avanti perche' la catena sia avviata, abbastanza presto perche' restino turni
    /// per rientrare. Deriva dal seme come tutto il resto — fresco a ogni tentativo,
    /// rigiocabile dall'evidenza.
    ///
    /// Il PASS del profilo chiede piu' anelli di quanti la pista ne conceda prima
    /// dell'interruzione (`min_chained_calls: 4` > 3): passare IMPLICA essere
    /// rientrati, e l'unica strada per rientrare e' tornare all'elenco precedente.
    pub(crate) fn anello_cieco(&self) -> usize {
        if self.frazione("cieco") < 0.5 {
            2
        } else {
            3
        }
    }

    /// Il codice di un fascicolo del profilo `latent_state`. Prefisso diverso dagli
    /// handle perche' vive in un altro mondo: li' e' un bersaglio da indirizzare, qui
    /// e' un'entita' di cui seguire lo stato. Stessa fonte (SHA-256 del seme), quindi
    /// stesse garanzie: ~47 bit, non inventabile, fresco a ogni tentativo.
    pub(crate) fn codice(&self, k: usize) -> String {
        format!("F-{}", self.token(&format!("stato:{k}"), 10))
    }

    /// Una frazione deterministica in [0,1) per l'etichetta `label`.
    ///
    /// Stessa fonte dei token e nessun generatore nuovo (regola L): `latent_state` la
    /// usa per decidere DOVE, dentro la sua zona, cade un aggiornamento. La posizione
    /// e' parte dell'istanza — RULER e BABILong misurano il degrado per posizione, e
    /// una posizione fissa misurerebbe quella invece della capacita'.
    pub(crate) fn frazione(&self, label: &str) -> f64 {
        let n = self
            .token(label, 6)
            .bytes()
            .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
        f64::from(n % 10_000) / 10_000.0
    }

    /// Il token che vive UNICAMENTE dentro un messaggio d'errore. E' cio' che rende
    /// verificabile "il recupero e' informato dall'errore" senza chiedere il parere
    /// di un LLM: se la chiamata dopo lo contiene, il modello ha LETTO l'errore —
    /// non c'e' altro posto da cui possa averlo preso.
    pub(crate) fn token_errore(&self, label: &str) -> String {
        format!("E-{}", self.token(&format!("errore:{label}"), 10))
    }
}

/// La risposta del mondo a UNA tool-call.
#[derive(Debug, Clone)]
pub(crate) struct WorldReply {
    /// Cio' che vede il MODELLO. Solo testo: il messaggio di tool_result non ha
    /// campi `is_error`/`exit_code` sul filo, esattamente come in produzione.
    pub text: String,
    /// Cio' che sappiamo NOI, per costruzione: l'errore l'abbiamo piantato noi.
    /// Il predicato legge QUESTO, mai `text`. Non e' una violazione della regola M:
    /// la regola vieta di dedurre lo stato tecnico dalla prosa ALTRUI, e questa
    /// prosa e' nostra.
    pub is_error: bool,
    /// Il token che questa risposta ha piantato, se ne ha piantato uno. E' la sola
    /// fonte di verita' del taint tracking: contiamo un anello solo se la chiamata
    /// successiva porta un token che SAPPIAMO di aver emesso.
    pub planted: Option<String>,
}

impl WorldReply {
    fn ok(text: impl Into<String>, planted: Option<String>) -> Self {
        Self { text: text.into(), is_error: false, planted }
    }

    /// Un errore che DICHIARA il proprio rimedio: `message` dice cosa fare,
    /// `retryable` se un altro tentativo possa riuscire. E' la forma delle API vere
    /// (un 409 con "retry with ...") ed e' il contratto di risolvibilita' di questo
    /// mondo — un ostacolo il cui rimedio non e' derivabile da nessun canale
    /// osservabile misura un pavimento, non i modelli (0/30 conclusivi, due volte).
    ///
    /// Un solo posto sa come si scrive un ostacolo parlante, cosi' i tre che
    /// esistono non possono divergere (regola L). `retryable` e' `true` ogni volta
    /// che un'altra mossa PUO' riuscire: dichiarare `false` mentre si pretende una
    /// seconda mossa e' la contraddizione che azzero' il profilo di recupero.
    fn errore_parlante(codice: &str, rimedio: &str, ritentabile: bool, extra: Value) -> Self {
        let mut corpo = json!({ "message": rimedio, "retryable": ritentabile });
        if let Some(o) = extra.as_object() {
            for (k, v) in o {
                corpo[k] = v.clone();
            }
        }
        Self::errore(codice, corpo)
    }

    /// Errore-come-valore-di-ritorno (Gorilla File System): il modello lo riceve
    /// come un normale tool_result, non come un'eccezione di trasporto.
    fn errore(codice: &str, extra: Value) -> Self {
        let mut body = json!({ "error": { "code": codice } });
        if let Some(o) = extra.as_object() {
            for (k, v) in o {
                body["error"][k] = v.clone();
            }
        }
        Self { text: body.to_string(), is_error: true, planted: None }
    }
}

/// Cosa sta misurando questo mondo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorldKind {
    /// `tool_chain` (high): la profondita' di data-flow.
    Catena,
    /// `tool_recovery` (heavy): il recupero informato da un errore.
    Recupero,
}

/// Il mondo: una funzione (nome_tool, input) -> risposta, piu' lo stato minimo dei
/// token emessi. Puro: niente filesystem, niente processi, niente DB.
#[derive(Debug)]
pub(crate) struct ScriptedWorld {
    kind: WorldKind,
    seed: TokenSeed,
    /// Gli handle che il mondo ha gia' consegnato, in ordine di anello.
    emessi: Vec<String>,
    /// L'anello piu' avanti che il mondo abbia consegnato. Prima questo numero si
    /// leggeva da `emessi.len()` dopo un `resize` con stringhe vuote per tenere
    /// allineati gli indici: un contatore travestito da lista, che lasciava un ""
    /// in testa a `emessi`. Ora il contatore e' esplicito e `emessi` e' solo cio'
    /// che dice di essere.
    frontiera: usize,
    /// La pista si e' gia' interrotta? L'interruzione e' FIRST-TOUCH come il guasto
    /// del recupero: al primo contatto con l'anello cieco, sempre.
    ramo_cieco_annunciato: bool,
    /// Quante volte ogni tool e' stato chiamato: serve al trigger del guasto
    /// (`nth_call`), che e' dichiarativo e deterministico — mai probabilistico.
    chiamate: Vec<String>,
    /// Il guasto e' gia' scattato? L'attivazione e' FIRST-TOUCH: al primo contatto
    /// col tool bersaglio, sempre, non con una probabilita'.
    guasto_scattato: bool,
    /// Il token piantato nell'errore, quando il guasto e' scattato.
    token_errore: Option<String>,
}

/// Il rimedio che l'errore di recupero DICHIARA: nomina il campo esatto
/// (`current_epoch`) e l'azione (riprova includendolo negli argomenti), come
/// fanno le API vere (un 409 con "retry with ..."). E' il contratto di
/// risolvibilita' del test: senza, la mappatura token->argomenti non e'
/// derivabile da nessun canale osservabile e il test misura un pavimento
/// (0/30 conclusivi, misurato DUE volte). Il rimedio sta nell'ERRORE, che il
/// modello deve leggere: l'istruzione del task continua a non annunciare il
/// guasto, quindi resta un test di capacita', non di obbedienza.
const RIMEDIO_STALE: &str = "The handle epoch is stale. Retry the same call, \
     including the value of 'current_epoch' below in the arguments.";

/// Cosa DICHIARA l'errore del ramo cieco. E' la lezione del recupero applicata alla
/// catena, e non e' negoziabile: un ostacolo il cui rimedio non e' derivabile da un
/// canale osservabile non e' un test severo, e' un pavimento (0/30 conclusivi,
/// misurato due volte prima che l'errore del recupero dicesse cosa fare).
///
/// Qui tutto cio' che serve e' gia' nella conversazione: l'elenco che ha prodotto
/// questo handle e' un tool_result precedente, e conteneva DUE voci. L'errore nomina
/// l'azione ("torna a quell'elenco, chiama l'altra voce") senza nominare il valore:
/// il ref giusto va ripescato leggendo indietro, che e' esattamente la capacita' in
/// esame. Dirlo per esteso lo regalerebbe; tacerlo lo renderebbe indovinello.
const RIMEDIO_RAMO_CIECO: &str = "This branch is closed and will not reopen. Go back \
     to the listing that gave you this reference and call the OTHER entry of that \
     same listing - the one you did not choose. The trail continues from there.";

/// Cosa dichiara l'errore di chi segue la voce sbagliata: NOMINA il criterio
/// (confronta `owner` col custode del compito) e non il valore. Il custode e' nel
/// primo messaggio: il rimedio e' derivabile, ma va ricordato.
const RIMEDIO_CUSTODE: &str = "That entry is not held by the custodian named in your \
     task. Compare the 'owner' field of each entry with the custodian you were given.";

impl ScriptedWorld {
    /// Costruisce il mondo. `Err` se un token che va GUADAGNATO comparirebbe gia'
    /// nella richiesta iniziale: `long_context` affida la stessa regola a un COMMENTO
    /// ("il needle non compare MAI nel system prompt"), qui e' un invariante che costa
    /// due `contains` e trasforma una promessa in un guard. Un token a valle gia'
    /// visibile renderebbe la catena scorciatoiabile e il test misurerebbe la nostra
    /// ingenuita' invece del modello.
    ///
    /// `handle(0)` e' l'ECCEZIONE, e non e' un'indulgenza: e' l'anello di partenza, e
    /// la richiesta DEVE nominarlo o il modello non ha da dove cominciare (lo dice
    /// `handle_iniziale`). Vietarlo insieme agli altri e' costato 32 giri su 32
    /// inconclusive: il guard rifiutava esattamente cio' che il progetto impone.
    /// Il mondo sa da se' qual e' il suo ingresso e non chiede al chiamante di
    /// dichiararlo: nessun accordo da tenere allineato, quindi nessuno da rompere.
    pub(crate) fn new(
        kind: WorldKind,
        seed: TokenSeed,
        richiesta_iniziale: &[&str],
    ) -> Result<Self, String> {
        let mondo = Self {
            kind,
            seed,
            emessi: Vec::new(),
            frontiera: 0,
            ramo_cieco_annunciato: false,
            chiamate: Vec::new(),
            guasto_scattato: false,
            token_errore: None,
        };
        let nella_richiesta =
            |tok: &str| richiesta_iniziale.iter().any(|t| t.contains(tok));
        for k in 0..8 {
            // Gli handle a valle si guadagnano seguendo la catena; le esche viaggiano
            // solo nelle risposte, quindi nessuna e' ammessa nella richiesta.
            if k > 0 && nella_richiesta(&mondo.seed.handle(k)) {
                return Err(format!(
                    "token a valle {} gia' presente nella richiesta",
                    mondo.seed.handle(k)
                ));
            }
            if nella_richiesta(&mondo.seed.esca(k)) {
                return Err(format!("esca {} gia' presente nella richiesta", mondo.seed.esca(k)));
            }
        }
        Ok(mondo)
    }

    /// L'anello di partenza: e' l'UNICO bersaglio che la richiesta iniziale nomina.
    pub(crate) fn handle_iniziale(&self) -> String {
        self.seed.handle(0)
    }

    /// Gli handle emessi finora (il taint tracking confronta contro questi).
    pub(crate) fn emessi(&self) -> &[String] {
        &self.emessi
    }

    /// Il token piantato nell'errore, se il guasto e' scattato.
    pub(crate) fn token_errore_emesso(&self) -> Option<&str> {
        self.token_errore.as_deref()
    }

    /// Risponde a UNA tool-call. Il bersaglio si cerca in TUTTI i valori-foglia
    /// dell'input, mai in un campo con un nome preciso: quale campo usare lo decide
    /// il modello, e bocciarlo perche' ha scritto `handle` invece di `path`
    /// misurerebbe la nostra convenzione, non la sua capacita'.
    pub(crate) fn answer(&mut self, nome: &str, input: &Value) -> WorldReply {
        self.chiamate.push(nome.to_string());
        let pagliaio = foglie_concatenate(input);
        match self.kind {
            WorldKind::Catena => self.risposta_catena(&pagliaio),
            WorldKind::Recupero => self.risposta_recupero(nome, &pagliaio),
        }
    }

    /// La catena: chi indirizza l'anello k riceve l'anello k+1 — TRANNE all'anello
    /// cieco, dove la pista si interrompe e va ripresa dall'elenco precedente.
    ///
    /// Il match sul token PRECEDE qualunque considerazione sul nome del tool: un
    /// `run_command` che passa l'handle dentro `cat` vale quanto un `read_file` che
    /// lo passa in `path` — il modello e' libero di scegliere lo strumento, e
    /// bocciare una preferenza di stile fra i due misurerebbe noi.
    fn risposta_catena(&mut self, pagliaio: &str) -> WorldReply {
        if let Some(k) = self.anello_indirizzato(pagliaio) {
            return self.dalla_pista(k);
        }
        if let Some(k) = self.voce_estranea_indirizzata(pagliaio) {
            return self.dalla_voce_estranea(k);
        }
        // Nessun handle: il modello ha inventato un bersaglio (tipicamente un path
        // plausibile dal pre-training).
        WorldReply::errore(
            "E_HANDLE_REQUIRED",
            json!({ "hint": "il bersaglio deve essere un handle ottenuto da una chiamata precedente" }),
        )
    }

    /// Chi indirizza un anello della pista avanza — TRANNE sull'anello cieco, dove
    /// la pista finisce, e finisce per SEMPRE: ripresentare lo stesso handle non la
    /// riapre.
    ///
    /// L'anello resta CONTATO: il modello ci e' arrivato seguendo il criterio, ed e'
    /// stato il mondo a chiudergli la strada. Toglierglielo punirebbe la mossa
    /// giusta e renderebbe indistinguibile chi segue il criterio da chi tira a
    /// indovinare, cioe' esattamente la separazione che questo profilo esiste per
    /// misurare.
    fn dalla_pista(&mut self, k: usize) -> WorldReply {
        if k != self.seed.anello_cieco() {
            return self.pianta_prossimo(k);
        }
        self.ramo_cieco_annunciato = true;
        WorldReply::errore_parlante("E_BRANCH_CLOSED", RIMEDIO_RAMO_CIECO, true, json!({}))
    }

    /// Chi indirizza la voce di un ALTRO custode ha sbagliato criterio — TRANNE
    /// dopo l'interruzione e sul suo anello, dove quella voce e' la via di rientro.
    ///
    /// E' LA DEVIAZIONE, il punto in cui una regola appena imparata va sospesa
    /// perche' il mondo l'ha detto: seguire il criterio non basta piu', bisogna aver
    /// letto l'errore e ricordarsi l'elenco di due turni prima. Fuori da li' la
    /// voce estranea resta sbagliata, o "prendi sempre l'altra" diventerebbe una
    /// strategia valida ovunque.
    fn dalla_voce_estranea(&mut self, k: usize) -> WorldReply {
        if self.ramo_cieco_annunciato && k == self.seed.anello_cieco() {
            return self.pianta_prossimo(k);
        }
        // `retryable: true`: la mossa giusta esiste ed e' a portata di mano (l'altra
        // voce dello stesso elenco). Dichiarare `false` qui sarebbe l'errore che
        // azzero' il profilo di recupero — vietare cio' che si pretende — con
        // l'aggravante che un modello obbediente si arrenderebbe al primo passo
        // falso invece di correggere il tiro.
        WorldReply::errore_parlante("E_OWNER_MISMATCH", RIMEDIO_CUSTODE, true, json!({}))
    }

    /// L'anello della pista che la chiamata indirizza, il piu' avanti se ne nomina
    /// piu' d'uno: ripresentare un handle vecchio non guadagna un anello nuovo.
    fn anello_indirizzato(&self, pagliaio: &str) -> Option<usize> {
        (0..=self.frontiera).rev().find(|k| pagliaio.contains(&self.seed.handle(*k)))
    }

    /// L'anello di cui la chiamata indirizza la voce ESTRANEA (quella di un altro
    /// custode). Cercata su tutti gli anelli consegnati, non solo sui primi due:
    /// il distrattore vive a ogni passo, e riconoscerlo solo all'inizio faceva
    /// passare per "bersaglio inventato" un abbaglio dell'ottavo elenco.
    fn voce_estranea_indirizzata(&self, pagliaio: &str) -> Option<usize> {
        (0..=self.frontiera).rev().find(|k| pagliaio.contains(&self.seed.esca(*k)))
    }

    /// Consegna l'anello k+1 a chi ha indirizzato il k.
    fn pianta_prossimo(&mut self, k: usize) -> WorldReply {
        let prossimo = self.seed.handle(k + 1);
        if !self.emessi.contains(&prossimo) {
            self.emessi.push(prossimo.clone());
        }
        self.frontiera = self.frontiera.max(k + 1);
        let testo = self.elenco(k + 1);
        WorldReply::ok(testo, Some(prossimo))
    }

    /// L'elenco dell'anello `k`: due voci della stessa forma, distinte SOLO dal
    /// custode a cui sono affidate.
    ///
    /// Cos'e' sparito, e perche': il campo `state` (`current`/`superseded`) e la
    /// nota "usa la voce current". Erano un'etichetta che tradiva il distrattore, e
    /// rendevano vincente una strategia di una riga sola. Ora le due voci sono
    /// simmetriche per forma e per etichetta: l'unica differenza e' un valore da
    /// confrontare con un criterio che sta nel primo messaggio.
    ///
    /// L'ORDINE viene dal seme, non e' fisso: chi prende sempre il primo `ref`
    /// sbaglia in circa meta' degli anelli, quindi non arriva in fondo. Con la voce
    /// buona sempre in testa avremmo lasciato in piedi una seconda scorciatoia di
    /// una riga, appena tolta la prima.
    fn elenco(&self, k: usize) -> String {
        let vera = json!({ "ref": self.seed.handle(k), "owner": self.seed.custode() });
        let estranea =
            json!({ "ref": self.seed.esca(k), "owner": self.seed.custode_estraneo(k) });
        let entries = if self.seed.frazione(&format!("ordine:{k}")) < 0.5 {
            json!([vera, estranea])
        } else {
            json!([estranea, vera])
        };
        json!({ "entries": entries }).to_string()
    }

    /// Il recupero: il primo contatto col tool bersaglio fallisce, SEMPRE, e l'errore
    /// porta con se' il dato che serve a rimediare. Il token nell'errore e' cio' che
    /// rende "informato" un fatto invece di un giudizio: non esiste altro posto da
    /// cui il modello possa averlo preso.
    fn risposta_recupero(&mut self, nome: &str, pagliaio: &str) -> WorldReply {
        if !self.guasto_scattato {
            self.guasto_scattato = true;
            let tok = self.seed.token_errore(nome);
            self.token_errore = Some(tok.clone());
            // Il dato c'e', l'azione no: l'handle e' scaduto e l'errore dice quale
            // sia quello valido. Ripetere identico non puo' funzionare; leggere si'.
            //
            // L'errore DICE COSA FARE (`message` esplicito). Storia delle tarature,
            // perche' non si regredisca:
            //   1. `retryable:false` + niente message -> 0/30: auto-contraddittorio
            //      (il token invitava, l'header vietava; i modelli obbedivano).
            //   2. `retryable:true` + niente message -> ancora 0 conclusivi su glm-5,
            //      minimax-m2, deepseek-r1: la mappatura "current_epoch -> mettilo
            //      negli argomenti" non e' derivabile da NESSUN canale osservabile.
            //      Uno 0% unanime e' un pavimento da design, non una misura (il
            //      principio di raggiungibilita' di BFCL V3; ToolMaze misura floor
            //      effect sotto il 20% sui fault impliciti-permanenti).
            //   3. Ora: message che nomina il campo e l'azione, come fanno le API
            //      vere (409 con "retry with..."). Resta un test di CAPACITA', non
            //      di obbedienza: il rimedio sta NELL'ERRORE (che il modello deve
            //      leggere e applicare), mai nell'ISTRUZIONE del task, che continua
            //      a non annunciare il guasto. Sui benchmark pubblicati i frontier
            //      passano il recupero informato al 40-60% (ToolMaze): se qui
            //      saturasse al 100%, il gradino successivo e' spostare il rimedio
            //      nello SCHEMA del tool (livello 2), non togliere il message.
            return WorldReply::errore_parlante(
                "E_HANDLE_STALE",
                RIMEDIO_STALE,
                true,
                json!({ "current_epoch": tok }),
            );
        }
        match self.token_errore.as_deref() {
            // Ha portato il token che solo l'errore conteneva: ha letto e si e'
            // adattato.
            Some(t) if pagliaio.contains(t) => WorldReply::ok(
                json!({ "ok": true, "note": "epoch accettata" }).to_string(),
                Some(t.to_string()),
            ),
            // Ha riprovato senza il token: la stessa azione, o una diversa ma cieca.
            _ => WorldReply::errore("E_HANDLE_STALE", json!({ "retryable": false })),
        }
    }
}

/// Tutti i valori-foglia dell'input, concatenati. Serve a cercare un token
/// OVUNQUE il modello l'abbia messo: in `path`, in `handle`, dentro `command`, in un
/// array annidato. Il nome del campo e' una convenzione nostra; il fatto e' che il
/// token sia stato riportato.
fn foglie_concatenate(v: &Value) -> String {
    let mut out = String::new();
    raccogli(v, &mut out);
    out
}

fn raccogli(v: &Value, out: &mut String) {
    match v {
        Value::String(s) => {
            out.push_str(s);
            out.push('\n');
        }
        Value::Array(a) => a.iter().for_each(|x| raccogli(x, out)),
        Value::Object(o) => o.values().for_each(|x| raccogli(x, out)),
        altro => {
            out.push_str(&altro.to_string());
            out.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seme() -> TokenSeed {
        TokenSeed {
            provider: "p".into(),
            model: "m".into(),
            profile_key: "agentic_chain".into(),
            attempt: 1,
            seed: 42,
        }
    }

    /// Stesso seme = stessi token (una riga di evidence si rigioca identica);
    /// tentativo diverso = token diversi (l'istanza e' fresca, non memorizzabile).
    #[test]
    fn i_token_sono_deterministici_ma_freschi() {
        let a = seme();
        let b = seme();
        assert_eq!(a.handle(0), b.handle(0), "stesso seme -> stesso token");

        let mut c = seme();
        c.attempt = 2;
        assert_ne!(a.handle(0), c.handle(0), "tentativo diverso -> token diverso");

        let mut d = seme();
        d.seed = 43;
        assert_ne!(a.handle(0), d.handle(0), "seed diverso -> token diverso");
    }

    /// Il vero e l'esca sono indistinguibili per forma: chi discrimina lo fa
    /// leggendo, non guardando la lunghezza.
    #[test]
    fn l_esca_ha_la_stessa_forma_del_vero() {
        let s = seme();
        assert_ne!(s.handle(1), s.esca(1));
        assert_eq!(s.handle(1).len(), s.esca(1).len());
        assert!(s.handle(1).starts_with("H-") && s.esca(1).starts_with("H-"));
    }

    /// L'INVARIANTE: nessun token puo' essere gia' nella richiesta iniziale,
    /// altrimenti la catena e' scorciatoiabile e il test misura la nostra ingenuita'
    /// invece del modello. E' la regola del needle, resa guard.
    #[test]
    fn un_token_gia_nella_richiesta_rende_il_mondo_non_costruibile() {
        let s = seme();
        let trapelato = s.handle(2);
        let e = ScriptedWorld::new(WorldKind::Catena, seme(), &[&trapelato]);
        assert!(e.is_err(), "un token visibile nella richiesta deve impedire il giro");
        assert!(ScriptedWorld::new(WorldKind::Catena, seme(), &["nessun token qui"]).is_ok());
    }

    /// Un path inventato non apre la catena: e' il prior del pre-training
    /// (`read_file("src/main.rs")`) che va negato, o il primo anello si salta.
    #[test]
    fn un_path_letterale_non_e_un_bersaglio() {
        let mut w = ScriptedWorld::new(WorldKind::Catena, seme(), &[]).unwrap();
        let r = w.answer("read_file", &json!({ "path": "src/main.rs" }));
        assert!(r.is_error);
        assert!(r.text.contains("E_HANDLE_REQUIRED"));
        assert!(r.planted.is_none(), "un bersaglio inventato non consegna anelli");
    }

    /// Il tool non conta, conta il bersaglio: `run_command` con `cat <handle>` deve
    /// valere quanto `read_file`. Bocciare la scelta dello strumento misurerebbe la
    /// nostra preferenza di stile.
    #[test]
    fn il_bersaglio_conta_il_nome_del_tool_no() {
        let mut w = ScriptedWorld::new(WorldKind::Catena, seme(), &[]).unwrap();
        let h0 = w.handle_iniziale();
        let r = w.answer("run_command", &json!({ "command": format!("cat {h0}") }));
        assert!(!r.is_error, "l'handle era dentro il comando: e' un bersaglio valido");
        assert!(r.planted.is_some(), "chi indirizza l'anello 0 riceve l'anello 1");
    }

    /// La voce di un ALTRO custode non apre nulla, e non e' un errore di trasporto:
    /// e' il criterio del compito che non e' stato rispettato. L'errore lo NOMINA
    /// (confronta `owner`) senza dire quale sia il valore giusto: il custode e' nel
    /// primo messaggio, e ricordarselo e' meta' del test.
    #[test]
    fn chi_segue_la_voce_di_un_altro_custode_non_avanza() {
        let mut w = ScriptedWorld::new(WorldKind::Catena, seme(), &[]).unwrap();
        let h0 = w.handle_iniziale();
        w.answer("read_file", &json!({ "path": h0 })); // anello 1 consegnato
        let r = w.answer("read_file", &json!({ "path": seme().esca(1) }));
        assert!(r.is_error);
        assert!(r.text.contains("E_OWNER_MISMATCH"));
        assert!(r.planted.is_none());
        assert!(r.text.contains("owner"), "l'errore nomina il criterio, non il valore");
        assert!(
            !r.text.contains(&seme().custode()),
            "e NON regala il custode: sta nell'istruzione, va ricordato"
        );
    }

    /// L'ETICHETTA NON C'E' PIU'. E' la regressione da cui nasce tutto il ridisegno:
    /// finche' l'elenco marcava la voce buona con `state: "current"`, la strategia
    /// vincente era cercare quella parola — e infatti passavano tutti, piccoli
    /// inclusi. Le due voci ora sono simmetriche per forma E per etichetta.
    #[test]
    fn l_elenco_non_contiene_nessuna_etichetta_che_tradisca_la_voce_buona() {
        let mut w = ScriptedWorld::new(WorldKind::Catena, seme(), &[]).unwrap();
        let h0 = w.handle_iniziale();
        let r = w.answer("read_file", &json!({ "path": h0 }));
        for parola in ["current", "superseded", "state", "note", "valid", "latest"] {
            assert!(
                !r.text.contains(parola),
                "l'elenco non deve contenere '{parola}': e' un'etichetta che tradisce \
                 il distrattore e rende vincente una ricerca di stringa. {}",
                r.text
            );
        }
        let v: Value = serde_json::from_str(&r.text).expect("l'elenco e' JSON");
        let voci = v["entries"].as_array().expect("due voci").clone();
        assert_eq!(voci.len(), 2);
        // Le uniche chiavi sono le stesse per entrambe: si distinguono per VALORE.
        for voce in &voci {
            let chiavi: std::collections::BTreeSet<&str> =
                voce.as_object().expect("oggetto").keys().map(String::as_str).collect();
            assert_eq!(
                chiavi,
                ["owner", "ref"].into_iter().collect(),
                "stessa forma per entrambe: si distinguono per VALORE, non per campi"
            );
        }
        assert_ne!(voci[0]["owner"], voci[1]["owner"], "custodi diversi");
    }

    /// L'ORDINE delle voci non e' fisso. Senza questo, tolta la scorciatoia
    /// "cerca current" ne restava un'altra da una riga sola: "prendi il primo ref".
    /// Su una manciata di anelli le due posizioni devono comparire entrambe.
    #[test]
    fn la_voce_buona_non_sta_sempre_in_testa() {
        let mut posizioni = std::collections::BTreeSet::new();
        for attempt in 1..12 {
            let mut s = seme();
            s.attempt = attempt;
            let mut w = ScriptedWorld::new(WorldKind::Catena, s.clone(), &[]).unwrap();
            let r = w.answer("read_file", &json!({ "path": s.handle(0) }));
            let v: Value = serde_json::from_str(&r.text).expect("l'elenco e' JSON");
            let prima = v["entries"][0]["ref"].as_str().unwrap_or_default().to_string();
            posizioni.insert(prima == s.handle(1));
        }
        assert_eq!(
            posizioni.len(),
            2,
            "la voce buona deve comparire sia in testa sia in coda: se stesse sempre \
             per prima, 'prendi il primo ref' sarebbe la nuova scorciatoia"
        );
    }

    /// LA PISTA SI INTERROMPE, e l'errore DICE COSA FARE. E' il vincolo di
    /// raggiungibilita' pagato 0/30 due volte sul profilo di recupero: un ostacolo
    /// il cui rimedio non e' derivabile non e' severo, e' un pavimento.
    #[test]
    fn il_ramo_cieco_dichiara_il_proprio_rimedio() {
        let s = seme();
        let mut w = ScriptedWorld::new(WorldKind::Catena, s.clone(), &[]).unwrap();
        for k in 0..s.anello_cieco() {
            let r = w.answer("read_file", &json!({ "path": s.handle(k) }));
            assert!(!r.is_error, "prima dell'interruzione la pista scorre: anello {k}");
        }
        let r = w.answer("read_file", &json!({ "path": s.handle(s.anello_cieco()) }));
        assert!(r.is_error);
        assert!(r.text.contains("E_BRANCH_CLOSED"));
        assert!(r.planted.is_none(), "un ramo chiuso non consegna anelli");
        let v: Value = serde_json::from_str(&r.text).expect("l'errore e' JSON");
        let msg = v["error"]["message"].as_str().unwrap_or_default();
        assert!(
            msg.contains("OTHER entry") && msg.contains("listing"),
            "il rimedio va DICHIARATO: torna all'elenco, prendi l'altra voce. {msg}"
        );
        assert!(
            !msg.contains(&s.esca(s.anello_cieco())),
            "ma il ref giusto NON si regala: va ripescato leggendo indietro"
        );
    }

    /// LA DEVIAZIONE: dopo l'interruzione la voce scartata diventa la via di
    /// rientro, e la catena riprende. Senza questo il ramo cieco sarebbe un muro e
    /// il profilo misurerebbe un pavimento invece dei modelli.
    #[test]
    fn dopo_l_interruzione_la_voce_scartata_riporta_sulla_pista() {
        let s = seme();
        let d = s.anello_cieco();
        let mut w = ScriptedWorld::new(WorldKind::Catena, s.clone(), &[]).unwrap();
        for k in 0..=d {
            w.answer("read_file", &json!({ "path": s.handle(k) }));
        }
        let r = w.answer("read_file", &json!({ "path": s.esca(d) }));
        assert!(!r.is_error, "la voce scartata e' la via di rientro: {}", r.text);
        assert_eq!(r.planted.as_deref(), Some(s.handle(d + 1).as_str()));
        // E la pista prosegue normale da li' in poi.
        let dopo = w.answer("read_file", &json!({ "path": s.handle(d + 1) }));
        assert!(!dopo.is_error, "oltre la deviazione la catena e' di nuovo lineare");
    }

    /// La deviazione si apre SOLO dopo l'interruzione e SOLO sul suo anello: prima,
    /// la stessa voce e' un errore di criterio. Senza questo asse, "prendi sempre
    /// l'altra voce" diventerebbe una strategia valida ovunque.
    #[test]
    fn la_deviazione_non_e_aperta_prima_dell_interruzione() {
        let s = seme();
        let d = s.anello_cieco();
        let mut w = ScriptedWorld::new(WorldKind::Catena, s.clone(), &[]).unwrap();
        for k in 0..d {
            w.answer("read_file", &json!({ "path": s.handle(k) }));
        }
        // Siamo ARRIVATI all'anello cieco ma non l'abbiamo ancora toccato.
        let r = w.answer("read_file", &json!({ "path": s.esca(d) }));
        assert!(r.is_error, "senza l'interruzione la voce estranea resta sbagliata");
        assert!(r.text.contains("E_OWNER_MISMATCH"));
    }

    /// Ripresentare l'handle del ramo chiuso non lo riapre: l'interruzione e'
    /// permanente e dichiarata tale. Chi insiste registra `repeated_failed`.
    #[test]
    fn insistere_sul_ramo_chiuso_non_lo_riapre() {
        let s = seme();
        let d = s.anello_cieco();
        let mut w = ScriptedWorld::new(WorldKind::Catena, s.clone(), &[]).unwrap();
        for k in 0..=d {
            w.answer("read_file", &json!({ "path": s.handle(k) }));
        }
        let r = w.answer("read_file", &json!({ "path": s.handle(d) }));
        assert!(r.is_error && r.text.contains("E_BRANCH_CLOSED"));
        assert!(r.planted.is_none());
    }

    /// RECUPERO: il primo contatto fallisce sempre (first-touch, mai una
    /// probabilita') e l'errore porta il dato che serve.
    #[test]
    fn il_primo_contatto_fallisce_e_l_errore_porta_il_dato() {
        let mut w = ScriptedWorld::new(WorldKind::Recupero, seme(), &[]).unwrap();
        let r = w.answer("read_file", &json!({ "path": "qualunque" }));
        assert!(r.is_error);
        assert!(r.text.contains("E_HANDLE_STALE"));
        let tok = w.token_errore_emesso().expect("il guasto ha piantato il token").to_string();
        assert!(r.text.contains(&tok), "il token vive dentro il messaggio d'errore");
        // CONTRATTO DI RISOLVIBILITA' (dal floor effect 0/30 misurato due volte):
        // l'errore deve DIRE COSA FARE, nominando il campo esatto che porta il
        // token. Senza, la mappatura "token -> argomenti del retry" non e'
        // derivabile da nessun canale osservabile e il test misura un pavimento,
        // non i modelli. Il rimedio sta nell'errore, mai nell'istruzione del task.
        let v: Value = serde_json::from_str(&r.text).expect("l'errore e' JSON");
        let msg = v["error"]["message"].as_str().unwrap_or_default();
        assert!(
            msg.contains("current_epoch") && msg.to_lowercase().contains("retry"),
            "il message deve nominare il campo del token e l'azione: {msg}"
        );
    }

    /// Il RECUPERO VERO: la chiamata dopo porta il token che SOLO l'errore conteneva.
    /// Non c'e' altro posto da cui possa averlo preso: "informato dall'errore" e' un
    /// fatto, non un giudizio.
    #[test]
    fn chi_porta_il_token_dell_errore_ha_letto_l_errore() {
        let mut w = ScriptedWorld::new(WorldKind::Recupero, seme(), &[]).unwrap();
        w.answer("read_file", &json!({ "path": "x" }));
        let tok = w.token_errore_emesso().unwrap().to_string();
        let r = w.answer("read_file", &json!({ "epoch": tok }));
        assert!(!r.is_error, "ha usato il dato dell'errore: recupero riuscito");
    }

    /// Ripetere la chiamata fallita non recupera. E nemmeno cambiare tool a caso:
    /// senza il token, l'azione e' cieca.
    #[test]
    fn ripetere_o_cambiare_alla_cieca_non_e_un_recupero() {
        let mut w = ScriptedWorld::new(WorldKind::Recupero, seme(), &[]).unwrap();
        w.answer("read_file", &json!({ "path": "x" }));
        assert!(w.answer("read_file", &json!({ "path": "x" })).is_error, "identica");
        assert!(w.answer("list_files", &json!({ "path": "y" })).is_error, "diversa ma cieca");
    }
}
