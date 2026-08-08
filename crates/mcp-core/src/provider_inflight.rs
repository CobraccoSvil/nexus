//! PUNTO UNICO (regola L) della domanda: **quante chiamate sono IN VOLO verso
//! questo fornitore adesso, e posso mandargliene un'altra?**
//!
//! ## Il difetto che ha reso necessario il modulo (08/08/2026)
//!
//! Progetto gestione-corsi. Il Consiglio ha convocato otto figure in fan-out —
//! `provider_analyst` (due), `program_manager`, `project_manager`,
//! `functional_analyst`, `security_engineer`, `software_architect`,
//! `ui_ux_designer` — e sono scadute TUTTE E OTTO. Zero pareri su otto, budget
//! consumato per intero, nessun prodotto. Nello stesso momento tre fornitori su
//! nove erano fuori per credito esaurito (anthropic, openai, perplexity) e ne
//! restavano cinque a servire otto chiamate concorrenti, piu' il carico di
//! quattro sessioni di sviluppo in parallelo.
//!
//! Il tempo speso NON era tempo di risposta del modello: era tempo di CODA. Un
//! timeout adattivo sulla latenza avrebbe misurato la cosa sbagliata — il
//! fornitore rispondeva bene, semplicemente non a otto chiamate insieme.
//!
//! ## Perche' la domanda non esisteva
//!
//! Il sistema sapeva gia' rispondere sullo STATO di un fornitore:
//! [`crate::provider_cooldown`] («e' escluso adesso?»), `nexus_provider_health`
//! («ha credito?»). Nessuna di quelle domande riguarda il CARICO ISTANTANEO, e
//! nessuna cambia risposta perche' altre sette chiamate sono gia' partite un
//! millisecondo fa. Otto figure che partono insieme sceglievano tutte fra gli
//! stessi cinque fornitori, e nessuna sapeva delle altre sette.
//!
//! Due semafori esistevano gia' e non potevano coprire il caso, perche'
//! governano il NUMERO e mai la DESTINAZIONE: `subagent_fanout_max_parallel`
//! (locale al fan-out, default 6) e `fanout_process_max_parallel` (di processo,
//! default 12). Dodici chiamate concorrenti che vanno tutte allo stesso
//! fornitore rispettano entrambi i tetti.
//!
//! ## Il contatore che non torna a zero
//!
//! E' il difetto classico di questa forma, e qui non e' rappresentabile: il
//! conteggio lo tiene una GUARDIA RAII ([`PermessoChiamata`]) che decrementa nel
//! proprio `Drop`. Un task tokio cancellato a meta' chiamata — cioe' ogni figura
//! che scade, che e' esattamente il caso misurato — droppa il future, quindi la
//! guardia, quindi decrementa. Un decremento esplicito scritto dopo l'`await`
//! non verrebbe mai eseguito su quel percorso, e il fornitore risulterebbe
//! eternamente saturo proprio dopo un'ondata di timeout.
//!
//! ## Il tetto e' di SCHEDULING, non di ammissione
//!
//! Va detto chiaramente perche' non se ne fraintenda la forza: allo scadere
//! dell'attesa la chiamata PARTE lo stesso, dichiarando
//! [`EsitoAttesa::CodaScaduta`]. Rifiutarla trasformerebbe un ritardo in un
//! fallimento certo, che e' peggio del problema — e la misura di successo di
//! questo lavoro e' «pareri invece di timeout», non «code piu' ordinate». Il
//! tetto serializza quando serializzare aiuta; non decide chi ha diritto di
//! chiamare. Il contatore, di conseguenza, puo' superare il tetto: e' un FATTO
//! osservato, non una quota.
//!
//! ## Confine
//!
//! Il registro e' ISTANZIABILE e l'istanza globale e' solo il default di
//! produzione ([`registro`]). E' la stessa scelta di `spawn_fanout_with`, e per
//! la stessa ragione (regola O): un test che deve riprodurre il fan-out reale
//! con fornitori limitati non puo' farlo su uno stato globale condiviso con gli
//! altri test, e un test che gira su una simulazione a un thread non misura il
//! sistema.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use uuid::Uuid;

