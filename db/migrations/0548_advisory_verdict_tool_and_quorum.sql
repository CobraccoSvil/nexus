-- 0548_advisory_verdict_tool_and_quorum.sql
-- M2 pezzo C del "consiglio di figure professionali" (advisory panel a monte).
--
-- (1) Abilita il tool brain-only `advisory_verdict` (gemello di `review_verdict`
--     mig 0538) sulle 6 figure di analisi (mig 0546): il tool esiste nel catalogo
--     statico (AGENT_TOOLS_JSON) ed e' in SUBAGENT_ONLY_TOOLS (nexus-agent-tools),
--     quindi arriva SOLO ai kind che lo whitelistano. Senza questa riga il modello
--     della figura non potrebbe dichiarare il parere strutturato (feature muta).
-- (2) Istruzione operativa nel prompt di ogni figura: senza, il modello non sa che
--     il tool esiste ne' quando chiamarlo (pattern noto del repo).
-- (3) Policy del quorum del panel ADVISORY (gemello di orchestrator.review_quorum_*
--     mig 0539): letta dal coordinatore (pezzo D) e passata al punto unico PURO
--     decisions::compose_advisory_synthesis (regola G: DB-driven, mai hardcoded).
--
-- I setting di ATTIVAZIONE a soglia (complessita' minima, kill-switch del consiglio)
-- arrivano nella migrazione del pezzo D, insieme al coordinatore che li consuma.
--
-- Idempotente: array_append guardato da NOT ANY; append al prompt guardato da
-- NOT LIKE; settings ON CONFLICT DO NOTHING.

-- (1) advisory_verdict nella tool_whitelist delle 6 figure.
UPDATE nexus_subagent_definitions
   SET tool_whitelist = array_append(tool_whitelist, 'advisory_verdict'),
       updated_at = NOW()
 WHERE kind IN ('program_manager','project_manager','functional_analyst',
                'software_architect','sysadmin','security_engineer')
   AND NOT ('advisory_verdict' = ANY(tool_whitelist));

-- (2) Istruzione advisory_verdict appesa al prompt di ogni figura (guardata da
--     NOT LIKE: idempotente, un solo append anche a riesecuzione).
UPDATE nexus_prompt_templates
   SET content = content || E'\n\n<verdetto_strutturato>\nChiudi SEMPRE la tua analisi chiamando il tool advisory_verdict come ULTIMISSIMA azione: verdict=proceed se dalla tua lente si puo'' procedere senza vincoli aggiuntivi; proceed_with_changes se servono i requisiti che elenchi; block SOLO se hai almeno un rischio con evidenza concreta che rende la richiesta non eseguibile cosi''. requirements = vincoli azionabili che l''esecuzione deve rispettare; risks = lista di {severity: alta|media|bassa, description con evidenza}; recommendations = suggerimenti non vincolanti. Un block senza alcun rischio con evidenza viene RIFIUTATO. Il final_answer in prosa resta il resoconto umano; il parere macchina e'' SOLO quello del tool.\n</verdetto_strutturato>',
       version = version + 1,
       updated_at = NOW()
 WHERE key IN ('subagent.program_manager.base','subagent.project_manager.base',
               'subagent.functional_analyst.base','subagent.software_architect.base',
               'subagent.sysadmin.base','subagent.security_engineer.base')
   AND content NOT LIKE '%advisory_verdict%';

-- (3) Policy del quorum del panel advisory (gemello mig 0539). I default nel codice
--     (AdvisoryPolicy::default) coincidono con questi valori come safe-default se la
--     riga manca: min_valid=1, block_on_high_severity=true.
INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.council_advisory_min_valid', '1', 'orchestrator',
   'Consiglio a monte: minimo numero di pareri VALIDI (figura con esito + advisory dichiarato) perche'' il panel advisory sia conclusivo; sotto soglia il verdetto e'' inconclusive (mai trattato come via libera). DB-driven, regola G.'),
  ('orchestrator.council_advisory_block_on_high_severity', 'true', 'orchestrator',
   'Consiglio a monte: veto avversario del panel advisory. true = un solo verdetto block con un rischio severity alta fa vincere il veto anche in minoranza; false = il block vale come voto ordinario. DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
