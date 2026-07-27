//! Enforcement quota e registrazione usage nel ledger.
//!
//! Convenzione Nexus: `tenant_id = project_id` (UUID).
//!
//! Regola L: il LISTINO (quanto costa un modello) vive nel punto unico
//! `nexus-pricing`, non qui. Questo modulo tiene solo cio' che e' suo: la POLICY
//! di quota e la scrittura del ledger. La differenza conta — la domanda "quanto
//! costa (provider, model)?" e' una sola, mentre "cosa faccio se non lo so"
//! dipende dal chiamante, e qui la risposta e' sempre "degrada e annota, mai
//! respingere la richiesta".
//!
//! NB storica: una versione precedente di questa doc indicava
//! `crates/billing-service` come "API interna autoritativa" verso cui far
//! convergere il gateway alla "Fase 6". Era la direzione sbagliata: quel crate e'
//! un fork divergente che non scrive alcuna riga di ledger e porta ancora i
//! difetti (default currency EUR, filtro `is_enabled` sulla contabilita') che
//! mcp-core aveva gia' corretto. La convergenza e' su `nexus-pricing`.
//!
//! Regola F: nessun prompt/response/segreto nei log; solo importi/conteggi.

use anyhow::Result;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use nexus_pricing::{
    calculate_cost, calculate_cost_breakdown, resolve_active_price_in, CostBreakdown, PriceLookup,
    TokenUsage, UsageUnit,
};

use crate::types::{LlmRequest, LlmResponse, MessageContent};

/// Riga di quota attiva (parita' con la query del server.ts).
#[derive(Debug, Clone, sqlx::FromRow)]
struct QuotaRow {
    scope_type: String,
    token_limit: Option<i64>,
    cost_limit: Option<f64>,
    valid_from: chrono::DateTime<chrono::Utc>,
    valid_to: chrono::DateTime<chrono::Utc>,
}

/// Quota superata: tradotta in HTTP 403 dal chiamante (come `QUOTA_EXCEEDED` -> 403).
#[derive(Debug, thiserror::Error)]
#[error("quota_exceeded:{scope}:{reason}")]
pub struct QuotaExceeded {
    pub scope: String,
    pub reason: String,
}

/// Listino di (provider, model) + currency di piattaforma, dal punto unico.
///
/// DEGRADO ESPLICITO (policy del gateway): se la currency non e' configurata o il
/// DB del listino non risponde, questa funzione NON propaga l'errore. Il motivo e'
/// che i suoi chiamanti stanno sul percorso della richiesta: `enforce_quota`
/// propaga con `?` e il suo errore diventa una richiesta RESPINTA. Far fallire una
/// chiamata LLM perche' non sappiamo prezzarla sostituirebbe una sottostima con un
/// outage — un prezzo troppo alto per un problema di contabilita'.
///
/// La visibilita' che la regola G esige non viene sacrificata: si ottiene ALL'AVVIO
/// con `nexus_pricing::assert_configured`, dove fallire e' gratuito, piu' il WARN
/// qui sotto e `details.price_state` sulla riga di ledger.
async fn lookup_price(db: &PgPool, provider: &str, model: &str) -> PriceLookup {
    let currency = match nexus_pricing::platform_currency(db).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "gateway-billing: currency di piattaforma non risolvibile -> costo non calcolabile \
                 (la richiesta prosegue: vedi assert_configured all'avvio)"
            );
            return PriceLookup::NotInCatalog;
        }
    };
    match resolve_active_price_in(db, provider, model, &currency).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, provider = %provider, model = %model,
                "gateway-billing: lettura listino fallita -> costo non calcolabile");
            PriceLookup::NotInCatalog
        }
    }
}

/// Stima i token di input dai messaggi (char/4, parita' col server.ts).
pub fn estimate_prompt_tokens(req: &LlmRequest) -> i64 {
    let chars: usize = req
        .messages
        .iter()
        .map(|m| match &m.content {
            MessageContent::Text(t) => t.len(),
            // I blocchi non-testo non contribuiscono alla stima char-based.
            MessageContent::Blocks(_) => 0,
        })
        .sum();
    // Ceil division per 4 (parita' con Math.ceil(chars/4) del server.ts).
    ((chars as i64) + 3) / 4
}

/// Come si stima il consumo ai fini della quota.
///
/// Esiste perche' la stima a token ha senso solo per le chiamate testuali. Sulle
/// modalita' media produce numeri senza rapporto col consumo: il prompt di
/// un'immagine diviso quattro, e per la trascrizione addirittura zero (il
/// messaggio user della richiesta sintetica e' vuoto). Finche' quelle chiamate
/// non avevano identita' la quota era comunque no-op e la cosa non si vedeva;
/// dal momento in cui l'identita' c'e', ereditare quella stima significherebbe
/// consumare la quota-token di un progetto con un numero inventato — e nel caso
/// peggiore rifiutare una richiesta per un motivo falso.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaEstimate {
    /// Chiamata testuale: token stimati da messaggi e `max_tokens`.
    Testuale,
    /// Modalita' non-testuale: nessun token stimato. Resta attiva la quota di
    /// COSTO, che diventa efficace appena `ai_price_catalog_unit` e' popolata.
    NonTestuale,
}

