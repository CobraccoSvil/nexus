//! Autenticazione del gateway.
//!
//! Porting dell'hook `preHandler` di `server.ts`:
//!   - `/health` e `/providers` sono PUBBLICI (esenti);
//!   - se il JWT secret non e' configurato (dev), si lascia passare;
//!   - altrimenti serve un `Authorization: Bearer <token>` valido, dove `<token>`
//!     e' il service token (chiamate interne mcp-core -> gateway) oppure un JWT
//!     firmato col secret di piattaforma.
//!
//! Riuso punto unico (regola L): la validazione JWT usa `nexus_auth::Claims` e
//! `jsonwebtoken` con lo stesso algoritmo/validazione del resto della piattaforma.
//! Regola F: niente token nei log; in caso di fallimento si logga solo il path.

use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use jsonwebtoken::{decode, DecodingKey, Validation};
use nexus_auth::Claims;

use super::AppState;

/// Estrae il token bearer dall'header `Authorization`, se presente e ben formato.
pub fn bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

/// Decide se un token e' valido per lo stato corrente. Logica pura e testabile:
///   - senza token: rifiuta;
///   - se il token coincide col service token: passa (bypass interno);
///   - altrimenti il token deve essere un JWT firmato col secret di piattaforma.
///
/// NON esiste piu' il ramo "nessun secret configurato -> passa tutto". Era
/// raggiungibile per costruzione (`jwt_secret` e' seminata vuota e generata al
/// primo login, mentre il gateway ne prendeva uno snapshot all'avvio), e in
/// quello stato il gateway accettava qualunque richiesta anche SENZA header.
/// Ora `AppState::jwt_secret` e' una `String` garantita dal bootstrap: lo stato
/// permissivo non e' rappresentabile.
pub fn token_is_valid(state: &AppState, token: Option<&str>) -> bool {
    let secret = state.jwt_secret.as_str();

    let Some(token) = token else {
        return false;
    };

    // Non esiste piu' un confronto con un bearer STATICO condiviso. Le chiamate
    // interne (mcp-core -> gateway) presentano un JWT a vita breve firmato con
    // la stessa chiave di piattaforma (`nexus_auth::service_bearer`), quindi
    // passano dalla validazione ordinaria qui sotto: un percorso solo, nessun
    // ramo privilegiato da tenere allineato.
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .is_ok()
}

