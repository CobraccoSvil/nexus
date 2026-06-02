use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

/// Flag globale per il classificatore LLM degli intent.
/// Inizializzato da `main.rs` dopo la lettura del DB (settings.llm_classifier_enabled).
/// L'env var `NEXUS_LLM_CLASSIFIER_ENABLED` resta come override di emergenza
/// (applicata a ogni chiamata, priorita' piu' alta del valore atomico).
static LLM_CLASSIFIER_ENABLED: AtomicBool = AtomicBool::new(true);

/// Imposta il valore del flag dal DB all'avvio. Chiamato da `main.rs`.
pub fn set_llm_classifier_enabled(val: bool) {
    LLM_CLASSIFIER_ENABLED.store(val, Ordering::Relaxed);
}
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use mcp_proto::neural::{
    neural_core_service_client::NeuralCoreServiceClient, ClassifyIntentRequest, EmbedTextRequest,
    GenerateAgentTurnRequest, GenerateCompletionRequest, RouteModelRequest,
};

use crate::{
    provider_cooldown::{is_provider_in_cooldown, put_provider_in_cooldown},
    billing::{self, UsageNumbers},
    nexus_gateway::{NexusGatewayClient, GwMessage, GwMetadata, GwRequest, intent_to_alias},
    domain::OrchestratorAudit,
    vector_memory,
};

const KNOWN_PROVIDERS: [&str; 5] = ["anthropic", "openai", "google", "deepseek", "mistral"];
const KNOWN_INTENTS: [&str; 6] = ["fix", "refactor", "test", "docs", "architecture", "chat"];

// ---------------------------------------------------------------------------
// Routing semantico inline — nessuna chiamata gRPC, zero latenza aggiuntiva
// ---------------------------------------------------------------------------

/// Classifica l'intent dal testo del messaggio tramite keyword matching.
/// Rispecchia esattamente la logica Python in brain/router/service.py.
fn classify_intent_local(message: &str) -> (&'static str, f32) {
    let lower = message.to_lowercase();

    // ── Intent "debug" prioritario ────────────────────────────────────────
    // Riconosce pattern di stack trace o richieste agentiche multi-step su errori
    // reali (.NET / Java / Python / Rust / Node). Questo intent va a tier "heavy"
    // perché richiede sequence di tool call (read_file → str_replace → restart),
    // che modelli "code-light" come codestral non sanno orchestrare.
    let stack_trace_signals = [
        "\n   at ", "\n    at ", "traceback (most recent call last)",
        ".exception", ".error:", "stack trace", "stacktrace",
        "panicked at", "fatal:", "rejectedexecutionexception",
    ];
    let agentic_signals = [
        "analizza la gerarchia", "causa radice", "root cause",
        "stack trace", "tool call", "fixare", "esegui restart",
        "leggi il file", "appsettings", "log degli ultimi",
    ];
    let stack_hits: usize = stack_trace_signals.iter().filter(|s| lower.contains(*s)).count();
    let agent_hits: usize = agentic_signals.iter().filter(|s| lower.contains(*s)).count();
    // Se trovo sia un segno di stack trace sia un segno di richiesta agentica,
    // o se trovo molti segni di stack trace (≥2), instrado verso "debug".
    if stack_hits >= 2 || (stack_hits >= 1 && agent_hits >= 1) || agent_hits >= 2 {
        let confidence = (0.85 + (stack_hits + agent_hits) as f32 * 0.02).min(0.97);
        return ("debug", confidence);
    }

    // Order matters: file_ops e system_admin valutati PRIMA di fix/refactor
    // perche' "elimina i file Dockerfile" matcha sia "elimina" sia "file" sia
    // "Dockerfile" e finirebbe in fix; ma e' un task di file management,
    // non di bug fixing. Stesso ragionamento per system_admin (docker,
    // systemctl, container) che richiede modelli con tool use solido.
    let rules: &[(&str, &[&str])] = &[
        ("file_ops", &[
            "elimina file", "rimuovi file", "cancella file", "remove file",
            "delete file", "elimina i file", "remove the file",
            "elimina la cartella", "rimuovi la cartella", "delete folder",
            "elimina dockerfile", "rimuovi dockerfile",
            "elimina docker-compose", "rimuovi docker-compose",
            "elimina configurazione docker", "remove docker configuration",
            "elimina file di configurazione", "rimuovi file di configurazione",
            "ripulisci la directory", "cleanup directory",
        ]),
        ("system_admin", &[
            "docker stop", "docker rm", "docker prune", "system prune",
            "ferma il container", "stop container", "kill container",
            "elimina container", "remove container", "delete container",
            "ferma il servizio", "stop service", "systemctl stop",
            "systemctl restart", "restart service",
            "compose down", "compose up", "docker compose",
            "elimina docker", "rimuovi docker locale", "elimina docker locale",
            // Anche i pattern dell'analyzer: "rimuovere il container",
            // "container ridondante", ecc.
            "rimuovere il container", "rimuovere container",
            "container ridondante", "container superfluo",
        ]),
        ("fix",          &["/fix", "bug", "error", "crash", "broken", "debug", "issue", "patch", "errore", "correggi", "risolvi", "problema"]),
        // Refactor include task di migrazione codice (es. "migra il backend da SQL Server a PostgreSQL",
        // "converti TypeScript a JavaScript", "sostituisci EFCore con Npgsql"). Senza questi prefissi
        // laschi, "migra .NET 9 ..." finiva nel default "chat" → mistral-small inadatto.
        // I prefissi (no parola intera) catturano forme verbali italiane multiple:
        // "migra/migrare/migrate", "porta/portare", "converti/convertire", "trasforma/trasformare",
        // "sposta/spostare", "sostituisci/sostituire", "rimpiazza/rimpiazzare".
        ("refactor",     &["/refactor", "refactor", "clean", "simplify", "extract", "improve",
                           "migliora", "pulisci", "semplifica", "ristruttura",
                           "migra ", "migrare ", "porta da ", "porta a ", "portare da ",
                           "converti ", "convertire ", "trasform", "sposta da ", "spostare da ",
                           "sostituisci ", "sostituire ", "rimpiazza ", "rimpiazzare ",
                           "migrate from", "migrate to", "convert to", "rename "]),
        ("test",         &["/test", "test", "coverage", "assert", "spec", "unit test", "integration test"]),
        ("docs",         &["/docs", "document", "readme", "jsdoc", "comment", "explain", "documenta", "commenta", "spiega"]),
        // Architecture e' per task di PLANNING (piano di migrazione, design system) senza
        // toccare ancora codice. I verbi imperativi di migrazione codice vanno a "refactor".
        ("architecture", &["/arch", "architecture", "design", "system", "plan",
                           "architettura", "progetta",
                           "piano di migrazione", "migration plan",
                           "strategia di migrazione", "migration strategy"]),
    ];

    for (intent, keywords) in rules {
        let matches: u32 = keywords.iter().filter(|kw| lower.contains(*kw)).count() as u32;
        if matches >= 1 {
            let confidence = (0.75 + matches as f32 * 0.05).min(0.95);
            return (intent, confidence);
        }
    }

    ("chat", 0.82)
}

/// Detecta se il messaggio contiene keyword distruttivi (rm -rf, drop table,
/// docker prune, force push). In tal caso il routing promuove il
/// behavior_mode a "approfondita" cosi' viene scelto un modello capable
/// (Claude Sonnet/Opus, GPT-4.1) invece dei modelli "lite" che tendono
/// a interpretare liberamente le richieste distruttive.
///
/// Le keyword sono **prefissi/sottostringhe** intenzionalmente lasche:
/// `"elimin"` matcha "elimina/eliminare/elimini/eliminate", `"rimuov"`
/// matcha "rimuovi/rimuovere/rimuove/rimosso", ecc. Cosi' evitiamo bug
/// di mancato match dovuti a forme verbali diverse (e.g. infinito vs
/// imperativo) — la trade-off di qualche falso positivo e' accettabile
/// (al peggio scegliamo un modello piu' capace di quanto serva).
fn is_risky_task(message: &str) -> bool {
    let lc = message.to_lowercase();
    const RISKY: &[&str] = &[
        // Filesystem distruttive
        "rm -rf", " rm ", "rmdir", "unlink",
        // Verbi distruttivi (prefissi: matchano forme infinitive/imperative/coniugate)
        "elimin", "rimuov", "cancell", "delete", "remove",
        // Docker / container
        "docker prune", "system prune", "docker rm ", "docker rmi",
        "compose down", "ferma il container", "stop container",
        // Database
        "drop table", "drop database", "drop schema", "truncate",
        // Git distruttive
        "git reset --hard", "force push", "--force", "git clean",
        // Sistema
        "shutdown", "reboot", "systemctl stop", "systemctl disable",
        "kill -9", "pkill",
    ];
    RISKY.iter().any(|kw| lc.contains(*kw))
}

/// Detecta se il messaggio descrive un **task agentico multi-step** che
/// richiede esplorazione codebase, lettura file, modifica config, esecuzione
/// comandi — anche quando il fraseggio breve farebbe pensare a una semplice
/// chat (`intent=chat`). Esempi:
///   - "imposta un utente admin per l'applicazione"
///   - "configura il backend"
///   - "crea un endpoint per /healthz"
///   - "abilita HTTPS sul dev server"
///   - "deploya il microservizio doc-service"
///
/// Senza questo detector tali richieste con `intent=chat` cadrebbero su
/// modelli leggeri (gemini-flash, gpt-4.1-nano, mistral-small) inadeguati
/// per orchestrare tool call. Il chiamante riclassifica l'intent a
/// `system_admin` (già mappato a modelli capable in `nexus_routing_matrix`).
///
/// Le keyword sono **prefissi laschi** che matchano forme verbali multiple
/// (imposta/imposti/impostare/impostazione, ecc.) — falsi positivi sono
/// accettabili (al peggio scegliamo un modello più capace di quanto serva).
/// `is_risky_task` ha priorità: i verbi distruttivi vanno comunque a
/// `approfondita`, non riclassificati a `system_admin`.
fn is_agentic_request(message: &str) -> bool {
    let lc = message.to_lowercase();
    const AGENTIC_VERBS: &[&str] = &[
        // Setup / configurazione
        "imposta", "impost", "configur", "setup", "set up", "set-up",
        "abilit", "disabilit", "enable", "disable",
        // Creazione / modifica
        "crea ", "create ", "creare ", "aggiung", "add ",
        "cambi", "modific", "aggiorn", "update ", "modify ",
        // Deploy / esecuzione
        "deploy", "lancia ", "launch", "avvia", "start service",
        "installa", "install ",
        // Investigazione + azione
        "trova ", "find ", "individua", "identifica",
        "verifica ", "verify ", "controlla ",
        // Implementazione / integrazione
        "implementa", "integra", "integrate ", "implement",
        // Riparazione (oltre fix che è già intent dedicato)
        "ripar", "ripara",
        // Domande "come/dove" + azione (heuristic for "how to do X")
        "come faccio a", "come si imposta", "come configurare",
        "how do i ", "how to set",
    ];
    // Matching: almeno una keyword + il messaggio non è puramente informativo.
    // Heuristic per escludere domande puramente "cos'e'": se inizia con
    // "cos'e'", "che cosa", "what is", il task è informativo, non agentico.
    let purely_informational = lc.starts_with("cos'e")
        || lc.starts_with("cosa e")
        || lc.starts_with("che cosa")
        || lc.starts_with("what is")
        || lc.starts_with("spiegami");
    if purely_informational {
        return false;
    }
    AGENTIC_VERBS.iter().any(|kw| lc.contains(*kw))
}

/// Detecta se il messaggio descrive **risoluzione di test falliti** (non
/// semplice creazione/esecuzione di test): "esegui i test e risolvi i fail",
/// "lancia Playwright e correggi gli errori", "fai funzionare i test che
/// falliscono", ecc.
///
/// Senza questo detector tali richieste con `intent=test` cadrebbero sulla
/// routing matrix `test|bilanciata` che usa modelli LEGGERI (deepseek-chat,
/// gpt-4.1-mini) — questi non sono in grado di orchestrare multi-file edit,
/// debug AuthJS, refactor config. Il chiamante riclassifica l'intent a
/// `fix_complesso` (mappato a claude-haiku/claude-sonnet/mistral-large in
/// `nexus_routing_matrix`).
///
/// Le keyword sono **prefissi laschi** che matchano forme verbali multiple:
/// (risolv/risolvere/risolto, fix/fixa/fixato, correg/correggere/corretto).
/// Falsi positivi accettabili (al peggio modello piu' capace di quanto serva).
fn is_test_failure_resolution(message: &str) -> bool {
    let lc = message.to_lowercase();
    // Deve menzionare i test E richiedere un'azione correttiva
    let mentions_tests = ["test", "playwright", "vitest", "jest", "pytest", "cargo test"]
        .iter()
        .any(|kw| lc.contains(*kw));
    if !mentions_tests {
        return false;
    }
    // Verbi/pattern correttivi (prefissi che coprono coniugazioni it/en).
    // Sono inclusi anche pattern di fallimento osservati in produzione
    // (problemi di catch playwright_test): "falliti", "fallit*", "fix m"
    // ("fix M44" è un format ricorrente nei prompt di Nexus M-tickets).
    const CORRECTIVE_VERBS: &[&str] = &[
        "risolv", "fix", "correg", "ripar", "ripara",
        "fai funzionare", "fai passare", "make pass", "make work",
        "fai partire e", "esegui e", "lancia e",
        "applica fix", "applica patch", "applica corre",
        "make them pass", "pass all tests", "tutti i test passino",
        "fai in modo che", "fai sì che", "fa sì che",
        "non funziona", "non funzionano", "non passano",
        "stanno fallendo", "are failing", "is failing", "failing",
        "failure", "failures", "failed",
        // Italiano: fallit* matcha fallito/falliti/fallita/fallite
        "fallit", "falliscono", "fallimento", "fallimenti",
        // Format M-ticket Nexus: "Fix M44: ..." viene generato per ogni problema
        "fix m", "errore — problema",
        // Errori da error-fix workflow
        "errore rilevato", "severita: error", "severità: error",
    ];
    CORRECTIVE_VERBS.iter().any(|kw| lc.contains(*kw))
}

/// Variante di `classify_intent_local` che applica la promozione agentic:
/// se l'intent classificato e' `chat` ma `is_agentic_request` rileva una
/// richiesta multi-step (es. "imposta un utente admin"), riclassifica come
/// `system_admin` per dirottare a modelli capable in routing matrix.
///
/// Inoltre: se l'intent e' `test` ma `is_test_failure_resolution` rileva
/// una richiesta di risoluzione fail, riclassifica come `fix_complesso`
/// per evitare il routing a modelli leggeri (gpt-4.1-mini).
///
/// Usata come **fallback** quando il classifier LLM non e' disponibile.
/// Path async preferito: `classify_intent_async`.
fn classify_intent_with_agentic_promotion(message: &str) -> (&'static str, f32) {
    let (intent, confidence) = classify_intent_local(message);
    if intent == "chat" && is_agentic_request(message) {
        // Confidence ridotta perche' e' una promozione euristica, non una
        // classificazione diretta.
        return ("system_admin", 0.70);
    }
    // Promozione → fix_complesso quando il messaggio chiede di RISOLVERE
    // fail di test (es. "esegui i test e correggi gli errori"). Senza
    // questa promozione il task finisce su modelli light incapaci di
    // orchestrare multi-file edit + debug.
    // Sia `intent="test"` (creazione test interpretata male) sia
    // `intent="fix"` (intent generico senza tier) vengono promossi:
    // `fix_complesso` mappa esplicitamente a modelli capable in DB.
    if (intent == "test" || intent == "fix") && is_test_failure_resolution(message) {
        return ("fix_complesso", 0.70);
    }
    (intent, confidence)
}

