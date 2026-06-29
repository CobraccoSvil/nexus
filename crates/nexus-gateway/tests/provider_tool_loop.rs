//! Test di PARITA' PROVIDER sul mini tool-loop a 2 turni (integration test).
//!
//! Scopo: esercitare OGNI provider LLM del gateway su uno stesso scenario
//! tool-call -> tool-result -> risposta finale, cosi' i quirk specifici di ogni
//! provider (configurazione thinking, formato tool-call, round-trip di
//! `reasoning`/`thinking_signature`) emergano in CI invece che in produzione
//! uno alla volta. E' la rete di sicurezza che cattura le regressioni gia' viste
//! nel repo, ad esempio:
//!   - DeepSeek: HTTP 400 "Thinking mode does not support tool_choice" al turno 1,
//!     e "reasoning_content in the thinking mode must be passed back" al turno 2
//!     (gate thinking+tool e ri-passaggio `reasoning`);
//!   - Anthropic: HTTP 400 se al turno 2 manca la `signature` del blocco thinking
//!     (round-trip `thinking_signature`);
//!   - Google: `functionDeclarations` mancanti / `thinkingBudget` non a zero /
//!     `thoughtSignature` non re-inviata;
//!   - Mistral: HTTP 422 "last role assistant" / trailing assistant nella history.
//!
//! PARAMETRIZZAZIONE: un singolo `async fn esegui_tool_loop(provider, modello)`
//! contiene tutta la logica dei 2 turni e le asserzioni; ogni provider ha un
//! `#[tokio::test]` sottile che costruisce il proprio provider concreto e lo
//! invoca. Cosi' il riquadro di test e' un solo punto unico (regola L) e i 5
//! test sono solo l'innesto provider-specifico.
//!
//! SKIP NON-FALLIMENTARE: ogni test legge la propria API key da env var dedicata
//! (`DEEPSEEK_API_KEY`, `GOOGLE_API_KEY`/`GEMINI_API_KEY`, `ANTHROPIC_API_KEY`,
//! `MISTRAL_API_KEY`, `OPENAI_API_KEY` — gli stessi nomi dei `.env*.example` del
//! repo). Se la chiave non e' presente in env, il test stampa `skip <provider>:
//! no key` e ritorna SENZA panic. Cosi' la CI senza segreti passa verde, mentre
//! in locale (o in un job con i segreti) i test girano davvero contro le API.
//! Niente DB: le chiavi runtime stanno nel DB (`settings`), ma un integration
//! test non deve dipendere da un DB popolato di segreti — la fonte canonica per
//! la CI sono le env var.
//!
//! Regola F: niente segreti nel log (si stampa solo il nome provider sullo skip).
//! Regola G: i nomi modello qui sono SOLO scelte di test economiche/valide, non
//! configurazione di business; il gateway in produzione li legge dal DB.

use std::env;

use reqwest::Client;

use nexus_gateway::provider::LlmProvider;
use nexus_gateway::providers::{
    AnthropicProvider, DeepSeekProvider, GoogleProvider, MistralProvider, OpenAiProvider,
};
use nexus_gateway::types::{
    LlmContentBlock, LlmMessage, LlmRequest, LlmResponse, LlmToolCall, LlmToolDefinition,
    MessageContent, RequestMetadata, ToolFunctionDef,
};

/// Nome del tool fittizio usato in entrambi i turni. Oggetto parametri vuoto ma
/// valido (JSON Schema `object` senza proprieta'): tutti i provider lo accettano.
const TOOL_NAME: &str = "get_current_time";

/// Messaggio user del turno 1: chiede esplicitamente di usare lo strumento, per
/// massimizzare la probabilita' di una tool-call anche senza force-action.
const USER_PROMPT: &str = "Dimmi l'ora corrente usando lo strumento.";

/// Risultato fittizio del tool, iniettato al turno 2 come messaggio `tool`.
const TOOL_RESULT: &str = "Sono le 14:30";

/// Costruisce i metadati di tracciamento richiesti dalla request. Valori di test:
/// nessun segreto, tier 0 (pubblico), feature dedicata cosi' eventuali log/ledger
/// sono riconoscibili come provenienti da questo test.
fn metadata() -> RequestMetadata {
    RequestMetadata {
        tenant_id: "test-tenant".to_string(),
        user_id: "test-user".to_string(),
        // request_id univoco per run: niente id hardcoded condiviso fra esecuzioni.
        request_id: uuid::Uuid::new_v4().to_string(),
        sensitivity_tier: 0,
        feature: "provider-tool-loop-test".to_string(),
    }
}

/// L'unico tool offerto al modello: `get_current_time` senza parametri.
/// `parameters` e' un JSON Schema `object` vuoto ma valido (alcuni provider
/// rifiutano uno schema assente o malformato).
fn tool_definition() -> LlmToolDefinition {
    LlmToolDefinition {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: TOOL_NAME.to_string(),
            description: Some("Restituisce l'ora corrente.".to_string()),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            strict: None,
        },
    }
}

