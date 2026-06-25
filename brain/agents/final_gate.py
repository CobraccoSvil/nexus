"""final_gate: gate generale fail-closed per task software senza plan_phase.

Il verifier_node gira solo quando il piano e' attivo (`plan_phase_active`). Per i
task software che chiudono SENZA plan (executor diretto, end_turn) non c'era
alcuna verifica: un'app placeholder (hello-world) montata sopra un design
importato passava silenziosamente (fail-open).

Questo modulo chiude quel buco riusando il motore di verifica generale
(`criteria_runner`), in particolare il criterio `no_orphan_imported`: se esiste
codice staged in figma_export/ con abbastanza moduli, l'entry servito
(src/main.tsx) deve raggiungerli via grafo degli import. Hello-world -> fallisce
(re-executor); design montato -> passa (chiude); nessuno staging -> N/A.

Tutta la configurazione e' letta da `orchestrator_config` (settings DB, cache
60s). Nessun nome modello / valore hardcoded (regola G).
"""
from __future__ import annotations

import logging
import os
import re
from typing import Any

from langchain_core.messages import HumanMessage

from . import orchestrator_config, criteria_runner

logger = logging.getLogger(__name__)

# ToolRunnerClient gRPC iniettato dal graph builder (come verifier_node).
_tool_runner = None


def configure(tool_runner: Any) -> None:
    """Inject del ToolRunnerClient usato dai criteri generali."""
    global _tool_runner
    _tool_runner = tool_runner


def _is_software_task(state: dict[str, Any], cfg: dict[str, Any]) -> bool:
    """True se il run va trattato come task software (quindi verificabile dal gate).

    Due segnali in OR (de-lessicalizzazione, incidente Beauty-Book 2026-06-11):
    1. STRUTTURALE (primario): il run ha gia' eseguito tool che MUTANO il
       filesystem/progetto (write/edit/rename/estrazioni/comandi). Un run che ha
       toccato il progetto va verificato A PRESCINDERE dall'intent classificato:
       il caso reale era intent=architecture (fuori whitelist) che aveva spostato
       file con rename e ha chiuso senza alcuna verifica.
    2. Whitelist intent (legacy, `agent.final_gate.software_intents`): copre i
       run software che chiudono senza step mutativi (es. solo pianificazione
       che DEVE comunque passare dal gate per il no-orphan check).

    Lo state usa `user_intent` (popolato da router_node); fallback su `intent`.
    """
    try:
        from .nodes.helpers import has_filesystem_mutation_in_history
        if has_filesystem_mutation_in_history(state.get("messages") or []):
            return True
    except Exception as exc:  # pragma: no cover - difensivo
        logger.debug("final_gate: check strutturale mutazioni saltato (%s)", exc)
    intent = str(state.get("user_intent") or state.get("intent") or "").lower()
    if not intent:
        return False
    software_intents = [str(i).lower() for i in (cfg.get("final_gate_software_intents") or [])]
    return intent in software_intents


def _project_slug(name: str) -> str:
    """Riproduce in Python l'algoritmo di slug usato dalle unit systemd lato Rust
    (project_workspace/logs.rs: name.to_lowercase().replace([' ', '_'], '-')).
    Stesso slug del prefisso unit '{slug}-' e dei detached log
    '/tmp/nexus-proj-{slug}-*.log'. Punto unico (regola L) per la corrispondenza
    cross-language: senza, il brain costruirebbe slug diversi e i comandi log
    matcherebbero unit inesistenti."""
    return name.lower().replace(" ", "-").replace("_", "-")


