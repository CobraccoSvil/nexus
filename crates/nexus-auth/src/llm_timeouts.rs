//! Punto unico dei timeout delle chiamate LLM (regola L + regola G).
//!
//! # Perche' esiste
//!
//! I timeout erano decisi in due crate senza che nessuno guardasse la relazione
//! fra loro, e la gerarchia risultante era INVERTITA:
//!
//! | cosa | valore | dove |
//! |---|---|---|
//! | run di una figura (N turni) | 300s | `orchestrator.subagent_default_timeout_s` |
//! | UNA chiamata gateway -> provider | 300s | `max(complete 120, stream 300)` |
//! | mcp-core -> gateway (con retry) | 435s | `120*3+45+30` |
//!
//! Il budget di UNA chiamata era `>=` al budget dell'INTERO run multi-turno che
//! la contiene: una singola chiamata appesa consumava il 100% della vita del
//! run, che moriva per `RunTimeout` con **zero iterazioni completate** (`it=0`).
//! Non era un difetto del modello: era aritmetica. Il sintomo veniva attribuito
//! di volta in volta al modello di turno (z-ai/glm-4.7-flash, poi deepseek, poi
//! google), perche' l'innesco (una chiamata lenta) e' casuale mentre la
//! conseguenza e' deterministica.
//!
//! In piu' `gateway.complete_timeout_seconds` (120) NON aveva alcun effetto: era
//! usata solo dentro un `max(complete, stream)` che la scartava sempre (300 >
//! 120), e nel gateway non esisteva NESSUN timeout logico per-richiesta. Una
//! setting viva nel DB, letta a ogni avvio, e inerte.
//!
//! # Il contratto
//!
//! Tutto deriva da due grandezze primarie lette dal DB (regola G):
//!   * `orchestrator.subagent_default_timeout_s` -> il run PIU' CORTO che
//!     contiene chiamate LLM (le figure del consiglio). E' il vincolo piu'
//!     stretto, quindi il riferimento conservativo per tutti.
//!   * `agent.llm.min_guaranteed_turns` -> quanti turni il run deve poter
//!     completare **anche nel caso peggiore** in cui ogni chiamata esaurisce il
//!     proprio budget.
//!
//! e ne discendono, con l'invariante `request_budget * min_turns <= run_timeout`
//! garantito PER COSTRUZIONE (vedi [`LlmTimeouts::derive`] e i test):
//!   * `request_budget` — deadline end-to-end di UNA `/v1/complete`, retry e
//!     chain inclusi. E' il numero che impedisce a una chiamata di mangiarsi il
//!     run.
//!   * `per_attempt` — cap su un singolo `provider.complete()`, cosi' un
//!     provider appeso non brucia il budget dell'intera chain.
//!   * `client_budget` — quanto mcp-core attende il gateway (budget + margine).
//!
//! `gateway.complete_timeout_seconds` torna EFFICACE come cap per-tentativo, ma
//! solo nella direzione che conta: puo' STRINGERE (`min`), mai sforare il
//! budget. Alzarla oltre `request_budget` non ha effetto — ed e' giusto cosi':
//! nessuna setting deve poter violare l'invariante.

use std::time::Duration;

use sqlx::PgPool;

/// Turni che un run deve poter completare nel caso peggiore. Seed: mig **0587**.
pub const DEFAULT_MIN_GUARANTEED_TURNS: u64 = 4;
/// Cap per-tentativo verso il provider (completion non-streaming). Seed: mig **0586**.
pub const DEFAULT_COMPLETE_TIMEOUT_SECS: u64 = 120;
/// Timeout dello streaming SSE verso il provider. Seed: mig **0586**.
pub const DEFAULT_STREAM_TIMEOUT_SECS: u64 = 300;
/// Timeout del run di un subagente. Seed: `orchestrator.subagent_default_timeout_s`.
pub const DEFAULT_RUN_TIMEOUT_SECS: u64 = 300;
/// Margine di rete/serializzazione sopra `request_budget` per il client
/// mcp-core -> gateway: il client non deve mollare PRIMA che il gateway abbia
/// avuto modo di rispondere entro il proprio budget (altrimenti il gateway
/// lavora per un chiamante che non c'e' piu').
pub const CLIENT_BUDGET_MARGIN_SECS: u64 = 15;
/// Sotto i 2 turni garantiti il concetto stesso di run multi-turno non esiste.
const MIN_TURNS_FLOOR: u64 = 2;
/// Budget end-to-end di una richiesta DIFFERIBILE. Seed: mig **0729**.
pub const DEFAULT_FLEX_REQUEST_BUDGET_SECS: u64 = 900;
/// Cap per-tentativo di una richiesta DIFFERIBILE. Seed: mig **0729**.
pub const DEFAULT_FLEX_PER_ATTEMPT_SECS: u64 = 900;

