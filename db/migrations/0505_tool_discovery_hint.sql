-- Migrazione 0505: aggiunge la sezione <tool_discovery> ai system prompt
-- coder/base per indirizzare l'agente verso i tool di verifica (ADR 0019 L4),
-- in particolare il nuovo `nexus_verify_change` (mig 0503, ADR 0019 L3).
--
-- Chiude il pattern osservato negli incidenti: l'agente dichiara "non ho tool
-- per eseguire i test" senza aver cercato nel catalogo, oppure chiude un task
-- di codice senza alcuna verifica oggettiva.
--
-- Sentinel string `ADR0019_TOOL_DISCOVERY_v1` garantisce idempotenza su re-run.

DO $$
DECLARE
    sentinel TEXT := '<!-- ADR0019_TOOL_DISCOVERY_v1 -->';
    block TEXT := E'\n\n<!-- ADR0019_TOOL_DISCOVERY_v1 -->\n<tool_discovery>\nQuando devi VERIFICARE il lavoro fatto su codice:\n- VERIFICA COMPLETA prima di dichiarare concluso un task di codice: `nexus_verify_change` con scope=''full'' (typecheck -> build -> lint -> test, si ferma al primo errore, esito strutturato).\n- CHECK RAPIDO dopo una modifica: `nexus_verify_change` con scope=''quick'' (typecheck+lint).\n- SOLO TEST: `nexus_verify_change` scope=''test'', oppure `run_tests` (Playwright/suite del progetto) o `run_specific_test` per un singolo test.\n- COMANDO PUNTUALE: `run_command` per un comando one-shot specifico.\n\nNON dichiarare mai "non ho tool per eseguire test/build" senza aver prima cercato nel catalogo (`nexus_mcp_tool_search`) — i tool di verifica esistono. NON dichiarare `task_complete` con outcome=done su un task di codice senza aver eseguito almeno una verifica oggettiva (nexus_verify_change o run_tests con esito verde).\n</tool_discovery>';
BEGIN
    UPDATE nexus_prompt_templates
    SET content = content || block
    WHERE key IN ('system.nexus_base', 'agent.coder.base')
      AND is_active = TRUE
      AND content NOT LIKE '%' || sentinel || '%';

    RAISE NOTICE 'Migrazione 0505 applicata: tool_discovery ADR 0019 L4 iniettata nei system prompt';
END
$$;
