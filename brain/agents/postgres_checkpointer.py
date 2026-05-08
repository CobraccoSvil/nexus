"""Checkpointer PostgreSQL asincrono per LangGraph 0.2+.

Implementa BaseCheckpointSaver di LangGraph usando asyncpg per evitare il deadlock
causato da AsyncSqliteSaver in contesto sincrono durante FastAPI startup.

La serializzazione passa per `langchain_core.load.dumps/loads`, che gestisce
correttamente i tipi LangChain (HumanMessage, AIMessage, ToolMessage, ecc.) — il
`json` standard solleva `TypeError: Object of type HumanMessage is not JSON serializable`.
"""
from __future__ import annotations

import json as json_module
import logging
from typing import Any, AsyncIterator, Optional

import asyncpg
from langchain_core.load import dumps as _lc_dumps, loads as _lc_loads
from langchain_core.runnables import RunnableConfig
from langgraph.checkpoint.base import (
    BaseCheckpointSaver,
    Checkpoint,
    CheckpointMetadata,
    CheckpointTuple,
    ChannelVersions,
)

logger = logging.getLogger(__name__)


def _safe_dumps(obj: Any) -> str:
    """Serializza con langchain_core (gestisce BaseMessage), fallback su json standard."""
    try:
        return _lc_dumps(obj)
    except Exception:  # pragma: no cover — solo rete di sicurezza
        return json_module.dumps(obj, default=str)


def _safe_loads(s: str | bytes) -> Any:
    """Deserializza con langchain_core, fallback su json standard se non è LC-format."""
    if isinstance(s, bytes):
        s = s.decode("utf-8")
    try:
        return _lc_loads(s)
    except Exception:
        return json_module.loads(s)


