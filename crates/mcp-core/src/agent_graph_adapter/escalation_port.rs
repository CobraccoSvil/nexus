//! Adapter del trait [`nexus_agent_graph::runtime::ports::EscalationPort`].
//!
//! IMPLEMENTA (FASE 2b) `EscalationPort::escalation_inputs` risolvendo gli input
//! dell'auto-escalation:
//!   1. catena intra-provider da `nexus_model_escalation_chain` (mig 0128) via
//!      `sqlx` — `WHERE provider=$1 AND base_model=$2 AND is_active=TRUE ORDER BY
//!      escalation_position ASC`, 1:1 con il Python (`helpers.py:1737-1739` /
//!      `__init__.py:3190-3196`);
//!   2. stato cooldown del provider corrente dalla FONTE UNICA del gate (ADR 0020,
//!      `crate::provider_cooldown::is_provider_in_cooldown`);
//!   3. candidato cross-provider risolvendo il purpose `loop_fallback_default`
//!      dalla routing matrix (regola G, `internal_routing::resolve_purpose_model_db`).
//!
//! FILTRO PROVIDER REGISTRATI (nota verifica PR-J1): il signature-loop Python ha la
//! guardia `_providers._providers.get(provider)` (`__init__.py:3200`): se la chain
//! DB punta a un provider NON disponibile runtime, il candidato Tier 1 viene
//! scartato e si cade al Tier 2 cross-provider. Qui replichiamo l'intento in modo
//! DB-driven (regola G): un provider e' "disponibile" se ha una API key configurata
//! in `settings` (categoria `providers`, `<provider>_api_key` non vuota). Se il
//! provider corrente NON e' disponibile, la catena (intra-provider) viene
//! AZZERATA: `pick_escalation_model` salta cosi' il Tier 1 e usa il cross-provider.
//!
//! FAIL-OPEN (sicurezza): su guasto di lettura (DB/router down) ritorna
//! `EscalationInputs::default()` (catena vuota, `provider_in_cooldown=false`,
//! `cross_provider=None`) -> la selezione risolve a `None` (chiusura secca), mai un
//! `PortError`. CONFINE (regola L): qui SOLO l'I/O; la SELEZIONE resta nel modulo
//! puro `nexus_agent_graph::decisions::escalation::pick_escalation_model`.

use async_trait::async_trait;
use sqlx::PgPool;

use nexus_agent_graph::decisions::escalation::{ChainEntry, CrossProviderCandidate};
use nexus_agent_graph::runtime::ports::{EscalationInputs, EscalationPort, PortError};

use crate::internal_routing::{resolve_purpose_model_db, PurposeResolution};
use crate::provider_cooldown::is_provider_in_cooldown;

/// Sentinelle del router cross-provider: NON sono provider reali (regola G), vanno
/// trattate come "nessun candidato" (parita' col Python `helpers.py:1753-1754`).
const SENTINELS: [&str; 2] = ["__router_unavailable__", "__no_capable_provider__"];

/// Adapter [`EscalationPort`] -> `nexus_model_escalation_chain` (mig 0128) + gate
/// cooldown (ADR 0020) + purpose `loop_fallback_default` (routing matrix).
pub struct PgEscalationPort {
    /// Pool Postgres per la lettura della catena di escalation, dei provider
    /// disponibili (`settings`) e per la risoluzione del purpose cross-provider.
    db: PgPool,
}

impl PgEscalationPort {
    /// Costruisce l'adapter sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// `true` se il provider e' disponibile runtime (API key configurata in
    /// `settings`, categoria `providers`, chiave `<provider>_api_key` non vuota).
    /// Replica DB-driven della guardia Python `_providers._providers.get(provider)`
    /// (`__init__.py:3200`): un provider senza chiave non e' realmente disponibile,
    /// quindi escalare su di lui sprecherebbe un turno. FAIL-OPEN: su errore DB
    /// ritorna `true` (NON priva il run del Tier 1 per un guasto infrastrutturale).
    async fn provider_available(&self, provider: &str) -> bool {
        let key = format!("{}_api_key", provider.trim().to_lowercase());
        match sqlx::query_scalar::<_, String>(
            "SELECT value FROM settings \
             WHERE category = 'providers' AND key = $1 LIMIT 1",
        )
        .bind(&key)
        .fetch_optional(&self.db)
        .await
        {
            Ok(Some(value)) => !value.trim().is_empty(),
            Ok(None) => false,
            Err(e) => {
                tracing::warn!(
                    provider = %provider,
                    error = %e,
                    "escalation_port: lettura disponibilita' provider fallita, fail-open disponibile"
                );
                true
            }
        }
    }