/// Middleware di autenticazione. Esenta i path pubblici e applica `token_is_valid`.
pub async fn require_auth(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let path = req.uri().path();
    if path == "/health" || path == "/providers" {
        return Ok(next.run(req).await);
    }

    let token = bearer_token(req.headers());
    if token_is_valid(&state, token) {
        Ok(next.run(req).await)
    } else {
        tracing::warn!(path, "gateway: richiesta non autorizzata");
        Err(StatusCode::UNAUTHORIZED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cooldown::CooldownManager;
    use crate::model_alias_resolver::ModelAliasResolver;
    use crate::policy_engine::PolicyEngine;
    use crate::redaction::presidio_client::PresidioClient;
    use crate::server::bootstrap::GatewayConfig;
    use crate::server::RuntimeState;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use std::sync::Arc;

    const POLICY: &str = r#"
profile: cloud
routing:
  tier_0:
    primary: openai
"#;
    const ALIASES: &str = "aliases: {}";

    /// Stato di test SENZA pool reale: usa un pool lazy che non viene mai
    /// interrogato dai test di auth (la logica e' pura). `connect_lazy` richiede
    /// un contesto tokio per costruire il pool, quindi i test che lo usano sono
    /// `#[tokio::test]` (anche se la validazione e' sincrona e non tocca il DB).
    fn state(jwt_secret: String) -> Option<AppState> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://nobody:nopass@127.0.0.1:1/none")
            .ok()?;
        let runtime = RuntimeState {
            providers: vec![],
            policy: Arc::new(PolicyEngine::from_yaml_str(POLICY).unwrap()),
            aliases: Arc::new(ModelAliasResolver::from_yaml_str(ALIASES).unwrap()),
            presidio: PresidioClient::new(),
            profile: "cloud".to_string(),
            config: Arc::new(GatewayConfig {
                profile: "cloud".to_string(),
                aliases_file: "x".to_string(),
                policy_file: "y".to_string(),
            }),
            timeouts: nexus_auth::llm_timeouts::LlmTimeouts::defaults(),
        };
        Some(AppState {
            db: pool,
            jwt_secret,
            mcp_core_url: "http://localhost:4000".to_string(),
            cooldown: CooldownManager::new(),
            // Questi test misurano l'AUTENTICAZIONE: nessuna richiesta arriva a
            // un provider, quindi il catalogo non viene mai interrogato. Una
            // mappa vuota qui e' la dichiarazione che non c'e' nulla da
            // classificare, non un catalogo di comodo.
            vocabolario_errori: crate::tassonomia_errori::VocabolarioErrori::con_mappa(
                crate::tassonomia_errori::Mappa::da_righe([]),
            ),
            runtime: Arc::new(tokio::sync::RwLock::new(runtime)),
        })
    }

    fn make_jwt(secret: &str) -> String {
        let claims = Claims {
            sub: "00000000-0000-0000-0000-000000000000".to_string(),
            role: "user".to_string(),
            exp: 9_999_999_999,
        };
        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    /// Al posto del vecchio `dev_mode_senza_secret_passa_sempre`, che asseriva
    /// come CORRETTO il bypass poi rivelatosi il difetto: con `jwt_secret =
    /// None` il gateway accettava qualunque richiesta, anche senza header, e
    /// quello stato era raggiungibile per costruzione (chiave seminata vuota,
    /// generata al primo login, snapshot preso dal gateway all'avvio).
    ///
    /// Oggi `AppState::jwt_secret` e' una `String`: lo stato permissivo non e'
    /// rappresentabile e questo test lo fissa dal lato osservabile — nessuna
    /// richiesta passa senza una credenziale valida.
    #[tokio::test]
    async fn nessuna_richiesta_passa_senza_credenziale() {
        let Some(st) = state("a".repeat(40)) else {
            return; // ambiente senza supporto pool lazy: skip
        };
        assert!(
            !token_is_valid(&st, None),
            "senza token non si passa: era il bypass dell'auth"
        );
        assert!(
            !token_is_valid(&st, Some("qualunque-cosa")),
            "un token arbitrario non deve passare"
        );
        assert!(
            !token_is_valid(&st, Some("")),
            "il token vuoto non deve coincidere con nulla"
        );
    }

    /// Il bearer statico condiviso non esiste piu': una stringa nota non e' piu'
    /// una credenziale. Prima `"dev-internal-token"` — un valore scritto nel
    /// sorgente — apriva il gateway; questo test lo fissa come rifiutato.
    #[tokio::test]
    async fn il_vecchio_bearer_statico_non_e_piu_una_credenziale() {
        let Some(st) = state("a".repeat(40)) else {
            return;
        };
        assert!(
            !token_is_valid(&st, Some("dev-internal-token")),
            "il bearer statico e' stato eliminato: deve essere rifiutato come \
             qualunque altra stringa"
        );
    }

    /// Le chiamate interne (mcp-core -> gateway) ora presentano un JWT di
    /// servizio firmato con la stessa chiave di piattaforma, quindi passano dal
    /// percorso di validazione ORDINARIO: nessun ramo privilegiato.
    #[tokio::test]
    async fn il_jwt_di_servizio_passa_dalla_validazione_ordinaria() {
        let secret = "a".repeat(40);
        let Some(st) = state(secret.clone()) else {
            return;
        };
        let claims = Claims {
            sub: nexus_auth::SERVICE_SUBJECT.to_string(),
            role: "service".to_string(),
            exp: 9_999_999_999,
        };
        let bearer = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();
        assert!(token_is_valid(&st, Some(&bearer)));
    }

    #[tokio::test]
    async fn token_assente_o_sbagliato_fallisce() {
        let secret = "a".repeat(40);
        let Some(st) = state(secret) else {
            return;
        };
        assert!(!token_is_valid(&st, None));
        assert!(!token_is_valid(&st, Some("token-inventato")));
    }

    #[tokio::test]
    async fn jwt_valido_passa_jwt_firmato_diverso_fallisce() {
        let secret = "s".repeat(40);
        let Some(st) = state(secret.clone()) else {
            return;
        };
        let good = make_jwt(&secret);
        assert!(token_is_valid(&st, Some(&good)));
        // JWT firmato con un secret diverso: rifiutato.
        let bad = make_jwt(&"x".repeat(40));
        assert!(!token_is_valid(&st, Some(&bad)));
    }

    #[test]
    fn bearer_token_estrae_solo_con_prefisso() {
        let mut h = axum::http::HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer abc123".parse().unwrap(),
        );
        assert_eq!(bearer_token(&h), Some("abc123"));

        let mut h2 = axum::http::HeaderMap::new();
        h2.insert(axum::http::header::AUTHORIZATION, "abc123".parse().unwrap());
        assert_eq!(bearer_token(&h2), None);

        assert_eq!(bearer_token(&axum::http::HeaderMap::new()), None);
    }
}
