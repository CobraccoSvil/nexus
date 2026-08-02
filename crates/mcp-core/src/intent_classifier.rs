//! Classifier intent agentico LLM-based: e' l'unico motore di classificazione
//! dell'intent in Nexus.
//!
//! Metodo: un LLM piccolo e veloce (risolto via purpose `intent_classifier`,
//! regola G — niente nome modello hardcoded) produce un JSON strutturato che
//! viene parsato e validato. Niente keyword/embeddings.
//!
//! Chi lo chiama: `orchestrator::intent` per la classificazione del turno e
//! `native_engine` per i segnali del RouterNode. I `derive_*` sono il punto unico
//! (regola L) che traduce l'esito in `action_oriented`/`report_only`: chiunque
//! abbia bisogno di quei due booleani passa di qui, non li ri-deduce.
//!
//! Oltre all'intent, il giudizio del turno porta i segnali che PRIMA venivano
//! indovinati leggendo il testo con liste di keyword sparse per il codebase
//! (regola M): gli slot d'azione ([`ActionSlots`]), il cambio di provider/modello
//! chiesto dall'utente ([`ModelSwitchSignal`]) e le competenze che il task
//! richiede. Sono campi dello STESSO schema e della STESSA chiamata: non c'e' una
//! chiamata al modello per ciascuna domanda.
//!
//! Le competenze sono l'unico campo il cui vocabolario non e' una costante: e' il
//! roster figure del DB, iniettato nel prompt al posto di `{competenze}` e
//! riusato per validare la risposta. Quando il template non lo ospita il campo
//! resta `None` — "non dichiarabile" — e i consumatori lo sanno, invece di
//! ricevere una lista che il modello ha inventato senza conoscere i nomi ammessi.
//!
//! Punti unici riusati (regola L):
//! - estrazione JSON dalla risposta LLM: `nexus_types::llm_json::extract_json_block`
//! - cache TTL: `nexus_cache::TtlCache`
//! - lettura settings: `nexus_auth::get_setting`
//! - risoluzione modello: `internal_routing::resolve_purpose_model_db`
//! - chiamata LLM: `nexus_gateway::NexusGatewayClient::complete`
//! - schema slot: `routing_slots::ActionSlots`
//! - roster figure convocabili: `agent_tools::subagent_native::convocable_kinds`

use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::nexus_gateway::{GwMessage, GwMetadata, GwRequest, NexusGatewayClient};
use crate::routing_slots::ActionSlots;
use nexus_cache::TtlCache;

// ── Parametri operativi (NON nomi modello) ──────────────────────────────────
// Solo dimensioni di cache, timeout e soglie. Il MODELLO arriva sempre dal DB
// via purpose `intent_classifier` (regola G). Specchiano i DEFAULT_* del Python.

const DEFAULT_CACHE_TTL_SECONDS: u64 = 24 * 60 * 60; // 24 ore
                                                     // Parita' col Python: dimensione cache. `TtlCache` non ha cap hard (eviction
                                                     // gestita dal TTL), quindi questa costante e' documentazione del contratto: non
                                                     // e' letta dal codice (l'eviction e' solo TTL-based), ma documenta la parita'.
#[allow(
    dead_code,
    reason = "documenta la dimensione cache del Python; TtlCache evince solo per TTL"
)]
const DEFAULT_CACHE_MAX_ENTRIES: usize = 10_000;
const DEFAULT_LLM_TIMEOUT_SECONDS: f32 = 5.0;
const DEFAULT_AMBIGUITY_MIN_CONFIDENCE: f32 = 0.70;
const DEFAULT_AMBIGUITY_MIN_MARGIN: f32 = 0.15;

/// Chiavi settings.routing.* lette dal DB (parita' con `_CONFIG_KEYS` Python).
const KEY_CACHE_TTL: &str = "routing.classifier_cache_ttl_seconds";
const KEY_LLM_TIMEOUT: &str = "routing.llm_classifier_timeout_seconds";
const KEY_AMBIGUITY_MIN_CONFIDENCE: &str = "routing.ambiguity_min_confidence";
const KEY_AMBIGUITY_MIN_MARGIN: &str = "routing.ambiguity_min_margin";

/// Purpose tier-aware (mig 0338) usato per risolvere il modello del classifier.
const CLASSIFIER_PURPOSE: &str = "intent_classifier";

/// Setting con il prompt template del classifier (mig 0447). Il DB HA la
/// precedenza; la costante [`CLASSIFIER_PROMPT_FALLBACK`] e' usata SOLO se la
/// chiave e' assente o malformata (regola G: configurazione nel DB).
const KEY_CLASSIFIER_PROMPT: &str = "system.intent_classifier_prompt";

/// Intent ammessi: punto unico, devono coincidere con `brain/router/intents.py`
/// `ALLOWED_INTENTS` e con la colonna `intent_key` di `nexus_routing_matrix`.
/// `agentic_default` e' l'intent di SISTEMA (fallback neutro), non emesso dal
/// classifier LLM ma assegnato quando l'interpretazione semantica fallisce.
const ALLOWED_INTENTS: &[&str] = &[
    "chat",
    "debug",
    "fix",
    "refactor",
    "test",
    "docs",
    "architecture",
    "file_ops",
    "system_admin",
    "code_read",
    // Ricerca web citata (Perplexity Sonar): richieste esplicite di informazioni
    // aggiornate/fatti recenti dal web con fonti. Flusso NON-agentico.
    "ricerca_web",
    "agentic_default",
];

/// Livelli di complessita' canonici (`ALLOWED_COMPLEXITY` Python).
const ALLOWED_COMPLEXITY: &[&str] = &["low", "medium", "high"];

/// action_verb ammessi (`ALLOWED_ACTION_VERBS` Python).
const ALLOWED_ACTION_VERBS: &[&str] = &[
    "read",
    "write",
    "resolve",
    "analyze",
    "refactor",
    "configure",
    "deploy",
    "delete",
];

/// target.type ammessi (`ALLOWED_TARGET_TYPES` Python).
const ALLOWED_TARGET_TYPES: &[&str] = &[
    "code",
    "tests",
    "config",
    "service",
    "docs",
    "data",
    "infrastructure",
];

/// scope ammessi (`ALLOWED_SCOPES` Python).
const ALLOWED_SCOPES: &[&str] = &["single", "multi_file", "cross_service", "system_wide"];

/// Quanto resta valido in memoria il roster delle competenze convocabili. Stesso
/// ordine di grandezza delle altre cache di configurazione (60s): una figura
/// abilitata dall'admin entra nel vocabolario del classificatore entro un minuto.
const COMPETENCE_VOCAB_TTL_SECONDS: u64 = 60;

/// Placeholder del vocabolario delle competenze nel template del prompt. Il
/// `{message}` e' l'altro; questo e' OPZIONALE per costruzione — un template che
/// non lo contiene produce un giudizio senza il campo competenze, non un errore.
const PLACEHOLDER_COMPETENZE: &str = "{competenze}";

// ── Prompt template fallback (porting 1:1 di `_CLASSIFIER_PROMPT`) ───────────
// FALLBACK documentato (regola G): la fonte autoritativa e' la setting DB
// `system.intent_classifier_prompt` (mig 0447). Questa costante e' usata SOLO
// se il template DB e' assente o malformato (il `{message}` non sostituibile).
// `{message}` e' l'unico placeholder; le doppie graffe del Python (`{{`/`}}`)
// qui NON servono perche' la sostituzione e' un semplice replace, non un
// `.format()` su tutto il template.
const CLASSIFIER_PROMPT_FALLBACK: &str = r#"Intent classifier for a coding assistant. Return ONLY a JSON object, no markdown, no text.

Message: """{message}"""