/// Classificatore DETERMINISTICO di fallback (keyword/pattern based, IT+EN).
///
/// Esiste per rendere la classificazione intent **indipendente da un LLM che
/// puo' fallire** (overload, quota, timeout). Viene usato in due punti di
/// `classify_intent_full`:
///   (a) come **pre-check** prima di chiamare l'LLM: se un pattern agentico ad
///       altissima confidenza matcha, si salta del tutto la chiamata HTTP
///       (piu' veloce e robusto);
///   (b) come **fallback** quando l'LLM fallisce: invece di degradare a
///       `chat` (che NON attiva il path agent + tool), si usa il risultato
///       deterministico se disponibile.
///
/// Ritorna `Some((intent_static, confidence))` con un intent **ammesso dalla
/// routing matrix** (vedi `intent_str_to_static` + le promozioni
/// `system_admin` / `fix_complesso`). Ritorna `None` quando nessun pattern
/// matcha, lasciando decidere all'LLM o al default `chat`.
///
/// I pattern sono prefissi/sottostringhe lasche per coprire le forme verbali
/// italiane (crea/creare/crei, implementa/implementare, ecc.) e gli
/// equivalenti inglesi. Falsi positivi sono accettabili: al peggio si attiva
/// il path agent quando non strettamente necessario, che e' molto meno grave
/// del falso negativo opposto (task agentico trattato come chat).
fn deterministic_intent_fallback(message: &str) -> Option<(&'static str, f32)> {
    let lc = message.to_lowercase();
    if lc.trim().is_empty() {
        return None;
    }

    // Escludi le richieste puramente informative ("cos'e'", "che cosa", ...):
    // non sono task agentici anche se contengono verbi che altrimenti
    // matcherebbero. Coerente con `is_agentic_request`.
    let purely_informational = lc.starts_with("cos'e")
        || lc.starts_with("cosa e")
        || lc.starts_with("che cosa")
        || lc.starts_with("what is")
        || lc.starts_with("spiegami");

    // ── 0. Pattern DOCS: valutati PRIMA dell'agentico generico perche'
    // "scrivi readme per il progetto" matcherebbe altrimenti il blocco
    // agentico (verbo "scrivi" + contesto "progetto"). DOCS e' piu' specifico.
    const DOCS_PATTERNS: &[&str] = &[
        "documenta", "genera doc", "genera la doc", "genera documentazione",
        "crea documentazione", "crea la documentazione",
        "scrivi readme", "scrivi il readme", "genera readme",
        "write docs", "generate docs", "write readme",
    ];
    if DOCS_PATTERNS.iter().any(|p| lc.contains(*p)) {
        return Some(("docs", 0.70));
    }

    // ── 1. Pattern AGENTICI forti: creazione/modifica codice e infra. ──────
    // Verbi imperativi/infinitivi di azione su artefatti software. Match ad
    // ALTA confidenza (0.85): attiva il path agent (intent != "chat").
    // L'intent scelto e' `system_admin`, gia' mappato a modelli capable con
    // tool use solido in `nexus_routing_matrix` (stesso target della
    // promozione agentic esistente).
    const AGENTIC_VERBS: &[&str] = &[
        "crea ", "creare", "crei ",
        "implementa", "implementare",
        "sviluppa", "sviluppare",
        "costruisci", "costruire",
        "genera ", "generare",
        "scrivi ", "scrivere",
        "aggiung", "add ",
        "modific", "modify ",
        "corregg", "fixa", "fix ",
        "refactor",
        "installa", "install ",
        "configur",
        "avvia", "esegui ", "lancia ",
        "build", "deploy",
        "scaffold",
    ];
    // Contesto che qualifica il verbo come task software (evita falsi positivi
    // tipo "crea un account sul sito X" che non e' un task di codice).
    const SOFTWARE_CONTEXT: &[&str] = &[
        "app", "applicazione", "progetto", "project",
        "file", "funzione", "function", "componente", "component",
        "servizio", "service", "endpoint", "server", "api",
        "script", "test", "codice", "code", "feature",
        "modulo", "module", "classe", "class", "container",
        "docker", "database", "schema", "migrazione", "migration",
        "pagina", "page", "form", "route",
    ];
    if !purely_informational {
        let has_verb = AGENTIC_VERBS.iter().any(|v| lc.contains(*v));
        let has_ctx = SOFTWARE_CONTEXT.iter().any(|c| lc.contains(*c));
        if has_verb && has_ctx {
            return Some(("system_admin", 0.85));
        }
    }

    // ── 2. Pattern di LETTURA / ANALISI: confidence media. ─────────────────
    // Richieste di ispezione del codice/stato che attivano comunque tool di
    // lettura (intent != "chat"). Mappa a `debug` (intent ammesso con tool
    // use) — non esiste `code_read`/`analyze` nella matrix: `debug` e'
    // l'intent piu' vicino che instrada a modelli capaci di leggere file.
    const READ_VERBS: &[&str] = &[
        "leggi ", "mostra", "analizza", "analizzare",
        "cosa fa", "che cosa fa", "quanti ", "quante ",
        "elenca", "lista ", "trova ", "cerca ", "individua",
        "ispeziona", "esamina",
    ];
    const READ_CONTEXT: &[&str] = &[
        "file", "src/", "codice", "code", "funzione", "function",
        "classe", "class", "modulo", "module", "errore", "error",
        "log", "endpoint", "test", "progetto", "repository", "repo",
    ];
    if !purely_informational {
        let has_read_verb = READ_VERBS.iter().any(|v| lc.contains(*v));
        let has_read_ctx = READ_CONTEXT.iter().any(|c| lc.contains(*c));
        if has_read_verb && has_read_ctx {
            return Some(("debug", 0.65));
        }
    }

    None
}

/// Risultato del classifier LLM (Fase 2). Specchia il JSON dell'endpoint
/// `POST /classify-intent-agentic` esposto da `brain/grpc_server/main.py`.
#[derive(Debug, Clone, serde::Deserialize)]
struct AgenticIntentResponse {
    intent: String,
    agentic_score: f32,
    requires_tools: bool,
    #[allow(dead_code)]
    complexity: String,
    confidence: f32,
    #[allow(dead_code)]
    model_used: String,
    #[serde(default)]
    cached: bool,
    #[serde(default)]
    fallback_used: bool,
    /// Top 3 candidati sortati per confidence DESC. Sempre contiene almeno
    /// `intent` come primo elemento.
    #[serde(default)]
    candidates: Vec<IntentCandidate>,
    /// True se il classifier ritiene la decisione ambigua (confidence < 0.70
    /// oppure margine sul secondo candidato < 0.15). Quando true il caller
    /// dovrebbe chiedere disambiguazione all'utente invece di indovinare.
    #[serde(default)]
    is_ambiguous: bool,
    /// Slot canonici per routing slot-based (Livello 4 NLU, mig 0133).
    /// Se `slots.is_complete()` E `slots.confidence >= soglia`, il caller
    /// usa `nexus_routing_slots_matrix` come fonte primaria di routing
    /// (piu' specifica della classica (intent, behavior_mode)).
    #[serde(default)]
    slots: crate::routing_slots::ActionSlots,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct IntentCandidate {
    pub intent: String,
    pub confidence: f32,
}

/// Risultato esteso di classificazione: oltre a (intent, confidence) include
/// candidati alternativi, flag di ambiguita' E slot canonici per supportare
/// disambiguazione + routing slot-based (best practice NLU: Rasa/Dialogflow/LUIS).
#[derive(Debug, Clone)]
pub struct ClassifiedIntent {
    pub intent: &'static str,
    pub confidence: f32,
    pub candidates: Vec<IntentCandidate>,
    pub is_ambiguous: bool,
    /// Slot canonici (action_verb, target_type, framework, scope) estratti
    /// dal classifier LLM. Vuoto se il classifier keyword fallback e' stato
    /// usato. Quando `slots.is_complete()` E `slots.confidence >= 0.60`, il
    /// router prova prima la `nexus_routing_slots_matrix` (mig 0133), e
    /// cade sul routing classico (intent, behavior_mode) se non c'e' match.
    pub slots: crate::routing_slots::ActionSlots,
}

/// Soglia di confidence default sotto la quale ignoriamo la classificazione LLM
/// e usiamo il fallback keyword. Override DB: `settings.routing.llm_classifier_min_confidence`
/// caricato in `RoutingThresholds`. Questa costante e' usata solo dal path che
/// non passa per `Orchestrator` (es. test isolati).
const LLM_CLASSIFIER_MIN_CONFIDENCE_DEFAULT: f32 = 0.60;

/// Soglia (default) sopra la quale il classificatore deterministico keyword
/// viene usato come pre-check saltando l'LLM. Override DB:
/// `settings.routing.intent_deterministic_high`. Usata solo quando la cache
/// `RoutingThresholds` non e' disponibile.
const INTENT_DETERMINISTIC_HIGH_DEFAULT: f32 = 0.85;

/// Soglia (default) minima sotto la quale il deterministico NON viene usato
/// nemmeno come fallback quando l'LLM ricade su `chat`. Override DB:
/// `settings.routing.intent_deterministic_min`.
const INTENT_DETERMINISTIC_MIN_DEFAULT: f32 = 0.60;

// ── Telemetria routing (mig 0112) ───────────────────────────────────────────

/// Calcola sha256(message[:1000]) per la telemetria. Non e' PII e ci permette
/// di fare GROUP BY prompt_hash sulla tabella nexus_routing_decisions per
/// vedere prompt ricorrenti / drift del classifier.
fn prompt_hash(message: &str) -> String {
    use sha2::{Digest, Sha256};
    let head: String = message.chars().take(1000).collect();
    let mut hasher = Sha256::new();
    hasher.update(head.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Fire-and-forget INSERT in `nexus_routing_decisions`. Spawna un task tokio
/// per non aggiungere latenza al path caldo. Eventuali errori sono loggati WARN.
#[allow(clippy::too_many_arguments)]
fn spawn_routing_decision_insert(
    db: PgPool,
    message: &str,
    estimated_tokens: i32,
    behavior_mode: &str,
    intent: &str,
    classifier_confidence: f32,
    selected_provider: &str,
    selected_model: &str,
    decision_source: &str,
    rationale: &str,
    no_capable_provider: bool,
    providers_in_cooldown: &[String],
) {
    let p_hash = prompt_hash(message);
    let behavior_mode = behavior_mode.to_string();
    let intent = intent.to_string();
    let selected_provider = selected_provider.to_string();
    let selected_model = selected_model.to_string();
    let decision_source = decision_source.to_string();
    let rationale = rationale.to_string();
    let cooldown: Vec<String> = providers_in_cooldown.to_vec();

    tokio::spawn(async move {
        let res = sqlx::query(
            r#"INSERT INTO nexus_routing_decisions
               (prompt_hash, estimated_tokens, behavior_mode,
                intent, classifier_source, classifier_confidence, classifier_cached,
                selected_provider, selected_model, decision_source, rationale,
                no_capable_provider, providers_in_cooldown, fallback_triggered)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)"#,
        )
        .bind(&p_hash)
        .bind(estimated_tokens)
        .bind(&behavior_mode)
        .bind(&intent)
        // classifier_source: per ora derivato (LLM se confidence > soglia,
        // altrimenti keyword/promotion). Fase 4 separera' i flussi esplicitamente.
        .bind(if classifier_confidence >= 0.85 { "llm" } else { "keyword_or_promotion" })
        .bind(classifier_confidence)
        .bind::<Option<bool>>(None)  // classifier_cached: non noto a questo livello
        .bind(&selected_provider)
        .bind(&selected_model)
        .bind(&decision_source)
        .bind(&rationale)
        .bind(no_capable_provider)
        .bind(&cooldown)
        .bind(no_capable_provider)  // fallback_triggered = no_capable_provider
        .execute(&db)
        .await;
        if let Err(e) = res {
            tracing::warn!("routing telemetry insert failed: {e}");
        }
    });
}

/// Mappa stringa di intent dal classifier LLM al `&'static str` usato dalla
/// matrice di routing. Solo intent ammessi ritornano `Some`; valori sconosciuti
/// fanno `None` cosi' il caller cade sul fallback keyword.
fn intent_str_to_static(intent: &str) -> Option<&'static str> {
    match intent {
        "chat" => Some("chat"),
        "debug" => Some("debug"),
        "fix" => Some("fix"),
        "refactor" => Some("refactor"),
        "test" => Some("test"),
        "docs" => Some("docs"),
        "architecture" => Some("architecture"),
        "file_ops" => Some("file_ops"),
        "system_admin" => Some("system_admin"),
        _ => None,
    }
}

/// Classifier asincrono: prova prima il classifier LLM (gemini-flash via brain
/// REST `/classify-intent-agentic`), poi cade su keyword + promozione agentic.
///
/// **Feature flag**: env var `NEXUS_LLM_CLASSIFIER_ENABLED` (default: `true`).
/// Settarla a `false` disabilita la chiamata HTTP e usa solo le keyword
/// (utile per smoke test o se il brain e' down).
///
/// **Timeout**: 3 secondi per la chiamata HTTP. In caso di timeout/errore, il
/// classifier LLM e' cache-first quindi una richiesta precedente identica
/// risponde in <50ms; ma se la cache e' fredda accettiamo il keyword fallback
/// per non bloccare il routing.
///
/// **Trust criteria**: usiamo il risultato LLM solo se:
///   1. La risposta e' arrivata entro il timeout
///   2. `confidence >= LLM_CLASSIFIER_MIN_CONFIDENCE` (default 0.60)
///   3. `fallback_used == false` (il brain stesso non ha fallato)
///   4. L'intent ritornato e' tra quelli noti alla matrix
async fn classify_intent_async_with_threshold(
    message: &str,
    min_confidence: f32,
    timeout_seconds: f32,
) -> (&'static str, f32) {
    // Priorita': env var override > AtomicBool inizializzato dal DB in main.rs.
    let llm_enabled = match std::env::var("NEXUS_LLM_CLASSIFIER_ENABLED").as_deref() {
        Ok(v) => !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off"),
        Err(_) => LLM_CLASSIFIER_ENABLED.load(Ordering::Relaxed),
    };

    if !llm_enabled || message.trim().is_empty() {
        return classify_intent_with_agentic_promotion(message);
    }

    let brain_url = std::env::var("BRAIN_REST_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
    let url = format!("{}/classify-intent-agentic", brain_url.trim_end_matches('/'));

    // Timeout configurabile via routing.llm_classifier_timeout_seconds (mig 0111).
    // Il classifier Python ha cache TTL 24h, request ripetuta risponde in <50ms.
    let timeout_dur = std::time::Duration::from_millis((timeout_seconds * 1000.0) as u64);
    let http = match reqwest::Client::builder().timeout(timeout_dur).build() {
        Ok(c) => c,
        Err(_) => return classify_intent_with_agentic_promotion(message),
    };

    let body = serde_json::json!({ "message": message });
    let resp = match http.post(&url).json(&body).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::debug!("classifier LLM: HTTP {} — fallback keyword", r.status());
            return classify_intent_with_agentic_promotion(message);
        }
        Err(e) => {
            tracing::debug!("classifier LLM: rete fallita ({e}) — fallback keyword");
            return classify_intent_with_agentic_promotion(message);
        }
    };

    let parsed: AgenticIntentResponse = match resp.json().await {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!("classifier LLM: JSON malformato ({e}) — fallback keyword");
            return classify_intent_with_agentic_promotion(message);
        }
    };

    if parsed.fallback_used || parsed.confidence < min_confidence {
        tracing::debug!(
            "classifier LLM: scarso (fallback={}, conf={}, threshold={}) — uso keyword",
            parsed.fallback_used, parsed.confidence, min_confidence
        );
        return classify_intent_with_agentic_promotion(message);
    }

    let intent_static = match intent_str_to_static(&parsed.intent) {
        Some(s) => s,
        None => {
            tracing::warn!("classifier LLM: intent sconosciuto '{}' — fallback", parsed.intent);
            return classify_intent_with_agentic_promotion(message);
        }
    };

    tracing::info!(
        "classifier LLM: intent={} agentic_score={:.2} confidence={:.2} cached={}",
        intent_static, parsed.agentic_score, parsed.confidence, parsed.cached
    );

    // Up-tier extra basato su agentic_score: se il messaggio e' classificato
    // come "chat" ma l'agentic_score e' alto (>0.7), promuoviamo a system_admin.
    if intent_static == "chat" && parsed.agentic_score > 0.70 && parsed.requires_tools {
        return ("system_admin", parsed.confidence);
    }

    // Promozione → fix_complesso quando il messaggio chiede di risolvere
    // fail di test. La routing matrix `test|*` ha modelli light
    // (deepseek-chat, gpt-4.1-mini) inadeguati per orchestrare multi-file
    // edit + debug. `fix_complesso` mappa a modelli capable.
    // Sia `test` sia `fix` vengono promossi (entrambi sono target sub-tier).
    if (intent_static == "test" || intent_static == "fix")
        && is_test_failure_resolution(message)
    {
        tracing::info!(
            "intent: promozione {} → fix_complesso (test failure resolution detected)",
            intent_static
        );
        return ("fix_complesso", parsed.confidence);
    }

    (intent_static, parsed.confidence)
}

