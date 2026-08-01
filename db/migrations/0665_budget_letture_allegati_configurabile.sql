-- Migrazione 0665 — Il budget di lettura allegati per sessione torna nel DB.
--
-- Reintroduce `agent.attachment.session_read_budget_bytes` (500000), con lo
-- STESSO valore che il codice applica oggi: nessun cambio di comportamento,
-- solo la fonte che diventa quella dichiarata.
--
-- Perche': la mig 0216 cancello' questa chiave motivandolo con "non piu'
-- referenziata dal codice". La premessa era falsa per questa chiave. Il gate
-- che consuma il budget e' vivo e gira a ogni turno agentico
-- (`attachment_budget_block` in nexus-agent-graph/src/nodes/tool_dispatch.rs:
-- `nexus_read_attachment` e `nexus_read_archive_entry` vengono rifiutati con un
-- tool_result sintetico quando i byte cumulativi della sessione raggiungono il
-- budget). Cancellata la chiave, il gate ha continuato a girare sul 500000
-- cablato nel `Default` della config: un valore che nessun amministratore
-- poteva vedere ne' cambiare, cioe' il fallback silenzioso che la regola G
-- vieta. Lo stesso numero e' documentato in CLAUDE.md sezione I come "default
-- DB" — la documentazione descriveva un DB che non conteneva la chiave.
--
-- Con la chiave presente, chi voleva l'esito della 0216 (nessun cap di
-- contenuto: la pre-extraction passa dal RAG) puo' ottenerlo alzando il valore,
-- senza toccare il codice. ATTENZIONE: `0` NON significa "nessun budget" —
-- il confronto e' `byte_gia_letti < budget`, quindi 0 blocca la prima lettura.
-- Per togliere di fatto il limite si alza il valore.
--
-- Idempotente: ON CONFLICT (key) DO NOTHING (se un ambiente la ha gia', il suo
-- valore vince).

-- Le altre due chiavi qui sotto esistono nel DB vivo ma non le semina alcuna
-- migrazione: sono nate a mano. Ora il motore nativo le LEGGE, quindi devono
-- sopravvivere a un wipe del DB e alla ri-applicazione delle migrazioni (regola
-- H, punto 2). I valori sono quelli gia' presenti in produzione e coincidono coi
-- safe-default del nodo: nessun cambio di comportamento, solo la fonte che
-- diventa riproducibile.

INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('agent.attachment.session_read_budget_bytes', '500000', 'agent',
     'Budget cumulativo (byte) delle letture di allegati per sessione. Oltre la soglia nexus_read_attachment e nexus_read_archive_entry ricevono un tool_result sintetico che indirizza agli estrattori strutturati. Il confronto e'' byte_gia_letti < budget: 0 blocca la prima lettura, per togliere il limite si alza il valore. Default 500000.',
     NOW()),
    ('agent.context.max_chars', '400000', 'agent',
     'Budget totale del contesto di un turno in caratteri. Se i tool_result del turno lo sfondano, il tool_dispatch li comprime tutti a una quota equa (offload in RAG, degrado a troncamento testa+coda). Default 400000.',
     NOW()),
    ('agent.tools.discovery_schema_max_bytes', '8192', 'agent',
     'Dimensione massima (byte) dell''input_schema di un tool scoperto via nexus_mcp_tool_search. Sopra la soglia lo schema viene azzerato al default {"type":"object","properties":{}} prima di essere iniettato come tool nativo. Default 8192.',
     NOW())
ON CONFLICT (key) DO NOTHING;
