//! `progress_controller`: punto unico (regola L) del controllo di avanzamento
//! del ciclo agentico. Porting 1:1 di `brain/agents/progress_controller.py`.
//!
//! Centralizza UNA sola domanda: "di fronte a uno stallo, qual e' la prossima
//! mossa?". La risposta segue SEMPRE la stessa gerarchia, identica per ogni asse
//! di stallo:
//!   1. GUIDE          -> forza-azione guidata (rimuovi read-only + tool_choice
//!                        required) + nudge assertivo.
//!   2. FORCE_DIAGNOSE -> stadio intermedio (solo asse repeated_action) prima di
//!                        escalation/abort.
//!   3. ESCALATE       -> promuovi il turno a un modello piu' capace.
//!   4. ABORT          -> guida+escalation esaurite -> chiusura con verifica E2E.
//!
//! La funzione [`decide`] e' PURA (nessun IO, nessuna lettura DB): i segnali
//! arrivano gia' risolti dal chiamante, identica a `should_force_tool_choice`.
//! Cosi' resta deterministica e testabile in isolamento.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// Assi di stallo riconosciuti. Stringhe stabili (vedi serde rename): usate come
/// chiavi negli insiemi di stato ("assi gia' guidati") e nei log/meta_step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Axis {
    #[serde(rename = "exploration")]
    Exploration,
    #[serde(rename = "signature")]
    Signature,
    #[serde(rename = "g1_descriptive")]
    G1Descriptive,
    #[serde(rename = "repeated_action")]
    RepeatedAction,
    #[serde(rename = "resource_reallocation")]
    ResourceReallocation,
    /// Stessa domanda-di-chiarimento ripetuta all'utente attraverso PIU' run
    /// della sessione (cross-run, dal detector `ClarifyHistoryPort`). E' l'asse
    /// che il loop email (chat Beaty-Book) attraversava senza mai essere
    /// rilevato: la loop-detection copriva solo le firme di TOOL, non le domande
    /// ripetute all'utente. Popolato all'avvio del run dai `nexus_agent_meta_steps`
    /// kind='clarify' della sessione (regola M: la DECISIONE deriva dal segnale
    /// strutturato `declared_outcome=needs_input`, la firma-testo e' solo euristica).
    #[serde(rename = "repeated_user_question")]
    RepeatedUserQuestion,
}

impl Axis {
    /// Stringa stabile dell'asse (chiave per gli insiemi `already_*`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Axis::Exploration => "exploration",
            Axis::Signature => "signature",
            Axis::G1Descriptive => "g1_descriptive",
            Axis::RepeatedAction => "repeated_action",
            Axis::ResourceReallocation => "resource_reallocation",
            Axis::RepeatedUserQuestion => "repeated_user_question",
        }
    }
}

/// Azioni possibili, in ordine di severita' crescente.
///
/// `force_diagnose`: stadio intermedio per l'asse repeated_action (dopo GUIDE,
/// prima di ESCALATE/ABORT). Obbliga l'agente a leggere l'errore, dichiarare la
/// causa radice e cambiare azione (vedi mig 0386).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    #[serde(rename = "proceed")]
    Proceed,
    #[serde(rename = "guide")]
    Guide,
    /// Cambio di STRATEGIA forzato (solo asse repeated_action, dopo
    /// guide+diagnose e PRIMA dell'escalation di modello): il modello corrente
    /// riceve l'ordine di abbandonare l'approccio che si ripete e provarne uno
    /// ALTERNATIVO (strumento diverso / piu' contesto / passo piu' piccolo).
    /// E' il comportamento standard di un agente capace: davanti a uno stallo
    /// prima si cambia strada, poi — solo se serve — si cambia cavallo.
    ChangeStrategy,
    #[serde(rename = "force_diagnose")]
    ForceDiagnose,
    #[serde(rename = "escalate")]
    Escalate,
    #[serde(rename = "abort")]
    Abort,
    /// Poni all'utente UNA domanda mirata e chiudi il turno con esito STRUTTURATO
    /// `needs_input` (ADR 0034 `task_complete`). Prodotta SOLO dal meta-reasoner
    /// (`RecoveryMove::AskUser`), MAI da [`decide`]: la gerarchia fissa non
    /// interroga l'utente. Un cap strutturale per-sessione (`already_asked_user`)
    /// a monte impedisce la ri-domanda infinita che ha condannato il loop email.
    #[serde(rename = "ask_user")]
    AskUser,
    /// Dichiara il blocco in modo ONESTO e strutturato (`task_complete{blocked}`,
    /// ADR 0034), con `blocker` dal vocabolario ADR 0034. Prodotta SOLO dal
    /// meta-reasoner (`RecoveryMove::DeclareBlocked`): chiude il run senza lasciare
    /// al modello un turno libero (evita la fabbricazione di dati).
    #[serde(rename = "declare_blocked")]
    DeclareBlocked,
}

/// stop_reason UNICO emesso da un abort coordinato. `route_after_executor` lo
/// instrada alla verifica E2E (final_gate), non al learner morto. Tenuto distinto
/// dai legacy "loop_detected"/"g1_cap_reached".
pub const ABORT_STOP_REASON: &str = "loop_abort";

