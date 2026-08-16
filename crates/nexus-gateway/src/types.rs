//! Tipi del contratto LLM del gateway.
//!
//! Fedeli a `packages/shared/src/llm-types.ts` (lingua franca: OpenAI Chat
//! Completions). Il client esistente in `crates/mcp-core/src/nexus_gateway.rs`
//! usa una versione ridotta (`GwRequest`/`GwResponse`); qui modelliamo il
//! contratto COMPLETO che il server deve deserializzare. Alla Fase 6 il client
//! mcp-core verra' allineato a riusare questi tipi (regola L: punto unico).

use serde::{Deserialize, Serialize};

/// Tier di sensibilita' del dato (0 = pubblico ... 3 = massimo riservato).
pub type SensitivityTier = u8;

/// Blocco di contenuto strutturato di un messaggio (testo, immagine, risultato
/// di tool). Corrisponde a `LLMContentBlock`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmContentBlock {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// Chiamata a tool emessa dal modello (`LLMToolCall`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunctionCall,
    /// Firma opaca di reasoning (`thoughtSignature`) che Gemini 3 emette PER
    /// OGNI `functionCall` e IMPONE di ri-passare sulla rispettiva part nei
    /// turni con tool, altrimenti HTTP 400 INVALID_ARGUMENT ("Function call is
    /// missing a thought_signature in functionCall parts"). A differenza di
    /// Anthropic (una firma per blocco thinking, a livello di messaggio via
    /// [`LlmMessage::thinking_signature`]) qui la firma e' PER-CALL.
    /// Retrocompatibile: assente/`None` per tutti gli altri provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolFunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Definizione di un tool offerto al modello (`LLMToolDefinition`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmToolDefinition {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunctionDef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunctionDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// Contenuto di un messaggio: stringa semplice oppure lista di blocchi.
/// Modella `string | LLMContentBlock[]` con un enum untagged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<LlmContentBlock>),
}

/// Messaggio della conversazione (`LLMMessage`).
///
/// `PartialEq` non e' decorativo: e' il segnale su cui il gateway decide se un
/// retry ha senso. Due history uguali producono la stessa richiesta e quindi lo
/// stesso rifiuto — confrontarle e' l'unico modo di saperlo senza spendere la
/// chiamata (vedi `history_sanitizer::retry_changes_history`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String,
    pub content: MessageContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<LlmToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Firma opaca del blocco `thinking` di un turno assistant precedente
    /// (extended thinking Anthropic). Quando presente su un messaggio
    /// `assistant`, il provider la re-include come block `thinking` con
    /// `signature` nei turni con tool (l'API Anthropic la richiede, altrimenti
    /// HTTP 400). Retrocompatibile: assente/`None` per tutti gli altri provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    /// Testo del ragionamento (`reasoning_content`) di un turno assistant
    /// precedente generato in thinking mode da DeepSeek. Vincolo analogo al
    /// `thinking_signature` Anthropic: l'API DeepSeek IMPONE che, per gli
    /// assistant message prodotti in thinking mode, il `reasoning_content` venga
    /// RI-PASSATO nelle richieste successive, altrimenti HTTP 400 ("The
    /// reasoning_content in the thinking mode must be passed back to the API").
    /// Il chiamante lo rispedisce da [`LlmResponse::reasoning`] del turno
    /// precedente; il provider OpenAI-compat lo re-include nel wire SOLO per il
    /// dialetto DeepSeek (vedi `build_request_body`). Retrocompatibile:
    /// assente/`None` per tutti gli altri provider, che non vedono il campo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Esito DICHIARATO del tool su un messaggio `role="tool"` (regola Q): il
    /// tool ha fallito (`Some(true)`), ha fatto cio' che doveva (`Some(false)`),
    /// oppure nessuno lo ha dichiarato (`None`).
    ///
    /// I casi sono TRE e il tipo lo dice: `None` non e' "e' andata bene", e'
    /// "non lo so" — un messaggio tool ricostruito dal sanitizer, o inviato da
    /// un chiamante che non parla questa versione del contratto, non porta alcun
    /// esito e non deve fingerne uno.
    ///
    /// PERCHE' ARRIVA FIN QUI: il primo consumatore dell'esito di un tool e' il
    /// MODELLO, e sul wire quell'esito non aveva un campo. Finche' i tool
    /// scrivevano il marker `U+274C` in testa al testo il modello lo riceveva
    /// comunque; per ogni tool migrato a `RispostaTool` quel marker non c'e'
    /// piu', e senza questo campo un fallimento arriva al modello come un
    /// tool_result indistinguibile da uno riuscito.
    ///
    /// COSA NE FA CIASCUN DIALETTO (nessuno finge un campo che non ha):
    /// - Anthropic ha `is_error` sul blocco `tool_result` e lo emette nativo;
    /// - OpenAI-compat e Google NON hanno un campo equivalente sul messaggio
    ///   tool: il degrado e' dichiarato e reso in un punto solo da
    ///   [`crate::providers::tool_error_channel`], che compone il TESTO dal
    ///   campo al confine (regola Q punto 3) invece di lasciare muto l'esito.
    ///
    /// Retrocompatibile: `#[serde(default)]` + omesso in serializzazione, quindi
    /// una richiesta che non lo porta resta valida e vale `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// Metadati di tracciamento e tenancy della richiesta (`RequestMetadata`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMetadata {
    pub tenant_id: String,
    pub user_id: String,
    pub request_id: String,
    #[serde(default)]
    pub sensitivity_tier: SensitivityTier,
    pub feature: String,
}

