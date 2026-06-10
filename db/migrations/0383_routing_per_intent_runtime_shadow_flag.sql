-- 0383_routing_per_intent_runtime_shadow_flag.sql
--
-- FASE 3 del consolidamento routing (ADR 0030), Stadio 1: flag dello SHADOW-COMPARE
-- del routing per-intent. Quando 'true', resolve_agent_provider calcola IN PARALLELO
-- (senza cambiare la decisione servita) la risoluzione tier-runtime via i requirements
-- e logga la divergenza vs il lookup statico (target tracing "routing_shadow"), per
-- misurare la parita' su traffico reale PRIMA di abilitare il routing runtime (stadi 2-3).
--
-- Default 'false' (regola G: niente attivazione implicita; kill-switch via questo flag).
-- Lo shadow gira SOLO sugli intent senza manual_override attivo.

INSERT INTO settings (key, value, category, description)
VALUES (
    'routing.per_intent_runtime_shadow',
    'false',
    'routing',
    'FASE 3 (ADR 0030) Stadio 1: se true, abilita lo shadow-compare del routing per-intent (logga divergenza statico vs tier-runtime, NON cambia la decisione servita). Default false. Solo intent senza manual_override.'
)
ON CONFLICT (key) DO NOTHING;