def _resolve_log_command(state: dict[str, Any], cfg: dict[str, Any]) -> str:
    """Punto unico (regola L) per risolvere il comando log del criterio
    service_logs_clean nel final_gate. Generalizza il vecchio
    `cfg.get('final_gate_runtime_log_command')` (docker-only hardcoded nei
    settings) a una risoluzione PER-PROGETTO consapevole dello stack.

    Ordine di priorita' (esegue la prima regola applicabile):

      1. Override admin per-progetto (setting
         `agent.final_gate.runtime_log_command_per_project`, JSON object
         {project_id_uuid: 'comando shell'}). L'admin sa esattamente cosa
         eseguire: vince su tutto.
      2. Auto-detect dallo stack guardando i `run_configurations` essential
         del progetto:
           - se almeno una usa docker/podman -> stack CONTAINER -> default
             docker compose (`agent.final_gate.runtime_log_command`).
           - se TUTTE sono native (npm/cargo/dotnet/python/...) -> stack
             NATIVE -> systemd template
             (`agent.final_gate.runtime_log_command_systemd`) con `{slug}`
             sostituito dal name del progetto (riproduce esattamente il
             prefisso delle unit installate dal wizard, vedi
             project_workspace/logs.rs).
      3. Fallback retro-compatibile: setting globale
         `agent.final_gate.runtime_log_command` (docker compose default).

    Best-effort: su errore DB ritorna il default docker (non blocca mai il
    final_gate per problemi di config); senza project_id ritorna il default
    docker (non c'e' modo di per-progettizzare un run senza progetto)."""
    docker_default = str(cfg.get("final_gate_runtime_log_command") or "")
    project_id = state.get("project_id") or os.environ.get("NEXUS_PROJECT_ID", "")
    project_id_str = str(project_id or "").strip()
    if not project_id_str:
        return docker_default

    try:
        from brain.utils.db_pool import connect as _db_connect
    except Exception:
        return docker_default

    try:
        with _db_connect() as conn, conn.cursor() as cur:
            # 1. Override admin esplicito per project_id (vince su tutto).
            cur.execute(
                "SELECT value FROM settings "
                "WHERE key = 'agent.final_gate.runtime_log_command_per_project'"
            )
            row = cur.fetchone()
            raw = (row[0] if row and row[0] else "").strip()
            if raw and raw not in ("{}", "null"):
                try:
                    import json as _json
                    overrides = _json.loads(raw)
                    if isinstance(overrides, dict):
                        cmd = overrides.get(project_id_str)
                        if cmd and isinstance(cmd, str) and cmd.strip():
                            return cmd.strip()
                except Exception as exc:  # pragma: no cover - difensivo
                    logger.debug(
                        "runtime_log_command_per_project: JSON invalido (%s)", exc,
                    )

            # 2. Auto-detect dallo stack: i run_configurations 'essential'
            #    decidono se siamo Container (docker/podman) o Native (systemd).
            cur.execute(
                "SELECT kind, command FROM run_configurations "
                "WHERE project_id = %s AND essential = TRUE",
                (project_id_str,),
            )
            rows = cur.fetchall()
            if rows:
                def _is_container(cmd: str) -> bool:
                    low = (cmd or "").lower()
                    return (
                        low.startswith(("docker", "podman"))
                        or "docker compose" in low
                        or "docker-compose" in low
                        or "podman compose" in low
                    )
                is_container_stack = any(_is_container(str(c or "")) for _k, c in rows)
                if is_container_stack:
                    return docker_default

                # Stack nativo (npm/cargo/dotnet/...): risolvi lo slug dal
                # name del progetto e applica il template systemd. NB: lo
                # slug deve coincidere con quello del wizard Rust (logs.rs),
                # vedi _project_slug.
                cur.execute(
                    "SELECT name FROM projects WHERE id = %s",
                    (project_id_str,),
                )
                name_row = cur.fetchone()
                if name_row and name_row[0]:
                    slug = _project_slug(str(name_row[0]))
                    cur.execute(
                        "SELECT value FROM settings "
                        "WHERE key = 'agent.final_gate.runtime_log_command_systemd'"
                    )
                    tpl_row = cur.fetchone()
                    tpl = (tpl_row[0] if tpl_row and tpl_row[0] else "").strip()
                    if tpl and "{slug}" in tpl:
                        return tpl.replace("{slug}", slug)
    except Exception as exc:  # pragma: no cover - difensivo
        logger.debug("final_gate._resolve_log_command: %s", exc)

    # 3. Fallback retro-compatibile (docker default storico).
    return docker_default


