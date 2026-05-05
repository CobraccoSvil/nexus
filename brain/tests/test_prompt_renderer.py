"""Test del renderer dei placeholder nei prompt agente.

Garantisce le invarianti chiave:
- Mai lasciare `{{...}}` letterale nel prompt finale.
- Resolver noti producono i valori attesi.
- Resolver mancante fallisce in stringa vuota (no crash, no leak).
"""
from __future__ import annotations

import re

from brain.agents import prompt_renderer

PLACEHOLDER_RE = re.compile(r"\{\{[^}]+\}\}")


def test_no_residual_placeholder_in_output():
    template = "Linguaggio: {{lang_hint}}; Tipo: {{type_hint}}; Repo: {{repo_summary}}."
    out = prompt_renderer.render(template, {}, intent="bug_fix")
    assert PLACEHOLDER_RE.search(out) is None, f"placeholder residuo in: {out}"


def test_lang_hint_from_state():
    template = "Sei un coder esperto{{lang_hint}}."
    out = prompt_renderer.render(template, {"repo_lang": "Rust"}, intent="code_generation")
    assert ", linguaggio Rust" in out
    assert "{{" not in out


def test_lang_hint_empty_when_missing():
    template = "Sei un coder esperto{{lang_hint}}."
    out = prompt_renderer.render(template, {}, intent="code_generation")
    assert out == "Sei un coder esperto."


def test_type_hint_from_intent():
    template = "Output atteso: {{type_hint}}."
    out_bug = prompt_renderer.render(template, {}, intent="bug_fix")
    assert "fix mirata" in out_bug
    out_test = prompt_renderer.render(template, {}, intent="test_generation")
    assert "test unitari" in out_test


def test_type_hint_default_for_unknown_intent():
    template = "{{type_hint}}"
    out = prompt_renderer.render(template, {}, intent="intent_inesistente")
    assert out == "task generico"
    assert "{{" not in out


def test_repo_summary_default_when_missing():
    template = "Contesto: {{repo_summary}}"
    out = prompt_renderer.render(template, {}, intent="chat")
    assert "metadati non disponibili" in out


def test_repo_summary_from_state():
    template = "{{repo_summary}}"
    out = prompt_renderer.render(template, {"repo_summary": "Monorepo TS+Rust"}, intent="chat")
    assert out == "Monorepo TS+Rust"


def test_unknown_placeholder_replaced_with_empty():
    template = "prefix{{placeholder_che_non_esiste}}suffix"
    out = prompt_renderer.render(template, {}, intent="chat")
    assert out == "prefixsuffix"


def test_empty_template_returns_empty():
    assert prompt_renderer.render("", {}, intent="chat") == ""


def test_multiple_occurrences():
    template = "{{lang_hint}} e ancora {{lang_hint}}"
    out = prompt_renderer.render(template, {"lang_hint": None, "repo_lang": "Python"}, intent="chat")
    # entrambe le occorrenze sostituite
    assert out.count(", linguaggio Python") == 2


def test_register_custom_resolver():
    prompt_renderer.register_resolver("custom_test", lambda _s, _i: "OK")
    template = "valore: {{custom_test}}"
    out = prompt_renderer.render(template, {}, intent="chat")
    assert out == "valore: OK"