/// Segnali grezzi del turno corrente, raccolti dall'executor.
///
/// Tutti con default neutri: il chiamante popola solo quelli che conosce nel
/// punto in cui chiama (pre-LLM vs post-LLM).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressSignals {
    /// Esplorazione: chiamate consecutive di sola lettura.
    pub exploration_count: i64,
    /// Soglia esplorazione.
    pub exploration_threshold: i64,
    /// Signature-loop: nome del tool ripetuto identico (None = nessun loop).
    pub signature_loop_tool: Option<String>,
    /// Domande-chiarimento IDENTICHE gia' poste all'utente nella SESSIONE
    /// (CROSS-RUN), dal detector `ClarifyHistoryPort` (via
    /// `AgentState::repeated_clarify_count`). Regola M: conta i turni precedenti
    /// che hanno gia' posto la STESSA domanda (segnale strutturato: i meta_step
    /// `kind='clarify'` della sessione). `0` su un run normale.
    pub repeated_user_question_count: i64,
    /// Soglia (`agent.loop.repeated_user_question_threshold`, mig 0510, default 2)
    /// oltre la quale (`>=`) scatta l'asse `RepeatedUserQuestion`. Regola G:
    /// arriva dal DB, mai hardcoded. Default conservativo 2: con la soglia attuale
    /// e nessuna storia l'asse non scatta mai -> comportamento invariato.
    pub repeated_user_question_threshold: i64,
    /// G1 descrittivo: il modello descrive senza agire (reroute_count >= max).
    pub g1_over_cap: bool,
    /// Azione produttiva ripetuta identica oltre soglia: (label, conteggio).
    pub repeated_action: Option<(String, i64)>,
    /// `true` se l'azione ripetuta e' un `edit_file`/`write_file` FALLITO (es.
    /// `old_string` non corrispondente al file reale). Cambia il nudge da
    /// generico ("cambia approccio") a SPECIFICO e attuabile: "copia l'old_string
    /// ESATTO dall'estratto numerato gia' presente nell'errore qui sopra". Causa
    /// radice del falso-stallo: la correzione dell'old_string e' un'azione
    /// LEGITTIMA, non una ripetizione da abortire.
    pub repeated_action_edit_failed: bool,
    /// `true` se l'azione ripetuta e' un tool di SOLA LETTURA (read_file/list_files/
    /// grep & co.). La GUIDE NON deve forzare un'altra tool call (forzerebbe un
    /// ennesimo read-only -> nuovo loop): deve guidare a CONCLUDERE con testo (il
    /// contesto e' gia' stato raccolto). NON-convergenza, regola H. Per i tool
    /// produttivi resta la forza-azione correttiva (force_action=true).
    pub repeated_action_read_only: bool,
    /// `true` se l'azione ripetuta e' l'avvio di un SERVIZIO long-running
    /// (`run_service`/`service_restart`) FALLITO: il servizio parte e muore subito
    /// (porta occupata, config errata, dipendenza mancante), l'agente lo rilancia
    /// IDENTICO e scatta il falso-stallo. Rilanciare un dev server che muore NON e'
    /// un'azione produttiva: la causa va DIAGNOSTICATA leggendo l'output del servizio.
    /// Cambia il nudge in SPECIFICO ("leggi i log del servizio, correggi la causa,
    /// non rilanciare") e NON forza l'azione (force_action=false), cosi' i tool di
    /// lettura log (read_service_output/tail_service_logs, read-only) restano
    /// disponibili invece di essere rimossi dalla forza-azione; inoltre evita l'ABORT
    /// (come per l'edit fallito) finche' la diagnosi non e' stata sfruttata: l'agente
    /// NON deve arrendersi su un servizio che non parte, deve capire perche'.
    pub repeated_action_service_failed: bool,
    /// `true` se l'azione ripetuta e' FALLITA per SEGNALE STRUTTURATO (exit_code
    /// != 0 / `is_error` del tool_result), a prescindere dal tipo di tool (regola
    /// M: lo stato tecnico si legge dal segnale, non dal testo). Generalizza
    /// `edit_failed`/`service_failed` a QUALSIASI azione ripetuta che fallisce
    /// davvero (es. un `run_command` di health-check `curl` con exit 7): un
    /// fallimento reale ripetuto NON e' un loop da abortire, e' una causa radice
    /// da diagnosticare. Instrada a FORCE_DIAGNOSE (guida alla causa), mai
    /// all'ABORT "il modello non riesce". Se `edit_failed`/`service_failed` sono
    /// gia' true, i loro nudge SPECIFICI hanno precedenza; questo copre il resto.
    pub repeated_action_failed: bool,
    /// `true` se il turno corrente e' ACTION-ORIENTED (task di modifica/fix), come
    /// derivato da `turn_action_oriented(state.action_oriented)`. Biforca il nudge
    /// del ramo read-only: su un task di fix una lettura ripetuta NON va chiusa con
    /// testo (l'agente rinuncerebbe a 0 file modificati), va invece orientata all'
    /// EDIT (il contesto e' gia' stato letto -> applica la correzione). Su un task
    /// informativo (action_oriented=false) resta il nudge "concludi con testo".
    /// Default `true` (conservativo, identico a `turn_action_oriented(None)`).
    pub action_oriented: bool,
    /// Riallocazione risorse: numero request_port ravvicinate (a prescindere dal
    /// label: il variare del label E' il sintomo del loop).
    pub reallocation_count: i64,
    /// Soglia riallocazione.
    pub reallocation_threshold: i64,
    /// `true` se il run ha gia' allocazioni/servizi attivi noti.
    pub has_active_resources: bool,
    /// Budget di escalation gia' consumato.
    pub escalations: i64,
    /// Budget massimo di escalation.
    pub max_escalations: i64,
    /// `true` se c'e' un candidato di escalation disponibile.
    pub has_escalation_candidate: bool,
    /// Assi per cui la forza-azione (GUIDE) e' GIA' stata applicata.
    pub already_guided: HashSet<String>,
    /// Assi per cui la DIAGNOSI FORZATA (force_diagnose) e' GIA' stata applicata.
    pub already_diagnosed: HashSet<String>,
    /// Assi per cui il CAMBIO DI STRATEGIA forzato e' GIA' stato applicato.
    pub already_strategy_shifted: HashSet<String>,
    /// `true` abilita lo stadio intermedio force_diagnose per repeated_action.
    pub force_diagnose_enabled: bool,
}

impl Default for ProgressSignals {
    fn default() -> Self {
        // Default neutri identici ai default Python (ProgressSignals dataclass).
        Self {
            exploration_count: 0,
            exploration_threshold: 6,
            signature_loop_tool: None,
            // Default neutri: 0 occorrenze, soglia 2 (= seed mig 0510). Con nessuna
            // storia l'asse RepeatedUserQuestion non scatta mai (0 < 2) ->
            // comportamento invariato sui run normali.
            repeated_user_question_count: 0,
            repeated_user_question_threshold: 2,
            g1_over_cap: false,
            repeated_action: None,
            repeated_action_edit_failed: false,
            repeated_action_read_only: false,
            repeated_action_service_failed: false,
            repeated_action_failed: false,
            // Conservativo: identico a `turn_action_oriented(None) == true`.
            action_oriented: true,
            reallocation_count: 0,
            reallocation_threshold: 3,
            has_active_resources: false,
            escalations: 0,
            max_escalations: 3,
            has_escalation_candidate: false,
            already_guided: HashSet::new(),
            already_diagnosed: HashSet::new(),
            already_strategy_shifted: HashSet::new(),
            force_diagnose_enabled: false,
        }
    }
}

/// Esito della decisione. Il chiamante lo APPLICA (inietta nudge, rimuove tool,
/// escala, fa il return di abort); la LOGICA vive qui.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressDecision {
    pub action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub axis: Option<Axis>,
    /// GUIDE: rimuovi i tool di sola lettura e obbliga tool_choice required.
    pub force_action: bool,
    /// Testo del nudge assertivo da iniettare (None se non serve).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nudge_text: Option<String>,
    /// Solo per ABORT: lo stop_reason coordinato (instradato a final_gate).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Spiegazione breve per log/meta_step (perche' questa mossa).
    pub reason: String,
}

/// Direttiva porte CONDIZIONALE (grounding). Vedi `_port_directive` Python.
fn port_directive(has_active_resources: bool) -> &'static str {
    if has_active_resources {
        "per le porte NON allocarne di nuove: i servizi del progetto sono gia' \
attivi (vedi blocco RISORSE PROGETTO), riusa o riavvia quelli esistenti \
invece di chiederne di nuove"
    } else {
        "request_port SOLO per un servizio NUOVO (non ancora in ascolto)"
    }
}

