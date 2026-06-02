-- 0252_code_doc_wiki.sql
-- Fase W2 code-wiki: documentazione AI per-file (note kind='code_doc').
--
-- Settings del generatore + modello dedicato in nexus_purpose_model (purpose
-- 'code_doc'): il codice NON hardcoda il modello (regola G), lo legge da qui.
-- Default google/gemini-2.5-flash: economico, capace, adatto a documentare.
-- Modificabile da admin senza redeploy. Idempotente.

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('kb.code_doc.enabled', 'true', 'kb',
     'W2: abilita la generazione della code-wiki (note code_doc per file).', 'f'),
    ('kb.code_doc.max_files', '50', 'kb',
     'Numero massimo di file documentati per esecuzione della code-wiki.', 'f'),
    ('kb.code_doc.max_source_chars', '12000', 'kb',
     'Caratteri di sorgente inviati all''LLM per file (troncamento).', 'f'),
    ('kb.code_doc.max_file_bytes', '200000', 'kb',
     'Dimensione massima file (byte) considerato dalla code-wiki.', 'f')
ON CONFLICT (key) DO NOTHING;

INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes) VALUES
    ('code_doc', 'google', 'gemini-2.5-flash',
     'W2 code-wiki: documentazione AI per-file. Modello economico e capace.')
ON CONFLICT (purpose) DO NOTHING;
