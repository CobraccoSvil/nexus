//! Riordino cache-aware e finestre-aware dei candidati `Rank::CostFirst`.
//!
//! # Il difetto che chiude
//!
//! La selezione del servizio unico ordina i candidati sul prezzo NOMINALE
//! dell'input (`AGENTIC_COST_FIRST_ORDER`), che ignora due fatti gia' misurati
//! e gia' usati altrove:
//!
//! - l'hit-rate di prompt-cache osservato sul ledger (mig 0656): in un loop
//!   agentico il prefisso e' identico a ogni iterazione, e sullo stesso task
//!   mistral (5,2% di hit) e' costato 18 volte deepseek (67%). La catena di
//!   escalation ordina GIA' sul costo atteso (`escalation_port`); la selezione
//!   primaria — che decide la maggioranza delle chiamate — no, ed e' la ragione
//!   per cui mistral fa il 62-65% delle chiamate del parco;
//! - le finestre orarie di prezzo (mig 0715): deepseek in fascia peak vale 2x,
//!   e un ORDER BY sul prezzo base non le vede mai.
//!
//! # Il criterio
//!
//! Costo ATTESO del milione di token di prompt:
//! `expected_call_cost(prezzo_vigente, CallShape{1M, 0}, hit).total_cost`.
//! NESSUNA seconda formula (regola L): la funzione e' esattamente quella
//! dell'escalation, il prezzo vigente viene da
//! [`nexus_pricing::resolve_active_prices_at`] (il moltiplicatore di finestra
//! si applica DENTRO il resolve, quindi le fasce orarie entrano gratis), l'hit
//! da [`nexus_ledger::observed_cache_hit_rates`]. `CallShape{1M, 0}` e' lo
//! STESSO asse dell'ORDER BY di oggi (input per milione), non un blended
//! inventato: l'output resta fuori dal criterio, come oggi.
//!
//! # Il tier resta primario
//!
//! Con `min_distinct_providers <= 1` le righe sono tutte del PRIMO tier non
//! vuoto della catena, quindi il vincolo e' automatico. Nel fan-out (righe
//! multi-tier) il riordino e' per-gruppo: sort stabile per costo, poi sort
//! stabile per indice del gruppo di tier — lo stesso trucco «il tier torna a
//! comandare» di `escalation_port::riordina_per_costo_atteso`.
//!
//! # Best-effort
//!
//! Su guasto di lettura (currency, listino) l'ordine resta quello SQL: chi non
//! sa non peggiora. L'hit-rate e' un DI PIU': un guasto li' degrada a cache
//! fredda per tutti, come nell'escalation.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::collections::HashMap;

use nexus_pricing::{expected_call_cost, CacheHitRate, CallShape, PriceLookup};

/// Quanti candidati recuperare per il riordino. Misurato ~10 modelli/tier sul
/// catalog vivo, margine 2. Come [`super::model_service`] usa il suo
/// `GOVERNED_CANDIDATE_POOL`: un pool del PRIMO tier con candidati — il
/// riordino promuove fra alternative GIA' ammissibili, non allarga la
/// selezione ad altri tier (la degradazione resta di `TierPolicy`).
pub(super) const COST_RANK_POOL: i64 = 12;

/// Flag di rollout (mig 0721). A OFF il percorso e' bit-identico allo storico.
pub(super) const FLAG_CACHE_AWARE: &str = "routing.cost_rank_cache_aware";

/// `true` se il riordino cache-aware e' acceso (default OFF: nessuna funzione
/// si accende da sola, stessa disciplina di `governance_enabled`).
pub(super) async fn cache_aware_enabled(db: &PgPool) -> bool {
    nexus_auth::get_bool_setting_or(db, FLAG_CACHE_AWARE, false).await
}

