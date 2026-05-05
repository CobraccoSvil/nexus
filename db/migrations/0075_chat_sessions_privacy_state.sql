-- Traccia quando il gateway ha re-instradato automaticamente su provider locale per privacy.
-- Usato per azzerare la preferenza di sessione (preferred_provider) dopo un evento privacy:
-- quando l'utente riprende con contenuto non sensibile, il sistema torna al routing automatico
-- invece di riprendere il provider che era stato impostato manualmente.
ALTER TABLE chat_sessions
    ADD COLUMN IF NOT EXISTS privacy_rerouted_at TIMESTAMPTZ;
