"""Offload lossless del contesto LLM verso RAG (Qdrant).

Principio (decisione utente): il context window del modello e' un limite FISICO
del provider, ma il DATO non deve mai essere PERSO. Prima che il codice tronchi /
comprima / scarti un tool result o un messaggio vecchio per stare dentro il
context window, il contenuto COMPLETO viene indicizzato in Qdrant (collection
`tool_results_chunks`, gia' prevista dalla mig 0200) e diventa recuperabile
on-demand dall'agente via il tool MCP `nexus_search_semantic`.

Quindi: la compressione nel prompt resta (necessaria), ma a livello di SISTEMA
diventa LOSSLESS — il dato vive in RAG.

Caratteristiche:
  - Idempotente: dedup per hash sha256 del contenuto (point id deterministico).
    Re-indicizzare lo stesso contenuto e' un upsert no-op.
  - Best-effort: ogni errore degrada a no-op (non blocca mai il run). Se Qdrant
    o l'embedding service non sono disponibili, il troncamento avviene comunque
    (degraded mode), ma loggato come WARN.
  - DB-driven (regola G): soglie, top_k, snippet, on/off da `settings` (mig 0217)
    con cache 60s. Nessun magic number nel codice tranne i defaults safe.

Usato da brain/agents/nodes.py nei punti di troncamento/compressione.
"""
from __future__ import annotations

import hashlib
import logging
import time
from typing import Any

logger = logging.getLogger(__name__)

# ── Cache settings offload (TTL 60s) ─────────────────────────────────────────
_OFFLOAD_CACHE: dict[str, Any] = {"loaded_at": 0.0, "config": None}
_OFFLOAD_TTL_SEC = 60.0

# Defaults safe (usati SOLO se il DB e' down). Allineati alla mig 0217.
_OFFLOAD_DEFAULTS: dict[str, Any] = {
    # Flag master: se false, nessun offload (degrada al vecchio comportamento).
    "rag_offload_enabled": True,
    # Soglia minima caratteri sotto la quale NON vale la pena indicizzare:
    # contenuti piccoli stanno gia' interi nel prompt, niente perdita.
    "offload_min_chars": 2000,
    # Dimensione chunk e overlap per spezzare contenuti grandi prima dell'embed.
    "chunk_size": 1000,
    "chunk_overlap": 200,
    # Collection Qdrant per i tool result offloadati (mig 0200).
    "collection_tool_results": "tool_results_chunks",
    # Numero massimo di chunk indicizzati per singolo contenuto (anti-abuso:
    # un file da decine di MB non deve generare migliaia di point in un colpo).
    "max_chunks_per_item": 500,
    # Parametri di recupero RAG (consumati anche da nodes.py per _build_rag_context).
    "rag_top_k": 12,
    "rag_snippet_max_chars": 4000,
}