/// Richiesta di completion (`LLMRequest`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    pub model: String,
    pub messages: Vec<LlmMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<LlmToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Configurazione extended thinking richiesta dal chiamante. Quando
    /// `enabled` e' true il provider (oggi solo Anthropic) attiva la modalita'
    /// thinking. Retrocompatibile: `None` = nessuna richiesta di thinking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    /// Vincolo di scelta del tool, in stile OpenAI Chat Completions (lingua
    /// franca del gateway). Governa quanto il modello e' OBBLIGATO a chiamare un
    /// tool: il brain lo imposta a `"required"` quando il force-action anti-loop
    /// / `progress_controller` devono costringere l'agente ad AGIRE invece di
    /// descrivere. Formati accettati (identici all'API OpenAI):
    ///   - stringa `"auto"`   -> il modello sceglie se chiamare un tool;
    ///   - stringa `"required"` -> il modello DEVE chiamare almeno un tool;
    ///   - stringa `"none"`   -> il modello NON deve chiamare tool;
    ///   - oggetto `{"type":"function","function":{"name":"X"}}` -> forza il tool `X`.
    /// Ogni provider lo rimappa al proprio dialetto nel rispettivo
    /// `build_request_body` (OpenAI-compat passthrough nativo; Anthropic
    /// `tool_choice` con `{type:any|tool|auto}`; Google
    /// `tool_config.function_calling_config.mode`). Retrocompatibile: `None` =
    /// nessun vincolo inviato (comportamento storico, equivalente ad `auto`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// Pin esplicito del provider da eseguire (bypass routing). Quando `Some`,
    /// il gateway esegue ESATTAMENTE quel provider col `model` indicato
    /// (strippato dell'eventuale prefisso `provider/`), SENZA `policy.decide` e
    /// SENZA fallback cross-provider: se il provider e' in cooldown o non e'
    /// configurato, la richiesta fallisce (nessun ripiego su un altro provider).
    /// Serve al chiamante (mcp-core) che ha gia' deciso provider+modello via
    /// routing matrix DB, per evitare un secondo routing divergente nel gateway.
    /// Retrocompatibile: `None` = routing per tier + fallback (comportamento
    /// storico invariato).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_provider: Option<String>,
    /// Durata del RUN che ha originato questa richiesta, in secondi.
    ///
    /// Il gateway deriva i propri budget (`request_budget`, `per_attempt`) dal
    /// run: `budget = run / min_turns`. Ma li derivava una volta sola all'avvio,
    /// dal default globale `orchestrator.subagent_default_timeout_s` (300s),
    /// mentre il run vero e' PER FIGURA (`nexus_subagent_definitions.timeout_s`:
    /// `review` 240, `implement` 600). Una figura da 240s riceveva tentativi
    /// dimensionati su 300: il gateway prometteva turni che il cronometro del
    /// chiamante non poteva mantenere.
    ///
    /// Il chiamante e' l'unico a conoscere questo numero, quindi lo porta con se'.
    /// Retrocompatibile in entrambi i versi: assente (client vecchio) = i timeout
    /// per-processo di sempre; ignoto (gateway vecchio) = serde lo scarta.
    /// Puo' solo STRINGERE i budget, mai allungarli oltre il default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_timeout_secs: Option<u64>,
    pub metadata: RequestMetadata,
}

#[cfg(test)]
mod test_wire_run_timeout {
    use super::*;

    /// Client VECCHIO -> gateway NUOVO.
    ///
    /// Il corpo e' lo stesso sottoinsieme di campi che `scripts/onprem-smoke.sh`
    /// invia oggi in produzione: se questo test diventa rosso, quel corpo smette
    /// di essere accettato e lo smoke test on-prem si rompe in silenzio.
    #[test]
    fn un_corpo_senza_il_campo_resta_valido() {
        let corpo = r#"{
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "ciao"}],
            "metadata": {"tenant_id":"t","user_id":"u","request_id":"r","sensitivity_tier":0,"feature":"smoke"}
        }"#;
        let req: LlmRequest = serde_json::from_str(corpo).expect("corpo storico valido");
        assert_eq!(req.run_timeout_secs, None);
    }

    /// Client NUOVO -> gateway NUOVO: il valore arriva.
    #[test]
    fn il_campo_arriva_quando_e_presente() {
        let corpo = r#"{
            "model": "gpt-4o-mini",
            "messages": [],
            "run_timeout_secs": 240,
            "metadata": {"tenant_id":"t","user_id":"u","request_id":"r","sensitivity_tier":0,"feature":"agent"}
        }"#;
        let req: LlmRequest = serde_json::from_str(corpo).expect("corpo nuovo valido");
        assert_eq!(req.run_timeout_secs, Some(240));
    }

    /// Client NUOVO -> gateway VECCHIO: il campo non deve comparire quando e'
    /// assente, o un deserializzatore piu' rigido a valle lo vedrebbe come
    /// `null` inatteso.
    #[test]
    fn il_campo_assente_non_viene_serializzato() {
        let req = LlmRequest {
            model: "m".into(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            run_timeout_secs: None,
            metadata: RequestMetadata {
                tenant_id: "t".into(),
                user_id: "u".into(),
                request_id: "r".into(),
                sensitivity_tier: 0,
                feature: "f".into(),
            },
        };
        let json = serde_json::to_string(&req).expect("serializza");
        assert!(
            !json.contains("run_timeout_secs"),
            "campo assente non deve finire sul wire: {json}"
        );
    }
}

/// Configurazione extended thinking (`thinking` di `LLMRequest`). `budget_tokens`
/// opzionale: se assente il provider usa il budget dai settings DB (regola G).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
    /// Thinking OBBLIGATORIO per il modello (es. gemini-3): il modello RIFIUTA
    /// `thinkingBudget=0` (HTTP 400) e, senza `thinkingConfig`, applica il suo
    /// thinking DEFAULT ILLIMITATO che divora il tetto di output -> risposta vuota
    /// (`finish_reason=length`). Quando `true`, `resolve_thinking` emette un budget
    /// bounded ESPLICITO anche sui turni con tool (invece di `DisabledForTools`),
    /// e `build_generation_config` alza `maxOutputTokens` del budget cosi' la
    /// risposta ha spazio. Popolato dall'adapter mcp-core dalla policy del catalog
    /// (`agentic_thinking_policy='native'`, regola G): default `false` -> nessuna
    /// regressione per i modelli non-obbligatori. Solo trasporto, non serializzato
    /// verso i provider (usato in `resolve_thinking`).
    #[serde(default)]
    pub mandatory: bool,
}

