-- 0256_deepseek_v4_thinking_capability.sql
--
-- Root cause (confermato empiricamente con chiamata API reale):
-- i modelli DeepSeek V4 (deepseek-v4-flash, deepseek-v4-pro) girano in
-- "thinking mode" e l'API rifiuta un tool_choice forzato con
--   HTTP 400 invalid_request: "Thinking mode does not support this tool_choice".
-- La capability in nexus_provider_capabilities aveva pero' thinking=false, quindi
-- adapter_base.resolve_tool_choice forzava tool_choice="required" al primo turno
-- (tool_choice_style=openai_required + tool_choice_first_turn_force=true) ->
-- ogni turno agente con tool falliva con 400 -> DeepSeek non poteva eseguire
-- alcuna operazione file (file_ops) e chiudeva con "completamento vuoto".
--
-- Fix dato (questo file): allinea il flag thinking al comportamento reale del
-- modello. Combinato col fix di codice in adapter_base.resolve_tool_choice
-- (guard `not cap.thinking` sulla forzatura del primo turno), il tool_choice per
-- questi modelli degrada automaticamente ad "auto": il modello decide da se'
-- quando invocare i tool, senza forzatura, e l'API non risponde piu' 400.
--
-- Secondo dato errato corretto qui: max_context_tokens = 8192. Verificato
-- empiricamente che l'API DeepSeek V4 accetta un prompt da ~78.000 token senza
-- errori (prompt_tokens=78020), quindi 8192 era un placeholder che mutilava il
-- contesto agentico (system prompt + tool + history) facendo fallire i task
-- reali. Il context window ufficiale dei modelli DeepSeek e' 128K token; lo
-- allineiamo a 131072, coerente con l'evidenza empirica e la documentazione del
-- provider. default/hard output tokens restano invariati (8192/16384).
--
-- Niente UPDATE ad-hoc fuori migrazione (regola G/H del CLAUDE.md): la verita'
-- delle capability resta nel DB, veicolata da migrazione versionata.

UPDATE nexus_provider_capabilities
SET thinking = true,
    max_context_tokens = 131072,
    updated_at = now()
WHERE provider = 'deepseek'
  AND model IN ('deepseek-v4-flash', 'deepseek-v4-pro');
