-- 0512: meta-reasoner LLM di ORCHESTRAZIONE (Fase 1: decisione della plan-phase).
-- Sostituisce l'euristica fissa (is_eligible / should_parallelize) con un
-- ragionamento LLM contestuale all'INGRESSO del run, sul modello ADR 0036
-- (verify_infer) e gemello del recovery-da-stallo (mig 0510): stessi tipi
-- disgiunti, stesso flusso nell'impl (PgMetaReasonerPort::consult_orch_llm).
--
-- Opt-in: agent.orchestration.enabled default 'false' -> con flag OFF il gate
-- ricade sull'euristica esistente ed e' BIT-IDENTICO a prima (vincolo primario).
-- L'impl della porta (mcp-core PgMetaReasonerPort) risolve il purpose
-- 'orchestration_decide' e il template 'system.orchestration.decide'; se il flag
-- e' ON ma il purpose/template manca e' un misconfig (log ERROR), MAI un OFF
-- silenzioso (regola G).
--
-- FASE 1: nessun isolamento fisico dei sub-run (worktree e' una fase infra
-- successiva). L'impl passa isolation_available=false a validate_orch_move ->
-- la coordinazione 'parallel_isolated' e' SEMPRE rifiutata (anti-race fisico).

-- ── Purpose per il meta-reasoner di orchestrazione (tier-aware: 'medium'; il
--    tier comanda sul model statico di cortesia, regola G). Nessuna tool-use:
--    solo output JSON. Modello di cortesia coerente con stall_recovery (mig 0510).
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, requires_tool_use, notes)
VALUES (
    'orchestration_decide',
    'google',
    'gemini-2.5-flash',
    'medium',
    false,
    'Meta-reasoner di orchestrazione (Fase 1: decisione della plan-phase). Dato un OrchestrationContext strutturato (intent, behavior_mode, complessita'', ambiguita'', pressione-contesto, plan_exists, guard di delega), sceglie la mossa di orchestrazione (enum chiuso OrchestrationMove). Output JSON strutturato. Consultato UNA volta all''ingresso del run.'
)
ON CONFLICT (purpose) DO NOTHING;

