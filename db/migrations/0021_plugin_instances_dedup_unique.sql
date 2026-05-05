-- Deduplica plugin instances (stesso catalog_item_id) e impone unicita' globale.
-- Regola richiesta: non possono esistere due plugin uguali.

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

CREATE UNIQUE INDEX IF NOT EXISTS ux_plugin_instances_catalog_item_unique
    ON plugin_instances (catalog_item_id);

