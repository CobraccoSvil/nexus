-- 0165_nexus_resource_quotas.sql
-- Quote risorse per progetto: porte allocabili, RAM, disco, container, pool DB.
--
-- Riga sentinella project_id = '00000000-0000-0000-0000-000000000000' contiene
-- i default globali letti quando un progetto non ha override esplicito.
--
-- Le quote sono enforcement HARD: i tool agente rifiutano l'allocazione quando
-- il conteggio raggiunge il limite (crates/mcp-core/src/security/quotas.rs).

CREATE TABLE IF NOT EXISTS nexus_resource_quotas (
    project_id UUID PRIMARY KEY,
    max_ports INT NOT NULL DEFAULT 20
        CHECK (max_ports > 0 AND max_ports <= 50),       -- bucket fisico = 50
    max_memory_mb INT NOT NULL DEFAULT 4096
        CHECK (max_memory_mb >= 256),
    max_disk_mb INT NOT NULL DEFAULT 10240
        CHECK (max_disk_mb >= 512),
    max_containers INT NOT NULL DEFAULT 5
        CHECK (max_containers >= 0),
    max_db_pool_size INT NOT NULL DEFAULT 10
        CHECK (max_db_pool_size > 0 AND max_db_pool_size <= 100),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Foreign key opzionale: la riga sentinella non punta a un progetto reale.
-- I record per-progetto reali sono garantiti da CASCADE manuale all'inserimento.
-- (project_id reale referenzia projects(id) ma la riga sentinella e' eccezione)

-- Default globali (sentinella). Letta quando un progetto non ha riga propria.
INSERT INTO nexus_resource_quotas (project_id)
VALUES ('00000000-0000-0000-0000-000000000000')
ON CONFLICT (project_id) DO NOTHING;

COMMENT ON TABLE nexus_resource_quotas IS
    'Quote per-progetto su porte/RAM/disk/container/pool DB. Riga 0000... = default globali.';
COMMENT ON COLUMN nexus_resource_quotas.max_ports IS
    'Massimo porte allocate simultaneamente. Il bucket fisico per progetto e'' 50 porte (services.rs::PROJECT_PORT_BUCKET_SIZE).';
COMMENT ON COLUMN nexus_resource_quotas.max_memory_mb IS
    'Somma RAM container del progetto (Docker cgroup memory).';
COMMENT ON COLUMN nexus_resource_quotas.max_disk_mb IS
    'Disco massimo in project_root (enforcement via du -s ogni 60s).';
COMMENT ON COLUMN nexus_resource_quotas.max_containers IS
    'Container Docker simultanei creati dai tool agente.';
COMMENT ON COLUMN nexus_resource_quotas.max_db_pool_size IS
    'Pool max_connections aperto da project_db_query verso il DB del progetto.';
