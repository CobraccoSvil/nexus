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
//!
//! ## Redazione PII asimmetrica per ruolo (fix radice loop email, regola H/M)
//!
//! Le PII rilevate da Presidio in strict mode vengono sostituite con placeholder
//! reversibili (`__NEXUS_<KIND>_<N>__` in [`RedactionMap`]). La reidratazione
//! post-flight tocca solo `response.content`, MAI i tool_input generati dal
//! modello: percio' un'email che l'utente scrive nel PROPRIO messaggio come
//! soggetto del task (es. "perche' costantino@cobracco.it non fa login") arriva
//! al modello come placeholder opaco, che il modello usa letteralmente come
//! parametro SQL -> zero match -> ri-chiede il dato -> loop infinito (incidente
//! Beaty-Book 2026-07-03).
//!
//! La PII fornita VOLONTARIAMENTE dall'utente nel proprio messaggio (`role=user`)
//! NON e' un leak di dati di TERZI: e' input necessario al task. Il flag DB
//! `gateway.redaction.skip_pii_in_user_messages` (opt-in, **default false** =
//! comportamento storico) attiva la redazione ASIMMETRICA: le entita' PII nei
//! messaggi `role=user` NON vengono redatte, mentre le PII che emergono da
//! qualunque altro ruolo (assistant/tool: potenziali dati di terzi da
//! tool_result) restano redatte. I SEGRETI (secret scanner) restano SEMPRE
//! redatti su OGNI ruolo, incluso `user`: un'API key incollata per errore non e'
//! mai un dato operativo lecito. La distinzione PII-utente (rilassabile) vs
//! segreto (mai) coincide con la separazione gia' presente tra ramo Presidio e
//! ramo secret scanner.
//!
//! ## Segnale strutturato di redazione (regola M)
//!
//! Quando una redazione e' applicata la pipeline emette un SEGNALE STRUTTURATO
//! ([`RedactionResult::redactions`] + flag aggregati in [`RedactionStats`]), non
//! solo il placeholder testuale. Il consumatore a valle (mcp-core:
//! `routing/signals.rs::tool_result_outcome_after` / costruzione dello
//! `StallContext`) legge il flag strutturato invece di fare `contains("[REDACTED:")`
//! sul testo: cosi' `redaction_rejected` diventa un segnale codificato alla fonte
//! e non un match di prosa. Regola F: il segnale porta SOLO `kind`+`class`, mai
//! il valore originale.

use std::sync::LazyLock;

use regex::Regex;

use super::code_anonymizer::CodeAnonymizer;
use super::path_policy::{PathDecision, PathPolicy};
use super::presidio_client::PresidioClient;
use super::redaction_map::RedactionMap;
use super::secret_scanner::{PatternType, SecretScanner};
use crate::types::{LlmMessage, LlmRequest, LlmResponse, MessageContent};

/// Ruolo di un messaggio il cui contenuto e' input VOLONTARIO dell'utente. La
/// policy PII asimmetrica rilassa la redazione delle PII solo per questo ruolo
/// (l'email/CF che l'utente scrive nel proprio messaggio e' il soggetto del
/// task, non un leak di dati di terzi). Gli altri ruoli (assistant/tool)
/// possono trasportare PII emerse da tool_result: restano redatti.
const USER_ROLE: &str = "user";

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
    /// Redazione PII asimmetrica: quando `true`, le entita' PII di Presidio nei
    /// messaggi `role=user` (dato volontario dell'utente, soggetto del task) NON
    /// vengono redatte; le PII di ogni altro ruolo restano redatte e i SEGRETI
    /// restano sempre redatti su tutti i ruoli. Opt-in dal flag DB
    /// `gateway.redaction.skip_pii_in_user_messages` (default `false` = storico).
    pub skip_pii_in_user_messages: bool,
}

/// Classe di ordine superiore di un dato redatto: distingue la PII (dato
/// personale, potenzialmente rilassabile per l'input volontario dell'utente) dal
/// segreto (chiave/token/credenziale, MAI rilassabile). Il consumatore a valle
/// puo' ragionare sulla classe senza conoscere i singoli `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedactionClass {
    /// Entita' PII (Presidio) o identificatore anonimizzato (code anonymizer).
    Pii,
    /// Segreto strutturato (secret scanner): API key, token, connection string.
    Secret,
}

impl RedactionClass {
    /// Etichetta stabile (machine-readable) per il trasporto strutturato a valle.
    pub fn as_str(self) -> &'static str {
        match self {
            RedactionClass::Pii => "pii",
            RedactionClass::Secret => "secret",
        }
    }
}

