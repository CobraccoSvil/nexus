"""Test che "cancella il file X" classifichi come file_ops (audit 28/05/2026)."""
from brain.agents.profile_loader import AgentProfile
from brain.router.service import SemanticRouter


def main() -> int:
    r = SemanticRouter()
    cases = [
        ("cancella il file variables.txt", "file_ops"),
        ("elimina il file foo.txt", "file_ops"),
        ("rimuovi il file pippo.py", "file_ops"),
        ("delete the file test.js", "file_ops"),
        ("rinomina il file from.txt", "file_ops"),
        ("cancella questo file", "file_ops"),
        # Casi che NON devono cadere su file_ops
        ("quante variabili ci sono", "code_read"),
        ("genera l'analisi tecnica", "docs"),
    ]
    fails = []
    for msg, expected in cases:
        result = r._classify_by_keywords(msg)
        actual = result["intent"]
        if actual != expected:
            fails.append((msg, expected, actual))
            print(f"FAIL: '{msg}' atteso={expected} ottenuto={actual}")
        else:
            print(f"OK: '{msg}' -> {actual}")

    # Verifica _ALWAYS_ON_TOOLS contiene delete_file
    assert "delete_file" in AgentProfile._ALWAYS_ON_TOOLS, "delete_file deve essere always-on"
    assert "rename_file" in AgentProfile._ALWAYS_ON_TOOLS, "rename_file deve essere always-on"
    print(f"OK always_on_tools: {sorted(AgentProfile._ALWAYS_ON_TOOLS)}")

    if fails:
        print(f"\n{len(fails)} FAIL su {len(cases)} casi")
        return 1
    print(f"\nOK {len(cases)}/{len(cases)} classifier file_ops casi")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
