-- 0454_service_discovery_no_variants.sql
-- Fix doppione wizard: l'agent di service_discovery (mig 0361) generava varianti
-- sintetiche dello STESSO servizio. Es. Beauty-Book: il backend appariva due
-- volte, come "pnpm run dev:backend" E come "nodemon --exec tsx src/app.ts"
-- (stesso entrypoint src/app.ts, due voci) -> il wizard mostrava un terzo
-- candidato "backend-isolato" ridondante, oltre a frontend e backend reali.
--
-- Causa radice: il prompt vietava di duplicare i servizi a livello di
-- docker-vs-nativo, ma NON le varianti di AVVIO dello stesso processo. Qui si
-- aggiunge la regola esplicita al <protocollo> e il check corrispondente nel
-- <reflection>, con UPDATE chirurgici (REPLACE su sottostringhe univoche gia'
-- verificate in DB) invece di riscrivere l'intero template (evita divergenze).
--
-- Idempotente: a REPLACE applicato la sottostringa target non esiste piu' nel
-- content (entrambi i target includono il loro stesso confine), quindi una
-- seconda esecuzione e' no-op.

BEGIN;

-- Regola nel protocollo: un servizio per ENTRYPOINT, non per modo di avvio.
UPDATE nexus_prompt_templates
SET content = REPLACE(
    content,
    $old$a meno che non siano chiaramente alternativi.
</protocollo>$old$,
    $new$a meno che non siano chiaramente alternativi.
4. UN servizio per ENTRYPOINT, non per modo di avvio. Se lo stesso processo
   (stesso file/script di ingresso, es. src/app.ts) puo' essere lanciato in piu'
   modi - via script del package manager (es. "pnpm run dev:backend") E via
   comando diretto equivalente (es. "nodemon --exec tsx src/app.ts") - e' LO
   STESSO servizio: emetti UNA sola voce, scegliendo la forma canonica via script
   del package manager. NON creare varianti "isolate"/"dirette"/"alternative"
   dello stesso ruolo: ogni "short" deve identificare un processo DISTINTO, non un
   modo diverso di avviare lo stesso processo.
</protocollo>$new$
)
WHERE key = 'agent.service_discovery';

-- Check corrispondente nel reflection (ancorato alla riga precedente per restare
-- idempotente: dopo l'inserimento il target completo non si ripresenta).
UPDATE nexus_prompt_templates
SET content = REPLACE(
    content,
    $old$- port_vars contiene NOMI, mai numeri.
- nessun placeholder {{...}} non sostituito.$old$,
    $new$- port_vars contiene NOMI, mai numeri.
- nessun servizio e' una variante di avvio di un altro: stesso entrypoint via
  script del package manager e via comando diretto -> tienine UNO solo.
- nessun placeholder {{...}} non sostituito.$new$
)
WHERE key = 'agent.service_discovery';

COMMIT;
