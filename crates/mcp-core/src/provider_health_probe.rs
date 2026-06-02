//! Worker `provider_health_probe` — pinga ogni provider LLM ogni 5 minuti
//! con un prompt minimale per accertarne la salute reale (non solo presenza
//! API key).
//!
//! Motivazione: il vecchio `/api/gateway/providers` ritornava `healthy:true`
//! per tutti i provider con API key configurata, indipendentemente dal fatto
//! che il provider rispondesse o meno (es. quota esaurita, rate limit). Il
//! LED nello statusbar restava verde fino al primo errore reale fatto da un
//! utente. Con questo worker, lo stato e' sondato proattivamente:
//!
//!   - Se la risposta e' OK in <10s → provider healthy.
//!   - Se la risposta e' un errore di tipo billing/quota → cooldown lungo (6h).
//!   - Se timeout >10s → cooldown breve (60s) come "slow".
//!
//! Il risultato e' persistito in `nexus_provider_health_history` per:
//!   - Letture rapide da `gateway_providers_handler` (`last_health_check_at`).
//!   - Dashboard admin con grafici latency / error rate.
//!
//! Costo: 5 provider × ~1 token risposta × 12 check/h × 24h ≈ 1500 tokens/giorno
//! totali → trascurabile (~$0.001/giorno con i prezzi attuali).
//!
//! Configurazione via env:
//!   - `NEXUS_PROVIDER_HEALTH_PROBE_ENABLED=true` (default: true; disabilita in dev)
//!   - `NEXUS_PROVIDER_HEALTH_PROBE_INTERVAL_S=300` (default: 300, min 60)

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use sqlx::PgPool;
use tokio::time::sleep;

use crate::orchestrator::{default_model_for_provider, Orchestrator};
use crate::provider_cooldown::{is_provider_in_cooldown, provider_health_timings, put_provider_in_long_cooldown, remove_cooldown};
// `put_provider_in_cooldown` e' `pub(crate)` -> accessibile, ma la signature
// e' `(provider: &str, retry_after_seconds: Option<u64>)`. Per slow/timeout
// usiamo l'overload corto.
use crate::provider_cooldown::put_provider_in_cooldown;

/// Lista dei provider da probare. Allineata con `KNOWN_PROVIDERS` in
/// `orchestrator.rs`. Mantenuta hard-coded perche' sono note staticamente
/// (un nuovo provider richiede comunque modifiche al codice di routing).
const PROBED_PROVIDERS: &[&str] = &["anthropic", "openai", "google", "deepseek", "mistral"];

/// Prompt minimale: 1 parola, ci aspettiamo una risposta breve.
/// Il provider tipicamente risponde con "Hi!" o "Hello!" (1-2 token).
const PROBE_PROMPT: &str = "hi";

// Timeout del probe ("slow" oltre soglia) e cooldown breve per provider lento
// sono DB-driven: settings `provider.health_probe_timeout_s` /
// `provider.slow_cooldown_s` (migrazione 0252). Vedi
// `provider_cooldown::provider_health_timings()`. I default storici (30s / 60s)
// restano in `ProviderHealthTimings::default()`.

/// Avvia il worker in background. Restituisce subito; il loop gira per
/// l'intera vita del processo.
///
/// Chiamato da `main.rs` con i valori letti dal DB (tabella settings).
/// Override di emergenza via env: `NEXUS_PROVIDER_HEALTH_PROBE_ENABLED`,
/// `NEXUS_PROVIDER_HEALTH_PROBE_INTERVAL_S` (priorita' piu' alta del DB).
pub fn spawn_health_probe(orchestrator: Arc<Orchestrator>, db: PgPool, enabled: bool, interval_s: u64) {
    // L'env var resta come override di emergenza (priorita' > DB).
    let enabled = match std::env::var("NEXUS_PROVIDER_HEALTH_PROBE_ENABLED").as_deref() {
        Ok("false") | Ok("0") => false,
        Ok("true") | Ok("1") => true,
        _ => enabled,
    };
    if !enabled {
        tracing::info!("provider_health_probe: DISABILITATO (provider_health_probe_enabled=false)");
        return;
    }
    let interval_s = std::env::var("NEXUS_PROVIDER_HEALTH_PROBE_INTERVAL_S")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(interval_s)
        .max(60);
    tracing::info!(
        "provider_health_probe: avvio worker (interval={}s, providers={:?})",
        interval_s, PROBED_PROVIDERS,
    );
    tokio::spawn(async move {
        // Aspetta 30s al primo avvio per dare tempo agli altri servizi
        // di stabilizzarsi (DB ready, brain pronto, ecc.).
        sleep(Duration::from_secs(30)).await;
        loop {
            run_one_round(&orchestrator, &db).await;
            sleep(Duration::from_secs(interval_s)).await;
        }
    });
}

