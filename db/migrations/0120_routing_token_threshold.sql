-- 0120_routing_token_threshold.sql
-- BP8 piano riduzione token: model cascading su context budget.
-- Aggiunge la colonna escalation_threshold_tokens alla routing_matrix.
-- Quando il context stimato per il turno supera questa soglia, il routing
-- promuove a un modello con context window piu' ampio (Opus/Sonnet) oppure
-- declassifica a uno piu' economico (Haiku) per ridurre il costo.
-- Il valore NULL preserva il comportamento attuale (no cascading).

ALTER TABLE nexus_routing_matrix
    ADD COLUMN IF NOT EXISTS escalation_threshold_tokens INTEGER;

ALTER TABLE nexus_routing_matrix
    ADD COLUMN IF NOT EXISTS escalation_provider TEXT;

ALTER TABLE nexus_routing_matrix
    ADD COLUMN IF NOT EXISTS escalation_model_id TEXT;

COMMENT ON COLUMN nexus_routing_matrix.escalation_threshold_tokens IS
    'Soglia in token stimati sopra la quale il router escalation a escalation_provider/escalation_model_id. NULL = nessuna escalation.';

COMMENT ON COLUMN nexus_routing_matrix.escalation_provider IS
    'Provider target dell escalation (es. anthropic per upgrade a Opus).';

COMMENT ON COLUMN nexus_routing_matrix.escalation_model_id IS
    'Model id target dell escalation (es. claude-opus-4-7 per task lunghi).';

-- Esempio di seeding: per intent code/refactor con behavior_mode=bilanciata,
-- se il contesto > 100k token escalation a Sonnet (piu' largo).
-- L'amministratore puo' modificare via UI.
UPDATE nexus_routing_matrix
SET escalation_threshold_tokens = 100000,
    escalation_provider = provider,
    escalation_model_id = CASE
        WHEN model_id LIKE 'claude-haiku%' THEN 'claude-sonnet-4-5'
        WHEN model_id LIKE 'gpt-4o-mini%' THEN 'gpt-4o'
        WHEN model_id LIKE 'gemini-2.5-flash%' THEN 'gemini-2.5-pro'
        ELSE NULL
    END
WHERE intent IN ('code', 'code_edit', 'refactor', 'analyze', 'fix', 'implement')
  AND escalation_threshold_tokens IS NULL
  AND model_id IS NOT NULL;
