-- 0326_upscale_target_tier.sql  (regola G)
--
-- Il setting `agent.upscale.preferred_targets` conteneva nomi modello
-- hardcoded (claude-opus-4-6, gemini-2.5-pro, gpt-5.5, claude-sonnet-4-6)
-- usati come whitelist per lo smart upscale (escalation contesto grande).
-- Violazione regola G: il routing deve usare il tier, non nomi specifici.
--
-- Sostituiamo con `agent.upscale.target_tier` (default 'heavy'): il codice
-- ora interroga ai_price_catalog scegliendo il modello con context_window
-- piu' grande nel tier configurato, supports_tool_use=true,
-- agentic_thinking_policy<>'exclude'. Tier-driven, auto-allineante: appena
-- un nuovo modello heavy entra nel catalog viene candidato automaticamente.

-- Aggiungi il nuovo setting (idempotente).
INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.upscale.target_tier',
    'heavy',
    'agents',
    'Tier (light|medium|heavy) usato dallo smart upscale quando il context window del modello corrente non basta. Il codice sceglie dinamicamente il modello con context_window piu grande nel tier (regola G).'
)
ON CONFLICT (key) DO NOTHING;

-- Rimuove il vecchio setting con nomi hardcoded (deprecato).
DELETE FROM settings WHERE key = 'agent.upscale.preferred_targets';
