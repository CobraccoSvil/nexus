-- Migrazione 0290 — Fix command_template playwright-install-deps per Ubuntu 24.04 noble.
--
-- Il seed iniziale (mig 0289) elencava libnssutil3 e libgtk-3-0, ma su Ubuntu
-- 24.04 questi nomi non esistono:
--   - libnssutil3   -> incluso in libnss3 (no pacchetto separato)
--   - libgtk-3-0    -> rinominato libgtk-3-0t64 (transizione time_t 64-bit)
--
-- Errore osservato: "E: Unable to locate package libnssutil3" (exit=100).
--
-- Fix idempotente: aggiorna solo se la riga del seed e' rimasta al valore
-- originale, cosi' eventuali personalizzazioni admin via UI sono preservate.

UPDATE nexus_sudo_purposes
   SET command_template = 'apt-get install -y libnspr4 libnss3 libasound2t64 libxss1 libgbm1 libgtk-3-0t64 libpangocairo-1.0-0 libatk1.0-0t64 libatk-bridge2.0-0t64 libcups2t64 libxshmfence1',
       description = 'Installa le librerie di sistema necessarie a chromium-headless-shell (Playwright). Risolve l''errore "Target page, context or browser has been closed" quando il binary del browser non puo'' avviarsi per assenza di libnspr4/libnss3/libasound. Pacchetti adeguati a Ubuntu 24.04 noble (libnssutil3 in libnss3, libgtk-3-0 -> libgtk-3-0t64).',
       updated_at = NOW()
 WHERE name = 'playwright-install-deps'
   AND command_template = 'apt-get install -y libnspr4 libnss3 libnssutil3 libasound2t64 libxss1 libgbm1 libgtk-3-0 libpangocairo-1.0-0 libatk1.0-0t64 libatk-bridge2.0-0t64 libcups2t64 libxshmfence1';
