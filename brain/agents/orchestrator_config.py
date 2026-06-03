"""Configurazione runtime per il nuovo orchestrator Plan/Act/Verify (PR-1+).

Tutti i parametri vengono letti ESCLUSIVAMENTE dalla tabella `settings` del DB
(categoria 'orchestrator'). Nessuna variabile d'ambiente viene usata: ogni
modifica admin e' applicabile a caldo dall'interfaccia admin senza rideploy.

Cache in memoria con TTL 60 secondi: un aggiornamento admin diventa attivo
entro un minuto senza riavvio del servizio.

Chiavi DB (categoria 'orchestrator', vedi mig 0150):
    plan_phase_enabled              bool   default: false
    plan_behavior_modes             csv    default: 'automatico,continuo'
    plan_intents                    csv    default: 'code,implement,fix,refactor,scaffold_app,architecture'
    plan_min_token_budget           int    default: 2000
    planner_prompt_key              str    default: 'agent.planner.base'
    todo_reminder_every_n_steps     int    default: 5
    todo_reminder_min_todos         int    default: 3
    verifier_enabled                bool   default: false
    max_verify_cycles               int    default: 3
    max_plan_revisions              int    default: 2
    verifier_timeout_s              float  default: 30.0

Fail-safe: se DB irraggiungibile al primo avvio o keys mancanti, i default
conservativi qui sotto DISABILITANO la feature (plan_phase_enabled=False,
verifier_enabled=False) cosi' il sistema continua a comportarsi come prima
del PR-1.
"""
from __future__ import annotations

import logging
import threading
import time
from typing import Any

logger = logging.getLogger(__name__)

# TTL della cache locale (secondi). Ogni modifica admin diventa attiva entro questo intervallo.
_CACHE_TTL_S = 60.0

# Tutte le chiavi attese dal DB. Niente prefisso 'orchestrator.' qui: viene aggiunto
# in _load_from_db quando si formula la query. Le chiavi locali tengono il nome
# pulito per uso programmatico.
_KEY_PREFIX = "orchestrator."
_KEYS = (
    "plan_phase_enabled",
    "plan_behavior_modes",
    "plan_intents",
    "plan_min_token_budget",
    "planner_prompt_key",
    "todo_reminder_every_n_steps",
    "todo_reminder_min_todos",
    "verifier_enabled",
    "max_verify_cycles",
    "max_plan_revisions",
    "verifier_timeout_s",
    # PR-C: worker-mode (orchestrator-worker puro)
    "worker_mode_enabled",
    "worker_mode_tool_whitelist",
    # PR-D: attivazione adattiva da confidence
    "adaptive_classifier_enabled",
    "adaptive_gating_enabled",
    "adaptive_agentic_score_min",
    "adaptive_low_confidence_max",
    # gia' presenti in DB ma usati anche qui: subagents_enabled / auto_delegation
    "subagents_enabled",
    "auto_delegation_enabled",
    "max_parallel_subagents",
    # Cluster 1: plan_rationale + RAG
    "plan_rationale_enabled",
    "plan_rationale_rag_topk",
    "plan_rationale_min_score",
    "plan_rationale_persist_as_note",
    # Cluster 3: verifica esplorativa RAG-informed
    "exploratory_verify_enabled",
    "exploratory_verify_max_cycles",
    "exploratory_verify_topk",
    "exploratory_verify_min_score",
    # Cluster 2: nodo understanding
    "understanding_enabled",
    "understanding_fanout_enabled",
    "understanding_synthesize_enabled",
    "understanding_topk",
    "understanding_min_token_budget",
    "understanding_max_explore",
    # Allineamento Componente B: contesto continuo via RAG ai sub-agent
    "subagent_rag_grounding_enabled",
    "subagent_rag_grounding_topk",
    "subagent_rag_grounding_min_score",
    "subagent_rag_grounding_snippet_max",
    "subagent_inherit_plan_rationale",
    # Comp.3a/3b: coordinamento azioni via DAG
    "dag_topological_enabled",
    "dag_parallel_enabled",
    "dag_max_parallel",
    "dag_verify_layer",
    # Final gate generale (fail-closed) anti-placeholder
    "final_gate_enabled",
    "final_gate_software_intents",
    "final_gate_max_cycles",
    "import_staging_dirs",
    "no_orphan_min_ratio",
    "verifier_fail_closed",
)