/// Richiesta del TURNO 1: messaggio user + il tool fittizio + `tool_choice` che
/// FORZA la chiamata al tool (`required`). Forzare il tool e' la condizione che
/// fa emergere i quirk: e' proprio la combinazione thinking + `tool_choice` che
/// rompeva DeepSeek, e il `tool_config.function_calling_config.mode` che rompeva
/// Google. `temperature` bassa per ridurre la varianza (non e' un sync barrier).
fn richiesta_turno1(modello: &str) -> LlmRequest {
    LlmRequest {
        model: modello.to_string(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Text(USER_PROMPT.to_string()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: None,
        }],
        temperature: Some(0.0),
        max_tokens: Some(1024),
        tools: Some(vec![tool_definition()]),
        response_format: None,
        stream: Some(false),
        thinking: None,
        tool_choice: Some(serde_json::json!("required")),
        // Niente routing/fallback: il test esercita ESATTAMENTE il provider scelto
        // chiamando `provider.complete` direttamente (non passa dal gateway).
        pin_provider: None,
        metadata: metadata(),
    }
}

/// Richiesta del TURNO 2: ricostruisce la history come la rimanderebbe il brain.
///   1. il messaggio `user` originale;
///   2. il messaggio `assistant` del turno 1, con le tool-call emesse E i campi
///      `reasoning` / `thinking_signature` COPIATI dalla risposta del turno 1
///      (e' il round-trip che DeepSeek e Anthropic IMPONGONO: senza, HTTP 400);
///   3. un messaggio `tool` per OGNI tool-call, con `tool_call_id` corretto e il
///      risultato fittizio come content.
/// Al turno 2 NON si forza piu' il tool (`tool_choice: None`): vogliamo che il
/// modello produca la risposta testuale finale a partire dal tool-result.
fn richiesta_turno2(modello: &str, risposta1: &LlmResponse) -> LlmRequest {
    let tool_calls = risposta1
        .tool_calls
        .clone()
        .expect("turno 2 costruito solo se il turno 1 ha prodotto tool_calls");

    // Messaggio assistant che "ricorda" le tool-call del turno 1, ri-passando
    // reasoning + thinking_signature dalla LlmResponse precedente.
    let assistant = LlmMessage {
        role: "assistant".to_string(),
        // Content puo' essere vuoto quando l'assistant ha solo chiamato tool.
        content: MessageContent::Text(risposta1.content.clone()),
        tool_call_id: None,
        tool_calls: Some(tool_calls.clone()),
        name: None,
        thinking_signature: risposta1.thinking_signature.clone(),
        reasoning: risposta1.reasoning.clone(),
    };

    // Un messaggio tool per ciascuna tool-call (tool_call_id deve combaciare).
    let tool_messages: Vec<LlmMessage> = tool_calls.iter().map(messaggio_tool).collect();

    let mut messages = vec![
        LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Text(USER_PROMPT.to_string()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: None,
        },
        assistant,
    ];
    messages.extend(tool_messages);

    LlmRequest {
        model: modello.to_string(),
        messages,
        temperature: Some(0.0),
        max_tokens: Some(1024),
        tools: Some(vec![tool_definition()]),
        response_format: None,
        stream: Some(false),
        thinking: None,
        // Turno 2: nessun vincolo, ci aspettiamo testo finale.
        tool_choice: None,
        pin_provider: None,
        metadata: metadata(),
    }
}

/// Costruisce il messaggio `tool` (tool-result) per una tool-call. Il content e'
/// modellato come blocco `tool_result` (formato canonico del contratto), con il
/// `tool_use_id` che combacia con l'id della tool-call: cosi' Anthropic (block
/// `tool_result`) e gli OpenAI-compat (campo `tool_call_id`) lo risolvono
/// entrambi. Il `tool_call_id` di primo livello e' valorizzato per i provider
/// OpenAI-compat che lo leggono da li'.
fn messaggio_tool(tc: &LlmToolCall) -> LlmMessage {
    LlmMessage {
        role: "tool".to_string(),
        content: MessageContent::Blocks(vec![LlmContentBlock {
            kind: "tool_result".to_string(),
            text: None,
            image_url: None,
            tool_use_id: Some(tc.id.clone()),
            content: Some(TOOL_RESULT.to_string()),
        }]),
        tool_call_id: Some(tc.id.clone()),
        tool_calls: None,
        name: Some(tc.function.name.clone()),
        thinking_signature: None,
        reasoning: None,
    }
}

