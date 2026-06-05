"""Neural Core entry point.

Runs the gRPC server by default.
Pass --rest to also start the FastAPI debug server on port 8001.

NOTA architetturale: il monolite storico e' stato suddiviso in moduli coesi
dentro `brain/grpc_server/` mantenendo identico il comportamento:
  - runtime.py        : stato condiviso (router, embeddings, providers,
                        classifier, grafo, checkpointer, client gRPC) e helper
                        condivisi (settings cache, sicurezza terminale, reload
                        chiavi/DNS, warmup Vertex).
  - app.py            : factory FastAPI + middleware + startup/shutdown +
                        inclusione router.
  - routes/core.py    : health, classify, route-model, embed, search,
                        providers, complete, reload-settings.
  - routes/vision.py  : /vision/describe, /vision/compare.
  - routes/agent.py   : project-analyze, sub-agent, clarifications,
                        batch-analyze, grafo agent run/approve/state/feedback/
                        stats e gli streaming SSE.
  - routes/terminal.py: websocket /ws/terminal.

Questo modulo resta l'entry point sottile: importa `app` (e lo stato dei
servizi da `runtime`), avvia REST + gRPC e gestisce la CLI. Le globali e gli
helper storici (`embeddings`, `router`, `providers`, `_verify_terminal_token`,
`_load_keys_from_db`, ...) sono ri-esportati qui per retro-compatibilita' con i
chiamanti esistenti (es. brain/tests/test_terminal_token_auth.py).
"""
from __future__ import annotations

import logging
import threading

# Single-instance guard: il fd del lock va tenuto APERTO per tutta la vita del
# processo (chiuderlo rilascerebbe il flock). Lo conserviamo a livello modulo.
_SINGLE_INSTANCE_LOCK_FD: int | None = None


def _acquire_single_instance_lock(grpc_port: int) -> None:
    """Garantisce UNA sola istanza del brain (regola L: punto unico).

    Acquisisce un flock esclusivo non-bloccante su un file legato alla porta
    gRPC. Se un'altra istanza e' gia' viva (es. un restart che ha lasciato il
    vecchio processo appeso), il bind fallisce e usciamo SUBITO con un messaggio
    chiaro, invece di coesistere e servire richieste dal codice vecchio. Difesa
    indipendente da SO_REUSEPORT, dal deploy e da systemd.
    """
    global _SINGLE_INSTANCE_LOCK_FD
    import fcntl
    import os
    import sys

    lock_path = os.environ.get("NEXUS_BRAIN_LOCK", f"/tmp/nexus-brain-{grpc_port}.lock")
    fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        os.close(fd)
        logger.error(
            "Un'altra istanza del brain e' gia' attiva (lock %s occupato). "
            "Esco per non coesistere sulla porta %d (evita richieste servite "
            "a caso da codice vecchio). Ferma il processo vecchio e riprova.",
            lock_path, grpc_port,
        )
        sys.exit(1)
    # Scrivi il PID per diagnostica; mantieni il fd aperto (NON chiudere).
    os.ftruncate(fd, 0)
    os.write(fd, f"{os.getpid()}\n".encode())
    _SINGLE_INSTANCE_LOCK_FD = fd
    logger.info("Single-instance lock acquisito (%s, pid=%d)", lock_path, os.getpid())

# App FastAPI gia' costruita con middleware, lifespan e router inclusi.
from brain.grpc_server.app import app

# Stato e helper condivisi (ri-esportati per retro-compatibilita').
from brain.grpc_server import runtime
from brain.grpc_server.runtime import (  # noqa: F401  (re-export pubblico)
    agentic_classifier,
    embeddings,
    providers,
    router,
    _apply_dns_override,
    _get_agent_graph,
    _get_or_init_checkpointer,
    _load_keys_from_db,
    _prepare_shell_command,
    _verify_terminal_token,
    _warmup_google_provider,
)

logger = logging.getLogger(__name__)


def _start_rest(port: int) -> None:
    import uvicorn
    uvicorn.run(app, host="0.0.0.0", port=port, log_level="info")


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s")

    # Porte dal DB (regola G: unica fonte di verita', niente env/hardcoded).
    from brain.utils import settings_db
    grpc_port = settings_db.resolve_port("brain_grpc_port")
    rest_port = settings_db.resolve_port("brain_rest_port")

    # Single-instance guard PRIMA di bindare qualunque porta: se un'altra
    # istanza e' viva, esce subito invece di coesistere (vedi funzione sopra).
    _acquire_single_instance_lock(grpc_port)

    # Load API keys from DB at startup
    result = runtime._load_keys_from_db()
    logger.info("Startup key reload: %s", result)

    # Load agent prompts from DB into in-memory registry
    try:
        from brain.agents import prompt_registry
        n_prompts = prompt_registry.load_from_db()
        logger.info("Startup agent prompts loaded: %d", n_prompts)
    except Exception as exc:
        logger.warning("Startup agent prompts load fallito: %s", exc)

    rest_thread = threading.Thread(target=_start_rest, args=(rest_port,), daemon=True)
    rest_thread.start()
    logger.info("FastAPI HTTP server avviato su porta %d", rest_port)

    from brain.grpc_server import neural_service
    neural_service.embeddings = runtime.embeddings
    neural_service.router = runtime.router
    neural_service.providers = runtime.providers
    neural_service.serve(port=grpc_port)


if __name__ == "__main__":
    main()