/// Nudge ASSERTIVO per l'esplorazione. Vedi `_exploration_nudge` Python.
fn exploration_nudge(count: i64, has_active_resources: bool) -> String {
    format!(
        "STOP esplorazione: hai gia' letto/cercato {count} volte di fila senza \
produrre nulla. I tool di sola lettura sono ora DISABILITATI per questo \
turno: l'unico modo di avanzare e' agire sul progetto (modifica file o \
comando di esecuzione/verifica; {}) \
oppure, se la richiesta era una domanda, rispondere subito a parole con il \
risultato.",
        port_directive(has_active_resources)
    )
}

/// Nudge GROUNDED per il loop di riallocazione porte. Vedi
/// `_resource_reallocation_nudge` Python.
fn resource_reallocation_nudge(count: i64) -> String {
    format!(
        "STOP: hai gia' richiesto porte {count} volte di fila. I servizi del \
progetto sono gia' attivi: NON riallocare e NON variare il label per \
ottenere una porta nuova (request_port e' idempotente per scopo e ti \
ridarebbe comunque la porta esistente). Usa il blocco RISORSE PROGETTO nel \
contesto: riusa la porta del servizio del tuo scopo se e' attivo, riavvialo \
se e' allocato ma spento. Chiama request_port SOLO per un servizio NUOVO \
che non e' ancora in ascolto."
    )
}

/// Nudge per il loop su tool identico ripetuto. Vedi `_signature_nudge` Python.
fn signature_nudge(tool: &str) -> String {
    format!(
        "STOP: hai ripetuto la stessa tool call ('{tool}', stesso input) senza \
progresso. NON ripeterla. Se ti mancano informazioni fai una richiesta \
diversa e piu' specifica; altrimenti procedi con l'azione concreta \
successiva o riassumi lo stato a parole."
    )
}

/// Nudge per la risposta descrittiva su richiesta d'azione (G1). Vedi `_g1_nudge`.
fn g1_nudge() -> String {
    "STOP: hai descritto i passi senza eseguirli. I tool di sola lettura sono \
disabilitati per questo turno: avanza eseguendo il prossimo step concreto \
con una tool call che modifica il progetto o lancia un comando."
        .to_string()
}

/// Nudge dell'asse `RepeatedUserQuestion` (loop clarification cross-run): la
/// stessa domanda e' gia' stata posta all'utente in un turno precedente della
/// sessione. Ri-chiederla e' inutile (l'incidente email: l'input veniva
/// ri-oscurato ad ogni giro). Guida a USARE il valore gia' fornito senza
/// ri-chiederlo, oppure a dichiarare il blocco in modo strutturato.
fn repeated_user_question_nudge() -> String {
    "STOP: hai gia' posto questa stessa domanda all'utente in un turno precedente \
e hai gia' ricevuto risposta. NON ri-chiedere lo stesso dato: usa il valore gia' \
fornito cosi' com'e' (anche se appare oscurato/placeholder, trattalo come opaco e \
passalo al tool che lo consuma senza interpretarlo), oppure, se davvero manca un \
dato indispensabile e diverso, dichiara il blocco con task_complete."
        .to_string()
}

/// Vocabolario build/test per il nudge build-aware. Vedi `_BUILD_TEST_LABEL_KEYWORDS`.
const BUILD_TEST_LABEL_KEYWORDS: &[&str] = &[
    "build",
    "tsc",
    "compile",
    "cargo check",
    "cargo build",
    "cargo test",
    "npm run",
    "npm test",
    "pnpm",
    "yarn",
    "lint",
    "eslint",
    "make",
    "pytest",
    "run_tests",
    "test",
    "gradle",
    "mvn",
    "go build",
    "go test",
];

/// True se il label del comando ripetuto e' un build/compilazione/test. Vedi
/// `_is_build_or_test_label` Python.
fn is_build_or_test_label(label: &str) -> bool {
    let l = label.to_lowercase();
    BUILD_TEST_LABEL_KEYWORDS.iter().any(|k| l.contains(k))
}

/// Nudge per la ripetizione identica di una azione produttiva (build/test-aware).
/// Nudge del CAMBIO DI STRATEGIA (livello 1.9, solo repeated_action): impone
/// al modello CORRENTE di abbandonare l'approccio che si ripete e provarne uno
/// alternativo, restando sul task. Ordinato per efficacia: strumento diverso,
/// piu' contesto, decomposizione. E' l'equivalente runtime della direttiva
/// permanente `<anti_loop>` dei system prompt (mig 0506).
fn strategy_shift_nudge(label: &str, count: i64) -> String {
    format!(
        "Hai gia' provato '{label}' {count} volte senza che l'esito cambiasse: correggere \
non basta piu', CAMBIA STRATEGIA restando su questo task. Scegli ORA una strada DIVERSA: \
(a) strumento alternativo — es. write_file col contenuto completo invece di edit_file, un \
comando diverso che ottiene lo stesso effetto; (b) piu' contesto — leggi il file completo, \
i log o l'errore per intero prima di riprovare; (c) decomposizione — fai UN passo piu' \
piccolo e verificabile del problema. NON ripetere '{label}' identico. Se ogni strada e' \
davvero impedita da una causa esterna (credenziale, permesso, servizio, dipendenza), \
dichiara l'esito con task_complete (outcome=blocked + blocker)."
    )
}

/// Vedi `_repeated_action_nudge` Python.
fn repeated_action_nudge(label: &str, count: i64) -> String {
    if is_build_or_test_label(label) {
        format!(
            "STOP: hai gia' eseguito '{label}' {count} volte. Ri-eseguire un \
build/test NON riduce gli errori: li riduce solo correggere i file \
che l'output segnala (ogni errore ha file:riga; in fondo il totale, \
es. 'Found N errors'). Lavora sulla causa, non sulla ripetizione del \
comando. Se l'output era troncato e non vedi tutti gli errori, \
correggi quelli visibili e segnala che ne mancano, invece di \
ri-eseguire per scoprirli."
        )
    } else {
        format!(
            "STOP: hai gia' eseguito la stessa azione ({label}) {count} volte. \
Ripeterla identica NON cambia il risultato. NON ripeterla: leggi \
l'esito dell'esecuzione precedente, e poi (a) se l'azione e' riuscita, \
PROCEDI al passo successivo o concludi verificando il risultato reale; \
(b) se e' fallita, cambia approccio (causa radice diversa), non rieseguire \
lo stesso comando/edit."
        )
    }
}

/// Nudge per la ripetizione identica di una LETTURA (read_file/list_files/grep)
/// su un turno INFORMATIVO (action_oriented=false): il contesto e' gia' stato
/// raccolto, ripetere la stessa lettura non aggiunge nulla. Guida a CONCLUDERE con
/// testo (NON a un'altra tool call). NON-convergenza, regola H.
fn repeated_read_only_nudge(label: &str, count: i64) -> String {
    format!(
        "STOP: hai gia' eseguito la stessa lettura ({label}) {count} volte con lo \
stesso bersaglio. Ripeterla NON aggiunge informazioni: il contenuto e' gia' nel \
contesto sopra. NON rileggere e NON cercare altro. Rispondi ORA a parole con il \
risultato richiesto in base a cio' che hai gia' raccolto; se ti serve davvero un \
dato diverso, fai UNA lettura su un bersaglio DIVERSO, non la stessa."
    )
}

