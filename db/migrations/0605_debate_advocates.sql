-- 0605_debate_advocates.sql
-- TESI CONTRAPPOSTE (debate): il gap architetturale del motore multi-agente.
--
-- Root cause: Nexus sapeva MISURARE il dissenso emerso (advisory_panel.dissent)
-- e dare il veto alla minoranza con evidenza, ma non sapeva PROVOCARE il
-- disaccordo: tutte le figure del consiglio ricevono lo stesso task e nessun
-- prompt dice "argomenta contro". Su una decisione architetturale (A vs B) il
-- consenso di sei lenti concordi non e' una prova che l'alternativa sia
-- peggiore: nessuno l'ha difesa.
--
-- Meccanica (tutta come DATO, pattern mig 0546/0548/0554):
--   1. il consiglio dichiara `contested_decision {topic, options[]}` dentro
--      advisory_verdict (campo strutturato, regola M);
--   2. il coordinatore assegna una posizione per avvocato (round-robin puro,
--      debate_panel::plan_debate) e li convoca in parallelo;
--   3. ogni avvocato difende la SUA tesi e chiude con debate_position
--      (stance support|oppose). Una resa con evidenza `alta` squalifica
--      l'opzione anche in minoranza: e' lo stesso veto della
--      minoranza-con-evidenza degli altri panel.
--
-- `orchestrator.debate_enabled` nasce 'false' (flip nella mig 0607 dopo la
-- E2E). Il numero di avvocati NON e' qui: lo decide il resolver di
-- dimensionamento (mig 0602, profili per-classe) entro debate_max_advocates.
--
-- Idempotente su tutte le tabelle.

-- (1) Purpose model dell'avvocato: TIER-ONLY (regola G) — provider/model_id
--     VUOTI di proposito, la scelta del modello concreto e' sempre di
--     best_model_for_tier dal catalog (capability + cooldown aware). Tier
--     'heavy': argomentare contro evidenza e riconoscere quando la propria tesi
--     non regge e' un compito di giudizio critico, come architetto e security.
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes) VALUES
    ('debate_advocate', '', '', 'heavy', 'reasoning', true, 'Dibattito a tesi contrapposte: avvocato di una posizione (mig 0605). Tier-only: nessun modello statico.')
ON CONFLICT (purpose) DO UPDATE SET
    tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = NOW();

-- (2) Kind `advocate`: READ-ONLY come le figure del consiglio (analizza il
--     codice, non lo tocca) + `debate_position` come canale di chiusura.
INSERT INTO nexus_subagent_definitions (kind, description, prompt_key, tool_whitelist, model_purpose, max_iterations, timeout_s, is_background) VALUES
    ('advocate',
     'Avvocato di una tesi in un dibattito a posizioni contrapposte (read-only): difende con evidenza dal codice la posizione assegnata, attacca le avverse, e la arrende onestamente se non regge.',
     'subagent.advocate.base',
     ARRAY['read_file','search_in_files','list_files','search_codebase_semantic','recall_context','nexus_search_semantic','knowledge_search','debate_position'],
     'debate_advocate', 12, 240, false)
ON CONFLICT (kind) DO UPDATE SET
    description = EXCLUDED.description,
    prompt_key = EXCLUDED.prompt_key,
    tool_whitelist = EXCLUDED.tool_whitelist,
    model_purpose = EXCLUDED.model_purpose,
    max_iterations = EXCLUDED.max_iterations,
    timeout_s = EXCLUDED.timeout_s,
    updated_at = NOW();

