-- 0658_classificatore_giudizio_semantico.sql
--
-- Chiude 4 dei 29 confronti semantici del censimento del 29/07/2026: punti in
-- cui il codice indovinava dal TESTO (liste di keyword, sottostringhe, conteggi)
-- una domanda che e' genuinamente semantica, mentre il classificatore che gira
-- gia' su ogni turno (`intent_classifier::classify`, mcp-core) restava
-- inutilizzato per quella stessa domanda. Il codice Rust e' esteso nello stesso
-- commit; questa migrazione porta il template del prompt e i parametri
-- operativi allo stesso passo (regola G: niente hardcoded, il DB e' la fonte).
--
-- 1) `routing_slots.rs::infer_slots_heuristic` (8 liste di keyword) rimossa:
--    fabbricava confidence=0.65 SOPRA la soglia 0.60 del consumatore per
--    costruzione. La soglia stessa ora ha una chiave propria
--    (`routing.slots_min_confidence`) invece di un letterale nel call site.
-- 2) `chat_messages::intent::detect_model_switch` (5 liste provider + 18 verbi)
--    rimossa: leggeva "e' un comando?" dal testo, e una DOMANDA su un modello
--    ("perche' gemini risponde male") veniva consumata come switch. Il giudizio
--    e' ora un campo strutturato del classificatore (`model_switch`).
-- 3) `profiles::auto_select_profile` (conteggio sottostringhe, slice a 300 BYTE
--    su UTF-8 — panicava su un accento) sostituita da richiamo semantico
--    (embedder in-process, soglia propria `orchestrator.profile_auto_select_min_similarity`).
--    Codice Rust puro, nessuna chiave di prompt da toccare qui.
-- 4) `agent_tools::subagent_native::select_council_figures` accetta ora le
--    competenze dichiarate dal classificatore (validate contro il roster
--    figure) al posto delle keyword d'ambito, che restano SOLO ripiego per
--    classifier_resolved==false. Corollario: la keyword `certificat` di mig
--    0553 e' morta dal passaggio al match a parola intera (mig 0650:
--    `touches_domain_keyword`) — "certificat" non e' mai una parola intera in
--    italiano, sempre "certificato/certificati/certificazione".
--
-- Il vincolo trasversale (dal censimento): il ripiego dev'essere ONESTO. Sotto
-- soglia o senza vocabolario iniettato, i nuovi campi restano `None`/assenti e
-- i consumatori sanno di dover ripiegare — mai una stima che scavalca la soglia
-- di chi la consuma.

-- ── 1) Soglia propria per gli slot di routing (era un letterale 0.60 nel
--       call site `resolve_slot_routing_hit`) ─────────────────────────────────
INSERT INTO settings (key, value, category, description, is_secret) VALUES
  ('routing.slots_min_confidence', '0.60', 'routing',
   'Soglia di confidence sotto cui gli slot d''azione del classificatore (action_verb/target_type/framework/scope) NON guidano il routing e si torna al percorso classico (intent, behavior_mode). Letta da Orchestrator::route_by_slots, punto unico (prima era un letterale duplicabile nel call site).',
   false)
ON CONFLICT (key) DO NOTHING;

-- ── 3) Soglia di similarita' per la selezione automatica del profilo
--       (richiamo semantico, sostituisce il conteggio di sottostringhe) ──────
INSERT INTO settings (key, value, category, description, is_secret) VALUES
  ('orchestrator.profile_auto_select_min_similarity', '0.55', 'orchestrator',
   'Soglia di similarita'' coseno (embedding richiesta vs embedding profilo: nome+descrizione+system_prompt) sotto cui nessun profilo e'' considerato pertinente in modalita'' "auto" (profile_selection::select_best_profile). Sotto soglia si ritorna il default esplicito, mai il candidato meno-peggio.',
   false)
ON CONFLICT (key) DO NOTHING;

-- ── 2) Estende lo schema del classificatore (mig 0447) con `model_switch` e
--       `competencies` — stessa chiamata, stesso schema, campi in piu' ──────
UPDATE nexus_prompt_templates
   SET content = replace(
           content,
           E'  "confidence": 0.0..1.0\n}}\n}}\n\nIntent meaning:',
           E'  "confidence": 0.0..1.0\n}},\n"model_switch": {{"is_switch": bool, "provider": "provider slug or \\"\\"", "model": "model id or \\"\\""}},\n"competencies": [one or more of: {competenze}]\n}}\n\nIntent meaning:'
       ),
       updated_at = NOW(),
       updated_by = 'migration_0658'
 WHERE key = 'system.intent_classifier_prompt'
   AND strpos(content, E'  "confidence": 0.0..1.0\n}}\n}}\n\nIntent meaning:') > 0
   AND strpos(content, 'model_switch') = 0;

UPDATE nexus_prompt_templates
   SET content = replace(
           content,
           E'- When unsure, prefer true (do not block legitimate fixes).\n\nUse confidence<0.7 honestly when ambiguous (downstream asks user). NEVER inflate.',
           E'- When unsure, prefer true (do not block legitimate fixes).\n\n"model_switch" -- is this message a CONFIGURATION COMMAND that changes which provider/model answers, or is it WORK to do?\n- is_switch=true ONLY for an explicit instruction to switch: "usa claude", "passa a gemini 2.5 pro", "switch to gpt-4o", "rispondi con mistral". Fill "provider" with the vendor slug (anthropic|openai|google|mistral|deepseek|...) and "model" with the model id if the user named one, "" otherwise.\n- is_switch=false when the message merely MENTIONS a model or vendor while asking for work or an explanation: "voglio capire perche'' gemini risponde male", "confronta claude e gpt", "il modello mistral va in timeout, indaga", "aggiungi il supporto a openai nel codice". These are tasks, not settings.\n- WHEN IN DOUBT, is_switch=false. Mistaking a request for a switch SWALLOWS the user''s task; mistaking a switch for a request only costs one turn.\n\n"competencies" -- which professional lenses does this task actually need, from the closed list in the schema above. Empty list [] when the task needs none in particular. Use ONLY names from that list; never invent one.\n\nUse confidence<0.7 honestly when ambiguous (downstream asks user). NEVER inflate.'
       ),
       updated_at = NOW(),
       updated_by = 'migration_0658'
 WHERE key = 'system.intent_classifier_prompt'
   AND strpos(content, E'- When unsure, prefer true (do not block legitimate fixes).\n\nUse confidence<0.7 honestly when ambiguous (downstream asks user). NEVER inflate.') > 0
   AND strpos(content, '"model_switch" --') = 0;

-- ── 4) `certificat` non e' mai una parola intera: dal match a sottostringa
--       (mig 0553) al match a parola intera (mig 0650) e' diventata muta.
--       Sostituita dalle sue forme italiane reali. ──────────────────────────
UPDATE settings
   SET value = replace(value, ',certificat,', ',certificato,certificati,certificazione,'),
       updated_at = NOW()
 WHERE key = 'orchestrator.council_infra_keywords'
   AND value LIKE '%,certificat,%';
