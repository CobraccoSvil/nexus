"""Test del punto unico should_disable_thinking (mig 0390, regola L).

Root cause coperta: i modelli dual-mode (deepseek-v4-pro/-flash, policy
'disable_for_tools') girano in thinking mode di default; nelle chiamate
TESTUALI dei task interni (purpose: chat title, doc gen, summary, classifier)
il budget di output finiva in reasoning_content e il content tornava vuoto
(incidenti "deepseek non scrive" / hollow_completion). Il fix spegne il
thinking anche SENZA tool quando la chiamata e' marcata internal_task, con
gate dal setting DB providers.thinking_disable_internal_text.

I test sono puri (nessun DB, nessuna rete): il flag DB viene monkeypatchato
sul modulo adapter_base; le capability sono sintetiche.
"""
from __future__ import annotations

import pytest

from brain.providers import adapter_base
from brain.providers.adapter_base import should_disable_thinking
from brain.tests._capability_factory import make_capability


def _cap(**overrides):
    """Dual-mode V4 di default (il caso degli incidenti); override per gli altri."""
    base = dict(
        provider="deepseek",
        model="deepseek-v4-flash",
        thinking=True,
        agentic_thinking_policy="disable_for_tools",
    )
    base.update(overrides)
    return make_capability(**base)


@pytest.fixture()
def _flag_on(monkeypatch):
    """Setting DB ON senza toccare il DB (bypassa la cache TTL del modulo)."""
    monkeypatch.setattr(adapter_base, "_internal_text_thinking_disabled", lambda: True)


@pytest.fixture()
def _flag_off(monkeypatch):
    monkeypatch.setattr(adapter_base, "_internal_text_thinking_disabled", lambda: False)


# ── Ramo ADR 0025 (con tool): comportamento invariato ────────────────────────

def test_tools_dual_mode_disables_thinking(_flag_off):
    """Con tool il thinking si spegne SEMPRE per i dual-mode, anche a flag off
    (il flag governa solo il ramo testuale interno)."""
    assert should_disable_thinking(_cap(), has_tools=True) is True


def test_tools_native_policy_keeps_thinking(_flag_on):
    """Policy 'native' (es. reasoner puri): mai spento, con o senza tool."""
    cap = _cap(agentic_thinking_policy="native")
    assert should_disable_thinking(cap, has_tools=True) is False
    assert should_disable_thinking(cap, has_tools=False, internal_task=True) is False


def test_capability_missing_is_noop(_flag_on):
    """Capability assente: degrado safe, nessun cambio di comportamento."""
    assert should_disable_thinking(None, has_tools=True) is False
    assert should_disable_thinking(None, has_tools=False, internal_task=True) is False


# ── Ramo mig 0390 (testuale interno) ─────────────────────────────────────────

def test_internal_text_dual_mode_disables_thinking(_flag_on):
    """Il caso degli incidenti: task interno testuale su v4 -> thinking off."""
    assert should_disable_thinking(_cap(), has_tools=False, internal_task=True) is True


def test_user_chat_text_keeps_thinking(_flag_on):
    """Chat utente (non marcata internal_task) senza tool: thinking intatto."""
    assert should_disable_thinking(_cap(), has_tools=False, internal_task=False) is False


def test_internal_text_respects_db_flag_off(_flag_off):
    """Flag DB off: il ramo testuale interno torna al comportamento storico."""
    assert should_disable_thinking(_cap(), has_tools=False, internal_task=True) is False


def test_internal_text_policy_none_is_noop(_flag_on):
    """Modelli senza policy dual-mode: il ramo interno non li tocca."""
    cap = _cap(thinking=False, agentic_thinking_policy="none")
    assert should_disable_thinking(cap, has_tools=False, internal_task=True) is False


# ── Wiring adapter DeepSeek: extra_body presente nella chiamata testuale ─────

class _FakeUsage:
    prompt_tokens = 10
    completion_tokens = 5
    total_tokens = 15


class _FakeMessage:
    content = "ok"
    reasoning_content = ""
    tool_calls = None


class _FakeChoice:
    message = _FakeMessage()
    finish_reason = "stop"


class _FakeResponse:
    choices = [_FakeChoice()]
    usage = _FakeUsage()


class _CapturingClient:
    """Client OpenAI-compatible finto che cattura i kwargs della create()."""

    def __init__(self) -> None:
        self.captured: dict = {}

        outer = self

        class _Completions:
            async def create(self, **kwargs):
                outer.captured = kwargs
                return _FakeResponse()

        class _Chat:
            completions = _Completions()

        self.chat = _Chat()


@pytest.mark.asyncio
async def test_deepseek_generate_internal_task_sends_extra_body(monkeypatch):
    """generate() testuale con internal_task=True invia extra_body thinking
    disabled (il probe API 2026-06-10 ha verificato che e' supportato anche
    senza tool)."""
    from brain.providers.deepseek_provider import DeepSeekProvider
    from brain.providers import capability_loader

    prov = DeepSeekProvider.__new__(DeepSeekProvider)
    # Bypass del mixin ApiKeyClientMixin: niente DB nei test.
    prov._api_key_provider = lambda: "test-key"
    prov._cached_key = ""
    prov._client = None
    client = _CapturingClient()
    monkeypatch.setattr(DeepSeekProvider, "_get_client", lambda self: client)
    monkeypatch.setattr(
        capability_loader, "load_capability", lambda p, m: _cap()
    )
    monkeypatch.setattr(adapter_base, "_internal_text_thinking_disabled", lambda: True)

    result = await prov.generate("deepseek-v4-flash", "titolo?", internal_task=True)
    assert result.content == "ok"
    assert client.captured.get("extra_body") == {"thinking": {"type": "disabled"}}


@pytest.mark.asyncio
async def test_deepseek_generate_user_text_no_extra_body(monkeypatch):
    """generate() senza internal_task: nessun extra_body (chat utente intatta)."""
    from brain.providers.deepseek_provider import DeepSeekProvider
    from brain.providers import capability_loader

    prov = DeepSeekProvider.__new__(DeepSeekProvider)
    # Bypass del mixin ApiKeyClientMixin: niente DB nei test.
    prov._api_key_provider = lambda: "test-key"
    prov._cached_key = ""
    prov._client = None
    client = _CapturingClient()
    monkeypatch.setattr(DeepSeekProvider, "_get_client", lambda self: client)
    monkeypatch.setattr(
        capability_loader, "load_capability", lambda p, m: _cap()
    )
    monkeypatch.setattr(adapter_base, "_internal_text_thinking_disabled", lambda: True)

    result = await prov.generate("deepseek-v4-flash", "ciao", internal_task=False)
    assert result.content == "ok"
    assert "extra_body" not in client.captured
