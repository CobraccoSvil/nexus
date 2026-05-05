"""Test di integrazione per il ToolRunner gRPC.

Richiede un'istanza di mcp-core avviata con ENABLE_TOOL_RUNNER=1 +
TOOL_RUNNER_ADDR=127.0.0.1:50071 (o variabile d'ambiente personalizzata
letta dal client). Una session_id valida con workspace associato deve
esistere nel DB Nexus. Skip se le variabili non sono settate.
"""
from __future__ import annotations

import os
import socket
import uuid

import pytest

from brain.grpc_clients.tool_runner_client import ToolRunnerClient


def _tool_runner_reachable(addr: str) -> bool:
    host, _, port = addr.partition(":")
    try:
        with socket.create_connection((host, int(port)), timeout=1.0):
            return True
    except OSError:
        return False


TOOL_RUNNER_ADDR = os.environ.get("TOOL_RUNNER_ADDR", "127.0.0.1:50071")
TEST_SESSION_ID = os.environ.get("TOOL_RUNNER_TEST_SESSION_ID")

pytestmark = pytest.mark.skipif(
    not _tool_runner_reachable(TOOL_RUNNER_ADDR) or not TEST_SESSION_ID,
    reason=(
        "ToolRunner non raggiungibile o TOOL_RUNNER_TEST_SESSION_ID non "
        "impostata. Avvia mcp-core con ENABLE_TOOL_RUNNER=1 e fornisci "
        "una session_id valida."
    ),
)


@pytest.mark.asyncio
async def test_list_files_roundtrip() -> None:
    """Chiama `list_files` su path "." per verificare il giro
    richiesta/risposta end-to-end."""
    client = ToolRunnerClient(address=TOOL_RUNNER_ADDR)
    try:
        result = await client.execute_tool(
            tool_name="list_files",
            tool_input={"path": "."},
            session_id=TEST_SESSION_ID,
            tool_use_id=f"test-{uuid.uuid4()}",
        )
        assert result.tool_use_id.startswith("test-")
        assert result.result_json, "risultato vuoto"
        assert not result.is_error, f"errore tool: {result.result_json}"
        assert result.duration_ms >= 0
    finally:
        await client.close()


@pytest.mark.asyncio
async def test_unknown_tool_is_error() -> None:
    """Un tool_name inesistente deve tornare is_error=True senza
    sollevare eccezioni lato client."""
    client = ToolRunnerClient(address=TOOL_RUNNER_ADDR)
    try:
        result = await client.execute_tool(
            tool_name="tool_che_non_esiste",
            tool_input={},
            session_id=TEST_SESSION_ID,
            tool_use_id=f"test-{uuid.uuid4()}",
        )
        assert result.is_error is True
    finally:
        await client.close()
