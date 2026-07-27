//! Scrittura del ledger: ogni riga di `ai_usage_ledger` nasce qui.
//!
//! Le SQL sono costanti e non testo inline dentro le funzioni. Non e' cosmetica:
//! l'elenco delle colonne scritto a mano e' il difetto che ha tenuto a zero le
//! quattro colonne di cache su 7.405 righe — una colonna non nominata cade sul
//! DEFAULT e nessun compilatore la reclama. Come costanti, un test puo'
//! confrontarle col testo VERO della migrazione che quelle colonne le ha create
//! (regola O), invece di ricopiarne l'elenco.

use anyhow::Result;
use serde_json::{json, Value};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use nexus_pricing::{
    calculate_cost, calculate_cost_breakdown, CostBreakdown, PriceLookup, TokenUsage,
};

use crate::{
    active_quotas, quote::primo_sforo, usage_for_quotas, ChargedBy, Declaration, Identity,
    LedgerEntry, LedgerUsage, MediaUsage, QuotaLock, Reservation, Settlement,
};

// ── La riga di STIMA: prenotata, rifiutata o marker ────────────
//
// Erano QUATTRO INSERT ricopiate (una `reserved` e due `rejected` in
// `reserve_usage`, piu' il marker del job batch in `prompt_templates`), identiche
// tranne per due valori. Il marker le aveva gia' divergere: portava una currency
// 'EUR' hardcoded mentre la piattaforma e' su USD.

/// L'unica INSERT per una riga che porta una STIMA.
///
/// `finalized_at` non e' un parametro ma una CONSEGUENZA del rifiuto: una riga
/// che nasce rifiutata nasce anche chiusa, una prenotazione resta aperta finche'
/// qualcuno non la finalizza o la rilascia. Legarli qui rende impossibile la
/// combinazione incoerente "rifiutata ma ancora aperta".
const SQL_INSERT_STIMA: &str = r#"
        INSERT INTO ai_usage_ledger (
            id, user_id, project_id, provider, model,
            prompt_tokens, completion_tokens, total_tokens,
            input_cost, output_cost, total_cost, currency,
            status, rejection_reason, details, finalized_at
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15,
            CASE WHEN $14::text IS NULL THEN NULL ELSE NOW() END
        )
        "#;

/// I campi di una riga di stima, raggruppati per non passare quindici argomenti
/// sciolti (e per non poterne scambiare due dello stesso tipo).
struct RigaStima<'a> {
    ledger_id: Uuid,
    identity: Identity,
    provider: &'a str,
    model: &'a str,
    prompt_tokens: i32,
    completion_tokens: i32,
    total_tokens: i32,
    input_cost: f64,
    output_cost: f64,
    total_cost: f64,
    currency: &'a str,
    /// `Some` = riga `rejected` col motivo; `None` = riga `reserved`.
    rejection_reason: Option<String>,
    details: Value,
}

async fn insert_stima<'e, E>(exec: E, r: RigaStima<'_>) -> Result<(), sqlx::Error>
where
    E: PgExecutor<'e>,
{
    let status = if r.rejection_reason.is_some() {
        "rejected"
    } else {
        "reserved"
    };
    sqlx::query(SQL_INSERT_STIMA)
        .bind(r.ledger_id)
        .bind(r.identity.user_id)
        .bind(r.identity.project_id)
        .bind(r.provider)
        .bind(r.model)
        .bind(r.prompt_tokens)
        .bind(r.completion_tokens)
        .bind(r.total_tokens)
        .bind(r.input_cost)
        .bind(r.output_cost)
        .bind(r.total_cost)
        .bind(r.currency)
        .bind(status)
        .bind(r.rejection_reason)
        .bind(r.details)
        .execute(exec)
        .await
        .map(|_| ())
}

