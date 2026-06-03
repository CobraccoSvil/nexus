-- 0271: probe tool-aware + catalog_sync probe-aware.
--
-- Root cause (inversione del catalog diagnosticata su sistema live):
--   1) model_health_probe usava SOLO un "ping" chat (generate_completion senza
--      tool). Cosi' un modello come mistral-large-latest risultava healthy=true
--      anche se col tool-forcing reale l'alias risolveva a un modello Labs ->
--      HTTP 403, facendo fallire i task agentici. Il probe era CIECO al
--      fallimento del tool-forcing -> modello rotto-per-agente restava
--      supports_tool_use=true.
--   2) catalog_sync disabilitava (is_enabled=false) modelli assenti dalla lista
--      upstream LiteLLM/provider ANCHE se il probe li trovava sani per l'account
--      (es. mistral-large-2411) -> spegneva il modello buono.
--
-- FIX 1 (probe tool-aware): accanto al ping chat, il worker esegue un tool-probe
--   sul path agente (generate_agent_turn) per i SOLI modelli supports_tool_use=true,
--   forzando una tool call su un tool fittizio. Fallimento (403/malformed/no
--   tool call) -> consecutive_tool_failures++; a soglia -> supports_tool_use=false
--   (NON is_enabled=false). Successo -> reset + riabilita tool-capability.
--   Riusa la colonna consecutive_tool_failures (mig 0269) e la soglia
--   agent.model_tool_failure_threshold (mig 0269). Niente nuove colonne.
--
-- FIX 2 (catalog_sync probe-aware): prima di disabilitare un modello assente da
--   upstream, controlla l'health reale (ai_model_health_history recente healthy
--   OPPURE consecutive_failures=0). Se sano per l'account -> NON disabilita.
--   Niente nuove colonne (decisione tracciata via ai_price_catalog_audit).

-- Flag enabled del tool-probe (regola G: config nel DB, niente env/hardcode).
INSERT INTO settings (key, value, category, description)
VALUES (
  'agent.model_tool_probe.enabled',
  'true',
  'routing',
  'Se true, model_health_probe esegue (oltre al ping chat) un tool-probe sul path agente per i soli modelli supports_tool_use=true: forza una tool call su un tool fittizio. A soglia (agent.model_tool_failure_threshold) marca supports_tool_use=false senza toccare is_enabled. Disattivabile per ridurre il costo delle chiamate API.'
)
ON CONFLICT (key) DO NOTHING;

-- Finestra di freschezza dell'health usata da catalog_sync (FIX 2): un modello
-- assente da upstream NON viene disabilitato se ha un probe healthy entro questa
-- finestra. Default 24h (allineato a cadenza probe 30m + margine).
INSERT INTO settings (key, value, category, description)
VALUES (
  'agent.catalog_sync_health_window_hours',
  '24',
  'routing',
  'Finestra (ore) entro cui un health check healthy=true rende un modello "recentemente sano" per il catalog_sync. Se sano e assente da upstream, catalog_sync lo lascia is_enabled=true (la verita e l account, non la lista upstream).'
)
ON CONFLICT (key) DO NOTHING;
