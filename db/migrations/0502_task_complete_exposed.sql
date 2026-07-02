-- 0502: esposizione del tool task_complete al modello (ADR 0034).
--
-- Root cause: il tool task_complete (dichiarazione strutturata dell'esito,
-- WAVE 3 / ADR 0034) aveva l'handler nel grafo nativo (tool_dispatch) e il
-- consumo nel routing (declared_outcome), ma NON era definito in nessun
-- catalogo esposto al modello: era un tool del brain Python (rimosso, commit
-- 75a6d62) mai portato in AGENT_TOOLS_JSON. Risultato: zero chiamate a
-- task_complete in tutti i run del motore nativo (verificato su
-- beaty_book_nexus), e l'esito dei run letto SOLO dalle euristiche
-- lessicali/strutturali che l'ADR 0034 vuole sostituire.
--
-- La definizione del tool ora vive in AGENT_TOOLS_JSON (fonte unica degli
-- schema, crates/nexus-agent-tools/src/tool_schema.rs). Questa migrazione
-- aggiunge task_complete alle whitelist DB-driven che filtrano il catalogo
-- (senza, il tool resterebbe strippato: discovery-first e' attivo di default):
--
--   agent.tools.discovery_first_whitelist  set iniziale discovery-first (M16)
--   agent.tools.core_whitelist             set core discovery on-demand
--   agent.tools.inline_core_whitelist      set inline ADR 0016 fase A.2
--   automation.o_series_essential_tools    set ridotto modelli o-series
--   automation.study_mode_readonly_tools   whitelist study mode: dichiarare
--                                          l'esito e' read-only (nessun
--                                          side-effect sul progetto)
--
-- Append idempotente: se la chiave non contiene gia' task_complete.

UPDATE settings
   SET value = value || ',task_complete'
 WHERE key IN (
         'agent.tools.discovery_first_whitelist',
         'agent.tools.core_whitelist',
         'agent.tools.inline_core_whitelist',
         'automation.o_series_essential_tools',
         'automation.study_mode_readonly_tools'
       )
   AND value NOT LIKE '%task_complete%';