/// Prenota il consumo di una chiamata che sta per partire: il gate delle QUOTE.
///
/// La riga `reserved` non serve solo a contabilizzare: e' cio' che rende visibile
/// il consumo PRIMA che la chiamata avvenga, senza il quale N richieste
/// concorrenti sforerebbero tutte lo stesso limite. Lettura e scrittura stanno
/// nella stessa transazione, con le quote prese `FOR UPDATE`.
///
/// Il prezzo si legge dal punto unico e l'errore si PROPAGA: qui, a differenza
/// del gateway, non siamo ancora sul percorso della risposta e fallire e' meno
/// grave che prenotare alla cieca. Su prezzo IGNOTO invece non si respinge (che
/// sarebbe un cambio di policy sulle quote) ma lo si DICE: una stima a 0 non
/// consuma quota di costo e lascia sforare senza che nessuno se ne accorga.
pub async fn reserve(
    db: &PgPool,
    identity: Identity,
    provider: &str,
    model: &str,
    prompt_tokens: i32,
    estimated_completion_tokens: i32,
    details: Value,
) -> Result<Reservation> {
    let lookup = nexus_pricing::resolve_active_price(db, provider, model).await?;
    let currency = nexus_pricing::platform_currency(db).await?;
    if matches!(lookup, PriceLookup::Unknown) {
        tracing::warn!(
            target: "billing",
            "reserve: prezzo IGNOTO (pricing_state='unknown') -> stima quota a 0 \
             (provider={provider}, model={model})",
        );
    }
    let total_tokens = prompt_tokens.saturating_add(estimated_completion_tokens);
    let (input_cost, output_cost, total_cost) = match &lookup {
        PriceLookup::Priced(p) => {
            calculate_cost(p, prompt_tokens as i64, estimated_completion_tokens as i64)
        }
        _ => (0.0, 0.0, 0.0),
    };

    let mut tx = db.begin().await?;
    let quotas = active_quotas(&mut *tx, identity.user_id, identity.project_id, QuotaLock::ForUpdate)
        .await?;
    let consumi = usage_for_quotas(&mut *tx, &quotas).await?;

    let ledger_id = Uuid::new_v4();
    let sforo = primo_sforo(&quotas, &consumi, total_tokens as i64, total_cost);
    let riga = RigaStima {
        ledger_id,
        identity,
        provider,
        model,
        prompt_tokens,
        completion_tokens: estimated_completion_tokens,
        total_tokens,
        input_cost,
        output_cost,
        total_cost,
        currency: &currency,
        rejection_reason: sforo.as_ref().map(|e| e.to_string()),
        details,
    };
    // La riga si scrive in entrambi i casi, e in entrambi la transazione si
    // chiude: il rifiuto e' un fatto contabile, non solo un errore di ritorno.
    insert_stima(&mut *tx, riga).await?;
    tx.commit().await?;

    match sforo {
        Some(e) => Err(anyhow::Error::new(e)),
        None => Ok(Reservation {
            ledger_id,
            lookup,
            currency,
        }),
    }
}

/// Marker "job partito" del batch: una riga `reserved` a costo zero.
///
/// Non e' una prenotazione da chiudere — nessuno la finalizzera' — ma un fatto
/// che si vuole poter leggere sul DB giusto. Passa dalla stessa INSERT delle
/// altre stime: quando era una copia a se' portava una currency 'EUR' decisa in
/// proprio, mentre la piattaforma e' su USD (regola G).
pub async fn insert_marker(
    db: &PgPool,
    identity: Identity,
    provider: &str,
    model: &str,
    details: Value,
) -> Result<Uuid> {
    let currency = nexus_pricing::platform_currency(db).await?;
    let ledger_id = Uuid::new_v4();
    insert_stima(
        db,
        RigaStima {
            ledger_id,
            identity,
            provider,
            model,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            input_cost: 0.0,
            output_cost: 0.0,
            total_cost: 0.0,
            currency: &currency,
            rejection_reason: None,
            details,
        },
    )
    .await?;
    Ok(ledger_id)
}

// ── La riga di una chiamata GIA' avvenuta ──────────────────────

/// La INSERT del percorso testuale.
///
/// `id` e' RESTITUITO dalla INSERT invece di essere letto dopo: e' l'unico modo
/// per dichiarare al chiamante QUALE riga porta l'addebito senza una seconda
/// query che potrebbe pescarne un'altra.
const SQL_INSERT_FINALIZED: &str = r#"
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

/// Listino con DEGRADO esplicito: non propaga.
///
/// Policy di chi sta sul percorso della risposta: far fallire una chiamata LLM
/// gia' avvenuta perche' non sappiamo prezzarla sostituirebbe una sottostima con
/// un outage, e il denaro e' gia' stato speso comunque. La visibilita' che la
/// regola G esige si ottiene ALL'AVVIO con `nexus_pricing::assert_configured`,
/// dove fallire e' gratuito, piu' il WARN qui sotto e `details.price_state` sulla
/// riga.
pub(crate) async fn listino_degradante(db: &PgPool, provider: &str, model: &str) -> PriceLookup {
    let currency = match nexus_pricing::platform_currency(db).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "ledger: currency di piattaforma non risolvibile -> costo non calcolabile \
                 (la richiesta prosegue: vedi assert_configured all'avvio)"
            );
            return PriceLookup::NotInCatalog;
        }
    };
    match nexus_pricing::resolve_active_price_in(db, provider, model, &currency).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, provider = %provider, model = %model,
                "ledger: lettura listino fallita -> costo non calcolabile");
            PriceLookup::NotInCatalog
        }
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