/// Separatore della chiave di coppia. Lo stesso carattere di controllo usato da
/// [`crate::provider_cooldown`], e per la stessa ragione: non compare in nessun
/// nome di fornitore o di modello, quindi `a\u{1}b` non e' ambiguo mentre
/// `a/b` lo sarebbe (i nomi di modello contengono `/`).
const SEP: char = '\u{1}';

/// Setting (regola G) del tetto di chiamate concorrenti verso UN fornitore.
pub const KEY_MAX_PER_PROVIDER: &str = "routing.inflight_max_per_provider";
/// Quante chiamate concorrenti verso lo stesso fornitore prima di accodare, se
/// il DB tace. Tre non e' una taratura fine: e' il punto in cui, con cinque
/// fornitori disponibili, un fan-out da otto viene servito senza che nessun
/// fornitore ne riceva piu' di tre insieme.
pub const DEFAULT_MAX_PER_PROVIDER: usize = 3;

/// Setting (regola G) del tetto di ATTESA in coda.
pub const KEY_QUEUE_WAIT_MAX_S: &str = "routing.inflight_queue_wait_max_s";
/// Quanto si attende al massimo un permesso, se il DB tace. Generoso di
/// proposito: l'attesa e' utile solo se dura abbastanza da far passare davanti
/// una chiamata intera. Piu' corto di cosi' equivarrebbe a non accodare.
pub const DEFAULT_QUEUE_WAIT_MAX_S: u64 = 90;

/// Per quanto si ricorda l'attesa di un run che non l'ha mai riscossa. Solo i
/// run che scadono leggono il proprio tempo di coda; quelli che finiscono bene
/// lasciano la voce, e senza potatura la mappa crescerebbe per tutta la vita del
/// processo.
const ETA_MAX_ATTESA: Duration = Duration::from_secs(3600);

/// Esito dell'attesa di un permesso. Tipizzato perche' e' cio' su cui la
/// diagnosi del timeout decide (regola Q): «ha aspettato» e «non ha aspettato»
/// portano a due rimedi opposti, e un `bool` non avrebbe spazio per il terzo
/// caso, che e' «ha aspettato invano».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EsitoAttesa {
    /// Permesso concesso subito: il fornitore non era saturo.
    Immediato,
    /// Ha atteso, poi e' passato. L'attesa e' tempo di budget speso in coda.
    Atteso { attesa: Duration },
    /// L'attesa ha superato il tetto e la chiamata parte comunque (vedi la nota
    /// sul tetto di scheduling). `in_volo` e' quante chiamate risultavano verso
    /// quel fornitore quando si e' rinunciato ad aspettare.
    CodaScaduta { atteso: Duration, in_volo: usize },
}

impl EsitoAttesa {
    /// Quanto tempo di budget se n'e' andato in coda. `Immediato` -> zero, che
    /// qui e' una misura vera e non un ripiego.
    pub fn attesa(&self) -> Duration {
        match self {
            Self::Immediato => Duration::ZERO,
            Self::Atteso { attesa } => *attesa,
            Self::CodaScaduta { atteso, .. } => *atteso,
        }
    }
}

/// Stato di un fornitore nel registro.
struct StatoFornitore {
    /// Il semaforo che serializza. Dimensionato al primo uso del fornitore: un
    /// `Semaphore` non si rimpicciolisce a caldo senza stati transitori, ed e'
    /// la stessa convenzione (dichiarata) di `process_fanout_semaphore`.
    semaforo: Arc<tokio::sync::Semaphore>,
}

/// Registro del carico. Istanziabile: vedi la nota sul confine.
pub struct RegistroCarico {
    /// Chiamate in volo per COPPIA `provider\u{1}model`. La coppia e' la grana
    /// piu' fine osservabile, e il carico e' additivo: «quante verso il
    /// fornitore» e' la somma delle sue coppie. Il contrario non sarebbe vero,
    /// ed e' il motivo per cui si conta qui e non per solo fornitore — chi
    /// sceglie fra due modelli dello stesso fornitore ha bisogno di distinguerli.
    in_volo: Mutex<HashMap<String, usize>>,
    /// Semafori per FORNITORE. Il tetto e' dell'endpoint, non del modello: due
    /// modelli dello stesso fornitore condividono la stessa connessione e la
    /// stessa capacita' di servizio.
    fornitori: Mutex<HashMap<String, StatoFornitore>>,
    /// Attesa accumulata per run, con l'istante dell'ultima scrittura per la
    /// potatura.
    attesa_per_run: Mutex<HashMap<Uuid, (Duration, Instant)>>,
    /// Permessi per fornitore, fissati alla costruzione del registro.
    max_per_provider: usize,
    /// Tetto d'attesa.
    attesa_max: Duration,
}

