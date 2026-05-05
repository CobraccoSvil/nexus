"""Schema TypedDict per lo stato condiviso del grafo LangGraph."""
from __future__ import annotations

from operator import add
from typing import Annotated, Sequence

from langchain_core.messages import BaseMessage
from typing_extensions import TypedDict


class AgentState(TypedDict, total=False):
    """Stato condiviso tra tutti i nodi del grafo Nexus Agent.

    `messages` usa l'annotazione `add` per l'aggregazione automatica LangGraph.
    """

    messages: Annotated[Sequence[BaseMessage], add]
    user_intent: str
    task_type: str
    behavior_mode: str
    token_budget: int
    result: str | None
    provider_used: str | None
    model_used: str | None
    feedback_score: float | None
    latency_ms: float | None
    token_usage: int | None
    iterations: int
    thread_id: str

    # ── Agent tool loop (Fase 2) ─────────────────────────────────────────────
    # Popolati da executor_node quando il modello risponde con tool_use.
    pending_tool_uses: list[dict]
    # "tool_use" → loop verso tool_dispatch; "end_turn"/"stop" → learner.
    # "loop_detected" → executor ha rilevato chiamate ripetute, forza chiusura.
    stop_reason: str | None
    # Firme delle ultime tool calls (tool_name + hash input) per loop detection.
    recent_tool_signatures: list[str]
    # Tools dichiarati al modello (schema Anthropic-compatible).
    tools_json: list[dict]
    # System prompt del profilo agente (vuoto = default).
    system_text: str
    # Session/tool runner wiring (iniettati dal chiamante di /agent/run).
    session_id: str | None
    # Flag HITL: True dopo /agent/approve, permette di saltare interrupt in loop.
    approved: bool
    # Provider/model forzati dall'esterno (override routing).
    provider_override: str | None
    model_override: str | None
    # Profilo agente selezionato (core/github/specialized/general).
    # None = nessun profilo (comportamento legacy).
    profile_name: str | None

    # ── Metriche AI estese ──────────────────────────────────────────────────
    # Tokens e costi tracciati dal nodo executor
    prompt_tokens: int | None
    completion_tokens: int | None
    cache_creation_tokens: int | None
    cache_read_tokens: int | None
    total_tokens: int | None
    total_cost_usd: float | None
    cache_hit_rate: float | None
    temperature: float | None
    top_p: float | None
    created_at: str | None  # ISO8601
    completed_at: str | None  # ISO8601

    # ── Self-reflection (Fase 2) ────────────────────────────────────────────
    # Punteggio prodotto dal reflection_node (0.0-1.0). None = skip per sampling.
    reflection_score: float | None
    # Dettaglio per dimensione: correctness, completeness, efficiency, safety
    reflection_dimensions: dict | None
    # Punti deboli rilevati dal valutatore
    reflection_weaknesses: list | None
    # Suggerimenti di miglioramento del prompt
    reflection_suggestions: list | None
    # Reward finale fuso: 0.7 * heuristic + 0.3 * reflection_score
    final_reward: float | None