/// Nudge per la lettura ripetuta su un turno ACTION-ORIENTED (task operativo):
/// l'agente ha gia' ispezionato il bersaglio, ripeterne la lettura non avvicina al
/// risultato. Diversamente dal ramo informativo, qui la chiusura con testo sarebbe
/// una RINUNCIA (0 azioni su un task operativo): il nudge orienta all'AZIONE concreta.
/// L'azione risolutiva NON e' sempre un edit: molti task si chiudono con `run_command`
/// (installare una dipendenza, avviare/riavviare un servizio, lanciare build/test/
/// migrazioni). Orientare solo a edit_file lasciava l'agente bloccato sui task "comando"
/// (incidente Playwright "Failed to install browsers": 6 iter di sola search -> ABORT a
/// 0 azioni). Mantiene l'anti-loop (NON ri-leggere identico), ma verso l'AZIONE.
fn repeated_read_only_action_nudge(label: &str, count: i64) -> String {
    format!(
        "STOP: hai gia' letto/ispezionato lo stesso bersaglio ({label}) {count} volte. \
Il contenuto e' GIA' nel contesto sopra: rileggerlo non avvicina al risultato. Questo \
e' un task OPERATIVO, non una domanda: NON rileggere e NON rispondere a parole \
descrivendo cosa faresti. ORA ESEGUI l'azione che risolve il task: edit_file/write_file \
se va modificato un file, oppure run_command se va eseguito un comando (installare una \
dipendenza mancante, avviare/riavviare un servizio, lanciare build, test o migrazioni). \
Se ti serve davvero un dettaglio diverso fai UNA lettura su un bersaglio DIVERSO, poi \
procedi subito con l'azione."
    )
}

/// Nudge SPECIFICO per `edit_file`/`write_file` FALLITO ripetuto (causa radice
/// del falso-stallo). NON dice "rileggi con read_file": l'estratto numerato del
/// contenuto reale e' GIA' nel messaggio d'errore precedente (vedi
/// `build_old_string_not_found_message` in mcp-core). L'agente deve solo COPIARE
/// da li' l'old_string esatto. Sostituisce il nudge generico "cambia approccio"
/// che lasciava l'agente senza l'istruzione attuabile -> ABORT a 0 file modificati.
fn repeated_action_edit_failed_nudge(label: &str, count: i64) -> String {
    format!(
        "STOP: '{label}' e' fallito {count} volte perche' l'old_string NON \
corrisponde al testo reale del file. NON ripetere lo stesso old_string e NON \
chiamare read_file: il contenuto attuale del file e' GIA' nell'errore qui sopra \
(estratto numerato, riga per riga). COPIA da quell'estratto l'old_string ESATTO \
\u{2014} spazi, newline e indentazione inclusi \u{2014} scegliendo abbastanza \
righe da renderlo univoco, poi richiama edit_file con quell'old_string corretto. \
Stai CORREGGENDO l'old_string, non ripetendo: e' l'azione giusta per sbloccarti."
    )
}

/// Nudge SPECIFICO per un SERVIZIO long-running (`run_service`/`service_restart`)
/// FALLITO ripetuto: il servizio e' stato avviato piu' volte ma continua a uscire/
/// non resta attivo. Rilanciarlo identico non cambia nulla: la causa va letta dai
/// log del servizio. NON forza l'azione (cosi' read_service_output/tail_service_logs
/// restano disponibili) e NON porta all'abort: l'agente non deve arrendersi, deve
/// diagnosticare. Gemello di [`repeated_action_edit_failed_nudge`] per i servizi.
fn repeated_action_service_failed_nudge(label: &str, count: i64) -> String {
    format!(
        "STOP: hai avviato '{label}' {count} volte ma il servizio NON resta attivo \
(parte e muore subito). NON rilanciarlo di nuovo: e' la stessa azione di prima. \
Leggi PRIMA l'output dell'avvio fallito \u{2014} usa read_service_output (o \
tail_service_logs) sul servizio appena avviato \u{2014} per individuare la CAUSA \
dell'uscita (porta gia' occupata, dipendenza non installata, errore di sintassi/\
config in vite.config.ts/package.json, variabile d'ambiente mancante). CORREGGI \
quella causa con edit_file/run_command, poi riavvia il servizio UNA sola volta. Se \
la causa e' esterna e non risolvibile (porta riservata, credenziale, dipendenza non \
installabile), dichiaralo esplicitamente: non e' un loop, e' un blocco da segnalare."
    )
}

/// Nudge di DIAGNOSI FORZATA (stadio tra GUIDE e ABORT). Vedi
/// `_force_diagnose_nudge` Python (build/test-aware).
fn force_diagnose_nudge(label: &str, count: i64) -> String {
    if is_build_or_test_label(label) {
        format!(
            "STOP: hai ripetuto '{label}' {count} volte e il sollecito precedente \
non ha cambiato nulla. Ri-eseguire il build NON e' un'azione diversa: e' la \
stessa di prima. Un errore di build/test e' CORREGGIBILE: leggi gli errori \
dell'ultima esecuzione (file:riga riportati qui sotto), individua la CAUSA \
RADICE (tipo mancante / import errato / simbolo non definito, non il sintomo) \
leggendo l'output del comando qui sopra, e CORREGGI ORA i file segnalati con una \
tool call (edit_file/write_file); SOLO DOPO rie-esegui. NON chiudere il turno e \
NON dichiararti bloccato: un errore di build/test si risolve correggendo il \
codice, non e' una dipendenza mancante."
        )
    } else {
        format!(
            "STOP: hai ripetuto '{label}' {count} volte e il sollecito precedente non \
ha cambiato nulla. Identifica la CAUSA RADICE del fallimento dall'esito \
esatto dell'ultima esecuzione (la causa reale, non il sintomo) e attaccala \
con un'azione diversa, non la stessa di prima. Se non esiste un'azione \
diversa praticabile, dichiara che sei bloccato e perche' (es. dipendenza / \
credenziale / permesso / servizio mancante): il turno chiudera' con la \
diagnosi e il prossimo passo, non con una ripetizione."
        )
    }
}

