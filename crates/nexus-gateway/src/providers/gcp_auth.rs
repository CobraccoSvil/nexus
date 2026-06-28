//! Auth Google Cloud Vertex AI per il provider Google (backend "vertex").
//!
//! Replica in Rust il meccanismo del brain (`brain/providers/google_provider.py`,
//! mig 0183): il backend e' selezionato via settings DB `google_provider_backend`
//! ("gemini" = API key direct, "vertex" = Service Account OAuth2). Per Vertex il
//! Service Account JSON e' letto dal DB (`google_vertex_credentials_json`,
//! is_secret=true); da esso si estraggono `client_email` + `private_key` (PEM
//! RSA) con cui si firma un JWT RS256 scambiato su
//! `https://oauth2.googleapis.com/token` (grant_type jwt-bearer) per ottenere un
//! access token Bearer. Il token e' cachato in memoria fino a poco prima della
//! scadenza.
//!
//! Regola G: niente fallback ADC/env/GOOGLE_APPLICATION_CREDENTIALS. La sola
//! fonte di verita' e' il DB. Regola F: la private key e il token non vengono mai
//! loggati in chiaro.

use std::time::Duration;

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// Endpoint del token OAuth2 di Google (scambio JWT-bearer -> access_token).
const GOOGLE_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

/// Scope OAuth2 richiesto per chiamare le API Vertex AI (aiplatform).
const VERTEX_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// Durata di validita' del JWT firmato (Google ammette fino a 3600s).
const JWT_LIFETIME_SECS: i64 = 3600;

/// Margine di sicurezza: il token cachato viene rinnovato questo numero di
/// secondi PRIMA della scadenza reale, per evitare di usarlo a cavallo della
/// scadenza (clock skew + latenza di rete).
const TOKEN_REFRESH_MARGIN_SECS: i64 = 120;

/// Chiavi settings (regola G) del backend Google. Stesse del brain (mig 0183).
pub const SETTING_BACKEND: &str = "google_provider_backend";
pub const SETTING_VERTEX_PROJECT: &str = "google_vertex_project";
pub const SETTING_VERTEX_LOCATION: &str = "google_vertex_location";
/// Region candidate per il DISCOVERY (list_models) e il fallback di region in
/// inference (mig 0476). CSV ordinato per preferenza: la prima e' UE
/// (data-residency), la prima che risponde non-404 vince. Assente => si usa la
/// sola `SETTING_VERTEX_LOCATION`.
pub const SETTING_VERTEX_DISCOVERY_LOCATIONS: &str = "google_vertex_discovery_locations";
pub const SETTING_VERTEX_CREDENTIALS_JSON: &str = "google_vertex_credentials_json";

/// Service Account Google deserializzato dal JSON in DB. Solo i campi usati per
/// il flusso JWT-bearer; gli altri (project_id, token_uri, ...) sono ignorati.
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceAccount {
    /// Sempre "service_account" per un SA valido.
    #[serde(default)]
    pub r#type: String,
    /// Email del SA, usata come `iss`/`sub` del JWT.
    pub client_email: String,
    /// Chiave privata RSA in PEM (`-----BEGIN PRIVATE KEY-----...`).
    pub private_key: String,
    /// Project id del SA (informativo; il project Vertex arriva dai settings).
    #[serde(default)]
    pub project_id: String,
}

impl ServiceAccount {
    /// Deserializza e valida un Service Account JSON (parita' con
    /// `_setup_vertex_credentials` del brain: `type=="service_account"` e campi
    /// minimi presenti). Niente IO, niente rete: testabile in isolamento.
    pub fn parse(json: &str) -> anyhow::Result<Self> {
        let sa: ServiceAccount = serde_json::from_str(json)
            .map_err(|e| anyhow::anyhow!("Service Account JSON non valido: {e}"))?;
        if sa.r#type != "service_account" {
            anyhow::bail!(
                "Service Account JSON type='{}' non e' 'service_account'",
                sa.r#type
            );
        }
        if sa.client_email.trim().is_empty() {
            anyhow::bail!("Service Account JSON: client_email vuoto");
        }
        if sa.private_key.trim().is_empty() {
            anyhow::bail!("Service Account JSON: private_key vuoto");
        }
        Ok(sa)
    }
}

