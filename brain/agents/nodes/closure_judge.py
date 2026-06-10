"""Giudice LLM binario di chiusura run — WAVE 3.3 (de-lessicalizzazione governance).

Causa radice (regola H): l'esito "lavoro compiuto / non compiuto" di un run
d'azione e' oggi inferito dal TESTO dell'output del modello con blacklist
monolingua (`_detect_unfulfilled_intent` ~150 frasi it/en + regex morfologiche,
`resigned_patterns` 16 frasi). Qualsiasi lingua o stile nuovo le rompe: e' la
ragione per cui Nexus non e' universale.

Fix universale a due tempi:
 1. il modello DICHIARA l'esito via `task_complete` (WAVE 3, gia' attivo, segnale
    PRIMARIO in route_after_executor);
 2. dove la dichiarazione manca, un judge LLM language-independent giudica al
    posto delle blacklist.

Questo modulo implementa (2) in modalita' **SHADOW** (mig 0391,
`agent.closure_judge.shadow_enabled`): gira sui casi ambigui, registra in
telemetria il proprio verdetto e il DISACCORDO con la blacklist lessicale, ma
NON cambia la decisione del grafo. Avvia cosi' la finestra di confronto
(~2 settimane) prerequisito per promuovere il judge a fonte attiva e rimuovere
le blacklist — niente rimozione cieca (regola H). Tutto best-effort: ogni errore
e' loggato e ignorato, mai blocca la chiusura del run.

Punto unico (regola L): l'unica logica "giudizio di chiusura LLM" vive qui; il
learner_node delega senza re-implementare. Modello via purpose+tier (regola G):
`closure_judge` tier=light, nessun modello hardcoded.
"""
from __future__ import annotations

import asyncio
import json
import logging
import re
import time
from typing import Any, Callable

logger = logging.getLogger(__name__)

# Cache config 60s (stessa convenzione di _get_learning_config).
_CFG_TTL = 60.0
_cfg_cache: dict[str, Any] | None = None
_cfg_ts: float = 0.0

_CFG_DEFAULTS: dict[str, Any] = {
    "shadow_enabled": True,
    "min_result_chars": 40,
}

# Timeout della chiamata LLM: il judge e' leggero e off-path (shadow), non deve
# allungare percettibilmente la chiusura del run.
_JUDGE_TIMEOUT_S = 6.0
_JUDGE_MAX_TOKENS = 120


def _load_config() -> dict[str, Any]:
    """Legge i due setting agent.closure_judge.* dal DB con cache 60s.
    Fail-safe: su errore mantiene l'ultima cache valida o i default."""
    global _cfg_cache, _cfg_ts
    import os

    now = time.monotonic()
    if _cfg_cache is not None and now - _cfg_ts < _CFG_TTL:
        return _cfg_cache

    database_url = os.environ.get("DATABASE_URL")
    if not database_url:
        return dict(_CFG_DEFAULTS)
    try:
        import psycopg2  # type: ignore[import-untyped]

        conn = psycopg2.connect(database_url)
        try:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT key, value FROM settings WHERE key IN "
                    "('agent.closure_judge.shadow_enabled',"
                    " 'agent.closure_judge.min_result_chars')"
                )
                rows = dict(cur.fetchall())
        finally:
            conn.close()
        cfg = {
            "shadow_enabled": str(
                rows.get("agent.closure_judge.shadow_enabled", "true")
            ).strip().lower() != "false",
            "min_result_chars": int(
                rows.get("agent.closure_judge.min_result_chars", "40") or "40"
            ),
        }
    except Exception as exc:  # pragma: no cover - difensivo
        logger.debug("closure_judge: lettura config fallita (%s), uso cache/default", exc)
        return dict(_cfg_cache) if _cfg_cache is not None else dict(_CFG_DEFAULTS)

    _cfg_cache = cfg
    _cfg_ts = now
    return cfg


def _build_prompt(task_input: str, result: str) -> str:
    """Prompt binario LANGUAGE-INDEPENDENT: giudica se la richiesta e' stata
    portata a termine guardando task + risposta finale, in qualsiasi lingua.

    Volutamente NON elenca frasi-spia (sarebbe una blacklist lessicale travestita):
    chiede un giudizio semantico e impone output JSON chiuso."""
    # Tronchiamo per tenere il giudice economico: l'inizio del task e la coda
    # della risposta (dove di solito sta la conclusione) sono i pezzi decisivi.
    task_trunc = (task_input or "").strip()[:1500]
    result_trunc = (result or "").strip()
    if len(result_trunc) > 2500:
        result_trunc = result_trunc[:1200] + "\n[...]\n" + result_trunc[-1200:]
    return (
        "Sei un valutatore neutrale. Ti vengono dati: la RICHIESTA di un utente a "
        "un agente software e la RISPOSTA FINALE dell'agente. Giudica SOLO se la "
        "richiesta risulta portata a termine in base alla risposta, ignorando la "
        "lingua e lo stile. Una risposta che rimanda il lavoro, dichiara di non "
        "poter procedere, chiede all'utente di fare lui, o promette un'azione "
        "futura non ancora svolta NON e' compiuta. Una risposta che riporta un "
        "lavoro effettivamente svolto (anche con limiti dichiarati) e' compiuta.\n\n"
        "Rispondi ESCLUSIVAMENTE con un oggetto JSON, senza testo attorno:\n"
        '{"fulfilled": true|false, "reason": "<max 12 parole>"}\n\n'
        f"RICHIESTA:\n{task_trunc}\n\n"
        f"RISPOSTA FINALE:\n{result_trunc}\n\n"
        "JSON:"
    )


_JSON_OBJ_RE = re.compile(r"\{.*\}", re.DOTALL)


