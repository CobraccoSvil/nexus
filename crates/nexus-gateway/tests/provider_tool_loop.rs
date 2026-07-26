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
//! # DOVE ARRIVA QUESTO STRUMENTO, E DOVE NO (regola O)
//!
//! Misurato il 2026-07-26, prima di credere all'elenco di quirk qui sopra:
//!
//! 1. **In CI questi test non chiamano nessun provider.** `cargo test
//!    --workspace` gira dentro `pnpm verify` (`.github/workflows/verify.yml` ->
//!    `scripts/verify.sh`), il cui unico `env:` e' `DATABASE_URL`; in TUTTI i
//!    workflow del repo le occorrenze di `secrets.` sono zero. Nessuna chiave
//!    provider esiste in quell'ambiente, quindi i 5 test saltavano sempre — e
//!    uno skip che ritorna verde e' indistinguibile da un successo. Il verde
//!    "copriva" proprio i 400 che continuavano a costare run in produzione.
//!    Rimedio: [`chiave_provider`] + [`copertura_live_dichiarata`] (sotto).
//! 2. **Il loop parte da `provider.complete`, non dal gateway.** Non attraversa
//!    routing, retry, ne' `history_sanitizer`: la history del turno 2 e' quella
//!    PERFETTA costruita qui. I 400 che nascono da una history *manipolata* fra
//!    i due turni (p.es. una sanitizzazione che rimuove il `reasoning` prima di
//!    un retry) restano fuori portata per costruzione.
//! 3. **Il quirk DeepSeek del `reasoning_content` non e' raggiungibile da qui**,
//!    nemmeno con la chiave: `resolve_reasoning` (`src/providers/deepseek.rs`)
//!    spegne il thinking appena la richiesta porta dei tool, e questo test
//!    manda `tools` in ENTRAMBI i turni. Senza thinking non c'e'
//!    `reasoning_content`, quindi il 400 "must be passed back" non puo'
//!    emergere. Quel gate e' coperto in modo deterministico e gratuito dagli
//!    unit test di `resolve_reasoning`, che girano in CI senza chiavi: e' LI'
//!    che va aggiunto un caso quando il gate cambia, non qui.
//!
//! Quello che questo file misura davvero, quando le chiavi ci sono, e' la
//! PARITA' del contratto a 2 turni sul filo del provider: tool-call emessa al
//! turno 1 con `tool_choice` forzato, e testo finale al turno 2 con
//! tool_calls + tool_result + round-trip di `reasoning`/`thinking_signature`
//! cosi' come li ha restituiti il turno 1.
//!
//! # SKIP VISIBILE, NON SILENZIOSO
//!
//! Ogni test legge la propria API key dal punto unico [`chiave_provider`], che
//! consulta la tabella [`PROVIDER_KEYS`] (gli stessi nomi dei `.env*.example`
//! del repo). Comportamento in assenza di chiave:
//!
//! - con `REQUIRE_PROVIDER_TESTS=1` il test **FALLISCE** nominando la env var
//!   mancante: e' la modalita' del job dedicato `provider-live.yml`, dove un
//!   segreto non configurato deve arrossare invece di sparire;
//! - senza quella variabile il test salta, ma stampa un marker riconoscibile
//!   (`NEXUS_PROVIDER_LIVE_SKIP <provider> (<ENV_VAR> assente)`) e
//!   [`copertura_live_dichiarata`] — che gira SEMPRE — dichiara il conteggio
//!   con la sua premessa (`COPERTURA LIVE PROVIDER: n/5 ...`), su stdout e,
//!   se `NEXUS_PROVIDER_SKIP_REPORT` e' impostata, su file per un gate.
//!
//! Scartato `#[ignore]`: toglierebbe l'esecuzione in locale (dove le chiavi ci
//! sono) proprio a chi puo' permettersela, e un test "ignored" resta comunque
//! un verde nel conteggio finale — il difetto da chiudere e' esattamente
//! quello. Il gate `pnpm verify` NON acquisisce chiamate reali: senza
//! `REQUIRE_PROVIDER_TESTS` il costo e la flakiness restano zero.
//!
//! Niente DB: le chiavi runtime stanno nel DB (`settings`), ma un integration
//! test non deve dipendere da un DB popolato di segreti — la fonte canonica per
//! la CI sono le env var.
//!
//! Regola F: niente segreti nel log (si stampa solo il nome provider e il NOME
//! della env var, mai il valore).
//! Regola G: i nomi modello qui sono SOLO scelte di test economiche/valide, non
//! configurazione di business; il gateway in produzione li legge dal DB.

use std::env;
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write as _;

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
        run_timeout_secs: None,
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
        run_timeout_secs: None,
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

