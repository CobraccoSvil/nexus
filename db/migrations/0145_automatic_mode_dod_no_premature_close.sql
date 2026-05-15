-- Fix M56: aggiungi DoD (Definition of Done) esplicita al prompt automatic mode
-- per evitare la chiusura prematura del run (sintomo iter_7).
--
-- Sintomo: l'agente scriveva 2 file (package.json root + backend/package.json),
-- poi diceva "Operazione completata. Ho eseguito 2 step." e chiudeva il run
-- come `completed` anche senza schema DB, codice, frontend, test.
--
-- Causa: il modello (o4-mini, scelto per misclassification intent=docs su iter_7
-- pre M57) era libero di considerare il task done dopo qualunque output.
--
-- Fix: prepend al body una sezione DoD assertiva che impone i criteri di
-- accettazione e vieta esplicitamente la chiusura precoce.

UPDATE nexus_prompt_templates
SET content = $$DEFINITION OF DONE (DoD) — IL TASK NON E' COMPLETO FINCHE':
- Per scaffolding app: backend avviato + frontend avviato + DB creato con tabelle reali + curl http risponde 200 sui due endpoint principali (almeno).
- Per fix/refactor: i sorgenti compilano + i test esistenti passano + il sintomo originale e' risolto.
- Per docs: il file richiesto esiste sul filesystem con il contenuto previsto.
NON dichiarare il task completato finche' la DoD non e' verificata via tool concreto (run_command + curl, prisma migrate, test runner). VIETATE le frasi tipo "Operazione completata", "Ho eseguito N step", "Fatto" senza prove di funzionamento.
Se ti accorgi che il task e' grosso, NON delegare: continua a iterare nello stesso run finche' la DoD passa o raggiungi il cap iterazioni.

$$ || content,
    updated_at = NOW()
WHERE key = 'automation.mode_automatic_instruction';
