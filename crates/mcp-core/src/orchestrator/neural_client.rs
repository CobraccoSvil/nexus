//! Client del neural-core.
//!
//! Storicamente questo client faceva da proxy gRPC verso il brain Python
//! (porta 50051) per OGNI operazione AI. Due percorsi sono stati progressivamente
//! cablati IN-PROCESS verso i punti unici Rust, eliminando il round-trip al brain:
//!
//! - `embed_text*`  -> embedder ONNX in-process (`NexusBridge::embedder`).
//! - `generate_completion` / `generate_agent_turn` -> Nexus LLM Gateway Rust
//!   (`NexusGatewayClient`, porta 4060) DIRETTAMENTE. Prima il giro era assurdo
//!   (`mcp-core` gRPC -> `brain` `GenerateCompletion`/`GenerateAgentTurn` ->
//!   `GatewayProvider` -> gateway): il brain non faceva altro che inoltrare al
//!   gateway e ri-normalizzare la risposta. Ora mcp-core parla col gateway senza
//!   intermediari (verso zero-Python). La FORMA del `Value` ritornato e'
//!   identica a quella che il brain produceva (replica di
//!   `brain/grpc_server/neural_service.py::GenerateCompletion/GenerateAgentTurn`
//!   + `brain/providers/gateway_provider.py::_build_agent_result`), cosi' i call
//!   site (`prompt_templates`, `chat_sessions`, `model_health_probe`,
//!   `provider_health_probe`, `service_discovery`, `learned_instructions`,
//!   `nexus-wiki`, ...) restano INVARIATI.
//!
//! - `generate_document` (rendering .docx) -> renderer Rust in-process
//!   (`crate::docx_render::render_document`). Era l'ultimo RPC AI-adiacente
//!   ancora servito dal brain: il .docx viene ora assemblato interamente in
//!   Rust (ZIP + OOXML), senza round-trip di rete (verso zero-Python).
//!
//! Restano sul gRPC al brain solo gli RPC batch/model-sync non toccati da questo
//! refactoring.

use serde_json::{json, Value};

// Nota: i tipi del vecchio proxy gRPC (`mcp_proto::neural`, generati da
// proto/neural_core.proto) non esistono piu': il proto e' stato rimosso col
// brain. `NeuralCoreClient` non incapsula piu' un canale gRPC, tutti i metodi
// delegano all'embedder ONNX in-process o al Nexus LLM Gateway.
use crate::nexus_gateway::{GwMessage, GwMetadata, GwRequest, GwResponse, NexusGatewayClient};

/// Client storico del neural-core. Non incapsula piu' un canale gRPC verso il
/// brain: TUTTI i metodi rimasti delegano all'embedder ONNX in-process
/// (`NexusBridge`) o al Nexus LLM Gateway (`NexusGatewayClient`). Resta una
/// struct vuota (zero-sized) per preservare le firme dei call site storici
/// (`NeuralCoreClient::connect(url).embed_text(...)` ecc.) senza propagare un
/// refactoring piu' ampio. L'ultimo RPC gRPC al brain qui presente
/// (`generate_document`) e' stato sostituito dal renderer Rust `docx_render`.
#[derive(Clone, Default)]
pub struct NeuralCoreClient;

impl std::fmt::Debug for NeuralCoreClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NeuralCoreClient").finish_non_exhaustive()
    }
}

impl NeuralCoreClient {
    /// Costruisce il client. Non apre piu' alcun canale gRPC (il brain non e' nel
    /// percorso): l'`url` e' accettato per compatibilita' di firma coi call site
    /// che ancora leggono `settings.neural_core_url`, ma e' ignorato. Resta
    /// `async`/`Result` per non toccare i ~5 call site esistenti.
    pub async fn connect(_url: &str) -> anyhow::Result<Self> {
        Ok(Self)
    }

    /// Variante per i test unit. Identica a `connect` ora che non c'e' piu' un
    /// canale: nessun I/O, serve solo a costruire un `AgentToolContext`.
    #[cfg(test)]
    pub(crate) fn disconnected_for_tests() -> Self {
        Self
    }

