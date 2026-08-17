//! `latency_telemetry`: PUNTO UNICO I/O (regola L) della latenza OSSERVATA per
//! coppia (provider, model), gemello di [`crate::governance_telemetry`] e con
//! lo stesso confine d'inversione: qui SOLO l'I/O (lettura
//! `ai_model_health_history` + config `routing.latency.*`); il CRITERIO — chi
//! sta dentro un budget dichiarato, e la ricaduta a pool svuotato — e' il
//! modulo PURO [`nexus_agent_graph::decisions::latency_budget`].
//!
//! # Fonte e forma della misura
//!
//! La fonte e' lo storico dei probe (`ai_model_health_history.latency_ms`,
//! scritto dal writer unico `model_health_probe::record_model_health`): il
//! ledger nuovo NON ha una durata per chiamata (verificato: nessuna colonna),
//! e aggiungerla e' un'estensione dichiarata di fase 4. Si legge il
//! PERCENTILE (`percentile_cont`), non la media: un outlier singolo sposta la
//! media di un fattore che il percentile assorbe, e la domanda del budget e'
//! «entro quanto arriva DI SOLITO», non «quanto costa in media l'attesa».
//! Contano i soli probe SANI (`healthy AND latency_ms IS NOT NULL`): la
//! latenza di un probe fallito misura il fallimento, non il modello.
//!
//! Regola G: finestra, soglia campioni e percentile sono in `settings`
//! (`routing.latency.*`, mig 0725). I default nel codice sono parametri di
//! calcolo locale (stessa disciplina di `RECENT_WINDOW_DEFAULT` della
//! governance), allineati ai seed della migrazione.
//!
//! FAIL-OPEN: qualunque guasto di lettura -> nessuna osservazione ->
//! `LatencyFit::Unknown` per tutti -> nessuna esclusione (regola Q: chi non
//! sa non esclude), MAI un errore che rompe la selezione.

use std::collections::HashMap;

use sqlx::PgPool;

use nexus_agent_graph::decisions::latency_budget::{
    filtra_per_budget, EsitoBudgetLatenza, LatencyObservation,
};

/// Finestra (ore) dello storico probe considerato. Al ritmo del probe (~30m)
/// 72h valgono ~144 campioni per modello.
const LATENCY_WINDOW_HOURS_SETTING: &str = "routing.latency.window_hours";
/// Campioni minimi perche' il percentile sia una misura e non rumore.
const LATENCY_MIN_SAMPLES_SETTING: &str = "routing.latency.min_samples";
/// Il percentile letto (0..1], es. 0.95.
const LATENCY_PERCENTILE_SETTING: &str = "routing.latency.percentile";

/// Default dei tre parametri (mig 0725). Parametri di calcolo locale, non
/// magic fallback su un modello (regola G non si applica): restano
/// configurabili da DB e i seed della migrazione portano gli stessi valori.
const WINDOW_HOURS_DEFAULT: i64 = 72;
const MIN_SAMPLES_DEFAULT: i64 = 5;
const PERCENTILE_DEFAULT: f64 = 0.95;

/// I tre parametri del criterio, letti dal DB e VALIDATI (un valore fuori
/// intervallo cade sul default, come nella `GovernancePolicy`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatencyPolicy {
    pub window_hours: i64,
    pub min_samples: i64,
    pub percentile: f64,
}

impl Default for LatencyPolicy {
    fn default() -> Self {
        Self {
            window_hours: WINDOW_HOURS_DEFAULT,
            min_samples: MIN_SAMPLES_DEFAULT,
            percentile: PERCENTILE_DEFAULT,
        }
    }
}

// Come in `governance_telemetry`: le letture ritornano `Option` perche' il
// chiamante VALIDA l'intervallo prima di decidere — col default applicato a
// monte un valore fuori intervallo sarebbe indistinguibile da uno assente.

async fn setting_i64(db: &PgPool, key: &str) -> Option<i64> {
    nexus_auth::get_setting(db, key)
        .await
        .and_then(|v| v.trim().parse::<i64>().ok())
}

async fn setting_f64(db: &PgPool, key: &str) -> Option<f64> {
    nexus_auth::get_setting(db, key)
        .await
        .and_then(|v| v.trim().parse::<f64>().ok())
}

/// Costruisce la [`LatencyPolicy`] dai settings DB (cache 60s di nexus-auth).
/// Best-effort: valore assente/malformato/fuori intervallo -> default.
pub async fn load_latency_policy(db: &PgPool) -> LatencyPolicy {
    let def = LatencyPolicy::default();
    let window_hours = setting_i64(db, LATENCY_WINDOW_HOURS_SETTING)
        .await
        .filter(|v| *v > 0)
        .unwrap_or(def.window_hours);
    let min_samples = setting_i64(db, LATENCY_MIN_SAMPLES_SETTING)
        .await
        .filter(|v| *v > 0)
        .unwrap_or(def.min_samples);
    let percentile = setting_f64(db, LATENCY_PERCENTILE_SETTING)
        .await
        .filter(|v| *v > 0.0 && *v <= 1.0)
        .unwrap_or(def.percentile);
    LatencyPolicy {
        window_hours,
        min_samples,
        percentile,
    }
}

