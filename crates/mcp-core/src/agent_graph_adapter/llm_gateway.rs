//! Adapter del trait [`nexus_agent_graph::runtime::ports::LlmGateway`].
//!
//! Implementa `LlmGateway::complete` delegando al gateway LLM concreto di mcp-core
//! ([`crate::nexus_gateway::NexusGatewayClient`], HTTP verso il Nexus LLM Gateway —
//! catena Fallback DB-driven). Il provider/model arrivano gia' risolti nella
//! [`LlmRequest`] (regola G): l'adapter li inoltra al gateway via `pin_provider`,
//! mai li sceglie/hardcoda.
//!
//! RISCHIO NOTO (memoria progetto "Gateway droppava tool_choice" / "google tool
//! monco"): l'impl DEVE onorare `force_tool_choice` end-to-end. La mappatura
//! `force_tool_choice -> tool_choice` viaggia nel campo `GwRequest::tool_choice`
//! (stringa OpenAI `"required"`/`"none"`, omesso = `auto`) che il server gateway
//! propaga al provider; le `tool_calls` tornano in `GwResponse::tool_calls` e
//! diventano `LlmResponse::tool_calls` + `assistant_content` (blocchi
//! `anthropic_content`) per ricostruire il `Message::Ai` con i tool_use originali.
//! Se uno dei due lati si "perdesse" il force-action anti-loop sarebbe
//! neutralizzato: i test before/after lo coprono esplicitamente.
//!
//! ## Due impl coesistono per ruolo (regola L, punto unico LLM)
//!
//! - [`GatewayLlmAdapter`] (primario / cutover): completion REAL. IGNORA
//!   `LlmRequest::purpose` — un turno REAL non cambia comportamento.
//! - [`ReplayLlmGateway`] (SHADOW, read-only): NON chiama l'LLM. Per la chiamata
//!   dell'executor RIGIOCA la sequenza di tool del run PRIMARIO letta da
//!   `agent_steps` (cosi' `num_tool_calls` converge col primario e le divergenze
//!   residue sono BUG VERI del grafo, non artefatti LLM); per le chiamate
//!   ausiliarie (planner/reflection/clarify_expand) ritorna una risposta NEUTRA
//!   deterministica (costo zero, zero RNG-divergenza). Lo switch per ruolo e' nel
//!   PUNTO UNICO `native_engine::build_native_engine` (come per `ToolExecutor`).

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::sync::{Mutex, OnceCell};
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{LlmGateway, LlmRequest, LlmResponse, LlmUsage, PortError};
use nexus_agent_graph::state::ToolUse;

use crate::nexus_gateway::{
    GwMessage, GwMetadata, GwRequest, GwResponse, GwThinkingConfig, GwToolCall, GwToolFunctionCall,
    NexusGatewayClient,
};

/// Adapter [`LlmGateway`] -> [`NexusGatewayClient`].
pub struct GatewayLlmAdapter {
    /// Client del gateway LLM concreto a cui la `complete` delega.
    gateway: NexusGatewayClient,
    /// Pool del meta-DB per la risoluzione purpose->modello (`nexus_purpose_model`)
    /// delle chiamate ausiliarie che arrivano con `model` vuoto (regola G).
    db: sqlx::PgPool,
    /// Project id (UUID stringa) del run -> `GwMetadata.tenant_id`. Senza, il
    /// gateway NON registra l'usage nel ledger (record_usage_to_ledger esce se
    /// tenant_id e' vuoto): era la causa del "costo sempre a 0" post-cutover.
    project_id: String,
    /// User id (UUID stringa) owner del run -> `GwMetadata.user_id`.
    user_id: String,
}

impl GatewayLlmAdapter {
    /// Costruisce l'adapter sul client gateway concreto con il meta-DB (purpose
    /// resolver) e l'identita' del run (project_id/user_id) per il ledger billing.
    pub fn new(
        gateway: NexusGatewayClient,
        db: sqlx::PgPool,
        project_id: String,
        user_id: String,
    ) -> Self {
        Self {
            gateway,
            db,
            project_id,
            user_id,
        }
    }
}

/// Se la richiesta arriva con `model` vuoto, ritorna il purpose da risolvere via
/// `nexus_purpose_model`; errore chiaro se manca anche il purpose. I nodi
/// ausiliari del grafo (es. clarify_or_expand) inviano deliberatamente
/// provider/model vuoti + `purpose` contando su questa risoluzione (regola G:
/// modello dal DB, mai hardcoded). Prima di questo guard la richiesta vuota
/// arrivava al gateway as-is e la cascata falliva su TUTTI i provider con
/// 400/404 fuorvianti ("you must provide a model parameter", incidente
/// 2026-07-02). MAI inoltrare un modello vuoto.
fn purpose_for_empty_model(req: &LlmRequest) -> Result<Option<String>, PortError> {
    if !req.model.trim().is_empty() {
        return Ok(None);
    }
    match req
        .purpose
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        Some(p) => Ok(Some(p.to_string())),
        None => Err(PortError::Llm(
            "richiesta LLM senza modello ne' purpose: il chiamante deve risolvere \
             provider/modello a monte (routing matrix) o indicare un purpose \
             (nexus_purpose_model, regola G)"
                .to_string(),
        )),
    }
}

#[async_trait]
impl LlmGateway for GatewayLlmAdapter {
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, PortError> {
        let mut req = req;
        if let Some(purpose) = purpose_for_empty_model(&req)? {
            // Punto unico della risoluzione purpose->(provider, model): regola L,
            // `into_model` mappa gli esiti non risolvibili in errore leggibile
            // (niente fallback hardcoded, regola G).
            let (provider, model) =
                crate::internal_routing::resolve_purpose_model_db(&self.db, &purpose)
                    .await
                    .into_model(&purpose)
                    .map_err(PortError::Llm)?;
            tracing::debug!(
                purpose = %purpose,
                provider = %provider,
                model = %model,
                "llm adapter: modello risolto dal purpose (richiesta ausiliaria)"
            );
            req.provider = provider;
            req.model = model;
        }
        let mut gw_req = build_gw_request(&req);
        // Identita' del run per il ledger di billing. `build_gw_request` e' una fn
        // pura (testata) che lascia tenant_id/user_id vuoti; li popoliamo qui dal
        // contesto del run iniettato in `new()`. Senza questo il gateway scarta la
        // registrazione usage (record_usage_to_ledger return su tenant vuoto) e il
        // costo risultava sempre 0. request_id = run_id e' gia' valorizzato.
        gw_req.metadata.tenant_id = self.project_id.clone();
        gw_req.metadata.user_id = self.user_id.clone();
        let resp = self
            .gateway
            .complete(gw_req)
            .await
            .map_err(|e| classify_gateway_error(&e))?;
        let mut mapped = map_gw_response(resp);
        // Costo REALE del turno (regola M: token STRUTTURATI dell'usage x prezzo del
        // modello EFFETTIVO dal catalog). Il gateway non lo riporta nella response
        // (lo calcola solo per il ledger a valle), quindi map_gw_response lo lascia a
        // None: lo popoliamo QUI, dove il catalog e' accessibile (self.db), cosi' il
        // motore lo accumula per il cap in dollari del RUN. Best-effort: prezzo ignoto
        // (modello non in catalog) -> resta None (nessun cap spurio, regola G).
        if mapped.usage.total_cost_usd.is_none() {
            if let Some(model) = mapped.model_used.clone() {
                let provider = mapped
                    .provider_used
                    .clone()
                    .unwrap_or_else(|| req.provider.clone());
                mapped.usage.total_cost_usd =
                    turn_cost_usd(&self.db, &provider, &model, &mapped.usage).await;
            }
        }
        Ok(mapped)
    }
}

/// Classifica l'errore del gateway concreto nella variante di porta corretta.
/// PUNTO UNICO (regola L) della traduzione errore-gateway -> [`PortError`]: e'
/// l'unico confine fra il formato d'errore del `NexusGatewayClient` e la porta.
///
/// SEGNALE STRUTTURATO via DOWNCAST (regola M): il client tipizza gli HTTP
/// non-2xx in [`crate::nexus_gateway::GatewayHttpError`] con `code` e `details`
/// gia' estratti dal body JSON — qui si decide sui CAMPI, mai sul testo:
///   - `code = PROVIDER_ERROR` (500 aggregato "tutti i provider hanno fallito")
///     -> [`PortError::ProviderUnavailable`] con la CAUSA da
///     `details.primary_cause` (classe del primo fallimento: cooldown /
///     billing / client_error / transient), cosi' l'executor tenta il FALLBACK
///     cross-provider E il meta-step racconta il motivo vero;
///   - `code = POLICY_TIER_EXCLUDED` (403: provider escluso dalla policy per il
///     sensitivity tier del contenuto) -> `ProviderUnavailable` con causa
///     `PolicyTierExcluded`: anche qui il failover e' la risposta giusta (un
///     altro provider ammesso), con racconto onesto "contenuto riservato";
///   - ogni altro errore (4xx di richiesta, parse, HTTP) resta un
///     [`PortError::Llm`] generico (chiusura come prima).
/// Gateway vecchio senza `details` -> causa `Unknown` (retro-compatibile).
fn classify_gateway_error(err: &anyhow::Error) -> PortError {
    use nexus_agent_graph::runtime::ports::{ProviderFailureCause, ProviderUnavailableInfo};

    let Some(http) = err
        .chain()
        .find_map(|c| c.downcast_ref::<crate::nexus_gateway::GatewayHttpError>())
    else {
        return PortError::Llm(err.to_string());
    };
    match http.code.as_deref() {
        Some("PROVIDER_ERROR") => {
            let cause = match http
                .details
                .as_ref()
                .and_then(|d| d.get("primary_cause"))
                .and_then(|c| c.as_str())
            {
                // "transient": la chiamata e' appena fallita per errore
                // transitorio e il gateway ha messo il provider in cooldown
                // breve -> per il racconto equivale a un cooldown.
                Some("cooldown") | Some("transient") => ProviderFailureCause::Cooldown,
                Some("billing") | Some("cooldown_billing") => ProviderFailureCause::Billing,
                Some("client_error") => ProviderFailureCause::ClientError,
                // 200 degenere (content vuoto, zero tool-call, finish non
                // terminale): il provider e' sano ma il turno e' improduttivo.
                // Causa dedicata cosi' l'executor RIPIEGA cross-provider (ramo
                // else != ClientError), invece di chiudere con un hollow turn.
                Some("empty_completion") => ProviderFailureCause::EmptyCompletion,
                _ => ProviderFailureCause::Unknown,
            };
            PortError::ProviderUnavailable(ProviderUnavailableInfo::new(cause, err.to_string()))
        }
        Some("POLICY_TIER_EXCLUDED") => PortError::ProviderUnavailable(
            ProviderUnavailableInfo::new(ProviderFailureCause::PolicyTierExcluded, err.to_string()),
        ),
        _ => PortError::Llm(err.to_string()),
    }
}

