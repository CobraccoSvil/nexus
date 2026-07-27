//! Test unitari del modulo orchestrator.

use super::*;
// Schema `ai_price_catalog` dal punto unico condiviso (regola L).
use crate::orchestrator::provider_choice::InvalidProviderOverrideMode;
use crate::test_support::create_ai_price_catalog_table;

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
fn needs_catalog_fallback_include_no_model_e_provider_sani_no() {
    // Regressione (fix routing coding, regola H): la sentinella __no_model__ deve
    // SEMPRE innescare il fallback tier-aware dal catalog. Prima il ramo
    // __no_model__ in resolve_agent_provider scavalcava il catalog e cadeva su un
    // default per-provider tier-blind (google/gemini-flash, modello light) per i
    // task di coding heavy quando anthropic+openai erano in cooldown.
    assert!(
        needs_catalog_fallback("__no_model__"),
        "__no_model__ deve innescare il fallback catalog"
    );
    // Un provider mai messo in cooldown (nome univoco per non collidere con lo
    // stato globale di altri test) e' servibile direttamente: nessun fallback.
    assert!(
        !needs_catalog_fallback("__test_healthy_provider_ncf"),
        "un provider sano non deve innescare il fallback catalog"
    );
}

#[sqlx::test]
async fn coding_fallback_resta_su_tier_heavy_non_su_google_light(pool: sqlx::PgPool) {
    // Regressione (fix routing coding, regola H): con i provider forti di coding
    // (anthropic, openai) non disponibili, il fallback per un task heavy deve
    // scegliere un modello HEAVY tool-capable di un provider sano, MAI degradare a
    // un google/gemini-flash LIGHT solo perche' e' il piu' economico e featured.
    // E' l'invariante su cui poggia il fix: select_agentic_model, consultato dal
    // ramo __no_model__/cooldown di resolve_agent_provider, rispetta il tier
    // dell'intent (heavy) ed esclude per costruzione i modelli light.
    create_ai_price_catalog_table(&pool).await;
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, performance_tier, capabilities, is_featured, input_cost_per_million_tokens) VALUES \
         ('anthropic', 'claude-heavy', true, true, 'disable_for_tools', 'heavy', '[\"reasoning\"]', true,  3.0), \
         ('openai',    'gpt-heavy',    true, true, 'disable_for_tools', 'heavy', '[\"reasoning\"]', true,  2.0), \
         ('deepseek',  'reasoner',     true, true, 'none',              'heavy', '[\"reasoning\"]', false, 0.5), \
         ('google',    'gemini-flash', true, true, 'none',              'light', '[\"reasoning\"]', true,  0.1)",
    )
    .execute(&pool)
    .await
    .expect("insert catalog");

    // Simula anthropic+openai indisponibili (cooldown) via exclude_providers,
    // con lo stesso ordine usato dal fallback di resolve_agent_provider.
    // SERVIZIO UNICO: Degrade da 'heavy' — la catena scende heavy->high->medium->
    // light, ma il corto-circuito si ferma al PRIMO tier con candidati: 'heavy' ha
    // deepseek/reasoner sano, quindi google/gemini-flash (light) non viene mai
    // raggiunto benche' sia il piu' economico e featured.
    let out = crate::orchestrator::model_service::select_model(
        &pool,
        &crate::orchestrator::model_service::ModelRequest::agentic("heavy")
            .capability(Some("reasoning"))
            .exclude(&["anthropic".to_string(), "openai".to_string()]),
    )
    .await
    .map(|c| (c.provider, c.model));
    assert_eq!(out, Ok(("deepseek".to_string(), "reasoner".to_string())));
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
        classifier_resolved: true,
        complexity: "medium".to_string(),
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
// Usano un pool sqlx isolato (DB temporaneo per test). La tabella
// ai_price_catalog e' creata dal punto unico condiviso (regola L); la query
// `provider_for_model` ne usa solo provider/model/is_enabled/costo. Idempotenti
// e indipendenti dall'ordine: ogni test ha il proprio DB.

