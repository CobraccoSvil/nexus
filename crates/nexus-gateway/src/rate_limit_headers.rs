//! Sensore degli header di rate limit dichiarati dai fornitori.
//!
//! I fornitori dichiarano su OGNI risposta HTTP quanto resta dei propri bucket
//! (richieste e token) e quando si azzerano. Il sistema finora buttava quel
//! segnale e scopriva la saturazione solo dal 429: questo modulo lo LEGGE
//! (parser puro sui nomi reali dei tre dialetti osservati), lo tiene in un
//! registro in-process e lo persiste a intervallo su
//! `nexus_rate_limit_observations` (mig 0718).
//!
//! SOLO SENSORE: nessuna decisione automatica legge queste osservazioni in
//! questa fase. Collegarle a cooldown/selezione e' fase futura e passa dal
//! punto unico della portata del cooldown ([[portata-cooldown]]): un secondo
//! decisore qui rifarebbe i due registri disallineati del 13/08/2026.
//!
//! Il riconoscimento e' per PRESENZA dell'header, mai per nome del provider
//! (regola M: si legge cio' che c'e' sul wire): mistral usa gli stessi nomi di
//! groq/openai col suffisso `-minute`, e un match sul provider leggerebbe due
//! dialetti dove ce n'e' uno.
//!
//! Semantiche NON uniformi, conservate in `raw`: su groq `limit-requests` e'
//! AL GIORNO mentre `limit-tokens` e' AL MINUTO (MISURATO 13/08/2026, memoria
//! "test diretti provider 13-08") — i numeri normalizzati si leggono col raw
//! accanto, che porta gli header originali cosi' come il wire li ha detti.
//! Anthropic separa input/output tokens: `tokens_*` normalizza la famiglia
//! INPUT (la piu' vincolante in pratica); la famiglia output resta leggibile
//! in `raw`, quindi la scelta non e' distruttiva.

use std::sync::OnceLock;

use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use sqlx::PgPool;

/// Cio' che gli header di rate limit di UNA risposta dichiarano. Campi `None`
/// = header assente (mai 0: l'assenza non e' un limite, regola Q).
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitObservation {
    pub requests_limit: Option<i64>,
    pub requests_remaining: Option<i64>,
    pub requests_reset_at: Option<DateTime<Utc>>,
    pub tokens_limit: Option<i64>,
    pub tokens_remaining: Option<i64>,
    pub tokens_reset_at: Option<DateTime<Utc>>,
    /// Header originali (nome -> valore) per l'audit: la normalizzazione non
    /// butta la fonte.
    pub raw: serde_json::Map<String, serde_json::Value>,
    pub observed_at: DateTime<Utc>,
}

/// I nomi con cui i dialetti osservati dichiarano ciascun campo, in cascata
/// (come `WireUsage::cached_input_tokens`: un dialetto che riusa un nome viene
/// letto senza toccare nulla).
///
/// Famiglie reali:
///   - `x-ratelimit-{limit,remaining,reset}-{requests,tokens}` (groq, openai);
///     mistral con suffisso `-minute`;
///   - `anthropic-ratelimit-{requests,input-tokens}-{limit,remaining,reset}`
///     (reset in RFC3339).
const NOMI_REQUESTS_LIMIT: &[&str] = &[
    "x-ratelimit-limit-requests",
    "x-ratelimit-limit-requests-minute",
    "anthropic-ratelimit-requests-limit",
];
const NOMI_REQUESTS_REMAINING: &[&str] = &[
    "x-ratelimit-remaining-requests",
    "x-ratelimit-remaining-requests-minute",
    "anthropic-ratelimit-requests-remaining",
];
const NOMI_REQUESTS_RESET: &[&str] = &[
    "x-ratelimit-reset-requests",
    "x-ratelimit-reset-requests-minute",
    "anthropic-ratelimit-requests-reset",
];
const NOMI_TOKENS_LIMIT: &[&str] = &[
    "x-ratelimit-limit-tokens",
    "x-ratelimit-limit-tokens-minute",
    "anthropic-ratelimit-input-tokens-limit",
];
const NOMI_TOKENS_REMAINING: &[&str] = &[
    "x-ratelimit-remaining-tokens",
    "x-ratelimit-remaining-tokens-minute",
    "anthropic-ratelimit-input-tokens-remaining",
];
const NOMI_TOKENS_RESET: &[&str] = &[
    "x-ratelimit-reset-tokens",
    "x-ratelimit-reset-tokens-minute",
    "anthropic-ratelimit-input-tokens-reset",
];

