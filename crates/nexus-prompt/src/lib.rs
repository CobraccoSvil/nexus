//! Composizione del system prompt: **quali** blocchi vi entrano e **come** si
//! rendono.
//!
//! Il confine di questo crate e' il dominio, non la comodita'. Qui sta cio' che
//! decide il CONTENUTO del prompt; la POSIZIONE dei blocchi resta il punto unico
//! di [`nexus_types::system_prompt`] (`CONFINE_DI_TURNO`, `parte_stabile`), che
//! questo crate usa e non duplica — un blocco ricalcolato messo in testa taglia
//! il riuso del prefisso di tutto cio' che lo segue, e quella decisione ha gia'
//! il suo posto.
//!
//! Perche' esiste (2026-08-05): questi moduli vivevano in `mcp-core`, che era un
//! **binario puro**. Il catalogo di CLAUDE.md li dichiarava punti unici, ma un
//! bin non e' linkabile: nessun altro crate poteva delegarvi, e la regola L
//! — "i call site delegano al punto unico" — era, fuori da mcp-core,
//! inapplicabile per costruzione.

pub mod ambiente;
pub mod blocchi;
pub mod learned;
pub mod processo;