#[sqlx::test]
async fn provider_for_model_modello_noto_ritorna_provider(pool: sqlx::PgPool) {
    create_ai_price_catalog_table(&pool).await;

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
    create_ai_price_catalog_table(&pool).await;

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
            assert!(
                !v["error"].is_null(),
                "il Value d'errore deve avere `error`: {v}"
            );
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

/// REGRESSIONE dell'incidente del consiglio (2026-07-15): il purpose delle figure
/// senior chiedeva `tier=heavy`; openai e anthropic — gli UNICI provider che
/// l'euristica del catalog ammette in quel tier insieme a google — sono finiti
/// insieme in cooldown billing (`credit_balance_too_low`), e l'unico heavy sano
/// restante era escluso dal gate pre-GA. Pool VUOTO -> `NoCapableModel` -> 2
/// figure su 6 morte in 6.9s, MENTRE modelli sani stavano un gradino sotto.
///
/// La chat utente non moriva: `route_model_from_catalog` usava gia'
/// `agentic_tier_chain`. Il purpose passava `&[tier]` (catena di UN elemento):
/// stessa domanda, due risposte diverse (odore regola L).
///
/// Invariante: quando il tier richiesto e' esaurito, la selezione DEGRADA al
/// primo tier con un candidato sano invece di fallire.
#[sqlx::test]
async fn purpose_degrada_quando_il_tier_richiesto_e_esaurito(pool: sqlx::PgPool) {
    create_ai_price_catalog_table(&pool).await;
    // Il catalog dell'incidente, in piccolo: gli unici 'heavy' sono dei due
    // provider senza credito; il modello sano vive nel tier sotto.
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, performance_tier, capabilities, input_cost_per_million_tokens) VALUES \
         ('openai',    'gpt-heavy',       true, true, 'disable_for_tools', 'heavy',  '[\"reasoning\"]', 2.0), \
         ('anthropic', 'claude-heavy',    true, true, 'disable_for_tools', 'heavy',  '[\"reasoning\"]', 3.0), \
         ('deepseek',  'deepseek-v4-pro', true, true, 'none',              'high',   '[\"reasoning\"]', 0.5)",
    )
    .execute(&pool)
    .await
    .expect("insert catalog");

    // openai + anthropic esclusi = il cooldown billing dell'incidente.
    let out = crate::orchestrator::model_service::select_model(
        &pool,
        &crate::orchestrator::model_service::ModelRequest::agentic("heavy")
            .capability(Some("reasoning"))
            .exclude(&["openai".to_string(), "anthropic".to_string()]),
    )
    .await
    .map(|c| (c.provider, c.model, c.effective_tier));

    let (provider, model, effective_tier) = out.expect(
        "il tier 'heavy' e' esaurito ma 'high' ha un modello SANO: la selezione \
         deve degradare, non ritornare None (era l'incidente: NoCapableModel con \
         19 modelli sani un gradino sotto)",
    );
    assert_eq!(
        (provider.as_str(), model.as_str()),
        ("deepseek", "deepseek-v4-pro"),
        "deve scegliere il modello sano del tier immediatamente inferiore"
    );
    // Il tier effettivo e' un DATO (regola M): permette di DICHIARARE la
    // degradazione invece di lasciarla dedurre dal nome del modello.
    assert_eq!(
        effective_tier.as_deref(),
        Some("high"),
        "il tier effettivo deve tornare al chiamante per poter dichiarare la degradazione"
    );
}

/// Complemento del test sopra: quando il tier richiesto HA un candidato sano,
/// la degradazione NON deve scattare (niente ripieghi gratuiti su modelli piu'
/// deboli, che sarebbe il difetto opposto).
#[sqlx::test]
async fn purpose_resta_sul_tier_richiesto_quando_e_disponibile(pool: sqlx::PgPool) {
    create_ai_price_catalog_table(&pool).await;
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, performance_tier, capabilities, input_cost_per_million_tokens) VALUES \
         ('google',   'gemini-heavy',    true, true, 'disable_for_tools', 'heavy', '[\"reasoning\"]', 2.0), \
         ('deepseek', 'deepseek-v4-pro', true, true, 'none',              'high',  '[\"reasoning\"]', 0.1)",
    )
    .execute(&pool)
    .await
    .expect("insert catalog");

    let out = crate::orchestrator::model_service::select_model(
        &pool,
        &crate::orchestrator::model_service::ModelRequest::agentic("heavy")
            .capability(Some("reasoning")),
    )
    .await
    .map(|c| (c.provider, c.model, c.effective_tier));

    let (provider, model, effective_tier) = out.expect("il tier heavy ha un modello sano");
    assert_eq!(
        (provider.as_str(), model.as_str(), effective_tier.as_deref()),
        ("google", "gemini-heavy", Some("heavy")),
        "col tier richiesto disponibile NON si degrada, benche' 'high' costi 20 volte meno"
    );
}

