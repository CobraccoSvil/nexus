-- 0439: MVP "ultra come flusso di default" — parametri DB (regola G).
--
-- Ultra e' un paradigma di FLUSSO ortogonale al routing provider: decomposizione
-- parallela dei sotto-task indipendenti + verifica avversariale a panel. Non e'
-- una scelta utente (niente dropdown standard/ultra): e' il comportamento
-- standard, adattivo alla complessita' (il planner si attiva su task complessi
-- via is_eligible_adaptive; su task semplici il flusso resta diretto).
--
-- Due componenti:
--   B (decomposizione parallela): il DAG parallelo (dispatch_subagents a ondate)
--     prima scattava solo se almeno un todo aveva depends_on espliciti. Col
--     planner riformulato (0436) i todo dichiarano le dipendenze REALI, quindi i
--     todo SENZA depends_on sono davvero indipendenti -> il caso piu'
--     parallelizzabile. dag_parallel_min_ready abilita il parallelo anche per
--     loro (>= N todo ready). Punto unico: dag_scheduler.should_parallelize.
--   A (panel di verifica avversariale): al posto del singolo check esplorativo,
--     K verificatori indipendenti con lenti diverse (correttezza, sicurezza,
--     casi limite) valutano in parallelo; se >= consensus segnalano un problema
--     il todo non passa. Punto di innesto: verifier_node._run_verify_panel.
--
-- Prerequisiti gia' attivi nel DB (verificati): dag_parallel_enabled=true,
-- dag_topological_enabled=true, plan_phase_enabled=true, adaptive_gating=true.
--
-- Idempotente: ON CONFLICT (key) DO NOTHING (non sovrascrive scelte admin).

INSERT INTO settings (key, value, category, description) VALUES
    (
        'orchestrator.dag_parallel_min_ready',
        '2',
        'orchestrator',
        'Ultra (decomposizione parallela): numero minimo di todo ready per attivare il DAG parallelo anche senza dipendenze esplicite (todo indipendenti). <= 1 = comportamento storico (parallelo solo con depends_on).'
    ),
    (
        'orchestrator.verify_panel_enabled',
        'true',
        'orchestrator',
        'Ultra (verifica avversariale): se true, dopo i criteri deterministici passati un panel di K verificatori con lenti diverse valuta il todo; sostituisce il singolo check esplorativo.'
    ),
    (
        'orchestrator.verify_panel_size',
        '3',
        'orchestrator',
        'Ultra: numero di verificatori (lenti) del panel avversariale. Cap sulla lista verify_panel_lenses.'
    ),
    (
        'orchestrator.verify_panel_consensus',
        '2',
        'orchestrator',
        'Ultra: numero minimo di verificatori che devono segnalare un problema perche'' il todo non passi (consenso). Con 2 su 3 = maggioranza.'
    ),
    (
        'orchestrator.verify_panel_lenses',
        'correttezza,sicurezza,casi limite',
        'orchestrator',
        'Ultra: lenti del panel avversariale (csv). Ogni lente guarda il task da un angolo diverso. Lenti note: correttezza, sicurezza, casi limite, performance.'
    )
ON CONFLICT (key) DO NOTHING;
