-- Popolamento retroattivo custom_instructions per SolarMatch.
-- Per i progetti già analizzati prima della migration 0065, le custom_instructions
-- non sono state generate automaticamente. Questo script le popola manualmente
-- per il progetto SolarMatch trovato tramite il path del repository.

UPDATE projects
SET custom_instructions = $$=== ISTRUZIONI SPECIFICHE DEL PROGETTO ===
1. VERIFICA OBBLIGATORIA: dopo aver modificato file TypeScript, TSX, JSX o CSS in Next.js, esegui `pnpm verify` dalla directory root del progetto prima di dichiarare il task completato. Se il comando fallisce, correggi tutti gli errori prima di concludere. NON dichiarare mai 'task completato' senza aver verificato che il build è pulito.
2. INTEGRITÀ NEXT.JS: quando rimuovi o sposti componenti, verifica che TUTTI i link/href che puntavano a quell'elemento (es. ancore #id, import, route) siano aggiornati di conseguenza. Verifica che i CSS module usino solo classi definite nel file .module.css corrispondente.
=== FINE ISTRUZIONI PROGETTO ===$$
WHERE custom_instructions IS NULL
  AND (
    analysis_json->>'frameworks' ILIKE '%next%'
    OR name ILIKE '%solarmatch%'
    OR name ILIKE '%solar%'
  );