    pub async fn embed_text(&self, model: &str, text: &str) -> anyhow::Result<Vec<f32>> {
        let (_model, vector) = self.embed_text_with_model(model, text).await?;
        Ok(vector)
    }

    /// Come `embed_text`, ma ritorna anche il nome del modello usato per
    /// generare il vettore. Serve a costruire una signature dell'embedder
    /// (modello + dimensione) da incorporare negli hash di indicizzazione:
    /// cosi' un cambio di embedder invalida automaticamente gli hash e forza il
    /// reindex, senza interventi manuali sul DB.
    ///
    /// Embedding ONNX MiniLM-384d IN-PROCESS (regola L: punto unico embedder,
    /// lo stesso `NexusBridge::embedder()` usato da `/api/embed`,
    /// `Orchestrator::embed_text` e dai tool ruvector). Niente piu' round-trip
    /// gRPC verso il brain Python: il brain stesso (`brain/embeddings/service.py`)
    /// ormai delega a `/api/embed` di mcp-core, quindi il gRPC `EmbedText` faceva
    /// un giro inutile Rust -> brain -> HTTP -> mcp-core -> ONNX. Ora chiama
    /// direttamente l'embedder locale.
    ///
    /// Label: replica la logica dell'handler `/api/embed` (label
    /// `all-MiniLM-L6-v2` quando `model` e' vuoto, altrimenti il `model`
    /// richiesto). Tutti i call site passano `""`, quindi il label resta
    /// `all-MiniLM-L6-v2` -- identico a quello che il brain ritornava prima,
    /// percio' gli hash di indicizzazione restano validi (nessun reindex).
    /// I vettori sono identici (stesso modello ONNX, parita' validata).
    pub async fn embed_text_with_model(
        &self,
        model: &str,
        text: &str,
    ) -> anyhow::Result<(String, Vec<f32>)> {
        let bridge = crate::nexus_bridge::NexusBridge::global().ok_or_else(|| {
            anyhow::anyhow!("nexus bridge non inizializzato (embed_text_with_model)")
        })?;
        let used_model = if model.trim().is_empty() {
            "all-MiniLM-L6-v2".to_string()
        } else {
            model.to_string()
        };
        // embed() e' CPU-bound sincrono: spawn_blocking per non bloccare il
        // runtime async (stesso pattern di Orchestrator::embed_text).
        let text = text.to_string();
        let vector = tokio::task::spawn_blocking(move || bridge.embed_one(&text))
            .await
            .map_err(|e| anyhow::anyhow!("embed_text_with_model spawn_blocking join: {e}"))?;
        if vector.is_empty() {
            anyhow::bail!("empty_embed_vector");
        }
        Ok((used_model, vector))
    }

    /// Risolve il client del Nexus LLM Gateway dal pool DB del bridge globale
    /// (`NexusGatewayClient::from_db`, PUNTO UNICO del cablaggio gateway —
    /// regola L). Stesso pattern di `embed_text_with_model`: attinge al singleton
    /// `NexusBridge` invece di propagare un `PgPool` lungo l'intera catena di
    /// costruzione del `NeuralCoreClient` (clonato in decine di contesti).
    /// La porta e' risolta da `settings` (regola G), non hardcoded.
    async fn gateway(&self) -> anyhow::Result<NexusGatewayClient> {
        let bridge = crate::nexus_bridge::NexusBridge::global()
            .ok_or_else(|| anyhow::anyhow!("nexus bridge non inizializzato (gateway)"))?;
        let db = bridge
            .db()
            .ok_or_else(|| anyhow::anyhow!("nexus bridge senza pool DB (gateway)"))?;
        Ok(NexusGatewayClient::from_db(db).await)
    }