/// Mappa una [`LlmRequest`] (porta) nel [`GwRequest`] del client gateway.
///
/// - `provider` -> `pin_provider` (provider gia' risolto a monte dalla routing
///   matrix, regola G: il gateway non deve re-instradare) + `model` prefissato
///   `provider/model` (il pin server strippa il prefisso);
/// - `system_text` -> primo messaggio `role:"system"` (il server estrae il system
///   come campo separato per i provider che lo richiedono, es. Anthropic);
/// - `force_tool_choice` -> `tool_choice` (`Some(true)`->`"required"`,
///   `Some(false)`->`"none"`, `None`->omesso = `auto`);
/// - `tools` -> schema OpenAI atteso dal server (conversione da Anthropic-style se
///   necessario, vedi [`tools_to_openai_schema`]).
fn build_gw_request(req: &LlmRequest) -> GwRequest {
    let mut messages: Vec<GwMessage> = Vec::with_capacity(req.messages.len() + 1);

    // System come PRIMO messaggio con role "system" (forma che il server normalizza
    // per provider). Solo se non vuoto e se non c'e' gia' un system nei messaggi.
    let has_system_msg = req.messages.iter().any(|m| m.role == "system");
    if let Some(sys) = req.system_text.as_ref() {
        if !sys.is_empty() && !has_system_msg {
            messages.push(GwMessage {
                role: "system".to_string(),
                content: Value::String(sys.clone()),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                thinking_signature: None,
            });
        }
    }
    for m in &req.messages {
        // CONTINUITA' TOOL MULTI-TURN (regola L): preserva tool_calls (turno
        // assistant con tool) e tool_call_id (turno tool/risultato) end-to-end.
        // I `ToolUse` della porta (id/name/input) diventano `GwToolCall` in forma
        // OpenAI Chat Completions (`{id, type:"function", function:{name,
        // arguments-stringa-JSON}}`), che il server mappa nei block `tool_use`
        // Anthropic. Senza questo i tool_use finivano appiattiti in content e i
        // tool_result perdevano l'id -> HTTP 400.
        let tool_calls = m.tool_calls.as_ref().map(|calls| {
            calls
                .iter()
                .map(|tc| GwToolCall {
                    id: tc.id.clone(),
                    kind: "function".to_string(),
                    function: GwToolFunctionCall {
                        name: tc.name.clone(),
                        // arguments e' una STRINGA JSON (contratto OpenAI).
                        arguments: serde_json::to_string(&tc.input)
                            .unwrap_or_else(|_| "{}".to_string()),
                    },
                    // ROUND-TRIP firma PER-CALL (Gemini 3): la thoughtSignature
                    // catturata in risposta viaggia allegata alla ToolUse e va
                    // ri-passata identica sulla stessa functionCall, altrimenti
                    // HTTP 400 INVALID_ARGUMENT. Inerte per gli altri provider.
                    thought_signature: tc.thought_signature.clone(),
                })
                .collect::<Vec<_>>()
        });
        messages.push(GwMessage {
            role: m.role.clone(),
            content: m.content.clone(),
            tool_calls,
            tool_call_id: m.tool_call_id.clone(),
            // ROUND-TRIP reasoning (DeepSeek): il reasoning_content di un turno
            // assistant in thinking mode VA ri-passato (vincolo HTTP 400). Il
            // server lo inoltra SOLO al dialetto DeepSeek; per gli altri provider
            // resta inerte. Speculare al thinking_signature Anthropic.
            reasoning: m.reasoning.clone(),
            // ROUND-TRIP thinking_signature (Anthropic, per-messaggio): la firma
            // del blocco thinking VA ri-passata nei turni con tool (HTTP 400
            // senza). Il server la inoltra SOLO ad Anthropic; inerte altrove.
            thinking_signature: m.thinking_signature.clone(),
        });
    }

    // Modello prefissato col provider risolto: il pin server fa lo strip del
    // prefisso "provider/". Coerente con il path pin di routes.rs.
    let model = if req.provider.is_empty() {
        req.model.clone()
    } else {
        format!("{}/{}", req.provider, req.model)
    };

    GwRequest {
        model,
        messages,
        max_tokens: req.max_tokens.and_then(|m| u32::try_from(m).ok()),
        temperature: None,
        tools: req.tools.as_ref().map(|t| tools_to_openai_schema(t)),
        response_format: req.response_format.clone(),
        thinking: req.thinking.as_ref().map(|t| GwThinkingConfig {
            enabled: t.enabled,
            budget_tokens: t.budget_tokens,
        }),
        tool_choice: force_tool_choice_to_value(req.force_tool_choice),
        pin_provider: if req.provider.is_empty() {
            None
        } else {
            Some(req.provider.clone())
        },
        metadata: GwMetadata {
            tenant_id: String::new(),
            user_id: "system".to_string(),
            request_id: req.run_id.clone().unwrap_or_default(),
            sensitivity_tier: 0,
            feature: req.intent.clone().unwrap_or_else(|| "agent".to_string()),
        },
    }
}

/// Mappa `force_tool_choice` al valore `tool_choice` in stile OpenAI atteso dal
/// gateway. PUNTO CRITICO del force-action: `Some(true)` DEVE produrre
/// `"required"` (il modello e' OBBLIGATO a chiamare un tool); `Some(false)` ->
/// `"none"` (turno puramente testuale); `None` -> omesso (= `auto`).
fn force_tool_choice_to_value(force: Option<bool>) -> Option<Value> {
    match force {
        Some(true) => Some(Value::String("required".to_string())),
        Some(false) => Some(Value::String("none".to_string())),
        None => None,
    }
}

/// Normalizza i tool al formato OpenAI Chat Completions atteso dal server gateway
/// (`[{type:"function", function:{name, description?, parameters}}]`).
///
/// I tool del sistema Nexus sono Anthropic-style (`{name, description,
/// input_schema}`, vedi `AGENT_TOOLS_JSON`): vanno convertiti
/// (`input_schema`->`function.parameters`). Un tool gia' in formato OpenAI
/// (chiave `function` presente) e' lasciato invariato. PUNTO UNICO della
/// conversione (regola L): un solo posto traduce lo schema verso il gateway.
/// Riusato anche da `NeuralCoreClient::generate_agent_turn` (`tools_json`
/// Anthropic-style dei probe/discovery) per non duplicare la traduzione.
pub(crate) fn tools_to_openai_schema(tools: &[Value]) -> Value {
    let converted: Vec<Value> = tools
        .iter()
        .map(|t| {
            // Gia' OpenAI-style: passthrough.
            if t.get("function").is_some() {
                return t.clone();
            }
            // Anthropic-style -> OpenAI.
            let name = t.get("name").cloned().unwrap_or(Value::Null);
            let description = t.get("description").cloned();
            let parameters = t
                .get("input_schema")
                .or_else(|| t.get("parameters"))
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"}));
            let mut function = json!({ "name": name, "parameters": parameters });
            if let Some(desc) = description {
                function["description"] = desc;
            }
            json!({ "type": "function", "function": function })
        })
        .collect();
    Value::Array(converted)
}

/// Normalizza il `finish_reason` del gateway (vocabolario wire OpenAI-canonico
/// prodotto da `nexus-gateway`: tutti i provider — anthropic/openai/mistral/google/
/// deepseek — vengono ricondotti a `tool_calls`/`length`/`stop` lato server) al
/// vocabolario della porta [`nexus_agent_graph::runtime::ports::LlmResponse`]
/// (`tool_use`/`max_tokens`/`end_turn`), che e' lo stesso atteso dal punto unico
/// `stop_reason_from_str` dell'executor (mappato 1:1 sul vocabolario Anthropic).
///
/// CAUSA DEL BUG "hollow primario" (2026-06-27): il gateway ritorna
/// `finish_reason="tool_calls"` quando il modello chiama un tool, ma l'executor
/// riconosceva solo `"tool_use"` (Anthropic-style) e cadeva nel default
/// `_ => EndTurn`: il turno con tool_call diventava una chiusura, il
/// `tool_dispatch` veniva saltato (`route_after_executor` instrada al dispatch SOLO
/// su `StopReason::ToolUse`), il run finiva a `end_turn` con 0 step e content vuoto.
/// Lo SHADOW non lo coglieva perche' il `ReplayLlmGateway` costruisce gia'
/// `stop_reason="tool_use"` a mano (non passa dal `finish_reason` del gateway reale).
///
/// PUNTO UNICO (regola L): la traduzione wire->porta vive qui, l'unico confine fra
/// il formato del gateway e la porta `LlmResponse`. Valori ignoti -> passthrough
/// (robustezza: niente magic, una stringa sconosciuta cade poi sul default
/// `end_turn` del punto unico executor, come oggi).
fn normalize_gw_finish_reason(finish: &str) -> String {
    match finish {
        "tool_calls" => "tool_use".to_string(),
        "length" => "max_tokens".to_string(),
        "stop" => "end_turn".to_string(),
        other => other.to_string(),
    }
}