/// Variante "full" che ritorna `ClassifiedIntent` (con candidati e flag
/// ambiguita') invece del solo `(intent, confidence)`.
///
/// Best practice NLU: quando `is_ambiguous=true` il caller deve chiedere
/// disambiguazione all'utente prima di scegliere un provider/modello.
///
/// Stesso flusso di `classify_intent_async_with_threshold` ma propaga
/// i campi aggiuntivi del classifier LLM (`candidates`, `is_ambiguous`).
async fn classify_intent_async_full_with_threshold(
    message: &str,
    min_confidence: f32,
    timeout_seconds: f32,
) -> ClassifiedIntent {
    let llm_enabled = match std::env::var("NEXUS_LLM_CLASSIFIER_ENABLED").as_deref() {
        Ok(v) => !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off"),
        Err(_) => LLM_CLASSIFIER_ENABLED.load(Ordering::Relaxed),
    };

    // Helper: wrap di un (intent, confidence) keyword-based in ClassifiedIntent.
    // Il path keyword non ha candidati alternativi: ne sintetizziamo uno solo.
    // is_ambiguous = (confidence < min_confidence) — best effort: il classifier
    // keyword non ha visione semantica del task, quindi sotto soglia trattiamo
    // la decisione come incerta e l'agente chiedera' chiarimenti.
    let keyword_to_full = |intent: &'static str, conf: f32| -> ClassifiedIntent {
        ClassifiedIntent {
            intent,
            confidence: conf,
            candidates: vec![IntentCandidate {
                intent: intent.to_string(),
                confidence: conf,
            }],
            is_ambiguous: conf < min_confidence,
            // Keyword classifier: nessuno slot semantico estraibile.
            slots: crate::routing_slots::ActionSlots::default(),
        }
    };

    if !llm_enabled || message.trim().is_empty() {
        let (i, c) = classify_intent_with_agentic_promotion(message);
        return keyword_to_full(i, c);
    }

    let brain_url = std::env::var("BRAIN_REST_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
    let url = format!("{}/classify-intent-agentic", brain_url.trim_end_matches('/'));

    let timeout_dur = std::time::Duration::from_millis((timeout_seconds * 1000.0) as u64);
    let http = match reqwest::Client::builder().timeout(timeout_dur).build() {
        Ok(c) => c,
        Err(_) => {
            let (i, c) = classify_intent_with_agentic_promotion(message);
            return keyword_to_full(i, c);
        }
    };

    let body = serde_json::json!({ "message": message });
    let resp = match http.post(&url).json(&body).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => {
            let (i, c) = classify_intent_with_agentic_promotion(message);
            return keyword_to_full(i, c);
        }
    };

    let parsed: AgenticIntentResponse = match resp.json().await {
        Ok(p) => p,
        Err(_) => {
            let (i, c) = classify_intent_with_agentic_promotion(message);
            return keyword_to_full(i, c);
        }
    };

    if parsed.fallback_used || parsed.confidence < min_confidence {
        let (i, c) = classify_intent_with_agentic_promotion(message);
        // Conserviamo i candidati LLM se disponibili (utili per audit anche
        // quando il top intent non passa la soglia di confidence).
        let candidates = if parsed.candidates.is_empty() {
            vec![IntentCandidate { intent: i.to_string(), confidence: c }]
        } else {
            parsed.candidates
        };
        return ClassifiedIntent {
            intent: i,
            confidence: c,
            candidates,
            is_ambiguous: parsed.is_ambiguous || c < min_confidence,
            // Anche sotto-soglia conserviamo gli slot per audit/UI: il
            // routing classico ha priorita' ma il debug rimane visibile.
            slots: parsed.slots,
        };
    }

    let intent_static = match intent_str_to_static(&parsed.intent) {
        Some(s) => s,
        None => {
            let (i, c) = classify_intent_with_agentic_promotion(message);
            return keyword_to_full(i, c);
        }
    };

    // Promozioni euristiche (chat→system_admin, test/fix→fix_complesso).
    // Una volta promosso, il flag is_ambiguous viene RESETTATO perche' la
    // promozione e' deterministica (regola hardcoded, non probabilistica).
    let (final_intent, ambiguous) =
        if intent_static == "chat" && parsed.agentic_score > 0.70 && parsed.requires_tools {
            ("system_admin", false)
        } else if (intent_static == "test" || intent_static == "fix")
            && is_test_failure_resolution(message)
        {
            ("fix_complesso", false)
        } else {
            (intent_static, parsed.is_ambiguous)
        };

    ClassifiedIntent {
        intent: final_intent,
        confidence: parsed.confidence,
        candidates: parsed.candidates,
        is_ambiguous: ambiguous,
        slots: parsed.slots,
    }
}

/// Wrapper che usa i default hardcoded — solo per call site che non hanno
/// accesso a `Orchestrator` (e quindi a `RoutingThresholds`).
/// Il path principale (resolve_agent_provider*, run) usa
/// `Orchestrator::classify_intent_with_db_thresholds`.
async fn classify_intent_async(message: &str) -> (&'static str, f32) {
    classify_intent_async_with_threshold(
        message,
        LLM_CLASSIFIER_MIN_CONFIDENCE_DEFAULT,
        5.0,
    ).await
}

#[derive(Debug)]
struct RoutingDecision {
    provider: String,
    model: String,
    #[allow(dead_code)]
    rationale: &'static str,
}

/// Soglie token per intent_key (route_model_with_mode). Letti da
/// `settings.routing.token_threshold_*` via `RoutingThresholds`.
/// Usato come "view" minimale per non passare l'intera struct.
#[derive(Debug, Clone, Copy)]
struct TokenThresholds {
    chat_breve: u32,
    chat_media: u32,
    complex_fix: u32,
}

impl TokenThresholds {
    /// Default = seed mig 0111 (allineato).
    fn defaults() -> Self {
        Self {
            chat_breve: 400,
            chat_media: 1_500,
            complex_fix: 3_000,
        }
    }

    fn from_routing_thresholds(t: &crate::routing_config::RoutingThresholds) -> Self {
        Self {
            chat_breve: t.token_threshold_chat_breve,
            chat_media: t.token_threshold_chat_media,
            complex_fix: t.token_threshold_complex_fix,
        }
    }
}

/// Risultato dettagliato di [`Orchestrator::resolve_agent_provider_detailed`].
/// Esposto come JSON tramite l'endpoint internal di routing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RoutingResolveResult {
    pub provider: String,
    pub model: String,
    pub intent: String,
    pub mode: String,
    pub risky: bool,
    pub rationale: String,
    /// Fonte della decisione di model. Permette al chiamante (brain Python,
    /// observability, dashboard admin) di capire come e' stato scelto:
    ///   - "matrix"   = decisa dalla `route_model_with_mode` (matrix statica)
    ///   - "catalog"  = decisa dal catalogo prezzi `ai_price_catalog`
    ///                  (modalita' dinamica, ottimizzazione costo/capability)
    ///   - "override" = forzata da `provider_override` utente
    ///   - "cooldown_fallback" = matrix scelta era anthropic ma in cooldown,
    ///                            sostituita con prossimo capable
    pub source: String,
    /// Behavior mode reale a livello DB (puo' differire da `mode` esposto
    /// se il routing applica un override risky o se il dinamico viene
    /// degradato a bilanciata sui task rischiosi).
    pub configured_behavior_mode: String,
    /// True se TUTTI i provider della hierarchy sono in cooldown E il
    /// `provider`/`model` ritornato e' un'ultima istanza non garantita.
    /// Il chiamante DEVE fermarsi e avvertire l'utente: nessuno dei
    /// provider configurati e' al momento utilizzabile (quote esaurite,
    /// rate limit, billing). Continuare comunque produrrebbe lo stesso
    /// errore in loop.
    pub no_capable_provider: bool,
    /// Lista provider in cooldown al momento della decisione, ordinata
    /// come la hierarchy. Permette al frontend di mostrare un alert
    /// dettagliato ("anthropic e openai sono in cooldown — solo deepseek
    /// e' disponibile").
    pub providers_in_cooldown: Vec<String>,
    /// Se valorizzato, il routing NON ha potuto decidere perche' la matrice
    /// DB e' irraggiungibile o non popolata. Il chiamante DEVE fermarsi
    /// (HTTP 503 Service Unavailable) e mostrare il messaggio all'admin.
    /// Niente fallback hardcoded: e' un errore di configurazione, non
    /// un caso da nascondere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct DynamicRoutingDecision {
    provider: String,
    model: String,
    #[allow(dead_code)]
    rationale: &'static str,
}

/// Stima la complessità reale del messaggio ignorando liste/elenchi ripetitivi
/// (es. quality findings con decine di righe identiche).
/// Ritorna il numero di token "significativi" (prime 300 parole uniche).
fn estimate_complexity(message: &str) -> u32 {
    // Conta solo le prime 200 parole per la complessità dell'intent — il resto sono dati
    let core_words = message.split_whitespace().take(200).count() as u32;
    core_words.saturating_mul(2).max(50)
}

/// Sceglie provider e modello in base all'intent, complessità e behavior_mode.
// route_model_local rimossa: era dead_code dopo l'introduzione di route_model_from_catalog
// (refactor 0101 model-registry). Tutti i call site usano route_model_with_mode con la
// matrice DB passata esplicitamente.

/// Seleziona il modello ottimale dal catalogo DB per la modalità richiesta.
/// La modalità "dinamico" sceglie il modello più adatto per capability+tier,
/// privilegiando il costo più basso a parità di tier richiesto.
async fn route_model_from_catalog(
    db: &PgPool,
    base_tier: &str,
    capability: &str,
    mode: &str,
) -> Option<DynamicRoutingDecision> {
    // Promozione/declassamento del tier in base al behavior_mode.
    // "approfondita" scala in alto, "veloce"/"economica" scala in basso.
    // Il `base_tier` arriva gia' risolto dal chiamante via IntentCapabilityMap
    // (mig 0110), che applica le soglie di token threshold per l'intent.
    let required_tier = match mode {
        "approfondita" => match base_tier {
            "light"  => "medium",
            "medium" => "heavy",
            other    => other,
        },
        "veloce" | "economica" => match base_tier {
            "heavy"  => "medium",
            other    => other,
        },
        _ => base_tier,
    };

    // Query al catalogo: trova il modello più economico che soddisfa tier+capability.
    // Per "veloce" ordina per speed_tier, per "economica" per costo, per altri per featured.
    let order_clause = match mode {
        "veloce"    => "CASE speed_tier WHEN 'fast' THEN 0 WHEN 'medium' THEN 1 ELSE 2 END, input_cost_per_million_tokens ASC",
        "economica" => "input_cost_per_million_tokens ASC",
        "approfondita" => "is_featured DESC, input_cost_per_million_tokens DESC",
        _           => "is_featured DESC, input_cost_per_million_tokens ASC",
    };

    // Provider in cooldown sono esclusi dalla selezione: chiamarli produrrebbe
    // un errore (billing/rate limit) e farebbe fallire l'intera richiesta utente.
    // Lista da memoria condivisa popolata da provider_cooldown::put_provider_in_cooldown.
    let cooldown_providers: Vec<String> = crate::provider_cooldown::cooldown_snapshot()
        .into_iter()
        .map(|(name, _, _)| name)
        .collect();

    let query = format!(
        r#"SELECT provider, model FROM ai_price_catalog
           WHERE is_enabled = TRUE
             AND performance_tier = $1
             AND capabilities @> $2::jsonb
             AND supports_tool_use = TRUE
             AND provider <> ALL($3)
           ORDER BY {order_clause}
           LIMIT 1"#
    );

    let capability_json = format!("[\"{capability}\"]");

    let row: Option<(String, String)> = sqlx::query_as(&query)
        .bind(required_tier)
        .bind(&capability_json)
        .bind(&cooldown_providers)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    if let Some((provider, model)) = row {
        return Some(DynamicRoutingDecision {
            provider,
            model,
            rationale: "catalog dynamic routing",
        });
    }

    None
}

/// Seleziona il miglior modello del catalog per un dato `tier`, opzionalmente
/// filtrato per `capability` e `requires_tool_use`. Usato dalla risoluzione
/// tier-based dei purpose (mig 0203): es. il purpose 'planner' -> tier 'heavy'
/// + capability 'reasoning' sceglie dinamicamente il miglior modello heavy
/// disponibile (esclusi i provider in cooldown), il piu' economico tra i
/// featured. Ritorna None se nessun candidato soddisfa i criteri (il chiamante
/// cade sul fallback statico del purpose).
pub async fn best_model_for_tier(
    db: &PgPool,
    tier: &str,
    capability: Option<&str>,
    requires_tool_use: bool,
) -> Option<(String, String)> {
    let cooldown_providers: Vec<String> = crate::provider_cooldown::cooldown_snapshot()
        .into_iter()
        .map(|(name, _, _)| name)
        .collect();

    // Costruzione dinamica dei placeholder: $1 = tier sempre presente.
    // capability (se Some) e cooldown ottengono i numeri successivi in ordine,
    // cosi' la posizione resta sempre coerente coi bind effettivi.
    let mut idx = 1; // $1 = tier
    let capability_json = capability.map(|c| format!("[\"{c}\"]"));
    let capability_predicate = if capability_json.is_some() {
        idx += 1;
        format!("AND capabilities @> ${idx}::jsonb")
    } else {
        String::new()
    };
    let tool_use_predicate = if requires_tool_use {
        "AND supports_tool_use = TRUE"
    } else {
        ""
    };
    idx += 1;
    let cooldown_idx = idx; // ultimo placeholder

    let query = format!(
        r#"SELECT provider, model FROM ai_price_catalog
           WHERE is_enabled = TRUE
             AND performance_tier = $1
             {capability_predicate}
             {tool_use_predicate}
             AND provider <> ALL(${cooldown_idx})
           ORDER BY is_featured DESC, input_cost_per_million_tokens ASC
           LIMIT 1"#
    );

    let mut q = sqlx::query_as::<_, (String, String)>(&query).bind(tier);
    if let Some(cap) = capability_json.as_ref() {
        q = q.bind(cap);
    }
    q = q.bind(&cooldown_providers);

    q.fetch_optional(db).await.ok().flatten()
}

/// Route (intent, behavior_mode) -> (provider, model) consultando la matrice DB
/// (cache 60s in-memory). Sostituisce la matrice hardcoded che era qui prima
/// del refactor 0101 (vedi `crates/mcp-core/src/routing_matrix.rs`).
///
/// Se la matrice non ha entry per (intent, mode), fallback in cascata:
/// 1. Prova lo stesso intent con mode 'bilanciata'
/// 2. Prova default per provider 'anthropic' (tool use solido)
/// 3. Ultima istanza: default per provider 'openai'
fn route_model_with_mode(
    matrix: &crate::routing_matrix::RoutingMatrix,
    intent: &str,
    estimated_tokens: u32,
    mode: &str,
    preferred_provider_for_intent: Option<&str>,
    token_thresholds: &TokenThresholds,
) -> RoutingDecision {
    // Determina intent_key composta usando le soglie da settings.routing.*
    // (mig 0111). I valori default sono replicati in `TokenThresholds::defaults()`.
    let intent_key = match intent {
        "debug" => "debug",
        "architecture" => "architecture",
        "refactor" => "refactor",
        "fix" => {
            if estimated_tokens > token_thresholds.complex_fix { "fix_complesso" }
            else { "fix_semplice" }
        }
        "test" => "test",
        "docs" => "docs",
        "file_ops" => "file_ops",
        "system_admin" => "system_admin",
        _ => {
            if estimated_tokens <= token_thresholds.chat_breve { "chat_breve" }
            else if estimated_tokens <= token_thresholds.chat_media { "chat_media" }
            else { "chat_lunga" }
        }
    };

    // Routing matrix: (intent_key, mode) → (provider, model)
    // Budget-aware lookup: usa `lookup_with_budget` che applica le regole
    // escalation (mig 0120) quando `estimated_tokens >= threshold`. Cosi'
    // task lunghi/complessi prendono automaticamente il modello escalation
    // (es. google: 2.5-pro -> 3.1-pro-preview-customtools sopra soglia).
    // Senza questo (bug 30/05/2026) i campi escalation_* del DB erano popolati
    // ma mai usati — il routing prendeva sempre il modello base.
    let est_i32: i32 = estimated_tokens.try_into().unwrap_or(i32::MAX);

    // Helper: skip provider in cooldown — chiamarli produrrebbe billing/rate-limit
    // error che farebbe fallire l'intera richiesta utente.
    let in_cooldown = |p: &str| crate::provider_cooldown::is_provider_in_cooldown(p);

    // 1. Lookup diretto (intent_key, mode) nella matrice DB con escalation
    if let Some((provider, model)) = matrix.lookup_with_budget(intent_key, mode, est_i32) {
        if !in_cooldown(&provider) {
            return RoutingDecision {
                provider,
                model,
                rationale: "routing_matrix DB",
            };
        }
        tracing::warn!("route_model_with_mode: skip provider {} (in cooldown)", provider);
    }

    // 2. Fallback: prova lo stesso intent con mode 'bilanciata' (budget-aware)
    if mode != "bilanciata" {
        if let Some((provider, model)) = matrix.lookup_with_budget(intent_key, "bilanciata", est_i32) {
            if !in_cooldown(&provider) {
                return RoutingDecision {
                    provider,
                    model,
                    rationale: "routing_matrix DB (mode fallback bilanciata)",
                };
            }
            tracing::warn!("route_model_with_mode: skip provider {} su fallback bilanciata (in cooldown)", provider);
        }
    }

    // 2b. Fallback: cerca QUALSIASI mode per lo stesso intent_key con un provider non in cooldown
    for try_mode in &["bilanciata", "approfondita", "veloce", "economica"] {
        if let Some((provider, model)) = matrix.lookup_with_budget(intent_key, try_mode, est_i32) {
            if !in_cooldown(&provider) {
                return RoutingDecision {
                    provider,
                    model,
                    rationale: "routing_matrix DB (cooldown bypass: any mode)",
                };
            }
        }
    }

    // 3. Fallback: usa il preferred_provider per l'intent passato dal caller
    // (letto dalla cache nexus_intent_capability, mig 0110). Se non specificato
    // o se il provider non ha default model in matrix.default_model, ritorna
    // una sentinella `__no_model__` che il chiamante a monte traduce in
    // RoutingResolveResult { no_capable_provider: true } → HTTP 503.
    if let Some(provider) = preferred_provider_for_intent {
        if let Some(model) = matrix.default_model(provider) {
            return RoutingDecision {
                provider: provider.to_string(),
                model,
                rationale: "routing_matrix default per preferred_provider intent",
            };
        }
    }

    // 4. Nessun match possibile. Niente fallback hardcoded (regola G CLAUDE.md):
    // il chiamante DEVE intercettare questa sentinella e propagare HTTP 503.
    tracing::error!(
        "route_model_with_mode: nessun match per (intent={}, mode={}) e preferred_provider mancante o non in matrix.default_models. \
         Verifica nexus_routing_matrix e nexus_intent_capability.",
        intent_key, mode
    );
    RoutingDecision {
        provider: "__no_model__".to_string(),
        model: "__no_model__".to_string(),
        rationale: "no model available — verifica routing matrix + intent_capability",
    }
}

