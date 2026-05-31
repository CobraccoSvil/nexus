-- 0231_command_hints_extra.sql
--
-- Pattern aggiuntivi a nexus_command_hints, imparati dal test E2E reale di
-- scaffolding app Beauty-Book da Figma (30-31/05/2026). Il modello in iterazione
-- incappava in errori di build risolvibili solo conoscendo la storia recente
-- dei pacchetti (rebrand, v6 vs v7, peer dep). Iniettando l'hint nel
-- tool_result di run_command il modello evita il loop di errori.
--
-- Idempotente: ON CONFLICT (pattern) DO NOTHING.

INSERT INTO nexus_command_hints (pattern, pattern_kind, hint_text, severity) VALUES
    -- React Router v6: createBrowserRouter NON e' in 'react-router', e' in 'react-router-dom'
    ('npm install react-router',
     'substring',
     'ATTENZIONE React Router: la v6 separa l''API. ''createBrowserRouter'' / ''RouterProvider'' / ''useNavigate'' / ''useLocation'' sono in ''react-router-dom'' (non in ''react-router''). Installa con: npm install react-router-dom@^6 e usa import { ... } from ''react-router-dom''. La v7 sta unificando ma molti progetti generati assumono v6.',
     'warning'),
    -- Errore tipico "does not provide export createBrowserRouter"
    ('createBrowserRouter',
     'substring',
     'NOTA: ''createBrowserRouter'' richiede ''react-router-dom'' (v6) o ''react-router'' (v7+). Se l''errore dice "does not provide an export named", il package installato e'' v6 e l''import deve venire da ''react-router-dom''.',
     'info'),
    -- npx shadcn add fallisce per peer dep o eccezione SSL
    ('npx shadcn',
     'substring',
     'NOTA: ''npx shadcn add'' richiede --legacy-peer-deps oppure pnpm/yarn con peer-deps disabilitate. ALTERNATIVA RAPIDA per non bloccarsi: usa il tool ''nexus_install_shadcn_components'' (DB-driven, crea stub funzionali in src/components/ui/ senza npm). I componenti stub coprono button, input, card, alert, tabs, label e bastano per buildare l''app.',
     'info'),
    -- Vite "Failed to resolve import" da path relativo che salta una sottocartella
    ('Failed to resolve import',
     'substring',
     'NOTA Vite import: ''Failed to resolve import "X" from "Y"'' significa che il path relativo e'' errato. Controlla con run_command(''ls <cartella-di-Y>'') se il file esiste davvero, e correggi il path nell''import. Pattern comune: import ''./Foo'' quando il file e'' in ''./sotto/Foo'' (importa ''./sotto/Foo'').',
     'info'),
    -- tailwindcss-animate missing
    ('tailwindcss-animate',
     'substring',
     'NOTA: ''tailwindcss-animate'' e'' richiesto dal preset shadcn ma NON viene installato automaticamente. Aggiungilo con: npm install tailwindcss-animate. Errore tipico: "[postcss] Cannot find module ''tailwindcss-animate''" da tailwind.config.js.',
     'info'),
    -- @babel/core missing per @vitejs/plugin-react
    ('@vitejs/plugin-react',
     'substring',
     'NOTA: ''@vitejs/plugin-react'' richiede ''@babel/core'' come peer dependency. Se l''errore e'' "Cannot find package ''@babel/core''" durante pre-transform Vite, installa con: npm install --save-dev @babel/core.',
     'info'),
    -- npm install fallisce per problem peer
    ('npm install',
     'substring',
     'NOTA: se ''npm install'' fallisce con ERESOLVE peer dep mismatch, considera npm install --legacy-peer-deps. Ma e'' un workaround: appena possibile aggiorna i pacchetti incompatibili invece di mascherare.',
     'info')
ON CONFLICT (pattern) DO NOTHING;