/// Il confine della degradazione: COL PIN non si degrada. Il chiamante ha
/// un'alternativa migliore (togliere il pin e prendere il tier giusto altrove),
/// quindi il pin deve cedere il PROVIDER, mai la qualita'. Senza questo confine
/// un pin su un provider con soli modelli deboli aggancerebbe il run a un tier
/// inferiore invece di lasciar vincere il modello giusto di un altro provider
/// (regressione di `pin_non_capable_degrada_al_purpose_normale`).
#[sqlx::test]
async fn col_pin_non_si_degrada_il_tier(pool: sqlx::PgPool) {
    create_ai_price_catalog_table(&pool).await;
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, performance_tier, capabilities, input_cost_per_million_tokens) VALUES \
         ('mistral',  'mistral-medium', true, true, 'none', 'medium', '[\"code\"]', 1.0), \
         ('deepseek', 'deepseek-flash', true, true, 'none', 'light',  '[\"code\"]', 0.5)",
    )
    .execute(&pool)
    .await
    .expect("insert catalog");

    // Pin su deepseek, che NON ha un 'medium': deve ritornare None (il chiamante
    // ritentera' senza pin e prendera' mistral-medium), NON degradare a
    // deepseek-flash pur di onorare il pin.
    let out = crate::orchestrator::model_service::select_model(
        &pool,
        &crate::orchestrator::model_service::ModelRequest::agentic("medium")
            .capability(Some("code"))
            .pinned("deepseek"),
    )
    .await
    .map(|c| (c.provider, c.model))
    .ok();
    assert_eq!(
        out, None,
        "col pin il tier non si degrada: il provider pinnato non ha il tier \
         richiesto -> None, cosi' il chiamante abbandona il pin e prende il \
         modello del tier GIUSTO da un altro provider"
    );
}

/// REGRESSIONE del ramo NON-agentico (residuo del fix 6006084f, trovato dal
/// censimento dei punti unici). La degradazione era stata messa dentro
/// `if requires_tool_use { .. }`: i 41 purpose non-agentici (vision, chat,
/// embedding) restavano col difetto dell'incidente.
///
/// Il caso concreto e' utente-visibile: col gate vision (core.rs:716) un turno
/// con un'immagine allegata, se il tier vision e' esaurito, NON trova modello,
/// logga "nessun modello vision disponibile" e prosegue COL MODELLO CIECO —
/// mentre lo stesso turno in modalita' agentica degraderebbe e vedrebbe
/// l'immagine. La capability resta un filtro: il ripiego e' sempre un modello
/// che sa fare la cosa richiesta, solo meno capace.
#[sqlx::test]
async fn purpose_non_agentico_degrada_e_resta_capace(pool: sqlx::PgPool) {
    create_ai_price_catalog_table(&pool).await;
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, supports_vision, performance_tier, input_cost_per_million_tokens) VALUES \
         ('openai',   'gpt-vision-heavy', true, false, true,  'heavy',  2.0), \
         ('google',   'gemini-vision',    true, false, true,  'medium', 0.5), \
         ('deepseek', 'deepseek-cieco',   true, false, false, 'medium', 0.1)",
    )
    .execute(&pool)
    .await
    .expect("insert catalog");

    // openai escluso = il tier vision 'heavy' e' esaurito.
    let out = crate::orchestrator::model_service::select_model(
        &pool,
        // NON agentico: e' il ramo che restava indietro
        &crate::orchestrator::model_service::ModelRequest::non_agentic("heavy")
            .capability(Some("vision"))
            .exclude(&["openai".to_string()]),
    )
    .await
    .map(|c| (c.provider, c.model, c.effective_tier));

    let (provider, model, effective_tier) = out.expect(
        "il tier vision 'heavy' e' esaurito ma 'medium' ha un modello vision SANO: \
         deve degradare, non lasciare il turno senza occhi",
    );
    assert_eq!(
        (provider.as_str(), model.as_str()),
        ("google", "gemini-vision"),
        "deve degradare su un modello che VEDE ancora, mai su deepseek-cieco \
         (piu' economico ma senza supports_vision)"
    );
    assert_eq!(
        effective_tier.as_deref(),
        Some("medium"),
        "anche il ramo non-agentico deve dire il tier effettivo, per poter \
         dichiarare la degradazione (regola M)"
    );
}

