"""Test del punto unico TtlCache (brain/utils/ttl_cache.py)."""
import time

import pytest

from brain.utils.ttl_cache import TtlCache


def test_hit_valido_ritorna_il_valore():
    c: TtlCache[str, int] = TtlCache(ttl_seconds=60.0)
    c.set("k", 42)
    assert c.get("k") == 42


def test_chiave_assente_ritorna_none():
    c: TtlCache[str, int] = TtlCache(ttl_seconds=60.0)
    assert c.get("missing") is None


def test_ttl_scaduto_ritorna_none():
    c: TtlCache[str, int] = TtlCache(ttl_seconds=0.01)
    c.set("k", 1)
    time.sleep(0.05)
    assert c.get("k") is None


def test_invalidate_rimuove_la_entry():
    c: TtlCache[str, int] = TtlCache(ttl_seconds=60.0)
    c.set("k", 1)
    c.invalidate("k")
    assert c.get("k") is None


def test_clear_svuota():
    c: TtlCache[str, int] = TtlCache(ttl_seconds=60.0)
    c.set("a", 1)
    c.set("b", 2)
    c.clear()
    assert len(c) == 0


def test_ttl_zero_o_negativo_rifiutato():
    with pytest.raises(ValueError):
        TtlCache(ttl_seconds=0)
    with pytest.raises(ValueError):
        TtlCache(ttl_seconds=-1.0)
