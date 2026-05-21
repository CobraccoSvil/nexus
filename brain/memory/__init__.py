"""Modulo di apprendimento persistente per Nexus (PostgreSQL-backed)."""
from __future__ import annotations

from .storage import PostgresLearningStorage

# Alias retrocompatibile
LocalLearningStorage = PostgresLearningStorage

__all__ = ["PostgresLearningStorage", "LocalLearningStorage"]
