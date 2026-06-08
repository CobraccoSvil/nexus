-- 0366_task_playbooks.sql
--
-- Task-Playbook Engine: conoscenza di dominio riusabile, attivata dal contesto.
--
-- Problema: per task ricorrenti che richiedono know-how non ovvio (es.
-- "implementa l'app dal file Figma"), l'utente dovrebbe scrivere un prompt lungo
-- e dettagliato che descrive COME procedere. Quel sapere deve vivere in Nexus,
-- non nel prompt utente. Un playbook e' un testo-guida (passi/contesto/anti-
-- pattern) iniettato nel prompt operativo dell'agente quando il contesto del
-- turno corrisponde al suo trigger. Generico: nuovi tipi di task = nuove righe,
-- non nuovo codice (regola G: config nel DB; regola L: un solo punto inietta).
--
-- Schema coerente con nexus_prompt_templates (mig 0035) e nexus_shared_directives
-- (mig 0135): history + is_active/enabled + priority + version. Idempotente.

BEGIN;

CREATE TABLE IF NOT EXISTS nexus_task_playbooks (
    id            SERIAL PRIMARY KEY,
    key           TEXT NOT NULL UNIQUE,
    title         TEXT NOT NULL,
    description   TEXT NOT NULL DEFAULT '',
    -- Assi di match (tutti opzionali): un asse assente = non vincola.
    --   { "intent": ["implement","fix"],            -- intent ammessi (OR)
    --     "keywords": ["figma",".make"],            -- almeno una nel testo utente
    --     "attachment_kind": "figma_make",          -- kind allegato presente
    --     "project_markers": ["figma_export"] }     -- marcatore nella root progetto
    trigger_json  JSONB NOT NULL DEFAULT '{}'::jsonb,
    guidance_text TEXT NOT NULL,
    category      TEXT NOT NULL DEFAULT 'general',
    priority      INT  NOT NULL DEFAULT 100,
    enabled       BOOLEAN NOT NULL DEFAULT TRUE,
    version       INT  NOT NULL DEFAULT 1,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_by    TEXT NOT NULL DEFAULT 'system'
);

CREATE TABLE IF NOT EXISTS nexus_task_playbook_history (
    id            SERIAL PRIMARY KEY,
    playbook_id   INTEGER NOT NULL REFERENCES nexus_task_playbooks(id) ON DELETE CASCADE,
    guidance_text TEXT NOT NULL,
    trigger_json  JSONB NOT NULL,
    version       INT  NOT NULL,
    changed_by    TEXT NOT NULL DEFAULT 'system',
    changed_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    change_note   TEXT
);

CREATE INDEX IF NOT EXISTS idx_task_playbooks_enabled
    ON nexus_task_playbooks(enabled) WHERE enabled = TRUE;
CREATE INDEX IF NOT EXISTS idx_task_playbook_history_pid
    ON nexus_task_playbook_history(playbook_id);

-- Kill switch globale (regola G: niente fallback hardcoded nel codice).
INSERT INTO settings (key, value, category, description) VALUES
    ('orchestrator.task_playbook.enabled', 'true', 'orchestrator',
     'Abilita l''iniezione dei task playbook (nexus_task_playbooks) nel prompt operativo dell''agente.')
ON CONFLICT (key) DO NOTHING;

-- Primo playbook: implementazione da Figma Make (.make). Generale, NON legato a
-- uno specifico progetto. E' il "sapere" che altrimenti l'utente dovrebbe scrivere.
INSERT INTO nexus_task_playbooks (key, title, description, trigger_json, guidance_text, category, priority, enabled)
VALUES (
    'implement.figma_make',
    'Implementazione app da Figma Make (.make)',
    'Guida per realizzare l''app a partire da un allegato Figma Make: estrazione codice + ricostruzione ambiente shadcn/Tailwind.',
    '{"keywords": ["figma", ".make", "figma make", "realizza l''app", "realizza il sito", "implementa l''app", "implementa il sito", "realizza l''applicazione"], "attachment_kind": "figma_make", "project_markers": ["figma_export"]}'::jsonb,
    E'# Implementazione da Figma Make (.make)\n\nUn allegato Figma Make (.make) e'' uno ZIP che contiene il codice React/TypeScript\ndell''app dentro `ai_chat.json` (NON in `canvas.fig`, che e'' binario opaco non\nparsabile). Nexus sa estrarlo.\n\nProcedi cosi'', iterando finche'' compila e il sito si vede nel browser:\n\n1. Estrai il code-snapshot con il tool `nexus_extract_figma_code` (passando\n   l''`attachment_id` del .make) nella cartella `figma_export/`. Per la sola\n   specifica/intento del design usa invece `nexus_extract_figma_structure`.\n2. ATTENZIONE: i .make di norma esportano SOLO il codice business (App, routes,\n   pages, componenti di dominio) ma NON i componenti shadcn/ui ne il setup\n   Tailwind: sono boilerplate del template Figma, solo IMPORTATI dai file. Controlla\n   gli import `./components/ui/*` e `@/components/ui/*` e ricostruisci cio'' che manca.\n3. Migra il codice estratto nel frontend del progetto (tipicamente `frontend/src`),\n   sostituendo eventuali stub placeholder, e adatta entry (`main.tsx` + `index.html`)\n   e rotte per montare l''app con la sua pagina iniziale.\n4. Aggiungi le dipendenze realmente importate e installale. Tipiche per un .make:\n   `react-router` (NON react-router-dom), `lucide-react`, `recharts`, `sonner`,\n   `class-variance-authority`, `clsx`, `tailwind-merge`, `tailwindcss`, e i\n   `@radix-ui/*` richiesti dai componenti shadcn. Se il progetto gira in un\n   container, installa dentro il container.\n5. Configura Tailwind (tailwind.config + postcss.config + globals.css con le\n   direttive @tailwind) se assente, e importa il css nell''entry.\n6. Avvia/riavvia il dev server e VERIFICA nel browser che la pagina iniziale del\n   design sia renderizzata davvero (non una pagina bianca), correggendo gli import\n   mancanti finche'' compila.',
    'implementation',
    100,
    TRUE
)
ON CONFLICT (key) DO NOTHING;

COMMIT;