#[derive(Debug, sqlx::FromRow)]
struct SettingValueRow {
    key: String,
    value: String,
}

#[derive(Debug, Clone)]
#[derive(Default)]
struct RoutingConfig {
    provider_hierarchy: Vec<String>,
    default_provider: Option<String>,
    default_model: Option<String>,
    token_budget: u32,
    max_token_budget: u32,
    provider_models: HashMap<String, String>,
    intent_provider_hierarchy: HashMap<String, Vec<String>>,
    behavior_mode: String,
}

impl RoutingConfig {
    fn from_settings(settings: &[SettingValueRow]) -> Self {
        let mut values = HashMap::new();
        for setting in settings {
            values.insert(setting.key.as_str(), setting.value.trim().to_string());
        }

        let provider_hierarchy = [
            "provider_hierarchy",
            "provider_priority",
            "provider_order",
            "fallback_order",
        ]
        .iter()
        .find_map(|key| parse_provider_list(values.get(key).map(String::as_str)))
        .unwrap_or_else(|| {
            parse_provider_list(values.get("default_provider").map(String::as_str)).unwrap_or_else(
                || {
                    KNOWN_PROVIDERS
                        .iter()
                        .map(|provider| (*provider).to_string())
                        .collect()
                },
            )
        });

        let default_provider = values
            .get("default_provider")
            .map(|value| value.to_lowercase())
            .filter(|value| !value.is_empty());
        let default_model = values
            .get("default_model")
            .cloned()
            .filter(|value| !value.is_empty());

        let token_budget = values
            .get("token_budget")
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(4096);
        let max_token_budget = values
            .get("max_token_budget")
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(token_budget.max(4096));

        let mut provider_models = HashMap::new();
        for provider in KNOWN_PROVIDERS {
            for key in [
                format!("provider_model_{provider}"),
                format!("{provider}_model"),
            ] {
                if let Some(value) = values.get(key.as_str()).filter(|value| !value.is_empty()) {
                    provider_models.insert(provider.to_string(), value.clone());
                    break;
                }
            }
        }

        let mut intent_provider_hierarchy = HashMap::new();
        for intent in KNOWN_INTENTS {
            let keys = [
                format!("routing_{intent}_providers"),
                format!("{intent}_provider_hierarchy"),
                format!("{intent}_providers"),
            ];
            if let Some(providers) = keys
                .iter()
                .find_map(|key| parse_provider_list(values.get(key.as_str()).map(String::as_str)))
            {
                intent_provider_hierarchy.insert(intent.to_string(), providers);
            }
        }

        let behavior_mode = values
            .get("nexus_behavior_mode")
            .cloned()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "bilanciata".to_string());

        Self {
            provider_hierarchy,
            default_provider,
            default_model,
            token_budget,
            max_token_budget,
            provider_models,
            intent_provider_hierarchy,
            behavior_mode,
        }
    }

    fn resolve_token_budget(&self, suggested_budget: Option<u32>) -> u32 {
        let requested = suggested_budget.unwrap_or(self.token_budget).max(1);
        requested.min(self.max_token_budget.max(1))
    }

    fn candidates(&self, intent: &str, suggested_provider: Option<&str>) -> Vec<String> {
        let mut providers = Vec::new();

        if let Some(intent_chain) = self.intent_provider_hierarchy.get(intent) {
            for provider in intent_chain {
                push_unique(&mut providers, provider.clone());
            }
        }

        for provider in &self.provider_hierarchy {
            push_unique(&mut providers, provider.clone());
        }

        if let Some(provider) = self.default_provider.as_ref() {
            push_unique(&mut providers, provider.clone());
        }

        if let Some(provider) = suggested_provider
            .map(str::trim)
            .filter(|provider| !provider.is_empty())
        {
            push_unique(&mut providers, provider.to_lowercase());
        }

        for provider in KNOWN_PROVIDERS {
            push_unique(&mut providers, provider.to_string());
        }

        providers
    }

    fn resolve_model(
        &self,
        matrix: &crate::routing_matrix::RoutingMatrix,
        provider: &str,
        suggested_provider: Option<&str>,
        suggested_model: Option<&str>,
    ) -> String {
        if let Some(model) = self.provider_models.get(provider) {
            return model.clone();
        }

        if self.default_provider.as_deref() == Some(provider) {
            if let Some(model) = self
                .default_model
                .as_ref()
                .filter(|value| !value.is_empty())
            {
                return model.clone();
            }
        }

        if suggested_provider == Some(provider) {
            if let Some(model) = suggested_model.filter(|value| !value.is_empty()) {
                return model.to_string();
            }
        }

        default_model_for_provider(matrix, provider)
    }
}

#[derive(Debug, Clone)]
pub struct ChatAttachment {
    /// UUID dell'allegato in `chat_message_attachments`. Popolato dopo
    /// `persist_message_attachments` per consentire al prompt iniziale di
    /// stampare un suggerimento `nexus_inspect_attachment(attachment_id=...)`.
    /// `None` quando l'allegato non e' ancora stato persistito (caso non
    /// raggiunto in produzione, mantenuto Option per backward compat).
    pub id: Option<Uuid>,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub text_content: String,
    pub base64_content: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutomationMode {
    Study,
    Confirm,
    Automatic,
}

impl AutomationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Study => "study",
            Self::Confirm => "confirm",
            Self::Automatic => "automatic",
        }
    }

    /// Restituisce la chiave DB del template per le istruzioni di modalità.
    fn prompt_instruction_template_key(self) -> &'static str {
        match self {
            Self::Study    => "automation.mode_study_instruction",
            Self::Confirm  => "automation.mode_confirm_instruction",
            Self::Automatic => "automation.mode_automatic_instruction",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OrchestratorRequest {
    pub user_id: String,
    pub project_id: String,
    pub profile_id: String,
    pub message: String,
    pub active_files: Vec<String>,
    pub session_id: Option<String>,
    pub request_message_id: Option<String>,
    pub provider_override: Option<String>,
    pub model_override: Option<String>,
    pub automation_mode: AutomationMode,
    pub attachments: Vec<ChatAttachment>,
}

#[derive(Debug, Clone)]
pub struct OrchestratorResult {
    pub payload: Value,
    #[allow(dead_code)]
    pub audit: OrchestratorAudit,
}

#[derive(Clone)]
pub struct NeuralCoreClient {
    client: NeuralCoreServiceClient<tonic::transport::Channel>,
    brain_http_url: String,
}

impl std::fmt::Debug for NeuralCoreClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NeuralCoreClient").finish_non_exhaustive()
    }
}

impl NeuralCoreClient {
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        // Default tonic decoding limit è 4MB: non basta per prompt grandi.
        // Allineato al server Python (128MB) in neural_service.py::serve.
        const MAX_MSG: usize = 128 * 1024 * 1024;
        let client = NeuralCoreServiceClient::connect(url.to_string())
            .await?
            .max_decoding_message_size(MAX_MSG)
            .max_encoding_message_size(MAX_MSG);
        tracing::info!("Connected to Neural Core at {} (max msg 128MB)", url);
        let brain_http_url = std::env::var("BRAIN_HTTP_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
        Ok(Self { client, brain_http_url })
    }

    #[allow(dead_code)]
    pub async fn classify_intent(
        &self,
        project_id: &str,
        profile_id: &str,
        message: &str,
    ) -> anyhow::Result<Value> {
        let mut client = self.client.clone();
        let resp = client
            .classify_intent(ClassifyIntentRequest {
                project_id: project_id.to_string(),
                profile_id: profile_id.to_string(),
                message: message.to_string(),
            })
            .await?;
        let json: Value = serde_json::from_str(&resp.into_inner().json)?;
        Ok(json)
    }

    #[allow(dead_code)]
    pub async fn route_model(
        &self,
        project_id: &str,
        profile_id: &str,
        intent: &str,
        token_budget: u32,
    ) -> anyhow::Result<Value> {
        let mut client = self.client.clone();
        let resp = client
            .route_model(RouteModelRequest {
                project_id: project_id.to_string(),
                profile_id: profile_id.to_string(),
                intent: intent.to_string(),
                token_budget,
            })
            .await?;
        let json: Value = serde_json::from_str(&resp.into_inner().json)?;
        Ok(json)
    }

    pub async fn embed_text(&self, model: &str, text: &str) -> anyhow::Result<Vec<f32>> {
        let mut client = self.client.clone();
        let resp = client
            .embed_text(EmbedTextRequest {
                model: model.to_string(),
                text: text.to_string(),
            })
            .await?;
        let json: Value = serde_json::from_str(&resp.into_inner().json)?;
        let vector = json
            .get("vector")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("invalid_embed_response"))?
            .iter()
            .filter_map(|value| value.as_f64().map(|num| num as f32))
            .collect::<Vec<_>>();
        if vector.is_empty() {
            anyhow::bail!("empty_embed_vector");
        }
        Ok(vector)
    }

    pub async fn generate_completion(
        &self,
        provider: &str,
        model: &str,
        prompt: &str,
    ) -> anyhow::Result<Value> {
        let mut client = self.client.clone();
        let resp = client
            .generate_completion(GenerateCompletionRequest {
                provider: provider.to_string(),
                model: model.to_string(),
                prompt: prompt.to_string(),
            })
            .await?;
        let json: Value = serde_json::from_str(&resp.into_inner().json)?;
        Ok(json)
    }

    pub async fn generate_agent_turn(
        &self,
        provider: &str,
        model: &str,
        messages_json: &str,
        tools_json: &str,
        max_tokens: u32,
        system_text: &str,
    ) -> anyhow::Result<Value> {
        let mut client = self.client.clone();
        let resp = client
            .generate_agent_turn(GenerateAgentTurnRequest {
                provider: provider.to_string(),
                model: model.to_string(),
                messages_json: messages_json.to_string(),
                tools_json: tools_json.to_string(),
                max_tokens,
                system_text: system_text.to_string(),
            })
            .await?;
        let json: Value = serde_json::from_str(&resp.into_inner().json)?;
        Ok(json)
    }

    /// Streaming di un turno agente via HTTP SSE: chiama il brain su /agent-turn/stream
    /// e invoca `on_token` per ogni token parziale ricevuto.
    /// Ritorna il risultato completo (stesso schema JSON di generate_agent_turn) al termine.
    pub async fn generate_agent_turn_stream(
        &self,
        provider: &str,
        model: &str,
        messages_json: &str,
        tools_json: &str,
        max_tokens: u32,
        system_text: &str,
        on_token: impl Fn(String) + Send,
    ) -> anyhow::Result<Value> {
        let url = format!("{}/agent-turn/stream", self.brain_http_url);
        let body = serde_json::json!({
            "provider": provider,
            "model": model,
            "messages_json": messages_json,
            "tools_json": tools_json,
            "max_tokens": max_tokens,
            "system_text": system_text,
        });
        let http_result = reqwest::Client::new()
            .post(&url)
            .json(&body)
            .send()
            .await;
        let mut resp = match http_result {
            Ok(r) if r.status().is_success() => r,
            Ok(_) | Err(_) => {
                // Provider non supporta streaming HTTP — fallback a gRPC senza token delta
                tracing::debug!(
                    "brain HTTP stream non disponibile per provider={}, fallback a gRPC",
                    provider
                );
                return self
                    .generate_agent_turn(provider, model, messages_json, tools_json, max_tokens, system_text)
                    .await;
            }
        };

        let mut fallback_required = false;

        let mut result: Option<Value> = None;
        let mut buf = String::new();
        while let Some(chunk) = resp.chunk().await? {
            buf.push_str(&String::from_utf8_lossy(&chunk));
            loop {
                if let Some(pos) = buf.find("\n\n") {
                    let line = buf[..pos].to_string();
                    buf = buf[pos + 2..].to_string();
                    if let Some(data) = line.strip_prefix("data: ") {
                        if let Ok(json) = serde_json::from_str::<Value>(data) {
                            match json.get("type").and_then(Value::as_str) {
                                Some("token") => {
                                    if let Some(delta) = json.get("delta").and_then(Value::as_str) {
                                        on_token(delta.to_string());
                                    }
                                }
                                Some("done") => {
                                    result = json.get("result").cloned();
                                }
                                Some("error") => {
                                    let msg = json
                                        .get("message")
                                        .and_then(Value::as_str)
                                        .unwrap_or("errore sconosciuto");
                                    if msg.contains("non supporta lo streaming") {
                                        // Provider non supporta streaming — fallback a gRPC
                                        tracing::debug!("provider {} non supporta streaming, fallback a gRPC", provider);
                                        fallback_required = true;
                                        break;
                                    }
                                    // Estrai retry_after dal metadata (header Retry-After) per
                                    // cooldown dinamico nel loop agente. Lo incastoniamo come
                                    // marcatore `[retry_after=N]` nel messaggio.
                                    let retry_after = json
                                        .get("metadata")
                                        .and_then(|m| m.get("retry_after_seconds"))
                                        .and_then(Value::as_u64);
                                    let error_class = json
                                        .get("metadata")
                                        .and_then(|m| m.get("error_class"))
                                        .and_then(Value::as_str)
                                        .unwrap_or("");
                                    let marker_retry = retry_after
                                        .map(|n| format!(" [retry_after={}]", n))
                                        .unwrap_or_default();
                                    let marker_class = if !error_class.is_empty() {
                                        format!(" [error_class={}]", error_class)
                                    } else {
                                        String::new()
                                    };
                                    anyhow::bail!(
                                        "brain stream error: {}{}{}",
                                        msg, marker_retry, marker_class
                                    );
                                }
                                _ => {}
                            }
                        }
                    }
                } else {
                    break;
                }
            }
        }

        if fallback_required {
            return self
                .generate_agent_turn(provider, model, messages_json, tools_json, max_tokens, system_text)
                .await;
        }

        result.ok_or_else(|| anyhow::anyhow!("nessun risultato dal brain stream"))
    }

    pub async fn provider_health(&self, provider: &str) -> anyhow::Result<Value> {
        let mut client = self.client.clone();
        let resp = client
            .get_provider_health(mcp_proto::neural::ProviderHealthRequest {
                provider: provider.to_string(),
            })
            .await?;
        let json: Value = serde_json::from_str(&resp.into_inner().json)?;
        Ok(json)
    }

    pub async fn is_healthy(&self) -> bool {
        self.provider_health("system").await.is_ok()
    }

    /// Classificazione errori provider via il PUNTO UNICO (brain
    /// error_handler.classify_error, RPC ClassifyError). mcp-core NON classifica
    /// in proprio: passa il testo dell'errore e riceve l'error_class canonico
    /// (billing_error, auth_error, rate_limit, context_too_long, not_found, ...).
    /// Fallback "error" solo se il brain e' irraggiungibile (nessuna logica di
    /// pattern duplicata lato Rust).
    pub async fn classify_error(&self, error_text: &str, provider: &str) -> String {
        let mut client = self.client.clone();
        let req = mcp_proto::neural::ClassifyErrorRequest {
            error_text: error_text.to_string(),
            provider: provider.to_string(),
        };
        match client.classify_error(req).await {
            Ok(resp) => serde_json::from_str::<Value>(&resp.into_inner().json)
                .ok()
                .and_then(|j| j.get("error_class").and_then(|v| v.as_str()).map(String::from))
                .unwrap_or_else(|| "error".to_string()),
            Err(e) => {
                tracing::debug!("classify_error gRPC fallito: {e}");
                "error".to_string()
            }
        }
    }

    pub async fn generate_document(
        &self,
        doc_type: &str,
        content_json: &str,
        output_path: &str,
        standard: &str,
        title: &str,
        project_name: &str,
    ) -> anyhow::Result<(String, i32, i32)> {
        let mut client = self.client.clone();
        let resp = client
            .generate_document(mcp_proto::neural::GenerateDocumentRequest {
                doc_type: doc_type.to_string(),
                content_json: content_json.to_string(),
                output_path: output_path.to_string(),
                standard: standard.to_string(),
                title: title.to_string(),
                project_name: project_name.to_string(),
            })
            .await?;
        let inner = resp.into_inner();
        if !inner.error.is_empty() {
            anyhow::bail!("Document generation error: {}", inner.error);
        }
        Ok((inner.file_path, inner.page_count, inner.section_count))
    }
}

#[derive(Clone)]
pub struct Orchestrator {
    pub(crate) neural: NeuralCoreClient,
    pub(crate) template_cache: crate::prompt_templates::TemplateCache,
    pub(crate) nexus_gateway: Option<NexusGatewayClient>,
    /// Cache della matrice di routing letta da DB (nexus_routing_matrix).
    /// Refresh background ogni 60s. Sostituisce i model name hardcoded
    /// che erano sparsi in `route_model_with_mode` e `default_model_for_provider`.
    /// Inizializzata in main.rs e clonata qui (la cache interna e' Arc<RwLock<...>>).
    pub(crate) routing_matrix: crate::routing_matrix::RoutingMatrixCache,
    /// Cache parametri routing (settings.routing.*) — mig 0111. Refresh 60s.
    pub(crate) routing_thresholds: crate::routing_config::RoutingThresholdsCache,
    /// Cache mapping intent -> tier/capability/preferred_provider — mig 0110.
    pub(crate) intent_capability: crate::routing_config::IntentCapabilityCache,
    /// Cache della matrice slot-based (mig 0133, Livello 4 NLU).
    /// Lookup gerarchico (action_verb, target_type, framework, scope) →
    /// (provider, model). Piu' precisa di (intent, behavior_mode); il
    /// router la prova per prima e cade su routing classico se no-match.
    pub(crate) slots_matrix: crate::routing_slots::SlotsRoutingMatrixCache,
}