# Override del nome completo (key DB) per le chiavi che NON usano il prefisso
# 'orchestrator.'. Il final gate vive sotto la categoria/prefisso 'agent.'
# (coerente con le migrazioni recenti su settings, es. 0262/0263). Le chiavi
# locali restano pulite per uso programmatico.
_KEY_FULL_NAME: dict[str, str] = {
    "final_gate_enabled": "agent.final_gate.enabled",
    "final_gate_software_intents": "agent.final_gate.software_intents",
    "final_gate_max_cycles": "agent.final_gate.max_cycles",
    "import_staging_dirs": "agent.import_staging_dirs",
    "no_orphan_min_ratio": "agent.no_orphan.min_ratio",
    "verifier_fail_closed": "agent.verifier.fail_closed",
}


def _full_key(local_key: str) -> str:
    """Nome completo della chiave nel DB per una chiave locale."""
    return _KEY_FULL_NAME.get(local_key, _KEY_PREFIX + local_key)

# Default conservativi: feature OFF se DB irraggiungibile.
_SAFE_DEFAULTS: dict[str, Any] = {
    "plan_phase_enabled": False,
    "plan_behavior_modes": ["automatico", "continuo"],
    "plan_intents": ["code", "implement", "fix", "refactor", "scaffold_app", "architecture"],
    "plan_min_token_budget": 2000,
    "planner_prompt_key": "agent.planner.base",
    "todo_reminder_every_n_steps": 5,
    "todo_reminder_min_todos": 3,
    "verifier_enabled": False,
    "max_verify_cycles": 3,
    "max_plan_revisions": 2,
    "verifier_timeout_s": 30.0,
    # PR-C: worker-mode OFF di default (sistema attuale invariato).
    "worker_mode_enabled": False,
    "worker_mode_tool_whitelist": [
        "list_files", "read_file", "search_in_files", "recall_context",
        "search_codebase_semantic", "nexus_todo_write", "dispatch_subagent",
        "nexus_subagent_poll", "nexus_subagent_resume",
    ],
    # PR-D: attivazione adattiva OFF di default.
    "adaptive_classifier_enabled": False,
    "adaptive_gating_enabled": False,
    "adaptive_agentic_score_min": 0.7,
    "adaptive_low_confidence_max": 0.5,
    # delega/subagent (default coerenti con DB attuale)
    "subagents_enabled": True,
    "auto_delegation_enabled": True,
    "max_parallel_subagents": 3,
    # Cluster 1: plan_rationale + RAG (OFF di default)
    "plan_rationale_enabled": False,
    "plan_rationale_rag_topk": 5,
    "plan_rationale_min_score": 0.55,
    "plan_rationale_persist_as_note": False,
    # Cluster 3: verifica esplorativa (OFF di default)
    "exploratory_verify_enabled": False,
    "exploratory_verify_max_cycles": 1,
    "exploratory_verify_topk": 5,
    "exploratory_verify_min_score": 0.5,
    # Cluster 2: nodo understanding (OFF di default)
    "understanding_enabled": False,
    "understanding_fanout_enabled": False,
    "understanding_synthesize_enabled": False,
    "understanding_topk": 8,
    "understanding_min_token_budget": 3000,
    "understanding_max_explore": 3,
    # Componente B: contesto continuo via RAG ai sub-agent (OFF di default)
    "subagent_rag_grounding_enabled": False,
    "subagent_rag_grounding_topk": 5,
    "subagent_rag_grounding_min_score": 0.55,
    "subagent_rag_grounding_snippet_max": 800,
    "subagent_inherit_plan_rationale": False,
    # Comp.3a/3b: coordinamento azioni via DAG (OFF di default)
    "dag_topological_enabled": False,
    "dag_parallel_enabled": False,
    "dag_max_parallel": 2,
    "dag_verify_layer": True,
    # Final gate generale (fail-closed) anti-placeholder (mig 0265).
    "final_gate_enabled": True,
    "final_gate_software_intents": [
        "code", "debug", "scaffold", "implement", "build",
        "frontend", "fix", "refactor",
    ],
    "final_gate_max_cycles": 2,
    "import_staging_dirs": ["figma_export"],
    "no_orphan_min_ratio": 0.4,
    "verifier_fail_closed": True,
}

