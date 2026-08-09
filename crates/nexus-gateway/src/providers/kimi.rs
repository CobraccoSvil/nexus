//! Provider Kimi (Moonshot AI), endpoint OpenAI-compatibile con tre quirk.
//!
//! Gemello di [`super::mistral::MistralProvider`] per la forma — thin wrapper su
//! [`OpenAiCompatClient`], composizione e non ereditarieta' (regola L) — ma NON
//! costruibile dal provider generico del registry, e la ragione non e' di stile.
//!
//! La documentazione ufficiale (`platform.kimi.ai/docs/api/models-overview`)
//! dichiara che su `kimi-k3`, `kimi-k2.7-code` e `kimi-k2.6` la `temperature` e'
//! un valore FISSO del modello, e verbatim: "'Fixed' means the parameter cannot
//! be modified: passing any other value returns an error, so do not pass it
//! explicitly". Il provider generico inoltra `req.temperature` cosi' com'e', e
//! nel sistema esistono tre call site che mandano `Some(0.0)` a ogni chiamata
//! (`agent_graph_adapter/summary_store.rs`, `agent_graph_adapter/next_actions_deriver.rs`,
//! `project_workspace/wizard.rs`): con l'adapter generico quei tre percorsi
//! sarebbero un HTTP 400 sistematico. Il quirk sta nel CONTRATTO del fornitore,
//! non nel testo di un errore: la sede giusta e' un dialetto (regola M/H), non
//! una toppa a valle.
//!
//! Gli altri due quirk stanno in [`ReasoningDialect::Kimi`], che li documenta:
//! `max_completion_tokens` al posto del deprecato `max_tokens`, e il round-trip
//! del `reasoning_content` preteso dal Preserved Thinking.
//!
//! CACHE: il fornitore cachea il prefisso da solo ("Context Caching is
//! automatically enabled for all model requests"), ma documenta anche il campo
//! `prompt_cache_key` come l'etichetta con cui raggruppare le richieste della
//! stessa sessione. E' la posizione di Mistral, e la si dichiara col vocabolario
//! gia' esistente: [`PromptCacheKeying::RequiresKey`].

use async_trait::async_trait;
use reqwest::Client;

use crate::provider::{ChunkStream, LlmProvider};
use crate::providers::openai_compat::{OpenAiCompatClient, ReasoningDialect, ResolvedReasoning};
use crate::types::{LlmRequest, LlmResponse, PromptCacheKeying, SensitivityTier};

/// Tier ammessi: pubblico/interno/confidenziale. Mai il tier 3, riservato a chi
/// gira on-premise: Moonshot e' un fornitore cloud extra-UE come gli altri.
const TIERS: &[SensitivityTier] = &[0, 1, 2];

/// Endpoint internazionale di default (override via costruttore/registry).
///
/// L'host API resta `api.moonshot.ai`: sono i domini di DOCUMENTAZIONE e console
/// ad essere migrati a `platform.kimi.ai` (301). Le chiavi dei due lati non sono
/// interscambiabili — una chiave del lato cinese su questo host non degrada, da'
/// 401 — quindi chi usa `api.moonshot.cn` cambia anche la chiave, e per farlo
/// gli basta il setting `kimi_base_url` senza toccare codice (regola G).
const DEFAULT_BASE_URL: &str = "https://api.moonshot.ai/v1";

/// Finestra del modello piu' capiente offerto (`kimi-k3`, 1M token). E' il tetto
/// del PROVIDER: la finestra per-modello vive in `ai_price_catalog.context_window`
/// e non si duplica qui.
const MAX_CONTEXT_TOKENS: u32 = 1_048_576;

pub struct KimiProvider {
    client: OpenAiCompatClient,
}

impl KimiProvider {
    pub fn new(http: Client, api_key: impl Into<String>, base_url: Option<String>) -> Self {
        let base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Self {
            client: OpenAiCompatClient::new(http, base_url, api_key, "kimi")
                .with_prompt_cache_keying(PromptCacheKeying::RequiresKey),
        }
    }

