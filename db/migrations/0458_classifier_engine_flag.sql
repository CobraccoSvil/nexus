-- 0458_classifier_engine_flag.sql
-- Tappa 1b del porting LangGraph->Rust: flag di COESISTENZA per il classifier
-- intent. Regola G/L: la scelta del motore di classificazione (Python via
-- endpoint brain `/classify-intent-agentic` vs Rust in-process
-- `crate::intent_classifier::classify`) ha UN solo punto di verita' nel DB.
--
-- Punto unico (regola L): il selettore vive in `orchestrator::intent` e legge
-- questa chiave. Tutti i call site di classificazione delegano a quel selettore
-- invece di scegliere il motore in proprio.
--
-- Default 'python': il path vivo resta INVARIATO finche' l'admin non promuove
-- esplicitamente 'rust'. Niente magic-fallback hardcoded nel codice: il valore
-- letto dal DB e' l'unica fonte (un valore mancante/ignoto -> motore stabile
-- 'python', loggato). Cache 60s lato Rust (stesso pattern degli altri
-- settings.routing.*). Verso la rimozione della dipendenza HTTP
-- /classify-intent-agentic. Idempotente.
INSERT INTO settings (key, value, category, description)
VALUES (
    'routing.classifier_engine',
    'python',
    'routing',
    'Motore di classificazione intent: ''python'' (default, endpoint brain /classify-intent-agentic) oppure ''rust'' (crate::intent_classifier::classify in-process). Punto unico di scelta in orchestrator::intent (regola L). Cache 60s. Valore ignoto -> ''python'' (motore stabile).'
)
ON CONFLICT (key) DO NOTHING;
