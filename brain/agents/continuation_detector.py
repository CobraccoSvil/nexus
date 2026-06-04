"""Continuation detection (ADR 0017 follow-up — Fix A).

Quando un turno agente chiude con `end_turn` ma la `final_answer` contiene
pattern di "promessa di continuazione" (es. "sto procedendo", "I'll proceed
to..."), si tratta di **continuation hallucination**: il modello ha narrato
un'intenzione futura come se fosse un piano, ma poi ha chiuso il turno
emettendo end_turn senza chiamare il tool successivo.

Tipico di Gemini 2.5 Pro su task agentici (osservato chat 6 Beauty-Book
run e38aaba7 2026-06-04 12:51): "Sto procedendo con la creazione di altri
test..." -> end_turn -> nessun nuovo write_file.

Output: should_auto_restart(final_answer, has_completion_marker) -> bool.

Comportamento:
- True  -> il caller (executor_node) deve relanciare il turno con prompt
           di follow-up "continua quello che avevi promesso".
- False -> turno completato legittimamente, nessuna azione.

Config DB-driven (regola G, cache 60s).
"""
from __future__ import annotations

import logging
import re
import time
from typing import Any

logger = logging.getLogger(__name__)

# ── Pattern di "promessa di continuazione" ─────────────────────────────────
# IT + EN. Regex case-insensitive con boundary per evitare false positive.
# Es: "ho proceduto a creare X" NON deve matchare (passato concluso);
# invece "sto procedendo a creare X" deve matchare (presente progressivo).
_CONTINUATION_PATTERNS_RAW: tuple[str, ...] = (
    # Italiano (presente progressivo / futuro intenzionale)
    r"sto\s+procedendo",
    r"procedo\s+(con|a|alla|al|all)",
    r"ora\s+(creo|implemento|scrivo|aggiungo|genero|procedo|passo)",
    r"vado\s+a\s+(creare|implementare|scrivere|aggiungere|generare)",
    r"continuo\s+(con|a|la|il|l)",
    r"passo\s+a\s+(creare|implementare|scrivere|verificare|testare)",
    r"creer[oò]\s+",
    r"implementer[oò]\s+",
    r"il\s+prossimo\s+(passo|step)\s+(è|sara)",
    r"adesso\s+(creo|implemento|scrivo|aggiungo)",
    # Inglese
    r"i'?ll\s+(proceed|now|create|implement|write|add|continue)",
    r"i'?m\s+going\s+to\s+(create|implement|write|add|continue)",
    r"next,?\s+i\s+(will|'ll)",
    r"now\s+i'?ll",
    r"moving\s+on\s+to",
    r"let\s+me\s+now",
    r"the\s+next\s+step\s+is",
    r"i\s+will\s+(create|implement|write|add|now)",
)
_CONTINUATION_RE = re.compile(
    "|".join(_CONTINUATION_PATTERNS_RAW),
    re.IGNORECASE,
)

# ── Pattern di "completamento esplicito" ────────────────────────────────────
# Quando la risposta termina con uno di questi marker, il task e' considerato
# completato anche se contiene una frase di continuazione (es. summary di
# steps gia' eseguiti + "TASK COMPLETATO"). Sono case-insensitive.
_COMPLETION_MARKERS_RAW: tuple[str, ...] = (
    r"task\s+complet[ao]t[oa]",
    r"task\s+completed",
    r"task\s+done",
    r"lavoro\s+complet[ao]t[oa]",
    r"tutti\s+i\s+test\s+sono\s+stati\s+creati",
    r"all\s+tests\s+have\s+been\s+created",
    r"work\s+complete",
    r"^\s*done\s*$",
    r"^\s*fatto\s*$",
)
_COMPLETION_RE = re.compile(
    "|".join(_COMPLETION_MARKERS_RAW),
    re.IGNORECASE | re.MULTILINE,
)

# ── Config DB-driven ────────────────────────────────────────────────────────
_CFG_CACHE: dict[str, Any] = {"loaded_at": 0.0, "config": None}
_CFG_TTL_SEC = 60.0
_DEFAULT_CONFIG: dict[str, Any] = {
    "enabled": True,
    "max_auto_restarts": 3,
    "min_promise_recency_chars": 200,
    "follow_up_prompt": (
        "Hai dichiarato di voler proseguire ma hai chiuso il turno senza farlo. "
        "Esegui ORA i prossimi passi che avevi promesso, usando i tool del progetto. "
        "Quando hai veramente finito tutti i task, scrivi come ultima riga: "
        "TASK COMPLETATO."
    ),
}


