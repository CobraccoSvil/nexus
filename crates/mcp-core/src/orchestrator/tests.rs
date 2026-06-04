//! Test unitari del modulo orchestrator.

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
    assert!(is_agentic_request(
        "imposta un utente admin per l'applicazione e dammi user e password"
    ));
    // Setup / configurazione
    assert!(is_agentic_request(
        "Configura il backend per usare PostgreSQL"
    ));
    assert!(is_agentic_request("Setup HTTPS sul dev server"));
    assert!(is_agentic_request("Abilita CORS per /api/*"));
    // Creazione
    assert!(is_agentic_request("Crea un endpoint /healthz"));
    assert!(is_agentic_request(
        "Aggiungi una migrazione per la tabella users"
    ));
    // Deploy / esecuzione
    assert!(is_agentic_request("Deploya il microservizio doc-service"));
    assert!(is_agentic_request("Lancia i test di integrazione"));
    assert!(is_agentic_request("Avvia il servizio backend"));
    // Domande "come fare X"
    assert!(is_agentic_request(
        "Come faccio a creare un nuovo utente admin?"
    ));
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
    let (intent, _) =
        classify_intent_with_agentic_promotion("imposta un utente admin per l'applicazione");
    assert_eq!(
        intent, "system_admin",
        "prompt agentic breve deve essere promosso a system_admin"
    );

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
    assert!(is_test_failure_resolution(
        "esegui i test playwright e risolvi i fail"
    ));
    assert!(is_test_failure_resolution(
        "lancia i test e correggi gli errori"
    ));
    assert!(is_test_failure_resolution(
        "fai funzionare i test Playwright"
    ));
    assert!(is_test_failure_resolution("fix i test che falliscono"));
    assert!(is_test_failure_resolution(
        "Run Playwright tests and make them pass"
    ));
    assert!(is_test_failure_resolution(
        "i test playwright stanno fallendo, ripara"
    ));
    assert!(is_test_failure_resolution("applica fix ai test pytest"));
    assert!(is_test_failure_resolution(
        "i test cargo non passano, risolvi"
    ));
    assert!(is_test_failure_resolution(
        "verifica perche' i test failure"
    ));
    assert!(is_test_failure_resolution(
        "playwright test failure: indaga e correggi"
    ));
}

#[test]
fn test_is_test_failure_resolution_negativi() {
    // "scrivi un test" non e' una risoluzione di fallimento
    assert!(!is_test_failure_resolution(
        "scrivi un test unitario per la funzione X"
    ));
    // "esegui test" senza richiesta di fix non promuove
    assert!(!is_test_failure_resolution("esegui i test playwright"));
    // Senza menzione test
    assert!(!is_test_failure_resolution(
        "risolvi questo errore di compilazione"
    ));
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
        "Esegui i test Playwright e risolvi i fallimenti rilevati",
    );
    assert_eq!(
        intent, "fix_complesso",
        "test + verbo correttivo deve essere promosso a fix_complesso"
    );

    let (intent, _) = classify_intent_with_agentic_promotion("fai funzionare i test Playwright");
    assert_eq!(intent, "fix_complesso");

    // Negativo: solo "scrivi test" resta test (no failure resolution)
    let (intent, _) =
        classify_intent_with_agentic_promotion("scrivi i test unitari per il modulo auth");
    // Nota: classify_intent_local potrebbe ritornare un altro intent qui;
    // l'importante e' che NON sia fix_complesso senza failure resolution.
    assert_ne!(
        intent, "fix_complesso",
        "creazione test senza failure resolution non deve essere promossa"
    );
}

#[test]
fn test_classify_intent_local_file_ops() {
    // Verifiche dei nuovi intent introdotti
    let (intent, _) =
        classify_intent_local("Per favore elimina i file Dockerfile rimasti nel progetto");
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
    let (intent, _) = classify_intent_local("Migra il backend .NET 9 da SQL Server a PostgreSQL");
    assert_eq!(intent, "refactor");
}

#[test]
fn test_classify_intent_local_converti_typescript_va_a_refactor() {
    let (intent, _) = classify_intent_local("Converti questi file da JavaScript a TypeScript");
    assert_eq!(intent, "refactor");
}

#[test]
fn test_classify_intent_local_sostituisci_libreria_va_a_refactor() {
    let (intent, _) =
        classify_intent_local("Sostituisci la libreria axios con fetch nativa in tutti i moduli");
    assert_eq!(intent, "refactor");
}

#[test]
fn test_classify_intent_local_piano_migrazione_va_a_architecture() {
    // Distinzione: PLANNING di migrazione (no codice) → architecture
    let (intent, _) = classify_intent_local(
        "Definisci un piano di migrazione del database da MySQL a PostgreSQL",
    );
    assert_eq!(intent, "architecture");
}

