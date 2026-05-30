"""criteria_runner (PR-2): esegue i singoli acceptance_criteria del verifier.

Ogni criterion ha forma:
  {
    "id": "criterion-uuid",       # opzionale, generato se mancante
    "type": "run_command" | "http" | "file_exists" | "regex_in_output" | "db_query",
    "spec": {...},                # parametri specifici per tipo
    "expected": {...}              # condizione di successo
  }

Il verifier_node chiama `run_criterion(c, ctx)` per ogni criterion e
aggrega i risultati. NON e' un nodo LangGraph: e' chiamato sincrono
(via await) dal verifier_node. Nessun LLM call qui dentro (98%
deterministic).
"""
from __future__ import annotations

import asyncio
import logging
import os
import re
import time
import uuid
from typing import Any

logger = logging.getLogger(__name__)


async def run_criterion(criterion: dict[str, Any], ctx: dict[str, Any]) -> tuple[bool, dict[str, Any]]:
    """Esegue un singolo acceptance_criterion.

    Args:
        criterion: il dict del criterion (type, spec, expected)
        ctx: contesto runtime con `tool_runner`, `session_id`, `project_id`,
             `timeout_s` (override per-criterion ammesso)

    Returns:
        (passed: bool, evidence: dict) — evidence ha {type, output, duration_ms, error?}
    """
    started = time.monotonic()
    c_type = (criterion.get("type") or "").lower().strip()
    # Difesa: il planner LLM puo' generare `spec`/`expected` come STRINGA
    # invece che come dict (es. `"spec": "ls README.md"`). Senza questo check
    # il `spec.get(...)` negli handler sottostanti crasha con
    # `'str' object has no attribute 'get'` e il verifier marca cycle fallito
    # in loop fino a `blocked` (osservato nel run f14696bc). Normalizziamo a
    # dict cosi' gli handler ritornano "spec.command obbligatorio" leggibile.
    spec_raw = criterion.get("spec")
    if isinstance(spec_raw, dict):
        spec = spec_raw
    else:
        if spec_raw not in (None, "", {}):
            logger.warning(
                "criterion type=%s ha spec non-dict (%s): %r — normalizzo a {}",
                c_type, type(spec_raw).__name__, str(spec_raw)[:200],
            )
        spec = {}
    expected_raw = criterion.get("expected")
    if isinstance(expected_raw, dict):
        expected = expected_raw
    else:
        if expected_raw not in (None, "", {}):
            logger.warning(
                "criterion type=%s ha expected non-dict (%s): %r — normalizzo a {}",
                c_type, type(expected_raw).__name__, str(expected_raw)[:200],
            )
        expected = {}
    timeout_s = float(criterion.get("timeout_s") or ctx.get("timeout_s") or 30.0)

    if c_type == "run_command":
        ok, ev = await _check_run_command(spec, expected, ctx, timeout_s)
    elif c_type == "http":
        ok, ev = await _check_http(spec, expected, timeout_s)
    elif c_type == "file_exists":
        ok, ev = await _check_file_exists(spec, expected, ctx, timeout_s)
    elif c_type == "regex_in_output":
        ok, ev = await _check_regex_in_output(spec, expected, ctx, timeout_s)
    elif c_type == "db_query":
        ok, ev = await _check_db_query(spec, expected, timeout_s)
    else:
        ok = False
        ev = {"error": f"tipo di criterion sconosciuto: '{c_type}'"}

    ev["type"] = c_type
    ev["duration_ms"] = int((time.monotonic() - started) * 1000)
    return ok, ev


# ── run_command ───────────────────────────────────────────────────────────