impl RegistroCarico {
    /// Registro con tetti espliciti. I test lo costruiscono cosi'; la
    /// produzione passa da [`registro_da_settings`], che i tetti li legge dal DB.
    pub fn new(max_per_provider: usize, attesa_max: Duration) -> Self {
        Self {
            in_volo: Mutex::new(HashMap::new()),
            fornitori: Mutex::new(HashMap::new()),
            attesa_per_run: Mutex::new(HashMap::new()),
            // Zero permessi bloccherebbe tutto per sempre: un tetto non valido
            // degrada al default, mai al silenzio.
            max_per_provider: max_per_provider.max(1),
            attesa_max,
        }
    }

    /// Chiamate in volo verso una COPPIA.
    pub fn in_volo_verso_modello(&self, provider: &str, model: &str) -> usize {
        let chiave = chiave_coppia(provider, model);
        self.in_volo
            .lock()
            .ok()
            .and_then(|m| m.get(&chiave).copied())
            .unwrap_or(0)
    }

    /// Chiamate in volo verso un FORNITORE, sommate su tutti i suoi modelli.
    /// E' la domanda che si pone chi deve scegliere dove mandare la prossima.
    pub fn in_volo_verso_fornitore(&self, provider: &str) -> usize {
        let prefisso = format!("{}{SEP}", provider.trim().to_lowercase());
        self.in_volo
            .lock()
            .ok()
            .map(|m| {
                m.iter()
                    .filter(|(k, _)| k.starts_with(&prefisso))
                    .map(|(_, v)| *v)
                    .sum()
            })
            .unwrap_or(0)
    }

    /// Fra `candidati`, quale ricevera' MENO chiamate.
    ///
    /// Il carico atteso di un fornitore e' quello che ha in volo ADESSO piu' le
    /// chiamate che stanno per partirgli addosso in questo stesso giro
    /// (`prenotati`). Le seconde contano quanto le prime: nel fan-out del
    /// Consiglio i pin si decidono in sequenza prima che parta chiunque, quindi
    /// guardando il solo volo tutte e otto le figure vedrebbero il sistema
    /// scarico e sceglierebbero lo stesso fornitore — che e' precisamente il
    /// difetto, in una forma nuova.
    ///
    /// A parita' di carico vince il PRIMO, cioe' l'ordine di preferenza che il
    /// chiamante ha gia' stabilito (costo, tier, salute). Il carico e' un
    /// tie-break fra pari, non un criterio che scavalca la qualita': il
    /// fornitore meno carico non e' per questo il migliore.
    pub fn indice_meno_carico(
        &self,
        candidati: &[String],
        prenotati: &HashMap<String, usize>,
    ) -> Option<usize> {
        candidati
            .iter()
            .enumerate()
            .min_by_key(|(i, p)| {
                let chiave = p.trim().to_lowercase();
                let carico =
                    self.in_volo_verso_fornitore(&chiave) + prenotati.get(&chiave).copied().unwrap_or(0);
                // `*i` come secondo criterio rende il minimo STABILE: senza,
                // `min_by_key` sceglierebbe fra i pari in modo dipendente
                // dall'implementazione, e due run identici darebbero pin diversi.
                (carico, *i)
            })
            .map(|(i, _)| i)
    }

    /// Il semaforo di un fornitore, creandolo al primo uso.
    fn semaforo_di(&self, provider: &str) -> Option<Arc<tokio::sync::Semaphore>> {
        let chiave = provider.trim().to_lowercase();
        let mut mappa = self.fornitori.lock().ok()?;
        let stato = mappa.entry(chiave).or_insert_with(|| StatoFornitore {
            semaforo: Arc::new(tokio::sync::Semaphore::new(self.max_per_provider)),
        });
        Some(stato.semaforo.clone())
    }

