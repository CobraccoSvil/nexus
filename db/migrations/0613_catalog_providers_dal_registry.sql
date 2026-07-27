-- 0613: il catalog_sync deduce i provider dal registry; policy per openrouter e groq.
--
-- Contesto (indagine 2026-07-17): il setting `catalog_sync.providers`
-- ('anthropic,openai,mistral,deepseek,google') era una LISTA HARDCODED che
-- duplicava cio' che `nexus_provider_registry` gia' sa (regola G+L). openrouter,
-- groq e perplexity erano nel registry (is_active=true, key/enabled valorizzati,
-- IDENTICI ai 5 nel CSV) ma FUORI dal CSV: la discovery live non girava per loro
-- pur vedendoli il gateway (openrouter 342 modelli, groq 17). Il catalogo ne
-- aveva 4 e 4.
--
-- Fix (codice, mig): la lista dei provider da sincronizzare ora e' DEDOTTA dal
-- registry (`providers_da_sincronizzare` in model_catalog_sync.rs). Il setting
-- CSV diventa un fossile e va rimosso: se restasse, sarebbe una seconda fonte di
-- verita' silente (nessuno lo legge piu', ma qualcuno potrebbe re-introdurlo).

-- (1) PEZZO 1: rimuovi il setting CSV. Un nuovo provider attivo+configurato nel
--     registry entra nella discovery da solo, senza toccare liste.
DELETE FROM settings WHERE key = 'catalog_sync.providers';

-- (2) PEZZO 3: policy di selezione per openrouter e groq.
--
--     Fino ad ora i due provider non avevano riga in nexus_model_selection_policy
--     -> `model_passes_selection_policy` ammetteva TUTTI (unwrap_or(true)). Con la
--     discovery finalmente attiva per loro (PEZZO 1) e la policy che governa anche
--     l'INSERT (PEZZO 2, model_catalog_sync.rs), senza una policy openrouter
--     inietterebbe 342 modelli grezzi. La policy filtra ai "chat/agentic seri",
--     escludendo i duplicati dei provider diretti (anthropic/openai/google/
--     mistralai) e il rumore (tts/whisper/image/embedding/guard/vision-variant/
--     distill/free/latest).
--
--     Pattern verificati con `~` POSIX sui nomi reali del gateway (BEGIN/ROLLBACK
--     sul DB vivo): openrouter 44 pass su 342, groq 5 su 17 (49 totali). Includono
--     kimi-k2 e varianti, minimax-m*, glm-4.5/4.6/4.7/5.x, grok-4.3/4.20/4.5,
--     nemotron-3-super/ultra, deepseek-r1, command-a, qwen3-max/235b/coder;
--     groq: compound(+mini), llama-4-scout, qwen3-32b, qwen3.6-27b.
--
--     denied_patterns identici per i due provider: il rumore e i duplicati dei
--     provider diretti sono gli stessi. Un modello passa se matcha un allowed E
--     nessun denied (stessa semantica di model_passes_selection_policy).
INSERT INTO nexus_model_selection_policy (provider, allowed_patterns, denied_patterns)
VALUES
  (
    'openrouter',
    ARRAY[
      '^moonshotai/kimi',
      '^minimax/minimax-m',
      '^z-ai/glm-[45]',
      '^x-ai/grok-[34]',
      '^nvidia/nemotron-3-(super|ultra)',
      '^deepseek/deepseek-(r1|v3\.1)',
      '^cohere/command-a',
      '^qwen/qwen3-(max|235b|coder-plus|coder-next)',
      '^qwen/qwen3\.[67]-(max|plus)'
    ]::text[],
    ARRAY[
      ':free$', '-tts', 'whisper', 'orpheus', 'image', 'imagen', '-audio',
      'guard', 'embedding', 'moderation', '-vl', 'latest$', 'distill',
      '[0-9]v$', 'v-turbo$',
      '^anthropic/', '^openai/', '^google/', '^mistralai/'
    ]::text[]
  ),
  (
    'groq',
    ARRAY[
      '^meta-llama/llama-4',
      '^groq/compound',
      '^qwen/qwen3',
      '^moonshotai/kimi'
    ]::text[],
    ARRAY[
      ':free$', '-tts', 'whisper', 'orpheus', 'image', 'imagen', '-audio',
      'guard', 'embedding', 'moderation', '-vl', 'latest$', 'distill',
      '[0-9]v$', 'v-turbo$',
      '^anthropic/', '^openai/', '^google/', '^mistralai/'
    ]::text[]
  )
ON CONFLICT (provider) DO UPDATE
  SET allowed_patterns = EXCLUDED.allowed_patterns,
      denied_patterns = EXCLUDED.denied_patterns,
      updated_at = NOW();