async def _check_run_command(
    spec: dict[str, Any], expected: dict[str, Any], ctx: dict[str, Any], timeout_s: float,
) -> tuple[bool, dict[str, Any]]:
    cmd = spec.get("command") or ""
    if not cmd:
        return False, {"error": "spec.command obbligatorio"}
    tool_runner = ctx.get("tool_runner")
    session_id = ctx.get("session_id")
    if not tool_runner or not session_id:
        return False, {"error": "tool_runner o session_id assenti"}

    tool_input: dict[str, Any] = {"command": cmd}
    if spec.get("working_dir"):
        tool_input["working_dir"] = spec["working_dir"]

    try:
        result = await asyncio.wait_for(
            tool_runner.execute_tool(
                tool_name="run_command",
                tool_input=tool_input,
                session_id=str(session_id),
                tool_use_id=str(uuid.uuid4()),
            ),
            timeout=timeout_s,
        )
    except asyncio.TimeoutError:
        return False, {"error": "timeout", "command": cmd, "timeout_s": timeout_s}
    except Exception as exc:
        return False, {"error": f"execute_tool: {exc}", "command": cmd}

    raw = result.result_json or ""
    # `run_command` ritorna testo con "EXIT CODE: N" + STDOUT/STDERR
    m = re.search(r"EXIT CODE:\s*(-?\d+)", raw)
    actual_exit = int(m.group(1)) if m else None
    expected_exit = expected.get("exit_code", 0)

    passed = (actual_exit is not None and actual_exit == int(expected_exit))
    evidence = {
        "command": cmd,
        "exit_code": actual_exit,
        "expected_exit": expected_exit,
        "output_excerpt": raw[:600],
    }
    return passed, evidence


# ── http ──────────────────────────────────────────────────────────────────


async def _check_http(
    spec: dict[str, Any], expected: dict[str, Any], timeout_s: float,
) -> tuple[bool, dict[str, Any]]:
    url = spec.get("url") or ""
    method = (spec.get("method") or "GET").upper()
    if not url:
        return False, {"error": "spec.url obbligatorio"}

    try:
        import httpx  # type: ignore[import-untyped]
    except ImportError:
        return False, {"error": "httpx non installato"}

    expected_status = int(expected.get("status", 200))
    try:
        async with httpx.AsyncClient(timeout=timeout_s) as client:
            resp = await client.request(method, url)
            actual = resp.status_code
            body_excerpt = resp.text[:400]
    except Exception as exc:
        return False, {"error": f"http call: {exc}", "url": url}

    passed = actual == expected_status
    if "body_contains" in expected:
        needle = expected["body_contains"]
        passed = passed and (needle in body_excerpt or needle in resp.text)

    return passed, {
        "url": url,
        "method": method,
        "status": actual,
        "expected_status": expected_status,
        "body_excerpt": body_excerpt,
    }


# ── file_exists ──────────────────────────────────────────────────────────