/// Enforce quota PRIMA della chiamata al provider (guardrail). Stima i token e il
/// costo, somma all'uso corrente del periodo e blocca se sfora un limite attivo.
/// No-op se mancano `tenant_id`/`user_id` o se non ci sono quote (parita' server.ts).
pub async fn enforce_quota(
    db: &PgPool,
    req: &LlmRequest,
    provider: &str,
    model: &str,
    stima: QuotaEstimate,
) -> Result<()> {
    let project_id = req.metadata.tenant_id.trim();
    let user_id = req.metadata.user_id.trim();
    if project_id.is_empty() || user_id.is_empty() {
        return Ok(());
    }
    let (Ok(project_uuid), Ok(user_uuid)) = (Uuid::parse_str(project_id), Uuid::parse_str(user_id))
    else {
        // Metadati non-UUID: il gateway non puo' applicare quote -> passa (no-op).
        return Ok(());
    };

    // Solo per il log della quota superata: se non e' risolvibile lo si dice,
    // non si inventa una valuta (regola G).
    let currency = nexus_pricing::platform_currency(db)
        .await
        .unwrap_or_else(|_| "currency non configurata".to_string());

    let (estimated_prompt, estimated_completion) = match stima {
        QuotaEstimate::Testuale => (
            estimate_prompt_tokens(req),
            req.max_tokens.map(|t| t as i64).unwrap_or(0),
        ),
        // Nessun token: queste chiamate non ne consumano, e fingere il contrario
        // eroderebbe la quota-token del progetto con un numero arbitrario.
        QuotaEstimate::NonTestuale => (0, 0),
    };
    let estimated_total = estimated_prompt + estimated_completion;

    // Stima per le quote: senza listino resta 0 (non si inventa un prezzo, e
    // rifiutare qui sarebbe un cambio di policy). Lo zero e' pero' dichiarato,
    // non implicito: `Unknown` viene loggato perche' una stima a 0 non consuma
    // quota di costo e lascia sforare in silenzio.
    let estimated_cost = match lookup_price(db, provider, model).await {
        PriceLookup::Priced(p) => calculate_cost(&p, estimated_prompt, estimated_completion).2,
        PriceLookup::Unknown => {
            tracing::warn!(
                provider = %provider,
                model = %model,
                "gateway-quota: prezzo IGNOTO (pricing_state='unknown') -> stima costo 0, \
                 la quota di costo non viene consumata per questa chiamata"
            );
            0.0
        }
        PriceLookup::NotInCatalog => 0.0,
    };

    let quotas = sqlx::query_as::<_, QuotaRow>(
        r#"
        SELECT scope_type, token_limit::bigint AS token_limit, cost_limit::float8 AS cost_limit,
               valid_from, valid_to
        FROM ai_quota_policies
        WHERE is_enabled = TRUE
          AND valid_from <= NOW()
          AND valid_to > NOW()
          AND (
            (scope_type = 'user' AND user_id = $1) OR
            (scope_type = 'project' AND project_id = $2) OR
            (scope_type = 'user_project' AND user_id = $1 AND project_id = $2)
          )
        ORDER BY scope_type ASC
        "#,
    )
    .bind(user_uuid)
    .bind(project_uuid)
    .fetch_all(db)
    .await?;

    for q in &quotas {
        let (used_tokens, used_cost) =
            usage_for_scope(db, &q.scope_type, user_uuid, project_uuid, q.valid_from, q.valid_to)
                .await?;

        if let Some(limit) = q.token_limit {
            if used_tokens + estimated_total > limit {
                return Err(anyhow::Error::new(QuotaExceeded {
                    scope: q.scope_type.clone(),
                    reason: "token_limit".to_string(),
                }));
            }
        }
        if let Some(limit) = q.cost_limit {
            if used_cost + estimated_cost > limit {
                tracing::warn!(
                    scope = %q.scope_type,
                    currency = %currency,
                    provider,
                    "gateway: quota costo superata"
                );
                return Err(anyhow::Error::new(QuotaExceeded {
                    scope: q.scope_type.clone(),
                    reason: "cost_limit".to_string(),
                }));
            }
        }
    }

    Ok(())
}

/// Uso corrente (token, costo) per uno scope nel periodo del vincolo, su ledger
/// `reserved`/`finalized`. Parita' con la query del server.ts.
async fn usage_for_scope(
    db: &PgPool,
    scope_type: &str,
    user_id: Uuid,
    project_id: Uuid,
    valid_from: chrono::DateTime<chrono::Utc>,
    valid_to: chrono::DateTime<chrono::Utc>,
) -> Result<(i64, f64)> {
    // La clausola scope discrimina i predicati: usiamo $1/$2 condizionati su scope.
    let row = sqlx::query_as::<_, (i64, f64)>(
        r#"
        SELECT
            COALESCE(SUM(total_tokens), 0)::bigint AS tokens,
            COALESCE(SUM(total_cost), 0)::float8 AS cost
        FROM ai_usage_ledger
        WHERE status IN ('reserved', 'finalized')
          AND created_at >= $4
          AND created_at <  $5
          AND (
            ($3 = 'user'         AND user_id = $1) OR
            ($3 = 'project'      AND project_id = $2) OR
            ($3 = 'user_project' AND user_id = $1 AND project_id = $2)
          )
        "#,
    )
    .bind(user_id)
    .bind(project_id)
    .bind(scope_type)
    .bind(valid_from)
    .bind(valid_to)
    .fetch_one(db)
    .await?;
    Ok(row)
}

/// La INSERT del percorso testuale, come costante e non inline.
///
/// Non e' cosmetica: l'elenco delle colonne scritto a mano dentro la funzione e'
/// il difetto che ha tenuto a zero le quattro colonne di cache su 7.405 righe.
/// Una colonna dimenticata non e' un errore di compilazione — il valore cade sul
/// DEFAULT e nessuno se ne accorge. Come costante, un test puo' confrontarla col
/// testo VERO della migrazione che quelle colonne le ha create (regola O).
/// `id` e' RESTITUITO dalla INSERT invece di essere letto dopo: e' l'unico modo
/// per dichiarare al chiamante QUALE riga porta l'addebito senza una seconda
/// query che potrebbe pescarne un'altra.
const SQL_INSERT_LEDGER_TESTO: &str = r#"
        INSERT INTO ai_usage_ledger (
            user_id, project_id, run_id, provider, model,
            prompt_tokens, completion_tokens, total_tokens,
            cache_read_tokens, cache_creation_tokens,
            input_cost, output_cost, cache_read_cost, cache_creation_cost, total_cost,
            currency, status, details
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, 'finalized', $17
        )
        RETURNING id
        "#;

