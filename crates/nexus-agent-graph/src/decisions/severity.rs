//! `severity`: RI-ESPORTA il punto unico del vocabolario di GRAVITA' degli
//! elementi di evidenza dei panel, che vive in [`nexus_types::severity`].
//!
//! Il modulo nasceva qui, accanto ai suoi primi chiamanti (i panel del grafo),
//! ma un VOCABOLARIO non appartiene al motore agentico. Lo dichiarava gia' il
//! suo stesso doc: «e' il vocabolario dichiarato dagli SCHEMI dei tool
//! advisory_verdict / review_verdict / debate_position ... si estende QUI e
//! negli schemi». Cioe' due posti — e finche' gli schemi erano JSON scritto a
//! mano, nessuno poteva accorgersi se divergevano.
//!
//! Col contratto d'ingresso (`nexus-agent-tools::tool_inputs`) gli schemi sono
//! diventati tipi Rust, e quei tre tool avrebbero avuto bisogno di un enum con
//! gli stessi tre valori — un gemello che nessun compilatore obbliga a restare
//! allineato, cioe' la duplicazione nella sua forma peggiore. Ma
//! `nexus-agent-tools` non vede questo crate: un punto unico raggiungibile solo
//! da meta' dei suoi chiamanti smette di essere unico alla prima riga che non
//! puo' importarlo.
//!
//! Il modulo e' quindi migrato in `nexus-types` (crate leaf, gia' dipendenza di
//! tutti) e qui resta il re-export: i call site storici
//! (`super::severity::any_high`, `decisions::severity::Severity`) non cambiano
//! una riga. Stessa mossa, stessa motivazione e stesso esito di
//! [`super::tiers`], che l'aveva gia' fatta per la scala performance-tier.

pub use nexus_types::severity::*;
