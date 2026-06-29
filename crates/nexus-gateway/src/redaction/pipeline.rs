//! Orchestratore della pipeline di redazione (pre-flight) e reidratazione
//! (post-flight).
//!
//! Porting di `packages/llm-gateway/src/redaction/redaction-pipeline.ts`.
//!
//! Pre-flight (`redact`): per ogni messaggio
//!   - system/tool: solo secret scanner (redazione diretta);
//!   - altri: path policy (blacklist -> errore, whitelist -> bypass) +
//!     secret scanner + code anonymizer + Presidio (in strict mode sostituisce
//!     le entita' PII con placeholder reidratabili).
//!
//! Post-flight (`rehydrate`): sostituisce i placeholder nella risposta del
//! provider con i valori originali, usando la `RedactionMap` della request.
//!
//! Regola F: le `stats` contengono solo CONTEGGI e TIPI, mai valori. La
//! `RedactionMap` (che contiene gli originali) non e' mai loggata.

use std::sync::LazyLock;

use regex::Regex;

use super::code_anonymizer::CodeAnonymizer;
use super::path_policy::{PathDecision, PathPolicy};
use super::presidio_client::PresidioClient;
use super::redaction_map::RedactionMap;
use super::secret_scanner::SecretScanner;
use crate::types::{LlmMessage, LlmRequest, LlmResponse, MessageContent};

/// Errore di redazione: un file in blacklist non puo' essere inviato a provider
/// esterni. Parita' con `RedactionError` di `@nexus/shared` (qui definito
/// localmente: non esiste un equivalente Rust condiviso).
#[derive(Debug, thiserror::Error)]
pub enum RedactionError {
    #[error("file '{file_path}' in blacklist: non inviabile a provider esterni")]
    Blocked { file_path: String },
}

/// Opzioni della pipeline.
#[derive(Debug, Clone, Default)]
pub struct RedactionOptions {
    /// In strict mode le entita' PII di Presidio sono sostituite con placeholder.
    pub strict_mode: bool,
    /// TTL della redaction map (None -> default 5 min).
    pub ttl: Option<std::time::Duration>,
    /// Override whitelist/blacklist della path policy (None -> default).
    pub whitelist: Option<Vec<String>>,
    pub blacklist: Option<Vec<String>>,
}

/// Statistiche aggregate della redazione (solo conteggi/tipi, regola F).
#[derive(Debug, Clone, Default)]
pub struct RedactionStats {
    pub secrets_found: usize,
    pub pii_found: usize,
    pub code_anonymized: usize,
    pub types: Vec<String>,
}

/// Esito della redazione pre-flight: messaggi redatti, mappa di reidratazione,
/// statistiche.
#[derive(Debug)]
pub struct RedactionResult {
    pub messages: Vec<LlmMessage>,
    pub map: RedactionMap,
    pub stats: RedactionStats,
}

/// Estrae un path di file riferito nel contenuto (parita' con `extractFilePath`).
static FILE_PATH_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:File|file):\s*([\w./\-]+\.\w+)").expect("regex file-label valida"));
static FILE_PATH_COMMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^//\s*([\w./\-]+\.\w+)").expect("regex file-comment valida"));

