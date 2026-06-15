"""Punto unico (regola L) per il freno di contesto sui codepath sub-agente.

Background: la pipeline anti-overflow del contesto (dedup tool_results, drop
base64, rolling summary, token brake) era applicata SOLO dentro
`executor_node` nel main loop agentico. Tutti gli altri codepath che chiamano
direttamente i provider LLM (planner_node, verifier_node, clarify_or_expand,
understanding_node, next_actions, summarizer) bypassavano il freno e potevano
spedire al gateway contesti enormi -> il sub-agente Mistral apriva con 254K
token nella history e non riusciva a chiudere il task.

Questo modulo espone due helper neutri:

  - ``apply_context_reduction(messages, model, iteration=0) -> list``
    Pipeline completa di riduzione su una lista di messaggi LangChain
    (HumanMessage/AIMessage/ToolMessage). Idempotente: una history gia'
    compressa non cambia. Pensata per i sub-agenti che ricevono `messages`
    da uno state condiviso (es. planner_node ramo principale e fallback).

  - ``clamp_single_prompt(prompt, model) -> str``
    Troncamento difensivo "head + tail" su un singolo prompt stringa (o un
    singolo `user_msg` per chiamate tipo `[{"role": "user", "content": ...}]`).
    Usato dai codepath che costruiscono al volo un solo messaggio
    (verifier, understanding, next_actions, summarizer, clarify).

Entrambi gli helper:
  - leggono la config da ``brain.agents.context_offload._load_offload_config()``
    indirettamente attraverso le funzioni esistenti (cache 60s DB-driven),
  - non sollevano mai (best-effort: in caso di errore ritornano l'input
    invariato, con log warning),
  - non hanno hardcode di nomi modello (regola G): la window e' letta da
    ``_model_context_window(model)`` -> ``ai_price_catalog``.

Riusa SENZA duplicare:
  - ``_dedup_tool_results_history`` (brain.agents.nodes.helpers)
  - ``_drop_unused_base64_payloads`` (idem)
  - ``_apply_rolling_summary`` (idem)
  - ``_apply_token_brake`` (brain.agents.nodes.__init__, import lazy per
    rompere il ciclo helpers <- nodes <- helpers)
  - ``_model_context_window`` + ``_count_tokens`` (helpers)
  - ``orchestrator_config.get()`` per le soglie context (max_context_ratio,
    forced_rag_threshold, ecc.), allineate al codepath executor.
"""
from __future__ import annotations

import logging
from typing import Any

logger = logging.getLogger(__name__)


# ── Helper interni: caricamento config con cache 60s (riusa offload_config) ───
def _load_ctx_cfg() -> dict[str, Any]:
    """Configurazione di compressione/brake usata anche da executor_node.

    Riusa ``orchestrator_config.get()`` (lo stesso dizionario letto da
    executor_node, vedi nodes/__init__.py righe 2630-2701) cosi' la riduzione
    sub-agente segue esattamente le stesse soglie del main loop. Mai solleva.
    """
    try:
        from brain.agents import orchestrator_config

        return orchestrator_config.get() or {}
    except Exception as exc:
        logger.warning("context_brake: orchestrator_config non disponibile (%s)", exc)
        return {}


# ── Pipeline completa per liste di messaggi LangChain ────────────────────────
def apply_context_reduction(
    messages: list[Any],
    model: str | None,
    iteration: int = 0,
) -> list[Any]:
    """Applica la pipeline di riduzione contesto a una lista di messaggi.

    Sequenza (stessa di executor_node, vedi nodes/__init__.py:2630-2701):
      1. dedup tool_results identici nella history (semantico, hash sui primi 256 char);
      2. drop dei payload base64 non piu' usati (mantiene ultimi `keep_recent`);
      3. rolling summary (offload lossless su Qdrant se attivo);
      4. token brake (cap hard sotto window*ratio, fallback a head+tail).

    Idempotente: ri-applicare su messaggi gia' compressi e' un no-op (le
    funzioni sottostanti gia' lo garantiscono). Non solleva: ogni errore
    degrada a "ritorna l'input invariato".

    Parametri:
      messages: lista di messaggi LangChain (HumanMessage/AIMessage/ToolMessage).
      model: nome modello target; usato dal brake per leggere context_window.
      iteration: indice iterazione corrente (rolling summary la usa come trigger
        modulo `window`). Default 0 = "primo turno", che disattiva il rolling.

    Ritorna la nuova lista (mai mutata in-place).
    """
    if not messages:
        return messages
    try:
        cfg = _load_ctx_cfg()
        # 1. dedup tool_results
        try:
            from .nodes.helpers import _dedup_tool_results_history

            if cfg.get("dedup_tool_results_enabled", True):
                messages = _dedup_tool_results_history(messages)
        except Exception as exc:
            logger.debug("context_brake: dedup skip (%s)", exc)

        # 2. drop base64 non usati
        try:
            from .nodes.helpers import _drop_unused_base64_payloads

            messages = _drop_unused_base64_payloads(messages)
        except Exception as exc:
            logger.debug("context_brake: drop_base64 skip (%s)", exc)

        # 3. rolling summary
        try:
            from .nodes.helpers import _apply_rolling_summary

            # embeddings=None: il rolling_summary degrada a no-op se non c'e'
            # un embedding service per offloadare. I sub-agenti girano senza
            # contesto embedding caricato — meglio saltare l'offload che
            # bloccare la chiamata.
            messages = _apply_rolling_summary(messages, iteration, None)
        except Exception as exc:
            logger.debug("context_brake: rolling_summary skip (%s)", exc)

        # 4. token brake (cap hard)
        if model:
            try:
                # Import lazy: nodes/__init__.py importa da helpers, helpers da
                # nodes romperebbe il ciclo. Risolviamo qui dentro.
                from .nodes import _apply_token_brake

                messages = _apply_token_brake(messages, model, cfg, iteration)
            except Exception as exc:
                logger.debug("context_brake: token_brake skip (%s)", exc)
        return messages
    except Exception as exc:
        logger.warning("context_brake: pipeline fallita, history invariata (%s)", exc)
        return messages