/// Chiave DB: budget end-to-end delle richieste differibili.
pub const KEY_FLEX_REQUEST_BUDGET: &str = "gateway.flex.request_budget_seconds";
/// Chiave DB: cap per-tentativo delle richieste differibili.
pub const KEY_FLEX_PER_ATTEMPT: &str = "gateway.flex.per_attempt_seconds";

/// Chiave DB: turni minimi garantiti per un run.
pub const KEY_MIN_GUARANTEED_TURNS: &str = "agent.llm.min_guaranteed_turns";
/// Chiave DB: cap per-tentativo completion.
pub const KEY_COMPLETE_TIMEOUT: &str = "gateway.complete_timeout_seconds";
/// Chiave DB: timeout streaming.
pub const KEY_STREAM_TIMEOUT: &str = "gateway.stream_timeout_seconds";
/// Chiave DB: timeout del run di un subagente (il run piu' corto).
pub const KEY_RUN_TIMEOUT: &str = "orchestrator.subagent_default_timeout_s";

/// I timeout LLM derivati, coerenti fra loro per costruzione.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LlmTimeouts {
    /// Budget dell'intero run multi-turno (riferimento: il piu' corto).
    pub run_timeout: Duration,
    /// Deadline end-to-end di UNA `/v1/complete` (retry + chain inclusi).
    pub request_budget: Duration,
    /// Cap su un singolo `provider.complete()`.
    pub per_attempt: Duration,
    /// Attesa di mcp-core verso il gateway.
    pub client_budget: Duration,
    /// Timeout dello streaming SSE.
    pub stream_timeout: Duration,
    /// Turni minimi garantiti usati nella derivazione.
    pub min_guaranteed_turns: u64,
    /// Il cap per-tentativo GREZZO, prima del `min` col budget.
    ///
    /// Serve a [`Self::for_run`]: ri-derivare passando `per_attempt` (gia'
    /// clampato) al posto del cap originale e' LOSSY. Coi valori reali
    /// (run 300 / complete 120 / 4 turni -> per_attempt 75), una figura
    /// `implement` da 600s otterrebbe `min(75, 150) = 75` invece del corretto
    /// `min(120, 150) = 120`: non un regresso rispetto a oggi, ma meta' del
    /// beneficio buttata via per i run lunghi. Conservando il grezzo, la
    /// ri-derivazione da' lo stesso risultato di una lettura fresca dal DB.
    pub complete_cap: Duration,
    /// Budget dedicato alle richieste DIFFERIBILI, quando qualcuno lo dichiara.
    ///
    /// `None` = NESSUNA dichiarazione, e non «una dichiarazione uguale
    /// all'ordinario»: sono stati diversi e li distingue il tipo (regola Q).
    /// Coincidevano in una prima stesura, e il difetto non era teorico —
    /// [`Self::for_run`] ri-applicava come dichiarazione i budget ordinari del
    /// run PRECEDENTE, cosi' che ri-derivare su un run piu' corto lasciasse in
    /// piedi i numeri di quello lungo. Lo ha trovato
    /// `ri_derivare_senza_db_equivale_a_rileggere_dal_db`, che esisteva gia'.
    pub flex: Option<BudgetFlex>,
}

/// I budget delle richieste DIFFERIBILI (`LlmRequest.deferrable`).
///
/// PERCHE' SONO NUMERI PROPRI. I budget ordinari nascono dalla domanda «quanti
/// turni deve poter completare il run che contiene questa chiamata»; una
/// richiesta differibile non appartiene a nessun run con turni da garantire —
/// e' un titolo di conversazione, un riassunto, una nota — quindi quella
/// domanda per lei non si pone. Il tier differibile dei fornitori (openai
/// `flex`) costa la meta' e in cambio non promette latenza: dimensionarlo su un
/// run lo farebbe scadere prima di servire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetFlex {
    /// Deadline end-to-end (chain e retry inclusi) di una richiesta differibile.
    pub request_budget: Duration,
    /// Cap su un singolo tentativo, GIA' limitato al tetto di trasporto.
    pub per_attempt: Duration,
    /// Il cap per-tentativo CHIESTO, prima del taglio al tetto di trasporto.
    ///
    /// E' un campo e non una variabile locale di [`LlmTimeouts::with_flex`] per
    /// un solo motivo: rendere DERIVABILE il fatto che il taglio ci sia stato
    /// ([`LlmTimeouts::flex_limitato_dal_trasporto`]). Senza, «il budget
    /// differibile e' 900» e «e' 900 ma ne valgono 300» sarebbero lo stesso
    /// stato, e il secondo si scoprirebbe solo cronometrando le chiamate.
    pub per_attempt_chiesto: Duration,
}

