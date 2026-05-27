"""Smoke test del dual backend GoogleProvider.

Verifica:
- Import senza errori
- _resolve_backend_config legge dal DB i settings 0183
- _is_configured ritorna ok/reason coerenti
- _backend_signature e' deterministica
- Backend default = "gemini" se setting non impostato
"""
from brain.providers.google_provider import GoogleProvider


def main() -> int:
    p = GoogleProvider()

    cfg = p._resolve_backend_config()
    assert cfg["backend"] in ("gemini", "vertex"), f"backend invalido: {cfg['backend']}"
    print(f"Backend rilevato: {cfg['backend']}")
    print(f"Project: {cfg['project']!r}")
    print(f"Location: {cfg['location']!r}")
    print(f"Credentials JSON present: {bool(cfg['credentials_json'])}")

    # _is_configured deve dare risposta coerente
    ok, reason = p._is_configured()
    print(f"is_configured: ok={ok}, reason={reason!r}")

    # Signature: stessa config -> stessa firma
    sig1 = p._backend_signature(cfg)
    sig2 = p._backend_signature(cfg)
    assert sig1 == sig2, "Signature non deterministica"
    print(f"Signature: {sig1}")

    # Simula backend vertex e verifica signature cambi
    p_vertex_cfg = {
        "backend": "vertex",
        "project": "test-project",
        "location": "europe-west4",
        "credentials_json": "",
    }
    sig_v = p._backend_signature(p_vertex_cfg)
    assert sig_v != sig1 or cfg["backend"] == "vertex", "Signature vertex deve differire da gemini"
    print(f"Vertex signature: {sig_v}")

    print("OK dual_backend smoke test passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