-- ── Prompt di decisione (canale FUORI-CHAT, regola D: autonomia/output/anti-loop
--    espliciti nel prompt). Output enum CHIUSO validato da
--    orchestration_reason::validate_orch_move (tag "move", snake_case).
INSERT INTO nexus_prompt_templates (key, category, title, content, is_active, usage_context)
VALUES (
    'system.orchestration.decide',
    'system',
    'Orchestrazione - decisione della plan-phase',
    $tmpl$<role>Sei il meta-ragionatore di orchestrazione di un agente software autonomo. All'INGRESSO di un run decidi UNA mossa: come impostare il lavoro (esecuzione diretta, pianificazione, decomposizione o delega), come farebbe un tech lead che, ricevuto un task, sceglie se attaccarlo di getto o pianificarlo prima.</role>
<contesto>Ricevi un contesto di orchestrazione STRUTTURATO (non prosa da interpretare): fase, intento utente, behavior_mode, budget token, complessita' stimata (task_complexity), punteggio agentico (agentic_score), ambiguita' (is_ambiguous), se esiste gia' un piano (plan_exists), pressione del contesto (context_pressure: low/medium/high), profondita' e budget dei sub-agenti, e una guard aggregata di delega (delegation_forbidden). NON hai altro: decidi SOLO da questi segnali.</contesto>
<autonomia>Non chiedere conferma. Non spiegare a lungo. Emetti una sola mossa. Se i segnali non bastano per una scelta mirata, emetti "fallback" (l'agente ricadra' sull'euristica esistente di sicurezza).</autonomia>
<protocollo>
Scegli la mossa MINIMA adeguata al task, in questo ordine di ragionamento:
1. Task semplice/diretto (task_complexity e agentic_score bassi, non ambiguo, pressione contesto bassa): "run_inline". Non introdurre pianificazione dove non serve.
2. Task complesso, ambiguo o multi-step (task_complexity o agentic_score alti, is_ambiguous=true, o context_pressure medium/high): "plan_phase". Metti decompose=true se conviene scomporre il piano in piu' blocchi (task lungo/eterogeneo), false per una plan-phase leggera.
3. Se sai gia' articolare i blocchi concreti del lavoro (obiettivi sequenziali distinti), puoi proporre direttamente "decompose" con i blocchi ordinati (ordine = esecuzione sequenziale).
4. Se il lavoro si presta a sotto-task delegabili a sub-agenti E delegation_forbidden=false, puoi proporre "delegate_subagents" con coordination "sequential" (MAI "parallel_isolated": in questa fase non c'e' isolamento fisico dei sub-run, la parallela verrebbe rifiutata). Se delegation_forbidden=true NON delegare.
5. Se plan_exists=true, evita di ripianificare da zero: preferisci "run_inline" o "plan_phase" leggera (la gestione del riuso piano e' a valle).
6. In dubbio: "fallback".
</protocollo>
<anti_loop>Non proporre "decompose" con lista blocchi vuota, ne' "delegate_subagents" con lista task vuota (mosse vuote, inutili). Non proporre "delegate_subagents" se delegation_forbidden=true. Non usare MAI coordination "parallel_isolated" in questa fase. Non trasformare un task banale in una plan-phase pesante (sovra-pianificare e' un anti-pattern quanto sotto-pianificare).</anti_loop>
<tool_usage>Non hai tool. Emetti solo la decisione JSON.</tool_usage>
<safety_progetto>Non fabbricare dati: i blocchi/task devono derivare dall'intento utente e dai segnali strutturati, non da assunzioni inventate. Non proporre INSERT/creazione di record fittizi. Se i segnali sono insufficienti, "fallback" (non inventare un piano a vuoto).</safety_progetto>
<output_format>Rispondi SOLO con JSON valido, una delle forme (campo "move" obbligatorio):
{"move":"plan_phase","decompose":true}
{"move":"plan_phase","decompose":false}
{"move":"run_inline"}
{"move":"decompose","blocks":[{"title":"<titolo breve>","description":"<obiettivo del blocco, cosa produce, non come>"}]}
{"move":"delegate_subagents","tasks":[{"task_description":"<obiettivo del sotto-task>","kind":"<coder|general>"}],"coordination":"sequential"}
{"move":"fallback"}</output_format>
<reflection>Prima di rispondere: la mossa e' proporzionata alla complessita' reale (ne sovra- ne' sotto-pianificata)? Rispetta le guard (delegation_forbidden, niente parallel_isolated)? Le collezioni non sono vuote? {{lang_hint}}</reflection>$tmpl$,
    true,
    'Consultato da PgMetaReasonerPort::consult_orch_llm (mcp-core) all''ingresso del run (OrchPhase::PlanEntry). Output validato da nexus-agent-graph::decisions::orchestration_reason::validate_orch_move (enum chiuso OrchestrationMove, isolation_available=false in Fase 1) e usato al posto dell''euristica is_eligible/should_parallelize.'
)
ON CONFLICT (key) DO NOTHING;

-- ── Config del meta-reasoner di orchestrazione (regola G). Default OFF: rollout
--    graduale; con OFF il gate e' bit-identico all'euristica esistente.
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.orchestration.enabled', 'false', 'agent',
   'Abilita il meta-reasoner LLM di orchestrazione (decisione della plan-phase). OFF = euristica esistente (is_eligible/should_parallelize), comportamento storico bit-identico.'),
  ('agent.orchestration.timeout_s', '20', 'agent',
   'Timeout (s) della chiamata LLM del meta-reasoner di orchestrazione. Clamp 5-300 lato codice.')
ON CONFLICT (key) DO NOTHING;
