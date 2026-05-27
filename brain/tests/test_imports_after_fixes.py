"""Smoke import dei 3 file modificati per i fix A/B/C (audit 27/05/2026)."""
from brain.agents.criteria_runner import _check_file_exists  # noqa: F401
from brain.agents.verifier_node import verifier_node  # noqa: F401
from brain.router.service import SemanticRouter  # noqa: F401


def main() -> int:
    # Verifica che SemanticRouter contenga i nuovi pattern code_read
    router = SemanticRouter()
    result = router._classify_by_keywords("quante variabili ci sono nel progetto")
    print(f"classify('quante variabili...') -> intent={result['intent']} confidence={result.get('confidence')}")
    assert result["intent"] == "code_read", f"Atteso code_read, ricevuto {result['intent']}"

    result2 = router._classify_by_keywords("quanti file di test sono presenti")
    print(f"classify('quanti file di test...') -> intent={result2['intent']}")
    assert result2["intent"] == "code_read", f"Atteso code_read, ricevuto {result2['intent']}"

    # Verifica che "genera analisi tecnica" cada ancora su docs
    result3 = router._classify_by_keywords("genera l'analisi tecnica del progetto")
    print(f"classify('genera analisi...') -> intent={result3['intent']}")
    assert result3["intent"] == "docs", f"Atteso docs, ricevuto {result3['intent']}"

    print("OK fix A: classifier code_read pattern works")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