_lock = threading.RLock()
_cache: dict[str, Any] = dict(_SAFE_DEFAULTS)
_cache_loaded_at: float = 0.0


def _coerce(value: str, default: Any) -> Any:
    """Converte una stringa value dal DB nel tipo del default."""
    if isinstance(default, bool):
        return value.strip().lower() in ("true", "1", "yes", "on")
    if isinstance(default, float):
        try:
            return float(value.strip())
        except (ValueError, TypeError):
            return default
    if isinstance(default, int):
        try:
            return int(value.strip())
        except (ValueError, TypeError):
            return default
    if isinstance(default, list):
        # CSV → list[str], strip + filter empty
        return [s.strip() for s in value.split(",") if s.strip()]
    return value.strip()


def _load_from_db() -> dict[str, Any]:
    """Legge i settings orchestrator dalla tabella `settings` via psycopg2."""
    import os
    database_url = os.environ.get("DATABASE_URL", "")
    if not database_url:
        logger.warning("orchestrator_config: DATABASE_URL non impostato, uso safe_defaults")
        return dict(_SAFE_DEFAULTS)

    try:
        import psycopg2  # type: ignore[import-untyped]
    except ImportError:
        logger.warning("orchestrator_config: psycopg2 non installato, uso safe_defaults")
        return dict(_SAFE_DEFAULTS)

    full_keys = [_full_key(k) for k in _KEYS]
    try:
        conn = psycopg2.connect(database_url)
        try:
            with conn.cursor() as cur:
                keys_placeholder = ",".join(f"'{k}'" for k in full_keys)
                cur.execute(
                    f"SELECT key, value FROM settings WHERE key IN ({keys_placeholder})"
                )
                rows = {k: v for k, v in cur.fetchall()}
        finally:
            conn.close()
    except Exception as exc:
        logger.error("orchestrator_config: errore lettura DB: %s", exc)
        return dict(_cache)  # mantiene valori precedenti in caso di errore transitorio

    result: dict[str, Any] = {}
    for local_key, safe_val in _SAFE_DEFAULTS.items():
        raw = rows.get(_full_key(local_key), "")
        if not raw:
            result[local_key] = safe_val
            continue
        result[local_key] = _coerce(raw, safe_val)

    return result


def _refresh_if_stale() -> None:
    """Ricarica la cache dal DB se il TTL e' scaduto."""
    global _cache, _cache_loaded_at
    now = time.monotonic()
    with _lock:
        if now - _cache_loaded_at < _CACHE_TTL_S:
            return
        fresh = _load_from_db()
        _cache = fresh
        _cache_loaded_at = now
        logger.debug(
            "orchestrator_config: cache aggiornata (plan_enabled=%s verifier_enabled=%s)",
            fresh.get("plan_phase_enabled"),
            fresh.get("verifier_enabled"),
        )


def get() -> dict[str, Any]:
    """Restituisce la configurazione orchestrator corrente (cache TTL 60s)."""
    _refresh_if_stale()
    with _lock:
        return dict(_cache)


# ── Accessori tipizzati ────────────────────────────────────────────────────

def plan_phase_enabled() -> bool:
    return bool(get()["plan_phase_enabled"])


def verifier_enabled() -> bool:
    return bool(get()["verifier_enabled"])


def plan_behavior_modes() -> list[str]:
    return list(get()["plan_behavior_modes"])


def plan_intents() -> list[str]:
    return list(get()["plan_intents"])


def plan_min_token_budget() -> int:
    return int(get()["plan_min_token_budget"])


def planner_prompt_key() -> str:
    return str(get()["planner_prompt_key"])


def todo_reminder_every_n_steps() -> int:
    return int(get()["todo_reminder_every_n_steps"])


def todo_reminder_min_todos() -> int:
    return int(get()["todo_reminder_min_todos"])


def max_verify_cycles() -> int:
    return int(get()["max_verify_cycles"])


def max_plan_revisions() -> int:
    return int(get()["max_plan_revisions"])


def verifier_timeout_s() -> float:
    return float(get()["verifier_timeout_s"])


def worker_mode_enabled() -> bool:
    return bool(get()["worker_mode_enabled"])


def worker_mode_tool_whitelist() -> list[str]:
    return list(get()["worker_mode_tool_whitelist"])


