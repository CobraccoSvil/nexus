-- Migrazione 0293 — Direttiva anti-narration nel system prompt (ADR 0017 Fix B).
--
-- Aggiunge a `system.nexus_base` un blocco esplicito che vieta al modello di
-- chiudere il turno annunciando azioni future. Caso reale chat 6 Beauty-Book
-- (run e38aaba7, 12:51): Gemini 2.5 Pro ha narrato "Sto procedendo con la
-- creazione di altri test..." e ha emesso end_turn senza chiamare il prossimo
-- tool. Con questo prompt il modello viene istruito a:
--   (1) chiamare il tool SUBITO se intende fare altri passi;
--   (2) chiudere SOLO con "TASK COMPLETATO" se ha veramente finito;
--   (3) usare "DOMANDA UTENTE: ..." se necessita di input.
--
-- Idempotente: aggiunge il blocco SOLO se il marker [[NEXUS_CONTINUATION_PROTOCOL]]
-- non e' gia' presente. Permette re-applicazione sicura della migrazione.

UPDATE nexus_prompt_templates
   SET content = content || E'\n\n[[NEXUS_CONTINUATION_PROTOCOL]]\n### PROTOCOLLO DI CONTINUAZIONE (OBBLIGATORIO) ###\n\nSe prevedi di fare altri passi, chiama il tool successivo ORA, in questo turno. NON narrare quello che farai dopo.\n\nVIETATO come ultima riga del turno (Nexus rileva e ti rilancia automaticamente):\n  - "sto procedendo con..."\n  - "continuo con / passo a..."\n  - "ora creo / ora scrivo / ora implemento..."\n  - "creerò / implementerò / aggiungerò..."\n  - "il prossimo passo / passo successivo..."\n  - In inglese: "I''m proceeding", "I''ll proceed", "moving on to", "I''ll create..."\n\nOBBLIGATORIO come ultima riga del turno (scegli UNA):\n  - TASK COMPLETATO    -> hai finito tutto, riepilogo sopra\n  - DOMANDA UTENTE: ?  -> ti serve input umano, fai una domanda concreta\n  - (tool call vero)   -> hai altri passi da fare, eseguili adesso senza annunciarli\n\nQuesto protocollo previene il continuation hallucination: il modello dichiara intenzioni future ma chiude il turno senza eseguirle, lasciando l''utente nel dubbio se Nexus stia ancora lavorando.\n',
       updated_at = NOW(),
       version = version + 1
 WHERE key = 'system.nexus_base'
   AND content NOT LIKE '%[[NEXUS_CONTINUATION_PROTOCOL]]%';

-- Stesso blocco anche su agent.coder.base (il system prompt dell'agent coder
-- ha il proprio template clonato; va aggiornato per consistenza).
UPDATE nexus_prompt_templates
   SET content = content || E'\n\n[[NEXUS_CONTINUATION_PROTOCOL]]\n### PROTOCOLLO DI CONTINUAZIONE (OBBLIGATORIO) ###\n\nSe prevedi di fare altri passi, chiama il tool successivo ORA, in questo turno. NON narrare quello che farai dopo.\n\nVIETATO come ultima riga del turno (Nexus rileva e ti rilancia automaticamente):\n  - "sto procedendo con..."\n  - "continuo con / passo a..."\n  - "ora creo / ora scrivo / ora implemento..."\n  - "creerò / implementerò / aggiungerò..."\n  - "il prossimo passo / passo successivo..."\n  - In inglese: "I''m proceeding", "I''ll proceed", "moving on to", "I''ll create..."\n\nOBBLIGATORIO come ultima riga del turno (scegli UNA):\n  - TASK COMPLETATO    -> hai finito tutto, riepilogo sopra\n  - DOMANDA UTENTE: ?  -> ti serve input umano, fai una domanda concreta\n  - (tool call vero)   -> hai altri passi da fare, eseguili adesso senza annunciarli\n',
       updated_at = NOW(),
       version = version + 1
 WHERE key = 'agent.coder.base'
   AND content NOT LIKE '%[[NEXUS_CONTINUATION_PROTOCOL]]%';
