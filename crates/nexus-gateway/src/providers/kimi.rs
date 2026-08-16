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
//! Gli altri tre quirk stanno in [`ReasoningDialect::Kimi`], che li documenta:
//! `max_completion_tokens` al posto del deprecato `max_tokens`, il round-trip
//! del `reasoning_content` preteso dal Preserved Thinking, e lo spegnimento del
//! pensiero — che questo modulo decide, perche' e' l'unico a sapere se il
//! modello lo consenta (vedi [`PensieroSpegnibile`]).
//!
//! CACHE: il fornitore cachea il prefisso da solo ("Context Caching is
//! automatically enabled for all model requests"), ma documenta anche il campo
//! `prompt_cache_key` come l'etichetta con cui raggruppare le richieste della
//! stessa sessione. E' la posizione di Mistral, e la si dichiara col vocabolario
//! gia' esistente: [`PromptCacheKeying::RequiresKey`].

use std::time::Duration;

use async_trait::async_trait;
use nexus_cache::TtlCache;
use reqwest::Client;
use sqlx::PgPool;

use crate::provider::{ChunkStream, LlmProvider};
use crate::providers::openai_compat::{OpenAiCompatClient, ReasoningDialect, ResolvedReasoning};
use crate::types::{LlmRequest, LlmResponse, PromptCacheKeying, SensitivityTier};

/// Tier ammessi: pubblico/interno/confidenziale. Mai il tier 3, riservato a chi
/// gira on-premise: Moonshot e' un fornitore cloud extra-UE come gli altri.
const TIERS: &[SensitivityTier] = &[0, 1, 2];

/// Nome del provider: identita' verso il registry, chiave di `ai_price_catalog` e
/// `provider_used` della risposta. Scritto una volta perche' le tre non possano
/// divergere — una query che cercasse un nome diverso da quello con cui il
/// provider e' costruito non troverebbe mai la riga, e in silenzio.
const PROVIDER_NAME: &str = "kimi";

/// TTL della disattivabilita' letta dal catalogo: 60s, come le altre cache di
/// configurazione del gateway (`policy_engine`, `cooldown`, affinita' upstream).
const CATALOG_TTL: Duration = Duration::from_secs(60);

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

/// Il fornitore accetta di SPEGNERE il ragionamento su questo modello?
///
/// Tre varianti e non un `bool` (regola Q): l'ignoto ha una conseguenza propria e
/// non deve degradare ne' a «si'» ne' a «no» per comodita' di chi legge. I due
/// errori non si equivalgono affatto — spegnere dove il fornitore non lo consente
/// e' un HTTP 400 su OGNI chiamata a quel modello (l'immagine speculare del
/// difetto della temperatura che questo driver gia' evita), mentre non spegnere
/// dove si potrebbe costa il tetto di output. Percio' solo [`Self::Si`] autorizza,
/// e [`Self::NonDichiarato`] si comporta come il codice si comportava prima che
/// il dato esistesse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PensieroSpegnibile {
    /// MISURATO: l'API accetta `thinking: {"type":"disabled"}`.
    Si,
    /// MISURATO: l'API risponde 400 «only type=enabled is allowed».
    No,
    /// Il catalogo non lo dice — riga assente, colonna `NULL`, o DB che non ha
    /// parlato. Non e' una misura, e non si spegne.
    NonDichiarato,
}

impl PensieroSpegnibile {
    /// Dalla colonna `ai_price_catalog.thinking_can_be_disabled` (mig 0705), dove
    /// il `NULL` della colonna e la riga assente collassano legittimamente nella
    /// stessa risposta: in entrambi i casi nessuno ha dichiarato nulla.
    fn dal_catalogo(letto: Option<bool>) -> Self {
        match letto {
            Some(true) => Self::Si,
            Some(false) => Self::No,
            None => Self::NonDichiarato,
        }
    }

    /// L'unico punto in cui questo fatto diventa un permesso.
    fn consente_lo_spegnimento(self) -> bool {
        matches!(self, Self::Si)
    }
}

