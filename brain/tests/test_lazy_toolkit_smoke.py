"""Smoke test rapido del lazy toolkit.

Eseguito tramite `python3 -m brain.tests.test_lazy_toolkit_smoke` per
verificare che la modifica a `_INTENT_TOOL_SUBSET` non rompe l'import
e che il toolkit minimo e' definito correttamente.
"""
from brain.agents.profile_loader import _INTENT_TOOL_SUBSET, _LAZY_MINIMAL_TOOLKIT


def main() -> int:
    assert isinstance(_LAZY_MINIMAL_TOOLKIT, list), "toolkit deve essere lista"
    assert len(_LAZY_MINIMAL_TOOLKIT) >= 5, "toolkit troppo ristretto"
    assert "nexus_mcp_tool_search" in _LAZY_MINIMAL_TOOLKIT
    assert "nexus_mcp_tool_call" in _LAZY_MINIMAL_TOOLKIT

    # Intent generici ora usano lazy toolkit
    for intent in ("code", "code_edit", "implement", "fix", "debug"):
        assert _INTENT_TOOL_SUBSET[intent] is _LAZY_MINIMAL_TOOLKIT, (
            f"intent {intent} non usa lazy toolkit"
        )

    # Intent docs ha solo nexus_doc_generate (fix precedente conservato)
    assert _INTENT_TOOL_SUBSET["docs"] == ["nexus_doc_generate"]
    assert _INTENT_TOOL_SUBSET["doc_generate"] == ["nexus_doc_generate"]

    print(f"OK toolkit_size={len(_LAZY_MINIMAL_TOOLKIT)} intents_lazy=5 intents_docs=2")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
