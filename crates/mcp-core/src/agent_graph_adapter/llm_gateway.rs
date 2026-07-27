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
//! [`GatewayLlmAdapter`] espone la completion REAL: IGNORA `LlmRequest::purpose`
//! quando il modello e' gia' risolto (il purpose serve solo a risolvere un modello
//! quando quello richiesto e' vuoto, `purpose_for_empty_model`).

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::PgPool;

use nexus_agent_graph::runtime::ports::{LlmGateway, LlmRequest, LlmResponse, LlmUsage, PortError};
use nexus_agent_graph::state::ToolUse;
// La resa di un errore del gateway vive accanto ai tipi che la sanno produrre
// (`crate::nexus_gateway`): qui si delega, non si ri-decide.
use crate::nexus_gateway::{
    rendered_from_error, GwMessage, GwMetadata, GwRequest, GwResponse, GwThinkingConfig, GwToolCall,
    GwToolFunctionCall, NexusGatewayClient,
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

    /// Osservazione dei turni REALI (model_observation, ANELLO 5): un turno
    /// degenere di una figura/sub-run deve contare nel catalog anche se il run
    /// poi muore in timeout.
    ///
    /// Solo con provider pinnato: su una cascata multi-provider l'errore
    /// aggregato non e' attribuibile al singolo modello, e attribuirlo lo stesso
    /// sporcherebbe il catalog del modello sbagliato.
    fn observe_pinned_turn_failure(&self, req: &LlmRequest, e: &anyhow::Error) {
        if req.provider.trim().is_empty() {
            return;
        }
        let cause = e
            .chain()
            .find_map(|c| c.downcast_ref::<crate::nexus_gateway::GatewayHttpError>())
            .and_then(|h| h.details.as_ref())
            .and_then(|d| d.get("primary_cause"))
            .and_then(|c| c.as_str())
            .map(str::to_owned);
        crate::model_observation::observe_turn_failure(
            self.db.clone(),
            req.provider.clone(),
            req.model.clone(),
            cause,
        );
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
                .to_string().into(),
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
                    .map_err(|msg| PortError::Llm(msg.into()))?;
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
        // RC-1 (fix gemini-3 empty-completion): thinking OBBLIGATORIO dal catalog
        // (punto con accesso a self.db, regola G: niente nomi modello hardcoded). Per i
        // modelli google con `agentic_thinking_policy='native'` iniettiamo un budget
        // bounded + il flag `mandatory` cosi' il gateway emette `Enabled(budget)` invece
        // di `DisabledForTools` (che gemini-3 rifiuta -> thinking illimitato -> risposta
        // vuota finish=length). GATE alla FAMIGLIA google (il fix e' google-API-specifico:
        // solo google.rs::resolve_thinking legge `mandatory`): 'native' e' usato anche da
        // OpenAI o1/o3/o4, che gestiscono il reasoning nativamente e NON vanno toccati.
        let is_google = matches!(
            req.provider.trim().to_lowercase().as_str(),
            "google" | "vertex" | "vertex_ai" | "gemini"
        );
        if is_google {
            if let Some(budget) = crate::capability::resolve_mandatory_thinking_budget(
                &self.db,
                &req.provider,
                &req.model,
            )
            .await
            {
                let explicit = gw_req.thinking.as_ref().and_then(|t| t.budget_tokens);
                gw_req.thinking = Some(GwThinkingConfig {
                    enabled: true,
                    budget_tokens: Some(explicit.unwrap_or(budget)),
                    mandatory: true,
                });
            }
        }
        let resp = match self.gateway.complete(gw_req).await {
            Ok(r) => r,
            Err(e) => {
                self.observe_pinned_turn_failure(&req, &e);
                return Err(classify_gateway_error(&e));
            }
        };
        let mut mapped = map_gw_response(resp);
        // Turno produttivo: azzera il contatore sul provider/model EFFETTIVI.
        {
            let provider = mapped
                .provider_used
                .clone()
                .unwrap_or_else(|| req.provider.clone());
            let model = mapped
                .model_used
                .clone()
                .unwrap_or_else(|| req.model.clone());
            if !provider.trim().is_empty() && !model.trim().is_empty() {
                crate::model_observation::observe_turn_success(self.db.clone(), provider, model);
            }
        }
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
        // Ramo di degrado: e' qui che finisce OGNI errore non-HTTP, cioe' tutto
        // il trasporto. Restituiva `err.to_string()`, e siccome a monte
        // l'errore era un `anyhow!` costruito sulla riga diagnostica dei log,
        // quella riga diventava il messaggio in chat. Ora l'errore di trasporto
        // e' TIPIZZATO (`GatewayTransportError`) e porta la sua frase.
        return PortError::Llm(rendered_from_error(err));
    };
    match http.code.as_deref() {
        Some("PROVIDER_ERROR") => {
            // Causa e codice si leggono dallo STESSO blocco `details`, e dallo
            // stesso fallimento: il gateway scrive `primary_cause` e
            // `failures[0]` insieme (`nexus-gateway/src/server/routes.rs`).
            let details = http.details.as_ref();
            let cause = match details
                .and_then(|d| d.get("primary_cause"))
                .and_then(|c| c.as_str())
            {
                // "transient": la chiamata e' appena fallita per errore
                // transitorio e il gateway ha messo il provider in cooldown
                // breve -> per il racconto equivale a un cooldown.
                Some("cooldown") | Some("transient") => ProviderFailureCause::Cooldown,
                Some("billing") | Some("cooldown_billing") => ProviderFailureCause::Billing,
                Some("client_error") => ProviderFailureCause::ClientError,
                // 413 request_too_large: ritentare lo stesso provider e' inutile, ma
                // un provider a finestra/limite piu' grande accetta -> failover
                // cross-provider (ramo else != ClientError in allows_cross_provider_failover),
                // invece di chiudere n/d. Simmetrico a `empty_completion`.
                Some("context_too_long") => ProviderFailureCause::ContextTooLong,
                // 200 degenere (content vuoto, zero tool-call, finish non
                // terminale): il provider e' sano ma il turno e' improduttivo.
                // Causa dedicata cosi' l'executor RIPIEGA cross-provider (ramo
                // else != ClientError), invece di chiudere con un hollow turn.
                Some("empty_completion") => ProviderFailureCause::EmptyCompletion,
                _ => ProviderFailureCause::Unknown,
            };
            PortError::ProviderUnavailable(
                ProviderUnavailableInfo::new(cause, rendered_from_error(err).message)
                    .with_code(codice_del_fallimento_primario(details)),
            )
        }
        Some("POLICY_TIER_EXCLUDED") => {
            PortError::ProviderUnavailable(ProviderUnavailableInfo::new(
                ProviderFailureCause::PolicyTierExcluded,
                rendered_from_error(err).message,
            ))
        }
        _ => PortError::Llm(rendered_from_error(err)),
    }
}