-- (3) Prompt dell'avvocato (schema XML sez. D). La POSIZIONE ASSEGNATA arriva
--     nel task (prima riga, forma canonica `POSIZIONE ASSEGNATA: <testo>`):
--     nessun placeholder da rendere, il prompt e' identico per tutti gli
--     avvocati e la loro DIVERSITA' e' interamente nel task.
INSERT INTO nexus_prompt_templates (key, category, title, content, is_active, version, updated_by, updated_at) VALUES
('subagent.advocate.base', 'automation', 'Dibattito: avvocato di una tesi',
$$<role>Sei un Avvocato in un dibattito a tesi contrapposte del consiglio Nexus. Ti viene ASSEGNATA una posizione su una decisione architetturale e il tuo compito e' costruire il caso piu' forte possibile per essa, con prove dal codice. NON scrivi ne' esegui codice: studi, argomenti, concludi.</role>

<contesto>
La tua POSIZIONE ASSEGNATA e le POSIZIONI AVVERSE sono dichiarate nel task che ricevi. Altri avvocati difendono le posizioni avverse IN PARALLELO a te, con lo stesso mandato: non li vedi e non li senti. Un coordinatore raccogliera' le posizioni e decidera' sul merito degli argomenti. La tua voce e' UNA parte del dibattito, non il verdetto.
</contesto>

<lente>
- La tua posizione assegnata: quali fatti del codice la SOSTENGONO? Cerca prove concrete (file:riga), non principi generali.
- Le posizioni avverse: dove sono DEBOLI davvero? Attacca con evidenza verificata, mai con retorica o con supposizioni sul codice che non hai letto.
- I costi nascosti della tua stessa posizione: se esistono, li dichiari nei rischi. Un avvocato che nasconde i costi della propria tesi non aiuta la decisione, la inquina.
- Il criterio: cosa rende una posizione preferibile QUI, in QUESTO repo, con questi vincoli (regole CLAUDE.md, punti unici esistenti, ADR)?
</lente>

<autonomia>
- Tool read-only: read_file, search_in_files, list_files, search_codebase_semantic, recall_context, nexus_search_semantic, knowledge_search.
- Orientati con la ricerca semantica; NON leggere file a tappeto. Le prove valgono piu' del volume: tre file letti bene battono trenta sfogliati.
</autonomia>

<principi_nexus>
- Regola H (fix definitivi): se la tua posizione assegnata e' una toppa che maschera un sintomo, dillo — arrenderla e' il comportamento corretto.
- Regola L (punto unico): se una posizione reintroduce logica gia' esistente altrove, e' un argomento forte contro di essa.
- Regola M: argomenta su segnali e fatti verificabili, mai su impressioni.
</principi_nexus>

<anti_loop>
Un solo giro di studio mirato. Concludi appena hai le prove che ti servono: non ri-esplorare in cerca della certezza assoluta.
</anti_loop>

<onesta_intellettuale>
Sei un avvocato, NON un tifoso. Se studiate le prove la tua posizione NON regge, dichiara stance=oppose con i rischi che l'hanno demolita: e' il contributo piu' prezioso del dibattito, non una sconfitta. Difendere l'indifendibile per lealta' al ruolo assegnato falserebbe la decisione — che e' esattamente cio' che il dibattito serve a evitare. Attenzione: un oppose con un rischio di severity alta SQUALIFICA la posizione anche se altri la sostengono; dichiara alta solo con evidenza verificata nel codice, mai per rafforzare la resa.
</onesta_intellettuale>

<output_format>
Chiudi OBBLIGATORIAMENTE chiamando il tool debate_position:
- assigned_position: la posizione assegnata, ripetuta ALLA LETTERA come compare nella prima riga del task (e' la chiave con cui il tuo voto viene attribuito: se la riscrivi, il voto si perde).
- stance: support se la tua posizione regge ed e' preferibile alle avverse; oppose se non regge.
- summary: la tua arringa in breve (cosa hai verificato, con quale conclusione).
- key_arguments: gli argomenti concreti, ognuno con evidenza (file:riga dove possibile).
- risks: i rischi trovati con severity alta|media|bassa. Obbligatorio con stance=oppose.
Niente dump di codice.
</output_format>$$,
true, 1, 'system', NOW())
ON CONFLICT (key) DO UPDATE SET
    content = EXCLUDED.content,
    is_active = true,
    version = nexus_prompt_templates.version + 1,
    updated_at = NOW(),
    updated_by = 'migration_0605';

-- (4) Guard 1 del dispatcher: il kind deve stare ANCHE nella whitelist runtime
--     (una definition senza whitelist e' una feature muta). Pattern idempotente
--     unnest + DISTINCT della mig 0546.
UPDATE settings
   SET value = (
       SELECT string_agg(k, ',' ORDER BY k)
       FROM (
           SELECT DISTINCT trim(x) AS k
           FROM unnest(
               string_to_array(COALESCE(value, ''), ',') || ARRAY['advocate']
           ) AS x
           WHERE trim(x) <> ''
       ) t
   ),
       updated_at = NOW()
 WHERE key = 'orchestrator.subagent_kinds_whitelist';

-- (5) Settings del dibattito. `debate_enabled` nasce OFF: il paradigma si
--     accende in blocco con la mig 0607, dopo la verifica E2E.
INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.debate_enabled', 'false', 'orchestrator',
   'Tesi contrapposte: abilita la convocazione degli avvocati quando il consiglio dichiara una decisione architetturale contesa (contested_decision). Richiede anche orchestrator.sizing_enabled (il numero di avvocati viene dal profilo per-classe) e il kill-switch globale dei sub-agent. Flip a true con mig 0607 dopo la E2E.'),
  ('orchestrator.debate_advocate_kind', 'advocate', 'orchestrator',
   'Kind del sub-agente avvocato convocato nel dibattito (gemello di orchestrator.multi_provider_kind). Deve esistere in nexus_subagent_definitions, essere abilitato, avere debate_position in tool_whitelist ed essere in orchestrator.subagent_kinds_whitelist.'),
  ('orchestrator.debate_max_advocates', '4', 'orchestrator',
   'Backstop assoluto sul numero di avvocati di un dibattito: il resolver di dimensionamento (mig 0602) decide quanti convocarne entro questo tetto. Sotto 2 il dibattito non si tiene (senza contraddittorio non e'' un dibattito).')
ON CONFLICT (key) DO NOTHING;

-- (6) Le figure del CONSIGLIO devono SAPERE di poter dichiarare una decisione
--     contesa: senza questa istruzione il campo esisterebbe nello schema del
--     tool ma nessuno lo userebbe (il pattern "feature dormiente" della mig
--     0571, dove la direttiva mancante = 0 esecuzioni per settimane).
--
--     SOLO le 6 figure del consiglio (roster mig 0553: council_figures +
--     council_infra_figures). `provider_analyst` NON e' incluso di proposito:
--     il coordinatore legge `contested_decision` dalla sintesi del CONSIGLIO
--     (agent_run.rs, maybe_convene_debate), non da quella del panel
--     multi-provider — istruirlo a dichiararla produrrebbe una dichiarazione
--     che nessuno legge, cioe' spesa di token e un'aspettativa tradita.
--
--     Guardia NOT LIKE = idempotenza.
UPDATE nexus_prompt_templates
   SET content = content || E'\n\n<decisione_contesa>\nSe la richiesta nasconde una DECISIONE ARCHITETTURALE aperta - piu'' strade alternative difendibili, dove la scelta cambia il progetto e nessuna e'' ovviamente superiore - dichiarala nel campo contested_decision di advisory_verdict: topic (la decisione in una riga) e options (le alternative reali, almeno due, ognuna comprensibile da sola). Avvocati indipendenti riceveranno UNA opzione ciascuno da difendere con evidenza, e il coordinatore decidera'' sul merito del confronto.\nNON dichiararla per un dettaglio implementativo, per una scelta gia'' presa nel repo (ADR o punto unico esistente), ne'' quando una strada e'' chiaramente giusta: convocheresti un dibattito costoso su una domanda gia'' risolta.\n</decisione_contesa>',
       version = version + 1,
       updated_at = NOW(),
       updated_by = 'migration_0605'
 WHERE key IN (
        'subagent.program_manager.base',
        'subagent.project_manager.base',
        'subagent.functional_analyst.base',
        'subagent.software_architect.base',
        'subagent.sysadmin.base',
        'subagent.security_engineer.base'
       )
   AND content NOT LIKE '%contested_decision%';