/// Questa richiesta vuole una RISPOSTA o un RAGIONAMENTO?
///
/// Funzione pura: e' la VOLONTA' del chiamante, distinta dal fatto del fornitore
/// (`PensieroSpegnibile`) con cui viene incrociata. Due segnali, in quest'ordine:
///
/// 1. la preferenza ESPLICITA, quando c'e': chi si e' preso la briga di dirlo
///    comanda, in entrambi i versi (e' il canale che usa la batteria di
///    qualificazione per chiedere il pensiero acceso);
/// 2. in sua assenza, la presenza di TOOL. E' un run agentico: si vuole l'azione,
///    e il tetto di output e' li' per contenere una risposta, non un ragionamento.
///    E' la stessa traduzione al confine che [`super::deepseek`] fa gia' — li' per
///    evitare un 400, qui per evitare che il tetto se lo prenda il pensiero.
///
/// La volonta' sta QUI e non in `agentic_thinking_policy`, che pure ha il valore
/// giusto nel proprio vocabolario (`disable_for_tools`) e per kimi vale `'none'`:
/// quella colonna la RISCRIVE il catalog sync da un'euristica sul nome, che per
/// «kimi-k2.6» non trova alcun marcatore di reasoning. Un valore scritto li'
/// verrebbe cancellato dal primo giro di sync (motivazione estesa nella mig 0705).
///
/// Una lista di tool VUOTA non e' «ci sono tool»: senza tool veri il turno e'
/// testuale e vale il default del fornitore, come per DeepSeek.
fn vuole_risposta_e_non_ragionamento(req: &LlmRequest) -> bool {
    if let Some(t) = req.thinking.as_ref() {
        return !t.enabled;
    }
    req.tools.as_ref().is_some_and(|t| !t.is_empty())
}

pub struct KimiProvider {
    client: OpenAiCompatClient,
    /// Serve a leggere la disattivabilita' del pensiero dal catalogo. Opzionale:
    /// i test che esercitano la sola mappatura request/response non hanno DB, e
    /// senza si perde uno spegnimento, non la correttezza della chiamata.
    db: Option<PgPool>,
    /// Per MODELLO, perche' la risposta cambia per modello dentro lo stesso
    /// fornitore: e' l'intero punto di questo meccanismo. Vi entra solo cio' che
    /// si e' letto — un errore non e' una misura, e scriverlo qui
    /// cristallizzerebbe per 60s un'ignoranza momentanea.
    spegnibile: TtlCache<String, PensieroSpegnibile>,
}

impl KimiProvider {
    /// Costruisce il provider senza accesso DB: la disattivabilita' del pensiero
    /// non sara' leggibile e nessuna richiesta lo spegnera' (comportamento
    /// anteriore alla mig 0705).
    pub fn new(http: Client, api_key: impl Into<String>, base_url: Option<String>) -> Self {
        Self::with_db(http, api_key, base_url, None)
    }

    /// Costruisce il provider con accesso DB per leggere dal catalogo se il
    /// pensiero sia disattivabile su quel modello (regola G: il fatto sta nel
    /// dato, non in un riconoscimento sul nome scritto qui).
    pub fn with_db(
        http: Client,
        api_key: impl Into<String>,
        base_url: Option<String>,
        db: Option<PgPool>,
    ) -> Self {
        let base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Self {
            // TETTO: Moonshot dichiara `max_tokens` deprecato in favore di
            // `max_completion_tokens` (doc `/docs/api/chat`). E' un fatto del
            // FORNITORE e sta sul client, non sul dialetto reasoning — che
            // continua a governare temperatura, round-trip e spegnimento.
            client: OpenAiCompatClient::new(http, base_url, api_key, PROVIDER_NAME)
                .with_prompt_cache_keying(PromptCacheKeying::RequiresKey)
                .with_tetto_su_completion(),
            db,
            spegnibile: TtlCache::new(CATALOG_TTL),
        }
    }

    /// Se il fornitore consenta di spegnere il pensiero su questo modello, dal
    /// catalogo (mig 0705).
    ///
    /// Ogni via di fuga porta a [`PensieroSpegnibile::NonDichiarato`], e sempre
    /// nella stessa direzione: non spegnere. Nessun DB agganciato, riga assente,
    /// colonna `NULL`, query fallita — in nessuno di questi casi qualcuno ha
    /// misurato alcunche', e il costo dell'errore non e' simmetrico. Un guasto
    /// del DB piu' lungo della TTL riporta al comportamento di ieri, che e'
    /// sicuro; la direzione opposta manderebbe un `disabled` a un modello che
    /// risponde 400.
    async fn pensiero_spegnibile(&self, model: &str) -> PensieroSpegnibile {
        if let Some(v) = self.spegnibile.get(model) {
            return v;
        }
        let Some(db) = self.db.as_ref() else {
            return PensieroSpegnibile::NonDichiarato;
        };
        // `Option<Option<bool>>`: il primo livello e' la riga, il secondo la
        // colonna nullable. Entrambi gli assenti dicono la stessa cosa.
        let letto: Result<Option<(Option<bool>,)>, sqlx::Error> = sqlx::query_as(
            "SELECT thinking_can_be_disabled FROM ai_price_catalog \
             WHERE provider = $1 AND model = $2",
        )
        .bind(PROVIDER_NAME)
        .bind(model)
        .fetch_optional(db)
        .await;
        match letto {
            Ok(row) => {
                let esito = PensieroSpegnibile::dal_catalogo(row.and_then(|(v,)| v));
                self.spegnibile.insert(model.to_string(), esito);
                esito
            }
            Err(e) => {
                // Regola F: nei campi solo identificatori di configurazione e la
                // causa strutturata dell'errore, mai il payload.
                tracing::warn!(
                    provider = PROVIDER_NAME,
                    model = %model,
                    error = %e,
                    "disattivabilita' del pensiero non leggibile dal catalogo: \
                     il ragionamento resta acceso e puo' consumare il tetto di output"
                );
                PensieroSpegnibile::NonDichiarato
            }
        }
    }

