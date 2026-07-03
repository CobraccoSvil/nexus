-- 0516: SCALE-CONTROLLER bidirezionale LLM-driven (FASE 0: fondamenta INERTI).
-- Terzo scope disgiunto della MetaReasonerPort (accanto a stall_recovery mig 0510
-- e orchestration_decide mig 0512): la scala-tier (light/medium/heavy) come PUNTO
-- UNICO pre-crisi. Dato un andamento del run STRUTTURATO (ScaleContext), decide una
-- ScaleMove (KeepTier / UpscaleTo / DownscaleTo) sul TIER astratto; il tier->modello
-- sano e' risolto a valle (PR-B) da select_agentic_model/best_model_for_tier.
--
-- Opt-in: agent.scale.enabled default 'false' -> con flag OFF (e nessun nodo/detector
-- in PR-A) il metodo assess_scale non e' MAI chiamato -> motore BIT-IDENTICO (vincolo
-- primario). L'impl della porta (mcp-core PgMetaReasonerPort::consult_scale_llm)
-- risolve il purpose 'scale_assess' e il template 'system.scale.assess'; se il flag
-- e' ON ma il purpose/template manca e' un misconfig (log ERROR), MAI un OFF
-- silenzioso (regola G).
--
-- FASE 0 (questa mig): purpose + template + settings INERTI. Nessun nodo, nessun
-- detector, nessuna emissione (quelli sono PR-B). current_tier viene checkpointato
-- ma non letto da alcun decisore -> bit-identico.

-- ── Purpose per lo scale-controller (tier-aware: 'medium'; il tier comanda sul
--    model_id statico di cortesia, regola G — internal_routing::resolve_purpose_model
--    ignora il model_id statico quando il tier e' valorizzato). Nessuna tool-use:
--    solo output JSON. Modello di cortesia coerente con stall_recovery/orchestration
--    (mig 0510/0512), gia' presente nel catalog seed (mig 0102).
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, requires_tool_use, notes)
VALUES (
    'scale_assess',
    'google',
    'gemini-2.5-flash',
    'medium',
    false,
    'Scale-controller bidirezionale: dato un andamento del run strutturato (ScaleContext: tier corrente, floor per intent, iterazioni/cap, pressione-contesto, streak errori, escalation, cost, capability), decide la scala-tier (enum chiuso ScaleMove: keep_tier/upscale_to/downscale_to su light/medium/heavy). L''LLM sceglie SOLO tier + confidence; i 5 gate deterministici (confidenza, banda-morta asimmetrica, cooldown, clamp 1 gradino, reversal-pin) sono applicati a valle e NON scavalcabili. Output JSON strutturato. Consultato solo a checkpoint (non a ogni iterazione).'
)
ON CONFLICT (purpose) DO NOTHING;