def _resolve_build_command(state: dict[str, Any]) -> tuple[str, str | None] | None:
    """Risolve il comando build del progetto per il criterio di COMPILAZIONE del
    final_gate (fix qualita' 2026-06-15). Priorita':
      1. run_configurations del progetto con label ~ 'build' o role='build'
         (fonte per-progetto canonica);
      2. setting `agent.final_gate.build_command` (auto-detect generico default).
    Ritorna (command, working_dir|None), oppure None se il build-check e'
    disabilitato o nessun comando e' risolvibile. Best-effort: su errore DB
    ritorna None (N/A, non blocca la chiusura)."""
    try:
        from brain.utils.db_pool import connect as _db_connect
    except Exception:
        return None
    project_id = state.get("project_id") or os.environ.get("NEXUS_PROJECT_ID", "")
    try:
        with _db_connect() as conn, conn.cursor() as cur:
            cur.execute(
                "SELECT value FROM settings WHERE key = 'agent.final_gate.build_check_enabled'"
            )
            row = cur.fetchone()
            enabled = (row[0] if row and row[0] else "true").strip().lower() in ("true", "1", "yes")
            if not enabled:
                return None
            if project_id:
                cur.execute(
                    "SELECT command, args, cwd FROM run_configurations "
                    "WHERE project_id = %s "
                    "AND (lower(label) LIKE %s OR lower(coalesce(role, '')) = 'build') "
                    "ORDER BY (lower(coalesce(role, '')) = 'build') DESC LIMIT 1",
                    (project_id, "%build%"),
                )
                rc = cur.fetchone()
                if rc and rc[0]:
                    command, args, cwd = rc[0], rc[1] or [], rc[2]
                    full = command + ((" " + " ".join(args)) if args else "")
                    return (full, cwd)
            cur.execute(
                "SELECT value FROM settings WHERE key = 'agent.final_gate.build_command'"
            )
            row = cur.fetchone()
            cmd = (row[0] if row and row[0] else "").strip()
            if cmd:
                return (cmd, None)
    except Exception as exc:  # pragma: no cover - difensivo
        logger.debug("final_gate._resolve_build_command: %s", exc)
    return None


def _resolve_endpoint_check(state: dict[str, Any]) -> dict[str, Any] | None:
    """Risolve un criterio HTTP FUNZIONALE per il final_gate (B1): una chiamata
    reale all'endpoint che il task doveva far funzionare (es. il login). Senza,
    l'agente chiude 'completed' con la build verde ma l'endpoint ancora rotto
    (incidente login Beauty-Book: 500 dal proxy, ma build TS a posto).

    De-lessicalizzato (linea anti-lessicale del repo): NON guarda il testo del
    task. Scatta SOLO se il progetto ha una `run_configurations` con role='endpoint'
    (o label ~ 'endpoint') e uno spec in `http_spec`. Gate via setting
    `agent.final_gate.endpoint_check_enabled`. Ritorna il criterio {type:'http',
    spec, expected} pronto per criteria_runner, oppure None (N/A, non blocca i
    progetti senza endpoint configurato). Best-effort: su errore DB ritorna None.
    """
    try:
        from brain.utils.db_pool import connect as _db_connect
    except Exception:
        return None
    project_id = state.get("project_id") or os.environ.get("NEXUS_PROJECT_ID", "")
    if not project_id:
        return None
    try:
        with _db_connect() as conn, conn.cursor() as cur:
            cur.execute(
                "SELECT value FROM settings WHERE key = 'agent.final_gate.endpoint_check_enabled'"
            )
            row = cur.fetchone()
            enabled = (row[0] if row and row[0] else "true").strip().lower() in (
                "true",
                "1",
                "yes",
            )
            if not enabled:
                return None
            cur.execute(
                "SELECT command, http_spec FROM run_configurations "
                "WHERE project_id = %s "
                "AND (lower(coalesce(role, '')) = 'endpoint' OR lower(label) LIKE %s) "
                "AND http_spec IS NOT NULL "
                "ORDER BY (lower(coalesce(role, '')) = 'endpoint') DESC LIMIT 1",
                (project_id, "%endpoint%"),
            )
            rc = cur.fetchone()
            if not rc:
                return None
            command, http_spec = rc[0], rc[1]
            if not isinstance(http_spec, dict):
                return None
            # http_spec: {url, method?, body?, headers?, expected_status?, body_contains?}
            url = http_spec.get("url") or command
            if not url:
                return None
            spec: dict[str, Any] = {"url": url, "method": http_spec.get("method", "GET")}
            for k in ("body", "headers"):
                if k in http_spec:
                    spec[k] = http_spec[k]
            expected: dict[str, Any] = {}
            if "expected_status" in http_spec:
                expected["status"] = http_spec["expected_status"]
            if "body_contains" in http_spec:
                expected["body_contains"] = http_spec["body_contains"]
            return {
                "type": "http",
                "spec": spec,
                "expected": expected,
                "timeout_s": _endpoint_timeout_s(),
            }
    except Exception as exc:  # pragma: no cover - difensivo
        logger.debug("final_gate._resolve_endpoint_check: %s", exc)
    return None