    /// Catena intra-provider per `(provider, base_model)` dalla tabella mig 0128,
    /// gia' filtrata `is_active=TRUE` e ordinata per `escalation_position` ASC.
    /// Vuota su qualunque errore (best-effort) o se i parametri sono assenti.
    async fn chain_for(&self, provider: &str, base_model: &str) -> Vec<ChainEntry> {
        if provider.trim().is_empty() || base_model.trim().is_empty() {
            return Vec::new();
        }
        match sqlx::query_scalar::<_, String>(
            "SELECT escalation_model FROM nexus_model_escalation_chain \
             WHERE provider = $1 AND base_model = $2 AND is_active = TRUE \
             ORDER BY escalation_position ASC",
        )
        .bind(provider)
        .bind(base_model)
        .fetch_all(&self.db)
        .await
        {
            Ok(rows) => rows
                .into_iter()
                .map(|escalation_model| ChainEntry { escalation_model })
                .collect(),
            Err(e) => {
                tracing::warn!(
                    provider = %provider,
                    error = %e,
                    "escalation_port: lettura catena escalation fallita, fail-open catena vuota"
                );
                Vec::new()
            }
        }
    }

    /// Candidato cross-provider (`loop_fallback_default`) dal router. Eleggibile
    /// SOLO la variante `Resolved` (tier-only, niente fallback hardcoded: ogni
    /// altro esito — NotFound / NoCapableModel / MatrixUnavailable — NON e' un
    /// candidato valido, regola G/H). `None` anche su sentinella o coppia vuota.
    /// Best-effort: ogni esito non-risolto -> `None`.
    async fn cross_provider(&self) -> Option<CrossProviderCandidate> {
        let (provider, model) =
            match resolve_purpose_model_db(&self.db, "loop_fallback_default").await {
                PurposeResolution::Resolved {
                    provider, model, ..
                } => (provider, model),
                _ => return None,
            };
        if SENTINELS.contains(&provider.as_str()) || SENTINELS.contains(&model.as_str()) {
            return None;
        }
        if provider.trim().is_empty() || model.trim().is_empty() {
            return None;
        }
        Some(CrossProviderCandidate { provider, model })
    }
}

