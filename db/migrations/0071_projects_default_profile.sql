-- Profilo AI di default per progetto.
-- Quando impostato, la chat lo pre-seleziona automaticamente all'apertura del progetto.
ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS default_profile_id UUID
        REFERENCES user_profiles(id) ON DELETE SET NULL;