/// Segnale strutturato di una redazione applicata (regola M): `kind` = tipo
/// specifico (es. `email_address`, `aws_key`), `class` = classe di ordine
/// superiore. Regola F: NON contiene mai il valore originale. Emesso dalla
/// pipeline cosi' che il consumatore a valle riconosca la redazione da un campo
/// codificato invece che da `contains("[REDACTED:")` sul testo.
#[derive(Debug, Clone)]
pub struct AppliedRedaction {
    pub kind: String,
    pub class: RedactionClass,
}

/// Statistiche aggregate della redazione (solo conteggi/tipi, regola F).
#[derive(Debug, Clone, Default)]
pub struct RedactionStats {
    pub secrets_found: usize,
    pub pii_found: usize,
    pub code_anonymized: usize,
    pub types: Vec<String>,
    /// Flag strutturato: almeno un SEGRETO e' stato redatto (regola M). Distinto
    /// dal solo conteggio per esporre l'esito booleano al consumatore a valle.
    pub secret_redacted: bool,
    /// Flag strutturato: almeno una PII e' stata redatta (dopo la policy
    /// asimmetrica: se `skip_pii_in_user_messages` sopprime la redazione PII
    /// utente, quella PII NON conta come redatta). Regola M.
    pub pii_redacted: bool,
}

/// Esito della redazione pre-flight: messaggi redatti, mappa di reidratazione,
/// statistiche.
#[derive(Debug)]
pub struct RedactionResult {
    pub messages: Vec<LlmMessage>,
    pub map: RedactionMap,
    pub stats: RedactionStats,
    /// Segnale strutturato per-redazione (regola M): la lista delle redazioni
    /// EFFETTIVAMENTE applicate, con `kind`+`class`, mai il valore. Il
    /// consumatore a valle (mcp-core `tool_result_outcome_after` / costruzione
    /// dello `StallContext`) legge questo campo per popolare `redaction_rejected`
    /// come segnale codificato, invece di scansionare il testo alla ricerca di
    /// `[REDACTED:` o `__NEXUS_`.
    pub redactions: Vec<AppliedRedaction>,
}

impl RedactionResult {
    /// `true` se almeno una redazione (PII o segreto) e' stata applicata. Helper
    /// di lettura per il consumatore a valle (regola M): esito booleano da un
    /// segnale strutturato, mai da un match testuale.
    pub fn any_redacted(&self) -> bool {
        !self.redactions.is_empty()
    }

    /// `true` se almeno una PII e' stata redatta (dopo la policy asimmetrica).
    pub fn pii_redacted(&self) -> bool {
        self.stats.pii_redacted
    }

