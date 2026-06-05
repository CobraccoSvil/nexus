"""Cache TTL generica (punto unico Python, regola L / ADR 0026).

Paritetica al lato Rust ``nexus_cache::TtlCache``. Prima questo pattern era
duplicato in 4+ punti del brain (``providers/catalog_loader.py``,
``grpc_server/neural_service.py::_default_model_for_provider``,
``router/agentic_classifier.py::_load_classifier_config``,
``agents/nodes/helpers.py``), ognuno con il suo dict ``_CACHE`` / ``_CACHE_TS``
e la sua eccezione custom. Ora la logica vive qui una volta sola.
"""
from __future__ import annotations

import threading
import time
from typing import Dict, Generic, Hashable, Optional, Tuple, TypeVar

K = TypeVar("K", bound=Hashable)
V = TypeVar("V")


class TtlCache(Generic[K, V]):
    """Cache chiave-valore con TTL uniforme per tutte le entry.

    Thread-safe: tutte le operazioni sono protette da un ``threading.Lock``
    (psycopg2 e' sincrono, il brain serve richieste concorrenti via uvicorn).
    """

    __slots__ = ("_store", "_ttl_seconds", "_lock")

    def __init__(self, ttl_seconds: float) -> None:
        if ttl_seconds <= 0:
            raise ValueError("ttl_seconds deve essere > 0")
        self._store: Dict[K, Tuple[V, float]] = {}
        self._ttl_seconds: float = float(ttl_seconds)
        self._lock = threading.Lock()

    def get(self, key: K) -> Optional[V]:
        """Ritorna il valore se presente e non scaduto, altrimenti ``None``."""
        now = time.monotonic()
        with self._lock:
            hit = self._store.get(key)
            if hit is None:
                return None
            value, ts = hit
            if (now - ts) < self._ttl_seconds:
                return value
            return None

    def set(self, key: K, value: V) -> None:
        """Inserisce/aggiorna una entry, marcandola con l'istante corrente."""
        with self._lock:
            self._store[key] = (value, time.monotonic())

    def invalidate(self, key: K) -> None:
        """Rimuove esplicitamente una entry (per invalidazione su update)."""
        with self._lock:
            self._store.pop(key, None)

    def clear(self) -> None:
        """Svuota completamente la cache."""
        with self._lock:
            self._store.clear()

    def __len__(self) -> int:
        with self._lock:
            return len(self._store)