impl Orchestrator {
    pub fn new(
        neural: NeuralCoreClient,
        template_cache: crate::prompt_templates::TemplateCache,
        routing_matrix: crate::routing_matrix::RoutingMatrixCache,
        routing_thresholds: crate::routing_config::RoutingThresholdsCache,
        intent_capability: crate::routing_config::IntentCapabilityCache,
        slots_matrix: crate::routing_slots::SlotsRoutingMatrixCache,
    ) -> Self {
        Self {
            neural,
            template_cache,
            nexus_gateway: None,
            routing_matrix,
            routing_thresholds,
            intent_capability,
            slots_matrix,
        }
    }

    pub fn with_gateway(mut self, gw: NexusGatewayClient) -> Self {
        self.nexus_gateway = Some(gw);
        self
    }

    pub async fn neural_healthy(&self) -> bool {
        self.neural.is_healthy().await
    }

    /// Classifier intent che usa le soglie da DB (mig 0111). Sostituisce le
    /// chiamate a `classify_intent_async(message)` nei call site di routing.
    /// Se la cache `routing_thresholds` non e' disponibile, fallback ai default.
    async fn classify_intent_with_db_thresholds(
        &self,
        message: &str,
    ) -> (&'static str, f32) {
        let (min_conf, timeout_s) = match self.routing_thresholds.current_async().await {
            Ok(t) => (t.llm_classifier_min_confidence, t.llm_classifier_timeout_seconds),
            Err(_) => (LLM_CLASSIFIER_MIN_CONFIDENCE_DEFAULT, 5.0),
        };
        classify_intent_async_with_threshold(message, min_conf, timeout_s).await
    }

    /// Variante "full" che ritorna `ClassifiedIntent` con candidati + flag
    /// ambiguita'. Usata da `spawn_agent_run` per decidere se chiedere
    /// disambiguazione all'utente (best practice NLU).
    pub async fn classify_intent_full(&self, message: &str) -> ClassifiedIntent {
        let (min_conf, timeout_s, det_high, det_min) =
            match self.routing_thresholds.current_async().await {
                Ok(t) => (
                    t.llm_classifier_min_confidence,
                    t.llm_classifier_timeout_seconds,
                    t.intent_deterministic_high,
                    t.intent_deterministic_min,
                ),
                Err(_) => (
                    LLM_CLASSIFIER_MIN_CONFIDENCE_DEFAULT,
                    5.0,
                    INTENT_DETERMINISTIC_HIGH_DEFAULT,
                    INTENT_DETERMINISTIC_MIN_DEFAULT,
                ),
            };

        // Classificatore deterministico (keyword/pattern). Calcolato una volta
        // e riusato sia come pre-check sia come fallback se l'LLM fallisce.
        let deterministic = deterministic_intent_fallback(message);

        // (a) PRE-CHECK: se il deterministico e' confidente >= soglia alta,
        // saltiamo del tutto l'LLM. E' piu' veloce e, soprattutto, robusto:
        // un task agentico evidente ("Crea l'applicazione descritta nel file")
        // viene instradato al path agent anche se il classifier LLM e' down.
        if let Some((det_intent, det_conf)) = deterministic {
            if det_conf >= det_high {
                tracing::info!(
                    "classify_intent: deterministic match intent={} conf={:.2} (pre-check, LLM saltato)",
                    det_intent, det_conf
                );
                return ClassifiedIntent {
                    intent: det_intent,
                    confidence: det_conf,
                    candidates: vec![IntentCandidate {
                        intent: det_intent.to_string(),
                        confidence: det_conf,
                    }],
                    is_ambiguous: false,
                    slots: crate::routing_slots::ActionSlots::default(),
                };
            }
        }

        // (b) Path normale: prova l'LLM. Se ritorna un risultato valido lo usa.
        let llm_result =
            classify_intent_async_full_with_threshold(message, min_conf, timeout_s).await;

        // L'LLM e' considerato "non utile" quando ricade su `chat` con
        // confidence sotto la soglia minima del deterministico: in quel caso
        // un eventuale match deterministico (anche a confidence media) e' piu'
        // affidabile per non perdere il path agent. Questo copre sia il caso
        // di vero fallimento HTTP (gia' degradato a keyword internamente) sia
        // il caso di LLM che risponde "chat" su un task chiaramente agentico.
        let llm_degraded_to_chat = llm_result.intent == "chat";
        if llm_degraded_to_chat {
            if let Some((det_intent, det_conf)) = deterministic {
                if det_intent != "chat" && det_conf >= det_min {
                    tracing::warn!(
                        "classify_intent: LLM ha prodotto chat (conf={:.2}), uso fallback deterministico intent={} conf={:.2}",
                        llm_result.confidence, det_intent, det_conf
                    );
                    return ClassifiedIntent {
                        intent: det_intent,
                        confidence: det_conf,
                        candidates: vec![IntentCandidate {
                            intent: det_intent.to_string(),
                            confidence: det_conf,
                        }],
                        is_ambiguous: false,
                        slots: crate::routing_slots::ActionSlots::default(),
                    };
                }
            }
        }

        llm_result
    }

    /// Routing slot-first (Livello 4 NLU): se il classifier ha estratto slot
    /// validi con confidence sufficiente, tenta lookup nella `nexus_routing_slots_matrix`
    /// (mig 0133). In caso di no-match o slot incompleti, ritorna `None` e il
    /// caller fa fallback al routing classico `(intent, behavior_mode)`.
    ///
    /// `min_slot_confidence`: soglia sopra la quale fidarsi degli slot.
    /// Tipicamente 0.60 — sotto questa soglia il classifier "non e' sicuro"
    /// di action_verb/scope e meglio cadere sul routing classico testato.
    ///
    /// Ritorna `Some((provider, model, rationale))` dove rationale spiega
    /// la decisione (utile per audit telemetria + UI debug).
    pub async fn route_by_slots(
        &self,
        slots: &crate::routing_slots::ActionSlots,
        min_slot_confidence: f32,
    ) -> Option<(String, String, &'static str)> {
        if !slots.is_complete() {
            return None;
        }
        if !slots.meets_confidence(min_slot_confidence) {
            tracing::debug!(
                "route_by_slots: confidence {:.2} < soglia {:.2}, fallback intent classico",
                slots.confidence, min_slot_confidence
            );
            return None;
        }
        let matrix = self.slots_matrix.current_async().await?;
        // Cooldown-awareness: scorri la chain di candidati (priority DESC) e
        // ritorna il primo provider NON in cooldown. Se tutti i provider della
        // chain matrice slots sono in cooldown, ritorna None → fallback al
        // routing classico (che ha la propria cooldown chain).
        let chain = matrix.lookup_chain(slots);
        if chain.is_empty() {
            tracing::debug!(
                "route_by_slots: nessun match per ({}, {}, {}, {}) in matrix",
                slots.action_verb, slots.target_type, slots.framework, slots.scope,
            );
            return None;
        }
        let mut skipped: Vec<String> = Vec::new();
        for (provider, model) in &chain {
            if crate::provider_cooldown::is_provider_in_cooldown(provider) {
                skipped.push(provider.clone());
                continue;
            }
            if !skipped.is_empty() {
                tracing::info!(
                    "route_by_slots: skip provider in cooldown [{}], scelto {}/{} (chain pos {}/{})",
                    skipped.join(","), provider, model,
                    skipped.len() + 1, chain.len(),
                );
            } else {
                tracing::info!(
                    "route_by_slots: slots=({}, {}, {}, {}) → {}/{}",
                    slots.action_verb, slots.target_type, slots.framework, slots.scope,
                    provider, model,
                );
            }
            return Some((provider.clone(), model.clone(), "slots_matrix"));
        }
        // Tutti i provider della chain matrice sono in cooldown.
        tracing::warn!(
            "route_by_slots: TUTTI i {} provider della chain in cooldown [{}], fallback intent classico",
            chain.len(), skipped.join(",")
        );
        None
    }

    /// Helper unico: estrae preferred_provider per intent (da nexus_intent_capability,
    /// mig 0110) + TokenThresholds (da settings.routing.*, mig 0111). Usato dai
    /// call site di `route_model_with_mode` per evitare di duplicare il pattern
    /// "leggi cache → estrai → passa".
    async fn routing_helpers_for(
        &self,
        intent: &str,
    ) -> (Option<String>, TokenThresholds) {
        let preferred = match self.intent_capability.current_async().await {
            Ok(map) => map.preferred_provider_for(intent).map(String::from),
            Err(_) => None,
        };
        let thresholds = match self.routing_thresholds.current_async().await {
            Ok(t) => TokenThresholds::from_routing_thresholds(&t),
            Err(_) => TokenThresholds::defaults(),
        };
        (preferred, thresholds)
    }

    pub async fn embed_text(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.neural.embed_text("", text).await
    }

    /// Versione "detailed" di [`resolve_agent_provider`] che restituisce
    /// anche l'intent classificato, la modalita' effettiva e una stringa
    /// di rationale leggibile. Esposta tramite `/api/internal/routing/decide`
    /// in modo che il brain Python possa consultare il routing Rust senza
    /// duplicare la matrice in `service.py::_ROUTING_MATRIX`.
    pub async fn resolve_agent_provider_detailed(
        &self,
        db: &PgPool,
        _project_id: &str,
        _profile_id: &str,
        message: &str,
        provider_override: Option<&str>,
        model_override: Option<&str>,
        context_message_count: usize,
        // Modalita' scelta per la singola sessione (es. dal dropdown chat).
        // Se `Some`, sovrascrive `nexus_behavior_mode` DB solo per questa chiamata.
        behavior_mode_session: Option<&str>,
    ) -> RoutingResolveResult {
        // Snapshot della routing matrix DB (cache 60s, lock-free clone Arc).
        // Se la matrice non e' caricata (DB down all'avvio), ritorniamo
        // immediatamente un risultato di errore — niente fallback hardcoded.
        let matrix_arc = match self.routing_matrix.current() {
            Ok(m) => m,
            Err(e) => {
                return RoutingResolveResult {
                    provider: String::new(),
                    model: String::new(),
                    intent: "unknown".to_string(),
                    mode: "unknown".to_string(),
                    risky: false,
                    rationale: format!("routing_matrix non disponibile: {e}"),
                    source: "error".to_string(),
                    configured_behavior_mode: "unknown".to_string(),
                    no_capable_provider: true,
                    providers_in_cooldown: vec![],
                    error: Some(format!(
                        "Configurazione routing mancante: {e}. \
                         Applica le migrazioni 0101 e 0102 e popola le tabelle \
                         nexus_routing_matrix / nexus_provider_default_model / nexus_purpose_model."
                    )),
                };
            }
        };
        let matrix = &*matrix_arc;
        // Risolve il behavior_mode effettivo: sessione > DB globale.
        // Caricato prima di resolve_agent_provider per passarlo coerentemente.
        let routing_for_mode = Self::load_routing_config(db).await.unwrap_or_default();
        let configured_behavior_mode = behavior_mode_session
            .filter(|v| !v.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| routing_for_mode.behavior_mode.clone());

        let (provider, model) = self
            .resolve_agent_provider(db, _project_id, _profile_id, message, provider_override, model_override, context_message_count, Some(&configured_behavior_mode))
            .await;
        // Riclassifica via classifier LLM (gemini-flash, cache 24h) con fallback
        // keyword + promozione agentic. Vedi `classify_intent_async`.
        let (intent, confidence) = self.classify_intent_with_db_thresholds(message).await;
        let risky = is_risky_task(message);
        let effective_mode = if risky && configured_behavior_mode != "approfondita" {
            "approfondita".to_string()
        } else if configured_behavior_mode == "dinamico" {
            "bilanciata".to_string()
        } else {
            configured_behavior_mode.clone()
        };

        // Deduci la sorgente della decisione confrontando con i percorsi noti.
        // Non e' una telemetria diretta del flusso (richiederebbe restituire
        // il dato dalla resolve_agent_provider) ma una ricostruzione coerente:
        //   - se l'utente ha forzato un override, source = "override"
        //   - se behavior_mode e' "dinamico" e il task NON e' rischioso e il
        //     model non corrisponde a quello della matrix statica, source = "catalog"
        //   - altrimenti source = "matrix"
        let source: &'static str = if provider_override
            .filter(|v| !v.trim().is_empty()).is_some()
        {
            "override"
        } else if configured_behavior_mode == "dinamico" && !risky {
            // In modalita' dinamica non rischiosa il catalogo prezzi e' autoritativo.
            // Verifichiamo: se il modello scelto NON e' quello della matrix per
            // (intent, "bilanciata"), allora il catalogo lo ha sovrascritto.
            let (pref, thr) = self.routing_helpers_for(intent).await;
            let matrix_default = route_model_with_mode(matrix, intent, 1500, "bilanciata", pref.as_deref(), &thr);
            if matrix_default.model != model {
                "catalog"
            } else {
                "matrix"
            }
        } else {
            "matrix"
        };

        // Calcola lista provider in cooldown: ci serve sia per il flag
        // `no_capable_provider` sia per mostrare al frontend un alert
        // "questi provider non sono disponibili" piuttosto che far girare
        // a vuoto le richieste.
        let known_providers = ["anthropic", "openai", "deepseek", "google", "mistral"];
        let providers_in_cooldown: Vec<String> = known_providers
            .iter()
            .filter(|p| is_provider_in_cooldown(p))
            .map(|p| p.to_string())
            .collect();
        // Nessun provider capable = il provider scelto e' lui stesso in
        // cooldown (succede quando tutti gli altri della hierarchy sono
        // anch'essi in cooldown e l'algoritmo riusa l'originale).
        let no_capable_provider = is_provider_in_cooldown(&provider)
            || providers_in_cooldown.len() >= known_providers.len();

        let cooldown_note = if no_capable_provider {
            " ⚠ NESSUN PROVIDER DISPONIBILE — fermarsi e avvertire utente"
        } else if !providers_in_cooldown.is_empty() {
            // Indica all'utente quali provider non sono al momento usabili.
            // Es. "[cooldown:anthropic,openai]"
            ""
        } else {
            ""
        };

        let rationale = format!(
            "intent={} confidence={:.2} mode={}{} source={} → {}/{}{}{}",
            intent, confidence, effective_mode,
            if risky { " [risky→approfondita]" } else { "" },
            source, provider, model,
            if !providers_in_cooldown.is_empty() {
                format!(" [cooldown:{}]", providers_in_cooldown.join(","))
            } else { String::new() },
            cooldown_note,
        );

        // Telemetria fire-and-forget: INSERT in nexus_routing_decisions (mig 0112).
        // Non blocchiamo il path caldo. Errore di insert -> WARN log, decisione
        // di routing comunque restituita.
        // Stima token a partire dal message — necessaria per la telemetria
        // (campo estimated_tokens). resolve_agent_provider_detailed non la
        // calcola altrove: la stima e' veloce (count parole * 2).
        let est_tokens = estimate_complexity(message) as i32;
        spawn_routing_decision_insert(
            db.clone(),
            message,
            est_tokens,
            &configured_behavior_mode,
            intent,
            confidence,
            &provider,
            &model,
            source,
            &rationale,
            no_capable_provider,
            &providers_in_cooldown,
        );

        RoutingResolveResult {
            provider, model,
            intent: intent.to_string(),
            mode: effective_mode,
            risky,
            rationale,
            source: source.to_string(),
            configured_behavior_mode,
            no_capable_provider,
            providers_in_cooldown,
            error: None,
        }
    }

