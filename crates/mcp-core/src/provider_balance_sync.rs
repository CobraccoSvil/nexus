//! Worker `provider_balance_sync` — punto unico (regola L) del "quanto credito
//! REALE resta presso il fornitore?" per i fornitori che espongono un endpoint
//! di saldo interrogabile: deepseek (`GET /user/balance`), openrouter
//! (`GET /credits`, con ripiego `GET /auth/key` per le chiavi senza permesso
//! credits) e kimi (`GET /users/me/balance`).
//!
//! Nato come `deepseek_balance_sync` (un solo fornitore); generalizzato con la
//! mig 0719, che aggiunge a `provider_budget_status` le colonne del saldo
//! osservato (`last_known_balance_usd`, `balance_observed_at`,
//! `balance_source`) e semina le righe `openrouter`/`kimi`.
//!
//! Logica per fornitore:
//!   - leggi la API key da `settings` (stessa chiave del registry)
//!   - interroga l'endpoint di saldo (GET gratuita, verificata: memoria
//!     "il credito si puo' chiedere")
//!   - `spent_real = monthly_budget_usd - balance` (clipped a >= 0), piu' il
//!     saldo GREZZO nelle tre colonne dell'osservazione
//!
//! SOLO SENSORE in questa fase: nessun consumatore decide su queste righe oltre
//! a cio' che gia' esisteva (il gate `budget_exhausted` di
//! `provider_health_probe`, che legge `provider_budget_remaining_view` PRIMA di
//! probare). Il pre-check saldo-vs-prenotazione e' fase futura e va progettato
//! a parte: collegare il saldo a cooldown/selezione da qui sarebbe un secondo
//! scrittore di esclusioni (vedi [[portata-cooldown]]).
//!
//! Cadenza default: 15 min (tre GET per giro, trascurabile).

use sqlx::PgPool;
use std::time::Duration;
use tokio::time::sleep;

const MIN_INTERVAL_S: u64 = 60;

/// Endpoint di saldo deepseek: URL intero e non una base, perche' il fornitore
/// lo serve fuori dal prefisso delle completion (invariato dalla prima
/// versione del worker).
const DEEPSEEK_BALANCE_URL: &str = "https://api.deepseek.com/user/balance";

/// Base di default openrouter: STESSO valore del `base_url_default` seminato
/// nel registry (mig 0567). Il setting `openrouter_base_url` vince quando
/// valorizzato — stesso resolver del registry, replicato qui perche' mcp-core
/// non puo' importare il descrittore del gateway.
const OPENROUTER_DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// Base di default kimi: STESSO valore di
/// `nexus-gateway/src/providers/kimi.rs::DEFAULT_BASE_URL` e del registry
/// (mig 0690). Il letterale e' ripetuto CON questo rimando perche' mcp-core
/// non dipende dal crate gateway; il setting `kimi_base_url` vince.
const KIMI_DEFAULT_BASE_URL: &str = "https://api.moonshot.ai/v1";

/// I fornitori che espongono un endpoint di saldo interrogabile. Enum chiuso e
/// non trait: tre varianti note, nessun polimorfismo aperto necessario
/// (regola L, tabella dei meccanismi: variante semplice -> enum + match).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FornitoreConSaldo {
    Deepseek,
    Openrouter,
    Kimi,
}

impl FornitoreConSaldo {
    fn tutti() -> [Self; 3] {
        [Self::Deepseek, Self::Openrouter, Self::Kimi]
    }

    /// Chiave della API key in `settings` (la stessa che il registry dichiara
    /// in `key_setting`: mig 0032/0567/0690).
    fn key_setting(self) -> &'static str {
        match self {
            Self::Deepseek => "deepseek_api_key",
            Self::Openrouter => "openrouter_api_key",
            Self::Kimi => "kimi_api_key",
        }
    }

    fn nome(self) -> &'static str {
        match self {
            Self::Deepseek => "deepseek",
            Self::Openrouter => "openrouter",
            Self::Kimi => "kimi",
        }
    }

    /// Chiave del setting di base URL (None = endpoint fisso, non ribasabile).
    fn base_url_setting(self) -> Option<&'static str> {
        match self {
            Self::Deepseek => None,
            Self::Openrouter => Some("openrouter_base_url"),
            Self::Kimi => Some("kimi_base_url"),
        }
    }
}

