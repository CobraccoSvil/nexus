-- 0358_intent_capability_agentic.sql
--
-- Allinea nexus_intent_capability (mig 0110) con gli intent che il classifier
-- produce ma che NON erano mappati, causandone il degrado a modelli light.
--
-- DIAGNOSI (verificata sui log + DB):
--   Il behavior_mode di default e' 'dinamico' (settings.nexus_behavior_mode), per
--   cui il routing agentico passa dal ramo "dinamico/catalog" in
--   orchestrator/core.rs: prende (tier, capability) da nexus_intent_capability e
--   chiama route_model_from_catalog. MA agentic_default (e code_read, fix_*,
--   chat_*) NON erano in nexus_intent_capability: il codice cadeva nel default
--   hardcoded ("light","chat") -> route_model_from_catalog(light) ->
--   mistral-small-latest. Per questo i run agentici continuavano a usare un
--   modello light nonostante i fix su nexus_routing_matrix (0353) e sulla
--   slot-matrix (0357), che governano percorsi DIVERSI.
--
-- FIX in due parti (regola G/H):
--   1) Codice: rimosso il magic fallback "light" nel ramo dinamico
--      (orchestrator/core.rs) -> default medium/reasoning + WARN.
--   2) Questa migrazione: popola i tier corretti per gli intent agentici, cosi'
--      il ramo dinamico sceglie un modello adeguato (es. mistral-large per il
--      tier medium) invece di mistral-small.
--
-- Tier scelti coerenti con nexus_intent_routing_requirements (0353):
--   agentic_default=medium (tuttofare con tool), code_read=medium (long-context),
--   fix_semplice=light, fix_complesso=heavy, chat_breve/media=light,
--   chat_lunga=medium.
--
-- preferred_provider lasciato NULL di proposito: il router sceglie provider per
--   tier+disponibilita' (niente provider fisso), come da filosofia di routing.
--
-- DEBITO NOTO (regola L): esistono DUE fonti di "tier per intent" disallineate
--   - nexus_intent_capability (0110, ramo dinamico)
--   - nexus_intent_routing_requirements (0353, auto-promoter -> routing_matrix)
--   Vanno consolidate in un punto unico (task separato). Questa migrazione le
--   riallinea sui dati; il consolidamento strutturale resta da fare.
--
-- Idempotente: ON CONFLICT (intent) DO NOTHING.

INSERT INTO nexus_intent_capability
    (intent, base_tier, base_capability, preferred_provider, notes)
VALUES
    ('agentic_default', 'medium', 'reasoning', NULL,
     'Tuttofare agentico con tool-loop: medium di base, niente degrado a light'),
    ('code_read',       'medium', 'code',      NULL,
     'Lettura/comprensione codice: medium (long-context)'),
    ('fix_semplice',    'light',  'code',      NULL,
     'Fix puntuale single-file'),
    ('fix_complesso',   'heavy',  'reasoning', NULL,
     'Fix multi-file/cross-service: ragionamento esteso'),
    ('chat_breve',      'light',  'chat',      NULL,
     'Chat breve'),
    ('chat_media',      'light',  'chat',      NULL,
     'Chat di lunghezza media'),
    ('chat_lunga',      'medium', 'chat',      NULL,
     'Chat lunga / contesto ampio')
ON CONFLICT (intent) DO NOTHING;