/// Come il provider riporta i token di prompt serviti dalla cache.
///
/// Non e' una preferenza ne' una policy: e' un FATTO del formato di risposta,
/// e l'unico punto del sistema che lo conosce e' l'adapter che quel formato lo
/// deserializza. Viaggia come enum e non come nome di provider proprio perche'
/// il punto di normalizzazione non deve contenere un `match provider` (regola L:
/// la conoscenza resta dove nasce, la regola sta scritta una volta sola).
///
/// E' il segnale STRUTTURATO (regola M) che dice in quale verso normalizzare
/// verso la convenzione del sistema, il LORDO: la variante inclusiva non tocca
/// nulla, quella separata somma le due quantita' di cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptCacheReporting {
    /// Il conteggio di prompt e' LORDO: comprende gia' i token serviti da cache.
    /// OpenAI (`prompt_tokens` con `prompt_tokens_details.cached_tokens`),
    /// DeepSeek (`prompt_cache_hit_tokens`), Google/Vertex
    /// (`promptTokenCount` con `cachedContentTokenCount`) e tutti i dialetti
    /// OpenAI-compatibili che ne ereditano la forma (mistral, groq, perplexity,
    /// vllm, openrouter).
    CachedIncludedInPrompt,
    /// Il conteggio di prompt e' gia' NETTO e i token di cache sono riportati a
    /// parte. Anthropic: `input_tokens` esclude sia `cache_read_input_tokens`
    /// sia `cache_creation_input_tokens`.
    CachedReportedSeparately,
}

/// Cosa deve fare la RICHIESTA perche' il provider riusi il prefisso gia'
/// calcolato. Duale di [`PromptCacheReporting`], che riguarda la risposta:
/// quello dice come si LEGGE la cache ottenuta, questo cosa si deve CHIEDERE
/// per ottenerla.
///
/// Come il duale, e' un fatto del dialetto e non una preferenza, quindi lo
/// dichiara l'adapter che quel dialetto lo conosce: il punto unico che compone
/// il body non contiene un `match provider` (regola L).
///
/// Misurato sul provider reale il 29/07/2026, stesso prefisso di 11.918 token
/// ripetuto tre volte su `mistral-medium-latest`: senza `prompt_cache_key`
/// `cached_tokens` resta 0 a ogni chiamata, con la chiave passa a 11.904 dalla
/// seconda in poi. Sul ledger il difetto valeva 93 chiamate con 2,9 milioni di
/// token di prompt e 80 token di cache totali.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptCacheKeying {
    /// Il provider riconosce il prefisso da solo: la richiesta non dichiara
    /// nulla. DeepSeek (misurato 66-68% di token serviti da cache senza alcun
    /// campo in richiesta), Anthropic (che usa invece `cache_control` sui
    /// blocchi, dialetto proprio e non OpenAI).
    ProviderManaged,
    /// Il provider riusa il prefisso solo se la richiesta porta un
    /// identificatore stabile del gruppo di chiamate che lo condividono
    /// (`prompt_cache_key`). Mistral, OpenAI.
    ///
    /// E' un hint di ROUTING, non una chiave di lookup: il provider confronta
    /// comunque il contenuto reale del prefisso, quindi una chiave troppo larga
    /// puo' solo far perdere un riuso, mai servire il prefisso di un altro.
    RequiresKey,
    /// L'endpoint e' un instradatore verso fornitori terzi, e i livelli di
    /// affinita' sono DUE: quale fornitore (lo fissa `session_id`, letto
    /// dall'instradatore) e quale macchina DI quel fornitore (lo fissa
    /// `prompt_cache_key`, che l'instradatore inoltra a valle). OpenRouter.
    ///
    /// Servono entrambi i campi, con la stessa chiave. MISURATO su
    /// OpenRouter->xAI (29/07/2026, 4 chiamate consecutive a prefisso
    /// identico): col solo `session_id` la cache non arrivava MAI — 128 token
    /// fissi, il blocco minimo — perche' il fornitore era quello giusto ma la
    /// macchina cambiava a ogni colpo; col solo `prompt_cache_key`, e con
    /// entrambi, 8704/8797 stabile dal secondo colpo. E' la stessa domanda di
    /// [`Self::RequiresKey`] — «quali chiamate condividono il prefisso?» —
    /// posta due volte, una per livello.
    ///
    /// I livelli sono in realta' TRE, e il primo `session_id` non lo copre: vedi
    /// [`Self::requires_upstream_pinning`].
    RequiresSessionId,
}

impl PromptCacheKeying {
    /// Su questo endpoint la richiesta deve dichiarare anche QUALE fornitore a
    /// valle preferisce?
    ///
    /// E' il terzo livello di affinita' di un instradatore, e non e' un doppione
    /// di `session_id`: quel campo il fornitore doveva fissarlo e NON lo fissa.
    /// MISURATO il 29/07/2026 chiamando direttamente l'API OpenRouter — l'unico
    /// modo per leggere il campo `provider` della risposta e vedere chi ha
    /// servito — 8 chiamate consecutive a prefisso identico, con `session_id` e
    /// `prompt_cache_key` regolarmente inviati: su `qwen/qwen3-235b-a22b-2507`
    /// la stessa sequenza girava fra DeepInfra, Alibaba e Novita, 0/8 di cache;
    /// su `z-ai/glm-4.7-flash` alternava Cloudflare e DeepInfra, 6/8.
    ///
    /// Il livello conta perche' i fornitori dello stesso modello NON si
    /// equivalgono: su qwen3-235b, Google serve il 99% del prefisso e
    /// DeepInfra/Alibaba/Novita non ne servono niente. Restare sul fornitore
    /// giusto vale piu' del fissare "un" fornitore qualunque.
    ///
    /// `x-ai/grok-4.5` non aveva questo difetto — e' il modello su cui il fix
    /// precedente sembrava sufficiente — perche' OpenRouter lo serve da un solo
    /// fornitore: li' non c'era niente da scegliere.
    ///
    /// Il criterio sta qui, il VALORE (quale fornitore per quale modello) sta nel
    /// DB, tabella `nexus_router_upstream_affinity` (mig 0657, regola G). Chi
    /// aggiunge un nuovo instradatore tocca solo `cache_keying_per_endpoint`.
    pub fn requires_upstream_pinning(self) -> bool {
        matches!(self, Self::RequiresSessionId)
    }
}