/// Registra l'usage effettivo nel ledger come `finalized` (parita' con
/// `recordUsageToLedger`). No-op se mancano metadati o usage. Best-effort: gli
/// errori sono loggati ma non interrompono la risposta al chiamante.
///
/// RITORNA la riga scritta ([`LedgerEntry`]), oppure `None` quando non ne ha
/// scritta alcuna. Il valore di ritorno non e' telemetria: e' il segnale
/// STRUTTURATO (regola M) su cui il chiamante decide se addebitare a sua volta.
/// I casi di `None` sono reali e non deducibili dall'esito della chiamata LLM,
/// che e' RIUSCITA in tutti e tre:
///   - richiesta senza identita' (`tenant_id`/`user_id` vuoti): e' il caso di
///     `NeuralCoreClient::generate_completion`, che manda `GwMetadata::default`;
///   - identita' non-UUID;
///   - INSERT fallita (DB) — best-effort, ma il chiamante deve saperlo, o
///     rilasciando la propria prenotazione perderebbe del tutto l'addebito.
pub async fn record_usage_to_ledger(
    db: &PgPool,
    req: &LlmRequest,
    resp: &LlmResponse,
) -> Option<crate::types::LedgerEntry> {
    let project_id = req.metadata.tenant_id.trim();
    let user_id = req.metadata.user_id.trim();
    if project_id.is_empty() || user_id.is_empty() {
        return None;
    }
    let (Ok(project_uuid), Ok(user_uuid)) = (Uuid::parse_str(project_id), Uuid::parse_str(user_id))
    else {
        return None;
    };

    let provider = &resp.provider_used;
    let model = &resp.model_used;
    let tokens = token_usage_from(&resp.usage);

    let price = lookup_price(db, provider, model).await;
    // `price_state` e' il segnale STRUTTURATO del perche' di un costo: chi legge il
    // ledger distingue "0 perche' gratis" da "0 perche' non so quanto costa" senza
    // dedurlo dall'importo. `price_missing` resta per i lettori esistenti ed e'
    // `true` in ENTRAMBI i casi di costo non calcolabile.
    let price_state = price.state_label();
    let price_missing = price.is_missing();
    let (currency, costo) = match &price {
        PriceLookup::Priced(p) => (
            p.currency.trim().to_uppercase(),
            calculate_cost_breakdown(p, &tokens),
        ),
        _ => {
            if matches!(price, PriceLookup::Unknown) {
                tracing::warn!(
                    provider = %provider,
                    model = %model,
                    prompt_tokens = tokens.prompt_tokens,
                    completion_tokens = tokens.completion_tokens,
                    "gateway-ledger: prezzo IGNOTO (pricing_state='unknown') -> costo NON calcolabile, \
                     registro 0 esplicito. Il modello non dovrebbe essere routabile: vedi il ciclo \
                     reconcile_disable_price_unknown del catalog_sync"
                );
            }
            // Costo 0 -> la currency e' vacua, ma la colonna e' NOT NULL: si annota
            // quella di piattaforma. Se nemmeno quella e' leggibile il DB e' giu' e
            // l'INSERT qui sotto fallisce comunque, quindi la stringa vuota non
            // raggiunge una riga persistita.
            let cur = nexus_pricing::platform_currency(db).await.unwrap_or_default();
            (cur, costo_nullo())
        }
    };

    let details = json!({
        "request_id": req.metadata.request_id,
        "feature": req.metadata.feature,
        "price_missing": price_missing,
        "price_state": price_state,
        // Stato del listino di CACHE, separato da quello del listino base: un
        // `cache_read_cost` a zero puo' voler dire "nessun token da cache" o
        // "tariffa non a listino, token fatturati a prezzo pieno di input", e i
        // due casi non si distinguono dall'importo (regola M).
        "cache_price_state": costo.cache_price_state(),
    });

    // run_id (= request_id nei metadata): abilita il breakdown costo per run /
    // sessione (M71). NULL se il chiamante non lo passa o non e' un UUID valido.
    let run_uuid = Uuid::parse_str(req.metadata.request_id.trim()).ok();

    let res = sqlx::query_scalar::<_, Uuid>(SQL_INSERT_LEDGER_TESTO)
    .bind(user_uuid)
    .bind(project_uuid)
    .bind(run_uuid)
    .bind(provider)
    .bind(model)
    .bind(tokens.prompt_tokens)
    .bind(tokens.completion_tokens)
    .bind(tokens.total_tokens())
    .bind(tokens.cache_read_tokens)
    .bind(tokens.cache_creation_tokens)
    .bind(costo.input_cost)
    .bind(costo.output_cost)
    .bind(costo.cache_read_cost)
    .bind(costo.cache_creation_cost)
    .bind(costo.total_cost)
    .bind(&currency)
    .bind(details)
    .fetch_one(db)
    .await;

    match res {
        Ok(id) => Some(crate::types::LedgerEntry {
            id,
            total_cost: costo.total_cost,
            currency,
        }),
        Err(e) => {
            // Regola F: solo l'errore SQL, nessun payload.
            tracing::warn!(error = %e, "gateway-ledger: insert ledger fallita (best-effort)");
            None
        }
    }
}

/// Dalla `LlmUsage` (gia' normalizzata dall'adapter al prompt LORDO, vedi
/// `LlmUsage::normalized`) ai token che il listino sa tariffare.
///
/// Trasporto e basta: i due contratti hanno la stessa convenzione — prompt
/// lordo, cache come sottoinsieme — e lo scorporo lo fa `nexus-pricing` al
/// momento di moltiplicare per le tariffe.
///
/// PUNTO UNICO della conversione (regola L): e' l'unico passaggio fra il
/// contratto del gateway e quello di `nexus-pricing`. Prima le due quantita' di
/// cache si fermavano qui — venivano lette dagli adapter, propagate fino a
/// questa funzione e poi semplicemente non nominate nella INSERT, restando al
/// DEFAULT 0 su tutte le righe del ledger.
fn token_usage_from(usage: &crate::types::LlmUsage) -> TokenUsage {
    TokenUsage {
        prompt_tokens: usage.input_tokens as i64,
        completion_tokens: usage.output_tokens as i64,
        cache_read_tokens: usage.cache_read_tokens.unwrap_or(0) as i64,
        cache_creation_tokens: usage.cache_creation_tokens.unwrap_or(0) as i64,
    }
}

