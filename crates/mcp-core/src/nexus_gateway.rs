use anyhow::Result;
use nexus_types::error_presentation::{
    render_user_error, ErrorDomain, ErrorFacts, HasErrorFacts, RenderedError, TransportFacts,
};
use nexus_types::provider_failure::EsclusioneDichiarata;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// HTTP client per il Nexus LLM Gateway (porta 4060).
#[derive(Clone)]
pub struct NexusGatewayClient {
    http: reqwest::Client,
    base_url: String,
    /// Pool su cui coniare il bearer di servizio a ogni richiesta.
    ///
    /// Prima qui c'era un `service_token: String` STATICO, letto da un'env che
    /// non era impostata da nessuna parte e quindi sempre pari alla costante
    /// `"dev-internal-token"` scritta nel sorgente — che il gateway accettava
    /// come bypass dell'autenticazione. Ora il token e' un JWT a vita breve
    /// firmato con la chiave di piattaforma: va coniato al momento dell'uso,
    /// non conservato, perche' scade.
    db: sqlx::PgPool,
    /// Il run per cui questo client e' stato costruito, se noto: viene timbrato
    /// su ogni richiesta cosi' il gateway dimensiona i propri budget sullo stesso
    /// cronometro del chiamante.
    run_timeout_secs: Option<u64>,
    /// IDENTITA' del run, quando il costruttore la conosce. Serve a una sola
    /// domanda: se questo run scade, quanto del suo budget se n'e' andato in
    /// CODA verso un fornitore saturo invece che in attesa del modello
    /// (`provider_inflight`). `None` = attesa non attribuibile a nessun run, che
    /// e' il caso legittimo delle chiamate senza budget proprio (classificatore,
    /// wizard, discovery) e non un ripiego.
    run_id: Option<uuid::Uuid>,
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
    /// Esito DICHIARATO del tool su un messaggio `role="tool"` (regola Q): il
    /// tool ha fallito (`Some(true)`), ha fatto cio' che doveva (`Some(false)`),
    /// oppure nessuno lo ha dichiarato (`None`). Allineato a
    /// `LlmMessage::is_error` del server (`nexus-gateway::types`), che lo
    /// traduce nel campo nativo del dialetto dove esiste (Anthropic `is_error`
    /// sul blocco `tool_result`) e ne dichiara il degrado dove non esiste.
    ///
    /// E' l'ultimo anello del canale strutturato dell'esito: il primo
    /// consumatore di quell'esito e' il MODELLO, e prima di questo campo gli
    /// arrivava solo il testo. Finche' i tool scrivevano il marker `U+274C` in
    /// testa al risultato la dichiarazione passava comunque; per un tool
    /// migrato a `RispostaTool` il marker non c'e' piu'. Omesso quando `None`
    /// (qualunque ruolo != tool, o esito non dichiarato).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
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

/// Usage come lo riporta il gateway sul wire: `input_tokens` e' il prompt LORDO
/// (il contesto inviato, cache compresa) e i due campi di cache ne sono il
/// DETTAGLIO. Il gateway normalizza a questa convenzione prima di rispondere,
/// qualunque sia il formato del provider
/// (`nexus_gateway::LlmUsage::normalized`).
#[derive(Deserialize, Debug, Clone, Default)]
pub struct GwUsage {
    /// Token di prompt LORDI: comprendono i due conteggi di cache qui sotto.
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Token serviti da prompt cache (Anthropic `cache_read_input_tokens`):
    /// sottoinsieme di `input_tokens`, con la sua tariffa. `None` se il provider
    /// non li riporta.
    #[serde(default)]
    pub cache_read_tokens: Option<u32>,
    /// Token scritti in cache (creazione voce). Vedi sopra.
    #[serde(default)]
    pub cache_creation_tokens: Option<u32>,
    /// Token di ragionamento che il provider tiene FUORI da `output_tokens`
    /// (oggi il solo Google: `candidatesTokenCount` porta il testo visibile,
    /// `thoughtsTokenCount` viaggia a parte). A differenza dei due campi di
    /// cache, che sono un dettaglio del prompt, questo e' un ADDENDO
    /// dell'output: chi paga somma dal punto unico
    /// `nexus_types::token_usage::completion_tokens_billable`.
    ///
    /// `None` sui provider che lo contano gia' dentro (Anthropic, dialetto
    /// OpenAI) e sui turni senza ragionamento.
    #[serde(default)]
    pub reasoning_tokens: Option<u32>,
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
    /// Cosa ha fatto il GATEWAY della contabilita' di questa chiamata
    /// (`nexus_gateway::types::LlmResponse::ledger` sull'altro lato del wire).
    ///
    /// `Written` e' il permesso strutturato di NON addebitare una seconda volta:
    /// chi ha prenotato (`billing::reserve_usage`) rilascia invece di
    /// finalizzare, e l'addebito resta uno solo. `NoIdentity` e `WriteFailed`
    /// sono "non ho scritto" detti con precisione, e obbligano a finalizzare.
    ///
    /// `None` NON significa "non ho scritto": significa che il gateway non ha
    /// dichiarato nulla, cioe' che non parla questa versione del contratto. Su
    /// una chiamata partita con identita' contabile valida e' un sospetto — quel
    /// gateway la riga potrebbe averla scritta lo stesso — e chi legge lo scopre
    /// da `nexus_ledger::Declaration::audit`, non da qui. Il punto unico che
    /// decide chi addebita resta `billing::settle_usage` (regola L).
    #[serde(default)]
    pub ledger: Option<GwLedgerOutcome>,
}

/// Vocabolario contabile del wire, dal punto unico (`nexus-ledger`).
///
/// E' lo STESSO tipo che il gateway serializza, non uno specchio: entrambi i
/// lati del wire lo importano da li'. Finche' erano struct gemelle — una per
/// lato, "stessi nomi di campo" per convenzione — nessun compilatore le teneva
/// allineate, e il segnale su cui si decide chi addebita sarebbe silenziosamente
/// diventato illeggibile al primo campo rinominato da una parte sola.
///
/// Il tipo condiviso non basta pero' a tenere allineati i due CONTENITORI
/// (`LlmResponse` di la', `GwResponse` di qua): quelli restano due struct
/// specchiate a mano, e a tenerle allineate e' il test di confine in fondo a
/// questo file, che serializza la risposta col produttore vero e la rilegge di
/// qua.
pub use nexus_ledger::LedgerOutcome as GwLedgerOutcome;

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

/// L'esclusione che il gateway ha dichiarato in QUESTA risposta d'errore.
///
/// Ponte fra il tipo dell'errore (che conosce solo questo modulo) e il criterio
/// puro, che vive nel crate del contratto insieme al vocabolario delle classi:
/// cosi' il json prodotto dal gateway e' letto dalla stessa funzione che i test
/// del produttore attraversano, invece che da una copia scritta qui (regola O).
///
/// Il gateway che non parla questa versione del contratto — nessun `details`,
/// o `failures` senza il residuo — produce [`EsclusioneDichiarata::Nessuna`]:
/// un'assenza non autorizza a inventare per quanto tempo escludere qualcuno.
pub(crate) fn esclusione_dichiarata(err: &GatewayHttpError) -> EsclusioneDichiarata {
    EsclusioneDichiarata::dal_blocco_details(err.details.as_ref())
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
///
/// `pub(crate)`: e' il punto unico dei fatti di trasporto reqwest per tutto
/// mcp-core (regola L) — `task_watchdog::probe_qdrant`/`probe_gateway` lo
/// riusano per classificare i probe invece di ricopiare la scansione di
/// `source()`.
pub(crate) fn transport_facts(e: &reqwest::Error, target: &str) -> TransportFacts {
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
    // Una resa GIA' FATTA non si ri-deriva: chi l'ha prodotta aveva i fatti
    // ancora vivi, ed eventualmente sapeva cose che qui non esistono piu' (che
    // il provider era pinnato dall'utente, per esempio). Senza questo ramo
    // ri-passerebbe dal `to_string()`, cioe' dal solo `message`, e i rami sotto
    // — che cercano tipi ormai assenti dalla catena — la degraderebbero al
    // generico "il servizio AI interno ha risposto con un errore".
    if let Some(r) = err.chain().find_map(|c| c.downcast_ref::<RenderedError>()) {
        return r.clone();
    }
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
    /// Bearer di servizio per questa richiesta: JWT a vita breve firmato con la
    /// chiave di piattaforma. Il conio e' cachato in `nexus_auth` (TTL piu'
    /// corta della vita del token), quindi non firma a ogni chiamata.
    async fn bearer(&self) -> Result<String> {
        nexus_auth::service_bearer(&self.db).await
    }

    pub fn new(base_url: String, db: sqlx::PgPool) -> Self {
        let timeout_secs = nexus_auth::llm_timeouts::LlmTimeouts::defaults()
            .client_budget
            .as_secs();
        Self::with_timeout(base_url, db, timeout_secs, None)
    }

    fn with_timeout(
        base_url: String,
        db: sqlx::PgPool,
        timeout_secs: u64,
        run_timeout_secs: Option<u64>,
    ) -> Self {
        Self {
            run_timeout_secs,
            run_id: None,
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
            db,
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
        let timeout_secs = resolve_client_timeout_secs(db, run_timeout_secs).await;
        Self::with_timeout(
            gw_url,
            db.clone(),
            timeout_secs,
            nexus_auth::llm_timeouts::run_secs_utile(run_timeout_secs),
        )
    }

    /// Chiede al registro il permesso di occupare il fornitore, accodandosi se
    /// e' saturo. La guardia ritornata tiene la chiamata «in volo» finche' vive.
    ///
    /// `None` quando il fornitore NON e' dichiarato sulla richiesta
    /// (`pin_provider` assente): li' e' il gateway a scegliere la destinazione, e
    /// attribuire il carico a un fornitore indovinato dal nome del modello
    /// significherebbe contare le chiamate di qualcun altro. Meglio un carico
    /// sottostimato e vero che uno completo e inventato — e' la stessa ragione
    /// per cui `attesa_del_run` distingue «non misurata» da «zero».
    async fn governo_del_carico(
        &self,
        req: &GwRequest,
    ) -> Option<crate::provider_inflight::PermessoChiamata> {
        let provider = req.pin_provider.as_deref()?.trim();
        if provider.is_empty() {
            return None;
        }
        let registro = crate::provider_inflight::registro_da_settings(&self.db).await;
        let (permesso, esito) = registro.permesso(provider, &req.model, self.run_id).await;
        match esito {
            crate::provider_inflight::EsitoAttesa::Immediato => {}
            crate::provider_inflight::EsitoAttesa::Atteso { attesa } => {
                tracing::debug!(
                    target: "provider_inflight",
                    provider = %provider, model = %req.model,
                    attesa_ms = attesa.as_millis() as u64,
                    "chiamata accodata: il fornitore era saturo, permesso ottenuto"
                );
            }
            crate::provider_inflight::EsitoAttesa::CodaScaduta { atteso, in_volo } => {
                // WARN e non errore: la chiamata parte comunque (tetto di
                // scheduling). E' il segnale che il fornitore e' sotto-dimensionato
                // per il carico corrente, cioe' esattamente il fatto che la sera
                // dell'08/08 nessuno poteva vedere.
                tracing::warn!(
                    target: "provider_inflight",
                    provider = %provider, model = %req.model,
                    atteso_s = atteso.as_secs(), in_volo,
                    "attesa in coda scaduta: la chiamata parte comunque sul fornitore saturo"
                );
            }
        }
        Some(permesso)
    }

    /// Dichiara di quale run e' il client, per l'attribuzione del tempo di coda.
    ///
    /// Fluente e non un parametro di [`from_db_for_run`] perche' i costruttori
    /// hanno gia' ~30 call site che il run non lo conoscono e non devono
    /// conoscerlo: chi lo sa lo dice, gli altri restano com'erano.
    pub fn per_run(mut self, run_id: uuid::Uuid) -> Self {
        self.run_id = Some(run_id);
        self
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
        // GOVERNO DEL CARICO (regola L, punto unico `provider_inflight`). Sta
        // QUI perche' questo e' il confine: ogni chiamata al modello di mcp-core
        // passa da `complete`, quindi il conteggio vede il carico VERO — le
        // figure di un fan-out, ma anche il classificatore, il wizard e le altre
        // sessioni, che verso il fornitore pesano esattamente quanto le figure.
        // Contarlo piu' in alto (nel fan-out) avrebbe misurato una convocazione,
        // non un fornitore.
        //
        // La guardia vive fino a fine funzione: dal momento in cui la richiesta
        // parte a quando la risposta e' letta. Se il task viene cancellato nel
        // mezzo — cioe' ogni volta che una figura scade — il `Drop` decrementa.
        let _carico = self.governo_del_carico(&req).await;
        let resp = self
            .http
            .post(format!("{}/v1/complete", self.base_url))
            .header("Authorization", format!("Bearer {}", self.bearer().await?))
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
            let errore = GatewayHttpError::from_response(status, body);
            // ALLINEAMENTO DEI DUE REGISTRI. Sta QUI, allo stesso confine del
            // governo del carico e per la stessa ragione: ogni chiamata al
            // modello di mcp-core passa da `complete`, quindi l'esclusione che
            // il gateway dichiara la impara TUTTO mcp-core — gate duale,
            // motore agentico, purpose interni, wizard, classificatore — e non
            // il solo percorso che la classificava per il failover.
            //
            // PORTATA dichiarata: restano fuori i due client che parlano lo
            // stesso contratto wire da crate a MONTE di questo
            // (`nexus-agent-tools::gateway_client` per i tool vision,
            // `nexus-types::gateway_client` per admin-service e i worker). Non
            // e' una lacuna dell'innesto: quel registro vive qui, e loro non
            // possono vederlo ne' lo interrogano — nessuno dei due passa dalla
            // selezione dei modelli che il registro governa.
            crate::provider_cooldown::registra_esclusione_dichiarata(
                &esclusione_dichiarata(&errore),
            );
            return Err(errore.into());
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
            .header("Authorization", format!("Bearer {}", self.bearer().await?))
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
    /// Tetto di output in token dichiarato dal provider nel listing
    /// (`outputTokenLimit`: oggi lo espone il solo Google); `None` se non
    /// dichiarato, oppure gateway anteriore al campo. MAI inventato a valle
    /// (regola G/H): la persistenza a catalogo/capability e' un passo
    /// successivo, qui il campo deve solo attraversare il wire.
    #[serde(default)]
    pub output_token_limit: Option<i64>,
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
                output_token_limit: None,
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
                context_window: Some(131072),
                output_token_limit: None,
            }
        );
        assert_eq!(metas[1].context_window, None);
    }

    #[test]
    fn il_tetto_di_output_dichiarato_attraversa_il_wire_dei_meta() {
        // Il corpo e' composto COME LO COMPONE LA ROTTA del gateway
        // (`models_for_provider`: json! con i ModelMeta serializzati), non
        // ricopiato a mano (regola O): se il campo cambiasse nome da un lato
        // solo, questo test rosseggia. MUTAZIONE: togliere `output_token_limit`
        // da GwModelMeta (o la lettura di `outputTokenLimit` nel parser google,
        // che e' l'unico produttore del valore) fa rosseggiare l'assert sul
        // tetto; un gateway VECCHIO senza campo degrada a None (secondo meta).
        let metas = vec![
            ::nexus_gateway::provider::ModelMeta {
                id: "gemini-2.5-flash".into(),
                context_window: Some(1_048_576),
                output_token_limit: Some(65_536),
            },
            ::nexus_gateway::provider::ModelMeta {
                id: "gemini-2.5-pro".into(),
                context_window: None,
                output_token_limit: None,
            },
        ];
        let ids: Vec<&str> = metas.iter().map(|m| m.id.as_str()).collect();
        let corpo =
            serde_json::json!({ "provider": "google", "models": ids, "models_meta": metas });
        let parsed: GwModelsResponse =
            serde_json::from_value(corpo).expect("parse del corpo composto dalla rotta");
        let letti = parsed.into_metas();
        assert_eq!(letti[0].output_token_limit, Some(65_536));
        assert_eq!(
            letti[1].output_token_limit, None,
            "non dichiarato resta ignoto: mai un tetto inventato (regola G/H)"
        );
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

// ── Il CONFINE: cio' che il gateway serializza, mcp-core lo legge ──────────
//
// `GwResponse` e' la struct specchiata A MANO di `nexus_gateway::types::LlmResponse`.
// I due processi vivono in due crate che non si vedono e si aggiornano in momenti
// diversi; a tenerli allineati non c'era ne' un tipo condiviso, ne' uno schema,
// ne' un test. Il campo su cui poggia l'intero fix del doppio addebito
// attraversa proprio quel confine: se un rename, un `rename_all` o un proxy che
// ri-serializza spostano la chiave, `settle` riceve `None` a ogni chiamata e il
// doppio addebito torna identico — silenzioso, verde, invisibile.
//
// Il test parte dal PRODUTTORE di produzione (`server::billing::record_and_declare`,
// la stessa funzione che chiama la pipeline HTTP), serializza con `serde_json`
// come fa axum e rilegge di qua con `GwResponse`. Costruire a mano la struct di
// arrivo non chiuderebbe niente: e' esattamente la forma che ha lasciato passare
// il difetto (regola O).
//
// Vive qui, e non in un crate terzo, perche' mcp-core e' bin-only: nessun altro
// puo' vedere `GwResponse`. La strada alternativa — un contratto condiviso, cioe'
// spostare anche i due CONTENITORI in un crate comune — tocca molta piu'
// struttura di quanta ne serva, e collide con il consolidamento del ledger in
// corso. Il tipo del CAMPO e' gia' condiviso (`nexus_ledger::LedgerOutcome`); qui
// si tiene il resto, che nessun tipo puo' tenere: il nome della chiave e la forma
// del contenitore.
#[cfg(test)]
mod confine_wire_tests {
    use super::*;
    // `::` esplicito: dentro questo file `nexus_gateway` e' anche il nome del
    // MODULO di mcp-core, e la confusione fra i due lati del wire e' proprio cio'
    // che questo test esiste per impedire.
    use ::nexus_gateway::server::billing::record_and_declare;
    use ::nexus_gateway::types::{
        LlmMessage, LlmRequest, LlmResponse, LlmUsage, MessageContent, PromptCacheReporting,
        ReasoningTokens, RequestMetadata,
    };
    use sqlx::PgPool;
    use uuid::Uuid;

    /// Listino con le quattro tariffe distinte (forma della mig 0403): serve a
    /// far nascere un costo NON nullo, cosi' l'importo dichiarato e' osservabile.
    async fn seed_listino(pool: &PgPool) {
        sqlx::query(
            "INSERT INTO ai_price_catalog ( \
                 provider, model, \
                 input_cost_per_million_tokens, output_cost_per_million_tokens, \
                 cache_read_cost_per_million_tokens, cache_creation_cost_per_million_tokens, \
                 currency, pricing_state \
             ) VALUES ('anthropic', 'claude-x', 3.0, 15.0, 0.3, 3.75, 'USD', 'priced')",
        )
        .execute(pool)
        .await
        .expect("seed ai_price_catalog");
    }

    /// La richiesta come la manda mcp-core: con l'identita' contabile nei
    /// metadata (`tenant_id` = progetto, `user_id` = utente).
    fn richiesta_con_identita(project: Uuid, user: Uuid, run: Uuid) -> LlmRequest {
        LlmRequest {
            model: "claude-x".into(),
            messages: vec![LlmMessage {
                role: "user".into(),
                content: MessageContent::Text("ciao".into()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
                reasoning: None,
                is_error: None,
            }],
            temperature: None,
            max_tokens: Some(64),
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: RequestMetadata {
                tenant_id: project.to_string(),
                user_id: user.to_string(),
                request_id: run.to_string(),
                sensitivity_tier: 0,
                feature: "chat".into(),
            },
            run_timeout_secs: None,
        }
    }

    /// La risposta come esce dal fallback dei provider: l'usage nasce dal suo
    /// produttore (`LlmUsage::normalized`), il campo contabile e' ancora vuoto —
    /// lo valorizza la pipeline, ed e' il passaggio sotto esame.
    fn risposta_dal_provider() -> LlmResponse {
        LlmResponse {
            content: "ok".into(),
            tool_calls: None,
            usage: LlmUsage::normalized(
                PromptCacheReporting::CachedIncludedInPrompt,
                1_000_000,
                400_000,
                None,
                None,
                ReasoningTokens::IncludedInOutput,
            ),
            model_used: "claude-x".into(),
            provider_used: "anthropic".into(),
            latency_ms: 7,
            finish_reason: "stop".into(),
            privacy_rerouted: None,
            reasoning: None,
            thinking_signature: None,
            citations: None,
            ledger: None,
        }
    }

    /// Il JSON che il gateway mette sul wire per questa risposta.
    ///
    /// E' `serde_json::to_string` sulla struct del gateway: la stessa cosa che fa
    /// `axum::Json` nel handler. Nessuna chiave scritta a mano.
    fn sul_wire(resp: &LlmResponse) -> String {
        serde_json::to_string(resp).expect("il gateway serializza la risposta")
    }

    /// La riga scritta dal gateway arriva a mcp-core, e ci arriva INTERA.
    ///
    /// Attraversa i due produttori veri: `record_and_declare` scrive nel ledger e
    /// dichiara sulla risposta (e' la riga di `routes.rs` che pubblica il
    /// segnale), poi la risposta viene serializzata come sul wire e riletta da
    /// `GwResponse`, che e' cio' che il client di mcp-core deserializza davvero.
    ///
    /// MUTAZIONE — la piu' importante di tutte, ed e' quella temuta: mettere
    /// `#[serde(rename = "ledger_entry")]` sul campo `ledger` di UNO dei due lati
    /// (o rinominarlo del tutto) fa fallire questo test con "il gateway ha
    /// dichiarato una riga: mcp-core deve vederla". Prima di oggi quel rename
    /// lasciava tutto verde e riportava il doppio addebito in produzione.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_riga_dichiarata_dal_gateway_sopravvive_al_wire(pool: PgPool) {
        let (user, project) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        seed_listino(&pool).await;
        let run = Uuid::new_v4();

        // Lato gateway: si scrive e si dichiara, con la funzione della pipeline.
        let mut resp = risposta_dal_provider();
        record_and_declare(&pool, &richiesta_con_identita(project, user, run), &mut resp).await;

        // Il wire, e la rilettura dal lato di mcp-core.
        let letta: GwResponse =
            serde_json::from_str(&sul_wire(&resp)).expect("mcp-core deve saper leggere la risposta");

        // 1. Il segnale ha attraversato il confine.
        let dichiarazione = letta
            .ledger
            .as_ref()
            .expect("il gateway ha dichiarato un esito: mcp-core deve vederlo");
        assert_eq!(dichiarazione.as_str(), "written");
        let letto = dichiarazione
            .entry()
            .expect("il gateway ha dichiarato una riga: mcp-core deve vederla");

        // 2. Ed e' arrivato INTERO. Un campo che si perde per strada e' un id
        //    nullo o un costo a zero: la correlazione punterebbe altrove e
        //    l'importo mostrato all'utente divergerebbe dal ledger.
        let scritta = resp.ledger.as_ref().and_then(|o| o.entry()).expect("scritta");
        assert_eq!(letto.id, scritta.id);
        assert_eq!(letto.currency, scritta.currency);
        assert!((letto.total_cost - scritta.total_cost).abs() < 1e-12);

        // 3. E la riga dichiarata e' quella che sta NEL DATABASE, non un numero
        //    che si e' propagato coerente fra due strutture entrambe sbagliate.
        let riga: (Uuid, f64, String) = sqlx::query_as(
            "SELECT id, total_cost::float8, currency FROM ai_usage_ledger WHERE run_id = $1",
        )
        .bind(run)
        .fetch_one(&pool)
        .await
        .expect("la riga scritta dal gateway");
        assert_eq!(letto.id, riga.0);
        assert!((letto.total_cost - riga.1).abs() < 1e-9);
        assert_eq!(letto.currency, riga.2);
        // 1M x 3.0 + 0.4M x 15.0 = 9.0: un costo vero, non uno zero che
        // combacerebbe con qualunque cosa.
        assert!((riga.1 - 9.0).abs() < 1e-9, "costo scritto {}", riga.1);

        // 4. E la decisione che ne consegue: con la riga dichiarata, chi ha
        //    prenotato NON deve finalizzare.
        let dichiarazione = nexus_ledger::Declaration::dal_wire(letta.ledger);
        assert!(dichiarazione.entry().is_some());
        assert_eq!(
            dichiarazione.audit(true),
            nexus_ledger::DeclarationAudit::Coerente
        );
    }

    /// Anche il "non ho scritto" attraversa il confine, e resta distinguibile dal
    /// silenzio.
    ///
    /// Sono i due casi che prima collassavano entrambi in `None`. Qui il gateway
    /// non scrive perche' la richiesta non porta identita' (`GwMetadata::default`,
    /// il percorso di `NeuralCoreClient`), e lo DICE: chi legge sa che nessuno ha
    /// addebitato e finalizza, senza doverlo indovinare.
    ///
    /// MUTAZIONE: facendo tacere la pipeline (`resp.ledger` lasciato a `None`
    /// invece che `Some(NoIdentity)`), il verdetto diventa `NonDichiarata` e
    /// l'asserzione sul codice fallisce — ed e' il verdetto giusto, perche' un
    /// gateway muto e' indistinguibile da uno di build vecchia che ha scritto.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_non_ho_scritto_attraversa_il_wire_e_non_e_silenzio(pool: PgPool) {
        seed_listino(&pool).await;

        let mut resp = risposta_dal_provider();
        let mut req = richiesta_con_identita(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        req.metadata.tenant_id = String::new();
        req.metadata.user_id = String::new();
        record_and_declare(&pool, &req, &mut resp).await;

        let letta: GwResponse =
            serde_json::from_str(&sul_wire(&resp)).expect("mcp-core deve saper leggere la risposta");
        let dichiarazione = nexus_ledger::Declaration::dal_wire(letta.ledger);
        assert_eq!(dichiarazione.as_str(), "no_identity");
        assert!(
            dichiarazione.entry().is_none(),
            "nessuna riga scritta: la prenotazione DEVE essere finalizzata"
        );

        // Senza identita' mandata, il "non ho scritto" e' la risposta giusta e
        // nessuno deve essere svegliato...
        assert_eq!(
            dichiarazione.audit(false),
            nexus_ledger::DeclarationAudit::Coerente
        );
        // ...ma la stessa frase su una chiamata partita CON identita' significa
        // che l'identita' si e' persa fra i due processi, e allora si', va detto.
        assert_eq!(
            dichiarazione.audit(true),
            nexus_ledger::DeclarationAudit::IdentitaPersa
        );

        // E il silenzio resta una TERZA cosa: un gateway che non dichiara nulla.
        let muta: GwResponse = serde_json::from_str(r#"{"content":"ok",
            "usage":{"input_tokens":1,"output_tokens":1},
            "model_used":"m","provider_used":"p","latency_ms":0,"finish_reason":"stop"}"#)
            .expect("risposta di un gateway che non parla questo contratto");
        assert!(muta.ledger.is_none());
        assert_eq!(
            nexus_ledger::Declaration::dal_wire(muta.ledger).audit(true),
            nexus_ledger::DeclarationAudit::NonDichiarata,
            "un gateway muto su una chiamata con identita' e' un sospetto di doppio addebito"
        );
    }

    /// L'INNESTO, provato dove la produzione lo attraversa (regola O): una
    /// chiamata vera di `complete` contro un gateway che risponde come il
    /// gateway vero, e il registro locale che DOPO esclude quel fornitore.
    ///
    /// Perche' non basta provare il criterio: quello puro ha gia' i suoi test
    /// in `nexus-types`, e resterebbero tutti verdi se `complete` non lo
    /// chiamasse mai — cioe' proprio la forma in cui questo difetto e' vissuto
    /// finora, col segnale che attraversava il confine e nessuno che ne
    /// traesse la conseguenza.
    ///
    /// MUTAZIONE che lo fa rosseggiare, col difetto reale: togliere la chiamata
    /// a `registra_esclusione_dichiarata` dal ramo d'errore di `complete` -> il
    /// fornitore resta eleggibile e la selezione lo riconvoca, per tutta la
    /// durata che il gateway sta onorando.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn complete_allinea_il_registro_locale_a_cio_che_il_gateway_rifiuta(db: sqlx::PgPool) {
        use nexus_types::provider_failure::{chiave, classe};

        // Nome irripetibile: il registro dei cooldown e' globale al processo e
        // i test girano in parallelo.
        let fornitore = "prova-innesto-complete";
        assert!(
            !crate::provider_cooldown::is_provider_in_cooldown(fornitore),
            "premessa: il fornitore parte disponibile"
        );

        // Il corpo che il gateway compone davvero, con le chiavi del CONTRATTO
        // (un rename le rompe qui a compile time, non in esercizio).
        let corpo = serde_json::json!({
            "code": "PROVIDER_ERROR",
            "message": "tutti i provider hanno fallito",
            "details": {
                chiave::PRIMARY_CAUSE: classe::COOLDOWN,
                chiave::FAILURES: [{
                    chiave::PROVIDER: fornitore,
                    chiave::MODELLO: "un-modello",
                    chiave::CLASSE: classe::COOLDOWN,
                    chiave::ATTESA_S: 900,
                }],
            },
        })
        .to_string();

        // Gateway finto: una risposta sola, poi chiude.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("porta effimera");
        let porta = listener.local_addr().expect("indirizzo").port();
        let finto = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut socket, _) = listener.accept().await.expect("connessione");
            let mut scarto = [0u8; 4096];
            let _ = socket.read(&mut scarto).await;
            // Il terminatore di riga di HTTP e' parte del protocollo, non dei
            // fine-riga di questo file: dichiarato come costante perche' un
            // normalizzatore d'albero non possa toccarlo.
            const CRLF: &str = "\r\n";
            let intestazioni = [
                "HTTP/1.1 500 Internal Server Error",
                "Content-Type: application/json",
                &format!("Content-Length: {}", corpo.len()),
                "Connection: close",
                "",
                "",
            ]
            .join(CRLF);
            let _ = socket.write_all(intestazioni.as_bytes()).await;
            let _ = socket.write_all(corpo.as_bytes()).await;
            let _ = socket.flush().await;
        });