/// Conteggio token consumati.
///
/// ## Convenzione: `input_tokens` e' il LORDO (punto unico, regola L)
///
/// `input_tokens` e' sempre il prompt LORDO: comprende i token serviti da cache
/// e quelli scritti in cache, che `cache_read_tokens` e `cache_creation_tokens`
/// riportano a parte come DETTAGLIO, non come quantita' da sommare.
///
/// Il lordo e' cio' che quasi tutti i consumatori vogliono — quanto contesto e'
/// stato inviato, quanto e' piena la finestra, quanto e' cresciuta la history da
/// un turno all'altro — ed e' l'unica quantita' confrontabile fra turni e fra
/// provider: la quota servita da cache la decide il provider e cambia da una
/// chiamata all'altra, quindi un prompt "al netto" oscilla per ragioni che non
/// hanno nulla a che vedere col contesto inviato.
///
/// Il NETTO serve a un solo consumatore, la tariffa, perche' le tre quantita'
/// hanno tre prezzi diversi (cache read 0.1x-0.5x, cache creation 1.25x, input
/// 1x — vedi `db/migrations/0403_cache_prices_catalog.sql`). Lo scorporo avviene
/// LI' e solo li': `nexus_pricing::calculate_cost_breakdown` sottrae le due
/// quantita' di cache dal lordo e moltiplica le tre parti per le tre tariffe.
///
/// I provider divergono su come riportano il prompt (vedi
/// [`PromptCacheReporting`]). La normalizzazione VERSO IL LORDO avviene UNA
/// volta, in [`LlmUsage::normalized`], chiamata dagli adapter. A valle (gateway
/// ledger, mcp-core, agent-graph, UI) il significato dei campi non dipende piu'
/// da chi ha risposto.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LlmUsage {
    /// Token di prompt LORDI: il contesto inviato, cache COMPRESA.
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Token serviti da cache (prompt caching): sottoinsieme di `input_tokens`,
    /// riportato a parte perche' ha una tariffa propria. `None` finche' il
    /// provider non li riporta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u32>,
    /// Token scritti in cache (creazione voce cache): anch'essi sottoinsieme di
    /// `input_tokens`, con la loro tariffa. Vedi sopra.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u32>,
    /// Token di ragionamento che il provider tiene FUORI da `output_tokens`.
    ///
    /// A differenza dei due campi di cache — che sono un DETTAGLIO di
    /// `input_tokens` — questo e' un ADDENDO: si somma all'output per ottenere
    /// il fatturabile, e la somma la fa il punto unico
    /// `nexus_types::token_usage::completion_tokens_billable`.
    ///
    /// `None` su quasi tutti i provider, e non perche' non pensino: significa
    /// che il conteggio di output del wire li comprende gia' (vedi
    /// [`ReasoningTokens`]). Valorizzato oggi dal solo adapter Google.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
    /// Costo TOTALE della chiamata dichiarato dal fornitore sul wire (USD).
    /// Oggi lo emette il solo OpenRouter (usage accounting, opt-in nel body:
    /// mig 0717). E' un FATTO del wire che il ledger REGISTRA, non un prezzo
    /// che qualcuno calcola: il listino resta `nexus-pricing` (regola L), e la
    /// precedenza fra dichiarato e riprezzato la decide il ledger in un punto
    /// solo. `None` = il fornitore non lo dichiara.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_cost_usd: Option<f64>,
    /// Costo dell'inference a valle dichiarato da un aggregatore (openrouter
    /// `cost_details.upstream_inference_cost`). Solo telemetria: finisce nei
    /// `details` della riga di ledger, nessuna decisione lo legge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_cost_usd: Option<f64>,
}

/// Convenzione con cui un provider riporta i token di ragionamento, dichiarata
/// dall'adapter che ne deserializza il formato.
///
/// E' un tipo e non un `Option<u32>` perche' la distinzione che conta non e'
/// "quanti", ma "erano gia' contati?" — e la risposta sbagliata non produce un
/// numero strano, produce un addebito doppio. Con questa forma un adapter non
/// puo' portare un numero dichiarando che e' gia' incluso: la variante che
/// afferma l'inclusione non ha posto dove metterlo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningTokens {
    /// Il conteggio di output del wire COMPRENDE gia' il ragionamento: nulla da
    /// sommare. E' il caso di Anthropic (`output_tokens`) e di OpenAI
    /// (`completion_tokens`, di cui `completion_tokens_details.reasoning_tokens`
    /// e' un dettaglio), quindi di tutto il dialetto OpenAI-compatibile.
    IncludedInOutput,
    /// Il wire riporta l'output VISIBILE e il ragionamento a parte: i due vanno
    /// sommati per ottenere il fatturabile. E' il caso di Google, dove
    /// `candidatesTokenCount` esclude `thoughtsTokenCount` mentre
    /// `totalTokenCount` li comprende entrambi. `None` quando il turno non ne ha
    /// prodotti (campo assente dalla risposta).
    Separate(Option<u32>),
}

impl LlmUsage {
    /// Costruisce l'usage normalizzato alla convenzione del sistema: prompt
    /// LORDO.
    ///
    /// PUNTO UNICO (regola L): e' l'unico posto del workspace dove si decide se
    /// i token di cache vanno sommati al prompt. Gli adapter passano i numeri
    /// VERBATIM dal wire e dichiarano soltanto la propria convenzione — sul
    /// prompt ([`PromptCacheReporting`]) e sul ragionamento ([`ReasoningTokens`]).
    ///
    /// Il ragionamento NON viene sommato all'output qui: resta un campo a se',
    /// perche' `output_tokens` ha un secondo consumatore che misura il testo
    /// PRODOTTO (`is_degenerate_completion`). La somma verso il fatturabile
    /// avviene al confine con la contabilita'.
    pub fn normalized(
        reporting: PromptCacheReporting,
        prompt_tokens_wire: u32,
        output_tokens: u32,
        cache_read_tokens: Option<u32>,
        cache_creation_tokens: Option<u32>,
        reasoning: ReasoningTokens,
    ) -> Self {
        let input_tokens = match reporting {
            // Il wire e' gia' lordo: nulla da fare. Sottrarre qui — come faceva
            // la convenzione opposta — renderebbe il prompt non confrontabile
            // fra turni e romperebbe ogni consumatore che misura il contesto.
            PromptCacheReporting::CachedIncludedInPrompt => prompt_tokens_wire,
            // Il wire e' al netto: il lordo e' la somma, dal punto unico
            // `nexus_types::token_usage::prompt_tokens_gross` (somma satura).
            PromptCacheReporting::CachedReportedSeparately => {
                nexus_types::token_usage::prompt_tokens_gross(
                    prompt_tokens_wire,
                    cache_read_tokens,
                    cache_creation_tokens,
                )
            }
        };
        // Lo zero e' il caso di gran lunga piu' frequente sui provider che
        // riportano a parte (turno senza ragionamento) e non e' informazione:
        // `None` e "zero token" dicono la stessa cosa a chi somma, e tenerli
        // distinti moltiplicherebbe i casi da asserire nei test senza che
        // nessun consumatore sappia farci qualcosa.
        let reasoning_tokens = match reasoning {
            ReasoningTokens::IncludedInOutput => None,
            ReasoningTokens::Separate(n) => n.filter(|&n| n > 0),
        };
        Self {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            reasoning_tokens,
            // I costi dichiarati non c'entrano con la normalizzazione dei
            // token: li valorizza il solo adapter che li legge dal wire, con
            // [`Self::with_declared_cost`].
            declared_cost_usd: None,
            upstream_cost_usd: None,
        }
    }

