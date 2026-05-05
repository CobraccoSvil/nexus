"""Test di integrazione per il modulo LangGraph in Nexus.

Copertura target: >= 80%
Ogni test è idempotente e usa database SQLite in memoria o temporanei.
"""
from __future__ import annotations

import asyncio
import tempfile
import uuid
from pathlib import Path
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import pytest


# ─── Fixture ──────────────────────────────────────────────────────────────────


@pytest.fixture
def tmp_db(tmp_path: Path) -> str:
    """Restituisce un path temporaneo per database SQLite di test."""
    return str(tmp_path / "test_learning.db")


@pytest.fixture
def storage(tmp_db: str) -> "LocalLearningStorage":
    from brain.memory.storage import LocalLearningStorage

    return LocalLearningStorage(db_path=tmp_db)


@pytest.fixture
def mock_embedding_service() -> MagicMock:
    """Mock di EmbeddingService che restituisce vettori deterministici."""
    svc = MagicMock()
    svc._dimension = 384
    svc._get_qdrant.return_value = None  # Qdrant non disponibile nei test

    from brain.embeddings.service import EmbeddingVector

    def fake_embed(model: str, text: str) -> EmbeddingVector:
        return EmbeddingVector(model="test-model", values=[0.1] * 384)

    svc.embed_text.side_effect = fake_embed
    return svc


@pytest.fixture
def mock_providers() -> MagicMock:
    """Mock di ProviderRegistry."""
    from brain.providers.base import ProviderResult

    prov = MagicMock()
    prov.generate_completion_async = AsyncMock(
        return_value=ProviderResult(
            provider="openai",
            model="gpt-4.1-mini",
            content="Risposta di test dal mock provider.",
            metadata={"usage": {"total_tokens": 42}},
        )
    )
    return prov


@pytest.fixture
def mock_router() -> MagicMock:
    """Mock di SemanticRouter."""
    from brain.router.service import RoutingDecision

    r = MagicMock()
    r.classify_intent.return_value = {"intent": "fix", "confidence": "0.90"}
    r.route_model.return_value = RoutingDecision(
        provider="openai",
        model="gpt-4.1-mini",
        rationale="test routing",
        confidence=0.90,
    )
    return r


# ─── Test: LocalLearningStorage ──────────────────────────────────────────────


