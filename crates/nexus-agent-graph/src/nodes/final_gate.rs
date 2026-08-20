//! `FinalGateNode` — porta la parte DETERMINISTICA del `final_gate_node`
//! (`brain/agents/final_gate.py:496-551`).
//!
//! Il final_gate e' il gate generale fail-closed per i task SOFTWARE che
//! chiudono SENZA plan_phase (executor diretto, end_turn): senza di esso
//! un'app placeholder montata sopra un design importato passerebbe in silenzio
//! (fail-open, incidente Beauty-Book 2026-06-11). E' un nodo DETERMINISTICO:
//! NON chiama l'LLM (a differenza di router/understanding/clarify). L'unico I/O
//! e' l'esecuzione dei criteri di verifica, delegata al sotto-sistema
//! [`CriteriaRunner`] (vedi sotto); la config DB-driven e' risolta a MONTE.
//!
//! ## Cosa porta QUESTO PR (deterministico, testato golden 1:1)
//!
//! - **La decision machine** (`final_gate.py:496-546`, [`FinalGateNode::run`]):
//!   gate `enabled`/`is_software_task` -> pass-through `{}`; ramo PASSED ->
//!   `{final_gate_cycle:0, stop_reason:end_turn, final_gate_passed:true}`; ramo
//!   FORCED (`forced_close_unverified` OR `cycle>=max_cycles`) ->
//!   `{final_gate_cycle:0, stop_reason:end_turn, final_gate_passed:false}`
//!   (verdetto NEGATIVO esplicito: il finalizzatore mappa FailedDiagnosed e
//!   annota il resoconto); ramo FAIL -> inietta `HumanMessage(_render_failed_block)`
//!   + `{messages:append, final_gate_cycle:cycle, stop_reason:tool_use,
//!   pending_tool_uses:[]}`.
//! - **`is_software_task`**: DELEGATO al PUNTO UNICO `signals::is_software_task`
//!   (regola L: e' identico a `_is_software_task` del Python e gia' usato dalle
//!   `route_after_*`; NON re-implementato qui). Esso valuta in OR il segnale
//!   STRUTTURALE `has_filesystem_mutation_in_history` (lista mutator-tools
//!   DB-driven dalla config) e la whitelist intent.
//! - **La costruzione delle spec criteri** (`final_gate.py:400-470`,
//!   [`FinalGateNode::build_criteria`]): `no_orphan_imported` (sempre),
//!   `outputs_exist` (sempre), `service_logs_clean` (se `runtime_check_enabled` +
//!   `log_command`), `run_command`-build (se `build_command` presente) e i
//!   criteri `http`-endpoint (configurati nel progetto + DICHIARATI dall'agente
//!   via `task_complete.endpoints`, punto unico
//!   [`crate::decisions::endpoint_probes`]). Costruzione PURA.
//! - **`_count_build_errors` + `_BUILD_ERROR_PATTERNS`** (`final_gate.py:276-294`):
//!   regex TS/rustc/SyntaxError/TypeError/generico, conteggio indicativo. 1:1.
//! - **`_render_failed_block`** (`final_gate.py:396-493`,
//!   [`FinalGateNode::render_failed_block`]): testo `<final_gate_failed>` con
//!   excerpt per criterio, ramo speciale build (max_output_chars,
//!   output_truncated, header), direttive fail-closed, prefisso `<autonomy_hint>`
//!   se behavior_mode autonomo. Stringhe deterministiche 1:1.
//! - **`esito_criteri`** (`final_gate.py:392`): il reduce sui risultati
//!   MISURABILI; i non misurabili non bocciano ma tolgono alla chiusura il
//!   titolo di "verificata" (vedi [`CriterionOutcome`]).
//!
//! ## Cosa NON porta (sotto-sistema delegato dietro porta + TODO espliciti)
//!
//! - **Esecuzione dei criteri** (`criteria_runner._check_*`,
//!   `brain/agents/criteria_runner.py`): SOTTO-SISTEMA separato (come
//!   `closure_judge` per il learner). Il nodo costruisce le [`CriterionSpec`] e
//!   ottiene i [`CriterionResult`] tramite il trait [`CriteriaRunner`]; la LOGICA
//!   dei singoli criteri (grafo import, esistenza file su disco, parsing log,
//!   esecuzione build) NON e' portata in questo PR -> TODO esplicito, porta
//!   dedicata da implementare con l'integrazione del ToolRunner gRPC. Per il
//!   GOLDEN i risultati sono INPUT stubati.
//! - **La risoluzione della config** (`_resolve_build_command`/
//!   `_resolve_log_command`/`_build_timeout_s`/`_build_output_max_chars` +
//!   `_project_slug`): tutta lettura DB (regola G) -> risolta A MONTE dal
//!   chiamante, passata nella [`FinalGateConfig`]. In particolare `_project_slug`
//!   NON e' portato qui (vive in mcp-core `project_workspace/logs.rs` e serve solo
//!   al risolutore della config; `nexus-agent-graph` non puo' dipendere da
//!   mcp-core, regola L).
//!
//! Il nodo NON instrada: l'edge post-final_gate vive in `routing::route_after_final_gate`.

use std::sync::LazyLock;

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::routing::config::RoutingConfig;
use crate::routing::signals;
use crate::runtime::ports::{CriterionProvenance, CriterionResult, CriterionSpec};
use crate::runtime::AgentNodeCtx;
use crate::state::{
    AgentState, FinalGateVerdict, GateRouting, Message, MessageContent, StateDelta, StopReason,
};

/// Pattern di errore di compilazione comuni (TypeScript, Rust, generici).
/// Replica 1:1 `_BUILD_ERROR_PATTERNS` (`final_gate.py:276-282`). Il conteggio
/// e' indicativo (best-effort): comunica all'agente la SCALA del problema, non
/// e' un parser esatto. Compilati una volta sola (`LazyLock`).
static BUILD_ERROR_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // tsc: `error TS\d+:` (IGNORECASE).
        Regex::new(r"(?i)error TS\d+:").expect("regex tsc valida"),
        // rustc: `\berror\[E\d+\]` (IGNORECASE).
        Regex::new(r"(?i)\berror\[E\d+\]").expect("regex rustc valida"),
        Regex::new(r"\bSyntaxError\b").expect("regex SyntaxError valida"),
        Regex::new(r"\bTypeError\b").expect("regex TypeError valida"),
        // generico cargo/cc: `^\s*error:\s` (MULTILINE).
        Regex::new(r"(?m)^\s*error:\s").expect("regex error generico valida"),
        // vite/rollup/esbuild: il bundler JS puo' USCIRE 0 anche quando il build
        // FALLISCE (config con exit-code bugiardo). Questi pattern rendono
        // count_build_errors > 0 -> il criterio build fallisce comunque (rete di
        // sicurezza in criteria_runner::check_run_command). Sono FRASI di
        // fallimento DETERMINISTICHE (regola M): vite le stampa SOLO quando il
        // build fallisce davvero, mai su un successo con warning.
        Regex::new(r"(?i)could not resolve\b").expect("regex rollup resolve valida"),
        Regex::new(r"(?i)\berror during build\b").expect("regex vite build valida"),
        Regex::new(r"(?i)\bbuild failed\b").expect("regex vite failed valida"),
        // NB: NIENTE pattern sul solo prefisso `[plugin:...]`. Vite lo emette anche
        // su WARNING benigni (`[plugin:vite:reporter]` per import misto dinamico/
        // statico o chunk > 500 kB), quindi contarlo come errore boccia un build
        // uscito 0 e OGGETTIVAMENTE riuscito -> falso negativo del final_gate (run
        // 48793fde, Beaty-Book: `pnpm build` exit 0 + reporter warning, gate 2/2
        // bocciato). Un vero errore di plugin stampa SEMPRE "error during build:"
        // (o "could not resolve" / "build failed"), gia' coperti sopra: il prefisso
        // nudo non aggiunge copertura, aggiunge solo falsi positivi.
    ]
});

/// Config DB-driven del nodo final_gate, PASSATA (regola G: nessuna lettura DB
/// nel nodo, nessun fallback hardcoded dentro la logica decisionale).
///
/// Marcatore in `extra` posato dal final_gate quando la NON-CONVERGENZA del gate
/// (criteri OGGETTIVI ancora falliti a `cycle >= max_cycles`) va promossa a un
/// tentativo di ESCALATION di modello invece di chiudere secco. Consumato
/// dall'executor al rientro (`ToolUse`), che delega al PUNTO UNICO di escalation
/// [`crate::nodes::executor`]`::maybe_escalate_nonconvergence` (regola L): il
/// gate NON ha la porta di escalation, l'executor si'. Segnale STRUTTURATO
/// (regola M), non testo. Vedi `route_after_final_gate` (ToolUse -> executor).
pub const FINAL_GATE_ESCALATION_KEY: &str = "final_gate_escalation_pending";

/// Mappa i settings risolti dal brain (`orchestrator_config.get()` +
/// `_resolve_build_command`/`_resolve_log_command`/`_build_timeout_s`/
/// `_build_output_max_chars`, `final_gate.py:78-271`). I comandi build/log sono
/// gia' RISOLTI per-progetto a monte (regola G): il nodo li riceve pronti, non
/// legge il DB ne' calcola lo slug.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FinalGateConfig {
    /// Final gate abilitato (`final_gate_enabled`, default true). OFF ->
    /// pass-through `{}` (`final_gate.py:505`).
    pub enabled: bool,
    /// Cap di cicli del final gate (`final_gate_max_cycles`, default 2). Al cap
    /// si chiude comunque (no loop infinito, `final_gate.py:532`).
    pub max_cycles: i64,
    /// Verifica runtime E2E dei log servizi abilitata
    /// (`final_gate_runtime_check_enabled`, default ON). Aggiunge il criterio
    /// `service_logs_clean` (`final_gate.py:342`).
    pub runtime_check_enabled: bool,
    /// Timeout (s) dei criteri comando della catena di verifica (default 180:
    /// i build sono lenti). Risolto a monte.
    pub build_timeout_s: f64,
    /// Limite caratteri dell'output_excerpt del criterio build esposto
    /// all'agente quando fallisce (`_build_output_max_chars`, default 4000,
    /// clamp 1000-32000). Risolto a monte (gia' clampato dal chiamante).
    pub build_output_max_chars: i64,
    /// Comando log dei servizi risolto per-progetto (`_resolve_log_command`).
    /// Vuoto = nessun criterio `service_logs_clean` anche se runtime_check ON
    /// (`final_gate.py:344`).
    pub log_command: String,
    /// Pattern di errore runtime per `service_logs_clean`
    /// (`final_gate_runtime_error_patterns`). Vuoto ammesso.
    pub runtime_error_patterns: Vec<String>,
    /// Ratio minimo di moduli raggiunti per `no_orphan_imported`
    /// (`no_orphan_min_ratio`, default 0.4).
    pub no_orphan_min_ratio: f64,
    /// Directory di staging per `no_orphan_imported` (`import_staging_dirs`,
    /// default `["figma_export"]`).
    pub import_staging_dirs: Vec<String>,
    /// Timeout (s) generale dei criteri non-build (`verifier_timeout_s`, def 30):
    /// passato nel ctx dei criteri (`final_gate.py:313`).
    pub criteria_timeout_s: f64,
    /// Criteri ENDPOINT HTTP CONFIGURATI nel progetto, risolti A MONTE (regola G:
    /// la lettura di `run_configurations` con `role='endpoint'` + `http_spec`
    /// resta fuori dal nodo, come `log_command`). Lista, non singolo: un CRUD ha
    /// molti endpoint e quello rotto non e' quasi mai la GET — nel caso reale
    /// (gestione-spese, 2026-07-28) la GET rispondeva 200 e la POST 500.
    ///
    /// Vuota = nessun endpoint configurato o check disabilitato. NON e' l'unica
    /// fonte: i criteri DICHIARATI dall'agente si aggiungono in
    /// [`FinalGateNode::build_criteria`] (una config manuale che nessuno compila
    /// equivale a nessuna verifica — vedi [`crate::decisions::endpoint_probes`]).
    pub endpoint_criteria: Vec<CriterionSpec>,
    /// Gate delle prove HTTP funzionali (`agent.final_gate.endpoint_check_enabled`,
    /// mig 0455, default true). OFF -> nessun criterio `http`, ne' configurato ne'
    /// dichiarato, e nessuna dichiarazione di verifica funzionale mancante: il
    /// gate torna al comportamento storico.
    pub endpoint_check_enabled: bool,
    /// Timeout (s) di UNA chiamata HTTP del gate
    /// (`agent.final_gate.endpoint_timeout_seconds`, mig 0455, default 15).
    pub endpoint_timeout_s: f64,
    /// Origine del servizio FRONTEND del progetto (`http://host:porta`), quando
    /// ne esiste uno con una porta allocata. `None` = nessun frontend, o porta
    /// non risolvibile: non si prova nulla, mai un host indovinato.
    ///
    /// Serve a provare gli endpoint dichiarati ANCHE attraverso il frontend
    /// (`decisions::endpoint_probes::criteri_integrazione_frontend`): due
    /// servizi sani che non si parlano non sono un'applicazione, ed e'
    /// esattamente cio' che il gate lasciava passare.
    pub origine_frontend: Option<String>,
    /// P5: gate design_verify abilitato (agent.final_gate.design_verify_enabled,
    /// default true). Si applica SOLO se nella history c'e' un nexus_visual_compare
    /// (task figma): None = non-figma -> non blocca.
    pub design_verify_enabled: bool,
    /// P5: soglia minima di similarity_score (0-100) per chiudere un task figma
    /// (agent.final_gate.design_verify_min_score, default 70).
    pub design_verify_min_score: i64,
    /// ADR 0018 leva 3: criteri STRUTTURALI `action_requested` /
    /// `tool_capability` / `completion_confirmed`
    /// (`agent.final_gate.structural_criteria_enabled`, default true, mig 0503).
    pub structural_criteria_enabled: bool,
    /// Criterio docs claim-vs-fatti (`agent.final_gate.docs_criterion_enabled`,
    /// mig 0676). Default Rust FALSE: a DB muto un criterio nuovo non boccia.
    pub docs_criterion_enabled: bool,
    /// Glob dei file di documentazione (`agent.final_gate.docs_globs`,
    /// separatore `;`, es. `README*;docs/**`): il claim `updated` pretende
    /// almeno un file toccato che vi corrisponda.
    pub docs_globs: Vec<String>,
    /// Osservazione del dialogo frontend<->backend da browser reale
    /// (`agent.final_gate.browser_dialogue_enabled`, mig 0681). Default Rust
    /// FALSE: a DB muto un criterio nuovo non boccia (stesso rollout di
    /// `docs_criterion_enabled`).
    pub browser_dialogue_enabled: bool,
    /// Prefissi di URL esterni al progetto (CDN, font, telemetria) che non
    /// contano come difetto d'integrazione
    /// (`agent.final_gate.browser_third_parties`, separatore `;`).
    pub browser_third_parties: Vec<String>,
    /// Millisecondi di attesa che la rete si calmi dopo il primo render: le
    /// chiamate dati partono dopo, e osservare troppo presto vedrebbe una
    /// pagina che non ha ancora chiesto nulla
    /// (`agent.final_gate.browser_settle_ms`).
    pub browser_settle_ms: u64,
    /// Criterio «lo stile che il codice DICHIARA e' applicato?», gia' costruito
    /// dal motore. `None` = criterio spento o progetto senza radice: non nasce.
    ///
    /// Arriva PRONTO invece di essere costruito qui perche' il criterio puro
    /// vive in `nexus-agent-tools` (`ui_styling`), che questo crate non vede: e'
    /// lo stesso trattamento di `endpoint_criteria`, risolti a monte. Riscriverne
    /// una copia qui per soddisfare il grafo delle dipendenze sarebbe la seconda
    /// definizione dello stesso criterio (regola L).
    pub ui_styling_criterion: Option<crate::runtime::ports::CriterionSpec>,
    /// Criterio «l'app SENZA server mostra davvero il suo contenuto?», gia'
    /// costruito dal motore. `None` = criterio spento (modalita' `off`) oppure
    /// nessuna radice di anteprima configurata.
    ///
    /// NON dipende piu' dal fatto che il progetto SIA un'app statica, e non e'
    /// un dettaglio: quella natura veniva decisa a t=0, prima che il run
    /// scrivesse alcunche', quindi su un progetto nuovo il criterio non nasceva
    /// mai (misurato l'11/08/2026: pagina rotta, run chiuso «task complete»).
    /// La pagina — e con lei la precedenza del servizio — si risolve al momento
    /// della VERIFICA, col punto unico `decisions::pagina_del_run`; dove non si
    /// applica, il criterio lo DICHIARA nell'evidenza invece di sparire, e non
    /// declassa nessuno (un progetto senza interfaccia resta una risposta, non
    /// un inconcludente).
    ///
    /// Arriva PRONTO per la stessa ragione di `ui_styling_criterion`: la radice
    /// degli indirizzi di anteprima e la configurazione della misura stanno nel
    /// DB, che questo crate non legge. Il nodo vi aggiunge la sola parte che
    /// conosce lui — il contenitore DICHIARATO dall'agente.
    pub static_render_criterion: Option<crate::runtime::ports::CriterionSpec>,
    /// Criterio «i file di codice che questo run ha PRODOTTO si caricano nel
    /// loro runtime?», gia' costruito dal motore. `None` = criterio spento.
    ///
    /// E' il caso BASE della famiglia, e mancava. Gli altri quattro chiedono a
    /// un servizio, a un browser, a una suite o ai sorgenti dell'interfaccia:
    /// nessuno chiedeva se un file di codice PARTA. MISURATO il 17/08/2026 su un
    /// progetto vuoto — `calcolatrice.test.js` con sintassi Jest senza Jest
    /// (`ReferenceError: describe is not defined`), nessuna porta registrata,
    /// quindi nessuno degli altri criteri applicabile, e il gate ha chiuso
    /// «passato» due volte col beneficio del dubbio.
    ///
    /// Arriva PRONTO per la stessa ragione degli altri due qui sopra: il
    /// vocabolario dei runtime e i parametri della misura stanno nel DB, che
    /// questo crate non legge. Il nodo non vi aggiunge nulla — QUALI file
    /// provare non e' una domanda che si possa porre a t=0, e la pone chi
    /// verifica al registro delle scritture.
    pub codice_eseguibile_criterion: Option<crate::runtime::ports::CriterionSpec>,
    /// Criterio «le PROVE che questo run ha dichiarato sono superate?»
    /// (mig 0737), gia' costruito dal motore. `None` = criterio spento.
    ///
    /// E' l'unico criterio il cui contenuto non e' cablato: gli altri sette
    /// portano una domanda decisa a monte, questo porta le prove che il
    /// Consiglio e l'agente hanno EMESSO per QUESTO task. Il catalogo delle
    /// domande cablate e' incompleto per costruzione — MISURATO il 17/08/2026,
    /// dove su un progetto senza porte il gate non aveva niente da chiedere e
    /// ha chiuso «passato» due volte su un test non eseguibile.
    ///
    /// Arriva PRONTO per la stessa ragione degli altri tre qui sopra: la
    /// politica di ammissione e i parametri della misura stanno nel DB, che
    /// questo crate non legge. Il nodo vi aggiunge la sola parte che conosce —
    /// il PIANO, che a t=0 non esiste ancora: i panel non hanno deliberato e
    /// l'agente non ha dichiarato niente.
    pub piano_verifica_criterion: Option<crate::runtime::ports::CriterionSpec>,
    /// ADR 0036: catena di verifica PER-AMBIENTE risolta a monte (profilo
    /// inferito da LLM in `project_verify_profiles`, step marcati gate=true).
    /// Un criterio `run_command` per step, nell'ordine del profilo (es.
    /// typecheck poi build — per un progetto Vite la sola build non
    /// type-checka e chiudeva "verificato" codice rotto a runtime). NESSUN
    /// comando generico di ripiego (decisione utente): vuota = nessun
    /// criterio comando.
    pub verify_steps: Vec<VerifyStepCmd>,
    /// `true` quando il profilo di verifica NON e' disponibile (mai inferito
    /// e LLM irraggiungibile): il gate lo DICHIARA nella narrazione live
    /// invece di verificare con comandi generici (esito onesto, regola M).
    pub verify_profile_missing: bool,
    /// ESCALATION su non-convergenza del gate (`agent.final_gate.
    /// escalate_on_nonconvergence`, default true). Quando il gate esaurisce
    /// `max_cycles` con criteri OGGETTIVI ancora falliti — un modello scadente
    /// che non ripara il codice entro i suoi tentativi — invece di chiudere
    /// secco `FailedDiagnosed` cede il turno all'executor per PROMUOVERE a un
    /// modello piu' capace (punto unico `maybe_escalate_nonconvergence`, regola
    /// L). Bound da `auto_escalations < max_escalations` (anti-loop): esaurite le
    /// promozioni il gate chiude come prima (backstop invariato). OFF ->
    /// comportamento storico bit-identico (chiusura secca al cap).
    pub escalate_on_nonconvergence: bool,
    /// Tetto di escalation cumulative del run (`agent.executor.max_escalations`,
    /// riusa la stessa chiave dell'executor: e' lo STESSO budget `auto_escalations`
    /// condiviso, regola L/G). Il gate lo usa solo per decidere se cedere il turno:
    /// oltre il tetto chiude secco (l'executor rifiuterebbe comunque via
    /// `maybe_escalate_nonconvergence`, ma evitiamo il giro a vuoto).
    pub max_escalations: i64,
}

/// Comando di UNO step della catena di verifica per-ambiente (ADR 0036),
/// gia' risolto e validato a monte (regola G: il nodo non legge DB/LLM).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifyStepCmd {
    /// Etichetta canonica: typecheck | build | lint | test.
    pub step: String,
    /// Comando completo, gia' passato dalla safety dei comandi.
    pub command: String,
    /// Working dir relativa alla root del progetto (monorepo). `None` = root.
    pub working_dir: Option<String>,
    /// Exit code del comando misurato sull'albero PRE-LAVORO (all'innesto del
    /// profilo, prima che il run tocchi file). Baseline del gate delta-aware
    /// sui criteri: un criterio che fallisce ORA con lo STESSO exit code
    /// non-zero della baseline e senza file d'errore localizzati e' un
    /// fallimento PRE-ESISTENTE dell'ambiente (es. `npx eslint` exit 2 per
    /// config assente), non una regressione del run: non boccia, viene
    /// dichiarato. `None` = baseline non misurata -> fail-closed (criterio
    /// assoluto, comportamento storico).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_exit_code: Option<i64>,
}

impl Default for FinalGateConfig {
    fn default() -> Self {
        // Default IDENTICI ai safe-default del brain (orchestrator_config.py +
        // i default delle funzioni `_resolve_*`). Valgono SOLO se il DB e'
        // irraggiungibile, mai come magic fallback nella logica.
        Self {
            enabled: true,
            max_cycles: 2,
            runtime_check_enabled: true,
            build_timeout_s: 180.0,
            build_output_max_chars: 4000,
            log_command: String::new(),
            runtime_error_patterns: Vec::new(),
            no_orphan_min_ratio: 0.4,
            import_staging_dirs: vec!["figma_export".to_string()],
            criteria_timeout_s: 30.0,
            endpoint_criteria: Vec::new(),
            endpoint_check_enabled: true,
            endpoint_timeout_s: 15.0,
            design_verify_enabled: true,
            design_verify_min_score: 70,
            structural_criteria_enabled: true,
            // FALSE a DB muto: il seed (mig 0676) lo accende.
            docs_criterion_enabled: false,
            docs_globs: vec!["README*".to_string(), "docs/**".to_string()],
            // FALSE a DB muto, come sopra: il seed (mig 0681) lo accende.
            browser_dialogue_enabled: false,
            browser_third_parties: Vec::new(),
            browser_settle_ms: 2000,
            // Nessun criterio a DB muto: lo costruisce il motore quando la
            // chiave e' accesa e il progetto ha una radice.
            ui_styling_criterion: None,
            static_render_criterion: None,
            codice_eseguibile_criterion: None,
            piano_verifica_criterion: None,
            verify_steps: Vec::new(),
            verify_profile_missing: false,
            origine_frontend: None,
            // Trigger di escalation su non-convergenza del gate ON di default
            // (il gap che chiudeva secco un modello scadente). max_escalations
            // = safe-default dell'executor (3): stesso budget condiviso.
            escalate_on_nonconvergence: true,
            max_escalations: 3,
        }
    }
}