/// Da dove viene il numero (regola Q: la provenienza e' un campo).
/// Identificatori inglesi canonici sul DB (regola N); il CHECK della mig 0719
/// replica questo vocabolario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FonteSaldo {
    Endpoint,
    AuthKeyFallback,
}

impl FonteSaldo {
    fn as_db(self) -> &'static str {
        match self {
            Self::Endpoint => "endpoint",
            Self::AuthKeyFallback => "auth_key_fallback",
        }
    }
}

/// L'esito di UNA interrogazione. L'ignoto e' una variante, non un `None`
/// comodo (regola Q): ogni ramo ha un rimedio diverso e il log lo dichiara.
#[derive(Debug, Clone, Copy, PartialEq)]
enum EsitoSaldo {
    /// Il fornitore ha risposto col saldo: si persiste.
    Osservato { balance_usd: f64, fonte: FonteSaldo },
    /// Chiave assente in settings: skip silenzioso (debug), non e' un guasto.
    NonConfigurato,
    /// Status non-2xx senza ripiego previsto: il fornitore ha rifiutato.
    HttpRespinto { status: u16 },
    /// Body 2xx senza il campo atteso: il contratto del wire e' cambiato,
    /// e va detto QUALE campo manca.
    FormaInattesa { campo: &'static str },
    /// La richiesta non ha raggiunto il fornitore (DNS, timeout, TLS) oppure
    /// la config non era leggibile: nessuno status da riportare, e non e' la
    /// stessa cosa di un rifiuto.
    TrasportoFallito,
    /// La base URL configurata punta a una piattaforma che non fattura USD
    /// (kimi_base_url su host diverso da api.moonshot.ai: `.cn` = CNY): il
    /// numero non e' comparabile e NON si scrive. Il controllo e' sulla
    /// CONFIG, mai sul testo di una risposta (regola M).
    ValutaNonComparabile,
}

/// Avvia il worker in background: un giro su tutti i fornitori con endpoint di
/// saldo ogni `interval_s` secondi (minimo 60). `enabled` e' il default del
/// chiamante, scavalcabile via env var.
pub fn spawn_provider_balance_sync(db: PgPool, enabled: bool, interval_s: u64) {
    // Env var rinominate secche da NEXUS_DEEPSEEK_BALANCE_SYNC_* (il modulo e'
    // gia' in deroga alla regola G: nessuna lettura doppia dei vecchi nomi,
    // sarebbe un doppione).
    let enabled = match std::env::var("NEXUS_PROVIDER_BALANCE_SYNC_ENABLED").as_deref() {
        Ok("false") | Ok("0") => false,
        Ok("true") | Ok("1") => true,
        _ => enabled,
    };
    if !enabled {
        tracing::info!("provider_balance_sync: DISABILITATO");
        return;
    }
    let interval_s = std::env::var("NEXUS_PROVIDER_BALANCE_SYNC_INTERVAL_S")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(interval_s)
        .max(MIN_INTERVAL_S);
    tracing::info!("provider_balance_sync: avvio worker (interval={interval_s}s)");
    tokio::spawn(async move {
        // Aspetta 90s al primo avvio per dare tempo agli altri servizi.
        sleep(Duration::from_secs(90)).await;
        loop {
            run_one_sync(&db).await;
            sleep(Duration::from_secs(interval_s)).await;
        }
    });
}

async fn run_one_sync(db: &PgPool) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("provider_balance_sync: client build fallito: {e}");
            return;
        }
    };
    for f in FornitoreConSaldo::tutti() {
        sync_fornitore(db, &client, f).await;
    }
}

