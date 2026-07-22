use anyhow::Result;
use nexus_types::error_presentation::{
    render_user_error, ErrorDomain, ErrorFacts, HasErrorFacts, RenderedError, TransportFacts,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// HTTP client per il Nexus LLM Gateway (porta 4060).
#[derive(Clone)]
pub struct NexusGatewayClient {
    http: reqwest::Client,
    base_url: String,
    service_token: String,
    /// Il run per cui questo client e' stato costruito, se noto: viene timbrato
    /// su ogni richiesta cosi' il gateway dimensiona i propri budget sullo stesso
    /// cronometro del chiamante.
    run_timeout_secs: Option<u64>,
}

/// Messaggio della conversazione inviato al gateway.
///
/// `content` e' un [`Value`] perche' il contratto del server (`MessageContent`
/// untagged in `nexus-gateway::types`) accetta SIA una stringa semplice (turno
/// testuale) SIA una lista di blocchi `{type, ...}` (tool_use/tool_result/image).
/// L'agent graph adapter (executor) deve poter trasportare i blocchi per la
/// continuita' tool_use/tool_result, quindi il campo non puo' essere un `String`
/// rigido. I call site testuali costruiscono `content: json!("...")`.
///
/// CONTINUITA' TOOL MULTI-TURN (regola L, allineato a `LlmMessage` del server in
/// `nexus-gateway::types`): un turno `assistant` che ha chiamato tool porta i
/// `tool_use` in [`GwMessage::tool_calls`] (NON appiattiti in `content`); un turno
/// `tool` (risultato) ha `role="tool"` + [`GwMessage::tool_call_id`] valorizzato.
/// Il server (`to_anthropic_messages`) riconosce la coppia tool_use/tool_result
/// SOLO da questi campi: senza di essi Anthropic risponde HTTP 400 (`tool_use ids
/// without tool_result`). Campi `Option` additivi: omessi (`skip_serializing_if`)
/// sui messaggi testuali, retrocompatibili coi call site esistenti.
#[derive(Serialize, Clone, Debug, Default)]
pub struct GwMessage {
    pub role: String,
    pub content: Value,
    /// Tool-call emesse da un turno `assistant` (continuita' tool_use). Gli id qui
    /// DEVONO combaciare col [`GwMessage::tool_call_id`] del messaggio `tool` che ne
    /// porta il risultato. Omesso quando `None` (turno testuale).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<GwToolCall>>,
    /// Id della tool-call a cui un messaggio `role="tool"` (risultato) risponde.
    /// Omesso quando `None` (qualunque ruolo != tool).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Reasoning (`reasoning_content`) di un turno `assistant` precedente generato
    /// in thinking mode (DeepSeek), da RI-PASSARE al gateway: il server lo inoltra
    /// SOLO al dialetto DeepSeek (vincolo HTTP 400 "The reasoning_content in the
    /// thinking mode must be passed back to the API"). Allineato a
    /// `LlmMessage::reasoning` del server (`nexus-gateway::types`). Omesso quando
    /// `None` (turno senza reasoning / altri ruoli o provider).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Firma opaca del blocco `thinking` (Anthropic) di un turno `assistant`
    /// precedente, da RI-PASSARE al gateway: il server la inoltra SOLO ad Anthropic
    /// (vincolo HTTP 400 sui turni con tool). Allineata a
    /// `LlmMessage::thinking_signature` del server (`nexus-gateway::types`). Omessa
    /// quando `None` (turno senza thinking / altri ruoli o provider).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct GwMetadata {
    pub tenant_id: String,
    pub user_id: String,
    pub request_id: String,
    pub sensitivity_tier: u8,
    pub feature: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct GwThinkingConfig {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
    /// Thinking OBBLIGATORIO per il modello (gemini-3, policy DB
    /// `agentic_thinking_policy='native'`): quando true il gateway emette un thinking
    /// budget bounded (Enabled) invece di DisabledForTools (che gemini-3 rifiuta ->
    /// thinking illimitato -> risposta vuota). Popolato in `complete()` dal catalog
    /// (self.db, regola G). Serializzato verso il gateway (default false).
    #[serde(default)]
    pub mandatory: bool,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct GwRequest {
    pub model: String,
    pub messages: Vec<GwMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Tool dichiarati al modello. Il contratto del server e' lo schema OpenAI
    /// (`[{type:"function", function:{name, description?, parameters}}]`): chi
    /// passa tool Anthropic-style (`{name, description, input_schema}`) li
    /// converte PRIMA di valorizzare questo campo (vedi adapter LlmGateway).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
    /// Vincolo sul formato della risposta nel contratto gateway completo
    /// (`response_format` OpenAI-style). Il gateway lo inoltra solo ai provider
    /// che lo supportano o lo traducono nel proprio dialetto.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
    /// Configurazione thinking esplicita del chiamante. `None` conserva il
    /// comportamento storico DB-driven/provider-specifico del gateway.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<GwThinkingConfig>,
    /// Vincolo di scelta tool in stile OpenAI (`"auto"` | `"required"` | `"none"`
    /// | `{"type":"function","function":{"name":"X"}}`). DEVE arrivare al gateway
    /// per non neutralizzare il force-action anti-loop (memoria progetto "Gateway
    /// droppava tool_choice"): omesso quando `None` (equivale ad `auto`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    /// Pin esplicito del provider (bypass routing nel gateway). Quando `Some`, il
    /// gateway esegue ESATTAMENTE quel provider col `model` indicato, senza
    /// `policy.decide` ne' fallback cross-provider. Il chiamante (mcp-core) che ha
    /// gia' deciso provider+modello via routing matrix DB lo valorizza per evitare
    /// un secondo routing divergente (regola G).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin_provider: Option<String>,
    /// Durata del run che ha originato la richiesta: da qui il gateway deriva i
    /// budget di QUESTA chiamata invece di usare il default globale.
    ///
    /// Non si valorizza a mano nei costruttori: lo timbra il client in
    /// [`NexusGatewayClient::complete`], che e' l'unico a sapere per quale run e'
    /// stato costruito. Chi compone la richiesta (funzioni pure come
    /// `build_gw_request`) il run non lo conosce.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_timeout_secs: Option<u64>,
    pub metadata: GwMetadata,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct GwUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Token serviti da prompt cache (Anthropic `cache_read_input_tokens`).
    /// `None` se il provider non li riporta.
    #[serde(default)]
    pub cache_read_tokens: Option<u32>,
    /// Token scritti in cache (creazione voce). Vedi sopra.
    #[serde(default)]
    pub cache_creation_tokens: Option<u32>,
}

/// Funzione chiamata in una tool-call (forma OpenAI Chat Completions): `arguments`
/// e' una STRINGA JSON. `Serialize`+`Deserialize`: la stessa forma serve sia in
/// RISPOSTA (tool_calls emesse dal modello) sia in RICHIESTA (tool_calls di un
/// turno assistant precedente re-inviato per la continuita' multi-turn).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GwToolFunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Tool-call emessa dal modello, come la riporta il gateway e come la rispedisce
/// il chiamante nei turni successivi (`LlmToolCall` del contratto: `{id, type,
/// function:{name, arguments}}`). Bidirezionale (`Serialize`+`Deserialize`).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GwToolCall {
    pub id: String,
    /// Discriminante OpenAI (`"function"`). In risposta e' deserializzato per
    /// tolleranza; in richiesta DEVE valere `"function"`: lo costruiamo
    /// esplicitamente nell'adapter. `default` -> stringa vuota in deserializzazione
    /// se assente (tollerante).
    #[serde(rename = "type", default)]
    pub kind: String,
    pub function: GwToolFunctionCall,
    /// Firma opaca di reasoning (`thoughtSignature`) di Gemini 3, PER-CALL.
    /// Combacia col campo omonimo di `LlmToolCall` (contratto gateway): il
    /// gateway la emette in RISPOSTA su ogni tool-call e la esige di ritorno in
    /// RICHIESTA sulla stessa `functionCall`, pena HTTP 400 INVALID_ARGUMENT.
    /// Additivo/tollerante (`default` + skip se `None`): assente per gli altri
    /// provider e retrocompatibile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

/// Informazioni sul re-routing automatico per motivi di privacy.
/// Presente nella risposta quando la richiesta è stata instradata su provider locale
/// al posto del provider cloud originalmente richiesto.
#[derive(Deserialize, Debug, Clone)]
pub struct GwPrivacyRerouted {
    pub provider: String,
    pub blocked_tier: u8,
    pub reason: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct GwResponse {
    pub content: String,
    /// Tool-call emesse dal modello (vuoto/`None` per un turno testuale). DEVE
    /// essere popolato dal gateway quando il modello chiama un tool, altrimenti il
    /// force-action e' inutile (memoria progetto "google tool monco"): l'adapter
    /// le mappa in `ToolUse` per il `Message::Ai`.
    #[serde(default)]
    pub tool_calls: Option<Vec<GwToolCall>>,
    pub usage: GwUsage,
    pub model_used: String,
    pub provider_used: String,
    pub latency_ms: u64,
    pub finish_reason: String,
    /// Presente se il gateway ha re-instradato automaticamente su provider locale per privacy
    #[serde(default)]
    pub privacy_rerouted: Option<GwPrivacyRerouted>,
    /// Testo del ragionamento (extended thinking) quando il provider lo emette.
    /// Parte del contratto wire (deserializzazione tollerante). La porta
    /// [`nexus_agent_graph::runtime::ports::LlmResponse`] non espone ancora il
    /// reasoning: il campo e' qui pronto per il wiring futuro, non ancora letto.
    #[serde(default)]
    #[allow(dead_code)]
    pub reasoning: Option<String>,
    /// Firma opaca del blocco `thinking` da ri-passare nei turni con tool
    /// (Anthropic). `None` per gli altri provider. Ora LETTA dall'adapter
    /// (`map_gw_response`) e trasportata nel round-trip via
    /// `Message::Ai::thinking_signature`.
    #[serde(default)]
    pub thinking_signature: Option<String>,
    /// Citazioni (URL fonti) dei provider di ricerca (Perplexity `citations`).
    /// Parte del contratto wire (default tollerante); propagata nel `metadata` del
    /// messaggio assistant per il pannello "Fonti consultate". `None` per gli altri
    /// provider (regola M: campo strutturato, mai estratto dal testo).
    #[serde(default)]
    pub citations: Option<Vec<String>>,
}

/// Errore HTTP del Nexus Gateway coi segnali STRUTTURATI del body JSON estratti
/// al punto di costruzione (regola M): `code` (`PROVIDER_ERROR`,
/// `POLICY_TIER_EXCLUDED`, `TIER_BLOCKED`, ...) e `details` (fallimenti
/// per-provider con classe, tier rilevato, provider ammessi — vedi
/// `PipelineError` in `nexus-gateway::server::routes`). I decisori fanno
/// `downcast_ref::<GatewayHttpError>()`, mai match sul testo. `Display` e'
/// IDENTICO al vecchio `bail!("Nexus Gateway {status}: {body}")` cosi' i
/// consumatori legacy della stringa non cambiano comportamento.
#[derive(Debug)]
pub struct GatewayHttpError {
    pub status: u16,
    /// Riga di status per il display (es. "500 Internal Server Error").
    status_text: String,
    /// Codice d'errore strutturato dal body JSON del gateway, se presente.
    pub code: Option<String>,
    /// Blocco `details` del body (failures classificate, tier, ammessi).
    pub details: Option<Value>,
    /// La FRASE gia' resa dal gateway (`user_message`), quando la risposta la
    /// porta. Il gateway l'ha scritta mentre provider, modello e status del
    /// fornitore erano ancora vivi: qui quei fatti non esistono piu', quindi
    /// questa frase si TRASPORTA, non si ri-deriva.
    pub user_message: Option<String>,
    /// L'identificatore canonico della classe deciso dal gateway (`user_code`).
    /// Piu' specifico di quello che si puo' dedurre da questo lato del confine
    /// (dove tutto e' "errore del gateway").
    pub user_code: Option<String>,
    /// Body grezzo: solo display/log, mai per decidere.
    pub body: String,
}

impl GatewayHttpError {
    pub fn from_response(status: reqwest::StatusCode, body: String) -> Self {
        let parsed: Option<Value> = serde_json::from_str(&body).ok();
        let stringa = |chiave: &str| {
            parsed
                .as_ref()
                .and_then(|v| v.get(chiave))
                .and_then(|c| c.as_str())
                .map(str::to_string)
        };
        let details = parsed.as_ref().and_then(|v| v.get("details")).cloned();
        // La resa si rilegge dal punto unico che la scrive dall'altro lato: cosi'
        // un rename delle chiavi non puo' rompere il solo trasporto lasciando
        // verdi i test dei due estremi.
        let rendered = parsed.as_ref().and_then(RenderedError::from_wire);
        Self {
            status: status.as_u16(),
            status_text: status.to_string(),
            code: stringa("code"),
            details,
            user_message: rendered.as_ref().map(|r| r.message.clone()),
            user_code: rendered.as_ref().map(|r| r.code.clone()),
            body,
        }
    }
}

impl std::fmt::Display for GatewayHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Nexus Gateway {}: {}", self.status_text, self.body)
    }
}

impl std::error::Error for GatewayHttpError {}

/// Descrive un errore di TRASPORTO di reqwest con i suoi segnali strutturati e
/// la catena delle cause (punto unico, regola L+M).
///
/// Perche' esiste: `map_err(|e| anyhow!("... {e}"))` stampa solo il `Display` di
/// `reqwest::Error`, che per un fallimento di invio e' sempre e solo
/// "error sending request for url (...)" — una frase che non dice NULLA su cosa
/// sia andato storto. La causa vera vive in `source()` (es. "connection closed
/// before message completed", `os error 10054` = reset del peer, `10055` =
/// buffer di sistema esauriti) e nei predicati tipizzati di reqwest. Senza,
/// resta solo da indovinare: e' esattamente il caso dei 3 `agent_turn` che
/// falliscono nello stesso millisecondo verso 127.0.0.1:4060 mentre `/health`
/// risponde in 0.33s.
fn transport_error_detail(e: &reqwest::Error) -> String {
    let mut kinds: Vec<&str> = Vec::new();
    if e.is_connect() {
        kinds.push("connect");
    }
    if e.is_timeout() {
        kinds.push("timeout");
    }
    if e.is_request() {
        kinds.push("request");
    }
    if e.is_body() {
        kinds.push("body");
    }
    if e.is_decode() {
        kinds.push("decode");
    }
    if kinds.is_empty() {
        kinds.push("altro");
    }
    // Catena delle cause: e' qui che vive l'informazione utile.
    let mut chain: Vec<String> = Vec::new();
    let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(s) = src {
        // Codice OS grezzo quando la causa e' un errore di I/O: e' il segnale
        // piu' specifico che esista (10054 reset, 10055 no buffer, 10048 porte).
        if let Some(io) = s.downcast_ref::<std::io::Error>() {
            chain.push(match io.raw_os_error() {
                Some(code) => format!("io({:?}, os_error={code}): {io}", io.kind()),
                None => format!("io({:?}): {io}", io.kind()),
            });
        } else {
            chain.push(s.to_string());
        }
        src = std::error::Error::source(s);
    }
    if chain.is_empty() {
        chain.push("(nessuna causa sottostante)".to_string());
    }
    format!("{e} [kind={}] <- {}", kinds.join("+"), chain.join(" <- "))
}

/// I segnali STRUTTURATI del fallimento di trasporto, dai predicati tipizzati di
/// reqwest e dal primo `io::Error` della catena (regola M).
///
/// Gemello strutturato di [`transport_error_detail`]: quello produce la riga
/// diagnostica per i log, questo i FATTI da cui nasce la frase per l'utente. La
/// stessa catena serve due canali diversi, e nessuno dei due deriva dall'altro
/// leggendone il testo.
fn transport_facts(e: &reqwest::Error, target: &str) -> TransportFacts {
    let mut io_kind = None;
    let mut os_error = None;
    let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(s) = src {
        if let Some(io) = s.downcast_ref::<std::io::Error>() {
            io_kind = Some(format!("{:?}", io.kind()));
            os_error = io.raw_os_error();
            break;
        }
        src = std::error::Error::source(s);
    }
    TransportFacts {
        is_connect: e.is_connect(),
        is_timeout: e.is_timeout(),
        io_kind,
        os_error,
        target: Some(target.to_string()),
    }
}

/// Errore di TRASPORTO verso il gateway, tipizzato (regole L+M).
///
/// Qui viveva `anyhow!("Nexus Gateway HTTP error: {}", transport_error_detail(&e))`:
/// una stringa opaca che a valle nessuno poteva piu' interrogare. Il ramo di
/// classificazione (`classify_gateway_error`) cercava solo `GatewayHttpError`,
/// non lo trovava, e ripiegava su `err.to_string()` — cioe' la riga diagnostica
/// nata per i log finiva TALE E QUALE nella bolla di chat:
/// "error sending request for url (...) [kind=connect] <- io(ConnectionRefused,
/// os_error=10061)".
///
/// Ora l'errore porta i suoi fatti. `Display` da' la frase umana, `detail` (nei
/// fatti) conserva la catena INTATTA per i log: il segnale diagnostico non si
/// perde, cambia canale.
#[derive(Debug)]
pub struct GatewayTransportError {
    facts: ErrorFacts,
}

impl GatewayTransportError {
    pub fn from_reqwest(e: &reqwest::Error, target: &str) -> Self {
        let mut facts = ErrorFacts::opaque(ErrorDomain::Transport, transport_error_detail(e));
        facts.transport = Some(transport_facts(e, target));
        Self { facts }
    }
}

impl HasErrorFacts for GatewayTransportError {
    fn error_facts(&self) -> ErrorFacts {
        self.facts.clone()
    }
}

impl std::fmt::Display for GatewayTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.rendered().message)
    }
}

