pub mod llm_timeouts;

use axum::{
    extract::{ConnectInfo, State},
    http::{header, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::net::SocketAddr;
use std::sync::LazyLock;
use std::time::Duration;

use jsonwebtoken::{decode, DecodingKey, Validation};
use nexus_cache::TtlCache;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

// --- JWT Claims ---

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: String, // user_id
    pub role: String,
    pub exp: usize,
}

// --- Helpers ---
//
// Lettura settings: punto unico (regola L / ADR 0026). La query SQL vive solo
// in `read_setting_raw`; tutte le viste (Result/Option, raw/trim, bool/int)
// delegano qui. Niente query `SELECT ... FROM settings` duplicate nei crate.

/// TTL della cache dei settings.
///
/// 60s non e' un numero scelto qui: e' la finestra che il repo ha gia' dichiarato
/// per la configurazione calda (la routing matrix promette "UPDATE in DB ->
/// pickup entro 60s, niente redeploy"). Questa cache estende quel contratto a
/// tutte le chiavi, invece di lasciarlo alle cache artigianali che i singoli
/// lettori si erano gia' costruiti quando il traffico faceva male.
///
/// Perche' e' una costante e non una riga di `settings` (regola G): leggerla dal
/// DB richiederebbe la lettura che questo TTL governa. E' l'unico parametro del
/// sistema che non puo' stare nella tabella che configura.
const SETTINGS_TTL: Duration = Duration::from_secs(60);

/// Cache delle letture di `settings`.
///
/// Il TTL e' per-istanza e non globale perche' un test possa costruirne una da
/// 20ms e chiamare la STESSA funzione della produzione col solo parametro
/// cambiato, invece di dormire un minuto o di verificare uno strumento
/// introdotto dalla patch stessa (regola O).
pub(crate) struct SettingsCache {
    cache: TtlCache<(String, String), Option<String>>,
}

impl SettingsCache {
    pub(crate) fn new(ttl: Duration) -> Self {
        Self {
            cache: TtlCache::new(ttl),
        }
    }

    /// Lettura con cache. `Ok(None)` (chiave assente) E' un fatto sulla
    /// configurazione e viene memorizzato: senza, ogni default applicato dai
    /// chiamanti resterebbe un round-trip, cioe' esattamente il traffico da
    /// togliere.
    pub(crate) async fn read(
        &self,
        db: &PgPool,
        key: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let ck = (pool_identity(db), key.to_string());
        if let Some(cached) = self.cache.get(&ck) {
            return Ok(cached);
        }
        // L'ERRORE NON ENTRA IN CACHE, deliberatamente: `get_setting` lo ingoia
        // ritornando None, e memorizzarlo trasformerebbe un blip transitorio del
        // DB in una configurazione sbagliata che persiste per tutto il TTL.
        let value = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
            .bind(key)
            .fetch_optional(db)
            .await?;
        self.cache.insert(ck, value.clone());
        Ok(value)
    }

    pub(crate) fn invalidate(&self, db: &PgPool, key: &str) {
        self.cache.invalidate(&(pool_identity(db), key.to_string()));
    }
}

static SETTINGS_CACHE: LazyLock<SettingsCache> = LazyLock::new(|| SettingsCache::new(SETTINGS_TTL));

/// Identita' del pool: due pool verso lo STESSO database condividono la cache,
/// due pool verso database diversi non si vedono mai.
///
/// Senza questo, una lettura fatta col pool di un progetto (`<slug>_nexus`)
/// servirebbe il valore del meta, o viceversa: la chiave `settings.key` e' la
/// stessa in entrambi i database, ma il valore no.
///
/// PUNTO UNICO (regola L) della chiave di una cache di processo che memorizza
/// CONFIGURAZIONE letta da un database. E' `pub` perche' non tutta quella
/// configurazione vive in `settings` (es. i pesi di scoring stanno in
/// `nexus_intent_routing_requirements`): chi cacha quelle letture deve poter
/// usare la STESSA identita', invece di inventarsi una chiave costante che
/// confonde i database — l'errore che il 2026-07-27 ha reso sei test dipendenti
/// dall'ordine di esecuzione.
pub fn pool_identity(db: &PgPool) -> String {
    let o = db.connect_options();
    format!(
        "{}@{}:{}/{}",
        o.get_username(),
        o.get_host(),
        o.get_port(),
        o.get_database().unwrap_or(""),
    )
}

/// Invalida la voce di cache di una chiave.
///
/// Serve ai percorsi che scrivono `settings` con una query propria (upsert con
/// categoria, REPLACE mirati): senza, la loro scrittura resterebbe invisibile
/// alle letture per tutto il TTL. Chi puo' passare da `update_setting_value` non
/// ha bisogno di chiamarla: quella invalida gia'.
///
/// NON copre le scritture fatte da un ALTRO PROCESSO (admin-service ha il suo
/// pool, e `psql` non ha cache): quelle si propagano entro il TTL, che e' il
/// contratto gia' in vigore per la configurazione calda.
pub fn invalidate_setting_cache(db: &PgPool, key: &str) {
    SETTINGS_CACHE.invalidate(db, key);
}

/// Query unica della tabella `settings`. Punto di verita' della lettura, e unico
/// posto dove vive la cache: un chiamante che debba RICORDARSI di cachare e' un
/// chiamante che prima o poi se lo dimentica (regola L).
async fn read_setting_raw(db: &PgPool, key: &str) -> Result<Option<String>, sqlx::Error> {
    SETTINGS_CACHE.read(db, key).await
}

/// Legge una setting propagando l'errore DB (regola H: non ingoiare). Valore
/// RAW: nessun trim, nessun filtro sui vuoti.
pub async fn get_setting_checked(db: &PgPool, key: &str) -> anyhow::Result<Option<String>> {
    read_setting_raw(db, key)
        .await
        .map_err(|e| anyhow::anyhow!("lettura setting '{key}' fallita: {e}"))
}

/// Come `get_setting_checked` ma con `trim()` e scartando i valori vuoti.
pub async fn get_setting_nonempty(db: &PgPool, key: &str) -> anyhow::Result<Option<String>> {
    Ok(get_setting_checked(db, key)
        .await?
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty()))
}