/// Estrae una `&str` da un campo evidence con la semantica `or` FALSY di Python
/// (`final_gate.py:421`): ritorna `Some(s)` SOLO se il campo e' una stringa NON
/// vuota; una stringa vuota `""` (falsy in Python) o un tipo diverso/assente
/// ritornano `None`, cosi' il chiamante cade sul campo successivo della catena.
/// Senza questo, `Some("")` interromperebbe il fallback (divergenza dal Python).
/// Il titolo del meta-step quando il gate chiude col PASS: dichiara quanti
/// criteri non si sono potuti misurare, invece di tacerli.
///
/// "Verifica superata" davanti a criteri non misurati e' precisamente la frase
/// che il caso `Inconclusive` esiste per non far dire piu': il numero, nel
/// titolo, e' cio' che distingue una verifica CONCLUSA da una verifica in cui
/// una parte non e' stata guardata.
/// Il campo con cui un criterio dichiara il proprio verdetto nell'evidenza.
/// Scritto una volta: e' la chiave che i due sunti del payload rileggono.
const CAMPO_VERDICT: &str = "verdict";

fn titolo_verifica_superata(inconclusive: usize) -> String {
    if inconclusive == 0 {
        return "Verifica superata".to_string();
    }
    format!("Verifica superata sui criteri misurabili ({inconclusive} non misurabili)")
}

fn str_truthy(v: Option<&Value>) -> Option<&str> {
    match v.and_then(Value::as_str) {
        Some(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

/// Estrae un `i64` da un campo evidence con la semantica `or` FALSY di Python
/// (`final_gate.py:435`: `output_total_chars or len(text)`): ritorna `Some(n)`
/// SOLO se il campo e' un intero NON-zero; lo `0` (falsy in Python), un tipo
/// diverso o l'assenza ritornano `None`, cosi' il chiamante cade sul default
/// (`len(text)`). Senza questo, `Some(0)` verrebbe mantenuto (divergenza).
fn i64_truthy(v: Option<&Value>) -> Option<i64> {
    match v.and_then(Value::as_i64) {
        Some(n) if n != 0 => Some(n),
        _ => None,
    }
}

/// Conta occorrenze grezze di errori in un output di build (TS/Rust/...).
/// Replica 1:1 `_count_build_errors` (`final_gate.py:285-294`): somma i match
/// di tutti i [`BUILD_ERROR_PATTERNS`]. Indicativo: comunica all'agente quanti
/// errori deve risolvere (non solo il primo). 0 se l'output e' vuoto o nessun
/// pattern matcha.
pub fn count_build_errors(output: &str) -> usize {
    if output.is_empty() {
        return 0;
    }
    BUILD_ERROR_PATTERNS
        .iter()
        .map(|pat| pat.find_iter(output).count())
        .sum()
}

/// Regex che catturano il FILE di un errore di compilazione dai formati con
/// localizzazione esplicita (best-effort, come [`count_build_errors`]). Gruppo 1
/// = path del file. Coprono tsc, rustc/cargo, eslint (stylish + compact) e
/// vite/rollup/esbuild — i formati usati dai profili di verifica reali (il
/// profilo standard e' `npx eslint` + `pnpm build`). I formati ANCORA non
/// coperti non contribuiscono qui: [`build_error_files`] ritorna un set VUOTO e
/// il chiamante ricade sul criterio assoluto (fail-closed, mai fail-open).
///
/// Nota anti-falso-positivo: i pattern che localizzano su una singola riga
/// (tsc/rustc/compact/esbuild) matchano SOLO righe con `error` (non `warning`),
/// perche' solo un errore fa fallire il gate. Il pattern eslint stylish, dove il
/// path e' su una riga a se' seguita da righe indentate, cattura il path solo se
/// dopo gli eventuali `warning` compare almeno un `error` (un file con soli
/// warning non e' una regressione).
static BUILD_ERROR_FILE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // tsc: `src/foo.tsx(12,3): error TS1234:` -> path = tutto prima della `(`.
        Regex::new(r"(?im)^\s*([^\n(]+?)\(\d+,\d+\):\s+error\s+TS\d+")
            .expect("regex tsc file valida"),
        // rustc/cargo: `  --> src/foo.rs:12:5` -> path prima di `:riga:col`.
        Regex::new(r"(?m)^\s*-->\s+(.+?):\d+:\d+").expect("regex rustc file valida"),
        // eslint stylish (default): il path e' una riga a se' (non indentata) che
        // termina con un'estensione sorgente, seguita da righe `  riga:col  livello`.
        // Cattura il path solo se, dopo zero o piu' `warning`, arriva un `error`.
        Regex::new(
            r"(?m)^(\S[^\n]*\.(?:tsx?|jsx?|vue|svelte|mjs|cjs))\r?\n(?:[ \t]+\d+:\d+[ \t]+warning[^\n]*\r?\n)*[ \t]+\d+:\d+[ \t]+error\b",
        )
        .expect("regex eslint stylish valida"),
        // eslint compact (`-f compact`): `src/foo.js: line 1, col 1, Error - msg`.
        Regex::new(r"(?im)^(.+?):\s+line\s+\d+,\s+col\s+\d+,\s+Error\b")
            .expect("regex eslint compact valida"),
        // vite/rollup resolve: `Could not resolve "./x" from "src/foo.ts"` /
        // `Rollup failed to resolve import "x" from "src/foo.ts"` -> path = `from`.
        Regex::new(
            r#"(?im)(?:could not resolve|failed to resolve import)\s+["'][^"'\n]*["']\s+from\s+["']([^"'\n]+)["']"#,
        )
        .expect("regex rollup resolve valida"),
        // esbuild/vite generico: `src/foo.ts:12:34: ERROR: msg` (path senza `:`
        // interni per non spezzare i drive Windows su `file:riga:col: error`).
        Regex::new(r"(?im)^\s*([^\s:][^\n:]*?):\d+:\d+:\s+error\b")
            .expect("regex esbuild valida"),
    ]
});

/// Normalizza un path per il confronto cross-piattaforma: separatori `/`, senza
/// prefisso `./`, trimmato. NON lowercasa (Linux e' case-sensitive).
fn normalize_path(p: &str) -> String {
    p.trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string()
}

/// Estrae il set dei FILE che presentano errori nell'output di un comando di
/// verifica (tsc/rustc), con path normalizzati. Best-effort: un set VUOTO
/// significa "nessuna localizzazione ricavabile" (formato non coperto o output
/// pulito) — il chiamante NON deve dedurne "nessun errore" (per quello c'e'
/// [`count_build_errors`]), ma ricadere sul criterio assoluto (fail-closed).
pub fn build_error_files(output: &str) -> std::collections::BTreeSet<String> {
    let mut files = std::collections::BTreeSet::new();
    if output.is_empty() {
        return files;
    }
    for pat in BUILD_ERROR_FILE_PATTERNS.iter() {
        for cap in pat.captures_iter(output) {
            if let Some(m) = cap.get(1) {
                let norm = normalize_path(m.as_str());
                if !norm.is_empty() {
                    files.insert(norm);
                }
            }
        }
    }
    files
}

/// True se il file `error_file` (dall'output di un build) e' lo STESSO file
/// `touched_file` (modificato dal run), robusto a root/prefissi diversi: match
/// esatto o suffisso a CONFINE DI SEGMENTO (`a/b/x.ts` vs `x.ts`, o vs
/// `/abs/a/b/x.ts`). Il confine `/` evita che `Page.tsx` matchi `LoginPage.tsx`.
pub fn error_file_matches_touched(error_file: &str, touched_file: &str) -> bool {
    let e = normalize_path(error_file);
    let t = normalize_path(touched_file);
    if e.is_empty() || t.is_empty() {
        return false;
    }
    e == t || e.ends_with(&format!("/{t}")) || t.ends_with(&format!("/{e}"))
}

/// Esito dei criteri di verifica, in un tipo che porta anche i BUCHI.
///
/// Tre casi e non due, perche' «nessuno ha bocciato» e «tutti hanno passato»
/// sono affermazioni diverse: la prima e' vera anche quando non si e' misurato
/// nulla. Un `bool` le confonde, ed e' cosi' che gate generali tutti
/// inconcludenti chiudevano un todo come `Completed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EsitoCriteri {
    /// Almeno un criterio misurato, nessuno bocciato, nessun buco.
    Superato,
    /// Nessun criterio bocciato, ma la verifica ha dei buchi: criteri
    /// inconcludenti, o nessun criterio misurabile affatto. Non e' una prova di
    /// difetto (non boccia) e non e' una prova di correttezza: chi chiude su
    /// questo lo fa sapendo che la chiusura non e' VERIFICATA.
    SuperatoNonVerificato { inconclusive: usize },
    /// Almeno un criterio misurato ha fallito.
    Fallito,
}

/// Nodo final_gate. Stateless: legge lo stato + la config passata + la
/// `RoutingConfig` (per `is_software_task`/lista mutator). I criteri sono
/// eseguiti dietro il trait [`crate::runtime::ports::CriteriaRunner`], iniettato
/// nel costruttore (dipendenza propria del nodo, NON nel ctx: minimizza
/// l'impatto sugli altri nodi). La config DB-driven e' risolta A MONTE (regola
/// G); la decision machine + rendering e' interamente qui.
pub struct FinalGateNode {
    /// Config DB-driven del gate (regola G: passata, mai letta dal nodo).
    cfg: FinalGateConfig,
    /// Config di routing: serve a `signals::is_software_task` (lista
    /// mutator-tools DB-driven + whitelist intent), punto unico (regola L).
    routing_cfg: RoutingConfig,
    /// Motore criteri (sotto-sistema delegato). mcp-core lo implementera' col
    /// ToolRunner gRPC; nei test e' stubato.
    criteria: std::sync::Arc<dyn crate::runtime::ports::CriteriaRunner>,
    /// Persistenza dei meta-step di fase `final_gate` (narrazione live in chat:
    /// "Verifico il risultato" / esito). Pattern emit+persist, punto unico
    /// [`crate::nodes::emit_phase_meta`] (regola L).
    meta_steps: std::sync::Arc<dyn crate::runtime::ports::MetaStepStore>,
}

impl FinalGateNode {
    /// Costruisce il nodo con la config DB-driven gia' risolta dal chiamante e
    /// il motore criteri concreto (o stub nei test).
    pub fn new(
        cfg: FinalGateConfig,
        routing_cfg: RoutingConfig,
        criteria: std::sync::Arc<dyn crate::runtime::ports::CriteriaRunner>,
        meta_steps: std::sync::Arc<dyn crate::runtime::ports::MetaStepStore>,
    ) -> Self {
        Self {
            cfg,
            routing_cfg,
            criteria,
            meta_steps,
        }
    }

    /// Costruisce le spec dei criteri da eseguire (`final_gate.py:400-470`).
    /// PURA: nessun I/O. L'ordine e' load-bearing (riprodotto 1:1):
    /// `no_orphan_imported`, `outputs_exist`, poi opzionali `service_logs_clean`
    /// (se runtime_check_enabled + log_command non vuoto), `run_command`-build
    /// (se build_command presente) e infine i criteri `http`-endpoint: quelli
    /// CONFIGURATI nel progetto (risolti a monte) e quelli DICHIARATI dall'agente
    /// in `task_complete.endpoints` (ADR 0034), nell'ordine di dichiarazione.
    ///
    /// COPERTURA: questo metodo e' verificato dagli unit test Rust
    /// (`build_criteria_ordine_e_opzionali`), NON dal golden cross-language. Il
    /// golden e' deliberatamente DB-free, ma il LATO Python di questa costruzione
    /// e' inline nella coroutine async `_run_final_gate_criteria` e i suoi pezzi
    /// opzionali derivano da `_resolve_log_command`/`_resolve_build_command`/
    /// `_build_timeout_s`/`_build_output_max_chars` — tutte funzioni che aprono
    /// connessioni DB (`brain/agents/final_gate.py:78,186,234,249`). Estrarre
    /// l'assemblaggio in una funzione pura lato Python (per chiamarlo dallo
    /// script golden senza I/O) significherebbe o modificare il brain, o replicare
    /// la risoluzione config nello script — un secondo punto di verita' che
    /// diverge dal Python reale (anti-pattern regola L/H). Qui la config arriva
    /// gia' RISOLTA in [`FinalGateConfig`] (regola G), quindi `build_criteria` e'
    /// puro assemblaggio della lista: l'unit test Rust ne copre ordine, opzionali
    /// e contenuto spec senza bisogno della parita' cross-language.
    pub fn build_criteria(&self, state: &AgentState) -> Vec<CriterionSpec> {
        let mut criteria: Vec<CriterionSpec> = Vec::new();

        // (1) no_orphan_imported (anti-placeholder), sempre.
        criteria.push(CriterionSpec {
            provenance: CriterionProvenance::Gate,
            criterion_type: "no_orphan_imported".to_string(),
            spec: json!({
                "staging_dir": self.cfg.import_staging_dirs,
                "min_reached_ratio": self.cfg.no_orphan_min_ratio,
            }),
            expected: json!({ "mounted": true }),
            timeout_s: None,
        });

        // (2) outputs_exist (claim-vs-fatti), sempre. run_id = thread_id.
        let run_id = state.thread_id.clone().unwrap_or_default();
        criteria.push(CriterionSpec {
            provenance: CriterionProvenance::Gate,
            criterion_type: "outputs_exist".to_string(),
            spec: json!({ "run_id": run_id }),
            expected: json!({}),
            timeout_s: None,
        });

        // (3) service_logs_clean (verifica runtime E2E), se abilitato e c'e' un
        //     comando log risolto a monte.
        if self.cfg.runtime_check_enabled && !self.cfg.log_command.is_empty() {
            criteria.push(CriterionSpec {
                provenance: CriterionProvenance::Gate,
                criterion_type: "service_logs_clean".to_string(),
                spec: json!({
                    "command": self.cfg.log_command,
                    "patterns": self.cfg.runtime_error_patterns,
                }),
                expected: json!({}),
                timeout_s: None,
            });
        }

        // (4) Verifica che il codice sia SANO per il SUO ambiente (ADR 0036):
        //     un criterio run_command per ogni step gate=true del profilo
        //     inferito dall'LLM, nell'ordine del profilo. Il fallimento di uno
        //     step espone all'agente comando e output di QUELLO step. NESSUN
        //     comando generico di ripiego (decisione utente): senza profilo
        //     nessun criterio comando, con dichiarazione onesta nel run()
        //     (l'incidente Beaty-Book nasceva dal "npm run build" cieco che
        //     per Vite non type-checka). max_output_chars dedicato (mig 0426)
        //     + timeout build per ciascun comando.
        // Gate DELTA-aware (regola H, causa del blocco su debito preesistente):
        // i file toccati dal run. Il criteria_runner conta come REGRESSIONE (che
        // fallisce il gate) solo un errore di build che colpisce uno di questi
        // file; gli errori in file NON toccati sono debito preesistente del
        // progetto e non impediscono la chiusura di un task che non li ha
        // introdotti (es. login task bocciato da errori di tipo in BookingPage.tsx).
        // Calcolato una volta (punto unico signals::touched_files_in_history,
        // regola L) e passato in ogni spec run_command.
        let touched_files: Vec<String> =
            signals::touched_files_in_history(&state.messages, &self.routing_cfg)
                .into_iter()
                .collect();

        for vs in &self.cfg.verify_steps {
            let mut spec = serde_json::Map::new();
            spec.insert("command".to_string(), json!(vs.command));
            spec.insert("label".to_string(), json!(vs.step));
            spec.insert(
                "max_output_chars".to_string(),
                json!(self.cfg.build_output_max_chars),
            );
            spec.insert("touched_files".to_string(), json!(touched_files));
            if let Some(cwd) = &vs.working_dir {
                spec.insert("working_dir".to_string(), json!(cwd));
            }
            // Baseline pre-lavoro dello step (delta-aware sui criteri): il
            // criteria_runner non boccia un fallimento IDENTICO alla baseline.
            if let Some(be) = vs.baseline_exit_code {
                spec.insert("baseline_exit_code".to_string(), json!(be));
            }
            criteria.push(CriterionSpec {
                provenance: CriterionProvenance::Gate,
                criterion_type: "run_command".to_string(),
                spec: Value::Object(spec),
                expected: json!({ "exit_code": 0 }),
                timeout_s: Some(self.cfg.build_timeout_s),
            });
        }

        // (5) http-endpoint: chiamate REALI agli endpoint che il task doveva far
        //     funzionare. DUE fonti, entrambe accodate qui nell'ordine
        //     "configurato prima, dichiarato poi": la configurazione per-progetto
        //     risolta a monte (regola G) e la DICHIARAZIONE dell'agente
        //     (`task_complete.endpoints`, ADR 0034), tradotta dal punto unico
        //     [`crate::decisions::endpoint_probes`] (regola L: qui non si
        //     ri-decide come si prova un endpoint).
        //
        //     Perche' servono entrambe: senza la configurazione un progetto che
        //     l'ha compilata non verrebbe provato; senza la dichiarazione — il
        //     caso NORMALE, perche' quella configurazione e' manuale e quasi
        //     nessuno la compila — non verrebbe provato NIENTE. E' esattamente
        //     cosi' che il gate ha chiuso "superato" su un'app la cui POST
        //     rispondeva 500 (gestione-spese, 2026-07-28): la GET, l'unica che
        //     l'agente aveva provato da se', rispondeva 200.
        if self.cfg.endpoint_check_enabled {
            criteria.extend(self.cfg.endpoint_criteria.iter().cloned());
            criteria.extend(
                crate::decisions::endpoint_probes::endpoint_criteria_from_declaration(
                    state.declared_outcome.as_ref(),
                    self.cfg.endpoint_timeout_s,
                ),
            );
            // Gli STESSI endpoint, ma attraverso il frontend: e' la sola prova
            // che i due servizi si parlino. Backend sano + frontend servito +
            // proxy mal configurato = due processi vivi e nessuna applicazione,
            // ed era verde (inventario-magazzino, 2026-08-04).
            criteria.extend(
                crate::decisions::endpoint_probes::criteri_integrazione_frontend(
                    state.declared_outcome.as_ref(),
                    self.cfg.origine_frontend.as_deref(),
                    self.cfg.endpoint_timeout_s,
                ),
            );
        }

        // (5c) Il DIALOGO osservato da un browser reale. Non duplica la prova
        //      d'integrazione qui sopra: quella chiede al server, con reqwest,
        //      gli endpoint che l'agente ha DICHIARATO; questa carica la pagina
        //      e guarda cosa chiede DAVVERO. La differenza non e' di grado —
        //      reqwest non manda `Origin` e non esegue JS, quindi CORS assente
        //      e URL costruito male gli sono invisibili per costruzione. Sono
        //      le due cause che hanno fatto dichiarare completa quattro volte
        //      un'app in cui nel browser falliva ogni chiamata
        //      (biblioteca-scolastica, misurato il 06/08/2026).
        //      Senza origine frontend il criterio non nasce: un progetto
        //      senza interfaccia non ha questo dialogo da misurare.
        if self.cfg.browser_dialogue_enabled {
            let criterio = crate::decisions::browser_dialogue::criterio_dialogo(
                self.cfg.origine_frontend.as_deref(),
                self.cfg.endpoint_timeout_s,
                &self.cfg.browser_third_parties,
                self.cfg.browser_settle_ms,
            );
            // Un criterio ACCESO che non nasce e' indistinguibile, nei fatti,
            // da uno spento: e' la forma di difetto che questo criterio esiste
            // per chiudere, e non deve valere per lui stesso. Se manca
            // l'origine si dichiara PERCHE' — l'operatore vede che il gate ha
            // rinunciato a guardare, invece di un silenzio che somiglia a un
            // «tutto a posto».
            if criterio.is_none() {
                // Il testo NON afferma piu' una causa che qui non si conosce.
                // Fino al 09/08/2026 diceva «il progetto non ha un servizio
                // frontend con porta allocata», e su gestione-corsi era falso:
                // il frontend c'era, vivo e con la sua porta: a non riconoscerlo
                // era il criterio di risoluzione dell'origine, che cercava la
                // parola «frontend» nella label. Chi leggeva il log andava a
                // cercare un servizio mancante. Il verdetto con la sua causa lo
                // dichiara chi l'origine la risolve (punto unico
                // `decisions::origine_frontend`, log in
                // `mcp_core::native_engine`).
                tracing::info!(
                    target: "nexus_agent_graph::final_gate",
                    "dialogo browser non misurato: nessuna origine frontend risolta \
                     (il verdetto e la sua causa sono nel log di origine_frontend)"
                );
            }
            criteria.extend(criterio);
        }

        // (5d) Lo STILE dichiarato dal codice ha una fonte che lo applica?
        //      Non e' un giudizio di gusto — «bello» non e' un criterio — ma un
        //      fatto: o esiste qualcosa che rende quelle classi, o non esiste.
        //      La lente c'era gia', completa e con i suoi test, e non toccava la
        //      chiusura di un run: era un tool offerto a due figure. MISURATO il
        //      06/08/2026 su agenda-medica, dove il tool era perfino stato
        //      CHIAMATO — Tailwind scritto nei componenti, in package.json, e
        //      nessuna config, nessun .css, nessun import: pagina grezza, run
        //      «completato». Interrogarla qui e' la differenza fra una misura
        //      costruita e una in esercizio.
        criteria.extend(self.cfg.ui_styling_criterion.clone());

        // (5e) L'app SENZA server MOSTRA il proprio contenuto? Il criterio (5c)
        //      non copre questo caso e non per svista: e' costruito attorno a
        //      un'ORIGINE HTTP, e un'app statica non ha un servizio a cui
        //      chiedere. Qui il contenuto non arriva dalla rete — lo genera il
        //      JavaScript della pagina — quindi l'unico modo di sapere se e'
        //      arrivato e' aprirla e guardare il DOM dopo che ha girato.
        //      MISURATO l'08/08/2026 su gestione-corsi: `landing/index.html`
        //      approvata al terzo tentativo guardando i FILE, con le sei card
        //      generate all'avvio da una funzione in fondo allo script. La
        //      pagina era corretta; un `throw` prima di quella chiamata avrebbe
        //      dato lo stesso file valido e una griglia vuota, e nessun criterio
        //      attivo distingueva i due casi.
        //      Il contenitore DICHIARATO si innesta qui perche' e' l'unica parte
        //      che il nodo conosce (sta nello stato del run); il resto del
        //      criterio arriva gia' risolto dal motore.
        criteria.extend(self.cfg.static_render_criterion.clone().map(|c| {
            crate::decisions::static_render::con_contenitore(c, state.declared_outcome.as_ref())
        }));

        // (5f) IL CASO BASE: il codice che questo run ha PRODOTTO si carica?
        //      I quattro criteri qui sopra chiedono a un servizio, a un browser,
        //      a una suite o ai sorgenti dell'interfaccia. Nessuno chiedeva se un
        //      file di codice PARTA — e su un progetto senza porte, senza pagina
        //      e senza suite non ne nasce nemmeno uno. MISURATO il 17/08/2026:
        //      task «una calcolatrice e i suoi test», `calcolatrice.test.js` con
        //      sintassi Jest in un progetto senza Jest, gate «passato» due volte
        //      (cycle=2 inconclusive=2, poi cycle=1 inconclusive=2) e run chiuso
        //      «completed». Il Consiglio aveva emesso il rischio esatto prima che
        //      il lavoro cominciasse; mancava chi lo riscontrasse alla fine.
        //      NIENTE si innesta qui, a differenza della resa: quali file provare
        //      e' una domanda che a t=0 non ha risposta e che al momento della
        //      verifica si pone al registro delle scritture, non allo stato.
        criteria.extend(self.cfg.codice_eseguibile_criterion.clone());

        // (5g) LE PROVE CHE QUESTO RUN HA DICHIARATO. I sei criteri qui sopra
        //      sono un CATALOGO, e un catalogo di domande cablate e' incompleto
        //      per costruzione: nessuna lista conterra' mai «crea un libro via
        //      POST, rileggilo via GET, controlla che sia nella tabella e
        //      cancellalo». Quella prova la sa scrivere solo chi conosce il
        //      task — e il sistema la sa gia' scrivere: per il caso del
        //      17/08/2026 il Consiglio aveva emesso 17 requisiti, ma in PROSA, e
        //      il riscontro ha potuto dire soltanto «applicati=0,
        //      non_applicati=2, non_verificabili=15».
        //      Il PIANO si compone QUI perche' e' l'unico punto in cui lo stato
        //      del run e' visibile: le prove dei panel advisory viaggiano
        //      nell'`extra` (la stessa chiave con due scrittori dei requisiti) e
        //      quelle dell'agente nella sua dichiarazione di chiusura. L'ordine
        //      e' load-bearing e incarna «giudice != worker»: chi non ha scritto
        //      il codice viene prima, e l'esecutore puo' solo AGGIUNGERE.
        criteria.extend(
            self.cfg
                .piano_verifica_criterion
                .clone()
                .map(|c| Self::criterio_col_piano(c, state)),
        );

        // (6) design_verify (P5): per i task figma l'agente non puo' chiudere con
        //     una resa visiva sotto soglia che HA GIA' misurato con nexus_visual_compare.
        //     Deterministico: prende l'ultimo similarity_score dalla history (niente
        //     vision nel gate). None (nessun confronto) = task non-figma -> non blocca.
        if self.cfg.design_verify_enabled {
            let mut last_score: Option<i64> = None;
            for m in &state.messages {
                if let Message::Tool { content, .. } = m {
                    if let Ok(v) = serde_json::from_str::<Value>(content.flatten_text().trim()) {
                        if let Some(sc) = v.get("similarity_score").and_then(Value::as_i64) {
                            last_score = Some(sc);
                        }
                    }
                }
            }
            if let Some(score) = last_score {
                criteria.push(CriterionSpec {
                    provenance: CriterionProvenance::Gate,
                    criterion_type: "design_verify".to_string(),
                    spec: json!({
                        "similarity_score": score,
                        "min_score": self.cfg.design_verify_min_score,
                    }),
                    expected: json!({ "min_score": self.cfg.design_verify_min_score }),
                    timeout_s: None,
                });
            }
        }

        // (7) Criteri STRUTTURALI (ADR 0018 leva 3): i FATTI macchina vengono
        //     dallo stato QUI (come design_verify), i check restano PURI nel
        //     criteria_runner (regola M: mai dedotti dalla prosa). Kill-switch
        //     DB-driven `agent.final_gate.structural_criteria_enabled` (mig 0503).
        if self.cfg.structural_criteria_enabled {
            let action_oriented = state.action_oriented.unwrap_or(false);
            // action_requested: una richiesta d'azione chiusa senza NESSUNA
            // azione produttiva in history non puo' passare il gate.
            criteria.push(CriterionSpec {
                provenance: CriterionProvenance::Gate,
                criterion_type: "action_requested".to_string(),
                spec: json!({
                    "action_oriented": action_oriented,
                    "has_productive_action":
                        signals::has_productive_action_in_history(&state.messages),
                }),
                expected: json!({ "acted": true }),
                timeout_s: None,
            });
            // tool_capability: un task software con ZERO tool esposti e senza
            // alcuna tool call gia' osservata e' una misconfigurazione
            // (catalogo/whitelist), non colpa del task. Se la history contiene
            // tool_use, il catalogo c'era al momento dell'azione: non bocciare
            // per un `tools_json` non propagato al gate/resume.
            let tools_count = state.tools_json.as_ref().map(|t| t.len()).unwrap_or(0);
            criteria.push(CriterionSpec {
                provenance: CriterionProvenance::Gate,
                criterion_type: "tool_capability".to_string(),
                spec: json!({
                    "tools_count": tools_count,
                    "has_tool_calls": signals::has_tool_calls_in_history(&state.messages),
                    "action_oriented": action_oriented,
                }),
                expected: json!({ "capable": true }),
                timeout_s: None,
            });
            // completion_confirmed: la chiusura di un task software va
            // CONFERMATA da una dichiarazione strutturata (task_complete,
            // ADR 0034) — qualunque outcome onesto passa, l'assenza no.
            let declared = state
                .declared_outcome
                .as_ref()
                .and_then(|v| v.get("outcome"))
                .and_then(Value::as_str);
            // Delega post-subagente: se il PADRE coordinatore non ha dichiarato a
            // sua volta ma un sub-agente delegato in QUESTO run e' arrivato a
            // chiusura (ha percio' dichiarato via task_complete), la CHIUSURA
            // onesta del run ESISTE gia'. Il criterio ne cerca UNA, non che sia
            // del padre (run 48793fde: il coordinatore delega la riscrittura del
            // file, il figlio dichiara `done`, il padre rientra nel gate senza
            // ri-dichiarare -> completion_confirmed bocciava a torto). Segnale
            // MACCHINA dalla history (regola M), punto unico signals (regola L).
            let subagent_completed = signals::has_completed_subagent_dispatch(&state.messages);
            criteria.push(CriterionSpec {
                provenance: CriterionProvenance::Gate,
                criterion_type: "completion_confirmed".to_string(),
                spec: json!({
                    "declared_outcome": declared,
                    "subagent_completed": subagent_completed,
                }),
                expected: json!({ "confirmed": true }),
                timeout_s: None,
            });
        }

        // docs_updated (mig 0676): coerenza fra il CLAIM sulla documentazione
        // e i file davvero toccati ("hai DICHIARATO updated: e' vero?" e'
        // misurabile sul diff — claim-vs-fatti, ADR 0034). FUORI dal blocco
        // strutturale (review W2, rilievo 8): ha il SUO kill-switch, spegnere
        // i criteri strutturali non deve spegnerlo in silenzio. Porta anche il
        // fatto `subagent_completed` (rilievo 3/10, stessa lezione del run
        // 48793fde su completion_confirmed): nei run orchestrati i file li
        // scrivono i SUB-RUN e la history del padre non ha write_file — un
        // "updated" senza doc locale ma con delega completata degrada a
        // Inconclusive invece di bocciare un lavoro sano.
        if self.cfg.docs_criterion_enabled {
            criteria.push(self.criterio_docs(state, &touched_files));
        }

        criteria
    }