    /// Chiede il permesso di chiamare `provider`/`model`, attendendo se il
    /// fornitore e' saturo.
    ///
    /// Ritorna SEMPRE una guardia: allo scadere dell'attesa la chiamata parte
    /// comunque (vedi la nota sul tetto di scheduling) e l'esito lo dichiara.
    /// `run_id` serve solo a ricordare quanto ha atteso quel run — `None` per
    /// le chiamate che non appartengono a un run (classificatore, wizard,
    /// discovery), che non hanno un budget da spendere in coda.
    pub async fn permesso(
        self: &Arc<Self>,
        provider: &str,
        model: &str,
        run_id: Option<Uuid>,
    ) -> (PermessoChiamata, EsitoAttesa) {
        let (permesso_sem, esito) = self.acquisisci(provider).await;

        let attesa = esito.attesa();
        if !attesa.is_zero() {
            self.annota_attesa(run_id, attesa);
        }

        let chiave = chiave_coppia(provider, model);
        if let Ok(mut m) = self.in_volo.lock() {
            *m.entry(chiave.clone()).or_insert(0) += 1;
        }
        (
            PermessoChiamata {
                registro: Arc::clone(self),
                chiave,
                _permesso: permesso_sem,
            },
            esito,
        )
    }

    /// L'ATTESA vera e propria: prende il posto sul fornitore e dice com'e'
    /// andata. Separata da [`RegistroCarico::permesso`] perche' sono due
    /// responsabilita' diverse — qui si aspetta, di la' si CONTA — e perche' il
    /// vincolo sul lock (sotto) vale solo per questa meta'.
    ///
    /// Il lock del registro NON attraversa mai l'await: si prende l'Arc del
    /// semaforo e lo si rilascia subito. Tenerlo attraverso l'attesa
    /// bloccherebbe ogni altra chiamata del processo, incluse quelle verso
    /// fornitori diversi — cioe' produrrebbe, in forma peggiore, esattamente la
    /// serializzazione globale che questo modulo esiste per evitare.
    async fn acquisisci(
        &self,
        provider: &str,
    ) -> (Option<tokio::sync::OwnedSemaphorePermit>, EsitoAttesa) {
        // Registro non interrogabile (lock avvelenato): si procede senza governo
        // del carico invece di fermare il sistema. E' una rinuncia dichiarata,
        // non un successo.
        let Some(sem) = self.semaforo_di(provider) else {
            return (None, EsitoAttesa::Immediato);
        };
        if let Ok(p) = sem.clone().try_acquire_owned() {
            return (Some(p), EsitoAttesa::Immediato);
        }
        let inizio = Instant::now();
        match tokio::time::timeout(self.attesa_max, sem.acquire_owned()).await {
            Ok(Ok(p)) => (
                Some(p),
                EsitoAttesa::Atteso {
                    attesa: inizio.elapsed(),
                },
            ),
            // Semaforo chiuso: non accade (mai `close()`), ma non si inventa un
            // permesso ne' si blocca la chiamata.
            Ok(Err(_)) => (None, EsitoAttesa::Immediato),
            Err(_) => (
                None,
                EsitoAttesa::CodaScaduta {
                    atteso: inizio.elapsed(),
                    in_volo: self.in_volo_verso_fornitore(provider),
                },
            ),
        }
    }

    /// Somma l'attesa al conto del run e pota le voci vecchie.
    fn annota_attesa(&self, run_id: Option<Uuid>, attesa: Duration) {
        let Some(run_id) = run_id else { return };
        let Ok(mut mappa) = self.attesa_per_run.lock() else {
            return;
        };
        let ora = Instant::now();
        mappa.retain(|_, (_, visto)| ora.duration_since(*visto) < ETA_MAX_ATTESA);
        let voce = mappa.entry(run_id).or_insert((Duration::ZERO, ora));
        voce.0 += attesa;
        voce.1 = ora;
    }

    /// Quanto ha atteso in coda questo run, in tutto.
    ///
    /// `None` significa «non misurata» e non «zero»: un run che non e' passato
    /// da qui non ha atteso *per quanto ne sappiamo*, ed e' una cosa diversa
    /// dall'aver atteso zero (regola Q). La lettura CONSUMA la voce: chi la
    /// chiede sta chiudendo il run, e tenerla servirebbe solo a far crescere la
    /// mappa.
    pub fn attesa_del_run(&self, run_id: Uuid) -> Option<Duration> {
        self.attesa_per_run
            .lock()
            .ok()
            .and_then(|mut m| m.remove(&run_id))
            .map(|(attesa, _)| attesa)
    }
}

