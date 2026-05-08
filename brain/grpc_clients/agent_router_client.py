"""Client async per il servizio gRPC AgentRouter esposto da mcp-core.

Il brain (LangGraph) invoca `select_agent` dal `router_node` come sub-step
del routing semantico: il Q-Learning router di nexus-orchestrator sceglie
il profilo agente piu' adatto al task, che il brain carica da
`brain/agents/profiles/<name>.yaml`. Al termine dell'esecuzione il brain
chiama `submit_feedback` per aggiornare il Q-value con il reward osservato.

Contratto: vedi proto/agent_router.proto.
"""
from __future__ import annotations

import json
import logging
import os
import uuid
from dataclasses import dataclass, field
from typing import Any

import grpc

from brain.grpc_server.generated import agent_router_pb2, agent_router_pb2_grpc  # type: ignore[import-untyped]

logger = logging.getLogger(__name__)


@dataclass
class CandidateAgent:
    agent_type: str
    similarity_score: float
    q_value: float


@dataclass
class SelectAgentResult:
    agent_type: str
    q_value: float
    confidence: float
    strategy: str
    candidates: list[CandidateAgent] = field(default_factory=list)
    decision_time_us: int = 0

    @property
    def is_empty(self) -> bool:
        return not self.agent_type


class AgentRouterClient:
    """Wrapper async attorno al service gRPC AgentRouter. Thread-safe."""

    def __init__(
        self,
        address: str | None = None,
        mcp_core_url: str | None = None,
    ) -> None:
        # gRPC address (per select_agent — read-only Q-table query, mantenuto)
        self.address = address or os.environ.get(
            "AGENT_ROUTER_ADDR", "127.0.0.1:50072"
        )
        # REST URL (per submit_feedback — Fase C consolidamento)
        self.mcp_core_url = (mcp_core_url or os.environ.get(
            "MCP_CORE_URL", "http://localhost:4000"
        )).rstrip("/")
        self._channel: grpc.aio.Channel | None = None
        self._stub: agent_router_pb2_grpc.AgentRouterStub | None = None

    async def _ensure_channel(self) -> agent_router_pb2_grpc.AgentRouterStub:
        # Ricreo il canale se assente oppure se lo stato non e' piu' attivo
        # (TRANSIENT_FAILURE / SHUTDOWN) — gestisce il reconnect automatico
        # dopo che il server viene avviato in un secondo momento.
        if self._channel is not None:
            try:
                state = self._channel.get_state(try_to_connect=False)
                # grpc.ChannelConnectivity: IDLE=0, CONNECTING=1, READY=2,
                # TRANSIENT_FAILURE=3, SHUTDOWN=4
                if state.value >= 4:  # SHUTDOWN
                    await self._channel.close()
                    self._channel = None
                    self._stub = None
            except Exception:
                self._channel = None
                self._stub = None
        if self._stub is None:
            self._channel = grpc.aio.insecure_channel(self.address)
            self._stub = agent_router_pb2_grpc.AgentRouterStub(self._channel)
        return self._stub

    async def close(self) -> None:
        if self._channel is not None:
            await self._channel.close()
            self._channel = None
            self._stub = None

    async def select_agent(
        self,
        *,
        task_type: str,
        instructions: str,
        task_id: str | None = None,
        context: dict[str, Any] | None = None,
        forced_agent_type: str = "",
        timeout: float | None = 10.0,
    ) -> SelectAgentResult:
        """Chiede al router Q-Learning quale profilo agente usare.

        Ritorna un `SelectAgentResult` sempre: in caso di errore/connessione
        fallita il campo `agent_type` e' vuoto e il chiamante puo' fallback.
        """
        stub = await self._ensure_channel()
        context_json = json.dumps(context or {}, ensure_ascii=False) if context else ""
        req = agent_router_pb2.SelectAgentRequest(
            task_id=task_id or str(uuid.uuid4()),
            task_type=task_type,
            instructions=instructions,
            context_json=context_json,
            forced_agent_type=forced_agent_type,
        )
        try:
            resp = await stub.SelectAgent(req, timeout=timeout)
        except grpc.aio.AioRpcError as e:
            logger.warning(
                "AgentRouter.SelectAgent fallita task_type=%s code=%s: %s",
                task_type, e.code(), e.details(),
            )
            return SelectAgentResult(
                agent_type="", q_value=0.0, confidence=0.0, strategy="UNAVAILABLE",
            )
        return SelectAgentResult(
            agent_type=resp.agent_type,
            q_value=resp.q_value,
            confidence=resp.confidence,
            strategy=resp.strategy,
            candidates=[
                CandidateAgent(c.agent_type, c.similarity_score, c.q_value)
                for c in resp.candidates
            ],
            decision_time_us=resp.decision_time_us,
        )

    async def submit_feedback(
        self,
        *,
        task_id: str,
        task_type: str,
        agent_type: str,
        reward: float,
        duration_ms: int = 0,
        is_terminal: bool = True,
        timeout: float | None = 5.0,
    ) -> float:
        """Invia il reward osservato per aggiornare il Q-value.

        Fase C consolidamento (vedi piano `questo-lo-stesso-proud-blossom.md`):
        sostituita la chiamata gRPC `SubmitFeedback` con REST POST verso
        `/api/internal/learning/feedback` di mcp-core. Motivazione:
        - Rust diventa unico writer della Q-table (no race condition).
        - Brain non dipende piu' da `agent_router_pb2.FeedbackRequest`
          (rigenerazione protobuf non necessaria al cambio schema feedback).
        - Coerente con `/api/internal/routing/decide` (Fase A).

        Il path gRPC `select_agent` e' mantenuto invariato per la query Q-router
        (legge la Q-table, solo lettura, no race issue).

        Ritorna il Q-value aggiornato, o 0.0 se la chiamata fallisce.
        """
        import json as _json
        import urllib.request
        import urllib.error
        url = f"{self.mcp_core_url}/api/internal/learning/feedback"
        body = _json.dumps({
            "task_id": task_id,
            "task_type": task_type,
            "agent_type": agent_type,
            "reward": float(max(0.0, min(1.0, reward))),
            "duration_ms": int(duration_ms),
            "is_terminal": bool(is_terminal),
        }).encode("utf-8")
        req = urllib.request.Request(
            url, data=body, headers={"Content-Type": "application/json"},
        )
        # Eseguiamo la chiamata HTTP in un executor per non bloccare il loop
        # async (urllib e' sincrono). Timeout 5s di default come la versione gRPC.
        import asyncio
        loop = asyncio.get_running_loop()
        timeout_val = float(timeout) if timeout is not None else 5.0
        try:
            payload = await loop.run_in_executor(
                None,
                lambda: _http_post_json_blocking(req, timeout_val),
            )
            return float(payload.get("new_q_value", 0.0))
        except (urllib.error.URLError, TimeoutError, OSError, ValueError) as e:
            logger.warning(
                "internal_learning/feedback fallita task_id=%s: %s",
                task_id, e,
            )
            return 0.0


def _http_post_json_blocking(request: "urllib.request.Request", timeout: float) -> dict:
    """Helper sincrono: esegue la POST e ritorna il JSON decodato.
    Usato dentro `loop.run_in_executor` per integrazione async.
    """
    import json as _json
    import urllib.request
    with urllib.request.urlopen(request, timeout=timeout) as resp:
        return _json.loads(resp.read().decode("utf-8"))
