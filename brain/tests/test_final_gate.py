"""Test del final_gate (gate generale fail-closed anti-placeholder).

Verifica che `final_gate_node`:
  (a) task software + design orfano (main.tsx non importa nulla) -> rimanda
      all'executor (stop_reason tool_use + HumanMessage col verdetto);
  (b) task software + design montato -> chiude (stop_reason end_turn);
  (c) intent non software -> pass-through ({}).

Inoltre verifica `_resolve_log_command` (mig 0427, fix 2026-06-15) che risolve
il comando log del criterio service_logs_clean PER-PROGETTO:
  (d) override admin per project_id vince su tutto;
  (e) stack container (run_config docker) -> default docker compose;
  (f) stack native (run_config npm/cargo) -> template systemd con slug
      sostituito; conferma che lo slug riproduce l'algoritmo di
      project_workspace/logs.rs (`Beauty Book` -> `beauty-book`).
  (g) senza project_id e senza row in run_configurations -> fallback al
      default docker (retro-compatibilita').

Il tool_runner e' un fake che simula `run_command` (find) e `read_file` con lo
stesso stile di test_criteria_no_orphan.py.
"""
import contextlib
from unittest import mock

import pytest

from brain.agents import final_gate as fg


class _FakeResult:
    def __init__(self, s: str):
        self.result_json = s


def _make_runner(files: dict, find_map: dict):
    class _R:
        async def execute_tool(self, tool_name, tool_input, session_id, tool_use_id):
            if tool_name == "run_command":
                cmd = tool_input["command"]
                for d, lst in find_map.items():
                    if f"find {d} " in cmd:
                        return _FakeResult("EXIT CODE: 0\n" + "\n".join(lst))
                return _FakeResult("EXIT CODE: 0\n")
            if tool_name == "read_file":
                p = tool_input["path"]
                return _FakeResult(files.get(p, "[Errore] not found"))
            return _FakeResult("")
    return _R()


_STAGING = [f"figma_export/src/app/{x}" for x in (
    "App.tsx", "routes.tsx", "pages/LoginPage.tsx", "pages/BookingPage.tsx",
    "components/admin/AdminDashboard.tsx", "components/barber/CustomersTab.tsx",
    "services/bookingService.ts",
)]

_CFG = {
    "final_gate_enabled": True,
    "final_gate_software_intents": ["code", "debug", "scaffold", "implement", "build"],
    "final_gate_max_cycles": 2,
    "import_staging_dirs": ["figma_export"],
    "no_orphan_min_ratio": 0.4,
    "verifier_timeout_s": 10,
}


@pytest.fixture(autouse=True)
def _patch_cfg(monkeypatch):
    monkeypatch.setattr(fg.orchestrator_config, "get", lambda: dict(_CFG))
    yield


@pytest.mark.asyncio
async def test_software_orphan_design_reroutes_to_executor(monkeypatch):
    files = {"src/main.tsx": 'import React from "react";\nrender(<div>Hello World!</div>);'}
    monkeypatch.setattr(
        fg, "_tool_runner",
        _make_runner(files, {"figma_export": _STAGING, "src": ["src/main.tsx"]}),
    )
    state = {"user_intent": "debug", "session_id": "s1", "behavior_mode": "automatico"}
    out = await fg.final_gate_node(state)
    assert out["stop_reason"] == "tool_use"
    assert out["final_gate_cycle"] == 1
    assert out["pending_tool_uses"] == []
    assert out["messages"]
    assert "final_gate_failed" in out["messages"][0].content


