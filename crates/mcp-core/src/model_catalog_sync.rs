//! Worker `model_catalog_sync` — fix Bug 7 (audit 26/05/2026).
//!
//! Mantiene `ai_price_catalog` allineato con i modelli realmente esposti
//! dalle API dei provider. Senza questo, quando un provider deprecava un
//! modello (es. DeepSeek v3 -> v4) il catalog restava stale per settimane e
//! gli agent_run fallivano con "hollow completion" perche' chiamavano modelli
//! inesistenti lato provider, sprecando token e degradando l'UX.
//!
//! Flusso ogni N ore (settings `catalog_sync.interval_hours`, default 6):
//!   1. Lista provider DEDOTTA dal `nexus_provider_registry` (is_active +
//!      configurato) via [`providers_da_sincronizzare`] — niente CSV hardcoded
//!   2. Per ogni provider: GET {gateway}/v1/models/{provider} (via UNICA per
//!      la discovery — il gateway incapsula l'auth di ogni provider, Vertex
//!      Service Account incluso). Poi confronta con catalog: INSERT nuovi
//!      (is_enabled=false, prezzi 0 da raffinare manualmente), DISABLE modelli
//!      non piu' esposti.
//!   3. Audit ogni delta in `ai_price_catalog_audit`
//!   4. Emit notification dispatcher 'CatalogModelChanged' per admin
//!
//! Sorgente UNICA dei modelli live: il Nexus Gateway (regola L). Il worker NON
//! chiama piu' direttamente gli endpoint `api.{provider}.com/v1/models` ne'
//! delega al brain per Google: tutta l'auth (API key cloud, Vertex Service
//! Account) e' gia' nel gateway, che espone `GET /v1/models/{provider}` per
//! tutti i provider. Vedi `NexusGatewayClient::list_models`.
//!
//! Provider locali (ollama/vllm): skipped, catalog manuale per setup custom.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sqlx::PgPool;

use crate::model_health_probe::{probe_model_on_insert, ProbeOnInsertResult};
use crate::orchestrator::Orchestrator;
use crate::settings::get_setting;

const DEFAULT_INTERVAL_HOURS: u64 = 6;

/// True se il nome modello (gia' lowercase) rientra nella blacklist STRUTTURALE
/// dei modelli che per design NON sono chat completion (TTS/transcribe/embedding/
/// realtime/instruct legacy/image-gen/moderation). Estratta da
/// `is_chat_compatible_model` (regola L) senza cambi di comportamento: la
/// valutazione resta l'unione delle quattro blacklist per posizione (substring/
/// infix/prefix/suffix), nello stesso ordine.
fn matches_incompatible_blacklist(lower: &str) -> bool {
    const SUBSTRING_BLACKLIST: &[&str] = &[
        "voxtral",
        "whisper",
        "embedding",
        "moderation",
        "unknown-provider",
    ];
    if SUBSTRING_BLACKLIST.iter().any(|bad| lower.contains(bad)) {
        return true;
    }
    const INFIX_BLACKLIST: &[&str] = &[
        "-tts-",
        "-transcribe-",
        "-realtime-",
        "-instruct-",
        "-unknown-",
    ];
    if INFIX_BLACKLIST.iter().any(|bad| lower.contains(bad)) {
        return true;
    }
    const PREFIX_BLACKLIST: &[&str] = &[
        "tts-",
        "dall-e",
        "dalle-",
        "imagen",
        "instruct-",
        "babbage",
        "davinci-00",
        "text-embedding",
    ];
    if PREFIX_BLACKLIST.iter().any(|bad| lower.starts_with(bad)) {
        return true;
    }
    const SUFFIX_BLACKLIST: &[&str] = &["-tts", "-transcribe", "-realtime", "-embed", "-instruct"];
    SUFFIX_BLACKLIST.iter().any(|bad| lower.ends_with(bad))
}

/// Filtro chat-compatibilita': i provider espongono nelle loro `/v1/models` API
/// anche modelli specializzati (voice, TTS, transcribe, embedding, instruct
/// legacy, image generation, modelli "preview" hollow) che NON sono usabili
/// dalla chat agentic di Nexus. Senza filtro, il catalog viene inquinato e
/// il routing puo' selezionarli, generando errori in cascata.
///
/// Ritorna `true` se il modello e' un valido modello di chat completion.
fn is_chat_compatible_model(model: &str) -> bool {
    let lower = model.to_lowercase();
    if matches_incompatible_blacklist(&lower) {
        return false;
    }
    // Nota: NESSUNA blacklist per nome modello/famiglia (es. ex-blacklist
    // hardcoded di gemini-3.x). I modelli "fantasma" (esposti dall'API
    // discovery ma rispondono 404/hollow in inference) sono ora rilevati
    // dinamicamente dal `probe_model_on_insert` chiamato qui sotto al primo
    // discovery. Self-healing: quando un modello placeholder viene poi davvero
    // rilasciato dal provider, il probe successivo passa e il modello viene
    // riabilitato automaticamente dal worker `model_health_probe`. La blacklist
    // strutturale qui sopra (TTS/embedding/realtime/instruct/imagen) resta:
    // sono modelli che per design NON sono chat completion, il probe sarebbe
    // solo rumore.
    true
}

/// PUNTO UNICO (regola L) della classificazione MEDIA di un modello dal nome.
///
/// I provider espongono nelle loro `/v1/models` API anche modelli specializzati
/// in generazione/comprensione di media (immagini, audio, video) che NON sono
/// chat completion ma vanno comunque gestiti dal catalog per essere instradati
/// PER CAPABILITY (image generation, trascrizione, sintesi vocale, ecc.). Questa
/// funzione e' la sorgente UNICA delle regole "nome -> media kind": le STESSE
/// regex sono replicate nel backfill della migrazione 0478 (un solo posto sa
/// quali nomi sono media; il seed SQL e il codice condividono le regole).
///
/// Ritorna il media kind canonico (`"image_gen"`, `"audio_in"`, `"audio_out"`,
/// `"video_gen"`) o `None` se il modello NON e' un media (chat/vision/embedding/
/// realtime/instruct/moderation restano fuori da qui). Le colonne canoniche
/// corrispondenti sono `supports_image_gen|audio_in|audio_out|video_gen`
/// (mig 0478), gemelle di `supports_vision` (mig 0318).
///
/// Niente nome modello hardcoded come model-id di business (regola G): qui si
/// classifica per FAMIGLIA/naming, come `is_chat_compatible_model` e
/// `classify_capabilities`. L'ordine dei controlli e' per specificita': image
/// prima (gpt-image vs gpt chat), poi audio_in (whisper/transcribe), poi
/// audio_out (tts), poi video (veo/sora).
pub(crate) fn classify_media_kind(model: &str) -> Option<&'static str> {
    let m = model.to_ascii_lowercase();

    // image_gen: dall-e / dalle / imagen / gpt-image / *-image* / nano-banana.
    //   - "dall-e" e "dalle-" (OpenAI), "imagen" (Google), "gpt-image" (OpenAI),
    //   - suffisso/infix "-image" (es. *-image, *-image-1), "nano-banana" (Gemini).
    let is_image = m.contains("dall-e")
        || m.contains("dalle")
        || m.contains("imagen")
        || m.contains("gpt-image")
        || m.ends_with("-image")
        || m.contains("-image-")
        || m.contains("nano-banana");
    if is_image {
        return Some("image_gen");
    }

    // audio_in: trascrizione / speech-to-text. whisper, *-transcribe, transcribe-*,
    //           voxtral (Mistral audio understanding).
    let is_audio_in = m.contains("whisper")
        || m.contains("-transcribe")
        || m.contains("transcribe-")
        || m.contains("voxtral");
    if is_audio_in {
        return Some("audio_in");
    }

    // audio_out: text-to-speech. suffisso "-tts", prefisso "tts-", infix "-tts-".
    let is_audio_out = m.ends_with("-tts") || m.starts_with("tts-") || m.contains("-tts-");
    if is_audio_out {
        return Some("audio_out");
    }

    // video_gen: veo (Google), sora (OpenAI).
    let is_video = m.contains("veo") || m.contains("sora");
    if is_video {
        return Some("video_gen");
    }

    None
}

/// Mappa il media kind canonico alla colonna booleana di `ai_price_catalog`
/// (punto unico, regola L: un solo posto traduce kind -> colonna, sia qui per
/// il sync sia, specularmente, in `model_selection.rs` per la WHERE). Ritorna
/// `None` per un kind non riconosciuto (impossibile dai chiamanti interni che
/// passano l'output di `classify_media_kind`, ma fail-safe).
pub(crate) fn media_kind_column(kind: &str) -> Option<&'static str> {
    match kind {
        "image_gen" => Some("supports_image_gen"),
        "audio_in" => Some("supports_audio_in"),
        "audio_out" => Some("supports_audio_out"),
        "video_gen" => Some("supports_video_gen"),
        _ => None,
    }
}

/// INSERT di un modello MEDIA scoperto via API discovery. Default sicuro:
/// `is_enabled=false` (richiede abilitazione esplicita, come i chat nuovi),
/// `supports_tool_use=false` (un media non e' agentico),
/// `pricing_state='unknown'` (costo placeholder, regola H: non e' "gratis").
/// Il flag `supports_<media>` corrispondente al `kind` viene messo a TRUE.
/// Ritorna `Some(1)` se la riga e' stata inserita, `None` altrimenti.
async fn insert_media_model(
    db: &PgPool,
    provider: &str,
    api_model: &str,
    kind: &str,
) -> Option<u32> {
    let Some(col) = media_kind_column(kind) else {
        tracing::warn!(
            "catalog_sync[{}]: media kind sconosciuto '{}' per '{}', skip insert",
            provider,
            kind,
            api_model
        );
        return None;
    };
    // performance_tier NULL (mig 0599): un modello media (image/audio/video) nasce
    // senza prezzo (`pricing_state='unknown'`) e senza tool: non c'e' alcun fatto
    // su cui fondare una fascia, e il tier agentico non lo riguarda nemmeno.
    // Prima ereditava una fascia indovinata dal NOME, che per un image-gen non
    // significa niente.
    //
    // Colonna interpolata SOLO da `media_kind_column` (whitelist statica, niente
    // input utente): nessuna SQL injection. I valori restano bind parametrici.
    let sql = format!(
        "INSERT INTO ai_price_catalog \
         (provider, model, display_name, input_cost_per_million_tokens, \
          output_cost_per_million_tokens, currency, capabilities, performance_tier, \
          is_enabled, supports_tool_use, {col}, pricing_state, effective_from) \
         VALUES ($1, $2, $3, 0, 0, 'USD', '[]'::jsonb, NULL, false, false, TRUE, 'unknown', NOW()) \
         ON CONFLICT (provider, model) DO NOTHING"
    );
    match sqlx::query(&sql)
        .bind(provider)
        .bind(api_model)
        .bind(api_model)
        .execute(db)
        .await
    {
        Ok(r) if r.rows_affected() > 0 => {
            on_media_model_inserted(db, provider, api_model, kind).await;
            Some(1)
        }
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(
                "catalog_sync[{}]: insert media '{}' fallito: {}",
                provider,
                api_model,
                e
            );
            None
        }
    }
}

/// Coda dell'INSERT riuscito di un modello media: audit + log. Estratta da
/// `insert_media_model` senza cambi di comportamento.
async fn on_media_model_inserted(db: &PgPool, provider: &str, api_model: &str, kind: &str) {
    audit_log(
        db,
        provider,
        api_model,
        "inserted",
        json!({"source": "api_discovery", "media_kind": kind}),
    )
    .await;
    tracing::info!(
        "catalog_sync[{}]: + nuovo modello MEDIA '{}' (kind={}, disabled, no probe)",
        provider,
        api_model,
        kind
    );
}

/// Riallinea il flag `supports_<media>` di un media GIA' presente, sulle sole
/// righe `capability_source='auto'` (le 'manual' restano intatte). Stesso
/// principio del riallineamento `supports_vision` per i chat (regola H+L):
/// una correzione della classificazione media si propaga ai modelli esistenti
/// senza migrazione-dati manuale.
async fn realign_media_flags(db: &PgPool, provider: &str, api_model: &str, kind: &str) {
    let Some(col) = media_kind_column(kind) else {
        return;
    };
    // Colonna da whitelist statica (media_kind_column): niente SQL injection.
    let sql = format!(
        "UPDATE ai_price_catalog SET {col} = TRUE, updated_at = NOW() \
         WHERE provider = $1 AND model = $2 AND capability_source = 'auto' \
           AND {col} <> TRUE"
    );
    if let Err(e) = sqlx::query(&sql)
        .bind(provider)
        .bind(api_model)
        .execute(db)
        .await
    {
        tracing::warn!(
            "catalog_sync[{}]: realign media flag '{}' fallito: {}",
            provider,
            api_model,
            e
        );
    }
}

/// Inferisce le capabilities di un modello dal nome quando l'API discovery
/// non le ritorna esplicite (caso comune per Google Vertex). Restituisce una
/// lista pronta per `capabilities` JSONB di ai_price_catalog.
///
/// Bug osservato 30/05/2026: `gemini-3.1-pro-preview-customtools` passa il
/// probe-on-insert (HTTP 200) ma resta con capabilities=[]. Il routing filtra
/// per capability matching e quel modello (potenzialmente il piu' capable di
/// Vertex per tool use) non viene mai scelto. Soluzione: dopo il probe OK,
/// se capabilities e' vuoto, popolare con una stima euristica basata sul nome.
///
/// Regole (in ordine di specificita'):
///   - *-pro / *-pro-preview / *-pro-* (es. customtools)  -> reasoning/code/long-context
///   - *-flash-lite                                        -> chat/simple
///   - *-flash / *-flash-preview                           -> code/chat/fix
///   - *-opus / *-sonnet                                   -> reasoning/code/long-context
///   - *-haiku                                             -> chat/simple
///   - *-codex / *-codestral / *-devstral                  -> code
///   - default                                             -> chat (sempre safe)
///
/// Restituisce sempre almeno `["chat"]`. Non infersce vision/image/audio
/// (queste richiedono detection esplicita lato provider).
pub(crate) fn infer_capabilities_from_name(provider: &str, model: &str) -> Vec<&'static str> {
    let m = model.to_ascii_lowercase();
    // Aggiungi marker tool_use_optimized per modelli con suffisso "customtools".
    let mut caps: Vec<&'static str> = Vec::new();
    if m.contains("customtools") {
        caps.push("tool_use_optimized");
    }
    for c in base_caps_from_name(provider, &m) {
        if !caps.contains(c) {
            caps.push(c);
        }
    }
    caps
}

/// Capability "base" (senza marker `customtools`) di un modello dal nome, per
/// FAMIGLIA/provider. `m` gia' lowercase. Estratta da
/// `infer_capabilities_from_name` (regola L) senza cambi di comportamento.
/// Ritorna sempre almeno `["chat"]`.
fn base_caps_from_name(provider: &str, m: &str) -> &'static [&'static str] {
    match provider {
        "google" => {
            if m.contains("pro") {
                &["reasoning", "code", "long-context", "chat"]
            } else if m.contains("flash-lite") {
                &["chat", "simple"]
            } else if m.contains("flash") {
                &["code", "chat", "fix"]
            } else {
                &["chat"]
            }
        }
        "anthropic" => {
            if m.contains("opus") || m.contains("sonnet") {
                &["reasoning", "code", "long-context", "chat"]
            } else if m.contains("haiku") {
                &["chat", "simple"]
            } else {
                &["chat"]
            }
        }
        "openai" => {
            if m.contains("codex") {
                &["code", "chat"]
            } else if m.contains("nano") || m.contains("mini") {
                &["chat", "simple"]
            } else {
                &["reasoning", "code", "chat"]
            }
        }
        "mistral" => {
            if m.contains("codestral") || m.contains("devstral") {
                &["code", "chat"]
            } else if m.contains("large") || m.contains("magistral") {
                &["reasoning", "code", "chat"]
            } else if m.contains("medium") {
                &["code", "chat"]
            } else {
                &["chat"]
            }
        }
        "deepseek" => {
            if m.contains("pro") || m.contains("reasoner") {
                &["reasoning", "code", "chat"]
            } else {
                &["code", "chat"]
            }
        }
        _ => &["chat"],
    }
}









/// Flag di capability canonici di un modello (colonne reali di ai_price_catalog).
/// Vedi ADR 0024 e migrazione 0318: il catalog e' l'UNICA fonte; il brain li
/// legge derivati via vista `v_model_capabilities`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClassifiedCaps {
    /// Il modello sa invocare tool (function-calling).
    pub supports_tool_use: bool,
    /// Il modello accetta input immagine (vision).
    pub supports_vision: bool,
    /// Gira in thinking/extended-thinking mode -> l'adapter NON deve forzare
    /// tool_choice e abilita il budget di ragionamento. Include i reasoning-only
    /// e gli ibridi come Claude opus/sonnet. L'ESCLUSIONE dal routing agentico
    /// (l'ex "concetto A", colonna `is_thinking` rimossa in mig 0608) e' espressa
    /// SOLO da `agentic_thinking_policy` ('exclude'/'native'), ADR 0025.
    pub uses_thinking_mode: bool,
    /// Policy d'uso nei run agentici (ADR 0025), driver canonico dell'eleggibilita'
    /// e del toggle modalita': none | disable_for_tools | native | exclude.
    pub agentic_thinking_policy: &'static str,
}