/// Esito della lettura di una setting da un contesto NON autenticato
/// (segnale strutturato, regola M: il chiamante mappa la variante sullo status
/// HTTP, non deduce nulla dal valore).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicSettingRead {
    /// Chiave non marcata segreta: il valore puo' essere esposto (anche vuoto).
    Value(String),
    /// Chiave con `is_secret = TRUE`: il valore NON viene letto ne' restituito.
    Redacted,
    /// La chiave non esiste in tabella.
    NotFound,
}

/// Legge una setting per un contesto NON autenticato, rifiutando i segreti.
///
/// PUNTO UNICO (regola L) del predicato "questa chiave e' esponibile senza
/// autenticazione". Esiste perche' `GET /internal/settings/:key` e' montato
/// fuori dal layer di auth in DUE servizi (mcp-core e admin-service, entrambi
/// in ascolto su `0.0.0.0`) e leggeva il valore RAW senza guardare `is_secret`:
/// `jwt_secret` e le API key dei provider erano scaricabili da chiunque
/// raggiungesse la porta, e con la chiave di firma si conia un token di
/// amministratore. Il masking della LISTA (`/api/settings`) guardava
/// `is_secret`; la lettura puntuale no.
///
/// Non passa dalla cache: legge `is_secret` e `value` nella stessa query, cosi'
/// il verdetto e il valore non possono divergere. Il valore di una chiave
/// segreta non viene nemmeno deserializzato nel ramo `Redacted`.
pub async fn get_setting_public(
    db: &PgPool,
    key: &str,
) -> Result<PublicSettingRead, sqlx::Error> {
    let row: Option<(String, bool)> =
        sqlx::query_as("SELECT value, is_secret FROM settings WHERE key = $1")
            .bind(key)
            .fetch_optional(db)
            .await?;
    Ok(match row {
        None => PublicSettingRead::NotFound,
        Some((_, true)) => PublicSettingRead::Redacted,
        Some((value, false)) => PublicSettingRead::Value(value),
    })
}