// ─────────────────────────────────────────────────────────────────
// Preferenza e pin del provider (dropdown + pulsante "Forza" del composer).
//
// I test attraversano la STESSA strada della produzione (regola O), tutta:
// il corpo JSON della POST della chat -> `SendChatMessageRequest` (serde) ->
// `ProviderOverrideMode::try_parse` -> `ProviderChoice::resolve` ->
// `build_chat_gateway_call`, cioe' esattamente i passaggi che fanno l'handler e
// `execute_via_gateway`; la configurazione nasce da
// `RoutingConfig::from_settings`, il produttore vero.
//
// Perche' partire dal WIRE e non dall'enum: il difetto che questi test
// presidiano e' che il pulsante "Forza" non arrivava MAI al backend. Un test che
// costruisse la variante pinnata a mano non vedrebbe la differenza —
// resterebbe verde anche se il campo sparisse dal corpo della richiesta o serde
// lo leggesse con un altro nome. E' il modo in cui il difetto (pin mai
// valorizzato, modello prefissato col nome del provider) e' sopravvissuto finora.
// ─────────────────────────────────────────────────────────────────

/// Configurazione di routing dal produttore vero, non da uno struct literal.
fn routing_bilanciata() -> RoutingConfig {
    RoutingConfig::from_settings(&[SettingValueRow {
        key: "nexus_behavior_mode".to_string(),
        value: "bilanciata".to_string(),
    }])
}

/// ORACOLO indipendente: a chi appartiene questo modello, secondo i DATI della
/// matrice? Non lo chiede alla logica di risoluzione (che e' cio' che stiamo
/// misurando), guarda le associazioni provider->modello che la matrice porta.
fn provider_del_modello(matrix: &crate::routing_matrix::RoutingMatrix, model: &str) -> Vec<String> {
    let mut proprietari: Vec<String> = matrix
        .default_models
        .iter()
        .filter(|(_, m)| m.as_str() == model)
        .map(|(p, _)| p.clone())
        .chain(
            matrix
                .by_intent_mode
                .values()
                .filter(|(_, m)| m.as_str() == model)
                .map(|(p, _)| p.clone()),
        )
        .collect();
    proprietari.sort();
    proprietari.dedup();
    proprietari
}

/// La scelta di provider come nasce IN PRODUZIONE: dal corpo JSON della POST
/// `/api/chat/sessions/:id/messages`. Attraversa i tre passaggi veri —
/// deserializzazione della richiesta, parse dell'identificatore canonico,
/// risoluzione col provider che la sessione ricorda — invece di costruire
/// l'enum a mano.
///
/// `session_preferred` e' cio' che `chat_sessions.preferred_provider` porta per
/// quella sessione (None = nessuna preferenza persistita).
fn scelta_dal_wire(wire: &str, session_preferred: Option<&str>) -> ProviderChoice {
    let body: crate::chat_messages::SendChatMessageRequest =
        serde_json::from_str(wire).expect("corpo della POST /messages deserializzabile");
    let mode = ProviderOverrideMode::try_parse(body.provider_override_mode.as_deref())
        .expect("identificatore canonico di provider_override_mode");
    ProviderChoice::resolve(body.provider_override.as_deref(), mode, session_preferred)
}

/// Corpo della POST come lo manda il composer: provider scelto dal dropdown e
/// stato del pulsante "Forza" tradotto nel suo identificatore canonico.
fn wire_chat(provider: &str, mode: &str) -> String {
    format!(
        r#"{{"content":"riassumi il file","providerOverride":"{provider}","providerOverrideMode":"{mode}"}}"#
    )
}

