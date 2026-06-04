-- 0299_agent_tool_use_capability_gate.sql
-- ADR 0018 Fase 1(a): gate di capability sul routing agentico.
-- Espone in `settings` il flag che abilita il gate `supports_tool_use` per i run
-- agentici (intent != 'chat'). Default 'true': un modello con
-- ai_price_catalog.supports_tool_use=false non viene mai assegnato a un loop
-- agentico (caso reale: mistral-code-latest). Il codice Rust legge questo flag
-- con cache (crate::settings::get_setting); nessun fallback hardcoded (regola G).
-- Disattivabile ('false') solo per debug locale, mai in produzione.

INSERT INTO settings (key, value, category, description)
VALUES (
  'agent.require_tool_use_capability',
  'true',
  'agent',
  'Se true, il routing dei run agentici (intent != chat) scarta i modelli con ai_price_catalog.supports_tool_use=false e fa fallback al primo modello tool-capable del tier (ADR 0018 leva 0).'
)
ON CONFLICT (key) DO NOTHING;
