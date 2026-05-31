-- 0233_db_query_hints.sql
--
-- Hint per redirigere l'agente dai comandi psql/mysql diretti (spesso non
-- installati nel WSL host) verso i nuovi tool builtin nexus_db_query /
-- nexus_db_tables / nexus_db_describe, che operano sul DB applicativo del
-- progetto risolvendo la connessione da project_database_config.
--
-- Caso scatenante (31/05/2026): "inserisci un utente con email X" -> il modello
-- prova `psql` -> command not found -> stallo. Con questi hint, il tool_result
-- di run_command suggerisce subito l'alternativa corretta.
--
-- Idempotente: ON CONFLICT (pattern) DO NOTHING.

INSERT INTO nexus_command_hints (pattern, pattern_kind, hint_text, severity) VALUES
    ('psql',
     'substring',
     'ATTENZIONE: NON usare psql per il DB del progetto (spesso non installato nel host). Usa i tool builtin: nexus_db_query (per SELECT/INSERT/UPDATE/DELETE/CREATE TABLE), nexus_db_tables (lista tabelle), nexus_db_describe (schema tabella). La connessione al DB applicativo dedicato e'' risolta automaticamente. Esempio: nexus_db_query({sql: "INSERT INTO users(email) VALUES ($1) RETURNING id", params: ["x@y.it"]}).',
     'warning'),
    ('mysql -',
     'substring',
     'NOTA: per il DB del progetto usa nexus_db_query invece di mysql CLI. Risolve la connessione dal config del progetto. (Se il progetto usa MySQL e non Postgres, segnalalo: i tool nexus_db_* assumono Postgres.)',
     'info'),
    ('mongosh',
     'substring',
     'NOTA: i tool nexus_db_* gestiscono Postgres. Per MongoDB usa il driver applicativo del progetto o uno script Node/Python, non la shell diretta.',
     'info')
ON CONFLICT (pattern) DO NOTHING;

-- Aggiungi anche un pattern dev_diagnostics: "command not found: psql" nel
-- log di un run_command -> suggerisce il tool builtin.
INSERT INTO nexus_dev_diagnostics (pattern_regex, category, fix_template, severity, confidence, description) VALUES
    ('psql: (command not found|not found|No such file)',
     'wrong_db_tool',
     'tool:nexus_db_tables:{}',
     'warning', 90,
     'psql non installato. Usa i tool builtin nexus_db_query / nexus_db_tables / nexus_db_describe per operare sul DB del progetto.')
ON CONFLICT (pattern_regex) DO NOTHING;