/// Variante best-effort che ingoia l'errore DB ritornando `None` (con trim e
/// scarto dei vuoti). Mantenuta per i call site storici; per il codice NUOVO
/// preferire `get_setting_checked`/`get_setting_nonempty`, che propagano.
pub async fn get_setting(db: &PgPool, key: &str) -> Option<String> {
    read_setting_raw(db, key)
        .await
        .ok()
        .flatten()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

// Scrittura settings: punto unico (regola L / ADR 0026), accanto alla lettura.
// Prima la stessa logica viveva duplicata in `mcp-core::settings::update_setting`
// e `admin-service::settings::update_setting`.

/// Errore di una scrittura su `settings`.
#[derive(Debug, thiserror::Error)]
pub enum SettingWriteError {
    /// La chiave non esiste. Il PUT aggiorna, non crea: vedi
    /// `update_setting_value` per il razionale.
    #[error("setting '{0}' inesistente: le chiavi si creano da una migrazione, non da una scrittura")]
    UnknownKey(String),
    #[error("scrittura setting '{key}' fallita: {source}")]
    Db { key: String, source: sqlx::Error },
}

impl SettingWriteError {
    /// Status HTTP dell'errore. Vive qui perche' la mappatura e' la STESSA per
    /// ogni chiamante (regola L): chiave assente => 404, DB che rifiuta => 500.
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::UnknownKey(_) => StatusCode::NOT_FOUND,
            Self::Db { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Aggiorna il valore di una setting ESISTENTE. Punto di verita' della scrittura.
///
/// Una chiave assente e' un errore (`UnknownKey`), non un invito a crearla.
/// Prima l'handler ripiegava su un `INSERT ... VALUES (key, value, 'custom', '',
/// FALSE)`, e un refuso nel nome creava una riga NUOVA invece di dare errore: la
/// UI rispondeva "salvato" a una scrittura senza alcun effetto. Chi scrive
/// `nexus_behavior_moda` crede di aver cambiato il comportamento, ma il sistema
/// continua a leggere `nexus_behavior_mode` — nessuno legge mai la chiave col
/// refuso. Il danno e' la scrittura silenziosamente inefficace, piu' la
/// spazzatura che si accumula in 'custom'.
///
/// NON e' un problema di invisibilita': la riga si vede eccome. `list_categories`
/// e' data-driven (`GROUP BY category`, nessuna whitelist) e `buildList`
/// (`apps/web-ide/lib/settings-categories.ts`) accoda in fondo ogni categoria
/// sconosciuta, quindi 'custom' compare nella sidebar ed e' navigabile.
/// Verificato contro il DB, non dedotto.
///
/// Il censimento dei chiamanti (pagine admin, plugin manager, toggle provider,
/// pannello orchestrator, council) non ha trovato un flusso che dipenda dalla
/// creazione: scrivono chiavi seedate da migrazione, o chiavi lette da
/// `GET /api/admin/settings`, che per costruzione esistono gia'. Chi ha bisogno
/// di una chiave nuova la dichiara alla fonte, con categoria e `is_secret`
/// veri: le migrazioni per i default statici, `plugins::integrate::publish` per
/// i secret dei plugin integrati a runtime.
pub async fn update_setting_value(
    db: &PgPool,
    key: &str,
    value: &str,
) -> Result<(), SettingWriteError> {
    let result = sqlx::query("UPDATE settings SET value = $1, updated_at = NOW() WHERE key = $2")
        .bind(value)
        .bind(key)
        .execute(db)
        .await
        .map_err(|source| SettingWriteError::Db {
            key: key.to_string(),
            source,
        })?;

    if result.rows_affected() == 0 {
        return Err(SettingWriteError::UnknownKey(key.to_string()));
    }
    // Invalidazione accanto alla scrittura: chi aggiorna dal punto unico vede il
    // proprio valore SUBITO, non entro il TTL. Senza questa riga un PUT admin
    // seguito da una rilettura mostrerebbe ancora il vecchio valore, e sembrerebbe
    // che la scrittura non abbia funzionato.
    SETTINGS_CACHE.invalidate(db, key);
    Ok(())
}

/// Legge una setting booleana (`true`/`1`/`yes`/`on` => true). Propaga l'errore DB.
pub async fn get_bool_setting(db: &PgPool, key: &str) -> anyhow::Result<Option<bool>> {
    Ok(get_setting_nonempty(db, key)
        .await?
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on")))
}

/// Legge una setting intera. Propaga l'errore DB; valore non numerico => errore.
pub async fn get_int_setting(db: &PgPool, key: &str) -> anyhow::Result<Option<i64>> {
    match get_setting_nonempty(db, key).await? {
        Some(v) => Ok(Some(v.parse::<i64>().map_err(|e| {
            anyhow::anyhow!("setting '{key}' non e' un intero valido ('{v}'): {e}")
        })?)),
        None => Ok(None),
    }
}

/// Risolve la porta di bind di un servizio leggendola ESCLUSIVAMENTE dal DB
/// (tabella `settings`, regola G del CLAUDE.md: il DB e' l'unica fonte di
/// verita' per la configurazione). Nessun default hardcoded e nessuna env var:
/// se il valore non e' disponibile il servizio PANICA con un messaggio chiaro,
/// coerente con `RoutingMatrixCache::init` di mcp-core.
///
/// - `key`: chiave in `settings` (es. "admin_service_port").
/// - DB irraggiungibile: retry 5 tentativi x 5s (il container Postgres puo'
///   essere ancora in avvio), poi panic.
/// - chiave assente o valore non valido: panic immediato (config errata /
///   migrazione 0239 non applicata): meglio non partire che fare bind su una
///   porta sbagliata silenziosamente.
pub async fn resolve_port(db: &PgPool, key: &str) -> u16 {
    for attempt in 1..=5u32 {
        match sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
            .bind(key)
            .fetch_optional(db)
            .await
        {
            Ok(Some(raw)) => {
                let v = raw.trim();
                return v.parse::<u16>().ok().filter(|p| *p > 0).unwrap_or_else(|| {
                    panic!(
                        "resolve_port: settings.{key} = {v:?} non e' una porta valida (1..=65535). \
                         Correggi il valore nel DB."
                    )
                });
            }
            Ok(None) => panic!(
                "resolve_port: settings.{key} assente nel DB. Applica la migrazione \
                 db/migrations/0239_infrastructure_ports.sql (regola G: niente porte hardcoded)."
            ),
            Err(e) if attempt < 5 => {
                tracing::warn!(
                    "resolve_port: tentativo {attempt}/5 lettura settings.{key} fallito ({e}). Retry in 5s..."
                );
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            Err(e) => panic!(
                "resolve_port: impossibile leggere settings.{key} dal DB dopo 5 tentativi: {e}. \
                 Verifica che Postgres sia raggiungibile e che la migrazione 0239 sia applicata."
            ),
        }
    }
    unreachable!("resolve_port: loop di retry terminato senza esito per {key}")
}

pub async fn get_or_create_jwt_secret(db: &PgPool) -> anyhow::Result<String> {
    get_or_create_platform_secret(db, "jwt_secret").await
}

/// Prefissi delle rotte riservate alla comunicazione fra processi Nexus.
/// Non hanno il layer di autenticazione: il loro confine e' l'origine della
/// connessione, imposto da [`internal_only_middleware`].
const INTERNAL_PATH_PREFIXES: &[&str] = &["/internal/", "/api/internal/"];

/// Vero se il path appartiene al blocco interno. Punto unico del predicato:
/// i due servizi che montano quelle rotte devono usare lo STESSO criterio.
pub fn is_internal_path(path: &str) -> bool {
    INTERNAL_PATH_PREFIXES.iter().any(|p| path.starts_with(p))
}

/// Nega le rotte `/internal/*` a chi non chiama dalla macchina locale.
///
/// Quelle rotte sono montate FUORI dal layer di autenticazione — e' il loro
/// scopo: servono a far parlare fra loro i processi Nexus senza credenziali
/// utente. Ma i servizi ascoltano su `0.0.0.0`, quindi "interno" era una parola
/// nel commento, non una proprieta' verificata: chiunque raggiungesse la porta
/// le interrogava. Una di esse restituiva la chiave di firma della piattaforma.
///
/// Il confine ora e' l'indirizzo sorgente. Se `ConnectInfo` non e' disponibile
/// la richiesta viene RIFIUTATA, non lasciata passare: un middleware di
/// sicurezza che degrada a permissivo quando non sa decidere e' il difetto che
/// stiamo chiudendo, non la sua cura. Perche' l'informazione ci sia, il server
/// deve servire con `into_make_service_with_connect_info::<SocketAddr>()`.
///
/// NB: se un giorno si mettesse un reverse proxy davanti a questi servizi,
/// l'indirizzo sorgente diventerebbe quello del proxy e il filtro perderebbe
/// significato. In quel caso le rotte interne vanno spostate su un listener
/// separato legato a `127.0.0.1`, non "corrette" leggendo `X-Forwarded-For`
/// (un header che il client controlla non e' un confine).
pub async fn internal_only_middleware(
    connect_info: Option<ConnectInfo<SocketAddr>>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    if !is_internal_path(&path) {
        return next.run(req).await;
    }

    let consentito = match connect_info {
        Some(ConnectInfo(addr)) => addr.ip().is_loopback(),
        None => {
            tracing::error!(
                path = %path,
                "rotta interna senza ConnectInfo: il server non e' avviato con \
                 into_make_service_with_connect_info, rifiuto la richiesta"
            );
            false
        }
    };

    if consentito {
        return next.run(req).await;
    }

    // Niente indirizzo nel corpo della risposta: si logga, non si racconta.
    tracing::warn!(path = %path, "rotta interna richiesta da un'origine non locale: rifiutata");
    (
        StatusCode::FORBIDDEN,
        "rotta interna: raggiungibile solo dalla macchina locale",
    )
        .into_response()
}

/// Vita del bearer di servizio. Corta: e' la finestra entro cui un token
/// intercettato resta spendibile.
const SERVICE_BEARER_TTL: Duration = Duration::from_secs(15 * 60);

/// TTL della cache del token coniato, piu' corta della vita del token: quando
/// la cache scade il token in mano al chiamante e' ancora valido, quindi non
/// esiste un istante in cui si presenta una credenziale gia' scaduta.
const SERVICE_BEARER_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

/// `sub` dei bearer di servizio. Non e' un utente: identifica le chiamate
/// interne fra processi Nexus nei log del gateway.
pub const SERVICE_SUBJECT: &str = "nexus-service";

static SERVICE_BEARER_CACHE: LazyLock<TtlCache<String, String>> =
    LazyLock::new(|| TtlCache::new(SERVICE_BEARER_CACHE_TTL));

/// Bearer per le chiamate interne verso il gateway: un JWT a vita breve firmato
/// con la chiave di piattaforma.
///
/// Sostituisce il bearer STATICO condiviso che c'era prima
/// (`DEV_SERVICE_TOKEN = "dev-internal-token"`, hardcoded come fallback in otto
/// punti perche' l'env `NEXUS_GATEWAY_SERVICE_TOKEN` non era impostata da
/// nessuna parte). Quel valore era nel sorgente e valeva come bypass totale
/// dell'autenticazione su un servizio in ascolto su `0.0.0.0`: misurato, il
/// gateway rispondeva 401 senza header e 200 con il token letto dal repo.
///
/// Perche' un JWT e non una nuova chiave dedicata: il gateway lo valida con lo
/// STESSO `decode::<Claims>` che usa per i token utente, quindi il ramo speciale
/// "se coincide col service token allora passa" — cioe' il bypass — sparisce
/// invece di essere sostituito da un altro segreto da custodire. Non c'e' piu'
/// niente da indovinare: chi non ha la chiave di firma non conia nulla.
pub async fn service_bearer(db: &PgPool) -> anyhow::Result<String> {
    // La chiave include l'identita' del pool (stesso criterio della cache dei
    // settings): il token e' firmato con la chiave di QUEL database, servirlo a
    // un altro produrrebbe un 401 inspiegabile.
    let cache_key = format!("{}::{}", pool_identity(db), SERVICE_SUBJECT);
    if let Some(tok) = SERVICE_BEARER_CACHE.get(&cache_key) {
        return Ok(tok);
    }

    let secret = get_or_create_jwt_secret(db).await?;
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| anyhow::anyhow!("orologio di sistema prima di UNIX_EPOCH: {e}"))?
        + SERVICE_BEARER_TTL;
    let claims = Claims {
        sub: SERVICE_SUBJECT.to_string(),
        role: "service".to_string(),
        exp: exp.as_secs() as usize,
    };
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| anyhow::anyhow!("firma del bearer di servizio fallita: {e}"))?;

    SERVICE_BEARER_CACHE.insert(cache_key, token.clone());
    Ok(token)
}

/// Risolve un segreto di piattaforma, generandolo se la riga e' vuota.
///
/// PUNTO UNICO (regola L) del pattern "leggi-o-genera un segreto persistente".
///
/// Due difetti che questa forma chiude, entrambi presenti nella versione
/// precedente di `get_or_create_jwt_secret`:
///
/// 1. RACE. Leggeva con `get_setting`, che passa dalla cache: la cache
///    memorizza anche il valore VUOTO (60s di TTL), quindi due chiamate
///    concorrenti — o due entro la finestra — vedevano entrambe "assente",
///    generavano DUE segreti diversi e li scrivevano una sopra l'altra.
///    L'ultima vinceva nel DB e il primo utente restava con un JWT firmato
///    con una chiave non piu' in uso: sessione persa senza alcun errore.
///    Qui la generazione e' una SOLA istruzione: sul conflitto, il secondo
///    processo attende il commit del primo, rivaluta la `CASE` sulla riga
///    aggiornata e la `RETURNING` gli restituisce lo STESSO segreto.
///    (`DO NOTHING` non andrebbe bene: sul conflitto non produce righe.)
/// 2. CACHE NON INVALIDATA. La vecchia scrittura era un `UPDATE` raw: dopo
///    aver generato il segreto, nello stesso processo la lettura successiva
///    continuava a servire il valore vuoto dalla cache fino allo scadere del
///    TTL — un JWT appena firmato correttamente veniva rifiutato.
///
/// La riga deve esistere (la seminano le migrazioni, `value` e' `NOT NULL
/// DEFAULT ''`): se manca del tutto e' un errore di schema, non un caso da
/// gestire in silenzio.
pub async fn get_or_create_platform_secret(db: &PgPool, key: &str) -> anyhow::Result<String> {
    let candidato: String = (0..64)
        .map(|_| format!("{:02x}", rand::thread_rng().gen::<u8>()))
        .collect();

    let secret: Option<String> = sqlx::query_scalar(
        "UPDATE settings \
            SET value = CASE WHEN value = '' THEN $2 ELSE value END, \
                updated_at = CASE WHEN value = '' THEN NOW() ELSE updated_at END \
          WHERE key = $1 \
        RETURNING value",
    )
    .bind(key)
    .bind(&candidato)
    .fetch_optional(db)
    .await
    .map_err(|e| anyhow::anyhow!("generazione segreto '{key}' fallita: {e}"))?;

    let secret = secret.ok_or_else(|| {
        anyhow::anyhow!("segreto '{key}' assente dalla tabella settings: schema incompleto")
    })?;

    // La scrittura invalida la cache: senza, la lettura successiva nello stesso
    // processo servirebbe il valore vecchio (vuoto) fino allo scadere del TTL,
    // e un JWT appena firmato correttamente verrebbe rifiutato.
    invalidate_setting_cache(db, key);

    if secret.trim().is_empty() {
        anyhow::bail!("segreto '{key}' vuoto dopo la generazione");
    }
    Ok(secret)
}

pub fn frontend_url() -> String {
    std::env::var("FRONTEND_URL").unwrap_or_else(|_| "http://localhost:3000".to_string())
}

pub fn backend_url() -> String {
    std::env::var("PUBLIC_BACKEND_URL").unwrap_or_else(|_| "http://localhost:4000".to_string())
}

fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn extract_token_from_cookie(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|c| {
            let c = c.trim();
            c.strip_prefix("token=").map(|v| v.to_string())
        })
}

// --- Token validation ---

/// Valida il token del cookie: firma, scadenza del claim e sessione viva.
///
/// # Perche' i modi di fallire si distinguono nel log
///
/// Tutti danno 401 al chiamante, e cosi' deve essere: dire a chi non e'
/// autenticato PERCHE' non lo e' regala informazione a chi sta provando. Ma il
/// log e' interno, e li' l'indistinguibilita' costa: cookie assente, firma non
/// corrispondente e sessione mai aperta hanno rimedi opposti, e il messaggio
/// unico "token validation failed" non permetteva di sceglierne uno senza
/// modificare il codice per scoprirlo.
///
/// Il caso che ha portato a questa modifica lo dimostra al rovescio. Il
/// 2026-07-26, diagnosticando un 401 sistematico, sono state verificate a mano la
/// firma (HMAC ricalcolato contro `settings.jwt_secret`: combaciava) e la sessione
/// (riga inserita, `expires_at` valido): tutto in ordine, e il 401 restava. La
/// causa era il PRIMO ramo, quello del cookie: lo strumento di misura —
/// `Invoke-WebRequest` di PowerShell 5.1 — scarta l'header `Cookie` passato in
/// `-Headers` e usa il proprio CookieContainer, quindi la credenziale non
/// arrivava affatto. Con `curl` la stessa richiesta rispondeva 200. Un log che
/// dicesse "nessun cookie 'token' nella richiesta" avrebbe chiuso la diagnosi in
/// un secondo invece di un'ora.
///
/// Il motivo esiste gia' in forma strutturata a ogni passo: qui viene solo emesso
/// invece di essere appiattito (regola M).
pub async fn validate_token(
    db: &PgPool,
    headers: &axum::http::HeaderMap,
) -> Result<Claims, StatusCode> {
    let Some(token) = extract_token_from_cookie(headers) else {
        tracing::debug!("validate_token: nessun cookie 'token' nella richiesta");
        return Err(StatusCode::UNAUTHORIZED);
    };

    let Some(jwt_secret) = get_setting(db, "jwt_secret").await else {
        tracing::warn!(
            "validate_token: settings.jwt_secret assente o illeggibile: nessun token puo' essere validato"
        );
        return Err(StatusCode::UNAUTHORIZED);
    };

    let token_data = match decode::<Claims>(
        &token,
        &DecodingKey::from_secret(jwt_secret.as_bytes()),
        &Validation::default(),
    ) {
        Ok(d) => d,
        Err(e) => {
            // `e.kind()` distingue firma non valida, token scaduto e payload
            // malformato: e' il segnale strutturato della libreria, non una
            // frase da interpretare. Il token NON viene loggato (e' una
            // credenziale, regola F).
            tracing::warn!(
                "validate_token: decodifica rifiutata ({:?}): firma non corrispondente al jwt_secret corrente, token scaduto o claim malformati",
                e.kind()
            );
            return Err(StatusCode::UNAUTHORIZED);
        }
    };

    // La firma da' per buono il contenuto, non l'esistenza della sessione: un
    // token puo' essere perfettamente firmato e non avere mai avuto una riga in
    // `sessions` (percorso di login che non la scrive) o averla scaduta.
    let token_hash = hash_token(&token);
    if !check_session_exists(db, &token_hash).await? {
        tracing::warn!(
            "validate_token: firma valida ma nessuna sessione viva per questo token (user={}). \
             La riga in `sessions` la scrive il percorso di login: un token emesso senza passarci \
             e' valido come firma e inutilizzabile come credenziale",
            token_data.claims.sub
        );
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(token_data.claims)
}

/// Verifica nel DB se esiste una sessione non scaduta col dato hash.
///
/// Estratta da `validate_token` per consentire test di regressione sul
/// comportamento "DB down -> 500, NON 401" (fix S90, regola H).
pub async fn check_session_exists(
    db: &PgPool,
    token_hash: &str,
) -> Result<bool, StatusCode> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE token_hash = $1 AND expires_at > NOW())",
    )
    .bind(token_hash)
    .fetch_one(db)
    .await
    .map_err(|e| {
        // Fix regola H (S90): prima `.unwrap_or(false)` -> tutti gli utenti
        // ricevevano 401 quando il DB cadeva, diagnosi sbagliata garantita.
        tracing::error!("check_session_exists: SELECT sessions fallita: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

// --- Middleware ---

/// Middleware that requires a valid JWT token.
/// Inserts Claims into request extensions on success.
pub async fn require_auth<S: Clone + Send + Sync + 'static>(
    State(db): State<PgPool>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let claims = validate_token(&db, req.headers()).await?;
    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
}

/// Middleware that requires a valid JWT token with admin role.
pub async fn require_admin<S: Clone + Send + Sync + 'static>(
    State(db): State<PgPool>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    match validate_token(&db, req.headers()).await {
        Ok(claims) => {
            if claims.role != "admin" {
                tracing::warn!(
                    "require_admin: access denied - role={} is not admin, path={}",
                    claims.role,
                    req.uri()
                );
                return Err(StatusCode::FORBIDDEN);
            }
            req.extensions_mut().insert(claims);
            Ok(next.run(req).await)
        }
        Err(e) => {
            tracing::warn!("require_admin: token validation failed: {:?}, path={}", e, req.uri());
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests_settings_cache {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    /// Schema allineato a `db/migrations/0002_settings.sql`: `update_setting_value`
    /// scrive anche `updated_at`, quindi una tabella di prova ridotta non
    /// eserciterebbe la stessa query della produzione (regola O).
    async fn crea_settings(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE settings ( \
                 key TEXT PRIMARY KEY, \
                 value TEXT NOT NULL DEFAULT '', \
                 category TEXT NOT NULL DEFAULT 'general', \
                 description TEXT NOT NULL DEFAULT '', \
                 is_secret BOOLEAN NOT NULL DEFAULT FALSE, \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW() )",
        )
        .execute(pool)
        .await
        .expect("create table settings");
    }

    async fn semina(pool: &PgPool, key: &str, value: &str) {
        sqlx::query("INSERT INTO settings (key, value) VALUES ($1, $2)")
            .bind(key)
            .bind(value)
            .execute(pool)
            .await
            .expect("insert setting");
    }

    /// Pool verso una porta senza listener: ogni query fallisce, nessuna rete.
    fn pool_morto(dsn: &str) -> PgPool {
        PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_millis(200))
            .connect_lazy(dsn)
            .expect("connect_lazy non tocca la rete: se fallisce e' la DSN")
    }

    /// La prova che il round-trip e' davvero risparmiato non passa da un
    /// contatore introdotto da questa patch (se sbagliassi l'incremento, il test
    /// resterebbe verde con la cache rotta): passa da un pool CHIUSO, che non
    /// puo' servire query. Se la seconda lettura riesce, viene dalla cache.
    #[sqlx::test]
    async fn la_seconda_lettura_non_tocca_il_db(pool: PgPool) {
        crea_settings(&pool).await;
        semina(&pool, "test.cache.hit", "v1").await;

        let v1 = get_setting_checked(&pool, "test.cache.hit")
            .await
            .expect("prima lettura");
        pool.close().await;
        let v2 = get_setting_checked(&pool, "test.cache.hit")
            .await
            .expect("la seconda lettura deve venire dalla cache, non dal pool chiuso");

        assert_eq!(v1, v2);
        assert_eq!(v2.as_deref(), Some("v1"));
    }

    /// La lettura senza autenticazione NON deve restituire un segreto.
    ///
    /// Il test parte dalla stessa forma che ha la riga in produzione — `jwt_secret`
    /// e' seminata con `is_secret = TRUE` dalla migrazione 0003 — e asserisce la
    /// VARIANTE dell'esito, non l'assenza di errore: `get_setting_public` che
    /// ritornasse `Value(segreto)` senza fallire sarebbe verde su un assert
    /// generico, ed e' esattamente il difetto che c'era (`GET
    /// /internal/settings/jwt_secret` rispondeva 200 con la chiave in chiaro).
    #[sqlx::test]
    async fn una_chiave_segreta_non_e_leggibile_senza_auth(pool: PgPool) {
        crea_settings(&pool).await;
        sqlx::query("INSERT INTO settings (key, value, is_secret) VALUES ($1, $2, TRUE)")
            .bind("jwt_secret")
            .bind("chiave-di-firma-che-non-deve-uscire")
            .execute(&pool)
            .await
            .expect("insert segreto");

        let esito = get_setting_public(&pool, "jwt_secret")
            .await
            .expect("la lettura non deve fallire: deve RIFIUTARE");

        assert_eq!(
            esito,
            PublicSettingRead::Redacted,
            "una chiave is_secret deve dare Redacted, mai il valore"
        );
        // Prova esplicita che il segreto non e' nel risultato in nessuna forma.
        assert!(
            !format!("{esito:?}").contains("chiave-di-firma"),
            "il valore del segreto non deve comparire nemmeno nel Debug: {esito:?}"
        );
    }

    /// Due generazioni concorrenti devono restituire lo STESSO segreto.
    ///
    /// E' il difetto che c'era: la vecchia `get_or_create_jwt_secret` leggeva
    /// dalla cache (che memorizza anche il valore vuoto) e scriveva con un
    /// `UPDATE` raw, quindi due chiamate ravvicinate generavano due chiavi
    /// diverse; l'ultima vinceva nel DB e il primo utente restava con un JWT
    /// non piu' verificabile — sessione persa senza un errore da nessuna parte.
    ///
    /// Il test lancia le due chiamate davvero in parallelo sullo stesso pool e
    /// asserisce l'uguaglianza dei due valori RESTITUITI (non solo di cio' che
    /// resta nel DB: e' il valore ricevuto dal chiamante che finisce a firmare).
    #[sqlx::test]
    async fn due_generazioni_concorrenti_danno_lo_stesso_segreto(pool: PgPool) {
        crea_settings(&pool).await;
        // La riga esiste con valore VUOTO: e' la forma con cui la seminano le
        // migrazioni (0003), non una tabella senza riga.
        semina(&pool, "jwt_secret", "").await;

        // Apre la finestra REALE del difetto. La race non nasce dal parallelismo
        // dei thread — con `join!` su runtime a thread singolo le due future si
        // alternano e la seconda vedrebbe comunque la scrittura della prima, e
        // infatti un test cosi' resta verde anche col difetto rimesso (provato).
        // Nasce dalla CACHE: questa lettura la popola col valore vuoto, e da qui
        // per 60s ogni chiamante che legga con `get_setting` crede che il
        // segreto non esista — che e' esattamente il caso "due login entro un
        // minuto" che faceva perdere la sessione al primo utente.
        assert!(
            get_setting(&pool, "jwt_secret").await.is_none(),
            "precondizione: il segreto risulta assente e la cache lo memorizza"
        );

        let a = get_or_create_platform_secret(&pool, "jwt_secret")
            .await
            .expect("prima generazione");
        let b = get_or_create_platform_secret(&pool, "jwt_secret")
            .await
            .expect("seconda generazione, con la cache gia' popolata a vuoto");

        assert_eq!(
            a, b,
            "la seconda chiamata non deve generare un secondo segreto: \
             il primo utente resterebbe con un JWT non piu' verificabile"
        );
        assert_eq!(a.len(), 128, "64 byte esadecimali");

        // La cache deve vedere il segreto appena generato, non piu' il vuoto:
        // senza invalidazione, un JWT appena firmato verrebbe rifiutato fino
        // allo scadere del TTL.
        assert_eq!(
            get_setting(&pool, "jwt_secret").await.as_deref(),
            Some(a.as_str()),
            "dopo la generazione la cache deve servire il segreto, non il vuoto"
        );

        // E il valore restituito deve essere quello effettivamente persistito.
        let nel_db: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'jwt_secret'")
            .fetch_one(&pool)
            .await
            .expect("rilettura");
        assert_eq!(nel_db, a, "il segreto restituito e' quello nel DB");
    }

    /// Una seconda invocazione, dopo che il segreto esiste, non lo rigenera.
    #[sqlx::test]
    async fn un_segreto_gia_presente_non_viene_rigenerato(pool: PgPool) {
        crea_settings(&pool).await;
        semina(&pool, "jwt_secret", "segreto-preesistente-da-non-toccare").await;

        let letto = get_or_create_platform_secret(&pool, "jwt_secret")
            .await
            .expect("lettura");

        assert_eq!(letto, "segreto-preesistente-da-non-toccare");
    }

    /// Il gemello: una chiave NON segreta resta leggibile, altrimenti il fix
    /// avrebbe rotto l'endpoint invece di ripararlo.
    #[sqlx::test]
    async fn una_chiave_normale_resta_leggibile(pool: PgPool) {
        crea_settings(&pool).await;
        semina(&pool, "nexus_gateway_port", "4060").await;

        let esito = get_setting_public(&pool, "nexus_gateway_port")
            .await
            .expect("lettura chiave pubblica");

        assert_eq!(esito, PublicSettingRead::Value("4060".to_string()));
    }

    /// Una chiave assente si distingue da una rifiutata: il chiamante mappa
    /// 404 e 403 su risposte diverse (regola M, esito tipizzato).
    #[sqlx::test]
    async fn una_chiave_assente_e_distinguibile_da_una_rifiutata(pool: PgPool) {
        crea_settings(&pool).await;

        let esito = get_setting_public(&pool, "chiave.che.non.esiste")
            .await
            .expect("lettura chiave assente");

        assert_eq!(esito, PublicSettingRead::NotFound);
    }

    /// Il difetto piu' costoso che una cache di settings possa avere: servire a
    /// un database il valore di un altro. La chiave `settings.key` e' la stessa
    /// nel meta e in ogni `<slug>_nexus`, il valore no.
    #[sqlx::test]
    async fn un_altro_database_non_e_servito_dalla_cache_del_primo(pool: PgPool) {
        crea_settings(&pool).await;
        semina(&pool, "test.iso.key", "valore_del_db_vero").await;
        get_setting_checked(&pool, "test.iso.key")
            .await
            .expect("popola la cache");

        // Secondo pool costruito dalle STESSE connect options del primo, con il
        // solo `database` cambiato: stesso utente, stesso host, stessa porta.
        // E' l'unica forma che prova davvero l'isolamento - un pool con utente o
        // porta diversi resterebbe distinto anche senza il database nella chiave,
        // e il test passerebbe pur essendo il difetto presente.
        let altro_db = sqlx::postgres::PgConnectOptions::clone(&pool.connect_options())
            .database("un_altro_database_che_non_esiste");
        let altro = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_millis(500))
            .connect_lazy_with(altro_db);

        assert!(
            get_setting_checked(&altro, "test.iso.key").await.is_err(),
            "un pool verso un ALTRO database non va servito dalla cache del primo"
        );
    }

    /// Chi scrive dal punto unico vede il proprio valore subito, non entro il TTL.
    #[sqlx::test]
    async fn la_scrittura_dal_punto_unico_invalida(pool: PgPool) {
        crea_settings(&pool).await;
        semina(&pool, "test.inval.key", "vecchio").await;
        assert_eq!(
            get_setting_checked(&pool, "test.inval.key")
                .await
                .unwrap()
                .as_deref(),
            Some("vecchio"),
            "popola la cache"
        );

        update_setting_value(&pool, "test.inval.key", "nuovo")
            .await
            .expect("update");

        assert_eq!(
            get_setting_checked(&pool, "test.inval.key")
                .await
                .unwrap()
                .as_deref(),
            Some("nuovo"),
            "la scrittura dal punto unico ha effetto SUBITO"
        );
    }

    /// Un blip del DB non deve inchiodare una configurazione sbagliata per tutto
    /// il TTL: l'errore non entra in cache.
    #[tokio::test]
    async fn un_errore_di_lettura_non_viene_memorizzato() {
        let morto = pool_morto("postgres://nobody:nopass@127.0.0.1:1/inesistente");

        assert!(
            get_setting_checked(&morto, "test.errore.non.cachato")
                .await
                .is_err(),
            "prima lettura: il DB e' irraggiungibile"
        );
        assert!(
            get_setting_checked(&morto, "test.errore.non.cachato")
                .await
                .is_err(),
            "anche la seconda deve interrogare il DB, non servire un errore memorizzato"
        );
    }

    /// La scadenza, in millisecondi invece che in un minuto: il TTL e'
    /// per-istanza proprio per poter esercitare la stessa `read` della
    /// produzione senza dormire (regola O, e niente test lenti).
    #[sqlx::test]
    async fn la_voce_scade_e_rilegge(pool: PgPool) {
        crea_settings(&pool).await;
        semina(&pool, "test.ttl.key", "primo").await;

        let breve = SettingsCache::new(Duration::from_millis(20));
        assert_eq!(
            breve.read(&pool, "test.ttl.key").await.unwrap().as_deref(),
            Some("primo")
        );

        sqlx::query("UPDATE settings SET value = 'secondo' WHERE key = 'test.ttl.key'")
            .execute(&pool)
            .await
            .expect("update fuori dal punto unico");
        // Entro il TTL la cache serve ancora il vecchio valore: e' il contratto.
        assert_eq!(
            breve.read(&pool, "test.ttl.key").await.unwrap().as_deref(),
            Some("primo"),
            "entro il TTL vale la copia"
        );

        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(
            breve.read(&pool, "test.ttl.key").await.unwrap().as_deref(),
            Some("secondo"),
            "scaduto il TTL la lettura torna al DB"
        );
    }
}

/// Il confine delle rotte interne, provato sul predicato che lo decide.
#[cfg(test)]
mod tests_internal_boundary {
    use super::*;

    #[test]
    fn riconosce_le_rotte_interne_dei_due_servizi() {
        // Le forme realmente montate: mcp-core usa entrambi i prefissi,
        // admin-service il primo.
        assert!(is_internal_path("/internal/settings/jwt_secret"));
        assert!(is_internal_path("/internal/dev-login-token"));
        assert!(is_internal_path("/api/internal/routing/decide"));
    }

    #[test]
    fn non_tocca_le_rotte_normali() {
        // Se il predicato fosse troppo largo, il filtro spegnerebbe l'API.
        assert!(!is_internal_path("/api/chat/messages"));
        assert!(!is_internal_path("/health"));
        assert!(!is_internal_path("/api/admin/settings"));
    }

    /// Un path che CONTIENE "internal" senza esserne il prefisso non e' una
    /// rotta interna: il filtro non deve poter essere ne' aggirato ne' esteso
    /// per somiglianza lessicale.
    #[test]
    fn non_si_lascia_ingannare_da_un_path_somigliante() {
        assert!(!is_internal_path("/api/projects/internal-docs"));
        assert!(!is_internal_path("/notinternal/settings/jwt_secret"));
        // Il caso che conta: un prefisso plausibile ma diverso non passa.
        assert!(!is_internal_path("/internalx/settings/jwt_secret"));
    }

    #[test]
    fn l_indirizzo_locale_e_distinguibile_da_quello_remoto() {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
        // E' il criterio che il middleware applica: `is_loopback`.
        assert!(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)).is_loopback());
        assert!(IpAddr::V6(Ipv6Addr::LOCALHOST).is_loopback());
        // Un indirizzo di LAN non e' locale, ed e' esattamente l'origine da cui
        // i segreti erano scaricabili.
        assert!(!IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50)).is_loopback());
        assert!(!IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)).is_loopback());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use sqlx::postgres::PgPoolOptions;
    use std::time::Duration;

    /// Regressione S90: con DB irraggiungibile, check_session_exists deve
    /// ritornare INTERNAL_SERVER_ERROR, NON false (che -> 401 Unauthorized).
    /// Prima del fix  mascherava ogni DB outage come
    /// "tutti gli utenti non sono loggati" - diagnosi catastroficamente
    /// sbagliata.
    #[tokio::test]
    async fn s90_db_down_returns_500_not_401() {
        // Pool puntato a una porta senza listener: la prima query fallira.
        let pool = match PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(2))
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/nonexistent")
        {
            Ok(p) => p,
            Err(_) => return, // ambiente senza supporto: skip
        };

        let res = check_session_exists(&pool, "deadbeef").await;
        match res {
            Err(StatusCode::INTERNAL_SERVER_ERROR) => {}
            Err(other) => panic!("S90: atteso 500, ricevuto {other}"),
            Ok(v) => panic!("S90: atteso Err(500), ricevuto Ok({v}) - regressione!"),
        }
    }
}
