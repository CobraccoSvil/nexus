-- Migrazione 0125: flag operativi e parametri di tuning migrati da env var a tabella settings
--
-- Ogni impostazione conserva l'env var come override di emergenza (priorita' piu' alta).
-- Il valore canonico e' nel DB e modificabile da /admin/settings senza redeploy.
--
-- Non migrate (necessitano dell'env var prima che il DB sia raggiungibile):
--   DATABASE_URL, REDIS_URL, QDRANT_URL, JWT_SECRET, *_SERVICE_PORT,
--   NEXT_PUBLIC_*, BRAIN_URL, CORE_SERVICE_URL, RUST_LOG, NODE_ENV.

INSERT INTO settings (key, value, category, description, is_secret) VALUES

    -- ── Categoria: agent ──────────────────────────────────────────────────────

    ('tool_runner_enabled',
     'true',
     'agent',
     'Abilita il server gRPC ToolRunner (porta 50071) usato dal brain LangGraph per '
     'eseguire i tool Nexus builtin. Override di emergenza: ENABLE_TOOL_RUNNER=1. '
     'Richiede riavvio di mcp-core per applicare la modifica.',
     FALSE),

    ('llm_classifier_enabled',
     'true',
     'agent',
     'Abilita il classificatore LLM degli intent (chiamata REST /classify-intent-agentic '
     'al brain Python). Se false usa solo keyword matching locale: piu'' veloce ma meno '
     'preciso. Override di emergenza: NEXUS_LLM_CLASSIFIER_ENABLED=false. '
     'Richiede riavvio di mcp-core.',
     FALSE),

    ('anthropic_system_cache_ttl',
     '1h',
     'agent',
     'TTL della cache prompt di sistema per Anthropic: 5m (default Anthropic) o 1h '
     '(extended-cache-ttl-2025-04-11). Il valore 1h massimizza il cache hit rate fra '
     'turni distanti (il system prompt cambia raramente). Override: '
     'NEXUS_ANTHROPIC_SYSTEM_CACHE_TTL. Richiede riavvio del brain.',
     FALSE),

    ('terminal_default_shell',
     'bash',
     'agent',
     'Shell di default per i terminali agente: bash, zsh, fish. Su Windows: '
     'powershell.exe. Override di emergenza: TERMINAL_SHELL. '
     'Richiede riavvio del brain e di mcp-core.',
     FALSE),

    -- ── Categoria: monitoring ─────────────────────────────────────────────────

    ('provider_health_probe_enabled',
     'true',
     'monitoring',
     'Abilita il worker di health-check periodico dei provider LLM. Ogni ciclo invia '
     'una richiesta minimale a ciascun provider configurato per rilevare cooldown / '
     'quota esaurita prima del primo errore reale. Override: '
     'NEXUS_PROVIDER_HEALTH_PROBE_ENABLED=false. Richiede riavvio di mcp-core.',
     FALSE),

    ('provider_health_probe_interval_s',
     '300',
     'monitoring',
     'Intervallo in secondi tra i cicli di health-check provider (minimo 60, '
     'default 300 = 5 minuti). Abbassarlo aumenta la reattivita'' ma aggiunge '
     'costo token marginale. Override: NEXUS_PROVIDER_HEALTH_PROBE_INTERVAL_S. '
     'Richiede riavvio di mcp-core.',
     FALSE),

    -- ── Categoria: gateway ────────────────────────────────────────────────────

    ('http_timeout_secs',
     '30',
     'gateway',
     'Timeout in secondi per il client HTTP Nexus verso i provider LLM e i servizi '
     'interni (default 30). Aumentare se si usano modelli lenti con streaming disabilitato. '
     'Override: NEXUS_HTTP_TIMEOUT_SECS. Richiede riavvio di mcp-core.',
     FALSE),

    ('http_pool_max',
     '20',
     'gateway',
     'Numero massimo di connessioni idle per host nel pool HTTP Nexus (default 20). '
     'Aumentare in ambienti ad alto parallelismo (>10 utenti simultanei). '
     'Override: NEXUS_HTTP_POOL_MAX. Richiede riavvio di mcp-core.',
     FALSE),

    ('nexus_profile',
     'cloud',
     'gateway',
     'Profilo operativo del gateway LLM: cloud, onprem, hybrid. Determina quale file '
     'di policy viene caricato da config/policies/ e quali tier di modelli sono '
     'consentiti. Override: NEXUS_PROFILE. Richiede riavvio di mcp-core.',
     FALSE),

    -- ── Categoria: billing ────────────────────────────────────────────────────

    ('brain_billing_enabled',
     'false',
     'billing',
     'Abilita la registrazione di utilizzo AI nel ledger billing (tabella ai_usage_ledger) '
     'dal brain Python. Tenere false in sviluppo locale per non inquinare i dati di '
     'produzione. Override di emergenza: NEXUS_BRAIN_BILLING=on. '
     'Richiede riavvio del brain.',
     FALSE),

    -- ── Categoria: projects ───────────────────────────────────────────────────

    ('extra_project_roots',
     '',
     'projects',
     'Lista separata da virgola di percorsi extra ammessi per il browse progetti '
     '(es. /mnt/data,/opt/repos). Vuoto = solo la root del progetto attivo. '
     'Override di emergenza: NEXUS_EXTRA_ROOTS. Richiede riavvio di mcp-core.',
     FALSE),

    -- ── Categoria: system ─────────────────────────────────────────────────────

    ('brain_log_level',
     'info',
     'system',
     'Livello di log del brain Python: debug, info, warning, error. '
     'In sviluppo locale si usa debug; in produzione info. '
     'Override di emergenza: LOG_LEVEL. Richiede riavvio del brain.',
     FALSE)

ON CONFLICT (key) DO UPDATE
    SET description = EXCLUDED.description,
        category    = EXCLUDED.category;
