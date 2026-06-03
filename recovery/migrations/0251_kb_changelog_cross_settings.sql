-- 0251_kb_changelog_cross_settings.sql
--
-- M12.4 — Cross-link changelog del meta-vault Nexus verso la KB.
--
-- Quando un changelog del meta-vault Nexus viene applicato (apply_generated_doc
-- con was_updated=true e kind che contiene 'changelog'), viene creata/aggiornata
-- una nota project_knowledge_notes (kind='nexus_changelog_cross',
-- source_kind='external', external_source_id=doc_id) SOLO nel progetto Nexus
-- (il meta-progetto, root_path = repo Nexus), se registrato.
--
-- Regola E (isolamento progetti): i file toccati da un changelog Nexus
-- appartengono solo al meta-progetto. Niente note nei progetti utente. Se la
-- repo Nexus non e' registrata come progetto, la feature e' un no-op silenzioso.
--
-- Regola G: il gate vive qui in settings, niente fallback hardcoded del
-- comportamento. Cache 60s lato Rust (coerente con kb.ingest.enabled).
--
-- Niente nuova colonna/tabella: external_source_id e source_kind esistono gia'
-- (mig 0227_knowledge_graph_import.sql).

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('kb.changelog_cross_enabled', 'true', 'kb',
     'Abilita il cross-link dei changelog del meta-vault Nexus nella KB del meta-progetto Nexus (M12.4). No-op se Nexus non e'' registrato come progetto.',
     FALSE)
ON CONFLICT (key) DO NOTHING;