impl LlmTimeouts {
    /// Derivazione PURA (niente IO): l'invariante si testa senza DB.
    ///
    /// Garantisce `request_budget * min_turns <= run_timeout`: e' il vincolo che
    /// impedisce a una singola chiamata di consumare l'intero run.
    pub fn derive(
        run_timeout_secs: u64,
        complete_secs: u64,
        stream_secs: u64,
        min_turns: u64,
    ) -> Self {
        let run = run_timeout_secs.max(1);
        let turns = min_turns.max(MIN_TURNS_FLOOR);
        // Divisione intera: arrotonda per DIFETTO, quindi l'invariante
        // budget*turns <= run resta vero anche quando run non e' divisibile.
        let budget = (run / turns).max(1);
        // Il cap per-tentativo puo' solo STRINGERE il budget, mai sforarlo.
        let per_attempt = complete_secs.max(1).min(budget);
        Self {
            run_timeout: Duration::from_secs(run),
            request_budget: Duration::from_secs(budget),
            per_attempt: Duration::from_secs(per_attempt),
            client_budget: Duration::from_secs(budget.saturating_add(CLIENT_BUDGET_MARGIN_SECS)),
            stream_timeout: Duration::from_secs(stream_secs.max(1)),
            min_guaranteed_turns: turns,
            complete_cap: Duration::from_secs(complete_secs.max(1)),
            // Nessun budget differibile DICHIARATO: una richiesta differibile
            // vale quanto le altre. Lo dichiara [`Self::with_flex`], che i soli
            // `resolve` chiamano dopo aver letto il DB.
            flex: None,
        }
    }

    /// Dichiara i budget delle richieste DIFFERIBILI, dai settings (regola G).
    ///
    /// DUE VINCOLI, e il secondo e' quello che conta:
    ///
    /// 1. i budget differibili non possono ACCORCIARE quelli ordinari: un tier
    ///    che serve a dare piu' tempo non puo' toglierne;
    /// 2. il cap per-tentativo non puo' superare il TETTO DI TRASPORTO
    ///    ([`Self::client_http_timeout`]), che e' congelato nel client reqwest
    ///    all'avvio. Una deadline logica piu' lunga del trasporto non allunga
    ///    la chiamata: la fa morire al tetto con un errore di TRASPORTO opaco
    ///    al posto dell'`attempt_timeout` strutturato su cui il motore fa
    ///    failover (regola M) — e' lo stesso vincolo che `request_timeouts`
    ///    dichiara gia' per il verso opposto. Non si alza il tetto per farci
    ///    stare il differibile: quel numero governa anche lo STREAMING, che non
    ///    ha una deadline logica propria, e allungarlo li' sarebbe un effetto
    ///    collaterale che il tier differibile non ha alcun titolo a produrre.
    ///
    /// Il taglio non e' silenzioso: [`Self::flex_limitato_dal_trasporto`] lo
    /// dichiara da un campo, e `resolve` lo mette a WARN all'avvio. Per un
    /// budget differibile davvero piu' lungo si alza `gateway.stream_timeout_seconds`,
    /// che e' il numero da cui il tetto di trasporto discende.
    ///
    /// Il budget END-TO-END invece NON si taglia: comprende la chain e i retry,
    /// cioe' piu' chiamate da al piu' un tetto ciascuna, e limitarlo al tetto
    /// di una sola chiamata negherebbe al differibile proprio il secondo
    /// tentativo per cui il budget esiste.
    pub fn with_flex(mut self, request_budget_secs: u64, per_attempt_secs: u64) -> Self {
        let tetto_trasporto = self.client_http_timeout();
        let request_budget =
            Duration::from_secs(request_budget_secs.max(1)).max(self.request_budget);
        let per_attempt_chiesto =
            Duration::from_secs(per_attempt_secs.max(1)).max(self.per_attempt);
        self.flex = Some(BudgetFlex {
            request_budget,
            per_attempt: per_attempt_chiesto.min(tetto_trasporto).min(request_budget),
            per_attempt_chiesto,
        });
        self
    }

    /// Il cap per-tentativo differibile e' stato accorciato dal tetto di
    /// trasporto? Predicato DERIVATO dai campi, non una stringa nei log: chi
    /// deve allarmarsi (o solo capire perche' una chiamata differibile e'
    /// scaduta prima del previsto) legge questo, non la prosa di un WARN.
    /// Falso quando nessun budget e' dichiarato: li' non c'e' niente da tagliare.
    pub fn flex_limitato_dal_trasporto(&self) -> bool {
        self.flex
            .is_some_and(|f| f.per_attempt_chiesto > f.per_attempt)
    }

