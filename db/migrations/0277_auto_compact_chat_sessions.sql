-- Mig 0277 — Auto-compact automatico delle sessioni chat a soglia.
--
-- Problema (regola H, causa radice): il compact della chat era SOLO manuale
-- (pulsante "Compatta chat" -> POST .../compact). In una conversazione lunga il
-- contesto cresceva illimitato (osservato 460-518% del context window) fino a
-- saturare i modelli piccoli, rallentare il run e impantanarlo. Il fix non e'
-- alzare i limiti o troncare a mano, ma compattare automaticamente la sessione
-- quando il rapporto token/context_window supera una soglia configurabile,
-- riusando la stessa logica del compact manuale (compact_session_core).
--
-- Tutti i parametri sono DB-driven (regola G): nessun valore hardcoded nella
-- logica. I default safe nel codice (cache 60s) coincidono con i valori qui.
-- Idempotente (ON CONFLICT DO NOTHING).
--
-- Note:
--   * agent.context.auto_compact_enabled e' il flag master (default true): puo'
--     disattivare l'auto-compact senza redeploy.
--   * agent.context.auto_compact_ratio e' la soglia ratio = session_tokens /
--     context_window oltre la quale (>=) la sessione viene compattata PRIMA del
--     turno agente. Default 0.80, range valido [0.5, 0.95] (il codice clampa).

INSERT INTO settings (key, value, category, description, updated_at)
VALUES
    (
        'agent.context.auto_compact_enabled',
        'true',
        'agent',
        'Flag master auto-compact. Se true (default), prima di ogni nuovo turno agente il sistema valuta il rapporto token sessione / context window del modello attivo e, se >= agent.context.auto_compact_ratio, compatta automaticamente la sessione (stesso meccanismo del pulsante "Compatta chat"). Se false, il compact resta solo manuale.',
        NOW()
    ),
    (
        'agent.context.auto_compact_ratio',
        '0.80',
        'agent',
        'Soglia ratio = session_tokens / context_window oltre la quale (>=) scatta l''auto-compact prima del turno agente. Default 0.80. Range valido [0.5, 0.95]: il codice clampa i valori fuori range. Token sessione = somma dei total/prompt tokens dei messaggi non soft-deleted (deleted_at IS NULL), con stima a ~4 char/token quando i token non sono persistiti. context_window dal catalog ai_price_catalog del modello risolto per il turno.',
        NOW()
    )
ON CONFLICT (key) DO NOTHING;
