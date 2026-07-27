-- 0594_thinking_matrix_suite_v2.sql
-- Fase 5 del gate di qualificazione: profilo thinking_matrix + bump suite v2.
--
-- La matrice riesegue il carico agentico reale in DUE configurazioni thinking
-- esplicite (off e native) e DERIVA agentic_thinking_policy dai fatti (punto
-- unico derive_thinking_policy in model_qualification.rs): era il campo che
-- nessuno verificava (una policy sbagliata = gemini-3 empty-completion, glm
-- dichiarato reasoning con policy inerte). uses_thinking_mode segue. Le righe
-- con capability_locked (curatela) NON vengono riscritte.
--
-- Gira SOLO sui modelli che dichiarano reasoning (applies_when): per gli altri
-- la policy del catalog resta invariata. Non-blocking: la matrice decide la
-- POLICY, non la promozione (un exclude derivato esclude comunque dal routing
-- agentico via agentic_thinking_policy <> 'exclude').
--
-- BUMP suite_version 1 -> 2 su tutti i profili: le righe qualificate con la
-- suite v1 (e il grandfather v0) diventano candidate alla RI-qualificazione
-- IN SHADOW (restano nel pool durante la prova) — e' il meccanismo con cui la
-- batteria evolve quando impariamo un nuovo modo di rompersi (design, fase 5).

UPDATE ai_model_probe_profile SET suite_version = 2;

INSERT INTO ai_model_probe_profile
  (profile_key, suite_version, ord, kind, is_blocking, applies_when, grants, payload, pass_predicate, enabled)
VALUES
  ('thinking_matrix', 2, 40, 'thinking_matrix', FALSE,
   '{"declared_capabilities_contains": "reasoning"}',
   '[]',
   '{"tool_names": ["read_file", "read_file_lines", "list_files", "search_in_files", "write_file", "edit_file", "run_command", "advisory_verdict"], "system_template_key": "system.nexus_base", "history_chars": 20000, "max_tokens": 4096, "timeout_s": 90, "repeat": 2, "thinking_budget_tokens": 2048}',
   '{"min_tool_calls": 1, "max_latency_ms": 60000, "promote_min_passes": 2}',
   TRUE)
ON CONFLICT (profile_key) DO NOTHING;