    /// Aggiunge il costo DICHIARATO dal fornitore sul wire (USD). Lo chiama il
    /// solo adapter che lo legge (oggi il dialetto OpenAI-compat per
    /// openrouter); per tutti gli altri i campi restano `None` = non
    /// dichiarato, che non e' un costo zero (regola Q).
    pub fn with_declared_cost(mut self, total: Option<f64>, upstream: Option<f64>) -> Self {
        self.declared_cost_usd = total;
        self.upstream_cost_usd = upstream;
        self
    }
}

/// Informazioni sul re-routing per privacy (`privacy_rerouted`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyRerouted {
    pub provider: String,
    pub blocked_tier: u8,
    pub reason: String,
}

/// Vocabolario contabile del wire, dal punto unico (`nexus-ledger`).
///
/// [`LedgerOutcome`] e' il segnale STRUTTURATO (regola M) con cui il gateway
/// dichiara al chiamante cosa ha fatto della contabilita' di QUESTA chiamata:
/// `written` (con la [`LedgerEntry`] scritta) e' il permesso di non addebitare
/// una seconda volta; `no_identity` e `write_failed` sono "non ho scritto" detti
/// con precisione, e obbligano il chiamante ad addebitare lui.
///
/// Nessuno dei due va dedotto dal fatto che la chiamata sia RIUSCITA: la
/// chiamata riesce in tutti e tre i casi.
///
/// L'unico esito non dichiarabile e' il campo ASSENTE, e ora significa una cosa
/// sola: gateway che non parla questa versione del contratto. NON e' innocuo, e
/// il chiamante lo tratta come sospetto quando aveva mandato un'identita' valida
/// (`nexus_ledger::Declaration::audit`) — un gateway di build precedente la riga
/// l'ha scritta comunque, quindi finalizzare per silenzio addebita due volte.
///
/// I tipi vivono nel punto unico e qui sono ri-esportati: e' il vocabolario
/// CONDIVISO fra i due lati del wire, e finche' erano struct gemelle — una per
/// lato — nessun compilatore le teneva allineate.
pub use nexus_ledger::{LedgerEntry, LedgerOutcome};

/// Risposta non-streaming (`LLMResponse`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<LlmToolCall>>,
    pub usage: LlmUsage,
    pub model_used: String,
    pub provider_used: String,
    pub latency_ms: u64,
    pub finish_reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub privacy_rerouted: Option<PrivacyRerouted>,
    /// Testo del ragionamento (extended thinking) visibile, quando il provider
    /// lo emette. Retrocompatibile: `None` se non disponibile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Firma opaca del blocco `thinking` da ri-passare nei turni successivi con
    /// tool (Anthropic). Il chiamante la rispedisce via
    /// [`LlmMessage::thinking_signature`]. Retrocompatibile: `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    /// Citazioni (URL fonti) dei provider di ricerca: Perplexity espone un array
    /// top-level `citations` nella risposta (non standard OpenAI). Retrocompatibile:
    /// `None` per i provider che non le emettono. Regola M: campo strutturato,
    /// mai estratto dal testo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citations: Option<Vec<String>>,
    /// Cosa ha fatto il gateway della contabilita' di questa chiamata (vedi
    /// [`LedgerOutcome`]). La valorizza SOLO la pipeline HTTP, in
    /// `server::billing::record_and_declare`: i provider non la toccano, ed e'
    /// per questo che nascono tutte con `None`.
    ///
    /// `None` qui dentro significa "non ancora dichiarato". Sul WIRE lo stesso
    /// `None` sparisce (`skip_serializing_if`) e diventa il silenzio che il
    /// chiamante legge come `Declaration::Muta`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<LedgerOutcome>,
}