/// Da dove viene l'hit atteso di una coppia (regola Q: l'ignoto e' una
/// variante dichiarata, e "niente cache" non e' "non so").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProvenienzaHit {
    /// La misura del ledger (finestra + soglia di mig 0656).
    Misurata,
    /// Il ledger misura hit > 0 ma `supports_prompt_cache` dichiara FALSE:
    /// deriva della colonna. Vince la misura (il fatto piu' recente batte la
    /// dichiarazione stantia, stessa evidenza della mig 0703 §3) e il
    /// chiamante lo dice con un warn.
    DichiarazioneSmentita,
    /// Nessuna misura E `supports_prompt_cache = FALSE`: la cache NON esiste,
    /// e' un fatto dichiarato, non un'assenza di dato. Costo identico a
    /// `Ignota`, ma la telemetria distingue "niente cache" da "non so"
    /// (regola M).
    CacheAssenteDichiarata,
    /// Nessuna misura e nessuna dichiarazione negativa: listino pieno.
    Ignota,
}

/// PUNTO UNICO della composizione fra la misura del ledger e la dichiarazione
/// `supports_prompt_cache` della vista capability. PURA.
///
/// Precedenza: la MISURA vince sempre quando c'e' (il ledger e' un fatto, la
/// colonna una dichiarazione). La dichiarazione decide solo dove il ledger
/// tace: FALSE trasforma l'assenza di misura in un fatto (`Observed(0.0)`),
/// TRUE o assenza della riga lasciano l'ignoto ignoto.
pub(super) fn risolvi_hit(
    misura: Option<CacheHitRate>,
    dichiarato: Option<bool>,
) -> (CacheHitRate, ProvenienzaHit) {
    match (misura, dichiarato) {
        (Some(CacheHitRate::Observed(f)), Some(false)) if f > 0.0 => (
            CacheHitRate::Observed(f),
            ProvenienzaHit::DichiarazioneSmentita,
        ),
        (Some(m), _) => (m, ProvenienzaHit::Misurata),
        (None, Some(false)) => (
            CacheHitRate::observed(0.0),
            ProvenienzaHit::CacheAssenteDichiarata,
        ),
        (None, _) => (CacheHitRate::Unknown, ProvenienzaHit::Ignota),
    }
}

/// I fatti da cui si calcola il costo atteso: listini vigenti (finestre
/// incluse), misure di hit dal ledger, dichiarazioni della vista capability.
struct FattiCosto {
    prezzi: HashMap<String, HashMap<String, PriceLookup>>,
    hit_misurato: HashMap<String, HashMap<String, CacheHitRate>>,
    dichiarazioni: HashMap<(String, String), bool>,
}

/// Carica i fatti per i provider dei candidati, una lettura batch per provider
/// (mai una per riga). `None` = currency o listino illeggibili: il criterio
/// stesso manca, l'ordine resta quello SQL. L'hit-rate invece e' un DI PIU'
/// (stessa scelta dell'escalation): un guasto li' degrada a cache fredda per
/// tutti invece di annullare il riordino.
async fn carica_fatti(db: &PgPool, providers: &[&str], at: DateTime<Utc>) -> Option<FattiCosto> {
    let currency = match nexus_pricing::platform_currency(db).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "cost_rank: currency illeggibile, ordine di listino SQL");
            return None;
        }
    };
    let hit_window = match nexus_ledger::HitRateWindow::load(db).await {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!(error = %e, "cost_rank: finestra hit-rate non configurata, cache fredda per tutti");
            None
        }
    };
    let mut prezzi = HashMap::new();
    let mut hit_misurato = HashMap::new();
    for p in providers {
        match nexus_pricing::resolve_active_prices_at(db, p, &currency, at).await {
            Ok(m) => {
                prezzi.insert(p.to_string(), m);
            }
            Err(e) => {
                tracing::warn!(provider = %p, error = %e, "cost_rank: listino illeggibile, ordine di listino SQL");
                return None;
            }
        }
        if let Some(w) = hit_window {
            match nexus_ledger::observed_cache_hit_rates(db, p, w).await {
                Ok(m) => {
                    hit_misurato.insert(p.to_string(), m);
                }
                Err(e) => {
                    tracing::warn!(provider = %p, error = %e, "cost_rank: hit-rate illeggibile, cache fredda");
                }
            }
        }
    }
    Some(FattiCosto {
        prezzi,
        hit_misurato,
        dichiarazioni: supports_prompt_cache_batch(db, providers).await,
    })
}

