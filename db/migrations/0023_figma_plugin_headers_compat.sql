-- Figma plugin compatibility:
-- aggiunge binding header X-Figma-Token + X-Figma-Region
-- anche per installazioni esistenti create prima del fix.

UPDATE plugin_instances pi
SET
    secret_bindings = jsonb_set(
        jsonb_set(
            jsonb_set(
                COALESCE(pi.secret_bindings, '{}'::jsonb),
                '{headers,Authorization}',
                '"figma_oauth_token"'::jsonb,
                TRUE
            ),
            '{headers,X-Figma-Token}',
            '"figma_oauth_token"'::jsonb,
            TRUE
        ),
        '{headers,X-Figma-Region}',
        '"figma_region"'::jsonb,
        TRUE
    ),
    updated_at = NOW()
FROM plugin_catalog_items c
WHERE c.id = pi.catalog_item_id
  AND c.slug = 'figma-http';