#[test]
fn test_route_model_with_mode_file_ops_approfondita() {
    // Test usa la fallback safe matrix (anthropic claude-sonnet per tutti gli intent rischiosi)
    let m = crate::routing_matrix::RoutingMatrix::fallback_safe();
    let thr = TokenThresholds::defaults();
    let d = route_model_with_mode(
        &m,
        "file_ops",
        1500,
        "approfondita",
        Some("anthropic"),
        &thr,
    );
    assert_eq!(d.provider, "anthropic");
    assert_eq!(d.model, "claude-sonnet-4-6");
}

#[test]
fn test_route_model_with_mode_system_admin_bilanciata() {
    let m = crate::routing_matrix::RoutingMatrix::fallback_safe();
    let thr = TokenThresholds::defaults();
    let d = route_model_with_mode(
        &m,
        "system_admin",
        1500,
        "bilanciata",
        Some("anthropic"),
        &thr,
    );
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
    assert_eq!(
        d.provider, "__no_model__",
        "deve ritornare sentinella, non gpt-4o-mini hardcoded"
    );
    assert_eq!(d.model, "__no_model__");
}

#[test]
fn test_route_model_with_mode_uses_token_thresholds() {
    // Verifica che le soglie token vengano lette dai thresholds passati
    // invece dei valori hardcoded 400/1500/3000.
    let m = crate::routing_matrix::RoutingMatrix::fallback_safe();
    let custom_thr = TokenThresholds {
        chat_breve: 100, // soglia molto bassa: anche 200 token va a media
        chat_media: 200,
        complex_fix: 500, // fix con 600 token va a fix_complesso
    };
    // Con questi thresholds, intent=fix tokens=600 -> fix_complesso
    // (la matrix fallback_safe ha fix_complesso × bilanciata mappato).
    let d = route_model_with_mode(&m, "fix", 600, "bilanciata", None, &custom_thr);
    // fix_complesso × bilanciata -> claude-haiku in fallback_safe matrix
    assert_eq!(d.provider, "anthropic");
    assert!(
        d.model.contains("haiku"),
        "atteso haiku per fix_complesso bilanciata, got {}",
        d.model
    );
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
fn classified(
    intent: &'static str,
    conf: f32,
    candidates: Vec<(&str, f32)>,
    ambig: bool,
) -> ClassifiedIntent {
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
    let (intent, conf) = deterministic_intent_fallback(
        "Crea l'applicazione completa descritta nel file allegato. Implementala e avviala.",
    )
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
    assert!(
        conf > 0.0 && conf < 0.85,
        "confidence media attesa, got {conf}"
    );
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

// ─────────────────────────────────────────────────────────────────
// Test ADR 0023: provider_for_model (model_override da solo -> provider)
// ─────────────────────────────────────────────────────────────────
//
// Usano un pool sqlx isolato (DB temporaneo per test). Creano una tabella
// minima ai_price_catalog con solo le colonne usate dalla query. Idempotenti
// e indipendenti dall'ordine: ogni test ha il proprio DB.

#[sqlx::test]
async fn provider_for_model_modello_noto_ritorna_provider(pool: sqlx::PgPool) {
    sqlx::query(
        "CREATE TABLE ai_price_catalog (
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            is_enabled BOOLEAN NOT NULL DEFAULT true,
            input_cost_per_million_tokens DOUBLE PRECISION
        )",
    )
    .execute(&pool)
    .await
    .expect("create table");

    // Stesso modello su due provider: deve vincere il piu' economico (mistral).
    sqlx::query(
        "INSERT INTO ai_price_catalog (provider, model, is_enabled, input_cost_per_million_tokens)
         VALUES ('openai', 'shared-model', true, 5.0),
                ('mistral', 'shared-model', true, 2.0)",
    )
    .execute(&pool)
    .await
    .expect("insert rows");

    let provider = provider_for_model(&pool, "shared-model").await;
    assert_eq!(
        provider,
        Some("mistral".to_string()),
        "deve scegliere il provider col costo input piu' basso (deterministico)"
    );
}

#[sqlx::test]
async fn provider_for_model_modello_disabilitato_o_ignoto_ritorna_none(pool: sqlx::PgPool) {
    sqlx::query(
        "CREATE TABLE ai_price_catalog (
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            is_enabled BOOLEAN NOT NULL DEFAULT true,
            input_cost_per_million_tokens DOUBLE PRECISION
        )",
    )
    .execute(&pool)
    .await
    .expect("create table");

    // Un modello presente ma disabilitato non deve essere risolto.
    sqlx::query(
        "INSERT INTO ai_price_catalog (provider, model, is_enabled, input_cost_per_million_tokens)
         VALUES ('mistral', 'disabled-model', false, 1.0)",
    )
    .execute(&pool)
    .await
    .expect("insert rows");

    // Modello ignoto -> None (il chiamante fa fallback al routing, regola G).
    assert_eq!(provider_for_model(&pool, "unknown-model").await, None);
    // Modello disabilitato -> None.
    assert_eq!(provider_for_model(&pool, "disabled-model").await, None);
}
