//! Test unitari del modulo orchestrator.

use super::*;

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
        manual_overrides: std::collections::HashSet::new(),
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
fn intent_str_to_static_mappa_code_read_e_agentic_default() {
    // Regressione: prima `code_read` non era mappato -> un code_read dall'LLM
    // cadeva nel fallback. E il fallback neutro `agentic_default` deve essere
    // riconosciuto. Un intent sconosciuto resta None.
    assert_eq!(intent_str_to_static("code_read"), Some("code_read"));
    assert_eq!(
        intent_str_to_static("agentic_default"),
        Some("agentic_default")
    );
    assert_eq!(intent_str_to_static("chat"), Some("chat"));
    assert_eq!(intent_str_to_static("intent_inventato_xyz"), None);
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

// ─────────────────────────────────────────────────────────────────
// Test embedder ONNX in-process (cablaggio embed_text, regola L)
//
// Verifica che NeuralCoreClient::embed_text / embed_text_with_model usino
// l'embedder in-process del NexusBridge (ONNX o HashEmbedder fallback) e NON
// facciano piu' alcuna RPC gRPC verso il brain. Il client e' costruito con
// `disconnected_for_tests()` (endpoint irraggiungibile 127.0.0.1:1): se il
// metodo facesse ancora gRPC, fallirebbe la connessione. Il successo dimostra
// l'assenza di round-trip di rete.
// ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_embed_text_with_model_in_process_no_grpc() {
    // Inizializza il bridge globale (idempotente). Nell'ambiente di test il
    // modello ONNX di norma non e' presente -> HashEmbedder(256) come fallback;
    // in entrambi i casi l'embedding e' in-process, senza brain.
    crate::nexus_bridge::NexusBridge::init_global();

    let client = NeuralCoreClient::disconnected_for_tests();

    // embed_text_with_model: label coerente + vettore non vuoto, senza gRPC.
    let (label, vector) = client
        .embed_text_with_model("", "verifica embedding in-process")
        .await
        .expect("embed_text_with_model deve riuscire in-process (zero gRPC)");
    // Model vuoto -> label canonico, identico a quello che il brain ritornava:
    // gli hash di indicizzazione restano validi (nessun reindex).
    assert_eq!(label, "all-MiniLM-L6-v2");
    assert!(!vector.is_empty(), "il vettore non deve essere vuoto");

    // embed_text (wrapper): stesso vettore, solo il valore.
    let vector2 = client
        .embed_text("", "verifica embedding in-process")
        .await
        .expect("embed_text deve riuscire in-process (zero gRPC)");
    assert_eq!(
        vector, vector2,
        "embed_text e embed_text_with_model devono produrre lo stesso vettore"
    );

    // Model esplicito -> il label richiesto viene propagato (parita' con la
    // logica dell'handler /api/embed).
    let (label_explicit, _v) = client
        .embed_text_with_model("custom-embedder", "x")
        .await
        .expect("embed con model esplicito deve riuscire");
    assert_eq!(label_explicit, "custom-embedder");
}

// ─────────────────────────────────────────────────────────────────
// Test cablaggio gateway (generate_completion / generate_agent_turn, regola L)
//
// Verifica che i due metodi NON facciano piu' alcuna RPC gRPC verso il brain.
// Il client e' costruito con `disconnected_for_tests()` (canale tonic verso
// 127.0.0.1:1, irraggiungibile): se i metodi facessero ancora gRPC al brain,
// l'errore sarebbe un transport-error tonic ("transport error" / "connection
// refused"). Con bridge globale inizializzato SENZA pool DB, i metodi falliscono
// invece a MONTE (risoluzione gateway dal pool del bridge), dimostrando che il
// percorso e' quello gateway-in-process e non piu' il gRPC al brain.
// ─────────────────────────────────────────────────────────────────

/// Asserisce che un esito di `generate_completion`/`generate_agent_turn` non
/// provenga MAI dal gRPC verso il brain. Due scenari ammessi (entrambi senza
/// brain): (a) `Err` dalla risoluzione gateway via bridge (bridge senza pool);
/// (b) `Ok` con Value d'errore costruito dal mapping quando il gateway HTTP non
/// risponde (bridge con pool da altri test, gateway down). In nessun caso deve
/// comparire un transport-error tonic verso il brain.
fn assert_no_brain_grpc(outcome: anyhow::Result<serde_json::Value>) {
    match outcome {
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            assert!(
                msg.contains("gateway") || msg.contains("pool db") || msg.contains("bridge"),
                "errore atteso dal cablaggio gateway, non dal gRPC al brain: {msg}"
            );
            assert!(
                !msg.contains("transport error") && !msg.contains("tcp connect"),
                "nessun errore di trasporto gRPC verso il brain deve comparire: {msg}"
            );
        }
        Ok(v) => {
            // Ramo (b): il mapping ha intercettato l'errore HTTP del gateway e ha
            // prodotto la forma Value d'errore storica del brain (content
            // "[Error: ...]" + error/error_class). Non c'e' stato gRPC al brain.
            let content = v["content"].as_str().unwrap_or_default();
            assert!(
                content.starts_with("[Error:"),
                "atteso Value d'errore dal mapping gateway (gateway HTTP down): {v}"
            );
            assert!(!v["error"].is_null(), "il Value d'errore deve avere `error`: {v}");
        }
    }
}

#[tokio::test]
async fn test_generate_completion_no_grpc_al_brain() {
    crate::nexus_bridge::NexusBridge::init_global();
    let client = NeuralCoreClient::disconnected_for_tests();
    let outcome = client
        .generate_completion("anthropic", "claude-x", "ping")
        .await;
    assert_no_brain_grpc(outcome);
}

#[tokio::test]
async fn test_generate_agent_turn_no_grpc_al_brain() {
    crate::nexus_bridge::NexusBridge::init_global();
    let client = NeuralCoreClient::disconnected_for_tests();
    let outcome = client
        .generate_agent_turn(
            "openai",
            "gpt-x",
            "[{\"role\":\"user\",\"content\":\"hi\"}]",
            "[]",
            256,
            "",
        )
        .await;
    assert_no_brain_grpc(outcome);
}
