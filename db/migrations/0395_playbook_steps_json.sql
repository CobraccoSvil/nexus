-- 0395_playbook_steps_json.sql
--
-- Passi STRUTTURATI per i task playbook -> todos deterministici dal planner.
--
-- Causa radice (incidente Beauty-Book 2026-06-11): il playbook
-- implement.figma_make ha matchato in TUTTI i run del task figma, ma il planner
-- veniva saltato perche' il modello non emetteva nexus_todo_write
-- (planner_node: "il modello non ha emesso nexus_todo_write — skip") -> niente
-- DoD, niente verifier, nessuna decomposizione: deepseek collassava in
-- esplorazione, mistral faceva 2 azioni laterali e dichiarava vittoria.
--
-- Fix: i playbook multi-step espongono i passi come ARRAY JSON strutturato
-- (steps_json). Quando un playbook con passi matcha e il modello non emette il
-- piano, il planner genera i todos DETERMINISTICAMENTE dai passi (niente
-- speranza che l'LLM li trascriva). Il guidance_text resta la guida discorsiva
-- nel system prompt; steps_json e' il contratto operativo.
--
-- Idempotente.

ALTER TABLE nexus_task_playbooks
    ADD COLUMN IF NOT EXISTS steps_json JSONB NOT NULL DEFAULT '[]'::jsonb;

COMMENT ON COLUMN nexus_task_playbooks.steps_json IS
'Passi operativi strutturati (array di stringhe). Se non vuoto e il modello planner non emette nexus_todo_write, il planner genera i todos da questi passi (deterministico). Mig 0395.';

UPDATE nexus_task_playbooks
SET steps_json = '[
  "Estrai il codice dal file .make con nexus_extract_figma_code (target figma_export/); se il codice risulta GIA'' estratto in una directory del progetto (verifica con list_files), NON ri-estrarre e usa quello esistente",
  "Censisci gli import non risolvibili nei componenti estratti (componenti ./components/ui/* e @/components/ui/*, librerie esterne): l''export Figma Make NON include i componenti shadcn/ui ne'' il setup Tailwind",
  "Installa le dipendenze mancanti rilevate (es. tailwindcss, lucide-react, react-router, @radix-ui/*, clsx, tailwind-merge, class-variance-authority) con npm install",
  "Crea/aggiorna il bootstrap del frontend: index.html, main.tsx, css con @tailwind, tailwind.config (content sulle directory del codice), postcss.config, vite.config, tsconfig",
  "Ricostruisci i componenti shadcn/ui referenziati ma assenti (button, card, input, tabs, ecc.) + lib/utils (cn)",
  "Avvia il dev server e VERIFICA nel browser le rotte principali; correggi gli errori di build/runtime fino a pagina funzionante"
]'::jsonb
WHERE key = 'implement.figma_make';
