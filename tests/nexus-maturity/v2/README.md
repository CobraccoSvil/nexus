# Nexus Maturity v2

Rubrica automatica D1-D12 per misurare la qualita' di un agent run di scaffolding o code-fix.

## Uso

```bash
# Replay di un run storico
DATABASE_URL=postgres://nexus:nexus@localhost:5433/nexus \
  python tests/nexus-maturity/v2/run_rubric.py \
    --run-id 80a3d5a7-de47-47bd-ab78-771cfde1f63c \
    --output report.md \
    --json report.json

# Score di soglia: >= 22 / 36 → exit 0 (sufficiente)
```

## Dimensioni

| Dim | Cosa misura | Fonte dati |
|---|---|---|
| D1 | Iterazioni totali | `agent_runs.iteration_count` |
| D2 | Step falliti (proxy categorie fix) | `agent_steps.status = 'failed'` |
| D3 | Completezza PRD (attori, UC, NFR, stack) | filesystem `PRD.md` |
| D4 | Coerenza schema DB | filesystem `*.prisma`, `*.sql`, `models.py` |
| D5 | `pnpm verify` / `cargo check` / `pytest --collect-only` | exec |
| D6 | Violazioni qualita (modelli hardcoded, unwrap, emoji, payload log) | grep filesystem |
| D7 | Loop sterili intercettati | `agent_runs.final_answer LIKE '%loop_detected%'` |
| D8 | Auto-correzione (verifier passed ratio) | `nexus_agent_verifier_runs` |
| D9 | Contamination Nexus | `git diff /home/administrator/ideai` |
| D10 | Costo totale | `agent_runs.total_cost` |
| D11 | Tempo totale | `completed_at - created_at` |
| D12 | Sub-agent efficacy | `nexus_subagent_runs.status` |

## Interpretazione punteggio

- 30-36: production-ready
- 22-29: maturo (gap mirati)
- 14-21: immaturo (serve hardening)
- 0-13: ridisegno richiesto

## Differenze vs v1

| Aspetto | v1 (`rubric.md`) | v2 |
|---|---|---|
| Compilazione | manuale (markdown table) | automatica (Python + DB) |
| Esecuzione | a fine sessione | per ogni run, anche replay |
| Output | markdown editabile | markdown + JSON + exit code |
| CI | non integrabile | exit code 0/1 per gate |