/// Currency e costo della riga, piu' i due segnali sullo stato del listino.
async fn prezza_chiamata(
    db: &PgPool,
    provider: &str,
    model: &str,
    tokens: &TokenUsage,
) -> (String, CostBreakdown, &'static str, bool) {
    let price = listino_degradante(db, provider, model).await;
    let state = price.state_label();
    let missing = price.is_missing();
    match &price {
        PriceLookup::Priced(p) => (
            p.currency.trim().to_uppercase(),
            calculate_cost_breakdown(p, tokens),
            state,
            missing,
        ),
        _ => {
            if matches!(price, PriceLookup::Unknown) {
                tracing::warn!(
                    provider = %provider, model = %model,
                    prompt_tokens = tokens.prompt_tokens,
                    completion_tokens = tokens.completion_tokens,
                    "ledger: prezzo IGNOTO (pricing_state='unknown') -> costo NON calcolabile, \
                     registro 0 esplicito. Il modello non dovrebbe essere routabile: vedi il \
                     ciclo reconcile_disable_price_unknown del catalog_sync"
                );
            }
            // Costo 0 -> la currency e' vacua, ma la colonna e' NOT NULL: si
            // annota quella di piattaforma. Se nemmeno quella e' leggibile il DB
            // e' giu' e la INSERT fallisce comunque, quindi la stringa vuota non
            // raggiunge una riga persistita.
            let cur = nexus_pricing::platform_currency(db).await.unwrap_or_default();
            (cur, costo_nullo(), state, missing)
        }
    }
}