    /// Completion testuale one-shot. Cablata DIRETTAMENTE al Nexus LLM Gateway
    /// (niente piu' gRPC al brain): costruisce un `GwRequest` con un solo turno
    /// `user`, `pin_provider` = il provider gia' risolto a monte (regola G,
    /// nessun secondo routing) e nessun tool.
    ///
    /// La FORMA del `Value` ritornato replica
    /// `brain/grpc_server/neural_service.py::GenerateCompletion`:
    /// `{provider, model, content, metadata:{usage, [error]}, error, error_class}`.
    /// I call site leggono `content`, `metadata.usage` (billing
    /// `extract_usage_numbers`), `metadata.error` / `error` (`completion_has_error`),
    /// `error_class` (probe): tutte queste chiavi sono preservate.
    pub async fn generate_completion(
        &self,
        provider: &str,
        model: &str,
        prompt: &str,
    ) -> anyhow::Result<Value> {
        let gw = self.gateway().await?;
        let req = GwRequest {
            model: model.to_string(),
            messages: vec![GwMessage {
                role: "user".to_string(),
                content: json!(prompt),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                thinking_signature: None,
            }],
            pin_provider: Some(provider.to_string()),
            metadata: GwMetadata {
                feature: "neural_completion".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        match gw.complete(req).await {
            Ok(resp) => Ok(completion_value_from_gw(provider, model, &resp)),
            // Errore HTTP/timeout del gateway: NON propagare l'errore grezzo, ma
            // costruire un Value d'errore nella STESSA forma che il brain
            // ritornava (content "[Error: ...]" + error/error_class). I call site
            // dipendono da questa forma per la detection "errore ingoiato"
            // (provider_health_probe) e per `completion_has_error` (core).
            Err(e) => Ok(error_completion_value(provider, model, &e.to_string())),
        }
    }

    /// Turno agentico (con tool support). Cablato DIRETTAMENTE al Nexus LLM
    /// Gateway. `messages_json` (lista `{role, content, ...}`) e `tools_json`
    /// (tool Anthropic-style `{name, description, input_schema}`) vengono mappati
    /// nel contratto del gateway; `pin_provider` evita un secondo routing
    /// (regola G); i tool sono tradotti nel dialetto OpenAI dal PUNTO UNICO
    /// `tools_to_openai_schema` (regola L).
    ///
    /// La FORMA del `Value` ritornato replica
    /// `neural_service.py::GenerateAgentTurn` +
    /// `gateway_provider.py::_build_agent_result`: `{provider, model, content,
    /// stop_reason, tool_use_blocks, assistant_content, usage, [error,
    /// error_class]}`. `model_health_probe` legge `error_class`, `stop_reason` e
    /// `tool_use_blocks[].name`; gli altri call site solo `content`.
    pub async fn generate_agent_turn(
        &self,
        provider: &str,
        model: &str,
        messages_json: &str,
        tools_json: &str,
        max_tokens: u32,
        system_text: &str,
    ) -> anyhow::Result<Value> {
        let gw = self.gateway().await?;

        // Messaggi grezzi -> GwMessage. I call site passano sempre
        // `{role, content}` testuali (content stringa); la deserializzazione
        // tollerante preserva eventuali tool_calls/tool_call_id futuri.
        let raw_messages: Vec<Value> = if messages_json.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(messages_json)
                .map_err(|e| anyhow::anyhow!("generate_agent_turn: messages_json invalido: {e}"))?
        };
        let messages: Vec<GwMessage> = raw_messages
            .into_iter()
            .map(gw_message_from_value)
            .collect();

        // system_text -> primo messaggio role=system (il gateway lo riconduce al
        // system del provider). Anteporlo preserva il comportamento del brain che
        // passava `system_text` a `generate_agent_turn_sync`.
        let mut all_messages = Vec::with_capacity(messages.len() + 1);
        if !system_text.trim().is_empty() {
            all_messages.push(GwMessage {
                role: "system".to_string(),
                content: json!(system_text),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                thinking_signature: None,
            });
        }
        all_messages.extend(messages);

        // Tool Anthropic-style -> OpenAI (punto unico, regola L). Lista vuota
        // ("[]") -> nessun tool.
        let tools: Option<Value> = if tools_json.trim().is_empty() || tools_json.trim() == "[]" {
            None
        } else {
            let parsed: Vec<Value> = serde_json::from_str(tools_json)
                .map_err(|e| anyhow::anyhow!("generate_agent_turn: tools_json invalido: {e}"))?;
            if parsed.is_empty() {
                None
            } else {
                Some(crate::agent_graph_adapter::llm_gateway::tools_to_openai_schema(&parsed))
            }
        };

        let req = GwRequest {
            model: model.to_string(),
            messages: all_messages,
            max_tokens: Some(max_tokens),
            tools,
            pin_provider: Some(provider.to_string()),
            metadata: GwMetadata {
                feature: "neural_agent_turn".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        match gw.complete(req).await {
            Ok(resp) => Ok(agent_turn_value_from_gw(provider, model, &resp)),
            Err(e) => {
                // Regola M: il gateway ha GIA' classificato il fallimento alla fonte
                // (`CallFailure` -> `details.primary_cause`) e l'errore arriva qui
                // TIPIZZATO (`GatewayHttpError`, vedi nexus_gateway.rs: "i decisori
                // fanno downcast_ref, mai match sul testo"). Prima questo ramo faceva
                // `e.to_string()` e la classe veniva ri-dedotta con una regex sul
                // Display: il segnale moriva qui. Ora si legge il codice strutturato e
                // il testo resta solo per display/log.
                let structured = structured_error_class(&e);
                Ok(error_agent_turn_value_with_class(
                    provider,
                    model,
                    &e.to_string(),
                    structured,
                ))
            }
        }
    }

    /// Health di un provider. Cablato al gateway: per `"system"` riflette lo stato
    /// del gateway (sostituisce il vecchio `GetProviderHealth("system")` del
    /// brain); per un provider specifico ritorna lo stato del gateway nella forma
    /// `{status, reason?}` letta da `Orchestrator::run` (chiavi `status` ∈
    /// {ready, ok} e `reason`/`skipReasons`). Il gateway possiede gia' il gate di
    /// disponibilita' provider/cooldown (ADR 0020), quindi un `200` su `/health`
    /// significa "instradabile"; la verifica fine del singolo provider avviene
    /// comunque sul `complete` (errore -> error_class).
    pub async fn provider_health(&self, provider: &str) -> anyhow::Result<Value> {
        let gw = self.gateway().await?;
        if gw.is_healthy().await {
            Ok(json!({
                "status": "ok",
                "service": "nexus-gateway",
                "provider": provider,
            }))
        } else {
            Ok(json!({
                "status": "unavailable",
                "service": "nexus-gateway",
                "provider": provider,
                "reason": "gateway_unreachable",
            }))
        }
    }

    /// Salute complessiva del path AI. Ora riflette il Nexus LLM Gateway (non piu'
    /// il brain): il brain non e' piu' nel percorso di completion/agent-turn. Usato
    /// dall'health check UI (`/health` -> `neural_core`).
    pub async fn is_healthy(&self) -> bool {
        match self.gateway().await {
            Ok(gw) => gw.is_healthy().await,
            Err(_) => false,
        }
    }

    /// Classificazione errori provider via il PUNTO UNICO Rust
    /// (``crate::provider_error_classifier::classify_text``, paritetico a
    /// ``brain/providers/error_handler.py`` con golden test cross-language —
    /// regola L / ADR 0026, Wave 8b).
    ///
    /// Prima questa funzione faceva una RPC gRPC ``ClassifyError`` al brain
    /// per ogni errore provider — overhead inutile su un path d'errore caldo,
    /// e fragile (se il brain e' down, non riesci nemmeno a classificare).
    /// Ora il classificatore vive in Rust: zero round-trip di rete, zero
    /// dipendenza da brain healthy per gestire un errore provider.
    ///
    /// La parte SDK-specifica (estrazione `retry-after` da
    /// `exc.response.headers`) resta lato Python perche' richiede l'oggetto
    /// eccezione vero, e viaggia gia' nel ``metadata`` del ``ProviderResult``
    /// (campo ``retry_after_seconds`` consumato dal loop agente lato brain).
    ///
    /// L'``&self`` e ``async`` sono mantenuti per compatibilita' di firma con
    /// i call site esistenti, ma la funzione e' di fatto sincrona e priva di
    /// I/O di rete.
    pub async fn classify_error(&self, error_text: &str, _provider: &str) -> String {
        crate::provider_error_classifier::classify_text(error_text).stop_reason
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Mapping GwResponse -> forma Value storica del brain.
//
// Questi helper sono il PUNTO UNICO (regola L) della traduzione fra il contratto
// del Nexus LLM Gateway (`GwResponse`) e la forma `Value` che i call site dei due
// metodi `NeuralCoreClient` si aspettano (la stessa che il brain produceva). Sono
// liberi (testabili senza rete) e usati SOLO da `generate_completion` /
// `generate_agent_turn` sopra.
// ───────────────────────────────────────────────────────────────────────────

/// Costruisce il dict `usage` interno dalla `GwUsage` del gateway, replicando
/// `gateway_provider.py::_usage_to_internal` (convenzione Anthropic
/// `input_tokens`/`output_tokens` + chiavi cache opzionali). Questa forma e'
/// letta da `billing::extract_usage_numbers` (cerca `input_tokens`/`output_tokens`
/// sotto `metadata.usage` per le completion, sotto `usage` per gli agent-turn) e
/// da `nexus-wiki::extract_usage_tokens`.
fn usage_value_from_gw(resp: &GwResponse) -> Value {
    let mut usage = serde_json::Map::new();
    usage.insert("input_tokens".to_string(), json!(resp.usage.input_tokens));
    usage.insert("output_tokens".to_string(), json!(resp.usage.output_tokens));
    if let Some(cr) = resp.usage.cache_read_tokens {
        if cr > 0 {
            usage.insert("cache_read_tokens".to_string(), json!(cr));
        }
    }
    if let Some(cc) = resp.usage.cache_creation_tokens {
        if cc > 0 {
            usage.insert("cache_creation_tokens".to_string(), json!(cc));
        }
    }
    Value::Object(usage)
}

/// Forma `Value` di `generate_completion` (success), paritetica a
/// `neural_service.py::GenerateCompletion`: `{provider, model, content,
/// metadata:{usage}, error:null, error_class:null}`. Il provider/model "usati"
/// dal gateway prevalgono su quelli richiesti (il pin li conferma comunque).
fn completion_value_from_gw(provider: &str, model: &str, resp: &GwResponse) -> Value {
    let used_provider = if resp.provider_used.is_empty() {
        provider
    } else {
        resp.provider_used.as_str()
    };
    let used_model = if resp.model_used.is_empty() {
        model
    } else {
        resp.model_used.as_str()
    };
    json!({
        "provider": used_provider,
        "model": used_model,
        "content": resp.content,
        "metadata": {
            "usage": usage_value_from_gw(resp),
        },
        "error": Value::Null,
        "error_class": Value::Null,
    })
}

/// Forma `Value` d'errore di `generate_completion`, paritetica al ramo `except`
/// del brain: `content` "[Error: ...]", `error` umano, `error_class` dal punto
/// unico Rust `provider_error_classifier`. `completion_has_error` (core) e la
/// detection "errore ingoiato" di `provider_health_probe` dipendono da questa
/// forma (prefisso `[Error:` + `error`/`error_class` non-null).
fn error_completion_value(provider: &str, model: &str, raw_error: &str) -> Value {
    let class = crate::provider_error_classifier::classify_text(raw_error).stop_reason;
    json!({
        "provider": provider,
        "model": model,
        "content": format!("[Error: {raw_error}]"),
        "metadata": {
            "usage": json!({ "input_tokens": 0, "output_tokens": 0 }),
            "error": raw_error,
            "error_class": class,
        },
        "error": raw_error,
        "error_class": class,
    })
}

/// Converte i `tool_calls` (dialetto OpenAI) della `GwResponse` nei blocchi
/// interni `(tool_use_blocks, assistant_tool_blocks)`, replicando
/// `gateway_provider.py::_tool_calls_to_blocks`. Ogni tool-call
/// `{id, function:{name, arguments(JSON string)}}` -> `{id, name, input(dict)}`
/// (e il corrispondente blocco assistant `{type:"tool_use", ...}`).
fn tool_blocks_from_gw(resp: &GwResponse) -> (Vec<Value>, Vec<Value>) {
    let mut tool_use_blocks: Vec<Value> = Vec::new();
    let mut assistant_blocks: Vec<Value> = Vec::new();
    for tc in resp.tool_calls.iter().flatten() {
        let input: Value =
            serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| json!({}));
        let block = json!({
            "id": tc.id,
            "name": tc.function.name,
            "input": input,
        });
        tool_use_blocks.push(block.clone());
        let mut assistant = serde_json::Map::new();
        assistant.insert("type".to_string(), json!("tool_use"));
        if let Value::Object(map) = block {
            for (k, v) in map {
                assistant.insert(k, v);
            }
        }
        assistant_blocks.push(Value::Object(assistant));
    }
    (tool_use_blocks, assistant_blocks)
}

/// Forma `Value` di `generate_agent_turn` (success), paritetica a
/// `neural_service.py::GenerateAgentTurn` + `gateway_provider.py::_build_agent_result`:
/// `{provider, model, content, stop_reason, tool_use_blocks, assistant_content,
/// usage, error:null, error_class:null}`. `stop_reason` = "tool_use" se ci sono
/// tool-call, altrimenti "end_turn" (come il brain, che NON propaga il
/// finish_reason grezzo qui ma lo deriva dalla presenza di tool-call).
fn agent_turn_value_from_gw(provider: &str, model: &str, resp: &GwResponse) -> Value {
    let used_provider = if resp.provider_used.is_empty() {
        provider
    } else {
        resp.provider_used.as_str()
    };
    let used_model = if resp.model_used.is_empty() {
        model
    } else {
        resp.model_used.as_str()
    };
    let (tool_use_blocks, tool_assistant_blocks) = tool_blocks_from_gw(resp);

    // assistant_content: blocco text (se content non vuoto) + blocchi tool_use,
    // nello stesso ordine del brain (`_build_agent_result`).
    let mut assistant_content: Vec<Value> = Vec::new();
    if !resp.content.is_empty() {
        assistant_content.push(json!({ "type": "text", "text": resp.content }));
    }
    assistant_content.extend(tool_assistant_blocks);

    let stop_reason = if tool_use_blocks.is_empty() {
        "end_turn"
    } else {
        "tool_use"
    };

    json!({
        "provider": used_provider,
        "model": used_model,
        "content": resp.content,
        "stop_reason": stop_reason,
        "tool_use_blocks": tool_use_blocks,
        "assistant_content": assistant_content,
        "usage": usage_value_from_gw(resp),
        "error": Value::Null,
        "error_class": Value::Null,
    })
}

/// Forma `Value` d'errore di `generate_agent_turn`, paritetica al ramo `except`
/// del brain: `stop_reason="error"` + `error`/`error_class`. `evaluate_tool_probe`
/// (model_health_probe) legge `error_class` e `stop_reason="error"`.
/// Estrae la classe d'errore dal segnale STRUTTURATO del gateway, se presente
/// (regola M). Percorre la catena di `anyhow` cercando un [`GatewayHttpError`] e ne
/// legge `details.primary_cause`, la classificazione fatta dal gateway ALLA FONTE.
/// `None` -> il chiamante ricade sul classificatore testuale (comportamento storico).
///
/// PUNTO UNICO del ponte segnale-strutturato -> `error_class` per il path neural: la
/// mappa cause->classe vive in `provider_error_classifier` (regola L), qui c'e' solo
/// l'estrazione.
fn structured_error_class(err: &anyhow::Error) -> Option<&'static str> {
    let gw_err = err
        .chain()
        .find_map(|c| c.downcast_ref::<crate::nexus_gateway::GatewayHttpError>())?;
    let cause = gw_err
        .details
        .as_ref()?
        .get("primary_cause")?
        .as_str()?;
    crate::provider_error_classifier::error_class_from_primary_cause(cause)
}

/// Come [`error_agent_turn_value`] ma con la classe gia' derivata da un segnale
/// strutturato: se `structured` e' `Some`, vince sul classificatore testuale (che
/// sul Display del gateway non puo' ricostruire la causa reale).
fn error_agent_turn_value_with_class(
    provider: &str,
    model: &str,
    raw_error: &str,
    structured: Option<&'static str>,
) -> Value {
    match structured {
        Some(class) => error_agent_turn_value_inner(provider, model, raw_error, class.to_string()),
        None => error_agent_turn_value(provider, model, raw_error),
    }
}

fn error_agent_turn_value(provider: &str, model: &str, raw_error: &str) -> Value {
    let class = crate::provider_error_classifier::classify_text(raw_error).stop_reason;
    error_agent_turn_value_inner(provider, model, raw_error, class)
}

fn error_agent_turn_value_inner(
    provider: &str,
    model: &str,
    raw_error: &str,
    class: String,
) -> Value {
    json!({
        "provider": provider,
        "model": model,
        "content": format!("[Error: {raw_error}]"),
        "stop_reason": "error",
        "tool_use_blocks": [],
        "assistant_content": [],
        "usage": json!({ "input_tokens": 0, "output_tokens": 0 }),
        "error": raw_error,
        "error_class": class,
    })
}

/// Converte un messaggio grezzo `{role, content, [tool_calls], [tool_call_id]}`
/// in `GwMessage`. I call site passano sempre `{role, content}` testuali; i campi
/// tool sono preservati se presenti (robustezza, retrocompatibile col contratto
/// gateway). `role`/`content` mancanti -> default difensivi.
fn gw_message_from_value(v: Value) -> GwMessage {
    let role = v
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user")
        .to_string();
    let content = v.get("content").cloned().unwrap_or_else(|| json!(""));
    let tool_calls = v
        .get("tool_calls")
        .and_then(|tc| serde_json::from_value(tc.clone()).ok());
    let tool_call_id = v
        .get("tool_call_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    // Round-trip reasoning DeepSeek: se la history porta il reasoning_content di
    // un turno assistant precedente, lo ri-passiamo (vincolo HTTP 400). Il server
    // lo inoltra solo al dialetto DeepSeek.
    let reasoning = v
        .get("reasoning")
        .and_then(Value::as_str)
        .map(str::to_string);
    GwMessage {
        role,
        content,
        tool_calls,
        tool_call_id,
        reasoning,
        thinking_signature: None,
    }
}

#[cfg(test)]
mod gateway_mapping_tests {
    use super::*;
    use crate::nexus_gateway::{GwToolCall, GwToolFunctionCall, GwUsage};

    fn base_resp() -> GwResponse {
        GwResponse {
            content: String::new(),
            tool_calls: None,
            usage: GwUsage {
                input_tokens: 12,
                output_tokens: 34,
                cache_read_tokens: None,
                cache_creation_tokens: None,
            },
            model_used: "m-real".to_string(),
            provider_used: "anthropic".to_string(),
            latency_ms: 5,
            finish_reason: "stop".to_string(),
            privacy_rerouted: None,
            reasoning: None,
            thinking_signature: None,
            citations: None,
        }
    }

    #[test]
    fn completion_preserva_content_usage_e_chiavi_errore_null() {
        let mut resp = base_resp();
        resp.content = "ciao".to_string();
        let v = completion_value_from_gw("openai", "gpt-x", &resp);
        // content leggibile dai call site.
        assert_eq!(v["content"], "ciao");
        // billing::extract_usage_numbers legge metadata.usage.{input,output}_tokens.
        assert_eq!(v["metadata"]["usage"]["input_tokens"], 12);
        assert_eq!(v["metadata"]["usage"]["output_tokens"], 34);
        // completion_has_error: error/metadata.error null -> nessun errore.
        assert!(v["error"].is_null());
        assert!(v["metadata"]["error"].is_null());
        // provider/model "usati" dal gateway prevalgono.
        assert_eq!(v["provider"], "anthropic");
        assert_eq!(v["model"], "m-real");
    }

    #[test]
    fn completion_errore_ha_prefisso_error_e_classe() {
        let v = error_completion_value("anthropic", "claude-x", "HTTP 401 invalid api key");
        let content = v["content"].as_str().unwrap();
        assert!(content.starts_with("[Error:"));
        assert!(!v["error"].is_null());
        // 401 -> auth_error dal punto unico classifier.
        assert_eq!(v["error_class"], "auth_error");
        assert_eq!(v["metadata"]["error_class"], "auth_error");
    }

    #[test]
    fn agent_turn_testuale_stop_reason_end_turn_e_no_tool_blocks() {
        let mut resp = base_resp();
        resp.content = "solo testo".to_string();
        let v = agent_turn_value_from_gw("openai", "gpt-x", &resp);
        assert_eq!(v["content"], "solo testo");
        assert_eq!(v["stop_reason"], "end_turn");
        assert!(v["tool_use_blocks"].as_array().unwrap().is_empty());
        // assistant_content: un solo blocco text.
        let ac = v["assistant_content"].as_array().unwrap();
        assert_eq!(ac.len(), 1);
        assert_eq!(ac[0]["type"], "text");
        // usage top-level (extract_usage_tokens degli agent-turn).
        assert_eq!(v["usage"]["input_tokens"], 12);
    }

    #[test]
    fn agent_turn_con_tool_call_stop_reason_tool_use_e_blocchi_per_probe() {
        let mut resp = base_resp();
        resp.content = "procedo".to_string();
        resp.tool_calls = Some(vec![GwToolCall {
            id: "call_1".to_string(),
            kind: "function".to_string(),
            function: GwToolFunctionCall {
                name: "nexus_probe_tool".to_string(),
                arguments: r#"{"ok":true}"#.to_string(),
            },
            thought_signature: None,
        }]);
        let v = agent_turn_value_from_gw("anthropic", "claude-x", &resp);
        // model_health_probe::evaluate_tool_probe: stop_reason + tool_use_blocks[].name.
        assert_eq!(v["stop_reason"], "tool_use");
        let tub = v["tool_use_blocks"].as_array().unwrap();
        assert_eq!(tub.len(), 1);
        assert_eq!(tub[0]["name"], "nexus_probe_tool");
        assert_eq!(tub[0]["id"], "call_1");
        assert_eq!(tub[0]["input"]["ok"], true);
        // assistant_content: blocco text + blocco tool_use.
        let ac = v["assistant_content"].as_array().unwrap();
        assert_eq!(ac.len(), 2);
        assert_eq!(ac[0]["type"], "text");
        assert_eq!(ac[1]["type"], "tool_use");
        assert_eq!(ac[1]["name"], "nexus_probe_tool");
    }

    #[test]
    fn agent_turn_errore_stop_reason_error_e_classe() {
        let v = error_agent_turn_value("google", "gemini-x", "429 rate limit exceeded");
        assert_eq!(v["stop_reason"], "error");
        assert_eq!(v["error_class"], "rate_limit");
        assert!(v["tool_use_blocks"].as_array().unwrap().is_empty());
    }

    #[test]
    fn gw_message_da_value_testuale_e_con_tool() {
        let m = gw_message_from_value(json!({"role":"user","content":"hi"}));
        assert_eq!(m.role, "user");
        assert_eq!(m.content, json!("hi"));
        assert!(m.tool_calls.is_none());
        // role mancante -> default user; content mancante -> stringa vuota.
        let d = gw_message_from_value(json!({}));
        assert_eq!(d.role, "user");
        assert_eq!(d.content, json!(""));
    }
}
