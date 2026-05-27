"""Smoke test del billing cooldown locale (registry.py)."""
import time

from brain.providers.registry import (
    _is_in_billing_cooldown,
    _mark_billing_cooldown,
    get_billing_cooldown_snapshot,
    _PROVIDER_COOLDOWN_TTL_S,
)


def main() -> int:
    # 1. Provider non in cooldown all'inizio
    assert not _is_in_billing_cooldown("anthropic")
    assert get_billing_cooldown_snapshot() == {}

    # 2. Marca anthropic in cooldown
    _mark_billing_cooldown("anthropic")
    assert _is_in_billing_cooldown("anthropic")

    # 3. Snapshot lo mostra
    snap = get_billing_cooldown_snapshot()
    assert "anthropic" in snap
    assert 0 < snap["anthropic"] <= _PROVIDER_COOLDOWN_TTL_S

    # 4. Case-insensitive
    assert _is_in_billing_cooldown("ANTHROPIC")
    assert _is_in_billing_cooldown("Anthropic")

    # 5. Altri provider non interessati
    assert not _is_in_billing_cooldown("openai")

    print(f"OK cooldown_ttl={_PROVIDER_COOLDOWN_TTL_S}s anthropic_remaining={snap['anthropic']}s")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
