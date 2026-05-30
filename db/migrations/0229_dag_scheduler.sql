-- Migrazione 0229: parallelizzazione DAG opt-in (Componente 3b).
--
-- Forma strutturata e mutuamente esclusiva del worker-mode (PR-C): quando
-- dag_parallel_enabled e' true e il piano ha dipendenze (depends_on), lo
-- scheduler esegue in parallelo i todo "ready" (tutte le dipendenze completate)
-- via il tool dispatch_subagents (Comp.0), a ondate con un cap conservativo.
--
-- Default-OFF e cap basso: il parallelismo e' sperimentale. Con il flag OFF il
-- comportamento e' il DAG topologico sequenziale (Comp.3a) o il loop storico.
-- dag_topological_enabled e' gia' in 0228.

INSERT INTO settings (key, value, category, description) VALUES
    ('orchestrator.dag_parallel_enabled', 'false', 'orchestrator',
     'Comp.3b: se true (e dag presente), i todo ready vengono eseguiti in parallelo via dispatch_subagents. Mutuamente esclusivo col worker-mode.'),
    ('orchestrator.dag_max_parallel', '2', 'orchestrator',
     'Comp.3b: numero massimo di todo eseguiti in parallelo per ondata (cap conservativo).'),
    ('orchestrator.dag_verify_layer', 'true', 'orchestrator',
     'Comp.3b: se true, dopo ogni ondata parallela verifica i todo completati prima di procedere al layer successivo.')
ON CONFLICT (key) DO NOTHING;
