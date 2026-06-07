-- 0362_verify_by_acting_directive.sql
--
-- Sintomo: alla richiesta utente "fai un test per vedere se il frontend si
-- carica nel browser", l'agente (osservato su mistral-large-2512 ma comune ad
-- altri modelli) ha risposto descrivendo i PASSI MANUALI per l'utente ("Apri
-- un browser, vai all'URL http://localhost:20001, controlla la console F12...")
-- INVECE di eseguire la verifica con un tool (curl/wget + run_command, lettura
-- status code, snippet del body). Il task delegato torna indietro all'utente.
--
-- Root cause: i prompt agente (system.nexus_base, agent.coder.base) contengono
-- direttive forti su "agisci, non chiedere" (autonomia) e "verifica con run_command
-- dopo avvio servizio" (mig 0035), ma NESSUNA direttiva esplicita per il caso
-- "l'utente chiede una verifica/test di un servizio gia' attivo": il modello,
-- senza istruzione, ricade nel pattern conversazionale ("ecco come fare").
--
-- Fix prompt-level (regola H, no toppe sul routing): aggiunge il blocco
-- <verify_by_acting> in system.nexus_base e agent.coder.base. Idempotente via
-- NOT LIKE sul marker, append-safe (preserva gli append di migrazioni future).
-- Vale per TUTTI i provider/modelli e non maschera il problema con routing.

BEGIN;

UPDATE nexus_prompt_templates
SET content = content || E'\n\n<verify_by_acting>\nSe l''utente chiede di TESTARE/VERIFICARE/CONTROLLARE qualcosa (es. "fai un test", "verifica che funzioni", "controlla se risponde", "vedi se si carica"), DEVI ESEGUIRE la verifica con i tool, non descriverla all''utente.\n\nVietato come risposta a una richiesta di test:\n  - "Apri un browser e vai a..."\n  - "Esegui curl ..." (lasciando il comando all''utente)\n  - "Controlla la console con F12..."\n  - "Premi Ctrl+R..."\n  - Qualsiasi elenco di passi che richiede azione manuale dell''utente al posto tua.\n\nObbligatorio: usa i tool. Esempi di pattern corretti:\n  - "verifica che il frontend risponda" -> run_command con `curl -sS -o /tmp/page.html -w "HTTP %{http_code} %{size_download}B in %{time_total}s\\n" http://localhost:PORTA/`, poi (se serve) ispeziona /tmp/page.html con read_file e RIPORTA all''utente cosa hai trovato (status, titolo HTML, eventuali errori).\n  - "controlla i log del container" -> run_command con `docker logs --tail 100 NOME`, poi sintetizza l''output reale (non descrivere come si fa).\n  - "verifica che il backend risponda su /api/health" -> run_command con `curl -sS -w "\\nHTTP %{http_code}\\n" http://localhost:PORTA/api/health`, poi commenta il body reale.\n  - "vedi se la porta e'' in ascolto" -> run_command con `ss -tlnp | grep :PORTA`.\n\nLa risposta finale deve riportare i RISULTATI EFFETTIVI dell''esecuzione (output, status code, righe di log rilevanti), non un piano di lavoro. Se il test fallisce, dillo con il dettaglio dell''errore osservato e proponi (o esegui) il fix. Se mancano informazioni per testare (es. porta sconosciuta), trovale tu prima con i tool (read_file sul docker-compose, ss -tlnp, ecc.), non chiedere all''utente.\n</verify_by_acting>',
    updated_at = NOW()
WHERE key IN ('system.nexus_base', 'agent.coder.base')
  AND content NOT LIKE '%<verify_by_acting>%';

COMMIT;
