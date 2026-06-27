-- 0461_project_analyzer_purpose.sql
--
-- Purpose dedicato per il deep-analyzer di progetto (POST /api/projects/:id/deep-analyze).
--
-- Contesto (cutover brain->Rust): la chiamata LLM dell'agente
-- agent.project.analyzer viveva nel brain Python (POST /agent/project-analyze),
-- che caricava una "provider chain" da nexus_provider_default_model e iterava
-- i provider a mano (fallback chain duplicata). Portata al gateway Rust
-- (NexusGatewayClient) in mcp-core: deep_analyze.rs ora risolve il modello via
-- il PUNTO UNICO resolve_purpose_model_db (regola L) e il fallback per
-- cooldown/billing e' delegato a best_model_for_tier (catalog + cooldown gate),
-- senza re-implementare il loop chain.
--
-- Per farlo serve un purpose con `tier` valorizzato (lo schema purpose e'
-- tier-only dalla mig 0344): il deep-analyzer emette un JSON di insights
-- (output strutturato di media lunghezza, niente loop tool-use), quindi
-- tier = 'medium' e requires_tool_use = false. Coerente con docs_generator
-- (mig 0343) ma su tier piu' economico perche' l'analyzer gira in background
-- su molti progetti.
--
-- DB-driven (regola G): nessun nome modello hardcoded nel codice; il tier e i
-- modelli candidati restano interamente configurabili da questa tabella +
-- ai_price_catalog. Idempotente.

INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
VALUES (
    'project_analyzer',
    'openai',
    'gpt-4.1-mini',
    'medium',
    NULL,
    false,
    'Deep-analyzer di progetto (deep_analyze.rs). Routing per tier=medium '
    || '(JSON insights strutturato, modello capace ma economico, gira in '
    || 'background). provider/model_id statici NON usati (schema tier-only); '
    || 'fallback per cooldown via best_model_for_tier. Mig 0461 (cutover brain->Rust).'
)
ON CONFLICT (purpose) DO UPDATE
    SET tier = EXCLUDED.tier,
        required_capability = EXCLUDED.required_capability,
        requires_tool_use = EXCLUDED.requires_tool_use,
        notes = EXCLUDED.notes,
        updated_at = NOW();