impl LlmResponse {
    /// Risposta DEGENERE: HTTP 200 senza alcun output utile (regola M, solo
    /// segnali strutturati, mai parsing di prosa). Vero quando il turno non
    /// produce ne' testo ne' tool-call E il `finish_reason` non e' una chiusura
    /// legittima. Caso tipico: Gemini consuma l'intero budget nel thinking e
    /// ritorna `content=""`, `tool_calls=None`, `finish_reason="length"`
    /// (google.rs `map_finish_reason` MAX_TOKENS -> "length"). Senza questo
    /// predicato il gateway tratterebbe il 200 come successo e il motore non
    /// ripiegherebbe mai su un provider alternativo.
    ///
    /// Condizioni (tutte necessarie):
    /// - `content` vuoto o solo whitespace;
    /// - nessuna tool-call (`None` oppure `Vec` vuoto);
    /// - `finish_reason` NON e' un blocco di safety deliberato (`"content_filter"`),
    ///   l'unico esito con output vuoto che NON va aggirato con un failover.
    ///
    /// NB (regola M): il segnale PRIMARIO e strutturale e' "nessun output utile"
    /// (content vuoto + zero tool-call). NON si esclude `"stop"`: Google
    /// (`map_finish_reason`) collassa a `"stop"` ogni finishReason anomalo non
    /// mappato — `MALFORMED_FUNCTION_CALL`, `OTHER`, `BLOCKLIST`,
    /// `FINISH_REASON_UNSPECIFIED` — e `MALFORMED_FUNCTION_CALL` con output vuoto e'
    /// il caso Gemini PIU' frequente di hollow sul tool-forcing (agent_run.rs:3169).
    /// Un turno senza output e' inservibile qualunque sia il `finish_reason`, e
    /// ripiegare su un altro provider e' sempre preferibile a restituire un 200
    /// vuoto; la sola eccezione e' `content_filter`, dove il vuoto e' una scelta di
    /// safety intenzionale da non aggirare.
    ///
    /// Non e' degenere una risposta con SOLE tool-call (content vuoto ma
    /// `tool_calls` non vuoto): e' il normale comportamento agentico.
    pub fn is_degenerate_completion(&self) -> bool {
        let no_content = self.content.trim().is_empty();
        let no_tool_calls = self
            .tool_calls
            .as_ref()
            .is_none_or(|calls| calls.is_empty());
        // Solo il blocco di safety (`content_filter`) e' una chiusura legittima con
        // output vuoto; `"stop"` NON e' escluso (Google vi collassa anche
        // MALFORMED_FUNCTION_CALL, output vuoto reale che deve ripiegare).
        let safety_block = self.finish_reason == "content_filter";
        if safety_block {
            return false;
        }
        // (1) Storico: nessun output utile (content vuoto E nessuna tool-call).
        if no_content && no_tool_calls {
            return true;
        }
        // (2) CONTRATTO STRUTTURATO (regola M, fix gemini hollow-ma-non-vuoto): il
        // modello e' stato TRONCATO dal limite di token (finish=length: il budget e'
        // stato speso nel thinking) senza una tool-call e con output VISIBILE
        // trascurabile. Il segnale e' l'`usage.output_tokens` STRUTTURATO (per Gemini
        // = candidatesTokenCount, che ESCLUDE i thinking token, google.rs
        // usage_from_metadata): ~0 significa "nessun output reale" = fallito, anche se
        // resta un frammento di content. Cattura il caso che (1) mancava (RC-2: una
        // functionCall vuota/malformata o un frammento non-degenere sfuggiva al gate).
        // Gating STRETTO (finish=length + no tool-call + output <= floor) per non
        // flaggare un troncamento legittimo con output reale.
        let cut_off_by_length = matches!(self.finish_reason.as_str(), "length" | "max_tokens");
        cut_off_by_length && no_tool_calls && self.usage.output_tokens <= HOLLOW_OUTPUT_TOKENS_FLOOR
    }
}

/// Soglia (token di output VISIBILE) sotto la quale un turno troncato da `length`
/// senza tool-call e' considerato hollow (contratto strutturato, regola M). Piccola:
/// distingue un frammento trascurabile da una risposta parziale reale.
const HOLLOW_OUTPUT_TOKENS_FLOOR: u32 = 32;

/// Delta di tool-call durante lo streaming.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCallDeltaFunction {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallDelta {
    pub index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<ToolCallDeltaFunction>,
}

/// Chunk di streaming (`LLMStreamChunk`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmStreamChunk {
    pub delta: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_delta: Option<ToolCallDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<LlmUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_used: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_used: Option<String>,
    /// Delta del testo di reasoning (extended thinking) durante lo streaming.
    /// Retrocompatibile: `None` sui chunk che non portano thinking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_delta: Option<String>,
}

/// Stato di salute di un provider (`ProviderStatus`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub name: String,
    pub healthy: bool,
    pub last_check: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Messaggio di errore di billing (crediti esauriti). Presente solo se rilevato.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billing_error: Option<String>,
}

/// Richiesta di generazione immagine (`ImageGenRequest`). Speculare a
/// [`LlmRequest`] ma per il task image-generation: niente messaggi/tool, solo un
/// `prompt` testuale. Regola G: il `model` arriva sempre dal chiamante (nessun
/// default hardcoded). `pin_provider` ha la stessa semantica di
/// [`LlmRequest::pin_provider`] (bypass routing, esecuzione di QUEL provider).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenRequest {
    pub model: String,
    pub prompt: String,
    /// Numero di immagini da generare (default lato provider se assente).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    /// Dimensione richiesta (es. "1024x1024"); il formato dipende dal provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    /// Pin esplicito del provider da eseguire (bypass routing). Quando `Some`, il
    /// gateway esegue ESATTAMENTE quel provider; quando `None`, sceglie il primo
    /// provider sano che dichiara `supports_image_gen()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_provider: Option<String>,
    pub metadata: RequestMetadata,
}

/// Una immagine generata. I provider espongono o il base64 inline (OpenAI
/// `b64_json`, Vertex `bytesBase64Encoded`) o una URL temporanea (OpenAI
/// `response_format=url`): entrambi opzionali, almeno uno valorizzato. `mime`
/// presente quando il provider lo dichiara (Vertex `mimeType`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedImage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub b64_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
}

/// Risposta di generazione immagine (`ImageGenResponse`). Speculare a
/// [`LlmResponse`] per i campi di tracciamento (model/provider/latency).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenResponse {
    pub images: Vec<GeneratedImage>,
    pub model_used: String,
    pub provider_used: String,
    pub latency_ms: u64,
}

/// Richiesta di generazione video (`VideoGenRequest`, text-to-video). Speculare a
/// [`ImageGenRequest`] ma per il task video-gen: niente messaggi/tool, solo un
/// `prompt` testuale + la durata opzionale. Regola G: il `model` arriva sempre
/// dal chiamante (nessun default hardcoded). `pin_provider` ha la stessa
/// semantica di [`ImageGenRequest::pin_provider`] (bypass routing, esecuzione di
/// QUEL provider).
///
/// DIFFERENZA CHIAVE rispetto a image-gen: il backend (Vertex Veo) e' ASYNC
/// long-running (`:predictLongRunning` -> operation -> poll). Per l'MVP il polling
/// e' incapsulato DENTRO il gateway (richiesta/risposta sincrona per il client):
/// l'handler fa start + poll-loop con timeout DB-driven, poi ritorna il video.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoGenRequest {
    pub model: String,
    pub prompt: String,
    /// Durata richiesta del video in secondi (default lato provider se assente).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u32>,
    /// Pin esplicito del provider da eseguire (bypass routing). Quando `Some`, il
    /// gateway esegue ESATTAMENTE quel provider; quando `None`, sceglie il primo
    /// provider sano che dichiara `supports_video_gen()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_provider: Option<String>,
    pub metadata: RequestMetadata,
}