def adaptive_classifier_enabled() -> bool:
    return bool(get()["adaptive_classifier_enabled"])


def adaptive_gating_enabled() -> bool:
    return bool(get()["adaptive_gating_enabled"])


def subagents_enabled() -> bool:
    return bool(get()["subagents_enabled"])


def plan_rationale_enabled() -> bool:
    return bool(get()["plan_rationale_enabled"])


def plan_rationale_rag_topk() -> int:
    return int(get()["plan_rationale_rag_topk"])


def plan_rationale_min_score() -> float:
    return float(get()["plan_rationale_min_score"])


def plan_rationale_persist_as_note() -> bool:
    return bool(get()["plan_rationale_persist_as_note"])


def auto_delegation_enabled() -> bool:
    return bool(get()["auto_delegation_enabled"])


def is_eligible(behavior_mode: str | None, intent: str | None, token_budget: int) -> bool:
    """Helper booleano: il run corrente puo' attivare il planner?

    Tutti e 4 i check devono passare:
    1. plan_phase_enabled = True
    2. behavior_mode in plan_behavior_modes
    3. intent in plan_intents
    4. token_budget >= plan_min_token_budget
    """
    cfg = get()
    if not cfg["plan_phase_enabled"]:
        return False
    if behavior_mode and behavior_mode.lower() not in [m.lower() for m in cfg["plan_behavior_modes"]]:
        return False
    if intent and intent.lower() not in [i.lower() for i in cfg["plan_intents"]]:
        return False
    if int(token_budget or 0) < int(cfg["plan_min_token_budget"]):
        return False
    return True


def is_eligible_adaptive(
    behavior_mode: str | None,
    intent: str | None,
    token_budget: int,
    *,
    complexity: str | None = None,
    confidence: float | None = None,
    agentic_score: float | None = None,
    is_ambiguous: bool | None = None,
) -> bool:
    """Variante adattiva di is_eligible (PR-D).

    Gate HARD sempre applicati (come is_eligible ma SENZA il filtro intent quando
    il gating adattivo e' attivo):
      1. plan_phase_enabled
      2. behavior_mode in plan_behavior_modes
      3. token_budget >= plan_min_token_budget

    Se adaptive_gating_enabled e' OFF -> comportamento legacy (richiede anche
    intent in plan_intents): identico a is_eligible.

    Se ON -> il planner forte si attiva quando i segnali del classifier
    indicano complessita'/incertezza:
      - complexity == 'high', OPPURE
      - is_ambiguous, OPPURE
      - agentic_score >= adaptive_agentic_score_min, OPPURE
      - confidence < adaptive_low_confidence_max
    Altrimenti (task semplice + alta confidence) -> flusso economico diretto.
    """
    cfg = get()
    if not cfg["plan_phase_enabled"]:
        return False
    if behavior_mode and behavior_mode.lower() not in [m.lower() for m in cfg["plan_behavior_modes"]]:
        return False
    if int(token_budget or 0) < int(cfg["plan_min_token_budget"]):
        return False

    if not cfg.get("adaptive_gating_enabled"):
        # Legacy: richiede anche intent in plan_intents.
        if intent and intent.lower() not in [i.lower() for i in cfg["plan_intents"]]:
            return False
        return True

    # Gating adattivo: decide dai segnali del classifier.
    _complexity = str(complexity or "").lower()
    # Task a BASSA complessita' non attivano MAI il planner forte, anche se il
    # classifier assegna un agentic_score alto (es. "elenca i file" -> agentic
    # 0.9-0.98 ma complexity 'low'). Sono task diretti che l'executor risolve in
    # pochi step: planner+todo+verifier+sub-agenti su questi e' over-orchestrazione
    # (osservata dal vivo: "2 agenti in parallelo" per un semplice listing).
    if _complexity == "low":
        return False
    if _complexity == "high":
        return True
    if is_ambiguous:
        return True
    if agentic_score is not None and float(agentic_score) >= float(cfg["adaptive_agentic_score_min"]):
        return True
    if confidence is not None and float(confidence) < float(cfg["adaptive_low_confidence_max"]):
        return True
    return False


def force_reload() -> None:
    """Invalida la cache e forza una rilettura immediata dal DB (utile nei test)."""
    global _cache_loaded_at
    with _lock:
        _cache_loaded_at = 0.0
