-- Fix M53: abilita modelli moderni in ai_price_catalog + aggiorna default
-- per provider + aggiorna routing_matrix con modelli piu recenti per famiglia.
--
-- Sintomo segnalato dall utente: "vedo sempre gli stessi modelli abbastanza
-- vecchi". Causa: il cron run_catalog_sync (24h) popola ai_price_catalog da
-- LiteLLM JSON (122 OpenAI, 49 Mistral, 37 Google, 21 Anthropic, 6 DeepSeek
-- al 2026-05-15) ma inserisce con is_enabled=false. Solo 22 modelli totali
-- erano enabled (seed iniziale 2026-05-05). Inoltre nexus_routing_matrix e
-- nexus_provider_default_model puntavano ancora ai modelli 2024-2025.
--
-- Modelli abilitati per provider (criterio: top reasoning + balanced + cheap
-- + coding-focused + legacy fallback, sempre dal piu recente disponibile in
-- catalog):

-- ── OpenAI: enable famiglia gpt-5 + codex + legacy gpt-4.1 ──
UPDATE ai_price_catalog SET is_enabled = true WHERE provider = 'openai' AND model IN (
    'gpt-5.5', 'gpt-5.5-pro',
    'gpt-5.4', 'gpt-5.4-mini', 'gpt-5.4-nano', 'gpt-5.4-pro',
    'gpt-5.3-chat-latest', 'gpt-5.3-codex',
    'gpt-5.2', 'gpt-5.2-pro', 'gpt-5.2-codex', 'gpt-5.2-chat-latest',
    'gpt-5.1', 'gpt-5.1-codex', 'gpt-5.1-codex-max', 'gpt-5.1-codex-mini', 'gpt-5.1-chat-latest',
    'gpt-5', 'gpt-5-pro', 'gpt-5-mini', 'gpt-5-nano', 'gpt-5-chat-latest', 'gpt-5-codex',
    'gpt-4.1', 'gpt-4.1-mini', 'gpt-4.1-nano',
    'gpt-4o', 'gpt-4o-mini',
    'o3', 'o3-pro', 'o3-mini', 'o4-mini', 'o1', 'o1-pro'
);

-- ── Anthropic: claude-4-x sonnet+opus+haiku (no obsolete claude-3 tranne haiku) ──
UPDATE ai_price_catalog SET is_enabled = true WHERE provider = 'anthropic' AND model IN (
    'claude-opus-4-7', 'claude-opus-4-7-20260416',
    'claude-opus-4-6', 'claude-opus-4-6-20260205',
    'claude-opus-4-5', 'claude-opus-4-5-20251101',
    'claude-opus-4-1', 'claude-opus-4-1-20250805',
    'claude-sonnet-4-6',
    'claude-sonnet-4-5', 'claude-sonnet-4-5-20250929',
    'claude-haiku-4-5', 'claude-haiku-4-5-20251001',
    'claude-3-haiku-20240307'
);

-- ── Google: famiglia 2.5 + nuove preview + legacy 2.0/1.5 ──
UPDATE ai_price_catalog SET is_enabled = true WHERE provider = 'google' AND model IN (
    'gemini-2.5-pro', 'gemini-2.5-flash', 'gemini-2.5-flash-lite',
    'gemini-2.5-flash-image', 'gemini-2.5-computer-use-preview-10-2025',
    'gemini-2.5-flash-native-audio-latest',
    'gemini-2.0-flash', 'gemini-2.0-flash-lite',
    'gemini-1.5-flash',
    'deep-research-pro-preview-12-2025'
);

-- ── Mistral: famiglia large-3 + medium-3 + magistral + devstral + codestral ──
UPDATE ai_price_catalog SET is_enabled = true WHERE provider = 'mistral' AND model IN (
    'mistral-large-3', 'mistral-large-latest', 'mistral-large-2411',
    'mistral-medium-3-1-2508', 'mistral-medium-latest', 'mistral-medium-2505',
    'mistral-small-latest', 'mistral-small-3-2-2506',
    'magistral-medium-latest', 'magistral-medium-2509',
    'magistral-small-latest',
    'codestral-latest', 'codestral-2508',
    'devstral-medium-latest', 'devstral-small-latest',
    'open-mistral-nemo'
);

