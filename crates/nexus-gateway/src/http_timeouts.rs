//! Timeout HTTP del gateway verso i provider (regola G).
//!
//! Le chiavi `gateway.complete_timeout_seconds` e `gateway.stream_timeout_seconds`
//! sono seedate in mig 0421. Il client HTTP condiviso tra tutti i provider usa il
//! massimo dei due: le completion non-streaming rispettano il primo, lo streaming
//! il secondo (tipicamente piu' lungo).

use std::time::Duration;

use sqlx::PgPool;

/// Default mig 0421: timeout completion non-streaming verso il provider.
pub const DEFAULT_COMPLETE_TIMEOUT_SECS: u64 = 120;
/// Default mig 0421: timeout streaming SSE verso il provider.
pub const DEFAULT_STREAM_TIMEOUT_SECS: u64 = 300;

fn parse_positive_u64(raw: Option<String>, default: u64) -> u64 {
    raw.and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

/// Timeout per richiesta HTTP verso un provider (completion o stream).
/// Usa `max(complete, stream)` perche' il client e' condiviso.
pub async fn resolve_provider_http_timeout(db: &PgPool) -> Duration {
    let complete = parse_positive_u64(
        nexus_auth::get_setting(db, "gateway.complete_timeout_seconds").await,
        DEFAULT_COMPLETE_TIMEOUT_SECS,
    );
    let stream = parse_positive_u64(
        nexus_auth::get_setting(db, "gateway.stream_timeout_seconds").await,
        DEFAULT_STREAM_TIMEOUT_SECS,
    );
    Duration::from_secs(complete.max(stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_positive_u64_default_su_vuoto() {
        assert_eq!(parse_positive_u64(None, 120), 120);
        assert_eq!(parse_positive_u64(Some("0".into()), 120), 120);
        assert_eq!(parse_positive_u64(Some("abc".into()), 120), 120);
    }

    #[test]
    fn parse_positive_u64_valorizza() {
        assert_eq!(parse_positive_u64(Some("90".into()), 120), 90);
    }
}