/// Un giro su UN fornitore: risolve config, interroga, persiste o dichiara
/// perche' non ha persistito. Ogni variante dell'esito ha il suo log: un
/// worker che tace su un ramo e' un ramo che nessuno scopre mai.
async fn sync_fornitore(db: &PgPool, client: &reqwest::Client, f: FornitoreConSaldo) {
    let esito = esito_per(db, client, f).await;
    let nome = f.nome();
    match esito {
        EsitoSaldo::Osservato { balance_usd, fonte } => {
            match persisti_balance(db, nome, balance_usd, fonte).await {
                Ok(0) => tracing::warn!(
                    "provider_balance_sync: riga assente in provider_budget_status per {nome} \
                     (seed della migrazione dei saldi non applicato?)"
                ),
                Ok(_) => tracing::info!(
                    "provider_balance_sync: {nome} saldo osservato {balance_usd:.2} USD (fonte {})",
                    fonte.as_db()
                ),
                Err(e) => tracing::warn!("provider_balance_sync: UPDATE {nome} fallito: {e}"),
            }
        }
        EsitoSaldo::NonConfigurato => {
            tracing::debug!(
                "provider_balance_sync: {nome} non configurato ({}), skip",
                f.key_setting()
            );
        }
        EsitoSaldo::HttpRespinto { status } => {
            tracing::warn!("provider_balance_sync: {nome} HTTP {status} dall'endpoint di saldo");
        }
        EsitoSaldo::FormaInattesa { campo } => {
            tracing::warn!(
                "provider_balance_sync: {nome} body senza il campo atteso '{campo}': contratto del wire cambiato?"
            );
        }
        EsitoSaldo::TrasportoFallito => {
            tracing::warn!("provider_balance_sync: {nome} endpoint di saldo non raggiunto");
        }
        EsitoSaldo::ValutaNonComparabile => {
            tracing::warn!(
                "provider_balance_sync: {nome} saldo non comparabile in USD \
                 (base URL configurata fuori da api.moonshot.ai), skip"
            );
        }
    }
}

/// Risolve la config (chiave, base URL) e delega a `interroga`. Separata da
/// `sync_fornitore` perche' produce l'esito senza consumarlo.
async fn esito_per(db: &PgPool, client: &reqwest::Client, f: FornitoreConSaldo) -> EsitoSaldo {
    let api_key = match nexus_auth::get_setting_nonempty(db, f.key_setting()).await {
        Ok(Some(k)) => k,
        Ok(None) => return EsitoSaldo::NonConfigurato,
        Err(e) => {
            tracing::warn!(
                "provider_balance_sync: lettura {} fallita: {e}",
                f.key_setting()
            );
            return EsitoSaldo::TrasportoFallito;
        }
    };
    let base_override = match f.base_url_setting() {
        None => None,
        Some(key) => match nexus_auth::get_setting_nonempty(db, key).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("provider_balance_sync: lettura {key} fallita: {e}");
                return EsitoSaldo::TrasportoFallito;
            }
        },
    };
    // Il controllo di currency sta sulla CONFIG risolta, prima di qualunque
    // richiesta: il body di kimi non dichiara la valuta (regola M).
    if f == FornitoreConSaldo::Kimi {
        let base = base_override.as_deref().unwrap_or(KIMI_DEFAULT_BASE_URL);
        if !kimi_fattura_usd(base) {
            return EsitoSaldo::ValutaNonComparabile;
        }
    }
    interroga(client, f, &api_key, base_override.as_deref()).await
}

/// Su `api.moonshot.ai` la piattaforma fattura USD; l'host cinese
/// (`api.moonshot.cn`) fattura CNY e il numero non e' comparabile col resto
/// della tabella. Funzione PURA sul solo host della base URL.
fn kimi_fattura_usd(base_url: &str) -> bool {
    host_di(base_url) == Some("api.moonshot.ai")
}

/// Host di una base URL http(s), senza dipendere da un crate di parsing URL:
/// il taglio e' allo schema e al primo `/`.
fn host_di(base_url: &str) -> Option<&str> {
    let senza_schema = base_url
        .strip_prefix("https://")
        .or_else(|| base_url.strip_prefix("http://"))?;
    senza_schema.split('/').next()
}

/// La risposta HTTP di una GET di saldo, gia' classificata (regola M: lo
/// status e' il segnale, il body si parsa solo sul 2xx).
enum RispostaSaldo {
    Corpo(serde_json::Value),
    Respinta(u16),
    NonRaggiunto,
}

