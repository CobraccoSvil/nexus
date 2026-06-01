-- 0245_settings_plan_remainder.sql
-- M7 + M12 + M13 + M14 + M15 — Settings del piano (dump fedele dal DB prod).
-- Raggruppa le chiavi di configurazione delle milestone non-provider:
-- routing.degradation.* (Q-value), kb.* (ingest/autolink/lifecycle/intake),
-- impact.* + regression_gate.* (impact analysis), agent.todos.* (todo).
-- Regola G: unica fonte di verita. Idempotente (ON CONFLICT DO NOTHING).

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('agent.todos.carry_over_enabled', 'true', 'agent', 'M15.4: a fine run i todo pending/blocked vengono marcati carry_over=true (con origin_run_id) invece di restare orfani, cosi'' il planner del run successivo li eredita come backlog.', 'f'),
    ('agent.todos.live_events', 'true', 'agent', 'M15.1: emette eventi SSE live (TodoUpdated per todo + PlanUpdated finale) quando lo status di un todo cambia, dopo il commit della transazione.', 'f'),
    ('agent.todos.user_editable', 'true', 'agent', 'M15.3: abilita l''endpoint POST /api/agent/todos/{run_id}/edit per modificare i todo del piano dall''interfaccia utente (add/edit/reorder/remove).', 'f'),
    ('impact.depth_cap', '2', 'impact', 'Profondita'' massima di traversal nella forward closure dell''impact analysis (M13.4).', 'f'),
    ('impact.enabled', 'true', 'impact', 'Abilita il popolamento del code graph durante reindex_single_file (M13.1).', 'f'),
    ('impact.max_nodes', '60', 'impact', 'Numero massimo di nodi raccolti in una singola impact run (anti-esplosione).', 'f'),
    ('impact.test_informed_enabled', 'true', 'impact', 'Abilita il blocco <impact_brief> nel planner (M13.6): il planner vede impact set e test esistenti e genera todo di test/verifica mirati.', 'f'),
    ('impact.test_informed_max_listed_tests', '15', 'impact', 'Numero massimo di test esistenti elencati nel blocco <impact_brief> (anti-rumore nel prompt del planner).', 'f'),
    ('impact.test_informed_max_seed_paths', '12', 'impact', 'Numero massimo di seed path (file citati dall utente) inviati a tests-for-run in fase di planning.', 'f'),
    ('kb.autolink.enabled', 'true', 'kb', 'Abilita il link composer post-create note (M12.3).', 'f'),
    ('kb.autolink.semantic_threshold', '0.65', 'kb', 'Score minimo Qdrant per creare un link relates semantico.', 'f'),
    ('kb.autolink.semantic_top_k', '3', 'kb', 'Top-K note semanticamente simili da considerare per link relates.', 'f'),
    ('kb.autolink.wikilink_max_per_note', '10', 'kb', 'Cap wikilink esplicitamente risolti per nota (anti-DoS).', 'f'),
    ('kb.changelog_cross_enabled', 'true', 'kb', 'Abilita il cross-link dei changelog del meta-vault Nexus nella KB del meta-progetto Nexus (M12.4). No-op se Nexus non e'' registrato come progetto.', 'f'),
    ('kb.ingest.body_max_chars', '20000', 'kb', 'Max char del body_md ingestito (final_answer molto lunghi vengono troncati con suffisso).', 'f'),
    ('kb.ingest.cjk_max_ratio_pct', '20', 'knowledge', 'Hallucination guard kb.ingest: se >= N percento dei caratteri della final_answer e CJK (hiragana, katakana, hangul, hanzi), la nota agent_summary NON viene creata (probabile deriva semantica). 0 = disabilitato.', 'f'),
    ('kb.ingest.enabled', 'true', 'kb', 'Abilita ingestione automatica del final_answer in project_knowledge_notes (M12.1).', 'f'),
    ('kb.ingest.min_chars', '300', 'kb', 'Lunghezza minima del final_answer per essere ingestito come note (filtro substance).', 'f'),
    ('kb.ingest.title_max_chars', '120', 'kb', 'Max char per il title della note generato dal final_answer.', 'f'),
    ('kb.intake.confirm_if_implemented', 'true', 'kb', 'M14.4: se true, una richiesta gia'' implementata e verificata (contesto invariato) chiede conferma anche in modalita'' automatica prima di rifarla.', 'f'),
    ('kb.lifecycle.auto_deprecate_on_correction', 'true', 'kb', 'M14.2: quando una richiesta utente corregge una decisione esistente (verdetto intake correction) e il run completa, marca la nota vecchia deprecated e crea un link correction dalla nuova nota.', 'f'),
    ('kb.lifecycle.context_stale_enabled', 'true', 'kb', 'M14.3: marca context-stale le note active i cui file coperti vengono modificati da un run successivo non collegato alla nota (segnalazione, non cancellazione).', 'f'),
    ('regression_gate.enabled', 'true', 'regression_gate', 'Abilita il regression gate SOFT a fine run (M13.4): esegue i test dell impact set e avvisa senza bloccare.', 'f'),
    ('regression_gate.hard_block', 'false', 'regression_gate', 'Abilita il blocco HARD del regression gate (M13.5): se i test dell impact set falliscono il run e bloccato e l auto-commit non committa. Default-OFF (rollout).', 'f'),
    ('regression_gate.max_cycles', '1', 'regression_gate', 'Numero massimo di cicli fix-and-retest che il gate hard concede prima di bloccare definitivamente il run.', 'f'),
    ('regression_gate.max_tests', '10', 'regression_gate', 'Numero massimo di test dell impact set eseguiti dal gate per run (cap anti-latenza).', 'f'),
    ('regression_gate.soft_only', 'true', 'regression_gate', 'Forza modalita SOFT (solo warning, nota e todo). Il blocco hard e M13.5, non ancora implementato.', 'f'),
    ('regression_gate.test_timeout_s', '120', 'regression_gate', 'Timeout in secondi per singolo test eseguito dal regression gate.', 'f'),
    ('routing.degradation.cooldown_seconds', '3600', 'routing', 'Durata cooldown provider-intent (M7)', 'f'),
    ('routing.degradation.min_visits', '5', 'routing', 'Min visite prima di applicare degradation (M7)', 'f'),
    ('routing.degradation.threshold', '0.7', 'routing', 'Failure rate soglia per cooldown provider-intent (M7)', 'f')
ON CONFLICT (key) DO NOTHING;
