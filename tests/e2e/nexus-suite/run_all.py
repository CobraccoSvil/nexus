#!/usr/bin/env python3
"""Runner top-level che esegue tutti gli scenari E2E in sequenza.

Uso (richiede servizi up):
  python tests/e2e/nexus-suite/run_all.py

Output:
  - exit 0 se tutti i test passati o skippati
  - exit 1 se almeno uno fallisce
"""
import subprocess
import sys
from pathlib import Path


def main():
    here = Path(__file__).parent
    # Invoca pytest con verbose + tb breve
    cmd = [
        sys.executable, "-m", "pytest",
        str(here),
        "-v",
        "--tb=short",
        "--no-header",
        # Non fermarsi al primo errore; vogliamo capire quanti scenari rompono.
        "-x" if "--fail-fast" in sys.argv else "--continue-on-collection-errors",
    ]
    print(f"[e2e-suite] {' '.join(cmd)}")
    return subprocess.call(cmd)


if __name__ == "__main__":
    sys.exit(main())