    /// Il dialetto e' SEMPRE `Kimi`, senza condizioni sul modello o sulla
    /// richiesta: i tre comportamenti che porta con se' (niente temperatura,
    /// `max_completion_tokens`, round-trip del pensiero) valgono per l'intero
    /// parco moderno del fornitore.
    ///
    /// `enabled` ed `effort` restano ai valori neutri perche' oggi non hanno un
    /// produttore: il pensiero su k3/k2.7-code non e' disattivabile, e i default
    /// dichiarati dal fornitore (k3 `max`, k2.x `enabled`) sono quelli che
    /// vogliamo. Governare l'effort richiederebbe di distinguere k3 dai k2.x, e
    /// quella distinzione va fatta con un dato (regola G), non con un
    /// riconoscimento sul nome scritto qui prima che serva.
    fn resolve_reasoning(&self) -> ResolvedReasoning {
        ResolvedReasoning {
            dialect: ReasoningDialect::Kimi,
            enabled: true,
            effort: None,
        }
    }
}

#[async_trait]
impl LlmProvider for KimiProvider {
    fn name(&self) -> &str {
        "kimi"
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn max_context_tokens(&self) -> u32 {
        MAX_CONTEXT_TOKENS
    }

    fn tier_compatibility(&self) -> &[SensitivityTier] {
        TIERS
    }

    async fn complete(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
        self.client
            .complete_with_reasoning(req, &self.resolve_reasoning())
            .await
    }

    async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        self.client
            .stream_with_reasoning(req, &self.resolve_reasoning())
            .await
    }

    async fn healthcheck(&self) -> bool {
        self.client.healthcheck().await
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        self.client.list_models().await
    }

    /// `GET /v1/models` di Moonshot dichiara `context_length` accanto all'id: la
    /// si propaga come fa Mistral, cosi' il catalog sync scrive la finestra REALE
    /// invece di lasciarla ignota (regola G/H).
    async fn list_models_meta(&self) -> anyhow::Result<Vec<crate::provider::ModelMeta>> {
        self.client.list_models_meta().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LlmMessage, MessageContent, RequestMetadata};

    fn provider() -> KimiProvider {
        KimiProvider::new(Client::new(), "chiave", None)
    }

    /// Richiesta con i due campi che il dialetto deve trattare in modo diverso
    /// dal default: una temperatura esplicita e un tetto di output. La history
    /// porta un assistant col pensiero, per il round-trip.
    fn richiesta() -> LlmRequest {
        LlmRequest {
            model: "kimi-k3".to_string(),
            messages: vec![
                LlmMessage {
                    role: "system".to_string(),
                    content: MessageContent::Text("istruzioni di progetto".to_string()),
                    tool_call_id: None,
                    tool_calls: None,
                    name: None,
                    thinking_signature: None,
                    reasoning: None,
                    is_error: None,
                },
                LlmMessage {
                    role: "assistant".to_string(),
                    content: MessageContent::Text("ecco la risposta".to_string()),
                    tool_call_id: None,
                    tool_calls: None,
                    name: None,
                    thinking_signature: None,
                    reasoning: Some("ho ragionato cosi'".to_string()),
                    is_error: None,
                },
            ],
            // E' il valore che i tre call site interni mandano davvero.
            temperature: Some(0.0),
            max_tokens: Some(2048),
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: RequestMetadata {
                tenant_id: "t".to_string(),
                user_id: "u".to_string(),
                request_id: "r".to_string(),
                sensitivity_tier: 0,
                feature: "f".to_string(),
            },
            run_timeout_secs: None,
        }
    }

