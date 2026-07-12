-- 0565_provider_registry.sql
-- Registry provider data-driven (Parte F2): sposta nel DB la REGISTRAZIONE dei
-- provider LLM (quali esistono, con quale api_key/enabled/base_url e formato API),
-- oggi cablata a mano nel bootstrap del gateway (`ProviderKeys` a campi fissi +
-- blocchi `if let`). Regola G (config nel DB) + L (punto unico cross-crate).
--
-- Cosa NON fa: NON sposta i QUIRK per-provider nel DB (o-series OpenAI, XML/thinking
-- DeepSeek, prompt-cache Anthropic, Vertex/region Google) — quelli restano nei loro
-- adapter dedicati, selezionati per nome dalla factory del bootstrap. La
-- parametrizzazione dei quirk e' F3. Qui i provider con quirk hanno `api_format`
-- dedicato; i provider OpenAI-compatibili PURI (mistral, vllm, e i futuri
-- perplexity/openrouter/groq) hanno `api_format='openai_compat'` e usano il
-- provider generico costruito da questi campi.
--
-- Comportamento invariato: il seed replica esattamente la registrazione attuale
-- (stessi setting api_key/enabled, stesse capacita' dei wrapper mistral.rs/vllm.rs).
-- Il gateway usa il registry se presente, altrimenti ricade sui 6 provider noti
-- (fail-safe: se questa migrazione non e' ancora applicata all'avvio, nessuna
-- regressione).

CREATE TABLE IF NOT EXISTS nexus_provider_registry (
    name               TEXT PRIMARY KEY,
    -- Formato API -> seleziona l'adapter/costruttore nel bootstrap.
    api_format         TEXT NOT NULL
                       CHECK (api_format IN ('openai', 'anthropic', 'google', 'deepseek', 'openai_compat')),
    -- Nomi dei setting da cui il loader risolve chiave/abilitazione/base_url.
    key_setting        TEXT,              -- NULL per provider senza api_key (vllm)
    enabled_setting    TEXT,              -- NULL = nessun flag *_enabled dedicato
    base_url_setting   TEXT,              -- setting per override base_url (NULL = non applicabile)
    base_url_default   TEXT,              -- default se il setting manca (NULL = costante nel costruttore dedicato)
    -- Criterio di attivazione (l'eterogeneita' storica: google via SA, vllm via url).
    activation         TEXT NOT NULL DEFAULT 'api_key'
                       CHECK (activation IN ('api_key', 'base_url', 'api_key_or_vertex')),
    -- Capacita' USATE dal provider generico (openai_compat). Per gli adapter
    -- dedicati sono documentali: il costruttore dedicato le hardcoda.
    tiers              INTEGER[] NOT NULL DEFAULT '{0,1,2}',
    max_context_tokens INTEGER NOT NULL DEFAULT 128000,
    supports_tools     BOOLEAN NOT NULL DEFAULT true,
    is_active          BOOLEAN NOT NULL DEFAULT true,
    sort_order         INTEGER NOT NULL DEFAULT 100,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Seed dei 6 provider attuali (registrazione IDENTICA a bootstrap.rs::ProviderKeys).
-- tiers/max_context/supports_tools rilevanti solo per api_format='openai_compat'
-- (mistral, vllm): valori presi da providers/mistral.rs e providers/vllm.rs.
INSERT INTO nexus_provider_registry
    (name, api_format, key_setting, enabled_setting, base_url_setting, base_url_default, activation, tiers, max_context_tokens, supports_tools, sort_order)
VALUES
    ('openai',    'openai',        'openai_api_key',    'openai_enabled',    'openai_base_url',    NULL,                          'api_key',           '{0,1,2}',   400000,  true, 10),
    ('anthropic', 'anthropic',     'anthropic_api_key', 'anthropic_enabled', 'anthropic_base_url', NULL,                          'api_key',           '{0,1,2}',   200000,  true, 20),
    ('google',    'google',        'google_api_key',    'google_enabled',    NULL,                 NULL,                          'api_key_or_vertex', '{0,1,2}',   1000000, true, 30),
    ('mistral',   'openai_compat', 'mistral_api_key',   'mistral_enabled',   'mistral_base_url',   'https://api.mistral.ai/v1',   'api_key',           '{0,1,2}',   128000,  true, 40),
    ('deepseek',  'deepseek',      'deepseek_api_key',  'deepseek_enabled',  'deepseek_base_url',  NULL,                          'api_key',           '{0,1,2}',   128000,  true, 50),
    ('vllm',      'openai_compat', NULL,                NULL,                'vllm_base_url',      NULL,                          'base_url',          '{0,1,2,3}', 32768,   true, 60)
ON CONFLICT (name) DO NOTHING;
