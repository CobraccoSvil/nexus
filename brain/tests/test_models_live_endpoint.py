"""Smoke test endpoint /providers/google/models/live (brain).

Verifica:
- Risposta HTTP 200
- Schema { provider: "google", models: [...] }
- Models contiene almeno un 'gemini-' (Vertex o Gemini direct)

Esegui solo se brain e' up: serve nexus-neural-wsl.service active.
"""
import sys
import urllib.request
import json


def main() -> int:
    url = "http://localhost:8001/providers/google/models/live"
    try:
        with urllib.request.urlopen(url, timeout=30) as r:
            assert r.status == 200, f"status {r.status}"
            data = json.loads(r.read().decode())
    except Exception as exc:
        print(f"FAIL: {exc}")
        return 1

    assert data.get("provider") == "google", f"provider sbagliato: {data}"
    models = data.get("models", [])
    assert isinstance(models, list), "models deve essere lista"
    assert len(models) > 0, "lista vuota"
    gemini_models = [m for m in models if m.startswith("gemini-")]
    assert len(gemini_models) > 0, f"nessun gemini nei modelli: {models[:5]}"

    print(f"OK: ricevuti {len(models)} modelli, di cui {len(gemini_models)} gemini")
    print(f"   Primi 5: {models[:5]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