    /// Il criterio del piano con dentro le PROVE di questo run (mig 0737).
    ///
    /// L'ORDINE delle due fonti e' load-bearing e incarna «giudice != worker»:
    /// gli apparati advisory, che non hanno scritto il codice, vengono PRIMA;
    /// l'agente esecutore in coda. La dedup conserva la prima provenienza,
    /// quindi l'esecutore puo' AGGIUNGERE prove ma non intestarsi ne'
    /// sostituire quelle di chi lo giudica.
    /// I FATTI DEL RUN viaggiano insieme al piano perche' decidono se quelle
    /// prove si possano eseguire affatto: la MODALITA' (in Conferma un umano
    /// approva ogni `run_command` dell'agente, e il gate non ha nessuno a cui
    /// chiedere) e la SUPERFICIE DI DIALOGO (se il gate duale rimanda a un
    /// umano, ne esiste uno che vedra' la domanda?). Il nodo e' l'unico punto
    /// che vede lo stato, e li passa INTERO: cosi' il fatto aggiunto dopo non
    /// puo' fermarsi a meta' strada.
    fn criterio_col_piano(criterio: CriterionSpec, state: &AgentState) -> CriterionSpec {
        use crate::decisions::piano_di_verifica::{con_piano, PianoDiVerifica, PIANO_VERIFICA_KEY};
        let piano = PianoDiVerifica::unione(&[
            PianoDiVerifica::from_value(state.extra.get(PIANO_VERIFICA_KEY)),
            PianoDiVerifica::da_dichiarazione(state.declared_outcome.as_ref()),
        ]);
        con_piano(criterio, &piano, state)
    }

    /// Il criterio docs (mig 0676): estratto per tenere piatto `build_criteria`
    /// e dare alla spec UN solo costruttore.
    fn criterio_docs(&self, state: &AgentState, touched_files: &[String]) -> CriterionSpec {
        let docs_declared = state
            .declared_outcome
            .as_ref()
            .and_then(|v| v.get("docs_updated"))
            .and_then(Value::as_str);
        CriterionSpec {
            provenance: CriterionProvenance::Gate,
            criterion_type: "docs_updated".to_string(),
            spec: json!({
                "declared": docs_declared,
                "touched_files": touched_files,
                "docs_globs": self.cfg.docs_globs,
                "subagent_completed": signals::has_completed_subagent_dispatch(&state.messages),
            }),
            expected: json!({ "consistent": true }),
            timeout_s: None,
        }
    }

    /// Il reduce sui criteri, in UNA risposta che porta anche i buchi.
    ///
    /// ROOT CAUSE del tipo: la risposta viveva in DUE funzioni, `all_passed` e
    /// `any_inconclusive`, e il doc della prima diceva «quella meta' della
    /// risposta la porta any_inconclusive, e il chiamante la usa per NON
    /// dichiarare verificata la chiusura». Ma una meta' si puo' dimenticare, e
    /// `verifier::run_general_gates` la dimenticava: prendeva solo `all_passed`
    /// e ritornava `(bool, Vec<CriterionResult>)`. Con gate generali TUTTI
    /// inconcludenti, `all_passed` e' vero per vacuita' — quindi il ramo
    /// fail-closed su task software chiudeva il todo come `Completed` e la riga
    /// `nexus_agent_verifier_runs` diceva `passed: true`, senza che nessuno
    /// avesse misurato niente.
    ///
    /// Ora la risposta e' una sola e l'ignoto e' una VARIANTE (regola Q), non un
    /// valore comodo: chi la riceve deve nominare `SuperatoNonVerificato` per
    /// trattarlo come un successo, e a quel punto lo sta scegliendo invece di
    /// subirlo. Niente `impl From<..> for bool` e niente `is_passed()`: sono le
    /// proiezioni che riaprirebbero esattamente la scelta che il tipo chiude.
    pub fn esito_criteri(results: &[CriterionResult]) -> EsitoCriteri {
        let inconclusive = results.iter().filter(|r| r.inconclusive()).count();
        let misurabili: Vec<&CriterionResult> =
            results.iter().filter(|r| !r.inconclusive()).collect();
        if misurabili.iter().any(|r| !r.passed()) {
            return EsitoCriteri::Fallito;
        }
        // Nessun criterio bocciato. Se pero' non se n'e' misurato NESSUNO — lista
        // vuota o tutti inconcludenti — il "passato" e' per vacuita' e va detto.
        if misurabili.is_empty() {
            return EsitoCriteri::SuperatoNonVerificato { inconclusive };
        }
        if inconclusive > 0 {
            return EsitoCriteri::SuperatoNonVerificato { inconclusive };
        }
        EsitoCriteri::Superato
    }

    /// True se l'UNICO tipo di criterio fallito e' `completion_confirmed`: il
    /// lavoro ha superato TUTTI i criteri OGGETTIVI (build/typecheck/runtime/no
    /// orphan/...) e ne manca solo la firma strutturale (il modello ha risolto ma
    /// non ha chiamato `task_complete`, ADR 0034). Guida il TURNO DI GRAZIA del
    /// gate: invece di bocciare un lavoro di fatto riuscito, si concede un turno
    /// mirato per la dichiarazione. Richiede almeno un fallito (altrimenti e' gia'
    /// PASSED) e che TUTTI i falliti siano `completion_confirmed`.
    fn only_completion_confirmed_failed(results: &[CriterionResult]) -> bool {
        let mut any_failed = false;
        for r in results.iter().filter(|r| r.failed()) {
            any_failed = true;
            if r.criterion_type != "completion_confirmed" {
                return false;
            }
        }
        any_failed
    }

    /// Costruisce il testo del `HumanMessage` da iniettare quando il gate
    /// fallisce (`_render_failed_block`, `final_gate.py:396-493`). PURA: i
    /// risultati arrivano gia' calcolati; il `behavior_mode` viene dallo stato.
    /// Riproduce 1:1 il corpo `<final_gate_failed>`, il ramo speciale build
    /// (header + count + nota troncamento), le direttive fail-closed e il
    /// prefisso `<autonomy_hint>` per le modalita' autonome.
    /// Sunto STRUTTURATO dei criteri FALLITI per il payload del meta_step della
    /// timeline "Decisioni del turno" (regola M/osservabilita'). Il resoconto
    /// finale rimanda l'utente a "controlla i criteri falliti nella timeline", ma
    /// il payload del meta_step non li conteneva (solo cycle/phase): il messaggio
    /// era percio' a vuoto. Qui esponiamo, per ogni criterio non passato, il tipo
    /// + il comando (se run_command) + un excerpt breve dell'output/verdict, dalla
    /// stessa evidence gia' usata da [`render_failed_block`] (nessuna prosa nuova).
    fn failed_criteria_meta(results: &[CriterionResult]) -> Value {
        let items: Vec<Value> = results
            .iter()
            .filter(|r| r.failed())
            .map(|r| {
                let ev = &r.evidence;
                let excerpt = str_truthy(ev.get("output_excerpt"))
                    .or_else(|| str_truthy(ev.get(CAMPO_VERDICT)))
                    .or_else(|| str_truthy(ev.get("error")))
                    .unwrap_or("");
                let excerpt: String = excerpt.chars().take(500).collect();
                let mut item = serde_json::json!({
                    "type": r.criterion_type,
                    "command": ev.get("command").and_then(Value::as_str),
                    "excerpt": excerpt,
                });
                // Segnali STRUTTURATI del criterio comando (regola M): l'excerpt e'
                // l'output umano, ma la DECISIONE pass/fail dipende da exit_code +
                // build_errors. Esporli AS-IS (exit_code anche quando null: la resa
                // dell'excerpt non lo mostra) rende un falso negativo diagnosticabile
                // dal solo payload: es. exit_code=0 + build_errors>0 = falso positivo
                // dei pattern di errore su un build riuscito (run 48793fde).
                if r.criterion_type == "run_command" {
                    if let Value::Object(map) = &mut item {
                        map.insert(
                            "exit_code".to_string(),
                            ev.get("exit_code").cloned().unwrap_or(Value::Null),
                        );
                        map.insert(
                            "build_errors".to_string(),
                            ev.get("build_errors").cloned().unwrap_or(Value::Null),
                        );
                    }
                }
                item
            })
            .collect();
        Value::Array(items)
    }

    /// Sunto STRUTTURATO dei criteri NON MISURATI, gemello di
    /// [`Self::failed_criteria_meta`] per l'altra meta' del referto.
    ///
    /// ## Il difetto (MISURATO il 19/08/2026, progetto `t4-prove-consiglio`)
    ///
    /// Un `Inconclusive` non e' `failed()`, quindi non entrava in
    /// `failed_criteria` ne' nel testo del rimando: l'unica traccia persistita
    /// era il CONTATORE (`{"phase":"passed","inconclusive":3}`, il meta_step 198
    /// di quel run). Le figure avevano emesso venticinque prove eseguibili, il
    /// gate duale non le aveva autorizzate, nessuna era girata — e per chi legge
    /// il referto quel run era indistinguibile da uno in cui nessuno aveva
    /// dichiarato prove: un intero che passa da 2 a 3.
    ///
    /// I criteri l'evidenza ce l'avevano gia' e completa (`skipped_cause`,
    /// `skipped_reason`, i conteggi per prova): mancava il TRASPORTO. Il testo
    /// si compone DAI campi, e qui i campi si fermavano prima del confine
    /// (regola Q, punto 3).
    ///
    /// GENERICO e non specifico del piano: i tre campi che legge sono quelli con
    /// cui QUALUNQUE criterio dichiara di non aver potuto misurare, e un
    /// estrattore dedicato a un tipo lascerebbe muti gli altri due che oggi
    /// contribuiscono allo stesso contatore.
    fn inconclusive_criteria_meta(results: &[CriterionResult]) -> Value {
        let items: Vec<Value> = results
            .iter()
            .filter(|r| r.inconclusive())
            .map(Self::criterio_non_misurato_meta)
            .collect();
        Value::Array(items)
    }

    /// UNA voce dell'elenco dei non misurati.
    fn criterio_non_misurato_meta(r: &CriterionResult) -> Value {
        let ev = &r.evidence;
        let verdetto = ev.get(CAMPO_VERDICT).and_then(Value::as_str);
        // La CAUSA e' il campo su cui si conta e si filtra; il motivo in prosa
        // le sta accanto per chi legge, e nessuno lo rilegge.
        let motivo = str_truthy(ev.get("skipped_reason"))
            .or_else(|| str_truthy(ev.get("error")))
            .or_else(|| verdetto.filter(|v| !v.is_empty()))
            .unwrap_or("");
        let mut item = serde_json::Map::new();
        item.insert("type".to_string(), serde_json::json!(r.criterion_type));
        item.insert(CAMPO_VERDICT.to_string(), serde_json::json!(verdetto));
        item.insert(
            "skipped_cause".to_string(),
            serde_json::json!(ev.get("skipped_cause").and_then(Value::as_str)),
        );
        item.insert(
            "reason".to_string(),
            serde_json::json!(motivo.chars().take(500).collect::<String>()),
        );
        // I conteggi che rendono «non ho misurato» una misura: sono esattamente
        // cio' che distingue un piano vuoto da venticinque prove che nessuno ha
        // eseguito.
        for campo in ["prove", "cause"] {
            let Some(v) = ev.get(campo) else { continue };
            item.insert(campo.to_string(), v.clone());
        }
        Value::Object(item)
    }

    /// L'elenco dei criteri NON MISURATI dentro il payload di un ciclo del gate.
    ///
    /// Un punto solo per i QUATTRO payload che lo portano: la chiave scritta a
    /// mano in ognuno era il modo in cui un ramo poteva restare indietro, e i
    /// rami che chiudono POSITIVAMENTE sono proprio quelli in cui il referto
    /// aveva il solo contatore.
    /// Il payload dei DUE rami che chiudono POSITIVAMENTE.
    ///
    /// Un costruttore solo, per la stessa ragione per cui quei due rami non
    /// possono avere due idee di «verificato» (regola L): erano due oggetti
    /// identici scritti a mano, ed e' proprio li' che il contatore
    /// `inconclusive` viaggiava senza il proprio elenco — 19/08/2026,
    /// venticinque prove dichiarate e zero eseguite indistinguibili da un piano
    /// vuoto.
    fn payload_chiusura(
        cycle: i64,
        fase: &str,
        inconclusive_n: usize,
        results: &[CriterionResult],
    ) -> Value {
        Self::con_criteri_non_misurati(
            serde_json::json!({ "cycle": cycle, "phase": fase, "inconclusive": inconclusive_n }),
            results,
        )
    }

    fn con_criteri_non_misurati(mut payload: Value, results: &[CriterionResult]) -> Value {
        if let Value::Object(map) = &mut payload {
            map.insert(
                "inconclusive_criteria".to_string(),
                Self::inconclusive_criteria_meta(results),
            );
        }
        payload
    }