-- ── DeepSeek: enable tutti (sono solo 6) ──
UPDATE ai_price_catalog SET is_enabled = true WHERE provider = 'deepseek' AND model IN (
    'deepseek-v3.2', 'deepseek-v3', 'deepseek-r1',
    'deepseek-reasoner', 'deepseek-chat', 'deepseek-coder'
);

-- ── Update default per provider (chiamato in fallback nexus_provider_default_model) ──
UPDATE nexus_provider_default_model SET model_id = 'gpt-5-mini',         updated_at = NOW(), notes = notes || ' | Aggiornato a gpt-5-mini (mig 0143)'         WHERE provider = 'openai' AND model_id = 'gpt-4o-mini';
UPDATE nexus_provider_default_model SET model_id = 'claude-sonnet-4-6',  updated_at = NOW()  WHERE provider = 'anthropic' AND model_id <> 'claude-sonnet-4-6';
UPDATE nexus_provider_default_model SET model_id = 'gemini-2.5-flash',   updated_at = NOW()  WHERE provider = 'google'   AND model_id <> 'gemini-2.5-flash';
UPDATE nexus_provider_default_model SET model_id = 'mistral-large-3',    updated_at = NOW(), notes = notes || ' | Aggiornato a mistral-large-3 (mig 0143)' WHERE provider = 'mistral' AND model_id = 'mistral-large-2411';
UPDATE nexus_provider_default_model SET model_id = 'deepseek-chat',      updated_at = NOW()  WHERE provider = 'deepseek' AND model_id <> 'deepseek-chat';

-- ── Update routing_matrix: porta i modelli alle versioni piu recenti per famiglia ──
-- (criterio: dove c'era una versione vecchia, sostituisco con la piu recente
-- coerente per intent+behavior_mode. Lascio i provider stessi.)

-- gpt-4.1-nano → gpt-5-nano (cheap/fast)
UPDATE nexus_routing_matrix SET model_id = 'gpt-5-nano',   updated_at = NOW() WHERE provider = 'openai' AND model_id = 'gpt-4.1-nano';
-- gpt-4.1-mini → gpt-5-mini (balanced)
UPDATE nexus_routing_matrix SET model_id = 'gpt-5-mini',   updated_at = NOW() WHERE provider = 'openai' AND model_id = 'gpt-4.1-mini';
-- gpt-4.1 → gpt-5 (top frontier)
UPDATE nexus_routing_matrix SET model_id = 'gpt-5',        updated_at = NOW() WHERE provider = 'openai' AND model_id = 'gpt-4.1';
-- gpt-4o-mini → gpt-5-mini
UPDATE nexus_routing_matrix SET model_id = 'gpt-5-mini',   updated_at = NOW() WHERE provider = 'openai' AND model_id = 'gpt-4o-mini';

-- claude-opus-4-6 → claude-opus-4-7 (latest frontier)
UPDATE nexus_routing_matrix SET model_id = 'claude-opus-4-7',  updated_at = NOW() WHERE provider = 'anthropic' AND model_id = 'claude-opus-4-6';

-- mistral-large-2411 → mistral-large-3 (latest top)
UPDATE nexus_routing_matrix SET model_id = 'mistral-large-3', updated_at = NOW() WHERE provider = 'mistral' AND model_id = 'mistral-large-2411';

-- mistral-small-latest e codestral-latest sono alias dinamici, lascia stare
-- gemini-2.5-flash e claude-haiku-4-5-20251001 sono ancora i piu recenti per quella famiglia

-- Reporting
SELECT 'enabled_per_provider' AS scope, provider, COUNT(*) AS count
FROM ai_price_catalog WHERE is_enabled = true
GROUP BY provider ORDER BY provider;
