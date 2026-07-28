-- 0647_final_gate_endpoint_probes.sql
-- Riporta la configurazione delle prove HTTP del final_gate, ora che il criterio
-- e' davvero cablato nel motore nativo.
--
-- Storia delle due chiavi: introdotte dalla 0455 per il criterio `http` del
-- final_gate (letto allora da brain/agents/final_gate.py::_resolve_endpoint_check),
-- cancellate dalla 0463 con la rimozione del brain Python, perche' nel grafo Rust
-- l'endpoint "e' risolto per-progetto a monte" — solo che quella risoluzione non
-- e' mai stata scritta: `FinalGateConfig.endpoint_criterion` restava al `Default`
-- (`None`) con un TODO, e con `None` il criterio non si aggiunge. Conseguenza
-- misurata il 28/07/2026 (progetto gestione-spese): il final_gate ha dichiarato
-- "superato" piu' volte un'applicazione in cui `GET /api/expenses` rispondeva 200
-- e `POST /api/expenses` rispondeva 500. Il gate verificava che il codice
-- COMPILASSE e nulla di cio' che l'applicazione FA.
--
-- La 0645 aveva gia' fissato il principio per questa stessa famiglia: "le settings
-- seguono il codice", e tornano con la migrazione che le cabla. Questa e' quella
-- migrazione.
--
-- Consumatori (regola G, nessun fallback nascosto: il `Default` Rust vale solo a
-- chiave assente ed e' identico ai valori qui sotto):
--   agent.final_gate.endpoint_check_enabled   -> mcp-core::native_engine::load_final_gate_config
--   agent.final_gate.endpoint_timeout_seconds -> idem, e da li' il timeout di OGNI
--                                                criterio http, configurato o dichiarato
--
-- Le prove non sono piu' solo quelle CONFIGURATE in `run_configurations`
-- (role='endpoint' + http_spec, colonna della 0455, che resta e ora viene
-- finalmente letta): l'agente DICHIARA gli endpoint creati in
-- `task_complete.endpoints` (ADR 0034) e il gate li chiama tutti, metodi di
-- scrittura compresi. Una configurazione manuale che nessuno compila equivale a
-- nessuna verifica: il progetto dell'incidente non l'aveva.
--
-- Idempotente: INSERT ... ON CONFLICT DO NOTHING.

INSERT INTO settings (key, value, category, description) VALUES
(
    'agent.final_gate.endpoint_check_enabled', 'true', 'agent',
    'Se true, il final_gate esegue le prove HTTP funzionali prima di chiudere: chiama gli endpoint dichiarati dall''agente in task_complete.endpoints e quelli configurati nel progetto (run_configurations role=endpoint con http_spec). Un endpoint che non risponde come atteso FA FALLIRE il gate. Il verdetto nasce dallo status HTTP (regola M), mai dal corpo della risposta. OFF = comportamento storico: nessuna verifica funzionale.'
),
(
    'agent.final_gate.endpoint_timeout_seconds', '15', 'agent',
    'Timeout (secondi) di UNA chiamata HTTP del final_gate, sia per gli endpoint dichiarati dall''agente sia per quelli configurati nel progetto.'
)
ON CONFLICT (key) DO NOTHING;
