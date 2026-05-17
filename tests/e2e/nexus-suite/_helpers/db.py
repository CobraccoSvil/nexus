"""Wrapper psycopg2 minimale per gli E2E."""
import psycopg2
import psycopg2.extras
from contextlib import contextmanager
from .cfg import cfg


@contextmanager
def db():
    conn = psycopg2.connect(cfg.database_url, cursor_factory=psycopg2.extras.RealDictCursor)
    try:
        yield conn
    finally:
        conn.close()


def fetchone(sql: str, params: tuple = ()):
    with db() as conn, conn.cursor() as cur:
        cur.execute(sql, params)
        return cur.fetchone()


def fetchall(sql: str, params: tuple = ()):
    with db() as conn, conn.cursor() as cur:
        cur.execute(sql, params)
        return cur.fetchall()
