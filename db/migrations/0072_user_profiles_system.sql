-- Fase 2: unificazione profili utente + template profilo di sistema.
-- I profili di sistema (is_system=true) sono visibili a tutti, non eliminabili,
-- ma "forkabili" in una copia personale dall'utente.

-- 1. Rendi user_id nullable (i profili di sistema non appartengono a nessun utente)
ALTER TABLE user_profiles
    ALTER COLUMN user_id DROP NOT NULL;

-- 2. Aggiungi colonne per distinguere profili di sistema
ALTER TABLE user_profiles
    ADD COLUMN IF NOT EXISTS is_system       BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS source_template_key TEXT;

-- 3. Backfill: inserisce i template categoria 'profile' come profili di sistema
--    (solo se non già presenti, per idempotenza)
INSERT INTO user_profiles (user_id, name, description, avatar_emoji, system_prompt, is_system, source_template_key)
SELECT
    NULL,
    t.title,
    NULL,
    CASE t.key
        WHEN 'profile.developer_csharp_dotnet'    THEN '💙'
        WHEN 'profile.developer_react_typescript' THEN '⚛️'
        WHEN 'profile.devops_infrastructure'      THEN '🐳'
        WHEN 'profile.developer_python'           THEN '🐍'
        WHEN 'profile.developer_rust'             THEN '🦀'
        WHEN 'profile.developer_vue_nuxt'         THEN '💚'
        WHEN 'profile.data_science_ml'            THEN '📊'
        WHEN 'profile.developer_mobile'           THEN '📱'
        ELSE '🤖'
    END,
    t.content,
    TRUE,
    t.key
FROM nexus_prompt_templates t
WHERE t.category = 'profile'
  AND NOT EXISTS (
      SELECT 1 FROM user_profiles up
      WHERE up.source_template_key = t.key AND up.is_system = TRUE
  );

-- 4. Indice per ricercare i profili di sistema rapidamente
CREATE INDEX IF NOT EXISTS idx_user_profiles_is_system ON user_profiles(is_system);
