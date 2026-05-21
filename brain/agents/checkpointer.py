"""Setup del checkpointer PostgreSQL per LangGraph."""
from __future__ import annotations

import logging
import os
from pathlib import Path

from .postgres_checkpointer import PostgresCheckpointer

logger = logging.getLogger(__name__)


def get_postgres_connection_string() -> str:
    """Restituisce la connection string PostgreSQL da variabili di ambiente.

    Fallback a localhost con credenziali default se non configurate.
    """
    return os.environ.get(
        "DATABASE_URL",
        "postgresql://nexus:nexus@localhost:5433/nexus",
    )


def create_checkpointer() -> PostgresCheckpointer:
    """Crea e ritorna un PostgresCheckpointer per LangGraph 0.2+.

    Usa PostgreSQL con asyncpg per evitare il deadlock causato da AsyncSqliteSaver
    quando il grafo viene compilato in contesto sincrono (FastAPI startup) ma
    poi invocato da ainvoke() che crea un event loop asincrono interno.

    Returns:
        PostgresCheckpointer initializzato con la connection string PostgreSQL.

    Raises:
        ValueError: Se DATABASE_URL non è impostato e localhost:5432 non è raggiungibile.
    """
    connection_string = get_postgres_connection_string()
    logger.info(f"Creazione checkpointer PostgreSQL con connessione: {connection_string.split('@')[1] if '@' in connection_string else 'localhost'}")

    return PostgresCheckpointer(connection_string=connection_string)


def get_memory_db_path() -> str:
    """Deprecato: il learning storage ora usa PostgreSQL.

    Mantenuto per retrocompatibilita. Il path non viene piu utilizzato
    dal sistema — i dati sono in brain_learning_interactions su PostgreSQL.
    """
    brain_root = Path(__file__).parent.parent
    nexus_memory = brain_root / "nexus_memory"
    nexus_memory.mkdir(parents=True, exist_ok=True)
    return str(nexus_memory / "learning.db")
