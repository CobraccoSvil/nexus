-- Fase B ultracode (verifica avversaria tra agenti): il kind `review` dichiara
-- il verdetto della code review in forma STRUTTURATA via tool brain-only
-- `review_verdict` (gemello di `task_complete`, ADR 0034 / regola M) invece del
-- solo markdown in prosa.
--
-- Il verdetto normalizzato {verdict: pass|fail|needs_changes, summary,
-- findings[{file, line?, severity, description}]} finisce nello stato del grafo
-- (`AgentState.review_verdict`), attraversa il confine sub-run dentro
-- `NativeRunOutcome::structured_verdict()` (campo `review` del blocco esito,
-- Fase A: colonna `nexus_subagent_runs.verdict`, mig project/0009) ed e'
-- leggibile dal coordinatore nel tool_result di dispatch_subagent(s) e dal poll
-- (`nexus_subagent_poll`), senza parsare prosa.
--
-- Attivazione DB-driven (regola G, nessun flag runtime): il tool esiste nel
-- catalogo statico (AGENT_TOOLS_JSON) ma il catalogo del run PRINCIPALE lo
-- filtra (SUBAGENT_ONLY_TOOLS in nexus-agent-tools, consumato da
-- build_tools_json_for_agent); ai SUB-agenti arriva solo via tool_whitelist.
-- Questa migrazione lo aggiunge al solo kind `review`.
--
-- Idempotente: array_append guardato da NOT ANY; append al prompt guardato da
-- NOT LIKE.

UPDATE nexus_subagent_definitions
   SET tool_whitelist = array_append(tool_whitelist, 'review_verdict'),
       updated_at = NOW()
 WHERE kind = 'review'
   AND NOT ('review_verdict' = ANY(tool_whitelist));

-- Istruzione operativa nel prompt del revisore: senza, il modello non sa che
-- il tool esiste ne' quando chiamarlo (feature muta = pattern noto del repo:
-- "tool con handler ma mai dichiarato" e' una feature morta).
UPDATE nexus_prompt_templates
   SET content = content || E'\n\n<verdetto_strutturato>\nChiudi SEMPRE la review chiamando il tool review_verdict come ULTIMISSIMA azione: verdict=pass solo se non hai trovato difetti reali; fail se il lavoro non e'' accettabile; needs_changes se e'' accettabile ma va corretto. Ogni finding deve avere file ed evidenza concreta (scenario di fallimento), niente osservazioni vaghe. Un verdetto negativo senza findings viene RIFIUTATO. Il final_answer in prosa resta il resoconto umano; il verdetto macchina e'' SOLO quello del tool.\n</verdetto_strutturato>',
       version = version + 1,
       updated_at = NOW()
 WHERE key = 'subagent.review.base'
   AND content NOT LIKE '%review_verdict%';