def _endpoint_timeout_s() -> float:
    """Timeout (s) del criterio endpoint HTTP, da settings (default 15)."""
    try:
        from brain.utils import settings_db

        return float(
            settings_db.get_setting("agent.final_gate.endpoint_timeout_seconds", "15")
            or "15"
        )
    except Exception:
        return 15.0


def _build_timeout_s() -> float:
    """Timeout (s) del criterio build, da settings (default 180: i build sono
    lenti, i 30s del verifier non basterebbero)."""
    try:
        from brain.utils.db_pool import connect as _db_connect
        with _db_connect() as conn, conn.cursor() as cur:
            cur.execute(
                "SELECT value FROM settings WHERE key = 'agent.final_gate.build_timeout_s'"
            )
            row = cur.fetchone()
            return float(row[0]) if row and row[0] else 180.0
    except Exception:
        return 180.0


def _build_output_max_chars() -> int:
    """Limite caratteri dell'output_excerpt esposto all'agente quando il
    criterio BUILD fallisce (mig 0426). Default 4000: un build TS/cargo puo'
    emettere molti errori; troncare a 600 char rende invisibili tutti quelli
    sotto il primo, e l'agente sistema un errore alla volta restando in loop.
    Best-effort: su errore DB usa il default 4000."""
    try:
        from brain.utils.db_pool import connect as _db_connect
        with _db_connect() as conn, conn.cursor() as cur:
            cur.execute(
                "SELECT value FROM settings WHERE key = 'agent.final_gate.build_output_max_chars'"
            )
            row = cur.fetchone()
            v = int(row[0]) if row and row[0] else 4000
            # Guard: minimi/massimi sani.
            if v < 1000:
                v = 1000
            if v > 32000:
                v = 32000
            return v
    except Exception:
        return 4000


# Pattern di errore di compilazione comuni (TypeScript, Rust, generici). Il
# conteggio e' indicativo (best-effort): serve a comunicare all'agente la
# SCALA del problema, non a essere un parser esatto.
_BUILD_ERROR_PATTERNS = (
    re.compile(r"error TS\d+:", re.IGNORECASE),       # tsc
    re.compile(r"\berror\[E\d+\]", re.IGNORECASE),    # rustc
    re.compile(r"\bSyntaxError\b"),
    re.compile(r"\bTypeError\b"),
    re.compile(r"^\s*error:\s", re.MULTILINE),         # generico cargo/cc
)


def _count_build_errors(output: str) -> int:
    """Conta occorrenze grezze di errori in un output di build (TS/Rust/...).
    Indicativo: serve a far sapere all'agente quanti errori deve risolvere
    (non solo il primo). Ritorna 0 se l'output e' vuoto o nessun pattern matcha."""
    if not output:
        return 0
    total = 0
    for pat in _BUILD_ERROR_PATTERNS:
        total += len(pat.findall(output))
    return total