/// Classificatore UNICO delle capability di un modello (ADR 0024).
///
/// Invocato da OGNI path di aggiornamento del catalog (sync LiteLLM in
/// `models::run_catalog_sync` e discovery provider in `sync_provider`), cosi'
/// quando i modelli si aggiornano la classificazione si aggiorna con loro.
/// Usa i metadata espliciti quando disponibili (LiteLLM: function_calling,
/// vision, reasoning); altrimenti euristica sul nome. Niente nome modello
/// hardcoded nella logica di business: qui classifichiamo per FAMIGLIA.
///
/// Le righe `capability_source='manual'` NON vengono toccate dai chiamanti
/// (guard SQL nell'UPSERT): questa funzione produce solo il default 'auto'.
pub(crate) fn classify_capabilities(
    provider: &str,
    model: &str,
    meta_tool_use: Option<bool>,
    meta_vision: Option<bool>,
    meta_reasoning: Option<bool>,
    // Provider instradabili da /vision/describe (punto unico DB-driven, setting
    // `vision.routable_providers`, mig 0373). Letto dal chiamante e passato qui:
    // la funzione resta pura/sincrona/testabile. supports_vision=true SOLO se il
    // provider e' in questo set (regola G/L: niente lista hardcoded duplicata).
    vision_routable: &std::collections::HashSet<String>,
) -> ClassifiedCaps {
    let p = provider.to_ascii_lowercase();
    let m = model.to_ascii_lowercase();

    // ── Un modello MEDIA (whisper/tts/dall-e/veo, punto unico
    //    `classify_media_kind`) non e' un modello di testo: mai tool_use, mai
    //    thinking. Senza questo guard il default `meta_tool_use.unwrap_or(true)`
    //    del path LiteLLM marcava whisper-1/tts-1 come tool-capable (dato
    //    sporco misurato il 16/07: modelli audio candidabili come consiglieri;
    //    ripulito dalla mig 0608 — questo guard evita il rientro al sync
    //    successivo). ──
    if classify_media_kind(&m).is_some() {
        return ClassifiedCaps {
            supports_tool_use: false,
            supports_vision: false,
            uses_thinking_mode: false,
            agentic_thinking_policy: "none",
        };
    }

    // ── tool_use: metadata esplicito, altrimenti default true (la stragrande
    //    maggioranza dei modelli chat moderni supporta function calling). ──
    //    ECCEZIONE per FAMIGLIA (regola L): i magistral sono la linea reasoning
    //    di Mistral e SUPPORTANO function calling (docs.mistral.ai/capabilities/
    //    function_calling elenca Magistral Small/Medium 1.2 tra i modelli
    //    function-calling), ma il metadata LiteLLM e' INCOERENTE tra varianti
    //    (alcune ritornano tool_use=false). Classifichiamo per famiglia con un
    //    valore unico (tool_use=true), ignorando il metadata per-modello
    //    inaffidabile. Verificato live: i magistral chiamano i tool col
    //    tool_choice forzato MANTENENDO il reasoning attivo (non serve, ne' e'
    //    possibile, spegnerlo: reasoning_effort non e' supportato dai magistral).
    //    Stessa eccezione per FAMIGLIA per DeepSeek V4 (v4-pro, v4-flash):
    //    verificato live (2026-06-10, probe API diretto) che ENTRAMBI eseguono
    //    function calling correttamente (finish_reason=tool_calls, tool call ben
    //    formata, reasoning attivo insieme ai tool; nel tool-loop l'adapter li fa
    //    girare non-thinking via extra_body.thinking=disabled, policy
    //    'disable_for_tools'). Il FALSE storico nel catalog era un degrado
    //    runtime (malformed_tool_calls da run hollow) scritto senza guard
    //    capability_source, non una verita' del provider.
    let supports_tool_use =
        if (p == "mistral" && m.contains("magistral")) || (p == "deepseek" && m.contains("v4")) {
            true
        } else {
            meta_tool_use.unwrap_or(true)
        };

    // ── vision: riflette l'instradabilita' REALE da brain/grpc_server/routes/
    //    vision.py. La lista dei provider instradabili NON e' piu' hardcoded ma
    //    vive nel setting DB `vision.routable_providers` (mig 0373), passato qui
    //    in `vision_routable` (punto unico DB-driven, regola G/L). Il metadata
    //    `meta_vision` di LiteLLM e' lasco ("accetta immagini in input") e marca
    //    falsi positivi (es. mistral-small) che il routing per tier sceglierebbe
    //    per vision_describe -> /vision/describe 501. Quindi: vision=true SOLO se
    //    il provider e' routable; l'euristica/metadata per-modello distingue poi
    //    QUALI modelli del provider hanno vision.
    let supports_vision = if vision_routable.contains(p.as_str()) {
        match p.as_str() {
            "openai" => meta_vision.unwrap_or_else(|| {
                m.starts_with("gpt-4o")
                    || m.starts_with("gpt-4.1")
                    || m.starts_with("gpt-4-turbo")
                    || m.starts_with("o1")
                    || m.starts_with("o3")
            }),
            "anthropic" => meta_vision.unwrap_or_else(|| {
                m.contains("opus") || m.contains("sonnet") || m.contains("haiku")
            }),
            "google" => meta_vision.unwrap_or_else(|| m.contains("gemini")),
            // Provider routable senza euristica per-modello nota: usa il metadata.
            _ => meta_vision.unwrap_or(false),
        }
    } else {
        // Provider senza ramo in vision.py: non instradabile -> niente vision.
        false
    };

    // ── Detection reasoning model (guida sia A sia B). ──
    // Famiglie reasoning-only note (incompatibili col tool-forcing o col loop
    // multi-turno nell'integrazione attuale).
    let reasoning_only_family = match p.as_str() {
        // OpenAI o-series: o1/o3/o4 (prefisso "o" + cifra).
        "openai" => m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4"),
        // DeepSeek reasoner e V4 (richiede reasoning_content passback non
        // implementato -> non regge il loop agentico, vedi mig 0317).
        "deepseek" => m.contains("reasoner") || m.contains("v4"),
        // Mistral magistral: linea reasoning.
        "mistral" => m.contains("magistral"),
        // Google: i Gemini 2.5 NON sono reasoning-only da escludere. Sono
        // dual-mode tool-capable e si gestiscono via agentic_thinking_policy
        // ='disable_for_tools' (gestito sotto da `gemini_25_thinking`): girano
        // non-thinking nei tool-loop, evitando il MALFORMED_FUNCTION_CALL.
        "google" => false,
        _ => false,
    };
    // Gemini 2.5 (pro E flash, ESCLUSO flash-lite) ha il thinking attivo di
    // default: col function calling in modalita' thinking produce
    // MALFORMED_FUNCTION_CALL (mig 0274). Va trattato come dual-mode -> tool-capable
    // + policy 'disable_for_tools' (non-thinking nei tool-loop), NON escluso
    // dall'agentico. Bug storico: l'euristica precedente assumeva che solo i
    // *-pro fossero thinking ("flash NON lo sono"), lasciando gemini-2.5-flash
    // con policy 'none' -> thinking attivo coi tool -> MALFORMED ad ogni run.
    // flash-lite NON ha thinking di default: resta policy 'none'.
    let gemini_25_thinking = p == "google" && m.contains("gemini-2.5") && !m.contains("flash-lite");
    // Marker generici nel nome, cross-provider.
    let name_reasoning = m.contains("reasoner")
        || m.contains("reasoning")
        || m.contains("thinking")
        || reasoning_only_family;
    // Metadata esplicito reasoning (LiteLLM) ha priorita' come segnale positivo.
    let is_reasoning_signal = meta_reasoning.unwrap_or(false) || name_reasoning;

    // Non forzare tool_choice: tutti i reasoning + gli ibridi con
    // extended thinking (Claude opus/sonnet) + i Gemini 2.5 thinking.
    // L'esclusione dal routing agentico dei reasoning-only e' espressa dalla
    // sola `agentic_thinking_policy` sotto (ex colonna is_thinking, mig 0608).
    let uses_thinking_mode = is_reasoning_signal || is_hybrid_agentic(&p, &m) || gemini_25_thinking;

    // Policy agentica canonica (ADR 0025):
    //   - exclude: NON eleggibile per i run agentici (tool-loop). Due casi:
    //     (1) reasoning-only SENZA function calling (deepseek-reasoner);
    //     (2) modelli COMPLETION/CHAT LEGACY tool-capable ma inadatti ai tool-loop
    //         complessi (deepseek-coder = V2 completion, deepseek-chat = V3). Sui
    //         task a tier light + capability 'code' (fix/fix_semplice/test) questi
    //         'none' legacy scavalcavano i V4 reasoning (causa radice "agentic usa
    //         deepseek-coder"). Restano eleggibili per i path NON agentici.
    //   - native:  reasoning con tool nativi (OpenAI o-series).
    //   - disable_for_tools: dual-mode (deepseek-v4, magistral, gemini-pro, claude
    //     opus/sonnet, qualunque thinking tool-capable) -> non-thinking nei tool-loop.
    //   - none: modello non-thinking standard.
    let agentic_thinking_policy: &'static str = if p == "deepseek"
        && (m.contains("reasoner") || m.contains("coder") || m.contains("chat"))
    {
        "exclude"
    } else if p == "openai" && reasoning_only_family {
        "native"
    } else if is_reasoning_signal || is_hybrid_agentic(&p, &m) || gemini_25_thinking {
        "disable_for_tools"
    } else {
        "none"
    };

    ClassifiedCaps {
        supports_tool_use,
        supports_vision,
        uses_thinking_mode,
        agentic_thinking_policy,
    }
}

/// Modelli ibridi: ottimi per l'agentico (NON escludere, A=false) ma con
/// extended thinking che impone tool_choice non forzato (B=true).
fn is_hybrid_agentic(provider: &str, model_lc: &str) -> bool {
    match provider {
        "anthropic" => model_lc.contains("opus") || model_lc.contains("sonnet"),
        _ => false,
    }
}

/// PUNTO UNICO della regola di ammissione di un modello al catalog ABILITATO
/// (ADR 0025, regola L). True se (provider, model) e' ammesso dalla policy
/// `nexus_model_selection_policy`: matcha un `allowed_pattern` (o la lista e'
/// vuota) e NESSUN `denied_pattern`. Se non esiste una riga policy per il
/// provider, ammette (true) per non bloccare provider non ancora configurati.
/// La discovery la consulta prima di abilitare un modello, cosi' i modelli
/// legacy (pruned dalla mig 0320) non rientrano via probe-on-insert.
pub(crate) async fn model_passes_selection_policy(
    db: &PgPool,
    provider: &str,
    model: &str,
) -> bool {
    let row: Option<(bool,)> = sqlx::query_as::<_, (bool,)>(
        "SELECT (
             ( $2 ~ ANY(allowed_patterns) OR cardinality(allowed_patterns) = 0 )
             AND NOT ( $2 ~ ANY(denied_patterns) )
         ) AS ok
         FROM nexus_model_selection_policy WHERE provider = $1",
    )
    .bind(provider)
    .bind(model)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    row.map(|(ok,)| ok).unwrap_or(true)
}

/// Esito di una passata di reconciliation policy->catalog.
#[derive(Debug, Default, Clone, Copy)]
pub struct PolicyReconcileStats {
    /// Modelli portati a is_enabled=true perche' rientrati nella policy.
    pub enabled: u64,
    /// Modelli portati a is_enabled=false perche' usciti dalla policy.
    pub disabled: u64,
}

/// RECONCILIATION policy->catalog (regola H: causa radice, non toppa; regola L:
/// punto unico = la `nexus_model_selection_policy`).
///
/// Il sync di discovery (`sync_provider`) abilita un modello SOLO quando lo
/// "vede ricomparire" dall'API live. I modelli importati staticamente nel
/// catalog (es. il dataset LiteLLM) che l'API non re-elenca non venivano MAI
/// rivalutati contro la policy del provider e restavano col loro
/// `is_enabled` iniziale per sempre (incidente google: 0/157 abilitati pur con
/// policy corretta). Questa funzione riallinea `is_enabled` di OGNI riga del
/// catalog alla policy del suo provider, ad ogni tick e al boot.
///
/// Semantica (un solo punto di verita', la stessa SQL di
/// `model_passes_selection_policy`, applicata set-based via JOIN sulla tabella
/// policy — niente logica duplicata):
///   - "passa la policy" = ( model ~ ANY(allowed_patterns)
///                           OR cardinality(allowed_patterns)=0 )
///                         AND NOT ( model ~ ANY(denied_patterns) )
///   - ABILITA i modelli che passano la policy e sono disabilitati MA NON per
///     un fallimento: solo se `auto_disabled_reason IS NULL` oppure indica un
///     motivo di POLICY (contiene 'policy'). I reason di fallimento
///     runtime/probe/billing/missing_from_api/tool-capability NON vengono
///     toccati: quelli li gestisce `model_health_probe` / i cicli dedicati.
///   - DISABILITA i modelli abilitati che NON passano piu' la policy
///     (auto_disabled_reason='fuori model_selection_policy').
///   - RISPETTA i lock manuali: salta `capability_source='manual'` e la
///     convenzione `auto_disabled_reason LIKE 'manual:%'`.
///   - SALTA i modelli media (image-gen/audio/video): non sono chat-compatibili
///     per design e non rientrano nella chat model_selection_policy; la loro
///     eleggibilita' dipende dai flag `supports_<media>` (gestiti altrove).
///     Si applica solo alle righe che NON hanno alcuna capability media.
/// Predicato lock manuale: punto unico della convenzione (vedi sync_provider).
/// Sia la colonna reale `capability_source='manual'` sia la convenzione
/// `auto_disabled_reason LIKE 'manual:%'`.
pub(crate) const RECONCILE_MANUAL_LOCKED_SQL: &str = "(c.capability_source = 'manual' \
     OR (c.auto_disabled_reason IS NOT NULL AND c.auto_disabled_reason LIKE 'manual:%'))";
/// Predicato media: una riga e' "media" se espone una capability non-chat.
/// Queste righe sono escluse dalla reconciliation chat.
pub(crate) const RECONCILE_IS_MEDIA_SQL: &str = "(c.supports_image_gen OR c.supports_audio_in \
     OR c.supports_audio_out OR c.supports_video_gen)";
/// Predicato PREZZO IGNOTO (mig 0477): la riga e' a catalogo ma il suo listino
/// non e' noto. I provider non espongono i prezzi nelle `/v1/models`, quindi il
/// discovery inserisce un costo 0 che e' un PLACEHOLDER, non "gratis" — ed e'
/// proprio `pricing_state` a dirlo. Punto unico (regola L) del predicato di
/// ELEGGIBILITA': un modello a prezzo ignoto non e' routabile, perche' altrimenti
/// costo 0 lo rende il piu' "conveniente" di tutti nello scoring e vince il
/// routing fatturando a zero (misurate 873 chiamate e 16,4M token a costo 0).
///
/// Nomina `'unknown'` ESPLICITAMENTE invece di "diverso da priced": `'free'` e' il
/// terzo stato ammesso dal CHECK ed e' un gratuito REALE, legittimamente
/// routabile. Regola M: si guarda lo stato strutturato, mai la grandezza del costo.
///
/// `alias` = alias di `ai_price_catalog` nella query ("c" dove c'e' un FROM,
/// stringa vuota nelle UPDATE dirette sulla tabella).
pub(crate) fn price_unknown_sql(alias: &str) -> String {
    let prefix = if alias.is_empty() {
        String::new()
    } else {
        format!("{alias}.")
    };
    format!("({prefix}pricing_state = 'unknown')")
}

/// Reason con cui il ciclo prezzo marca le righe disabilitate: distinta da quelle
/// di policy, cosi' `reconcile_enable_returning_to_policy` (che ri-abilita solo
/// `reason IS NULL` o `ILIKE '%policy%'`) non le resuscita.
pub(crate) const PRICE_UNKNOWN_REASON: &str = "price_unknown";

/// "passa la policy" — stessa espressione di `model_passes_selection_policy`,
/// ma set-based via JOIN. Se non esiste riga policy per il provider il JOIN
/// non matcha e la riga non viene toccata (coerente con l'ammissione di
/// default: provider non configurato = non si forza alcuna decisione).
pub(crate) const RECONCILE_PASSES_POLICY_SQL: &str = "( c.model ~ ANY(p.allowed_patterns) \
         OR cardinality(p.allowed_patterns) = 0 ) \
     AND NOT ( c.model ~ ANY(p.denied_patterns) )";

/// ABILITA (reconciliation policy->catalog): rientrati nella policy, disabilitati
/// per assenza/policy (non per fallimento). `auto_disabled_reason IS NULL` (mai
/// disabilitato esplicito, tipico dei modelli importati staticamente) OPPURE
/// reason di policy. Ritorna le righe abilitate. Estratta da
/// `reconcile_catalog_with_policy` senza cambi di comportamento.
async fn reconcile_enable_returning_to_policy(db: &PgPool) -> anyhow::Result<u64> {
    let manual = RECONCILE_MANUAL_LOCKED_SQL;
    let media = RECONCILE_IS_MEDIA_SQL;
    let passes = RECONCILE_PASSES_POLICY_SQL;
    let price_unknown = price_unknown_sql("c");
    let enable_sql = format!(
        "UPDATE ai_price_catalog c \
         SET is_enabled = true, \
             effective_from = NOW(), \
             auto_disabled_at = NULL, \
             auto_disabled_reason = NULL, \
             updated_at = NOW() \
         FROM nexus_model_selection_policy p \
         WHERE p.provider = c.provider \
           AND c.is_enabled = false \
           AND NOT {manual} \
           AND NOT {media} \
           AND NOT {price_unknown} \
           AND ( c.auto_disabled_reason IS NULL \
                 OR c.auto_disabled_reason ILIKE '%policy%' ) \
           AND ( {passes} )",
    );
    Ok(sqlx::query(&enable_sql).execute(db).await?.rows_affected())
}

/// DISABILITA (ciclo prezzo): abilitati ma a prezzo IGNOTO. E' il ramo che rende
/// la regola AUTO-RIPARANTE: senza, i 13 modelli gia' abilitati resterebbero
/// routabili per sempre e servirebbe un UPDATE a mano — cioe' la toppa che la
/// regola H vieta. `reconcile_enable_returning_to_policy` non li recupera (ha
/// `is_enabled = false` e non tocca i reason estranei alla policy).
///
/// Esclude `manual` e `media` come gli altri rami: le righe tts/whisper sono
/// gestite a mano e non passano di qui.
async fn reconcile_disable_price_unknown(db: &PgPool) -> anyhow::Result<u64> {
    let manual = RECONCILE_MANUAL_LOCKED_SQL;
    let media = RECONCILE_IS_MEDIA_SQL;
    let price_unknown = price_unknown_sql("c");
    let disable_sql = format!(
        "UPDATE ai_price_catalog c \
         SET is_enabled = false, \
             auto_disabled_at = NOW(), \
             auto_disabled_reason = '{PRICE_UNKNOWN_REASON}', \
             updated_at = NOW() \
         WHERE c.is_enabled = true \
           AND {price_unknown} \
           AND NOT {manual} \
           AND NOT {media}",
    );
    Ok(sqlx::query(&disable_sql).execute(db).await?.rows_affected())
}

/// DISABILITA (reconciliation policy->catalog): attualmente abilitati ma non piu'
/// conformi alla policy. Ritorna le righe disabilitate. Estratta da
/// `reconcile_catalog_with_policy` senza cambi di comportamento.
async fn reconcile_disable_leaving_policy(db: &PgPool) -> anyhow::Result<u64> {
    let manual = RECONCILE_MANUAL_LOCKED_SQL;
    let media = RECONCILE_IS_MEDIA_SQL;
    let passes = RECONCILE_PASSES_POLICY_SQL;
    let disable_sql = format!(
        "UPDATE ai_price_catalog c \
         SET is_enabled = false, \
             auto_disabled_at = NOW(), \
             auto_disabled_reason = 'fuori model_selection_policy', \
             updated_at = NOW() \
         FROM nexus_model_selection_policy p \
         WHERE p.provider = c.provider \
           AND c.is_enabled = true \
           AND NOT {manual} \
           AND NOT {media} \
           AND NOT ( {passes} )",
    );
    Ok(sqlx::query(&disable_sql).execute(db).await?.rows_affected())
}

pub async fn reconcile_catalog_with_policy(db: &PgPool) -> anyhow::Result<PolicyReconcileStats> {
    let enabled = reconcile_enable_returning_to_policy(db).await?;
    // Ciclo prezzo PRIMA di quello policy: un modello a prezzo ignoto non e'
    // routabile a prescindere dalla policy, e la reason dedicata deve vincere su
    // 'fuori model_selection_policy' (che il ramo enable ri-abiliterebbe).
    let disabled_price = reconcile_disable_price_unknown(db).await?;
    let disabled = reconcile_disable_leaving_policy(db).await? + disabled_price;

    if disabled_price > 0 {
        tracing::info!(
            "catalog_sync: {} modelli disabilitati per prezzo ignoto (pricing_state='unknown'): \
             costo 0 e' un placeholder, non 'gratis' — vedi mig 0477",
            disabled_price,
        );
    }
    if enabled > 0 || disabled > 0 {
        tracing::info!(
            "catalog_sync: policy reconciliation — enabled={} disabled={}",
            enabled,
            disabled,
        );
        // Audit aggregato (lo stile per-modello sarebbe troppo verboso su
        // riallineamenti di massa). Una riga riepilogativa con i conteggi.
        audit_log(
            db,
            "*",
            "*",
            "policy_reconciled",
            json!({ "enabled": enabled, "disabled": disabled }),
        )
        .await;
    } else {
        tracing::debug!("catalog_sync: policy reconciliation — no changes");
    }

    Ok(PolicyReconcileStats { enabled, disabled })
}

/// Loop principale: chiamato da `main.rs` startup via `tokio::spawn(...)`.
/// `orchestrator` (Option) e' usato per il probe-on-insert dei nuovi modelli
/// scoperti dall'API discovery — se None, i nuovi modelli vengono inseriti
/// con `is_enabled=false` (comportamento legacy, sicuro ma richiede admin
/// per abilitare). In produzione e' sempre Some.
pub async fn catalog_sync_loop(db: PgPool, orchestrator: Option<Arc<Orchestrator>>) {
    // Boot: aspetta 30s per dare priorita' agli altri worker.
    tokio::time::sleep(Duration::from_secs(30)).await;
    tracing::info!("catalog_sync worker avviato");

    loop {
        match sync_tick(&db, orchestrator.as_deref()).await {
            Ok(stats) => {
                if stats.inserted > 0 || stats.disabled > 0 || stats.reenabled > 0 {
                    tracing::info!(
                        "catalog_sync: tick completato (provider_ok={} provider_skipped={} inserted={} disabled={} reenabled={})",
                        stats.providers_ok, stats.providers_skipped,
                        stats.inserted, stats.disabled, stats.reenabled,
                    );
                } else {
                    tracing::debug!(
                        "catalog_sync: tick completato (no changes, providers_ok={})",
                        stats.providers_ok,
                    );
                }
            }
            Err(e) => {
                tracing::warn!("catalog_sync: tick fallito: {}", e);
            }
        }

        // Dopo OGNI tick (anche se non ha trovato nuovi modelli), esegui
        // auto-upgrade della routing matrix + auto-populate escalation.
        // Senza questo i campi escalation_* della routing matrix restavano
        // NULL per i nuovi intent/modelli, e `lookup_with_budget` non
        // escalava mai a un modello piu' capable per task lunghi.
        if let Err(e) = crate::models::auto_upgrade_models_and_routing(&db).await {
            tracing::warn!("catalog_sync: auto_upgrade_models_and_routing fallito: {e}");
        }

        // Reconciliation policy->catalog (regola H/L): riallinea is_enabled di
        // OGNI riga del catalog alla policy del suo provider, anche per i
        // modelli importati staticamente che il discovery API non re-elenca.
        if let Err(e) = reconcile_catalog_with_policy(&db).await {
            tracing::warn!("catalog_sync: reconcile_catalog_with_policy fallito: {e}");
        }

        if let Err(e) = crate::reconcile_default_models::reconcile_provider_default_models(&db).await
        {
            tracing::warn!("catalog_sync: reconcile_provider_default_models fallito: {e}");
        }

        // Sleep fino al prossimo tick (interval dinamico dalle settings).
        let interval = load_interval_hours(&db).await;
        tokio::time::sleep(Duration::from_secs(interval * 3600)).await;
    }
}

