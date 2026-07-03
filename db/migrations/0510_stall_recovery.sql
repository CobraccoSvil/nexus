-- 0510: meta-reasoner LLM di recovery-da-stallo + costanti anti-loop portate in
-- DB (regola G). Eleva il "livello meta" del motore: la RISPOSTA allo stallo
-- passa da state-machine fissa (progress_controller GUIDE->DIAGNOSE->ESCALATE->
-- ABORT, nudge hardcoded) a ragionamento LLM contestuale, sul modello ADR 0036
-- (verify_infer). I detector strutturati di stallo restano il SEGNALE invariato
-- (regola M): cambia solo la risposta.
--
-- Opt-in: agent.stall_recovery.enabled default 'false' -> con flag OFF il motore
-- e' bit-identico a prima (fallback alla gerarchia fissa). L'impl della porta
-- (mcp-core PgMetaReasonerPort) risolve il purpose 'stall_recovery' e il
-- template 'system.stall_recovery.decide'; se il flag e' ON ma il purpose manca
-- e' un misconfig (log ERROR), MAI un OFF silenzioso.
--
-- Le costanti anti-loop (LOOP_THRESHOLD, RECENT_SIGNATURES_CAP, offset
-- forced-text, budget escalation) erano hardcoded nel codice: ora sono setting.
-- I valori seminati coincidono con le ex-costanti -> comportamento invariato.

-- ── Purpose per il meta-reasoner (tier-aware: 'medium'; il tier comanda sul
--    model statico di cortesia, regola G). Nessuna tool-use: solo output JSON.
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, requires_tool_use, notes)
VALUES (
    'stall_recovery',
    'google',
    'gemini-2.5-flash',
    'medium',
    false,
    'Meta-reasoner di recovery-da-stallo: data una situazione di stallo strutturata (StallContext), sceglie la prossima mossa strategica (enum chiuso RecoveryMove). Output JSON strutturato. Consultato solo quando un detector strutturato segnala stallo (non a ogni iterazione).'
)
ON CONFLICT (purpose) DO NOTHING;

-- ── Prompt di decisione (canale FUORI-CHAT, regola D: autonomia/output/anti-loop
--    espliciti nel prompt). Output enum CHIUSO validato da meta_reason::validate_move.
INSERT INTO nexus_prompt_templates (key, category, title, content, is_active, usage_context)
VALUES (
    'system.stall_recovery.decide',
    'system',
    'Recovery-da-stallo - decisione mossa strategica',
    $tmpl$<role>Sei il meta-ragionatore di un agente software autonomo. L'agente si e' BLOCCATO: ripete azioni senza progredire. Il tuo compito e' decidere UNA mossa strategica per sbloccarlo, come farebbe un ingegnere esperto che si accorge di girare a vuoto e cambia approccio.</role>
<contesto>Ricevi uno stato di stallo STRUTTURATO (non prosa): asse dello stallo, azione ripetuta e conteggio, esito strutturato dell'ultimo tool, escalation gia' fatte, cosa e' gia' stato tentato (guide/diagnose/strategy), se un tool ha rifiutato l'input per redazione, quante volte la stessa domanda e' gia' stata posta all'utente, file modificati. NON hai la storia completa: decidi SOLO da questi segnali.</contesto>
<autonomia>Non chiedere conferma. Non spiegare a lungo. Emetti una sola mossa. Se i segnali non bastano per una mossa mirata, scegli "fallback" (l'agente ricadra' sulla gerarchia fissa di sicurezza).</autonomia>
<protocollo>
Scegli la mossa MINIMA che ha piu' probabilita' di sbloccare, in questo ordine di preferenza:
1. Se l'agente non ha ancora provato a cambiare APPROCCIO (already_strategy_shifted=false) e il problema sembra di metodo, non di capacita': "shift_strategy" con un nudge che riorienta (strumento diverso, passo piu' piccolo, piu' contesto).
2. Se un'azione ripetuta FALLISCE davvero (repeated_action_*_failed / last_tool_outcome=error) e non e' ancora stata diagnosticata: "force_diagnose" con un nudge che ordina di leggere l'errore e dichiarare la causa radice.
3. Se redaction_rejected=true OPPURE repeated_clarify_count elevato: l'agente sta sbattendo su un dato OSCURATO o ha gia' ri-chiesto lo stesso dato all'utente. NON far ri-chiedere: se il dato oscurato basta come opaco, "shift_strategy" (usalo cosi' com'e', passalo al tool senza interpretarlo); se manca un dato davvero indispensabile e diverso e already_asked_user=false, "ask_user" con UNA domanda precisa; se already_asked_user=true, "declare_blocked" con blocker appropriato.
4. Se guide+strategy+diagnose sono gia' stati tentati e c'e' budget escalation (escalations < max_escalations): "escalate_model".
5. Se tutto e' stato tentato e non c'e' via d'uscita: "declare_blocked" con il blocker giusto.
6. Altrimenti "continue_guided" con un nudge assertivo, o "fallback".
</protocollo>
<anti_loop>NON proporre "ask_user" se already_asked_user=true (ri-chiedere e' il loop stesso). NON proporre "escalate_model" se escalations>=max_escalations. Un dato che appare "[REDACTED:...]" o placeholder NON e' un valore da chiedere all'utente: e' opaco per te, trattalo come tale.</anti_loop>
<tool_usage>Non hai tool. Emetti solo la decisione JSON.</tool_usage>
<output_format>Rispondi SOLO con JSON valido, una delle forme:
{"move":"continue_guided","nudge":"<istruzione assertiva>"}
{"move":"shift_strategy","nudge":"<come cambiare approccio>"}
{"move":"force_diagnose","nudge":"<ordine di diagnosticare la causa>"}
{"move":"escalate_model"}
{"move":"ask_user","question":"<UNA domanda precisa>"}
{"move":"declare_blocked","blocker":"<uno di: dependency|credential|permission|service|request_ambiguity|safety>"}
{"move":"fallback"}</output_format>
<safety_progetto>Non suggerire MAI di fabbricare/inserire dati fittizi (INSERT di utenti/record) per "far passare" una verifica: e' falsare il risultato. Se manca un dato reale, "declare_blocked".</safety_progetto>
<reflection>Prima di rispondere: la mossa evita di ripetere cio' che e' gia' fallito? Rispetta i cap (already_asked_user, escalations)? Il blocker, se usato, e' nel vocabolario? {{lang_hint}}</reflection>$tmpl$,
    true,
    'Consultato da PgMetaReasonerPort (mcp-core) nel nodo StallRecovery quando un detector strutturato segnala stallo. Output validato da nexus-agent-graph::decisions::meta_reason::validate_move (enum chiuso RecoveryMove) e tradotto in ProgressDecision.'
)
ON CONFLICT (key) DO NOTHING;

