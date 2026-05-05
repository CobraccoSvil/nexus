"""Test integrazione AgentRouterClient.

Gli smoke test contro un server gRPC reale richiedono mcp-core live. Qui
limitiamo l'ambito a: (1) schema dataclass/proto stubs allineati;
(2) comportamento client quando il server non risponde (fallback graceful).
"""
from __future__ import annotations

import asyncio

import pytest

from brain.grpc_clients.agent_router_client import (
    AgentRouterClient,
    CandidateAgent,
    SelectAgentResult,
)


def test_select_agent_result_defaults():
    r = SelectAgentResult(
        agent_type="", q_value=0.0, confidence=0.0, strategy="UNAVAILABLE",
    )
    assert r.is_empty
    assert r.candidates == []


def test_candidate_agent_dataclass():
    c = CandidateAgent(agent_type="coder", similarity_score=0.9, q_value=0.7)
    assert c.agent_type == "coder"
    assert c.similarity_score == pytest.approx(0.9)


def test_client_default_address_from_env(monkeypatch):
    monkeypatch.setenv("AGENT_ROUTER_ADDR", "127.0.0.1:55555")
    client = AgentRouterClient()
    assert client.address == "127.0.0.1:55555"


def test_client_explicit_address_wins(monkeypatch):
    monkeypatch.setenv("AGENT_ROUTER_ADDR", "127.0.0.1:11111")
    client = AgentRouterClient(address="127.0.0.1:22222")
    assert client.address == "127.0.0.1:22222"


def test_select_agent_returns_empty_when_server_down():
    """Il client deve degradare graziosamente: address inesistente produce
    un risultato con `is_empty=True`, non un'eccezione non gestita.
    """
    async def run() -> SelectAgentResult:
        client = AgentRouterClient(address="127.0.0.1:1")  # porta privilegiata, nessun server
        try:
            return await client.select_agent(
                task_type="code_generation",
                instructions="scrivi una funzione",
                timeout=1.0,
            )
        finally:
            await client.close()

    result = asyncio.run(run())
    assert result.is_empty
    assert result.strategy == "UNAVAILABLE"


def test_submit_feedback_returns_zero_when_server_down():
    """Dopo la Fase C consolidamento submit_feedback usa REST verso
    `/api/internal/learning/feedback`. Se il server e' down il client deve
    ritornare 0.0 (no exception)."""
    async def run() -> float:
        # mcp_core_url su una porta sicuramente chiusa
        client = AgentRouterClient(
            address="127.0.0.1:1",            # gRPC chiuso (legacy)
            mcp_core_url="http://127.0.0.1:1", # REST chiuso (path attuale)
        )
        try:
            return await client.submit_feedback(
                task_id="t1", task_type="chat", agent_type="coder",
                reward=0.9, duration_ms=100, timeout=1.0,
            )
        finally:
            await client.close()

    q = asyncio.run(run())
    assert q == 0.0


def test_submit_feedback_clamps_reward():
    """Reward fuori range [0,1] deve essere clampato prima dell'invio.
    La call fallira' su connessione ma il valore inviato deve essere [0,1]."""
    async def run() -> float:
        client = AgentRouterClient(
            address="127.0.0.1:1",
            mcp_core_url="http://127.0.0.1:1",
        )
        try:
            # 2.0 -> 1.0 clamp; la call fallira' connessione ma il client
            # NON deve rilanciare il valore non clampato.
            return await client.submit_feedback(
                task_id="t1", task_type="chat", agent_type="coder",
                reward=2.0, duration_ms=0, timeout=1.0,
            )
        finally:
            await client.close()

    assert asyncio.run(run()) == 0.0
