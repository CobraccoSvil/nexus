-- Migrazione 0097: storico health check provider LLM.
--
-- Aggiunge la tabella `nexus_provider_health_history` popolata dal worker
-- `provider_health_probe` (vedi `crates/mcp-core/src/provider_health_probe.rs`).
--
-- Il worker ogni 5 minuti chiama ogni provider con un prompt minimale
-- ("hi", ~1 token output, costo trascurabile ~$0.001/giorno totali). Il
-- risultato viene scritto qui per:
--   1. Aggiornare il LED nello statusbar PRIMA che l'utente faccia il primo
--      click (oggi il LED diventa giallo solo dopo un errore reale).
--   2. Fornire dati al dashboard admin per uno storico latency/errori.
--
-- Lo schema e' append-only: nessuna UPDATE, ogni check e' una riga nuova.
-- Cleanup periodico oltre 30 giorni va fatto da un worker dedicato (out of scope).

CREATE TABLE IF NOT EXISTS nexus_provider_health_history (
    id BIGSERIAL PRIMARY KEY,
    provider TEXT NOT NULL,
    healthy BOOLEAN NOT NULL,
    latency_ms INT,
    --Categoria errore quando healthy=FALSE: "quota_exceeded",
    --"credit_balance_too_low", "billing_required", "rate_limit",
    --"timeout", "auth_error", "connection_error", "unknown".
    error_kind TEXT,
    --Messaggio d'errore originale (troncato a 500 char).
    error_message TEXT,
    checked_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indice per query "ultimo check per provider" (lookup O(log N)).
CREATE INDEX IF NOT EXISTS idx_provider_health_history_provider_checked
    ON nexus_provider_health_history (provider, checked_at DESC);

-- Indice per query "tutti gli errori di un dato kind nelle ultime 24h"
-- (utile per dashboard alert).
CREATE INDEX IF NOT EXISTS idx_provider_health_history_error_kind
    ON nexus_provider_health_history (error_kind, checked_at DESC)
    WHERE error_kind IS NOT NULL;

COMMENT ON TABLE nexus_provider_health_history IS
'Storico health check provider LLM. Popolato dal worker provider_health_probe ogni 5 minuti. Append-only.';
