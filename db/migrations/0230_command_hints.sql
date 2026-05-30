-- 0230_command_hints.sql
--
-- Tabella nexus_command_hints: hint contestuali iniettati nel tool_result di
-- run_command quando il comando matcha un pattern noto (es. package deprecato,
-- comando rinominato, sintassi cambiata).
--
-- Caso d'uso: bug osservato 30/05/2026 — gemini-2.5-flash chiamato in loop
-- `npx shadcn-ui add` (deprecato dal 2025, ora `shadcn`). Senza intervento,
-- il modello esauriva il budget iter ritentando lo stesso comando fallito.
--
-- Soluzione radicale: il dispatcher run_command, prima di eseguire, controlla
-- se il command matcha un pattern in nexus_command_hints e, in caso, prefissa
-- il tool_result con l'hint correttivo. Cache lato Rust 60s.
--
-- Niente hardcode lato codice: nuovi pattern si aggiungono via INSERT in DB.
-- Disable rapido via UPDATE enabled=false. Compatibile con regola G (DB-only,
-- niente fallback hardcoded).

CREATE TABLE IF NOT EXISTS nexus_command_hints (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    pattern         TEXT NOT NULL UNIQUE,
    pattern_kind    TEXT NOT NULL DEFAULT 'substring' CHECK (pattern_kind IN ('substring', 'regex')),
    hint_text       TEXT NOT NULL,
    severity        TEXT NOT NULL DEFAULT 'warning' CHECK (severity IN ('info', 'warning', 'error')),
    enabled         BOOLEAN NOT NULL DEFAULT true,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT nexus_command_hints_pattern_not_empty CHECK (length(trim(pattern)) > 0),
    CONSTRAINT nexus_command_hints_hint_not_empty CHECK (length(trim(hint_text)) > 0)
);

CREATE INDEX IF NOT EXISTS nexus_command_hints_enabled_idx
    ON nexus_command_hints (enabled) WHERE enabled = true;

COMMENT ON TABLE nexus_command_hints IS
    'Hint correttivi iniettati nel tool_result di run_command quando il command matcha un pattern. Caricati con cache 60s.';
COMMENT ON COLUMN nexus_command_hints.pattern IS
    'Substring (case-insensitive) o regex. Match sul testo del command (esclusi argomenti shell).';
COMMENT ON COLUMN nexus_command_hints.hint_text IS
    'Testo che precede il tool_result. Es: "ATTENZIONE: npm install shadcn-ui rinominato a shadcn. Usa: npm install shadcn".';

-- Pre-popolamento con pattern noti (rebrand, deprecazioni, cambio sintassi).
INSERT INTO nexus_command_hints (pattern, pattern_kind, hint_text, severity) VALUES
    -- shadcn-ui -> shadcn (rebrand 2025)
    ('shadcn-ui',
     'substring',
     'ATTENZIONE: shadcn-ui e'' stato rinominato a shadcn nel 2025. Usa il comando con shadcn (es. `npx shadcn@latest add ...` invece di `npx shadcn-ui add ...`).',
     'warning'),
    -- Yarn 1 deprecato in Node 20+
    ('yarn install',
     'substring',
     'NOTA: se ricevi errore di engine o cwd su yarn install, valuta `corepack enable && corepack prepare yarn@stable --activate` oppure passa a npm/pnpm.',
     'info'),
    -- create-react-app deprecato
    ('create-react-app',
     'substring',
     'ATTENZIONE: create-react-app e'' stato deprecato (febbraio 2025). Usa Vite (`npm create vite@latest`) oppure Next.js per nuovi progetti React.',
     'warning'),
    -- npm install --force / --legacy-peer-deps abuso
    ('--legacy-peer-deps',
     'substring',
     'NOTA: --legacy-peer-deps maschera conflitti di peer dependency, non li risolve. Se compaiono errori a runtime dopo l''install, identifica il pacchetto incompatibile e aggiorna la versione invece di forzare.',
     'info'),
    -- Docker compose v1 syntax (docker-compose con trattino)
    ('docker-compose ',
     'substring',
     'NOTA: la sintassi `docker-compose` (con trattino) e'' la v1 deprecata. La v2 e'' `docker compose` (con spazio). Su sistemi recenti la v1 potrebbe non essere installata.',
     'info')
ON CONFLICT (pattern) DO NOTHING;
