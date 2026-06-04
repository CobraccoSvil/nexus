-- ═════════════════════════════════════════════════════════════════════════
-- 0301 — Settings per il watcher bidirezionale vault->DB (ADR 0017 v2 TODO1).
--
-- Quando l'utente edita un file `.md` in `docs/.nexus-vault/` o in un
-- `<project_root>/.nexus-vault/`, il watcher osserva il filesystem e chiama
-- `wiki::reingest` per re-ingestionare il singolo file. Questo permette di
-- modificare i doc direttamente con Obsidian/altro editor e vedere i cambi
-- propagarsi in `wiki_docs` + Qdrant senza passare per la UI.
-- ═════════════════════════════════════════════════════════════════════════

-- Chiavi nuove:
--   - agent.wiki.watcher_enabled               (default 'true')
--   - agent.wiki.watcher_debounce_ms           (default '500', evita storm sui save multipli)
--   - agent.wiki.watcher_poll_interval_secs    (default '60', refresh lista progetti monitorati)

INSERT INTO settings (key, value)
VALUES
    ('agent.wiki.watcher_enabled', 'true'),
    ('agent.wiki.watcher_debounce_ms', '500'),
    ('agent.wiki.watcher_poll_interval_secs', '60')
ON CONFLICT (key) DO NOTHING;