    /// Risolve il provider/model ottimale per l'agente basandosi sull'intent del messaggio.
    /// Replica la logica di routing della chat normale: classify_intent → route_model → candidates.
    /// Fallback a (default_provider, default_model) se Neural Core non è disponibile.
    pub async fn resolve_agent_provider(
        &self,
        db: &PgPool,
        _project_id: &str,
        _profile_id: &str,
        message: &str,
        provider_override: Option<&str>,
        model_override: Option<&str>,
        context_message_count: usize,
        // Override del behavior_mode per questa singola chiamata (sessione utente).
        // Se `Some`, sostituisce `routing.behavior_mode` letto dal DB.
        behavior_mode_override: Option<&str>,
    ) -> (String, String) {
        // Snapshot della routing matrix DB (cache 60s, await sul lock se busy).
        // Se la matrice non e' caricata (caso impossibile dopo init() che ha
        // retry-loop + panic), ritorniamo placeholder vuoti — il caller
        // resolve_agent_provider_detailed gia' gestisce questo errore prima.
        let matrix_arc = match self.routing_matrix.current_async().await {
            Ok(m) => m,
            Err(e) => {
                tracing::error!("resolve_agent_provider: matrix non disponibile: {e}");
                return (String::new(), String::new());
            }
        };
        let matrix = &*matrix_arc;
        // Se l'utente ha forzato un provider specifico, lo rispettiamo.
        // Se ha forzato anche il modello, lo usiamo direttamente senza
        // passare per resolve_model (che applicherebbe override admin).
        if let Some(p) = provider_override.filter(|v| !v.trim().is_empty()) {
            if let Some(m) = model_override.filter(|v| !v.trim().is_empty()) {
                return (p.to_string(), m.to_string());
            }
            let routing = Self::load_routing_config(db).await.unwrap_or_default();
            let model = routing.resolve_model(matrix, p, Some(p), model_override);
            return (p.to_string(), model);
        }

        // Routing locale — zero latenza gRPC
        // estimate_complexity usa solo le prime 200 parole per non farsi ingannare da liste dati
        let base_estimated = estimate_complexity(message);
        // Se la sessione ha già molti messaggi (continuazione di task lungo),
        // alza la stima per evitare di assegnare modelli troppo piccoli.
        // Ogni 10 messaggi = +1000 token equivalenti (cap a 6000).
        let context_bonus = ((context_message_count / 10) as u32 * 1_000).min(6_000);
        let estimated_tokens = base_estimated.saturating_add(context_bonus);
        let (intent, _confidence) = self.classify_intent_with_db_thresholds(message).await;
        // La RoutingConfig admin può sovrascrivere il modello per provider.
        // Il behavior_mode effettivo: override sessione > DB globale.
        let routing = Self::load_routing_config(db).await.unwrap_or_default();
        let effective_behavior_mode: String = behavior_mode_override
            .filter(|v| !v.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| routing.behavior_mode.clone());
        // Task rischioso ha PRIORITA' assoluta: salta il ramo dinamico (catalogo
        // prezzi sceglie modelli "light" per costo, ma per task distruttivi
        // serve un modello capable). L'override mode -> approfondita applicato
        // PRIMA del ramo dinamico forza la matrix statica.
        let risky_pre = is_risky_task(message);
        // Se behavior_mode == "dinamico" E il task NON e' rischioso, consulta il
        // catalogo prezzi (come fa Orchestrator::run).
        // Altrimenti usa la matrix statica route_model_with_mode.
        // Estratte come String per uniformare i due rami (catalogo restituisce String,
        // matrix restituisce &'static str).
        // Caso speciale "dinamico": il catalogo prezzi è autoritativo.
        // Saltiamo candidates() e provider_models perché altrimenti riordinano sempre
        // sui provider configurati nell'admin (anthropic/openai prima) e applicano
        // il provider_model_<x> override → risultato: il dinamico non sceglie mai nulla.
        if effective_behavior_mode == "dinamico" && !risky_pre {
            // Risolvi tier/capability dalla cache intent_capability (mig 0110)
            // invece dal match Rust statico (rimosso). Se intent non mappato,
            // default light/chat (caso tipico di intent legacy non in seed).
            let icap_arc = self.intent_capability.current_async().await.ok();
            let (base_tier, capability) = match icap_arc.as_deref() {
                Some(map) => match map.get(intent) {
                    Some(c) => (
                        c.tier_for_tokens(estimated_tokens),
                        c.base_capability.clone(),
                    ),
                    None => ("light".to_string(), "chat".to_string()),
                },
                None => {
                    tracing::warn!("intent_capability cache non disponibile, uso defaults light/chat");
                    ("light".to_string(), "chat".to_string())
                }
            };
            if let Some(d) = route_model_from_catalog(db, &base_tier, &capability, "dinamico").await {
                let provider = d.provider;
                if !is_provider_in_cooldown(&provider) {
                let model = model_override
                    .filter(|v| !v.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or(d.model);
                tracing::info!(
                    "Agent routing (dinamico/catalog): intent={} tokens~{} → {}/{}",
                    intent, estimated_tokens, provider, model
                );
                return (provider, model);
                } else {
                    tracing::warn!("Agent routing: '{}' in cooldown (catalog/dinamico), skip", provider);
                }
            }
            // Catalogo vuoto → cade nel ramo statico bilanciata sotto
        }

        // Override "task rischioso": se il messaggio contiene verbi distruttivi
        // (rm -rf, drop table, docker prune, force push, ecc.), promuoviamo
        // automaticamente il behavior_mode a "approfondita". Motivazione:
        // i modelli leggeri (mistral-small, gpt-4.1-nano) tendono a interpretare
        // liberamente le richieste distruttive (es. "elimina file Docker" ->
        // ricrea i file). Per task ad alto impatto serve un modello capable.
        let effective_mode = if is_risky_task(message) && effective_behavior_mode != "approfondita" {
            tracing::info!(
                "Agent routing: task rischioso rilevato (mode {} -> approfondita)",
                effective_behavior_mode
            );
            "approfondita"
        } else if effective_behavior_mode == "dinamico" {
            "bilanciata"
        } else {
            effective_behavior_mode.as_str()
        };

        let (pref_provider, thresholds) = self.routing_helpers_for(intent).await;
        let d = route_model_with_mode(
            matrix, intent, estimated_tokens, effective_mode,
            pref_provider.as_deref(), &thresholds,
        );
        let decision_provider = d.provider.to_string();
        let decision_model = d.model.to_string();

        // La matrice gestisce già cooldown e fallback internamente.
        // Usa direttamente provider+model dalla matrice: la decisione
        // (intent, mode) → (provider, model) è specifica e non deve
        // essere sovrascritta dai default generici provider_model_*.
        // Solo se la matrice non ha trovato un provider disponibile
        // (__no_model__), cade sui candidati + default admin.
        let (provider, model) = if decision_provider != "__no_model__" {
            let model = if let Some(m) = model_override.filter(|v| !v.trim().is_empty()) {
                m.to_string()
            } else {
                decision_model.clone()
            };
            (decision_provider, model)
        } else {
            let provider = routing
                .candidates(intent, Some(decision_provider.as_str()))
                .into_iter()
                .find(|p| !is_provider_in_cooldown(p))
                .unwrap_or_else(|| decision_provider.clone());
            let model = model_override
                .filter(|v| !v.trim().is_empty())
                .map(str::to_string)
                .or_else(|| routing.provider_models.get(&provider).cloned())
                .unwrap_or_else(|| default_model_for_provider(matrix, &provider).to_string());
            (provider, model)
        };

        // Se il provider scelto e' in cooldown (rate-limit recente nel processo), trova alternativa.
        // Il fallback rispetta tier/capability: per task critici (heavy/medium) non degrada
        // silenziosamente a un default generico che potrebbe essere un modello inadeguato.
        let (provider, model) = if is_provider_in_cooldown(&provider) {
            tracing::warn!("Agent routing: '{}' scelto dal routing ma in cooldown, cerco alternativa", provider);

            // Risolvi tier/capability dalla cache (stessi valori usati sopra nel routing)
            let icap_arc = self.intent_capability.current_async().await.ok();
            let (tier, cap) = match icap_arc.as_deref() {
                Some(map) => match map.get(intent) {
                    Some(c) => (c.tier_for_tokens(estimated_tokens), c.base_capability.clone()),
                    None => ("light".to_string(), "chat".to_string()),
                },
                None => ("light".to_string(), "chat".to_string()),
            };

            // Strategia: cerca nel catalogo un modello dello stesso tier (o un
            // livello sotto) da un provider NON in cooldown. Mantiene la qualita'
            // richiesta per il task — non degrada a default generico.
            let tiers_to_try: Vec<&str> = match tier.as_str() {
                "heavy"  => vec!["heavy", "medium"],
                "medium" => vec!["medium"],
                _        => vec!["light"],
            };

            let mut found = None;
            for try_tier in &tiers_to_try {
                let rows: Vec<(String, String)> = sqlx::query_as(
                    r#"SELECT provider, model FROM ai_price_catalog
                       WHERE is_enabled = TRUE
                         AND performance_tier = $1
                         AND capabilities @> $2::jsonb
                         AND supports_tool_use = TRUE
                       ORDER BY input_cost_per_million_tokens ASC
                       LIMIT 10"#
                ).bind(try_tier).bind(format!("[\"{cap}\"]"))
                .fetch_all(db).await.unwrap_or_default();

                for (alt_provider, alt_model) in &rows {
                    if !is_provider_in_cooldown(alt_provider) {
                        tracing::info!(
                            "Agent routing (cooldown-fallback tier-aware): {} → {}/{} (tier={})",
                            provider, alt_provider, alt_model, try_tier
                        );
                        found = Some((alt_provider.clone(), alt_model.clone()));
                        break;
                    }
                }
                if found.is_some() { break; }
            }

            // Ultimo resort: hierarchy classica (se il catalogo non ha nulla)
            found.unwrap_or_else(|| {
                let hierarchy_str: Option<String> = futures::executor::block_on(async {
                    sqlx::query_scalar(
                        "SELECT value FROM settings WHERE key = 'provider_hierarchy' LIMIT 1"
                    ).fetch_optional(db).await.ok().flatten()
                });
                let hier: Vec<String> = hierarchy_str.as_deref().unwrap_or(&provider)
                    .split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect();
                let alt = hier.iter().find(|p| !is_provider_in_cooldown(p))
                    .cloned()
                    .unwrap_or_else(|| provider.clone());
                let alt_model = default_model_for_provider(matrix, &alt).to_string();
                tracing::warn!("Agent routing (cooldown-fallback legacy): {} → {}/{}", provider, alt, alt_model);
                (alt, alt_model)
            })
        } else {
            (provider, model)
        };

        tracing::info!(
            "Agent routing (local): intent={} tokens~{} → {}/{}",
            intent, estimated_tokens, provider, model
        );

        (provider, model)
    }

    pub async fn run(
        &self,
        db: &PgPool,
        input: OrchestratorRequest,
    ) -> anyhow::Result<OrchestratorResult> {
        let user_id =
            Uuid::parse_str(&input.user_id).map_err(|_| anyhow::anyhow!("invalid_user_id"))?;
        let project_uuid = Uuid::parse_str(&input.project_id)
            .map_err(|_| anyhow::anyhow!("invalid_project_id"))?;
        let run_id = Uuid::new_v4();

        // Snapshot della routing matrix DB (cache 60s, await sul lock).
        // Se la matrice non e' caricata, ritorniamo errore esplicito invece
        // di un fallback nascosto.
        let matrix_arc = self
            .routing_matrix
            .current_async()
            .await
            .map_err(|e| anyhow::anyhow!("routing_matrix non disponibile: {e}. Verifica DB e migrazioni 0101/0102."))?;
        let matrix = &*matrix_arc;

        // Step 1 + 2: Routing locale — zero gRPC, zero latenza aggiuntiva
        // Usa estimate_complexity per non farsi ingannare da messaggi con liste dati lunghe
        let msg_tokens_estimate = estimate_complexity(&input.message);
        let (intent_str, _confidence) = self.classify_intent_with_db_thresholds(&input.message).await;
        let intent = intent_str.to_string();
        let mut routing = Self::load_routing_config(db).await?;

        // Routing: se modalità "dinamico" usa il catalogo DB, altrimenti la matrice statica
        // Risolvi tier/capability dalla cache intent_capability (mig 0110).
        let icap_arc = self.intent_capability.current_async().await.ok();
        let (base_tier, capability) = match icap_arc.as_deref() {
            Some(map) => match map.get(intent_str) {
                Some(c) => (
                    c.tier_for_tokens(msg_tokens_estimate),
                    c.base_capability.clone(),
                ),
                None => ("light".to_string(), "chat".to_string()),
            },
            None => ("light".to_string(), "chat".to_string()),
        };
        let (suggested_provider, suggested_model): (Option<String>, Option<String>) = if routing.behavior_mode == "dinamico" {
            match route_model_from_catalog(db, &base_tier, &capability, "dinamico").await {
                Some(dyn_decision) if !is_provider_in_cooldown(&dyn_decision.provider) => {
                    tracing::info!(
                        "Dynamic catalog routing: intent={} tokens~{} → {}/{}",
                        intent, msg_tokens_estimate, dyn_decision.provider, dyn_decision.model
                    );
                    (Some(dyn_decision.provider.to_string()), Some(dyn_decision.model.to_string()))
                }
                other => {
                    if let Some(ref d) = other {
                        tracing::warn!(
                            "Dynamic catalog routing: {}/{} in cooldown, cerco alternativa tier-aware",
                            d.provider, d.model
                        );
                    }
                    // Cerca nel catalogo un modello dello stesso tier (o inferiore)
                    // da un provider NON in cooldown
                    let tiers_to_try: Vec<&str> = match base_tier.as_str() {
                        "heavy"  => vec!["heavy", "medium"],
                        "medium" => vec!["medium", "light"],
                        _        => vec!["light"],
                    };
                    let mut catalog_alt = None;
                    for try_tier in &tiers_to_try {
                        let rows: Vec<(String, String)> = sqlx::query_as(
                            r#"SELECT provider, model FROM ai_price_catalog
                               WHERE is_enabled = TRUE
                                 AND performance_tier = $1
                                 AND capabilities @> $2::jsonb
                                 AND supports_tool_use = TRUE
                               ORDER BY input_cost_per_million_tokens ASC
                               LIMIT 10"#
                        ).bind(try_tier).bind(format!("[\"{capability}\"]"))
                        .fetch_all(db).await.unwrap_or_default();

                        for (alt_p, alt_m) in &rows {
                            if !is_provider_in_cooldown(alt_p) {
                                tracing::info!(
                                    "Dynamic catalog routing (cooldown-fallback tier-aware): → {}/{} (tier={})",
                                    alt_p, alt_m, try_tier
                                );
                                catalog_alt = Some((Some(alt_p.clone()), Some(alt_m.clone())));
                                break;
                            }
                        }
                        if catalog_alt.is_some() { break; }
                    }

                    catalog_alt.unwrap_or_else(|| {
                        let (pref, thr) = futures::executor::block_on(self.routing_helpers_for(intent_str));
                        let d = route_model_with_mode(matrix, intent_str, msg_tokens_estimate, "bilanciata",
                                                       pref.as_deref(), &thr);
                        tracing::info!("Dynamic routing fallback to bilanciata: {}/{}", d.provider, d.model);
                        (Some(d.provider.to_string()), Some(d.model.to_string()))
                    })
                }
            }
        } else if routing.behavior_mode == "manuale" {
            // Manuale: nessun routing automatico — usa provider/model da config admin
            let (pref, thr) = self.routing_helpers_for(intent_str).await;
            let d = route_model_with_mode(matrix, intent_str, msg_tokens_estimate, "bilanciata",
                                           pref.as_deref(), &thr);
            tracing::info!(
                "Manual routing config: intent={} tokens~{} → {}/{}",
                intent, msg_tokens_estimate, d.provider, d.model
            );
            (Some(d.provider.to_string()), Some(d.model.to_string()))
        } else {
            let (pref, thr) = self.routing_helpers_for(intent_str).await;
            let d = route_model_with_mode(matrix, intent_str, msg_tokens_estimate, &routing.behavior_mode,
                                           pref.as_deref(), &thr);
            tracing::info!(
                "Local routing: intent={} tokens~{} mode={} → {}/{}",
                intent, msg_tokens_estimate, routing.behavior_mode, d.provider, d.model
            );
            (Some(d.provider.to_string()), Some(d.model.to_string()))
        };
        if let Some(project_chain) =
            Self::load_project_intent_chain(db, project_uuid, &intent).await?
        {
            routing
                .intent_provider_hierarchy
                .insert(intent.clone(), project_chain);
        }
        let forced_provider = input
            .provider_override
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_lowercase());
        let forced_model = input
            .model_override
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let token_budget = routing.resolve_token_budget(Some(msg_tokens_estimate.max(4096)));

        // Step 3: Build optimized prompt
        let context = mcp_token::optimize_context(&input.message, token_budget as usize);
        if context.tokens_saved > 0 {
            tracing::warn!(
                "CONTEXT_DROP: messaggio utente ridotto da {} → {} token (risparmiati: {}) per budget={}",
                context.original_tokens,
                context.optimized_tokens,
                context.tokens_saved,
                token_budget,
            );
        }
        let prompt_corrections = self
            .load_prompt_corrections(db, project_uuid, &input.message)
            .await
            .unwrap_or_default();
        let composed_prompt = Self::compose_prompt(
            db,
            &self.template_cache,
            &context.optimized_prompt,
            &prompt_corrections,
            input.automation_mode,
            &input.attachments,
        )
        .await;

        // ── Step 4: LLM Execution ─────────────────────────────────────────────────
        // PATH A: Nexus Gateway (routing, DLP, rate limiting, fallback automatico)
        // PATH B: Brain gRPC diretto (legacy, usato se il gateway non è disponibile)
        let (provider, model, completion, usage, total_cost, currency) =
            if let Some(gw) = &self.nexus_gateway {
                let alias = intent_to_alias(
                    &intent,
                    &routing.behavior_mode,
                    forced_model.as_deref(),
                );
                let gw_model = if let Some(fp) = &forced_provider {
                    format!("{fp}/{}", forced_model.as_deref().unwrap_or(&alias))
                } else {
                    alias
                };
                let gw_req = GwRequest {
                    model: gw_model,
                    messages: vec![GwMessage {
                        role: "user".to_string(),
                        content: composed_prompt.clone(),
                    }],
                    max_tokens: Some(token_budget),
                    temperature: None,
                    tools: None,
                    metadata: GwMetadata {
                        tenant_id: input.project_id.clone(),
                        user_id: input.user_id.clone(),
                        request_id: run_id.to_string(),
                        sensitivity_tier: 0,
                        feature: intent.clone(),
                    },
                };
                let prompt_tokens = mcp_token::count_tokens(&composed_prompt) as i32;
                let estimated_completion = (token_budget as i32 - prompt_tokens).max(0);
                // Fallback provider/model letti da DB (matrice routing) invece che hardcoded.
                let fallback_provider: String = match matrix.lookup("chat", "bilanciata") {
                    Some((p, _)) => p,
                    None => "openai".to_string(),
                };
                let hint_provider_owned: String = forced_provider
                    .as_deref()
                    .or(suggested_provider.as_deref())
                    .map(String::from)
                    .unwrap_or(fallback_provider);
                let fallback_model: String = default_model_for_provider(matrix, &hint_provider_owned);
                let hint_model_owned: String = forced_model
                    .as_deref()
                    .or(suggested_model.as_deref())
                    .map(String::from)
                    .unwrap_or(fallback_model);
                let hint_provider = hint_provider_owned.as_str();
                let hint_model = hint_model_owned.as_str();
                let reservation = billing::reserve_usage(
                    db, user_id, project_uuid, hint_provider, hint_model,
                    prompt_tokens, estimated_completion,
                    json!({"intent": intent, "profile_id": input.profile_id,
                           "via_nexus_gateway": true,
                           "corrections_count": prompt_corrections.len()}),
                )
                .await.map_err(|e| anyhow::anyhow!("billing_rejected: {e}"))?;

                let gw_resp = match gw.complete(gw_req).await {
                    Ok(r) => r,
                    Err(e) => {
                        billing::release_usage(db, &reservation, "gateway_error").await;
                        anyhow::bail!("Nexus Gateway failed for intent '{intent}': {e}");
                    }
                };
                let actual_usage = UsageNumbers {
                    prompt_tokens: gw_resp.usage.input_tokens as i32,
                    completion_tokens: gw_resp.usage.output_tokens as i32,
                    total_tokens: (gw_resp.usage.input_tokens + gw_resp.usage.output_tokens) as i32,
                };
                let (_, _, cost, cur) =
                    billing::finalize_usage(db, &reservation, run_id, &actual_usage).await?;
                let gw_completion = json!({"content": gw_resp.content, "metadata": {
                    "provider": gw_resp.provider_used, "model": gw_resp.model_used,
                    "latency_ms": gw_resp.latency_ms, "finish_reason": gw_resp.finish_reason},
                    "privacy_rerouted": gw_resp.privacy_rerouted.as_ref().map(|pr| json!({
                        "provider": pr.provider,
                        "blocked_tier": pr.blocked_tier,
                        "reason": pr.reason,
                    }))
                });
                if let Some(ref pr) = gw_resp.privacy_rerouted {
                    tracing::warn!(
                        "Nexus Gateway: privacy re-route tier={} → local provider={} intent={} tokens={}",
                        pr.blocked_tier, pr.provider, intent, actual_usage.total_tokens
                    );
                } else {
                    tracing::info!(
                        "Nexus Gateway: intent={} provider={} model={} tokens={}",
                        intent, gw_resp.provider_used, gw_resp.model_used, actual_usage.total_tokens
                    );
                }
                (gw_resp.provider_used, gw_resp.model_used, gw_completion, actual_usage, cost, cur)
            } else {
                // PATH B: Brain gRPC legacy
                let mut selected_provider: Option<String> = None;
                let mut selected_model: Option<String> = None;
                let mut completion: Option<serde_json::Value> = None;
                let mut usage: Option<UsageNumbers> = None;
                let mut usage_cost: Option<(f64, f64, f64, String)> = None;
                let mut skip_reasons = Vec::new();

                // In modalità dinamico la scelta del catalogo è autoritativa
                let provider_candidates = if let Some(provider) = forced_provider.as_ref() {
                    vec![provider.clone()]
                } else if routing.behavior_mode == "dinamico" {
                    if let Some(p) = suggested_provider.as_ref() {
                        vec![p.clone()]
                    } else {
                        routing.candidates(&intent, suggested_provider.as_deref())
                    }
                } else {
                    routing.candidates(&intent, suggested_provider.as_deref())
                };
        for provider in provider_candidates {
            let health = match self.neural.provider_health(&provider).await {
                Ok(health) => health,
                Err(error) => {
                    skip_reasons.push(format!("{provider}:health_check_failed:{error}"));
                    continue;
                }
            };

            let status = health["status"].as_str().unwrap_or("unknown");
            if !matches!(status, "ready" | "ok") {
                let reason = health["reason"]
                    .as_str()
                    .or_else(|| health["skipReasons"].get(0).and_then(Value::as_str))
                    .unwrap_or(status);
                skip_reasons.push(format!("{provider}:skipped:{reason}"));
                continue;
            }

            let model = if forced_provider.as_deref() == Some(provider.as_str()) {
                forced_model.clone().unwrap_or_else(|| {
                    routing.resolve_model(matrix, &provider, Some(provider.as_str()), None)
                })
            } else if routing.behavior_mode == "dinamico"
                && suggested_provider.as_deref() == Some(provider.as_str())
                && suggested_model.is_some()
            {
                // In dinamico fidiamoci del catalogo: niente override da provider_model_<x>.
                // suggested_model.is_some() controllato sopra; clone+unwrap_or e' difensivo.
                suggested_model.clone().unwrap_or_default()
            } else {
                routing.resolve_model(
                    matrix,
                    &provider,
                    suggested_provider.as_deref(),
                    suggested_model.as_deref(),
                )
            };
            let prompt_tokens = mcp_token::count_tokens(&composed_prompt) as i32;
            let estimated_completion_tokens = token_budget as i32 - prompt_tokens;
            let reservation = match billing::reserve_usage(
                db,
                user_id,
                project_uuid,
                &provider,
                &model,
                prompt_tokens,
                estimated_completion_tokens.max(0),
                json!({
                    "intent": intent,
                    "profile_id": input.profile_id,
                    "corrections_count": prompt_corrections.len(),
                    "request_message_id": input.request_message_id,
                    "automation_mode": input.automation_mode.as_str(),
                    "provider_override": forced_provider,
                    "model_override": forced_model,
                    "attachments_count": input.attachments.len(),
                }),
            )
            .await
            {
                Ok(reservation) => reservation,
                Err(error) => {
                    skip_reasons.push(format!("{provider}:billing_rejected:{error}"));
                    continue;
                }
            };

            let provider_completion = match self
                .neural
                .generate_completion(&provider, &model, &composed_prompt)
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    billing::release_usage(db, &reservation, "provider_error").await;
                    let error_msg = error.to_string();
                    // Distingui rate limit da altri errori
                    if error_msg.contains("429") || error_msg.to_lowercase().contains("rate_limit")
                        || error_msg.to_lowercase().contains("quota")
                        || error_msg.to_lowercase().contains("too_many_requests") {
                        skip_reasons.push(format!("{provider}:rate_limited:{error_msg}"));
                        tracing::warn!("Provider {provider} è rate-limited, provo il prossimo candidato");
                    } else {
                        skip_reasons.push(format!("{provider}:execution_error:{error_msg}"));
                    }
                    continue;
                }
            };

            if completion_has_error(&provider_completion) {
                billing::release_usage(db, &reservation, "provider_failed").await;
                let error = provider_completion["metadata"]["error"]
                    .as_str()
                    .unwrap_or("generation_failed");
                // Distingui rate limit da altri errori anche nella risposta
                if error.contains("429") || error.to_lowercase().contains("rate_limit")
                    || error.to_lowercase().contains("quota")
                    || error.to_lowercase().contains("too_many_requests") {
                    skip_reasons.push(format!("{provider}:rate_limited:{error}"));
                    tracing::warn!("Provider {provider} segnala rate limit nella risposta, provo il prossimo");
                } else {
                    skip_reasons.push(format!("{provider}:failed:{error}"));
                }
                continue;
            }

            let usage_numbers = billing::extract_usage_numbers(
                &provider_completion,
                prompt_tokens,
                estimated_completion_tokens,
            );
            let finalized_cost =
                billing::finalize_usage(db, &reservation, run_id, &usage_numbers).await?;

            selected_provider = Some(provider);
            selected_model = Some(model);
            completion = Some(provider_completion);
            usage = Some(usage_numbers);
            usage_cost = Some(finalized_cost);
            break;
        }

