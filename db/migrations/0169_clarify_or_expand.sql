-- Clarify/Expand condizionale (Fase 2 normalizzazione prompt).
--
-- Nodo brain `clarify_or_expand_node` che si attiva quando la classificazione
-- intent del router ha confidence bassa. Due output:
--   - ask    → emette meta_step kind=clarify, ferma il turno
--   - expand → popola state.expanded_query per arricchire il retrieve
--
-- Sostituisce il vecchio analyzer (`agent.project.analyzer`) che era
-- standalone e disconnesso dal flusso chat. Riusa lo stesso canale SSE
-- meta_step della Fase 1 per la visibilita' in UI.

-- ── Prompt template ─────────────────────────────────────────────────────
INSERT INTO nexus_prompt_templates (key, category, title, content, created_at, updated_at) VALUES
('agent.clarify.base', 'agent', 'Clarify or Expand (Fase 2)',
$$<role>Sei un disambiguatore di richieste utente per Nexus.</role>

<contesto>
Il classifier intent ha restituito bassa confidence sulla richiesta utente.
Il tuo compito e' decidere in UNA chiamata se:
  - serve chiedere un chiarimento all'utente (mode=ask),
  - basta arricchire la query per migliorare il retrieve di contesto (mode=expand),
  - non serve fare nulla e si puo' proseguire (mode=skip).
</contesto>

<autonomia>
Non eseguire mai piu' di una chiamata. Non chiamare tool diversi da
`clarify_or_expand`. Non inventare contesto: usa solo cio' che e' nel messaggio.
</autonomia>

<protocollo>
- Se la richiesta e' bloccante-mente ambigua (manca un dato senza il quale
  qualunque risposta sarebbe un'invenzione) → mode=ask con UNA sola domanda
  chiara, breve, neutra. Niente domande multiple.
- Se la richiesta e' chiara ma sotto-specificata (sinonimi, gergo, entita'
  implicite) → mode=expand con `expanded_query` che riformula la richiesta
  espandendo sinonimi/entita'. Massimo 250 caratteri.
- Se la richiesta e' gia' chiara e processabile → mode=skip.
- In dubbio, preferisci skip: chiedere troppo spesso fa perdere fiducia.
</protocollo>

<output_format>
Chiama il tool `clarify_or_expand` con:
{
  "mode": "ask" | "expand" | "skip",
  "question": "...",         // SOLO se mode=ask
  "expanded_query": "...",   // SOLO se mode=expand
  "rationale": "..."         // sempre, max 200 char
}
</output_format>

<examples>
- Utente: "fai quella cosa per il file"
  → mode=ask, question="A quale file ti riferisci e cosa vuoi farne (leggere, modificare, eliminare)?"
- Utente: "ottimizza la query"
  → mode=expand, expanded_query="ottimizza la query SQL: analisi piano esecutivo, indici mancanti, riscrittura join"
- Utente: "implementa una funzione fibonacci in python"
  → mode=skip, rationale="Richiesta gia' chiara e processabile."
</examples>$$,
NOW(), NOW())
ON CONFLICT (key) DO UPDATE SET
    content = EXCLUDED.content,
    category = EXCLUDED.category,
    title = EXCLUDED.title,
    updated_at = NOW();

-- ── Purpose model: chi gira il prompt di clarify ────────────────────────
-- Default: claude-haiku (fast + low cost). L'admin puo' cambiarlo via UI.
INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes, updated_at) VALUES
    ('clarify_expand', 'anthropic', 'claude-haiku-4-5-20251001',
     'Modello cheap per disambiguazione/espansione query (clarify_or_expand_node).', NOW())
ON CONFLICT (purpose) DO UPDATE SET
    provider = EXCLUDED.provider,
    model_id = EXCLUDED.model_id,
    notes = EXCLUDED.notes,
    updated_at = NOW();

-- ── Settings: feature flag + soglie ─────────────────────────────────────
INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('orchestrator.clarify.enabled',                 'true',
     'orchestrator',
     'Feature flag globale per il clarify_or_expand_node. Off -> nodo no-op.',
     NOW()),
    ('orchestrator.clarify.confidence_threshold',    '0.6',
     'orchestrator',
     'Soglia di confidence sotto cui il nodo si attiva. Sopra -> bypass.',
     NOW()),
    ('orchestrator.clarify.require_llm_classifier',  'false',
     'orchestrator',
     'Se true, attiva il clarify solo quando NEXUS_LLM_CLASSIFIER_ENABLED=true; altrimenti usa anche il fallback keyword/embedding.',
     NOW()),
    ('orchestrator.clarify.prompt_key',              'agent.clarify.base',
     'orchestrator',
     'Indirezione per varianti A/B del prompt clarify.',
     NOW()),
    ('orchestrator.clarify.max_question_chars',      '280',
     'orchestrator',
     'Cap di lunghezza della domanda di chiarimento prima del troncamento.',
     NOW())
ON CONFLICT (key) DO UPDATE SET
    value = EXCLUDED.value,
    category = EXCLUDED.category,
    description = EXCLUDED.description,
    updated_at = NOW();