fn extract_file_path(content: &str) -> Option<String> {
    if let Some(c) = FILE_PATH_LABEL.captures(content) {
        return c.get(1).map(|m| m.as_str().to_string());
    }
    FILE_PATH_COMMENT
        .captures(content)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

/// Orchestratore. Compone scanner, presidio, anonymizer, path policy.
#[derive(Debug, Clone)]
pub struct RedactionPipeline {
    scanner: SecretScanner,
    presidio: PresidioClient,
    anonymizer: CodeAnonymizer,
    path_policy: PathPolicy,
    strict_mode: bool,
    ttl: Option<std::time::Duration>,
}

impl RedactionPipeline {
    /// Crea la pipeline. Il `PresidioClient` e' fornito (config/cache condivisa).
    pub fn new(presidio: PresidioClient, opts: RedactionOptions) -> Self {
        let path_policy = PathPolicy::new(opts.whitelist.as_deref(), opts.blacklist.as_deref());
        Self {
            scanner: SecretScanner,
            presidio,
            anonymizer: CodeAnonymizer,
            path_policy,
            strict_mode: opts.strict_mode,
            ttl: opts.ttl,
        }
    }

    /// Pre-flight: redige tutti i messaggi della richiesta.
    pub async fn redact(&self, req: &LlmRequest) -> Result<RedactionResult, RedactionError> {
        let mut map = match self.ttl {
            Some(ttl) => RedactionMap::with_ttl(req.metadata.request_id.clone(), ttl),
            None => RedactionMap::new(req.metadata.request_id.clone()),
        };
        let mut stats = RedactionStats::default();
        let mut redacted_messages: Vec<LlmMessage> = Vec::with_capacity(req.messages.len());

        for msg in &req.messages {
            let content_str = message_text(&msg.content);

            // system/tool: solo secret scanner.
            if msg.role == "system" || msg.role == "tool" {
                let (red, count) = self.scanner.redact(&content_str);
                stats.secrets_found += count;
                redacted_messages.push(with_content(msg, red));
                continue;
            }

            // Path policy: blacklist blocca, whitelist bypassa.
            if let Some(file_path) = extract_file_path(&content_str) {
                match self.path_policy.check_path(&file_path) {
                    PathDecision::Blocked => {
                        return Err(RedactionError::Blocked { file_path });
                    }
                    PathDecision::Whitelisted => {
                        // Passa senza redaction.
                        redacted_messages.push(with_content(msg, content_str));
                        continue;
                    }
                    PathDecision::Redact => {}
                }
            }

            // Secret scanner -> code anonymizer (con redaction map).
            let (after_secrets, secret_count) = self.scanner.redact(&content_str);
            stats.secrets_found += secret_count;

            let anon = self.anonymizer.anonymize(&after_secrets, &mut map);
            stats.code_anonymized += anon.count;
            for t in &anon.types {
                push_unique(&mut stats.types, t);
            }
            let mut text = anon.text;

            // Presidio PII: in strict mode sostituisce le entita' con placeholder.
            let presidio = self.presidio.analyze(&text).await;
            if presidio.has_pii {
                stats.pii_found += presidio.entities.len();
                if self.strict_mode {
                    // Sostituisce dalla fine all'inizio per non invalidare gli offset.
                    let mut entities = presidio.entities.clone();
                    entities.sort_by_key(|e| std::cmp::Reverse(e.start));
                    for e in &entities {
                        if e.end <= text.len() && e.start <= e.end {
                            let original = text[e.start..e.end].to_string();
                            let kind = e.entity_type.to_ascii_lowercase();
                            let placeholder = map.store(&original, &kind);
                            push_unique(&mut stats.types, &kind);
                            text.replace_range(e.start..e.end, &placeholder);
                        }
                    }
                }
            }

            redacted_messages.push(with_content(msg, text));
        }

        Ok(RedactionResult {
            messages: redacted_messages,
            map,
            stats,
        })
    }

    /// Post-flight: reidrata la risposta del provider con i valori originali.
    pub fn rehydrate(&self, response: &LlmResponse, map: &mut RedactionMap) -> LlmResponse {
        let mut out = response.clone();
        out.content = map.rehydrate(&response.content);
        out
    }
}

/// Testo di un contenuto di messaggio (stringa o blocchi serializzati).
fn message_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::Blocks(blocks) => serde_json::to_string(blocks).unwrap_or_default(),
    }
}

/// Clona un messaggio sostituendo il contenuto testuale.
fn with_content(msg: &LlmMessage, text: String) -> LlmMessage {
    LlmMessage {
        role: msg.role.clone(),
        content: MessageContent::Text(text),
        tool_call_id: msg.tool_call_id.clone(),
        tool_calls: msg.tool_calls.clone(),
        name: msg.name.clone(),
        thinking_signature: msg.thinking_signature.clone(),
        // Il reasoning_content DeepSeek di un turno assistant precedente viaggia
        // intatto attraverso la redaction (non e' un payload testuale da
        // redarre: e' il pensiero da ri-passare all'API, vincolo HTTP 400).
        reasoning: msg.reasoning.clone(),
    }
}

