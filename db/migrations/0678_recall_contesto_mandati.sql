-- 0678: recupero del contesto per pertinenza nei mandati delle figure (W4
-- del processo standard, pilastro "ottimizzazione del contesto con
-- vettorializzazione dove possibile").
--
-- Due gambe, UN interruttore (due interruttori per lo stesso pilastro
-- raddoppiano gli stati incoerenti):
-- 1. il MANDATO di ogni figura convocata riceve il blocco
--    <contesto_richiamato> (hits di rag::search_semantic sopra soglia, fonte
--    e score dichiarati) — innesto unico in prepare_subagent_run, nel
--    MESSAGGIO iniziale, mai nel system (disciplina cache);
-- 2. il SYSTEM delle figure con tool semantici in whitelist riceve la
--    direttiva <recupero_contesto> (stabile per kind: nessun costo cache).
-- Le soglie RIUSANO knowledge.context_injection_top_k/_min_score (stessa
-- semantica: una seconda famiglia di chiavi con gli stessi numeri e' la
-- divergenza di domani).
--
-- KILL-SWITCH (reversibile a caldo):
--   UPDATE settings SET value = 'false'
--    WHERE key = 'agent.subagent.context_recall_enabled';

INSERT INTO settings (key, value, category, description) VALUES
    ('agent.subagent.context_recall_enabled', 'true', 'agent',
     'Interruttore UNICO del pilastro W4: blocco <contesto_richiamato> nel mandato delle figure + direttiva <recupero_contesto> nel system dei kind con tool semantici. Fail-open: Qdrant giu'' o zero hit = mandato senza blocco, mai convocazione bloccata (mig 0678).'),
    ('agent.subagent.mandate_recall_kinds', 'kb,code', 'agent',
     'Kind interrogati dal recall del mandato (CSV; il parse delega a SourceKind::parse, la policy ammette kb|code|attachment|meta_doc). Valori fuori vocabolario o non ammessi scartati con WARN (mig 0678).')
ON CONFLICT (key) DO NOTHING;

INSERT INTO nexus_prompt_templates
    (key, category, title, content, is_active, version, updated_by, updated_at)
VALUES
    ('subagent.directive.context_recall', 'automation', 'Direttiva: recupero contesto semantico',
'<recupero_contesto>
Prima di letture massive o esplorazioni ad ampio raggio, interroga l''indice semantico del progetto coi tool che hai a disposizione ({{tools}}): recupera i frammenti pertinenti al task e leggi per intero SOLO i file che quei frammenti indicano. Il blocco <contesto_richiamato> nel tuo mandato, quando presente, viene dallo stesso indice: trattalo come punto di partenza, non come perimetro. Un indice che non risponde non e'' un blocco: prosegui con le letture mirate e dichiaralo.
</recupero_contesto>',
     TRUE, 1, 'migration_0678', NOW())
ON CONFLICT (key) DO NOTHING;

-- Tool semantici ai kind implementativi (base + domain della 0218, che
-- avevano un vocabolario dimezzato: solo search_codebase_semantic) e a
-- review (pattern 0538: array_append idempotente). `verify` resta escluso
-- dal CATALOGO TOOL e quindi dalla direttiva (deterministico per contratto:
-- un tool di similarita' li' e' rumore); il blocco <contesto_richiamato>
-- nel MANDATO invece raggiunge ogni figura, verify compreso — il gate del
-- blocco e' l'interruttore del pilastro, non la whitelist.
UPDATE nexus_subagent_definitions
   SET tool_whitelist = array_append(tool_whitelist, 'nexus_search_semantic'),
       updated_at = NOW()
 WHERE kind IN ('implement', 'review', 'frontend_implementer', 'db_architect', 'test_author')
   AND NOT ('nexus_search_semantic' = ANY(tool_whitelist));

UPDATE nexus_subagent_definitions
   SET tool_whitelist = array_append(tool_whitelist, 'search_codebase_semantic'),
       updated_at = NOW()
 WHERE kind IN ('implement', 'review', 'frontend_implementer', 'db_architect', 'test_author')
   AND NOT ('search_codebase_semantic' = ANY(tool_whitelist));

-- Guard: la migrazione dichiara se il seed non ha morso.
DO $$
DECLARE
    v_flag TEXT;
    v_directive INT;
BEGIN
    SELECT value INTO v_flag FROM settings
     WHERE key = 'agent.subagent.context_recall_enabled';
    IF v_flag IS NULL THEN
        RAISE EXCEPTION 'mig 0678: chiave context_recall_enabled assente dopo il seed';
    END IF;
    IF v_flag <> 'true' THEN
        RAISE NOTICE 'mig 0678: recall preesistente spento (%) lasciato invariato', v_flag;
    END IF;
    SELECT COUNT(*) INTO v_directive FROM nexus_prompt_templates
     WHERE key = 'subagent.directive.context_recall' AND is_active = true;
    IF v_directive = 0 THEN
        RAISE EXCEPTION 'mig 0678: template della direttiva assente o disattivo';
    END IF;
END $$;
