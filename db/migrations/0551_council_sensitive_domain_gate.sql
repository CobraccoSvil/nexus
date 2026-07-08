-- 0551_council_sensitive_domain_gate.sql
-- FIX DEFINITIVO del gate di attivazione del consiglio (regola H): sostituisce la
-- metrica SBAGLIATA introdotta in mig 0550.
--
-- Causa radice: il gate usava estimate_prompt_complexity, che pesa keyword di
-- AZIONE / quantita' di lavoro (fullstack +10, end-to-end +8, refactor +4,
-- create/deploy/migrate +3, build/backend/frontend/database +2) + step markers +
-- file path. Un task di DOMINIO sensibile come "aggiungi 2FA via email con OTP,
-- rate limiting, brute force, segreti" tocca UNA sola keyword pesata (database=2)
-- -> score 2 << soglia 40 -> la direttiva <consiglio_analisi> veniva rimossa e il
-- consiglio NON si attivava proprio sui task per cui serve. Verificato E2E sulla UI.
--
-- Fix: il consiglio si attiva quando il task tocca AMBITI A RISCHIO (auth/sicurezza,
-- pagamenti, schema/migrazioni DB, azioni distruttive, privacy, ...), rilevati da
-- keyword di DOMINIO (match substring, case-insensitive). Punto unico:
-- prompt_templates::gate_council_directive + count_sensitive_domain_hits (puro,
-- testato). DB-driven (regola G): keyword e soglia-hit nei settings, niente hardcode.

-- (1) Rimuove il setting della metrica sbagliata (mig 0550), non piu' letto.
DELETE FROM settings WHERE key = 'orchestrator.council_min_complexity';

-- (2) Keyword di ambito sensibile (CSV, match substring: radici per coprire le
--     declinazioni, es. 'autentica' -> autenticazione/autenticare/autenticato).
INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.council_trigger_keywords',
   'autentica,login,log in,password,oauth,token,jwt,2fa,mfa,otp,sessione,autorizzazione,permess,ruolo,ruoli,sicurezza,crittografia,cifratura,segret,credenzial,api key,pagament,carta di credito,stripe,paypal,checkout,fattura,transazion,migrazion,schema,database,tabella,colonna,indice,elimina,cancella,drop table,truncate,rimuov,gdpr,dati personali,dati sensibili,privacy,pii,deploy,webhook,integrazione,terze parti,rate limit,brute force,cron,scheduler,upload',
   'orchestrator',
   'Consiglio a monte: keyword di AMBITO SENSIBILE (CSV, match substring case-insensitive). Se il messaggio utente ne tocca almeno council_min_trigger_hits, la direttiva <consiglio_analisi> resta nel system prompt (il modello convoca le figure); altrimenti viene rimossa (percorso diretto). Rileva ambiti a rischio di dominio, non la quantita di lavoro. DB-driven, regola G.'),
  ('orchestrator.council_min_trigger_hits', '1', 'orchestrator',
   'Consiglio a monte: numero MINIMO di keyword di ambito sensibile (council_trigger_keywords) che il messaggio deve toccare per attivare il consiglio. Default 1 (basta un ambito sensibile). DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
