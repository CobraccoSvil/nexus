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
use crate::provider_cooldown::{
    is_provider_in_cooldown, provider_health_timings, put_provider_in_long_cooldown,
    remove_cooldown,
};
// `put_provider_in_cooldown` non usato qui: cooldown delegato a
// `model_health_probe::dispatch_probe_error` (punto unico classificazione).

/// Fallback dei provider da probare, usato SOLO se il catalog e' irraggiungibile
/// o vuoto (fail-safe: non smettere di sondare i provider core per una query
/// fallita). La lista PRIMARIA e' data-driven dal catalog (vedi
/// [`probed_providers`]): un provider con almeno un modello `is_enabled` viene
/// sondato automaticamente, senza toccare questa costante (regola G).
const FALLBACK_PROBED_PROVIDERS: &[&str] = &["anthropic", "openai", "google", "deepseek", "mistral"];

/// Provider da sondare in un round, derivati dal catalog (regola G/L): i
/// provider con almeno un modello abilitato. Un provider nuovo (es. onboardato
/// con la sua migrazione catalog) entra nel probe senza modifiche al codice.
pub(crate) async fn probed_providers(db: &PgPool) -> Vec<String> {
    let from_db: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT provider FROM ai_price_catalog WHERE is_enabled = true ORDER BY provider",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();
    resolve_probed_providers(from_db)
}

