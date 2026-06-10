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

# Timeout gRPC (secondi) per tool che richiedono esecuzione lunga.
# Tool non presenti in questa mappa usano il default di 120s.
_TOOL_TIMEOUT_OVERRIDES: dict[str, float] = {
    "run_playwright_tests": 900.0,  # Playwright: fino a 15 min + margine (42 test ~8-10 min)
    "run_command": 300.0,           # Comandi shell arbitrari
    "run_tests": 300.0,             # Test runner generico
    "build_project": 600.0,         # Build (npm/cargo/mvn)
    "install_dependencies": 300.0,  # pnpm/npm install
}

_DEFAULT_TOOL_TIMEOUT: float = 120.0


@dataclass
class ToolResult:
    """Risultato di una singola invocazione tool."""

    tool_use_id: str
    result_json: str
    is_error: bool
    duration_ms: int
    # Exit code STRUTTURATO (contratto dati A, censimento 2026-06-10): per i
    # tool che eseguono comandi, mcp-core lo estrae UNA VOLTA dal proprio output
    # e lo propaga qui. None = non applicabile (tool non-comando). I consumer
    # (helpers anti-stallo, criteria_runner) lo leggono invece di ri-parsare
    # "EXIT CODE: N" dal testo.
    exit_code: int | None = None

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
        if address:
            self.address = address
        else:
            # Gerarchia: env var (override emergenza) > DB (canonico) > hardcoded.
            env_addr = os.environ.get("TOOL_RUNNER_ADDR")
            if env_addr:
                self.address = env_addr
            else:
                from brain.utils.settings_db import get_setting
                # Fallback allineato alla realta' di bind di mcp-core
                # (env .env: TOOL_RUNNER_ADDR=127.0.0.1:50500). Mai impostare
                # 50501 qui: e' la porta dell'AgentRouter, non del ToolRunner.
                self.address = get_setting("tool_runner_addr", "127.0.0.1:50500")
        self._channel: grpc.aio.Channel | None = None
        self._stub: tool_runner_pb2_grpc.ToolRunnerStub | None = None

    # Limite massimo per messaggi gRPC ricevuti da mcp-core.
    # Il default gRPC e' 4MB: tool come search_in_files su codebase grandi
    # possono restituire risultati piu' grandi. Il ToolRunner Rust tronca
    # a 500KB, ma per sicurezza il canale accetta fino a 64MB.
    _GRPC_MAX_MSG_BYTES = 64 * 1024 * 1024  # 64MB

    async def _ensure_channel(self) -> tool_runner_pb2_grpc.ToolRunnerStub:
        if self._stub is None:
            self._channel = grpc.aio.insecure_channel(
                self.address,
                options=[
                    ("grpc.max_receive_message_length", self._GRPC_MAX_MSG_BYTES),
                    ("grpc.max_send_message_length", self._GRPC_MAX_MSG_BYTES),
                ],
            )
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
        timeout: float | None = None,
        canonical_tool: Any = None,
    ) -> ToolResult:
        """Esecuzione unaria.

        `tool_input` puo' essere un dict (serializzato) oppure una
        stringa JSON gia' pronta.

        Se `canonical_tool` (istanza di `brain.providers._models.CanonicalTool`)
        e' fornito, valida gli args contro l'`input_schema` PRIMA di chiamare
        mcp-core (M2 del piano provider-unification): se la validazione
        fallisce, ritorna immediatamente un ToolResult `is_error=True` con
        feedback strutturato in italiano per l'LLM (sblocca il caso Gemini
        che inventa attachment_id non esistenti).
        """
        if timeout is None:
            timeout = _TOOL_TIMEOUT_OVERRIDES.get(tool_name, _DEFAULT_TOOL_TIMEOUT)

        # Args potrebbero essere JSON string: per validare servono dict.
        validated_input = tool_input
        if isinstance(tool_input, str):
            try:
                validated_input = json.loads(tool_input)
            except json.JSONDecodeError:
                validated_input = None

        # M2 — validation pre-execution (opt-in via canonical_tool).
        if canonical_tool is not None and isinstance(validated_input, dict):
            try:
                from brain.providers.tool_validator import validate_tool_args
                vres = validate_tool_args(canonical_tool, validated_input)
                if not vres.ok:
                    logger.info(
                        "tool_validator: rejected tool=%s args (path=%s): %s",
                        tool_name, vres.error_path, vres.error_message,
                    )
                    return ToolResult(
                        tool_use_id=tool_use_id,
                        result_json=json.dumps(
                            {"error": "tool_args_validation_failed", "feedback": vres.feedback or vres.error_message},
                            ensure_ascii=False,
                        ),
                        is_error=True,
                        duration_ms=0,
                    )
            except Exception as exc:
                # Validator best-effort: errori interni non bloccano l'esecuzione.
                logger.debug("tool_validator skip per tool=%s: %s", tool_name, exc)

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
            exit_code=(resp.exit_code if getattr(resp, "has_exit_code", False) else None),
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
