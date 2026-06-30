-- 0495_seed_project_separation_flag.sql
-- Fase 2+ separazione DB: feature flag globale che governa il punto unico
-- project_data_pool. Se 'true', i dati per-progetto dei domini gia' migrati
-- (chat, ...) vengono letti/scritti nel DB metadati del progetto (<slug>_nexus)
-- invece del meta-DB centrale. Default 'false' = comportamento storico.
-- Si abilita SOLO dopo che TUTTI i call-site di un dominio sono instradati su
-- project_data_pool (altrimenti split-brain tra i due DB).
INSERT INTO settings (key, value, category, description)
VALUES (
    'db.project_separation.enabled',
    'false',
    'database',
    'Fase 2+ separazione DB: se true i dati per-progetto dei domini migrati vengono letti/scritti nel DB metadati del progetto (<slug>_nexus) invece del meta-DB centrale. Default false. Abilitare solo a conversione completa di un dominio.'
)
ON CONFLICT (key) DO NOTHING;