/// Risposta di generazione video (`VideoGenResponse`). Il provider Veo puo'
/// restituire i byte del video inline (base64) oppure una `gcsUri` (URL Google
/// Cloud Storage). Entrambi opzionali, almeno uno valorizzato: il chiamante che
/// puo' salvare path-safe usa `video_base64`, altrimenti riporta la `url` con una
/// nota (regola H: niente fetch nascosto di una URL esterna). Speculare a
/// [`ImageGenResponse`]/[`TtsResponse`] per i campi di tracciamento.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoGenResponse {
    /// Video codificato base64 inline (Veo `bytesBase64Encoded`). `None` quando il
    /// provider risponde solo con una `gcsUri`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_base64: Option<String>,
    /// URL del video (Veo `gcsUri`), quando il provider non emette i byte inline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// MIME del video prodotto (es. `video/mp4`): dal campo `mimeType` del
    /// provider quando presente, altrimenti un default coerente.
    pub mime: String,
    pub model_used: String,
    pub provider_used: String,
    pub latency_ms: u64,
}

/// Richiesta di trascrizione audio (`TranscribeRequest`, speech-to-text).
/// Speculare a [`ImageGenRequest`] ma per il task audio-in: niente messaggi/tool,
/// solo l'audio (base64) + il modello. Regola G: il `model` arriva sempre dal
/// chiamante (nessun default hardcoded). `pin_provider` ha la stessa semantica di
/// [`LlmRequest::pin_provider`] (bypass routing, esecuzione di QUEL provider).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscribeRequest {
    pub model: String,
    /// Audio sorgente codificato base64 (il gateway lo decodifica e lo invia come
    /// multipart `file` al provider). Niente URL: il gateway non fa fetch esterni.
    pub audio_base64: String,
    /// MIME dell'audio (es. `audio/mpeg`, `audio/wav`): usato per nominare la part
    /// multipart con l'estensione corretta. `None` => estensione generica.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// Lingua dell'audio in ISO-639-1 (es. `it`, `en`), opzionale: migliora
    /// accuratezza/latency. `None` => il provider la rileva da solo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Pin esplicito del provider da eseguire (bypass routing). Quando `Some`, il
    /// gateway esegue ESATTAMENTE quel provider; quando `None`, sceglie il primo
    /// provider sano che dichiara `supports_audio_in()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_provider: Option<String>,
    pub metadata: RequestMetadata,
}

/// Risposta di trascrizione audio (`TranscribeResponse`). Speculare a
/// [`ImageGenResponse`] per i campi di tracciamento (model/provider/latency).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscribeResponse {
    /// Testo trascritto dall'audio.
    pub text: String,
    pub model_used: String,
    pub provider_used: String,
    pub latency_ms: u64,
}

/// Richiesta di sintesi vocale (`TtsRequest`, text-to-speech). Speculare a
/// [`ImageGenRequest`] ma per il task audio-out: niente messaggi/tool, solo il
/// testo da pronunciare + il modello. Regola G: il `model` arriva sempre dal
/// chiamante (nessun default hardcoded). `pin_provider` ha la stessa semantica di
/// [`LlmRequest::pin_provider`] (bypass routing, esecuzione di QUEL provider).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsRequest {
    pub model: String,
    /// Testo da convertire in audio.
    pub input: String,
    /// Voce del modello TTS (es. `alloy`, `nova`): opzionale, default lato
    /// provider se assente. Non e' un nome modello (regola G): e' un timbro.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Formato audio richiesto (es. `mp3`, `wav`, `opus`, `flac`): opzionale,
    /// default lato provider (`mp3`) se assente.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<String>,
    /// Pin esplicito del provider da eseguire (bypass routing). Quando `Some`, il
    /// gateway esegue ESATTAMENTE quel provider; quando `None`, sceglie il primo
    /// provider sano che dichiara `supports_audio_out()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_provider: Option<String>,
    pub metadata: RequestMetadata,
}

/// Risposta di sintesi vocale (`TtsResponse`). Il provider risponde con BYTES
/// binari (Content-Type `audio/mpeg`): il gateway li legge e li ritorna in base64
/// al client, coerente con il resto del contratto JSON. Speculare a
/// [`ImageGenResponse`] per i campi di tracciamento (model/provider/latency).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsResponse {
    /// Audio sintetizzato codificato base64 (il client lo decodifica e lo salva).
    pub audio_base64: String,
    /// MIME dell'audio prodotto (es. `audio/mpeg`): dal Content-Type della risposta
    /// del provider, o derivato dal `response_format` richiesto.
    pub mime: String,
    pub model_used: String,
    pub provider_used: String,
    pub latency_ms: u64,
}

