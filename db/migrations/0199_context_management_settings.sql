-- Mig 0199 — Context size management (FIX A-D ADR 0014).
--
-- 4 fix strutturali contro l'esplosione del context in executor_node:
--   A) Compressione anticipata escalante da iter 5.
--   B) Deduplicazione tool_result identici per signature (tool_name+args).
--   C) Drop body base64 da tool_result vecchi non citati.
--   D) Predictive cap pre-tool: blocca invocazioni che porterebbero
--      il context oltre una soglia configurabile del context_window del modello.
--
-- Tutti i parametri sono configurabili da DB: nessun fallback hardcoded
-- nel codice tranne i defaults safe usati se il DB e' down (cache 60s).
-- Idempotente: ON CONFLICT (key) DO NOTHING.

INSERT INTO settings (key, value, category, description, updated_at)
VALUES
    (
        'agent.context.compress_start_iter',
        '5',
        'agent',
        'Iterazione di executor a partire dalla quale attivare la compressione escalante dei tool_result. Prima viene applicata solo la dedup. Default 5 (FIX A).',
        NOW()
    ),
    (
        'agent.context.compress_phase_boundaries',
        '5,10,20,50',
        'agent',
        'CSV crescente dei boundary di fase per compressione escalante. iter < primo = no compressione. Tra phase[i] e phase[i+1] = applica keep_recent[i] e max_chars[i]. Le tre liste boundaries/keep_recent/max_chars devono avere stessa lunghezza.',
        NOW()
    ),
    (
        'agent.context.compress_phase_keep_recent',
        '8,5,3,2',
        'agent',
        'CSV keep_recent per ogni fase di compressione (allineato a compress_phase_boundaries).',
        NOW()
    ),
    (
        'agent.context.compress_phase_max_chars',
        '2000,1000,500,150',
        'agent',
        'CSV max_content_chars per ogni fase di compressione (allineato a compress_phase_boundaries).',
        NOW()
    ),
    (
        'agent.context.dedup_tool_results_enabled',
        'true',
        'agent',
        'Se true (default) ogni iter executor applica _dedup_tool_results_history: tool_result vecchi con stessa signature (sha256(tool_name+args_json)) vengono sostituiti con placeholder, tenendo solo l ultimo. FIX B.',
        NOW()
    ),
    (
        'agent.context.drop_unused_base64_age',
        '3',
        'agent',
        'Soglia (n messaggi successivi) entro la quale verificare se un blob base64 di un tool_result vecchio viene citato testualmente. Se non viene citato, il body base64 viene sostituito con un placeholder. FIX C.',
        NOW()
    ),
    (
        'agent.context.predictive_cap_ratio',
        '0.5',
        'agent',
        'Soglia (0.3-0.9) sul context_window del modello: se context_attuale + stima_tool_result supera ratio*context_window, la chiamata al tool viene intercettata e sostituita da tool_result sintetico di errore. FIX D.',
        NOW()
    )
ON CONFLICT (key) DO NOTHING;