/// Riga grezza della query (una per coppia con storico sano in finestra).
#[derive(Debug, sqlx::FromRow)]
struct LatencyRow {
    provider: String,
    model: String,
    p_ms: i64,
    samples: i64,
}

/// La query del percentile: UNNEST delle coppie candidate, `percentile_cont`
/// sui probe SANI in finestra. Costante NOMINATA accanto alla funzione, cosi'
/// la mutazione del test (`percentile_cont` -> media) ha un bersaglio unico e
/// senza ambiguita'.
const SQL_PERCENTILE_LATENZA: &str = "WITH cand AS ( \
         SELECT provider, model FROM UNNEST($1::text[], $2::text[]) AS c(provider, model) \
     ) \
     SELECT h.provider AS provider, h.model AS model, \
            (percentile_cont($3) WITHIN GROUP (ORDER BY h.latency_ms))::bigint AS p_ms, \
            COUNT(*)::bigint AS samples \
       FROM ai_model_health_history h \
       JOIN cand ON cand.provider = h.provider AND cand.model = h.model \
      WHERE h.healthy AND h.latency_ms IS NOT NULL \
        AND h.checked_at > NOW() - make_interval(hours => $4::int) \
      GROUP BY h.provider, h.model";

/// Carica la latenza osservata per l'insieme di coppie `(provider, model)`:
/// una query batch con `percentile_cont(policy.percentile)` sui probe SANI in
/// finestra. Le coppie senza storico non compaiono nella mappa (a valle:
/// `LatencyFit::Unknown`, che non esclude — regola Q).
///
/// FAIL-OPEN: errore SQL -> mappa vuota con warn (nessuna esclusione).
pub async fn load_latency_observations(
    db: &PgPool,
    candidates: &[(String, String)],
    policy: &LatencyPolicy,
) -> HashMap<(String, String), LatencyObservation> {
    if candidates.is_empty() {
        return HashMap::new();
    }
    let providers: Vec<String> = candidates.iter().map(|(p, _)| p.clone()).collect();
    let models: Vec<String> = candidates.iter().map(|(_, m)| m.clone()).collect();
    let rows: Vec<LatencyRow> = match sqlx::query_as::<_, LatencyRow>(SQL_PERCENTILE_LATENZA)
        .bind(&providers)
        .bind(&models)
        .bind(policy.percentile)
        .bind(policy.window_hours.min(i64::from(i32::MAX)) as i32)
        .fetch_all(db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "latency_telemetry: lettura latenze fallita, fail-open (nessuna esclusione)"
            );
            return HashMap::new();
        }
    };
    rows.into_iter()
        .map(|r| {
            let oss = LatencyObservation {
                p_ms: r.p_ms,
                samples: r.samples,
            };
            ((r.provider, r.model), oss)
        })
        .collect()
}

