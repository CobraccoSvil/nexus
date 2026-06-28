-- Hint correttivo per le installazioni GLOBALI (npm/pnpm/yarn -g/--global).
--
-- Causa radice (GAP sudo "install globali falliscono muti"): un comando come
-- `npm install -g <pkg>` NON inizia con `sudo`, quindi non viene intercettato
-- dal routing privilegiato (privileged.rs) ne' bloccato dalla safety: viene
-- eseguito in shell senza privilegi e fallisce con EACCES sul prefix globale.
-- L'errore opaco mandava l'agente in retry/loop sullo stesso comando. Qui un
-- hint DB-driven (nexus_command_hints, mig 0230) lo intercetta PRIMA e fornisce
-- l'alternativa corretta, senza richiedere privilegi ne' rebuild (regola G/H).

INSERT INTO nexus_command_hints (pattern, pattern_kind, hint_text, severity) VALUES
    ('install -g',
     'substring',
     'ATTENZIONE: l''installazione globale (-g) richiede privilegi di root e fallisce con EACCES nei progetti utente (il prefix globale non e'' scrivibile). NON usare install globali: aggiungi la dipendenza al progetto (`npm install <pkg>` / `pnpm add <pkg>` SENZA -g) oppure eseguila una tantum con `npx <pkg>` / `pnpm dlx <pkg>`.',
     'warning'),
    ('add -g',
     'substring',
     'ATTENZIONE: `pnpm add -g` (install globale) richiede root e fallisce con EACCES nei progetti utente. Aggiungi la dipendenza al progetto (`pnpm add <pkg>` SENZA -g) oppure usala una tantum con `pnpm dlx <pkg>`.',
     'warning'),
    ('--global',
     'substring',
     'ATTENZIONE: l''install globale (--global) richiede root e fallisce con EACCES nei progetti utente. Usa una dipendenza locale del progetto (senza --global) oppure un runner una-tantum (`npx`/`pnpm dlx`).',
     'warning'),
    ('yarn global add',
     'substring',
     'ATTENZIONE: `yarn global add` richiede root e fallisce con EACCES nei progetti utente. Aggiungi la dipendenza al progetto (`yarn add <pkg>`) oppure usala una tantum con `npx <pkg>`.',
     'warning')
ON CONFLICT (pattern) DO NOTHING;
