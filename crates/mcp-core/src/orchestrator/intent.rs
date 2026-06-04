//! Classificazione intent: keyword matching locale, promozione
//! agentica, fallback deterministico e classificazione LLM async.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use uuid::Uuid;

use mcp_proto::neural::{
    neural_core_service_client::NeuralCoreServiceClient, ClassifyIntentRequest, EmbedTextRequest,
    GenerateAgentTurnRequest, GenerateCompletionRequest, RouteModelRequest,
};

use crate::{
    billing::{self, UsageNumbers},
    domain::OrchestratorAudit,
    nexus_gateway::{intent_to_alias, GwMessage, GwMetadata, GwRequest, NexusGatewayClient},
    provider_cooldown::{is_provider_in_cooldown, put_provider_in_cooldown},
    vector_memory,
};

use super::*;

/// Classifica l'intent dal testo del messaggio tramite keyword matching.
/// Rispecchia esattamente la logica Python in brain/router/service.py.
pub(crate) fn classify_intent_local(message: &str) -> (&'static str, f32) {
    let lower = message.to_lowercase();

    // ── Intent "debug" prioritario ────────────────────────────────────────
    // Riconosce pattern di stack trace o richieste agentiche multi-step su errori
    // reali (.NET / Java / Python / Rust / Node). Questo intent va a tier "heavy"
    // perché richiede sequence di tool call (read_file → str_replace → restart),
    // che modelli "code-light" come codestral non sanno orchestrare.
    let stack_trace_signals = [
        "\n   at ",
        "\n    at ",
        "traceback (most recent call last)",
        ".exception",
        ".error:",
        "stack trace",
        "stacktrace",
        "panicked at",
        "fatal:",
        "rejectedexecutionexception",
    ];
    let agentic_signals = [
        "analizza la gerarchia",
        "causa radice",
        "root cause",
        "stack trace",
        "tool call",
        "fixare",
        "esegui restart",
        "leggi il file",
        "appsettings",
        "log degli ultimi",
    ];
    let stack_hits: usize = stack_trace_signals
        .iter()
        .filter(|s| lower.contains(*s))
        .count();
    let agent_hits: usize = agentic_signals
        .iter()
        .filter(|s| lower.contains(*s))
        .count();
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
        (
            "file_ops",
            &[
                "elimina file",
                "rimuovi file",
                "cancella file",
                "remove file",
                "delete file",
                "elimina i file",
                "remove the file",
                "elimina la cartella",
                "rimuovi la cartella",
                "delete folder",
                "elimina dockerfile",
                "rimuovi dockerfile",
                "elimina docker-compose",
                "rimuovi docker-compose",
                "elimina configurazione docker",
                "remove docker configuration",
                "elimina file di configurazione",
                "rimuovi file di configurazione",
                "ripulisci la directory",
                "cleanup directory",
            ],
        ),
        (
            "system_admin",
            &[
                "docker stop",
                "docker rm",
                "docker prune",
                "system prune",
                "ferma il container",
                "stop container",
                "kill container",
                "elimina container",
                "remove container",
                "delete container",
                "ferma il servizio",
                "stop service",
                "systemctl stop",
                "systemctl restart",
                "restart service",
                "compose down",
                "compose up",
                "docker compose",
                "elimina docker",
                "rimuovi docker locale",
                "elimina docker locale",
                // Anche i pattern dell'analyzer: "rimuovere il container",
                // "container ridondante", ecc.
                "rimuovere il container",
                "rimuovere container",
                "container ridondante",
                "container superfluo",
            ],
        ),
        (
            "fix",
            &[
                "/fix", "bug", "error", "crash", "broken", "debug", "issue", "patch", "errore",
                "correggi", "risolvi", "problema",
            ],
        ),
        // Refactor include task di migrazione codice (es. "migra il backend da SQL Server a PostgreSQL",
        // "converti TypeScript a JavaScript", "sostituisci EFCore con Npgsql"). Senza questi prefissi
        // laschi, "migra .NET 9 ..." finiva nel default "chat" → mistral-small inadatto.
        // I prefissi (no parola intera) catturano forme verbali italiane multiple:
        // "migra/migrare/migrate", "porta/portare", "converti/convertire", "trasforma/trasformare",
        // "sposta/spostare", "sostituisci/sostituire", "rimpiazza/rimpiazzare".
        (
            "refactor",
            &[
                "/refactor",
                "refactor",
                "clean",
                "simplify",
                "extract",
                "improve",
                "migliora",
                "pulisci",
                "semplifica",
                "ristruttura",
                "migra ",
                "migrare ",
                "porta da ",
                "porta a ",
                "portare da ",
                "converti ",
                "convertire ",
                "trasform",
                "sposta da ",
                "spostare da ",
                "sostituisci ",
                "sostituire ",
                "rimpiazza ",
                "rimpiazzare ",
                "migrate from",
                "migrate to",
                "convert to",
                "rename ",
            ],
        ),
        (
            "test",
            &[
                "/test",
                "test",
                "coverage",
                "assert",
                "spec",
                "unit test",
                "integration test",
            ],
        ),
        (
            "docs",
            &[
                "/docs",
                "document",
                "readme",
                "jsdoc",
                "comment",
                "explain",
                "documenta",
                "commenta",
                "spiega",
            ],
        ),
        // Architecture e' per task di PLANNING (piano di migrazione, design system) senza
        // toccare ancora codice. I verbi imperativi di migrazione codice vanno a "refactor".
        (
            "architecture",
            &[
                "/arch",
                "architecture",
                "design",
                "system",
                "plan",
                "architettura",
                "progetta",
                "piano di migrazione",
                "migration plan",
                "strategia di migrazione",
                "migration strategy",
            ],
        ),
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
pub(crate) fn is_risky_task(message: &str) -> bool {
    let lc = message.to_lowercase();
    const RISKY: &[&str] = &[
        // Filesystem distruttive
        "rm -rf",
        " rm ",
        "rmdir",
        "unlink",
        // Verbi distruttivi (prefissi: matchano forme infinitive/imperative/coniugate)
        "elimin",
        "rimuov",
        "cancell",
        "delete",
        "remove",
        // Docker / container
        "docker prune",
        "system prune",
        "docker rm ",
        "docker rmi",
        "compose down",
        "ferma il container",
        "stop container",
        // Database
        "drop table",
        "drop database",
        "drop schema",
        "truncate",
        // Git distruttive
        "git reset --hard",
        "force push",
        "--force",
        "git clean",
        // Sistema
        "shutdown",
        "reboot",
        "systemctl stop",
        "systemctl disable",
        "kill -9",
        "pkill",
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
pub(crate) fn is_agentic_request(message: &str) -> bool {
    let lc = message.to_lowercase();
    const AGENTIC_VERBS: &[&str] = &[
        // Setup / configurazione
        "imposta",
        "impost",
        "configur",
        "setup",
        "set up",
        "set-up",
        "abilit",
        "disabilit",
        "enable",
        "disable",
        // Creazione / modifica
        "crea ",
        "create ",
        "creare ",
        "aggiung",
        "add ",
        "cambi",
        "modific",
        "aggiorn",
        "update ",
        "modify ",
        // Deploy / esecuzione
        "deploy",
        "lancia ",
        "launch",
        "avvia",
        "start service",
        "installa",
        "install ",
        // Investigazione + azione
        "trova ",
        "find ",
        "individua",
        "identifica",
        "verifica ",
        "verify ",
        "controlla ",
        // Implementazione / integrazione
        "implementa",
        "integra",
        "integrate ",
        "implement",
        // Riparazione (oltre fix che è già intent dedicato)
        "ripar",
        "ripara",
        // Domande "come/dove" + azione (heuristic for "how to do X")
        "come faccio a",
        "come si imposta",
        "come configurare",
        "how do i ",
        "how to set",
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
pub(crate) fn is_test_failure_resolution(message: &str) -> bool {
    let lc = message.to_lowercase();
    // Deve menzionare i test E richiedere un'azione correttiva
    let mentions_tests = [
        "test",
        "playwright",
        "vitest",
        "jest",
        "pytest",
        "cargo test",
    ]
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
        "risolv",
        "fix",
        "correg",
        "ripar",
        "ripara",
        "fai funzionare",
        "fai passare",
        "make pass",
        "make work",
        "fai partire e",
        "esegui e",
        "lancia e",
        "applica fix",
        "applica patch",
        "applica corre",
        "make them pass",
        "pass all tests",
        "tutti i test passino",
        "fai in modo che",
        "fai sì che",
        "fa sì che",
        "non funziona",
        "non funzionano",
        "non passano",
        "stanno fallendo",
        "are failing",
        "is failing",
        "failing",
        "failure",
        "failures",
        "failed",
        // Italiano: fallit* matcha fallito/falliti/fallita/fallite
        "fallit",
        "falliscono",
        "fallimento",
        "fallimenti",
        // Format M-ticket Nexus: "Fix M44: ..." viene generato per ogni problema
        "fix m",
        "errore — problema",
        // Errori da error-fix workflow
        "errore rilevato",
        "severita: error",
        "severità: error",
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
pub(crate) fn classify_intent_with_agentic_promotion(message: &str) -> (&'static str, f32) {
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
pub(crate) fn deterministic_intent_fallback(message: &str) -> Option<(&'static str, f32)> {
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
        "documenta",
        "genera doc",
        "genera la doc",
        "genera documentazione",
        "crea documentazione",
        "crea la documentazione",
        "scrivi readme",
        "scrivi il readme",
        "genera readme",
        "write docs",
        "generate docs",
        "write readme",
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
        "crea ",
        "creare",
        "crei ",
        "implementa",
        "implementare",
        "sviluppa",
        "sviluppare",
        "costruisci",
        "costruire",
        "genera ",
        "generare",
        "scrivi ",
        "scrivere",
        "aggiung",
        "add ",
        "modific",
        "modify ",
        "corregg",
        "fixa",
        "fix ",
        "refactor",
        "installa",
        "install ",
        "configur",
        "avvia",
        "esegui ",
        "lancia ",
        "build",
        "deploy",
        "scaffold",
    ];
    // Contesto che qualifica il verbo come task software (evita falsi positivi
    // tipo "crea un account sul sito X" che non e' un task di codice).
    const SOFTWARE_CONTEXT: &[&str] = &[
        "app",
        "applicazione",
        "progetto",
        "project",
        "file",
        "funzione",
        "function",
        "componente",
        "component",
        "servizio",
        "service",
        "endpoint",
        "server",
        "api",
        "script",
        "test",
        "codice",
        "code",
        "feature",
        "modulo",
        "module",
        "classe",
        "class",
        "container",
        "docker",
        "database",
        "schema",
        "migrazione",
        "migration",
        "pagina",
        "page",
        "form",
        "route",
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
        "leggi ",
        "mostra",
        "analizza",
        "analizzare",
        "cosa fa",
        "che cosa fa",
        "quanti ",
        "quante ",
        "elenca",
        "lista ",
        "trova ",
        "cerca ",
        "individua",
        "ispeziona",
        "esamina",
    ];
    const READ_CONTEXT: &[&str] = &[
        "file",
        "src/",
        "codice",
        "code",
        "funzione",
        "function",
        "classe",
        "class",
        "modulo",
        "module",
        "errore",
        "error",
        "log",
        "endpoint",
        "test",
        "progetto",
        "repository",
        "repo",
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
pub(crate) struct AgenticIntentResponse {
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
pub(crate) const LLM_CLASSIFIER_MIN_CONFIDENCE_DEFAULT: f32 = 0.60;

/// Soglia (default) sopra la quale il classificatore deterministico keyword
/// viene usato come pre-check saltando l'LLM. Override DB:
/// `settings.routing.intent_deterministic_high`. Usata solo quando la cache
/// `RoutingThresholds` non e' disponibile.
pub(crate) const INTENT_DETERMINISTIC_HIGH_DEFAULT: f32 = 0.85;

/// Soglia (default) minima sotto la quale il deterministico NON viene usato
/// nemmeno come fallback quando l'LLM ricade su `chat`. Override DB:
/// `settings.routing.intent_deterministic_min`.
pub(crate) const INTENT_DETERMINISTIC_MIN_DEFAULT: f32 = 0.60;

// ── Telemetria routing (mig 0112) ───────────────────────────────────────────

/// Calcola sha256(message[:1000]) per la telemetria. Non e' PII e ci permette
/// di fare GROUP BY prompt_hash sulla tabella nexus_routing_decisions per
/// vedere prompt ricorrenti / drift del classifier.
pub(crate) fn prompt_hash(message: &str) -> String {
    use sha2::{Digest, Sha256};
    let head: String = message.chars().take(1000).collect();
    let mut hasher = Sha256::new();
    hasher.update(head.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Fire-and-forget INSERT in `nexus_routing_decisions`. Spawna un task tokio
/// per non aggiungere latenza al path caldo. Eventuali errori sono loggati WARN.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_routing_decision_insert(
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
        .bind(if classifier_confidence >= 0.85 {
            "llm"
        } else {
            "keyword_or_promotion"
        })
        .bind(classifier_confidence)
        .bind::<Option<bool>>(None) // classifier_cached: non noto a questo livello
        .bind(&selected_provider)
        .bind(&selected_model)
        .bind(&decision_source)
        .bind(&rationale)
        .bind(no_capable_provider)
        .bind(&cooldown)
        .bind(no_capable_provider) // fallback_triggered = no_capable_provider
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
pub(crate) fn intent_str_to_static(intent: &str) -> Option<&'static str> {
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
pub(crate) async fn classify_intent_async_with_threshold(
    message: &str,
    min_confidence: f32,
    timeout_seconds: f32,
) -> (&'static str, f32) {
    // Priorita': env var override > AtomicBool inizializzato dal DB in main.rs.
    let llm_enabled = match std::env::var("NEXUS_LLM_CLASSIFIER_ENABLED").as_deref() {
        Ok(v) => !matches!(
            v.trim().to_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => LLM_CLASSIFIER_ENABLED.load(Ordering::Relaxed),
    };

    if !llm_enabled || message.trim().is_empty() {
        return classify_intent_with_agentic_promotion(message);
    }

    let brain_url =
        std::env::var("BRAIN_REST_URL").unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
    let url = format!(
        "{}/classify-intent-agentic",
        brain_url.trim_end_matches('/')
    );

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
            parsed.fallback_used,
            parsed.confidence,
            min_confidence
        );
        return classify_intent_with_agentic_promotion(message);
    }

    let intent_static = match intent_str_to_static(&parsed.intent) {
        Some(s) => s,
        None => {
            tracing::warn!(
                "classifier LLM: intent sconosciuto '{}' — fallback",
                parsed.intent
            );
            return classify_intent_with_agentic_promotion(message);
        }
    };

    tracing::info!(
        "classifier LLM: intent={} agentic_score={:.2} confidence={:.2} cached={}",
        intent_static,
        parsed.agentic_score,
        parsed.confidence,
        parsed.cached
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
    if (intent_static == "test" || intent_static == "fix") && is_test_failure_resolution(message) {
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
pub(crate) async fn classify_intent_async_full_with_threshold(
    message: &str,
    min_confidence: f32,
    timeout_seconds: f32,
) -> ClassifiedIntent {
    let llm_enabled = match std::env::var("NEXUS_LLM_CLASSIFIER_ENABLED").as_deref() {
        Ok(v) => !matches!(
            v.trim().to_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
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

    let brain_url =
        std::env::var("BRAIN_REST_URL").unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
    let url = format!(
        "{}/classify-intent-agentic",
        brain_url.trim_end_matches('/')
    );

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
            vec![IntentCandidate {
                intent: i.to_string(),
                confidence: c,
            }]
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
pub(crate) async fn classify_intent_async(message: &str) -> (&'static str, f32) {
    classify_intent_async_with_threshold(message, LLM_CLASSIFIER_MIN_CONFIDENCE_DEFAULT, 5.0).await
}
