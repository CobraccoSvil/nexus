-- PR-3 — Prompt keys per clarifying questions (Codex) + auto-delegation (Cursor)
-- + project_instructions injection (AGENTS.md). Schema reale: content (non template),
-- + category obbligatoria (CHECK in {'system','quality','automation','chat','docs','profile','agent'}),
-- + title obbligatorio.

INSERT INTO nexus_prompt_templates (key, category, title, content, schema_type, updated_by, updated_at)
VALUES (
    'agent.clarifying.detect',
    'agent',
    'Clarifying questions detector (pre-flight)',
    $$<role>Pre-flight ambiguity detector. Esamina il task utente e decide se servono chiarimenti.</role>

<output_format>
Se il task e' chiaro (stack, scope, obiettivo determinabili), rispondi SOLO con: NO_CLARIFICATION_NEEDED

Se ambiguo, emetti il tool `request_clarification` con max {{max_questions}} domande mirate.
Ogni domanda DEVE avere:
  - id: short slug (es. "stack", "auth_mode")
  - question: testo in italiano, una sola frase
  - suggested_default: la risposta di default in caso di modalita' Automatico

NON inventare domande superflue. Se 1 sola e' davvero necessaria, fai 1.
</output_format>

<examples>
Task: "Fai una app per noleggio auto" → AMBIGUO (stack? frontend? auth?)
Task: "Aggiungi log strutturato in src/server.ts" → CHIARO
Task: "Correggi il bug X nel file Y" → CHIARO se Y esiste
</examples>$$,
    'plain',
    'system',
    NOW()
)
ON CONFLICT (key) DO UPDATE SET content = EXCLUDED.content, updated_at = NOW();

INSERT INTO nexus_prompt_templates (key, category, title, content, schema_type, updated_by, updated_at)
VALUES (
    'agent.clarifying.defaults_applied',
    'agent',
    'Clarifying questions: default applicati (trasparenza)',
    $$<assunzioni_applicate>
Modalita': {{behavior_mode}} → ipotesi applicate automaticamente (utente NON ha confermato):

{{defaults_block}}

Includi una sezione "## Assunzioni" nel PRD/plan markdown con questa lista, cosi' l'utente puo' correggere se necessario.
</assunzioni_applicate>$$,
    'plain',
    'system',
    NOW()
)
ON CONFLICT (key) DO UPDATE SET content = EXCLUDED.content, updated_at = NOW();

INSERT INTO nexus_prompt_templates (key, category, title, content, schema_type, updated_by, updated_at)
VALUES (
    'system.project_instructions_block',
    'system',
    'Project instructions block (AGENTS.md style)',
    $$<project_instructions>
File: {{file_path}} (live, content_hash={{content_hash}})

{{content}}
</project_instructions>

NOTA: questo blocco e' istruzione vincolante del progetto. Rispettalo in tutte le decisioni di stack/codice/test.$$,
    'plain',
    'system',
    NOW()
)
ON CONFLICT (key) DO UPDATE SET content = EXCLUDED.content, updated_at = NOW();

INSERT INTO nexus_prompt_templates (key, category, title, content, schema_type, updated_by, updated_at)
VALUES (
    'system.available_subagents_block',
    'system',
    'Available sub-agents block (Cursor auto-delegation)',
    $$<available_subagents>
Sub-agent kinds disponibili (puoi delegare via tool `dispatch_subagent`):

{{subagents_block}}

Quando un sotto-task ha scope indipendente, delega SUBITO al kind piu' adatto invece di farlo inline.
Sub-agent in parallelo: puoi emettere N `dispatch_subagent` nello stesso turno (fino a {{max_parallel}}).
</available_subagents>$$,
    'plain',
    'system',
    NOW()
)
ON CONFLICT (key) DO UPDATE SET content = EXCLUDED.content, updated_at = NOW();

INSERT INTO nexus_prompt_templates (key, category, title, content, schema_type, updated_by, updated_at)
VALUES (
    'agent.verifier.base',
    'agent',
    'Verifier: sintesi finale post-DoD',
    $$<role>Sintesi finale post-verifier deterministico.</role>

<contesto>
Tutti i todo del plan sono completati. Stato:
{{todos_summary}}

Acceptance criteria verificati:
{{criteria_results}}
</contesto>

<output_format>
Produci un riepilogo finale conciso (max 600 char) per l'utente:
1. Cosa e' stato fatto (1-2 frasi)
2. Cosa e' stato verificato deterministicamente (1-2 frasi)
3. Eventuali todo `blocked` o `skipped` (se presenti)
Niente emoji, niente markdown headings, plain text.
</output_format>$$,
    'plain',
    'system',
    NOW()
)
ON CONFLICT (key) DO UPDATE SET content = EXCLUDED.content, updated_at = NOW();

-- Settings auto-delegation/project-override (idempotenti via WHERE).
INSERT INTO settings (key, value, updated_at) VALUES
    ('orchestrator.auto_delegation_enabled', 'true', NOW()),
    ('orchestrator.subagent_project_override_enabled', 'true', NOW()),
    ('orchestrator.subagent_parallel_in_round', 'true', NOW())
ON CONFLICT (key) DO UPDATE
SET value = EXCLUDED.value, updated_at = NOW()
WHERE settings.value IS DISTINCT FROM EXCLUDED.value;
