-- Mig 0280 — Freno TOKEN-based intra-turno (estensione FIX A-D ADR 0014).
--
-- I FIX A-D (mig 0199) frenano il context su base ITERAZIONE e toccano SOLO
-- i blocchi tool_result. In turni agentici con molte iterazioni (loop su tool)
-- i messaggi ASSISTANT lunghi (ragionamenti del modello) non venivano compressi
-- e nessun freno guardava il context_window reale del modello: il context
-- poteva superare 5x il window (osservato: 756K token su window 128K).
--
-- Questo freno aggiunge una barriera TOKEN-based in executor_node, applicata
-- subito prima della costruzione dei messaggi Anthropic:
--   - stima i token (~4 char/token via _estimate_context_chars);
--   - se >= max_context_ratio * context_window (da ai_price_catalog) applica
--     una compressione AGGRESSIVA che tronca TUTTI i messaggi vecchi (inclusi
--     gli assistant), preservando la richiesta originale e i riassunti;
--   - cap di sicurezza hard se ancora sopra il window.
--
-- Tutti i parametri sono DB-driven (cache 60s, regola G). I default safe nel
-- codice valgono solo se il DB e' down. Idempotente: ON CONFLICT (key) DO NOTHING.

INSERT INTO settings (key, value, category, description, updated_at)
VALUES
    (
        'agent.context.max_context_ratio',
        '0.70',
        'agent',
        'Soglia (0.4-0.9) sul context_window del modello attivo: se la stima token del contesto in executor supera ratio*context_window, scatta la compressione aggressiva TOKEN-based che tronca anche i messaggi assistant lunghi. Default 0.70.',
        NOW()
    ),
    (
        'agent.context.aggressive_keep_recent',
        '3',
        'agent',
        'Numero di messaggi piu recenti mantenuti integri dalla compressione aggressiva TOKEN-based. La richiesta originale (primo messaggio umano) e i riassunti rolling sono sempre preservati. Default 3.',
        NOW()
    ),
    (
        'agent.context.aggressive_max_chars',
        '200',
        'agent',
        'max_content_chars per la compressione aggressiva TOKEN-based: i messaggi vecchi (inclusi assistant) vengono troncati a questa lunghezza con marker [...troncato per limite contesto...]. Default 200.',
        NOW()
    )
ON CONFLICT (key) DO NOTHING;
