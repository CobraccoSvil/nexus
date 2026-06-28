-- 0471_escalation_chain_from_catalog.sql
-- Catena di escalation modelli DERIVATA dal catalog (punto unico, regola L).
--
-- Causa radice (regola H): esistevano DUE fonti di verita' per "qual e' il prossimo
-- modello piu' capace" - il catalog ai_price_catalog (salute-aware, alimentato da
-- catalog_sync) e la tabella SEED MANUALE nexus_model_escalation_chain (mig 0128),
-- senza writer in produzione, senza JOIN al catalog, con target non validati contro
-- is_enabled/cooldown. Risultato: la catena puntava a modelli morti (deepseek ->
-- deepseek-reasoner non abilitato, mentre i reali v4-flash/v4-pro non avevano catena)
-- e i tier erano DUPLICATI e disallineati (gemini-2.5-pro heavy nel catalog vs medium
-- nella chain). Un nuovo modello sincronizzato non entrava mai nella catena (drift).
--
-- Fix strutturale: la catena diventa una VISTA derivata dal catalog. Cosi':
--   (1) il catalog e' l'unico punto di verita' (nessuna duplicazione di tier/dati);
--   (2) un nuovo modello sincronizzato entra AUTOMATICAMENTE (la vista si ricalcola);
--   (3) la resilienza e' ereditata: WHERE is_enabled = TRUE esclude i modelli morti
--       (auto-disabilitati dal model_health_probe); il consumer applica anche il
--       cooldown e i filtri tool/thinking via EligibilityFilter (punto unico);
--   (4) la catena e' RICCA: enumera TUTTI i modelli sani del provider ordinati per
--       escalation_rank crescente (dal piu' economico/leggero al piu' capace),
--       attraversando i tier - molti livelli reali, non 1-3 voci.
--
-- Assi di classificazione (nessun nuovo tier-valore inventato, regola L):
--   - CAPACITA': performance_tier {light, medium, heavy} (esistente nel catalog).
--   - COSTO (derivato): blended_cost = input*0.75 + output*0.25 (formula di costo,
--     non un model-id: pesa di piu' l'input, dominante negli agentici tool-heavy).
--   - SCORE GLOBALE (derivato, per l'ordine ricco multi-livello):
--     escalation_rank = tier_ord*1_000_000 + round(blended_cost*1000), con
--     tier_ord light:0 medium:1 heavy:2. Scala monotona "economico/leggero ->
--     capace/costoso" che attraversa i tier: ORDER BY escalation_rank ASC = catena.
--   - cost_bucket {speed, value, frontier}: comodita' di lettura, soglie note.
--
-- La vista NON memorizza nulla (zero terza copia del tier): e' pura proiezione.
-- La tabella nexus_model_escalation_chain NON viene droppata qui: resta finche' il
-- consumer Rust (PgEscalationPort::chain_for) non e' ricablato sulla vista; il DROP
-- e' una migrazione separata dopo la finestra di osservazione (regola H, no big-bang).

CREATE OR REPLACE VIEW v_model_escalation_chain AS
SELECT
    provider,
    model,
    performance_tier,
    speed_tier,
    is_enabled,
    consecutive_failures,
    consecutive_tool_failures,
    supports_tool_use,
    supports_vision,
    agentic_thinking_policy,
    capabilities,
    context_window,
    (input_cost_per_million_tokens * 0.75
        + output_cost_per_million_tokens * 0.25) AS blended_cost,
    (
        (CASE performance_tier
            WHEN 'light' THEN 0
            WHEN 'medium' THEN 1
            WHEN 'heavy' THEN 2
            ELSE 1
         END) * 1000000
        + round(
            (input_cost_per_million_tokens * 0.75
                + output_cost_per_million_tokens * 0.25) * 1000
          )
    )::bigint AS escalation_rank,
    CASE
        WHEN (input_cost_per_million_tokens * 0.75
                + output_cost_per_million_tokens * 0.25) < 0.5 THEN 'speed'
        WHEN (input_cost_per_million_tokens * 0.75
                + output_cost_per_million_tokens * 0.25) > 3.0 THEN 'frontier'
        ELSE 'value'
    END AS cost_bucket
FROM ai_price_catalog
WHERE is_enabled = TRUE;

COMMENT ON VIEW v_model_escalation_chain IS
    'Catena di escalation derivata dal catalog (mig 0471, regola L). Proiezione pura '
    'di ai_price_catalog (solo is_enabled) con blended_cost, escalation_rank e '
    'cost_bucket derivati. Ordina per escalation_rank ASC per la catena ricca. '
    'Sostituisce la tabella seed nexus_model_escalation_chain (DROP in mig separata '
    'dopo il ricablaggio del consumer).';
