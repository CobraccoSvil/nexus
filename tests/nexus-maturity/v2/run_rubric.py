#!/usr/bin/env python3
"""Nexus maturity v2 — rubric runner automatizzato (PR-4 Livello 5).

Esegue le 12 dimensioni della rubrica (D1-D12) su un agent run reale e
produce un report markdown + JSON.

Modalita' di esecuzione:
  1. Replay di un run esistente: `python run_rubric.py --run-id <uuid>`
     legge agent_runs/agent_steps/nexus_agent_todos/nexus_subagent_runs dal
     DB e calcola le dimensioni passive (D1, D2, D6, D7, D8, D10, D11, D12).
  2. Live: `python run_rubric.py --live --project-id <uuid>` lancia un nuovo
     scaffold via API e poi misura.

Note: le dimensioni D3 (PRD completeness), D4 (schema coherence), D5
(`pnpm verify`) richiedono accesso al filesystem del progetto generato
(workspaces.absolute_path) e tool esterni — se assenti vengono marcati
"n/a" nel report.

Target massimo: 36/36 (3 punti per dimensione).
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, asdict, field
from pathlib import Path
from typing import Optional

try:
    import psycopg2
    import psycopg2.extras
except ImportError:
    print("ERRORE: pip install psycopg2-binary", file=sys.stderr)
    sys.exit(2)


@dataclass
class DimScore:
    dim: str
    label: str
    score: int  # 0..3
    evidence: str
    note: str = ""


@dataclass
class RubricReport:
    run_id: str
    timestamp: str
    project_root: Optional[str]
    dimensions: list[DimScore] = field(default_factory=list)

    @property
    def total(self) -> int:
        return sum(d.score for d in self.dimensions)

    @property
    def max_total(self) -> int:
        return 3 * len(self.dimensions)

    def maturity_label(self) -> str:
        t = self.total
        if t >= 30:
            return "production-ready"
        if t >= 22:
            return "maturo (gap mirati)"
        if t >= 14:
            return "immaturo (serve hardening)"
        return "non-pronto (ridisegno)"


# ── Helpers DB ──────────────────────────────────────────────────────────────

def db_connect():
    url = os.environ.get("DATABASE_URL", "postgres://nexus:nexus@localhost:5433/nexus")
    return psycopg2.connect(url, cursor_factory=psycopg2.extras.RealDictCursor)


def fetch_run(conn, run_id: str) -> Optional[dict]:
    with conn.cursor() as cur:
        cur.execute(
            """SELECT ar.id::text AS id, ar.status, ar.iteration_count,
                      ar.provider, ar.model, ar.created_at, ar.completed_at,
                      ar.total_cost, ar.total_tokens, ar.project_id::text,
                      ar.final_answer,
                      EXTRACT(EPOCH FROM (COALESCE(ar.completed_at,NOW())-ar.created_at))::int AS dur_s,
                      w.absolute_path AS project_root
               FROM agent_runs ar
               LEFT JOIN workspaces w ON w.project_id = ar.project_id AND w.is_primary = true
               WHERE ar.id = %s""",
            (run_id,),
        )
        return cur.fetchone()


def fetch_steps_summary(conn, run_id: str) -> dict:
    with conn.cursor() as cur:
        cur.execute(
            """SELECT
                 COUNT(*) AS total,
                 COUNT(*) FILTER (WHERE status = 'failed') AS failed,
                 COUNT(*) FILTER (WHERE status = 'completed') AS completed,
                 COUNT(*) FILTER (WHERE tool_name = 'write_file' AND status='completed') AS files_written,
                 COUNT(*) FILTER (WHERE tool_name = 'run_command' AND status='completed') AS cmds_run
               FROM agent_steps WHERE run_id = %s""",
            (run_id,),
        )
        return dict(cur.fetchone() or {})


def fetch_todos_summary(conn, run_id: str) -> dict:
    with conn.cursor() as cur:
        cur.execute(
            """SELECT status, COUNT(*) AS n FROM nexus_agent_todos
               WHERE run_id = %s GROUP BY status""",
            (run_id,),
        )
        return {r["status"]: r["n"] for r in cur.fetchall()}


def fetch_subagent_summary(conn, run_id: str) -> dict:
    with conn.cursor() as cur:
        cur.execute(
            """SELECT status, COUNT(*) AS n, SUM(cost_usd) AS cost
               FROM nexus_subagent_runs
               WHERE parent_run_id::text = %s GROUP BY status""",
            (run_id,),
        )
        return {r["status"]: {"n": r["n"], "cost": float(r["cost"] or 0)} for r in cur.fetchall()}


def fetch_verifier_summary(conn, run_id: str) -> dict:
    with conn.cursor() as cur:
        cur.execute(
            """SELECT COUNT(*) AS total,
                      COUNT(*) FILTER (WHERE passed) AS passed
               FROM nexus_agent_verifier_runs WHERE run_id::text = %s""",
            (run_id,),
        )
        return dict(cur.fetchone() or {})


# ── Dimensioni D1-D12 ───────────────────────────────────────────────────────

def dim_d1_iterazioni(run: dict) -> DimScore:
    iters = int(run.get("iteration_count") or 0)
    score = 3 if iters <= 5 else 2 if iters <= 15 else 1 if iters <= 40 else 0
    return DimScore("D1", "Iterazioni totali", score,
                    f"iteration_count = {iters}",
                    "<=5 ideale, >40 immaturo")


def dim_d2_categorie_fix(steps: dict) -> DimScore:
    # Proxy: tool failed durante il run = "fix necessari".
    fails = int(steps.get("failed") or 0)
    score = 3 if fails == 0 else 2 if fails <= 3 else 1 if fails <= 10 else 0
    return DimScore("D2", "Categorie fix necessari", score,
                    f"step falliti = {fails}",
                    "tool_result con status=failed (proxy per A/B/D/E/F)")


def dim_d3_prd_completezza(project_root: Optional[str]) -> DimScore:
    if not project_root or not Path(project_root).exists():
        return DimScore("D3", "Completezza PRD", 0, "project_root assente", "n/a")
    prd_paths = [Path(project_root)/"PRD.md", Path(project_root)/"docs"/"PRD.md", Path(project_root)/"docs"/"prd.md"]
    prd = next((p for p in prd_paths if p.exists()), None)
    if not prd:
        return DimScore("D3", "Completezza PRD", 0, "PRD.md non trovato", "")
    content = prd.read_text(encoding="utf-8", errors="ignore").lower()
    sections = {"attori": "attor" in content or "actors" in content,
                "casi_uso": "casi d" in content or "use case" in content or "caso d" in content,
                "nfr": "non funziona" in content or "non-functional" in content or "performance" in content,
                "stack": "stack" in content or "tecnologie" in content}
    n = sum(sections.values())
    score = 3 if n == 4 else 2 if n == 3 else 1 if n == 2 else 0
    return DimScore("D3", "Completezza PRD", score,
                    f"sezioni rilevate: {sections}",
                    str(prd))


def dim_d4_schema_db(project_root: Optional[str]) -> DimScore:
    if not project_root or not Path(project_root).exists():
        return DimScore("D4", "Coerenza schema DB", 0, "n/a", "")
    candidates = list(Path(project_root).rglob("schema.prisma"))
    candidates += list(Path(project_root).rglob("*.sql"))
    candidates += list(Path(project_root).rglob("models.py"))
    candidates = [p for p in candidates if "node_modules" not in str(p) and ".venv" not in str(p)]
    if not candidates:
        return DimScore("D4", "Coerenza schema DB", 0, "nessun file schema trovato", "")
    txt = "\n".join(p.read_text(encoding="utf-8", errors="ignore") for p in candidates[:5])
    # Heuristics: ha tabelle + FK + indici?
    has_tables = bool(re.search(r"(?i)(create table|model\s+\w+|class\s+\w+\(.*SQLModel)", txt))
    has_fk = "foreign key" in txt.lower() or "references" in txt.lower() or "foreign_key=" in txt.lower() or "back_populates=" in txt.lower()
    has_idx = "index" in txt.lower()
    score = sum([has_tables, has_fk, has_idx])
    return DimScore("D4", "Coerenza schema DB", score,
                    f"tables={has_tables} fk={has_fk} index={has_idx}",
                    f"{len(candidates)} file analizzati")


def dim_d5_verify(project_root: Optional[str]) -> DimScore:
    if not project_root or not Path(project_root).exists():
        return DimScore("D5", "pnpm verify (o equivalente)", 0, "n/a", "")
    pj = Path(project_root)
    # Detect stack
    if (pj/"package.json").exists():
        cmd = ["pnpm", "verify"]
    elif (pj/"Cargo.toml").exists():
        cmd = ["cargo", "check"]
    elif (pj/"pyproject.toml").exists() or (pj/"requirements.txt").exists():
        cmd = ["python", "-m", "pytest", "--collect-only"]
    else:
        return DimScore("D5", "pnpm verify", 0, "stack non riconosciuto", "")
    try:
        r = subprocess.run(cmd, cwd=str(pj), capture_output=True, timeout=60)
        score = 3 if r.returncode == 0 else 1
        return DimScore("D5", "pnpm verify", score,
                        f"{' '.join(cmd)} → exit {r.returncode}",
                        r.stderr.decode("utf-8", errors="ignore")[:200])
    except (FileNotFoundError, subprocess.TimeoutExpired) as exc:
        return DimScore("D5", "pnpm verify", 0,
                        f"{' '.join(cmd)} fallito: {exc}", "")


def dim_d6_violazioni_qualita(project_root: Optional[str]) -> DimScore:
    if not project_root or not Path(project_root).exists():
        return DimScore("D6", "Violazioni qualita", 0, "n/a", "")
    pj = Path(project_root)
    hits = {"hardcoded_models": 0, "unwrap_outside_test": 0, "payload_log": 0, "emoji_in_code": 0}
    for f in pj.rglob("*"):
        if not f.is_file() or any(x in str(f) for x in ["node_modules", ".venv", ".git", "target/"]):
            continue
        if f.suffix not in (".rs", ".py", ".ts", ".tsx", ".js"):
            continue
        try:
            txt = f.read_text(encoding="utf-8", errors="ignore")
        except Exception:
            continue
        for m in ("claude-sonnet", "claude-haiku", "claude-opus", "gpt-4", "gpt-5", "gemini-2", "deepseek-chat", "mistral-large"):
            if m in txt:
                hits["hardcoded_models"] += 1
                break
        if f.suffix == ".rs" and ".unwrap()" in txt and "#[test]" not in txt and "#[cfg(test)]" not in txt:
            hits["unwrap_outside_test"] += 1
        if re.search(r"(?i)(?:log(?:ger)?|print).*(?:payload|prompt|response)\s*[=:]", txt):
            hits["payload_log"] += 1
        if re.search(r"[☀-➿\U0001F300-\U0001FAFF]", txt):
            hits["emoji_in_code"] += 1
    total_violations = sum(hits.values())
    score = 3 if total_violations == 0 else 2 if total_violations <= 2 else 1 if total_violations <= 8 else 0
    return DimScore("D6", "Violazioni qualita", score, f"hits = {hits}", f"totale = {total_violations}")


def dim_d7_loop_sterili(conn, run_id: str) -> DimScore:
    with conn.cursor() as cur:
        cur.execute(
            "SELECT COUNT(*) AS n FROM agent_runs WHERE id = %s AND final_answer LIKE %s",
            (run_id, "%loop_detected%"),
        )
        loop_runs = (cur.fetchone() or {}).get("n", 0)
    score = 3 if loop_runs == 0 else 1
    return DimScore("D7", "Loop sterili intercettati", score,
                    f"runs con loop_detected = {loop_runs}", "")


def dim_d8_autocorrezione(verifier: dict) -> DimScore:
    total = int(verifier.get("total") or 0)
    passed = int(verifier.get("passed") or 0)
    if total == 0:
        return DimScore("D8", "Auto-correzione interna", 1, "nessun verifier run", "verifier disattivo o run skipped")
    ratio = passed / total
    score = 3 if ratio >= 0.9 else 2 if ratio >= 0.6 else 1 if ratio > 0 else 0
    return DimScore("D8", "Auto-correzione interna", score,
                    f"verifier passed/total = {passed}/{total} ({ratio:.0%})",
                    "")


def dim_d9_contamination(project_root: Optional[str]) -> DimScore:
    # Verifica che /home/administrator/ideai non sia stato toccato.
    ideai = Path("/home/administrator/ideai")
    if not ideai.exists():
        return DimScore("D9", "Contamination Nexus", 0, "ideai non trovato", "")
    try:
        r = subprocess.run(["git", "diff", "--shortstat"], cwd=ideai, capture_output=True, timeout=10)
        diff = r.stdout.decode("utf-8", errors="ignore").strip()
        # Diff vuoto OR contiene solo file di test temporanei = OK
        contaminated = bool(diff and "files changed" in diff)
        score = 0 if contaminated else 3
        return DimScore("D9", "Contamination Nexus", score,
                        diff or "git diff pulito", "monorepo /home/administrator/ideai")
    except Exception as e:
        return DimScore("D9", "Contamination Nexus", 1, f"git diff fallito: {e}", "")


def dim_d10_costo(run: dict) -> DimScore:
    cost = float(run.get("total_cost") or 0)
    score = 3 if cost <= 0.5 else 2 if cost <= 2 else 1 if cost <= 10 else 0
    return DimScore("D10", "Costo totale", score,
                    f"total_cost USD = {cost:.4f}", "")


def dim_d11_tempo(run: dict) -> DimScore:
    dur = int(run.get("dur_s") or 0)
    score = 3 if dur <= 60 else 2 if dur <= 600 else 1 if dur <= 1800 else 0
    return DimScore("D11", "Tempo totale", score, f"durata sec = {dur}", "")


def dim_d12_subagent_efficacia(subagents: dict) -> DimScore:
    completed = subagents.get("completed", {}).get("n", 0) if "completed" in subagents else 0
    failed = subagents.get("failed", {}).get("n", 0) if "failed" in subagents else 0
    timeout = subagents.get("timeout", {}).get("n", 0) if "timeout" in subagents else 0
    total = completed + failed + timeout
    if total == 0:
        return DimScore("D12", "Sub-agent efficacy", 1, "nessun sub-agent invocato", "n/a")
    ratio = completed / total
    score = 3 if ratio >= 0.9 else 2 if ratio >= 0.6 else 1
    return DimScore("D12", "Sub-agent efficacy", score,
                    f"completed/total = {completed}/{total}",
                    f"avg cost = {sum(s.get('cost',0) for s in subagents.values()) / max(total,1):.4f}")


# ── Entrypoint ──────────────────────────────────────────────────────────────

def assess(run_id: str) -> RubricReport:
    conn = db_connect()
    try:
        run = fetch_run(conn, run_id)
        if not run:
            raise SystemExit(f"run_id {run_id} non trovato in agent_runs")
        project_root = run.get("project_root")
        steps = fetch_steps_summary(conn, run_id)
        todos = fetch_todos_summary(conn, run_id)
        subagents = fetch_subagent_summary(conn, run_id)
        verifier = fetch_verifier_summary(conn, run_id)
        report = RubricReport(
            run_id=run_id,
            timestamp=time.strftime("%Y-%m-%dT%H:%M:%S"),
            project_root=project_root,
        )
        report.dimensions = [
            dim_d1_iterazioni(run),
            dim_d2_categorie_fix(steps),
            dim_d3_prd_completezza(project_root),
            dim_d4_schema_db(project_root),
            dim_d5_verify(project_root),
            dim_d6_violazioni_qualita(project_root),
            dim_d7_loop_sterili(conn, run_id),
            dim_d8_autocorrezione(verifier),
            dim_d9_contamination(project_root),
            dim_d10_costo(run),
            dim_d11_tempo(run),
            dim_d12_subagent_efficacia(subagents),
        ]
        return report
    finally:
        conn.close()


def render_markdown(report: RubricReport) -> str:
    lines = [
        f"# Nexus Maturity v2 — run {report.run_id}",
        f"",
        f"- timestamp: {report.timestamp}",
        f"- project_root: `{report.project_root}`",
        f"- **totale: {report.total} / {report.max_total}** ({report.maturity_label()})",
        f"",
        f"| Dim | Cosa misura | Score | Evidenza | Note |",
        f"|-----|-------------|-------|----------|------|",
    ]
    for d in report.dimensions:
        lines.append(f"| {d.dim} | {d.label} | **{d.score}/3** | {d.evidence} | {d.note} |")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run-id", required=True, help="UUID del run da valutare")
    ap.add_argument("--output", help="Path output markdown (default stdout)")
    ap.add_argument("--json", help="Path output JSON (opzionale)")
    args = ap.parse_args()

    report = assess(args.run_id)
    md = render_markdown(report)
    if args.output:
        Path(args.output).write_text(md, encoding="utf-8")
        print(f"[maturity-v2] report scritto in {args.output}")
    else:
        print(md)
    if args.json:
        data = asdict(report)
        Path(args.json).write_text(json.dumps(data, indent=2, default=str), encoding="utf-8")
        print(f"[maturity-v2] json scritto in {args.json}")
    sys.exit(0 if report.total >= 22 else 1)


if __name__ == "__main__":
    main()