#[derive(Debug, Default)]
struct SyncStats {
    providers_ok: u32,
    providers_skipped: u32,
    inserted: u32,
    disabled: u32,
    reenabled: u32,
}

async fn load_interval_hours(db: &PgPool) -> u64 {
    get_setting(db, "catalog_sync.interval_hours")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_INTERVAL_HOURS)
        .max(1) // mai meno di 1h per non hammerare le API
}

/// Provider instradabili dall'endpoint /vision/describe (setting
/// `vision.routable_providers`, mig 0373). Punto unico DB-driven della lista
/// (regola G/L): `classify_capabilities` lo usa per marcare supports_vision e
/// `vision.py` lo legge per il routing/messaggio. Default vuoto se il setting
/// manca: fail-safe VISIBILE (nessun modello vision -> il purpose vision_describe
/// segnala no_capable), niente magic fallback hardcoded (regola G).
pub(crate) async fn load_vision_routable(db: &PgPool) -> std::collections::HashSet<String> {
    get_setting(db, "vision.routable_providers")
        .await
        .ok()
        .flatten()
        .map(|csv| {
            csv.split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Trigger manuale sync (endpoint admin / test E2E).
pub async fn trigger_sync_now(
    db: &PgPool,
    orchestrator: Option<&Orchestrator>,
) -> Result<SyncSummary, String> {
    let stats = sync_tick(db, orchestrator)
        .await
        .map_err(|e| e.to_string())?;
    Ok(SyncSummary {
        providers_ok: stats.providers_ok,
        providers_skipped: stats.providers_skipped,
        inserted: stats.inserted,
        disabled: stats.disabled,
        reenabled: stats.reenabled,
    })
}

#[derive(Debug, serde::Serialize)]
pub struct SyncSummary {
    pub providers_ok: u32,
    pub providers_skipped: u32,
    pub inserted: u32,
    pub disabled: u32,
    pub reenabled: u32,
}

async fn sync_tick(db: &PgPool, orchestrator: Option<&Orchestrator>) -> anyhow::Result<SyncStats> {
    // Honor enable flag.
    let enabled = get_setting(db, "catalog_sync.enabled")
        .await?
        .map(|v| v == "true" || v == "1")
        .unwrap_or(true);
    if !enabled {
        tracing::debug!("catalog_sync: disabilitato via settings, skip tick");
        return Ok(SyncStats::default());
    }

    let disable_missing = get_setting(db, "catalog_sync.disable_missing")
        .await?
        .map(|v| v == "true" || v == "1")
        .unwrap_or(true);
    let insert_new_disabled = get_setting(db, "catalog_sync.insert_new_disabled")
        .await?
        .map(|v| v == "true" || v == "1")
        .unwrap_or(true);

    // PEZZO 1 (regola G+L): la lista dei provider da sincronizzare NON e' piu' un
    // CSV hardcoded (setting `catalog_sync.providers`, rimosso dalla mig 0613) ma
    // e' DEDOTTA dal `nexus_provider_registry` (unica fonte). Un provider nuovo,
    // attivo e configurato entra nella discovery da solo, senza toccare liste.
    let providers = providers_da_sincronizzare(db).await?;

    let mut stats = SyncStats::default();
    for provider in &providers {
        sync_one_provider_into_stats(
            db,
            provider,
            disable_missing,
            insert_new_disabled,
            orchestrator,
            &mut stats,
        )
        .await;
    }
    Ok(stats)
}

/// Riga del `nexus_provider_registry` con i campi che servono a dedurre se un
/// provider va sincronizzato (PEZZO 1): nome + activation + le chiavi settings da
/// cui leggere lo stato di configurazione.
#[derive(sqlx::FromRow)]
struct ProviderRegistryRow {
    name: String,
    activation: Option<String>,
    key_setting: Option<String>,
    enabled_setting: Option<String>,
    base_url_setting: Option<String>,
}

/// PUNTO UNICO (regola G+L) dei provider da sincronizzare: dedotti dal
/// `nexus_provider_registry` (unica fonte di verita'), NON da una lista CSV
/// hardcoded. Prima il setting `catalog_sync.providers` duplicava cio' che il
/// registry gia' sa, e un provider `is_active` + configurato ma fuori dal CSV
/// (openrouter/groq/perplexity) restava fuori dalla discovery live pur essendo
/// identico ai 5 nel CSV. Rimosso il CSV (mig 0613), un nuovo provider nel
/// registry entra nella discovery da solo.
///
/// Un provider entra se `is_active = true` E [`provider_is_configured`].
async fn providers_da_sincronizzare(db: &PgPool) -> anyhow::Result<Vec<String>> {
    let rows: Vec<ProviderRegistryRow> = sqlx::query_as::<_, ProviderRegistryRow>(
        "SELECT name, activation, key_setting, enabled_setting, base_url_setting \
         FROM nexus_provider_registry WHERE is_active = true ORDER BY sort_order, name",
    )
    .fetch_all(db)
    .await?;
    let mut out = Vec::new();
    for r in rows {
        if provider_is_configured(db, &r).await? {
            out.push(r.name);
        }
    }
    Ok(out)
}

/// "Configurato" di un provider del registry (PEZZO 1). Due sole forme di
/// attivazione:
///   - activation contenente 'api_key' (incl. 'api_key_or_vertex'): richiede
///     `enabled_setting` = 'true'/'1' E `key_setting` valorizzato nei settings;
///   - activation = 'base_url' (vllm): richiede `base_url_setting` valorizzato.
/// Ogni altra activation (o assente) -> non configurato (escluso pulito, niente
/// panico). Un errore di lettura settings PROPAGA (regola M): un provider non
/// deve sparire dalla sync per un DB che sbatte — meglio far fallire il tick.
async fn provider_is_configured(db: &PgPool, r: &ProviderRegistryRow) -> anyhow::Result<bool> {
    Ok(match r.activation.as_deref() {
        Some(a) if a.contains("api_key") => {
            setting_is_true(db, r.enabled_setting.as_deref()).await?
                && setting_is_valued(db, r.key_setting.as_deref()).await?
        }
        Some("base_url") => setting_is_valued(db, r.base_url_setting.as_deref()).await?,
        _ => false,
    })
}

/// True se il setting `key` esiste e vale 'true'/'1'. Chiave `None`/assente -> false.
async fn setting_is_true(db: &PgPool, key: Option<&str>) -> anyhow::Result<bool> {
    match key {
        Some(k) => Ok(get_setting(db, k)
            .await?
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false)),
        None => Ok(false),
    }
}

/// True se il setting `key` esiste ed e' valorizzato (non vuoto). Chiave
/// `None`/assente -> false.
async fn setting_is_valued(db: &PgPool, key: Option<&str>) -> anyhow::Result<bool> {
    match key {
        Some(k) => Ok(get_setting(db, k)
            .await?
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)),
        None => Ok(false),
    }
}

/// Esegue `sync_provider` per un provider e accumula l'esito in `stats`
/// (ok/skipped + delta inserted/disabled/reenabled). Estratta dal loop di
/// `sync_tick` senza cambi di comportamento.
async fn sync_one_provider_into_stats(
    db: &PgPool,
    provider: &str,
    disable_missing: bool,
    insert_new_disabled: bool,
    orchestrator: Option<&Orchestrator>,
    stats: &mut SyncStats,
) {
    match sync_provider(
        db,
        provider,
        disable_missing,
        insert_new_disabled,
        orchestrator,
    )
    .await
    {
        Ok((ins, dis, re)) => {
            stats.providers_ok += 1;
            stats.inserted += ins;
            stats.disabled += dis;
            stats.reenabled += re;
        }
        Err(e) => {
            stats.providers_skipped += 1;
            tracing::warn!("catalog_sync[{}] skip: {}", provider, e);
        }
    }
}

/// Modelli live del provider indicizzati per il sync: la lista dei nomi e la
/// mappa nome -> finestra di contesto DICHIARATA (solo quando >0).
struct DiscoveredModels {
    api_models: Vec<String>,
    declared_windows: std::collections::HashMap<String, i64>,
}

/// Fetch dei modelli live dal gateway + indicizzazione (finestre dichiarate).
/// Estratta da `sync_provider` senza cambi di comportamento: bail su lista vuota
/// (sospetta, per safety), poi costruisce `declared_windows` e `api_models`.
async fn discover_provider_models(
    orchestrator: Option<&Orchestrator>,
    provider: &str,
) -> anyhow::Result<DiscoveredModels> {
    // Fetch modelli live dal gateway (via UNICA per la discovery: il gateway
    // incapsula l'auth di ogni provider, Vertex Service Account incluso, quindi
    // non servono api_key qui ne' la delega al brain per Google — regola L).
    let api_metas = fetch_provider_models(orchestrator, provider).await?;
    if api_metas.is_empty() {
        anyhow::bail!("gateway ha ritornato lista vuota (sospetto, skip per safety)");
    }
    // Finestra di contesto DICHIARATA dal provider per modello (quando l'API la
    // espone, es. Mistral max_context_length). Fonte per scrivere il valore
    // REALE nel catalog: mai inventarne uno (regola H, incidente 2026-07-06:
    // il default schema 8192 preso per finestra vera paralizzava i sub-run).
    let mut declared_windows: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    for m in &api_metas {
        if let Some(w) = m.context_window.filter(|w| *w > 0) {
            declared_windows.insert(m.id.clone(), w);
        }
    }
    let api_models: Vec<String> = api_metas.into_iter().map(|m| m.id).collect();
    Ok(DiscoveredModels {
        api_models,
        declared_windows,
    })
}

/// Carica i modelli del catalog locale per un provider: mappa
/// nome -> (is_enabled, manual_locked). Estratta da `sync_provider` senza cambi
/// di comportamento.
///
/// Il flag `manual_locked` viene letto da `auto_disabled_reason LIKE 'manual:%'`
/// — convenzione introdotta col fix Bug A (audit 27/05/2026) per evitare che
/// il worker riabiliti modelli che l'admin ha disabilitato manualmente
/// (es. modelli "preview" che l'API espone ma non accetta in inference).
async fn load_catalog_models(
    db: &PgPool,
    provider: &str,
) -> anyhow::Result<std::collections::HashMap<String, (bool, bool)>> {
    let catalog_rows = sqlx::query_as::<_, (String, bool, bool)>(
        "SELECT model, is_enabled, \
         (auto_disabled_reason IS NOT NULL AND auto_disabled_reason LIKE 'manual:%') AS manual_locked \
         FROM ai_price_catalog WHERE provider = $1",
    )
    .bind(provider)
    .fetch_all(db)
    .await?;
    // (model_name -> (is_enabled, manual_locked))
    Ok(catalog_rows
        .into_iter()
        .map(|(m, e, l)| (m, (e, l)))
        .collect())
}

/// Delta prodotti dall'elaborazione di UN modello scoperto dall'API discovery.
#[derive(Debug, Default, Clone, Copy)]
struct ModelSyncDelta {
    inserted: u32,
    reenabled: u32,
    tier_realigned: u32,
}

/// Ramo MEDIA terminale di `process_discovered_model`: insert dedicato, default
/// sicuro (is_enabled=false), NIENTE probe chat (probe_model_on_insert
/// chiamerebbe una completion: un media non e' chat -> sarebbe rumore/falso
/// fallimento). NON applica la chat model_selection_policy ne' il pruning chat:
/// i media vivono dei loro flag supports_<media>. Ritorna quante righe media
/// sono state inserite (0 o 1). Estratto senza cambi di comportamento.
async fn process_discovered_media_model(
    db: &PgPool,
    provider: &str,
    api_model: &str,
    kind: &str,
    insert_new_disabled: bool,
    catalog_models: &std::collections::HashMap<String, (bool, bool)>,
) -> u32 {
    match catalog_models.contains_key(api_model) {
        false if insert_new_disabled => insert_media_model(db, provider, api_model, kind)
            .await
            .unwrap_or(0),
        // Media gia' presente: riallinea il flag supports_<media> sulle
        // righe 'auto' (gemello del riallineamento supports_vision dei chat).
        true => {
            realign_media_flags(db, provider, api_model, kind).await;
            0
        }
        false => 0,
    }
}

/// Elabora un singolo modello scoperto dall'API discovery (ramo INSERT del passo
/// 1 di `sync_provider`), ritornando i delta. Estratta senza cambi di
/// comportamento: gate chat/media, ramo media terminale, ramo chat (insert nuovo
/// / riallineamento + re-enable esistente).
async fn process_discovered_model(
    db: &PgPool,
    orchestrator: Option<&Orchestrator>,
    provider: &str,
    api_model: &str,
    insert_new_disabled: bool,
    catalog_models: &std::collections::HashMap<String, (bool, bool)>,
    declared_windows: &std::collections::HashMap<String, i64>,
) -> ModelSyncDelta {
    let mut delta = ModelSyncDelta::default();

    // Classificazione MEDIA (punto unico, regola L): un modello image-gen/
    // audio/video NON e' chat completion ma va comunque gestito dal catalog
    // per essere instradato PER CAPABILITY. Prima questi venivano SCARTATI
    // da `is_chat_compatible_model` (blacklist), rendendoli inesistenti per
    // il routing media. Ora: se e' un media lo inseriamo coi suoi flag; se
    // NON e' un media E non e' chat-compatibile (embedding/realtime/instruct/
    // moderation) lo skippiamo come prima.
    let media = classify_media_kind(api_model);
    if media.is_none() && !is_chat_compatible_model(api_model) {
        tracing::debug!(
            "catalog_sync[{}]: skip '{}' (non chat-compatible, non media)",
            provider,
            api_model
        );
        return delta;
    }

    // Ramo MEDIA: terminale (non attraversa il match chat sottostante).
    if let Some(kind) = media {
        delta.inserted += process_discovered_media_model(
            db,
            provider,
            api_model,
            kind,
            insert_new_disabled,
            catalog_models,
        )
        .await;
        return delta;
    }

    // Ramo CHAT.
    let declared_window = declared_windows.get(api_model).copied();
    process_discovered_chat_model(
        db,
        orchestrator,
        provider,
        api_model,
        insert_new_disabled,
        declared_window,
        catalog_models.get(api_model).copied(),
        &mut delta,
    )
    .await;
    delta
}

/// Ramo CHAT di `process_discovered_model`: se il modello non e' nel catalog lo
/// inserisce (default sicuro is_enabled=false + probe-on-insert); se e' gia'
/// presente riallinea tier/vision + finestra di contesto e prova il re-enable.
/// Aggiorna `delta` in loco. Estratto senza cambi di comportamento.
#[allow(clippy::too_many_arguments)]
async fn process_discovered_chat_model(
    db: &PgPool,
    orchestrator: Option<&Orchestrator>,
    provider: &str,
    api_model: &str,
    insert_new_disabled: bool,
    declared_window: Option<i64>,
    catalog_entry: Option<(bool, bool)>,
    delta: &mut ModelSyncDelta,
) {
    match catalog_entry {
        None => {
            // PEZZO 2 (regola L): la policy governa anche l'INSERT, non solo
            // l'enable. Un provider con policy restrittiva inserisce SOLO i suoi
            // allowed; un provider SENZA riga policy (`unwrap_or(true)`) inserisce
            // tutti (comportamento invariato per i 5 provider che ce l'hanno gia').
            // Punto unico: si riusa `model_passes_selection_policy` — la stessa SQL
            // del reconcile e dell'enable — invece di duplicare il filtro.
            if insert_new_disabled
                && model_passes_selection_policy(db, provider, api_model).await
                && insert_new_chat_model(db, orchestrator, provider, api_model, declared_window)
                    .await
            {
                delta.inserted += 1;
            }
        }
        Some((is_enabled, manual_locked)) => {
            delta.tier_realigned += realign_existing_model(db, provider, api_model).await;
            // Finestra dichiarata dal provider: riallinea il catalog al
            // valore REALE (self-healing dei placeholder, regola H).
            realign_context_window(db, provider, api_model, declared_window).await;
            if reenable_existing_model(db, provider, api_model, is_enabled, manual_locked).await {
                delta.reenabled += 1;
            }
        }
    }
}

async fn sync_provider(
    db: &PgPool,
    provider: &str,
    disable_missing: bool,
    insert_new_disabled: bool,
    orchestrator: Option<&Orchestrator>,
) -> anyhow::Result<(u32, u32, u32)> {
    let DiscoveredModels {
        api_models,
        declared_windows,
    } = discover_provider_models(orchestrator, provider).await?;

    let catalog_models = load_catalog_models(db, provider).await?;
    let api_set: std::collections::HashSet<&str> = api_models.iter().map(|s| s.as_str()).collect();

    // 1. Nuovi modelli dall'API non presenti nel catalog -> INSERT (+ riallineo).
    let ModelSyncDelta {
        inserted,
        reenabled,
        tier_realigned,
    } = accumulate_discovered_models(
        db,
        orchestrator,
        provider,
        insert_new_disabled,
        &api_models,
        &catalog_models,
        &declared_windows,
    )
    .await;

    // 1bis. Prune dei modelli gia' nel catalog non piu' chat-compatibili o fuori
    // policy (self-healing, ADR 0025).
    let mut disabled = prune_incompatible_models(db, provider, &catalog_models).await;

    // 2. Modelli del catalog enabled non piu' nell'API -> disable.
    if disable_missing {
        disabled +=
            disable_missing_models(db, provider, &catalog_models, &api_models, &api_set).await;
    }

    if tier_realigned > 0 {
        tracing::info!(
            "catalog_sync[{}]: performance_tier riallineato su {} righe 'auto'",
            provider,
            tier_realigned
        );
    }
    Ok((inserted, disabled, reenabled))
}

/// Passo 1 di `sync_provider`: itera i modelli scoperti dall'API e accumula i
/// delta (insert nuovi + riallineo/re-enable esistenti). Estratto senza cambi di
/// comportamento; `disabled` NON e' qui (i due passi di disable restano nel
/// chiamante).
#[allow(clippy::too_many_arguments)]
async fn accumulate_discovered_models(
    db: &PgPool,
    orchestrator: Option<&Orchestrator>,
    provider: &str,
    insert_new_disabled: bool,
    api_models: &[String],
    catalog_models: &std::collections::HashMap<String, (bool, bool)>,
    declared_windows: &std::collections::HashMap<String, i64>,
) -> ModelSyncDelta {
    let mut total = ModelSyncDelta::default();
    for api_model in api_models {
        let delta = process_discovered_model(
            db,
            orchestrator,
            provider,
            api_model,
            insert_new_disabled,
            catalog_models,
            declared_windows,
        )
        .await;
        total.inserted += delta.inserted;
        total.reenabled += delta.reenabled;
        total.tier_realigned += delta.tier_realigned;
    }
    total
}

/// 1bis. Auto-disabilita i modelli GIA' nel catalog (is_enabled=true) non piu'
/// chat-compatibili (blacklist) O fuori model_selection_policy (famiglia legacy).
/// Estratta da `sync_provider` senza cambi di comportamento. Ritorna quante righe
/// sono state disabilitate.
///
/// Senza questo passo l'unico controllo di compatibility e' all'INSERT, quindi un
/// modello entrato prima dell'aggiornamento della blacklist (es. gemini-3.5-flash,
/// regola H CLAUDE.md) resterebbe ON per sempre. `manual_locked` viene rispettato.
/// I modelli media (image-gen/audio/video) sono esclusi dal prune chat: la loro
/// eleggibilita' dipende dai flag supports_<media>, gestiti altrove.
async fn prune_incompatible_models(
    db: &PgPool,
    provider: &str,
    catalog_models: &std::collections::HashMap<String, (bool, bool)>,
) -> u32 {
    let mut disabled = 0u32;
    for (catalog_model, (is_enabled, manual_locked)) in catalog_models {
        // GATE MEDIA (regola L): un media non e' chat-compatibile per design.
        if classify_media_kind(catalog_model).is_some() {
            continue;
        }
        if !*is_enabled || *manual_locked {
            continue;
        }
        let chat_ok = is_chat_compatible_model(catalog_model);
        let policy_ok = model_passes_selection_policy(db, provider, catalog_model).await;
        if chat_ok && policy_ok {
            continue;
        }
        let reason = if !chat_ok {
            "not_chat_compatible"
        } else {
            "fuori model_selection_policy (legacy)"
        };
        if prune_disable_one_model(db, provider, catalog_model, reason).await {
            disabled += 1;
        }
    }
    disabled
}

/// Disabilita un singolo modello incompatibile/fuori-policy con il `reason`
/// dato + audit + log. Ritorna `true` se una riga e' stata disabilitata.
/// Estratta da `prune_incompatible_models` senza cambi di comportamento.
async fn prune_disable_one_model(
    db: &PgPool,
    provider: &str,
    catalog_model: &str,
    reason: &str,
) -> bool {
    let res = sqlx::query(
        "UPDATE ai_price_catalog SET is_enabled = false, \
         auto_disabled_at = NOW(), auto_disabled_reason = $3 \
         WHERE provider = $1 AND model = $2",
    )
    .bind(provider)
    .bind(catalog_model)
    .bind(reason)
    .execute(db)
    .await;
    if let Ok(r) = res {
        if r.rows_affected() > 0 {
            audit_log(
                db,
                provider,
                catalog_model,
                "disabled",
                json!({ "reason": reason }),
            )
            .await;
            tracing::warn!(
                "catalog_sync[{}]: disabled '{}' ({})",
                provider,
                catalog_model,
                reason,
            );
            return true;
        }
    }
    false
}

/// True se `api_model` e' `catalog_model` seguito da un suffisso data ISO
/// `-YYYYMMDD` (9 caratteri, dash + 8 cifre). Usata per preservare gli alias
/// "senza data" quando l'API ritorna solo la variante datata.
fn is_dated_variant_of(api_model: &str, catalog_model: &str) -> bool {
    if !api_model.starts_with(catalog_model) || api_model.len() <= catalog_model.len() {
        return false;
    }
    let suffix = &api_model[catalog_model.len()..];
    suffix.starts_with('-') && suffix.len() == 9 && suffix[1..].chars().all(|c| c.is_ascii_digit())
}

/// True se un modello del catalog assente dall'API va PRESERVATO (non
/// disabilitato) perche' e' un alias (con/senza suffisso data) ancora coperto
/// dall'API, OPPURE perche' e' "recentemente sano" secondo l'account (FIX 2: la
/// lista upstream e' un INDIZIO, non la verita'). Nel caso "recentemente sano"
/// registra anche l'annotazione diagnostica idempotente. Estratta da
/// `disable_missing_models` senza cambi di comportamento.
async fn missing_model_should_be_preserved(
    db: &PgPool,
    provider: &str,
    catalog_model: &str,
    api_models: &[String],
    api_set: &std::collections::HashSet<&str>,
) -> bool {
    // Skip alias "senza data" (es. claude-haiku-4-5) se l'API ritorna lo stesso
    // modello con suffisso data (es. claude-haiku-4-5-20251001). Anthropic
    // ritorna solo dated, ma il catalog/routing usa l'alias (piu' stabile).
    if api_models
        .iter()
        .any(|api_m| is_dated_variant_of(api_m, catalog_model))
    {
        return true; // alias preservato (es. claude-haiku-4-5)
    }

    // Skip alias con suffisso data se la base name e' nell'API.
    // Es: catalog "claude-sonnet-4-6-20251201" disabilitato solo se anche
    // "claude-sonnet-4-6" non e' nell'API.
    let base_name = strip_date_suffix(catalog_model);
    if base_name.as_str() != catalog_model && api_set.contains(base_name.as_str()) {
        return true;
    }

    // FIX 2 (catalog_sync probe-aware): la lista upstream LiteLLM/provider e'
    // un INDIZIO, non la verita'. La verita' e' l'account: se il probe
    // (model_health_probe) trova ancora il modello SANO, non spegnerlo solo
    // perche' "datato" / non piu' in lista. Disabilitiamo solo se anche
    // l'health reale lo conferma rotto.
    match salute_modello_recente(db, provider, catalog_model).await {
        SaluteModello::Sano => {
            // Annotazione diagnostica idempotente: il modello resta
            // is_enabled=true (NON tocchiamo auto_disabled_*), ma l'audit_log
            // registra la decisione cosi' e' rintracciabile perche' un modello
            // "datato" e' rimasto attivo.
            tracing::info!(
                "catalog_sync[{}]: '{}' assente da upstream MA probe recente healthy -> NON disabilito (legacy, lascio is_enabled=true)",
                provider, catalog_model,
            );
            audit_log(
                db,
                provider,
                catalog_model,
                "kept_enabled_legacy",
                json!({"reason":"missing_from_api_but_recently_healthy"}),
            )
            .await;
            true
        }
        // Nessun verdetto: non si scrive. Non e' "sano" (niente audit che lo
        // affermi), e' l'assenza del fatto su cui la disabilitazione si fonda.
        // Il sync successivo ripone la domanda; un modello di troppo acceso per
        // un giro non costa nulla, un parco spento da un DB muto si'.
        SaluteModello::NonInterrogabile => {
            tracing::warn!(
                "catalog_sync[{}]: '{}' assente da upstream e salute NON interrogabile -> nessuna scrittura (rimando al prossimo sync)",
                provider, catalog_model,
            );
            true
        }
        SaluteModello::NonSano => false,
    }
}

/// Disabilita un singolo modello assente dall'API (reason 'missing_from_api') +
/// audit + log. Ritorna `true` se una riga e' stata effettivamente disabilitata.
/// Estratta da `disable_missing_models` senza cambi di comportamento.
async fn disable_one_missing_model(db: &PgPool, provider: &str, catalog_model: &str) -> bool {
    let res = sqlx::query(
        "UPDATE ai_price_catalog SET is_enabled = false, \
         auto_disabled_at = NOW(), auto_disabled_reason = 'missing_from_api' \
         WHERE provider = $1 AND model = $2",
    )
    .bind(provider)
    .bind(catalog_model)
    .execute(db)
    .await;
    if let Ok(r) = res {
        if r.rows_affected() > 0 {
            audit_log(
                db,
                provider,
                catalog_model,
                "disabled",
                json!({"reason":"missing_from_api"}),
            )
            .await;
            tracing::warn!(
                "catalog_sync[{}]: - disabled '{}' (non piu nell API)",
                provider,
                catalog_model,
            );
            return true;
        }
    }
    false
}

/// 2. Disabilita i modelli del catalog enabled non piu' presenti nell'API.
/// Estratta da `sync_provider` senza cambi di comportamento. Ritorna quante righe
/// sono state disabilitate. Preserva gli alias (con/senza suffisso data) e i
/// modelli "recentemente sani" secondo l'account (FIX 2: la lista upstream e' un
/// INDIZIO, non la verita').
async fn disable_missing_models(
    db: &PgPool,
    provider: &str,
    catalog_models: &std::collections::HashMap<String, (bool, bool)>,
    api_models: &[String],
    api_set: &std::collections::HashSet<&str>,
) -> u32 {
    let mut disabled = 0u32;
    for (catalog_model, (is_enabled, _manual_locked)) in catalog_models {
        if !*is_enabled || api_set.contains(catalog_model.as_str()) {
            continue;
        }
        if missing_model_should_be_preserved(db, provider, catalog_model, api_models, api_set).await
        {
            continue;
        }
        if disable_one_missing_model(db, provider, catalog_model).await {
            disabled += 1;
        }
    }
    disabled
}

/// Valore di `context_window` da scrivere nel catalog (regola H): quello
/// DICHIARATO dal provider quando l'API lo espone, altrimenti 0 = finestra
/// IGNOTA (i gate che la usano si disattivano su 0). Mai un placeholder
/// spacciato per reale: il default schema 8192 (mig 0032) paralizzava i run
/// via predictive cap (incidente sub-agente 2026-07-06).
fn catalog_window_value(declared: Option<i64>) -> i32 {
    declared.filter(|w| *w > 0).unwrap_or(0) as i32
}

/// INSERT di un nuovo modello CHAT scoperto via API discovery (default sicuro
/// is_enabled=false) + probe-on-insert. Estratta dal ramo `None` di
/// `sync_provider` senza cambi di comportamento. Ritorna `true` se la riga e'
/// stata inserita (rows_affected>0).
///
/// pricing_state='unknown': costo 0 e' un PLACEHOLDER non raffinato, NON un
/// modello gratuito (regola H, mig 0477). `context_window` esplicito via
/// [`catalog_window_value`].
///
/// performance_tier: **NULL** (mig 0599). Un modello appena scoperto non ha
/// ancora un agentic_index sincronizzato, quindi NON esiste alcun fatto su cui
/// fondare una fascia — ed e' esattamente per questo che il nome sembrava l'unica
/// opzione. Ma la risposta giusta non e' indovinare: e' non avere tier finche' i
/// fatti non arrivano. Il tier NULL non toglie nulla, perche' la riga nasce
/// `is_enabled=false` + `unqualified` ed e' gia' fuori dal pool agentico (gate
/// mig 0595); al primo sync dell'indice ci pensa [`refresh_tiers_from_index`],
/// e alla prima banda certificata la batteria scrive `measured`.
///
/// Prima qui c'era `infer_tier_from_name`, che indovinava la fascia dal NOME: e'
/// cosi' che `gpt-5.6-sol` (il piu' capace del parco) e' finito in 'high' e il
/// tier 'heavy' e' diventato un fossile di cio' che l'euristica sapeva quando fu
/// scritta.
async fn insert_new_chat_model(
    db: &PgPool,
    orchestrator: Option<&Orchestrator>,
    provider: &str,
    api_model: &str,
    declared_window: Option<i64>,
) -> bool {
    let context_window = catalog_window_value(declared_window);
    let res = sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, display_name, input_cost_per_million_tokens, \
          output_cost_per_million_tokens, currency, capabilities, performance_tier, is_enabled, pricing_state, context_window, effective_from) \
         VALUES ($1, $2, $3, 0, 0, 'USD', '[]'::jsonb, NULL, false, 'unknown', $4, NOW()) \
         ON CONFLICT (provider, model) DO NOTHING",
    )
    .bind(provider)
    .bind(api_model)
    .bind(api_model)
    .bind(context_window)
    .execute(db)
    .await;
    match res {
        Ok(r) if r.rows_affected() > 0 => {
            on_chat_model_inserted(db, orchestrator, provider, api_model).await;
            true
        }
        _ => false,
    }
}