@pytest.mark.asyncio
async def test_software_mounted_design_closes(monkeypatch):
    files = {
        "src/main.tsx": 'import App from "./app/App";',
        "src/app/App.tsx": 'import { router } from "./routes"; import {X} from "./components/admin/AdminDashboard";',
        "src/app/routes.tsx": 'import L from "./pages/LoginPage"; import B from "./pages/BookingPage";',
        "src/app/pages/LoginPage.tsx": 'import {b} from "../services/bookingService";',
        "src/app/pages/BookingPage.tsx": "export default function B(){}",
        "src/app/components/admin/AdminDashboard.tsx": 'import {C} from "../barber/CustomersTab";',
        "src/app/components/barber/CustomersTab.tsx": "export const C=1;",
        "src/app/services/bookingService.ts": "export const b=1;",
    }
    monkeypatch.setattr(
        fg, "_tool_runner",
        _make_runner(files, {"figma_export": _STAGING, "src": list(files.keys())}),
    )
    state = {"user_intent": "build", "session_id": "s1"}
    out = await fg.final_gate_node(state)
    # final_gate_passed: segnale per l'esito canonico CompletedVerified (mig 0386),
    # presente SOLO sul ramo "verifica passata" (non su forced_close/cap).
    assert out == {
        "final_gate_cycle": 0,
        "stop_reason": "end_turn",
        "final_gate_passed": True,
    }


@pytest.mark.asyncio
async def test_non_software_intent_passthrough(monkeypatch):
    monkeypatch.setattr(fg, "_tool_runner", _make_runner({}, {}))
    state = {"user_intent": "chat", "session_id": "s1"}
    out = await fg.final_gate_node(state)
    assert out == {}


# ── direttive rafforzate + conteggio errori (fix qualita' 2026-06-15) ───────


def test_count_build_errors_ts_and_rust():
    """Conta errori TS e Rust nello stesso output (best-effort, non parser esatto)."""
    output = (
        "src/foo.tsx(12,5): error TS2304: Cannot find name 'Customer'.\n"
        "src/bar.tsx(7,1): error TS6133: 'unused' is declared but never used.\n"
        "src/baz.tsx(3,2): error TS2322: Type 'string' is not assignable to type 'number'.\n"
        "error[E0277]: the trait bound is not satisfied\n"
    )
    assert fg._count_build_errors(output) == 4


def test_count_build_errors_empty():
    assert fg._count_build_errors("") == 0
    assert fg._count_build_errors("compilazione OK\n") == 0


def test_render_failed_block_includes_strong_directives_no_build():
    """Il blocco renderizzato contiene SEMPRE la direttiva 'leggi TUTTO l'output
    e correggi TUTTI gli errori', anche quando il criterio fallito non e' build."""
    state = {"behavior_mode": "automatico"}
    results = [
        {
            "type": "no_orphan_imported",
            "passed": False,
            "evidence": {"verdict": "design importato non raggiunto da src/main.tsx"},
        }
    ]
    body = fg._render_failed_block(state, cycle=1, max_cycles=2, results=results)
    body_lower = body.lower()
    assert "leggi tutto l'output qui sopra" in body_lower
    assert "correggi tutti gli errori" in body_lower
    assert "convergenza" in body_lower
    assert "no_orphan_imported" in body


def test_render_failed_block_build_excerpt_not_re_truncated_and_counts_errors():
    """Per il criterio BUILD (run_command con exit_code+output_total_chars
    nell'evidence): l'excerpt NON viene ri-tagliato a 900 char e il conteggio
    errori e' visibile all'agente."""
    long_output = "\n".join(
        f"src/file{i}.tsx({i},1): error TS2304: Cannot find name 'Foo{i}'."
        for i in range(20)
    )  # ~1400+ char, 20 errori
    results = [
        {
            "type": "run_command",
            "passed": False,
            "evidence": {
                "command": "npm run build",
                "exit_code": 2,
                "expected_exit": 0,
                "output_excerpt": long_output,
                "output_truncated": False,
                "output_total_chars": len(long_output),
            },
        }
    ]
    body = fg._render_failed_block({}, cycle=1, max_cycles=2, results=results)
    # Conteggio errori esposto.
    assert "errori rilevati: 20" in body
    # Direttiva specifica con il numero.
    assert "Numero di errori rilevati nel build: 20" in body
    # L'output integrale e' presente (l'ultimo errore TS sopravvive).
    assert "file19.tsx" in body
    # Convergenza richiesta.
    assert "CONVERGENZA" in body


