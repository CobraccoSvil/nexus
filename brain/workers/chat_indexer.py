"""Worker per indicizzare i messaggi chat passati nella collection RAG
`chat_history_chunks` (ADR 0015).

Esecuzione: chiamare `run_once(db_conn)` ogni ~5 minuti da un loop esterno
(es. APScheduler in main.py startup_event), oppure manualmente per backfill.

Indicizza i messaggi creati nelle ultime 24h che non hanno ancora un
record in `chat_history_indexed`.

Schema payload Qdrant:
    source_kind: "chat_history"
    source_id:   message_id
    project_id:  project_id
    session_id:  session_id
    chunk_text:  contenuto chunked
    metadata:    {role, created_at}
"""
from __future__ import annotations

import logging
import os
import uuid

import httpx
import psycopg

from brain.embeddings import EmbeddingService

logger = logging.getLogger(__name__)

QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")
COLLECTION = os.getenv("RAG_CHAT_HISTORY_COLLECTION", "chat_history_chunks")
DIM = int(os.getenv("RAG_EMBED_DIM", "384"))
CHUNK_SIZE = int(os.getenv("RAG_CHUNK_SIZE", "1000"))
CHUNK_OVERLAP = int(os.getenv("RAG_CHUNK_OVERLAP", "200"))


def _chunk(text: str, size: int, overlap: int) -> list[str]:
    """Chunker minimo allineato al chunker Rust (best effort)."""
    if not text:
        return []
    if len(text) <= size:
        return [text]
    out = []
    step = max(1, size - overlap)
    i = 0
    while i < len(text):
        end = min(len(text), i + size)
        out.append(text[i:end].strip())
        if end == len(text):
            break
        i += step
    return [c for c in out if c]


def _ensure_collection(client: httpx.Client) -> None:
    r = client.get(f"{QDRANT_URL}/collections/{COLLECTION}")
    if r.status_code == 200:
        return
    r = client.put(
        f"{QDRANT_URL}/collections/{COLLECTION}",
        json={"vectors": {"size": DIM, "distance": "Cosine"}},
    )
    r.raise_for_status()
    for field in ("project_id", "source_id", "source_kind", "session_id"):
        client.put(
            f"{QDRANT_URL}/collections/{COLLECTION}/index",
            json={"field_name": field, "field_schema": "keyword"},
        )


def _stable_point_id(message_id: str, idx: int) -> str:
    import hashlib
    h = hashlib.sha256(f"chat_history::{message_id}::{idx}".encode()).digest()[:16]
    b = bytearray(h)
    b[6] = (b[6] & 0x0F) | 0x40
    b[8] = (b[8] & 0x3F) | 0x80
    return str(uuid.UUID(bytes=bytes(b)))


def run_once(conn: psycopg.Connection, embeddings: EmbeddingService | None = None) -> int:
    """Indicizza i messaggi non ancora processati delle ultime 24h.

    Ritorna il numero di messaggi indicizzati.
    """
    embeddings = embeddings or EmbeddingService()
    with conn.cursor() as cur:
        cur.execute(
            """
            CREATE TABLE IF NOT EXISTS chat_history_indexed (
                message_id UUID PRIMARY KEY,
                indexed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                chunk_count INTEGER NOT NULL DEFAULT 0
            )
            """
        )
        cur.execute(
            """
            SELECT cm.id, cm.session_id, cs.project_id, cm.role, cm.content, cm.created_at
            FROM chat_messages cm
            JOIN chat_sessions cs ON cs.id = cm.session_id
            LEFT JOIN chat_history_indexed chi ON chi.message_id = cm.id
            WHERE cm.created_at > NOW() - INTERVAL '24 hours'
              AND chi.message_id IS NULL
              AND cm.content IS NOT NULL
              AND length(cm.content) >= 32
            LIMIT 500
            """
        )
        rows = cur.fetchall()

    if not rows:
        return 0

    with httpx.Client(timeout=10.0) as http:
        _ensure_collection(http)
        indexed = 0
        for msg_id, session_id, project_id, role, content, created_at in rows:
            chunks = _chunk(content, CHUNK_SIZE, CHUNK_OVERLAP)
            if not chunks:
                continue
            vectors = embeddings.embed_batch("", chunks)
            points = []
            for idx, (chunk_text, vec) in enumerate(zip(chunks, vectors)):
                points.append({
                    "id": _stable_point_id(str(msg_id), idx),
                    "vector": vec.values,
                    "payload": {
                        "source_kind": "chat_history",
                        "source_id": str(msg_id),
                        "project_id": str(project_id) if project_id else None,
                        "session_id": str(session_id) if session_id else None,
                        "chunk_index": idx,
                        "chunk_text": chunk_text,
                        "metadata": {
                            "role": role,
                            "created_at": created_at.isoformat() if created_at else None,
                        },
                    },
                })
            r = http.put(
                f"{QDRANT_URL}/collections/{COLLECTION}/points?wait=true",
                json={"points": points},
            )
            r.raise_for_status()
            with conn.cursor() as cur:
                cur.execute(
                    "INSERT INTO chat_history_indexed (message_id, chunk_count) VALUES (%s, %s) "
                    "ON CONFLICT (message_id) DO UPDATE SET indexed_at = NOW(), chunk_count = EXCLUDED.chunk_count",
                    (msg_id, len(chunks)),
                )
            conn.commit()
            indexed += 1
    logger.info("chat_indexer: indicizzati %d messaggi (su %d candidati)", indexed, len(rows))
    return indexed


if __name__ == "__main__":  # pragma: no cover
    import sys
    dsn = os.getenv("DATABASE_URL")
    if not dsn:
        print("DATABASE_URL non impostato", file=sys.stderr)
        sys.exit(1)
    logging.basicConfig(level=logging.INFO)
    with psycopg.connect(dsn) as c:
        run_once(c)
