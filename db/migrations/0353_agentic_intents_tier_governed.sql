-- 0353_agentic_intents_tier_governed.sql
--
-- Porta gli intent AGENTICI core (agentic_default, debug, system_admin,
-- code_read) sotto il meccanismo di routing per TIER, eliminando i pin statici.
--
-- DIAGNOSI:
--   La filosofia del sistema e': nexus_intent_routing_requirements definisce il
--   solo dato fisso (tier + tool_use + capability) per ogni (intent,
--   behavior_mode); il routing_matrix_auto_promoter sceglie provider+modello
--   dinamicamente dal catalog (disponibilita', cooldown, costo). La
--   nexus_routing_matrix e' una CACHE materializzata da quel processo.
--
--   MA agentic_default/debug/system_admin/code_read non erano nei requirements:
--   esistevano solo come righe matrix statiche con manual_override=true (pin di
--   mig 0260/0268/0270/0274/0337). Senza requirements l'auto-promoter non li
--   governava; coi pin la matrix non si aggiornava mai -> modelli morti.
--
-- FIX (regola H/G):
--   1) INSERT dei requirements (tier) per i 4 intent x 4 mode.
--   2) Rimozione di manual_override dalle righe matrix di quegli intent, cosi'
--      l'auto-promoter le ripopola e mantiene per tier+disponibilita'.
--   Da qui in poi: per gli intent NESSUN provider/modello fisso. Solo il tier.
--
-- Idempotente: ON CONFLICT sui requirements, UPDATE condizionato sulla matrix.

-- 1) Requirements (tier) per gli intent agentici. Modellati su file_ops /
--    fix_complesso. weight_* lasciati ai default della tabella.
INSERT INTO nexus_intent_routing_requirements
    (intent, behavior_mode, preferred_tier, requires_tool_use, required_capabilities, cost_direction)
VALUES
    -- agentic_default: tuttofare agentico (tool-loop). Capace ma non sempre heavy.
    ('agentic_default', 'approfondita', 'heavy',  true, ARRAY['code','reasoning'], 'desc'),
    ('agentic_default', 'bilanciata',   'medium', true, ARRAY['code','reasoning'], 'asc'),
    ('agentic_default', 'economica',    'medium', true, ARRAY['code'],             'asc'),
    ('agentic_default', 'veloce',       'medium', true, ARRAY['code'],             'asc'),
    -- debug: richiede reasoning forte + tool. Heavy nelle modalita' alte.
    ('debug', 'approfondita', 'heavy',  true, ARRAY['code','reasoning','fix'], 'desc'),
    ('debug', 'bilanciata',   'heavy',  true, ARRAY['code','reasoning','fix'], 'desc'),
    ('debug', 'economica',    'medium', true, ARRAY['code','fix'],             'asc'),
    ('debug', 'veloce',       'medium', true, ARRAY['code','fix'],             'asc'),
    -- system_admin: operazioni rischiose (comandi/deploy). Modello capace.
    ('system_admin', 'approfondita', 'heavy',  true, ARRAY['code','reasoning'], 'desc'),
    ('system_admin', 'bilanciata',   'heavy',  true, ARRAY['code','reasoning'], 'desc'),
    ('system_admin', 'economica',    'medium', true, ARRAY['code'],             'asc'),
    ('system_admin', 'veloce',       'medium', true, ARRAY['code'],             'asc'),
    -- code_read: lettura/comprensione codice. Tool per leggere file, long-context.
    ('code_read', 'approfondita', 'medium', true, ARRAY['code','long-context'], 'desc'),
    ('code_read', 'bilanciata',   'medium', true, ARRAY['code','long-context'], 'asc'),
    ('code_read', 'economica',    'light',  true, ARRAY['code'],                'asc'),
    ('code_read', 'veloce',       'light',  true, ARRAY['code'],                'asc')
ON CONFLICT (intent, behavior_mode) DO NOTHING;

-- 2) Libera i pin statici: rimuove manual_override dalle righe matrix degli
--    intent agentici, cosi' il promoter le governa per tier. NON tocca gli
--    intent fuori da questo set (eventuali pin volontari restano).
UPDATE nexus_routing_matrix
   SET manual_override = false,
       notes = COALESCE(notes, '') ||
               CASE WHEN COALESCE(notes,'') LIKE '%[0353 unpin]%'
                    THEN '' ELSE ' [0353 unpin: tier-governed]' END,
       updated_at = NOW()
 WHERE intent IN ('agentic_default', 'debug', 'system_admin', 'code_read')
   AND COALESCE(manual_override, false) = true;