    pub fn render_failed_block(
        state: &AgentState,
        cycle: i64,
        max_cycles: i64,
        results: &[CriterionResult],
    ) -> String {
        // Corpo specifico per criterio fallito (aggregato, non testo fisso).
        let failed: Vec<&CriterionResult> = results.iter().filter(|r| r.failed()).collect();
        let mut body_parts: Vec<String> = Vec::new();
        // build_errors_count e' load-bearing OLTRE il loop (entra nelle direttive
        // se >0, final_gate.py:464). build_truncated invece e' usato SOLO nel
        // ramo build per l'header, quindi resta locale al loop.
        let mut build_errors_count: usize = 0;

        for r in &failed {
            let ev = &r.evidence;
            // excerpt = output_excerpt or verdict or error or "" (Python:421).
            // Semantica `or` FALSY: una STRINGA VUOTA "" e' falsy come None, quindi
            // cade sul campo successivo (non solo su None/tipo-sbagliato). Usiamo
            // `str_truthy` (Some solo se la stringa NON e' vuota) per replicarla
            // 1:1: con output_excerpt:"" + verdict valorizzato si rende il verdict.
            let excerpt = str_truthy(ev.get("output_excerpt"))
                .or_else(|| str_truthy(ev.get(CAMPO_VERDICT)))
                .or_else(|| str_truthy(ev.get("error")))
                .unwrap_or("");
            if excerpt.is_empty() {
                continue;
            }
            // is_build_run_cmd: run_command con exit_code e output_total_chars
            // presenti nell'evidence (`final_gate.py:424-428`).
            let is_build_run_cmd = r.criterion_type == "run_command"
                && !ev.get("exit_code").map(Value::is_null).unwrap_or(true)
                && !ev
                    .get("output_total_chars")
                    .map(Value::is_null)
                    .unwrap_or(true);

            if is_build_run_cmd {
                // L'excerpt e' gia' tagliato dal runner: lo passiamo intero.
                let text = excerpt.to_string();
                build_errors_count = count_build_errors(&text);
                let build_truncated = ev
                    .get("output_truncated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                // total_chars = int(output_total_chars or len(text)) (Python:435).
                // Semantica `or` FALSY: 0 e' falsy, quindi cade su len(text) come
                // l'assenza o un tipo non-intero. `i64_truthy` ritorna Some solo se
                // il valore e' un intero NON-zero; altrimenti len(text) (codepoint).
                let total_chars = i64_truthy(ev.get("output_total_chars"))
                    .unwrap_or_else(|| text.chars().count() as i64);
                let mut header_bits = vec![format!("[{}]", r.criterion_type)];
                if build_errors_count > 0 {
                    header_bits.push(format!("errori rilevati: {build_errors_count}"));
                }
                if build_truncated {
                    // len(text) Python = numero di char (str), non byte.
                    let text_len = text.chars().count();
                    header_bits.push(format!(
                        "output troncato ({text_len}/{total_chars} char): \
                         rilancia il build per leggere il resto"
                    ));
                }
                body_parts.push(format!("{}\n{}", header_bits.join(" "), text));
            } else {
                // Altri criteri: excerpt tagliato a 900 char (codepoint, come
                // `str(excerpt)[:900]` Python).
                let truncated: String = excerpt.chars().take(900).collect();
                body_parts.push(format!("[{}]\n{}", r.criterion_type, truncated));
            }
        }

        let detail = if body_parts.is_empty() {
            "Una verifica del gate e' fallita.".to_string()
        } else {
            body_parts.join("\n\n")
        };

        // Direttive fail-closed. Composte DAI criteri falliti (regola Q punto 3),
        // non da un letterale fisso.
        //
        // MISURATO il 12/08/2026 su `test-11-08-listino`: erano SEMPRE di forma
        // BUILD — «rilancia il comando di build con run_command», «finche' il
        // build non passa al 100% (exit 0, zero errori)» — e venivano consegnate
        // tali e quali a un fallimento di `static_render`, dove nessun build
        // esiste e nessun exit code puo' andare a zero. L'agente riceveva
        // istruzioni su un'attivita' che non era la sua, e la traccia lo mostra:
        // dopo il rimando, cinque letture consecutive senza una scrittura.
        //
        // Le righe comuni restano comuni: quelle di build tornano SOLO se un
        // criterio di build e' davvero fra i falliti. Il ramo build e' identico a
        // prima, incluso il conteggio in posizione 1.
        let tipi_falliti: std::collections::BTreeSet<&str> =
            failed.iter().map(|r| r.criterion_type.as_str()).collect();
        let c_e_build = build_errors_count > 0
            || tipi_falliti.contains("run_command")
            || tipi_falliti.contains("suite");
        let mut directives_lines: Vec<String> = vec![
            "DIRETTIVE (fail-closed):".to_string(),
            "- Leggi TUTTO l'output qui sopra: ogni errore va corretto, non solo il primo."
                .to_string(),
            "- Correggi TUTTI gli errori in un solo giro quando possibile: edita ogni file"
                .to_string(),
            "  impattato (anche errori 'banali' tipo unused/type mismatch contano).".to_string(),
        ];
        if c_e_build {
            directives_lines.push(
                "- Se l'output e' troncato (vedi nota 'output troncato'), rilancia il comando di"
                    .to_string(),
            );
            directives_lines
                .push("  build con run_command (o rileggi i file impattati) per vedere il resto.".to_string());
            directives_lines.push(
                "- Lavora per CONVERGENZA: niente 'task completato' finche' il build non passa"
                    .to_string(),
            );
            directives_lines
                .push("  al 100% (exit 0, zero errori). Riverifica sempre dopo le correzioni.".to_string());
        } else {
            directives_lines.push(
                "- Lavora per CONVERGENZA: niente 'task completato' finche' la verifica non passa."
                    .to_string(),
            );
            directives_lines
                .push("  Riverifica sempre dopo le correzioni, esercitando il flusso reale.".to_string());
        }
        if tipi_falliti.contains(crate::decisions::static_render::CRITERION_TYPE) {
            // La posizione la porta il rilievo quando il browser l'ha
            // dichiarata (`EccezionePagina::posizione`): dirgli di andarci e'
            // l'unica direttiva che gli risparmia la ricerca a tentoni.
            directives_lines.push(
                "- La pagina NON mostra il proprio contenuto. Se il rilievo indica una posizione"
                    .to_string(),
            );
            directives_lines.push(
                "  (file:riga:colonna), correggi LI': e' il punto esatto in cui l'esecuzione si e'"
                    .to_string(),
            );
            directives_lines
                .push("  interrotta, e tutto cio' che seguiva non e' mai girato.".to_string());
            directives_lines.push(
                "- Il criterio guarda il DOM DOPO l'esecuzione del JavaScript, non il sorgente:"
                    .to_string(),
            );
            directives_lines
                .push("  un file sintatticamente plausibile ma che lancia resta bocciato.".to_string());
        }
        if build_errors_count > 0 {
            directives_lines.insert(
                1,
                format!(
                    "- Numero di errori rilevati nel build: {build_errors_count}. \
                     Risolvili TUTTI prima del prossimo final_gate."
                ),
            );
        }
        let directives = directives_lines.join("\n");

        let mut body = format!(
            "<final_gate_failed cycle=\"{cycle}/{max_cycles}\">\n\
             Verifica pre-chiusura FALLITA. NON dichiarare il task completato finche'\n\
             non e' risolto e RIVERIFICATO esercitando il flusso reale.\n\n\
             {detail}\n\n\
             {directives}\n\
             </final_gate_failed>"
        );

        // Prefisso autonomy_hint per le modalita' autonome (`final_gate.py:481-492`):
        // usa `automation_mode` (enum canonico), non sinonimi su behavior_mode.
        if let Some(mode_label) = state.automation_mode.and_then(|m| m.wire_label()) {
            let autonomy_prefix = format!(
                "<autonomy_hint mode=\"{mode_label}\">\n\
                 L'utente ha selezionato la modalita' '{mode_label}': procedi\n\
                 AUTONOMAMENTE con l'integrazione. NON chiedere conferma, NON scrivere\n\
                 domande tipo 'Vuoi che lo faccia?' o 'Confermi?'. Esegui direttamente\n\
                 le modifiche necessarie usando i tool disponibili.\n\
                 </autonomy_hint>\n\n"
            );
            body = format!("{autonomy_prefix}{body}");
        }
        body
    }
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for FinalGateNode {
    fn id(&self) -> NodeId {
        NodeId::FinalGate
    }

    async fn run(&self, state: &AgentState, ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        // ── Gate enabled / is_software_task (final_gate.py:505-506) ───────────
        // enabled OFF o task non-software -> pass-through {} (flusso prosegue).
        // is_software_task: PUNTO UNICO signals::is_software_task (regola L).
        if !self.cfg.enabled || !signals::is_software_task(state, &self.routing_cfg) {
            return Ok(Self::pass_through());
        }

        // ── Figura col giudizio dichiarato -> il contratto non si applica ─────
        // Il contratto di questo gate e' dell'ESECUTORE: l'ambiente deve
        // verificare (build, endpoint). Il deliverable di una FIGURA e' il
        // GIUDIZIO — review_verdict / advisory_verdict / debate_position, gia'
        // validato in forma alla dichiarazione — e applicarle i criteri
        // d'ambiente lega la sua sorte al codice che sta giudicando: un
        // `verdict: fail` fondato fa fallire ANCHE la build, il gate rimanda
        // "in correzione" un giudice senza tool di scrittura, e il run cicla
        // fino al wall-clock, dove il verdetto valido viene scartato come
        // timeout. Misurato su bacheca-attivita (run 1845a0ce, 2026-07-30):
        // verdetto fail con 8 finding a 152s dei 240s di budget, poi 3
        // bocciature su `npx tsc` (i difetti appena certificati) e morte a
        // 240s esatti. Bastava un `run_command` in history (e' in
        // fs_mutator_tools) a rendere "software task" il revisore: il gemello
        // feb8998a, che aveva solo letto file, chiudeva pulito. Punto unico:
        // declared_role_channel, lo stesso con cui l'edge post-ToolDispatch
        // instrada qui; solo le figure hanno quei tool in whitelist, quindi un
        // run esecutore non puo' eludere il gate per questa via.
        if let Some(delta) = self.chiusura_dichiarativa_della_figura(state, ctx).await {
            return Ok(delta);
        }

        // ── Cycle (final_gate.py:508-509) ─────────────────────────────────────
        let cycle = state.final_gate_cycle.unwrap_or(0) + 1;
        let max_cycles = self.cfg.max_cycles;

        // Narrazione live: la chat racconta che parte la verifica oggettiva.
        crate::nodes::emit_phase_meta(
            ctx.emit.as_ref(),
            self.meta_steps.as_ref(),
            "final_gate",
            format!("Verifico il risultato (tentativo {cycle}/{max_cycles})"),
            serde_json::json!({"cycle": cycle, "max_cycles": max_cycles, "phase": "start"}),
        )
        .await;
        self.dichiara_profilo_mancante(cycle, ctx).await;

        // ── Esecuzione criteri (sotto-sistema delegato) ───────────────────────
        // Un guasto infrastrutturale del runner propaga NodeError; un fallimento
        // di un singolo criterio e' mappato dal concreto su
        // CriterionResult{passed:false} (parita' col try/except Python,
        // final_gate.py:381-385) e NON propaga errore.
        let criteria = self.build_criteria(state);
        // Esito ONESTO sul fronte FUNZIONALE (regola M, gemello di
        // `verify_profile_missing`): il run ha interrogato un servizio HTTP da se'
        // — quindi un servizio HTTP c'e' — ma nessun endpoint e' stato dichiarato
        // ne' configurato, percio' il gate non ne provera' nessuno. Chiudere
        // "verificato" in questa condizione e' cio' che ha lasciato passare una
        // POST che rispondeva 500 mentre la GET, la sola provata dall'agente,
        // rispondeva 200 (gestione-spese, 2026-07-28).
        let http_probes = signals::http_probes_in_history(&state.messages);
        let functional_probe_missing = self.cfg.endpoint_check_enabled
            && !criteria.iter().any(|c| c.criterion_type == "http")
            && http_probes > 0;
        if functional_probe_missing && cycle <= 1 {
            crate::nodes::emit_phase_meta(
                ctx.emit.as_ref(),
                self.meta_steps.as_ref(),
                "final_gate",
                "Verifica funzionale degli endpoint NON eseguita: nessun endpoint dichiarato in task_complete ne' configurato nel progetto".to_string(),
                serde_json::json!({
                    "phase": "endpoints_undeclared",
                    "http_calls_in_history": http_probes,
                }),
            )
            .await;
        }
        let results = self
            .criteria
            .run(criteria)
            .await
            .map_err(|e| NodeError::Failed {
                node: "final_gate",
                message: format!("esecuzione criteri fallita: {e}"),
            })?;

        // L'esito porta gia' i buchi: `SuperatoNonVerificato` non boccia (non
        // prova nulla sul codice) ma toglie alla chiusura il titolo di
        // "verificata". Qui il ramo di chiusura resta lo stesso per entrambi i
        // casi non-falliti — la distinzione la usa il titolo, come prima — ma
        // ora e' il TIPO a portarla, non due letture separate che si possono
        // disallineare.
        let esito = Self::esito_criteri(&results);
        let inconclusive_n = match esito {
            EsitoCriteri::SuperatoNonVerificato { inconclusive } => inconclusive,
            _ => 0,
        };
        let passed = !matches!(esito, EsitoCriteri::Fallito);
        // UN solo criterio di "chiusura non verificata" per i DUE rami che
        // chiudono positivamente (PASSED, e la rimisura dopo il turno di
        // grazia): due espressioni darebbero due idee di "verificato" che
        // divergono al primo ritocco (regola L).
        let chiusura_non_verificata =
            self.cfg.verify_profile_missing || functional_probe_missing || inconclusive_n > 0;

        // ── Ramo PASSED (final_gate.py:513-522) ───────────────────────────────
        // Chiude con esito canonico CompletedVerified lato mcp-core.
        if passed {
            tracing::info!(
                target: "nexus_agent_graph::final_gate",
                cycle,
                inconclusive = inconclusive_n,
                "final_gate: passato -> chiusura"
            );
            crate::nodes::emit_phase_meta(
                ctx.emit.as_ref(),
                self.meta_steps.as_ref(),
                "final_gate",
                titolo_verifica_superata(inconclusive_n),
                Self::payload_chiusura(cycle, "passed", inconclusive_n, &results),
            )
            .await;
            return Ok(StateDelta {
                final_gate_cycle: Some(Some(0)),
                stop_reason: Some(Some(StopReason::EndTurn)),
                final_gate_passed: Some(Some(true)),
                final_gate_verdict: Some(Some(FinalGateVerdict::Passed)),
                gate_routing: Some(Some(GateRouting::Chiude)),
                // Esito ONESTO (regola M): i criteri soft (no_orphan/outputs_exist)
                // sono passati, ma se il profilo di verifica dell'ambiente manca
                // NESSUN comando di verifica reale e' stato eseguito. Lo segnaliamo
                // come "svolto ma non verificato" -> il finalizzatore mappa
                // CompletedUnverified (distinto da CompletedVerified). Con profilo
                // presente (verifica eseguita) e' Some(false): esito verificato.
                // Stessa cosa sul fronte FUNZIONALE: un'app con un servizio HTTP di
                // cui nessuno ha provato un endpoint non e' un'app verificata,
                // per quanto il suo codice compili.
                // Terza forma dello stesso buco, e la sola che prima veniva
                // scambiata per una misura: un criterio NON MISURABILE. Non
                // boccia (non prova un difetto) e non assolve (non prova la
                // correttezza), quindi ha esattamente la conseguenza degli altri
                // due -- il finalizzatore mappa `CompletedUnverified`.
                final_gate_unverified: Some(Some(chiusura_non_verificata)),
                ..Default::default()
            }
            .into_opaque());
        }

        // ── Ramo FORCED / CAP (final_gate.py:524-537) ─────────────────────────
        // Chiusura SENZA re-executor su forced_close_unverified (abort anti-loop:
        // re-eseguire duplicherebbe il messaggio finale) o cap raggiunto. I
        // criteri sono gia' stati eseguiti e NON sono passati (siamo dopo il ramo
        // PASSED): registriamo il verdetto NEGATIVO esplicito
        // `final_gate_passed=false` (segnale strutturato, regola M). Serve al
        // finalizzatore per mappare FailedDiagnosed anche su una dichiarazione
        // "done" ottimista del modello e per annotare il resoconto con l'esito
        // reale, invece di lasciarlo passare per "completed".
        let forced_close = state.forced_close_unverified.unwrap_or(false);
        if forced_close || cycle >= max_cycles {
            // ── TURNO DI GRAZIA completion (regola M + UX) ────────────────────
            // Il modello ha superato TUTTI i criteri OGGETTIVI (build/typecheck/
            // runtime/...) ma ha omesso la sola dichiarazione strutturata di
            // chiusura (`completion_confirmed`: niente `task_complete`). Non
            // bocciare come FailedDiagnosed un lavoro di fatto riuscito e
            // verificato: concedi UN turno mirato per la firma. SOLO al primo
            // ingresso al cap (`cycle == max_cycles`) e NON su forced_close (abort
            // anti-loop).
            //
            // LA GRAZIA NON E' UN VERDETTO, E' UNA PROMESSA. Restituendo il
            // turno si consegna il codice a chi lo puo' ancora cambiare, quindi
            // cio' che si e' appena misurato descrive l'albero di ADESSO e non
            // l'esito del run: il verdetto emesso e' `PassedPendingSignature`
            // (non terminale) e l'ultima parola spetta alla RIMISURA. Perche' la
            // rimisura possa avvenire il ciclo scende a `max_cycles - 1` — la
            // porta ordinaria (`final_gate_eligible`) resta aperta per UN solo
            // rientro — e la seconda grazia la vieta il flag DEDICATO
            // `final_gate_grace_granted`, monotono (regola Q: il contatore
            // porterebbe due significati opposti sullo stesso numero).
            //
            // Prima il turno portava `final_gate_cycle` a `max_cycles` con la
            // didascalia «il giro successivo rientra qui e chiude»: quella
            // premessa il routing la rende IMPOSSIBILE (`final_gate_eligible`
            // pretende `cycle < max_cycles`), quindi il turno di grazia era
            // l'ULTIMO ingresso del gate e il suo verdetto positivo sopravviveva
            // a qualunque scrittura successiva.
            let grazia_gia_concessa = state.final_gate_grace_granted.unwrap_or(false);
            let solo_firma_manca = Self::only_completion_confirmed_failed(&results);
            if !forced_close && cycle == max_cycles && !grazia_gia_concessa && solo_firma_manca {
                crate::nodes::emit_phase_meta(
                    ctx.emit.as_ref(),
                    self.meta_steps.as_ref(),
                    "final_gate",
                    "Criteri oggettivi superati: chiudi con task_complete".to_string(),
                    serde_json::json!({"cycle": cycle, "phase": "completion_grace"}),
                )
                .await;
                let hm = Message::Human {
                    content: MessageContent::text(
                        "I criteri OGGETTIVI di verifica (build, typecheck, runtime) sono \
                         TUTTI superati: il lavoro e' verificato e completo. Manca solo la \
                         dichiarazione strutturata di chiusura. Chiudi ORA il task chiamando \
                         il tool task_complete (outcome + summary). NON serve altro lavoro sul \
                         codice: NON ri-eseguire build/typecheck, chiama SOLO task_complete."
                            .to_string(),
                    ),
                };
                return Ok(StateDelta {
                    messages: Some(vec![hm]),
                    // UN solo rientro: la porta ordinaria resta aperta e la
                    // seconda grazia la vieta il flag qui sotto.
                    final_gate_cycle: Some(Some(max_cycles.saturating_sub(1))),
                    final_gate_grace_granted: Some(Some(true)),
                    stop_reason: Some(Some(StopReason::ToolUse)),
                    pending_tool_uses: Some(Some(vec![])),
                    // Il lavoro e' verificato SULL'ALBERO MISURATO: manca solo la
                    // firma. Senza questo verdetto esplicito il ciclo veniva letto
                    // a valle come "verifica fallita" e un run RIUSCITO chiudeva
                    // FailedDiagnosed.
                    gate_routing: Some(Some(GateRouting::RimandaInCorrezione)),
                    final_gate_verdict: Some(Some(FinalGateVerdict::PassedPendingSignature)),
                    // Se il run muore PRIMA della rimisura promessa, l'unica
                    // lettura onesta e' "svolto ma non verificato": la misura che
                    // abbiamo descrive un albero che da qui in poi puo' cambiare.
                    // Il rientro nel gate ricalcola questo campo dai fatti.
                    final_gate_unverified: Some(Some(true)),
                    ..Default::default()
                }
                .into_opaque());
            }

            // ── CHIUSURA dopo la grazia: firma ancora assente, criteri
            // oggettivi superati ────────────────────────────────────────────
            // Stessa constatazione del turno di grazia, ma qui il gate CHIUDE:
            // il turno non torna a nessuno, quindi l'albero appena misurato e'
            // quello finale e l'affermazione "criteri oggettivi superati" resta
            // vera. E' il caso per cui la grazia esiste — l'agente ha lavorato,
            // tutto passa, la sola dichiarazione strutturata non arriva — e
            // continua a chiudere bene: `final_gate_passed=true` piu' l'onesta'
            // sul non-misurato, esattamente come il ramo PASSED.
            //
            // Senza questo ramo la grazia sarebbe una trappola: chi la riceve e
            // firma chiude bene, chi la riceve e tace verrebbe bocciato
            // `FailedFinal` con i criteri oggettivi tutti verdi.
            if !forced_close && grazia_gia_concessa && solo_firma_manca {
                return Ok(self
                    .chiusura_con_firma_assente(cycle, inconclusive_n, &results, chiusura_non_verificata, ctx)
                    .await);
            }
            // ── NON-CONVERGENZA -> ESCALATION di modello (regola H, "niente di
            // fisso") ─────────────────────────────────────────────────────────
            // Il gate ha esaurito `max_cycles` con criteri OGGETTIVI ancora
            // falliti: il modello corrente non ripara il codice entro i suoi
            // tentativi. Invece di chiudere secco `FailedDiagnosed` (che lascia
            // il run su un modello scadente — il gap del run cc01d06d,
            // mistral-medium-3 x52 iter, 0 escalation), CEDE il turno
            // all'executor perche' PROMUOVA a un modello piu' capace tramite il
            // PUNTO UNICO `maybe_escalate_nonconvergence` (il gate non ha la porta
            // di escalation, l'executor si': regola L). Condizioni:
            //   - flag ON;
            //   - NON `forced_close` (abort anti-loop: chiudi, non ciclare);
            //   - NON solo-`completion_confirmed` (oggettivi ok = lavoro DI FATTO
            //     completo -> chiudi, non sprecare un'escalation; quel caso ha
            //     gia' il turno di grazia sopra);
            //   - `auto_escalations < max_escalations` (anti-loop: budget
            //     condiviso con l'executor; esaurito -> chiudi secco sotto).
            // `final_gate_cycle=0`: il modello promosso riparte con cicli freschi
            // sul gate. Il flag `FINAL_GATE_ESCALATION_KEY` e' il segnale
            // STRUTTURATO (regola M) che l'executor consuma al rientro (ToolUse).
            let auto_escalations = state
                .extra
                .get("auto_escalations")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            if self.cfg.escalate_on_nonconvergence
                && !forced_close
                && !solo_firma_manca
                && auto_escalations < self.cfg.max_escalations
            {
                tracing::warn!(
                    target: "nexus_agent_graph::final_gate",
                    cycle,
                    max_cycles,
                    auto_escalations,
                    "final_gate: non-convergenza al cap -> cedo il turno all'executor per ESCALATION di modello"
                );
                crate::nodes::emit_phase_meta(
                    ctx.emit.as_ref(),
                    self.meta_steps.as_ref(),
                    "final_gate",
                    "Verifica non superata al limite tentativi: promuovo a un modello piu' capace".to_string(),
                    Self::con_criteri_non_misurati(serde_json::json!({"cycle": cycle, "phase": "nonconvergence_escalation", "failed_criteria": Self::failed_criteria_meta(&results)}), &results),
                )
                .await;
                let block = Self::render_failed_block(state, cycle, max_cycles, &results);
                let mut extra_out = state.extra.clone();
                extra_out.insert(FINAL_GATE_ESCALATION_KEY.to_string(), serde_json::json!(true));
                return Ok(StateDelta {
                    messages: Some(vec![Message::Human {
                        content: MessageContent::text(block),
                    }]),
                    // Cicli freschi per il modello promosso (il gate non ricade
                    // subito al cap ereditando il conteggio del predecessore).
                    final_gate_cycle: Some(Some(0)),
                    stop_reason: Some(Some(StopReason::ToolUse)),
                    pending_tool_uses: Some(Some(vec![])),
                    gate_routing: Some(Some(GateRouting::RimandaInCorrezione)),
                    final_gate_verdict: Some(Some(FinalGateVerdict::EscalationHandoff)),
                    extra: Some(extra_out),
                    ..Default::default()
                }
                .into_opaque());
            }
            tracing::warn!(
                target: "nexus_agent_graph::final_gate",
                forced_close,
                cycle,
                max_cycles,
                "final_gate: chiusura senza re-executor"
            );
            crate::nodes::emit_phase_meta(
                ctx.emit.as_ref(),
                self.meta_steps.as_ref(),
                "final_gate",
                "Verifica non superata: chiudo (limite tentativi)".to_string(),
                Self::con_criteri_non_misurati(serde_json::json!({"cycle": cycle, "phase": "forced_close", "forced": forced_close, "failed_criteria": Self::failed_criteria_meta(&results)}), &results),
            )
            .await;
            return Ok(StateDelta {
                final_gate_cycle: Some(Some(0)),
                stop_reason: Some(Some(StopReason::EndTurn)),
                final_gate_passed: Some(Some(false)),
                final_gate_verdict: Some(Some(FinalGateVerdict::FailedFinal)),
                gate_routing: Some(Some(GateRouting::Chiude)),
                ..Default::default()
            }
            .into_opaque());
        }

        // ── Ramo FAIL (final_gate.py:539-546) ─────────────────────────────────
        // Inietta il verdetto come HumanMessage e rimanda all'executor.
        tracing::info!(
            target: "nexus_agent_graph::final_gate",
            cycle,
            max_cycles,
            "final_gate: fallito -> re-executor"
        );
        crate::nodes::emit_phase_meta(
            ctx.emit.as_ref(),
            self.meta_steps.as_ref(),
            "final_gate",
            format!("Verifica fallita: rimando in correzione ({cycle}/{max_cycles})"),
            Self::con_criteri_non_misurati(serde_json::json!({"cycle": cycle, "max_cycles": max_cycles, "phase": "failed", "failed_criteria": Self::failed_criteria_meta(&results)}), &results),
        )
        .await;
        let block = Self::render_failed_block(state, cycle, max_cycles, &results);
        let hm = Message::Human {
            content: MessageContent::text(block),
        };
        Ok(StateDelta {
            messages: Some(vec![hm]),
            final_gate_cycle: Some(Some(cycle)),
            // Da qui in poi il run lavora SOTTO un rimando, e chi giudica i suoi
            // passi deve saperlo: senza, un comando che ripara cio' che il gate
            // ha contestato risulta «non coerente» con la richiesta dell'utente.
            criteri_in_correzione: Some(
                results
                    .iter()
                    .filter(|r| r.failed())
                    .map(|r| r.criterion_type.clone())
                    .collect(),
            ),
            stop_reason: Some(Some(StopReason::ToolUse)),
            // pending_tool_uses azzerato a lista vuota (durata 1 turno):
            // Some(Some(vec![])) e' AZZERA, distinto da None (no-op).
            pending_tool_uses: Some(Some(vec![])),
            // L'UNICO ramo in cui una ri-verifica e' davvero attesa: se il run
            // muore prima di rientrare, "verifica fallita e non ripetuta" e' vero.
            gate_routing: Some(Some(GateRouting::RimandaInCorrezione)),
            final_gate_verdict: Some(Some(FinalGateVerdict::FailedPendingCorrection)),
            ..Default::default()
        }
        .into_opaque())
    }
}

impl FinalGateNode {
    /// Figura col giudizio dichiarato: il contratto di questo gate non si
    /// applica, e il run chiude senza verifica d'ambiente.
    ///
    /// Il contratto del gate e' dell'ESECUTORE: l'ambiente deve verificare
    /// (build, endpoint). Il deliverable di una FIGURA e' il GIUDIZIO —
    /// review_verdict / advisory_verdict / debate_position, gia' validato in
    /// forma alla dichiarazione — e applicarle i criteri d'ambiente lega la sua
    /// sorte al codice che sta giudicando: un `verdict: fail` fondato fa fallire
    /// ANCHE la build, il gate rimanda "in correzione" un giudice senza tool di
    /// scrittura, e il run cicla fino al wall-clock, dove il verdetto valido
    /// viene scartato come timeout. Misurato su bacheca-attivita (run 1845a0ce,
    /// 2026-07-30): verdetto fail con 8 finding a 152s dei 240s di budget, poi
    /// 3 bocciature su `npx tsc` (i difetti appena certificati) e morte a 240s
    /// esatti. Bastava un `run_command` in history (e' in fs_mutator_tools) a
    /// rendere "software task" il revisore: il gemello feb8998a, che aveva solo
    /// letto file, chiudeva pulito. Punto unico: declared_role_channel, lo
    /// stesso con cui l'edge post-ToolDispatch instrada qui; solo le figure
    /// hanno quei tool in whitelist, quindi un run esecutore non puo' eludere
    /// il gate per questa via.
    ///
    /// `Some(delta)` = la figura chiude qui; `None` = non e' una figura, il
    /// gate prosegue col proprio contratto.
    async fn chiusura_dichiarativa_della_figura(
        &self,
        state: &AgentState,
        ctx: &AgentNodeCtx,
    ) -> Option<OpaqueDelta> {
        let channel = crate::routing::declared_role_channel(state)?;
        crate::nodes::emit_phase_meta(
            ctx.emit.as_ref(),
            self.meta_steps.as_ref(),
            "final_gate",
            format!(
                "Giudizio dichiarato ({channel}): la verifica dell'ambiente non si applica alla figura"
            ),
            serde_json::json!({"phase": "role_deliverable_declared", "channel": channel}),
        )
        .await;
        Some(Self::pass_through())
    }

    /// ADR 0036, esito ONESTO (regola M): se il profilo di verifica
    /// dell'ambiente non e' disponibile (mai inferito e LLM irraggiungibile)
    /// il gate NON esegue comandi generici di ripiego e lo DICHIARA in chat,
    /// una sola volta (primo ciclo): l'utente sa che la chiusura non include
    /// la verifica tecnica dell'ambiente. La condizione vive QUI, non nel
    /// chiamante: `run` delega il "se", oltre al "come".
    async fn dichiara_profilo_mancante(&self, cycle: i64, ctx: &AgentNodeCtx) {
        if !self.cfg.verify_profile_missing || cycle > 1 {
            return;
        }
        crate::nodes::emit_phase_meta(
            ctx.emit.as_ref(),
            self.meta_steps.as_ref(),
            "final_gate",
            "Verifica tecnica dell'ambiente NON eseguita: profilo non disponibile (inferenza LLM non riuscita)".to_string(),
            serde_json::json!({"phase": "profile_missing"}),
        )
        .await;
    }

    /// CHIUSURA con i criteri oggettivi superati e la sola firma assente, DOPO
    /// che il turno di grazia e' gia' stato speso.
    ///
    /// E' l'altra meta' della grazia e ne rende vera l'affermazione: qui il gate
    /// CHIUDE, quindi l'albero appena misurato e' quello finale e
    /// [`FinalGateVerdict::ObjectivePassedSignatureMissing`] non puo' piu' essere
    /// smentito da una scrittura successiva. Senza questo ramo la grazia sarebbe
    /// una trappola — chi la riceve e firma chiude bene, chi la riceve e tace
    /// verrebbe bocciato `FailedFinal` con tutti gli oggettivi verdi.
    ///
    /// `non_verificata` arriva dal chiamante ed e' lo STESSO valore che riceve il
    /// ramo PASSED: i due rami che chiudono positivamente non possono avere due
    /// idee di "verificato" (regola L).
    async fn chiusura_con_firma_assente(
        &self,
        cycle: i64,
        inconclusive_n: usize,
        results: &[CriterionResult],
        non_verificata: bool,
        ctx: &AgentNodeCtx,
    ) -> OpaqueDelta {
        tracing::info!(
            target: "nexus_agent_graph::final_gate",
            cycle,
            inconclusive = inconclusive_n,
            "final_gate: criteri oggettivi superati alla rimisura, firma assente -> chiusura"
        );
        crate::nodes::emit_phase_meta(
            ctx.emit.as_ref(),
            self.meta_steps.as_ref(),
            "final_gate",
            "Criteri oggettivi superati, dichiarazione di chiusura assente: chiudo".to_string(),
            Self::payload_chiusura(
                cycle,
                "objective_passed_signature_missing",
                inconclusive_n,
                results,
            ),
        )
        .await;
        StateDelta {
            final_gate_cycle: Some(Some(0)),
            stop_reason: Some(Some(StopReason::EndTurn)),
            final_gate_passed: Some(Some(true)),
            final_gate_verdict: Some(Some(FinalGateVerdict::ObjectivePassedSignatureMissing)),
            gate_routing: Some(Some(GateRouting::Chiude)),
            final_gate_unverified: Some(Some(non_verificata)),
            ..Default::default()
        }
        .into_opaque()
    }

    /// Delta pass-through (`final_gate.py:506`): il gate non si applica, il
    /// flusso prosegue verso la chiusura.
    ///
    /// Unica chiave nel delta: la DICHIARAZIONE di routing (regola M). Il Python
    /// ritornava `{}` letterale e il rimando si deduceva dallo `stop_reason`, ma
    /// quel campo lo scrive anche l'executor: un pass-through muto lasciava
    /// decidere l'edge a un valore altrui. Dichiarare `Chiude` rende il ramo
    /// esplicito e impedisce di ereditare il `RimandaInCorrezione` di un
    /// passaggio precedente.
    fn pass_through() -> OpaqueDelta {
        StateDelta {
            gate_routing: Some(Some(GateRouting::Chiude)),
            ..Default::default()
        }
        .into_opaque()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nexus_graph::node::GraphNode;
    use nexus_graph::GraphState as _;
    use sqlx::postgres::PgPoolOptions;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::*;
    use crate::runtime::ports::CriterionOutcome;
    use crate::runtime::test_doubles::{
        NullEventSink, StubCriteriaRunner, StubLlmGateway, StubMetaStepStore, StubToolExecutor,
    };
    use crate::runtime::AgentNodeCtx;
    use crate::state::{ContentBlock, Message, MessageContent};

    fn apply(base: AgentState, delta: nexus_graph::StateDelta) -> AgentState {
        let mut s = base;
        s.merge(delta);
        s
    }

    fn ok_result(t: &str) -> CriterionResult {
        CriterionResult {
            criterion_type: t.to_string(),
            outcome: CriterionOutcome::Passed,
            evidence: json!({}),
        }
    }

    fn fail_result(t: &str, evidence: Value) -> CriterionResult {
        CriterionResult {
            criterion_type: t.to_string(),
            outcome: CriterionOutcome::Failed,
            evidence,
        }
    }

    fn inconclusive_result(t: &str) -> CriterionResult {
        CriterionResult {
            criterion_type: t.to_string(),
            outcome: CriterionOutcome::Inconclusive,
            evidence: json!({"skipped_reason": "non interrogabile"}),
        }
    }

    /// Ctx di test. Il motore criteri NON e' nel ctx: vive nel nodo
    /// (`FinalGateNode::new`). PgPool lazy (il final_gate non scrive DB), LLM stub
    /// mai chiamato (nodo deterministico).
    fn ctx_with() -> AgentNodeCtx {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy non si connette");
        AgentNodeCtx {
            isolation_available: false,
            db: pool,
            llm: Arc::new(StubLlmGateway::with_text("non usato")),
            tools: Arc::new(StubToolExecutor::with_success(json!("ok"))),
            emit: Arc::new(NullEventSink),
            cfg: RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            advisory_gate: None,
        step_gate: None,
        }
    }

    fn node_with(
        cfg: FinalGateConfig,
        criteria: Arc<dyn crate::runtime::ports::CriteriaRunner>,
    ) -> FinalGateNode {
        FinalGateNode::new(
            cfg,
            RoutingConfig::default(),
            criteria,
            std::sync::Arc::new(StubMetaStepStore::default()),
        )
    }

    /// Messaggio AI con un tool_use mutativo: rende il task "software"
    /// strutturalmente (write_file e' in fs_mutator_tools), cosi' il gate non
    /// pass-through-a per non-software.
    fn ai_mutation() -> Message {
        Message::Ai {
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: "c1".into(),
                name: "write_file".into(),
                input: json!({"path": "src/main.tsx"}),
                thought_signature: None,
            }]),
            tool_calls: vec![],
            reasoning: None,
            thinking_signature: None,
        }
    }