/// Coda dell'INSERT riuscito di un modello chat: audit + log + probe-on-insert.
/// Il probe prova subito una chiamata di test al provider (il modello e'
/// is_enabled=false di default): se passa, abilita; se fallisce con
/// model_not_found/hollow, marca esplicitamente il motivo cosi' l'admin sa che
/// NON va abilitato manualmente. Cosi' i modelli "fantasma" (es. la famiglia
/// gemini-3.x al 05/2026) non possono mai entrare enabled via auto-discovery.
async fn on_chat_model_inserted(
    db: &PgPool,
    orchestrator: Option<&Orchestrator>,
    provider: &str,
    api_model: &str,
) {
    audit_log(
        db,
        provider,
        api_model,
        "inserted",
        json!({"source":"api_discovery"}),
    )
    .await;
    tracing::info!(
        "catalog_sync[{}]: + nuovo modello rilevato '{}'",
        provider,
        api_model
    );
    if let Some(orch) = orchestrator {
        probe_new_model_on_insert(db, orch, provider, api_model).await;
    }
}

/// Riallinea `supports_vision` e `performance_tier` di un modello GIA' presente
/// (righe `capability_source='auto'`) all'euristica corrente. Estratta da
/// `sync_provider` senza cambi di comportamento. Ritorna 1 se il tier e' stato
/// riallineato (rows_affected>0), 0 altrimenti — cosi' il chiamante mantiene il
/// contatore `tier_realigned` invariato.
///
/// FIX A (regola H): prima il sync toccava SOLO is_enabled, quindi una correzione
/// dell'euristica vision/tier non si propagava ai modelli esistenti (serviva una
/// migrazione-dati manuale). Ora si propaga da sola; le righe 'manual' (curate
/// dall'admin) restano intatte (guard capability_source='auto').
async fn realign_existing_model(db: &PgPool, provider: &str, api_model: &str) -> u32 {
    let vision_routable = load_vision_routable(db).await;
    let cc = classify_capabilities(provider, api_model, None, None, None, &vision_routable);
    let _ = sqlx::query(
        "UPDATE ai_price_catalog SET supports_vision = $3, updated_at = NOW() \
         WHERE provider = $1 AND model = $2 AND capability_source = 'auto' \
           AND supports_vision <> $3",
    )
    .bind(provider)
    .bind(api_model)
    .bind(cc.supports_vision)
    .execute(db)
    .await;

    // performance_tier DAI FATTI (mig 0599), non dal nome. Il sync e' il momento
    // giusto: i prezzi e la finestra sono appena stati raffinati dalla discovery.
    refresh_tier_prior(db, provider, api_model).await
}

/// Ricalcola il tier `synced` di UN modello dall'indice della classificazione
/// esterna. Sostituisce `infer_tier_from_name`, che indovinava la fascia dal
/// NOME, e il ripiego sul prezzo (mig 0608).
///
/// La PRECEDENZA fra le fonti non si decide qui: la scrittura delega ad
/// [`apply_tier`] (punto unico, regola L), che sa che `measured` e `manual`
/// sono piu' autorevoli di un seme sincronizzato. Prima quella regola era una
/// WHERE scritta a mano in questa funzione e un CASE scritto a mano nella
/// batteria: due formulazioni della stessa cosa, allineate solo dalla diligenza.
pub(crate) async fn refresh_tier_prior(db: &PgPool, provider: &str, api_model: &str) -> u32 {
    let Some(bands) = tier_prior_bands(db).await else {
        return 0; // prior disabilitato dal flag DB (o percentuali mancanti)
    };
    // Il path per-modello (discovery, sync LiteLLM) legge l'ANCORA PERSISTITA
    // (mig 0615): il massimo del parco lo ricalcola solo il giro set-based
    // `refresh_tiers_from_index`, che la aggiorna con la deadband.
    let Some(leader) = read_anchor(db, "catalog.tier_relative.anchor").await else {
        tracing::debug!(
            provider = %provider, model = %api_model,
            "refresh_tier_prior: ancora della scala relativa non ancora fissata \
             (catalog.tier_relative.anchor vuota): tier invariato fino al primo \
             giro di refresh_tiers_from_index"
        );
        return 0;
    };
    refresh_tier_prior_con_leader(db, provider, api_model, leader, &bands).await
}

/// Un setting NUMERICO, se presente e parsabile. Punto unico del pattern
/// lettura+parse di questo modulo (il default, dove esiste, resta al chiamante).
async fn setting_num<T: std::str::FromStr>(db: &PgPool, key: &str) -> Option<T> {
    crate::settings::get_setting(db, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse().ok())
}

/// L'ancora persistita sotto `key`, se e' un numero positivo. Vuota o assente =
/// scala non ancora ancorata.
async fn read_anchor(db: &PgPool, key: &str) -> Option<f64> {
    setting_num::<f64>(db, key).await.filter(|v| *v > 0.0)
}

/// Il corpo del refresh di UN modello, col leader gia' risolto dal chiamante.
async fn refresh_tier_prior_con_leader(
    db: &PgPool,
    provider: &str,
    api_model: &str,
    leader: f64,
    bands: &crate::orchestrator::model_service::RelativeBands,
) -> u32 {
    // L'indice STANTIO viene scartato QUI (non nella funzione pura): la fonte e'
    // undocumented e puo' sparire senza preavviso — un indice vecchio non deve
    // passare per fresco.
    let max_age_h = setting_num::<i64>(db, "catalog.agentic_index_sync.max_age_hours")
        .await
        .unwrap_or(168);
    let agentic_index: Option<Option<f64>> = sqlx::query_scalar(
        "SELECT CASE WHEN agentic_index_at IS NULL \
                       OR agentic_index_at < now() - make_interval(hours => $3::int) \
                     THEN NULL ELSE agentic_index END \
           FROM ai_price_catalog WHERE provider = $1 AND model = $2 LIMIT 1",
    )
    .bind(provider)
    .bind(api_model)
    .bind(max_age_h as i32)
    .fetch_optional(db)
    .await
    // Regola H: l'errore SQL si LOGGA, non si inghiotte con `.ok()`. Una
    // query invalida piu' un `.ok()` resta rotta per mesi senza che nulla
    // arrossisca — il compilatore non guarda dentro le stringhe SQL.
    .map_err(|e| {
        tracing::warn!(
            provider = %provider, model = %api_model, error = %e,
            "refresh_tier_prior: lettura dell'indice fallita, tier non derivato"
        );
    })
    .ok()
    .flatten();
    // Indice assente o stantio: NON si azzera il tier esistente. Un tier vecchio
    // e' peggio di uno misurato ma MOLTO meglio di nessuno: `performance_tier`
    // NULL non matcha il filtro della tier-chain, quindi azzerare toglierebbe il
    // modello dal routing per tier. Misurato sul catalogo il 16/07: 43 modelli
    // QUALIFICATI (21 heavy, 10 medium, 6 frontier) sono scoperti dall'indice —
    // OpenRouter non lista quei nomi, e non e' una lacuna temporanea. Restano
    // routabili col tier che hanno finche' la batteria non scrive 'measured'.
    let Some(derived) =
        crate::model_qualification::derive_tier_prior(agentic_index.flatten(), leader, bands)
    else {
        tracing::debug!(
            provider = %provider, model = %api_model,
            "derive_tier: indice assente o stantio -> tier invariato (lo misurera' la batteria)"
        );
        return 0;
    };
    use crate::orchestrator::model_service::{apply_tier, TierSource};
    match apply_tier(db, provider, api_model, derived, TierSource::Synced).await {
        Ok(true) => {
            tracing::info!(
                provider = %provider, model = %api_model, tier = %derived,
                "catalog_sync: tier dalla classificazione esterna (synced), non dal nome"
            );
            1
        }
        Ok(false) => 0,
        // Regola H: un errore SQL si LOGGA. Con `if let Ok(r)` questo UPDATE e'
        // rimasto muto mentre falliva (colonna assente nello schema): il tier non
        // veniva scritto e nulla lo diceva. E' lo stesso pattern che ha tenuto
        // rotte per mesi le query del pannello DB.
        Err(e) => {
            tracing::warn!(
                provider = %provider, model = %api_model, error = %e,
                "refresh_tier_prior: UPDATE del tier fallito"
            );
            0
        }
    }
}