async def run_general_gates(
    state: dict[str, Any], cfg: dict[str, Any]
) -> tuple[bool, list[dict[str, Any]]]:
    """Esegue i criteri generali via criteria_runner.

    Per ora un unico criterio: `no_orphan_imported` (anti-placeholder).
    Best-effort: su eccezione di un criterio, passed=False con evidence error.

    Ritorna (all_passed, results) con results = lista di
    {type, passed, evidence}.
    """
    project_id = state.get("project_id") or os.environ.get("NEXUS_PROJECT_ID", "")
    ctx = {
        "tool_runner": _tool_runner,
        "session_id": state.get("session_id"),
        "project_id": project_id,
        "timeout_s": cfg.get("verifier_timeout_s", 30),
    }

    criteria: list[dict[str, Any]] = [
        {
            "type": "no_orphan_imported",
            "spec": {
                "staging_dir": cfg.get("import_staging_dirs") or ["figma_export"],
                "min_reached_ratio": cfg.get("no_orphan_min_ratio", 0.4),
            },
            "expected": {"mounted": True},
        },
        # Claim-vs-fatti (incidente Beauty-Book 2026-06-11): gli output dichiarati
        # dagli STEP del run (write/edit/rename-to) devono esistere su disco a
        # fine run. Strutturale puro (agent_steps -> filesystem), nessuna lettura
        # del final_answer. N/A se il run non ha step mutativi file.
        {
            "type": "outputs_exist",
            "spec": {"run_id": str(state.get("thread_id") or "")},
            "expected": {},
        },
    ]
    # Verifica runtime E2E (mig 0374): i log dei servizi non devono contenere
    # errori runtime. Cattura il pattern "codice scritto ma flusso reale rotto"
    # (es. endpoint 500 perche' una tabella manca) che l'agente ignorerebbe.
    # Il comando log e' risolto PER-PROGETTO (mig 0427, regola L): docker
    # compose per stack container, journalctl --user --user-unit '{slug}-*' per
    # stack systemd; override admin esplicito tramite setting
    # `agent.final_gate.runtime_log_command_per_project`.
    if cfg.get("final_gate_runtime_check_enabled"):
        log_cmd = _resolve_log_command(state, cfg)
        if log_cmd:
            criteria.append({
                "type": "service_logs_clean",
                "spec": {
                    "command": log_cmd,
                    "patterns": cfg.get("final_gate_runtime_error_patterns") or [],
                },
                "expected": {},
            })

    # Criterio BUILD (fix qualita' 2026-06-15): il codice deve COMPILARE prima
    # di chiudere "completed", non solo esistere (outputs_exist). Comando
    # risolto per-progetto (run_config 'build' -> setting auto-detect); N/A se
    # non risolvibile -> non blocca i progetti senza build. Timeout dedicato
    # (i build sono lenti).
    build = _resolve_build_command(state)
    if build is not None:
        build_cmd, build_cwd = build
        # `max_output_chars`: override del troncamento standard (600) del
        # _check_run_command. Un build TS/cargo emette molti errori sotto il
        # primo: senza piu' contesto l'agente vede solo l'errore in cima e
        # ignora gli altri (fix qualita' 2026-06-15, mig 0426).
        build_crit: dict[str, Any] = {
            "type": "run_command",
            "spec": {
                "command": build_cmd,
                "max_output_chars": _build_output_max_chars(),
            },
            "expected": {"exit_code": 0},
            "timeout_s": _build_timeout_s(),
        }
        if build_cwd:
            build_crit["spec"]["working_dir"] = build_cwd
        criteria.append(build_crit)

    # Criterio ENDPOINT HTTP (B1): per i progetti con una run_configuration
    # role='endpoint', verifica con una chiamata REALE che l'endpoint risponda come
    # atteso PRIMA di chiudere "completed". De-lessicalizzato: scatta solo su config
    # strutturale, non sul testo del task. N/A (non blocca) se non configurato.
    # Risolve il caso "build verde ma login ancora 500" (incidente Beauty-Book).
    endpoint_crit = _resolve_endpoint_check(state)
    if endpoint_crit is not None:
        criteria.append(endpoint_crit)

    results: list[dict[str, Any]] = []
    for c in criteria:
        try:
            ok, evidence = await criteria_runner.run_criterion(c, ctx)
        except Exception as exc:
            logger.error("final_gate: criterion %s exception: %s", c.get("type"), exc)
            ok, evidence = False, {"error": str(exc)}
        results.append({
            "type": c.get("type"),
            "passed": bool(ok),
            "evidence": evidence,
        })

    all_passed = all(r["passed"] for r in results)
    return all_passed, results


