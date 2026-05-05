"""Client async per il servizio gRPC ToolRunner esposto da mcp-core.

Il brain (LangGraph) invoca questo client dal nodo `tool_dispatch` per
eseguire i tool (run_command, read_file, write_file, run_service, ecc.)
contro il filesystem/terminale della sessione chat dell'utente.

Contratto: vedi proto/tool_runner.proto.
"""
from __future__ import annotations

import json
import logging
import os
import uuid
from dataclasses import dataclass
from typing import Any, AsyncIterator

import grpc

# Gli stub Python sono generati da grpc_server/compile_proto.py.
from brain.grpc_server.generated import tool_runner_pb2, tool_runner_pb2_grpc  # type: ignore[import-untyped]

logger = logging.getLogger(__name__)


@dataclass
class ToolResult:
    """Risultato di una singola invocazione tool."""

    tool_use_id: str
    result_json: str
    is_error: bool
    duration_ms: int

    def parsed(self) -> Any:
        """Decodifica `result_json` come JSON se possibile, altrimenti
        ritorna la stringa grezza."""
        try:
            return json.loads(self.result_json)
        except (json.JSONDecodeError, TypeError):
            return self.result_json


class ToolRunnerClient:
    """Wrapper async attorno al service gRPC ToolRunner.

    Istanza riusabile: tiene aperto un canale persistente verso
    mcp-core. Thread-safe per uso concorrente.
    """

    def __init__(self, address: str | None = None) -> None:
        self.address = address or os.environ.get(
            "TOOL_RUNNER_ADDR", "127.0.0.1:50071"
        )
        self._channel: grpc.aio.Channel | None = None
        self._stub: tool_runner_pb2_grpc.ToolRunnerStub | None = None

    async def _ensure_channel(self) -> tool_runner_pb2_grpc.ToolRunnerStub:
        if self._stub is None:
            self._channel = grpc.aio.insecure_channel(self.address)
            self._stub = tool_runner_pb2_grpc.ToolRunnerStub(self._channel)
        return self._stub

    async def close(self) -> None:
        if self._channel is not None:
            await self._channel.close()
            self._channel = None
            self._stub = None

    async def execute_tool(
        self,
        *,
        tool_name: str,
        tool_input: Any,
        session_id: str,
        tool_use_id: str,
        correlation_id: str | None = None,
        timeout: float | None = 120.0,
    ) -> ToolResult:
        """Esecuzione unaria.

        `tool_input` puo' essere un dict (serializzato) oppure una
        stringa JSON gia' pronta.
        """
        stub = await self._ensure_channel()
        if isinstance(tool_input, str):
            input_json = tool_input
        else:
            input_json = json.dumps(tool_input, ensure_ascii=False)

        req = tool_runner_pb2.ExecuteToolRequest(
            tool_name=tool_name,
            tool_input_json=input_json,
            session_id=session_id,
            tool_use_id=tool_use_id,
            correlation_id=correlation_id or str(uuid.uuid4()),
        )
        try:
            resp = await stub.ExecuteTool(req, timeout=timeout)
        except grpc.aio.AioRpcError as e:
            logger.error(
                "ToolRunner.ExecuteTool fallita tool=%s session=%s code=%s: %s",
                tool_name,
                session_id,
                e.code(),
                e.details(),
            )
            # Propaghiamo come tool_result d'errore, non come eccezione:
            # il loop LangGraph deve poter continuare con is_error=True.
            return ToolResult(
                tool_use_id=tool_use_id,
                result_json=json.dumps(
                    {"error": f"gRPC {e.code().name}: {e.details()}"}
                ),
                is_error=True,
                duration_ms=0,
            )

        return ToolResult(
            tool_use_id=resp.tool_use_id,
            result_json=resp.tool_result_json,
            is_error=resp.is_error,
            duration_ms=resp.duration_ms,
        )

    async def stream_tool_output(
        self,
        *,
        tool_name: str,
        tool_input: Any,
        session_id: str,
        tool_use_id: str,
        correlation_id: str | None = None,
    ) -> AsyncIterator[tool_runner_pb2.ToolChunk]:
        """Esecuzione streaming per tool long-running."""
        stub = await self._ensure_channel()
        if isinstance(tool_input, str):
            input_json = tool_input
        else:
            input_json = json.dumps(tool_input, ensure_ascii=False)

        req = tool_runner_pb2.ExecuteToolRequest(
            tool_name=tool_name,
            tool_input_json=input_json,
            session_id=session_id,
            tool_use_id=tool_use_id,
            correlation_id=correlation_id or str(uuid.uuid4()),
        )
        async for chunk in stub.StreamToolOutput(req):
            yield chunk