/// Applica il budget dichiarato a un POOL di candidati `(provider, model,
/// tier)`: carica policy e osservazioni, delega il verdetto al criterio puro
/// ([`filtra_per_budget`]) e DICHIARA l'esito nei log — la ricaduta a pool
/// pieno e' un warn con i numeri (regola M).
pub(crate) async fn applica_budget_latenza(
    db: &PgPool,
    rows: Vec<(String, String, Option<String>)>,
    budget_ms: i64,
) -> (Vec<(String, String, Option<String>)>, EsitoBudgetLatenza) {
    if rows.is_empty() {
        return (rows, EsitoBudgetLatenza::Filtrato { esclusi: 0 });
    }
    let policy = load_latency_policy(db).await;
    let pairs: Vec<(String, String)> = rows
        .iter()
        .map(|(p, m, _)| (p.clone(), m.clone()))
        .collect();
    let osservazioni = load_latency_observations(db, &pairs, &policy).await;
    let per_riga: Vec<Option<LatencyObservation>> =
        pairs.iter().map(|k| osservazioni.get(k).copied()).collect();
    let filtro = filtra_per_budget(&per_riga, budget_ms, policy.min_samples);
    match filtro.esito {
        EsitoBudgetLatenza::RicadutaPoolPieno { oltre_budget } => tracing::warn!(
            budget_ms,
            oltre_budget,
            pool = rows.len(),
            "latency_budget: TUTTI i candidati osservati eccedono il budget: \
             si serve il pool intero, col segnale nel rationale (mai fail-closed)"
        ),
        EsitoBudgetLatenza::Filtrato { esclusi } if esclusi > 0 => tracing::info!(
            budget_ms,
            esclusi,
            pool = rows.len(),
            "latency_budget: candidati oltre il budget esclusi dal pool"
        ),
        EsitoBudgetLatenza::Filtrato { .. } => {}
    }
    let mut per_indice: Vec<Option<(String, String, Option<String>)>> =
        rows.into_iter().map(Some).collect();
    let kept = filtro
        .keep
        .into_iter()
        .map(|i| per_indice[i].take().expect("indice usato una volta sola"))
        .collect();
    (kept, filtro.esito)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_health_probe::record_model_health;
    use nexus_agent_graph::decisions::latency_budget::{latency_fit, LatencyFit};

    /// Test 5 del design: il PERCENTILE regge l'outlier che sposterebbe la
    /// media. Storico seminato dal WRITER di produzione (regola O) e policy
    /// letta dal produttore reale (`load_latency_policy` sui seed della mig
    /// 0725: finestra 72h, 5 campioni, p95).
    ///
    /// I numeri, ricalcolati rispetto al design (che con 19+1 campioni non
    /// discriminava: a n=20 il peso dell'outlier nella media, 1/20, coincide
    /// con la frazione d'interpolazione del p95, 0.05): 20 probe a 1000ms +
    /// 1 outlier a 60000ms -> p95 = 1000 (indice 0.95*20 = 19, tutto sotto
    /// l'outlier), MEDIA = 3809. Col budget 3000ms il p95 passa, la media
    /// avrebbe escluso.
    ///
    /// MUTAZIONE (eseguita davvero, vedi commit): `percentile_cont($3)` che
    /// diventa `AVG(h.latency_ms)` -> p_ms = 3809 -> `Exceeds`, il test
    /// rosseggia su entrambe le asserzioni finali.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_percentile_regge_l_outlier_che_sposterebbe_la_media(pool: PgPool) {
        for _ in 0..20 {
            record_model_health(&pool, "prova", "m-lento-a-volte", true, Some(1_000), None, None)
                .await;
        }
        record_model_health(&pool, "prova", "m-lento-a-volte", true, Some(60_000), None, None)
            .await;
        // Un probe FALLITO con latenza enorme: non deve entrare nel conto
        // (la latenza di un fallimento misura il fallimento, non il modello).
        record_model_health(
            &pool,
            "prova",
            "m-lento-a-volte",
            false,
            Some(999_999),
            Some("timeout"),
            None,
        )
        .await;

        let policy = load_latency_policy(&pool).await;
        assert_eq!(
            policy,
            LatencyPolicy { window_hours: 72, min_samples: 5, percentile: 0.95 },
            "la policy viene dai seed della mig 0725"
        );
        let chiave = ("prova".to_string(), "m-lento-a-volte".to_string());
        let oss = load_latency_observations(&pool, std::slice::from_ref(&chiave), &policy).await;
        let o = oss.get(&chiave).expect("osservazione presente");
        assert_eq!(o.samples, 21, "i soli probe SANI contano: il fallito resta fuori");
        assert_eq!(
            o.p_ms, 1_000,
            "p95 di 20x1000 + 1x60000 = 1000; la MEDIA sarebbe 3809 — e' il \
             discriminante della mutazione percentile->media"
        );
        // Fino alla CONSEGUENZA (regola O), non alla stringa: col budget 3000
        // il verdetto e' Fits; la media (3809) avrebbe dato Exceeds.
        assert_eq!(
            latency_fit(Some(o), 3_000, policy.min_samples),
            LatencyFit::Fits
        );
    }

    /// Le coppie senza storico non compaiono nella mappa: a valle il criterio
    /// le tratta come `Unknown` (non escluse, regola Q). E la finestra taglia
    /// lo storico VECCHIO: una misura fuori finestra non e' una misura.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn fuori_finestra_e_mai_osservato_non_producono_misure(pool: PgPool) {
        // 6 probe sani, poi retrodatati OLTRE la finestra di 72h (iniezione
        // dell'istante: la forma della riga resta quella del produttore).
        for _ in 0..6 {
            record_model_health(&pool, "prova", "m-vecchio", true, Some(1_000), None, None).await;
        }
        sqlx::query(
            "UPDATE ai_model_health_history \
             SET checked_at = NOW() - make_interval(hours => 100) \
             WHERE provider = 'prova' AND model = 'm-vecchio'",
        )
        .execute(&pool)
        .await
        .expect("retrodatazione");

        let policy = load_latency_policy(&pool).await;
        let candidati = vec![
            ("prova".to_string(), "m-vecchio".to_string()),
            ("prova".to_string(), "m-mai-visto".to_string()),
        ];
        let oss = load_latency_observations(&pool, &candidati, &policy).await;
        assert!(
            oss.is_empty(),
            "storico fuori finestra e coppia mai osservata: nessuna misura, {oss:?}"
        );
    }
}