impl std::error::Error for GatewayTransportError {}

/// I fatti di un errore HTTP del gateway: `code` e `details` sono gia' estratti
/// dal body al punto di costruzione, e il body integrale scende a `detail`.
///
/// `upstream_message` e' la frase che il gateway ha gia' reso. Qui cablava
/// `None`, e non poteva essere altrimenti: da questo lato del confine i fatti
/// del fornitore — quale provider, quale modello, quale status — non esistono
/// piu', quindi il messaggio poteva solo dire "il servizio AI interno ha
/// risposto con un errore". Ora la frase viaggia col suo errore.
impl HasErrorFacts for GatewayHttpError {
    fn error_facts(&self) -> ErrorFacts {
        let class = self
            .details
            .as_ref()
            .and_then(|d| d.get("primary_cause"))
            .and_then(|c| c.as_str())
            .map(str::to_string);
        let mut facts = ErrorFacts {
            domain: ErrorDomain::Gateway,
            http_status: Some(self.status),
            code: self.code.clone(),
            class,
            provider: None,
            model: None,
            transport: None,
            upstream_message: None,
            detail: self.body.clone(),
        };
        if let Some(m) = &self.user_message {
            facts = facts.with_upstream(m.clone());
        }
        facts
    }
}

/// Il [`RenderedError`] di un errore che arriva dal client gateway.
///
/// PUNTO DI RACCORDO (regola L) fra i produttori tipizzati di questo modulo e il
/// punto unico di presentazione: cerca nella catena il primo errore che sa
/// dichiarare i propri fatti e delega la frase a `render_user_error`. Nessuna
/// ispezione del testo: se nessuno dei tipi noti e' presente, i fatti sono
/// onestamente opachi e il messaggio resta generico, ma il dettaglio tecnico
/// finisce dove va — in `detail`, non nella bolla di chat.
///
/// Vive QUI, accanto ai due tipi di cui fa il downcast, e non nell'adapter del
/// motore: la stessa domanda ("che frase mostro per questo errore?") se la pone
/// anche il confine HTTP della chat, e due copie divergerebbero.
pub fn rendered_from_error(err: &anyhow::Error) -> RenderedError {
    if let Some(t) = err.chain().find_map(|c| c.downcast_ref::<GatewayTransportError>()) {
        return t.rendered();
    }
    if let Some(h) = err.chain().find_map(|c| c.downcast_ref::<GatewayHttpError>()) {
        let mut rendered = h.rendered();
        // Il verdetto del gateway VINCE su quello locale: e' stato deciso dove
        // status e codice del fornitore erano ancora leggibili. Da qui, ogni
        // errore del gateway sarebbe indistinguibile da ogni altro.
        if let Some(code) = h.user_code.clone().filter(|c| !c.trim().is_empty()) {
            rendered.code = code;
        }
        return rendered;
    }
    render_user_error(&ErrorFacts::opaque(ErrorDomain::Gateway, err.to_string()))
}