/// La chiave di costo di UN candidato: costo atteso del milione di token di
/// prompt. `None` = non calcolabile (fuori catalog o listino 'unknown'): resta
/// in coda fra i suoi pari invece di fingere costo zero.
fn chiave_di_costo(fatti: &FattiCosto, provider: &str, model: &str) -> Option<f64> {
    let lookup = fatti.prezzi.get(provider).and_then(|m| m.get(model));
    let Some(PriceLookup::Priced(prezzo)) = lookup else {
        return None;
    };
    let misura = fatti
        .hit_misurato
        .get(provider)
        .and_then(|m| m.get(model))
        .copied();
    let dichiarato = fatti
        .dichiarazioni
        .get(&(provider.to_string(), model.to_string()))
        .copied();
    let (hit, provenienza) = risolvi_hit(misura, dichiarato);
    if provenienza == ProvenienzaHit::DichiarazioneSmentita {
        tracing::warn!(
            provider = %provider, model = %model,
            "cost_rank: supports_prompt_cache=FALSE ma il ledger misura hit di \
             cache: vince la misura (deriva della colonna, stessa evidenza della \
             migrazione di igiene config-token)"
        );
    }
    let call = CallShape {
        prompt_tokens: 1_000_000,
        completion_tokens: 0,
    };
    let costo = expected_call_cost(prezzo, &call, hit).total_cost;
    // La chiave di costo per candidato e' un dato dichiarato (regola M): chi
    // legge il log puo' rifare il confronto, non dedurlo.
    tracing::debug!(
        provider = %provider, model = %model,
        costo_atteso_1m_prompt = costo,
        hit = hit.state_label(),
        "cost_rank: chiave di costo del candidato"
    );
    Some(costo)
}

/// Riordina i candidati `CostFirst` sul costo ATTESO (cache + finestre orarie).
/// L'ingresso di produzione: fissa l'istante a `Utc::now()`, come
/// `resolve_active_price_in` fa per la lettura singola.
pub(super) async fn rerank_expected_cost(
    db: &PgPool,
    rows: Vec<(String, String, Option<String>)>,
) -> Vec<(String, String, Option<String>)> {
    rerank_expected_cost_at(db, rows, Utc::now()).await
}

