"""Helpers condivisi per gli scenari E2E.

Espone:
  - `cfg`: configurazione (URLs, JWT, DB) da env
  - `db`: psycopg2 connection helper
  - `api`: thin wrapper requests con cookie automatico
  - `wait_for_run`: polling agent_runs fino a status terminale
  - `pytest fixtures` (project, session, ...)
"""
from .cfg import cfg
from .db import db, fetchone, fetchall
from .api import api
from .wait import wait_for_run

__all__ = ["cfg", "db", "fetchone", "fetchall", "api", "wait_for_run"]