fn push_unique(types: &mut Vec<String>, t: &str) {
    if !types.iter().any(|x| x == t) {
        types.push(t.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LlmUsage, RequestMetadata};

    fn meta(request_id: &str) -> RequestMetadata {
        RequestMetadata {
            tenant_id: "t".into(),
            user_id: "u".into(),
            request_id: request_id.into(),
            sensitivity_tier: 0,
            feature: "chat".into(),
        }
    }

    fn user_msg(text: &str) -> LlmMessage {
        LlmMessage {
            role: "user".into(),
            content: MessageContent::Text(text.into()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: None,
        }
    }

    fn request(messages: Vec<LlmMessage>, request_id: &str) -> LlmRequest {
        LlmRequest {
            model: "gw-test".into(),
            messages,
            temperature: None,
            max_tokens: None,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: meta(request_id),
        }
    }

    fn pipeline(strict: bool) -> RedactionPipeline {
        RedactionPipeline::new(
            PresidioClient::new(),
            RedactionOptions {
                strict_mode: strict,
                ..Default::default()
            },
        )
    }

    #[tokio::test]
    async fn redige_segreto_nel_messaggio() {
        let p = pipeline(false);
        let req = request(
            vec![user_msg("la mia key e' AKIAIOSFODNN7EXAMPLE, usala")],
            "req-secret",
        );
        let res = p.redact(&req).await.expect("redazione ok");
        let out = match &res.messages[0].content {
            MessageContent::Text(t) => t.clone(),
            _ => panic!("atteso testo"),
        };
        assert!(out.contains("[REDACTED:aws_key]"));
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(res.stats.secrets_found >= 1);
    }

    #[tokio::test]
    async fn round_trip_anonimizzazione_e_reidratazione() {
        let p = pipeline(false);
        // Identificatore @confidential -> anonimizzato; reidratazione lo ripristina.
        let code = "// @confidential\nconst dbSecret = load();\nuse(dbSecret);";
        let req = request(vec![user_msg(code)], "req-rt");
        let mut res = p.redact(&req).await.expect("redazione ok");

        let redacted_text = match &res.messages[0].content {
            MessageContent::Text(t) => t.clone(),
            _ => panic!("atteso testo"),
        };
        assert!(!redacted_text.contains("dbSecret"));
        assert!(res.stats.code_anonymized >= 1);

        // Il provider "risponde" citando il placeholder: la reidratazione lo
        // riporta al nome originale.
        let response = LlmResponse {
            content: redacted_text.clone(),
            tool_calls: None,
            usage: LlmUsage {
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: None,
                cache_creation_tokens: None,
            },
            model_used: "gw-test".into(),
            provider_used: "test".into(),
            latency_ms: 1,
            finish_reason: "stop".into(),
            privacy_rerouted: None,
            reasoning: None,
            thinking_signature: None,
        };
        let rehydrated = p.rehydrate(&response, &mut res.map);
        assert!(rehydrated.content.contains("dbSecret"));
    }

    #[tokio::test]
    async fn blacklist_blocca_invio() {
        let p = pipeline(false);
        let req = request(
            vec![user_msg("File: config/.env\nDB_PASS=...")],
            "req-bl",
        );
        let err = p.redact(&req).await.expect_err("deve fallire");
        match err {
            RedactionError::Blocked { file_path } => assert_eq!(file_path, "config/.env"),
        }
    }

    #[tokio::test]
    async fn whitelist_bypassa_redaction() {
        let p = pipeline(false);
        // README e' whitelist: passa senza alterazioni anche con un finto segreto.
        let content = "File: README.md\nesempio: AKIAIOSFODNN7EXAMPLE";
        let req = request(vec![user_msg(content)], "req-wl");
        let res = p.redact(&req).await.expect("redazione ok");
        let out = match &res.messages[0].content {
            MessageContent::Text(t) => t.clone(),
            _ => panic!("atteso testo"),
        };
        // Bypass: il contenuto resta identico (nessuna redazione).
        assert_eq!(out, content);
        assert_eq!(res.stats.secrets_found, 0);
    }

    #[tokio::test]
    async fn system_message_solo_secret_scanner() {
        let p = pipeline(false);
        let sys = LlmMessage {
            role: "system".into(),
            content: MessageContent::Text("istruzioni con ghp_abcdefghijklmnopqrstuvwxyz0123456789".into()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: None,
        };
        let req = request(vec![sys], "req-sys");
        let res = p.redact(&req).await.expect("redazione ok");
        let out = match &res.messages[0].content {
            MessageContent::Text(t) => t.clone(),
            _ => panic!("atteso testo"),
        };
        assert!(out.contains("[REDACTED:github_pat]"));
        assert!(res.stats.secrets_found >= 1);
    }

    #[test]
    fn extract_file_path_da_label_e_commento() {
        assert_eq!(
            extract_file_path("File: src/app.ts\n...").as_deref(),
            Some("src/app.ts")
        );
        assert_eq!(
            extract_file_path("// lib/util.rs\nfn main(){}").as_deref(),
            Some("lib/util.rs")
        );
        assert_eq!(extract_file_path("nessun path qui"), None);
    }
}