/// Parser PURO: legge gli header di rate limit di una risposta. `None` =
/// nessun header di rate limit presente — mai un'osservazione vuota.
///
/// `now` e' un parametro e non `Utc::now()` perche' i reset relativi ("59s")
/// diventano istanti assoluti rispetto al momento dell'osservazione, e i test
/// iniettano l'istante (regola O).
pub fn osserva(
    headers: &reqwest::header::HeaderMap,
    now: DateTime<Utc>,
) -> Option<RateLimitObservation> {
    // Il raw raccoglie OGNI header che parla di rate limit, anche quelli che
    // la normalizzazione non copre (famiglia output-tokens di anthropic,
    // retry-after non incluso: ha gia' il suo lettore in `parse_retry_after`).
    // I nomi di `http::HeaderName` sono gia' minuscoli.
    let mut raw = serde_json::Map::new();
    for (nome, valore) in headers {
        let n = nome.as_str();
        if n.contains("ratelimit") {
            if let Ok(v) = valore.to_str() {
                raw.insert(n.to_string(), serde_json::Value::String(v.to_string()));
            }
        }
    }
    if raw.is_empty() {
        return None;
    }

    let testo = |nomi: &[&str]| -> Option<String> {
        nomi.iter().find_map(|n| {
            headers
                .get(*n)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        })
    };
    let numero = |nomi: &[&str]| testo(nomi).and_then(|s| s.trim().parse::<i64>().ok());

    Some(RateLimitObservation {
        requests_limit: numero(NOMI_REQUESTS_LIMIT),
        requests_remaining: numero(NOMI_REQUESTS_REMAINING),
        requests_reset_at: testo(NOMI_REQUESTS_RESET).and_then(|s| parse_reset(&s, now)),
        tokens_limit: numero(NOMI_TOKENS_LIMIT),
        tokens_remaining: numero(NOMI_TOKENS_REMAINING),
        tokens_reset_at: testo(NOMI_TOKENS_RESET).and_then(|s| parse_reset(&s, now)),
        raw,
        observed_at: now,
    })
}

/// Reset -> istante assoluto. Tre forme reali:
///   - `"2026-08-16T12:00:00Z"` (RFC3339, anthropic) -> parse diretto;
///   - `"30"` (secondi, anche frazionari) -> now + n;
///   - `"2m59.56s"` / `"1s"` / `"6m0s"` (durata stile Go, groq/openai)
///     -> now + durata.
/// Forma ignota -> `None`: un reset non capito non diventa un istante
/// inventato.
fn parse_reset(valore: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let v = valore.trim();
    if v.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(v) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(secondi) = v.parse::<f64>() {
        if secondi.is_finite() && secondi >= 0.0 {
            return Some(now + Duration::milliseconds((secondi * 1000.0).round() as i64));
        }
        return None;
    }
    parse_go_duration(v).map(|d| now + d)
}

