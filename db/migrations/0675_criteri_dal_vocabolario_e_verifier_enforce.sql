-- 0675_criteri_dal_vocabolario_e_verifier_enforce.sql
--
-- Il verifier per-todo passa da 'observe' a 'enforce' (il requisito "validato
-- e testato in TUTTE le fasi" ha ora la fase implementazione presidiata), e la
-- causa che teneva il flip bloccato viene chiusa ALLA FONTE.
--
-- ROOT CAUSE (misurata il 04/08/2026 sui 9 DB progetto del cluster app):
-- dei 42 criteri authored nei todo esistenti, 10 hanno tipi che il runner non
-- esegue — regex_in_output (6) e db_query (4). Non era deriva del modello:
-- erano ESATTAMENTE i due tipi che il punto 4 di agent.planner.base
-- INSEGNAVA. La mig 0635 aveva messo il verifier in observe perche' il 57%
-- dei todo falliva "per forma": la forma la dettava il prompt.
--
-- Il fix ha tre pezzi, di cui due nel codice (stesso changeset):
--   1. lo schema di nexus_todo_write ha ora un enum derivato dal vocabolario
--      UNICO del contratto (TODO_CRITERION_TYPES in nexus-agent-graph
--      runtime/ports.rs, accanto a CriterionSpec) — il provider vincola il
--      modello alla fonte, nessuna normalizzazione di sinonimi a codice
--      (vietata: insegue le varianti);
--   2. il runner degrada il tipo ignoto a Inconclusive SOLO per i criteri a
--      provenienza Todo (campo tipizzato CriterionProvenance): i 10 criteri
--      legacy non bocciano nessun todo per forma, i criteri del GATE
--      continuano a fallire rumorosamente su un typo;
--   3. (qui) il prompt del planner elenca i tipi del vocabolario e spiega
--      come esprimere i bisogni dei due tipi rimossi con tipi eseguibili.
--
-- Impatto del flip, conosciuto prima di farlo (dry-run GAP-8): 20 todo nei DB
-- progetto, 4 aperti; 32 criteri su 42 gia' eseguibili. Reversibile a caldo
-- (il verifier rilegge la config a ogni run): UPDATE inverso a 'observe'.

BEGIN;

-- 1. Il punto 4 del planner insegna il vocabolario ESEGUIBILE (testo esatto
--    letto dal DB vivo il 04/08/2026; il replace non morde su un testo
--    editato a mano e il guard sotto lo segnala).
UPDATE nexus_prompt_templates
SET content = replace(content,
'4. Per ogni todo definisci acceptance_criteria come array di check di tipo:
   - run_command (comando shell + expected exit_code)
   - http (URL + expected_status)
   - file_exists (path)
   - regex_in_output (regex su stdout del comando)
   - db_query (query Postgres + expected row count o valore)',
'4. Per ogni todo definisci acceptance_criteria come array di check di tipo
   ESEGUIBILE dal runner (lo schema del tool li vincola):
   - run_command (command + exit code 0 = passa)
   - http (url + expected_status, default 200)
   - file_exists (path)
   Un bisogno "regex sull''output" si esprime come run_command con grep:
     {type: run_command, command: "npm ls fastify | grep -q fastify"}
   Un valore atteso dal DB applicativo si esprime facendo portare il
   confronto all''exit code:
     {type: run_command, command: "psql $DATABASE_URL -tA -c ''SELECT count(*) FROM utenti'' | grep -qx 3"}
   Un criterio che il runner non sa eseguire non conta: il todo verrebbe
   valutato senza.'),
    version = version + 1, updated_at = NOW(), updated_by = 'migration_0675'
WHERE key = 'agent.planner.base'
  AND content LIKE '%- db_query (query Postgres + expected row count o valore)%';

-- 1b. Il punto 5 non promette l'inautorabile (review W1, rilievo F8):
--     l'esempio canonico "curl http://localhost:$BACKEND_PORT/api/health"
--     non e' authorabile correttamente al plan time (la porta non e' ancora
--     nota e il letterale non viene espanso).
UPDATE nexus_prompt_templates
SET content = replace(content,
'5. Il piano DEVE includere un acceptance_criterion end-to-end che dipende dal completamento dell''intero lavoro:
   - per scaffold app: curl http://localhost:$BACKEND_PORT/api/health -> 200
   - per fix bug: npm test passes (exit 0)
   - per refactor: pnpm verify passes',
'5. Il piano DEVE includere un acceptance_criterion end-to-end che dipende dal completamento dell''intero lavoro:
   - per scaffold app: {type: http, url: con la porta REALE gia'' allocata
     (chiedila con request_port PRIMA di pianificare); se al plan time la
     porta non e'' nota, usa run_command che la legge dall''ambiente del
     progetto (es. "curl -sf http://localhost:$PORT/api/health")}
   - per fix bug: {type: run_command, command: "npm test"}
   - per refactor: {type: run_command, command: "pnpm verify"}'),
    version = version + 1, updated_at = NOW(), updated_by = 'migration_0675'
WHERE key = 'agent.planner.base'
  AND content LIKE '%curl http://localhost:$BACKEND_PORT/api/health -> 200%';

-- 2. Il flip: enforce. I criteri eseguibili dei todo diventano vincolanti
--    (il fail-closed sui gate generali quando TUTTI i criteri sono
--    inconcludenti esiste gia' — verifier.rs:606-635). Confronto NORMALIZZATO
--    come fa il runtime (trim+lower — review W1, rilievo F10).
UPDATE settings
SET value = 'enforce', updated_at = NOW()
WHERE key = 'agent.verifier.todo_criteria_mode'
  AND lower(trim(value)) = 'observe';

-- Guard: la migrazione verifica il proprio effetto.
DO $$
DECLARE
    modo TEXT;
BEGIN
    SELECT lower(trim(value)) INTO modo
      FROM settings WHERE key = 'agent.verifier.todo_criteria_mode';
    -- 'off' e' il kill-switch legittimo del verifier: la migrazione non lo
    -- scavalca (stesso rispetto del rollback che la 0674 ha per is_active).
    -- EXCEPTION solo se il flip non e' avvenuto dove doveva: 'observe'
    -- residuo o chiave assente. Confronto normalizzato come il runtime (F10).
    IF modo IS NULL OR modo = 'observe' THEN
        RAISE EXCEPTION '0675: todo_criteria_mode = % (il flip a enforce non e'' avvenuto)', COALESCE(modo, 'NULL');
    ELSIF modo <> 'enforce' THEN
        RAISE NOTICE '0675: todo_criteria_mode = % (kill-switch rispettato, flip non applicato)', modo;
    END IF;
    -- Il flip e il prompt sono ACCOPPIATI (review W1, rilievo F4): rendere
    -- vincolanti i criteri mentre il prompt insegna ancora tipi fuori
    -- vocabolario ricreerebbe l'incidente della 0635 (57% bocciato per
    -- forma). Se il replace non ha morso (testo editato a mano), il deploy si
    -- FERMA: si riallinea il prompt dal pannello admin e si rilancia.
    IF EXISTS (SELECT 1 FROM nexus_prompt_templates
                WHERE key = 'agent.planner.base'
                  AND content LIKE '%- db_query (query Postgres%') THEN
        RAISE EXCEPTION '0675: agent.planner.base insegna ancora db_query: il flip a enforce non puo'' accompagnare un prompt che detta tipi fuori vocabolario';
    END IF;
END $$;

COMMIT;