async fn get_saldo_json(client: &reqwest::Client, url: &str, api_key: &str) -> RispostaSaldo {
    // Bearer come per le completion: gli endpoint di saldo dei tre fornitori
    // autenticano con la stessa chiave API.
    let resp = match client
        .get(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("provider_balance_sync: GET {url} fallita: {e}");
            return RispostaSaldo::NonRaggiunto;
        }
    };
    let status = resp.status();
    if !status.is_success() {
        return RispostaSaldo::Respinta(status.as_u16());
    }
    match resp.json().await {
        Ok(v) => RispostaSaldo::Corpo(v),
        Err(e) => {
            tracing::warn!("provider_balance_sync: parse JSON da {url} fallito: {e}");
            RispostaSaldo::NonRaggiunto
        }
    }
}

/// Mappa la risposta HTTP di un endpoint di saldo sull'esito, dato il parser
/// PURO del corpo, la fonte da dichiarare e il campo atteso quando il body non
/// lo porta. Punto unico della mappatura (regola L): prima viveva copiata in
/// tre rami di `interroga`, uno per fornitore.
fn esito_da_risposta(
    risposta: RispostaSaldo,
    parse: fn(&serde_json::Value) -> Option<f64>,
    fonte: FonteSaldo,
    campo: &'static str,
) -> EsitoSaldo {
    match risposta {
        RispostaSaldo::Corpo(body) => match parse(&body) {
            Some(v) => EsitoSaldo::Osservato {
                balance_usd: v,
                fonte,
            },
            None => EsitoSaldo::FormaInattesa { campo },
        },
        RispostaSaldo::Respinta(s) => EsitoSaldo::HttpRespinto { status: s },
        RispostaSaldo::NonRaggiunto => EsitoSaldo::TrasportoFallito,
    }
}

/// Interroga l'endpoint di saldo di UN fornitore. I parser sono PURI e
/// separati (regola O: testabili sui body reali); qui vive il solo I/O e la
/// composizione degli URL.
async fn interroga(
    client: &reqwest::Client,
    f: FornitoreConSaldo,
    api_key: &str,
    base_url: Option<&str>,
) -> EsitoSaldo {
    match f {
        FornitoreConSaldo::Deepseek => esito_da_risposta(
            get_saldo_json(client, DEEPSEEK_BALANCE_URL, api_key).await,
            parse_saldo_deepseek,
            FonteSaldo::Endpoint,
            "balance_infos[currency=USD].total_balance",
        ),
        FornitoreConSaldo::Openrouter => {
            let base = base_url
                .unwrap_or(OPENROUTER_DEFAULT_BASE_URL)
                .trim_end_matches('/');
            let esito = esito_da_risposta(
                get_saldo_json(client, &format!("{base}/credits"), api_key).await,
                parse_saldo_openrouter_credits,
                FonteSaldo::Endpoint,
                "data.total_credits/data.total_usage",
            );
            // Il 403 su /credits e' una variante PREVISTA, non un rifiuto: le
            // chiavi senza permesso credits leggono il residuo dal proprio
            // profilo (/auth/key), e la fonte lo dichiara. Il parser di quel
            // ramo rifiuta `limit` null (chiave senza tetto): non si inventa
            // un saldo infinito.
            if let EsitoSaldo::HttpRespinto { status: 403 } = esito {
                return esito_da_risposta(
                    get_saldo_json(client, &format!("{base}/auth/key"), api_key).await,
                    parse_saldo_openrouter_key,
                    FonteSaldo::AuthKeyFallback,
                    "data.limit_remaining",
                );
            }
            esito
        }
        FornitoreConSaldo::Kimi => {
            let base = base_url.unwrap_or(KIMI_DEFAULT_BASE_URL).trim_end_matches('/');
            esito_da_risposta(
                get_saldo_json(client, &format!("{base}/users/me/balance"), api_key).await,
                parse_saldo_kimi,
                FonteSaldo::Endpoint,
                "data.available_balance",
            )
        }
    }
}

/// deepseek: `balance_infos[currency=USD].total_balance`, stringa decimale.
fn parse_saldo_deepseek(body: &serde_json::Value) -> Option<f64> {
    body.get("balance_infos")?
        .as_array()?
        .iter()
        .find(|info| info.get("currency").and_then(|c| c.as_str()) == Some("USD"))?
        .get("total_balance")?
        .as_str()?
        .parse()
        .ok()
}

