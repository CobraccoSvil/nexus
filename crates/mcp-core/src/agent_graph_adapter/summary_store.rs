//! Adapter del trait [`nexus_agent_graph::runtime::ports::SummaryStore`].
//!
//! IMPLEMENTA `SummaryStore::summarize` (intervento 3, rolling-summary)
//! chiamando il gateway LLM col MODELLO ECONOMICO per RIASSUMERE il prefisso
//! della history conversazionale gia' serializzato dal punto unico PURO
//! ([`nexus_agent_graph::decisions::context_reduction::serialize_prefix_for_summary`]).
//! La logica di DECISIONE (cutoff / serializzazione / applicazione) resta PURA
//! fuori da qui (regola L): questo adapter isola SOLO l'I/O della chiamata LLM.
//!
//! MODELLO ECONOMICO (regola G — unica fonte DB): risolto dal setting dedicato
//! `agent.context.rolling_summary_model` nel formato `provider/model`, letto col
//! PUNTO UNICO [`nexus_auth::get_setting`]. Niente nome modello hardcoded, niente
//! env var, niente fallback. Se il setting manca o e' malformato la chiamata
//! fallisce (`PortError`) e il nodo executor degrada (history invariata).
//! SCELTA del setting dedicato vs purpose `conversation_summary`: la chiave
//! `agent.context.rolling_summary_model` e' la fonte UNICA e dedicata a QUESTA
//! funzione (mig settings rolling), gia' popolata; usarla evita di mescolare due
//! fonti di verita' (regola G/L) e di ereditare il modello di un altro purpose.
//!
//! GATE Real/Replay (PUNTO UNICO del gate shadow, regola L; uniforme con
//! [`super::context_offload::RagContextOffloadAdapter`]): in [`ExecMode::Replay`]
//! (run shadow read-only) e' un NO-OP che ritorna `PortError` -> il nodo NON
//! riassume (degrado a history invariata), cosi' lo shadow non diverge dal replay
//! ne' spende una chiamata LLM. In [`ExecMode::Real`] riassume davvero.
//! BEST-EFFORT con DEGRADO A HISTORY INVARIATA: su guasto (modello non risolto,
//! porta gateway assente, HTTP error, timeout) ritorna `PortError` e il nodo
//! prosegue (compress/token_brake fanno comunque il loro lavoro). Il guasto del
//! summarizer NON deve MAI rompere il run.
//!
//! Regola F: niente prompt/response in chiaro nei log (solo lunghezze).

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{ExecMode, PortError, SummaryStore};

use crate::nexus_gateway::{GwMessage, GwMetadata, GwRequest, NexusGatewayClient};

/// Setting (regola G) col modello economico del rolling-summary nel formato
/// `provider/model` (es. `google/gemini-2.5-flash-lite`). Unica fonte DB.
const ROLLING_MODEL_SETTING: &str = "agent.context.rolling_summary_model";

/// Prompt di sistema del summarizer. Override opzionale via template DB
/// `system.rolling_summary` (pattern di `next_actions_deriver`): se presente e
/// non vuoto sostituisce questo testo.
const SUMMARY_SYSTEM_PROMPT: &str = "Riassumi in modo conciso la seguente \
conversazione preservando decisioni, fatti e stato; ometti dettagli ridondanti.";

/// Timeout della chiamata al summarizer (secondi). Best-effort: oltre questa
/// soglia il nodo degrada a history invariata.
const SUMMARY_TIMEOUT_SECS: u64 = 30;

/// Cap dei token in output del riassunto (difensivo: un riassunto conciso non
/// deve esplodere; oltre questo cap il guadagno di contesto svanirebbe).
const SUMMARY_MAX_TOKENS: u32 = 1024;

/// Adapter [`SummaryStore`] -> gateway LLM col modello economico.
pub struct PgSummaryStore {
    /// Pool Postgres: risolve il modello economico (`agent.context.rolling_summary_model`,
    /// regola G), il prompt di sistema (template opzionale) e la porta del gateway.
    db: PgPool,
}

impl PgSummaryStore {
    /// Costruisce l'adapter sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// Risolve `(provider, model)` dal setting `agent.context.rolling_summary_model`
    /// (formato `provider/model`). `None` se il setting manca o e' malformato (il
    /// chiamante degrada). Regola G: unica fonte DB, nessun fallback hardcoded.
    async fn resolve_model(&self) -> Option<(String, String)> {
        let raw: Option<String> = nexus_auth::get_setting(&self.db, ROLLING_MODEL_SETTING).await;
        let raw = raw?;
        let (provider, model) = raw.split_once('/')?;
        let provider = provider.trim();
        let model = model.trim();
        if provider.is_empty() || model.is_empty() {
            return None;
        }
        Some((provider.to_string(), model.to_string()))
    }

    /// Prompt di sistema: template DB `system.rolling_summary` se presente e non
    /// vuoto, altrimenti il default [`SUMMARY_SYSTEM_PROMPT`] (graceful degradation,
    /// stesso pattern di `next_actions_deriver::build_extractor_prompt`).
    async fn system_prompt(&self) -> String {
        let tpl = sqlx::query_scalar::<_, String>(
            "SELECT content FROM nexus_prompt_templates \
             WHERE key = 'system.rolling_summary' AND is_active = TRUE LIMIT 1",
        )
        .fetch_optional(&self.db)
        .await
        .ok()
        .flatten();
        match tpl.filter(|t| !t.trim().is_empty()) {
            Some(t) => t,
            None => SUMMARY_SYSTEM_PROMPT.to_string(),
        }
    }