    /// Stato software (mutazione fs in history) che entra nel gate.
    fn software_state() -> AgentState {
        AgentState {
            messages: vec![ai_mutation()],
            thread_id: Some("11111111-1111-1111-1111-111111111111".to_string()),
            ..Default::default()
        }
    }

    // ── Gate pass-through ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn gate_enabled_off_passthrough() {
        let cfg = FinalGateConfig {
            enabled: false,
            ..Default::default()
        };
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![ok_result(
            "no_orphan_imported",
        )]));
        let node = node_with(cfg, runner.clone());
        let ctx = ctx_with();
        let st = software_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        // Pass-through: nessun cambio di stop_reason / final_gate_passed.
        assert_eq!(out.stop_reason, None);
        assert_eq!(out.final_gate_passed, None);
        // Criteri NON eseguiti (gate prima dei criteri).
        assert!(runner.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn gate_non_software_passthrough() {
        // Nessuna mutazione fs + intent non-software -> non software -> {}.
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![ok_result(
            "no_orphan_imported",
        )]));
        let node = node_with(FinalGateConfig::default(), runner.clone());
        let ctx = ctx_with();
        let st = AgentState {
            user_intent: Some("chat".into()),
            ..Default::default()
        };
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, None);
        assert!(runner.seen.lock().unwrap().is_empty());
    }

    /// REGRESSIONE (bacheca-attivita, run 1845a0ce, 2026-07-30): il revisore
    /// aveva dichiarato `review_verdict: fail` a 152s dei 240s di budget, ma un
    /// `run_command` in history (e' in `fs_mutator_tools`) lo rendeva "software
    /// task" e il gate gli applicava il contratto dell'ESECUTORE: `npx tsc`
    /// falliva proprio per i difetti appena certificati dal verdetto, il
    /// giudice — senza tool di scrittura — veniva rimandato "in correzione" e
    /// ciclava fino al wall-clock, dove il verdetto valido veniva scartato come
    /// timeout.
    ///
    /// Il test attraversa il NODO reale con lo stesso segnale dell'incidente
    /// (`run_command` come mutator, criteri che FALLISCONO) e arriva alla
    /// CONSEGUENZA sull'edge reale (`route_after_final_gate`), per i tre canali
    /// di ruolo.
    ///
    /// MUTAZIONE: rimuovendo il guard `declared_role_channel` dal nodo, la
    /// route diventa `Executor` (il rimando in correzione del difetto reale) e
    /// `seen` non e' piu' vuoto.
    #[tokio::test]
    async fn il_giudice_non_riceve_il_contratto_dell_esecutore() {
        use crate::routing::{route_after_final_gate, NodeTarget};

        let ai_run_command = Message::Ai {
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: "c1".into(),
                name: "run_command".into(),
                input: json!({"command": "node -e \"require('sqlite3')\""}),
                thought_signature: None,
            }]),
            tool_calls: vec![],
            reasoning: None,
            thinking_signature: None,
        };
        let giudizio = json!({"verdict": "fail", "summary": "bug bloccante"});
        for canale in ["review_verdict", "advisory_verdict", "debate_position"] {
            let runner = Arc::new(StubCriteriaRunner::with_results(vec![fail_result(
                "run_command",
                json!({"excerpt": "error TS2307"}),
            )]));
            let node = node_with(FinalGateConfig::default(), runner.clone());
            let ctx = ctx_with();
            let mut st = AgentState {
                messages: vec![ai_run_command.clone()],
                thread_id: Some("11111111-1111-1111-1111-111111111111".to_string()),
                ..Default::default()
            };
            match canale {
                "review_verdict" => st.review_verdict = Some(giudizio.clone()),
                "advisory_verdict" => st.advisory_verdict = Some(giudizio.clone()),
                _ => st.debate_position = Some(giudizio.clone()),
            }

            let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));

            assert_eq!(
                route_after_final_gate(&out),
                NodeTarget::Learner,
                "una figura col giudizio dichiarato ({canale}) chiude, non va in correzione"
            );
            assert!(
                runner.seen.lock().unwrap().is_empty(),
                "i criteri d'ambiente non si applicano a una figura ({canale})"
            );
            // Il gate non ha giudicato: nessun verdetto proprio, nessun ciclo.
            assert_eq!(out.final_gate_verdict, None);
            assert_eq!(out.final_gate_cycle, None);
        }
    }

    // ── Ramo PASSED ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn passed_chiude_con_final_gate_passed() {
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![
            ok_result("no_orphan_imported"),
            ok_result("outputs_exist"),
        ]));
        let node = node_with(FinalGateConfig::default(), runner.clone());
        let ctx = ctx_with();
        let st = software_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.final_gate_passed, Some(true));
        assert_eq!(out.final_gate_cycle, Some(0));
        // Criteri eseguiti una volta.
        let seen = runner.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
    }

    // ── Turno di grazia completion ──────────────────────────────────────────────

    #[test]
    fn only_completion_confirmed_failed_predicato() {
        // solo completion_confirmed fallito (oggettivi ok) -> true
        assert!(FinalGateNode::only_completion_confirmed_failed(&[
            ok_result("run_command"),
            fail_result("completion_confirmed", json!({})),
        ]));
        // un criterio OGGETTIVO fallito -> false (non e' solo la firma)
        assert!(!FinalGateNode::only_completion_confirmed_failed(&[
            fail_result("run_command", json!({})),
            fail_result("completion_confirmed", json!({})),
        ]));
        // nessun fallito -> false (e' gia' PASSED)
        assert!(!FinalGateNode::only_completion_confirmed_failed(&[
            ok_result("run_command")
        ]));
    }

    #[tokio::test]
    async fn completion_grace_concede_turno_extra_su_solo_completion_confirmed() {
        // Tutti gli oggettivi passano, solo completion_confirmed fallisce, al cap
        // (cycle == max_cycles=2): il gate concede UN turno mirato per task_complete
        // invece di chiudere failed.
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![
            ok_result("no_orphan_imported"),
            ok_result("run_command"),
            fail_result(
                "completion_confirmed",
                json!({"excerpt": "manca task_complete"}),
            ),
        ]));
        let node = node_with(FinalGateConfig::default(), runner);
        let ctx = ctx_with();
        let st = AgentState {
            final_gate_cycle: Some(1), // -> cycle diventa 2 == max_cycles
            ..software_state()
        };
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        // NON boccia: rimanda all'executor chiedendo task_complete.
        assert_eq!(out.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(
            out.final_gate_passed, None,
            "il turno di grazia NON registra final_gate_passed=false"
        );
        let msg = serde_json::to_string(&out.messages).unwrap();
        assert!(
            msg.contains("task_complete"),
            "il turno mirato chiede task_complete: {msg}"
        );
    }

    /// La grazia RESTITUISCE il turno: cio' che ha misurato descrive l'albero di
    /// adesso, non l'esito del run. Il delta deve percio' dichiarare una PROMESSA
    /// (verdetto non terminale + porta lasciata aperta a UNA rimisura), non un
    /// verdetto positivo definitivo.
    ///
    /// Mutazione che lo rende rosso: riportare il delta a
    /// `final_gate_verdict = ObjectivePassedSignatureMissing` e
    /// `final_gate_cycle = cycle`.
    #[tokio::test]
    async fn completion_grace_promette_una_rimisura_e_non_un_esito() {
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![
            ok_result("codice_eseguibile"),
            fail_result("completion_confirmed", json!({})),
        ]));
        let node = node_with(FinalGateConfig::default(), runner);
        let ctx = ctx_with();
        let st = AgentState {
            final_gate_cycle: Some(1), // -> cycle diventa 2 == max_cycles
            ..software_state()
        };
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(
            out.final_gate_verdict,
            Some(FinalGateVerdict::PassedPendingSignature),
            "il turno di grazia non ha ancora l'ultima parola: il verdetto non e' un esito"
        );
        assert_eq!(
            out.final_gate_grace_granted,
            Some(true),
            "la grazia si dichiara con un campo suo, non col contatore"
        );
        assert_eq!(
            out.final_gate_cycle,
            Some(1),
            "il ciclo lascia aperta la porta ordinaria del gate per UNA rimisura"
        );
        assert_eq!(
            out.final_gate_unverified,
            Some(true),
            "se il run muore prima della rimisura, l'esito onesto e' 'svolto ma non verificato'"
        );
    }

    /// REGOLA O — il rientro promesso deve avvenire per la strada VERA. Il test
    /// precedente di questa famiglia fabbricava `final_gate_cycle = 2`, uno stato
    /// che il routing non consegna MAI al nodo (`final_gate_eligible` pretende
    /// `cycle < max_cycles`): era verde e misurava un ingresso che non avviene.
    ///
    /// Qui lo stato lo produce il gate stesso e la domanda la si pone alle DUE
    /// porte reali: il turno ordinario e il cap delle iterazioni — che nel run
    /// misurato il 19/08/2026 era quella percorsa.
    #[tokio::test]
    async fn dopo_la_grazia_il_routing_riporta_al_gate_anche_al_cap() {
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![
            ok_result("codice_eseguibile"),
            fail_result("completion_confirmed", json!({})),
        ]));
        let node = node_with(FinalGateConfig::default(), runner);
        let ctx = ctx_with();
        let st = AgentState {
            final_gate_cycle: Some(1),
            ..software_state()
        };
        let dopo_grazia = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));

        let cfg = RoutingConfig::default();
        assert!(
            crate::routing::signals::final_gate_eligible(&dopo_grazia, &cfg),
            "porta ordinaria: il gate deve poter rimisurare"
        );

        // Porta del CAP: `route_after_executor` ramo (4). Il marcatore "il gate
        // non e' mai entrato" e' falso — un verdetto c'e' — quindi senza il
        // segnale di rimisura promessa si chiuderebbe senza verificare.
        let al_cap = AgentState {
            iterations: Some(cfg.iteration_cap),
            stop_reason: Some(StopReason::EndTurn),
            pending_tool_uses: Some(vec![]),
            ..dopo_grazia.clone()
        };
        assert_eq!(
            crate::routing::route_after_executor(&al_cap, &cfg),
            crate::routing::NodeTarget::FinalGate,
            "al cap delle iterazioni la rimisura promessa deve avvenire"
        );
    }

    /// LA CATENA del difetto misurato il 19/08/2026 su `t3-codice-eseguibile`:
    /// grazia concessa su un albero che in quell'istante passava davvero, agente
    /// che poi cambia il codice e lo rompe, gate che RIMISURA. Il verdetto finale
    /// deve venire dalla seconda misura, non dalla prima.
    ///
    /// Mutazione che lo rende rosso: far tornare la grazia a un verdetto
    /// definitivo (il gate non rientra e il run chiude sulla misura vecchia).
    #[tokio::test]
    async fn la_rimisura_dopo_la_grazia_boccia_il_codice_rotto() {
        let ctx = ctx_with();
        // Giro 1: il file si carica, manca solo la firma -> grazia.
        let primo = node_with(
            FinalGateConfig::default(),
            Arc::new(StubCriteriaRunner::with_results(vec![
                ok_result("codice_eseguibile"),
                fail_result("completion_confirmed", json!({})),
            ])),
        );
        let st = AgentState {
            final_gate_cycle: Some(1),
            // Escalation gia' esaurite: qui si misura la CHIUSURA, non la
            // promozione di modello (che ha i suoi test).
            extra: [("auto_escalations".to_string(), json!(3))]
                .into_iter()
                .collect(),
            ..software_state()
        };
        let dopo_grazia = apply(st.clone(), primo.run(&st, &ctx).await.expect("run ok"));

        // Giro 2: l'agente ha tolto cio' che faceva caricare il file.
        let secondo = node_with(
            FinalGateConfig::default(),
            Arc::new(StubCriteriaRunner::with_results(vec![
                fail_result("codice_eseguibile", json!({"verdict": "code_broken"})),
                fail_result("completion_confirmed", json!({})),
            ])),
        );
        let fine = apply(
            dopo_grazia.clone(),
            secondo
                .run(&dopo_grazia, &ctx)
                .await
                .expect("run ok"),
        );
        assert_eq!(
            fine.final_gate_passed,
            Some(false),
            "un criterio oggettivo fallito alla rimisura e' il verdetto del run"
        );
        assert_eq!(
            fine.final_gate_verdict,
            Some(FinalGateVerdict::FailedFinal),
            "nessuna seconda grazia: il verdetto e' terminale"
        );
    }

    /// Il caso PER CUI la grazia esiste non si rompe: chi ha superato tutti gli
    /// oggettivi e non firma nemmeno dopo il turno mirato chiude comunque BENE —
    /// ma ora sulla misura fresca, col gate che CHIUDE invece di restituire il
    /// turno, che e' cio' che rende vera l'affermazione del verdetto.
    #[tokio::test]
    async fn grazia_gia_concessa_e_firma_ancora_assente_chiude_bene() {
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![
            ok_result("codice_eseguibile"),
            fail_result("completion_confirmed", json!({})),
        ]));
        let node = node_with(FinalGateConfig::default(), runner);
        let ctx = ctx_with();
        let st = AgentState {
            final_gate_cycle: Some(1),
            final_gate_grace_granted: Some(true),
            final_gate_verdict: Some(FinalGateVerdict::PassedPendingSignature),
            ..software_state()
        };
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.gate_routing, Some(GateRouting::Chiude));
        assert_eq!(
            out.final_gate_passed,
            Some(true),
            "i criteri oggettivi sono superati sull'albero FINALE: non e' una bocciatura"
        );
        assert_eq!(
            out.final_gate_verdict,
            Some(FinalGateVerdict::ObjectivePassedSignatureMissing),
            "qui il gate chiude: l'affermazione del verdetto resta vera"
        );
        assert_eq!(
            out.final_gate_unverified,
            Some(false),
            "profilo di verifica presente e nessun criterio non misurabile: chiusura verificata"
        );
    }

    // ── Ramo FORCED / CAP ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn forced_close_chiude_con_final_gate_passed_false() {
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![fail_result(
            "no_orphan_imported",
            json!({"verdict": "placeholder rilevato"}),
        )]));
        let node = node_with(FinalGateConfig::default(), runner.clone());
        let ctx = ctx_with();
        let mut st = software_state();
        st.forced_close_unverified = Some(true);
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        // Chiude (end_turn) col verdetto NEGATIVO esplicito final_gate_passed=false
        // (il finalizzatore mappa FailedDiagnosed e annota il resoconto).
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.final_gate_passed, Some(false));
        assert_eq!(out.final_gate_cycle, Some(0));
        // Nessun messaggio iniettato (chiusura, non re-executor).
        assert_eq!(out.messages.len(), 1, "solo il messaggio AI preesistente");
    }

    #[tokio::test]
    async fn cap_raggiunto_chiude_senza_re_executor() {
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![fail_result(
            "no_orphan_imported",
            json!({"verdict": "fail"}),
        )]));
        // max_cycles=2, final_gate_cycle gia' a 1 -> cycle diventa 2 >= 2 -> cap.
        // Escalation su non-convergenza OFF: backstop di chiusura secca (comportamento
        // storico bit-identico). Con il flag ON il gate cederebbe all'executor (test
        // cap_nonconvergenza_delega_escalation).
        let node = node_with(
            FinalGateConfig {
                escalate_on_nonconvergence: false,
                ..FinalGateConfig::default()
            },
            runner.clone(),
        );
        let ctx = ctx_with();
        let mut st = software_state();
        st.final_gate_cycle = Some(1);
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.final_gate_passed, Some(false));
        assert_eq!(out.final_gate_cycle, Some(0));
    }

    // ── Ramo NON-CONVERGENZA -> escalation (mig 0577) ───────────────────────────

    #[tokio::test]
    async fn cap_nonconvergenza_delega_escalation() {
        // Cap con criterio OGGETTIVO fallito + flag ON + escalation disponibile
        // (auto_escalations=0 < max): il gate NON chiude secco, cede il turno
        // all'executor posando FINAL_GATE_ESCALATION_KEY (segnale strutturato) e
        // azzerando final_gate_cycle (cicli freschi per il modello promosso).
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![fail_result(
            "no_orphan_imported",
            json!({"verdict": "fail"}),
        )]));
        let node = node_with(FinalGateConfig::default(), runner);
        let ctx = ctx_with();
        let mut st = software_state();
        st.final_gate_cycle = Some(1); // cycle -> 2 == max_cycles
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(
            out.final_gate_passed, None,
            "la delega NON registra un verdetto negativo"
        );
        assert_eq!(
            out.final_gate_cycle,
            Some(0),
            "il promosso riparte con cicli di gate freschi"
        );
        assert_eq!(
            out.extra
                .get(FINAL_GATE_ESCALATION_KEY)
                .and_then(Value::as_bool),
            Some(true),
            "il flag di escalation e' posato per l'executor"
        );
        // Il verdetto fallito e' iniettato come contesto per il modello promosso.
        assert_eq!(out.messages.len(), 2);
    }

    #[tokio::test]
    async fn cap_nonconvergenza_escalation_esaurita_chiude_secco() {
        // Stesso scenario ma auto_escalations gia' al tetto (== max_escalations):
        // budget di promozioni esaurito -> il gate chiude secco (backstop), niente
        // flag, niente loop.
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![fail_result(
            "no_orphan_imported",
            json!({"verdict": "fail"}),
        )]));
        let node = node_with(FinalGateConfig::default(), runner);
        let ctx = ctx_with();
        let mut st = software_state();
        st.final_gate_cycle = Some(1);
        st.extra
            .insert("auto_escalations".to_string(), json!(3)); // == max_escalations default
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.final_gate_passed, Some(false));
        assert_eq!(out.final_gate_cycle, Some(0));
        assert!(
            out.extra.get(FINAL_GATE_ESCALATION_KEY).is_none(),
            "escalation esaurita: nessun flag posato"
        );
    }

    #[tokio::test]
    async fn cap_solo_completion_confirmed_non_delega() {
        // Cap con SOLO completion_confirmed fallito (oggettivi ok) e grazia gia'
        // spesa (cycle > max_cycles): il lavoro e' di fatto completo -> chiude, NON
        // spreca un'escalation (esclusione !only_completion_confirmed_failed).
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![
            ok_result("run_command"),
            fail_result("completion_confirmed", json!({})),
        ]));
        let node = node_with(FinalGateConfig::default(), runner);
        let ctx = ctx_with();
        let mut st = software_state();
        st.final_gate_cycle = Some(2); // cycle -> 3 > max_cycles: no grazia, no delega
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.final_gate_passed, Some(false));
        assert!(out.extra.get(FINAL_GATE_ESCALATION_KEY).is_none());
    }

    // ── Ramo FAIL (re-executor) ─────────────────────────────────────────────────

    #[tokio::test]
    async fn fail_re_executor_inietta_human_message() {
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![fail_result(
            "no_orphan_imported",
            json!({"verdict": "hello-world non raggiunge il design importato"}),
        )]));
        let node = node_with(FinalGateConfig::default(), runner.clone());
        let ctx = ctx_with();
        // cycle parte da 0 -> 1, max_cycles 2, non forced -> FAIL.
        let st = software_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(out.final_gate_cycle, Some(1));
        assert_eq!(out.final_gate_passed, None);
        // pending_tool_uses azzerato a lista vuota.
        assert_eq!(out.pending_tool_uses, Some(vec![]));
        // Da qui il run lavora SOTTO un rimando, e chi giudica i suoi passi
        // deve poterlo sapere: senza questo campo, un comando che ripara cio'
        // che il gate ha appena contestato risulta estraneo alla richiesta
        // dell'utente e viene rifiutato (misurato il 12/08/2026).
        //
        // MUTAZIONE: togliere `criteri_in_correzione` dal delta del ramo FAIL ->
        // questo test cade, e col difetto reale (il giudice torna cieco).
        assert!(
            !out.criteri_in_correzione.is_empty(),
            "il rimando deve dichiarare PER COSA il run sta correggendo"
        );
        // HumanMessage del gate accodato (AI preesistente + il nuovo Human).
        assert_eq!(out.messages.len(), 2);
        let last = out.messages.last().expect("ultimo messaggio");
        match last {
            Message::Human { content } => {
                let text = content.flatten_text();
                assert!(text.contains("<final_gate_failed cycle=\"1/2\">"));
                assert!(text.contains("hello-world non raggiunge"));
                assert!(text.contains("DIRETTIVE (fail-closed):"));
            }
            other => panic!("atteso HumanMessage, trovato {other:?}"),
        }
    }

    // ── build_criteria deterministico ────────────────────────────────────────────

    /// IL NODO E' L'UNICO PUNTO CHE VEDE LO STATO, ed e' li' che il piano entra
    /// nella spec (mig 0737). Senza questo test la catena resterebbe verde con
    /// un criterio che nasce SEMPRE VUOTO: le prove del Consiglio viaggiano
    /// nell'`extra`, quelle dell'agente nella dichiarazione di chiusura, e chi
    /// non le raccoglie qui non le raccoglie mai piu' (regola O).
    ///
    /// Copre anche la precedenza «giudice != worker»: il Consiglio viene PRIMA,
    /// e l'agente che ridichiara la stessa prova non se ne intesta la
    /// provenienza.
    ///
    /// MUTAZIONE ESEGUITA: togliere `PianoDiVerifica::da_dichiarazione` dalla
    /// composizione in `build_criteria` lascia una sola prova nella spec e il
    /// test rosseggia — l'agente perderebbe il proprio canale, e con lui il
    /// caso in cui NESSUN apparato advisory ha deliberato.
    #[test]
    fn build_criteria_raccoglie_il_piano_dallo_stato_e_dalla_dichiarazione() {
        use crate::decisions::piano_di_verifica::{
            criterio_piano, OriginePiano, ParametriPiano, PianoDiVerifica, CHIAVE_PROVE,
            CRITERION_TYPE,
        };
        let prova_consiglio = serde_json::json!({
            "descrizione": "il file di test parte col runner del progetto",
            "comando": "node --test calcolatrice.test.js",
            "attesa": {"tipo": "exit_code", "codice": 0},
            "origine": "council",
        });
        let cfg = FinalGateConfig {
            structural_criteria_enabled: false,
            piano_verifica_criterion: criterio_piano(
                None,
                &ParametriPiano {
                    abilitato: true,
                    timeout_s: 60.0,
                    max_prove: 20,
                },
            ),
            ..Default::default()
        };
        let node = node_with(cfg, Arc::new(StubCriteriaRunner::with_results(vec![])));
        let mut st = software_state();
        st.extra.insert(
            crate::decisions::PIANO_VERIFICA_KEY.to_string(),
            Value::Array(vec![prova_consiglio.clone()]),
        );
        st.declared_outcome = Some(serde_json::json!({
            "outcome": "done",
            "summary": "fatto",
            "prove": [
                prova_consiglio,
                {
                    "descrizione": "la sorgente si carica",
                    "comando": "node --check calcolatrice.js",
                    "attesa": {"tipo": "exit_code", "codice": 0},
                },
            ],
        }));

        let crits = node.build_criteria(&st);
        let criterio = crits
            .iter()
            .find(|c| c.criterion_type == CRITERION_TYPE)
            .expect("il criterio del piano e' fra quelli costruiti");
        let piano = PianoDiVerifica::from_value(criterio.spec.get(CHIAVE_PROVE));
        assert_eq!(piano.len(), 2, "la prova ripetuta non si conta due volte");
        assert_eq!(piano.prove[0].origine, OriginePiano::Consiglio);
        assert_eq!(piano.prove[1].origine, OriginePiano::Agente);
        assert_eq!(piano.prove[1].comando, "node --check calcolatrice.js");
    }

    /// PIANO ASSENTE: il criterio nasce lo stesso, con un piano VUOTO scritto
    /// nella spec, e il resto del gate e' bit-identico a prima.
    ///
    /// E' il requisito «il criterio nasce anche a configurazione mancante»: un
    /// criterio che sparisse quando nessuno ha dichiarato prove renderebbe il
    /// silenzio indistinguibile da una verifica, che e' il difetto del 17/08.
    #[test]
    fn senza_prove_il_criterio_nasce_col_piano_vuoto_e_il_resto_non_cambia() {
        use crate::decisions::piano_di_verifica::{
            criterio_piano, ParametriPiano, CHIAVE_PROVE, CRITERION_TYPE,
        };
        let parametri = ParametriPiano {
            abilitato: true,
            timeout_s: 60.0,
            max_prove: 20,
        };
        let st = software_state();
        let spento = node_with(
            FinalGateConfig {
                structural_criteria_enabled: false,
                ..Default::default()
            },
            Arc::new(StubCriteriaRunner::with_results(vec![])),
        );
        let acceso = node_with(
            FinalGateConfig {
                structural_criteria_enabled: false,
                piano_verifica_criterion: criterio_piano(None, &parametri),
                ..Default::default()
            },
            Arc::new(StubCriteriaRunner::with_results(vec![])),
        );
        let prima: Vec<String> = spento
            .build_criteria(&st)
            .iter()
            .map(|c| c.criterion_type.clone())
            .collect();
        let dopo = acceso.build_criteria(&st);
        let tipi: Vec<String> = dopo.iter().map(|c| c.criterion_type.clone()).collect();
        assert_eq!(
            tipi,
            [prima, vec![CRITERION_TYPE.to_string()]].concat(),
            "il criterio si accoda e non riordina nulla"
        );
        let criterio = dopo.last().expect("il criterio del piano");
        assert_eq!(
            criterio.spec[CHIAVE_PROVE],
            serde_json::json!([]),
            "il piano vuoto si scrive lo stesso: «nessuno ha dichiarato prove» \
             non e' «non ho letto il piano»"
        );
    }

    #[test]
    fn build_criteria_ordine_e_opzionali() {
        // Questo test fissa l'ORDINE storico dei criteri 1-6: i criteri
        // strutturali (ADR 0018 leva 3, blocco 7) sono spenti qui e coperti
        // dal test dedicato build_criteria_strutturali.
        let base_cfg = FinalGateConfig {
            structural_criteria_enabled: false,
            ..Default::default()
        };
        // Senza verify_steps e con runtime_check ON ma log_command vuoto:
        // solo no_orphan + outputs_exist (NESSUN comando generico di ripiego,
        // ADR 0036: senza profilo, nessun criterio comando).
        let node = node_with(
            base_cfg.clone(),
            Arc::new(StubCriteriaRunner::with_results(vec![])),
        );
        let st = software_state();
        let crits = node.build_criteria(&st);
        let types: Vec<&str> = crits.iter().map(|c| c.criterion_type.as_str()).collect();
        assert_eq!(types, vec!["no_orphan_imported", "outputs_exist"]);

        // Con log_command + profilo per-ambiente (ADR 0036): un run_command
        // PER STEP nell'ordine del profilo (qui typecheck poi build).
        let profile_steps = vec![
            VerifyStepCmd {
                step: "typecheck".to_string(),
                command: "npx tsc --noEmit".to_string(),
                working_dir: None,
                baseline_exit_code: None,
            },
            VerifyStepCmd {
                step: "build".to_string(),
                command: "pnpm build".to_string(),
                working_dir: Some("app".to_string()),
                baseline_exit_code: None,
            },
        ];
        let cfg = FinalGateConfig {
            log_command: "docker compose logs".to_string(),
            verify_steps: profile_steps.clone(),
            ..base_cfg.clone()
        };
        let node2 = node_with(cfg, Arc::new(StubCriteriaRunner::with_results(vec![])));
        let crits2 = node2.build_criteria(&st);
        let types2: Vec<&str> = crits2.iter().map(|c| c.criterion_type.as_str()).collect();
        assert_eq!(
            types2,
            vec![
                "no_orphan_imported",
                "outputs_exist",
                "service_logs_clean",
                "run_command",
                "run_command"
            ]
        );
        // Ogni step porta comando, etichetta, max_output_chars, working_dir e
        // timeout; l'ordine e' quello del profilo (typecheck PRIMA della build:
        // per Vite la sola build non type-checka).
        let tc = &crits2[3];
        assert_eq!(tc.spec["command"], json!("npx tsc --noEmit"));
        assert_eq!(tc.spec["label"], json!("typecheck"));
        let build = crits2.last().expect("build criterion");
        assert_eq!(build.spec["command"], json!("pnpm build"));
        assert_eq!(build.spec["label"], json!("build"));
        assert_eq!(build.spec["max_output_chars"], json!(4000));
        assert_eq!(build.spec["working_dir"], json!("app"));
        assert_eq!(build.timeout_s, Some(180.0));
        assert_eq!(build.expected, json!({ "exit_code": 0 }));

        // Con anche gli endpoint CONFIGURATI risolti a monte: http ULTIMO
        // nell'ordine (`final_gate.py:468-470`). Le spec arrivano pronte dal
        // risolutore DB (`load_configured_endpoint_criteria`): il nodo le accoda 1:1.
        let endpoint = CriterionSpec {
            provenance: CriterionProvenance::Gate,
            criterion_type: "http".to_string(),
            spec: json!({ "url": "http://localhost:3000/api/login", "method": "POST" }),
            expected: json!({ "status": 200 }),
            timeout_s: Some(15.0),
        };
        let cfg3 = FinalGateConfig {
            log_command: "docker compose logs".to_string(),
            verify_steps: profile_steps.clone(),
            endpoint_criteria: vec![endpoint.clone()],
            ..base_cfg.clone()
        };
        let node3 = node_with(cfg3, Arc::new(StubCriteriaRunner::with_results(vec![])));
        let crits3 = node3.build_criteria(&st);
        let types3: Vec<&str> = crits3.iter().map(|c| c.criterion_type.as_str()).collect();
        assert_eq!(
            types3,
            vec![
                "no_orphan_imported",
                "outputs_exist",
                "service_logs_clean",
                "run_command",
                "run_command",
                "http"
            ]
        );
        // Il criterio endpoint e' accodato 1:1 (spec/expected/timeout invariati).
        assert_eq!(crits3.last().expect("endpoint criterion"), &endpoint);

        // Endpoint risolto SENZA build/log: comunque accodato dopo i 2 sempre-on.
        let cfg4 = FinalGateConfig {
            endpoint_criteria: vec![endpoint.clone()],
            ..base_cfg.clone()
        };
        let node4 = node_with(cfg4, Arc::new(StubCriteriaRunner::with_results(vec![])));
        let crits4 = node4.build_criteria(&st);
        let types4: Vec<&str> = crits4.iter().map(|c| c.criterion_type.as_str()).collect();
        assert_eq!(types4, vec!["no_orphan_imported", "outputs_exist", "http"]);
    }

    // ── prove HTTP funzionali: dalla dichiarazione dell'agente ──────────────────

    /// L'app del caso reale: porta assegnata dal bucket di progetto, endpoint di
    /// lettura e scrittura sulla stessa risorsa.
    const URL_SPESE: &str = "http://localhost:24817/api/expenses";
    const URL_HEALTH: &str = "http://localhost:24817/api/health";

    /// Stato di un run che ha creato un CRUD: mutazione fs + una GET provata a
    /// mano dall'agente (`curl`), esattamente la history del caso reale
    /// (gestione-spese, 2026-07-28).
    fn crud_state() -> AgentState {
        let curl = Message::Ai {
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: "c2".into(),
                name: "run_command".into(),
                input: json!({"command": format!("curl -s {URL_SPESE}")}),
                thought_signature: None,
            }]),
            tool_calls: vec![],
            reasoning: None,
            thinking_signature: None,
        };
        let mut st = software_state();
        st.messages.push(curl);
        st
    }

    /// Dichiarazione di chiusura come la produce la PRODUZIONE: l'input grezzo di
    /// `task_complete` passa dal punto unico `normalize_declared_outcome`, lo
    /// stesso che scrive `state.declared_outcome` nel tool_dispatch. Costruirla a
    /// mano fisserebbe l'assunto che il test vuole verificare (regola O).
    fn declared_from_tool_input(tool_input: Value) -> Value {
        crate::decisions::tool_dispatch::normalize_declared_outcome(&tool_input)
            .expect("task_complete valido")
    }

    #[test]
    fn endpoint_dichiarati_diventano_criteri_http() {
        // Il difetto: il criterio HTTP non veniva costruito MAI. Qui si parte da
        // cio' che l'agente DICHIARA e si verifica che il gate abbia le prove —
        // inclusa la POST, il metodo che nessuna altra fonte espone (l'agente
        // aveva provato da se' solo la GET).
        let node = node_with(
            FinalGateConfig::default(),
            Arc::new(StubCriteriaRunner::with_results(vec![])),
        );
        let mut st = crud_state();
        st.declared_outcome = Some(declared_from_tool_input(json!({
            "outcome": "done",
            "summary": "CRUD spese completo",
            "endpoints": [
                {"method": "GET", "url": URL_SPESE},
                {"method": "POST", "url": URL_SPESE,
                 "body": {"amount": 12.5, "description": "prova gate", "category": "test"}},
            ],
        })));
        let crits = node.build_criteria(&st);
        let http: Vec<&CriterionSpec> = crits
            .iter()
            .filter(|c| c.criterion_type == "http")
            .collect();
        assert_eq!(http.len(), 2, "una prova per endpoint dichiarato: {crits:?}");
        assert_eq!(http[0].spec["method"], json!("GET"));
        assert_eq!(http[1].spec["method"], json!("POST"));
        assert_eq!(http[1].spec["url"], json!(URL_SPESE));
        // Lo status atteso e' la famiglia 2xx del punto unico (non una lista
        // ricopiata qui, che divergerebbe): il 500 dell'incidente e' fuori.
        assert_eq!(
            http[1].expected["status"],
            json!(crate::decisions::endpoint_probes::DEFAULT_SUCCESS_STATUSES)
        );
        // Regola M: si decide sullo status, il corpo non entra nella decisione.
        assert!(http[1].expected.get("body_contains").is_none());
    }

    #[test]
    fn endpoint_check_off_nessuna_prova() {
        // Kill-switch DB-driven: OFF -> nessun criterio http, ne' configurato ne'
        // dichiarato (comportamento storico).
        let cfg = FinalGateConfig {
            endpoint_check_enabled: false,
            endpoint_criteria: vec![CriterionSpec {
                provenance: CriterionProvenance::Gate,
                criterion_type: "http".to_string(),
                spec: json!({ "url": URL_HEALTH }),
                expected: json!({ "status": 200 }),
                timeout_s: Some(15.0),
            }],
            ..FinalGateConfig::default()
        };
        let node = node_with(cfg, Arc::new(StubCriteriaRunner::with_results(vec![])));
        let mut st = crud_state();
        st.declared_outcome = Some(declared_from_tool_input(json!({
            "outcome": "done",
            "summary": "fatto",
            "endpoints": [{"method": "GET", "url": URL_SPESE}],
        })));
        assert!(!node
            .build_criteria(&st)
            .iter()
            .any(|c| c.criterion_type == "http"));
    }

    /// Runner che si comporta come l'applicazione dell'incidente: ogni criterio
    /// passa, TRANNE la chiamata di scrittura verso `/api/expenses`, che risponde
    /// 500. Decide sugli spec RICEVUTI, non su una lista fissa: se il gate non
    /// costruisce la prova, il 500 non lo vede nessuno e il run chiude "superato"
    /// — che e' esattamente il difetto.
    struct AppConLaPostRotta;

    #[async_trait]
    impl crate::runtime::ports::CriteriaRunner for AppConLaPostRotta {
        async fn run(
            &self,
            criteria: Vec<CriterionSpec>,
        ) -> Result<Vec<CriterionResult>, crate::runtime::ports::PortError> {
            Ok(criteria
                .into_iter()
                .map(|c| {
                    let scrittura = c.criterion_type == "http"
                        && c.spec["method"] == json!("POST")
                        && c.spec["url"]
                            .as_str()
                            .unwrap_or("")
                            .ends_with("/api/expenses");
                    CriterionResult {
                        criterion_type: c.criterion_type,
                        outcome: CriterionOutcome::measured(!scrittura),
                        evidence: if scrittura {
                            json!({"status": 500, "verdict": "POST /api/expenses -> 500 (atteso 200/201/202/204)"})
                        } else {
                            json!({})
                        },
                    }
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn la_post_rotta_boccia_il_gate() {
        // CONSEGUENZA (regola O): non basta che il criterio compaia nella lista,
        // deve FAR FALLIRE la chiusura. Con la POST che risponde 500 il gate non
        // chiude "superato": rimanda all'agente il blocco di correzione.
        let node = node_with(FinalGateConfig::default(), Arc::new(AppConLaPostRotta));
        let ctx = ctx_with();
        let mut st = crud_state();
        st.action_oriented = Some(true);
        st.tools_json = Some(vec![json!({"name": "write_file"})]);
        st.declared_outcome = Some(declared_from_tool_input(json!({
            "outcome": "done",
            "summary": "CRUD spese completo",
            "endpoints": [
                {"method": "GET", "url": URL_SPESE},
                {"method": "POST", "url": URL_SPESE,
                 "body": {"amount": 12.5}},
            ],
        })));
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(
            out.stop_reason,
            Some(StopReason::ToolUse),
            "una POST che risponde 500 non chiude il turno: torna all'agente"
        );
        assert_ne!(
            out.final_gate_passed,
            Some(true),
            "il gate non puo' dichiararsi superato con una scrittura rotta"
        );
        assert_eq!(out.final_gate_cycle, Some(1), "ciclo di correzione aperto");
        // Il blocco iniettato NOMINA la chiamata fallita: senza, il re-loop
        // sarebbe cieco (l'agente non saprebbe quale endpoint riparare).
        let ultimo = out.messages.last().expect("messaggio iniettato");
        let testo = match ultimo {
            Message::Human { content } => content.flatten_text(),
            altro => panic!("atteso HumanMessage, trovato {altro:?}"),
        };
        assert!(
            testo.contains("/api/expenses") && testo.contains("500"),
            "il blocco deve dire cosa e' fallito:\n{testo}"
        );
    }

    #[tokio::test]
    async fn senza_endpoint_dichiarati_la_chiusura_non_e_verificata() {
        // Se nessuno dichiara endpoint il gate non ne prova nessuno: e' il caso
        // NORMALE, e va DETTO. Un run che ha interrogato un servizio HTTP da se'
        // (curl in history) e chiude senza una sola prova funzionale chiude
        // "svolto ma non verificato", non "verificato".
        let node = node_with(
            FinalGateConfig::default(),
            Arc::new(StubCriteriaRunner::with_results(vec![ok_result("outputs_exist")])),
        );
        let ctx = ctx_with();
        let mut st = crud_state();
        st.declared_outcome = Some(declared_from_tool_input(json!({
            "outcome": "done",
            "summary": "CRUD spese completo",
        })));
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.final_gate_passed, Some(true), "i criteri stubati passano");
        assert_eq!(
            out.final_gate_unverified,
            Some(true),
            "nessuna prova funzionale eseguita: la chiusura non e' verificata"
        );

        // Contro-prova: con gli endpoint dichiarati le prove ci sono, e la
        // chiusura torna a essere verificata.
        let mut st2 = crud_state();
        st2.declared_outcome = Some(declared_from_tool_input(json!({
            "outcome": "done",
            "summary": "CRUD spese completo",
            "endpoints": [{"method": "GET", "url": URL_SPESE}],
        })));
        let out2 = apply(st2.clone(), node.run(&st2, &ctx).await.expect("run ok"));
        assert_eq!(out2.final_gate_unverified, Some(false));
    }

    #[tokio::test]
    async fn nessuna_attivita_http_nessun_allarme() {
        // Un run che non ha mai toccato HTTP (refactor, libreria) non deve essere
        // marcato non-verificato per mancanza di endpoint: non ne ha.
        let node = node_with(
            FinalGateConfig::default(),
            Arc::new(StubCriteriaRunner::with_results(vec![ok_result("outputs_exist")])),
        );
        let ctx = ctx_with();
        let st = software_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.final_gate_unverified, Some(false));
    }

    // ── criteri strutturali (ADR 0018 leva 3) ─────────────────────────────────────

    #[test]
    fn build_criteria_strutturali() {
        // Col flag ON (default) i 3 criteri strutturali sono accodati in coda
        // coi FATTI estratti dallo stato: action_oriented, azione produttiva in
        // history, tools_count, declared_outcome.
        let node = node_with(
            FinalGateConfig::default(),
            Arc::new(StubCriteriaRunner::with_results(vec![])),
        );
        let mut st = software_state();
        st.action_oriented = Some(true);
        st.tools_json = Some(vec![json!({"name": "write_file"})]);
        st.declared_outcome = Some(json!({"outcome": "done", "summary": "fatto"}));
        let crits = node.build_criteria(&st);
        let types: Vec<&str> = crits.iter().map(|c| c.criterion_type.as_str()).collect();
        assert!(types.contains(&"action_requested"));
        assert!(types.contains(&"tool_capability"));
        assert!(types.contains(&"completion_confirmed"));

        let ar = crits
            .iter()
            .find(|c| c.criterion_type == "action_requested")
            .expect("action_requested");
        assert_eq!(ar.spec["action_oriented"], json!(true));
        assert_eq!(ar.expected, json!({ "acted": true }));

        let tc = crits
            .iter()
            .find(|c| c.criterion_type == "tool_capability")
            .expect("tool_capability");
        assert_eq!(tc.spec["tools_count"], json!(1));
        assert_eq!(tc.spec["has_tool_calls"], json!(true));

        let cc = crits
            .iter()
            .find(|c| c.criterion_type == "completion_confirmed")
            .expect("completion_confirmed");
        assert_eq!(cc.spec["declared_outcome"], json!("done"));

        // Stato senza dichiarazione: il fatto e' null (il check fallira' nel
        // runner con l'invito a chiudere con task_complete).
        let mut st2 = software_state();
        st2.declared_outcome = None;
        let crits2 = node.build_criteria(&st2);
        let cc2 = crits2
            .iter()
            .find(|c| c.criterion_type == "completion_confirmed")
            .expect("completion_confirmed");
        assert_eq!(cc2.spec["declared_outcome"], json!(null));

        // Kill-switch OFF: nessun criterio strutturale.
        let node_off = node_with(
            FinalGateConfig {
                structural_criteria_enabled: false,
                ..Default::default()
            },
            Arc::new(StubCriteriaRunner::with_results(vec![])),
        );
        let types_off: Vec<String> = node_off
            .build_criteria(&software_state())
            .iter()
            .map(|c| c.criterion_type.clone())
            .collect();
        assert!(!types_off.iter().any(|t| t == "action_requested"
            || t == "tool_capability"
            || t == "completion_confirmed"));
    }

    #[test]
    fn build_criteria_tool_capability_porta_history_tool_call() {
        // Regressione: in alcuni resume/fan-in il catalogo nello stato puo' essere
        // vuoto/assente, ma la history contiene gia' tool_use eseguiti. Il criterio
        // deve ricevere quel fatto strutturato invece di bocciare solo su
        // tools_count=0.
        let node = node_with(
            FinalGateConfig::default(),
            Arc::new(StubCriteriaRunner::with_results(vec![])),
        );
        let mut st = software_state();
        st.tools_json = None;
        let crits = node.build_criteria(&st);
        let tc = crits
            .iter()
            .find(|c| c.criterion_type == "tool_capability")
            .expect("tool_capability");
        assert_eq!(tc.spec["tools_count"], json!(0));
        assert_eq!(tc.spec["has_tool_calls"], json!(true));
    }

    /// L'ANELLO CHE CONTA: il criterio del dialogo deve NASCERE nel gate. In
    /// questo repo esiste gia' una lente corretta e mai interrogata da nessun
    /// nodo (`ui_styling`), e il risultato misurato e' che l'app dell'incidente
    /// aveva Tailwind dichiarato, installato e non configurato senza che
    /// nessuno se ne accorgesse: una misura che nessun gate interroga si e'
    /// costruita, non e' entrata in esercizio.
    ///
    /// Mutazione: togliere l'`extend` da build_criteria (o lasciare il flag
    /// spento con l'origine presente) -> il criterio sparisce e questo test
    /// rosseggia. Senza il flag, invece, non deve nascere affatto: un criterio
    /// nuovo a DB muto non boccia.
    #[test]
    fn il_criterio_del_dialogo_nasce_nel_gate_solo_se_acceso() {
        use crate::decisions::browser_dialogue::CRITERION_TYPE;
        let con_frontend = |acceso: bool| FinalGateConfig {
            browser_dialogue_enabled: acceso,
            origine_frontend: Some("http://localhost:35954".to_string()),
            browser_third_parties: vec!["https://fonts.googleapis.com".to_string()],
            browser_settle_ms: 2500,
            ..FinalGateConfig::default()
        };
        let cerca = |cfg: FinalGateConfig| {
            let node = node_with(cfg, Arc::new(StubCriteriaRunner::with_results(vec![])));
            node.build_criteria(&software_state())
                .into_iter()
                .find(|c| c.criterion_type == CRITERION_TYPE)
        };

        let c = cerca(con_frontend(true)).expect("acceso con un frontend: il criterio nasce");
        assert_eq!(c.spec["url"], "http://localhost:35954");
        assert_eq!(c.spec["third_parties"][0], "https://fonts.googleapis.com");
        assert_eq!(c.spec["settle_ms"], 2500);

        assert!(
            cerca(con_frontend(false)).is_none(),
            "a flag spento un criterio nuovo non boccia"
        );
        // Senza frontend non c'e' dialogo da misurare: niente criterio, e
        // nessun Failed per un progetto che non ha interfaccia.
        assert!(
            cerca(FinalGateConfig {
                browser_dialogue_enabled: true,
                origine_frontend: None,
                ..FinalGateConfig::default()
            })
            .is_none(),
            "senza origine frontend il criterio non deve nascere"
        );
    }

    /// L'ANELLO CHE CONTA per il criterio della resa: deve arrivare fino ai
    /// criteri del gate, e portarsi dietro la DICHIARAZIONE dell'agente.
    ///
    /// Le due parti nascono in posti diversi — la configurazione della misura la
    /// risolve il motore, che legge il DB; il contenitore lo sa solo qui, dove
    /// si vede lo stato del run — ed e' proprio la giunzione in cui una delle
    /// due puo' perdersi senza che nulla fallisca: il criterio resterebbe
    /// valido, con un segnale in meno e nessuno ad accorgersene.
    ///
    /// MUTAZIONE: togliere `con_contenitore` dall'`extend` -> il selettore
    /// sparisce dalla spec e il browser non cerca piu' nulla, cioe' il caso
    /// «id sbagliato, nessuna eccezione, pagina piena» torna verde.
    #[test]
    fn il_criterio_della_resa_arriva_al_gate_con_la_dichiarazione() {
        use crate::decisions::risorse_pagina::PoliticaRisorse;
        use crate::decisions::static_render::{self, ModalitaResa, ParametriMisura, CRITERION_TYPE};
        let criterio = static_render::criterio_resa(
            Some("http://127.0.0.1:4000"),
            None,
            &ParametriMisura {
                minimo_elementi: 5,
                timeout_s: 15.0,
                attesa_ms: 2500,
                politica: PoliticaRisorse::nuova(vec!["image".into()], Some(1.0)),
                modalita: ModalitaResa::Applica,
            },
        );
        let cerca = |dichiarato: Option<Value>| {
            let node = node_with(
                FinalGateConfig {
                    static_render_criterion: criterio.clone(),
                    ..FinalGateConfig::default()
                },
                Arc::new(StubCriteriaRunner::with_results(vec![])),
            );
            let mut st = software_state();
            st.declared_outcome = dichiarato;
            node.build_criteria(&st)
                .into_iter()
                .find(|c| c.criterion_type == CRITERION_TYPE)
        };

        let c = cerca(Some(
            json!({ "outcome": "done", "rendered_container": "#courses-grid" }),
        ))
        .expect("il criterio risolto dal motore deve arrivare ai criteri del gate");
        assert_eq!(c.spec[static_render::CHIAVE_CONTENITORE], "#courses-grid");
        assert_eq!(c.spec[static_render::CHIAVE_MIN_ELEMENTI], 5);
        // La PAGINA non e' nella spec: la risolve chi verifica. Se tornasse qui
        // tornerebbe anche il difetto — un indirizzo composto a t=0 e'
        // l'indirizzo della pagina di ieri.
        assert!(
            c.spec.get("url").is_none(),
            "l'indirizzo si compone alla verifica, non a t=0"
        );
        assert_eq!(c.spec[static_render::CHIAVE_MODALITA], "enforce");

        // Senza dichiarazione il criterio nasce lo stesso, coi due segnali che
        // non richiedono di dichiarare nulla.
        let senza = cerca(None).expect("il criterio non dipende dalla dichiarazione");
        assert!(senza.spec.get(static_render::CHIAVE_CONTENITORE).is_none());

        // UNA SOLA pagina misurata, quindi UN SOLO criterio di questo tipo. I
        // consumatori lo cercano con `.find(...)`: con due criteri il secondo
        // sarebbe invisibile a loro e a questo test, che resterebbe verde
        // misurando solo il primo.
        let node = node_with(
            FinalGateConfig {
                static_render_criterion: criterio.clone(),
                ..FinalGateConfig::default()
            },
            Arc::new(StubCriteriaRunner::with_results(vec![])),
        );
        assert_eq!(
            node.build_criteria(&software_state())
                .iter()
                .filter(|c| c.criterion_type == CRITERION_TYPE)
                .count(),
            1,
            "il gate misura una pagina sola"
        );

        // Criterio spento (modalita' `off`) o nessuna radice di anteprima: il
        // motore non lo costruisce e il gate non lo inventa. Un inconcludente
        // qui declasserebbe a `completed_unverified` ogni run a cui il criterio
        // non si applica.
        let node = node_with(
            FinalGateConfig::default(),
            Arc::new(StubCriteriaRunner::with_results(vec![])),
        );
        assert!(
            !node
                .build_criteria(&software_state())
                .iter()
                .any(|c| c.criterion_type == CRITERION_TYPE),
            "senza criterio risolto dal motore, il gate non ne crea uno"
        );
    }

    // ── count_build_errors ────────────────────────────────────────────────────────

    #[test]
    fn count_build_errors_pattern() {
        assert_eq!(count_build_errors(""), 0);
        assert_eq!(count_build_errors("tutto ok, nessun errore qui"), 0);
        // tsc.
        assert_eq!(
            count_build_errors("src/a.ts(1,1): error TS2304: not found\nerror TS1005: ;"),
            2
        );
        // rustc.
        assert_eq!(count_build_errors("error[E0432]: unresolved import"), 1);
        // SyntaxError/TypeError.
        assert_eq!(count_build_errors("SyntaxError: bad\nTypeError: x"), 2);
        // generico cargo (a inizio riga).
        assert_eq!(count_build_errors("error: cannot find crate"), 1);
        // vite/rollup: build che ESCE 0 ma FALLISCE (rete di sicurezza mig 0465).
        assert!(
            count_build_errors(
                "error during build:\nCould not resolve \"./components/ui/sonner\" from \"src/app/App.tsx\""
            ) >= 2,
            "pattern vite (could not resolve + error during build) devono contare"
        );
        assert!(count_build_errors("x Build failed in 312ms") >= 1);
        // REGRESSIONE run 48793fde (Beaty-Book): un build vite uscito 0 e RIUSCITO
        // emette warning del reporter col prefisso `[plugin:vite:reporter]` e la
        // nota "chunks are larger than 500 kB". Questi NON sono errori: contarli
        // bocciava un build oggettivamente verde (falso negativo del final_gate).
        let vite_ok_con_warning = "vite v5.4.21 building for production...\n\
             2334 modules transformed.\n\
             [plugin:vite:reporter] [plugin vite:reporter]\n\
             (!) src/app/services/bookingService.ts is dynamically imported by \
             src/app/components/admin/AppointmentsTab.tsx but also statically imported by \
             src/app/components/admin/AdminLogin.tsx, dynamic import will not move module \
             into another chunk.\n\
             (!) Some chunks are larger than 500 kB after minification.\n\
             built in 7.67s";
        assert_eq!(
            count_build_errors(vite_ok_con_warning),
            0,
            "un build vite riuscito con soli warning (reporter/chunk-size) non deve contare errori"
        );
    }

    /// UN PIANO DI 25 PROVE NEGATE NON E' UN PIANO VUOTO (19/08/2026).
    ///
    /// Il referto portava il solo CONTATORE (`{"phase":"passed",
    /// "inconclusive":3}`): venticinque prove dichiarate dalle figure e nessuna
    /// eseguita valevano un intero che passa da 2 a 3, e chi legge non aveva
    /// modo di distinguere «non c'erano prove» da «c'erano e nessuno le ha
    /// eseguite» — che sono due fatti con due rimedi (regola Q).
    ///
    /// L'evidenza la produce il PRODUTTORE reale (`classifica_piano` +
    /// `evidenza_piano`): fabbricarla a mano proverebbe che il trasporto sa
    /// leggere il JSON che il test sa scrivere (regola O).
    ///
    /// MUTAZIONE: togliere `inconclusive_criteria` dal payload del ramo
    /// `passed` -> l'ultima asserzione cade, e il referto torna a essere il solo
    /// intero del difetto.
    #[test]
    fn il_referto_distingue_25_prove_negate_da_un_piano_vuoto() {
        use crate::decisions::piano_di_verifica::{
            classifica_piano, evidenza_piano, CausaNonEseguita, EsitoProva, EsitoSingolo,
            PianoDiVerifica,
        };
        use crate::decisions::step_gate::StepGateDecision;

        // Le prove nascono dalla FORMA in cui le figure le emettono, attraverso
        // il lettore di produzione: una `Prova` costruita a mano proverebbe che
        // il referto sa rendere cio' che il test sa costruire (regola O).
        let piano = PianoDiVerifica::from_value(Some(&json!((0..25)
            .map(|i| json!({
                "descrizione": format!("la POST senza titolo risponde 400 ({i})"),
                "comando": format!("curl -s -o /dev/null -w '%{{http_code}}' -X POST /todo/{i}"),
                "attesa": { "tipo": "output_contains", "testo": "400" },
                "origine": "council",
            }))
            .collect::<Vec<_>>())));
        assert_eq!(piano.len(), 25, "le venticinque prove del caso misurato");
        // Il caso misurato: la decisione e' `NeedsHuman` in un run che non ha
        // nessuno a cui chiedere, e la causa la deriva il punto unico.
        let causa = CausaNonEseguita::dal_gate(
            StepGateDecision::NeedsHuman,
            &[
                crate::decisions::step_gate::StepVerdict::Approve,
                crate::decisions::step_gate::StepVerdict::Abstained,
            ],
            false,
        )
        .expect("un NeedsHuman non e' un via libera");
        let esiti: Vec<EsitoProva> = piano
            .prove
            .into_iter()
            .map(|prova| EsitoProva {
                prova,
                esito: EsitoSingolo::non_eseguibile(causa.clone()),
            })
            .collect();
        let negato = CriterionResult {
            criterion_type: "piano_di_verifica".to_string(),
            outcome: CriterionOutcome::Inconclusive,
            evidence: evidenza_piano(&classifica_piano(&esiti), &esiti),
        };
        // L'altro criterio inconcludente del run, che non c'entra col piano.
        let altro = CriterionResult {
            criterion_type: "ui_styling".to_string(),
            outcome: CriterionOutcome::Inconclusive,
            evidence: json!({ "verdict": "vocabolario assente" }),
        };

        let meta = FinalGateNode::inconclusive_criteria_meta(&[negato, altro]);
        let arr = meta.as_array().expect("array");
        assert_eq!(arr.len(), 2, "entrambi i non misurati arrivano al referto");
        assert_eq!(arr[0]["type"], json!("piano_di_verifica"));
        assert_eq!(
            arr[0]["skipped_cause"],
            json!("no_human_to_decide"),
            "il rimedio e' sul quorum del gate, non sulla prova"
        );
        assert_eq!(
            arr[0]["prove"]["dichiarate"],
            json!(25),
            "e' IL numero che il contatore da solo non poteva portare"
        );
        assert_eq!(arr[0]["prove"]["non_eseguibili"], json!(25));
        assert_eq!(arr[0]["cause"]["no_human_to_decide"], json!(25));
        assert!(
            arr[0]["reason"].as_str().is_some_and(|r| !r.is_empty()),
            "la prosa sta accanto ai campi, e nessuno la rilegge"
        );

        // E il PIANO VUOTO non e' nemmeno inconcludente: `dichiara_un_esito`
        // resta vero, quindi non compare qui. La differenza fra i due casi non
        // e' piu' un intero.
        let vuoto = CriterionResult {
            criterion_type: "piano_di_verifica".to_string(),
            outcome: CriterionOutcome::Passed,
            evidence: evidenza_piano(&classifica_piano(&[]), &[]),
        };
        assert_eq!(
            FinalGateNode::inconclusive_criteria_meta(&[vuoto]),
            json!([])
        );
    }

    #[test]
    fn failed_criteria_meta_espone_exit_code_e_build_errors() {
        // Regola M: per un criterio comando il payload della timeline deve portare
        // i segnali STRUTTURATI (exit_code + build_errors), non solo l'excerpt
        // umano, cosi' un falso negativo (exit 0 ma bocciato) e' diagnosticabile
        // dal solo meta_step. `exit_code` va emesso AS-IS anche quando null.
        let build = fail_result(
            "run_command",
            json!({
                "command": "pnpm build",
                "exit_code": 0,
                "build_errors": 1,
                "output_excerpt": "EXIT CODE: 0\nSTDOUT:\nbuild ok",
                "output_total_chars": 30,
            }),
        );
        let cc = fail_result(
            "completion_confirmed",
            json!({ "declared_outcome": null, "verdict": "chiudi con task_complete" }),
        );
        let meta = FinalGateNode::failed_criteria_meta(&[build, cc]);
        let arr = meta.as_array().expect("array");
        assert_eq!(arr[0]["type"], json!("run_command"));
        assert_eq!(arr[0]["exit_code"], json!(0), "exit_code AS-IS nel payload");
        assert_eq!(
            arr[0]["build_errors"],
            json!(1),
            "build_errors visibile: exit 0 + 1 = falso positivo"
        );
        // I criteri NON-comando non portano exit_code/build_errors (niente rumore).
        assert_eq!(arr[1]["type"], json!("completion_confirmed"));
        assert!(arr[1].get("exit_code").is_none());
        assert!(arr[1].get("build_errors").is_none());

        // exit_code AS-IS anche quando l'estrazione FALLISCE (null): il criterio
        // e' comunque un run_command, il null deve comparire per rendere visibile
        // che l'estrazione non e' riuscita.
        let no_exit = fail_result(
            "run_command",
            json!({
                "command": "npx eslint .",
                "exit_code": null,
                "build_errors": 0,
                "output_total_chars": 10,
                "output_excerpt": "[Auto-probe] long-running",
            }),
        );
        let meta2 = FinalGateNode::failed_criteria_meta(&[no_exit]);
        assert_eq!(meta2.as_array().unwrap()[0]["exit_code"], json!(null));
    }

    // ── render_failed_block ────────────────────────────────────────────────────────

    /// Le direttive parlano di cio' che e' fallito DAVVERO.
    ///
    /// MISURATO il 12/08/2026: a un fallimento della RESA venivano consegnate
    /// direttive di forma build — «rilancia il comando di build con
    /// run_command», «finche' il build non passa al 100% (exit 0, zero errori)»
    /// — su un progetto statico dove nessun build esiste e nessun exit code puo'
    /// andare a zero. Il modello riceveva istruzioni su un'attivita' che non era
    /// la sua, e nella traccia del run seguono cinque letture consecutive senza
    /// una scrittura.
    ///
    /// MUTAZIONE: rimettere le righe di build fuori dal ramo `c_e_build` ->
    /// questo test cade sulla riga «exit 0», col difetto reale.
    #[test]
    fn le_direttive_non_parlano_di_build_a_un_fallimento_della_resa() {
        let st = software_state();
        let results = vec![fail_result(
            crate::decisions::static_render::CRITERION_TYPE,
            json!({
                "error": "la pagina non mostra il proprio contenuto: il JavaScript della \
                          pagina ha lanciato: Invalid or unexpected token (in listino.html:75:20)"
            }),
        )];
        let block = FinalGateNode::render_failed_block(&st, 1, 2, &results);
        assert!(block.contains("DIRETTIVE (fail-closed):"));
        assert!(
            !block.contains("exit 0"),
            "un criterio senza build non deve ricevere il contratto del build:\n{block}"
        );
        assert!(
            !block.contains("rilancia il comando di"),
            "niente istruzioni di build dove non c'e' un build:\n{block}"
        );
        // E porta la direttiva che serve: andare dove il rilievo indica.
        assert!(block.contains("file:riga:colonna"));
        assert!(block.contains("DOM DOPO l'esecuzione del JavaScript"));
        // La posizione del rilievo resta leggibile nel corpo.
        assert!(block.contains("listino.html:75:20"));
    }

    /// Il ramo build NON cambia: quando il criterio fallito e' un comando, le
    /// direttive di build tornano tali e quali.
    #[test]
    fn le_direttive_di_build_restano_dove_un_build_c_e() {
        let st = software_state();
        let results = vec![fail_result(
            "run_command",
            json!({"output_excerpt": "error: boom", "exit_code": 1, "output_total_chars": 11}),
        )];
        let block = FinalGateNode::render_failed_block(&st, 1, 2, &results);
        assert!(block.contains("al 100% (exit 0, zero errori)"));
        assert!(block.contains("rilancia il comando di"));
        assert!(
            !block.contains("file:riga:colonna"),
            "le direttive della resa non entrano dove la resa non e' in causa"
        );
    }

    #[test]
    fn render_failed_block_build_con_troncamento() {
        let st = software_state();
        let results = vec![fail_result(
            "run_command",
            json!({
                "output_excerpt": "error TS2304: a\nerror TS2305: b",
                "exit_code": 1,
                "output_total_chars": 9999,
                "output_truncated": true,
            }),
        )];
        let block = FinalGateNode::render_failed_block(&st, 1, 2, &results);
        assert!(block.contains("<final_gate_failed cycle=\"1/2\">"));
        // Header build: tipo + count errori + nota troncamento.
        assert!(block.contains("[run_command]"));
        assert!(block.contains("errori rilevati: 2"));
        assert!(block.contains("output troncato"));
        assert!(block.contains("/9999 char)"));
        // La riga col conteggio errori e' inserita nelle direttive.
        assert!(block.contains("Numero di errori rilevati nel build: 2"));
        // Niente autonomy_hint (behavior_mode non autonomo).
        assert!(!block.contains("<autonomy_hint"));
    }

    #[test]
    fn render_failed_block_criterio_non_build_e_autonomy_hint() {
        let mut st = software_state();
        st.automation_mode = Some(crate::state::AutomationMode::Automatic);
        let results = vec![fail_result(
            "service_logs_clean",
            json!({"verdict": "errore 500 nei log del servizio"}),
        )];
        let block = FinalGateNode::render_failed_block(&st, 1, 2, &results);
        // autonomy_hint prefisso per modalita' autonoma.
        assert!(block.contains("<autonomy_hint mode=\"automatic\">"));
        assert!(block.contains("[service_logs_clean]"));
        assert!(block.contains("errore 500 nei log"));
        // Nessun header build (niente "errori rilevati").
        assert!(!block.contains("errori rilevati:"));
    }

    #[test]
    fn render_failed_block_excerpt_vuoto_cade_su_verdict() {
        // FIX A (parita' falsy): output_excerpt = "" deve cadere su verdict
        // (Python `or`). Senza il fix, "" interromperebbe la catena e il criterio
        // verrebbe saltato (excerpt vuoto -> continue), perdendo il verdict.
        let st = software_state();
        let results = vec![fail_result(
            "service_logs_clean",
            json!({"output_excerpt": "", "verdict": "errore 500 dal verdict"}),
        )];
        let block = FinalGateNode::render_failed_block(&st, 1, 2, &results);
        // Il verdict e' renderizzato (non saltato).
        assert!(block.contains("[service_logs_clean]"));
        assert!(block.contains("errore 500 dal verdict"));
        // Il corpo NON e' il placeholder di "nessun criterio renderizzato".
        assert!(!block.contains("Una verifica del gate e' fallita."));
    }

    #[test]
    fn render_failed_block_total_chars_zero_cade_su_len_text() {
        // FIX B (parita' falsy): output_total_chars = 0 deve cadere su len(text)
        // (Python `or`). text = "error TS2304: x" -> 15 codepoint. Con lo 0 tenuto
        // (vecchio Rust) l'header sarebbe "(15/0 char)", col fallback "(15/15 char)".
        let st = software_state();
        let text = "error TS2304: x";
        let results = vec![fail_result(
            "run_command",
            json!({
                "output_excerpt": text,
                "exit_code": 1,
                "output_total_chars": 0,
                "output_truncated": true,
            }),
        )];
        let block = FinalGateNode::render_failed_block(&st, 1, 2, &results);
        let expected_len = text.chars().count();
        assert!(block.contains("output troncato"));
        // total_chars = len(text), NON 0.
        assert!(
            block.contains(&format!("({expected_len}/{expected_len} char)")),
            "atteso fallback a len(text)={expected_len}, blocco:\n{block}"
        );
        assert!(!block.contains("/0 char)"), "lo 0 non deve essere tenuto");
    }

    #[test]
    fn render_failed_block_nessun_excerpt_default() {
        let st = software_state();
        // Criterio fallito senza output_excerpt/verdict/error.
        let results = vec![fail_result("outputs_exist", json!({}))];
        let block = FinalGateNode::render_failed_block(&st, 2, 2, &results);
        assert!(block.contains("Una verifica del gate e' fallita."));
    }

    #[test]
    fn esito_criteri_reduce() {
        assert_eq!(
            FinalGateNode::esito_criteri(&[ok_result("a"), ok_result("b")]),
            EsitoCriteri::Superato
        );
        assert_eq!(
            FinalGateNode::esito_criteri(&[ok_result("a"), fail_result("b", json!({}))]),
            EsitoCriteri::Fallito
        );
        // Lista VUOTA: non ha bocciato nessuno, ma non ha nemmeno misurato
        // niente. Prima usciva `true` come un pass pieno (parita' col reduce
        // Python), e da li' un todo si chiudeva Completed senza prove.
        assert_eq!(
            FinalGateNode::esito_criteri(&[]),
            EsitoCriteri::SuperatoNonVerificato { inconclusive: 0 }
        );
    }

    // ── Criteri NON MISURABILI ────────────────────────────────────────────────

    #[test]
    fn il_non_misurabile_e_fuori_dal_conteggio_in_entrambe_le_direzioni() {
        // Non boccia: un criterio che nessuno ha potuto eseguire non prova un
        // difetto. Prima `all(r.passed)` lo trattava come un pass PIENO, il che
        // e' la stessa cosa solo finche' nessuno chiede "quanto e' stato
        // verificato": la seconda meta' della risposta e' `any_inconclusive`.
        // LA PROVA del difetto chiuso: tutti inconcludenti non e' un pass
        // pieno. Le due meta' della risposta ora sono UNA, e chi la riceve deve
        // nominare la variante per trattarla come un successo.
        //
        // MUTAZIONE: far tornare `Superato` quando `misurabili.is_empty()` fa
        // rosseggiare questa asserzione — ed e' il valore del difetto reale, per
        // cui un todo si chiudeva `Completed` con zero criteri misurati.
        let solo_inc = [inconclusive_result("no_orphan_imported")];
        assert_eq!(
            FinalGateNode::esito_criteri(&solo_inc),
            EsitoCriteri::SuperatoNonVerificato { inconclusive: 1 },
            "un criterio non misurabile non boccia, ma non verifica nemmeno"
        );

        // E non copre un criterio davvero fallito.
        let misto = [
            inconclusive_result("no_orphan_imported"),
            fail_result("run_command", json!({})),
        ];
        assert_eq!(FinalGateNode::esito_criteri(&misto), EsitoCriteri::Fallito);

        // Un misurato che passa INSIEME a un inconcludente: passa, ma con
        // riserva dichiarata.
        assert_eq!(
            FinalGateNode::esito_criteri(&[ok_result("a"), inconclusive_result("b")]),
            EsitoCriteri::SuperatoNonVerificato { inconclusive: 1 }
        );

        // Nessun inconcludente -> nessuna riserva.
        assert_eq!(
            FinalGateNode::esito_criteri(&[ok_result("a"), fail_result("b", json!({}))]),
            EsitoCriteri::Fallito
        );
    }

    #[test]
    fn il_non_misurabile_non_e_un_criterio_fallito_da_rendere() {
        // Il blocco di correzione e la timeline elencano cio' che ha FALLITO:
        // un criterio non misurato non ha niente da far correggere all'agente.
        let results = [
            inconclusive_result("no_orphan_imported"),
            fail_result("run_command", json!({"output_excerpt": "boom"})),
        ];
        let meta = FinalGateNode::failed_criteria_meta(&results);
        let items = meta.as_array().expect("array");
        assert_eq!(items.len(), 1, "solo il criterio MISURATO e fallito");
        assert_eq!(items[0]["type"], json!("run_command"));
        // E non impedisce il turno di grazia: l'unico criterio FALLITO resta
        // `completion_confirmed` anche in presenza di un inconcludente.
        assert!(FinalGateNode::only_completion_confirmed_failed(&[
            ok_result("run_command"),
            inconclusive_result("outputs_exist"),
            fail_result("completion_confirmed", json!({})),
        ]));
    }

    #[tokio::test]
    async fn criterio_non_misurabile_chiude_ma_non_verificato() {
        // CONSEGUENZA (regola O): non basta che il conteggio escluda
        // l'inconcludente; il run deve chiudere con `final_gate_unverified`,
        // che mcp-core mappa su `CompletedUnverified` invece di `Completed`.
        // Nessun run in piu' chiude VERIFICATO per questa via: i criteri che
        // passano da `passed=true` a "non misurabile" perdono il titolo.
        let node = node_with(
            FinalGateConfig::default(),
            Arc::new(StubCriteriaRunner::with_results(vec![
                ok_result("outputs_exist"),
                inconclusive_result("no_orphan_imported"),
            ])),
        );
        let ctx = ctx_with();
        let st = software_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(
            out.stop_reason,
            Some(StopReason::EndTurn),
            "un criterio non misurabile NON boccia: il gate chiude"
        );
        assert_eq!(out.final_gate_passed, Some(true));
        assert_eq!(
            out.final_gate_unverified,
            Some(true),
            "nessuno ha misurato quel criterio: la chiusura non e' verificata"
        );
    }

    #[tokio::test]
    async fn tutti_i_criteri_misurati_chiude_verificato() {
        // Contro-prova della precedente: senza inconcludenti la chiusura resta
        // verificata (il nuovo segnale non marca tutti i run).
        let node = node_with(
            FinalGateConfig::default(),
            Arc::new(StubCriteriaRunner::with_results(vec![ok_result(
                "outputs_exist",
            )])),
        );
        let ctx = ctx_with();
        let st = software_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.final_gate_unverified, Some(false));
    }
}

#[cfg(test)]
mod golden {
    //! Golden-test di PARITA' 1:1 vs Python sulla logica DETERMINISTICA del nodo
    //! final_gate. Lo script `scripts/gen_golden_final_gate.py` importa le
    //! funzioni reali da `brain/agents/final_gate.py` (`_is_software_task`,
    //! `_count_build_errors`, `_render_failed_block`, `route_after_final_gate`) +
    //! replica la decision machine di `final_gate_node` (deterministica, dati i
    //! risultati criteri) e salva `{case_id, function, input, output}` in
    //! `/tmp/golden_final_gate.json`. Qui ricostruiamo l'input, chiamiamo la
    //! funzione Rust corrispondente e verifichiamo `output == golden Python`.
    //!
    //! `#[ignore]` perche' dipende dal file generato. Comando:
    //!   python3 crates/nexus-agent-graph/scripts/gen_golden_final_gate.py
    //!   cargo test -p nexus-agent-graph --lib golden_final_gate_parita -- --ignored

    use serde::Deserialize;
    use serde_json::{json, Value};

    use super::{count_build_errors, EsitoCriteri, FinalGateNode};
    use crate::routing::config::RoutingConfig;
    use crate::routing::{route_after_final_gate, signals, NodeTarget};
    use crate::runtime::ports::{CriterionOutcome, CriterionResult};
    use crate::state::{AgentState, ContentBlock, Message, MessageContent};

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        case_id: String,
        function: String,
        input: Value,
        output: Value,
    }

    /// Ricostruisce un `AgentState` minimale dai campi dell'input golden usati
    /// dalle funzioni testate (messages con tool_use, user_intent, behavior_mode,
    /// thread_id, forced_close_unverified, final_gate_cycle, stop_reason). Le
    /// assegnazioni sono CONDIZIONALI (un campo si popola solo se la chiave e'
    /// presente nell'input golden): `st` parte dal costruttore con `messages` +
    /// `..Default::default()`, poi i campi opzionali sono settati se presenti.
    fn state_from(input: &Value) -> AgentState {
        // messages: lista di {role, tool_uses:[name,...]} -> Message::Ai con blocchi.
        // Costruite PRIMA dell'inizializzazione di `st` (evita
        // field_reassign_with_default: il costruttore usa direttamente i campi).
        let mut msgs: Vec<Message> = Vec::new();
        if let Some(arr) = input.get("messages").and_then(Value::as_array) {
            for m in arr {
                let role = m.get("role").and_then(Value::as_str).unwrap_or("");
                if role == "assistant" || role == "ai" {
                    let mut blocks: Vec<ContentBlock> = Vec::new();
                    if let Some(tools) = m.get("tool_uses").and_then(Value::as_array) {
                        for (i, t) in tools.iter().enumerate() {
                            if let Some(name) = t.as_str() {
                                blocks.push(ContentBlock::ToolUse {
                                    id: format!("c{i}"),
                                    name: name.to_string(),
                                    input: json!({}),
                                    thought_signature: None,
                                });
                            }
                        }
                    }
                    msgs.push(Message::Ai {
                        content: MessageContent::Blocks(blocks),
                        tool_calls: vec![],
                        reasoning: None,
                        thinking_signature: None,
                    });
                } else if role == "user" || role == "human" {
                    let c = m.get("content").and_then(Value::as_str).unwrap_or("");
                    msgs.push(Message::Human {
                        content: MessageContent::text(c),
                    });
                }
            }
        }
        let mut st = AgentState {
            messages: msgs,
            ..Default::default()
        };
        if let Some(ui) = input.get("user_intent").and_then(Value::as_str) {
            st.user_intent = Some(ui.to_string());
        }
        if let Some(intent) = input.get("intent").and_then(Value::as_str) {
            st.extra.insert("intent".to_string(), json!(intent));
        }
        if let Some(bm) = input.get("behavior_mode").and_then(Value::as_str) {
            st.behavior_mode = Some(bm.to_string());
        }
        if let Some(tid) = input.get("thread_id").and_then(Value::as_str) {
            st.thread_id = Some(tid.to_string());
        }
        if let Some(f) = input
            .get("forced_close_unverified")
            .and_then(Value::as_bool)
        {
            st.forced_close_unverified = Some(f);
        }
        if let Some(c) = input.get("final_gate_cycle").and_then(Value::as_i64) {
            st.final_gate_cycle = Some(c);
        }
        if let Some(sr) = input.get("stop_reason").and_then(Value::as_str) {
            st.stop_reason = serde_json::from_value(json!(sr)).ok();
        }
        st
    }

    /// Risultati criteri dall'input golden (lista {type, passed, evidence}).
    fn results_from(input: &Value) -> Vec<CriterionResult> {
        input
            .get("results")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .map(|r| CriterionResult {
                        criterion_type: r
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        outcome: CriterionOutcome::measured(
                            r.get("passed").and_then(Value::as_bool).unwrap_or(false),
                        ),
                        evidence: r.get("evidence").cloned().unwrap_or(json!({})),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Replica in Rust la DECISION MACHINE di `final_gate_node` data la
    /// pre-condizione che il gate sia entrato (enabled + software gia' decisi a
    /// monte dal golden) e i risultati criteri. Ritorna il delta nella forma
    /// confrontabile col Python (`{}` o dict). NON esegue i criteri (input).
    fn decision_delta(st: &AgentState, results: &[CriterionResult], max_cycles: i64) -> Value {
        let cycle = st.final_gate_cycle.unwrap_or(0) + 1;
        let passed = !matches!(
            FinalGateNode::esito_criteri(results),
            EsitoCriteri::Fallito
        );
        if passed {
            return json!({
                "final_gate_cycle": 0,
                "stop_reason": "end_turn",
                "final_gate_passed": true,
            });
        }
        let forced = st.forced_close_unverified.unwrap_or(false);
        if forced || cycle >= max_cycles {
            return json!({"final_gate_cycle": 0, "stop_reason": "end_turn", "final_gate_passed": false});
        }
        let block = FinalGateNode::render_failed_block(st, cycle, max_cycles, results);
        json!({
            "messages": [block],
            "final_gate_cycle": cycle,
            "stop_reason": "tool_use",
            "pending_tool_uses": [],
        })
    }

    #[test]
    #[ignore = "richiede /tmp/golden_final_gate.json generato da gen_golden_final_gate.py"]
    fn golden_final_gate_parita() {
        let Some(raw) =
            crate::golden_util::load_golden("golden_final_gate.json", "gen_golden_final_gate.py")
        else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(!cases.is_empty(), "golden vuoto");

        // is_software_task usa la RoutingConfig di default (whitelist + mutator
        // identici ai _SAFE_DEFAULTS Python).
        let routing_cfg = RoutingConfig::default();

        let mut checked = 0usize;
        for c in &cases {
            let got: Value = match c.function.as_str() {
                "is_software_task" => {
                    let st = state_from(&c.input);
                    json!(signals::is_software_task(&st, &routing_cfg))
                }
                "count_build_errors" => {
                    let out = c.input.get("output").and_then(Value::as_str).unwrap_or("");
                    json!(count_build_errors(out))
                }
                "render_failed_block" => {
                    let st = state_from(&c.input);
                    let cycle = c.input.get("cycle").and_then(Value::as_i64).unwrap_or(1);
                    let max_cycles = c
                        .input
                        .get("max_cycles")
                        .and_then(Value::as_i64)
                        .unwrap_or(2);
                    let results = results_from(&c.input);
                    json!(FinalGateNode::render_failed_block(
                        &st, cycle, max_cycles, &results
                    ))
                }
                "route_after_final_gate" => {
                    let st = state_from(&c.input);
                    let target = match route_after_final_gate(&st) {
                        NodeTarget::Executor => "executor",
                        NodeTarget::Learner => "learner",
                        other => panic!("target inatteso da route_after_final_gate: {other:?}"),
                    };
                    json!(target)
                }
                "decision_machine" => {
                    let st = state_from(&c.input);
                    let results = results_from(&c.input);
                    let max_cycles = c
                        .input
                        .get("max_cycles")
                        .and_then(Value::as_i64)
                        .unwrap_or(2);
                    decision_delta(&st, &results, max_cycles)
                }
                other => panic!("funzione golden sconosciuta: {other} (caso {})", c.case_id),
            };

            assert!(
                got == c.output,
                "PARITA' FALLITA caso {} ({}):\n  rust   = {}\n  python = {}",
                c.case_id,
                c.function,
                got,
                c.output
            );
            checked += 1;
        }
        println!("golden final_gate: {checked} casi verificati, tutti verdi");
    }

    // ── Gate DELTA-aware: localizzazione errori + matching file ─────────────────

    #[test]
    fn build_error_files_estrae_path_tsc_e_rustc() {
        use super::build_error_files;
        let tsc = "src/app/pages/BookingPage.tsx(156,7): error TS2554: Expected 2 arguments\n\
                   src/app/pages/LoginPage.tsx(5,10): error TS2305: no member\n\
                   Found 2 errors.";
        let files = build_error_files(tsc);
        assert!(files.contains("src/app/pages/BookingPage.tsx"));
        assert!(files.contains("src/app/pages/LoginPage.tsx"));
        assert_eq!(files.len(), 2);

        let rustc = "error[E0432]: unresolved import\n  --> crates/foo/src/lib.rs:12:5\n";
        assert!(build_error_files(rustc).contains("crates/foo/src/lib.rs"));

        // Una riga d'errore stylish ISOLATA (senza la riga-path sopra) -> set
        // VUOTO: il chiamante ricade sul criterio assoluto (fail-closed).
        assert!(build_error_files("  1:1  error  Unexpected token  no-undef").is_empty());
        assert!(build_error_files("").is_empty());
    }

    #[test]
    fn build_error_files_estrae_path_eslint_e_vite() {
        use super::build_error_files;

        // eslint stylish (default): path su riga a se' + righe indentate. Solo il
        // file con un `error` viene catturato; il file con soli `warning` NO.
        let stylish = "\
/repo/src/app/LoginForm.tsx\n  \
  12:5   error    'foo' is not defined       no-undef\n  \
  20:1   warning  Unexpected console         no-console\n\
\n\
/repo/src/app/OnlyWarn.tsx\n  \
  3:1    warning  Missing semicolon          semi\n\
\n\
\u{2716} 2 problems (1 error, 1 warning)";
        let files = build_error_files(stylish);
        assert!(
            files.contains("/repo/src/app/LoginForm.tsx"),
            "il file con error deve essere catturato: {files:?}"
        );
        assert!(
            !files.iter().any(|f| f.contains("OnlyWarn")),
            "un file con soli warning non e' una regressione: {files:?}"
        );

        // eslint compact (`-f compact`).
        let compact =
            "src/utils/date.ts: line 4, col 2, Error - 'x' is assigned but never used (no-unused-vars)";
        assert!(build_error_files(compact).contains("src/utils/date.ts"));

        // vite/rollup: import irrisolto -> path del file che importa (`from`).
        let rollup = "\
[vite]: Rollup failed to resolve import \"./missing\" from \"src/pages/Home.tsx\".\n\
This is most likely unintended.";
        assert!(build_error_files(rollup).contains("src/pages/Home.tsx"));

        // esbuild/vite generico: `path:riga:col: ERROR: msg`.
        let esbuild = "  src/components/Button.tsx:15:22: ERROR: Unexpected \"}\"";
        assert!(build_error_files(esbuild).contains("src/components/Button.tsx"));
    }

    #[test]
    fn error_file_matches_touched_suffisso_a_segmento() {
        use super::error_file_matches_touched;
        assert!(error_file_matches_touched(
            "src/app/pages/LoginPage.tsx",
            "src/app/pages/LoginPage.tsx"
        ));
        assert!(error_file_matches_touched(
            "src/app/pages/LoginPage.tsx",
            "LoginPage.tsx"
        ));
        assert!(error_file_matches_touched(
            "D:/proj/src/app/LoginPage.tsx",
            "src/app/LoginPage.tsx"
        ));
        // Normalizzazione (backslash + ./).
        assert!(error_file_matches_touched(
            "./src/app\\LoginPage.tsx",
            "src/app/LoginPage.tsx"
        ));
        // NON deve matchare basename simili in dir diverse (confine di segmento).
        assert!(!error_file_matches_touched(
            "src/pages/BookingPage.tsx",
            "src/pages/LoginPage.tsx"
        ));
        assert!(!error_file_matches_touched("a/Page.tsx", "b/OtherPage.tsx"));
        assert!(!error_file_matches_touched("", "x"));
    }
}