Schema (all keys required):
{
"intent": one of ["chat","debug","fix","refactor","test","docs","architecture","file_ops","system_admin","code_read"],
"agentic_score": 0.0..1.0,
"requires_tools": bool,
"authorizes_changes": bool,
"complexity": "low"|"medium"|"high",
"confidence": 0.0..1.0,
"candidates": [{"intent":"...","confidence":0..1}, up to 3],
"slots": {
  "action_verb": "read"|"write"|"resolve"|"analyze"|"refactor"|"configure"|"deploy"|"delete",
  "target_type": "code"|"tests"|"config"|"service"|"docs"|"data"|"infrastructure",
  "framework": e.g. "playwright"|"pytest"|"cargo"|"jest"|"docker" or "" if generic,
  "scope": "single"|"multi_file"|"cross_service"|"system_wide",
  "confidence": 0.0..1.0
},
"model_switch": {"is_switch": bool, "provider": "provider slug or \"\"", "model": "model id or \"\""},
"competencies": [one or more of: {competenze}]
}

Intent meaning:
- chat=conversational, no action; debug=find root cause of failure; fix=repair specific known bug;
- refactor=restructure no behavior change; test=WRITE new tests; docs=write documentation;
- code_read=read/inspect files; architecture=high-level design; file_ops=create/delete files;
- system_admin=configure services/deploy.

CRITICAL:
- "scrivi test per X" -> intent=test, action_verb=write.
- "esegui test e correggi fail" / "fai funzionare i test" -> intent=debug, action_verb=resolve.
- "fix bug at file.py:42" -> intent=fix, action_verb=resolve, scope=single.
- "leggi file.py" -> intent=code_read, action_verb=read.
- "fai/crea/costruisci/realizza una app|applicazione|sistema|sito|servizio|piattaforma per X" -> intent=architecture, action_verb=write, scope=system_wide, complexity=high. E' scaffolding completo (PRD + schema DB + backend + frontend + test). NON e' docs.
- "scaffold/genera progetto" / "boilerplate" / "starter kit" -> intent=architecture, scope=system_wide.
- "imposta/configura/abilita un utente admin|il backend|un servizio|CORS|HTTPS", "setup X", "deploya/avvia X" -> intent=system_admin, requires_tools=true. E' un task agentico multi-step, NON chat anche se la frase e' breve.
- RETROSPECTIVE/META requests about work ALREADY done in this conversation -- "riassumi cosa hai fatto/sistemato", "spiega cosa e' successo", "che modifiche hai applicato?", "fammi il punto" -> intent=chat, requires_tools=false, agentic_score<=0.2. The user wants a TEXT answer about past work, NOT new actions or documentation files. NOT docs (docs = write documentation files into the repo).
"authorizes_changes" -- THE KEY report-vs-act judgment, decide it from the user's intent:
- true when the user wants the assistant to MODIFY code/system: fix, implement, refactor, scaffold, configure, deploy, delete, "fai funzionare", "correggi", "sistema", "crea". This is the default for action intents.
- false when the user wants only to INSPECT and be TOLD the result: "verifica/controlla che X compili|funzioni|risponda E riporta/dimmi l'esito", "controlla lo stato di X e fammi sapere", "fai un check e riportami", "leggi/spiega X". requires_tools can still be true (checks need build/test/curl/read) but NO code changes are wanted.
- CRITICAL: a verify/report task stays authorizes_changes=false EVEN IF a check FAILS -- finding something broken does NOT authorize fixing it; report it instead. Only switch to true if the user explicitly also asks to fix ("verifica e CORREGGI|SISTEMA|fai funzionare X").
- When unsure, prefer true (do not block legitimate fixes).

"model_switch" -- is this message a CONFIGURATION COMMAND that changes which provider/model answers, or is it WORK to do?
- is_switch=true ONLY for an explicit instruction to switch: "usa claude", "passa a gemini 2.5 pro", "switch to gpt-4o", "rispondi con mistral". Fill "provider" with the vendor slug (anthropic|openai|google|mistral|deepseek|...) and "model" with the model id if the user named one, "" otherwise.
- is_switch=false when the message merely MENTIONS a model or vendor while asking for work or an explanation: "voglio capire perche' gemini risponde male", "confronta claude e gpt", "il modello mistral va in timeout, indaga", "aggiungi il supporto a openai nel codice". These are tasks, not settings.
- WHEN IN DOUBT, is_switch=false. Mistaking a request for a switch SWALLOWS the user's task; mistaking a switch for a request only costs one turn.

"competencies" -- which professional lenses does this task actually need, from the closed list in the schema above. Empty list [] when the task needs none in particular. Use ONLY names from that list; never invent one.

Use confidence<0.7 honestly when ambiguous (downstream asks user). NEVER inflate.

Return ONLY the JSON object."#;

// ── Schema risultato ─────────────────────────────────────────────────────────

/// Intent candidato con confidence individuale (multi-label / disambiguazione).
/// Specchia `IntentCandidate` (Python).
#[derive(Debug, Clone, PartialEq)]
pub struct IntentCandidate {
    pub intent: String,
    pub confidence: f32,
}

/// Cambio di provider/modello che l'utente CHIEDE in questo turno, come lo
/// dichiara il classificatore.
///
/// E' un segnale, non un verdetto: dice che il modello ha letto un comando di
/// configurazione, non che quel provider esista o serva quel modello. Il verdetto
/// - con la validazione contro il listino - e' del punto unico
/// [`crate::model_switch`] (regola L).
///
/// `provider` e' obbligatorio perche' un cambio senza destinazione non e' un
/// cambio; `model` e' opzionale perche' "usa claude" non nomina un modello.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSwitchSignal {
    pub provider: String,
    pub model: Option<String>,
}

/// Risultato della classificazione agentica. Porting 1:1 di `AgenticIntent`
/// (Python): tutti i campi, stessi default fail-safe.
#[derive(Debug, Clone)]
pub struct AgenticIntent {
    pub intent: String,
    pub agentic_score: f32,
    pub requires_tools: bool,
    /// Complessita' del task (`low`/`medium`/`high`). Consumata dal gate
    /// `deliberate` del consiglio e dal resolver di dimensionamento
    /// (`orchestration_sizing::TaskComplexity::try_parse`, punto unico del parse).
    pub complexity: String,
    pub confidence: f32,
    pub model_used: String,
    pub cached: bool,
    pub fallback_used: bool,
    /// Giudizio agentico DIRETTO: l'utente AUTORIZZA modifiche in questo turno?
    /// Default `true` = fail-safe (non blocca i fix legittimi). Vedi nota Python.
    pub authorizes_changes: bool,
    /// Top 3 candidati sortati per confidence DESC. Contiene almeno `intent`.
    pub candidates: Vec<IntentCandidate>,
    /// True quando confidence < soglia OPPURE margine sul secondo < soglia.
    pub is_ambiguous: bool,
    /// Slot canonici (Livello 4 NLU). Riusa il tipo del punto unico routing_slots.
    pub slots: ActionSlots,
    /// Il turno e' un comando di cambio provider/modello? `None` = no, oppure il
    /// classificatore non ha risposto. L'asimmetria e' deliberata e vive nel
    /// parsing (`is_switch` di default `false`): un dubbio non deve mai mangiare
    /// la richiesta dell'utente.
    pub model_switch: Option<ModelSwitchSignal>,
    /// Competenze che il task richiede, nel vocabolario del roster figure
    /// iniettato nel prompt.
    ///
    /// `Some(v)` - anche `Some(vec![])` - e' una DICHIARAZIONE: il classificatore
    /// ha risposto conoscendo i nomi ammessi, e la lista vuota significa "nessuna
    /// lente d'ambito serve". `None` significa "non dichiarabile" (fallback di
    /// sistema, roster illeggibile o template senza il placeholder): i consumatori
    /// ripiegano sul vocabolario keyword e sanno perche'.
    pub competencies: Option<Vec<String>>,
}

