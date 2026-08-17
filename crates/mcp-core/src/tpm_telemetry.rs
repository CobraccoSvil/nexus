//! `tpm_telemetry`: PUNTO UNICO I/O (regola L) della capienza TPM osservata per
//! coppia (provider, model), gemello di [`crate::latency_telemetry`] e con lo
//! stesso confine d'inversione: qui SOLO l'I/O (lettura
//! `nexus_rate_limit_observations` + config `routing.tpm_*`); il CRITERIO — chi
//! e' oltre il limite, chi ha il residuo scarso, e la ricaduta a pool svuotato
//! — e' il modulo PURO [`nexus_agent_graph::decisions::capienza_tpm`].
//!
//! # Fonte
//!
//! `nexus_rate_limit_observations` (mig 0718), chiave `(provider, model)`, una
//! riga per coppia sempre con l'ULTIMA osservazione. Scrittore unico: il
//! flusher del gateway, che persiste a intervallo le sole voci cambiate. Quel
//! sensore nacque dichiarando «solo telemetria, nessun consumatore decisionale
//! in questa fase»: questa e' la fase in cui il consumatore arriva.
//!
//! # Perche' la freschezza NON e' un WHERE
//!
//! Il gemello della latenza taglia la finestra in SQL, e li' e' corretto: una
//! misura fuori finestra non e' una misura, e la causa non serve a valle. Qui
//! invece il criterio DISTINGUE «mai osservata» da «osservata troppo tempo fa»
//! ([`MotivoIgnota`]), perche' i due dicono cose diverse su un fornitore — il
//! primo che il sensore non l'ha mai visto, il secondo che l'ha visto e non lo
//! vede piu'. Filtrare per eta' in SQL collasserebbe le due cause in una riga
//! assente, cioe' distruggerebbe l'informazione prima che il criterio possa
//! dichiararla (regola Q).
//!
//! FAIL-OPEN: qualunque guasto di lettura -> nessuna osservazione ->
//! `Ignota{MaiOsservata}` per tutti -> nessuna esclusione e nessuna
//! retrocessione, MAI un errore che rompe la selezione.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use nexus_agent_graph::decisions::capienza_tpm::{
    ordina_per_capienza, EsitoCapienza, OsservazioneTpm,
};

/// Interruttore del criterio. Nasce ACCESO (mig 0735): a fatti ignoti non fa
/// nulla, e dove i fatti ci sono evita un 429 certo.
const TPM_GUARD_ENABLED_SETTING: &str = "routing.tpm_guard_enabled";
/// Oltre questa eta' (secondi) un'osservazione non descrive piu' il minuto
/// corrente, e il criterio la dichiara scaduta invece di deciderci sopra.
const TPM_OBSERVATION_MAX_AGE_SETTING: &str = "routing.tpm_observation_max_age_s";

/// Default della soglia di freschezza (mig 0735). Parametro di calcolo locale,
/// non un magic fallback su un modello (regola G non si applica): resta
/// configurabile da DB e il seed della migrazione porta lo stesso valore.
const OBSERVATION_MAX_AGE_DEFAULT: i64 = 120;

/// I due parametri del criterio, letti dal DB e VALIDATI (un valore fuori
/// intervallo cade sul default, come nella [`crate::latency_telemetry`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TpmPolicy {
    /// `false` = il criterio non nasce: percorso bit-identico allo storico.
    pub enabled: bool,
    pub observation_max_age_s: i64,
}

impl Default for TpmPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            observation_max_age_s: OBSERVATION_MAX_AGE_DEFAULT,
        }
    }
}