/// Costo non calcolabile: tutte le voci a zero, nessun ripiego da dichiarare.
fn costo_nullo() -> CostBreakdown {
    CostBreakdown {
        input_cost: 0.0,
        output_cost: 0.0,
        cache_read_cost: 0.0,
        cache_creation_cost: 0.0,
        total_cost: 0.0,
        cache_tokens_billed_as_input: 0,
    }
}

// ── Consumo delle modalita' non-testuali ──────────────────
//
// Image-gen, video-gen, trascrizione e sintesi vocale costano denaro reale e
// fino alla mig 0634 non producevano NESSUNA riga di ledger: quote e report
// sottostimavano in silenzio. Non era una dimenticanza — lo schema non aveva
// modo di dire "3 immagini" o "42 secondi", e i doc-comment dei 4 handler
// dichiaravano la scelta di non inventare un costo (regola G/H).
//
// Ora la quantita' si registra sempre; il COSTO si accende quando
// `ai_price_catalog_unit` viene popolata. Finche' e' vuota le righe portano 0
// con `price_state='not_in_catalog'`, che e' un'informazione, non una bugia.

/// Modalita' della chiamata. Finisce in `ai_usage_ledger.usage_kind`, che ha un
/// CHECK: le stringhe qui sotto devono combaciare con la mig 0634.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    /// Audio in ingresso: trascrizione.
    AudioIn,
    /// Audio in uscita: sintesi vocale.
    AudioOut,
}

impl MediaKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Video => "video",
            MediaKind::AudioIn => "audio_in",
            MediaKind::AudioOut => "audio_out",
        }
    }
}

/// Da dove viene la quantita' registrata.
///
/// Non e' un dettaglio: nessuno dei quattro provider dichiara oggi quanto ha
/// prodotto (OpenAI Images scarta l'usage, la trascrizione gira con
/// `response_format=json` che non porta la durata, il video non riporta i
/// secondi effettivi). Fatturare cio' che abbiamo CHIESTO e' accettabile, ma chi
/// legge la riga deve poter distinguere un dato misurato da una stima.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantitySource {
    /// Contata sulla risposta del provider.
    Provider,
    /// Dedotta da cio' che e' stato richiesto.
    Request,
    /// Non conoscibile.
    None,
}

impl QuantitySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            QuantitySource::Provider => "provider",
            QuantitySource::Request => "request",
            QuantitySource::None => "none",
        }
    }
}

/// Quanto e' stato consumato da una chiamata non-testuale.
#[derive(Debug, Clone)]
pub struct MediaUsage {
    pub kind: MediaKind,
    /// `None` quando la quantita' non e' conoscibile: il CHECK
    /// `chk_ledger_quantity_coerente` impone che allora `source` sia `None`, cosi'
    /// "non lo so" non puo' travestirsi da zero.
    pub quantity: Option<f64>,
    pub unit: UsageUnit,
    pub source: QuantitySource,
}

impl MediaUsage {
    /// Quantita' contata sulla risposta del provider.
    pub fn misurata(kind: MediaKind, unit: UsageUnit, quantity: f64) -> Self {
        Self { kind, quantity: Some(quantity), unit, source: QuantitySource::Provider }
    }

    /// Quantita' dedotta dalla richiesta (il provider non la dichiara).
    pub fn da_richiesta(kind: MediaKind, unit: UsageUnit, quantity: f64) -> Self {
        Self { kind, quantity: Some(quantity), unit, source: QuantitySource::Request }
    }

    /// Consumo avvenuto ma non quantificabile: la riga si scrive lo stesso
    /// (chi, cosa, quale modello), senza inventare un numero.
    pub fn non_quantificabile(kind: MediaKind, unit: UsageUnit) -> Self {
        Self { kind, quantity: None, unit, source: QuantitySource::None }
    }
}

/// `(project_id, user_id)` dai metadata, o `None` se non sono utilizzabili.
///
/// Stessa guard del percorso testuale, ma qui il fallimento si DICE: quando
/// scatta, un consumo reale resta fuori dalla contabilita', e il silenzio e'
/// esattamente il motivo per cui il buco e' rimasto aperto tanto a lungo.
fn identita_del_chiamante(req: &LlmRequest, kind: MediaKind) -> Option<(Uuid, Uuid)> {
    let project_id = req.metadata.tenant_id.trim();
    let user_id = req.metadata.user_id.trim();
    if project_id.is_empty() || user_id.is_empty() {
        tracing::warn!(
            kind = kind.as_str(),
            "gateway-ledger: consumo media NON registrato, identita' assente nei metadata"
        );
        return None;
    }
    match (Uuid::parse_str(project_id), Uuid::parse_str(user_id)) {
        (Ok(p), Ok(u)) => Some((p, u)),
        _ => {
            tracing::warn!(
                kind = kind.as_str(),
                "gateway-ledger: consumo media NON registrato, identita' non e' un UUID"
            );
            None
        }
    }
}

/// Costo del consumo e stato del listino, come coppia `(costo, price_state)`.
///
/// Il costo esiste solo se esistono ENTRAMBI: un listino per quell'unita' E una
/// quantita' da moltiplicare. Mancando l'uno o l'altra il risultato e' 0
/// DICHIARATO via `price_state`, mai dedotto — chi legge la riga distingue
/// "gratis" da "non so quanto costa" senza guardare l'importo.
async fn prezza_consumo(
    db: &PgPool,
    provider: &str,
    model: &str,
    usage: &MediaUsage,
    currency: &str,
) -> (f64, &'static str) {
    let price = match nexus_pricing::resolve_unit_price(db, provider, model, usage.unit, currency)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "gateway-ledger: lookup listino per-unita' fallito");
            nexus_pricing::UnitPriceLookup::NotInCatalog
        }
    };
    let costo = usage.quantity.and_then(|q| price.cost_for(q)).unwrap_or(0.0);
    (costo, price.state_label())
}

