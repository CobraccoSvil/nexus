-- 0554: Flusso advisory multi-provider tra Consiglio delle Competenze e ultracode.
--
-- Obiettivo: analizzare lo stesso problema con provider/modelli diversi senza
-- hardcode nel codice. Il runtime seleziona candidati distinti dal catalog tramite
-- `nexus_purpose_model` e avvia sub-run nativi read-only con pin provider/model
-- derivato dal DB. Ogni analista chiude con `advisory_verdict`; l'aggregatore riusa
-- la sintesi advisory strutturata del Consiglio.

INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.multi_provider_enabled', 'true', 'orchestrator',
   'Abilita il pre-step advisory multi-provider sui task che superano il gate del Consiglio delle Competenze. I provider/modelli sono scelti da catalog/purpose, non dal codice.'),
  ('orchestrator.multi_provider_kind', 'provider_analyst', 'orchestrator',
   'Kind sub-agent read-only usato per ogni voce del panel multi-provider. Deve essere presente in nexus_subagent_definitions e in orchestrator.subagent_kinds_whitelist.'),
  ('orchestrator.multi_provider_purpose', 'multi_provider_advisory', 'orchestrator',
   'Purpose model tier-aware usato per selezionare i candidati provider distinti del panel multi-provider.'),
  ('orchestrator.multi_provider_max_providers', '3', 'orchestrator',
   'Numero massimo di provider distinti da convocare nel panel multi-provider.'),
  ('orchestrator.multi_provider_min_providers', '2', 'orchestrator',
   'Numero minimo di provider distinti richiesti per attivare il panel; sotto soglia il pre-step viene saltato.')
ON CONFLICT (key) DO NOTHING;

INSERT INTO nexus_purpose_model
    (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
VALUES
    ('multi_provider_advisory', 'deepseek', 'deepseek-v4-flash', 'medium',
     'reasoning', true,
     'Panel multi-provider: selezione di provider distinti dal catalog per analisi read-only (mig 0554)')
ON CONFLICT (purpose) DO UPDATE SET
    tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = NOW();

INSERT INTO nexus_subagent_definitions
    (kind, description, prompt_key, tool_whitelist, model_purpose,
     max_iterations, timeout_s, is_background)
VALUES
    ('provider_analyst',
     'Figura read-only multi-provider: analizza la richiesta dal punto di vista del provider/modello assegnato e produce advisory_verdict.',
     'subagent.provider_analyst.base',
     ARRAY['read_file','search_in_files','list_files','search_codebase_semantic',
           'recall_context','nexus_search_semantic','knowledge_search','advisory_verdict'],
     'multi_provider_advisory', 10, 240, false)
ON CONFLICT (kind) DO UPDATE SET
    description = EXCLUDED.description,
    prompt_key = EXCLUDED.prompt_key,
    tool_whitelist = EXCLUDED.tool_whitelist,
    model_purpose = EXCLUDED.model_purpose,
    max_iterations = EXCLUDED.max_iterations,
    timeout_s = EXCLUDED.timeout_s,
    updated_at = NOW();

UPDATE settings
   SET value = value || ',provider_analyst'
 WHERE key = 'orchestrator.subagent_kinds_whitelist'
   AND value NOT LIKE '%provider_analyst%';

INSERT INTO nexus_prompt_templates
    (key, category, title, content, is_active, version, updated_by, updated_at)
VALUES
('subagent.provider_analyst.base', 'automation', 'Multi-provider analyst',
$$<role>Sei una voce del panel multi-provider Nexus. Analizzi la richiesta dal punto di vista del provider/modello assegnato nel contesto del sub-run. NON scrivi codice e NON esegui comandi: osservi, confronti trade-off e produci un parere strutturato.</role>

<contesto>
Ricevi la richiesta utente e un blocco "Provider assegnato". Altre voci del panel analizzano la stessa richiesta con provider/modelli diversi. Il coordinatore usera' SOLO il tuo advisory_verdict strutturato per comporre vincoli, rischi e veto.
</contesto>

<lente>
- Quali assunzioni, punti di forza e failure-mode potrebbe introdurre il provider/modello assegnato?
- Quali vincoli devono passare al piano prima di ultracode?
- Quali rischi emergono se un altro provider avesse una prospettiva diversa o piu' conservativa?
- Quali verifiche oggettive servono per evitare che il piano dipenda dalla prosa di un modello?
</lente>

<principi_nexus>
- Provider e modelli sono dati di routing DB-driven: non proporre hardcode.
- Usa segnali strutturati, non parsing della prosa, per decidere esiti tecnici.
- Se trovi una toppa, segnala il fix alla causa radice.
- Se serve logica condivisa, richiedi un punto unico di controllo.
</principi_nexus>

<verdetto_strutturato>
Chiudi SEMPRE chiamando advisory_verdict come ultimissima azione: verdict=proceed se il piano puo' procedere senza vincoli aggiuntivi; proceed_with_changes se servono requisiti; block solo se hai un rischio con evidenza concreta che rende la richiesta non eseguibile cosi'. requirements = vincoli azionabili per ultracode; risks = lista di {severity: alta|media|bassa, description con evidenza}; recommendations = suggerimenti non vincolanti.
</verdetto_strutturato>$$,
true, 1, 'migration_0554', NOW())
ON CONFLICT (key) DO UPDATE SET
    content = EXCLUDED.content,
    is_active = true,
    version = nexus_prompt_templates.version + 1,
    updated_at = NOW();
