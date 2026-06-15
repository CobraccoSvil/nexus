//! Catena di fallback tra provider.
//!
//! Data una lista ORDINATA di provider (es. quella prodotta dal
//! [`crate::policy_engine::PolicyEngine`] per un tier), [`FallbackChain`] prova
//! il primo provider non in cooldown e tenta la completion. Su errore:
//!   - classifica l'errore (billing vs transitorio) con [`is_billing_error`];
//!   - marca il provider nel [`CooldownManager`] con la causa corretta;
//!   - passa al provider successivo.
//!
//! Se tutti i provider falliscono (o sono tutti in cooldown), ritorna un errore
//! aggregato che riassume i tentativi (senza prompt/response: regola F).
//!
//! Regola L: la decisione "provider disponibile?" delega al [`CooldownManager`]
//! (punto unico dello stato di cooldown); la classificazione billing delega a
//! [`crate::providers::is_billing_error`]. Nessuna logica duplicata qui.

use std::sync::Arc;

use crate::cooldown::CooldownManager;
use crate::provider::LlmProvider;
use crate::providers::is_billing_error;
use crate::types::{LlmRequest, LlmResponse};

/// Esito di un tentativo fallito (per il messaggio d'errore aggregato).
#[derive(Debug)]
struct AttemptFailure {
    provider: String,
    /// `true` se saltato perche' gia' in cooldown (non e' stato chiamato).
    skipped_cooldown: bool,
    /// Messaggio d'errore del tentativo (gia' privo di payload utente).
    error: Option<String>,
}

/// Catena di fallback. Possiede i provider (dietro `Arc`) e condivide il
/// `CooldownManager` con il resto del gateway (clone a basso costo).
pub struct FallbackChain {
    providers: Vec<Arc<dyn LlmProvider>>,
    cooldown: CooldownManager,
}

impl FallbackChain {
    /// Costruisce la catena con i provider GIA' ordinati per priorita'.
    pub fn new(providers: Vec<Arc<dyn LlmProvider>>, cooldown: CooldownManager) -> Self {
        Self {
            providers,
            cooldown,
        }
    }

    /// Esegue la completion provando i provider in ordine. Il primo successo
    /// vince. Su errore marca il cooldown e continua. Ritorna l'errore aggregato
    /// se nessun provider riesce.
    pub async fn complete(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
        let mut failures: Vec<AttemptFailure> = Vec::new();

        for provider in &self.providers {
            let name = provider.name();

            if self.cooldown.is_in_cooldown(name) {
                failures.push(AttemptFailure {
                    provider: name.to_string(),
                    skipped_cooldown: true,
                    error: None,
                });
                continue;
            }

            match provider.complete(req).await {
                Ok(resp) => return Ok(resp),
                Err(err) => {
                    let msg = err.to_string();
                    // Classificazione billing vs transitorio (punto unico).
                    if is_billing_error(&msg) {
                        self.cooldown.mark_billing(name, Some(msg.clone()));
                    } else {
                        self.cooldown.mark_transient(name, Some(msg.clone()));
                    }
                    failures.push(AttemptFailure {
                        provider: name.to_string(),
                        skipped_cooldown: false,
                        error: Some(msg),
                    });
                }
            }
        }

        Err(aggregate_error(&failures))
    }
}