# ── Clamp difensivo per prompt singoli ───────────────────────────────────────
def _max_prompt_tokens(model: str | None, cfg: dict[str, Any]) -> int:
    """Soglia massima di token per un singolo prompt (head+tail).

    Usa ``max_context_ratio`` (default 0.55) per coerenza con il main loop:
    un prompt da un sub-agente non puo' superare la stessa frazione del
    window del modello. Fallback safe 65K token (META di un window 128K)
    se la window del modello non e' letta dal DB.
    """
    try:
        from .nodes.helpers import _model_context_window

        window = _model_context_window(model or "") if model else 128_000
    except Exception:
        window = 128_000
    ratio = float(cfg.get("max_context_ratio", 0.55) or 0.55)
    # Sotto 0.1 perdiamo il senso del clamp; sopra 0.95 non ha effetto pratico.
    ratio = max(0.1, min(0.95, ratio))
    return max(1024, int(window * ratio))


def clamp_single_prompt(prompt: str, model: str | None) -> str:
    """Tronca un singolo prompt a ~max_context_ratio * window (head + tail).

    Strategia:
      - Stima i token con tiktoken (riuso ``_count_tokens``).
      - Se sotto soglia: ritorna invariato (no-op idempotente).
      - Se sopra: mantiene il 60% iniziale + il 40% finale del budget (head+tail),
        inserendo un marker esplicito al taglio. Cosi' la richiesta originale
        (in testa) e il contesto piu' recente (in coda) sopravvivono entrambi.

    Non solleva: ogni errore (tokenizer non disponibile, modello non in catalogo)
    ricade su una stima ``len(prompt)//4 == tokens`` e applica comunque il taglio.
    Idempotente: ri-applicare su un prompt gia' troncato non lo cambia.
    """
    if not prompt or not isinstance(prompt, str):
        return prompt
    try:
        cfg = _load_ctx_cfg()
        max_tokens = _max_prompt_tokens(model, cfg)
        try:
            from .nodes.helpers import _count_tokens

            est = _count_tokens(prompt)
        except Exception:
            est = max(1, len(prompt) // 4)
        if est <= max_tokens:
            return prompt
        # Budget caratteri: stimiamo 4 char/token come limite superiore prudente.
        # Il marker e' un blocco esplicito cosi' il modello capisce il taglio.
        budget_chars = max_tokens * 4
        marker = "\n\n[... contenuto troncato dal freno di contesto (sub-agente) ...]\n\n"
        head_chars = int(budget_chars * 0.6)
        tail_chars = max(0, budget_chars - head_chars - len(marker))
        if head_chars + tail_chars >= len(prompt):
            return prompt
        new_prompt = prompt[:head_chars] + marker + prompt[-tail_chars:] if tail_chars else (
            prompt[:head_chars] + marker
        )
        logger.info(
            "context_brake: clamp_single_prompt model=%s est_tokens=%d > max=%d -> "
            "troncato %d -> %d char (head=%d tail=%d)",
            model, est, max_tokens, len(prompt), len(new_prompt), head_chars, tail_chars,
        )
        return new_prompt
    except Exception as exc:
        logger.warning("context_brake: clamp_single_prompt fallito (%s), prompt invariato", exc)
        return prompt


__all__ = ["apply_context_reduction", "clamp_single_prompt"]
