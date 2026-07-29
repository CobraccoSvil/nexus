-- 0653_ui_ux_designer_advisory_verdict.sql
-- La figura ui_ux_designer non poteva consegnare il proprio parere.
--
-- SINTOMO OSSERVATO (28-29/07/2026, progetto gestione-spese): nel Consiglio la
-- figura compariva con "errore / Sub-run terminato senza esito positivo", mentre
-- sysadmin e functional_analyst rispondevano regolarmente. I log dicevano una
-- cosa apparentemente contraddittoria:
--
--   subagent_native: sub-run eseguito sul grafo nativo kind=ui_ux_designer
--                    completed=true iterations=2 summary_len=43
--   consiglio a monte: figura senza parere valido kind=ui_ux_designer
--                      status=RunFailed detail_code=run_failed
--
-- Il sub-run COMPLETA (non e' un crash, ne' un timeout, ne' un provider caduto) e
-- subito dopo risulta fallito. Le due righe non si contraddicono: descrivono
-- l'esecuzione riuscita di un agente che non aveva modo di dichiarare l'esito.
--
-- CAUSA: la 0650 ha costruito la tool_whitelist della nuova figura ricopiando
-- quella delle figure esistenti e aggiungendo i due tool suoi
-- (ui_layout_patterns, ui_reference_search), ma nel farlo ha perso
-- `advisory_verdict` -- il tool con cui una figura del Consiglio CONSEGNA il
-- parere strutturato (mig 0548). Senza quel tool la figura puo' leggere,
-- cercare e ragionare, e non puo' dire niente: il verdetto non arriva e
-- l'aggregatore la conta come "senza parere valido".
--
-- Confronto al momento del difetto:
--   functional_analyst  ... knowledge_search, advisory_verdict
--   sysadmin            ... knowledge_search, advisory_verdict
--   ui_ux_designer      ... knowledge_search, ui_layout_patterns, ui_reference_search
--
-- FIX: aggiungere advisory_verdict alla whitelist, senza toccare il resto.
-- L'UPDATE e' idempotente e non ricopia la lista: la ESTENDE solo se il tool
-- manca davvero (regola H: un fix che sopravvive a un re-apply e a un wipe+replay
-- delle migrazioni, non un UPDATE ad-hoc lanciato a mano).

UPDATE nexus_subagent_definitions
SET tool_whitelist = array_append(tool_whitelist, 'advisory_verdict'),
    updated_at = NOW()
WHERE kind = 'ui_ux_designer'
  AND NOT ('advisory_verdict' = ANY(tool_whitelist));