/// Applica il tier dell'indice a TUTTE le righe che ne hanno uno, non solo a
/// quelle che il sync del listino tocca. Ritorna quante righe sono cambiate.
///
/// Perche' esiste (buco misurato il 16/07): `sync_agentic_index` scrive
/// l'indice su ogni modello che riesce a matchare, ma il TIER veniva derivato
/// solo da `refresh_tier_prior`, chiamato per-modello da due soli path — il
/// sync LiteLLM (`models::run_catalog_sync`) e la discovery
/// (`realign_existing_model`). Un modello con indice fresco che non passa da
/// nessuno dei due (perche' il listino LiteLLM non lo conosce, tipico dei
/// modelli nuovi) non riceveva MAI il tier: l'indice restava nella riga,
/// inerte. Se l'indice e' la BASE della classificazione, deve raggiungere
/// tutti.
///
/// Regola L: non ri-deriva nulla. Calcola il LEADER una volta (il massimo
/// `agentic_index` fresco fra le righe enabled — il cooldown transitorio di un
/// provider NON esclude dal leader: il parco e' il parco), lo ancora con la
/// deadband (mig 0615) e poi enumera le righe candidate delegando al punto
/// unico `refresh_tier_prior_con_leader`, che rilegge la riga e applica la
/// scala — cosi' le bande restano definite in un solo posto
/// (`tier_from_leader`) invece di essere riscritte come `CASE` in questa query.
pub(crate) async fn refresh_tiers_from_index(db: &PgPool) -> u32 {
    let Some(bands) = tier_prior_bands(db).await else {
        return 0; // prior disabilitato dal flag DB (o percentuali mancanti)
    };
    let Some(leader) = leader_riancorato(db).await else {
        tracing::debug!(
            "refresh_tiers_from_index: nessun indice fresco su righe enabled e \
             nessuna ancora persistita: la scala relativa non ha un leader"
        );
        return 0;
    };
    let candidati: Vec<(String, String)> = sqlx::query_as(
        "SELECT provider, model FROM ai_price_catalog \
          WHERE agentic_index IS NOT NULL \
            AND (tier_source IS NULL OR tier_source = 'synced')",
    )
    .fetch_all(db)
    .await
    .map_err(|e| {
        tracing::warn!(error = %e, "refresh_tiers_from_index: enumerazione fallita");
    })
    .unwrap_or_default();

    let mut cambiati = 0;
    for (provider, model) in candidati {
        cambiati += refresh_tier_prior_con_leader(db, &provider, &model, leader, &bands).await;
    }
    cambiati
}

/// Il leader della scala del prior, RI-ANCORATO: massimo indice fresco fra le
/// righe enabled, passato per la deadband contro l'ancora persistita
/// (`catalog.tier_relative.anchor`) e persistito se lo scarto la supera.
/// `None` = ne' un massimo fresco ne' un'ancora precedente: la scala non parte.
async fn leader_riancorato(db: &PgPool) -> Option<f64> {
    use crate::orchestrator::model_service::{persist_anchor, resolve_anchor};
    let max_age_h = setting_num::<i64>(db, "catalog.agentic_index_sync.max_age_hours")
        .await
        .unwrap_or(168);
    let fresco: Option<(String, f64)> = sqlx::query_as(
        "SELECT provider || '/' || model, agentic_index FROM ai_price_catalog \
          WHERE is_enabled AND agentic_index IS NOT NULL \
            AND agentic_index_at >= now() - make_interval(hours => $1::int) \
          ORDER BY agentic_index DESC LIMIT 1",
    )
    .bind(max_age_h as i32)
    .fetch_optional(db)
    .await
    .map_err(|e| tracing::warn!(error = %e, "leader_riancorato: lettura del massimo fallita"))
    .ok()
    .flatten();
    let ancora_attuale = read_anchor(db, "catalog.tier_relative.anchor").await;
    let Some((leader_model, nuovo_max)) = fresco else {
        // Parco senza indice fresco: si resta sull'ancora che c'e' (le righe
        // stantie verranno comunque saltate dal check per-riga).
        return ancora_attuale;
    };
    let Some(deadband) = read_deadband(db).await else {
        return ancora_attuale.or(Some(nuovo_max)); // senza deadband non si persiste
    };
    let (ancora, persisti) = resolve_anchor(ancora_attuale, nuovo_max, deadband);
    if persisti {
        tracing::info!(
            ancora = ancora, leader = %leader_model,
            "scala relativa: nuova ancora del tier prior (deadband superata)"
        );
        persist_anchor(db, "catalog.tier_relative", ancora, &leader_model).await;
    }
    Some(ancora)
}

/// La deadband dell'ancora (mig 0615). Assente = WARN visibile, nessun
/// aggiornamento dell'ancora (regola G: la chiave nasce in migrazione, non da
/// un default nel codice).
async fn read_deadband(db: &PgPool) -> Option<f64> {
    let v = setting_num::<f64>(db, "catalog.tier_relative.anchor_deadband_pct").await;
    if v.is_none() {
        tracing::warn!(
            "catalog.tier_relative.anchor_deadband_pct assente o non numerica \
             (applicare la migrazione #0615): l'ancora della scala relativa non si aggiorna"
        );
    }
    v
}

/// Normalizza un id modello per il confronto fra cataloghi diversi (il nostro e
/// quello di OpenRouter). PURA.
///
/// Toglie il prefisso provider (`openai/gpt-5` -> `gpt-5`), il suffisso DATA e i
/// separatori. Lo strip della data e' SICURO e misurato: `claude-opus-4-5-20251101`
/// e' lo stesso modello di `claude-opus-4.5`, `gpt-5.4-mini-2026-03-17` di
/// `gpt-5.4-mini`. Sul parco reale porta la copertura da 31/110 (28%) a 43/110
/// (39%) con **zero chiavi ambigue**.
///
/// NON fa prefix-match, e non deve mai farlo: `gpt-5-nano` matcherebbe `gpt-5` e
/// un nano si prenderebbe l'indice del flagship — cioe' verrebbe promosso a
/// heavy. E' un errore gia' commesso in una prima analisi manuale (`o3` matchato
/// con `o3-mini-high`): qui e' escluso per costruzione.
pub(crate) fn normalize_model_key(id: &str) -> String {
    let senza_provider = id.rsplit('/').next().unwrap_or(id).to_ascii_lowercase();
    // -2026-03-17 oppure -20251101 in coda (e SOLO in coda).
    let senza_data = strip_date_suffix(&senza_provider);
    senza_data
        .chars()
        .filter(|c| *c != '-' && *c != '_' && *c != '.')
        .collect()
}

/// Toglie un suffisso data (`-YYYY-MM-DD` o `-YYYYMMDD`) in coda. PURA.
///
/// PUNTO UNICO (regola L). Ne esistevano DUE con lo stesso nome: questa e una che
/// gestiva solo `-YYYYMMDD` (8 cifre) lasciando passare il formato ISO
/// `-YYYY-MM-DD`. Il suo test lo ASSERIVA (`gpt-4o-mini-2024-07-18` invariato),
/// ma il chiamante — `catalog_sync`, "skip alias con suffisso data se la base
/// name e' nell'API" — dichiara l'intento OPPOSTO: era un limite
/// dell'implementazione fossilizzato in un test, non una scelta di design.
///
/// Consolidando, un alias con data ISO viene ora riconosciuto come alias della
/// sua base, quindi PRESERVATO invece che disabilitato quando la base e' viva
/// nell'API: il cambio va verso il "non disabilitare", che e' il lato sicuro.
fn strip_date_suffix(s: &str) -> String {
    let b = s.as_bytes();
    // -YYYY-MM-DD (11 char)
    if b.len() > 11 {
        let coda = &s[s.len() - 11..];
        let c: Vec<char> = coda.chars().collect();
        if c[0] == '-'
            && c[5] == '-'
            && c[8] == '-'
            && c.iter()
                .enumerate()
                .all(|(i, ch)| matches!(i, 0 | 5 | 8) || ch.is_ascii_digit())
        {
            return s[..s.len() - 11].to_string();
        }
    }
    // -YYYYMMDD (9 char)
    if b.len() > 9 {
        let coda = &s[s.len() - 9..];
        let c: Vec<char> = coda.chars().collect();
        if c[0] == '-' && c[1..].iter().all(char::is_ascii_digit) {
            return s[..s.len() - 9].to_string();
        }
    }
    s.to_string()
}

/// Estrae `{chiave_normalizzata -> agentic_index}` dal JSON di OpenRouter. PURA
/// (nessun I/O: il payload arriva dal chiamante, cosi' il parsing e' testabile
/// senza rete).
///
/// Scarta le chiavi AMBIGUE: se due id diversi normalizzano alla stessa chiave con
/// indici DIVERSI, nessuno dei due entra. Meglio nessun indice che quello del
/// modello sbagliato — un indice sbagliato promuove un modello nel routing, un
/// indice assente lo lascia al ripiego sul prezzo.
pub(crate) fn parse_agentic_index_payload(
    payload: &Value,
) -> std::collections::HashMap<String, f64> {
    use std::collections::HashMap;
    let mut per_chiave: HashMap<String, Vec<f64>> = HashMap::new();
    let Some(models) = payload.get("data").and_then(Value::as_array) else {
        return HashMap::new();
    };
    for m in models {
        let Some(id) = m.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(idx) = m
            .get("benchmarks")
            .and_then(|b| b.get("artificial_analysis"))
            .and_then(|a| a.get("agentic_index"))
            .and_then(Value::as_f64)
        else {
            continue;
        };
        per_chiave.entry(normalize_model_key(id)).or_default().push(idx);
    }
    per_chiave
        .into_iter()
        .filter_map(|(k, v)| {
            let primo = *v.first()?;
            // Ambigua = stessi nome normalizzato, indici diversi -> si scarta.
            if v.iter().any(|x| (x - primo).abs() > f64::EPSILON) {
                tracing::warn!(
                    chiave = %k, valori = ?v,
                    "agentic_index: chiave AMBIGUA, scartata (meglio nessun indice \
                     che quello del modello sbagliato)"
                );
                return None;
            }
            Some((k, primo))
        })
        .collect()
}

/// Sincronizza l'`agentic_index` dal catalogo OpenRouter (mig 0600).
///
/// La fonte e' pubblica e senza autenticazione, ma il campo
/// `benchmarks.artificial_analysis` e' UNDOCUMENTED (le doc citano solo Design
/// Arena): puo' sparire senza preavviso. Per questo scrive anche
/// `agentic_index_at` — cosi' `refresh_tier_prior` scarta un indice STANTIO e
/// ricade sul prezzo, invece di fidarsi per sempre di un dato morto.
///
/// Ritorna quanti modelli hanno ricevuto un indice.
pub async fn sync_agentic_index(db: &PgPool) -> Result<u64, String> {
    let on = crate::settings::get_setting(db, "catalog.agentic_index_sync.enabled")
        .await
        .ok()
        .flatten()
        .map(|v| matches!(v.trim(), "true" | "1" | "yes" | "on"))
        .unwrap_or(false);
    if !on {
        return Ok(0);
    }
    let url = crate::settings::get_setting(db, "catalog.agentic_index_sync.url")
        .await
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            "catalog.agentic_index_sync.url assente in settings (mig 0600)".to_string()
        })?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let payload: Value = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch {url}: {e}"))?
        .json()
        .await
        .map_err(|e| format!("json {url}: {e}"))?;
    let per_chiave = parse_agentic_index_payload(&payload);
    if per_chiave.is_empty() {
        // Il campo e' undocumented: se sparisce, questo e' il segnale. Non si
        // azzerano gli indici esistenti — invecchiano, e max_age_hours li scarta.
        return Err(
            "nessun agentic_index nel payload: la fonte (undocumented) potrebbe \
             essere cambiata. Gli indici esistenti restano e invecchiano."
                .to_string(),
        );
    }
    // Il match avviene in SQL sulla chiave normalizzata, calcolata sui NOSTRI id
    // con la STESSA funzione pura (regola L: una sola normalizzazione).
    let nostri: Vec<(String, String)> =
        sqlx::query_as("SELECT provider, model FROM ai_price_catalog")
            .fetch_all(db)
            .await
            .map_err(|e| format!("select catalog: {e}"))?;
    let mut aggiornati = 0u64;
    for (provider, model) in nostri {
        let Some(idx) = per_chiave.get(&normalize_model_key(&model)) else {
            continue;
        };
        let res = sqlx::query(
            "UPDATE ai_price_catalog \
                SET agentic_index = $3, agentic_index_at = NOW(), updated_at = NOW() \
              WHERE provider = $1 AND model = $2 \
                AND (agentic_index IS DISTINCT FROM $3 OR agentic_index_at IS NULL)",
        )
        .bind(&provider)
        .bind(&model)
        .bind(idx)
        .execute(db)
        .await;
        if let Ok(r) = res {
            aggiornati += r.rows_affected();
        }
    }
    tracing::info!(
        indici_disponibili = per_chiave.len(),
        modelli_aggiornati = aggiornati,
        "agentic_index: sync completato"
    );
    Ok(aggiornati)
}

/// La scala del tier `synced` dal DB (regola G, mig 0615: le soglie ASSOLUTE
/// della 0600 sono state sostituite dalle percentuali relative al leader).
/// `None` = prior disabilitato dal flag O percentuali mancanti (fail-visibile,
/// niente default hardcoded): restano solo `manual` e `measured`, e un modello
/// mai misurato ha tier NULL. Le percentuali le carica il punto unico
/// `model_service::relative_bands` (regola L: la stessa scala serve anche alle
/// bande measured).
pub(crate) async fn tier_prior_bands(
    db: &PgPool,
) -> Option<crate::orchestrator::model_service::RelativeBands> {
    let on = crate::settings::get_setting(db, "catalog.tier_prior.enabled")
        .await
        .ok()
        .flatten()
        .map(|v| matches!(v.trim(), "true" | "1" | "yes" | "on"))
        .unwrap_or(false);
    if !on {
        return None;
    }
    crate::orchestrator::model_service::relative_bands(db, "catalog.tier_relative").await
}

/// Riallinea `context_window` di un modello GIA' nel catalog al valore
/// DICHIARATO dal provider nella discovery (es. Mistral `max_context_length`).
///
/// Self-healing dei placeholder (regola H, incidente sub-agente 2026-07-06: il
/// default schema 8192 preso per finestra reale faceva bloccare OGNI tool dal
/// predictive cap): la fonte autoritativa della finestra e' il provider stesso;
/// quando la dichiara, il catalog converge senza patch manuali. `None`/<=0 =
/// non dichiarata -> nessun tocco (il valore esistente, anche 0 = ignota,
/// resta). UPDATE mirato (solo se differisce) + audit.
async fn realign_context_window(
    db: &PgPool,
    provider: &str,
    api_model: &str,
    declared_window: Option<i64>,
) {
    let Some(window) = declared_window.filter(|w| *w > 0) else {
        return;
    };
    let res = sqlx::query(
        "UPDATE ai_price_catalog SET context_window = $3, updated_at = NOW() \
         WHERE provider = $1 AND model = $2 AND context_window IS DISTINCT FROM $3",
    )
    .bind(provider)
    .bind(api_model)
    .bind(window as i32)
    .execute(db)
    .await;
    let realigned = matches!(res, Ok(r) if r.rows_affected() > 0);
    if !realigned {
        return;
    }
    tracing::info!(
        "catalog_sync[{}]: context_window riallineato '{}' -> {} (dichiarato dal provider)",
        provider,
        api_model,
        window
    );
    audit_log(
        db,
        provider,
        api_model,
        "context_window_realigned",
        json!({"context_window": window, "source": "provider_declared"}),
    )
    .await;
}

/// Re-enable di un modello GIA' presente disabilitato ma ricomparso nell'API,
/// SOLO se ammesso dalla policy (ADR 0025). Estratta da `sync_provider` senza
/// cambi di comportamento. Ritorna `true` se una riga e' stata riabilitata.
async fn reenable_existing_model(
    db: &PgPool,
    provider: &str,
    api_model: &str,
    is_enabled: bool,
    manual_locked: bool,
) -> bool {
    // Re-enable SOLO se il modello e' ammesso dalla policy (ADR 0025): un legacy
    // ricomparso nell'API non deve rientrare.
    let policy_ok = model_passes_selection_policy(db, provider, api_model).await;
    if !is_enabled && !manual_locked && policy_ok {
        return do_reenable_model(db, provider, api_model).await;
    } else if !is_enabled && manual_locked {
        // Skip: admin lo ha disabilitato manualmente, non riabilitare anche se ricompare.
        tracing::debug!(
            "catalog_sync[{}]: skip re-enable '{}' (manual_locked)",
            provider,
            api_model
        );
    }
    false
}

/// Esegue l'UPDATE di re-enable di un modello ricomparso nell'API + audit + log.
/// Ritorna `true` se una riga e' stata riabilitata. Estratta da
/// `reenable_existing_model` senza cambi di comportamento.
///
/// Il reason si azzera SOLO se appartiene al ciclo is_enabled: i reason del
/// ciclo tool-capability ('malformed_tool_calls', 'tool_probe_failed:%') vanno
/// PRESERVATI — azzerarli lasciava supports_tool_use=false orfano (reason NULL),
/// irraggiungibile dal ri-test del probe (incidente magistral-small-2509,
/// 2026-06-10).
async fn do_reenable_model(db: &PgPool, provider: &str, api_model: &str) -> bool {
    // "Ricomparso in API" non dice nulla sul suo PREZZO: senza questo guard e' il
    // path che ha ri-abilitato mistral-medium-3 (prezzo ignoto) 78 volte, vanificando
    // ogni disabilitazione. E' il piu' insidioso dei punti di eleggibilita': ignora
    // policy e reason, quindi resuscita cio' che gli altri rami hanno escluso.
    let sql = format!(
        "UPDATE ai_price_catalog SET is_enabled = true, effective_from = NOW(), \
         auto_disabled_at = NULL, \
         auto_disabled_reason = CASE WHEN {tool_reason} \
                                     THEN auto_disabled_reason \
                                     ELSE NULL END, \
         updated_at = NOW() \
         WHERE provider = $1 AND model = $2 \
           AND NOT {price_unknown}",
        tool_reason = crate::tool_capability::TOOL_REASON_PREDICATE_SQL,
        price_unknown = price_unknown_sql(""),
    );
    let res = sqlx::query(&sql)
        .bind(provider)
        .bind(api_model)
        .execute(db)
        .await;
    if let Ok(r) = res {
        if r.rows_affected() > 0 {
            audit_log(db, provider, api_model, "reenabled", json!({})).await;
            tracing::info!(
                "catalog_sync[{}]: re-enabled '{}' (ricomparso API)",
                provider,
                api_model
            );
            return true;
        }
    }
    false
}

/// Probe-on-insert di un nuovo modello appena scoperto (is_enabled=false di
/// default). Chiama il provider con una completion di test e, in base all'esito,
/// abilita/annota il modello. Estratta da `sync_provider` (comportamento
/// invariato): un unico ramo `match` sul risultato del probe.
async fn probe_new_model_on_insert(
    db: &PgPool,
    orch: &Orchestrator,
    provider: &str,
    api_model: &str,
) {
    match probe_model_on_insert(orch, provider, api_model).await {
        ProbeOnInsertResult::Healthy => {
            apply_probe_healthy(db, provider, api_model).await;
        }
        ProbeOnInsertResult::ModelBroken(kind) => {
            apply_probe_model_broken(db, provider, api_model, &kind).await;
        }
        ProbeOnInsertResult::ProviderDown(kind) => {
            apply_probe_provider_down(db, provider, api_model, &kind).await;
        }
        ProbeOnInsertResult::Inconclusive(reason) => {
            tracing::debug!(
                "catalog_sync[{}]: probe inconclusive su '{}': {} -> resta disabled (default)",
                provider,
                api_model,
                reason
            );
        }
        ProbeOnInsertResult::Transient(kind) => {
            // Errore opaco/transitorio al confine gateway/provider: il modello
            // appena scoperto resta disabilitato (default), SENZA reason punitivo.
            // Il model_health_probe periodico lo rivalutera' a provider stabile.
            tracing::debug!(
                "catalog_sync[{}]: probe transitorio su '{}': {} -> resta disabled (riprovato dal probe periodico)",
                provider, api_model, kind
            );
        }
    }
}

/// Ramo `ModelBroken` del probe-on-insert: annota il reason punitivo
/// (`failed_initial_probe:<kind>`) + audit + log. Estratto senza cambi di
/// comportamento.
async fn apply_probe_model_broken(db: &PgPool, provider: &str, api_model: &str, kind: &str) {
    let reason = format!("failed_initial_probe:{}", kind);
    let _ = sqlx::query(
        "UPDATE ai_price_catalog \
         SET auto_disabled_at = NOW(), \
             auto_disabled_reason = $3, updated_at = NOW() \
         WHERE provider = $1 AND model = $2",
    )
    .bind(provider)
    .bind(api_model)
    .bind(&reason)
    .execute(db)
    .await;
    audit_log(
        db,
        provider,
        api_model,
        "probe_failed_on_insert",
        json!({"reason": reason}),
    )
    .await;
    tracing::warn!(
        "catalog_sync[{}]: probe FAIL su nuovo modello '{}' (reason={}) -> resta disabled",
        provider,
        api_model,
        kind
    );
}

