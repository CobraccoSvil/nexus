-- 0324_planner_non_thinking_primary.sql  (ADR 0025)
--
-- Il PLANNER (planner_node) forza tool_choice su nexus_todo_write. La sua
-- tier-rule era tier='heavy'+capability='reasoning', e nel tier heavy l'UNICO
-- modello tool-capable e' gemini-2.5-pro (thinking). Sotto tool_choice forzato i
-- modelli thinking Vertex ritornano sistematicamente MALFORMED_FUNCTION_CALL /
-- output vuoto (confermato a runtime: "planner_node: nessuna tool call dal
-- primario google/gemini-2.5-pro"). Risultato: il planner non crea mai il piano
-- al primo colpo, entra in retry/loop e il run termina con "completamento vuoto".
--
-- Fix strutturale (ADR 0025: non-thinking per i tool-loop, a maggior ragione
-- sotto tool_choice FORZATO): il planner deve partire da un modello NATIVAMENTE
-- non-thinking (policy='none'), che emette la tool call in modo affidabile
-- (verificato: codestral-latest / mistral-small-latest emettono nexus_todo_write).
--
--   - Disattiva la tier-rule del planner (tier=NULL): la risoluzione tier-based
--     (best_model_for_tier -> heavy -> gemini-2.5-pro) veniva PRIMA dello statico
--     (vedi internal_routing.rs::resolve_purpose). Azzerandola, resolve_purpose
--     usa il (provider, model_id) statico.
--   - Riconcilia il (provider, model_id) statico del planner al MIGLIOR modello
--     ENABLED non-thinking tool-capable, preferendo capacita' (tier medium/heavy)
--     poi featured poi costo. Selezione DINAMICA (nessun nome hardcoded, regola G):
--     una famiglia non-thinking nuova/migliore subentra automaticamente.
--
-- planner_fallback resta la rete di sicurezza (gia' non-thinking via FIX 1).
-- Idempotente.

WITH best_nt AS (
    SELECT provider, model
    FROM ai_price_catalog
    WHERE is_enabled = true
      AND supports_tool_use = true
      AND agentic_thinking_policy = 'none'
    ORDER BY
      CASE performance_tier WHEN 'heavy' THEN 3 WHEN 'medium' THEN 2 ELSE 1 END DESC,
      is_featured DESC,
      input_cost_per_million_tokens ASC NULLS LAST
    LIMIT 1
)
UPDATE nexus_purpose_model pm
SET tier = NULL,
    provider = b.provider,
    model_id = b.model,
    updated_at = NOW()
FROM best_nt b
WHERE pm.purpose = 'planner';