/// Codice d'errore STRUTTURATO del fallimento PRIMARIO, dai `details` del
/// gateway (`failures[0].code`, regola M: mai dalla prosa del messaggio).
///
/// E' lo stesso elemento da cui nasce `primary_cause`, quindi causa e codice
/// descrivono sempre il medesimo fallimento.
///
/// Perche' esiste: il codice viaggiava gia' sul wire e il consumatore esisteva
/// gia' ([`ProviderUnavailableInfo::allows_cross_provider_failover`], col
/// vocabolario DB `routing.client_error_failover_codes`), ma nessuno collegava i
/// due capi: `code` restava `None` su OGNI errore reale, e per un `ClientError`
/// `None` significa "non recuperabile". La whitelist non veniva quindi mai
/// consultata, e i test del gate restavano verdi perche' costruivano il codice a
/// mano invece di riceverlo dal produttore (regola O).
fn codice_del_fallimento_primario(details: Option<&Value>) -> Option<String> {
    details
        .and_then(|d| d.get("failures"))
        .and_then(|f| f.as_array())
        .and_then(|f| f.first())
        .and_then(|f| f.get("code"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
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
            // `mandatory` non arriva dall'executor: lo inietta `complete()` dalla
            // policy del catalog (RC-1, self.db). Qui default false (fn pura).
            mandatory: false,
        }),
        tool_choice: force_tool_choice_to_value(req.force_tool_choice),
        pin_provider: if req.provider.is_empty() {
            None
        } else {
            Some(req.provider.clone())
        },
        // `None` di proposito: questa e' una funzione PURA e il run non lo
        // conosce. Lo timbra il client in `NexusGatewayClient::complete`, che e'
        // stato costruito PER quel run (`from_db_for_run`). E' lo stesso motivo
        // per cui il pin del provider viene applicato al momento della chiamata.
        run_timeout_secs: None,
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
///
/// PUNTO UNICO (regola L): la traduzione wire->porta vive qui, l'unico confine fra
/// il formato del gateway e la porta `LlmResponse`. Valori ignoti -> passthrough
/// (robustezza: niente magic, una stringa sconosciuta cade poi sul default
/// `end_turn` del punto unico executor, come oggi).
///
/// `pub(crate)` perche' anche il path NEURALE ha bisogno dello stesso vocabolario:
/// `agent_turn_value_from_gw` (neural_client) espone il `finish_reason` normalizzato
/// nel Value del turno. Chiamare QUI e' l'unico modo di non riscrivere la mappa
/// altrove: il loop multi-step deve poter distinguere "troncato dal nostro cap"
/// (`max_tokens`) da "ha smesso di chiamare tool" (`end_turn`), e quella distinzione
/// nasce dal `finish_reason` del gateway, non da un'euristica.
pub(crate) fn normalize_gw_finish_reason(finish: &str) -> String {
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
/// Costo in USD del turno, dal punto unico `nexus-pricing` (regola G/M: prezzo
/// dal DB, token dal segnale strutturato). `None` se il prezzo e' ignoto
/// (modello non a catalog, `pricing_state='unknown'`, currency non configurata o
/// DB in errore) -> nessun cap spurio.
///
/// Qui viveva una TERZA implementazione del listino: query propria su
/// `ai_price_catalog`, senza filtro di currency ne' di finestra `effective_*`,
/// senza `pricing_state` e — soprattutto — cieca alla cache, cioe' con tutti i
/// token di prompt a tariffa piena. Non era solo cosmetica: questo numero
/// alimenta `run_cost_cumulative_usd`, il FRENO DI SPESA del run, quindi la
/// sovrastima stringeva il freno.
async fn turn_cost_usd(db: &PgPool, provider: &str, model: &str, usage: &LlmUsage) -> Option<f64> {
    let lookup = match nexus_pricing::resolve_active_price(db, provider, model).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, provider, model,
                "turn_cost_usd: listino non leggibile -> costo del turno non calcolabile");
            return None;
        }
    };
    let nexus_pricing::PriceLookup::Priced(price) = lookup else {
        return None;
    };
    // I due contratti hanno la stessa convenzione — prompt LORDO, cache come
    // sottoinsieme — quindi i conteggi si passano com'e': lo scorporo lo fa il
    // listino, unico punto in cui il netto serve.
    let tokens = nexus_pricing::TokenUsage {
        prompt_tokens: usage.prompt_tokens.max(0),
        completion_tokens: usage.completion_tokens.max(0),
        cache_read_tokens: usage.cache_read_tokens.unwrap_or(0).max(0),
        cache_creation_tokens: usage.cache_creation_tokens.unwrap_or(0).max(0),
    };
    Some(nexus_pricing::calculate_cost_breakdown(&price, &tokens).total_cost)
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
            // `input_tokens` del gateway e' gia' il prompt LORDO (convenzione
            // unica, normalizzata dall'adapter del provider): si copia e basta.
            prompt_tokens: resp.usage.input_tokens as i64,
            completion_tokens: resp.usage.output_tokens as i64,
            // Totale del turno = prompt lordo + completion. I due conteggi di
            // cache sono gia' dentro il prompt: sommarli qui li conterebbe due
            // volte e questo totale divergerebbe da quello che l'executor scrive
            // nello stato per lo stesso turno.
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

