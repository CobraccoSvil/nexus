-- 0434: idempotenza reale request_port per (project_id, label) + direttiva "riusa-prima".
--
-- CAUSA RADICE del loop request_port (diagnosi confermata, Beauty-Book):
-- Nexus non ha un punto unico autoritativo "servizio del progetto -> attivo su
-- quale porta -> riusa/riavvia/alloca". request_port (find_or_allocate) e'
-- idempotente solo per match ESATTO (project_id,label) ma SENZA vincolo DB:
-- esiste solo uq_port UNIQUE(port). Variando il contorno della label
-- (es. "backend" -> "Backend - Nodemon (TypeScript)") l'agente ottiene sempre
-- una nuova allocazione 'dynamic' invece di riusare la porta gia' attiva,
-- e si generano righe DUPLICATE per (project_id,label).
--
-- Questa migrazione abilita il fix lato codice (resource_resolver +
-- find_or_allocate consapevole) fornendo:
--   1. vocabolario allocation_mode completo ('adopted' oggi assente: il ramo
--      adopt di find_or_allocate scrive 'adopted' ma il CHECK lo rifiuta);
--   2. deduplica preventiva delle righe per (project_id,label);
--   3. indice UNIQUE(project_id,label) che abilita
--      INSERT ... ON CONFLICT (project_id,label) DO UPDATE (idempotenza reale);
--   4. direttiva di sequenza "riusa-prima" nel blocco <port_allocation> dei
--      system prompt, che rimanda al blocco RISORSE PROGETTO e corregge il
--      claim fuorviante "idempotente per label".
--
-- Riferimenti: mig 0114 (uq_port), 0146 (CHECK 'dynamic'/'existing'),
-- 0396 (blocco <port_allocation> v2). Idempotente, re-apply safe.

BEGIN;

-- 1. Vocabolario allocation_mode: aggiungi 'adopted' (riuso di un orfano LISTEN
--    riagganciato dal probe). Pattern di mig 0146: drop+add del CHECK.
ALTER TABLE nexus_port_allocations
    DROP CONSTRAINT IF EXISTS nexus_port_allocations_allocation_mode_check;

ALTER TABLE nexus_port_allocations
    ADD CONSTRAINT nexus_port_allocations_allocation_mode_check
        CHECK (allocation_mode IN ('auto', 'manual', 'dynamic', 'existing', 'adopted'));

-- 2. DEDUPLICA PREVENTIVA per (project_id, label) PRIMA dell'indice UNIQUE.
--    La tabella puo' contenere duplicati emersi a runtime (vedi Beauty-Book con
--    due righe label='backend'). Tieni UNA riga per (project_id,label) con
--    priorita' deterministica:
--      (a) allocation_mode "informativo del riuso" (adopted/existing/dynamic)
--          sopra 'auto'/'manual';
--      (b) a parita', service_unit valorizzato (riga registrata dal wizard);
--      (c) a parita', updated_at piu' recente;
--      (d) tie-break id.
--    Cancella le righe rn>1. No-op se non ci sono duplicati.
WITH ranked AS (
    SELECT
        id,
        row_number() OVER (
            PARTITION BY project_id, label
            ORDER BY
                CASE allocation_mode
                    WHEN 'adopted'  THEN 0
                    WHEN 'existing' THEN 1
                    WHEN 'dynamic'  THEN 2
                    WHEN 'manual'   THEN 3
                    WHEN 'auto'     THEN 4
                    ELSE 5
                END ASC,
                (service_unit IS NOT NULL) DESC,
                updated_at DESC,
                id DESC
        ) AS rn
    FROM nexus_port_allocations
)
DELETE FROM nexus_port_allocations
 WHERE id IN (SELECT id FROM ranked WHERE rn > 1);

-- 3. Indice UNIQUE(project_id, label): rende la coppia identita' del servizio
--    e abilita ON CONFLICT (project_id, label) DO UPDATE in find_or_allocate.
--    A questo punto i duplicati sono stati rimossi, l'indice regge.
CREATE UNIQUE INDEX IF NOT EXISTS uq_port_alloc_project_label
    ON nexus_port_allocations (project_id, label);

-- 4. Direttiva "riusa-prima" nel blocco <port_allocation> dei system prompt.
--    Aggiunge la sequenza: leggi RISORSE PROGETTO -> riusa/riavvia se attivo ->
--    request_port SOLO per servizio nuovo. Corregge il claim "idempotente per
--    label" (vero solo a match esatto: variare la label genera doppioni, ora
--    impediti dall'indice + ON CONFLICT DO UPDATE).
--    Marker di idempotenza: presenza di 'riusa-prima' nel blocco. Pattern
--    regexp_replace di mig 0396. Non-greedy per non oltrepassare il chiudi-tag.
UPDATE nexus_prompt_templates
   SET content = regexp_replace(
        content,
        '<port_allocation>.*?</port_allocation>',
        '<port_allocation>
Ogni porta TCP del progetto (server HTTP, gRPC, WebSocket, DB, qualsiasi listener) passa SOLO dai tool Nexus. NON hardcodare mai 3000, 8080, 5173 o altre porte fisse.

SEQUENZA riusa-prima (OBBLIGATORIA prima di allocare):
1. LEGGI il blocco RISORSE PROGETTO nel tuo contesto (stato runtime reale: servizi del progetto, porte, se sono in ascolto). E'' la fonte autoritativa, non riscoprirla con tool.
2. Se un servizio del TUO scopo e'' gia'' ATTIVO (in ascolto), RIUSA la sua porta: non chiamare request_port.
3. Se e'' allocato ma SPENTO, RIAVVIALO sulla sua porta esistente (service_restart / riavvio del processo), non allocarne una nuova.
4. Chiama request_port SOLO per un servizio NUOVO non elencato nelle RISORSE PROGETTO.

- VERIFICA/elenco porte: chiama nexus_list_ports (sola lettura: bucket assegnato + allocazioni registrate). Non dedurre le porte leggendo i sorgenti.
- ALLOCAZIONE: chiama request_port(label="<servizio>") e usa la porta ritornata (range 20000-39999). request_port riusa la porta di un servizio gia'' attivo dello stesso scopo (allocation_mode=''existing'') invece di allocarne una nuova; variare il contorno della label NON crea un servizio nuovo.
- Un fallback env con default numerico (process.env.PORT || 5000, os.environ.get("PORT", 5000), env::var("PORT").unwrap_or("3000")) E'' a tutti gli effetti una porta hardcoded: viene RIFIUTATO in scrittura. Se serve un default, usa la porta ALLOCATA da request_port.
- Vietato aggirare lo scanner con run_command/sed/heredoc: i processi su porte non allocate vengono terminati dal port enforcer.

Se hardcodi una porta il servizio va in conflitto con altri progetti sulla stessa macchina e la scrittura viene rifiutata.
</port_allocation>'
        )
 WHERE key IN ('system.nexus_base', 'agent.coder.base')
   AND content LIKE '%<port_allocation>%'
   AND content NOT LIKE '%riusa-prima%';

COMMIT;
