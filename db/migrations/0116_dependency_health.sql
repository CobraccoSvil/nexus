-- Migrazione 0116: storico health check dipendenze infrastrutturali
--
-- Tabella per tracciare lo stato di Qdrant (vector DB) e dell'embedder
-- (servizio embedding nel brain Python). Segue lo schema di
-- nexus_provider_health_history (mig 0097), con colonne analoghe.
--
-- Motivazione: la quality scan si bloccava indefinitamente quando Qdrant
-- raggiungeva il limite di file descriptor ("too many open files"). Senza
-- monitoraggio proattivo, l'utente vedeva solo "Errore durante la scansione"
-- dopo 4 minuti di polling inutile. Con questa tabella il watchdog persiste
-- lo stato delle dipendenze e i task background consultano lo stato in-memory
-- per decidere se avviare la fase vettoriale.

CREATE TABLE IF NOT EXISTS nexus_dependency_health (
    id BIGSERIAL PRIMARY KEY,
    dependency TEXT NOT NULL,
    healthy BOOLEAN NOT NULL,
    latency_ms INT,
    error_kind TEXT,
    error_message TEXT,
    checked_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_dep_health_dep_checked
    ON nexus_dependency_health (dependency, checked_at DESC);

-- Pulizia automatica: mantieni solo le ultime 24h di storico per non
-- far crescere la tabella indefinitamente (1440 righe/giorno per dipendenza
-- con intervallo 60s, ~3000 righe totali).
-- La pulizia avviene nel watchdog Rust, non con un cron SQL.
