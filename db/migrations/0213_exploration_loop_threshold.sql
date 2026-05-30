-- Migrazione 0213: settings.agent.exploration_loop_threshold
--
-- Soglia DB-driven per la loop-detection SEMANTICA dell'executor (Python:
-- brain/agents/nodes.py, _load_exploration_loop_threshold).
--
-- Contesto: quando l'utente carica un allegato (es. Figma .make / ZIP) e chiede
-- "crea l'applicazione descritta nel file", il modello puo' incatenare molte
-- chiamate a tool di SOLA esplorazione (nexus_list_archive_entries,
-- nexus_read_archive_entry, nexus_inspect_attachment, nexus_extract_figma_structure,
-- ...) variando entry/offset. La loop-detection per signature identica non
-- scatta (input sempre diversi) e si arriva a 50+ iterazioni senza scrivere.
--
-- Questa soglia governa la loop-detection sulla FAMIGLIA di tool esplorativi:
--   - >= soglia          -> nudge forte verso write_file/request_port
--   - >= 2x soglia        -> abort (il modello ha ignorato il nudge)
--
-- Niente hardcode lato codice (regola G del CLAUDE.md): il default 6 nel codice
-- e' solo il fallback sicuro quando il DB e' irraggiungibile. Cambiare il
-- comportamento e' un UPDATE su questa riga + <=60s di refresh cache.
-- Riusa la tabella settings esistente con prefisso 'agent.'.

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('agent.exploration_loop_threshold', '6', 'agent',
     'Numero di chiamate consecutive a tool di sola esplorazione (lettura/ispezione allegati e file) oltre il quale l''executor inietta un nudge verso la scrittura; a 2x la soglia abortisce. Una call produttiva (write_file, edit_file, run_command, request_port, ...) azzera il contatore. Intero >= 1.',
     false)
ON CONFLICT (key) DO NOTHING;
