"""Test del final_gate (gate generale fail-closed anti-placeholder).

Verifica che `final_gate_node`:
  (a) task software + design orfano (main.tsx non importa nulla) -> rimanda
      all'executor (stop_reason tool_use + HumanMessage col verdetto);
  (b) task software + design montato -> chiude (stop_reason end_turn);
  (c) intent non software -> pass-through ({}).

Il tool_runner e' un fake che simula `run_command` (find) e `read_file` con lo
stesso stile di test_criteria_no_orphan.py.
"""
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