/// Mappa la [`GwResponse`] del gateway nella [`LlmResponse`] della porta.
///
/// - `tool_calls`: `GwToolCall` -> [`ToolUse`] (l'`arguments` stringa JSON e'
///   parsato in `input`); inoltre ricostruisce `assistant_content` (blocchi
///   `anthropic_content`: `{type:"text",...}` + `{type:"tool_use", id, name,
///   input}`) per il `Message::Ai` con i tool_use originali (continuita'
///   tool_use/tool_result);
/// - `provider_used`/`model_used`: gli EFFETTIVI post cascade/sticky del gateway;
/// - `usage`: cache_read/creation mappati (telemetria token);
/// - `finish_reason` -> `stop_reason` NORMALIZZATO al vocabolario della porta
///   ([`normalize_gw_finish_reason`]): `tool_calls`->`tool_use` e' load-bearing per
///   instradare al `tool_dispatch` (vedi nota sul bug hollow).
/// Costo in USD del turno = `prompt_tokens * prezzo_input + completion_tokens *
/// prezzo_output` (per milione) dal catalog (regola G/M: prezzo dal DB, token dal
/// segnale strutturato). `None` se il prezzo e' ignoto (modello non in catalog o
/// errore) -> nessun cap spurio. Cast a `double precision` cosi' una colonna
/// NUMERIC arriva come `f64`. Nessuna cache: una query leggera per turno LLM, che
/// e' gia' latente in secondi (overhead trascurabile).
async fn turn_cost_usd(db: &PgPool, provider: &str, model: &str, usage: &LlmUsage) -> Option<f64> {
    let (in_cost, out_cost): (f64, f64) = sqlx::query_as(
        "SELECT input_cost_per_million_tokens::double precision, \
                output_cost_per_million_tokens::double precision \
         FROM ai_price_catalog WHERE provider = $1 AND model = $2 LIMIT 1",
    )
    .bind(provider)
    .bind(model)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()?;
    let input = usage.prompt_tokens.max(0) as f64;
    let output = usage.completion_tokens.max(0) as f64;
    Some((input * in_cost + output * out_cost) / 1_000_000.0)
}

fn map_gw_response(resp: GwResponse) -> LlmResponse {
    let tool_calls: Vec<ToolUse> = resp
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|tc| ToolUse {
            id: tc.id,
            name: tc.function.name,
            // arguments e' una STRINGA JSON: la parsiamo in Value; se vuota/invalida -> {}.
            input: serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| json!({})),
            // Firma PER-CALL (Gemini 3): la conserviamo sulla ToolUse cosi'
            // fluisce nella history e viene ri-passata nel turno successivo.
            thought_signature: tc.thought_signature,
        })
        .collect();

    // assistant_content (blocchi anthropic_content) per la continuita': blocco
    // text (se non vuoto) seguito dai blocchi tool_use; vuoto quando non c'e'
    // alcun tool_use (l'executor ricostruisce dal solo `content`). PUNTO UNICO
    // della costruzione (regola L): stesso helper usato dal replay.
    let assistant_content = assistant_content_for_tool_calls(&resp.content, &tool_calls);

    LlmResponse {
        content: resp.content,
        tool_calls,
        usage: LlmUsage {
            prompt_tokens: resp.usage.input_tokens as i64,
            completion_tokens: resp.usage.output_tokens as i64,
            total_tokens: (resp.usage.input_tokens + resp.usage.output_tokens) as i64,
            cache_creation_tokens: resp.usage.cache_creation_tokens.map(|v| v as i64),
            cache_read_tokens: resp.usage.cache_read_tokens.map(|v| v as i64),
            // Il /v1/complete del gateway NON riporta il costo del turno (il ledger
            // lo calcola lato gateway, non e' nella response). Resta None: il
            // chiamante lo derivera' altrove se serve (regola G: niente magic).
            total_cost_usd: None,
        },
        provider_used: Some(resp.provider_used),
        model_used: Some(resp.model_used),
        assistant_content,
        stop_reason: Some(normalize_gw_finish_reason(&resp.finish_reason)),
        // Pensiero aggregato dal gateway (prima SCARTATO -> ThinkingBlock vuoto):
        // GwResponse.reasoning esiste, qui smettiamo solo di buttarlo (regola L).
        reasoning: resp.reasoning,
        // Firma thinking Anthropic (per-messaggio): ora LETTA e propagata nella
        // history per il round-trip (prima scartata: GwResponse era dead_code).
        thinking_signature: resp.thinking_signature,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  ReplayLlmGateway — gateway di REPLAY per lo shadow (read-only, costo zero)
// ═══════════════════════════════════════════════════════════════════════════

/// Una riga `agent_steps` del run PRIMARIO rilevante per il replay dell'executor.
///
/// `step_index` deterministico (mig 0009 / [`crate::agent_graph_adapter::
/// agent_step_store`]): per un primario PYTHON e' `iteration*1000 + idx_locale`
/// (lo shadow ha SEMPRE un primario Python — `run_via_brain`). `tool_name` e'
/// LETTERALE: per i tool wrappati e' `"nexus_mcp_tool_call"` (il vero tool sta in
/// `tool_input.tool_name`) — il replay NON lo spacchetta, restituisce la colonna
/// `tool_name` cosi' com'e' (vedi [`ReplayLlmGateway`]).
///
/// `created_at_us`: timestamp di scrittura in MICROSECONDI epoch. E' il
/// discriminante del TURNO REALE: gli step di uno stesso turno LLM del primario
/// sono scritti nello stesso batch -> `created_at` quasi-identico (osservato:
/// identico al microsecondo per i batch INSERT singoli, fino a ~2-3ms di jitter per
/// INSERT separati nello stesso burst); turni/ondate diversi sono distanti >=
/// centinaia di ms (osservato: minimo ~356ms, tipico secondi). Vedi
/// [`group_steps_by_turn`].
#[derive(Debug, Clone)]
struct ReplayStep {
    /// Indice globale dello step (`iteration*1000 + idx_locale` per i primari Python).
    step_index: i64,
    /// Nome del tool LETTERALE (colonna `tool_name`, non spacchettato).
    tool_name: String,
    /// Argomenti del tool (colonna `tool_input` JSONB).
    tool_input: Value,
    /// `created_at` in MICROSECONDI epoch (discriminante del turno reale).
    created_at_us: i64,
}

/// PUNTO UNICO della lettura degli step di replay da `agent_steps` (funzione
/// libera, testabile col solo `&PgPool`, regola L). Tutti gli step del run
/// primario che hanno prodotto un tool_use, ordinati in modo DETERMINISTICO.
///
/// I nodi ausiliari (planner/reflection/clarify) NON scrivono `agent_steps`:
/// quindi qui arrivano SOLO gli step dell'executor (1:1 con la sequenza di tool
/// che lo shadow deve rigiocare).
///
/// ORDINAMENTO (FIX shadow LLM-Replay, difesa in profondita'): `created_at ASC,
/// step_index ASC, id ASC`. Lo `step_index` del primario Python NON e' univoco
/// per run (retry/fallback della cascade riusano lo stesso indice), quindi un
/// ordinamento per solo `step_index` poteva MESCOLARE le ondate (gruppi-turno di
/// `group_steps_by_turn`) e produrre divergenza ("loop"). `created_at` da' l'ordine
/// temporale reale; `step_index` poi `id` (PK UUID) sono tiebreak deterministici
/// quando `created_at` collide. Il raggruppamento per TURNO REALE usa lo stesso
/// `created_at` (vedi `group_steps_by_turn`), NON il quoziente `step_index / 1000`
/// (inaffidabile su fonte sporca con indici riusati dalle ondate).
///
/// `created_at` e' letto come MICROSECONDI epoch (`EXTRACT(EPOCH ...) * 1e6`) cosi'
/// il raggruppamento per turno lavora su un intero deterministico e indipendente
/// dal fuso/tipo Rust del timestamp.
async fn load_replay_steps(
    db: &PgPool,
    primary_run_id: Uuid,
) -> Result<Vec<ReplayStep>, PortError> {
    let rows: Vec<(i64, String, Value, i64)> = sqlx::query_as(
        "SELECT step_index::bigint, tool_name, tool_input, \
                (EXTRACT(EPOCH FROM created_at) * 1000000)::bigint AS created_at_us \
         FROM agent_steps \
         WHERE run_id = $1 \
         ORDER BY created_at ASC, step_index ASC, id ASC",
    )
    .bind(primary_run_id)
    .fetch_all(db)
    .await
    .map_err(|e| PortError::Llm(format!("replay caricamento agent_steps: {e}")))?;

    Ok(rows
        .into_iter()
        .map(
            |(step_index, tool_name, tool_input, created_at_us)| ReplayStep {
                step_index,
                tool_name,
                tool_input,
                created_at_us,
            },
        )
        .collect())
}

/// PUNTO UNICO del fetch del messaggio finale del primario (`agent_runs.final_answer`),
/// usato dallo shadow per chiudere come il primario quando il cursore e' esausto.
/// `Ok(None)` se la colonna e' NULL (chiusura con content vuoto). Funzione libera,
/// testabile col solo `&PgPool` (regola L).
async fn load_primary_final_answer(
    db: &PgPool,
    primary_run_id: Uuid,
) -> Result<Option<String>, PortError> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT final_answer FROM agent_runs WHERE id = $1")
            .bind(primary_run_id)
            .fetch_optional(db)
            .await
            .map_err(|e| PortError::Llm(format!("replay lettura final_answer: {e}")))?;
    Ok(row.and_then(|(fa,)| fa))
}

/// Default della tolleranza di gap (microsecondi) usata da `group_steps_by_turn`
/// quando il setting DB `agent.shadow.replay_turn_gap_ms` non e' valorizzato.
/// 50ms (= 50_000 us): sta nel mezzo della separazione di oltre due ordini di
/// grandezza misurata sui dati reali (intra-turno <= ~2.6ms vs inter-turno >=
/// ~356ms). Regola G: la soglia operativa e' nel DB; questo e' solo il fallback
/// documentato della funzione PURA.
const DEFAULT_TURN_GAP_US: i64 = 50_000;

