-- Migrazione 0667 — I flag del nodo understanding tornano a 'false', dichiarando
-- perche' erano 'true'.
--
-- Le sei chiavi `orchestrator.understanding_*` nascono con la migrazione 0207 e
-- la 0564 le porta a 'true' elencandole fra i "flag VERIFICATI letti dal codice
-- Rust attuale". La verifica non c'era: `load_understanding_config` in
-- crates/mcp-core/src/native_engine.rs cablava i safe-default con un commento che
-- AFFERMAVA l'inesistenza di quelle chiavi nel DB, cercandole col prefisso
-- sbagliato (`understanding.` invece di `orchestrator.understanding_`). Per un
-- anno il nodo e' rimasto spento mentre la configurazione lo dichiarava acceso, e
-- il commento sbagliato impediva al lettore successivo di cercarle.
--
-- Il codice ora le legge (stessa modifica che veicola questa migrazione), quindi
-- il valore in tabella governa davvero il comportamento. Riportarle a 'false' NON
-- e' un ripiego: e' la registrazione dello stato reale in cui il sistema ha
-- girato e sul quale sono state tarate tutte le misure. L'accensione del nodo
-- understanding (fase pre-planner con fan-out di sub-agent explore, quindi costo
-- e latenza aggiuntivi su OGNI run) e' un cambio di comportamento sostanziale che
-- va valutato a se' — ma da oggi quella valutazione si conclude con un UPDATE,
-- non con un deploy (regola G).
--
-- Le altre tre chiavi (topk, min_token_budget, max_explore) restano ai valori
-- correnti: sono parametri, non interruttori, e ora vengono letti.

UPDATE settings
SET value = 'false',
    updated_at = NOW()
WHERE key IN (
    'orchestrator.understanding_enabled',
    'orchestrator.understanding_fanout_enabled',
    'orchestrator.understanding_synthesize_enabled'
);