// Soglia "outage locale": se in un solo round troppi provider distinti
// falliscono con un errore non-billing, e' praticamente certo che il problema
// sia l'infrastruttura locale (brain bridge giu', rete WSL ko, DNS) e non un
// guasto simultaneo dei provider remoti. In quel caso annulliamo i cooldown
// applicati nel round per evitare di marcare tutti i provider come down (visto
// in prod il 17-18 maggio: 5 provider down in 8 secondi con "tcp connect error"
// mentre il brain era riavviato). Soglia DB-driven: setting
// `provider.outage_threshold` (migrazione 0252).

/// Accumula i provider che hanno fallito nel round corrente con cooldown
/// applicato, per poterli liberare se viene rilevato un outage locale.
static ROUND_COOLDOWN_VICTIMS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn round_victims_push(provider: &str) {
    let store = ROUND_COOLDOWN_VICTIMS.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut v) = store.lock() {
        v.push(provider.to_string());
    }
}

fn round_victims_drain() -> Vec<String> {
    let store = ROUND_COOLDOWN_VICTIMS.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut v) = store.lock() {
        std::mem::take(&mut *v)
    } else {
        Vec::new()
    }
}

/// Esegue un ciclo completo di probe per tutti i provider.
///
/// Anche i provider in cooldown vengono pingati (a cadenza ridotta) per
/// rilevare il recovery automatico: senza questo, un provider che torna
/// a funzionare dopo billing recharge resterebbe "down" fino alla scadenza
/// naturale del cooldown (anche 6h). Il costo extra e' trascurabile —
/// stiamo parlando di una chiamata da ~1 token ogni 5 minuti.
async fn run_one_round(orchestrator: &Orchestrator, db: &PgPool) {
    let _ = round_victims_drain(); // reset accumulator
    for provider in PROBED_PROVIDERS {
        let in_cooldown = is_provider_in_cooldown(provider);
        if in_cooldown {
            tracing::debug!(
                "provider_health_probe: probe {provider} (era in cooldown — test recovery)"
            );
        }
        probe_one(orchestrator, db, provider).await;
        // Distanzia le chiamate ai provider per non saturare la rete
        // (anche se sono indipendenti, evita spike di traffico).
        sleep(Duration::from_secs(2)).await;
    }
    // Outage detection: se troppi provider sono andati in cooldown breve
    // nello stesso round, e' infrastruttura locale. Rollback.
    let victims = round_victims_drain();
    if victims.len() >= provider_health_timings().outage_threshold {
        tracing::warn!(
            "provider_health_probe: OUTAGE LOCALE rilevato — {} provider hanno fallito \
             nello stesso round ({:?}). Rollback dei cooldown applicati: e' quasi \
             sicuramente brain bridge / rete / DNS, non i provider remoti.",
            victims.len(),
            victims,
        );
        for p in victims {
            remove_cooldown(&p);
        }
    }
}