def _load_config() -> dict[str, Any]:
    """Settings agent.continuation.* (cache 60s, fallback safe se DB down)."""
    now = time.time()
    cached = _CFG_CACHE["config"]
    if cached is not None and (now - _CFG_CACHE["loaded_at"]) < _CFG_TTL_SEC:
        return cached  # type: ignore[no-any-return]
    cfg = dict(_DEFAULT_CONFIG)
    try:
        from brain.utils.settings_db import get_bool_setting, get_setting
        cfg["enabled"] = get_bool_setting(
            "agent.continuation.auto_restart_enabled", _DEFAULT_CONFIG["enabled"]
        )
        cfg["max_auto_restarts"] = int(
            get_setting(
                "agent.continuation.max_auto_restarts",
                str(_DEFAULT_CONFIG["max_auto_restarts"]),
            )
            or _DEFAULT_CONFIG["max_auto_restarts"]
        )
        cfg["min_promise_recency_chars"] = int(
            get_setting(
                "agent.continuation.min_promise_recency_chars",
                str(_DEFAULT_CONFIG["min_promise_recency_chars"]),
            )
            or _DEFAULT_CONFIG["min_promise_recency_chars"]
        )
        cfg["follow_up_prompt"] = (
            get_setting(
                "agent.continuation.follow_up_prompt",
                _DEFAULT_CONFIG["follow_up_prompt"],
            ).strip()
            or _DEFAULT_CONFIG["follow_up_prompt"]
        )
    except Exception as exc:
        logger.warning(
            "continuation_detector: load DB fallito, uso default (%s)", exc
        )
    _CFG_CACHE["config"] = cfg
    _CFG_CACHE["loaded_at"] = now
    return cfg


def detect_continuation_promise(final_answer: str) -> bool:
    """True se `final_answer` contiene una promessa di continuazione NON seguita
    da un marker di completamento esplicito.

    Heuristica:
    1. Se contiene marker di completamento (TASK COMPLETATO, ecc.) -> False.
    2. Se contiene pattern di continuazione nelle ULTIME `min_promise_recency_chars`
       (di default 200) del testo -> True. (La recency e' importante: una
       "promessa" in mezzo al testo seguita da reali azioni finali NON va
       trattata come continuation hallucination.)
    3. Altrimenti -> False.

    Pure function (no DB access), testabile in unit.
    """
    if not final_answer:
        return False
    cfg = _load_config()
    if not cfg["enabled"]:
        return False
    # 1. Marker di completamento -> turno valido.
    if _COMPLETION_RE.search(final_answer):
        return False
    # 2. Cerca pattern nella coda del testo (recency).
    recency = max(50, int(cfg["min_promise_recency_chars"]))
    tail = final_answer[-recency:] if len(final_answer) > recency else final_answer
    return bool(_CONTINUATION_RE.search(tail))


def should_auto_restart(
    final_answer: str,
    has_tool_calls_this_turn: bool,
    iteration: int,
    automation_mode: str,
    supervisor_mode: str,
    prior_auto_restarts: int = 0,
) -> tuple[bool, str]:
    """Ritorna (do_restart, reason).

    Auto-restart attivo SOLO se:
    - settings.agent.continuation.auto_restart_enabled = true (default true)
    - automation_mode in {"automatic", "continuous"} (esplicitamente autorizzato dall'utente)
    - supervisor_mode in {"continuous", "every_step"} (l'utente sta usando il loop)
    - prior_auto_restarts < max_auto_restarts (evita loop infinito)
    - L'agente NON ha gia' chiamato tool in questo turno (se ha chiamato tool E
      poi parlato di "sto procedendo" il vero motivo e' un altro: lo lascio
      passare; questa heuristica e' specifica per il caso narrare-senza-agire
      visto su Gemini 2.5 Pro).
    - detect_continuation_promise(final_answer) -> True
    """
    cfg = _load_config()
    if not cfg["enabled"]:
        return False, "disabled-by-setting"
    if automation_mode not in {"automatic", "continuous"}:
        return False, f"automation_mode={automation_mode!r} non autorizza auto-restart"
    if supervisor_mode not in {"continuous", "every_step", "on_anomaly"}:
        return False, f"supervisor_mode={supervisor_mode!r} non autorizza auto-restart"
    if prior_auto_restarts >= int(cfg["max_auto_restarts"]):
        return (
            False,
            f"max_auto_restarts={cfg['max_auto_restarts']} raggiunto (prior={prior_auto_restarts})",
        )
    if has_tool_calls_this_turn:
        # L'agente ha chiamato qualche tool nel turno corrente: la frase "sto
        # procedendo" potrebbe essere una transizione legittima a un turno
        # successivo dell'utente; non forziamo restart.
        return False, "il turno ha gia' chiamato tool"
    if not detect_continuation_promise(final_answer):
        return False, "nessuna promessa di continuazione rilevata"
    return True, f"continuation hallucination rilevata (iter={iteration})"


def follow_up_prompt() -> str:
    """Prompt da iniettare nel turno successivo quando si fa auto-restart."""
    return str(_load_config()["follow_up_prompt"])