/// Raggruppa gli step (gia' ordinati per `created_at` ASC, `step_index` ASC, `id`
/// ASC da [`load_replay_steps`]) per TURNO REALE del primario, usando il GAP di
/// `created_at`: due step consecutivi nello stesso turno LLM hanno `created_at`
/// quasi-identico (scritti nello stesso batch dal brain), mentre turni/ondate
/// diversi sono distanti. Un gap maggiore di `gap_us` (microsecondi) apre un turno
/// nuovo; gap minore-uguale tiene lo step nel turno corrente.
///
/// Sostituisce il precedente raggruppamento per quoziente `step_index / 1000`, che
/// era INAFFIDABILE sulla fonte sporca: il brain su retry/fallback dello stesso run
/// RIUSA gli `step_index` (es. 3000-3003 in DUE ondate con `created_at` distanti),
/// quindi il quoziente ACCORPAVA ondate diverse in un solo mega-turno -> tool
/// esplorativi ripetuti in un turno -> `detect_signature_loop` spurio + stop_reason
/// "loop" + `num_tool_calls` troncato. Il GAP temporale ricostruisce i turni reali.
///
/// PURA (regola L): nessun I/O. `gap_us` e' iniettato dal chiamante
/// ([`ReplayLlmGateway::groups`], DB-driven con fallback [`DEFAULT_TURN_GAP_US`]).
/// Ogni gruppo mantiene gli step nell'ordine di ingresso (= ordine temporale).
fn group_steps_by_turn(steps: &[ReplayStep], gap_us: i64) -> Vec<Vec<ReplayStep>> {
    let mut groups: Vec<Vec<ReplayStep>> = Vec::new();
    let mut prev_ts: Option<i64> = None;
    for s in steps {
        let new_turn = match prev_ts {
            // Primo step, oppure salto temporale oltre la tolleranza -> turno nuovo.
            None => true,
            Some(prev) => s.created_at_us.saturating_sub(prev) > gap_us,
        };
        if new_turn {
            groups.push(Vec::new());
        }
        groups
            .last_mut()
            .expect("almeno un gruppo creato sopra")
            .push(s.clone());
        prev_ts = Some(s.created_at_us);
    }
    groups
}

/// Costruisce la [`LlmResponse`] dell'executor da rigiocare per UN gruppo-turno
/// del primario. PURA: emette un `tool_use` per ogni step del gruppo (id sintetico
/// `replay-{step_index}`, name = colonna `tool_name` LETTERALE, input = `tool_input`)
/// e ricostruisce `assistant_content` riusando la STESSA forma di [`map_gw_response`]
/// (blocchi `{type:"tool_use", id, name, input}`), per la continuita'
/// tool_use/tool_result attesa da `build_assistant_message` dell'executor (regola L:
/// non duplichiamo la logica di costruzione assistant_content, usiamo l'helper
/// condiviso [`assistant_content_for_tool_calls`]).
///
/// RISCHIO NOTO (design): per i tool wrappati `tool_name='nexus_mcp_tool_call'` il
/// `name` resta la COLONNA (non spacchettiamo `tool_input.tool_name`), cosi'
/// `num_tool_calls` combacia 1:1 col primario (che conta la stessa colonna). Se il
/// grafo Rust ramifica diversamente su `nexus_mcp_tool_call` e' una divergenza VERA
/// da far emergere, non da nascondere.
fn replay_response_for_group(group: &[ReplayStep], req: &LlmRequest) -> LlmResponse {
    let tool_calls: Vec<ToolUse> = group
        .iter()
        .map(|s| ToolUse {
            id: format!("replay-{}", s.step_index),
            name: s.tool_name.clone(),
            input: s.tool_input.clone(),
            thought_signature: None,
        })
        .collect();
    let assistant_content = assistant_content_for_tool_calls("", &tool_calls);
    LlmResponse {
        content: String::new(),
        tool_calls,
        usage: LlmUsage::default(),
        // provider/model EFFETTIVI = quelli del req (lo shadow non fa cascade).
        provider_used: if req.provider.is_empty() {
            None
        } else {
            Some(req.provider.clone())
        },
        model_used: if req.model.is_empty() {
            None
        } else {
            Some(req.model.clone())
        },
        assistant_content,
        stop_reason: Some("tool_use".to_string()),
        reasoning: None,
        thinking_signature: None,
    }
}

/// Helper condiviso (regola L): blocchi `assistant_content` in forma
/// `anthropic_content` da un testo opzionale + i `tool_use`. Stessa forma prodotta
/// da [`map_gw_response`] (blocco text se non vuoto, poi i blocchi tool_use; vuoto
/// se non c'e' alcun tool_use). Estratto per essere riusato dal replay senza
/// duplicare la costruzione.
fn assistant_content_for_tool_calls(text: &str, tool_calls: &[ToolUse]) -> Vec<Value> {
    if tool_calls.is_empty() {
        return Vec::new();
    }
    let mut blocks: Vec<Value> = Vec::new();
    if !text.is_empty() {
        blocks.push(json!({ "type": "text", "text": text }));
    }
    for tc in tool_calls {
        let mut block = json!({
            "type": "tool_use",
            "id": tc.id,
            "name": tc.name,
            "input": tc.input,
        });
        // Firma PER-CALL (Gemini 3): inclusa nel blocco tool_use SOLO se presente,
        // cosi' `ContentBlock::ToolUse` la conserva e la history la ri-passa.
        if let Some(sig) = &tc.thought_signature {
            block["thought_signature"] = json!(sig);
        }
        blocks.push(block);
    }
    blocks
}

/// Risposta NEUTRA deterministica per le chiamate AUSILIARIE in shadow (purpose
/// planner/reflection/clarify_expand o `None`): content vuoto, nessun tool_call,
/// `stop_reason=None`, usage zero, assistant_content vuoto. NESSUNA chiamata LLM
/// reale. Neutralizza il planner (pass-through, gia' default OFF), la reflection
/// (no valutazione LLM, resta il reward euristico deterministico) e clarify/
/// understanding (no-op: i nodi gestiscono gia' "nessun tool_use emesso" come skip).
fn neutral_auxiliary_response() -> LlmResponse {
    LlmResponse::default()
}

/// Gateway LLM di REPLAY per lo SHADOW (read-only, costo zero). NON chiama mai
/// l'LLM reale:
/// - per la chiamata dell'EXECUTOR (`purpose == "executor"`) consuma il prossimo
///   gruppo-turno del primario (lazy-load di `agent_steps`, raggruppati per turno
///   reale via gap di `created_at`) ed emette gli stessi tool_use, nello stesso
///   ordine temporale. Esaurito il cursore, chiude con `agent_runs.final_answer`
///   del primario (`stop_reason=end_turn`), cosi' lo shadow termina come il primario;
/// - per le chiamate AUSILIARIE (planner/reflection/clarify_expand o `None`)
///   ritorna [`neutral_auxiliary_response`] (deterministica, zero I/O).
///
/// Coesiste con [`GatewayLlmAdapter`] (Real): lo switch e' per ruolo nel punto
/// unico `native_engine::build_native_engine`. Il design NON include un fallback
/// `real`: in shadow puro gli ausiliari sono neutralizzati e l'executor e' replay,
/// quindi nessuna chiamata REAL e' mai necessaria.
pub struct ReplayLlmGateway {
    /// Pool Postgres: lettura `agent_steps` / `agent_runs` del primario.
    db: PgPool,
    /// Run primario di cui RIGIOCARE le decisioni dell'executor (= thread_id).
    primary_run_id: Uuid,
    /// Step del primario raggruppati per TURNO REALE (lazy-load alla prima chiamata
    /// executor). `OnceCell`: una sola lettura per run shadow.
    groups: OnceCell<Vec<Vec<ReplayStep>>>,
    /// Cursore sui gruppi-iterazione: indice del PROSSIMO gruppo da consumare.
    /// Ogni `complete()` dell'executor avanza di 1 (interior mutability su `&self`).
    cursor: Mutex<usize>,
    /// `agent_runs.final_answer` del primario (lazy-load): chiusura dello shadow a
    /// cursore esausto. `Some(None)` = caricato ma NULL (content vuoto).
    final_answer: OnceCell<Option<String>>,
}

impl ReplayLlmGateway {
    /// Costruisce il gateway di replay sul run primario dato.
    pub fn new(db: PgPool, primary_run_id: Uuid) -> Self {
        Self {
            db,
            primary_run_id,
            groups: OnceCell::new(),
            cursor: Mutex::new(0),
            final_answer: OnceCell::new(),
        }
    }

    /// Lazy-load (una sola volta) degli step del primario raggruppati per TURNO
    /// REALE. La tolleranza di gap e' DB-driven (regola G): setting
    /// `agent.shadow.replay_turn_gap_ms` (millisecondi), default
    /// [`DEFAULT_TURN_GAP_US`] / 1000 se assente/non parsabile.
    async fn groups(&self) -> Result<&Vec<Vec<ReplayStep>>, PortError> {
        self.groups
            .get_or_try_init(|| async {
                let steps = load_replay_steps(&self.db, self.primary_run_id).await?;
                let gap_us = self.turn_gap_us().await;
                Ok(group_steps_by_turn(&steps, gap_us))
            })
            .await
    }

    /// Tolleranza di gap per il raggruppamento per turno, in MICROSECONDI, letta dal
    /// setting `agent.shadow.replay_turn_gap_ms` (millisecondi nel DB). Fallback
    /// [`DEFAULT_TURN_GAP_US`] se il setting e' assente, vuoto o non parsabile (la
    /// lettura non deve mai far fallire lo shadow read-only). Regola G: nessun
    /// hardcode della soglia operativa nella logica; il default e' solo la rete di
    /// sicurezza documentata.
    async fn turn_gap_us(&self) -> i64 {
        crate::settings::get_setting(&self.db, "agent.shadow.replay_turn_gap_ms")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .filter(|ms| *ms >= 0)
            .map(|ms| ms.saturating_mul(1000))
            .unwrap_or(DEFAULT_TURN_GAP_US)
    }

    /// Lazy-load (una sola volta) del `final_answer` del primario.
    async fn final_answer(&self) -> Result<&Option<String>, PortError> {
        self.final_answer
            .get_or_try_init(|| load_primary_final_answer(&self.db, self.primary_run_id))
            .await
    }

