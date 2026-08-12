-- 0703: igiene della configurazione token — la mappa torna a corrispondere al
-- territorio (censimento del 12/08/2026, docs/token-optimization.md).
--
-- Tre interventi, ognuno con la sua decisione dichiarata:
--
-- 1. DELETE dei quattro setting dell'offload del system prompt e della finestra
--    rolling, senza lettore Rust dal porting (erano del brain Python rimosso):
--      - agent.context.system_prompt_offload_threshold_tokens (0286)
--      - agent.context.system_prompt_summary_max_tokens       (0286)
--      - agent.context.system_offload_sections                (0404/0414)
--      - agent.context.rolling_window_turns                   (0286)
--    Una chiave che nessuno legge non e' una feature disattivata: e' una
--    promessa falsa nel pannello admin — chi la cambia crede di governare un
--    comportamento che non esiste. Se l'offload del system verra' portato in
--    Rust, le chiavi rinasceranno con la migrazione che ne cabla il lettore.
--
-- 2. NESSUNA DELETE per agent.context.dedup_tool_results_enabled e
--    agent.context.drop_unused_base64_age (0199): erano anch'esse senza
--    lettore, ma il comportamento che descrivono ESISTE — il wiring arriva
--    con questo stesso lavoro (ExecutorConfig + load in native_engine). Per
--    drop_unused_base64_age vince il valore del DB (3): il binario cablava 8
--    e le due verita' divergevano dal primo giorno.
--
-- 3. UPDATE di supports_prompt_cache dove il LEDGER la smentisce: il
--    censimento (xtask capability-census, prova_prompt_cache) ha misurato
--    9 coppie dichiarate false con letture di cache reali — la sola
--    mistral/mistral-small-latest ne aveva 2.461.120 su 152 chiamate.
--    Il criterio e' DERIVATO dai fatti (stessa query della prova), non un
--    elenco ricopiato: su un DB nuovo il ledger e' vuoto e l'UPDATE non tocca
--    nulla; sul META vivo corregge esattamente le coppie smentite. Idempotente.
--    La colonna resta senza lettori (lo dichiara il vocabolario capability):
--    questo allineamento evita che il primo lettore futuro nasca su un dato
--    falso, ed e' il primo passo del rimedio "collegarla o rimuoverla".

DELETE FROM settings
 WHERE key IN (
    'agent.context.system_prompt_offload_threshold_tokens',
    'agent.context.system_prompt_summary_max_tokens',
    'agent.context.system_offload_sections',
    'agent.context.rolling_window_turns'
 );

UPDATE nexus_provider_capabilities c
   SET supports_prompt_cache = TRUE,
       updated_at = now()
  FROM (
        SELECT provider, model
          FROM ai_usage_ledger
         WHERE status = 'finalized'
         GROUP BY provider, model
        HAVING SUM(cache_read_tokens) > 0
       ) prova
 WHERE c.provider = prova.provider
   AND c.model = prova.model
   AND c.supports_prompt_cache IS DISTINCT FROM TRUE;
