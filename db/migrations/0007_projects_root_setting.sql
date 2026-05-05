INSERT INTO settings (key, value, category, description, is_secret)
VALUES (
    'projects_base_root',
    '',
    'infrastructure',
    'Root assoluta sotto cui e'' consentita la registrazione/navigazione dei progetti',
    FALSE
)
ON CONFLICT (key) DO NOTHING;