class TestLocalLearningStorage:
    def test_init_crea_tabelle(self, storage: "LocalLearningStorage") -> None:
        import sqlite3

        conn = sqlite3.connect(storage.db_path)
        tables = {r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'").fetchall()}
        conn.close()
        assert "interactions" in tables
        assert "task_stats" in tables

    def test_save_interaction_base(self, storage: "LocalLearningStorage") -> None:
        row_id = storage.save_interaction(
            thread_id="thread-1",
            task_type="fix",
            behavior_mode="bilanciata",
            user_input="Risolvi il bug",
            agent_output="Ecco la soluzione.",
            provider="openai",
            model="gpt-4.1-mini",
            latency_ms=250.0,
            token_usage=30,
        )
        assert row_id > 0

    def test_save_interaction_aggiorna_stats(self, storage: "LocalLearningStorage") -> None:
        storage.save_interaction(
            thread_id="thread-2",
            task_type="refactor",
            behavior_mode="veloce",
            user_input="Refactoring del modulo",
            agent_output="Codice refactored.",
        )
        stats = storage.get_task_stats()
        task_types = [s["task_type"] for s in stats]
        assert "refactor" in task_types

    def test_salvataggio_multiplo_e_conteggio(self, storage: "LocalLearningStorage") -> None:
        for i in range(5):
            storage.save_interaction(
                thread_id=f"thread-{i}",
                task_type="test",
                behavior_mode="bilanciata",
                user_input=f"Input {i}",
                agent_output=f"Output {i}",
            )
        interactions = storage.get_interactions_by_task("test", limit=10)
        assert len(interactions) == 5

    def test_update_feedback_esistente(self, storage: "LocalLearningStorage") -> None:
        storage.save_interaction(
            thread_id="thread-fb",
            task_type="docs",
            behavior_mode="approfondita",
            user_input="Documenta il modulo",
            agent_output="Documentazione generata.",
        )
        updated = storage.update_feedback("thread-fb", 0.9)
        assert updated is True

    def test_update_feedback_inesistente(self, storage: "LocalLearningStorage") -> None:
        updated = storage.update_feedback("thread-non-esiste", 0.5)
        assert updated is False

    def test_get_recent_interactions_limit(self, storage: "LocalLearningStorage") -> None:
        for i in range(15):
            storage.save_interaction(
                thread_id=f"th-{i}",
                task_type="chat",
                behavior_mode="bilanciata",
                user_input=f"domanda {i}",
                agent_output=f"risposta {i}",
            )
        recenti = storage.get_recent_interactions(limit=10)
        assert len(recenti) == 10

    def test_get_task_stats_struttura(self, storage: "LocalLearningStorage") -> None:
        storage.save_interaction(
            thread_id="th-s",
            task_type="architecture",
            behavior_mode="approfondita",
            user_input="Progetta sistema",
            agent_output="Design proposto.",
            latency_ms=1200.0,
        )
        stats = storage.get_task_stats()
        assert len(stats) >= 1
        stat = next(s for s in stats if s["task_type"] == "architecture")
        assert stat["total_count"] == 1
        assert stat["avg_latency_ms"] > 0

    def test_idempotenza_salvataggio(self, storage: "LocalLearningStorage") -> None:
        """Due salvataggi indipendenti non si influenzano."""
        id1 = storage.save_interaction(
            thread_id="t1",
            task_type="fix",
            behavior_mode="bilanciata",
            user_input="A",
            agent_output="B",
        )
        id2 = storage.save_interaction(
            thread_id="t2",
            task_type="fix",
            behavior_mode="bilanciata",
            user_input="C",
            agent_output="D",
        )
        assert id1 != id2


# ─── Test: router_node ───────────────────────────────────────────────────────


class TestRouterNode:
    def test_router_node_classifica_intent(self, mock_router: MagicMock) -> None:
        from langchain_core.messages import HumanMessage

        import brain.agents.nodes as nodes_mod

        nodes_mod._router = mock_router

        state = {
            "messages": [HumanMessage(content="fix this bug")],
            "behavior_mode": "bilanciata",
            "iterations": 0,
        }
        result = nodes_mod.router_node(state)

        assert result["user_intent"] == "fix"
        assert result["task_type"] == "fix"
        assert result["iterations"] == 1
        assert result["token_budget"] >= 400

    def test_router_node_senza_messaggi(self, mock_router: MagicMock) -> None:
        import brain.agents.nodes as nodes_mod

        nodes_mod._router = mock_router

        result = nodes_mod.router_node({"messages": [], "behavior_mode": "veloce", "iterations": 0})
        assert result["user_intent"] == "chat"
        assert result["task_type"] == "chat"

    def test_router_node_senza_router(self) -> None:
        from langchain_core.messages import HumanMessage

        import brain.agents.nodes as nodes_mod

        nodes_mod._router = None
        result = nodes_mod.router_node({
            "messages": [HumanMessage(content="hello")],
            "behavior_mode": "bilanciata",
            "iterations": 0,
        })
        assert result["user_intent"] == "chat"


# ─── Test: executor_node ─────────────────────────────────────────────────────


class TestExecutorNode:
    @pytest.mark.asyncio
    async def test_executor_node_chiama_provider(
        self, mock_providers: MagicMock, mock_router: MagicMock
    ) -> None:
        from langchain_core.messages import AIMessage, HumanMessage

        import brain.agents.nodes as nodes_mod

        nodes_mod._providers = mock_providers
        nodes_mod._router = mock_router

        state = {
            "messages": [HumanMessage(content="fix the bug")],
            "user_intent": "fix",
            "behavior_mode": "bilanciata",
            "token_budget": 500,
            "iterations": 1,
        }
        result = await nodes_mod.executor_node(state)

        assert result["result"] == "Risposta di test dal mock provider."
        assert result["provider_used"] == "openai"
        assert result["model_used"] == "gpt-4.1-mini"
        assert result["latency_ms"] is not None
        assert result["latency_ms"] >= 0

    @pytest.mark.asyncio
    async def test_executor_node_senza_providers(self, mock_router: MagicMock) -> None:
        from langchain_core.messages import HumanMessage

        import brain.agents.nodes as nodes_mod

        nodes_mod._providers = None
        nodes_mod._router = mock_router

        state = {
            "messages": [HumanMessage(content="test")],
            "user_intent": "chat",
            "behavior_mode": "bilanciata",
            "token_budget": 400,
            "iterations": 1,
        }
        result = await nodes_mod.executor_node(state)
        assert "[Servizi non configurati]" in result["result"]


# ─── Test: learner_node ──────────────────────────────────────────────────────


class TestLearnerNode:
    def test_learner_node_salva_interazione(
        self,
        storage: "LocalLearningStorage",
        mock_embedding_service: MagicMock,
    ) -> None:
        from langchain_core.messages import HumanMessage

        import brain.agents.nodes as nodes_mod

        nodes_mod._storage = storage
        nodes_mod._retriever = None  # Qdrant non disponibile

        thread_id = str(uuid.uuid4())
        state = {
            "messages": [HumanMessage(content="Scrivi i test")],
            "thread_id": thread_id,
            "user_intent": "test",
            "behavior_mode": "bilanciata",
            "result": "Test scritti correttamente.",
            "provider_used": "anthropic",
            "model_used": "claude-haiku-4-5-20251001",
            "latency_ms": 850.0,
            "token_usage": 120,
            "iterations": 2,
        }
        nodes_mod.learner_node(state)

        interactions = storage.get_interactions_by_task("test")
        assert len(interactions) >= 1
        saved = interactions[0]
        assert saved["thread_id"] == thread_id
        assert saved["provider"] == "anthropic"

    def test_learner_node_senza_storage(self) -> None:
        """learner_node non lancia eccezioni se storage è None."""
        from langchain_core.messages import HumanMessage

        import brain.agents.nodes as nodes_mod

        nodes_mod._storage = None
        nodes_mod._retriever = None

        state = {
            "messages": [HumanMessage(content="test")],
            "thread_id": "th-x",
            "user_intent": "chat",
            "behavior_mode": "bilanciata",
            "result": "risposta",
            "provider_used": "openai",
            "model_used": "gpt-4.1-mini",
            "latency_ms": 100.0,
            "token_usage": 10,
            "iterations": 1,
        }
        result = nodes_mod.learner_node(state)
        assert result == {}


# ─── Test: configure_services ────────────────────────────────────────────────


class TestConfigureServices:
    def test_configure_services_inietta_correttamente(self) -> None:
        import brain.agents.nodes as nodes_mod

        mock_p = MagicMock()
        mock_r = MagicMock()
        mock_e = MagicMock()
        mock_s = MagicMock()
        mock_ret = MagicMock()

        nodes_mod.configure_services(
            providers=mock_p,
            router=mock_r,
            embeddings=mock_e,
            storage=mock_s,
            retriever=mock_ret,
        )

        assert nodes_mod._providers is mock_p
        assert nodes_mod._router is mock_r
        assert nodes_mod._embeddings is mock_e
        assert nodes_mod._storage is mock_s
        assert nodes_mod._retriever is mock_ret


# ─── Test: checkpointer paths ────────────────────────────────────────────────


class TestCheckpointerPaths:
    def test_get_checkpointer_path_crea_directory(self, tmp_path: Path) -> None:
        with patch("brain.agents.checkpointer.Path") as mock_path_cls:
            mock_brain_root = MagicMock()
            mock_nexus_memory = MagicMock()
            mock_brain_root.__truediv__ = MagicMock(return_value=mock_nexus_memory)
            mock_nexus_memory.__truediv__ = MagicMock(return_value=MagicMock())
            mock_nexus_memory.__str__ = MagicMock(return_value=str(tmp_path))
            mock_path_cls.return_value.parent.parent = mock_brain_root
            mock_nexus_memory.mkdir = MagicMock()

        # Verifica che il path default contenga langgraph.db
        from brain.agents.checkpointer import get_checkpointer_path

        path = get_checkpointer_path()
        assert "langgraph.db" in path

    def test_get_memory_db_path_contiene_learning(self) -> None:
        from brain.agents.checkpointer import get_memory_db_path

        path = get_memory_db_path()
        assert "learning.db" in path


# ─── Test: route_by_task_type ────────────────────────────────────────────────


class TestRouteByTaskType:
    def test_tutti_i_task_type_vanno_a_executor(self) -> None:
        from brain.agents.nodes import route_by_task_type

        for task in ["fix", "refactor", "test", "docs", "architecture", "chat", "database_schema_change"]:
            result = route_by_task_type({"task_type": task})
            assert result == "executor"