def _render_failed_block(
    state: dict[str, Any], cycle: int, max_cycles: int, results: list[dict[str, Any]]
) -> str:
    """Costruisce il testo del HumanMessage da iniettare quando il gate fallisce.

    Rispetta la modalita' autonoma (automatic/continuo) prependendo un blocco
    <autonomy_hint> come fa verifier_node._render_failed_block.
    """
    # Corpo specifico per criterio fallito: ogni criterio (no_orphan_imported,
    # service_logs_clean, ...) fornisce gia' il suo output_excerpt con diagnosi +
    # "AGISCI". Li aggreghiamo invece di un testo fisso (prima parlava solo del
    # caso Figma; ora copre anche gli errori runtime).
    #
    # Cap per-criterio: il run_command del criterio BUILD porta gia' il suo
    # `max_output_chars` (mig 0426, default 4000): rispettiamo quel budget per
    # NON ri-troncare a 900 (taglio storico) gli errori di build, altrimenti
    # l'agente vede solo il primo errore TS/cargo e corregge una cosa alla
    # volta restando in loop. Per gli altri criteri (excerpt = verdict/error
    # breve) il taglio rimane a 900 per contenere il prompt.
    failed = [r for r in results if not r.get("passed")]
    body_parts: list[str] = []
    build_errors_count = 0
    build_truncated = False
    for r in failed:
        ev = r.get("evidence") or {}
        excerpt = ev.get("output_excerpt") or ev.get("verdict") or ev.get("error") or ""
        if not excerpt:
            continue
        is_build_run_cmd = (
            r.get("type") == "run_command"
            and ev.get("exit_code") is not None
            and ev.get("output_total_chars") is not None
        )
        if is_build_run_cmd:
            # Il criterio build espone l'excerpt gia' tagliato dal runner alla
            # soglia configurata: lo passiamo intero.
            text = str(excerpt)
            build_errors_count = _count_build_errors(text)
            build_truncated = bool(ev.get("output_truncated"))
            total_chars = int(ev.get("output_total_chars") or len(text))
            header_bits = [f"[{r.get('type')}]"]
            if build_errors_count > 0:
                header_bits.append(f"errori rilevati: {build_errors_count}")
            if build_truncated:
                header_bits.append(
                    f"output troncato ({len(text)}/{total_chars} char): "
                    "rilancia il build per leggere il resto"
                )
            body_parts.append(" ".join(header_bits) + "\n" + text)
        else:
            body_parts.append(f"[{r.get('type')}]\n{str(excerpt)[:900]}")
    detail = "\n\n".join(body_parts) if body_parts else "Una verifica del gate e' fallita."

    # Direttiva rafforzata (fix qualita' 2026-06-15): l'agente deve leggere
    # TUTTO l'output, correggere TUTTI gli errori (non solo il primo) e
    # lavorare per CONVERGENZA. Niente "completed" finche' il build non passa
    # al 100%. Se l'output e' troncato, rilanciare il comando di build (o
    # rileggere i file impattati) per recuperare il contesto mancante.
    directives_lines = [
        "DIRETTIVE (fail-closed):",
        "- Leggi TUTTO l'output qui sopra: ogni errore va corretto, non solo il primo.",
        "- Correggi TUTTI gli errori in un solo giro quando possibile: edita ogni file",
        "  impattato (anche errori 'banali' tipo unused/type mismatch contano).",
        "- Se l'output e' troncato (vedi nota 'output troncato'), rilancia il comando di",
        "  build con run_command (o rileggi i file impattati) per vedere il resto.",
        "- Lavora per CONVERGENZA: niente 'task completato' finche' il build non passa",
        "  al 100% (exit 0, zero errori). Riverifica sempre dopo le correzioni.",
    ]
    if build_errors_count > 0:
        directives_lines.insert(
            1,
            f"- Numero di errori rilevati nel build: {build_errors_count}. "
            "Risolvili TUTTI prima del prossimo final_gate.",
        )
    directives = "\n".join(directives_lines)

    body = (
        f"<final_gate_failed cycle=\"{cycle}/{max_cycles}\">\n"
        "Verifica pre-chiusura FALLITA. NON dichiarare il task completato finche'\n"
        "non e' risolto e RIVERIFICATO esercitando il flusso reale.\n\n"
        f"{detail}\n\n"
        f"{directives}\n"
        "</final_gate_failed>"
    )

    behavior_mode = (state.get("behavior_mode") or "").strip().lower()
    is_autonomous = behavior_mode in ("automatic", "automatico", "continuous", "continuo")
    if is_autonomous:
        autonomy_prefix = (
            "<autonomy_hint mode=\"" + behavior_mode + "\">\n"
            "L'utente ha selezionato la modalita' '" + behavior_mode + "': procedi\n"
            "AUTONOMAMENTE con l'integrazione. NON chiedere conferma, NON scrivere\n"
            "domande tipo 'Vuoi che lo faccia?' o 'Confermi?'. Esegui direttamente\n"
            "le modifiche necessarie usando i tool disponibili.\n"
            "</autonomy_hint>\n\n"
        )
        body = autonomy_prefix + body
    return body


