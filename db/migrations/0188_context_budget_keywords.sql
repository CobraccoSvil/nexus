-- 0188: Aggiunge keyword italiane mancanti ai pesi di complessita' del budget
-- iterazioni adattivo. "Implementa l'applicazione" aveva complexity_score = 0
-- perche' "implementa" non era nella lista, risultando in 90 iterazioni
-- (base=60 * weak_model_multiplier=1.5) per un task complesso.
-- Questo causava accumulo incontrollato di tool results nel contesto LLM
-- con esplosione a 448K+ prompt tokens.

INSERT INTO settings (key, value, description)
VALUES (
    'agent.complexity.keyword_weights',
    '{"create":3,"write_file":2,"install":2,"build":2,"systemctl":2,"docker":2,"pnpm":2,"npm":1,"deploy":3,"migrate":3,"refactor":4,"fullstack":10,"end-to-end":8,"backend":2,"frontend":2,"database":2,"crea":3,"installa":2,"esegui":2,"avvia":2,"configura":2,"implementa":4,"sviluppa":4,"costruisci":3,"genera":2,"scaffold":5,"progetto":2,"applicazione":3,"applicazion":3}',
    'Pesi keyword per stima complessita'' task agente (budget iterazioni adattivo). Chiave: substring da cercare nel prompt, valore: punti complessita''. Aggiornamento: aggiunti verbi italiani (implementa, sviluppa, costruisci, genera, scaffold) e sostantivi (progetto, applicazione).'
)
ON CONFLICT (key) DO UPDATE SET
    value = EXCLUDED.value,
    description = EXCLUDED.description;
