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

use nexus_pricing::{calculate_cost, resolve_active_price_in, PriceLookup, UsageUnit};

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

/// Registra l'usage effettivo nel ledger come `finalized` (parita' con
/// `recordUsageToLedger`). No-op se mancano metadati o usage. Best-effort: gli
/// errori sono loggati ma non interrompono la risposta al chiamante.
pub async fn record_usage_to_ledger(db: &PgPool, req: &LlmRequest, resp: &LlmResponse) {
    let project_id = req.metadata.tenant_id.trim();
    let user_id = req.metadata.user_id.trim();
    if project_id.is_empty() || user_id.is_empty() {
        return;
    }
    let (Ok(project_uuid), Ok(user_uuid)) = (Uuid::parse_str(project_id), Uuid::parse_str(user_id))
    else {
        return;
    };

    let provider = &resp.provider_used;
    let model = &resp.model_used;
    let prompt_tokens = resp.usage.input_tokens as i64;
    let completion_tokens = resp.usage.output_tokens as i64;
    let total_tokens = prompt_tokens + completion_tokens;

    let price = lookup_price(db, provider, model).await;
    // `price_state` e' il segnale STRUTTURATO del perche' di un costo: chi legge il
    // ledger distingue "0 perche' gratis" da "0 perche' non so quanto costa" senza
    // dedurlo dall'importo. `price_missing` resta per i lettori esistenti ed e'
    // `true` in ENTRAMBI i casi di costo non calcolabile.
    let price_state = price.state_label();
    let price_missing = price.is_missing();
    let (currency, input_cost, output_cost, total_cost) = match &price {
        PriceLookup::Priced(p) => {
            let (i, o, t) = calculate_cost(p, prompt_tokens, completion_tokens);
            (p.currency.trim().to_uppercase(), i, o, t)
        }
        _ => {
            if matches!(price, PriceLookup::Unknown) {
                tracing::warn!(
                    provider = %provider,
                    model = %model,
                    prompt_tokens,
                    completion_tokens,
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
            (cur, 0.0, 0.0, 0.0)
        }
    };

    let details = json!({
        "request_id": req.metadata.request_id,
        "feature": req.metadata.feature,
        "price_missing": price_missing,
        "price_state": price_state,
    });

    // run_id (= request_id nei metadata): abilita il breakdown costo per run /
    // sessione (M71). NULL se il chiamante non lo passa o non e' un UUID valido.
    let run_uuid = Uuid::parse_str(req.metadata.request_id.trim()).ok();

    let res = sqlx::query(
        r#"
        INSERT INTO ai_usage_ledger (
            user_id, project_id, run_id, provider, model,
            prompt_tokens, completion_tokens, total_tokens,
            input_cost, output_cost, total_cost,
            currency, status, details
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'finalized', $13
        )
        "#,
    )
    .bind(user_uuid)
    .bind(project_uuid)
    .bind(run_uuid)
    .bind(provider)
    .bind(model)
    .bind(prompt_tokens)
    .bind(completion_tokens)
    .bind(total_tokens)
    .bind(input_cost)
    .bind(output_cost)
    .bind(total_cost)
    .bind(currency)
    .bind(details)
    .execute(db)
    .await;

    if let Err(e) = res {
        // Regola F: solo l'errore SQL, nessun payload.
        tracing::warn!(error = %e, "gateway-ledger: insert ledger fallita (best-effort)");
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
}