                let provider = selected_provider.ok_or_else(|| anyhow::anyhow!(
                    "No AI provider available for intent '{intent}'. Skip reasons: {}",
                    skip_reasons.join(", ")
                ))?;
                let model = selected_model.unwrap_or_else(|| default_model_for_provider(matrix, &provider).to_string());
                let completion = completion.ok_or_else(|| anyhow::anyhow!("No completion generated"))?;
                let usage = usage.unwrap_or(UsageNumbers { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 });
                let (_, _, cost, cur) = usage_cost.unwrap_or((0.0, 0.0, 0.0, "EUR".to_string()));
                (provider, model, completion, usage, cost, cur)
            };

        // Step 5: Build audit record
        let audit = OrchestratorAudit {
            project_id: input.project_id.clone(),
            profile_id: input.profile_id.clone(),
            intent: intent.clone(),
            provider: provider.clone(),
            model: model.clone(),
            token_budget,
            tokens_saved: context.tokens_saved as u32,
            resources: input.active_files.clone(),
            guardrail_result: "allowed".to_string(),
        };

        // Step 6: Persist audit to database
        let audit_json = serde_json::to_value(&audit)?;
        let session_uuid = input
            .session_id
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok());
        let profile_uuid = Uuid::parse_str(&input.profile_id).ok();
        sqlx::query(
            r#"
            INSERT INTO orchestrator_runs (id, project_id, user_id, session_id, profile_id, status, audit_json)
            VALUES ($1, $2::uuid, $3, $4, $5, 'completed', $6)
            "#,
        )
        .bind(run_id)
        .bind(&input.project_id)
        .bind(user_id)
        .bind(session_uuid)
        .bind(profile_uuid)
        .bind(&audit_json)
        .execute(db)
        .await
        .ok(); // Non-fatal: log but don't fail the request

        let payload = json!({
            "run_id": run_id.to_string(),
            "intent": intent,
            "provider": provider,
            "model": model,
            "completion": completion,
            "tokens_saved": context.tokens_saved,
            "prompt_tokens": usage.prompt_tokens,
            "completion_tokens": usage.completion_tokens,
            "total_tokens": usage.total_tokens,
            "total_cost": total_cost,
            "currency": currency,
            "applied_corrections": prompt_corrections,
            "automation_mode": input.automation_mode.as_str(),
            "attachments_count": input.attachments.len(),
        });

        Ok(OrchestratorResult { payload, audit })
    }

    async fn load_prompt_corrections(
        &self,
        db: &PgPool,
        project_id: Uuid,
        query: &str,
    ) -> anyhow::Result<Vec<Value>> {
        let globally_enabled = sqlx::query_scalar::<_, String>(
            "SELECT value FROM settings WHERE key = 'learning_prompt_corrections_enabled'",
        )
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|value| value.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(true);
        if !globally_enabled {
            return Ok(Vec::new());
        }

        let project_enabled = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT prompt_corrections_enabled
            FROM project_learning_config
            WHERE project_id = $1
            "#,
        )
        .bind(project_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .unwrap_or(true);
        if !project_enabled {
            return Ok(Vec::new());
        }

        let embedding = match self.neural.embed_text("", query).await {
            Ok(vector) => vector,
            Err(error) => {
                tracing::warn!("Unable to embed query for prompt corrections: {error}");
                return Ok(Vec::new());
            }
        };

        let hits =
            match vector_memory::search_prompt_correction_points(db, &embedding, project_id, 5)
                .await
            {
                Ok(hits) => hits,
                Err(error) => {
                    tracing::warn!("Unable to search prompt corrections: {error}");
                    return Ok(Vec::new());
                }
            };

        let mut corrections = Vec::new();
        let mut correction_ids = Vec::<Uuid>::new();
        for hit in hits {
            if hit.score < 0.78 {
                continue;
            }
            let correction_id = hit
                .payload
                .get("correction_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok());
            let text = hit
                .payload
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if text.is_empty() {
                continue;
            }
            if let Some(correction_id) = correction_id {
                correction_ids.push(correction_id);
            }
            corrections.push(json!({
                "id": correction_id.map(|value| value.to_string()).unwrap_or_default(),
                "text": text,
                "score": hit.score,
                "intent": hit.payload.get("intent").and_then(Value::as_str).unwrap_or("chat"),
                "pointId": hit.point_id,
            }));
        }

        if !correction_ids.is_empty() {
            let _ = sqlx::query(
                r#"
                UPDATE prompt_corrections
                SET retrieved_count = retrieved_count + 1,
                    last_retrieved_at = NOW(),
                    updated_at = NOW()
                WHERE id = ANY($1)
                "#,
            )
            .bind(&correction_ids)
            .execute(db)
            .await;
        }

        Ok(corrections)
    }

    async fn compose_prompt(
        db: &PgPool,
        cache: &crate::prompt_templates::TemplateCache,
        base_prompt: &str,
        corrections: &[Value],
        automation_mode: AutomationMode,
        attachments: &[ChatAttachment],
    ) -> String {
        let tpl_key = automation_mode.prompt_instruction_template_key();
        let mode_instruction = crate::prompt_templates::get_template_or_default(
            db, cache, tpl_key,
        )
        .await;
        let mut sections = vec![mode_instruction];

        if !corrections.is_empty() {
            let mut block = String::from("Correzioni note (da rispettare se pertinenti):\n");
            for correction in corrections {
                if let Some(text) = correction.get("text").and_then(Value::as_str) {
                    block.push_str("- ");
                    block.push_str(text.trim());
                    block.push('\n');
                }
            }
            sections.push(block.trim().to_string());
        }

        if !attachments.is_empty() {
            let text_attachments: Vec<_> = attachments.iter().filter(|a| !a.text_content.is_empty()).collect();
            let image_attachments: Vec<_> = attachments.iter().filter(|a| a.base64_content.is_some()).collect();
            if !text_attachments.is_empty() {
                let mut block = String::from("Allegati utente per questo messaggio:\n");
                for attachment in &text_attachments {
                    block.push_str(&format!(
                        "\n### File: {} ({}, {} bytes)\n{}\n",
                        attachment.name,
                        attachment.mime_type,
                        attachment.size_bytes,
                        attachment.text_content
                    ));
                }
                sections.push(block.trim().to_string());
            }
            if !image_attachments.is_empty() {
                let names: Vec<_> = image_attachments.iter().map(|a| a.name.as_str()).collect();
                sections.push(format!("L'utente ha allegato {} immagine/i: {}. Le immagini sono incluse come content block nel messaggio.", names.len(), names.join(", ")));
            }
        }

        sections.push(base_prompt.to_string());
        sections.join("\n\n")
    }

    async fn load_project_intent_chain(
        db: &PgPool,
        project_id: Uuid,
        intent: &str,
    ) -> anyhow::Result<Option<Vec<String>>> {
        let key = format!("project_{}_routing_{}_providers", project_id, intent);
        let value = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
            .bind(&key)
            .fetch_optional(db)
            .await?;
        Ok(parse_provider_list(value.as_deref()))
    }

    async fn load_routing_config(db: &PgPool) -> anyhow::Result<RoutingConfig> {
        let settings = sqlx::query_as::<_, SettingValueRow>(
            r#"
            SELECT key, value
            FROM settings
            WHERE category = 'routing'
               OR key IN (
                    'provider_hierarchy',
                    'provider_priority',
                    'provider_order',
                    'fallback_order',
                    'default_provider',
                    'default_model',
                    'token_budget',
                    'max_token_budget',
                    'provider_model_openai',
                    'provider_model_anthropic',
                    'provider_model_google',
                    'openai_model',
                    'anthropic_model',
                    'google_model',
                    'routing_fix_providers',
                    'routing_refactor_providers',
                    'routing_test_providers',
                    'routing_docs_providers',
                    'routing_architecture_providers',
                    'routing_chat_providers'
               )
            "#,
        )
        .fetch_all(db)
        .await?;

        Ok(RoutingConfig::from_settings(&settings))
    }
}

fn parse_provider_list(value: Option<&str>) -> Option<Vec<String>> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }

    let parsed = if raw.starts_with('[') {
        serde_json::from_str::<Vec<String>>(raw).ok()?
    } else {
        raw.split(',')
            .map(|provider| provider.trim().to_lowercase())
            .filter(|provider| !provider.is_empty())
            .collect::<Vec<_>>()
    };

    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}

fn push_unique(values: &mut Vec<String>, candidate: String) {
    if !values.iter().any(|value| value == &candidate) {
        values.push(candidate);
    }
}

/// Modello di default per un provider, letto dalla matrice DB
/// (`nexus_provider_default_model`, vedi migrazione 0101).
///
/// La matrice e' SEMPRE popolata: in caso di DB irraggiungibile,
/// `RoutingMatrix::fallback_safe()` riempie i 5 provider standard
/// (openai, anthropic, google, mistral, deepseek) con modelli letti
/// da env var `NEXUS_FALLBACK_<PROVIDER>_MODEL` o, in ultima istanza,
/// dal fallback hardcoded di emergenza in fallback_safe().
///
/// Se viene richiesto un provider sconosciuto (non in DB ne' nei 5
/// standard), ritorna un placeholder `unknown-provider-model` che
/// triggera errore 400 dal layer chiamante. NON c'e' fallback al
/// modello "gpt-4o-mini" hardcoded come prima.
pub fn default_model_for_provider(
    matrix: &crate::routing_matrix::RoutingMatrix,
    provider: &str,
) -> String {
    matrix
        .default_model(provider)
        .unwrap_or_else(|| {
            tracing::warn!(
                "default_model_for_provider: provider '{}' non presente nella matrice DB ne' nei 5 standard. \
                 Aggiungilo via UPDATE/INSERT su nexus_provider_default_model.",
                provider
            );
            format!("unknown-provider-{}", provider)
        })
}