/// Claims del JWT-bearer richiesto da Google per il grant
/// `urn:ietf:params:oauth:grant-type:jwt-bearer`.
///
/// - `iss`/`sub`: email del Service Account;
/// - `scope`: spazio dei permessi (cloud-platform per Vertex);
/// - `aud`: SEMPRE l'endpoint del token (non l'API di destinazione);
/// - `iat`/`exp`: finestra di validita' (<= 3600s).
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct JwtClaims {
    pub iss: String,
    pub scope: String,
    pub aud: String,
    pub exp: i64,
    pub iat: i64,
}

impl JwtClaims {
    /// Costruisce i claim per lo scambio JWT-bearer. `now_unix` iniettato cosi'
    /// il test e' deterministico (niente lettura dell'orologio reale).
    pub fn new(client_email: &str, now_unix: i64) -> Self {
        Self {
            iss: client_email.to_string(),
            scope: VERTEX_SCOPE.to_string(),
            aud: GOOGLE_TOKEN_URI.to_string(),
            exp: now_unix + JWT_LIFETIME_SECS,
            iat: now_unix,
        }
    }
}

/// Firma i claim con la private key RSA del SA (algoritmo RS256). Funzione pura
/// (nessuna rete): testabile con una chiave RSA generata in-test.
pub fn sign_jwt(claims: &JwtClaims, private_key_pem: &str) -> anyhow::Result<String> {
    let key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
        .map_err(|e| anyhow::anyhow!("private key del Service Account non valida (PEM RSA): {e}"))?;
    let token = encode(&Header::new(Algorithm::RS256), claims, &key)
        .map_err(|e| anyhow::anyhow!("firma JWT RS256 fallita: {e}"))?;
    Ok(token)
}

/// Costruisce l'URL Vertex AI per una `action` arbitraria sul modello (punto
/// unico, regola L): la chat usa `generateContent`/`streamGenerateContent`,
/// l'image-gen usa `predict` (Imagen). project/location dai settings (regola G).
///
/// `https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}/publishers/google/models/{model}:{action}`
pub fn vertex_action_endpoint(project: &str, location: &str, model: &str, action: &str) -> String {
    format!(
        "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}/publishers/google/models/{model}:{action}"
    )
}

/// Costruisce l'URL Vertex AI per l'azione `generateContent` /
/// `streamGenerateContent` (chat). Delega a [`vertex_action_endpoint`] e aggiunge
/// `?alt=sse` per lo streaming. Firma invariata per i chiamanti chat esistenti.
pub fn vertex_endpoint(project: &str, location: &str, model: &str, stream: bool) -> String {
    let action = if stream {
        "streamGenerateContent"
    } else {
        "generateContent"
    };
    let mut url = vertex_action_endpoint(project, location, model, action);
    if stream {
        url.push_str("?alt=sse");
    }
    url
}

/// Risposta del token endpoint OAuth2 Google.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    /// Secondi alla scadenza (di norma 3599).
    #[serde(default)]
    expires_in: i64,
}

/// Token in cache con il suo istante di scadenza (epoch secondi).
#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    /// Epoch (secondi) oltre il quale il token va considerato scaduto.
    expires_at: i64,
}

/// Auth Vertex con cache in-memory del token Bearer. Una istanza per provider:
/// `Mutex<Option<CachedToken>>` evita scambi concorrenti ridondanti e protegge
/// la sezione critica del refresh.
pub struct VertexAuth {
    http: Client,
    service_account: ServiceAccount,
    cached: Mutex<Option<CachedToken>>,
}

impl VertexAuth {
    /// Costruisce l'auth da un Service Account gia' parsato.
    pub fn new(http: Client, service_account: ServiceAccount) -> Self {
        Self {
            http,
            service_account,
            cached: Mutex::new(None),
        }
    }

    /// Costruisce l'auth dal JSON grezzo del SA (valida + parsa).
    pub fn from_credentials_json(http: Client, credentials_json: &str) -> anyhow::Result<Self> {
        let sa = ServiceAccount::parse(credentials_json)?;
        Ok(Self::new(http, sa))
    }