class PostgresCheckpointer(BaseCheckpointSaver):
    """Checkpointer PostgreSQL asincrono per LangGraph 0.2+."""

    def __init__(
        self,
        connection_string: str,
        pool: Optional[asyncpg.Pool] = None,
    ) -> None:
        """Inizializza il checkpointer PostgreSQL.

        Args:
            connection_string: URL di connessione PostgreSQL
                              (es. "postgresql://user:pass@localhost:5432/dbname")
            pool: Pool asyncpg esistente (opzionale). Se None, viene creato
                  durante l'inizializzazione asincrona.
        """
        super().__init__()
        self.connection_string = connection_string
        self.pool = pool
        self._initialized = False

    @property
    def config_specs(self) -> list[Any]:
        """Ritorna le specifiche di configurazione per il checkpointer."""
        return []

    async def _ensure_initialized(self) -> None:
        """Inizializza il pool e la tabella se non ancora fatto."""
        if self._initialized:
            return

        if self.pool is None:
            # asyncpg non supporta ?sslmode=disable nella URL — rimuoviamo il parametro
            # e passiamo ssl=False come kwarg separato se necessario.
            from urllib.parse import urlparse, urlencode, parse_qs, urlunparse
            parsed = urlparse(self.connection_string)
            qs = parse_qs(parsed.query)
            sslmode = qs.pop("sslmode", ["require"])[0]
            ssl_kwarg: dict = {"ssl": False} if sslmode in ("disable", "allow", "prefer") else {}
            clean_url = urlunparse(parsed._replace(
                # Normalizza lo scheme senza “doppio replace” (bug: "postgresql" -> "postgresqlql")
                scheme=("postgresql" if parsed.scheme == "postgres" else parsed.scheme),
                query=urlencode({k: v[0] for k, v in qs.items()}),
            ))
            self.pool = await asyncpg.create_pool(
                clean_url,
                min_size=2,
                max_size=10,
                command_timeout=30.0,
                **ssl_kwarg,
            )

        # Crea la tabella se non esiste
        async with self.pool.acquire() as conn:
            await conn.execute(
                """
                CREATE TABLE IF NOT EXISTS langgraph_checkpoints (
                    thread_id TEXT NOT NULL,
                    checkpoint_id TEXT NOT NULL,
                    checkpoint_data JSONB NOT NULL,
                    metadata JSONB NOT NULL DEFAULT '{}',
                    versions JSONB NOT NULL DEFAULT '{}',
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (thread_id, checkpoint_id)
                )
                """
            )
            # Indice per query rapide per thread_id
            await conn.execute(
                """
                CREATE INDEX IF NOT EXISTS idx_checkpoints_thread_id
                ON langgraph_checkpoints(thread_id, created_at DESC)
                """
            )

        self._initialized = True

    async def aput(
        self,
        config: RunnableConfig,
        checkpoint: Checkpoint,
        metadata: CheckpointMetadata,
        new_versions: ChannelVersions,
    ) -> RunnableConfig:
        """Salva un checkpoint in modo asincrono.

        Args:
            config: Configurazione del grafo (contiene thread_id, checkpoint_id)
            checkpoint: Dati del checkpoint da salvare
            metadata: Metadati del checkpoint
            new_versions: Versioni dei canali

        Returns:
            La stessa config passata in input
        """
        await self._ensure_initialized()

        configurable = config.get("configurable", {})
        thread_id = configurable.get("thread_id", "default")
        checkpoint_id = checkpoint.get("id")

        if not checkpoint_id:
            raise ValueError("checkpoint deve contenere un 'id'")

        async with self.pool.acquire() as conn:  # type: ignore[union-attr]
            await conn.execute(
                """
                INSERT INTO langgraph_checkpoints
                (thread_id, checkpoint_id, checkpoint_data, metadata, versions)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (thread_id, checkpoint_id)
                DO UPDATE SET
                  checkpoint_data = $3,
                  metadata = $4,
                  versions = $5
                """,
                thread_id,
                checkpoint_id,
                _safe_dumps(checkpoint),
                _safe_dumps(metadata),
                _safe_dumps(new_versions),
            )

        return config

    async def aget_tuple(
        self,
        config: RunnableConfig,
    ) -> Optional[CheckpointTuple]:
        """Recupera un CheckpointTuple in modo asincrono (richiesto da graph.astream()).

        Args:
            config: Configurazione del grafo (contiene thread_id e opzionalmente checkpoint_id)

        Returns:
            CheckpointTuple più recente o None se non trovato
        """
        await self._ensure_initialized()

        configurable = config.get("configurable", {})
        thread_id = configurable.get("thread_id", "default")
        checkpoint_id = configurable.get("checkpoint_id")

        async with self.pool.acquire() as conn:  # type: ignore[union-attr]
            if checkpoint_id:
                row = await conn.fetchrow(
                    """
                    SELECT checkpoint_id, checkpoint_data, metadata, versions
                    FROM langgraph_checkpoints
                    WHERE thread_id = $1 AND checkpoint_id = $2
                    LIMIT 1
                    """,
                    thread_id, checkpoint_id,
                )
            else:
                row = await conn.fetchrow(
                    """
                    SELECT checkpoint_id, checkpoint_data, metadata, versions
                    FROM langgraph_checkpoints
                    WHERE thread_id = $1
                    ORDER BY created_at DESC
                    LIMIT 1
                    """,
                    thread_id,
                )

            if not row:
                return None

            checkpoint_data = _safe_loads(row["checkpoint_data"])
            metadata = _safe_loads(row["metadata"])

            return CheckpointTuple(
                config={
                    **config,
                    "configurable": {
                        **configurable,
                        "checkpoint_id": row["checkpoint_id"],
                    },
                },
                checkpoint=checkpoint_data,
                metadata=metadata,
                parent_config=None,
            )

    async def aput_writes(
        self,
        config: RunnableConfig,
        writes: list[tuple[str, Any]],
        task_id: str,
        task_path: str = "",
    ) -> None:
        """Salva writes intermedie (richiesto da LangGraph 0.2+ durante astream).

        Implementazione no-op: le writes vengono gestite interamente da aput().
        """

    async def aget(
        self,
        config: RunnableConfig,
    ) -> Optional[Checkpoint]:
        """Recupera un checkpoint in modo asincrono.

        Args:
            config: Configurazione del grafo

        Returns:
            Il checkpoint più recente o None se non trovato
        """
        await self._ensure_initialized()

        configurable = config.get("configurable", {})
        thread_id = configurable.get("thread_id", "default")

        async with self.pool.acquire() as conn:  # type: ignore[union-attr]
            row = await conn.fetchrow(
                """
                SELECT checkpoint_data FROM langgraph_checkpoints
                WHERE thread_id = $1
                ORDER BY created_at DESC
                LIMIT 1
                """,
                thread_id,
            )

            if not row:
                return None

            return _safe_loads(row["checkpoint_data"])

    async def alist(
        self,
        config: RunnableConfig | None,
        *,
        filter: dict[str, Any] | None = None,
        before: RunnableConfig | None = None,
        limit: int | None = None,
    ) -> AsyncIterator[CheckpointTuple]:
        """Lista tutti i checkpoint per un thread in modo asincrono.

        Args:
            config: Configurazione del grafo
            filter: Filtri aggiuntivi (non supportati)
            before: Ritorna checkpoint prima di questo (non supportato)
            limit: Numero massimo di checkpoint da ritornare

        Yields:
            CheckpointTuple per ogni checkpoint trovato
        """
        await self._ensure_initialized()

        configurable = (config or {}).get("configurable", {})
        thread_id = configurable.get("thread_id", "default")

        limit_clause = f"LIMIT {limit}" if limit else ""

        async with self.pool.acquire() as conn:  # type: ignore[union-attr]
            rows = await conn.fetch(
                f"""
                SELECT checkpoint_id, checkpoint_data, metadata, versions
                FROM langgraph_checkpoints
                WHERE thread_id = $1
                ORDER BY created_at DESC
                {limit_clause}
                """,
                thread_id,
            )

        for row in rows:
            checkpoint_data = _safe_loads(row["checkpoint_data"])
            metadata = _safe_loads(row["metadata"])
            versions = json_module.loads(row["versions"])

            yield CheckpointTuple(
                config={
                    **(config or {}),
                    "configurable": {
                        **configurable,
                        "checkpoint_id": row["checkpoint_id"],
                    },
                },
                checkpoint=checkpoint_data,
                metadata=metadata,
                parent_config=None,
            )

    def put(
        self,
        config: RunnableConfig,
        checkpoint: Checkpoint,
        metadata: CheckpointMetadata,
        new_versions: ChannelVersions,
    ) -> RunnableConfig:
        """Metodo sincrono non supportato.

        Solleva NotImplementedError per forzare l'uso di ainvoke().
        """
        raise NotImplementedError(
            "PostgresCheckpointer supporta solo operazioni asincrone. "
            "Usa graph.ainvoke() invece di graph.invoke()"
        )

    def get(
        self,
        config: RunnableConfig,
    ) -> Optional[Checkpoint]:
        """Metodo sincrono non supportato.

        Solleva NotImplementedError per forzare l'uso di ainvoke().
        """
        raise NotImplementedError(
            "PostgresCheckpointer supporta solo operazioni asincrone. "
            "Usa graph.ainvoke() invece di graph.invoke()"
        )

    def list(
        self,
        config: RunnableConfig | None,
        *,
        filter: dict[str, Any] | None = None,
        before: RunnableConfig | None = None,
        limit: int | None = None,
    ) -> list[CheckpointTuple]:
        """Metodo sincrono non supportato.

        Solleva NotImplementedError per forzare l'uso di ainvoke().
        """
        raise NotImplementedError(
            "PostgresCheckpointer supporta solo operazioni asincrone. "
            "Usa graph.ainvoke() invece di graph.invoke()"
        )

    async def aclose(self) -> None:
        """Chiude il pool di connessioni PostgreSQL."""
        if self.pool:
            await self.pool.close()
            self.pool = None
            self._initialized = False