fn completion_has_error(completion: &Value) -> bool {
    // metadata.error presente e non-null → errore
    if let Some(err) = completion
        .get("metadata")
        .and_then(|metadata| metadata.get("error"))
    {
        if !err.is_null() {
            return true;
        }
    }
    // campo error a root presente e non-null → errore
    if let Some(err) = completion.get("error") {
        if !err.is_null() {
            return true;
        }
    }
    completion
        .get("content")
        .and_then(Value::as_str)
        .map(|content| {
            let trimmed = content.trim_start();
            trimmed.starts_with("[Error:") || trimmed.starts_with("[error:")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_risky_imperativo_e_infinito() {
        // Bug reale visto in produzione: il prompt "Rimuovere le credenziali"
        // (infinito) non era riconosciuto come rischioso perche' la keyword era
        // "rimuovi " (imperativo). Le keyword sono ora prefissi laschi che
        // matchano tutte le forme verbali.
        assert!(is_risky_task("Rimuovere le credenziali in chiaro"));
        assert!(is_risky_task("Rimuovi i file Docker"));
        assert!(is_risky_task("Eliminare la cartella build"));
        assert!(is_risky_task("Elimina i file Dockerfile"));
        assert!(is_risky_task("Cancellare la configurazione obsoleta"));
        assert!(is_risky_task("Lancia rm -rf node_modules"));
        assert!(is_risky_task("DROP TABLE users"));
        assert!(is_risky_task("git reset --hard HEAD~3"));
        assert!(is_risky_task("docker prune -a"));
    }

    #[test]
    fn test_is_risky_negativi() {
        assert!(!is_risky_task("ciao come stai"));
        assert!(!is_risky_task("scrivi una funzione che somma due numeri"));
        assert!(!is_risky_task("come si configura il backend?"));
    }

    #[test]
    fn test_is_agentic_request_positivi() {
        // Caso paradigmatico del bug originale
        assert!(is_agentic_request("imposta un utente admin per l'applicazione e dammi user e password"));
        // Setup / configurazione
        assert!(is_agentic_request("Configura il backend per usare PostgreSQL"));
        assert!(is_agentic_request("Setup HTTPS sul dev server"));
        assert!(is_agentic_request("Abilita CORS per /api/*"));
        // Creazione
        assert!(is_agentic_request("Crea un endpoint /healthz"));
        assert!(is_agentic_request("Aggiungi una migrazione per la tabella users"));
        // Deploy / esecuzione
        assert!(is_agentic_request("Deploya il microservizio doc-service"));
        assert!(is_agentic_request("Lancia i test di integrazione"));
        assert!(is_agentic_request("Avvia il servizio backend"));
        // Domande "come fare X"
        assert!(is_agentic_request("Come faccio a creare un nuovo utente admin?"));
    }

    #[test]
    fn test_is_agentic_request_negativi() {
        // Domande puramente informative non sono agentic
        assert!(!is_agentic_request("Cos'e' un middleware in Express?"));
        assert!(!is_agentic_request("Che cosa fa il pattern repository?"));
        assert!(!is_agentic_request("Spiegami come funziona OAuth"));
        // Saluti / chat casuale
        assert!(!is_agentic_request("ciao come stai"));
        assert!(!is_agentic_request("grazie del supporto"));
    }

    #[test]
    fn test_classify_intent_with_agentic_promotion() {
        // Caso paradigmatico: prompt agentic breve viene promosso da chat a system_admin
        let (intent, _) = classify_intent_with_agentic_promotion(
            "imposta un utente admin per l'applicazione"
        );
        assert_eq!(intent, "system_admin",
            "prompt agentic breve deve essere promosso a system_admin");

        // Promozione anche per "configura"
        let (intent, _) = classify_intent_with_agentic_promotion("Configura il backend");
        assert_eq!(intent, "system_admin");

        // Domanda informativa pura resta su chat
        let (intent, _) = classify_intent_with_agentic_promotion("Cos'e' Docker?");
        assert_eq!(intent, "chat");

        // Intent gia' specifici NON vengono toccati dalla promozione
        let (intent, _) = classify_intent_with_agentic_promotion("Elimina i file Dockerfile");
        assert_eq!(intent, "file_ops", "intent specifico non viene riscritto");
    }

    // ─────────────────────────────────────────────────────────────────
    // Test promozione test → fix_complesso (test failure resolution)
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_is_test_failure_resolution_positivi() {
        // Casi paradigmatici osservati in produzione (Redemptor / Playwright)
        assert!(is_test_failure_resolution("esegui i test playwright e risolvi i fail"));
        assert!(is_test_failure_resolution("lancia i test e correggi gli errori"));
        assert!(is_test_failure_resolution("fai funzionare i test Playwright"));
        assert!(is_test_failure_resolution("fix i test che falliscono"));
        assert!(is_test_failure_resolution("Run Playwright tests and make them pass"));
        assert!(is_test_failure_resolution("i test playwright stanno fallendo, ripara"));
        assert!(is_test_failure_resolution("applica fix ai test pytest"));
        assert!(is_test_failure_resolution("i test cargo non passano, risolvi"));
        assert!(is_test_failure_resolution("verifica perche' i test failure"));
        assert!(is_test_failure_resolution("playwright test failure: indaga e correggi"));
    }

    #[test]
    fn test_is_test_failure_resolution_negativi() {
        // "scrivi un test" non e' una risoluzione di fallimento
        assert!(!is_test_failure_resolution("scrivi un test unitario per la funzione X"));
        // "esegui test" senza richiesta di fix non promuove
        assert!(!is_test_failure_resolution("esegui i test playwright"));
        // Senza menzione test
        assert!(!is_test_failure_resolution("risolvi questo errore di compilazione"));
        // Chat informativa
        assert!(!is_test_failure_resolution("come si configura playwright?"));
        // Verb correttivo senza test
        assert!(!is_test_failure_resolution("fix questo bug nel server"));
    }

    #[test]
    fn test_promotion_test_a_fix_complesso() {
        // Caso paradigmatico Redemptor: gpt-4.1-mini diagnosticava invece di
        // applicare fix perche' intent=test mappava a modelli light.
        let (intent, _) = classify_intent_with_agentic_promotion(
            "Esegui i test Playwright e risolvi i fallimenti rilevati"
        );
        assert_eq!(intent, "fix_complesso",
            "test + verbo correttivo deve essere promosso a fix_complesso");

        let (intent, _) = classify_intent_with_agentic_promotion(
            "fai funzionare i test Playwright"
        );
        assert_eq!(intent, "fix_complesso");

        // Negativo: solo "scrivi test" resta test (no failure resolution)
        let (intent, _) = classify_intent_with_agentic_promotion(
            "scrivi i test unitari per il modulo auth"
        );
        // Nota: classify_intent_local potrebbe ritornare un altro intent qui;
        // l'importante e' che NON sia fix_complesso senza failure resolution.
        assert_ne!(intent, "fix_complesso",
            "creazione test senza failure resolution non deve essere promossa");
    }

    #[test]
    fn test_classify_intent_local_file_ops() {
        // Verifiche dei nuovi intent introdotti
        let (intent, _) = classify_intent_local("Per favore elimina i file Dockerfile rimasti nel progetto");
        assert_eq!(intent, "file_ops");
    }

    #[test]
    fn test_classify_intent_local_system_admin() {
        let (intent, _) = classify_intent_local("Esegui docker compose down per fermare i container");
        assert_eq!(intent, "system_admin");
    }

    #[test]
    fn test_classify_intent_local_debug_via_stack_trace() {
        let msg = "Got NullReferenceException with stack trace at line 42 in ProcessRequest, can you fixare il bug?";
        let (intent, _) = classify_intent_local(msg);
        assert_eq!(intent, "debug");
    }

    #[test]
    fn test_classify_intent_local_migra_dotnet_va_a_refactor() {
        // Bug residuo del refactor 0101: "migra il backend .NET 9 da SQL Server a PostgreSQL"
        // veniva classificato come "chat" e routato a mistral-small (inadatto per code migration).
        // Con i prefissi laschi "migra "/"migrare " in refactor, ora va correttamente in refactor.
        let (intent, _) = classify_intent_local(
            "Migra il backend .NET 9 da SQL Server a PostgreSQL"
        );
        assert_eq!(intent, "refactor");
    }

    #[test]
    fn test_classify_intent_local_converti_typescript_va_a_refactor() {
        let (intent, _) = classify_intent_local("Converti questi file da JavaScript a TypeScript");
        assert_eq!(intent, "refactor");
    }

    #[test]
    fn test_classify_intent_local_sostituisci_libreria_va_a_refactor() {
        let (intent, _) = classify_intent_local(
            "Sostituisci la libreria axios con fetch nativa in tutti i moduli"
        );
        assert_eq!(intent, "refactor");
    }

    #[test]
    fn test_classify_intent_local_piano_migrazione_va_a_architecture() {
        // Distinzione: PLANNING di migrazione (no codice) → architecture
        let (intent, _) = classify_intent_local(
            "Definisci un piano di migrazione del database da MySQL a PostgreSQL"
        );
        assert_eq!(intent, "architecture");
    }

    #[test]
    fn test_route_model_with_mode_file_ops_approfondita() {
        // Test usa la fallback safe matrix (anthropic claude-sonnet per tutti gli intent rischiosi)
        let m = crate::routing_matrix::RoutingMatrix::fallback_safe();
        let thr = TokenThresholds::defaults();
        let d = route_model_with_mode(&m, "file_ops", 1500, "approfondita", Some("anthropic"), &thr);
        assert_eq!(d.provider, "anthropic");
        assert_eq!(d.model, "claude-sonnet-4-6");
    }

    #[test]
    fn test_route_model_with_mode_system_admin_bilanciata() {
        let m = crate::routing_matrix::RoutingMatrix::fallback_safe();
        let thr = TokenThresholds::defaults();
        let d = route_model_with_mode(&m, "system_admin", 1500, "bilanciata", Some("anthropic"), &thr);
        assert_eq!(d.provider, "anthropic");
        // Almeno un modello "haiku" o "sonnet", mai "small" o "nano"
        assert!(!d.model.contains("nano"), "model={}", d.model);
        assert!(!d.model.contains("small"), "model={}", d.model);
    }

    #[test]
    fn test_route_model_with_mode_no_hardcoded_last_resort() {
        // Verifica che il fallback hardcoded "openai/gpt-4o-mini" sia stato
        // rimosso (Fase 3, regola G CLAUDE.md). Una matrice vuota + nessun
        // preferred_provider deve ritornare la sentinella __no_model__,
        // NON un modello arbitrario.
        use crate::routing_matrix::RoutingMatrix;
        use std::collections::HashMap;
        let empty = RoutingMatrix {
            by_intent_mode: HashMap::new(),
            default_models: HashMap::new(),
            purpose_models: HashMap::new(),
            purpose_tiers: HashMap::new(),
            escalations: HashMap::new(),
            loaded_at: std::time::Instant::now(),
        };
        let thr = TokenThresholds::defaults();
        // No preferred_provider -> sentinella
        let d = route_model_with_mode(&empty, "system_admin", 1500, "bilanciata", None, &thr);
        assert_eq!(d.provider, "__no_model__", "deve ritornare sentinella, non gpt-4o-mini hardcoded");
        assert_eq!(d.model, "__no_model__");
    }

    #[test]
    fn test_route_model_with_mode_uses_token_thresholds() {
        // Verifica che le soglie token vengano lette dai thresholds passati
        // invece dei valori hardcoded 400/1500/3000.
        let m = crate::routing_matrix::RoutingMatrix::fallback_safe();
        let custom_thr = TokenThresholds {
            chat_breve: 100,   // soglia molto bassa: anche 200 token va a media
            chat_media: 200,
            complex_fix: 500,  // fix con 600 token va a fix_complesso
        };
        // Con questi thresholds, intent=fix tokens=600 -> fix_complesso
        // (la matrix fallback_safe ha fix_complesso × bilanciata mappato).
        let d = route_model_with_mode(&m, "fix", 600, "bilanciata", None, &custom_thr);
        // fix_complesso × bilanciata -> claude-haiku in fallback_safe matrix
        assert_eq!(d.provider, "anthropic");
        assert!(d.model.contains("haiku"), "atteso haiku per fix_complesso bilanciata, got {}", d.model);
    }

    #[test]
    fn test_prompt_hash_stable() {
        // sha256(message[:1000]) deve essere deterministico e ignorare il
        // contenuto oltre 1000 char per consistency tra prompt simili.
        let h1 = prompt_hash("hello world");
        let h2 = prompt_hash("hello world");
        assert_eq!(h1, h2);
        // Hash diverso per messaggio diverso
        let h3 = prompt_hash("hello!");
        assert_ne!(h1, h3);
        // Stessi primi 1000 char -> stesso hash anche con coda diversa
        let long_a = "x".repeat(1000) + "tail_a";
        let long_b = "x".repeat(1000) + "tail_b";
        assert_eq!(prompt_hash(&long_a), prompt_hash(&long_b));
    }

    // ─────────────────────────────────────────────────────────────────
    // Test L2: ClassifiedIntent + disambiguation logic
    // ─────────────────────────────────────────────────────────────────

    /// Helper per creare un ClassifiedIntent di test.
    fn classified(intent: &'static str, conf: f32, candidates: Vec<(&str, f32)>, ambig: bool) -> ClassifiedIntent {
        ClassifiedIntent {
            intent,
            confidence: conf,
            candidates: candidates
                .into_iter()
                .map(|(i, c)| IntentCandidate {
                    intent: i.to_string(),
                    confidence: c,
                })
                .collect(),
            is_ambiguous: ambig,
            slots: crate::routing_slots::ActionSlots::default(),
        }
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: deterministic_intent_fallback (classificatore robusto)
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn deterministic_fallback_task_agentico_creazione_app() {
        // Caso reale dell'incidente: questo messaggio NON deve degradare a
        // chat. Deve ritornare un intent agentico ad alta confidenza cosi'
        // il pre-check salta l'LLM e il path agent parte anche se l'LLM e' down.
        let (intent, conf) =
            deterministic_intent_fallback("Crea l'applicazione completa descritta nel file allegato. Implementala e avviala.")
                .expect("atteso match agentico");
        assert_ne!(intent, "chat");
        assert_eq!(intent, "system_admin");
        assert!(conf >= 0.85, "confidence attesa alta, got {conf}");
    }

    #[test]
    fn deterministic_fallback_chat_pura_e_none() {
        // Conversazione pura: nessun verbo+contesto software -> None,
        // lascia decidere all'LLM o al default chat.
        assert!(deterministic_intent_fallback("ciao come stai").is_none());
        assert!(deterministic_intent_fallback("grazie mille, ottimo lavoro").is_none());
    }

    #[test]
    fn deterministic_fallback_lettura_codice() {
        // "leggi src/app.js" -> intent di lettura/analisi (debug), non chat.
        let (intent, conf) = deterministic_intent_fallback("leggi src/app.js e dimmi cosa fa")
            .expect("atteso match lettura");
        assert_eq!(intent, "debug");
        assert!(conf > 0.0 && conf < 0.85, "confidence media attesa, got {conf}");
    }

    #[test]
    fn deterministic_fallback_docs() {
        let (intent, _) = deterministic_intent_fallback("scrivi readme per questo progetto")
            .expect("atteso match docs");
        assert_eq!(intent, "docs");
    }

    #[test]
    fn deterministic_fallback_richiesta_informativa_non_agentica() {
        // "cos'e' un endpoint?" contiene "endpoint" ma e' una domanda
        // informativa: non deve essere classificata come agentica.
        assert!(deterministic_intent_fallback("cos'e' un endpoint REST?").is_none());
    }

    #[test]
    fn classified_intent_struct_e_costruibile_e_serializzabile() {
        // Smoke test: la struct ClassifiedIntent e i suoi campi sono pubblici
        // e tipizzati correttamente per essere passati a chat_messages.rs.
        let c = classified("debug", 0.85, vec![("debug", 0.85), ("fix", 0.40)], false);
        assert_eq!(c.intent, "debug");
        assert_eq!(c.candidates.len(), 2);
        assert_eq!(c.candidates[0].intent, "debug");
        assert!(!c.is_ambiguous);
    }

    #[test]
    fn intent_candidate_e_serializzabile_a_json() {
        // Serializzabilita' necessaria perche' i candidati vengono persistiti
        // in chat_messages.metadata per audit + UI.
        let c = IntentCandidate {
            intent: "fix".to_string(),
            confidence: 0.7,
        };
        let json_str = serde_json::to_string(&c).expect("serialize ok");
        assert!(json_str.contains("\"fix\""));
        assert!(json_str.contains("0.7"));
        // Round-trip
        let parsed: IntentCandidate = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed.intent, "fix");
    }
}