/// Voce della tabella di alias modello (`ModelAliasEntry`, da model-aliases.yaml).
///
/// I tre campi modello sono `Option` perche' nello YAML possono valere `null`
/// (es. alias solo-onprem o alias di fallback senza on-premise). `#[serde(default)]`
/// li rende anche assenti-tolleranti: una chiave mancante equivale a `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAliasEntry {
    #[serde(default)]
    pub cloud_primary: Option<String>,
    #[serde(default)]
    pub cloud_secondary: Option<String>,
    #[serde(default)]
    pub onprem: Option<String>,
    pub min_tier: SensitivityTier,
    pub max_tier: SensitivityTier,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Costruisce una `LlmResponse` minimale variando i soli campi rilevanti per
    /// il predicato di degenerazione.
    fn resp(content: &str, tool_calls: Option<Vec<LlmToolCall>>, finish_reason: &str) -> LlmResponse {
        LlmResponse {
            content: content.to_string(),
            tool_calls,
            usage: LlmUsage {
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                reasoning_tokens: None,
                declared_cost_usd: None,
                upstream_cost_usd: None,
            },
            model_used: "m".to_string(),
            provider_used: "p".to_string(),
            latency_ms: 0,
            finish_reason: finish_reason.to_string(),
            privacy_rerouted: None,
            reasoning: None,
            thinking_signature: None,
            citations: None,
            ledger: None,
        }
    }

    fn a_tool_call() -> LlmToolCall {
        LlmToolCall {
            id: "call_1".to_string(),
            kind: "function".to_string(),
            function: ToolFunctionCall {
                name: "read_file".to_string(),
                arguments: "{}".to_string(),
            },
            thought_signature: None,
        }
    }

    #[test]
    fn empty_content_length_finish_is_degenerate() {
        // Caso Gemini: budget consumato dal thinking, content vuoto,
        // finish_reason="length", nessuna tool-call -> degenere.
        assert!(resp("", None, "length").is_degenerate_completion());
        assert!(resp("   \n\t ", None, "length").is_degenerate_completion());
        assert!(resp("", Some(vec![]), "length").is_degenerate_completion());
    }

    #[test]
    fn only_tool_calls_is_not_degenerate() {
        // Comportamento agentico legittimo: content vuoto ma tool-call presenti.
        let r = resp("", Some(vec![a_tool_call()]), "tool_calls");
        assert!(!r.is_degenerate_completion());
        // Anche con content vuoto e finish_reason non-stop: le tool-call salvano.
        let r2 = resp("", Some(vec![a_tool_call()]), "length");
        assert!(!r2.is_degenerate_completion());
    }

    #[test]
    fn empty_stop_is_degenerate_but_safety_block_is_not() {
        // "stop" con output vuoto e' DEGENERE: Google collassa a "stop" anche
        // MALFORMED_FUNCTION_CALL (il caso Gemini piu' frequente di hollow sul
        // tool-forcing). Il turno non ha output -> deve ripiegare su un altro provider.
        assert!(resp("", None, "stop").is_degenerate_completion());
        // Blocco di safety (content_filter): esito deliberato, NON aggirabile via failover.
        assert!(!resp("", None, "content_filter").is_degenerate_completion());
    }

    #[test]
    fn risposta_reale_troncata_non_e_degenere() {
        // Una risposta REALE troncata da length (output_tokens sostanziosi, > floor)
        // NON e' degenere: e' un output valido tagliato, non un hollow da thinking.
        let mut r = resp("una risposta lunga e utile del modello", None, "length");
        r.usage.output_tokens = 500;
        assert!(!r.is_degenerate_completion());
        // finish "stop" con content: risposta completa, mai degenere (a prescindere
        // dagli output_tokens).
        assert!(!resp("parziale", None, "stop").is_degenerate_completion());
        let mut r3 = resp("breve ma completa", None, "stop");
        r3.usage.output_tokens = 3;
        assert!(!r3.is_degenerate_completion());
    }

    /// Il punto unico della normalizzazione, sui due casi che divergono.
    ///
    /// La proprieta' che conta e' l'INVARIANTE: qualunque sia la convenzione del
    /// provider, dopo la normalizzazione `input_tokens` vale il prompt LORDO —
    /// lo stesso numero, per lo stesso contesto inviato, da un provider e
    /// dall'altro. E' cio' su cui poggiano tutti i consumatori a valle.
    #[test]
    fn la_normalizzazione_porta_sempre_al_prompt_lordo() {
        // Convenzione inclusiva: i 4 di cache stanno DENTRO i 10 di prompt, che
        // e' gia' il lordo. Nulla da sommare.
        let incluso = LlmUsage::normalized(
            PromptCacheReporting::CachedIncludedInPrompt,
            10,
            5,
            Some(4),
            None,
            ReasoningTokens::IncludedInOutput,
        );
        assert_eq!(incluso.input_tokens, 10);
        assert_eq!(incluso.cache_read_tokens, Some(4));

        // Convenzione separata: i 4 di cache sono FUORI dai 10 di prompt, che e'
        // il netto. Il lordo e' 14, ed e' il numero che il provider inclusivo
        // avrebbe scritto per lo stesso contesto.
        let separato = LlmUsage::normalized(
            PromptCacheReporting::CachedReportedSeparately,
            10,
            5,
            Some(4),
            None,
            ReasoningTokens::IncludedInOutput,
        );
        assert_eq!(separato.input_tokens, 14);
        assert_eq!(separato.cache_read_tokens, Some(4));

        // Anche la cache di SCRITTURA e' fuori dall'input di Anthropic.
        let con_creazione = LlmUsage::normalized(
            PromptCacheReporting::CachedReportedSeparately,
            100,
            20,
            Some(900),
            Some(50),
            ReasoningTokens::IncludedInOutput,
        );
        assert_eq!(con_creazione.input_tokens, 1_050);

        // Senza cache le due convenzioni coincidono: nessuna regressione sulle
        // chiamate che non ne fanno uso (la stragrande maggioranza oggi).
        for reporting in [
            PromptCacheReporting::CachedIncludedInPrompt,
            PromptCacheReporting::CachedReportedSeparately,
        ] {
            let u = LlmUsage::normalized(
                reporting,
                42,
                7,
                None,
                None,
                ReasoningTokens::IncludedInOutput,
            );
            assert_eq!(u.input_tokens, 42);
        }
    }

    /// Dato incoerente dal provider a convenzione separata: la somma satura
    /// invece di wrappare. Un `+` al posto della somma satura produrrebbe qui un
    /// prompt piccolissimo che passerebbe per sano.
    #[test]
    fn somma_satura_su_conteggi_incoerenti() {
        let u = LlmUsage::normalized(
            PromptCacheReporting::CachedReportedSeparately,
            u32::MAX,
            1,
            Some(999),
            Some(1),
            ReasoningTokens::IncludedInOutput,
        );
        assert_eq!(u.input_tokens, u32::MAX);
    }

    #[test]
    fn frammento_hollow_troncato_da_length_e_degenere() {
        // CONTRATTO STRUTTURATO (regola M, RC-2): un frammento trascurabile con
        // finish=length e output_tokens ~0 (budget speso nel thinking) e' hollow, anche
        // se il content non e' strettamente vuoto. Il segnale e' l'usage STRUTTURATO
        // (candidatesTokenCount, esclude i thinking token), non il parsing del testo.
        let mut r = resp("x", None, "length");
        r.usage.output_tokens = 1;
        assert!(r.is_degenerate_completion());
        // Con una tool-call NON e' degenere (progresso agentico), a prescindere.
        let mut r2 = resp("x", Some(vec![a_tool_call()]), "length");
        r2.usage.output_tokens = 1;
        assert!(!r2.is_degenerate_completion());
        // "max_tokens" (alcuni provider) e' trattato come "length".
        let mut r4 = resp("", None, "max_tokens");
        r4.usage.output_tokens = 0;
        assert!(r4.is_degenerate_completion());
    }
}
