"""Test di integrazione del PostgreSQL Checkpointer con LangGraph."""

import asyncio
from unittest.mock import MagicMock, patch, AsyncMock
import pytest

from brain.agents.postgres_checkpointer import PostgresCheckpointer


class TestPostgresCheckpointerIntegration:
    """Test del checkpointer PostgreSQL."""

    @pytest.mark.asyncio
    async def test_checkpointer_initialization(self) -> None:
        """Test che il checkpointer si inizializza correttamente."""
        # Mock del pool asyncpg per evitare connessioni reali
        with patch("brain.agents.postgres_checkpointer.asyncpg.create_pool") as mock_create_pool:
            mock_pool = AsyncMock()
            mock_create_pool.return_value = mock_pool

            # Crea il checkpointer
            checkpointer = PostgresCheckpointer("postgresql://test:test@localhost:5432/test")

            # Verifica che non sia inizializzato
            assert not checkpointer._initialized

            # Inizializza
            await checkpointer._ensure_initialized()

            # Verifica che il pool sia stato creato
            assert checkpointer._initialized
            assert checkpointer.pool is not None
            mock_create_pool.assert_called_once()

            # Verifica che sia stata creata la tabella
            mock_pool.acquire.return_value.__aenter__.return_value.execute.assert_called()

    @pytest.mark.asyncio
    async def test_checkpointer_aput(self) -> None:
        """Test del salvataggio di un checkpoint."""
        with patch("brain.agents.postgres_checkpointer.asyncpg.create_pool") as mock_create_pool:
            mock_pool = AsyncMock()
            mock_conn = AsyncMock()
            mock_pool.acquire.return_value.__aenter__.return_value = mock_conn
            mock_create_pool.return_value = mock_pool

            checkpointer = PostgresCheckpointer("postgresql://test:test@localhost:5432/test")
            await checkpointer._ensure_initialized()

            # Prepara il checkpoint
            config = {"configurable": {"thread_id": "test-thread"}}
            checkpoint = {"id": "cp-1", "data": "test"}
            metadata = {"source": "test"}
            new_versions = {}

            # Salva il checkpoint
            result = await checkpointer.aput(config, checkpoint, metadata, new_versions)

            # Verifica che il risultato sia la stessa config
            assert result == config

            # Verifica che execute sia stato chiamato
            mock_conn.execute.assert_called()

    @pytest.mark.asyncio
    async def test_checkpointer_aget(self) -> None:
        """Test del recupero di un checkpoint."""
        with patch("brain.agents.postgres_checkpointer.asyncpg.create_pool") as mock_create_pool:
            mock_pool = AsyncMock()
            mock_conn = AsyncMock()
            mock_pool.acquire.return_value.__aenter__.return_value = mock_conn

            # Mock della risposta
            mock_row = {"checkpoint_data": '{"id": "cp-1", "data": "test"}'}
            mock_conn.fetchrow.return_value = mock_row
            mock_create_pool.return_value = mock_pool

            checkpointer = PostgresCheckpointer("postgresql://test:test@localhost:5432/test")
            await checkpointer._ensure_initialized()

            # Recupera il checkpoint
            config = {"configurable": {"thread_id": "test-thread"}}
            checkpoint = await checkpointer.aget(config)

            # Verifica che il checkpoint sia stato recuperato
            assert checkpoint is not None
            assert checkpoint["id"] == "cp-1"

    def test_checkpointer_config_specs(self) -> None:
        """Test della proprietà config_specs."""
        checkpointer = PostgresCheckpointer("postgresql://test:test@localhost:5432/test")
        assert checkpointer.config_specs == []

    def test_sync_methods_not_supported(self) -> None:
        """Test che i metodi sincroni non siano supportati."""
        checkpointer = PostgresCheckpointer("postgresql://test:test@localhost:5432/test")

        config = {"configurable": {"thread_id": "test"}}
        checkpoint = {"id": "test"}
        metadata = {}
        versions = {}

        # Verifica che i metodi sincroni sollevino NotImplementedError
        with pytest.raises(NotImplementedError):
            checkpointer.put(config, checkpoint, metadata, versions)

        with pytest.raises(NotImplementedError):
            checkpointer.get(config)

        with pytest.raises(NotImplementedError):
            checkpointer.list(config)

    @pytest.mark.asyncio
    async def test_checkpointer_alist(self) -> None:
        """Test della lista di checkpoint."""
        with patch("brain.agents.postgres_checkpointer.asyncpg.create_pool") as mock_create_pool:
            mock_pool = AsyncMock()
            mock_conn = AsyncMock()
            mock_pool.acquire.return_value.__aenter__.return_value = mock_conn

            # Mock della risposta
            mock_rows = [
                {
                    "checkpoint_id": "cp-1",
                    "checkpoint_data": '{"id": "cp-1"}',
                    "metadata": "{}",
                    "versions": "{}",
                },
                {
                    "checkpoint_id": "cp-2",
                    "checkpoint_data": '{"id": "cp-2"}',
                    "metadata": "{}",
                    "versions": "{}",
                },
            ]
            mock_conn.fetch.return_value = mock_rows
            mock_create_pool.return_value = mock_pool

            checkpointer = PostgresCheckpointer("postgresql://test:test@localhost:5432/test")
            await checkpointer._ensure_initialized()

            # Lista i checkpoint
            config = {"configurable": {"thread_id": "test-thread"}}
            checkpoints = []
            async for cp in checkpointer.alist(config):
                checkpoints.append(cp)

            # Verifica che siano stati recuperati i checkpoint
            assert len(checkpoints) == 2
            assert checkpoints[0].checkpoint["id"] == "cp-1"
            assert checkpoints[1].checkpoint["id"] == "cp-2"

    @pytest.mark.asyncio
    async def test_checkpointer_aclose(self) -> None:
        """Test della chiusura del pool."""
        with patch("brain.agents.postgres_checkpointer.asyncpg.create_pool") as mock_create_pool:
            mock_pool = AsyncMock()
            mock_create_pool.return_value = mock_pool

            checkpointer = PostgresCheckpointer("postgresql://test:test@localhost:5432/test")
            await checkpointer._ensure_initialized()

            # Verifica che il pool sia inizializzato
            assert checkpointer.pool is not None

            # Chiude il pool
            await checkpointer.aclose()

            # Verifica che close sia stato chiamato
            mock_pool.close.assert_called_once()

            # Verifica che il pool sia None
            assert checkpointer.pool is None
            assert not checkpointer._initialized


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