/// Helper (regola L): blocchi `assistant_content` in forma `anthropic_content` da
/// un testo opzionale + i `tool_use` (blocco text se non vuoto, poi i blocchi
/// tool_use; vuoto se non c'e' alcun tool_use). Usato da [`map_gw_response`].
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

    /// Il vocabolario REALE dei codici recuperabili, dal produttore che lo
    /// definisce (safe-default di `ExecutorConfig`, sovrascritto dal DB in
    /// esercizio). Ricopiarlo qui a mano renderebbe il test cieco a una modifica
    /// del vocabolario: e' l'errore che ha tenuto verde questo gate mentre era
    /// morto (regola O).
    fn codici_recuperabili() -> Vec<String> {
        nexus_agent_graph::nodes::ExecutorConfig::default().recoverable_client_error_codes
    }

    #[test]
    fn il_codice_del_provider_arriva_al_gate_del_failover() {
        // Un 400 PROVIDER-SPECIFICO (Google invalid_argument): un altro provider
        // accetterebbe la stessa richiesta -> il failover DEVE essere consentito.
        // L'asserzione e' sul VERDETTO, non sulla stringa: e' il consumatore vero
        // del codice, ed e' l'unica cosa che prova che il dato ha attraversato
        // tutto il percorso invece di fermarsi a meta' (regola O).
        let err = gw_err(
            400,
            "{\"error\":\"tutti i provider hanno fallito -> google (google HTTP 400: \
invalid argument)\",\"code\":\"PROVIDER_ERROR\",\"details\":{\"primary_cause\":\"client_error\",\
\"failures\":[{\"provider\":\"google\",\"class\":\"client_error\",\"status\":400,\
\"code\":\"invalid_argument\"}]}}",
        );
        match classify_gateway_error(&err) {
            PortError::ProviderUnavailable(info) => {
                assert_eq!(info.code.as_deref(), Some("invalid_argument"));
                assert!(
                    info.allows_cross_provider_failover(&codici_recuperabili()),
                    "un 400 provider-specifico deve poter ripiegare su un altro provider"
                );
            }
            other => panic!("atteso ProviderUnavailable, avuto {other:?}"),
        }
    }

    #[test]
    fn il_400_di_formato_condiviso_non_apre_il_failover() {
        // Mistral invalid_request_message_order: la history malformata e' la
        // STESSA per ogni provider, ritentare altrove fallirebbe uguale bruciando
        // token (incidente f0ad0337). Il codice arriva, e il verdetto e' NO.
        let err = gw_err(
            400,
            "{\"error\":\"tutti i provider hanno fallito -> mistral (mistral HTTP 400: Not the \
same number of function calls and responses)\",\"code\":\"PROVIDER_ERROR\",\
\"details\":{\"primary_cause\":\"client_error\",\"failures\":[{\"provider\":\"mistral\",\
\"class\":\"client_error\",\"status\":400,\"code\":\"invalid_request_message_order\"}]}}",
        );
        match classify_gateway_error(&err) {
            PortError::ProviderUnavailable(info) => {
                assert_eq!(info.code.as_deref(), Some("invalid_request_message_order"));
                assert!(
                    !info.allows_cross_provider_failover(&codici_recuperabili()),
                    "un 400 di history condivisa fallirebbe su qualunque provider"
                );
            }
            other => panic!("atteso ProviderUnavailable, avuto {other:?}"),
        }
    }

    #[test]
    fn senza_codice_strutturato_il_failover_resta_chiuso() {
        // Gateway che non espone `failures[].code`: nessun segnale strutturato,
        // quindi nessun failover cieco (conservativo, regola M). E' anche lo
        // stato in cui si trovava OGNI errore prima che il codice venisse
        // collegato: serve a distinguere "non recuperabile" da "non misurato".
        let err = gw_err(
            400,
            "{\"error\":\"tutti i provider hanno fallito -> deepseek (HTTP 400)\",\
\"code\":\"PROVIDER_ERROR\",\"details\":{\"primary_cause\":\"client_error\",\
\"failures\":[{\"provider\":\"deepseek\",\"class\":\"client_error\",\"status\":400}]}}",
        );
        match classify_gateway_error(&err) {
            PortError::ProviderUnavailable(info) => {
                assert_eq!(info.code, None);
                assert!(!info.allows_cross_provider_failover(&codici_recuperabili()));
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
            ledger: None,
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
        // Totale = prompt LORDO (10) + completion (5). I 3+7 di cache sono gia'
        // dentro i 10: sommarli li conterebbe due volte.
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

    // ── turn_cost_usd: il numero che stringe il freno di spesa ────────────────
    //
    // Questo costo alimenta `run_cost_cumulative_usd`, cioe' il cap in dollari del
    // run: un errore qui non e' contabile, e' operativo — o si spende oltre il
    // tetto, o il run viene fermato per un costo che non e' stato sostenuto.
    //
    // I test girano sullo schema META reale (regola O): il filtro di currency, la
    // finestra `effective_*` e lo scarto su `pricing_state='unknown'` vivono nella
    // QUERY del punto unico `nexus-pricing`, e una fixture `CREATE TABLE` ricopiata
    // a mano non potrebbe esercitarli.

    /// L'usage come arriva DAVVERO al chiamante di `turn_cost_usd`: payload del
    /// wire -> `GwResponse` -> `map_gw_response`. E' la strada di
    /// `NexusGatewayLlmPort::complete`, che passa `&mapped.usage`. Costruire la
    /// `LlmUsage` della porta a mano salterebbe proprio il passaggio in cui le
    /// quantita' vengono ripartite.
    fn usage_dal_wire() -> LlmUsage {
        // `input_tokens` e' il prompt LORDO: i 2M letti da cache e i 0.5M scritti
        // ne fanno parte, quindi restano 1M a tariffa piena.
        let resp: GwResponse = serde_json::from_str(
            r#"{
                "content": "ok",
                "usage": {
                    "input_tokens": 3500000,
                    "output_tokens": 400000,
                    "cache_read_tokens": 2000000,
                    "cache_creation_tokens": 500000
                },
                "model_used": "claude-x",
                "provider_used": "anthropic",
                "latency_ms": 3,
                "finish_reason": "stop"
            }"#,
        )
        .expect("payload wire del gateway");
        map_gw_response(resp).usage
    }

    /// Riga di listino con TUTTI gli assi su cui la query del punto unico
    /// discrimina: currency, finestra di validita', stato del prezzo, tariffe.
    struct RigaListino<'a> {
        model: &'a str,
        currency: &'a str,
        pricing_state: &'a str,
        /// Offset da `NOW()` per `effective_from` (intervallo Postgres).
        da: &'a str,
        /// Offset per `effective_to`; `None` = finestra ancora aperta.
        a: Option<&'a str>,
        /// Tariffe di input/output per milione. Quelle di cache NON si passano:
        /// le deriva la INSERT con la stessa regola della mig 0130
        /// (`read = input x 0.10`, `creation = input x 1.25`), cosi' una riga a
        /// tariffe zero resta zero DOVUNQUE invece di essere un ibrido che in
        /// catalog non esiste.
        ///
        /// Non sono un dettaglio della fixture: il trigger della mig 0583 promuove
        /// a `'priced'` qualunque riga scritta con `pricing_state='unknown'` e un
        /// costo > 0. Un "unknown" a tariffe vere in produzione NON esiste — il
        /// placeholder ha per forza tariffe a zero, ed e' proprio li' che lo zero
        /// "non so quanto costa" deve restare distinguibile dallo zero "gratis".
        tariffe: (f64, f64),
    }

    /// Riga a listino tipica: tariffe distinte, currency di piattaforma, in vigore.
    fn riga_viva(model: &str) -> RigaListino<'_> {
        RigaListino {
            model,
            currency: "USD",
            pricing_state: "priced",
            da: "-1 hour",
            a: None,
            tariffe: (3.0, 15.0),
        }
    }

    async fn seed_prezzo(pool: &PgPool, riga: RigaListino<'_>) {
        sqlx::query(
            "INSERT INTO ai_price_catalog ( \
                 provider, model, \
                 input_cost_per_million_tokens, output_cost_per_million_tokens, \
                 cache_read_cost_per_million_tokens, cache_creation_cost_per_million_tokens, \
                 currency, pricing_state, effective_from, effective_to \
             ) VALUES ('anthropic', $1, $6, $7, $6 * 0.10, $6 * 1.25, $2, $3, \
                       NOW() + $4::interval, \
                       CASE WHEN $5::text IS NULL THEN NULL \
                            ELSE NOW() + $5::interval END)",
        )
        .bind(riga.model)
        .bind(riga.currency)
        .bind(riga.pricing_state)
        .bind(riga.da)
        .bind(riga.a)
        .bind(riga.tariffe.0)
        .bind(riga.tariffe.1)
        .execute(pool)
        .await
        .expect("seed ai_price_catalog");
    }

    /// Caso vivo: il prezzo c'e' e il prompt si scorpora in tre parti, ognuna
    /// alla tariffa che le compete, non tutto alla tariffa piena di input.
    ///
    /// A tariffa piena sul lordo (3,5M x 3.0 = 10.50, piu' 6.00 di output) il
    /// totale sarebbe 16.50: e' la sovrastima che stringeva il freno di spesa.
    #[sqlx::test(migrator = "nexus_test_schema::META_MIGRATOR")]
    async fn turn_cost_usd_paga_ogni_quantita_alla_sua_tariffa(pool: PgPool) {
        seed_prezzo(&pool, riga_viva("claude-x")).await;

        let costo = turn_cost_usd(&pool, "anthropic", "claude-x", &usage_dal_wire())
            .await
            .expect("prezzo a listino -> costo calcolabile");

        // 1M x 3.0 + 0.4M x 15.0 + 2M x 0.3 + 0.5M x 3.75.
        assert!(
            (costo - 11.475).abs() < 1e-9,
            "costo {costo}, atteso 11.475 (a tariffa piena sul lordo sarebbe 16.5)"
        );
    }

    /// Le tre forme in cui il listino NON si applica alla chiamata. In tutte il
    /// costo resta `None`: un numero inventato qui e' peggio dell'assenza, perche'
    /// il cap in dollari lo tratterebbe come speso.
    #[sqlx::test(migrator = "nexus_test_schema::META_MIGRATOR")]
    async fn turn_cost_usd_non_applica_un_listino_che_non_e_suo(pool: PgPool) {
        // La currency di piattaforma e' USD (mig 0294): una riga in EUR non e' il
        // prezzo di questa chiamata.
        seed_prezzo(
            &pool,
            RigaListino {
                currency: "EUR",
                ..riga_viva("altra-currency")
            },
        )
        .await;
        // Finestra CHIUSA in passato: il prezzo non e' piu' in vigore.
        seed_prezzo(
            &pool,
            RigaListino {
                da: "-2 day",
                a: Some("-1 day"),
                ..riga_viva("scaduto")
            },
        )
        .await;
        // Finestra che deve ancora aprirsi.
        seed_prezzo(
            &pool,
            RigaListino {
                da: "+1 day",
                ..riga_viva("futuro")
            },
        )
        .await;

        let usage = usage_dal_wire();
        for model in ["altra-currency", "scaduto", "futuro", "mai-visto"] {
            assert_eq!(
                turn_cost_usd(&pool, "anthropic", model, &usage).await,
                None,
                "il listino di '{model}' non si applica a questa chiamata: \
                 il costo deve restare non calcolabile"
            );
        }
    }

    /// `pricing_state='unknown'` (mig 0477): la riga esiste ma il prezzo e' un
    /// PLACEHOLDER a zero. Trattarla come un prezzo darebbe `Some(0.0)`, cioe' un
    /// turno dichiarato gratuito: il freno di spesa lo sommerebbe come nulla e non
    /// scatterebbe mai. La distinzione fra "costa zero" e "non so quanto costa"
    /// vive qui, ed e' l'unica cosa che separa i due esiti.
    #[sqlx::test(migrator = "nexus_test_schema::META_MIGRATOR")]
    async fn turn_cost_usd_scarta_il_prezzo_dichiarato_ignoto(pool: PgPool) {
        seed_prezzo(
            &pool,
            RigaListino {
                pricing_state: "unknown",
                tariffe: (0.0, 0.0),
                ..riga_viva("claude-x")
            },
        )
        .await;

        assert_eq!(
            turn_cost_usd(&pool, "anthropic", "claude-x", &usage_dal_wire()).await,
            None,
            "pricing_state='unknown' non e' un prezzo: nessun costo va calcolato, \
             nemmeno lo zero che le tariffe placeholder produrrebbero"
        );
    }

    /// Listino non leggibile (DB in errore): `None` e nessun panico. E' il ramo
    /// che tiene il turno in piedi quando la contabilita' non e' disponibile —
    /// far fallire la chiamata LLM per un problema di prezzo sarebbe un outage.
    #[sqlx::test(migrator = "nexus_test_schema::META_MIGRATOR")]
    async fn turn_cost_usd_su_listino_illeggibile_resta_none(pool: PgPool) {
        seed_prezzo(&pool, riga_viva("claude-x")).await;
        // Il prezzo c'e' e sarebbe calcolabile: e' la lettura a rompersi.
        assert!(turn_cost_usd(&pool, "anthropic", "claude-x", &usage_dal_wire())
            .await
            .is_some());

        sqlx::query("DROP TABLE ai_price_catalog CASCADE")
            .execute(&pool)
            .await
            .expect("drop del catalog");

        assert_eq!(
            turn_cost_usd(&pool, "anthropic", "claude-x", &usage_dal_wire()).await,
            None,
            "errore di lettura del listino -> costo non calcolabile, mai un numero"
        );
    }
}