async def _check_file_exists(
    spec: dict[str, Any], expected: dict[str, Any], ctx: dict[str, Any], timeout_s: float,
) -> tuple[bool, dict[str, Any]]:
    """Verifica che un file esista usando `list_files` sulla dir parent + match basename.

    Audit 27/05/2026: il vecchio approccio usava `read_file` + string match
    fragile (`"[Errore"`, `"non trovato"`). Falsi negativi visti quando:
    - `read_file` ritornava errore in inglese (`"[Error lettura...]"`)
    - `read_file` falliva con `canonicalize()` → "Percorso non autorizzato"
      anche per file legittimamente creati fuori dal canonicalize del root
    - Race condition tra `write_file` e `read_file` sul filesystem
    Ora usiamo `list_files` sulla dir parent (piu' tollerante) e matchiamo
    il basename: se la dir esiste e il file e' elencato, esiste davvero.
    Fallback a `read_file` con string match esteso se la dir non e' listabile.
    """
    path = spec.get("path") or ""
    if not path:
        return False, {"error": "spec.path obbligatorio"}
    tool_runner = ctx.get("tool_runner")
    session_id = ctx.get("session_id")
    if not tool_runner or not session_id:
        return False, {"error": "tool_runner o session_id assenti"}

    # ── 1. Tenta list_files sulla directory parent ─────────────────────────
    # Estrai parent dir e basename: posix-style perche' i path nei progetti
    # Nexus usano sempre /. Se path = "variables.txt", parent="." basename="variables.txt".
    parts = path.rsplit("/", 1)
    if len(parts) == 1:
        parent_dir = "."
        basename = parts[0]
    else:
        parent_dir = parts[0] or "."
        basename = parts[1]
    try:
        result = await asyncio.wait_for(
            tool_runner.execute_tool(
                tool_name="list_files",
                tool_input={"directory": parent_dir},
                session_id=str(session_id),
                tool_use_id=str(uuid.uuid4()),
            ),
            timeout=timeout_s,
        )
        raw = result.result_json or ""
        # list_files ritorna lista di entry (es. "- variables.txt", "📄 variables.txt").
        # Cerchiamo il basename come token isolato (con boundary spazio/inizio riga).
        # Match permissivo: il basename appare come parola intera nel raw.
        import re as _re
        pattern = r"(?:^|[\s/\"'`])" + _re.escape(basename) + r"(?:$|[\s\"'`])"
        list_indicates_present = bool(_re.search(pattern, raw, _re.MULTILINE))
        list_indicates_error = (
            raw.startswith("❌")
            or "[Errore" in raw[:30]
            or "[Error" in raw[:30]
            or "non trovato" in raw[:80].lower()
            or "not found" in raw[:80].lower()
        )
        if not list_indicates_error:
            # list_files ha avuto successo: la sua risposta e' la fonte di verita'
            exists = list_indicates_present
            expected_exists = bool(expected.get("exists", True))
            passed = (exists == expected_exists)
            return passed, {
                "path": path,
                "exists": exists,
                "expected_exists": expected_exists,
                "method": "list_files",
                "parent_dir": parent_dir,
                "basename": basename,
                "output_excerpt": raw[:300],
            }
    except (asyncio.TimeoutError, Exception) as exc:
        # list_files fallito, prova read_file come fallback (vedi sotto)
        list_err = str(exc)
    else:
        list_err = "list_files ha ritornato errore: " + raw[:80]

    # ── 2. Fallback: read_file con string match esteso ──────────────────────
    try:
        result = await asyncio.wait_for(
            tool_runner.execute_tool(
                tool_name="read_file",
                tool_input={"path": path},
                session_id=str(session_id),
                tool_use_id=str(uuid.uuid4()),
            ),
            timeout=timeout_s,
        )
    except asyncio.TimeoutError:
        return False, {"error": "timeout", "path": path, "list_err": list_err}
    except Exception as exc:
        return False, {"error": f"execute_tool: {exc}", "path": path, "list_err": list_err}

    raw = result.result_json or ""
    # Pattern di errore estesi per coprire IT/EN/altri formati
    error_markers = (
        raw.startswith("❌")
        or "[Errore" in raw[:60]
        or "[Error" in raw[:60]
        or "non trovato" in raw[:120].lower()
        or "not found" in raw[:120].lower()
        or "no such file" in raw[:120].lower()
        or "enoent" in raw[:120].lower()
    )
    exists = not error_markers
    expected_exists = bool(expected.get("exists", True))
    passed = (exists == expected_exists)

    return passed, {
        "path": path,
        "exists": exists,
        "expected_exists": expected_exists,
        "method": "read_file_fallback",
        "list_err": list_err,
        "output_excerpt": raw[:200],
    }


# ── regex_in_output (run_command + regex su stdout) ──────────────────────