    /// Restituisce un access token Bearer valido, riusando la cache finche' il
    /// token non e' prossimo alla scadenza (margine `TOKEN_REFRESH_MARGIN_SECS`).
    /// Altrimenti firma un nuovo JWT e lo scambia con Google. Doppio controllo
    /// dentro il lock per evitare refresh concorrenti ridondanti.
    pub async fn access_token(&self) -> anyhow::Result<String> {
        let now = now_unix();
        {
            let guard = self.cached.lock().await;
            if let Some(tok) = guard.as_ref() {
                if tok.expires_at - TOKEN_REFRESH_MARGIN_SECS > now {
                    return Ok(tok.access_token.clone());
                }
            }
        }
        // Refresh: prende il lock, ricontrolla (un altro task potrebbe aver gia'
        // rinnovato mentre attendevamo), altrimenti scambia un nuovo token.
        let mut guard = self.cached.lock().await;
        let now = now_unix();
        if let Some(tok) = guard.as_ref() {
            if tok.expires_at - TOKEN_REFRESH_MARGIN_SECS > now {
                return Ok(tok.access_token.clone());
            }
        }
        let fresh = self.exchange_token(now).await?;
        let token = fresh.access_token.clone();
        *guard = Some(fresh);
        Ok(token)
    }

    /// Firma il JWT e lo scambia con Google per un access token. Separata cosi'
    /// la logica di firma/parse e' testabile senza rete; questa funzione e' l'unico
    /// punto che effettua IO HTTP verso Google.
    async fn exchange_token(&self, now: i64) -> anyhow::Result<CachedToken> {
        let claims = JwtClaims::new(&self.service_account.client_email, now);
        let assertion = sign_jwt(&claims, &self.service_account.private_key)?;

        let params = [
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:jwt-bearer",
            ),
            ("assertion", assertion.as_str()),
        ];

        let resp = self
            .http
            .post(GOOGLE_TOKEN_URI)
            .form(&params)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("richiesta token Vertex fallita: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            // Regola F: il body puo' contenere dettagli ma non segreti; lo
            // propaghiamo al caller per il cooldown, non lo logghiamo qui.
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("token endpoint Vertex HTTP {}: {}", status.as_u16(), body);
        }

        let parsed: TokenResponse = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("risposta token Vertex non valida: {e}"))?;
        // expires_in tipicamente 3599; se assente/0 usiamo la vita del JWT come
        // tetto prudente.
        let lifetime = if parsed.expires_in > 0 {
            parsed.expires_in
        } else {
            JWT_LIFETIME_SECS
        };
        Ok(CachedToken {
            access_token: parsed.access_token,
            expires_at: now + lifetime,
        })
    }
}

