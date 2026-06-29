//! Classificatore di sensibilita': combina secret scanner (sync) + Presidio
//! (async, PII) ed eleva il tier effettivo.
//!
//! Porting di `packages/llm-gateway/src/router/sensitivity-classifier.ts`.
//!
//! Il tier effettivo e' il MASSIMO tra i tier dei pattern segreti rilevati e il
//! tier delle entita' PII di Presidio. Il caller (policy engine) usa questo tier
//! per il gate cloud.
//!
//! Regola F: i `reasons` riportano SOLO tipo di pattern/entita' e tier, MAI il
//! valore rilevato. Niente log qui: la classificazione e' calcolo puro + una
//! chiamata HTTP delegata al `PresidioClient` (che gestisce il proprio logging).

use super::presidio_client::PresidioClient;
use super::secret_scanner::SecretScanner;
use crate::types::{LlmMessage, MessageContent, SensitivityTier};

/// Esito della classificazione (`ClassificationResult` del TS).
#[derive(Debug, Clone, Default)]
pub struct ClassificationResult {
    pub tier: SensitivityTier,
    /// Motivi leggibili (tipo + tier), senza valori sensibili.
    pub reasons: Vec<String>,
    /// Tipi di pattern segreto trovati (snake_case).
    pub secret_patterns: Vec<String>,
    /// Tipi di entita' PII Presidio trovati.
    pub presidio_entities: Vec<String>,
}

/// Classificatore. Compone scanner + client Presidio (composition over
/// inheritance, regola L).
#[derive(Debug, Clone)]
pub struct SensitivityClassifier {
    scanner: SecretScanner,
    presidio: PresidioClient,
}

impl SensitivityClassifier {
    /// Costruisce il classificatore con il client Presidio fornito (consente di
    /// condividere config/cache con il resto del gateway).
    pub fn new(presidio: PresidioClient) -> Self {
        Self {
            scanner: SecretScanner,
            presidio,
        }
    }

    /// Concatena il testo di tutti i messaggi (parita' con il TS: stringa
    /// concatenata con newline; i blocchi non-testo sono serializzati JSON).
    fn full_text(messages: &[LlmMessage]) -> String {
        messages
            .iter()
            .map(|m| match &m.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Blocks(blocks) => {
                    serde_json::to_string(blocks).unwrap_or_default()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Classifica i messaggi combinando secret scanner e Presidio (async).
    pub async fn classify(&self, messages: &[LlmMessage]) -> ClassificationResult {
        let text = Self::full_text(messages);

        let mut result = self.classify_secrets(&text);

        // Presidio PII (fallback graceful gestito dentro `analyze`).
        let presidio = self.presidio.analyze(&text).await;
        if presidio.has_pii {
            for e in &presidio.entities {
                if !result.presidio_entities.contains(&e.entity_type) {
                    result.presidio_entities.push(e.entity_type.clone());
                }
            }
            if presidio.max_tier > result.tier {
                result.tier = presidio.max_tier;
                result.reasons.push(format!(
                    "PII Presidio: {} (tier {})",
                    result.presidio_entities.join(", "),
                    presidio.max_tier
                ));
            }
        }

        result
    }

    /// Versione sincrona senza Presidio (path veloci a bassa latenza).
    /// Parita' con `classifySync` del TS.
    pub fn classify_sync(&self, messages: &[LlmMessage]) -> ClassificationResult {
        let text = Self::full_text(messages);
        self.classify_secrets(&text)
    }

    /// Parte sincrona: solo secret scanner. Calcola tier e reasons.
    fn classify_secrets(&self, text: &str) -> ClassificationResult {
        let mut result = ClassificationResult::default();
        let scan = self.scanner.scan(text);
        if scan.found {
            for p in &scan.patterns {
                let label = p.kind.as_str().to_string();
                result.secret_patterns.push(label.clone());
                if p.tier > result.tier {
                    result.tier = p.tier;
                }
                result.reasons.push(format!("pattern {label} (tier {})", p.tier));
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RequestMetadata;

    fn msg(role: &str, text: &str) -> LlmMessage {
        LlmMessage {
            role: role.to_string(),
            content: MessageContent::Text(text.to_string()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: None,
        }
    }

    fn classifier() -> SensitivityClassifier {
        // Presidio non configurato -> classify usa solo il secret scanner.
        SensitivityClassifier::new(PresidioClient::new())
    }

    #[test]
    fn classify_sync_eleva_tier_su_segreto() {
        let c = classifier();
        let msgs = vec![msg("user", "la mia key e' AKIAIOSFODNN7EXAMPLE")];
        let r = c.classify_sync(&msgs);
        assert_eq!(r.tier, 3);
        assert!(r.secret_patterns.contains(&"aws_key".to_string()));
        assert!(r.reasons.iter().any(|s| s.contains("tier 3")));
    }

    #[test]
    fn classify_sync_testo_pulito_tier0() {
        let c = classifier();
        let msgs = vec![msg("user", "ciao, mi spieghi una funzione?")];
        let r = c.classify_sync(&msgs);
        assert_eq!(r.tier, 0);
        assert!(r.secret_patterns.is_empty());
        assert!(r.presidio_entities.is_empty());
    }

    #[tokio::test]
    async fn classify_async_senza_presidio_eleva_solo_su_segreto() {
        let c = classifier();
        // Presidio non configurato -> nessuna PII; il segreto eleva a tier 3.
        let msgs = vec![msg("user", "token: ghp_abcdefghijklmnopqrstuvwxyz0123456789")];
        let r = c.classify(&msgs).await;
        assert_eq!(r.tier, 3);
        assert!(r.secret_patterns.contains(&"github_pat".to_string()));
        // Presidio down/non configurato -> nessuna entita'.
        assert!(r.presidio_entities.is_empty());
    }

    #[test]
    fn full_text_concatena_messaggi() {
        let msgs = vec![msg("user", "riga uno"), msg("assistant", "riga due")];
        let txt = SensitivityClassifier::full_text(&msgs);
        assert_eq!(txt, "riga uno\nriga due");
    }

    #[test]
    fn reasons_non_contengono_il_valore_segreto() {
        // Regola F: il reason riporta solo tipo+tier, mai il token.
        let c = classifier();
        let segreto = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let msgs = vec![msg("user", &format!("token {segreto}"))];
        let r = c.classify_sync(&msgs);
        for reason in &r.reasons {
            assert!(!reason.contains(segreto), "il reason non deve contenere il segreto");
        }
    }

    // Helper di compilazione: assicura che il tipo metadata sia accessibile per
    // eventuali test futuri di integrazione (no-op a runtime).
    #[allow(dead_code)]
    fn _meta() -> RequestMetadata {
        RequestMetadata {
            tenant_id: "t".into(),
            user_id: "u".into(),
            request_id: "r".into(),
            sensitivity_tier: 0,
            feature: "chat".into(),
        }
    }
}
