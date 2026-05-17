-- Dispatcher: modello LLM per fallback classifier (eventi custom).
-- Usato solo per eventi non coperti dalle regole hardcoded.
-- Cache Redis 1h, timeout 800ms, budget 50 token: costo trascurabile.

INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes) VALUES
    ('ui_hint_classifier', 'anthropic', 'claude-haiku-4-5-20251001',
     'dispatcher classifier fallback per eventi Custom non coperti dalle regole')
ON CONFLICT (purpose) DO NOTHING;
