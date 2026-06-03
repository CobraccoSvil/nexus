"""Test del gate generale `no_orphan_imported` (criteria_runner).

Verifica il discriminante anti-placeholder: se esiste codice importato/staged
(es. figma_export/) con abbastanza moduli, l'entry servito dell'app deve
montarli via grafo degli import. Un hello-world sopra un design importato
DEVE fallire il gate; un design effettivamente montato deve passarlo; un
progetto senza staging significativo e' N/A.
"""
import pytest

from brain.agents import criteria_runner as cr


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


@pytest.mark.asyncio
async def test_hello_world_placeholder_fails_gate():
    files = {"src/main.tsx": 'import React from "react";\nrender(<div>Hello World!</div>);'}
    ctx = {"session_id": "s1", "timeout_s": 10,
           "tool_runner": _make_runner(files, {"figma_export": _STAGING, "src": ["src/main.tsx"]})}
    ok, ev = await cr.run_criterion(
        {"type": "no_orphan_imported", "spec": {}, "expected": {"mounted": True}}, ctx)
    assert ok is False
    assert ev["mounted"] is False
    assert ev["ratio"] == 0.0


@pytest.mark.asyncio
async def test_mounted_design_passes_gate():
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
    ctx = {"session_id": "s1", "timeout_s": 10,
           "tool_runner": _make_runner(files, {"figma_export": _STAGING, "src": list(files.keys())})}
    ok, ev = await cr.run_criterion(
        {"type": "no_orphan_imported", "spec": {}, "expected": {"mounted": True}}, ctx)
    assert ok is True
    assert ev["mounted"] is True
    assert ev["ratio"] >= 0.4


@pytest.mark.asyncio
async def test_no_staging_is_not_applicable():
    files = {"src/main.tsx": "x"}
    ctx = {"session_id": "s1", "timeout_s": 10,
           "tool_runner": _make_runner(files, {"figma_export": [], "src": ["src/main.tsx"]})}
    ok, ev = await cr.run_criterion(
        {"type": "no_orphan_imported", "spec": {}, "expected": {"mounted": True}}, ctx)
    assert ok is True
    assert ev.get("skipped") is not None