/// Epoch corrente in secondi. Isolata per non spargere `SystemTime` nel codice.
fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chiave privata RSA 2048 (PKCS#8 PEM) generata APPOSTA per i test con
    /// `openssl genpkey` — NON e' una credenziale di produzione e non protegge
    /// nulla. Serve solo a far passare la firma RS256 reale in `sign_jwt` senza
    /// dipendere dalla crate `rsa` per generarla a runtime.
    const TEST_RSA_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCOUu5Salq3UWds\nLaOaDB9gIkhwp15efXHHpDMeCKVEu4+VRPEKPkL0F2cL0VQ2HAtgblAQtKXnuqmN\nC4BOvQWqraf9A/jPc3AFHc96pB7WkKyfH9iqFnBxnlF+hXDTa3jfEVr74guRjGol\nAihzndGI9PcLPrsLCw4jO3escGyaNCxApb/jK/KVXq8amUuCJjXNaxLypAHF+xMG\n9C/Hi52CncrQt19Qro9kULhR5PIHci4yScAlT8cQUyW51P2XNbGVxtMam9FzTKrq\n6i7F42TfY98jfVYvTPJJtV9KtllpQn3trVpvfDxVPlkBqJXwG+1XInzEWuUxdFg9\naz1z0ouBAgMBAAECggEACFlUErCbfrp/QyjQOpawdn68WiNvVUKtnIUE3KOsMkKA\nc0S2kR/C5LfEuzw94Oi3QCJofvph6xyXmqcMkVHkVbVXL+/+NgkzDpHHnI2pI3Qc\nND91gdDMKtYbOL1SN4zL6+YEPNdvT0v55A7i2Zlt88dPALFe3pB49VclN4/mxwrn\nEicqZ8XS7tEh7JxBPjdrFpux0ngtRD9V7sqUmq64St41Z4RJXdvrIacHu9JKhDwS\n4hk4sFZSr9rb/KsDYGOPV3UdgWQT9ASjaw5pjo/WkArimV1iGDYhmy7h8icinqGf\nK8N+69waem+fHB6NPBq1uax2okhcc6ZtKn6h7KffGQKBgQDBM0mug1GsGyzhTYsZ\naTEst8ot/jwjbr+0MVDTMWks+G+sobS8LgSl+KyATHZPu1MoasKvo6bApdcrhQrM\nd4wu73xzVzQcwQUv/EdyM67Tklc1Srtp3htobQs+VFkRNqtDwnXM6xvJpsVYrLpF\n8J+fguCmQPG/1+OzUv6HBI0tfQKBgQC8lhVp1244t6bIuV3YcBPhB5e+OPV0GoeR\nsyTzOcmtupcLH9C8oRAxTFWExkVtKGx1EiIY0380ruapsuAYubgcd9kK7dEGsaP5\n3tUJaoaHp1BXZLHYPADXUSD7GrdkPzn7plQKWNwDk2XSGmwKorKvWx6xJAXYvSgL\ns1YC5HsFVQKBgCeOQqWySUR9g+DVKYeYW/MV5hUomlN+100jU7MJyMjmTKcBrKli\nmp0InvjdrtOAPPRnd1jSns+OpNFKOf3G9DKf4dETp53DfzZl5pLhNggnTCejF2AD\nk4T73nNyfZHiqYoEBG5iLJxKwtj29GRhca0M9MXVQinPH9WVRnuKWQVZAoGAJJOb\nbZ7aAugj0hSZzgvW3zNgzAUyHiWzG6A6T25q3mYtO3wdOSioAlfC0nC+MHDBsGcm\n89e5eVde55UI/+KtgeAA2azMeNblbeY5PY1KsG7UF08xazYgF6Llma5R2YCl11go\nPqWDbrIc8oYrZFiv/XDX4BWTiLkPVk2fJgp4jc0CgYEAg4pwfbEFJgXYckS/h2mG\nxa3PnTIPijxLGZIoVb7lC7DjqpBEhVYq5C8KALUWAK8GSKzq4Ch8ploQUXB6u8w8\nn0/5if4vCfFngSAKkzF/rzDIYgM+6wXv4rURZvJSjl+Ho0TZMhhQbltBp4SV7lyB\nLxTdI3A04yWS/ct/UV5Alb8=\n-----END PRIVATE KEY-----\n";

    fn sample_sa_json() -> String {
        serde_json::json!({
            "type": "service_account",
            "project_id": "nexus-test",
            "private_key_id": "abc123",
            "private_key": "-----BEGIN PRIVATE KEY-----\nFAKE\n-----END PRIVATE KEY-----\n",
            "client_email": "nexus-sa@nexus-test.iam.gserviceaccount.com",
            "client_id": "9999",
            "token_uri": "https://oauth2.googleapis.com/token"
        })
        .to_string()
    }

    #[test]
    fn parse_service_account_ok() {
        let sa = ServiceAccount::parse(&sample_sa_json()).expect("SA valido");
        assert_eq!(sa.r#type, "service_account");
        assert_eq!(
            sa.client_email,
            "nexus-sa@nexus-test.iam.gserviceaccount.com"
        );
        assert_eq!(sa.project_id, "nexus-test");
        assert!(sa.private_key.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn parse_service_account_tipo_errato() {
        let json = serde_json::json!({
            "type": "authorized_user",
            "client_email": "x@y.z",
            "private_key": "-----BEGIN PRIVATE KEY-----\nX\n-----END PRIVATE KEY-----\n"
        })
        .to_string();
        let err = ServiceAccount::parse(&json).unwrap_err();
        assert!(err.to_string().contains("service_account"));
    }

    #[test]
    fn parse_service_account_json_invalido() {
        let err = ServiceAccount::parse("{ non json").unwrap_err();
        assert!(err.to_string().contains("non valido"));
    }

    #[test]
    fn parse_service_account_campi_mancanti() {
        // client_email assente -> errore di deserializzazione (campo richiesto).
        let json = serde_json::json!({
            "type": "service_account",
            "private_key": "-----BEGIN PRIVATE KEY-----\nX\n-----END PRIVATE KEY-----\n"
        })
        .to_string();
        assert!(ServiceAccount::parse(&json).is_err());
    }

    #[test]
    fn jwt_claims_struttura() {
        let now = 1_700_000_000;
        let claims = JwtClaims::new("sa@nexus.iam.gserviceaccount.com", now);
        assert_eq!(claims.iss, "sa@nexus.iam.gserviceaccount.com");
        assert_eq!(claims.scope, "https://www.googleapis.com/auth/cloud-platform");
        // aud DEVE essere l'endpoint del token, non l'API di destinazione.
        assert_eq!(claims.aud, "https://oauth2.googleapis.com/token");
        assert_eq!(claims.iat, now);
        assert_eq!(claims.exp, now + 3600);
        assert!(claims.exp > claims.iat);
    }

    #[test]
    fn sign_jwt_con_pem_invalido_fallisce() {
        let claims = JwtClaims::new("sa@x.iam.gserviceaccount.com", 1_700_000_000);
        let err = sign_jwt(&claims, "non una chiave pem").unwrap_err();
        assert!(err.to_string().contains("private key"));
    }

    #[test]
    fn sign_jwt_con_pem_rsa_valida_produce_tre_segmenti() {
        // Usa la chiave RSA reale di test embeddata (no rete, no segreti di prod).
        let claims = JwtClaims::new("sa@x.iam.gserviceaccount.com", 1_700_000_000);
        let token = sign_jwt(&claims, TEST_RSA_PRIVATE_KEY).expect("firma ok");
        // Un JWT ha tre parti separate da '.': header.payload.signature.
        assert_eq!(token.split('.').count(), 3);
        // Il primo segmento (header) decodificato deve dichiarare RS256.
        let header = token.split('.').next().unwrap();
        use base64::Engine;
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(header)
            .expect("header base64url");
        let header_json: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(header_json["alg"], "RS256");

        // Il payload deve contenere i claim attesi (round-trip).
        let payload_seg = token.split('.').nth(1).unwrap();
        let payload_raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload_seg)
            .expect("payload base64url");
        let payload: JwtClaims = serde_json::from_slice(&payload_raw).unwrap();
        assert_eq!(payload, claims);
    }

    #[test]
    fn vertex_endpoint_generate() {
        let url = vertex_endpoint("nexus-test", "europe-west4", "gemini-2.0-flash", false);
        assert_eq!(
            url,
            "https://europe-west4-aiplatform.googleapis.com/v1/projects/nexus-test/locations/europe-west4/publishers/google/models/gemini-2.0-flash:generateContent"
        );
    }

    #[test]
    fn vertex_action_endpoint_predict_per_imagen() {
        let url = vertex_action_endpoint("nexus-test", "europe-west4", "imagen-3.0", "predict");
        assert_eq!(
            url,
            "https://europe-west4-aiplatform.googleapis.com/v1/projects/nexus-test/locations/europe-west4/publishers/google/models/imagen-3.0:predict"
        );
        // Nessun ?alt=sse: e' una chiamata predict non-streaming.
        assert!(!url.contains("alt=sse"));
    }

    #[test]
    fn vertex_endpoint_stream_aggiunge_alt_sse() {
        let url = vertex_endpoint("nexus-test", "us-central1", "gemini-2.5-pro", true);
        assert!(url.ends_with(
            "/publishers/google/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        ));
        assert!(url.starts_with("https://us-central1-aiplatform.googleapis.com/v1/projects/nexus-test/locations/us-central1/"));
    }

    #[test]
    fn from_credentials_json_valida_il_sa() {
        // SA con private key non-PEM: parse OK (validazione PEM avviene solo alla
        // firma), ma type errato deve fallire subito.
        let bad = serde_json::json!({
            "type": "user", "client_email": "x@y.z",
            "private_key": "-----BEGIN PRIVATE KEY-----\nX\n-----END PRIVATE KEY-----\n"
        })
        .to_string();
        assert!(VertexAuth::from_credentials_json(Client::new(), &bad).is_err());
        // SA strutturalmente valido -> costruzione OK.
        assert!(VertexAuth::from_credentials_json(Client::new(), &sample_sa_json()).is_ok());
    }

}