    /// Gli stessi timeout, nella forma che vale per una richiesta DIFFERIBILE.
    ///
    /// Proiezione e non derivazione: i budget differibili sono gia' risolti in
    /// [`Self::with_flex`], qui si limitano a prendere il posto di quelli
    /// ordinari. Senza dichiarazione ritorna se stesso: una richiesta
    /// differibile vale quanto le altre, che e' il comportamento di sempre.
    ///
    /// NON rispetta `request_budget * min_turns <= run_timeout`, ed e'
    /// deliberato: quell'invariante protegge un run multi-turno, e una
    /// richiesta differibile non ne ha uno (il chiamante infatti non dichiara
    /// `run_timeout_secs` — e dove lo dichiara, e' il run a vincere: vedi
    /// `request_timeouts` nel gateway).
    pub fn per_flex(&self) -> Self {
        let Some(f) = self.flex else {
            return *self;
        };
        Self {
            request_budget: f.request_budget,
            per_attempt: f.per_attempt,
            client_budget: f
                .request_budget
                .saturating_add(Duration::from_secs(CLIENT_BUDGET_MARGIN_SECS)),
            ..*self
        }
    }

    /// Gli stessi timeout, ri-derivati per un run di durata NOTA. Senza DB.
    ///
    /// E' [`Self::resolve_for_run`] senza IO: serve dove il run reale si scopre
    /// a valle della lettura dei settings — nel gateway, che conosce la durata
    /// solo quando arriva la richiesta, e non puo' interrogare il DB a ogni
    /// chiamata.
    ///
    /// `None` (o zero: nel DB significa "non impostato") lascia i timeout
    /// invariati. Punto unico della scelta: la stessa `run_secs_utile` che usa
    /// `resolve_for_run`.
    pub fn for_run(&self, run_timeout_secs: Option<u64>) -> Self {
        let Some(run) = run_secs_utile(run_timeout_secs) else {
            return *self;
        };
        Self::derive(
            run,
            self.complete_cap.as_secs(),
            self.stream_timeout.as_secs(),
            self.min_guaranteed_turns,
        )
        .con_la_dichiarazione_differibile_di(self)
    }

    /// Ri-applica la dichiarazione differibile di `altro`, che la ri-derivazione
    /// sul run avrebbe perso.
    ///
    /// I budget differibili NON discendono dal run, quindi non si ricalcolano:
    /// si ri-applicano dai valori CHIESTI, gli stessi che `resolve` ha letto dal
    /// DB. Passare i CONCESSI renderebbe la ri-derivazione lossy, come lo
    /// sarebbe passare `per_attempt` al posto di `complete_cap` a
    /// [`Self::derive`]. E un'ASSENZA resta un'assenza: e' l'unico modo perche'
    /// un run piu' corto ottenga i propri budget invece di quelli, ormai
    /// sganciati, del run precedente.
    fn con_la_dichiarazione_differibile_di(self, altro: &Self) -> Self {
        match altro.flex {
            Some(f) => {
                self.with_flex(f.request_budget.as_secs(), f.per_attempt_chiesto.as_secs())
            }
            None => self,
        }
    }

    /// Timeout del client reqwest CONDIVISO fra completion e streaming: deve
    /// coprire il caso piu' lungo (lo streaming), perche' e' un tetto di
    /// trasporto, non il budget applicativo. Le completion sono limitate dalla
    /// deadline logica (`request_budget`/`per_attempt`), non da questo valore.
    pub fn client_http_timeout(&self) -> Duration {
        self.per_attempt.max(self.stream_timeout)
    }

    /// Oltre quanto SILENZIO un run non sta piu' lavorando, ma e' bloccato.
    ///
    /// PERCHE' ESISTE. La rete di sicurezza che avvolge un sub-run era armata
    /// sulla DURATA TOTALE (`tetto_assoluto_s` = timeout x 4 = 1200s), e la
    /// ragione dichiarata era coprire «un run wedged dentro una singola chiamata
    /// al modello, che non raggiunge mai un confine di iterazione». Ma quel caso
    /// non puo' durare 1200s: [`Self::client_budget`] e' il timeout del client
    /// reqwest verso il gateway, quindi una chiamata si interrompe entro quel
    /// tempo e il run TORNA al confine di iterazione, dove il criterio di
    /// progresso decide. La durata totale non proteggeva da cio' per cui era
    /// stata messa — e nel frattempo era LEI a decidere: il 09/08/2026 ha
    /// fermato quattro figure su nove con 4, 5, 17 e 22 passi in corso, cioe'
    /// mentre lavoravano.
    ///
    /// La domanda giusta non e' «da quanto va avanti» ma «da quanto TACE», e la
    /// soglia non si sceglie a mano: la fissa il tempo massimo in cui una
    /// chiamata puo' legittimamente non produrre nulla. Il fattore DUE copre il
    /// turno che spende un budget pieno e ne ricomincia un altro senza aver
    /// ancora registrato un passo: oltre due budget interi senza un solo fatto,
    /// non c'e' lavoro in corso.
    ///
    /// Derivata e non configurabile: un setting a parte sarebbe una seconda
    /// verita' da tenere allineata a `client_budget`, e il giorno in cui
    /// divergessero la soglia mentirebbe con l'aria di una configurazione
    /// (regola G).
    pub fn soglia_silenzio(&self) -> Duration {
        self.client_budget.saturating_mul(2)
    }

