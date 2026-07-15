//! Helper di TEST condivisi del crate `mcp-core`.
//!
//! Punto unico (regola L) dello schema di test della tabella `ai_price_catalog`.
//! Prima la definizione era duplicata tra il modulo `#[cfg(test)]` di
//! `orchestrator::model_selection` e quello di
//! `agent_graph_adapter::model_upscale_port`: due `CREATE TABLE` indipendenti che
//! dovevano restare identici a mano. La duplicazione ha gia' causato una
//! regressione (mig 0478: aggiunte le colonne media `supports_image_gen`,
//! `supports_audio_in`, `supports_audio_out`, `supports_video_gen` che il punto
//! unico `select_models_tierchain` ha iniziato a filtrare; solo una delle due
//! copie venne aggiornata, l'altra rimase obsoleta e i suoi `#[sqlx::test]`
//! fallivano con "column does not exist" -> fail-open `Ok(None)` -> panico).
//!
//! Con un solo helper, una nuova colonna del catalog si aggiunge QUI una volta
//! sola e ogni `#[sqlx::test]` del crate resta automaticamente allineato.

use sqlx::PgPool;

/// Crea la tabella `ai_price_catalog` con lo schema canonico usato dai
/// `#[sqlx::test]` del crate.
///
/// L'insieme delle colonne deve restare allineato a quelle lette dal punto unico
/// `crate::orchestrator::select_models_tierchain` (in particolare i media kind
/// della mig 0478, sempre referenziati nella WHERE per i purpose testuali). Una
/// colonna nuova nel catalog va aggiunta qui e basta: i call site delegano.
pub(crate) async fn create_ai_price_catalog_table(pool: &PgPool) {
    sqlx::query(
        "CREATE TABLE ai_price_catalog ( \
             provider TEXT NOT NULL, \
             model TEXT NOT NULL, \
             is_enabled BOOLEAN NOT NULL DEFAULT true, \
             supports_tool_use BOOLEAN NOT NULL DEFAULT true, \
             supports_vision BOOLEAN NOT NULL DEFAULT false, \
             supports_image_gen BOOLEAN NOT NULL DEFAULT false, \
             supports_audio_in BOOLEAN NOT NULL DEFAULT false, \
             supports_audio_out BOOLEAN NOT NULL DEFAULT false, \
             supports_video_gen BOOLEAN NOT NULL DEFAULT false, \
             agentic_thinking_policy TEXT NOT NULL DEFAULT 'none', \
             uses_thinking_mode BOOLEAN NOT NULL DEFAULT false, \
             performance_tier TEXT NOT NULL DEFAULT 'medium', \
             capabilities JSONB NOT NULL DEFAULT '[]', \
             context_window INTEGER NOT NULL DEFAULT 8192, \
             input_cost_per_million_tokens DOUBLE PRECISION NOT NULL DEFAULT 0, \
             output_cost_per_million_tokens DOUBLE PRECISION NOT NULL DEFAULT 0, \
             is_featured BOOLEAN NOT NULL DEFAULT false, \
             speed_tier TEXT NOT NULL DEFAULT 'medium', \
             consecutive_failures INT NOT NULL DEFAULT 0, \
             consecutive_tool_failures INT NOT NULL DEFAULT 0, \
             auto_disabled_reason TEXT, \
             qualification_state TEXT NOT NULL DEFAULT 'unqualified', \
             qualified_capabilities JSONB NOT NULL DEFAULT '[]', \
             qualification_expires_at TIMESTAMPTZ, \
             pricing_state TEXT NOT NULL DEFAULT 'priced' \
         )",
    )
    .execute(pool)
    .await
    .expect("create ai_price_catalog");
}