def _load_offload_config() -> dict[str, Any]:
    """Carica i settings agent.context.rag_offload.* + agent.rag.* dal DB (cache 60s).

    Mai solleva: se il DB e' down, ritorna i defaults safe e mantiene l'ultima
    cache valida. Garantisce che l'agente continui anche in degraded mode.
    """
    now = time.time()
    cached = _OFFLOAD_CACHE["config"]
    if cached is not None and (now - _OFFLOAD_CACHE["loaded_at"]) < _OFFLOAD_TTL_SEC:
        return cached  # type: ignore[return-value]

    config = dict(_OFFLOAD_DEFAULTS)
    try:
        import os
        import psycopg2  # type: ignore[import-untyped]

        db_url = os.environ.get("DATABASE_URL") or os.environ.get("POSTGRES_URL", "")
        if not db_url:
            raise RuntimeError("DATABASE_URL assente")
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT key, value FROM settings WHERE "
                    "key LIKE 'agent.context.rag_offload.%%' OR "
                    "key IN ('agent.rag.chunk_size', 'agent.rag.chunk_overlap', "
                    "'agent.rag.collection_tool_results', 'agent.rag.top_k_default')"
                )
                rows = cur.fetchall()
        for key, value in rows:
            sval = str(value).strip().strip('"')
            try:
                if key == "agent.context.rag_offload.enabled":
                    config["rag_offload_enabled"] = sval.lower() not in ("false", "0", "off", "no")
                elif key == "agent.context.rag_offload.min_chars":
                    config["offload_min_chars"] = max(0, int(sval))
                elif key == "agent.context.rag_offload.max_chunks_per_item":
                    config["max_chunks_per_item"] = max(1, int(sval))
                elif key == "agent.context.rag_offload.top_k":
                    config["rag_top_k"] = max(1, int(sval))
                elif key == "agent.context.rag_offload.snippet_max_chars":
                    config["rag_snippet_max_chars"] = max(100, int(sval))
                elif key == "agent.rag.chunk_size":
                    config["chunk_size"] = max(200, int(sval))
                elif key == "agent.rag.chunk_overlap":
                    config["chunk_overlap"] = max(0, int(sval))
                elif key == "agent.rag.collection_tool_results":
                    config["collection_tool_results"] = sval
            except Exception as parse_exc:
                logger.warning("context_offload: parse setting %s fallito: %s", key, parse_exc)
    except Exception as exc:
        logger.warning("context_offload: load DB fallito, uso defaults safe (%s)", exc)

    _OFFLOAD_CACHE["config"] = config
    _OFFLOAD_CACHE["loaded_at"] = now
    return config


def _content_hash(text: str) -> str:
    """sha256 esadecimale del contenuto, usato per dedup e point id deterministico."""
    return hashlib.sha256(text.encode("utf-8", errors="ignore")).hexdigest()


def _point_id(content_hash: str, chunk_index: int) -> str:
    """Point id deterministico (UUID v5-like derivato da hash+indice).

    Qdrant accetta UUID stringa o intero unsigned. Derivo un UUID stabile dal
    digest cosi' l'upsert dello stesso chunk e' idempotente (no duplicati).
    """
    import uuid

    return str(uuid.uuid5(uuid.NAMESPACE_URL, f"{content_hash}:{chunk_index}"))


def _chunk_text(text: str, chunk_size: int, overlap: int) -> list[str]:
    """Spezza il testo in chunk con overlap. Semplice e robusto (char-based)."""
    if chunk_size <= 0:
        return [text]
    step = max(1, chunk_size - overlap)
    chunks: list[str] = []
    pos = 0
    n = len(text)
    while pos < n:
        chunks.append(text[pos : pos + chunk_size])
        pos += step
    return chunks


