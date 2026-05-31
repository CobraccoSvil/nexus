-- 0232_dev_diagnostics.sql
--
-- Tabella nexus_dev_diagnostics: pattern regex su output dev server (vite/next/
-- webpack/cargo) + categoria errore + fix template. Usata dal tool
-- nexus_dev_server_diagnose per trasformare un log di errore in una lista
-- di azioni concrete.
--
-- Differenza vs nexus_command_hints (mig 0230/0231): qui i pattern matchano
-- l'OUTPUT di un dev server gia' avviato (HMR, build, runtime), non l'INPUT
-- di un comando. Lo scopo e' rendere "auto-healing" il loop iterativo
-- "avvia → vede errore → fixa → riavvia" che il modello fa a mano.
--
-- Il fix_template e' una stringa interpretabile dal tool: puo' essere:
--   - 'shell:<comando>' → esegue run_command
--   - 'install_pkg:<nome>' → npm install <nome>
--   - 'install_pkg:{1}' → npm install con capture group regex
--   - 'sed:<glob>:<from>:<to>' → sostituisce in file matching glob
--   - 'rewrite_import:<from>:<to>' → riscrive import path nei sorgenti
--   - 'tool:<tool_name>:<args_json>' → invoca altro tool builtin
--   - 'create_file:<path>:<template_id>' → crea file da template noto
--
-- Tutte sostituzioni dei placeholder {1},{2},{file},{from},{module} avvengono
-- nel tool. Per ora il matching e' regex Rust standard.

CREATE TABLE IF NOT EXISTS nexus_dev_diagnostics (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    pattern_regex   TEXT NOT NULL UNIQUE,
    category        TEXT NOT NULL,
    fix_template    TEXT NOT NULL,
    severity        TEXT NOT NULL DEFAULT 'warning' CHECK (severity IN ('info', 'warning', 'error')),
    confidence      INTEGER NOT NULL DEFAULT 80 CHECK (confidence BETWEEN 0 AND 100),
    description     TEXT NOT NULL DEFAULT '',
    enabled         BOOLEAN NOT NULL DEFAULT true,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS nexus_dev_diagnostics_enabled_idx
    ON nexus_dev_diagnostics (enabled) WHERE enabled = true;

COMMENT ON TABLE nexus_dev_diagnostics IS
    'Pattern regex su output dev server -> fix template. Usata da nexus_dev_server_diagnose per auto-healing loop iterativo. Cache 60s.';
COMMENT ON COLUMN nexus_dev_diagnostics.fix_template IS
    'Formato: shell:<cmd> | install_pkg:<nome o {1}> | sed:<glob>:<from>:<to> | rewrite_import:<from>:<to> | tool:<name>:<json_args> | create_file:<path>:<template_id>';
COMMENT ON COLUMN nexus_dev_diagnostics.confidence IS
    'Confidence 0-100: 100 = applicare automaticamente, 50-80 = suggerire ma chiedere, <50 = ipotesi.';

-- Pre-popolamento con pattern noti dal test E2E Beauty-Book + altri comuni.
INSERT INTO nexus_dev_diagnostics (pattern_regex, category, fix_template, severity, confidence, description) VALUES
    -- Vite: pacchetto npm mancante (caso generico)
    ('Cannot find package ''([^'']+)'' imported from',
     'missing_pkg',
     'install_pkg:{1}',
     'error', 95,
     'Pacchetto NPM mancante in pre-transform. Auto-fix: npm install del modulo.'),
    -- Vite: peer dep @babel/core per @vitejs/plugin-react
    ('Cannot find package ''[^'']*@babel/core',
     'missing_pkg',
     'install_pkg:@babel/core',
     'error', 100,
     '@vitejs/plugin-react richiede @babel/core. Auto-fix sicuro.'),
    -- Vite: import relativo non risolto (modello deve correggere path)
    ('Failed to resolve import "([^"]+)" from "([^"]+)"',
     'broken_import',
     'tool:nexus_resolve_import:{"missing":"{1}","from":"{2}"}',
     'error', 70,
     'Import path errato. Tool risolutore cerca il file corretto e suggerisce il rewrite.'),
    -- React Router v6: createBrowserRouter non esportato da react-router
    ('does not provide an export named ''createBrowserRouter''',
     'rewrite_import',
     'rewrite_import:react-router:react-router-dom',
     'error', 100,
     'v6: createBrowserRouter sta in react-router-dom. Auto-fix safe + npm install react-router-dom@^6.'),
    -- React Router v6: useNavigate / useLocation idem
    ('does not provide an export named ''useNavigate''',
     'rewrite_import',
     'rewrite_import:react-router:react-router-dom',
     'error', 100,
     'v6: useNavigate sta in react-router-dom.'),
    -- postcss: tailwindcss-animate
    ('\[postcss\] Cannot find module ''([^'']+)''',
     'missing_pkg',
     'install_pkg:{1}',
     'error', 95,
     'Postcss plugin mancante. Auto-fix: npm install.'),
    -- index.html mancante (vite 404 sulla root con tutto OK in src/)
    ('http://[^/]+/ - 404',
     'missing_index_html',
     'create_file:index.html:vite_basic',
     'warning', 85,
     'Probabile mancanza di index.html nella root del progetto vite.'),
    -- Vite: errore generico HMR connection lost (probabile crash dev server)
    ('server connection lost',
     'server_crashed',
     'shell:tail -n 50 {log_path}',
     'info', 60,
     'Server vite ha probabilmente crashato. Leggere ultimo output.'),
    -- npm: ENOENT su node_modules (mai installato)
    ('Cannot find module ''([^'']+)''\s+Require stack:',
     'missing_pkg',
     'install_pkg:{1}',
     'error', 90,
     'Modulo Node mancante. Auto-fix se npm install non e'' stato eseguito.'),
    -- shadcn ui component mancante (file)
    ('Failed to resolve import "[^"]*components/ui/([a-z-]+)" from',
     'shadcn_missing',
     'tool:nexus_install_shadcn_components:{"components":["{1}"]}',
     'error', 95,
     'Componente shadcn UI non scaffolded. Auto-fix: crea stub con tool dedicato.'),
    -- sonner: import da path locale
    ('Failed to resolve import "[^"]*components/ui/sonner"',
     'rewrite_import',
     'rewrite_import:./components/ui/sonner:sonner',
     'error', 90,
     'Toaster va importato dal package sonner direttamente, non da components/ui.'),
    -- Vite ESM: type:module mancante in package.json
    ('Module type of file:.*\.js is not specified',
     'config_inconsistent',
     'shell:echo "Aggiungi \"type\": \"module\" al package.json"',
     'info', 50,
     'Warning ESM, non bloccante ma fastidioso nei log. Fix manuale.')
ON CONFLICT (pattern_regex) DO NOTHING;