    /// Il dialetto e' SEMPRE `Kimi`, senza condizioni sul modello o sulla
    /// richiesta: i quirk di forma che porta con se' (niente temperatura,
    /// `max_completion_tokens`, round-trip del pensiero) valgono per l'intero
    /// parco moderno del fornitore.
    ///
    /// `enabled` e' l'unico campo che varia, ed e' la CONGIUNZIONE di due cose
    /// che restano distinte fino a qui: cosa vuole il chiamante
    /// ([`vuole_risposta_e_non_ragionamento`], pura) e cosa il fornitore consente
    /// ([`Self::pensiero_spegnibile`], dal catalogo). Il pensiero si spegne solo
    /// se entrambe lo dicono; in ogni altro caso resta il default del fornitore,
    /// che e' acceso.
    ///
    /// `effort` resta neutro: e' accettato dal solo `kimi-k3`, nessun chiamante lo
    /// esprime oggi, e un ramo senza produttore sarebbe codice morto (regola O).
    async fn resolve_reasoning(&self, req: &LlmRequest) -> ResolvedReasoning {
        let spegnere = vuole_risposta_e_non_ragionamento(req)
            && self
                .pensiero_spegnibile(&req.model)
                .await
                .consente_lo_spegnimento();
        ResolvedReasoning {
            dialect: ReasoningDialect::Kimi,
            enabled: !spegnere,
            effort: None,
        }
    }
}