    /// Valori di default (nessun DB disponibile): stessa derivazione, stessi
    /// invarianti. Niente numeri magici sparsi nei costruttori.
    pub fn defaults() -> Self {
        Self::derive(
            DEFAULT_RUN_TIMEOUT_SECS,
            DEFAULT_COMPLETE_TIMEOUT_SECS,
            DEFAULT_STREAM_TIMEOUT_SECS,
            DEFAULT_MIN_GUARANTEED_TURNS,
        )
    }

    /// Risolve dal DB (regola G: unica fonte). Ogni chiave mancante o non
    /// parsabile ricade sul proprio default, poi la derivazione riallinea il
    /// tutto: nessuna combinazione di settings puo' violare l'invariante.
    pub async fn resolve(db: &PgPool) -> Self {
        let run = setting_u64(db, KEY_RUN_TIMEOUT, DEFAULT_RUN_TIMEOUT_SECS).await;
        let complete = setting_u64(db, KEY_COMPLETE_TIMEOUT, DEFAULT_COMPLETE_TIMEOUT_SECS).await;
        let stream = setting_u64(db, KEY_STREAM_TIMEOUT, DEFAULT_STREAM_TIMEOUT_SECS).await;
        let turns = setting_u64(db, KEY_MIN_GUARANTEED_TURNS, DEFAULT_MIN_GUARANTEED_TURNS).await;
        let flex_budget =
            setting_u64(db, KEY_FLEX_REQUEST_BUDGET, DEFAULT_FLEX_REQUEST_BUDGET_SECS).await;
        let flex_attempt =
            setting_u64(db, KEY_FLEX_PER_ATTEMPT, DEFAULT_FLEX_PER_ATTEMPT_SECS).await;
        let t = Self::derive(run, complete, stream, turns).with_flex(flex_budget, flex_attempt);
        if let (true, Some(f)) = (t.flex_limitato_dal_trasporto(), t.flex) {
            tracing::warn!(
                chiesto_s = f.per_attempt_chiesto.as_secs(),
                concesso_s = f.per_attempt.as_secs(),
                tetto_trasporto_s = t.client_http_timeout().as_secs(),
                chiave = KEY_FLEX_PER_ATTEMPT,
                "timeout: il cap per-tentativo differibile e' piu' lungo del tetto di \
                 trasporto e vale quest'ultimo. Per allungarlo davvero si alza \
                 gateway.stream_timeout_seconds, da cui il tetto discende"
            );
        }
        t
    }

    /// Come [`resolve`], ma per un run di durata NOTA.
    ///
    /// L'invariante `request_budget * min_turns <= run_timeout` vale solo
    /// rispetto al run su cui e' stata calcolata. `resolve` usa il default
    /// globale (`orchestrator.subagent_default_timeout_s`, 300s), ma le figure
    /// hanno il PROPRIO `nexus_subagent_definitions.timeout_s`: `review` ne ha
    /// 240, `implement` 600. Con `min_turns = 4` il budget derivato dal globale
    /// e' 75s, quindi a un `review` venivano promessi 4 turni da 75s = 300s
    /// dentro un run che ne dura 240: l'invariante era verificata contro un run
    /// che nessuna figura possiede davvero, e la figura veniva uccisa dal
    /// cronometro credendo di avere ancora turni a disposizione.
    ///
    /// Passando qui la durata reale, i turni garantiti tornano a essere una
    /// promessa mantenibile (240/4 = 60s per turno). NON allunga nulla: stringe
    /// il budget della singola chiamata quando il run e' piu' corto del default.
    pub async fn resolve_for_run(db: &PgPool, run_timeout_secs: Option<u64>) -> Self {
        Self::resolve(db).await.for_run(run_timeout_secs)
    }
}