def _parse_response(raw: str) -> dict[str, Any] | None:
    """Estrae {fulfilled: bool, reason: str} dal testo LLM. Strict sul bool
    (niente bool('false')=True): se `fulfilled` non e' un booleano reale ritorna
    None (verdetto non utilizzabile -> il judge si astiene, nessuna telemetria
    fuorviante)."""
    if not raw:
        return None
    m = _JSON_OBJ_RE.search(raw)
    if not m:
        return None
    try:
        obj = json.loads(m.group(0))
    except (json.JSONDecodeError, ValueError):
        return None
    if not isinstance(obj, dict):
        return None
    fulfilled = obj.get("fulfilled")
    if not isinstance(fulfilled, bool):
        return None
    reason = str(obj.get("reason", "")).strip()[:120]
    return {"fulfilled": fulfilled, "reason": reason}


async def _resolve_model() -> tuple[str, str] | None:
    """Risolve (provider, model) per il purpose 'closure_judge' dal router DB
    (tier=light, regola G). None se non risolvibile. Client sincrono in executor."""
    try:
        from brain.router.service import _routing_client_singleton

        loop = asyncio.get_event_loop()
        decision = await loop.run_in_executor(
            None,
            lambda: _routing_client_singleton().purpose_model(purpose="closure_judge"),
        )
        if decision.provider.startswith("__") or decision.model.startswith("__"):
            logger.debug("closure_judge: purpose non risolvibile (%s)", decision.rationale)
            return None
        return decision.provider, decision.model
    except Exception as exc:  # pragma: no cover - difensivo
        logger.debug("closure_judge: resolve modello fallito (%s)", exc)
        return None


async def run_shadow(
    state: dict[str, Any],
    providers: Any,
    blacklist_unfulfilled_fn: Callable[[str | None], bool],
) -> None:
    """Esegue il judge in SHADOW e logga il confronto con la blacklist lessicale.

    NON ritorna nulla e NON modifica lo state: e' puramente osservativo (regola H,
    niente promozione cieca). Gating economico:
      - shadow abilitato in DB;
      - esito NON gia' dichiarato via task_complete (altrimenti il judge e' inutile);
      - result presente e sopra la soglia minima caratteri;
      - run d'azione (per chat puro la nozione di "compiuto" non si applica).
    """
    cfg = _load_config()
    if not cfg["shadow_enabled"]:
        return
    # Se il modello ha gia' dichiarato l'esito, il segnale primario c'e': skip.
    declared = state.get("declared_outcome")
    if isinstance(declared, dict) and declared.get("outcome") in (
        "done", "blocked", "needs_input",
    ):
        return
    result = (state.get("result") or "").strip()
    if len(result) < int(cfg["min_result_chars"]):
        return
    # Run d'azione: turn_action_oriented (classifier, mig 0387) o intent agentico.
    intent = str(state.get("user_intent") or "").lower()
    action_like = bool(state.get("turn_action_oriented")) or intent.startswith("agentic") or (
        intent in ("system_admin", "coding", "debugging")
    )
    if not action_like:
        return
    if providers is None:
        return

    # Input task (primo messaggio umano), stesso testo che vede la blacklist.
    task_input = ""
    for msg in state.get("messages", []) or []:
        content = getattr(msg, "content", None)
        if content is not None and type(msg).__name__ == "HumanMessage":
            task_input = content if isinstance(content, str) else str(content)
            break

    resolved = await _resolve_model()
    if resolved is None:
        return
    prov_name, model = resolved
    prov = None
    try:
        prov = providers._providers.get(prov_name)  # type: ignore[attr-defined]
    except Exception:
        prov = None
    if prov is None or not hasattr(prov, "generate_completion_async"):
        return

    prompt = _build_prompt(task_input, result)
    t0 = time.monotonic()
    try:
        raw_res = await asyncio.wait_for(
            prov.generate_completion_async(model, prompt, max_tokens=_JUDGE_MAX_TOKENS, temperature=0.0),
            timeout=_JUDGE_TIMEOUT_S,
        )
    except asyncio.TimeoutError:
        logger.debug("closure_judge: timeout (%.1fs)", _JUDGE_TIMEOUT_S)
        return
    except Exception as exc:  # pragma: no cover - difensivo
        logger.debug("closure_judge: errore chiamata LLM (%s)", exc)
        return
    latency_ms = int((time.monotonic() - t0) * 1000)

    raw_text = raw_res.content if hasattr(raw_res, "content") else str(raw_res)
    verdict = _parse_response(raw_text)
    if verdict is None:
        logger.debug("closure_judge: verdetto non parsabile, astensione")
        return

    judge_unfulfilled = not verdict["fulfilled"]
    try:
        blacklist_unfulfilled = bool(blacklist_unfulfilled_fn(state.get("result")))
    except Exception:
        blacklist_unfulfilled = False
    agree = judge_unfulfilled == blacklist_unfulfilled

    thread_id = state.get("thread_id", "?")
    # Telemetria di confronto (grep-able per il bilancio a ~2 settimane).
    logger.info(
        "closure_judge_shadow: thread=%s judge_unfulfilled=%s blacklist_unfulfilled=%s "
        "agree=%s latency=%dms reason=%r",
        thread_id, judge_unfulfilled, blacklist_unfulfilled, agree, latency_ms, verdict["reason"],
    )
    if not agree:
        # Riga dedicata per il conteggio dei disaccordi (decisione di promozione).
        logger.warning(
            "closure_judge_disagreement: thread=%s judge=%s blacklist=%s reason=%r",
            thread_id,
            "unfulfilled" if judge_unfulfilled else "fulfilled",
            "unfulfilled" if blacklist_unfulfilled else "fulfilled",
            verdict["reason"],
        )
