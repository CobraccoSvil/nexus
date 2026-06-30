-- Preferenza modello per sessione di chat.
-- Viene impostata automaticamente quando l'utente scrive comandi come "usa mistral",
-- "cambia a claude", ecc. e persiste per tutti i messaggi successivi della sessione.
ALTER TABLE chat_sessions
    ADD COLUMN IF NOT EXISTS preferred_provider TEXT,
    ADD COLUMN IF NOT EXISTS preferred_model     TEXT;
