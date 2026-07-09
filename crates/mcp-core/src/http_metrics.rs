//! Histogram in-memory delle latenze HTTP per route (telemetria abilitante).
//!
//! Punto unico (regola L) per la misura della durata delle richieste HTTP di
//! mcp-core: il middleware `middleware::http_timing_middleware` registra qui,
//! l'endpoint `GET /nexus/metrics` (`nexus_bridge::nexus_prometheus`) appende
//! il render. La chiave di serie usa il template di route di axum
//! (`MatchedPath`, es. `/api/chat/sessions/{id}/messages`), MAI il path raw:
//! cosi' gli UUID non esplodono la cardinalita'.
//!
//! Nessuna dipendenza nuova: lo storage e' un `Mutex<HashMap>` (lock tenuto
//! per pochi nanosecondi a richiesta) e il formato di output e' il text
//! format Prometheus gia' usato dai contatori del bridge.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Upper bound dei bucket in millisecondi (piu' il bucket implicito +Inf).
/// Copre dal ping sub-10ms alle chiamate LLM multi-minuto.
const BUCKETS_MS: [f64; 12] = [
    5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 15000.0, 60000.0,
];

#[derive(Default)]
struct Series {
    /// Conteggi cumulativi per bucket (convenzione Prometheus: le <= bound).
    buckets: [u64; BUCKETS_MS.len()],
    count: u64,
    sum_ms: f64,
}

type SeriesKey = (String, String, u16); // (route template, method, status)

static SERIES: OnceLock<Mutex<HashMap<SeriesKey, Series>>> = OnceLock::new();

fn series() -> &'static Mutex<HashMap<SeriesKey, Series>> {
    SERIES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Registra una richiesta completata. `route` e' il template di MatchedPath.
pub fn record(route: &str, method: &str, status: u16, elapsed: Duration) {
    let ms = elapsed.as_secs_f64() * 1000.0;
    let Ok(mut map) = series().lock() else {
        return; // lock avvelenato: telemetria best-effort, mai panicare il path HTTP
    };
    let s = map
        .entry((route.to_string(), method.to_string(), status))
        .or_default();
    for (i, bound) in BUCKETS_MS.iter().enumerate() {
        if ms <= *bound {
            s.buckets[i] += 1;
        }
    }
    s.count += 1;
    s.sum_ms += ms;
}

/// Render del blocco histogram in text format Prometheus, da appendere
/// all'output di `/nexus/metrics`. Stringa vuota se nessuna richiesta ancora.
pub fn render_prometheus() -> String {
    let Ok(map) = series().lock() else {
        return String::new();
    };
    if map.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(4096);
    out.push_str("# HELP nexus_http_request_duration_ms HTTP request duration by route template\n");
    out.push_str("# TYPE nexus_http_request_duration_ms histogram\n");

    // Ordine stabile per output deterministico (test + diff scrape leggibili).
    let mut keys: Vec<&SeriesKey> = map.keys().collect();
    keys.sort();
    for key in keys {
        let (route, method, status) = key;
        let s = &map[key];
        let labels = format!("route=\"{route}\",method=\"{method}\",status=\"{status}\"");
        for (i, bound) in BUCKETS_MS.iter().enumerate() {
            out.push_str(&format!(
                "nexus_http_request_duration_ms_bucket{{{labels},le=\"{bound}\"}} {}\n",
                s.buckets[i]
            ));
        }
        out.push_str(&format!(
            "nexus_http_request_duration_ms_bucket{{{labels},le=\"+Inf\"}} {}\n",
            s.count
        ));
        out.push_str(&format!(
            "nexus_http_request_duration_ms_sum{{{labels}}} {:.3}\n",
            s.sum_ms
        ));
        out.push_str(&format!(
            "nexus_http_request_duration_ms_count{{{labels}}} {}\n",
            s.count
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_render_shape() {
        record("/api/test/{id}", "GET", 200, Duration::from_millis(42));
        record("/api/test/{id}", "GET", 200, Duration::from_millis(7));
        let out = render_prometheus();
        assert!(out.contains("# TYPE nexus_http_request_duration_ms histogram"));
        assert!(out.contains(
            "nexus_http_request_duration_ms_count{route=\"/api/test/{id}\",method=\"GET\",status=\"200\"} 2"
        ));
        // 7ms cade nel bucket le=10, 42ms no: il bucket 10 conta 1.
        assert!(out.contains("le=\"10\"} 1\n"));
        // Entrambe <= 50ms.
        assert!(out.contains("le=\"50\"} 2\n"));
    }
}