/// Durata stile Go: sequenza di coppie numero+unita' (`h`, `m`, `s`, `ms`),
/// numeri anche frazionari. `"2m59.56s"` = 179.56s. `ms` va provato PRIMA di
/// `m`, o "250ms" verrebbe letto come 250 minuti seguiti da una `s` orfana.
fn parse_go_duration(s: &str) -> Option<Duration> {
    let mut totale_ms: f64 = 0.0;
    let mut resto = s;
    let mut almeno_una = false;
    while !resto.is_empty() {
        let fine_num = resto
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(resto.len());
        if fine_num == 0 {
            return None;
        }
        let (num, dopo) = resto.split_at(fine_num);
        let n: f64 = num.parse().ok()?;
        let (moltiplicatore_ms, dopo) = if let Some(r) = dopo.strip_prefix("ms") {
            (1.0, r)
        } else if let Some(r) = dopo.strip_prefix('h') {
            (3_600_000.0, r)
        } else if let Some(r) = dopo.strip_prefix('m') {
            (60_000.0, r)
        } else if let Some(r) = dopo.strip_prefix('s') {
            (1_000.0, r)
        } else {
            return None;
        };
        totale_ms += n * moltiplicatore_ms;
        almeno_una = true;
        resto = dopo;
    }
    if !almeno_una || !totale_ms.is_finite() {
        return None;
    }
    Some(Duration::milliseconds(totale_ms.round() as i64))
}

/// Una voce del registro: l'ultima osservazione della coppia, con la marcatura
/// "sporca" che dice al flusher se e' cambiata dall'ultimo giro.
struct VoceRegistro {
    oss: RateLimitObservation,
    sporca: bool,
}

/// Registro in-process delle osservazioni per coppia (fornitore, modello).
/// Struct e non funzioni sul globale, cosi' i test ne costruiscono uno proprio
/// e il flush testato e' LO STESSO metodo che il task periodico chiama
/// (regola O).
struct RegistroRateLimit {
    voci: DashMap<(String, String), VoceRegistro>,
}

/// L'UPSERT dello snapshot: una riga per coppia, sempre l'ultima osservazione.
const SQL_UPSERT_OSSERVAZIONE: &str = r#"
        INSERT INTO nexus_rate_limit_observations (
            provider, model,
            requests_limit, requests_remaining, requests_reset_at,
            tokens_limit, tokens_remaining, tokens_reset_at,
            raw, observed_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        ON CONFLICT (provider, model) DO UPDATE SET
            requests_limit     = EXCLUDED.requests_limit,
            requests_remaining = EXCLUDED.requests_remaining,
            requests_reset_at  = EXCLUDED.requests_reset_at,
            tokens_limit       = EXCLUDED.tokens_limit,
            tokens_remaining   = EXCLUDED.tokens_remaining,
            tokens_reset_at    = EXCLUDED.tokens_reset_at,
            raw                = EXCLUDED.raw,
            observed_at        = EXCLUDED.observed_at
        "#;

impl RegistroRateLimit {
    fn new() -> Self {
        Self {
            voci: DashMap::new(),
        }
    }

    /// Registra l'ultima osservazione della coppia e la marca da persistere.
    fn registra(&self, provider: &str, model: &str, oss: RateLimitObservation) {
        self.voci.insert(
            (provider.to_string(), model.to_string()),
            VoceRegistro { oss, sporca: true },
        );
    }

    /// Persiste le sole voci cambiate dall'ultimo giro. Ritorna quante righe
    /// ha scritto: e' il segnale su cui il test distingue un flush mirato da
    /// uno incondizionato.
    ///
    /// Un UPSERT per voce e non un batch: le voci cambiate in un giro sono le
    /// coppie ATTIVE nell'intervallo (una manciata), non un elenco che cresce.
    async fn flush(&self, db: &PgPool) -> usize {
        let sporche: Vec<((String, String), RateLimitObservation)> = self
            .voci
            .iter()
            .filter(|voce| voce.value().sporca)
            .map(|voce| (voce.key().clone(), voce.value().oss.clone()))
            .collect();

        let mut scritte = 0usize;
        for ((provider, model), oss) in sporche {
            if !persisti_osservazione(db, &provider, &model, &oss).await {
                continue;
            }
            scritte += 1;
            self.pulisci_se_invariata(&provider, &model, oss.observed_at);
        }
        scritte
    }

    /// Toglie la marcatura sporca SOLO se nel frattempo non e' arrivata
    /// un'osservazione piu' nuova (confronto su `observed_at`): senza, una
    /// registrazione concorrente al flush verrebbe marcata pulita senza essere
    /// mai stata scritta.
    fn pulisci_se_invariata(&self, provider: &str, model: &str, scritta_a: DateTime<Utc>) {
        let chiave = (provider.to_string(), model.to_string());
        if let Some(mut voce) = self.voci.get_mut(&chiave) {
            if voce.oss.observed_at == scritta_a {
                voce.sporca = false;
            }
        }
    }
}

/// L'UPSERT di UNA osservazione. Best-effort: un sensore che non riesce a
/// persistere non interrompe nulla, ma lo dice — la voce resta sporca e il
/// prossimo giro ritenta. Ritorna `true` se la riga e' stata scritta.
async fn persisti_osservazione(
    db: &PgPool,
    provider: &str,
    model: &str,
    oss: &RateLimitObservation,
) -> bool {
    let esito = sqlx::query(SQL_UPSERT_OSSERVAZIONE)
        .bind(provider)
        .bind(model)
        .bind(oss.requests_limit)
        .bind(oss.requests_remaining)
        .bind(oss.requests_reset_at)
        .bind(oss.tokens_limit)
        .bind(oss.tokens_remaining)
        .bind(oss.tokens_reset_at)
        .bind(serde_json::Value::Object(oss.raw.clone()))
        .bind(oss.observed_at)
        .execute(db)
        .await;
    match esito {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(error = %e, provider = %provider,
                "rate-limit: upsert osservazione fallito (best-effort)");
            false
        }
    }
}

