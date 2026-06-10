-- 0390_thinking_internal_text_disable.sql
-- Thinking OFF per i task interni TESTUALI (senza tool) sui modelli dual-mode.
--
-- Root cause (incidenti "deepseek non scrive" / hollow_completion): i modelli
-- dual-mode (deepseek-v4-pro/-flash, agentic_thinking_policy='disable_for_tools')
-- girano in thinking mode di DEFAULT. L'adapter spegneva il thinking SOLO nelle
-- richieste CON tool (ramo `if oai_tools:` in deepseek_provider.generate_agent_turn,
-- ADR 0025). Tutte le chiamate testuali dei task interni (purpose come chat title,
-- doc gen, conversation_summary, classifier, summarizer, next_actions — 41 purpose
-- non-agentici) lasciavano il thinking acceso: il budget di output finiva in
-- reasoning_content e il content tornava VUOTO (finish=length o end_turn vuoto),
-- mitigato solo dal cascade fallback (sintomo, non causa).
--
-- Verifica empirica (2026-06-10, probe API DeepSeek diretto): extra_body
-- {"thinking":{"type":"disabled"}} e' supportato ANCHE nelle richieste senza
-- tool: reasoning azzerato, risposta corretta, pochi token. (reasoning_effort
-- invece e' erratico sui v4: su v4-flash AUMENTA il reasoning -> non usato.)
--
-- Fix (regola G: criterio configurabile da DB, niente hardcode; regola L: punto
-- unico): il flag governa `should_disable_thinking` in
-- brain/providers/adapter_base.py. Per le chiamate marcate `internal_task=True`
-- (canali gRPC GenerateCompletion/GenerateAgentTurn — usati solo da task interni
-- mcp-core — REST /complete e nodi interni del brain) il thinking dei modelli
-- 'disable_for_tools' viene spento anche SENZA tool. La chat utente (executor
-- LangGraph) NON e' marcata internal_task: il reasoning resta attivo dove ha
-- valore per l'utente. Cache lato Python 60s.
--
-- Idempotente.

INSERT INTO settings (key, value, category, description)
VALUES (
    'providers.thinking_disable_internal_text',
    'true',
    'providers',
    'Se true, nelle chiamate TESTUALI (senza tool) dei task interni (purpose: chat title, doc gen, summary, classifier, ecc.) il thinking dei modelli dual-mode (agentic_thinking_policy=disable_for_tools, es. deepseek-v4) viene disabilitato via API. Evita che il budget di output bruci in reasoning_content producendo risposte vuote (hollow). Non tocca la chat utente. Cache 60s.'
)
ON CONFLICT (key) DO NOTHING;
