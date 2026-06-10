"""Smoke test del billing cooldown locale (registry.py).

Verifica SOLO la cache in-memory locale del cooldown billing. Dal consolidamento
"cooldown writer unico" (ADR 0030/0020) _mark_billing_cooldown NON scrive piu' il
DB: notifica mcp-core (Rust = writer unico di nexus_provider_health). Qui il
bridge e' reso no-op per isolare il test dal cross-process (regola F: idempotente,
nessun side-effect su mcp-core/DB) e si usa un provider FITTIZIO (mai un provider
reale, per non metterlo davvero in cooldown).
"""

import brain.providers.cooldown_bridge as _cb

# Isola dal cross-process: il bridge non deve fare il POST reale durante il test.
_cb.notify_provider_error_sync = lambda *a, **k: None  # type: ignore[assignment]

from brain.providers.registry import (  # noqa: E402
    _is_in_billing_cooldown,
    _mark_billing_cooldown,
    _clear_billing_cooldown,
    get_billing_cooldown_snapshot,
    _billing_cooldown_ttl_s,
)

_P = "__test_billing_provider__"


def main() -> int:
    # 1. Provider fittizio non in cooldown all'inizio.
    assert not _is_in_billing_cooldown(_P)

    # 2. Marca in cooldown: solo cache locale (bridge no-op).
    _mark_billing_cooldown(_P)
    assert _is_in_billing_cooldown(_P)

    # 3. Snapshot locale lo mostra con un remaining sensato.
    snap = get_billing_cooldown_snapshot()
    assert _P in snap
    assert 0 < snap[_P] <= _billing_cooldown_ttl_s()

    # 4. Case-insensitive.
    assert _is_in_billing_cooldown(_P.upper())

    # 5. Clear locale lo rimuove dalla cache (la riabilitazione persistente
    #    cross-process e' governata da mcp-core, non testata qui).
    _clear_billing_cooldown(_P)
    assert _P not in get_billing_cooldown_snapshot()

    print(f"OK cooldown_ttl={_billing_cooldown_ttl_s()}s (cache locale, bridge isolato)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