/// Registra il consumo di una chiamata non-testuale. Gemella di
/// [`record_usage_to_ledger`] e con le stesse regole: no-op senza identita',
/// best-effort, `status='finalized'`.
///
/// PUNTO UNICO (regola L): i quattro handler media sono copie parallele, e
/// scrivere il ledger in ognuno avrebbe creato la quinta copia. Qui la logica
/// sta una volta sola e i chiamanti dichiarano soltanto cosa hanno consumato.
pub async fn record_media_usage_to_ledger(
    db: &PgPool,
    req: &LlmRequest,
    provider_used: &str,
    model_used: &str,
    usage: MediaUsage,
) {
    let Some((project_uuid, user_uuid)) = identita_del_chiamante(req, usage.kind) else {
        return;
    };

    // Currency prima del prezzo: serve comunque, perche' la colonna e' NOT NULL
    // senza default (un default hardcoded qui e' gia' costato 3.993 righe orfane
    // prima della mig 0294).
    let currency = match nexus_pricing::platform_currency(db).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "gateway-ledger: currency di piattaforma illeggibile, riga media non scritta");
            return;
        }
    };

    let (total_cost, price_state) =
        prezza_consumo(db, provider_used, model_used, &usage, &currency).await;

    let details = json!({
        "request_id": req.metadata.request_id,
        "feature": req.metadata.feature,
        "price_missing": total_cost == 0.0,
        "price_state": price_state,
    });

    let run_uuid = Uuid::parse_str(req.metadata.request_id.trim()).ok();

    let riga = RigaMedia {
        user_uuid,
        project_uuid,
        run_uuid,
        provider: provider_used,
        model: model_used,
        total_cost,
        currency: &currency,
        details,
        usage: &usage,
    };
    if let Err(e) = inserisci_riga_media(db, riga).await {
        tracing::warn!(error = %e, "gateway-ledger: insert ledger media fallita (best-effort)");
    }
}

/// Campi di una riga di consumo media, raggruppati per non passare dodici
/// argomenti sciolti.
struct RigaMedia<'a> {
    user_uuid: Uuid,
    project_uuid: Uuid,
    run_uuid: Option<Uuid>,
    provider: &'a str,
    model: &'a str,
    total_cost: f64,
    currency: &'a str,
    details: serde_json::Value,
    usage: &'a MediaUsage,
}