/// Pinga un singolo provider. Persiste sempre il risultato (anche success).
async fn probe_one(orchestrator: &Orchestrator, db: &PgPool, provider: &str) {
    // ── Budget exhausted check ────────────────────────────────────────────
    // I provider AI consumer non espongono balance via API per la maggior
    // parte (vedi tabella in 0173_provider_budget_tracking.sql). Tracciamo
    // internamente lo speso e dichiariamo il provider unhealthy quando
    // (monthly_budget - spent) < min_threshold. Cosi' la UI mostra LED
    // giallo/rosso e il routing dinamico evita il provider.
    let budget_exhausted: Option<bool> = sqlx::query_scalar(
        "SELECT is_exhausted FROM provider_budget_remaining_view
          WHERE provider = $1 AND monthly_budget_usd > 0",
    )
    .bind(provider)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    if budget_exhausted == Some(true) {
        tracing::warn!(
            "provider_health_probe: {provider} budget esaurito (tracking interno) — skip probe + cooldown lungo"
        );
        put_provider_in_long_cooldown(provider, "budget_exhausted");
        let _ = sqlx::query(
            r#"INSERT INTO nexus_provider_health_history
               (provider, healthy, latency_ms, error_kind, error_message)
               VALUES ($1, false, 0, 'budget_exhausted',
                       'Budget mensile per il provider esaurito. Vai in Admin > Provider LLM > Ricarica budget.')"#,
        )
        .bind(provider)
        .execute(db)
        .await;
        nexus_events::dispatcher::broadcast_all_global(
            nexus_events::ProjectEvent::ProviderHealthChanged {
                provider: provider.to_string(),
                status: "down".to_string(),
                latency_ms: None,
            },
        );
        return;
    }

    let matrix_arc = match orchestrator.routing_matrix.current_async().await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("probe_one: routing_matrix non disponibile ({e}), skip {provider}");
            return;
        }
    };
    let model = default_model_for_provider(&matrix_arc, provider);
    let timings = provider_health_timings();
    let probe_timeout_s = timings.health_probe_timeout_s;
    let slow_cooldown_s = timings.slow_cooldown_s;
    let started = Instant::now();
    // generate_completion vive su `NeuralCoreClient`, accessibile via il
    // campo `pub(crate) neural` di `Orchestrator`.
    let result: Result<anyhow::Result<serde_json::Value>, tokio::time::error::Elapsed> =
        tokio::time::timeout(
            Duration::from_secs(probe_timeout_s),
            orchestrator.neural.generate_completion(provider, &model, PROBE_PROMPT),
        )
        .await;
    let latency_ms = started.elapsed().as_millis() as i32;

    let (healthy, error_kind, error_message) = match result {
        Ok(Ok(response)) => {
            // Detection "errore ingoiato dal brain": brain/providers/*.py
            // intercetta Exception e ritorna ProviderResult con content
            // "[Error: ...]" invece di propagare. Senza questo check, il probe
            // marcava il provider come healthy mentre in realta' billing era
            // esaurito (caso reale 2026-05-20: LED verde su anthropic ma run
            // reali fallivano con credit_balance_too_low).
            let content_text = extract_response_content_text(&response);
            let trimmed = content_text.trim();
            // error_class CANONICO dal brain (error_handler) e' la fonte di
            // verita': il content puo' essere un messaggio umano che NON inizia
            // con "[Error:" (il brain riformatta), quindi non basta il check sul
            // prefisso. Se error_class e' presente, deriva il kind da quello.
            let ec_canon = response
                .get("error_class")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    response
                        .get("metadata")
                        .and_then(|m| m.get("error_class"))
                        .and_then(|v| v.as_str())
                })
                .unwrap_or("")
                .to_string();
            let is_brain_swallowed_error =
                trimmed.starts_with("[Error:") || trimmed.starts_with("[error:");
            if !ec_canon.is_empty() || is_brain_swallowed_error {
                let inner = trimmed.trim_start_matches('[').trim_end_matches(']');
                // error_class dal response se presente, altrimenti dal punto
                // UNICO (brain gRPC). Nessuna classificazione locale di testo.
                let ec = if !ec_canon.is_empty() {
                    ec_canon.clone()
                } else {
                    orchestrator.neural.classify_error(inner, provider).await
                };
                let kind = kind_from_error_class(&ec);
                tracing::warn!(
                    "provider_health_probe: {provider} errore provider (class={kind})"
                );
                // billing/auth: persistenti -> long cooldown (servono soldi/key).
                let billing = matches!(
                    kind.as_str(),
                    "quota_exceeded" | "credit_balance_too_low" | "billing_required" | "auth_error"
                );
                if billing {
                    put_provider_in_long_cooldown(provider, &kind);
                } else {
                    put_provider_in_cooldown(provider, Some(slow_cooldown_s));
                }
                let detail = if inner.is_empty() { content_text.as_str() } else { inner };
                (false, Some(kind), Some(truncate(detail, 500)))
            } else {
                // Probe-OK con "hi" (1-2 token output): NON garantisce che
                // il provider sia veramente pronto per workload reali. Un
                // account anthropic con credit basso accetta call da 1 token
                // ma fallisce su 5000+ token. Quindi:
                //   - se in LONG cooldown (billing/quota): NON rimuovo. Solo
                //     un run reale di chat_messages.rs puo' confermare il
                //     recovery (vede [Error:] o success reale).
                //   - se in SHORT cooldown (rate_limit/timeout): rimuovo,
                //     perche' "hi" e' sufficiente a verificare che il provider
                //     non sia piu' rate-limited.
                let long_kinds = ["billing_error", "quota_exceeded",
                    "credit_balance_too_low", "billing_required"];
                let is_in_long = crate::provider_cooldown::cooldown_snapshot()
                    .iter()
                    .find(|(name, _, _)| name == provider)
                    .map(|(_, _, reason)| {
                        reason.as_ref().is_some_and(|r| {
                            long_kinds.iter().any(|k| r.contains(k)) ||
                            r.contains("credit") || r.contains("quota") ||
                            r.contains("billing")
                        })
                    })
                    .unwrap_or(false);
                if is_provider_in_cooldown(provider) {
                    if is_in_long {
                        tracing::debug!(
                            "provider_health_probe: {provider} probe-OK ma in LONG cooldown — non rimuovo (attendo run reale)"
                        );
                    } else {
                        tracing::info!(
                            "provider_health_probe: {provider} RECOVERED — rimuovo cooldown breve residuo"
                        );
                        remove_cooldown(provider);
                    }
                }
                tracing::debug!(
                    "provider_health_probe: {provider} OK in {latency_ms}ms"
                );
                (true, None, None)
            }
        }
        Ok(Err(e)) => {
            // Provider ha risposto con errore (HTTP error o JSON parse).
            // Classifico per decidere il tipo di cooldown.
            let msg = e.to_string();
            // Classificazione via il punto UNICO (brain gRPC); niente pattern locali.
            let kind = kind_from_error_class(&orchestrator.neural.classify_error(&msg, provider).await);
            tracing::warn!(
                "provider_health_probe: {provider} ERROR ({kind}) in {latency_ms}ms: {msg}",
                msg = &msg[..msg.len().min(200)],
            );
            // Categorie di errore:
            //   - billing/quota → cooldown lungo (6h): provider DAVVERO down per credito
            //   - infrastruttura locale (tcp connect / gRPC Unavailable / DNS):
            //     NON cooldown — il problema e' della rete o del brain bridge,
            //     non del provider remoto. Senza questa eccezione un hiccup
            //     di rete locale marcava simultaneamente tutti i provider come
            //     down (visto in produzione: 5 provider falliti in 8 secondi).
            //   - altri (rate_limit/timeout/auth/unknown) → cooldown breve 60s
            let is_local_infra =
                matches!(kind.as_str(), "connection_error")
                || msg.contains("tcp connect error")
                || msg.contains("Unavailable")
                || msg.contains("ECONNREFUSED");
            if matches!(
                kind.as_str(),
                "quota_exceeded" | "credit_balance_too_low" | "billing_required"
            ) {
                put_provider_in_long_cooldown(provider, &kind);
            } else if is_local_infra {
                tracing::warn!(
                    "provider_health_probe: {provider} ERROR ma sembra problema di rete \
                     locale / brain bridge — NESSUN cooldown applicato"
                );
                // Anche se NON applichiamo cooldown ora, contiamolo per
                // outage detection: se 3+ provider hanno is_local_infra in
                // un round, e' OUTAGE certo e NON dobbiamo applicare nulla.
                round_victims_push(provider);
            } else {
                put_provider_in_cooldown(provider, Some(slow_cooldown_s));
                round_victims_push(provider);
            }
            (false, Some(kind), Some(truncate(&msg, 500)))
        }
        Err(_timeout_elapsed) => {
            // Timeout: provider troppo lento. Cooldown breve.
            tracing::warn!(
                "provider_health_probe: {provider} TIMEOUT (>{probe_timeout_s}s)"
            );
            put_provider_in_cooldown(provider, Some(slow_cooldown_s));
            // Timeout: spesso e' anch'esso un sintomo di outage locale
            // (brain bridge lento, WSL DNS lento, internet bloccato). Conta
            // come victim per outage detection.
            round_victims_push(provider);
            (
                false,
                Some("timeout".to_string()),
                Some(format!("nessuna risposta in {probe_timeout_s}s")),
            )
        }
    };

    // Notifica tutti i client connessi (event-driven, no polling nel pannello provider).
    nexus_events::dispatcher::broadcast_all_global(
        nexus_events::ProjectEvent::ProviderHealthChanged {
            provider: provider.to_string(),
            status: if healthy { "up".to_string() } else { "down".to_string() },
            latency_ms: Some(latency_ms as i64),
        },
    );

    // Persistenza fire-and-forget. Errori del DB non interrompono il loop.
    let row_result = sqlx::query(
        r#"INSERT INTO nexus_provider_health_history
           (provider, healthy, latency_ms, error_kind, error_message)
           VALUES ($1, $2, $3, $4, $5)"#,
    )
    .bind(provider)
    .bind(healthy)
    .bind(latency_ms)
    .bind(error_kind.as_deref())
    .bind(error_message.as_deref())
    .execute(db)
    .await;
    if let Err(e) = row_result {
        tracing::warn!("provider_health_probe: persistenza fallita per {provider}: {e}");
    }
}

