//! Pipeline DLP/redaction del gateway (Fase 4).
//!
//! Porting fedele del sotto-modulo `packages/llm-gateway/src/redaction/` + il
//! ramo classificazione di `packages/llm-gateway/src/router/`. Componenti:
//!
//! - [`secret_scanner`]: scanner regex su STRINGA (primo punto Rust dei pattern
//!   del gateway, portati da `@nexus/shared`; vedi nota nel modulo e ADR 0026).
//! - [`presidio_client`]: client HTTP al PII detector, config da settings
//!   (regola G), fallback graceful se il servizio e' down (regola F: no leak).
//! - [`sensitivity_classifier`]: combina scanner + Presidio, eleva il tier.
//! - [`code_anonymizer`]: anonimizza identificatori/segreti/literal ad alta entropia.
//! - [`path_policy`]: whitelist/blacklist sui path (glob -> regex, niente nuova dep).
//! - [`redaction_map`]: mappa placeholder->originale per la reidratazione.
//! - [`pipeline`]: orchestratore pre-flight (redazione) e post-flight (reidratazione).
//!
//! Regola F: nessun modulo logga il testo scansionato, i segreti o i valori PII;
//! solo conteggi e tipi. Regola L: composizione (struct + funzioni), niente
//! ereditarieta'.

pub mod code_anonymizer;
pub mod path_policy;
pub mod pipeline;
pub mod presidio_client;
pub mod redaction_map;
pub mod secret_scanner;
pub mod sensitivity_classifier;