/// Costruisce la [`TpmPolicy`] dai settings DB (cache 60s di nexus-auth).
/// Best-effort: valore assente/malformato/non positivo -> default.
///
/// Il DEFAULT dell'interruttore e' `true` — al contrario della governance, che
/// non si accende da sola — perche' qui il comportamento a fatti assenti e'
/// gia' quello storico: senza osservazioni il criterio e' `Ignota` per tutti e
/// non tocca nulla. Un default `false` renderebbe il criterio inerte anche
/// dove i fatti ci sono, che e' esattamente lo stato da cui si esce.
pub async fn load_tpm_policy(db: &PgPool) -> TpmPolicy {
    let def = TpmPolicy::default();
    let enabled =
        nexus_auth::get_bool_setting_or(db, TPM_GUARD_ENABLED_SETTING, def.enabled).await;
    let observation_max_age_s = nexus_auth::get_setting(db, TPM_OBSERVATION_MAX_AGE_SETTING)
        .await
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(def.observation_max_age_s);
    TpmPolicy {
        enabled,
        observation_max_age_s,
    }
}

/// Riga grezza della query (una per coppia con un'osservazione a registro).
#[derive(Debug, sqlx::FromRow)]
struct TpmRow {
    provider: String,
    model: String,
    tokens_limit: Option<i64>,
    tokens_remaining: Option<i64>,
    tokens_reset_at: Option<DateTime<Utc>>,
    observed_at: DateTime<Utc>,
}

/// La lettura batch: UNNEST delle coppie candidate, join sull'ultima
/// osservazione. Nessun filtro sull'eta' (vedi la nota di modulo). Costante
/// NOMINATA accanto alla funzione, cosi' una mutazione di test ha un bersaglio
/// unico e senza ambiguita'.
const SQL_OSSERVAZIONI_TPM: &str = "WITH cand AS ( \
         SELECT provider, model FROM UNNEST($1::text[], $2::text[]) AS c(provider, model) \
     ) \
     SELECT o.provider AS provider, o.model AS model, \
            o.tokens_limit AS tokens_limit, o.tokens_remaining AS tokens_remaining, \
            o.tokens_reset_at AS tokens_reset_at, o.observed_at AS observed_at \
       FROM nexus_rate_limit_observations o \
       JOIN cand ON cand.provider = o.provider AND cand.model = o.model";

/// Carica l'ultima osservazione di rate limit per l'insieme di coppie
/// `(provider, model)`. Le coppie senza riga non compaiono nella mappa (a
/// valle: `Ignota{MaiOsservata}`, che non esclude — regola Q).
///
/// FAIL-OPEN: errore SQL -> mappa vuota con warn (nessuna esclusione).
pub async fn load_tpm_observations(
    db: &PgPool,
    candidates: &[(String, String)],
) -> HashMap<(String, String), OsservazioneTpm> {
    if candidates.is_empty() {
        return HashMap::new();
    }
    let providers: Vec<String> = candidates.iter().map(|(p, _)| p.clone()).collect();
    let models: Vec<String> = candidates.iter().map(|(_, m)| m.clone()).collect();
    let rows: Vec<TpmRow> = match sqlx::query_as::<_, TpmRow>(SQL_OSSERVAZIONI_TPM)
        .bind(&providers)
        .bind(&models)
        .fetch_all(db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "tpm_telemetry: lettura osservazioni rate limit fallita, fail-open (nessuna esclusione)"
            );
            return HashMap::new();
        }
    };
    rows.into_iter()
        .map(|r| {
            let oss = OsservazioneTpm {
                tokens_limit: r.tokens_limit,
                tokens_remaining: r.tokens_remaining,
                tokens_reset_at: r.tokens_reset_at,
                observed_at: r.observed_at,
            };
            ((r.provider, r.model), oss)
        })
        .collect()
}

/// L'esito nei log, coi numeri (regola M): chi legge rifa' il conto invece di
/// dedurlo. La ricaduta e' un warn perche' descrive un parco inadatto alla
/// richiesta; l'esclusione e la retrocessione sono routine.
fn dichiara_esito(esito: EsitoCapienza, richiesta_token: i64, pool: usize) {
    match esito {
        EsitoCapienza::RicadutaPoolPieno { oltre_limite } => tracing::warn!(
            richiesta_token,
            oltre_limite,
            pool,
            "capienza_tpm: TUTTI i candidati dichiarano un tetto TPM sotto la \
             dimensione della richiesta: si serve il pool intero, col segnale nel \
             rationale (il rifiuto per tetto e' un fallimento veloce e gia' \
             gestito, mentre nessun modello ferma il run)"
        ),
        EsitoCapienza::Applicato {
            esclusi,
            retrocessi,
        } if esclusi > 0 || retrocessi > 0 => tracing::info!(
            richiesta_token,
            esclusi,
            retrocessi,
            pool,
            "capienza_tpm: candidati oltre il tetto TPM esclusi, residuo scarso retrocesso"
        ),
        EsitoCapienza::Applicato { .. } => {}
    }
}