/// Registra una chiamata testuale GIA' avvenuta, come riga `finalized`.
///
/// RITORNA la riga scritta, oppure `None` se la INSERT e' fallita. Il valore di
/// ritorno non e' telemetria: e' il segnale STRUTTURATO (regola M) su cui chi ha
/// prenotato decide se addebitare a sua volta (vedi [`settle`]). Best-effort: un
/// errore di scrittura non interrompe la risposta al chiamante, ma DEVE essere
/// dichiarato, o chi rilasciasse la propria prenotazione perderebbe del tutto
/// l'addebito.
///
/// L'identita' arriva gia' risolta: chi chiama la estrae dai propri tipi e, se
/// non c'e', non chiama affatto. E' il caso reale delle richieste senza
/// `tenant_id`/`user_id` (`GwMetadata::default`), dove il gateway non scrive.
pub async fn record_tokens(
    db: &PgPool,
    identity: Identity,
    provider: &str,
    model: &str,
    tokens: &TokenUsage,
    request_id: &str,
    feature: &str,
) -> Option<LedgerEntry> {
    let (currency, costo, price_state, price_missing) =
        prezza_chiamata(db, provider, model, tokens).await;

    let details = json!({
        "request_id": request_id,
        "feature": feature,
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
    let run_uuid = Uuid::parse_str(request_id.trim()).ok();

    let res = sqlx::query_scalar::<_, Uuid>(SQL_INSERT_FINALIZED)
        .bind(identity.user_id)
        .bind(identity.project_id)
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
        Ok(id) => Some(LedgerEntry {
            id,
            total_cost: costo.total_cost,
            currency,
        }),
        Err(e) => {
            // Regola F: solo l'errore SQL, nessun payload.
            tracing::warn!(error = %e, "ledger: insert finalized fallita (best-effort)");
            None
        }
    }
}

// ── Chiusura di una prenotazione ───────────────────────────────

/// La UPDATE che chiude la riga di ledger.
///
/// `details` si FONDE (`||`) invece di essere sovrascritto: la prenotazione vi ha
/// gia' messo i propri campi (feature, price_state...) e la finalizzazione vi
/// aggiunge soltanto lo stato del listino di cache. Un assegnamento secco li
/// perderebbe.
const SQL_UPDATE_FINALIZE: &str = r#"
        UPDATE ai_usage_ledger
        SET run_id = $2,
            prompt_tokens = $3,
            completion_tokens = $4,
            total_tokens = $5,
            cache_read_tokens = $6,
            cache_creation_tokens = $7,
            input_cost = $8,
            output_cost = $9,
            cache_read_cost = $10,
            cache_creation_cost = $11,
            total_cost = $12,
            details = details || $13::jsonb,
            status = 'finalized',
            finalized_at = NOW()
        WHERE id = $1
        "#;

/// Chiude la prenotazione coi numeri REALI della chiamata.
///
/// Il costo si calcola dai token effettivi, ma solo se il listino era noto: su
/// prezzo ignoto il ledger registra uno zero DICHIARATO (`price_state`), non un
/// costo calcolato su un prezzo placeholder. Lo scorporo della cache lo fa il
/// punto unico `nexus-pricing`; qui non si moltiplica nulla.
pub async fn finalize(
    db: &PgPool,
    reservation: &Reservation,
    run_id: Uuid,
    usage: &LedgerUsage,
) -> Result<Settlement> {
    let costo = match &reservation.lookup {
        PriceLookup::Priced(p) => calculate_cost_breakdown(p, &usage.tokens),
        _ => costo_nullo(),
    };

    // Lo stesso segnale STRUTTURATO che porta la riga scritta da chi ha eseguito
    // la chiamata: senza, un `cache_read_cost` a zero sarebbe leggibile su meta'
    // delle righe e ambiguo sull'altra meta' (regola M).
    let details = json!({ "cache_price_state": costo.cache_price_state() });

    sqlx::query(SQL_UPDATE_FINALIZE)
        .bind(reservation.ledger_id)
        .bind(run_id)
        .bind(usage.tokens.prompt_tokens)
        .bind(usage.tokens.completion_tokens)
        .bind(usage.total_tokens)
        .bind(usage.tokens.cache_read_tokens)
        .bind(usage.tokens.cache_creation_tokens)
        .bind(costo.input_cost)
        .bind(costo.output_cost)
        .bind(costo.cache_read_cost)
        .bind(costo.cache_creation_cost)
        .bind(costo.total_cost)
        .bind(details)
        .execute(db)
        .await?;

    Ok(Settlement {
        total_cost: costo.total_cost,
        currency: reservation.currency.clone(),
        charged_by: ChargedBy::Reservation,
    })
}

/// La UPDATE che rilascia una prenotazione.
///
/// AZZERA i conteggi e gli importi. Non e' pulizia estetica: su una riga
/// `released` la stima e' denaro che nessuno ha speso, e non tutti i lettori del
/// ledger filtrano per stato — `usage_report` somma `total_cost` di TUTTE le
/// righe che passano i suoi filtri. Lasciarci la stima significherebbe sostituire
/// un doppio addebito con un doppio addebito piu' discreto. L'importo di una riga
/// deve essere coerente col suo stato strutturato (regola M): `released` = non
/// addebitato = 0.
///
/// La stima non viene distrutta, trasloca in `details.released_estimate`: e'
/// leggibile per audit e non e' sommabile per sbaglio. Le espressioni a destra
/// dell'assegnamento vedono i valori VECCHI della riga, quindi il travaso e
/// l'azzeramento convivono nella stessa UPDATE.
///
/// `$3` e' NULL quando non c'e' nulla da correlare: senza `COALESCE` la
/// concatenazione con NULL azzererebbe l'intero `details`.
const SQL_UPDATE_RELEASE: &str = r#"
        UPDATE ai_usage_ledger
        SET status = 'released',
            rejection_reason = $2,
            finalized_at = NOW(),
            details = details || jsonb_build_object(
                'released_estimate',
                jsonb_build_object(
                    'prompt_tokens', prompt_tokens,
                    'completion_tokens', completion_tokens,
                    'total_tokens', total_tokens,
                    'total_cost', total_cost
                )
            ) || COALESCE($3::jsonb, '{}'::jsonb),
            prompt_tokens = 0,
            completion_tokens = 0,
            total_tokens = 0,
            input_cost = 0,
            output_cost = 0,
            total_cost = 0
        WHERE id = $1
        "#;

/// Rilascia una prenotazione: la riga resta (coi suoi `details`, che sono il
/// contesto che solo il chiamante conosce — intent, profile_id,
/// corrections_count) ma esce dalla contabilita' e dalle quote, che contano solo
/// `reserved` e `finalized`.
///
/// `extra_details` si fonde nei `details`: e' il posto della CORRELAZIONE quando
/// il rilascio avviene perche' ad addebitare e' stato qualcun altro
/// (`superseded_by_ledger_id`). `None` quando non c'e' nulla da correlare — il
/// rilascio per fallimento della chiamata.
pub async fn release(
    db: &PgPool,
    reservation: &Reservation,
    reason: &str,
    extra_details: Option<Value>,
) {
    if let Err(e) = sqlx::query(SQL_UPDATE_RELEASE)
        .bind(reservation.ledger_id)
        .bind(reason)
        .bind(extra_details)
        .execute(db)
        .await
    {
        // Una prenotazione non rilasciata resta 'reserved' e continua a occupare
        // quota per sempre: il fallimento va VISTO, non ingoiato (regola F: solo
        // l'errore SQL, nessun payload).
        tracing::warn!(
            target: "billing",
            error = %e,
            ledger_id = %reservation.ledger_id,
            reason = %reason,
            "release: rilascio della prenotazione fallito, la riga resta 'reserved' e occupa quota"
        );
    }
}

/// PUNTO UNICO della domanda "chi addebita questa chiamata".
///
/// Prima la risposta era implicita e sbagliata: chi prenotava finalizzava
/// SEMPRE, mentre il gateway — dentro la stessa richiesta HTTP — inseriva gia'
/// la propria riga `finalized`. Due righe finalizzate, stesso `run_id`, stessi
/// token, costo raddoppiato (incidente 2026-07-27).
///
/// La decisione NON si deduce dall'esito della chiamata: si legge dal segnale
/// strutturato che chi ha eseguito emette solo quando ha davvero scritto
/// ([`LedgerEntry`], regola M). Rilasciare "perche' la chiamata e' riuscita"
/// perderebbe l'addebito su tutti i percorsi in cui quella riga non c'e':
/// richiesta senza identita', identita' non-UUID, INSERT fallita.
///
/// La prenotazione NON viene cancellata: resta come riga `released` che conserva
/// i propri `details` e punta alla riga che porta l'addebito, cosi' il contesto
/// che solo il prenotante conosce non si perde e le due righe sono correlate.
/// La dichiarazione arriva INTERA e non gia' ridotta a `Option<&LedgerEntry>`:
/// la riduzione e' la decisione, e farla al call site significherebbe averne una
/// copia per chiamante. `Muta` e `Illeggibile` finalizzano come "nessuno ha
/// scritto" — l'unica scelta che non perde l'addebito — ma non sono innocue, e
/// chi ha chiamato lo scopre da [`Declaration::audit`], non da qui: solo lui sa
/// cosa aveva mandato.
pub async fn settle(
    db: &PgPool,
    reservation: &Reservation,
    run_id: Uuid,
    usage: &LedgerUsage,
    declaration: &Declaration,
) -> Result<Settlement> {
    match declaration.entry() {
        Some(entry) => {
            release(
                db,
                reservation,
                "gateway_ledger",
                Some(json!({ "superseded_by_ledger_id": entry.id })),
            )
            .await;
            Ok(Settlement {
                total_cost: entry.total_cost,
                currency: entry.currency.clone(),
                charged_by: ChargedBy::Executor,
            })
        }
        None => finalize(db, reservation, run_id, usage).await,
    }
}

// ── Consumo delle modalita' non-testuali ───────────────────────

/// L'INSERT del consumo media.
///
/// I costi per-unita' finiscono in `total_cost`: non sono ne' input ne' output, e
/// spalmarli su una delle due colonne token-oriented direbbe una cosa falsa a chi
/// le legge. `input_cost`/`output_cost` e i token restano 0 (default di colonna):
/// per queste righe il consumo vive in `quantity`.
const SQL_INSERT_MEDIA: &str = r#"
        INSERT INTO ai_usage_ledger (
            user_id, project_id, run_id, provider, model,
            total_cost, currency, status, details,
            usage_kind, quantity, quantity_unit, quantity_source
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, 'finalized', $8, $9, $10, $11, $12
        )
        "#;

/// Registra il consumo di una chiamata non-testuale (immagini, video, audio).
///
/// Stesse regole del percorso testuale: best-effort, `status='finalized'`.
/// PUNTO UNICO anche verso l'alto: i quattro handler media sono copie parallele,
/// e scrivere il ledger in ognuno avrebbe creato la quinta copia.
pub async fn record_media(
    db: &PgPool,
    identity: Identity,
    provider: &str,
    model: &str,
    usage: &MediaUsage,
    request_id: &str,
    feature: &str,
) {
    // Currency prima del prezzo: serve comunque, perche' la colonna e' NOT NULL
    // senza default (un default hardcoded qui e' gia' costato 3.993 righe orfane
    // prima della mig 0294).
    let currency = match nexus_pricing::platform_currency(db).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e,
                "ledger: currency di piattaforma illeggibile, riga media non scritta");
            return;
        }
    };

    let price = match nexus_pricing::resolve_unit_price(db, provider, model, usage.unit, &currency)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "ledger: lookup listino per-unita' fallito");
            nexus_pricing::UnitPriceLookup::NotInCatalog
        }
    };
    // Il costo esiste solo se esistono ENTRAMBI: un listino per quell'unita' E
    // una quantita' da moltiplicare. Mancando l'uno o l'altra il risultato e' 0
    // DICHIARATO via `price_state`, mai dedotto.
    let total_cost = usage.quantity.and_then(|q| price.cost_for(q)).unwrap_or(0.0);

    let details = json!({
        "request_id": request_id,
        "feature": feature,
        "price_missing": total_cost == 0.0,
        "price_state": price.state_label(),
    });

    let res = sqlx::query(SQL_INSERT_MEDIA)
        .bind(identity.user_id)
        .bind(identity.project_id)
        .bind(Uuid::parse_str(request_id.trim()).ok())
        .bind(provider)
        .bind(model)
        .bind(total_cost)
        .bind(&currency)
        .bind(details)
        .bind(usage.kind.as_str())
        .bind(usage.quantity)
        .bind(usage.quantity.map(|_| usage.unit.as_str()))
        .bind(usage.source.as_str())
        .execute(db)
        .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, "ledger: insert media fallita (best-effort)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Le SQL e le colonne che la migrazione ha creato ─────────
    //
    // Una colonna non nominata cade sul DEFAULT e nessun compilatore la reclama:
    // e' cosi' che `cache_read_tokens` e `cache_creation_tokens` sono rimaste a
    // zero su 7.405 chiamate mentre gli adapter le leggevano. Il confronto e' col
    // testo VERO della migrazione applicata al database (regola O), non con una
    // lista ricopiata nel test.
    const MIGRAZIONE_0129: &str =
        include_str!("../../../db/migrations/0129_ledger_cache_columns.sql");

    /// Ogni colonna di cache creata dalla mig 0129 deve comparire in ENTRAMBE le
    /// scritture che chiudono una chiamata: la INSERT di chi ha eseguito e la
    /// UPDATE di chi aveva prenotato. Erano due costanti in due crate, tenute
    /// gemelle a mano; qui il test le guarda insieme.
    #[test]
    fn le_due_scritture_nominano_le_colonne_di_cache() {
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
                SQL_INSERT_FINALIZED.contains(colonna),
                "la INSERT non elenca {colonna}: resterebbe al DEFAULT 0"
            );
            assert!(
                SQL_UPDATE_FINALIZE.contains(colonna),
                "la UPDATE non assegna {colonna}: resterebbe al valore della prenotazione, cioe' 0"
            );
        }
    }

    /// Un segnaposto per ogni valore bindato. Uno scarto qui e' un errore SQL a
    /// runtime: sulla INSERT e' best-effort e quindi solo loggato, cioe'
    /// invisibile; sulla UPDATE propaga con `?`, cioe' una finalizzazione che
    /// fallisce dopo che la chiamata e' stata pagata.
    #[test]
    fn i_segnaposto_coprono_i_bind() {
        for (sql, n, nome) in [
            (SQL_INSERT_STIMA, 15, "INSERT stima"),
            (SQL_INSERT_FINALIZED, 17, "INSERT finalized"),
            (SQL_UPDATE_FINALIZE, 13, "UPDATE finalize"),
            (SQL_INSERT_MEDIA, 12, "INSERT media"),
        ] {
            for i in 1..=n {
                assert!(
                    sql.contains(&format!("${i}")),
                    "{nome}: placeholder ${i} assente"
                );
            }
            assert!(
                !sql.contains(&format!("${}", n + 1)),
                "{nome}: placeholder di troppo rispetto ai bind"
            );
        }
    }

}