#[async_trait]
impl LlmProvider for KimiProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
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
        let reasoning = self.resolve_reasoning(req).await;
        self.client.complete_with_reasoning(req, &reasoning).await
    }

    async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        let reasoning = self.resolve_reasoning(req).await;
        self.client.stream_with_reasoning(req, &reasoning).await
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
    use crate::types::{
        LlmMessage, LlmToolDefinition, MessageContent, RequestMetadata, ThinkingConfig,
        ToolFunctionDef,
    };

    fn provider() -> KimiProvider {
        KimiProvider::new(Client::new(), "chiave", None)
    }

    fn provider_con_catalogo(pool: PgPool) -> KimiProvider {
        KimiProvider::with_db(Client::new(), "chiave", None, Some(pool))
    }

    /// Un tool qualunque: serve solo a rendere la richiesta un turno agentico.
    fn tool_qualunque() -> LlmToolDefinition {
        LlmToolDefinition {
            kind: "function".to_string(),
            function: ToolFunctionDef {
                name: "read_file".to_string(),
                description: None,
                parameters: serde_json::json!({"type": "object", "properties": {}}),
                strict: None,
            },
        }
    }

    /// Turno agentico su un modello preciso: e' la forma in cui il difetto e'
    /// stato misurato — tool presenti, tetto stretto, e il pensiero che se lo
    /// prende tutto.
    fn richiesta_agentica(model: &str) -> LlmRequest {
        let mut req = richiesta();
        req.model = model.to_string();
        req.tools = Some(vec![tool_qualunque()]);
        req
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
    async fn corpo_di(p: &KimiProvider, req: &LlmRequest) -> serde_json::Value {
        serde_json::to_value(
            p.client
                .corpo_della_richiesta(req, false, &p.resolve_reasoning(req).await)
                .await,
        )
        .expect("il corpo serializza")
    }

    async fn corpo() -> serde_json::Value {
        corpo_di(&provider(), &richiesta()).await
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

    // ── spegnimento del pensiero ──────────────────────────────────────────────

    /// La VOLONTA', da sola. Pura: non tocca il catalogo, quindi misura solo il
    /// segnale del chiamante e non lo confonde col permesso del fornitore.
    #[test]
    fn la_volonta_viene_dal_chiamante_e_poi_dai_tool() {
        // Turno testuale senza preferenza: default del fornitore (pensiero acceso).
        assert!(!vuole_risposta_e_non_ragionamento(&richiesta()));

        // Run agentico: si vuole l'azione, e il tetto deve contenere la risposta.
        assert!(vuole_risposta_e_non_ragionamento(&richiesta_agentica(
            "kimi-k2.6"
        )));

        // Una lista di tool VUOTA non e' «ci sono tool»: fissa il confine del ramo.
        let mut vuota = richiesta();
        vuota.tools = Some(vec![]);
        assert!(!vuole_risposta_e_non_ragionamento(&vuota));

        // La preferenza esplicita comanda, in ENTRAMBI i versi, anche contro i
        // tool: e' il canale con cui la batteria di qualificazione chiede il
        // pensiero acceso, e un run agentico non deve poterglielo negare.
        let mut chiede_pensiero = richiesta_agentica("kimi-k2.6");
        chiede_pensiero.thinking = Some(ThinkingConfig {
            enabled: true,
            budget_tokens: None,
            mandatory: false,
        });
        assert!(!vuole_risposta_e_non_ragionamento(&chiede_pensiero));

        let mut chiede_risposta = richiesta();
        chiede_risposta.thinking = Some(ThinkingConfig {
            enabled: false,
            budget_tokens: None,
            mandatory: false,
        });
        assert!(vuole_risposta_e_non_ragionamento(&chiede_risposta));
    }

    /// Senza catalogo non si spegne NIENTE, nemmeno dove il fornitore lo
    /// consentirebbe: il permesso e' un dato, e in sua assenza vale il
    /// comportamento anteriore alla mig 0705.
    #[tokio::test]
    async fn senza_catalogo_il_pensiero_resta_acceso() {
        let p = provider();
        assert_eq!(
            p.pensiero_spegnibile("kimi-k2.6").await,
            PensieroSpegnibile::NonDichiarato
        );
        let corpo = corpo_di(&p, &richiesta_agentica("kimi-k2.6")).await;
        assert!(
            corpo.get("thinking").is_none(),
            "senza il dato non si dichiara nulla sul pensiero"
        );
    }

    /// IL test del difetto: il permesso arriva dal CATALOGO, per modello, e la
    /// conseguenza si guarda sul corpo che parte davvero.
    ///
    /// I quattro verdetti vengono dalla mig 0705, non da un inserimento di
    /// comodo: sono cio' che l'API ha risposto il 13/08/2026, e se qualcuno
    /// cambia quella migrazione questo test se ne accorge (regola O). Il corpo
    /// passa da `resolve_reasoning` e `corpo_della_richiesta` reali: costruire un
    /// `ResolvedReasoning` a mano fisserebbe qui l'assunto da verificare.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_pensiero_si_spegne_solo_dove_il_fornitore_lo_consente(pool: PgPool) {
        let p = provider_con_catalogo(pool);

        // MISURATO: `thinking: {"type":"disabled"}` accettato (reasoning 575 -> 1).
        for consentito in ["kimi-k2.6", "kimi-k3"] {
            assert_eq!(
                p.pensiero_spegnibile(consentito).await,
                PensieroSpegnibile::Si,
                "{consentito}: la 0705 lo dichiara disattivabile"
            );
            let corpo = corpo_di(&p, &richiesta_agentica(consentito)).await;
            assert_eq!(
                corpo["thinking"]["type"], "disabled",
                "{consentito}: senza questo campo il tetto se lo prende il \
                 ragionamento (content vuoto a 1024, 214,8s a 8192)"
            );
        }

        // MISURATO: HTTP 400 «only type=enabled is allowed». Spegnere qui
        // sarebbe un errore su OGNI chiamata a quel modello.
        for vietato in ["kimi-k2.7-code", "kimi-k2.7-code-highspeed"] {
            assert_eq!(
                p.pensiero_spegnibile(vietato).await,
                PensieroSpegnibile::No,
                "{vietato}: la 0705 lo dichiara NON disattivabile"
            );
            let corpo = corpo_di(&p, &richiesta_agentica(vietato)).await;
            assert!(
                corpo.get("thinking").is_none(),
                "{vietato}: un thinking esplicito qui e' un 400 sistematico"
            );
        }

        // Un modello che il catalogo non conosce non autorizza niente: l'ignoto
        // non degrada a permesso.
        assert_eq!(
            p.pensiero_spegnibile("kimi-k9-mai-vista").await,
            PensieroSpegnibile::NonDichiarato
        );
        let corpo = corpo_di(&p, &richiesta_agentica("kimi-k9-mai-vista")).await;
        assert!(corpo.get("thinking").is_none());
    }

    /// Il permesso non basta: senza la volonta' il pensiero resta acceso. Prova
    /// che i due segnali sono davvero in congiunzione e che il turno testuale su
    /// un modello disattivabile non cambia comportamento.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_permesso_da_solo_non_spegne_niente(pool: PgPool) {
        let p = provider_con_catalogo(pool);
        let mut req = richiesta();
        req.model = "kimi-k2.6".to_string();
        assert!(req.tools.is_none(), "premessa: turno testuale");

        let corpo = corpo_di(&p, &req).await;
        assert!(
            corpo.get("thinking").is_none(),
            "nessuno ha chiesto una risposta secca: vale il default del fornitore"
        );
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
