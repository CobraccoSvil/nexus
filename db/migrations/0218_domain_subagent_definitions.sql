-- Migrazione 0218: kind sub-agent domain-specific (Componente C dell'allineamento).
--
-- Porta i miei agenti Claude Code (rust/python/frontend/db/doc/test) DENTRO
-- Nexus come kind runtime MULTI-PROVIDER: ogni kind risolve il modello via
-- model_purpose -> nexus_purpose_model (tier-based, mig 0203) -> routing matrix.
-- Eseguibili su QUALSIASI provider, dati locali, niente nomi modello hardcoded
-- (regola G). I prompt sono in mig 0219; la whitelist abilitante in mig 0220.

-- Purpose model per i nuovi kind (tier-based; provider/model_id = solo fallback
-- degenere se il catalog non ha candidati per quel tier, MAI un modello scelto).
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes) VALUES
    ('worker_code_rust',     'deepseek', 'deepseek-v4-flash', 'medium', 'code',      true,  'C: rust_implementer, medium/code (mig 0218)'),
    ('worker_code_python',   'deepseek', 'deepseek-v4-flash', 'medium', 'code',      true,  'C: python_implementer, medium/code (mig 0218)'),
    ('worker_code_frontend', 'deepseek', 'deepseek-v4-flash', 'medium', 'code',      true,  'C: frontend_implementer, medium/code (mig 0218)'),
    ('worker_code_test',     'deepseek', 'deepseek-v4-flash', 'medium', 'code',      true,  'C: test_author, medium/code (mig 0218)'),
    ('worker_reasoning_db',  'deepseek', 'deepseek-v4-pro',   'medium', 'reasoning', true,  'C: db_architect, medium/reasoning (mig 0218)'),
    ('worker_doc',           'openai',   'gpt-4.1-nano',      'light',  NULL,        false, 'C: doc_writer, light tool-capable (mig 0218)')
ON CONFLICT (purpose) DO UPDATE SET
    tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = NOW();

-- Definizioni dei 6 kind domain-specific.
INSERT INTO nexus_subagent_definitions (kind, description, prompt_key, tool_whitelist, model_purpose, max_iterations, timeout_s, is_background) VALUES
    ('rust_implementer',
     'Implementa modifiche al backend Rust (crates/mcp-core, nexus-orchestrator, microservizi): endpoint, worker, agent kind, MCP tool.',
     'subagent.rust_implementer.base',
     ARRAY['read_file','write_file','edit_file','run_command','run_tests','search_in_files','list_files','search_codebase_semantic','recall_context','nexus_search_semantic'],
     'worker_code_rust', 30, 600, false),
    ('python_implementer',
     'Implementa modifiche al brain Python (LangGraph nodes, memory, providers, router semantico).',
     'subagent.python_implementer.base',
     ARRAY['read_file','write_file','edit_file','run_command','run_tests','search_in_files','list_files','search_codebase_semantic','recall_context','nexus_search_semantic'],
     'worker_code_python', 30, 600, false),
    ('frontend_implementer',
     'Implementa modifiche al frontend (apps/web-ide Next.js, admin, componenti React, responsive, i18n).',
     'subagent.frontend_implementer.base',
     ARRAY['read_file','write_file','edit_file','run_command','run_playwright_tests','search_in_files','list_files','search_codebase_semantic','recall_context'],
     'worker_code_frontend', 30, 600, false),
    ('db_architect',
     'Progetta schema DB: migrazioni Postgres, collection Qdrant, tabella settings, routing matrix.',
     'subagent.db_architect.base',
     ARRAY['read_file','write_file','edit_file','run_command','search_in_files','list_files','search_codebase_semantic','recall_context'],
     'worker_reasoning_db', 20, 300, false),
    ('doc_writer',
     'Scrive/aggiorna documentazione nel meta-vault (ADR, runbook, architecture, decisioni).',
     'subagent.doc_writer.base',
     ARRAY['read_file','write_file','edit_file','search_in_files','list_files','recall_context','knowledge_search'],
     'worker_doc', 15, 240, false),
    ('test_author',
     'Scrive test: Playwright E2E, Rust unit/integration, Python pytest. Idempotenti, niente flakiness.',
     'subagent.test_author.base',
     ARRAY['read_file','write_file','edit_file','run_command','run_tests','run_specific_test','run_playwright_tests','search_in_files','list_files','search_codebase_semantic','recall_context'],
     'worker_code_test', 25, 300, false)
ON CONFLICT (kind) DO UPDATE SET
    description = EXCLUDED.description,
    prompt_key = EXCLUDED.prompt_key,
    tool_whitelist = EXCLUDED.tool_whitelist,
    model_purpose = EXCLUDED.model_purpose,
    max_iterations = EXCLUDED.max_iterations,
    timeout_s = EXCLUDED.timeout_s,
    updated_at = NOW();