def offload_to_rag(
    embeddings: Any,
    content: str,
    *,
    source_kind: str,
    metadata: dict[str, Any] | None = None,
) -> dict[str, Any] | None:
    """Indicizza il contenuto COMPLETO in Qdrant prima che venga troncato.

    Ritorna un dict con `{ref, chunks, chars, content_hash}` se l'offload e'
    avvenuto (o era gia' presente: idempotente), altrimenti None se:
      - offload disabilitato da DB;
      - contenuto sotto la soglia min_chars (sta gia' intero nel prompt);
      - embedding service / Qdrant non disponibili (degraded, loggato WARN).

    Il `ref` ritornato e' un identificatore stabile (content_hash) che viene
    incluso nel puntatore lasciato nel prompt, cosi' l'agente sa cosa cercare
    via `nexus_search_semantic`.

    Best-effort: non solleva mai. Il chiamante deve trattare None come "non
    indicizzato" e procedere comunque col troncamento (la perdita resta solo se
    l'infrastruttura RAG e' down, condizione eccezionale e loggata).
    """
    cfg = _load_offload_config()
    if not cfg["rag_offload_enabled"]:
        return None
    if not content or len(content) < int(cfg["offload_min_chars"]):
        return None
    if embeddings is None:
        logger.warning("context_offload: embedding service assente, contenuto NON indicizzato (perdita potenziale, kind=%s)", source_kind)
        return None

    content_hash = _content_hash(content)
    collection = str(cfg["collection_tool_results"])

    try:
        client = embeddings._get_qdrant()
    except Exception as exc:
        logger.warning("context_offload: _get_qdrant fallito: %s", exc)
        client = None
    if client is None:
        logger.warning("context_offload: Qdrant non disponibile, contenuto NON indicizzato (perdita potenziale, kind=%s)", source_kind)
        return None

    # Assicura la collection con la dimensione del modello di embedding.
    try:
        from qdrant_client.models import Distance, PointStruct, VectorParams  # type: ignore[import-untyped]

        existing = [c.name for c in client.get_collections().collections]
        if collection not in existing:
            client.create_collection(
                collection_name=collection,
                vectors_config=VectorParams(
                    size=int(getattr(embeddings, "_dimension", 384)),
                    distance=Distance.COSINE,
                ),
            )
            logger.info("context_offload: collection Qdrant creata: %s", collection)
    except Exception as exc:
        logger.warning("context_offload: ensure collection fallito: %s", exc)
        return None

    chunks = _chunk_text(content, int(cfg["chunk_size"]), int(cfg["chunk_overlap"]))
    max_chunks = int(cfg["max_chunks_per_item"])
    if len(chunks) > max_chunks:
        # Indicizzo comunque tutti i chunk ma in batch limitati per non esplodere:
        # qui rispetto il cap per item; oltre il cap il dato eccedente verrebbe
        # perso, quindi loggo esplicitamente.
        logger.warning(
            "context_offload: contenuto %s genera %d chunk > cap %d; indicizzo i primi %d (resto NON indicizzato)",
            content_hash[:12], len(chunks), max_chunks, max_chunks,
        )
        chunks = chunks[:max_chunks]

    try:
        vectors = embeddings.embed_batch("", chunks)
    except Exception as exc:
        logger.warning("context_offload: embed_batch fallito: %s", exc)
        return None

    base_meta = dict(metadata or {})
    points = []
    for idx, (chunk, vec) in enumerate(zip(chunks, vectors)):
        payload = {
            **base_meta,
            "source_kind": source_kind,
            "content_hash": content_hash,
            "chunk_index": idx,
            "chunk_count": len(chunks),
            "total_chars": len(content),
            "text": chunk,
        }
        points.append(
            PointStruct(id=_point_id(content_hash, idx), vector=vec.values, payload=payload)
        )

    try:
        client.upsert(collection_name=collection, points=points)
    except Exception as exc:
        logger.warning("context_offload: upsert Qdrant fallito: %s", exc)
        return None

    logger.info(
        "context_offload: indicizzati %d chunk (%d char) kind=%s ref=%s",
        len(points), len(content), source_kind, content_hash[:12],
    )
    return {
        "ref": content_hash,
        "chunks": len(points),
        "chars": len(content),
        "content_hash": content_hash,
    }


def build_pointer(original_len: int, offload: dict[str, Any] | None, *, what: str = "tool result") -> str:
    """Costruisce il puntatore testuale da lasciare nel prompt al posto del taglio.

    Se l'offload e' avvenuto, il puntatore dice esplicitamente al modello che il
    contenuto completo e' recuperabile via `nexus_search_semantic`. Altrimenti
    (degraded) segnala solo il troncamento, come prima.
    """
    if offload is not None:
        return (
            f"\n\n[contenuto completo {original_len} caratteri INDICIZZATO in RAG "
            f"(ref={offload['ref'][:12]}, {offload['chunks']} chunk). "
            f"Nessun dato perso: recupera le parti rilevanti con il tool "
            f"`nexus_search_semantic` (source_kinds includono tool_results) "
            f"usando parole chiave di cio' che ti serve da questo {what}.]\n\n"
        )
    return (
        f"\n\n[... TRONCATO: {original_len} caratteri, "
        f"offload RAG non disponibile in questo istante ...]\n\n"
    )