/// Esegue il mini tool-loop a 2 turni su un provider concreto e ne ASSERISCE il
/// comportamento. Punto unico (regola L): tutta la logica di test vive qui; i
/// `#[tokio::test]` per-provider sono solo l'innesto che costruisce il provider.
///
/// - `etichetta`: nome leggibile del provider (solo per i messaggi di asserzione).
/// - `provider`: l'istanza concreta (dyn) gia' costruita con la sua api_key.
/// - `modello`: nome modello chat economico e valido per quel provider.
async fn esegui_tool_loop(etichetta: &str, provider: &dyn LlmProvider, modello: &str) {
    // ---- TURNO 1: deve emergere una tool-call al tool fittizio. ----
    let req1 = richiesta_turno1(modello);
    let risposta1 = provider
        .complete(&req1)
        .await
        .unwrap_or_else(|e| panic!("[{etichetta}] turno 1 fallito (complete): {e:#}"));

    let tool_calls = risposta1.tool_calls.as_ref().unwrap_or_else(|| {
        panic!(
            "[{etichetta}] turno 1: nessun tool_calls (content='{}', finish='{}')",
            risposta1.content, risposta1.finish_reason
        )
    });
    assert!(
        !tool_calls.is_empty(),
        "[{etichetta}] turno 1: tool_calls presente ma vuoto"
    );
    assert_eq!(
        tool_calls[0].function.name, TOOL_NAME,
        "[{etichetta}] turno 1: il modello ha chiamato un tool diverso da {TOOL_NAME}"
    );

    // ---- TURNO 2: con la history + tool-result, deve produrre testo finale. ----
    let req2 = richiesta_turno2(modello, &risposta1);
    let risposta2 = provider
        .complete(&req2)
        .await
        .unwrap_or_else(|e| panic!("[{etichetta}] turno 2 fallito (complete): {e:#}"));

    assert!(
        !risposta2.content.trim().is_empty(),
        "[{etichetta}] turno 2: content vuoto (finish='{}')",
        risposta2.finish_reason
    );
}

/// Legge una API key da `primario`, con eventuale `alias` (es. GEMINI_API_KEY per
/// Google). Ritorna `None` se nessuna delle due e' presente/non vuota: il test la
/// usa per lo skip non-fallimentare. Niente trim distruttivo del valore.
fn api_key(primario: &str, alias: Option<&str>) -> Option<String> {
    env::var(primario)
        .ok()
        .or_else(|| alias.and_then(|a| env::var(a).ok()))
        .filter(|s| !s.trim().is_empty())
}

/// Client HTTP condiviso del test (un'unica connessione pool per chiamata e
/// riuso fra i 2 turni). Default builder: nessun timeout custom (le API possono
/// avere cold start; non e' un sync barrier hardcoded).
fn http() -> Client {
    Client::new()
}

#[tokio::test]
async fn deepseek_tool_loop() {
    // Quirk catturati: gate thinking+tool_choice (400 "Thinking mode does not
    // support tool_choice") al turno 1; ri-passaggio reasoning_content (400
    // "reasoning_content ... must be passed back") al turno 2.
    let Some(key) = api_key("DEEPSEEK_API_KEY", None) else {
        eprintln!("skip deepseek: no key");
        return;
    };
    let provider = DeepSeekProvider::new(http(), key, None);
    // Modello chat economico dual-mode (thinking di default): esercita il gate.
    esegui_tool_loop("deepseek", &provider, "deepseek-chat").await;
}

#[tokio::test]
async fn google_tool_loop() {
    // Quirk catturati: functionDeclarations presenti, thinkingBudget=0 quando si
    // forza un tool, thoughtSignature re-inviata al turno 2. Accetta sia
    // GOOGLE_API_KEY (nome del repo) sia GEMINI_API_KEY (comune nelle CI Gemini).
    let Some(key) = api_key("GOOGLE_API_KEY", Some("GEMINI_API_KEY")) else {
        eprintln!("skip google: no key");
        return;
    };
    // Senza DB il backend ricade su Gemini con la api_key iniettata (?key=...),
    // sufficiente per il test (Vertex richiederebbe Service Account + project).
    let provider = GoogleProvider::new(http(), key, None);
    esegui_tool_loop("google", &provider, "gemini-2.5-flash").await;
}

#[tokio::test]
async fn anthropic_tool_loop() {
    // Quirk catturati: round-trip thinking_signature (400 se al turno 2 manca la
    // signature del blocco thinking), tool_use/tool_result come content block.
    let Some(key) = api_key("ANTHROPIC_API_KEY", None) else {
        eprintln!("skip anthropic: no key");
        return;
    };
    let provider = AnthropicProvider::new(http(), key, None);
    esegui_tool_loop("anthropic", &provider, "claude-haiku-4-5").await;
}

#[tokio::test]
async fn mistral_tool_loop() {
    // Quirk catturati: trailing assistant (422 "last role assistant"), tool-call
    // OpenAI-compat e ri-passaggio history.
    let Some(key) = api_key("MISTRAL_API_KEY", None) else {
        eprintln!("skip mistral: no key");
        return;
    };
    let provider = MistralProvider::new(http(), key, None);
    esegui_tool_loop("mistral", &provider, "mistral-small-latest").await;
}

#[tokio::test]
async fn openai_tool_loop() {
    // Quirk catturati: tool_choice passthrough nativo, ri-passaggio tool_calls +
    // tool message con tool_call_id corretto.
    let Some(key) = api_key("OPENAI_API_KEY", None) else {
        eprintln!("skip openai: no key");
        return;
    };
    let provider = OpenAiProvider::new(http(), key, None);
    esegui_tool_loop("openai", &provider, "gpt-4o-mini").await;
}