/// openrouter /credits: il residuo e' `total_credits - total_usage` — il solo
/// `total_credits` e' il COMPRATO storico, non il disponibile.
fn parse_saldo_openrouter_credits(body: &serde_json::Value) -> Option<f64> {
    let data = body.get("data")?;
    let credits = data.get("total_credits")?.as_f64()?;
    let usage = data.get("total_usage")?.as_f64()?;
    Some(credits - usage)
}

/// openrouter /auth/key: `data.limit_remaining`, valido SOLO se `data.limit`
/// non e' null (limit null = chiave illimitata: nessun residuo da dichiarare).
fn parse_saldo_openrouter_key(body: &serde_json::Value) -> Option<f64> {
    let data = body.get("data")?;
    if data
        .get("limit")
        .map(serde_json::Value::is_null)
        .unwrap_or(true)
    {
        return None;
    }
    data.get("limit_remaining")?.as_f64()
}

/// kimi: `data.available_balance` — comprende cash E voucher, ed e' cio' che
/// il fornitore scala a ogni chiamata (`cash_balance` da solo ignorerebbe i
/// voucher spendibili).
fn parse_saldo_kimi(body: &serde_json::Value) -> Option<f64> {
    body.get("data")?.get("available_balance")?.as_f64()
}

/// Persiste il balance osservato: `spent = max(0, monthly_budget - balance)`,
/// piu' il saldo GREZZO nelle tre colonne dell'osservazione (mig 0719).
/// Ritorna le righe toccate (0 = nessuna riga per quel provider in tabella).
///
/// PERCHE' `notes` SI SOSTITUISCE E NON SI ACCODA. Questa UPDATE gira a ogni
/// giro del worker (default 15 min, minimo 60s) e la sua nota era concatenata
/// con `notes = COALESCE(notes,'') || ' [sync ...]'`: una cella che cresce di
/// ~34 byte per giro, senza alcun limite e senza che nessuno la rilegga mai
/// — la `BudgetRow` del wire admin non seleziona `notes`, ed e' l'unica
/// lettura che quella colonna abbia. MISURATO il 10/08/2026 sul META vivo:
/// 5363 accodamenti della STESSA stringa per 182.499 byte (178 kB) in una sola
/// cella, e deepseek era l'unico provider con `notes` non NULL — cioe' l'unico
/// che passa di qui. La colonna e' un'annotazione per un umano che guarda la
/// riga, non un registro di eventi: il QUANDO e' gia' in `updated_at` e il
/// QUANTO nelle colonne numeriche, quindi la forma corretta e' l'ultimo valore
/// osservato, non la loro storia (regola H: il campo non e' un log).
///
/// Non diventa una colonna tipizzata (regola Q) di proposito: nessun lettore la
/// consulterebbe, e una colonna che nessun `SELECT` tocca e' il difetto che il
/// censimento delle capability ha gia' misurato altrove (20 colonne su 32 senza
/// alcun consumatore). Resta prosa per un umano, e la prosa si sostituisce.
async fn persisti_balance(
    db: &PgPool,
    provider: &str,
    balance: f64,
    fonte: FonteSaldo,
) -> Result<u64, sqlx::Error> {
    let r = sqlx::query(
        r#"UPDATE provider_budget_status
              SET spent_current_period_usd = GREATEST(monthly_budget_usd - $2, 0),
                  last_known_balance_usd = $2,
                  balance_observed_at = NOW(),
                  balance_source = $3,
                  updated_at = NOW(),
                  notes = '[sync ' || $1 || ' api: balance=' || $4 || ']'
            WHERE provider = $1"#,
    )
    .bind(provider)
    .bind(balance)
    .bind(fonte.as_db())
    .bind(format!("{balance:.2}"))
    .execute(db)
    .await?;
    Ok(r.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Parser puri sui body REALI dei fornitori (regola O: fixture verbatim
    //    dal wire, fonte annotata accanto a ciascuna) ──────────────────────

    /// Body reale di `GET https://api.deepseek.com/user/balance`.
    ///
    /// MUTAZIONE che lo fa rosseggiare: leggere `granted_balance` invece di
    /// `total_balance`, o smettere di filtrare per currency USD.
    #[test]
    fn parse_deepseek_legge_il_total_balance_usd() {
        let body: serde_json::Value = serde_json::from_str(
            r#"{"is_available":true,"balance_infos":[{"currency":"USD","total_balance":"12.34","granted_balance":"0.00","topped_up_balance":"12.34"}]}"#,
        )
        .unwrap();
        assert_eq!(parse_saldo_deepseek(&body), Some(12.34));
        assert_eq!(parse_saldo_deepseek(&serde_json::json!({})), None);
    }

    /// Body reale di `GET https://openrouter.ai/api/v1/credits`: il residuo e'
    /// la DIFFERENZA fra comprato e consumato.
    ///
    /// MUTAZIONE che lo fa rosseggiare: leggere `total_credits` senza
    /// sottrarre `total_usage` — l'assert vedrebbe 25.0, cioe' il comprato
    /// storico spacciato per disponibile.
    #[test]
    fn parse_openrouter_credits_sottrae_il_consumato() {
        let body: serde_json::Value =
            serde_json::from_str(r#"{"data":{"total_credits":25.0,"total_usage":14.6}}"#).unwrap();
        let saldo = parse_saldo_openrouter_credits(&body).expect("saldo osservato");
        assert!((saldo - 10.4).abs() < 1e-9, "atteso 10.4, ottenuto {saldo}");
        assert_eq!(
            parse_saldo_openrouter_credits(&serde_json::json!({"data":{"total_credits":25.0}})),
            None,
            "senza total_usage non si inventa un residuo"
        );
    }

    /// Body reale di `GET https://openrouter.ai/api/v1/auth/key` (il ripiego
    /// delle chiavi senza permesso credits). `limit` null = chiave senza
    /// tetto: NON si inventa un saldo infinito.
    ///
    /// MUTAZIONE che lo fa rosseggiare: ignorare `limit` e leggere comunque
    /// `limit_remaining` — il caso null tornerebbe comunque None per assenza
    /// del numero, ma il contratto "il limit decide" cadrebbe sulla fixture
    /// con limit null e limit_remaining VALORIZZATO qui sotto.
    #[test]
    fn parse_openrouter_key_rispetta_il_limit_null() {
        let body: serde_json::Value = serde_json::from_str(
            r#"{"data":{"limit":20.0,"usage":9.6,"limit_remaining":10.4}}"#,
        )
        .unwrap();
        let saldo = parse_saldo_openrouter_key(&body).expect("saldo osservato");
        assert!((saldo - 10.4).abs() < 1e-9);
        // Chiave illimitata: limit null. Il limit_remaining che alcune
        // risposte portano comunque NON va letto: non e' un saldo.
        let illimitata: serde_json::Value = serde_json::from_str(
            r#"{"data":{"limit":null,"usage":9.6,"limit_remaining":90.4}}"#,
        )
        .unwrap();
        assert_eq!(parse_saldo_openrouter_key(&illimitata), None);
    }

    /// Body reale di `GET https://api.moonshot.ai/v1/users/me/balance`. Il
    /// campo giusto e' `available_balance` (cash + voucher spendibili).
    ///
    /// MUTAZIONE che lo fa rosseggiare: leggere `cash_balance` — la seconda
    /// fixture (voucher > 0, available != cash) divergerebbe.
    #[test]
    fn parse_kimi_legge_available_e_non_cash() {
        let body: serde_json::Value = serde_json::from_str(
            r#"{"code":0,"data":{"available_balance":49.5,"voucher_balance":0.0,"cash_balance":49.5},"status":true}"#,
        )
        .unwrap();
        assert_eq!(parse_saldo_kimi(&body), Some(49.5));
        let con_voucher: serde_json::Value = serde_json::from_str(
            r#"{"code":0,"data":{"available_balance":50.0,"voucher_balance":10.0,"cash_balance":40.0},"status":true}"#,
        )
        .unwrap();
        assert_eq!(parse_saldo_kimi(&con_voucher), Some(50.0));
    }

    /// Il controllo di currency di kimi sta sulla CONFIG (host della base
    /// URL), mai sul testo di una risposta (regola M).
    #[test]
    fn kimi_fattura_usd_solo_su_moonshot_ai() {
        assert!(kimi_fattura_usd("https://api.moonshot.ai/v1"));
        assert!(kimi_fattura_usd("https://api.moonshot.ai/v1/"));
        assert!(!kimi_fattura_usd("https://api.moonshot.cn/v1"));
        assert!(!kimi_fattura_usd("ftp://api.moonshot.ai/v1"));
    }

    /// Il ripiego 403 di openrouter, attraverso `interroga` contro un server
    /// finto su porta effimera (stesso pattern del test
    /// `complete_allinea_il_registro_locale...` in nexus_gateway.rs):
    /// `/credits` risponde 403, `/auth/key` 200, e l'esito dichiara la fonte
    /// del ripiego.
    ///
    /// MUTAZIONE che lo fa rosseggiare: trattare il 403 come `HttpRespinto`
    /// (togliere il ramo di ripiego da `interroga`) — l'assert vede il rifiuto
    /// al posto dell'osservazione.
    #[tokio::test]
    async fn interroga_openrouter_ripiega_su_auth_key_sul_403() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("porta effimera");
        let porta = listener.local_addr().expect("indirizzo").port();
        let finto = tokio::spawn(async move {
            // Due richieste sequenziali (Connection: close): /credits e poi
            // /auth/key. Il terminatore di riga di HTTP e' parte del
            // protocollo, non dei fine-riga di questo file.
            const CRLF: &str = "\r\n";
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.expect("connessione");
                let mut buf = [0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let richiesta = String::from_utf8_lossy(&buf[..n]).to_string();
                let (status, corpo) = if richiesta.starts_with("GET /credits") {
                    (
                        "HTTP/1.1 403 Forbidden",
                        r#"{"error":{"message":"This key does not have permission to access credits","code":403}}"#,
                    )
                } else {
                    (
                        "HTTP/1.1 200 OK",
                        r#"{"data":{"limit":20.0,"usage":9.6,"limit_remaining":10.4}}"#,
                    )
                };
                let intestazioni = [
                    status,
                    "Content-Type: application/json",
                    &format!("Content-Length: {}", corpo.len()),
                    "Connection: close",
                    "",
                    "",
                ]
                .join(CRLF);
                let _ = socket.write_all(intestazioni.as_bytes()).await;
                let _ = socket.write_all(corpo.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let base = format!("http://127.0.0.1:{porta}");
        let esito = interroga(
            &client,
            FornitoreConSaldo::Openrouter,
            "chiave-di-prova",
            Some(&base),
        )
        .await;
        let _ = finto.await;

        match esito {
            EsitoSaldo::Osservato { balance_usd, fonte } => {
                assert!((balance_usd - 10.4).abs() < 1e-9);
                assert_eq!(fonte, FonteSaldo::AuthKeyFallback);
            }
            altro => panic!("atteso Osservato via auth_key_fallback, ottenuto {altro:?}"),
        }
    }

    /// Le righe seminate dalla mig 0719 esistono dopo le migrazioni: protegge
    /// il ramo `rows_affected == 0` del worker (che senza seed sarebbe la
    /// norma per i due fornitori nuovi, non un'anomalia).
    ///
    /// MUTAZIONE che lo fa rosseggiare: togliere l'INSERT di seed dalla 0719.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn seed_0719_semina_openrouter_e_kimi(db: PgPool) {
        let presenti: Vec<String> = sqlx::query_scalar(
            "SELECT provider FROM provider_budget_status WHERE provider IN ('openrouter','kimi') ORDER BY provider",
        )
        .fetch_all(&db)
        .await
        .expect("righe seminate");
        assert_eq!(presenti, vec!["kimi".to_string(), "openrouter".to_string()]);
    }

    /// Il worker gira in continuazione: la nota dev'essere STABILE nel tempo,
    /// non crescente. Il test attraversa la stessa funzione che il worker
    /// chiama (regola O): la crescita viveva nella statement SQL, quindi un
    /// test su un helper che compone la stringa non avrebbe potuto vederla.
    ///
    /// Con la generalizzazione (mig 0719) misura anche: le colonne
    /// dell'osservazione scritte accanto allo spent, e l'ISOLAMENTO per
    /// provider — il giro su openrouter non deve toccare la riga deepseek.
    ///
    /// MUTAZIONI che lo fanno rosseggiare: rimettere `notes =
    /// COALESCE(notes,'') || ' [sync ...]'` (la lunghezza dopo il terzo giro
    /// triplica); togliere il `WHERE provider = $1` dalla UPDATE (il giro
    /// openrouter riscrive il saldo di deepseek e l'assert sull'isolamento
    /// cade).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_nota_di_sync_non_cresce_a_ogni_giro(db: PgPool) {
        // UPSERT: la mig 0173 semina gia' una riga `deepseek` a budget 0. Il
        // test non la ricrea (sarebbe una fixture ricopiata, regola O): prende
        // quella vera e le da' il tetto che serve al caso.
        sqlx::query(
            "INSERT INTO provider_budget_status (provider, monthly_budget_usd) VALUES ('deepseek', 20)
             ON CONFLICT (provider) DO UPDATE SET monthly_budget_usd = 20",
        )
        .execute(&db)
        .await
        .expect("seed riga deepseek");

        let lunghezza = |db: PgPool| async move {
            sqlx::query_scalar::<_, Option<i32>>(
                "SELECT length(notes)::int FROM provider_budget_status WHERE provider = 'deepseek'",
            )
            .fetch_one(&db)
            .await
            .expect("length(notes)")
        };

        assert_eq!(
            persisti_balance(&db, "deepseek", 8.0, FonteSaldo::Endpoint)
                .await
                .unwrap(),
            1
        );
        let dopo_uno = lunghezza(db.clone()).await.expect("nota scritta al primo giro");
        persisti_balance(&db, "deepseek", 8.0, FonteSaldo::Endpoint)
            .await
            .unwrap();
        persisti_balance(&db, "deepseek", 8.0, FonteSaldo::Endpoint)
            .await
            .unwrap();
        let dopo_tre = lunghezza(db.clone()).await.expect("nota presente al terzo giro");

        assert_eq!(
            dopo_uno, dopo_tre,
            "la nota deve essere sostituita: accodandola, 5363 giri hanno prodotto 178 kB in una cella"
        );

        // La nota porta l'ULTIMO valore osservato, non la loro storia.
        persisti_balance(&db, "deepseek", 3.5, FonteSaldo::Endpoint)
            .await
            .unwrap();
        let nota: String = sqlx::query_scalar(
            "SELECT notes FROM provider_budget_status WHERE provider = 'deepseek'",
        )
        .fetch_one(&db)
        .await
        .expect("notes");
        assert_eq!(nota, "[sync deepseek api: balance=3.50]");

        // Lo spent resta il dato reale derivato dal balance appena osservato,
        // e accanto ci sono le colonne dell'osservazione (mig 0719).
        let (spent, saldo, fonte): (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT spent_current_period_usd::text, last_known_balance_usd::text, balance_source
               FROM provider_budget_status WHERE provider = 'deepseek'",
        )
        .fetch_one(&db)
        .await
        .expect("riga deepseek");
        assert_eq!(spent.parse::<f64>().unwrap(), 16.5);
        assert_eq!(
            saldo.as_deref().map(|v| v.parse::<f64>().unwrap()),
            Some(3.5)
        );
        assert_eq!(fonte.as_deref(), Some("endpoint"));

        // ISOLAMENTO: il giro su openrouter (riga seminata dalla 0719, con la
        // fonte di ripiego) non tocca la riga deepseek appena scritta.
        assert_eq!(
            persisti_balance(&db, "openrouter", 10.4, FonteSaldo::AuthKeyFallback)
                .await
                .unwrap(),
            1
        );
        let (saldo_ds, fonte_or): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT (SELECT last_known_balance_usd::text FROM provider_budget_status WHERE provider = 'deepseek'),
                    (SELECT balance_source FROM provider_budget_status WHERE provider = 'openrouter')",
        )
        .fetch_one(&db)
        .await
        .expect("righe deepseek e openrouter");
        assert_eq!(
            saldo_ds.as_deref().map(|v| v.parse::<f64>().unwrap()),
            Some(3.5),
            "il giro su openrouter non deve toccare la riga deepseek"
        );
        assert_eq!(fonte_or.as_deref(), Some("auth_key_fallback"));
    }
}