/// Applica il fallback ai provider noti se la lista dal catalog e' vuota (query
/// fallita o catalog senza modelli abilitati). Puro: testabile senza DB.
fn resolve_probed_providers(from_db: Vec<String>) -> Vec<String> {
    if from_db.is_empty() {
        FALLBACK_PROBED_PROVIDERS
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        from_db
    }
}

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
pub fn spawn_health_probe(
    orchestrator: Arc<Orchestrator>,
    db: PgPool,
    enabled: bool,
    interval_s: u64,
) {
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
        "provider_health_probe: avvio worker (interval={}s, providers=data-driven dal catalog, fallback={:?})",
        interval_s,
        FALLBACK_PROBED_PROVIDERS,
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
/// I provider in cooldown TRANSIENT (rate-limit/timeout/rete) vengono pingati
/// per rilevare il recovery rapido. I provider in cooldown BILLING (credito/
/// quota esaurito) vengono invece SALTATI: il loro re-probe e' gestito dal loop
/// dedicato `billing_cooldown_recovery_loop` (ogni BILLING_REPROBE_INTERVAL_S).
/// Sondarli ANCHE qui ogni ~5 min rinnovava il cooldown lungo (6h) e generava
/// 500 a cascata sul gateway (incidente Beauty-Book): regola L, un solo punto
/// ri-testa i billing.
async fn run_one_round(orchestrator: &Orchestrator, db: &PgPool) {
    let _ = round_victims_drain(); // reset accumulator
    let providers = probed_providers(db).await;
    for provider in &providers {
        let provider = provider.as_str();
        if crate::provider_cooldown::is_provider_in_billing_cooldown(provider) {
            tracing::debug!(
                "provider_health_probe: skip {provider} (billing cooldown — \
                 gestito dal recovery loop dedicato)"
            );
            continue;
        }
        let in_cooldown = is_provider_in_cooldown(provider);
        if in_cooldown {
            tracing::debug!(
                "provider_health_probe: probe {provider} (era in cooldown transient — test recovery)"
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

/// Con quale modello si sonda un fornitore: l'esito e' un TIPO perche' i casi
/// sono TRE e il terzo non e' un modello (regola Q).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelloProbe {
    /// Il default della matrice e' abilitato a catalogo: si usa quello.
    Configurato(String),
    /// Il default configurato NON e' abilitato (deprecato dal fornitore,
    /// squalificato, fuori policy): si sonda con un modello vivo e lo si
    /// DICHIARA, invece di far risultare malato il fornitore per colpa di un
    /// modello morto.
    Sostituito { usato: String, configurato: String },
    /// Il fornitore non ha nemmeno un modello abilitato: non c'e' niente da
    /// interrogare, e non e' un fornitore giu' (lo dice gia'
    /// `ProviderReadiness::Stalled(NoModels)`).
    NessunoAbilitato,
}

impl ModelloProbe {
    /// Il modello da interrogare, se ce n'e' uno.
    pub(crate) fn modello(&self) -> Option<&str> {
        match self {
            ModelloProbe::Configurato(m) => Some(m.as_str()),
            ModelloProbe::Sostituito { usato, .. } => Some(usato.as_str()),
            ModelloProbe::NessunoAbilitato => None,
        }
    }

    /// Il modello da interrogare, DICHIARANDO la sostituzione se c'e' stata.
    ///
    /// I due chiamanti (il giro periodico e il probe-then-reenable) fanno la
    /// stessa cosa e la fanno qui: tenere separati log e scelta li obbligava a
    /// due righe identiche ciascuno, e la prossima chiamata ne dimenticherebbe
    /// una — il log, cioe' proprio cio' che rende visibile la sostituzione.
    pub(crate) fn risolvi(&self, provider: &str) -> Option<String> {
        if let ModelloProbe::Sostituito { usato, configurato } = self {
            tracing::warn!(
                target: "provider_health_probe",
                provider = %provider,
                configurato = %configurato,
                usato = %usato,
                "il default del fornitore non e' abilitato a catalogo: sondo con un modello vivo"
            );
        }
        self.modello().map(str::to_string)
    }
}

/// Sceglie il modello del probe: il default della matrice SE il catalogo lo
/// abilita, altrimenti un modello abilitato dello stesso fornitore.
///
/// MISURATO il 17/08/2026: `nexus_provider_default_model` puntava per groq a
/// `llama-3.1-8b-instant` (disabilitato dal 15/07, `disqualified`, e rimosso
/// dal fornitore) e per perplexity a `sonar` (disabilitato). Il probe li
/// interrogava lo stesso, prendeva 404 `model_not_found` e l'intero fornitore
/// risultava `not_found` nello stato provider — un modello morto che fa
/// apparire malato un endpoint che risponde 200 (verificato con probe diretto
/// su `openai/gpt-oss-20b`: HTTP 200 mentre lo stato del fornitore diceva
/// `not_found`).
///
/// La domanda «quale modello uso per sondare» non puo' avere per risposta un
/// modello che il catalogo ha spento: i fornitori deprecano di continuo, e un
/// default che punta nel vuoto si ripresenta (due su nove, il giorno della
/// misura). Fra gli abilitati si preferisce il piu' economico: il probe e' una
/// chiamata a pagamento che si ripete a ogni giro.
pub(crate) async fn modello_per_probe(
    db: &PgPool,
    matrix: &crate::routing_matrix::RoutingMatrix,
    provider: &str,
) -> ModelloProbe {
    let configurato = default_model_for_provider(matrix, provider);
    // UNA query per le due domande («il default e' abilitato?» e «qual e' il
    // piu' economico fra gli abilitati?»): due letture separate darebbero due
    // fotografie dello stesso catalogo, e su un sync concorrente potrebbero
    // non essere la stessa.
    let righe: Vec<(String, bool)> = sqlx::query_as(
        "SELECT model, is_enabled FROM ai_price_catalog           WHERE provider = $1 AND (model = $2 OR is_enabled = true)           ORDER BY input_cost_per_million_tokens ASC NULLS LAST, model",
    )
    .bind(provider)
    .bind(&configurato)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    if righe
        .iter()
        .any(|(model, abilitato)| *abilitato && model == &configurato)
    {
        return ModelloProbe::Configurato(configurato);
    }
    match righe.into_iter().find(|(_, abilitato)| *abilitato) {
        Some((usato, _)) => ModelloProbe::Sostituito { usato, configurato },
        None => ModelloProbe::NessunoAbilitato,
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
    //
    // Il criterio "esiste un tetto?" NON e' piu' un `AND monthly_budget_usd > 0`
    // scritto qui dentro: lo decide `provider_spend_cap`, che e' anche cio' che
    // il pannello admin interroga. Con due `> 0` in due posti, enforcement e
    // pannello potevano dare risposte diverse sullo stesso fornitore — ed e'
    // esattamente cio' che accadeva: qui il fornitore senza tetto veniva
    // scartato in silenzio, li' spariva dalla lista.
    let fatti: Option<(Option<String>, Option<String>, bool)> = sqlx::query_as(
        "SELECT monthly_budget_usd::text, spent_current_period_usd::text, is_exhausted
           FROM provider_budget_remaining_view
          WHERE provider = $1",
    )
    .bind(provider)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let cap = match &fatti {
        // Nessuna riga (o query fallita: `.ok()` sopra le confonde, come faceva
        // gia' il codice precedente). In entrambi i casi non si ferma nessuno —
        // e' la direzione giusta in cui sbagliare: un errore del DB non deve
        // togliere di mezzo un fornitore sano.
        None => crate::provider_spend_cap::SpendCap::UncappedIdle,
        Some((budget, spent, esaurito)) => crate::provider_spend_cap::classifica(
            budget.as_deref().and_then(|v| v.parse().ok()),
            spent.as_deref().and_then(|v| v.parse().ok()),
            *esaurito,
        ),
    };
    if cap.ferma_adesso() {
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
    let Some(model) = modello_per_probe(db, &matrix_arc, provider)
        .await
        .risolvi(provider)
    else {
        tracing::warn!(
            target: "provider_health_probe",
            provider = %provider,
            "nessun modello abilitato: niente da sondare, salto il giro"
        );
        return;
    };
    let timings = provider_health_timings();
    let probe_timeout_s = timings.health_probe_timeout_s;
    let started = Instant::now();
    // generate_completion vive su `NeuralCoreClient`, accessibile via il
    // campo `pub(crate) neural` di `Orchestrator`.
    let result: Result<anyhow::Result<serde_json::Value>, tokio::time::error::Elapsed> =
        tokio::time::timeout(
            Duration::from_secs(probe_timeout_s),
            orchestrator
                .neural
                .generate_completion(provider, &model, PROBE_PROMPT),
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
            let ec_canon = error_class_from_completion(&response).to_string();
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
                tracing::warn!("provider_health_probe: {provider} errore provider (class={kind})");
                if matches!(
                    crate::model_health_probe::dispatch_probe_error(provider, &model, &ec, db)
                        .await,
                    crate::model_health_probe::ProbeDispatchOutcome::ProviderCooldown
                ) {
                    round_victims_push(provider);
                }
                let detail = if inner.is_empty() {
                    content_text.as_str()
                } else {
                    inner
                };
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
                //
                // «E' un cooldown LUNGO?» ha gia' un punto unico:
                // `is_provider_in_billing_cooldown`, che legge la severita'
                // REGISTRATA da chi il cooldown lo ha messo. Qui la domanda
                // veniva ri-posta cercando 7 sottostringhe inglesi nella
                // `reason` (regola M), su uno snapshot le cui chiavi potevano
                // essere composte — quindi per una coppia il `find` non trovava
                // nulla e il ramo degradava a "short" in silenzio.
                let is_in_long = crate::provider_cooldown::is_provider_in_billing_cooldown(provider);
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
                tracing::debug!("provider_health_probe: {provider} OK in {latency_ms}ms");
                (true, None, None)
            }
        }
        Ok(Err(e)) => {
            // Provider ha risposto con errore (HTTP error o JSON parse).
            // Classifico per decidere il tipo di cooldown.
            let msg = e.to_string();
            // Classificazione via il punto UNICO (brain gRPC); niente pattern locali.
            let ec = orchestrator.neural.classify_error(&msg, provider).await;
            let kind = kind_from_error_class(&ec);
            tracing::warn!(
                "provider_health_probe: {provider} ERROR ({kind}) in {latency_ms}ms: {msg}",
                msg = &msg[..msg.len().min(200)],
            );
            // Infrastruttura locale (brain bridge / rete): NON cooldown — il problema
            // e' locale, non del provider remoto.
            let is_local_infra = matches!(kind.as_str(), "connection_error")
                || msg.contains("tcp connect error")
                || msg.contains("Unavailable")
                || msg.contains("ECONNREFUSED");
            if is_local_infra {
                tracing::warn!(
                    "provider_health_probe: {provider} ERROR ma sembra problema di rete \
                     locale / brain bridge — NESSUN cooldown applicato"
                );
                round_victims_push(provider);
            } else if matches!(
                crate::model_health_probe::dispatch_probe_error(provider, &model, &ec, db).await,
                crate::model_health_probe::ProbeDispatchOutcome::ProviderCooldown
            ) {
                round_victims_push(provider);
            }
            (false, Some(kind), Some(truncate(&msg, 500)))
        }
        Err(_timeout_elapsed) => {
            tracing::warn!("provider_health_probe: {provider} TIMEOUT (>{probe_timeout_s}s)");
            if matches!(
                crate::model_health_probe::dispatch_probe_error(provider, &model, "timeout", db)
                    .await,
                crate::model_health_probe::ProbeDispatchOutcome::ProviderCooldown
            ) {
                round_victims_push(provider);
            }
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
            status: if healthy {
                "up".to_string()
            } else {
                "down".to_string()
            },
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

    // Su probe SANO azzera lo snapshot dell'ultimo errore in nexus_provider_health.
    // Senza questo `last_error` resta congelato all'ultimo fallimento (anche solo
    // transitorio, es. connessione morta post-sleep) e il banner mostra il provider
    // "down" pur essendo tornato sano: la scrittura di last_error (gateway cooldown.rs)
    // avviene solo su ERRORE, mai un azzeramento su successo. `billing_cooldown_until`
    // NON viene toccato (gestito dal recovery billing). `AND last_error IS NOT NULL`
    // evita UPDATE inutili quando non c'e' nulla da azzerare.
    if healthy {
        let cleared = sqlx::query(
            r#"UPDATE nexus_provider_health
               SET last_error = NULL, last_error_at = NULL, last_error_source = NULL, updated_at = now()
               WHERE provider = $1 AND last_error IS NOT NULL"#,
        )
        .bind(provider)
        .execute(db)
        .await;
        if let Err(e) = cleared {
            tracing::warn!("provider_health_probe: azzeramento last_error fallito per {provider}: {e}");
        }
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
    db: &PgPool,
    provider: &str,
    timeout_s: u64,
) -> ProbeOutcome {
    let matrix_arc = match orchestrator.routing_matrix.current_async().await {
        Ok(m) => m,
        Err(e) => return ProbeOutcome::Transient(format!("routing_matrix non disponibile: {e}")),
    };
    let Some(model) = modello_per_probe(db, &matrix_arc, provider)
        .await
        .risolvi(provider)
    else {
        return ProbeOutcome::Transient(format!(
            "nessun modello abilitato per '{provider}': niente da interrogare"
        ));
    };
    let result = tokio::time::timeout(
        Duration::from_secs(timeout_s.max(1)),
        orchestrator
            .neural
            .generate_completion(provider, &model, PROBE_PROMPT),
    )
    .await;

    match result {
        Ok(Ok(response)) => {
            // error_class CANONICO dal response: il brain riformatta l'errore in
            // un messaggio umano che NON inizia con "[Error:", quindi senza
            // questo check la recovery-loop credeva il provider sano e lo
            // riabilitava (bug: openai/anthropic senza credito tornavano attivi).
            let ec = error_class_from_completion(&response);
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
        Ok(Err(e)) => outcome_from_error_class(
            &orchestrator
                .neural
                .classify_error(&e.to_string(), provider)
                .await,
        ),
        Err(_elapsed) => ProbeOutcome::Transient("timeout".to_string()),
    }
}

/// La classe d'errore dichiarata nel `Value` di `generate_completion`, letta dai due
/// posti in cui quel produttore la scrive (top-level e sotto `metadata`).
///
/// Estratta come funzione per la regola O: e' il campo su cui `probe_provider_once`
/// decide se spegnere un fornitore, e un test che lo leggesse per conto proprio
/// misurerebbe la propria copia invece di questa. Chi la produce e' il punto unico
/// `neural_client::error_completion_from_error`, che dal 10/08/2026 la deriva dal
/// segnale STRUTTURATO del gateway e non piu' dalla prosa dell'errore.
fn error_class_from_completion(response: &serde_json::Value) -> &str {
    response
        .get("error_class")
        .and_then(|v| v.as_str())
        .or_else(|| {
            response
                .get("metadata")
                .and_then(|m| m.get("error_class"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
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
        // Il nome viene dal vocabolario CONDIVISO con l'altro scrittore di
        // `nexus_provider_health_history.error_kind`, il `CooldownManager` del
        // gateway: finche' erano due letterali indipendenti, lo stesso stato
        // usciva come `credit_balance_too_low` da qui e come `billing` da li'.
        "billing_error" => nexus_types::provider_failure::stato_salute::CREDIT_BALANCE_TOO_LOW
            .to_string(),
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
/// logica di `agent_turn_setup.rs::classify_provider_error`.
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

    /// IL CASO REALE MISURATO il 17/08/2026: il default del fornitore punta a un
    /// modello che il catalogo ha DISABILITATO (groq -> `llama-3.1-8b-instant`,
    /// squalificato dal 15/07 e rimosso dal fornitore; perplexity -> `sonar`).
    /// Il probe lo interrogava lo stesso, prendeva 404 `model_not_found`, e
    /// l'intero fornitore risultava `not_found` mentre il suo endpoint
    /// rispondeva 200 su un modello vivo.
    ///
    /// Qui il criterio si misura contro lo schema REALE (META_MIGRATOR) e la
    /// matrice REALE letta dal DB, non contro una mappa scritta a mano.
    ///
    /// MUTAZIONE: far ritornare a `modello_per_probe` il default senza guardare
    /// `is_enabled` (il comportamento di prima) fa cadere il primo assert con
    /// `Configurato("modello-morto")`.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_probe_non_interroga_un_modello_che_il_catalogo_ha_spento(pool: PgPool) {
        sqlx::query("DELETE FROM ai_price_catalog WHERE provider = 'prov-probe'")
            .execute(&pool)
            .await
            .expect("pulizia");
        for (model, enabled, costo) in [
            ("modello-morto", false, 0.01_f64),
            ("modello-vivo-caro", true, 5.0_f64),
            ("modello-vivo-economico", true, 0.20_f64),
        ] {
            sqlx::query(
                // `last_probe_healthy_at` valorizzato: il trigger
                // `enforce_probe_before_enable` (mig 0629) respinge a false
                // ogni riga abilitata senza un probe sano alle spalle.
                "INSERT INTO ai_price_catalog                  (provider, model, display_name, input_cost_per_million_tokens,                   output_cost_per_million_tokens, currency, is_enabled, pricing_state,                   last_probe_healthy_at)                  VALUES ('prov-probe', $1, $1, $2, $2, 'USD', $3, 'priced', now())",
            )
            .bind(model)
            .bind(costo)
            .bind(enabled)
            .execute(&pool)
            .await
            .expect("seed catalog");
        }
        sqlx::query(
            "INSERT INTO nexus_provider_default_model (provider, model_id, notes)              VALUES ('prov-probe', 'modello-morto', 'default che punta nel vuoto')              ON CONFLICT (provider) DO UPDATE SET model_id = EXCLUDED.model_id",
        )
        .execute(&pool)
        .await
        .expect("seed default");

        let matrix = crate::routing_matrix::fetch_from_db(&pool)
            .await
            .expect("matrice dal DB reale");

        // Il default e' spento: si sonda con un modello vivo, e la sostituzione
        // e' DICHIARATA (non un silenzioso ripiego).
        let scelta = modello_per_probe(&pool, &matrix, "prov-probe").await;
        assert_eq!(
            scelta,
            ModelloProbe::Sostituito {
                usato: "modello-vivo-economico".to_string(),
                configurato: "modello-morto".to_string(),
            },
            "col default spento si sonda il piu' economico fra gli abilitati"
        );

        // Riabilitato il default, torna a essere quello configurato.
        sqlx::query(
            "UPDATE ai_price_catalog SET is_enabled = true, last_probe_healthy_at = now()               WHERE provider = 'prov-probe' AND model = 'modello-morto'",
        )
        .execute(&pool)
        .await
        .expect("riabilito");
        assert_eq!(
            modello_per_probe(&pool, &matrix, "prov-probe").await,
            ModelloProbe::Configurato("modello-morto".to_string()),
            "se il catalogo lo abilita, il default configurato comanda"
        );

        // Nessun modello abilitato: non c'e' niente da interrogare, e non e'
        // un fornitore giu' (regola Q: l'ignoto e' una variante dichiarata).
        sqlx::query("UPDATE ai_price_catalog SET is_enabled = false WHERE provider = 'prov-probe'")
            .execute(&pool)
            .await
            .expect("spengo tutto");
        assert_eq!(
            modello_per_probe(&pool, &matrix, "prov-probe").await,
            ModelloProbe::NessunoAbilitato
        );
    }

    /// LA CONSEGUENZA, non la stringa (regola O): un 429 di groq per tetto giornaliero
    /// non deve produrre `ProbeOutcome::Billing`, che e' cio' che porta a
    /// `put_provider_in_long_cooldown` (SEI ORE) e toglie il fornitore dal routing.
    ///
    /// La catena e' attraversata per intero e per la stessa strada della produzione:
    /// l'errore nasce dal produttore vero (`GatewayHttpError::from_response`), il
    /// `Value` da `error_completion_from_error`, la lettura del campo da
    /// `error_class_from_completion` — le stesse tre funzioni che esegue
    /// `probe_provider_once`.
    ///
    /// MISURATO il 10/08/2026: groq in `billing_cooldown_until = 18:09:52` con
    /// "Quota AI esaurita o credito insufficiente" mentre alle 12:15 rispondeva 200 OK
    /// e la quota si era gia' riaperta.
    #[test]
    fn un_rate_limit_giornaliero_non_spegne_il_fornitore_per_credito() {
        let err: anyhow::Error = crate::nexus_gateway::GatewayHttpError::from_response(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({
                "error": "tutti i provider hanno fallito -> groq (groq HTTP 429: Rate limit \
                          reached for model `openai/gpt-oss-20b` service tier `on_demand` on \
                          tokens per day (TPD): Limit 200000, Used 199373. Need more tokens? \
                          Upgrade to Dev Tier today at https://console.groq.com/settings/billing)",
                "code": "PROVIDER_ERROR",
                "details": {
                    "primary_cause": "transient",
                    "failures": [{"provider": "groq", "class": "transient",
                                  "status": 429, "code": "rate_limit_exceeded"}]
                }
            })
            .to_string(),
        )
        .into();

        let response = crate::orchestrator::neural_client::valore_derrore_dal_gateway(
            "groq",
            "openai/gpt-oss-20b",
            &err,
            crate::orchestrator::neural_client::FormaErrore::Completion,
        );
        let esito = outcome_from_error_class(error_class_from_completion(&response));

        assert!(
            !matches!(esito, ProbeOutcome::Billing(_)),
            "un tetto di frequenza non e' credito esaurito: esito ottenuto {esito:?}"
        );
        assert!(
            matches!(esito, ProbeOutcome::Transient(ref k) if k == "transient"),
            "l'esito atteso e' transitorio (si riprova), non billing: {esito:?}"
        );
    }

    #[test]
    fn resolve_probed_providers_usa_il_catalog_quando_presente() {
        let from_db = vec!["openai".to_string(), "perplexity".to_string()];
        // La lista dal catalog vince: un provider nuovo (perplexity) e' incluso
        // senza toccare la costante di fallback.
        assert_eq!(
            resolve_probed_providers(from_db),
            vec!["openai".to_string(), "perplexity".to_string()]
        );
    }

    #[test]
    fn resolve_probed_providers_fallback_se_catalog_vuoto() {
        // Query fallita / catalog senza modelli abilitati -> fail-safe sui noti.
        let expected: Vec<String> = FALLBACK_PROBED_PROVIDERS
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(resolve_probed_providers(vec![]), expected);
    }

    #[test]
    fn kind_from_error_class_mappa_billing() {
        // Il valore atteso e' la COSTANTE condivisa, non una stringa ricopiata:
        // e' cosi' che questo scrittore e quello del gateway restano legati.
        assert_eq!(
            kind_from_error_class("billing_error"),
            nexus_types::provider_failure::stato_salute::CREDIT_BALANCE_TOO_LOW
        );
        assert_eq!(kind_from_error_class("auth_error"), "auth_error");
        assert_eq!(kind_from_error_class("rate_limit"), "rate_limit");
        // Regressione: 403 forbidden (es. Mistral labs_not_enabled) NON deve
        // collassare in auth_error, altrimenti finisce in long cooldown 6h.
        assert_eq!(kind_from_error_class("forbidden"), "forbidden");
    }

    #[test]
    fn outcome_from_error_class_billing_e_transient() {
        assert!(matches!(
            outcome_from_error_class("billing_error"),
            ProbeOutcome::Billing(_)
        ));
        assert!(matches!(
            outcome_from_error_class("rate_limit"),
            ProbeOutcome::Transient(_)
        ));
        // 401 auth_error: il provider e' inutilizzabile -> Billing (long cooldown).
        assert!(matches!(
            outcome_from_error_class("auth_error"),
            ProbeOutcome::Billing(_)
        ));
        // 403 forbidden: per-modello/per-risorsa -> Transient (short cooldown),
        // non spegne l'intero provider.
        assert!(matches!(
            outcome_from_error_class("forbidden"),
            ProbeOutcome::Transient(_)
        ));
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