/// Costruisce l'errore aggregato da una lista di tentativi falliti. Riporta per
/// ogni provider lo stato (cooldown / errore). Regola F: gli `error` contengono
/// gia' solo messaggi del provider, mai prompt/response.
fn aggregate_error(failures: &[AttemptFailure]) -> anyhow::Error {
    if failures.is_empty() {
        return anyhow::anyhow!("fallback: nessun provider configurato nella catena");
    }

    let parts: Vec<String> = failures
        .iter()
        .map(|f| {
            if f.skipped_cooldown {
                format!("{} (in cooldown, saltato)", f.provider)
            } else {
                format!(
                    "{} ({})",
                    f.provider,
                    f.error.as_deref().unwrap_or("errore sconosciuto")
                )
            }
        })
        .collect();

    anyhow::anyhow!(
        "fallback: tutti i provider hanno fallito -> {}",
        parts.join("; ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use crate::provider::{ChunkStream, LlmProvider};
    use crate::types::{LlmUsage, RequestMetadata, SensitivityTier};

    /// Esito controllabile del `complete` di un provider finto.
    enum Behaviour {
        Ok,
        ErrBilling,
        ErrTransient,
    }

    struct FakeProvider {
        name: String,
        behaviour: Behaviour,
        complete_calls: AtomicUsize,
    }

    impl FakeProvider {
        fn new(name: &str, behaviour: Behaviour) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                behaviour,
                complete_calls: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl LlmProvider for FakeProvider {
        fn name(&self) -> &str {
            &self.name
        }
        fn supports_tools(&self) -> bool {
            true
        }
        fn supports_streaming(&self) -> bool {
            true
        }
        fn max_context_tokens(&self) -> u32 {
            1000
        }
        fn tier_compatibility(&self) -> &[SensitivityTier] {
            &[0]
        }
        async fn complete(&self, _req: &LlmRequest) -> anyhow::Result<LlmResponse> {
            self.complete_calls.fetch_add(1, Ordering::SeqCst);
            match self.behaviour {
                Behaviour::Ok => Ok(LlmResponse {
                    content: "ok".to_string(),
                    tool_calls: None,
                    usage: LlmUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_read_tokens: None,
                        cache_creation_tokens: None,
                    },
                    model_used: "m".to_string(),
                    provider_used: self.name.clone(),
                    latency_ms: 0,
                    finish_reason: "stop".to_string(),
                    privacy_rerouted: None,
                    reasoning: None,
                    thinking_signature: None,
                }),
                Behaviour::ErrBilling => {
                    anyhow::bail!("openai HTTP 402: insufficient_quota for org")
                }
                Behaviour::ErrTransient => anyhow::bail!("connection reset by peer"),
            }
        }
        async fn stream(&self, _req: &LlmRequest) -> anyhow::Result<ChunkStream> {
            anyhow::bail!("non usato")
        }
        async fn healthcheck(&self) -> bool {
            true
        }
    }

    fn request() -> LlmRequest {
        LlmRequest {
            model: "m".to_string(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
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
        }
    }

    #[tokio::test]
    async fn primo_provider_sano_vince() {
        let p1 = FakeProvider::new("openai", Behaviour::Ok);
        let p2 = FakeProvider::new("mistral", Behaviour::Ok);
        let chain = FallbackChain::new(
            vec![p1.clone(), p2.clone()],
            CooldownManager::new(),
        );

        let resp = chain.complete(&request()).await.unwrap();
        assert_eq!(resp.provider_used, "openai");
        // Il secondo provider non viene nemmeno chiamato.
        assert_eq!(p1.complete_calls.load(Ordering::SeqCst), 1);
        assert_eq!(p2.complete_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn fallback_su_billing_marca_cooldown_e_passa_al_successivo() {
        let p1 = FakeProvider::new("openai", Behaviour::ErrBilling);
        let p2 = FakeProvider::new("mistral", Behaviour::Ok);
        let cooldown = CooldownManager::new();
        let chain = FallbackChain::new(vec![p1.clone(), p2.clone()], cooldown.clone());

        let resp = chain.complete(&request()).await.unwrap();
        // Il fallback ha funzionato: mistral risponde.
        assert_eq!(resp.provider_used, "mistral");
        assert_eq!(p1.complete_calls.load(Ordering::SeqCst), 1);
        assert_eq!(p2.complete_calls.load(Ordering::SeqCst), 1);
        // openai e' stato messo in cooldown billing.
        assert!(cooldown.is_in_cooldown("openai"));
        let snap = cooldown.snapshot();
        let entry = snap.iter().find(|s| s.name == "openai").unwrap();
        assert!(entry.billing_error.is_some());
    }

    #[tokio::test]
    async fn provider_gia_in_cooldown_viene_saltato() {
        let p1 = FakeProvider::new("openai", Behaviour::Ok);
        let p2 = FakeProvider::new("mistral", Behaviour::Ok);
        let cooldown = CooldownManager::new();
        // Pre-marca openai: la catena deve saltarlo senza chiamarlo.
        cooldown.mark_billing("openai", Some("credit balance too low".to_string()));
        let chain = FallbackChain::new(vec![p1.clone(), p2.clone()], cooldown.clone());

        let resp = chain.complete(&request()).await.unwrap();
        assert_eq!(resp.provider_used, "mistral");
        // openai NON e' stato chiamato (saltato per cooldown).
        assert_eq!(p1.complete_calls.load(Ordering::SeqCst), 0);
        assert_eq!(p2.complete_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn tutti_falliscono_ritorna_errore_aggregato() {
        let p1 = FakeProvider::new("openai", Behaviour::ErrBilling);
        let p2 = FakeProvider::new("mistral", Behaviour::ErrTransient);
        let cooldown = CooldownManager::new();
        let chain = FallbackChain::new(vec![p1.clone(), p2.clone()], cooldown.clone());

        let err = chain.complete(&request()).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("openai"));
        assert!(msg.contains("mistral"));
        assert!(msg.contains("tutti i provider hanno fallito"));
        // Cooldown applicato con le cause corrette.
        assert!(cooldown.is_in_cooldown("openai"));
        assert!(cooldown.is_in_cooldown("mistral"));
        let snap = cooldown.snapshot();
        let openai = snap.iter().find(|s| s.name == "openai").unwrap();
        let mistral = snap.iter().find(|s| s.name == "mistral").unwrap();
        // openai billing, mistral transient (niente billing_error).
        assert!(openai.billing_error.is_some());
        assert!(mistral.billing_error.is_none());
    }

    #[tokio::test]
    async fn catena_vuota_ritorna_errore() {
        let chain = FallbackChain::new(vec![], CooldownManager::new());
        let err = chain.complete(&request()).await.unwrap_err();
        assert!(err.to_string().contains("nessun provider configurato"));
    }
}