/// Gli ingressi del caso misurato il 27/07/2026: l'utente sceglie deepseek
/// mentre il routing aveva suggerito un modello di google (nel ledger di quel
/// giorno era `gemini-3.1-flash-lite`; qui si usa un modello google che la
/// matrice di test conosce, cosi' l'oracolo puo' dire A CHI appartiene).
fn spec_deepseek<'a>(
    routing: &'a RoutingConfig,
    matrix: &'a crate::routing_matrix::RoutingMatrix,
    provider_choice: &'a ProviderChoice,
    forced_model: Option<&'a str>,
) -> ChatCallSpec<'a> {
    ChatCallSpec {
        routing,
        matrix,
        intent: "fix",
        provider_choice,
        forced_model,
        suggested_provider: Some("google"),
        suggested_model: Some("gemini-2.5-flash"),
        composed_prompt: "riassumi il file",
        token_budget: 4096,
        tenant_id: "progetto-x",
        user_id: "utente-y",
        request_id: "run-1".to_string(),
    }
}

#[test]
fn identificatori_canonici_di_provider_override_mode() {
    // Regola N: un solo identificatore inglese per stato, niente sinonimi.
    assert_eq!(
        ProviderOverrideMode::try_parse(Some("preferred")),
        Ok(ProviderOverrideMode::Preferred)
    );
    assert_eq!(
        ProviderOverrideMode::try_parse(Some("pinned")),
        Ok(ProviderOverrideMode::Pinned)
    );
    for sinonimo in [
        "forza", "force", "forced", "hard", "pin", "Pinned", "PREFERRED", "soft", "auto",
    ] {
        assert_eq!(
            ProviderOverrideMode::try_parse(Some(sinonimo)),
            Err(InvalidProviderOverrideMode),
            "'{sinonimo}' non e' un identificatore canonico: accettarlo creerebbe \
             il secondo vocabolario che la regola N vieta"
        );
    }
    // Campo assente o vuoto: il vincolo PIU' DEBOLE. E' lo stato di ogni
    // superficie che il pulsante "Forza" non ce l'ha (resend, riattivazione,
    // worker, client vecchi): nessuna deve ereditare un vincolo duro.
    assert_eq!(
        ProviderOverrideMode::try_parse(None),
        Ok(ProviderOverrideMode::Preferred)
    );
    assert_eq!(
        ProviderOverrideMode::try_parse(Some("  ")),
        Ok(ProviderOverrideMode::Preferred)
    );
}

#[test]
fn pin_duro_dal_wire_viaggia_come_pin_e_il_modello_non_e_prefissato() {
    // Due difetti in uno. Il primo: il provider scelto finiva nel NOME del
    // modello ("deepseek/coder-large") e `pin_provider` restava None, quindi il
    // gateway re-instradava per policy e rispondeva con un altro fornitore.
    // Il secondo: il pulsante "Forza" non arrivava al backend, quindi il pin —
    // quando ha cominciato a funzionare — sarebbe scattato per la SOLA selezione
    // dal dropdown. Qui il pin c'e' perche' il corpo della richiesta lo dichiara.
    let routing = routing_bilanciata();
    let matrix = crate::routing_matrix::RoutingMatrix::fallback_safe();
    let scelta = scelta_dal_wire(&wire_chat("deepseek", "pinned"), None);
    let call = build_chat_gateway_call(spec_deepseek(&routing, &matrix, &scelta, None));

    assert_eq!(
        call.request.pin_provider.as_deref(),
        Some("deepseek"),
        "il comando dell'utente deve viaggiare come PIN: e' il campo con cui il \
         gateway esegue QUEL provider senza re-instradare"
    );
    assert!(
        !call.request.model.contains('/'),
        "il modello non va prefissato col provider: il prefisso distrugge la \
         risoluzione e il fornitore rifiuta il nome letterale (model={})",
        call.request.model
    );
    assert_eq!(
        call.request.model, "deepseek-chat",
        "col pin il gateway non risolve alias: deve ricevere un modello concreto"
    );
}

#[test]
fn provider_pinnato_e_modello_libero_danno_una_coppia_dello_stesso_provider() {
    // Il difetto nel ledger: la prenotazione portava (deepseek,
    // gemini-3.1-flash-lite) — un modello di google prenotato su deepseek —
    // perche' il modello suggerito scavalcava il provider forzato.
    let routing = routing_bilanciata();
    let matrix = crate::routing_matrix::RoutingMatrix::fallback_safe();
    let scelta = scelta_dal_wire(&wire_chat("deepseek", "pinned"), None);
    let call = build_chat_gateway_call(spec_deepseek(&routing, &matrix, &scelta, None));

    assert_eq!(call.ledger_provider, "deepseek");
    assert_eq!(
        call.ledger_model, call.request.model,
        "la riga di prenotazione deve portare la stessa coppia che si sta per \
         chiedere davvero"
    );
    let proprietari = provider_del_modello(&matrix, &call.ledger_model);
    assert!(
        !proprietari.is_empty(),
        "modello sconosciuto alla matrice: {}",
        call.ledger_model
    );
    assert_eq!(
        proprietari,
        vec!["deepseek".to_string()],
        "il modello prenotato deve appartenere al provider forzato, non a quello \
         suggerito dal routing (modello={})",
        call.ledger_model
    );
}