/// La variante con l'ISTANTE come parametro: i test iniettano l'ora invece di
/// aspettare la fascia giusta (regola O, stessa ragione per cui
/// `resolve_active_prices_at` e' pubblica).
pub(super) async fn rerank_expected_cost_at(
    db: &PgPool,
    rows: Vec<(String, String, Option<String>)>,
    at: DateTime<Utc>,
) -> Vec<(String, String, Option<String>)> {
    if rows.len() < 2 {
        return rows;
    }
    // Provider distinti, nell'ordine di prima apparizione.
    let mut providers: Vec<&str> = Vec::new();
    for (p, _, _) in &rows {
        if !providers.contains(&p.as_str()) {
            providers.push(p.as_str());
        }
    }
    let Some(fatti) = carica_fatti(db, &providers, at).await else {
        return rows;
    };
    // Chiave di costo per riga, calcolata UNA volta (il warn della
    // dichiarazione smentita non deve ripetersi a ogni confronto del sort).
    let costi: Vec<Option<f64>> = rows
        .iter()
        .map(|(p, m, _)| chiave_di_costo(&fatti, p, m))
        .collect();

    // Indice del GRUPPO di tier: prima apparizione nell'ordine SQL. Non
    // `tier_rank`: la catena `Flexible` scende e poi sale, quindi l'ordine dei
    // gruppi non e' monotono nel rank — comanda l'ordine di visita della
    // catena, che e' quello con cui le righe arrivano.
    let mut indice_gruppo: HashMap<Option<&str>, usize> = HashMap::new();
    let gruppi: Vec<usize> = rows
        .iter()
        .map(|(_, _, t)| {
            let prossimo = indice_gruppo.len();
            *indice_gruppo.entry(t.as_deref()).or_insert(prossimo)
        })
        .collect();

    let mut ordine: Vec<usize> = (0..rows.len()).collect();
    // Sort STABILE per costo (Some prima di None, comparatore identico a
    // `escalation_port::riordina_per_costo_atteso`), poi sort STABILE per
    // gruppo: dentro ogni gruppo sopravvive l'ordine per costo atteso, fra
    // gruppi comanda la catena. A costi uniformi il tie-break SQL
    // (`is_featured`) resta intatto per stabilita'.
    ordine.sort_by(|&a, &b| match (costi[a], costi[b]) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    ordine.sort_by_key(|&i| gruppi[i]);

    let mut per_indice: Vec<Option<(String, String, Option<String>)>> =
        rows.into_iter().map(Some).collect();
    ordine
        .into_iter()
        .map(|i| per_indice[i].take().expect("indice usato una volta sola"))
        .collect()
}

/// Lettura batch della dichiarazione `supports_prompt_cache` dalla vista unica
/// `v_model_capabilities` (ADR 0024) — primo lettore decisionale della colonna.
/// Un modello ASSENTE dalla vista non ha dichiarato nulla: `None`, mai un
/// default. Best-effort: su errore la mappa e' vuota (nessuna dichiarazione),
/// il criterio degrada alla sola misura del ledger.
async fn supports_prompt_cache_batch(
    db: &PgPool,
    providers: &[&str],
) -> HashMap<(String, String), bool> {
    let providers: Vec<String> = providers.iter().map(|p| p.to_string()).collect();
    match sqlx::query_as::<_, (String, String, bool)>(
        "SELECT provider, model, supports_prompt_cache \
           FROM v_model_capabilities \
          WHERE provider = ANY($1)",
    )
    .bind(&providers)
    .fetch_all(db)
    .await
    {
        Ok(rows) => rows.into_iter().map(|(p, m, f)| ((p, m), f)).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "cost_rank: v_model_capabilities illeggibile, nessuna dichiarazione di cache");
            HashMap::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// La composizione misura/dichiarazione, a tabella. Le etichette contano
    /// (regola M): "niente cache" (`Observed(0.0)` dichiarata) e "non so"
    /// (`Unknown`) portano allo stesso costo ma NON sono lo stesso fatto.
    ///
    /// MUTAZIONE: se `risolvi_hit` collassa i casi — p.es. `(None, Some(false))`
    /// che torna `Unknown`, o la misura che cede alla dichiarazione — una riga
    /// della tabella rosseggia.
    #[test]
    fn la_composizione_misura_dichiarazione_a_tabella() {
        // La misura vince sulla dichiarazione TRUE.
        assert_eq!(
            risolvi_hit(Some(CacheHitRate::Observed(0.7)), Some(true)),
            (CacheHitRate::Observed(0.7), ProvenienzaHit::Misurata)
        );
        // La misura > 0 vince sulla dichiarazione FALSE, ma la deriva si dichiara.
        assert_eq!(
            risolvi_hit(Some(CacheHitRate::Observed(0.7)), Some(false)),
            (
                CacheHitRate::Observed(0.7),
                ProvenienzaHit::DichiarazioneSmentita
            )
        );
        // Misura ZERO con flag FALSE: non e' una smentita, e' una conferma.
        assert_eq!(
            risolvi_hit(Some(CacheHitRate::Observed(0.0)), Some(false)),
            (CacheHitRate::Observed(0.0), ProvenienzaHit::Misurata)
        );
        // Nessuna misura + FALSE: la cache NON esiste — Observed(0.0), non Unknown.
        assert_eq!(
            risolvi_hit(None, Some(false)),
            (
                CacheHitRate::Observed(0.0),
                ProvenienzaHit::CacheAssenteDichiarata
            )
        );
        // Nessuna misura + TRUE o nessuna riga: ignoto dichiarato, listino pieno.
        assert_eq!(
            risolvi_hit(None, Some(true)),
            (CacheHitRate::Unknown, ProvenienzaHit::Ignota)
        );
        assert_eq!(
            risolvi_hit(None, None),
            (CacheHitRate::Unknown, ProvenienzaHit::Ignota)
        );
    }

    async fn seed_modello(
        pool: &sqlx::PgPool,
        provider: &str,
        model: &str,
        tier: &str,
        input: f64,
        cache_read: Option<f64>,
    ) {
        sqlx::query(
            "INSERT INTO ai_price_catalog \
               (provider, model, performance_tier, input_cost_per_million_tokens, \
                output_cost_per_million_tokens, cache_read_cost_per_million_tokens, \
                currency, is_enabled, supports_tool_use, agentic_thinking_policy, \
                capabilities, context_window, pricing_state, qualification_state, \
                last_probe_healthy_at) \
             VALUES ($1,$2,$3,$4,1.0,$5,'USD',TRUE,TRUE,'none','[\"code\"]'::jsonb, \
                     200000,'priced','qualified',now())",
        )
        .bind(provider)
        .bind(model)
        .bind(tier)
        .bind(input)
        .bind(cache_read)
        .execute(pool)
        .await
        .expect("seed catalog");
    }

    /// Test 2 del design: la finestra oraria di prezzo (mig 0715) entra nel
    /// confronto. Il prezzo vigente passa dal resolver unico
    /// (`resolve_active_prices_at`, istante INIETTATO — regola O), quindi il
    /// moltiplicatore x3 della fascia si vede; a parita' di fascia spenta
    /// l'ordine resta quello nominale.
    ///
    /// MUTAZIONE (eseguita davvero, vedi commit): se il reranker legge il
    /// prezzo con SQL diretto dal catalogo — il difetto censito — la finestra
    /// non si vede, l'ordine non si ribalta alle 02:30 e il test rosseggia.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_finestra_peak_entra_nel_confronto(pool: sqlx::PgPool) {
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        // A nominale 0.40 < B 0.60: l'ordine SQL mette A prima.
        seed_modello(&pool, "prov-a", "a-nominale", "medium", 0.40, None).await;
        seed_modello(&pool, "prov-b", "b-fisso", "medium", 0.60, None).await;
        // Fascia peak 01:00-04:00 UTC a x3 sul solo provider A (jolly di modello).
        sqlx::query(
            "INSERT INTO ai_price_window (provider, model, start_utc, end_utc, multiplier, label) \
             VALUES ('prov-a', NULL, '01:00', '04:00', 3.0, 'peak-test')",
        )
        .execute(&pool)
        .await
        .expect("seed finestra");

        let rows = vec![
            ("prov-a".to_string(), "a-nominale".to_string(), Some("medium".to_string())),
            ("prov-b".to_string(), "b-fisso".to_string(), Some("medium".to_string())),
        ];

        // In fascia peak (02:30 UTC): A vale 1.20 > 0.60, vince B.
        let in_peak = Utc.with_ymd_and_hms(2026, 8, 16, 2, 30, 0).unwrap();
        let ordinati = rerank_expected_cost_at(&pool, rows.clone(), in_peak).await;
        assert_eq!(
            ordinati[0].1, "b-fisso",
            "in fascia peak il prezzo vigente di A e' x3: deve vincere B. Se qui \
             c'e' ancora A, il reranker sta leggendo il prezzo base senza finestre"
        );

        // Fuori fascia (12:00 UTC): ordine nominale, vince A.
        let fuori_peak = Utc.with_ymd_and_hms(2026, 8, 16, 12, 0, 0).unwrap();
        let ordinati = rerank_expected_cost_at(&pool, rows, fuori_peak).await;
        assert_eq!(
            ordinati[0].1, "a-nominale",
            "fuori fascia il listino base comanda: 0.40 < 0.60"
        );
    }
}