-- ── Config del meta-reasoner (regola G). Default OFF: rollout graduale.
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.stall_recovery.enabled', 'false', 'agent',
   'Abilita il meta-reasoner LLM di recovery-da-stallo. OFF = gerarchia fissa (comportamento storico).'),
  ('agent.stall_recovery.timeout_s', '20', 'agent',
   'Timeout (s) della chiamata LLM del meta-reasoner. Clamp 5-300 lato codice.'),
  ('agent.stall_recovery.max_moves_per_session', '6', 'agent',
   'Budget di consultazioni del meta-reasoner per SESSIONE (non per-run): oltre, si ricade sulla gerarchia fissa (ABORT).')
ON CONFLICT (key) DO NOTHING;

-- ── Costanti anti-loop ex-hardcoded portate in DB (regola G). Valori = ex
--    costanti -> comportamento invariato. Punto unico del budget escalation.
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.executor.max_escalations', '3', 'agent',
   'Budget massimo di escalation del run (ex literal 3): cap unico condiviso da progress_controller e auto-escalation al signature-loop.'),
  ('agent.executor.forced_text_offset', '5', 'agent',
   'Offset sottratto a iteration_cap per la soglia forced-text (ex costante iteration_cap-5).'),
  ('agent.loop.signature_threshold', '3', 'agent',
   'Occorrenze della stessa signature di tool oltre cui e'' loop (ex costante LOOP_THRESHOLD=3).'),
  ('agent.loop.recent_signatures_cap', '12', 'agent',
   'Cap della coda di signature mantenute per la loop-detection (ex costante RECENT_SIGNATURES_CAP=12).'),
  ('agent.loop.repeated_user_question_threshold', '2', 'agent',
   'Occorrenze della stessa domanda-di-chiarimento nella sessione (cross-run) oltre cui scatta l''asse RepeatedUserQuestion.'),
  ('agent.loop.max_ask_user_per_session', '1', 'agent',
   'Numero massimo di domande-di-chiarimento che il meta-reasoner puo'' porre per sessione prima di forzare declare_blocked.')
ON CONFLICT (key) DO NOTHING;
