-- M171: Purpose model per provider test_connection() e admin tool selection.
--
-- Motivazione (CLAUDE.md §G — niente magic fallback):
--
-- 1) `brain/providers/anthropic_provider.py:646` aveva `model="claude-haiku-..."`
--    hardcoded nella chiamata di ping `test_connection()`. Lo sostituiamo con
--    lookup `nexus_purpose_model` purpose='provider_test_connection.anthropic'.
--    Stessa pattern applicabile a OpenAI/Google/Mistral/Deepseek se in futuro
--    le rispettive `test_connection` richiederanno un modello dichiarato.
--
-- 2) `crates/admin-service/src/prompt_templates.rs:976` aveva claude-haiku
--    hardcoded come `"model": "..."` nella request a `/generate` per
--    l'auto-selezione di tool da catalogo MCP. Lo sostituiamo con lookup
--    purpose='admin.tool_selection'.
--
-- Modello scelto in entrambi i casi: claude-haiku-4-5-20251001 — sufficiente
-- (single-turn ping / classificazione breve) e a basso costo. Modificabile
-- via admin senza redeploy.

INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes) VALUES
    ('provider_test_connection.anthropic', 'anthropic', 'claude-haiku-4-5-20251001',
     'Ping di test_connection() per il provider Anthropic — modello minimo, max_tokens=10'),
    ('admin.tool_selection',               'anthropic', 'claude-haiku-4-5-20251001',
     'Auto-selezione tool MCP per prompt template (admin-service/prompt_templates.rs)')
ON CONFLICT (purpose) DO NOTHING;