/// Esito sintetico di un probe attivo, usato dalla logica probe-then-reenable
/// (`provider_cooldown::billing_cooldown_recovery_loop`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// Il provider ha risposto correttamente: e' sano.
    Healthy,
    /// Errore billing/quota: il credito e' ancora insufficiente.
    Billing(String),
    /// Esito non conclusivo (rate-limit, timeout, errore di rete/infra, ecc.).
    Transient(String),
}

/// Esegue un singolo probe "hi" verso un provider e ne classifica l'esito.
///
/// Funzione pura rispetto allo stato di cooldown: NON applica/rimuove cooldown
/// ne' persiste history — si limita a interrogare il provider e classificare.
/// Il chiamante decide cosa fare. Riusa `extract_response_content_text` e
/// `classify_probe_error` con la stessa semantica di `probe_one`, inclusa la
/// detection dell'errore "ingoiato dal brain" (`[Error: ...]`).
pub async fn probe_provider_once(
    orchestrator: &Orchestrator,
    provider: &str,
    timeout_s: u64,
) -> ProbeOutcome {
    let matrix_arc = match orchestrator.routing_matrix.current_async().await {
        Ok(m) => m,
        Err(e) => return ProbeOutcome::Transient(format!("routing_matrix non disponibile: {e}")),
    };
    let model = default_model_for_provider(&matrix_arc, provider);
    let result = tokio::time::timeout(
        Duration::from_secs(timeout_s.max(1)),
        orchestrator.neural.generate_completion(provider, &model, PROBE_PROMPT),
    )
    .await;

    match result {
        Ok(Ok(response)) => {
            // error_class CANONICO dal response: il brain riformatta l'errore in
            // un messaggio umano che NON inizia con "[Error:", quindi senza
            // questo check la recovery-loop credeva il provider sano e lo
            // riabilitava (bug: openai/anthropic senza credito tornavano attivi).
            let ec = response
                .get("error_class")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    response
                        .get("metadata")
                        .and_then(|m| m.get("error_class"))
                        .and_then(|v| v.as_str())
                })
                .unwrap_or("");
            if !ec.is_empty() {
                return outcome_from_error_class(ec);
            }
            let content_text = extract_response_content_text(&response);
            let trimmed = content_text.trim();
            if trimmed.starts_with("[Error:") || trimmed.starts_with("[error:") {
                let inner = trimmed.trim_start_matches('[').trim_end_matches(']');
                outcome_from_error_class(&orchestrator.neural.classify_error(inner, provider).await)
            } else {
                ProbeOutcome::Healthy
            }
        }
        Ok(Err(e)) => {
            outcome_from_error_class(&orchestrator.neural.classify_error(&e.to_string(), provider).await)
        }
        Err(_elapsed) => ProbeOutcome::Transient("timeout".to_string()),
    }
}