/// Il registro globale del processo, come le altre cache del crate.
static REGISTRO: OnceLock<RegistroRateLimit> = OnceLock::new();

fn registro() -> &'static RegistroRateLimit {
    REGISTRO.get_or_init(RegistroRateLimit::new)
}

/// Registra nell'unico registro del processo l'osservazione di una risposta.
/// La chiamano gli adapter subito dopo la `send()`, ANCHE sulle risposte
/// non-2xx: un 429 porta gli header piu' informativi.
pub fn registra(provider: &str, model: &str, oss: RateLimitObservation) {
    registro().registra(provider, model, oss);
}

/// Chiave del setting che governa l'intervallo dello snapshot (mig 0718).
const INTERVAL_SETTING: &str = "gateway.rate_limit_snapshot_interval_s";

/// Intervallo dello snapshot dal DB. `0` = snapshot disattivo. Setting assente
/// o illeggibile -> disattivo: se la mig 0718 non e' applicata non esiste
/// nemmeno la tabella su cui scrivere, e uno snapshot "di default" fallirebbe
/// a ogni giro — il degrado e' spento e dichiarato, non un numero inventato
/// (regola G).
async fn intervallo_snapshot(db: &PgPool) -> u64 {
    match nexus_auth::get_setting(db, INTERVAL_SETTING).await {
        Some(v) => v.trim().parse::<u64>().unwrap_or_else(|_| {
            tracing::warn!(valore = %v, "rate-limit: {INTERVAL_SETTING} non numerico, snapshot disattivo");
            0
        }),
        None => 0,
    }
}

