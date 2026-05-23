-- Migrazione: registra il nome del vault Obsidian per il meta-vault Nexus e per
-- ogni progetto utente, abilitando i deep-link `obsidian://open?vault=...`.
--
-- Il nome del vault non e' deducibile programmaticamente da Nexus (e' assegnato
-- dall'utente quando aggiunge la cartella in Obsidian). Lo configura l'utente in UI.

-- 1. Meta-vault Nexus (singleton, in settings)
INSERT INTO settings (key, value, category, description) VALUES
    ('meta_docs.obsidian_vault_name', '', 'meta_docs',
     'Nome del vault Obsidian registrato per docs/.nexus-vault/ (vuoto = non configurato)')
ON CONFLICT (key) DO NOTHING;

-- 2. Per-progetto: colonna sulla tabella projects
ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS obsidian_vault_name TEXT NOT NULL DEFAULT '';

COMMENT ON COLUMN projects.obsidian_vault_name IS
    'Nome del vault Obsidian registrato dall''utente per il Knowledge vault del progetto (.nexus/knowledge/). Stringa vuota = non configurato.';