        let client = NexusGatewayClient::new(format!("http://127.0.0.1:{porta}"), db);
        let esito = client
            .complete(GwRequest {
                model: "un-modello".into(),
                messages: Vec::new(),
                ..Default::default()
            })
            .await;
        assert!(esito.is_err(), "il gateway ha risposto 500");
        let _ = finto.await;

        assert!(
            crate::provider_cooldown::is_provider_in_cooldown(fornitore),
            "dopo il rifiuto dichiarato la selezione non deve piu' convocarlo: e' il              difetto misurato il 12/08/2026 sul gate duale"
        );

        crate::provider_cooldown::remove_cooldown(fornitore);
    }

    /// Un tetto TPD di UN modello esclude la COPPIA, e lascia in servizio il
    /// resto del fornitore — lungo la catena vera, dai segnali reali.
    ///
    /// IL DIFETTO. Fino al 13/08/2026 mcp-core teneva un SECONDO scrittore di
    /// esclusioni accanto a questo (`agent_turn_setup::apply_provider_cooldown`,
    /// alimentato da un secondo classificatore il cui vocabolario non conosceva
    /// `transient`), e la sua firma non aveva un parametro `model`: qualunque
    /// cosa scrivesse era del FORNITORE INTERO. Misurato nei log: alle
    /// 18:32:47.743443 il cooldown corretto sulla coppia
    /// `groq/openai/gpt-oss-20b`, e 439 microsecondi dopo lo STESSO evento
    /// riclassificato `billing_error` — la parola `billing` sta nell'URL di
    /// documentazione che groq mette nel messaggio — con groq intero spento.
    ///
    /// I SEGNALI SONO QUELLI VERI e la portata la deriva il PRODUTTORE
    /// (`PortataCooldown::da_segnale`, nel gateway): il test non decide da se'
    /// che un 429 con `rate_limit_exceeded` sia un tetto di modello — lo chiede
    /// al codice che comporra' quel campo in produzione (regola O). Ricopiare
    /// `"model"` a mano qui renderebbe verde il test anche se il gateway
    /// smettesse di dichiararlo.
    ///
    /// MUTAZIONE che lo fa rosseggiare, col difetto reale: rimettere il ripiego
    /// lessicale e il suo scrittore, cioe' aggiungere dopo la costruzione di
    /// `errore` una qualunque forma di
    /// `put_provider_in_long_cooldown(fornitore, ...)` /
    /// `put_provider_in_short_cooldown(fornitore, ...)` -> il secondo assert
    /// fallisce con «fornitore intero escluso per un limite di modello».
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn un_tetto_del_modello_non_spegne_il_fornitore(db: sqlx::PgPool) {
        use nexus_gateway::cooldown::PortataCooldown;
        use nexus_types::provider_failure::{chiave, classe};

        // Nome irripetibile: il registro dei cooldown e' globale al processo.
        let fornitore = "prova-tpd-groq";
        let modello_saturo = "openai/gpt-oss-20b";
        let modello_sano = "llama-3.3-70b-versatile";

        // I segnali REALI del 429 TPD di groq. Il messaggio porta la parola che
        // traeva in inganno il ripiego lessicale: senza, il test non misurerebbe
        // il difetto.
        let status = 429u16;
        let codice = "rate_limit_exceeded";
        let messaggio = "Rate limit reached for model `openai/gpt-oss-20b` in organization \
             org_01 service tier `on_demand` on tokens per day (TPD): Limit 200000, \
             Used 199512. Need more tokens? Upgrade at https://console.groq.com/settings/billing";
        let portata = PortataCooldown::da_segnale(Some(status), Some(codice));
        assert_eq!(
            portata,
            PortataCooldown::Modello,
            "premessa: e' il gateway a stabilire che un tetto di frequenza e' del modello"
        );

        let corpo = serde_json::json!({
            "code": "PROVIDER_ERROR",
            "message": format!("tutti i provider hanno fallito -> {fornitore} ({messaggio})"),
            "details": {
                chiave::PRIMARY_CAUSE: classe::TRANSIENT,
                chiave::FAILURES: [{
                    chiave::PROVIDER: fornitore,
                    chiave::MODELLO: modello_saturo,
                    chiave::CLASSE: classe::TRANSIENT,
                    chiave::PORTATA: portata.wire(),
                    chiave::ATTESA_S: 1424,
                }],
            },
        })
        .to_string();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("porta effimera");
        let porta = listener.local_addr().expect("indirizzo").port();
        let finto = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut socket, _) = listener.accept().await.expect("connessione");
            let mut scarto = [0u8; 4096];
            let _ = socket.read(&mut scarto).await;
            const CRLF: &str = "\r\n";
            let intestazioni = [
                "HTTP/1.1 500 Internal Server Error",
                "Content-Type: application/json",
                &format!("Content-Length: {}", corpo.len()),
                "Connection: close",
                "",
                "",
            ]
            .join(CRLF);
            let _ = socket.write_all(intestazioni.as_bytes()).await;
            let _ = socket.write_all(corpo.as_bytes()).await;
            let _ = socket.flush().await;
        });

        let client = NexusGatewayClient::new(format!("http://127.0.0.1:{porta}"), db);
        let esito = client
            .complete(GwRequest {
                model: modello_saturo.into(),
                messages: Vec::new(),
                ..Default::default()
            })
            .await;
        assert!(esito.is_err(), "il gateway ha risposto 500");
        let _ = finto.await;

        assert!(
            crate::provider_cooldown::is_model_in_cooldown(fornitore, modello_saturo),
            "la coppia che ha sforato il tetto deve uscire dalla selezione"
        );
        assert!(
            !crate::provider_cooldown::is_provider_in_cooldown(fornitore),
            "fornitore intero escluso per un limite di modello: e' il difetto del \
             13/08/2026, un secondo scrittore senza portata che spegne l'account"
        );
        assert!(
            !crate::provider_cooldown::is_model_in_cooldown(fornitore, modello_sano),
            "gli altri modelli dello stesso fornitore hanno quota propria"
        );

        crate::provider_cooldown::remove_cooldown(fornitore);
    }
}
