-- 0452_shadow_telemetry.sql
-- FASE 3 del porting dell'orchestrazione agentica da Python/LangGraph a Rust:
-- INFRASTRUTTURA dei nodi I/O + scaffold della modalita' SHADOW. Rischio di
-- regressione NULLO: la tabella serve SOLO alla telemetria del confronto
-- primario<->shadow, che e' opt-in e read-only; in default globale (motore
-- 'python', mig 0451) non viene mai scritta.
--
-- Regola H (fix definitivo, non toppa): lo schema della telemetria shadow nasce
-- da una migrazione VERSIONATA, NON da un `CREATE TABLE IF NOT EXISTS` eseguito
-- a runtime dal codice. Lo schema vive qui, sotto controllo di versione; il
-- punto unico di scrittura e' crates/nexus-agent-graph/src/shadow/mod.rs
-- (persist_node_diff).
--
-- Regola G: nessun nome modello / provider qui dentro. La selezione del motore
-- (python/rust/shadow) resta su nexus_orchestrator_engine (mig 0451).
--
-- Idempotente: CREATE TABLE IF NOT EXISTS + CREATE INDEX IF NOT EXISTS.

-- ---------------------------------------------------------------------------
-- Telemetria del confronto per-nodo fra il run PRIMARIO e il run SHADOW.
--
-- Ogni riga e' il diff dell'output (StateDelta serializzato) di UN nodo:
-- `primary_output`/`shadow_output` sono i due JSON confrontati, `divergent_keys`
-- l'elenco delle chiavi top-level che divergono (calcolato da compute_diff in
-- Rust, output deterministico/ordinato). Read-only rispetto al run: questa
-- tabella e' SOLA osservabilita', non influenza il run primario verso l'utente.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS nexus_shadow_telemetry (
    id             UUID PRIMARY KEY,
    run_id         UUID NOT NULL,
    node_name      TEXT NOT NULL,
    primary_output JSONB NOT NULL,
    shadow_output  JSONB NOT NULL,
    divergent_keys TEXT[] NOT NULL DEFAULT '{}',
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Analisi della telemetria per run (tutti i nodi di uno stesso run shadow).
CREATE INDEX IF NOT EXISTS idx_nexus_shadow_telemetry_run
    ON nexus_shadow_telemetry (run_id, created_at);

-- Filtro rapido sui soli nodi che hanno divergenze (parita' da investigare):
-- indice parziale sull'array non vuoto.
CREATE INDEX IF NOT EXISTS idx_nexus_shadow_telemetry_divergent
    ON nexus_shadow_telemetry (node_name)
    WHERE cardinality(divergent_keys) > 0;

COMMENT ON TABLE nexus_shadow_telemetry IS
    'Telemetria opt-in/read-only del confronto per-nodo primario<->shadow del porting LangGraph->Rust (Fase 3). Punto unico di scrittura: crates/nexus-agent-graph/src/shadow/mod.rs::persist_node_diff. Non influenza il run primario.';
