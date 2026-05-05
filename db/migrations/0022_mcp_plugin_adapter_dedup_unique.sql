-- Hardening dedup plugin manager:
-- 1) dedup plugin_instances per catalog_item_id
-- 2) dedup mcp_servers per plugin_instance_id
-- 3) enforce unique constraints to avoid future duplicates

WITH ranked AS (
    SELECT
        id,
        catalog_item_id,
        ROW_NUMBER() OVER (
            PARTITION BY catalog_item_id
            ORDER BY created_at ASC, id ASC
        ) AS rn,
        FIRST_VALUE(id) OVER (
            PARTITION BY catalog_item_id
            ORDER BY created_at ASC, id ASC
        ) AS keeper_id
    FROM plugin_instances
),
dups AS (
    SELECT id AS duplicate_id, keeper_id
    FROM ranked
    WHERE rn > 1
)
UPDATE mcp_servers m
SET plugin_instance_id = d.keeper_id,
    updated_at = NOW()
FROM dups d
WHERE m.plugin_instance_id = d.duplicate_id;

WITH ranked AS (
    SELECT
        id,
        catalog_item_id,
        ROW_NUMBER() OVER (
            PARTITION BY catalog_item_id
            ORDER BY created_at ASC, id ASC
        ) AS rn,
        FIRST_VALUE(id) OVER (
            PARTITION BY catalog_item_id
            ORDER BY created_at ASC, id ASC
        ) AS keeper_id
    FROM plugin_instances
),
dups AS (
    SELECT id AS duplicate_id, keeper_id
    FROM ranked
    WHERE rn > 1
)
UPDATE plugin_instance_tool_policies p
SET plugin_instance_id = d.keeper_id,
    updated_at = NOW()
FROM dups d
WHERE p.plugin_instance_id = d.duplicate_id
  AND NOT EXISTS (
      SELECT 1
      FROM plugin_instance_tool_policies keep
      WHERE keep.plugin_instance_id = d.keeper_id
  );

WITH ranked AS (
    SELECT
        id,
        catalog_item_id,
        ROW_NUMBER() OVER (
            PARTITION BY catalog_item_id
            ORDER BY created_at ASC, id ASC
        ) AS rn
    FROM plugin_instances
),
dups AS (
    SELECT id AS duplicate_id
    FROM ranked
    WHERE rn > 1
)
DELETE FROM plugin_instance_tool_policies p
USING dups d
WHERE p.plugin_instance_id = d.duplicate_id;

WITH ranked AS (
    SELECT
        id,
        catalog_item_id,
        ROW_NUMBER() OVER (
            PARTITION BY catalog_item_id
            ORDER BY created_at ASC, id ASC
        ) AS rn,
        FIRST_VALUE(id) OVER (
            PARTITION BY catalog_item_id
            ORDER BY created_at ASC, id ASC
        ) AS keeper_id
    FROM plugin_instances
),
dups AS (
    SELECT id AS duplicate_id, keeper_id
    FROM ranked
    WHERE rn > 1
)
UPDATE plugin_instance_health_runs h
SET plugin_instance_id = d.keeper_id
FROM dups d
WHERE h.plugin_instance_id = d.duplicate_id;

WITH ranked AS (
    SELECT
        id,
        catalog_item_id,
        ROW_NUMBER() OVER (
            PARTITION BY catalog_item_id
            ORDER BY created_at ASC, id ASC
        ) AS rn,
        FIRST_VALUE(id) OVER (
            PARTITION BY catalog_item_id
            ORDER BY created_at ASC, id ASC
        ) AS keeper_id
    FROM plugin_instances
),
dups AS (
    SELECT id AS duplicate_id, keeper_id
    FROM ranked
    WHERE rn > 1
)
UPDATE plugin_audit_events a
SET plugin_instance_id = d.keeper_id
FROM dups d
WHERE a.plugin_instance_id = d.duplicate_id;

WITH ranked AS (
    SELECT
        id,
        catalog_item_id,
        ROW_NUMBER() OVER (
            PARTITION BY catalog_item_id
            ORDER BY created_at ASC, id ASC
        ) AS rn
    FROM plugin_instances
),
dups AS (
    SELECT id AS duplicate_id
    FROM ranked
    WHERE rn > 1
)
DELETE FROM plugin_instances p
USING dups d
WHERE p.id = d.duplicate_id;

-- Dedup adapter MCP collegati alla stessa istanza plugin:
-- mantieni il più recente, rimuovi gli altri.
WITH ranked AS (
    SELECT
        id,
        plugin_instance_id,
        ROW_NUMBER() OVER (
            PARTITION BY plugin_instance_id
            ORDER BY updated_at DESC NULLS LAST, created_at DESC NULLS LAST, id DESC
        ) AS rn
    FROM mcp_servers
    WHERE plugin_instance_id IS NOT NULL
),
dups AS (
    SELECT id
    FROM ranked
    WHERE rn > 1
)
DELETE FROM mcp_servers m
USING dups d
WHERE m.id = d.id;

CREATE UNIQUE INDEX IF NOT EXISTS ux_plugin_instances_catalog_item_unique
    ON plugin_instances (catalog_item_id);

CREATE UNIQUE INDEX IF NOT EXISTS ux_mcp_servers_plugin_instance_unique
    ON mcp_servers (plugin_instance_id)
    WHERE plugin_instance_id IS NOT NULL;
