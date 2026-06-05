-- 0320_model_selection_policy.sql  (ADR 0025, Parte 1)
--
-- Disponibilita' modelli DETERMINISTICA. Il catalog accumulava modelli vecchi:
-- il dump LiteLLM porta tutto lo storico e la discovery /v1/models espone modelli
-- deprecati che il probe-on-insert abilitava ("abilita tutto cio' che risponde").
-- Stato osservato: OpenAI 62 enabled/140 (gpt-3.5-turbo, gpt-4-0613, gpt-4-turbo),
-- ecc. Filosofia accrescitiva invece di "usa i modelli correnti".
--
-- Soluzione: policy DB-driven per provider (allowlist/denylist per FAMIGLIA, regex)
-- + prune deterministico dei modelli legacy. La discovery (sync_provider) abilitera'
-- solo i modelli che matchano l'allowlist e non la denylist (vedi model_catalog_sync.rs),
-- cosi' i vecchi non rientrano. Niente nomi modello hardcoded nel codice: i pattern
-- stanno nel DB (regola G). Auto-allineante: un nuovo modello della stessa famiglia
-- (es. gpt-5.6) rientra automaticamente; uno di famiglia legacy resta escluso.
--
-- Idempotente.

CREATE TABLE IF NOT EXISTS nexus_model_selection_policy (
    provider          text PRIMARY KEY,
    allowed_patterns  text[] NOT NULL DEFAULT '{}',   -- regex POSIX: famiglie correnti ammesse
    denied_patterns   text[] NOT NULL DEFAULT '{}',   -- regex POSIX: famiglie legacy escluse
    updated_at        timestamptz NOT NULL DEFAULT now()
);

-- Seed: famiglie correnti (allowed) e legacy (denied) per provider. Verificate sui
-- modelli realmente serviti (giugno 2026) + doc ufficiali. Denylist prudente: solo
-- famiglie chiaramente superate.
INSERT INTO nexus_model_selection_policy (provider, allowed_patterns, denied_patterns) VALUES
  ('openai',
   ARRAY['^gpt-5', '^gpt-4\.1', '^gpt-4o', '^o[1-9]'],
   ARRAY['^gpt-3', '^gpt-4$', '^gpt-4-', '^gpt-4-turbo', '^gpt-4-0613', '^gpt-4-vision', '^davinci', '^babbage']),
  ('anthropic',
   ARRAY['^claude-(opus|sonnet|haiku)-4'],
   ARRAY['^claude-3', '^claude-2', '^claude-instant']),
  ('google',
   ARRAY['^gemini-2\.5'],
   ARRAY['^gemini-1', '^gemma', 'image', 'embedding', '^aqa', '-tts', 'imagen']),
  ('deepseek',
   ARRAY['^deepseek-v4', '^deepseek-reasoner'],
   ARRAY['^deepseek-v3', '^deepseek-coder', '^deepseek-chat', '^deepseek-r1']),
  ('mistral',
   ARRAY['^mistral-(large|medium|small)', '^codestral', '^devstral', '^ministral', '^magistral', '^open-mistral-nemo'],
   ARRAY['^mistral-tiny', '^mistral-7b', '^mixtral', '^open-mistral-7b', 'c21211', '^open-mixtral'])
ON CONFLICT (provider) DO UPDATE SET
  allowed_patterns = EXCLUDED.allowed_patterns,
  denied_patterns  = EXCLUDED.denied_patterns,
  updated_at       = now();

-- Prune deterministico: disabilita i modelli che matchano una denylist del loro
-- provider. Reversibile (is_enabled=false, motivo tracciato). NON tocca le righe
-- gia' marcate manualmente (capability_source='manual') per non sovrascrivere
-- decisioni admin sui flag — ma il prune e' su is_enabled, ortogonale.
UPDATE ai_price_catalog c
SET is_enabled = false,
    auto_disabled_at = COALESCE(c.auto_disabled_at, now()),
    auto_disabled_reason = 'legacy: fuori model_selection_policy (mig 0320)',
    updated_at = now()
FROM nexus_model_selection_policy p
WHERE c.provider = p.provider
  AND c.is_enabled = true
  AND c.model ~ ANY(p.denied_patterns);

-- Audit di quanti modelli legacy restano enabled per provider (per il log di
-- applicazione migrazione e la verifica successiva). Nessun effetto sui dati.
DO $$
DECLARE r record;
BEGIN
  FOR r IN
    SELECT c.provider, count(*) AS legacy_enabled
    FROM ai_price_catalog c JOIN nexus_model_selection_policy p ON p.provider = c.provider
    WHERE c.is_enabled = true AND c.model ~ ANY(p.denied_patterns)
    GROUP BY c.provider
  LOOP
    RAISE WARNING 'model_selection_policy: % ha ancora % modelli legacy enabled (verificare denylist)', r.provider, r.legacy_enabled;
  END LOOP;
END $$;