def test_render_failed_block_signals_truncation_when_output_cut():
    """Quando l'output e' stato troncato dal runner, la nota 'output troncato'
    appare e l'agente sa di dover richiedere piu' contesto."""
    excerpt = "error TS9999: troncato\n" * 50  # 1100 char circa
    results = [
        {
            "type": "run_command",
            "passed": False,
            "evidence": {
                "command": "npm run build",
                "exit_code": 2,
                "expected_exit": 0,
                "output_excerpt": excerpt,
                "output_truncated": True,
                "output_total_chars": len(excerpt) + 10_000,
            },
        }
    ]
    body = fg._render_failed_block({}, cycle=1, max_cycles=2, results=results)
    assert "output troncato" in body
    assert "rilancia il build per leggere il resto" in body


# ── _resolve_log_command per-progetto (mig 0427) ────────────────────────────


def test_project_slug_riproduce_algoritmo_rust():
    """Lo slug Python deve coincidere col Rust (project_workspace/logs.rs):
    name.to_lowercase().replace([' ', '_'], '-'). Senza coincidenza, le unit
    systemd '{slug}-*.service' non vengono mai trovate dal journalctl."""
    assert fg._project_slug("Beauty Book") == "beauty-book"
    assert fg._project_slug("foo_bar baz") == "foo-bar-baz"
    assert fg._project_slug("Already-Slug") == "already-slug"


def _fake_db_pool_connect(rows_by_sql_keyword: dict):
    """Sostituto di brain.utils.db_pool.connect per i test:
    una mappa keyword-sotto-stringa-nello-SQL -> rows (lista di tuple).
    Per ogni `execute(query)`, ritorniamo le righe del primo keyword matchato.
    `fetchone` ritorna la prima riga, `fetchall` tutte. Niente DB reale, niente
    parser SQL: ci basta che il test guidi il flusso decisionale."""
    state = {"last_rows": []}

    cur = mock.MagicMock()

    def _execute(query, params=None):
        for kw, rows in rows_by_sql_keyword.items():
            if kw in query:
                state["last_rows"] = list(rows)
                return None
        state["last_rows"] = []
        return None

    def _fetchone():
        return state["last_rows"][0] if state["last_rows"] else None

    def _fetchall():
        return list(state["last_rows"])

    cur.execute.side_effect = _execute
    cur.fetchone.side_effect = _fetchone
    cur.fetchall.side_effect = _fetchall
    cur.__enter__ = lambda self: cur
    cur.__exit__ = lambda self, *a: False
    conn = mock.MagicMock()
    conn.cursor.return_value = cur

    @contextlib.contextmanager
    def _connect(*args, **kwargs):
        yield conn

    return _connect


_CFG_LOG = {
    "final_gate_runtime_log_command": "docker compose logs --tail 200 --no-color 2>&1 | tail -n 200",
}


def test_resolve_log_command_senza_project_id_default_docker():
    """Senza project_id, niente per-progettizzazione possibile -> docker default."""
    cmd = fg._resolve_log_command(state={}, cfg=_CFG_LOG)
    assert cmd == _CFG_LOG["final_gate_runtime_log_command"]


def test_resolve_log_command_override_admin_vince(monkeypatch):
    """Override admin esplicito (`runtime_log_command_per_project`, JSON
    {project_id: cmd}) vince su qualunque auto-detect."""
    pid = "11111111-1111-1111-1111-111111111111"
    fake = _fake_db_pool_connect({
        "runtime_log_command_per_project": [
            (f'{{"{pid}": "tail -n 50 /var/log/custom.log"}}',),
        ],
        # Le query successive non vengono mai eseguite perche' l'override
        # corto-circuita; metterle qui non cambia nulla, e' difensivo.
        "run_configurations": [("shell", "docker")],
        "FROM projects": [("Beauty Book",)],
        "runtime_log_command_systemd": [
            ('journalctl --user --user-unit "{slug}-*" --no-pager -n 200',),
        ],
    })
    with mock.patch("brain.utils.db_pool.connect", fake):
        cmd = fg._resolve_log_command(state={"project_id": pid}, cfg=_CFG_LOG)
    assert cmd == "tail -n 50 /var/log/custom.log"