/// Env var che rende OBBLIGATORIE le chiamate live: se impostata a `1`, una
/// chiave mancante non e' piu' uno skip ma un fallimento. La usa il job
/// `provider-live.yml`, cosi' un segreto non configurato arrossa invece di
/// sparire in un verde muto.
const REQUIRE_ENV: &str = "REQUIRE_PROVIDER_TESTS";

/// Env var opzionale: percorso di un file dove [`copertura_live_dichiarata`]
/// scrive il conteggio della copertura, perche' un gate esterno possa leggerlo
/// senza fare parsing dell'output di `cargo test`.
const REPORT_ENV: &str = "NEXUS_PROVIDER_SKIP_REPORT";

/// Prefisso del marker di skip su stdout. Stringa cercabile nei log di CI e dal
/// guard `test-provider-live-onesto` in `scripts/check-single-source.sh`.
const MARKER_SKIP: &str = "NEXUS_PROVIDER_LIVE_SKIP";

/// Tabella `(etichetta, env var primaria, alias)` dei provider esercitati.
///
/// PUNTO UNICO (regola L): la consultano sia i `#[tokio::test]` per-provider,
/// via [`chiave_provider`], sia la sentinella [`copertura_live_dichiarata`] che
/// conta la copertura. Una sola fonte, quindi il conteggio non puo' divergere
/// dai test realmente presenti. Ogni etichetta qui DEVE avere il suo
/// `async fn <etichetta>_tool_loop` nel file: lo verifica il guard
/// `test-provider-live-onesto`.
const PROVIDER_KEYS: &[(&str, &str, Option<&str>)] = &[
    ("deepseek", "DEEPSEEK_API_KEY", None),
    ("google", "GOOGLE_API_KEY", Some("GEMINI_API_KEY")),
    ("anthropic", "ANTHROPIC_API_KEY", None),
    ("mistral", "MISTRAL_API_KEY", None),
    ("openai", "OPENAI_API_KEY", None),
];

/// Legge la prima env var non vuota fra `primario` e l'eventuale `alias`.
/// Niente trim distruttivo del valore restituito.
fn leggi_chiave(primario: &str, alias: Option<&str>) -> Option<String> {
    env::var(primario)
        .ok()
        .or_else(|| alias.and_then(|a| env::var(a).ok()))
        .filter(|s| !s.trim().is_empty())
}

/// Descrive le env var accettate per un provider (per i messaggi: solo NOMI,
/// mai valori — regola F).
fn nomi_env(primario: &str, alias: Option<&str>) -> String {
    match alias {
        Some(a) => format!("{primario} o {a}"),
        None => primario.to_string(),
    }
}

/// Cerca in [`PROVIDER_KEYS`] la riga di `etichetta`. Assente = errore di
/// programmazione nel test (etichetta scritta a mano diversa dalla tabella),
/// non una condizione di runtime: si panica subito.
fn riga_provider(etichetta: &str) -> (&'static str, Option<&'static str>) {
    PROVIDER_KEYS
        .iter()
        .find(|(nome, _, _)| *nome == etichetta)
        .map(|(_, primario, alias)| (*primario, *alias))
        .unwrap_or_else(|| {
            panic!("provider '{etichetta}' assente da PROVIDER_KEYS: aggiungilo alla tabella")
        })
}

/// PUNTO UNICO dell'accesso alla chiave nei test live (regola L).
///
/// Ritorna `Some(key)` se la chiave c'e'. Se manca:
/// - con `REQUIRE_PROVIDER_TESTS=1` PANICA, nominando le env var accettate;
/// - altrimenti stampa il marker [`MARKER_SKIP`] e ritorna `None`.
///
/// Nessun chiamante deve leggere `env::var` per una chiave provider da solo: uno
/// skip fuori da qui tornerebbe invisibile alla sentinella e al guard.
fn chiave_provider(etichetta: &str) -> Option<String> {
    let (primario, alias) = riga_provider(etichetta);
    if let Some(key) = leggi_chiave(primario, alias) {
        return Some(key);
    }
    let env_richieste = nomi_env(primario, alias);
    assert!(
        !richiede_test_live(),
        "[{etichetta}] {REQUIRE_ENV}=1 ma la chiave manca: imposta {env_richieste}. \
         Il test live NON puo' essere considerato coperto."
    );
    println!("{MARKER_SKIP} {etichetta} ({env_richieste} assente)");
    None
}

/// Vero se l'ambiente esige l'esecuzione live (`REQUIRE_PROVIDER_TESTS=1`).
fn richiede_test_live() -> bool {
    env::var(REQUIRE_ENV).is_ok_and(|v| v.trim() == "1")
}