/// L'INSERT vero e proprio.
///
/// I costi per-unita' finiscono in `total_cost`: non sono ne' input ne' output, e
/// spalmarli su una delle due colonne token-oriented direbbe una cosa falsa a chi
/// le legge. `input_cost`/`output_cost` e i token restano 0 (default di colonna):
/// per queste righe il consumo vive in `quantity`.
async fn inserisci_riga_media(db: &PgPool, r: RigaMedia<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO ai_usage_ledger (
            user_id, project_id, run_id, provider, model,
            total_cost, currency, status, details,
            usage_kind, quantity, quantity_unit, quantity_source
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, 'finalized', $8, $9, $10, $11, $12
        )
        "#,
    )
    .bind(r.user_uuid)
    .bind(r.project_uuid)
    .bind(r.run_uuid)
    .bind(r.provider)
    .bind(r.model)
    .bind(r.total_cost)
    .bind(r.currency)
    .bind(r.details)
    .bind(r.usage.kind.as_str())
    .bind(r.usage.quantity)
    .bind(r.usage.quantity.map(|_| r.usage.unit.as_str()))
    .bind(r.usage.source.as_str())
    .execute(db)
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LlmMessage, RequestMetadata};
    // `Row::get` per rileggere le colonne del ledger nel test della giuntura.
    use sqlx::Row;

    // ── Vocabolario del consumo media ──────────────────
    //
    // Le stringhe di `MediaKind`, `QuantitySource` e `UsageUnit` finiscono in
    // colonne con un CHECK: se divergono dalla migrazione, l'INSERT fallisce a
    // RUNTIME e il consumo torna invisibile — cioe' il difetto che questo lavoro
    // ha appena chiuso, di nuovo, senza che nessun test se ne accorga.
    //
    // Il testo della migrazione e' incluso a compile-time dal file VERO applicato
    // al database (regola O): non e' una copia delle costanti riscritta nel test,
    // che direbbe soltanto che il codice e' uguale a se stesso.
    const MIGRAZIONE_0634: &str =
        include_str!("../../../../db/migrations/0634_media_usage_units.sql");

    /// Ogni `usage_kind` che il codice sa produrre deve essere ammesso dal CHECK.
    #[test]
    fn i_kind_del_codice_sono_ammessi_dalla_migrazione() {
        for kind in [
            MediaKind::Image,
            MediaKind::Video,
            MediaKind::AudioIn,
            MediaKind::AudioOut,
        ] {
            let atteso = format!("'{}'", kind.as_str());
            assert!(
                MIGRAZIONE_0634.contains(&atteso),
                "usage_kind {atteso} non compare nella migrazione 0634: l'INSERT fallirebbe sul CHECK"
            );
        }
    }

    /// Idem per la provenienza della quantita'.
    #[test]
    fn le_fonti_della_quantita_sono_ammesse_dalla_migrazione() {
        for source in [
            QuantitySource::Provider,
            QuantitySource::Request,
            QuantitySource::None,
        ] {
            let atteso = format!("'{}'", source.as_str());
            assert!(
                MIGRAZIONE_0634.contains(&atteso),
                "quantity_source {atteso} non compare nella migrazione 0634"
            );
        }
    }

    /// E per le unita', che vivono nel crate pricing ma finiscono in due CHECK
    /// (ledger e listino unitario).
    #[test]
    fn le_unita_sono_ammesse_dalla_migrazione() {
        for unit in [UsageUnit::Image, UsageUnit::Second, UsageUnit::Character] {
            let atteso = format!("'{}'", unit.as_str());
            assert!(
                MIGRAZIONE_0634.contains(&atteso),
                "quantity_unit {atteso} non compare nella migrazione 0634"
            );
        }
    }

    /// "Non lo so" e "zero" restano distinguibili: il costruttore per il
    /// consumo non quantificabile non deve produrre una quantita'.
    #[test]
    fn non_quantificabile_non_inventa_uno_zero() {
        let u = MediaUsage::non_quantificabile(MediaKind::AudioIn, UsageUnit::Second);
        assert_eq!(u.quantity, None);
        assert_eq!(u.source, QuantitySource::None);
        // E' la coppia che il CHECK chk_ledger_quantity_coerente impone:
        // (quantity_source = 'none') = (quantity IS NULL).
        let misurata = MediaUsage::misurata(MediaKind::Image, UsageUnit::Image, 3.0);
        assert_eq!(misurata.quantity, Some(3.0));
        assert_ne!(misurata.source, QuantitySource::None);
        let stimata = MediaUsage::da_richiesta(MediaKind::Video, UsageUnit::Second, 8.0);
        assert_eq!(stimata.source, QuantitySource::Request);
        assert!(stimata.quantity.is_some());
    }

    fn req(messages: Vec<&str>, max_tokens: Option<u32>) -> LlmRequest {
        LlmRequest {
            model: "m".into(),
            messages: messages
                .into_iter()
                .map(|t| LlmMessage {
                    role: "user".into(),
                    content: MessageContent::Text(t.into()),
                    tool_call_id: None,
                    tool_calls: None,
                    name: None,
                    thinking_signature: None,
                    reasoning: None,
                })
                .collect(),
            temperature: None,
            max_tokens,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: RequestMetadata {
                tenant_id: "t".into(),
                user_id: "u".into(),
                request_id: "r".into(),
                sensitivity_tier: 0,
                feature: "chat".into(),
            },
            run_timeout_secs: None,
        }
    }

    #[test]
    fn stima_token_char_su_4_arrotonda_per_eccesso() {
        // 9 char -> ceil(9/4) = 3.
        assert_eq!(estimate_prompt_tokens(&req(vec!["123456789"], None)), 3);
        // 8 char -> 2; somma su piu' messaggi.
        assert_eq!(
            estimate_prompt_tokens(&req(vec!["1234", "5678"], None)),
            2
        );
        // vuoto -> 0.
        assert_eq!(estimate_prompt_tokens(&req(vec![""], None)), 0);
    }

    // NB: il test `calculate_cost_scala_per_milione` (e il clamp dei token
    // negativi) vive ora accanto alla funzione, in `nexus-pricing`. Riprodurlo qui
    // testerebbe una funzione che questo crate non possiede piu'.

    #[test]
    fn quota_exceeded_display() {
        let e = QuotaExceeded {
            scope: "user".into(),
            reason: "token_limit".into(),
        };
        assert_eq!(e.to_string(), "quota_exceeded:user:token_limit");
    }

    // ── Il ledger e le colonne di cache ────────────────────
    //
    // La riga di ledger la scrive una INSERT con l'elenco delle colonne a mano:
    // una colonna omessa cade sul DEFAULT, e nessun compilatore la reclama. E'
    // esattamente cosi' che `cache_read_tokens` e `cache_creation_tokens` sono
    // rimaste a zero su 7.405 chiamate mentre gli adapter le leggevano.
    //
    // Il confronto e' col testo VERO della migrazione applicata al database
    // (regola O), non con una lista ricopiata nel test.
    const MIGRAZIONE_0129: &str =
        include_str!("../../../../db/migrations/0129_ledger_cache_columns.sql");

    /// Ogni colonna che la mig 0129 ha aggiunto al ledger deve comparire nella
    /// INSERT che il gateway esegue davvero.
    #[test]
    fn la_insert_del_ledger_nomina_le_colonne_di_cache() {
        for colonna in [
            "cache_read_tokens",
            "cache_creation_tokens",
            "cache_read_cost",
            "cache_creation_cost",
        ] {
            assert!(
                MIGRAZIONE_0129.contains(colonna),
                "la migrazione 0129 non crea {colonna}: il test guarda il file sbagliato"
            );
            assert!(
                SQL_INSERT_LEDGER_TESTO.contains(colonna),
                "la INSERT del ledger non elenca {colonna}: la colonna resterebbe al DEFAULT 0"
            );
        }
        // Un segnaposto per ogni valore bindato: 17 bind, 17 placeholder. Uno
        // scarto qui e' un errore SQL a runtime, best-effort e quindi solo
        // loggato — cioe' invisibile.
        for n in 1..=17 {
            assert!(
                SQL_INSERT_LEDGER_TESTO.contains(&format!("${n}")),
                "placeholder ${n} assente dalla INSERT"
            );
        }
        assert!(
            !SQL_INSERT_LEDGER_TESTO.contains("$18"),
            "placeholder di troppo rispetto ai bind"
        );
    }

    /// Dalla `LlmUsage` che gli adapter producono alla riga che il ledger
    /// scrive: i numeri che finiscono nelle colonne, non una struct fabbricata.
    #[test]
    fn i_token_di_cache_arrivano_alla_riga_di_ledger() {
        // Il valore parte dal suo PRODUTTORE (`LlmUsage::normalized`, lo stesso
        // che chiamano gli adapter), non da un letterale scritto qui.
        let anthropic = crate::types::LlmUsage::normalized(
            crate::types::PromptCacheReporting::CachedReportedSeparately,
            100,
            20,
            Some(900),
            Some(50),
        );
        let t = token_usage_from(&anthropic);
        assert_eq!(t.prompt_tokens, 1_050, "il wire era al netto: 100+900+50");
        assert_eq!(t.cache_read_tokens, 900);
        assert_eq!(t.cache_creation_tokens, 50);
        // `total_tokens` e' prompt lordo + completion: la cache e' gia' dentro.
        assert_eq!(t.total_tokens(), 1_070);

        let openai = crate::types::LlmUsage::normalized(
            crate::types::PromptCacheReporting::CachedIncludedInPrompt,
            1_000,
            20,
            Some(900),
            None,
        );
        let t = token_usage_from(&openai);
        assert_eq!(t.prompt_tokens, 1_000, "il wire era gia' lordo");
        assert_eq!(t.cache_read_tokens, 900);
        // Il totale resta 1.020 come prima di questo lavoro: la serie storica
        // di quote e report non ha un gradino al deploy.
        assert_eq!(t.total_tokens(), 1_020);
    }

    /// La CONSEGUENZA sul costo, sulla stessa strada del ledger: con la tariffa
    /// di cache a listino, i token cached non pagano il prezzo pieno di input.
    #[test]
    fn il_costo_della_riga_scorpora_la_cache() {
        // Listino Anthropic tipico (mig 0403: read 0.1x, creation 1.25x).
        let price = nexus_pricing::PriceSnapshot {
            input_cost_per_million_tokens: 3.0,
            output_cost_per_million_tokens: 15.0,
            cache_read_cost_per_million_tokens: Some(0.3),
            cache_creation_cost_per_million_tokens: Some(3.75),
            currency: "USD".into(),
        };
        let usage = crate::types::LlmUsage::normalized(
            crate::types::PromptCacheReporting::CachedIncludedInPrompt,
            1_000_000,
            0,
            Some(900_000),
            None,
        );
        let tokens = token_usage_from(&usage);
        let costo = calculate_cost_breakdown(&price, &tokens);

        // Scorporato: 100k a 3.0 + 900k a 0.3 = 0.30 + 0.27 = 0.57.
        assert!((costo.total_cost - 0.57).abs() < 1e-9, "totale {}", costo.total_cost);
        assert!((costo.cache_read_cost - 0.27).abs() < 1e-9);
        assert_eq!(costo.cache_price_state(), "priced");

        // A tariffa piena (la formula di prima) 1M di token costerebbe 3.0: il
        // test rosseggia se lo scorporo sparisce.
        let a_tariffa_piena = calculate_cost(&price, tokens.prompt_tokens, 0).2;
        assert!((a_tariffa_piena - 3.0).abs() < 1e-9);
        assert!(costo.total_cost < a_tariffa_piena);

        // Senza tariffa a listino il costo torna ESATTAMENTE quello di prima —
        // mai peggiore — e il ripiego e' dichiarato.
        let senza_listino_cache = nexus_pricing::PriceSnapshot {
            cache_read_cost_per_million_tokens: None,
            cache_creation_cost_per_million_tokens: None,
            ..price.clone()
        };
        let ripiego = calculate_cost_breakdown(&senza_listino_cache, &tokens);
        assert!((ripiego.total_cost - a_tariffa_piena).abs() < 1e-12);
        assert_eq!(ripiego.cache_price_state(), "cache_price_missing");
    }

    // ── La GIUNTURA: dai numeri alle COLONNE ───────────────────
    //
    // I tre test qui sopra coprono i PEZZI: la normalizzazione, la conversione
    // verso il listino, lo scorporo del costo. Nessuno dimostra la giuntura, cioe'
    // che il valore bindato in posizione N finisca nella colonna che in posizione
    // N e' dichiarata. Il conteggio dei segnaposto ($1..$17) non lo dimostra: con
    // quattro conteggi e cinque importi adiacenti e omogenei, uno scambio di
    // posizione non e' un errore che il compilatore veda, non e' un errore SQL, e
    // si paga in denaro.
    //
    // L'unico modo di dimostrarlo e' percorrere la strada della produzione fino in
    // fondo e RILEGGERE la riga dal database vero, sullo schema reale applicato dal
    // META_MIGRATOR (regola O).

    /// Identita' che le FK del ledger esigono (`users`, `projects`) piu' il
    /// `run_id`, che il gateway deriva dal `request_id`: quello non ha piu' una FK
    /// — la mig 0276 l'ha tolta perche' i run agentici non stanno in
    /// `orchestrator_runs` — quindi e' un UUID libero.
    ///
    /// Il seeding delle due identita' viene dal punto unico
    /// [`nexus_test_schema::seed_identita_meta`]: la gemella di questo test in
    /// `mcp_core::billing` semina le stesse tabelle, e due copie a mano
    /// divergerebbero alla prima colonna NOT NULL aggiunta.
    async fn seed_identita(pool: &PgPool) -> (Uuid, Uuid, Uuid) {
        let (user, project) = nexus_test_schema::seed_identita_meta(pool).await;
        (user, project, Uuid::new_v4())
    }

    /// Listino con ENTRAMBE le tariffe di cache valorizzate (forma della mig 0403)
    /// e le quattro tariffe DISTINTE: e' cio' che rende osservabile uno scambio.
    async fn seed_listino(pool: &PgPool) {
        sqlx::query(
            "INSERT INTO ai_price_catalog ( \
                 provider, model, \
                 input_cost_per_million_tokens, output_cost_per_million_tokens, \
                 cache_read_cost_per_million_tokens, cache_creation_cost_per_million_tokens, \
                 currency, pricing_state \
             ) VALUES ('anthropic', 'claude-x', 3.0, 15.0, 0.3, 3.75, 'USD', 'priced')",
        )
        .execute(pool)
        .await
        .expect("seed ai_price_catalog");
    }

    /// Richiesta con l'identita' che il gateway richiede per contabilizzare.
    /// Riusa il costruttore dei test esistenti invece di ricopiarlo.
    fn req_con_identita(project: Uuid, user: Uuid, run: Uuid) -> LlmRequest {
        let mut r = req(vec!["ciao"], Some(64));
        r.metadata.tenant_id = project.to_string();
        r.metadata.user_id = user.to_string();
        r.metadata.request_id = run.to_string();
        r
    }

    /// Risposta come la costruisce un adapter: l'usage nasce dal suo PRODUTTORE
    /// (`LlmUsage::normalized`), che e' l'unico posto dove si decide se i token di
    /// cache vanno sommati al prompt per arrivare al lordo.
    fn resp_anthropic_con_cache() -> LlmResponse {
        LlmResponse {
            content: "ok".to_string(),
            tool_calls: None,
            usage: crate::types::LlmUsage::normalized(
                crate::types::PromptCacheReporting::CachedReportedSeparately,
                1_000_000,
                400_000,
                Some(2_000_000),
                Some(500_000),
            ),
            model_used: "claude-x".to_string(),
            provider_used: "anthropic".to_string(),
            latency_ms: 7,
            finish_reason: "stop".to_string(),
            privacy_rerouted: None,
            reasoning: None,
            thinking_signature: None,
            citations: None,
            ledger_entry: None,
        }
    }

    /// I dodici numeri della riga, ognuno nella sua colonna. Scelti distinti a due
    /// a due proprio perche' uno scambio non possa passare inosservato.
    #[sqlx::test(migrator = "nexus_test_schema::META_MIGRATOR")]
    async fn record_usage_scrive_ogni_numero_nella_sua_colonna(pool: PgPool) {
        let (user, project, run) = seed_identita(&pool).await;
        seed_listino(&pool).await;

        let dichiarata = record_usage_to_ledger(
            &pool,
            &req_con_identita(project, user, run),
            &resp_anthropic_con_cache(),
        )
        .await;

        let riga = sqlx::query(
            "SELECT id, user_id, project_id, provider, model, status, currency, details, \
                    prompt_tokens, completion_tokens, total_tokens, \
                    cache_read_tokens, cache_creation_tokens, \
                    input_cost::float8          AS input_cost, \
                    output_cost::float8         AS output_cost, \
                    cache_read_cost::float8     AS cache_read_cost, \
                    cache_creation_cost::float8 AS cache_creation_cost, \
                    total_cost::float8          AS total_cost \
               FROM ai_usage_ledger WHERE run_id = $1",
        )
        .bind(run)
        .fetch_one(&pool)
        .await
        .expect("la riga di ledger deve esistere: l'insert e' best-effort e un errore \
                 SQL qui sarebbe solo loggato, cioe' invisibile");

        // Identita' e chiavi testuali: anche queste sono bind posizionali.
        assert_eq!(riga.get::<Uuid, _>("user_id"), user);
        assert_eq!(riga.get::<Uuid, _>("project_id"), project);
        assert_eq!(riga.get::<String, _>("provider"), "anthropic");
        assert_eq!(riga.get::<String, _>("model"), "claude-x");
        assert_eq!(riga.get::<String, _>("status"), "finalized");
        assert_eq!(riga.get::<String, _>("currency"), "USD");

        // I quattro CONTEGGI. `prompt_tokens` e' il LORDO (il wire Anthropic era
        // al netto: 1M + 2M + 0.5M) e i due conteggi di cache ne sono il
        // dettaglio; il totale e' prompt lordo + completion.
        assert_eq!(riga.get::<i32, _>("prompt_tokens"), 3_500_000);
        assert_eq!(riga.get::<i32, _>("completion_tokens"), 400_000);
        assert_eq!(riga.get::<i64, _>("cache_read_tokens"), 2_000_000);
        assert_eq!(riga.get::<i64, _>("cache_creation_tokens"), 500_000);
        assert_eq!(riga.get::<i32, _>("total_tokens"), 3_900_000);

        // I cinque IMPORTI, alle quattro tariffe distinte del listino:
        // 1M x 3.0, 0.4M x 15.0, 2M x 0.3, 0.5M x 3.75. Il messaggio riporta il
        // valore LETTO: su uno scambio di bind e' quello che dice quale colonna ha
        // preso il posto di quale.
        for (colonna, atteso) in [
            ("input_cost", 3.0),
            ("output_cost", 6.0),
            ("cache_read_cost", 0.6),
            ("cache_creation_cost", 1.875),
            ("total_cost", 11.475),
        ] {
            let letto: f64 = riga.get(colonna);
            assert!(
                (letto - atteso).abs() < 1e-9,
                "{colonna}: letto {letto}, atteso {atteso}"
            );
        }

        // E lo stato del listino, che e' il segnale con cui si distingue uno zero
        // "gratis" da uno zero "non so quanto costa" (regola M).
        let details: serde_json::Value = riga.get("details");
        assert_eq!(details["price_state"], "priced");
        assert_eq!(details["price_missing"], false);
        assert_eq!(details["cache_price_state"], "priced");

        // E cio' che il gateway DICHIARA di aver scritto e' la riga che ha
        // scritto davvero. Non e' una formalita': su questa dichiarazione il
        // chiamante che ha prenotato decide di NON addebitare una seconda volta
        // (mcp_core::billing::settle_usage). Se l'id o l'importo dichiarati non
        // fossero quelli della riga, la correlazione punterebbe altrove e il
        // costo mostrato all'utente divergerebbe dal ledger.
        let entry = dichiarata.expect("il gateway ha scritto: deve dichiarare la riga");
        assert_eq!(entry.id, riga.get::<Uuid, _>("id"));
        assert_eq!(entry.currency, riga.get::<String, _>("currency"));
        assert!(
            (entry.total_cost - riga.get::<f64, _>("total_cost")).abs() < 1e-9,
            "costo dichiarato {} != costo scritto {}",
            entry.total_cost,
            riga.get::<f64, _>("total_cost")
        );
    }

    /// La domanda che decide se rilasciare una prenotazione e' "il gateway ha
    /// scritto?", e la risposta NON e' "la chiamata e' riuscita".
    ///
    /// Qui la chiamata riesce (la `LlmResponse` e' identica al test sopra) ma i
    /// metadata non portano identita': e' esattamente cio' che manda
    /// `NeuralCoreClient::generate_completion` (`GwMetadata::default`). Il
    /// gateway non scrive nulla, e deve DIRLO: un chiamante che rilasciasse la
    /// prenotazione fidandosi dell'esito perderebbe l'intero addebito.
    ///
    /// MUTAZIONE: facendo dichiarare una riga fabbricata prima della guardia
    /// d'identita', questo test e il suo gemello qui sopra rosseggiano entrambi
    /// — verificato: qui zero righe scritte a fronte di una dichiarata, li'
    /// l'id dichiarato non e' quello della riga.
    #[sqlx::test(migrator = "nexus_test_schema::META_MIGRATOR")]
    async fn senza_identita_il_gateway_non_scrive_e_lo_dichiara(pool: PgPool) {
        seed_listino(&pool).await;

        // `req` senza tenant_id/user_id: la forma di default dei metadata.
        let dichiarata =
            record_usage_to_ledger(&pool, &req(vec!["ciao"], Some(64)), &resp_anthropic_con_cache())
                .await;

        assert!(
            dichiarata.is_none(),
            "senza identita' il gateway non scrive: dichiarare una riga inesistente \
             autorizzerebbe il chiamante a rilasciare la prenotazione, perdendo l'addebito"
        );
        let righe: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ai_usage_ledger")
            .fetch_one(&pool)
            .await
            .expect("conteggio ledger");
        assert_eq!(righe, 0, "nessuna riga doveva essere scritta");
    }
}
