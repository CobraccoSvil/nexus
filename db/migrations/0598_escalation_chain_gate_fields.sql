-- 0598: la vista di escalation espone i campi del gate di qualificazione.
--
-- Perche' (censimento punti unici, 2026-07-15). `chain_for`
-- (agent_graph_adapter/escalation_port.rs) deriva da questa vista la catena di
-- escalation intra-provider — cioe' il modello a cui il motore SALE per uscire
-- da un loop agentico. Filtrava solo `supports_tool_use` e `context_window`:
--   - NON filtrava `agentic_thinking_policy <> 'exclude'`, benche' la vista lo
--     esponga gia': l'onda di allineamento "FASE 2b" aggiunse quel filtro al
--     promoter e SALTO' questo sito. Un modello inadatto ai tool-loop poteva
--     quindi essere promosso proprio dal meccanismo che serve a uscire da un
--     loop;
--   - non poteva applicare il gate di qualificazione (mig 0591/0595) ne'
--     escludere i modelli marcati morti dal probe, perche' la vista non
--     esponeva quei campi. Da qui questa migrazione.
--
-- La vista resta una PROIEZIONE (nessuna politica dentro): espone i fatti, e
-- chi seleziona decide. Il gate e' una scelta del routing, non della vista:
-- applicarlo qui dentro lo renderebbe invisibile e non disattivabile.
--
-- La scala tier di `escalation_rank`/`performance_tier_ord` e' invariata (e' gia'
-- a 5 livelli, corretta): la si ricopia identica perche' CREATE OR REPLACE VIEW
-- pretende la stessa lista di colonne nello stesso ordine. Resta uno SPECCHIO di
-- `nexus_agent_graph::decisions::tiers` — vedi `tier_rank_sql`, che dal lato Rust
-- genera l'ordinamento dal vocabolario unico.

CREATE OR REPLACE VIEW v_model_escalation_chain AS
SELECT provider,
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
    input_cost_per_million_tokens * 0.75 + output_cost_per_million_tokens * 0.25 AS blended_cost,
    ((
        CASE performance_tier
            WHEN 'light'::text THEN 0
            WHEN 'medium'::text THEN 1
            WHEN 'high'::text THEN 2
            WHEN 'heavy'::text THEN 3
            WHEN 'frontier'::text THEN 4
            ELSE 1
        END * 1000000)::numeric + round((input_cost_per_million_tokens * 0.75 + output_cost_per_million_tokens * 0.25) * 1000::numeric))::bigint AS escalation_rank,
        CASE
            WHEN (input_cost_per_million_tokens * 0.75 + output_cost_per_million_tokens * 0.25) < 0.5 THEN 'speed'::text
            WHEN (input_cost_per_million_tokens * 0.75 + output_cost_per_million_tokens * 0.25) > 3.0 THEN 'frontier'::text
            ELSE 'value'::text
        END AS cost_bucket,
        CASE performance_tier
            WHEN 'light'::text THEN 0
            WHEN 'medium'::text THEN 1
            WHEN 'high'::text THEN 2
            WHEN 'heavy'::text THEN 3
            WHEN 'frontier'::text THEN 4
            ELSE 1
        END AS performance_tier_ord,
    -- NUOVI: i fatti che servono al gate. La vista li espone, non li applica.
    qualification_state,
    qualification_expires_at,
    auto_disabled_reason
   FROM ai_price_catalog
  WHERE is_enabled = true;

COMMENT ON VIEW v_model_escalation_chain IS
  'Proiezione del catalog per la catena di escalation (mig 0471/0475/0528; '
  'campi del gate aggiunti in 0598). Espone i FATTI: il filtro di eleggibilita'' '
  '(gate di qualificazione, pre-GA, thinking-policy) e'' una decisione di chi '
  'seleziona, non della vista.';
