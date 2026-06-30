INSERT INTO settings (key, value, category, description, is_secret)
VALUES
  (
    'qdrant_project_context_collection',
    'project_context',
    'infrastructure',
    'Qdrant collection per indicizzazione iniziale del contesto/storia progetto',
    FALSE
  )
ON CONFLICT (key) DO NOTHING;
