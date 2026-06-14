"""Costruzione dell'app FastAPI del Neural Core (debug/health + endpoint REST).

Concentra: creazione dell'istanza `app`, middleware CORS, eventi di
startup/shutdown (checkpointer PostgreSQL) e l'inclusione dei router suddivisi
per responsabilita' (core, vision, agent, terminal).

Lo stato condiviso vive in `brain.grpc_server.runtime`.
"""
from __future__ import annotations

import logging

from fastapi import FastAPI
from starlette.middleware.cors import CORSMiddleware

from brain.grpc_server import runtime
from brain.grpc_server.routes import agent as agent_routes
from brain.grpc_server.routes import core as core_routes
from brain.grpc_server.routes import terminal as terminal_routes
from brain.grpc_server.routes import vision as vision_routes

logger = logging.getLogger(__name__)


# --- FastAPI (debug / health) ---
app = FastAPI(title="Nexus Neural Core", version="0.2.0")

app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_methods=["*"],
    allow_headers=["*"],
)


@app.on_event("startup")
async def startup_event() -> None:
    """Inizializza il checkpointer PostgreSQL asincrono durante l'avvio."""
    logger.info("FastAPI startup: inizializzazione checkpointer PostgreSQL")
    try:
        await runtime._get_or_init_checkpointer()
        logger.info("Checkpointer PostgreSQL pronto")
    except Exception as exc:
        logger.error("Errore durante l'inizializzazione del checkpointer: %s", exc)
        raise


@app.on_event("shutdown")
async def shutdown_event() -> None:
    """Chiude il checkpointer PostgreSQL durante l'arresto."""
    if runtime._checkpointer is not None:
        logger.info("FastAPI shutdown: chiusura checkpointer PostgreSQL")
        try:
            await runtime._checkpointer.aclose()  # type: ignore[attr-defined]
            runtime._checkpointer = None
        except Exception as exc:
            logger.error("Errore durante la chiusura del checkpointer: %s", exc)


# Inclusione dei router REST/WS. Nessun prefisso: i path restano identici
# a quelli del monolite (es. /health, /agent/run/stream, /ws/terminal/...).
app.include_router(core_routes.router)
app.include_router(vision_routes.router)
app.include_router(agent_routes.router)
app.include_router(terminal_routes.router)
