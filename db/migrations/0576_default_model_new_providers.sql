-- 0576_default_model_new_providers.sql
--
-- Fix radice: i provider onboardati Groq/OpenRouter/Perplexity non avevano una
-- riga in nexus_provider_default_model. Il health probe (provider_health_probe.rs
-- probe_one/probe_provider_once) e il routing statico usano
-- default_model_for_provider(&matrix, provider), che per un provider assente
-- ritorna il sentinel fail-loud `unknown-provider-<name>` (model_routing.rs:1071).
-- Risultato: il probe sondava i 3 provider con un modello INESISTENTE ->
-- HTTP 404/400 "model unknown-provider-groq does not exist" -> healthy=false ->
-- provider mostrati "down" nonostante le chiavi siano valide.
--
-- Le mig 0566-0568 seedavano registry+settings+catalog ma NON questo default:
-- lacuna dell'onboarding, qui colmata. Modelli scelti economici/veloci (adatti a
-- un probe leggero) e gia' abilitati nel catalog.

INSERT INTO nexus_provider_default_model (provider, model_id, notes) VALUES
    ('groq',       'llama-3.1-8b-instant', 'Default per health probe e routing statico (light, veloce).'),
    ('openrouter', 'z-ai/glm-5.2',         'Default per health probe e routing statico (high, economico).'),
    ('perplexity', 'sonar',                'Default per health probe e routing statico (ricerca web citata).')
ON CONFLICT (provider) DO NOTHING;