impl AgenticIntent {
    /// Risultato di fallback NEUTRO (`agentic_default`) quando l'interpretazione
    /// semantica LLM non e' disponibile (LLM down/timeout/JSON invalido/config
    /// assente). Porting di `AgenticIntentClassifier._fallback_result`.
    ///
    /// `fallback_used=true`, `authorizes_changes=true` (fail-safe), niente
    /// disambiguazione (scelta di sistema deliberata, non incertezza).
    fn fallback(reason: &str) -> Self {
        let intent = "agentic_default".to_string();
        let confidence = 0.5;
        AgenticIntent {
            intent: intent.clone(),
            agentic_score: 0.6,
            requires_tools: true,
            complexity: "medium".to_string(),
            confidence,
            model_used: format!("fallback:{reason}"),
            cached: false,
            fallback_used: true,
            authorizes_changes: true,
            candidates: vec![IntentCandidate { intent, confidence }],
            is_ambiguous: false,
            slots: ActionSlots::default(),
            // Nessun giudizio: non "non e' uno switch e non serve nessuna
            // competenza", ma "non lo so". I due campi lo dicono in modo
            // distinguibile — `model_switch` perche' il ripiego onesto di quella
            // domanda coincide col no, `competencies` perche' il suo no e'
            // `Some(vec![])` e va tenuto separato dal silenzio.
            model_switch: None,
            competencies: None,
        }
    }
}

// ── DTO di deserializzazione del JSON LLM (tollerante) ───────────────────────

/// Forma grezza ricevuta dall'LLM. Tutti i campi `Option`/`default` cosi' che
/// un JSON con chiavi mancanti deserializzi comunque; la validazione successiva
/// (`validate_parsed`) decide se l'oggetto e' utilizzabile o se cadere in
/// fallback. I booleani sono `serde_json::Value` per applicare `strict_bool`
/// (i modelli quotano spesso i bool come stringhe "true"/"false").
#[derive(Debug, Deserialize, Default)]
struct RawIntent {
    #[serde(default)]
    intent: Option<String>,
    #[serde(default)]
    agentic_score: Option<f32>,
    #[serde(default)]
    requires_tools: Option<serde_json::Value>,
    #[serde(default)]
    authorizes_changes: Option<serde_json::Value>,
    #[serde(default)]
    complexity: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    candidates: Option<Vec<RawCandidate>>,
    #[serde(default)]
    slots: Option<RawSlots>,
    #[serde(default)]
    model_switch: Option<RawModelSwitch>,
    #[serde(default)]
    competencies: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct RawModelSwitch {
    #[serde(default)]
    is_switch: Option<serde_json::Value>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RawCandidate {
    #[serde(default)]
    intent: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
}

#[derive(Debug, Deserialize, Default)]
struct RawSlots {
    #[serde(default)]
    action_verb: Option<String>,
    #[serde(default)]
    target_type: Option<String>,
    #[serde(default)]
    framework: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
}

// ── Configurazione operativa risolta dal DB ──────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct ClassifierConfig {
    cache_ttl_seconds: u64,
    llm_timeout_seconds: f32,
    ambiguity_min_confidence: f32,
    ambiguity_min_margin: f32,
}

impl ClassifierConfig {
    /// Default tecnici per i SOLI parametri operativi (parita' con
    /// `_classifier_operational_defaults` Python). NON include il modello.
    fn defaults() -> Self {
        ClassifierConfig {
            cache_ttl_seconds: DEFAULT_CACHE_TTL_SECONDS,
            llm_timeout_seconds: DEFAULT_LLM_TIMEOUT_SECONDS,
            ambiguity_min_confidence: DEFAULT_AMBIGUITY_MIN_CONFIDENCE,
            ambiguity_min_margin: DEFAULT_AMBIGUITY_MIN_MARGIN,
        }
    }

    /// Legge le settings.routing.* dal DB (punto unico `get_setting`). Le chiavi
    /// assenti o non parsabili usano il default tecnico (mai un modello).
    async fn load(db: &PgPool) -> Self {
        let mut cfg = ClassifierConfig::defaults();
        if let Some(v) = read_f32_setting(db, KEY_CACHE_TTL).await {
            if v >= 0.0 {
                cfg.cache_ttl_seconds = v as u64;
            }
        }
        if let Some(v) = read_f32_setting(db, KEY_LLM_TIMEOUT).await {
            cfg.llm_timeout_seconds = v;
        }
        if let Some(v) = read_f32_setting(db, KEY_AMBIGUITY_MIN_CONFIDENCE).await {
            cfg.ambiguity_min_confidence = v;
        }
        if let Some(v) = read_f32_setting(db, KEY_AMBIGUITY_MIN_MARGIN).await {
            cfg.ambiguity_min_margin = v;
        }
        cfg
    }
}

/// Helper: legge una setting e la parsa come f32. `None` se assente o non
/// numerica (il caller usa il default tecnico).
async fn read_f32_setting(db: &PgPool, key: &str) -> Option<f32> {
    nexus_auth::get_setting(db, key)
        .await
        .and_then(|v| v.trim().parse::<f32>().ok())
}

// ── Cache TTL in-memory ──────────────────────────────────────────────────────
//
// Il `TtlCache` ha un TTL fisso fissato alla `new()`. Replichiamo la cache lazy
// del Python (costruita col TTL dal DB alla prima `classify`) con un `OnceLock`
// inizializzato col TTL letto al primo accesso. Se l'admin cambia il TTL a
// runtime, il valore precedente resta fino al riavvio del processo — stesso
// comportamento del Python (la cache TTL si ricrea solo al riavvio).
static CACHE: OnceLock<TtlCache<String, AgenticIntent>> = OnceLock::new();

fn cache_handle(ttl_seconds: u64) -> &'static TtlCache<String, AgenticIntent> {
    CACHE.get_or_init(|| TtlCache::new(Duration::from_secs(ttl_seconds)))
}