async fn setting_u64(db: &PgPool, key: &str, default: u64) -> u64 {
    crate::get_setting(db, key)
        .await
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

/// La durata di run da usare, quando e' NOTA. `None` significa "non la so,
/// chiedila al DB": e' la sola porta d'ingresso del default globale.
///
/// Estratta perche' la SCELTA della sorgente e' il punto in cui il difetto
/// vive, e dentro `resolve_for_run` sarebbe verificabile solo con un DB —
/// cioe' mai (regola O).
pub fn run_secs_utile(run_timeout_secs: Option<u64>) -> Option<u64> {
    run_timeout_secs.filter(|&s| s > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L'INVARIANTE. Questo test e' la ragione per cui il modulo esiste: se
    /// qualcuno reintroduce un budget >= al run, qui diventa rosso.
    #[test]
    fn una_chiamata_non_puo_mangiarsi_il_run() {
        for run in [60_u64, 120, 300, 301, 599, 900] {
            for turns in [0_u64, 1, 2, 3, 4, 7, 10] {
                for complete in [1_u64, 30, 120, 500] {
                    let t = LlmTimeouts::derive(run, complete, 300, turns);
                    let budget = t.request_budget.as_secs();
                    let effective_turns = t.min_guaranteed_turns;
                    assert!(
                        budget * effective_turns <= t.run_timeout.as_secs(),
                        "budget {budget}s x {effective_turns} turni sfora il run \
                         {run}s (complete={complete})"
                    );
                    assert!(
                        t.per_attempt <= t.request_budget,
                        "il cap per-tentativo non puo' superare il budget della richiesta"
                    );
                }
            }
        }
    }

    /// La regressione storica, in numeri: coi valori LIVE del DB il budget di
    /// una chiamata era 300s contro un run di 300s (it=0 garantito).
    #[test]
    fn i_valori_storici_non_producono_piu_budget_pari_al_run() {
        let t = LlmTimeouts::derive(300, 120, 300, DEFAULT_MIN_GUARANTEED_TURNS);
        assert_eq!(t.request_budget, Duration::from_secs(75));
        assert_eq!(t.per_attempt, Duration::from_secs(75));
        assert_eq!(t.client_budget, Duration::from_secs(90));
        // Il punto: il client non attende piu' del run che lo contiene (era 435 > 300).
        assert!(t.client_budget < t.run_timeout);
    }

    /// `complete_timeout_seconds` deve poter STRINGERE (prima era inerte: il
    /// `max(120, 300)` la scartava sempre).
    #[test]
    fn il_cap_per_tentativo_e_efficace_solo_in_restrizione() {
        let stretto = LlmTimeouts::derive(300, 30, 300, 4);
        assert_eq!(stretto.per_attempt, Duration::from_secs(30), "deve stringere");
        let largo = LlmTimeouts::derive(300, 1000, 300, 4);
        assert_eq!(
            largo.per_attempt,
            largo.request_budget,
            "non puo' sforare il budget"
        );
    }

    /// La soglia di silenzio sta SOPRA il tempo in cui una chiamata puo'
    /// legittimamente tacere e SOTTO il tetto storico che sostituisce: sotto il
    /// primo ucciderebbe chi aspetta una risposta legittima, sopra il secondo
    /// non sarebbe un miglioramento ma un tetto piu' largo con un altro nome.
    #[test]
    fn la_soglia_di_silenzio_sta_fra_una_chiamata_e_il_tetto_storico() {
        let t = LlmTimeouts::derive(300, 120, 300, DEFAULT_MIN_GUARANTEED_TURNS);
        let silenzio = t.soglia_silenzio();
        assert!(
            silenzio > t.client_budget,
            "una chiamata in volo non e' silenzio: {silenzio:?} deve superare {:?}",
            t.client_budget
        );
        // Il tetto storico era timeout x 4 = 1200s con questi valori.
        assert!(
            silenzio < Duration::from_secs(1200),
            "una soglia >= al tetto storico non cambierebbe nulla: {silenzio:?}"
        );
        assert_eq!(silenzio, Duration::from_secs(180));
    }

    /// La soglia SEGUE `client_budget` invece di essere un numero proprio: con
    /// una figura piu' lunga il budget cresce e la soglia con lui. E' cio' che
    /// le impedisce di diventare una seconda verita' da riallineare a mano.
    #[test]
    fn la_soglia_di_silenzio_segue_il_budget_e_non_e_un_numero_proprio() {
        let corta = LlmTimeouts::derive(300, 120, 300, 4);
        let lunga = LlmTimeouts::derive(1200, 120, 300, 4);
        assert!(lunga.client_budget > corta.client_budget, "premessa del test");
        assert!(
            lunga.soglia_silenzio() > corta.soglia_silenzio(),
            "la soglia deve muoversi col budget, non restare ferma"
        );
    }

    /// Il client HTTP e' condiviso: deve coprire lo streaming, che e' piu' lungo
    /// del cap per-tentativo delle completion.
    #[test]
    fn il_client_condiviso_copre_lo_streaming() {
        let t = LlmTimeouts::derive(300, 120, 300, 4);
        assert_eq!(t.client_http_timeout(), Duration::from_secs(300));
    }

    /// Il difetto che `resolve_for_run` chiude: l'invariante veniva verificata
    /// contro un run che la figura non possiede.
    ///
    /// `orchestrator.subagent_default_timeout_s` vale 300, ma la figura `review`
    /// ha `timeout_s = 240` in `nexus_subagent_definitions`. Derivando dal
    /// default, a un review venivano promessi 4 turni da 75s = 300s dentro un
    /// cronometro che scade a 240: il quarto turno non esisteva, e la figura
    /// veniva uccisa mentre credeva di avere ancora budget. Con la durata reale
    /// i turni tornano a essere una promessa mantenibile.
    #[test]
    fn il_budget_deve_nascere_dal_run_reale_non_dal_default_globale() {
        let review_reale = 240_u64;

        let dal_default = LlmTimeouts::derive(
            DEFAULT_RUN_TIMEOUT_SECS,
            120,
            300,
            DEFAULT_MIN_GUARANTEED_TURNS,
        );
        assert!(
            dal_default.request_budget.as_secs() * dal_default.min_guaranteed_turns > review_reale,
            "premessa del difetto: il budget derivato dal default sfora il run \
             vero della figura review"
        );

        let dal_reale =
            LlmTimeouts::derive(review_reale, 120, 300, DEFAULT_MIN_GUARANTEED_TURNS);
        assert_eq!(dal_reale.request_budget, Duration::from_secs(60));
        assert!(
            dal_reale.request_budget.as_secs() * dal_reale.min_guaranteed_turns <= review_reale,
            "coi 240s reali i turni garantiti devono starci dentro"
        );
        assert!(
            dal_reale.client_budget < Duration::from_secs(review_reale),
            "nemmeno l'attesa del client puo' superare il run della figura"
        );
    }

    /// La durata nota vince sul default; solo l'assenza (o uno zero, che nel DB
    /// significa "non impostato") lascia parlare il setting globale.
    #[test]
    fn la_durata_nota_vince_sul_default_globale() {
        assert_eq!(run_secs_utile(Some(240)), Some(240));
        assert_eq!(
            run_secs_utile(Some(0)),
            None,
            "timeout_s = 0 e' 'non impostato', non 'run istantaneo'"
        );
        assert_eq!(run_secs_utile(None), None);
    }

    /// `for_run` deve dare lo STESSO risultato di una lettura fresca dal DB.
    ///
    /// E' il punto in cui la ri-derivazione poteva perdere informazione: se
    /// `LlmTimeouts` non conservasse `complete_cap`, l'unico cap disponibile
    /// sarebbe `per_attempt`, GIA' clampato al budget del run precedente. Coi
    /// valori reali (300/120/4 -> per_attempt 75), una figura `implement` da
    /// 600s otterrebbe 75 invece di 120: non un regresso, ma meta' del
    /// beneficio buttata via -- e in silenzio.
    ///
    /// Mutazione che rende rosso: in `for_run` passare `self.per_attempt`
    /// invece di `self.complete_cap`.
    #[test]
    fn ri_derivare_senza_db_equivale_a_rileggere_dal_db() {
        let complete = 120_u64;
        for run_reale in [60_u64, 240, 300, 600, 900] {
            let dal_db_fresco = LlmTimeouts::derive(run_reale, complete, 300, 4);
            let ri_derivato = LlmTimeouts::derive(DEFAULT_RUN_TIMEOUT_SECS, complete, 300, 4)
                .for_run(Some(run_reale));
            assert_eq!(
                ri_derivato, dal_db_fresco,
                "run {run_reale}: ri-derivare non deve perdere il cap grezzo"
            );
        }
    }

    /// Durata sconosciuta: i timeout restano quelli del default globale.
    #[test]
    fn senza_durata_nota_for_run_non_tocca_nulla() {
        let base = LlmTimeouts::derive(300, 120, 300, 4);
        assert_eq!(base.for_run(None), base);
        assert_eq!(base.for_run(Some(0)), base, "0 = non impostato");
    }

    /// Un run piu' LUNGO del default non deve essere stretto dal default: la
    /// figura `implement` ha `timeout_s = 600` e i suoi turni valgono 150s.
    #[test]
    fn un_run_piu_lungo_del_default_ottiene_il_suo_budget() {
        let t = LlmTimeouts::derive(600, 1000, 300, DEFAULT_MIN_GUARANTEED_TURNS);
        assert_eq!(t.request_budget, Duration::from_secs(150));
    }

    /// Senza dichiarazione, una richiesta differibile vale quanto le altre: il
    /// meccanismo resta SPENTO finche' il DB non lo dichiara (regola G).
    #[test]
    fn senza_dichiarazione_il_differibile_non_esiste() {
        let t = LlmTimeouts::derive(300, 120, 300, 4);
        assert_eq!(t.flex, None, "l'assenza e' uno stato, non un valore che coincide");
        assert_eq!(t.per_flex(), t, "nessun budget dichiarato = nessun budget diverso");
        assert!(!t.flex_limitato_dal_trasporto());
    }

    /// IL vincolo del lotto: il cap per-tentativo differibile non puo' superare
    /// il TETTO DI TRASPORTO, congelato nel client reqwest all'avvio. Una
    /// deadline logica oltre il tetto non allunga la chiamata, la fa morire con
    /// un errore di trasporto opaco al posto dell'`attempt_timeout` strutturato
    /// (regola M).
    ///
    /// Coi valori LIVE il taglio C'E': tetto 300s contro i 900s che la doc del
    /// tier differibile suggerisce. Il meccanismo funziona, ma non per il tempo
    /// che si crede, e il predicato lo DICHIARA invece di lasciarlo scoprire
    /// cronometrando (regola Q).
    ///
    /// MUTAZIONE: togliere `.min(tetto_trasporto)` da `with_flex` -> rosso qui.
    #[test]
    fn il_cap_differibile_non_supera_il_tetto_di_trasporto() {
        let t = LlmTimeouts::derive(300, 120, 300, 4).with_flex(900, 900);
        assert_eq!(t.client_http_timeout(), Duration::from_secs(300), "premessa");
        let f = t.flex.expect("dichiarato");
        assert_eq!(f.per_attempt, Duration::from_secs(300), "tagliato al tetto");
        assert_eq!(f.per_attempt_chiesto, Duration::from_secs(900));
        assert!(
            t.flex_limitato_dal_trasporto(),
            "il taglio dev'essere leggibile da un campo, non solo dai log"
        );
        // Il budget END-TO-END non si taglia: comprende chain e retry, cioe'
        // piu' chiamate da al piu' un tetto ciascuna.
        assert_eq!(f.request_budget, Duration::from_secs(900));
        assert_eq!(t.per_flex().request_budget, Duration::from_secs(900));
        assert_eq!(t.per_flex().per_attempt, Duration::from_secs(300));
    }

    /// Alzare lo streaming alza il tetto, e con lui il cap concesso: e' la
    /// strada che il WARN indica all'operatore, e deve funzionare davvero.
    #[test]
    fn alzando_il_tetto_il_differibile_ottiene_cio_che_chiede() {
        let t = LlmTimeouts::derive(300, 120, 900, 4).with_flex(900, 900);
        assert_eq!(t.flex.expect("dichiarato").per_attempt, Duration::from_secs(900));
        assert!(!t.flex_limitato_dal_trasporto());
    }

    /// Il tier differibile serve a dare piu' tempo: non puo' toglierne. Un
    /// setting piu' stretto degli ordinari non li accorcia.
    #[test]
    fn il_differibile_non_puo_accorciare_gli_ordinari() {
        let t = LlmTimeouts::derive(300, 120, 300, 4).with_flex(10, 10);
        let f = t.flex.expect("dichiarato");
        assert_eq!(f.request_budget, t.request_budget);
        assert_eq!(f.per_attempt, t.per_attempt);
        assert_eq!(t.per_flex(), t, "dichiarare meno non cambia niente");
    }

    /// La dichiarazione differibile non discende dal run e non deve sparire
    /// ri-derivando su un run noto.
    ///
    /// MUTAZIONE: togliere il `.with_flex(...)` da `for_run` -> rosso.
    #[test]
    fn ri_derivare_su_un_run_noto_conserva_il_differibile() {
        let t = LlmTimeouts::derive(300, 120, 300, 4).with_flex(900, 900);
        for run in [60_u64, 240, 600, 900] {
            let f = t.for_run(Some(run)).flex.expect("la dichiarazione sopravvive");
            assert_eq!(
                f.request_budget,
                Duration::from_secs(900),
                "run {run}: il budget differibile e' un numero proprio"
            );
            assert_eq!(f.per_attempt_chiesto, Duration::from_secs(900));
        }
    }

    #[test]
    fn turni_sotto_il_pavimento_sono_clampati() {
        assert_eq!(LlmTimeouts::derive(300, 120, 300, 0).min_guaranteed_turns, 2);
        assert_eq!(LlmTimeouts::derive(300, 120, 300, 1).min_guaranteed_turns, 2);
    }
}