/// Mappa l'error_class CANONICO (dal brain, unico classificatore) in
/// `ProbeOutcome`: billing/auth → `Billing` (down per credito/key), resto →
/// `Transient`. Nessuna classificazione di testo locale.
fn outcome_from_error_class(ec: &str) -> ProbeOutcome {
    let kind = kind_from_error_class(ec);
    if matches!(
        kind.as_str(),
        "quota_exceeded" | "credit_balance_too_low" | "billing_required" | "auth_error"
    ) {
        ProbeOutcome::Billing(kind)
    } else {
        ProbeOutcome::Transient(kind)
    }
}

/// Mappa l'error_class canonico al "kind" di cooldown. Solo corrispondenza
/// valore->valore, nessun pattern (la classificazione e' nel brain).
fn kind_from_error_class(ec: &str) -> String {
    match ec {
        "billing_error" => "credit_balance_too_low".to_string(),
        // 401: credenziali invalide -> tutte le chiamate falliranno, il provider
        // va messo in cooldown (vedi outcome_from_error_class -> Billing).
        "auth_error" => "auth_error".to_string(),
        // 403: NON e' un problema di credenziali. E' per-modello/per-risorsa
        // (es. Mistral 403 labs_not_enabled: un modello Labs non abilitato
        // nell'org; oppure un modello senza accesso sul progetto). Mantenendo
        // il kind "forbidden" l'outcome resta Transient -> cooldown breve sul
        // singolo probe, senza disabilitare l'intero provider per 6 ore.
        "forbidden" => "forbidden".to_string(),
        "" => "error".to_string(),
        other => other.to_string(),
    }
}