/// Budget HTTP mcp-core -> gateway per `/v1/complete`, dal punto unico
/// (`nexus_auth::llm_timeouts`, regola L).
///
/// Qui viveva un calcolo GEMELLO — `complete x max_attempts + cooldown_cap +
/// margine` = **435s** — costruito sull'assunzione che il gateway concedesse
/// 120s per tentativo. L'assunzione era falsa (il gateway ne concedeva 300, e il
/// 120 non era applicato da nessuno): il risultato era che mcp-core attendeva
/// una singola chiamata piu' a lungo (435s) di quanto vivesse l'intero run che
/// l'aveva chiesta (300s). Ora il budget e' DERIVATO dal run, non moltiplicato.
async fn resolve_client_timeout_secs(db: &sqlx::PgPool, run_timeout_secs: Option<u64>) -> u64 {
    nexus_auth::llm_timeouts::LlmTimeouts::resolve_for_run(db, run_timeout_secs)
        .await
        .client_budget
        .as_secs()
}

impl NexusGatewayClient {
    pub fn new(base_url: String, service_token: String) -> Self {
        let timeout_secs = nexus_auth::llm_timeouts::LlmTimeouts::defaults()
            .client_budget
            .as_secs();
        Self::with_timeout(base_url, service_token, timeout_secs, None)
    }

