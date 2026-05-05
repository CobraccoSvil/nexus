-- Migration 0031: Google Gemini Batch API settings
INSERT INTO settings (key, value, category, description, is_secret) VALUES
  ('google_batch_api_enabled', 'false', 'providers', 'Abilita Google Gemini Batch API per analisi approfondita (50% costo)', false),
  ('google_batch_model', 'gemini-2.5-flash', 'providers', 'Modello Gemini per batch job', false),
  ('google_batch_threshold', '5', 'providers', 'Numero minimo di file per usare Batch API (altrimenti chiamate sincrone)', false)
ON CONFLICT (key) DO NOTHING;
