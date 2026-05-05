-- Patterns per rilevare comandi long-running nell'agent loop.
-- Ogni pattern è una sequenza di token che, se trovata nel comando, causa
-- il routing automatico verso run_in_terminal (fire-and-forget).
-- Gestibile dall'admin UI senza ricompilare il backend.

CREATE TABLE IF NOT EXISTS long_running_patterns (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    pattern     TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    enabled     BOOLEAN NOT NULL DEFAULT TRUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Seed: patterns precedentemente hardcoded
INSERT INTO long_running_patterns (pattern, description) VALUES
    ('pnpm run dev',                'pnpm dev server'),
    ('npm run dev',                 'npm dev server'),
    ('yarn dev',                    'yarn dev server'),
    ('bun run dev',                 'bun dev server'),
    ('next dev',                    'Next.js dev server'),
    ('nuxt dev',                    'Nuxt dev server'),
    ('svelte-kit dev',              'SvelteKit dev server'),
    ('webpack --watch',             'webpack watch mode'),
    ('tsc --watch',                 'TypeScript watch mode'),
    ('tail -f',                     'tail follow mode'),
    ('docker compose up',           'Docker Compose'),
    ('npm start',                   'npm start'),
    ('pnpm start',                  'pnpm start'),
    ('cargo watch',                 'Cargo watch'),
    ('dotnet run',                  '.NET run'),
    ('dotnet watch',                '.NET watch'),
    ('flask run',                   'Flask server'),
    ('uvicorn',                     'Uvicorn ASGI server'),
    ('python -m flask',             'Flask via python -m'),
    ('python manage.py runserver',  'Django runserver'),
    ('go run',                      'Go run'),
    ('nodemon',                     'Nodemon watcher'),
    ('vite dev',                    'Vite dev server'),
    ('vite preview',                'Vite preview server'),
    ('vite',                        'Vite (bare command)')
ON CONFLICT (pattern) DO NOTHING;