    fn with_timeout(
        base_url: String,
        service_token: String,
        timeout_secs: u64,
        run_timeout_secs: Option<u64>,
    ) -> Self {
        Self {
            run_timeout_secs,
            // Resilienza connessioni morte post-sleep (regola H): niente riuso di
            // socket idle dal pool + keepalive. Il default keep-alive faceva fallire
            // le chiamate mcp-core -> gateway con "error sending request" dopo che la
            // macchina si risvegliava dallo sleep (connessioni TCP morte riusate).
            // Vedi il gemello in nexus-gateway bootstrap (client gateway -> provider).
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(timeout_secs))
                .tcp_keepalive(std::time::Duration::from_secs(30))
                .pool_max_idle_per_host(0)
                .build()
                .expect("reqwest client"),
            base_url,
            service_token,
        }
    }

    /// Costruisce il client risolvendo la porta del gateway dal DB (regola G:
    /// niente porta hardcoded; il solo segreto del token resta in env, come in
    /// `main.rs`). PUNTO UNICO (regola L) del cablaggio gateway riusato da
    /// `build_native_deps` (run principale) e dall'orchestrazione sub-agente
    /// nativa (`agent_tools::subagent_native`): prima la sequenza
    /// `resolve_port -> format url -> NexusGatewayClient::new` era duplicata.
    pub async fn from_db(db: &sqlx::PgPool) -> Self {
        Self::from_db_for_run(db, None).await
    }

    /// Come [`from_db`], ma per un run di durata NOTA (il `timeout_s` della
    /// figura sub-agente).
    ///
    /// Il budget d'attesa nasceva sempre dal default globale di 300s anche
    /// quando il run che lo conteneva ne durava 240 (`review`): mcp-core poteva
    /// restare appeso a una singola chiamata oltre la vita del sub-run che
    /// l'aveva chiesta, ed e' il timeout che uccideva i review. Passando qui la
    /// durata reale, l'attesa del client resta dentro il cronometro della figura.
    pub async fn from_db_for_run(db: &sqlx::PgPool, run_timeout_secs: Option<u64>) -> Self {
        let gw_port = nexus_auth::resolve_port(db, "nexus_gateway_port").await;
        let gw_url = format!("http://127.0.0.1:{gw_port}");
        let gw_token = std::env::var("NEXUS_GATEWAY_SERVICE_TOKEN")
            .unwrap_or_else(|_| "dev-internal-token".to_string());
        let timeout_secs = resolve_client_timeout_secs(db, run_timeout_secs).await;
        Self::with_timeout(
            gw_url,
            gw_token,
            timeout_secs,
            nexus_auth::llm_timeouts::run_secs_utile(run_timeout_secs),
        )
    }

    /// Timbra sulla richiesta il run per cui il client e' stato costruito.
    ///
    /// Sta QUI e non nei costruttori di [`GwRequest`] perche' chi compone la
    /// richiesta e' una funzione pura che il run non lo conosce: lo conosce il
    /// client, che e' stato creato per quel sub-run. Stesso precedente del pin
    /// provider applicato in `agent_graph_adapter::llm_gateway`.
    fn body_for(&self, mut req: GwRequest) -> GwRequest {
        req.run_timeout_secs = self.run_timeout_secs;
        req
    }

    pub async fn complete(&self, req: GwRequest) -> Result<GwResponse> {
        let resp = self
            .http
            .post(format!("{}/v1/complete", self.base_url))
            .header("Authorization", format!("Bearer {}", self.service_token))
            .json(&self.body_for(req))
            .send()
            .await
            .map_err(|e| anyhow::Error::new(GatewayTransportError::from_reqwest(&e, &self.base_url)))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            // Errore TIPIZZATO (regola M): il punto di costruzione — che conosce
            // il contratto del body del gateway — estrae `code` e `details`; il
            // punto di decisione (adapter agent-graph) fa downcast, non parsing
            // della stringa. Display identico al vecchio bail! per i log.
            return Err(GatewayHttpError::from_response(status, body).into());
        }

        resp.json::<GwResponse>()
            .await
            .map_err(|e| anyhow::anyhow!("Nexus Gateway response parse: {e}"))
    }

    pub async fn is_healthy(&self) -> bool {
        self.http
            .get(format!("{}/health", self.base_url))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Autodiscovery dei modelli live di un provider via gateway
    /// (`GET /v1/models/{provider}`). Il gateway e' la via UNICA per la
    /// discovery: incapsula l'auth di ogni provider (incluso Vertex con
    /// Service Account), cosi' il worker catalog non deve replicare le
    /// chiamate dirette ne' delegare al brain per Google (regola L).
    /// Ritorna id + finestra di contesto dichiarata dal provider (dal campo
    /// additivo `models_meta`; gateway senza il campo -> finestre `None`,
    /// retro-compatibile).
    pub async fn list_models(&self, provider: &str) -> Result<Vec<GwModelMeta>> {
        let resp = self
            .http
            .get(format!("{}/v1/models/{provider}", self.base_url))
            .header("Authorization", format!("Bearer {}", self.service_token))
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| anyhow::Error::new(GatewayTransportError::from_reqwest(&e, &self.base_url)))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Nexus Gateway {status}: {body}");
        }

        resp.json::<GwModelsResponse>()
            .await
            .map(GwModelsResponse::into_metas)
            .map_err(|e| anyhow::anyhow!("Nexus Gateway models parse: {e}"))
    }
}