async def _check_regex_in_output(
    spec: dict[str, Any], expected: dict[str, Any], ctx: dict[str, Any], timeout_s: float,
) -> tuple[bool, dict[str, Any]]:
    cmd = spec.get("command") or ""
    pattern = expected.get("pattern") or spec.get("pattern") or ""
    if not cmd or not pattern:
        return False, {"error": "spec.command e expected.pattern obbligatori"}
    tool_runner = ctx.get("tool_runner")
    session_id = ctx.get("session_id")
    if not tool_runner or not session_id:
        return False, {"error": "tool_runner o session_id assenti"}

    try:
        result = await asyncio.wait_for(
            tool_runner.execute_tool(
                tool_name="run_command",
                tool_input={"command": cmd},
                session_id=str(session_id),
                tool_use_id=str(uuid.uuid4()),
            ),
            timeout=timeout_s,
        )
    except asyncio.TimeoutError:
        return False, {"error": "timeout", "command": cmd}
    except Exception as exc:
        return False, {"error": f"execute_tool: {exc}", "command": cmd}

    raw = result.result_json or ""
    try:
        flags = re.MULTILINE | re.DOTALL if expected.get("multiline") else re.MULTILINE
        match = re.search(pattern, raw, flags)
    except re.error as exc:
        return False, {"error": f"regex invalida: {exc}", "pattern": pattern}

    passed = match is not None
    return passed, {
        "command": cmd,
        "pattern": pattern,
        "match": match.group(0) if match else None,
        "output_excerpt": raw[:400],
    }


# ── db_query (Postgres applicativo o nexus DB) ────────────────────────────


async def _check_db_query(
    spec: dict[str, Any], expected: dict[str, Any], timeout_s: float,
) -> tuple[bool, dict[str, Any]]:
    query = spec.get("query") or ""
    if not query:
        return False, {"error": "spec.query obbligatorio"}

    # connection_string: opzionale, default usa DATABASE_URL.
    # Per le app di progetto l'agente passa il connection string del DB applicativo
    # (es. postgres://nexus:nexus@localhost:5433/<slug>).
    conn_str = spec.get("connection_string") or os.environ.get("DATABASE_URL", "")
    if not conn_str:
        return False, {"error": "connection_string o DATABASE_URL obbligatori"}

    try:
        import psycopg2  # type: ignore[import-untyped]
        from psycopg2.extras import RealDictCursor  # type: ignore[import-untyped]
    except ImportError:
        return False, {"error": "psycopg2 non installato"}

    loop = asyncio.get_event_loop()

    def _run_query():
        conn = psycopg2.connect(conn_str, cursor_factory=RealDictCursor, connect_timeout=int(timeout_s))
        try:
            with conn.cursor() as cur:
                cur.execute(query)
                if cur.description:
                    rows = cur.fetchall()
                else:
                    rows = []
                return rows
        finally:
            conn.close()

    try:
        rows = await asyncio.wait_for(loop.run_in_executor(None, _run_query), timeout=timeout_s)
    except asyncio.TimeoutError:
        return False, {"error": "timeout", "query": query[:200]}
    except Exception as exc:
        return False, {"error": f"db_query: {exc}", "query": query[:200]}

    # Check possibili:
    # expected.row_count = int → confronto esatto
    # expected.min_rows / expected.max_rows → range
    # expected.value_eq = {column, value} → row[0][column] == value
    passed = True
    notes: list[str] = []

    if "row_count" in expected:
        target = int(expected["row_count"])
        if len(rows) != target:
            passed = False
            notes.append(f"row_count {len(rows)} != expected {target}")
    if "min_rows" in expected and len(rows) < int(expected["min_rows"]):
        passed = False
        notes.append(f"row_count {len(rows)} < min {expected['min_rows']}")
    if "max_rows" in expected and len(rows) > int(expected["max_rows"]):
        passed = False
        notes.append(f"row_count {len(rows)} > max {expected['max_rows']}")
    if "value_eq" in expected and rows:
        ve = expected["value_eq"] or {}
        col = ve.get("column")
        target = ve.get("value")
        actual = rows[0].get(col) if col else None
        if actual != target:
            passed = False
            notes.append(f"row[0].{col} = {actual!r} != expected {target!r}")

    return passed, {
        "query": query[:200],
        "row_count": len(rows),
        "rows_excerpt": [dict(r) for r in rows[:3]],
        "notes": notes,
    }