    /// Costruisce il client gateway dalla porta nel DB (regola G: niente env/porta
    /// hardcoded). `None` se la lettura della porta fallisce. Riusa il token di
    /// servizio condiviso (stesso pattern di `next_actions_deriver`).
    async fn gateway_client(&self) -> Option<NexusGatewayClient> {
        let port = nexus_auth::resolve_port(&self.db, "nexus_gateway_port").await;
        if port == 0 {
            return None;
        }
        let url = format!("http://127.0.0.1:{port}");
        let token = std::env::var("NEXUS_GATEWAY_SERVICE_TOKEN")
            .unwrap_or_else(|_| "dev-internal-token".to_string());
        Some(NexusGatewayClient::new(url, token))
    }
}

#[async_trait]
impl SummaryStore for PgSummaryStore {
    /// Riassume `text` (prefisso serializzato dal punto unico puro) col modello
    /// economico risolto dal DB (regola G). GATE Real: in [`ExecMode::Replay`] e'
    /// un no-op che ritorna `PortError` (il run shadow non riassume). Su qualunque
    /// guasto (anche in Real) ritorna `PortError` (il nodo degrada a history
    /// invariata). Best-effort.
    async fn summarize(&self, text: String, mode: ExecMode) -> Result<String, PortError> {
        if mode != ExecMode::Real {
            // Run shadow read-only: niente chiamata LLM (gate shadow). Il nodo
            // degrada = salta il summary, non diverge dal replay.
            return Err(PortError::Llm(
                "rolling_summary: no-op in Replay (run shadow read-only)".to_string(),
            ));
        }
        if text.trim().is_empty() {
            return Err(PortError::Llm(
                "rolling_summary: prefisso vuoto, niente da riassumere".to_string(),
            ));
        }

        // 1. Modello economico (regola G, unica fonte DB). Manca -> degrada.
        let Some((provider, model)) = self.resolve_model().await else {
            tracing::warn!(
                setting = ROLLING_MODEL_SETTING,
                "rolling_summary: modello non risolto dal DB, degrado a history invariata"
            );
            return Err(PortError::Llm(
                "rolling_summary: modello non risolto (agent.context.rolling_summary_model)"
                    .to_string(),
            ));
        };

        // 2. Client gateway dalla porta nel DB (regola G). Manca -> degrada.
        let Some(gw) = self.gateway_client().await else {
            return Err(PortError::Llm(
                "rolling_summary: porta gateway non risolta".to_string(),
            ));
        };

        // 3. Prompt di sistema (template DB o default) + il prefisso da riassumere.
        let system = self.system_prompt().await;
        let req = GwRequest {
            model,
            messages: vec![
                GwMessage {
                    role: "system".to_string(),
                    content: serde_json::Value::String(system),
                    ..Default::default()
                },
                GwMessage {
                    role: "user".to_string(),
                    content: serde_json::Value::String(text),
                    ..Default::default()
                },
            ],
            max_tokens: Some(SUMMARY_MAX_TOKENS),
            temperature: Some(0.0),
            // Pin esplicito: provider+modello gia' decisi dal setting (regola G),
            // niente secondo routing nel gateway.
            pin_provider: Some(provider),
            metadata: GwMetadata {
                tenant_id: "internal".to_string(),
                user_id: "system".to_string(),
                request_id: Uuid::new_v4().to_string(),
                sensitivity_tier: 0,
                feature: "rolling_summary".to_string(),
            },
            ..Default::default()
        };

        // 4. Chiamata best-effort con timeout: ogni esito non-Ok -> PortError.
        let resp = match tokio::time::timeout(
            std::time::Duration::from_secs(SUMMARY_TIMEOUT_SECS),
            gw.complete(req),
        )
        .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "rolling_summary: chiamata gateway fallita");
                return Err(PortError::Llm(format!("rolling_summary: {e}")));
            }
            Err(_) => {
                tracing::warn!("rolling_summary: chiamata gateway in timeout");
                return Err(PortError::Llm("rolling_summary: timeout".to_string()));
            }
        };

        let summary = resp.content.trim();
        if summary.is_empty() {
            return Err(PortError::Llm(
                "rolling_summary: risposta vuota dal modello".to_string(),
            ));
        }
        tracing::info!(
            in_chars = resp.content.len(),
            "rolling_summary: prefisso riassunto dal modello economico"
        );
        Ok(summary.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In `Replay` la summarize e' un no-op che ritorna `PortError` (gate shadow):
    /// nessuna chiamata LLM, nessun accesso al gateway. Il nodo degrada a history
    /// invariata. Il gate scatta prima di toccare il DB.
    #[sqlx::test]
    async fn replay_e_un_noop_che_ritorna_porterror(pool: PgPool) {
        let store = PgSummaryStore::new(pool.clone());
        let res = store
            .summarize(
                "[human]: ciao\n[assistant]: salve".to_string(),
                ExecMode::Replay,
            )
            .await;
        assert!(
            res.is_err(),
            "in Replay la summarize deve fallire (no-op), il nodo degrada"
        );
    }

    /// Prefisso vuoto in `Real` -> `PortError` (niente da riassumere). Non tocca il
    /// gateway perche' il controllo precede la risoluzione del modello.
    #[sqlx::test]
    async fn prefisso_vuoto_in_real_ritorna_porterror(pool: PgPool) {
        let store = PgSummaryStore::new(pool.clone());
        let res = store.summarize("   ".to_string(), ExecMode::Real).await;
        assert!(res.is_err(), "prefisso vuoto -> PortError");
    }
}