/// Task periodico: persiste le voci cambiate ogni
/// `gateway.rate_limit_snapshot_interval_s` secondi. Con il setting a 0 lo
/// snapshot resta spento ma il task RICONTROLLA il DB una volta al minuto:
/// la riattivazione e' una UPDATE, non un riavvio (regola G).
pub fn spawn_snapshot_flusher(db: PgPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let intervallo = intervallo_snapshot(&db).await;
            if intervallo == 0 {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                continue;
            }
            tokio::time::sleep(std::time::Duration::from_secs(intervallo)).await;
            let scritte = registro().flush(&db).await;
            if scritte > 0 {
                tracing::debug!(scritte, "rate-limit: snapshot osservazioni persistito");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

    fn istante() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-16T10:00:00Z")
            .expect("istante fisso")
            .with_timezone(&Utc)
    }

    /// Le tre forme reali del reset, coi campioni verbatim dei fornitori.
    ///
    /// MUTAZIONE: trattando `"2m59.56s"` come secondi (saltando il parser di
    /// durata) l'assert sull'istante cade — 179.56s non e' 2.59s ne' un parse
    /// fallito.
    #[test]
    fn parse_reset_legge_le_tre_forme_reali() {
        let now = istante();
        // Durata stile Go (groq/openai).
        assert_eq!(
            parse_reset("2m59.56s", now),
            Some(now + Duration::milliseconds(179_560))
        );
        assert_eq!(parse_reset("1s", now), Some(now + Duration::seconds(1)));
        assert_eq!(parse_reset("6m0s", now), Some(now + Duration::seconds(360)));
        assert_eq!(
            parse_reset("250ms", now),
            Some(now + Duration::milliseconds(250))
        );
        // RFC3339 (anthropic).
        assert_eq!(
            parse_reset("2026-08-16T12:00:00Z", now),
            Some(
                DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
                    .expect("rfc3339")
                    .with_timezone(&Utc)
            )
        );
        // Secondi nudi.
        assert_eq!(parse_reset("30", now), Some(now + Duration::seconds(30)));
        // Forma ignota: nessun istante inventato.
        assert_eq!(parse_reset("fra poco", now), None);
        assert_eq!(parse_reset("", now), None);
        assert_eq!(parse_reset("12x", now), None);
    }

    fn headers_da(coppie: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (nome, valore) in coppie {
            h.insert(
                HeaderName::from_bytes(nome.as_bytes()).expect("nome header"),
                HeaderValue::from_str(valore).expect("valore header"),
            );
        }
        h
    }

    /// I tre dialetti reali, riconosciuti per PRESENZA dell'header e mai per
    /// nome del provider: la stessa funzione, senza sapere chi ha risposto.
    ///
    /// MUTAZIONE: facendo dipendere il riconoscimento dal nome del provider
    /// (un parametro in piu' con un match), il caso mistral — stessi campi,
    /// suffisso `-minute` — rosseggia, perche' nessun match lo nominerebbe.
    #[test]
    fn osserva_riconosce_i_tre_dialetti_dagli_header() {
        let now = istante();

        // groq/openai: nomi base, reset a durata.
        let groq = osserva(
            &headers_da(&[
                ("x-ratelimit-limit-requests", "14400"),
                ("x-ratelimit-remaining-requests", "14399"),
                ("x-ratelimit-reset-requests", "6m0s"),
                ("x-ratelimit-limit-tokens", "8000"),
                ("x-ratelimit-remaining-tokens", "7521"),
                ("x-ratelimit-reset-tokens", "2m59.56s"),
            ]),
            now,
        )
        .expect("osservazione groq");
        assert_eq!(groq.requests_limit, Some(14_400));
        assert_eq!(groq.tokens_remaining, Some(7_521));
        assert_eq!(
            groq.tokens_reset_at,
            Some(now + Duration::milliseconds(179_560))
        );
        assert_eq!(groq.raw.len(), 6, "il raw conserva TUTTI gli header letti");

        // mistral: stessi campi col suffisso -minute.
        let mistral = osserva(
            &headers_da(&[
                ("x-ratelimit-limit-tokens-minute", "2000000"),
                ("x-ratelimit-remaining-tokens-minute", "1999000"),
                ("x-ratelimit-reset-tokens-minute", "60"),
            ]),
            now,
        )
        .expect("osservazione mistral");
        assert_eq!(mistral.tokens_limit, Some(2_000_000));
        assert_eq!(mistral.tokens_remaining, Some(1_999_000));
        assert_eq!(mistral.tokens_reset_at, Some(now + Duration::seconds(60)));
        assert_eq!(mistral.requests_limit, None, "assente = None, mai 0");

        // anthropic: prefisso proprio, tokens normalizzati sulla famiglia
        // INPUT, reset RFC3339; la famiglia output resta nel raw.
        let anthropic = osserva(
            &headers_da(&[
                ("anthropic-ratelimit-requests-limit", "4000"),
                ("anthropic-ratelimit-requests-remaining", "3999"),
                ("anthropic-ratelimit-requests-reset", "2026-08-16T10:01:00Z"),
                ("anthropic-ratelimit-input-tokens-limit", "400000"),
                ("anthropic-ratelimit-input-tokens-remaining", "399500"),
                ("anthropic-ratelimit-input-tokens-reset", "2026-08-16T10:01:00Z"),
                ("anthropic-ratelimit-output-tokens-limit", "80000"),
            ]),
            now,
        )
        .expect("osservazione anthropic");
        assert_eq!(anthropic.requests_limit, Some(4_000));
        assert_eq!(anthropic.tokens_limit, Some(400_000));
        assert_eq!(
            anthropic.requests_reset_at,
            Some(now + Duration::seconds(60))
        );
        assert!(
            anthropic
                .raw
                .contains_key("anthropic-ratelimit-output-tokens-limit"),
            "la famiglia output non normalizzata resta leggibile nel raw"
        );

        // Nessun header di rate limit: nessuna osservazione, mai una vuota.
        assert_eq!(
            osserva(&headers_da(&[("content-type", "application/json")]), now),
            None
        );
    }

    fn osservazione(now: DateTime<Utc>, tokens_remaining: i64) -> RateLimitObservation {
        RateLimitObservation {
            requests_limit: Some(100),
            requests_remaining: Some(99),
            requests_reset_at: None,
            tokens_limit: Some(8_000),
            tokens_remaining: Some(tokens_remaining),
            tokens_reset_at: Some(now + Duration::seconds(60)),
            raw: serde_json::json!({"x-ratelimit-remaining-tokens": tokens_remaining.to_string()})
                .as_object()
                .expect("oggetto")
                .clone(),
            observed_at: now,
        }
    }

    /// Il flusher persiste le SOLE voci cambiate: due registrate -> 2 righe;
    /// ri-flush senza registrazioni nuove -> 0 scritture; una nuova
    /// osservazione -> 1 UPDATE con i valori nuovi.
    ///
    /// Il flush testato e' lo stesso metodo che il task periodico chiama
    /// (regola O: il registro e' una struct, il globale solo un'istanza).
    ///
    /// MUTAZIONE: rendendo il flush incondizionato (ignorare `sporca`) il
    /// secondo assert cade con 2 invece di 0.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_flusher_scrive_solo_le_voci_cambiate(pool: PgPool) {
        let registro = RegistroRateLimit::new();
        let t0 = istante();

        registro.registra("groq", "openai/gpt-oss-20b", osservazione(t0, 7_000));
        registro.registra("mistral", "mistral-small-latest", osservazione(t0, 1_500));
        assert_eq!(registro.flush(&pool).await, 2, "primo giro: due voci nuove");

        let righe: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM nexus_rate_limit_observations")
                .fetch_one(&pool)
                .await
                .expect("conteggio");
        assert_eq!(righe, 2);

        // Nessuna registrazione nuova: il giro non scrive nulla.
        assert_eq!(
            registro.flush(&pool).await,
            0,
            "senza cambi lo snapshot non tocca il DB"
        );

        // Un'osservazione nuova sulla stessa coppia: UN update, valori nuovi.
        let t1 = t0 + Duration::seconds(30);
        registro.registra("groq", "openai/gpt-oss-20b", osservazione(t1, 6_200));
        assert_eq!(registro.flush(&pool).await, 1);
        let (remaining, observed_at): (i64, DateTime<Utc>) = sqlx::query_as(
            "SELECT tokens_remaining, observed_at FROM nexus_rate_limit_observations \
              WHERE provider = 'groq' AND model = 'openai/gpt-oss-20b'",
        )
        .fetch_one(&pool)
        .await
        .expect("riga groq");
        assert_eq!(remaining, 6_200);
        assert_eq!(observed_at, t1);

        // Le righe restano due: l'UPSERT aggiorna, non accumula.
        let righe: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM nexus_rate_limit_observations")
                .fetch_one(&pool)
                .await
                .expect("conteggio");
        assert_eq!(righe, 2);
    }
}
