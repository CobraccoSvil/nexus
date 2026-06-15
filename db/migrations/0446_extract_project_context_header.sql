-- 0446: A3 — estrazione delle frasi-DIRETTIVA dell'header contesto progetto e
-- del blocco DB (agent_run.rs). I DATI (nome, root, git, summary, elenco DB)
-- restano interpolati nel codice via {{...}}; nel DB vanno solo le frasi fisse.
-- Regola G/D. Il codice aggiunge i newline di bordo (\n iniziale del db-block,
-- \n\n finale dell'header), quindi qui i template NON li includono.
--
-- Idempotente: ON CONFLICT (key) DO NOTHING.

INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES
(
    'system.db_connections_directive',
    'system',
    'Intestazione blocco DB configurati (direttiva)',
    $dbc$Database configurati (usa questi per connetterti, NON chiedere credenziali all'utente):$dbc$,
    'migration_0446'
),
(
    'system.project_context_header_with_summary',
    'system',
    'Header contesto progetto (con analisi)',
    $hdr$=== CONTESTO PROGETTO (non chiedere queste informazioni: sono gia' qui) ===
Progetto: {{project_name}} | Root: {{project_root}} | Git: {{is_git_repo}}
{{analysis_summary}}{{db_connections_block}}
=== FINE CONTESTO PROGETTO ===$hdr$,
    'migration_0446'
),
(
    'system.project_context_header_no_summary',
    'system',
    'Header contesto progetto (senza analisi)',
    $hdr$=== CONTESTO PROGETTO ===
Progetto: {{project_name}} | Root: {{project_root}} | Git: {{is_git_repo}}{{db_connections_block}}
(Nessuna analisi disponibile: usa list_files per esplorare la struttura)
=== FINE CONTESTO PROGETTO ===$hdr$,
    'migration_0446'
)
ON CONFLICT (key) DO NOTHING;