    /// `true` se almeno un segreto e' stato redatto (mai rilassato).
    pub fn secret_redacted(&self) -> bool {
        self.stats.secret_redacted
    }
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
    skip_pii_in_user_messages: bool,
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
            skip_pii_in_user_messages: opts.skip_pii_in_user_messages,
        }
    }

    /// Pre-flight: redige tutti i messaggi della richiesta.
    pub async fn redact(&self, req: &LlmRequest) -> Result<RedactionResult, RedactionError> {
        let mut map = match self.ttl {
            Some(ttl) => RedactionMap::with_ttl(req.metadata.request_id.clone(), ttl),
            None => RedactionMap::new(req.metadata.request_id.clone()),
        };
        let mut stats = RedactionStats::default();
        let mut redactions: Vec<AppliedRedaction> = Vec::new();
        let mut redacted_messages: Vec<LlmMessage> = Vec::with_capacity(req.messages.len());

        for msg in &req.messages {
            let content_str = message_text(&msg.content);
            // Policy PII asimmetrica: rilassa la sola PII sul dato volontario
            // dell'utente. I SEGRETI restano redatti su ogni ruolo (anche user).
            let relax_pii = self.skip_pii_in_user_messages && msg.role == USER_ROLE;

            // system/tool: solo secret scanner (mai input utente diretto).
            if msg.role == "system" || msg.role == "tool" {
                let red = self.scan_and_redact_secret_layer(
                    &content_str,
                    false,
                    &mut stats,
                    &mut redactions,
                );
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

            // Secret scanner (con eventuale rilassamento PII) -> code anonymizer.
            let after_secrets = self.scan_and_redact_secret_layer(
                &content_str,
                relax_pii,
                &mut stats,
                &mut redactions,
            );

            let anon = self.anonymizer.anonymize(&after_secrets, &mut map);
            stats.code_anonymized += anon.count;
            for t in &anon.types {
                push_unique(&mut stats.types, t);
                // Il code anonymizer opera su identificatori/literal ad alta
                // entropia (classe PII/segreto-strutturale): traccia il segnale.
                stats.pii_redacted = true;
                redactions.push(AppliedRedaction {
                    kind: t.clone(),
                    class: RedactionClass::Pii,
                });
            }
            let mut text = anon.text;

            // Presidio PII: in strict mode sostituisce le entita' con placeholder
            // reversibile. La policy asimmetrica salta la sostituzione (ma NON la
            // rilevazione: pii_found resta conteggiato per il tier) quando la PII
            // e' input volontario dell'utente.
            let presidio = self.presidio.analyze(&text).await;
            if presidio.has_pii {
                stats.pii_found += presidio.entities.len();
                if self.strict_mode && !relax_pii {
                    // Sostituisce dalla fine all'inizio per non invalidare gli offset.
                    let mut entities = presidio.entities.clone();
                    entities.sort_by_key(|e| std::cmp::Reverse(e.start));
                    for e in &entities {
                        if e.end <= text.len() && e.start <= e.end {
                            let original = text[e.start..e.end].to_string();
                            let kind = e.entity_type.to_ascii_lowercase();
                            let placeholder = map.store(&original, &kind);
                            push_unique(&mut stats.types, &kind);
                            stats.pii_redacted = true;
                            redactions.push(AppliedRedaction {
                                kind,
                                class: RedactionClass::Pii,
                            });
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
            redactions,
        })
    }

    /// Applica il layer secret-scanner al testo e registra il segnale strutturato
    /// (regola M) per ogni tipo effettivamente redatto. Con `relax_pii=true`
    /// redige i SOLI segreti (delega al punto unico `redact_secrets_only`),
    /// lasciando in chiaro le PII fornite dall'utente; con `relax_pii=false`
    /// redige segreti + PII come il profilo storico (`redact`). Il `kind` di
    /// ciascuna redazione e' ricavato da uno `scan()` sul testo ORIGINALE, cosi'
    /// il segnale distingue PII e segreto senza duplicare la classificazione
    /// (delega a [`PatternType::is_pii`]).
    fn scan_and_redact_secret_layer(
        &self,
        content: &str,
        relax_pii: bool,
        stats: &mut RedactionStats,
        redactions: &mut Vec<AppliedRedaction>,
    ) -> String {
        // Un solo tipo per kind (il redact conta i tipi, non le occorrenze).
        let mut seen_kinds: Vec<PatternType> = Vec::new();
        for p in self.scanner.scan(content).patterns {
            // Con rilassamento, la PII utente NON e' redatta: non entra nel segnale.
            if relax_pii && p.kind.is_pii() {
                continue;
            }
            if !seen_kinds.contains(&p.kind) {
                seen_kinds.push(p.kind);
            }
        }

        for kind in &seen_kinds {
            let (class, is_secret) = if kind.is_pii() {
                (RedactionClass::Pii, false)
            } else {
                (RedactionClass::Secret, true)
            };
            if is_secret {
                stats.secret_redacted = true;
            } else {
                stats.pii_redacted = true;
            }
            redactions.push(AppliedRedaction {
                kind: kind.as_str().to_string(),
                class,
            });
        }

        if relax_pii {
            let (red, count) = self.scanner.redact_secrets_only(content);
            stats.secrets_found += count;
            red
        } else {
            let (red, count) = self.scanner.redact(content);
            // `redact` conta TUTTI i tipi (segreti + PII): il campo storico
            // `secrets_found` resta invariato per compatibilita' con i log.
            stats.secrets_found += count;
            red
        }
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
            run_timeout_secs: None,
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

    fn pipeline_relax_pii(strict: bool) -> RedactionPipeline {
        RedactionPipeline::new(
            PresidioClient::new(),
            RedactionOptions {
                strict_mode: strict,
                skip_pii_in_user_messages: true,
                ..Default::default()
            },
        )
    }

    fn msg_role(role: &str, text: &str) -> LlmMessage {
        LlmMessage {
            role: role.into(),
            content: MessageContent::Text(text.into()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: None,
        }
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
                reasoning_tokens: None,
            },
            model_used: "gw-test".into(),
            provider_used: "test".into(),
            latency_ms: 1,
            finish_reason: "stop".into(),
            privacy_rerouted: None,
            reasoning: None,
            thinking_signature: None,
            citations: None,
            ledger: None,
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

    // --- Policy PII asimmetrica (fix loop email Beaty-Book) ---

    #[tokio::test]
    async fn default_redige_la_pii_utente_comportamento_storico() {
        // Senza il flag, l'email dell'utente viene oscurata come sempre (secret
        // scanner pattern email_pii): comportamento storico preservato.
        let p = pipeline(false);
        let req = request(
            vec![user_msg("perche' costantino@cobracco.it non fa login?")],
            "req-default",
        );
        let res = p.redact(&req).await.expect("redazione ok");
        let out = match &res.messages[0].content {
            MessageContent::Text(t) => t.clone(),
            _ => panic!("atteso testo"),
        };
        assert!(out.contains("[REDACTED:email_pii]"));
        assert!(!out.contains("costantino@cobracco.it"));
        assert!(res.pii_redacted(), "segnale strutturato PII atteso");
    }

    #[tokio::test]
    async fn policy_asimmetrica_non_redige_pii_utente_ma_redige_segreto() {
        // Con il flag ON: l'email che l'utente scrive come soggetto del task NON
        // viene oscurata (chiude il loop), MA un segreto incollato per errore
        // resta redatto (protezione segreti mai rilassata).
        let p = pipeline_relax_pii(false);
        let req = request(
            vec![user_msg(
                "perche' costantino@cobracco.it non fa login? key=AKIAIOSFODNN7EXAMPLE",
            )],
            "req-relax",
        );
        let res = p.redact(&req).await.expect("redazione ok");
        let out = match &res.messages[0].content {
            MessageContent::Text(t) => t.clone(),
            _ => panic!("atteso testo"),
        };
        // PII utente in chiaro.
        assert!(out.contains("costantino@cobracco.it"));
        assert!(!out.contains("[REDACTED:email_pii]"));
        // Segreto SEMPRE redatto.
        assert!(out.contains("[REDACTED:aws_key]"));
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
        // Segnale strutturato: segreto redatto, PII utente NO.
        assert!(res.secret_redacted(), "segreto redatto -> segnale atteso");
        assert!(
            !res.pii_redacted(),
            "PII utente rilassata -> nessun segnale PII"
        );
    }

    #[tokio::test]
    async fn policy_asimmetrica_redige_pii_di_ruolo_non_user() {
        // Il rilassamento vale SOLO per role=user: una PII in un messaggio
        // assistant (potenziale dato di terzi da tool_result) resta redatta.
        let p = pipeline_relax_pii(false);
        let req = request(
            vec![
                msg_role("user", "controlla il record"),
                msg_role("assistant", "trovato cliente terzo@example.com nel db"),
            ],
            "req-asst",
        );
        let res = p.redact(&req).await.expect("redazione ok");
        let asst = match &res.messages[1].content {
            MessageContent::Text(t) => t.clone(),
            _ => panic!("atteso testo"),
        };
        assert!(asst.contains("[REDACTED:email_pii]"));
        assert!(!asst.contains("terzo@example.com"));
        assert!(res.pii_redacted(), "PII di ruolo non-user resta redatta");
    }

    #[tokio::test]
    async fn segnale_strutturato_espone_kind_e_classe() {
        // (b) segnale strutturato alla fonte: il consumatore a valle legge
        // kind+class, non fa contains("[REDACTED:") sul testo.
        let p = pipeline(false);
        let req = request(
            vec![user_msg(
                "key=AKIAIOSFODNN7EXAMPLE e mail mario@example.com",
            )],
            "req-signal",
        );
        let res = p.redact(&req).await.expect("redazione ok");
        assert!(res.any_redacted());
        assert!(res.secret_redacted());
        assert!(res.pii_redacted());
        // Il segnale distingue segreto e PII per classe.
        let has_secret = res
            .redactions
            .iter()
            .any(|r| r.class == RedactionClass::Secret && r.kind == "aws_key");
        let has_pii = res
            .redactions
            .iter()
            .any(|r| r.class == RedactionClass::Pii && r.kind == "email_pii");
        assert!(has_secret, "segnale segreto atteso");
        assert!(has_pii, "segnale PII atteso");
        // Regola F: il segnale non contiene mai il valore originale.
        for r in &res.redactions {
            assert!(!r.kind.contains("AKIA"));
            assert!(!r.kind.contains("mario@example.com"));
        }
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