/// Punto unico: data la fotografia del progresso, decide la prossima mossa.
///
/// Gerarchia, applicata all'asse di stallo a priorita' piu' alta:
///   - se l'asse NON e' ancora stato guidato (forza-azione) -> GUIDE
///   - solo per repeated_action + flag: -> FORCE_DIAGNOSE
///   - altrimenti, se c'e' un candidato di escalation nel budget -> ESCALATE
///   - altrimenti -> ABORT (verso la verifica E2E)
///
/// Priorita' tra assi: esplorazione, signature-loop, repeated_user_question
/// (cross-run), resource_reallocation, repeated_action, g1-descrittivo. Nessuno
/// stallo -> proceed.
pub fn decide(signals: &ProgressSignals) -> ProgressDecision {
    // Determina l'asse di stallo prioritario (None = nessuno stallo bloccante).
    let axis: Option<Axis> = if signals.exploration_count
        >= 2 * signals.exploration_threshold.max(1)
    {
        Some(Axis::Exploration)
    } else if signals
        .signature_loop_tool
        .as_deref()
        .is_some_and(|t| !t.is_empty())
    {
        // Python: `elif signals.signature_loop_tool:` — truthy = non vuoto/non None.
        Some(Axis::Signature)
    } else if signals.repeated_user_question_count
        >= signals.repeated_user_question_threshold.max(1)
    {
        // Loop clarification CROSS-RUN (l'incidente email Beaty-Book): la STESSA
        // domanda-chiarimento e' gia' stata posta all'utente >= soglia volte nella
        // sessione (segnale strutturato dai meta_step `kind='clarify'`, regola M).
        // Priorita' TRA signature e repeated_action: un loop di TOOL o esplorazione
        // ha precedenza (piu' locale), ma la ri-domanda ripetuta precede gli assi
        // intra-run repeated_action/g1. `.max(1)`: soglia degenere <=0 -> almeno 1.
        Some(Axis::RepeatedUserQuestion)
    } else if signals.reallocation_count >= signals.reallocation_threshold.max(1) {
        Some(Axis::ResourceReallocation)
    } else if signals.repeated_action.is_some() {
        Some(Axis::RepeatedAction)
    } else if signals.g1_over_cap {
        Some(Axis::G1Descriptive)
    } else {
        None
    };

    let Some(axis) = axis else {
        return ProgressDecision {
            action: Action::Proceed,
            axis: None,
            force_action: false,
            nudge_text: None,
            stop_reason: None,
            reason: "nessuno stallo bloccante".to_string(),
        };
    };

    let already = signals.already_guided.contains(axis.as_str());
    let already_diagnosed = signals.already_diagnosed.contains(axis.as_str());
    let can_escalate =
        signals.has_escalation_candidate && signals.escalations < signals.max_escalations;

    // Livello 1 — GUIDE (forza-azione): solo se non gia' tentato per questo asse.
    if !already {
        let nudge = match axis {
            Axis::Exploration => {
                exploration_nudge(signals.exploration_count, signals.has_active_resources)
            }
            Axis::Signature => {
                signature_nudge(signals.signature_loop_tool.as_deref().unwrap_or(""))
            }
            Axis::ResourceReallocation => resource_reallocation_nudge(signals.reallocation_count),
            Axis::RepeatedAction => {
                let (label, count) = signals
                    .repeated_action
                    .clone()
                    .unwrap_or_else(|| (String::new(), 0));
                // edit_file/write_file FALLITO -> nudge SPECIFICO (copia
                // l'old_string esatto dall'estratto gia' nell'errore); lettura
                // ripetuta -> nudge biforcato per action_oriented (su un fix orienta
                // all'EDIT, su una domanda "concludi con testo"); altrimenti generico.
                if signals.repeated_action_edit_failed {
                    repeated_action_edit_failed_nudge(&label, count)
                } else if signals.repeated_action_service_failed {
                    repeated_action_service_failed_nudge(&label, count)
                } else if signals.repeated_action_read_only {
                    if signals.action_oriented {
                        repeated_read_only_action_nudge(&label, count)
                    } else {
                        repeated_read_only_nudge(&label, count)
                    }
                } else {
                    repeated_action_nudge(&label, count)
                }
            }
            Axis::G1Descriptive => g1_nudge(),
            // Asse alimentato dal detector clarification cross-run (Task #5): con
            // reasoner OFF resta la GUIDE fissa (nudge "non ri-chiedere"); con
            // reasoner ON e' quest'ultimo a decidere la mossa. `decide` non lo
            // assegna finche' il detector non e' innestato -> arm per esaustivita'.
            Axis::RepeatedUserQuestion => repeated_user_question_nudge(),
        };
        // Solo resource_reallocation resta SOFT (il nudge ordina di riusare le porte,
        // non c'e' un'azione correttiva diretta). Per repeated_action PRODUTTIVA
        // FORZIAMO una nuova tool call (force-action): un'azione ripetuta che fallisce
        // va CORRETTA, non ripetuta ne' abbandonata -> rimuove i read-only e impone
        // tool_choice. Per repeated_action di SOLA LETTURA il comportamento e'
        // ACTION-AWARE: su un task INFORMATIVO non forziamo (forzare un altro
        // read-only creerebbe un nuovo loop -> il nudge guida a concludere con testo,
        // NON-convergenza regola H); su un task di MODIFICA/fix forziamo invece la
        // tool call cosi' l'agente APPLICA l'edit (il nudge orienta a edit_file/
        // write_file) invece di rinunciare con 0 file modificati.
        let force = match axis {
            // resource_reallocation: SOFT (riusa-porte, nessuna azione correttiva).
            Axis::ResourceReallocation => false,
            // repeated_user_question: SOFT. Il nudge guida a USARE il valore gia'
            // fornito (anche se oscurato, come opaco) o a dichiarare il blocco con
            // task_complete: NON forziamo una tool call (forzare tool_choice
            // required rimuoverebbe i read-only e obbligherebbe un'azione-tool che
            // qui non e' la mossa giusta -> alimenterebbe un altro loop). Con
            // reasoner ON e' quest'ultimo a decidere una mossa piu' ricca.
            Axis::RepeatedUserQuestion => false,
            // SERVIZIO long-running fallito: NON forzare. La forza-azione rimuove i
            // tool read-only, ma qui l'agente DEVE poterli usare (read_service_output/
            // tail_service_logs) per leggere perche' il servizio e' morto; il nudge lo
            // guida a diagnosticare e correggere, non a rilanciare. Forzare l'azione
            // rimuoverebbe i log e lo costringerebbe a ri-avviare -> il loop che
            // vogliamo spezzare.
            Axis::RepeatedAction if signals.repeated_action_service_failed => false,
            // repeated_action di SOLA LETTURA su turno INFORMATIVO: NON forzare
            // (forzerebbe un altro read-only -> nuovo loop); il nudge guida a
            // concludere con testo. Su turno ACTION-ORIENTED invece forziamo l'edit.
            Axis::RepeatedAction if signals.repeated_action_read_only && !signals.action_oriented => {
                false
            }
            _ => true,
        };
        let reason = match axis {
            Axis::RepeatedAction => {
                format!("stallo {}: forza-azione correttiva (no ripetizione)", axis.as_str())
            }
            Axis::ResourceReallocation => {
                format!("stallo {}: nudge riusa-porte (no nuova allocazione)", axis.as_str())
            }
            _ => format!(
                "stallo {}: forza-azione (rimuovo read-only + tool_choice required)",
                axis.as_str()
            ),
        };
        return ProgressDecision {
            action: Action::Guide,
            axis: Some(axis),
            force_action: force,
            nudge_text: Some(nudge),
            stop_reason: None,
            reason,
        };
    }

    // Livello 1.5 — FORCE_DIAGNOSE: solo per repeated_action, dopo che la GUIDE
    // soft non ha cambiato nulla e PRIMA di escalation/abort.
    //
    // Per l'edit_file/write_file FALLITO la diagnosi forzata e' SEMPRE preferita a
    // escalation/ABORT finche' l'estratto numerato (gia' presente nell'errore) non
    // e' stato sfruttato: l'edit fallito NON e' una causa bloccante (dipendenza /
    // credenziale / permesso), e' un old_string da correggere. Quindi non serve un
    // modello piu' capace ne' una chiusura: serve copiare l'estratto. Per le altre
    // ripetizioni resta il comportamento storico (governato dal flag).
    // Un'azione ripetuta che FALLISCE per segnale STRUTTURATO (exit_code/is_error,
    // regola M) e' una causa radice da diagnosticare, non un loop da abortire: la
    // diagnosi forzata e' SEMPRE preferita a escalation/ABORT (come per
    // edit/service falliti). Copre es. una `curl` di health-check con exit 7.
    let want_force_diagnose = matches!(axis, Axis::RepeatedAction)
        && !already_diagnosed
        && (signals.force_diagnose_enabled
            || signals.repeated_action_edit_failed
            || signals.repeated_action_service_failed
            || signals.repeated_action_failed);
    if want_force_diagnose {
        let (label, count) = signals
            .repeated_action
            .clone()
            .unwrap_or_else(|| (String::new(), 0));
        // Per l'edit fallito riusa il nudge SPECIFICO (copia l'old_string esatto
        // dall'estratto); per il servizio fallito il nudge diagnostico-servizio (leggi
        // i log, correggi la causa); altrimenti quello diagnostico generico.
        let nudge = if signals.repeated_action_edit_failed {
            repeated_action_edit_failed_nudge(&label, count)
        } else if signals.repeated_action_service_failed {
            repeated_action_service_failed_nudge(&label, count)
        } else {
            force_diagnose_nudge(&label, count)
        };
        let reason = if signals.repeated_action_edit_failed {
            "stallo repeated_action (edit fallito): correzione old_string forzata, \
                niente ABORT finche' l'estratto non e' sfruttato"
                .to_string()
        } else if signals.repeated_action_service_failed {
            "stallo repeated_action (servizio fallito): diagnosi log-servizio forzata, \
                niente ABORT e niente forza-azione (i log restano leggibili)"
                .to_string()
        } else {
            "stallo repeated_action: correzione forzata prima di escalation/abort".to_string()
        };
        return ProgressDecision {
            action: Action::ForceDiagnose,
            axis: Some(axis),
            // Forza una tool call correttiva: la diagnosi deve sfociare in un edit,
            // non in testo o resa. ECCEZIONE servizio fallito: NON forzare, cosi' i
            // tool di lettura log (read-only) restano disponibili per capire perche'
            // il servizio muore (forzare rimuoverebbe i log -> rilancio cieco -> loop).
            force_action: !signals.repeated_action_service_failed,
            nudge_text: Some(nudge),
            stop_reason: None,
            reason,
        };
    }

    // Livello 1.9 — CAMBIO DI STRATEGIA (solo repeated_action): guida e diagnosi
    // gia' spese, PRIMA di cambiare modello si ordina al modello CORRENTE di
    // cambiare STRADA (strumento alternativo / piu' contesto / passo piu'
    // piccolo). E' il comportamento standard di un agente capace davanti a uno
    // stallo: prima si cambia approccio, poi — solo se serve — il modello.
    // L'escalation resta il livello successivo, non viene consumata qui.
    let want_strategy_shift = matches!(axis, Axis::RepeatedAction)
        && !signals.already_strategy_shifted.contains(axis.as_str());
    if want_strategy_shift {
        let (label, count) = signals
            .repeated_action
            .clone()
            .unwrap_or_else(|| (String::new(), 0));
        return ProgressDecision {
            action: Action::ChangeStrategy,
            axis: Some(axis),
            // Il cambio di strategia deve produrre un'AZIONE nuova, non testo.
            force_action: true,
            nudge_text: Some(strategy_shift_nudge(&label, count)),
            stop_reason: None,
            reason: "stallo repeated_action: cambio di strategia forzato prima dell'escalation \
                di modello"
                .to_string(),
        };
    }

    // Livello 2 — ESCALATE: gia' guidato ma ancora bloccato, c'e' budget.
    if can_escalate {
        return ProgressDecision {
            action: Action::Escalate,
            axis: Some(axis),
            force_action: false,
            nudge_text: None,
            stop_reason: None,
            reason: format!(
                "stallo {} persiste dopo forza-azione: escalation modello ({}/{})",
                axis.as_str(),
                signals.escalations + 1,
                signals.max_escalations
            ),
        };
    }

    // Livello 3 — ABORT verso verifica: guida+escalation esaurite.
    ProgressDecision {
        action: Action::Abort,
        axis: Some(axis),
        force_action: false,
        nudge_text: None,
        stop_reason: Some(ABORT_STOP_REASON.to_string()),
        reason: format!(
            "stallo {}: guida ed escalation esaurite -> chiusura con verifica E2E (final_gate)",
            axis.as_str()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nessuno_stallo_proceed() {
        let d = decide(&ProgressSignals::default());
        assert_eq!(d.action, Action::Proceed);
        assert_eq!(d.axis, None);
    }

    #[test]
    fn esplorazione_guida_con_risorse_attive() {
        let signals = ProgressSignals {
            exploration_count: 12,
            exploration_threshold: 6,
            has_active_resources: true,
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(d.action, Action::Guide);
        assert_eq!(d.axis, Some(Axis::Exploration));
        assert!(d.force_action);
        assert!(d.nudge_text.as_deref().unwrap().contains("NON allocarne di nuove"));
    }

    #[test]
    fn repeated_action_forza_azione_correttiva() {
        // Un'azione ripetuta che fallisce (es. build) va CORRETTA, non ripetuta ne'
        // abbandonata: la GUIDE forza una tool call (force_action=true) cosi' il
        // modello DEVE agire. Prima era force=false (solo nudge testuale) -> un
        // modello debole si arrendeva e scattava l'ABORT (incidente pnpm build).
        let signals = ProgressSignals {
            repeated_action: Some(("run_command: npm run build".to_string(), 3)),
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(d.action, Action::Guide);
        assert!(d.force_action, "repeated_action ora forza l'azione correttiva");
    }

    #[test]
    fn repeated_action_edit_fallito_nudge_specifico() {
        // GUIDE di un edit_file FALLITO: il nudge deve essere SPECIFICO (copia
        // l'old_string esatto dall'estratto), non quello generico.
        let signals = ProgressSignals {
            repeated_action: Some(("edit_file: src/lib.rs".to_string(), 2)),
            repeated_action_edit_failed: true,
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(d.action, Action::Guide);
        assert!(d.force_action);
        let nudge = d.nudge_text.as_deref().unwrap();
        assert!(
            nudge.contains("old_string ESATTO"),
            "atteso nudge specifico edit-fallito, ottenuto: {nudge}"
        );
        assert!(nudge.contains("NON chiamare read_file"));
        // Non deve essere il nudge generico.
        assert!(!nudge.contains("cambia approccio"));
    }

    #[test]
    fn repeated_action_servizio_fallito_guida_diagnostica_senza_forzare() {
        // GUIDE di un run_service FALLITO ripetuto (dev server che muore): il nudge
        // deve essere SPECIFICO (leggi i log del servizio, correggi la causa) e NON
        // deve forzare l'azione (cosi' read_service_output/tail_service_logs restano
        // disponibili; forzare rimuoverebbe i read-only e costringerebbe a rilanciare).
        let signals = ProgressSignals {
            repeated_action: Some(("run_service: pnpm run dev".to_string(), 2)),
            repeated_action_service_failed: true,
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(d.action, Action::Guide);
        assert!(
            !d.force_action,
            "servizio fallito: NON forzare (i log devono restare leggibili)"
        );
        let nudge = d.nudge_text.as_deref().unwrap();
        assert!(
            nudge.contains("read_service_output"),
            "atteso nudge diagnostico-servizio, ottenuto: {nudge}"
        );
        assert!(nudge.contains("NON rilanciarlo"));
        // Non deve essere il nudge edit-specifico.
        assert!(!nudge.contains("old_string ESATTO"));
    }

    #[test]
    fn repeated_action_servizio_fallito_force_diagnose_niente_abort() {
        // Dopo la GUIDE (asse gia' guidato), un servizio fallito NON deve abortire ne'
        // escalare: deve restare in FORCE_DIAGNOSE (leggi i log e correggi), come per
        // l'edit fallito. force_action resta OFF (i log devono restare leggibili).
        let mut guided = HashSet::new();
        guided.insert(Axis::RepeatedAction.as_str().to_string());
        let signals = ProgressSignals {
            repeated_action: Some(("run_service: pnpm run dev".to_string(), 3)),
            repeated_action_service_failed: true,
            already_guided: guided,
            // C'e' un candidato di escalation con budget: senza il ramo servizio,
            // cadrebbe in ESCALATE/ABORT invece di restare in diagnosi.
            has_escalation_candidate: true,
            escalations: 0,
            max_escalations: 3,
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(
            d.action,
            Action::ForceDiagnose,
            "servizio fallito gia' guidato: diagnosi forzata, non abort/escalate"
        );
        assert!(!d.force_action, "i log del servizio devono restare leggibili");
        let nudge = d.nudge_text.as_deref().unwrap();
        assert!(nudge.contains("read_service_output"));
    }

    #[test]
    fn repeated_action_fallito_strutturale_forza_diagnosi_non_abort() {
        // Regola M generalizzata: un'azione ripetuta che FALLISCE per segnale
        // STRUTTURATO (es. `run_command: curl` con exit 7 = server non in ascolto),
        // gia' guidata e senza stadio force_diagnose abilitato globalmente, NON deve
        // abortire ("il modello non riesce") ma restare in FORCE_DIAGNOSE (guida
        // alla causa radice). Prima del fix cadeva in ESCALATE/ABORT.
        let mut guided = HashSet::new();
        guided.insert(Axis::RepeatedAction.as_str().to_string());
        let signals = ProgressSignals {
            repeated_action: Some((
                "run_command: curl -sS http://localhost:31788/".to_string(),
                2,
            )),
            repeated_action_failed: true,
            already_guided: guided,
            // Nessun candidato escalation: senza il ramo failed, cadrebbe in ABORT.
            has_escalation_candidate: false,
            force_diagnose_enabled: false,
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(
            d.action,
            Action::ForceDiagnose,
            "fallimento strutturale ripetuto: diagnosi forzata, non abort"
        );
        // Nudge diagnostico generico: causa radice + eventuale blocco esplicito.
        let nudge = d.nudge_text.as_deref().unwrap();
        assert!(nudge.contains("CAUSA RADICE") || nudge.contains("causa radice"));
    }

    #[test]
    fn repeated_action_non_edit_nudge_generico() {
        // GUIDE di una ripetizione NON-edit (es. comando generico): resta il
        // nudge generico, non quello edit-specifico.
        let signals = ProgressSignals {
            repeated_action: Some(("run_command: ls".to_string(), 2)),
            repeated_action_edit_failed: false,
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(d.action, Action::Guide);
        let nudge = d.nudge_text.as_deref().unwrap();
        assert!(!nudge.contains("old_string ESATTO"));
    }

    #[test]
    fn repeated_action_read_only_informativo_guida_a_concludere_senza_forzare() {
        // GUIDE di una LETTURA ripetuta su un turno INFORMATIVO (action_oriented=false):
        // NON deve forzare un'altra tool call (forzerebbe un ennesimo read-only ->
        // nuovo loop): deve guidare a CONCLUDERE con testo (NON-convergenza, regola H).
        let signals = ProgressSignals {
            repeated_action: Some(("read_file: src/main.rs".to_string(), 2)),
            repeated_action_read_only: true,
            action_oriented: false,
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(d.action, Action::Guide);
        assert!(
            !d.force_action,
            "una lettura ripetuta informativa NON deve forzare un'altra tool call"
        );
        let nudge = d.nudge_text.as_deref().unwrap();
        assert!(
            nudge.contains("Rispondi ORA a parole"),
            "atteso nudge 'concludi con testo', ottenuto: {nudge}"
        );
        // Non deve essere il nudge generico produttivo ne' quello edit-oriented.
        assert!(!nudge.contains("cambia approccio"));
        assert!(!nudge.contains("APPLICA la correzione"));
    }

    #[test]
    fn repeated_action_read_only_action_oriented_orienta_all_edit() {
        // GUIDE di una LETTURA ripetuta su un turno ACTION-ORIENTED (task di fix):
        // NON deve chiudere con testo (sarebbe una RINUNCIA a 0 file modificati). Deve
        // orientare all'EDIT e forzare la tool call, preservando l'anti-loop (no
        // ri-lettura identica). Causa radice dell'incidente "porta hardcoded".
        let signals = ProgressSignals {
            repeated_action: Some(("read_file: vite.config.ts".to_string(), 2)),
            repeated_action_read_only: true,
            action_oriented: true,
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(d.action, Action::Guide);
        assert!(
            d.force_action,
            "su un task di fix la lettura ripetuta deve forzare l'edit, non rinunciare"
        );
        let nudge = d.nudge_text.as_deref().unwrap();
        // Orienta all'azione concreta (edit O comando), NON alla chiusura testuale.
        assert!(
            nudge.contains("ESEGUI l'azione"),
            "atteso nudge orientato all'azione, ottenuto: {nudge}"
        );
        assert!(nudge.contains("edit_file/write_file"));
        // Copre anche i task che si risolvono con un COMANDO (installazioni, build,
        // restart): orientare solo a edit_file bloccava i task "comando" (Playwright).
        assert!(nudge.contains("run_command"));
        // NON deve orientare alla risposta a parole (sarebbe la rinuncia).
        assert!(
            !nudge.contains("Rispondi ORA a parole"),
            "su un fix NON deve guidare a rispondere a parole, ottenuto: {nudge}"
        );
        // Anti-loop preservato: vieta comunque la ri-lettura identica.
        assert!(nudge.contains("NON rileggere"));
    }

    #[test]
    fn repeated_action_edit_fallito_force_diagnose_prima_di_abort() {
        // Gia' guidato + budget escalation esaurito: per l'edit fallito si va a
        // FORCE_DIAGNOSE (nudge specifico) PRIMA dell'ABORT, anche senza il flag
        // force_diagnose_enabled, finche' l'estratto non e' sfruttato.
        let mut guided = HashSet::new();
        guided.insert("repeated_action".to_string());
        let signals = ProgressSignals {
            repeated_action: Some(("edit_file: src/lib.rs".to_string(), 2)),
            repeated_action_edit_failed: true,
            already_guided: guided,
            has_escalation_candidate: false,
            force_diagnose_enabled: false,
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(d.action, Action::ForceDiagnose);
        assert!(d.nudge_text.as_deref().unwrap().contains("old_string ESATTO"));
    }

    #[test]
    fn repeated_action_edit_fallito_abort_solo_dopo_diagnosi() {
        // Gia' guidato, diagnosticato E strategia gia' cambiata, niente
        // escalation: solo allora l'edit fallito puo' chiudere con ABORT.
        let mut guided = HashSet::new();
        guided.insert("repeated_action".to_string());
        let mut diagnosed = HashSet::new();
        diagnosed.insert("repeated_action".to_string());
        let mut strategy = HashSet::new();
        strategy.insert("repeated_action".to_string());
        let signals = ProgressSignals {
            repeated_action: Some(("edit_file: src/lib.rs".to_string(), 2)),
            repeated_action_edit_failed: true,
            already_guided: guided,
            already_diagnosed: diagnosed,
            already_strategy_shifted: strategy,
            has_escalation_candidate: false,
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(d.action, Action::Abort);
        assert_eq!(d.stop_reason.as_deref(), Some(ABORT_STOP_REASON));
    }

    #[test]
    fn repeated_action_cambio_strategia_prima_di_escalation() {
        // NUOVO livello 1.9: guida e diagnosi spese, strategia NON ancora
        // cambiata -> ChangeStrategy (force-action, nudge con alternative),
        // ANCHE con un candidato di escalation disponibile: prima si cambia
        // strada, poi il modello.
        let mut guided = HashSet::new();
        guided.insert("repeated_action".to_string());
        let mut diagnosed = HashSet::new();
        diagnosed.insert("repeated_action".to_string());
        let signals = ProgressSignals {
            repeated_action: Some(("edit_file: src/lib.rs".to_string(), 3)),
            repeated_action_edit_failed: true,
            already_guided: guided,
            already_diagnosed: diagnosed,
            has_escalation_candidate: true,
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(d.action, Action::ChangeStrategy);
        assert!(d.force_action);
        let nudge = d.nudge_text.as_deref().unwrap();
        assert!(nudge.contains("CAMBIA STRATEGIA"));
        assert!(nudge.contains("task_complete"));
    }

    #[test]
    fn repeated_user_question_scatta_a_soglia_guida_soft() {
        // A soglia (count >= threshold) l'asse RepeatedUserQuestion scatta: GUIDE
        // col nudge fisso "non ri-chiedere", SENZA forzare la tool call (SOFT: il
        // nudge guida a usare il valore gia' fornito o a dichiarare il blocco).
        let signals = ProgressSignals {
            repeated_user_question_count: 2,
            repeated_user_question_threshold: 2,
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(d.action, Action::Guide);
        assert_eq!(d.axis, Some(Axis::RepeatedUserQuestion));
        assert!(
            !d.force_action,
            "repeated_user_question e' SOFT: non forza una tool call"
        );
        let nudge = d.nudge_text.as_deref().unwrap();
        assert!(
            nudge.contains("gia' posto questa stessa domanda"),
            "atteso nudge repeated_user_question, ottenuto: {nudge}"
        );
        assert!(nudge.contains("task_complete"));
    }

    #[test]
    fn repeated_user_question_sotto_soglia_non_scatta() {
        // Comportamento invariato: con la soglia di default (2) e count 1 l'asse
        // non scatta -> nessuno stallo -> proceed. Preserva i run normali.
        let signals = ProgressSignals {
            repeated_user_question_count: 1,
            repeated_user_question_threshold: 2,
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(d.action, Action::Proceed);
        assert_eq!(d.axis, None);
    }

    #[test]
    fn repeated_user_question_default_neutro_non_scatta() {
        // Default puri (count 0, soglia 2): l'asse non scatta MAI su un run senza
        // storia di clarify ripetuti -> comportamento bit-invariato.
        let d = decide(&ProgressSignals::default());
        assert_eq!(d.action, Action::Proceed);
        assert_eq!(d.axis, None);
    }

    #[test]
    fn repeated_user_question_priorita_tra_signature_e_repeated_action() {
        // Con signature-loop E ripetizione domanda entrambi attivi, vince
        // signature (piu' locale). Con repeated_action E ripetizione domanda,
        // vince repeated_user_question (priorita' TRA signature e repeated_action).
        let sig_wins = ProgressSignals {
            signature_loop_tool: Some("read_file".to_string()),
            repeated_user_question_count: 5,
            repeated_user_question_threshold: 2,
            ..Default::default()
        };
        assert_eq!(decide(&sig_wins).axis, Some(Axis::Signature));

        let ruq_over_repeated = ProgressSignals {
            repeated_action: Some(("edit_file: x".to_string(), 3)),
            repeated_user_question_count: 2,
            repeated_user_question_threshold: 2,
            ..Default::default()
        };
        assert_eq!(
            decide(&ruq_over_repeated).axis,
            Some(Axis::RepeatedUserQuestion),
            "repeated_user_question ha priorita' su repeated_action"
        );
    }

    #[test]
    fn repeated_user_question_soglia_degenere_almeno_uno() {
        // Soglia <= 0 non deve rendere l'asse sempre attivo con count 0: `.max(1)`
        // impone almeno 1 occorrenza. Count 0, soglia 0 -> non scatta.
        let no = ProgressSignals {
            repeated_user_question_count: 0,
            repeated_user_question_threshold: 0,
            ..Default::default()
        };
        assert_eq!(decide(&no).axis, None);
        // Count 1, soglia 0 -> scatta (1 >= max(1)).
        let yes = ProgressSignals {
            repeated_user_question_count: 1,
            repeated_user_question_threshold: 0,
            ..Default::default()
        };
        assert_eq!(decide(&yes).axis, Some(Axis::RepeatedUserQuestion));
    }

    #[test]
    fn abort_dopo_guida_senza_escalation() {
        let mut guided = HashSet::new();
        guided.insert("signature".to_string());
        let signals = ProgressSignals {
            signature_loop_tool: Some("read_file".to_string()),
            already_guided: guided,
            has_escalation_candidate: false,
            ..Default::default()
        };
        let d = decide(&signals);
        assert_eq!(d.action, Action::Abort);
        assert_eq!(d.stop_reason.as_deref(), Some(ABORT_STOP_REASON));
    }
}