#[test]
fn provider_pinnato_e_modello_entrambi_scelti_viaggiano_intatti() {
    let routing = routing_bilanciata();
    let matrix = crate::routing_matrix::RoutingMatrix::fallback_safe();
    let scelta = scelta_dal_wire(&wire_chat("deepseek", "pinned"), None);
    let call = build_chat_gateway_call(spec_deepseek(
        &routing,
        &matrix,
        &scelta,
        Some("deepseek-reasoner"),
    ));

    assert_eq!(call.request.pin_provider.as_deref(), Some("deepseek"));
    assert_eq!(call.request.model, "deepseek-reasoner");
    assert_eq!(call.ledger_model, "deepseek-reasoner");
    assert_eq!(call.ledger_provider, "deepseek");
}

#[test]
fn la_sola_preferenza_non_pinna_e_conserva_il_fallback() {
    // LA DECISIONE: il dropdown senza "Forza" e' una preferenza, non un ordine.
    // Il provider scelto entra come suggerimento (decide da dove parte il
    // routing e su cosa prenota il ledger) ma la richiesta NON porta il pin:
    // il gateway resta libero di instradare e di ripiegare su un altro
    // fornitore, che e' cio' che i due tooltip del composer promettono.
    let routing = routing_bilanciata();
    let matrix = crate::routing_matrix::RoutingMatrix::fallback_safe();
    let scelta = scelta_dal_wire(&wire_chat("deepseek", "preferred"), None);
    let call = build_chat_gateway_call(spec_deepseek(&routing, &matrix, &scelta, None));

    assert!(
        call.request.pin_provider.is_none(),
        "con la sola preferenza nessun pin: col pin il gateway va in strict, \
         chain di un solo provider e nessun fallback cross-provider — \
         l'opposto di cio' che il tooltip dichiara (pin={:?})",
        call.request.pin_provider
    );
    assert_eq!(
        call.request.model,
        crate::nexus_gateway::intent_to_alias("fix", "bilanciata", None),
        "senza pin si manda l'ALIAS logico: e' quello che il gateway sa risolvere \
         per ciascun provider della chain, ed e' cio' che tiene in vita il fallback"
    );
    assert_eq!(
        call.ledger_provider, "deepseek",
        "la preferenza vale come suggerimento: il ledger prenota da li'"
    );
    assert_eq!(
        call.ledger_model, "deepseek-chat",
        "coerenza della coppia prenotata: il modello e' del provider preferito, \
         non quello suggerito dal routing (google)"
    );
}

