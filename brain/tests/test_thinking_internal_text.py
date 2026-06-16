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


# NB: i due test di WIRING adapter DeepSeek (che chiamavano DeepSeekProvider.generate
# per verificare l'invio di extra_body thinking=disabled) sono stati RIMOSSI con il
# consolidamento del trasporto (regola L / ADR 0026): gli adapter SDK non eseguono
# piu' chiamate LLM. Lo spegnimento del thinking nelle chiamate testuali interne e'
# ora applicato dal gateway Rust (crates/nexus-gateway/src/providers/deepseek.rs,
# parita' funzionale con should_disable_thinking). Restano i test PURI del punto
# unico should_disable_thinking sopra, che ne coprono la logica decisionale.