    /// Replay della chiamata dell'executor: consuma il prossimo gruppo-turno;
    /// se esausto, chiude con `final_answer` del primario (`stop_reason=end_turn`).
    async fn complete_executor(&self, req: LlmRequest) -> Result<LlmResponse, PortError> {
        let groups = self.groups().await?;
        let idx = {
            let mut cur = self.cursor.lock().await;
            let i = *cur;
            *cur += 1;
            i
        };
        match groups.get(idx) {
            Some(group) => Ok(replay_response_for_group(group, &req)),
            None => {
                // Cursore esausto: lo shadow chiude come il primario.
                let final_answer = self.final_answer().await?.clone().unwrap_or_default();
                Ok(LlmResponse {
                    content: final_answer,
                    stop_reason: Some("end_turn".to_string()),
                    ..Default::default()
                })
            }
        }
    }
}

#[async_trait]
impl LlmGateway for ReplayLlmGateway {
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, PortError> {
        match req.purpose.as_deref() {
            Some("executor") => self.complete_executor(req).await,
            // Ausiliari (planner/reflection/clarify_expand) o purpose assente:
            // risposta neutra deterministica, nessuna chiamata LLM reale.
            _ => Ok(neutral_auxiliary_response()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nexus_gateway::{GwToolCall, GwToolFunctionCall, GwUsage};
    use nexus_agent_graph::runtime::ports::{LlmMessage, ThinkingConfig};

    fn base_req() -> LlmRequest {
        LlmRequest {
            provider: "anthropic".to_string(),
            model: "claude-x".to_string(),
            messages: vec![LlmMessage {
                role: "user".to_string(),
                content: json!("ciao"),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    // ── purpose_for_empty_model: guard sul modello vuoto (regola G) ───────────

    #[test]
    fn modello_valorizzato_non_tocca_il_purpose() {
        // Path executor: provider/model gia' risolti a monte -> nessuna risoluzione.
        let req = base_req();
        assert!(matches!(purpose_for_empty_model(&req), Ok(None)));
    }

    #[test]
    fn modello_vuoto_con_purpose_richiede_risoluzione() {
        // Path ausiliario (clarify_expand): model vuoto + purpose -> da risolvere.
        let req = LlmRequest {
            provider: String::new(),
            model: String::new(),
            purpose: Some("clarify_expand".to_string()),
            ..base_req()
        };
        assert_eq!(
            purpose_for_empty_model(&req).unwrap(),
            Some("clarify_expand".to_string())
        );
    }

    #[test]
    fn modello_vuoto_senza_purpose_e_errore_chiaro() {
        // MAI inoltrare un modello vuoto al gateway: prima del guard la cascata
        // falliva su tutti i provider con 400 fuorvianti (incidente 2026-07-02).
        let req = LlmRequest {
            provider: String::new(),
            model: "  ".to_string(),
            purpose: None,
            ..base_req()
        };
        let err = purpose_for_empty_model(&req).unwrap_err();
        assert!(matches!(err, PortError::Llm(_)));
        assert!(err.to_string().contains("senza modello ne' purpose"));
    }

    #[test]
    fn purpose_solo_spazi_equivale_ad_assente() {
        let req = LlmRequest {
            model: String::new(),
            purpose: Some("   ".to_string()),
            ..base_req()
        };
        assert!(purpose_for_empty_model(&req).is_err());
    }

    // ── classify_gateway_error: segnale strutturato cooldown/provider ─────────

    use crate::nexus_gateway::GatewayHttpError;
    use nexus_agent_graph::runtime::ports::ProviderFailureCause;

    /// Costruisce l'errore tipizzato come farebbe `NexusGatewayClient::complete`
    /// su un HTTP non-2xx (stesso punto di costruzione, regola M).
    fn gw_err(status: u16, body: &str) -> anyhow::Error {
        GatewayHttpError::from_response(
            reqwest::StatusCode::from_u16(status).expect("status valido"),
            body.to_string(),
        )
        .into()
    }

    #[test]
    fn classify_provider_error_code_diventa_provider_unavailable() {
        // Il 500 aggregato del gateway porta code=PROVIDER_ERROR: e' il segnale
        // strutturato che l'executor matcha per il fallback cross-provider. La
        // causa arriva da details.primary_cause, MAI dal testo del messaggio.
        let err = gw_err(
            500,
            "{\"error\":\"tutti i provider hanno fallito -> anthropic (in cooldown, 42s \
rimanenti)\",\"code\":\"PROVIDER_ERROR\",\"details\":{\"primary_cause\":\"cooldown\"}}",
        );
        match classify_gateway_error(&err) {
            PortError::ProviderUnavailable(info) => {
                assert_eq!(info.cause, ProviderFailureCause::Cooldown);
            }
            other => panic!("atteso ProviderUnavailable, avuto {other:?}"),
        }
    }

    #[test]
    fn classify_client_error_riporta_la_causa_onesta() {
        // Il 4xx del provider (es. deepseek 400, incidente run 48793fde) NON e'
        // un cooldown: la causa strutturata client_error arriva alla porta cosi'
        // il meta-step non mente all'utente.
        let err = gw_err(
            500,
            "{\"error\":\"tutti i provider hanno fallito -> deepseek (deepseek HTTP 400: \
invalid request)\",\"code\":\"PROVIDER_ERROR\",\"details\":{\"primary_cause\":\"client_error\",\
\"failures\":[{\"provider\":\"deepseek\",\"class\":\"client_error\",\"status\":400}]}}",
        );
        match classify_gateway_error(&err) {
            PortError::ProviderUnavailable(info) => {
                assert_eq!(info.cause, ProviderFailureCause::ClientError);
                assert!(!info.cause.is_cooldown_like());
            }
            other => panic!("atteso ProviderUnavailable, avuto {other:?}"),
        }
    }

    #[test]
    fn classify_policy_tier_excluded_e_failover_con_causa_policy() {
        // Esclusione di policy per sensitivity tier (403 POLICY_TIER_EXCLUDED):
        // failover con racconto onesto "contenuto riservato", non "instabilita'".
        let err = gw_err(
            403,
            "{\"error\":\"provider deepseek escluso dalla policy per sensitivity tier 3\",\
\"code\":\"POLICY_TIER_EXCLUDED\",\"details\":{\"provider\":\"deepseek\",\"detected_tier\":3,\
\"allowed_providers\":[]}}",
        );
        match classify_gateway_error(&err) {
            PortError::ProviderUnavailable(info) => {
                assert_eq!(info.cause, ProviderFailureCause::PolicyTierExcluded);
            }
            other => panic!("atteso ProviderUnavailable, avuto {other:?}"),
        }
    }

    #[test]
    fn classify_gateway_vecchio_senza_details_degrada_a_unknown() {
        // Retro-compatibilita': un gateway non aggiornato manda PROVIDER_ERROR
        // senza details -> failover come prima, causa Unknown (mai inventata).
        let err = gw_err(
            500,
            "{\"error\":\"tutti i provider hanno fallito -> mistral (cooldown billing, 600s \
rimanenti)\",\"code\":\"PROVIDER_ERROR\"}",
        );
        match classify_gateway_error(&err) {
            PortError::ProviderUnavailable(info) => {
                assert_eq!(info.cause, ProviderFailureCause::Unknown);
            }
            other => panic!("atteso ProviderUnavailable, avuto {other:?}"),
        }
    }

    #[test]
    fn classify_altri_errori_restano_llm_generico() {
        // Un 400 di richiesta (BAD_REQUEST) o un errore HTTP non e' un cooldown:
        // resta Llm generico -> l'executor chiude come prima (StopReason::Error).
        let bad = gw_err(
            400,
            "{\"error\":\"model non valido\",\"code\":\"BAD_REQUEST\"}",
        );
        assert!(matches!(classify_gateway_error(&bad), PortError::Llm(_)));
        // Errore di trasporto senza GatewayHttpError nella catena.
        let http = anyhow::anyhow!("Nexus Gateway HTTP error: connection refused");
        assert!(matches!(classify_gateway_error(&http), PortError::Llm(_)));
    }

    // ── force_tool_choice end-to-end (BEFORE/AFTER) ───────────────────────────

    #[test]
    fn force_tool_choice_some_true_arriva_required() {
        // AFTER: force_tool_choice=Some(true) -> tool_choice "required" nel GwRequest.
        let mut req = base_req();
        req.force_tool_choice = Some(true);
        req.tools = Some(vec![
            json!({"name": "edit_file", "input_schema": {"type": "object"}}),
        ]);

        let gw = build_gw_request(&req);
        // Il vincolo DEVE essere presente e valere "required" (non droppato).
        assert_eq!(gw.tool_choice, Some(json!("required")));
        // E deve serializzare nel JSON inviato al gateway (prova che ARRIVA al server).
        let body = serde_json::to_value(&gw).unwrap();
        assert_eq!(body["tool_choice"], json!("required"));
    }

    #[test]
    fn force_tool_choice_none_omette_tool_choice() {
        // BEFORE (comportamento auto): force_tool_choice=None -> nessun tool_choice.
        let req = base_req(); // force_tool_choice = None (Default)
        let gw = build_gw_request(&req);
        assert_eq!(gw.tool_choice, None);
        // Omesso nel JSON (skip_serializing_if): il gateway tratta come `auto`.
        let body = serde_json::to_value(&gw).unwrap();
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn force_tool_choice_some_false_diventa_none_testuale() {
        let mut req = base_req();
        req.force_tool_choice = Some(false);
        let gw = build_gw_request(&req);
        assert_eq!(gw.tool_choice, Some(json!("none")));
    }

    #[test]
    fn response_format_e_thinking_attraversano_adapter() {
        // AFTER: i parametri avanzati del contratto gateway non vengono piu'
        // droppati dal client mcp-core prima di arrivare al provider.
        let mut req = base_req();
        req.response_format = Some(json!({"type": "json_object"}));
        req.thinking = Some(ThinkingConfig {
            enabled: true,
            budget_tokens: Some(512),
        });

        let gw = build_gw_request(&req);
        let body = serde_json::to_value(&gw).unwrap();
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["thinking"]["enabled"], true);
        assert_eq!(body["thinking"]["budget_tokens"], 512);
    }

    // ── provider/model pin + system ───────────────────────────────────────────

    #[test]
    fn provider_va_in_pin_e_model_prefissato() {
        let req = base_req();
        let gw = build_gw_request(&req);
        // provider risolto -> pin_provider esplicito (no re-routing nel gateway).
        assert_eq!(gw.pin_provider.as_deref(), Some("anthropic"));
        // model prefissato provider/model (il pin server strippa il prefisso).
        assert_eq!(gw.model, "anthropic/claude-x");
    }

    #[test]
    fn system_text_diventa_primo_messaggio_system() {
        let mut req = base_req();
        req.system_text = Some("sei un assistente".to_string());
        let gw = build_gw_request(&req);
        assert_eq!(gw.messages[0].role, "system");
        assert_eq!(gw.messages[0].content, json!("sei un assistente"));
        assert_eq!(gw.messages[1].role, "user");
    }

    #[test]
    fn system_text_non_duplicato_se_gia_presente() {
        let mut req = base_req();
        req.system_text = Some("X".to_string());
        req.messages.insert(
            0,
            LlmMessage {
                role: "system".to_string(),
                content: json!("system esistente"),
                ..Default::default()
            },
        );
        let gw = build_gw_request(&req);
        // Un solo messaggio system (quello gia' presente), il system_text non lo duplica.
        let n_system = gw.messages.iter().filter(|m| m.role == "system").count();
        assert_eq!(n_system, 1);
        assert_eq!(gw.messages[0].content, json!("system esistente"));
    }

    // ── tools: conversione Anthropic-style -> OpenAI ──────────────────────────

    #[test]
    fn tools_anthropic_convertiti_in_openai() {
        let req = {
            let mut r = base_req();
            r.tools = Some(vec![json!({
                "name": "read_file",
                "description": "legge un file",
                "input_schema": {"type": "object", "properties": {}}
            })]);
            r
        };
        let gw = build_gw_request(&req);
        let tools = gw.tools.unwrap();
        let t = &tools[0];
        assert_eq!(t["type"], "function");
        assert_eq!(t["function"]["name"], "read_file");
        assert_eq!(t["function"]["description"], "legge un file");
        // input_schema -> function.parameters
        assert_eq!(t["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn tools_gia_openai_passthrough() {
        let req = {
            let mut r = base_req();
            r.tools = Some(vec![json!({
                "type": "function",
                "function": {"name": "x", "parameters": {"type": "object"}}
            })]);
            r
        };
        let gw = build_gw_request(&req);
        let tools = gw.tools.unwrap();
        assert_eq!(tools[0]["function"]["name"], "x");
    }

    // ── tool_calls round-trip (GwResponse -> LlmResponse) ─────────────────────

    fn gw_resp_with_tool_call() -> GwResponse {
        GwResponse {
            content: "procedo".to_string(),
            tool_calls: Some(vec![GwToolCall {
                id: "call_1".to_string(),
                kind: "function".to_string(),
                function: GwToolFunctionCall {
                    name: "edit_file".to_string(),
                    arguments: r#"{"path":"a.rs","content":"x"}"#.to_string(),
                },
                thought_signature: None,
            }]),
            usage: GwUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: Some(3),
                cache_creation_tokens: Some(7),
            },
            model_used: "claude-real".to_string(),
            provider_used: "anthropic".to_string(),
            latency_ms: 42,
            finish_reason: "tool_calls".to_string(),
            privacy_rerouted: None,
            reasoning: None,
            thinking_signature: None,
            citations: None,
        }
    }

    #[test]
    fn tool_calls_round_trip_popolato_e_assistant_content() {
        let out = map_gw_response(gw_resp_with_tool_call());
        // tool_calls popolato.
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].id, "call_1");
        assert_eq!(out.tool_calls[0].name, "edit_file");
        // arguments (stringa JSON) parsato in input.
        assert_eq!(out.tool_calls[0].input["path"], "a.rs");

        // assistant_content: blocco text + blocco tool_use (per il Message::Ai).
        assert_eq!(out.assistant_content.len(), 2);
        assert_eq!(out.assistant_content[0]["type"], "text");
        assert_eq!(out.assistant_content[0]["text"], "procedo");
        assert_eq!(out.assistant_content[1]["type"], "tool_use");
        assert_eq!(out.assistant_content[1]["id"], "call_1");
        assert_eq!(out.assistant_content[1]["name"], "edit_file");
        assert_eq!(out.assistant_content[1]["input"]["content"], "x");

        // I blocchi assistant_content sono deserializzabili in ContentBlock
        // (forma autoritativa attesa da build_assistant_message dell'executor).
        for b in &out.assistant_content {
            serde_json::from_value::<nexus_agent_graph::state::ContentBlock>(b.clone())
                .expect("assistant_content deserializzabile in ContentBlock");
        }

        // provider/model EFFETTIVI + stop_reason NORMALIZZATO al vocabolario della
        // porta: il gateway riporta finish_reason="tool_calls" (OpenAI-canonico),
        // qui deve diventare "tool_use" (Anthropic-style atteso dall'executor) cosi'
        // il `route_after_executor` instrada al tool_dispatch (FIX hollow primario).
        assert_eq!(out.provider_used.as_deref(), Some("anthropic"));
        assert_eq!(out.model_used.as_deref(), Some("claude-real"));
        assert_eq!(out.stop_reason.as_deref(), Some("tool_use"));
    }

    // ── normalizzazione finish_reason wire->porta (FIX hollow primario) ────────

    #[test]
    fn normalize_finish_reason_tool_calls_diventa_tool_use() {
        // Il caso che causava l'hollow: tool_calls (gateway) -> tool_use (porta).
        assert_eq!(normalize_gw_finish_reason("tool_calls"), "tool_use");
    }

    #[test]
    fn normalize_finish_reason_stop_diventa_end_turn() {
        assert_eq!(normalize_gw_finish_reason("stop"), "end_turn");
    }

    #[test]
    fn normalize_finish_reason_length_diventa_max_tokens() {
        assert_eq!(normalize_gw_finish_reason("length"), "max_tokens");
    }

    #[test]
    fn normalize_finish_reason_ignoto_passthrough() {
        // Robustezza: una stringa fuori contratto non viene inventata, passa cosi'
        // com'e' (cadra' poi sul default end_turn del punto unico executor).
        assert_eq!(normalize_gw_finish_reason("error"), "error");
        assert_eq!(normalize_gw_finish_reason("boh"), "boh");
    }

    #[test]
    fn map_gw_response_turno_tool_instrada_a_tool_use() {
        // E2E del mapping: una GwResponse con finish_reason="tool_calls" e 1 tool_call
        // -> LlmResponse con stop_reason "tool_use" + tool_calls popolato (il turno
        // verra' instradato al dispatch, non chiuso a end_turn). Riproduce il run reale.
        let out = map_gw_response(gw_resp_with_tool_call());
        assert_eq!(out.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(out.tool_calls.len(), 1);
    }

    #[test]
    fn usage_cache_mappato() {
        let out = map_gw_response(gw_resp_with_tool_call());
        assert_eq!(out.usage.prompt_tokens, 10);
        assert_eq!(out.usage.completion_tokens, 5);
        assert_eq!(out.usage.total_tokens, 15);
        assert_eq!(out.usage.cache_read_tokens, Some(3));
        assert_eq!(out.usage.cache_creation_tokens, Some(7));
        // Il /v1/complete non riporta il costo: None (niente magic).
        assert_eq!(out.usage.total_cost_usd, None);
    }

    #[test]
    fn turno_testuale_senza_tool_calls_assistant_content_vuoto() {
        let mut resp = gw_resp_with_tool_call();
        resp.tool_calls = None;
        resp.content = "solo testo".to_string();
        let out = map_gw_response(resp);
        assert!(out.tool_calls.is_empty());
        // Nessun tool_use da preservare -> assistant_content vuoto (l'executor
        // ricostruisce dal solo content).
        assert!(out.assistant_content.is_empty());
        assert_eq!(out.content, "solo testo");
    }

    // ── continuita' tool multi-turn: LlmMessage -> GwMessage (bug 2026-06-26) ──

    #[test]
    fn multi_turn_assistant_tool_calls_e_tool_role_id_preservati() {
        use nexus_agent_graph::state::ToolUse;

        // Sequenza multi-step: [Human, Ai-con-tool_use(id=X), Tool(tool_call_id=X)].
        let req = LlmRequest {
            provider: "anthropic".to_string(),
            model: "claude-x".to_string(),
            messages: vec![
                LlmMessage {
                    role: "user".to_string(),
                    content: json!("leggi a.rs"),
                    ..Default::default()
                },
                LlmMessage {
                    role: "assistant".to_string(),
                    content: json!("procedo a leggere"),
                    tool_calls: Some(vec![ToolUse {
                        id: "call_X".to_string(),
                        name: "read_file".to_string(),
                        input: json!({"path": "a.rs"}),
                        thought_signature: None,
                    }]),
                    tool_call_id: None,
                    reasoning: None,
                    thinking_signature: None,
                },
                LlmMessage {
                    role: "tool".to_string(),
                    content: json!("contenuto di a.rs"),
                    tool_calls: None,
                    tool_call_id: Some("call_X".to_string()),
                    reasoning: None,
                    thinking_signature: None,
                },
            ],
            ..Default::default()
        };

        let gw = build_gw_request(&req);
        // messages[0] = user (nessun tool).
        assert_eq!(gw.messages[0].role, "user");
        assert!(gw.messages[0].tool_calls.is_none());

        // messages[1] = assistant con tool_calls NON vuoto contenente id=call_X.
        // Il tool_use NON deve essere appiattito in content (resta il testo).
        let ai = &gw.messages[1];
        assert_eq!(ai.role, "assistant");
        let calls = ai
            .tool_calls
            .as_ref()
            .expect("assistant deve avere tool_calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_X");
        assert_eq!(calls[0].kind, "function");
        assert_eq!(calls[0].function.name, "read_file");
        // arguments e' una STRINGA JSON (contratto OpenAI).
        let args: Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "a.rs");
        // Il content NON contiene il tool_use appiattito.
        assert_eq!(ai.content, json!("procedo a leggere"));

        // messages[2] = role "tool" con tool_call_id = call_X (round-trip).
        let tool = &gw.messages[2];
        assert_eq!(tool.role, "tool");
        assert_eq!(tool.tool_call_id.as_deref(), Some("call_X"));
        assert_eq!(tool.content, json!("contenuto di a.rs"));

        // Coerenza id: tool_use dell'assistant == tool_call_id del messaggio tool.
        assert_eq!(calls[0].id, tool.tool_call_id.clone().unwrap());

        // Serializzazione wire: il JSON inviato al gateway porta i campi (prova che
        // ARRIVANO al server `to_anthropic_messages`, che riconosce la coppia).
        let body = serde_json::to_value(&gw).unwrap();
        let m1 = &body["messages"][1];
        assert_eq!(m1["role"], "assistant");
        assert_eq!(m1["tool_calls"][0]["id"], "call_X");
        assert_eq!(m1["tool_calls"][0]["type"], "function");
        // L'assistant NON serializza tool_call_id (None -> omesso).
        assert!(m1.get("tool_call_id").is_none());
        let m2 = &body["messages"][2];
        assert_eq!(m2["role"], "tool");
        assert_eq!(m2["tool_call_id"], "call_X");
        // Il messaggio tool NON serializza tool_calls (None -> omesso).
        assert!(m2.get("tool_calls").is_none());
    }

    // ── ReplayLlmGateway: raggruppamento + risposta replay (PURI) ─────────────

    /// Step di test con `created_at` esplicito in MICROSECONDI epoch: il
    /// raggruppamento per turno lavora sul GAP di `created_at`, quindi i test
    /// devono controllare il timestamp (non piu' il quoziente `step_index/1000`).
    fn step_at(step_index: i64, tool_name: &str, created_at_us: i64) -> ReplayStep {
        ReplayStep {
            step_index,
            tool_name: tool_name.to_string(),
            tool_input: json!({"k": step_index}),
            created_at_us,
        }
    }

    #[test]
    fn group_steps_turno_multi_tool_resta_unito() {
        // Un turno LLM multi-tool: stesso created_at (batch INSERT singolo) -> 1
        // gruppo con tutti i tool, ordine preservato. Default 50ms di gap.
        let t = 1_000_000_000;
        let steps = vec![
            step_at(0, "read_file", t),
            step_at(1, "list_files", t),
            step_at(2, "grep", t),
        ];
        let groups = group_steps_by_turn(&steps, DEFAULT_TURN_GAP_US);
        assert_eq!(groups.len(), 1, "stesso created_at -> un solo turno");
        assert_eq!(groups[0].len(), 3);
        assert_eq!(groups[0][0].tool_name, "read_file");
        assert_eq!(groups[0][1].tool_name, "list_files");
        assert_eq!(groups[0][2].tool_name, "grep");
    }

    #[test]
    fn group_steps_due_ondate_stessi_step_index_turni_separati() {
        // FIX shadow: due ondate (retry/fallback) RIUSANO gli step_index (3000-3003)
        // ma con created_at distante. Il quoziente /1000 le accorpava in un
        // mega-turno (loop spurio); il gap temporale le tiene SEPARATE.
        // Ondata 1: step 3000-3003 a t. Ondata 2 (stessi indici): a t + 40s.
        let t = 1_000_000_000;
        let ond2 = t + 40_000_000; // +40s, ben oltre i 50ms di tolleranza
        let steps = vec![
            step_at(3000, "read_file", t),
            step_at(3001, "read_file", t),
            step_at(3002, "read_file", t),
            step_at(3003, "read_file", t),
            step_at(3000, "list_files", ond2),
            step_at(3001, "list_files", ond2),
            step_at(3002, "list_files", ond2),
            step_at(3003, "read_file", ond2),
        ];
        let groups = group_steps_by_turn(&steps, DEFAULT_TURN_GAP_US);
        assert_eq!(
            groups.len(),
            2,
            "due ondate -> due turni, NON un mega-turno"
        );
        assert_eq!(groups[0].len(), 4);
        assert!(groups[0].iter().all(|s| s.tool_name == "read_file"));
        assert_eq!(groups[1].len(), 4);
        // Il secondo turno e' l'ondata 2 (list_files...), separata nel tempo.
        assert_eq!(groups[1][0].tool_name, "list_files");
    }

    #[test]
    fn group_steps_micro_jitter_intra_turno_resta_unito() {
        // Caso reale 4531a1c7: step 1-4 stesso burst con jitter ~1.5-2ms tra loro
        // (INSERT separati) -> DEVONO restare nello stesso turno (gap < 50ms), non
        // spezzarsi in 4 turni-da-1-tool.
        let base = 1_000_000_000;
        let steps = vec![
            step_at(1, "read_file", base),
            step_at(2, "list_files", base + 1_974), // +1.974ms
            step_at(3, "read_file", base + 3_479),  // +1.505ms
            step_at(4, "list_files", base + 5_111), // +1.632ms
        ];
        let groups = group_steps_by_turn(&steps, DEFAULT_TURN_GAP_US);
        assert_eq!(groups.len(), 1, "micro-jitter intra-turno -> un solo turno");
        assert_eq!(groups[0].len(), 4);
    }

    #[test]
    fn group_steps_gap_sopra_soglia_spezza_turno() {
        // Caso reale 6cfd2e34: gap minimo inter-turno osservato ~356ms (>50ms) deve
        // spezzare; gap intra-turno ~2ms (<50ms) deve unire.
        let base = 1_000_000_000;
        let steps = vec![
            step_at(3000, "list_files", base),
            step_at(3001, "list_files", base), // batch identico -> stesso turno
            // +356ms: nuovo turno (step 1-5, ondata diversa)
            step_at(1, "nexus_run_notes", base + 356_000),
            step_at(2, "list_files", base + 358_000), // +2ms dal precedente -> stesso turno
        ];
        let groups = group_steps_by_turn(&steps, DEFAULT_TURN_GAP_US);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 2, "primo turno: i due list_files identici");
        assert_eq!(groups[1].len(), 2, "secondo turno: run_notes + list_files");
        assert_eq!(groups[1][0].tool_name, "nexus_run_notes");
    }

    #[test]
    fn group_steps_vuoto_nessun_gruppo() {
        let groups = group_steps_by_turn(&[], DEFAULT_TURN_GAP_US);
        assert!(groups.is_empty());
    }

    #[test]
    fn replay_response_emette_tool_use_letterali() {
        // Tool wrappato: name = colonna tool_name LETTERALE (nexus_mcp_tool_call),
        // NON spacchettato da tool_input.tool_name. id sintetico replay-{step_index}.
        let group = vec![ReplayStep {
            step_index: 1000,
            tool_name: "nexus_mcp_tool_call".to_string(),
            tool_input: json!({"tool_name": "build", "args": {}}),
            created_at_us: 1_000_000_000,
        }];
        let req = LlmRequest {
            provider: "anthropic".to_string(),
            model: "claude-x".to_string(),
            ..Default::default()
        };
        let resp = replay_response_for_group(&group, &req);
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "nexus_mcp_tool_call");
        assert_eq!(resp.tool_calls[0].id, "replay-1000");
        // input = tool_input INTEGRO (col tool vero dentro, non spacchettato).
        assert_eq!(resp.tool_calls[0].input["tool_name"], "build");
        assert_eq!(resp.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(resp.content, "");
        // provider/model effettivi = quelli del req (nessun cascade in shadow).
        assert_eq!(resp.provider_used.as_deref(), Some("anthropic"));
        assert_eq!(resp.model_used.as_deref(), Some("claude-x"));
        // assistant_content deserializzabile in ContentBlock (continuita' tool_use).
        for b in &resp.assistant_content {
            serde_json::from_value::<nexus_agent_graph::state::ContentBlock>(b.clone())
                .expect("assistant_content deserializzabile");
        }
    }

    #[test]
    fn risposta_ausiliaria_neutra_deterministica() {
        // (c) purpose ausiliario -> LlmResponse neutra, nessun I/O.
        let neutra = neutral_auxiliary_response();
        assert_eq!(neutra.content, "");
        assert!(neutra.tool_calls.is_empty());
        assert_eq!(neutra.stop_reason, None);
        assert!(neutra.assistant_content.is_empty());
        assert_eq!(neutra.usage, LlmUsage::default());
    }

    // ── ReplayLlmGateway: end-to-end via complete() (sqlx) ─────────────────────

    async fn create_tables(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE agent_runs ( \
                 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                 final_answer TEXT \
             )",
        )
        .execute(pool)
        .await
        .expect("create agent_runs");
        sqlx::query(
            "CREATE TABLE agent_steps ( \
                 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                 run_id UUID NOT NULL, \
                 step_index INT NOT NULL, \
                 tool_name TEXT NOT NULL, \
                 tool_input JSONB NOT NULL, \
                 tool_result TEXT, \
                 status TEXT NOT NULL DEFAULT 'running', \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW() \
             )",
        )
        .execute(pool)
        .await
        .expect("create agent_steps");
    }

    async fn insert_run(pool: &PgPool, final_answer: Option<&str>) -> Uuid {
        let run = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_runs (id, final_answer) VALUES ($1, $2)")
            .bind(run)
            .bind(final_answer)
            .execute(pool)
            .await
            .expect("run");
        run
    }

    fn executor_req() -> LlmRequest {
        LlmRequest {
            provider: "anthropic".to_string(),
            model: "claude-x".to_string(),
            purpose: Some("executor".to_string()),
            ..Default::default()
        }
    }

    /// (a) sequenza multi-turno: 2 turni (turno 1 con 2 tool, turno 2 con 1 tool,
    /// separati nel tempo) -> 2 complete() coi tool giusti, poi la 3a complete() =
    /// end_turn + final_answer del primario. (b) conteggio tool per turno = primario.
    /// I turni sono discriminati dal GAP di `created_at` (turno 1 stesso timestamp,
    /// turno 2 a +2s).
    #[sqlx::test]
    async fn replay_executor_sequenza_multi_turno(pool: PgPool) {
        create_tables(&pool).await;
        let run = insert_run(&pool, Some("FATTO")).await;
        // Turno 1: read_file (0) + list_files (1) stesso created_at (batch).
        insert_step_at(&pool, run, 0, "read_file", "2026-06-27T10:00:00Z").await;
        insert_step_at(&pool, run, 1, "list_files", "2026-06-27T10:00:00Z").await;
        // Turno 2: edit_file (2000) a +2s (oltre la tolleranza 50ms).
        insert_step_at(&pool, run, 2000, "edit_file", "2026-06-27T10:00:02Z").await;

        let gw = ReplayLlmGateway::new(pool.clone(), run);

        // 1a complete() (executor): turno 1 -> 2 tool nello stesso ordine.
        let r1 = gw.complete(executor_req()).await.expect("turno 1");
        assert_eq!(r1.tool_calls.len(), 2, "conteggio tool turno 1 = primario");
        assert_eq!(r1.tool_calls[0].name, "read_file");
        assert_eq!(r1.tool_calls[1].name, "list_files");
        assert_eq!(r1.stop_reason.as_deref(), Some("tool_use"));

        // 2a complete(): turno 2 -> 1 tool.
        let r2 = gw.complete(executor_req()).await.expect("turno 2");
        assert_eq!(r2.tool_calls.len(), 1, "conteggio tool turno 2 = primario");
        assert_eq!(r2.tool_calls[0].name, "edit_file");

        // (d) 3a complete(): cursore esausto -> end_turn + final_answer del primario.
        let r3 = gw.complete(executor_req()).await.expect("turno 3");
        assert!(r3.tool_calls.is_empty());
        assert_eq!(r3.content, "FATTO");
        assert_eq!(r3.stop_reason.as_deref(), Some("end_turn"));
    }

    /// FIX shadow LLM-Replay E2E (regressione del MEGA-TURNO): due ondate
    /// retry/fallback RIUSANO gli step_index (3000-3003) con `created_at` distanti.
    /// Il vecchio raggruppamento per quoziente /1000 le ACCORPAVA in un solo turno da
    /// 8 tool -> signature-loop spurio. Col raggruppamento per turno reale lo shadow
    /// vede DUE turni separati (4 + 4 tool), come il primario. Riproduce 6cfd2e34.
    #[sqlx::test]
    async fn replay_executor_due_ondate_non_collassano_in_mega_turno(pool: PgPool) {
        create_tables(&pool).await;
        let run = insert_run(&pool, Some("done")).await;
        // Ondata 1: step 3000-3003 stesso created_at.
        insert_step_at(&pool, run, 3000, "read_file", "2026-06-27T10:50:53Z").await;
        insert_step_at(&pool, run, 3001, "read_file", "2026-06-27T10:50:53Z").await;
        insert_step_at(&pool, run, 3002, "read_file", "2026-06-27T10:50:53Z").await;
        insert_step_at(&pool, run, 3003, "read_file", "2026-06-27T10:50:53Z").await;
        // Ondata 2: STESSI step_index, +41s (gap enorme -> turno separato).
        insert_step_at(&pool, run, 3000, "list_files", "2026-06-27T10:51:34Z").await;
        insert_step_at(&pool, run, 3001, "list_files", "2026-06-27T10:51:34Z").await;
        insert_step_at(&pool, run, 3002, "list_files", "2026-06-27T10:51:34Z").await;
        insert_step_at(&pool, run, 3003, "read_file", "2026-06-27T10:51:34Z").await;

        let gw = ReplayLlmGateway::new(pool.clone(), run);

        // Turno 1: 4 tool (ondata 1), NON un mega-turno da 8.
        let r1 = gw.complete(executor_req()).await.expect("turno 1");
        assert_eq!(r1.tool_calls.len(), 4, "ondata 1 = un turno da 4 tool");
        assert!(r1.tool_calls.iter().all(|t| t.name == "read_file"));

        // Turno 2: gli altri 4 tool (ondata 2), turno distinto.
        let r2 = gw.complete(executor_req()).await.expect("turno 2");
        assert_eq!(r2.tool_calls.len(), 4, "ondata 2 = secondo turno da 4 tool");
        assert_eq!(r2.tool_calls[0].name, "list_files");

        // 3a: cursore esausto -> end_turn.
        let r3 = gw.complete(executor_req()).await.expect("end_turn");
        assert!(r3.tool_calls.is_empty());
        assert_eq!(r3.stop_reason.as_deref(), Some("end_turn"));
    }

    /// (c) purpose ausiliario via complete() -> risposta neutra SENZA leggere il DB
    /// (run_id inesistente: se toccasse il DB fallirebbe, ma e' neutralizzato prima).
    #[sqlx::test]
    async fn replay_purpose_ausiliario_neutro_senza_io(pool: PgPool) {
        create_tables(&pool).await;
        // Nessun run inserito: un purpose ausiliario NON deve leggere agent_steps.
        let gw = ReplayLlmGateway::new(pool.clone(), Uuid::new_v4());
        for purpose in ["planner", "reflection", "clarify_expand"] {
            let req = LlmRequest {
                purpose: Some(purpose.to_string()),
                ..Default::default()
            };
            let resp = gw.complete(req).await.expect("ausiliario neutro");
            assert!(resp.tool_calls.is_empty());
            assert_eq!(resp.content, "");
            assert_eq!(resp.stop_reason, None);
        }
        // purpose None -> anch'esso neutro.
        let resp = gw
            .complete(LlmRequest::default())
            .await
            .expect("none neutro");
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.stop_reason, None);
    }

    /// (d) cursore esausto subito (nessuno step) -> la PRIMA complete() executor
    /// chiude con end_turn (final_answer NULL -> content vuoto).
    #[sqlx::test]
    async fn replay_executor_cursore_esausto_subito(pool: PgPool) {
        create_tables(&pool).await;
        let run = insert_run(&pool, None).await; // final_answer NULL
        let gw = ReplayLlmGateway::new(pool.clone(), run);

        let r = gw
            .complete(executor_req())
            .await
            .expect("end_turn immediato");
        assert!(r.tool_calls.is_empty());
        assert_eq!(r.content, "", "final_answer NULL -> content vuoto");
        assert_eq!(r.stop_reason.as_deref(), Some("end_turn"));
    }

    /// (e) tre turni distinti (un tool ciascuno, separati nel tempo) consumati in
    /// ordine da complete() successive. Lo `step_index` e' irrilevante per i confini
    /// di turno: conta solo il GAP di `created_at` (qui ogni tool a +1s dal
    /// precedente -> tre turni). Verifica anche il caso single-tool-per-turno.
    #[sqlx::test]
    async fn replay_executor_turni_separati_single_tool(pool: PgPool) {
        create_tables(&pool).await;
        let run = insert_run(&pool, Some("done")).await;
        insert_step_at(&pool, run, 0, "a", "2026-06-27T10:00:00Z").await;
        insert_step_at(&pool, run, 2000, "b", "2026-06-27T10:00:01Z").await;
        insert_step_at(&pool, run, 3000, "c", "2026-06-27T10:00:02Z").await;

        let gw = ReplayLlmGateway::new(pool.clone(), run);
        assert_eq!(
            gw.complete(executor_req()).await.unwrap().tool_calls[0].name,
            "a"
        );
        assert_eq!(
            gw.complete(executor_req()).await.unwrap().tool_calls[0].name,
            "b"
        );
        assert_eq!(
            gw.complete(executor_req()).await.unwrap().tool_calls[0].name,
            "c"
        );
        // 4a: esausto -> end_turn.
        let last = gw.complete(executor_req()).await.unwrap();
        assert_eq!(last.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(last.content, "done");
    }

    /// Inserisce uno step con `created_at` ESPLICITO (per testare l'ordinamento
    /// temporale e il raggruppamento per turno a parita'/duplicazione di
    /// `step_index`). `tool_input` fisso a `{}` (irrilevante per questi test).
    async fn insert_step_at(
        pool: &PgPool,
        run_id: Uuid,
        step_index: i32,
        tool_name: &str,
        created_at_iso: &str,
    ) {
        sqlx::query(
            "INSERT INTO agent_steps \
             (id, run_id, step_index, tool_name, tool_input, status, created_at) \
             VALUES (gen_random_uuid(), $1, $2, $3, $4, 'completed', $5::timestamptz)",
        )
        .bind(run_id)
        .bind(step_index)
        .bind(tool_name)
        .bind(json!({}))
        .bind(created_at_iso)
        .execute(pool)
        .await
        .expect("insert step at");
    }

    /// FIX shadow LLM-Replay (difesa in profondita'): con `step_index` DUPLICATI
    /// (ondate retry/fallback del primario) l'ordine di `load_replay_steps` deve
    /// seguire `created_at` (tiebreak `step_index`, poi `id`), NON il solo
    /// `step_index`. Un ordinamento per solo `step_index` mescolerebbe le ondate.
    #[sqlx::test]
    async fn load_replay_steps_ordina_per_created_at_con_step_index_duplicati(pool: PgPool) {
        create_tables(&pool).await;
        let run = insert_run(&pool, Some("done")).await;
        // Ondate con step_index COLLIDENTI ma created_at crescente nell'ordine reale:
        // ondata 1 (idx 0,1) PRIMA, poi ondata 2 (idx 0,1 di nuovo, retry) DOPO.
        // Inserimento volutamente FUORI ordine per dimostrare che e' la query a
        // ordinare (non l'ordine di insert).
        insert_step_at(&pool, run, 1, "w2_b", "2026-06-27T10:00:04Z").await; // ondata 2, secondo
        insert_step_at(&pool, run, 0, "w1_a", "2026-06-27T10:00:01Z").await; // ondata 1, primo
        insert_step_at(&pool, run, 1, "w1_b", "2026-06-27T10:00:02Z").await; // ondata 1, secondo
        insert_step_at(&pool, run, 0, "w2_a", "2026-06-27T10:00:03Z").await; // ondata 2, primo

        let steps = load_replay_steps(&pool, run).await.expect("load steps");
        let order: Vec<&str> = steps.iter().map(|s| s.tool_name.as_str()).collect();
        assert_eq!(
            order,
            vec!["w1_a", "w1_b", "w2_a", "w2_b"],
            "ordine = created_at, NON mescolato per solo step_index"
        );
    }
}
