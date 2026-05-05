-- Assicura che nexus_active_routing_pct esista con category='routing'
-- così il filtro routingItems nel frontend la include automaticamente.
-- Il bulk_update la inseriva con category='custom' (default fallback),
-- il che causava la scomparsa del valore dopo il refresh dei settings.
INSERT INTO settings (key, value, category, description, is_secret, updated_at)
VALUES (
    'nexus_active_routing_pct',
    '0',
    'routing',
    'Percentuale di richieste chat gestite dal router Q-Learning Nexus (0=off, 100=tutto). A/B testing: imposta 10-50 per un rollout graduale.',
    FALSE,
    NOW()
)
ON CONFLICT (key) DO UPDATE
    SET category    = 'routing',
        description = EXCLUDED.description,
        -- Aggiorna category ma NON sovrascrive il valore se già impostato
        updated_at  = NOW()
WHERE settings.category != 'routing';