-- ── Prompt di decisione (canale FUORI-CHAT, regola D: autonomia/output/anti-loop
--    espliciti nel prompt). Output enum CHIUSO validato da
--    scale_reason::validate_scale_move (tag "move", snake_case). L'LLM NON scavalca
--    i gate deterministici (lo dice esplicitamente il prompt).
INSERT INTO nexus_prompt_templates (key, category, title, content, is_active, usage_context)
VALUES (
    'system.scale.assess',
    'system',
    'Scale-controller - scala-tier del modello (up/down)',
    $tmpl$<role>Sei il controller di scala-modello di un agente software autonomo. Durante un run osservi l'andamento e decidi UNA mossa: se il tier del modello (light/medium/heavy) e' adeguato, o se conviene salire (piu' potenza) o scendere (meno costo), come farebbe un tech lead che, visto come procede il lavoro, decide se serve un ingegnere piu' senior o se basta uno junior.</role>
<contesto>Ricevi un andamento del run STRUTTURATO (non prosa da interpretare): tier corrente (current_tier), pavimento per l'intento (intent_tier_floor, sotto cui NON si scende), intento utente, behavior_mode, iterazioni/cap e coda residua (tail_headroom), complessita' stimata, task_critical, pressione del contesto (context_pressure: low/medium/high), token stimati e headroom finestra, file modificati (files_modified_delta), todo chiusi (todos_closed), conteggio errori e streak senza errori (error_free_streak), azione ripetuta fallita, escalation gia' fatte, escalation_lock_active, costo speso/cap, capability richiesta, turni dall'ultimo cambio, inversioni (reversal_count). NON hai altro: decidi SOLO da questi segnali.</contesto>
<autonomia>Non chiedere conferma. Non spiegare a lungo. Emetti una sola mossa. Se i segnali non indicano un cambio chiaro, emetti "keep_tier" (nessun cambio: la rete di sicurezza).</autonomia>
<protocollo>
Ragiona sulla POTENZA adeguata al task, in questo ordine:
1. Segnali di difficolta' (errori che salgono, context_pressure alta, task_critical, complessita' alta, azione ripetuta fallita): considera "upscale_to" verso il tier immediatamente superiore. Salire e' facile (basta un segnale chiaro): un errore di upscale costa poco.
2. Segnali di run che fila liscio (pressione bassa, error_free_streak alta, todo chiusi, file modificati, zero escalation): SOLO allora considera "downscale_to" verso il tier immediatamente inferiore, per risparmiare. Scendere e' difficile (serve banda pulita STRETTA): un errore di downscale rischia il loop.
3. NON scendere MAI sotto intent_tier_floor. NON scendere se escalation_lock_active=true. NON scendere se task_critical=true e i margini non sono ampi.
4. In dubbio o con segnali contrastanti: "keep_tier".
</protocollo>
<anti_loop>NON scavalcare i gate: la tua mossa passa comunque per 5 gate deterministici (confidenza minima, banda-morta asimmetrica, cooldown post-cambio, clamp a 1 gradino per volta, reversal-pin). Non chiedere un salto di 2 tier in una volta (verrebbe clampato). Non oscillare: se hai gia' cambiato di recente, preferisci "keep_tier". La confidence deve riflettere la forza reale dei segnali, non essere gonfiata.</anti_loop>
<tool_usage>Non hai tool. Emetti solo la decisione JSON.</tool_usage>
<safety_progetto>Non inventare uno stato che i segnali non mostrano. Se i dati sono insufficienti, "keep_tier" (non forzare un cambio a vuoto). Preferisci sempre la sicurezza (tier piu' alto) quando il costo di sbagliare e' alto (task_critical, capability critica).</safety_progetto>
<output_format>Rispondi SOLO con JSON valido, una delle forme (campo "move" obbligatorio; tier in {light,medium,heavy}; confidence in [0,1]):
{"move":"keep_tier"}
{"move":"upscale_to","tier":"heavy","confidence":0.85}
{"move":"downscale_to","tier":"light","confidence":0.80}</output_format>
<reflection>Prima di rispondere: la mossa e' giustificata dai segnali strutturati (non da un'impressione)? Rispetta il floor e l'escalation_lock? La confidence e' onesta? In dubbio, "keep_tier". {{lang_hint}}</reflection>$tmpl$,
    true,
    'Consultato da PgMetaReasonerPort::consult_scale_llm (mcp-core) a checkpoint del run (SCALE-CONTROLLER, PR-B). Output validato da nexus-agent-graph::decisions::scale_reason::validate_scale_move (enum chiuso ScaleMove) e passato ai 5 gate deterministici di apply_hysteresis (l''LLM non li scavalca). PR-A: inerte (nessun nodo/detector lo consuma ancora).'
)
ON CONFLICT (key) DO NOTHING;

-- ── Config dello scale-controller (regola G: le soglie vivono nel DB, niente
--    fallback hardcoded nel codice). TUTTI default conservativi/OFF: con
--    agent.scale.enabled=false (default) il controller non e' mai attivo.
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.scale.enabled', 'false', 'agent',
   'Kill-switch dello scale-controller bidirezionale (up/down del tier modello). OFF (default) = nessuna valutazione, comportamento storico bit-identico.'),
  ('agent.scale.downscale_enabled', 'false', 'agent',
   'Abilita il DOWNSCALE (scendere di tier). Rollout: prima solo up-consolidation (downscale OFF), poi down su run lunghi/batch. OFF = solo upscale, mai downscale.'),
  ('agent.scale.eval_every_iters', '4', 'agent',
   'Cadenza (in iterazioni) di valutazione dello scale-controller. Chiave-cache: floor(iterations/N).'),
  ('agent.scale.min_tail_iters', '6', 'agent',
   'Gate break-even: coda residua minima (iteration_cap - iterations) per attivare il controller. Sotto la soglia non si valuta (costo netto zero su run corti).'),
  ('agent.scale.min_confidence', '0.70', 'agent',
   'Soglia di confidenza LLM sotto cui la mossa degrada a keep_tier (gate 1).'),
  ('agent.scale.change_cooldown_turns', '2', 'agent',
   'Turni di cooldown dopo un cambio-tier prima di consentirne un altro (gate 3, anti-ping-pong).'),
  ('agent.scale.downscale_clean_window', '3', 'agent',
   'Streak minima di iterazioni senza errori richiesta per il downscale (banda-morta asimmetrica, gate 2).'),
  ('agent.scale.max_reversals', '2', 'agent',
   'Inversioni di direzione sulla stessa coppia di tier oltre cui si PINNA al tier piu'' alto e si smette di consultare (gate 5, reversal-pin).'),
  ('agent.scale.max_tier_changes_per_run', '3', 'agent',
   'Cambi-tier massimi per run: al raggiungimento pinna heavy e disattiva (trigger di pin-up, non mute secco).'),
  ('agent.scale.max_evals_per_run', '6', 'agent',
   'Cap di consultazioni LLM dello scale-controller per run (budget/costo).'),
  ('agent.scale.window_overhead_ratio', '1.3', 'agent',
   'Overhead applicato a est_tokens per il vincolo finestra nel downscale (FIX-B): il tier target deve avere finestra >= est_tokens * ratio.'),
  ('agent.scale.timeout_s', '15', 'agent',
   'Timeout (s) della chiamata LLM dello scale-controller. Clamp 5-300 lato codice.')
ON CONFLICT (key) DO NOTHING;
