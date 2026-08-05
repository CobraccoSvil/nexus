-- 0680: una scrittura che cambia i soli FINE-RIGA non e' progresso.
--
-- IL DIFETTO, misurato il 2026-08-05 leggendo la catena.
--
-- `nexus-agent-graph::decisions::correction_progress` decide se un rimando in
-- correzione ha prodotto qualcosa confrontando due hash:
--
--     WriteFact::cambia_il_contenuto() -> before_sha256 != after_sha256
--
-- Quel modulo dichiara di esistere proprio per vedere "il caso Some(x) ==
-- Some(x), la riscrittura identica" — l'agente che simula attivita' salvando
-- file immutati. Ma gli hash li calcola `mcp-core::file_mutations::sha256_hex`
-- sui BYTE GREZZI, e una riscrittura che cambia solo la convenzione di fine-riga
-- produce byte diversi a contenuto identico. Quel caso sfugge al criterio, e il
-- ciclo lo classifica `Effettivo`: "il rimando ha prodotto progresso", su un
-- file in cui non e' cambiata una virgola.
--
-- Non e' teorico su questa piattaforma: `core.autocrlf=true` e' attivo, e il
-- 2026-08-05 quindici script del repo risultavano materializzati CRLF mentre il
-- loro blob era LF.
--
-- PERCHE' UNA COLONNA E NON UN CONFRONTO A VALLE.
--
-- `correction_progress` riceve HASH, non byte: da due digest non si puo' dedurre
-- se differiscano per i soli fine-riga. Il fatto va misurato dove i byte
-- esistono ancora — `file_mutations::derive`, che li ha in mano — e persistito.
--
-- PERCHE' NON SI NORMALIZZA ALLA FONTE, che sarebbe stato piu' corto.
--
-- Gli stessi `before_sha256`/`after_sha256` hanno altri lettori, e pongono una
-- domanda DIVERSA: `mutations_api` li usa per chiedersi se qualcuno ha toccato
-- il file dopo la mutazione registrata, e `service_recovery` per sapere cosa e'
-- stato scritto. Li' anche un fine-riga diverso E' una modifica. Normalizzare
-- l'hash alla fonte avrebbe dato la risposta giusta a un consumatore e quella
-- sbagliata agli altri due.
--
-- IL CRITERIO E' UN PUNTO UNICO: `nexus_migrations::fine_riga` (commit
-- 6cfe9f54), che CLASSIFICA (Identici | SoloFineRiga | ContenutoDiverso) invece
-- di normalizzare in silenzio. La stessa domanda si e' presentata tre volte in
-- tre posti diversi — mig 0500, mig 117/118, e qui.

ALTER TABLE file_mutations
    ADD COLUMN IF NOT EXISTS solo_fine_riga BOOLEAN;

-- NULL e non FALSE come default, ed e' la parte che conta.
--
-- Le righe scritte prima di questa migrazione non sono state misurate: nessuno
-- ha confrontato i loro byte. `FALSE` affermerebbe "il contenuto era davvero
-- cambiato", che per quelle righe non si sa. `NULL` dice "non misurato", e il
-- criterio lo tratta come tale — sul pregresso il comportamento resta
-- esattamente quello di prima (regola Q: l'ignoto e' un caso, non un valore
-- comodo).
COMMENT ON COLUMN file_mutations.solo_fine_riga IS
    'TRUE se la mutazione ha cambiato i soli fine-riga (contenuto identico). NULL = non misurato (riga anteriore alla mig 0680): il criterio del progresso ricade sul confronto degli hash, come prima.';