async def final_gate_node(state: dict[str, Any]) -> dict[str, Any]:
    """Gate generale fail-closed.

    - Pass-through ({}): se disabilitato o task non software.
    - Passa: chiude (stop_reason end_turn -> reflection).
    - Cap raggiunto: chiude comunque (niente loop infinito).
    - Fallisce: inietta verdetto e rimanda all'executor (stop_reason tool_use).
    """
    cfg = orchestrator_config.get()
    if not cfg.get("final_gate_enabled") or not _is_software_task(state, cfg):
        return {}

    cycle = int(state.get("final_gate_cycle", 0) or 0) + 1
    max_cycles = int(cfg["final_gate_max_cycles"])

    passed, results = await run_general_gates(state, cfg)

    if passed:
        logger.info("final_gate: passato (cycle=%d) -> chiusura", cycle)
        # Segnale per la macchina a stati di terminazione (mig 0386): la verifica
        # E2E e' passata -> esito canonico CompletedVerified lato mcp-core. NON
        # impostato sul ramo forced_close (abort: resta FailedDiagnosed).
        return {
            "final_gate_cycle": 0,
            "stop_reason": "end_turn",
            "final_gate_passed": True,
        }

    # Chiusura SENZA re-executor quando:
    #  - forced_close_unverified: siamo qui per un ABORT anti-loop. L'agente e'
    #    gia' dichiarato bloccato; rimandarlo all'executor lo fa ri-abortire,
    #    accumulando un secondo AIMessage identico -> messaggio finale DUPLICATO
    #    (bug osservato) e un mini-loop abort<->final_gate. La verifica E2E e'
    #    stata comunque eseguita una volta sopra: chiudiamo.
    #  - cap raggiunto: chiusura per evitare loop infinito.
    forced_close = bool(state.get("forced_close_unverified"))
    if forced_close or cycle >= max_cycles:
        logger.warning(
            "final_gate: chiusura senza re-executor (forced_close=%s, cycle=%d/%d)",
            forced_close, cycle, max_cycles,
        )
        return {"final_gate_cycle": 0, "stop_reason": "end_turn"}

    logger.info("final_gate: fallito (cycle=%d/%d) -> re-executor", cycle, max_cycles)
    hm = HumanMessage(content=_render_failed_block(state, cycle, max_cycles, results))
    return {
        "messages": [hm],
        "final_gate_cycle": cycle,
        "stop_reason": "tool_use",
        "pending_tool_uses": [],
    }


def route_after_final_gate(state: dict[str, Any]) -> str:
    """Routing post-final_gate: re-executor su tool_use, altrimenti chiusura."""
    return "executor" if state.get("stop_reason") == "tool_use" else "learner"
