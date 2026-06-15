"""Test del flusso "ultra" (MVP): decomposizione parallela (B) + panel di
verifica avversariale (A).

B: dag_scheduler.should_parallelize decide se il DAG parallelo scatta. Il fix
   chiave e' parallelizzare anche i todo INDIPENDENTI (senza depends_on), prima
   bloccati dalla sola guardia _has_deps.
A: verifier_node._run_verify_panel lancia K verificatori con lenti diverse e
   applica il consenso (>= verify_panel_consensus segnalazioni -> non passa).
"""
from __future__ import annotations

import asyncio

import pytest

from brain.agents import verifier_node
from brain.agents.dag_scheduler import should_parallelize


def _t(tid: str, status: str = "pending", deps: list | None = None) -> dict:
    return {"id": tid, "status": status, "depends_on": deps or []}


# ── B: decomposizione parallela (should_parallelize) ─────────────────────────

def test_parallelizza_todo_indipendenti_sopra_soglia():
    """3 todo indipendenti, min_ready=2 -> parallelizza (caso prima bloccato)."""
    todos = [_t("a"), _t("b"), _t("c")]
    assert should_parallelize(todos, todos, {"dag_parallel_min_ready": 2}) is True


def test_non_parallelizza_singolo_todo():
    """1 solo todo ready, min_ready=2 -> esecuzione normale, niente DAG."""
    todos = [_t("a")]
    assert should_parallelize(todos, todos, {"dag_parallel_min_ready": 2}) is False


def test_parallelizza_con_dipendenze_anche_un_solo_ready():
    """Comportamento storico: con depends_on espliciti parallelizza il ready
    layer anche se ha un solo elemento."""
    todos = [_t("a", status="completed"), _t("b", deps=["a"])]
    ready = [_t("b", deps=["a"])]
    assert should_parallelize(ready, todos, {"dag_parallel_min_ready": 2}) is True


def test_min_ready_1_disabilita_parallelismo_indipendenti():
    """min_ready<=1 = comportamento storico: todo indipendenti NON parallelizzano."""
    todos = [_t("a"), _t("b"), _t("c")]
    assert should_parallelize(todos, todos, {"dag_parallel_min_ready": 1}) is False


def test_ready_vuoto_non_parallelizza():
    todos = [_t("a", status="in_progress")]
    assert should_parallelize([], todos, {"dag_parallel_min_ready": 2}) is False


def test_default_min_ready_e_due():
    """cfg senza la chiave -> default 2 (ultra di default)."""
    todos = [_t("a"), _t("b")]
    assert should_parallelize(todos, todos, {}) is True


# ── A: panel di verifica avversariale (consenso) ─────────────────────────────

class _FakeDecision:
    provider = "fakeprov"
    model = "fakemodel"


class _FakeRouting:
    def purpose_model(self, purpose):  # noqa: ARG002
        return _FakeDecision()


class _FakeResult:
    def __init__(self, content: str):
        self.content = content


class _FakeProviders:
    """generate_completion ritorna una risposta diversa per lente, riconosciuta
    dal nome della lente presente nel prompt."""

    def __init__(self, per_lens: dict[str, str]):
        self._per_lens = per_lens

    def generate_completion(self, provider, model, prompt):  # noqa: ARG002
        low = prompt.lower()
        for kw, content in self._per_lens.items():
            if kw in low:
                return _FakeResult(content)
        return _FakeResult("OK")


@pytest.fixture
def _verifier_globals():
    orig = (verifier_node._providers, verifier_node._routing_client, verifier_node._tool_runner)
    yield
    (verifier_node._providers, verifier_node._routing_client, verifier_node._tool_runner) = orig


@pytest.fixture(autouse=True)
def _no_clamp(monkeypatch):
    """Neutralizza il clamp del prompt (dipende dal registry modelli) nei test."""
    monkeypatch.setattr("brain.agents.context_brake.clamp_single_prompt", lambda p, m: p)


def _cfg() -> dict:
    return {
        "verify_panel_size": 3,
        "verify_panel_consensus": 2,
        "verify_panel_lenses": ["correttezza", "sicurezza", "casi limite"],
    }


def test_panel_consenso_raggiunto_blocca(_verifier_globals):
    """2 lenti su 3 segnalano un problema (consensus=2) -> todo non passa."""
    verifier_node._providers = _FakeProviders({
        "correttezza": "PROBLEMA: output errato sul caso base",
        "sicurezza": "PROBLEMA: token esposto nei log",
        "casi limite": "OK",
    })
    verifier_node._routing_client = _FakeRouting()
    verifier_node._tool_runner = None  # past_failures = ""
    ok, finding = asyncio.run(
        verifier_node._run_verify_panel({}, {"content": "task X"}, [], {}, _cfg())
    )
    assert ok is False
    assert finding  # findings aggregati non vuoti


def test_panel_consenso_non_raggiunto_passa(_verifier_globals):
    """1 sola lente segnala -> consensus=2 non raggiunto -> todo passa."""
    verifier_node._providers = _FakeProviders({
        "correttezza": "PROBLEMA: dubbio minore",
        "sicurezza": "OK",
        "casi limite": "OK",
    })
    verifier_node._routing_client = _FakeRouting()
    verifier_node._tool_runner = None
    ok, finding = asyncio.run(
        verifier_node._run_verify_panel({}, {"content": "task X"}, [], {}, _cfg())
    )
    assert ok is True
    assert finding == ""


def test_panel_tutte_ok_passa(_verifier_globals):
    """Nessuna lente segnala -> passa."""
    verifier_node._providers = _FakeProviders({})  # default OK
    verifier_node._routing_client = _FakeRouting()
    verifier_node._tool_runner = None
    ok, finding = asyncio.run(
        verifier_node._run_verify_panel({}, {"content": "task X"}, [], {}, _cfg())
    )
    assert ok is True


def test_panel_size_limita_il_numero_di_lenti(_verifier_globals):
    """verify_panel_size=1 -> una sola lente; con consensus=2 non basta mai."""
    verifier_node._providers = _FakeProviders({
        "correttezza": "PROBLEMA: x",
        "sicurezza": "PROBLEMA: y",
        "casi limite": "PROBLEMA: z",
    })
    verifier_node._routing_client = _FakeRouting()
    verifier_node._tool_runner = None
    cfg = _cfg()
    cfg["verify_panel_size"] = 1  # solo la prima lente
    cfg["verify_panel_consensus"] = 2
    ok, _ = asyncio.run(
        verifier_node._run_verify_panel({}, {"content": "task X"}, [], {}, cfg)
    )
    # Una sola lente segnala, consensus=2 -> passa comunque.
    assert ok is True
