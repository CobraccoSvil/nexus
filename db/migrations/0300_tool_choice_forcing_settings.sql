-- Migrazione 0300: settings.agent.tool_choice_forcing_* (ADR 0018 leva 2)
--
-- Forcing del tool_choice nei turni d'azione iniziali: il modello NON puo'
-- chiudere il turno narrando senza eseguire (stop narrativo alla radice).
-- Decisione DB-driven (regola G del CLAUDE.md): i default nel codice Python
-- (brain/agents/nodes/helpers.py, _load_tool_choice_forcing_config) valgono
-- SOLO come fallback sicuro quando il DB e' irraggiungibile.
--
-- Contesto: ADR 0018 (docs/.nexus-vault/adr/0018-*). La funzione pura
-- should_force_tool_choice decide se forzare in base a:
--   - flag enabled (questa riga);
--   - tool disponibili nel turno;
--   - task action-oriented (richiesta d'azione dell'utente);
--   - iteration <= max_iteration (questa riga): dopo i primi turni il modello
--     resta libero di chiudere il task senza forzatura;
--   - non in fase di discovery M16 (gia' gestita altrove);
--   - provider/modello con uno style di tool_choice che supporta il forcing
--     (anthropic_any / openai_required / google_function_calling_any).
--
-- Se il forcing causa un errore provider (MALFORMED_FUNCTION_CALL o modello non
-- tool-capable), l'executor ritenta automaticamente UNA volta SENZA forcing per
-- quel turno, senza far fallire il run.
--
-- Cambiare il comportamento e' un UPDATE su queste righe + <=60s di refresh
-- cache, senza redeploy. Riusa la tabella settings esistente, prefisso 'agent.'.

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('agent.tool_choice_forcing_enabled', 'true', 'agent',
     'ADR 0018 leva 2: se true, l''executor forza tool_choice nei turni d''azione iniziali cosi'' il modello deve emettere una tool call invece di chiudere narrando. Disattivabile per debug; in produzione tenere true. Booleano.',
     false),
    ('agent.tool_choice_forcing_max_iteration', '2', 'agent',
     'ADR 0018 leva 2: iterazione massima (inclusa) entro cui il tool_choice forcing resta attivo. Oltre questa soglia il modello e'' libero di chiudere il task senza forzatura. Intero >= 0.',
     false)
ON CONFLICT (key) DO NOTHING;