/// Ramo `ProviderDown` del probe-on-insert: provider giu' (quota/billing/auth),
/// non possiamo sapere se il modello e' valido. Lasciamo disabled con
/// motivazione esplicita: il model_health_probe worker (run periodico) lo
/// riabilitera' quando il provider torna up E il probe passa. Estratto senza
/// cambi di comportamento.
async fn apply_probe_provider_down(db: &PgPool, provider: &str, api_model: &str, kind: &str) {
    let reason = format!("provider_down_on_insert:{}", kind);
    let _ = sqlx::query(
        "UPDATE ai_price_catalog SET auto_disabled_reason = $3, updated_at = NOW() \
         WHERE provider = $1 AND model = $2",
    )
    .bind(provider)
    .bind(api_model)
    .bind(&reason)
    .execute(db)
    .await;
    tracing::info!(
        "catalog_sync[{}]: probe inconclusive (provider down: {}) su '{}' -> resta disabled, model_health_probe lo rivedra'",
        provider, kind, api_model
    );
}

/// Ramo `Healthy` del probe-on-insert: infersce capabilities/tier dal nome,
/// classifica i flag canonici (ADR 0024) e applica il gate allowlist (ADR 0025)
/// abilitando il modello SOLO se ammesso dalla model_selection_policy. Scrive i
/// flag SOLO su righe 'auto' (le 'manual' restano intatte). Estratta da
/// `sync_provider` senza cambi di comportamento.
async fn apply_probe_healthy(db: &PgPool, provider: &str, api_model: &str) {
    // Infer capabilities dal nome: il modello e' ora utilizzabile, ma il routing
    // filtra per capability matching e capabilities=[] lo renderebbe invisibile.
    // Popoliamo SOLO se attualmente vuoto (rispetta override admin).
    let inferred_caps = infer_capabilities_from_name(provider, api_model);
    let caps_json = json!(inferred_caps);
    // Classificazione flag canonici (ADR 0024): discovery -> solo euristica nome
    // (niente metadata LiteLLM). Scritti SOLO su righe 'auto'.
    let vision_routable = load_vision_routable(db).await;
    let cc = classify_capabilities(provider, api_model, None, None, None, &vision_routable);
    // Gate allowlist (ADR 0025): abilita SOLO se il modello e' ammesso dalla
    // model_selection_policy. Cosi' i modelli legacy (pruned dalla 0320) non
    // rientrano via probe-on-insert.
    let allowed = model_passes_selection_policy(db, provider, api_model).await;
    write_probe_healthy_flags(db, provider, api_model, &caps_json, &cc, allowed).await;
    audit_log(
        db, provider, api_model,
        if allowed { "probe_ok_on_insert" } else { "probe_ok_but_outside_policy" },
        json!({"action": if allowed {"auto_enabled"} else {"kept_disabled_outside_allowlist"}, "inferred_capabilities": inferred_caps}),
    )
    .await;
    tracing::info!(
        "catalog_sync[{}]: probe OK su '{}' -> {} (policy allowlist)",
        provider,
        api_model,
        if allowed {
            "abilitato"
        } else {
            "lasciato disabilitato (fuori allowlist)"
        }
    );
}

/// UPDATE dei flag canonici (ADR 0024) + is_enabled dopo un probe Healthy.
/// Scrive `capabilities` solo se attualmente vuoto e i flag SOLO su righe
/// 'auto' (le 'manual' restano intatte).
///
/// `is_enabled` diventa `allowed` (gate allowlist ADR 0025) **E** prezzo noto: un
/// probe che risponde dimostra che il modello FUNZIONA, non che sappiamo quanto
/// costa. I due gate sono ortogonali e vanno entrambi superati. Il gate prezzo
/// tocca solo l'abilitazione: le capability inferite si scrivono comunque, cosi'
/// il giorno in cui il listino arriva la riga e' gia' completa.
/// `$7` = `allowed` (booleano), `$8` = la policy (testo). I due NON sono
/// intercambiabili: fino al 30/07/2026 questo statement usava `$8` sia dove
/// serviva il booleano (`is_enabled = ($8 AND NOT ...)`) sia dove serviva il
/// testo (`agentic_thinking_policy = ... THEN $8`), e `$7` non compariva mai.
/// Postgres deduce il tipo di un parametro dal primo uso, quindi rifiutava lo
/// statement al parse — «in CASE i tipi text e boolean non combaciano» — e il
/// `let _ =` sull'`execute` inghiottiva l'errore. La funzione non ha mai scritto
/// una riga, in silenzio, per tutta la sua vita: il probe dichiarava "modello
/// abilitato" nei log e nell'audit mentre il catalog restava intatto.
///
/// L'errore ora si logga (regola M: l'esito viene dal `Result` di sqlx, non
/// dall'assenza di sintomi). Resta non fatale — un flag non scritto non deve
/// interrompere il giro di sync — ma smette di essere invisibile.
async fn write_probe_healthy_flags(
    db: &PgPool,
    provider: &str,
    api_model: &str,
    caps_json: &Value,
    cc: &ClassifiedCaps,
    allowed: bool,
) {
    let price_unknown = price_unknown_sql("");
    let sql = format!(
        "UPDATE ai_price_catalog \
         SET is_enabled = ($7 AND NOT {price_unknown}), \
             last_probe_healthy_at = CASE WHEN ($7 AND NOT {price_unknown}) THEN NOW() ELSE last_probe_healthy_at END, \
             auto_disabled_at = CASE WHEN ($7 AND NOT {price_unknown}) THEN NULL ELSE NOW() END, \
             auto_disabled_reason = CASE \
                 WHEN ($7 AND NOT {price_unknown}) THEN NULL \
                 WHEN {price_unknown} THEN '{PRICE_UNKNOWN_REASON}' \
                 ELSE 'fuori model_selection_policy (mig 0320)' END, \
             updated_at = NOW(), \
             capabilities = CASE \
                 WHEN capabilities IS NULL OR capabilities = '[]'::jsonb \
                     THEN $3::jsonb \
                 ELSE capabilities \
             END, \
             supports_tool_use = CASE WHEN capability_source='auto' THEN $4 ELSE supports_tool_use END, \
             supports_vision = CASE WHEN capability_source='auto' THEN $5 ELSE supports_vision END, \
             uses_thinking_mode = CASE WHEN capability_source='auto' THEN $6 ELSE uses_thinking_mode END, \
             agentic_thinking_policy = CASE WHEN capability_source='auto' THEN $8 ELSE agentic_thinking_policy END \
         WHERE provider = $1 AND model = $2 AND is_enabled = false",
    );
    if let Err(e) = sqlx::query(&sql)
        .bind(provider)
        .bind(api_model)
        .bind(caps_json)
        .bind(cc.supports_tool_use)
        .bind(cc.supports_vision)
        .bind(cc.uses_thinking_mode)
        .bind(allowed)
        .bind(cc.agentic_thinking_policy)
        .execute(db)
        .await
    {
        tracing::warn!(
            provider,
            model = api_model,
            error = %e,
            "catalog_sync: scrittura flag post-probe fallita (catalog invariato)"
        );
    }
}

/// Verdetto sulla salute recente di un modello secondo l'ACCOUNT (regola Q:
/// l'ignoto e' una variante, mai un valore comodo).
///
/// La terza variante non e' un dettaglio diagnostico: la conseguenza del
/// verdetto e' una SCRITTURA (disabilitazione della riga di catalog), e
/// collassare l'errore di interrogazione su `NonSano` significa che un DB che
/// non risponde disabilita modelli sani. Su `NonInterrogabile` non si scrive:
/// il prossimo giro del sync ripone la domanda.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaluteModello {
    /// Un fatto dice che il modello risponde ancora per questo account.
    Sano,
    /// Un fatto dice il contrario: probe recente fallito, o fallimenti
    /// consecutivi registrati nel catalog.
    NonSano,
    /// Nessun fatto: la domanda non ha potuto raggiungere il DB.
    NonInterrogabile,
}

/// FIX 2: verifica se un modello e' "recentemente sano" secondo l'account,
/// non secondo la lista upstream. Usato dal catalog_sync per NON disabilitare
/// modelli assenti da upstream ma ancora funzionanti per l'account.
///
/// Sano = (a) esiste un health check recente con healthy=true entro la finestra
///         `agent.catalog_sync_health_window_hours` (default 24h), OPPURE
///        (b) il catalog riporta consecutive_failures=0 per il modello (mai
///         fallito un probe model-specific).
async fn salute_modello_recente(db: &PgPool, provider: &str, model: &str) -> SaluteModello {
    let window_hours = load_health_window_hours(db).await;

    // (a) Ultimo health check recente healthy=true.
    let recent_healthy = sqlx::query_scalar::<_, bool>(
        "SELECT healthy
           FROM ai_model_health_history
          WHERE provider = $1 AND model = $2
            AND checked_at >= NOW() - make_interval(hours => $3)
          ORDER BY checked_at DESC
          LIMIT 1",
    )
    .bind(provider)
    .bind(model)
    .bind(window_hours as i32)
    .fetch_optional(db)
    .await;

    match recent_healthy {
        Err(e) => {
            tracing::warn!(
                provider,
                model,
                error = %e,
                "catalog_sync: storico health non interrogabile (nessun verdetto di salute)"
            );
            SaluteModello::NonInterrogabile
        }
        Ok(Some(true)) => SaluteModello::Sano,
        Ok(Some(false)) => SaluteModello::NonSano,
        // (b) Fallback: nessun probe recente, ma il catalog non registra
        // fallimenti model-specific consecutivi -> modello ancora valido.
        Ok(None) => model_has_no_consecutive_failures(db, provider, model).await,
    }
}

/// Finestra di freschezza health DB-driven (setting
/// `agent.catalog_sync_health_window_hours`, default 24h; regola G: niente
/// hardcode magico). Estratta da `model_recently_healthy`.
async fn load_health_window_hours(db: &PgPool) -> i64 {
    get_setting(db, "agent.catalog_sync_health_window_hours")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|h| *h > 0)
        .unwrap_or(24)
}

/// Fallback (b) di [`salute_modello_recente`]: nessun probe recente, ma il
/// catalog non registra fallimenti model-specific consecutivi
/// (`consecutive_failures=0`, cioe' nessun probe l'ha trovato rotto) ->
/// modello ancora valido.
///
/// Riga ASSENTE (`Ok(None)`) e' un fatto, non un ignoto: non esiste nulla da
/// preservare, quindi `NonSano`. Solo l'errore di interrogazione e' ignoto.
async fn model_has_no_consecutive_failures(
    db: &PgPool,
    provider: &str,
    model: &str,
) -> SaluteModello {
    let cf = sqlx::query_scalar::<_, i32>(
        "SELECT consecutive_failures FROM ai_price_catalog
          WHERE provider = $1 AND model = $2",
    )
    .bind(provider)
    .bind(model)
    .fetch_optional(db)
    .await;
    match cf {
        Err(e) => {
            tracing::warn!(
                provider,
                model,
                error = %e,
                "catalog_sync: consecutive_failures non interrogabile (nessun verdetto di salute)"
            );
            SaluteModello::NonInterrogabile
        }
        Ok(Some(0)) => SaluteModello::Sano,
        Ok(_) => SaluteModello::NonSano,
    }
}

