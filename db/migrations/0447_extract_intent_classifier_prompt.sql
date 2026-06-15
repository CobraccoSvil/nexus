-- 0447: B6 — estrazione del prompt del classifier di intent (cuore del router).
-- Regola G/D. Il template usa .format(message=...): {message} e' il placeholder,
-- le doppie graffe {{ }} restano (diventano { } nel prompt finale, per il JSON).
-- Il codice ha fallback ROBUSTO (try/except) alla costante _CLASSIFIER_PROMPT:
-- se il template DB e' assente o malformato il router NON si rompe.
--
-- Idempotente: ON CONFLICT (key) DO NOTHING.

INSERT INTO nexus_prompt_templates (key, category, title, content, updated_by) VALUES
(
    'system.intent_classifier_prompt',
    'system',
    'Classificatore agentico di intent (router) — schema JSON',
    $cls$Intent classifier for a coding assistant. Return ONLY a JSON object, no markdown, no text.

Message: """{message}"""

Schema (all keys required):
{{
"intent": one of ["chat","debug","fix","refactor","test","docs","architecture","file_ops","system_admin","code_read"],
"agentic_score": 0.0..1.0,
"requires_tools": bool,
"authorizes_changes": bool,
"complexity": "low"|"medium"|"high",
"confidence": 0.0..1.0,
"candidates": [{{"intent":"...","confidence":0..1}}, up to 3],
"slots": {{
  "action_verb": "read"|"write"|"resolve"|"analyze"|"refactor"|"configure"|"deploy"|"delete",
  "target_type": "code"|"tests"|"config"|"service"|"docs"|"data"|"infrastructure",
  "framework": e.g. "playwright"|"pytest"|"cargo"|"jest"|"docker" or "" if generic,
  "scope": "single"|"multi_file"|"cross_service"|"system_wide",
  "confidence": 0.0..1.0
}}
}}

Intent meaning:
- chat=conversational, no action; debug=find root cause of failure; fix=repair specific known bug;
- refactor=restructure no behavior change; test=WRITE new tests; docs=write documentation;
- code_read=read/inspect files; architecture=high-level design; file_ops=create/delete files;
- system_admin=configure services/deploy.

CRITICAL:
- "scrivi test per X" → intent=test, action_verb=write.
- "esegui test e correggi fail" / "fai funzionare i test" → intent=debug, action_verb=resolve.
- "fix bug at file.py:42" → intent=fix, action_verb=resolve, scope=single.
- "leggi file.py" → intent=code_read, action_verb=read.
- "fai/crea/costruisci/realizza una app|applicazione|sistema|sito|servizio|piattaforma per X" → intent=architecture, action_verb=write, scope=system_wide, complexity=high. E' scaffolding completo (PRD + schema DB + backend + frontend + test). NON e' docs.
- "scaffold/genera progetto" / "boilerplate" / "starter kit" → intent=architecture, scope=system_wide.
- "imposta/configura/abilita un utente admin|il backend|un servizio|CORS|HTTPS", "setup X", "deploya/avvia X" → intent=system_admin, requires_tools=true. E' un task agentico multi-step, NON chat anche se la frase e' breve.
- RETROSPECTIVE/META requests about work ALREADY done in this conversation — "riassumi cosa hai fatto/sistemato", "spiega cosa e' successo", "che modifiche hai applicato?", "fammi il punto" → intent=chat, requires_tools=false, agentic_score<=0.2. The user wants a TEXT answer about past work, NOT new actions or documentation files. NOT docs (docs = write documentation files into the repo).
"authorizes_changes" — THE KEY report-vs-act judgment, decide it from the user's intent:
- true when the user wants the assistant to MODIFY code/system: fix, implement, refactor, scaffold, configure, deploy, delete, "fai funzionare", "correggi", "sistema", "crea". This is the default for action intents.
- false when the user wants only to INSPECT and be TOLD the result: "verifica/controlla che X compili|funzioni|risponda E riporta/dimmi l'esito", "controlla lo stato di X e fammi sapere", "fai un check e riportami", "leggi/spiega X". requires_tools can still be true (checks need build/test/curl/read) but NO code changes are wanted.
- CRITICAL: a verify/report task stays authorizes_changes=false EVEN IF a check FAILS — finding something broken does NOT authorize fixing it; report it instead. Only switch to true if the user explicitly also asks to fix ("verifica e CORREGGI|SISTEMA|fai funzionare X").
- When unsure, prefer true (do not block legitimate fixes).

Use confidence<0.7 honestly when ambiguous (downstream asks user). NEVER inflate.

Examples:
- "ciao" → {{"intent":"chat","agentic_score":0.0,"requires_tools":false,"complexity":"low","confidence":0.99,"candidates":[{{"intent":"chat","confidence":0.99}}],"slots":{{"action_verb":"read","target_type":"code","framework":"","scope":"single","confidence":0.10}}}}
- "Fai una app per la gestione di un autonoleggio" → {{"intent":"architecture","agentic_score":0.95,"requires_tools":true,"complexity":"high","confidence":0.92,"candidates":[{{"intent":"architecture","confidence":0.92}}],"slots":{{"action_verb":"write","target_type":"service","framework":"","scope":"system_wide","confidence":0.90}}}}
- "Crea un sito ecommerce con catalogo e carrello" → {{"intent":"architecture","agentic_score":0.95,"requires_tools":true,"complexity":"high","confidence":0.92,"candidates":[{{"intent":"architecture","confidence":0.92}}],"slots":{{"action_verb":"write","target_type":"service","framework":"","scope":"system_wide","confidence":0.88}}}}
- "leggi src/main.py" → {{"intent":"code_read","agentic_score":0.8,"requires_tools":true,"complexity":"low","confidence":0.95,"candidates":[{{"intent":"code_read","confidence":0.95}}],"slots":{{"action_verb":"read","target_type":"code","framework":"","scope":"single","confidence":0.95}}}}
- "scrivi un test per foo()" → {{"intent":"test","agentic_score":0.7,"requires_tools":true,"complexity":"medium","confidence":0.92,"candidates":[{{"intent":"test","confidence":0.92}}],"slots":{{"action_verb":"write","target_type":"tests","framework":"","scope":"single","confidence":0.90}}}}
- "esegui i test playwright e risolvi i fail" → {{"intent":"debug","agentic_score":0.95,"requires_tools":true,"complexity":"high","confidence":0.85,"candidates":[{{"intent":"debug","confidence":0.85}},{{"intent":"fix","confidence":0.70}}],"slots":{{"action_verb":"resolve","target_type":"tests","framework":"playwright","scope":"multi_file","confidence":0.92}}}}
- "i test pytest non passano, correggi" → {{"intent":"debug","agentic_score":0.9,"requires_tools":true,"complexity":"high","confidence":0.80,"candidates":[{{"intent":"debug","confidence":0.80}},{{"intent":"fix","confidence":0.65}}],"slots":{{"action_verb":"resolve","target_type":"tests","framework":"pytest","scope":"multi_file","confidence":0.88}}}}
- "fix null pointer at handlers.py:42" → {{"intent":"fix","agentic_score":0.85,"requires_tools":true,"complexity":"medium","confidence":0.90,"candidates":[{{"intent":"fix","confidence":0.90}}],"slots":{{"action_verb":"resolve","target_type":"code","framework":"","scope":"single","confidence":0.85}}}}
- "deploya il microservizio doc-service" → {{"intent":"system_admin","agentic_score":0.9,"requires_tools":true,"complexity":"high","confidence":0.92,"candidates":[{{"intent":"system_admin","confidence":0.92}}],"slots":{{"action_verb":"deploy","target_type":"service","framework":"docker","scope":"cross_service","confidence":0.90}}}}
- "elimina i dockerfile rimasti" → {{"intent":"file_ops","agentic_score":0.7,"requires_tools":true,"complexity":"low","confidence":0.88,"candidates":[{{"intent":"file_ops","confidence":0.88}}],"slots":{{"action_verb":"delete","target_type":"infrastructure","framework":"docker","scope":"multi_file","confidence":0.85}}}}
- "verifica che il backend compili e che il frontend buildi, riporta l'esito di ogni controllo" → {{"intent":"code_read","agentic_score":0.5,"requires_tools":true,"authorizes_changes":false,"complexity":"medium","confidence":0.85,"candidates":[{{"intent":"code_read","confidence":0.85}}],"slots":{{"action_verb":"analyze","target_type":"code","framework":"","scope":"multi_file","confidence":0.85}}}}
- "controlla che il servizio risponda e dimmi lo stato" → {{"intent":"code_read","agentic_score":0.4,"requires_tools":true,"authorizes_changes":false,"complexity":"low","confidence":0.88,"candidates":[{{"intent":"code_read","confidence":0.88}}],"slots":{{"action_verb":"analyze","target_type":"service","framework":"","scope":"single","confidence":0.88}}}}
- "verifica perche' il backend crasha e correggilo" → {{"intent":"debug","agentic_score":0.9,"requires_tools":true,"authorizes_changes":true,"complexity":"high","confidence":0.85,"candidates":[{{"intent":"debug","confidence":0.85}}],"slots":{{"action_verb":"resolve","target_type":"service","framework":"","scope":"multi_file","confidence":0.85}}}}

Return ONLY the JSON object.$cls$,
    'migration_0447'
)
ON CONFLICT (key) DO NOTHING;