    /// Il corpo che parte davvero, costruito dalla stessa strada di `complete`:
    /// `resolve_reasoning` reale e `corpo_della_richiesta` reale, non un
    /// `ResolvedReasoning` scritto a mano nel test (regola O — e' l'errore che
    /// rese verdi per sempre i tre test di `error_class_from_gateway`).
    async fn corpo() -> serde_json::Value {
        let p = provider();
        serde_json::to_value(
            p.client
                .corpo_della_richiesta(&richiesta(), false, &p.resolve_reasoning())
                .await,
        )
        .expect("il corpo serializza")
    }

    /// IL quirk che costa 400 a ogni chiamata: la temperatura non deve partire.
    ///
    /// La doc dice "passing any other value returns an error, so do not pass it
    /// explicitly", e tre call site interni mandano `Some(0.0)` sempre. Il test
    /// parte da una richiesta che LA CONTIENE: se il dialetto smettesse di
    /// filtrarla, qui comparirebbe.
    #[tokio::test]
    async fn la_temperatura_non_parte_verso_kimi() {
        let corpo = corpo().await;
        assert!(
            corpo.get("temperature").is_none(),
            "temperature inviata a Kimi: e' un HTTP 400 su ogni chiamata"
        );
    }

    /// `max_tokens` e' deprecato: il tetto viaggia su `max_completion_tokens`.
    #[tokio::test]
    async fn il_tetto_di_output_usa_il_campo_non_deprecato() {
        let corpo = corpo().await;
        assert_eq!(
            corpo.get("max_completion_tokens").and_then(|v| v.as_u64()),
            Some(2048)
        );
        assert!(corpo.get("max_tokens").is_none());
    }

    /// Preserved Thinking: l'assistant torna indietro col proprio
    /// `reasoning_content`, come la doc prescrive.
    ///
    /// MISURATO il 09/08/2026 che l'API non rifiuta il turno che lo omette (a
    /// differenza di DeepSeek, che risponde 400): il campo si manda perche' su
    /// un modello che pensa sempre e' la continuita' del ragionamento fra un
    /// turno e il successivo, non una difesa da un errore.
    #[tokio::test]
    async fn il_pensiero_dell_assistant_torna_indietro() {
        let corpo = corpo().await;
        let messaggi = corpo["messages"].as_array().expect("array di messaggi");
        let assistant = messaggi
            .iter()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
            .expect("l'assistant e' nella history");
        assert_eq!(
            assistant.get("reasoning_content").and_then(|v| v.as_str()),
            Some("ho ragionato cosi'"),
            "il pensiero non e' tornato indietro: il modello riparte senza il proprio \
             ragionamento del turno precedente"
        );
    }

    /// Dal NOME del provider fino ai campi di affinita' sul wire: guarda la
    /// CONSEGUENZA, non il valore dell'enum. Un test su `cache_keying()`
    /// proverebbe solo che il costruttore ritorna cio' che c'e' scritto.
    ///
    /// `session_id` e `provider` NON devono partire: sono i campi che un
    /// instradatore verso terzi legge, e Moonshot serve i propri modelli — un
    /// campo sconosciuto e' il solo verso in cui questa scelta puo' fare danno.
    #[tokio::test]
    async fn la_chiave_di_cache_parte_e_i_campi_da_instradatore_no() {
        let corpo = corpo().await;
        assert!(
            corpo
                .get("prompt_cache_key")
                .and_then(|v| v.as_str())
                .is_some_and(|k| !k.is_empty()),
            "senza prompt_cache_key il fornitore raggruppa le richieste a caso"
        );
        assert!(corpo.get("session_id").is_none());
        assert!(corpo.get("provider").is_none());
    }

    #[test]
    fn capacita_dichiarate() {
        let p = provider();
        assert_eq!(p.name(), "kimi");
        assert!(p.supports_tools());
        assert!(p.supports_streaming());
        assert_eq!(p.max_context_tokens(), 1_048_576);
        assert_eq!(p.tier_compatibility(), &[0, 1, 2]);
    }
}