/// `sha256(message.trim()[:1000] | vocabolario)`.
///
/// Il vocabolario entra nella chiave perche' fa parte della DOMANDA posta al
/// modello: con un roster diverso la risposta ammessa e' diversa, e servire la
/// vecchia per 24 ore significherebbe rispondere a una domanda che non e' piu'
/// quella (regola O). Senza vocabolario iniettato la chiave torna a dipendere dal
/// solo messaggio.
fn cache_key(message: &str, vocab_fingerprint: &str) -> String {
    let head: String = message.trim().chars().take(1000).collect();
    let mut hasher = Sha256::new();
    hasher.update(head.as_bytes());
    hasher.update(b"|");
    hasher.update(vocab_fingerprint.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Roster delle competenze convocabili, in cache per
/// [`COMPETENCE_VOCAB_TTL_SECONDS`]: il classificatore gira su ogni turno e non
/// deve interrogare il DB ogni volta per una lista che cambia quando un admin
/// abilita una figura.
static COMPETENCE_VOCAB: OnceLock<TtlCache<&'static str, Vec<String>>> = OnceLock::new();

/// Le competenze che il classificatore puo' nominare: il roster figure del DB
/// (punto unico `convocable_kinds`, regola L — le figure sono un DATO, non un enum
/// nel codice). Lista vuota se il DB non risponde o la whitelist e' vuota: in quel
/// caso il campo `competencies` resta `None` e i consumatori ripiegano.
async fn competence_vocabulary(db: &PgPool) -> Vec<String> {
    let cache = COMPETENCE_VOCAB
        .get_or_init(|| TtlCache::new(Duration::from_secs(COMPETENCE_VOCAB_TTL_SECONDS)));
    if let Some(hit) = cache.get("roster") {
        return hit;
    }
    let kinds = crate::agent_tools::subagent_native::convocable_kinds(db).await;
    if kinds.is_empty() {
        tracing::warn!(
            "classifier: roster figure vuoto (DB o whitelist) -> campo competencies non dichiarabile"
        );
        return kinds;
    }
    cache.insert("roster", kinds.clone());
    kinds
}

// ── Parsing robusto ───────────────────────────────────────────────────────────

/// Parsing STRETTO di un booleano dal JSON LLM (punto unico, regola L —
/// porting di `_strict_bool` Python). Accetta bool nativo o le stringhe
/// canoniche "true"/"false"; qualunque altro valore -> `default` con WARN: il
/// degrado di un campo di GOVERNANCE non deve mai essere silenzioso.
fn strict_bool(raw: Option<&serde_json::Value>, field: &str, default: bool) -> bool {
    match raw {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => match s.trim().to_lowercase().as_str() {
            "true" => true,
            "false" => false,
            _ => {
                tracing::warn!(
                    field = %field,
                    "classifier: campo malformato (stringa non bool) -> default {default} (degrado loggato)"
                );
                default
            }
        },
        None => default,
        Some(other) => {
            tracing::warn!(
                field = %field, value = %other,
                "classifier: campo malformato -> default {default} (degrado loggato)"
            );
            default
        }
    }
}

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

/// Estrae e valida i 4 slot canonici (porting di `_parse_slots`). Valori non
/// canonici svuotano il campo corrispondente (il caller fa fallback al routing
/// classico). `framework` e' free-form. Best-effort: niente errori.
fn parse_slots(raw: Option<RawSlots>) -> ActionSlots {
    let Some(raw) = raw else {
        return ActionSlots::default();
    };
    let mut action_verb = raw.action_verb.unwrap_or_default().trim().to_lowercase();
    let mut target_type = raw.target_type.unwrap_or_default().trim().to_lowercase();
    let framework = raw.framework.unwrap_or_default().trim().to_lowercase();
    let mut scope = raw.scope.unwrap_or_default().trim().to_lowercase();
    let confidence = clamp01(raw.confidence.unwrap_or(0.0));

    if !ALLOWED_ACTION_VERBS.contains(&action_verb.as_str()) {
        action_verb.clear();
    }
    if !ALLOWED_TARGET_TYPES.contains(&target_type.as_str()) {
        target_type.clear();
    }
    if !ALLOWED_SCOPES.contains(&scope.as_str()) {
        scope.clear();
    }
    ActionSlots {
        action_verb,
        target_type,
        framework,
        scope,
        confidence,
    }
}

/// Estrae il segnale di cambio provider/modello. `None` = non e' uno switch.
///
/// ASIMMETRICO per contratto: `is_switch` ha default `false` e ogni forma dubbia
/// (oggetto assente, booleano malformato, provider vuoto) ricade sul no. Le due
/// direzioni dell'errore non si pagano allo stesso prezzo — un mancato
/// riconoscimento costa all'utente un turno, un riconoscimento di troppo GLI
/// MANGIA la richiesta, che viene consumata come comando di configurazione e non
/// arriva mai all'agente.
fn parse_model_switch(raw: Option<RawModelSwitch>) -> Option<ModelSwitchSignal> {
    let raw = raw?;
    if !strict_bool(raw.is_switch.as_ref(), "model_switch.is_switch", false) {
        return None;
    }
    let provider = raw.provider.unwrap_or_default().trim().to_lowercase();
    if provider.is_empty() {
        // Uno switch senza destinazione non e' uno switch: se il modello dice
        // "si'" ma non sa verso cosa, la risposta utile e' no.
        tracing::warn!("classifier: model_switch.is_switch=true senza provider -> ignorato");
        return None;
    }
    let model = raw
        .model
        .unwrap_or_default()
        .trim()
        .to_lowercase()
        .to_string();
    Some(ModelSwitchSignal {
        provider,
        model: (!model.is_empty()).then_some(model),
    })
}

/// Filtra le competenze dichiarate tenendo SOLO quelle del vocabolario iniettato
/// (insieme chiuso), normalizzate e deduplicate preservando l'ordine.
///
/// `None` quando la dichiarazione non e' valutabile: vocabolario vuoto (roster
/// illeggibile) o non iniettato nel prompt (template senza placeholder). In quei
/// casi il modello non poteva conoscere i nomi ammessi, quindi una sua lista non
/// vale come giudizio e i consumatori devono ripiegare sapendolo — diverso da
/// `Some(vec![])`, che e' la dichiarazione "nessuna competenza serve".
fn parse_competencies(
    raw: Option<Vec<String>>,
    vocab: &[String],
    vocab_injected: bool,
) -> Option<Vec<String>> {
    if !vocab_injected || vocab.is_empty() {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for item in raw? {
        let name = item.trim().to_lowercase();
        if name.is_empty() || out.contains(&name) {
            continue;
        }
        if vocab.iter().any(|k| k.eq_ignore_ascii_case(&name)) {
            out.push(name);
        } else {
            tracing::warn!(
                competenza = %name,
                "classifier: competenza fuori dal roster figure -> scartata"
            );
        }
    }
    Some(out)
}

/// Estrae fino a 3 candidati validati (porting di `_parse_candidates`). Se
/// assente/malformato ritorna `[top]`. Ordina per confidence DESC.
fn parse_candidates(
    raw: Option<Vec<RawCandidate>>,
    top_intent: &str,
    top_conf: f32,
) -> Vec<IntentCandidate> {
    let fallback = || {
        vec![IntentCandidate {
            intent: top_intent.to_string(),
            confidence: top_conf,
        }]
    };
    let Some(raw) = raw else {
        return fallback();
    };
    let mut out: Vec<IntentCandidate> = Vec::new();
    for item in raw.into_iter().take(3) {
        let Some(intent) = item.intent else { continue };
        let intent = intent.trim().to_lowercase();
        if !ALLOWED_INTENTS.contains(&intent.as_str()) {
            continue;
        }
        let Some(conf) = item.confidence else {
            continue;
        };
        out.push(IntentCandidate {
            intent,
            confidence: clamp01(conf),
        });
    }
    if out.is_empty() {
        return fallback();
    }
    out.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// True se il classificatore non e' abbastanza sicuro (porting di
/// `_is_ambiguous`): top confidence sotto soglia OPPURE margine sul secondo
/// candidato troppo stretto. Soglie DB-driven passate dal caller.
fn is_ambiguous(candidates: &[IntentCandidate], min_confidence: f32, min_margin: f32) -> bool {
    let Some(top) = candidates.first() else {
        return true;
    };
    if top.confidence < min_confidence {
        return true;
    }
    if let Some(second) = candidates.get(1) {
        if top.confidence - second.confidence < min_margin {
            return true;
        }
    }
    false
}

/// Valida il JSON parsato e costruisce un `AgenticIntent` (porting di
/// `_validate_parsed`). Ritorna `None` se lo schema e' invalido (intent o
/// complexity fuori enum, score/confidence mancanti): il caller cade in
/// fallback. Il campo `model_used` resta vuoto (riempito dal caller).
fn validate_parsed(
    raw: RawIntent,
    ambiguity_min_confidence: f32,
    ambiguity_min_margin: f32,
    vocab: &[String],
    vocab_injected: bool,
) -> Option<AgenticIntent> {
    let intent = raw.intent?.trim().to_lowercase();
    if !ALLOWED_INTENTS.contains(&intent.as_str()) {
        tracing::warn!(intent = %intent, "classifier: intent fuori enum");
        return None;
    }
    let agentic_score = clamp01(raw.agentic_score?);
    let requires_tools = strict_bool(raw.requires_tools.as_ref(), "requires_tools", true);
    let complexity = raw.complexity?.trim().to_lowercase();
    if !ALLOWED_COMPLEXITY.contains(&complexity.as_str()) {
        tracing::warn!(complexity = %complexity, "classifier: complexity fuori enum");
        return None;
    }
    let confidence = clamp01(raw.confidence?);
    let candidates = parse_candidates(raw.candidates, &intent, confidence);
    let ambiguous = is_ambiguous(&candidates, ambiguity_min_confidence, ambiguity_min_margin);
    let slots = parse_slots(raw.slots);
    let model_switch = parse_model_switch(raw.model_switch);
    let competencies = parse_competencies(raw.competencies, vocab, vocab_injected);
    // Default true = fail-safe: se l'LLM non popola il campo NON si bloccano i fix.
    let authorizes_changes =
        strict_bool(raw.authorizes_changes.as_ref(), "authorizes_changes", true);
    Some(AgenticIntent {
        intent,
        agentic_score,
        requires_tools,
        complexity,
        confidence,
        model_used: String::new(),
        cached: false,
        fallback_used: false,
        authorizes_changes,
        candidates,
        is_ambiguous: ambiguous,
        slots,
        model_switch,
        competencies,
    })
}

/// Costruisce il prompt dal DB (`system.intent_classifier_prompt`, mig 0447)
/// con fallback alla costante. Porting di `_classifier_prompt` (Python):
/// il DB ha la precedenza, la costante e' usata solo se il template e' assente
/// o non contiene il placeholder `{message}` (malformato).
/// Ritorna il prompt e se il vocabolario delle competenze e' stato DAVVERO
/// iniettato: un template privo di [`PLACEHOLDER_COMPETENZE`] produce un prompt
/// valido in cui il modello non conosce i nomi ammessi, e cio' che dicesse sul
/// campo `competencies` non sarebbe un giudizio (vedi [`parse_competencies`]).
async fn build_prompt(db: &PgPool, message: &str, vocab: &[String]) -> (String, bool) {
    let template = match nexus_auth::get_setting(db, KEY_CLASSIFIER_PROMPT).await {
        Some(tpl) if tpl.contains("{message}") => tpl,
        Some(_) => {
            tracing::warn!(
                "agentic_classifier: template DB privo di {{message}}, uso fallback costante"
            );
            CLASSIFIER_PROMPT_FALLBACK.to_string()
        }
        None => CLASSIFIER_PROMPT_FALLBACK.to_string(),
    };
    let injected = !vocab.is_empty() && template.contains(PLACEHOLDER_COMPETENZE);
    let prompt = template
        .replace(PLACEHOLDER_COMPETENZE, &vocab.join(", "))
        .replace("{message}", message);
    (prompt, injected)
}

// ── Punto di ingresso ────────────────────────────────────────────────────────

/// Classifica l'intent agentico di `message` via LLM.
///
/// Flusso (porting di `AgenticIntentClassifier.classify`):
/// 1. message vuoto -> fallback `agentic_default`.
/// 2. carica config operativa dal DB (TTL cache, timeout, soglie ambiguity).
/// 3. cache lookup (key `sha256(message[:1000])`, TTL da DB).
/// 4. risolve il modello via purpose `intent_classifier` (regola G, tier-aware);
///    se non risolvibile -> fallback.
/// 5. costruisce il prompt (DB con fallback costante) e chiama il gateway con
///    timeout configurabile.
/// 6. parse JSON robusto + validazione; su qualunque errore/timeout -> fallback.
/// 7. cache put del risultato valido.
///
/// Niente panico, niente magic-fallback di modello (regola G).
pub async fn classify(db: &PgPool, gateway: &NexusGatewayClient, message: &str) -> AgenticIntent {
    if message.trim().is_empty() {
        return AgenticIntent::fallback("empty_message");
    }

    let cfg = ClassifierConfig::load(db).await;
    let cache = cache_handle(cfg.cache_ttl_seconds);
    // Il vocabolario si risolve PRIMA della cache: fa parte della chiave, perche'
    // fa parte della domanda.
    let vocab = competence_vocabulary(db).await;
    let key = cache_key(message, &vocab.join(","));
    if let Some(mut hit) = cache.get(&key) {
        hit.cached = true;
        return hit;
    }

    // Il prompt Python tronca a 2000 char prima del format.
    let truncated: String = message.chars().take(2000).collect();
    let (prompt, vocab_injected) = build_prompt(db, &truncated, &vocab).await;

    // Candidati tier-aware CON FAILOVER (regola G/L): il classifier e'
    // latency-critical e NON deve morire se il PRIMO provider del tier e'
    // lento/instabile (es. Vertex cold-start ~8s che manda in timeout la singola
    // chiamata). `resolve_purpose_provider_candidates_db` ritorna N provider
    // DISTINTI del tier (health/cooldown-aware, niente provider hardcoded, stesso
    // routing del resto del sistema); si prova in ordine e si fa FAILOVER al
    // successivo su timeout/errore/CONTENT VUOTO (caso Gemini)/JSON invalido,
    // arrendendosi al neutro SOLO se TUTTI falliscono.
    let limit = nexus_auth::get_setting(db, "routing.classifier_failover_candidates")
        .await
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(3)
        .max(1);
    let candidates = match crate::internal_routing::resolve_purpose_provider_candidates_db(
        db,
        CLASSIFIER_PURPOSE,
        limit,
        // Il classificatore prova i candidati in cascata finche' uno risponde:
        // gli basta il primo, non chiede diversita' di provider.
        1,
    )
    .await
    {
        Ok(c) if !c.is_empty() => c,
        _ => {
            tracing::warn!("classifier: nessun candidato tier risolvibile -> fallback");
            return AgenticIntent::fallback("config_unavailable");
        }
    };

    let timeout = Duration::from_millis((cfg.llm_timeout_seconds * 1000.0) as u64);
    let validation = ValidationInputs {
        cfg: &cfg,
        vocab: &vocab,
        vocab_injected,
    };
    for cand in &candidates {
        let Some(mut validated) = try_classify_once(
            gateway,
            &cand.provider,
            &cand.model,
            &prompt,
            timeout,
            &validation,
        )
        .await
        else {
            tracing::warn!(
                provider = %cand.provider, model = %cand.model,
                "classifier: candidato fallito, failover al prossimo del tier"
            );
            continue;
        };
        if validated.model_used.is_empty() {
            validated.model_used = format!("{}/{}", cand.provider, cand.model);
        }
        // Osservabilita' (mig 0460): engine='rust' + provenienza, mai il prompt (regola F).
        tracing::info!(
            engine = "rust",
            intent = %validated.intent,
            agentic_score = validated.agentic_score,
            confidence = validated.confidence,
            requires_tools = validated.requires_tools,
            authorizes_changes = validated.authorizes_changes,
            model_used = %validated.model_used,
            "classifier intent (rust in-process, tier-failover)"
        );
        cache.insert(key, validated.clone());
        return validated;
    }
    tracing::warn!(
        candidates = candidates.len(),
        "classifier: TUTTI i candidati del tier falliti -> fallback neutro"
    );
    AgenticIntent::fallback("all_candidates_failed")
}

/// Cio' che serve a VALIDARE una risposta: le soglie di ambiguita' e il
/// vocabolario chiuso delle competenze, con l'informazione se e' stato davvero
/// mostrato al modello. Viaggiano insieme perche' validare con un vocabolario
/// diverso da quello iniettato darebbe un giudizio su una domanda che non e' stata
/// posta.
struct ValidationInputs<'a> {
    cfg: &'a ClassifierConfig,
    vocab: &'a [String],
    vocab_injected: bool,
}

/// UN tentativo di classificazione su un provider/model specifico. `None` su
/// QUALSIASI fallimento (timeout, errore gateway, provider-error inline, CONTENT
/// VUOTO, JSON non estraibile/invalido, schema invalido) cosi' il chiamante fa
/// FAILOVER al prossimo candidato del tier (regola M: fallimento dal segnale, non
/// dalla prosa). Isola la logica di una singola chiamata dal loop di failover.
async fn try_classify_once(
    gateway: &NexusGatewayClient,
    provider: &str,
    model: &str,
    prompt: &str,
    timeout: Duration,
    validation: &ValidationInputs<'_>,
) -> Option<AgenticIntent> {
    let req = GwRequest {
        model: format!("{provider}/{model}"),
        messages: vec![GwMessage {
            role: "user".to_string(),
            content: serde_json::Value::String(prompt.to_string()),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            thinking_signature: None,
            is_error: None,
        }],
        pin_provider: Some(provider.to_string()),
        metadata: GwMetadata {
            feature: CLASSIFIER_PURPOSE.to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let resp = match tokio::time::timeout(timeout, gateway.complete(req)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            tracing::warn!(provider, "classifier: gateway error ({e})");
            return None;
        }
        Err(_) => {
            tracing::warn!(provider, timeout_s = timeout.as_secs(), "classifier: timeout");
            return None;
        }
    };
    // CONTENT VUOTO o provider-error inline -> fallimento ritentabile (failover):
    // il caso Gemini/Vertex con content vuoto (finish_reason=length/token budget)
    // NON deve arrendersi al neutro, ma provare il prossimo provider.
    let stripped = resp.content.trim();
    if stripped.is_empty() || stripped.starts_with("[Error:") || stripped.starts_with("[error:") {
        tracing::warn!(provider, "classifier: content vuoto o provider-error inline");
        return None;
    }
    let value = nexus_types::llm_json::extract_json_block(&resp.content)?;
    let raw: RawIntent = serde_json::from_value(value).ok()?;
    let mut validated = validate_parsed(
        raw,
        validation.cfg.ambiguity_min_confidence,
        validation.cfg.ambiguity_min_margin,
        validation.vocab,
        validation.vocab_injected,
    )?;
    if !resp.model_used.is_empty() {
        validated.model_used = resp.model_used;
    }
    Some(validated)
}

// ── Punto unico derivazione action_oriented / report_only (regola L) ─────────
// Le decisioni `action_oriented` e `report_only` vivono in un solo posto: i call
// site (tool_choice forcing, G1, resoconto, routing post-executor) delegano qui
// invece di re-implementare la deduzione dai campi del classifier.

/// Default per `routing.action_oriented_min_agentic_score` (parita' col `0.5`
/// hardcoded nel ramo Python). Il caller passa il valore letto dal DB.
pub const DEFAULT_ACTION_ORIENTED_MIN_SCORE: f32 = 0.5;

/// Decide se il turno CORRENTE e' d'azione (tool use) o conversazionale.
///
/// Porting di `__init__.py:686-707`:
/// - `intent_hint` presente (disambiguazione risolta) -> `true` per costruzione.
/// - altrimenti, se il classifier ha risposto (`requires_tools` o
///   `agentic_score` presenti) -> `requires_tools OR agentic_score >= min_score`.
/// - altrimenti (classifier down/non configurato) -> `true` conservativo: i
///   guard anti-descrittivi restano attivi.
pub fn derive_action_oriented(
    intent_hint: Option<&str>,
    requires_tools: Option<bool>,
    agentic_score: Option<f32>,
    min_score: f32,
) -> bool {
    if intent_hint.is_some_and(|h| !h.trim().is_empty()) {
        return true;
    }
    if requires_tools.is_some() || agentic_score.is_some() {
        return requires_tools.unwrap_or(false) || agentic_score.unwrap_or(0.0) >= min_score;
    }
    true
}

/// Decide se il turno e' report-only (verifica/lettura senza modifiche).
///
/// Porting di `__init__.py:736-739`:
/// - classifier ha risolto -> `(NOT intent_hint) AND (NOT authorizes_changes)`.
/// - classifier degradato -> `NOT intent_hint`.
///
/// `intent_hint` (disambiguazione risolta) e' sempre un'azione: forza `false`.
pub fn derive_report_only(
    classifier_resolved: bool,
    intent_hint: Option<&str>,
    authorizes_changes: bool,
) -> bool {
    let has_hint = intent_hint.is_some_and(|h| !h.trim().is_empty());
    if classifier_resolved {
        !has_hint && !authorizes_changes
    } else {
        !has_hint
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_from_json(s: &str) -> RawIntent {
        let value = nexus_types::llm_json::extract_json_block(s).expect("json estraibile");
        serde_json::from_value(value).expect("deserializza RawIntent")
    }

    /// Roster di prova: i nomi sono `kind` come quelli che `convocable_kinds`
    /// legge dal DB.
    fn vocab() -> Vec<String> {
        ["sysadmin", "security_engineer", "ui_ux_designer"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// Validazione col vocabolario iniettato, come in produzione quando il
    /// template DB ospita il placeholder.
    fn validated(raw: RawIntent) -> Option<AgenticIntent> {
        validate_parsed(raw, 0.70, 0.15, &vocab(), true)
    }

    #[test]
    fn validate_parsed_risposta_valida() {
        let raw = raw_from_json(
            r#"{"intent":"fix","agentic_score":0.85,"requires_tools":true,
                "authorizes_changes":true,"complexity":"medium","confidence":0.90,
                "candidates":[{"intent":"fix","confidence":0.90}],
                "slots":{"action_verb":"resolve","target_type":"code","framework":"",
                         "scope":"single","confidence":0.85}}"#,
        );
        let r = validated(raw).expect("schema valido");
        assert_eq!(r.intent, "fix");
        assert!((r.agentic_score - 0.85).abs() < 1e-6);
        assert!(r.requires_tools);
        assert!(r.authorizes_changes);
        assert_eq!(r.complexity, "medium");
        assert_eq!(r.slots.action_verb, "resolve");
        assert_eq!(r.slots.target_type, "code");
        assert_eq!(r.slots.scope, "single");
        assert!(r.slots.is_complete());
        assert!(!r.is_ambiguous);
        assert!(!r.fallback_used);
    }

    #[test]
    fn validate_parsed_campi_mancanti_usano_default() {
        // requires_tools/authorizes_changes/candidates/slots assenti:
        // default fail-safe (true) e slots vuoti, candidato singolo derivato.
        let raw = raw_from_json(
            r#"{"intent":"chat","agentic_score":0.1,"complexity":"low","confidence":0.95}"#,
        );
        let r = validated(raw).expect("schema valido");
        assert_eq!(r.intent, "chat");
        assert!(r.requires_tools, "requires_tools assente -> default true");
        assert!(
            r.authorizes_changes,
            "authorizes_changes assente -> default true"
        );
        assert_eq!(r.candidates.len(), 1);
        assert_eq!(r.candidates[0].intent, "chat");
        assert!(!r.slots.is_complete(), "slots assenti -> incompleti");
    }

    #[test]
    fn validate_parsed_intent_fuori_enum_e_none() {
        let raw = raw_from_json(
            r#"{"intent":"banana","agentic_score":0.5,"complexity":"low","confidence":0.9}"#,
        );
        assert!(validated(raw).is_none());
    }

    #[test]
    fn validate_parsed_complexity_fuori_enum_e_none() {
        let raw = raw_from_json(
            r#"{"intent":"fix","agentic_score":0.5,"complexity":"extreme","confidence":0.9}"#,
        );
        assert!(validated(raw).is_none());
    }

    #[test]
    fn strict_bool_stringa_false_diventa_false() {
        // Il bug storico: bool("false") era True in Python permissivo.
        let raw = raw_from_json(
            r#"{"intent":"code_read","agentic_score":0.4,"requires_tools":"false",
                "authorizes_changes":"false","complexity":"low","confidence":0.9}"#,
        );
        let r = validated(raw).expect("schema valido");
        assert!(!r.requires_tools, "\"false\" quotato -> false");
        assert!(!r.authorizes_changes, "\"false\" quotato -> false");
    }

    #[test]
    fn strict_bool_stringa_true_e_bool_nativo() {
        assert!(strict_bool(Some(&serde_json::json!(true)), "f", false));
        assert!(!strict_bool(Some(&serde_json::json!(false)), "f", true));
        assert!(strict_bool(Some(&serde_json::json!("TRUE")), "f", false));
        assert!(!strict_bool(Some(&serde_json::json!("False")), "f", true));
        // Valore non riconducibile a bool -> default (degrado loggato).
        assert!(strict_bool(Some(&serde_json::json!(42)), "f", true));
        assert!(!strict_bool(None, "f", false));
    }

    #[test]
    fn is_ambiguous_per_confidence_bassa() {
        let cands = vec![IntentCandidate {
            intent: "fix".into(),
            confidence: 0.60,
        }];
        // 0.60 < 0.70 -> ambiguo
        assert!(is_ambiguous(&cands, 0.70, 0.15));
        // 0.80 >= 0.70 e nessun secondo -> non ambiguo
        let cands2 = vec![IntentCandidate {
            intent: "fix".into(),
            confidence: 0.80,
        }];
        assert!(!is_ambiguous(&cands2, 0.70, 0.15));
    }

    #[test]
    fn is_ambiguous_per_margine_stretto() {
        let cands = vec![
            IntentCandidate {
                intent: "debug".into(),
                confidence: 0.80,
            },
            IntentCandidate {
                intent: "fix".into(),
                confidence: 0.70,
            },
        ];
        // margine 0.10 < 0.15 -> ambiguo nonostante top alta
        assert!(is_ambiguous(&cands, 0.70, 0.15));
        // margine 0.20 >= 0.15 -> non ambiguo
        let cands2 = vec![
            IntentCandidate {
                intent: "debug".into(),
                confidence: 0.85,
            },
            IntentCandidate {
                intent: "fix".into(),
                confidence: 0.65,
            },
        ];
        assert!(!is_ambiguous(&cands2, 0.70, 0.15));
    }

    #[test]
    fn parse_slots_valori_non_canonici_svuotano_campo() {
        let raw = RawSlots {
            action_verb: Some("frobnicate".into()), // non canonico -> ""
            target_type: Some("CODE".into()),       // case-insensitive -> "code"
            framework: Some("Playwright".into()),   // free-form lower
            scope: Some("galaxy".into()),           // non canonico -> ""
            confidence: Some(1.5),                  // clamp a 1.0
        };
        let s = parse_slots(Some(raw));
        assert_eq!(s.action_verb, "");
        assert_eq!(s.target_type, "code");
        assert_eq!(s.framework, "playwright");
        assert_eq!(s.scope, "");
        assert!((s.confidence - 1.0).abs() < 1e-6);
        assert!(!s.is_complete());
    }

    #[test]
    fn parse_candidates_ordina_desc_e_filtra_sconosciuti() {
        let raw = vec![
            RawCandidate {
                intent: Some("fix".into()),
                confidence: Some(0.6),
            },
            RawCandidate {
                intent: Some("banana".into()), // fuori enum -> scartato
                confidence: Some(0.9),
            },
            RawCandidate {
                intent: Some("debug".into()),
                confidence: Some(0.8),
            },
        ];
        let out = parse_candidates(Some(raw), "fix", 0.6);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].intent, "debug"); // 0.8 prima di 0.6
        assert_eq!(out[1].intent, "fix");
    }

    #[test]
    fn parse_candidates_assenti_ritorna_top() {
        let out = parse_candidates(None, "chat", 0.99);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].intent, "chat");
        assert!((out[0].confidence - 0.99).abs() < 1e-6);
    }

    #[test]
    fn fallback_ha_proprieta_attese() {
        let f = AgenticIntent::fallback("timeout");
        assert_eq!(f.intent, "agentic_default");
        assert!(f.fallback_used);
        assert!(f.authorizes_changes, "fail-safe: non blocca i fix");
        assert!(f.requires_tools);
        assert!(!f.is_ambiguous, "scelta di sistema, non incertezza");
        assert_eq!(f.model_used, "fallback:timeout");
        assert_eq!(f.candidates.len(), 1);
        assert_eq!(f.candidates[0].intent, "agentic_default");
    }

    #[test]
    fn cache_key_stabile_e_tronca() {
        let v = "sysadmin,security_engineer";
        let a = cache_key("ciao", v);
        let b = cache_key("  ciao  ", v); // trim -> stessa chiave
        assert_eq!(a, b);
        // Due messaggi diversi -> chiavi diverse.
        assert_ne!(cache_key("ciao", v), cache_key("addio", v));
        // Lunghezza sha256 esadecimale.
        assert_eq!(a.len(), 64);
    }

    /// Il vocabolario e' parte della domanda: con un roster diverso la risposta
    /// ammessa e' diversa, quindi la voce in cache non e' la stessa.
    #[test]
    fn cache_key_cambia_col_vocabolario() {
        assert_ne!(
            cache_key("prepara il deploy", "sysadmin"),
            cache_key("prepara il deploy", "sysadmin,ui_ux_designer")
        );
    }

    #[test]
    fn build_prompt_fallback_costante_sostituisce_message() {
        // Senza DB: la sostituzione del placeholder funziona sulla costante.
        let p = CLASSIFIER_PROMPT_FALLBACK.replace("{message}", "leggi src/main.py");
        assert!(p.contains("leggi src/main.py"));
        assert!(!p.contains("{message}"));
    }

    /// Il template di ripiego DEVE ospitare il vocabolario: se il placeholder si
    /// perdesse, `parse_competencies` degraderebbe a `None` per sempre e il
    /// Consiglio non riceverebbe mai una competenza dichiarata.
    #[test]
    fn prompt_fallback_ospita_il_placeholder_competenze() {
        assert!(CLASSIFIER_PROMPT_FALLBACK.contains(PLACEHOLDER_COMPETENZE));
        assert!(CLASSIFIER_PROMPT_FALLBACK.contains("\"model_switch\""));
    }

    // ── model_switch: il verso costoso dell'errore e' il falso positivo ──────

    /// Percorso reale: il JSON del modello attraversa `extract_json_block` +
    /// `serde_json::from_value` come in `try_classify_once`, non un `RawIntent`
    /// costruito a mano (regola O).
    #[test]
    fn model_switch_comando_esplicito_riconosciuto() {
        let raw = raw_from_json(
            r#"{"intent":"chat","agentic_score":0.1,"complexity":"low","confidence":0.95,
                "model_switch":{"is_switch":true,"provider":"anthropic","model":"claude-sonnet-4-6"}}"#,
        );
        let r = validated(raw).expect("schema valido");
        let sw = r.model_switch.expect("switch riconosciuto");
        assert_eq!(sw.provider, "anthropic");
        assert_eq!(sw.model.as_deref(), Some("claude-sonnet-4-6"));
    }

    #[test]
    fn model_switch_senza_modello_nominato() {
        let raw = raw_from_json(
            r#"{"intent":"chat","agentic_score":0.1,"complexity":"low","confidence":0.9,
                "model_switch":{"is_switch":true,"provider":"mistral","model":""}}"#,
        );
        let sw = validated(raw)
            .expect("schema valido")
            .model_switch
            .expect("switch");
        assert_eq!(sw.provider, "mistral");
        assert!(sw.model.is_none(), "modello non nominato -> None");
    }

    /// Il caso che ha originato il fix: una DOMANDA su un modello. Se venisse
    /// letta come switch, il turno non arriverebbe mai all'agente.
    #[test]
    fn domanda_su_un_modello_non_e_uno_switch() {
        let raw = raw_from_json(
            r#"{"intent":"debug","agentic_score":0.7,"complexity":"medium","confidence":0.88,
                "model_switch":{"is_switch":false,"provider":"google","model":"gemini-2.5-pro"}}"#,
        );
        assert!(
            validated(raw).expect("schema valido").model_switch.is_none(),
            "is_switch=false -> nessuno switch, anche col provider nominato"
        );
    }

    #[test]
    fn model_switch_dubbio_ricade_sul_no() {
        // Oggetto assente, booleano non interpretabile, provider vuoto: tre forme
        // di dubbio, un solo esito ammesso.
        for body in [
            r#""#,
            r#","model_switch":{"is_switch":42,"provider":"openai"}"#,
            r#","model_switch":{"is_switch":true,"provider":"   "}"#,
        ] {
            let raw = raw_from_json(&format!(
                r#"{{"intent":"chat","agentic_score":0.1,"complexity":"low","confidence":0.9{body}}}"#
            ));
            assert!(
                validated(raw).expect("schema valido").model_switch.is_none(),
                "il dubbio non deve mai diventare uno switch: {body}"
            );
        }
    }

    // ── competenze: insieme chiuso e distinzione silenzio/nessuna ────────────

    #[test]
    fn competenze_fuori_roster_scartate_e_dedotte_una_sola_volta() {
        let raw = raw_from_json(
            r#"{"intent":"system_admin","agentic_score":0.8,"complexity":"medium","confidence":0.9,
                "competencies":["sysadmin","SYSADMIN","chief_happiness_officer"," security_engineer "]}"#,
        );
        let c = validated(raw)
            .expect("schema valido")
            .competencies
            .expect("dichiarate");
        assert_eq!(c, vec!["sysadmin", "security_engineer"]);
    }

    /// `Some(vec![])` (nessuna competenza serve) e `None` (non dichiarabile) non
    /// sono la stessa cosa: il primo e' un giudizio, il secondo un silenzio, e il
    /// consumatore ripiega solo sul secondo.
    #[test]
    fn competenze_nessuna_e_diverso_da_non_dichiarabile() {
        let vuota = raw_from_json(
            r#"{"intent":"chat","agentic_score":0.1,"complexity":"low","confidence":0.9,
                "competencies":[]}"#,
        );
        assert_eq!(
            validated(vuota).expect("schema valido").competencies,
            Some(Vec::new())
        );

        let senza_campo = raw_from_json(
            r#"{"intent":"chat","agentic_score":0.1,"complexity":"low","confidence":0.9}"#,
        );
        assert_eq!(
            validated(senza_campo).expect("schema valido").competencies,
            None,
            "campo assente -> niente dichiarazione"
        );

        // Vocabolario NON iniettato nel prompt: il modello non conosceva i nomi
        // ammessi, quindi la sua lista non e' un giudizio.
        let non_iniettato = raw_from_json(
            r#"{"intent":"chat","agentic_score":0.1,"complexity":"low","confidence":0.9,
                "competencies":["sysadmin"]}"#,
        );
        assert_eq!(
            validate_parsed(non_iniettato, 0.70, 0.15, &vocab(), false)
                .expect("schema valido")
                .competencies,
            None
        );
    }

    #[test]
    fn fallback_non_dichiara_ne_switch_ne_competenze() {
        let f = AgenticIntent::fallback("timeout");
        assert!(f.model_switch.is_none());
        assert!(
            f.competencies.is_none(),
            "il fallback tace: non e' 'nessuna competenza'"
        );
    }

    // ── derive_action_oriented (porting __init__.py:686-707) ─────────────────

    #[test]
    fn action_oriented_intent_hint_sempre_true() {
        // Disambiguazione risolta -> azione per costruzione, ignora gli altri.
        assert!(derive_action_oriented(
            Some("fix"),
            Some(false),
            Some(0.0),
            0.5
        ));
    }

    #[test]
    fn action_oriented_da_requires_tools() {
        assert!(derive_action_oriented(None, Some(true), Some(0.1), 0.5));
        assert!(!derive_action_oriented(None, Some(false), Some(0.1), 0.5));
    }

    #[test]
    fn action_oriented_da_agentic_score_soglia() {
        // requires_tools assente ma score presente: usa la soglia.
        assert!(derive_action_oriented(None, None, Some(0.5), 0.5));
        assert!(derive_action_oriented(None, None, Some(0.9), 0.5));
        assert!(!derive_action_oriented(None, None, Some(0.49), 0.5));
    }

    #[test]
    fn action_oriented_classifier_down_e_conservativo_true() {
        // Ne' requires_tools ne' agentic_score: classifier down -> true.
        assert!(derive_action_oriented(None, None, None, 0.5));
        // intent_hint vuoto = assente.
        assert!(derive_action_oriented(Some("  "), None, None, 0.5));
    }

    // ── derive_report_only (porting __init__.py:736-739) ─────────────────────

    #[test]
    fn report_only_classifier_risolto() {
        // Risolto + non autorizza modifiche + niente hint -> report_only true.
        assert!(derive_report_only(true, None, false));
        // Risolto + autorizza modifiche -> false (e' un'azione).
        assert!(!derive_report_only(true, None, true));
        // Hint presente -> sempre azione, mai report_only.
        assert!(!derive_report_only(true, Some("fix"), false));
    }

    #[test]
    fn report_only_classifier_degradato() {
        // Degradato: report_only = NOT intent_hint (default read/report).
        assert!(derive_report_only(false, None, true));
        assert!(derive_report_only(false, None, false));
        // Con hint -> azione.
        assert!(!derive_report_only(false, Some("debug"), false));
    }

    // ── Parita' classifier Rust vs Python (test di integrazione, #[ignore]) ──
    // Richiede servizi VIVI (DB, gateway su nexus_gateway_port, brain REST). Si
    // esegue a mano per la validazione del cutover classifier_engine='rust':
    //   cargo test --bin mcp-core -- --ignored --nocapture parita_classifier
    // Non gira in `pnpm verify` (nessun servizio esterno in CI).
    #[derive(serde::Deserialize)]
    struct PyIntent {
        intent: String,
        agentic_score: f32,
        requires_tools: bool,
        authorizes_changes: bool,
        #[serde(default)]
        fallback_used: bool,
    }

    

    fn truncate(s: &str, n: usize) -> String {
        if s.len() <= n {
            s.to_string()
        } else {
            format!("{}…", &s[..n.saturating_sub(1)])
        }
    }
    fn bool_c(b: bool) -> &'static str {
        if b {
            "T"
        } else {
            "F"
        }
    }

    // ── Migrazione 0658 (regola O: si legge dal migrator reale, non da una
    // stringa fabbricata nel test) ────────────────────────────────────────────

    /// Il prompt che il classificatore USA DAVVERO in produzione (mig 0447 +
    /// 0658), non una sua ricostruzione: se la migrazione avesse mancato il
    /// match esatto del testo da sostituire (es. per un CRLF o uno spazio
    /// diverso), l'UPDATE sarebbe un no-op silenzioso e questo test lo
    /// scoprirebbe leggendo la riga vera, non una copia.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn mig_0658_prompt_classificatore_porta_model_switch_e_competenze(pool: sqlx::PgPool) {
        let content: String = sqlx::query_scalar(
            "SELECT content FROM nexus_prompt_templates WHERE key = 'system.intent_classifier_prompt'",
        )
        .fetch_one(&pool)
        .await
        .expect("il template deve esistere (mig 0447)");
        assert!(
            content.contains("\"model_switch\": {{\"is_switch\": bool"),
            "schema JSON senza model_switch: {content}"
        );
        assert!(
            content.contains("\"competencies\": [one or more of: {competenze}]"),
            "schema JSON senza il placeholder del vocabolario competenze: {content}"
        );
        assert!(
            content.contains("\"model_switch\" -- is this message a CONFIGURATION COMMAND"),
            "guida is_switch assente: {content}"
        );
        assert!(
            content.contains("WHEN IN DOUBT, is_switch=false"),
            "l'asimmetria del ripiego (dubbio -> non e' uno switch) non e' nel prompt: {content}"
        );
        // Non duplicata: applicare la migrazione due volte (idempotenza, gia'
        // garantita dal guard strpos) non deve raddoppiare il blocco.
        assert_eq!(
            content.matches("\"model_switch\" --").count(),
            1,
            "il blocco di guida non deve comparire piu' di una volta: {content}"
        );
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn mig_0658_settings_soglie_seminate(pool: sqlx::PgPool) {
        let slots: String =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = 'routing.slots_min_confidence'")
                .fetch_one(&pool)
                .await
                .expect("routing.slots_min_confidence deve esistere");
        assert_eq!(slots.trim().parse::<f32>().unwrap(), 0.60);

        let profile_sim: String = sqlx::query_scalar(
            "SELECT value FROM settings WHERE key = 'orchestrator.profile_auto_select_min_similarity'",
        )
        .fetch_one(&pool)
        .await
        .expect("orchestrator.profile_auto_select_min_similarity deve esistere");
        assert_eq!(profile_sim.trim().parse::<f32>().unwrap(), 0.55);
    }

    /// Il caso concreto che ha originato la regola: `certificat` non e' MAI una
    /// parola intera in italiano, quindi dal passaggio al match a parola intera
    /// (mig 0650) era diventata muta. Letta dal migrator reale: se il guard
    /// LIKE della migrazione non avesse trovato la sottostringa esatta seminata
    /// da mig 0553, la keyword morta sarebbe rimasta — questo test lo
    /// scoprirebbe.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn mig_0658_certificat_sostituita_da_parole_intere(pool: sqlx::PgPool) {
        let value: String = sqlx::query_scalar(
            "SELECT value FROM settings WHERE key = 'orchestrator.council_infra_keywords'",
        )
        .fetch_one(&pool)
        .await
        .expect("orchestrator.council_infra_keywords deve esistere (mig 0553)");
        let keywords: Vec<&str> = value.split(',').collect();
        assert!(
            !keywords.contains(&"certificat"),
            "la keyword morta deve essere sparita: {value}"
        );
        for parola in ["certificato", "certificati", "certificazione"] {
            assert!(
                keywords.contains(&parola),
                "manca '{parola}' fra le keyword infra: {value}"
            );
        }
    }
}
