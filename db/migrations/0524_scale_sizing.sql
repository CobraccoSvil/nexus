-- 0524: SIZING AGENTICO — terza direzione dello SCALE-CONTROLLER (opt-in, OFF).
--
-- Rende ADATTIVE (non piu' soglie fisse geometriche) le decisioni di DIMENSIONAMENTO
-- del motore: compress phases/keep_recent/max_chars + compress_start_iter, freno
-- token (max_context_ratio), rolling_summary on/off + keep_recent, soglia g1-loop.
-- Riusa la porta MetaReasonerPort::assess_scale + il nodo ScaleControl gia' esistenti
-- (mig 0516): la stessa consultazione decide il TIER (up/down) O il SIZING (postura).
--
-- Pattern (gemello del tier): l'LLM sceglie SOLO una POSTURA bounded
-- (compact/relax/hold) + confidence (regola M); la traduzione in soglie CONCRETE e'
-- DETERMINISTICA e PROPORZIONALE ai segnali (durata run, crescita history, rumore
-- tool_result, progresso), clampata a bound invarianti. Gate dedicato (kill-switch
-- nested + confidenza + cooldown anti-thrash) separato dai 5 gate tier.
--
-- Opt-in NESTED: agent.scale.sizing_enabled default 'false'. Con scale ON ma sizing
-- OFF il detector NON popola i segnali sizing (omessi dal JSON all'LLM), l'addendum
-- NON e' appeso al prompt (prompt byte-identico al pre-0524) e il gate degrada ogni
-- adjust_sizing a keep_tier -> flusso TIER BIT-IDENTICO. Con scale OFF: inerte come
-- oggi. L'unico "magic value" ammesso e' assente: le soglie vengono dal DB (regola G).

-- ── Settings del sizing (regola G: soglie nel DB, niente fallback hardcoded) ──────
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.scale.sizing_enabled', 'false', 'agent',
   'Kill-switch NESTED del sizing agentico (mig 0524). OFF (default) = con scale ON il flusso tier resta bit-identico (nessun segnale sizing, nessun addendum, gate degrada adjust_sizing a keep_tier). ON = lo scale-controller puo'' adattare le soglie di dimensionamento (compress/token_brake/rolling/g1-loop) via postura.'),
  ('agent.scale.sizing_cooldown_turns', '3', 'agent',
   'Turni minimi tra due cambi di POSTURA di sizing (anti-thrash del sizing, DISTINTO dal cooldown tier change_cooldown_turns).'),
  ('agent.scale.sizing_aggressiveness', '0.5', 'agent',
   'Quanto una postura spinge le soglie di dimensionamento, in [0,1] (0 = quasi neutro, 1 = spinta massima entro i bound invarianti dell''algoritmo). UNICA manopola DB del trasformatore proporzionale; floor/ceil (keep_recent, max_chars, freno, moltiplicatore g1) sono invarianti nel codice.')
ON CONFLICT (key) DO NOTHING;

-- ── Addendum template (mig 0524): descrive la mossa `adjust_sizing`. APPESO al
--    template base `system.scale.assess` (mig 0516) dall'adapter SOLO quando
--    agent.scale.sizing_enabled=true (con OFF il prompt resta byte-identico al
--    pre-0524). Canale FUORI-CHAT (regola D): autonomia/output/anti-loop espliciti.
INSERT INTO nexus_prompt_templates (key, category, title, content, is_active, usage_context)
VALUES (
    'system.scale.assess.sizing',
    'system',
    'Scale-controller - addendum SIZING (dimensionamento adattivo)',
    $tmpl$<sizing>
OLTRE al tier (upscale_to/downscale_to/keep_tier) puoi decidere il DIMENSIONAMENTO del motore per QUESTO run, con la mossa "adjust_sizing". Non cambia il modello: cambia quanto contesto il motore tiene vivo e quanto comprime. Come un tech lead che, visto quanto e' cresciuto e quanto e' rumoroso il thread di lavoro, decide se stringere la gestione del contesto o allentarla.

Ricevi in aggiunta questi segnali (presenti solo quando il sizing e' attivo): history_size (numero messaggi), history_growth_rate (messaggi per iterazione), tool_result_noise (caratteri del piu' grande output recente), effective_window (finestra del modello), recent_productive (il run sta compiendo azioni utili).

Scegli UNA postura:
- "compact": il contesto e' sotto pressione (est_tokens/token_headroom_ratio alti vicino a effective_window), la history cresce in fretta (history_growth_rate alto) o e' rumorosa (tool_result_noise alto). Comprimi prima e piu' forte, attiva il rolling-summary, stringi il freno token. Se recent_productive=true dai piu' respiro alla soglia di loop (non interrompere un modello che avanza).
- "relax": il run fila con finestra ampia (pressione bassa, headroom largo) e senza rumore. Allenta la compressione (piu' contesto vivo), rolling-summary off. Piu' fedelta', meno aggressivita'.
- "hold": segnali non chiari o contrastanti -> nessun cambio (rete di sicurezza).

NON scegli tu i numeri: dichiari solo la postura + confidence; il motore traduce in soglie concrete in modo deterministico, proporzionale ai segnali e clampato a bound sicuri. La tua mossa passa comunque per un gate (confidenza minima, cooldown anti-oscillazione): non oscillare, se hai appena cambiato postura preferisci "hold". Emetti "adjust_sizing" SOLO se una direzione di dimensionamento e' chiaramente giustificata dai segnali; altrimenti resta su una decisione di tier o "keep_tier".
</sizing>
<output_format_sizing>Se decidi il dimensionamento, rispondi SOLO con JSON valido nella forma (postura in {compact,relax,hold}; confidence in [0,1]):
{"move":"adjust_sizing","posture":"compact","confidence":0.80}</output_format_sizing>$tmpl$,
    true,
    'Addendum appeso a system.scale.assess da PgMetaReasonerPort::consult_scale_llm (mcp-core) SOLO se agent.scale.sizing_enabled=true. Output validato da nexus-agent-graph::decisions::scale_reason::validate_scale_move (variante ScaleMove::AdjustSizing) e passato al gate dedicato apply_sizing_gate (kill-switch nested + confidenza + cooldown). La traduzione postura->soglie e'' deterministica (resolve_sizing_overrides).'
)
ON CONFLICT (key) DO NOTHING;
