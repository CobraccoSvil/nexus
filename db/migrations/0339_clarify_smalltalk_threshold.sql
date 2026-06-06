-- 0339_clarify_smalltalk_threshold.sql
--
-- Soglia per distinguere lo SMALL-TALK puro (saluti/ringraziamenti) dalle
-- richieste SOSTANZIALI nel clarify_or_expand_node (brain/agents).
--
-- Contesto: l'intake gate (gia' esistente, _intake_gate) consulta la KB del
-- progetto per capire se una richiesta e' nuova/duplicata e coerente (off_topic).
-- Prima veniva BYPASSATO per tutto l'intent 'chat'. Ora il bypass scatta solo
-- per lo small-talk puro: intent chat con agentic_score <= questa soglia. Sopra,
-- anche una "chat" (es. una domanda sul progetto finita su intent chat) passa
-- per l'intake gate e consulta la KB. La distinzione e' semantica (agentic_score
-- dal classifier LLM), non keyword.
--
-- DB-driven (regola G): il codice ha solo il default tecnico 0.3.

INSERT INTO settings (key, value, category, description)
VALUES (
    'clarify.smalltalk_agentic_score_max',
    '0.3',
    'orchestrator',
    'Soglia agentic_score (<=) sotto cui un intent chat e'' small-talk puro e bypassa l''intake gate. Sopra, la chat e'' una richiesta sostanziale e consulta la KB del progetto.'
)
ON CONFLICT (key) DO NOTHING;
