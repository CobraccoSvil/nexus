//! `tiers`: RI-ESPORTA il punto unico del vocabolario performance-tier, che vive
//! in [`nexus_types::tiers`].
//!
//! Il modulo nasceva qui, accanto al suo primo chiamante (l'escalation del
//! grafo), ma un VOCABOLARIO non appartiene al motore agentico: lo usa anche
//! `admin-service`, che non puo' dipendere da questo crate (nexus-graph, il
//! checkpointer sqlx, tokio-util: un servizio CRUD si tirerebbe dietro l'intero
//! motore per sapere che `heavy` viene dopo `high`). Un punto unico raggiungibile
//! solo da meta' dei suoi chiamanti smette di essere unico alla prima riga che
//! non puo' importarlo — e la scala tier ha gia' pagato quel prezzo una volta
//! (incidente 2026-07-15: 9 copie manuali, una rimasta a 3 livelli).
//!
//! Il modulo e' quindi migrato in `nexus-types` (crate leaf, gia' dipendenza di
//! tutti) e qui resta il re-export: i call site storici
//! (`nexus_agent_graph::decisions::tiers::*`, `super::tiers::tier_rank` in
//! `escalation`) non cambiano una riga, e la guard `tier-scale` di
//! `check-single-source.sh` continua a indicare un percorso valido.

pub use nexus_types::tiers::*;
