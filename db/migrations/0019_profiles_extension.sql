-- User-owned profiles (GPT/Gem style).
-- chat_profiles e' mantenuta intatta (orchestrator-scoped, referenziata da FK).

CREATE TABLE IF NOT EXISTS user_profiles (
  id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id             UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name                TEXT        NOT NULL,
  description         TEXT,
  avatar_emoji        TEXT        NOT NULL DEFAULT '🤖',
  system_prompt       TEXT        NOT NULL DEFAULT '',
  default_provider    TEXT,
  default_model       TEXT,
  default_automation  TEXT,
  is_default          BOOLEAN     NOT NULL DEFAULT FALSE,
  created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  UNIQUE(user_id, name)
);

CREATE UNIQUE INDEX IF NOT EXISTS user_profiles_one_default_per_user
  ON user_profiles (user_id)
  WHERE is_default = TRUE;

CREATE INDEX IF NOT EXISTS idx_user_profiles_user_id ON user_profiles(user_id);
