"""Test del punto unico DB connection (brain/utils/db_pool.py)."""
import os

import pytest

from brain.utils.db_pool import DbUrlUnavailable, get_db_url


def test_get_db_url_legge_database_url(monkeypatch):
    monkeypatch.delenv("POSTGRES_URL", raising=False)
    monkeypatch.setenv("DATABASE_URL", "postgres://test")
    assert get_db_url() == "postgres://test"


def test_get_db_url_legge_postgres_url_se_database_assente(monkeypatch):
    monkeypatch.delenv("DATABASE_URL", raising=False)
    monkeypatch.setenv("POSTGRES_URL", "postgres://altro")
    assert get_db_url() == "postgres://altro"


def test_get_db_url_solleva_se_entrambe_assenti(monkeypatch):
    monkeypatch.delenv("DATABASE_URL", raising=False)
    monkeypatch.delenv("POSTGRES_URL", raising=False)
    with pytest.raises(DbUrlUnavailable):
        get_db_url()


def test_niente_default_hardcoded(monkeypatch):
    """Regola G: zero fallback nascosti. Vietato qualsiasi default."""
    monkeypatch.delenv("DATABASE_URL", raising=False)
    monkeypatch.delenv("POSTGRES_URL", raising=False)
    with pytest.raises(DbUrlUnavailable):
        get_db_url()