/// SENTINELLA — gira SEMPRE, con o senza chiavi, e dichiara DA DOVE guarda.
///
/// Regola O, "un numero senza la sua premessa e' un'opinione": i 5 test live
/// possono essere verdi perche' hanno interrogato le API o perche' non le hanno
/// mai toccate, e i due casi erano indistinguibili. Questo test rende il
/// conteggio esplicito nell'output (`COPERTURA LIVE PROVIDER: n/5`), elenca per
/// nome i provider scoperti, e sotto `REQUIRE_PROVIDER_TESTS=1` FALLISCE se la
/// copertura non e' piena.
#[test]
fn copertura_live_dichiarata() {
    let mut coperti: Vec<&str> = Vec::new();
    let mut scoperti: Vec<String> = Vec::new();

    for (etichetta, primario, alias) in PROVIDER_KEYS {
        if leggi_chiave(primario, *alias).is_some() {
            coperti.push(etichetta);
        } else {
            scoperti.push(format!("{etichetta} ({})", nomi_env(primario, *alias)));
        }
    }

    let totale = PROVIDER_KEYS.len();
    let mut riepilogo = format!(
        "COPERTURA LIVE PROVIDER: {}/{totale} chiamano davvero le API",
        coperti.len()
    );
    if !coperti.is_empty() {
        let _ = write!(riepilogo, "; coperti: {}", coperti.join(", "));
    }
    if !scoperti.is_empty() {
        let _ = write!(riepilogo, "; SCOPERTI: {}", scoperti.join(", "));
    }
    println!("{riepilogo}");

    // Report su file per un gate esterno: lo scrive SOLO questa sentinella, che
    // ispeziona tutte le righe da sola, quindi nessuna scrittura concorrente.
    let percorso = env::var(REPORT_ENV).unwrap_or_default();
    let percorso = percorso.trim();
    if !percorso.is_empty() {
        match OpenOptions::new().create(true).append(true).open(percorso) {
            Ok(mut f) => {
                let _ = writeln!(f, "{riepilogo}");
            }
            // Un report non scrivibile non deve mascherare l'esito del test:
            // si segnala e si prosegue (l'informazione resta su stdout).
            Err(e) => println!("{MARKER_SKIP} report non scrivibile ({percorso}): {e}"),
        }
    }

    let copertura_incompleta = richiede_test_live() && !scoperti.is_empty();
    assert!(
        !copertura_incompleta,
        "{REQUIRE_ENV}=1 ma {} provider su {totale} non hanno chiave: {}. \
         I quirk elencati in testa a questo file NON sono coperti.",
        scoperti.len(),
        scoperti.join(", ")
    );
}

/// Client HTTP condiviso del test (un'unica connessione pool per chiamata e
/// riuso fra i 2 turni). Default builder: nessun timeout custom (le API possono
/// avere cold start; non e' un sync barrier hardcoded).
fn http() -> Client {
    Client::new()
}

#[tokio::test]
async fn deepseek_tool_loop() {
    // Quirk catturato: il turno 1 con `tool_choice` forzato non deve dare 400
    // "Thinking mode does not support tool_choice".
    //
    // NON catturato qui (vedi punto 3 dell'intestazione): il 400
    // "reasoning_content ... must be passed back". `resolve_reasoning` spegne il
    // thinking perche' la richiesta porta dei tool, quindi il turno 1 non
    // produce `reasoning_content` e al turno 2 non c'e' niente da rimandare. Il
    // gate e' coperto dagli unit test di `resolve_reasoning`, deterministici e
    // senza chiave.
    let Some(key) = chiave_provider("deepseek") else {
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
    let Some(key) = chiave_provider("google") else {
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
    let Some(key) = chiave_provider("anthropic") else {
        return;
    };
    let provider = AnthropicProvider::new(http(), key, None);
    esegui_tool_loop("anthropic", &provider, "claude-haiku-4-5").await;
}

#[tokio::test]
async fn mistral_tool_loop() {
    // Quirk catturati: trailing assistant (422 "last role assistant"), tool-call
    // OpenAI-compat e ri-passaggio history.
    let Some(key) = chiave_provider("mistral") else {
        return;
    };
    let provider = MistralProvider::new(http(), key, None);
    esegui_tool_loop("mistral", &provider, "mistral-small-latest").await;
}

#[tokio::test]
async fn openai_tool_loop() {
    // Quirk catturati: tool_choice passthrough nativo, ri-passaggio tool_calls +
    // tool message con tool_call_id corretto.
    let Some(key) = chiave_provider("openai") else {
        return;
    };
    let provider = OpenAiProvider::new(http(), key, None);
    esegui_tool_loop("openai", &provider, "gpt-4o-mini").await;
}