/// Classifica un messaggio di errore in una categoria nota. Mirror della
/// logica di `brain_agent_client.rs::classify_provider_error`.
/// Estrae il content testuale dalla response gRPC del brain.
/// Usato per intercettare "[Error: ...]" che il brain ritorna in caso di
/// exception (vedi brain/providers/anthropic_provider.py:211 e simili).
fn extract_response_content_text(value: &serde_json::Value) -> String {
    if let Some(s) = value.get("content").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(arr) = value.get("choices").and_then(|v| v.as_array()) {
        if let Some(first) = arr.first() {
            if let Some(s) = first
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                return s.to_string();
            }
        }
    }
    String::new()
}

/// Tronca una stringa a `max` caratteri (per evitare TEXT giganti nel DB).
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_from_error_class_mappa_billing() {
        assert_eq!(kind_from_error_class("billing_error"), "credit_balance_too_low");
        assert_eq!(kind_from_error_class("auth_error"), "auth_error");
        assert_eq!(kind_from_error_class("rate_limit"), "rate_limit");
        // Regressione: 403 forbidden (es. Mistral labs_not_enabled) NON deve
        // collassare in auth_error, altrimenti finisce in long cooldown 6h.
        assert_eq!(kind_from_error_class("forbidden"), "forbidden");
    }

    #[test]
    fn outcome_from_error_class_billing_e_transient() {
        assert!(matches!(outcome_from_error_class("billing_error"), ProbeOutcome::Billing(_)));
        assert!(matches!(outcome_from_error_class("rate_limit"), ProbeOutcome::Transient(_)));
        // 401 auth_error: il provider e' inutilizzabile -> Billing (long cooldown).
        assert!(matches!(outcome_from_error_class("auth_error"), ProbeOutcome::Billing(_)));
        // 403 forbidden: per-modello/per-risorsa -> Transient (short cooldown),
        // non spegne l'intero provider.
        assert!(matches!(outcome_from_error_class("forbidden"), ProbeOutcome::Transient(_)));
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("ciao", 10), "ciao");
        assert_eq!(truncate("ciao mondo bellissimo", 5), "ciao …");
    }

    #[test]
    fn outcome_billing_per_credito_e_quota() {
        // error_class canonici (dal brain, unico classificatore) di tipo
        // billing/quota -> Billing: il provider resta in long cooldown finche'
        // un 200 reale non lo riabilita. La classificazione da testo grezzo non
        // esiste piu' in mcp-core (vedi outcome_from_error_class).
        assert_eq!(
            outcome_from_error_class("billing_error"),
            ProbeOutcome::Billing("credit_balance_too_low".to_string())
        );
        assert_eq!(
            outcome_from_error_class("quota_exceeded"),
            ProbeOutcome::Billing("quota_exceeded".to_string())
        );
        assert_eq!(
            outcome_from_error_class("billing_required"),
            ProbeOutcome::Billing("billing_required".to_string())
        );
    }

    #[test]
    fn outcome_transient_per_rate_limit_e_timeout() {
        // error_class canonici non-billing -> Transient: nuovo tentativo al giro dopo.
        assert_eq!(
            outcome_from_error_class("rate_limit"),
            ProbeOutcome::Transient("rate_limit".to_string())
        );
        assert_eq!(
            outcome_from_error_class("timeout"),
            ProbeOutcome::Transient("timeout".to_string())
        );
        assert_eq!(
            outcome_from_error_class("connection_error"),
            ProbeOutcome::Transient("connection_error".to_string())
        );
    }
}