async fn audit_log(db: &PgPool, provider: &str, model: &str, action: &str, details: Value) {
    let _ = sqlx::query(
        "INSERT INTO ai_price_catalog_audit (provider, model, action, details) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(provider)
    .bind(model)
    .bind(action)
    .bind(details)
    .execute(db)
    .await;
}

/// Discovery dei modelli live di un provider tramite il Nexus Gateway
/// (`GET /v1/models/{provider}`), via UNICA per l'autodiscovery (regola L).
///
/// Il gateway incapsula l'auth di OGNI provider — incluso Vertex con Service
/// Account — quindi qui non servono api_key ne' la vecchia delega al brain per
/// Google (`/providers/google/models/live`): un solo punto di accesso HTTP
/// (`NexusGatewayClient::list_models`) sostituisce le chiamate dirette agli
/// endpoint `api.{provider}.com/v1/models`.
///
/// La logica a valle (filtro chat-compat, infer capabilities/tier, classify,
/// upsert) resta invariata: cambia solo la SORGENTE della lista nomi modello.
async fn fetch_provider_models(
    orchestrator: Option<&Orchestrator>,
    provider: &str,
) -> anyhow::Result<Vec<crate::nexus_gateway::GwModelMeta>> {
    // L'orchestrator puo' mancare (chiamata fuori dal server); il suo gateway no.
    let orch = orchestrator.ok_or_else(|| {
        anyhow::anyhow!("orchestrator non disponibile: autodiscovery modelli impossibile")
    })?;
    orch.nexus_gateway.list_models(provider).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Soglie del tier relativo che quattro test seminavano con lo STESSO
    /// `INSERT` multi-riga ricopiato: una sola definizione, e il seed passa dal
    /// punto unico (che invalida la cache di lettura, chiave per chiave).
    const SOGLIE_TIER_RELATIVO: &[(&str, &str)] = &[
        ("catalog.tier_prior.enabled", "true"),
        ("catalog.tier_relative.frontier_pct", "0.85"),
        ("catalog.tier_relative.heavy_pct", "0.65"),
        ("catalog.tier_relative.high_pct", "0.45"),
        ("catalog.tier_relative.medium_pct", "0.20"),
        ("catalog.tier_relative.anchor", "54"),
        ("catalog.tier_relative.anchor_model", "p/leader"),
        ("catalog.tier_relative.anchor_at", ""),
        ("catalog.tier_relative.anchor_deadband_pct", "0.03"),
        ("catalog.agentic_index_sync.max_age_hours", "168"),
    ];


    #[test]
    fn test_is_chat_compatible_no_per_name_blacklist() {
        // La blacklist hardcoded per famiglia gemini-3.x e' stata RIMOSSA:
        // il filtraggio dei modelli fantasma e' ora fatto dal probe attivo
        // (probe_model_on_insert) al primo discovery. La blacklist semantica
        // qui sotto resta solo per i modelli che per DESIGN non sono chat
        // completion (TTS, embedding, realtime, instruct legacy).
        // Quindi questi devono passare la blacklist (e poi il probe deciderà):
        assert!(is_chat_compatible_model("gemini-3.5-flash"));
        assert!(is_chat_compatible_model("gemini-3-pro-preview"));
        assert!(is_chat_compatible_model("gemini-1.0-pro"));
        assert!(is_chat_compatible_model("gemini-2.5-pro"));
        assert!(is_chat_compatible_model("gpt-5-future"));
        assert!(is_chat_compatible_model("claude-5-sonnet"));
    }

    #[test]
    fn test_is_chat_compatible_blacklist_other_providers() {
        // Smoke test: la blacklist storica continua a funzionare.
        assert!(!is_chat_compatible_model("tts-1"));
        assert!(!is_chat_compatible_model("text-embedding-3-small"));
        assert!(!is_chat_compatible_model("gpt-4o-realtime-preview"));
        assert!(!is_chat_compatible_model("whisper-1"));
        // Modelli chat veri restano OK.
        assert!(is_chat_compatible_model("claude-sonnet-4-6"));
        assert!(is_chat_compatible_model("gpt-4o"));
        assert!(is_chat_compatible_model("mistral-large-latest"));
    }

    #[test]
    fn test_classify_media_kind() {
        // image_gen: dall-e / imagen / gpt-image / *-image / nano-banana.
        assert_eq!(classify_media_kind("dall-e-3"), Some("image_gen"));
        assert_eq!(classify_media_kind("dall-e-2"), Some("image_gen"));
        assert_eq!(
            classify_media_kind("imagen-3.0-generate-002"),
            Some("image_gen")
        );
        assert_eq!(classify_media_kind("gpt-image-1"), Some("image_gen"));
        assert_eq!(
            classify_media_kind("gemini-2.5-flash-image"),
            Some("image_gen")
        );
        assert_eq!(classify_media_kind("gemini-nano-banana"), Some("image_gen"));
        // audio_in: whisper / transcribe / voxtral.
        assert_eq!(classify_media_kind("whisper-1"), Some("audio_in"));
        assert_eq!(classify_media_kind("gpt-4o-transcribe"), Some("audio_in"));
        assert_eq!(
            classify_media_kind("voxtral-small-latest"),
            Some("audio_in")
        );
        // audio_out: tts.
        assert_eq!(classify_media_kind("tts-1"), Some("audio_out"));
        assert_eq!(classify_media_kind("tts-1-hd"), Some("audio_out"));
        assert_eq!(classify_media_kind("gpt-4o-mini-tts"), Some("audio_out"));
        // video_gen: veo / sora.
        assert_eq!(classify_media_kind("veo-2.0"), Some("video_gen"));
        assert_eq!(classify_media_kind("sora"), Some("video_gen"));
        // NON-media (chat/vision/embedding) -> None.
        assert_eq!(classify_media_kind("gpt-4o"), None);
        assert_eq!(classify_media_kind("claude-sonnet-4-6"), None);
        assert_eq!(classify_media_kind("gemini-2.5-pro"), None);
        assert_eq!(classify_media_kind("text-embedding-3-small"), None);
        assert_eq!(classify_media_kind("mistral-large-latest"), None);
    }

    #[test]
    fn test_media_kind_column() {
        assert_eq!(media_kind_column("image_gen"), Some("supports_image_gen"));
        assert_eq!(media_kind_column("audio_in"), Some("supports_audio_in"));
        assert_eq!(media_kind_column("audio_out"), Some("supports_audio_out"));
        assert_eq!(media_kind_column("video_gen"), Some("supports_video_gen"));
        assert_eq!(media_kind_column("bogus"), None);
    }

    #[test]
    fn test_strip_date_suffix() {
        assert_eq!(strip_date_suffix("claude-sonnet-4-6"), "claude-sonnet-4-6");
        assert_eq!(
            strip_date_suffix("claude-sonnet-4-6-20251201"),
            "claude-sonnet-4-6"
        );
        assert_eq!(strip_date_suffix("gpt-4o-mini"), "gpt-4o-mini");
        // Il formato ISO ORA viene rimosso: prima no, e il test lo asseriva — ma
        // il chiamante vuole proprio riconoscere l'alias della sua base (vedi il
        // doc di strip_date_suffix). Consolidamento delle due copie omonime.
        assert_eq!(strip_date_suffix("gpt-4o-mini-2024-07-18"), "gpt-4o-mini");
        // NON e' una data: 4 cifre. 'ministral-8b-2512' e' un nome di modello.
        assert_eq!(strip_date_suffix("ministral-8b-2512"), "ministral-8b-2512");
    }

    // ── ADR 0024: classificatore unico delle capability ──

    /// Routable di test: i provider con ramo vision.py (setting reale: mig 0373).
    fn rt() -> std::collections::HashSet<String> {
        ["google", "anthropic", "openai"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn classify_metadata_litellm_ha_priorita() {
        // function_calling/vision espliciti vincono sull'euristica.
        let c = classify_capabilities(
            "openai",
            "gpt-4o",
            Some(true),
            Some(true),
            Some(false),
            &rt(),
        );
        assert!(c.supports_tool_use);
        assert!(c.supports_vision);
        assert_eq!(c.agentic_thinking_policy, "none");
        assert!(!c.uses_thinking_mode);
    }

    /// Un modello MEDIA non e' un modello di testo: il classificatore lo spegne
    /// PRIMA di ogni euristica. Senza questo guard, il default
    /// `meta_tool_use.unwrap_or(true)` del path LiteLLM marcava whisper-1/tts-1
    /// come tool-capable: modelli audio dentro il pool agentico dei consiglieri
    /// (dato sporco misurato il 16/07, ripulito dalla mig 0608).
    #[test]
    fn classify_un_media_non_e_mai_tool_capable() {
        for model in ["whisper-1", "tts-1", "tts-1-hd", "gpt-image-1", "gpt-4o-transcribe"] {
            // Anche con metadata assente (il caso LiteLLM che sporcava i dati).
            let c = classify_capabilities("openai", model, None, None, None, &rt());
            assert!(!c.supports_tool_use, "{model}: un media non fa tool-loop");
            assert!(!c.supports_vision, "{model}: un media non e' un modello vision");
            assert_eq!(c.agentic_thinking_policy, "none");
            // E nemmeno un metadata bugiardo lo promuove.
            let c = classify_capabilities("openai", model, Some(true), Some(true), None, &rt());
            assert!(
                !c.supports_tool_use,
                "{model}: il guard media vince sul metadata"
            );
        }
    }

    #[test]
    fn classify_o_series_e_reasoning_only_tool_nativi() {
        // o-series: reasoning-only -> tool NATIVI (policy 'native') + thinking
        // mode. NB: la vecchia colonna is_thinking diceva "escludi da agentico"
        // mentre la policy dice 'native' (dentro, con tool nativi): la
        // contraddizione fra le due colonne e' il motivo per cui la colonna
        // e' stata rimossa (mig 0608, ADR 0025).
        let c = classify_capabilities("openai", "o3-mini", None, None, None, &rt());
        assert_eq!(c.agentic_thinking_policy, "native");
        assert!(c.uses_thinking_mode);
    }

    #[test]
    fn classify_deepseek_v4_reasoning_only() {
        // deepseek-v4-pro: linea reasoning -> thinking mode attivo, dual-mode
        // nei tool-loop (policy asserita nel test di famiglia qui sotto).
        let c = classify_capabilities("deepseek", "deepseek-v4-pro", None, None, None, &rt());
        assert!(c.uses_thinking_mode);
    }

    #[test]
    fn classify_deepseek_v4_famiglia_tool_capable() {
        // Verita' di famiglia (verificata live 2026-06-10): i deepseek-v4 fanno
        // function calling. tool_use=true ANCHE con metadata assente o false
        // (il FALSE storico era un degrado runtime, non una verita' provider).
        // Policy dual-mode: 'disable_for_tools' (non-thinking nei tool-loop).
        for model in ["deepseek-v4-pro", "deepseek-v4-flash"] {
            for meta in [None, Some(false)] {
                let c = classify_capabilities("deepseek", model, meta, None, None, &rt());
                assert!(
                    c.supports_tool_use,
                    "{model} (meta_tool_use={meta:?}) deve restare tool-capable"
                );
                assert_eq!(c.agentic_thinking_policy, "disable_for_tools");
            }
        }
    }

    #[test]
    fn classify_claude_ibrido_agentico_ma_thinking_mode() {
        // Claude opus/sonnet: NON escluso da agentico (A=false) ma extended
        // thinking -> non forzare tool_choice (B=true). Caso che il merge naïf
        // avrebbe rotto.
        let c = classify_capabilities("anthropic", "claude-sonnet-4-6", None, None, None, &rt());
        assert_eq!(
            c.agentic_thinking_policy, "disable_for_tools",
            "Claude ibrido: agentic-eligibile, non-thinking nei tool-loop"
        );
        assert!(
            c.uses_thinking_mode,
            "Claude usa extended thinking -> non forzare"
        );
        assert!(c.supports_vision);
    }

    #[test]
    fn classify_modello_chat_standard_non_thinking() {
        // mistral-large: tool-capable, non-thinking -> candidato agentico ideale.
        let c = classify_capabilities("mistral", "mistral-large-2411", None, None, None, &rt());
        assert!(c.supports_tool_use);
        assert_eq!(c.agentic_thinking_policy, "none");
        assert!(!c.uses_thinking_mode);
    }

    #[test]
    fn classify_vision_solo_provider_instradabili() {
        // La capability vision riflette l'instradabilita' da vision.py (google,
        // anthropic, openai). I provider SENZA ramo vision (mistral, deepseek)
        // devono avere supports_vision=false ANCHE se LiteLLM passa meta_vision=true
        // (falso positivo): altrimenti best_model_for_tier li sceglierebbe per il
        // purpose vision_describe -> /vision/describe 501.
        let mistral = classify_capabilities(
            "mistral",
            "mistral-small-latest",
            None,
            Some(true),
            None,
            &rt(),
        );
        assert!(
            !mistral.supports_vision,
            "mistral non e' instradabile da vision.py: vision=false"
        );
        let pixtral = classify_capabilities(
            "mistral",
            "pixtral-large-latest",
            None,
            Some(true),
            None,
            &rt(),
        );
        assert!(
            !pixtral.supports_vision,
            "pixtral non e' instradabile finche' vision.py non supporta mistral"
        );
        let deepseek =
            classify_capabilities("deepseek", "deepseek-chat", None, Some(true), None, &rt());
        assert!(
            !deepseek.supports_vision,
            "deepseek non e' instradabile: vision=false"
        );
        // Provider instradabili: vision corretta.
        assert!(
            classify_capabilities("google", "gemini-2.5-flash-lite", None, None, None, &rt())
                .supports_vision
        );
        assert!(
            classify_capabilities("anthropic", "claude-haiku-4-5", None, None, None, &rt())
                .supports_vision
        );
        assert!(classify_capabilities("openai", "gpt-4o", None, None, None, &rt()).supports_vision);
        // Per i provider instradabili, meta_vision esplicito e' rispettato.
        assert!(
            classify_capabilities("openai", "gpt-4o-mini", None, Some(true), None, &rt())
                .supports_vision
        );
    }

    #[test]
    fn classify_gemini_25_thinking_dual_mode() {
        // Gemini 2.5 (pro E flash) ha il thinking attivo di default: e' dual-mode,
        // NON reasoning-only. Eleggibile all'agentico (policy 'disable_for_tools',
        // non-thinking nei tool-loop -> niente MALFORMED_FUNCTION_CALL),
        // uses_thinking_mode=true. flash-lite NON ha thinking.
        let flash = classify_capabilities("google", "gemini-2.5-flash", None, None, None, &rt());
        assert!(flash.uses_thinking_mode, "gemini-2.5-flash e' thinking");
        assert_eq!(
            flash.agentic_thinking_policy, "disable_for_tools",
            "gemini-2.5-flash NON va escluso dall'agentico"
        );
        let pro = classify_capabilities("google", "gemini-2.5-pro", None, None, None, &rt());
        assert_eq!(
            pro.agentic_thinking_policy, "disable_for_tools",
            "gemini-2.5-pro e' dual-mode, non reasoning-only"
        );
        let lite =
            classify_capabilities("google", "gemini-2.5-flash-lite", None, None, None, &rt());
        assert!(
            !lite.uses_thinking_mode,
            "gemini-2.5-flash-lite NON e' thinking"
        );
        assert_eq!(lite.agentic_thinking_policy, "none");
    }

    // ── ADR 0025: agentic_thinking_policy per famiglia ──

    #[test]
    fn classify_agentic_thinking_policy_per_famiglia() {
        let p = |prov, model| {
            classify_capabilities(prov, model, None, None, None, &rt()).agentic_thinking_policy
        };
        // Reasoning-only senza function calling -> exclude.
        assert_eq!(p("deepseek", "deepseek-reasoner"), "exclude");
        // Legacy completion/chat: esclusi dall'agentico (inadatti ai tool-loop),
        // restano per i path non-agentici.
        assert_eq!(p("deepseek", "deepseek-coder"), "exclude");
        assert_eq!(p("deepseek", "deepseek-chat"), "exclude");
        // I V4 reasoning dual-mode restano eleggibili (disable_for_tools).
        assert_eq!(p("deepseek", "deepseek-v4-flash"), "disable_for_tools");
        assert_eq!(p("deepseek", "deepseek-v4-pro"), "disable_for_tools");
        // OpenAI o-series: tool nativi -> native.
        assert_eq!(p("openai", "o3-mini"), "native");
        // Dual-mode (thinking + tool-capable) -> disable_for_tools.
        assert_eq!(p("deepseek", "deepseek-v4-pro"), "disable_for_tools");
        assert_eq!(p("anthropic", "claude-sonnet-4-6"), "disable_for_tools");
        assert_eq!(p("google", "gemini-2.5-pro"), "disable_for_tools");
        assert_eq!(p("mistral", "magistral-medium-latest"), "disable_for_tools");
        // Modelli non-thinking standard -> none.
        assert_eq!(p("openai", "gpt-4o"), "none");
        assert_eq!(p("mistral", "mistral-large-2411"), "none");
        // Gemini 2.5 flash/pro: thinking di default -> disable_for_tools.
        // flash-lite NON e' thinking -> none.
        assert_eq!(p("google", "gemini-2.5-flash"), "disable_for_tools");
        assert_eq!(p("google", "gemini-2.5-flash-lite"), "none");
    }

    #[test]
    fn classify_magistral_tool_use_uniforme_per_famiglia() {
        // I magistral (linea reasoning Mistral) supportano function calling
        // (doc Mistral). La classificazione DEVE essere uniforme per l'intera
        // famiglia, indipendentemente dalla variante e dal metadata LiteLLM
        // (che e' incoerente: alcune varianti ritornano tool_use=false). Tutte:
        //   - supports_tool_use=true (verita' Mistral, per famiglia)
        //   - uses_thinking_mode=true (linea reasoning)
        //   - agentic_thinking_policy='disable_for_tools' (non-thinking nei tool)
        for model in [
            "magistral-small-latest",
            "magistral-medium-latest",
            "magistral-small-2509",
            "magistral-medium-2509",
            "magistral-medium-1-2-2509",
        ] {
            // Metadata assente.
            let c = classify_capabilities("mistral", model, None, None, None, &rt());
            assert!(
                c.supports_tool_use,
                "{model}: tool_use deve essere true (doc Mistral)"
            );
            assert!(
                c.uses_thinking_mode,
                "{model}: e' reasoning -> uses_thinking_mode"
            );
            assert_eq!(
                c.agentic_thinking_policy, "disable_for_tools",
                "{model}: reasoning dual-mode -> disable_for_tools"
            );
            // REGRESSIONE: anche col metadata LiteLLM incoerente (tool_use=false),
            // la classificazione per famiglia forza true. E' il bug che causava il
            // degrado a supports_tool_use=false di alcune varianti magistral.
            let c_bad_meta =
                classify_capabilities("mistral", model, Some(false), None, None, &rt());
            assert!(
                c_bad_meta.supports_tool_use,
                "{model}: il metadata tool_use=false di LiteLLM va ignorato per la famiglia magistral"
            );
        }
    }

    // Invarianti della reconciliation policy->catalog. Test puri (no DB): le
    // query SQL set-based sono il punto sensibile, qui ne fissiamo i predicati
    // che proteggono lock manuali, media e whitelist dei reason di policy.
    #[test]
    fn test_reconcile_manual_lock_predicate() {
        // Entrambi i segnali di lock manuale devono essere presenti: la colonna
        // reale capability_source='manual' e la convenzione auto_disabled_reason
        // LIKE 'manual:%'. Rimuoverne uno riabiliterebbe modelli lockati a mano.
        assert!(RECONCILE_MANUAL_LOCKED_SQL.contains("capability_source = 'manual'"));
        assert!(RECONCILE_MANUAL_LOCKED_SQL.contains("auto_disabled_reason LIKE 'manual:%'"));
    }

    #[test]
    fn test_price_unknown_predicate_nomina_solo_unknown() {
        // REGRESSIONE (misurata): 13 modelli a prezzo IGNOTO erano routabili e
        // hanno fatturato 873 chiamate / 16,4M token a costo 0.
        //
        // Il predicato deve nominare 'unknown' ESPLICITAMENTE: 'free' e' il terzo
        // stato ammesso dal CHECK (mig 0477) ed e' un gratuito REALE, che deve
        // restare routabile. Un predicato scritto come "<> 'priced'" escluderebbe
        // anche i modelli gratuiti — errore silenzioso e opposto all'intento.
        let p = price_unknown_sql("c");
        assert!(p.contains("pricing_state = 'unknown'"), "predicato: {p}");
        assert!(!p.contains("priced"), "non deve ragionare per negazione: {p}");
        assert!(!p.contains("free"), "il gratuito reale resta routabile: {p}");
        // Mai dedurre lo stato dal costo (regola M).
        assert!(
            !p.contains("cost"),
            "lo stato non si deduce dalla grandezza del costo: {p}"
        );
    }

    #[test]
    fn test_price_unknown_predicate_alias() {
        // Con alias (query con FROM) e senza (UPDATE diretta): stesso punto unico,
        // due forme. Un alias sbagliato fa fallire la query a runtime, non a build.
        assert_eq!(price_unknown_sql("c"), "(c.pricing_state = 'unknown')");
        assert_eq!(price_unknown_sql(""), "(pricing_state = 'unknown')");
    }

    #[test]
    fn test_reconcile_media_excluded() {
        // I modelli media non rientrano nella chat policy: tutte e quattro le
        // capability media devono comparire nel predicato di esclusione.
        for cap in [
            "supports_image_gen",
            "supports_audio_in",
            "supports_audio_out",
            "supports_video_gen",
        ] {
            assert!(
                RECONCILE_IS_MEDIA_SQL.contains(cap),
                "predicato media privo di {cap}"
            );
        }
    }

    #[test]
    fn test_reconcile_passes_policy_matches_single_source() {
        // La condizione "passa la policy" set-based deve essere la STESSA di
        // model_passes_selection_policy (punto unico, regola L): allowed con
        // fallback cardinality=0, e negazione dei denied.
        assert!(RECONCILE_PASSES_POLICY_SQL.contains("c.model ~ ANY(p.allowed_patterns)"));
        assert!(RECONCILE_PASSES_POLICY_SQL.contains("cardinality(p.allowed_patterns) = 0"));
        assert!(RECONCILE_PASSES_POLICY_SQL.contains("NOT ( c.model ~ ANY(p.denied_patterns) )"));
    }
    /// Lo strip della DATA e' sicuro: e' lo stesso modello. Misurato sul parco:
    /// porta la copertura da 28% a 39% con ZERO ambiguita'.
    #[test]
    fn la_chiave_ignora_provider_separatori_e_data() {
        // Casi REALI del catalog (i nostri id vs quelli di OpenRouter).
        assert_eq!(
            normalize_model_key("claude-opus-4-5-20251101"),
            normalize_model_key("anthropic/claude-opus-4.5")
        );
        assert_eq!(
            normalize_model_key("gpt-5.4-mini-2026-03-17"),
            normalize_model_key("openai/gpt-5.4-mini")
        );
        assert_eq!(
            normalize_model_key("claude-haiku-4-5-20251001"),
            normalize_model_key("anthropic/claude-haiku-4.5")
        );
    }

    /// IL CONFINE: niente prefix-match. `gpt-5-nano` NON e' `gpt-5` — prendersi
    /// l'indice del flagship significherebbe promuovere un nano a heavy. E' un
    /// errore gia' commesso in una prima analisi manuale (o3 -> o3-mini-high).
    #[test]
    fn un_nano_non_e_il_suo_flagship() {
        assert_ne!(normalize_model_key("gpt-5-nano"), normalize_model_key("gpt-5"));
        assert_ne!(normalize_model_key("gpt-5.4-mini"), normalize_model_key("gpt-5.4"));
        assert_ne!(normalize_model_key("o3-mini-high"), normalize_model_key("o3"));
        // E una data NON in coda non e' un suffisso data.
        assert_eq!(normalize_model_key("gpt-4o-2024-05-13-preview"),
                   "gpt4o20240513preview".replace('-', ""));
    }

    /// Il parsing del payload REALE di OpenRouter (forma verificata sull'API viva
    /// il 16/07): l'indice sta in benchmarks.artificial_analysis.agentic_index.
    #[test]
    fn estrae_l_indice_dal_payload_reale() {
        let payload = json!({"data": [
            {"id": "openai/gpt-5.6-sol",
             "benchmarks": {"artificial_analysis": {"intelligence_index": 58.9,
                                                    "coding_index": 77.4,
                                                    "agentic_index": 54.0}}},
            {"id": "mistralai/mistral-large-2512",
             "benchmarks": {"artificial_analysis": {"agentic_index": 5.5}}},
            // Senza benchmarks: il campo e' omesso per i modelli non valutati.
            {"id": "openai/gpt-image-1"},
            // Solo Design Arena (l'UNICA fonte documentata): niente indice.
            {"id": "x/y", "benchmarks": {"design_arena": [{"elo": 1172}]}}
        ]});
        let m = parse_agentic_index_payload(&payload);
        assert_eq!(m.len(), 2, "solo i modelli con agentic_index");
        assert_eq!(m.get(&normalize_model_key("gpt-5.6-sol")), Some(&54.0));
        assert_eq!(m.get(&normalize_model_key("mistral-large-2512")), Some(&5.5));
    }

    /// Una chiave AMBIGUA viene scartata: meglio NESSUN indice che quello del
    /// modello sbagliato. Un indice errato promuove un modello nel routing; uno
    /// assente lascia il tier NULL finche' la batteria non misura.
    #[test]
    fn le_chiavi_ambigue_vengono_scartate() {
        let payload = json!({"data": [
            {"id": "a/modello-20250101", "benchmarks": {"artificial_analysis": {"agentic_index": 10.0}}},
            {"id": "b/modello-20260101", "benchmarks": {"artificial_analysis": {"agentic_index": 50.0}}}
        ]});
        // Entrambi normalizzano a "modello" ma con indici DIVERSI.
        let m = parse_agentic_index_payload(&payload);
        assert!(m.is_empty(), "chiave ambigua -> nessun indice, non uno a caso: {m:?}");
        // Stesso nome, STESSO indice: nessuna ambiguita', si tiene.
        let payload = json!({"data": [
            {"id": "a/modello-20250101", "benchmarks": {"artificial_analysis": {"agentic_index": 10.0}}},
            {"id": "b/modello-20260101", "benchmarks": {"artificial_analysis": {"agentic_index": 10.0}}}
        ]});
        assert_eq!(parse_agentic_index_payload(&payload).len(), 1);
    }
    /// REGRESSIONE (difetto misurato sul campo il 16/07, introdotto da me mentre
    /// curavo lo stesso difetto): il tier deve venire dall'agentic_index, non dal
    /// prezzo.
    ///
    /// `run_catalog_sync` derivava il tier inline coi soli prezzo+finestra (l'indice
    /// li' non e' noto: vive nella riga) e lo scriveva; `refresh_tier_prior` — che
    /// l'indice ce l'ha — girava su un ALTRO path e non lo correggeva. Due punti
    /// per la stessa domanda, e vinceva quello MENO informato. Effetto misurato:
    /// mistral-large-2512 (agentic 5.5, cioe' quintultimo del parco) classificato
    /// 'heavy' perche' costa $0.50 con 262k di finestra, e le inversioni salite a
    /// 90 — peggio del nome, che ne faceva 64.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_tier_viene_dall_indice_non_dal_prezzo(pool: sqlx::PgPool) {
        // Schema REALE (regola O): `ai_price_catalog` (mig 0608) e `settings`
        // (mig 0002) arrivano dalla migrazione. Il DELETE isola dal catalog
        // reale; l'ON CONFLICT sovrascrive le soglie coi valori del test.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        crate::test_support::seed_settings(&pool, SOGLIE_TIER_RELATIVO).await;
        // IL CASO REALE: mistral-large-2512 — costa poco ma ha una finestra enorme
        // (il vecchio prior prezzo+finestra diceva 'heavy') e un agentic_index
        // bassissimo. Solo l'indice parla (mig 0608).
        sqlx::query(
            "INSERT INTO ai_price_catalog              (provider, model, input_cost_per_million_tokens, output_cost_per_million_tokens, context_window,               agentic_index, agentic_index_at, performance_tier, tier_source, currency, last_probe_healthy_at)              VALUES ('mistral', 'mistral-large-2512', 0.5, 0.5, 262144, 5.5, NOW(), NULL, NULL, 'USD', NOW())",
        )
        .execute(&pool)
        .await
        .expect("seed");

        // DIAGNOSI: la scala si carica davvero?
        let t = tier_prior_bands(&pool).await;
        assert!(t.is_some(), "tier_prior_bands ha ritornato None: il prior non parte");
        refresh_tier_prior(&pool, "mistral", "mistral-large-2512").await;

        let (tier, src): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT performance_tier, tier_source FROM ai_price_catalog WHERE model='mistral-large-2512'",
        )
        .fetch_one(&pool)
        .await
        .expect("riga");
        assert_eq!(
            (tier.as_deref(), src.as_deref()),
            (Some("light"), Some("synced")),
            "agentic 5.5 -> light, fonte 'synced'. Col vecchio prior sul prezzo              ($0.50 + 262k di finestra) sarebbe 'heavy': e' il difetto misurato sul campo"
        );
    }

    /// Un indice STANTIO non vale come fresco (la fonte e' undocumented e puo'
    /// sparire), ma il tier gia' presente NON si azzera: `performance_tier` NULL
    /// non matcha il filtro della tier-chain, quindi azzerare toglierebbe il
    /// modello dal routing. Misurato il 16/07: 43 modelli QUALIFICATI sono
    /// scoperti dall'indice (OpenRouter non lista quei nomi) — azzerarli
    /// avrebbe dimezzato il pool. Restano col loro tier finche' la batteria non
    /// scrive 'measured'.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn l_indice_stantio_non_azzera_il_tier_gia_presente(pool: sqlx::PgPool) {
        // Schema REALE (regola O): vedi nota sul test precedente.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        crate::test_support::seed_settings(&pool, SOGLIE_TIER_RELATIVO).await;
        // L'indice e' di un mese fa: STANTIO -> non deriva un tier nuovo. Ma il
        // tier gia' presente resta: toglierlo significherebbe togliere il
        // modello dal routing.
        sqlx::query(
            "INSERT INTO ai_price_catalog              (provider, model, input_cost_per_million_tokens, output_cost_per_million_tokens, context_window,               agentic_index, agentic_index_at, performance_tier, tier_source, currency, last_probe_healthy_at)              VALUES ('mistral', 'vecchio', 0.5, 0.5, 262144, 5.5, NOW() - interval '30 days', 'heavy', NULL, 'USD', NOW())",
        )
        .execute(&pool)
        .await
        .expect("seed");

        let scritte = refresh_tier_prior(&pool, "mistral", "vecchio").await;

        let (tier, src): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT performance_tier, tier_source FROM ai_price_catalog WHERE model='vecchio'",
        )
        .fetch_one(&pool)
        .await
        .expect("riga");
        assert_eq!(scritte, 0, "nessuna scrittura: l'indice stantio non si esprime");
        assert_eq!(
            (tier.as_deref(), src.as_deref()),
            (Some("heavy"), None),
            "indice STANTIO -> il tier resta (il modello continua a essere              routabile) e la fonte resta NULL: 'non so da dove viene', che e' la              verita' finche' la batteria non lo misura"
        );
    }

    /// L'indice deve raggiungere ANCHE i modelli che il listino LiteLLM non
    /// conosce. `refresh_tier_prior` gira solo sui modelli toccati dal sync del
    /// listino o dalla discovery: un modello nuovo con indice fresco restava
    /// senza tier per sempre, con l'indice inerte nella riga. E' il caso reale
    /// dei 6 modelli che la mig 0608 declassa da 'manual' a NULL (fra cui
    /// claude-opus-4-7, indice 44.4, che il listino non elenca).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_giro_set_based_riclassifica_chi_il_listino_non_tocca(pool: sqlx::PgPool) {
        // Schema REALE (regola O): `settings` (mig 0002) ha gia' `updated_at`. Il
        // DELETE isola il catalog dal parco reale: l'algoritmo di "massimo del
        // PARCO" (usato per ri-ancorare) e l'assert finale su tutte le righe
        // dipendono ENTRAMBI dal catalog contenere solo le righe di questo test.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        crate::test_support::seed_settings(&pool, SOGLIE_TIER_RELATIVO).await;
        sqlx::query(
            "INSERT INTO ai_price_catalog              (provider, model, agentic_index, agentic_index_at, performance_tier, tier_source, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at)              VALUES              ('anthropic', 'opus-fossile', 44.4, NOW(), 'medium', NULL, 1.0, 1.0, 'USD', NOW()),              ('x', 'gia-misurato',  10.0, NOW(), 'frontier', 'measured', 1.0, 1.0, 'USD', NOW()),              ('x', 'senza-indice',  NULL, NULL,  'heavy', NULL, 1.0, 1.0, 'USD', NOW())",
        )
        .execute(&pool)
        .await
        .expect("seed");

        let cambiati = refresh_tiers_from_index(&pool).await;

        assert_eq!(cambiati, 1, "solo il fossile con indice va riclassificato");
        let righe: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT model, performance_tier, tier_source FROM ai_price_catalog ORDER BY model",
        )
        .fetch_all(&pool)
        .await
        .expect("righe");
        assert_eq!(
            righe,
            vec![
                ("gia-misurato".into(), Some("frontier".into()), Some("measured".into())),
                // SEMANTICA RELATIVA (mig 0615): 44.4 e' il massimo fresco del
                // parco enabled -> il giro RI-ANCORA (54 -> 44.4, scarto 17.8% >
                // deadband 3%) e il leader del parco E' frontier per definizione.
                ("opus-fossile".into(), Some("frontier".into()), Some("synced".into())),
                ("senza-indice".into(), Some("heavy".into()), None),
            ],
            "l'indice riclassifica il fossile sul leader del PARCO; NON tocca chi              la batteria ha gia' misurato (measured vince), ne' azzera chi              l'indice non copre"
        );
        // L'ancora e' stata PERSISTITA dal punto unico (update_setting_value):
        // il prossimo giro per-modello leggera' 44.4, non il 54 di ieri.
        let ancora: (String,) = sqlx::query_as(
            "SELECT value FROM settings WHERE key = 'catalog.tier_relative.anchor'",
        )
        .fetch_one(&pool)
        .await
        .expect("ancora");
        assert_eq!(ancora.0, "44.4", "l'ancora segue il massimo del parco oltre la deadband");
    }

    /// Quando l'indice CONFERMA il tier gia' presente, la PROVENIENZA deve
    /// aggiornarsi comunque: la riga arriva dalla mig 0608 con tier fossile e
    /// `tier_source` NULL, e l'indice l'ha appena convalidata. Col solo guard
    /// `performance_tier IS DISTINCT FROM $3` l'UPDATE non scattava e il modello
    /// restava a provenienza ignota per sempre — la bugia che questo lavoro
    /// rimuove.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn l_indice_che_conferma_il_tier_ne_registra_la_provenienza(pool: sqlx::PgPool) {
        // Schema REALE (regola O): vedi nota sui test precedenti.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        crate::test_support::seed_settings(&pool, SOGLIE_TIER_RELATIVO).await;
        // Tier 'heavy' fossile (fonte ignota) e indice fresco 36.4 -> heavy: il
        // VALORE non cambia, la PROVENIENZA si'.
        sqlx::query(
            "INSERT INTO ai_price_catalog              (provider, model, agentic_index, agentic_index_at, performance_tier, tier_source, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at)              VALUES ('deepseek', 'v4-pro', 36.4, NOW(), 'heavy', NULL, 1.0, 1.0, 'USD', NOW())",
        )
        .execute(&pool)
        .await
        .expect("seed");

        refresh_tier_prior(&pool, "deepseek", "v4-pro").await;

        let (tier, src): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT performance_tier, tier_source FROM ai_price_catalog WHERE model='v4-pro'",
        )
        .fetch_one(&pool)
        .await
        .expect("riga");
        assert_eq!(
            (tier.as_deref(), src.as_deref()),
            (Some("heavy"), Some("synced")),
            "l'indice conferma 'heavy': il tier resta, ma ora la fonte DICE che              e' stato sincronizzato invece di tacere"
        );
    }

    // ── PEZZO 1: i provider da sincronizzare sono DEDOTTI dal registry ──

    /// Crea `nexus_provider_registry` + `settings` per i test PEZZO 1.
    async fn crea_registry_e_settings(pool: &sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE nexus_provider_registry ( \
                 name TEXT PRIMARY KEY, \
                 activation TEXT, \
                 key_setting TEXT, \
                 enabled_setting TEXT, \
                 base_url_setting TEXT, \
                 is_active BOOLEAN NOT NULL DEFAULT true, \
                 sort_order INT NOT NULL DEFAULT 0 )",
        )
        .execute(pool)
        .await
        .expect("registry table");
        crate::test_support::create_settings_table(pool).await;
    }

    /// PEZZO 1 (regola G+L, test via il produttore reale — regola O): la lista
    /// dei provider da sincronizzare esce da `providers_da_sincronizzare` che
    /// interroga il registry vero, NON da un CSV. openrouter (attivo+configurato,
    /// identico ai 5 storici) DEVE entrare: era proprio il caso che il CSV
    /// hardcoded lasciava fuori.
    #[sqlx::test]
    async fn pezzo1_providers_dedotti_dal_registry(pool: sqlx::PgPool) {
        crea_registry_e_settings(&pool).await;
        sqlx::query(
            "INSERT INTO nexus_provider_registry \
                 (name, activation, key_setting, enabled_setting, base_url_setting, is_active, sort_order) VALUES \
              ('openai',     'api_key',           'openai_key',     'openai_en',     NULL,            true, 1), \
              ('google',     'api_key_or_vertex', 'google_key',     'google_en',     NULL,            true, 2), \
              ('openrouter', 'api_key',           'openrouter_key', 'openrouter_en', NULL,            true, 3), \
              ('spento',     'api_key',           'spento_key',     'spento_en',     NULL,            true, 4), \
              ('senza_key',  'api_key',           'senza_key_key',  'senza_key_en',  NULL,            true, 5), \
              ('vllm_ok',    'base_url',          NULL,             NULL,            'vllm_ok_url',    true, 6), \
              ('vllm_vuoto', 'base_url',          NULL,             NULL,            'vllm_vuoto_url', true, 7), \
              ('inattivo',   'api_key',           'inattivo_key',   'inattivo_en',   NULL,            false,8)",
        )
        .execute(&pool)
        .await
        .expect("seed registry");
        crate::test_support::seed_settings(
            &pool,
            &[
                ("openai_en", "true"),
                ("openai_key", "sk-1"),
                ("google_en", "true"),
                ("google_key", "g-1"),
                ("openrouter_en", "true"),
                ("openrouter_key", "or-1"),
                ("spento_en", "false"),
                ("spento_key", "sk-2"),
                ("senza_key_en", "true"),
                ("senza_key_key", ""),
                ("vllm_ok_url", "http://127.0.0.1:8000/v1"),
                ("vllm_vuoto_url", ""),
                ("inattivo_en", "true"),
                ("inattivo_key", "sk-3"),
            ],
        )
        .await;

        let got = providers_da_sincronizzare(&pool).await.expect("query");

        assert_eq!(
            got,
            vec![
                "openai".to_string(),
                "google".to_string(),
                "openrouter".to_string(),
                "vllm_ok".to_string(),
            ],
            "entrano SOLO gli attivi+configurati, ordinati per sort_order: \
             'spento' (enabled=false) e 'senza_key' (key vuota) fuori; \
             'vllm_vuoto' (base_url vuoto) fuori; 'inattivo' (is_active=false) fuori; \
             openrouter — identico ai 5 storici — NON resta piu' fuori"
        );
    }

    // ── PEZZO 2: la policy governa anche l'INSERT, non solo l'enable ──

    /// PEZZO 2 (regola L; test via il produttore reale — regola O): il ramo di
    /// INSERT di `process_discovered_chat_model` (il nodo di produzione toccato)
    /// consulta `model_passes_selection_policy` PRIMA di inserire. Un provider
    /// CON policy restrittiva inserisce solo gli allowed; uno SENZA riga policy
    /// inserisce tutti (unwrap_or(true), invariato per i 5 provider storici).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn pezzo2_policy_governa_insert(pool: sqlx::PgPool) {
        // Schema REALE (regola O): `ai_price_catalog` (col vincolo UNIQUE
        // `uq_price_catalog_provider_model` gia' dalla mig 0032, il
        // `capability_source` dalla mig successiva) e `nexus_model_selection_policy`
        // (mig 0320) arrivano dalla migrazione. I DELETE isolano dai dati di
        // produzione: l'assert finale conta le righe ESATTE del catalog.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        sqlx::query("DELETE FROM nexus_model_selection_policy")
            .execute(&pool)
            .await
            .expect("pulizia policy");
        sqlx::query(
            "INSERT INTO nexus_model_selection_policy (provider, allowed_patterns, denied_patterns) \
             VALUES ('provA', ARRAY['^good-']::text[], ARRAY[]::text[])",
        )
        .execute(&pool)
        .await
        .expect("seed policy");

        let mut delta = ModelSyncDelta::default();
        // ramo None (modello nuovo), orchestrator=None -> niente probe.
        process_discovered_chat_model(&pool, None, "provA", "good-1", true, None, None, &mut delta)
            .await;
        process_discovered_chat_model(&pool, None, "provA", "bad-1", true, None, None, &mut delta)
            .await;
        process_discovered_chat_model(
            &pool, None, "provB", "qualsiasi-1", true, None, None, &mut delta,
        )
        .await;

        let models: Vec<String> =
            sqlx::query_scalar("SELECT provider || '/' || model FROM ai_price_catalog ORDER BY 1")
                .fetch_all(&pool)
                .await
                .expect("catalog");
        assert_eq!(
            models,
            vec!["provA/good-1".to_string(), "provB/qualsiasi-1".to_string()],
            "provA con policy inserisce SOLO 'good-1' (bad-1 filtrato PRIMA \
             dell'insert); provB senza policy inserisce comunque (unwrap_or(true))"
        );
        assert_eq!(delta.inserted, 2, "due soli insert: good-1 e qualsiasi-1");
    }

    /// I FLAG POST-PROBE DEVONO ARRIVARE NEL CATALOG.
    ///
    /// Lo statement di `write_probe_healthy_flags` usava `$8` sia dove serviva un
    /// booleano (`is_enabled = ($8 AND NOT ...)`) sia dove serviva un testo
    /// (`agentic_thinking_policy = ... THEN $8`), e `$7` non compariva mai.
    /// Postgres deduce il tipo di un parametro dal primo uso, quindi rifiutava lo
    /// statement al PARSE: «in CASE i tipi text e boolean non combaciano». Il
    /// `let _ =` sull'`execute` inghiottiva l'errore, cosi' la funzione non ha
    /// mai scritto una riga — in silenzio, mentre `apply_probe_healthy` loggava
    /// "probe OK -> abilitato" e scriveva l'audit.
    ///
    /// Il test chiama la funzione VERA (regola O): ricopiare la query qui
    /// avrebbe misurato la copia, e una copia corretta sarebbe restata verde
    /// sopra una produzione rotta. Ripristinando `$8` al posto di `$7` nello
    /// statement, questo test rosseggia.
    #[sqlx::test]
    async fn i_flag_post_probe_arrivano_nel_catalog(pool: sqlx::PgPool) {
        crate::test_support::create_ai_price_catalog_table(&pool).await;
        sqlx::query(
            "ALTER TABLE ai_price_catalog \
               ADD COLUMN capability_source TEXT NOT NULL DEFAULT 'auto', \
               ADD COLUMN last_probe_healthy_at TIMESTAMPTZ, \
               ADD COLUMN auto_disabled_at TIMESTAMPTZ",
        )
        .execute(&pool)
        .await
        .expect("colonne");
        // Due righe disabilitate (la WHERE della funzione tocca solo `is_enabled
        // = false`): una 'auto' che i flag li accetta, una 'manual' che li
        // rifiuta perche' e' curata a mano.
        sqlx::query(
            "INSERT INTO ai_price_catalog \
               (provider, model, is_enabled, capability_source, agentic_thinking_policy, \
                supports_tool_use, pricing_state) \
             VALUES ('p', 'auto-1',   false, 'auto',   'none', false, 'priced'), \
                    ('p', 'curato-1', false, 'manual', 'none', false, 'priced')",
        )
        .execute(&pool)
        .await
        .expect("seed");

        let caps = json!(["chat"]);
        let cc = ClassifiedCaps {
            supports_tool_use: true,
            supports_vision: false,
            uses_thinking_mode: true,
            agentic_thinking_policy: "disable_for_tools",
        };
        write_probe_healthy_flags(&pool, "p", "auto-1", &caps, &cc, true).await;
        write_probe_healthy_flags(&pool, "p", "curato-1", &caps, &cc, true).await;

        let (enabled, policy, tool_use): (bool, String, bool) = sqlx::query_as(
            "SELECT is_enabled, agentic_thinking_policy, supports_tool_use \
               FROM ai_price_catalog WHERE model = 'auto-1'",
        )
        .fetch_one(&pool)
        .await
        .expect("riga auto-1");
        assert!(
            enabled,
            "allowed=true e prezzo noto: la riga va abilitata. Con lo statement \
             rotto restava false perche' l'UPDATE non partiva affatto"
        );
        assert_eq!(
            (policy.as_str(), tool_use),
            ("disable_for_tools", true),
            "su una riga 'auto' i flag classificati devono essere scritti"
        );

        let (policy_curata, tool_use_curato): (String, bool) = sqlx::query_as(
            "SELECT agentic_thinking_policy, supports_tool_use \
               FROM ai_price_catalog WHERE model = 'curato-1'",
        )
        .fetch_one(&pool)
        .await
        .expect("riga curato-1");
        assert_eq!(
            (policy_curata.as_str(), tool_use_curato),
            ("none", false),
            "una riga 'manual' e' curata a mano: i flag classificati non la toccano"
        );
    }

    /// REGRESSIONE (violazione 20): un DB che non risponde diventava un modello
    /// DISABILITATO. `model_recently_healthy` collassava l'errore sqlx su
    /// `false` con `.ok().flatten()` — cioe' sul verdetto "non e' sano" — e la
    /// conseguenza di quel verdetto e' una SCRITTURA (`is_enabled=false`,
    /// `auto_disabled_reason='missing_from_api'`).
    ///
    /// Il test attraversa il PRODUTTORE vero (regola O): l'errore non e'
    /// fabbricato, nasce dal pool CHIUSO, cioe' dallo stesso `sqlx::Error` che
    /// il sync incontrerebbe con Postgres irraggiungibile.
    ///
    /// PROVA DI MUTAZIONE ESEGUITA: fatto ritornare `NonSano` sul ramo `Err` di
    /// `salute_modello_recente` (cioe' il vecchio `.ok().flatten()`), il test
    /// rosseggia col valore del difetto reale — `left: NonSano`,
    /// `right: NonInterrogabile` — e con esso cade il verdetto di preservazione,
    /// cioe' il caller torna a scrivere.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn db_muto_non_e_un_modello_da_spegnere(pool: sqlx::PgPool) {
        // Il DB c'e' e lo schema e' quello reale; poi sparisce sotto i piedi del
        // sync, che e' esattamente il caso da coprire.
        pool.close().await;

        assert_eq!(
            salute_modello_recente(&pool, "p", "m").await,
            SaluteModello::NonInterrogabile,
            "senza risposta dal DB non esiste alcun fatto sulla salute: \
             l'ignoto e' una variante, non 'non e' sano'"
        );

        let api_models: Vec<String> = vec!["altro".to_string()];
        let api_set: std::collections::HashSet<&str> = api_models
            .iter()
            .map(|s| s.as_str())
            .collect::<std::collections::HashSet<_>>();
        assert!(
            missing_model_should_be_preserved(&pool, "p", "m", &api_models, &api_set).await,
            "modello assente da upstream + salute non interrogabile: si PRESERVA \
             (nessuna disabilitazione), il prossimo sync ripone la domanda"
        );
    }

    /// Il rovescio del test precedente: quando il fatto C'E', il verdetto resta
    /// quello di prima. Senza questo, "non scrivo mai" passerebbe entrambi.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn fallimenti_consecutivi_registrati_restano_non_sano(pool: sqlx::PgPool) {
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        sqlx::query(
            "INSERT INTO ai_price_catalog \
               (provider, model, input_cost_per_million_tokens, output_cost_per_million_tokens, \
                consecutive_failures, currency) \
             VALUES ('p', 'rotto', 1.0, 1.0, 3, 'USD'), \
                    ('p', 'illeso', 1.0, 1.0, 0, 'USD')",
        )
        .execute(&pool)
        .await
        .expect("seed");

        assert_eq!(
            salute_modello_recente(&pool, "p", "rotto").await,
            SaluteModello::NonSano,
            "3 fallimenti consecutivi sono un FATTO: il modello si disabilita"
        );
        assert_eq!(
            salute_modello_recente(&pool, "p", "illeso").await,
            SaluteModello::Sano,
            "nessun fallimento consecutivo: il modello resta acceso (FIX 2)"
        );
        assert_eq!(
            salute_modello_recente(&pool, "p", "inesistente").await,
            SaluteModello::NonSano,
            "riga ASSENTE e' un fatto, non un ignoto: non c'e' nulla da preservare"
        );
    }
}