#[test]
fn la_preferenza_di_sessione_da_sola_non_produce_un_pin() {
    // DIFETTO 2. `chat_sessions.preferred_provider` viene scritto dal solo
    // cambio del dropdown e sopravvive al refresh: se il pin si ereditasse da
    // li', ogni messaggio successivo — anche inviato da una superficie che il
    // pulsante "Forza" non ce l'ha — nascerebbe vincolato, e una sessione
    // ripresa altrove porterebbe un vincolo invisibile. Se poi quel provider
    // entra in cooldown, la sessione resta bloccata senza spiegazione.
    let routing = routing_bilanciata();
    let matrix = crate::routing_matrix::RoutingMatrix::fallback_safe();
    // Corpo SENZA scelta di provider (il caso di ogni superficie che non ha il
    // dropdown), sessione che ricorda deepseek.
    let scelta = scelta_dal_wire(r#"{"content":"riassumi il file"}"#, Some("deepseek"));
    let call = build_chat_gateway_call(spec_deepseek(&routing, &matrix, &scelta, None));

    assert_eq!(
        scelta.provider(),
        Some("deepseek"),
        "la preferenza persiste: e' il provider da cui si riparte"
    );
    assert!(
        call.request.pin_provider.is_none(),
        "il pin NON si eredita: un vincolo duro vale per la richiesta in cui lo \
         si da' (pin={:?})",
        call.request.pin_provider
    );
}

#[test]
fn il_modo_pinned_senza_provider_non_pinna_il_ricordo_della_sessione() {
    // Un client che manda il modo ma non il provider (o lo manda vuoto) non
    // deve poter pinnare cio' che la sessione ricorda: il modo qualifica il
    // provider della RICHIESTA, non un provider qualsiasi che si trovi in giro.
    let scelta = scelta_dal_wire(
        r#"{"content":"x","providerOverride":"","providerOverrideMode":"pinned"}"#,
        Some("deepseek"),
    );
    assert_eq!(scelta.provider(), Some("deepseek"));
    assert_eq!(
        scelta.pinned_provider(),
        None,
        "il modo senza provider non pinna il ricordo della sessione"
    );
    assert_eq!(scelta.label(), "preferred");
}

#[test]
fn senza_scelta_dell_utente_resta_l_alias_e_nessun_pin() {
    // Dropdown su "Auto" e nessuna preferenza di sessione: decide il routing.
    let routing = routing_bilanciata();
    let matrix = crate::routing_matrix::RoutingMatrix::fallback_safe();
    let scelta = scelta_dal_wire(r#"{"content":"riassumi il file"}"#, None);
    let call = build_chat_gateway_call(spec_deepseek(&routing, &matrix, &scelta, None));

    assert_eq!(scelta, ProviderChoice::Auto);
    assert!(
        call.request.pin_provider.is_none(),
        "senza scelta nessun pin: il gateway deve poter instradare"
    );
    assert_eq!(
        call.request.model,
        crate::nexus_gateway::intent_to_alias("fix", "bilanciata", None),
        "senza pin si manda l'alias, non un modello concreto"
    );
    assert_eq!(call.ledger_provider, "google");
    assert_eq!(call.ledger_model, "gemini-2.5-flash");
}

#[test]
fn il_fallimento_col_provider_forzato_lo_dice_all_utente() {
    // L'errore arriva dal PRODUTTORE vero (`GatewayHttpError::from_response`),
    // non da una stringa fabbricata: e' li' che status e codice vengono
    // estratti, ed e' quel tipo che il confine HTTP della chat cerca.
    let body = r#"{"error":"il provider pinnato ha rifiutato","code":"PROVIDER_ERROR"}"#;
    let err: anyhow::Error = crate::nexus_gateway::GatewayHttpError::from_response(
        reqwest::StatusCode::BAD_REQUEST,
        body.to_string(),
    )
    .into();

    let rendered = rendered_chat_gateway_error(&err, Some("deepseek"));
    assert!(
        rendered.message.contains("forzato") && rendered.message.contains("deepseek"),
        "col pin il gateway non ripiega su altri fornitori: l'utente deve leggere \
         che il provider era forzato ({})",
        rendered.message
    );
    assert!(
        !rendered.message.contains(body),
        "il body grezzo non deve finire in chat: {}",
        rendered.message
    );
    assert!(
        rendered.detail.contains("PROVIDER_ERROR"),
        "il dettaglio tecnico non si perde: {}",
        rendered.detail
    );

    // La stessa strada della produzione fino al confine HTTP: execute_via_gateway
    // ritorna la resa come errore tipizzato e `chat_messages::run` la rilegge con
    // `rendered_from_error`. Se quel giro la degradasse, l'utente tornerebbe a
    // leggere una frase generica.
    let propagato = anyhow::Error::new(rendered.clone());
    let al_confine = crate::nexus_gateway::rendered_from_error(&propagato);
    assert_eq!(al_confine.message, rendered.message);
    assert_eq!(al_confine.code, rendered.code);
    assert_eq!(al_confine.detail, rendered.detail);
}

#[test]
fn senza_forzatura_la_resa_non_parla_di_provider_forzati() {
    let err: anyhow::Error = crate::nexus_gateway::GatewayHttpError::from_response(
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        r#"{"error":"boom","code":"PROVIDER_ERROR"}"#.to_string(),
    )
    .into();
    let rendered = rendered_chat_gateway_error(&err, None);
    assert!(
        !rendered.message.contains("forzato"),
        "senza pin la frase non deve inventare una forzatura: {}",
        rendered.message
    );
}