def test_resolve_log_command_stack_container_usa_docker(monkeypatch):
    """Stack container (almeno una run_config essential e' docker/podman) ->
    default docker compose."""
    pid = "22222222-2222-2222-2222-222222222222"
    fake = _fake_db_pool_connect({
        "runtime_log_command_per_project": [("{}",)],
        "run_configurations": [
            ("shell", "docker"),
            ("shell", "make"),
        ],
        "FROM projects": [("Some Project",)],
        "runtime_log_command_systemd": [
            ('journalctl --user --user-unit "{slug}-*" --no-pager -n 200',),
        ],
    })
    with mock.patch("brain.utils.db_pool.connect", fake):
        cmd = fg._resolve_log_command(state={"project_id": pid}, cfg=_CFG_LOG)
    assert cmd == _CFG_LOG["final_gate_runtime_log_command"]


def test_resolve_log_command_stack_native_usa_systemd(monkeypatch):
    """Stack native (tutte le run_config essential sono npm/cargo/dotnet) ->
    template systemd con {slug} sostituito dal name del progetto."""
    pid = "33333333-3333-3333-3333-333333333333"
    fake = _fake_db_pool_connect({
        "runtime_log_command_per_project": [("{}",)],
        "run_configurations": [
            ("npm", "pnpm"),
            ("cargo", "cargo"),
        ],
        "FROM projects": [("Beauty Book",)],
        "runtime_log_command_systemd": [
            ('journalctl --user --user-unit "{slug}-*" --no-pager -n 200',),
        ],
    })
    with mock.patch("brain.utils.db_pool.connect", fake):
        cmd = fg._resolve_log_command(state={"project_id": pid}, cfg=_CFG_LOG)
    # {slug} sostituito con beauty-book, nessun residuo di placeholder.
    assert "beauty-book" in cmd
    assert "{slug}" not in cmd
    assert "journalctl --user --user-unit" in cmd


def test_resolve_log_command_native_senza_run_config_fallback_docker(monkeypatch):
    """Stack senza run_configurations essential (es. progetto neonato): non
    avendo segnali per dedurre lo stack, fallback al docker default (retro-
    compatibilita' col comportamento pre-0427)."""
    pid = "44444444-4444-4444-4444-444444444444"
    fake = _fake_db_pool_connect({
        "runtime_log_command_per_project": [("{}",)],
        "run_configurations": [],  # nessuna essential
        "FROM projects": [("Beauty Book",)],
        "runtime_log_command_systemd": [
            ('journalctl --user --user-unit "{slug}-*" --no-pager -n 200',),
        ],
    })
    with mock.patch("brain.utils.db_pool.connect", fake):
        cmd = fg._resolve_log_command(state={"project_id": pid}, cfg=_CFG_LOG)
    assert cmd == _CFG_LOG["final_gate_runtime_log_command"]


def test_resolve_log_command_override_json_invalido_passa_oltre(monkeypatch):
    """Override con JSON malformato: log debug, prosegue con auto-detect (non
    blocca il final_gate)."""
    pid = "55555555-5555-5555-5555-555555555555"
    fake = _fake_db_pool_connect({
        "runtime_log_command_per_project": [("non-e'-json-valido",)],
        "run_configurations": [("shell", "docker compose")],
        "FROM projects": [("Some Project",)],
        "runtime_log_command_systemd": [
            ('journalctl --user --user-unit "{slug}-*" --no-pager -n 200',),
        ],
    })
    with mock.patch("brain.utils.db_pool.connect", fake):
        cmd = fg._resolve_log_command(state={"project_id": pid}, cfg=_CFG_LOG)
    # Auto-detect ha visto docker compose -> docker default.
    assert cmd == _CFG_LOG["final_gate_runtime_log_command"]


def test_resolve_log_command_db_down_ritorna_docker_default(monkeypatch):
    """Se la connessione DB solleva, _resolve_log_command non rompe il
    final_gate: ritorna il docker default (regola H: meglio chiudere
    visibilmente che ammettere un override invisibile/inconsistente)."""
    pid = "66666666-6666-6666-6666-666666666666"

    @contextlib.contextmanager
    def _broken(*a, **kw):
        raise RuntimeError("DB down nel test")
        yield  # pragma: no cover

    with mock.patch("brain.utils.db_pool.connect", _broken):
        cmd = fg._resolve_log_command(state={"project_id": pid}, cfg=_CFG_LOG)
    assert cmd == _CFG_LOG["final_gate_runtime_log_command"]