#[async_trait]
impl EscalationPort for PgEscalationPort {
    /// Risolve gli input dell'escalation per il turno corrente. SOLA LETTURA.
    /// FAIL-OPEN: ogni sotto-lettura degrada a vuoto, mai un `PortError`.
    async fn escalation_inputs(
        &self,
        _intent: Option<&str>,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Result<EscalationInputs, PortError> {
        // Tier 2 (cross-provider): sempre risolto, indipendente dalla coppia
        // corrente; e' il modulo puro a decidere se usarlo.
        let cross_provider = self.cross_provider().await;

        // Tier 1 (intra-provider): solo se provider+model valorizzati.
        let (chain, provider_in_cooldown) = match (provider, model) {
            (Some(p), Some(m)) if !p.trim().is_empty() && !m.trim().is_empty() => {
                let in_cooldown = is_provider_in_cooldown(p);
                // Filtro PR-J1: se il provider corrente NON e' disponibile runtime
                // (nessuna API key), azzeriamo la catena intra-provider cosi' la
                // selezione salta il Tier 1 e usa il cross-provider (parita' con la
                // guardia `_providers._providers.get(provider)` del Python).
                let available = self.provider_available(p).await;
                let chain = if available {
                    self.chain_for(p, m).await
                } else {
                    tracing::info!(
                        provider = %p,
                        "escalation_port: provider corrente non disponibile runtime, \
                         Tier 1 saltato (catena azzerata), si usera' il cross-provider"
                    );
                    Vec::new()
                };
                (chain, in_cooldown)
            }
            _ => (Vec::new(), false),
        };

        Ok(EscalationInputs {
            chain,
            provider_in_cooldown,
            cross_provider,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Schema minimale per i test: la tabella della catena (mig 0128) + `settings`
    /// (per la disponibilita' provider) + `nexus_purpose_model` VUOTA (cosi'
    /// `resolve_purpose_model_db` ritorna `NotFound` -> `cross_provider = None`:
    /// isoliamo il Tier 1 senza dipendere da catalog/routing).
    async fn create_schema(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE nexus_model_escalation_chain ( \
                 provider TEXT NOT NULL, \
                 base_model TEXT NOT NULL, \
                 escalation_position INT NOT NULL, \
                 escalation_model TEXT NOT NULL, \
                 capability_tier TEXT NOT NULL DEFAULT 'medium', \
                 is_active BOOLEAN NOT NULL DEFAULT TRUE, \
                 PRIMARY KEY (provider, base_model, escalation_position) \
             )",
        )
        .execute(pool)
        .await
        .expect("create nexus_model_escalation_chain");
        sqlx::query(
            "CREATE TABLE settings ( \
                 key TEXT PRIMARY KEY, \
                 value TEXT NOT NULL, \
                 category TEXT \
             )",
        )
        .execute(pool)
        .await
        .expect("create settings");
        sqlx::query(
            "CREATE TABLE nexus_purpose_model ( \
                 purpose TEXT PRIMARY KEY, \
                 tier TEXT, \
                 required_capability TEXT, \
                 requires_tool_use BOOLEAN NOT NULL DEFAULT false \
             )",
        )
        .execute(pool)
        .await
        .expect("create nexus_purpose_model");
    }

    /// Marca un provider come disponibile inserendone la API key in `settings`.
    async fn set_api_key(pool: &PgPool, provider: &str, value: &str) {
        sqlx::query(
            "INSERT INTO settings (key, value, category) VALUES ($1, $2, 'providers')",
        )
        .bind(format!("{provider}_api_key"))
        .bind(value)
        .execute(pool)
        .await
        .expect("insert api key");
    }

    async fn seed_chain(pool: &PgPool, rows: &[(&str, &str, i32, &str, bool)]) {
        for (provider, base, pos, model, active) in rows {
            sqlx::query(
                "INSERT INTO nexus_model_escalation_chain \
                 (provider, base_model, escalation_position, escalation_model, capability_tier, is_active) \
                 VALUES ($1, $2, $3, $4, 'medium', $5)",
            )
            .bind(provider)
            .bind(base)
            .bind(pos)
            .bind(model)
            .bind(active)
            .execute(pool)
            .await
            .expect("insert chain row");
        }
    }

    /// La catena rispetta `is_active=TRUE` e l'ordine `escalation_position` ASC.
    #[sqlx::test]
    async fn chain_filtra_attive_e_ordina_per_posizione(pool: PgPool) {
        create_schema(&pool).await;
        set_api_key(&pool, "anthropic", "sk-live").await;
        // Inserite fuori ordine + una posizione disattivata (NON deve comparire).
        seed_chain(
            &pool,
            &[
                ("anthropic", "claude-haiku-4-5", 2, "claude-opus-4-6", true),
                ("anthropic", "claude-haiku-4-5", 1, "claude-sonnet-4-6", true),
                ("anthropic", "claude-haiku-4-5", 3, "modello-disattivato", false),
            ],
        )
        .await;
        let port = PgEscalationPort::new(pool.clone());
        let inputs = port
            .escalation_inputs(None, Some("anthropic"), Some("claude-haiku-4-5"))
            .await
            .expect("fail-open: mai PortError");
        let models: Vec<&str> = inputs
            .chain
            .iter()
            .map(|c| c.escalation_model.as_str())
            .collect();
        assert_eq!(
            models,
            vec!["claude-sonnet-4-6", "claude-opus-4-6"],
            "ordine per posizione ASC, solo is_active=TRUE"
        );
        // nexus_purpose_model vuota -> cross_provider None.
        assert!(inputs.cross_provider.is_none());
    }

    /// Provider corrente NON disponibile (nessuna API key) -> catena Tier 1
    /// AZZERATA (filtro PR-J1), anche se la tabella avrebbe righe.
    #[sqlx::test]
    async fn provider_non_registrato_azzera_la_catena(pool: PgPool) {
        create_schema(&pool).await;
        // NESSUNA api key per 'anthropic' -> provider non disponibile.
        seed_chain(
            &pool,
            &[("anthropic", "claude-haiku-4-5", 1, "claude-sonnet-4-6", true)],
        )
        .await;
        let port = PgEscalationPort::new(pool.clone());
        let inputs = port
            .escalation_inputs(None, Some("anthropic"), Some("claude-haiku-4-5"))
            .await
            .expect("fail-open");
        assert!(
            inputs.chain.is_empty(),
            "provider non registrato -> Tier 1 saltato (catena vuota)"
        );
    }

    /// API key presente ma VUOTA -> provider non disponibile (catena azzerata).
    #[sqlx::test]
    async fn provider_con_api_key_vuota_azzera_la_catena(pool: PgPool) {
        create_schema(&pool).await;
        set_api_key(&pool, "anthropic", "   ").await;
        seed_chain(
            &pool,
            &[("anthropic", "claude-haiku-4-5", 1, "claude-sonnet-4-6", true)],
        )
        .await;
        let port = PgEscalationPort::new(pool.clone());
        let inputs = port
            .escalation_inputs(None, Some("anthropic"), Some("claude-haiku-4-5"))
            .await
            .expect("fail-open");
        assert!(inputs.chain.is_empty(), "api key vuota -> provider non disponibile");
    }

    /// Provider/model assenti -> Tier 1 saltato (catena vuota), nessun cooldown.
    #[sqlx::test]
    async fn coppia_assente_catena_vuota(pool: PgPool) {
        create_schema(&pool).await;
        let port = PgEscalationPort::new(pool.clone());
        let inputs = port.escalation_inputs(None, None, None).await.expect("fail-open");
        assert!(inputs.chain.is_empty());
        assert!(!inputs.provider_in_cooldown);
        assert!(inputs.cross_provider.is_none());
    }

    /// Catena assente per la coppia corrente -> vuota (non un errore).
    #[sqlx::test]
    async fn coppia_senza_catena_ritorna_vuoto(pool: PgPool) {
        create_schema(&pool).await;
        set_api_key(&pool, "openai", "sk-live").await;
        let port = PgEscalationPort::new(pool.clone());
        let inputs = port
            .escalation_inputs(None, Some("openai"), Some("gpt-4o-mini"))
            .await
            .expect("fail-open");
        assert!(inputs.chain.is_empty());
    }
}