/// Applica la capienza dichiarata a un POOL di candidati `(provider, model,
/// tier)`: carica policy e osservazioni, delega il verdetto al criterio puro
/// ([`ordina_per_capienza`]) e DICHIARA l'esito nei log — l'esclusione e la
/// ricaduta portano i numeri (regola M).
///
/// L'istante `adesso` si legge QUI e si passa al criterio: il modulo puro
/// resta golden-abile e non conosce l'orologio.
pub(crate) async fn applica_capienza_tpm(
    db: &PgPool,
    rows: Vec<(String, String, Option<String>)>,
    richiesta_token: i64,
) -> (Vec<(String, String, Option<String>)>, EsitoCapienza) {
    let inerte = EsitoCapienza::Applicato {
        esclusi: 0,
        retrocessi: 0,
    };
    if rows.is_empty() {
        return (rows, inerte);
    }
    let policy = load_tpm_policy(db).await;
    if !policy.enabled {
        return (rows, inerte);
    }
    let pairs: Vec<(String, String)> = rows.iter().map(|(p, m, _)| (p.clone(), m.clone())).collect();
    let osservazioni = load_tpm_observations(db, &pairs).await;
    let per_riga: Vec<Option<OsservazioneTpm>> =
        pairs.iter().map(|k| osservazioni.get(k).copied()).collect();
    let ordine = ordina_per_capienza(
        &per_riga,
        richiesta_token,
        policy.observation_max_age_s,
        Utc::now(),
    );
    dichiara_esito(ordine.esito, richiesta_token, rows.len());
    let mut per_indice: Vec<Option<(String, String, Option<String>)>> =
        rows.into_iter().map(Some).collect();
    let kept = ordine
        .keep
        .into_iter()
        .map(|i| per_indice[i].take().expect("indice usato una volta sola"))
        .collect();
    (kept, ordine.esito)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_agent_graph::decisions::capienza_tpm::{capienza, MotivoIgnota, VerdettoCapienza};

    /// Semina passando dalla catena di produzione INTERA (regola O): gli
    /// header REALI di groq -> il parser del gateway (`osserva`) -> l'UPSERT
    /// unico (`persisti_osservazione`). Ne' la riga ne' l'osservazione sono
    /// costruite a mano: se domani cambia il nome di un header o una colonna,
    /// a rosseggiare e' il test del LETTORE, che e' il punto.
    ///
    /// `eta_s` retrodata l'osservazione rispetto ad adesso: e' l'unico
    /// parametro iniettato, perche' la freschezza e' cio' che il criterio
    /// giudica.
    async fn semina_header_groq(
        pool: &PgPool,
        provider: &str,
        model: &str,
        limite: i64,
        residuo: i64,
        eta_s: i64,
    ) {
        use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
        let mut h = HeaderMap::new();
        for (nome, valore) in [
            ("x-ratelimit-limit-tokens", limite.to_string()),
            ("x-ratelimit-remaining-tokens", residuo.to_string()),
            // Forma reale di groq: durata stile Go.
            ("x-ratelimit-reset-tokens", "59s".to_string()),
        ] {
            h.insert(
                HeaderName::from_static(nome),
                HeaderValue::from_str(&valore).expect("valore header"),
            );
        }
        let osservato_a = Utc::now() - chrono::Duration::seconds(eta_s);
        let oss = nexus_gateway::rate_limit_headers::osserva(&h, osservato_a)
            .expect("gli header di rate limit sono riconosciuti dal parser di produzione");
        assert!(
            nexus_gateway::rate_limit_headers::persisti_osservazione(pool, provider, model, &oss)
                .await,
            "l'UPSERT unico del gateway deve scrivere la riga"
        );
    }

    /// I fatti arrivano fino al criterio: la riga del sensore diventa il
    /// verdetto strutturale coi numeri veri del 17/08.
    ///
    /// MUTAZIONE: se la lettura scambiasse `tokens_limit` con
    /// `tokens_remaining` (le due colonne adiacenti dello stesso UPSERT) il
    /// verdetto diventa `ResiduoInsufficiente` e il test rosseggia.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn l_osservazione_del_sensore_arriva_al_criterio(pool: PgPool) {
        semina_header_groq(&pool, "groq", "openai/gpt-oss-20b", 8_000, 120, 20).await;
        let policy = load_tpm_policy(&pool).await;
        assert_eq!(
            policy,
            TpmPolicy {
                enabled: true,
                observation_max_age_s: 120
            },
            "la policy viene dai seed della mig 0735"
        );
        let chiave = ("groq".to_string(), "openai/gpt-oss-20b".to_string());
        let oss = load_tpm_observations(&pool, std::slice::from_ref(&chiave)).await;
        let o = oss.get(&chiave).expect("osservazione presente");
        assert_eq!(o.tokens_limit, Some(8_000));
        assert_eq!(o.tokens_remaining, Some(120));
        // Fino alla CONSEGUENZA (regola O), non alla riga: 180K contro 8000 e'
        // strutturale.
        assert_eq!(
            capienza(Some(o), 180_000, policy.observation_max_age_s, Utc::now()),
            VerdettoCapienza::OltreIlLimite {
                richiesta: 180_000,
                limite: 8_000
            }
        );
    }

    /// La freschezza NON e' un WHERE: una riga vecchia arriva fino al criterio,
    /// che la dichiara SCADUTA — distinta da una coppia mai osservata.
    ///
    /// MUTAZIONE: aggiungere all'SQL un filtro sull'eta' (come fa il gemello
    /// della latenza) rende le due cause indistinguibili e la prima
    /// asserzione rosseggia.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_riga_vecchia_arriva_al_criterio_che_la_dichiara_scaduta(pool: PgPool) {
        semina_header_groq(&pool, "groq", "vecchio", 8_000, 7_000, 1_800).await;
        let vista = ("groq".to_string(), "vecchio".to_string());
        let mai = ("groq".to_string(), "mai-visto".to_string());
        let oss = load_tpm_observations(&pool, &[vista.clone(), mai.clone()]).await;
        assert!(
            oss.contains_key(&vista),
            "la riga vecchia si legge lo stesso: e' il criterio a giudicarne l'eta'"
        );
        assert!(!oss.contains_key(&mai));
        let adesso = Utc::now();
        assert!(
            matches!(
                capienza(oss.get(&vista), 5_000, 120, adesso),
                VerdettoCapienza::Ignota {
                    motivo: MotivoIgnota::OsservazioneScaduta { .. }
                }
            ),
            "vista e non piu' vista != mai vista"
        );
        assert_eq!(
            capienza(oss.get(&mai), 5_000, 120, adesso),
            VerdettoCapienza::Ignota {
                motivo: MotivoIgnota::MaiOsservata
            }
        );
    }

    /// L'interruttore spegne il criterio: il pool esce identico anche con
    /// un'osservazione che escluderebbe tutti.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn a_flag_spento_il_pool_esce_identico(pool: PgPool) {
        crate::test_support::seed_setting(&pool, TPM_GUARD_ENABLED_SETTING, "false").await;
        semina_header_groq(&pool, "groq", "openai/gpt-oss-20b", 8_000, 120, 20).await;
        let rows = vec![(
            "groq".to_string(),
            "openai/gpt-oss-20b".to_string(),
            Some("light".to_string()),
        )];
        let (out, esito) = applica_capienza_tpm(&pool, rows.clone(), 180_000).await;
        assert_eq!(out, rows, "a flag spento il criterio non nasce");
        assert_eq!(
            esito,
            EsitoCapienza::Applicato {
                esclusi: 0,
                retrocessi: 0
            }
        );
        assert!(esito.segnali().is_empty());
    }
}