/// La guardia: finche' vive, la chiamata risulta in volo.
///
/// Non ha metodi. Non serve chiamare niente per «chiudere»: il conteggio
/// finisce quando il valore esce di scope, che e' l'unica forma che regge alla
/// cancellazione del task (vedi la nota in testa al modulo).
pub struct PermessoChiamata {
    registro: Arc<RegistroCarico>,
    chiave: String,
    /// Il permesso del semaforo. `None` quando la chiamata e' partita senza
    /// (coda scaduta, o registro non interrogabile): il conteggio vale lo
    /// stesso, perche' la chiamata e' in volo davvero.
    _permesso: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl Drop for PermessoChiamata {
    fn drop(&mut self) {
        if let Ok(mut m) = self.registro.in_volo.lock() {
            match m.get_mut(&self.chiave) {
                // Si rimuove a zero: una coppia mai piu' usata non deve restare
                // nella mappa, che e' anche cio' che `in_volo_verso_fornitore`
                // scorre.
                Some(n) if *n <= 1 => {
                    m.remove(&self.chiave);
                }
                Some(n) => *n -= 1,
                None => {}
            }
        }
    }
}

/// Chiave di coppia, normalizzata come quella dei cooldown.
fn chiave_coppia(provider: &str, model: &str) -> String {
    format!(
        "{}{SEP}{}",
        provider.trim().to_lowercase(),
        model.trim().to_lowercase()
    )
}

static REGISTRO: OnceLock<Arc<RegistroCarico>> = OnceLock::new();

/// Il registro di PRODUZIONE, dimensionato al primo uso dai settings.
///
/// Idempotente (OnceLock): la prima chiamata vince. I tetti si applicano al
/// riavvio del servizio, come per gli altri semafori del processo.
pub async fn registro_da_settings(db: &sqlx::PgPool) -> Arc<RegistroCarico> {
    if let Some(r) = REGISTRO.get() {
        return Arc::clone(r);
    }
    let max = leggi_usize(db, KEY_MAX_PER_PROVIDER)
        .await
        .unwrap_or(DEFAULT_MAX_PER_PROVIDER);
    let attesa = leggi_usize(db, KEY_QUEUE_WAIT_MAX_S)
        .await
        .map(|s| s as u64)
        .unwrap_or(DEFAULT_QUEUE_WAIT_MAX_S);
    let nuovo = Arc::new(RegistroCarico::new(max, Duration::from_secs(attesa)));
    let _ = REGISTRO.set(nuovo);
    Arc::clone(REGISTRO.get().expect("appena inizializzato"))
}

/// Il registro gia' inizializzato, se c'e'.
///
/// Per i consumatori che non hanno un pool a portata di mano e non devono
/// crearlo: leggere il carico e' una domanda, non un motivo per inizializzare
/// il governo della concorrenza.
pub fn registro() -> Option<Arc<RegistroCarico>> {
    REGISTRO.get().map(Arc::clone)
}

async fn leggi_usize(db: &sqlx::PgPool, chiave: &str) -> Option<usize> {
    nexus_auth::get_setting(db, chiave)
        .await
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registro_di_prova(max: usize) -> Arc<RegistroCarico> {
        Arc::new(RegistroCarico::new(max, Duration::from_millis(200)))
    }

    /// La guardia conta, e smette di contare quando esce di scope.
    #[tokio::test]
    async fn il_permesso_conta_finche_vive() {
        let r = registro_di_prova(4);
        assert_eq!(r.in_volo_verso_fornitore("acme"), 0);
        {
            let (_g, esito) = r.permesso("acme", "m1", None).await;
            assert_eq!(esito, EsitoAttesa::Immediato);
            assert_eq!(r.in_volo_verso_fornitore("acme"), 1);
            assert_eq!(r.in_volo_verso_modello("acme", "m1"), 1);
        }
        assert_eq!(
            r.in_volo_verso_fornitore("acme"),
            0,
            "uscita di scope = chiamata non piu' in volo"
        );
    }

    /// Il carico di un fornitore e' la SOMMA dei suoi modelli: e' la forma della
    /// domanda che si pone chi deve scegliere dove mandare la prossima chiamata.
    ///
    /// MUTAZIONE: contare per sola coppia e rispondere con quella (togliere la
    /// somma da `in_volo_verso_fornitore`) fa vedere 1 dove ce ne sono 2, e un
    /// fornitore gia' occupato sembrerebbe libero.
    #[tokio::test]
    async fn il_carico_del_fornitore_somma_i_suoi_modelli() {
        let r = registro_di_prova(4);
        let (_a, _) = r.permesso("acme", "m1", None).await;
        let (_b, _) = r.permesso("acme", "m2", None).await;
        assert_eq!(r.in_volo_verso_modello("acme", "m1"), 1);
        assert_eq!(r.in_volo_verso_fornitore("acme"), 2);
        assert_eq!(
            r.in_volo_verso_fornitore("altro"),
            0,
            "il carico di un fornitore non e' quello di un altro"
        );
    }

    /// IL difetto, nella forma in cui e' stato misurato: un task CANCELLATO a
    /// meta' chiamata: e' esattamente cio' che accade a ogni figura che scade,
    /// cioe' a tutte e otto quella sera.
    ///
    /// MUTAZIONE: sostituire il `Drop` con un decremento esplicito scritto dopo
    /// l'await (la forma che verrebbe naturale) lascia il contatore a 1 per
    /// sempre, e il fornitore risulta saturo dopo l'ondata di timeout —
    /// il difetto peggiora se stesso.
    #[tokio::test]
    async fn un_task_cancellato_non_lascia_la_chiamata_in_volo() {
        let r = registro_di_prova(4);
        let r2 = Arc::clone(&r);
        let task = tokio::spawn(async move {
            let (_g, _) = r2.permesso("acme", "m1", None).await;
            // Attesa che non finira' mai: il task viene abortito qui dentro,
            // con la guardia viva.
            tokio::time::sleep(Duration::from_secs(3600)).await;
        });
        // Lascia partire il task e prendere il permesso.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(r.in_volo_verso_fornitore("acme"), 1);
        task.abort();
        let _ = task.await;
        assert_eq!(
            r.in_volo_verso_fornitore("acme"),
            0,
            "un task cancellato deve liberare il conteggio: e' il caso di ogni figura che scade"
        );
    }

    /// Oltre il tetto si aspetta, e l'attesa e' DICHIARATA.
    #[tokio::test]
    async fn oltre_il_tetto_si_accoda() {
        let r = registro_di_prova(1);
        let primo = r.permesso("acme", "m1", None).await;
        let r2 = Arc::clone(&r);
        let secondo = tokio::spawn(async move { r2.permesso("acme", "m1", None).await.1 });
        tokio::time::sleep(Duration::from_millis(30)).await;
        // Il primo libera: il secondo passa dopo aver atteso.
        drop(primo);
        match secondo.await.expect("task") {
            EsitoAttesa::Atteso { attesa } => assert!(attesa > Duration::ZERO),
            altro => panic!("atteso un'attesa dichiarata, ottenuto {altro:?}"),
        }
    }

    /// Il tetto e' di SCHEDULING: scaduta l'attesa la chiamata parte lo stesso,
    /// e lo dichiara. Se rifiutasse, un ritardo diventerebbe un fallimento
    /// certo — il contrario della misura di successo.
    #[tokio::test]
    async fn scaduta_l_attesa_la_chiamata_parte_comunque() {
        let r = registro_di_prova(1);
        let _primo = r.permesso("acme", "m1", None).await;
        let (_g, esito) = r.permesso("acme", "m1", None).await;
        match esito {
            EsitoAttesa::CodaScaduta { in_volo, .. } => {
                assert_eq!(in_volo, 1, "dichiara quante ne aveva davanti");
            }
            altro => panic!("attesa una coda scaduta, ottenuto {altro:?}"),
        }
        assert_eq!(
            r.in_volo_verso_fornitore("acme"),
            2,
            "il contatore misura cio' che vola davvero, anche oltre il tetto"
        );
    }

    /// L'attesa si accumula sul RUN, e la lettura la consuma.
    #[tokio::test]
    async fn l_attesa_si_accumula_sul_run() {
        let r = registro_di_prova(1);
        let run = Uuid::new_v4();
        assert_eq!(r.attesa_del_run(run), None, "non misurata != zero");
        let _primo = r.permesso("acme", "m1", None).await;
        let (_g, _) = r.permesso("acme", "m1", Some(run)).await;
        let attesa = r.attesa_del_run(run).expect("misurata");
        assert!(attesa > Duration::ZERO);
        assert_eq!(r.attesa_del_run(run), None, "la lettura consuma la voce");
    }

    /// Fornitori diversi non si ostacolano: e' l'intero punto della distinzione
    /// per destinazione, che i due semafori preesistenti non potevano fare.
    #[tokio::test]
    async fn fornitori_diversi_non_si_accodano_a_vicenda() {
        let r = registro_di_prova(1);
        let _acme = r.permesso("acme", "m1", None).await;
        let (_altro, esito) = r.permesso("altro", "m1", None).await;
        assert_eq!(
            esito,
            EsitoAttesa::Immediato,
            "il tetto e' per fornitore, non globale"
        );
    }

    /// LA distribuzione, nella forma esatta del difetto: otto figure, cinque
    /// fornitori.
    ///
    /// Prima, dalla sesta figura in poi l'esclusione svuotava il pool e il
    /// ripiego era «il piu' preferito» per tutte e tre le eccedenti: 4-1-1-1-1.
    /// Simulando lo stesso giro col criterio del carico si ottiene 2-2-2-1-1,
    /// cioe' nessun fornitore con piu' del doppio del minimo.
    ///
    /// MUTAZIONE: togliere `prenotati` dalla somma di `indice_meno_carico` (e
    /// guardare il solo volo, che durante l'assegnazione e' zero per tutti) fa
    /// tornare sempre l'indice 0 e riproduce il 4-1-1-1-1.
    #[tokio::test]
    async fn otto_figure_su_cinque_fornitori_si_distribuiscono() {
        let r = registro_di_prova(4);
        let fornitori: Vec<String> = ["alfa", "beta", "gamma", "delta", "epsilon"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut prenotati: HashMap<String, usize> = HashMap::new();
        // Il giro di `resolve_council_assignments`: le prime cinque prendono un
        // fornitore libero ciascuna, dalla sesta si sceglie il meno carico.
        for i in 0..8usize {
            let scelto = if i < fornitori.len() {
                i
            } else {
                r.indice_meno_carico(&fornitori, &prenotati)
                    .expect("almeno un candidato")
            };
            *prenotati
                .entry(fornitori[scelto].clone())
                .or_insert(0) += 1;
        }
        let mut conteggi: Vec<usize> = fornitori
            .iter()
            .map(|f| prenotati.get(f).copied().unwrap_or(0))
            .collect();
        conteggi.sort_unstable();
        assert_eq!(
            conteggi,
            vec![1, 1, 2, 2, 2],
            "otto figure su cinque fornitori si distribuiscono, non si ammucchiano"
        );
    }

    /// Il carico gia' IN VOLO conta quanto le prenotazioni: e' cio' che rende il
    /// criterio sensibile alle ALTRE sessioni, che la sera dell'08/08 pesavano
    /// sugli stessi cinque fornitori e che nessuna convocazione poteva vedere.
    #[tokio::test]
    async fn il_volo_di_un_altra_sessione_sposta_la_scelta() {
        let r = registro_di_prova(4);
        let fornitori: Vec<String> = vec!["alfa".into(), "beta".into()];
        let vuoto = HashMap::new();
        assert_eq!(
            r.indice_meno_carico(&fornitori, &vuoto),
            Some(0),
            "a parita' vince il piu' preferito"
        );
        // Qualcun altro sta gia' occupando 'alfa'.
        let (_g1, _) = r.permesso("alfa", "m1", None).await;
        assert_eq!(
            r.indice_meno_carico(&fornitori, &vuoto),
            Some(1),
            "con alfa occupato la scelta si sposta su beta"
        );
    }

    /// Nomi normalizzati come nei cooldown: `OpenAI` e `openai` sono lo stesso
    /// fornitore, o il carico si spezzerebbe in due conti che nessuno somma.
    #[tokio::test]
    async fn il_nome_del_fornitore_e_normalizzato() {
        let r = registro_di_prova(4);
        let (_g, _) = r.permesso("  OpenAI  ", "GPT-X", None).await;
        assert_eq!(r.in_volo_verso_fornitore("openai"), 1);
        assert_eq!(r.in_volo_verso_modello("openai", "gpt-x"), 1);
    }
}