/// Metadati di un modello dal gateway (`models_meta` di
/// `GET /v1/models/{provider}`): id + finestra dichiarata dal provider.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GwModelMeta {
    pub id: String,
    /// Finestra di contesto in token dichiarata dal provider; `None` se l'API
    /// del provider non la espone (il catalogo scrive 0 = ignota, regola H).
    #[serde(default)]
    pub context_window: Option<i64>,
}

/// Risposta di `GET /v1/models/{provider}` del gateway.
#[derive(Deserialize, Debug)]
struct GwModelsResponse {
    #[allow(dead_code)]
    provider: String,
    models: Vec<String>,
    /// Campo additivo del gateway aggiornato; assente su gateway vecchio.
    #[serde(default)]
    models_meta: Vec<GwModelMeta>,
}

impl GwModelsResponse {
    /// Proietta la risposta in metadati: usa `models_meta` quando presente,
    /// altrimenti degrada agli id di `models` con finestra ignota (`None`).
    fn into_metas(self) -> Vec<GwModelMeta> {
        if !self.models_meta.is_empty() {
            return self.models_meta;
        }
        self.models
            .into_iter()
            .map(|id| GwModelMeta {
                id,
                context_window: None,
            })
            .collect()
    }
}

/// Mappa intent + behavior_mode all'alias definito in config/model-aliases.yaml.
pub fn intent_to_alias(intent: &str, behavior_mode: &str, forced_model: Option<&str>) -> String {
    if let Some(m) = forced_model {
        return m.to_string();
    }
    match (intent, behavior_mode) {
        ("architecture" | "design", _) => "reasoning-heavy",
        ("fix" | "refactor", _) => "coder-large",
        ("test" | "docs", "approfondita") => "coder-large",
        ("test" | "docs", _) => "coder-small",
        (_, "approfondita" | "dinamico") => "coder-large",
        _ => "coder-small",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_models_response_per_provider() {
        // Forma di GET /v1/models/{provider}: {"provider":"...","models":[...]}.
        // Gateway VECCHIO senza models_meta: degrada agli id con finestra None.
        let raw = r#"{"provider":"openai","models":["gpt-4o","gpt-4o-mini","o3"]}"#;
        let parsed: GwModelsResponse = serde_json::from_str(raw).expect("parse models response");
        assert_eq!(parsed.provider, "openai");
        assert_eq!(parsed.models, vec!["gpt-4o", "gpt-4o-mini", "o3"]);
        let metas = parsed.into_metas();
        assert_eq!(metas.len(), 3);
        assert!(metas.iter().all(|m| m.context_window.is_none()));
    }

    #[test]
    fn parse_models_meta_con_finestra_dichiarata() {
        // Gateway aggiornato: models_meta porta la finestra dichiarata dal
        // provider (Mistral max_context_length); id senza finestra -> None.
        let raw = r#"{"provider":"mistral",
            "models":["mistral-medium-3","mistral-ocr-latest"],
            "models_meta":[
                {"id":"mistral-medium-3","context_window":131072},
                {"id":"mistral-ocr-latest"}
            ]}"#;
        let parsed: GwModelsResponse = serde_json::from_str(raw).expect("parse meta");
        let metas = parsed.into_metas();
        assert_eq!(metas.len(), 2);
        assert_eq!(
            metas[0],
            GwModelMeta {
                id: "mistral-medium-3".into(),
                context_window: Some(131072)
            }
        );
        assert_eq!(metas[1].context_window, None);
    }

    #[test]
    fn parse_models_response_lista_vuota() {
        // Provider configurato ma senza modelli: lista vuota valida (il worker
        // tratta poi la lista vuota come skip-per-safety).
        let raw = r#"{"provider":"deepseek","models":[]}"#;
        let parsed: GwModelsResponse = serde_json::from_str(raw).expect("parse empty models");
        assert_eq!(parsed.provider, "deepseek");
        assert!(parsed.models.is_empty());
    }

    #[test]
    fn parse_models_response_google_via_gateway() {
        // Google passa per il gateway come tutti gli altri (auth Vertex inclusa
        // nel gateway): nessuna forma speciale lato mcp-core.
        let raw = r#"{"provider":"google","models":["gemini-2.5-pro","gemini-2.5-flash"]}"#;
        let parsed: GwModelsResponse = serde_json::from_str(raw).expect("parse google models");
        assert_eq!(parsed.models.len(), 2);
        assert!(parsed.models.contains(&"gemini-2.5-pro".to_string()));
    }

    /// La regressione, in numeri: il client attendeva una singola chiamata
    /// (435s) piu' a lungo del run che la conteneva (300s). Ora il budget e'
    /// derivato dal run e gli resta sotto, sempre.
    #[test]
    fn il_client_non_attende_piu_del_run_che_lo_contiene() {
        let t = nexus_auth::llm_timeouts::LlmTimeouts::defaults();
        assert_eq!(t.client_budget.as_secs(), 90, "era 435 (120*3+45+30)");
        assert!(
            t.client_budget < t.run_timeout,
            "il budget di UNA chiamata non puo' superare il run intero"
        );
    }

    /// Prova che il fix AGGIUNGE informazione: il `Display` di reqwest da solo
    /// non dice cosa sia andato storto ("error sending request for url (...)"),
    /// la catena si'. Usa una porta chiusa: errore di trasporto vero, nessuna
    /// dipendenza da servizi esterni.
    #[tokio::test]
    async fn transport_error_detail_mostra_la_causa_non_solo_il_display() {
        let c = reqwest::Client::new();
        let e = c
            .post("http://127.0.0.1:59999/v1/complete")
            .body("{}")
            .send()
            .await
            .expect_err("porta chiusa: deve fallire");

        let display_solo = e.to_string();
        let dettaglio = transport_error_detail(&e);

        assert!(
            dettaglio.contains("kind="),
            "manca il segnale strutturato: {dettaglio}"
        );
        assert!(
            dettaglio.contains("<-"),
            "manca la catena delle cause: {dettaglio}"
        );
        // Il punto: il dettaglio dice di piu' del Display, che e' cio' che
        // loggavamo prima e che non bastava a diagnosticare nulla.
        assert!(
            dettaglio.len() > display_solo.len(),
            "il dettaglio non aggiunge nulla al Display: {dettaglio}"
        );
    }

    /// LA REGRESSIONE, dal produttore vero (regola O): un fallimento reqwest
    /// REALE verso una porta chiusa, non un errore fabbricato a mano.
    ///
    /// E' il testo che l'utente ha visto in chat. Il test non asserisce una
    /// stringa intermedia ma la CONSEGUENZA: cosa esce dal `Display`, cioe' cio'
    /// che ogni `format!("{e}")` a valle produrra'.
    ///
    /// Mutazione che rende rosso: far tornare `Display` a stampare
    /// `self.facts.detail`, o ricostruire l'errore con `anyhow!("...{}",
    /// transport_error_detail(&e))`.
    #[tokio::test]
    async fn l_errore_di_trasporto_si_presenta_da_solo_in_modo_leggibile() {
        let c = reqwest::Client::new();
        let e = c
            .post("http://127.0.0.1:59999/v1/complete")
            .body("{}")
            .send()
            .await
            .expect_err("porta chiusa: deve fallire");

        let err = GatewayTransportError::from_reqwest(&e, "127.0.0.1:59999");
        let mostrato = err.to_string();

        assert!(
            !mostrato.contains("os_error") && !mostrato.contains("kind="),
            "gergo di sistema nel messaggio mostrato: {mostrato}"
        );
        assert!(
            !mostrato.contains("error sending request"),
            "il Display di reqwest e' arrivato all'utente: {mostrato}"
        );
        assert!(
            mostrato.contains("127.0.0.1:59999"),
            "il messaggio non dice CHI non risponde: {mostrato}"
        );

        // Il segnale diagnostico non si perde: cambia canale.
        let facts = err.error_facts();
        assert!(
            facts.detail.contains("kind=") && facts.detail.contains("<-"),
            "la catena diagnostica deve restare INTATTA nel detail: {}",
            facts.detail
        );
        let t = facts.transport.expect("i fatti di trasporto");
        assert!(t.is_connect, "porta chiusa: reqwest lo dichiara con is_connect");
        assert!(
            t.os_error.is_some(),
            "il codice OS e' il segnale piu' specifico e va estratto"
        );
    }

    /// La frase del gateway ATTRAVERSA il confine HTTP.
    ///
    /// Il body non e' scritto a mano: lo compone `write_into`, cioe' lo stesso
    /// produttore che lo scrive nel gateway (regola O). Se un giorno le chiavi
    /// cambiassero nome, questo test non resterebbe verde per costruzione.
    ///
    /// La CONSEGUENZA che si asserisce: da questo lato del confine i fatti del
    /// fornitore non esistono piu' (nessun provider, nessun modello, nessuno
    /// status del provider), quindi senza trasporto il messaggio sarebbe il
    /// generico "il servizio AI interno ha risposto con un errore" — vero e
    /// inutile.
    #[test]
    fn la_frase_del_gateway_arriva_intera_dopo_il_confine_http() {
        let reso = nexus_types::error_presentation::render_user_error(
            &ErrorFacts::opaque(
                ErrorDomain::Provider,
                "tutti i provider hanno fallito -> mistral (mistral HTTP 429: {\"error\":{}})",
            )
            .with_provider("mistral")
            .with_status(429)
            .with_code("rate_limit_exceeded"),
        );
        let mut body = serde_json::json!({
            "error": "tutti i provider hanno fallito -> mistral (...)",
            "code": "PROVIDER_ERROR",
            "details": { "primary_cause": "transient" },
        });
        reso.write_into(&mut body);

        let err: anyhow::Error =
            GatewayHttpError::from_response(reqwest::StatusCode::INTERNAL_SERVER_ERROR, body.to_string())
                .into();
        let rendered = rendered_from_error(&err);

        assert!(
            rendered.message.contains("mistral"),
            "la frase del gateway non e' stata trasportata: {}",
            rendered.message
        );
        assert!(
            !rendered.message.contains('{'),
            "il body grezzo e' rientrato nella frase: {}",
            rendered.message
        );
        // Il codice del gateway VINCE: da qui ogni errore sarebbe indistinguibile
        // ("gateway_all_providers_failed") e il frontend non potrebbe proporre
        // l'azione giusta.
        assert_eq!(rendered.code, "provider_rate_limited");
        assert!(
            rendered.detail.contains("HTTP 429"),
            "il tecnico integrale non deve perdersi: {}",
            rendered.detail
        );
    }

    /// Gateway che NON porta la resa (versione vecchia, o errore emesso da un
    /// path non ancora migrato): nessuna frase inventata, nessuna deduzione dal
    /// testo. Il messaggio resta generico e il body integrale finisce in detail.
    #[test]
    fn senza_resa_trasportata_si_ripiega_onestamente() {
        let body = r#"{"error":"boom {\"raw\":1}","code":"PROVIDER_ERROR"}"#;
        let err: anyhow::Error = GatewayHttpError::from_response(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            body.to_string(),
        )
        .into();
        let rendered = rendered_from_error(&err);
        assert_eq!(rendered.code, "gateway_all_providers_failed");
        assert!(
            !rendered.message.contains('{'),
            "il blob non deve mai entrare nella frase: {}",
            rendered.message
        );
        assert!(rendered.detail.contains("boom"), "il tecnico resta intero");
    }
}
