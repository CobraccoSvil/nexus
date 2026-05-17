"""Pytest config per la suite E2E nexus."""
import os
import sys
from pathlib import Path

# Aggiungi _helpers al path per import diretto.
HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))

# Carica .env se presente
env_file = HERE / ".env"
if env_file.exists():
    for line in env_file.read_text().splitlines():
        if line.strip() and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            os.environ.setdefault(k.strip(), v.strip())
