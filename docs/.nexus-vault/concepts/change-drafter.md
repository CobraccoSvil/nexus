---
id: dfe9e5d7-4809-4d94-89b6-3063a2cb5f8b
kind: other
title: ChangeDrafter (modifica supervisionata)
slug: change-drafter
tags:
  - concept
  - change
  - approval
  - workflow
source_files:
  - crates/mcp-core/src/meta_docs/generators/concepts.rs
auto_generated: true
created_at: 2026-05-23T11:09:00Z
updated_at: 2026-06-04T05:07:03Z
nexus_meta_version: 1
---

# ChangeDrafter

Workflow di modifica codice/doc supervisionata. Quando un agente o un sub-agent vuole applicare modifiche non triviali, **propone prima** una struttura formale all'utente.

## Output proposto

```json
{
  "razionale": "Perche' questa modifica e' necessaria",
  "impact_analysis": {
    "files_to_modify": [...],
    "breaking_changes": bool,
    "migration_required": bool,
    "tests_to_update": [...]
  },
  "diff_proposto": "<unified diff>",
  "verification_steps": [...],
  "alternative_considerate": [...]
}
```

## UI

Il componente `<ChangeDraftCard>` mostra il draft nella chat con 3 azioni:
- **Applica** - esegue il diff, ri-verify, commit
- **Modifica** - editor inline (max 3 iter)
- **Annulla** - draft `dismissed` per learning

## Tabella

`change_drafts` traccia ogni draft con `status` (pending/approved/rejected/applied/superseded/dismissed).

Vedi [[postgres-tables]], [[sub-agents-claude-code]].
