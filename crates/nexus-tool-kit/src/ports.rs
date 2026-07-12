//! Bucket deterministico delle porte di progetto + porte riservate Nexus.
//! Punto unico (regola L), estratto da project_workspace::services nello
//! split 7.4 fase B: il sandbox (in questo crate) valida le PORT contro
//! il bucket; mcp-core re-esporta da project_workspace::services.

use uuid::Uuid;

/// Porte riservate da Nexus e dai suoi servizi di infrastruttura.
/// I processi di progetto NON devono mai usare queste porte.
///
/// Range riservato HTTP:  4000–4079  (microservizi Nexus)
/// Range riservato gRPC:  4100–4139  (canali gRPC interni, target migrazione)
/// Porte gRPC attuali:    50051–50501 (in uso finché non migrati)
/// Progetti utente:       20000-39999 (bucket assegnati da find_free_project_port)
pub const NEXUS_RESERVED_PORTS: &[u16] = &[
    // Porte di sistema
    80, 443,
    // ── HTTP Nexus (4000-4079) ─────────────────────────────────────────────
    4000, // mcp-core HTTP
    4001, // web-ide (target migrazione da 3000)
    4010, // admin-service
    4020, // ex chat-service (crate rimosso, porta resta riservata nel bucket)
    4030, // doc-service
    4040, // billing-service
    4050, // plugin-service
    4060, // nexus-gateway
    4070, // neural-core REST (target migrazione da 8001)
    // ── gRPC interno Nexus (4100-4139, target migrazione) ─────────────────
    4100, // neural-core gRPC (target da 50051)
    4110, // tool-runner gRPC (target da 50500)
    4120, // agent-router gRPC (target da 50501)
    4130, // presidio gRPC (target da 50052)
    // ── web-ide attuale ───────────────────────────────────────────────────
    3000, // Nexus web-ide (attuale)
    // ── Porte gRPC attuali (porte alte, in uso finché non migrati) ────────
    8001,  // neural-core REST (attuale)
    50051, // neural-core gRPC
    50052, // presidio gRPC
    50500, // tool-runner gRPC (reale, vedi mig 0239)
    50501, // agent-router gRPC (reale, vedi mig 0190/0239)
    // ── Database e infrastruttura ─────────────────────────────────────────
    5432, 5433, 5434, // PostgreSQL (5432 host, 5433 cluster meta, 5434 cluster app)
    6333, 6334, // Qdrant REST + gRPC
    6379, // Redis
    8080, // nginx interno
    // ── Monitoring e observability ────────────────────────────────────────
    3001,  // Grafana
    4055,  // browser-bridge-mcp
    4317,  // OpenTelemetry Collector gRPC
    4318,  // OpenTelemetry Collector HTTP
    9090,  // Prometheus
    16686, // Jaeger UI
];

/// Range dedicato ai servizi dei progetti gestiti (deve evitare conflitti con Nexus e con servizi host comuni).
/// Scelta conservativa: porte alte non privilegiate, fuori dal range Nexus e fuori dai DB.
pub const PROJECT_PORT_RANGE_START: u16 = 20000;
pub const PROJECT_PORT_RANGE_END: u16 = 39999;
/// Numero porte per progetto nel bucket deterministico.
pub const PROJECT_PORT_BUCKET_SIZE: u16 = 50;

pub fn project_bucket_start(project_id: &Uuid) -> u16 {
    // Hash stabile: usa i primi 8 byte (big-endian) del UUID.
    let b = project_id.as_bytes();
    let mut v: u64 = 0;
    for &byte in b.iter().take(8) {
        v = (v << 8) | (byte as u64);
    }
    let buckets: u64 = ((PROJECT_PORT_RANGE_END - PROJECT_PORT_RANGE_START + 1) as u64)
        / (PROJECT_PORT_BUCKET_SIZE as u64);
    let idx = if buckets == 0 { 0 } else { v % buckets };
    PROJECT_PORT_RANGE_START + (idx as u16) * PROJECT_PORT_BUCKET_SIZE
}

/// True se `port` puo' essere REGISTRATA come porta esposta da un servizio di
/// progetto: deve stare nel bucket globale dei progetti
/// [`PROJECT_PORT_RANGE_START`, `PROJECT_PORT_RANGE_END`] e NON essere una porta
/// riservata Nexus/infrastruttura (`NEXUS_RESERVED_PORTS`).
///
/// Punto unico (regola L): il rilevamento porta-da-output dei servizi e la
/// registrazione in `nexus_port_allocations` delegano a questo predicato invece
/// di replicare la lista riservata o i confronti di range. Le porte
/// d'infrastruttura condivise (es. Postgres :5434) compaiono nei log come
/// destinazione di CONNESSIONE, non come listener del servizio: non vanno mai
/// registrate come porta del progetto.
pub fn is_project_registrable_port(port: u16) -> bool {
    !NEXUS_RESERVED_PORTS.contains(&port)
        && (PROJECT_PORT_RANGE_START..=PROJECT_PORT_RANGE_END).contains(&port)
}

