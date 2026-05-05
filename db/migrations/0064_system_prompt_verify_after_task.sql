-- Aggiunge istruzione globale: l'agente deve eseguire `pnpm verify` (o equivalente)
-- prima di concludere qualsiasi task che modifica file di un progetto con build/typecheck.
-- Questo previene il problema di modifiche che rompono silenziosamente il build
-- (come accaduto con SolarMatch dopo le modifiche al frontend di Sofia).

UPDATE nexus_prompt_templates
SET content = content || $$

REGOLA FINALE OBBLIGATORIA — VERIFICA BUILD:
Prima di dichiarare il task completato, se hai modificato file TypeScript/CSS/JSX in un progetto
che ha uno script `verify` o `typecheck` o `build`, DEVI eseguire:
  run_command("cd <project_dir> && pnpm verify")   (se esiste `pnpm verify`)
  oppure run_command("cd <project_dir> && pnpm typecheck && pnpm build")
Se il comando fallisce, correggi gli errori prima di concludere.
NON dichiarare mai "task completato" se non hai verificato che il build è pulito.$$,
    updated_at = now(),
    updated_by = 'system'
WHERE key = 'system.nexus_base';

-- Aggiunge regola specifica per SolarMatch al prompt supervisor
UPDATE nexus_prompt_templates
SET content = content || $$

  → Se il task riguarda SolarMatch (file in src/components/sofia, src/app, src/components/public):
    Alla fine del task l'agente DEVE eseguire run_command("cd /path/to/solarmatch && pnpm verify").
    Se non lo ha fatto, forza un redirect: "Esegui `pnpm verify` in D:\\Sviluppo\\solarmatch prima di concludere."$$,
    updated_at = now(),
    updated_by = 'system'
WHERE key = 'automation.supervisor_monitoring';
