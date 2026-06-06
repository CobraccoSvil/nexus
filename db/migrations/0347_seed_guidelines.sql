-- 0347_seed_guidelines.sql
--
-- Seed iniziale della knowledge base direttive (nexus_prompt_guideline).
-- Tutte inserite con is_active=FALSE (pending): diventano operative solo dopo
-- approvazione esplicita dell'admin (valorizzazione di approved_by), coerente
-- con la decisione "KB versionata e approvata dall'admin".
--
-- Due famiglie:
--   - best practice di prompt engineering (source='official_docs'/'cookbook')
--   - regole interne del progetto (source='internal_rule', sezioni A/D + lingua)
--
-- check_hint e' l'istruzione operativa passata al valutatore LLM
-- (brain POST /agent/prompt-revise) per assegnare il punteggio della dimensione.
--
-- Idempotente: ON CONFLICT (practice_key, version) DO NOTHING.

INSERT INTO nexus_prompt_guideline
    (practice_key, source, source_url, description, check_hint, severity, applies_to, version, is_active)
VALUES
  ('xml_schema', 'official_docs', 'https://docs.claude.com/en/docs/build-with-claude/prompt-engineering/use-xml-tags',
   'Struttura il prompt con lo schema XML standard a 9 tag.',
   'Il prompt deve contenere i tag <role>, <contesto>, <autonomia>, <protocollo>, <tool_usage>, <anti_loop>, <output_format>, <examples>, <reflection>. Penalizza l''assenza di ciascun tag; gravita'' massima se mancano <role> o <output_format>.',
   'must', 'agent', 1, FALSE),

  ('few_shot', 'official_docs', 'https://docs.claude.com/en/docs/build-with-claude/prompt-engineering/multishot-prompting',
   'Includi esempi few-shot concreti e realistici.',
   'Verifica che <examples> contenga almeno un esempio realistico input->output pertinente al compito, non placeholder vuoti o generici.',
   'should', 'all', 1, FALSE),

  ('chain_of_thought', 'official_docs', 'https://docs.claude.com/en/docs/build-with-claude/prompt-engineering/chain-of-thought',
   'Fai ragionare il modello prima di produrre la risposta finale.',
   'Verifica che il prompt inviti a un ragionamento esplicito (protocollo step-by-step o passo di reflection) prima dell''output finale.',
   'should', 'all', 1, FALSE),

  ('role_system', 'official_docs', 'https://docs.claude.com/en/docs/build-with-claude/prompt-engineering/system-prompts',
   'Definisci ruolo e contesto nel system prompt.',
   'Verifica che il prompt apra con un <role> chiaro e un <contesto> che inquadri compito, vincoli e ambiente operativo.',
   'must', 'all', 1, FALSE),

  ('prefill', 'official_docs', 'https://docs.claude.com/en/docs/build-with-claude/prompt-engineering/prefill-claudes-response',
   'Usa il prefill della risposta assistant per output strutturati.',
   'Per output JSON puro, valuta se il prompt predispone un prefill della risposta (apertura della struttura attesa) per ridurre preamboli. Pratica opzionale.',
   'nice', 'all', 1, FALSE),

  ('output_format', 'official_docs', 'https://docs.claude.com/en/docs/build-with-claude/prompt-engineering/be-clear-and-direct',
   'Specifica il formato di output e i criteri di successo.',
   'Verifica che <output_format> definisca chiaramente la forma attesa dell''output (schema/JSON/struttura) e i criteri di completamento del compito.',
   'should', 'all', 1, FALSE),

  ('prompt_chaining', 'cookbook', 'https://github.com/anthropics/claude-cookbooks/tree/main/patterns/agents',
   'Scomponi i compiti complessi in catene di prompt coerenti.',
   'Per compiti multi-step, valuta se il prompt si inserisce in una catena coerente (plan->act->verify) anziche'' essere un monolite. Pratica architetturale.',
   'nice', 'all', 1, FALSE),

  ('self_check_reflection', 'official_docs', 'https://docs.claude.com/en/docs/build-with-claude/prompt-engineering/chain-of-thought',
   'Includi un passo di auto-verifica/reflection finale.',
   'Verifica che <reflection> richieda un''auto-valutazione finale su correttezza, completezza, efficienza e sicurezza.',
   'should', 'agent', 1, FALSE),

  ('rule_d_offchat_complete', 'internal_rule', NULL,
   'Fuori dalla chat il prompt e'' l''unico contratto: deve essere completo.',
   'Per i prompt usati fuori dalla chat (REST/worker/batch/reflection), verifica che includano esplicitamente autonomia, anti-loop, output format, examples e reflection (sezione D di CLAUDE.md).',
   'must', 'system', 1, FALSE),

  ('rule_a_no_emoji', 'internal_rule', NULL,
   'Niente emoji nei prompt e nei contenuti generati.',
   'Verifica l''assenza di qualunque emoji o pittogramma nel contenuto del prompt (sezione A di CLAUDE.md).',
   'must', 'all', 1, FALSE),

  ('rule_lang_italian', 'internal_rule', NULL,
   'I prompt agente devono essere redatti in italiano.',
   'Verifica che il testo del prompt sia in italiano; gli identificatori di codice e i nomi dei tag XML restano nella lingua originale.',
   'must', 'all', 1, FALSE)
ON CONFLICT (practice_key, version) DO NOTHING;
