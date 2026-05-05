-- Migration 0053: Agent Registry — registro degli agent types.
--
-- Persistenza del registro degli agenti per Nexus:
--   nexus_agent_types   → definizioni degli agent type (60+)
--   nexus_agent_skills  → skill capabilities per agente
--   nexus_agent_stats   → statistiche runtime aggregate
--
-- Sincronizzato con nexus-agents::AgentType enum.
-- Le righe vengono inserite all'avvio del servizio (upsert).

-- ---------------------------------------------------------------------------
-- Enum: agent category
-- ---------------------------------------------------------------------------
DO $$ BEGIN
    CREATE TYPE agent_category AS ENUM (
        'core',
        'specialized',
        'github',
        'research',
        'ops',
        'custom'
    );
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

-- ---------------------------------------------------------------------------
-- Tabella: nexus_agent_types
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS nexus_agent_types (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Chiave tecnica (corrisponde al variant dell'enum Rust, snake_case)
    agent_key       TEXT        NOT NULL UNIQUE,
    -- Nome display
    display_name    TEXT        NOT NULL,
    category        agent_category NOT NULL DEFAULT 'core',
    description     TEXT        NOT NULL DEFAULT '',
    -- Embedding del profilo agente (384d MiniLM), per HNSW seed
    profile_embedding float4[],
    -- Configurazione default (timeout, max_iterations, ecc.)
    default_config  JSONB       NOT NULL DEFAULT '{}',
    -- Feature flags
    enabled         BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE nexus_agent_types IS
    'Registry degli agent types disponibili nel sistema Nexus.';

CREATE INDEX IF NOT EXISTS idx_nexus_agent_types_category
    ON nexus_agent_types (category)
    WHERE enabled = TRUE;

-- ---------------------------------------------------------------------------
-- Tabella: nexus_agent_skills
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS nexus_agent_skills (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_key       TEXT        NOT NULL REFERENCES nexus_agent_types(agent_key) ON DELETE CASCADE,
    skill_name      TEXT        NOT NULL,
    description     TEXT        NOT NULL DEFAULT '',
    -- Task types per cui questo skill è applicabile
    applicable_tasks TEXT[]     NOT NULL DEFAULT '{}',
    -- Priorità (0-100)
    priority        INTEGER     NOT NULL DEFAULT 50,
    UNIQUE (agent_key, skill_name)
);

CREATE INDEX IF NOT EXISTS idx_nexus_agent_skills_agent
    ON nexus_agent_skills (agent_key);

-- ---------------------------------------------------------------------------
-- Tabella: nexus_agent_stats
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS nexus_agent_stats (
    agent_key           TEXT        PRIMARY KEY REFERENCES nexus_agent_types(agent_key) ON DELETE CASCADE,
    total_executions    BIGINT      NOT NULL DEFAULT 0,
    successful_executions BIGINT    NOT NULL DEFAULT 0,
    failed_executions   BIGINT      NOT NULL DEFAULT 0,
    avg_quality_score   REAL        NOT NULL DEFAULT 0.0,
    avg_execution_ms    REAL        NOT NULL DEFAULT 0.0,
    -- Aggiornato ogni volta che viene chiamato update_q_value
    last_executed_at    TIMESTAMPTZ,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- ---------------------------------------------------------------------------
-- Dati seed: core agent types
-- ---------------------------------------------------------------------------
INSERT INTO nexus_agent_types (agent_key, display_name, category, description)
VALUES
    -- Core (4)
    ('coder',      'Coder',      'core',       'Implementa feature, scrive codice, crea funzioni e moduli'),
    ('tester',     'Tester',     'core',       'Scrive test, verifica coverage, quality assurance'),
    ('reviewer',   'Reviewer',   'core',       'Revisiona codice, trova bug, suggerisce miglioramenti'),
    ('architect',  'Architect',  'core',       'Progetta architetture, decisioni tecniche, system design'),

    -- Specialized (12)
    ('security_architect',      'Security Architect',      'specialized', 'SAST, dependency audit, vulnerability analysis'),
    ('performance_engineer',    'Performance Engineer',    'specialized', 'Ottimizzazione query, profiling, benchmarking'),
    ('database_designer',       'Database Designer',       'specialized', 'Schema design, migrazioni, query optimization'),
    ('frontend_specialist',     'Frontend Specialist',     'specialized', 'UI/UX, React/Next.js, responsive design'),
    ('devops_engineer',         'DevOps Engineer',         'ops',         'CI/CD, Docker, Kubernetes, deployment automation'),
    ('api_designer',            'API Designer',            'specialized', 'REST/GraphQL API design, OpenAPI spec'),
    ('documenter',              'Documenter',              'specialized', 'Documentazione tecnica, README, ADR'),
    ('researcher',              'Researcher',              'research',    'Ricerca tecnologica, analisi comparativa, POC'),
    ('analyst',                 'Analyst',                 'research',    'Analisi requisiti, specifiche, business logic'),
    ('optimizer',               'Optimizer',               'specialized', 'Refactoring, code smell, performance improvements'),
    ('debugger',                'Debugger',                'specialized', 'Root cause analysis, log analysis, fix bugs'),
    ('data_engineer',           'Data Engineer',           'specialized', 'Pipeline dati, ETL, data modeling'),

    -- GitHub integration (13)
    ('github_pr_manager',       'GitHub PR Manager',       'github', 'Gestione pull request, merge strategy'),
    ('github_code_reviewer',    'GitHub Code Reviewer',    'github', 'Review automatica PR, commenti inline'),
    ('github_issue_analyzer',   'GitHub Issue Analyzer',   'github', 'Analisi issue, triage, prioritization'),
    ('github_release_manager',  'GitHub Release Manager',  'github', 'Release notes, changelog, tag management'),
    ('github_ci_debugger',      'GitHub CI Debugger',      'github', 'Debug CI failures, fix workflow YAML'),
    ('github_dependency_bot',   'GitHub Dependency Bot',   'github', 'Dependency updates, security patches'),
    ('github_branch_manager',   'GitHub Branch Manager',   'github', 'Branch strategy, naming, protection rules'),
    ('github_metrics',          'GitHub Metrics',          'github', 'Repository stats, contributor analysis'),
    ('github_webhook_handler',  'GitHub Webhook Handler',  'github', 'Gestione webhook, event processing'),
    ('github_copilot_reviewer', 'GitHub Copilot Reviewer', 'github', 'AI-assisted code review con contesto GitHub'),
    ('github_issue_creator',    'GitHub Issue Creator',    'github', 'Crea issue automatiche da bug report'),
    ('github_project_manager',  'GitHub Project Manager',  'github', 'GitHub Projects v2, sprint planning'),
    ('github_actions_builder',  'GitHub Actions Builder',  'github', 'Crea e ottimizza workflow CI/CD'),

    -- Research/Ops (31)
    ('planner',             'Planner',              'specialized', 'Task decomposition, project planning, roadmap'),
    ('migrator',            'Migrator',             'specialized', 'Database migrations, schema evolution'),
    ('refactorer',          'Refactorer',           'specialized', 'Code refactoring, pattern application'),
    ('integrator',          'Integrator',           'specialized', 'System integration, API connectivity'),
    ('monitor',             'Monitor',              'ops',         'Monitoring, alerting, SLA tracking'),
    ('deployer',            'Deployer',             'ops',         'Deployment automation, rollout strategy'),
    ('scaler',              'Scaler',               'ops',         'Auto-scaling, capacity planning'),
    ('chaos_engineer',      'Chaos Engineer',       'ops',         'Failure injection, resilience testing'),
    ('ml_engineer',         'ML Engineer',          'specialized', 'Machine learning, model training, inference'),
    ('embedder_agent',      'Embedder Agent',       'specialized', 'Embedding generation, vector operations'),
    ('summarizer',          'Summarizer',           'research',    'Sintesi documenti, abstract generation'),
    ('translator',          'Translator',           'research',    'Code/doc translation, language conversion'),
    ('validator',           'Validator',            'specialized', 'Input validation, schema validation, contracts'),
    ('formatter',           'Formatter',            'specialized', 'Code formatting, linting, style enforcement'),
    ('profiler',            'Profiler',             'specialized', 'Performance profiling, bottleneck analysis'),
    ('tracer',              'Tracer',               'ops',         'Distributed tracing, observability'),
    ('logger',              'Logger',               'ops',         'Logging strategy, structured logging'),
    ('cacher',              'Cacher',               'specialized', 'Caching strategy, Redis, cache invalidation'),
    ('scheduler',           'Scheduler',            'ops',         'Job scheduling, cron, task queuing'),
    ('notifier',            'Notifier',             'ops',         'Notification system, webhooks, alerts'),
    ('auditor',             'Auditor',              'specialized', 'Security audit, compliance, GDPR'),
    ('cleaner',             'Cleaner',              'specialized', 'Dead code removal, cleanup, dependency pruning'),
    ('versioner',           'Versioner',            'ops',         'Version management, semver, compatibility'),
    ('tagger',              'Tagger',               'specialized', 'Metadata tagging, categorization, taxonomy'),
    ('indexer',             'Indexer',              'specialized', 'Search indexing, full-text search setup'),
    ('compressor',          'Compressor',           'specialized', 'Data compression, binary optimization'),
    ('encryptor',           'Encryptor',            'specialized', 'Encryption, key management, secrets'),
    ('authenticator',       'Authenticator',        'specialized', 'Auth flows, OAuth, JWT, session management'),
    ('load_balancer',       'Load Balancer',        'ops',         'Load balancing config, traffic distribution'),
    ('backup_manager',      'Backup Manager',       'ops',         'Backup strategy, disaster recovery'),
    ('cost_optimizer',      'Cost Optimizer',       'ops',         'Cloud cost analysis, resource optimization')
ON CONFLICT (agent_key) DO UPDATE
    SET display_name = EXCLUDED.display_name,
        description  = EXCLUDED.description,
        updated_at   = NOW();

-- Seed stats rows per ogni agent
INSERT INTO nexus_agent_stats (agent_key)
SELECT agent_key FROM nexus_agent_types
ON CONFLICT (agent_key) DO NOTHING;
